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
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::Ordering;

use crate::config::{reload_config, Config, CONFIG};
use crate::event_monitor::SHORTCUT_IS_CMD;
use crate::ffi::*;
use crate::i18n::{t, tf};
use crate::menu::{refresh_menu_titles, set_shortcut_mode};
use crate::overlay::{apply_theme, refresh_highlight, update_status_label};
// 跨模块共享状态(由 main.rs 持有)/ cross-module shared state (owned by main.rs)
use crate::MENU_TARGET;

// locale 下拉项:显示用各语言原生写法(语言选择器的通用约定),值对应 config.i18n.locale。
// Locale popup items: displayed in each language's own script (convention for language pickers);
// values map to config.i18n.locale.
const LOCALE_LABELS: [&str; 4] = ["Auto", "English", "简体中文", "繁體中文"];
const LOCALE_VALUES: [&str; 4] = ["auto", "en", "zh-Hans", "zh-Hant"];

// ========== 设置窗口状态 / settings window state ==========

// 设置窗口的控件指针集合（非模态窗口，复用，隐藏而非销毁）。
// Holds pointers to the settings window's controls (non-modal, reused, hidden not destroyed).
struct SettingsUi {
    window: *mut AnyObject,
    theme: *mut AnyObject,         // NSPopUpButton: dark / light / auto
    glass_style: *mut AnyObject,   // NSPopUpButton: regular / clear
    glass_tint: *mut AnyObject,    // NSTextField: RRGGBBAA hex
    corner_radius: *mut AnyObject, // NSTextField
    cards_per_row: *mut AnyObject,
    card_width: *mut AnyObject,
    card_height: *mut AnyObject,
    card_gap: *mut AnyObject,
    icon_size: *mut AnyObject,
    modifier: *mut AnyObject,      // NSPopUpButton: option / command
    locale: *mut AnyObject,        // NSPopUpButton: auto / en / zh-Hans / zh-Hant
    show_minimized: *mut AnyObject, // NSPopUpButton: 不显示 / 显示 / show minimized windows (hide / show)
    log_level: *mut AnyObject,     // NSPopUpButton: trace / debug / info / warn / error
}
unsafe impl Send for SettingsUi {}
unsafe impl Sync for SettingsUi {}
static SETTINGS_UI: Mutex<Option<SettingsUi>> = Mutex::new(None);

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
    let field: *mut AnyObject = msg_send![field, initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))];
    let ns = make_nsstring(value);
    let _: () = msg_send![field, setStringValue: ns];
    CFRelease(ns as *const c_void);
    field
}

/// 下拉选择控件(alloc +1)。
/// Pop-up button (alloc +1).
unsafe fn make_popup(x: f64, y: f64, w: f64, h: f64, items: &[&str], selected: usize) -> *mut AnyObject {
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

/// 区块标题(加粗 label),加入父视图后 release。
/// Bold section header label; released after being added to the parent.
unsafe fn add_header(parent: *mut AnyObject, text: &str, x: f64, y: f64, w: f64) {
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let label: *mut AnyObject = msg_send![label, initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, 20.0))];
    let ns = make_nsstring(text);
    let _: () = msg_send![label, setStringValue: ns];
    CFRelease(ns as *const c_void);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let font: *mut AnyObject = msg_send![class!(NSFont), boldSystemFontOfSize: 13.0f64];
    let _: () = msg_send![label, setFont: font];
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
    let _: () = msg_send![parent, addSubview: label];
    release_obj(label);
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
    if let Err(e) = cfg.save() {
        show_alert(&t("alert.save_failed_title"), &e);
        return;
    }
    let _ = reload_config();
    set_shortcut_mode(cfg.keyboard.modifier == "command");
    apply_config_refresh();
    hide_settings();
}

pub(crate) extern "C" fn on_settings_cancel(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    hide_settings();
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
        let ui = SETTINGS_UI.lock().unwrap();
        if let Some(u) = ui.as_ref() {
            let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![nsapp, activateIgnoringOtherApps: true];
            let _: () = msg_send![u.window, makeKeyAndOrderFront: std::ptr::null::<AnyObject>()];
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
        let _: () = msg_send![alert, addButtonWithTitle: ns3];
        CFRelease(ns3 as *const c_void);
        let _resp: isize = msg_send![alert, runModal];
        release_obj(alert);
    }
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
        let gs_idx: isize = if cfg.appearance.glass_style == "clear" { 1 } else { 0 };
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
        cfg.appearance.glass_style = if gs_idx == 1 { "clear".into() } else { "regular".into() };
        cfg.appearance.glass_tint = nsstring_to_rust(msg_send![ui.glass_tint, stringValue]);
        match parse_f64(&nsstring_to_rust(msg_send![ui.corner_radius, stringValue])) {
            Ok(v) => cfg.appearance.corner_radius = v,
            Err(_) => errs.push(tf("errors.not_a_number", &[("field", "appearance.corner_radius")])),
        }
        match parse_usize(&nsstring_to_rust(msg_send![ui.cards_per_row, stringValue])) {
            Ok(v) => cfg.layout.cards_per_row = v,
            Err(_) => errs.push(tf("errors.not_an_integer", &[("field", "layout.cards_per_row")])),
        }
        match parse_f64(&nsstring_to_rust(msg_send![ui.card_width, stringValue])) {
            Ok(v) => cfg.layout.card_width = v,
            Err(_) => errs.push(tf("errors.not_a_number", &[("field", "layout.card_width")])),
        }
        match parse_f64(&nsstring_to_rust(msg_send![ui.card_height, stringValue])) {
            Ok(v) => cfg.layout.card_height = v,
            Err(_) => errs.push(tf("errors.not_a_number", &[("field", "layout.card_height")])),
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
        cfg.keyboard.modifier = if mod_idx == 1 { "command".into() } else { "option".into() };
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
            1 => "warn", 2 => "error",
            _ => "info", // index 0 or out-of-range
        }.into();
    }
    for e in cfg.validate() {
        errs.push(e);
    }
    (cfg, errs)
}

/// 构建设置窗口(只建一次,存入 SETTINGS_UI,之后复用、隐藏而非销毁)。
/// Build the settings window once, store it in SETTINGS_UI, then reuse (hide, not destroy).
fn create_settings_window() {
    unsafe {
        let view_w = 420.0;
        let style: u64 = (1 << 0) | (1 << 1); // titled + closable
        // 窗口高度:加语言区段后从 500 调到 570,避免最后一行与底部按钮重叠。
        // 再加窗口区段(checkbox)后调到 640,加日志区段后调到 680。
        // Window height: 500 -> 570 for the Language section, -> 640 for the Window section,
        // -> 680 for the Logging section, so the last row doesn't overlap the bottom buttons.
        let frame = NSRect::new(NSPoint::new(220.0, 180.0), NSSize::new(view_w, 680.0));
        let window: *mut AnyObject = msg_send![class!(NSWindow), alloc];
        let window: *mut AnyObject = msg_send![window, initWithContentRect: frame, styleMask: style, backing: 2u64, defer: false];
        let ns_title = make_nsstring(&t("settings.window_title"));
        let _: () = msg_send![window, setTitle: ns_title];
        CFRelease(ns_title as *const c_void);
        let _: () = msg_send![window, setReleasedWhenClosed: false];
        let content: *mut AnyObject = msg_send![window, contentView];
        // 用 contentView 的实际高度做布局(标题栏会占掉一部分,不能直接用窗口高度)。
        // Layout against the contentView's real height (the title bar eats part of it).
        let content_frame: NSRect = msg_send![content, frame];
        let content_h = content_frame.size.height;

        let mut ui = SettingsUi {
            window,
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
        };

        let label_x = 12.0;
        let label_w = 150.0;
        let ctrl_x = 170.0;
        let ctrl_w = view_w - ctrl_x - 12.0;
        let row_h = 22.0;
        let row_pitch = 28.0;
        let mut y = content_h - 12.0; // 顶部光标:下一个元素的底边 y / top cursor (bottom y of next element)

        // --- 外观 Appearance ---
        y -= 24.0;
        add_header(content, &t("settings.header_appearance"), 12.0, y, view_w - 24.0);
        y -= 8.0 + row_h;
        ui.theme = add_row(content, label_x, y, label_w, row_h, &t("settings.row_theme"), make_popup(ctrl_x, y, ctrl_w, row_h, &["dark", "light", "auto"], 0));
        y -= row_pitch;
        ui.glass_style = add_row(content, label_x, y, label_w, row_h, &t("settings.row_glass_style"), make_popup(ctrl_x, y, ctrl_w, row_h, &["regular", "clear"], 0));
        y -= row_pitch;
        // TODO: glass_tint 改用 NSColorWell(系统取色器)替代 hex 文本框,体验更好。
        // TODO: replace glass_tint's hex text field with NSColorWell (system color picker).
        ui.glass_tint = add_row(content, label_x, y, label_w, row_h, &t("settings.row_glass_tint"), make_text_input(ctrl_x, y, ctrl_w, row_h, "eeeeee66"));
        y -= row_pitch;
        ui.corner_radius = add_row(content, label_x, y, label_w, row_h, &t("settings.row_corner_radius"), make_text_input(ctrl_x, y, ctrl_w, row_h, "64"));

        // --- 布局 Layout ---
        y -= 14.0 + 24.0;
        add_header(content, &t("settings.header_layout"), 12.0, y, view_w - 24.0);
        y -= 8.0 + row_h;
        ui.cards_per_row = add_row(content, label_x, y, label_w, row_h, &t("settings.row_cards_per_row"), make_text_input(ctrl_x, y, ctrl_w, row_h, "6"));
        y -= row_pitch;
        ui.card_width = add_row(content, label_x, y, label_w, row_h, &t("settings.row_card_width"), make_text_input(ctrl_x, y, ctrl_w, row_h, "140"));
        y -= row_pitch;
        ui.card_height = add_row(content, label_x, y, label_w, row_h, &t("settings.row_card_height"), make_text_input(ctrl_x, y, ctrl_w, row_h, "180"));
        y -= row_pitch;
        ui.card_gap = add_row(content, label_x, y, label_w, row_h, &t("settings.row_card_gap"), make_text_input(ctrl_x, y, ctrl_w, row_h, "0"));
        y -= row_pitch;
        ui.icon_size = add_row(content, label_x, y, label_w, row_h, &t("settings.row_icon_size"), make_text_input(ctrl_x, y, ctrl_w, row_h, "110"));

        // --- 键盘 Keyboard ---
        y -= 14.0 + 24.0;
        add_header(content, &t("settings.header_keyboard"), 12.0, y, view_w - 24.0);
        y -= 8.0 + row_h;
        // 修饰键下拉项:显示 Option+Tab / Command+Tab(快捷键名,各 locale 保持原文);值由索引映射到 option/command。
        // Modifier popup items: show Option+Tab / Command+Tab (shortcut names, kept verbatim across locales);
        // the value is mapped from the index to option/command.
        let mod_labels = [t("settings.modifier_option"), t("settings.modifier_command")];
        let mod_label_refs: Vec<&str> = mod_labels.iter().map(|s| s.as_str()).collect();
        ui.modifier = add_row(content, label_x, y, label_w, row_h, &t("settings.row_modifier"), make_popup(ctrl_x, y, ctrl_w, row_h, &mod_label_refs, 0));

        // --- 语言 Language ---
        y -= 14.0 + 24.0;
        add_header(content, &t("settings.header_language"), 12.0, y, view_w - 24.0);
        y -= 8.0 + row_h;
        ui.locale = add_row(content, label_x, y, label_w, row_h, &t("settings.row_locale"), make_popup(ctrl_x, y, ctrl_w, row_h, &LOCALE_LABELS, 0));

        // --- 窗口 Window ---
        y -= 14.0 + 24.0;
        add_header(content, &t("settings.header_windows"), 12.0, y, view_w - 24.0);
        y -= 8.0 + row_h;
        // show_minimized 下拉框:项 = [不显示, 显示];默认 index 0(不显示)。
        // show_minimized popup: items = [Hide, Show]; default index 0 (hide).
        let sm_labels = [t("settings.show_minimized_hide"), t("settings.show_minimized_show")];
        let sm_label_refs: Vec<&str> = sm_labels.iter().map(|s| s.as_str()).collect();
        ui.show_minimized = add_row(content, label_x, y, label_w, row_h, &t("settings.row_show_minimized"), make_popup(ctrl_x, y, ctrl_w, row_h, &sm_label_refs, 0));

        // --- 日志 Logging ---
        y -= 14.0 + 24.0;
        add_header(content, &t("settings.header_logging"), 12.0, y, view_w - 24.0);
        y -= 8.0 + row_h;
        // 日志级别下拉框:项 = [info, warn, error];默认 index 0(info)。
        // Log level popup: items = [info, warn, error]; default index 0 (info).
        let log_levels: [&str; 3] = ["info", "warn", "error"];
        ui.log_level = add_row(content, label_x, y, label_w, row_h, &t("settings.row_log_level"), make_popup(ctrl_x, y, ctrl_w, row_h, &log_levels, 0));

        // --- 确认 / 取消 ---
        let target = match *MENU_TARGET.lock().unwrap() {
            Some(t) => t.0,
            None => return,
        };
        let cancel: *mut AnyObject = msg_send![class!(NSButton), alloc];
        let cancel: *mut AnyObject = msg_send![cancel, initWithFrame: NSRect::new(NSPoint::new(view_w - 200.0, 14.0), NSSize::new(80.0, 28.0))];
        set_control_title(cancel, &t("settings.btn_cancel"));
        let _: () = msg_send![cancel, setBezelStyle: 1isize];
        let _: () = msg_send![cancel, setTarget: target];
        let _: () = msg_send![cancel, setAction: sel!(handleSettingsCancel:)];
        let _: () = msg_send![content, addSubview: cancel];
        release_obj(cancel);

        let ok: *mut AnyObject = msg_send![class!(NSButton), alloc];
        let ok: *mut AnyObject = msg_send![ok, initWithFrame: NSRect::new(NSPoint::new(view_w - 110.0, 14.0), NSSize::new(90.0, 28.0))];
        set_control_title(ok, &t("settings.btn_ok"));
        let _: () = msg_send![ok, setBezelStyle: 1isize];
        let _: () = msg_send![ok, setTarget: target];
        let _: () = msg_send![ok, setAction: sel!(handleSettingsOk:)];
        let _: () = msg_send![content, addSubview: ok];
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
        }
    }
}
