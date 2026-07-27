//! 状态栏菜单:菜单项状态(SHORTCUT_ITEM / FIXED_MENU_ITEMS)、菜单动作回调
//! (handle_quit / toggle_shortcut / toggle_theme / reload_config)、以及快捷键模式切换
//! 与菜单标题刷新。setup_status_bar 仍留在 main.rs(装配代码)。
//!
//! Status bar menu: menu-item state (SHORTCUT_ITEM / FIXED_MENU_ITEMS), menu action
//! callbacks (handle_quit / toggle_shortcut / toggle_theme / reload_config), shortcut-mode
//! switching, and menu-title refresh. setup_status_bar stays in main.rs (setup wiring).

use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send};
use std::ffi::c_void;
use std::sync::atomic::Ordering;
use std::sync::Mutex;

use crate::config::{reload_config, CONFIG};
use crate::event_monitor::{GlobalEvent, SHORTCUT_IS_CMD};
use crate::ffi::*;
use crate::i18n::t;
use crate::overlay::{apply_theme, extract_uncached_icons, refresh_highlight, update_status_label};
// invalidate_settings_window 由 settings.rs 提供 / provided by settings.rs
use crate::settings::invalidate_settings_window;
use crate::window_collector::clear_icon_cache;
// 跨模块共享状态(由 main.rs 持有)/ cross-module shared state (owned by main.rs)
use crate::{log_info, log_warn};
use crate::{STATUS_EVENT_TX, TAB_STATE, THEME_STATE};

// ========== 菜单项状态 / menu-item state ==========

pub(crate) struct ShortcutState {
    pub(crate) item: *mut AnyObject,
}
unsafe impl Send for ShortcutState {}
unsafe impl Sync for ShortcutState {}
pub(crate) static SHORTCUT_ITEM: Mutex<Option<ShortcutState>> = Mutex::new(None);

// 固定标题的菜单项(settings / reload / clear_cache / quit)。locale 变更时由 refresh_menu_titles 批量重设标题。
// Fixed-title menu items (settings / reload / clear_cache / quit); re-titled in bulk by refresh_menu_titles on locale change.
pub(crate) struct FixedMenuItems {
    pub(crate) settings: *mut AnyObject,
    pub(crate) reload: *mut AnyObject,
    pub(crate) clear_cache: *mut AnyObject,
    pub(crate) quit: *mut AnyObject,
}
unsafe impl Send for FixedMenuItems {}
unsafe impl Sync for FixedMenuItems {}
pub(crate) static FIXED_MENU_ITEMS: Mutex<Option<FixedMenuItems>> = Mutex::new(None);

// ========== 菜单动作 / menu actions ==========

/// 设置快捷键模式(Cmd / Opt),同步运行时状态 SHORTCUT_IS_CMD 与菜单标签。
/// Set shortcut mode (Cmd / Opt), syncing runtime SHORTCUT_IS_CMD and the menu label.
pub(crate) fn set_shortcut_mode(is_cmd: bool) {
    SHORTCUT_IS_CMD.store(is_cmd, Ordering::SeqCst);
    let key = if is_cmd {
        "menu.toggle_shortcut.opt"
    } else {
        "menu.toggle_shortcut.cmd"
    };
    if let Some(ref s) = *SHORTCUT_ITEM.lock().unwrap() {
        unsafe {
            let ns_title = make_nsstring(&t(key));
            let _: () = msg_send![s.item, setTitle: ns_title];
            CFRelease(ns_title as *const c_void);
        }
    }
}

/// 用当前 locale 与状态重设全部菜单项标题。用于 locale 变更(reload)与启动时修正初始标签。
/// Re-title all menu items from the current locale and state. Used on locale change (reload)
/// and at startup to fix the initial labels.
pub(crate) fn refresh_menu_titles() {
    unsafe {
        // theme item:标题取决于当前主题(点一下要切到另一边)
        // theme item: label depends on current theme (clicking switches to the other)
        let is_dark = CONFIG.read().unwrap().appearance.theme.as_str() != "light";
        let theme_key = if is_dark {
            "menu.toggle_theme.light"
        } else {
            "menu.toggle_theme.dark"
        };
        if let Some(ref mut s) = *THEME_STATE.lock().unwrap() {
            s.is_dark = is_dark;
            let ns_title = make_nsstring(&t(theme_key));
            let _: () = msg_send![s.item, setTitle: ns_title];
            CFRelease(ns_title as *const c_void);
        }
        // shortcut item
        let is_cmd = SHORTCUT_IS_CMD.load(Ordering::SeqCst);
        let sc_key = if is_cmd {
            "menu.toggle_shortcut.opt"
        } else {
            "menu.toggle_shortcut.cmd"
        };
        if let Some(ref s) = *SHORTCUT_ITEM.lock().unwrap() {
            let ns_title = make_nsstring(&t(sc_key));
            let _: () = msg_send![s.item, setTitle: ns_title];
            CFRelease(ns_title as *const c_void);
        }
        // 固定标题项 / fixed-title items
        if let Some(ref items) = *FIXED_MENU_ITEMS.lock().unwrap() {
            for (item, key) in [
                (items.settings, "menu.settings"),
                (items.reload, "menu.reload_config"),
                (items.clear_cache, "menu.clear_icon_cache"),
                (items.quit, "menu.quit"),
            ] {
                let ns_title = make_nsstring(&t(key));
                let _: () = msg_send![item, setTitle: ns_title];
                CFRelease(ns_title as *const c_void);
            }
        }
    }
}

pub(crate) extern "C" fn handle_quit(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    log_info!("User quit via menu bar.");
    unsafe {
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![nsapp, terminate: std::ptr::null::<AnyObject>()];
    }
}

// 设置里「缺权限」警告条的「打开隐私与安全性」按钮回调。
// Handler for the "Open Privacy & Security" button on the settings permission-warning banner.
pub(crate) extern "C" fn handle_open_privacy(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    crate::open_privacy_accessibility();
}

pub(crate) extern "C" fn handle_toggle_shortcut(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    let is_cmd = !SHORTCUT_IS_CMD.load(Ordering::SeqCst);
    // 持久化到 config(与主题切换一致,重启后保留用户选择)。
    // Persist to config (matches theme toggle, so the choice survives restart).
    {
        let mut cfg = CONFIG.write().unwrap();
        cfg.keyboard.modifier = if is_cmd {
            "command".to_string()
        } else {
            "option".to_string()
        };
        let _ = cfg.save();
    }
    set_shortcut_mode(is_cmd);
    log_info!("Shortcut: {}", if is_cmd { "Cmd+Tab" } else { "Opt+Tab" });
}

pub(crate) extern "C" fn handle_toggle_theme(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    // Flip theme in CONFIG and persist to file so menu <-> config are linked.
    let new_theme = match CONFIG.read().unwrap().appearance.theme.as_str() {
        "dark" => "light",
        _ => "dark",
    };
    {
        let mut cfg = CONFIG.write().unwrap();
        cfg.appearance.theme = new_theme.to_string();
        // Save to file
        let path = {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            std::path::PathBuf::from(home).join(".config/oh-my-tab/config.toml")
        };
        if let Ok(toml_str) = toml::to_string_pretty(&*cfg) {
            let _ = std::fs::write(&path, toml_str);
        }
    }
    let is_dark = new_theme == "dark";
    let new_label = if is_dark {
        t("menu.toggle_theme.light")
    } else {
        t("menu.toggle_theme.dark")
    };
    log_info!(
        "Toggled theme to {}",
        if is_dark { "dark" } else { "light" }
    );
    // Update menu item title
    let mut state = THEME_STATE.lock().unwrap();
    if let Some(ref mut s) = *state {
        s.is_dark = is_dark;
        unsafe {
            let ns_title = make_nsstring(&new_label);
            let _: () = msg_send![s.item, setTitle: ns_title];
            CFRelease(ns_title as *const c_void);
        }
    }
    drop(state);
    if let Some(tx) = STATUS_EVENT_TX.get() {
        let _ = tx.send(GlobalEvent::ThemeToggled);
    }
}

pub(crate) extern "C" fn handle_reload_config(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    let errs = reload_config();
    if errs.is_empty() {
        log_info!("Config reloaded successfully.");
    } else {
        log_warn!("Config reload: {} error(s):", errs.len());
        for e in &errs {
            log_warn!("  • {}", e);
        }
    }
    // 同步快捷键模式:手动改 config 的 keyboard.modifier 后 Reload 也要生效。必须在
    // refresh_menu_titles 之前,让菜单标题刷新时读到正确值。
    // Sync shortcut mode: a manual edit of config.keyboard.modifier must take effect on Reload.
    // Must run before refresh_menu_titles so the label refresh sees the correct value.
    set_shortcut_mode(CONFIG.read().unwrap().keyboard.modifier == "command");
    // 同步开机自启:手动改 config 的 [startup] launch_at_login 后 Reload 也要生效。
    // Sync launch-at-login: a manual edit of [startup] launch_at_login must take effect on Reload.
    crate::autostart::sync(CONFIG.read().unwrap().startup.launch_at_login);
    // locale 可能随 reload 改变:重设全部菜单标题 + 作废设置窗口待下次按新 locale 重建
    // locale may change on reload: re-title all menus + invalidate the settings window
    // so it rebuilds with the new locale on next open
    refresh_menu_titles();
    invalidate_settings_window();
    // Apply immediately
    apply_theme();
    refresh_highlight();
    update_status_label();
}

/// 清空图标缓存:删除缓存目录里所有 {pid}.png,失效内存里的 icon_path,
/// 并立即重新提取当前窗口的图标(浮窗可见时 rebuild_cards 会就地刷新卡片)。
///
/// Clear the icon cache: remove all {pid}.png from the cache dir, invalidate in-memory
/// icon_path, and re-extract icons for current windows immediately (rebuild_cards refreshes
/// the cards in place if the overlay is visible).
pub(crate) extern "C" fn handle_clear_icon_cache(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    clear_icon_cache();
    // 内存里的 icon_path 仍指向已删除的文件,置 None 让卡片重新走提取流程。
    // in-memory icon_path still points at deleted files; reset to None so cards re-extract.
    {
        let mut state_opt = TAB_STATE.lock().unwrap();
        if let Some(ref mut state) = *state_opt {
            for w in state.windows.iter_mut() {
                w.icon_path = None;
            }
        }
    }
    // 立即重新提取当前窗口的图标(仅当前已收集的窗口,非全部运行中 App)。
    // Re-extract icons for currently-collected windows only (not all running apps).
    extract_uncached_icons();
    log_info!("Icon cache cleared.");
}
