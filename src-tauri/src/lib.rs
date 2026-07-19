#![allow(dead_code)]

mod permissions;
mod window_collector;
mod switcher;

use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;

use parking_lot::Mutex;
use tauri::{Emitter, Manager};
use window_collector::{MruMap, WindowInfo};

macro_rules! log {
    ($($arg:tt)*) => {{
        eprint!("[LOG] ");
        eprintln!($($arg)*);
        std::io::stderr().flush().ok();
    }};
}

const CMD_KEY: u32 = 91; // Meta key (Command on macOS)

pub struct AppState {
    pub mru: Arc<Mutex<MruMap>>,
    pub cached_windows: Arc<Mutex<Vec<WindowInfo>>>,
}

impl AppState {
    pub fn new() -> Self {
        AppState {
            mru: Arc::new(Mutex::new(HashMap::new())),
            cached_windows: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[tauri::command]
fn get_windows(state: tauri::State<AppState>) -> Vec<WindowInfo> {
    state.cached_windows.lock().clone()
}

#[tauri::command]
fn activate_window(pid: i32) -> Result<bool, String> {
    let cls = objc2::class!(NSRunningApplication);
    let app: Option<objc2::rc::Retained<objc2::runtime::AnyObject>> =
        unsafe { objc2::msg_send![cls, runningApplicationWithProcessIdentifier: pid] };
    let Some(app) = app else {
        return Ok(false);
    };
    let opts: u64 = 1;
    unsafe {
        let _: () = objc2::msg_send![&app, activateWithOptions: opts];
    }
    Ok(true)
}

#[tauri::command]
fn dismiss_overlay(app: tauri::AppHandle) -> Result<(), String> {
    app.emit("hide-overlay", ()).map_err(|e| e.to_string())
}

#[tauri::command]
fn resize_overlay(app: tauri::AppHandle, width: f64, height: f64) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("overlay") {
        use tauri::{LogicalPosition, LogicalSize};
        w.set_size(LogicalSize::new(width, height)).map_err(|e| e.to_string())?;
        if let Ok(Some(monitor)) = app.primary_monitor() {
            let s = monitor.size();
            let scale = monitor.scale_factor();
            let sw = s.width as f64 / scale;
            let sh = s.height as f64 / scale;
            let x = (sw - width) / 2.0;
            let y = (sh - height) / 2.0;
            if x >= 0.0 && y >= 0.0 {
                w.set_position(LogicalPosition::new(x, y)).map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    std::panic::set_hook(Box::new(|info| {
        eprintln!("!!! PANIC: {}", info);
        if let Some(loc) = info.location() {
            eprintln!("!!! at {}:{}:{}", loc.file(), loc.line(), loc.column());
        }
        std::io::stderr().flush().ok();
    }));

    log!("run() start");

    tauri::Builder::default()
        .setup(|app| {
            log!("setup start");
            permissions::check_accessibility_permission();

            let mru = Arc::new(Mutex::new(MruMap::new()));
            let cached = Arc::new(Mutex::new(Vec::<WindowInfo>::new()));

            let wins = window_collector::collect_windows(&mut *mru.lock());
            *cached.lock() = wins;
            log!("initial scan: {} windows", cached.lock().len());

            app.manage(AppState {
                mru: mru.clone(),
                cached_windows: cached.clone(),
            });

            if let Some(w) = app.get_webview_window("overlay") {
                use objc2::msg_send;
                use objc2::rc::Retained;
                use objc2::runtime::AnyObject;
                let ns_win = w.ns_window().expect("ns_window failed");
                let ns_win = unsafe { &*(ns_win as *const AnyObject) };
                unsafe {
                    let _: () = msg_send![ns_win, setHasShadow: true];
                    let content_view: Retained<AnyObject> = msg_send![ns_win, contentView];
                    let _: () = msg_send![&content_view, setWantsLayer: true];
                    let layer: Retained<AnyObject> = msg_send![&content_view, layer];
                    let _: () = msg_send![&layer, setCornerRadius: 14.0f64];
                    let _: () = msg_send![&layer, setMasksToBounds: true];
                }
                log!("rounded corners applied to overlay window");
            }

            log!("(CGEventTap skipped - using frontend keyboard handling)");
            log!("setup complete, Oh My Tab ready");

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_windows,
            activate_window,
            dismiss_overlay,
            resize_overlay
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
