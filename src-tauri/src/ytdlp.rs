// yt-dlp 포터블 관리 + 검색 / 재생 주소 추출.
// 시스템에 설치된 yt-dlp를 쓰지 않고, 앱 데이터 폴더에 직접 두고 쓴다.
//   위치: %LOCALAPPDATA%\com.afterlife.player\bin\yt-dlp\yt-dlp.exe
// 실행할 때마다 압축을 푸는 단일 exe(약 1.9초) 대신, 미리 풀어 둔 공식 판(yt-dlp_win.zip, 약 0.9초)을 쓴다.
// GitHub 최신 릴리스(yt-dlp/yt-dlp)와 버전을 비교해서, 없거나 오래됐으면 새로 받는다.
// 받은 파일은 릴리스의 SHA2-256SUMS 와 해시를 대조한 뒤에만 교체한다.

use crate::fetch;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::AppHandle;
use tokio::sync::OnceCell;

const LATEST_API: &str = "https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest";
const DOWNLOAD_BASE: &str = "https://github.com/yt-dlp/yt-dlp/releases/download";
const ASSET: &str = "yt-dlp_win.zip";
const DIR: &str = "yt-dlp";
const EXE: &str = "yt-dlp.exe";
/// 다운로드 진행률 이벤트 { downloaded, total }
const EVENT_PROGRESS: &str = "ytdlp://progress";
/// 검색 결과 최대 곡 수 (화면 MAX_TRACKS 와 같게)
const SEARCH_LIMIT: u32 = 10;

static UPDATING: AtomicBool = AtomicBool::new(false);

/// 미리 풀어 둔 판 (설치 · 업데이트 확인 기준)
fn onedir_path(app: &AppHandle) -> Option<PathBuf> {
    Some(fetch::bin_dir(app)?.join(DIR).join(EXE))
}

/// 실행할 yt-dlp: 미리 풀어 둔 판, 아직 없으면 예전 단일 exe 판 (업데이트 직후 받는 동안에도 검색되게)
pub fn binary_path(app: &AppHandle) -> Option<PathBuf> {
    let onedir = onedir_path(app)?;
    if onedir.is_file() {
        return Some(onedir);
    }
    let legacy = fetch::bin_dir(app)?.join(EXE);
    Some(if legacy.is_file() { legacy } else { onedir })
}

fn command(path: &Path) -> Command {
    let mut cmd = Command::new(path);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// `yt-dlp.exe --version` (없거나 실행 실패면 None)
pub fn local_version(path: &Path) -> Option<String> {
    if !path.is_file() {
        return None;
    }
    let out = command(path).arg("--version").output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// yt-dlp 를 실행해서 JSON 출력을 받는다 (창 없이, 다른 스레드에서)
async fn run_json(app: &AppHandle, args: Vec<String>) -> Result<Value, String> {
    let path = binary_path(app).filter(|p| p.is_file()).ok_or("yt-dlp 가 아직 설치되지 않음")?;
    let out = tauri::async_runtime::spawn_blocking(move || {
        command(&path)
            .args(["--no-warnings", "--encoding", "utf-8"])
            .args(&args)
            .output()
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| format!("yt-dlp 실행 실패: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("알 수 없는 오류");
        return Err(last.trim().to_string());
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("yt-dlp 결과 해석 실패: {e}"))
}

/* ───────── 검색 ───────── */

#[derive(Serialize)]
pub struct Track {
    id: String,
    title: String,
    artist: String,
    duration: f64,
    thumbnail: String,
    url: String,
    source: &'static str,
}

fn to_track(e: &Value) -> Option<Track> {
    let id = e.get("id")?.as_str()?.to_string();
    let s = |k: &str| e.get(k).and_then(Value::as_str).filter(|v| !v.is_empty());
    Some(Track {
        title: s("title").unwrap_or("UNKNOWN SIGNAL").to_string(),
        artist: s("channel").or_else(|| s("uploader")).unwrap_or("UNKNOWN").to_string(),
        duration: e.get("duration").and_then(Value::as_f64).unwrap_or(0.0),
        // 정사각형으로 잘라 쓰므로 가벼운 320×180 섬네일
        thumbnail: format!("https://i.ytimg.com/vi/{id}/mqdefault.jpg"),
        url: format!("https://www.youtube.com/watch?v={id}"),
        source: "youtube",
        id,
    })
}

/// 검색어면 YouTube 검색 10곡, 링크면 그 영상(재생목록이면 앞 10곡)
#[tauri::command]
pub async fn yt_search(app: AppHandle, query: String) -> Result<Vec<Track>, String> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(vec![]);
    }
    let target = if q.starts_with("http://") || q.starts_with("https://") {
        q.to_string()
    } else {
        format!("ytsearch{SEARCH_LIMIT}:{q}")
    };
    let json = run_json(
        &app,
        vec![
            "-J".into(),
            "--flat-playlist".into(),
            "--playlist-end".into(),
            SEARCH_LIMIT.to_string(),
            target,
        ],
    )
    .await?;
    let tracks = match json.get("entries").and_then(Value::as_array) {
        Some(entries) => entries.iter().filter_map(to_track).collect(),
        None => to_track(&json).into_iter().collect(),
    };
    Ok(tracks)
}

/* ───────── 재생 주소 ───────── */

#[derive(Clone)]
pub struct Stream {
    pub url: String,
    /// yt-dlp 가 알려 준 요청 헤더 전부 (User-Agent, Accept, Accept-Language, Sec-Fetch-Mode …)
    pub headers: Vec<(String, String)>,
    /// 오디오 파일 크기 (모르면 None → 중계 서버가 직접 알아냄)
    pub filesize: Option<u64>,
    pub duration: f64,
}

/// 영상 주소 → 가장 좋은 오디오 스트림 주소.
/// (웹페이지 받기를 건너뛰는 빠른 방식은 YouTube 봇 확인을 불러서 쓰지 않음)
async fn resolve_stream(app: &AppHandle, page_url: &str) -> Result<Stream, String> {
    let args: Vec<String> = vec!["-J".into(), "--no-playlist".into(), "-f".into(), "bestaudio/best".into(), page_url.into()];
    let json = run_json(app, args).await?;
    let url = json
        .get("url")
        .and_then(Value::as_str)
        .ok_or("재생 가능한 오디오를 찾지 못함")?
        .to_string();
    let headers = json
        .get("http_headers")
        .and_then(Value::as_object)
        .map(|h| h.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect())
        .unwrap_or_default();
    let filesize = json.get("filesize").and_then(Value::as_u64);
    let duration = json.get("duration").and_then(Value::as_f64).unwrap_or(0.0);
    Ok(Stream { url, headers, filesize, duration })
}

/* 주소 캐시: 미리 뽑아 둔 주소를 만료 전까지 재사용 (다음 곡 · 마우스 올린 곡 · 다시 듣기)
   같은 곡을 동시에 두 번 뽑지 않도록 진행 중인 추출도 함께 기다린다 */
type Slot = Arc<OnceCell<Result<Stream, String>>>;
static CACHE: OnceLock<Mutex<HashMap<String, (Slot, Instant)>>> = OnceLock::new();

fn cache() -> &'static Mutex<HashMap<String, (Slot, Instant)>> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// YouTube 오디오 주소의 expire= (유닉스 초) 10분 전까지만 씀. 없으면 뽑은 뒤 1시간.
fn still_valid(s: &Stream, made: Instant) -> bool {
    let expire = s
        .url
        .split(['?', '&'])
        .find_map(|kv| kv.strip_prefix("expire=")?.parse::<u64>().ok());
    match expire {
        Some(exp) => {
            let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            now + 600 < exp
        }
        None => made.elapsed() < Duration::from_secs(3600),
    }
}

/// 재생할 오디오 주소. fresh = true 면 캐시를 버리고 새로 뽑음 (재시도용)
pub async fn stream_for(app: &AppHandle, page_url: &str, fresh: bool) -> Result<(Stream, bool), String> {
    let slot = {
        let mut map = cache().lock().map_err(|e| e.to_string())?;
        let reuse = !fresh
            && map.get(page_url).is_some_and(|(slot, made)| match slot.get() {
                None => true, // 뽑는 중 → 같이 기다림
                Some(Ok(s)) => still_valid(s, *made),
                Some(Err(_)) => false,
            });
        if !reuse {
            map.insert(page_url.to_string(), (Arc::new(OnceCell::new()), Instant::now()));
        }
        map[page_url].0.clone()
    };
    let cached = slot.initialized();
    let result = slot.get_or_init(|| resolve_stream(app, page_url)).await.clone();
    if result.is_err() {
        let mut map = cache().lock().map_err(|e| e.to_string())?;
        if map.get(page_url).is_some_and(|(s, _)| Arc::ptr_eq(s, &slot)) {
            map.remove(page_url); // 실패는 기억하지 않음
        }
    }
    result.map(|s| (s, cached))
}

/* 마지막 수단: mpv 가 YouTube 주소를 못 열 때(403 등) yt-dlp 가 오디오를 직접 받아 파일로 재생.
   yt-dlp 다운로더는 YouTube 가 기대하는 방식(조각 요청 등)으로 받으므로 대개 통과한다.
   파일은 %LOCALAPPDATA%\com.afterlife.player\cache 에 두고 앱을 켤 때마다 비운다. */
pub fn cache_dir(app: &AppHandle) -> Option<PathBuf> {
    Some(fetch::bin_dir(app)?.parent()?.join("cache"))
}

pub fn clear_cache(app: &AppHandle) {
    if let Some(dir) = cache_dir(app) {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

pub async fn download_audio(app: &AppHandle, page_url: &str) -> Result<PathBuf, String> {
    let exe = binary_path(app).filter(|p| p.is_file()).ok_or("yt-dlp 가 아직 설치되지 않음")?;
    let dir = cache_dir(app).ok_or("앱 데이터 폴더를 찾을 수 없음")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let template = dir.join("%(id)s.%(ext)s").display().to_string();
    let url = page_url.to_string();
    let out = tauri::async_runtime::spawn_blocking(move || {
        command(&exe)
            .args(["--no-warnings", "--encoding", "utf-8", "--no-playlist", "--no-part", "-f", "bestaudio/best"])
            .args(["-o", &template, "--print", "after_move:filepath", &url])
            .output()
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| format!("yt-dlp 실행 실패: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("다운로드 실패").trim().to_string());
    }
    let path = String::from_utf8_lossy(&out.stdout).lines().last().unwrap_or("").trim().to_string();
    let path = PathBuf::from(path);
    if path.is_file() { Ok(path) } else { Err("받은 오디오 파일을 찾지 못함".into()) }
}

/// 미리 뽑아 두기 (기다리지 않고 바로 돌아감)
#[tauri::command]
pub fn yt_prefetch(app: AppHandle, url: String) {
    tauri::async_runtime::spawn(async move {
        let _ = stream_for(&app, &url, false).await;
    });
}

/* ───────── 설치 · 업데이트 ───────── */

#[derive(Deserialize)]
struct Release {
    tag_name: String,
}

async fn latest_tag(client: &reqwest::Client) -> Result<String, String> {
    Ok(fetch::get_json::<Release>(client, LATEST_API).await?.tag_name)
}

#[derive(Serialize)]
pub struct YtdlpInfo {
    /// 앱에 들어 있는 버전 (없으면 None)
    version: Option<String>,
    /// GitHub 최신 버전
    latest: String,
    update_available: bool,
}

/// GitHub 최신 버전과 비교만 한다 (다운로드 안 함)
#[tauri::command]
pub async fn ytdlp_check(app: AppHandle) -> Result<YtdlpInfo, String> {
    // 미리 풀어 둔 판이 없으면 (예전 단일 exe 만 있어도) 새로 받도록 "없음"으로 봄
    let path = onedir_path(&app).ok_or("앱 데이터 폴더를 찾을 수 없음")?;
    let version = tauri::async_runtime::spawn_blocking(move || local_version(&path))
        .await
        .map_err(|e| e.to_string())?;
    let latest = latest_tag(&fetch::client()?).await?;
    let update_available = version.as_deref() != Some(latest.as_str());
    Ok(YtdlpInfo { version, latest, update_available })
}

/// 최신 yt-dlp_win.zip 을 받아 해시 확인 후 풀어서 교체. 새 버전 문자열을 돌려준다.
#[tauri::command]
pub async fn ytdlp_update(app: AppHandle) -> Result<String, String> {
    if UPDATING.swap(true, Ordering::SeqCst) {
        return Err("이미 업데이트 중".into());
    }
    let result = download_latest(&app).await;
    UPDATING.store(false, Ordering::SeqCst);
    result
}

async fn download_latest(app: &AppHandle) -> Result<String, String> {
    let bin = fetch::bin_dir(app).ok_or("앱 데이터 폴더를 찾을 수 없음")?;
    let client = fetch::client()?;
    let tag = latest_tag(&client).await?;
    let base = format!("{DOWNLOAD_BASE}/{tag}");

    // 공식 해시 목록에서 yt-dlp_win.zip 해시 찾기
    let sums = client
        .get(format!("{base}/SHA2-256SUMS"))
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("해시 목록 다운로드 실패: {e}"))?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    let expected = sums
        .lines()
        .find_map(|l| {
            let (hash, name) = l.split_once(char::is_whitespace)?;
            (name.trim() == ASSET).then(|| hash.to_string())
        })
        .ok_or("해시 목록에 yt-dlp_win.zip 이 없음")?;

    let data = fetch::download_verified(app, &client, &format!("{base}/{ASSET}"), &expected, EVENT_PROGRESS).await?;

    // 임시 폴더에 풀고 → 기존 폴더와 바꿔치기 (중간에 실패해도 기존 것은 그대로)
    let path = tauri::async_runtime::spawn_blocking(move || -> Result<PathBuf, String> {
        let new_dir = bin.join(format!("{DIR}.new"));
        let _ = std::fs::remove_dir_all(&new_dir);
        let mut zip = zip::ZipArchive::new(Cursor::new(data)).map_err(|e| format!("압축 파일 열기 실패: {e}"))?;
        for i in 0..zip.len() {
            let mut f = zip.by_index(i).map_err(|e| e.to_string())?;
            let Some(rel) = f.enclosed_name() else { continue }; // 폴더 밖으로 나가는 경로는 무시
            let out = new_dir.join(rel);
            if f.is_dir() {
                std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
                continue;
            }
            if let Some(p) = out.parent() {
                std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
            }
            let mut w = std::fs::File::create(&out).map_err(|e| e.to_string())?;
            std::io::copy(&mut f, &mut w).map_err(|e| e.to_string())?;
        }
        let exe = new_dir.join(EXE);
        if !exe.is_file() {
            return Err("압축 파일에 yt-dlp.exe 가 없음".into());
        }
        let dir = bin.join(DIR);
        let old = bin.join(format!("{DIR}.old"));
        let _ = std::fs::remove_dir_all(&old);
        if dir.exists() {
            std::fs::rename(&dir, &old).map_err(|e| format!("교체 실패 (yt-dlp 사용 중일 수 있음): {e}"))?;
        }
        std::fs::rename(&new_dir, &dir).map_err(|e| format!("교체 실패: {e}"))?;
        let _ = std::fs::remove_dir_all(&old);
        let _ = std::fs::remove_file(bin.join(EXE)); // 예전 단일 exe 판 정리
        Ok(dir.join(EXE))
    })
    .await
    .map_err(|e| e.to_string())??;

    tauri::async_runtime::spawn_blocking(move || local_version(&path))
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "받은 yt-dlp 실행 확인 실패".into())
}
