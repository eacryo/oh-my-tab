//! 状态栏菜单:菜单项状态(SHORTCUT_ITEM / FIXED_MENU_ITEMS)、菜单动作回调
//! (handle_quit / toggle_shortcut / toggle_theme / reload_config)、以及快捷键模式切换
//! 与菜单标题刷新。setup_status_bar 仍留在 main.rs(装配代码)。
//!
//! Status bar menu: menu-item state (SHORTCUT_ITEM / FIXED_MENU_ITEMS), menu action
//! callbacks (handle_quit / toggle_shortcut / toggle_theme / reload_config), shortcut-mode
//! switching, and menu-title refresh. setup_status_bar stays in main.rs (setup wiring).

use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::atomic::Ordering;

use crate::config::{flush_config_sync, persist_config_now, reload_config, CONFIG};
use crate::event_monitor::SHORTCUT_IS_CMD;
use crate::ffi::*;
use crate::i18n::t;
use crate::icon_cache::clear_icon_cache;
use crate::overlay::extract_uncached_icons;
// 跨模块共享状态(由 main.rs 持有)/ cross-module shared state (owned by main.rs)
use crate::log_info;
use crate::with_tab_state;

// ========== 菜单项状态 / menu-item state ==========

pub(crate) struct ShortcutState {
    pub(crate) item: *mut AnyObject,
}

pub(crate) struct ThumbnailState {
    pub(crate) item: *mut AnyObject,
}

pub(crate) struct ServiceMenuState {
    pub(crate) items: [*mut AnyObject; 5],
}

// 固定标题的菜单项(settings / reload / clear_cache / quit)。locale 变更时由 refresh_menu_titles 批量重设标题。
// Fixed-title menu items (settings / reload / clear_cache / quit); re-titled in bulk by refresh_menu_titles on locale change.
pub(crate) struct FixedMenuItems {
    pub(crate) settings: *mut AnyObject,
    pub(crate) reload: *mut AnyObject,
    pub(crate) clear_cache: *mut AnyObject,
    pub(crate) quit: *mut AnyObject,
}

/// Menu controls are AppKit objects and therefore belong to the main-thread runtime.
/// 菜单控件是 AppKit 对象，只归属主线程 runtime。
pub(crate) struct MenuUi {
    pub(crate) shortcut: Option<ShortcutState>,
    pub(crate) thumbnail: Option<ThumbnailState>,
    pub(crate) services: Option<ServiceMenuState>,
    pub(crate) fixed: Option<FixedMenuItems>,
}

thread_local! {
    static MENU_UI: RefCell<MenuUi> = const { RefCell::new(MenuUi {
        shortcut: None,
        thumbnail: None,
        services: None,
        fixed: None,
    }) };
}

pub(crate) fn with_menu_ui<R>(f: impl FnOnce(&mut MenuUi) -> R) -> R {
    crate::debug_assert_main_thread();
    MENU_UI.with(|ui| f(&mut ui.borrow_mut()))
}

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
    let item = with_menu_ui(|ui| ui.shortcut.as_ref().map(|state| state.item));
    if let Some(item) = item {
        unsafe {
            set_menu_item_title(item, &t(key));
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
    let item = with_menu_ui(|ui| ui.thumbnail.as_ref().map(|state| state.item));
    if let Some(item) = item {
        unsafe {
            set_menu_item_title(item, &t(key));
        }
    }
}

const SERVICE_MENU_TITLE_KEYS: [&str; 5] = [
    "menu.service_windows",
    "menu.service_mouse",
    "menu.service_clipboard",
    "menu.service_window_control",
    "menu.service_quick_actions",
];

const SERVICE_MENU_SYMBOLS: [&str; 5] = [
    "rectangle.on.rectangle",
    "computermouse",
    "doc.text",
    "rectangle.split.2x2",
    "bolt.circle",
];

#[derive(Clone, Copy)]
enum ServiceToggle {
    Windows,
    Mouse,
    Clipboard,
    WindowControl,
    QuickActions,
}

impl ServiceToggle {
    fn from_tag(tag: isize) -> Option<Self> {
        match tag {
            0 => Some(Self::Windows),
            1 => Some(Self::Mouse),
            2 => Some(Self::Clipboard),
            3 => Some(Self::WindowControl),
            4 => Some(Self::QuickActions),
            _ => None,
        }
    }

    fn enabled(self, cfg: &crate::config::Config) -> bool {
        match self {
            Self::Windows => cfg.windows.enabled,
            Self::Mouse => cfg.mouse.enabled,
            Self::Clipboard => cfg.clipboard.enabled,
            Self::WindowControl => cfg.window_control.enabled,
            Self::QuickActions => cfg.quick_actions.enabled,
        }
    }

    fn set_enabled(self, cfg: &mut crate::config::Config, enabled: bool) {
        match self {
            Self::Windows => cfg.windows.enabled = enabled,
            Self::Mouse => cfg.mouse.enabled = enabled,
            Self::Clipboard => cfg.clipboard.enabled = enabled,
            Self::WindowControl => cfg.window_control.enabled = enabled,
            Self::QuickActions => cfg.quick_actions.enabled = enabled,
        }
    }
}

unsafe fn set_menu_item_symbol(item: *mut AnyObject, symbol: &str) {
    let symbol_ns = make_nsstring(symbol);
    let image: *mut AnyObject = msg_send![
        class!(NSImage),
        imageWithSystemSymbolName: symbol_ns,
        accessibilityDescription: std::ptr::null::<AnyObject>()
    ];
    CFRelease(symbol_ns as *const c_void);
    if !image.is_null() {
        let _: () = msg_send![image, setTemplate: true];
        let _: () = msg_send![item, setImage: image];
    }
}

unsafe fn make_menu_item(
    target: *mut AnyObject,
    title: &str,
    action: Sel,
    tag: isize,
    symbol: Option<&str>,
) -> *mut AnyObject {
    let title_ns = make_nsstring(title);
    let key_ns = make_nsstring("");
    let item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
    let item: *mut AnyObject =
        msg_send![item, initWithTitle: title_ns, action: action, keyEquivalent: key_ns];
    CFRelease(title_ns as *const c_void);
    CFRelease(key_ns as *const c_void);
    let _: () = msg_send![item, setTarget: target];
    let _: () = msg_send![item, setTag: tag];
    set_menu_item_title(item, title);
    if let Some(symbol) = symbol {
        set_menu_item_symbol(item, symbol);
    }
    item
}

/// Build the directly expanded service-toggle section between Settings and shortcut mode.
/// 构建位于“设置”和快捷键模式之间、直接展开的五个大类开关。
pub(crate) unsafe fn build_service_menu(menu: *mut AnyObject, target: *mut AnyObject) {
    let items: [*mut AnyObject; 5] = std::array::from_fn(|index| {
        let item = make_menu_item(
            target,
            &t(SERVICE_MENU_TITLE_KEYS[index]),
            sel!(handleToggleService:),
            index as isize,
            Some(SERVICE_MENU_SYMBOLS[index]),
        );
        // 使用原生 NSMenuItem image/title/state，交由 AppKit 统一处理列对齐、悬停和点击。
        // Use native NSMenuItem image/title/state so AppKit owns column alignment, hover, and clicks.
        let _: () = msg_send![item, setState: 0isize];
        let _: () = msg_send![menu, addItem: item];
        item
    });
    with_menu_ui(|ui| {
        ui.services = Some(ServiceMenuState { items });
    });
    refresh_service_menu();
}

pub(crate) fn refresh_service_menu() {
    let enabled: [bool; 5] = {
        let cfg = CONFIG.read().unwrap();
        std::array::from_fn(|index| {
            ServiceToggle::from_tag(index as isize)
                .expect("service menu index")
                .enabled(&cfg)
        })
    };
    let Some(items) = with_menu_ui(|ui| ui.services.as_ref().map(|state| state.items)) else {
        return;
    };
    unsafe {
        for (index, item) in items.into_iter().enumerate() {
            let title = t(SERVICE_MENU_TITLE_KEYS[index]);
            set_menu_item_title(item, &title);
            let _: () = msg_send![item, setState: if enabled[index] { 1isize } else { 0isize }];
        }
    }
}

pub(crate) extern "C" fn handle_toggle_service(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    if sender.is_null() {
        return;
    }
    let tag: isize = unsafe { msg_send![sender as *mut AnyObject, tag] };
    let Some(service) = ServiceToggle::from_tag(tag) else {
        return;
    };
    let old_cfg = CONFIG.read().unwrap().clone();
    let mut new_cfg = old_cfg.clone();
    let enabled = !service.enabled(&old_cfg);
    service.set_enabled(&mut new_cfg, enabled);
    if let Ok(mut current) = CONFIG.write() {
        *current = new_cfg.clone();
    }
    persist_config_now();
    crate::runtime_config::apply_config_change(
        &old_cfg,
        &new_cfg,
        crate::runtime_config::ConfigChangeSource::Menu,
    );
    crate::settings::refresh_service_controls_from_config();
    refresh_service_menu();
    log_info!(
        "Service {:?}: {}",
        tag,
        if enabled { "enabled" } else { "disabled" }
    );
}

/// 用当前 locale 与状态重设全部菜单项标题。用于 locale 变更(reload)与启动时修正初始标签。
/// Re-title all menu items from the current locale and state. Used on locale change (reload)
/// and at startup to fix the initial labels.
pub(crate) fn refresh_menu_titles() {
    set_thumbnail_mode(CONFIG.read().unwrap().layout.thumbnails_enabled);
    refresh_service_menu();
    unsafe {
        // shortcut item
        let is_cmd = SHORTCUT_IS_CMD.load(Ordering::SeqCst);
        let sc_key = if is_cmd {
            "menu.toggle_shortcut.opt"
        } else {
            "menu.toggle_shortcut.cmd"
        };
        let shortcut = with_menu_ui(|ui| ui.shortcut.as_ref().map(|state| state.item));
        if let Some(item) = shortcut {
            set_menu_item_title(item, &t(sc_key));
        }
        // 固定标题项 / fixed-title items
        let fixed = with_menu_ui(|ui| {
            ui.fixed
                .as_ref()
                .map(|items| (items.settings, items.reload, items.clear_cache, items.quit))
        });
        if let Some((settings, reload, clear_cache, quit)) = fixed {
            for (item, key) in [
                (settings, "menu.settings"),
                (reload, "menu.reload_config"),
                (clear_cache, "menu.clear_icon_cache"),
                (quit, "menu.quit"),
            ] {
                set_menu_item_title(item, &t(key));
            }
        }
    }
}

pub(crate) extern "C" fn handle_quit(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    log_info!("User quit via menu bar.");
    quit_application();
}

/// Flush state, restore system pointer settings, and terminate the accessory application.
/// Flush 状态、恢复系统指针设置并退出辅助应用。
pub(crate) fn quit_application() {
    if let Err(e) = flush_config_sync() {
        log_info!("Config flush before quit failed: {}", e);
    }
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
    let old_cfg = CONFIG.read().unwrap().clone();
    let new_cfg = {
        let mut cfg = old_cfg.clone();
        cfg.keyboard.modifier = if is_cmd {
            "command".to_string()
        } else {
            "option".to_string()
        };
        if let Ok(mut current) = CONFIG.write() {
            *current = cfg.clone();
        }
        cfg
    };
    persist_config_now();
    crate::runtime_config::apply_config_change(
        &old_cfg,
        &new_cfg,
        crate::runtime_config::ConfigChangeSource::Menu,
    );
    log_info!("Shortcut: {}", if is_cmd { "Cmd+Tab" } else { "Opt+Tab" });
}

pub(crate) extern "C" fn handle_toggle_thumbnail(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    let thumbnails_enabled = !crate::theme::thumbnails_enabled();
    let old_cfg = CONFIG.read().unwrap().clone();
    let new_cfg = {
        let mut cfg = old_cfg.clone();
        cfg.layout.thumbnails_enabled = thumbnails_enabled;
        if let Ok(mut current) = CONFIG.write() {
            *current = cfg.clone();
        }
        cfg
    };
    persist_config_now();
    // 模式改变后丢弃当前卡片布局,下次召唤按新模式重建;关闭时同时释放内存截图。
    // Drop the current card layout so the next summon rebuilds in the new mode; disabling
    // thumbnails also releases the in-memory window images.
    crate::runtime_config::apply_config_change(
        &old_cfg,
        &new_cfg,
        crate::runtime_config::ConfigChangeSource::Menu,
    );
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
    with_tab_state(|state_opt| {
        if let Some(state) = state_opt.as_mut() {
            for window in &mut state.windows {
                window.icon_path = None;
            }
        }
    });
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
