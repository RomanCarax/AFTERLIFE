// 이 PC 전용 오디오 중계 서버 (127.0.0.1:임의 포트, 바깥에서는 접근 불가).
// mpv 가 YouTube(googlevideo) 주소를 직접 열면 403 으로 거부당하는 일이 잦아서
// (실제 기록: 첫 곡이 403 두 번 → 파일 받기로 약 19초),
// mpv 는 여기로 요청하고 여기서 yt-dlp 가 알려 준 요청 헤더를 모두 붙여
// yt-dlp 처럼 조각(최대 10MB) 단위로 받아 흘려 준다.
// mpv 의 Range 요청(위치 이동)을 그대로 지원한다.

use crate::player::log;
use crate::ytdlp::Stream;
use std::io::{self, Read};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

/// yt-dlp 의 YouTube http_chunk_size 와 같게
const CHUNK: u64 = 10 * 1024 * 1024;
/// YouTube TV 클라이언트 주소(c=TVHTML5)는 발급 직후 1~3초 동안 403 을 준다 (실측: 0.8초 403 → 3.9초 206).
/// 그래서 403 이면 잠깐 기다렸다 다시 요청 (0.7초 × 최대 8번 ≈ 5.6초)
const RETRY_403: u32 = 8;
const RETRY_WAIT: std::time::Duration = std::time::Duration::from_millis(700);
/// 최근 등록한 곡만 기억
const KEEP: usize = 16;

struct Upstream {
    url: String,
    headers: Vec<(String, String)>,
    size: Option<u64>,
    mime: String,
}

struct Proxy {
    port: u16,
    next: AtomicU64,
    streams: Mutex<Vec<(u64, Arc<Upstream>)>>,
}

static PROXY: OnceLock<Proxy> = OnceLock::new();

/// 앱을 켤 때 한 번: 서버를 열고 요청마다 스레드 하나로 처리
pub fn start() -> Result<(), String> {
    let server = Server::http("127.0.0.1:0").map_err(|e| format!("중계 서버 열기 실패: {e}"))?;
    let port = server.server_addr().to_ip().map(|a| a.port()).ok_or("중계 서버 포트를 알 수 없음")?;
    PROXY
        .set(Proxy { port, next: AtomicU64::new(1), streams: Mutex::new(Vec::new()) })
        .map_err(|_| "중계 서버가 이미 있음")?;
    std::thread::spawn(move || {
        // 일시정지 중에는 mpv 가 천천히 읽으므로 전체 시간 제한은 두지 않음
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(None)
            .build()
            .expect("HTTP 클라이언트 생성 실패");
        for req in server.incoming_requests() {
            let client = client.clone();
            std::thread::spawn(move || handle(req, &client));
        }
    });
    log(format!("proxy: 127.0.0.1:{port} 에서 대기"));
    Ok(())
}

/// mpv 가 열 주소. 중계 서버가 없으면 원래 주소 그대로.
pub fn register(s: &Stream) -> String {
    let Some(p) = PROXY.get() else { return s.url.clone() };
    let param = |key: &str| {
        s.url.split(['?', '&']).find_map(|kv| kv.strip_prefix(key).map(str::to_string))
    };
    let up = Upstream {
        url: s.url.clone(),
        headers: s.headers.clone(),
        size: s.filesize.or_else(|| param("clen=")?.parse().ok()),
        mime: param("mime=").map(|m| m.replace("%2F", "/")).unwrap_or_else(|| "application/octet-stream".into()),
    };
    let id = p.next.fetch_add(1, Ordering::SeqCst);
    let mut list = p.streams.lock().unwrap();
    list.push((id, Arc::new(up)));
    let excess = list.len().saturating_sub(KEEP);
    list.drain(..excess);
    format!("http://127.0.0.1:{}/s/{id}", p.port)
}

fn find(id: u64) -> Option<Arc<Upstream>> {
    let list = PROXY.get()?.streams.lock().ok()?;
    list.iter().find(|(i, _)| *i == id).map(|(_, u)| u.clone())
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).unwrap()
}

fn handle(req: Request, client: &reqwest::blocking::Client) {
    let id = req.url().strip_prefix("/s/").and_then(|s| s.parse::<u64>().ok());
    let Some(up) = id.and_then(find) else {
        let _ = req.respond(Response::empty(StatusCode(404)));
        return;
    };
    let range = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("Range"))
        .and_then(|h| parse_range(h.value.as_str()));

    // 파일 크기를 모르면 첫 바이트만 요청해서 알아냄 (위치 이동에 필요)
    let size = up.size.or_else(|| probe_size(client, &up));
    let (start, end) = match (range, size) {
        (Some((s, e)), Some(total)) => (s, e.unwrap_or(total - 1).min(total - 1)),
        (Some((s, e)), None) => (s, e.unwrap_or(u64::MAX)),
        (None, Some(total)) => (0, total - 1),
        (None, None) => (0, u64::MAX),
    };
    if size.is_some_and(|total| start >= total) {
        let _ = req.respond(Response::empty(StatusCode(416)));
        return;
    }

    // 첫 조각을 먼저 열어 봄: YouTube 가 거부하면 바로 실패를 알려서 다음 방법으로 넘어가게
    let mut reader = ChunkReader { client: client.clone(), up: up.clone(), pos: start, end, cur: None, chunk_bytes: 0, done: false };
    if let Err(e) = reader.open_chunk() {
        log(format!("proxy #{}: 첫 조각 실패 ({e})", id.unwrap_or(0)));
        let _ = req.respond(Response::empty(StatusCode(502)));
        return;
    }

    let mut headers = vec![header("Content-Type", &up.mime), header("Accept-Ranges", "bytes")];
    let len = size.map(|_| end - start + 1);
    let status = if range.is_some() && size.is_some() {
        headers.push(header("Content-Range", &format!("bytes {start}-{end}/{}", size.unwrap())));
        206
    } else {
        200
    };
    if *req.method() == Method::Head {
        let _ = req.respond(Response::new(StatusCode(status), headers, io::empty(), len.map(|l| l as usize), None));
        return;
    }
    // mpv 가 위치를 옮기면 연결을 끊음 → 쓰기 실패는 정상 종료
    let _ = req.respond(Response::new(StatusCode(status), headers, reader, len.map(|l| l as usize), None));
}

/// "bytes=START-" 또는 "bytes=START-END"
fn parse_range(v: &str) -> Option<(u64, Option<u64>)> {
    let (s, e) = v.trim().strip_prefix("bytes=")?.split_once('-')?;
    Some((s.trim().parse().ok()?, e.trim().parse().ok()))
}

fn upstream_get(client: &reqwest::blocking::Client, up: &Upstream, from: u64, to: u64) -> reqwest::blocking::RequestBuilder {
    let mut rb = client.get(&up.url);
    for (k, v) in &up.headers {
        rb = rb.header(k.as_str(), v.as_str());
    }
    rb.header("Range", format!("bytes={from}-{to}"))
}

fn probe_size(client: &reqwest::blocking::Client, up: &Upstream) -> Option<u64> {
    let r = upstream_get(client, up, 0, 0).send().ok()?;
    let cr = r.headers().get("content-range")?.to_str().ok()?;
    cr.rsplit('/').next()?.parse().ok()
}

/// [pos, end] 구간을 YouTube 에서 조각 단위로 이어 받는 읽기 도구
struct ChunkReader {
    client: reqwest::blocking::Client,
    up: Arc<Upstream>,
    pos: u64,
    end: u64,
    cur: Option<reqwest::blocking::Response>,
    /// 지금 조각에서 받은 바이트 수 (빈 조각이면 끝으로 봄 → 끝없이 요청하지 않게)
    chunk_bytes: u64,
    done: bool,
}

impl ChunkReader {
    fn open_chunk(&mut self) -> io::Result<()> {
        let to = self.pos.saturating_add(CHUNK - 1).min(self.end);
        let mut tries = 0;
        let r = loop {
            let r = upstream_get(&self.client, &self.up, self.pos, to).send().map_err(io::Error::other)?;
            if r.status().as_u16() != 403 || tries >= RETRY_403 {
                break r;
            }
            tries += 1;
            std::thread::sleep(RETRY_WAIT); // 방금 발급된 주소라 아직 안 열림 → 잠깐 기다림
        };
        if tries > 0 {
            log(format!("proxy: 403 → {tries}번 기다린 뒤 응답 {} (약 {:.1}초)", r.status().as_u16(), tries as f32 * 0.7));
        }
        match r.status().as_u16() {
            200 | 206 => {
                self.cur = Some(r);
                self.chunk_bytes = 0;
                Ok(())
            }
            416 => {
                self.done = true; // 크기를 모를 때 끝까지 읽음
                Ok(())
            }
            code => Err(io::Error::other(format!("YouTube 응답 {code} (구간 {}-{to})", self.pos))),
        }
    }
}

impl Read for ChunkReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.done || self.pos > self.end {
                return Ok(0);
            }
            if self.cur.is_none() {
                if let Err(e) = self.open_chunk() {
                    log(format!("proxy: 중간 조각 실패 ({e})"));
                    return Err(e);
                }
                continue;
            }
            let n = self.cur.as_mut().unwrap().read(buf)?;
            if n == 0 {
                self.done = self.chunk_bytes == 0; // 빈 조각 = 더 받을 것 없음
                self.cur = None; // 이 조각 끝 → 다음 조각
                continue;
            }
            self.chunk_bytes += n as u64;
            self.pos += n as u64;
            return Ok(n);
        }
    }
}
