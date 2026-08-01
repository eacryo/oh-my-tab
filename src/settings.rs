//! 设置窗口:SettingsUi 状态、控件构造器(text/popup/header/row)、窗口构建/显示/收集、
//! 校验告警、以及配置热应用(apply_config_refresh)。invalidate_settings_window 作废缓存
//! 窗口供 locale 变更后重建。
//!
//! Settings window: SettingsUi state, control builders (text/popup/header/row), window
//! build/show/collect, validation alerts, and hot config application (apply_config_refresh).
//! invalidate_settings_window drops the cached window so it rebuilds after a locale change.

use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::ffi::{c_void, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::config::{reload_config, Config, CONFIG};
use crate::event_monitor::SHORTCUT_IS_CMD;
use crate::ffi::*;
use crate::i18n::{t, tf};
use crate::log_info;
use crate::menu::{refresh_menu_titles, set_shortcut_mode};
use crate::overlay::{apply_theme, refresh_highlight, update_status_label};
// 跨模块共享状态(由 main.rs 持有)/ cross-module shared state (owned by main.rs)
use crate::MENU_TARGET;

// locale 下拉项:显示用各语言原生写法(语言选择器的通用约定),值对应 config.i18n.locale。
// Locale popup items: displayed in each language's own script (convention for language pickers);
// values map to config.i18n.locale.
const LOCALE_LABELS: [&str; 4] = ["Auto", "English", "简体中文", "繁體中文"];
const SCROLL_MODE_LABELS: [&str; 3] = ["Default", "Line", "Smooth"];
const SCROLL_MODE_VALUES: [&str; 3] = ["default", "line", "smooth"];
const SMOOTH_PRESET_LABELS: [&str; 13] = [
    "easeInOut",
    "easeIn",
    "easeOut",
    "linear",
    "quadratic",
    "cubic",
    "easeOutCubic",
    "easeInOutCubic",
    "quartic",
    "easeOutQuartic",
    "easeInOutQuartic",
    "smooth",
    "custom",
];
const LOCALE_VALUES: [&str; 4] = ["auto", "en", "zh-Hans", "zh-Hant"];

// ========== 设置窗口状态 / settings window state ==========

// 设置窗口的控件指针集合（非模态窗口，复用，隐藏而非销毁）。
// Holds pointers to the settings window's controls (non-modal, reused, hidden not destroyed).
struct SettingsUi {
    window: *mut AnyObject,
    sidebar_general: *mut AnyObject, // NSButton: 通用 / General (tag=0)
    sidebar_experimental: *mut AnyObject, // NSButton: 实验性功能 / Experimental (tag=1)
    sidebar_mouse: *mut AnyObject,   // NSButton: 鼠标控制 / Mouse (tag=2)
    sidebar_highlight: *mut AnyObject, // NSView: 选中行高亮背景 (layer-backed)
    general_view: *mut AnyObject,    // NSView: 通用页容器 / General page container
    experimental_view: *mut AnyObject, // NSView: 实验性页容器 / Experimental page container
    mouse_view: *mut AnyObject,      // NSView: 鼠标页容器 / Mouse page container
    theme: *mut AnyObject,           // NSPopUpButton: dark / light / auto
    glass_style: *mut AnyObject,     // NSPopUpButton: regular / clear
    glass_tint: *mut AnyObject,      // NSTextField: RRGGBBAA hex
    corner_radius: *mut AnyObject,   // NSTextField
    cards_per_row: *mut AnyObject,
    card_width: *mut AnyObject,
    card_height: *mut AnyObject,
    card_gap: *mut AnyObject,
    icon_size: *mut AnyObject,
    modifier: *mut AnyObject,        // NSPopUpButton: option / command
    locale: *mut AnyObject,          // NSPopUpButton: auto / en / zh-Hans / zh-Hant
    show_minimized: *mut AnyObject, // NSPopUpButton: 不显示 / 显示 / show minimized windows (hide / show)
    log_level: *mut AnyObject,      // NSPopUpButton: trace / debug / info / warn / error
    launch_at_login: *mut AnyObject, // NSButton (checkbox): 开机自启 / launch at login
    reverse_scroll: *mut AnyObject, // NSButton (checkbox): 反转滚动 / reverse scrolling
    enable_mouse: *mut AnyObject,   // NSButton (checkbox): 启用鼠标控制 / enable mouse control
    scroll_mode: *mut AnyObject,      // NSPopUpButton: default/line/smooth
    line_count: *mut AnyObject,       // NSTextField: line count input
    smooth_preset: *mut AnyObject,    // NSPopUpButton: smooth preset
    disable_pointer_accel: *mut AnyObject, // NSButton (checkbox): 禁用指针加速 / disable pointer acceleration
    ok_button: *mut AnyObject,        // NSButton: 确认按钮 / OK button
    accessibility_warning_view: *mut AnyObject, // NSView: 缺权限警告条容器 / permission-warning banner container
}
unsafe impl Send for SettingsUi {}
unsafe impl Sync for SettingsUi {}
static SETTINGS_UI: Mutex<Option<SettingsUi>> = Mutex::new(None);

// summon 期间是否临时藏起了设置窗口(orderOut),避免它贴在浮窗后面露出来。
// stash 时置 true;restore/push_settings_to_back 时 swap 回 false。
// Whether the settings window was temporarily orderOut'd during summon so it doesn't
// peek out behind the overlay. Set true on stash, swapped back to false on restore /
// push_settings_to_back.
static SETTINGS_HIDDEN_FOR_SUMMON: AtomicBool = AtomicBool::new(false);

// ========== 控件构造 helper / control-builder helpers ==========

fn parse_f64(s: &str) -> Result<f64, ()> {
    s.trim().parse::<f64>().map_err(|_| ())
}
fn parse_usize(s: &str) -> Result<usize, ()> {
    s.trim().parse::<usize>().map_err(|_| ())
}

/// 设置控件标题并释放临时 NSString。
/// Set a control's title and release the temporary NSString.
unsafe fn set_control_title(obj: *mut AnyObject, title: &str) {
    let ns = make_nsstring(title);
    let _: () = msg_send![obj, setTitle: ns];
    CFRelease(ns as *const c_void);
}

/// 用一个数值/字符串填进文本框,并释放临时 NSString。
/// Set a text field's value from anything Displayable, releasing the temp NSString.
unsafe fn set_field(field: *mut AnyObject, val: impl std::fmt::Display) {
    let s = format!("{}", val);
    let ns = make_nsstring(&s);
    let _: () = msg_send![field, setStringValue: ns];
    CFRelease(ns as *const c_void);
}

/// 可编辑文本框(alloc +1,由调用方持有或交给父视图后 release)。
/// Editable text field (alloc +1; caller owns or releases after adding to a parent).
unsafe fn make_text_input(x: f64, y: f64, w: f64, h: f64, value: &str) -> *mut AnyObject {
    let field: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let field: *mut AnyObject =
        msg_send![field, initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))];
    let ns = make_nsstring(value);
    let _: () = msg_send![field, setStringValue: ns];
    CFRelease(ns as *const c_void);
    field
}

/// 下拉选择控件(alloc +1)。
/// Pop-up button (alloc +1).
unsafe fn make_popup(
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    items: &[&str],
    selected: usize,
) -> *mut AnyObject {
    let popup: *mut AnyObject = msg_send![class!(NSPopUpButton), alloc];
    let popup: *mut AnyObject = msg_send![popup, initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h)), pullsDown: false];
    for &item in items {
        let ns = make_nsstring(item);
        let _: () = msg_send![popup, addItemWithTitle: ns];
        CFRelease(ns as *const c_void);
    }
    let _: () = msg_send![popup, selectItemAtIndex: selected as isize];
    popup
}

/// 勾选框(NSButton, NSSwitchButton=3)。alloc +1,加入父视图后由调用方 release。
/// Checkbox (NSButton, NSSwitchButton=3). alloc +1; caller releases after adding to parent.
unsafe fn make_checkbox(
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    title: &str,
    checked: bool,
) -> *mut AnyObject {
    let cb: *mut AnyObject = msg_send![class!(NSButton), alloc];
    let cb: *mut AnyObject =
        msg_send![cb, initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))];
    let _: () = msg_send![cb, setButtonType: 3isize]; // NSSwitchButton
    let ns = make_nsstring(title);
    let _: () = msg_send![cb, setTitle: ns];
    CFRelease(ns as *const c_void);
    let _: () = msg_send![cb, setState: if checked { 1isize } else { 0isize }];
    cb
}

/// 设侧边栏按钮标题为带 labelColor 的 attributed title(selected=true 用粗体)。
/// Set the sidebar button's title as an attributed title with labelColor (bold when selected).
unsafe fn set_sidebar_title(btn: *mut AnyObject, title: &str, selected: bool) {
    let font: *mut AnyObject = if selected {
        msg_send![class!(NSFont), boldSystemFontOfSize: 13.0f64]
    } else {
        msg_send![class!(NSFont), messageFontOfSize: 13.0f64]
    };
    let color: *mut AnyObject = msg_send![class!(NSColor), labelColor];
    let attrs: *mut AnyObject = msg_send![class!(NSMutableDictionary), alloc];
    let attrs: *mut AnyObject = msg_send![attrs, init];
    let k_font = make_nsstring("NSFont");
    let _: () = msg_send![attrs, setObject: font, forKey: k_font];
    CFRelease(k_font as *const c_void);
    let k_color = make_nsstring("NSColor");
    let _: () = msg_send![attrs, setObject: color, forKey: k_color];
    CFRelease(k_color as *const c_void);
    let title_ns = make_nsstring(title);
    let attr_str: *mut AnyObject = msg_send![class!(NSAttributedString), alloc];
    let attr_str: *mut AnyObject = msg_send![attr_str, initWithString: title_ns, attributes: attrs];
    let _: () = msg_send![btn, setAttributedTitle: attr_str];
    CFRelease(title_ns as *const c_void);
    release_obj(attr_str);
    release_obj(attrs);
}

/// 侧边栏按钮(borderless NSButton,tag 区分页,点击触发 handleSettingsSidebar:)。
/// Sidebar button (borderless NSButton; tag selects the page; click triggers handleSettingsSidebar:).
/// 高度固定 28(与行高一致);alloc +1,加入父视图后由调用方 release。
/// Height is fixed at 28 (matches row height); alloc +1, caller releases after adding to parent.
unsafe fn make_sidebar_button(
    parent: *mut AnyObject,
    target: *mut AnyObject,
    title: &str,
    tag: isize,
    x: f64,
    y: f64,
    w: f64,
) -> *mut AnyObject {
    let h = 28.0;
    let btn: *mut AnyObject = msg_send![class!(NSButton), alloc];
    let btn: *mut AnyObject =
        msg_send![btn, initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))];
    let _: () = msg_send![btn, setButtonType: 0isize]; // NSPushInPushButton
    let _: () = msg_send![btn, setBordered: false];
    let _: () = msg_send![btn, setTag: tag];
    set_control_title(btn, title);
    set_sidebar_title(btn, title, false);
    // 自适应:贴顶、贴左、固定尺寸 / adaptive: top- and left-anchored, fixed size
    let _: () = msg_send![btn, setAutoresizingMask: 12u64];
    let _: () = msg_send![btn, setTarget: target];
    let _: () = msg_send![btn, setAction: sel!(handleSettingsSidebar:)];
    let _: () = msg_send![parent, addSubview: btn];
    release_obj(btn);
    btn
}

/// 区块标题(加粗 label),加入父视图后 release。
/// Bold section header label; released after being added to the parent.
unsafe fn add_header(parent: *mut AnyObject, text: &str, x: f64, y: f64, w: f64) {
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let label: *mut AnyObject =
        msg_send![label, initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, 20.0))];
    let ns = make_nsstring(text);
    let _: () = msg_send![label, setStringValue: ns];
    CFRelease(ns as *const c_void);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let font: *mut AnyObject = msg_send![class!(NSFont), boldSystemFontOfSize: 13.0f64];
    let _: () = msg_send![label, setFont: font];
    // 自适应:宽度随父视图拉伸、顶部锚定(MinYMargin)。autoresizing = WidthSizable | MinYMargin = 2|8 = 10。
    // Adaptive: stretch width with the parent, stay top-anchored (MinYMargin).
    let _: () = msg_send![label, setAutoresizingMask: 10u64];
    let _: () = msg_send![parent, addSubview: label];
    release_obj(label);
}

/// 加一行:右对齐 label + 控件。控件由调用方创建并传入;加入父视图后 release,返回该控件指针。
/// Add a row: right-aligned label + control. The control is created by the caller;
/// it is released after being added to the parent. Returns the control pointer.
unsafe fn add_row(
    parent: *mut AnyObject,
    label_x: f64,
    y: f64,
    label_w: f64,
    h: f64,
    label_text: &str,
    control: *mut AnyObject,
) -> *mut AnyObject {
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let label: *mut AnyObject = msg_send![label, initWithFrame: NSRect::new(NSPoint::new(label_x, y), NSSize::new(label_w, h))];
    let ns = make_nsstring(label_text);
    let _: () = msg_send![label, setStringValue: ns];
    CFRelease(ns as *const c_void);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let _: () = msg_send![label, setAlignment: 1isize]; // NSTextAlignmentRight
                                                        // 自适应:标签固定宽、顶部+左侧锚定(MinYMargin|MaxXMargin = 8|4 = 12)。
                                                        // Adaptive: label keeps fixed width, stays top- and left-anchored.
    let _: () = msg_send![label, setAutoresizingMask: 12u64];
    let _: () = msg_send![parent, addSubview: label];
    release_obj(label);
    // 自适应:控件宽度随父视图拉伸、顶部锚定(WidthSizable|MinYMargin = 2|8 = 10)。
    // Adaptive: control stretches its width with the parent, stays top-anchored.
    let _: () = msg_send![control, setAutoresizingMask: 10u64];
    let _: () = msg_send![parent, addSubview: control];
    release_obj(control);
    control
}

// ========== 设置窗口逻辑 / settings window logic ==========

pub(crate) extern "C" fn on_settings_open(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    show_settings();
}

pub(crate) extern "C" fn on_settings_ok(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    let (cfg, errs) = collect_settings_config();
    if !errs.is_empty() {
        show_alert(&t("alert.config_error_title"), &errs.join("\n"));
        return;
    }

    // 检查是否需要重启:对比新旧 mouse.enabled。按钮标题已在勾选框 toggle 时实时更新。
    // Check if restart needed: button title already updated in real time when checkbox toggled.
    let needs_restart = {
        let old_cfg = CONFIG.read().unwrap();
        old_cfg.mouse.enabled != cfg.mouse.enabled
    };

    if let Err(e) = cfg.save() {
        show_alert(&t("alert.save_failed_title"), &e);
        return;
    }
    let _ = reload_config();
    // 指针加速设置(禁用/恢复)实时生效,无需重启。
    // Pointer acceleration settings take effect immediately, no restart needed.
    crate::mouse::pointer::apply();
    set_shortcut_mode(cfg.keyboard.modifier == "command");
    apply_config_refresh();
    hide_settings();

    if needs_restart {
        // 新进程读取已保存的配置,自动启用/停用鼠标 event tap。
        // The new process reads the saved config and auto-stops/starts the mouse tap.
        let exe = std::env::current_exe().expect("failed to get executable path");
        let _ = std::process::Command::new(exe).spawn();
        std::process::exit(0);
    }
}

/// 检查 enable_mouse 勾选框状态与已保存配置是否一致,不一致时将 OK 按钮改为"确认并重启"。
/// Check if the enable_mouse checkbox state matches the saved config; if not, change OK to
/// "OK && Restart".
unsafe fn update_ok_button_title(ui: &SettingsUi) {
    let state: isize = msg_send![ui.enable_mouse, state];
    let saved = CONFIG.read().map(|c| c.mouse.enabled).unwrap_or(false);
    if (state == 1) != saved {
        set_control_title(ui.ok_button, &t("settings.btn_ok_restart"));
    } else {
        set_control_title(ui.ok_button, &t("settings.btn_ok"));
    }
}

/// enable_mouse 勾选框 toggle 回调:实时更新 OK 按钮标题。
/// Callback when the enable_mouse checkbox is toggled: update the OK button title in real time.
pub(crate) extern "C" fn handle_enable_mouse_toggle(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    unsafe {
        let ui = SETTINGS_UI.lock().unwrap();
        if let Some(u) = ui.as_ref() {
            update_ok_button_title(u);
        }
    }
}

pub(crate) extern "C" fn on_settings_cancel(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    hide_settings();
}

/// 侧边栏点击回调:读 sender 的 tag,切换到对应页。
/// Sidebar click callback: read the sender's tag and switch to that page.
pub(crate) extern "C" fn on_sidebar_select(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    let btn = sender as *mut AnyObject;
    let tag: isize = unsafe { msg_send![btn, tag] };
    select_sidebar(tag as usize);
}

/// 切换侧边栏选中页:高亮背景对齐到选中按钮、切换三个内容视图显隐、选中项粗体。
/// Switch the active settings page: align the highlight to the selected button, toggle the three
/// content views' visibility, and bold the selected item's label.
fn select_sidebar(idx: usize) {
    // tag 越界时回退到通用页 / fall back to the General page if the tag is out of range
    let idx = if idx > 2 { 0 } else { idx };
    unsafe {
        let ui = SETTINGS_UI.lock().unwrap();
        let ui = match ui.as_ref() {
            Some(u) => u,
            None => return,
        };
        let buttons = [
            ui.sidebar_general,
            ui.sidebar_experimental,
            ui.sidebar_mouse,
        ];
        let views = [ui.general_view, ui.experimental_view, ui.mouse_view];
        // 高亮背景对齐到选中按钮的 frame / align the highlight to the selected button's frame
        let frame: NSRect = msg_send![buttons[idx], frame];
        let _: () = msg_send![ui.sidebar_highlight, setFrame: frame];
        // 选中项粗体,另一项常规;都用 labelColor,颜色与 section 标题一致。
        // Bold the selected, regular for the other; both use labelColor to match section headers.
        let titles = [
            t("settings.sidebar_general"),
            t("settings.sidebar_experimental"),
            t("settings.sidebar_mouse"),
        ];
        for (i, &b) in buttons.iter().enumerate() {
            set_sidebar_title(b, &titles[i], i == idx);
        }
        // 切换三页显隐 / toggle the three pages' visibility
        for (i, &v) in views.iter().enumerate() {
            let _: () = msg_send![v, setHidden: i != idx];
        }
    }
}

/// 同步主题菜单标签并立即应用配置(主题/浮窗)。
/// Sync menu labels and apply the config immediately (theme / overlay).
fn apply_config_refresh() {
    refresh_menu_titles();
    invalidate_settings_window();
    apply_theme();
    refresh_highlight();
    update_status_label();
}

fn show_settings() {
    unsafe {
        {
            let ui = SETTINGS_UI.lock().unwrap();
            if ui.is_none() {
                drop(ui);
                create_settings_window();
            }
        }
        load_settings_values();
        // 每次打开都复位到通用页(窗口复用、隐藏不销毁,上次可能停在实验性页)。
        // Reset to the General page on every open (the window is reused / hidden, not destroyed,
        // so the previous selection may have been Experimental).
        select_sidebar(0);
        let ui = SETTINGS_UI.lock().unwrap();
        if let Some(u) = ui.as_ref() {
            // 切到 .regular:让设置窗口能正常激活抬升(从别的 App 顶部弹出来),关闭时切回。
            // Switch to .regular so the settings window can activate and raise itself above
            // the active app; reverted on close.
            crate::set_settings_activation_policy(true);
            let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![nsapp, activateIgnoringOtherApps: true];
            let _: () = msg_send![u.window, makeKeyAndOrderFront: std::ptr::null::<AnyObject>()];
            // 清掉默认 first responder,避免打开时光标落在 Glass color 文本框。
            // Clear the default first responder so the cursor doesn't land in the Glass color field on open.
            let _: bool = msg_send![u.window, makeFirstResponder: std::ptr::null::<AnyObject>()];
            // 按当前权限刷新警告条显隐(有权限就隐藏)/ refresh banner visibility by current permission
            let _: () =
                msg_send![u.accessibility_warning_view, setHidden: has_accessibility_permission()];
        }
    }
}

fn hide_settings() {
    unsafe {
        let ui = SETTINGS_UI.lock().unwrap();
        if let Some(u) = ui.as_ref() {
            let _: () = msg_send![u.window, orderOut: std::ptr::null::<AnyObject>()];
        }
    }
    // 切回 .accessory:设置窗口关闭,回到纯菜单栏(无 Dock 图标)。
    // Switch back to .accessory: the settings window is closed, return to pure menu-bar (no Dock icon).
    crate::set_settings_activation_policy(false);
    // 关闭 ≠ 「打开但隐藏」:清掉 stash 标志,防止 collect 为已关闭的设置窗口合成卡片。
    // Closing is not "open but hidden": clear the stash flag so collect never synthesizes
    // a card for a closed settings window.
    SETTINGS_HIDDEN_FOR_SUMMON.store(false, Ordering::SeqCst);
}

/// 浮窗显示时调用:若设置窗口正打开可见,临时 orderOut 藏起来并置标志。
/// 设置窗口已被 collect 收为卡片,这里只藏其实体,避免它贴在浮窗后面露出来。
/// 恢复由调用方决定:选中自己/取消 -> restore_settings_after_summon(),
/// 切到别的 App -> push_settings_to_back()(orderBack 沉底,在屏但不抬到最前)。
///
/// Called when the overlay is shown: if the settings window is open and visible,
/// orderOut it temporarily and set a flag. It was already collected as a card, so only
/// its body is hidden -- preventing it from peeking out behind the overlay. Restore is
/// decided by callers: selecting our own window or cancelling ->
/// restore_settings_after_summon(); switching to another app -> push_settings_to_back()
/// (orderBack to the very back, on-screen but not raised to the front).
pub(crate) fn stash_settings_for_summon() {
    unsafe {
        let ui = SETTINGS_UI.lock().unwrap();
        let Some(u) = ui.as_ref() else {
            return;
        };
        let visible: bool = msg_send![u.window, isVisible];
        if !visible {
            return;
        }
        let _: () = msg_send![u.window, orderOut: std::ptr::null::<AnyObject>()];
        SETTINGS_HIDDEN_FOR_SUMMON.store(true, Ordering::SeqCst);
    }
}

/// 选中自己的设置窗口或取消召唤时调用:若 stash 过设置窗口,orderFront 恢复它。
/// 调用方需在 activate_and_raise 之前调用——orderOut 的窗口无法被 AXRaise 抬升,
/// 必须先恢复可见,随后的 raise 才能把它抬到最前。
///
/// Called when selecting our own settings window or cancelling the summon: if the settings
/// window was stashed, orderFront it. Callers must invoke this before activate_and_raise --
/// an orderOut'd window can't be raised via AXRaise; it must be made visible first so the
/// subsequent raise can bring it to the front.
pub(crate) fn restore_settings_after_summon() {
    if !SETTINGS_HIDDEN_FOR_SUMMON.swap(false, Ordering::SeqCst) {
        return;
    }
    unsafe {
        let ui = SETTINGS_UI.lock().unwrap();
        if let Some(u) = ui.as_ref() {
            let _: () = msg_send![u.window, orderFront: std::ptr::null::<AnyObject>()];
        }
    }
}

/// 切换到「别的 App」后调用:把设置窗口沉到所有窗口最底层(orderBack)。
/// 保持可见(调度中心/我们的切换器仍能显示它),但不会被抬到目标窗口前面,
/// 也不会像 orderFront 那样贴在目标窗口正下方——对齐「切走时不参与」的语义。
/// 仅当 summon 期间 stash 过(设置窗口当时可见)才 orderBack;已关闭时 no-op,
/// 避免把已关闭的设置窗口重新显示出来。
///
/// Called after switching to a DIFFERENT app: push the settings window to the very back
/// of the z-order (orderBack). It stays visible (so Mission Control and our switcher still
/// show it) but is neither raised in front of the target window nor stacked right below it
/// the way orderFront would -- matching the "stays out of the way when switching away"
/// semantics. Only acts when the window was stashed during this summon (i.e. it was
/// visible); a closed settings window is never resurrected.
pub(crate) fn push_settings_to_back() {
    if !SETTINGS_HIDDEN_FOR_SUMMON.swap(false, Ordering::SeqCst) {
        return;
    }
    unsafe {
        let ui = SETTINGS_UI.lock().unwrap();
        if let Some(u) = ui.as_ref() {
            let _: () = msg_send![u.window, orderBack: std::ptr::null::<AnyObject>()];
        }
    }
}

/// 弹一个简单的告警框(app 模态),用于显示校验/保存错误。
/// Show a simple app-modal alert for validation / save errors.
fn show_alert(title: &str, msg: &str) {
    unsafe {
        let alert: *mut AnyObject = msg_send![class!(NSAlert), new];
        let ns1 = make_nsstring(title);
        let _: () = msg_send![alert, setMessageText: ns1];
        CFRelease(ns1 as *const c_void);
        let ns2 = make_nsstring(msg);
        let _: () = msg_send![alert, setInformativeText: ns2];
        CFRelease(ns2 as *const c_void);
        let ns3 = make_nsstring(&t("alert.btn_ok"));
        let _: *mut AnyObject = msg_send![alert, addButtonWithTitle: ns3];
        CFRelease(ns3 as *const c_void);
        let _resp: isize = msg_send![alert, runModal];
        release_obj(alert);
    }
}

/// 确认弹窗(两个按钮)。返回 true = 用户点了确认按钮。
/// Confirm dialog (two buttons). Returns true if the user clicked the confirm button.
pub(crate) fn confirm_alert(
    title: &str,
    msg: &str,
    confirm_label: &str,
    cancel_label: &str,
) -> bool {
    unsafe {
        let alert: *mut AnyObject = msg_send![class!(NSAlert), new];
        let ns1 = make_nsstring(title);
        let _: () = msg_send![alert, setMessageText: ns1];
        CFRelease(ns1 as *const c_void);
        let ns2 = make_nsstring(msg);
        let _: () = msg_send![alert, setInformativeText: ns2];
        CFRelease(ns2 as *const c_void);
        // 第一个按钮为默认(右,回车);确认在前,取消在后。
        // First button is the default (rightmost, Return); confirm first, cancel second.
        let n_confirm = make_nsstring(confirm_label);
        let _: *mut AnyObject = msg_send![alert, addButtonWithTitle: n_confirm];
        CFRelease(n_confirm as *const c_void);
        let n_cancel = make_nsstring(cancel_label);
        let _: *mut AnyObject = msg_send![alert, addButtonWithTitle: n_cancel];
        CFRelease(n_cancel as *const c_void);
        let resp: isize = msg_send![alert, runModal];
        release_obj(alert);
        resp == 1000 // NSAlertFirstButtonReturn = 确认 / confirm
    }
}

/// 恢复默认设置:确认 -> 把代码默认值写回 config.toml -> 重载 + 刷新 UI + 重填设置界面。
/// Restore defaults: confirm -> write code defaults to config.toml -> reload + refresh UI + repopulate settings.
pub(crate) extern "C" fn handle_restore_defaults(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    if !confirm_alert(
        &t("alert.restore_title"),
        &t("alert.restore_msg"),
        &t("alert.btn_restore"),
        &t("alert.btn_cancel"),
    ) {
        return;
    }
    // 把代码里的默认值写回 config.toml,但保留 launch_at_login--它是系统级登录项开关,
    // 不属于外观/布局/快捷键这类设置,不该被恢复默认重置(否则会注销用户已勾选的登录项)。
    // Write code defaults to config.toml, but preserve launch_at_login -- it's a system-level
    // login-item toggle, not an appearance/layout/shortcut setting, so Restore Defaults must not
    // reset it (otherwise it unregisters a login item the user explicitly enabled).
    let preserved_launch_at_login = CONFIG.read().unwrap().startup.launch_at_login;
    let old_enabled = CONFIG.read().unwrap().mouse.enabled;
    let mut defaults = Config::default();
    defaults.startup.launch_at_login = preserved_launch_at_login;
    if let Err(e) = defaults.save() {
        show_alert(&t("alert.save_failed_title"), &e);
        return;
    }
    // 重读(现在是默认值)+ 应用 locale/logger。
    // Reload (now defaults) + apply locale/logger.
    let _ = reload_config();
    // 恢复默认后指针加速设置随之恢复/应用。
    // Pointer acceleration settings follow the restored defaults.
    crate::mouse::pointer::apply();
    // 同步快捷键模式(默认 command)+ 开机自启(保留用户原值,不重置)。
    // Sync shortcut mode (default command) + launch-at-login (preserved user value, not reset).
    set_shortcut_mode(CONFIG.read().unwrap().keyboard.modifier == "command");
    crate::autostart::sync(CONFIG.read().unwrap().startup.launch_at_login);
    // 刷新 UI,但保留正在显示的设置窗口(不调 invalidate_settings_window,否则会关闭并释放它)。
    // Refresh UI but keep the open settings window (skip invalidate_settings_window, which would close+release it).
    refresh_menu_titles();
    apply_theme();
    refresh_highlight();
    update_status_label();
    // 设置窗口正打开:重填控件为默认值 + 复位到通用页。
    // Settings window is open: repopulate controls with defaults + reset to General page.
    load_settings_values();
    select_sidebar(0);
    // 恢复默认后可能改变了 mouse.enabled,刷新 OK 按钮标题。
    // Restore defaults may have changed mouse.enabled; refresh the OK button title.
    unsafe {
        if let Some(ui) = SETTINGS_UI.lock().unwrap().as_ref() {
            update_ok_button_title(ui);
            // 若 mouse.enabled 发生了实际变化,update_ok_button_title 无法感知(勾选框与 CONFIG 一致),
            // 手动修正按钮为"确认并重启"。
            let new_enabled = CONFIG.read().unwrap().mouse.enabled;
            if old_enabled != new_enabled {
                set_control_title(ui.ok_button, &t("settings.btn_ok_restart"));
            }
        }
    }
    log_info!("Config restored to defaults.");
}

/// 用当前 CONFIG 填充设置控件(每次打开都刷新,反映外部编辑 + Reload)。
/// Populate settings controls from current CONFIG (refreshed on each open).
fn load_settings_values() {
    let cfg = CONFIG.read().unwrap().clone();
    let is_cmd = SHORTCUT_IS_CMD.load(Ordering::SeqCst);
    unsafe {
        let ui = SETTINGS_UI.lock().unwrap();
        let ui = match ui.as_ref() {
            Some(u) => u,
            None => return,
        };
        let theme_idx: isize = match cfg.appearance.theme.as_str() {
            "dark" => 0,
            "light" => 1,
            _ => 2,
        };
        let _: () = msg_send![ui.theme, selectItemAtIndex: theme_idx];
        let gs_idx: isize = if cfg.appearance.glass_style == "clear" {
            1
        } else {
            0
        };
        let _: () = msg_send![ui.glass_style, selectItemAtIndex: gs_idx];
        set_field(ui.glass_tint, &cfg.appearance.glass_tint);
        set_field(ui.corner_radius, cfg.appearance.corner_radius);
        set_field(ui.cards_per_row, cfg.layout.cards_per_row);
        set_field(ui.card_width, cfg.layout.card_width);
        set_field(ui.card_height, cfg.layout.card_height);
        set_field(ui.card_gap, cfg.layout.card_gap);
        set_field(ui.icon_size, cfg.layout.icon_size);
        let mod_idx: isize = if is_cmd { 1 } else { 0 };
        let _: () = msg_send![ui.modifier, selectItemAtIndex: mod_idx];
        // locale:按 CONFIG.i18n.locale 选中对应项,未匹配回退第 0 项(auto)。
        // locale: select the item matching CONFIG.i18n.locale; fall back to index 0 (auto).
        let loc_idx: isize = LOCALE_VALUES
            .iter()
            .position(|v| *v == cfg.i18n.locale.as_str())
            .map(|i| i as isize)
            .unwrap_or(0);
        let _: () = msg_send![ui.locale, selectItemAtIndex: loc_idx];
        // show_minimized:按 CONFIG.windows.show_minimized 设复选框状态。
        // show_minimized: set the checkbox state from CONFIG.windows.show_minimized.
        // show_minimized:下拉框 index 0 = 不显示(false), 1 = 显示(true)。
        // show_minimized: popup index 0 = hide (false), 1 = show (true).
        let sm_idx = if cfg.windows.show_minimized { 1 } else { 0 };
        let _: () = msg_send![ui.show_minimized, selectItemAtIndex: sm_idx as isize];
        // log_level:下拉框 index 0..2 对应 info,warn,error;默认 index 0(info)。
        // log_level: popup index 0..2 = info, warn, error; default index 0 (info).
        let ll_idx = match cfg.logging.level.as_str() {
            "warn" => 1,
            "error" => 2,
            _ => 0, // "info" (default)
        };
        let _: () = msg_send![ui.log_level, selectItemAtIndex: ll_idx as isize];
        // launch_at_login:按 CONFIG.startup.launch_at_login 设勾选框状态。
        // launch_at_login: set the checkbox state from CONFIG.startup.launch_at_login.
        let _: () = msg_send![ui.launch_at_login, setState: if cfg.startup.launch_at_login { 1isize } else { 0isize }];
        // reverse_scroll:按 CONFIG.mouse.reverse_scroll 设勾选框状态。
        // reverse_scroll: set the checkbox state from CONFIG.mouse.reverse_scroll.
        let _: () = msg_send![ui.reverse_scroll, setState: if cfg.mouse.reverse_scroll { 1isize } else { 0isize }];
        // enable_mouse:按 CONFIG.mouse.enabled 设勾选框状态。
        // enable_mouse: set the checkbox state from CONFIG.mouse.enabled.
        let _: () =
            msg_send![ui.enable_mouse, setState: if cfg.mouse.enabled { 1isize } else { 0isize }];
        // disable_pointer_accel:按 CONFIG.mouse.pointer.disable_acceleration 设勾选框状态。
        // disable_pointer_accel: set the checkbox state from CONFIG.mouse.pointer.disable_acceleration.
        let _: () = msg_send![ui.disable_pointer_accel, setState: if cfg.mouse.pointer.disable_acceleration { 1isize } else { 0isize }];
        // scroll_mode:按 CONFIG.mouse.scroll_mode 选中对应项。
        // scroll_mode: select item matching CONFIG.mouse.scroll_mode.
        let sm_idx: isize = SCROLL_MODE_VALUES
            .iter()
            .position(|v| *v == cfg.mouse.scroll_mode.as_str())
            .map(|i| i as isize)
            .unwrap_or(0);
        let _: () = msg_send![ui.scroll_mode, selectItemAtIndex: sm_idx];
        // line_count:按 CONFIG.mouse.line_count 填文本。
        // line_count: fill text from CONFIG.mouse.line_count.
        set_field(ui.line_count, cfg.mouse.line_count);
        // smooth_preset:按 CONFIG.mouse.smooth_preset 选中对应项。
        // smooth_preset: select item matching CONFIG.mouse.smooth_preset.
        let sp_idx: isize = SMOOTH_PRESET_LABELS
            .iter()
            .position(|v| *v == cfg.mouse.smooth_preset.as_str())
            .map(|i| i as isize)
            .unwrap_or(0);
        let _: () = msg_send![ui.smooth_preset, selectItemAtIndex: sp_idx];

        // 每次打开设置时,同步 OK 按钮标题(可能因配置变化而需要重启)。
        // Sync OK button title on each settings open (config changes may require restart).
        update_ok_button_title(ui);
    }
}

/// 从控件收集成 Config(克隆当前 CONFIG,只覆盖表单内字段),并收集错误。
/// Collect a Config from the controls (clone current CONFIG, overwrite only in-form
/// fields) and gather parse + validation errors.
fn collect_settings_config() -> (Config, Vec<String>) {
    let mut cfg = CONFIG.read().unwrap().clone();
    let mut errs: Vec<String> = Vec::new();
    unsafe {
        let ui = SETTINGS_UI.lock().unwrap();
        let ui = match ui.as_ref() {
            Some(u) => u,
            None => return (cfg, vec!["settings UI not ready".into()]),
        };
        let theme_idx: isize = msg_send![ui.theme, indexOfSelectedItem];
        cfg.appearance.theme = match theme_idx {
            0 => "dark".into(),
            1 => "light".into(),
            _ => "auto".into(),
        };
        let gs_idx: isize = msg_send![ui.glass_style, indexOfSelectedItem];
        cfg.appearance.glass_style = if gs_idx == 1 {
            "clear".into()
        } else {
            "regular".into()
        };
        cfg.appearance.glass_tint = nsstring_to_rust(msg_send![ui.glass_tint, stringValue]);
        match parse_f64(&nsstring_to_rust(msg_send![ui.corner_radius, stringValue])) {
            Ok(v) => cfg.appearance.corner_radius = v,
            Err(_) => errs.push(tf(
                "errors.not_a_number",
                &[("field", "appearance.corner_radius")],
            )),
        }
        match parse_usize(&nsstring_to_rust(msg_send![ui.cards_per_row, stringValue])) {
            Ok(v) => cfg.layout.cards_per_row = v,
            Err(_) => errs.push(tf(
                "errors.not_an_integer",
                &[("field", "layout.cards_per_row")],
            )),
        }
        match parse_f64(&nsstring_to_rust(msg_send![ui.card_width, stringValue])) {
            Ok(v) => cfg.layout.card_width = v,
            Err(_) => errs.push(tf("errors.not_a_number", &[("field", "layout.card_width")])),
        }
        match parse_f64(&nsstring_to_rust(msg_send![ui.card_height, stringValue])) {
            Ok(v) => cfg.layout.card_height = v,
            Err(_) => errs.push(tf(
                "errors.not_a_number",
                &[("field", "layout.card_height")],
            )),
        }
        match parse_f64(&nsstring_to_rust(msg_send![ui.card_gap, stringValue])) {
            Ok(v) => cfg.layout.card_gap = v,
            Err(_) => errs.push(tf("errors.not_a_number", &[("field", "layout.card_gap")])),
        }
        match parse_f64(&nsstring_to_rust(msg_send![ui.icon_size, stringValue])) {
            Ok(v) => cfg.layout.icon_size = v,
            Err(_) => errs.push(tf("errors.not_a_number", &[("field", "layout.icon_size")])),
        }
        let mod_idx: isize = msg_send![ui.modifier, indexOfSelectedItem];
        cfg.keyboard.modifier = if mod_idx == 1 {
            "command".into()
        } else {
            "option".into()
        };
        // locale:下拉项顺序与 LOCALE_VALUES 对应;越界回退 auto。
        // locale: popup order matches LOCALE_VALUES; out-of-range falls back to auto.
        let loc_idx: isize = msg_send![ui.locale, indexOfSelectedItem];
        cfg.i18n.locale = LOCALE_VALUES
            .get(loc_idx as usize)
            .map(|s| s.to_string())
            .unwrap_or_else(|| "auto".into());
        // show_minimized:复选框 state(1=on / 0=off)。
        // show_minimized: checkbox state (1=on / 0=off).
        // show_minimized:下拉框 index 0 = 不显示(false), 1 = 显示(true)。
        // show_minimized: popup index 0 = hide (false), 1 = show (true).
        let sm_idx: isize = msg_send![ui.show_minimized, indexOfSelectedItem];
        cfg.windows.show_minimized = sm_idx == 1;
        // log_level:下拉框 index 0..2 对应 info,warn,error。
        // log_level: popup index 0..2 = info, warn, error.
        let ll_idx: isize = msg_send![ui.log_level, indexOfSelectedItem];
        cfg.logging.level = match ll_idx {
            1 => "warn",
            2 => "error",
            _ => "info", // index 0 or out-of-range
        }
        .into();
        // launch_at_login:勾选框 state(1=on / 0=off)。
        // launch_at_login: checkbox state (1=on / 0=off).
        let la_state: isize = msg_send![ui.launch_at_login, state];
        cfg.startup.launch_at_login = la_state == 1;
        // reverse_scroll:勾选框 state(1=on / 0=off)。
        // reverse_scroll: checkbox state (1=on / 0=off).
        let ns_state: isize = msg_send![ui.reverse_scroll, state];
        cfg.mouse.reverse_scroll = ns_state == 1;
        // enable_mouse:勾选框 state(1=on / 0=off)。
        // enable_mouse: checkbox state (1=on / 0=off).
        let em_state: isize = msg_send![ui.enable_mouse, state];
        cfg.mouse.enabled = em_state == 1;
        // disable_pointer_accel:勾选框 state(1=on / 0=off)。
        // disable_pointer_accel: checkbox state (1=on / 0=off).
        let dpa_state: isize = msg_send![ui.disable_pointer_accel, state];
        cfg.mouse.pointer.disable_acceleration = dpa_state == 1;
        // scroll_mode:下拉框 index 对应 SCROLL_MODE_VALUES。
        // scroll_mode: popup index matches SCROLL_MODE_VALUES.
        let sm_idx: isize = msg_send![ui.scroll_mode, indexOfSelectedItem];
        cfg.mouse.scroll_mode = SCROLL_MODE_VALUES
            .get(sm_idx as usize)
            .map(|s| s.to_string())
            .unwrap_or_else(|| "default".into());
        // line_count:文本输入。
        // line_count: text input.
        match parse_usize(&nsstring_to_rust(msg_send![ui.line_count, stringValue])) {
            Ok(v) => cfg.mouse.line_count = v.clamp(1, 10) as u32,
            Err(_) => errs.push(tf("errors.not_a_number", &[("field", "mouse.line_count")])),
        }
        // smooth_preset:下拉框 index 对应 SMOOTH_PRESET_LABELS。
        // smooth_preset: popup index matches SMOOTH_PRESET_LABELS.
        let sp_idx: isize = msg_send![ui.smooth_preset, indexOfSelectedItem];
        cfg.mouse.smooth_preset = SMOOTH_PRESET_LABELS
            .get(sp_idx as usize)
            .map(|s| s.to_string())
            .unwrap_or_else(|| "easeInOut".into());
    }
    for e in cfg.validate() {
        errs.push(e);
    }
    (cfg, errs)
}

/// 构建设置窗口(只建一次,存入 SETTINGS_UI,之后复用、隐藏而非销毁)。
/// Build the settings window once, store it in SETTINGS_UI, then reuse (hide, not destroy).
// 设置窗口自定义子类 OhMyTabSettingsWindow:重写 performClose:,让红色关闭按钮走 hide_settings
// (切回 .accessory),而不是默认的 orderOut(那样不会触发激活策略切换,导致 Dock 图标残留)。
// create_settings_window 在 invalidate 后可能被再次调用,故用 OnceLock 守卫只注册一次。
// Custom settings window subclass overriding performClose: so the red close button routes through
// hide_settings (which flips activation policy back to .accessory), instead of the default orderOut
// (which wouldn't trigger the policy switch, leaving the Dock icon around). create_settings_window
// can be called again after invalidate_settings_window, so registration is guarded with OnceLock.
extern "C" fn settings_window_perform_close(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    hide_settings();
}

struct SettingsWindowClass(*mut AnyObject);
unsafe impl Send for SettingsWindowClass {}
unsafe impl Sync for SettingsWindowClass {}

static SETTINGS_WINDOW_CLS: OnceLock<SettingsWindowClass> = OnceLock::new();

fn settings_window_class() -> *mut AnyObject {
    SETTINGS_WINDOW_CLS
        .get_or_init(|| unsafe {
            let name = CString::new("OhMyTabSettingsWindow").unwrap();
            let superclass = class!(NSWindow) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types = CString::new("v@:@").unwrap(); // -performClose:(id)sender -> void
            class_addMethod(
                cls,
                sel!(performClose:),
                settings_window_perform_close as *mut c_void,
                types.as_ptr(),
            );
            objc_registerClassPair(cls);
            SettingsWindowClass(cls)
        })
        .0
}

fn create_settings_window() {
    unsafe {
        let view_w = 580.0;
        let sidebar_w = 150.0;
        let style: u64 = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 3);
        // titled + closable + miniaturizable + resizable(三个红绿灯齐全)。resizable 是绿色 zoom
        // 按钮出现的必要条件;布局是绝对定位不随缩放,故下方用 min=max 固定窗口尺寸。
        // titled + closable + miniaturizable + resizable (all three traffic lights). resizable is
        // required for the green zoom button to appear; the layout is absolute-positioned and
        // doesn't adapt, so the window size is fixed below via min=max.
        // 窗口加左侧侧边栏后:宽 420 -> 580(侧边栏 150 + 1pt 分隔 + 内容 429)。
        // 内容拆成「通用 / 实验性」两页后,通用页(6 段 9 行)最高,高 748 -> 600 足够;
        // 通用页顶部预留 Accessibility 权限警告条,窗口再加 60 -> 660。
        // Window widened for the left sidebar: 420 -> 580 (sidebar 150 + 1pt divider + 429 content).
        // Content is now paged (General / Experimental); the General page (6 sections, 9 rows) is the
        // tallest, so height 748 -> 600 is enough; +60 -> 660 to reserve room for the
        // Accessibility permission warning banner at the top of the General page.
        let frame = NSRect::new(NSPoint::new(220.0, 180.0), NSSize::new(view_w, 660.0));
        let window: *mut AnyObject = msg_send![settings_window_class(), alloc];
        let window: *mut AnyObject = msg_send![window, initWithContentRect: frame, styleMask: style, backing: 2u64, defer: false];
        let ns_title = make_nsstring(&t("settings.window_title"));
        let _: () = msg_send![window, setTitle: ns_title];
        CFRelease(ns_title as *const c_void);
        let _: () = msg_send![window, setReleasedWhenClosed: false];
        // 自适应:窗口可缩放,行已设 autoresizing masks,内容随窗口调整。
        // 设最小尺寸避免缩太小导致控件重叠。
        // Adaptive: the window is resizable; rows already have autoresizing masks set,
        // so content adjusts with the window. Set a minimum size to avoid overlap.
        let _: () = msg_send![window, setMinSize: NSSize::new(450.0, 400.0)];
        let content: *mut AnyObject = msg_send![window, contentView];
        // 用 contentView 的实际高度做布局(标题栏会占掉一部分,不能直接用窗口高度)。
        // Layout against the contentView's real height (the title bar eats part of it).
        let content_frame: NSRect = msg_send![content, frame];
        let content_h = content_frame.size.height;

        let mut ui = SettingsUi {
            window,
            sidebar_general: std::ptr::null_mut(),
            sidebar_experimental: std::ptr::null_mut(),
            sidebar_mouse: std::ptr::null_mut(),
            sidebar_highlight: std::ptr::null_mut(),
            general_view: std::ptr::null_mut(),
            experimental_view: std::ptr::null_mut(),
            mouse_view: std::ptr::null_mut(),
            theme: std::ptr::null_mut(),
            glass_style: std::ptr::null_mut(),
            glass_tint: std::ptr::null_mut(),
            corner_radius: std::ptr::null_mut(),
            cards_per_row: std::ptr::null_mut(),
            card_width: std::ptr::null_mut(),
            card_height: std::ptr::null_mut(),
            card_gap: std::ptr::null_mut(),
            icon_size: std::ptr::null_mut(),
            modifier: std::ptr::null_mut(),
            locale: std::ptr::null_mut(),
            show_minimized: std::ptr::null_mut(),
            log_level: std::ptr::null_mut(),
            launch_at_login: std::ptr::null_mut(),
            reverse_scroll: std::ptr::null_mut(),
            enable_mouse: std::ptr::null_mut(),
            scroll_mode: std::ptr::null_mut(),
            line_count: std::ptr::null_mut(),
            smooth_preset: std::ptr::null_mut(),
            disable_pointer_accel: std::ptr::null_mut(),
            ok_button: std::ptr::null_mut(),
            accessibility_warning_view: std::ptr::null_mut(),
        };

        // 内容区 x(侧边栏右、1pt 分隔线右)、宽 / content area x (right of sidebar + divider) and width
        let content_x = sidebar_w + 1.0;
        let content_w = view_w - content_x; // 580 - 151 = 429
        let label_x = 12.0;
        let label_w = 150.0;
        let ctrl_x = 170.0;
        let ctrl_w = content_w - ctrl_x - 12.0;
        let row_h = 22.0;
        let row_pitch = 28.0;

        let target = match *MENU_TARGET.lock().unwrap() {
            Some(t) => t.0,
            None => return,
        };

        // --- 侧边栏 sidebar ---
        let sidebar_view: *mut AnyObject = msg_send![class!(NSView), alloc];
        let sidebar_view: *mut AnyObject = msg_send![sidebar_view, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(sidebar_w, content_h))];
        // 自适应:左侧锚定、高度随窗口拉伸(HeightSizable|MaxXMargin = 16|4 = 20)。
        // Adaptive: left-anchored, height stretches with the window.
        let _: () = msg_send![sidebar_view, setAutoresizingMask: 20u64];
        let _: () = msg_send![content, addSubview: sidebar_view];
        release_obj(sidebar_view);

        // 侧边栏与内容间的 1pt 分隔线(layer 填半透明黑,深浅色模式下都可见,旧 API 安全)。
        // 1pt divider between sidebar and content (layer-filled semi-transparent black; visible in both
        // light/dark; uses an old API so it's safe across macOS versions).
        let divider: *mut AnyObject = msg_send![class!(NSView), alloc];
        let divider: *mut AnyObject = msg_send![divider, initWithFrame: NSRect::new(NSPoint::new(sidebar_w, 0.0), NSSize::new(1.0, content_h))];
        let _: () = msg_send![divider, setAutoresizingMask: 20u64]; // 同 sidebar:左侧锚定、高度拉伸
        let _: () = msg_send![divider, setWantsLayer: true];
        let div_layer: *mut AnyObject = msg_send![divider, layer];
        let sep_color: *mut AnyObject =
            msg_send![class!(NSColor), colorWithCalibratedWhite: 0.0f64, alpha: 0.15f64];
        layer_set_background(div_layer, ns_color_to_cg(sep_color));
        let _: () = msg_send![content, addSubview: divider];
        release_obj(divider);

        // 侧边栏选中行的高亮背景(layer-backed NSView,theme 感知色),先于按钮加入以便按钮文字叠在上层。
        // Highlight background for the selected sidebar row (layer-backed NSView, theme-aware color);
        // added before the buttons so button titles draw on top of it.
        let btn_w = sidebar_w - 24.0;
        let btn_h = 28.0;
        let btn_y0 = content_h - 12.0 - 30.0;
        let highlight: *mut AnyObject = msg_send![class!(NSView), alloc];
        let highlight: *mut AnyObject = msg_send![highlight, initWithFrame: NSRect::new(NSPoint::new(12.0, btn_y0), NSSize::new(btn_w, btn_h))];
        let _: () = msg_send![highlight, setAutoresizingMask: 12u64]; // 贴顶、贴左 / top- and left-anchored
        let _: () = msg_send![highlight, setWantsLayer: true];
        let hl_layer: *mut AnyObject = msg_send![highlight, layer];
        let _: () = msg_send![hl_layer, setCornerRadius: 6.0f64];
        let sel_color: *mut AnyObject = msg_send![class!(NSColor), selectedControlColor];
        layer_set_background(hl_layer, ns_color_to_cg(sel_color));
        let _: () = msg_send![sidebar_view, addSubview: highlight];
        release_obj(highlight);
        ui.sidebar_highlight = highlight;

        // 两个侧边栏按钮(borderless,tag 0/1,点击触发 handleSettingsSidebar:)。
        // Two sidebar buttons (borderless, tag 0/1; click triggers handleSettingsSidebar:).
        ui.sidebar_general = make_sidebar_button(
            sidebar_view,
            target,
            &t("settings.sidebar_general"),
            0,
            12.0,
            btn_y0,
            btn_w,
        );
        ui.sidebar_experimental = make_sidebar_button(
            sidebar_view,
            target,
            &t("settings.sidebar_experimental"),
            1,
            12.0,
            btn_y0 - 34.0,
            btn_w,
        );
        ui.sidebar_mouse = make_sidebar_button(
            sidebar_view,
            target,
            &t("settings.sidebar_mouse"),
            2,
            12.0,
            btn_y0 - 68.0,
            btn_w,
        );

        // --- 通用页容器 general page container ---
        let general_view: *mut AnyObject = msg_send![class!(NSView), alloc];
        let general_view: *mut AnyObject = msg_send![general_view, initWithFrame: NSRect::new(NSPoint::new(content_x, 0.0), NSSize::new(content_w, content_h))];
        // 自适应:宽高随窗口拉伸(WidthSizable|HeightSizable = 2|16 = 18)。
        let _: () = msg_send![general_view, setAutoresizingMask: 18u64];
        let _: () = msg_send![content, addSubview: general_view];
        release_obj(general_view);
        ui.general_view = general_view;

        // --- 实验性页容器 experimental page container(初始隐藏 / initially hidden)---
        let experimental_view: *mut AnyObject = msg_send![class!(NSView), alloc];
        let experimental_view: *mut AnyObject = msg_send![experimental_view, initWithFrame: NSRect::new(NSPoint::new(content_x, 0.0), NSSize::new(content_w, content_h))];
        let _: () = msg_send![experimental_view, setHidden: true];
        let _: () = msg_send![experimental_view, setAutoresizingMask: 18u64]; // 同 general_view:宽高拉伸
        let _: () = msg_send![content, addSubview: experimental_view];
        release_obj(experimental_view);
        ui.experimental_view = experimental_view;

        // --- 鼠标页容器 mouse page container(初始隐藏 / initially hidden)---
        let mouse_view: *mut AnyObject = msg_send![class!(NSView), alloc];
        let mouse_view: *mut AnyObject = msg_send![mouse_view, initWithFrame: NSRect::new(NSPoint::new(content_x, 0.0), NSSize::new(content_w, content_h))];
        let _: () = msg_send![mouse_view, setHidden: true];
        let _: () = msg_send![mouse_view, setAutoresizingMask: 18u64]; // 同 general_view:宽高拉伸
        let _: () = msg_send![content, addSubview: mouse_view];
        release_obj(mouse_view);
        ui.mouse_view = mouse_view;

        // ===== 通用页内容 general page content =====
        let mut y = content_h - 12.0; // 顶部光标:下一个元素的底边 y / top cursor (bottom y of next element)

        // --- Accessibility 权限警告条(通用页顶部;仅缺权限时显示,show_settings 里按 setHidden 切换) ---
        // --- Accessibility permission warning banner (top of General page; shown only when permission
        //  is missing, toggled via setHidden in show_settings) ---
        let banner_h = 48.0;
        let banner: *mut AnyObject = msg_send![class!(NSView), alloc];
        let banner: *mut AnyObject = msg_send![
            banner,
            initWithFrame: NSRect::new(
                NSPoint::new(0.0, y - banner_h),
                NSSize::new(content_w, banner_h)
            )
        ];
        // 自适应:宽度拉伸、顶部锚定(WidthSizable|MinYMargin = 10)。
        let _: () = msg_send![banner, setAutoresizingMask: 10u64];
        let _: () = msg_send![general_view, addSubview: banner];
        release_obj(banner);
        ui.accessibility_warning_view = banner;

        // 警告文字:多行换行,系统红色 / warning text: word-wrapped, system red
        let warning_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let warning_label: *mut AnyObject = msg_send![
            warning_label,
            initWithFrame: NSRect::new(
                NSPoint::new(12.0, 6.0),
                NSSize::new(content_w - 160.0, banner_h - 12.0)
            )
        ];
        let wl = make_nsstring(&t("settings.accessibility_warning"));
        let _: () = msg_send![warning_label, setStringValue: wl];
        CFRelease(wl as *const c_void);
        let _: () = msg_send![warning_label, setEditable: false];
        let _: () = msg_send![warning_label, setBezeled: false];
        let _: () = msg_send![warning_label, setDrawsBackground: false];
        let _: () = msg_send![warning_label, setUsesSingleLineMode: false];
        let _: () = msg_send![warning_label, setLineBreakMode: 0isize]; // NSLineBreakByWordWrapping
        let red: *mut AnyObject = msg_send![class!(NSColor), systemRedColor];
        let _: () = msg_send![warning_label, setTextColor: red];
        // 自适应:宽度随 banner 拉伸、左锚定(WidthSizable = 2)。
        let _: () = msg_send![warning_label, setAutoresizingMask: 2u64];
        let _: () = msg_send![banner, addSubview: warning_label];
        release_obj(warning_label);

        // 「打开隐私与安全性」按钮 / "Open Privacy & Security" button
        let open_btn: *mut AnyObject = msg_send![class!(NSButton), alloc];
        let open_btn: *mut AnyObject = msg_send![
            open_btn,
            initWithFrame: NSRect::new(
                NSPoint::new(content_w - 150.0, (banner_h - 28.0) / 2.0),
                NSSize::new(140.0, 28.0)
            )
        ];
        set_control_title(open_btn, &t("settings.btn_open_privacy"));
        let _: () = msg_send![open_btn, setBezelStyle: 1isize];
        let _: () = msg_send![open_btn, setTarget: target];
        let _: () = msg_send![open_btn, setAction: sel!(handleOpenPrivacy:)];
        let _: () = msg_send![banner, addSubview: open_btn];
        release_obj(open_btn);

        // 默认按当前权限显隐(有权限就隐藏)/ initial visibility: hidden when permission is already granted
        let _: () = msg_send![banner, setHidden: has_accessibility_permission()];

        y -= banner_h + 12.0;

        // --- 外观 Appearance ---
        y -= 24.0;
        add_header(
            general_view,
            &t("settings.header_appearance"),
            12.0,
            y,
            content_w - 24.0,
        );
        y -= 8.0 + row_h;
        ui.theme = add_row(
            general_view,
            label_x,
            y,
            label_w,
            row_h,
            &t("settings.row_theme"),
            make_popup(ctrl_x, y, ctrl_w, row_h, &["dark", "light", "auto"], 0),
        );
        y -= row_pitch;
        ui.glass_style = add_row(
            general_view,
            label_x,
            y,
            label_w,
            row_h,
            &t("settings.row_glass_style"),
            make_popup(ctrl_x, y, ctrl_w, row_h, &["regular", "clear"], 0),
        );
        y -= row_pitch;
        // TODO: glass_tint 改用 NSColorWell(系统取色器)替代 hex 文本框,体验更好。
        // TODO: replace glass_tint's hex text field with NSColorWell (system color picker).
        ui.glass_tint = add_row(
            general_view,
            label_x,
            y,
            label_w,
            row_h,
            &t("settings.row_glass_tint"),
            make_text_input(ctrl_x, y, ctrl_w, row_h, "eeeeee66"),
        );
        y -= row_pitch;
        ui.corner_radius = add_row(
            general_view,
            label_x,
            y,
            label_w,
            row_h,
            &t("settings.row_corner_radius"),
            make_text_input(ctrl_x, y, ctrl_w, row_h, "64"),
        );

        // --- 键盘 Keyboard ---
        y -= 14.0 + 24.0;
        add_header(
            general_view,
            &t("settings.header_keyboard"),
            12.0,
            y,
            content_w - 24.0,
        );
        y -= 8.0 + row_h;
        // 修饰键下拉项:显示 Option+Tab / Command+Tab(快捷键名,各 locale 保持原文);值由索引映射到 option/command。
        // Modifier popup items: show Option+Tab / Command+Tab (shortcut names, kept verbatim across locales);
        // the value is mapped from the index to option/command.
        let mod_labels = [
            t("settings.modifier_option"),
            t("settings.modifier_command"),
        ];
        let mod_label_refs: Vec<&str> = mod_labels.iter().map(|s| s.as_str()).collect();
        ui.modifier = add_row(
            general_view,
            label_x,
            y,
            label_w,
            row_h,
            &t("settings.row_modifier"),
            make_popup(ctrl_x, y, ctrl_w, row_h, &mod_label_refs, 0),
        );

        // --- 语言 Language ---
        y -= 14.0 + 24.0;
        add_header(
            general_view,
            &t("settings.header_language"),
            12.0,
            y,
            content_w - 24.0,
        );
        y -= 8.0 + row_h;
        ui.locale = add_row(
            general_view,
            label_x,
            y,
            label_w,
            row_h,
            &t("settings.row_locale"),
            make_popup(ctrl_x, y, ctrl_w, row_h, &LOCALE_LABELS, 0),
        );

        // --- 窗口 Window ---
        y -= 14.0 + 24.0;
        add_header(
            general_view,
            &t("settings.header_windows"),
            12.0,
            y,
            content_w - 24.0,
        );
        y -= 8.0 + row_h;
        // show_minimized 下拉框:项 = [不显示, 显示];默认 index 0(不显示)。
        // show_minimized popup: items = [Hide, Show]; default index 0 (hide).
        let sm_labels = [
            t("settings.show_minimized_hide"),
            t("settings.show_minimized_show"),
        ];
        let sm_label_refs: Vec<&str> = sm_labels.iter().map(|s| s.as_str()).collect();
        ui.show_minimized = add_row(
            general_view,
            label_x,
            y,
            label_w,
            row_h,
            &t("settings.row_show_minimized"),
            make_popup(ctrl_x, y, ctrl_w, row_h, &sm_label_refs, 0),
        );

        // --- 日志 Logging ---
        y -= 14.0 + 24.0;
        add_header(
            general_view,
            &t("settings.header_logging"),
            12.0,
            y,
            content_w - 24.0,
        );
        y -= 8.0 + row_h;
        // 日志级别下拉框:项 = [info, warn, error];默认 index 0(info)。
        // Log level popup: items = [info, warn, error]; default index 0 (info).
        let log_levels: [&str; 3] = ["info", "warn", "error"];
        ui.log_level = add_row(
            general_view,
            label_x,
            y,
            label_w,
            row_h,
            &t("settings.row_log_level"),
            make_popup(ctrl_x, y, ctrl_w, row_h, &log_levels, 0),
        );

        // --- 启动 Startup ---
        y -= 14.0 + 24.0;
        add_header(
            general_view,
            &t("settings.header_startup"),
            12.0,
            y,
            content_w - 24.0,
        );
        y -= 8.0 + row_h;
        // 开机自启勾选框:标题留空(左侧 row label 已说明),仅放一个开关。
        // Launch-at-login checkbox: empty title (the row label on the left already describes it).
        ui.launch_at_login = add_row(
            general_view,
            label_x,
            y,
            label_w,
            row_h,
            &t("settings.row_launch_at_login"),
            make_checkbox(ctrl_x, y, ctrl_w, row_h, "", false),
        );

        // ===== 实验性页内容 experimental page content =====
        let mut y = content_h - 12.0;

        // 顶部说明文字(次级标签色、小字号、自动换行)。
        // Top note (secondary label color, small font, word-wrapping).
        y -= 30.0;
        let note: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let note: *mut AnyObject = msg_send![note, initWithFrame: NSRect::new(NSPoint::new(12.0, y), NSSize::new(content_w - 24.0, 30.0))];
        // 自适应:贴顶、宽度拉伸 / adaptive: top-anchored, width sizable
        let _: () = msg_send![note, setAutoresizingMask: 10u64];
        let note_ns = make_nsstring(&t("settings.experimental_note"));
        let _: () = msg_send![note, setStringValue: note_ns];
        CFRelease(note_ns as *const c_void);
        let _: () = msg_send![note, setBezeled: false];
        let _: () = msg_send![note, setDrawsBackground: false];
        let _: () = msg_send![note, setEditable: false];
        let _: () = msg_send![note, setSelectable: true];
        let _: () = msg_send![note, setUsesSingleLineMode: false];
        let _: () = msg_send![note, setLineBreakMode: 0isize]; // NSLineBreakByWordWrapping
        let sec_color: *mut AnyObject = msg_send![class!(NSColor), secondaryLabelColor];
        let _: () = msg_send![note, setTextColor: sec_color];
        let note_font: *mut AnyObject = msg_send![class!(NSFont), messageFontOfSize: 11.0f64];
        let _: () = msg_send![note, setFont: note_font];
        let _: () = msg_send![experimental_view, addSubview: note];
        release_obj(note);

        // --- 布局 Layout ---
        y -= 14.0 + 24.0;
        add_header(
            experimental_view,
            &t("settings.header_layout"),
            12.0,
            y,
            content_w - 24.0,
        );
        y -= 8.0 + row_h;
        ui.cards_per_row = add_row(
            experimental_view,
            label_x,
            y,
            label_w,
            row_h,
            &t("settings.row_cards_per_row"),
            make_text_input(ctrl_x, y, ctrl_w, row_h, "6"),
        );
        y -= row_pitch;
        ui.card_width = add_row(
            experimental_view,
            label_x,
            y,
            label_w,
            row_h,
            &t("settings.row_card_width"),
            make_text_input(ctrl_x, y, ctrl_w, row_h, "140"),
        );
        y -= row_pitch;
        ui.card_height = add_row(
            experimental_view,
            label_x,
            y,
            label_w,
            row_h,
            &t("settings.row_card_height"),
            make_text_input(ctrl_x, y, ctrl_w, row_h, "180"),
        );
        y -= row_pitch;
        ui.card_gap = add_row(
            experimental_view,
            label_x,
            y,
            label_w,
            row_h,
            &t("settings.row_card_gap"),
            make_text_input(ctrl_x, y, ctrl_w, row_h, "0"),
        );
        y -= row_pitch;
        ui.icon_size = add_row(
            experimental_view,
            label_x,
            y,
            label_w,
            row_h,
            &t("settings.row_icon_size"),
            make_text_input(ctrl_x, y, ctrl_w, row_h, "110"),
        );

        // ===== 鼠标页内容 mouse page content =====
        let mut y = content_h - 12.0;

        // --- 启用鼠标控制(总开关) / Enable mouse control ---
        y -= 8.0 + row_h;
        ui.enable_mouse = add_row(
            mouse_view,
            label_x,
            y,
            label_w,
            row_h,
            &t("settings.row_enable_mouse"),
            make_checkbox(ctrl_x, y, ctrl_w, row_h, "", false),
        );
        // 勾选框 toggle 时实时更新 OK 按钮标题(确认 vs 确认并重启)。
        // Update OK button title in real time when checkbox toggles (OK vs OK && Restart).
        let _: () = msg_send![ui.enable_mouse, setTarget: target];
        let _: () = msg_send![ui.enable_mouse, setAction: sel!(handleEnableMouseToggle:)];

        // --- 滚动模式 / Scroll mode ---
        y -= 8.0 + row_h;
        ui.scroll_mode = add_row(
            mouse_view,
            label_x,
            y,
            label_w,
            row_h,
            &t("settings.row_scroll_mode"),
            make_popup(ctrl_x, y, ctrl_w, row_h, &SCROLL_MODE_LABELS, 0),
        );

        // --- 行数(按行模式) / Line count (line mode) ---
        y -= 8.0 + row_h;
        ui.line_count = add_row(
            mouse_view,
            label_x,
            y,
            label_w,
            row_h,
            &t("settings.row_line_count"),
            make_text_input(ctrl_x, y, ctrl_w, row_h, "3"),
        );

        // --- 平滑预设 / Smooth preset ---
        y -= 8.0 + row_h;
        ui.smooth_preset = add_row(
            mouse_view,
            label_x,
            y,
            label_w,
            row_h,
            &t("settings.row_smooth_preset"),
            make_popup(ctrl_x, y, ctrl_w, row_h, &SMOOTH_PRESET_LABELS, 0),
        );

        // --- 滚动 Scrolling ---
        y -= 14.0 + 24.0;
        y -= 14.0 + 24.0;
        add_header(
            mouse_view,
            &t("settings.header_mouse_scrolling"),
            12.0,
            y,
            content_w - 24.0,
        );
        y -= 8.0 + row_h;
        // reverse_scroll 勾选框:标题留空(左侧 row label 已说明),仅放一个开关。
        // reverse_scroll checkbox: empty title (the row label on the left already describes it).
        ui.reverse_scroll = add_row(
            mouse_view,
            label_x,
            y,
            label_w,
            row_h,
            &t("settings.row_reverse_scroll"),
            make_checkbox(ctrl_x, y, ctrl_w, row_h, "", false),
        );

        // --- 指针 Pointer ---
        y -= 14.0 + 24.0;
        add_header(
            mouse_view,
            &t("settings.header_mouse_pointer"),
            12.0,
            y,
            content_w - 24.0,
        );
        y -= 8.0 + row_h;
        // disable_pointer_accel 勾选框:禁用系统鼠标加速,光标 1:1 线性跟踪。
        // disable_pointer_accel checkbox: disable system pointer acceleration for 1:1 linear
        // cursor tracking.
        ui.disable_pointer_accel = add_row(
            mouse_view,
            label_x,
            y,
            label_w,
            row_h,
            &t("settings.row_disable_pointer_accel"),
            make_checkbox(ctrl_x, y, ctrl_w, row_h, "", false),
        );

        // --- 确认 / 取消(加在 contentView 上,三页都可见)---
        // Restore Defaults 按钮(左侧偏上,版本号在其下方),与 OK/Cancel 同在 contentView,两页都可见。
        // 宽度限制在 sidebar 内,避免右边跨过 sidebar 分隔线(x=sidebar_w=150)。
        // Restore Defaults button (left, raised; version label below it), on contentView like
        // OK/Cancel, visible on both pages. Width kept within the sidebar so the right edge
        // doesn't cross the sidebar divider (x=sidebar_w=150).
        let restore: *mut AnyObject = msg_send![class!(NSButton), alloc];
        let restore: *mut AnyObject = msg_send![restore, initWithFrame: NSRect::new(NSPoint::new(12.0, 44.0), NSSize::new(126.0, 28.0))];
        let _: () = msg_send![restore, setAutoresizingMask: 36u64]; // 贴底、贴左 / bottom- and left-anchored
        set_control_title(restore, &t("settings.btn_restore_defaults"));
        let _: () = msg_send![restore, setBezelStyle: 1isize];
        let _: () = msg_send![restore, setTarget: target];
        let _: () = msg_send![restore, setAction: sel!(handleRestoreDefaults:)];
        let _: () = msg_send![content, addSubview: restore];
        release_obj(restore);

        // 版本号(Restore Defaults 下方,左下角),数据来自 Cargo.toml version(编译期 env! 嵌入)。
        // 版本号本身是数据不本地化,只本地化「版本」标签。
        // Version label (below Restore Defaults, bottom-left); version data from Cargo.toml via
        // compile-time env!. The version number itself is data (not localized), only the label is.
        let version_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let version_label: *mut AnyObject = msg_send![version_label, initWithFrame: NSRect::new(NSPoint::new(12.0, 14.0), NSSize::new(126.0, 20.0))];
        let _: () = msg_send![version_label, setAutoresizingMask: 36u64]; // 贴底、贴左
        let version_text = tf(
            "settings.version_label",
            &[("version", env!("CARGO_PKG_VERSION"))],
        );
        let version_ns = make_nsstring(&version_text);
        let _: () = msg_send![version_label, setStringValue: version_ns];
        CFRelease(version_ns as *const c_void);
        let _: () = msg_send![version_label, setBezeled: false];
        let _: () = msg_send![version_label, setDrawsBackground: false];
        let _: () = msg_send![version_label, setEditable: false];
        let _: () = msg_send![version_label, setAlignment: 0isize]; // NSTextAlignmentLeft
        let version_font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 11.0f64];
        let _: () = msg_send![version_label, setFont: version_font];
        let _: () = msg_send![content, addSubview: version_label];
        release_obj(version_label);

        // OK / Cancel on contentView so they stay visible on both pages.
        let cancel: *mut AnyObject = msg_send![class!(NSButton), alloc];
        let cancel: *mut AnyObject = msg_send![cancel, initWithFrame: NSRect::new(NSPoint::new(view_w - 200.0, 14.0), NSSize::new(80.0, 28.0))];
        let _: () = msg_send![cancel, setAutoresizingMask: 33u64]; // 贴底、贴右 / bottom- and right-anchored
        set_control_title(cancel, &t("settings.btn_cancel"));
        let _: () = msg_send![cancel, setBezelStyle: 1isize];
        let _: () = msg_send![cancel, setTarget: target];
        let _: () = msg_send![cancel, setAction: sel!(handleSettingsCancel:)];
        let _: () = msg_send![content, addSubview: cancel];
        release_obj(cancel);

        let ok: *mut AnyObject = msg_send![class!(NSButton), alloc];
        let ok: *mut AnyObject = msg_send![ok, initWithFrame: NSRect::new(NSPoint::new(view_w - 110.0, 14.0), NSSize::new(90.0, 28.0))];
        let _: () = msg_send![ok, setAutoresizingMask: 33u64]; // 贴底、贴右
        set_control_title(ok, &t("settings.btn_ok"));
        let _: () = msg_send![ok, setBezelStyle: 1isize];
        let _: () = msg_send![ok, setTarget: target];
        let _: () = msg_send![ok, setAction: sel!(handleSettingsOk:)];
        let _: () = msg_send![content, addSubview: ok];
        ui.ok_button = ok;
        release_obj(ok);

        *SETTINGS_UI.lock().unwrap() = Some(ui);
    }
}

/// 作废缓存的设置窗口(释放并置 None),下次打开时按当前 locale 重建。
/// Invalidate the cached settings window (release + set None) so it is rebuilt with the
/// current locale on next open. 用于 locale 变更后让设置窗口标签换语言。
pub(crate) fn invalidate_settings_window() {
    unsafe {
        let mut ui = SETTINGS_UI.lock().unwrap();
        if let Some(u) = ui.take() {
            // 窗口 alloc 是 +1且 setReleasedWhenClosed:false,需手动 release 一次;
            // 其子控件已由父视图持有,随窗口 dealloc 释放。
            // The window is alloc +1 with setReleasedWhenClosed:false, so release once manually;
            // its subviews are retained by the parent view and dealloc with the window.
            let _: () = msg_send![u.window, orderOut: std::ptr::null::<AnyObject>()];
            release_obj(u.window);
            // 窗口被作废(销毁),切回 .accessory(可能 locale 变更时设置正开着)。
            // The window is invalidated/destroyed; flip back to .accessory (it may have been open
            // during a locale change).
            crate::set_settings_activation_policy(false);
        }
    }
}
