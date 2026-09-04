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
use std::time::{Duration, Instant};

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
use crate::theme::{resolved_is_dark, ui_palette, UiPalette};
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
const TEXT_SIZE_MIN: i64 = 13;
const TEXT_SIZE_MAX: i64 = 20;
const TEXT_SIZE_DEFAULT: i64 = 15;
const TEXT_SIZE_VALUE_W: f64 = 40.0;
const TEXT_SIZE_VALUE_H: f64 = 18.0;

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
    sidebar_window_control: *mut AnyObject, // NSButton: 窗口控制 / Window control (tag=4)
    sidebar_about: *mut AnyObject,   // NSButton: 关于 / About (tag=5)
    sidebar_highlight: *mut AnyObject, // NSView: 选中行高亮背景 (layer-backed)
    general_view: *mut AnyObject,    // NSView: 通用页容器 / General page container
    switcher_view: *mut AnyObject,   // NSView: 应用切换浮窗页容器 / App switcher page container
    mouse_view: *mut AnyObject,      // NSView: 鼠标页容器 / Mouse page container
    clipboard_view: *mut AnyObject,  // NSView: 剪贴板历史页容器 / Clipboard page container
    window_control_view: *mut AnyObject, // NSView: 窗口控制页容器 / Window-control page container
    about_view: *mut AnyObject,      // NSView: 关于页容器 / About page container
    about_subtitle: *mut AnyObject,  // NSTextField: About 页版本号 / About-page version label
    theme: *mut AnyObject,           // NSPopUpButton: auto / light / dark
    glass_style: *mut AnyObject,     // NSPopUpButton: regular / clear
    glass_tint: *mut AnyObject,      // NSColorWell: 玻璃颜色 / glass tint
    glass_preview_switcher: *mut AnyObject, // NSGlassEffectView: app switcher preview
    glass_preview_clipboard: *mut AnyObject, // NSGlassEffectView: clipboard preview
    corner_radius: *mut AnyObject,   // NSTextField
    modifier: *mut AnyObject,        // NSPopUpButton: option / command
    locale: *mut AnyObject,          // NSPopUpButton: auto / en / zh-Hans / zh-Hant
    show_minimized: *mut AnyObject,  // NSSwitch: 显示最小化窗口 / show minimized windows
    thumbnails_enabled: *mut AnyObject, // NSPopUpButton: 窗口显示模式 / window display mode
    card_text_size: *mut AnyObject,  // NSSlider: 卡片文字大小 / card text size
    card_text_size_value_label: *mut AnyObject, // NSTextField: 卡片字号值 / card text-size value
    status_bar_text_size: *mut AnyObject, // NSSlider: 底部标题栏文字大小 / footer text size
    status_bar_text_size_value_label: *mut AnyObject, // NSTextField: 底部字号值 / footer text-size value
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
    line_count_card: *mut AnyObject,        // NSView: 行数卡片 / line-count card
    line_count_shadow: *mut AnyObject,      // NSView: 行数卡片阴影 / line-count card shadow
    line_count_separator: *mut AnyObject, // NSView: 行数行上方分割线 / separator above line-count row
    line_count_compact: bool, // 是否已移除条件行占位 / whether the conditional row is compacted
    disable_pointer_accel: *mut AnyObject, // NSSwitch: 禁用指针加速 / disable pointer acceleration
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
    window_control_enabled: *mut AnyObject, // NSSwitch: 启用窗口控制 / enable window control
    window_control_up: *mut AnyObject,      // NSSwitch: 启用 Option+上 / enable Option+Up
    window_control_down: *mut AnyObject,    // NSSwitch: 启用 Option+下 / enable Option+Down
    window_control_left: *mut AnyObject,    // NSSwitch: 启用 Option+左 / enable Option+Left
    window_control_right: *mut AnyObject,   // NSSwitch: 启用 Option+右 / enable Option+Right
    add_mapping_button: *mut AnyObject,     // NSButton: 添加映射 / add-mapping button
    mapping_enabled: *mut AnyObject, // NSSwitch: 按键映射总开关(per-device) / mappings master switch (per-device)
    mapping_empty: *mut AnyObject,   // NSTextField: 空状态提示(卡片内) / empty-state hint (in-card)
    device_indicator: *mut AnyObject, // NSButton: 当前选中设备指示器(点击打开选择器) / device indicator (opens picker)
    ok_button: *mut AnyObject,        // NSButton: 确认按钮 / OK button
    accessibility_warning_view: *mut AnyObject, // NSView: 缺权限警告条容器 / permission-warning banner container
    update_auto_check: *mut AnyObject, // NSSwitch: Sparkle 自动检查开关 / Sparkle auto-check switch
    update_auto_download: *mut AnyObject, // NSSwitch: Sparkle 自动下载开关 / Sparkle auto-download switch
    update_check_button: *mut AnyObject, // NSButton: 检查更新按钮(状态随流程变化) / check-updates button
    update_host: *mut AnyObject, // NSView: About 页内更新流程宿主容器 / In-about update flow host container
    update_host_window: *mut AnyObject, // NSWindow: 宿主所属设置窗口(供更新聚焦拉起) / host's settings window
    update_card: *mut AnyObject, // NSView: Updates 卡片(展开时撑高) / Updates card (grows when expanded)
    update_card_shadow: *mut AnyObject, // NSView: Updates 卡片阴影 / Updates card shadow
    update_card_compact_h: f64,  // 收起时卡片高度 / collapsed card height
    update_card_expanded: bool,  // 是否已为更新流程展开 / whether expanded for a flow
    update_host_origin_y: f64,   // 宿主收起时的原点 y(顶边 - 展开高) / host origin y when collapsed
}
unsafe impl Send for SettingsUi {}

/// 一行按键映射(只读显示):
/// - label:按钮名(只读)
/// - desc_label:动作描述(系统动作名/None 文本;Key Press 时用键帽胶囊)
/// - action_icon:非键盘动作对应的 SF Symbol
/// - edit:编辑按钮(tag = 按钮号,点击打开编辑面板)
/// - delete:删除按钮(tag = 按钮号)
/// - caps:键帽胶囊(Key Press 时显示)
///
/// One button-mapping row (read-only display):
/// - label: the button name
/// - desc_label: the action description (system-action name / None text; keycaps for Key
///   Press)
/// - action_icon: the SF Symbol for non-keyboard actions
/// - edit: the edit button (tag = button number; opens the edit panel)
/// - delete: the delete button (tag = button number)
/// - caps: keycap pills (shown for Key Press)
struct MappingRow {
    label: *mut AnyObject,
    desc_label: *mut AnyObject,
    action_icon: *mut AnyObject,
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

/// Gap from a section header to the top edge of its grouped card.
/// 分组标题到下方卡片顶边的统一间距。
const SETTINGS_SECTION_CARD_GAP: f64 = 4.0;

/// Gap between the previous card's bottom edge and the next section header.
/// 上一张卡片底边到下一组标题之间的统一间距。
const SETTINGS_SECTION_HEADER_GAP: f64 = 24.0;

/// Optical trailing inset for row controls, matching the text's visible leading inset.
/// 设置行控件的视觉右侧内边距,与左侧文字的可见起始位置保持一致。
const SETTINGS_CONTROL_TRAILING_INSET: f64 = 17.0;

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

/// 动作下拉与映射列表共用的 SF Symbols，索引与 `MAPPING_ACTION_KEYS` 一一对应。
/// SF Symbols shared by the action popup and mapping rows; indices match `MAPPING_ACTION_KEYS`.
const MAPPING_ACTION_SYMBOLS: [&str; 8] = [
    "dot.circle",
    "slash.circle",
    "keyboard",
    "square.grid.2x2",
    "square.grid.3x3",
    "macwindow",
    "rectangle.on.rectangle",
    "arrow.left.arrow.right",
];
unsafe impl Sync for SettingsUi {}
static SETTINGS_UI: Mutex<Option<SettingsUi>> = Mutex::new(None);
/// A visible settings window cannot be rebuilt during an appearance notification without
/// discarding unsaved edits; rebuild it immediately after the user closes it instead.
/// 设置窗口可见时不能在外观通知中直接重建(否则会丢未保存编辑),关闭后再立即重建。
static SYSTEM_APPEARANCE_REBUILD_PENDING: AtomicBool = AtomicBool::new(false);

/// About 页头部彩蛋的点击状态;连续点击需在短时间窗口内完成。
/// Hidden About-header easter-egg click state; consecutive clicks must happen within a short window.
static ABOUT_HEADER_CLICKS: Mutex<(u8, Option<Instant>)> = Mutex::new((0, None));
const ABOUT_HEADER_CLICK_WINDOW: Duration = Duration::from_secs(1);

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

pub(crate) mod components;
pub(crate) mod glass_preview;
pub(crate) mod mapping;
pub(crate) mod tooltip;
pub(crate) mod widgets;

use components::{
    SettingsButton, SettingsButtonRole, SettingsCard, SettingsControl, SettingsLayout,
    SettingsMappingActionIcon, SettingsPage, SettingsRow, SettingsSection, SettingsSidebar,
};
use glass_preview::*;
pub(crate) use glass_preview::{
    apply_glass_preview, on_glass_style_changed, on_glass_tint_changed,
    on_glass_tint_panel_changed, on_glass_tint_panel_will_close, on_glass_tint_reset,
};
use mapping::*;
pub(crate) use mapping::{
    cancel_recording_from_main, handle_add_mapping, handle_delete_mapping, handle_mapping_cancel,
    handle_mapping_confirm, handle_mapping_edit, handle_mapping_enabled_changed,
    handle_panel_action_changed, handle_panel_record_combo, handle_panel_record_trigger,
    handle_recording_cancelled, handle_recording_finished,
};
use widgets::*;

// ========== 控件构造 helper / control-builder helpers ==========

fn parse_f64(s: &str) -> Result<f64, ()> {
    s.trim().parse::<f64>().map_err(|_| ())
}
fn parse_usize(s: &str) -> Result<usize, ()> {
    s.trim().parse::<usize>().map_err(|_| ())
}

fn text_size_slider_value(value: f64) -> i64 {
    if value.is_finite() {
        (value.round() as i64).clamp(TEXT_SIZE_MIN, TEXT_SIZE_MAX)
    } else {
        TEXT_SIZE_DEFAULT
    }
}

unsafe fn make_text_size_value_label(
    parent: *mut AnyObject,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    value: i64,
) -> *mut AnyObject {
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let label: *mut AnyObject = msg_send![
        label,
        initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    ];
    set_field(label, value);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let _: () = msg_send![label, setUsesSingleLineMode: true];
    let _: () = msg_send![label, setAlignment: 1isize]; // NSTextAlignmentCenter
    let _: () = msg_send![parent, addSubview: label];
    release_obj(label);
    label
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
    start_inline_update_check();
}

/// 内联检查的公共入口:About 页按钮与更新通知点击共用。
/// Shared entry of the inline check: used by both the About-page button and the update
/// notification click.
fn start_inline_update_check() {
    // 点击即进入内联「检查中」:按钮切到该文案并禁用,并启动超时守卫(若 Sparkle 不回调也能恢复)。
    // Enter the inline checking phase: switch the button and disable it, arming a timeout guard so
    // the button recovers even if Sparkle never calls back.
    crate::updater::begin_inline_check();
    if !crate::updater::check_for_updates() {
        // Sparkle 不可用时显示「重试检查」并提示,避免按钮卡在「正在检查更新…」。
        // When Sparkle is unavailable, offer a retry and alert instead of leaving the button
        // stuck on "Checking…".
        crate::updater::set_check_button_status(&t("settings.btn_retry_update_check"), true);
        show_alert(
            &t("settings.update_unavailable_title"),
            &t("settings.update_unavailable_message"),
        );
    }
}

/// 更新通知点击落点:打开设置窗口、切到 About 页并发起内联检查。
/// 设置窗口存在时 host_view 已注册,后续"发现更新"界面会内联渲染进 About 页
/// (见 updater::render_target),而非独立弹窗。
/// Landing point of the update-notification click: open the settings window, jump to the
/// About page, and start an inline check. Once the settings window exists, its host view
/// is registered, so the "update found" UI renders INLINE in the About page (see
/// updater::render_target) instead of a standalone window.
pub(crate) fn open_about_updates() {
    show_settings();
    // show_settings 每次打开都复位到通用页,这里再切到 About(tag=5)。
    // show_settings resets to the General page on every open; switch to About (tag=5) here.
    select_sidebar(5);
    unsafe {
        let ui = SETTINGS_UI.lock().unwrap();
        if let Some(u) = ui.as_ref() {
            scroll_page_to_top(u.about_view);
        }
    }
    start_inline_update_check();
}

/// 更新流程开始时展开 About 页 Updates 卡片与文档,容纳内联的更新状态/进度/按钮。
/// Expand the About page Updates card and document at the start of a flow so inline update
/// status/progress/buttons fit within the following space.
pub(crate) fn expand_update_section(window_h: f64) {
    let mut ui_guard = SETTINGS_UI.lock().unwrap();
    let Some(ui) = ui_guard.as_mut() else {
        return;
    };
    if ui.update_card.is_null() || ui.update_host.is_null() {
        return;
    }
    unsafe {
        // 卡片撑到当前屏幕所需高度(宿主动态高度),保持宿主顶边固定在提示行下方,
        // 内容从顶部向下排布,卡片与宿主同高,避免按钮下方大块空白。
        // Grow the card to the current screen's required height (the host's dynamic height) while
        // keeping the host top fixed below the hint; the card matches the host so there's no large
        // blank below the buttons.
        let host_frame: NSRect = msg_send![ui.update_host, frame];
        let host_top = host_frame.origin.y + host_frame.size.height;
        let height_delta = window_h - host_frame.size.height;
        let _: () = msg_send![ui.update_host, setFrame: NSRect::new(NSPoint::new(host_frame.origin.x, host_top - window_h), NSSize::new(host_frame.size.width, window_h))];
        let _: () = msg_send![ui.update_host, setHidden: false];
        // 每个 Sparkle 阶段可能需要不同高度;已展开时按差值调整,避免后续控件继续使用旧高度翻转坐标。
        // Each Sparkle phase may need a different height; resize by the delta so later controls are
        // flipped against the current host height instead of the previous phase's height.
        let card_frame: NSRect = msg_send![ui.update_card, frame];
        let new_card = NSRect::new(
            NSPoint::new(card_frame.origin.x, card_frame.origin.y - height_delta),
            NSSize::new(card_frame.size.width, card_frame.size.height + height_delta),
        );
        let _: () = msg_send![ui.update_card, setFrame: new_card];
        let shadow_inset = SETTINGS_CARD_SHADOW_INSET;
        let _: () = msg_send![
            ui.update_card_shadow,
            setFrame: NSRect::new(
                NSPoint::new(new_card.origin.x - shadow_inset, new_card.origin.y - shadow_inset),
                NSSize::new(new_card.size.width + shadow_inset * 2.0, new_card.size.height + shadow_inset * 2.0),
            )
        ];
        // 更新内容直接替换检查按钮区域,不再把整块内容追加到按钮下方。
        // The update content replaces the check-button area instead of being appended below it.
        let _: () = msg_send![ui.update_check_button, setHidden: true];
        ui.update_card_expanded = true;

        // The compact document was fitted during construction; an expanded Sparkle host may
        // extend beyond that height, so re-measure the About page after changing the card.
        // 紧凑文档在构建时已拟合；Sparkle 宿主展开后可能超出原高度，因此卡片变化后重新测量 About 页。
        let clip: *mut AnyObject = msg_send![ui.about_view, contentView];
        let clip_bounds: NSRect = msg_send![clip, bounds];
        let document: *mut AnyObject = msg_send![ui.about_view, documentView];
        fit_settings_document_height(document, clip_bounds.size.height, 24.0, 32.0);
        let _: () = msg_send![ui.window, layoutIfNeeded];
        debug_validate_settings_page(ui.about_view, "about-expanded");
    }
}

/// 更新流程结束时收起 About 页 Updates 卡片与文档,恢复默认紧凑布局。
/// Collapse the About page Updates card and document when a flow ends, restoring the compact look.
pub(crate) fn collapse_update_section() {
    let mut ui_guard = SETTINGS_UI.lock().unwrap();
    let Some(ui) = ui_guard.as_mut() else {
        return;
    };
    if ui.update_card.is_null() || !ui.update_card_expanded {
        return;
    }
    unsafe {
        let compact_h = ui.update_card_compact_h;
        // 卡片与阴影恢复紧凑高度。
        // Restore the card and shadow to their compact height.
        let card_frame: NSRect = msg_send![ui.update_card, frame];
        let new_card = NSRect::new(
            NSPoint::new(
                card_frame.origin.x,
                card_frame.origin.y + (card_frame.size.height - compact_h),
            ),
            NSSize::new(card_frame.size.width, compact_h),
        );
        let _: () = msg_send![ui.update_card, setFrame: new_card];
        let shadow_inset = SETTINGS_CARD_SHADOW_INSET;
        let _: () = msg_send![
            ui.update_card_shadow,
            setFrame: NSRect::new(
                NSPoint::new(new_card.origin.x - shadow_inset, new_card.origin.y - shadow_inset),
                NSSize::new(new_card.size.width + shadow_inset * 2.0, new_card.size.height + shadow_inset * 2.0),
            )
        ];
        // 宿主高度清零、隐藏,并恢复原点,确保下次展开时顶边仍固定在按钮行下方。
        // Zero the host height, hide it, and restore its origin so the next expand keeps the top
        // fixed below the check button row.
        let host_frame: NSRect = msg_send![ui.update_host, frame];
        let _: () = msg_send![
            ui.update_host,
            setFrame: NSRect::new(
                NSPoint::new(host_frame.origin.x, ui.update_host_origin_y),
                NSSize::new(host_frame.size.width, 0.0)
            )
        ];
        let _: () = msg_send![ui.update_host, setHidden: true];
        let _: () = msg_send![ui.update_check_button, setHidden: false];
        ui.update_card_expanded = false;
    }
}

/// 在默认浏览器打开外部链接。
/// Open an external link in the default browser.
unsafe fn open_external_url(url: &str) {
    let url_string = make_nsstring(url);
    let url: *mut AnyObject = msg_send![class!(NSURL), URLWithString: url_string];
    CFRelease(url_string as *const c_void);
    if !url.is_null() {
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let _: bool = msg_send![workspace, openURL: url];
    }
}

/// 打开项目官方网站。
/// Open the project's official website in the default browser.
pub(crate) extern "C" fn handle_open_official_website(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    unsafe { open_external_url("https://oh-my-tab.app") }
}

/// 打开项目 GitHub 仓库。
/// Open the project's GitHub repository in the default browser.
pub(crate) extern "C" fn handle_open_github(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    unsafe { open_external_url("https://github.com/eacryo/oh-my-tab") }
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
        "layout.card_text_size",
        old.layout.card_text_size,
        new.layout.card_text_size
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
    changed!(
        "window_control.enabled",
        old.window_control.enabled,
        new.window_control.enabled
    );
    changed!(
        "window_control.up",
        old.window_control.up,
        new.window_control.up
    );
    changed!(
        "window_control.down",
        old.window_control.down,
        new.window_control.down
    );
    changed!(
        "window_control.left",
        old.window_control.left,
        new.window_control.left
    );
    changed!(
        "window_control.right",
        old.window_control.right,
        new.window_control.right
    );

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
    // Sparkle keeps its updater object alive for the process; apply the persisted toggles
    // immediately so the next automatic-check/download follows the user's choice.
    crate::updater::set_automatic_checks(cfg.updates.automatically_check);
    crate::updater::set_automatic_downloads(cfg.updates.automatically_download);
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

    // 窗口控制 tap 热切换(无需重启;start/stop 均幂等)。
    // Window-control tap hot-switch (no restart needed; start/stop are idempotent).
    if cfg.window_control.enabled {
        crate::window_management::start();
    } else {
        crate::window_management::stop();
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
    let tooltip = t("settings.tooltip_mouse_disabled");
    let device_available = !DEVICE_POPUP_KEYS.lock().unwrap().is_empty();
    // 无设备时下拉框始终禁用;其余控件仍由总开关控制。
    // Keep the device popup disabled when no device is connected; the remaining controls follow
    // the master switch.
    if on {
        SettingsRow::set_enabled(ui.device_indicator, device_available);
    } else {
        SettingsRow::set_enabled_with_tooltip(ui.device_indicator, false, &tooltip);
    }
    for &ctrl in &[
        ui.scroll_mode,
        ui.line_count,
        ui.reverse_scroll,
        ui.disable_pointer_accel,
    ] {
        SettingsRow::set_enabled_with_tooltip(ctrl, on, &tooltip);
    }
    update_mapping_controls_enabled(ui);
}

/// 根据应用切换器总开关状态,冻结其下方的窗口与键盘选项。
/// Freeze the window and keyboard options below the app-switcher master switch.
unsafe fn update_windows_controls_enabled(ui: &SettingsUi) {
    let state: isize = msg_send![ui.windows_enabled, state];
    let on = state == 1;
    let tooltip = t("settings.tooltip_windows_disabled");
    for &ctrl in &[
        ui.show_minimized,
        ui.thumbnails_enabled,
        ui.card_text_size,
        ui.card_text_size_value_label,
        ui.status_bar_text_size,
        ui.status_bar_text_size_value_label,
        ui.overlay_position,
        ui.corner_radius,
        ui.modifier,
    ] {
        SettingsRow::set_enabled_with_tooltip(ctrl, on, &tooltip);
    }
}

/// 根据剪贴板历史总开关状态,冻结其下方的历史选项。
/// Freeze the clipboard-history options below the clipboard master switch.
unsafe fn update_clipboard_controls_enabled(ui: &SettingsUi) {
    let state: isize = msg_send![ui.clipboard_enabled, state];
    let on = state == 1;
    let tooltip = t("settings.tooltip_clipboard_disabled");
    for &ctrl in &[
        ui.clipboard_pin_follow,
        ui.clipboard_persist,
        ui.clipboard_show_source_app,
        ui.clipboard_move_used_to_top,
        ui.clipboard_max_entries,
        ui.clipboard_auto_expire_days,
    ] {
        SettingsRow::set_enabled_with_tooltip(ctrl, on, &tooltip);
    }
}

/// 根据窗口控制总开关状态,冻结其下方的四个方向开关。
/// Freeze the four direction switches below the window-control master switch.
unsafe fn update_window_control_controls_enabled(ui: &SettingsUi) {
    let state: isize = msg_send![ui.window_control_enabled, state];
    let on = state == 1;
    let tooltip = t("settings.tooltip_window_control_disabled");
    for &ctrl in &[
        ui.window_control_up,
        ui.window_control_down,
        ui.window_control_left,
        ui.window_control_right,
    ] {
        SettingsRow::set_enabled_with_tooltip(ctrl, on, &tooltip);
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
unsafe fn update_mode_dependent_visibility(ui: &mut SettingsUi) {
    let idx: isize = msg_send![ui.scroll_mode, indexOfSelectedItem];
    let mode = SCROLL_MODE_VALUES
        .get(idx as usize)
        .copied()
        .unwrap_or("default");
    // 只有 Line 模式显示行数滑块(Default 不显示)。
    // Only Line mode shows the line-count slider (hidden on Default).
    let show_line = mode == "line";
    let compact = !show_line;
    if compact != ui.line_count_compact {
        // 默认模式移除整行及其布局占位,后续分组向上收拢;切回 Line 模式时恢复原位。
        // Remove the whole conditional row and its layout slot in Default mode; move later
        // sections up, then restore their original positions when Line mode returns.
        // The conditional row occupies one standard row slot inside the shared device card.
        // Shift the following sections by exactly that slot when the row is hidden or restored.
        // 条件行属于设备卡片,只占用一个标准行位。隐藏或恢复时,后续分组只移动这一行的高度。
        let removed_section_h = 8.0 + SettingsLayout::SINGLE_LINE_ROW_H;
        let shift = if compact {
            removed_section_h
        } else {
            -removed_section_h
        };
        let card_frame: NSRect = msg_send![ui.line_count_card, frame];
        // `mouse_view` is the scroll view; layout children live in its document view.
        // `mouse_view` 是滚动容器,实际布局子视图都在它的 document view 中。
        let document: *mut AnyObject = msg_send![ui.mouse_view, documentView];
        let subviews: *mut AnyObject = msg_send![document, subviews];
        let count: usize = msg_send![subviews, count];
        for i in 0..count {
            let view: *mut AnyObject = msg_send![subviews, objectAtIndex: i];
            if view == ui.line_count
                || view == ui.line_count_label
                || view == ui.line_count_value_label
                || view == ui.line_count_card
                || view == ui.line_count_shadow
            {
                continue;
            }
            let mut frame: NSRect = msg_send![view, frame];
            if frame.origin.y < card_frame.origin.y {
                frame.origin.y += shift;
                let _: () = msg_send![view, setFrame: frame];
            }
        }
        let mut compact_card_frame = card_frame;
        compact_card_frame.origin.y += shift;
        compact_card_frame.size.height -= shift;
        let _: () = msg_send![ui.line_count_card, setFrame: compact_card_frame];
        let shadow_inset = SETTINGS_CARD_SHADOW_INSET;
        let _: () = msg_send![
            ui.line_count_shadow,
            setFrame: NSRect::new(
                NSPoint::new(
                    compact_card_frame.origin.x - shadow_inset,
                    compact_card_frame.origin.y - shadow_inset,
                ),
                NSSize::new(
                    compact_card_frame.size.width + shadow_inset * 2.0,
                    compact_card_frame.size.height + shadow_inset * 2.0,
                ),
            )
        ];
        let _: () = msg_send![ui.line_count_separator, setHidden: compact];
        ui.line_count_compact = compact;
    }
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
        let mut ui = SETTINGS_UI.lock().unwrap();
        if let Some(u) = ui.as_mut() {
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

unsafe fn update_text_size_value_label(sender: *mut c_void, status_bar: bool) {
    let slider = sender as *mut AnyObject;
    let value: isize = msg_send![slider, integerValue];
    let ui = SETTINGS_UI.lock().unwrap();
    if let Some(u) = ui.as_ref() {
        let label = if status_bar {
            u.status_bar_text_size_value_label
        } else {
            u.card_text_size_value_label
        };
        set_field(label, value);
    }
}

pub(crate) extern "C" fn handle_card_text_size_changed(
    _self: *mut c_void,
    _cmd: Sel,
    sender: *mut c_void,
) {
    unsafe { update_text_size_value_label(sender, false) }
}

pub(crate) extern "C" fn handle_status_bar_text_size_changed(
    _self: *mut c_void,
    _cmd: Sel,
    sender: *mut c_void,
) {
    unsafe { update_text_size_value_label(sender, true) }
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

/// 应用切换器总开关回调:冻结/解冻下方窗口与键盘选项。
/// Callback for the app-switcher master switch: freeze/unfreeze the options below it.
pub(crate) extern "C" fn handle_windows_enabled_toggle(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    unsafe {
        let ui = SETTINGS_UI.lock().unwrap();
        if let Some(u) = ui.as_ref() {
            update_windows_controls_enabled(u);
        }
    }
}

/// 剪贴板历史总开关回调:冻结/解冻下方历史选项。
/// Callback for the clipboard-history master switch: freeze/unfreeze the options below it.
pub(crate) extern "C" fn handle_clipboard_enabled_toggle(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    unsafe {
        let ui = SETTINGS_UI.lock().unwrap();
        if let Some(u) = ui.as_ref() {
            update_clipboard_controls_enabled(u);
        }
    }
}

/// 窗口控制总开关回调:冻结/解冻下方四个方向开关。
/// Callback for the window-control master switch: freeze/unfreeze its four direction switches.
pub(crate) extern "C" fn handle_window_control_enabled_toggle(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    unsafe {
        let ui = SETTINGS_UI.lock().unwrap();
        if let Some(u) = ui.as_ref() {
            update_window_control_controls_enabled(u);
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
    } else {
        // 空列表时保留一个明确的不可选提示,避免空白下拉框看起来像加载失败。
        // Keep one explicit, non-selectable status item when the list is empty so the popup
        // does not look like a failed or incomplete load.
        let ns = make_nsstring(&t("settings.no_device_detected"));
        let _: () = msg_send![ui.device_indicator, addItemWithTitle: ns];
        CFRelease(ns as *const c_void);
    }
    let _: () = msg_send![ui.device_indicator, setEnabled: !keys.is_empty()];

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

pub(crate) extern "C" fn on_sidebar_select(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    let btn = sender as *mut AnyObject;
    let tag: isize = unsafe { msg_send![btn, tag] };
    select_sidebar(tag as usize);
    unsafe {
        // Keep an invisible origin at the clicked row so the next adjacent hover can glide from it.
        // 点击后保留当前行的不可见起点，让下一次相邻悬停可以从这里滑过去。
        widgets::prime_sidebar_hover_after_selection(btn);
    }
}

/// 切换侧边栏选中页:高亮背景对齐到选中按钮、切换六个内容视图显隐、选中项粗体。
/// Switch the active settings page: align the highlight to the selected button, toggle the six
/// content views' visibility, and bold the selected item's label.
fn select_sidebar(idx: usize) {
    // tag 越界时回退到通用页 / fall back to the General page if the tag is out of range
    let idx = if idx > 5 { 0 } else { idx };
    let previous_idx = SIDEBAR_SELECTED.swap(idx, Ordering::SeqCst);
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
            ui.sidebar_window_control,
            ui.sidebar_about,
        ];
        let views = [
            ui.general_view,
            ui.switcher_view,
            ui.mouse_view,
            ui.clipboard_view,
            ui.window_control_view,
            ui.about_view,
        ];
        // 高亮背景对齐到选中按钮的 frame / align the highlight to the selected button's frame
        let frame: NSRect = msg_send![buttons[idx], frame];
        // 鼠标已经停在目标 tab 上时,悬停背景已完成定位；点击只需同步选中态,避免重复播放
        // 一段明显的位移动画。键盘切换或非悬停切换仍保留完整 spring。
        // When the pointer is already over the target tab, the hover background is in place;
        // clicking only synchronizes selection instead of replaying a conspicuous glide.
        // Keyboard and non-hovered selection changes keep the full spring.
        let target_is_hovered = widgets::sidebar_button_is_hovered(buttons[idx]);
        SettingsSidebar::move_highlight(
            ui.sidebar_highlight,
            frame,
            previous_idx != idx && !target_is_hovered,
        );
        // 选中项使用强调色粗体，未选中项使用系统常规文本色。
        // Selected items use an accent-colored bold title; unselected items use the system label color.
        let titles = [
            t("settings.sidebar_general"),
            t("settings.sidebar_switcher"),
            t("settings.sidebar_mouse"),
            t("settings.sidebar_clipboard"),
            t("settings.sidebar_window_control"),
            t("settings.sidebar_about"),
        ];
        for (i, &b) in buttons.iter().enumerate() {
            let layer: *mut AnyObject = msg_send![b, layer];
            if !layer.is_null() {
                layer_set_background(layer, crate::ffi::hex_to_cg_color(0x00000000u32));
            }
            set_sidebar_title(b, &titles[i], i == idx);
        }
        // 切换六页显隐 / toggle the six pages' visibility
        for (i, &v) in views.iter().enumerate() {
            let _: () = msg_send![v, setHidden: i != idx];
        }
        // 刚显示的页(如从隐藏切出来)需先排版,clip bounds 才会正确,随后滚到顶部。
        // A just-shown page needs a layout pass first so the clip bounds are correct;
        // then scroll it to the top. layoutIfNeeded lives on the window, not the scroll view.
        let _: () = msg_send![ui.window, layoutIfNeeded];
        let page = SettingsPage {
            scroll: views[idx],
            document: msg_send![views[idx], documentView],
        };
        page.scroll_to_top();
        let page_names = [
            "general",
            "switcher",
            "mouse",
            "clipboard",
            "window-control",
            "about",
        ];
        page.validate(page_names[idx]);
    }
}

/// 同步主题菜单标签并立即应用配置(主题/浮窗)。
/// Sync menu labels and apply the config immediately (theme / overlay).
fn apply_config_refresh() {
    refresh_menu_titles();
    invalidate_settings_window();
    crate::clipboard::refresh_localized_ui();
    unsafe {
        crate::clipboard::apply_theme();
    }
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
        // A window order-out is not guaranteed to deliver mouseExited for its tracking areas;
        // clear the shared hover state before reusing the settings window.
        // 窗口 orderOut 不保证会为 tracking area 发送 mouseExited；复用设置窗口前先清理共享
        // 悬停状态，避免旧条目在重新打开后留下幽灵高亮。
        widgets::clear_sidebar_hover();
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
            // orderOut can bypass the sidebar tracking-area exit callback, so do not leave the
            // shared hover pill pointing at a row while the window is hidden.
            // orderOut 可能绕过侧栏 tracking area 的退出回调，窗口隐藏前不能留下仍指向旧条目的
            // 共享悬停气泡。
            widgets::clear_sidebar_hover();
            // 先释放设置锁再关闭颜色面板,通知回调会重新访问 SETTINGS_UI。
            // Release the settings lock before closing the color panel; its notification callback
            // re-enters SETTINGS_UI.
            close_glass_tint_panel(well);
            let _: () = msg_send![window, orderOut: std::ptr::null::<AnyObject>()];
        }
    }
    clear_glass_preview();
    if SYSTEM_APPEARANCE_REBUILD_PENDING.swap(false, Ordering::SeqCst) {
        invalidate_settings_window();
    }
    // 切回 .accessory:设置窗口关闭,回到纯菜单栏(无 Dock 图标)。
    // Switch back to .accessory: the settings window is closed, return to pure menu-bar (no Dock icon).
    crate::set_settings_activation_policy(false);
}

/// Re-apply the effective system appearance while preserving an in-progress settings edit.
///
/// Custom settings layers are painted with concrete palette colors at construction time. When
/// the window is visible, snapshot the form and current page, rebuild the window, then restore
/// both; this avoids the mixed light/dark state caused by updating only NSWindow.appearance.
///
/// 重新应用系统外观并保留进行中的设置编辑。设置页自绘图层在创建时写入具体调色板颜色,
/// 所以窗口可见时先保存表单和当前页、重建窗口再恢复,避免只更新 NSWindow appearance 导致
/// 明暗混杂。窗口隐藏时直接作废缓存,下次打开按新调色板重建。
pub(crate) fn refresh_system_appearance() {
    let (window, visible) = SETTINGS_UI
        .lock()
        .unwrap()
        .as_ref()
        .map(|ui| unsafe {
            let visible: bool = msg_send![ui.window, isVisible];
            (ui.window, visible)
        })
        .unwrap_or((std::ptr::null_mut(), false));

    if window.is_null() || !visible {
        invalidate_settings_window();
        return;
    }

    let (snapshot, page, frame) = {
        let (cfg, _) = collect_settings_config();
        let page = SIDEBAR_SELECTED.load(Ordering::SeqCst);
        let frame: NSRect = unsafe { msg_send![window, frame] };
        (cfg, page, frame)
    };

    invalidate_settings_window();
    show_settings();
    load_settings_from(&snapshot);
    select_sidebar(page);
    unsafe {
        let ui = SETTINGS_UI.lock().unwrap();
        if let Some(ui) = ui.as_ref() {
            let _: () = msg_send![ui.window, setFrame: frame, display: true];
        }
    }
}

/// Run every settings page through the real AppKit layout path and debug validator, then exit.
/// This is used by the ignored macOS smoke test; it deliberately exercises the same window
/// builder as the interactive app instead of constructing a simplified test-only hierarchy.
/// 在真实 AppKit 布局路径中依次验证所有设置页，然后退出。供 macOS ignored smoke test 使用，
/// 复用交互应用的窗口构建逻辑，不创建简化的测试专用 view tree。
pub(crate) fn settings_layout_smoke_runner() -> bool {
    unsafe {
        show_settings();
        let (window, pages) = {
            let ui = SETTINGS_UI.lock().unwrap();
            let Some(ui) = ui.as_ref() else {
                return false;
            };
            (
                ui.window,
                [
                    ui.general_view,
                    ui.switcher_view,
                    ui.mouse_view,
                    ui.clipboard_view,
                    ui.window_control_view,
                    ui.about_view,
                ],
            )
        };
        let names = [
            "general",
            "switcher",
            "mouse",
            "clipboard",
            "window-control",
            "about",
        ];
        for (index, page) in pages.iter().enumerate() {
            select_sidebar(index);
            let _: () = msg_send![window, layoutIfNeeded];
            scroll_page_to_top(*page);
            debug_validate_settings_page(*page, names[index]);
        }
        hide_settings();
        true
    }
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
        GLASS_UI_UPDATE.store(true, Ordering::SeqCst);
        let tint =
            crate::ffi::hex_to_ns_color(crate::config::parse_hex8(&cfg.appearance.glass_tint));
        let _: () = msg_send![ui.glass_tint, setColor: tint];
        let panel: *mut AnyObject = msg_send![class!(NSColorPanel), sharedColorPanel];
        let _: () = msg_send![panel, setColor: tint];
        GLASS_UI_UPDATE.store(false, Ordering::SeqCst);
        set_field(ui.corner_radius, cfg.appearance.corner_radius);
        let card_text_size = text_size_slider_value(cfg.layout.card_text_size);
        let status_bar_text_size = text_size_slider_value(cfg.fonts.status_bar_size);
        let _: () = msg_send![ui.card_text_size, setIntegerValue: card_text_size as isize];
        set_field(ui.card_text_size_value_label, card_text_size);
        let _: () = msg_send![
            ui.status_bar_text_size,
            setIntegerValue: status_bar_text_size as isize
        ];
        set_field(ui.status_bar_text_size_value_label, status_bar_text_size);
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
        let _: () = msg_send![
            ui.update_auto_download,
            setState: if cfg.updates.automatically_download { 1isize } else { 0isize }
        ];
        update_windows_controls_enabled(ui);

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
        // ===== 窗口控制页:填充启用开关 =====
        // Window-control page: populate the master and direction switches.
        let _: () = msg_send![
            ui.window_control_enabled,
            setState: if cfg.window_control.enabled { 1isize } else { 0isize }
        ];
        let _: () = msg_send![
            ui.window_control_up,
            setState: if cfg.window_control.up { 1isize } else { 0isize }
        ];
        let _: () = msg_send![
            ui.window_control_down,
            setState: if cfg.window_control.down { 1isize } else { 0isize }
        ];
        let _: () = msg_send![
            ui.window_control_left,
            setState: if cfg.window_control.left { 1isize } else { 0isize }
        ];
        let _: () = msg_send![
            ui.window_control_right,
            setState: if cfg.window_control.right { 1isize } else { 0isize }
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
        update_clipboard_controls_enabled(ui);
        update_window_control_controls_enabled(ui);
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
        let theme_idx: isize = msg_send![ui.theme, indexOfSelectedItem];
        cfg.appearance.theme = match theme_idx {
            0 => "dark",
            1 => "light",
            _ => "auto",
        }
        .into();
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
        let card_text_size: isize = msg_send![ui.card_text_size, integerValue];
        cfg.layout.card_text_size = card_text_size as f64;
        let status_bar_text_size: isize = msg_send![ui.status_bar_text_size, integerValue];
        cfg.fonts.status_bar_size = status_bar_text_size as f64;
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
        let download_state: isize = msg_send![ui.update_auto_download, state];
        cfg.updates.automatically_download = download_state == 1;

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
        // ===== 窗口控制页(全局配置)=====
        // Window-control page (global config).
        let wc_state: isize = msg_send![ui.window_control_enabled, state];
        cfg.window_control.enabled = wc_state == 1;
        let wc_up_state: isize = msg_send![ui.window_control_up, state];
        cfg.window_control.up = wc_up_state == 1;
        let wc_down_state: isize = msg_send![ui.window_control_down, state];
        cfg.window_control.down = wc_down_state == 1;
        let wc_left_state: isize = msg_send![ui.window_control_left, state];
        cfg.window_control.left = wc_left_state == 1;
        let wc_right_state: isize = msg_send![ui.window_control_right, state];
        cfg.window_control.right = wc_right_state == 1;
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
                widgets::settings_select_handle_window_mouse_down(window, event);
                tooltip::SettingsTooltip::handle_mouse_down(window, event);
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

/// Apply the resolved appearance to the settings window and its semantic AppKit controls.
/// 将解析后的主题应用到设置窗口及其依赖语义颜色的 AppKit 控件。
unsafe fn apply_settings_window_appearance(window: *mut AnyObject) {
    let name = make_nsstring(if resolved_is_dark() {
        "NSAppearanceNameDarkAqua"
    } else {
        "NSAppearanceNameAqua"
    });
    let appearance: *mut AnyObject = msg_send![class!(NSAppearance), appearanceNamed: name];
    CFRelease(name as *const c_void);
    if !appearance.is_null() {
        let _: () = msg_send![window, setAppearance: appearance];
    }
}

fn create_settings_window() {
    unsafe {
        let palette = settings_palette();
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
        apply_settings_window_appearance(window);
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
            layer_set_background(cv_layer, crate::ffi::hex_to_cg_color(palette.window_bg));
            let _: () = msg_send![cv_layer, setCornerRadius: window_clip_radius];
            let _: () = msg_send![cv_layer, setMasksToBounds: true];
        }

        let mut ui = SettingsUi {
            window,
            sidebar_general: std::ptr::null_mut(),
            sidebar_switcher: std::ptr::null_mut(),
            sidebar_mouse: std::ptr::null_mut(),
            sidebar_clipboard: std::ptr::null_mut(),
            sidebar_window_control: std::ptr::null_mut(),
            sidebar_about: std::ptr::null_mut(),
            sidebar_highlight: std::ptr::null_mut(),
            general_view: std::ptr::null_mut(),
            switcher_view: std::ptr::null_mut(),
            mouse_view: std::ptr::null_mut(),
            clipboard_view: std::ptr::null_mut(),
            window_control_view: std::ptr::null_mut(),
            about_view: std::ptr::null_mut(),
            about_subtitle: std::ptr::null_mut(),
            theme: std::ptr::null_mut(),
            glass_style: std::ptr::null_mut(),
            glass_tint: std::ptr::null_mut(),
            glass_preview_switcher: std::ptr::null_mut(),
            glass_preview_clipboard: std::ptr::null_mut(),
            corner_radius: std::ptr::null_mut(),
            thumbnails_enabled: std::ptr::null_mut(),
            card_text_size: std::ptr::null_mut(),
            card_text_size_value_label: std::ptr::null_mut(),
            status_bar_text_size: std::ptr::null_mut(),
            status_bar_text_size_value_label: std::ptr::null_mut(),
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
            line_count_card: std::ptr::null_mut(),
            line_count_shadow: std::ptr::null_mut(),
            line_count_separator: std::ptr::null_mut(),
            line_count_compact: false,
            disable_pointer_accel: std::ptr::null_mut(),
            mapping_scroll: std::ptr::null_mut(),
            mapping_doc: std::ptr::null_mut(),
            mapping_card: std::ptr::null_mut(),
            mapping_panel: std::ptr::null_mut(),
            mapping_rows: Vec::new(),
            clipboard_enabled: std::ptr::null_mut(),
            window_control_enabled: std::ptr::null_mut(),
            window_control_up: std::ptr::null_mut(),
            window_control_down: std::ptr::null_mut(),
            window_control_left: std::ptr::null_mut(),
            window_control_right: std::ptr::null_mut(),
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
            update_auto_download: std::ptr::null_mut(),
            update_check_button: std::ptr::null_mut(),
            update_host: std::ptr::null_mut(),
            update_host_window: std::ptr::null_mut(),
            update_card: std::ptr::null_mut(),
            update_card_shadow: std::ptr::null_mut(),
            update_card_compact_h: 0.0,
            update_card_expanded: false,
            update_host_origin_y: 0.0,
        };

        // The sidebar and detail pane meet directly at the original sidebar boundary; their
        // backgrounds provide the visual split instead of an inset outer card.
        // 左侧导航和右侧详情直接衔接，通过两种背景色区分，不再使用内缩的外框卡片。
        let content_x = card_w;
        let detail_w = view_w - content_x;
        let page_inset = 32.0;
        let page_x = content_x + page_inset;
        let content_w = detail_w - page_inset * 2.0;
        let layout = SettingsLayout::new(content_w);
        let label_x = layout.label_x;
        let label_w = layout.label_w;
        let ctrl_w = layout.control_w;
        let ctrl_x = layout.control_x;
        // HTML `.row` uses a 34pt control. All card rows now use a compact 54pt rhythm; detailed
        // explanatory paragraphs are intentionally omitted from the card interior.
        // HTML `.row` 控件仍为 34pt；卡片内统一使用紧凑的 54pt 节奏，详细说明不再塞进行内。
        let row_h = layout.row_h;
        let described_row_h = layout.described_row_h;

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
            layer_set_background(
                sidebar_layer,
                crate::ffi::hex_to_cg_color(palette.sidebar_bg),
            );
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
            layer_set_background(
                divider_layer,
                crate::ffi::hex_to_cg_color(palette.separator),
            );
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
            layer_set_background(main_layer, crate::ffi::hex_to_cg_color(palette.detail_bg));
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
            layer_set_background(footer_layer, crate::ffi::hex_to_cg_color(palette.footer_bg));
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
                crate::ffi::hex_to_cg_color(palette.card_border),
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
        let app_title_color = settings_text_color(SettingsTextRole::Primary);
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
            msg_send![class!(NSFont), systemFontOfSize: 12.0f64];
        let _: () = msg_send![app_subtitle, setFont: app_subtitle_font];
        let app_subtitle_color = settings_text_color(SettingsTextRole::Muted);
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
        layer_set_background(hl_layer, crate::ffi::hex_to_cg_color(palette.selection_bg));
        let _: () = msg_send![sidebar_view, addSubview: highlight];
        release_obj(highlight);
        ui.sidebar_highlight = highlight;

        // Six sidebar buttons (borderless, tags 0..5; click triggers handleSettingsSidebar:).
        let sidebar_buttons = SettingsSidebar::build(sidebar_view, target, 14.0, btn_y0, btn_w);
        [
            &mut ui.sidebar_general,
            &mut ui.sidebar_switcher,
            &mut ui.sidebar_mouse,
            &mut ui.sidebar_clipboard,
            &mut ui.sidebar_window_control,
            &mut ui.sidebar_about,
        ]
        .iter_mut()
        .zip(sidebar_buttons)
        .for_each(|(slot, button)| **slot = button);

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
                crate::ffi::hex_to_cg_color(palette.separator),
            );
        }
        let _: () = msg_send![sidebar_view, addSubview: sidebar_footer_line];
        release_obj(sidebar_footer_line);
        let restore = SettingsButton::action(
            NSRect::new(NSPoint::new(22.0, 20.0), NSSize::new(card_w - 44.0, 30.0)),
            &t("settings.btn_restore_defaults"),
            target,
            sel!(handleRestoreDefaults:),
            SettingsButtonRole::Action,
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
        // Build from generous provisional heights. They are intentionally not shrunk after child
        // frames are assigned: the pages use manual top-anchored coordinates, so post-hoc
        // shrinking would let AppKit move the children a second time.
        // 先用宽松的临时高度构建；子视图定位后不再收缩 document，因为页面是手动顶部锚定坐标，
        // 布局后收缩会让 AppKit 再次移动子视图。
        let general_doc_h = 1120.0;
        let switcher_doc_h = 1280.0;
        let mouse_doc_h = 1540.0;
        let clipboard_doc_h = 960.0;
        // 窗口控制页包含总开关和四个方向开关,高度留出描述文字的空间。
        // The window-control page contains the master plus four direction switches, with room
        // for each row's description.
        let window_control_doc_h = 760.0;
        let about_doc_h = 1300.0;

        let general_page = SettingsPage::new(content, page_frame, general_doc_h, false);
        let general_root = general_page.scroll;
        let general_view = general_page.document;
        ui.general_view = general_root;
        let switcher_page = SettingsPage::new(content, page_frame, switcher_doc_h, true);
        let switcher_root = switcher_page.scroll;
        let switcher_view = switcher_page.document;
        ui.switcher_view = switcher_root;
        let mouse_page = SettingsPage::new(content, page_frame, mouse_doc_h, true);
        let mouse_root = mouse_page.scroll;
        let mouse_view = mouse_page.document;
        ui.mouse_view = mouse_root;
        let clipboard_page = SettingsPage::new(content, page_frame, clipboard_doc_h, true);
        let clipboard_root = clipboard_page.scroll;
        let clipboard_view = clipboard_page.document;
        ui.clipboard_view = clipboard_root;
        let window_control_page =
            SettingsPage::new(content, page_frame, window_control_doc_h, true);
        let window_control_root = window_control_page.scroll;
        let window_control_view = window_control_page.document;
        ui.window_control_view = window_control_root;
        let about_page = SettingsPage::new(content, page_frame, about_doc_h, true);
        let about_root = about_page.scroll;
        let about_view = about_page.document;
        ui.about_view = about_root;

        // ===== 通用页内容 general page content =====
        let general_top = general_doc_h - 24.0;
        let mut y = general_top; // top cursor: bottom edge of the next element
        add_page_title(
            general_view,
            &t("settings.sidebar_general"),
            6.0,
            y - 34.0,
            content_w - 12.0,
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
        let open_btn = SettingsButton::action(
            NSRect::new(
                NSPoint::new(content_w - 150.0, (banner_h - 28.0) / 2.0),
                NSSize::new(140.0, 28.0),
            ),
            &t("settings.btn_open_privacy"),
            target,
            sel!(handleOpenPrivacy:),
            SettingsButtonRole::Action,
        );
        let _: () = msg_send![banner, addSubview: open_btn];
        release_obj(open_btn);

        // 默认按当前权限显隐(有权限就隐藏)/ initial visibility: hidden when permission is already granted
        let _: () = msg_send![banner, setHidden: has_accessibility_permission()];

        // --- 外观 Appearance ---
        y -= 12.0;
        let appearance_header_y = y;
        y = layout.next_row_cursor(y, described_row_h);
        let theme_items = [
            t("settings.theme_dark"),
            t("settings.theme_light"),
            t("settings.theme_auto"),
        ];
        let theme_item_refs: Vec<&str> = theme_items.iter().map(String::as_str).collect();
        ui.theme = SettingsRow::described(
            general_view,
            label_x,
            y,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_theme"),
            &t("settings.desc_theme"),
            SettingsControl::popup(ctrl_x, y + 10.0, ctrl_w, row_h, &theme_item_refs, 0),
        );
        y -= described_row_h;
        SettingsRow::separator(general_view, y + described_row_h, content_w);
        ui.glass_style = SettingsRow::described(
            general_view,
            label_x,
            y,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_glass_style"),
            &t("settings.desc_glass_style"),
            SettingsControl::popup(ctrl_x, y + 10.0, ctrl_w, row_h, &["Regular", "Clear"], 0),
        );
        let _: () = msg_send![ui.glass_style, setTarget: target];
        let _: () = msg_send![ui.glass_style, setAction: sel!(handleGlassStyleChanged:)];
        y -= described_row_h;
        SettingsRow::separator(general_view, y + described_row_h, content_w);
        ui.glass_tint = SettingsRow::described(
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
        let appearance_card_bottom = layout.card_bottom(y);
        let appearance_card_top = layout.card_top(appearance_header_y);
        SettingsSection::attach(
            general_view,
            NSRect::new(
                NSPoint::new(6.0, appearance_card_bottom),
                NSSize::new(
                    content_w - 12.0,
                    appearance_card_top - appearance_card_bottom,
                ),
            ),
            &t("settings.header_appearance"),
        );

        // --- 实时预览 Live preview ---
        y = layout.next_section_cursor(y);
        let preview_header_y = y;
        y = layout.next_row_cursor(y, row_h);
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
        SettingsSection::attach(
            general_view,
            NSRect::new(
                NSPoint::new(6.0, preview_y - 12.0),
                NSSize::new(
                    content_w - 12.0,
                    (preview_header_y - layout.card_header_gap) - (preview_y - 12.0),
                ),
            ),
            &t("settings.header_preview"),
        );

        // --- 语言 Language ---
        y = layout.next_section_cursor(y);
        let language_header_y = y;
        y = layout.next_row_cursor(y, described_row_h);
        let language_card_bottom = layout.card_bottom(y);
        let language_card_top = layout.card_top(language_header_y);
        ui.locale = SettingsRow::plain(
            general_view,
            label_x,
            y,
            label_w,
            described_row_h,
            &t("settings.row_locale"),
            SettingsControl::popup(ctrl_x, y, ctrl_w, row_h, &LOCALE_LABELS, 0),
        );
        SettingsSection::attach(
            general_view,
            NSRect::new(
                NSPoint::new(6.0, language_card_bottom),
                NSSize::new(content_w - 12.0, language_card_top - language_card_bottom),
            ),
            &t("settings.header_language"),
        );

        // --- 日志 Logging ---
        y = layout.next_section_cursor(y);
        let logging_header_y = y;
        y = layout.next_row_cursor(y, described_row_h);
        // 日志级别下拉框:项 = [debug, info];默认 index 1(info)。
        // Log level popup: items = [debug, info]; default index 1 (info).
        let log_levels: [&str; 2] = ["Debug", "Info"];
        ui.log_level = SettingsRow::described(
            general_view,
            label_x,
            y,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_log_level"),
            &t("settings.desc_log_level"),
            SettingsControl::popup(ctrl_x, y + 10.0, ctrl_w, row_h, &log_levels, 1),
        );
        SettingsSection::attach(
            general_view,
            NSRect::new(
                NSPoint::new(6.0, layout.card_bottom(y)),
                NSSize::new(
                    content_w - 12.0,
                    layout.card_top(logging_header_y) - layout.card_bottom(y),
                ),
            ),
            &t("settings.header_logging"),
        );

        // --- 启动 Startup ---
        y = layout.next_section_cursor(y);
        let startup_header_y = y;
        y = layout.next_row_cursor(y, described_row_h);
        // 开机自启开关:标题留空(左侧 row label 已说明),仅放一个 switch。
        // Launch-at-login switch: no title (the row label on the left already describes it).
        ui.launch_at_login = SettingsRow::described(
            general_view,
            label_x,
            y,
            content_w - label_x * 2.0 - 58.0,
            described_row_h,
            &t("settings.row_launch_at_login"),
            &t("settings.desc_launch_at_login"),
            SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
        );
        SettingsSection::attach(
            general_view,
            NSRect::new(
                NSPoint::new(6.0, layout.card_bottom(y)),
                NSSize::new(
                    content_w - 12.0,
                    layout.card_top(startup_header_y) - layout.card_bottom(y),
                ),
            ),
            &t("settings.header_startup"),
        );

        // ===== 应用切换浮窗页内容 switcher overlay page content =====
        let mut y = switcher_doc_h - 24.0;
        add_page_title(
            switcher_view,
            &t("settings.sidebar_switcher"),
            6.0,
            y - 34.0,
            content_w - 12.0,
        );
        y -= 62.0;

        // --- 窗口 Window ---
        y -= 12.0;
        let windows_header_y = y;
        y = layout.next_row_cursor(y, described_row_h);
        // 窗口切换总开关:关闭后 Cmd+Tab 透传给系统(原生切换器接管)。
        // App-switcher master switch: off = Cmd+Tab passes through to the system.
        let windows_master_row_y = y;
        ui.windows_enabled = SettingsRow::described(
            switcher_view,
            label_x,
            y,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_windows_enabled"),
            &t("settings.desc_windows_enabled"),
            SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
        );
        let _: () = msg_send![ui.windows_enabled, setTarget: target];
        let _: () = msg_send![
            ui.windows_enabled,
            setAction: sel!(handleWindowsEnabledToggle:)
        ];
        SettingsSection::attach(
            switcher_view,
            NSRect::new(
                NSPoint::new(6.0, layout.card_bottom(windows_master_row_y)),
                NSSize::new(
                    content_w - 12.0,
                    layout.card_top(windows_header_y) - layout.card_bottom(windows_master_row_y),
                ),
            ),
            &t("settings.header_windows"),
        );
        // The remaining window settings form a second card with its own section title.
        // 其余窗口设置单独成卡,并为卡片补充独立的小标题。
        y = layout.next_section_cursor(y);
        let windows_options_header_y = y;
        y = layout.next_row_cursor(y, described_row_h);
        // show_minimized 开关(切换器语义本就只有显/隐两态,用 Toggle 比下拉更直观)。
        // 英文标签较长,该行标签加宽;开关保留参考页面的右侧内边距。
        // show_minimized is inherently two-state, so a toggle is clearer than a popup. The long
        // English label uses a wider label column, while the switch stays aligned to the popups.
        ui.show_minimized = SettingsRow::tall(
            switcher_view,
            label_x,
            y,
            220.0,
            &t("settings.row_show_minimized"),
            SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
        )
        .1;
        y = layout.next_row_cursor(y, described_row_h);
        SettingsRow::separator(switcher_view, y + described_row_h + 3.0, content_w);
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
        ui.thumbnails_enabled = SettingsRow::tall(
            switcher_view,
            label_x,
            y,
            220.0,
            &t("settings.row_window_display_mode"),
            SettingsControl::popup(
                ctrl_x,
                y + 10.0,
                ctrl_w,
                row_h,
                &window_display_mode_refs,
                0,
            ),
        )
        .1;
        y = layout.next_row_cursor(y, described_row_h);
        SettingsRow::separator(switcher_view, y + described_row_h + 3.0, content_w);
        ui.card_text_size = SettingsRow::described(
            switcher_view,
            label_x,
            y,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_card_text_size"),
            &t("settings.desc_card_text_size"),
            SettingsControl::slider(
                ctrl_x,
                y + 10.0,
                ctrl_w - TEXT_SIZE_VALUE_W - 6.0,
                row_h,
                TEXT_SIZE_MIN,
                TEXT_SIZE_MAX,
                TEXT_SIZE_DEFAULT,
            ),
        );
        let text_size_value_y = y + 10.0 + (row_h - TEXT_SIZE_VALUE_H) / 2.0;
        ui.card_text_size_value_label = make_text_size_value_label(
            switcher_view,
            ctrl_x + ctrl_w - TEXT_SIZE_VALUE_W,
            text_size_value_y,
            TEXT_SIZE_VALUE_W,
            TEXT_SIZE_VALUE_H,
            TEXT_SIZE_DEFAULT,
        );
        let _: () = msg_send![ui.card_text_size, setTarget: target];
        let _: () = msg_send![
            ui.card_text_size,
            setAction: sel!(handleCardTextSizeChanged:)
        ];
        y = layout.next_row_cursor(y, described_row_h);
        SettingsRow::separator(switcher_view, y + described_row_h + 3.0, content_w);
        ui.status_bar_text_size = SettingsRow::described(
            switcher_view,
            label_x,
            y,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_status_bar_text_size"),
            &t("settings.desc_status_bar_text_size"),
            SettingsControl::slider(
                ctrl_x,
                y + 10.0,
                ctrl_w - TEXT_SIZE_VALUE_W - 6.0,
                row_h,
                TEXT_SIZE_MIN,
                TEXT_SIZE_MAX,
                TEXT_SIZE_DEFAULT,
            ),
        );
        let text_size_value_y = y + 10.0 + (row_h - TEXT_SIZE_VALUE_H) / 2.0;
        ui.status_bar_text_size_value_label = make_text_size_value_label(
            switcher_view,
            ctrl_x + ctrl_w - TEXT_SIZE_VALUE_W,
            text_size_value_y,
            TEXT_SIZE_VALUE_W,
            TEXT_SIZE_VALUE_H,
            TEXT_SIZE_DEFAULT,
        );
        let _: () = msg_send![ui.status_bar_text_size, setTarget: target];
        let _: () = msg_send![
            ui.status_bar_text_size,
            setAction: sel!(handleStatusBarTextSizeChanged:)
        ];
        y = layout.next_row_cursor(y, described_row_h);
        SettingsRow::separator(switcher_view, y + described_row_h + 3.0, content_w);
        // overlay_position 下拉框:项 = [跟随激活窗口, 始终显示在主屏幕];默认 index 0。
        // overlay_position popup: [Follow Active Window, Always on Main Screen]; default index 0.
        let op_labels = [
            t("settings.overlay_position_follow_active"),
            t("settings.overlay_position_main_screen"),
        ];
        let op_label_refs: Vec<&str> = op_labels.iter().map(|s| s.as_str()).collect();
        ui.overlay_position = SettingsRow::tall(
            switcher_view,
            label_x,
            y,
            label_w,
            &t("settings.row_overlay_position"),
            SettingsControl::popup(ctrl_x, y + 10.0, ctrl_w, row_h, &op_label_refs, 0),
        )
        .1;
        y = layout.next_row_cursor(y, described_row_h);
        SettingsRow::separator(switcher_view, y + described_row_h + 3.0, content_w);
        ui.corner_radius = SettingsRow::tall(
            switcher_view,
            label_x,
            y,
            label_w,
            &t("settings.row_corner_radius"),
            SettingsControl::text_input(ctrl_x, y + 10.0, ctrl_w, row_h, "64"),
        )
        .1;
        SettingsSection::attach(
            switcher_view,
            NSRect::new(
                NSPoint::new(6.0, layout.card_bottom(y)),
                NSSize::new(
                    content_w - 12.0,
                    layout.card_top(windows_options_header_y) - layout.card_bottom(y),
                ),
            ),
            &t("settings.header_window_options"),
        );

        // --- 键盘 Keyboard ---
        y = layout.next_section_cursor(y);
        let keyboard_header_y = y;
        y = layout.next_row_cursor(y, described_row_h);
        let keyboard_card_bottom = layout.card_bottom(y);
        let keyboard_card_top = layout.card_top(keyboard_header_y);
        // 修饰键下拉项:显示 Option+Tab / Command+Tab;值由索引映射到 option/command。
        // Modifier popup shows Option+Tab / Command+Tab; the index maps to option/command.
        let mod_labels = [
            t("settings.modifier_option"),
            t("settings.modifier_command"),
        ];
        let mod_label_refs: Vec<&str> = mod_labels.iter().map(|s| s.as_str()).collect();
        ui.modifier = SettingsRow::tall(
            switcher_view,
            label_x,
            y,
            label_w,
            &t("settings.row_modifier"),
            SettingsControl::popup(ctrl_x, y, ctrl_w, row_h, &mod_label_refs, 0),
        )
        .1;
        SettingsSection::attach(
            switcher_view,
            NSRect::new(
                NSPoint::new(6.0, keyboard_card_bottom),
                NSSize::new(content_w - 12.0, keyboard_card_top - keyboard_card_bottom),
            ),
            &t("settings.header_keyboard"),
        );

        // ===== 鼠标页内容 mouse page content =====
        let mut y = mouse_doc_h - 24.0;
        add_page_title(
            mouse_view,
            &t("settings.sidebar_mouse"),
            6.0,
            y - 34.0,
            content_w - 12.0,
        );
        y -= 62.0;

        // --- 启用鼠标控制(总开关,置于最顶) / Enable mouse control (topmost) ---
        y = layout.next_row_cursor(y, described_row_h);
        let enable_mouse_bottom = y;
        ui.enable_mouse = SettingsRow::described(
            mouse_view,
            label_x,
            y,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_enable_mouse"),
            &t("settings.desc_enable_mouse"),
            SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
        );
        // switch toggle 时实时更新 OK 按钮标题(确认 vs 确认并重启)。
        // Update OK button title in real time when the switch toggles (OK vs OK && Restart).
        let _: () = msg_send![ui.enable_mouse, setTarget: target];
        let _: () = msg_send![ui.enable_mouse, setAction: sel!(handleEnableMouseToggle:)];
        SettingsCard::attach(
            mouse_view,
            NSRect::new(
                NSPoint::new(6.0, enable_mouse_bottom - layout.card_padding),
                NSSize::new(
                    content_w - 12.0,
                    described_row_h + layout.card_padding * 2.0,
                ),
            ),
        );

        // --- 设备选择器(内嵌下拉框,切换即时刷新其余控件) / Device picker (inline popup) ---
        y = layout.next_section_cursor(y);
        let device_header_y = y;
        y = layout.next_row_cursor(y, described_row_h);
        // 下拉框:items 在 load_settings_values 里动态重建(设备列表可变)。
        // 首次创建放一个占位项,真正的内容在 load_settings_values -> rebuild_device_popup 填入。
        // Popup: items are rebuilt dynamically in load_settings_values (device list is mutable).
        // A placeholder is inserted here; the real items are filled by rebuild_device_popup.
        let dev_popup = SettingsControl::popup(ctrl_x, y + 10.0, ctrl_w, row_h, &[""], 0);
        style_flat_popup(dev_popup);
        // 绑定 target/action:选择变化时即时刷新其余控件为该设备的有效值。
        // Bind target/action: on selection change, immediately refresh the other controls with
        // the selected device's effective values.
        let _: () = msg_send![dev_popup, setTarget: target];
        let _: () = msg_send![dev_popup, setAction: sel!(handleDeviceChanged:)];
        ui.device_indicator = SettingsRow::tall(
            mouse_view,
            label_x,
            y,
            label_w,
            &t("settings.header_mouse_device"),
            dev_popup,
        )
        .1;

        // --- 滚动模式 / Scroll mode ---
        y = layout.next_row_cursor(y, described_row_h);
        let scroll_popup =
            SettingsControl::popup(ctrl_x, y + 10.0, ctrl_w, row_h, &SCROLL_MODE_LABELS, 0);
        style_flat_popup(scroll_popup);
        ui.scroll_mode = SettingsRow::tall(
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
        SettingsRow::separator(mouse_view, y + described_row_h + 3.0, content_w);

        // --- 行数(按行模式) / Line count (line mode) ---
        // Keep this conditional row in the same card as Device and Scroll mode.
        // 将这个条件行放进与 Device、Scroll mode 相同的卡片中。
        y = layout.next_row_cursor(y, described_row_h);
        ui.line_count_separator =
            SettingsRow::separator(mouse_view, y + described_row_h + 3.0, content_w);
        let (line_label, line_ctrl) = SettingsRow::tall(
            mouse_view,
            label_x,
            y,
            label_w,
            &t("settings.row_line_count"),
            // 整数滑块 1..=10(与 config 校验一致;对齐 LinearMouse By Lines 的滑块交互)。
            // 右侧留 ~40pt 放只读数值 label 显示当前值。
            // Integer slider 1..=10 (matches config validation; mirrors LinearMouse's
            // By Lines slider interaction). ~40pt on the right holds a read-only value label.
            SettingsControl::slider(ctrl_x, y + 10.0, ctrl_w - 40.0, row_h, 1, 10, 3),
        );
        ui.line_count = line_ctrl;
        ui.line_count_label = line_label;
        // 滑块右侧的只读数值 label:显示当前行数,拖动滑块时实时刷新。
        // Read-only value label right of the slider: shows the current line count, refreshed
        // live as the slider moves.
        let value_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let line_count_value_y = y + 10.0 + (row_h - TEXT_SIZE_VALUE_H) / 2.0;
        let value_label: *mut AnyObject = msg_send![value_label, initWithFrame: NSRect::new(NSPoint::new(ctrl_x + ctrl_w - 34.0, line_count_value_y), NSSize::new(30.0, TEXT_SIZE_VALUE_H))];
        set_field(value_label, 3);
        let _: () = msg_send![value_label, setBezeled: false];
        let _: () = msg_send![value_label, setDrawsBackground: false];
        let _: () = msg_send![value_label, setEditable: false];
        let _: () = msg_send![value_label, setUsesSingleLineMode: true];
        let _: () = msg_send![value_label, setAlignment: 1isize]; // NSTextAlignmentCenter
        let _: () = msg_send![mouse_view, addSubview: value_label];
        release_obj(value_label);
        ui.line_count_value_label = value_label;
        // 滑块拖动时实时刷新数值 label。
        // Refresh the value label live as the slider is dragged.
        let _: () = msg_send![ui.line_count, setTarget: target];
        let _: () = msg_send![ui.line_count, setAction: sel!(handleLineCountChanged:)];
        let device_card_parts = SettingsSection::attach(
            mouse_view,
            NSRect::new(
                NSPoint::new(6.0, layout.card_bottom(y)),
                NSSize::new(
                    content_w - 12.0,
                    layout.card_top(device_header_y) - layout.card_bottom(y),
                ),
            ),
            &t("settings.header_mouse_device"),
        );
        let device_card = device_card_parts.card;
        let device_shadow = device_card_parts.shadow;
        ui.line_count_card = device_card;
        ui.line_count_shadow = device_shadow;

        // --- 滚动 Scrolling ---
        y = layout.next_section_cursor(y);
        let scrolling_header_y = y;
        y = layout.next_row_cursor(y, described_row_h);
        // reverse_scroll 开关:标题+副标题描述滚动方向,开关保留右侧内边距。
        // reverse_scroll switch: title + subtitle describe the scroll inversion; the switch
        // keeps the reference page's trailing inset.
        ui.reverse_scroll = SettingsRow::described(
            mouse_view,
            label_x,
            y,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_reverse_scroll"),
            &t("settings.desc_reverse_scroll"),
            SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
        );
        SettingsSection::attach(
            mouse_view,
            NSRect::new(
                NSPoint::new(6.0, layout.card_bottom(y)),
                NSSize::new(
                    content_w - 12.0,
                    layout.card_top(scrolling_header_y) - layout.card_bottom(y),
                ),
            ),
            &t("settings.header_mouse_scrolling"),
        );

        // --- 指针 Pointer ---
        y = layout.next_section_cursor(y);
        let pointer_header_y = y;
        y = layout.next_row_cursor(y, described_row_h);
        // disable_pointer_accel 开关:禁用系统鼠标加速,光标 1:1 线性跟踪。
        // 副标题说明线性跟踪的用途;开关与所有开关行一样保留右侧内边距。
        // disable_pointer_accel switch: disable system pointer acceleration for 1:1 linear
        // cursor tracking. The subtitle explains linear tracking; the switch keeps the same
        // trailing inset as every other switch row.
        ui.disable_pointer_accel = SettingsRow::described(
            mouse_view,
            label_x,
            y,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_disable_pointer_accel"),
            &t("settings.desc_disable_pointer_accel"),
            SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
        );
        SettingsSection::attach(
            mouse_view,
            NSRect::new(
                NSPoint::new(6.0, layout.card_bottom(y)),
                NSSize::new(
                    content_w - 12.0,
                    layout.card_top(pointer_header_y) - layout.card_bottom(y),
                ),
            ),
            &t("settings.header_mouse_pointer"),
        );

        // --- 按键映射 Button Mappings ---
        // 绑定区:"Enable button mappings" 描述行 + 嵌套表格卡片(圆角子表格 + 添加按钮)。
        // Button mappings: an "Enable button mappings" described row + a nested table card
        // (rounded sub-table + the add-mapping button).
        y = layout.next_section_cursor(y);
        let mappings_header_y = y;
        // "Enable button mappings" 描述行(HTML 卡片顶部),替代原来放在区块标题右侧的开关。
        // "Enable button mappings" described row (HTML card top), replacing the old switch
        // that sat on the section-header row's right edge.
        y = layout.next_row_cursor(y, described_row_h);
        ui.mapping_enabled = SettingsRow::described(
            mouse_view,
            label_x,
            y,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_mapping_enable"),
            &t("settings.desc_mapping_enable"),
            SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
        );
        let _: () = msg_send![ui.mapping_enabled, setTarget: target];
        let _: () = msg_send![ui.mapping_enabled, setAction: sel!(handleMappingEnabledChanged:)];
        SettingsSection::attach(
            mouse_view,
            NSRect::new(
                NSPoint::new(6.0, layout.card_bottom(y)),
                NSSize::new(
                    content_w - 12.0,
                    layout.card_top(mappings_header_y) - layout.card_bottom(y),
                ),
            ),
            &t("settings.header_mouse_mappings"),
        );

        // --- 嵌套表格卡片(nested table card) ---
        y -= 24.0;
        let card_top = y;
        let card_w = content_w - 12.0;
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
        // Align the mapping card with the other settings cards; the nested table keeps its own
        // inset so only the outer border expands to the shared content width.
        // 按键映射外框与其他设置卡片共用左右边界,内部表格继续保留自己的内缩。
        let card_bg: *mut AnyObject = msg_send![card_bg, initWithFrame: NSRect::new(NSPoint::new(6.0, card_bottom), NSSize::new(content_w - 12.0, card_h))];
        let _: () = msg_send![card_bg, setFlipped: true];
        let _: () = msg_send![card_bg, setAutoresizingMask: 0u64];
        let _: () = msg_send![card_bg, setWantsLayer: true];
        let bg_layer: *mut AnyObject = msg_send![card_bg, layer];
        let _: () = msg_send![bg_layer, setCornerRadius: 14.0f64];
        let _: () = msg_send![bg_layer, setMasksToBounds: true];
        let palette = settings_palette();
        crate::ffi::layer_set_background(bg_layer, crate::ffi::hex_to_cg_color(palette.card_bg));
        crate::ffi::layer_set_border(bg_layer, crate::ffi::hex_to_cg_color(palette.card_border));
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
        crate::ffi::layer_set_background(
            panel_layer,
            crate::ffi::hex_to_cg_color(palette.field_bg),
        );
        crate::ffi::layer_set_border(
            panel_layer,
            crate::ffi::hex_to_cg_color(palette.card_border),
        );
        let _: () = msg_send![panel_layer, setBorderWidth: 1.0f64];
        let _: () = msg_send![card_bg, addSubview: panel];
        ui.mapping_panel = panel;
        release_obj(panel);
        // 表头带(.mapping-table thead)。
        // The header band (.mapping-table thead).
        let header_color = settings_text_color(SettingsTextRole::Secondary);
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
        let empty_color = settings_text_color(SettingsTextRole::Muted);
        let _: () = msg_send![empty, setTextColor: empty_color];
        let _: () = msg_send![empty, setHidden: true];
        let _: () = msg_send![card_bg, addSubview: empty];
        release_obj(empty);
        ui.mapping_empty = empty;
        // 添加按钮:卡片底部 action-row(全宽)。
        // Add-mapping button: full-width action row at the card bottom.
        let add_btn = SettingsButton::action(
            NSRect::new(
                NSPoint::new(
                    MAPPING_PANEL_X,
                    MAPPING_PANEL_TOP + MAPPING_HEADER_H + MAPPING_ROW_H * 3.0 + MAPPING_ACTION_TOP,
                ),
                NSSize::new(card_w - 2.0 * MAPPING_PANEL_X, MAPPING_ACTION_H),
            ),
            &t("settings.row_add_mapping"),
            target,
            sel!(handleAddMapping:),
            SettingsButtonRole::Compact,
        );
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
            6.0,
            cy - 34.0,
            content_w - 12.0,
        );
        cy -= 62.0;
        let clipboard_header_y = cy - 18.0;
        // header 与首行间距与其他页一致(8 + row_h = 30):此前 16pt 挨得太近。
        // Header-to-first-row gap matches the other pages (8 + row_h = 30); it used to be
        // 16pt, too cramped.
        cy = layout.next_row_cursor_with_extra(cy, described_row_h, 18.0);
        // 启用开关 / master switch.
        // 启用开关 / master switch.
        // 英文 "Enable clipboard history"(实测 146pt)+ cell 内边距在 label_w=150 边缘,
        // 与 persist/move_used_to_top 行一起加宽到 225(见下方注释)。
        // English "Enable clipboard history" (measured 146pt) plus cell padding sits on
        // the label_w=150 edge; widen to 225 along with the persist/move_used_to_top rows.
        let clipboard_master_row_y = cy;
        ui.clipboard_enabled = SettingsRow::described(
            clipboard_view,
            label_x,
            cy,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_clipboard_enabled"),
            &t("settings.desc_clipboard_enabled"),
            SettingsControl::switch(ctrl_x + ctrl_w, cy, row_h, false),
        );
        let _: () = msg_send![ui.clipboard_enabled, setTarget: target];
        let _: () = msg_send![
            ui.clipboard_enabled,
            setAction: sel!(handleClipboardEnabledToggle:)
        ];
        SettingsSection::attach(
            clipboard_view,
            NSRect::new(
                NSPoint::new(6.0, layout.card_bottom(clipboard_master_row_y)),
                NSSize::new(
                    content_w - 12.0,
                    layout.card_top(clipboard_header_y)
                        - layout.card_bottom(clipboard_master_row_y),
                ),
            ),
            &t("settings.header_clipboard"),
        );
        // Keep the history controls in a second titled card, matching the switcher layout.
        // 其余历史记录设置单独成卡,并与切换器页面使用相同的小标题间距。
        cy = layout.next_section_cursor(cy);
        let clipboard_options_header_y = cy;
        cy = layout.next_row_cursor(cy, described_row_h);
        // 置顶后选中项位置下拉框:项 = [跟随置顶, 保持当前位置];默认 index 0(跟随置顶),
        // 实际值由 load_settings_from 填充。
        // Pin-selection popup: items = [Follow the Pinned Entry, Keep Current Position];
        // default index 0 (follow); the real value is set by load_settings_from.
        let pin_labels = [
            t("settings.pin_follow_entry"),
            t("settings.pin_keep_position"),
        ];
        let pin_label_refs: Vec<&str> = pin_labels.iter().map(|s| s.as_str()).collect();
        ui.clipboard_pin_follow = SettingsRow::plain(
            clipboard_view,
            label_x,
            cy,
            220.0,
            described_row_h,
            &t("settings.row_clipboard_pin_follow"),
            SettingsControl::popup(ctrl_x, cy, ctrl_w, row_h, &pin_label_refs, 0),
        );
        cy = layout.next_row_cursor(cy, described_row_h);
        SettingsRow::separator(clipboard_view, cy + described_row_h + 3.0, content_w);
        // 保存历史开关(持久化到磁盘,重启不丢;明文落盘,隐私风险见 README)。
        // Persist switch (saved to disk, survives restarts; plaintext on disk -- the
        // privacy implications are documented in the README).
        // 保存历史开关(持久化到磁盘,重启不丢;明文落盘,隐私风险见 README)。
        // 中文标签"保存剪贴板历史记录到磁盘"(11 字)与英文 "Save clipboard history
        // to disk" 都超出默认 label_w=150(渲染截断),该行加宽到 225——与
        // show_minimized 行同款处理;开关保留右侧内边距,避免与边缘重叠。
        // Persist switch (saved to disk, survives restarts; plaintext on disk -- the
        // privacy implications are documented in the README). The Chinese (11 CJK
        // chars) and English labels both exceed the default label_w=150 (rendered
        // truncated), so this row widens its label to 225 -- same as the
        // show_minimized row; the switch keeps the trailing inset and stays clear of the edge.
        ui.clipboard_persist = SettingsRow::plain(
            clipboard_view,
            label_x,
            cy,
            220.0,
            described_row_h,
            &t("settings.row_clipboard_persist"),
            SettingsControl::switch(ctrl_x + ctrl_w, cy, row_h, false),
        );
        cy = layout.next_row_cursor(cy, described_row_h);
        SettingsRow::separator(clipboard_view, cy + described_row_h + 3.0, content_w);
        // 显示来源应用 / show the source app.
        ui.clipboard_show_source_app = SettingsRow::plain(
            clipboard_view,
            label_x,
            cy,
            label_w,
            described_row_h,
            &t("settings.row_clipboard_show_source_app"),
            SettingsControl::switch(ctrl_x + ctrl_w, cy, row_h, false),
        );
        cy = layout.next_row_cursor(cy, described_row_h);
        SettingsRow::separator(clipboard_view, cy + described_row_h + 3.0, content_w);
        // 使用后移到最前(粘贴是否重排历史;默认开 = 保持现状)。
        // Move used entries to the top (whether pasting reorders the history; on by
        // default = current behavior).
        // 英文 "Move used entries to top"(实测 150.3pt)超出 label_w=150 渲染截断
        // (用户切英文后看到 "move used entries to"),加宽到 225。
        // English "Move used entries to top" (measured 150.3pt) exceeds label_w=150 and
        // rendered truncated ("move used entries to" after switching to English), widened
        // to 225.
        ui.clipboard_move_used_to_top = SettingsRow::plain(
            clipboard_view,
            label_x,
            cy,
            220.0,
            described_row_h,
            &t("settings.row_clipboard_move_used_to_top"),
            SettingsControl::switch(ctrl_x + ctrl_w, cy, row_h, false),
        );
        cy = layout.next_row_cursor(cy, described_row_h);
        SettingsRow::separator(clipboard_view, cy + described_row_h + 3.0, content_w);
        // 最大条数(数字输入)/ max entries (number input).
        ui.clipboard_max_entries = SettingsRow::plain(
            clipboard_view,
            label_x,
            cy,
            label_w,
            described_row_h,
            &t("settings.row_clipboard_max_entries"),
            SettingsControl::text_input(ctrl_x, cy, ctrl_w, row_h, "50"),
        );
        cy = layout.next_row_cursor(cy, described_row_h);
        SettingsRow::separator(clipboard_view, cy + described_row_h + 3.0, content_w);
        // 自动过期天数(数字输入,0 = 关闭)/ auto-expire days (number input, 0 = off).
        ui.clipboard_auto_expire_days = SettingsRow::plain(
            clipboard_view,
            label_x,
            cy,
            label_w,
            described_row_h,
            &t("settings.row_clipboard_auto_expire_days"),
            SettingsControl::text_input(ctrl_x, cy, ctrl_w, row_h, "3"),
        );
        let clipboard_options_card_bottom = layout.card_bottom(cy);
        SettingsSection::attach(
            clipboard_view,
            NSRect::new(
                NSPoint::new(6.0, clipboard_options_card_bottom),
                NSSize::new(
                    content_w - 12.0,
                    layout.card_top(clipboard_options_header_y) - clipboard_options_card_bottom,
                ),
            ),
            &t("settings.header_clipboard_options"),
        );

        // ===== 窗口控制页内容 window control page content =====
        // 独立布局游标(该页内容与剪贴板页互不相关)。
        // Independent layout cursor (unrelated to the clipboard page).
        let mut wy = window_control_doc_h - 24.0;
        add_page_title(
            window_control_view,
            &t("settings.sidebar_window_control"),
            6.0,
            wy - 34.0,
            content_w - 12.0,
        );
        wy -= 62.0;
        let window_control_header_y = wy - 18.0;
        // header 与首行间距与剪贴板页一致(18 + row_gap)。
        // Header-to-first-row gap matches the clipboard page (18 + row_gap).
        wy = layout.next_row_cursor_with_extra(wy, described_row_h, 18.0);
        // 启用窗口控制(总开关):Option+方向键的全局拦截默认关闭,由用户显式开启。
        // Enable window control (master switch): the global Option+arrow interception is off
        // by default and must be explicitly opted in.
        ui.window_control_enabled = SettingsRow::described(
            window_control_view,
            label_x,
            wy,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_window_control_enabled"),
            &t("settings.desc_window_control_enabled"),
            SettingsControl::switch(ctrl_x + ctrl_w, wy, row_h, false),
        );
        let _: () = msg_send![ui.window_control_enabled, setTarget: target];
        let _: () = msg_send![
            ui.window_control_enabled,
            setAction: sel!(handleWindowControlEnabledToggle:)
        ];
        SettingsSection::attach(
            window_control_view,
            NSRect::new(
                NSPoint::new(6.0, layout.card_bottom(wy)),
                NSSize::new(
                    content_w - 12.0,
                    layout.card_top(window_control_header_y) - layout.card_bottom(wy),
                ),
            ),
            &t("settings.header_window_control"),
        );

        // 方向快捷键单独成一块卡片,总开关与具体方向配置互不混排。
        // Put the direction shortcuts in their own card so the master switch is separate from
        // the per-direction settings.
        wy = layout.next_section_cursor(wy);
        let window_control_shortcuts_header_y = wy;
        wy = layout.next_row_cursor(wy, described_row_h);
        ui.window_control_up = SettingsRow::described(
            window_control_view,
            label_x,
            wy,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_window_control_up"),
            &t("settings.desc_window_control_up"),
            SettingsControl::switch(ctrl_x + ctrl_w, wy, row_h, false),
        );
        wy = layout.next_row_cursor(wy, described_row_h);
        SettingsRow::separator(window_control_view, wy + described_row_h + 3.0, content_w);
        ui.window_control_down = SettingsRow::described(
            window_control_view,
            label_x,
            wy,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_window_control_down"),
            &t("settings.desc_window_control_down"),
            SettingsControl::switch(ctrl_x + ctrl_w, wy, row_h, false),
        );
        wy = layout.next_row_cursor(wy, described_row_h);
        SettingsRow::separator(window_control_view, wy + described_row_h + 3.0, content_w);
        ui.window_control_left = SettingsRow::described(
            window_control_view,
            label_x,
            wy,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_window_control_left"),
            &t("settings.desc_window_control_left"),
            SettingsControl::switch(ctrl_x + ctrl_w, wy, row_h, false),
        );
        wy = layout.next_row_cursor(wy, described_row_h);
        SettingsRow::separator(window_control_view, wy + described_row_h + 3.0, content_w);
        ui.window_control_right = SettingsRow::described(
            window_control_view,
            label_x,
            wy,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_window_control_right"),
            &t("settings.desc_window_control_right"),
            SettingsControl::switch(ctrl_x + ctrl_w, wy, row_h, false),
        );
        let window_control_shortcuts_card_bottom = layout.card_bottom(wy);
        SettingsSection::attach(
            window_control_view,
            NSRect::new(
                NSPoint::new(6.0, window_control_shortcuts_card_bottom),
                NSSize::new(
                    content_w - 12.0,
                    layout.card_top(window_control_shortcuts_header_y)
                        - window_control_shortcuts_card_bottom,
                ),
            ),
            &t("settings.header_window_control_shortcuts"),
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
        let about_subtitle_color = settings_text_color(SettingsTextRole::Muted);
        let _: () = msg_send![about_subtitle, setTextColor: about_subtitle_color];
        let _: () = msg_send![about_view, addSubview: about_subtitle];
        release_obj(about_subtitle);
        ui.about_subtitle = about_subtitle;

        // Transparent hit area for the five-click build-version easter egg. It is added after
        // the labels so it receives clicks across the whole header without changing its visuals.
        // 透明点击区域用于五击显示 build-version 的彩蛋。放在文字之后，覆盖整个头部但不改变外观。
        let about_header_hit: *mut AnyObject = msg_send![about_header_click_view_class(), alloc];
        let about_header_hit: *mut AnyObject = msg_send![
            about_header_hit,
            initWithFrame: NSRect::new(
                NSPoint::new(6.0, header_top - 64.0),
                NSSize::new(content_w - 12.0, 66.0),
            )
        ];
        let _: () = msg_send![about_view, addSubview: about_header_hit];
        release_obj(about_header_hit);

        let mut ay = header_top - 88.0;
        // Keep the App section title close to its card, matching the spacing used by the
        // other settings pages. The About card has three rows, so its content cursor is lower
        // than a normal section header; placing the title at the old cursor left a large void.
        // 让 App 分组标题贴近下方卡片,与其他设置页保持一致。About 卡片有三行内容,其内容
        // 游标比普通区块标题更低;沿用旧游标会在标题和卡片之间留下过大的空白。
        let app_label_y = ay - 35.0;
        ay -= 27.0;
        let about_row_step = layout.row_gap + described_row_h;
        let website_y = ay - about_row_step;
        // Keep every About row on the same two-column grid: label on the left, value on the right.
        // About 页面所有行统一使用两列网格：左侧标签，右侧值。
        let about_value_x = label_x + 145.0;
        let about_value_w = (content_w - 2.0 * label_x - 145.0).max(1.0);
        SettingsRow::plain(
            about_view,
            label_x,
            website_y,
            label_w,
            described_row_h,
            &t("settings.website_label"),
            SettingsControl::external_link(
                about_value_x,
                website_y,
                about_value_w,
                row_h,
                &t("settings.website_url"),
                0,
            ),
        );
        let github_y = website_y - about_row_step;
        SettingsRow::separator(about_view, github_y + described_row_h + 3.0, content_w);
        SettingsRow::plain(
            about_view,
            label_x,
            github_y,
            label_w,
            described_row_h,
            &t("settings.github_label"),
            SettingsControl::external_link(
                about_value_x,
                github_y,
                about_value_w,
                row_h,
                &t("settings.github_url"),
                1,
            ),
        );
        let version_y = github_y - about_row_step;
        SettingsRow::separator(about_view, version_y + described_row_h + 3.0, content_w);
        SettingsRow::plain(
            about_view,
            label_x,
            version_y,
            label_w,
            described_row_h,
            &t("settings.version_label_short"),
            SettingsControl::value_label(
                about_value_x,
                version_y,
                120.0,
                row_h,
                env!("CARGO_PKG_VERSION"),
            ),
        );
        let app_card_bottom = layout.card_bottom(version_y);
        SettingsSection::attach(
            about_view,
            NSRect::new(
                NSPoint::new(6.0, app_card_bottom),
                NSSize::new(
                    content_w - 12.0,
                    layout.card_top(app_label_y) - app_card_bottom,
                ),
            ),
            &t("settings.section_app"),
        );

        ay = version_y - 42.0;
        let updates_label_y = ay - 11.0;
        ay -= 27.0;
        let update_row_y = ay - 44.0;
        ui.update_auto_check = SettingsRow::described(
            about_view,
            label_x,
            update_row_y,
            (ctrl_x + ctrl_w) - label_x - 70.0,
            described_row_h,
            &t("settings.row_update_auto_check"),
            &t("settings.desc_update_auto_check"),
            SettingsControl::switch(ctrl_x + ctrl_w, update_row_y + 10.0, row_h, false),
        );
        // 自动下载并安装更新开关,位于「自动检查更新」与「检查更新」之间。
        // Automatically-download-and-install switch, between auto-check and the check button.
        let download_row_y = update_row_y - described_row_h;
        ui.update_auto_download = SettingsRow::described(
            about_view,
            label_x,
            download_row_y,
            (ctrl_x + ctrl_w) - label_x - 70.0,
            described_row_h,
            &t("settings.row_update_auto_download"),
            &t("settings.desc_update_auto_download"),
            SettingsControl::switch(ctrl_x + ctrl_w, download_row_y + 10.0, row_h, false),
        );
        // Keep the two update toggles visually grouped with the same inset divider used by other
        // multi-row cards. The rows are contiguous here, so the divider sits at their shared edge.
        // 两个更新开关属于同一张多行卡片，复用其他卡片的内缩分割线；两行相邻，分割线放在共享边界。
        SettingsRow::separator(about_view, update_row_y, content_w);
        // 检查更新:全宽长按钮,标题随流程在「检查更新…/检查中…/已是最新版本」间切换,尺寸不变。
        // Check for updates: a full-width button whose title switches between "Check for Updates…",
        // "Checking…", and "You're up to date" without changing size.
        let check_button = SettingsButton::action(
            NSRect::new(
                NSPoint::new(label_x, update_row_y - described_row_h - 46.0),
                NSSize::new(content_w - 2.0 * label_x, 32.0),
            ),
            &t("settings.btn_check_for_updates"),
            target,
            sel!(handleCheckForUpdates:),
            SettingsButtonRole::Action,
        );
        let _: () = msg_send![check_button, setTag: -3isize];
        let check_layer: *mut AnyObject = msg_send![check_button, layer];
        if !check_layer.is_null() {
            layer_set_background(
                check_layer,
                crate::ffi::hex_to_cg_color(settings_palette().button_bg),
            );
        }
        let _: () = msg_send![about_view, addSubview: check_button];
        ui.update_check_button = check_button;
        release_obj(check_button);
        // 内联更新流程的宿主容器:更新状态/进度/按钮渲染进这个 NSView,不再弹独立窗口。
        // Inline update-flow host container: update status/progress/buttons render here instead of
        // a separate NSWindow. Empty and hidden by default, so the About page stays compact; an
        // active flow expands the card + host via expand_update_section.
        // 宿主直接占用「检查更新」按钮的 frame(update_row_y - described_row_h - 46),内容用顶向下
        // 坐标排布,更新状态会替换按钮而不是追加在按钮下方。初始高度为 0,故 origin.y 即顶边。
        // The host occupies the check-button frame (update_row_y - described_row_h - 46); its
        // top-down content replaces the button instead of being appended below it. With an initial
        // height of 0, origin.y is the top.
        let compact_host_h = 0.0;
        let host_origin_y = update_row_y - described_row_h - 46.0;
        let update_host: *mut AnyObject = msg_send![class!(NSView), alloc];
        let update_host: *mut AnyObject = msg_send![
            update_host,
            initWithFrame: NSRect::new(
                NSPoint::new(label_x, host_origin_y),
                NSSize::new(content_w - 2.0 * label_x, compact_host_h),
            )
        ];
        let _: () = msg_send![update_host, setHidden: true];
        let _: () = msg_send![update_host, setFlipped: true]; // 子视图 y 自顶向下 / child y origin is top-down
        let _: () = msg_send![about_view, addSubview: update_host];
        release_obj(update_host);
        ui.update_host = update_host;
        ui.update_host_origin_y = host_origin_y;
        ui.update_host_window = window;
        crate::updater::set_update_host(update_host, window, check_button);
        // 收起时的卡片下沿紧贴「检查更新」按钮下方(update_row_y - described_row_h - 56),
        // 默认为内联区域预留,避免大块空白。
        // Collapsed card bottom hugs the check button below (update_row_y - described_row_h - 56);
        // the inline area is not reserved by default, avoiding a large blank.
        let compact_card_bottom = update_row_y - described_row_h - 56.0;
        let update_card_parts = SettingsSection::attach(
            about_view,
            NSRect::new(
                NSPoint::new(6.0, compact_card_bottom),
                NSSize::new(
                    content_w - 12.0,
                    layout.card_top(updates_label_y) - compact_card_bottom,
                ),
            ),
            &t("settings.section_updates"),
        );
        let update_card = update_card_parts.card;
        let update_card_shadow = update_card_parts.shadow;
        ui.update_card = update_card;
        ui.update_card_shadow = update_card_shadow;
        ui.update_card_compact_h = {
            let compact_frame: NSRect = msg_send![update_card, frame];
            compact_frame.size.height
        };

        // banner 最后添加:作为 general_view 的最后一个 subview,保证在内容之上(缺权限时覆盖顶部)。
        // Added last: as general_view's final subview so it floats above the content (when
        // permission is missing). It occupies no layout space, so no top gap when hidden.
        let _: () = msg_send![general_view, addSubview: banner];
        release_obj(banner);

        // Let AppKit finish its first layout pass before validating the actual view tree. Do not
        // shrink the provisional documents here: their children are top-anchored with
        // autoresizing masks, and post-hoc height fitting can move them a second time.
        // 先让 AppKit 完成首次布局，再校验真实 view tree。这里不收缩临时 document 高度：子视图
        // 使用顶部锚定 autoresizing，布局后再改高度会触发第二次位移。
        let _: () = msg_send![window, layoutIfNeeded];
        for (name, page) in [
            (
                "general",
                SettingsPage {
                    scroll: general_root,
                    document: general_view,
                },
            ),
            (
                "switcher",
                SettingsPage {
                    scroll: switcher_root,
                    document: switcher_view,
                },
            ),
            (
                "mouse",
                SettingsPage {
                    scroll: mouse_root,
                    document: mouse_view,
                },
            ),
            (
                "clipboard",
                SettingsPage {
                    scroll: clipboard_root,
                    document: clipboard_view,
                },
            ),
            (
                "about",
                SettingsPage {
                    scroll: about_root,
                    document: about_view,
                },
            ),
        ] {
            page.validate(name);
        }
        let update_host_frame: NSRect = msg_send![ui.update_host, frame];
        ui.update_host_origin_y = update_host_frame.origin.y;

        // --- 确认 / 取消(右侧 footer 内,所有页面都可见)---
        // Cancel and OK are children of the detail pane's footer, matching the HTML layout.
        let cancel = SettingsButton::action(
            NSRect::new(
                NSPoint::new(content_x + detail_w - 202.0, 14.0),
                NSSize::new(86.0, 32.0),
            ),
            &t("settings.btn_cancel"),
            target,
            sel!(handleSettingsCancel:),
            SettingsButtonRole::Footer,
        );
        let _: () = msg_send![cancel, setTag: -1isize];
        let cancel_layer: *mut AnyObject = msg_send![cancel, layer];
        if !cancel_layer.is_null() {
            // HTML footer buttons use a slightly more opaque white surface than small buttons.
            layer_set_background(
                cancel_layer,
                crate::ffi::hex_to_cg_color(settings_palette().footer_button_bg),
            );
        }
        let _: () = msg_send![cancel, setAutoresizingMask: 33u64]; // 贴底、贴右 / bottom- and right-anchored
        let _: () = msg_send![content, addSubview: cancel];
        release_obj(cancel);

        let ok = SettingsButton::action(
            NSRect::new(
                NSPoint::new(content_x + detail_w - 106.0, 14.0),
                NSSize::new(86.0, 32.0),
            ),
            &t("settings.btn_ok"),
            target,
            sel!(handleSettingsOk:),
            SettingsButtonRole::Primary,
        );
        let _: () = msg_send![ok, setTag: -2isize];
        let ok_layer: *mut AnyObject = msg_send![ok, layer];
        if !ok_layer.is_null() {
            // HTML `.footer .ok`: #0a84ff with a subtle rgba(0,0,0,.04) border.
            // HTML `.footer .ok`: 使用 #0a84ff 和轻微的 rgba(0,0,0,.04) 边框。
            layer_set_background(ok_layer, crate::ffi::hex_to_cg_color(0x0A84FFFF));
            crate::ffi::layer_set_border(ok_layer, crate::ffi::hex_to_cg_color(0x0000000A));
            let _: () = msg_send![ok_layer, setBorderWidth: 1.0f64];
            let _: () = msg_send![ok_layer, setCornerRadius: 8.0f64];
        }
        let ok_text = settings_text_color(SettingsTextRole::OnAccent);
        let _: () = msg_send![ok, setContentTintColor: ok_text];
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
    SYSTEM_APPEARANCE_REBUILD_PENDING.store(false, Ordering::SeqCst);
    let ui = SETTINGS_UI.lock().unwrap().take();
    if let Some(u) = ui {
        unsafe {
            close_glass_tint_panel(u.glass_tint);
            TRAFFIC_LIGHT_BASE_ORIGINS
                .lock()
                .unwrap()
                .remove(&(u.window as usize));
            // 先让 update 模块解除对宿主视图的引用,再释放窗口,避免它写入已释放视图。
            // Detach the updater's host references before releasing the window so it never touches
            // a deallocated view.
            crate::updater::clear_update_host();
            SettingsRow::clear_runtime_registry();
            widgets::clear_settings_select_registry();
            tooltip::SettingsTooltip::clear_runtime_registries();
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

    /// Exercise the real settings window on the AppKit main thread. This cannot run in the
    /// normal headless suite because NSWindow construction is GUI/session dependent.
    /// 在真实 AppKit 主线程中构建并遍历设置窗口；依赖 GUI 会话，因此不进入普通无头测试。
    #[test]
    #[ignore]
    fn settings_layout_smoke() {
        let exe = std::env::current_exe().expect("current exe");
        let app = exe
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.join("oh-my-tab"))
            .expect("app binary path");
        assert!(
            app.exists(),
            "app binary missing at {}: run `cargo build` first",
            app.display()
        );
        let out = std::process::Command::new(&app)
            .arg("--smoke-settings-layout")
            .output()
            .expect("failed to spawn app");
        assert!(
            out.status.success(),
            "settings layout smoke failed (exit {:?})\nstderr:\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
