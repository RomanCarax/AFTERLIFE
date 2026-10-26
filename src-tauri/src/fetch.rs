// 포터블 도구(yt-dlp, libmpv) 공용: 저장 폴더, GitHub 요청, 해시 확인 다운로드.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter, Manager};

const USER_AGENT: &str = concat!("AFTERLIFE/", env!("CARGO_PKG_VERSION"));

/// 포터블 도구를 두는 폴더: %LOCALAPPDATA%\com.afterlife.player\bin
pub fn bin_dir(app: &AppHandle) -> Option<PathBuf> {
    Some(app.path().app_local_data_dir().ok()?.join("bin"))
}

pub fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| e.to_string())
}

pub async fn get_json<T: serde::de::DeserializeOwned>(client: &reqwest::Client, url: &str) -> Result<T, String> {
    client
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("GitHub 확인 실패: {e}"))?
        .json()
        .await
        .map_err(|e| format!("GitHub 응답 해석 실패: {e}"))
}

/// 내려받으면서 진행률 이벤트를 보내고, SHA-256 이 기대값과 같을 때만 내용을 돌려준다.
pub async fn download_verified(
    app: &AppHandle,
    client: &reqwest::Client,
    url: &str,
    expected_sha256: &str,
    progress_event: &str,
) -> Result<Vec<u8>, String> {
    let mut resp = client
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("다운로드 실패: {e}"))?;
    let total = resp.content_length().unwrap_or(0);
    let mut data = Vec::with_capacity(total as usize);
    let mut hasher = Sha256::new();
    let mut last_pct = u64::MAX;
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("다운로드 중단: {e}"))? {
        hasher.update(&chunk);
        data.extend_from_slice(&chunk);
        // 1% 단위로만 알림
        let pct = if total > 0 { data.len() as u64 * 100 / total } else { 0 };
        if pct != last_pct {
            last_pct = pct;
            let _ = app.emit(progress_event, serde_json::json!({ "downloaded": data.len(), "total": total }));
        }
    }
    let actual: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if actual != expected_sha256.to_ascii_lowercase() {
        return Err("해시 불일치: 받은 파일이 공식 파일과 다름 (설치 안 함)".into());
    }
    Ok(data)
}

/// 임시 파일에 쓴 뒤 이름을 바꿔서 교체 (중간에 실패해도 기존 파일은 그대로)
pub fn write_replace(path: &Path, data: &[u8]) -> Result<(), String> {
    let dir = path.parent().ok_or("잘못된 경로")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("part");
    std::fs::write(&tmp, data).map_err(|e| format!("저장 실패: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("교체 실패 (사용 중일 수 있음): {e}")
    })
}
