//! 状态栏菜单:菜单项状态(SHORTCUT_ITEM / FIXED_MENU_ITEMS)、菜单动作回调
//! (handle_quit / toggle_shortcut / toggle_theme / reload_config)、以及快捷键模式切换
//! 与菜单标题刷新。setup_status_bar 仍留在 main.rs(装配代码)。
//!
//! Status bar menu: menu-item state (SHORTCUT_ITEM / FIXED_MENU_ITEMS), menu action
//! callbacks (handle_quit / toggle_shortcut / toggle_theme / reload_config), shortcut-mode
//! switching, and menu-title refresh. setup_status_bar stays in main.rs (setup wiring).

use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::ffi::c_void;
use std::sync::atomic::Ordering;
use std::sync::Mutex;

use crate::config::{reload_config, CONFIG};
use crate::event_monitor::SHORTCUT_IS_CMD;
use crate::ffi::*;
use crate::i18n::t;
use crate::overlay::{apply_theme, extract_uncached_icons, refresh_highlight, update_status_label};
// invalidate_settings_window 由 settings.rs 提供 / provided by settings.rs
use crate::icon_cache::clear_icon_cache;
use crate::settings::invalidate_settings_window;
// 跨模块共享状态(由 main.rs 持有)/ cross-module shared state (owned by main.rs)
use crate::log_info;
use crate::TAB_STATE;

// ========== 菜单项状态 / menu-item state ==========

pub(crate) struct ShortcutState {
    pub(crate) item: *mut AnyObject,
}
unsafe impl Send for ShortcutState {}
unsafe impl Sync for ShortcutState {}
pub(crate) static SHORTCUT_ITEM: Mutex<Option<ShortcutState>> = Mutex::new(None);

pub(crate) struct ThumbnailState {
    pub(crate) item: *mut AnyObject,
}
unsafe impl Send for ThumbnailState {}
unsafe impl Sync for ThumbnailState {}
pub(crate) static THUMBNAIL_ITEM: Mutex<Option<ThumbnailState>> = Mutex::new(None);

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

const MENU_TITLE_MIN_WIDTH: f64 = 72.0;
const MENU_TITLE_MAX_WIDTH: f64 = 280.0;

fn capped_menu_title_width(natural_width: f64) -> f64 {
    if natural_width.is_finite() {
        natural_width
            .ceil()
            .clamp(MENU_TITLE_MIN_WIDTH, MENU_TITLE_MAX_WIDTH)
    } else {
        MENU_TITLE_MIN_WIDTH
    }
}

/// Measure a title using the native menu font without attaching a custom view to the item.
/// 使用原生菜单字体测量标题,但不向菜单项附加自定义 view。
unsafe fn menu_title_width(title: &str) -> f64 {
    let field: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let field: *mut AnyObject = msg_send![
        field,
        initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 20.0))
    ];
    if field.is_null() {
        return MENU_TITLE_MIN_WIDTH;
    }
    let title_ns = make_nsstring(title);
    let _: () = msg_send![field, setStringValue: title_ns];
    CFRelease(title_ns as *const c_void);
    let _: () = msg_send![field, setUsesSingleLineMode: true];
    let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 13.0f64];
    let _: () = msg_send![field, setFont: font];
    let cell: *mut AnyObject = msg_send![field, cell];
    let size: NSSize = if cell.is_null() {
        NSSize::new(MENU_TITLE_MIN_WIDTH, 16.0)
    } else {
        msg_send![cell, cellSize]
    };
    release_obj(field);
    size.width
}

/// Keep menu titles compact while retaining the full title in the item's tooltip.
/// 保持菜单标题紧凑,同时把完整标题保存在菜单项 tooltip 中。
fn compact_menu_title(title: &str) -> String {
    if capped_menu_title_width(unsafe { menu_title_width(title) }) <= MENU_TITLE_MAX_WIDTH {
        return title.to_string();
    }

    let chars: Vec<char> = title.chars().collect();
    let mut low = 0usize;
    let mut high = chars.len();
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        let prefix: String = chars[..mid].iter().collect();
        let candidate = format!("{prefix}…");
        if capped_menu_title_width(unsafe { menu_title_width(&candidate) }) <= MENU_TITLE_MAX_WIDTH
        {
            low = mid;
        } else {
            high = mid - 1;
        }
    }

    if low == 0 {
        "…".to_string()
    } else {
        let prefix: String = chars[..low].iter().collect();
        format!("{prefix}…")
    }
}

/// Set a menu item's native title, truncating only oversized localized text.
/// 设置菜单项的原生标题,只截断超出宽度的本地化文本。
pub(crate) unsafe fn set_menu_item_title(item: *mut AnyObject, title: &str) {
    if item.is_null() {
        return;
    }

    let display_title = compact_menu_title(title);
    let title_ns = make_nsstring(&display_title);
    let _: () = msg_send![item, setTitle: title_ns];
    CFRelease(title_ns as *const c_void);

    if display_title == title {
        let _: () = msg_send![item, setToolTip: std::ptr::null::<AnyObject>()];
    } else {
        let tooltip_ns = make_nsstring(title);
        let _: () = msg_send![item, setToolTip: tooltip_ns];
        CFRelease(tooltip_ns as *const c_void);
    }
}

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
            set_menu_item_title(s.item, &t(key));
        }
    }
}

/// 设置缩略图菜单项标题,标题表示点击后将切换到的模式。
/// Set the thumbnail menu item's title; the title describes the mode activated by the click.
pub(crate) fn set_thumbnail_mode(thumbnails_enabled: bool) {
    let key = if thumbnails_enabled {
        "menu.toggle_thumbnail_mode.to_icons"
    } else {
        "menu.toggle_thumbnail_mode.to_thumbnails"
    };
    if let Some(ref s) = *THUMBNAIL_ITEM.lock().unwrap() {
        unsafe {
            set_menu_item_title(s.item, &t(key));
        }
    }
}

/// 用当前 locale 与状态重设全部菜单项标题。用于 locale 变更(reload)与启动时修正初始标签。
/// Re-title all menu items from the current locale and state. Used on locale change (reload)
/// and at startup to fix the initial labels.
pub(crate) fn refresh_menu_titles() {
    set_thumbnail_mode(CONFIG.read().unwrap().layout.thumbnails_enabled);
    unsafe {
        // shortcut item
        let is_cmd = SHORTCUT_IS_CMD.load(Ordering::SeqCst);
        let sc_key = if is_cmd {
            "menu.toggle_shortcut.opt"
        } else {
            "menu.toggle_shortcut.cmd"
        };
        if let Some(ref s) = *SHORTCUT_ITEM.lock().unwrap() {
            set_menu_item_title(s.item, &t(sc_key));
        }
        // 固定标题项 / fixed-title items
        if let Some(ref items) = *FIXED_MENU_ITEMS.lock().unwrap() {
            for (item, key) in [
                (items.settings, "menu.settings"),
                (items.reload, "menu.reload_config"),
                (items.clear_cache, "menu.clear_icon_cache"),
                (items.quit, "menu.quit"),
            ] {
                set_menu_item_title(item, &t(key));
            }
        }
    }
}

pub(crate) extern "C" fn handle_quit(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    log_info!("User quit via menu bar.");
    // 退出前恢复指针加速设置(否则系统鼠标保持线性,直到用户手动重置)。
    // Restore pointer acceleration settings before quitting (otherwise the mouse stays
    // linear until the user resets it manually).
    crate::mouse::pointer::restore();
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

pub(crate) extern "C" fn handle_toggle_thumbnail(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    let thumbnails_enabled = !crate::theme::thumbnails_enabled();
    {
        let mut cfg = CONFIG.write().unwrap();
        cfg.layout.thumbnails_enabled = thumbnails_enabled;
        if let Err(err) = cfg.save() {
            log_info!("Failed to save thumbnail mode: {}", err);
            return;
        }
    }
    set_thumbnail_mode(thumbnails_enabled);
    // 模式改变后丢弃当前卡片布局,下次召唤按新模式重建;关闭时同时释放内存截图。
    // Drop the current card layout so the next summon rebuilds in the new mode; disabling
    // thumbnails also releases the in-memory window images.
    crate::overlay::reset_switcher();
    crate::overlay::apply_theme();
    crate::overlay::refresh_highlight();
    crate::overlay::update_status_label();
    if thumbnails_enabled {
        crate::thumbnail::start();
    } else {
        crate::thumbnail::clear_runtime_cache();
    }
    log_info!(
        "Window thumbnails: {}",
        if thumbnails_enabled {
            "enabled"
        } else {
            "disabled"
        }
    );
}

pub(crate) extern "C" fn handle_reload_config(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    let errs = reload_config();
    if errs.is_empty() {
        log_info!("Config reloaded successfully.");
    } else {
        log_info!("Config reload: {} error(s):", errs.len());
        for e in &errs {
            log_info!("  • {}", e);
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
    crate::clipboard::refresh_localized_ui();
    // Apply immediately
    apply_theme();
    refresh_highlight();
    update_status_label();
    // 重新应用指针加速设置(reload 后配置可能变化)。
    // Re-apply pointer acceleration settings (config may have changed on reload).
    crate::mouse::pointer::apply();
    if CONFIG.read().unwrap().layout.thumbnails_enabled {
        crate::thumbnail::start();
    } else {
        crate::thumbnail::clear_runtime_cache();
    }
}

/// 清空图标缓存:删除缓存目录里所有缓存文件({key}.png + {key}.meta),失效内存里的 icon_path,
/// 并立即重新提取当前窗口的图标(浮窗可见时 rebuild_cards 会就地刷新卡片)。
///
/// Clear the icon cache: remove all cached files ({key}.png + {key}.meta) from the cache dir,
/// invalidate in-memory icon_path, and re-extract icons for current windows immediately
/// (rebuild_cards refreshes the cards in place if the overlay is visible).
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

#[cfg(test)]
mod tests {
    use super::{capped_menu_title_width, MENU_TITLE_MAX_WIDTH, MENU_TITLE_MIN_WIDTH};

    #[test]
    fn menu_title_width_stays_compact_until_the_maximum() {
        assert_eq!(capped_menu_title_width(40.0), MENU_TITLE_MIN_WIDTH);
        assert_eq!(capped_menu_title_width(120.4), 121.0);
        assert_eq!(capped_menu_title_width(10_000.0), MENU_TITLE_MAX_WIDTH);
        assert_eq!(capped_menu_title_width(f64::NAN), MENU_TITLE_MIN_WIDTH);
    }
}
