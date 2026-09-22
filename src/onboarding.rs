//! 首次运行引导:一个独立小窗口,把"必需权限 / 可选权限 / 怎么用与常用开关"讲清楚。
//!
//! 为什么需要它:本应用是菜单栏应用(LSUIElement),新装用户打开后只看得到菜单栏图标,
//! 缺辅助功能时再弹一个告警框——没人告诉他这个应用是干什么的、按哪个键、缺哪个权限。
//!
//! 设计要点:
//! - **不阻塞**:只有辅助功能是必需的,其余步骤都可跳过;不改任何默认值。
//! - **状态驱动**:每一步的内容由实时权限状态推导(已授权的步骤直接跳过),所以重开、
//!   权限被撤销、老用户升级都不会落到错误的步骤上。
//! - **只自动出现一次**:自动展示时立即写 UserDefaults 标记;此后只能从菜单栏"欢迎使用"
//!   或开发开关再次打开。缺权限的老情况仍由启动告警框兜底,不会因为标记而失联。
//! - **可验证**:`OH_MY_TAB_FORCE_ONBOARDING=1` / `--force-onboarding` 忽略标记;
//!   `--onboarding=reset` 先清标记;`OH_MY_TAB_FAKE_PERMISSIONS=ax:0,sr:1`(仅 debug 构建)
//!   伪造*展示用*权限状态,便于在不动 TCC 的前提下走完所有分支。
//!
//! First-run onboarding: a small standalone window covering the required permission, the
//! optional one, and how to use the app plus the common switches.
//!
//! Why it exists: this is a menu-bar app (LSUIElement), so a fresh install shows nothing but the
//! status item (and an alert when Accessibility is missing) and never explains what the app does,
//! which key to press, or which permission it needs.
//!
//! Design: never blocking (only Accessibility is required, every other step can be skipped and no
//! default changes); state-driven (each step's content comes from live permission state, so
//! reopening, revoking a grant or upgrading cannot land on the wrong step); auto-shown once (the
//! UserDefaults marker is written as soon as it appears, and the status item's "Welcome" entry or
//! a development switch reopens it -- a missing permission stays covered by the startup alert);
//! and verifiable (`OH_MY_TAB_FORCE_ONBOARDING=1` / `--force-onboarding` ignores the marker,
//! `--onboarding=reset` clears it first, and `OH_MY_TAB_FAKE_PERMISSIONS=ax:0,sr:1` -- debug
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
const FORCE_ENV: &str = "OH_MY_TAB_FORCE_ONBOARDING";
const FORCE_ARG: &str = "--force-onboarding";
const RESET_ARG: &str = "--onboarding=reset";
const FAKE_PERMISSIONS_ENV: &str = "OH_MY_TAB_FAKE_PERMISSIONS";

/// 按钮 tag:窗口里所有按钮共用一个 selector,按 tag 分派。
/// Button tags: every button in the window shares one selector and dispatches by tag.
const ACTION_START: isize = 1;
const ACTION_NEXT: isize = 2;
const ACTION_OPEN_ACCESSIBILITY: isize = 3;
const ACTION_RESTART_APP: isize = 4;
const ACTION_ALLOW_SCREEN: isize = 5;
const ACTION_SKIP_SCREEN: isize = 6;
const ACTION_FINISH: isize = 7;
const ACTION_LATER: isize = 8;
const ACTION_TOGGLE_LAUNCH_AT_LOGIN: isize = 9;
const ACTION_TOGGLE_CLIPBOARD: isize = 10;
const ACTION_TOGGLE_WINDOW_CONTROL: isize = 11;

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

/// 引导步骤。`Usage` 永远在最后,且是唯一不会被跳过的收尾页。
/// One onboarding step. `Usage` is always last and is the one page that is never skipped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Step {
    Welcome,
    Accessibility,
    ScreenRecording,
    Usage,
}

/// 由实时状态推导步骤序列:已满足的权限步骤直接跳过,`always_welcome`(强制/手动打开)时
/// 总是先给一页欢迎。
/// Derives the step list from live state: satisfied permission steps are skipped, and
/// `always_welcome` (forced or manually reopened) always starts with the welcome page.
pub(crate) fn steps_for(ax_granted: bool, screen_granted: bool, always_welcome: bool) -> Vec<Step> {
    let mut steps = Vec::new();
    if always_welcome || !ax_granted {
        steps.push(Step::Welcome);
    }
    if !ax_granted {
        steps.push(Step::Accessibility);
    }
    if !screen_granted {
        steps.push(Step::ScreenRecording);
    }
    steps.push(Step::Usage);
    steps
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
    std::env::args().any(|arg| arg.starts_with("--smoke"))
        || std::env::var_os("OH_MY_TAB_NO_ONBOARDING").is_some()
}

fn forced_from_env_or_args() -> bool {
    std::env::var_os(FORCE_ENV).is_some() || std::env::args().any(|arg| arg == FORCE_ARG)
}

fn reset_requested() -> bool {
    std::env::args().any(|arg| arg == RESET_ARG)
}

fn fake_permissions() -> PermissionOverride {
    if !cfg!(debug_assertions) {
        return PermissionOverride::default();
    }
    std::env::var(FAKE_PERMISSIONS_ENV)
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
    steps: Vec<Step>,
    index: usize,
    overrides: PermissionOverride,
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
    let forced = forced_from_env_or_args();
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
    show_internal(overrides, true);
    true
}

/// 手动重开(设置窗口「关于」页的「查看引导」):不重置标记,也不再强制欢迎页。
/// Manual reopen (the settings window's About page entry): no marker reset, and no forced welcome
/// page.
pub(crate) fn show_manually() {
    show_internal(fake_permissions(), false);
}

fn show_internal(overrides: PermissionOverride, always_welcome: bool) {
    let steps = steps_for(
        override_ax(overrides),
        override_screen(overrides),
        always_welcome,
    );
    let state = UiState {
        steps,
        index: 0,
        overrides,
    };
    let step_count = state.steps.len();
    *STATE.lock().unwrap() = Some(state);
    unsafe { ensure_window() };
    render_current_step();
    start_tick_timer();
    log_debug!("[onboarding] step 1/{} shown", step_count);
}

/// 1s tick:权限状态变化时重建当前步骤(用户去系统设置授权后不必手动刷新)。
/// The 1s tick: rebuilds the current step when the permission state changed, so a grant made in
/// System Settings shows up without any manual refresh.
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
    // 权限状态变了:按新状态重算步骤,尽量停在语义相同的一页上。
    // The permission state changed: recompute the steps and stay on the semantically same page
    // where possible.
    let current = state.steps.get(state.index).copied();
    let steps = steps_for(signature.0, signature.1, false);
    let index = current
        .and_then(|step| steps.iter().position(|candidate| *candidate == step))
        .unwrap_or(0);
    if let Some(state) = STATE.lock().unwrap().as_mut() {
        state.steps = steps;
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

/// 内容块垂直居中:标题/正文(/状态行)作为一个整体在"计数行"与"按钮行"之间居中,
/// 这样每一步的留白一致,不会出现某页中间一大块空洞(或按钮被挤到窗口外)。
/// Vertically centers the content block: title, body (and status line) sit as one block between
/// the step counter and the button row, so every page has the same whitespace instead of a hole in
/// the middle or buttons pushed outside the window.
///
/// 返回 (标题 y, 正文 y, 状态行 y);y 为底原点坐标系。
/// Returns (title y, body y, status y) in the bottom-origin coordinate system.
fn block_positions(body_h: f64, has_status: bool, bottom: f64) -> (f64, f64, f64) {
    let counter_y = WINDOW_H - 30.0;
    let status_h = if has_status { STATUS_H + 6.0 } else { 0.0 };
    let block_h = TITLE_H + 8.0 + body_h + status_h;
    let top = counter_y - 10.0;
    let start = bottom + ((top - bottom) - block_h).max(0.0) / 2.0;
    let status_y = start;
    let body_y = status_y + status_h;
    let title_y = body_y + body_h + 8.0;
    (title_y, body_y, status_y)
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
            Step::Welcome => render_welcome(content),
            Step::Accessibility => render_accessibility(content, ax_granted),
            Step::ScreenRecording => render_screen_recording(content, screen_granted),
            Step::Usage => render_usage(content),
        }
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

unsafe fn render_welcome(content: *mut AnyObject) {
    let app = app_display_name();
    let (title_y, body_y, _) = block_positions(WELCOME_BODY_H, false, BUTTON_Y + BUTTON_H + 18.0);
    add_label(
        content,
        &tf("onboarding.welcome_title", &[("app", &app)]),
        PAD,
        title_y,
        WINDOW_W - PAD * 2.0,
        TITLE_H,
        TITLE_STYLE,
    );
    add_label(
        content,
        &tf(
            "onboarding.welcome_body",
            &[("app", &app), ("shortcut", &shortcut_label())],
        ),
        PAD,
        body_y,
        WINDOW_W - PAD * 2.0,
        WELCOME_BODY_H,
        BODY_STYLE,
    );
    add_button(
        content,
        &t("onboarding.btn_start"),
        ACTION_START,
        PAD,
        BUTTON_Y,
        BUTTON_W,
        crate::settings::components::SettingsButtonRole::Primary,
    );
    add_button(
        content,
        &t("onboarding.btn_later"),
        ACTION_LATER,
        PAD + BUTTON_W + GAP,
        BUTTON_Y,
        BUTTON_W,
        crate::settings::components::SettingsButtonRole::Action,
    );
}

unsafe fn render_accessibility(content: *mut AnyObject, granted: bool) {
    let app = app_display_name();
    let (title_y, body_y, _) =
        block_positions(ACCESSIBILITY_BODY_H, true, BUTTON_Y + BUTTON_H + 18.0);
    add_label(
        content,
        &t("onboarding.accessibility_title"),
        PAD,
        title_y,
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
        body_y,
        WINDOW_W - PAD * 2.0,
        ACCESSIBILITY_BODY_H,
        BODY_STYLE,
    );
    let restart_required = crate::restart::restart_required();
    let (_, _, status_y) = block_positions(ACCESSIBILITY_BODY_H, true, BUTTON_Y + BUTTON_H + 18.0);
    let (status_key, status_color) = if restart_required {
        ("onboarding.status_restart_required", STATUS_WARN)
    } else if granted {
        ("onboarding.status_granted", STATUS_OK)
    } else {
        ("onboarding.status_missing", STATUS_WARN)
    };
    add_label(
        content,
        &t(status_key),
        PAD,
        status_y,
        WINDOW_W - PAD * 2.0,
        STATUS_H,
        status_style(status_color),
    );
    if granted && !restart_required {
        add_button(
            content,
            &t("onboarding.btn_next"),
            ACTION_NEXT,
            PAD,
            BUTTON_Y,
            BUTTON_W,
            crate::settings::components::SettingsButtonRole::Primary,
        );
    } else {
        add_button(
            content,
            &t("onboarding.btn_open_settings"),
            ACTION_OPEN_ACCESSIBILITY,
            PAD,
            BUTTON_Y,
            BUTTON_W,
            crate::settings::components::SettingsButtonRole::Primary,
        );
    }
    // 未授权时也给一个「下一步」:用户可以先把后面的用法页读完再回来授权,不至于卡在这一页。
    // A "Next" is offered while ungranted so the user can read the later pages first instead of
    // being stuck here.
    if !granted || restart_required {
        add_button(
            content,
            &t("onboarding.btn_next"),
            ACTION_NEXT,
            PAD + BUTTON_W + GAP,
            BUTTON_Y,
            BUTTON_W,
            crate::settings::components::SettingsButtonRole::Action,
        );
    }
    if restart_required {
        add_button(
            content,
            &tf("onboarding.btn_restart", &[("app", &app)]),
            ACTION_RESTART_APP,
            PAD + (BUTTON_W + GAP) * 2.0,
            BUTTON_Y,
            BUTTON_W,
            crate::settings::components::SettingsButtonRole::Action,
        );
    } else {
        add_button(
            content,
            &t("onboarding.btn_later"),
            ACTION_LATER,
            PAD + (BUTTON_W + GAP) * 2.0,
            BUTTON_Y,
            BUTTON_W,
            crate::settings::components::SettingsButtonRole::Action,
        );
    }
}

unsafe fn render_screen_recording(content: *mut AnyObject, granted: bool) {
    let (title_y, body_y, status_y) =
        block_positions(SCREEN_BODY_H, true, BUTTON_Y + BUTTON_H + 18.0);
    add_label(
        content,
        &t("onboarding.screen_title"),
        PAD,
        title_y,
        WINDOW_W - PAD * 2.0,
        TITLE_H,
        TITLE_STYLE,
    );
    add_label(
        content,
        &t("onboarding.screen_body"),
        PAD,
        body_y,
        WINDOW_W - PAD * 2.0,
        SCREEN_BODY_H,
        BODY_STYLE,
    );
    let (status_key, status_color) = if granted {
        ("onboarding.status_granted", STATUS_OK)
    } else {
        ("onboarding.status_missing", STATUS_WARN)
    };
    add_label(
        content,
        &t(status_key),
        PAD,
        status_y,
        WINDOW_W - PAD * 2.0,
        STATUS_H,
        status_style(status_color),
    );
    if granted {
        add_button(
            content,
            &t("onboarding.btn_next"),
            ACTION_NEXT,
            PAD,
            BUTTON_Y,
            BUTTON_W,
            crate::settings::components::SettingsButtonRole::Primary,
        );
    } else {
        add_button(
            content,
            &t("onboarding.btn_allow"),
            ACTION_ALLOW_SCREEN,
            PAD,
            BUTTON_Y,
            BUTTON_W,
            crate::settings::components::SettingsButtonRole::Primary,
        );
        add_button(
            content,
            &t("onboarding.btn_skip_screen"),
            ACTION_SKIP_SCREEN,
            PAD + BUTTON_W + GAP,
            BUTTON_Y,
            BUTTON_W,
            crate::settings::components::SettingsButtonRole::Action,
        );
    }
    add_button(
        content,
        &t("onboarding.btn_later"),
        ACTION_LATER,
        PAD + (BUTTON_W + GAP) * 2.0,
        BUTTON_Y,
        BUTTON_W,
        crate::settings::components::SettingsButtonRole::Action,
    );
}

unsafe fn render_usage(content: *mut AnyObject) {
    let (title_y, body_y, _) = block_positions(USAGE_BODY_H, false, TOGGLE_Y + BUTTON_H + 14.0);
    add_label(
        content,
        &t("onboarding.usage_title"),
        PAD,
        title_y,
        WINDOW_W - PAD * 2.0,
        TITLE_H,
        TITLE_STYLE,
    );
    add_label(
        content,
        &tf("onboarding.usage_body", &[("shortcut", &shortcut_label())]),
        PAD,
        body_y,
        WINDOW_W - PAD * 2.0,
        USAGE_BODY_H,
        BODY_STYLE,
    );
    let config = CONFIG.read().unwrap().clone();
    let toggles: [(isize, bool, &str); 3] = [
        (
            ACTION_TOGGLE_LAUNCH_AT_LOGIN,
            config.startup.launch_at_login,
            "onboarding.opt_launch_at_login",
        ),
        (
            ACTION_TOGGLE_CLIPBOARD,
            config.clipboard.enabled,
            "onboarding.opt_clipboard",
        ),
        (
            ACTION_TOGGLE_WINDOW_CONTROL,
            config.window_control.enabled,
            "onboarding.opt_window_control",
        ),
    ];
    let mut x = PAD;
    for (tag, on, key) in toggles {
        let title = tf(
            if on {
                "onboarding.toggle_on"
            } else {
                "onboarding.toggle_off"
            },
            &[("label", &t(key))],
        );
        add_button(
            content,
            &title,
            tag,
            x,
            TOGGLE_Y,
            TOGGLE_W,
            if on {
                crate::settings::components::SettingsButtonRole::Primary
            } else {
                crate::settings::components::SettingsButtonRole::Compact
            },
        );
        x += TOGGLE_W + GAP;
    }
    add_button(
        content,
        &t("onboarding.btn_finish"),
        ACTION_FINISH,
        PAD,
        FINISH_Y,
        BUTTON_W,
        crate::settings::components::SettingsButtonRole::Primary,
    );
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
        ACTION_START | ACTION_NEXT | ACTION_SKIP_SCREEN => advance(),
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
        ACTION_FINISH => {
            mark_completed();
            log_debug!("[onboarding] finished");
            hide();
        }
        ACTION_LATER => {
            // 「以后再说」= 本次不再打扰,但也算看过(否则每次启动都弹,反而更像 bug)。
            // "Later" dismisses for good: counting it as seen avoids nagging on every launch.
            mark_completed();
            log_debug!("[onboarding] dismissed");
            hide();
        }
        ACTION_TOGGLE_LAUNCH_AT_LOGIN => {
            toggle_config(|cfg| cfg.startup.launch_at_login = !cfg.startup.launch_at_login)
        }
        ACTION_TOGGLE_CLIPBOARD => {
            toggle_config(|cfg| cfg.clipboard.enabled = !cfg.clipboard.enabled)
        }
        ACTION_TOGGLE_WINDOW_CONTROL => {
            toggle_config(|cfg| cfg.window_control.enabled = !cfg.window_control.enabled)
        }
        _ => {}
    }
}

fn advance() {
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

/// 就地改一个配置字段:沿用设置窗口的路径(写 CONFIG → apply_config_change → 防抖落盘),
/// 这样开机自启同步、模块热切换等副作用与设置页完全一致。
/// Mutates one config field through the settings window's path (write CONFIG →
/// apply_config_change → debounced persist) so side effects such as the launch-at-login sync and
/// module hot-switching match the settings page exactly.
fn toggle_config(mutate: impl FnOnce(&mut Config)) {
    let old = CONFIG.read().unwrap().clone();
    let mut new = old.clone();
    mutate(&mut new);
    if let Ok(mut slot) = CONFIG.write() {
        *slot = new.clone();
    }
    crate::runtime_config::apply_config_change(
        &old,
        &new,
        crate::runtime_config::ConfigChangeSource::Settings,
    );
    schedule_config_persist();
    render_current_step();
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
const WINDOW_H: f64 = 250.0;
const TITLE_H: f64 = 24.0;
const WINDOW_STYLE_TITLED: u64 = 1;
const PAD: f64 = 24.0;
const GAP: f64 = 10.0;
const BUTTON_H: f64 = 32.0;
const BUTTON_W: f64 = 132.0;
const TOGGLE_W: f64 = 148.0;
const BUTTON_Y: f64 = 20.0;
/// 收尾页两行按钮:开关行在上,「完成」在下。
/// The last page stacks two button rows: the switches above, "Done" below.
const TOGGLE_Y: f64 = 58.0;
const FINISH_Y: f64 = 18.0;
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
const WELCOME_BODY_H: f64 = 54.0;
const ACCESSIBILITY_BODY_H: f64 = 62.0;
const SCREEN_BODY_H: f64 = 62.0;
const USAGE_BODY_H: f64 = 72.0;
const STATUS_H: f64 = 20.0;

const PRIMARY_TEXT: u32 = 0x1D1D1FFF;
const SECONDARY_TEXT: u32 = 0x6E6E73FF;
const STATUS_OK: u32 = 0x1F8B4CFF;
const STATUS_WARN: u32 = 0xC2410CFF;

#[cfg(test)]
mod tests {
    use super::*;

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
    fn steps_skip_satisfied_permissions_and_always_end_with_usage() {
        // 全缺:欢迎 → 辅助功能 → 屏幕录制 → 使用说明。
        assert_eq!(
            steps_for(false, false, false),
            vec![
                Step::Welcome,
                Step::Accessibility,
                Step::ScreenRecording,
                Step::Usage
            ]
        );
        // 辅助功能已授权:直接进屏幕录制(不再讲必需权限)。
        assert_eq!(
            steps_for(true, false, false),
            vec![Step::ScreenRecording, Step::Usage]
        );
        // 都已授权:只剩收尾页。
        assert_eq!(steps_for(true, true, false), vec![Step::Usage]);
        // 强制/手动打开:总先给欢迎页,但已满足的权限步骤仍然跳过。
        assert_eq!(
            steps_for(true, true, true),
            vec![Step::Welcome, Step::Usage]
        );
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
