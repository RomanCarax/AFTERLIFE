// libmpv 플레이어.
// 링크 없이 실행 중에 libmpv-2.dll 을 불러(libloading) mpv C API 를 직접 부른다.
// 이벤트 스레드가 mpv 이벤트를 받아 프런트엔드로 보낸다:
//   player://loaded  { duration }   곡 준비 끝 (재생 시작)
//   player://time    { pos }        재생 위치 (0.25초 간격)
//   player://duration{ duration }
//   player://ended                  곡 끝 (반복 꺼짐일 때, 끝에서 멈춘 채로 남음)
//   player://error   { message }

use crate::{mpvlib, ytdlp};
use libloading::Library;
use serde_json::json;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, State};

/* ───────── mpv C API (include/mpv/client.h, API 2.x) ───────── */

type Handle = *mut c_void;

const FORMAT_FLAG: c_int = 3;
const FORMAT_DOUBLE: c_int = 5;

const EVENT_NONE: c_int = 0;
const EVENT_SHUTDOWN: c_int = 1;
const EVENT_END_FILE: c_int = 7;
const EVENT_FILE_LOADED: c_int = 8;
const EVENT_PROPERTY_CHANGE: c_int = 22;

const END_FILE_EOF: c_int = 0;
const END_FILE_ERROR: c_int = 4;

#[repr(C)]
#[allow(dead_code)] // C 구조체 모양을 맞추기 위한 필드
struct MpvEvent {
    event_id: c_int,
    error: c_int,
    reply_userdata: u64,
    data: *mut c_void,
}

#[repr(C)]
#[allow(dead_code)] // C 구조체 모양을 맞추기 위한 필드
struct MpvEventProperty {
    name: *const c_char,
    format: c_int,
    data: *mut c_void,
}

#[repr(C)]
#[allow(dead_code)] // C 구조체 모양을 맞추기 위한 필드
struct MpvEventEndFile {
    reason: c_int,
    error: c_int,
}

struct Api {
    create: unsafe extern "C" fn() -> Handle,
    initialize: unsafe extern "C" fn(Handle) -> c_int,
    set_option_string: unsafe extern "C" fn(Handle, *const c_char, *const c_char) -> c_int,
    command: unsafe extern "C" fn(Handle, *mut *const c_char) -> c_int,
    set_property_string: unsafe extern "C" fn(Handle, *const c_char, *const c_char) -> c_int,
    observe_property: unsafe extern "C" fn(Handle, u64, *const c_char, c_int) -> c_int,
    wait_event: unsafe extern "C" fn(Handle, f64) -> *mut MpvEvent,
    error_string: unsafe extern "C" fn(c_int) -> *const c_char,
}

impl Api {
    unsafe fn load(lib: &'static Library) -> Result<Self, libloading::Error> {
        Ok(Api {
            create: *lib.get(b"mpv_create\0")?,
            initialize: *lib.get(b"mpv_initialize\0")?,
            set_option_string: *lib.get(b"mpv_set_option_string\0")?,
            command: *lib.get(b"mpv_command\0")?,
            set_property_string: *lib.get(b"mpv_set_property_string\0")?,
            observe_property: *lib.get(b"mpv_observe_property\0")?,
            wait_event: *lib.get(b"mpv_wait_event\0")?,
            error_string: *lib.get(b"mpv_error_string\0")?,
        })
    }
}

fn cstr(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_default()
}

/// mpv 인스턴스 하나. mpv API 는 여러 스레드에서 불러도 안전하다.
pub struct Mpv {
    api: Api,
    h: Handle,
}
unsafe impl Send for Mpv {}
unsafe impl Sync for Mpv {}

impl Mpv {
    fn new(dll: &std::path::Path) -> Result<Arc<Self>, String> {
        // DLL 은 앱이 끝날 때까지 쓰므로 놓지 않는다
        let lib: &'static Library =
            Box::leak(Box::new(unsafe { Library::new(dll) }.map_err(|e| format!("libmpv 불러오기 실패: {e}"))?));
        let api = unsafe { Api::load(lib) }.map_err(|e| format!("libmpv 함수 찾기 실패: {e}"))?;
        let h = unsafe { (api.create)() };
        if h.is_null() {
            return Err("mpv 생성 실패".into());
        }
        let mpv = Mpv { api, h };
        // 오디오 전용 · 창 없음 · yt-dlp 는 우리가 직접 돌리므로 mpv 의 ytdl 끔
        for (k, v) in [
            ("vid", "no"),
            ("video", "no"),
            ("vo", "null"),
            ("audio-display", "no"),
            ("ytdl", "no"),
            ("terminal", "no"),
            ("idle", "yes"),
            ("keep-open", "yes"), // 곡이 끝나도 닫지 않고 멈춤 → 다시 재생/이동 가능
            ("input-default-bindings", "no"),
            ("cache", "yes"),
        ] {
            mpv.set_option(k, v);
        }
        let rc = unsafe { (mpv.api.initialize)(mpv.h) };
        if rc < 0 {
            return Err(format!("mpv 초기화 실패: {}", mpv.err(rc)));
        }
        for (name, fmt) in [("time-pos", FORMAT_DOUBLE), ("duration", FORMAT_DOUBLE), ("eof-reached", FORMAT_FLAG)] {
            let n = cstr(name);
            unsafe { (mpv.api.observe_property)(mpv.h, 0, n.as_ptr(), fmt) };
        }
        Ok(Arc::new(mpv))
    }

    fn err(&self, rc: c_int) -> String {
        unsafe { CStr::from_ptr((self.api.error_string)(rc)) }.to_string_lossy().into_owned()
    }

    fn set_option(&self, k: &str, v: &str) {
        let (k, v) = (cstr(k), cstr(v));
        unsafe { (self.api.set_option_string)(self.h, k.as_ptr(), v.as_ptr()) };
    }

    fn set_prop(&self, k: &str, v: &str) -> Result<(), String> {
        let (kc, vc) = (cstr(k), cstr(v));
        let rc = unsafe { (self.api.set_property_string)(self.h, kc.as_ptr(), vc.as_ptr()) };
        if rc < 0 { Err(format!("{k}: {}", self.err(rc))) } else { Ok(()) }
    }

    fn command(&self, args: &[&str]) -> Result<(), String> {
        let owned: Vec<CString> = args.iter().map(|a| cstr(a)).collect();
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        let rc = unsafe { (self.api.command)(self.h, ptrs.as_mut_ptr()) };
        if rc < 0 { Err(format!("{}: {}", args[0], self.err(rc))) } else { Ok(()) }
    }

    /// mpv 이벤트를 받아 프런트엔드로 넘기는 스레드
    fn spawn_events(self: Arc<Self>, app: AppHandle) {
        std::thread::spawn(move || {
            let mut last_time = Instant::now() - Duration::from_secs(1);
            loop {
                let ev = unsafe { &*(self.api.wait_event)(self.h, -1.0) };
                match ev.event_id {
                    EVENT_NONE => continue,
                    EVENT_SHUTDOWN => break,
                    EVENT_FILE_LOADED => {
                        let _ = app.emit("player://loaded", ());
                    }
                    EVENT_END_FILE => {
                        let end = unsafe { &*(ev.data as *const MpvEventEndFile) };
                        match end.reason {
                            END_FILE_EOF => {
                                let _ = app.emit("player://ended", ());
                            }
                            END_FILE_ERROR => {
                                let _ = app.emit("player://error", json!({ "message": self.err(end.error) }));
                            }
                            _ => {} // 다른 곡으로 바꾸거나 멈춘 경우
                        }
                    }
                    EVENT_PROPERTY_CHANGE => {
                        let p = unsafe { &*(ev.data as *const MpvEventProperty) };
                        if p.data.is_null() {
                            continue;
                        }
                        let name = unsafe { CStr::from_ptr(p.name) }.to_string_lossy();
                        // keep-open 이라 곡 끝은 eof-reached 로 알 수 있다 (반복 중에는 오지 않음)
                        if p.format == FORMAT_FLAG {
                            if name == "eof-reached" && unsafe { *(p.data as *const c_int) } != 0 {
                                let _ = app.emit("player://ended", ());
                            }
                            continue;
                        }
                        if p.format != FORMAT_DOUBLE {
                            continue;
                        }
                        let value = unsafe { *(p.data as *const f64) };
                        if name == "time-pos" {
                            // 너무 자주 보내지 않게 0.25초 간격
                            if last_time.elapsed() >= Duration::from_millis(250) {
                                last_time = Instant::now();
                                let _ = app.emit("player://time", json!({ "pos": value }));
                            }
                        } else if name == "duration" {
                            let _ = app.emit("player://duration", json!({ "duration": value }));
                        }
                    }
                    _ => {}
                }
            }
        });
    }
}

/* ───────── Tauri 상태 · 명령 ───────── */

pub struct PlayerState {
    mpv: Mutex<Option<Arc<Mpv>>>,
    /// 곡 불러오기 순번: 주소를 뽑는 동안 다른 곡이 눌리면 이전 요청은 버린다
    generation: AtomicU64,
    /// mpv 가 아직 없을 때 받은 설정 (생성 시 적용)
    volume: Mutex<f64>,
    looping: Mutex<bool>,
}

impl Default for PlayerState {
    fn default() -> Self {
        PlayerState {
            mpv: Mutex::new(None),
            generation: AtomicU64::new(0),
            volume: Mutex::new(37.5), // 표시 50 × 0.75
            looping: Mutex::new(false),
        }
    }
}

impl PlayerState {
    pub fn is_loaded(&self) -> bool {
        self.mpv.lock().map(|m| m.is_some()).unwrap_or(false)
    }

    fn current(&self) -> Option<Arc<Mpv>> {
        self.mpv.lock().ok()?.clone()
    }

    /// 처음 재생할 때 libmpv 를 불러 mpv 를 만든다
    fn get_or_init(&self, app: &AppHandle) -> Result<Arc<Mpv>, String> {
        let mut slot = self.mpv.lock().map_err(|e| e.to_string())?;
        if let Some(m) = slot.as_ref() {
            return Ok(m.clone());
        }
        let dll = mpvlib::dll_path(app).filter(|p| p.is_file()).ok_or("libmpv 가 아직 설치되지 않음")?;
        let mpv = Mpv::new(&dll)?;
        mpv.set_prop("volume", &format!("{:.2}", *self.volume.lock().unwrap()))?;
        mpv.set_prop("loop-file", if *self.looping.lock().unwrap() { "inf" } else { "no" })?;
        mpv.clone().spawn_events(app.clone());
        *slot = Some(mpv.clone());
        Ok(mpv)
    }
}

/// 영상 주소를 받아 오디오 주소를 뽑고 재생. 도중에 다른 곡이 눌리면 Ok(false).
#[tauri::command]
pub async fn player_load(app: AppHandle, state: State<'_, PlayerState>, url: String) -> Result<bool, String> {
    let gen = state.generation.fetch_add(1, Ordering::SeqCst) + 1;
    let mpv = state.get_or_init(&app)?;
    mpv.command(&["stop"])?; // 주소를 뽑는 동안 이전 곡은 멈춤

    let stream = ytdlp::resolve_stream(&app, &url).await?;
    if state.generation.load(Ordering::SeqCst) != gen {
        return Ok(false);
    }
    if let Some(ua) = &stream.user_agent {
        let _ = mpv.set_prop("user-agent", ua);
    }
    mpv.set_prop("pause", "no")?;
    mpv.command(&["loadfile", &stream.url, "replace"])?;
    if stream.duration > 0.0 {
        let _ = app.emit("player://duration", json!({ "duration": stream.duration }));
    }
    Ok(true)
}

#[tauri::command]
pub fn player_pause(state: State<'_, PlayerState>, paused: bool) -> Result<(), String> {
    match state.current() {
        Some(m) => m.set_prop("pause", if paused { "yes" } else { "no" }),
        None => Ok(()),
    }
}

#[tauri::command]
pub fn player_seek(state: State<'_, PlayerState>, pos: f64) -> Result<(), String> {
    match state.current() {
        Some(m) => m.command(&["seek", &format!("{:.3}", pos.max(0.0)), "absolute"]),
        None => Ok(()),
    }
}

/// 실제 출력 볼륨 (0~75: 화면 표시값 × 0.75 는 프런트엔드가 계산)
#[tauri::command]
pub fn player_set_volume(state: State<'_, PlayerState>, volume: f64) -> Result<(), String> {
    let v = volume.clamp(0.0, 100.0);
    *state.volume.lock().map_err(|e| e.to_string())? = v;
    match state.current() {
        Some(m) => m.set_prop("volume", &format!("{v:.2}")),
        None => Ok(()),
    }
}

/// 한 곡 반복: mpv 가 직접 처음으로 돌아감
#[tauri::command]
pub fn player_set_loop(state: State<'_, PlayerState>, on: bool) -> Result<(), String> {
    *state.looping.lock().map_err(|e| e.to_string())? = on;
    match state.current() {
        Some(m) => m.set_prop("loop-file", if on { "inf" } else { "no" }),
        None => Ok(()),
    }
}
