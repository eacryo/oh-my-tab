//! 设置窗口:SettingsUi 状态、控件构造器(text/popup/header/row)、窗口构建/显示/收集、
//! 校验告警、以及配置热应用(apply_config_refresh)。invalidate_settings_window 作废缓存
//! 窗口供 locale 变更后重建。
//!
//! Settings window: SettingsUi state, control builders (text/popup/header/row), window
//! build/show/collect, validation alerts, and hot config application (apply_config_refresh).
//! invalidate_settings_window drops the cached window so it rebuilds after a locale change.

use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::ffi::{c_void, CString};
use std::sync::atomic::Ordering;
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
const SCROLL_MODE_LABELS: [&str; 2] = ["Default", "Line"];
const SCROLL_MODE_VALUES: [&str; 2] = ["default", "line"];
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
    modifier: *mut AnyObject,         // NSPopUpButton: option / command
    locale: *mut AnyObject,           // NSPopUpButton: auto / en / zh-Hans / zh-Hant
    show_minimized: *mut AnyObject,   // NSSwitch: 显示最小化窗口 / show minimized windows
    overlay_position: *mut AnyObject, // NSPopUpButton: 跟随激活窗口 / 主屏幕 / overlay position (follow active window / main screen)
    log_level: *mut AnyObject,        // NSPopUpButton: trace / debug / info / warn / error
    launch_at_login: *mut AnyObject,  // NSSwitch: 开机自启 / launch at login
    reverse_scroll: *mut AnyObject,   // NSSwitch: 反转滚动 / reverse scrolling
    enable_mouse: *mut AnyObject,     // NSSwitch: 启用鼠标控制 / enable mouse control
    scroll_mode: *mut AnyObject,      // NSPopUpButton: default/line
    line_count: *mut AnyObject,       // NSSlider: line count slider
    line_count_label: *mut AnyObject, // NSTextField: line count row 的 label / the row's label
    line_count_value_label: *mut AnyObject, // NSTextField: 滑块当前值(只读)/ slider's current value (read-only)
    disable_pointer_accel: *mut AnyObject,  // NSSwitch: 禁用指针加速 / disable pointer acceleration
    device_indicator: *mut AnyObject, // NSButton: 当前选中设备指示器(点击打开选择器) / device indicator (opens picker)
    ok_button: *mut AnyObject,        // NSButton: 确认按钮 / OK button
    accessibility_warning_view: *mut AnyObject, // NSView: 缺权限警告条容器 / permission-warning banner container
}
unsafe impl Send for SettingsUi {}
unsafe impl Sync for SettingsUi {}
static SETTINGS_UI: Mutex<Option<SettingsUi>> = Mutex::new(None);

/// 当前在鼠标页选中的设备范围。None = "所有鼠标";Some((vid,pid)) = 某款具体鼠标。
/// The currently-selected device scope on the Mouse page. None = "All Mice";
/// Some((vid,pid)) = a specific mouse.
static SELECTED_DEVICE: Mutex<Option<Option<crate::mouse::device::DeviceKey>>> = Mutex::new(None);

/// "自动切换到活跃设备"开关(内存态,不入配置)。
/// "Auto switch to active device" toggle (in-memory, not persisted to config).
#[allow(dead_code)]
static AUTO_SWITCH_DEVICE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

// ========== 控件构造 helper / control-builder helpers ==========

fn parse_f64(s: &str) -> Result<f64, ()> {
    s.trim().parse::<f64>().map_err(|_| ())
}
fn parse_usize(s: &str) -> Result<usize, ()> {
    s.trim().parse::<usize>().map_err(|_| ())
}

// ========== 鼠标 profile 读写 helper / mouse profile read/write helpers ==========

use crate::config::{DeviceMatcher, MouseProfile, PartialPointerSection};

/// 当前在鼠标页选中的设备范围(读 SELECTED_DEVICE;未初始化时默认 None="所有鼠标")。
/// The currently-selected device scope on the Mouse page (reads SELECTED_DEVICE; defaults to
/// None = "All Mice" when uninitialized).
fn current_selected_device() -> Option<crate::mouse::device::DeviceKey> {
    // SELECTED_DEVICE:外层 Option 表示"是否初始化过";内层 None = "所有鼠标"。
    // SELECTED_DEVICE: outer Option = "initialized?"; inner None = "All Mice".
    SELECTED_DEVICE.lock().unwrap().unwrap_or(None) // 未初始化 -> None("所有鼠标") / uninitialized -> None (All Mice)
}

/// 在 CONFIG 中查找匹配设备(VID,PID)的 profile 索引。None = 查找"所有鼠标"档。
/// Find the index of the profile matching (VID,PID) in CONFIG. None = find "All Mice".
fn find_profile_index(
    cfg: &Config,
    device: Option<crate::mouse::device::DeviceKey>,
) -> Option<usize> {
    cfg.mouse.profiles.iter().position(|p| {
        let vid_ok = p
            .device
            .vendor_id
            .map(|v| Some(v) == device.map(|(vid, _)| vid))
            .unwrap_or(device.is_none());
        let pid_ok = p
            .device
            .product_id
            .map(|p| Some(p) == device.map(|(_, pid)| pid))
            .unwrap_or(device.is_none());
        vid_ok && pid_ok
    })
}

/// 读取当前选中设备的有效值(合并"所有鼠标"档 + 该设备档后的结果),基于给定 Config 解析。
/// 用于在 UI 上显示当前实际生效的配置,以及恢复默认预览(传 Config::default())。
///
/// Read the effective value for the currently-selected device (merging the "All Mice" profile +
/// the device profile), resolved from a given Config. Used to show the effective config in the
/// UI, and for the restore-defaults preview (passing Config::default()).
fn resolve_selected_from(cfg: &Config) -> crate::mouse::resolve::ResolvedMouse {
    let dev = current_selected_device();
    crate::mouse::resolve::resolve_from_config(cfg, dev)
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

/// 原生 toggle switch(NSSwitch,开启蓝色/关闭灰色,系统设置同款)。
/// alloc +1,加入父视图后由调用方 release。
/// Native toggle switch (NSSwitch: blue when on, grey when off; same as System Settings).
/// alloc +1; caller releases after adding to parent.
/// 参数 right_x = 开关的右边界:所有开关右对齐到该边界,与下拉框(popup)的右缘
/// (ctrl_x + ctrl_w)保持一致,开关行不再左对齐。
/// The right_x parameter is the switch's RIGHT edge: every switch right-aligns to it, matching
/// the popups' right edge (ctrl_x + ctrl_w), so switch rows no longer left-align.
unsafe fn make_switch(right_x: f64, y: f64, h: f64, checked: bool) -> *mut AnyObject {
    let sw: *mut AnyObject = msg_send![class!(NSSwitch), alloc];
    let sw: *mut AnyObject =
        msg_send![sw, initWithFrame: NSRect::new(NSPoint::new(right_x, y), NSSize::new(0.0, 0.0))];
    // 用 setControlSize: 调小一档(small,regular 约 38x22,small 约 30x19),与设置行更协调。
    // setControlSize: shrinks the switch by one step (regular ~38x22, small ~30x19) so it fits
    // the settings rows better. fittingSize 再拿该档位的固有尺寸并垂直居中于行。
    let _: () = msg_send![sw, setControlSize: 1isize]; // NSControlSizeSmall
    let fs: NSSize = msg_send![sw, fittingSize];
    let (sw_w, sw_h) = if fs.width > 0.0 {
        (fs.width, fs.height)
    } else {
        (30.0, 19.0)
    };
    let _: () = msg_send![
        sw,
        setFrame: NSRect::new(
            NSPoint::new(right_x - sw_w, y + (h - sw_h) / 2.0),
            NSSize::new(sw_w, sw_h)
        )
    ];
    let _: () = msg_send![sw, setState: if checked { 1isize } else { 0isize }];
    sw
}

/// 整数滑块(NSSlider, min..=max, step 1)。alloc +1,加入父视图后由调用方 release。
/// Integer slider (NSSlider, min..=max, step 1). alloc +1; caller releases after adding to parent.
unsafe fn make_slider(
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    min: i64,
    max: i64,
    value: i64,
) -> *mut AnyObject {
    let slider: *mut AnyObject = msg_send![class!(NSSlider), alloc];
    let slider: *mut AnyObject =
        msg_send![slider, initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))];
    let _: () = msg_send![slider, setMinValue: min as f64];
    let _: () = msg_send![slider, setMaxValue: max as f64];
    // 整数步进:1 格 = 1 个单位(线性 Mouse By Lines 滑块同款:0...10 step 1)。
    // Integer steps: 1 tick = 1 unit (same as LinearMouse's By Lines slider: 0...10 step 1).
    let _: () = msg_send![slider, setNumberOfTickMarks: (max - min + 1) as isize];
    let _: () = msg_send![slider, setAllowsTickMarkValuesOnly: true];
    let _: () = msg_send![slider, setIntegerValue: value];
    slider
}

/// 设侧边栏按钮标题为 attributed title:未选中用 labelColor(系统文本色),选中用纯白
/// (whiteColor,与蓝色选中高亮搭配,原生 source-list 选中行文字观感同款)。
/// 注意:alternateSelectedControlTextColor 虽语义正确,但在 attributed title 里会被
/// macOS 26 解析成深色(实测渲染为 0,61,127),不能用;纯白渲染正常。
/// Set the sidebar button's title as an attributed title: labelColor when unselected, plain
/// white when selected (matches the accent-blue highlight; same look as native source-list
/// selection text). Note: alternateSelectedControlTextColor looks right semantically but macOS 26
/// resolves it to a dark color (measured 0,61,127) inside attributed titles, so plain white is used.
unsafe fn set_sidebar_title(btn: *mut AnyObject, title: &str, selected: bool) {
    let font: *mut AnyObject = if selected {
        msg_send![class!(NSFont), boldSystemFontOfSize: 13.0f64]
    } else {
        msg_send![class!(NSFont), messageFontOfSize: 13.0f64]
    };
    let color: *mut AnyObject = if selected {
        msg_send![class!(NSColor), whiteColor]
    } else {
        msg_send![class!(NSColor), labelColor]
    };
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
    add_row_with_label(parent, label_x, y, label_w, h, label_text, control).1
}

/// 同 add_row,但额外返回 label 指针(供条件显隐等需要隐藏整行的场景)。
/// label/control 加入父视图后由父视图持有,release 后指针仍有效(与 add_row 返回 control
/// 的约定一致)。
///
/// Same as add_row, but also returns the label pointer (for cases that need to hide the whole
/// row, e.g. conditional visibility). The label/control are retained by the parent view after
/// addSubview, so the pointers remain valid after release (same convention as add_row's
/// returned control).
unsafe fn add_row_with_label(
    parent: *mut AnyObject,
    label_x: f64,
    y: f64,
    label_w: f64,
    h: f64,
    label_text: &str,
    control: *mut AnyObject,
) -> (*mut AnyObject, *mut AnyObject) {
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
    (label, control)
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

    // 检查是否需要重启:对比新旧 mouse.enabled。按钮标题已在 switch toggle 时实时更新。
    // Check if restart needed: button title already updated in real time when the switch toggled.
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
        // 运行时热切换鼠标 event tap,不再 spawn 新进程(避免孤儿进程)。
        // Hot-switch the mouse event tap at runtime; no more process spawn (avoids orphans).
        if cfg.mouse.enabled {
            crate::mouse::start();
        } else {
            crate::mouse::stop();
        }
    }
}

/// 根据 enable_mouse switch 状态,冻结或解冻其下方的所有鼠标控件。
/// 未启用时控件灰显且不可交互(AppKit 自动处理灰显),避免用户修改无效配置。
///
/// Freeze or unfreeze all mouse controls below the enable_mouse switch based on its state.
/// When disabled, controls are greyed out and non-interactive (AppKit handles greying), preventing
/// users from editing config that won't take effect.
unsafe fn update_mouse_controls_enabled(ui: &SettingsUi) {
    let state: isize = msg_send![ui.enable_mouse, state];
    let on = state == 1;
    // 冻结 enable_mouse 以下的所有控件(含设备下拉框)。
    // Freeze everything below enable_mouse (including the device popup).
    for &ctrl in &[
        ui.device_indicator,
        ui.scroll_mode,
        ui.line_count,
        ui.reverse_scroll,
        ui.disable_pointer_accel,
    ] {
        let _: () = msg_send![ctrl, setEnabled: on];
    }
}

/// 根据当前滚动模式(Default/Line)刷新"行数"行的条件显隐:
/// - Line:显示"每 tick 行数"行
/// - Default:隐藏
///
/// 由 load_settings_values 与 handle_scroll_mode_changed 调用。
///
/// Refresh the conditional visibility of the "lines per tick" row based on the current scroll mode
/// (Default/Line):
/// - Line: the "lines per tick" row is shown
/// - Default: hidden
///
/// Called by load_settings_values and handle_scroll_mode_changed.
unsafe fn update_mode_dependent_visibility(ui: &SettingsUi) {
    let idx: isize = msg_send![ui.scroll_mode, indexOfSelectedItem];
    let mode = SCROLL_MODE_VALUES
        .get(idx as usize)
        .copied()
        .unwrap_or("default");
    // 只有 Line 模式显示行数滑块(Default 不显示)。
    // Only Line mode shows the line-count slider (hidden on Default).
    let show_line = mode == "line";
    let _: () = msg_send![ui.line_count_label, setHidden: !show_line];
    let _: () = msg_send![ui.line_count, setHidden: !show_line];
    // 行数滑块右侧的数值 label 随滑块一起显隐。
    // The line-count slider's value label hides with the slider.
    let _: () = msg_send![ui.line_count_value_label, setHidden: !show_line];
}

/// scroll_mode 下拉框切换回调:即时刷新行数行的条件显隐。
/// Scroll-mode popup changed callback: refresh the conditional visibility of the
/// lines-per-tick row immediately.
pub(crate) extern "C" fn handle_scroll_mode_changed(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    unsafe {
        let ui = SETTINGS_UI.lock().unwrap();
        if let Some(u) = ui.as_ref() {
            // 行数滑块显示当前配置的值(Line 模式的 line_count)。
            // The line-count slider shows the configured value (Line's line_count).
            let shown = {
                let cfg = CONFIG.read().unwrap().clone();
                resolve_selected_from(&cfg).line_count
            };
            let _: () = msg_send![u.line_count, setIntegerValue: shown as isize];
            set_field(u.line_count_value_label, shown);
            update_mode_dependent_visibility(u);
        }
    }
}

/// line_count 滑块拖动回调:实时刷新右侧数值 label。
/// Line-count slider drag callback: refresh the value label on the right live.
pub(crate) extern "C" fn handle_line_count_changed(
    _self: *mut c_void,
    _cmd: Sel,
    sender: *mut c_void,
) {
    unsafe {
        let slider = sender as *mut AnyObject;
        let val: isize = msg_send![slider, integerValue];
        let ui = SETTINGS_UI.lock().unwrap();
        if let Some(u) = ui.as_ref() {
            set_field(u.line_count_value_label, val);
        }
    }
}

/// enable_mouse switch toggle 回调:冻结/解冻下方控件。
/// Callback when the enable_mouse switch is toggled: freeze/unfreeze the controls below.
pub(crate) extern "C" fn handle_enable_mouse_toggle(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    unsafe {
        let ui = SETTINGS_UI.lock().unwrap();
        if let Some(u) = ui.as_ref() {
            update_mouse_controls_enabled(u);
        }
    }
}

pub(crate) extern "C" fn on_settings_cancel(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    hide_settings();
}

/// 设备下拉框的项与 DeviceKey 的映射(与 popup items 一一对应),供 handle_device_changed
/// 按 indexOfSelectedItem 反查。每次 rebuild_device_popup 重建。
/// 只有具体设备项,无"所有鼠标"通配项。
///
/// Mapping from popup-item index to DeviceKey (1:1 with popup items), used by
/// handle_device_changed to look up the selected device by indexOfSelectedItem. Rebuilt each time
/// rebuild_device_popup runs. Contains only concrete devices; no "All Mice" wildcard entry.
static DEVICE_POPUP_KEYS: Mutex<Vec<crate::mouse::device::DeviceKey>> = Mutex::new(Vec::new());

/// 基于当前已连接设备列表初始化/校准 SELECTED_DEVICE。
/// 必须在 resolve_selected() 之前调用,保证 resolve 拿到的是有效设备:
/// - 未初始化(首次打开设置)-> 选中第一个设备(若有)
/// - 已初始化但所选设备已被拔出 -> 回退到第一个设备
/// - 所选设备仍在列表 -> 保持
///
/// 无设备连接时清空选中(编辑"所有鼠标"基础层)。
/// 每次打开设置都重新校准,天然处理热插拔(设备增减)。
///
/// Initialize/calibrate SELECTED_DEVICE against the current connected-device list. Must run
/// before resolve_selected() so resolution always gets a valid device:
/// - uninitialized (first settings open) -> select the first device (if any)
/// - initialized but the selected device was unplugged -> fall back to the first device
/// - selected device still present -> keep it
///
/// With no devices connected, clears the selection (edits the "All Mice" base layer).
/// Recalibrated on every settings open, so hot-plug (device add/remove) is handled naturally.
fn ensure_selected_device() {
    let connected = crate::mouse::device::connected_devices();
    let cur = current_selected_device();

    if connected.is_empty() {
        // 无设备连接:清空选中状态(编辑"所有鼠标"基础层)。
        // No device connected: clear the selection (edits the "All Mice" base layer).
        *SELECTED_DEVICE.lock().unwrap() = Some(None);
        return;
    }

    // 当前设备仍在列表 -> 保持;否则(未初始化或被拔出)回退到第一个设备。
    // Keep the current device if it's still connected; otherwise (uninitialized or unplugged)
    // fall back to the first device.
    let still_connected = cur
        .map(|c| {
            connected
                .iter()
                .any(|d| d.vendor_id == c.0 && d.product_id == c.1)
        })
        .unwrap_or(false);
    if !still_connected {
        let first = &connected[0];
        *SELECTED_DEVICE.lock().unwrap() = Some(Some((first.vendor_id, first.product_id)));
    }
}

/// 重建设备下拉框的选项:仅各已连接设备(无"所有鼠标"通配项)。
/// 只负责 UI(items + 选中项);SELECTED_DEVICE 的状态校准由 ensure_selected_device 负责。
/// 由 load_settings_values 调用(每次打开设置时刷新,反映热插拔)。
///
/// Rebuild the device popup's items: only each connected device (no "All Mice" wildcard entry).
/// UI only (items + selection); SELECTED_DEVICE state calibration is handled by
/// ensure_selected_device. Called by load_settings_values (refreshed on each settings open to
/// reflect hot-plug changes).
unsafe fn rebuild_device_popup(ui: &SettingsUi) {
    let connected = crate::mouse::device::connected_devices();
    let cur = current_selected_device();

    // 构建下拉项与 key 映射:仅设备。
    // Build the popup items and the key mapping: devices only.
    let mut items: Vec<String> = Vec::new();
    let mut keys: Vec<crate::mouse::device::DeviceKey> = Vec::new();
    for d in &connected {
        items.push(format!(
            "{} ({:#x}:{:#x})",
            d.name, d.vendor_id, d.product_id
        ));
        keys.push((d.vendor_id, d.product_id));
    }

    // 清空旧项,填入新项。
    // Clear old items and fill in the new ones.
    let _: () = msg_send![ui.device_indicator, removeAllItems];
    for s in &items {
        let ns = make_nsstring(s);
        let _: () = msg_send![ui.device_indicator, addItemWithTitle: ns];
        CFRelease(ns as *const c_void);
    }

    // 选中当前设备对应的项;若已不在列表,选中第一个。
    // Select the item matching the current device; if it's gone, select the first.
    let sel_idx = cur
        .and_then(|c| keys.iter().position(|k| *k == c))
        .unwrap_or(0);
    if !keys.is_empty() {
        let _: () = msg_send![ui.device_indicator, selectItemAtIndex: sel_idx as isize];
    }

    *DEVICE_POPUP_KEYS.lock().unwrap() = keys;
}

/// 设置窗口开着时即时刷新设备下拉框(由插拔事件经主线程调用)。
/// 设备列表是外部实时状态(硬件插拔),不属于 OK/Cancel 门控范围——重连后应立即显示,
/// 无需点确定或重开设置。重建下拉用内存态 SELECTED_DEVICE 恢复选中,不会重置用户
/// 未保存的选择。窗口未打开时无操作(下次打开时 load_settings_values 仍会重建)。
///
/// Refresh the device popup live while the settings window is open (called on the main
/// thread from device plug/unplug events). The device list is external live state (hardware
/// attach/detach), not part of the OK/Cancel-gated preferences -- a reconnect should show
/// immediately without OK or reopening. The rebuild restores the selection from the in-memory
/// SELECTED_DEVICE, so unsaved choices survive. No-op when the window isn't open (it is
/// rebuilt on next open via load_settings_values anyway).
pub(crate) fn refresh_device_popup_if_open() {
    unsafe {
        let ui = SETTINGS_UI.lock().unwrap();
        if let Some(u) = ui.as_ref() {
            let visible: bool = msg_send![u.window, isVisible];
            if visible {
                rebuild_device_popup(u);
            }
        }
    }
}

/// 设备下拉框切换回调:更新 SELECTED_DEVICE 并即时刷新其余控件为新设备的有效值。
/// Device-popup selection-changed callback: update SELECTED_DEVICE and immediately refresh the
/// other controls with the newly-selected device's effective values.
pub(crate) extern "C" fn handle_device_changed(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    let popup = sender as *mut AnyObject;
    let idx: isize = unsafe { msg_send![popup, indexOfSelectedItem] };
    // DEVICE_POPUP_KEYS: Vec<DeviceKey>;取选中项对应的 key(均为具体设备,无通配项)。
    // DEVICE_POPUP_KEYS: Vec<DeviceKey>; get the key for the selected item (all concrete
    // devices; no wildcard entry).
    let new_dev = DEVICE_POPUP_KEYS.lock().unwrap().get(idx as usize).copied();
    *SELECTED_DEVICE.lock().unwrap() = Some(new_dev);
    // 只刷新鼠标页的 per-device 控件,不能走完整 load_settings_from——那会把
    // enable_mouse switch 重置为已保存的 cfg.mouse.enabled,冲掉用户刚勾选
    // 但尚未点 OK 的修改(启用鼠标控制是全局设置,切换设备不应动它)。
    // Only refresh the mouse page's per-device controls -- a full load_settings_from would
    // reset the enable_mouse switch to the saved cfg.mouse.enabled, wiping the user's
    // unsaved toggle (enable mouse control is a global setting; device switches must not
    // touch it).
    let cfg = CONFIG.read().unwrap().clone();
    let resolved = resolve_selected_from(&cfg);
    unsafe {
        let ui = SETTINGS_UI.lock().unwrap();
        if let Some(u) = ui.as_ref() {
            fill_mouse_device_controls(u, &resolved);
            // enable_mouse 勾选状态保持用户当前值;只重算冻结与条件显隐。
            // Keep the user's current enable_mouse state; only recompute freeze + visibility.
            update_mouse_controls_enabled(u);
            update_mode_dependent_visibility(u);
        }
    }
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
        // 选中项粗体 + 白字(whiteColor),未选中项常规 labelColor。
        // Bold + white text when selected; regular labelColor otherwise.
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

/// 红绿灯偏移常量:把系统红绿灯往右下挪一点,与左侧玻璃卡片对齐。
/// 窗口坐标 y 向上,右下 = x 增大 / y 减小。
/// Traffic-light offset: nudge the standard buttons down-right to align with the glass card.
/// Window coordinates point up, so down-right = x+ / y-.
const TRAFFIC_LIGHT_DX: f64 = 8.0;
const TRAFFIC_LIGHT_DY: f64 = -6.0;

/// 把三个红绿灯按钮往右下偏移:通过公开 API standardWindowButton: 拿到按钮视图直接改 frame
/// (没有公开 API 直接设红绿灯位置,旧私有 API setTrafficLightPosition: 等在 macOS 26 已移除,
/// 实测这是唯一可靠的做法)。
/// 注意:两参的 +standardWindowButton:forStyleMask: 是类方法,发给实例会被 objc2 的方法
/// 检查拦截崩掉(此前踩过的坑);必须用一参的实例方法 -standardWindowButton:。
/// 必须在窗口完成首次布局之后调用 —— 布局前移动会被 AppKit 重置;resize 也会重置,
/// 所以每次 show 和 resize 后都要重放(见 show_settings 与 resizeSubviewsWithOldSize:)。
/// 按钮为 nil 时静默跳过,旧版 macOS 同样适用。
///
/// Nudge the three traffic-light buttons down-right: grab the button views via the public
/// -standardWindowButton: and move their frames (no public API sets the traffic light position;
/// the old private setTrafficLightPosition: etc. are gone on macOS 26, and this is the only
/// reliable way -- verified on this machine). Note: the two-arg +standardWindowButton:forStyleMask:
/// is a CLASS method; sending it to an instance trips objc2's method check and panics (a pitfall
/// we hit) -- the one-arg instance method -standardWindowButton: must be used. Must run after the
/// window's first layout pass: moves before layout are reset by AppKit, and resize also resets
/// them, so the offset is re-applied on every show and resize (see show_settings and
/// resizeSubviewsWithOldSize:). Skips silently when a button is nil; works on older macOS too.
unsafe fn reposition_traffic_lights(window: *mut AnyObject) {
    // NSWindowButton: Close=0, Miniaturize=1, Zoom=2
    for tag in 0..3isize {
        let btn: *mut AnyObject = msg_send![window, standardWindowButton: tag];
        if btn.is_null() {
            continue;
        }
        let f: NSRect = msg_send![btn, frame];
        let _: () = msg_send![
            btn,
            setFrameOrigin: NSPoint::new(f.origin.x + TRAFFIC_LIGHT_DX, f.origin.y + TRAFFIC_LIGHT_DY)
        ];
    }
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
            // 红绿灯偏移:必须等窗口完成首次布局后再移动,否则会被 AppKit 重置。
            // Offset the traffic lights only after the window's first layout pass, or AppKit
            // resets them.
            let _: () = msg_send![u.window, layoutIfNeeded];
            reposition_traffic_lights(u.window);
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

/// 恢复默认设置:确认 -> 只把表单控件重填为代码默认值(不写盘)。
/// 用户点"确认(并重启)"时经 on_settings_ok 统一保存 + 应用;点"取消"则关窗,
/// 磁盘配置未被改动,天然撤销。
///
/// Restore defaults: confirm -> only repopulate the form controls with code defaults (no write).
/// The user then clicks OK (or OK && Restart) which saves + applies via on_settings_ok; clicking
/// Cancel just closes the window with the on-disk config untouched, i.e. a natural undo.
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
    // 构造默认配置,但保留 launch_at_login -- 它是系统级登录项开关,不属于外观/布局/
    // 快捷键这类设置,不该被恢复默认重置(否则会注销用户已勾选的登录项)。
    // 这里不写盘:仅用于预览填充;实际写盘在用户点 OK 后由 on_settings_ok 完成。
    // Build defaults, preserving launch_at_login -- it's a system-level login-item toggle, not
    // an appearance/layout/shortcut setting, so Restore Defaults must not reset it. Not saved
    // here: only used to preview-fill the form; the real save happens in on_settings_ok.
    let preserved_launch_at_login = CONFIG.read().unwrap().startup.launch_at_login;
    let mut defaults = Config::default();
    defaults.startup.launch_at_login = preserved_launch_at_login;

    // 鼠标页设备选择复位(有设备则回退第一个;无设备则清空),再重填表单。
    // Reset the mouse-page device selection (first device if any; clear if none), then refill.
    *SELECTED_DEVICE.lock().unwrap() = None;
    load_settings_from(&defaults);
    log_info!("Config form reset to defaults (not saved until OK).");
}

/// 用当前 CONFIG 填充设置控件(每次打开都刷新,反映外部编辑 + Reload)。
/// 重建设备下拉框(反映热插拔)。
///
/// Populate settings controls from current CONFIG (refreshed on each open). Rebuilds the device
/// popup to reflect hot-plug changes.
fn load_settings_values() {
    let cfg = CONFIG.read().unwrap().clone();
    load_settings_from(&cfg);
}

/// 用指定配置填充设置控件。
/// 供正常打开(读 CONFIG)与恢复默认预览(传 Config::default())共用。
///
/// Populate settings controls from a given config. Shared by normal open (reads CONFIG) and
/// restore-defaults preview (passes Config::default()).
fn load_settings_from(cfg: &Config) {
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
        // show_minimized:switch state(1=on / 0=off)。
        // show_minimized: switch state (1=on / 0=off).
        let sm_state = if cfg.windows.show_minimized {
            1isize
        } else {
            0isize
        };
        let _: () = msg_send![ui.show_minimized, setState: sm_state];
        // overlay_position:下拉框 index 0 = 跟随激活窗口(active_window), 1 = 主屏幕(main)。
        // overlay_position: popup index 0 = follow active window (active_window), 1 = main (main).
        let op_idx = match cfg.windows.overlay_position.as_str() {
            "main" => 1,
            _ => 0, // "active_window" (default)
        };
        let _: () = msg_send![ui.overlay_position, selectItemAtIndex: op_idx as isize];
        // log_level:下拉框 index 0..1 对应 debug,info;默认 index 1(info)。
        // log_level: popup index 0..1 = debug, info; default index 1 (info).
        let ll_idx = match cfg.logging.level.as_str() {
            "debug" => 0,
            _ => 1, // "info" (default)
        };
        let _: () = msg_send![ui.log_level, selectItemAtIndex: ll_idx as isize];
        // launch_at_login:按 CONFIG.startup.launch_at_login 设 switch 状态。
        // launch_at_login: set the switch state from CONFIG.startup.launch_at_login.
        let _: () = msg_send![ui.launch_at_login, setState: if cfg.startup.launch_at_login { 1isize } else { 0isize }];

        // ===== 鼠标页:按当前选中设备的有效配置(合并"所有鼠标"+该设备)填充控件 =====
        // Mouse page: populate controls from the effective config of the selected device
        // (merging "All Mice" + this device).
        // 先校准 SELECTED_DEVICE(基于当前设备列表;未初始化/被拔出时回退到第一个设备),
        // 再 resolve,保证显示的是实际生效设备的配置(修复首次打开显示错误档位的问题)。
        // Calibrate SELECTED_DEVICE first (against the current device list; falls back to the
        // first device when uninitialized/unplugged), then resolve, so the UI shows the actually
        // effective device's config (fixes the wrong-profile display on first open).
        ensure_selected_device();
        // 基于传入 cfg 解析选中设备的有效配置(恢复默认预览时 cfg = Config::default())。
        // Resolve the selected device's effective config from the given cfg (Config::default()
        // during the restore-defaults preview).
        let resolved = resolve_selected_from(cfg);
        // enable_mouse(总开关)始终读全局。
        // enable_mouse (master switch) always reads the global flag.
        let _: () =
            msg_send![ui.enable_mouse, setState: if cfg.mouse.enabled { 1isize } else { 0isize }];
        // 填充鼠标页设备相关控件(反转/加速/模式/行数/平滑预设)。
        // Fill the mouse page's per-device controls (reverse/accel/mode/line count/preset).
        fill_mouse_device_controls(ui, &resolved);

        // 重建设备下拉框(每次打开设置时刷新,反映热插拔)。
        // Rebuild the device popup (refreshed on each settings open to reflect hot-plug).
        rebuild_device_popup(ui);

        // 根据 enable_mouse 状态冻结/解冻下方控件。
        // Freeze/unfreeze the controls below based on the enable_mouse state.
        update_mouse_controls_enabled(ui);
        // 根据滚动模式刷新行数行的条件显隐。
        // Refresh the conditional visibility of the lines-per-tick row by mode.
        update_mode_dependent_visibility(ui);
    }
}

/// 填充鼠标页的 per-device 控件(反转/禁用加速/模式/行数/平滑预设)。
/// 供 load_settings_from 与 handle_device_changed 共用。
///
/// Fill the mouse page's per-device controls (reverse/disable-accel/mode/line-count/preset).
/// Shared by load_settings_from and handle_device_changed.
unsafe fn fill_mouse_device_controls(
    ui: &SettingsUi,
    resolved: &crate::mouse::resolve::ResolvedMouse,
) {
    // reverse_scroll:用有效值。
    // reverse_scroll: effective value.
    let _: () = msg_send![ui.reverse_scroll, setState: if resolved.reverse_scroll { 1isize } else { 0isize }];
    // disable_pointer_accel:用有效值。
    // disable_pointer_accel: effective value.
    let _: () = msg_send![ui.disable_pointer_accel, setState: if resolved.disable_acceleration { 1isize } else { 0isize }];
    // scroll_mode:用有效值。
    // scroll_mode: effective value.
    let sm_idx: isize = SCROLL_MODE_VALUES
        .iter()
        .position(|v| *v == resolved.scroll_mode.as_str())
        .map(|i| i as isize)
        .unwrap_or(0);
    let _: () = msg_send![ui.scroll_mode, selectItemAtIndex: sm_idx];
    // line_count:用有效值(Line 模式的行数滑块)。
    // line_count: effective value (Line mode's lines-per-notch slider).
    let _: () = msg_send![ui.line_count, setIntegerValue: resolved.line_count as isize];
    // 同步滑块右侧数值 label。
    // Sync the slider's value label.
    set_field(ui.line_count_value_label, resolved.line_count);
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
        // show_minimized:switch state(1=on / 0=off)。
        // show_minimized: switch state (1=on / 0=off).
        let sm_state: isize = msg_send![ui.show_minimized, state];
        cfg.windows.show_minimized = sm_state == 1;
        // overlay_position:下拉框 index 0 = 跟随激活窗口, 1 = 主屏幕。
        // overlay_position: popup index 0 = follow active window, 1 = main.
        let op_idx: isize = msg_send![ui.overlay_position, indexOfSelectedItem];
        cfg.windows.overlay_position = match op_idx {
            1 => "main",
            _ => "active_window", // index 0 or out-of-range
        }
        .into();
        // log_level:下拉框 index 0..1 对应 debug,info。
        // log_level: popup index 0..1 = debug, info.
        let ll_idx: isize = msg_send![ui.log_level, indexOfSelectedItem];
        cfg.logging.level = match ll_idx {
            0 => "debug",
            _ => "info", // index 1 or out-of-range
        }
        .into();
        // launch_at_login:switch state(1=on / 0=off)。
        // launch_at_login: switch state (1=on / 0=off).
        let la_state: isize = msg_send![ui.launch_at_login, state];
        cfg.startup.launch_at_login = la_state == 1;

        // ===== 鼠标页:把控件值写回当前选中设备的 profile =====
        // Mouse page: write control values back to the selected device's profile.
        // enable_mouse(总开关)始终写全局。
        // enable_mouse (master switch) always writes the global flag.
        let em_state: isize = msg_send![ui.enable_mouse, state];
        cfg.mouse.enabled = em_state == 1;

        // 其余字段写入选中设备的 profile(不存在则创建)。
        // The remaining fields go into the selected device's profile (creating it if absent).
        let dev = current_selected_device();
        let idx = find_profile_index(&cfg, dev);
        // 若 profile 不存在,新建一个并插入。
        // If the profile doesn't exist, create and insert one.
        if idx.is_none() {
            let new_p = MouseProfile {
                device: match dev {
                    Some((vid, pid)) => DeviceMatcher {
                        vendor_id: Some(vid),
                        product_id: Some(pid),
                    },
                    None => DeviceMatcher::default(),
                },
                ..Default::default()
            };
            cfg.mouse.profiles.push(new_p);
        }
        let idx = idx.unwrap_or(cfg.mouse.profiles.len() - 1);
        let p = &mut cfg.mouse.profiles[idx];

        // reverse_scroll:switch state(1=on / 0=off)。
        // reverse_scroll: switch state (1=on / 0=off).
        let ns_state: isize = msg_send![ui.reverse_scroll, state];
        p.reverse_scroll = Some(ns_state == 1);
        // disable_pointer_accel:switch state(1=on / 0=off)。
        // disable_pointer_accel: switch state (1=on / 0=off).
        let dpa_state: isize = msg_send![ui.disable_pointer_accel, state];
        p.pointer = Some(PartialPointerSection {
            disable_acceleration: Some(dpa_state == 1),
        });
        // scroll_mode:下拉框 index 对应 SCROLL_MODE_VALUES。
        // scroll_mode: popup index matches SCROLL_MODE_VALUES.
        let sm_idx: isize = msg_send![ui.scroll_mode, indexOfSelectedItem];
        p.scroll_mode = Some(
            SCROLL_MODE_VALUES
                .get(sm_idx as usize)
                .map(|s| s.to_string())
                .unwrap_or_else(|| "default".into()),
        );
        // line_count:滑块(整数,天然在 1..=10 内,无需 clamp/解析)。
        // 仅 Line 模式写回;Default 不写,保留已有值。
        // line_count: slider (integer, naturally within 1..=10; no clamp/parse needed).
        // Written only in Line mode; Default leaves it untouched.
        if p.scroll_mode.as_deref() == Some("line") {
            let lc_val: isize = msg_send![ui.line_count, integerValue];
            p.line_count = Some(lc_val.clamp(1, 10) as u32);
        }
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

// Cmd+Q 退出的常量:NSEventModifierFlagCommand = 1 << 20,ANSI Q 的 keyCode = 12。
// Constants for Cmd+Q handling: NSEventModifierFlagCommand = 1 << 20, ANSI Q keyCode = 12.
const NSEVENT_MODIFIER_FLAG_COMMAND: u64 = 1 << 20;
const KEYCODE_Q: u16 = 12;

/// 设置窗口的 performKeyEquivalent: 重写:key window 时拦截 Cmd+Q 退出 app。
/// 组合键(Cmd+...)的分发走 performKeyEquivalent: 链路(key window responder chain 先于
/// mainMenu)。设置窗口打开时 app 激活且窗口是 key window,Cmd+Q 必然到达这里——
/// 不依赖 mainMenu 分发(accessory app 的 mainMenu 对状态栏菜单不生效,这是之前的坑)。
/// 非 Cmd+Q 的组合键透传给 super,保证文本编辑等默认行为不受影响。
///
/// Override of performKeyEquivalent: on the settings window: intercept Cmd+Q while this
/// window is key. Command-combo dispatch goes through performKeyEquivalent: (key-window
/// responder chain before mainMenu). With the settings window open the app is active and the
/// window is key, so Cmd+Q is guaranteed to land here -- no reliance on mainMenu dispatch
/// (which doesn't work for status-bar menus on accessory apps, the earlier pitfall).
/// Other command combos fall through to super so text editing etc. keeps working.
extern "C" fn settings_window_perform_key_equivalent(
    _self: *mut c_void,
    _cmd: Sel,
    event: *mut AnyObject,
) -> bool {
    unsafe {
        let keycode: u16 = msg_send![event, keyCode];
        let flags: u64 = msg_send![event, modifierFlags];
        if keycode == KEYCODE_Q && (flags & NSEVENT_MODIFIER_FLAG_COMMAND) != 0 {
            // 与菜单 Quit 同路径:恢复指针加速 + terminate。sender 传 null 即可。
            // Same path as the menu Quit item: restore pointer acceleration + terminate.
            crate::menu::handle_quit(_self, sel!(handleQuit:), std::ptr::null_mut());
            return true;
        }
        let handled: bool = msg_send![
            super(_self as *mut AnyObject, objc2::runtime::AnyClass::get(c"NSWindow").unwrap()),
            performKeyEquivalent: event
        ];
        handled
    }
}

// 窗口 resize 会把红绿灯位置重置回默认(实测),重写 resizeSubviewsWithOldSize: 在
// super 布局之后重放偏移。位置重放是幂等的(每次设同样的 frame),不会引发布局循环。
// Resize resets the traffic lights to their default positions (verified), so override
// resizeSubviewsWithOldSize: to re-apply the offset after super's layout. The re-apply is
// idempotent (same frames each time) and cannot cause a layout loop.
extern "C" fn settings_window_resize_subviews(_self: *mut c_void, _cmd: Sel, old_size: NSSize) {
    unsafe {
        let _: () = msg_send![
            super(_self as *mut AnyObject, objc2::runtime::AnyClass::get(c"NSWindow").unwrap()),
            resizeSubviewsWithOldSize: old_size
        ];
        reposition_traffic_lights(_self as *mut AnyObject);
    }
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
            let types_key = CString::new("B@:@").unwrap(); // -performKeyEquivalent:(NSEvent*) -> BOOL
            class_addMethod(
                cls,
                sel!(performKeyEquivalent:),
                settings_window_perform_key_equivalent as *mut c_void,
                types_key.as_ptr(),
            );
            let types_resize = CString::new("v@:{CGSize=dd}").unwrap(); // -resizeSubviewsWithOldSize:(NSSize) -> void
            class_addMethod(
                cls,
                sel!(resizeSubviewsWithOldSize:),
                settings_window_resize_subviews as *mut c_void,
                types_resize.as_ptr(),
            );
            objc_registerClassPair(cls);
            SettingsWindowClass(cls)
        })
        .0
}

fn create_settings_window() {
    unsafe {
        // 窗口宽:玻璃卡片 195(260 的 75%)+ 10 间隙 + 内容 429 + 右缘 12 = 656。
        // Window width: 195 glass card (75% of 260) + 10pt gap + 429 content + 12pt right
        // margin = 656.
        let view_w = 656.0;
        let card_margin = 10.0;
        let card_w = 195.0;
        // 圆角分两档:窗口外框大圆角 26(unified 工具栏把主题帧圆角从 16 提到 26,见工具栏
        // 代码),侧边栏玻璃卡片圆角当前取 16(试调值;LinearMouse 原生是 8,窗口是 26)。
        // contentView 裁剪必须用 window_clip_radius(与窗口形状一致),否则 8~26 之间会露出
        // 透明角。
        // Corner radius comes in two tiers: the window frame is big (26, raised from 16 by the
        // unified toolbar -- see the toolbar code), while the sidebar glass card currently uses
        // 16 (a tuning value; LinearMouse's native proportions are 8 for the sidebar and 26 for
        // the window). The contentView clip must use window_clip_radius to match the window
        // shape, or the gap between the two radii would show through as transparent corners.
        let window_clip_radius = 26.0;
        let card_radius = 16.0;
        let style: u64 = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 3);
        // titled + closable + miniaturizable + resizable(三个红绿灯齐全)。resizable 是绿色 zoom
        // 按钮出现的必要条件;布局是绝对定位不随缩放,故下方用 min=max 固定窗口尺寸。
        // titled + closable + miniaturizable + resizable (all three traffic lights). resizable is
        // required for the green zoom button to appear; the layout is absolute-positioned and
        // doesn't adapt, so the window size is fixed below via min=max.
        // 窗口加左侧玻璃卡片后:宽 420 -> 580(旧侧边栏 150 + 1pt 分隔 + 内容 429),
        // 卡片加宽后 -> 721(卡片 260 + 10 间隙 + 内容 429 + 右缘 12),卡片回调后 -> 656
        // (卡片 195 + 10 间隙 + 内容 429 + 右缘 12)。
        // 内容拆成「通用 / 实验性」两页后,通用页(6 段 10 行)最高,高 768 -> 600 不够;
        // 通用页顶部预留 Accessibility 权限警告条,再加 60 -> 660;多出的窗口显示位置行再 +30 -> 690。
        // Content is now paged (General / Experimental); the General page (6 sections, 10 rows)
        // is the tallest, so 600 doesn't fit; +60 -> 660 reserves the Accessibility permission
        // warning banner at the top, and the overlay-position row adds +30 -> 690.
        // 初始位置:主显示器(screens[0])居中。不要用 NSScreen mainScreen(其语义是跟随
        // 键盘焦点窗口的屏幕,不是主屏,见 overlay_target_screen 的注释)。
        // Initial position: centered on the primary display (screens[0]). Don't use
        // NSScreen.mainScreen (it follows the key window, not the primary display; see
        // overlay_target_screen's note).
        let win_w = view_w;
        let win_h = 690.0;
        let (win_x, win_y) = {
            let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
            let count: usize = msg_send![screens, count];
            if count > 0 {
                // objectAtIndex: 的参数编码是 'q'(signed long),必须传 isize。
                // objectAtIndex: expects 'q' (signed long); pass isize.
                let s: *mut AnyObject = msg_send![screens, objectAtIndex: 0isize];
                let f: NSRect = msg_send![s, frame];
                (
                    f.origin.x + (f.size.width - win_w) / 2.0,
                    f.origin.y + (f.size.height - win_h) / 2.0,
                )
            } else {
                (220.0, 180.0)
            }
        };
        let frame = NSRect::new(NSPoint::new(win_x, win_y), NSSize::new(win_w, win_h));
        let window: *mut AnyObject = msg_send![settings_window_class(), alloc];
        let window: *mut AnyObject = msg_send![window, initWithContentRect: frame, styleMask: style, backing: 2u64, defer: false];
        let ns_title = make_nsstring(&t("settings.window_title"));
        let _: () = msg_send![window, setTitle: ns_title];
        CFRelease(ns_title as *const c_void);
        let _: () = msg_send![window, setReleasedWhenClosed: false];
        // 固定宽度:min/max 宽都等于设计宽度,高度可调 —— 系统设置同款(宽度不能左右调整)。
        // Fixed width: min and max width both equal the designed width, height stays adjustable --
        // same as System Settings (the width cannot be dragged).
        let _: () = msg_send![window, setMinSize: NSSize::new(view_w, 400.0)];
        let _: () = msg_send![window, setMaxSize: NSSize::new(view_w, 10000.0)];

        // 空 unified 工具栏:unified 工具栏(NSWindowToolbarStyleUnified=3)会把窗口主题帧
        // 圆角从 16 提到 26(实测;LinearMouse 的设置窗口就是这么做的),顶部条带随之变为
        // 玻璃材质条带、红绿灯在其中垂直居中。空工具栏不加入任何响应者,不影响
        // performKeyEquivalent:(Cmd+Q)与页面切换。必须在 contentLayoutRect 测量之前设置,
        // 布局高度会自动减去工具栏条带(658 -> 624)。
        // Empty unified toolbar: NSWindowToolbarStyleUnified (3) raises the theme frame's corner
        // radius from 16 to 26 (measured; that's how LinearMouse's settings window does it), and
        // the top strip becomes a glass material strip with the traffic lights centered in it.
        // An empty toolbar adds no responders, so performKeyEquivalent: (Cmd+Q) and page switching
        // are unaffected. Must be set before measuring contentLayoutRect, which then automatically
        // accounts for the toolbar strip (658 -> 624).
        let tb: *mut AnyObject = msg_send![class!(NSToolbar), alloc];
        let tb_id = make_nsstring("OhMyTabSettingsToolbar");
        let tb: *mut AnyObject = msg_send![tb, initWithIdentifier: tb_id];
        CFRelease(tb_id as *const c_void);
        let _: () = msg_send![window, setToolbar: tb];
        let _: () = msg_send![window, setToolbarStyle: 3isize]; // NSWindowToolbarStyleUnified
        release_obj(tb);

        let content: *mut AnyObject = msg_send![window, contentView];
        // content_h 只用于容器视图的满高尺寸(翻转 mask 后覆盖整个窗口);
        // 顶部锚定行的有效高度在翻转后用 layout_h 取(见下)。
        // content_h is only used for full-height containers (they cover the whole window after
        // the mask flip); top-anchored rows use layout_h measured after the flip (see below).
        let content_frame: NSRect = msg_send![content, frame];
        let content_h = content_frame.size.height;

        // 去掉红绿灯下方的标题栏分隔线:切到 fullSizeContentView + 透明标题栏后,
        // 内容区延伸到标题栏,AppKit 不再绘制那条 hairline;隐藏标题文字(系统设置同款观感)。
        // 注意:翻转 mask 前 contentView 可能尚未排版(未显示时返回全窗高度,实测 macOS 26
        // 上就是 690),所以不能在翻转前量有效高度。翻转后用 contentLayoutRect 量
        // 「红绿灯条带以下的内容可用区」(macOS 11+,部署目标 11.0 直接可用;min 兜底)。
        // 顶部锚定的行全部以 layout_h 定位,避免内容顶进红绿灯条带。
        // Remove the hairline under the traffic lights: with fullSizeContentView + a transparent
        // title bar the content extends into the title bar and AppKit stops drawing the separator;
        // the title text is hidden (System Settings look). The contentView must NOT be measured
        // before the flip -- an unlaid-out window reports the full height (690 on macOS 26 in
        // practice). After the flip, contentLayoutRect (macOS 11+; min target is 11.0) gives the
        // real layout area below the traffic-light strip; .min() guards a degenerate result.
        // All top-anchored rows are laid out against layout_h so nothing collides with the lights.
        let _: () = msg_send![window, setTitlebarAppearsTransparent: true];
        let _: () = msg_send![window, setStyleMask: style | (1 << 15)]; // NSWindowStyleMaskFullSizeContentView
        let _: () = msg_send![window, setTitleVisibility: 1isize]; // NSWindowTitleHidden
        let layout_rect: NSRect = msg_send![window, contentLayoutRect];
        let layout_h = layout_rect.size.height.min(content_h);

        // 窗口圆角:窗口自绘的 opaque 背景(系统默认小圆角)会盖住 contentView 的裁剪,
        // 单靠 layer cornerRadius 圆角出不来。做法:setOpaque:NO 关掉窗口自绘背景,由
        // contentView 的 layer 自己铺 windowBackgroundColor(深浅色语义色)并裁成
        // window_clip_radius(26,与窗口形状一致)圆角。主题切换会 invalidate 重建窗口,
        // 背景色随重建重取,不会在深浅色切换后过时。红绿灯是窗口 chrome、不在
        // contentView 内,不受裁剪;卡片 10pt 留白与版本号也都在圆角区之外。
        // Window corners: the window's own opaque background (system-default small rounding)
        // paints over contentView's clipping, so layer cornerRadius alone didn't round the window.
        // Fix: setOpaque:NO turns off the window-drawn background, and contentView's layer paints
        // windowBackgroundColor itself (a semantic color) clipped to window_clip_radius (26,
        // matching the window shape). Theme switches invalidate and rebuild the window, so the
        // color is re-captured and never goes stale across light/dark changes. The traffic lights
        // are window chrome outside contentView (not clipped); the card's 10pt margin and the
        // version label stay clear of the corner zone.
        let _: () = msg_send![window, setOpaque: false];
        let _: () = msg_send![content, setWantsLayer: true];
        let cv_layer: *mut AnyObject = msg_send![content, layer];
        if !cv_layer.is_null() {
            let bg_ns: *mut AnyObject = msg_send![class!(NSColor), windowBackgroundColor];
            layer_set_background(cv_layer, ns_color_to_cg(bg_ns));
            let _: () = msg_send![cv_layer, setCornerRadius: window_clip_radius];
            let _: () = msg_send![cv_layer, setMasksToBounds: true];
        }

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
            overlay_position: std::ptr::null_mut(),
            log_level: std::ptr::null_mut(),
            launch_at_login: std::ptr::null_mut(),
            reverse_scroll: std::ptr::null_mut(),
            enable_mouse: std::ptr::null_mut(),
            scroll_mode: std::ptr::null_mut(),
            line_count: std::ptr::null_mut(),
            line_count_label: std::ptr::null_mut(),
            line_count_value_label: std::ptr::null_mut(),
            disable_pointer_accel: std::ptr::null_mut(),
            device_indicator: std::ptr::null_mut(),
            ok_button: std::ptr::null_mut(),
            accessibility_warning_view: std::ptr::null_mut(),
        };

        // 内容区 x(卡片右缘 + 10pt 间隙)、宽 / content area x (card right edge + 10pt gap) and width
        let content_x = card_margin + card_w + card_margin; // 10 + 260 + 10 = 280
        let content_w = view_w - content_x - 12.0; // 721 - 280 - 12 = 429
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

        // --- 侧边栏 sidebar(悬浮玻璃卡片,系统设置同款观感)---
        // macOS 26+ 用 NSGlassEffectView(Liquid Glass,不设 tint 用系统默认);
        // 旧版用 NSVisualEffectView + sidebar 材质(经典磨砂侧边栏)。
        // 卡片宽 195、四周统一 10pt 留白(顶部与底部一致,不顶到窗口边框)、四角圆角 16
        // (当前试调值,见 card_radius 常量注释)。
        // 玻璃材质自带视觉边界,原来的 1pt 分隔线随之删除。
        // --- Sidebar: a floating glass card, same look as System Settings ---
        // macOS 26+ uses NSGlassEffectView (Liquid Glass, system default tint);
        // older macOS uses NSVisualEffectView with the sidebar material (classic frosted look).
        // The card is 195 wide with a uniform 10pt margin all around (top matches the bottom,
        // not flush with the window frame) and a 16pt corner radius (current tuning value; see
        // the card_radius constant's comment). The glass material carries its own edge, so the
        // old 1pt divider is removed.
        let card_h = content_h - card_margin * 2.0;
        let sidebar_view: *mut AnyObject = if AnyClass::get(c"NSGlassEffectView").is_some() {
            let cls = AnyClass::get(c"NSGlassEffectView").unwrap();
            let g: *mut AnyObject = msg_send![cls, alloc];
            let g: *mut AnyObject = msg_send![g, initWithFrame: NSRect::new(NSPoint::new(card_margin, card_margin), NSSize::new(card_w, card_h))];
            let _: () = msg_send![g, setStyle: 0i64]; // NSGlassEffectViewStyleRegular
            let _: () = msg_send![g, setCornerRadius: card_radius];
            // cornerRadius 属性只圆了着色/外观,背景模糊需 layer masksToBounds 一并裁剪
            // (与 main.rs 的 overlay 同款做法)。
            // The cornerRadius property only rounds the tint/appearance; the layer must also
            // clip the backdrop blur via masksToBounds (same trick as the overlay in main.rs).
            let _: () = msg_send![g, setWantsLayer: true];
            let g_layer: *mut AnyObject = msg_send![g, layer];
            if !g_layer.is_null() {
                let _: () = msg_send![g_layer, setCornerRadius: card_radius];
                let _: () = msg_send![g_layer, setMasksToBounds: true];
            }
            g
        } else {
            let ve: *mut AnyObject = msg_send![class!(NSVisualEffectView), alloc];
            let ve: *mut AnyObject = msg_send![ve, initWithFrame: NSRect::new(NSPoint::new(card_margin, card_margin), NSSize::new(card_w, card_h))];
            let _: () = msg_send![ve, setMaterial: 8u64]; // NSVisualEffectMaterialSidebar
            let _: () = msg_send![ve, setBlendingMode: 0u64]; // BehindWindow
            let _: () = msg_send![ve, setState: 1u64]; // Active
            let _: () = msg_send![ve, setWantsLayer: true];
            let ve_layer: *mut AnyObject = msg_send![ve, layer];
            if !ve_layer.is_null() {
                let _: () = msg_send![ve_layer, setCornerRadius: card_radius];
                let _: () = msg_send![ve_layer, setMasksToBounds: true];
            }
            ve
        };
        // 自适应:左侧锚定、高度随窗口拉伸(HeightSizable|MaxXMargin = 16|4 = 20)。
        // Adaptive: left-anchored, height stretches with the window.
        let _: () = msg_send![sidebar_view, setAutoresizingMask: 20u64];
        let _: () = msg_send![content, addSubview: sidebar_view];
        release_obj(sidebar_view);

        // 侧边栏选中行的高亮背景(layer-backed NSView,theme 感知色),先于按钮加入以便按钮文字叠在上层。
        // Highlight background for the selected sidebar row (layer-backed NSView, theme-aware color);
        // added before the buttons so button titles draw on top of it.
        // 卡片内布局:内边距 12;按钮顶边放在 layout_h(条带底)下方 6pt,靠近红绿灯
        // (btn_y0 为卡片坐标系)。
        // Card-local layout: 12pt inner margins. The buttons' top edge sits 6pt below layout_h
        // (the strip's bottom) so they stay close to the traffic lights. btn_y0 is in card coords.
        let btn_w = card_w - 24.0;
        let btn_h = 28.0;
        let btn_y0 = layout_h - card_margin - 6.0 - btn_h;
        let highlight: *mut AnyObject = msg_send![class!(NSView), alloc];
        let highlight: *mut AnyObject = msg_send![highlight, initWithFrame: NSRect::new(NSPoint::new(12.0, btn_y0), NSSize::new(btn_w, btn_h))];
        let _: () = msg_send![highlight, setAutoresizingMask: 12u64]; // 贴顶、贴左 / top- and left-anchored
        let _: () = msg_send![highlight, setWantsLayer: true];
        let hl_layer: *mut AnyObject = msg_send![highlight, layer];
        let _: () = msg_send![hl_layer, setCornerRadius: 6.0f64];
        // 选中高亮用系统强调色(controlAccentColor),与 NSSwitch 开启的蓝色一致
        // (LinearMouse 侧边栏选中高亮同款)。
        // Selection highlight uses the system accent color (controlAccentColor), matching the
        // NSSwitch's on-state blue (same as LinearMouse's sidebar selection highlight).
        let sel_color: *mut AnyObject = msg_send![class!(NSColor), controlAccentColor];
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
        let mut y = layout_h - 12.0; // 顶部光标:下一个元素的底边 y / top cursor (bottom y of next element)

        // --- Accessibility 权限警告条(通用页顶部覆盖;仅缺权限时显示,show_settings 里按 setHidden 切换) ---
        // --- Accessibility permission warning banner (floats at the top of General; shown only
        //  when permission is missing, toggled via setHidden in show_settings) ---
        // banner 不占用布局空间(通用页内容紧贴顶部),而是在内容构建完后作为最后一个
        // subview 添加,覆盖在顶部。frame 固定定位,不随 y 布局游标变化。
        // The banner does not reserve layout space (General content starts at the top); it is
        // added as the last subview after the content, floating over the top. Its frame is fixed
        // and independent of the y layout cursor.
        let banner_h = 48.0;
        let banner: *mut AnyObject = msg_send![class!(NSView), alloc];
        let banner: *mut AnyObject = msg_send![
            banner,
            initWithFrame: NSRect::new(
                NSPoint::new(0.0, layout_h - 12.0 - banner_h),
                NSSize::new(content_w, banner_h)
            )
        ];
        // 自适应:宽度拉伸、顶部锚定(WidthSizable|MinYMargin = 10)。
        // 注意:这里不 addSubview;在通用页内容构建完后统一添加(保证在最上层)。
        // Note: not added here; added after the General content build so it stays on top.
        let _: () = msg_send![banner, setAutoresizingMask: 10u64];
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

        // --- 外观 Appearance ---
        y -= 12.0;
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
        // show_minimized 开关(切换器语义本就只有显/隐两态,用 Toggle 比下拉更直观)。
        // 英文标签 "Show minimized windows on switch"(215pt)超出默认 label_w=150,
        // 该行标签加宽到 225;开关与所有开关行一样右对齐到 popup 右缘(ctrl_x + ctrl_w)。
        // show_minimized as a switch (the option is inherently two-state, so a toggle is more
        // intuitive than a popup). The English label (215pt) exceeds the default label_w=150,
        // so this row widens its label to 225; the switch right-aligns to the popups' right
        // edge (ctrl_x + ctrl_w), like every other switch row.
        ui.show_minimized = add_row(
            general_view,
            label_x,
            y,
            225.0,
            row_h,
            &t("settings.row_show_minimized"),
            make_switch(ctrl_x + ctrl_w, y, row_h, false),
        );
        y -= 8.0 + row_h;
        // overlay_position 下拉框:项 = [跟随激活窗口, 始终显示在主屏幕];默认 index 0(跟随激活窗口)。
        // overlay_position popup: items = [Follow Active Window, Always on Main Screen]; default index 0.
        let op_labels = [
            t("settings.overlay_position_follow_active"),
            t("settings.overlay_position_main_screen"),
        ];
        let op_label_refs: Vec<&str> = op_labels.iter().map(|s| s.as_str()).collect();
        ui.overlay_position = add_row(
            general_view,
            label_x,
            y,
            label_w,
            row_h,
            &t("settings.row_overlay_position"),
            make_popup(ctrl_x, y, ctrl_w, row_h, &op_label_refs, 0),
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
        // 日志级别下拉框:项 = [debug, info];默认 index 1(info)。
        // Log level popup: items = [debug, info]; default index 1 (info).
        let log_levels: [&str; 2] = ["debug", "info"];
        ui.log_level = add_row(
            general_view,
            label_x,
            y,
            label_w,
            row_h,
            &t("settings.row_log_level"),
            make_popup(ctrl_x, y, ctrl_w, row_h, &log_levels, 1),
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
        // 开机自启开关:标题留空(左侧 row label 已说明),仅放一个 switch。
        // Launch-at-login switch: no title (the row label on the left already describes it).
        ui.launch_at_login = add_row(
            general_view,
            label_x,
            y,
            label_w,
            row_h,
            &t("settings.row_launch_at_login"),
            make_switch(ctrl_x + ctrl_w, y, row_h, false),
        );

        // ===== 实验性页内容 experimental page content =====
        let mut y = layout_h - 12.0;

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
        let mut y = layout_h - 12.0;

        // --- 启用鼠标控制(总开关,置于最顶) / Enable mouse control (topmost) ---
        y -= 8.0 + row_h;
        ui.enable_mouse = add_row(
            mouse_view,
            label_x,
            y,
            label_w,
            row_h,
            &t("settings.row_enable_mouse"),
            make_switch(ctrl_x + ctrl_w, y, row_h, false),
        );
        // switch toggle 时实时更新 OK 按钮标题(确认 vs 确认并重启)。
        // Update OK button title in real time when the switch toggles (OK vs OK && Restart).
        let _: () = msg_send![ui.enable_mouse, setTarget: target];
        let _: () = msg_send![ui.enable_mouse, setAction: sel!(handleEnableMouseToggle:)];

        // --- 设备选择器(内嵌下拉框,切换即时刷新其余控件) / Device picker (inline popup) ---
        y -= 14.0 + 24.0;
        add_header(
            mouse_view,
            &t("settings.header_mouse_device"),
            12.0,
            y,
            content_w - 24.0,
        );
        y -= 8.0 + row_h;
        // 下拉框:items 在 load_settings_values 里动态重建(设备列表可变)。
        // 首次创建放一个占位项,真正的内容在 load_settings_values -> rebuild_device_popup 填入。
        // Popup: items are rebuilt dynamically in load_settings_values (device list is mutable).
        // A placeholder is inserted here; the real items are filled by rebuild_device_popup.
        let dev_popup = make_popup(ctrl_x, y, ctrl_w, row_h, &[""], 0);
        // 绑定 target/action:选择变化时即时刷新其余控件为该设备的有效值。
        // Bind target/action: on selection change, immediately refresh the other controls with
        // the selected device's effective values.
        let _: () = msg_send![dev_popup, setTarget: target];
        let _: () = msg_send![dev_popup, setAction: sel!(handleDeviceChanged:)];
        ui.device_indicator = add_row(
            mouse_view,
            label_x,
            y,
            label_w,
            row_h,
            &t("settings.header_mouse_device"),
            dev_popup,
        );

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
        // 滚动模式切换时即时刷新"行数"行的条件显隐。
        // Refresh the conditional visibility of the "lines per tick" row on mode switch.
        let _: () = msg_send![ui.scroll_mode, setTarget: target];
        let _: () = msg_send![ui.scroll_mode, setAction: sel!(handleScrollModeChanged:)];

        // --- 行数(按行模式) / Line count (line mode) ---
        y -= 8.0 + row_h;
        let (line_label, line_ctrl) = add_row_with_label(
            mouse_view,
            label_x,
            y,
            label_w,
            row_h,
            &t("settings.row_line_count"),
            // 整数滑块 1..=10(与 config 校验一致;对齐 LinearMouse By Lines 的滑块交互)。
            // 右侧留 ~40pt 放只读数值 label 显示当前值。
            // Integer slider 1..=10 (matches config validation; mirrors LinearMouse's
            // By Lines slider interaction). ~40pt on the right holds a read-only value label.
            make_slider(ctrl_x, y, ctrl_w - 40.0, row_h, 1, 10, 3),
        );
        ui.line_count = line_ctrl;
        ui.line_count_label = line_label;
        // 滑块右侧的只读数值 label:显示当前行数,拖动滑块时实时刷新。
        // Read-only value label right of the slider: shows the current line count, refreshed
        // live as the slider moves.
        let value_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let value_label: *mut AnyObject = msg_send![value_label, initWithFrame: NSRect::new(NSPoint::new(ctrl_x + ctrl_w - 34.0, y), NSSize::new(30.0, row_h))];
        set_field(value_label, 3);
        let _: () = msg_send![value_label, setBezeled: false];
        let _: () = msg_send![value_label, setDrawsBackground: false];
        let _: () = msg_send![value_label, setEditable: false];
        let _: () = msg_send![value_label, setAlignment: 1isize]; // NSTextAlignmentRight
        let _: () = msg_send![mouse_view, addSubview: value_label];
        release_obj(value_label);
        ui.line_count_value_label = value_label;
        // 滑块拖动时实时刷新数值 label。
        // Refresh the value label live as the slider is dragged.
        let _: () = msg_send![ui.line_count, setTarget: target];
        let _: () = msg_send![ui.line_count, setAction: sel!(handleLineCountChanged:)];

        // --- 滚动 Scrolling ---
        y -= 14.0 + 24.0;
        add_header(
            mouse_view,
            &t("settings.header_mouse_scrolling"),
            12.0,
            y,
            content_w - 24.0,
        );
        y -= 8.0 + row_h;
        // reverse_scroll 开关:标题留空(左侧 row label 已说明),仅放一个 switch。
        // reverse_scroll switch: no title (the row label on the left already describes it).
        ui.reverse_scroll = add_row(
            mouse_view,
            label_x,
            y,
            label_w,
            row_h,
            &t("settings.row_reverse_scroll"),
            make_switch(ctrl_x + ctrl_w, y, row_h, false),
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
        // disable_pointer_accel 开关:禁用系统鼠标加速,光标 1:1 线性跟踪。
        // 英文标签 "Disable pointer acceleration (linear tracking)"(269pt)超出默认
        // label_w=150 会被截断;该行标签加宽到 285。开关与所有开关行一样右对齐到
        // popup 右缘(ctrl_x + ctrl_w)。
        // disable_pointer_accel switch: disable system pointer acceleration for 1:1 linear
        // cursor tracking. The English label (269pt) exceeds the default label_w=150 and would
        // truncate; this row widens its label to 285. The switch right-aligns to the popups'
        // right edge (ctrl_x + ctrl_w), like every other switch row.
        ui.disable_pointer_accel = add_row(
            mouse_view,
            label_x,
            y,
            285.0,
            row_h,
            &t("settings.row_disable_pointer_accel"),
            make_switch(ctrl_x + ctrl_w, y, row_h, false),
        );

        // banner 最后添加:作为 general_view 的最后一个 subview,保证在内容之上(缺权限时覆盖顶部)。
        // Added last: as general_view's final subview so it floats above the content (when
        // permission is missing). It occupies no layout space, so no top gap when hidden.
        let _: () = msg_send![general_view, addSubview: banner];
        release_obj(banner);

        // --- 确认 / 取消(加在 contentView 上,三页都可见)---
        // Restore Defaults 按钮(玻璃卡片底部内侧,版本号在其下方),与 OK/Cancel 同在
        // contentView,两页都可见。宽度限制在卡片内(x=12..138,卡片右缘 x=205)。
        // Restore Defaults button (inside the glass card's bottom, version label below it),
        // on contentView like OK/Cancel, visible on both pages. Width kept within the card
        // (x=12..138, card right edge x=205).
        let restore: *mut AnyObject = msg_send![class!(NSButton), alloc];
        let restore: *mut AnyObject = msg_send![restore, initWithFrame: NSRect::new(NSPoint::new(12.0, 44.0), NSSize::new(126.0, 28.0))];
        let _: () = msg_send![restore, setAutoresizingMask: 36u64]; // 贴底、贴左 / bottom- and left-anchored
        set_control_title(restore, &t("settings.btn_restore_defaults"));
        let _: () = msg_send![restore, setBezelStyle: 1isize];
        let _: () = msg_send![restore, setTarget: target];
        let _: () = msg_send![restore, setAction: sel!(handleRestoreDefaults:)];
        let _: () = msg_send![content, addSubview: restore];
        release_obj(restore);

        // 版本号(Restore Defaults 下方,玻璃卡片底部内侧)。
        // x=20 避开卡片左下圆角区(卡片 x=10、圆角 12)。
        // Version label (below Restore Defaults, inside the glass card's bottom).
        // x=20 clears the card's bottom-left rounded corner (card x=10, radius 12).
        let version_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let version_label: *mut AnyObject = msg_send![version_label, initWithFrame: NSRect::new(NSPoint::new(20.0, 14.0), NSSize::new(126.0, 20.0))];
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
