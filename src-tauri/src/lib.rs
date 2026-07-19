#![allow(dead_code)]

mod permissions;
mod event_monitor;
mod window_collector;
mod switcher;

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use tauri::{Emitter, Manager};

use window_collector::{MruMap, WindowInfo};

pub struct AppState {
    pub mru: Arc<Mutex<MruMap>>,
}

impl AppState {
    pub fn new() -> Self {
        AppState {
            mru: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[tauri::command]
fn get_windows(state: tauri::State<AppState>) -> Vec<WindowInfo> {
    let mru = state.mru.lock();
    window_collector::collect_windows(&mru)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            permissions::check_accessibility_permission();
            println!("[setup] Permissions OK, starting MRU poller...");

            let mru = Arc::new(Mutex::new(MruMap::new()));
            window_collector::start_mru_poller(mru.clone());

            app.manage(AppState { mru });

            let app_handle = app.handle().clone();
            app_handle.emit("app-ready", ()).ok();
            println!("[setup] Oh My Tab ready");

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![get_windows])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
