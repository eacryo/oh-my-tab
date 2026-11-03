//! A transient, non-activating message at the bottom of the screen.
//!
//! The Finder menu's "Copy SHA-256" runs on a worker thread and takes as long as the file is big,
//! so the app has to say something between the click and the clipboard. The settings window's
//! success bubble cannot do that job: it is scoped to that window, which usually does not exist
//! while the user is working in Finder.
//!
//! The surface follows the same material every other floating panel uses (`glass::install_backdrop`,
//! the shared elevated corner radius and shadow), so it reads as part of the app rather than as a
//! bare grey rectangle. One panel, reused; state lives in `TOAST` on the main thread.

use std::cell::RefCell;
use std::sync::OnceLock;
use std::time::Duration;

use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use crate::ffi::{make_nsstring, release_obj, StaticClass};
use crate::theme;

/// Wide enough for one short sentence in every locale, at the control font.
const TOAST_WIDTH: f64 = 320.0;
/// One line of the control font, a 28pt icon, and breathing room above and below.
const TOAST_HEIGHT: f64 = 60.0;
/// Distance kept from the screen's visible bottom edge.
const TOAST_BOTTOM_MARGIN: f64 = 48.0;
/// Horizontal padding before the icon.
const TOAST_PADDING: f64 = 20.0;
/// The tick's box; the text starts after it plus the standard gap.
const TOAST_ICON_SIZE: f64 = 28.0;
const TOAST_ICON_GAP: f64 = 12.0;
/// How long a FINISHED message stays. A message still working has no deadline.
const TOAST_VISIBLE: Duration = Duration::from_millis(2500);

struct State {
    panel: *mut AnyObject,
    label: *mut AnyObject,
    icon: *mut AnyObject,
    backdrop: Option<crate::glass::InstalledBackdrop>,
    /// The lit inner bevel; kept so the view is not dropped while its parent still shows it.
    bevel: Option<crate::ffi::ObjPtr>,
    /// The pending auto-dismiss, cleared whenever the text changes or the panel is hidden.
    timer: *mut AnyObject,
}

thread_local! {
    static TOAST: RefCell<State> = const {
        RefCell::new(State {
            panel: std::ptr::null_mut(),
            label: std::ptr::null_mut(),
            icon: std::ptr::null_mut(),
            backdrop: None,
            bevel: None,
            timer: std::ptr::null_mut(),
        })
    };
}

/// Show `text` and keep it until `dismiss_after` elapses.
///
/// `dismiss_after: None` means "keep it up": that is the computing state, where any deadline would
/// expire before the work does on a large file, leaving the user unable to tell whether it is
/// still running. A later call replaces both the text and any pending timer.
pub(crate) fn show(text: &str, dismiss_after: Option<Duration>) {
    crate::debug_assert_main_thread();
    let shown = TOAST.with(|state| {
        let mut state = state.borrow_mut();
        unsafe { show_in(&mut state, text, dismiss_after) }
    });
    if !shown {
        crate::log_info!("[toast] could not present a message");
    }
}

/// Hide the panel now and cancel any pending dismissal.
#[cfg(test)]
pub(crate) fn hide() {
    crate::debug_assert_main_thread();
    TOAST.with(|state| {
        let mut state = state.borrow_mut();
        unsafe { hide_in(&mut state) }
    })
}

/// Whether a panel currently exists and is on screen.
#[cfg(test)]
pub(crate) fn is_shown() -> bool {
    TOAST.with(|state| {
        let state = state.borrow();
        !state.panel.is_null() && unsafe { msg_send![state.panel, isVisible] }
    })
}

unsafe fn show_in(state: &mut State, text: &str, dismiss_after: Option<Duration>) -> bool {
    if state.panel.is_null() {
        build(state);
        if state.panel.is_null() {
            return false;
        }
    }
    // Refresh the material on every presentation: the panel is reused across messages, and the
    // material or the theme may have changed since it was built. The keystroke display does the same
    // before each render rather than trusting the construction-time state.
    if let Some(backdrop) = state.backdrop {
        crate::glass::apply_live_properties(backdrop, toast_backdrop_options());
    }
    let text_ns = make_nsstring(text);
    let _: () = msg_send![state.label, setStringValue: text_ns];
    release_obj(text_ns);
    place(state.panel, state.bevel);
    let _: () = msg_send![state.panel, setAlphaValue: 0.0f64];
    let _: () = msg_send![state.panel, orderFrontRegardless];
    fade(state.panel, 1.0);
    cancel_timer(state);
    if let Some(delay) = dismiss_after {
        schedule_dismiss(state, delay);
    }
    true
}

unsafe fn hide_in(state: &mut State) {
    cancel_timer(state);
    if state.panel.is_null() {
        return;
    }
    // Fading out and then ordering out, unless the system asks for less motion, in which case the
    // panel goes at once -- a message nobody asked to see must not animate on its way out.
    if crate::theme::reduce_motion_enabled() {
        let _: () = msg_send![state.panel, orderOut: std::ptr::null_mut::<AnyObject>()];
        return;
    }
    let context: *mut AnyObject = msg_send![class!(NSAnimationContext), currentContext];
    if context.is_null() {
        let _: () = msg_send![state.panel, orderOut: std::ptr::null_mut::<AnyObject>()];
        return;
    }
    let _: () = msg_send![class!(NSAnimationContext), beginGrouping];
    let group: *mut AnyObject = msg_send![class!(NSAnimationContext), currentContext];
    let _: () = msg_send![group, setDuration: theme::ANIMATION_DURATION_FAST];
    let animator: *mut AnyObject = msg_send![state.panel, animator];
    let _: () = msg_send![animator, setAlphaValue: 0.0f64];
    let panel = state.panel;
    let completion: block2::RcBlock<dyn Fn()> = block2::RcBlock::new(move || {
        crate::callback_guard::void("toast_hide", || unsafe {
            let _: () = msg_send![panel, orderOut: std::ptr::null_mut::<AnyObject>()];
        });
    });
    let _: () = msg_send![group, setCompletionHandler: &*completion];
    let _: () = msg_send![class!(NSAnimationContext), endGrouping];
}

unsafe fn fade(panel: *mut AnyObject, alpha: f64) {
    if crate::theme::reduce_motion_enabled() {
        let _: () = msg_send![panel, setAlphaValue: alpha];
        return;
    }
    let _: () = msg_send![class!(NSAnimationContext), beginGrouping];
    let group: *mut AnyObject = msg_send![class!(NSAnimationContext), currentContext];
    let _: () = msg_send![group, setDuration: theme::ANIMATION_DURATION_FAST];
    let animator: *mut AnyObject = msg_send![panel, animator];
    let _: () = msg_send![animator, setAlphaValue: alpha];
    let _: () = msg_send![class!(NSAnimationContext), endGrouping];
}

/// The backdrop options, shared by construction and every live refresh: the native glass edge keeps
/// the system's specular outside the content clip, and the compensation keeps a panel that never
/// becomes key from reading half as substantial as the ones that do.
///
/// The compensation is strength-scaled the way the keystroke display scales it, and for the same
/// reason: this panel is passive, so AppKit dims the material, and a FULL-strength veil over clear
/// glass covers the material in an opaque fill. The user's glass then stops reading as glass. Clear
/// gets no veil at all, and the regular look gets a partial lift.
fn toast_backdrop_options() -> crate::glass::BackdropOptions {
    let tint = toast_compensation_tint(
        &crate::config::effective_glass_style(),
        theme::resolved_is_dark(),
    );
    crate::glass::BackdropOptions::new(Some(tint & 0xFF))
        .with_compensation_tint(tint)
        .with_elevation(Some(theme::ELEVATION_HIGH))
        .with_native_glass_edge()
        // The menu material, not the HUD one the other panels share. This surface borrows the look
        // of a context menu: it is a short-lived message, and `NSVisualEffectView` keeps that look
        // whether or not the window is key -- which is what a passive panel needs, because the
        // Liquid Glass material renders flat whenever its window loses key status.
        .with_frost_material(NS_VISUAL_EFFECT_MATERIAL_MENU)
}

/// `NSVisualEffectView.Material.menu`: the material menus and the Finder context menu use.
const NS_VISUAL_EFFECT_MATERIAL_MENU: i64 = 5;

/// The passive-panel lift for the current mode, with the alpha scaled by the glass style.
fn toast_compensation_tint(style: &str, dark: bool) -> u32 {
    let tint = if dark {
        theme::HUD_GLASS_COMPENSATION_DARK
    } else {
        theme::HUD_GLASS_COMPENSATION_LIGHT
    };
    if crate::glass::glass_style_index(style) == 1 {
        // Clear: no veil. The panel must stay as translucent as the style asks for.
        tint & 0xFFFF_FF00
    } else {
        // Regular: half strength, enough to lift the passive dim without hiding the material.
        (tint & 0xFFFF_FF00) | 0x60
    }
}

unsafe fn build(state: &mut State) {
    let local = NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(TOAST_WIDTH, TOAST_HEIGHT),
    );
    let panel: *mut AnyObject = msg_send![class!(NSPanel), alloc];
    let panel: *mut AnyObject = msg_send![
        panel,
        initWithContentRect: local,
        styleMask: 1u64 << 7, // borderless | NSWindowStyleMaskNonactivatingPanel
        backing: 2u64,
        defer: false
    ];
    if panel.is_null() {
        return;
    }
    let _: () = msg_send![panel, setReleasedWhenClosed: false];
    // The vibrant appearance decides what a dynamic ink resolves to, and the material's rim is drawn
    // from it. The keystroke display sets it before any content exists for the same reason.
    let appearance_name = make_nsstring(crate::glass::vibrant_appearance_name(
        theme::resolved_is_dark(),
    ));
    let appearance: *mut AnyObject =
        msg_send![class!(NSAppearance), appearanceNamed: appearance_name];
    release_obj(appearance_name);
    if !appearance.is_null() {
        let _: () = msg_send![panel, setAppearance: appearance];
    }
    // Floating level, so the message sits above ordinary windows like the other panels do.
    let _: () = msg_send![panel, setLevel: 3isize];
    let _: () = msg_send![panel, setIgnoresMouseEvents: true];
    let _: () = msg_send![panel, setHidesOnDeactivate: false];
    let _: () = msg_send![panel, setCollectionBehavior: ((1u64 << 0) | (1u64 << 6) | (1u64 << 8))];
    // The window itself is transparent: the material, its rounded corner and its shadow all come
    // from the shared backdrop, exactly as they do for the switcher and the keystroke display.
    let clear: *mut AnyObject = msg_send![class!(NSColor), clearColor];
    let _: () = msg_send![panel, setBackgroundColor: clear];
    let _: () = msg_send![panel, setOpaque: false];
    // The panel draws its own shadow: `install_backdrop` wraps the material in a shadow carrier with
    // a rounded path. Leaving AppKit's window shadow on as well stacks a second silhouette derived
    // from the opaque region, which on a transparent window extends well past the rounded corner and
    // showed as a speckled halo around the whole panel. The keystroke display disables it the same way.
    let _: () = msg_send![panel, setHasShadow: false];

    let backdrop = crate::glass::install_backdrop(
        panel,
        local,
        crate::glass::PANEL_CORNER_RADIUS,
        toast_backdrop_options(),
    );

    // One row centred in the panel: the tick and its text share the same vertical midline, and the
    // text's field is exactly one line tall because NSTextField draws from the TOP of its frame.
    // Giving it the full panel height (the first attempt) put the glyphs at the top and left the
    // lower half empty.
    // The lit inner bevel the keystroke display wears. `install_backdrop` already adds the
    // material's outline, which is a 0.5pt ring; this is the brighter bevel inside it, and without
    // it the panel's edge reads as a soft blur against a light background.
    let bevel = crate::glass::install_hud_bevel(&backdrop, crate::glass::PANEL_CORNER_RADIUS);

    let content: *mut AnyObject = backdrop.content_parent;
    let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: theme::FONT_CONTROL];
    let line_height = theme::line_height(theme::FONT_CONTROL, theme::LINE_HEIGHT_BODY_RATIO);

    let text_x = TOAST_PADDING + TOAST_ICON_SIZE + TOAST_ICON_GAP;
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let label: *mut AnyObject = msg_send![
        label,
        initWithFrame: NSRect::new(
            NSPoint::new(text_x, (TOAST_HEIGHT - line_height) / 2.0),
            NSSize::new(TOAST_WIDTH - text_x - TOAST_PADDING, line_height)
        )
    ];
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let _: () = msg_send![label, setSelectable: false];
    let _: () = msg_send![label, setUsesSingleLineMode: true];
    let _: () = msg_send![label, setAlignment: 0isize]; // left, beside the icon
    let _: () = msg_send![label, setLineBreakMode: 4isize]; // truncate the tail
    let _: () = msg_send![label, setFont: font];
    let ink: *mut AnyObject = msg_send![class!(NSColor), labelColor];
    let _: () = msg_send![label, setTextColor: ink];
    // The cell's own vertical alignment, the same setting the shared settings label uses: without
    // it a field taller than its text pins the glyphs to the top of the box.
    let cell: *mut AnyObject = msg_send![label, cell];
    if !cell.is_null() && msg_send![cell, respondsToSelector: sel!(setVerticalAlignment:)] {
        let _: () = msg_send![cell, setVerticalAlignment: 1isize]; // NSCenterTextAlignment
    }
    let _: () = msg_send![content, addSubview: label];

    let icon: *mut AnyObject = msg_send![class!(NSImageView), alloc];
    let icon: *mut AnyObject = msg_send![
        icon,
        initWithFrame: NSRect::new(
            NSPoint::new(TOAST_PADDING, (TOAST_HEIGHT - TOAST_ICON_SIZE) / 2.0),
            NSSize::new(TOAST_ICON_SIZE, TOAST_ICON_SIZE)
        )
    ];
    let symbol_name = make_nsstring("checkmark.circle.fill");
    let symbol: *mut AnyObject = msg_send![
        class!(NSImage),
        imageWithSystemSymbolName: symbol_name,
        accessibilityDescription: std::ptr::null_mut::<AnyObject>()
    ];
    release_obj(symbol_name);
    if !symbol.is_null() {
        let _: () = msg_send![icon, setImage: symbol];
        let _: () = msg_send![icon, setImageScaling: 1isize]; // NSImageScaleProportionallyDown
        let content_tint: *mut AnyObject = msg_send![class!(NSColor), secondaryLabelColor];
        let _: () = msg_send![icon, setContentTintColor: content_tint];
    }
    let _: () = msg_send![content, addSubview: icon];

    state.panel = panel;
    state.label = label;
    state.icon = icon;
    state.backdrop = Some(backdrop);
    state.bevel = bevel;
}

/// Where the panel sits on `visible`: centred horizontally, `TOAST_BOTTOM_MARGIN` above the
/// visible bottom edge. Pure so the rule is asserted without AppKit.
fn toast_origin(visible: NSRect) -> NSPoint {
    let centred_x = visible.origin.x + (visible.size.width - TOAST_WIDTH) / 2.0;
    // A screen narrower than the panel would otherwise push it past the left edge.
    let x = centred_x.max(visible.origin.x);
    NSPoint::new(x, visible.origin.y + TOAST_BOTTOM_MARGIN)
}

unsafe fn place(panel: *mut AnyObject, bevel: Option<crate::ffi::ObjPtr>) {
    // `visibleFrame`, not `frame`: the visible area already excludes the menu bar and the Dock.
    let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
    if screen.is_null() {
        return;
    }
    let visible: NSRect = msg_send![screen, visibleFrame];
    let origin = toast_origin(visible);
    // The window is larger than the panel by its shadow padding, so the requested panel rect is
    // converted through the insets rather than assigned to the window frame directly.
    let panel_rect = NSRect::new(origin, NSSize::new(TOAST_WIDTH, TOAST_HEIGHT));
    let insets = crate::glass::panel_insets_of(panel);
    let frame = theme::window_frame_for_panel(panel_rect, insets);
    let _: () = msg_send![panel, setFrame: frame, display: false];
    // setFrame does not synchronously re-lay-out the content. The shadow carrier derives its rounded
    // path from its own laid-out bounds, so without this pass the shadow is drawn for the unpadded
    // geometry the carrier was created with: a halo far outside the panel, which is what the first
    // version showed on screen.
    let _: () = msg_send![panel, layoutIfNeeded];
    // Layout is not drawing: `drawRect:` only runs once the view is marked dirty, and the bevel is
    // built after the material and never touched again. The keystroke display invalidates its bevel
    // on every property refresh for the same reason.
    //
    // The bevel is passed in rather than read back from `TOAST`: `place` is called from `show_in`,
    // which already holds this thread-local mutably, and a second `borrow()` there panics.
    if let Some(bevel) = bevel {
        let _: () = msg_send![bevel.0, setNeedsDisplay: true];
    }
}

unsafe fn cancel_timer(state: &mut State) {
    if state.timer.is_null() {
        return;
    }
    let _: () = msg_send![state.timer, invalidate];
    state.timer = std::ptr::null_mut();
}

unsafe fn schedule_dismiss(state: &mut State, delay: Duration) {
    let timer: *mut AnyObject = msg_send![
        class!(NSTimer),
        scheduledTimerWithTimeInterval: delay.as_secs_f64(),
        target: toast_timer_target(),
        selector: sel!(dismissToast:),
        userInfo: std::ptr::null::<AnyObject>(),
        repeats: false
    ];
    state.timer = timer;
}

static TIMER_CLASS: OnceLock<StaticClass> = OnceLock::new();

/// The one-shot timer's target: it holds no state and only names the selector to call.
unsafe fn toast_timer_target() -> *mut AnyObject {
    static TARGET: OnceLock<crate::ffi::CallbackTarget> = OnceLock::new();
    TARGET
        .get_or_init(|| {
            let name = c"OhMyTabToastTimerTarget";
            let class = StaticClass::register_once(&TIMER_CLASS, name, || {
                crate::ffi::build_class(name, class!(NSObject), |builder| {
                    builder.add_method::<AnyObject, _>(
                        sel!(dismissToast:),
                        dismiss_toast as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                    );
                })
            });
            let instance: *mut AnyObject = msg_send![class.0 as *mut AnyObject, new];
            crate::ffi::CallbackTarget::new(instance)
        })
        .0
}

unsafe extern "C" fn dismiss_toast(_this: *mut AnyObject, _cmd: Sel, _timer: *mut AnyObject) {
    crate::callback_guard::void("toast_dismiss", || {
        TOAST.with(|state| {
            let mut state = state.borrow_mut();
            // The timer that fired is the one this state scheduled; clear the handle first so
            // `hide_in` does not invalidate a timer that has already fired.
            state.timer = std::ptr::null_mut();
            unsafe { hide_in(&mut state) }
        });
    });
}

/// Development switch: present a message once the run loop is up, so `--toast-preview` shows the real
/// component. The preview never expires, which is what makes it comparable side by side with another
/// panel.
///
/// A timer rather than a direct call: `run()` reaches its launch switches before `NSApplication run`,
/// and a panel ordered front with no run loop never draws.
pub(crate) fn schedule_preview(text: String) {
    PREVIEW_TEXT.lock().unwrap().replace(text);
    unsafe {
        let _: *mut AnyObject = msg_send![
            class!(NSTimer),
            scheduledTimerWithTimeInterval: 1.0f64,
            target: preview_target(),
            selector: sel!(fireToastPreview:),
            userInfo: std::ptr::null::<AnyObject>(),
            repeats: false
        ];
    }
}

static PREVIEW_TEXT: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
static PREVIEW_CLASS: OnceLock<StaticClass> = OnceLock::new();

unsafe fn preview_target() -> *mut AnyObject {
    static TARGET: OnceLock<crate::ffi::CallbackTarget> = OnceLock::new();
    TARGET
        .get_or_init(|| {
            let name = c"OhMyTabToastPreviewTarget";
            let class = StaticClass::register_once(&PREVIEW_CLASS, name, || {
                crate::ffi::build_class(name, class!(NSObject), |builder| {
                    builder.add_method::<AnyObject, _>(
                        sel!(fireToastPreview:),
                        fire_preview as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                    );
                })
            });
            let instance: *mut AnyObject = msg_send![class.0 as *mut AnyObject, new];
            crate::ffi::CallbackTarget::new(instance)
        })
        .0
}

unsafe extern "C" fn fire_preview(_this: *mut AnyObject, _cmd: Sel, _timer: *mut AnyObject) {
    crate::callback_guard::void("fire_toast_preview", || {
        let Some(text) = PREVIEW_TEXT.lock().unwrap().take() else {
            return;
        };
        show_working(&text);
    });
}

/// A message that is still working. Kept up with no deadline, because a large file can take
/// longer than any fixed timeout would allow.
pub(crate) fn show_working(text: &str) {
    show(text, None);
}

/// A finished message: shown, then dismissed after the standard duration.
pub(crate) fn show_done(text: &str) {
    show(text, Some(TOAST_VISIBLE));
}

/// Build and tear down the panel, proving the component works without a visible wait.
#[cfg(test)]
pub(crate) fn smoke() -> bool {
    crate::debug_assert_main_thread();
    show_working("smoke");
    let built = is_shown();
    hide();
    built
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The panel needs the AppKit main thread, and libtest runs cases on worker threads, so these
    /// assert the geometry rule the panel is placed by rather than building it.
    #[test]
    fn the_panel_is_placed_inside_the_visible_bottom_edge() {
        let visible = NSRect::new(NSPoint::new(0.0, 25.0), NSSize::new(1440.0, 850.0));
        let origin = toast_origin(visible);
        assert_eq!(origin.y, 25.0 + TOAST_BOTTOM_MARGIN);
        assert_eq!(origin.x, (1440.0 - TOAST_WIDTH) / 2.0);
        assert!(origin.y >= visible.origin.y);
        assert!(origin.y + TOAST_HEIGHT <= visible.origin.y + visible.size.height);
    }

    #[test]
    fn a_screen_narrower_than_the_panel_keeps_it_on_screen() {
        let visible = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(200.0, 400.0));
        let origin = toast_origin(visible);
        assert!(origin.x >= visible.origin.x);
    }
}
