// 시스템 트레이: 창을 숨긴 채 백그라운드 재생.
// 왼쪽 클릭 → 창 보이기/숨기기, 오른쪽 클릭 → 메뉴 (열기 · 재생/일시정지 · 종료)

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};

const TRAY_ID: &str = "main";
/// 프런트엔드가 받아서 재생/일시정지를 토글하는 이벤트
const EVENT_TOGGLE_PLAY: &str = "tray://toggle-play";

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "AFTERLIFE 열기", true, None::<&str>)?;
    let toggle = MenuItem::with_id(app, "toggle", "재생 / 일시정지", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "종료", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&show, &toggle, &sep, &quit])?;

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(app.default_window_icon().cloned().expect("앱 아이콘 없음"))
        .tooltip("AFTERLIFE")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main(app),
            "toggle" => {
                let _ = app.emit(EVENT_TOGGLE_PLAY, ());
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_main(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

pub fn show_main(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
    }
}

fn toggle_main(app: &AppHandle) {
    let Some(win) = app.get_webview_window("main") else { return };
    if win.is_visible().unwrap_or(false) && !win.is_minimized().unwrap_or(false) {
        let _ = win.hide();
    } else {
        show_main(app);
    }
}

/// 트레이 아이콘에 마우스를 올렸을 때 보이는 문구 (지금 곡 표시용)
#[tauri::command]
pub fn set_tray_tooltip(app: AppHandle, text: String) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_tooltip(Some(text));
    }
}
