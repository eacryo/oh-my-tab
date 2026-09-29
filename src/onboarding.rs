//! First-run onboarding: a small standalone window that walks through permissions and login,
//! overlay display mode, and pointers to the other features.
//!
//! Why it exists: this is a menu-bar app (LSUIElement), so a fresh install shows nothing but the
//! status item (and an alert when Accessibility is missing) and never explains what the app does,
//! which key to press, or which permission it needs.
//!
//! Design: never blocking (permissions can be deferred, the display-mode and clipboard-history
//! choices take effect the moment they are picked because the switcher and the Option+V hotkey read
//! the live config, and launch at login applies on Next); four fixed steps; auto-shown once (the
//! UserDefaults marker is written as soon as it appears, and the status item's "Welcome" entry or a
//! development switch reopens it -- a missing permission stays covered by the startup alert);
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
use crate::log_info;

/// Written once the guide has been auto-shown (a UserDefaults cross-launch marker like the
/// update-notice ones: "already seen the guide" is UI lifecycle, not a setting the user tunes).
const COMPLETED_KEY: &str = "oh-my-tab-onboarding-completed";
/// Written when the app goes away with the guide still on screen (a permission restart: macOS may
/// quit us forcibly, and the screen-recording grant is often followed by "Quit & Reopen"). Holds
/// the step index + 1; 0/absent means nothing pending. Unlike the completion marker this is an
/// explicit "the user is mid-setup" intent, so the next launch reopens the guide there.
const RESUME_KEY: &str = "oh-my-tab-onboarding-resume";
/// The `CFBundleVersion` of the installation that last ran. A difference means the app was
/// (re-)installed rather than merely relaunched — the guide is a setup flow, so every install walks
/// it again, while a plain relaunch of the same build does not.
const BUILD_KEY: &str = "oh-my-tab-onboarding-build";
/// Development switches (argv, see dev_flags): force the guide, reset the marker, suppress it,
/// or fake the permission status.
const FORCE_FLAG: &str = "force-onboarding";
const RESET_ARG: &str = "--onboarding=reset";
const NO_ONBOARDING_FLAG: &str = "no-onboarding";
const FAKE_PERMISSIONS_FLAG: &str = "fake-permissions";

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
const ACTION_TOGGLE_CLIPBOARD_PERSIST: isize = 20;

/// DISPLAY-only permission override. It affects the guide's decisions and copy and never reaches
/// the event tap or the permission supervisor.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct PermissionOverride {
    pub(crate) ax: Option<bool>,
    pub(crate) screen: Option<bool>,
}

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

/// Everything the launch decision depends on, grouped so the booleans cannot be mixed up.
#[derive(Clone, Copy, Default)]
pub(crate) struct LaunchState {
    /// The guide's completion marker for this installation.
    pub(crate) completed: bool,
    /// `--force-onboarding`.
    pub(crate) forced: bool,
    /// A restart interrupted the guide and it should resume on its old step.
    pub(crate) resume: bool,
    /// This launch continues a Sparkle in-place update.
    pub(crate) sparkle_update: bool,
    /// No build recorded yet, or the recorded build differs: the app was installed, not relaunched.
    pub(crate) new_install: bool,
    pub(crate) ax_granted: bool,
    pub(crate) config_exists: bool,
    /// Smoke mode or `--no-onboarding`.
    pub(crate) suppressed: bool,
}

/// Whether the launch sequence opens the guide.
///
/// Manual installs are the point: a fresh or re-installed build always walks the guide again, while
/// an in-place Sparkle update does not (updating is not installing, and nagging right after an
/// update is exactly what the user did not ask for). A restart mid-guide resumes it, suppression
/// wins, and `--force-onboarding` always shows.
pub(crate) fn should_show_on_launch(state: LaunchState) -> bool {
    if state.forced {
        return true;
    }
    if state.suppressed {
        return false;
    }
    if state.resume {
        return true;
    }
    if state.sparkle_update {
        return false;
    }
    if state.new_install {
        return true;
    }
    // Same installation: the guide was already offered once, and an existing user (permission
    // granted plus a config file) is not nagged again.
    !state.completed && !(state.ax_granted && state.config_exists)
}

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

/// Whether the guide has already been shown (cross-launch).
pub(crate) fn completed() -> bool {
    unsafe { defaults_get_bool(COMPLETED_KEY) }
}

fn mark_completed() {
    unsafe { defaults_set_bool(COMPLETED_KEY, true) };
}

unsafe fn defaults_set_int(key: &str, value: isize) {
    let defaults: *mut AnyObject = msg_send![class!(NSUserDefaults), standardUserDefaults];
    let key_ns = make_nsstring(key);
    let _: () = msg_send![defaults, setInteger: value, forKey: key_ns];
    release_obj(key_ns);
}

unsafe fn defaults_get_int(key: &str) -> isize {
    let defaults: *mut AnyObject = msg_send![class!(NSUserDefaults), standardUserDefaults];
    let key_ns = make_nsstring(key);
    let value: isize = msg_send![defaults, integerForKey: key_ns];
    release_obj(key_ns);
    value
}

/// Remembers the guide's current step so an interrupted flow resumes there instead of being treated
/// as "already seen".
///
/// Called whenever the guide is shown and whenever its step changes, so the marker is already on
/// disk for as long as the guide is on screen. That is the point: an unexpected exit (crash, force
/// quit, power loss) never delivers the termination notification, and the completion marker written
/// when the guide appears would otherwise make the guide disappear for good. Do not move this back
/// into the quit/notification paths.
pub(crate) fn note_resume_if_visible() {
    if !is_visible() {
        return;
    }
    let step = STATE
        .lock()
        .unwrap()
        .as_ref()
        .map(|state| state.index)
        .unwrap_or(0);
    unsafe { defaults_set_int(RESUME_KEY, step as isize + 1) };
    log_debug!(
        "[onboarding] resume marker written (step {}/{})",
        step + 1,
        steps_for().len()
    );
}

/// The pending resume step, without consuming it (a suppressed run must not eat the marker).
fn peeking_resume_step() -> Option<usize> {
    let stored = unsafe { defaults_get_int(RESUME_KEY) };
    (stored > 0).then(|| (stored - 1) as usize)
}

fn clear_resume() {
    unsafe { defaults_remove(RESUME_KEY) };
}

unsafe fn defaults_set_string(key: &str, value: &str) {
    let defaults: *mut AnyObject = msg_send![class!(NSUserDefaults), standardUserDefaults];
    let key_ns = make_nsstring(key);
    let value_ns = make_nsstring(value);
    let _: () = msg_send![defaults, setObject: value_ns, forKey: key_ns];
    release_obj(value_ns);
    release_obj(key_ns);
}

unsafe fn defaults_get_string(key: &str) -> String {
    let defaults: *mut AnyObject = msg_send![class!(NSUserDefaults), standardUserDefaults];
    let key_ns = make_nsstring(key);
    let value: *mut AnyObject = msg_send![defaults, stringForKey: key_ns];
    release_obj(key_ns);
    crate::ffi::nsstring_to_rust(value)
}

#[derive(Clone)]
struct UiState {
    steps: [Step; 4],
    index: usize,
    overrides: PermissionOverride,
    launch_at_login: bool,
    thumbnails_enabled: bool,
    clipboard_enabled: bool,
    clipboard_persist: bool,
}

static WINDOW: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
static CONTENT: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
static TIMER: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
static STATE: MainThreadSlot<Option<UiState>> = MainThreadSlot::new(None);
/// The permission signature rendered last; a 1s tick rebuilds only when it changes.
static LAST_SIGNATURE: MainThreadSlot<Option<(bool, bool, bool)>> = MainThreadSlot::new(None);
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

/// Called from the launch sequence: shows the guide when the conditions hold and reports whether it
/// did (the caller then skips the missing-permission alert so the two never pop together).
pub(crate) fn maybe_show_on_launch() -> bool {
    let forced = forced_requested();
    if reset_requested() {
        unsafe { defaults_remove(COMPLETED_KEY) };
        unsafe { defaults_remove(BUILD_KEY) };
        clear_resume();
        log_debug!("[onboarding] markers cleared by {}", RESET_ARG);
    }
    let resume_step = peeking_resume_step();
    let config_exists = crate::config::config_file_exists();
    let overrides = fake_permissions();
    let ax_granted = override_ax(overrides);
    // A differing build means the app was installed rather than relaunched. The recorded value is
    // refreshed on every launch so the next one compares against this build.
    let build = unsafe { crate::ffi::bundle_info_string("CFBundleVersion") };
    let new_install = !build.is_empty() && unsafe { defaults_get_string(BUILD_KEY) } != build;
    if !build.is_empty() {
        unsafe { defaults_set_string(BUILD_KEY, &build) };
    }
    let state = LaunchState {
        completed: completed(),
        forced,
        resume: resume_step.is_some(),
        sparkle_update: crate::update_notice::just_updated_via_sparkle(),
        new_install,
        ax_granted,
        config_exists,
        suppressed: is_suppressed(),
    };
    if !should_show_on_launch(state) {
        log_debug!(
            "[onboarding] skipped (completed={} forced={} resume={} sparkle_update={} new_install={} ax={} config={} suppressed={} build={})",
            state.completed,
            state.forced,
            state.resume,
            state.sparkle_update,
            state.new_install,
            state.ax_granted,
            state.config_exists,
            state.suppressed,
            build
        );
        return false;
    }
    log_debug!(
        "[onboarding] showing (forced={} resume={:?} sparkle_update={} new_install={} ax={} screen={} config={})",
        forced,
        resume_step,
        state.sparkle_update,
        state.new_install,
        ax_granted,
        override_screen(overrides),
        config_exists
    );
    // Mark as shown: no further nagging for this installation (a new build starts fresh because the
    // recorded build differs). Forced/development runs do not write the completion marker so the
    // flow stays repeatable.
    if !forced {
        mark_completed();
    }
    clear_resume();
    show_internal_at(overrides, resume_step.unwrap_or(0));
    true
}

/// Manual reopen (the settings window's About page entry): no marker reset, and no forced welcome
/// page.
pub(crate) fn show_manually() {
    show_internal(fake_permissions());
}

fn show_internal(overrides: PermissionOverride) {
    show_internal_at(overrides, 0);
}

/// Shows the guide at a step (0 = the first): a resumed launch reopens where the permission restart
/// interrupted the user instead of starting over.
fn show_internal_at(overrides: PermissionOverride, start_index: usize) {
    let config = CONFIG.read().unwrap().clone();
    let steps = steps_for();
    let index = start_index.min(steps.len().saturating_sub(1));
    let state = UiState {
        steps,
        index,
        overrides,
        launch_at_login: config.startup.launch_at_login,
        thumbnails_enabled: config.layout.thumbnails_enabled,
        clipboard_enabled: config.clipboard.enabled,
        clipboard_persist: config.clipboard.persist,
    };
    let (step_number, step_count) = (state.index + 1, state.steps.len());
    *STATE.lock().unwrap() = Some(state);
    unsafe { ensure_window() };
    render_current_step();
    // The guide is on screen now: record the step before anything can take the process down.
    note_resume_if_visible();
    start_tick_timer();
    log_debug!("[onboarding] step {}/{} shown", step_number, step_count);
}

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

/// Hides the guide window (reused by tests and teardown paths). Finishing, skipping, or opening
/// Settings ends the flow, so a pending resume is dropped: a later launch must not reopen it.
pub(crate) fn hide() {
    stop_tick_timer();
    clear_resume();
    if let Some(window) = *WINDOW.lock().unwrap() {
        let _: () = unsafe { msg_send![window.0, orderOut: std::ptr::null::<AnyObject>()] };
    }
}

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
    // Closing must not release it: the pointer stays in the static slot for reuse (same
    // convention as the settings window).
    let _: () = msg_send![window, setReleasedWhenClosed: false];
    let _: () = msg_send![window, center];
    let content: *mut AnyObject = msg_send![window, contentView];
    *CONTENT.lock().unwrap() = Some(ObjPtr::new(content));
    *WINDOW.lock().unwrap() = Some(ObjPtr::new(window));
}

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

unsafe fn add_menu_icon_label(
    content: *mut AnyObject,
    text: &str,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    style: LabelStyle,
) {
    let Some((before_icon, after_icon)) = text.split_once("{icon}") else {
        add_label(content, text, x, y, w, h, style);
        return;
    };

    let png_bytes: &[u8] = include_bytes!("../assets/statusbar-icon.png");
    let data: *mut AnyObject = msg_send![
        class!(NSData),
        dataWithBytes: png_bytes.as_ptr() as *const c_void,
        length: png_bytes.len()
    ];
    let image: *mut AnyObject = msg_send![class!(NSImage), alloc];
    let image: *mut AnyObject = msg_send![image, initWithData: data];
    if image.is_null() {
        let fallback = text.replace("{icon}", &t("onboarding.app_fallback_name"));
        add_label(content, &fallback, x, y, w, h, style);
        return;
    }

    let is_template = true;
    let _: () = msg_send![image, setTemplate: is_template];
    let attachment: *mut AnyObject = msg_send![class!(NSTextAttachment), alloc];
    let attachment: *mut AnyObject = msg_send![attachment, init];
    let _: () = msg_send![attachment, setImage: image];
    let _: () = msg_send![
        attachment,
        setBounds: NSRect::new(NSPoint::new(0.0, -2.0), NSSize::new(17.0, 14.0))
    ];
    let icon_text: *mut AnyObject = msg_send![
        class!(NSAttributedString),
        attributedStringWithAttachment: attachment
    ];
    release_obj(attachment);
    release_obj(image);

    let attributed: *mut AnyObject = msg_send![class!(NSMutableAttributedString), alloc];
    let empty = make_nsstring("");
    let attributed: *mut AnyObject = msg_send![attributed, initWithString: empty];
    release_obj(empty);

    for part in [before_icon, " "] {
        if !part.is_empty() {
            let value = make_nsstring(part);
            let attributed_part: *mut AnyObject = msg_send![class!(NSAttributedString), alloc];
            let attributed_part: *mut AnyObject = msg_send![attributed_part, initWithString: value];
            release_obj(value);
            let _: () = msg_send![attributed, appendAttributedString: attributed_part];
            release_obj(attributed_part);
        }
    }
    let _: () = msg_send![attributed, appendAttributedString: icon_text];
    if !after_icon.is_empty() {
        let value = make_nsstring(after_icon);
        let attributed_part: *mut AnyObject = msg_send![class!(NSAttributedString), alloc];
        let attributed_part: *mut AnyObject = msg_send![attributed_part, initWithString: value];
        release_obj(value);
        let _: () = msg_send![attributed, appendAttributedString: attributed_part];
        release_obj(attributed_part);
    }

    let field: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let field: *mut AnyObject = msg_send![
        field,
        initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    ];
    let _: () = msg_send![field, setAttributedStringValue: attributed];
    release_obj(attributed);
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
        let _: () = msg_send![field, setLineBreakMode: 0isize];
    } else {
        let _: () = msg_send![field, setUsesSingleLineMode: true];
        let _: () = msg_send![field, setLineBreakMode: 4isize];
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
    // Register the action id without touching the button's tag (see ACTION_TAGS).
    ACTION_TAGS.lock().unwrap().push((button as usize, tag));
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
            Step::ClipboardHistory => {
                render_clipboard_history(content, state.clipboard_enabled, state.clipboard_persist)
            }
            Step::MoreFeatures => render_more_features(content),
        }
        render_footer(content, state.index);
    }
    *LAST_SIGNATURE.lock().unwrap() = Some(permission_signature(&state));
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
        "onboarding.launch_label",
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

/// Clipboard-history step geometry (bottom-up, like the window's non-flipped content view). The
/// persist row and its hint keep this place whether or not history is enabled, so toggling the
/// master switch never moves the rows above it; they are only added to the view tree while history
/// is on.
struct ClipboardStepLayout {
    title_y: f64,
    body_y: f64,
    body_h: f64,
    enabled_row_y: f64,
    persist_row_y: f64,
    persist_hint_y: f64,
    persist_hint_h: f64,
}

const CLIPBOARD_STEP: ClipboardStepLayout = ClipboardStepLayout {
    // The title keeps the same y as the other steps so moving between steps does not shift it.
    title_y: 194.0,
    body_y: 150.0,
    // 40pt holds the two wrapped lines of the longest body copy (en/zh); 48 used to leave a gap
    // above the now-taller switch stack.
    body_h: 40.0,
    enabled_row_y: 116.0,
    persist_row_y: 78.0,
    persist_hint_y: 56.0,
    persist_hint_h: 16.0,
};

/// Switch-row label height, and the width the label leaves for the switch on the trailing edge.
const SWITCH_ROW_LABEL_H: f64 = 28.0;
const SWITCH_ROW_LABEL_TRAILING: f64 = 58.0;

unsafe fn render_clipboard_history(content: *mut AnyObject, enabled: bool, persist: bool) {
    add_label(
        content,
        &t("onboarding.clipboard_title"),
        PAD,
        CLIPBOARD_STEP.title_y,
        WINDOW_W - PAD * 2.0,
        TITLE_H,
        TITLE_STYLE,
    );
    add_label(
        content,
        &t(if enabled {
            "onboarding.clipboard_body_enabled"
        } else {
            "onboarding.clipboard_body"
        }),
        PAD,
        CLIPBOARD_STEP.body_y,
        WINDOW_W - PAD * 2.0,
        CLIPBOARD_STEP.body_h,
        BODY_STYLE,
    );
    add_switch_row(
        content,
        CLIPBOARD_STEP.enabled_row_y,
        enabled,
        ACTION_TOGGLE_CLIPBOARD_DRAFT,
        "onboarding.clipboard_label",
    );
    // The persist options exist only while history is on. The draft keeps its value while the row
    // is hidden, so turning history back on restores the previous choice instead of resetting it.
    if enabled {
        add_switch_row(
            content,
            CLIPBOARD_STEP.persist_row_y,
            persist,
            ACTION_TOGGLE_CLIPBOARD_PERSIST,
            "onboarding.clipboard_persist_label",
        );
        add_label(
            content,
            &t("onboarding.clipboard_persist_body"),
            PAD,
            CLIPBOARD_STEP.persist_hint_y,
            WINDOW_W - PAD * 2.0,
            CLIPBOARD_STEP.persist_hint_h,
            HINT_STYLE,
        );
    }
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
    add_menu_icon_label(
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
    label_key: &str,
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
    let accessibility_label = make_nsstring(&t(label_key));
    let _: () = msg_send![switch, setAccessibilityLabel: accessibility_label];
    release_obj(accessibility_label);
    let _: () = msg_send![content, addSubview: switch];
    release_obj(switch);
}

/// One switch row: the label on the leading edge and the switch on the trailing edge, both placed
/// from the row's bottom edge so the caller positions rows on one shared grid.
unsafe fn add_switch_row(
    content: *mut AnyObject,
    row_y: f64,
    checked: bool,
    action_tag: isize,
    label_key: &str,
) {
    add_label(
        content,
        &t(label_key),
        PAD,
        row_y + (BUTTON_H - SWITCH_ROW_LABEL_H) / 2.0,
        WINDOW_W - PAD * 2.0 - SWITCH_ROW_LABEL_TRAILING,
        SWITCH_ROW_LABEL_H,
        TITLE_STYLE,
    );
    add_switch(
        content,
        WINDOW_W - PAD,
        row_y,
        checked,
        action_tag,
        label_key,
    );
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
        ACTION_ALLOW_SCREEN => {
            // Ask the system first: that request is what registers the app in the Screen Recording
            // list and shows the one-time prompt, so the Settings pane no longer needs a manual
            // "+". The pane stays the fallback for a user who already declined the prompt (or
            // toggles the switch manually).
            request_screen_recording_from_guide();
            if !crate::thumbnail::capture_allowed() {
                crate::open_privacy_screen_recording();
            }
        }
        ACTION_TOGGLE_LAUNCH_DRAFT => {
            if let Some(state) = STATE.lock().unwrap().as_mut() {
                state.launch_at_login = !state.launch_at_login;
            }
            render_current_step();
        }
        ACTION_TOGGLE_CLIPBOARD_DRAFT => {
            apply_clipboard_choice(tag, |state| {
                state.clipboard_enabled = !state.clipboard_enabled;
            });
        }
        ACTION_TOGGLE_CLIPBOARD_PERSIST => {
            apply_clipboard_choice(tag, |state| {
                state.clipboard_persist = !state.clipboard_persist;
            });
        }
        ACTION_SELECT_ICONS | ACTION_SELECT_THUMBNAILS => {
            let thumbnails_enabled = tag == ACTION_SELECT_THUMBNAILS;
            apply_choice_now(Step::DisplayMode, |state| {
                state.thumbnails_enabled = thumbnails_enabled;
            });
            if thumbnails_enabled {
                // Picking thumbnails is the contextual moment to ask for Screen Recording: the
                // system prompt (and the list entry it creates) then follows the user's own choice
                // instead of surprising them at launch or arriving only on the first summon.
                request_screen_recording_from_guide();
            }
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
    note_resume_if_visible();
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
    note_resume_if_visible();
}

/// Asks for Screen Recording after the user picked thumbnails or pressed the grant button. Never
/// while the DISPLAYED permission is faked: `--fake-permissions` walks the guide's branches without
/// touching TCC. `apply_choice_now` has already re-rendered (which activates the window), so the
/// system prompt is not left behind other windows.
fn request_screen_recording_from_guide() {
    if fake_permissions().screen.is_some() {
        return;
    }
    crate::thumbnail::request_screen_recording_from_guide();
}

/// Applies a guide choice immediately instead of waiting for Next: the switcher (display mode) and
/// the Option+V hotkey (clipboard history) both read the live `CONFIG`, so a deferred draft would
/// make the step's own instructions do nothing. The draft is updated first so a re-render keeps the
/// selection; `commit_step_selection` no-ops when the value already matches.
fn apply_choice_now(step: Step, update: impl FnOnce(&mut UiState)) {
    update_draft_and_commit(step, update);
    render_current_step();
}

/// Whether a clipboard-step choice changes the page's visible structure and therefore needs the
/// page rebuilt. The master switch shows/hides the persist row; the persist switch only changes its
/// own value, which `html_switch_mouse_down:` has already animated, so rebuilding would recreate
/// every switch and replay the checked master switch's off-to-on spring (the reported twitch).
fn clipboard_choice_rebuilds_page(action_tag: isize) -> bool {
    !matches!(action_tag, ACTION_TOGGLE_CLIPBOARD_PERSIST)
}

/// Applies a clipboard-step choice, rebuilding the page only when the choice changes what is on it.
fn apply_clipboard_choice(action_tag: isize, update: impl FnOnce(&mut UiState)) {
    update_draft_and_commit(Step::ClipboardHistory, update);
    if clipboard_choice_rebuilds_page(action_tag) {
        render_current_step();
    }
}

/// Updates the draft and commits it to the live config; callers decide whether the page is redrawn.
fn update_draft_and_commit(step: Step, update: impl FnOnce(&mut UiState)) {
    {
        let mut slot = STATE.lock().unwrap();
        let Some(state) = slot.as_mut() else {
            return;
        };
        update(state);
    }
    commit_step_selection(step);
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
        Step::ClipboardHistory => {
            new.clipboard.enabled = state.clipboard_enabled;
            new.clipboard.persist = state.clipboard_persist;
        }
        Step::MoreFeatures => {}
    }
    new
}

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

/// Dispatches the first control carrying the given action tag through the production selector
/// exactly as a click does: look the control up by its tag instead of keeping a second pointer.
unsafe fn dispatch_action_with_tag(tag: isize) -> Option<*mut AnyObject> {
    ACTION_TAGS
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|(_, candidate)| *candidate == tag)
        .map(|(pointer, _)| *pointer as *mut AnyObject)
}

/// Drives the display-mode segmented control: select the segment, then send the action.
unsafe fn dispatch_display_mode_segment(segment: isize) -> bool {
    let Some(control) = dispatch_action_with_tag(ACTION_DISPLAY_MODE) else {
        return false;
    };
    let _: () = msg_send![control, setSelectedSegment: segment];
    on_action(std::ptr::null_mut(), sel!(handleOnboardingAction:), control);
    true
}

/// GUI smoke runner (`--smoke-onboarding-live-apply`): show the guide, drive the real display-mode
/// segment and the clipboard switches (master + persist), and require every choice to reach the
/// live config without a Next click. Also asserts that the persist row only exists while history is
/// on and that the smoke run never asked for Screen Recording (a real run asks when thumbnails are
/// picked). Runs as a subprocess on the real AppKit main thread (see the ignored test below).
pub(crate) fn live_apply_smoke_runner() -> bool {
    let before_thumbnails = CONFIG.read().unwrap().layout.thumbnails_enabled;
    show_internal(PermissionOverride::default());
    // The resume marker must exist while the guide is on screen (an unexpected exit never delivers
    // the termination notification, and the completion marker alone would swallow the guide for
    // good), and it must follow the step.
    let resume_after_show = peeking_resume_step() == Some(0);
    // Step 1 -> 2. Committing step 1 is a no-op here: its draft mirrors the loaded config.
    advance();
    let resume_after_advance = peeking_resume_step() == Some(1);
    // Segment 0 = icons only, segment 1 = icons and thumbnails; pick the one that flips the value.
    let segment = if before_thumbnails { 0isize } else { 1isize };
    let expected_thumbnails = segment == 1;
    let segment_dispatched = unsafe { dispatch_display_mode_segment(segment) };
    let thumbnails_applied =
        CONFIG.read().unwrap().layout.thumbnails_enabled == expected_thumbnails;
    let thumbnails_draft = STATE
        .lock()
        .unwrap()
        .as_ref()
        .map(|state| state.thumbnails_enabled);

    // Step 2 -> 3. The persist row only exists while history is on, so bring the master switch to a
    // known ON state first: the loaded config may start either way (the ignored test runs both), and
    // a blind toggle from ON would turn it off and remove the very row this checks.
    advance();
    let before_clipboard = CONFIG.read().unwrap().clipboard.enabled;
    let master_switch_present =
        unsafe { dispatch_action_with_tag(ACTION_TOGGLE_CLIPBOARD_DRAFT).is_some() };
    let switch_dispatched = if before_clipboard {
        // Already on: the switch rendered checked from the draft, so there is nothing to drive.
        master_switch_present
    } else {
        unsafe {
            match dispatch_action_with_tag(ACTION_TOGGLE_CLIPBOARD_DRAFT) {
                Some(control) => {
                    let _: () = msg_send![control, setState: 1isize];
                    on_action(std::ptr::null_mut(), sel!(handleOnboardingAction:), control);
                    true
                }
                None => false,
            }
        }
    };
    let master_on = CONFIG.read().unwrap().clipboard.enabled;
    let clipboard_draft = STATE
        .lock()
        .unwrap()
        .as_ref()
        .map(|state| state.clipboard_enabled);
    // With history on, the persist row exists: flipping it must reach the live config and the draft,
    // and its accessibility label is the only per-row identity (all switches share one action), so
    // it must be the persist label.
    let before_persist = CONFIG.read().unwrap().clipboard.persist;
    let mut persist_ax_ok = false;
    let persist_dispatched = unsafe {
        match dispatch_action_with_tag(ACTION_TOGGLE_CLIPBOARD_PERSIST) {
            Some(control) => {
                let label: *mut AnyObject = msg_send![control, accessibilityLabel];
                persist_ax_ok =
                    crate::ffi::nsstring_to_rust(label) == t("onboarding.clipboard_persist_label");
                let _: () = msg_send![control, setState: isize::from(!before_persist)];
                on_action(std::ptr::null_mut(), sel!(handleOnboardingAction:), control);
                true
            }
            None => false,
        }
    };
    let persist_expected = !before_persist;
    let persist_applied = CONFIG.read().unwrap().clipboard.persist == persist_expected;
    let persist_draft = STATE
        .lock()
        .unwrap()
        .as_ref()
        .map(|state| state.clipboard_persist);
    // Turning history off must remove the persist row from the action map while the draft keeps the
    // choice, so re-enabling restores it.
    let disable_dispatched = unsafe {
        match dispatch_action_with_tag(ACTION_TOGGLE_CLIPBOARD_DRAFT) {
            Some(control) => {
                let _: () = msg_send![control, setState: 0isize];
                on_action(std::ptr::null_mut(), sel!(handleOnboardingAction:), control);
                true
            }
            None => false,
        }
    };
    let master_off = !CONFIG.read().unwrap().clipboard.enabled;
    let persist_hidden_when_disabled =
        unsafe { dispatch_action_with_tag(ACTION_TOGGLE_CLIPBOARD_PERSIST).is_none() };
    let persist_draft_retained = STATE
        .lock()
        .unwrap()
        .as_ref()
        .map(|state| state.clipboard_persist);
    // Picking thumbnails asks the system for Screen Recording in a real run; a smoke run must not
    // have asked (see the `--smoke` guard in thumbnail::request_permission_once).
    let permission_untouched = !crate::thumbnail::permission_prompted();
    let window_shown = is_visible();
    hide();
    // Finishing/skipping ends the flow: no pending resume may survive it.
    let resume_cleared = peeking_resume_step().is_none();
    log_info!(
        "[smoke-onboarding-live-apply] segment_dispatched={} thumbnails_applied={} thumbnails_draft={:?} master_switch_present={} switch_dispatched={} master_on={} clipboard_draft={:?} persist_dispatched={} persist_applied={} persist_draft={:?} persist_ax_ok={} disable_dispatched={} master_off={} persist_hidden_when_disabled={} persist_draft_retained={:?} permission_untouched={} resume_after_show={} resume_after_advance={} resume_cleared={} window={}",
        segment_dispatched,
        thumbnails_applied,
        thumbnails_draft,
        master_switch_present,
        switch_dispatched,
        master_on,
        clipboard_draft,
        persist_dispatched,
        persist_applied,
        persist_draft,
        persist_ax_ok,
        disable_dispatched,
        master_off,
        persist_hidden_when_disabled,
        persist_draft_retained,
        permission_untouched,
        resume_after_show,
        resume_after_advance,
        resume_cleared,
        window_shown
    );
    segment_dispatched
        && thumbnails_applied
        && thumbnails_draft == Some(expected_thumbnails)
        && master_switch_present
        && switch_dispatched
        && master_on
        && clipboard_draft == Some(true)
        && persist_dispatched
        && persist_applied
        && persist_draft == Some(persist_expected)
        && persist_ax_ok
        && disable_dispatched
        && master_off
        && persist_hidden_when_disabled
        && persist_draft_retained == Some(persist_expected)
        && permission_untouched
        && resume_after_show
        && resume_after_advance
        && resume_cleared
        && window_shown
}

const WINDOW_W: f64 = 520.0;
const WINDOW_H: f64 = 280.0;
const TITLE_H: f64 = 24.0;
const WINDOW_STYLE_TITLED: u64 = 1;
const PAD: f64 = 24.0;
const GAP: f64 = 10.0;
const BUTTON_H: f64 = 32.0;
const BUTTON_W: f64 = 132.0;
const BUTTON_Y: f64 = 20.0;
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
/// Small muted explanatory text under an option (e.g. the clipboard persist warning).
const HINT_STYLE: LabelStyle = LabelStyle {
    size: 11.5,
    weight: 0.0,
    color: 0x8E8E93FF,
    wrap: true,
};
fn status_style(color: u32) -> LabelStyle {
    LabelStyle {
        size: 13.5,
        weight: 0.0,
        color,
        wrap: false,
    }
}

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
        // Unknown keys and invalid values are ignored, keeping None (use the real state).
        let over = parse_fake_permissions("ax:maybe,sr:off,zz:1");
        assert_eq!(over.ax, None);
        assert_eq!(over.screen, Some(false));
        // Empty input and fragments without a colon must not panic.
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
            clipboard_persist: true,
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
        assert!(after_clipboard.clipboard.persist);
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

    /// The persist switch mirrors `config.clipboard.persist`: the clipboard step commits it, other
    /// steps leave it alone, and a disabled master switch keeps the draft so re-enabling history
    /// restores the previous choice instead of resetting it.
    #[test]
    fn onboarding_clipboard_step_commits_persist_and_keeps_it_while_disabled() {
        let old = Config::default();
        assert!(!old.clipboard.persist);
        let draft = UiState {
            steps: steps_for(),
            index: 0,
            overrides: PermissionOverride::default(),
            launch_at_login: false,
            thumbnails_enabled: false,
            clipboard_enabled: false,
            clipboard_persist: true,
        };
        let disabled = config_with_step_selection(&old, &draft, Step::ClipboardHistory);
        assert!(!disabled.clipboard.enabled);
        assert!(
            disabled.clipboard.persist,
            "the persist draft must survive a master switch that is off"
        );
        // A step that owns no clipboard field must not leak the draft into the config.
        let display = config_with_step_selection(&old, &draft, Step::DisplayMode);
        assert_eq!(display.clipboard.persist, old.clipboard.persist);
        assert_eq!(display.clipboard.enabled, old.clipboard.enabled);
    }

    /// The clipboard page's rows must not overlap: the persist hint clears the footer buttons, the
    /// persist row sits above the hint, the master row above the persist row, and the body above
    /// the master row. Kept as a pure check so a layout edit fails without a GUI.
    #[test]
    fn clipboard_step_rows_stay_ordered_and_clear_the_footer() {
        let step = CLIPBOARD_STEP;
        assert!(
            step.persist_hint_y > BUTTON_Y + BUTTON_H,
            "the persist hint must clear the footer buttons"
        );
        assert!(step.persist_row_y >= step.persist_hint_y + step.persist_hint_h);
        assert!(step.enabled_row_y >= step.persist_row_y + BUTTON_H);
        assert!(step.body_y >= step.enabled_row_y + BUTTON_H);
        assert!(step.title_y >= step.body_y + step.body_h);
        assert!(
            step.title_y + TITLE_H <= WINDOW_H - 34.0,
            "the title must clear the step counter pinned near the top edge"
        );
    }

    /// Toggling clipboard persistence must not rebuild the page: the clicked switch has already
    /// animated its own knob, and a rebuild recreates the master switch, whose `setState:` runs the
    /// off-to-on spring on a fresh button (the reported twitch). The master switch still rebuilds so
    /// the persist row appears/disappears with it.
    #[test]
    fn clipboard_choices_only_rebuild_the_page_when_their_structure_changes() {
        assert!(clipboard_choice_rebuilds_page(
            ACTION_TOGGLE_CLIPBOARD_DRAFT
        ));
        assert!(!clipboard_choice_rebuilds_page(
            ACTION_TOGGLE_CLIPBOARD_PERSIST
        ));
    }

    /// Builds one launch situation; each test names only the fields its story is about so the
    /// remaining ones stay at their "nothing recorded / not forced" defaults.
    fn launch(completed: bool, new_install: bool, sparkle_update: bool) -> LaunchState {
        LaunchState {
            completed,
            new_install,
            sparkle_update,
            ..LaunchState::default()
        }
    }

    #[test]
    fn every_install_walks_the_guide_but_a_sparkle_update_does_not() {
        // Fresh install: nothing recorded yet.
        assert!(should_show_on_launch(launch(false, true, false)));
        // Manual re-install over a completed guide: the build changed, so the guide comes back.
        assert!(should_show_on_launch(launch(true, true, false)));
        // Existing user (AX granted + config present) reinstalling manually still sees it.
        assert!(should_show_on_launch(LaunchState {
            ax_granted: true,
            config_exists: true,
            ..launch(true, true, false)
        }));
        // Sparkle in-place update: updating is not installing, so no nagging.
        assert!(!should_show_on_launch(launch(true, true, true)));
        // Relaunching the same installation after it was shown: no repetition.
        assert!(!should_show_on_launch(launch(true, false, false)));
        // Same installation, guide never completed, but an existing user with a config: still silent.
        assert!(!should_show_on_launch(LaunchState {
            ax_granted: true,
            config_exists: true,
            ..LaunchState::default()
        }));
        // Permission granted but no config file (freshly cleared, say) still needs the guide.
        assert!(should_show_on_launch(LaunchState {
            ax_granted: true,
            ..LaunchState::default()
        }));
    }

    #[test]
    fn a_restart_resumes_the_guide_and_suppression_wins_over_everything_but_force() {
        // Restarted mid-guide for a permission: the resume beats completed + existing user.
        assert!(should_show_on_launch(LaunchState {
            resume: true,
            ax_granted: true,
            config_exists: true,
            ..launch(true, false, false)
        }));
        // Suppression (smoke / --no-onboarding) beats a resume and a new install.
        assert!(!should_show_on_launch(LaunchState {
            suppressed: true,
            resume: true,
            ..launch(false, true, false)
        }));
        // --force-onboarding still shows regardless.
        assert!(should_show_on_launch(LaunchState {
            forced: true,
            suppressed: true,
            ..launch(true, false, false)
        }));
    }

    /// Picking a display mode or enabling clipboard history in the guide must reach the live
    /// config on the click, not on Next: a user who tested with the hotkey right after picking it
    /// used to keep the old mode, and the step's "try Option+V" hint would do nothing. The runner
    /// commits through the real apply path, which persists, so it gets a throwaway HOME. It is run
    /// once with clipboard history off and once with it on, because the persist row only exists
    /// while history is on and the runner must not assume the loaded config starts off.
    #[test]
    #[ignore]
    fn onboarding_live_apply_smoke() {
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
        for clipboard_enabled in [false, true] {
            let home = std::env::temp_dir().join(format!(
                "oh-my-tab-onboarding-smoke-{}-{clipboard_enabled}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&home);
            let config_dir = home.join(".config/oh-my-tab");
            std::fs::create_dir_all(&config_dir).expect("create smoke config dir");
            std::fs::write(
                config_dir.join("config.toml"),
                format!("[clipboard]\nenabled = {clipboard_enabled}\n"),
            )
            .expect("write smoke config");
            let out = std::process::Command::new(&app)
                .arg("--smoke-onboarding-live-apply")
                .env("HOME", &home)
                .output()
                .expect("failed to spawn app");
            let _ = std::fs::remove_dir_all(&home);
            assert!(
                out.status.success(),
                "onboarding live-apply smoke failed with clipboard.enabled={clipboard_enabled} (exit {:?})\nstdout:\n{}\nstderr:\n{}",
                out.status.code(),
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}
