// 재생 엔진 상태 확인.
// 둘 다 앱 안의 포터블 파일만 본다: yt-dlp(ytdlp.rs) = 검색·주소 추출, libmpv(mpvlib.rs) = 재생.

use crate::{mpvlib, ytdlp};
use serde::Serialize;
use tauri::AppHandle;

#[derive(Serialize)]
pub struct EngineStatus {
    /// 앱에 들어 있는 yt-dlp 버전, 없으면 None
    ytdlp: Option<String>,
    /// 앱에 들어 있는 libmpv DLL 경로, 없으면 None
    mpv: Option<String>,
}

#[tauri::command]
pub async fn engine_status(app: AppHandle) -> EngineStatus {
    let ytdlp_path = ytdlp::binary_path(&app);
    let ytdlp = tauri::async_runtime::spawn_blocking(move || ytdlp_path.and_then(|p| ytdlp::local_version(&p)))
        .await
        .ok()
        .flatten();
    let mpv = mpvlib::dll_path(&app)
        .filter(|p| p.is_file())
        .map(|p| p.display().to_string());
    EngineStatus { ytdlp, mpv }
}
