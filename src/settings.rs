//! 设置窗口:SettingsUi 状态、控件构造器(text/popup/header/row)、窗口构建/显示/收集、
//! 校验告警、以及配置热应用(apply_config_refresh)。invalidate_settings_window 作废缓存
//! 窗口供 locale 变更后重建。
//!
//! Settings window: SettingsUi state, control builders (text/popup/header/row), window
//! build/show/collect, validation alerts, and hot config application (apply_config_refresh).
//! invalidate_settings_window drops the cached window so it rebuilds after a locale change.

use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSEdgeInsets, NSPoint, NSRect, NSSize};
use std::collections::HashMap;
use std::ffi::{c_void, CString};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
    sidebar_switcher: *mut AnyObject, // NSButton: 应用切换浮窗 / App switcher overlay (tag=1)
    sidebar_mouse: *mut AnyObject,   // NSButton: 鼠标控制 / Mouse (tag=2)
    sidebar_clipboard: *mut AnyObject, // NSButton: 剪贴板历史 / Clipboard history (tag=3)
    sidebar_about: *mut AnyObject,   // NSButton: 关于 / About (tag=4)
    sidebar_highlight: *mut AnyObject, // NSView: 选中行高亮背景 (layer-backed)
    general_view: *mut AnyObject,    // NSView: 通用页容器 / General page container
    switcher_view: *mut AnyObject,   // NSView: 应用切换浮窗页容器 / App switcher page container
    mouse_view: *mut AnyObject,      // NSView: 鼠标页容器 / Mouse page container
    clipboard_view: *mut AnyObject,  // NSView: 剪贴板历史页容器 / Clipboard page container
    about_view: *mut AnyObject,      // NSView: 关于页容器 / About page container
    glass_style: *mut AnyObject,     // NSPopUpButton: regular / clear
    glass_tint: *mut AnyObject,      // NSColorWell: 玻璃颜色 / glass tint
    glass_preview_switcher: *mut AnyObject, // NSGlassEffectView: app switcher preview
    glass_preview_clipboard: *mut AnyObject, // NSGlassEffectView: clipboard preview
    corner_radius: *mut AnyObject,   // NSTextField
    modifier: *mut AnyObject,        // NSPopUpButton: option / command
    locale: *mut AnyObject,          // NSPopUpButton: auto / en / zh-Hans / zh-Hant
    show_minimized: *mut AnyObject,  // NSSwitch: 显示最小化窗口 / show minimized windows
    thumbnails_enabled: *mut AnyObject, // NSPopUpButton: 窗口显示模式 / window display mode
    windows_enabled: *mut AnyObject, // NSSwitch: 窗口切换总开关 / app-switcher master switch
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
    mapping_card: *mut AnyObject, // NSVisualEffectView: 按键映射外层卡片 / the mappings outer card
    mapping_panel: *mut AnyObject, // NSView: 嵌套圆角表格面板 / the nested rounded table panel
    mapping_rows: Vec<MappingRow>, // 动态绑定行(标签 + 删除按钮)/ live binding rows
    clipboard_enabled: *mut AnyObject, // NSSwitch: 启用剪贴板历史 / enable clipboard history
    clipboard_persist: *mut AnyObject, // NSSwitch: 保存剪贴板历史记录到磁盘 / persist clipboard history
    clipboard_move_used_to_top: *mut AnyObject, // NSSwitch: 使用后移到最前 / move used entries to top
    clipboard_max_entries: *mut AnyObject,      // NSTextField: 历史最大条数 / max history entries
    clipboard_auto_expire_days: *mut AnyObject, // NSTextField: 自动过期天数(0=关闭)/ auto-expire days (0 = off)
    clipboard_show_source_app: *mut AnyObject,  // NSSwitch: 显示来源应用 / show the source app
    clipboard_pin_follow: *mut AnyObject, // NSPopUpButton: 置顶后选中项位置 / selection after pin
    // (follow the pinned entry / keep current position)
    add_mapping_button: *mut AnyObject, // NSButton: 添加映射 / add-mapping button
    mapping_enabled: *mut AnyObject, // NSSwitch: 按键映射总开关(per-device) / mappings master switch (per-device)
    mapping_empty: *mut AnyObject,   // NSTextField: 空状态提示(卡片内) / empty-state hint (in-card)
    device_indicator: *mut AnyObject, // NSButton: 当前选中设备指示器(点击打开选择器) / device indicator (opens picker)
    ok_button: *mut AnyObject,        // NSButton: 确认按钮 / OK button
    accessibility_warning_view: *mut AnyObject, // NSView: 缺权限警告条容器 / permission-warning banner container
    update_auto_check: *mut AnyObject, // NSSwitch: Sparkle 自动检查开关 / Sparkle auto-check switch
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
    separator: *mut AnyObject,
    caps: Vec<*mut AnyObject>,
}
unsafe impl Send for MappingRow {}

/// 映射区行高(独立于全局 row_h;build 的卡片高度与 render 共用)。
/// Mapping-row height (independent of the global row_h; shared by the card height in build
/// and by render).
const MAPPING_HEADER_H: f64 = 32.0;
const MAPPING_ROW_H: f64 = 38.0;

// 嵌套映射表格(HTML `.mapping-table`)的布局参数。
// Layout constants for the nested mapping table (HTML `.mapping-table`).
const MAPPING_PANEL_X: f64 = 10.0; // 子表格在外层卡片内的水平内缩 / sub-table horizontal inset
const MAPPING_PANEL_TOP: f64 = 10.0; // 子表格顶部内缩 / sub-table top padding
const MAPPING_CELL_X: f64 = 12.0; // 行内容在子表格内的左内边距 / row content left padding
const MAPPING_ACTION_TOP: f64 = 12.0; // 添加按钮上方间距 / gap above the add-mapping button
const MAPPING_ACTION_H: f64 = 34.0; // 添加按钮高度 / add-mapping button height
const MAPPING_CARD_PAD_BOT: f64 = 10.0; // 卡片底部内边距 / card bottom padding

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

/// 程序同步 color well / color panel 时抑制回调重入。
/// Suppresses callback re-entry while synchronizing the color well and color panel in code.
static GLASS_UI_UPDATE: AtomicBool = AtomicBool::new(false);

/// 颜色面板与设置窗口的组合布局状态;详情面板同样只在打开时记录主窗原始位置。
/// Group-layout state for the color panel and settings window; like the detail panel, it stores
/// the main window's original position only while the group is open.
static GLASS_TINT_GROUP_ORIGINAL_ORIGIN: Mutex<Option<NSPoint>> = Mutex::new(None);
static GLASS_TINT_PANEL_OBSERVER_INSTALLED: AtomicBool = AtomicBool::new(false);

const GLASS_TINT_GROUP_GAP: f64 = 8.0;
const GLASS_TINT_SCREEN_MARGIN: f64 = 8.0;

struct GlassTintWellClass(*mut AnyObject);
unsafe impl Send for GlassTintWellClass {}
unsafe impl Sync for GlassTintWellClass {}

static GLASS_TINT_WELL_CLASS: OnceLock<GlassTintWellClass> = OnceLock::new();

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

/// Apply the HTML button surface to native NSButton instances.
unsafe fn style_html_button(button: *mut AnyObject, background_hex: u32, text_hex: u32) {
    let _: () = msg_send![button, setBezelStyle: 0isize];
    let _: () = msg_send![button, setBordered: false];
    let _: () = msg_send![button, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![button, layer];
    if !layer.is_null() {
        layer_set_background(layer, crate::ffi::hex_to_cg_color(background_hex));
        crate::ffi::layer_set_border(layer, crate::ffi::hex_to_cg_color(0x00000012u32));
        let _: () = msg_send![layer, setBorderWidth: 1.0f64];
        let _: () = msg_send![layer, setCornerRadius: 8.0f64];
        let _: () = msg_send![layer, setMasksToBounds: true];
    }
    let text_color = crate::ffi::hex_to_ns_color(text_hex);
    let _: () = msg_send![button, setContentTintColor: text_color];
}

struct HtmlActionButtonClass(*mut AnyObject);
unsafe impl Send for HtmlActionButtonClass {}
unsafe impl Sync for HtmlActionButtonClass {}

static HTML_ACTION_BUTTON_CLASS: OnceLock<HtmlActionButtonClass> = OnceLock::new();

fn html_action_button_class() -> *mut AnyObject {
    HTML_ACTION_BUTTON_CLASS
        .get_or_init(|| unsafe {
            let name = CString::new("OhMyTabHtmlActionButton").unwrap();
            let superclass = class!(NSButton) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(mouseEntered:),
                html_action_button_mouse_entered as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseExited:),
                html_action_button_mouse_exited as *mut c_void,
                types.as_ptr(),
            );
            objc_registerClassPair(cls);
            HtmlActionButtonClass(cls)
        })
        .0
}

extern "C" fn html_action_button_mouse_entered(this: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    unsafe {
        let button = this as *mut AnyObject;
        let tag: isize = msg_send![button, tag];
        let hover = match tag {
            -2 => 0x0077EDFFu32, // HTML footer `.ok:hover`
            -1 => 0x76768024u32, // HTML footer `button:hover`
            _ => 0x7676802Bu32,  // HTML small/tiny/full action hover
        };
        let layer: *mut AnyObject = msg_send![button, layer];
        if !layer.is_null() {
            layer_set_background(layer, crate::ffi::hex_to_cg_color(hover));
        }
    }
}

extern "C" fn html_action_button_mouse_exited(this: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    unsafe {
        let button = this as *mut AnyObject;
        let tag: isize = msg_send![button, tag];
        let normal = match tag {
            -2 => 0x0A84FFFFu32,
            -1 => 0xFFFFFFC7u32,
            -3 => 0x7676801Eu32, // HTML `.full-action`
            _ => 0xFFFFFFADu32,
        };
        let layer: *mut AnyObject = msg_send![button, layer];
        if !layer.is_null() {
            layer_set_background(layer, crate::ffi::hex_to_cg_color(normal));
        }
    }
}

/// 创建设置窗口统一使用的原生圆角操作按钮;调用方负责尺寸、自适应和父视图归属。
/// Create the native rounded action button shared by Settings; callers own frame, autoresizing,
/// and parent-view placement.
unsafe fn make_settings_action_button(
    frame: NSRect,
    title: &str,
    target: *mut AnyObject,
    action: Sel,
) -> *mut AnyObject {
    let button: *mut AnyObject = msg_send![html_action_button_class(), alloc];
    let button: *mut AnyObject = msg_send![button, initWithFrame: frame];
    set_control_title(button, title);
    let _: () = msg_send![button, setControlSize: 0isize]; // NSControlSizeRegular
                                                           // HTML .small-btn / footer buttons: translucent white surface with a hairline border.
    style_html_button(button, 0xFFFFFFADu32, 0x2E2E2EFFu32);
    let tracking: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
    let tracking: *mut AnyObject = msg_send![
        tracking,
        initWithRect: NSRect::new(NSPoint::new(0.0, 0.0), frame.size),
        options: 0x01u64 | 0x80u64 | 0x200u64,
        owner: button,
        userInfo: std::ptr::null::<AnyObject>()
    ];
    let _: () = msg_send![button, addTrackingArea: tracking];
    release_obj(tracking);
    let _: () = msg_send![button, setTarget: target];
    let _: () = msg_send![button, setAction: action];
    button
}

struct WebsiteLinkButtonClass(*mut AnyObject);
unsafe impl Send for WebsiteLinkButtonClass {}
unsafe impl Sync for WebsiteLinkButtonClass {}

static WEBSITE_LINK_BUTTON_CLASS: OnceLock<WebsiteLinkButtonClass> = OnceLock::new();
static SIDEBAR_BUTTON_CLASS: OnceLock<SidebarButtonClass> = OnceLock::new();
static SIDEBAR_SELECTED: AtomicUsize = AtomicUsize::new(0);
static SIDEBAR_TITLE_LABELS: LazyLock<Mutex<HashMap<usize, ObjPtr>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static SIDEBAR_ICON_VIEWS: LazyLock<Mutex<HashMap<usize, ObjPtr>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

extern "C" fn website_link_mouse_entered(this: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    unsafe {
        let color: *mut AnyObject = msg_send![class!(NSColor), systemBlueColor];
        let _: () = msg_send![this as *mut AnyObject, setTextColor: color];
        let cursor: *mut AnyObject = msg_send![class!(NSCursor), pointingHandCursor];
        let _: () = msg_send![cursor, set];
    }
}

extern "C" fn website_link_mouse_exited(this: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    unsafe {
        let color: *mut AnyObject = msg_send![class!(NSColor), linkColor];
        let _: () = msg_send![this as *mut AnyObject, setTextColor: color];
        let cursor: *mut AnyObject = msg_send![class!(NSCursor), arrowCursor];
        let _: () = msg_send![cursor, set];
    }
}

extern "C" fn website_link_mouse_down(_this: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    handle_open_official_website(
        std::ptr::null_mut(),
        sel!(handleOpenOfficialWebsite:),
        std::ptr::null_mut(),
    );
}

struct SidebarButtonClass(*mut AnyObject);
unsafe impl Send for SidebarButtonClass {}
unsafe impl Sync for SidebarButtonClass {}

extern "C" fn sidebar_button_mouse_entered(this: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    unsafe {
        let button = this as *mut AnyObject;
        let tag: isize = msg_send![button, tag];
        if tag >= 0 && tag as usize == SIDEBAR_SELECTED.load(Ordering::SeqCst) {
            return;
        }
        let layer: *mut AnyObject = msg_send![button, layer];
        if !layer.is_null() {
            layer_set_background(layer, crate::ffi::hex_to_cg_color(0x76768014u32));
        }
    }
}

extern "C" fn sidebar_button_mouse_exited(this: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    unsafe {
        let button = this as *mut AnyObject;
        let layer: *mut AnyObject = msg_send![button, layer];
        if !layer.is_null() {
            layer_set_background(layer, crate::ffi::hex_to_cg_color(0x00000000u32));
        }
    }
}

fn sidebar_button_class() -> *mut AnyObject {
    SIDEBAR_BUTTON_CLASS
        .get_or_init(|| unsafe {
            let name = CString::new("OhMyTabSidebarButton").unwrap();
            let superclass = class!(NSButton) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(mouseEntered:),
                sidebar_button_mouse_entered as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseExited:),
                sidebar_button_mouse_exited as *mut c_void,
                types.as_ptr(),
            );
            objc_registerClassPair(cls);
            SidebarButtonClass(cls)
        })
        .0
}

fn website_link_button_class() -> *mut AnyObject {
    WEBSITE_LINK_BUTTON_CLASS
        .get_or_init(|| unsafe {
            let name = CString::new("OhMyTabWebsiteLinkButton").unwrap();
            let superclass = class!(NSTextField) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(mouseDown:),
                website_link_mouse_down as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseEntered:),
                website_link_mouse_entered as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseExited:),
                website_link_mouse_exited as *mut c_void,
                types.as_ptr(),
            );
            objc_registerClassPair(cls);
            WebsiteLinkButtonClass(cls)
        })
        .0
}

/// 用一个数值/字符串填进文本框,并释放临时 NSString。
/// Set a text field's value from anything Displayable, releasing the temp NSString.
unsafe fn set_field(field: *mut AnyObject, val: impl std::fmt::Display) {
    let s = format!("{}", val);
    let ns = make_nsstring(&s);
    let _: () = msg_send![field, setStringValue: ns];
    CFRelease(ns as *const c_void);
}

/// NSTextFieldCell keeps a fixed baseline for single-line controls.  Our settings rows are
/// taller than that standard control height, so use a small cell subclass that gives AppKit a
/// centered 22pt drawing/editing rect inside the full row.  Both paths are overridden because
/// the field editor is laid out by `selectWithFrame:` rather than by the drawing method.
unsafe fn centered_text_field_cell_class() -> *mut AnyObject {
    static CELL_CLASS: OnceLock<ObjPtr> = OnceLock::new();
    CELL_CLASS
        .get_or_init(|| {
            let name = CString::new("OhMyTabCenteredTextFieldCell").unwrap();
            let superclass = class!(NSTextFieldCell) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let draw_types = CString::new("v@:{CGRect=dddd}@").unwrap();
            class_addMethod(
                cls,
                sel!(drawInteriorWithFrame:inView:),
                centered_text_field_cell_draw_interior as *mut c_void,
                draw_types.as_ptr(),
            );
            let select_types = CString::new("v@:{CGRect=dddd}@@@@qq").unwrap();
            class_addMethod(
                cls,
                sel!(selectWithFrame:inView:editor:delegate:start:length:),
                centered_text_field_cell_select as *mut c_void,
                select_types.as_ptr(),
            );
            objc_registerClassPair(cls);
            ObjPtr(cls)
        })
        .0
}

fn centered_text_field_cell_frame(bounds: NSRect) -> NSRect {
    let text_h = bounds.size.height.min(22.0);
    // The cell draws its baseline a few points above the geometric center of the 22pt rect, so
    // after centering the rect in the taller settings row a POSITIVE offset shifts the drawing
    // rect DOWN to compensate (the old +1 left the glyphs a touch high; a negative value pushed
    // them further up).
    let baseline_offset = if bounds.size.height > text_h {
        2.0
    } else {
        0.0
    };
    let horizontal_inset = bounds.size.width.min(8.0);
    NSRect::new(
        NSPoint::new(
            bounds.origin.x + horizontal_inset,
            bounds.origin.y + (bounds.size.height - text_h) / 2.0 + baseline_offset,
        ),
        NSSize::new(
            (bounds.size.width - horizontal_inset * 2.0).max(1.0),
            text_h,
        ),
    )
}

unsafe fn centered_text_field_cell_super_draw(cell: *mut c_void, rect: NSRect, view: *mut c_void) {
    #[repr(C)]
    struct ObjcSuper {
        receiver: *mut c_void,
        super_class: *mut c_void,
    }
    extern "C" {
        fn objc_msgSendSuper();
    }
    type F = unsafe extern "C" fn(*mut ObjcSuper, Sel, NSRect, *mut c_void) -> ();
    let super_class =
        objc2::runtime::AnyClass::get(c"NSTextFieldCell").unwrap() as *const _ as *mut c_void;
    let mut sup = ObjcSuper {
        receiver: cell,
        super_class,
    };
    let send: F = std::mem::transmute(objc_msgSendSuper as *const ());
    send(&mut sup, sel!(drawInteriorWithFrame:inView:), rect, view);
}

extern "C" fn centered_text_field_cell_draw_interior(
    this: *mut c_void,
    _cmd: Sel,
    bounds: NSRect,
    view: *mut c_void,
) {
    unsafe {
        centered_text_field_cell_super_draw(this, centered_text_field_cell_frame(bounds), view);
    }
}

extern "C" fn centered_text_field_cell_select(
    this: *mut c_void,
    _cmd: Sel,
    bounds: NSRect,
    view: *mut c_void,
    editor: *mut c_void,
    delegate: *mut c_void,
    start: isize,
    length: isize,
) {
    unsafe {
        #[repr(C)]
        struct ObjcSuper {
            receiver: *mut c_void,
            super_class: *mut c_void,
        }
        extern "C" {
            fn objc_msgSendSuper();
        }
        type F = unsafe extern "C" fn(
            *mut ObjcSuper,
            Sel,
            NSRect,
            *mut c_void,
            *mut c_void,
            *mut c_void,
            isize,
            isize,
        ) -> ();
        let super_class =
            objc2::runtime::AnyClass::get(c"NSTextFieldCell").unwrap() as *const _ as *mut c_void;
        let mut sup = ObjcSuper {
            receiver: this,
            super_class,
        };
        let send: F = std::mem::transmute(objc_msgSendSuper as *const ());
        send(
            &mut sup,
            sel!(selectWithFrame:inView:editor:delegate:start:length:),
            centered_text_field_cell_frame(bounds),
            view,
            editor,
            delegate,
            start,
            length,
        );
    }
}

/// 可编辑文本框(alloc +1,由调用方持有或交给父视图后 release)。
/// Editable text field (alloc +1; caller owns or releases after adding to a parent).
unsafe fn make_text_input(x: f64, y: f64, w: f64, h: f64, value: &str) -> *mut AnyObject {
    let field: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let field: *mut AnyObject =
        msg_send![field, initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))];
    let ns = make_nsstring(value);
    let cell: *mut AnyObject = msg_send![centered_text_field_cell_class(), alloc];
    let cell: *mut AnyObject = msg_send![cell, initTextCell: ns];
    let _: () = msg_send![field, setCell: cell];
    release_obj(cell);
    let _: () = msg_send![field, setStringValue: ns];
    CFRelease(ns as *const c_void);
    let _: () = msg_send![field, setBezeled: false];
    // Replacing NSTextField's cell resets the field's editability flags on some macOS versions.
    // Restore them explicitly so a click still opens the field editor and accepts typing.
    let _: () = msg_send![field, setEditable: true];
    let _: () = msg_send![field, setSelectable: true];
    // The HTML input has no native focus ring; keep the caret while removing AppKit's
    // blue outline that otherwise appears around a borderless NSTextField when editing.
    let _: () = msg_send![field, setFocusRingType: 1isize]; // NSFocusRingTypeNone
                                                            // Treat the value as a single line so AppKit centers its baseline in the 34pt row,
                                                            // matching the vertical alignment of the popup controls beside it.
    let _: () = msg_send![field, setUsesSingleLineMode: true];
    // `scrollable` belongs to NSTextFieldCell rather than NSTextField.  A single-line,
    // scrollable cell uses AppKit's vertically centered editor layout; guard the selector so an
    // older macOS implementation cannot turn this styling hint into a startup crash.
    let cell: *mut AnyObject = msg_send![field, cell];
    if !cell.is_null() {
        let supports_scrollable: bool = msg_send![cell, respondsToSelector: sel!(setScrollable:)];
        if supports_scrollable {
            let _: () = msg_send![cell, setScrollable: true];
        }
    }
    let _: () = msg_send![field, setAlignment: 0isize]; // NSTextAlignmentLeft
                                                        // The rounded layer below is the sole background surface. Keeping the cell background off
                                                        // avoids a second, darker strip when the custom cell draws inside its centered rect.
    let _: () = msg_send![field, setDrawsBackground: false];
    let _: () = msg_send![field, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![field, layer];
    if !layer.is_null() {
        layer_set_background(layer, crate::ffi::hex_to_cg_color(0x7676801Cu32));
        let _: () = msg_send![layer, setCornerRadius: 9.0f64];
        let _: () = msg_send![layer, setMasksToBounds: true];
    }
    field
}

fn color_component_to_byte(component: f64) -> u8 {
    (component.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn rgba_hex_from_components(red: f64, green: f64, blue: f64, alpha: f64) -> String {
    format!(
        "{:02x}{:02x}{:02x}{:02x}",
        color_component_to_byte(red),
        color_component_to_byte(green),
        color_component_to_byte(blue),
        color_component_to_byte(alpha)
    )
}

/// 将任意 NSColor 转换到 sRGB 后编码为配置使用的 RRGGBBAA。
/// Convert any NSColor to sRGB and encode it as the RRGGBBAA format used by the config.
unsafe fn ns_color_to_hex(color: *mut AnyObject) -> Option<String> {
    if color.is_null() {
        return None;
    }
    let space: *mut AnyObject = msg_send![class!(NSColorSpace), sRGBColorSpace];
    let srgb: *mut AnyObject = msg_send![color, colorUsingColorSpace: space];
    if srgb.is_null() {
        return None;
    }
    let red: f64 = msg_send![srgb, redComponent];
    let green: f64 = msg_send![srgb, greenComponent];
    let blue: f64 = msg_send![srgb, blueComponent];
    let alpha: f64 = msg_send![srgb, alphaComponent];
    Some(rgba_hex_from_components(red, green, blue, alpha))
}

/// 原生取色器色块,固定为右侧小色块而不是拉伸成文本框宽度。
/// Native color well, kept as a compact right-side swatch instead of stretching like a text field.
unsafe fn make_color_well(
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    value: &str,
    target: *mut AnyObject,
) -> *mut AnyObject {
    let well: *mut AnyObject = msg_send![glass_tint_well_class(), alloc];
    let well: *mut AnyObject = msg_send![
        well,
        initWithFrame: NSRect::new(NSPoint::new(x + w - 52.0, y), NSSize::new(52.0, h))
    ];
    let _: () = msg_send![well, setColorWellStyle: 0isize]; // NSColorWellStyleDefault
    let _: () = msg_send![well, setBordered: true];
    let _: () = msg_send![well, setContinuous: true];
    let _: () = msg_send![well, setTarget: target];
    let _: () = msg_send![well, setAction: sel!(handleGlassTintChanged:)];
    let responds: bool = msg_send![well, respondsToSelector: sel!(setSupportsAlpha:)];
    if responds {
        let _: () = msg_send![well, setSupportsAlpha: true];
    }
    let color = crate::ffi::hex_to_ns_color(crate::config::parse_hex8(value));
    let _: () = msg_send![well, setColor: color];
    let _: () = msg_send![well, setAutoresizingMask: 0u64];
    well
}

/// 纯逻辑:水平居中设置窗口+颜色面板的整体,面板在设置窗口右侧并垂直居中。
/// Pure: center the settings window + color panel as one horizontal group, with the panel on
/// the right and vertically centered against the settings window.
fn glass_tint_group_frames(settings: NSRect, panel: NSRect, screen: NSRect) -> (NSRect, NSRect) {
    let group_w = settings.size.width + GLASS_TINT_GROUP_GAP + panel.size.width;
    let min_x = screen.origin.x + GLASS_TINT_SCREEN_MARGIN;
    let max_x = screen.origin.x + screen.size.width - GLASS_TINT_SCREEN_MARGIN;
    let group_x = if group_w + 2.0 * GLASS_TINT_SCREEN_MARGIN <= screen.size.width {
        (screen.origin.x + (screen.size.width - group_w) / 2.0)
            .max(min_x)
            .min(max_x - group_w)
    } else {
        min_x
    };

    let min_y = screen.origin.y + GLASS_TINT_SCREEN_MARGIN;
    let max_y = screen.origin.y + screen.size.height - GLASS_TINT_SCREEN_MARGIN - panel.size.height;
    let centered_y = settings.origin.y + (settings.size.height - panel.size.height) / 2.0;
    let panel_y = if max_y >= min_y {
        centered_y.max(min_y).min(max_y)
    } else {
        min_y
    };

    (
        NSRect::new(NSPoint::new(group_x, settings.origin.y), settings.size),
        NSRect::new(
            NSPoint::new(
                group_x + settings.size.width + GLASS_TINT_GROUP_GAP,
                panel_y,
            ),
            panel.size,
        ),
    )
}

/// 取设置窗口所在屏幕的可见区域;窗口尚未绑定屏幕时回退到主屏。
/// Get the visible frame of the settings window's screen, falling back to the main screen before
/// AppKit has assigned one.
unsafe fn glass_tint_screen_frame(window: *mut AnyObject) -> NSRect {
    let screen: *mut AnyObject = msg_send![window, screen];
    if screen.is_null() {
        let main: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
        msg_send![main, visibleFrame]
    } else {
        msg_send![screen, visibleFrame]
    }
}

/// 打开取色器前把设置窗口向左移,让两个窗口作为一个整体居中。
/// Move the settings window left before opening the color panel so the two windows are centered
/// as one group.
unsafe fn position_glass_tint_group(save_original: bool) {
    let window = match SETTINGS_UI.lock().unwrap().as_ref() {
        Some(ui) => ui.window,
        None => return,
    };
    let panel: *mut AnyObject = msg_send![class!(NSColorPanel), sharedColorPanel];
    if panel.is_null() {
        return;
    }

    let settings_frame: NSRect = msg_send![window, frame];
    if save_original {
        let mut original = GLASS_TINT_GROUP_ORIGINAL_ORIGIN.lock().unwrap();
        if original.is_none() {
            *original = Some(settings_frame.origin);
        }
    }
    let panel_frame: NSRect = msg_send![panel, frame];
    let screen_frame = glass_tint_screen_frame(window);
    let (settings_frame, panel_frame) =
        glass_tint_group_frames(settings_frame, panel_frame, screen_frame);
    let _: () = msg_send![window, setFrameOrigin: settings_frame.origin];
    let _: () = msg_send![panel, setFrameOrigin: panel_frame.origin];
}

/// 颜色面板关闭后恢复设置窗口打开前的位置;重复调用必须安全。
/// Restore the settings window's pre-panel position after the color panel closes; repeated calls
/// are intentionally harmless.
pub(crate) fn restore_glass_tint_group() {
    let original = GLASS_TINT_GROUP_ORIGINAL_ORIGIN.lock().unwrap().take();
    let Some(origin) = original else { return };
    unsafe {
        let window = SETTINGS_UI.lock().unwrap().as_ref().map(|ui| ui.window);
        if let Some(window) = window {
            let _: () = msg_send![window, setFrameOrigin: origin];
        }
    }
}

/// 自定义 NSColorWell 在 AppKit 显示共享颜色面板前先调整窗口位置,避免左下角闪现。
/// Custom NSColorWell positioning before AppKit displays the shared color panel, avoiding a
/// flash in the screen's lower-left corner.
extern "C" fn glass_tint_well_activate(this: *mut c_void, _cmd: Sel, exclusive: bool) {
    unsafe {
        position_glass_tint_group(true);
        let superclass = AnyClass::get(c"NSColorWell").unwrap();
        let _: () = msg_send![
            super(this as *mut AnyObject, superclass),
            activate: exclusive
        ];
        // AppKit 可能在 activate:期间恢复面板记忆位置,因此 super 返回后再应用一次整体布局。
        // AppKit may restore the panel's remembered frame during activate:; apply the grouped
        // position once more after super so the final visible frame is deterministic.
        position_glass_tint_group(false);
    }
}

fn glass_tint_well_class() -> *mut AnyObject {
    GLASS_TINT_WELL_CLASS
        .get_or_init(|| unsafe {
            let name = CString::new("OhMyTabGlassTintWell").unwrap();
            let superclass = class!(NSColorWell) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types = CString::new("v@:B").unwrap();
            class_addMethod(
                cls,
                sel!(activate:),
                glass_tint_well_activate as *mut c_void,
                types.as_ptr(),
            );
            objc_registerClassPair(cls);
            GlassTintWellClass(cls)
        })
        .0
}

/// 创建设置页内的玻璃预览块,内容只使用抽象形状,不暴露真实窗口或剪贴板数据。
/// Create an in-settings glass preview using abstract shapes only, never real windows or clipboard data.
unsafe fn make_glass_preview(
    parent: *mut AnyObject,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    switcher: bool,
) -> *mut AnyObject {
    let is_macos_26 = AnyClass::get(c"NSGlassEffectView").is_some();
    let content_parent: *mut AnyObject;
    let glass: *mut AnyObject;
    if is_macos_26 {
        let glass_cls = AnyClass::get(c"NSGlassEffectView").unwrap();
        let view: *mut AnyObject = msg_send![glass_cls, alloc];
        glass = msg_send![
            view,
            initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
        ];
        let _: () = msg_send![glass, setCornerRadius: 12.0f64];
        let style = if crate::config::effective_glass_style() == "clear" {
            1i64
        } else {
            0i64
        };
        let _: () = msg_send![glass, setStyle: style];
        let tint = crate::ffi::hex_to_ns_color(crate::config::parse_hex8(
            &crate::config::effective_glass_tint(),
        ));
        let _: () = msg_send![glass, setTintColor: tint];
        let _: () = msg_send![glass, setWantsLayer: true];
        let layer: *mut AnyObject = msg_send![glass, layer];
        if !layer.is_null() {
            let _: () = msg_send![layer, setCornerRadius: 12.0f64];
            let _: () = msg_send![layer, setMasksToBounds: true];
        }
        let inner: *mut AnyObject = msg_send![class!(NSView), alloc];
        let inner: *mut AnyObject = msg_send![
            inner,
            initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h))
        ];
        let _: () = msg_send![inner, setAutoresizingMask: 18u64];
        let _: () = msg_send![glass, setContentView: inner];
        content_parent = inner;
    } else {
        let view: *mut AnyObject = msg_send![class!(NSVisualEffectView), alloc];
        glass = msg_send![
            view,
            initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
        ];
        let _: () = msg_send![glass, setBlendingMode: 1u64]; // WithinWindow
        let _: () = msg_send![glass, setMaterial: 12u64]; // Dark
        let _: () = msg_send![glass, setState: 1u64];
        content_parent = glass;
    }
    let _: () = msg_send![glass, setAutoresizingMask: 0u64];
    let _: () = msg_send![parent, addSubview: glass];
    release_obj(glass);

    if switcher {
        let tile_w = ((w - 42.0) / 2.0).max(56.0);
        add_preview_tile(
            content_parent,
            NSRect::new(NSPoint::new(14.0, 19.0), NSSize::new(tile_w, 52.0)),
            0xFFFFFF78,
            10.0,
        );
        add_preview_tile(
            content_parent,
            NSRect::new(
                NSPoint::new(w - 14.0 - tile_w, 19.0),
                NSSize::new(tile_w, 52.0),
            ),
            0xFFFFFF90,
            10.0,
        );
    } else {
        add_preview_tile(
            content_parent,
            NSRect::new(NSPoint::new(12.0, h - 22.0), NSSize::new(w - 24.0, 10.0)),
            0xFFFFFF70,
            5.0,
        );
        add_preview_tile(
            content_parent,
            NSRect::new(NSPoint::new(12.0, 25.0), NSSize::new(w - 24.0, 12.0)),
            0xFFFFFF62,
            5.0,
        );
        add_preview_tile(
            content_parent,
            NSRect::new(NSPoint::new(12.0, 9.0), NSSize::new(w - 42.0, 8.0)),
            0xFFFFFF48,
            4.0,
        );
    }
    glass
}

unsafe fn add_preview_tile(parent: *mut AnyObject, frame: NSRect, color_hex: u32, radius: f64) {
    let tile: *mut AnyObject = msg_send![class!(NSView), alloc];
    let tile: *mut AnyObject = msg_send![tile, initWithFrame: frame];
    let _: () = msg_send![tile, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![tile, layer];
    if !layer.is_null() {
        let _: () = msg_send![layer, setCornerRadius: radius];
        crate::ffi::layer_set_background(layer, crate::ffi::hex_to_cg_color(color_hex));
        crate::ffi::layer_set_border(layer, crate::ffi::hex_to_cg_color(0x0000000Au32));
        let _: () = msg_send![layer, setBorderWidth: 1.0f64];
    }
    let _: () = msg_send![parent, addSubview: tile];
    release_obj(tile);
}

unsafe fn add_preview_caption(parent: *mut AnyObject, text: &str, x: f64, y: f64, w: f64) {
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let label: *mut AnyObject = msg_send![
        label,
        initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, 18.0))
    ];
    let ns = make_nsstring(text);
    let _: () = msg_send![label, setStringValue: ns];
    CFRelease(ns as *const c_void);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let color: *mut AnyObject = msg_send![class!(NSColor), secondaryLabelColor];
    let font: *mut AnyObject = msg_send![class!(NSFont), messageFontOfSize: 11.0f64];
    let _: () = msg_send![label, setTextColor: color];
    let _: () = msg_send![label, setFont: font];
    let _: () = msg_send![parent, addSubview: label];
    release_obj(label);
}

unsafe fn configure_glass_tint_panel(target: *mut AnyObject) {
    let panel: *mut AnyObject = msg_send![class!(NSColorPanel), sharedColorPanel];
    let _: () = msg_send![panel, setShowsAlpha: true];
    let _: () = msg_send![panel, setContinuous: true];
    let _: () = msg_send![panel, setTarget: target];
    let _: () = msg_send![panel, setAction: sel!(handleGlassTintPanelChanged:)];
    if !GLASS_TINT_PANEL_OBSERVER_INSTALLED.load(Ordering::SeqCst) {
        let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
        let name = make_nsstring("NSWindowWillCloseNotification");
        let _: () = msg_send![
            center,
            addObserver: target,
            selector: sel!(handleGlassTintPanelWillClose:),
            name: name,
            object: panel
        ];
        CFRelease(name as *const c_void);
        GLASS_TINT_PANEL_OBSERVER_INSTALLED.store(true, Ordering::SeqCst);
    }

    // accessory 宽度必须匹配颜色面板本身;NSColorPanel 不会因 accessory 超宽而自动扩窗。
    // The accessory width must match the color panel; NSColorPanel does not widen itself for an
    // oversized accessory view.
    let panel_frame: NSRect = msg_send![panel, frame];
    let accessory_w = panel_frame.size.width.max(250.0);
    let accessory_margin = 8.0;
    let accessory: *mut AnyObject = msg_send![class!(NSView), alloc];
    let accessory: *mut AnyObject = msg_send![
        accessory,
        initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(accessory_w, 34.0))
    ];
    let reset = make_settings_action_button(
        NSRect::new(
            NSPoint::new(accessory_margin, 3.0),
            NSSize::new(accessory_w - accessory_margin * 2.0, 28.0),
        ),
        &t("settings.reset_glass_tint"),
        target,
        sel!(handleGlassTintReset:),
    );
    // 按本地化标题使用原生固有宽度,避免全宽按钮让系统圆角比例失真。
    // Use the native fitting width for the localized title so a full-width button does not distort
    // the system bezel's corner proportions.
    let fitting: NSSize = msg_send![reset, fittingSize];
    let max_reset_w = accessory_w - accessory_margin * 2.0;
    let reset_w = if fitting.width > 0.0 {
        fitting.width.clamp(80.0, max_reset_w)
    } else {
        max_reset_w.min(140.0)
    };
    let _: () = msg_send![
        reset,
        setFrame: NSRect::new(
            NSPoint::new((accessory_w - reset_w) / 2.0, 3.0),
            NSSize::new(reset_w, 28.0)
        )
    ];
    let _: () = msg_send![accessory, addSubview: reset];
    release_obj(reset);
    let _: () = msg_send![panel, setAccessoryView: accessory];
    release_obj(accessory);
}

/// 关闭并解绑系统取色面板,避免设置窗口销毁后面板继续改动悬空的 color well。
/// Close and detach the system color panel so it cannot mutate a dangling color well after the
/// settings window is destroyed.
unsafe fn close_glass_tint_panel(well: *mut AnyObject) {
    // NSColorPanel.sharedColorPanel is independent from the settings window, and AppKit can
    // report the color well as inactive while the shared panel is still visible. Always hide the
    // panel; `isActive` only decides whether the well needs an additional deactivate call.
    //
    // NSColorPanel.sharedColorPanel 独立于设置窗口,而且 AppKit 可能在共享面板仍可见时把
    // color well 报告为非 active。必须无条件隐藏面板;`isActive` 只能决定是否额外停用色块。
    if !well.is_null() {
        let active: bool = msg_send![well, isActive];
        if active {
            let _: () = msg_send![well, deactivate];
        }
    }
    let panel: *mut AnyObject = msg_send![class!(NSColorPanel), sharedColorPanel];
    if !panel.is_null() {
        let _: () = msg_send![panel, orderOut: std::ptr::null::<AnyObject>()];
        let _: () = msg_send![panel, setAccessoryView: std::ptr::null::<AnyObject>()];
    }
    restore_glass_tint_group();
}

unsafe fn update_settings_preview_views() {
    let style = if crate::config::effective_glass_style() == "clear" {
        1i64
    } else {
        0i64
    };
    let tint = crate::ffi::hex_to_ns_color(crate::config::parse_hex8(
        &crate::config::effective_glass_tint(),
    ));
    let ui = SETTINGS_UI.lock().unwrap();
    let Some(ui) = ui.as_ref() else { return };
    for preview in [ui.glass_preview_switcher, ui.glass_preview_clipboard] {
        if preview.is_null() {
            continue;
        }
        let supports_style: bool = msg_send![preview, respondsToSelector: sel!(setStyle:)];
        if supports_style {
            let _: () = msg_send![preview, setStyle: style];
        }
        let supports_tint: bool = msg_send![preview, respondsToSelector: sel!(setTintColor:)];
        if supports_tint {
            let _: () = msg_send![preview, setTintColor: tint];
        }
    }
}

/// 应用临时玻璃预览到真实浮窗和设置页内的两个模拟浮窗。
/// Apply the temporary glass preview to the real overlays and the two in-settings mock overlays.
pub(crate) fn apply_glass_preview() {
    unsafe {
        crate::overlay::apply_glass_properties();
        crate::clipboard::apply_glass_properties();
        update_settings_preview_views();
    }
}

fn clear_glass_preview() {
    crate::config::set_glass_style_preview(None);
    crate::config::set_glass_tint_preview(None);
    apply_glass_preview();
}

unsafe fn update_glass_tint_from_color(color: *mut AnyObject, sync_well: bool) {
    if GLASS_UI_UPDATE.load(Ordering::SeqCst) {
        return;
    }
    let Some(hex) = ns_color_to_hex(color) else {
        return;
    };
    if sync_well {
        if let Some(ui) = SETTINGS_UI.lock().unwrap().as_ref() {
            GLASS_UI_UPDATE.store(true, Ordering::SeqCst);
            let _: () = msg_send![ui.glass_tint, setColor: color];
            GLASS_UI_UPDATE.store(false, Ordering::SeqCst);
        }
    }
    crate::config::set_glass_tint_preview(Some(hex));
    apply_glass_preview();
}

pub(crate) extern "C" fn on_glass_tint_changed(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    unsafe { update_glass_tint_from_color(sender as *mut AnyObject, false) }
}

pub(crate) extern "C" fn on_glass_tint_panel_changed(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    unsafe {
        let panel: *mut AnyObject = msg_send![class!(NSColorPanel), sharedColorPanel];
        let color: *mut AnyObject = msg_send![panel, color];
        update_glass_tint_from_color(color, true);
    }
}

pub(crate) extern "C" fn on_glass_tint_panel_will_close(
    _self: *mut c_void,
    _cmd: Sel,
    _notification: *mut c_void,
) {
    restore_glass_tint_group();
}

pub(crate) extern "C" fn on_glass_tint_reset(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    unsafe {
        let default_hex = Config::default().appearance.glass_tint;
        let color = crate::ffi::hex_to_ns_color(crate::config::parse_hex8(&default_hex));
        GLASS_UI_UPDATE.store(true, Ordering::SeqCst);
        if let Some(ui) = SETTINGS_UI.lock().unwrap().as_ref() {
            let _: () = msg_send![ui.glass_tint, setColor: color];
        }
        let panel: *mut AnyObject = msg_send![class!(NSColorPanel), sharedColorPanel];
        let _: () = msg_send![panel, setColor: color];
        GLASS_UI_UPDATE.store(false, Ordering::SeqCst);
        crate::config::set_glass_tint_preview(Some(default_hex));
        apply_glass_preview();
    }
}

pub(crate) extern "C" fn on_glass_style_changed(
    _self: *mut c_void,
    _cmd: Sel,
    sender: *mut c_void,
) {
    unsafe {
        let idx: isize = msg_send![sender as *mut AnyObject, indexOfSelectedItem];
        crate::config::set_glass_style_preview(Some(if idx == 1 {
            "clear".into()
        } else {
            "regular".into()
        }));
        apply_glass_preview();
    }
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
    let _: () = msg_send![popup, setBezelStyle: 0isize];
    let _: () = msg_send![popup, setControlSize: 0isize]; // Regular
    let _: () = msg_send![popup, setBordered: false];
    let _: () = msg_send![popup, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![popup, layer];
    if !layer.is_null() {
        layer_set_background(layer, crate::ffi::hex_to_cg_color(0x7676801Cu32));
        let _: () = msg_send![layer, setCornerRadius: 9.0f64];
        let _: () = msg_send![layer, setMasksToBounds: true];
    }
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
    // Use the regular 38x22 switch from the reference instead of the undersized compact variant.
    // fittingSize gives the native size and the frame is then vertically centered in the row.
    let _: () = msg_send![sw, setControlSize: 0isize]; // NSControlSizeRegular
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

/// 设侧边栏按钮标题为 attributed title:未选中用 labelColor,选中用系统强调色。
/// Set the sidebar button title as an attributed title, using the normal label color when
/// unselected and the system accent color when selected.
unsafe fn set_sidebar_title(btn: *mut AnyObject, title: &str, selected: bool) {
    let font: *mut AnyObject = if selected {
        msg_send![class!(NSFont), boldSystemFontOfSize: 13.5f64]
    } else {
        msg_send![class!(NSFont), messageFontOfSize: 13.5f64]
    };
    let color: *mut AnyObject = if selected {
        msg_send![class!(NSColor), controlAccentColor]
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
    let label = SIDEBAR_TITLE_LABELS
        .lock()
        .unwrap()
        .get(&(btn as usize))
        .map(|p| p.0)
        .unwrap_or(btn);
    let _: () = msg_send![label, setStringValue: title_ns];
    let _: () = msg_send![label, setAttributedStringValue: attr_str];
    if let Some(icon) = SIDEBAR_ICON_VIEWS
        .lock()
        .unwrap()
        .get(&(btn as usize))
        .map(|p| p.0)
    {
        let _: () = msg_send![icon, setContentTintColor: color];
    }
    let _: () = msg_send![btn, setContentTintColor: color];
    CFRelease(title_ns as *const c_void);
    release_obj(attr_str);
    release_obj(attrs);
}

/// 侧边栏按钮(borderless NSButton,左对齐图标+文字,tag 区分页)。
/// Sidebar button (borderless NSButton; left-aligned icon + title; tag selects the page).
unsafe fn make_sidebar_button(
    parent: *mut AnyObject,
    target: *mut AnyObject,
    title: &str,
    tag: isize,
    x: f64,
    y: f64,
    w: f64,
) -> *mut AnyObject {
    let h = 38.0;
    let btn: *mut AnyObject = msg_send![sidebar_button_class(), alloc];
    let btn: *mut AnyObject =
        msg_send![btn, initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))];
    let _: () = msg_send![btn, setButtonType: 0isize]; // NSPushInPushButton
    let _: () = msg_send![btn, setBordered: false];
    // NSButton starts with the default title "Button".  The sidebar title is rendered by
    // the fixed-column NSTextField below, so clear the native title to avoid drawing it twice.
    let empty_title = make_nsstring("");
    let _: () = msg_send![btn, setTitle: empty_title];
    CFRelease(empty_title as *const c_void);
    let _: () = msg_send![btn, setAlignment: 0isize]; // NSTextAlignmentLeft
    let _: () = msg_send![btn, setWantsLayer: true];
    let btn_layer: *mut AnyObject = msg_send![btn, layer];
    if !btn_layer.is_null() {
        layer_set_background(btn_layer, crate::ffi::hex_to_cg_color(0x00000000u32));
        let _: () = msg_send![btn_layer, setCornerRadius: 10.0f64];
        let _: () = msg_send![btn_layer, setMasksToBounds: true];
    }
    let _: () = msg_send![btn, setTag: tag];
    let symbol = match tag {
        0 => "gearshape",
        1 => "rectangle.on.rectangle",
        2 => "computermouse",
        3 => "doc.on.clipboard",
        4 => "info.circle",
        _ => "circle",
    };
    let symbol_ns = make_nsstring(symbol);
    let image: *mut AnyObject = msg_send![
        class!(NSImage),
        imageWithSystemSymbolName: symbol_ns,
        accessibilityDescription: std::ptr::null::<AnyObject>()
    ];
    CFRelease(symbol_ns as *const c_void);
    if !image.is_null() {
        let icon_view: *mut AnyObject = msg_send![class!(NSImageView), alloc];
        let icon_view: *mut AnyObject = msg_send![
            icon_view,
            // SF Symbols have a little optical padding at the bottom; lift the image view
            // so the glyph's visual center shares the text baseline in the 38pt row.
            initWithFrame: NSRect::new(NSPoint::new(15.5, 10.0), NSSize::new(17.0, 17.0))
        ];
        let _: () = msg_send![icon_view, setImage: image];
        let _: () = msg_send![icon_view, setImageScaling: 3isize];
        let _: () = msg_send![icon_view, setEditable: false];
        let _: () = msg_send![icon_view, setWantsLayer: false];
        let _: () = msg_send![btn, addSubview: icon_view];
        SIDEBAR_ICON_VIEWS
            .lock()
            .unwrap()
            .insert(btn as usize, ObjPtr(icon_view));
        release_obj(icon_view);
    }
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let label: *mut AnyObject = msg_send![
        label,
        initWithFrame: NSRect::new(
            NSPoint::new(44.5, 11.0),
            NSSize::new((w - 51.0).max(1.0), 24.0),
        )
    ];
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let _: () = msg_send![label, setSelectable: false];
    let _: () = msg_send![label, setAlignment: 0isize];
    let _: () = msg_send![label, setUsesSingleLineMode: true];
    let _: () = msg_send![label, setEnabled: false];
    let _: () = msg_send![btn, addSubview: label];
    SIDEBAR_TITLE_LABELS
        .lock()
        .unwrap()
        .insert(btn as usize, ObjPtr(label));
    release_obj(label);
    let tracking: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
    let tracking: *mut AnyObject = msg_send![
        tracking,
        initWithRect: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h)),
        options: 0x01u64 | 0x80u64 | 0x200u64,
        owner: btn,
        userInfo: std::ptr::null::<AnyObject>()
    ];
    let _: () = msg_send![btn, addTrackingArea: tracking];
    release_obj(tracking);
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
    let section_text = text.to_uppercase();
    let ns = make_nsstring(&section_text);
    let _: () = msg_send![label, setStringValue: ns];
    CFRelease(ns as *const c_void);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let font: *mut AnyObject = msg_send![class!(NSFont), boldSystemFontOfSize: 11.0f64];
    let _: () = msg_send![label, setFont: font];
    let color: *mut AnyObject = msg_send![class!(NSColor), secondaryLabelColor];
    let _: () = msg_send![label, setTextColor: color];
    // 自适应:宽度随父视图拉伸、顶部锚定(MinYMargin)。autoresizing = WidthSizable | MinYMargin = 2|8 = 10。
    // Adaptive: stretch width with the parent, stay top-anchored (MinYMargin).
    let _: () = msg_send![label, setAutoresizingMask: 10u64];
    let _: () = msg_send![parent, addSubview: label];
    release_obj(label);
}

/// Add a page title matching the HTML redesign's large, tight heading.
unsafe fn add_page_title(parent: *mut AnyObject, text: &str, x: f64, y: f64, w: f64) {
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let label: *mut AnyObject =
        msg_send![label, initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, 32.0))];
    set_field(label, text);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let font: *mut AnyObject = msg_send![class!(NSFont), boldSystemFontOfSize: 25.0f64];
    let _: () = msg_send![label, setFont: font];
    let color: *mut AnyObject = msg_send![class!(NSColor), labelColor];
    let _: () = msg_send![label, setTextColor: color];
    let _: () = msg_send![label, setAutoresizingMask: 10u64];
    let _: () = msg_send![parent, addSubview: label];
    release_obj(label);
}

/// Build the About header icon from the app's own bundle icon, retaining the reference's
/// rounded container treatment while avoiding a second hand-drawn logo.
unsafe fn add_about_app_icon(parent: *mut AnyObject, x: f64, y: f64) {
    let icon: *mut AnyObject = msg_send![class!(NSView), alloc];
    let icon: *mut AnyObject = msg_send![
        icon,
        initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(58.0, 58.0))
    ];

    let image_name = make_nsstring("NSApplicationIcon");
    let image: *mut AnyObject = msg_send![class!(NSImage), imageNamed: image_name];
    CFRelease(image_name as *const c_void);
    if !image.is_null() {
        let image_view: *mut AnyObject = msg_send![class!(NSImageView), alloc];
        let image_view: *mut AnyObject = msg_send![
            image_view,
            // Let the app icon occupy the whole slot. The PNG already contains its own
            // rounded silhouette, so an additional white container would create a visible
            // border around it.
            initWithFrame: NSRect::new(NSPoint::new(-2.0, -2.0), NSSize::new(62.0, 62.0))
        ];
        let _: () = msg_send![image_view, setImage: image];
        let _: () = msg_send![image_view, setImageScaling: 3isize];
        let _: () = msg_send![image_view, setImageFrameStyle: 0isize];
        let _: () = msg_send![icon, addSubview: image_view];
        release_obj(image_view);
    }
    let _: () = msg_send![parent, addSubview: icon];
    release_obj(icon);
}

/// Add a grouped card behind a section, matching the HTML redesign's light card surface.
unsafe fn add_settings_card(parent: *mut AnyObject, frame: NSRect) {
    if frame.size.width <= 0.0 || frame.size.height <= 0.0 {
        return;
    }
    let card: *mut AnyObject = msg_send![class!(NSView), alloc];
    let card: *mut AnyObject = msg_send![card, initWithFrame: frame];
    let _: () = msg_send![card, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![card, layer];
    if !layer.is_null() {
        layer_set_background(layer, crate::ffi::hex_to_cg_color(0xFFFFFFE0u32));
        let _: () = msg_send![layer, setCornerRadius: 14.0f64];
        // Keep the outer shadow visible. The card has no child content that needs clipping.
        let _: () = msg_send![layer, setMasksToBounds: false];
        crate::ffi::layer_set_border(layer, crate::ffi::hex_to_cg_color(0x00000012u32));
        let _: () = msg_send![layer, setBorderWidth: 1.0f64];
        let _: () = msg_send![layer, setShadowOpacity: 0.025f32];
        let _: () = msg_send![layer, setShadowRadius: 8.0f64];
        let _: () = msg_send![layer, setShadowOffset: NSSize::new(0.0, -2.0)];
    }
    // Insert below controls and labels so the card never intercepts their mouse events.
    let _: () = msg_send![
        parent,
        addSubview: card,
        positioned: -1isize,
        relativeTo: std::ptr::null::<AnyObject>()
    ];
    release_obj(card);
}

/// Draw the HTML `.row + .row` hairline inside a grouped card.
unsafe fn add_row_separator(parent: *mut AnyObject, x: f64, y: f64, w: f64) {
    // Keep the hairline inside the card's rounded frame. Grouped cards are
    // inset by the same six points, so their row separators need that inset
    // as well instead of reaching the content pane edge.
    let line_x = x + 6.0;
    let line_w = (w - 12.0).max(1.0);
    let line: *mut AnyObject = msg_send![class!(NSView), alloc];
    let line: *mut AnyObject = msg_send![
        line,
        initWithFrame: NSRect::new(NSPoint::new(line_x, y), NSSize::new(line_w, 1.0))
    ];
    let _: () = msg_send![line, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![line, layer];
    if !layer.is_null() {
        layer_set_background(layer, crate::ffi::hex_to_cg_color(0x00000016u32));
    }
    let _: () = msg_send![parent, addSubview: line];
    release_obj(line);
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
    let label: *mut AnyObject = msg_send![label, initWithFrame: NSRect::new(
        NSPoint::new(label_x, y),
        NSSize::new(label_w, (h - 12.0).max(1.0)),
    )];
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

/// Add a taller settings row with the two-level title/subtitle hierarchy used by the HTML
/// reference. The caller positions the native control inside the row; this helper only builds
/// the leading text stack and returns the control after attaching it.
#[allow(clippy::too_many_arguments)]
unsafe fn add_described_row(
    parent: *mut AnyObject,
    x: f64,
    y: f64,
    text_w: f64,
    row_h: f64,
    title: &str,
    subtitle: &str,
    control: *mut AnyObject,
) -> *mut AnyObject {
    let title_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let title_label: *mut AnyObject = msg_send![
        title_label,
        initWithFrame: NSRect::new(
            NSPoint::new(x, y + row_h - 27.0),
            NSSize::new(text_w, 18.0),
        )
    ];
    set_field(title_label, title);
    let _: () = msg_send![title_label, setBezeled: false];
    let _: () = msg_send![title_label, setDrawsBackground: false];
    let _: () = msg_send![title_label, setEditable: false];
    let _: () = msg_send![title_label, setUsesSingleLineMode: true];
    let title_font: *mut AnyObject = msg_send![class!(NSFont), messageFontOfSize: 13.5f64];
    let _: () = msg_send![title_label, setFont: title_font];
    let _: () = msg_send![parent, addSubview: title_label];
    release_obj(title_label);

    let subtitle_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let subtitle_label: *mut AnyObject = msg_send![
        subtitle_label,
        initWithFrame: NSRect::new(
            NSPoint::new(x, y + 7.0),
            NSSize::new(text_w, 17.0),
        )
    ];
    set_field(subtitle_label, subtitle);
    let _: () = msg_send![subtitle_label, setBezeled: false];
    let _: () = msg_send![subtitle_label, setDrawsBackground: false];
    let _: () = msg_send![subtitle_label, setEditable: false];
    let _: () = msg_send![subtitle_label, setUsesSingleLineMode: true];
    let subtitle_font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 11.5f64];
    let _: () = msg_send![subtitle_label, setFont: subtitle_font];
    let subtitle_color: *mut AnyObject = msg_send![class!(NSColor), secondaryLabelColor];
    let _: () = msg_send![subtitle_label, setTextColor: subtitle_color];
    let _: () = msg_send![parent, addSubview: subtitle_label];
    release_obj(subtitle_label);

    let _: () = msg_send![control, setAutoresizingMask: 10u64];
    let _: () = msg_send![parent, addSubview: control];
    release_obj(control);
    control
}

/// HTML `.row` at 54pt but single-line: the label is vertically centered and the control
/// right-aligned. Used for the Device/Scroll-mode rows that have no subtitle.
#[allow(clippy::too_many_arguments)]
unsafe fn add_tall_row(
    parent: *mut AnyObject,
    label_x: f64,
    y: f64,
    label_w: f64,
    label_text: &str,
    control: *mut AnyObject,
) -> (*mut AnyObject, *mut AnyObject) {
    let h = 54.0; // matched described_row_h
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let label: *mut AnyObject = msg_send![label, initWithFrame: NSRect::new(NSPoint::new(label_x, y + (h - 22.0) / 2.0), NSSize::new(label_w, 22.0))];
    let ns = make_nsstring(label_text);
    let _: () = msg_send![label, setStringValue: ns];
    CFRelease(ns as *const c_void);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let _: () = msg_send![label, setAlignment: 0isize]; // left
    let font: *mut AnyObject = msg_send![class!(NSFont), messageFontOfSize: 13.5f64];
    let _: () = msg_send![label, setFont: font];
    let _: () = msg_send![label, setAutoresizingMask: 12u64];
    let _: () = msg_send![parent, addSubview: label];
    release_obj(label);
    let _: () = msg_send![control, setAutoresizingMask: 10u64];
    let _: () = msg_send![parent, addSubview: control];
    release_obj(control);
    (label, control)
}

/// Style an NSPopUpButton with the HTML `.field` look: a rounded light-gray surface, no
/// bezel, so the Device/Scroll-mode dropdowns match the flat reference control.
unsafe fn style_flat_popup(popup: *mut AnyObject) {
    let _: () = msg_send![popup, setBezelStyle: 0isize];
    let _: () = msg_send![popup, setControlSize: 0isize]; // Regular
    let _: () = msg_send![popup, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![popup, layer];
    if !layer.is_null() {
        let _: () = msg_send![layer, setCornerRadius: 9.0f64];
        let _: () = msg_send![layer, setMasksToBounds: true];
        crate::ffi::layer_set_background(layer, crate::ffi::hex_to_cg_color(0x7676801Du32));
        crate::ffi::layer_set_border(layer, crate::ffi::hex_to_cg_color(0x00000012u32));
        let _: () = msg_send![layer, setBorderWidth: 1.0f64];
    }
}

/// Create one transparent, vertically scrolling settings page above the fixed footer. The
/// document view keeps AppKit's normal bottom-left coordinate system so the existing layout
/// code can continue to position controls from a top cursor.
unsafe fn make_settings_page(
    parent: *mut AnyObject,
    frame: NSRect,
    document_h: f64,
    hidden: bool,
) -> (*mut AnyObject, *mut AnyObject) {
    let scroll: *mut AnyObject = msg_send![class!(NSScrollView), alloc];
    let scroll: *mut AnyObject = msg_send![scroll, initWithFrame: frame];
    // FullSizeContentView lets the scroll view reach into the title bar, so AppKit auto-adds a
    // top content inset. Without disabling it, the real scrollable top sits below the geometric
    // doc top, so scrollToPoint(0, doc_h - clip_h) never lands at the very top (the Mouse title
    // looks flush yet the scrollbar can still rise). Disable the auto inset and force zero inset.
    let _: () = msg_send![scroll, setAutomaticallyAdjustsContentInsets: false];
    let _: () = msg_send![scroll, setContentInsets: NSEdgeInsets { top: 0.0, left: 0.0, bottom: 0.0, right: 0.0 }];
    let _: () = msg_send![scroll, setBorderType: 0u64];
    let _: () = msg_send![scroll, setDrawsBackground: false];
    let _: () = msg_send![scroll, setHasHorizontalScroller: false];
    let _: () = msg_send![scroll, setHasVerticalScroller: true];
    let _: () = msg_send![scroll, setAutohidesScrollers: true];
    let _: () = msg_send![scroll, setScrollerStyle: 1isize]; // overlay
    let _: () = msg_send![scroll, setAutoresizingMask: 18u64];
    let _: () = msg_send![scroll, setHidden: hidden];

    let clip: *mut AnyObject = msg_send![scroll, contentView];
    let _: () = msg_send![clip, setDrawsBackground: false];

    let document: *mut AnyObject = msg_send![class!(NSView), alloc];
    let document: *mut AnyObject = msg_send![
        document,
        initWithFrame: NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(frame.size.width, document_h),
        )
    ];
    let _: () = msg_send![document, setAutoresizingMask: 2u64];
    let _: () = msg_send![scroll, setDocumentView: document];
    let _: () = msg_send![parent, addSubview: scroll];

    // Scroll to the TOP of the document so the page opens with the title flush at the top edge.
    // Measure the clip's real bounds height AFTER the doc view is attached (a frame-based guess
    // can be stale before layout and leaves the scrollbar mid-track).
    let clip_bounds: NSRect = msg_send![clip, bounds];
    let top_origin = (document_h - clip_bounds.size.height).max(0.0);
    let _: () = msg_send![clip, scrollToPoint: NSPoint::new(0.0, top_origin)];
    let _: () = msg_send![scroll, reflectScrolledClipView: clip];

    release_obj(document);
    release_obj(scroll);
    (scroll, document)
}

/// Scroll a settings page's clip view to the top. Call this after the window has been laid out:
/// a frame-time scrollToPoint gets reset by AppKit's first layout pass, leaving the scrollbar
/// mid-track. The page scroll views are the same views stored on SettingsUi (general_view, etc.).
unsafe fn scroll_page_to_top(scroll: *mut AnyObject) {
    if scroll.is_null() {
        return;
    }
    let clip: *mut AnyObject = msg_send![scroll, contentView];
    let doc: *mut AnyObject = msg_send![scroll, documentView];
    let doc_frame: NSRect = msg_send![doc, frame];
    let clip_bounds: NSRect = msg_send![clip, bounds];
    let top_origin = (doc_frame.size.height - clip_bounds.size.height).max(0.0);
    let _: () = msg_send![clip, scrollToPoint: NSPoint::new(0.0, top_origin)];
    let _: () = msg_send![scroll, reflectScrolledClipView: clip];
    let after: NSRect = msg_send![clip, bounds];
    log_debug!(
        "[settings] scroll_page_to_top: doc_h={:.1} clip_h={:.1} top_origin={:.1} -> clip_oy_after={:.1}",
        doc_frame.size.height,
        clip_bounds.size.height,
        top_origin,
        after.origin.y
    );
}

// ========== 设置窗口逻辑 / settings window logic ==========

pub(crate) extern "C" fn on_settings_open(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    show_settings();
}

/// 手动检查更新:把请求交给 Sparkle 的标准更新界面。
/// Manual update check: hand the request to Sparkle's standard update UI.
pub(crate) extern "C" fn handle_check_for_updates(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    if !crate::updater::check_for_updates() {
        show_alert(
            &t("settings.update_unavailable_title"),
            &t("settings.update_unavailable_message"),
        );
    }
}

/// 打开项目官方网站。
/// Open the project's official website in the default browser.
pub(crate) extern "C" fn handle_open_official_website(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    unsafe {
        let url_string = make_nsstring("https://oh-my-tab.app");
        let url: *mut AnyObject = msg_send![class!(NSURL), URLWithString: url_string];
        CFRelease(url_string as *const c_void);
        if !url.is_null() {
            let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
            let _: bool = msg_send![workspace, openURL: url];
        }
    }
}

/// 记录设置窗口提交的逐字段变更,不记录剪贴板历史内容。
/// Log field-level changes submitted from Settings without recording clipboard contents.
fn log_config_changes(old: &Config, new: &Config) {
    macro_rules! changed {
        ($name:literal, $old:expr, $new:expr) => {{
            let old_value = &$old;
            let new_value = &$new;
            if old_value != new_value {
                log_info!(
                    "[settings] config changed: {}: {:?} -> {:?}",
                    $name,
                    old_value,
                    new_value
                );
            }
        }};
    }

    changed!(
        "appearance.theme",
        old.appearance.theme,
        new.appearance.theme
    );
    changed!(
        "appearance.glass_style",
        old.appearance.glass_style,
        new.appearance.glass_style
    );
    changed!(
        "appearance.glass_tint",
        old.appearance.glass_tint,
        new.appearance.glass_tint
    );
    changed!(
        "appearance.corner_radius",
        old.appearance.corner_radius,
        new.appearance.corner_radius
    );

    changed!(
        "colors.dark.status_bar_text",
        old.colors.dark.status_bar_text,
        new.colors.dark.status_bar_text
    );
    changed!(
        "colors.dark.app_name",
        old.colors.dark.app_name,
        new.colors.dark.app_name
    );
    changed!(
        "colors.dark.win_title",
        old.colors.dark.win_title,
        new.colors.dark.win_title
    );
    changed!(
        "colors.dark.icon_inner_bg",
        old.colors.dark.icon_inner_bg,
        new.colors.dark.icon_inner_bg
    );
    changed!(
        "colors.dark.icon_text",
        old.colors.dark.icon_text,
        new.colors.dark.icon_text
    );
    changed!(
        "colors.dark.card_bg_sel",
        old.colors.dark.card_bg_sel,
        new.colors.dark.card_bg_sel
    );
    changed!(
        "colors.dark.card_border_sel",
        old.colors.dark.card_border_sel,
        new.colors.dark.card_border_sel
    );
    changed!(
        "colors.light.status_bar_text",
        old.colors.light.status_bar_text,
        new.colors.light.status_bar_text
    );
    changed!(
        "colors.light.app_name",
        old.colors.light.app_name,
        new.colors.light.app_name
    );
    changed!(
        "colors.light.win_title",
        old.colors.light.win_title,
        new.colors.light.win_title
    );
    changed!(
        "colors.light.icon_inner_bg",
        old.colors.light.icon_inner_bg,
        new.colors.light.icon_inner_bg
    );
    changed!(
        "colors.light.icon_text",
        old.colors.light.icon_text,
        new.colors.light.icon_text
    );
    changed!(
        "colors.light.card_bg_sel",
        old.colors.light.card_bg_sel,
        new.colors.light.card_bg_sel
    );
    changed!(
        "colors.light.card_border_sel",
        old.colors.light.card_border_sel,
        new.colors.light.card_border_sel
    );

    changed!(
        "fonts.status_bar_size",
        old.fonts.status_bar_size,
        new.fonts.status_bar_size
    );
    changed!(
        "fonts.status_bar_weight",
        old.fonts.status_bar_weight,
        new.fonts.status_bar_weight
    );
    changed!(
        "fonts.title_size",
        old.fonts.title_size,
        new.fonts.title_size
    );
    changed!(
        "fonts.title_weight",
        old.fonts.title_weight,
        new.fonts.title_weight
    );
    changed!(
        "fonts.app_name_size",
        old.fonts.app_name_size,
        new.fonts.app_name_size
    );
    changed!(
        "fonts.app_name_weight",
        old.fonts.app_name_weight,
        new.fonts.app_name_weight
    );

    changed!(
        "keyboard.modifier",
        old.keyboard.modifier,
        new.keyboard.modifier
    );
    changed!("i18n.locale", old.i18n.locale, new.i18n.locale);
    changed!("windows.enabled", old.windows.enabled, new.windows.enabled);
    changed!(
        "windows.show_minimized",
        old.windows.show_minimized,
        new.windows.show_minimized
    );
    changed!(
        "layout.thumbnails_enabled",
        old.layout.thumbnails_enabled,
        new.layout.thumbnails_enabled
    );
    changed!(
        "windows.overlay_position",
        old.windows.overlay_position,
        new.windows.overlay_position
    );
    changed!("logging.level", old.logging.level, new.logging.level);
    changed!(
        "logging.file_path",
        old.logging.file_path,
        new.logging.file_path
    );
    changed!(
        "startup.launch_at_login",
        old.startup.launch_at_login,
        new.startup.launch_at_login
    );
    changed!(
        "updates.automatically_check",
        old.updates.automatically_check,
        new.updates.automatically_check
    );

    changed!(
        "clipboard.enabled",
        old.clipboard.enabled,
        new.clipboard.enabled
    );
    changed!(
        "clipboard.max_entries",
        old.clipboard.max_entries,
        new.clipboard.max_entries
    );
    changed!(
        "clipboard.show_source_app",
        old.clipboard.show_source_app,
        new.clipboard.show_source_app
    );
    changed!(
        "clipboard.persist",
        old.clipboard.persist,
        new.clipboard.persist
    );
    changed!(
        "clipboard.move_used_to_top",
        old.clipboard.move_used_to_top,
        new.clipboard.move_used_to_top
    );
    changed!(
        "clipboard.auto_expire_days",
        old.clipboard.auto_expire_days,
        new.clipboard.auto_expire_days
    );
    changed!(
        "clipboard.picker_position",
        old.clipboard.picker_position,
        new.clipboard.picker_position
    );
    changed!(
        "clipboard.pin_follow_selection",
        old.clipboard.pin_follow_selection,
        new.clipboard.pin_follow_selection
    );

    changed!("mouse.enabled", old.mouse.enabled, new.mouse.enabled);

    // 鼠标配置档包含嵌套映射,用 Debug 快照比较并记录完整旧/新值。
    // Mouse profiles contain nested mappings, so compare and log complete Debug snapshots.
    let old_profiles = format!("{:?}", old.mouse.profiles);
    let new_profiles = format!("{:?}", new.mouse.profiles);
    if old_profiles != new_profiles {
        log_debug!(
            "[settings] config changed: mouse.profiles: {:?} -> {:?}",
            old_profiles,
            new_profiles
        );
    }
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
    let old_cfg = CONFIG.read().unwrap().clone();
    let needs_restart = old_cfg.mouse.enabled != cfg.mouse.enabled;

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
    // Sparkle keeps its updater object alive for the process; apply the persisted toggle
    // immediately so the next automatic-check interval follows the user's choice.
    crate::updater::set_automatic_checks(cfg.updates.automatically_check);
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

    // 缩略图服务热切换:start 幂等;关闭时 worker/observer 每任务前检查配置自动休眠,
    // 同时立即释放内存中的窗口截图(线程本身保留,便于重新开启)。
    // Thumbnail-service hot-switch: start is idempotent; when off, the worker and
    // observer re-check the config per job and sleep on their own; clear the in-memory
    // window-image cache immediately when the mode is disabled.
    if cfg.layout.thumbnails_enabled {
        crate::thumbnail::start();
    } else {
        crate::thumbnail::clear_runtime_cache();
    }

    // persist 热切换:开启 → 从磁盘加载并合并历史(与 start() 的加载去重互幂等);
    // 关闭 → 删除磁盘历史文件(内存历史保留到本次退出)。
    // Persist hot-switch: ON -> load and merge the history from disk (idempotent with
    // start()'s load, dedup makes the double-merge harmless); OFF -> delete the history
    // file (the in-memory history stays until this session ends).
    crate::clipboard::apply_persist_toggle(cfg.clipboard.persist);
    log_config_changes(&old_cfg, &cfg);
    log_info!("[settings] configuration saved and applied from settings window");
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
            let _: () = msg_send![row.separator, removeFromSuperview];
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
        let items_len = items.len();
        let row_h = MAPPING_ROW_H;
        // 卡片高度随行数增长(顶部固定):少行时保持 3 行,多行时向下长高,整页滚动。
        // The card height grows with the row count (top-anchored): it keeps three rows when
        // short and grows downward when long, the page scroll view handles the overflow.
        // 空状态提示:无行时显示。
        // Empty-state hint: shown when there are no rows.
        let _: () = msg_send![u.mapping_empty, setHidden: !items.is_empty()];
        // 只改高度,宽度保持初始值:曾用 setFrameSize(0.0, doc_h) 把宽清零,
        // 宽度为 0 的文档视图 hit-test 失败 —— 行内删除按钮永远点不到。
        // Resize height only, keeping the initial width: setFrameSize(0.0, doc_h) used to
        // zero the width, and a zero-width document view fails hit-testing -- the delete
        // buttons became unclickable.
        // flipped:y=0 在顶部,行从顶部依次向下排。
        // Flipped: y=0 is the top; rows stack down from the top.
        let mappings_on = {
            let st: isize = msg_send![u.mapping_enabled, state];
            st == 1
        };
        // 添加按钮一并置灰(开关关闭时不可添加新映射)。
        // The add button greys out too (no new mappings while off).
        let _: () = msg_send![u.add_mapping_button, setEnabled: mappings_on];
        // Rows sit inside the nested table panel: a left content inset and right-aligned actions.
        let row_x0 = MAPPING_PANEL_X + MAPPING_CELL_X;
        let card_frame: NSRect = msg_send![doc, frame];
        let card_w = card_frame.size.width;
        let row_right = card_w - MAPPING_PANEL_X - MAPPING_CELL_X;
        let btn_w = 60.0;
        let btn_gap = 6.0;
        let ed_x = row_right - (btn_w * 2.0 + btn_gap);
        let del_x = ed_x + btn_w + btn_gap;
        let btn_h = 27.0;
        let desc_x = row_x0 + 74.0;
        // Rows start below the header band (MAPPING_PANEL_TOP + MAPPING_HEADER_H).
        let mut y = MAPPING_PANEL_TOP + MAPPING_HEADER_H;
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
            let label: *mut AnyObject = msg_send![label, initWithFrame: NSRect::new(NSPoint::new(row_x0, y + (row_h - 22.0) / 2.0), NSSize::new(70.0, 22.0))];
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
            let desc_label: *mut AnyObject = msg_send![desc_label, initWithFrame: NSRect::new(NSPoint::new(desc_x, y + (row_h - 22.0) / 2.0), NSSize::new((ed_x - desc_x - 8.0).max(1.0), 22.0))];
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
            let edit: *mut AnyObject = msg_send![edit, initWithFrame: NSRect::new(NSPoint::new(ed_x, y + (row_h - btn_h) / 2.0), NSSize::new(btn_w, btn_h))];
            style_html_button(edit, 0x7676801Fu32, 0x44444AFFu32);
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
            let delete: *mut AnyObject = msg_send![delete, initWithFrame: NSRect::new(NSPoint::new(del_x, y + (row_h - btn_h) / 2.0), NSSize::new(btn_w, btn_h))];
            style_html_button(delete, 0x7676801Fu32, 0x44444AFFu32);
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
                let mut cap_x = desc_x + 4.0;
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
            let sep: *mut AnyObject = msg_send![sep, initWithFrame: NSRect::new(NSPoint::new(row_x0, y + row_h - 1.0), NSSize::new(row_right - row_x0, 1.0))];
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
                separator: sep,
                caps,
            });
            y += row_h;
        }
        // Grow the nested table + card to fit every row. The panel and the add button track the
        // growing table; the page scroll view handles any overflow, so nothing is clipped.
        {
            let card_frame: NSRect = msg_send![doc, frame];
            let card_w = card_frame.size.width;
            let panel_h = (MAPPING_HEADER_H + items_len as f64 * MAPPING_ROW_H)
                .max(MAPPING_HEADER_H + MAPPING_ROW_H * 3.0);
            let card_h = MAPPING_PANEL_TOP
                + panel_h
                + MAPPING_ACTION_TOP
                + MAPPING_ACTION_H
                + MAPPING_CARD_PAD_BOT;
            let card_top = card_frame.origin.y + card_frame.size.height;
            let card_bottom = card_top - card_h;
            // The panel keeps its top-left anchor; its height tracks the rows.
            let _: () = msg_send![u.mapping_panel, setFrameSize: NSSize::new(card_w - 2.0 * MAPPING_PANEL_X, panel_h)];
            let _: () = msg_send![doc, setFrame: NSRect::new(NSPoint::new(card_frame.origin.x, card_bottom), NSSize::new(card_w, card_h))];
            // The add button sits in the action row at the card bottom.
            let _: () = msg_send![u.add_mapping_button, setFrame: NSRect::new(NSPoint::new(MAPPING_PANEL_X, MAPPING_PANEL_TOP + panel_h + MAPPING_ACTION_TOP), NSSize::new(card_w - 2.0 * MAPPING_PANEL_X, MAPPING_ACTION_H))];
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
    log_debug!("[mouse] recording tap started");
    crate::event_tap::CFRunLoopRun();
    *REC_TAP.0.lock().unwrap() = None;
    *REC_RUNLOOP.0.lock().unwrap() = None;
    log_debug!("[mouse] recording tap stopped");
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
    log_debug!("[mouse] mapping panel opened (new mapping)");
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
    log_debug!("[mouse] mapping panel opened (edit button {})", tag);
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
    log_debug!("[mouse] recording trigger (press a mouse button)");
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
    log_debug!("[mouse] recording combo (press the key combo)");
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
    log_debug!(
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
    log_debug!("[mouse] mapping panel cancelled");
}

/// 映射总开关变化回调:重渲染映射行(关闭时行控件置灰不可点)。
/// The mappings master switch toggled: re-render the rows (greyed out and inert when off).
pub(crate) extern "C" fn handle_mapping_enabled_changed(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    render_mapping_rows();
    log_debug!("[mouse] mappings master switch toggled");
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
            style_html_button(rec_btn, 0xFFFFFFADu32, 0x2E2E2EFFu32);
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
            style_html_button(combo_btn, 0xFFFFFFADu32, 0x2E2E2EFFu32);
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
            style_html_button(cancel, 0xFFFFFFC7u32, 0x2E2E2EFFu32);
            let cancel_ns = make_nsstring(&t("settings.recording_cancel"));
            let _: () = msg_send![cancel, setTitle: cancel_ns];
            CFRelease(cancel_ns as *const c_void);
            let _: () = msg_send![cancel, setTarget: target];
            let _: () = msg_send![cancel, setAction: sel!(handleMappingCancel:)];
            let _: () = msg_send![ve, addSubview: cancel];
            release_obj(cancel);
            let ok: *mut AnyObject = msg_send![class!(NSButton), alloc];
            let ok: *mut AnyObject = msg_send![ok, initWithFrame: NSRect::new(NSPoint::new(336.0, 24.0), NSSize::new(88.0, 28.0))];
            style_html_button(ok, 0x0A84FFFFu32, 0xFFFFFFFFu32);
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
    // 移除遮罩。
    // Remove the dim layer.
    if let Some(d) = EDIT_DIM.lock().unwrap().take() {
        unsafe {
            let _: () = msg_send![d.0, removeFromSuperview];
        }
    }
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
    log_debug!("[mouse] removed mapping for button {}", tag);
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
            log_debug!("[mouse] panel trigger recorded: button {}", btn);
        }
        // 面板录组合键:更新面板显示(Key Press 动作)。
        // The panel recorded the combo: update the panel (Key Press action).
        RecMode::PanelCombo => {
            let desc = REC_DESC.lock().unwrap().clone();
            *EDIT_COMBO.lock().unwrap() = desc.clone();
            unsafe {
                update_mapping_panel();
            }
            log_debug!("[mouse] panel combo recorded: {}", desc);
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
    log_debug!("[mouse] button-mapping recording cancelled");
}

pub(crate) extern "C" fn on_sidebar_select(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    let btn = sender as *mut AnyObject;
    let tag: isize = unsafe { msg_send![btn, tag] };
    select_sidebar(tag as usize);
}

/// 切换侧边栏选中页:高亮背景对齐到选中按钮、切换五个内容视图显隐、选中项粗体。
/// Switch the active settings page: align the highlight to the selected button, toggle the five
/// content views' visibility, and bold the selected item's label.
fn select_sidebar(idx: usize) {
    // tag 越界时回退到通用页 / fall back to the General page if the tag is out of range
    let idx = if idx > 4 { 0 } else { idx };
    SIDEBAR_SELECTED.store(idx, Ordering::SeqCst);
    unsafe {
        let ui = SETTINGS_UI.lock().unwrap();
        let ui = match ui.as_ref() {
            Some(u) => u,
            None => return,
        };
        let buttons = [
            ui.sidebar_general,
            ui.sidebar_switcher,
            ui.sidebar_mouse,
            ui.sidebar_clipboard,
            ui.sidebar_about,
        ];
        let views = [
            ui.general_view,
            ui.switcher_view,
            ui.mouse_view,
            ui.clipboard_view,
            ui.about_view,
        ];
        // 高亮背景对齐到选中按钮的 frame / align the highlight to the selected button's frame
        let frame: NSRect = msg_send![buttons[idx], frame];
        let _: () = msg_send![ui.sidebar_highlight, setFrame: frame];
        // 选中项使用强调色粗体，未选中项使用系统常规文本色。
        // Selected items use an accent-colored bold title; unselected items use the system label color.
        let titles = [
            t("settings.sidebar_general"),
            t("settings.sidebar_switcher"),
            t("settings.sidebar_mouse"),
            t("settings.sidebar_clipboard"),
            t("settings.sidebar_about"),
        ];
        for (i, &b) in buttons.iter().enumerate() {
            let layer: *mut AnyObject = msg_send![b, layer];
            if !layer.is_null() {
                layer_set_background(layer, crate::ffi::hex_to_cg_color(0x00000000u32));
            }
            set_sidebar_title(b, &titles[i], i == idx);
        }
        // 切换五页显隐 / toggle the five pages' visibility
        for (i, &v) in views.iter().enumerate() {
            let _: () = msg_send![v, setHidden: i != idx];
        }
        // 刚显示的页(如从隐藏切出来)需先排版,clip bounds 才会正确,随后滚到顶部。
        // A just-shown page needs a layout pass first so the clip bounds are correct;
        // then scroll it to the top. layoutIfNeeded lives on the window, not the scroll view.
        let _: () = msg_send![ui.window, layoutIfNeeded];
        scroll_page_to_top(views[idx]);
    }
}

/// 同步主题菜单标签并立即应用配置(主题/浮窗)。
/// Sync menu labels and apply the config immediately (theme / overlay).
fn apply_config_refresh() {
    refresh_menu_titles();
    invalidate_settings_window();
    crate::clipboard::refresh_localized_ui();
    apply_theme();
    refresh_highlight();
    update_status_label();
}

/// 红绿灯偏移常量:让最左侧红绿灯与下方品牌标题的首字母左边缘对齐。
/// 窗口坐标 y 向上,上移 = y 增大。
/// Traffic-light offset: align the left edge of the first button with the first letter of the
/// brand title below. Window coordinates point up, so upward = y+.
const TRAFFIC_LIGHT_DX: f64 = 8.0;
const TRAFFIC_LIGHT_DY: f64 = 4.0;
static TRAFFIC_LIGHT_BASE_ORIGINS: LazyLock<Mutex<HashMap<usize, [Option<NSPoint>; 3]>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 把三个红绿灯按钮往左上偏移:通过公开 API standardWindowButton: 拿到按钮视图直接改 frame
/// (没有公开 API 直接设红绿灯位置,旧私有 API setTrafficLightPosition: 等在 macOS 26 已移除,
/// 实测这是唯一可靠的做法)。
/// 注意:两参的 +standardWindowButton:forStyleMask: 是类方法,发给实例会被 objc2 的方法
/// 检查拦截崩掉(此前踩过的坑);必须用一参的实例方法 -standardWindowButton:。
/// 必须在窗口完成首次布局之后调用 —— 布局前移动会被 AppKit 重置;resize 也会重置,
/// 所以每次 show 和 resize 后都要重放(见 show_settings 与 resizeSubviewsWithOldSize:)。
/// 首次布局时记录系统原始坐标,后续始终从原始坐标计算,避免重复 show/resize 时累积偏移。
/// 按钮为 nil 时静默跳过,旧版 macOS 同样适用。
///
/// Nudge the three traffic-light buttons up-left: grab the button views via the public
/// -standardWindowButton: and move their frames (no public API sets the traffic light position;
/// the old private setTrafficLightPosition: etc. are gone on macOS 26, and this is the only
/// reliable way -- verified on this machine). Note: the two-arg +standardWindowButton:forStyleMask:
/// is a CLASS method; sending it to an instance trips objc2's method check and panics (a pitfall
/// we hit) -- the one-arg instance method -standardWindowButton: must be used. Must run after the
/// window's first layout pass: moves before layout are reset by AppKit, and resize also resets
/// them, so the offset is re-applied on every show and resize (see show_settings and
/// resizeSubviewsWithOldSize:). Original positions are captured once per window so repeated
/// calls remain idempotent instead of accumulating the offset. Skips silently when a button is
/// nil; works on older macOS too.
unsafe fn reposition_traffic_lights(window: *mut AnyObject) {
    // NSWindowButton: Close=0, Miniaturize=1, Zoom=2
    let base_origins = {
        let mut all_origins = TRAFFIC_LIGHT_BASE_ORIGINS.lock().unwrap();
        let origins = all_origins
            .entry(window as usize)
            .or_insert([None, None, None]);
        for tag in 0..3isize {
            let btn: *mut AnyObject = msg_send![window, standardWindowButton: tag];
            if !btn.is_null() && origins[tag as usize].is_none() {
                let f: NSRect = msg_send![btn, frame];
                origins[tag as usize] = Some(f.origin);
            }
        }
        *origins
    };
    for tag in 0..3isize {
        let btn: *mut AnyObject = msg_send![window, standardWindowButton: tag];
        if let (false, Some(base_origin)) = (btn.is_null(), base_origins[tag as usize]) {
            let _: () = msg_send![
                btn,
                setFrameOrigin: NSPoint::new(
                    base_origin.x + TRAFFIC_LIGHT_DX,
                    base_origin.y + TRAFFIC_LIGHT_DY
                )
            ];
        }
    }
}

/// 将设置窗口居中到当前鼠标所在屏幕的可用区域,排除菜单栏和 Dock。
/// Center the settings window in the visible area of the screen under the cursor, excluding the
/// menu bar and Dock.
unsafe fn center_settings_window(window: *mut AnyObject) {
    let cursor: NSPoint = msg_send![class!(NSEvent), mouseLocation];
    let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
    let count: usize = msg_send![screens, count];
    let mut visible_frame: Option<NSRect> = None;

    for i in 0..count {
        let screen: *mut AnyObject = msg_send![screens, objectAtIndex: i as isize];
        let frame: NSRect = msg_send![screen, frame];
        if cursor.x >= frame.origin.x
            && cursor.x <= frame.origin.x + frame.size.width
            && cursor.y >= frame.origin.y
            && cursor.y <= frame.origin.y + frame.size.height
        {
            visible_frame = Some(msg_send![screen, visibleFrame]);
            break;
        }
    }

    let visible = visible_frame.unwrap_or_else(|| {
        let main: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
        msg_send![main, visibleFrame]
    });
    let window_frame: NSRect = msg_send![window, frame];
    let origin = NSPoint::new(
        visible.origin.x + (visible.size.width - window_frame.size.width) / 2.0,
        visible.origin.y + (visible.size.height - window_frame.size.height) / 2.0,
    );
    let _: () = msg_send![window, setFrameOrigin: origin];
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
        // 每次打开都复位到通用页(窗口复用、隐藏不销毁)。
        // Reset to the General page on every open (the window is reused / hidden, not destroyed).
        select_sidebar(0);
        let ui = SETTINGS_UI.lock().unwrap();
        if let Some(u) = ui.as_ref() {
            // 切到 .regular:让设置窗口能正常激活抬升(从别的 App 顶部弹出来),关闭时切回。
            // Switch to .regular so the settings window can activate and raise itself above
            // the active app; reverted on close.
            crate::set_settings_activation_policy(true);
            let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![nsapp, activateIgnoringOtherApps: true];
            center_settings_window(u.window);
            let _: () = msg_send![u.window, makeKeyAndOrderFront: std::ptr::null::<AnyObject>()];
            // 红绿灯偏移:必须等窗口完成首次布局后再移动,否则会被 AppKit 重置。
            // Offset the traffic lights only after the window's first layout pass, or AppKit
            // resets them.
            let _: () = msg_send![u.window, layoutIfNeeded];
            reposition_traffic_lights(u.window);
            // Scroll the visible page (General on open) to the top after layout, so the scrollbar
            // starts at the top of the track instead of the middle.
            scroll_page_to_top(u.general_view);
            // 清掉默认 first responder,避免打开时焦点落在 Glass color 控件。
            // Clear the default first responder so focus does not land on the Glass color control on open.
            let _: bool = msg_send![u.window, makeFirstResponder: std::ptr::null::<AnyObject>()];
            // 按当前权限刷新警告条显隐(有权限就隐藏)/ refresh banner visibility by current permission
            let _: () =
                msg_send![u.accessibility_warning_view, setHidden: has_accessibility_permission()];
        }
    }
}

fn hide_settings() {
    let window_and_well = SETTINGS_UI
        .lock()
        .unwrap()
        .as_ref()
        .map(|u| (u.window, u.glass_tint));
    unsafe {
        if let Some((window, well)) = window_and_well {
            // 先释放设置锁再关闭颜色面板,通知回调会重新访问 SETTINGS_UI。
            // Release the settings lock before closing the color panel; its notification callback
            // re-enters SETTINGS_UI.
            close_glass_tint_panel(well);
            let _: () = msg_send![window, orderOut: std::ptr::null::<AnyObject>()];
        }
    }
    clear_glass_preview();
    // 切回 .accessory:设置窗口关闭,回到纯菜单栏(无 Dock 图标)。
    // Switch back to .accessory: the settings window is closed, return to pure menu-bar (no Dock icon).
    crate::set_settings_activation_policy(false);
}

/// 从窗口切换浮窗关闭本应用的设置窗口,必须在主线程直接执行,不能通过后台 AX 操作回调。
/// Close this app's settings window from the switcher. This must run directly on the main thread,
/// rather than indirectly through a background AX action callback.
pub(crate) fn close_settings_from_switcher() {
    hide_settings();
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
    log_debug!("Config form reset to defaults (not saved until OK).");
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
        GLASS_UI_UPDATE.store(true, Ordering::SeqCst);
        let tint =
            crate::ffi::hex_to_ns_color(crate::config::parse_hex8(&cfg.appearance.glass_tint));
        let _: () = msg_send![ui.glass_tint, setColor: tint];
        let panel: *mut AnyObject = msg_send![class!(NSColorPanel), sharedColorPanel];
        let _: () = msg_send![panel, setColor: tint];
        GLASS_UI_UPDATE.store(false, Ordering::SeqCst);
        set_field(ui.corner_radius, cfg.appearance.corner_radius);
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
        // 窗口显示模式 index 0 = 仅图标, 1 = 图标和缩略图。
        // Window display mode index 0 = icons only, 1 = icons and thumbnails.
        let th_idx: isize = if cfg.layout.thumbnails_enabled { 1 } else { 0 };
        let _: () = msg_send![ui.thumbnails_enabled, selectItemAtIndex: th_idx];
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
        let _: () = msg_send![
            ui.update_auto_check,
            setState: if cfg.updates.automatically_check { 1isize } else { 0isize }
        ];

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
        // pin_follow_selection:下拉框 index 0 = 跟随置顶, 1 = 保持当前位置。
        // pin_follow_selection: popup index 0 = follow, 1 = keep.
        let pin_idx: isize = if cfg.clipboard.pin_follow_selection {
            0
        } else {
            1
        };
        let _: () = msg_send![ui.clipboard_pin_follow, selectItemAtIndex: pin_idx];
    }
    crate::config::set_glass_style_preview(Some(cfg.appearance.glass_style.clone()));
    crate::config::set_glass_tint_preview(Some(cfg.appearance.glass_tint.clone()));
    apply_glass_preview();
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
        let color: *mut AnyObject = msg_send![ui.glass_tint, color];
        if let Some(hex) = ns_color_to_hex(color) {
            cfg.appearance.glass_tint = hex;
        }
        match parse_f64(&nsstring_to_rust(msg_send![ui.corner_radius, stringValue])) {
            Ok(v) => cfg.appearance.corner_radius = v,
            Err(_) => errs.push(tf(
                "errors.not_a_number",
                &[("field", "appearance.corner_radius")],
            )),
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
        // 窗口显示模式 index 0 = 仅图标, 1 = 图标和缩略图。
        // Window display mode index 0 = icons only, 1 = icons and thumbnails.
        let th_idx: isize = msg_send![ui.thumbnails_enabled, indexOfSelectedItem];
        cfg.layout.thumbnails_enabled = th_idx == 1;
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
        let update_state: isize = msg_send![ui.update_auto_check, state];
        cfg.updates.automatically_check = update_state == 1;

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
        // pin_follow_selection:下拉框 index 0 = 跟随置顶, 1 = 保持当前位置。
        // pin_follow_selection: popup index 0 = follow, 1 = keep.
        let pin_idx: isize = msg_send![ui.clipboard_pin_follow, indexOfSelectedItem];
        cfg.clipboard.pin_follow_selection = pin_idx != 1;
    }
    for e in cfg.validate() {
        errs.push(e);
    }
    (cfg, errs)
}

/// 构建设置窗口(只建一次,存入 SETTINGS_UI,之后复用、隐藏而非销毁)。
/// Build the settings window once, store it in SETTINGS_UI, then reuse (hide, not destroy).
// 设置窗口自定义子类 OhMyTabSettingsWindow:重写 performClose:/close,让红色关闭按钮和
// 直接关闭路径都走 hide_settings(切回 .accessory),而不是默认的 orderOut(那样不会触发
// 激活策略切换,导致 Dock 图标残留,也不会清理独立的共享取色面板)。
// create_settings_window 在 invalidate 后可能被再次调用,故用 OnceLock 守卫只注册一次。
// Custom settings window subclass overriding performClose:/close so both the red close button and
// direct close paths route through hide_settings (which flips activation policy back to
// .accessory), instead of the default orderOut (which would not trigger the policy switch or
// clean up the independent shared color panel). create_settings_window can be called again after
// invalidate_settings_window, so registration is guarded with OnceLock.
extern "C" fn settings_window_perform_close(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    hide_settings();
}

extern "C" fn settings_window_close(_self: *mut c_void, _cmd: Sel) {
    hide_settings();
}

// Cmd+Q 退出的常量:NSEventModifierFlagCommand = 1 << 20,ANSI Q 的 keyCode = 12。
// Constants for Cmd+Q handling: NSEventModifierFlagCommand = 1 << 20, ANSI Q keyCode = 12.
const NSEVENT_MODIFIER_FLAG_COMMAND: u64 = 1 << 20;
const KEYCODE_Q: u16 = 12;
const NSEVENT_TYPE_LEFT_MOUSE_DOWN: usize = 1;

/// End inline text editing when the user clicks elsewhere in the settings window.
///
/// The settings window uses a borderless collection of plain NSViews as its page background.
/// Those views do not become first responder themselves, so AppKit otherwise leaves an
/// NSTextField editor active after a click on empty page space. Resigning before dispatching the
/// mouse event lets the clicked control become first responder again when appropriate.
extern "C" fn settings_window_send_event(_self: *mut c_void, _cmd: Sel, event: *mut AnyObject) {
    unsafe {
        if !event.is_null() {
            let event_type: usize = msg_send![event, type];
            if event_type == NSEVENT_TYPE_LEFT_MOUSE_DOWN {
                let window = _self as *mut AnyObject;
                let first_responder: *mut AnyObject = msg_send![window, firstResponder];
                if !first_responder.is_null() {
                    let is_text_field: bool =
                        msg_send![first_responder, isKindOfClass: class!(NSTextField)];
                    // While an NSTextField is being edited, AppKit installs its shared
                    // NSTextView field editor as the window's first responder.
                    let is_field_editor: bool =
                        msg_send![first_responder, isKindOfClass: class!(NSTextView)];
                    if is_text_field || is_field_editor {
                        let _: bool =
                            msg_send![window, makeFirstResponder: std::ptr::null::<AnyObject>()];
                    }
                }
            }
        }
        let _: () = msg_send![
            super(
                _self as *mut AnyObject,
                objc2::runtime::AnyClass::get(c"NSWindow").unwrap()
            ),
            sendEvent: event
        ];
    }
}

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
            let types_close = CString::new("v@:").unwrap(); // -close -> void
            class_addMethod(
                cls,
                sel!(close),
                settings_window_close as *mut c_void,
                types_close.as_ptr(),
            );
            let types_event = CString::new("v@:@").unwrap(); // -sendEvent:(NSEvent*) -> void
            class_addMethod(
                cls,
                sel!(sendEvent:),
                settings_window_send_event as *mut c_void,
                types_event.as_ptr(),
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
        // Keep the original compact settings window dimensions while applying the redesign's
        // typography, spacing, controls, and grouped-card treatment.
        // 保持原来的紧凑窗口尺寸，同时应用 redesign 的字体、间距、控件和分组卡片风格。
        // Give the redesigned detail pane enough room for full labels, links, and wide fields
        // while keeping the sidebar and the existing window height unchanged.
        let view_w = 820.0;
        let card_margin = 0.0;
        let card_w = 220.0;
        let window_clip_radius = 26.0;
        let card_radius = 0.0;
        let style: u64 = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 3);
        // titled + closable + miniaturizable + resizable(三个红绿灯齐全)。resizable 是绿色 zoom
        // 按钮出现的必要条件;布局是绝对定位不随缩放,故下方用 min=max 固定窗口尺寸。
        // titled + closable + miniaturizable + resizable (all three traffic lights). resizable is
        // required for the green zoom button to appear; the layout is absolute-positioned and
        // doesn't adapt, so the window size is fixed below via min=max.
        // The page content uses generous spacing and grouped cards so the controls remain easy
        // to scan without compressing the taller sections.
        // 初始位置:主显示器(screens[0])居中。不要用 NSScreen mainScreen(其语义是跟随
        // 键盘焦点窗口的屏幕,不是主屏,见 overlay_target_screen 的注释)。
        // Initial position: centered on the primary display (screens[0]). Don't use
        // NSScreen.mainScreen (it follows the key window, not the primary display; see
        // overlay_target_screen's note).
        let win_w = view_w;
        let win_h = 720.0;
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
                                                                   // The scroll region spans from the footer to the window's top edge (fullSizeContentView),
                                                                   // so the content scrolls all the way up behind the traffic-light strip — matching the
                                                                   // HTML, where the right panel's scroll area reaches the window border. content_h is the
                                                                   // full content height (the traffic lights sit over the sidebar, not the detail pane).
        let page_viewport_h = (content_h - 62.0).max(1.0);

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
        // are window chrome outside contentView (not clipped); the card's 10pt margin stays clear
        // of the corner zone.
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
            sidebar_switcher: std::ptr::null_mut(),
            sidebar_mouse: std::ptr::null_mut(),
            sidebar_clipboard: std::ptr::null_mut(),
            sidebar_about: std::ptr::null_mut(),
            sidebar_highlight: std::ptr::null_mut(),
            general_view: std::ptr::null_mut(),
            switcher_view: std::ptr::null_mut(),
            mouse_view: std::ptr::null_mut(),
            clipboard_view: std::ptr::null_mut(),
            about_view: std::ptr::null_mut(),
            glass_style: std::ptr::null_mut(),
            glass_tint: std::ptr::null_mut(),
            glass_preview_switcher: std::ptr::null_mut(),
            glass_preview_clipboard: std::ptr::null_mut(),
            corner_radius: std::ptr::null_mut(),
            thumbnails_enabled: std::ptr::null_mut(),
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
            mapping_card: std::ptr::null_mut(),
            mapping_panel: std::ptr::null_mut(),
            mapping_rows: Vec::new(),
            clipboard_enabled: std::ptr::null_mut(),
            clipboard_persist: std::ptr::null_mut(),
            clipboard_move_used_to_top: std::ptr::null_mut(),
            clipboard_max_entries: std::ptr::null_mut(),
            clipboard_auto_expire_days: std::ptr::null_mut(),
            clipboard_show_source_app: std::ptr::null_mut(),
            clipboard_pin_follow: std::ptr::null_mut(),
            add_mapping_button: std::ptr::null_mut(),
            mapping_enabled: std::ptr::null_mut(),
            mapping_empty: std::ptr::null_mut(),
            device_indicator: std::ptr::null_mut(),
            ok_button: std::ptr::null_mut(),
            accessibility_warning_view: std::ptr::null_mut(),
            update_auto_check: std::ptr::null_mut(),
        };

        // The sidebar and detail pane meet directly at the original sidebar boundary; their
        // backgrounds provide the visual split instead of an inset outer card.
        // 左侧导航和右侧详情直接衔接，通过两种背景色区分，不再使用内缩的外框卡片。
        let content_x = card_w;
        let detail_w = view_w - content_x;
        let page_inset = 32.0;
        let page_x = content_x + page_inset;
        let content_w = detail_w - page_inset * 2.0;
        let label_x = 12.0;
        let label_w = 220.0;
        let ctrl_w = 200.0;
        let ctrl_x = content_w - ctrl_w - 12.0;
        // HTML `.row` uses a 34pt control and roughly 54pt visual row; the native labels and
        // controls share that rhythm while grouped cards provide the surrounding padding.
        let row_h = 34.0;
        let described_row_h = 54.0;

        let target = match *MENU_TARGET.lock().unwrap() {
            Some(t) => t.0,
            None => return,
        };

        // --- 侧边栏 sidebar(悬浮玻璃卡片,系统设置同款观感)---
        // macOS 26+ 用 NSGlassEffectView(Liquid Glass,不设 tint 用系统默认);
        // 旧版用 NSVisualEffectView + sidebar 材质(经典磨砂侧边栏)。
        // --- Sidebar: a flat navigation column, matching the reference layout ---
        // macOS 26+ uses NSGlassEffectView (Liquid Glass, system default tint);
        // older macOS uses NSVisualEffectView with the sidebar material (classic frosted look).
        // The glass material supplies the subtle separation from the content pane.
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
        // Keep the navigation pane a distinct light-gray surface, while the detail pane uses the
        // window background. This mirrors the HTML reference's two-pane split without an inset
        // border around the whole settings area.
        let sidebar_layer: *mut AnyObject = msg_send![sidebar_view, layer];
        if !sidebar_layer.is_null() {
            layer_set_background(sidebar_layer, crate::ffi::hex_to_cg_color(0xF2F2F4DBu32));
        }
        // 自适应:左侧锚定、高度随窗口拉伸(HeightSizable|MaxXMargin = 16|4 = 20)。
        // Adaptive: left-anchored, height stretches with the window.
        let _: () = msg_send![sidebar_view, setAutoresizingMask: 20u64];
        let _: () = msg_send![content, addSubview: sidebar_view];
        release_obj(sidebar_view);

        // HTML `.sidebar { border-right: 1px solid rgba(0,0,0,.055) }`.
        let sidebar_divider: *mut AnyObject = msg_send![class!(NSView), alloc];
        let sidebar_divider: *mut AnyObject = msg_send![
            sidebar_divider,
            initWithFrame: NSRect::new(
                NSPoint::new(card_w - 1.0, 0.0),
                NSSize::new(1.0, content_h)
            )
        ];
        let _: () = msg_send![sidebar_divider, setWantsLayer: true];
        let divider_layer: *mut AnyObject = msg_send![sidebar_divider, layer];
        if !divider_layer.is_null() {
            layer_set_background(divider_layer, crate::ffi::hex_to_cg_color(0x0000000Eu32));
        }
        let _: () = msg_send![sidebar_divider, setAutoresizingMask: 20u64];
        let _: () = msg_send![content, addSubview: sidebar_divider];
        release_obj(sidebar_divider);

        // The right detail pane has its own white surface, directly beside the gray sidebar.
        let main_background: *mut AnyObject = msg_send![class!(NSView), alloc];
        let main_background: *mut AnyObject = msg_send![
            main_background,
            initWithFrame: NSRect::new(
                NSPoint::new(content_x, 0.0),
                NSSize::new(detail_w, content_h)
            )
        ];
        let _: () = msg_send![main_background, setWantsLayer: true];
        let main_layer: *mut AnyObject = msg_send![main_background, layer];
        if !main_layer.is_null() {
            layer_set_background(main_layer, crate::ffi::hex_to_cg_color(0xFFFFFFB0u32));
        }
        let _: () = msg_send![main_background, setAutoresizingMask: 18u64];
        let _: () = msg_send![content, addSubview: main_background];
        release_obj(main_background);

        // HTML `.footer`: a 62pt bottom bar with its own light surface and hairline separator.
        // It lives inside the detail pane and stays pinned to the bottom while the page grows.
        let footer: *mut AnyObject = msg_send![class!(NSView), alloc];
        let footer: *mut AnyObject = msg_send![
            footer,
            initWithFrame: NSRect::new(
                NSPoint::new(content_x, 0.0),
                NSSize::new(detail_w, 62.0),
            )
        ];
        let _: () = msg_send![footer, setWantsLayer: true];
        let footer_layer: *mut AnyObject = msg_send![footer, layer];
        if !footer_layer.is_null() {
            layer_set_background(footer_layer, crate::ffi::hex_to_cg_color(0xF8F8F9D1u32));
        }
        let footer_line: *mut AnyObject = msg_send![class!(NSView), alloc];
        let footer_line: *mut AnyObject = msg_send![
            footer_line,
            initWithFrame: NSRect::new(
                NSPoint::new(0.0, 61.0),
                NSSize::new(detail_w, 1.0),
            )
        ];
        let _: () = msg_send![footer_line, setWantsLayer: true];
        let footer_line_layer: *mut AnyObject = msg_send![footer_line, layer];
        if !footer_line_layer.is_null() {
            layer_set_background(
                footer_line_layer,
                crate::ffi::hex_to_cg_color(0x00000012u32),
            );
        }
        let _: () = msg_send![footer, addSubview: footer_line];
        release_obj(footer_line);
        let _: () = msg_send![footer, setAutoresizingMask: 18u64];
        let _: () = msg_send![content, addSubview: footer];
        release_obj(footer);

        // Sidebar identity block, matching the redesign's app title and subtitle above the nav.
        let app_title: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let app_title: *mut AnyObject = msg_send![
            app_title,
            initWithFrame: NSRect::new(
                // Sidebar content spans the full content view, including the unified toolbar
                // strip where the traffic lights live. Anchor the identity block to that full
                // height so it follows the HTML sidebar's compact top padding instead of being
                // pushed down by the toolbar's contentLayoutRect inset.
                NSPoint::new(24.0, content_h - 74.0),
                NSSize::new(card_w - 48.0, 22.0)
            )
        ];
        set_field(app_title, "Oh My Tab");
        let _: () = msg_send![app_title, setBezeled: false];
        let _: () = msg_send![app_title, setDrawsBackground: false];
        let _: () = msg_send![app_title, setEditable: false];
        let app_title_font: *mut AnyObject =
            msg_send![class!(NSFont), boldSystemFontOfSize: 15.0f64];
        let _: () = msg_send![app_title, setFont: app_title_font];
        let app_title_color: *mut AnyObject = msg_send![class!(NSColor), labelColor];
        let _: () = msg_send![app_title, setTextColor: app_title_color];
        let _: () = msg_send![sidebar_view, addSubview: app_title];
        release_obj(app_title);
        let app_subtitle: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let app_subtitle: *mut AnyObject = msg_send![
            app_subtitle,
            initWithFrame: NSRect::new(
                NSPoint::new(24.0, content_h - 94.0),
                NSSize::new(card_w - 48.0, 18.0)
            )
        ];
        set_field(app_subtitle, t("settings.window_title"));
        let _: () = msg_send![app_subtitle, setBezeled: false];
        let _: () = msg_send![app_subtitle, setDrawsBackground: false];
        let _: () = msg_send![app_subtitle, setEditable: false];
        let app_subtitle_font: *mut AnyObject =
            msg_send![class!(NSFont), systemFontOfSize: 11.5f64];
        let _: () = msg_send![app_subtitle, setFont: app_subtitle_font];
        let app_subtitle_color: *mut AnyObject = msg_send![class!(NSColor), tertiaryLabelColor];
        let _: () = msg_send![app_subtitle, setTextColor: app_subtitle_color];
        let _: () = msg_send![sidebar_view, addSubview: app_subtitle];
        release_obj(app_subtitle);

        // 侧边栏选中行的高亮背景(layer-backed NSView,theme 感知色),先于按钮加入以便按钮文字叠在上层。
        // Highlight background for the selected sidebar row (layer-backed NSView, theme-aware color);
        // added before the buttons so button titles draw on top of it.
        // 卡片内布局:内边距 12;按钮顶边按完整侧边栏高度定位,靠近红绿灯
        // (btn_y0 为卡片坐标系)。
        // Card-local layout: 12pt inner margins. The buttons stay close to the traffic lights;
        // btn_y0 is anchored to the full sidebar height rather than the toolbar-inset height.
        let btn_w = card_w - 28.0;
        let btn_h = 38.0;
        // Sidebar navigation is also anchored to the full-height sidebar. Using layout_h here
        // includes the toolbar inset a second time and leaves a large blank gap above the title.
        let btn_y0 = content_h - card_margin - 112.0 - btn_h;
        let highlight: *mut AnyObject = msg_send![class!(NSView), alloc];
        let highlight: *mut AnyObject = msg_send![highlight, initWithFrame: NSRect::new(NSPoint::new(14.0, btn_y0), NSSize::new(btn_w, btn_h))];
        let _: () = msg_send![highlight, setAutoresizingMask: 12u64]; // 贴顶、贴左 / top- and left-anchored
        let _: () = msg_send![highlight, setWantsLayer: true];
        let hl_layer: *mut AnyObject = msg_send![highlight, layer];
        let _: () = msg_send![hl_layer, setCornerRadius: 10.0f64];
        // 选中高亮用系统强调色(controlAccentColor),与 NSSwitch 开启的蓝色一致
        // (LinearMouse 侧边栏选中高亮同款)。
        // Selection highlight uses the system accent color (controlAccentColor), matching the
        // NSSwitch's on-state blue (same as LinearMouse's sidebar selection highlight).
        // The redesign uses a soft accent wash for the active row rather than a solid blue fill.
        layer_set_background(hl_layer, crate::ffi::hex_to_cg_color(0x0A84FF1Fu32));
        let _: () = msg_send![sidebar_view, addSubview: highlight];
        release_obj(highlight);
        ui.sidebar_highlight = highlight;

        // Five sidebar buttons (borderless, tags 0..4; click triggers handleSettingsSidebar:).
        ui.sidebar_general = make_sidebar_button(
            sidebar_view,
            target,
            &t("settings.sidebar_general"),
            0,
            14.0,
            btn_y0,
            btn_w,
        );
        ui.sidebar_switcher = make_sidebar_button(
            sidebar_view,
            target,
            &t("settings.sidebar_switcher"),
            1,
            14.0,
            btn_y0 - 42.0,
            btn_w,
        );
        ui.sidebar_mouse = make_sidebar_button(
            sidebar_view,
            target,
            &t("settings.sidebar_mouse"),
            2,
            14.0,
            btn_y0 - 84.0,
            btn_w,
        );
        ui.sidebar_clipboard = make_sidebar_button(
            sidebar_view,
            target,
            &t("settings.sidebar_clipboard"),
            3,
            14.0,
            btn_y0 - 126.0,
            btn_w,
        );
        ui.sidebar_about = make_sidebar_button(
            sidebar_view,
            target,
            &t("settings.sidebar_about"),
            4,
            14.0,
            btn_y0 - 168.0,
            btn_w,
        );

        // HTML `.sidebar-footer`: separator plus a compact Restore Defaults button at the
        // bottom of the navigation column, rather than in the main footer.
        let sidebar_footer_line: *mut AnyObject = msg_send![class!(NSView), alloc];
        let sidebar_footer_line: *mut AnyObject = msg_send![
            sidebar_footer_line,
            initWithFrame: NSRect::new(
                NSPoint::new(14.0, 61.0),
                NSSize::new(card_w - 28.0, 1.0),
            )
        ];
        let _: () = msg_send![sidebar_footer_line, setWantsLayer: true];
        let sidebar_footer_layer: *mut AnyObject = msg_send![sidebar_footer_line, layer];
        if !sidebar_footer_layer.is_null() {
            layer_set_background(
                sidebar_footer_layer,
                crate::ffi::hex_to_cg_color(0x0000000Fu32),
            );
        }
        let _: () = msg_send![sidebar_view, addSubview: sidebar_footer_line];
        release_obj(sidebar_footer_line);
        let restore = make_settings_action_button(
            NSRect::new(NSPoint::new(22.0, 20.0), NSSize::new(card_w - 44.0, 30.0)),
            &t("settings.btn_restore_defaults"),
            target,
            sel!(handleRestoreDefaults:),
        );
        let _: () = msg_send![restore, setAutoresizingMask: 36u64];
        let _: () = msg_send![sidebar_view, addSubview: restore];
        release_obj(restore);

        // The scroll view spans from the left gutter to the detail pane's right edge, so the
        // overlay scrollbar sits flush with the window edge (matching the OK/Cancel footer) and
        // no longer floats 32pt in from the right. The content keeps its 32pt gutter margins
        // inside the document, so only the scrollbar's position changes.
        let page_frame = NSRect::new(
            NSPoint::new(page_x, 62.0),
            NSSize::new(detail_w - page_inset, page_viewport_h),
        );
        let general_doc_h = 820.0;
        let switcher_doc_h = 840.0;
        let mouse_doc_h = 1240.0;
        let clipboard_doc_h = 700.0;
        let about_doc_h = 700.0;

        let (general_root, general_view) =
            make_settings_page(content, page_frame, general_doc_h, false);
        ui.general_view = general_root;
        let (switcher_root, switcher_view) =
            make_settings_page(content, page_frame, switcher_doc_h, true);
        ui.switcher_view = switcher_root;
        let (mouse_root, mouse_view) = make_settings_page(content, page_frame, mouse_doc_h, true);
        ui.mouse_view = mouse_root;
        let (clipboard_root, clipboard_view) =
            make_settings_page(content, page_frame, clipboard_doc_h, true);
        ui.clipboard_view = clipboard_root;
        let (about_root, about_view) = make_settings_page(content, page_frame, about_doc_h, true);
        ui.about_view = about_root;

        // ===== 通用页内容 general page content =====
        let general_top = general_doc_h - 24.0;
        let mut y = general_top; // top cursor: bottom edge of the next element
        add_page_title(
            general_view,
            &t("settings.sidebar_general"),
            label_x,
            y - 34.0,
            content_w - label_x * 2.0,
        );
        y -= 62.0;

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
                NSPoint::new(0.0, general_top - banner_h),
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
        let open_btn = make_settings_action_button(
            NSRect::new(
                NSPoint::new(content_w - 150.0, (banner_h - 28.0) / 2.0),
                NSSize::new(140.0, 28.0),
            ),
            &t("settings.btn_open_privacy"),
            target,
            sel!(handleOpenPrivacy:),
        );
        let _: () = msg_send![banner, addSubview: open_btn];
        release_obj(open_btn);

        // 默认按当前权限显隐(有权限就隐藏)/ initial visibility: hidden when permission is already granted
        let _: () = msg_send![banner, setHidden: has_accessibility_permission()];

        // --- 外观 Appearance ---
        y -= 12.0;
        let appearance_header_y = y;
        add_header(
            general_view,
            &t("settings.header_appearance"),
            label_x,
            y,
            content_w - 2.0 * label_x,
        );
        y -= 8.0 + described_row_h;
        ui.glass_style = add_described_row(
            general_view,
            label_x,
            y,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_glass_style"),
            &t("settings.desc_glass_style"),
            make_popup(ctrl_x, y + 10.0, ctrl_w, row_h, &["regular", "clear"], 0),
        );
        let _: () = msg_send![ui.glass_style, setTarget: target];
        let _: () = msg_send![ui.glass_style, setAction: sel!(handleGlassStyleChanged:)];
        y -= described_row_h;
        add_row_separator(general_view, 0.0, y + described_row_h, content_w);
        ui.glass_tint = add_described_row(
            general_view,
            label_x,
            y,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_glass_tint"),
            &t("settings.desc_glass_tint"),
            make_color_well(
                ctrl_x,
                y + 10.0,
                ctrl_w,
                row_h,
                &Config::default().appearance.glass_tint,
                target,
            ),
        );
        configure_glass_tint_panel(target);

        // --- 实时预览 Live preview ---
        y -= 14.0 + 24.0;
        add_header(
            general_view,
            &t("settings.header_preview"),
            label_x,
            y,
            content_w - 2.0 * label_x,
        );
        y -= 8.0 + row_h;
        let preview_h = 90.0;
        let preview_y = y - preview_h;
        let preview_w = (content_w - 2.0 * label_x - 12.0) / 2.0;
        let right_preview_x = label_x + preview_w + 12.0;
        add_preview_caption(
            general_view,
            &t("settings.preview_switcher"),
            label_x,
            preview_y + preview_h + 3.0,
            preview_w,
        );
        add_preview_caption(
            general_view,
            &t("settings.preview_clipboard"),
            right_preview_x,
            preview_y + preview_h + 3.0,
            preview_w,
        );
        ui.glass_preview_switcher =
            make_glass_preview(general_view, label_x, preview_y, preview_w, preview_h, true);
        ui.glass_preview_clipboard = make_glass_preview(
            general_view,
            right_preview_x,
            preview_y,
            preview_w,
            preview_h,
            false,
        );
        y = preview_y;
        add_settings_card(
            general_view,
            NSRect::new(
                NSPoint::new(6.0, preview_y - 12.0),
                NSSize::new(
                    content_w - 12.0,
                    (appearance_header_y + 2.0) - (preview_y - 12.0),
                ),
            ),
        );

        // --- 语言 Language ---
        y -= 14.0 + 24.0;
        let language_header_y = y;
        add_header(
            general_view,
            &t("settings.header_language"),
            label_x,
            y,
            content_w - 2.0 * label_x,
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
        add_settings_card(
            general_view,
            NSRect::new(
                NSPoint::new(6.0, y - 10.0),
                NSSize::new(content_w - 12.0, (language_header_y + 2.0) - (y - 10.0)),
            ),
        );

        // --- 日志 Logging ---
        y -= 14.0 + 24.0;
        let logging_header_y = y;
        add_header(
            general_view,
            &t("settings.header_logging"),
            label_x,
            y,
            content_w - 2.0 * label_x,
        );
        y -= 8.0 + described_row_h;
        // 日志级别下拉框:项 = [debug, info];默认 index 1(info)。
        // Log level popup: items = [debug, info]; default index 1 (info).
        let log_levels: [&str; 2] = ["debug", "info"];
        ui.log_level = add_described_row(
            general_view,
            label_x,
            y,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_log_level"),
            &t("settings.desc_log_level"),
            make_popup(ctrl_x, y + 10.0, ctrl_w, row_h, &log_levels, 1),
        );
        add_settings_card(
            general_view,
            NSRect::new(
                NSPoint::new(6.0, y - 10.0),
                NSSize::new(content_w - 12.0, (logging_header_y + 2.0) - (y - 10.0)),
            ),
        );

        // --- 启动 Startup ---
        y -= 14.0 + 24.0;
        let startup_header_y = y;
        add_header(
            general_view,
            &t("settings.header_startup"),
            label_x,
            y,
            content_w - 2.0 * label_x,
        );
        y -= 8.0 + described_row_h;
        // 开机自启开关:标题留空(左侧 row label 已说明),仅放一个 switch。
        // Launch-at-login switch: no title (the row label on the left already describes it).
        ui.launch_at_login = add_described_row(
            general_view,
            label_x,
            y,
            content_w - label_x * 2.0 - 58.0,
            described_row_h,
            &t("settings.row_launch_at_login"),
            &t("settings.desc_launch_at_login"),
            make_switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
        );
        add_settings_card(
            general_view,
            NSRect::new(
                NSPoint::new(6.0, y - 10.0),
                NSSize::new(content_w - 12.0, (startup_header_y + 2.0) - (y - 10.0)),
            ),
        );

        // ===== 应用切换浮窗页内容 switcher overlay page content =====
        let mut y = switcher_doc_h - 24.0;
        add_page_title(
            switcher_view,
            &t("settings.sidebar_switcher"),
            label_x,
            y - 34.0,
            content_w - label_x * 2.0,
        );
        y -= 62.0;

        // --- 窗口 Window ---
        y -= 12.0;
        let windows_header_y = y;
        add_header(
            switcher_view,
            &t("settings.header_windows"),
            label_x,
            y,
            content_w - 2.0 * label_x,
        );
        y -= 8.0 + described_row_h;
        // 窗口切换总开关:关闭后 Cmd+Tab 透传给系统(原生切换器接管)。
        // App-switcher master switch: off = Cmd+Tab passes through to the system.
        ui.windows_enabled = add_tall_row(
            switcher_view,
            label_x,
            y,
            label_w,
            &t("settings.row_windows_enabled"),
            make_switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
        )
        .1;
        y -= 8.0 + described_row_h;
        add_row_separator(switcher_view, 0.0, y + described_row_h + 3.0, content_w);
        // show_minimized 开关(切换器语义本就只有显/隐两态,用 Toggle 比下拉更直观)。
        // 英文标签较长,该行标签加宽;开关仍与 popup 右缘对齐。
        // show_minimized is inherently two-state, so a toggle is clearer than a popup. The long
        // English label uses a wider label column, while the switch stays aligned to the popups.
        ui.show_minimized = add_tall_row(
            switcher_view,
            label_x,
            y,
            220.0,
            &t("settings.row_show_minimized"),
            make_switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
        )
        .1;
        y -= 8.0 + described_row_h;
        add_row_separator(switcher_view, 0.0, y + described_row_h + 3.0, content_w);
        // 窗口显示模式:仅图标或图标和缩略图;配置仍由 thumbnails_enabled 布尔值保存。
        // Window display mode: icons only or icons and thumbnails; the config remains stored as
        // the thumbnails_enabled boolean.
        let window_display_mode_labels = [
            t("settings.window_display_mode_icons"),
            t("settings.window_display_mode_icons_thumbnails"),
        ];
        let window_display_mode_refs: Vec<&str> = window_display_mode_labels
            .iter()
            .map(|s| s.as_str())
            .collect();
        ui.thumbnails_enabled = add_tall_row(
            switcher_view,
            label_x,
            y,
            220.0,
            &t("settings.row_window_display_mode"),
            make_popup(
                ctrl_x,
                y + 10.0,
                ctrl_w,
                row_h,
                &window_display_mode_refs,
                0,
            ),
        )
        .1;
        y -= 8.0 + described_row_h;
        add_row_separator(switcher_view, 0.0, y + described_row_h + 3.0, content_w);
        // overlay_position 下拉框:项 = [跟随激活窗口, 始终显示在主屏幕];默认 index 0。
        // overlay_position popup: [Follow Active Window, Always on Main Screen]; default index 0.
        let op_labels = [
            t("settings.overlay_position_follow_active"),
            t("settings.overlay_position_main_screen"),
        ];
        let op_label_refs: Vec<&str> = op_labels.iter().map(|s| s.as_str()).collect();
        ui.overlay_position = add_tall_row(
            switcher_view,
            label_x,
            y,
            label_w,
            &t("settings.row_overlay_position"),
            make_popup(ctrl_x, y + 10.0, ctrl_w, row_h, &op_label_refs, 0),
        )
        .1;
        y -= 8.0 + described_row_h;
        add_row_separator(switcher_view, 0.0, y + described_row_h + 3.0, content_w);
        ui.corner_radius = add_tall_row(
            switcher_view,
            label_x,
            y,
            label_w,
            &t("settings.row_corner_radius"),
            make_text_input(ctrl_x, y + 10.0, ctrl_w, row_h, "64"),
        )
        .1;
        add_settings_card(
            switcher_view,
            NSRect::new(
                NSPoint::new(6.0, y - 10.0),
                NSSize::new(content_w - 12.0, (windows_header_y + 2.0) - (y - 10.0)),
            ),
        );

        // --- 键盘 Keyboard ---
        y -= 14.0 + 24.0;
        let keyboard_header_y = y;
        add_header(
            switcher_view,
            &t("settings.header_keyboard"),
            label_x,
            y,
            content_w - 2.0 * label_x,
        );
        y -= 8.0 + described_row_h;
        // 修饰键下拉项:显示 Option+Tab / Command+Tab;值由索引映射到 option/command。
        // Modifier popup shows Option+Tab / Command+Tab; the index maps to option/command.
        let mod_labels = [
            t("settings.modifier_option"),
            t("settings.modifier_command"),
        ];
        let mod_label_refs: Vec<&str> = mod_labels.iter().map(|s| s.as_str()).collect();
        ui.modifier = add_tall_row(
            switcher_view,
            label_x,
            y,
            label_w,
            &t("settings.row_modifier"),
            make_popup(ctrl_x, y + 10.0, ctrl_w, row_h, &mod_label_refs, 0),
        )
        .1;
        add_settings_card(
            switcher_view,
            NSRect::new(
                NSPoint::new(6.0, y - 10.0),
                NSSize::new(content_w - 12.0, (keyboard_header_y + 2.0) - (y - 10.0)),
            ),
        );

        // ===== 鼠标页内容 mouse page content =====
        let mut y = mouse_doc_h - 24.0;
        add_page_title(
            mouse_view,
            &t("settings.sidebar_mouse"),
            label_x,
            y - 34.0,
            content_w - label_x * 2.0,
        );
        y -= 62.0;

        // --- 启用鼠标控制(总开关,置于最顶) / Enable mouse control (topmost) ---
        y -= 8.0 + described_row_h;
        let enable_mouse_bottom = y;
        ui.enable_mouse = add_described_row(
            mouse_view,
            label_x,
            y,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_enable_mouse"),
            &t("settings.desc_enable_mouse"),
            make_switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
        );
        // switch toggle 时实时更新 OK 按钮标题(确认 vs 确认并重启)。
        // Update OK button title in real time when the switch toggles (OK vs OK && Restart).
        let _: () = msg_send![ui.enable_mouse, setTarget: target];
        let _: () = msg_send![ui.enable_mouse, setAction: sel!(handleEnableMouseToggle:)];
        add_settings_card(
            mouse_view,
            NSRect::new(
                NSPoint::new(6.0, enable_mouse_bottom - 10.0),
                NSSize::new(content_w - 12.0, described_row_h + 20.0),
            ),
        );

        // --- 设备选择器(内嵌下拉框,切换即时刷新其余控件) / Device picker (inline popup) ---
        y -= 14.0 + 24.0;
        let device_header_y = y;
        add_header(
            mouse_view,
            &t("settings.header_mouse_device"),
            label_x,
            y,
            content_w - 2.0 * label_x,
        );
        y -= 8.0 + described_row_h;
        // 下拉框:items 在 load_settings_values 里动态重建(设备列表可变)。
        // 首次创建放一个占位项,真正的内容在 load_settings_values -> rebuild_device_popup 填入。
        // Popup: items are rebuilt dynamically in load_settings_values (device list is mutable).
        // A placeholder is inserted here; the real items are filled by rebuild_device_popup.
        let dev_popup = make_popup(ctrl_x, y + 10.0, ctrl_w, row_h, &[""], 0);
        style_flat_popup(dev_popup);
        // 绑定 target/action:选择变化时即时刷新其余控件为该设备的有效值。
        // Bind target/action: on selection change, immediately refresh the other controls with
        // the selected device's effective values.
        let _: () = msg_send![dev_popup, setTarget: target];
        let _: () = msg_send![dev_popup, setAction: sel!(handleDeviceChanged:)];
        ui.device_indicator = add_tall_row(
            mouse_view,
            label_x,
            y,
            label_w,
            &t("settings.header_mouse_device"),
            dev_popup,
        )
        .1;

        // --- 滚动模式 / Scroll mode ---
        y -= 8.0 + described_row_h;
        let scroll_popup = make_popup(ctrl_x, y + 10.0, ctrl_w, row_h, &SCROLL_MODE_LABELS, 0);
        style_flat_popup(scroll_popup);
        ui.scroll_mode = add_tall_row(
            mouse_view,
            label_x,
            y,
            label_w,
            &t("settings.row_scroll_mode"),
            scroll_popup,
        )
        .1;
        // 滚动模式切换时即时刷新"行数"行的条件显隐。
        // Refresh the conditional visibility of the "lines per tick" row on mode switch.
        let _: () = msg_send![ui.scroll_mode, setTarget: target];
        let _: () = msg_send![ui.scroll_mode, setAction: sel!(handleScrollModeChanged:)];
        // The HTML device card contains both rows, with one internal hairline between them.
        add_row_separator(mouse_view, 0.0, y + described_row_h + 3.0, content_w);
        add_settings_card(
            mouse_view,
            NSRect::new(
                NSPoint::new(6.0, y - 10.0),
                NSSize::new(content_w - 12.0, (device_header_y + 2.0) - (y - 10.0)),
            ),
        );

        // --- 行数(按行模式) / Line count (line mode) ---
        y -= 8.0 + described_row_h;
        let (line_label, line_ctrl) = add_tall_row(
            mouse_view,
            label_x,
            y,
            label_w,
            &t("settings.row_line_count"),
            // 整数滑块 1..=10(与 config 校验一致;对齐 LinearMouse By Lines 的滑块交互)。
            // 右侧留 ~40pt 放只读数值 label 显示当前值。
            // Integer slider 1..=10 (matches config validation; mirrors LinearMouse's
            // By Lines slider interaction). ~40pt on the right holds a read-only value label.
            make_slider(ctrl_x, y + 10.0, ctrl_w - 40.0, row_h, 1, 10, 3),
        );
        ui.line_count = line_ctrl;
        ui.line_count_label = line_label;
        // 滑块右侧的只读数值 label:显示当前行数,拖动滑块时实时刷新。
        // Read-only value label right of the slider: shows the current line count, refreshed
        // live as the slider moves.
        let value_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let value_label: *mut AnyObject = msg_send![value_label, initWithFrame: NSRect::new(NSPoint::new(ctrl_x + ctrl_w - 34.0, y + 10.0), NSSize::new(30.0, row_h))];
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
        add_settings_card(
            mouse_view,
            NSRect::new(
                NSPoint::new(6.0, y - 10.0),
                NSSize::new(content_w - 12.0, described_row_h + 20.0),
            ),
        );

        // --- 滚动 Scrolling ---
        y -= 14.0 + 24.0;
        add_header(
            mouse_view,
            &t("settings.header_mouse_scrolling"),
            label_x,
            y,
            content_w - 2.0 * label_x,
        );
        y -= 8.0 + described_row_h;
        // reverse_scroll 开关:标题+副标题描述滚动方向,开关右对齐到 popup 右缘。
        // reverse_scroll switch: title + subtitle describe the scroll inversion; the switch
        // right-aligns to the popups' right edge.
        ui.reverse_scroll = add_described_row(
            mouse_view,
            label_x,
            y,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_reverse_scroll"),
            &t("settings.desc_reverse_scroll"),
            make_switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
        );
        add_settings_card(
            mouse_view,
            NSRect::new(
                NSPoint::new(6.0, y - 10.0),
                NSSize::new(content_w - 12.0, described_row_h + 20.0),
            ),
        );

        // --- 指针 Pointer ---
        y -= 14.0 + 24.0;
        let pointer_header_y = y;
        add_header(
            mouse_view,
            &t("settings.header_mouse_pointer"),
            label_x,
            y,
            content_w - 2.0 * label_x,
        );
        y -= 8.0 + described_row_h;
        // disable_pointer_accel 开关:禁用系统鼠标加速,光标 1:1 线性跟踪。
        // 副标题说明线性跟踪的用途;开关与所有开关行一样右对齐到 popup 右缘。
        // disable_pointer_accel switch: disable system pointer acceleration for 1:1 linear
        // cursor tracking. The subtitle explains linear tracking; the switch right-aligns to
        // the popups' right edge (ctrl_x + ctrl_w), like every other switch row.
        ui.disable_pointer_accel = add_described_row(
            mouse_view,
            label_x,
            y,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_disable_pointer_accel"),
            &t("settings.desc_disable_pointer_accel"),
            make_switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
        );
        add_settings_card(
            mouse_view,
            NSRect::new(
                NSPoint::new(6.0, y - 10.0),
                NSSize::new(content_w - 12.0, (pointer_header_y + 2.0) - (y - 10.0)),
            ),
        );

        // --- 按键映射 Button Mappings ---
        // 绑定区:"Enable button mappings" 描述行 + 嵌套表格卡片(圆角子表格 + 添加按钮)。
        // Button mappings: an "Enable button mappings" described row + a nested table card
        // (rounded sub-table + the add-mapping button).
        y -= 14.0 + 24.0;
        add_header(
            mouse_view,
            &t("settings.header_mouse_mappings"),
            label_x,
            y,
            content_w - 2.0 * label_x,
        );
        // "Enable button mappings" 描述行(HTML 卡片顶部),替代原来放在区块标题右侧的开关。
        // "Enable button mappings" described row (HTML card top), replacing the old switch
        // that sat on the section-header row's right edge.
        y -= 8.0 + described_row_h;
        ui.mapping_enabled = add_described_row(
            mouse_view,
            label_x,
            y,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_mapping_enable"),
            &t("settings.desc_mapping_enable"),
            make_switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
        );
        let _: () = msg_send![ui.mapping_enabled, setTarget: target];
        let _: () = msg_send![ui.mapping_enabled, setAction: sel!(handleMappingEnabledChanged:)];
        add_settings_card(
            mouse_view,
            NSRect::new(
                NSPoint::new(6.0, y - 10.0),
                NSSize::new(content_w - 12.0, described_row_h + 20.0),
            ),
        );

        // --- 嵌套表格卡片(nested table card) ---
        y -= 24.0;
        let card_top = y;
        let card_w = content_w - 24.0;
        let card_h = MAPPING_PANEL_TOP
            + (MAPPING_HEADER_H + MAPPING_ROW_H * 3.0)
            + MAPPING_ACTION_TOP
            + MAPPING_ACTION_H
            + MAPPING_CARD_PAD_BOT;
        let card_bottom = card_top - card_h;
        // 外层卡片:白色卡片(与其它设置卡片一致),只有嵌套表格和添加按钮是灰色/深色。
        // The outer card is a white settings card (same as every other card); only the nested
        // table and the add button carry the gray "dark" treatment from the HTML reference.
        let card_bg: *mut AnyObject = msg_send![class!(NSView), alloc];
        let card_bg: *mut AnyObject = msg_send![card_bg, initWithFrame: NSRect::new(NSPoint::new(label_x, card_bottom), NSSize::new(card_w, card_h))];
        let _: () = msg_send![card_bg, setFlipped: true];
        let _: () = msg_send![card_bg, setAutoresizingMask: 0u64];
        let _: () = msg_send![card_bg, setWantsLayer: true];
        let bg_layer: *mut AnyObject = msg_send![card_bg, layer];
        let _: () = msg_send![bg_layer, setCornerRadius: 14.0f64];
        let _: () = msg_send![bg_layer, setMasksToBounds: true];
        crate::ffi::layer_set_background(bg_layer, crate::ffi::hex_to_cg_color(0xFFFFFFE0u32));
        crate::ffi::layer_set_border(bg_layer, crate::ffi::hex_to_cg_color(0x00000012u32));
        let _: () = msg_send![bg_layer, setBorderWidth: 1.0f64];
        // 嵌套的 `.mapping-table`:圆角描边子面板,铺在行后面,让映射区有 HTML 的表格观感。
        // The nested `.mapping-table`: a rounded, bordered sub-panel behind the rows, giving
        // the bindings the HTML reference's table look.
        let panel: *mut AnyObject = msg_send![class!(NSView), alloc];
        let panel: *mut AnyObject = msg_send![panel, initWithFrame: NSRect::new(NSPoint::new(MAPPING_PANEL_X, MAPPING_PANEL_TOP), NSSize::new(card_w - 2.0 * MAPPING_PANEL_X, MAPPING_HEADER_H + MAPPING_ROW_H * 3.0))];
        let _: () = msg_send![panel, setWantsLayer: true];
        let panel_layer: *mut AnyObject = msg_send![panel, layer];
        let _: () = msg_send![panel_layer, setCornerRadius: 10.0f64];
        let _: () = msg_send![panel_layer, setMasksToBounds: true];
        crate::ffi::layer_set_background(panel_layer, crate::ffi::hex_to_cg_color(0x76768010u32));
        crate::ffi::layer_set_border(panel_layer, crate::ffi::hex_to_cg_color(0x0000000Fu32));
        let _: () = msg_send![panel_layer, setBorderWidth: 1.0f64];
        let _: () = msg_send![card_bg, addSubview: panel];
        ui.mapping_panel = panel;
        release_obj(panel);
        // 表头带(.mapping-table thead)。
        // The header band (.mapping-table thead).
        let header_color: *mut AnyObject = msg_send![class!(NSColor), secondaryLabelColor];
        let header_font: *mut AnyObject = msg_send![class!(NSFont), boldSystemFontOfSize: 12.0f64];
        for (hx, hw, htext) in [
            (
                MAPPING_PANEL_X + MAPPING_CELL_X,
                120.0,
                t("settings.mapping_column_button"),
            ),
            (
                MAPPING_PANEL_X + MAPPING_CELL_X + 80.0,
                130.0,
                t("settings.mapping_column_action"),
            ),
        ] {
            let hlabel: *mut AnyObject = msg_send![class!(NSTextField), alloc];
            let hlabel: *mut AnyObject = msg_send![hlabel, initWithFrame: NSRect::new(NSPoint::new(hx, MAPPING_PANEL_TOP + 7.0), NSSize::new(hw, 18.0))];
            let hns = make_nsstring(&htext);
            let _: () = msg_send![hlabel, setStringValue: hns];
            CFRelease(hns as *const c_void);
            let _: () = msg_send![hlabel, setBezeled: false];
            let _: () = msg_send![hlabel, setDrawsBackground: false];
            let _: () = msg_send![hlabel, setEditable: false];
            let _: () = msg_send![hlabel, setFont: header_font];
            let _: () = msg_send![hlabel, setTextColor: header_color];
            let _: () = msg_send![card_bg, addSubview: hlabel];
            release_obj(hlabel);
        }
        // 表头下方 hairline。
        // Hairline under the header band.
        let header_line: *mut AnyObject = msg_send![class!(NSView), alloc];
        let header_line: *mut AnyObject = msg_send![header_line, initWithFrame: NSRect::new(NSPoint::new(MAPPING_PANEL_X + MAPPING_CELL_X, MAPPING_PANEL_TOP + MAPPING_HEADER_H - 1.0), NSSize::new(card_w - 2.0 * (MAPPING_PANEL_X + MAPPING_CELL_X), 1.0))];
        let _: () = msg_send![header_line, setWantsLayer: true];
        let header_line_layer: *mut AnyObject = msg_send![header_line, layer];
        let header_line_color: *mut AnyObject = msg_send![class!(NSColor), separatorColor];
        layer_set_background(header_line_layer, ns_color_to_cg(header_line_color));
        let _: () = msg_send![card_bg, addSubview: header_line];
        release_obj(header_line);
        // 空状态提示(无行时显示在子表格内)。
        // Empty-state hint (inside the sub-table when there are no rows).
        let empty: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let empty: *mut AnyObject = msg_send![empty, initWithFrame: NSRect::new(NSPoint::new(MAPPING_PANEL_X + MAPPING_CELL_X, MAPPING_PANEL_TOP + MAPPING_HEADER_H + (MAPPING_ROW_H * 3.0) / 2.0 - 9.0), NSSize::new(card_w - 2.0 * (MAPPING_PANEL_X + MAPPING_CELL_X), 18.0))];
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
        // 添加按钮:卡片底部 action-row(全宽)。
        // Add-mapping button: full-width action row at the card bottom.
        let add_btn: *mut AnyObject = msg_send![class!(NSButton), alloc];
        let add_btn: *mut AnyObject = msg_send![add_btn, initWithFrame: NSRect::new(NSPoint::new(MAPPING_PANEL_X, MAPPING_PANEL_TOP + MAPPING_HEADER_H + MAPPING_ROW_H * 3.0 + MAPPING_ACTION_TOP), NSSize::new(card_w - 2.0 * MAPPING_PANEL_X, MAPPING_ACTION_H))];
        style_html_button(add_btn, 0x7676801Fu32, 0x2C2C30FFu32);
        let add_title = make_nsstring(&t("settings.row_add_mapping"));
        let _: () = msg_send![add_btn, setTitle: add_title];
        CFRelease(add_title as *const c_void);
        let _: () = msg_send![add_btn, setTarget: target];
        let _: () = msg_send![add_btn, setAction: sel!(handleAddMapping:)];
        let _: () = msg_send![card_bg, addSubview: add_btn];
        release_obj(add_btn);
        ui.add_mapping_button = add_btn;
        // 外层卡片 add 到页面。
        let _: () = msg_send![mouse_view, addSubview: card_bg];
        release_obj(card_bg);
        ui.mapping_card = card_bg;
        ui.mapping_scroll = std::ptr::null_mut();
        ui.mapping_doc = card_bg;
        // 初始渲染当前设备的映射。
        // Render the current device's mappings initially.
        render_mapping_rows();

        // ===== 剪贴板历史页内容 clipboard page content =====
        // 独立布局游标(该页内容与鼠标页互不相关)。
        // Independent layout cursor (this page's content is unrelated to the mouse page).
        let mut cy = clipboard_doc_h - 24.0;
        add_page_title(
            clipboard_view,
            &t("settings.sidebar_clipboard"),
            label_x,
            cy - 34.0,
            content_w - label_x * 2.0,
        );
        cy -= 62.0;
        let clipboard_header_y = cy - 18.0;
        add_header(
            clipboard_view,
            &t("settings.header_clipboard"),
            label_x,
            cy - 18.0,
            content_w - 2.0 * label_x,
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
            220.0,
            row_h,
            &t("settings.row_clipboard_enabled"),
            make_switch(ctrl_x + ctrl_w, cy, row_h, false),
        );
        cy -= 8.0 + row_h;
        add_row_separator(clipboard_view, 0.0, cy + row_h + 3.0, content_w);
        // 置顶后选中项位置下拉框:项 = [跟随置顶, 保持当前位置];默认 index 0(跟随置顶),
        // 实际值由 load_settings_from 填充。
        // Pin-selection popup: items = [Follow the Pinned Entry, Keep Current Position];
        // default index 0 (follow); the real value is set by load_settings_from.
        let pin_labels = [
            t("settings.pin_follow_entry"),
            t("settings.pin_keep_position"),
        ];
        let pin_label_refs: Vec<&str> = pin_labels.iter().map(|s| s.as_str()).collect();
        ui.clipboard_pin_follow = add_row(
            clipboard_view,
            label_x,
            cy,
            220.0,
            row_h,
            &t("settings.row_clipboard_pin_follow"),
            make_popup(ctrl_x, cy, ctrl_w, row_h, &pin_label_refs, 0),
        );
        cy -= 8.0 + row_h;
        add_row_separator(clipboard_view, 0.0, cy + row_h + 3.0, content_w);
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
            220.0,
            row_h,
            &t("settings.row_clipboard_persist"),
            make_switch(ctrl_x + ctrl_w, cy, row_h, false),
        );
        cy -= 8.0 + row_h;
        add_row_separator(clipboard_view, 0.0, cy + row_h + 3.0, content_w);
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
        add_row_separator(clipboard_view, 0.0, cy + row_h + 3.0, content_w);
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
            220.0,
            row_h,
            &t("settings.row_clipboard_move_used_to_top"),
            make_switch(ctrl_x + ctrl_w, cy, row_h, false),
        );
        cy -= 8.0 + row_h;
        add_row_separator(clipboard_view, 0.0, cy + row_h + 3.0, content_w);
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
        add_row_separator(clipboard_view, 0.0, cy + row_h + 3.0, content_w);
        // 自动过期天数(数字输入,0 = 关闭)/ auto-expire days (number input, 0 = off).
        ui.clipboard_auto_expire_days = add_row(
            clipboard_view,
            label_x,
            cy,
            label_w,
            row_h,
            &t("settings.row_clipboard_auto_expire_days"),
            make_text_input(ctrl_x, cy, ctrl_w, row_h, "3"),
        );
        cy -= 8.0 + row_h;
        add_row_separator(clipboard_view, 0.0, cy + row_h + 3.0, content_w);
        // 呼出快捷键说明(只读 label)/ shortcut hint (read-only label).
        let hint: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let hint: *mut AnyObject = msg_send![hint, initWithFrame: NSRect::new(NSPoint::new(label_x, cy), NSSize::new(content_w - 24.0, row_h))];
        set_field(hint, t("settings.row_clipboard_shortcut"));
        let _: () = msg_send![hint, setBezeled: false];
        let _: () = msg_send![hint, setDrawsBackground: false];
        let _: () = msg_send![hint, setEditable: false];
        let _: () = msg_send![clipboard_view, addSubview: hint];
        release_obj(hint);
        add_settings_card(
            clipboard_view,
            NSRect::new(
                NSPoint::new(6.0, cy - 10.0),
                NSSize::new(content_w - 12.0, (clipboard_header_y + 2.0) - (cy - 10.0)),
            ),
        );

        // ===== About page: page-header + App and Updates cards from preview (10). =====
        let header_top = about_doc_h - 68.0;
        add_about_app_icon(about_view, label_x, header_top - 58.0);

        let about_title: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let about_title: *mut AnyObject = msg_send![
            about_title,
            initWithFrame: NSRect::new(
                NSPoint::new(label_x + 73.0, header_top - 33.0),
                NSSize::new(content_w - 73.0 - label_x, 28.0),
            )
        ];
        set_field(about_title, "Oh My Tab");
        let _: () = msg_send![about_title, setBezeled: false];
        let _: () = msg_send![about_title, setDrawsBackground: false];
        let _: () = msg_send![about_title, setEditable: false];
        let about_title_font: *mut AnyObject =
            msg_send![class!(NSFont), boldSystemFontOfSize: 24.0f64];
        let _: () = msg_send![about_title, setFont: about_title_font];
        let _: () = msg_send![about_view, addSubview: about_title];
        release_obj(about_title);

        let about_subtitle: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let about_subtitle: *mut AnyObject = msg_send![
            about_subtitle,
            initWithFrame: NSRect::new(
                NSPoint::new(label_x + 73.0, header_top - 53.0),
                NSSize::new(content_w - 73.0 - label_x, 18.0),
            )
        ];
        set_field(
            about_subtitle,
            tf(
                "settings.version_label",
                &[("version", env!("CARGO_PKG_VERSION"))],
            ),
        );
        let _: () = msg_send![about_subtitle, setBezeled: false];
        let _: () = msg_send![about_subtitle, setDrawsBackground: false];
        let _: () = msg_send![about_subtitle, setEditable: false];
        let about_subtitle_font: *mut AnyObject =
            msg_send![class!(NSFont), systemFontOfSize: 13.0f64];
        let _: () = msg_send![about_subtitle, setFont: about_subtitle_font];
        let about_subtitle_color: *mut AnyObject = msg_send![class!(NSColor), secondaryLabelColor];
        let _: () = msg_send![about_subtitle, setTextColor: about_subtitle_color];
        let _: () = msg_send![about_view, addSubview: about_subtitle];
        release_obj(about_subtitle);

        let mut ay = header_top - 88.0;
        let app_label_y = ay - 11.0;
        add_header(
            about_view,
            &t("settings.section_app"),
            label_x + 3.0,
            app_label_y,
            content_w - label_x * 2.0,
        );
        ay -= 27.0;
        let website_y = ay - 44.0;
        let website_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let website_label: *mut AnyObject = msg_send![
            website_label,
            initWithFrame: NSRect::new(
                NSPoint::new(label_x + 15.0, website_y),
                NSSize::new(130.0, 28.0),
            )
        ];
        set_field(website_label, t("settings.website_label"));
        let _: () = msg_send![website_label, setBezeled: false];
        let _: () = msg_send![website_label, setDrawsBackground: false];
        let _: () = msg_send![website_label, setEditable: false];
        let website_label_font: *mut AnyObject =
            msg_send![class!(NSFont), systemFontOfSize: 13.5f64];
        let _: () = msg_send![website_label, setFont: website_label_font];
        let _: () = msg_send![about_view, addSubview: website_label];
        release_obj(website_label);
        let website_url_frame = NSRect::new(
            NSPoint::new(label_x + 145.0, website_y),
            NSSize::new((content_w - 2.0 * label_x - 145.0).max(1.0), 28.0),
        );
        let website_url: *mut AnyObject = msg_send![website_link_button_class(), alloc];
        let website_url: *mut AnyObject = msg_send![
            website_url,
            initWithFrame: website_url_frame
        ];
        set_field(website_url, t("settings.website_url"));
        let _: () = msg_send![website_url, setBezeled: false];
        let _: () = msg_send![website_url, setDrawsBackground: false];
        let _: () = msg_send![website_url, setEditable: false];
        let _: () = msg_send![website_url, setSelectable: false];
        let _: () = msg_send![website_url, setAlignment: 0isize];
        let website_url_font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 13.5f64];
        let _: () = msg_send![website_url, setFont: website_url_font];
        let website_url_color: *mut AnyObject = msg_send![class!(NSColor), linkColor];
        let _: () = msg_send![website_url, setTextColor: website_url_color];
        let website_tracking: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
        let website_tracking: *mut AnyObject = msg_send![
            website_tracking,
            initWithRect: NSRect::new(NSPoint::new(0.0, 0.0), website_url_frame.size),
            options: 0x01u64 | 0x80u64 | 0x200u64,
            owner: website_url,
            userInfo: std::ptr::null::<AnyObject>()
        ];
        let _: () = msg_send![website_url, addTrackingArea: website_tracking];
        release_obj(website_tracking);
        let _: () = msg_send![about_view, addSubview: website_url];
        release_obj(website_url);
        let version_y = website_y - 44.0;
        let version_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let version_label: *mut AnyObject = msg_send![
            version_label,
            initWithFrame: NSRect::new(
                NSPoint::new(label_x + 15.0, version_y),
                NSSize::new(130.0, 28.0),
            )
        ];
        set_field(version_label, t("settings.version_label_short"));
        let _: () = msg_send![version_label, setBezeled: false];
        let _: () = msg_send![version_label, setDrawsBackground: false];
        let _: () = msg_send![version_label, setEditable: false];
        let version_label_font: *mut AnyObject =
            msg_send![class!(NSFont), systemFontOfSize: 13.5f64];
        let _: () = msg_send![version_label, setFont: version_label_font];
        let _: () = msg_send![about_view, addSubview: version_label];
        release_obj(version_label);
        let version_value: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let version_value: *mut AnyObject = msg_send![
            version_value,
            initWithFrame: NSRect::new(
                NSPoint::new(label_x + 145.0, version_y),
                NSSize::new(120.0, 28.0),
            )
        ];
        set_field(version_value, env!("CARGO_PKG_VERSION"));
        let _: () = msg_send![version_value, setBezeled: false];
        let _: () = msg_send![version_value, setDrawsBackground: false];
        let _: () = msg_send![version_value, setEditable: false];
        let version_value_font: *mut AnyObject =
            msg_send![class!(NSFont), systemFontOfSize: 13.5f64];
        let _: () = msg_send![version_value, setFont: version_value_font];
        let version_value_color: *mut AnyObject = msg_send![class!(NSColor), secondaryLabelColor];
        let _: () = msg_send![version_value, setTextColor: version_value_color];
        let _: () = msg_send![about_view, addSubview: version_value];
        release_obj(version_value);
        add_settings_card(
            about_view,
            NSRect::new(
                NSPoint::new(label_x, version_y - 15.0),
                NSSize::new(content_w - 2.0 * label_x, 98.0),
            ),
        );

        ay = version_y - 42.0;
        add_header(
            about_view,
            &t("settings.section_updates"),
            label_x + 3.0,
            ay - 11.0,
            content_w - label_x * 2.0,
        );
        ay -= 27.0;
        let update_row_y = ay - 44.0;
        ui.update_auto_check = add_described_row(
            about_view,
            label_x + 15.0,
            update_row_y,
            (ctrl_x + ctrl_w) - (label_x + 15.0) - 70.0,
            described_row_h,
            &t("settings.row_update_auto_check"),
            &t("settings.desc_update_auto_check"),
            make_switch(ctrl_x + ctrl_w, update_row_y + 10.0, row_h, false),
        );
        let check_button = make_settings_action_button(
            NSRect::new(
                NSPoint::new(label_x + 15.0, update_row_y - 46.0),
                NSSize::new(content_w - 2.0 * label_x - 30.0, 32.0),
            ),
            &t("settings.btn_check_for_updates"),
            target,
            sel!(handleCheckForUpdates:),
        );
        let _: () = msg_send![check_button, setTag: -3isize];
        let check_layer: *mut AnyObject = msg_send![check_button, layer];
        if !check_layer.is_null() {
            layer_set_background(check_layer, crate::ffi::hex_to_cg_color(0x7676801Eu32));
        }
        let _: () = msg_send![about_view, addSubview: check_button];
        release_obj(check_button);
        let update_hint: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let update_hint: *mut AnyObject = msg_send![
            update_hint,
            initWithFrame: NSRect::new(
                NSPoint::new(label_x + 15.0, update_row_y - 80.0),
                NSSize::new(content_w - 2.0 * label_x - 30.0, 30.0),
            )
        ];
        set_field(update_hint, t("settings.update_placeholder_hint"));
        let _: () = msg_send![update_hint, setBezeled: false];
        let _: () = msg_send![update_hint, setDrawsBackground: false];
        let _: () = msg_send![update_hint, setEditable: false];
        let update_hint_font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 11.5f64];
        let _: () = msg_send![update_hint, setFont: update_hint_font];
        let update_hint_color: *mut AnyObject = msg_send![class!(NSColor), secondaryLabelColor];
        let _: () = msg_send![update_hint, setTextColor: update_hint_color];
        let _: () = msg_send![about_view, addSubview: update_hint];
        release_obj(update_hint);
        add_settings_card(
            about_view,
            NSRect::new(
                NSPoint::new(label_x, update_row_y - 91.0),
                NSSize::new(content_w - 2.0 * label_x, 155.0),
            ),
        );

        // banner 最后添加:作为 general_view 的最后一个 subview,保证在内容之上(缺权限时覆盖顶部)。
        // Added last: as general_view's final subview so it floats above the content (when
        // permission is missing). It occupies no layout space, so no top gap when hidden.
        let _: () = msg_send![general_view, addSubview: banner];
        release_obj(banner);

        // --- 确认 / 取消(右侧 footer 内,所有页面都可见)---
        // Cancel and OK are children of the detail pane's footer, matching the HTML layout.
        let cancel = make_settings_action_button(
            NSRect::new(
                NSPoint::new(content_x + detail_w - 202.0, 14.0),
                NSSize::new(86.0, 32.0),
            ),
            &t("settings.btn_cancel"),
            target,
            sel!(handleSettingsCancel:),
        );
        let _: () = msg_send![cancel, setTag: -1isize];
        let cancel_layer: *mut AnyObject = msg_send![cancel, layer];
        if !cancel_layer.is_null() {
            // HTML footer buttons use a slightly more opaque white surface than small buttons.
            layer_set_background(cancel_layer, crate::ffi::hex_to_cg_color(0xFFFFFFC7u32));
        }
        let _: () = msg_send![cancel, setAutoresizingMask: 33u64]; // 贴底、贴右 / bottom- and right-anchored
        let _: () = msg_send![content, addSubview: cancel];
        release_obj(cancel);

        let ok = make_settings_action_button(
            NSRect::new(
                NSPoint::new(content_x + detail_w - 106.0, 14.0),
                NSSize::new(86.0, 32.0),
            ),
            &t("settings.btn_ok"),
            target,
            sel!(handleSettingsOk:),
        );
        let _: () = msg_send![ok, setTag: -2isize];
        let ok_layer: *mut AnyObject = msg_send![ok, layer];
        if !ok_layer.is_null() {
            layer_set_background(ok_layer, crate::ffi::hex_to_cg_color(0x0A84FFFFu32));
        }
        let white: *mut AnyObject = msg_send![class!(NSColor), whiteColor];
        let _: () = msg_send![ok, setContentTintColor: white];
        let _: () = msg_send![ok, setAutoresizingMask: 33u64]; // 贴底、贴右
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
    let ui = SETTINGS_UI.lock().unwrap().take();
    if let Some(u) = ui {
        unsafe {
            close_glass_tint_panel(u.glass_tint);
            TRAFFIC_LIGHT_BASE_ORIGINS
                .lock()
                .unwrap()
                .remove(&(u.window as usize));
            // 窗口 alloc 是 +1且 setReleasedWhenClosed:false,需手动 release 一次;
            // 其子控件已由父视图持有,随窗口 dealloc 释放。
            // The window is alloc +1 with setReleasedWhenClosed:false, so release once manually;
            // its subviews are retained by the parent view and dealloc with the window.
            let _: () = msg_send![u.window, orderOut: std::ptr::null::<AnyObject>()];
            release_obj(u.window);
        }
        // 窗口被作废(销毁),切回 .accessory(可能 locale 变更时设置正开着)。
        // The window is invalidated/destroyed; flip back to .accessory (it may have been open
        // during a locale change).
        crate::set_settings_activation_policy(false);
    }
    // Sidebar labels/icons are owned by the invalidated window. Drop their raw-pointer entries
    // so a rebuilt window can never address a deallocated child view.
    SIDEBAR_TITLE_LABELS.lock().unwrap().clear();
    SIDEBAR_ICON_VIEWS.lock().unwrap().clear();
    clear_glass_preview();
}

#[cfg(test)]
mod tests {
    use super::{
        color_component_to_byte, glass_tint_group_frames, rgba_hex_from_components,
        GLASS_TINT_GROUP_GAP, GLASS_TINT_SCREEN_MARGIN,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize};

    #[test]
    fn color_components_round_and_clamp_to_rgba_hex() {
        assert_eq!(rgba_hex_from_components(0.0, 0.5, 1.0, 0.25), "0080ff40");
        assert_eq!(rgba_hex_from_components(-1.0, 2.0, 0.1, 0.9), "00ff1ae6");
        assert_eq!(color_component_to_byte(0.501), 128);
    }

    #[test]
    fn glass_tint_group_centers_settings_and_panel_together() {
        let screen = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1920.0, 1080.0));
        let settings = NSRect::new(NSPoint::new(100.0, 200.0), NSSize::new(656.0, 690.0));
        let panel = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(250.0, 397.0));
        let (settings_frame, panel_frame) = glass_tint_group_frames(settings, panel, screen);
        let group_w = settings.size.width + GLASS_TINT_GROUP_GAP + panel.size.width;
        assert_eq!(settings_frame.origin.x, (screen.size.width - group_w) / 2.0);
        assert_eq!(
            panel_frame.origin.x,
            settings_frame.origin.x + settings.size.width + GLASS_TINT_GROUP_GAP
        );
        assert_eq!(
            panel_frame.origin.y,
            settings.origin.y + (settings.size.height - panel.size.height) / 2.0
        );
    }

    #[test]
    fn glass_tint_group_uses_screen_origin_and_clamps_panel_vertically() {
        let screen = NSRect::new(NSPoint::new(-1280.0, 80.0), NSSize::new(800.0, 700.0));
        let settings = NSRect::new(NSPoint::new(-900.0, -200.0), NSSize::new(656.0, 690.0));
        let panel = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(250.0, 397.0));
        let (settings_frame, panel_frame) = glass_tint_group_frames(settings, panel, screen);
        assert_eq!(
            settings_frame.origin.x,
            screen.origin.x + GLASS_TINT_SCREEN_MARGIN
        );
        assert_eq!(
            panel_frame.origin.y,
            screen.origin.y + GLASS_TINT_SCREEN_MARGIN
        );
        assert!(panel_frame.origin.y + panel.size.height <= screen.origin.y + screen.size.height);
    }
}
