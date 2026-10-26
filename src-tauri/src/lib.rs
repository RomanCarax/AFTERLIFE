mod engine;
mod fetch;
mod mpvlib;
mod player;
mod tray;
mod ytdlp;

use tauri::WindowEvent;

pub fn run() {
    tauri::Builder::default()
        // 이미 실행 중이면 새로 띄우지 않고 숨겨진 창을 다시 보여준다
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            tray::show_main(app);
        }))
        .manage(player::PlayerState::default())
        .setup(|app| {
            mpvlib::apply_pending(app.handle()); // 받아 둔 새 libmpv 가 있으면 불러오기 전에 교체
            tray::create(app.handle())?;
            Ok(())
        })
        // 창 닫기(X) → 종료하지 않고 트레이로 숨김 (백그라운드 재생, Alt+Tab 목록에서 빠짐)
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            engine::engine_status,
            tray::set_tray_tooltip,
            ytdlp::ytdlp_check,
            ytdlp::ytdlp_update,
            ytdlp::yt_search,
            mpvlib::mpv_check,
            mpvlib::mpv_update,
            player::player_load,
            player::player_pause,
            player::player_seek,
            player::player_set_volume,
            player::player_set_loop
        ])
        .run(tauri::generate_context!())
        .expect("AFTERLIFE 실행 중 오류");
}
