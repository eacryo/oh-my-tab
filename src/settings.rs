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
use std::collections::HashMap;
use std::ffi::{c_void, CString};
use std::sync::atomic::Ordering;
use std::sync::{LazyLock, Mutex, OnceLock};

use crate::config::{reload_config, Config, CONFIG};
use crate::event_monitor::SHORTCUT_IS_CMD;
use crate::event_tap::{
    CGEventGetFlags, CGEventGetIntegerValueField, CGEventRef, CGEventTapProxy, CGEventType,
};
use crate::ffi::*;
use crate::i18n::{t, tf};
use crate::menu::{refresh_menu_titles, set_shortcut_mode};
use crate::mouse::shortcut::{button_name, describe_shortcut, display_shortcut};
use crate::overlay::{apply_theme, refresh_highlight, update_status_label};
use crate::{log_debug, log_info};
// 跨模块共享状态(由 main.rs 持有)/ cross-module shared state (owned by main.rs)
use crate::MENU_TARGET;

// locale 下拉项:显示用各语言原生写法(语言选择器的通用约定),值对应 config.i18n.locale。
// Locale popup items: displayed in each language's own script (convention for language pickers);
// values map to config.i18n.locale.
const LOCALE_LABELS: [&str; 4] = ["Auto", "English", "简体中文", "繁體中文"];
const SCROLL_MODE_LABELS: [&str; 2] = ["Default", "Line"];
const SCROLL_MODE_VALUES: [&str; 2] = ["default", "line"];
const LOCALE_VALUES: [&str; 4] = ["auto", "en", "zh-Hans", "zh-Hant"];

// ========== 按键映射录制状态 / button-mapping recording state ==========

/// 录制阶段。
/// Recording stage.
#[derive(PartialEq, Clone, Copy, Debug)]
enum RecStage {
    Idle,
    WaitingButton,
    WaitingCombo,
}

/// 录制阶段(主线程读写,录制线程经 performSelectorOnMainThread 推进)。
/// Recording stage (read/written on the main thread; the recording thread advances it via
/// performSelectorOnMainThread).
static REC_STAGE: Mutex<RecStage> = Mutex::new(RecStage::Idle);
/// 录制到的按钮号(mouseEventButtonNumber,>= 2)。
/// The button number captured while recording (mouseEventButtonNumber, >= 2).
static REC_BUTTON: Mutex<u32> = Mutex::new(0);
/// 录制到的快捷键描述(完成时由录制线程写入,主线程回调读取)。
/// The shortcut description captured while recording (written by the recording thread on
/// completion, read by the main-thread callback).
static REC_DESC: Mutex<String> = Mutex::new(String::new());
/// 当前选中设备的编辑态映射(未点 OK 前的内存缓存;设备切换时从配置重建)。
/// The selected device's in-edit mappings (in-memory until OK; rebuilt from config when the
/// device changes).
static MAPPING_EDITS: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// 录制 tap 线程句柄(短命,重复录制时替换)。
/// The recording tap thread handle (short-lived; replaced on each recording session).
static RECORD_THREAD: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);
/// 录制线程的 RunLoop 引用(完成/取消时 CFRunLoopStop)。
/// 包装 Send:static 里的裸指针需要 Send+Sync(与 mouse/event_tap.rs 的 RunLoopMutex 同模式)。
/// The recording thread's RunLoop (stopped on completion/cancel).
/// Send wrapper: raw pointers in statics need Send+Sync (same pattern as RunLoopMutex in
/// mouse/event_tap.rs).
struct RecRunLoopMutex(Mutex<Option<crate::event_tap::CFRunLoopRef>>);
unsafe impl Send for RecRunLoopMutex {}
unsafe impl Sync for RecRunLoopMutex {}
static REC_RUNLOOP: LazyLock<RecRunLoopMutex> = LazyLock::new(|| RecRunLoopMutex(Mutex::new(None)));

/// 录制中实时累积的修饰键(WaitingCombo 阶段,flagsChanged 时更新)。
/// Modifiers accumulated live while recording (updated on flagsChanged during
/// waiting-for-combo).
static REC_MODS: Mutex<u32> = Mutex::new(0);
/// 录制模式:面板里录侧键(触发条件)或录组合键(Key Press 动作)。
/// Recording mode: the panel records the side button (trigger) or the combo (Key Press).
#[derive(PartialEq, Clone, Copy, Debug)]
enum RecMode {
    PanelTrigger,
    PanelCombo,
}

/// 当前录制模式(主线程读写;finish 后由 handle_recording_finished 按模式收尾)。
/// The current recording mode (main-thread; handle_recording_finished finishes per mode).
static REC_MODE: Mutex<RecMode> = Mutex::new(RecMode::PanelTrigger);

// ========== 映射编辑面板 / mapping edit panel ==========

/// 正在编辑的按钮号(面板打开期间;新增时为 None,录制侧键后确定)。
/// The button number being edited (while the panel is open; None for a new mapping until
/// the side button is recorded).
static EDIT_BUTTON: Mutex<Option<u32>> = Mutex::new(None);
/// 面板里的动作类型下拉选中 index。
/// The panel's action-type popup selection.
static EDIT_ACTION_IDX: Mutex<isize> = Mutex::new(0);
/// 面板里录好的组合键描述(Key Press 动作;空 = 未录)。
/// The combo recorded in the panel (Key Press action; empty = not recorded).
static EDIT_COMBO: Mutex<String> = Mutex::new(String::new());
/// 面板窗口与控件。
/// The panel window and its controls.
static EDIT_PANEL: Mutex<Option<ObjPtr>> = Mutex::new(None);
static EDIT_PANEL_BTN_LABEL: Mutex<Option<ObjPtr>> = Mutex::new(None);
static EDIT_PANEL_ACTION: Mutex<Option<ObjPtr>> = Mutex::new(None);
static EDIT_PANEL_COMBO_BTN: Mutex<Option<ObjPtr>> = Mutex::new(None);
static EDIT_PANEL_COMBO_LABEL: Mutex<Option<ObjPtr>> = Mutex::new(None);
static EDIT_PANEL_OK: Mutex<Option<ObjPtr>> = Mutex::new(None);
/// 面板打开时的窗口遮罩(半透明灰层,modal 调暗设置窗口)。
/// The window dim layer while the panel is open (a translucent gray overlay that dims the
/// settings window, modal-style).
static EDIT_DIM: Mutex<Option<ObjPtr>> = Mutex::new(None);

/// 录制取消标志:取消时置位。既是 tap 创建重试的提前退出信号(录制线程可能还卡在
/// 缺权限的重试 sleep 里,此时 CFRunLoopStop 无效 —— 没有这个标志 tap 会常驻吞键),
/// 也供回调在 Idle 后防御性透传。
/// Recording-cancel flag: set on cancel. It bails the tap-creation retry loop early (the
/// recording thread may still be sleeping through permission-retries, where CFRunLoopStop
/// is a no-op -- without this flag the tap would linger and keep swallowing keys) and lets
/// the callback defensively pass everything through once idle.
static REC_CANCEL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 录制 tap 的 CFMachPort 引用:取消/完成时立即 CGEventTapEnable(false),不等 runloop
/// 退出(CFRunLoopStop 是异步的,退出前 tap 仍在分发事件,可能吞掉取消后的首个按键)。
/// The recording tap's CFMachPort: disabled immediately on cancel/finish via
/// CGEventTapEnable(false) -- CFRunLoopStop is asynchronous and the tap keeps dispatching
/// until the loop actually exits, which could swallow the first keystroke after cancel.
struct RecTapMutex(Mutex<Option<crate::event_tap::CFMachPortRef>>);
unsafe impl Send for RecTapMutex {}
unsafe impl Sync for RecTapMutex {}
static REC_TAP: LazyLock<RecTapMutex> = LazyLock::new(|| RecTapMutex(Mutex::new(None)));

// ========== 设置窗口状态 / settings window state ==========

// 设置窗口的控件指针集合（非模态窗口，复用，隐藏而非销毁）。
// Holds pointers to the settings window's controls (non-modal, reused, hidden not destroyed).
struct SettingsUi {
    window: *mut AnyObject,
    sidebar_general: *mut AnyObject, // NSButton: 通用 / General (tag=0)
    sidebar_experimental: *mut AnyObject, // NSButton: 实验性功能 / Experimental (tag=1)
    sidebar_mouse: *mut AnyObject,   // NSButton: 鼠标控制 / Mouse (tag=2)
    sidebar_clipboard: *mut AnyObject, // NSButton: 剪贴板历史 / Clipboard history (tag=3)
    sidebar_highlight: *mut AnyObject, // NSView: 选中行高亮背景 (layer-backed)
    general_view: *mut AnyObject,    // NSView: 通用页容器 / General page container
    experimental_view: *mut AnyObject, // NSView: 实验性页容器 / Experimental page container
    mouse_view: *mut AnyObject,      // NSView: 鼠标页容器 / Mouse page container
    clipboard_view: *mut AnyObject,  // NSView: 剪贴板历史页容器 / Clipboard page container
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
    windows_enabled: *mut AnyObject,  // NSSwitch: 窗口切换总开关 / app-switcher master switch
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
    // ---- 按键映射区 / button-mappings section ----
    mapping_scroll: *mut AnyObject, // NSScrollView: 绑定列表滚动容器 / the bindings scroll view
    mapping_doc: *mut AnyObject,    // NSView: 滚动容器里的 document view(行堆叠处)/ document view
    mapping_rows: Vec<MappingRow>,  // 动态绑定行(标签 + 删除按钮)/ live binding rows
    clipboard_enabled: *mut AnyObject, // NSSwitch: 启用剪贴板历史 / enable clipboard history
    clipboard_persist: *mut AnyObject, // NSSwitch: 保存剪贴板历史记录到磁盘 / persist clipboard history
    clipboard_move_used_to_top: *mut AnyObject, // NSSwitch: 使用后移到最前 / move used entries to top
    clipboard_max_entries: *mut AnyObject,      // NSTextField: 历史最大条数 / max history entries
    clipboard_auto_expire_days: *mut AnyObject, // NSTextField: 自动过期天数(0=关闭)/ auto-expire days (0 = off)
    clipboard_show_source_app: *mut AnyObject,  // NSSwitch: 显示来源应用 / show the source app
    clipboard_picker_position: *mut AnyObject, // NSPopUpButton: 跟随鼠标 / 主屏幕居中 / picker position
    // (follow mouse / centered on the main screen)
    add_mapping_button: *mut AnyObject, // NSButton: 添加映射 / add-mapping button
    mapping_enabled: *mut AnyObject, // NSSwitch: 按键映射总开关(per-device) / mappings master switch (per-device)
    mapping_empty: *mut AnyObject,   // NSTextField: 空状态提示(卡片内) / empty-state hint (in-card)
    device_indicator: *mut AnyObject, // NSButton: 当前选中设备指示器(点击打开选择器) / device indicator (opens picker)
    ok_button: *mut AnyObject,        // NSButton: 确认按钮 / OK button
    accessibility_warning_view: *mut AnyObject, // NSView: 缺权限警告条容器 / permission-warning banner container
}
unsafe impl Send for SettingsUi {}

/// 一行按键映射(只读显示):
/// - label:按钮名(只读)
/// - desc_label:动作描述(系统动作名/None 文本;Key Press 时用键帽胶囊)
/// - edit:编辑按钮(tag = 按钮号,点击打开编辑面板)
/// - delete:删除按钮(tag = 按钮号)
/// - caps:键帽胶囊(Key Press 时显示)
///
/// One button-mapping row (read-only display):
/// - label: the button name
/// - desc_label: the action description (system-action name / None text; keycaps for Key
///   Press)
/// - edit: the edit button (tag = button number; opens the edit panel)
/// - delete: the delete button (tag = button number)
/// - caps: keycap pills (shown for Key Press)
struct MappingRow {
    label: *mut AnyObject,
    desc_label: *mut AnyObject,
    edit: *mut AnyObject,
    delete: *mut AnyObject,
    caps: Vec<*mut AnyObject>,
}
unsafe impl Send for MappingRow {}

/// 映射区行高(独立于全局 row_h;build 的卡片高度与 render 共用)。
/// Mapping-row height (independent of the global row_h; shared by the card height in build
/// and by render).
const MAPPING_ROW_H: f64 = 28.0;

/// 动作类型下拉的项,index 与语义一一对应(render/变化回调共用)。
/// The action-type popup items; index maps 1:1 to semantics (shared by render and the
/// change handler).
const MAPPING_ACTION_KEYS: [&str; 8] = [
    "settings.mapping_action_default",
    "settings.mapping_action_none",
    "settings.mapping_action_key",
    "settings.mapping_action_missioncontrol",
    "settings.mapping_action_launchpad",
    "settings.mapping_action_showdesktop",
    "settings.mapping_action_appexpose",
    "settings.mapping_action_switcher",
];
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
    // 左对齐:设置项标签贴在内容区左侧(NSTextAlignmentLeft = 0,arm64/x86_64 一致)。
    // Left-aligned: the row label hugs the content area's left edge (NSTextAlignmentLeft = 0,
    // identical on arm64 and x86_64).
    let _: () = msg_send![label, setAlignment: 0isize]; // NSTextAlignmentLeft
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
    // 防御:点 OK 时若仍在录制,先收尾(复位 RECORDING);关闭编辑面板。
    // Defensive: wrap up any in-progress recording on OK; close the edit panel.
    cancel_recording_from_main();
    close_mapping_panel();
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

    // 窗口切换开关被关闭:收起可能正开着的浮窗并复位状态,避免残留。
    // The switcher switch was turned off: dismiss a possibly-open overlay and reset
    // the state, so nothing stale lingers.
    if !cfg.windows.enabled {
        crate::overlay::reset_switcher();
    }

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

    // 剪贴板历史轮询热切换(无需重启)。
    // Clipboard-history polling hot-switch (no restart needed).
    if cfg.clipboard.enabled {
        crate::clipboard::start();
    } else {
        crate::clipboard::stop();
    }

    // persist 热切换:开启 → 从磁盘加载并合并历史(与 start() 的加载去重互幂等);
    // 关闭 → 删除磁盘历史文件(内存历史保留到本次退出)。
    // Persist hot-switch: ON -> load and merge the history from disk (idempotent with
    // start()'s load, dedup makes the double-merge harmless); OFF -> delete the history
    // file (the in-memory history stays until this session ends).
    crate::clipboard::apply_persist_toggle(cfg.clipboard.persist);
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
    // 防御:点取消时若仍在录制,先收尾(复位 RECORDING);关闭编辑面板。
    // Defensive: wrap up any in-progress recording on cancel; close the edit panel.
    cancel_recording_from_main();
    close_mapping_panel();
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
        let mut ui_guard = SETTINGS_UI.lock().unwrap();
        if let Some(u) = ui_guard.as_mut() {
            fill_mouse_device_controls(u, &resolved);
            // enable_mouse 勾选状态保持用户当前值;只重算冻结与条件显隐。
            // Keep the user's current enable_mouse state; only recompute freeze + visibility.
            update_mouse_controls_enabled(u);
            update_mode_dependent_visibility(u);
            // 设备切换:映射编辑态换成新设备的专属 mappings 并重渲染。
            // Device switch: reload the in-edit mappings from the new device's own profile.
            let dev = current_selected_device();
            let prof_idx = find_profile_index(&cfg, dev);
            *MAPPING_EDITS.lock().unwrap() = prof_idx
                .map(|i| cfg.mouse.profiles[i].button_mappings.clone())
                .unwrap_or_default();

            render_mapping_rows_locked(u);
        }
    }
}

// 侧边栏点击回调:读 sender 的 tag,切换到对应页。
// Sidebar click callback: read the sender's tag and switch to that page.

// ========== 按键映射录制 / button-mapping recording ==========

/// 渲染当前设备的按键映射行到滚动容器(录制/删除/设备切换后调用)。
/// 清掉旧行后按按钮号排序重建;mapping_doc 是 flipped 视图,行从顶向下排。
///
/// Render the selected device's button-mapping rows into the scroll container (called after
/// recording / deletion / device switch). Old rows are removed first, then rebuilt sorted by
/// button number; mapping_doc is flipped, so rows stack top-down.
fn render_mapping_rows() {
    unsafe {
        let mut guard = SETTINGS_UI.lock().unwrap();
        let Some(u) = guard.as_mut() else { return };
        render_mapping_rows_locked(u);
    }
}

/// 持锁版本:调用方已持有 SETTINGS_UI 锁时使用(load_settings_from / handle_device_changed),
/// 避免对同一把非重入 Mutex 二次加锁自死锁。
///
/// Locked variant: used when the caller already holds the SETTINGS_UI lock
/// (load_settings_from / handle_device_changed), avoiding a self-deadlock on the same
/// non-reentrant Mutex.
unsafe fn render_mapping_rows_locked(u: &mut SettingsUi) {
    unsafe {
        // removeFromSuperview 已释放父视图持有的引用;创建时的 alloc +1 已在 addSubview 后
        // 用 release_obj 平衡过。这里绝不能再次 release —— 双重释放会 EXC_BAD_ACCESS。
        // removeFromSuperview already drops the superview's reference; the alloc +1 was
        // balanced by release_obj right after addSubview. Re-releasing here would double-free
        // (EXC_BAD_ACCESS).
        let stale = u.mapping_rows.len();
        for row in u.mapping_rows.drain(..) {
            let _: () = msg_send![row.label, removeFromSuperview];
            let _: () = msg_send![row.desc_label, removeFromSuperview];
            let _: () = msg_send![row.edit, removeFromSuperview];
            let _: () = msg_send![row.delete, removeFromSuperview];
            for cap in row.caps {
                let _: () = msg_send![cap, removeFromSuperview];
            }
        }
        let doc = u.mapping_doc;
        // 列表只显示已绑定的行 + 刚添加未配置的临时行(方案 A 的行内配置,动态行)。
        // The list shows bound rows plus freshly added unconfigured ones (in-row config
        // from scheme A, but dynamic rows).
        let mut items: Vec<(u32, String)> = MAPPING_EDITS
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(b, d)| b.parse::<u32>().ok().map(|n| (n, d.clone())))
            .collect();

        log_debug!(
            "[mouse] render mappings: removed {} stale rows, {} live entries",
            stale,
            items.len()
        );
        items.sort_by_key(|(b, _)| *b);
        let row_h = MAPPING_ROW_H;
        // 文档恒高卡片高度(行少时行从顶部排起,不留底部空白;行多时滚动)。
        // The document is always the card height (rows start at the top; scroll when more).
        // 空状态提示:无行时显示。
        // Empty-state hint: shown when there are no rows.
        let _: () = msg_send![u.mapping_empty, setHidden: !items.is_empty()];
        // 只改高度,宽度保持初始值:曾用 setFrameSize(0.0, doc_h) 把宽清零,
        // 宽度为 0 的文档视图 hit-test 失败 —— 行内删除按钮永远点不到。
        // Resize height only, keeping the initial width: setFrameSize(0.0, doc_h) used to
        // zero the width, and a zero-width document view fails hit-testing -- the delete
        // buttons became unclickable.
        // 卡片高度固定(build 时定为 3 行);行多时由圆角 + masksToBounds 裁剪。
        // The card height is fixed (3 rows at build time); extra rows are clipped by the
        // rounded corner + masksToBounds.
        // flipped:y=0 在顶部,行从顶部依次向下排。
        // Flipped: y=0 is the top; rows stack down from the top.
        let mappings_on = {
            let st: isize = msg_send![u.mapping_enabled, state];
            st == 1
        };
        // 添加按钮一并置灰(开关关闭时不可添加新映射)。
        // The add button greys out too (no new mappings while off).
        let _: () = msg_send![u.add_mapping_button, setEnabled: mappings_on];
        let mut y = 0.0;
        let target = MENU_TARGET.lock().unwrap().unwrap().0;
        for (btn, desc) in items {
            // 动作类型 index:默认/无/系统动作/快捷键(快捷键 = Key Press)。
            // Action-type index: default / none / system action / shortcut (Key Press).
            let (action_idx, is_key) = match crate::mouse::shortcut::parse_binding(&desc) {
                Ok(crate::mouse::shortcut::Binding::Key(_)) => (2, true),
                Ok(crate::mouse::shortcut::Binding::System(_)) => {
                    // 系统动作名映射到下拉 index(3..=6)。
                    // System-action names map to popup indices (3..=6).
                    let idx = crate::mouse::shortcut::SYSTEM_ACTIONS
                        .iter()
                        .position(|a| a.eq_ignore_ascii_case(&desc))
                        .map(|i| i + 3)
                        .unwrap_or(0);
                    (idx, false)
                }
                Ok(crate::mouse::shortcut::Binding::Switcher) => (7, false),
                Ok(crate::mouse::shortcut::Binding::None) => (1, false),
                Err(_) => (0, false),
            };
            // 按钮名。
            // The button name.
            // NSTextField 的 13pt 文字在 28pt 框内偏顶部(非垂直居中):label 下移 7pt
            // 让文字中线与右侧按钮文字对齐(实测校准)。
            // The 13pt text sits toward the TOP of the 28pt field (not vertically centered):
            // shifting the label down 7pt aligns its midline with the buttons' (calibrated).
            let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
            let label: *mut AnyObject = msg_send![label, initWithFrame: NSRect::new(NSPoint::new(0.0, y + 5.0), NSSize::new(60.0, row_h))];
            set_field(label, 0);
            let _: () = msg_send![label, setBezeled: false];
            let _: () = msg_send![label, setDrawsBackground: false];
            let _: () = msg_send![label, setEditable: false];
            let name_ns = make_nsstring(&button_name(btn));
            let _: () = msg_send![label, setStringValue: name_ns];
            CFRelease(name_ns as *const c_void);
            let _: () = msg_send![label, setEnabled: mappings_on];
            let _: () = msg_send![doc, addSubview: label];
            release_obj(label);
            // 动作描述:系统动作/None 显示文本;Key Press 显示键帽胶囊。
            // The action description: text for system actions/None; keycaps for Key Press.
            let desc_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
            let desc_label: *mut AnyObject = msg_send![desc_label, initWithFrame: NSRect::new(NSPoint::new(64.0, y + 5.0), NSSize::new(130.0, row_h))];
            set_field(desc_label, 0);
            let _: () = msg_send![desc_label, setBezeled: false];
            let _: () = msg_send![desc_label, setDrawsBackground: false];
            let _: () = msg_send![desc_label, setEditable: false];
            let _: () = msg_send![desc_label, setEnabled: mappings_on];
            if !is_key && action_idx > 0 {
                // 系统动作/None 的动作名文本(用 i18n 标签)。
                // The action-name text for system actions/None (i18n labels).
                let key = MAPPING_ACTION_KEYS
                    .get(action_idx)
                    .copied()
                    .unwrap_or("settings.mapping_action_default");
                let ns = make_nsstring(&t(key));
                let _: () = msg_send![desc_label, setStringValue: ns];
                CFRelease(ns as *const c_void);
            }
            let _: () = msg_send![doc, addSubview: desc_label];
            release_obj(desc_label);
            // 编辑按钮(打开编辑面板)。
            // The edit button (opens the edit panel).
            let edit: *mut AnyObject = msg_send![class!(NSButton), alloc];
            let edit: *mut AnyObject = msg_send![edit, initWithFrame: NSRect::new(NSPoint::new(200.0, y + 2.0), NSSize::new(72.0, 24.0))];
            let _: () = msg_send![edit, setBezelStyle: 2isize]; // NSRoundedBezelStyle
            let _: () = msg_send![edit, setTag: btn as isize];
            let _: () = msg_send![edit, setEnabled: mappings_on];
            let _: () = msg_send![edit, setTarget: target];
            let _: () = msg_send![edit, setAction: sel!(handleMappingEdit:)];
            let edit_title = make_nsstring(&t("settings.mapping_edit"));
            let _: () = msg_send![edit, setTitle: edit_title];
            CFRelease(edit_title as *const c_void);
            let _: () = msg_send![doc, addSubview: edit];
            release_obj(edit);
            // 删除按钮(文字样式,与编辑按钮同款)。
            // The delete button (text style, same look as Edit).
            let delete: *mut AnyObject = msg_send![class!(NSButton), alloc];
            let delete: *mut AnyObject = msg_send![delete, initWithFrame: NSRect::new(NSPoint::new(278.0, y + 2.0), NSSize::new(72.0, 24.0))];
            let _: () = msg_send![delete, setBezelStyle: 2isize]; // NSRoundedBezelStyle
            let _: () = msg_send![delete, setTag: btn as isize];
            let _: () = msg_send![delete, setEnabled: mappings_on];
            let _: () = msg_send![delete, setTarget: target];
            let _: () = msg_send![delete, setAction: sel!(handleDeleteMapping:)];
            let del_title = make_nsstring(&t("settings.mapping_delete"));
            let _: () = msg_send![delete, setTitle: del_title];
            CFRelease(del_title as *const c_void);
            let _: () = msg_send![doc, addSubview: delete];
            release_obj(delete);
            // 键帽胶囊:修饰符号 + 主键各一个圆角小方块(像真实键盘键帽)。
            // Keycap pills: one rounded square per modifier symbol + the main key (like
            // real keyboard keycaps).
            let mut caps: Vec<*mut AnyObject> = Vec::new();
            if is_key {
                let key_str = display_shortcut(&desc);
                let cap_size = 20.0;
                let cap_y = y + (row_h - cap_size) / 2.0;
                let mut cap_x = 268.0;
                for ch in key_str.chars() {
                    let cap: *mut AnyObject = msg_send![class!(NSTextField), alloc];
                    let cap: *mut AnyObject = msg_send![cap, initWithFrame: NSRect::new(NSPoint::new(cap_x, cap_y), NSSize::new(cap_size, cap_size))];
                    set_field(cap, 0);
                    let _: () = msg_send![cap, setBezeled: false];
                    let _: () = msg_send![cap, setDrawsBackground: false];
                    let _: () = msg_send![cap, setEditable: false];
                    let _: () = msg_send![cap, setAlignment: 1isize]; // center
                    let _: () = msg_send![cap, setEnabled: mappings_on];
                    let ch_ns = make_nsstring(&ch.to_string());
                    let _: () = msg_send![cap, setStringValue: ch_ns];
                    CFRelease(ch_ns as *const c_void);
                    // 圆角浅灰底。
                    // Rounded light-gray backing.
                    let _: () = msg_send![cap, setWantsLayer: true];
                    let cap_layer: *mut AnyObject = msg_send![cap, layer];
                    let _: () = msg_send![cap_layer, setCornerRadius: 4.0f64];
                    let cap_bg: *mut AnyObject = msg_send![class!(NSColor), separatorColor];
                    layer_set_background(cap_layer, ns_color_to_cg(cap_bg));
                    let _: () = msg_send![doc, addSubview: cap];
                    release_obj(cap);
                    caps.push(cap);
                    cap_x += cap_size + 4.0;
                }
            }
            // 行底分隔线(最后一行被卡片底圆角裁掉,无妨)。
            // Row-bottom separator (the last row's line is clipped by the card corner).
            let sep: *mut AnyObject = msg_send![class!(NSView), alloc];
            let sep: *mut AnyObject = msg_send![sep, initWithFrame: NSRect::new(NSPoint::new(0.0, y + row_h - 1.0), NSSize::new(380.0, 1.0))];
            let _: () = msg_send![sep, setWantsLayer: true];
            let sep_layer: *mut AnyObject = msg_send![sep, layer];
            let sep_color: *mut AnyObject = msg_send![class!(NSColor), separatorColor];
            layer_set_background(sep_layer, ns_color_to_cg(sep_color));
            let _: () = msg_send![doc, addSubview: sep];
            release_obj(sep);
            u.mapping_rows.push(MappingRow {
                label,
                desc_label,
                edit,
                delete,
                caps,
            });
            y += row_h;
        }
    }
}

// ========== 录制弹出浮窗 / recording popup panel ==========

/// 经 performSelectorOnMainThread 唤醒主线程上的设置回调(无参版本)。
/// Wake the settings callback on the main thread (argument-less variant).
fn notify_main(sel: Sel) {
    if let Some(t) = *MENU_TARGET.lock().unwrap() {
        unsafe {
            let _: () = msg_send![
                t.0,
                performSelectorOnMainThread: sel,
                withObject: std::ptr::null::<AnyObject>(),
                waitUntilDone: false
            ];
        }
    }
}

/// 录制完成/取消的公共收尾:复位状态与 RECORDING 标志、停止录制线程的 RunLoop、
/// 通知主线程回调。在录制 tap 线程上调用。
///
/// Common teardown for recording finish/cancel: reset the stage and the RECORDING flag,
/// stop the recording thread's RunLoop, and wake the main-thread callback. Called on the
/// recording tap thread.
/// 防御性取消录制:设置窗口 OK/Cancel/关闭时若仍在录制(面板录制中),复位状态,
/// 避免残留录制态影响下次使用。
/// Defensive recording cancel: when the settings window OKs/cancels/closes while a
/// recording is in progress, reset the state so nothing lingers.
pub(crate) fn cancel_recording_from_main() {
    if *REC_STAGE.lock().unwrap() != RecStage::Idle {
        *REC_STAGE.lock().unwrap() = RecStage::Idle;
        *REC_MODS.lock().unwrap() = 0;
        REC_DESC.lock().unwrap().clear();
        REC_CANCEL.store(true, std::sync::atomic::Ordering::Relaxed);
        crate::mouse::event_tap::RECORDING.store(false, Ordering::Relaxed);
        disable_rec_tap();
        if let Some(rl) = *REC_RUNLOOP.0.lock().unwrap() {
            unsafe {
                crate::event_tap::CFRunLoopStop(rl);
            }
        }
    }
}

/// 立即禁用录制 tap(若存在)。禁用是同步生效的,CGEventTapEnable(false) 后该 tap
/// 不再收到任何事件。
/// Immediately disable the recording tap (if any). Disabling is synchronous: after
/// CGEventTapEnable(false) the tap receives nothing more.
fn disable_rec_tap() {
    if let Some(tap) = *REC_TAP.0.lock().unwrap() {
        unsafe {
            crate::event_tap::CGEventTapEnable(tap, false);
        }
    }
}

unsafe fn finish_recording(success: bool) {
    *REC_STAGE.lock().unwrap() = RecStage::Idle;
    // 完成/取消后清零中间态,杜绝下次录制的残留(见 handle_add_mapping 的注释)。
    // Clear the intermediates on finish/cancel, so nothing leaks into the next session.
    *REC_MODS.lock().unwrap() = 0;
    REC_DESC.lock().unwrap().clear();
    REC_CANCEL.store(true, std::sync::atomic::Ordering::Relaxed);
    crate::mouse::event_tap::RECORDING.store(false, Ordering::Relaxed);
    // 先禁用 tap 再停 runloop:禁用立即生效,杜绝退出窗口期吞键。
    // Disable the tap before stopping the loop: disabling takes effect immediately,
    // eliminating the exit-window swallowing.
    disable_rec_tap();
    if let Some(rl) = *REC_RUNLOOP.0.lock().unwrap() {
        crate::event_tap::CFRunLoopStop(rl);
    }
    notify_main(if success {
        sel!(handleRecordingFinished:)
    } else {
        sel!(handleRecordingCancelled:)
    });
}

/// 录制 tap 回调(录制线程):捕获组合键 keyDown(esc 无修饰 = 取消)后完成。
/// 录制输入(组合键)吞掉,flagsChanged 透传并实时刷新浮窗修饰显示。
///
/// Recording tap callback (recording thread): a combo keyDown finishes the recording
/// (bare Esc cancels). The combo input is swallowed; flagsChanged passes through while
/// refreshing the popup's modifier display live.
unsafe extern "C" fn recording_tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    _user_info: *mut c_void,
) -> CGEventRef {
    match event_type {
        25 => {
            // otherMouseDown; 按钮号在 field 3(与 mouse/event_tap.rs 同)。
            // 只在 WaitingButton 阶段捕获/吞;取消后残留的 tap(若有)一律透传。
            // Button number lives in field 3 (same as mouse/event_tap.rs). Only capture/
            // swallow while WaitingButton; a lingering tap after cancel passes everything.
            if *REC_STAGE.lock().unwrap() == RecStage::WaitingButton {
                let btn = CGEventGetIntegerValueField(event, 3) as u32;
                if btn >= 2 {
                    *REC_BUTTON.lock().unwrap() = btn;
                    // 面板录触发:捕获侧键即完成,回调更新面板。
                    // Panel trigger recording: the side button completes it; the callback
                    // updates the panel.
                    finish_recording(true);
                    return std::ptr::null_mut();
                }
            }
            event
        }
        12 => {
            // flagsChanged:WaitingCombo 阶段实时累积修饰键,刷新浮窗显示(如按住 Cmd 显示 ⌘)。
            // 不吞事件(透传,用户仍可正常操作)。
            // flagsChanged: during WaitingCombo, accumulate modifiers live and refresh the
            // popup (e.g. holding Cmd shows ⌘). The event passes through (untouched).
            if *REC_STAGE.lock().unwrap() == RecStage::WaitingCombo {
                let flags = CGEventGetFlags(event) as u32;
                *REC_MODS.lock().unwrap() =
                    flags & (0x0010_0000 | 0x0008_0000 | 0x0004_0000 | 0x0002_0000);
                notify_main(sel!(handleRecordingStage:));
            }
            event
        }
        10 => {
            // keyDown:仅等待组合键阶段处理。
            // keyDown: only handled while waiting for the combo.
            if *REC_STAGE.lock().unwrap() == RecStage::WaitingCombo {
                let keycode = CGEventGetIntegerValueField(event, 9) as u16;
                let flags = CGEventGetFlags(event) as u32;
                let mods = flags & (0x0010_0000 | 0x0008_0000 | 0x0004_0000 | 0x0002_0000);
                // 无修饰的 Esc = 取消录制。
                // Bare Esc cancels the recording.
                if keycode == 53 && mods == 0 {
                    finish_recording(false);
                    return std::ptr::null_mut();
                }
                let desc = describe_shortcut(keycode, mods);
                *REC_DESC.lock().unwrap() = desc;
                finish_recording(true);
                return std::ptr::null_mut();
            }
            event
        }
        _ => event,
    }
}

/// 录制线程:独立 HID 层 tap 捕获按键/键盘(不干扰鼠标 tap 与窗口切换 tap;
/// RECORDING 标志已让鼠标 tap 跳过绑定执行)。RunLoop 在完成/取消时被停止。
///
/// Recording thread: a dedicated HID-level tap captures the button/keyboard input (does not
/// interfere with the mouse tap or the switcher tap; the RECORDING flag already makes the
/// mouse tap skip binding execution). The RunLoop is stopped on finish/cancel.
unsafe fn recording_thread() {
    let rl = crate::event_tap::CFRunLoopGetCurrent();
    *REC_RUNLOOP.0.lock().unwrap() = Some(rl);
    // otherMouseDown(25) | keyDown(10) | flagsChanged(12)
    let mask: crate::event_tap::CGEventMask = (1u64 << 25) | (1u64 << 10) | (1u64 << 12);
    let tap = crate::event_tap::create_tap_with_retry(
        crate::event_tap::tap_location::HID_EVENT_TAP,
        crate::event_tap::tap_placement::HEAD_INSERT,
        crate::event_tap::tap_options::DEFAULT_TAP,
        mask,
        Some(recording_tap_callback),
        std::ptr::null_mut(),
        "rec",
        Some(&REC_CANCEL),
    );
    let Some(tap) = tap else {
        // tap 创建失败(缺权限等):复位状态,让主线程提示取消。
        // Tap creation failed (missing permission etc.): reset state, notify cancel.
        *REC_STAGE.lock().unwrap() = RecStage::Idle;
        crate::mouse::event_tap::RECORDING.store(false, Ordering::Relaxed);
        *REC_RUNLOOP.0.lock().unwrap() = None;
        notify_main(sel!(handleRecordingCancelled:));
        return;
    };
    let source = crate::event_tap::CFMachPortCreateRunLoopSource(std::ptr::null_mut(), tap, 0);
    crate::event_tap::CFRunLoopAddSource(rl, source, crate::event_tap::kCFRunLoopDefaultMode);
    crate::event_tap::CGEventTapEnable(tap, true);
    *REC_TAP.0.lock().unwrap() = Some(tap);
    log_info!("[mouse] recording tap started");
    crate::event_tap::CFRunLoopRun();
    *REC_TAP.0.lock().unwrap() = None;
    *REC_RUNLOOP.0.lock().unwrap() = None;
    log_info!("[mouse] recording tap stopped");
}

/// 删除按钮(tag = 按钮号):移除该映射。
/// The delete button (tag = button number): removes that mapping.
/// 「添加映射」按钮:打开映射编辑面板(触发/动作/组合键在面板里一次配完)。
/// The "Add mapping" button: opens the mapping edit panel (trigger/action/combo configured
/// in one place, LinearMouse style).
pub(crate) extern "C" fn handle_add_mapping(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    // 面板已开:先关再开。
    // Panel already open: close it first.
    if EDIT_PANEL.lock().unwrap().is_some() {
        close_mapping_panel();
    }
    open_mapping_panel(None);
    log_info!("[mouse] mapping panel opened (new mapping)");
}

/// 列表行「编辑」回调(tag = 按钮号):打开面板预填该按钮的映射。
/// The row "Edit" callback (tag = button number): opens the panel prefilled.
pub(crate) extern "C" fn handle_mapping_edit(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    // 面板已开:先关再开(用户点编辑期望打开新面板,而不是无反应)。
    // Panel already open: close it first (the user expects a fresh panel, not silence).
    if EDIT_PANEL.lock().unwrap().is_some() {
        close_mapping_panel();
    }
    let tag: isize = unsafe { msg_send![sender as *mut AnyObject, tag] };
    open_mapping_panel(Some(tag as u32));
    log_info!("[mouse] mapping panel opened (edit button {})", tag);
}

/// 面板「录制触发」按钮:录侧键。
/// The panel "Record trigger" button: records the side button.
pub(crate) extern "C" fn handle_panel_record_trigger(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    if *REC_STAGE.lock().unwrap() != RecStage::Idle {
        return;
    }
    *REC_BUTTON.lock().unwrap() = 0;
    *REC_MODS.lock().unwrap() = 0;
    REC_DESC.lock().unwrap().clear();
    *REC_MODE.lock().unwrap() = RecMode::PanelTrigger;
    *REC_STAGE.lock().unwrap() = RecStage::WaitingButton;
    REC_CANCEL.store(false, std::sync::atomic::Ordering::Relaxed);
    crate::mouse::event_tap::RECORDING.store(true, Ordering::Relaxed);
    // 录制中禁用面板确认,防中途误确认。
    // Disable the panel OK while recording.
    unsafe {
        if let Some(o) = *EDIT_PANEL_OK.lock().unwrap() {
            let _: () = msg_send![o.0, setEnabled: false];
        }
    }
    log_info!("[mouse] recording trigger (press a mouse button)");
    *RECORD_THREAD.lock().unwrap() = Some(std::thread::spawn(|| unsafe { recording_thread() }));
}

/// 面板「录制组合键」按钮:录组合键(Key Press 动作)。
/// The panel "Record combo" button: records the combo (Key Press action).
pub(crate) extern "C" fn handle_panel_record_combo(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    if *REC_STAGE.lock().unwrap() != RecStage::Idle {
        return;
    }
    // 需要先有触发按钮(新增时)。
    // The trigger must exist first (for new mappings).
    let Some(btn) = *EDIT_BUTTON.lock().unwrap() else {
        return;
    };
    *REC_BUTTON.lock().unwrap() = btn;
    *REC_MODS.lock().unwrap() = 0;
    REC_DESC.lock().unwrap().clear();
    *REC_MODE.lock().unwrap() = RecMode::PanelCombo;
    *REC_STAGE.lock().unwrap() = RecStage::WaitingCombo;
    REC_CANCEL.store(false, std::sync::atomic::Ordering::Relaxed);
    crate::mouse::event_tap::RECORDING.store(true, Ordering::Relaxed);
    unsafe {
        if let Some(o) = *EDIT_PANEL_OK.lock().unwrap() {
            let _: () = msg_send![o.0, setEnabled: false];
        }
    }
    log_info!("[mouse] recording combo (press the key combo)");
    *RECORD_THREAD.lock().unwrap() = Some(std::thread::spawn(|| unsafe { recording_thread() }));
}

/// 面板动作下拉变化:更新组合键行显隐与确认可用性。
/// The panel action popup changed: refresh the combo row and OK availability.
pub(crate) extern "C" fn handle_panel_action_changed(
    _self: *mut c_void,
    _cmd: Sel,
    sender: *mut c_void,
) {
    let idx: isize = unsafe { msg_send![sender as *mut AnyObject, indexOfSelectedItem] };
    *EDIT_ACTION_IDX.lock().unwrap() = idx;
    // 切到非 Key Press 时清掉已录组合键。
    // Leaving Key Press clears the recorded combo.
    if idx != 2 {
        EDIT_COMBO.lock().unwrap().clear();
    }
    unsafe {
        update_mapping_panel();
    }
}

/// 面板「确认」:写入 MAPPING_EDITS 并关闭。
/// The panel "OK": write to MAPPING_EDITS and close.
pub(crate) extern "C" fn handle_mapping_confirm(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    let Some(btn) = *EDIT_BUTTON.lock().unwrap() else {
        return;
    };
    let idx = *EDIT_ACTION_IDX.lock().unwrap();
    let mut edits = MAPPING_EDITS.lock().unwrap();
    match idx {
        0 => {
            // Default:等同删除(列表只显示已绑定)。
            // Default: same as delete (the list shows bound rows only).
            edits.remove(&btn.to_string());
        }
        1 => {
            edits.insert(btn.to_string(), "none".to_string());
        }
        2 => {
            // Key Press:需要已录组合键(确认按钮已按可用性禁用)。
            // Key Press: needs a recorded combo (OK is disabled otherwise).
            let combo = EDIT_COMBO.lock().unwrap().clone();
            if combo.is_empty() {
                return;
            }
            edits.insert(btn.to_string(), combo);
        }
        7 => {
            // 打开切换器。
            // Open the switcher.
            edits.insert(btn.to_string(), "switcher".to_string());
        }
        i => {
            if let Some(name) = crate::mouse::shortcut::SYSTEM_ACTIONS.get((i - 3) as usize) {
                edits.insert(btn.to_string(), name.to_string());
            }
        }
    }
    drop(edits);
    close_mapping_panel();
    render_mapping_rows();
    log_info!(
        "[mouse] mapping panel confirmed: button {} -> index {}",
        btn,
        idx
    );
}

/// 面板「取消」:直接关闭,不改动。
/// The panel "Cancel": close without changes.
pub(crate) extern "C" fn handle_mapping_cancel(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    close_mapping_panel();
    log_info!("[mouse] mapping panel cancelled");
}

/// 映射总开关变化回调:重渲染映射行(关闭时行控件置灰不可点)。
/// The mappings master switch toggled: re-render the rows (greyed out and inert when off).
pub(crate) extern "C" fn handle_mapping_enabled_changed(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    render_mapping_rows();
    log_info!("[mouse] mappings master switch toggled");
}

// ========== 映射编辑面板实现 / mapping edit panel ==========

/// 打开映射编辑面板。btn = 正在编辑的按钮号(Some = 编辑已有映射,None = 新增)。
/// 新增时先从录制侧键开始;编辑时预填当前值。
///
/// Open the mapping edit panel. btn = the button being edited (Some = editing an existing
/// mapping, None = adding a new one). New mappings start by recording the side button;
/// existing ones are prefilled.
fn open_mapping_panel(btn: Option<u32>) {
    unsafe {
        *EDIT_BUTTON.lock().unwrap() = btn;
        *EDIT_COMBO.lock().unwrap() = String::new();
        // 预填:编辑已有映射时,按当前值推导动作 index 与组合键。
        // Prefill: for an existing mapping, derive the action index and combo from the
        // current value.
        let (action_idx, combo) = match btn {
            Some(b) => match MAPPING_EDITS.lock().unwrap().get(&b.to_string()) {
                Some(desc) => {
                    use crate::mouse::shortcut::{Binding, SYSTEM_ACTIONS};
                    match crate::mouse::shortcut::parse_binding(desc) {
                        Ok(Binding::Key(_)) => (2, desc.clone()),
                        Ok(Binding::System(_)) => (
                            SYSTEM_ACTIONS
                                .iter()
                                .position(|a| a.eq_ignore_ascii_case(desc))
                                .map(|i| i as isize + 3)
                                .unwrap_or(0),
                            String::new(),
                        ),
                        Ok(Binding::Switcher) => (7, String::new()),
                        Ok(Binding::None) => (1, String::new()),
                        Err(_) => (0, String::new()),
                    }
                }
                None => (0, String::new()),
            },
            None => (0, String::new()),
        };
        *EDIT_ACTION_IDX.lock().unwrap() = action_idx;
        *EDIT_COMBO.lock().unwrap() = combo;

        let existing = EDIT_PANEL.lock().unwrap().map(|p| p.0);
        let panel = if let Some(p) = existing {
            p
        } else {
            // 创建面板:圆角毛玻璃 + 触发/动作/组合键行 + 取消确认。
            // Create the panel: rounded material + trigger/action/combo rows + cancel/OK.
            let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(440.0, 240.0));
            let panel: *mut AnyObject = msg_send![class!(NSPanel), alloc];
            let panel: *mut AnyObject = msg_send![panel, initWithContentRect: frame, styleMask: 0u64, backing: 2u64, defer: false];
            let _: () = msg_send![panel, setReleasedWhenClosed: false];
            let _: () = msg_send![panel, setOpaque: false];
            let _: () = msg_send![panel, setLevel: 3isize]; // NSFloatingWindowLevel
                                                            // 背景透明:圆角外的四角露出后面的遮罩/设置窗口,圆角才可见。
                                                            // Transparent background: the corners outside the radius show what's behind
                                                            // (the dim layer / settings window), making the rounding visible.
            let clear_ns: *mut AnyObject = msg_send![class!(NSColor), clearColor];
            let _: () = msg_send![panel, setBackgroundColor: clear_ns];
            // 背景:普通视图 + windowBackgroundColor(与设置窗口右侧内容区同款颜色,
            // 同款机制 —— 该颜色的 CGColor 可用;controlBackgroundColor 的动态色才为 nil)。
            // Background: a plain view + windowBackgroundColor (same color and mechanism as
            // the settings window's content area -- its CGColor works; only
            // controlBackgroundColor's dynamic color is nil).
            let ve: *mut AnyObject = msg_send![class!(NSView), alloc];
            let ve: *mut AnyObject = msg_send![ve, initWithFrame: frame];
            let _: () = msg_send![ve, setWantsLayer: true];
            let ve_layer: *mut AnyObject = msg_send![ve, layer];
            let _: () = msg_send![ve_layer, setCornerRadius: 10.0f64];
            let _: () = msg_send![ve_layer, setMasksToBounds: true];
            let bg_ns: *mut AnyObject = msg_send![class!(NSColor), windowBackgroundColor];
            layer_set_background(ve_layer, ns_color_to_cg(bg_ns));
            let _: () = msg_send![panel, setContentView: ve];
            release_obj(ve);
            let target = MENU_TARGET.lock().unwrap().unwrap().0;
            // 触发行。
            // The trigger row.
            let t_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
            let t_label: *mut AnyObject = msg_send![t_label, initWithFrame: NSRect::new(NSPoint::new(16.0, 190.0), NSSize::new(110.0, 24.0))];
            set_field(t_label, 0);
            let _: () = msg_send![t_label, setBezeled: false];
            let _: () = msg_send![t_label, setDrawsBackground: false];
            let _: () = msg_send![t_label, setEditable: false];
            let t_label_ns = make_nsstring(&t("settings.mapping_panel_trigger"));
            let _: () = msg_send![t_label, setStringValue: t_label_ns];
            CFRelease(t_label_ns as *const c_void);
            let _: () = msg_send![ve, addSubview: t_label];
            release_obj(t_label);
            let btn_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
            let btn_label: *mut AnyObject = msg_send![btn_label, initWithFrame: NSRect::new(NSPoint::new(130.0, 190.0), NSSize::new(140.0, 24.0))];
            set_field(btn_label, 0);
            let _: () = msg_send![btn_label, setBezeled: false];
            let _: () = msg_send![btn_label, setDrawsBackground: false];
            let _: () = msg_send![btn_label, setEditable: false];
            let _: () = msg_send![ve, addSubview: btn_label];
            release_obj(btn_label);
            let rec_btn: *mut AnyObject = msg_send![class!(NSButton), alloc];
            let rec_btn: *mut AnyObject = msg_send![rec_btn, initWithFrame: NSRect::new(NSPoint::new(280.0, 190.0), NSSize::new(140.0, 24.0))];
            let _: () = msg_send![rec_btn, setBezelStyle: 2isize]; // NSRoundedBezelStyle
            let rec_title = make_nsstring(&t("settings.mapping_record"));
            let _: () = msg_send![rec_btn, setTitle: rec_title];
            CFRelease(rec_title as *const c_void);
            let _: () = msg_send![rec_btn, setTarget: target];
            let _: () = msg_send![rec_btn, setAction: sel!(handlePanelRecordTrigger:)];
            let _: () = msg_send![ve, addSubview: rec_btn];
            release_obj(rec_btn);
            // 动作行。
            // The action row.
            let a_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
            let a_label: *mut AnyObject = msg_send![a_label, initWithFrame: NSRect::new(NSPoint::new(16.0, 140.0), NSSize::new(110.0, 24.0))];
            set_field(a_label, 0);
            let _: () = msg_send![a_label, setBezeled: false];
            let _: () = msg_send![a_label, setDrawsBackground: false];
            let _: () = msg_send![a_label, setEditable: false];
            let a_label_ns = make_nsstring(&t("settings.mapping_panel_action"));
            let _: () = msg_send![a_label, setStringValue: a_label_ns];
            CFRelease(a_label_ns as *const c_void);
            let _: () = msg_send![ve, addSubview: a_label];
            release_obj(a_label);
            let popup_items: Vec<String> = MAPPING_ACTION_KEYS.iter().map(|k| t(k)).collect();
            let popup_items: Vec<&str> = popup_items.iter().map(|s| s.as_str()).collect();
            let action: *mut AnyObject = make_popup(130.0, 140.0, 290.0, 26.0, &popup_items, 0);
            let _: () = msg_send![action, setTarget: target];
            let _: () = msg_send![action, setAction: sel!(handlePanelActionChanged:)];
            // 下拉图标(与行内同款)。
            // Popup icons (same as the rows).
            let menu: *mut AnyObject = msg_send![action, menu];
            let item_cnt: usize = msg_send![menu, numberOfItems];
            let icons = [
                "dot.circle",
                "slash.circle",
                "keyboard",
                "square.grid.2x2",
                "square.grid.3x3",
                "macwindow",
                "rectangle.on.rectangle",
                "arrow.left.arrow.right",
            ];
            for (i, icon) in icons.iter().enumerate().take(item_cnt) {
                let item: *mut AnyObject = msg_send![menu, itemAtIndex: i as isize];
                let sym = make_nsstring(icon);
                let img: *mut AnyObject = msg_send![
                    class!(NSImage),
                    imageWithSystemSymbolName: sym,
                    accessibilityDescription: std::ptr::null::<AnyObject>()
                ];
                CFRelease(sym as *const c_void);
                if !img.is_null() {
                    let _: () = msg_send![item, setImage: img];
                }
            }
            let _: () = msg_send![ve, addSubview: action];
            release_obj(action);
            // 组合键行(Key Press 时显示)。
            // The combo row (shown for Key Press).
            let combo_btn: *mut AnyObject = msg_send![class!(NSButton), alloc];
            let combo_btn: *mut AnyObject = msg_send![combo_btn, initWithFrame: NSRect::new(NSPoint::new(130.0, 96.0), NSSize::new(140.0, 24.0))];
            let _: () = msg_send![combo_btn, setBezelStyle: 2isize];
            let combo_title = make_nsstring(&t("settings.mapping_record"));
            let _: () = msg_send![combo_btn, setTitle: combo_title];
            CFRelease(combo_title as *const c_void);
            let _: () = msg_send![combo_btn, setTarget: target];
            let _: () = msg_send![combo_btn, setAction: sel!(handlePanelRecordCombo:)];
            let _: () = msg_send![combo_btn, setHidden: true];
            let _: () = msg_send![ve, addSubview: combo_btn];
            release_obj(combo_btn);
            let combo_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
            let combo_label: *mut AnyObject = msg_send![combo_label, initWithFrame: NSRect::new(NSPoint::new(280.0, 96.0), NSSize::new(140.0, 24.0))];
            set_field(combo_label, 0);
            let _: () = msg_send![combo_label, setBezeled: false];
            let _: () = msg_send![combo_label, setDrawsBackground: false];
            let _: () = msg_send![combo_label, setEditable: false];
            let _: () = msg_send![combo_label, setHidden: true];
            let _: () = msg_send![ve, addSubview: combo_label];
            release_obj(combo_label);
            // 取消/确认。
            // Cancel/OK.
            let cancel: *mut AnyObject = msg_send![class!(NSButton), alloc];
            let cancel: *mut AnyObject = msg_send![cancel, initWithFrame: NSRect::new(NSPoint::new(240.0, 24.0), NSSize::new(88.0, 28.0))];
            let _: () = msg_send![cancel, setBezelStyle: 2isize];
            let cancel_ns = make_nsstring(&t("settings.recording_cancel"));
            let _: () = msg_send![cancel, setTitle: cancel_ns];
            CFRelease(cancel_ns as *const c_void);
            let _: () = msg_send![cancel, setTarget: target];
            let _: () = msg_send![cancel, setAction: sel!(handleMappingCancel:)];
            let _: () = msg_send![ve, addSubview: cancel];
            release_obj(cancel);
            let ok: *mut AnyObject = msg_send![class!(NSButton), alloc];
            let ok: *mut AnyObject = msg_send![ok, initWithFrame: NSRect::new(NSPoint::new(336.0, 24.0), NSSize::new(88.0, 28.0))];
            let _: () = msg_send![ok, setBezelStyle: 2isize];
            let ok_ns = make_nsstring(&t("settings.ok"));
            let _: () = msg_send![ok, setTitle: ok_ns];
            CFRelease(ok_ns as *const c_void);
            let _: () = msg_send![ok, setTarget: target];
            let _: () = msg_send![ok, setAction: sel!(handleMappingConfirm:)];
            let _: () = msg_send![ve, addSubview: ok];
            release_obj(ok);
            *EDIT_PANEL.lock().unwrap() = Some(ObjPtr(panel));
            *EDIT_PANEL_BTN_LABEL.lock().unwrap() = Some(ObjPtr(btn_label));
            *EDIT_PANEL_ACTION.lock().unwrap() = Some(ObjPtr(action));
            *EDIT_PANEL_COMBO_BTN.lock().unwrap() = Some(ObjPtr(combo_btn));
            *EDIT_PANEL_COMBO_LABEL.lock().unwrap() = Some(ObjPtr(combo_label));
            *EDIT_PANEL_OK.lock().unwrap() = Some(ObjPtr(ok));
            panel
        };
        // 更新面板显示。
        // Update the panel display.
        update_mapping_panel();
        // 定位:相对外层设置窗口居中(不随屏幕位置漂移)。
        // Position: centered on the settings window (does not drift with the screen).
        let win = SETTINGS_UI.lock().unwrap().as_ref().unwrap().window;
        let win_frame: NSRect = msg_send![win, frame];
        let pf: NSRect = msg_send![panel, frame];
        let _: () = msg_send![panel, setFrameOrigin: NSPoint::new(
            win_frame.origin.x + (win_frame.size.width - pf.size.width) / 2.0,
            win_frame.origin.y + (win_frame.size.height - pf.size.height) / 2.0
        )];
        // 遮罩:设置窗口内容区上的半透明灰层(modal 调暗;面板在遮罩之上)。
        // The dim layer: a translucent gray overlay on the settings content (modal dim;
        // the panel floats above it).
        let content: *mut AnyObject = msg_send![win, contentView];
        let content_bounds: NSRect = msg_send![content, bounds];
        let dim: *mut AnyObject = msg_send![class!(NSView), alloc];
        let dim: *mut AnyObject = msg_send![dim, initWithFrame: content_bounds];
        let _: () = msg_send![dim, setWantsLayer: true];
        let dim_layer: *mut AnyObject = msg_send![dim, layer];
        // 半透明黑 25%:hex 是 0xRRGGBBAA —— alpha 在最低字节。
        // Translucent black at 25%: hex is 0xRRGGBBAA -- alpha lives in the low byte.
        layer_set_background(dim_layer, hex_to_cg_color(0x00000040));
        let _: () = msg_send![content, addSubview: dim];
        release_obj(dim);
        *EDIT_DIM.lock().unwrap() = Some(ObjPtr(dim));
        let _: () = msg_send![panel, orderFrontRegardless];
    }
}

/// 刷新面板显示(触发按钮名/动作下拉/组合键行/确认可用性)。
/// Refresh the panel display (trigger name / action popup / combo row / OK availability).
unsafe fn update_mapping_panel() {
    let btn = *EDIT_BUTTON.lock().unwrap();
    // 触发按钮名。
    // The trigger button name.
    if let Some(l) = *EDIT_PANEL_BTN_LABEL.lock().unwrap() {
        let text = match btn {
            Some(b) => crate::mouse::shortcut::button_name(b),
            None => t("settings.mapping_panel_no_button"),
        };
        let ns = make_nsstring(&text);
        let _: () = msg_send![l.0, setStringValue: ns];
        CFRelease(ns as *const c_void);
    }
    let idx = *EDIT_ACTION_IDX.lock().unwrap();
    // 动作下拉。
    // The action popup.
    if let Some(a) = *EDIT_PANEL_ACTION.lock().unwrap() {
        let _: () = msg_send![a.0, selectItemAtIndex: idx];
    }
    // 组合键行显隐(Key Press = index 2)。
    // Combo row visibility (Key Press = index 2).
    let is_key = idx == 2;
    if let Some(b) = *EDIT_PANEL_COMBO_BTN.lock().unwrap() {
        let _: () = msg_send![b.0, setHidden: !is_key];
    }
    if let Some(l) = *EDIT_PANEL_COMBO_LABEL.lock().unwrap() {
        let _: () = msg_send![l.0, setHidden: !is_key];
        if is_key {
            let combo = EDIT_COMBO.lock().unwrap().clone();
            let text = if combo.is_empty() {
                t("settings.mapping_panel_no_combo")
            } else {
                display_shortcut(&combo)
            };
            let ns = make_nsstring(&text);
            let _: () = msg_send![l.0, setStringValue: ns];
            CFRelease(ns as *const c_void);
        }
    }
    // 确认可用性:Key Press 需要已录组合键;新增需要已录侧键。
    // OK availability: Key Press needs a recorded combo; a new mapping needs the trigger.
    let ok_enabled =
        (idx != 2 || !EDIT_COMBO.lock().unwrap().is_empty()) && (btn.is_some() || idx != 2);
    if let Some(o) = *EDIT_PANEL_OK.lock().unwrap() {
        let _: () = msg_send![o.0, setEnabled: ok_enabled];
    }
}

/// 关闭映射编辑面板(幂等)。
/// Close the mapping edit panel (idempotent).
fn close_mapping_panel() {
    log_debug!("[mouse] close panel: step 1 (orderOut)");
    // take():if-let scrutinee 的 MutexGuard 会贯穿整个 if 块,块内再 lock 同一把
    // Mutex 就是自死锁(风火轮,实测)。take 拿走值后 guard 立即释放。
    // take(): an if-let scrutinee MutexGuard lives for the WHOLE if block, so locking the
    // same Mutex inside it self-deadlocks (the beach ball, verified). take() moves the
    // value out and the guard drops immediately.
    if let Some(p) = EDIT_PANEL.lock().unwrap().take() {
        unsafe {
            let _: () = msg_send![p.0, orderOut: std::ptr::null::<AnyObject>()];
        }
    }
    log_debug!("[mouse] close panel: step 2 (remove dim)");
    // 移除遮罩。
    // Remove the dim layer.
    if let Some(d) = EDIT_DIM.lock().unwrap().take() {
        unsafe {
            let _: () = msg_send![d.0, removeFromSuperview];
        }
    }
    log_debug!("[mouse] close panel: step 3 (reset state)");
    *EDIT_BUTTON.lock().unwrap() = None;
    *EDIT_ACTION_IDX.lock().unwrap() = 0;
    EDIT_COMBO.lock().unwrap().clear();
}

/// 删除按钮(tag = 按钮号):移除该映射。
/// The delete button (tag = button number): removes that mapping.
pub(crate) extern "C" fn handle_delete_mapping(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    let tag: isize = unsafe { msg_send![sender as *mut AnyObject, tag] };
    MAPPING_EDITS.lock().unwrap().remove(&tag.to_string());
    render_mapping_rows();
    log_info!("[mouse] removed mapping for button {}", tag);
}

pub(crate) extern "C" fn handle_recording_finished(
    _self: *mut c_void,
    _cmd: Sel,
    _arg: *mut c_void,
) {
    let btn = *REC_BUTTON.lock().unwrap();
    match *REC_MODE.lock().unwrap() {
        // 面板录触发侧键:更新面板显示。
        // The panel recorded the trigger: update the panel.
        RecMode::PanelTrigger => {
            *EDIT_BUTTON.lock().unwrap() = Some(btn);
            unsafe {
                update_mapping_panel();
            }
            log_info!("[mouse] panel trigger recorded: button {}", btn);
        }
        // 面板录组合键:更新面板显示(Key Press 动作)。
        // The panel recorded the combo: update the panel (Key Press action).
        RecMode::PanelCombo => {
            let desc = REC_DESC.lock().unwrap().clone();
            *EDIT_COMBO.lock().unwrap() = desc.clone();
            unsafe {
                update_mapping_panel();
            }
            log_info!("[mouse] panel combo recorded: {}", desc);
        }
    }
}

/// 主线程回调:录制取消/失败。
/// Main-thread callback: recording cancelled/failed.
pub(crate) extern "C" fn handle_recording_cancelled(
    _self: *mut c_void,
    _cmd: Sel,
    _arg: *mut c_void,
) {
    log_info!("[mouse] button-mapping recording cancelled");
}

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
    let idx = if idx > 3 { 0 } else { idx };
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
            ui.sidebar_clipboard,
        ];
        let views = [
            ui.general_view,
            ui.experimental_view,
            ui.mouse_view,
            ui.clipboard_view,
        ];
        // 高亮背景对齐到选中按钮的 frame / align the highlight to the selected button's frame
        let frame: NSRect = msg_send![buttons[idx], frame];
        let _: () = msg_send![ui.sidebar_highlight, setFrame: frame];
        // 选中项粗体 + 白字(whiteColor),未选中项常规 labelColor。
        // Bold + white text when selected; regular labelColor otherwise.
        let titles = [
            t("settings.sidebar_general"),
            t("settings.sidebar_experimental"),
            t("settings.sidebar_mouse"),
            t("settings.sidebar_clipboard"),
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
        let mut ui_guard = SETTINGS_UI.lock().unwrap();
        let ui = match ui_guard.as_mut() {
            Some(u) => u,
            None => return,
        };
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
        // windows_enabled / show_minimized:switch state(1=on / 0=off)。
        // windows_enabled / show_minimized: switch state (1=on / 0=off).
        let we_state = if cfg.windows.enabled { 1isize } else { 0isize };
        let _: () = msg_send![ui.windows_enabled, setState: we_state];
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

        // 按键映射编辑态 = 当前设备 profile 自己的 mappings(不含"所有鼠标"档的合并值,
        // 编辑/删除只作用于这台设备的专属档;通配档在"所有鼠标"无 UI 项,不在此编辑)。
        // The mappings in-edit = the selected device's OWN profile mappings (not the merged
        // values: edits/deletes only touch this device's dedicated profile; the wildcard
        // "All Mice" profile has no UI entry, so it isn't edited here).
        let dev = current_selected_device();
        let prof_idx = find_profile_index(cfg, dev);
        *MAPPING_EDITS.lock().unwrap() = prof_idx
            .map(|i| cfg.mouse.profiles[i].button_mappings.clone())
            .unwrap_or_default();

        render_mapping_rows_locked(ui);

        // 重建设备下拉框(每次打开设置时刷新,反映热插拔)。
        // Rebuild the device popup (refreshed on each settings open to reflect hot-plug).
        rebuild_device_popup(ui);

        // 根据 enable_mouse 状态冻结/解冻下方控件。
        // Freeze/unfreeze the controls below based on the enable_mouse state.
        update_mouse_controls_enabled(ui);
        // 根据滚动模式刷新行数行的条件显隐。
        // Refresh the conditional visibility of the lines-per-tick row by mode.
        update_mode_dependent_visibility(ui);

        // ===== 剪贴板历史页:填充全局配置 =====
        // Clipboard page: populate from the global config.
        let _: () = msg_send![
            ui.clipboard_enabled,
            setState: if cfg.clipboard.enabled { 1isize } else { 0isize }
        ];
        let _: () = msg_send![
            ui.clipboard_persist,
            setState: if cfg.clipboard.persist { 1isize } else { 0isize }
        ];
        let _: () = msg_send![
            ui.clipboard_show_source_app,
            setState: if cfg.clipboard.show_source_app { 1isize } else { 0isize }
        ];
        let _: () = msg_send![
            ui.clipboard_move_used_to_top,
            setState: if cfg.clipboard.move_used_to_top { 1isize } else { 0isize }
        ];
        set_field(
            ui.clipboard_max_entries,
            cfg.clipboard.max_entries.to_string(),
        );
        set_field(
            ui.clipboard_auto_expire_days,
            cfg.clipboard.auto_expire_days.to_string(),
        );
        // picker_position:下拉框 index 0 = 跟随鼠标(mouse), 1 = 主屏幕居中(main)。
        // picker_position: popup index 0 = follow mouse (mouse), 1 = centered (main).
        let pos_idx = match cfg.clipboard.picker_position.as_str() {
            "main" => 1,
            _ => 0, // "mouse" (default)
        };
        let _: () = msg_send![ui.clipboard_picker_position, selectItemAtIndex: pos_idx as isize];
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
    // 映射总开关:用有效值(合并"所有鼠标"档后的生效值)。
    // The mappings master switch: the effective value (merged across profiles).
    let _: () = msg_send![ui.mapping_enabled, setState: if resolved.button_mappings_enabled { 1isize } else { 0isize }];
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
        // windows_enabled / show_minimized:switch state(1=on / 0=off)。
        // windows_enabled / show_minimized: switch state (1=on / 0=off).
        let we_state: isize = msg_send![ui.windows_enabled, state];
        cfg.windows.enabled = we_state == 1;
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
        // 按键映射:编辑态写回当前设备的专属 profile(整体替换)。
        // Button mappings: the in-edit state replaces the selected device's own profile.
        p.button_mappings = MAPPING_EDITS.lock().unwrap().clone();
        // 映射总开关:写当前设备档(不同鼠标可以不同值)。
        // The mappings master switch: written to the current device's profile (per-device).
        let me_state: isize = msg_send![ui.mapping_enabled, state];
        p.button_mappings_enabled = Some(me_state == 1);

        // ===== 剪贴板历史页(全局配置,不随设备)=====
        // Clipboard page (global config, not per-device).
        let cb_state: isize = msg_send![ui.clipboard_enabled, state];
        cfg.clipboard.enabled = cb_state == 1;
        let persist_state: isize = msg_send![ui.clipboard_persist, state];
        cfg.clipboard.persist = persist_state == 1;
        let src_state: isize = msg_send![ui.clipboard_show_source_app, state];
        cfg.clipboard.show_source_app = src_state == 1;
        let move_top_state: isize = msg_send![ui.clipboard_move_used_to_top, state];
        cfg.clipboard.move_used_to_top = move_top_state == 1;
        match parse_usize(&nsstring_to_rust(msg_send![
            ui.clipboard_max_entries,
            stringValue
        ])) {
            Ok(v) => cfg.clipboard.max_entries = v as u32,
            Err(_) => errs.push(tf(
                "errors.not_an_integer",
                &[("field", "clipboard.max_entries")],
            )),
        }
        match parse_usize(&nsstring_to_rust(msg_send![
            ui.clipboard_auto_expire_days,
            stringValue
        ])) {
            Ok(v) => cfg.clipboard.auto_expire_days = v as u32,
            Err(_) => errs.push(tf(
                "errors.not_an_integer",
                &[("field", "clipboard.auto_expire_days")],
            )),
        }
        // picker_position:下拉框 index 0 = 跟随鼠标, 1 = 主屏幕居中。
        // picker_position: popup index 0 = follow mouse, 1 = centered on the main screen.
        let pos_idx: isize = msg_send![ui.clipboard_picker_position, indexOfSelectedItem];
        cfg.clipboard.picker_position = match pos_idx {
            1 => "main",
            _ => "mouse", // index 0 or out-of-range
        }
        .into();
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
            sidebar_clipboard: std::ptr::null_mut(),
            sidebar_highlight: std::ptr::null_mut(),
            general_view: std::ptr::null_mut(),
            experimental_view: std::ptr::null_mut(),
            mouse_view: std::ptr::null_mut(),
            clipboard_view: std::ptr::null_mut(),
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
            windows_enabled: std::ptr::null_mut(),
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
            mapping_scroll: std::ptr::null_mut(),
            mapping_doc: std::ptr::null_mut(),
            mapping_rows: Vec::new(),
            clipboard_enabled: std::ptr::null_mut(),
            clipboard_persist: std::ptr::null_mut(),
            clipboard_move_used_to_top: std::ptr::null_mut(),
            clipboard_max_entries: std::ptr::null_mut(),
            clipboard_auto_expire_days: std::ptr::null_mut(),
            clipboard_show_source_app: std::ptr::null_mut(),
            clipboard_picker_position: std::ptr::null_mut(),
            add_mapping_button: std::ptr::null_mut(),
            mapping_enabled: std::ptr::null_mut(),
            mapping_empty: std::ptr::null_mut(),
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
        ui.sidebar_clipboard = make_sidebar_button(
            sidebar_view,
            target,
            &t("settings.sidebar_clipboard"),
            3,
            12.0,
            btn_y0 - 102.0,
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

        // --- 剪贴板历史页容器 clipboard page container(初始隐藏 / initially hidden)---
        let clipboard_view: *mut AnyObject = msg_send![class!(NSView), alloc];
        let clipboard_view: *mut AnyObject = msg_send![clipboard_view, initWithFrame: NSRect::new(NSPoint::new(content_x, 0.0), NSSize::new(content_w, content_h))];
        let _: () = msg_send![clipboard_view, setHidden: true];
        let _: () = msg_send![clipboard_view, setAutoresizingMask: 18u64]; // 同 general_view:宽高拉伸
        let _: () = msg_send![content, addSubview: clipboard_view];
        release_obj(clipboard_view);
        ui.clipboard_view = clipboard_view;

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
        // 窗口切换总开关:关闭后 Cmd+Tab 透传给系统(原生切换器接管)。
        // App-switcher master switch: off = Cmd+Tab passes through to the system.
        ui.windows_enabled = add_row(
            general_view,
            label_x,
            y,
            label_w,
            row_h,
            &t("settings.row_windows_enabled"),
            make_switch(ctrl_x + ctrl_w, y, row_h, false),
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
        // 右对齐:Apple Silicon 上 NSTextAlignment 走 iOS 值分支,Right=2(1 是 Center)。
        // Right-aligned: on Apple Silicon NSTextAlignment uses the iOS values, Right=2 (1
        // is Center).
        let _: () = msg_send![value_label, setAlignment: 2isize]; // NSTextAlignmentRight on arm64
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

        // --- 按键映射 Button Mappings ---
        // 绑定区:header + 滚动列表(固定高度,行溢出时滚动)+ 录制提示 + 添加按钮。
        // 动态行由 render_mapping_rows 填充(mapping_doc 为 flipped 文档视图)。
        // Button mappings: header + scroll list (fixed height, scrolls when rows overflow) +
        // a recording hint + the add button. Rows are filled by render_mapping_rows (mapping_doc
        // is a flipped document view).
        y -= 14.0 + 24.0;
        add_header(
            mouse_view,
            &t("settings.header_mouse_mappings"),
            12.0,
            y,
            content_w - 24.0,
        );
        // 映射总开关(per-device):header 行右侧的开关(NSSwitch,与设置行同款)。
        // The mappings master switch (per-device): a switch at the header row's right
        // (an NSSwitch, same as the settings rows).
        let ms: *mut AnyObject = make_switch(ctrl_x + ctrl_w, y - 4.0, row_h, true);
        // 开关变化即时重渲染映射行(关闭时行控件置灰不可点)。
        // Re-render the mapping rows on toggle (rows grey out and become inert when off).
        let _: () = msg_send![ms, setTarget: target];
        let _: () = msg_send![ms, setAction: sel!(handleMappingEnabledChanged:)];
        let _: () = msg_send![mouse_view, addSubview: ms];
        release_obj(ms);
        ui.mapping_enabled = ms;
        y -= 8.0 + row_h;
        // 显式坐标布局(不再依赖游标链):卡片顶部 = 当前 y,卡片 [top-list_h, top]。
        // Explicit coordinates (no cursor chain): card top = current y, card spans
        // [top-list_h, top] (the content view is NOT flipped, so a frame origin is the
        // bottom edge).
        let list_h = MAPPING_ROW_H * 3.0;
        // 卡片顶部 = 当前 y(非 flipped,origin 是底部)。
        // Card top = current y (not flipped; a frame origin is the bottom edge).
        let card_bottom = y - list_h;
        let card_w = content_w - 24.0;
        // 卡片底色:一块独立的圆角背景视图,先 add(mouse_view 上),scroll 盖在它上面。
        // NSScrollView 的 clipView 白底(及其 layer 透明化)实测都不可靠,干脆在 scroll
        // 底下垫一块圆角底色,scroll 自身完全透明 —— 与 clip 的绘制彻底解耦。
        // The card color is a standalone rounded background view added FIRST (on mouse_view),
        // with the scroll view on top. The clip view's white fill (and its layer
        // transparency) proved unreliable, so a rounded backdrop sits under the scroll and
        // the scroll itself is fully transparent -- decoupled from the clip's drawing.
        // 卡片:一块圆角底色视图,行直接铺在它上面(flipped,行从顶排)。
        // 不用 NSScrollView —— 它的 clipView 白底反复盖住底色(所有 layer 透明手段
        // 实测都不可靠),普通视图平铺干净利落;行数少(最多 7 个按钮)无需滚动,
        // 溢出由圆角 + masksToBounds 裁掉。
        // The card is one rounded background view with the rows laid directly on it
        // (flipped; rows stack from the top). No NSScrollView -- its clip view's white fill
        // kept hiding the background (every layer-transparency trick proved unreliable);
        // plain views are clean. Row counts are small (≤ 7 buttons) so scrolling is
        // unnecessary; overflow is clipped by the rounded corner + masksToBounds.
        // 卡片背景:NSVisualEffectView(material 自绘,跟随主题)。
        // 不要用 NSView+layer 的 controlBackgroundColor —— 动态系统色的 CGColor 为 nil,
        // layer 底色不渲染(实测);material 由控件自绘,不依赖 layer 色。
        // Card background: NSVisualEffectView (material drawn by the control, theme-aware).
        // Don't use NSView+layer with controlBackgroundColor -- dynamic system colors give
        // a nil CGColor and the layer fill never renders (verified); the material is drawn
        // by the control itself, independent of layer colors.
        let card_bg: *mut AnyObject = msg_send![class!(NSVisualEffectView), alloc];
        let card_bg: *mut AnyObject = msg_send![card_bg, initWithFrame: NSRect::new(NSPoint::new(label_x, card_bottom), NSSize::new(card_w, list_h))];
        let _: () = msg_send![card_bg, setBlendingMode: 1u64]; // WithinWindow
        let _: () = msg_send![card_bg, setMaterial: 0u64]; // NSVisualEffectMaterialAppearanceBased(跟随主题)
        let _: () = msg_send![card_bg, setState: 1u64]; // Active
        let _: () = msg_send![card_bg, setFlipped: true];
        let _: () = msg_send![card_bg, setWantsLayer: true];
        let bg_layer: *mut AnyObject = msg_send![card_bg, layer];
        let _: () = msg_send![bg_layer, setCornerRadius: 8.0f64];
        let _: () = msg_send![bg_layer, setMasksToBounds: true];
        let _: () = msg_send![mouse_view, addSubview: card_bg];
        release_obj(card_bg);
        ui.mapping_scroll = std::ptr::null_mut();
        ui.mapping_doc = card_bg;
        // 空状态提示(行数为 0 时显示在卡片内;render 时控制显隐)。
        // Empty-state hint (shown inside the card when there are no rows; toggled by render).
        let empty: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let empty: *mut AnyObject = msg_send![empty, initWithFrame: NSRect::new(NSPoint::new(label_x, card_bottom), NSSize::new(card_w, list_h))];
        set_field(empty, 0);
        let _: () = msg_send![empty, setBezeled: false];
        let _: () = msg_send![empty, setDrawsBackground: false];
        let _: () = msg_send![empty, setEditable: false];
        let _: () = msg_send![empty, setAlignment: 1isize]; // center
        let empty_ns = make_nsstring(&t("settings.mapping_empty"));
        let _: () = msg_send![empty, setStringValue: empty_ns];
        CFRelease(empty_ns as *const c_void);
        let empty_color: *mut AnyObject = msg_send![class!(NSColor), secondaryLabelColor];
        let _: () = msg_send![empty, setTextColor: empty_color];
        let _: () = msg_send![empty, setHidden: true];
        let _: () = msg_send![card_bg, addSubview: empty];
        release_obj(empty);
        ui.mapping_empty = empty;
        // 添加按钮:卡片底部下方 14pt(显式坐标,origin = 底部边)。
        // Add button: 14pt below the card bottom (explicit coordinate; origin = bottom edge).
        let btn_bottom = card_bottom - 14.0 - row_h;
        let add_btn: *mut AnyObject = msg_send![class!(NSButton), alloc];
        let add_btn: *mut AnyObject = msg_send![add_btn, initWithFrame: NSRect::new(NSPoint::new(label_x, btn_bottom), NSSize::new(card_w, row_h))];
        let _: () = msg_send![add_btn, setBezelStyle: 2isize]; // NSRoundedBezelStyle
        let add_title = make_nsstring(&t("settings.row_add_mapping"));
        let _: () = msg_send![add_btn, setTitle: add_title];
        CFRelease(add_title as *const c_void);
        let _: () = msg_send![add_btn, setTarget: target];
        let _: () = msg_send![add_btn, setAction: sel!(handleAddMapping:)];
        let _: () = msg_send![mouse_view, addSubview: add_btn];
        release_obj(add_btn);
        ui.add_mapping_button = add_btn;
        // 初始渲染当前设备的映射。
        // Render the current device's mappings initially.
        render_mapping_rows();

        // ===== 剪贴板历史页内容 clipboard page content =====
        // 独立布局游标(该页内容与鼠标页互不相关)。
        // Independent layout cursor (this page's content is unrelated to the mouse page).
        let mut cy = layout_h - 12.0;
        add_header(
            clipboard_view,
            &t("settings.header_clipboard"),
            12.0,
            cy - 18.0,
            content_w - 24.0,
        );
        // header 与首行间距与其他页一致(8 + row_h = 30):此前 16pt 挨得太近。
        // Header-to-first-row gap matches the other pages (8 + row_h = 30); it used to be
        // 16pt, too cramped.
        cy -= 18.0 + 8.0 + row_h;
        // 启用开关 / master switch.
        // 启用开关 / master switch.
        // 英文 "Enable clipboard history"(实测 146pt)+ cell 内边距在 label_w=150 边缘,
        // 与 persist/move_used_to_top 行一起加宽到 225(见下方注释)。
        // English "Enable clipboard history" (measured 146pt) plus cell padding sits on
        // the label_w=150 edge; widen to 225 along with the persist/move_used_to_top rows.
        ui.clipboard_enabled = add_row(
            clipboard_view,
            label_x,
            cy,
            225.0,
            row_h,
            &t("settings.row_clipboard_enabled"),
            make_switch(ctrl_x + ctrl_w, cy, row_h, false),
        );
        cy -= 8.0 + row_h;
        // 悬浮窗位置下拉框:项 = [跟随鼠标, 始终显示在主屏幕正中间];默认 index 0(跟随鼠标)。
        // Picker-position popup: items = [Follow Mouse, Always Center on Main Screen];
        // default index 0 (follow mouse).
        let pos_labels = [
            t("settings.picker_position_follow_mouse"),
            t("settings.picker_position_main_screen"),
        ];
        let pos_label_refs: Vec<&str> = pos_labels.iter().map(|s| s.as_str()).collect();
        ui.clipboard_picker_position = add_row(
            clipboard_view,
            label_x,
            cy,
            225.0,
            row_h,
            &t("settings.row_clipboard_picker_position"),
            make_popup(ctrl_x, cy, ctrl_w, row_h, &pos_label_refs, 0),
        );
        cy -= 8.0 + row_h;
        // 保存历史开关(持久化到磁盘,重启不丢;明文落盘,隐私风险见 README)。
        // Persist switch (saved to disk, survives restarts; plaintext on disk -- the
        // privacy implications are documented in the README).
        // 保存历史开关(持久化到磁盘,重启不丢;明文落盘,隐私风险见 README)。
        // 中文标签"保存剪贴板历史记录到磁盘"(11 字)与英文 "Save clipboard history
        // to disk" 都超出默认 label_w=150(渲染截断),该行加宽到 225——与
        // show_minimized 行同款处理;开关仍右对齐到 popup 右缘,不重叠。
        // Persist switch (saved to disk, survives restarts; plaintext on disk -- the
        // privacy implications are documented in the README). The Chinese (11 CJK
        // chars) and English labels both exceed the default label_w=150 (rendered
        // truncated), so this row widens its label to 225 -- same as the
        // show_minimized row; the switch still right-aligns to the popups' right
        // edge, no overlap.
        ui.clipboard_persist = add_row(
            clipboard_view,
            label_x,
            cy,
            225.0,
            row_h,
            &t("settings.row_clipboard_persist"),
            make_switch(ctrl_x + ctrl_w, cy, row_h, false),
        );
        cy -= 8.0 + row_h;
        // 显示来源应用 / show the source app.
        ui.clipboard_show_source_app = add_row(
            clipboard_view,
            label_x,
            cy,
            label_w,
            row_h,
            &t("settings.row_clipboard_show_source_app"),
            make_switch(ctrl_x + ctrl_w, cy, row_h, false),
        );
        cy -= 8.0 + row_h;
        // 使用后移到最前(粘贴是否重排历史;默认开 = 保持现状)。
        // Move used entries to the top (whether pasting reorders the history; on by
        // default = current behavior).
        // 英文 "Move used entries to top"(实测 150.3pt)超出 label_w=150 渲染截断
        // (用户切英文后看到 "move used entries to"),加宽到 225。
        // English "Move used entries to top" (measured 150.3pt) exceeds label_w=150 and
        // rendered truncated ("move used entries to" after switching to English), widened
        // to 225.
        ui.clipboard_move_used_to_top = add_row(
            clipboard_view,
            label_x,
            cy,
            225.0,
            row_h,
            &t("settings.row_clipboard_move_used_to_top"),
            make_switch(ctrl_x + ctrl_w, cy, row_h, false),
        );
        cy -= 8.0 + row_h;
        // 最大条数(数字输入)/ max entries (number input).
        ui.clipboard_max_entries = add_row(
            clipboard_view,
            label_x,
            cy,
            label_w,
            row_h,
            &t("settings.row_clipboard_max_entries"),
            make_text_input(ctrl_x, cy, ctrl_w, row_h, "50"),
        );
        cy -= 8.0 + row_h;
        // 自动过期天数(数字输入,0 = 关闭)/ auto-expire days (number input, 0 = off).
        ui.clipboard_auto_expire_days = add_row(
            clipboard_view,
            label_x,
            cy,
            label_w,
            row_h,
            &t("settings.row_clipboard_auto_expire_days"),
            make_text_input(ctrl_x, cy, ctrl_w, row_h, "30"),
        );
        cy -= 8.0 + row_h;
        // 呼出快捷键说明(只读 label)/ shortcut hint (read-only label).
        let hint: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let hint: *mut AnyObject = msg_send![hint, initWithFrame: NSRect::new(NSPoint::new(label_x, cy), NSSize::new(content_w - 24.0, row_h))];
        set_field(hint, t("settings.row_clipboard_shortcut"));
        let _: () = msg_send![hint, setBezeled: false];
        let _: () = msg_send![hint, setDrawsBackground: false];
        let _: () = msg_send![hint, setEditable: false];
        let _: () = msg_send![clipboard_view, addSubview: hint];
        release_obj(hint);

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
