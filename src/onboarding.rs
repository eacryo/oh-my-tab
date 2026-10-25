//! First-run onboarding: a small standalone window that walks through permissions and login,
//! overlay display mode, and pointers to the other features.
//!
//! Why it exists: this is a menu-bar app (LSUIElement), so a fresh install shows nothing but the
//! status item (and an alert when Accessibility is missing) and never explains what the app does,
//! which key to press, or which permission it needs.
//!
//! Design: never blocking (permissions can be deferred, the display-mode and clipboard-history
//! choices take effect the moment they are picked because the switcher and clipboard hotkey read
//! the live config, and launch at login applies on Next); four fixed steps; auto-shown once (the
//! UserDefaults marker is written as soon as it appears, and the status item's "Welcome" entry or a
//! development switch reopens it -- a missing permission stays covered by the startup alert);
//! and verifiable (`--force-onboarding` ignores the marker,
//! `--onboarding=reset` clears it first, and `--fake-permissions=ax:0,sr:1` -- debug
//! builds only -- fakes the DISPLAYED permission state so every branch can be walked without
//! touching TCC).

use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSPoint, NSRange, NSRect, NSSize};
use std::ffi::c_void;

use crate::callback_guard;
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
const ACTION_SELECT_MODIFIER: isize = 20;

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
    /// Draft index into `SWITCHER_MODIFIER_VALUES` for the switcher shortcut. Like every other
    /// choice on the step it previews and `advance()` commits it.
    modifier: usize,
    clipboard_enabled: bool,
    clipboard_shortcut: String,
}

static WINDOW: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
static CONTENT: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
static TIMER: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
static STATE: MainThreadSlot<Option<UiState>> = unsafe { MainThreadSlot::new(None) };
/// The permission signature rendered last; a 1s tick rebuilds only when it changes.
static LAST_SIGNATURE: MainThreadSlot<Option<(bool, bool, bool)>> =
    unsafe { MainThreadSlot::new(None) };
/// Button pointer to action tag. The action id must NOT ride `setTag:`: the SettingsButton hover
/// handlers select their palette from that tag (`mouseEntered:` and `mouseExited:` both read it,
/// mapping to the primary-blue / action / compact-grey palettes), so overwriting it breaks both the
/// hover colour and the restore-to-normal colour (measured: the primary button stayed grey after
/// the pointer left).
static ACTION_TAGS: MainThreadSlot<Vec<(usize, isize)>> =
    unsafe { MainThreadSlot::new(Vec::new()) };

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
        modifier: crate::config::switcher_modifier_index_of(&config.keyboard.modifier),
        clipboard_enabled: config.clipboard.enabled,
        clipboard_shortcut: config.clipboard.shortcut.clone(),
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

/// Keep the visible clipboard step's displayed binding in sync with config reloads without
/// exposing a setting choice in onboarding.
pub(crate) fn refresh_clipboard_shortcut(shortcut: &str) {
    crate::debug_assert_main_thread();
    let should_render = {
        let mut state = STATE.lock().unwrap();
        let Some(state) = state.as_mut() else {
            return;
        };
        state.clipboard_shortcut = shortcut.to_string();
        state.steps.get(state.index) == Some(&Step::ClipboardHistory)
    };
    if should_render && is_visible() {
        render_current_step();
    }
}

pub(crate) fn is_visible() -> bool {
    let Some(window) = *WINDOW.lock().unwrap() else {
        return false;
    };
    let visible: bool = unsafe { msg_send![window.0, isVisible] };
    visible
}

/// Complete the guide and dismiss it before handing the user to Settings.
pub(crate) fn finish_for_settings() {
    mark_completed();
    hide();
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
        styleMask: WINDOW_STYLE_MASK,
        backing: 2u64,
        defer: false
    ];
    let title = make_nsstring(&t("onboarding.window_title"));
    let _: () = msg_send![window, setTitle: title];
    release_obj(title);
    // Closing must not release it: the pointer stays in the static slot for reuse (same
    // convention as the settings window).
    let _: () = msg_send![window, setReleasedWhenClosed: false];
    // Custom chrome (the reference draws its own band and footer): transparent title bar, hidden
    // title, full-height content. Same approach as the settings window; AppKit keeps the window
    // title bar buttons functional and drawn above everything the app paints.
    let _: () = msg_send![window, setTitlebarAppearsTransparent: true];
    let _: () = msg_send![window, setStyleMask: WINDOW_STYLE_MASK | STYLE_MASK_FULL_SIZE_CONTENT];
    let _: () = msg_send![window, setTitleVisibility: WINDOW_TITLE_HIDDEN];
    // The app paints the window background itself, so AppKit's default keeps the desktop diff
    // from showing through the (unchanged) window shadow.
    let _: () = msg_send![window, setOpaque: false];
    // The guide draws its own band and hides the standard window buttons, so without this the
    // window would have no visible drag affordance at all: the empty background is the drag
    // surface (the settings window's own convention in settings/window.rs). Controls keep
    // consuming their own clicks; only untouched background (band, cards, page margins) drags.
    let _: () = msg_send![window, setMovableByWindowBackground: true];

    // The reference's dialog shows no traffic lights: hide the three standard window buttons.
    // Hidden buttons keep the native title-bar drag strip; `band_leading_edge` skips hidden
    // buttons so the leading row stays on the mock's 18pt.
    for tag in 0isize..3 {
        let button: *mut AnyObject = msg_send![window, standardWindowButton: tag];
        if !button.is_null() {
            let _: () = msg_send![button, setHidden: true];
        }
    }
    let _: () = msg_send![window, center];
    let content: *mut AnyObject = msg_send![window, contentView];
    *CONTENT.lock().unwrap() = Some(ObjPtr::new(content));
    *WINDOW.lock().unwrap() = Some(ObjPtr::new(window));
}

/// Paint the window surface: `window_bg` fill clipped to `radius-panel` (the settings window's
/// own root-surface convention). Called once per step render so a theme change while the guide
/// is open repaints in the same pass as the content.
unsafe fn paint_window_surface(content: *mut AnyObject) {
    let palette = crate::theme::ui_palette();
    let _: () = msg_send![content, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![content, layer];
    if layer.is_null() {
        return;
    }
    crate::ffi::layer_set_background(layer, crate::ffi::hex_to_cg_color(palette.window_bg));
    let _: () = msg_send![layer, setCornerRadius: crate::theme::RADIUS_PANEL];
    let _: () = msg_send![layer, setMasksToBounds: true];
}

/// Removes every subview of the content view (each step rebuilds its content, like the clipboard
/// picker does).
/// Apply the app's resolved theme to the guide window. Without this the window inherits
/// the SYSTEM appearance while the step content is painted from the app's resolved
/// palette, so a config-forced theme diverging from the system shows a light window with
/// dark content (or the reverse). Called at window creation, on every step render, and
/// from the global theme refresh.
pub(crate) fn apply_window_appearance() {
    crate::debug_assert_main_thread();
    let Some(window) = *WINDOW.lock().unwrap() else {
        return;
    };
    unsafe {
        let name = make_nsstring(if crate::theme::resolved_is_dark() {
            "NSAppearanceNameDarkAqua"
        } else {
            "NSAppearanceNameAqua"
        });
        let appearance: *mut AnyObject = msg_send![class!(NSAppearance), appearanceNamed: name];
        release_obj(name);
        if !appearance.is_null() {
            let _: () = msg_send![window.0, setAppearance: appearance];
        }
    }
}

unsafe fn clear_content(content: *mut AnyObject) {
    let subviews: *mut AnyObject = msg_send![content, subviews];
    if subviews.is_null() {
        return;
    }
    let count: isize = msg_send![subviews, count];
    for index in 0..count {
        let view: *mut AnyObject = msg_send![subviews, objectAtIndex: index];
        // A select registers itself in a global table by address; forgetting it before the view goes
        // away is what keeps the next sweep from sending `window` to a freed address.
        crate::settings::select::forget_settings_select(view);
        let _: () = msg_send![view, removeFromSuperview];
    }
}

/// Height `text` needs when wrapped inside `width` at the style's font and line height (AppKit
/// measurement, no window required; runs on the main thread during a step render). Used to size
/// the lede and the notes so the card below them keeps its exact 24pt gap instead of leaving the
/// space a fixed three-line reserve would.
///
/// The paragraph carries the same explicit line height the drawn field gets through
/// `ffi::set_text_field_line_height`, so measurement and drawing agree.
unsafe fn measured_text_height(text: &str, width: f64, style: LabelStyle) -> f64 {
    use crate::ffi::CFRelease;
    let attributed = attributed_string(text, style, true);
    let bounds: NSRect = msg_send![
        attributed,
        boundingRectWithSize: NSSize::new(width, f64::MAX),
        options: 0x1u64 | 0x40u64, // usesLineFragmentOrigin | usesFontLeading
    ];
    CFRelease(attributed as *const c_void);
    bounds.size.height.ceil()
}

/// Width `text` needs on one line at the style's font, through AppKit's own `cellSize`. The
/// cell's drawn width includes the small inset the cell lays its text out with — a raw
/// glyph-advance measurement (attributed boundingRect) came back short of the drawn cell, and a
/// truncated band title ("欢迎…" at exactly the measured width) was the result. Used to place
/// centered band chrome and right-aligned trailing elements by geometry instead of alignment
/// constants, whose numeric meaning differs between AppKit generations (see the settings
/// titlebar note).
unsafe fn single_line_text_width(text: &str, style: LabelStyle) -> f64 {
    let field: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let field: *mut AnyObject = msg_send![
        field,
        initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(10000.0, 1.0))
    ];
    let value = make_nsstring(text);
    let _: () = msg_send![field, setStringValue: value];
    release_obj(value);
    let _: () = msg_send![field, setBezeled: false];
    let _: () = msg_send![field, setDrawsBackground: false];
    let _: () = msg_send![field, setBordered: false];
    let _: () = msg_send![field, setEditable: false];
    let _: () = msg_send![field, setSelectable: false];
    let _: () = msg_send![field, setUsesSingleLineMode: true];
    let font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: style.size, weight: style.weight];
    let _: () = msg_send![field, setFont: font];
    let cell: *mut AnyObject = msg_send![field, cell];
    let size: NSSize = msg_send![cell, cellSize];
    release_obj(field);
    size.width.ceil()
}

/// Attributed string of `text` at the style's font; with `line_height` the paragraph pins the
/// style's drawn line height (min = max), without it the natural font leading is measured.
unsafe fn attributed_string(text: &str, style: LabelStyle, line_height: bool) -> *mut AnyObject {
    let value = make_nsstring(text);
    let attributed: *mut AnyObject = msg_send![class!(NSMutableAttributedString), alloc];
    let attributed: *mut AnyObject = msg_send![attributed, initWithString: value];
    release_obj(value);
    let font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: style.size, weight: style.weight];
    let font_key = make_nsstring("NSFont");
    let length: usize = msg_send![attributed, length];
    let _: () = msg_send![
        attributed,
        addAttribute: font_key,
        value: font,
        range: NSRange::new(0, length)
    ];
    release_obj(font_key);
    if line_height {
        let paragraph: *mut AnyObject = msg_send![class!(NSMutableParagraphStyle), alloc];
        let paragraph: *mut AnyObject = msg_send![paragraph, init];
        let line = crate::theme::line_height(style.size, wrapped_line_height_ratio(style.size));
        let _: () = msg_send![paragraph, setMinimumLineHeight: line];
        let _: () = msg_send![paragraph, setMaximumLineHeight: line];
        let _: () = msg_send![paragraph, setLineBreakMode: 0isize]; // NSLineBreakByWordWrapping
        let paragraph_key = make_nsstring("NSParagraphStyle");
        let _: () = msg_send![
            attributed,
            addAttribute: paragraph_key,
            value: paragraph,
            range: NSRange::new(0, length)
        ];
        release_obj(paragraph_key);
        release_obj(paragraph);
    }
    attributed
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
    let _: () = msg_send![field, setTextColor: hex_to_ns_color(style.color.resolve())];
    if style.wrap {
        let cell: *mut AnyObject = msg_send![field, cell];
        let _: () = msg_send![cell, setWraps: true];
        let _: () = msg_send![cell, setUsesSingleLineMode: false];
        let _: () = msg_send![field, setUsesSingleLineMode: false];
        let _: () = msg_send![field, setLineBreakMode: 0isize]; // NSLineBreakByWordWrapping
        crate::ffi::set_text_field_line_height(
            field,
            crate::theme::line_height(style.size, wrapped_line_height_ratio(style.size)),
        );
    } else {
        let _: () = msg_send![field, setUsesSingleLineMode: true];
        let _: () = msg_send![field, setLineBreakMode: 4isize]; // NSLineBreakByTruncatingTail
    }
    let _: () = msg_send![content, addSubview: field];
    release_obj(field);
}

fn wrapped_line_height_ratio(font_size: f64) -> f64 {
    if font_size <= crate::theme::FONT_CAPTION {
        crate::theme::LINE_HEIGHT_CAPTION_RATIO
    } else if font_size >= crate::theme::FONT_PAGE_TITLE {
        crate::theme::LINE_HEIGHT_TITLE_RATIO
    } else {
        crate::theme::LINE_HEIGHT_BODY_RATIO
    }
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

    let Some(icon_text) = inline_icon_attachment_run(style) else {
        let fallback = text.replace("{icon}", &t("onboarding.app_fallback_name"));
        add_label(content, &fallback, x, y, w, h, style);
        return;
    };

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
    release_obj(icon_text);
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
    let _: () = msg_send![field, setTextColor: hex_to_ns_color(style.color.resolve())];
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

/// One-character attributed run holding the menu-bar icon attachment, baked in the
/// label's exact color and cropped to the glyph. Two asset facts drive the geometry
/// (measured alpha bounding box of `statusbar-icon.png`, 162x128): the glyph occupies
/// only (39,22)-(124,107) — about 24% dead margin per side — and TextKit 1 does NOT tint
/// template attachment images with the foreground-color attribute (that only works for
/// text glyphs), so a raw template draws its black pixels verbatim, dark-on-dark in dark
/// mode. The builder therefore draws only the glyph box into a square image and composites
/// the tint there (same SourceAtop trick as the overlay's grayed_image).
/// `None` when the bundled icon fails to load.
unsafe fn inline_icon_attachment_run(style: LabelStyle) -> Option<*mut AnyObject> {
    // Glyph box in the asset's point coordinates; y is flipped (bottom-left origin).
    const GLYPH_RECT: NSRect = NSRect::new(NSPoint::new(39.0, 21.0), NSSize::new(85.0, 85.0));
    /// The rendered icon side, equal to the body text size so the mark reads as part of
    /// the sentence instead of a shrunken footnote.
    const ICON_SIDE: f64 = crate::theme::FONT_CONTROL;

    let png_bytes: &[u8] = include_bytes!("../assets/statusbar-icon.png");
    let data: *mut AnyObject = msg_send![
        class!(NSData),
        dataWithBytes: png_bytes.as_ptr() as *const c_void,
        length: png_bytes.len()
    ];
    let original: *mut AnyObject = msg_send![class!(NSImage), alloc];
    let original: *mut AnyObject = msg_send![original, initWithData: data];
    if original.is_null() {
        return None;
    }
    let tinted: *mut AnyObject = msg_send![class!(NSImage), alloc];
    let tinted: *mut AnyObject =
        msg_send![tinted, initWithSize: NSSize::new(GLYPH_RECT.size.width, GLYPH_RECT.size.height)];
    if tinted.is_null() {
        release_obj(original);
        return None;
    }
    let _: () = msg_send![tinted, lockFocus];
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), GLYPH_RECT.size);
    let _: () = msg_send![
        original,
        drawInRect: rect,
        fromRect: GLYPH_RECT,
        operation: 2isize, // SourceOver
        fraction: 1.0f64
    ];
    // SourceAtop (= 5): the fill lands only where the icon already has alpha.
    let ctx: *mut AnyObject = msg_send![class!(NSGraphicsContext), currentContext];
    let _: () = msg_send![ctx, setCompositingOperation: 5isize];
    let fill = hex_to_ns_color(style.color.resolve());
    let _: () = msg_send![fill, setFill];
    let _: () = msg_send![class!(NSBezierPath), fillRect: rect];
    let _: () = msg_send![ctx, setCompositingOperation: 2isize];
    let _: () = msg_send![tinted, unlockFocus];
    release_obj(original);

    let attachment: *mut AnyObject = msg_send![class!(NSTextAttachment), alloc];
    let attachment: *mut AnyObject = msg_send![attachment, init];
    let _: () = msg_send![attachment, setImage: tinted];
    let _: () = msg_send![
        attachment,
        setBounds: NSRect::new(NSPoint::new(0.0, -2.0), NSSize::new(ICON_SIDE, ICON_SIDE))
    ];
    let icon_text: *mut AnyObject = msg_send![
        class!(NSAttributedString),
        attributedStringWithAttachment: attachment
    ];
    // `attributedStringWithAttachment:` is a convenience constructor: the object is
    // autoreleased (+0), so it must NOT be released here.
    release_obj(attachment);
    release_obj(tinted);

    // Keep the font attribute for line metrics; the color attribute is belt-and-braces
    // for any AppKit path that does honor it.
    let icon_len: usize = msg_send![icon_text, length];
    let mutable_icon: *mut AnyObject = msg_send![icon_text, mutableCopy];
    // mutableCopy is +1; the autoreleased original must not be released again.
    let icon_color = hex_to_ns_color(style.color.resolve());
    let icon_font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: style.size, weight: style.weight];
    let color_key = make_nsstring("NSColor");
    let font_key = make_nsstring("NSFont");
    let _: () = msg_send![
        mutable_icon,
        addAttribute: color_key,
        value: icon_color,
        range: NSRange::new(0, icon_len)
    ];
    let _: () = msg_send![
        mutable_icon,
        addAttribute: font_key,
        value: icon_font,
        range: NSRange::new(0, icon_len)
    ];
    release_obj(color_key);
    release_obj(font_key);
    Some(mutable_icon)
}

/// Right inner edge of a card (row controls align here, 16pt from the card's right edge).
fn card_trailing_x() -> f64 {
    PAGE_PAD + CARD_W - CARD_INSET
}

/// Vertical center of a single caption-height line inside a row.
fn row_caption_top(row_top: f64) -> f64 {
    row_top + (ROW_H - CAPTION_LINE_H) / 2.0
}

/// Height of a row's leading two-line copy block (name + 4pt + description). The doc's 4px
/// base grid forbids the mock's 2px between siblings, so the block uses 4.
fn row_copy_block_h() -> f64 {
    BODY_LINE_H + 4.0 + CAPTION_LINE_H
}

/// Space above the copy block inside a row.
fn row_copy_top_offset() -> f64 {
    (ROW_H - row_copy_block_h()) / 2.0
}

/// A layer-backed `NSView` with a background wash, an optional border, and a corner radius.
/// The caller adds the returned view to a parent and releases it (alloc +1).
unsafe fn make_surface_view(
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    bg: u32,
    border: Option<(u32, f64)>,
    radius: f64,
) -> *mut AnyObject {
    let view: *mut AnyObject = msg_send![class!(NSView), alloc];
    let view: *mut AnyObject = msg_send![
        view,
        initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    ];
    paint_surface_layer(view, bg, border, radius);
    view
}

/// The layer painting `make_surface_view` does, applied to an already-allocated view (the
/// segmented control allocates its own subclass, so the painting has to come separately).
unsafe fn paint_surface_layer(
    view: *mut AnyObject,
    bg: u32,
    border: Option<(u32, f64)>,
    radius: f64,
) {
    let _: () = msg_send![view, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![view, layer];
    if !layer.is_null() {
        crate::ffi::layer_set_background(layer, crate::ffi::hex_to_cg_color(bg));
        let _: () = msg_send![layer, setCornerRadius: radius];
        let _: () = msg_send![layer, setMasksToBounds: true];
        if let Some((color, width)) = border {
            crate::ffi::layer_set_border(layer, crate::ffi::hex_to_cg_color(color));
            let _: () = msg_send![layer, setBorderWidth: width];
        }
    }
}

/// A 1pt `separator` hairline (the band's bottom edge, the footer's top edge, the dividers
/// between two rows sharing a card).
unsafe fn add_hairline(content: *mut AnyObject, x: f64, y: f64, w: f64) {
    let palette = crate::theme::ui_palette();
    let line = make_surface_view(x, y, w, 1.0, palette.separator, None, 0.0);
    let _: () = msg_send![content, addSubview: line];
    release_obj(line);
}

/// The title band: a 1pt hairline under the 48pt strip, the window title centered by its own
/// measured width (same reasoning as the settings titlebar), and the `1 / 4` step counter
/// right-aligned in the chrome inset.
/// The band's leading content edge on the borderless window: there are no traffic lights to
/// measure (standardWindowButton returns no buttons), so the icon starts at the mock's
/// `.bar { padding-left: 18px }` inset, like the reference.
const BAND_LEADING_INSET: f64 = 18.0;
/// The band's leading app icon (the reference's `.bar-icon`): 20pt wide with its own curve.
const BAND_ICON_SIZE: f64 = 20.0;
const BAND_ICON_RADIUS: f64 = 5.0;
const BAND_GAP: f64 = 8.0;

/// Where the band's content starts on this window: after any VISIBLE traffic lights (hidden
/// buttons are skipped — the guide hides all three), else the mock's own 18pt inset. The
/// measured support keeps a later chrome decision (buttons shown again) from silently
/// overlapping the leading row.
unsafe fn band_leading_edge(window: *mut AnyObject) -> f64 {
    let mut right = 0.0f64;
    // NSWindowButton order: Close=0, Miniaturize=1, Zoom=2.
    for tag in 0isize..3 {
        let button: *mut AnyObject = msg_send![window, standardWindowButton: tag];
        if button.is_null() {
            continue;
        }
        let hidden: bool = msg_send![button, isHidden];
        if hidden {
            continue;
        }
        let frame: NSRect = msg_send![button, frame];
        right = right.max(frame.origin.x + frame.size.width);
    }
    if right > 0.0 {
        right + BAND_GAP
    } else {
        BAND_LEADING_INSET
    }
}

unsafe fn add_band(content: *mut AnyObject, index: usize, total: usize) {
    add_hairline(content, 0.0, WINDOW_H - BAND_H - 1.0, WINDOW_W);
    // The leading row: the 20pt app icon (mock's .bar-icon) with the title beside it, starting
    // after the traffic lights. The icon view mirrors the settings sidebar's brand row.
    let leading = if let Some(window) = *WINDOW.lock().unwrap() {
        unsafe { band_leading_edge(window.0) }
    } else {
        BAND_LEADING_INSET
    };
    let icon_x = leading;
    let icon_y = y_from_top((BAND_H - BAND_ICON_SIZE) / 2.0, BAND_ICON_SIZE);
    let icon_view: *mut AnyObject = msg_send![class!(NSImageView), alloc];
    let icon_view: *mut AnyObject = msg_send![
        icon_view,
        initWithFrame: NSRect::new(
            NSPoint::new(icon_x, icon_y),
            NSSize::new(BAND_ICON_SIZE, BAND_ICON_SIZE)
        )
    ];
    let image = crate::load_embedded_app_icon();
    if !image.is_null() {
        let _: () = msg_send![icon_view, setImage: image];
        release_obj(image);
    }
    let _: () = msg_send![icon_view, setImageScaling: 3isize]; // NSImageScaleProportionallyUpOrDown
    let _: () = msg_send![icon_view, setEditable: false];
    let _: () = msg_send![icon_view, setWantsLayer: true];
    let icon_layer: *mut AnyObject = msg_send![icon_view, layer];
    if !icon_layer.is_null() {
        // The mock's .bar-icon { border-radius: 5px }: its own height-derived curve, not the
        // 8pt control radius (same derivation rule as the sidebar brand icon).
        let _: () = msg_send![icon_layer, setCornerRadius: BAND_ICON_RADIUS];
        let _: () = msg_send![icon_layer, setMasksToBounds: true];
    }
    let _: () = msg_send![content, addSubview: icon_view];
    release_obj(icon_view);

    let title_x = icon_x + BAND_ICON_SIZE + BAND_GAP;
    let title = t("onboarding.window_title");
    let title_w = single_line_text_width(&title, BAND_TITLE_STYLE).clamp(1.0, 240.0);
    add_label(
        content,
        &title,
        title_x,
        y_from_top((BAND_H - BODY_LINE_H) / 2.0, BODY_LINE_H),
        title_w,
        BODY_LINE_H,
        BAND_TITLE_STYLE,
    );
    let meta = tf(
        "onboarding.step_counter",
        &[
            ("current", &(index + 1).to_string()),
            ("total", &total.to_string()),
        ],
    );
    let meta_w = single_line_text_width(&meta, META_STYLE).max(1.0);
    add_label(
        content,
        &meta,
        WINDOW_W - CHROME_PAD - meta_w,
        y_from_top((BAND_H - CAPTION_LINE_H) / 2.0, CAPTION_LINE_H),
        meta_w,
        CAPTION_LINE_H,
        META_STYLE,
    );
}

/// A card plate (the reference's `.group`): `card_bg` fill, 1pt `card_border`, `radius-card`
/// corners. Rows are added above it as content siblings — their frames already sit inside the
/// plate, so the plate's corner clip cannot cut a corner through a label.
unsafe fn add_card(content: *mut AnyObject, top: f64, h: f64) {
    let palette = crate::theme::ui_palette();
    let card = make_surface_view(
        PAGE_PAD,
        y_from_top(top, h),
        CARD_W,
        h,
        palette.card_bg,
        Some((palette.card_border, 1.0)),
        crate::theme::RADIUS_CARD,
    );
    let _: () = msg_send![content, addSubview: card];
    release_obj(card);
}

/// The 1pt divider between two rows sharing one card: a full-width `separator` line on the
/// shared edge (§5: "the hairline on the shared edge").
unsafe fn add_row_separator(content: *mut AnyObject, row_top: f64) {
    add_hairline(content, PAGE_PAD, y_from_top(row_top - 1.0, 1.0), CARD_W);
}

/// The row's leading copy: 14pt name over a 12pt muted description.
unsafe fn add_row_copy(content: *mut AnyObject, row_top: f64, w: f64, name: &str, desc: &str) {
    let x = PAGE_PAD + CARD_INSET;
    add_label(
        content,
        name,
        x,
        y_from_top(row_top + row_copy_top_offset(), BODY_LINE_H),
        w,
        BODY_LINE_H,
        ROW_LABEL_STYLE,
    );
    add_label(
        content,
        desc,
        x,
        y_from_top(
            row_top + row_copy_top_offset() + BODY_LINE_H + 4.0,
            CAPTION_LINE_H,
        ),
        w,
        CAPTION_LINE_H,
        META_STYLE,
    );
}

/// A trailing in-row action button (`Action` role: the secondary field surface).
unsafe fn add_row_button(content: *mut AnyObject, row_top: f64, title: &str, tag: isize) {
    add_button(
        content,
        title,
        tag,
        card_trailing_x() - BUTTON_W,
        y_from_top(row_top + (ROW_H - BUTTON_H) / 2.0, BUTTON_H),
        BUTTON_W,
        crate::settings::components::SettingsButtonRole::Action,
    );
}

/// A trailing status caption, right-aligned by its own measured width (truncated hard at the
/// 160pt lane instead of a second text column).
unsafe fn add_status_label(
    content: *mut AnyObject,
    row_top: f64,
    text: &str,
    color: LabelColorRole,
) {
    let width = single_line_text_width(text, status_style(color)).clamp(1.0, STATUS_MAX_W);
    add_label(
        content,
        text,
        card_trailing_x() - width,
        y_from_top(row_caption_top(row_top), CAPTION_LINE_H),
        width,
        CAPTION_LINE_H,
        status_style(color),
    );
}

/// One per-logical-key chip (§6.1: fill wash, no border, `radius-legend-chip`), measured by its
/// own label width.
unsafe fn add_legend_chip(content: *mut AnyObject, row_top: f64, x: f64, w: f64, symbol: &str) {
    let palette = crate::theme::ui_palette();
    let y = y_from_top(row_top + (ROW_H - LEGEND_CHIP_H) / 2.0, LEGEND_CHIP_H);
    let wash_base: u32 = if palette.dark {
        0xFFFF_FFFF
    } else {
        0x0000_0000
    };
    let chip = make_surface_view(
        x,
        y,
        w,
        LEGEND_CHIP_H,
        (wash_base & 0xFFFF_FF00) | crate::theme::LEGEND_CHIP_ALPHA,
        None,
        crate::theme::RADIUS_LEGEND_CHIP,
    );
    let _: () = msg_send![content, addSubview: chip];
    release_obj(chip);
    add_label(content, symbol, x, y, w, LEGEND_CHIP_H, chip_label_style());
}

/// The guide's legend-chip text style: muted ink on the wash.
fn chip_label_style() -> LabelStyle {
    LabelStyle {
        size: crate::theme::FONT_CONTROL,
        weight: crate::theme::FONT_WEIGHT_REGULAR,
        color: LabelColorRole::Muted,
        wrap: false,
    }
}

/// The clipboard shortcut as one chip per logical key (shared `key_bindings` parser: one chip
/// per modifier symbol, then the key itself — a key whose name is longer than one glyph is never
/// split per character, §6.1).
fn shortcut_chips(shortcut: &str) -> Vec<String> {
    match crate::key_bindings::Trigger::parse(shortcut) {
        Ok(trigger) => trigger.keycap_chips(),
        Err(_) => vec![crate::mouse::shortcut::display_shortcut(shortcut)],
    }
}

/// The shortcut legend run: chips laid as a unit, right-aligned against the card's inner right
/// edge. Returns the run's left edge (the label column must stop before it).
unsafe fn add_shortcut_chips(content: *mut AnyObject, row_top: f64, shortcut: &str) -> f64 {
    let chips = shortcut_chips(shortcut);
    let widths: Vec<f64> = chips
        .iter()
        .map(|symbol| {
            let text_w = single_line_text_width(symbol, chip_label_style());
            (text_w + 2.0 * crate::theme::LEGEND_CHIP_TEXT_PAD_X)
                .clamp(LEGEND_CHIP_H, LEGEND_CHIP_H * 8.0)
        })
        .collect();
    let run_w: f64 =
        widths.iter().sum::<f64>() + LEGEND_CHIP_GAP * (widths.len().saturating_sub(1)) as f64;
    let mut x = card_trailing_x() - run_w;
    for (symbol, w) in chips.iter().zip(widths.iter()) {
        add_legend_chip(content, row_top, x, *w, symbol);
        x += *w + LEGEND_CHIP_GAP;
    }
    card_trailing_x() - run_w
}

/// A muted note under a card; returns the block's measured height.
unsafe fn add_note(content: *mut AnyObject, top: f64, text: &str) -> f64 {
    let h = measured_text_height(text, CARD_W, HINT_STYLE)
        .clamp(CAPTION_LINE_H, 2.0 * CAPTION_LINE_H + 2.0);
    add_label(
        content,
        text,
        PAGE_PAD,
        y_from_top(top, h),
        CARD_W,
        h,
        HINT_STYLE,
    );
    h
}

// --- The display-mode segmented control ---
//
// The reference builds its segmented control by hand (a field container, two labeled segments,
// the selected one a raised pill), which exact AppKit `NSSegmentedControl` does not track. This
// is the same shape; it keeps the smoke contract: the container carries the ACTION_DISPLAY_MODE
// tag, implements `selectedSegment`/`setSelectedSegment:`, and `on_action` maps a selected
// segment to the icons/thumbnails actions exactly as the old control did.

/// One rendered segmented control. Rebuilt per step render, like ACTION_TAGS.
#[derive(Clone)]
struct SegmentRecord {
    container: usize,
    children: Vec<(usize, usize)>, // (segment view, label view); index 0 icons, index 1 thumbnails
    selected: usize,
}
static SEGMENTS: MainThreadSlot<Vec<SegmentRecord>> = unsafe { MainThreadSlot::new(Vec::new()) };
static SEGMENT_HOVER: MainThreadSlot<Option<usize>> = unsafe { MainThreadSlot::new(None) };
static SEGMENT_FOCUS: MainThreadSlot<Option<usize>> = unsafe { MainThreadSlot::new(None) };

fn segment_record_by_container(container: usize) -> Option<SegmentRecord> {
    SEGMENTS
        .lock()
        .unwrap()
        .iter()
        .find(|record| record.container == container)
        .cloned()
}

fn segment_record_of_child(child: usize) -> Option<(SegmentRecord, usize)> {
    SEGMENTS.lock().unwrap().iter().find_map(|record| {
        let index = record
            .children
            .iter()
            .position(|(view, _)| *view == child)?;
        Some((record.clone(), index))
    })
}

/// Update the container's selection (main-thread map) and repaint its segments.
fn segment_set_selected(container: usize, selected: usize) {
    let Some(record) = segment_record_by_container(container) else {
        return;
    };
    let selected = selected.min(record.children.len().saturating_sub(1));
    let mut slot = SEGMENTS.lock().unwrap();
    let Some(record_ref) = slot.iter_mut().find(|record| record.container == container) else {
        return;
    };
    record_ref.selected = selected;
    unsafe { segment_apply_visual(record_ref) };
}

extern "C" fn segment_container_get_selected(this: *mut AnyObject, _cmd: Sel) -> isize {
    crate::callback_guard::event("onboarding_segment_get_selected", 0isize, || {
        segment_record_by_container(this as usize)
            .map(|record| record.selected as isize)
            .unwrap_or(0)
    })
}

extern "C" fn segment_container_set_selected(this: *mut AnyObject, _cmd: Sel, index: isize) {
    crate::callback_guard::void("onboarding_segment_set_selected", || {
        segment_set_selected(this as usize, index.max(0) as usize);
    });
}

/// Click/keyboard selection: repaint, then dispatch through the container's
/// ACTION_DISPLAY_MODE tag — the same path a programmatic selection takes, so a click and a
/// smoke dispatch produce identical behavior.
unsafe fn segmented_select(container: *mut AnyObject, selected: usize) {
    segment_set_selected(container as usize, selected);
    on_action(
        std::ptr::null_mut(),
        sel!(handleOnboardingAction:),
        container,
    );
}

/// The class the segmented control container is an instance of, registered once.
///
/// Deliberately **not** an `extern "C"` callback: its only caller is Rust (`add_display_mode_control`),
/// and registering a class can panic (a selector signature that asserts, a name already taken).
/// Kept as an ordinary function the panic unwinds to that caller instead of crossing the C ABI,
/// where it would abort the process from `panic_cannot_unwind`.
/// `register_once` is idempotent, so a failed registration leaves the class unregistered and the
/// next call retries rather than answering with a class that does not exist.
fn segment_container_class_handle() -> crate::ffi::StaticClass {
    static CLASS: std::sync::OnceLock<crate::ffi::StaticClass> = std::sync::OnceLock::new();
    crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabOnboardingSegmented", || {
        crate::ffi::build_class(
            c"OhMyTabOnboardingSegmented",
            class!(NSView),
            |builder| unsafe {
                builder.add_method::<AnyObject, _>(
                    sel!(selectedSegment),
                    segment_container_get_selected
                        as unsafe extern "C" fn(*mut AnyObject, Sel) -> isize,
                );
                builder.add_method::<AnyObject, _>(
                    sel!(setSelectedSegment:),
                    segment_container_set_selected
                        as unsafe extern "C" fn(*mut AnyObject, Sel, isize),
                );
            },
        )
    })
}

fn segment_child_class_handle() -> crate::ffi::StaticClass {
    static CLASS: std::sync::OnceLock<crate::ffi::StaticClass> = std::sync::OnceLock::new();
    crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabOnboardingSegment", || {
        crate::ffi::build_class(
            c"OhMyTabOnboardingSegment",
            class!(NSView),
            |builder| unsafe {
                builder.add_method::<AnyObject, _>(
                    sel!(mouseDown:),
                    segment_mouse_down as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
                builder.add_method::<AnyObject, _>(
                    sel!(keyDown:),
                    segment_key_down as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
                builder.add_method::<AnyObject, _>(
                    sel!(mouseEntered:),
                    segment_mouse_entered
                        as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
                builder.add_method::<AnyObject, _>(
                    sel!(mouseExited:),
                    segment_mouse_exited
                        as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
                builder.add_method::<AnyObject, _>(
                    sel!(acceptsFirstResponder),
                    segment_child_accepts_first_responder
                        as unsafe extern "C" fn(*mut AnyObject, Sel) -> objc2::runtime::Bool,
                );
                builder.add_method::<AnyObject, _>(
                    sel!(becomeFirstResponder),
                    segment_child_become_first_responder
                        as unsafe extern "C" fn(*mut AnyObject, Sel) -> objc2::runtime::Bool,
                );
                builder.add_method::<AnyObject, _>(
                    sel!(resignFirstResponder),
                    segment_child_resign_first_responder
                        as unsafe extern "C" fn(*mut AnyObject, Sel) -> objc2::runtime::Bool,
                );
                builder.add_method::<AnyObject, _>(
                    sel!(accessibilityPerformPress),
                    segment_accessibility_perform_press
                        as unsafe extern "C" fn(*mut AnyObject, Sel) -> objc2::runtime::Bool,
                );
            },
        )
    })
}

extern "C" fn segment_child_accepts_first_responder(
    _this: *mut AnyObject,
    _cmd: Sel,
) -> objc2::runtime::Bool {
    callback_guard::value(
        "segment_child_accepts_first_responder",
        objc2::runtime::Bool::YES,
        || objc2::runtime::Bool::YES,
    )
}

/// Pure key-down decision for one segment, so the keyboard logic is testable without an event:
/// `Some(index)` selects that segment, `None` passes the key to the responder chain. Space,
/// Return and the keypad Enter select the FOCUSED segment — the child's own index, never the
/// record's current selection — and the arrows flip a two-option radio group.
fn segment_key_selection(
    code: u16,
    selected_in_record: usize,
    child_index: usize,
) -> Option<usize> {
    match code {
        49 | 36 | 76 => Some(child_index), // Space, Return, keypad Enter
        123..=126 => Some(1 - selected_in_record),
        _ => None,
    }
}

extern "C" fn segment_mouse_down(this: *mut AnyObject, _cmd: Sel, _event: *mut AnyObject) {
    crate::callback_guard::void("onboarding_segment_click", || unsafe {
        let Some((record, index)) = segment_record_of_child(this as usize) else {
            return;
        };
        segmented_select(record.container as *mut AnyObject, index);
    });
}

extern "C" fn segment_key_down(this: *mut AnyObject, _cmd: Sel, event: *mut AnyObject) {
    crate::callback_guard::void("onboarding_segment_key", || unsafe {
        // Space/Return select the focused segment; unhandled keys travel up the responder
        // chain. See `segment_key_selection` for the full decision.
        let Some((record, child_index)) = segment_record_of_child(this as usize) else {
            return;
        };
        let code: u16 = msg_send![event, keyCode];
        match segment_key_selection(code, record.selected, child_index) {
            Some(selected) => {
                segmented_select(record.container as *mut AnyObject, selected);
            }
            None => {
                type F = unsafe extern "C" fn(*mut crate::ffi::ObjcSuper, Sel, *mut AnyObject);
                let mut sup = crate::ffi::ObjcSuper::for_defined_class(
                    this as *mut c_void,
                    segment_child_class_handle(),
                );
                let send: F = std::mem::transmute(crate::ffi::objc_msgSendSuper as *const ());
                send(&mut sup, sel!(keyDown:), event);
            }
        }
    });
}

/// VoiceOver's activation action routes through the same selection path a click takes.
extern "C" fn segment_accessibility_perform_press(
    this: *mut AnyObject,
    _cmd: Sel,
) -> objc2::runtime::Bool {
    callback_guard::value(
        "segment_accessibility_perform_press",
        objc2::runtime::Bool::YES,
        || {
            objc2::runtime::Bool::from(crate::callback_guard::bool(
                "onboarding_segment_ax_press",
                false,
                || unsafe {
                    match segment_record_of_child(this as usize) {
                        Some((record, index)) => {
                            segmented_select(record.container as *mut AnyObject, index);
                            true
                        }
                        None => false,
                    }
                },
            ))
        },
    )
}

/// The segment's accessibility value must be a boolean NSNumber (the native radio-button
/// contract), not a "0"/"1" string: the string left the checked state unreadable to
/// VoiceOver. The convenience constructor is autoreleased (+0): do not release it.
unsafe fn segment_a11y_value(checked: bool) -> *mut AnyObject {
    let flag = objc2::runtime::Bool::from(checked);
    msg_send![class!(NSNumber), numberWithBool: flag]
}

extern "C" fn segment_mouse_entered(this: *mut AnyObject, _cmd: Sel, _event: *mut AnyObject) {
    crate::callback_guard::void("onboarding_segment_hover_enter", || unsafe {
        *SEGMENT_HOVER.lock().unwrap() = Some(this as usize);
        if let Some(record) = segment_record_of_child(this as usize) {
            segment_apply_visual(&record.0);
        }
    });
}

extern "C" fn segment_mouse_exited(this: *mut AnyObject, _cmd: Sel, _event: *mut AnyObject) {
    crate::callback_guard::void("onboarding_segment_hover_exit", || unsafe {
        let mut slot = SEGMENT_HOVER.lock().unwrap();
        if *slot == Some(this as usize) {
            *slot = None;
        }
        drop(slot);
        if let Some(record) = segment_record_of_child(this as usize) {
            segment_apply_visual(&record.0);
        }
    });
}

extern "C" fn segment_child_become_first_responder(
    this: *mut AnyObject,
    _cmd: Sel,
) -> objc2::runtime::Bool {
    callback_guard::value(
        "segment_child_become_first_responder",
        objc2::runtime::Bool::YES,
        || unsafe {
            type F = unsafe extern "C" fn(*mut crate::ffi::ObjcSuper, Sel) -> objc2::runtime::Bool;
            let mut sup = crate::ffi::ObjcSuper::for_defined_class(
                this as *mut c_void,
                segment_child_class_handle(),
            );
            let send: F = std::mem::transmute(crate::ffi::objc_msgSendSuper as *const ());
            let accepted = send(&mut sup, sel!(becomeFirstResponder));
            if accepted.as_bool() {
                *SEGMENT_FOCUS.lock().unwrap() = Some(this as usize);
                if let Some(record) = segment_record_of_child(this as usize) {
                    segment_apply_visual(&record.0);
                }
            }
            accepted
        },
    )
}

extern "C" fn segment_child_resign_first_responder(
    this: *mut AnyObject,
    _cmd: Sel,
) -> objc2::runtime::Bool {
    callback_guard::value(
        "segment_child_resign_first_responder",
        objc2::runtime::Bool::YES,
        || unsafe {
            type F = unsafe extern "C" fn(*mut crate::ffi::ObjcSuper, Sel) -> objc2::runtime::Bool;
            let mut sup = crate::ffi::ObjcSuper::for_defined_class(
                this as *mut c_void,
                segment_child_class_handle(),
            );
            let send: F = std::mem::transmute(crate::ffi::objc_msgSendSuper as *const ());
            let accepted = send(&mut sup, sel!(resignFirstResponder));
            if accepted.as_bool() {
                let mut slot = SEGMENT_FOCUS.lock().unwrap();
                if *slot == Some(this as usize) {
                    *slot = None;
                }
                drop(slot);
                if let Some(record) = segment_record_of_child(this as usize) {
                    segment_apply_visual(&record.0);
                }
            }
            accepted
        },
    )
}

/// Repaint one record's segments to the current selection/hover/focus state. The selected
/// segment is the raised pill (`button_bg` + 1pt `card_border`); unselected ones paint no
/// surface and only their ink moves (`secondary_text`, primary on hover, like the reference).
/// The focused segment's outline is the 2pt accent ring (state carried by color, defined by
/// the accent's 3:1 non-text floor).
unsafe fn segment_apply_visual(record: &SegmentRecord) {
    let palette = crate::theme::ui_palette();
    let hovered = *SEGMENT_HOVER.lock().unwrap();
    let focused = *SEGMENT_FOCUS.lock().unwrap();
    for (index, (view, label)) in record.children.iter().enumerate() {
        let is_selected = index == record.selected;
        let is_hovered = hovered == Some(*view);
        let is_focused = focused == Some(*view);
        let layer: *mut AnyObject = msg_send![(*view) as *mut AnyObject, layer];
        if !layer.is_null() {
            if is_selected {
                crate::ffi::layer_set_background(
                    layer,
                    crate::ffi::hex_to_cg_color(palette.button_bg),
                );
                crate::ffi::layer_set_border(
                    layer,
                    crate::ffi::hex_to_cg_color(palette.card_border),
                );
                let _: () = msg_send![layer, setBorderWidth: 1.0f64];
            } else {
                crate::ffi::layer_set_background(layer, crate::ffi::hex_to_cg_color(0x00000000));
                let _: () = msg_send![layer, setBorderWidth: 0.0f64];
            }
            if is_focused {
                crate::ffi::layer_set_border(layer, crate::ffi::hex_to_cg_color(palette.accent));
                let _: () = msg_send![layer, setBorderWidth: 2.0f64];
            }
        }
        let ink = if is_selected || is_hovered {
            palette.primary_text
        } else {
            palette.secondary_text
        };
        let _: () = msg_send![*label as *mut AnyObject, setTextColor: hex_to_ns_color(ink)];
        // The AX checked value tracks the same state the pixels show.
        let _: () = msg_send![
            *view as *mut AnyObject,
            setAccessibilityValue: segment_a11y_value(is_selected)
        ];
    }
}

/// The switcher-shortcut dropdown. It reuses the settings page's own select control rather than an
/// `NSPopUpButton`: that class routes through AppKit's SwiftUI bridge, which aborts the process on
/// `NSTextAlignment` (measured 2026-10-24: `Fatal error: New case in NSTextAlignment` the moment a
/// popup is added to the guide's view tree). The step previews the choice; `advance()` commits it.
unsafe fn add_modifier_control(content: *mut AnyObject, top: f64, selected: usize) {
    let Some(target) = crate::CONTROLLER.lock().unwrap().map(|ptr| ptr.0) else {
        return;
    };
    // Labels follow the settings page's order, which is `SWITCHER_MODIFIER_VALUES` -- so the control
    // index is the config index and needs no mapping.
    let labels = [
        t("settings.modifier_option"),
        t("settings.modifier_command"),
        t("settings.modifier_control"),
    ];
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let popup = crate::settings::select::make_popup(
        crate::settings::select::SelectOwner::Onboarding,
        PAGE_PAD,
        y_from_top(top, SEGMENTED_H),
        SEGMENTED_W,
        SEGMENTED_H,
        &label_refs,
        selected.min(label_refs.len().saturating_sub(1)),
    );
    if popup.is_null() {
        return;
    }
    let _: () = msg_send![popup, setTarget: target];
    // The select sends this when a row is chosen; `on_action` maps the pointer to the tag below.
    let _: () = msg_send![popup, setAction: sel!(handleOnboardingAction:)];
    ACTION_TAGS
        .lock()
        .unwrap()
        .push((popup as usize, ACTION_SELECT_MODIFIER));
    let accessibility_label = make_nsstring(&t("settings.row_modifier"));
    let _: () = msg_send![popup, setAccessibilityLabel: accessibility_label];
    release_obj(accessibility_label);
    let _: () = msg_send![content, addSubview: popup];
    release_obj(popup);
}
/// The segmented control: a 320×30 field container with the two labeled segments inside, the
/// selected one the raised pill.
unsafe fn add_display_mode_control(
    content: *mut AnyObject,
    seg_top: f64,
    thumbnails_enabled: bool,
) {
    let palette = crate::theme::ui_palette();
    let segments: [&str; 2] = [
        &t("onboarding.display_icons"),
        &t("onboarding.display_thumbnails"),
    ];
    let selected = usize::from(thumbnails_enabled);
    let container_class = segment_container_class_handle();
    let container: *mut AnyObject = msg_send![container_class.0 as *mut AnyObject, alloc];
    let container: *mut AnyObject = msg_send![
        container,
        initWithFrame: NSRect::new(
            NSPoint::new(PAGE_PAD, y_from_top(seg_top, SEGMENTED_H)),
            NSSize::new(SEGMENTED_W, SEGMENTED_H)
        )
    ];
    paint_surface_layer(
        container,
        palette.field_bg,
        Some((palette.card_border, 1.0)),
        crate::theme::RADIUS_CONTROL,
    );
    let child_class = segment_child_class_handle();
    let child_class_ptr = child_class.0 as *mut AnyObject;
    // The container is not flipped, so its children's frames are bottom-up INSIDE the container.
    let mut children: Vec<(usize, usize)> = Vec::with_capacity(2);
    for (index, name) in segments.iter().enumerate() {
        let x = SEGMENT_PAD + index as f64 * (SEGMENT_W + SEGMENT_PAD);
        let view: *mut AnyObject = msg_send![child_class_ptr, alloc];
        let view: *mut AnyObject = msg_send![
            view,
            initWithFrame: NSRect::new(
                NSPoint::new(x, SEGMENT_PAD),
                NSSize::new(SEGMENT_W, SEGMENT_H)
            )
        ];
        // The visual state (selected pill / hover / focus) is painted by segment_apply_visual
        // below and on state changes; the layer only needs its corner radius here.
        let _: () = msg_send![view, setWantsLayer: true];
        let layer: *mut AnyObject = msg_send![view, layer];
        if !layer.is_null() {
            let _: () =
                msg_send![layer, setCornerRadius: crate::theme::RADIUS_CONTROL - SEGMENT_PAD]; // concentric: outer - padding
            let _: () = msg_send![layer, setMasksToBounds: true];
        }
        // The label is centered inside the segment by its own measured width, truncating hard
        // at the segment's leaf 16pt margin so even the longest translated label stays inside.
        let label_w = (single_line_text_width(name, BODY_STYLE) + 1.0).clamp(1.0, SEGMENT_W - 16.0);
        let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let label: *mut AnyObject = msg_send![
            label,
            initWithFrame: NSRect::new(
                NSPoint::new(
                    ((SEGMENT_W - label_w) / 2.0).max(4.0),
                    (SEGMENT_H - BODY_LINE_H) / 2.0,
                ),
                NSSize::new(label_w, BODY_LINE_H)
            )
        ];
        let value = make_nsstring(name);
        let _: () = msg_send![label, setStringValue: value];
        release_obj(value);
        let _: () = msg_send![label, setBezeled: false];
        let _: () = msg_send![label, setDrawsBackground: false];
        let _: () = msg_send![label, setEditable: false];
        let _: () = msg_send![label, setSelectable: false];
        let font: *mut AnyObject = msg_send![
            class!(NSFont),
            systemFontOfSize: crate::theme::FONT_CONTROL
        ];
        let _: () = msg_send![label, setFont: font];
        let _: () = msg_send![label, setUsesSingleLineMode: true];
        let _: () = msg_send![label, setLineBreakMode: 4isize]; // NSLineBreakByTruncatingTail
        let _: () = msg_send![view, addSubview: label];
        // Retained by its segment view; the record holds the pointer until the next render
        // clears it.
        release_obj(label);

        // One tracking area per segment: hover is a color-only move (§10), and active-always
        // tracks hovers even when the guide window itself is not key.
        let bounds: NSRect = msg_send![view, bounds];
        let area: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
        let area: *mut AnyObject = msg_send![
            area,
            initWithRect: bounds,
            options: 0x81u64, // NSTrackingMouseEnteredAndExited | NSTrackingActiveAlways
            owner: view,
            userInfo: std::ptr::null::<AnyObject>()
        ];
        let _: () = msg_send![view, addTrackingArea: area];
        release_obj(area);

        // Accessibility: a radio button with its choice's label and its checked value; the
        // container reads as the radio group.
        let child_ptr = view as usize;
        let role = make_nsstring("AXRadioButton");
        let _: () = msg_send![view, setAccessibilityRole: role];
        release_obj(role);
        let _: () = msg_send![view, setAccessibilityValue: segment_a11y_value(index == selected)];
        let a11y_label = make_nsstring(name);
        let _: () = msg_send![view, setAccessibilityLabel: a11y_label];
        release_obj(a11y_label);

        children.push((child_ptr, label as usize));
        let _: () = msg_send![container, addSubview: view];
        release_obj(view);
    }
    SEGMENTS.lock().unwrap().push(SegmentRecord {
        container: container as usize,
        children,
        selected,
    });
    let record = SEGMENTS.lock().unwrap().last().unwrap().clone();
    segment_apply_visual(&record);
    // No target/action wiring: the container is a plain NSView subclass. A selection always
    // dispatches through the ACTION_DISPLAY_MODE map entry (segmented_select from a click or a
    // key; the smoke runner via setSelectedSegment + the shared selector), and the map entry is
    // cleared on the next render like every other tag.
    ACTION_TAGS
        .lock()
        .unwrap()
        .push((container as usize, ACTION_DISPLAY_MODE));
    let group_label = make_nsstring(&t("onboarding.display_title"));
    let _: () = msg_send![container, setAccessibilityLabel: group_label];
    release_obj(group_label);
    let group_role = make_nsstring("AXRadioGroup");
    let _: () = msg_send![container, setAccessibilityRole: group_role];
    release_obj(group_role);
    let _: () = msg_send![content, addSubview: container];
    release_obj(container);
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

/// Re-render the visible step (theme/palette change fan-out); no-op when hidden.
pub(crate) fn rerender_if_visible() {
    if is_visible() {
        render_current_step();
    }
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
        // Rebuilding the content destroys every old control, so the action (and segment) maps
        // are cleared with it: a page holds a handful of controls, so neither can grow.
        ACTION_TAGS.lock().unwrap().clear();
        SEGMENTS.lock().unwrap().clear();
        *SEGMENT_HOVER.lock().unwrap() = None;
        *SEGMENT_FOCUS.lock().unwrap() = None;
        paint_window_surface(content);
        let (title_key, body) = step_copy(&state, step);
        // The step title and the lede flow downward from the band; the measured lede height is
        // what the card/field rows below follow (the reference's spacing rhythm: title, lede,
        // then a 24pt step to the card). The body's measured height is bounded by BODY_MAX_H —
        // the locale tests pin the shipped copy.
        add_label(
            content,
            &t(title_key),
            PAGE_PAD,
            y_from_top(TITLE_TOP, TITLE_LINE_H),
            CARD_W,
            TITLE_LINE_H,
            STEP_TITLE_STYLE,
        );
        let body_h = measured_text_height(&body, CARD_W, BODY_STYLE).clamp(1.0, BODY_MAX_H);
        // The more-features body carries the inline menu-bar icon ({icon}); the plain-body
        // builder falls back to a plain label for every other step.
        add_menu_icon_label(
            content,
            &body,
            PAGE_PAD,
            y_from_top(LEDE_TOP, body_h),
            CARD_W,
            body_h + 2.0,
            BODY_STYLE,
        );
        let card_row_top = LEDE_TOP + body_h + 24.0;
        match step {
            Step::PermissionsAndStartup => render_permissions_and_startup(
                content,
                card_row_top,
                ax_granted,
                crate::restart::restart_required(),
                state.launch_at_login,
            ),
            Step::DisplayMode => render_display_mode(
                content,
                card_row_top,
                state.thumbnails_enabled,
                state.modifier,
                screen_granted,
            ),
            Step::ClipboardHistory => render_clipboard_history(
                content,
                card_row_top,
                state.clipboard_enabled,
                &state.clipboard_shortcut,
            ),
            Step::MoreFeatures => render_more_features(content),
        }
        add_band(content, state.index, total);
        render_footer(content, state.index);
    }
    *LAST_SIGNATURE.lock().unwrap() = Some(permission_signature(&state));
    // The window may be closed: make sure it is frontmost after rendering (an accessory app must
    // activate explicitly). The appearance is re-asserted here so a theme change while the
    // guide is open reaches the reused window together with the re-rendered step content.
    unsafe {
        apply_window_appearance();
        if let Some(window) = *WINDOW.lock().unwrap() {
            let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![app, activateIgnoringOtherApps: true];
            let _: () = msg_send![window.0, makeKeyAndOrderFront: std::ptr::null::<AnyObject>()];
        }
    }
}

/// The step's title key and lede copy. The clipboard body reflects the live draft so the
/// toggle's own instruction stays true the moment it is picked.
fn step_copy(state: &UiState, step: Step) -> (&'static str, String) {
    match step {
        Step::PermissionsAndStartup => (
            "onboarding.permissions_title",
            tf(
                "onboarding.accessibility_body",
                &[
                    ("app", &app_display_name()),
                    // The committed config, not the draft: this copy tells the user which shortcut
                    // works right now. A drafted-but-unconfirmed modifier is not listening yet, and
                    // naming it here would describe a key that does nothing.
                    ("shortcut", &shortcut_label()),
                ],
            ),
        ),
        Step::DisplayMode => ("onboarding.display_title", t("onboarding.display_body")),
        Step::ClipboardHistory => (
            "onboarding.clipboard_title",
            clipboard_shortcut_body(&state.clipboard_shortcut, state.clipboard_enabled),
        ),
        Step::MoreFeatures => ("onboarding.more_title", t("onboarding.more_body")),
    }
}

unsafe fn render_permissions_and_startup(
    content: *mut AnyObject,
    card_row_top: f64,
    ax_granted: bool,
    restart_required: bool,
    launch_at_login: bool,
) {
    add_card(content, card_row_top, ROW_H * 2.0);
    // Row 1: the accessibility permission. The trailing slot carries the status caption while
    // both are fine and the action button when they are not (grant via System Settings, or a
    // restart to apply an already-granted permission that is pending).
    let label_w = card_trailing_x() - (PAGE_PAD + CARD_INSET) - STATUS_MAX_W - 8.0;
    add_row_copy(
        content,
        card_row_top,
        label_w,
        &t("onboarding.accessibility_label"),
        &t("onboarding.accessibility_desc"),
    );
    if restart_required {
        add_row_button(
            content,
            card_row_top,
            &tf("onboarding.btn_restart", &[("app", &app_display_name())]),
            ACTION_RESTART_APP,
        );
    } else if ax_granted {
        add_status_label(
            content,
            card_row_top,
            &t("onboarding.status_granted"),
            LabelColorRole::Success,
        );
    } else {
        add_row_button(
            content,
            card_row_top,
            &t("onboarding.btn_open_settings"),
            ACTION_OPEN_ACCESSIBILITY,
        );
    }
    add_row_separator(content, card_row_top + ROW_H);
    // Row 2: launch at login, an immediate draft whose toggle commits on Next (existing
    // behavior; the switch itself is the whole control column).
    let switch_label_w = card_trailing_x() - (PAGE_PAD + CARD_INSET) - 60.0;
    add_row_copy(
        content,
        card_row_top + ROW_H,
        switch_label_w,
        &t("onboarding.launch_label"),
        &t("onboarding.launch_desc"),
    );
    add_switch(
        content,
        card_trailing_x(),
        y_from_top(card_row_top + ROW_H, ROW_H),
        ROW_H,
        launch_at_login,
        ACTION_TOGGLE_LAUNCH_DRAFT,
        "onboarding.launch_label",
    );
}

unsafe fn render_display_mode(
    content: *mut AnyObject,
    card_row_top: f64,
    thumbnails_enabled: bool,
    modifier: usize,
    screen_granted: bool,
) {
    // The switcher shortcut comes first: it is the more basic choice on this step, and the step's
    // title covers both fields.
    add_label(
        content,
        &t("settings.row_modifier"),
        PAGE_PAD,
        y_from_top(card_row_top, CAPTION_LINE_H),
        200.0,
        CAPTION_LINE_H,
        FIELD_LABEL_STYLE,
    );
    let modifier_top = card_row_top + CAPTION_LINE_H + 8.0;
    add_modifier_control(content, modifier_top, modifier);
    let display_top = modifier_top + SEGMENTED_H + 16.0;
    // The segmented control: a field head (12pt semibold secondary label), 8pt below it the
    // 320×30 container.
    add_label(
        content,
        &t("onboarding.display_mode_label"),
        PAGE_PAD,
        y_from_top(display_top, CAPTION_LINE_H),
        200.0,
        CAPTION_LINE_H,
        FIELD_LABEL_STYLE,
    );
    let seg_top = display_top + CAPTION_LINE_H + 8.0;
    add_display_mode_control(content, seg_top, thumbnails_enabled);
    // The live Screen Recording permission line sits right under the control (the reference
    // mockup leaves this area empty; the app's flow needs the state made explicit).
    // Granted → a success-colored caption; missing → a warning caption, plus (while the
    // thumbnails mode is picked) the grant button on the same row.
    let segment_bottom = seg_top + SEGMENTED_H;
    let note_top = segment_bottom + 16.0;
    let note_h = 2.0 * CAPTION_LINE_H + 2.0;
    let needs_screen_permission = thumbnails_enabled && !screen_granted;
    let (key, color) = if screen_granted {
        (
            "onboarding.screen_permission_granted",
            LabelColorRole::Success,
        )
    } else if needs_screen_permission {
        (
            "onboarding.screen_permission_needed",
            LabelColorRole::Warning,
        )
    } else {
        (
            "onboarding.screen_permission_missing",
            LabelColorRole::Warning,
        )
    };
    let text_w = WINDOW_W
        - PAGE_PAD * 2.0
        - if needs_screen_permission {
            BUTTON_W + 16.0
        } else {
            0.0
        };
    add_label(
        content,
        &t(key),
        PAGE_PAD,
        y_from_top(note_top, note_h),
        text_w,
        note_h,
        status_style(color),
    );
    if needs_screen_permission {
        add_button(
            content,
            &t("onboarding.btn_open_settings"),
            ACTION_ALLOW_SCREEN,
            WINDOW_W - PAGE_PAD - BUTTON_W,
            y_from_top(note_top + (note_h - BUTTON_H) / 2.0, BUTTON_H),
            BUTTON_W,
            crate::settings::components::SettingsButtonRole::Action,
        );
    }
}

/// Clipboard-history step geometry. The card's two rows are the master toggle and the shortcut
/// legend; the storage note follows the card, whose vertical position follows the measured lede,
/// so a toggle never moves the rows relative to the card.
unsafe fn render_clipboard_history(
    content: *mut AnyObject,
    card_row_top: f64,
    enabled: bool,
    shortcut: &str,
) {
    add_card(content, card_row_top, ROW_H * 2.0);
    // Row 1: the master switch. The card cursor never moves on a toggle — only the trailing
    // storage note appears/disappears under the card.
    let switch_label_w = card_trailing_x() - (PAGE_PAD + CARD_INSET) - 60.0;
    add_row_copy(
        content,
        card_row_top,
        switch_label_w,
        &t("onboarding.clipboard_label"),
        &t("onboarding.clipboard_desc"),
    );
    add_switch(
        content,
        card_trailing_x(),
        y_from_top(card_row_top, ROW_H),
        ROW_H,
        enabled,
        ACTION_TOGGLE_CLIPBOARD_DRAFT,
        "onboarding.clipboard_label",
    );
    add_row_separator(content, card_row_top + ROW_H);
    // Row 2: the open shortcut, drawn as a legend run of one chip per key (right-aligned; the
    // descs under it explain where the binding can change, by the shared hint key).
    let chips_left = unsafe { add_shortcut_chips(content, card_row_top + ROW_H, shortcut) };
    let chip_label_w = (chips_left - 8.0).max(0.0) - (PAGE_PAD + CARD_INSET);
    add_row_copy(
        content,
        card_row_top + ROW_H,
        chip_label_w,
        &t("onboarding.clipboard_open_shortcut"),
        &t("onboarding.clipboard_shortcut_hint"),
    );
    // Where the history goes, stated once while the feature is on: it is written to disk while
    // the app runs, and the settings page's clear-on-quit switch is the way to leave nothing
    // behind.
    if enabled {
        unsafe {
            add_note(
                content,
                card_row_top + ROW_H * 2.0 + 12.0,
                &t("onboarding.clipboard_storage_note"),
            );
        }
    }
}

fn clipboard_shortcut_body(shortcut: &str, enabled: bool) -> String {
    let formatted = crate::mouse::shortcut::display_shortcut(shortcut);
    tf(
        if enabled {
            "onboarding.clipboard_body_enabled"
        } else {
            "onboarding.clipboard_body"
        },
        &[("shortcut", formatted.as_str())],
    )
}

unsafe fn render_more_features(_content: *mut AnyObject) {
    // The reference's final page: title, lede, and no in-page CTA — the footer's
    // "Done and open Settings" button hands off to the settings window, which the About-page
    // and menu entries can reopen just the same.
}

unsafe fn add_switch(
    content: *mut AnyObject,
    right_x: f64,
    y: f64,
    slot_h: f64,
    checked: bool,
    action_tag: isize,
    label_key: &str,
) {
    let switch = crate::settings::components::onboarding_switch(right_x, y, slot_h, checked);
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

unsafe fn render_footer(content: *mut AnyObject, index: usize) {
    // The footer: skip left by the chrome inset, previous + next right, under a 1pt separator
    // hairline. The skip link disappears on the last step exactly like the reference does.
    add_hairline(content, 0.0, FOOT_H, WINDOW_W);
    if index + 1 < steps_for().len() {
        add_text_button(
            content,
            &t("onboarding.btn_skip_guide"),
            ACTION_SKIP_GUIDE,
            CHROME_PAD,
        );
    }
    // The final primary is the reference's "done and open Settings": the label is longer than
    // Next's, so the button's width fits its own measured label (right edge pinned to the
    // chrome inset) instead of clipping the translation into the fixed 132pt.
    let last = index + 1 == steps_for().len();
    let primary_title = if last {
        t("onboarding.btn_finish")
    } else {
        t("onboarding.btn_next")
    };
    let primary_w = if last {
        (single_line_text_width(&primary_title, FOOTER_BUTTON_LABEL_STYLE) + 2.0 * 16.0)
            .max(BUTTON_W)
    } else {
        BUTTON_W
    };
    let primary_x = WINDOW_W - CHROME_PAD - primary_w;
    if index > 0 {
        add_button(
            content,
            &t("onboarding.btn_previous"),
            ACTION_BACK,
            primary_x - GAP - BUTTON_W,
            BUTTON_Y,
            BUTTON_W,
            crate::settings::components::SettingsButtonRole::Footer,
        );
    }
    let action = if last {
        // The guide completes AND hands off to Settings on the same click (the reference's
        // footer); the page keeps no separate open-Settings button.
        ACTION_OPEN_SETTINGS
    } else {
        ACTION_NEXT
    };
    add_button(
        content,
        &primary_title,
        action,
        primary_x,
        BUTTON_Y,
        primary_w,
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
        initWithFrame: NSRect::new(
            NSPoint::new(x, BUTTON_Y),
            NSSize::new(120.0, BUTTON_H)
        )
    ];
    let title_ns = make_nsstring(title);
    let _: () = msg_send![button, setTitle: title_ns];
    let _: () = msg_send![button, setAccessibilityLabel: title_ns];
    release_obj(title_ns);
    let _: () = msg_send![button, setButtonType: 0isize];
    let _: () = msg_send![button, setBordered: false];
    let _: () = msg_send![button, setContentTintColor: hex_to_ns_color(
        crate::theme::ui_palette().secondary_text
    )];
    let font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CAPTION];
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
        if tag == ACTION_SELECT_MODIFIER {
            // The control's index IS the config index: both follow `SWITCHER_MODIFIER_VALUES`.
            let index: isize = unsafe { msg_send![sender, indexOfSelectedItem] };
            let index = usize::try_from(index).unwrap_or(0);
            if let Some(state) = STATE.lock().unwrap().as_mut() {
                state.modifier = index;
            }
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
            apply_clipboard_choice(|state| {
                state.clipboard_enabled = !state.clipboard_enabled;
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
            finish_for_settings();
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

/// Commit the drafted shortcut through the same path the settings page uses. Separate from
/// `config_with_step_selection` on purpose: the display mode on that step applies on click, the
/// shortcut applies when the user confirms the step.
fn commit_modifier_selection() {
    let modifier = {
        let slot = STATE.lock().unwrap();
        let Some(state) = slot.as_ref() else {
            return;
        };
        crate::config::switcher_modifier_value_at(state.modifier).to_string()
    };
    let old = CONFIG.read().unwrap().clone();
    if old.keyboard.modifier == modifier {
        return;
    }
    let mut new = old.clone();
    new.keyboard.modifier = modifier;
    apply_config(&old, &new);
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
    // The shortcut is confirmed by leaving the step. It is kept apart from the display mode because
    // that one applies on click (it has to: picking thumbnails asks for Screen Recording then), and
    // riding along with it would write a modifier the user never confirmed by leaving the step.
    if current_step == Step::DisplayMode {
        commit_modifier_selection();
    }
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
/// the configured clipboard hotkey both read the live `CONFIG`, so a deferred draft would
/// make the step's own instructions do nothing. The draft is updated first so a re-render keeps the
/// selection; `commit_step_selection` no-ops when the value already matches.
fn apply_choice_now(step: Step, update: impl FnOnce(&mut UiState)) {
    update_draft_and_commit(step, update);
    render_current_step();
}

/// Applies a clipboard-step choice. The only remaining choice is the master switch, which shows or
/// hides the storage note, so the page is always redrawn.
fn apply_clipboard_choice(update: impl FnOnce(&mut UiState)) {
    update_draft_and_commit(Step::ClipboardHistory, update);
    render_current_step();
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
        Step::DisplayMode => {
            // The display mode applies the moment it is picked (picking thumbnails has to ask for
            // Screen Recording then), so it commits on its own. The modifier is NOT part of that:
            // it would ride along with a click that never said anything about the shortcut, and
            // `advance()` is what commits it (see `modifier_step_selection`).
            new.layout.thumbnails_enabled = state.thumbnails_enabled;
        }
        Step::ClipboardHistory => {
            new.clipboard.enabled = state.clipboard_enabled;
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
    // The guide and the settings page share one select table, keyed by view address. A step that is
    // re-rendered must leave exactly its own selects behind: a stale entry survives the view it names
    // and the next sweep sends `window` to a freed address.
    use crate::settings::select::{registered_select_count, SelectOwner};
    let selects_before = registered_select_count(SelectOwner::Onboarding);
    let before_thumbnails = CONFIG.read().unwrap().layout.thumbnails_enabled;
    show_internal(PermissionOverride::default());
    // The resume marker must exist while the guide is on screen (an unexpected exit never delivers
    // the termination notification, and the completion marker alone would swallow the guide for
    // good), and it must follow the step.
    let resume_after_show = peeking_resume_step() == Some(0);
    // Step 1 -> 2. Committing step 1 is a no-op here: its draft mirrors the loaded config.
    advance();
    let resume_after_advance = peeking_resume_step() == Some(1);
    // Step 2 is the only step with a select. One render must leave exactly one registration, and a
    // re-render of the same step must replace it rather than add to it.
    let selects_step_two = registered_select_count(SelectOwner::Onboarding);
    render_current_step();
    let selects_after_rerender = registered_select_count(SelectOwner::Onboarding);
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

    // The display mode applies on click; the shortcut next to it must not ride along. Pick a
    // modifier that differs from the live one and check the config still holds the old value.
    let before_modifier = CONFIG.read().unwrap().keyboard.modifier.clone();
    let drafted_modifier = if before_modifier == "control" { 1 } else { 2 };
    if let Some(state) = STATE.lock().unwrap().as_mut() {
        state.modifier = drafted_modifier;
    }
    let segment_again = if expected_thumbnails { 0isize } else { 1isize };
    let _ = unsafe { dispatch_display_mode_segment(segment_again) };
    let modifier_held_by_display_click =
        CONFIG.read().unwrap().keyboard.modifier == before_modifier;

    // Leaving step 2 is what confirms the shortcut, so this is the one moment it may reach the
    // config. Checked before the next `advance()`, which commits the clipboard step instead.
    advance();
    let modifier_committed_on_advance = CONFIG.read().unwrap().keyboard.modifier
        == crate::config::switcher_modifier_value_at(drafted_modifier);
    // Step 3 (the clipboard step). Its persist row only exists while history is on, so bring the
    // master switch to a known ON state first: the loaded config may start either way (the ignored
    // test runs both), and a blind toggle from ON would turn it off and remove that very row.
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
    // Turning history off must reach the live config, drop the in-memory history and delete what
    // the session wrote to disk (`clear_history_and_disk`).
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
    let history_cleared_on_disable = !crate::clipboard::history_file_exists();
    // Picking thumbnails asks the system for Screen Recording in a real run; a smoke run must not
    // have asked (see the `--smoke` guard in thumbnail::request_permission_once).
    let permission_untouched = !crate::thumbnail::permission_prompted();
    let window_shown = is_visible();
    hide();
    // Finishing/skipping ends the flow: no pending resume may survive it.
    let resume_cleared = peeking_resume_step().is_none();
    log_info!(
        "[smoke-onboarding-live-apply] segment_dispatched={} thumbnails_applied={} thumbnails_draft={:?} master_switch_present={} switch_dispatched={} master_on={} clipboard_draft={:?} disable_dispatched={} master_off={} history_cleared_on_disable={} permission_untouched={} resume_after_show={} resume_after_advance={} resume_cleared={} window={}",
        segment_dispatched,
        thumbnails_applied,
        thumbnails_draft,
        master_switch_present,
        switch_dispatched,
        master_on,
        clipboard_draft,
        disable_dispatched,
        master_off,
        history_cleared_on_disable,
        permission_untouched,
        resume_after_show,
        resume_after_advance,
        resume_cleared,
        window_shown
    );
    if !modifier_held_by_display_click || !modifier_committed_on_advance {
        eprintln!(
            "[smoke-onboarding-live-apply] shortcut ownership wrong: held_on_display_click={} committed_on_advance={}",
            modifier_held_by_display_click, modifier_committed_on_advance
        );
    }
    // One select on step 2, still one after a re-render: an accumulating or surviving registration is
    // the stale-address case.
    let select_registration_stable =
        selects_before == 0 && selects_step_two == 1 && selects_after_rerender == 1;
    // Leaving step 2 for a step with no select must release it. This is the page-turn path, not the
    // window-close path: `hide()` only hides a cached window, so the zero measured here comes from
    // step 3's render having forgotten step 2's control.
    hide();
    let selects_after_leaving_step_two = registered_select_count(SelectOwner::Onboarding);
    let select_released_on_step_change = selects_after_leaving_step_two == 0;
    if !select_registration_stable || !select_released_on_step_change {
        eprintln!(
            "[smoke-onboarding-live-apply] select registration wrong: before={} step_two={} after_rerender={} after_leaving_step_two={}",
            selects_before, selects_step_two, selects_after_rerender, selects_after_leaving_step_two
        );
    }
    segment_dispatched
        && thumbnails_applied
        && thumbnails_draft == Some(expected_thumbnails)
        && master_switch_present
        && switch_dispatched
        && master_on
        && clipboard_draft == Some(true)
        && disable_dispatched
        && master_off
        && history_cleared_on_disable
        && permission_untouched
        && resume_after_show
        && resume_after_advance
        && resume_cleared
        && window_shown
        && modifier_held_by_display_click
        && modifier_committed_on_advance
        && select_registration_stable
        && select_released_on_step_change
}

/// The guide's window: 640×510, the welcome reference's fixed dialog size. Step content is laid
/// out top-down (like the reference) and converted once by `y_from_top` into the bottom-up
/// origin AppKit uses.
const WINDOW_W: f64 = 640.0;
const WINDOW_H: f64 = 510.0;
/// Titled | Closable | Miniaturizable (without Resizable — every layout constant assumes the
/// fixed 640×510 dialog; see the mask test). The flow hides the three standard window buttons
/// below, so the guide carries no visible traffic lights (the reference's mockup dialog has
/// none either) while the window keeps what pure borderless takes away: keyboard eligibility
/// and a native drag-capable window surface.
const WINDOW_STYLE_MASK: u64 = (1 << 0) | (1 << 1) | (1 << 2);
/// NSWindowStyleMaskFullSizeContentView: the content reaches the window edge so the guide draws
/// its own title band and footer. Same chrome approach as the settings window; AppKit keeps the
/// traffic lights on top.
const STYLE_MASK_FULL_SIZE_CONTENT: u64 = 1 << 15;
/// 1 = NSWindowTitleHidden.
const WINDOW_TITLE_HIDDEN: isize = 1;

/// Title band height (the reference's `.bar`), full window width.
const BAND_H: f64 = 48.0;
/// Footer height (the reference's `.foot`), under a 1pt separator hairline at its top edge.
const FOOT_H: f64 = 60.0;
/// Page side edge (§5 page edges); the band and footer use the smaller 24pt chrome inset, the
/// same split the mock's bar/foot paddings make against the step padding.
const PAGE_PAD: f64 = 32.0;
const CHROME_PAD: f64 = 24.0;

/// The footer buttons' label type (14pt regular, the §4 `control` role).
const FOOTER_BUTTON_LABEL_STYLE: LabelStyle = LabelStyle {
    size: crate::theme::FONT_CONTROL,
    weight: crate::theme::FONT_WEIGHT_REGULAR,
    color: LabelColorRole::Primary,
    wrap: false,
};
/// Card geometry (§5 settings metrics: rows 52 tall, inner padding 16, card radius 12).
const CARD_W: f64 = WINDOW_W - PAGE_PAD * 2.0;
const ROW_H: f64 = 52.0;
const CARD_INSET: f64 = 16.0;

const GAP: f64 = 8.0;
const BUTTON_H: f64 = 32.0;
const BUTTON_W: f64 = 132.0;
const BUTTON_Y: f64 = (FOOT_H - BUTTON_H) / 2.0;

/// Title band bar height (the reference's `.bar`)
const TITLE_LINE_H: f64 = crate::theme::line_height(
    crate::theme::FONT_PAGE_TITLE,
    crate::theme::LINE_HEIGHT_TITLE_RATIO,
);
/// Wrapped body copy: the measured lede height is what the card cursor uses (a longer locale
/// pushes the card instead of clipping), but four wrapped lines is the budget every locale must
/// stay inside so the card+note stack still clears the footer.
const BODY_LINE_H: f64 = crate::theme::line_height(
    crate::theme::FONT_CONTROL,
    crate::theme::LINE_HEIGHT_BODY_RATIO,
);
const CAPTION_LINE_H: f64 = crate::theme::line_height(
    crate::theme::FONT_CAPTION,
    crate::theme::LINE_HEIGHT_CAPTION_RATIO,
);
const TITLE_TOP: f64 = BAND_H + 24.0;
const LEDE_TOP: f64 = TITLE_TOP + TITLE_LINE_H + 8.0;
const BODY_MAX_H: f64 = 4.0 * BODY_LINE_H;

/// Trailing status lane inside a card row: both measured label widths fit 160pt, and the lane is
/// what bounds the leading labels (see the measurement note in the tests).
const STATUS_MAX_W: f64 = 160.0;
/// Legend chip for the clipboard shortcut row: one chip per logical key (§6.1), 20pt tall.
const LEGEND_CHIP_H: f64 = 20.0;
const LEGEND_CHIP_GAP: f64 = 2.0;

/// Display-mode segmented control (the reference's `.segmented`): a 320×30 field container with
/// two 24pt segments inset by 2pt (a 2pt step inside the control's own geometry, §5).
const SEGMENTED_W: f64 = 320.0;
const SEGMENTED_H: f64 = 30.0;
const SEGMENT_PAD: f64 = 2.0;
const SEGMENT_H: f64 = SEGMENTED_H - 2.0 * SEGMENT_PAD;
const SEGMENT_W: f64 = (SEGMENTED_W - 3.0 * SEGMENT_PAD) / 2.0;

/// Bottom-up y for a top-down (flipped) `top` edge plus height: the window content is not
/// flipped.
fn y_from_top(top: f64, h: f64) -> f64 {
    WINDOW_H - top - h
}

/// The guide only needs its step-copy labels plus the chrome styles, kept as constants so each
/// call site does not repeat the parameters.
#[derive(Clone, Copy)]
struct LabelStyle {
    size: f64,
    weight: f64,
    color: LabelColorRole,
    wrap: bool,
}
#[derive(Clone, Copy)]
enum LabelColorRole {
    Primary,
    Secondary,
    Muted,
    Success,
    Warning,
}

impl LabelColorRole {
    fn resolve(self) -> u32 {
        self.resolve_from(crate::theme::ui_palette())
    }

    fn resolve_from(self, palette: crate::theme::UiPalette) -> u32 {
        match self {
            Self::Primary => palette.primary_text,
            Self::Secondary => palette.secondary_text,
            Self::Muted => palette.muted_text,
            Self::Success => palette.success_text,
            Self::Warning => palette.warning_text,
        }
    }
}

const STEP_TITLE_STYLE: LabelStyle = LabelStyle {
    size: crate::theme::FONT_PAGE_TITLE,
    weight: crate::theme::FONT_WEIGHT_BOLD,
    color: LabelColorRole::Primary,
    wrap: false,
};
/// The band's window title (the reference's `.bar-title`: 14pt semibold), centered on the window.
const BAND_TITLE_STYLE: LabelStyle = LabelStyle {
    size: crate::theme::FONT_CONTROL,
    weight: crate::theme::FONT_WEIGHT_SEMIBOLD,
    color: LabelColorRole::Primary,
    wrap: false,
};
/// The band's `1 / 4` step counter (the reference's `.step-meta`), right-aligned.
const META_STYLE: LabelStyle = LabelStyle {
    size: crate::theme::FONT_CAPTION,
    weight: crate::theme::FONT_WEIGHT_REGULAR,
    color: LabelColorRole::Muted,
    wrap: false,
};
/// Card row name (single line) and description under it (§4 caption role).
const ROW_LABEL_STYLE: LabelStyle = LabelStyle {
    size: crate::theme::FONT_CONTROL,
    weight: crate::theme::FONT_WEIGHT_REGULAR,
    color: LabelColorRole::Primary,
    wrap: false,
};
const BODY_STYLE: LabelStyle = LabelStyle {
    size: crate::theme::FONT_CONTROL,
    weight: crate::theme::FONT_WEIGHT_REGULAR,
    color: LabelColorRole::Secondary,
    wrap: true,
};
/// Small muted explanatory text (row descriptions, notes under a card).
const HINT_STYLE: LabelStyle = LabelStyle {
    size: crate::theme::FONT_CAPTION,
    weight: crate::theme::FONT_WEIGHT_REGULAR,
    color: LabelColorRole::Muted,
    wrap: true,
};
/// The `.field-label` over the segmented control (12pt semibold secondary).
const FIELD_LABEL_STYLE: LabelStyle = LabelStyle {
    size: crate::theme::FONT_CAPTION,
    weight: crate::theme::FONT_WEIGHT_SEMIBOLD,
    color: LabelColorRole::Secondary,
    wrap: false,
};
fn status_style(color: LabelColorRole) -> LabelStyle {
    LabelStyle {
        size: crate::theme::FONT_CAPTION,
        weight: crate::theme::FONT_WEIGHT_SEMIBOLD,
        color,
        wrap: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smoke_verdict::smoke_runner;

    #[test]
    fn onboarding_chrome_uses_the_shared_type_and_color_roles() {
        assert_eq!(
            [
                META_STYLE.size,
                HINT_STYLE.size,
                ROW_LABEL_STYLE.size,
                BODY_STYLE.size,
                FIELD_LABEL_STYLE.size,
                BAND_TITLE_STYLE.size,
                status_style(LabelColorRole::Warning).size,
                STEP_TITLE_STYLE.size,
            ],
            [12.0, 12.0, 14.0, 14.0, 12.0, 14.0, 12.0, 26.0]
        );
        // The status caption is semibold; the band title and the field label sit between the
        // regular and bold families (the sheet's 600 weight).
        assert_ne!(FIELD_LABEL_STYLE.weight, crate::theme::FONT_WEIGHT_REGULAR);
        assert_eq!(
            status_style(LabelColorRole::Warning).weight,
            crate::theme::FONT_WEIGHT_SEMIBOLD
        );

        let light = crate::theme::ui_palette_for_mode(false);
        let dark = crate::theme::ui_palette_for_mode(true);
        for palette in [light, dark] {
            assert_eq!(
                LabelColorRole::Primary.resolve_from(palette),
                palette.primary_text
            );
            assert_eq!(
                LabelColorRole::Secondary.resolve_from(palette),
                palette.secondary_text
            );
            assert_eq!(
                LabelColorRole::Muted.resolve_from(palette),
                palette.muted_text
            );
            assert_eq!(
                LabelColorRole::Success.resolve_from(palette),
                palette.success_text
            );
            assert_eq!(
                LabelColorRole::Warning.resolve_from(palette),
                palette.warning_text
            );
        }
        assert!((BODY_LINE_H - 19.6).abs() < f64::EPSILON * 128.0);
        // The bottom-up conversion is exact; the title's top edge lands on the 4pt grid from
        // the band's bottom edge (§5 spacing).
        assert_eq!(y_from_top(TITLE_TOP, 0.0), WINDOW_H - TITLE_TOP);
        assert_eq!(y_from_top(0.0, 10.0), WINDOW_H - 10.0);
        assert_eq!(TITLE_TOP % 4.0, 0.0);
    }

    #[test]
    fn wrapped_line_height_ratio_tracks_caption_body_and_title_roles() {
        assert_eq!(
            wrapped_line_height_ratio(crate::theme::FONT_CAPTION),
            crate::theme::LINE_HEIGHT_CAPTION_RATIO
        );
        assert_eq!(
            wrapped_line_height_ratio(crate::theme::FONT_CONTROL),
            crate::theme::LINE_HEIGHT_BODY_RATIO
        );
        assert_eq!(
            wrapped_line_height_ratio(crate::theme::FONT_PAGE_TITLE),
            crate::theme::LINE_HEIGHT_TITLE_RATIO
        );
    }

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

    /// The inline menu-icon run must carry an explicit foreground color and a NON-template
    /// image: TextKit 1 does not tint template attachment images, so a raw template asset
    /// draws its black pixels verbatim (dark-on-dark in dark mode). The bake-in in
    /// `inline_icon_attachment_run` is the fix; this test pins both guards.
    #[test]
    fn inline_icon_run_is_tinted_with_the_label_color() {
        // Image drawing and the conveniences inside need an autorelease pool; the test
        // thread has none of the app's runloop plumbing.
        let pool: *mut AnyObject = unsafe { msg_send![class!(NSAutoreleasePool), new] };
        let run = unsafe { inline_icon_attachment_run(BODY_STYLE) };
        let Some(run) = run else {
            unsafe {
                let _: () = msg_send![pool, drain];
            }
            panic!("the bundled statusbar icon must load");
        };
        unsafe {
            let length: usize = msg_send![run, length];
            assert_eq!(length, 1, "the attachment run holds exactly one character");

            let attachment_key = make_nsstring("NSAttachment");
            let mut attachment_range = NSRange::new(0, 0);
            let attachment: *mut AnyObject = msg_send![
                run,
                attribute: attachment_key,
                atIndex: 0usize,
                effectiveRange: &mut attachment_range
            ];
            release_obj(attachment_key);
            assert!(!attachment.is_null(), "the run must carry the attachment");
            let image: *mut AnyObject = msg_send![attachment, image];
            assert!(!image.is_null());
            let is_template: bool = msg_send![image, isTemplate];
            assert!(
                !is_template,
                "the icon image must be pre-tinted, not a template"
            );

            let color_key = make_nsstring("NSColor");
            let mut range = NSRange::new(0, 0);
            let color: *mut AnyObject = msg_send![
                run,
                attribute: color_key,
                atIndex: 0usize,
                effectiveRange: &mut range
            ];
            release_obj(color_key);
            assert!(!color.is_null(), "the icon run must carry NSColor");
            assert_eq!(range.location, 0);
            assert_eq!(range.length, 1, "the color must cover the whole run");
            let expected = hex_to_ns_color(BODY_STYLE.color.resolve());
            let equal: bool = msg_send![color, isEqual: expected];
            assert!(equal, "the icon tint must equal the label text color");
            let _: () = msg_send![pool, drain];
        }
    }

    /// Every color the guide paints is pinned to the welcome reference's own values
    /// (/Users/ceres/Downloads/onboarding.html), so a palette edit that quietly shifts the
    /// guide off the reference fails here (light mode; dark mode had no values in that HTML
    /// and answers to the design document's dark sheet instead).
    #[test]
    fn onboarding_palette_matches_the_reference_colors() {
        let light = crate::theme::ui_palette_for_mode(false);
        assert_eq!(light.window_bg, 0xECECECFF, "--window #ECECEC");
        assert_eq!(light.card_bg, 0xF9F9F9FF, "--surface #F9F9F9");
        assert_eq!(light.primary_text, 0x1D1C1AFF, "--ink #1D1C1A");
        assert_eq!(light.secondary_text, 0x4F4C48FF, "--ink-2 #4F4C48");
        assert_eq!(light.muted_text, 0x6B6862FF, "--muted #6B6862");
        assert_eq!(light.card_border, 0x2D28241A, "--border rgba(45,40,36,.10)");
        assert_eq!(
            light.separator, 0x2D282412,
            "--separator rgba(45,40,36,.07)"
        );
        assert_eq!(light.field_bg, 0x3C342E0E, "--field rgba(60,52,46,.055)");
        assert_eq!(
            light.switch_off_track, 0x3C342E33,
            "--switch-off rgba(60,52,46,.20)"
        );
        assert_eq!(light.accent, 0x2F6FEDFF, "--accent #2F6FED");
        assert_eq!(light.accent_hover, 0x1F5CDAFF, "--accent-hover #1F5CDA");
        assert_eq!(light.success_text, 0x176B3AFF, "--success #176B3A");
        assert_eq!(
            light.footer_button_bg, 0xFFFFFFFF,
            ".btn.secondary background var(--white)"
        );
        assert_eq!(light.hover_bg, 0x7676801F, "--hover rgba(118,118,128,.12)");
    }

    /// Embedded locale sources for the layout checks below; the assertions run over all three
    /// locales so a future translation is covered.
    const LOCALE_SOURCES: [(&str, &str); 3] = [
        ("en", include_str!("../locales/en.toml")),
        ("zh-Hans", include_str!("../locales/zh-Hans.toml")),
        ("zh-Hant", include_str!("../locales/zh-Hant.toml")),
    ];

    fn locale_onboarding_string(raw: &str, key: &str) -> String {
        let parsed: toml::Value = toml::from_str(raw).expect("locale parses");
        parsed["onboarding"][key]
            .as_str()
            .unwrap_or_else(|| panic!("onboarding.{key} missing"))
            .to_string()
    }

    /// The permissions row's trailing status lane is the measured minimum both status strings
    /// were reserved at (160pt); the leading name column must clear it, and every locale must
    /// define the strings the rows render.
    #[test]
    fn onboarding_row_copies_fit_the_trailing_lanes() {
        // Status lane + inner paddings + a name column that fits the measured long labels
        // (the name column alone has over 300pt of room for the longest translated label).
        let inner_right = PAGE_PAD + CARD_W - CARD_INSET;
        assert!(PAGE_PAD + CARD_INSET <= inner_right - STATUS_MAX_W - 8.0);
        for (locale, raw) in LOCALE_SOURCES {
            for key in [
                "accessibility_label",
                "accessibility_desc",
                "launch_label",
                "launch_desc",
                "clipboard_label",
                "clipboard_desc",
                "clipboard_open_shortcut",
                "display_mode_label",
                "screen_permission_granted",
                "screen_permission_missing",
                "screen_permission_needed",
                "status_granted",
                "status_missing",
                "status_restart_required",
            ] {
                assert!(
                    !locale_onboarding_string(raw, key).is_empty(),
                    "{locale}: onboarding.{key} is empty"
                );
            }
        }
    }

    /// The step stack must clear the footer in the worst case: the measured lede is bounded by
    /// BODY_MAX_H, and a two-row card plus the storage note below it must end before the footer
    /// hairline (top-down geometry; pure math so layout edits fail without a GUI).
    #[test]
    fn onboarding_step_stack_clears_the_footer() {
        const {
            assert!(LEDE_TOP + BODY_MAX_H + 24.0 > TITLE_TOP + TITLE_LINE_H);
        };
        let worst_body_bottom = LEDE_TOP + BODY_MAX_H;
        let card_top = worst_body_bottom + 24.0;
        let card_bottom = card_top + ROW_H * 2.0;
        let note_top = card_bottom + 12.0;
        // The storage note reserves two caption lines (§4 caption line height).
        let note_bottom = note_top + 2.0 * CAPTION_LINE_H;
        assert!(
            note_bottom <= WINDOW_H - FOOT_H,
            "the clipboard card+note stack reaches {note_bottom}pt: footer starts at {}",
            WINDOW_H - FOOT_H
        );
        // The control's permission line follows the segmented control and clears the footer.
        let segmented_bottom = card_top + CAPTION_LINE_H + 8.0 + SEGMENTED_H;
        assert!(segmented_bottom <= WINDOW_H - FOOT_H);
        let permission_bottom = segmented_bottom + 16.0 + 2.0 * CAPTION_LINE_H + 2.0;
        assert!(
            permission_bottom <= WINDOW_H - FOOT_H,
            "the display step's permission line reaches {permission_bottom}"
        );
    }

    /// Shortcut chips: one chip per logical key via the shared trigger parser; an unparsable
    /// value falls back to the raw display string as a single chip.
    #[test]
    fn shortcut_chips_are_one_chip_per_logical_key() {
        assert_eq!(shortcut_chips("option+v"), vec!["⌥", "V"]);
        assert_eq!(shortcut_chips("cmd+shift+v"), vec!["⌘", "⇧", "V"]);
        assert_eq!(shortcut_chips("control+pageup"), vec!["⌃", "Page Up"]);
        // The fallback is a single chip (never one chip per character, §6.1).
        assert_eq!(shortcut_chips("no-such-chord"), vec!["no-such-chord"]);
    }

    /// The window mask must keep AppKit's traffic lights: the plain `1 << 0` (then named
    /// "titled") is Borderless, which removed every standard window button, so the guide had
    /// no close button and `band_leading_edge` always fell back to the constant.
    #[test]
    fn window_mask_is_dialog_fixed_and_buttonless() {
        // Titled|Closable|Miniaturizable (7), no Resizable: titled keeps the window
        // key-eligible (a borderless window cannot receive keyboard events at all) while the
        // traffic lights are hidden by the flow; the fixed 640×510 layout must never be
        // resizable, since there are no size constraints.
        assert_eq!(
            WINDOW_STYLE_MASK, 7,
            "Titled|Closable|Miniaturizable, no Resizable bit"
        );
        assert_eq!(
            WINDOW_STYLE_MASK & (1 << 3),
            0,
            "Resizable must stay off: the fixed-layout guide has no size constraints"
        );
        let supports: bool = unsafe {
            msg_send![
                class!(NSWindow),
                instancesRespondToSelector: sel!(standardWindowButton:)
            ]
        };
        assert!(
            supports,
            "band_leading_edge measures the (hidden) traffic lights via standardWindowButton:"
        );
        // With all buttons hidden the leading row falls back to the mock's own 18pt padding.
        assert_eq!(BAND_LEADING_INSET, 18.0);
    }

    /// Keyboard activation must select the FOCUSED segment, never the record's current
    /// selection (space on the non-selected segment used to re-select the already-selected
    /// one and could never reach the new mode); arrows flip the group; other keys pass
    /// through. Pure decider, so `segment_key_down` only plugs the event in.
    #[test]
    fn segment_keys_select_the_focused_segment() {
        let space = segment_key_selection(49, 0, 1);
        assert_eq!(
            space,
            Some(1),
            "space must select the focused child, not {space:?}"
        );
        assert_eq!(segment_key_selection(36, 0, 1), Some(1), "return too");
        assert_eq!(segment_key_selection(76, 1, 0), Some(0), "keypad enter too");
        // Arrows flip a two-option radio group regardless of which child is focused.
        assert_eq!(segment_key_selection(123, 0, 0), Some(1));
        assert_eq!(segment_key_selection(124, 0, 1), Some(1));
        assert_eq!(segment_key_selection(125, 1, 0), Some(0));
        assert_eq!(segment_key_selection(126, 1, 1), Some(0));
        // Unhandled keys pass through.
        assert_eq!(segment_key_selection(48, 0, 0), None);
        assert_eq!(segment_key_selection(51, 1, 1), None);
    }

    /// VoiceOver needs an activation action and a boolean checked value on the custom radio
    /// segments; both are registered on the class and the value helper produces a boolean
    /// NSNumber of the given state (headless: class/method checks, no instance needed).
    #[test]
    fn segment_accessibility_contract_is_registered() {
        let responds: bool = unsafe {
            msg_send![
                segment_child_class_handle().0,
                instancesRespondToSelector: sel!(accessibilityPerformPress)
            ]
        };
        assert!(
            responds,
            "the segment class must expose accessibilityPerformPress"
        );
        unsafe {
            // objc2 decodes ObjC BOOL ('c' on arm64) correctly as `runtime::Bool`; a plain
            // `bool` read of the same message was measured wrong here.
            let on_value = segment_a11y_value(true);
            let is_number: objc2::runtime::Bool =
                msg_send![on_value, isKindOfClass: class!(NSNumber)];
            assert!(
                is_number.as_bool(),
                "the checked value must be an NSNumber, not a string"
            );
            let bool_value: objc2::runtime::Bool = msg_send![on_value, boolValue];
            assert!(
                bool_value.as_bool(),
                "a checked segment's value must be true"
            );
            let off_value = segment_a11y_value(false);
            let off: objc2::runtime::Bool = msg_send![off_value, boolValue];
            assert!(!off.as_bool(), "an unchecked segment's value must be false");
        }
    }

    /// The segmented control's geometry: the two segments with their 2pt insets and gap fill the
    /// container exactly, and the inner radius derives from the outer (concentric rule).
    #[test]
    fn segmented_geometry_fills_the_container() {
        assert_eq!(2.0 * SEGMENT_W + 3.0 * SEGMENT_PAD, SEGMENTED_W);
        assert!(SEGMENT_W > 100.0, "labels need room to stay on one line");
        assert_eq!(SEGMENT_H, SEGMENTED_H - 2.0 * SEGMENT_PAD);
        assert_eq!(
            crate::theme::RADIUS_CONTROL - SEGMENT_PAD,
            6.0,
            "the segment radius derives from the 8pt control radius minus the 2pt inset"
        );
    }

    /// The final footer CTA ("Done and open Settings") must keep every locale's label on one
    /// line: the button width is measured per render, so the test asserts the same measurement
    /// stays inside a sane cap (and that the two footer copy keys exist for every locale).
    #[test]
    fn footer_final_cta_labels_fit_the_measured_width() {
        unsafe {
            for (locale, raw) in LOCALE_SOURCES {
                for key in ["btn_finish", "btn_skip_guide"] {
                    let title = locale_onboarding_string(raw, key);
                    assert!(!title.is_empty(), "{locale}: onboarding.{key} is empty");
                    let label_w = single_line_text_width(&title, FOOTER_BUTTON_LABEL_STYLE);
                    assert!(
                        label_w <= WINDOW_W - 2.0 * CHROME_PAD - BUTTON_W - GAP,
                        "{locale}: the {key} label measures {label_w}pt and would not fit the footer"
                    );
                }
            }
        }
        // Next keeps the fixed width; the final CTA is what grows to its own label.
        let btn_next = locale_onboarding_string(LOCALE_SOURCES[0].1, "btn_next");
        unsafe {
            assert!(
                single_line_text_width(&btn_next, FOOTER_BUTTON_LABEL_STYLE) <= BUTTON_W - 16.0
            );
        }
    }

    /// Every locale's band title and step counter must render inside its band geometry; the
    /// measured width is capped so the centered title never collides with the right-aligned
    /// counter (AppKit heights measured live, since the helper needs no window).
    #[test]
    fn band_chrome_measured_sizes_fit_the_48pt_band() {
        unsafe {
            for (locale, raw) in LOCALE_SOURCES {
                let title = locale_onboarding_string(raw, "window_title");
                let title_w = single_line_text_width(&title, BAND_TITLE_STYLE);
                assert!(
                    title_w <= 240.0,
                    "{locale}: the band title measures {title_w}pt, over the clamp"
                );
                let counter = locale_onboarding_string(raw, "step_counter");
                assert!(counter.contains("{current}"));
            }
        }
        const {
            assert!((BAND_H - BODY_LINE_H) / 2.0 > 0.0);
            assert!((BAND_H - CAPTION_LINE_H) / 2.0 > 0.0);
            assert!((FOOT_H - BUTTON_H) / 2.0 > 0.0);
        };
    }

    /// The clipboard storage note is two caption lines at most in every locale, so the
    /// card-note stack measured by `onboarding_step_stack_clears_the_footer` stays honest (the
    /// runtime sizes the note by this same measurement; this test is the locale gate). Measured
    /// note of 34pt used to be the fixed box; the note now follows the measurement, so the cap
    /// pinning the stack is the guard.
    #[test]
    fn clipboard_storage_note_fits_every_locale() {
        let box_w = CARD_W;
        for (locale, raw) in LOCALE_SOURCES {
            let note = locale_onboarding_string(raw, "clipboard_storage_note");
            assert!(
                !note.is_empty(),
                "{locale}: onboarding.clipboard_storage_note is empty"
            );
            let measured = unsafe { measured_text_height(note.as_str(), box_w, HINT_STYLE) };
            assert!(
                measured <= 2.0 * CAPTION_LINE_H + 2.0,
                "{locale}: the storage note needs {measured:.1}pt but the stack budget is two lines"
            );
        }
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
    fn clipboard_onboarding_body_displays_the_configured_custom_chord() {
        let mut config = Config::default();
        config.clipboard.shortcut = "cmd+shift+v".into();
        for enabled in [false, true] {
            let body = clipboard_shortcut_body(&config.clipboard.shortcut, enabled);
            assert!(body.contains("⌘⇧V"), "{body}");
            assert!(!body.contains("Option+V"), "{body}");
        }
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
            modifier: 1,
            clipboard_enabled: true,
            clipboard_shortcut: "option+v".into(),
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

    /// The display mode is committed when it is clicked (it has to be: picking thumbnails asks for
    /// Screen Recording then). The shortcut must NOT ride along -- a user who picks a modifier and
    /// then changes the display mode has said nothing about confirming the shortcut.
    #[test]
    fn the_display_mode_does_not_carry_the_drafted_shortcut() {
        let old = Config::default();
        let draft = UiState {
            steps: steps_for(),
            index: 1,
            overrides: PermissionOverride::default(),
            launch_at_login: false,
            thumbnails_enabled: true,
            modifier: crate::config::switcher_modifier_index_of("control"),
            clipboard_enabled: false,
            clipboard_shortcut: "option+v".into(),
        };
        let after = config_with_step_selection(&old, &draft, Step::DisplayMode);
        assert!(after.layout.thumbnails_enabled);
        assert_eq!(
            after.keyboard.modifier, old.keyboard.modifier,
            "the shortcut is confirmed by leaving the step, not by picking a display mode"
        );
    }

    /// Leaving the step is what commits it, so the draft helper must resolve to the config value.
    #[test]
    fn the_drafted_shortcut_resolves_to_the_config_value() {
        for value in ["option", "command", "control"] {
            let index = crate::config::switcher_modifier_index_of(value);
            assert_eq!(crate::config::switcher_modifier_value_at(index), value);
        }
    }
    /// The persist switch mirrors `config.clipboard.persist`: the clipboard step commits it, other
    /// steps leave it alone, and a disabled master switch keeps the draft so re-enabling history
    /// restores the previous choice instead of resetting it.
    #[test]
    fn onboarding_clipboard_step_commits_persist_and_keeps_it_while_disabled() {
        let old = Config::default();
        let draft = UiState {
            steps: steps_for(),
            index: 0,
            overrides: PermissionOverride::default(),
            launch_at_login: false,
            thumbnails_enabled: false,
            modifier: 1,
            clipboard_enabled: false,
            clipboard_shortcut: "option+v".into(),
        };
        let disabled = config_with_step_selection(&old, &draft, Step::ClipboardHistory);
        assert!(!disabled.clipboard.enabled);
        // A step that owns no clipboard field must not leak the draft into the config.
        let display = config_with_step_selection(&old, &draft, Step::DisplayMode);
        assert_eq!(display.clipboard.enabled, old.clipboard.enabled);
    }

    /// The clipboard page's rows must not overlap: the storage note clears the footer buttons, the
    /// master row sits above the note, and the shortcut hint and body sit above the master row.
    /// Kept as a pure check so layout edits fail without a GUI.
    #[test]
    fn clipboard_step_rows_stay_ordered_and_clear_the_footer() {
        // The clipboard card carries exactly two rows: the master switch above the shortcut
        // legend, and the storage note follows the card while the rows never move.
        for (locale, raw) in LOCALE_SOURCES {
            for key in ["clipboard_body", "clipboard_body_enabled"] {
                assert!(
                    locale_onboarding_string(raw, key).contains("{shortcut}"),
                    "{locale}: onboarding.{key} must interpolate the effective binding"
                );
            }
            assert!(
                !locale_onboarding_string(raw, "clipboard_shortcut_hint").is_empty(),
                "{locale}: shortcut-change hint is missing"
            );
            assert!(
                !locale_onboarding_string(raw, "clipboard_desc").is_empty(),
                "{locale}: master-switch row description is missing"
            );
        }
        // The INFO keys the two rows show must exist and differ semantically where they should.
        assert_ne!(
            locale_onboarding_string(LOCALE_SOURCES[0].1, "clipboard_open_shortcut"),
            locale_onboarding_string(LOCALE_SOURCES[0].1, "clipboard_label"),
        );
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
    /// used to keep the old mode, and the step's displayed shortcut would do nothing. The runner
    /// commits through the real apply path, which persists, so it gets a throwaway HOME. It is run
    /// once with clipboard history off and once with it on, because the persist row only exists
    /// while history is on and the runner must not assume the loaded config starts off.
    #[test]
    #[ignore]
    fn onboarding_live_apply_smoke() {
        smoke_runner!("onboarding_live_apply_smoke", 1, {
            // The child is the process that ran the assertions, so its recorded verdict is
            // what this parent reports; it never infers one from the exit status.
            crate::smoke_verdict::run_smoke_child(
                "onboarding_live_apply_smoke",
                &["--smoke-onboarding-live-apply"],
            );
        });
    }
}
