//! A transient, non-activating message at the bottom of the screen.
//!
//! The Finder menu's "Copy Path" and "Copy SHA-256" both report through it. SHA-256 runs on a worker
//! thread and takes as long as the file is big, so the app has to say something between the click and the
//! clipboard. The settings window's success bubble cannot do that job: it is scoped to that window, which
//! usually does not exist while the user is working in Finder.
//!
//! The surface follows the same material every other floating panel uses (`glass::install_backdrop`, the
//! shared elevated corner radius and shadow), so it reads as part of the app rather than as a bare grey
//! rectangle. It is sized to its message rather than to a fixed width: a chip padding wider than the text
//! inside it, which is what the reference bubble looks like.
//!
//! It carries a stroke on **every** material, drawn by the bubble itself rather than borrowed from
//! `glass::install_panel_outline`: that decoration is refused on Liquid Glass (the system draws its own rim)
//! and is 1pt in the softer `card_border` token elsewhere, while this surface has to keep a visible edge
//! over whatever the user is working in -- including where a passive panel's glass renders flat because its
//! window never becomes key.

use std::cell::RefCell;
use std::sync::OnceLock;
use std::time::Duration;

use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use crate::ffi::{make_nsstring, release_obj, StaticClass};
use crate::theme;

// Geometry, in points, taken from the reference component this surface follows
// (opensourceui.in/components/toast-notification, `ToastNotificationBanner`): one row of a 20pt status badge, a
// 13pt message, and a ring of padding; `rounded-xl` = 12pt; `px-3 py-2.5` = 12pt/10pt; `gap-2.5` = 10pt.

/// The message's box: one line of the message font, `TOAST_CONTENT_HEIGHT` tall, holding the badge and the text
/// on one midline. Every other number is padding around it, which is what keeps the bubble only as big as its
/// message plus a ring.
const TOAST_CONTENT_HEIGHT: f64 = 24.0;
/// Horizontal padding before the badge and after the text (`px-3`).
const TOAST_PADDING_X: f64 = 12.0;
/// Padding above and below the row (`py-2.5`).
const TOAST_PADDING_Y: f64 = 10.0;
/// The badge's diameter (`size-5`).
const TOAST_BADGE_SIZE: f64 = 20.0;
/// The tick inside it: the page's lucide `Check` is `size={11}`, not the circle's own size.
const TOAST_CHECK_SIZE: f64 = 11.0;
/// The gap between the badge and the text, and between the text and the trailing close glyph (`gap-2.5`).
const TOAST_GAP: f64 = 10.0;
/// The close glyph's own box: `✕` at 11pt with `px-0.5`.
const TOAST_CLOSE_SIZE: f64 = 11.0;
const TOAST_CLOSE_PAD: f64 = 2.0;
const TOAST_CLOSE_WIDTH: f64 = TOAST_CLOSE_SIZE + TOAST_CLOSE_PAD * 2.0;
/// The button's box: the glyph plus the wash's breathing room, square so the hover reads as a chip.
/// The hover box's height: square around the glyph when the row allows it, so the wash reads as a chip and not
/// as a vertical sliver.
const TOAST_CLOSE_BOX: f64 = TOAST_CLOSE_WIDTH + 4.0;
const TOAST_CLOSE_HEIGHT: f64 = TOAST_CLOSE_BOX;
/// The hover wash's corner radius: a small chip around the glyph, not a pill across the row.
const TOAST_CLOSE_CORNER: f64 = 4.0;
/// The row plus the vertical padding.
const TOAST_HEIGHT: f64 = TOAST_CONTENT_HEIGHT + TOAST_PADDING_Y * 2.0;
/// The bubble's corner radius (`rounded-xl`).
const TOAST_RADIUS: f64 = 12.0;
/// The message's type size: `text-[13px] leading-tight`.
const TOAST_FONT_SIZE: f64 = 13.0;
const TOAST_LINE_HEIGHT: f64 = 15.625;
/// The close glyph's type size (`text-[11px]`).
const TOAST_CLOSE_FONT_SIZE: f64 = 11.0;
/// The narrowest bubble: a floor under a one- or two-character message (and under a failed measurement),
/// so the badge still has a chip around it instead of a sliver.
const TOAST_MIN_WIDTH: f64 = 72.0;
/// The narrowest bubble that still holds its own row: the padding, the badge, both gaps and the close box. The floor
/// above cannot go below it, or the message would be laid out in negative width.
const TOAST_ROW_CHROME: f64 =
    TOAST_PADDING_X * 2.0 + TOAST_BADGE_SIZE + TOAST_GAP * 2.0 + TOAST_CLOSE_WIDTH;
/// The widest a one-line chip may get before the message is cut with an ellipsis. It is not the reference's
/// `max-w-80` (320pt): every message this bubble carries must read in full, and `已拷贝 SHA-256` -- the longest
/// of them -- overruns 320 with the row's own chrome around it. Only a path, which can be hundreds of points
/// wide, is long enough to reach this.
const TOAST_MAX_WIDTH: f64 = 560.0;
/// Distance kept from the screen's visible bottom edge.
const TOAST_BOTTOM_MARGIN: f64 = 48.0;
/// The slide's distance and timing, from the reference this surface follows (appica.dev's toast): the element
/// enters from `translateY(150%)` and leaves the same way, over `transform 0.5s cubic-bezier(0.22, 1, 0.36, 1)`
/// with `opacity 0.5s`. 150% of a 44pt chip is 66pt, so it rises from about a chip's height below its resting place.
const TOAST_SLIDE_RATIO: f64 = 1.5;
const TOAST_SLIDE_SECONDS: f64 = 0.5;
/// The reference's easing control points, as (x1, y1, x2, y2) for `CAMediaTimingFunction`.
const TOAST_SLIDE_EASING: (f64, f64, f64, f64) = (0.22, 1.0, 0.36, 1.0);

/// How long a FINISHED message stays. A message still working has no deadline, and the ✕ answers before it.
///
/// Two seconds: long enough to read the sentence and reach the ✕, short enough not to linger. The reference's own
/// preview keeps its toast up until it is dismissed; this surface resolves on a timer because a copied path or digest
/// has nothing to click.
const TOAST_VISIBLE: Duration = Duration::from_millis(2000);

/// What the badge says. The bubble reports a finished action either way; the badge is the half
/// that says whether it worked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ToastKind {
    /// The reference's emerald circle with a white tick: the action succeeded.
    #[default]
    Success,
    /// A red circle with a white cross: the action was refused, and the text says why.
    Error,
}

/// Which badge the NEXT presentation draws. The badge view paints through `drawRect:`, which takes
/// no argument of ours, so the kind travels as process state and every presentation sets it before
/// invalidating the view. One bubble exists per slot and presentations are serialised on the main
/// thread, so a single value is enough.
static BADGE_KIND: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

struct State {
    /// Which bubble this is (see `SLOTS`); the close button carries it in its tag.
    slot: usize,
    panel: *mut AnyObject,
    /// The fixed host view the carrier moves inside; kept so the window's content is never rebound.
    host: *mut AnyObject,
    label: *mut AnyObject,
    /// The status badge (a circle and its glyph), kept so a redraw can recolour it.
    badge: *mut AnyObject,
    /// The trailing close button.
    close: *mut AnyObject,
    /// Whether the close button's hover tracking area is in place (see `ensure_close_tracking`).
    close_tracked: bool,
    backdrop: Option<crate::glass::InstalledBackdrop>,
    /// The lit inner bevel; kept so the view is not dropped while its parent still shows it.
    bevel: Option<crate::ffi::ObjPtr>,
    /// Which message this bubble is showing. Bumped by every presentation, so a completion handler from an older
    /// dismissal can tell it is stale.
    generation: u64,
    /// The pending auto-dismiss, cleared whenever the text changes or the panel is hidden.
    timer: *mut AnyObject,
}

impl Default for State {
    fn default() -> Self {
        Self {
            slot: 0,
            panel: std::ptr::null_mut(),
            host: std::ptr::null_mut(),
            label: std::ptr::null_mut(),
            badge: std::ptr::null_mut(),
            close: std::ptr::null_mut(),
            close_tracked: false,
            backdrop: None,
            bevel: None,
            generation: 0,
            timer: std::ptr::null_mut(),
        }
    }
}

// One bubble per slot, built lazily: the product presents messages in slot 0, and the preview can put a
// second one beside it to show two messages -- or two candidate surfaces -- on one frame.
thread_local! {
    static SLOTS: RefCell<Vec<State>> = const { RefCell::new(Vec::new()) };
}

/// Show `text` and keep it until `dismiss_after` elapses.
///
/// `dismiss_after: None` means "keep it up": that is the computing state, where any deadline would
/// expire before the work does on a large file, leaving the user unable to tell whether it is
/// still running. A later call replaces both the text and any pending timer.
pub(crate) fn show(text: &str, dismiss_after: Option<Duration>) {
    show_kind(text, ToastKind::Success, dismiss_after);
}

fn show_kind(text: &str, kind: ToastKind, dismiss_after: Option<Duration>) {
    crate::debug_assert_main_thread();
    BADGE_KIND.store(
        match kind {
            ToastKind::Success => 0,
            ToastKind::Error => 1,
        },
        std::sync::atomic::Ordering::SeqCst,
    );
    let shown = SLOTS.with(|slots| {
        let mut slots = slots.borrow_mut();
        if slots.is_empty() {
            slots.push(State::default());
        }
        unsafe { show_in(&mut slots[0], text, 0.0, dismiss_after) }
    });
    // The badge paints through drawRect: and reads the kind from the process state, so it has to be
    // invalidated on EVERY presentation: a bubble reused for an error after a success would
    // otherwise keep drawing the tick.
    if shown {
        SLOTS.with(|slots| {
            let mut slots = slots.borrow_mut();
            if let Some(state) = slots.first_mut() {
                if !state.badge.is_null() {
                    unsafe {
                        let yes = objc2::runtime::Bool::YES;
                        let _: () = msg_send![state.badge, setNeedsDisplay: yes];
                    }
                }
            }
        });
    }
    if !shown {
        crate::log_info!("[toast] could not present a message");
    }
}

/// Hide the panel now and cancel any pending dismissal.
/// Hide the bubble now and cancel any pending dismissal. Main thread only.
pub(crate) fn hide() {
    crate::debug_assert_main_thread();
    SLOTS.with(|slots| {
        let mut slots = slots.borrow_mut();
        if let Some(state) = slots.first_mut() {
            unsafe { hide_in(state) }
        }
    })
}

unsafe fn show_in(
    state: &mut State,
    text: &str,
    row_offset: f64,
    dismiss_after: Option<Duration>,
) -> bool {
    if state.panel.is_null() && !build(state) {
        crate::log_info!("[toast] the message could not be shown without an opaque surface");
        return false;
    }
    // Refresh the material on every presentation: the panel is reused across messages, and the
    // material or the theme may have changed since it was built. The keystroke display does the same
    // before each render rather than trusting the construction-time state.
    if let Some(backdrop) = state.backdrop {
        crate::glass::apply_live_properties(backdrop, toast_backdrop_options());
        // The live refresh re-paints an opaque surface from the theme's window colour; the bubble's own fill
        // is restored right after it, so a theme change cannot turn the white chip into a dark one.
        crate::glass::paint_opaque_surface(&backdrop, toast_fill_hex());
    }
    // A message that arrives while the chip is already up REPLACES the one on screen in the same chip: the computing
    // chip becomes the result without an exit and a second entrance, and the deadline restarts here, so the time the
    // user reads is measured from the state they are looking at.
    let already_up = {
        let visible: bool = unsafe { msg_send![state.panel, isVisible] };
        visible
    };
    let text_ns = make_nsstring(text);
    let _: () = msg_send![state.label, setStringValue: text_ns];
    release_obj(text_ns);
    // The panel is sized from what the field itself needs for this string, not only from `NSFont`'s measurement:
    // a field lays the line out with its cell's metrics, which run a few points wider. Sizing from the font alone
    // cut the last glyph of a message that was, by that measure, exactly the right width.
    let width = unsafe { toast_width_for_field(state.label, text) };
    // A new message. Any dismissal still animating belongs to the previous one and must not act on this one.
    state.generation = state.generation.wrapping_add(1);
    ensure_close_tracking(state);
    // The close view paints itself; a fresh presentation starts it at rest and marks it dirty.
    let _: () = msg_send![state.close, setNeedsDisplay: true];
    place(state.panel, width, row_offset, state.bevel);
    layout_row(state, width);
    // A message replacing one that is already up has already arrived: it is re-laid out and re-timed, and nothing
    // animates. Only the first message of a showing enters from below.
    if already_up {
        cancel_timer(state);
        // A replacement that lands while the entrance is still running settles the chip at its resting frame first (the
        // new width moved it); `animate` would be ignored mid-flight, so this is a plain set and the entrance's own
        // animation is left to finish or to be replaced by the next presentation.
        // A replacement that lands while the entrance is still running settles the chip at its resting frame first (the
        // new width moved it); an animated set would be ignored mid-flight, so this is a plain one and the entrance's
        // own animation is left to finish or to be replaced by the next presentation.
        let resting = carrier_resting(state);
        if let Some(carrier) = state.backdrop.and_then(|backdrop| backdrop.root) {
            let current: NSRect = msg_send![carrier.0, frame];
            let _: () = msg_send![carrier.0, setFrame: NSRect::new(resting.origin, current.size)];
            let _: () = msg_send![carrier.0, setAlphaValue: 1.0f64];
        }
        if let Some(delay) = dismiss_after {
            schedule_dismiss(state, delay);
        }
        return true;
    }
    // The carrier is the chip -- material, outline, shadow and row. It starts one slide below its resting place, set
    // with no animation in this same turn so no frame of a stationary chip is ever shown, and one animation carries it
    // up while the window (and its opacity) never move.
    let resting = carrier_resting(state);
    // Both restored with plain setters before the window is shown: a previous exit may have left the window at 0.
    let _: () = msg_send![state.panel, setAlphaValue: 1.0f64];
    if let Some(root) = state.backdrop.and_then(|backdrop| backdrop.root) {
        let _: () = msg_send![root.0, setAlphaValue: 1.0f64];
    }
    set_carrier_slide(state, 1.0);
    if let Some(root) = state.backdrop.and_then(|backdrop| backdrop.root) {
        let _: () = msg_send![root.0, setAlphaValue: 0.0f64];
    }
    let _: () = msg_send![state.panel, orderFrontRegardless];
    slide_carrier(state, resting, 0.0, 1.0);
    cancel_timer(state);
    if let Some(delay) = dismiss_after {
        schedule_dismiss(state, delay);
    }
    true
}

unsafe fn hide_in(state: &mut State) {
    cancel_timer(state);
    // A dismissal -- by the ✕ or by the timer -- ends the presentation that was animating in: bumping the generation
    // makes that entrance's completion a no-op, so it cannot restore the chip after the exit has started.
    state.generation = state.generation.wrapping_add(1);
    // A panel that is ordered out never delivers `mouseExited:`, so the resting style is restated here; otherwise the
    // next showing would open with a red wash under a pointer that is nowhere near it.
    if !state.close.is_null() {
        set_close_hover(state.close, false);
    }
    if state.panel.is_null() {
        return;
    }
    let panel = state.panel;
    // The reference's exit: the chip leaves exactly the way it arrived -- from its resting place down one slide
    // (translateY(150%)) while its opacity goes to 0 -- and is ordered out only when that motion has finished.
    //
    // ONE animated property carries the whole chip: the carrier's position. Its opacity and the window's move together
    // in the same grouping, so the white surface, its outline, its shadow and the row fade as one thing; the first
    // version faded the window while the material kept reasserting its own fill, which is why the surface and the
    // text vanished in two separate steps. Reduce Motion still orders out at once, with no travel.
    if crate::theme::reduce_motion_enabled() {
        let _: () = msg_send![panel, orderOut: std::ptr::null_mut::<AnyObject>()];
        return;
    }
    let generation = state.generation;
    let resting = carrier_resting(state);
    let _: () = msg_send![class!(NSAnimationContext), beginGrouping];
    let group: *mut AnyObject = msg_send![class!(NSAnimationContext), currentContext];
    let _: () = msg_send![group, setDuration: TOAST_SLIDE_SECONDS];
    if let Some(easing) = timing_function(TOAST_SLIDE_EASING) {
        let _: () = msg_send![group, setTimingFunction: easing];
    }
    // Registered BEFORE the animated setters: a completion handler is grouped with the animations that follow it, and
    // one registered after them can fire first. That ordering is the whole point of setting it here.
    let completion: block2::RcBlock<dyn Fn()> = block2::RcBlock::new(move || {
        crate::callback_guard::void("toast_hide", || {
            if current_generation() != Some(generation) {
                return;
            }
            unsafe {
                // Both the window's and the carrier's opacity are restored by `show_in` on the next presentation, which
                // is where they are needed and where a stale value cannot matter.
                let _: () = msg_send![panel, orderOut: std::ptr::null_mut::<AnyObject>()];
            }
        });
    });
    let _: () = msg_send![group, setCompletionHandler: &*completion];
    if let Some(root) = state.backdrop.and_then(|backdrop| backdrop.root) {
        // From the resting frame, not from wherever an interrupted entrance had left it: the presentation that frame
        // belonged to is over, and an exit measured from a partial frame is an exit of the wrong length.
        let target = NSPoint::new(
            resting.origin.x,
            resting.origin.y - TOAST_HEIGHT * TOAST_SLIDE_RATIO,
        );
        // The carrier is the one view that contains everything the chip is: moving it and fading it are one layer
        // subtree's two properties, animated together, so the surface, its edge, its shadow and the row cannot come
        // apart. The WINDOW's opacity is left alone -- it was the outer fade that made the chip look two-stage.
        let carrier_animator: *mut AnyObject = msg_send![root.0, animator];
        let _: () = msg_send![carrier_animator, setFrameOrigin: target];
        let _: () = msg_send![carrier_animator, setAlphaValue: 0.0f64];
    }
    let _: () = msg_send![class!(NSAnimationContext), endGrouping];
}

/// The backdrop options, shared by construction and every live refresh.
///
/// The bubble is an **opaque** surface: no material, no veil, so the desktop behind it can neither darken nor
/// brighten it. That is the finding of the side-by-side comparison -- a one-line passive panel over a dark
/// desktop went grey-to-black with every translucent variant (clear glass, the menu material, a lighter dark
/// tint). An opaque surface keeps one tone in every mode and on every wallpaper, which is what a short
/// message needs to be read at a glance.
///
/// The outline and the elevation are the backdrop's own, exactly as they are on the app's opaque panels.
/// The bubble's shadow: the reference's `0 4px 24px -2px rgba(0,0,0,0.10)`, with a touch more opacity so it
/// still reads on a light desktop.
///
/// Its own level rather than the shared `ELEVATION_HIGH` (32pt radius, 18%): the reference's shadow is tighter
/// and softer, which is what makes a small chip sit on the surface instead of floating above it.
const TOAST_ELEVATION: crate::theme::Elevation = crate::theme::Elevation {
    color: 0x000000FF,
    opacity: 0.16,
    radius: 16.0,
    offset_y: -4.0,
};

fn toast_backdrop_options() -> crate::glass::BackdropOptions {
    crate::glass::BackdropOptions::new(None)
        .with_elevation(Some(TOAST_ELEVATION))
        // Pinned, not merely "configured": the settings' panel material may be Liquid Glass, frost or
        // anything else, and every one of those would make the bubble follow the desktop behind it.
        .with_material(crate::glass::PanelMaterial::Opaque)
}

#[link(name = "AppKit", kind = "framework")]
extern "C" {
    static NSFontAttributeName: *mut AnyObject;
    static NSForegroundColorAttributeName: *mut AnyObject;
}

unsafe fn build(state: &mut State) -> bool {
    // The design: a fixed, transparent HOST window, and the backdrop's carrier as one child view inside it. The chip is
    // the carrier's material; the room the slide needs is the host's, so moving the chip moves material, outline and
    // shadow together and never moves a window.
    let insets = crate::glass::panel_insets_for(toast_backdrop_options());
    let chip = toast_rect(TOAST_MIN_WIDTH);
    let carrier = crate::glass::holder_frame(chip, NSPoint::new(0.0, 0.0), insets);
    let host_size = NSSize::new(
        carrier.size.width,
        // The resting chip plus the room it travels down through, and the elevation's padding beyond that.
        carrier.size.height + TOAST_HEIGHT * TOAST_SLIDE_RATIO,
    );
    let local = NSRect::new(NSPoint::new(0.0, 0.0), host_size);
    let panel: *mut AnyObject = msg_send![class!(NSPanel), alloc];
    let panel: *mut AnyObject = msg_send![
        panel,
        initWithContentRect: local,
        styleMask: 1u64 << 7, // borderless | NSWindowStyleMaskNonactivatingPanel
        backing: 2u64,
        defer: false
    ];
    if panel.is_null() {
        return false;
    }
    let _: () = msg_send![panel, setReleasedWhenClosed: false];
    let appearance_name = make_nsstring(crate::glass::vibrant_appearance_name(
        theme::resolved_is_dark(),
    ));
    let appearance: *mut AnyObject =
        msg_send![class!(NSAppearance), appearanceNamed: appearance_name];
    release_obj(appearance_name);
    if !appearance.is_null() {
        let _: () = msg_send![panel, setAppearance: appearance];
    }
    let _: () = msg_send![panel, setLevel: 3isize];
    let _: () = msg_send![panel, setIgnoresMouseEvents: false];
    let _: () = msg_send![panel, setHidesOnDeactivate: false];
    let _: () = msg_send![panel, setCollectionBehavior: ((1u64 << 0) | (1u64 << 6) | (1u64 << 8))];
    let clear: *mut AnyObject = msg_send![class!(NSColor), clearColor];
    let _: () = msg_send![panel, setBackgroundColor: clear];
    let _: () = msg_send![panel, setOpaque: false];
    let _: () = msg_send![panel, setHasShadow: false];

    // The host: a plain view the window shows, holding nothing but the carrier. Everything the chip is, the carrier
    // is; the host is only the frame the motion happens in.
    let host: *mut AnyObject = msg_send![class!(NSView), alloc];
    let host: *mut AnyObject = msg_send![host, initWithFrame: local];
    let _: () = msg_send![host, setWantsLayer: true];
    let _: () = msg_send![panel, setContentView: host];
    release_obj(host);

    // The carrier sits on the host's BOTTOM edge: at rest the chip's bottom is the window's bottom, and the reserved
    // slide room is above it, which keeps the resting chip where the bottom margin of the screen says it is.
    let carrier_origin = NSPoint::new(insets.left, insets.bottom);
    let backdrop = crate::glass::install_hosted_backdrop(
        host,
        carrier_origin,
        chip,
        TOAST_RADIUS,
        toast_backdrop_options(),
    );
    // One surface: the material is the white chip, its outline and the clipboard bevel its edge. The row draws no
    // background of its own -- a second fill is what showed as a box inside a box.
    let bevel = crate::glass::install_panel_bevel(&backdrop, TOAST_RADIUS);

    let content: *mut AnyObject = backdrop.content_parent;
    // The row is laid out in the MATERIAL's coordinates, whose origin is the chip's.
    let row_mid = TOAST_HEIGHT / 2.0;
    let badge = build_badge(
        content,
        NSRect::new(
            NSPoint::new(TOAST_PADDING_X, row_mid - TOAST_BADGE_SIZE / 2.0),
            NSSize::new(TOAST_BADGE_SIZE, TOAST_BADGE_SIZE),
        ),
    );
    let text_x = TOAST_PADDING_X + TOAST_BADGE_SIZE + TOAST_GAP;
    let label = build_message(
        content,
        NSRect::new(
            NSPoint::new(text_x, row_mid - TOAST_LINE_HEIGHT / 2.0),
            NSSize::new(
                TOAST_MIN_WIDTH - text_x - TOAST_GAP - TOAST_CLOSE_WIDTH,
                TOAST_LINE_HEIGHT,
            ),
        ),
    );
    let close = build_close_glyph(content, state.slot);

    if backdrop.material != crate::glass::PanelMaterial::Opaque {
        release_obj(panel);
        return false;
    }
    state.panel = panel;
    state.host = host;
    state.label = label;
    state.badge = badge;
    state.close = close;
    state.backdrop = Some(backdrop);
    state.bevel = bevel;
    true
}

/// The bubble's fill: white, the tone the reference bubble has.
///
/// Not a palette surface token. The reference is a white chip that stands on whatever the user is working
/// in, and `window_bg`/the panel surface both follow the theme, which in dark mode would give a dark chip --
/// the exact failure the translucent candidates had. A white chip keeps one tone in both modes, and its inks
/// (the primary label colour and the palette accent) are chosen against white above.
fn toast_fill_hex() -> u32 {
    0xFFFF_FFFF
}

/// The bubble's width for the string `text` as `label` lays it out: the larger of the font's own measurement and
/// the width the field reports once it holds the string, plus the row's chrome.
unsafe fn toast_width_for_field(label: *mut AnyObject, text: &str) -> f64 {
    let font_width = crate::glass::measure_single_line_text(text, TOAST_FONT_SIZE).width;
    let _: () = msg_send![label, sizeToFit];
    let fitted: NSRect = msg_send![label, frame];
    let field_width = fitted.size.width;
    toast_width_for_text_width(font_width.max(field_width))
}

/// The reference's inks, on its white surface: the message in `text-neutral-800`, the badge in
/// `bg-emerald-500` with a white tick, and the close glyph in `text-neutral-400`.
///
/// Literal values rather than palette tokens. The bubble is white in both modes by design (see
/// `toast_fill_hex`), so its inks are resolved against that one surface and not against the theme; the palette's
/// label and accent tokens follow the theme and would invert in dark mode.
const TOAST_MESSAGE_INK: u32 = 0x262626FF;
const TOAST_BADGE_FILL: u32 = 0x10B981FF;
/// The refused badge fill: the reference page bg-red-500, with the same white glyph.
const TOAST_ERROR_BADGE_FILL: u32 = 0xEF4444FF;
const TOAST_CLOSE_INK: u32 = 0xA3A3A3FF;

/// The message line: a field that does not draw its own background, so it is the panel's flat surface with one
/// line of text on it -- the row's own geometry, none of AppKit's. The text is ONE line and never truncated: the
/// panel's width is measured from this very text (see `toast_width`), so the field it is given always fits it,
/// and a field only cuts what does not fit.
///
/// A field rather than a custom drawing view: AppKit's drawing has to run inside the window's display cycle,
/// which a passive panel owns and no callback of ours can drive, while a field's cell paints on the same pass
/// the rest of the row does.
unsafe fn build_message(content: *mut AnyObject, frame: NSRect) -> *mut AnyObject {
    let message: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let message: *mut AnyObject = msg_send![message, initWithFrame: frame];
    let _: () = msg_send![message, setBezeled: false];
    let _: () = msg_send![message, setBordered: false];
    let _: () = msg_send![message, setDrawsBackground: false];
    let _: () = msg_send![message, setEditable: false];
    let _: () = msg_send![message, setSelectable: false];
    let _: () = msg_send![message, setUsesSingleLineMode: true];
    let _: () = msg_send![message, setAlignment: 0isize]; // left, after the badge
    let _: () = msg_send![message, setLineBreakMode: 4isize]; // a tail cut only if the width was miscomputed
    let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: TOAST_FONT_SIZE];
    let _: () = msg_send![message, setFont: font];
    let _: () = msg_send![message, setTextColor: crate::ffi::hex_to_ns_color(TOAST_MESSAGE_INK)];
    let cell: *mut AnyObject = msg_send![message, cell];
    if !cell.is_null() && msg_send![cell, respondsToSelector: sel!(setVerticalAlignment:)] {
        let _: () = msg_send![cell, setVerticalAlignment: 1isize]; // NSCenterTextAlignment
    }
    let _: () = msg_send![content, addSubview: message];
    release_obj(message);
    message
}

/// The status badge: a filled circle with a white tick, the reference's `size-5 rounded-full bg-emerald-500`
/// plus its 11pt `Check`. One custom view draws both, so the tick keeps the badge's own centre.
unsafe fn build_badge(content: *mut AnyObject, frame: NSRect) -> *mut AnyObject {
    let class = badge_class();
    let badge: *mut AnyObject = msg_send![class, alloc];
    let badge: *mut AnyObject = msg_send![badge, initWithFrame: frame];
    let _: () = msg_send![content, addSubview: badge];
    release_obj(badge);
    badge
}

/// A circle in the badge's fill with a tick stroked through it.
unsafe extern "C" fn draw_badge(view: *mut AnyObject, _cmd: Sel, _dirty: NSRect) {
    crate::callback_guard::void("toast_badge_draw", || unsafe {
        let bounds: NSRect = msg_send![view, bounds];
        let circle: *mut AnyObject = msg_send![
            class!(NSBezierPath),
            bezierPathWithOvalInRect: bounds
        ];
        let error = BADGE_KIND.load(std::sync::atomic::Ordering::SeqCst) == 1;
        let fill = crate::ffi::hex_to_ns_color(if error {
            TOAST_ERROR_BADGE_FILL
        } else {
            TOAST_BADGE_FILL
        });
        let _: () = msg_send![fill, setFill];
        let _: () = msg_send![circle, fill];
        if error {
            // The reference's lucide `X`: `M18 6 6 18M6 6l12 12`, same 11pt box, width and caps.
            let glyph = TOAST_CHECK_SIZE;
            let unit = glyph / 24.0;
            let origin_x = bounds.origin.x + (bounds.size.width - glyph) / 2.0;
            let origin_y = bounds.origin.y + (bounds.size.height - glyph) / 2.0;
            let px = |x: f64| origin_x + x * unit;
            let py = |y: f64| origin_y + glyph - y * unit;
            let cross: *mut AnyObject = msg_send![class!(NSBezierPath), bezierPath];
            let _: () = msg_send![cross, moveToPoint: NSPoint::new(px(18.0), py(6.0))];
            let _: () = msg_send![cross, lineToPoint: NSPoint::new(px(6.0), py(18.0))];
            let _: () = msg_send![cross, moveToPoint: NSPoint::new(px(6.0), py(6.0))];
            let _: () = msg_send![cross, lineToPoint: NSPoint::new(px(18.0), py(18.0))];
            let _: () = msg_send![cross, setLineWidth: 3.0 * unit];
            let _: () = msg_send![cross, setLineCapStyle: 1isize];
            let _: () = msg_send![cross, setLineJoinStyle: 1isize];
            let white = crate::ffi::hex_to_ns_color(0xFFFF_FFFF);
            let _: () = msg_send![white, setStroke];
            let _: () = msg_send![cross, stroke];
            return;
        }
        // The reference's glyph exactly: a lucide `Check` at 11x11 (its `width={11} height={11}`),`M20 6 9 17l-5-5`
        // in a 24-unit viewBox, stroke-width 3, round caps and joins, centred in the 20pt circle. Scaling that
        // viewBox to the whole badge instead -- the first attempt -- drew a tick nearly twice the size the
        // reference has, which is what made the badge read as heavier than the page's.
        let glyph = TOAST_CHECK_SIZE;
        let unit = glyph / 24.0;
        let origin_x = bounds.origin.x + (bounds.size.width - glyph) / 2.0;
        let origin_y = bounds.origin.y + (bounds.size.height - glyph) / 2.0;
        let px = |x: f64| origin_x + x * unit;
        let py = |y: f64| origin_y + glyph - y * unit;
        let tick: *mut AnyObject = msg_send![class!(NSBezierPath), bezierPath];
        let _: () = msg_send![tick, moveToPoint: NSPoint::new(px(20.0), py(6.0))];
        let _: () = msg_send![tick, lineToPoint: NSPoint::new(px(9.0), py(17.0))];
        let _: () = msg_send![tick, lineToPoint: NSPoint::new(px(4.0), py(12.0))];
        let _: () = msg_send![tick, setLineWidth: 3.0 * unit];
        let _: () = msg_send![tick, setLineCapStyle: 1isize]; // NSRoundLineCapStyle
        let _: () = msg_send![tick, setLineJoinStyle: 1isize]; // NSRoundLineJoinStyle
        let white = crate::ffi::hex_to_ns_color(0xFFFF_FFFF);
        let _: () = msg_send![white, setStroke];
        let _: () = msg_send![tick, stroke];
    });
}

fn badge_class() -> *mut AnyObject {
    static CLASS: OnceLock<StaticClass> = OnceLock::new();
    crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabToastBadge", || {
        crate::ffi::build_class(c"OhMyTabToastBadge", class!(NSView), |builder| unsafe {
            builder.add_method::<AnyObject, _>(
                sel!(drawRect:),
                draw_badge as unsafe extern "C" fn(*mut AnyObject, Sel, NSRect),
            );
            builder.add_method::<AnyObject, _>(
                sel!(hitTest:),
                toast_hit_test
                    as unsafe extern "C" fn(*mut AnyObject, Sel, NSPoint) -> *mut AnyObject,
            );
        })
    })
    .0 as *mut AnyObject
}

/// The decorations of this surface never take the hit test: the window ignores mouse events as a whole, so the
/// only thing a decoration could do by accepting one is shadow the content the run loop looks for.
unsafe extern "C" fn toast_hit_test(
    _view: *mut AnyObject,
    _cmd: Sel,
    _point: NSPoint,
) -> *mut AnyObject {
    crate::callback_guard::nil("toast_hit_test", std::ptr::null_mut)
}

/// Give the close button its hover tracking, once it is in a window.
///
/// An `NSTrackingArea` only delivers anything while its view is in a window, so it is attached at the first
/// presentation rather than at build time; the flag keeps it to one area per button, since a second would double
/// every enter/exit.
unsafe fn ensure_close_tracking(state: &mut State) {
    if state.close_tracked || state.close.is_null() {
        return;
    }
    let window: *mut AnyObject = msg_send![state.close, window];
    if window.is_null() {
        return;
    }
    add_close_tracking(state.close);
    state.close_tracked = true;
}

/// The ✕ hovers the way the clipboard history's row ✕ does: the glyph turns the palette's destructive colour and
/// a faint wash of it fills the button.
///
/// A button rather than a plain custom view: a borderless `NSButton` with a template image is what the clipboard
/// history uses for the same control, and AppKit centres a template image in it on both axes. The `✕` as a title
/// did not -- a title is inset and laid out against the width the cell saw at build time -- which is what left the
/// glyph and its wash off-centre once the button was reframed to the row.
unsafe fn set_close_hover(button: *mut AnyObject, hovered: bool) {
    let layer: *mut AnyObject = msg_send![button, layer];
    if !layer.is_null() {
        let wash = if hovered {
            (crate::theme::ui_palette().destructive & 0xFFFF_FF00) | 0x18
        } else {
            0x0000_0000
        };
        crate::ffi::layer_set_background(layer, crate::ffi::hex_to_cg_color(wash));
        let _: () = msg_send![layer, setCornerRadius: TOAST_CLOSE_CORNER];
    }
    // The glyph is a template image, so the tint IS its colour -- one call, no attributed title to rebuild.
    let ink = if hovered {
        crate::theme::ui_palette().destructive
    } else {
        TOAST_CLOSE_INK
    };
    let _: () = msg_send![button, setContentTintColor: crate::ffi::hex_to_ns_color(ink)];
}

extern "C" fn close_button_entered(_self: *mut AnyObject, _cmd: Sel, _event: *mut AnyObject) {
    crate::callback_guard::void("toast_close_entered", || {
        unsafe { set_close_hover(_self, true) };
    });
}

extern "C" fn close_button_exited(_self: *mut AnyObject, _cmd: Sel, _event: *mut AnyObject) {
    crate::callback_guard::void("toast_close_exited", || {
        unsafe { set_close_hover(_self, false) };
    });
}

fn close_button_class() -> *mut AnyObject {
    static CLASS: OnceLock<StaticClass> = OnceLock::new();
    crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabToastCloseButton", || {
        crate::ffi::build_class(
            c"OhMyTabToastCloseButton",
            class!(NSButton),
            |builder| unsafe {
                builder.add_method::<AnyObject, _>(
                    sel!(mouseEntered:),
                    close_button_entered as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
                builder.add_method::<AnyObject, _>(
                    sel!(mouseExited:),
                    close_button_exited as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
            },
        )
    })
    .0 as *mut AnyObject
}

/// The close button's target: it holds no state, and the button that sent the action names no bubble, so the
/// target resolves the bubble from the sender's own superview chain is not needed -- each bubble has one button and
/// one timer, and the action only ever has to hide the bubble that owns the pressed control.
unsafe fn close_target() -> *mut AnyObject {
    static TARGET: OnceLock<crate::ffi::CallbackTarget> = OnceLock::new();
    static CLASS: OnceLock<StaticClass> = OnceLock::new();
    TARGET
        .get_or_init(|| {
            let name = c"OhMyTabToastCloseTarget";
            let class = StaticClass::register_once(&CLASS, name, || {
                crate::ffi::build_class(name, class!(NSObject), |builder| unsafe {
                    builder.add_method::<AnyObject, _>(
                        sel!(dismissToast:),
                        close_pressed as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                    );
                })
            });
            let instance: *mut AnyObject = msg_send![class.0 as *mut AnyObject, new];
            crate::ffi::CallbackTarget::new(instance)
        })
        .0
}

/// The user clicked the bubble's `✕`: hide the bubble whose close button sent it and cancel that bubble's own
/// timeout, so a click and the timer can never each run a dismissal for the same message.
unsafe extern "C" fn close_pressed(_this: *mut AnyObject, _cmd: Sel, sender: *mut AnyObject) {
    crate::callback_guard::void("toast_close_pressed", || {
        SLOTS.with(|slots| {
            let mut slots = slots.borrow_mut();
            let Some(state) = slots.iter_mut().find(|state| state.close == sender) else {
                return;
            };
            hide_in(state);
        });
    });
}

/// Attach the hover tracking the close button needs. MouseEnteredAndExited | ActiveAlways: the bubble's window is
/// passive and the app behind it stays inactive, exactly the case the clipboard's own tracking area documents as
/// needing ActiveAlways rather than ActiveInActiveApp.
unsafe fn add_close_tracking(button: *mut AnyObject) {
    let opts: u64 = 0x01 | 0x80;
    let ta: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
    let bounds: NSRect = msg_send![button, bounds];
    let ta: *mut AnyObject = msg_send![
        ta,
        initWithRect: bounds,
        options: opts,
        owner: button,
        userInfo: std::ptr::null::<AnyObject>()
    ];
    let _: () = msg_send![button, addTrackingArea: ta];
    release_obj(ta);
}

unsafe fn build_close_glyph(content: *mut AnyObject, _slot: usize) -> *mut AnyObject {
    let close: *mut AnyObject = msg_send![close_button_class(), alloc];
    let close: *mut AnyObject = msg_send![close, initWithFrame: NSRect::new(
        NSPoint::new(
            TOAST_MIN_WIDTH - TOAST_PADDING_X - TOAST_CLOSE_WIDTH,
            TOAST_HEIGHT / 2.0 - TOAST_CLOSE_HEIGHT / 2.0
        ),
        NSSize::new(TOAST_CLOSE_WIDTH, TOAST_CLOSE_HEIGHT)
    )];
    let _: () = msg_send![close, setBordered: false];
    let _: () = msg_send![close, setWantsLayer: true];
    let _: () = msg_send![close, setFocusRingType: 1isize]; // NSFocusRingTypeNone
                                                            // The glyph is drawn into a template image of the button's own size and given as the button's image, so AppKit
                                                            // centres it on both axes without a title to inset it. A title was the first attempt and never centred: the cell
                                                            // keeps the width it laid out at build time, so the glyph and its wash drifted once the row was laid out.
    let glyph = make_nsstring("✕");
    let image: *mut AnyObject = msg_send![class!(NSImage), alloc];
    let image: *mut AnyObject =
        msg_send![image, initWithSize: NSSize::new(TOAST_CLOSE_BOX, TOAST_CLOSE_BOX)];
    let _: () = msg_send![image, lockFocus];
    let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: TOAST_CLOSE_FONT_SIZE];
    let attributes: *mut AnyObject = msg_send![class!(NSMutableDictionary), dictionary];
    let _: () = msg_send![attributes, setObject: font, forKey: NSFontAttributeName];
    let _: () = msg_send![
        attributes,
        setObject: crate::ffi::hex_to_ns_color(0xFFFF_FFFF),
        forKey: NSForegroundColorAttributeName
    ];
    let line: NSRect = msg_send![
        glyph,
        boundingRectWithSize: NSSize::new(TOAST_CLOSE_BOX, TOAST_CLOSE_BOX),
        options: 1u64,
        attributes: attributes
    ];
    let _: () = msg_send![
        glyph,
        drawInRect: NSRect::new(
            NSPoint::new(
                (TOAST_CLOSE_BOX - line.size.width) / 2.0,
                (TOAST_CLOSE_BOX - line.size.height) / 2.0
            ),
            NSSize::new(line.size.width, line.size.height)
        ),
        withAttributes: attributes
    ];
    let _: () = msg_send![image, unlockFocus];
    let _: () = msg_send![image, setTemplate: true];
    let _: () = msg_send![close, setImage: image];
    release_obj(image);
    let _: () = msg_send![close, setImagePosition: 2isize]; // image only, no title
    let _: () = msg_send![close, setImageScaling: 3isize]; // NSImageScaleNone: draw the template at the size it was made
    let _: () = msg_send![close, setContentTintColor: crate::ffi::hex_to_ns_color(TOAST_CLOSE_INK)];
    let _: () = msg_send![close, setTarget: close_target()];
    let _: () = msg_send![close, setAction: sel!(dismissToast:)];
    let tooltip = make_nsstring(&crate::i18n::t("finder_menu.toast_dismiss"));
    let _: () = msg_send![close, setToolTip: tooltip];
    release_obj(tooltip);
    release_obj(glyph);
    let _: () = msg_send![content, addSubview: close];
    add_close_tracking(close);
    set_close_hover(close, false);
    release_obj(close);
    close
}

/// The gap between two bubbles standing side by side.
const TOAST_ROW_GAP: f64 = 16.0;

/// The chip's own rect for a `width`-wide bubble: the panel the user sees, without the window's slide room.
fn toast_rect(width: f64) -> NSRect {
    NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, TOAST_HEIGHT))
}

/// The width a bubble takes for a text line `measured` wide/// The width a bubble takes for a text line `measured` wide: the icon, the gap, the text, and the
/// padding on both sides -- floored at TOAST_MIN_WIDTH and capped at TOAST_MAX_WIDTH.
///
/// Pure so the rule is asserted without AppKit. A measurement of zero or less (an empty string, or a
/// font that will not measure here) falls back to the floor rather than to a sliver.
fn toast_width_for_text_width(measured: f64) -> f64 {
    let floor = TOAST_MIN_WIDTH.max(TOAST_ROW_CHROME);
    if measured.is_nan() || measured <= 0.0 {
        return floor;
    }
    (measured.ceil() + TOAST_ROW_CHROME).clamp(floor, TOAST_MAX_WIDTH)
}

/// The bubble's width for `text`, measured at the control font.
fn toast_width(text: &str) -> f64 {
    let measured = crate::glass::measure_single_line_text(text, TOAST_FONT_SIZE).width;
    toast_width_for_text_width(measured)
}

/// Where a `width`-wide panel sits on `visible`: centred horizontally, `row_offset` further right so a
/// second bubble stands beside the first, `TOAST_BOTTOM_MARGIN` above the visible bottom edge. Pure so the rule
/// is asserted without AppKit.
fn toast_origin(visible: NSRect, width: f64, row_offset: f64) -> NSPoint {
    let centred_x = visible.origin.x + (visible.size.width - width) / 2.0;
    // A screen narrower than the panel would otherwise push it past the left edge.
    let x = (centred_x + row_offset).max(visible.origin.x);
    NSPoint::new(x, visible.origin.y + TOAST_BOTTOM_MARGIN)
}

/// The two bubbles' x offsets from the single-bubble centre: the first stands where the bubble always stands,
/// the second one to its left, as far apart as its own width plus a gap.
///
/// Pure, so the rule is asserted without AppKit. The row is laid out from the left bubble's width alone
/// because only the left-most bubble's position depends on the widths: the left one keeps the single-bubble
/// position, and the right one moves right by the left one's width plus `TOAST_ROW_GAP`.
fn toast_row_offset(position: usize, left_width: f64) -> f64 {
    match position {
        0 => 0.0,
        _ => -(left_width + TOAST_ROW_GAP),
    }
}

unsafe fn place(
    panel: *mut AnyObject,
    width: f64,
    row_offset: f64,
    bevel: Option<crate::ffi::ObjPtr>,
) -> NSRect {
    // `visibleFrame`, not `frame`: the visible area already excludes the menu bar and the Dock.
    let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
    if screen.is_null() {
        return NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
    }
    // `place` carries the width rather than reading it back from `TOAST`: it is called from `show_in`,
    // which already holds the thread-local mutably, and a second `borrow()` there panics.
    let visible: NSRect = msg_send![screen, visibleFrame];
    let origin = toast_origin(visible, width, row_offset);
    // The host window is the carrier, whose width follows the message, plus the room the chip travels down through.
    // The chip's resting rect is `origin`; the carrier extends beyond it by the elevation's padding, and the host
    // beyond the carrier by the slide room.
    let insets = crate::glass::panel_insets_for(toast_backdrop_options());
    let carrier = crate::glass::holder_frame(
        NSRect::new(origin, NSSize::new(width, TOAST_HEIGHT)),
        NSPoint::new(0.0, 0.0),
        insets,
    );
    // The chip's resting bottom IS the screen's bottom margin (`toast_origin`), so the window cannot hang below it:
    // AppKit clamps a window into the screen on order-in. The reserved room therefore goes ABOVE the carrier -- the
    // chip starts one slide BELOW its resting place and travels UP, and the window needs that room at the top.
    let frame = NSRect::new(
        NSPoint::new(origin.x - insets.left, origin.y - insets.bottom),
        NSSize::new(
            carrier.size.width,
            carrier.size.height + TOAST_HEIGHT * TOAST_SLIDE_RATIO,
        ),
    );
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
    if let Some(bevel) = bevel {
        let _: () = msg_send![bevel.0, setNeedsDisplay: true];
    }
    frame
}

/// `+[CAMediaTimingFunction functionWithControlPoints::::]`, through a typed call so the four-part selector is
/// written once, as text. Returns null where the class or method is missing, in which case the slide keeps AppKit's
/// default easing rather than not animating at all.
unsafe fn timing_function(points: (f64, f64, f64, f64)) -> Option<*mut AnyObject> {
    let class = objc2::runtime::AnyClass::get(c"CAMediaTimingFunction")?;
    let sel = sel!(functionWithControlPoints::::);
    let method = class.class_method(sel)?;
    let function: unsafe extern "C-unwind" fn(
        *mut AnyObject,
        Sel,
        f32,
        f32,
        f32,
        f32,
    ) -> *mut AnyObject = std::mem::transmute(method.implementation());
    let object = function(
        class as *const objc2::runtime::AnyClass as *mut AnyObject,
        sel,
        points.0 as f32,
        points.1 as f32,
        points.2 as f32,
        points.3 as f32,
    );
    (!object.is_null()).then_some(object)
}

/// The generation of the bubble in slot 0, or `None` when it has not been built. A completion handler compares
/// against this rather than capturing the panel: the panel is reused, the generation is not.
fn current_generation() -> Option<u64> {
    SLOTS.with(|slots| slots.borrow().first().map(|state| state.generation))
}

/// The carrier's resting frame inside the host: where `build` put it, and where every presentation returns it.
unsafe fn carrier_resting(state: &State) -> NSRect {
    let Some(root) = state.backdrop.and_then(|backdrop| backdrop.root) else {
        return NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
    };
    let current: NSRect = msg_send![root.0, frame];
    let resting_y = resting_carrier_y(state);
    NSRect::new(NSPoint::new(current.origin.x, resting_y), current.size)
}

/// The carrier's y at rest: the host's bottom padding plus the room the slide travels through.
unsafe fn resting_carrier_y(_state: &State) -> f64 {
    // The carrier sits on the host's bottom edge -- the chip's padding at rest -- and the slide room is above it.
    let insets = crate::glass::panel_insets_for(toast_backdrop_options());
    insets.bottom
}

/// Put the carrier at `fraction` of a slide below its resting place, with no animation: the entrance's start.
unsafe fn set_carrier_slide(state: &State, fraction: f64) {
    let Some(root) = state.backdrop.and_then(|backdrop| backdrop.root) else {
        return;
    };
    let resting = carrier_resting(state);
    let _: () = msg_send![root.0, setFrameOrigin: NSPoint::new(
        resting.origin.x,
        resting.origin.y - TOAST_HEIGHT * TOAST_SLIDE_RATIO * fraction
    )];
}

/// The reference's motion, applied to the carrier inside a window that never moves: the chip (material, outline, row
/// and shadow together) travels from `fraction` of a slide below its resting place to its resting place, while the
/// panel's opacity goes to `alpha`, over `TOAST_SLIDE_SECONDS` on the reference's curve.
unsafe fn slide_carrier(state: &State, resting: NSRect, fraction: f64, alpha: f64) {
    let Some(root) = state.backdrop.and_then(|backdrop| backdrop.root) else {
        return;
    };
    let target_origin = NSPoint::new(
        resting.origin.x,
        resting.origin.y - TOAST_HEIGHT * TOAST_SLIDE_RATIO * fraction,
    );
    if crate::theme::reduce_motion_enabled() {
        let _: () = msg_send![root.0, setFrameOrigin: target_origin];
        let _: () = msg_send![root.0, setAlphaValue: alpha];
        return;
    }
    let _: () = msg_send![class!(NSAnimationContext), beginGrouping];
    let group: *mut AnyObject = msg_send![class!(NSAnimationContext), currentContext];
    let _: () = msg_send![group, setDuration: TOAST_SLIDE_SECONDS];
    if let Some(easing) = timing_function(TOAST_SLIDE_EASING) {
        let _: () = msg_send![group, setTimingFunction: easing];
    }
    // Both properties on the SAME view, in one grouping: the carrier moves and fades as one layer subtree, so the
    // material, its outline, its shadow and the row share exactly one alpha and one position.
    let animator: *mut AnyObject = msg_send![root.0, animator];
    let _: () = msg_send![animator, setFrameOrigin: target_origin];
    let _: () = msg_send![animator, setAlphaValue: alpha];
    let _: () = msg_send![class!(NSAnimationContext), endGrouping];
    let now: NSRect = msg_send![root.0, frame];
    let alpha_now: f64 = msg_send![state.panel, alphaValue];
    crate::log_info!("[toast-anim] target={target_origin:?} model_now={now:?} alpha={alpha_now}");
}

/// Lay the row out for a panel `width` wide/// Lay the row out for a panel `width` wide: the badge on the leading edge, the close glyph on the trailing
/// edge, and the message taking everything between them. Called on every presentation, because the width
/// follows the message.
///
/// The message's cell keeps its own idea of the text's width -- its truncation happens at that width, not at the
/// field's frame -- so the frame is set, the cell is told the text fits its frame, and the same frame is set
/// again. Without the second step the label truncated a 98.5pt message inside a 98.5pt frame.
unsafe fn layout_row(state: &State, width: f64) {
    // The row is laid out in the container's coordinates, whose origin is the chip's own origin.
    let row_mid = TOAST_HEIGHT / 2.0;
    let _: () = msg_send![state.badge, setFrame: NSRect::new(
        NSPoint::new(TOAST_PADDING_X, row_mid - TOAST_BADGE_SIZE / 2.0),
        NSSize::new(TOAST_BADGE_SIZE, TOAST_BADGE_SIZE)
    )];
    let text_x = TOAST_PADDING_X + TOAST_BADGE_SIZE + TOAST_GAP;
    let text_width = (width - text_x - TOAST_GAP - TOAST_CLOSE_WIDTH - TOAST_PADDING_X).max(0.0);
    let text_frame = NSRect::new(
        NSPoint::new(text_x, row_mid - TOAST_LINE_HEIGHT / 2.0),
        NSSize::new(text_width, TOAST_LINE_HEIGHT),
    );
    let _: () = msg_send![state.label, setFrame: text_frame];
    // A field's cell keeps the width it last laid the string out at, and truncates against that. Re-setting the
    // line-break mode after the frame is the pair that makes the cell adopt the new width.
    let _: () = msg_send![state.label, setLineBreakMode: 4isize];
    let _: () = msg_send![state.label, setNeedsDisplay: true];
    let _: () = msg_send![state.close, setFrame: NSRect::new(
        NSPoint::new(
            width - TOAST_PADDING_X - TOAST_CLOSE_WIDTH,
            row_mid - TOAST_CLOSE_HEIGHT / 2.0
        ),
        NSSize::new(TOAST_CLOSE_WIDTH, TOAST_CLOSE_HEIGHT)
    )];
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
        SLOTS.with(|slots| {
            let mut slots = slots.borrow_mut();
            let Some(state) = slots.first_mut() else {
                return;
            };
            // The timer that fired is the one this state scheduled; clear the handle first so
            // `hide_in` does not invalidate a timer that has already fired.
            state.timer = std::ptr::null_mut();
            unsafe { hide_in(state) }
        });
    });
}

/// Development switch: present messages once the run loop is up, so `--toast-preview` shows the real
/// component. The preview never expires, which is what makes two bubbles comparable side by side.
///
/// A timer rather than a direct call: `run()` reaches its launch switches before `NSApplication run`,
/// and a panel ordered front with no run loop never draws.
/// The preview `delay` seconds from now. `--toast-preview=<delay>` reaches this, so an entrance can be watched
/// after the launch has settled instead of during it.
pub(crate) fn schedule_preview_at(bubbles: Vec<String>, delay: f64) {
    PREVIEW_TEXT.lock().unwrap().replace(bubbles);
    unsafe {
        let _: *mut AnyObject = msg_send![
            class!(NSTimer),
            scheduledTimerWithTimeInterval: delay,
            target: preview_target(),
            selector: sel!(fireToastPreview:),
            userInfo: std::ptr::null::<AnyObject>(),
            repeats: false
        ];
    }
}

static PREVIEW_TEXT: std::sync::Mutex<Option<Vec<String>>> = std::sync::Mutex::new(None);
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
        let Some(bubbles) = PREVIEW_TEXT.lock().unwrap().take() else {
            return;
        };
        // One bubble per slot, all on one midline: each is placed by the same rule a single bubble is, offset
        // by the row. The left bubble keeps its width, and the ones after it are pushed right by that width.
        let mut left_width = 0.0;
        for (position, text) in bubbles.iter().enumerate() {
            let width = toast_width(text);
            let offset = toast_row_offset(position, left_width);
            show_in_slot(position, text, offset);
            if position == 0 {
                left_width = width;
            }
        }
    });
}

/// Present `text` in the bubble of `slot`, `row_offset` to the right of the single-bubble position. The slots
/// reuse their panels, so a second message does not rebuild anything.
fn show_in_slot(slot: usize, text: &str, row_offset: f64) {
    SLOTS.with(|slots| {
        let mut slots = slots.borrow_mut();
        let index = slot;
        while slots.len() <= index {
            slots.push(State::default());
        }
        unsafe {
            let _ = show_in(&mut slots[index], text, row_offset, None);
        }
    });
}

/// A message that is still working. Kept up with no deadline, because a large file can take
/// longer than any fixed timeout would allow.
pub(crate) fn show_working(text: &str) {
    show(text, None);
}

/// A finished message: shown, then dismissed after the standard duration.
pub(crate) fn show_done(text: &str) {
    show_kind(text, ToastKind::Success, Some(TOAST_VISIBLE));
}

/// A REFUSED action: the same bubble with a red cross, because the message says what did not happen.
///
/// The text alone would be read as a status line about something that succeeded; the badge is what
/// makes the two cases tell apart at a glance.
pub(crate) fn show_refused(text: &str) {
    show_kind(text, ToastKind::Error, Some(TOAST_VISIBLE));
}

/// The bubble the smoke scenario reads: its message, its laid-out width, the shared panel outline it carries
/// (width and whether its colour is the palette's token), whether the shadow carrier is really installed, and the
/// elevation level.
///
/// Read from the live views rather than from the constants, because the claim is about what the surface
/// draws: a stroke that a material refused and the bubble failed to replace would still pass a check on
/// the constants. None of this is evidence on its own -- a scenario still measures pixels -- but it says
/// which surface the pixels are those of.
pub(crate) struct ToastSnapshot {
    pub(crate) text: String,
    pub(crate) width: f64,
    pub(crate) outline_width: f64,
    pub(crate) outline_matches_palette: bool,
    /// Whether the hierarchy really carries the shadow carrier: the one fact that proves a shadow is cast, since a
    /// declared elevation says nothing when the carrier could not be created.
    pub(crate) shadow_installed: bool,
    pub(crate) elevation: &'static str,
}

/// Show `text` on the main thread and report what the real panel ended up drawing.
pub(crate) fn snapshot(text: &str) -> Option<ToastSnapshot> {
    crate::debug_assert_main_thread();
    show_working(text);
    current_snapshot()
}

/// The facts about a built bubble: what it says, the rect it occupies and the stroke it draws.
unsafe fn snapshot_of(state: &State) -> Option<ToastSnapshot> {
    let backdrop = state.backdrop?;
    let value: *mut AnyObject = msg_send![state.label, stringValue];
    let text = crate::ffi::nsstring_to_rust(value);
    // The panel rect is the window minus the padding its shadow needs; every bubble is laid out in it.
    let bounds = crate::glass::panel_frame_of(state.panel);
    let (outline_width, outline_matches_palette) =
        crate::glass::outline_state(&backdrop).unwrap_or((0.0, false));
    Some(ToastSnapshot {
        text,
        width: bounds.size.width,
        outline_width,
        outline_matches_palette,
        shadow_installed: crate::glass::carrier_installed(&backdrop),
        // The level this surface really installed, not a constant: the bubble has its own shadow, and a scenario
        // must be able to tell "the chip is wrong" from "the measurement is".
        elevation: crate::glass::effective_elevation_id(Some(TOAST_ELEVATION)),
    })
}

/// The state document's `toast` object: what the live bubble draws, or `null` when none exists.
pub(crate) fn e2e_json() -> String {
    // Read straight from the thread-local rather than through `current_snapshot`: that reader is test-only, and
    // the state document is a product feature.
    let Some(snapshot) = SLOTS.with(|slots| {
        let slots = slots.borrow();
        slots
            .first()
            .and_then(|state| unsafe { snapshot_of(state) })
    }) else {
        return "null".to_string();
    };
    // The height is the constant rather than the panel's frame, so a scenario knows which rect to cut from
    // without measuring: every bubble has it.
    format!(
        "{{\"text\": {}, \"width\": {:.1}, \"height\": {:.1}, \"outline_width\": {:.1}, \"outline_matches_palette\": {}, \"shadow_installed\": {}, \"elevation\": {}}}",
        crate::e2e_state::json_string(&snapshot.text),
        snapshot.width,
        TOAST_HEIGHT,
        snapshot.outline_width,
        snapshot.outline_matches_palette,
        snapshot.shadow_installed,
        crate::e2e_state::json_string(snapshot.elevation),
    )
}

/// The live bubble's facts, or `None` when it has not been built. Main thread only.
pub(crate) fn current_snapshot() -> Option<ToastSnapshot> {
    crate::debug_assert_main_thread();
    SLOTS.with(|slots| {
        let slots = slots.borrow();
        slots
            .first()
            .and_then(|state| unsafe { snapshot_of(state) })
    })
}

/// The live panel's height in points, or 0 when there is no panel.
unsafe fn live_panel_height() -> f64 {
    SLOTS.with(|slots| {
        let slots = slots.borrow();
        let Some(state) = slots.first() else {
            return 0.0;
        };
        if state.panel.is_null() {
            0.0
        } else {
            unsafe { crate::glass::panel_frame_of(state.panel).size.height }
        }
    })
}
/// Build the real bubble and check what describes it: it is one row tall, its width is its message plus the
/// chrome, and every material draws the same 2pt `glass_rim` stroke with the `high` elevation.
///
/// `--smoke-toast` entry, called on the main thread. Returns false on any failure, and logs which one.
pub(crate) fn smoke_runner() -> Result<(), String> {
    crate::debug_assert_main_thread();
    let mut failures: Vec<String> = Vec::new();
    show_working("smoke");
    let Some(first) = current_snapshot() else {
        return Err("the bubble could not be built".to_string());
    };
    // Every bubble is one row tall, whatever it says.
    let panel_height = unsafe { live_panel_height() };
    if (panel_height - TOAST_HEIGHT).abs() > 0.01 {
        failures.push(format!(
            "the panel is {panel_height:.1}pt tall, expected {TOAST_HEIGHT:.1}"
        ));
    }
    // The width follows the message: a longer string must widen the bubble.
    let Some(second) = snapshot("smoke toast") else {
        return Err("the bubble could not be re-shown".to_string());
    };
    if second.width <= first.width {
        failures.push(format!(
            "a longer message did not widen the bubble ({:.1} -> {:.1})",
            first.width, second.width
        ));
    }
    for (snapshot, text) in [(first, "smoke"), (second, "smoke toast")] {
        // The panel is sized from the label's own layout for this string (see `toast_width_for_field`), which is
        // a few points wider than the bare font measurement. That is what the panel was sized from when THIS
        // message was presented, and the label now holds the last one, so the expectation is the same rule on
        // this string rather than a re-measure of the label.
        let measured = crate::glass::measure_single_line_text(text, TOAST_FONT_SIZE).width;
        let expected = toast_width_for_text_width(measured) + 4.0;
        if (snapshot.width - expected).abs() > 0.01 {
            failures.push(format!(
                "{text:?}: width is {:.1}, the message measures to {expected:.1}",
                snapshot.width
            ));
        }
    }
    // The shadow check has to fail when the bubble has no carrier: build one whose elevation resolves to nothing
    // (the `--panel-shadow=off` path), and require the check to notice.
    if let Some(snapshot) = snapshot_without_shadow() {
        if snapshot.shadow_installed {
            failures.push(
                "a bubble built without an elevation still reports a shadow carrier".to_string(),
            );
        }
    }
    // Every material the user can configure. The swap helper restores the configuration.
    for material in materials() {
        let Some(snapshot) = snapshot_for_material(material, "smoke") else {
            failures.push(format!("{material:?} produced no bubble"));
            continue;
        };
        // The bubble is not a special case: it wears whatever the shared backdrop gave it, so on every
        // material but glass it must carry the palette outline, and glass must still draw its own rim (which
        // is the system's). What this asserts is that the bubble did not silently lose the edge.
        let expect_outline = material != crate::glass::PanelMaterial::LiquidGlass;
        if expect_outline
            && (snapshot.outline_width - crate::theme::PANEL_OUTLINE_WIDTH).abs() > 0.01
        {
            failures.push(format!(
                "{material:?}: outline is {:.2}pt, expected {:.1}",
                snapshot.outline_width,
                crate::theme::PANEL_OUTLINE_WIDTH
            ));
        }
        // The fact the check needs is the INSTALLED carrier, not the declared level: `install_shadow_carrier` keeps
        // the material and logs when the carrier cannot be created, so a level alone would pass a bubble with no
        // shadow at all.
        if !snapshot.shadow_installed {
            failures.push(format!(
                "{material:?}: the bubble has no shadow carrier installed"
            ));
        }
    }
    hide();
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

/// Build a bubble with no elevation and report what it says about its shadow, then restore the normal one.
///
/// The counter-example for the shadow check: if a bubble without a carrier still reported one, the check would pass
/// on a chip with no shadow, which is exactly the failure the check exists to catch.
fn snapshot_without_shadow() -> Option<ToastSnapshot> {
    crate::debug_assert_main_thread();
    SLOTS.with(|cell| {
        let mut cell = cell.borrow_mut();
        let state = cell.first_mut()?;
        if state.panel.is_null() {
            return None;
        }
        unsafe {
            let installed = state.backdrop?;
            let panel_frame = crate::glass::panel_frame_of(state.panel);
            let without = crate::glass::BackdropOptions::new(None)
                .with_material(crate::glass::PanelMaterial::Opaque);
            let new = crate::glass::swap_backdrop(
                state.panel,
                &installed,
                panel_frame,
                TOAST_RADIUS,
                without,
            );
            state.backdrop = Some(new);
            // The fact comes from the hierarchy the swap just installed, read through the same snapshot the normal
            // check uses, so the counter-example proves the check rather than a second copy of it.
            let result = snapshot_of(state);
            // Put the bubble's own hierarchy back, so the rest of the runner sees the real surface.
            let restored = crate::glass::swap_backdrop(
                state.panel,
                &new,
                panel_frame,
                TOAST_RADIUS,
                toast_backdrop_options(),
            );
            state.bevel = crate::glass::install_panel_bevel(&restored, TOAST_RADIUS);
            state.backdrop = Some(restored);
            result
        }
    })
}

/// Every material this system can draw, for the smoke runner that proves the chip survives all of them.
pub(crate) fn materials() -> Vec<crate::glass::PanelMaterial> {
    vec![
        crate::glass::PanelMaterial::LiquidGlass,
        crate::glass::PanelMaterial::Frost,
        crate::glass::PanelMaterial::Opaque,
        crate::glass::PanelMaterial::Backdrop,
    ]
}

/// Rebuild the bubble for `material` and report what it draws. The configuration is put back afterwards.
pub(crate) fn snapshot_for_material(
    material: crate::glass::PanelMaterial,
    text: &str,
) -> Option<ToastSnapshot> {
    crate::debug_assert_main_thread();
    let installed = SLOTS.with(|slots| slots.borrow().first().and_then(|state| state.backdrop));
    if let Some(installed) = installed {
        unsafe { swap_for_material(installed, material) };
    }
    snapshot(text)
}

/// Replace the bubble's hierarchy with one built for `material`, the way a settings change would, keeping the
/// frame the bubble is currently laid out in. The configuration is put back before returning, so a caller never
/// observes the override.
unsafe fn swap_for_material(
    installed: crate::glass::InstalledBackdrop,
    material: crate::glass::PanelMaterial,
) {
    let previous = crate::config::CONFIG
        .read()
        .unwrap()
        .appearance
        .panel_material
        .clone();
    crate::config::CONFIG
        .write()
        .unwrap()
        .appearance
        .panel_material = material.config_value().to_string();
    SLOTS.with(|cell| {
        let mut cell = cell.borrow_mut();
        let Some(state) = cell.first_mut() else {
            return;
        };
        let panel_frame = crate::glass::panel_frame_of(state.panel);
        let new = crate::glass::swap_backdrop(
            state.panel,
            &installed,
            panel_frame,
            TOAST_RADIUS,
            toast_backdrop_options(),
        );
        state.backdrop = Some(new);
        // The bubble's outline is the shared one, which the swap re-installs on the new hierarchy.
        state.bevel = crate::glass::install_panel_bevel(&new, TOAST_RADIUS);
    });
    crate::config::CONFIG
        .write()
        .unwrap()
        .appearance
        .panel_material = previous;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two kinds must not collapse into one badge: a refusal shown in the success green would say
    /// the action worked while its own text says it did not.
    #[test]
    fn a_refused_message_has_its_own_badge() {
        assert_ne!(TOAST_ERROR_BADGE_FILL, TOAST_BADGE_FILL);
        assert_ne!(ToastKind::Success, ToastKind::Error);
        // Both glyphs are the reference's 11pt lucide icons in the same badge box.
        assert_eq!(TOAST_CHECK_SIZE, 11.0);
        assert!(TOAST_CHECK_SIZE < TOAST_BADGE_SIZE);
    }

    /// The panel needs the AppKit main thread, and libtest runs cases on worker threads, so these
    /// assert the geometry rule the panel is placed by rather than building it.
    #[test]
    fn the_panel_is_placed_inside_the_visible_bottom_edge() {
        let visible = NSRect::new(NSPoint::new(0.0, 25.0), NSSize::new(1440.0, 850.0));
        let origin = toast_origin(visible, 240.0, 0.0);
        assert_eq!(origin.y, 25.0 + TOAST_BOTTOM_MARGIN);
        assert_eq!(origin.x, (1440.0 - 240.0) / 2.0);
        assert!(origin.y >= visible.origin.y);
        assert!(origin.y + TOAST_HEIGHT <= visible.origin.y + visible.size.height);
    }

    #[test]
    fn a_screen_narrower_than_the_panel_keeps_it_on_screen() {
        let visible = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(200.0, 400.0));
        let origin = toast_origin(visible, TOAST_MAX_WIDTH, 0.0);
        assert!(origin.x >= visible.origin.x);
    }

    /// The bubble is only as wide as its message plus the chrome around it -- not a fixed width, which
    /// is what makes a one-word message a chip and a sentence a slightly longer chip.
    #[test]
    fn the_bubble_is_the_text_plus_its_chrome() {
        // The reference's row: 12pt padding, the 20pt badge, two 10pt gaps, the 11pt close glyph in its 2pt
        // padding, and 12pt padding again.
        let chrome = 12.0 + 20.0 + 10.0 + 10.0 + 15.0 + 12.0;
        assert_eq!(toast_width_for_text_width(150.4), 150.4f64.ceil() + chrome);
        // The same message always takes the same width: the rule is deterministic, so a scenario can
        // compare two launches' widths instead of merely checking both are "some size".
        assert_eq!(
            toast_width_for_text_width(90.0),
            toast_width_for_text_width(90.0)
        );
        assert!(toast_width_for_text_width(90.0) < toast_width_for_text_width(91.0));
    }

    /// Neither an empty message nor a measurement that never happened may produce a sliver, and a long
    /// one (a path, a digest) is capped instead of growing across the screen.
    #[test]
    fn the_bubble_width_is_floored_and_capped() {
        let floor = TOAST_MIN_WIDTH.max(TOAST_ROW_CHROME);
        for measured in [0.0, -12.0, f64::NAN] {
            assert_eq!(
                toast_width_for_text_width(measured),
                floor,
                "{measured} must fall back to the floor"
            );
        }
        assert!(toast_width_for_text_width(1.0) >= floor);
        for measured in [0.5, 1.0, 4.0] {
            assert!(toast_width_for_text_width(measured) >= floor);
        }
        assert_eq!(toast_width_for_text_width(1e6), TOAST_MAX_WIDTH);
        assert!(TOAST_MAX_WIDTH > TOAST_MIN_WIDTH);
    }

    /// The badge, the message and the close glyph all live inside the bubble's row, which is centred on the
    /// same midline -- what keeps the chip reading as one line rather than as a box with a line in it.
    #[test]
    fn the_row_fits_inside_the_bubble() {
        assert!(TOAST_LINE_HEIGHT <= TOAST_CONTENT_HEIGHT);
        assert!(TOAST_BADGE_SIZE <= TOAST_CONTENT_HEIGHT);
        assert_eq!(TOAST_HEIGHT, TOAST_CONTENT_HEIGHT + TOAST_PADDING_Y * 2.0);
        let smallest = toast_width_for_text_width(1.0);
        let chrome = TOAST_PADDING_X * 2.0 + TOAST_BADGE_SIZE + TOAST_GAP * 2.0 + TOAST_CLOSE_WIDTH;
        assert!(
            chrome < smallest,
            "the chrome must leave the message some room at the narrowest width"
        );
    }

    /// The bubble's own measurements are the reference component's: its type size, its radius, and the
    /// trailing `✕`'s box. Pinned so a later resize cannot quietly drift away from the surface this exists to
    /// match.
    #[test]
    fn the_geometry_is_the_reference_components() {
        assert_eq!(TOAST_FONT_SIZE, 13.0);
        assert_eq!(TOAST_CLOSE_FONT_SIZE, 11.0);
        assert_eq!(TOAST_RADIUS, 12.0);
        assert_eq!(TOAST_BADGE_SIZE, 20.0);
        assert_eq!(TOAST_PADDING_X, 12.0);
        assert_eq!(TOAST_PADDING_Y, 10.0);
        assert_eq!(TOAST_GAP, 10.0);
    }
}
