// yt-dlp 포터블 관리 + 검색 / 재생 주소 추출.
// 시스템에 설치된 yt-dlp를 쓰지 않고, 앱 데이터 폴더에 yt-dlp.exe를 직접 두고 쓴다.
//   위치: %LOCALAPPDATA%\com.afterlife.player\bin\yt-dlp.exe
// GitHub 최신 릴리스(yt-dlp/yt-dlp)와 버전을 비교해서, 없거나 오래됐으면 새로 받는다.
// 받은 파일은 릴리스의 SHA2-256SUMS 와 해시를 대조한 뒤에만 교체한다.

use crate::fetch;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::AppHandle;

const LATEST_API: &str = "https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest";
const DOWNLOAD_BASE: &str = "https://github.com/yt-dlp/yt-dlp/releases/download";
const ASSET: &str = "yt-dlp.exe";
/// 다운로드 진행률 이벤트 { downloaded, total }
const EVENT_PROGRESS: &str = "ytdlp://progress";
/// 검색 결과 최대 곡 수 (화면 MAX_TRACKS 와 같게)
const SEARCH_LIMIT: u32 = 10;

static UPDATING: AtomicBool = AtomicBool::new(false);

pub fn binary_path(app: &AppHandle) -> Option<PathBuf> {
    Some(fetch::bin_dir(app)?.join(ASSET))
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

pub struct Stream {
    pub url: String,
    pub user_agent: Option<String>,
    pub duration: f64,
}

/// 영상 주소 → 가장 좋은 오디오 스트림 주소
pub async fn resolve_stream(app: &AppHandle, page_url: &str) -> Result<Stream, String> {
    let json = run_json(
        app,
        vec!["-J".into(), "--no-playlist".into(), "-f".into(), "bestaudio/best".into(), page_url.into()],
    )
    .await?;
    let url = json
        .get("url")
        .and_then(Value::as_str)
        .ok_or("재생 가능한 오디오를 찾지 못함")?
        .to_string();
    let user_agent = json
        .pointer("/http_headers/User-Agent")
        .and_then(Value::as_str)
        .map(str::to_string);
    let duration = json.get("duration").and_then(Value::as_f64).unwrap_or(0.0);
    Ok(Stream { url, user_agent, duration })
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
    let path = binary_path(&app).ok_or("앱 데이터 폴더를 찾을 수 없음")?;
    let version = tauri::async_runtime::spawn_blocking(move || local_version(&path))
        .await
        .map_err(|e| e.to_string())?;
    let latest = latest_tag(&fetch::client()?).await?;
    let update_available = version.as_deref() != Some(latest.as_str());
    Ok(YtdlpInfo { version, latest, update_available })
}

/// 최신 yt-dlp.exe 를 받아 해시 확인 후 교체. 새 버전 문자열을 돌려준다.
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
    let path = binary_path(app).ok_or("앱 데이터 폴더를 찾을 수 없음")?;
    let client = fetch::client()?;
    let tag = latest_tag(&client).await?;
    let base = format!("{DOWNLOAD_BASE}/{tag}");

    // 공식 해시 목록에서 yt-dlp.exe 해시 찾기
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
        .ok_or("해시 목록에 yt-dlp.exe 가 없음")?;

    let data = fetch::download_verified(app, &client, &format!("{base}/{ASSET}"), &expected, EVENT_PROGRESS).await?;
    fetch::write_replace(&path, &data)?;

    tauri::async_runtime::spawn_blocking(move || local_version(&path))
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "받은 yt-dlp 실행 확인 실패".into())
}
