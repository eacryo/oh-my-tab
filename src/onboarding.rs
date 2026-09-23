//! 首次运行引导:一个独立小窗口,分步说明权限、显示方式和更多功能。
//!
//! 为什么需要它:本应用是菜单栏应用(LSUIElement),新装用户打开后只看得到菜单栏图标,
//! 缺辅助功能时再弹一个告警框——没人告诉他这个应用是干什么的、按哪个键、缺哪个权限。
//!
//! 设计要点:
//! - **不阻塞**:权限未授予时仍可继续;选项在点下一步后才应用。
//! - **四步固定**:权限与开机自启、浮窗显示方式、剪贴板历史、更多功能入口。
//! - **只自动出现一次**:自动展示时立即写 UserDefaults 标记;此后只能从菜单栏"欢迎使用"
//!   或开发开关再次打开。缺权限的老情况仍由启动告警框兜底,不会因为标记而失联。
//! - **可验证**:`--force-onboarding` 忽略标记;
//!   `--onboarding=reset` 先清标记;`--fake-permissions=ax:0,sr:1`(仅 debug 构建)
//!   伪造*展示用*权限状态,便于在不动 TCC 的前提下走完所有分支。
//!
//! First-run onboarding: a small standalone window that walks through permissions and login,
//! overlay display mode, and pointers to the other features.
//!
//! Why it exists: this is a menu-bar app (LSUIElement), so a fresh install shows nothing but the
//! status item (and an alert when Accessibility is missing) and never explains what the app does,
//! which key to press, or which permission it needs.
//!
//! Design: never blocking (permissions can be deferred and selections apply on Next); four fixed
//! steps; auto-shown once (the UserDefaults marker is written as soon as it appears, and the
//! status item's "Welcome" entry or a development switch reopens it -- a missing permission stays
//! covered by the startup alert);
//! and verifiable (`--force-onboarding` ignores the marker,
//! `--onboarding=reset` clears it first, and `--fake-permissions=ax:0,sr:1` -- debug
//! builds only -- fakes the DISPLAYED permission state so every branch can be walked without
//! touching TCC).

use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::ffi::c_void;

use crate::config::{schedule_config_persist, Config, CONFIG};
use crate::ffi::{hex_to_ns_color, make_nsstring, release_obj, MainThreadSlot, ObjPtr};
use crate::i18n::{t, tf};
use crate::log_debug;

// ========== 标记与开发开关 / marker and development switches ==========

/// 自动展示过就写这个标记(与 update_notice 的跨启动标记同类,放 UserDefaults 而非 config:
/// "看过引导"是 UI 生命周期,不是用户要调的设置项)。
/// Written once the guide has been auto-shown (a UserDefaults cross-launch marker like the
/// update-notice ones: "already seen the guide" is UI lifecycle, not a setting the user tunes).
const COMPLETED_KEY: &str = "oh-my-tab-onboarding-completed";
/// 开发开关(argv,见 dev_flags):强制展示 / 复位标记 / 抑制展示 / 伪造权限状态。
/// Development switches (argv, see dev_flags): force the guide, reset the marker, suppress it,
/// or fake the permission status.
const FORCE_FLAG: &str = "force-onboarding";
const RESET_ARG: &str = "--onboarding=reset";
const NO_ONBOARDING_FLAG: &str = "no-onboarding";
const FAKE_PERMISSIONS_FLAG: &str = "fake-permissions";

/// 按钮 tag:窗口里所有按钮共用一个 selector,按 tag 分派。
/// Button tags: every button in the window shares one selector and dispatches by tag.
const ACTION_NEXT: isize = 2;
const ACTION_OPEN_ACCESSIBILITY: isize = 3;
const ACTION_RESTART_APP: isize = 4;
const ACTION_ALLOW_SCREEN: isize = 5;
const ACTION_FINISH: isize = 7;
const ACTION_SKIP_GUIDE: isize = 12;
const ACTION_TOGGLE_LAUNCH_DRAFT: isize = 13;
const ACTION_SELECT_ICONS: isize = 14;
const ACTION_SELECT_THUMBNAILS: isize = 15;
const ACTION_OPEN_SETTINGS: isize = 16;
const ACTION_BACK: isize = 17;
const ACTION_TOGGLE_CLIPBOARD_DRAFT: isize = 18;
const ACTION_DISPLAY_MODE: isize = 19;

// ========== 纯逻辑(单测覆盖)/ pure logic (unit-tested) ==========

/// *展示用*权限覆盖。只影响引导窗口的判断与文案,绝不进入事件 tap / 权限监督的真实判定。
/// DISPLAY-only permission override. It affects the guide's decisions and copy and never reaches
/// the event tap or the permission supervisor.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct PermissionOverride {
    pub(crate) ax: Option<bool>,
    pub(crate) screen: Option<bool>,
}

/// 解析 `ax:1,sr:0` 形式的伪造权限(未提到的项保持 None = 用真实状态;非法项忽略)。
/// Parses a fake-permission spec like `ax:1,sr:0` (absent keys stay None = use the real state;
/// malformed entries are ignored).
pub(crate) fn parse_fake_permissions(spec: &str) -> PermissionOverride {
    let mut over = PermissionOverride::default();
    for part in spec.split(',') {
        let Some((key, value)) = part.split_once(':') else {
            continue;
        };
        let parsed = match value.trim() {
            "1" | "true" | "on" | "yes" => Some(true),
            "0" | "false" | "off" | "no" => Some(false),
            _ => None,
        };
        match key.trim() {
            "ax" | "accessibility" => over.ax = parsed,
            "sr" | "screen" | "screen_recording" => over.screen = parsed,
            _ => {}
        }
    }
    over
}

/// 固定的四步引导。
/// The four fixed onboarding steps.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Step {
    PermissionsAndStartup,
    DisplayMode,
    ClipboardHistory,
    MoreFeatures,
}

pub(crate) fn steps_for() -> [Step; 4] {
    [
        Step::PermissionsAndStartup,
        Step::DisplayMode,
        Step::ClipboardHistory,
        Step::MoreFeatures,
    ]
}

/// 是否该自动弹出。老用户(已授权 + 已有配置文件)静默跳过,避免升级后被引导打扰;
/// 被冒烟模式抑制或已经展示过(且未强制)都不弹。
/// Whether the guide may auto-show. Existing users (granted AND a config file already present) are
/// skipped silently so an upgrade does not nag them; smoke mode and an already-shown marker (when
/// not forced) also suppress it.
pub(crate) fn should_auto_show(
    completed: bool,
    forced: bool,
    ax_granted: bool,
    config_exists: bool,
    suppressed: bool,
) -> bool {
    if forced {
        return true;
    }
    if suppressed || completed {
        return false;
    }
    !(ax_granted && config_exists)
}

/// 引导窗口在开发开关/冒烟模式下是否被抑制。
/// Whether the guide is suppressed in smoke mode or by an explicit opt-out.
pub(crate) fn is_suppressed() -> bool {
    crate::dev_flags::any_prefix("--smoke") || crate::dev_flags::present(NO_ONBOARDING_FLAG)
}

fn forced_requested() -> bool {
    crate::dev_flags::present(FORCE_FLAG)
}

fn reset_requested() -> bool {
    std::env::args().any(|arg| arg == RESET_ARG)
}

fn fake_permissions() -> PermissionOverride {
    if !cfg!(debug_assertions) {
        return PermissionOverride::default();
    }
    crate::dev_flags::value(FAKE_PERMISSIONS_FLAG)
        .map(|spec| parse_fake_permissions(&spec))
        .unwrap_or_default()
}

// ========== UserDefaults 标记 / UserDefaults marker ==========

unsafe fn defaults_set_bool(key: &str, value: bool) {
    let defaults: *mut AnyObject = msg_send![class!(NSUserDefaults), standardUserDefaults];
    let key_ns = make_nsstring(key);
    let _: () = msg_send![defaults, setBool: value, forKey: key_ns];
    release_obj(key_ns);
}

unsafe fn defaults_get_bool(key: &str) -> bool {
    let defaults: *mut AnyObject = msg_send![class!(NSUserDefaults), standardUserDefaults];
    let key_ns = make_nsstring(key);
    let value: bool = msg_send![defaults, boolForKey: key_ns];
    release_obj(key_ns);
    value
}

unsafe fn defaults_remove(key: &str) {
    let defaults: *mut AnyObject = msg_send![class!(NSUserDefaults), standardUserDefaults];
    let key_ns = make_nsstring(key);
    let _: () = msg_send![defaults, removeObjectForKey: key_ns];
    release_obj(key_ns);
}

/// 已展示过引导(跨启动)。
/// Whether the guide has already been shown (cross-launch).
pub(crate) fn completed() -> bool {
    unsafe { defaults_get_bool(COMPLETED_KEY) }
}

fn mark_completed() {
    unsafe { defaults_set_bool(COMPLETED_KEY, true) };
}

// ========== 运行时状态 / runtime state ==========

#[derive(Clone)]
struct UiState {
    steps: [Step; 4],
    index: usize,
    overrides: PermissionOverride,
    launch_at_login: bool,
    thumbnails_enabled: bool,
    clipboard_enabled: bool,
}

static WINDOW: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
static CONTENT: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
static TIMER: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
static STATE: MainThreadSlot<Option<UiState>> = MainThreadSlot::new(None);
/// 上一次渲染时的权限签名,用来决定 1s tick 是否需要重建内容。
/// The permission signature rendered last; a 1s tick rebuilds only when it changes.
static LAST_SIGNATURE: MainThreadSlot<Option<(bool, bool, bool)>> = MainThreadSlot::new(None);
/// 按钮指针 → 动作 tag。**不能用 `setTag:` 携带动作 id**:SettingsButton 的悬停处理器按 tag
/// 选调色板(`mouseEntered:` 与 `mouseExited:` 都读它,分别对应主按钮蓝、次级、紧凑灰三种),
/// 覆盖 tag 会同时破坏悬停色和「移开后恢复常态色」(实测:主按钮移开指针后永久变灰)。
/// Button pointer to action tag. The action id must NOT ride `setTag:`: the SettingsButton hover
/// handlers select their palette from that tag (`mouseEntered:` and `mouseExited:` both read it,
/// mapping to the primary-blue / action / compact-grey palettes), so overwriting it breaks both the
/// hover colour and the restore-to-normal colour (measured: the primary button stayed grey after
/// the pointer left).
static ACTION_TAGS: MainThreadSlot<Vec<(usize, isize)>> = MainThreadSlot::new(Vec::new());

fn override_ax(over: PermissionOverride) -> bool {
    over.ax
        .unwrap_or_else(crate::ffi::has_accessibility_permission)
}

fn override_screen(over: PermissionOverride) -> bool {
    over.screen
        .unwrap_or_else(crate::thumbnail::capture_allowed)
}

fn permission_signature(state: &UiState) -> (bool, bool, bool) {
    (
        override_ax(state.overrides),
        override_screen(state.overrides),
        crate::restart::restart_required(),
    )
}

// ========== 入口 / entry points ==========

/// 启动序列调用:满足条件时展示引导,返回是否展示了(展示时调用方应跳过缺权限告警框,
/// 避免"告警框 + 引导窗"双弹)。
/// Called from the launch sequence: shows the guide when the conditions hold and reports whether it
/// did (the caller then skips the missing-permission alert so the two never pop together).
pub(crate) fn maybe_show_on_launch() -> bool {
    let forced = forced_requested();
    if reset_requested() {
        unsafe { defaults_remove(COMPLETED_KEY) };
        log_debug!("[onboarding] marker cleared by {}", RESET_ARG);
    }
    let config_exists = crate::config::config_file_exists();
    let overrides = fake_permissions();
    let ax_granted = override_ax(overrides);
    if !should_auto_show(
        completed(),
        forced,
        ax_granted,
        config_exists,
        is_suppressed(),
    ) {
        log_debug!(
            "[onboarding] skipped (completed={} forced={} ax={} config={} suppressed={})",
            completed(),
            forced,
            ax_granted,
            config_exists,
            is_suppressed()
        );
        return false;
    }
    log_debug!(
        "[onboarding] showing (forced={} ax={} screen={} config={})",
        forced,
        ax_granted,
        override_screen(overrides),
        config_exists
    );
    // 自动展示即写标记:此后不再自动打扰,但菜单栏"欢迎使用"与开发开关随时可重开,
    // 缺权限的老情况仍由启动告警框兜底。
    // Mark as shown: no further automatic nagging, while the status item's entry and the
    // development switches reopen it at will and the startup alert still covers a missing grant.
    // 强制/开发模式下不写标记,便于反复验证。
    // Forced/development runs do not write the marker so the flow stays repeatable.
    if !forced {
        mark_completed();
    }
    show_internal(overrides);
    true
}

/// 手动重开(设置窗口「关于」页的「查看引导」):不重置标记,也不再强制欢迎页。
/// Manual reopen (the settings window's About page entry): no marker reset, and no forced welcome
/// page.
pub(crate) fn show_manually() {
    show_internal(fake_permissions());
}

fn show_internal(overrides: PermissionOverride) {
    let config = CONFIG.read().unwrap().clone();
    let state = UiState {
        steps: steps_for(),
        index: 0,
        overrides,
        launch_at_login: config.startup.launch_at_login,
        thumbnails_enabled: config.layout.thumbnails_enabled,
        clipboard_enabled: config.clipboard.enabled,
    };
    let step_count = state.steps.len();
    *STATE.lock().unwrap() = Some(state);
    unsafe { ensure_window() };
    render_current_step();
    start_tick_timer();
    log_debug!("[onboarding] step 1/{} shown", step_count);
}

/// 1s tick:权限状态变化时重建当前页(用户去系统设置授权后不必手动刷新)。
/// The 1s tick rebuilds the current page when permission state changes, so a grant made in System
/// Settings appears without a manual refresh.
pub(crate) fn tick() {
    let Some(state) = STATE.lock().unwrap().clone() else {
        return;
    };
    if !is_visible() {
        stop_tick_timer();
        return;
    }
    let signature = permission_signature(&state);
    if *LAST_SIGNATURE.lock().unwrap() == Some(signature) {
        return;
    }
    // 步骤序列固定,保持当前页索引并刷新状态文案/屏幕录制提示。
    // The step sequence is fixed; keep its current index and refresh permission messaging.
    let current = state.steps.get(state.index).copied();
    let index = current
        .and_then(|step| state.steps.iter().position(|candidate| *candidate == step))
        .unwrap_or(0);
    if let Some(state) = STATE.lock().unwrap().as_mut() {
        state.index = index;
    }
    render_current_step();
}

pub(crate) fn is_visible() -> bool {
    let Some(window) = *WINDOW.lock().unwrap() else {
        return false;
    };
    let visible: bool = unsafe { msg_send![window.0, isVisible] };
    visible
}

/// 隐藏引导窗口(测试/退出路径复用)。
/// Hides the guide window (reused by tests and teardown paths).
pub(crate) fn hide() {
    stop_tick_timer();
    if let Some(window) = *WINDOW.lock().unwrap() {
        let _: () = unsafe { msg_send![window.0, orderOut: std::ptr::null::<AnyObject>()] };
    }
}

// ========== 窗口与内容 / window and content ==========

unsafe fn ensure_window() {
    if WINDOW.lock().unwrap().is_some() {
        return;
    }
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WINDOW_W, WINDOW_H));
    let window: *mut AnyObject = msg_send![class!(NSWindow), alloc];
    let window: *mut AnyObject = msg_send![
        window,
        initWithContentRect: frame,
        styleMask: WINDOW_STYLE_TITLED,
        backing: 2u64,
        defer: false
    ];
    let title = make_nsstring(&t("onboarding.window_title"));
    let _: () = msg_send![window, setTitle: title];
    release_obj(title);
    // 关窗不释放:指针留在静态槽里复用(与设置窗口同款约定)。
    // Closing must not release it: the pointer stays in the static slot for reuse (same
    // convention as the settings window).
    let _: () = msg_send![window, setReleasedWhenClosed: false];
    let _: () = msg_send![window, center];
    let content: *mut AnyObject = msg_send![window, contentView];
    *CONTENT.lock().unwrap() = Some(ObjPtr::new(content));
    *WINDOW.lock().unwrap() = Some(ObjPtr::new(window));
}

/// 清空内容视图的所有子视图(每步重建内容,与剪贴板浮窗同款做法)。
/// Removes every subview of the content view (each step rebuilds its content, like the clipboard
/// picker does).
unsafe fn clear_content(content: *mut AnyObject) {
    let subviews: *mut AnyObject = msg_send![content, subviews];
    if subviews.is_null() {
        return;
    }
    let count: isize = msg_send![subviews, count];
    for index in 0..count {
        let view: *mut AnyObject = msg_send![subviews, objectAtIndex: index];
        let _: () = msg_send![view, removeFromSuperview];
    }
}

unsafe fn add_label(
    content: *mut AnyObject,
    text: &str,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    style: LabelStyle,
) {
    let field: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let field: *mut AnyObject = msg_send![
        field,
        initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    ];
    let value = make_nsstring(text);
    let _: () = msg_send![field, setStringValue: value];
    release_obj(value);
    let _: () = msg_send![field, setBezeled: false];
    let _: () = msg_send![field, setDrawsBackground: false];
    let _: () = msg_send![field, setEditable: false];
    let _: () = msg_send![field, setSelectable: false];
    let font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: style.size, weight: style.weight];
    let _: () = msg_send![field, setFont: font];
    let _: () = msg_send![field, setTextColor: hex_to_ns_color(style.color)];
    if style.wrap {
        let cell: *mut AnyObject = msg_send![field, cell];
        let _: () = msg_send![cell, setWraps: true];
        let _: () = msg_send![cell, setUsesSingleLineMode: false];
        let _: () = msg_send![field, setUsesSingleLineMode: false];
        let _: () = msg_send![field, setLineBreakMode: 0isize]; // NSLineBreakByWordWrapping
    } else {
        let _: () = msg_send![field, setUsesSingleLineMode: true];
        let _: () = msg_send![field, setLineBreakMode: 4isize]; // NSLineBreakByTruncatingTail
    }
    let _: () = msg_send![content, addSubview: field];
    release_obj(field);
}

unsafe fn add_button(
    content: *mut AnyObject,
    title: &str,
    tag: isize,
    x: f64,
    y: f64,
    w: f64,
    role: crate::settings::components::SettingsButtonRole,
) {
    let Some(target) = crate::CONTROLLER.lock().unwrap().map(|ptr| ptr.0) else {
        return;
    };
    let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(w, BUTTON_H));
    let button = crate::settings::components::SettingsButton::action(
        frame,
        title,
        target,
        sel!(handleOnboardingAction:),
        role,
    );
    if button.is_null() {
        return;
    }
    // 登记动作 id,不碰按钮 tag(见 ACTION_TAGS 的说明)。
    // Register the action id without touching the button's tag (see ACTION_TAGS).
    ACTION_TAGS.lock().unwrap().push((button as usize, tag));
    // 自绘按钮的标题不会自动变成无障碍标签,显式设一下(VoiceOver 与 AX 检查都要靠它)。
    // A custom-drawn button's title does not become its accessibility label automatically; set it
    // explicitly (both VoiceOver and accessibility inspection rely on it).
    let accessibility_label = make_nsstring(title);
    let _: () = msg_send![button, setAccessibilityLabel: accessibility_label];
    release_obj(accessibility_label);
    crate::settings::widgets::configure_settings_button_wrapping(button, w, 3);
    let _: () = msg_send![content, addSubview: button];
    release_obj(button);
}

fn render_current_step() {
    let Some(state) = STATE.lock().unwrap().clone() else {
        return;
    };
    let Some(content) = *CONTENT.lock().unwrap() else {
        return;
    };
    let content = content.0;
    if content.is_null() {
        return;
    }
    let overrides = state.overrides;
    let ax_granted = override_ax(overrides);
    let screen_granted = override_screen(overrides);
    let total = state.steps.len();
    let Some(step) = state.steps.get(state.index).copied() else {
        return;
    };
    unsafe {
        clear_content(content);
        // 内容重建 = 旧按钮全部销毁,动作表随之清空(每页至多 4 个按钮,不会增长)。
        // Rebuilding the content destroys every old button, so the action map is cleared with it
        // (a page holds at most four buttons, so it cannot grow).
        ACTION_TAGS.lock().unwrap().clear();
        let title = tf(
            "onboarding.step_counter",
            &[
                ("current", &(state.index + 1).to_string()),
                ("total", &total.to_string()),
            ],
        );
        add_label(
            content,
            &title,
            PAD,
            WINDOW_H - 34.0,
            WINDOW_W - PAD * 2.0,
            18.0,
            COUNTER_STYLE,
        );
        match step {
            Step::PermissionsAndStartup => render_permissions_and_startup(
                content,
                ax_granted,
                crate::restart::restart_required(),
                state.launch_at_login,
            ),
            Step::DisplayMode => {
                render_display_mode(content, state.thumbnails_enabled, screen_granted)
            }
            Step::ClipboardHistory => render_clipboard_history(content, state.clipboard_enabled),
            Step::MoreFeatures => render_more_features(content),
        }
        render_footer(content, state.index);
    }
    *LAST_SIGNATURE.lock().unwrap() = Some(permission_signature(&state));
    // 窗口可能已被关闭:渲染后确保它在前台(accessory 应用要显式激活)。
    // The window may be closed: make sure it is frontmost after rendering (an accessory app must
    // activate explicitly).
    unsafe {
        if let Some(window) = *WINDOW.lock().unwrap() {
            let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![app, activateIgnoringOtherApps: true];
            let _: () = msg_send![window.0, makeKeyAndOrderFront: std::ptr::null::<AnyObject>()];
        }
    }
}

unsafe fn render_permissions_and_startup(
    content: *mut AnyObject,
    ax_granted: bool,
    restart_required: bool,
    launch_at_login: bool,
) {
    let app = app_display_name();
    add_label(
        content,
        &t("onboarding.permissions_title"),
        PAD,
        194.0,
        WINDOW_W - PAD * 2.0,
        TITLE_H,
        TITLE_STYLE,
    );
    add_label(
        content,
        &tf(
            "onboarding.accessibility_body",
            &[("app", &app), ("shortcut", &shortcut_label())],
        ),
        PAD,
        143.0,
        WINDOW_W - PAD * 2.0,
        46.0,
        BODY_STYLE,
    );
    let (status_key, status_color) = if restart_required {
        ("onboarding.status_restart_required", STATUS_WARN)
    } else if ax_granted {
        ("onboarding.status_granted", STATUS_OK)
    } else {
        ("onboarding.status_missing", STATUS_WARN)
    };
    add_label(
        content,
        &t("onboarding.accessibility_label"),
        PAD,
        105.0,
        136.0,
        STATUS_H,
        status_style(SECONDARY_TEXT),
    );
    add_label(
        content,
        &t(status_key),
        PAD + 136.0,
        105.0,
        190.0,
        STATUS_H,
        status_style(status_color),
    );
    if !ax_granted || restart_required {
        let primary_action = if restart_required {
            ACTION_RESTART_APP
        } else {
            ACTION_OPEN_ACCESSIBILITY
        };
        let primary_title = if restart_required {
            tf("onboarding.btn_restart", &[("app", &app)])
        } else {
            t("onboarding.btn_open_settings")
        };
        add_button(
            content,
            &primary_title,
            primary_action,
            WINDOW_W - PAD - BUTTON_W,
            99.0,
            BUTTON_W,
            crate::settings::components::SettingsButtonRole::Action,
        );
    }
    add_label(
        content,
        &t("onboarding.launch_label"),
        PAD,
        62.0,
        WINDOW_W - PAD * 2.0 - 58.0,
        28.0,
        TITLE_STYLE,
    );
    add_switch(
        content,
        WINDOW_W - PAD,
        59.0,
        launch_at_login,
        ACTION_TOGGLE_LAUNCH_DRAFT,
    );
}

unsafe fn render_display_mode(
    content: *mut AnyObject,
    thumbnails_enabled: bool,
    screen_granted: bool,
) {
    add_label(
        content,
        &t("onboarding.display_title"),
        PAD,
        194.0,
        WINDOW_W - PAD * 2.0,
        TITLE_H,
        TITLE_STYLE,
    );
    add_label(
        content,
        &t("onboarding.display_body"),
        PAD,
        151.0,
        WINDOW_W - PAD * 2.0,
        34.0,
        BODY_STYLE,
    );
    add_display_mode_control(content, thumbnails_enabled);
    let needs_screen_permission = thumbnails_enabled && !screen_granted;
    if needs_screen_permission {
        add_label(
            content,
            &t("onboarding.screen_permission_needed"),
            PAD,
            59.0,
            WINDOW_W - PAD * 2.0 - BUTTON_W - GAP,
            STATUS_H,
            status_style(STATUS_WARN),
        );
        add_button(
            content,
            &t("onboarding.btn_open_settings"),
            ACTION_ALLOW_SCREEN,
            WINDOW_W - PAD - BUTTON_W,
            55.0,
            BUTTON_W,
            crate::settings::components::SettingsButtonRole::Action,
        );
    }
}

unsafe fn render_clipboard_history(content: *mut AnyObject, enabled: bool) {
    add_label(
        content,
        &t("onboarding.clipboard_title"),
        PAD,
        194.0,
        WINDOW_W - PAD * 2.0,
        TITLE_H,
        TITLE_STYLE,
    );
    add_label(
        content,
        &t("onboarding.clipboard_body"),
        PAD,
        144.0,
        WINDOW_W - PAD * 2.0,
        48.0,
        BODY_STYLE,
    );
    add_label(
        content,
        &t("onboarding.clipboard_label"),
        PAD,
        91.0,
        WINDOW_W - PAD * 2.0 - 58.0,
        28.0,
        TITLE_STYLE,
    );
    add_switch(
        content,
        WINDOW_W - PAD,
        88.0,
        enabled,
        ACTION_TOGGLE_CLIPBOARD_DRAFT,
    );
}

unsafe fn render_more_features(content: *mut AnyObject) {
    add_label(
        content,
        &t("onboarding.more_title"),
        PAD,
        194.0,
        WINDOW_W - PAD * 2.0,
        TITLE_H,
        TITLE_STYLE,
    );
    add_label(
        content,
        &t("onboarding.more_body"),
        PAD,
        132.0,
        WINDOW_W - PAD * 2.0,
        46.0,
        BODY_STYLE,
    );
    add_button(
        content,
        &t("onboarding.btn_open_app_settings"),
        ACTION_OPEN_SETTINGS,
        PAD,
        78.0,
        BUTTON_W,
        crate::settings::components::SettingsButtonRole::Action,
    );
}

unsafe fn add_display_mode_control(content: *mut AnyObject, thumbnails_enabled: bool) {
    let control: *mut AnyObject = msg_send![class!(NSSegmentedControl), alloc];
    let control: *mut AnyObject = msg_send![
        control,
        initWithFrame: NSRect::new(NSPoint::new(PAD, 96.0), NSSize::new(320.0, 34.0))
    ];
    let _: () = msg_send![control, setSegmentCount: 2isize];
    let icons = make_nsstring(&t("onboarding.display_icons"));
    let thumbnails = make_nsstring(&t("onboarding.display_thumbnails"));
    let _: () = msg_send![control, setLabel: icons, forSegment: 0isize];
    let _: () = msg_send![control, setLabel: thumbnails, forSegment: 1isize];
    release_obj(icons);
    release_obj(thumbnails);
    let _: () =
        msg_send![control, setSelectedSegment: if thumbnails_enabled { 1isize } else { 0isize }];
    let Some(target) = crate::CONTROLLER.lock().unwrap().map(|ptr| ptr.0) else {
        release_obj(control);
        return;
    };
    let _: () = msg_send![control, setTarget: target];
    let _: () = msg_send![control, setAction: sel!(handleOnboardingAction:)];
    ACTION_TAGS
        .lock()
        .unwrap()
        .push((control as usize, ACTION_DISPLAY_MODE));
    let label = make_nsstring(&t("onboarding.display_title"));
    let _: () = msg_send![control, setAccessibilityLabel: label];
    release_obj(label);
    let _: () = msg_send![content, addSubview: control];
    release_obj(control);
}

unsafe fn add_switch(
    content: *mut AnyObject,
    right_x: f64,
    y: f64,
    checked: bool,
    action_tag: isize,
) {
    let switch = crate::settings::components::onboarding_switch(right_x, y, BUTTON_H, checked);
    if switch.is_null() {
        return;
    }
    let Some(target) = crate::CONTROLLER.lock().unwrap().map(|ptr| ptr.0) else {
        release_obj(switch);
        return;
    };
    let _: () = msg_send![switch, setTarget: target];
    let _: () = msg_send![switch, setAction: sel!(handleOnboardingAction:)];
    ACTION_TAGS
        .lock()
        .unwrap()
        .push((switch as usize, action_tag));
    let accessibility_label = make_nsstring(&t(if action_tag == ACTION_TOGGLE_LAUNCH_DRAFT {
        "onboarding.launch_label"
    } else {
        "onboarding.clipboard_label"
    }));
    let _: () = msg_send![switch, setAccessibilityLabel: accessibility_label];
    release_obj(accessibility_label);
    let _: () = msg_send![content, addSubview: switch];
    release_obj(switch);
}

unsafe fn render_footer(content: *mut AnyObject, index: usize) {
    if index + 1 < steps_for().len() {
        add_text_button(
            content,
            &t("onboarding.btn_skip_guide"),
            ACTION_SKIP_GUIDE,
            PAD,
        );
    }
    let next_x = WINDOW_W - PAD - BUTTON_W;
    if index > 0 {
        add_button(
            content,
            &t("onboarding.btn_previous"),
            ACTION_BACK,
            next_x - GAP - BUTTON_W,
            BUTTON_Y,
            BUTTON_W,
            crate::settings::components::SettingsButtonRole::Action,
        );
    }
    let (title, action) = if index + 1 == steps_for().len() {
        (t("onboarding.btn_finish"), ACTION_FINISH)
    } else {
        (t("onboarding.btn_next"), ACTION_NEXT)
    };
    add_button(
        content,
        &title,
        action,
        next_x,
        BUTTON_Y,
        BUTTON_W,
        crate::settings::components::SettingsButtonRole::Primary,
    );
}

unsafe fn add_text_button(content: *mut AnyObject, title: &str, action_tag: isize, x: f64) {
    let Some(target) = crate::CONTROLLER.lock().unwrap().map(|ptr| ptr.0) else {
        return;
    };
    let button: *mut AnyObject = msg_send![class!(NSButton), alloc];
    let button: *mut AnyObject = msg_send![
        button,
        initWithFrame: NSRect::new(NSPoint::new(x, BUTTON_Y + 2.0), NSSize::new(100.0, BUTTON_H - 4.0))
    ];
    let title_ns = make_nsstring(title);
    let _: () = msg_send![button, setTitle: title_ns];
    let _: () = msg_send![button, setAccessibilityLabel: title_ns];
    release_obj(title_ns);
    let _: () = msg_send![button, setButtonType: 0isize];
    let _: () = msg_send![button, setBordered: false];
    let _: () = msg_send![button, setContentTintColor: hex_to_ns_color(SECONDARY_TEXT)];
    let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 12.0f64];
    let _: () = msg_send![button, setFont: font];
    let _: () = msg_send![button, setTarget: target];
    let _: () = msg_send![button, setAction: sel!(handleOnboardingAction:)];
    ACTION_TAGS
        .lock()
        .unwrap()
        .push((button as usize, action_tag));
    let _: () = msg_send![content, addSubview: button];
    release_obj(button);
}

/// 应用显示名(CFBundleDisplayName → CFBundleName → 固定回退)。
/// The app's display name (CFBundleDisplayName -> CFBundleName -> literal fallback).
fn app_display_name() -> String {
    unsafe {
        let display = crate::ffi::bundle_info_string("CFBundleDisplayName");
        if !display.is_empty() {
            return display;
        }
        let name = crate::ffi::bundle_info_string("CFBundleName");
        if !name.is_empty() {
            return name;
        }
    }
    t("onboarding.app_fallback_name")
}

/// 当前生效的召唤键(读配置,默认 ⌘Tab)。
/// The effective switcher shortcut (read from config, default ⌘Tab).
fn shortcut_label() -> String {
    let modifier = CONFIG
        .read()
        .map(|config| config.keyboard.modifier.clone())
        .unwrap_or_default();
    match modifier.as_str() {
        "option" | "opt" | "alt" => "⌥Tab".to_string(),
        "control" | "ctrl" => "⌃Tab".to_string(),
        _ => "⌘Tab".to_string(),
    }
}

// ========== 按钮动作 / button actions ==========

/// 按钮分派(由 lib.rs 注册的 `handleOnboardingAction:` selector 调用)。
/// Button dispatch (called by the `handleOnboardingAction:` selector registered in lib.rs).
/// 按钮分派(由 lib.rs 注册的 `handleOnboardingAction:` selector 调用)。
/// Button dispatch (called by the `handleOnboardingAction:` selector registered in lib.rs).
pub(crate) extern "C" fn on_action(_self: *mut c_void, _cmd: Sel, sender: *mut AnyObject) {
    crate::callback_guard::void("onboarding_action", || {
        if sender.is_null() {
            return;
        }
        let key = sender as usize;
        let tag = ACTION_TAGS
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(pointer, _)| *pointer == key)
            .map(|(_, tag)| *tag)
            .unwrap_or(0);
        if tag == ACTION_DISPLAY_MODE {
            let selected: isize = unsafe { msg_send![sender, selectedSegment] };
            handle_action(if selected == 1 {
                ACTION_SELECT_THUMBNAILS
            } else {
                ACTION_SELECT_ICONS
            });
            return;
        }
        handle_action(tag);
    });
}

/// 设置窗口「关于」页的入口(`handleOpenOnboarding:`):与窗口内按钮共用同一个 selector 家族,
/// 但按 selector 派发(设置页的按钮不动 tag,见 AGENTS.md 里 SettingsButton tag 的约定)。
/// The settings window's About-page entry (`handleOpenOnboarding:`): it rides its own selector
/// rather than a tag, because the settings page must not touch a SettingsButton's tag (see the
/// SettingsButton tag note in AGENTS.md).
pub(crate) extern "C" fn on_open_from_settings(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut AnyObject,
) {
    crate::callback_guard::void("onboarding_open_from_settings", show_manually);
}

/// 1s 定时器回调(权限状态变化时重建当前步骤)。
/// The 1s timer callback (rebuilds the current step when the permission state changes).
pub(crate) extern "C" fn on_tick(_self: *mut c_void, _cmd: Sel, _timer: *mut c_void) {
    crate::callback_guard::void("onboarding_tick", tick);
}

pub(crate) fn handle_action(tag: isize) {
    match tag {
        ACTION_NEXT => advance(),
        ACTION_BACK => retreat(),
        ACTION_OPEN_ACCESSIBILITY => crate::open_privacy_accessibility(),
        ACTION_RESTART_APP => {
            let Some(target) = crate::CONTROLLER.lock().unwrap().map(|ptr| ptr.0) else {
                return;
            };
            unsafe {
                let _: () = msg_send![
                    target,
                    performSelectorOnMainThread: sel!(handlePermissionRestartNow:),
                    withObject: std::ptr::null::<AnyObject>(),
                    waitUntilDone: false
                ];
            }
        }
        ACTION_ALLOW_SCREEN => crate::open_privacy_screen_recording(),
        ACTION_TOGGLE_LAUNCH_DRAFT => {
            if let Some(state) = STATE.lock().unwrap().as_mut() {
                state.launch_at_login = !state.launch_at_login;
            }
            render_current_step();
        }
        ACTION_TOGGLE_CLIPBOARD_DRAFT => {
            if let Some(state) = STATE.lock().unwrap().as_mut() {
                state.clipboard_enabled = !state.clipboard_enabled;
            }
            render_current_step();
        }
        ACTION_SELECT_ICONS | ACTION_SELECT_THUMBNAILS => {
            if let Some(state) = STATE.lock().unwrap().as_mut() {
                state.thumbnails_enabled = tag == ACTION_SELECT_THUMBNAILS;
            }
            render_current_step();
        }
        ACTION_OPEN_SETTINGS => {
            mark_completed();
            hide();
            crate::settings::show_settings_page(0);
        }
        ACTION_FINISH => {
            mark_completed();
            log_debug!("[onboarding] finished");
            hide();
        }
        ACTION_SKIP_GUIDE => {
            mark_completed();
            log_debug!("[onboarding] skipped");
            hide();
        }
        _ => {}
    }
}

fn advance() {
    let current_step = {
        let slot = STATE.lock().unwrap();
        let Some(state) = slot.as_ref() else {
            return;
        };
        state.steps[state.index]
    };
    commit_step_selection(current_step);
    {
        let mut slot = STATE.lock().unwrap();
        let Some(state) = slot.as_mut() else {
            return;
        };
        if state.index + 1 >= state.steps.len() {
            return;
        }
        state.index += 1;
    }
    render_current_step();
}

fn retreat() {
    let mut slot = STATE.lock().unwrap();
    let Some(state) = slot.as_mut() else {
        return;
    };
    if state.index == 0 {
        return;
    }
    state.index -= 1;
    drop(slot);
    render_current_step();
}

fn commit_step_selection(step: Step) {
    let Some(state) = STATE.lock().unwrap().clone() else {
        return;
    };
    let old = CONFIG.read().unwrap().clone();
    let new = config_with_step_selection(&old, &state, step);
    if new != old {
        apply_config(&old, &new);
    }
}

fn config_with_step_selection(old: &Config, state: &UiState, step: Step) -> Config {
    let mut new = old.clone();
    match step {
        Step::PermissionsAndStartup => new.startup.launch_at_login = state.launch_at_login,
        Step::DisplayMode => new.layout.thumbnails_enabled = state.thumbnails_enabled,
        Step::ClipboardHistory => new.clipboard.enabled = state.clipboard_enabled,
        Step::MoreFeatures => {}
    }
    new
}

/// 将已确认的引导选择沿用设置窗口的更新路径,使运行时副作用和持久化与设置页一致。
/// Apply a confirmed onboarding choice through the settings update path so runtime effects and
/// persistence match the Settings page.
fn apply_config(old: &Config, new: &Config) {
    if let Ok(mut slot) = CONFIG.write() {
        *slot = new.clone();
    }
    crate::runtime_config::apply_config_change(
        old,
        new,
        crate::runtime_config::ConfigChangeSource::Settings,
    );
    schedule_config_persist();
}

// ========== 定时器 / timer ==========
fn start_tick_timer() {
    if TIMER.lock().unwrap().is_some() {
        return;
    }
    let Some(target) = crate::CONTROLLER.lock().unwrap().map(|ptr| ptr.0) else {
        return;
    };
    let timer: *mut AnyObject = unsafe {
        msg_send![
            class!(NSTimer),
            scheduledTimerWithTimeInterval: 1.0f64,
            target: target,
            selector: sel!(handleOnboardingTick:),
            userInfo: std::ptr::null::<AnyObject>(),
            repeats: true
        ]
    };
    if !timer.is_null() {
        *TIMER.lock().unwrap() = Some(ObjPtr::new(timer));
    }
}

fn stop_tick_timer() {
    if let Some(timer) = TIMER.lock().unwrap().take() {
        let _: () = unsafe { msg_send![timer.0, invalidate] };
    }
}

// ========== 布局常量 / layout constants ==========

const WINDOW_W: f64 = 520.0;
const WINDOW_H: f64 = 280.0;
const TITLE_H: f64 = 24.0;
const WINDOW_STYLE_TITLED: u64 = 1;
const PAD: f64 = 24.0;
const GAP: f64 = 10.0;
const BUTTON_H: f64 = 32.0;
const BUTTON_W: f64 = 132.0;
const BUTTON_Y: f64 = 20.0;
/// 引导窗口里只用到三档文本样式 + 一档状态行样式,收成常量避免每次调用重复传参。
/// The guide only needs three text styles plus one status-line style, kept as constants so each
/// call site does not repeat the parameters.
#[derive(Clone, Copy)]
struct LabelStyle {
    size: f64,
    weight: f64,
    color: u32,
    wrap: bool,
}
const TITLE_STYLE: LabelStyle = LabelStyle {
    size: 17.0,
    weight: 0.23,
    color: PRIMARY_TEXT,
    wrap: false,
};
const BODY_STYLE: LabelStyle = LabelStyle {
    size: 13.0,
    weight: 0.0,
    color: SECONDARY_TEXT,
    wrap: true,
};
const COUNTER_STYLE: LabelStyle = LabelStyle {
    size: 11.5,
    weight: 0.23,
    color: 0x8E8E93FF,
    wrap: false,
};
fn status_style(color: u32) -> LabelStyle {
    LabelStyle {
        size: 13.5,
        weight: 0.0,
        color,
        wrap: false,
    }
}

/// 正文高度:调用方按该页文案的行数给(中文最长的那一页决定值)。
/// Body heights, chosen per page from its line count (the longest Chinese page sets the value).
const STATUS_H: f64 = 20.0;

const PRIMARY_TEXT: u32 = 0x1D1D1FFF;
const SECONDARY_TEXT: u32 = 0x6E6E73FF;
const STATUS_OK: u32 = 0x1F8B4CFF;
const STATUS_WARN: u32 = 0xC2410CFF;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_button_tint_selector_is_supported_by_nsbutton() {
        let supported: bool = unsafe {
            msg_send![class!(NSButton), instancesRespondToSelector: sel!(setContentTintColor:)]
        };
        assert!(
            supported,
            "NSButton must support the onboarding text button tint API"
        );
    }

    #[test]
    fn fake_permission_spec_parses_known_keys_and_ignores_junk() {
        let over = parse_fake_permissions("ax:0,sr:1");
        assert_eq!(over.ax, Some(false));
        assert_eq!(over.screen, Some(true));
        // 未知键/非法值:忽略该键,保持 None(用真实状态)。
        let over = parse_fake_permissions("ax:maybe,sr:off,zz:1");
        assert_eq!(over.ax, None);
        assert_eq!(over.screen, Some(false));
        // 空串/无冒号片段不 panic。
        assert_eq!(parse_fake_permissions(""), PermissionOverride::default());
        assert_eq!(parse_fake_permissions("ax"), PermissionOverride::default());
    }

    #[test]
    fn onboarding_always_has_the_four_setup_steps() {
        assert_eq!(
            steps_for(),
            [
                Step::PermissionsAndStartup,
                Step::DisplayMode,
                Step::ClipboardHistory,
                Step::MoreFeatures
            ]
        );
    }

    #[test]
    fn onboarding_draft_settings_commit_only_for_their_step() {
        let old = Config::default();
        let draft = UiState {
            steps: steps_for(),
            index: 0,
            overrides: PermissionOverride::default(),
            launch_at_login: true,
            thumbnails_enabled: false,
            clipboard_enabled: true,
        };

        let after_permissions =
            config_with_step_selection(&old, &draft, Step::PermissionsAndStartup);
        assert!(after_permissions.startup.launch_at_login);
        assert_eq!(
            after_permissions.layout.thumbnails_enabled,
            old.layout.thumbnails_enabled
        );

        let after_display = config_with_step_selection(&old, &draft, Step::DisplayMode);
        assert!(!after_display.layout.thumbnails_enabled);
        assert_eq!(
            after_display.startup.launch_at_login,
            old.startup.launch_at_login
        );

        let after_clipboard = config_with_step_selection(&old, &draft, Step::ClipboardHistory);
        assert!(after_clipboard.clipboard.enabled);
        assert_eq!(
            after_clipboard.startup.launch_at_login,
            old.startup.launch_at_login
        );
        assert_eq!(
            after_clipboard.layout.thumbnails_enabled,
            old.layout.thumbnails_enabled
        );

        let after_more = config_with_step_selection(&old, &draft, Step::MoreFeatures);
        assert_eq!(after_more, old);
    }

    #[test]
    fn auto_show_is_forced_once_suppressed_or_for_existing_users() {
        // 未看过的全新安装:弹。
        assert!(should_auto_show(false, false, false, false, false));
        // 开发开关强制:即便看过/被抑制也弹。
        assert!(should_auto_show(true, true, true, true, true));
        // 已看过、或被冒烟模式抑制:不弹(除非强制)。
        assert!(!should_auto_show(true, false, false, false, false));
        assert!(!should_auto_show(false, false, false, false, true));
        // 老用户(已授权 + 已有配置)静默跳过,升级后不被引导打扰。
        assert!(!should_auto_show(false, false, true, true, false));
        // 已授权但没有配置文件(比如刚清过配置)= 仍算需要引导。
        assert!(should_auto_show(false, false, true, false, false));
    }
}
