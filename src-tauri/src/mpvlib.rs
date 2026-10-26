// libmpv 포터블 관리.
// mpv 공식 사이트가 안내하는 Windows 빌드(shinchiro/mpv-winbuild-cmake)의 GitHub 릴리스에서
// mpv-dev-x86_64-*.7z 를 받아 GitHub가 주는 SHA-256(digest)과 대조한 뒤, libmpv-2.dll 만 꺼내 둔다.
//   위치: %LOCALAPPDATA%\com.afterlife.player\bin\libmpv-2.dll  (+ libmpv.version 에 릴리스 태그)
// 재생 중에는 DLL을 바꿀 수 없으므로 libmpv-2.dll.new 로 받아 두고 다음 실행 때 교체한다.

use crate::fetch;
use serde::{Deserialize, Serialize};
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::AppHandle;

const LATEST_API: &str = "https://api.github.com/repos/shinchiro/mpv-winbuild-cmake/releases/latest";
const DLL: &str = "libmpv-2.dll";
const VERSION_FILE: &str = "libmpv.version";
const EVENT_PROGRESS: &str = "mpv://progress";

static UPDATING: AtomicBool = AtomicBool::new(false);

pub fn dll_path(app: &AppHandle) -> Option<PathBuf> {
    Some(fetch::bin_dir(app)?.join(DLL))
}

fn pending_path(app: &AppHandle) -> Option<PathBuf> {
    Some(fetch::bin_dir(app)?.join(format!("{DLL}.new")))
}

fn local_version(app: &AppHandle) -> Option<String> {
    let dll = dll_path(app)?;
    if !dll.is_file() {
        return None;
    }
    let v = std::fs::read_to_string(fetch::bin_dir(app)?.join(VERSION_FILE)).unwrap_or_default();
    Some(if v.trim().is_empty() { "INSTALLED".into() } else { v.trim().to_string() })
}

/// 앱 시작 시(DLL을 불러오기 전): 받아 둔 새 DLL이 있으면 교체
pub fn apply_pending(app: &AppHandle) {
    if let (Some(new), Some(dll)) = (pending_path(app), dll_path(app)) {
        if new.is_file() {
            let _ = std::fs::rename(&new, &dll);
        }
    }
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    /// "sha256:<hex>"
    digest: Option<String>,
}

/// 일반 x86_64 개발용 빌드 (-v3 는 최신 CPU 전용이라 제외)
fn pick_asset(rel: &Release) -> Option<&Asset> {
    rel.assets
        .iter()
        .find(|a| a.name.starts_with("mpv-dev-x86_64-") && !a.name.contains("-v3-") && a.name.ends_with(".7z"))
}

#[derive(Serialize)]
pub struct MpvInfo {
    version: Option<String>,
    latest: String,
    update_available: bool,
}

#[tauri::command]
pub async fn mpv_check(app: AppHandle) -> Result<MpvInfo, String> {
    let version = local_version(&app);
    let rel: Release = fetch::get_json(&fetch::client()?, LATEST_API).await?;
    let update_available = version.as_deref() != Some(rel.tag_name.as_str());
    Ok(MpvInfo { version, latest: rel.tag_name, update_available })
}

#[derive(Serialize)]
pub struct MpvUpdate {
    version: String,
    /// 이미 불러온 DLL은 바꿀 수 없어서 다음 실행 때 적용됨
    restart_required: bool,
}

#[tauri::command]
pub async fn mpv_update(app: AppHandle, loaded: tauri::State<'_, crate::player::PlayerState>) -> Result<MpvUpdate, String> {
    if UPDATING.swap(true, Ordering::SeqCst) {
        return Err("이미 받는 중".into());
    }
    let in_use = loaded.is_loaded();
    let result = install_latest(&app, in_use).await;
    UPDATING.store(false, Ordering::SeqCst);
    result.map(|version| MpvUpdate { version, restart_required: in_use })
}

async fn install_latest(app: &AppHandle, in_use: bool) -> Result<String, String> {
    let client = fetch::client()?;
    let rel: Release = fetch::get_json(&client, LATEST_API).await?;
    let asset = pick_asset(&rel).ok_or("릴리스에서 libmpv 개발용 파일을 찾지 못함")?;
    let expected = asset
        .digest
        .as_deref()
        .and_then(|d| d.strip_prefix("sha256:"))
        .ok_or("GitHub 해시 정보가 없어 설치하지 않음")?;
    let archive = fetch::download_verified(app, &client, &asset.browser_download_url, expected, EVENT_PROGRESS).await?;

    // 7z 에서 libmpv-2.dll 만 꺼내기 (압축 해제는 무거워서 다른 스레드에서)
    let dll = tauri::async_runtime::spawn_blocking(move || -> Result<Vec<u8>, String> {
        let mut reader = sevenz_rust2::ArchiveReader::new(Cursor::new(archive), sevenz_rust2::Password::empty())
            .map_err(|e| format!("압축 파일 열기 실패: {e}"))?;
        let mut found = None;
        reader
            .for_each_entries(|entry, r| {
                if entry.name().rsplit('/').next() == Some(DLL) {
                    let mut buf = Vec::with_capacity(entry.size() as usize);
                    std::io::copy(r, &mut buf)?;
                    found = Some(buf);
                    return Ok(false); // 찾았으니 그만
                }
                std::io::copy(r, &mut std::io::sink())?;
                Ok(true)
            })
            .map_err(|e| format!("압축 해제 실패: {e}"))?;
        found.ok_or_else(|| format!("압축 파일에 {DLL} 이 없음"))
    })
    .await
    .map_err(|e| e.to_string())??;

    let target = if in_use { pending_path(app) } else { dll_path(app) }.ok_or("앱 데이터 폴더를 찾을 수 없음")?;
    fetch::write_replace(&target, &dll)?;
    let dir = fetch::bin_dir(app).ok_or("앱 데이터 폴더를 찾을 수 없음")?;
    let _ = std::fs::write(dir.join(VERSION_FILE), &rel.tag_name);
    Ok(rel.tag_name.clone())
}
