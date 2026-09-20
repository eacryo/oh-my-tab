//! 设置窗口:SettingsUi 状态、控件构造器(text/popup/header/row)、窗口构建/显示、
//! 即时生效的字段调度器(所有修改实时写内存 CONFIG + 防抖落盘)、以及恢复默认
//! (整页/整应用)。invalidate_settings_window 作废缓存窗口供 locale/主题变更后重建。
//!
//! Settings window: SettingsUi state, control builders (text/popup/header/row), window
//! build/show, the live-apply control dispatcher (every change writes the in-memory CONFIG
//! immediately + persists to disk with a debounce), and restore-defaults (per page / whole
//! app). invalidate_settings_window drops the cached window so it rebuilds after a locale or
//! theme change.

use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSEdgeInsets, NSPoint, NSRect, NSSize};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{c_void, CString};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::config::{persist_config_now, schedule_config_persist, Config, CONFIG};
use crate::event_monitor::SHORTCUT_IS_CMD;
use crate::event_tap::{
    CGEventGetFlags, CGEventGetIntegerValueField, CGEventRef, CGEventTapProxy, CGEventType,
};
use crate::ffi::*;
use crate::i18n::{t, tf};
use crate::mouse::shortcut::{button_name, describe_shortcut, display_shortcut};
use crate::runtime_config::{apply_config_change, ConfigChangeSource};
use crate::theme::{resolved_is_dark, ui_palette, UiPalette};
use crate::{log_debug, log_info};
// 跨模块共享状态(由 main.rs 持有)/ cross-module shared state (owned by main.rs)
use crate::MENU_TARGET;

// locale 下拉项:显示用各语言原生写法(语言选择器的通用约定),值对应 config.i18n.locale。
// debug 构建额外提供长英语夹具,生产构建完全不包含该项。
// Locale popup items are displayed in each language's own script (the usual language-picker
// convention); debug builds add a long-English fixture, while production builds omit it.
#[cfg(any(debug_assertions, feature = "dev-long-text"))]
const LOCALE_LABELS: [&str; 5] = [
    "Auto",
    "English",
    "简体中文",
    "繁體中文",
    "[TEST] English x3",
];
#[cfg(not(any(debug_assertions, feature = "dev-long-text")))]
const LOCALE_LABELS: [&str; 4] = ["Auto", "English", "简体中文", "繁體中文"];
const SCROLL_MODE_LABELS: [&str; 2] = ["Default", "Line"];
const SCROLL_MODE_VALUES: [&str; 2] = ["default", "line"];
#[cfg(any(debug_assertions, feature = "dev-long-text"))]
const LOCALE_VALUES: [&str; 5] = [
    "auto",
    "en",
    "zh-Hans",
    "zh-Hant",
    crate::i18n::TEST_LONG_LOCALE,
];
#[cfg(not(any(debug_assertions, feature = "dev-long-text")))]
const LOCALE_VALUES: [&str; 4] = ["auto", "en", "zh-Hans", "zh-Hant"];
const TEXT_SIZE_MIN: i64 = 13;
const TEXT_SIZE_MAX: i64 = 20;
const TEXT_SIZE_DEFAULT: i64 = 15;
const CLIPBOARD_AUTO_EXPIRE_MIN: i64 = 0;
const CLIPBOARD_AUTO_EXPIRE_MAX: i64 = 7;
const CLIPBOARD_AUTO_EXPIRE_DEFAULT: i64 = 3;

/// Fixed width of the settings navigation pane, shared by layout and transient feedback.
/// 设置导航栏固定宽度，供页面布局和临时反馈提示共用。
pub(crate) const SETTINGS_SIDEBAR_WIDTH: f64 = 220.0;

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
static EDIT_PANEL: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
static EDIT_PANEL_BTN_LABEL: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
static EDIT_PANEL_ACTION: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
static EDIT_PANEL_COMBO_BTN: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
static EDIT_PANEL_COMBO_LABEL: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
static EDIT_PANEL_OK: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
/// 面板打开时的窗口遮罩(半透明灰层,modal 调暗设置窗口)。
/// The window dim layer while the panel is open (a translucent gray overlay that dims the
/// settings window, modal-style).
static EDIT_DIM: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);

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
pub(super) struct SettingsUi {
    window: *mut AnyObject,
    sidebar_general: *mut AnyObject, // NSButton: 通用 / General (tag=0)
    sidebar_switcher: *mut AnyObject, // NSButton: 应用切换浮窗 / App switcher overlay (tag=1)
    sidebar_mouse: *mut AnyObject,   // NSButton: 鼠标控制 / Mouse (tag=2)
    sidebar_clipboard: *mut AnyObject, // NSButton: 剪贴板历史 / Clipboard history (tag=3)
    sidebar_window_control: *mut AnyObject, // NSButton: 窗口控制 / Window control (tag=4)
    sidebar_quick_actions: *mut AnyObject, // NSButton: 快捷操作 / Quick actions (tag=5)
    sidebar_about: *mut AnyObject,   // NSButton: 关于 / About (tag=6)
    sidebar_highlight: *mut AnyObject, // NSView: 选中行高亮背景 (layer-backed)
    general_view: *mut AnyObject,    // NSView: 通用页容器 / General page container
    switcher_view: *mut AnyObject,   // NSView: 应用切换浮窗页容器 / App switcher page container
    mouse_view: *mut AnyObject,      // NSView: 鼠标页容器 / Mouse page container
    clipboard_view: *mut AnyObject,  // NSView: 剪贴板历史页容器 / Clipboard page container
    window_control_view: *mut AnyObject, // NSView: 窗口控制页容器 / Window-control page container
    quick_actions_view: *mut AnyObject, // NSView: 快捷操作页容器 / Quick-actions page container
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
    focused_thumbnail_prewarm: *mut AnyObject, // NSSwitch: 前台窗口缩略图后台预热 / focused thumbnail prewarm
    show_app_name_in_cards: *mut AnyObject, // NSSwitch: 卡片标题显示应用名 / app name in card titles
    card_text_size: *mut AnyObject,         // NSSlider: 卡片文字大小 / card text size
    card_text_size_value_label: *mut AnyObject, // NSTextField: 卡片字号值 / card text-size value
    status_bar_text_size: *mut AnyObject,   // NSSlider: 底部标题栏文字大小 / footer text size
    status_bar_text_size_value_label: *mut AnyObject, // NSTextField: 底部字号值 / footer text-size value
    windows_enabled: *mut AnyObject, // NSSwitch: 窗口切换总开关 / app-switcher master switch
    overlay_position: *mut AnyObject, // NSPopUpButton: 跟随激活窗口 / 主屏幕 / overlay position (follow active window / main screen)
    activation_mode: *mut AnyObject, // NSPopUpButton: 悬停激活 / 点击激活 / activation mode (hover / click)
    log_level: *mut AnyObject,       // NSPopUpButton: trace / debug / info / warn / error
    launch_at_login: *mut AnyObject, // NSSwitch: 开机自启 / launch at login
    reverse_scroll: *mut AnyObject,  // NSSwitch: 反转滚动 / reverse scrolling
    enable_mouse: *mut AnyObject,    // NSSwitch: 启用鼠标控制 / enable mouse control
    scroll_mode: *mut AnyObject,     // NSPopUpButton: default/line
    line_count: *mut AnyObject,      // NSSlider: line count slider
    line_count_label: *mut AnyObject, // NSTextField: line count row 的 label / the row's label
    line_count_value_label: *mut AnyObject, // NSTextField: 滑块当前值(只读)/ slider's current value (read-only)
    // 行数行是条件行:整块显隐由 CollapsibleRows 负责(卡片/阴影/分割线都在里面)。
    // The line-count row is conditional: CollapsibleRows owns the show/hide (card, shadow, and
    // divider included).
    line_count_block: CollapsibleRows,
    disable_pointer_accel: *mut AnyObject, // NSSwitch: 禁用指针加速 / disable pointer acceleration
    pointer_accel_slider: *mut AnyObject,  // NSSlider: 跟踪速度 0..=40
    pointer_accel_label: *mut AnyObject,   // NSTextField: 该行标题 / the row's label
    pointer_accel_value_label: *mut AnyObject, // NSTextField: 滑块当前值(只读)/ slider's current value (read-only)
    // 跟踪速度行是条件行:同上,整块显隐由组件负责。
    // The tracking-speed row is conditional: same deal, the component owns show/hide.
    pointer_accel_block: CollapsibleRows,
    // 仅"图标和缩略图"模式才相关的两行(前台预热 + 缩略图上的应用名),整块显隐。
    // The two rows that only apply in icons-and-thumbnails mode (focused prewarm + app name on
    // thumbnails), shown/hidden as one block.
    thumbnail_only_block: CollapsibleRows,
    // ---- 按键映射区 / button-mappings section ----
    mapping_scroll: *mut AnyObject, // NSScrollView: 绑定列表滚动容器 / the bindings scroll view
    mapping_doc: *mut AnyObject,    // NSView: 滚动容器里的 document view(行堆叠处)/ document view
    mapping_card: *mut AnyObject, // NSVisualEffectView: 按键映射外层卡片 / the mappings outer card
    mapping_panel: *mut AnyObject, // NSView: 嵌套圆角表格面板 / the nested rounded table panel
    mapping_rows: Vec<MappingRow>, // 动态绑定行(标签 + 删除按钮)/ live binding rows
    clipboard_enabled: *mut AnyObject, // NSSwitch: 启用剪贴板历史 / enable clipboard history
    clipboard_persist: *mut AnyObject, // NSSwitch: 保存剪贴板历史记录到磁盘 / persist clipboard history
    clipboard_move_used_to_top: *mut AnyObject, // NSSwitch: 使用后移到最前 / move used entries to top
    clipboard_delete_after_paste: *mut AnyObject, // NSSwitch: 粘贴后删除条目 / delete entry after paste
    clipboard_clear_system_pasteboard_after_paste: *mut AnyObject, // NSSwitch: 粘贴后清空系统剪贴板 / clear system pasteboard after paste
    // "同时删除系统剪贴板中对应条目"是"粘贴后删除条目"的子项:只有后者打开时才出现。
    // "Clear the matching system-pasteboard entry" is a child of "delete entry after paste": it
    // only appears while the latter is on.
    clipboard_delete_block: CollapsibleRows,
    clipboard_max_entries: *mut AnyObject, // NSTextField: 历史最大条数 / max history entries
    clipboard_auto_expire_days: *mut AnyObject, // NSSlider: 自动过期天数(0=永不过期)/ auto-expire days (0 = never)
    clipboard_auto_expire_days_value_label: *mut AnyObject, // NSTextField: 自动过期值 / auto-expire value
    clipboard_show_source_app: *mut AnyObject, // NSSwitch: 显示来源应用 / show the source app
    clipboard_pin_follow: *mut AnyObject, // NSPopUpButton: 置顶后选中项位置 / selection after pin
    // (follow the pinned entry / keep current position)
    window_control_enabled: *mut AnyObject, // NSSwitch: 启用窗口控制 / enable window control
    window_control_up: *mut AnyObject,      // NSSwitch: 启用 Option+上 / enable Option+Up
    window_control_down: *mut AnyObject,    // NSSwitch: 启用 Option+下 / enable Option+Down
    window_control_left: *mut AnyObject,    // NSSwitch: 启用 Option+左 / enable Option+Left
    window_control_right: *mut AnyObject,   // NSSwitch: 启用 Option+右 / enable Option+Right
    window_control_display_up: *mut AnyObject, // NSSwitch: Option+Shift+上移显示器 / move to upper display
    window_control_display_down: *mut AnyObject, // NSSwitch: Option+Shift+下移显示器 / move to lower display
    window_control_display_left: *mut AnyObject, // NSSwitch: Option+Shift+左移显示器 / move to left display
    window_control_display_right: *mut AnyObject, // NSSwitch: Option+Shift+右移显示器 / move to right display
    quick_actions_enabled: *mut AnyObject,        // NSSwitch: 启用快捷操作 / enable quick actions
    quick_actions_open_settings: *mut AnyObject,  // NSSwitch: Option+I 打开设置 / open settings
    quick_actions_open_finder: *mut AnyObject,    // NSSwitch: Option+E 打开访达 / open Finder
    quick_actions_show_desktop: *mut AnyObject,   // NSSwitch: Option+D 显示桌面 / show desktop
    quick_actions_lock_screen: *mut AnyObject,    // NSSwitch: Option+L 锁屏 / lock screen
    quick_actions_locate_pointer: *mut AnyObject, // NSSwitch: 双击 Control 显示鼠标位置 / double-Control pointer locator
    add_mapping_button: *mut AnyObject,           // NSButton: 添加映射 / add-mapping button
    mapping_enabled: *mut AnyObject, // NSSwitch: 按键映射总开关(per-device) / mappings master switch (per-device)
    mapping_empty: *mut AnyObject,   // NSTextField: 空状态提示(卡片内) / empty-state hint (in-card)
    device_indicator: *mut AnyObject, // NSButton: 当前选中设备指示器(点击打开选择器) / device indicator (opens picker)
    restore_defaults: RestoreDefaultsControl, // 左下角恢复默认组件 / restore-defaults control
    // 每页一个「恢复本页默认设置」控件(内嵌各页文档内容末尾,随滚动)。
    // One "Restore Page Defaults" control per page (embedded at the end of each page's
    // scrolling document).
    page_restores: [RestoreDefaultsControl; 7],
    permission_warning_view: *mut AnyObject, // NSView: 缺权限警告条容器 / permission-warning banner container
    update_auto_check: *mut AnyObject, // NSSwitch: Sparkle 自动检查开关 / Sparkle auto-check switch
    update_auto_download: *mut AnyObject, // NSSwitch: Sparkle 自动下载开关 / Sparkle auto-download switch
    update_check_button: *mut AnyObject, // NSButton: 检查更新按钮(状态随流程变化) / check-updates button
    update_host: *mut AnyObject, // NSView: About 页内更新流程宿主容器 / In-about update flow host container
    update_host_window: *mut AnyObject, // NSWindow: 宿主所属设置窗口(供更新聚焦拉起) / host's settings window
    update_card: *mut AnyObject, // NSView: Updates 卡片(展开时撑高) / Updates card (grows when expanded)
    update_card_shadow: *mut AnyObject, // NSView: Updates 卡片阴影 / Updates card shadow
    update_divider: *mut AnyObject, // NSView: 更新设置与结果之间的分割线 / divider between update settings and result
    update_card_compact_h: f64,     // 收起时卡片高度 / collapsed card height
    update_card_expanded: bool,     // 是否已为更新流程展开 / whether expanded for a flow
    update_host_origin_y: f64, // 宿主收起时的原点 y(顶边 - 展开高) / host origin y when collapsed
}

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

impl MappingRow {
    /// 该行参与「可用性 / 禁用提示」注册的全部 view(action_icon 在 Key Press 行上为 null)。
    /// 注册与注销共用这一份清单,防止两侧漂移后在注册表里留下悬垂地址(见 SettingsRow::forget)。
    ///
    /// Every view of this row that takes part in the availability / disabled-hint registry
    /// (action_icon is null on Key Press rows). Registration and unregistration share this list
    /// so the two sides cannot drift apart and leave a dangling registry entry behind
    /// (see SettingsRow::forget).
    fn interactive_views(&self) -> impl Iterator<Item = *mut AnyObject> + '_ {
        [
            self.label,
            self.desc_label,
            self.action_icon,
            self.edit,
            self.delete,
        ]
        .into_iter()
        .chain(self.caps.iter().copied())
    }
}

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
thread_local! {
    static SETTINGS_UI: RefCell<Option<SettingsUi>> = const { RefCell::new(None) };
}

pub(super) fn with_settings_ui<R>(f: impl FnOnce(&mut Option<SettingsUi>) -> R) -> R {
    #[cfg(not(test))]
    crate::debug_assert_main_thread();
    SETTINGS_UI.with(|ui| f(&mut ui.borrow_mut()))
}
/// Whether a background update check found a version the user has not opened yet.
/// 后台检查是否发现了用户尚未打开查看的新版本。
static UPDATE_AVAILABLE: AtomicBool = AtomicBool::new(false);
/// Whether an editable settings text field currently owns keyboard input.
/// 设置窗口中是否有可编辑文本框当前持有键盘输入。
static TEXT_INPUT_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Update the About-tab marker and retain the state until the settings window is created.
/// 更新 About 标签标记；设置窗口尚未创建时保留状态，待创建后再显示。
pub(crate) fn set_update_available(available: bool) {
    UPDATE_AVAILABLE.store(available, Ordering::SeqCst);
    unsafe {
        with_settings_ui(|ui| {
            if let Some(ui) = ui.as_ref() {
                widgets::set_sidebar_update_indicator(ui.sidebar_about, available);
            }
        });
    }
}

/// Update the cross-thread text-input hint used by the quick-action event tap.
/// 更新快捷操作事件 tap 跨线程读取的文本输入状态提示。
pub(crate) fn set_text_input_active(active: bool) {
    TEXT_INPUT_ACTIVE.store(active, Ordering::Release);
}

/// Read whether the settings window currently has an editable text input.
/// 读取设置窗口当前是否有可编辑文本输入。
pub(crate) fn is_text_input_active() -> bool {
    TEXT_INPUT_ACTIVE.load(Ordering::Acquire)
}

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
mod dispatch;
pub(crate) mod glass_preview;
pub(crate) mod mapping;
pub(crate) mod restore;
pub(crate) mod tooltip;
pub(crate) mod widgets;
mod window;

use components::{
    CollapsibleRows, RestoreDefaultsControl, SettingsButton, SettingsButtonRole, SettingsControl,
    SettingsLayout, SettingsMappingActionIcon, SettingsPage, SettingsPageHeader, SettingsRow,
    SettingsSection, SettingsSelect, SettingsSidebar,
};
use dispatch::*;
use glass_preview::*;
pub(crate) use glass_preview::{
    apply_glass_preview, on_glass_tint_changed, on_glass_tint_panel_changed,
    on_glass_tint_panel_will_close, on_glass_tint_reset,
};
use mapping::*;
pub(crate) use mapping::{
    cancel_recording_from_main, handle_add_mapping, handle_delete_mapping, handle_mapping_cancel,
    handle_mapping_confirm, handle_mapping_edit, handle_mapping_enabled_changed,
    handle_panel_action_changed, handle_panel_record_combo, handle_panel_record_trigger,
    handle_recording_cancelled, handle_recording_finished,
};
pub(super) use restore::{
    collapse_restore_confirmations, collapse_restore_confirmations_on_external_click,
};
pub(crate) use restore::{
    confirm_alert, handle_page_restore_defaults, handle_page_restore_defaults_cancel,
    handle_page_restore_defaults_confirm, handle_restore_defaults, handle_restore_defaults_cancel,
    handle_restore_defaults_confirm,
};
use widgets::*;
use window::*;
// 对 crate 其他模块暴露的入口(内部子模块实现;main.rs 经 use settings::* 使用)。
// Entry points exposed to the rest of the crate (implemented in child modules; main.rs
// consumes them through use settings::*).
pub(crate) use dispatch::{
    handle_clipboard_enabled_toggle, handle_device_changed, handle_enable_mouse_toggle,
    handle_export_logs, handle_quick_actions_enabled_toggle, handle_window_control_enabled_toggle,
    handle_windows_enabled_toggle, on_control_changed, on_control_text_did_change,
    on_control_text_did_end_editing, on_sidebar_select, refresh_device_popup_if_open,
    refresh_service_controls_from_config, refresh_switcher_controls_from_config,
};
pub(crate) use window::{
    close_settings_from_switcher, invalidate_settings_window, refresh_system_appearance,
    settings_layout_smoke_runner,
};

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

// ========== 鼠标 profile 读写 helper / mouse profile read/write helpers ==========

use crate::config::{DeviceMatcher, MouseProfile};

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
    // 匹配规则与运行时解析共用一份(mouse::resolve::matches):虚拟指针档按"注入"匹配,
    // 普通档按 VID/PID 匹配,通配档两个都不设。
    // The matching rule is shared with runtime resolution (mouse::resolve::matches): the
    // virtual-pointer profile matches by "injected", ordinary ones by VID/PID, the wildcard by
    // neither.
    cfg.mouse
        .profiles
        .iter()
        .position(|p| crate::mouse::resolve::matches(p, device))
}

/// 取选中设备的专属档索引,没有就按当前设备新建一个(鼠标页字段写入、按键映射提交、恢复
/// 默认三处共用)。
///
/// 这三处原本各自复制了一段"无档则按 (VID,PID) 建一个"的代码;新增虚拟指针档时那样的写法
/// 要改三遍,且很容易漏掉一处 —— 匹配仍走 resolve::matches(与运行时解析同源),创建规则
/// 也只有这一份。
///
/// The selected device's own profile index, creating one for the current device when absent
/// (shared by the mouse-page field writes, mapping commits and restore-defaults). Those three
/// call sites each carried a copy of the "create by (VID,PID) when missing" block; with the
/// virtual-pointer profile that would have meant the same edit three times and an easy miss.
/// Matching still goes through resolve::matches (the same rule runtime resolution uses), and
/// profile creation lives here only.
fn selected_device_profile_index(cfg: &mut Config) -> usize {
    let device = current_selected_device();
    if let Some(idx) = find_profile_index(cfg, device) {
        return idx;
    }
    let matcher = match device {
        // 虚拟指针:没有 VID/PID,只以 device_injected 标记自己。
        // Virtual pointer: no VID/PID; the device_injected flag is what identifies it.
        Some(key) if key == crate::mouse::device::VIRTUAL_DEVICE_KEY => DeviceMatcher {
            injected: Some(true),
            ..Default::default()
        },
        Some((vid, pid)) => DeviceMatcher {
            vendor_id: Some(vid),
            product_id: Some(pid),
            ..Default::default()
        },
        None => DeviceMatcher::default(),
    };
    cfg.mouse.profiles.push(MouseProfile {
        device: matcher,
        ..Default::default()
    });
    cfg.mouse.profiles.len() - 1
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

/// 构造供其他原生面板复用的设置页开关组件,并绑定调用方的动作。
/// Build the settings switch component for reuse by other native panels and bind the caller's
/// target/action.
pub(crate) unsafe fn make_shared_switch(
    right_x: f64,
    y: f64,
    h: f64,
    checked: bool,
    target: *mut AnyObject,
    action: Sel,
) -> *mut AnyObject {
    let switch = SettingsControl::switch(right_x, y, h, checked);
    if !switch.is_null() {
        let _: () = msg_send![switch, setTarget: target];
        let _: () = msg_send![switch, setAction: action];
    }
    switch
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
    // show_settings 每次打开都复位到通用页,这里再切到 About(tag=6)。
    // show_settings resets to the General page on every open; switch to About (tag=6) here.
    select_sidebar(6);
    unsafe {
        with_settings_ui(|ui| {
            if let Some(u) = ui.as_ref() {
                scroll_page_to_top(u.about_view);
            }
        });
    }
    start_inline_update_check();
}

/// 更新流程开始时展开 About 页 Updates 卡片与文档,容纳内联的更新状态/进度/按钮。
/// Expand the About page Updates card and document at the start of a flow so inline update
/// status/progress/buttons fit within the following space.
pub(crate) fn expand_update_section(window_h: f64) {
    with_settings_ui(|ui_guard| {
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
            let _: () = msg_send![ui.update_divider, setHidden: false];
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
            set_about_restore_control_visible_for_ui(ui, false);

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
    });
}

/// 更新流程结束时收起 About 页 Updates 卡片与文档,恢复默认紧凑布局。
/// Collapse the About page Updates card and document when a flow ends, restoring the compact look.
/// 更新流程展开/收起时同步隐藏/显示 About 页的恢复本页默认控件,
/// 避免展开的更新卡片与该控件在页面底部相互遮挡。
/// Hide/show the About page's restore control while the inline update card is expanded, so
/// the grown card and the control don't overlap at the page bottom.
/// Update the About restore control while the caller already owns the settings UI guard.
/// 调用方已经持有设置 UI 锁时，直接更新 About 恢复控件，避免递归获取同一把锁。
unsafe fn set_about_restore_control_visible_for_ui(ui: &mut SettingsUi, visible: bool) {
    let control = &mut ui.page_restores[6];
    let _: () = msg_send![control.container, setHidden: !visible];
    let _: () = msg_send![control.surface, setHidden: !visible];
}

pub(crate) fn collapse_update_section() {
    with_settings_ui(|ui_guard| {
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
            let _: () = msg_send![ui.update_divider, setHidden: true];
            let _: () = msg_send![ui.update_check_button, setHidden: false];
            ui.update_card_expanded = false;
            set_about_restore_control_visible_for_ui(ui, true);
        }
    });
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
                log_debug!(
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
        "layout.focused_thumbnail_prewarm",
        old.layout.focused_thumbnail_prewarm,
        new.layout.focused_thumbnail_prewarm
    );
    changed!(
        "layout.show_app_name_in_cards",
        old.layout.show_app_name_in_cards,
        new.layout.show_app_name_in_cards
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
    changed!(
        "windows.activation_mode",
        old.windows.activation_mode,
        new.windows.activation_mode
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
        "clipboard.delete_after_paste",
        old.clipboard.delete_after_paste,
        new.clipboard.delete_after_paste
    );
    changed!(
        "clipboard.clear_system_pasteboard_after_paste",
        old.clipboard.clear_system_pasteboard_after_paste,
        new.clipboard.clear_system_pasteboard_after_paste
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
    changed!(
        "window_control.display_up",
        old.window_control.display_up,
        new.window_control.display_up
    );
    changed!(
        "window_control.display_down",
        old.window_control.display_down,
        new.window_control.display_down
    );
    changed!(
        "window_control.display_left",
        old.window_control.display_left,
        new.window_control.display_left
    );
    changed!(
        "window_control.display_right",
        old.window_control.display_right,
        new.window_control.display_right
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

/// 用当前 CONFIG 填充设置控件(每次打开都刷新,反映外部编辑 + Reload)。
/// 重建设备下拉框(反映热插拔)。
///
/// Populate settings controls from current CONFIG (refreshed on each open). Rebuilds the device
/// popup to reflect hot-plug changes.
fn load_settings_values() {
    let cfg = CONFIG.read().unwrap().clone();
    load_settings_from(&cfg);
}

/// 用指定配置填充设置控件(正常打开 / 恢复默认后重填共用)。
/// Populate settings controls from a given config (shared by normal open and post-restore
/// refill).
fn load_settings_from(cfg: &Config) {
    let is_cmd = SHORTCUT_IS_CMD.load(Ordering::SeqCst);
    unsafe {
        with_settings_ui(|ui_guard| {
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
            let prewarm_state = if cfg.layout.focused_thumbnail_prewarm {
                1isize
            } else {
                0isize
            };
            let _: () = msg_send![ui.focused_thumbnail_prewarm, setState: prewarm_state];
            let app_name_state = if cfg.layout.show_app_name_in_cards {
                1isize
            } else {
                0isize
            };
            let _: () = msg_send![ui.show_app_name_in_cards, setState: app_name_state];
            // overlay_position:下拉框 index 0 = 跟随激活窗口(active_window), 1 = 主屏幕(main)。
            // overlay_position: popup index 0 = follow active window (active_window), 1 = main (main).
            let op_idx = match cfg.windows.overlay_position.as_str() {
                "main" => 1,
                _ => 0, // "active_window" (default)
            };
            let _: () = msg_send![ui.overlay_position, selectItemAtIndex: op_idx as isize];
            // activation_mode:下拉框 index 0 = 悬停激活(hover), 1 = 点击激活(click)。
            // activation_mode: popup index 0 = activate on hover (hover), 1 = activate on click (click).
            let activation_idx = match cfg.windows.activation_mode.as_str() {
                "click" => 1,
                _ => 0,
            };
            let _: () = msg_send![ui.activation_mode, selectItemAtIndex: activation_idx as isize];
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
            let _: () = msg_send![ui.enable_mouse, setState: if cfg.mouse.enabled { 1isize } else { 0isize }];
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
            // 三个条件行区块(行数/跟踪速度/仅缩略图两行)按各自条件重算显隐。
            // Recompute the three conditional row blocks (line count, tracking speed, the
            // thumbnail-only pair) from their own conditions.
            update_conditional_rows(ui);

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
                ui.window_control_display_up,
                setState: if cfg.window_control.display_up { 1isize } else { 0isize }
            ];
            let _: () = msg_send![
                ui.window_control_display_down,
                setState: if cfg.window_control.display_down { 1isize } else { 0isize }
            ];
            let _: () = msg_send![
                ui.window_control_display_left,
                setState: if cfg.window_control.display_left { 1isize } else { 0isize }
            ];
            let _: () = msg_send![
                ui.window_control_display_right,
                setState: if cfg.window_control.display_right { 1isize } else { 0isize }
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
            let _: () = msg_send![
                ui.clipboard_delete_after_paste,
                setState: if cfg.clipboard.delete_after_paste { 1isize } else { 0isize }
            ];
            let _: () = msg_send![
                ui.clipboard_clear_system_pasteboard_after_paste,
                setState: if cfg.clipboard.clear_system_pasteboard_after_paste {
                    1isize
                } else {
                    0isize
                }
            ];
            set_field(
                ui.clipboard_max_entries,
                cfg.clipboard.max_entries.to_string(),
            );
            let auto_expire_days = (cfg.clipboard.auto_expire_days as i64)
                .clamp(CLIPBOARD_AUTO_EXPIRE_MIN, CLIPBOARD_AUTO_EXPIRE_MAX);
            let _: () = msg_send![
                ui.clipboard_auto_expire_days,
                setIntegerValue: auto_expire_days as isize
            ];
            set_field(ui.clipboard_auto_expire_days_value_label, auto_expire_days);
            // pin_follow_selection:下拉框 index 0 = 跟随置顶, 1 = 保持当前位置。
            // pin_follow_selection: popup index 0 = follow, 1 = keep.
            let pin_idx: isize = if cfg.clipboard.pin_follow_selection {
                0
            } else {
                1
            };
            let _: () = msg_send![ui.clipboard_pin_follow, selectItemAtIndex: pin_idx];
            // ===== 快捷操作页:填充总开关与四个动作开关 =====
            // Quick-actions page: populate the master and four action switches.
            let _: () = msg_send![
                ui.quick_actions_enabled,
                setState: if cfg.quick_actions.enabled { 1isize } else { 0isize }
            ];
            let _: () = msg_send![
                ui.quick_actions_open_settings,
                setState: if cfg.quick_actions.open_settings { 1isize } else { 0isize }
            ];
            let _: () = msg_send![
                ui.quick_actions_open_finder,
                setState: if cfg.quick_actions.open_finder { 1isize } else { 0isize }
            ];
            let _: () = msg_send![
                ui.quick_actions_show_desktop,
                setState: if cfg.quick_actions.show_desktop { 1isize } else { 0isize }
            ];
            let _: () = msg_send![
                ui.quick_actions_lock_screen,
                setState: if cfg.quick_actions.lock_screen { 1isize } else { 0isize }
            ];
            let _: () = msg_send![
                ui.quick_actions_locate_pointer,
                setState: if cfg.quick_actions.locate_pointer { 1isize } else { 0isize }
            ];
            update_clipboard_controls_enabled(ui);
            update_window_control_controls_enabled(ui);
            update_quick_actions_controls_enabled(ui);
        });
    }
}

/// 填充鼠标页的 per-device 控件(反转/禁用加速/跟踪速度/模式/行数)。
/// 供 load_settings_from 与 handle_device_changed 共用。
///
/// Fill the mouse page's per-device controls (reverse/disable-accel/tracking-speed/mode/line-count).
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
    // 指针加速 / 跟踪速度:配置有值就用配置值;未设时显示设备当前生效值(与 LinearMouse
    // 一致,未设时读设备现值而不是伪造一个默认值);读不到再用兜底值。
    // 选中"所有鼠标"时没有单一设备可读,退回第一台已连接设备(LinearMouse 同款做法:
    // 无匹配设备时用 firstMatchedDevice)。
    //
    // Pointer acceleration / tracking speed: the configured value wins; when unset, show the
    // device's live value (same as LinearMouse -- read the device instead of inventing a
    // default); the fallback applies only when that read fails.
    // With "All Mice" selected there is no single device to read, so fall back to the first
    // connected one (LinearMouse does the same via firstMatchedDevice).
    let source_device = current_selected_device().or_else(|| {
        crate::mouse::device::connected_devices()
            .first()
            .map(|d| (d.vendor_id, d.product_id))
    });
    let acceleration = resolved
        .acceleration
        .or_else(|| source_device.and_then(crate::mouse::pointer::read_acceleration))
        .unwrap_or(crate::mouse::pointer::FALLBACK_ACCELERATION);
    // 滑杆是线性取值(0..=10);越界值(设备现值可能来自别的工具)夹到区间内,保证读数与滑块
    // 位置一致。
    // The slider is linear (0..=10); a value outside the range (the device value may come from
    // another tool) is clamped so the readout and the handle stay consistent.
    let acceleration = acceleration.clamp(
        crate::config::MOUSE_ACCELERATION_MIN,
        crate::config::MOUSE_ACCELERATION_MAX,
    );
    let _: () = msg_send![ui.pointer_accel_slider, setDoubleValue: acceleration];
    set_field(
        ui.pointer_accel_value_label,
        pointer_accel_display(acceleration),
    );
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

/// Refresh the cross-thread hint for the quick-action tap from the window's current responder.
/// 根据窗口当前 first responder 刷新快捷操作 tap 使用的跨线程状态提示。
unsafe fn refresh_text_input_state(window: *mut AnyObject) {
    let responder: *mut AnyObject = msg_send![window, firstResponder];
    if responder.is_null() {
        set_text_input_active(false);
        return;
    }
    let is_text_field: bool = msg_send![responder, isKindOfClass: class!(NSTextField)];
    let is_field_editor: bool = msg_send![responder, isKindOfClass: class!(NSTextView)];
    let active = if is_text_field || is_field_editor {
        let editable: bool = msg_send![responder, isEditable];
        editable
    } else {
        false
    };
    set_text_input_active(active);
}

/// End inline text editing when the user clicks elsewhere in the settings window.
///
/// The settings window uses a borderless collection of plain NSViews as its page background.
/// Those views do not become first responder themselves, so AppKit otherwise leaves an
/// NSTextField editor active after a click on empty page space. Resigning before dispatching the
/// mouse event lets the clicked control become first responder again when appropriate.
/// C 回调的 panic 边界:panic 穿不过 extern "C" 帧(会 abort 整个进程),这里统一接住。
/// Panic boundary for the C callback: a panic cannot unwind through an `extern "C"` frame (it
/// aborts the process), so it is contained here.
extern "C" fn settings_window_send_event(_self: *mut c_void, _cmd: Sel, event: *mut AnyObject) {
    crate::callback_guard::void("settings_window_send_event", || unsafe {
        settings_window_send_event_inner(_self, _cmd, event)
    });
}

unsafe fn settings_window_send_event_inner(_self: *mut c_void, _cmd: Sel, event: *mut AnyObject) {
    unsafe {
        if !event.is_null() {
            let event_type: usize = msg_send![event, type];
            if event_type == NSEVENT_TYPE_LEFT_MOUSE_DOWN {
                let window = _self as *mut AnyObject;
                collapse_restore_confirmations_on_external_click(window, event);
                // 下拉的"点外面收起"不再挂在这里:它现在由 widgets 里的本地事件监视器处理 ——
                // 下拉有自己的浮层窗口,只拦设置窗口的点击覆盖不到录制面板等其它窗口。
                // Closing a dropdown on an outside click no longer lives here: it is handled by the
                // local event monitor in widgets, because the dropdown has its own popup window and
                // hooking only the settings window misses the recording panel and others.
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
        if !_self.is_null() {
            refresh_text_input_state(_self as *mut AnyObject);
        }
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
            crate::quit_from_settings();
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
        refresh_settings_root_corner(_self as *mut AnyObject);
        grow_short_page_documents();
    }
}

/// 窗口变高时把矮于视口的页文档长高:非翻转文档矮于 clip 时会被 AppKit 贴在视口
/// 底部,整页内容统一下移 (视口高-文档高)——快捷操作页文档 728pt 在 752pt 高的
/// 窗口里整页下移 24pt 即此因。只增长不收缩;刚长高的可见页回顶,其余滚动位置
/// 不动(拖拽缩放期间幂等,无布局循环风险)。
/// When the window grows, raise any page document shorter than its viewport: AppKit
/// bottom-pins a non-flipped document shorter than its clip, shifting the whole page down
/// by (clip - document) -- the quick-actions page (728pt doc in a 752pt window) dropped
/// 24pt for exactly this reason. Grow-only; a just-grown visible page re-scrolls to the
/// top while other scroll offsets stay untouched (idempotent per resize tick, no layout
/// loop risk).
unsafe fn grow_short_page_documents() {
    with_settings_ui(|ui| {
        let Some(ui) = ui.as_ref() else {
            return;
        };
        let scrolls = [
            ui.general_view,
            ui.switcher_view,
            ui.mouse_view,
            ui.clipboard_view,
            ui.window_control_view,
            ui.quick_actions_view,
            ui.about_view,
        ];
        let selected = widgets::SIDEBAR_SELECTED.load(Ordering::SeqCst);
        for (index, &scroll) in scrolls.iter().enumerate() {
            if scroll.is_null() {
                continue;
            }
            let clip: *mut AnyObject = msg_send![scroll, contentView];
            if clip.is_null() {
                continue;
            }
            let doc: *mut AnyObject = msg_send![scroll, documentView];
            if doc.is_null() {
                continue;
            }
            let clip_bounds: NSRect = msg_send![clip, bounds];
            let doc_frame: NSRect = msg_send![doc, frame];
            if doc_frame.size.height < clip_bounds.size.height {
                let _: () = msg_send![doc, setFrame: NSRect::new(
                    NSPoint::new(0.0, 0.0),
                    NSSize::new(doc_frame.size.width, clip_bounds.size.height),
                )];
                if index == selected {
                    widgets::scroll_page_to_top(scroll);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn pointer_accel_slider_clamps_and_rounds() {
        assert_eq!(pointer_accel_from_slider(0.0), 0.0);
        assert_eq!(pointer_accel_from_slider(0.6875), 0.69);
        assert_eq!(pointer_accel_from_slider(1.0), 1.0);
        assert_eq!(pointer_accel_from_slider(1.234), 1.23);
        // 下界 0、上界 10(线性滑杆的端点);负值(理论上拖不到)夹到 0。
        // Bottom 0 and top 10 (the linear slider's ends); a negative value (unreachable in theory)
        // clamps to 0.
        assert_eq!(pointer_accel_from_slider(-0.42), 0.0);
        assert_eq!(pointer_accel_from_slider(99.0), MOUSE_ACCELERATION_MAX);
        assert_eq!(
            pointer_accel_from_slider(f64::NAN),
            crate::mouse::pointer::FALLBACK_ACCELERATION
        );
    }

    #[test]
    fn pointer_accel_readout_shows_two_decimals() {
        assert_eq!(pointer_accel_display(0.0), "0.00");
        // 默认值 1.00 = macOS 给鼠标键的出厂默认(功能上线前的手感)。
        // The 1.00 default is macOS's factory default for the mouse key (what the pointer felt like
        // before this setting existed).
        assert_eq!(pointer_accel_display(1.0), "1.00");
        assert_eq!(pointer_accel_display(0.6875), "0.69");
    }

    use super::{
        color_component_to_byte, glass_tint_group_frames, rgba_hex_from_components,
        settings_effective_corner_radius, GLASS_TINT_GROUP_GAP, GLASS_TINT_SCREEN_MARGIN,
    };
    use super::{pointer_accel_display, pointer_accel_from_slider};
    use crate::config::MOUSE_ACCELERATION_MAX;
    use objc2_foundation::{NSPoint, NSRect, NSSize};

    #[test]
    fn color_components_round_and_clamp_to_rgba_hex() {
        assert_eq!(rgba_hex_from_components(0.0, 0.5, 1.0, 0.25), "0080ff40");
        assert_eq!(rgba_hex_from_components(-1.0, 2.0, 0.1, 0.9), "00ff1ae6");
        assert_eq!(color_component_to_byte(0.501), 128);
    }

    #[test]
    fn mapping_row_interactive_views_match_the_registry_contract() {
        // 注册(mapping::update_mapping_controls_enabled)与注销(mapping::render_mapping_rows_locked)
        // 共用 MappingRow::interactive_views。该清单必须覆盖所有参与可用性注册的 view,并排除
        // 从不注册的 separator:少一个就会在注册表里留下悬垂地址,下一次设置窗口点击即崩溃。
        // Registration (mapping::update_mapping_controls_enabled) and unregistration
        // (mapping::render_mapping_rows_locked) share MappingRow::interactive_views. It must
        // cover every view that takes part in availability registration and exclude the
        // never-registered separator: a missing view leaves a dangling registry address that
        // crashes the next settings click.
        let p = |n: usize| n as *mut objc2::runtime::AnyObject;
        let row = super::MappingRow {
            label: p(1),
            desc_label: p(2),
            action_icon: p(3),
            edit: p(4),
            delete: p(5),
            separator: p(6),
            caps: vec![p(7), p(8)],
        };
        assert_eq!(
            row.interactive_views()
                .map(|view| view as usize)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5, 7, 8]
        );
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

    #[test]
    fn settings_corner_radius_uses_the_largest_effective_corner() {
        assert_eq!(
            settings_effective_corner_radius(Some([18.0, 26.0, 20.0, 24.0]), 12.0),
            26.0
        );
    }

    #[test]
    fn settings_corner_radius_falls_back_for_missing_or_invalid_values() {
        assert_eq!(settings_effective_corner_radius(None, 26.0), 26.0);
        assert_eq!(
            settings_effective_corner_radius(Some([18.0, f64::NAN, 20.0, 24.0]), 26.0),
            26.0
        );
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
