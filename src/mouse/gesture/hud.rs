//! The gesture trail window: a borderless, click-through overlay that follows the pointer.
//!
//! Two constraints shape this module:
//!
//! * It must never take focus or hit-testing. The window is borderless with \`ignoresMouseEvents\` and a
//!   backing level high enough to sit over application windows.
//! * The tap thread must never block on it. Trail updates are **coalesced**: the tap publishes "the
//!   latest trail" and a single main-thread task drains it, so a fast drag cannot queue one task per
//!   event. A hide is a separate, higher-priority request that is never merged away.
//!
//! Drawing happens on the main thread only; AppKit is not safe from the tap thread.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use std::ffi::c_void;

use crate::ffi::{
    make_nsstring, CFRelease, CGPathAddLineToPoint, CGPathCreateMutable, CGPathMoveToPoint,
};

use super::runtime::Trail;

/// The newest trail the tap thread wants drawn. Overwritten, never queued: a slow main thread sees
/// the latest state and skips the ones it missed.
static PENDING: Mutex<Option<Trail>> = Mutex::new(None);
/// Whether a draw task is already scheduled, so a burst of events schedules one task, not N.
static DRAW_SCHEDULED: AtomicBool = AtomicBool::new(false);
/// The session whose hide is waiting. It outranks any draw, but only for its own session: a hide
/// queued for gesture N must not close the frame gesture N+1 is drawing.
static HIDE_SESSION: AtomicU64 = AtomicU64::new(0);
/// \`true\` when a hide request is waiting (any session).
static HIDE_PENDING: AtomicBool = AtomicBool::new(false);
/// The session the window currently draws, so a stale task cannot clear a newer session's frame.
static DRAWN_SESSION: AtomicU64 = AtomicU64::new(0);

/// Publish the trail from the tap thread. Cheap: one lock and an atomic flag, no AppKit.
pub(crate) fn publish(trail: Trail) {
    if let Ok(mut slot) = PENDING.lock() {
        *slot = Some(trail);
    }
    if !DRAW_SCHEDULED.swap(true, Ordering::SeqCst) {
        crate::schedule_gesture_hud();
    }
}

/// Ask the main thread to hide the trail for \`session\`. Outranks a pending draw of the same session
/// only; a newer session's frame is left alone.
pub(crate) fn hide(session: u64) {
    HIDE_SESSION.store(session, Ordering::SeqCst);
    HIDE_PENDING.store(true, Ordering::SeqCst);
    if !DRAW_SCHEDULED.swap(true, Ordering::SeqCst) {
        crate::schedule_gesture_hud();
    }
}

/// The main-thread entry point the controller selector calls. Bridges the AppKit callback shape to
/// [[service]], so the scheduling logic stays a plain function the tests can drive.
pub(crate) extern "C" fn on_hud_request(
    _self: *mut std::ffi::c_void,
    _cmd: Sel,
    _arg: *mut std::ffi::c_void,
) {
    // Delivering this request in a DEFERRED task must still release the scheduler's claim: the
    // selector defers and the deferred task is `on_deferred_request`, which releases. This function
    // keeps its own release for the direct path.
    crate::callback_guard::void("on_hud_request", || {
        if crate::settings::settings_ui_is_busy() {
            crate::log_debug!("[gesture] hud request deferred: a settings callback is running");
            defer_to_main();
            return;
        }
        // A panic inside `service` must not leave the scheduler's claim held -- that is what froze
        // every later draw after the first frame failed -- so the release runs on the way out either
        // way. A request that lands while this task runs sets the claim again, and the drain loop in
        // `service` sees it.
        service();
        crate::release_gesture_hud_claim();
    });
}

/// Deliver a request that arrived while a settings callback was running.
///
/// The tap thread cannot see the settings window's borrow, so it asks the busy flag instead; this
/// is the task it queues when the answer is "not now". By the time it runs the callback has
/// returned, so it may look at the window either way.
pub(crate) extern "C" fn on_deferred_request(
    _self: *mut std::ffi::c_void,
    _cmd: Sel,
    _arg: *mut std::ffi::c_void,
) {
    crate::callback_guard::void("on_deferred_request", || {
        crate::log_debug!("[gesture] hud deferred task ran");
        if !crate::settings::settings_ui_is_busy() {
            crate::settings::gesture_recorder::refresh_recording_row_on_main();
        }
        service();
        crate::release_gesture_hud_claim();
    });
}

/// Queue the whole request again for the next run-loop turn, through the settings window's own
/// deferred target.
fn defer_to_main() {
    let Some(target) = crate::MENU_TARGET_DISPATCH
        .lock()
        .ok()
        .and_then(|slot| *slot)
        .map(|target| target.0)
    else {
        return;
    };
    unsafe {
        let _: () = objc2::msg_send![
            target,
            performSelectorOnMainThread: objc2::sel!(handleGestureHudDeferred:),
            withObject: std::ptr::null::<objc2::runtime::AnyObject>(),
            waitUntilDone: false
        ];
    }
}

/// Apply the latest request, then release the schedule flag so the next publish can schedule again.
/// Order matters -- the flag is cleared *after* the work, and the tap's publish path sets it *before*
/// it looks at the queue, so a publish during this task is either seen here or schedules its own.
pub(crate) fn service() {
    // A revoked switcher claim is dismissed here, on the main thread, because the revocation
    // itself can be raised from a background stop path.
    super::switcher::revoke_on_main();
    // A recording that finished on the tap thread is committed here, on the main thread, after the
    // live configuration has been checked again.
    crate::settings::gesture_recorder::commit_published_result();
    // A recording's live preview: the page's record row is rewritten in place from the coalesced
    // snapshot the tap thread publishes, not rebuilt per sample.
    crate::settings::gesture_recorder::refresh_recording_row_on_main();
    loop {
        if HIDE_PENDING.swap(false, Ordering::SeqCst) {
            let ending = HIDE_SESSION.load(Ordering::SeqCst);
            // Drop only the queued draw of the session being closed: a newer session's trail must
            // survive a late hide from the previous one.
            if let Ok(mut slot) = PENDING.lock() {
                if slot.as_ref().is_some_and(|trail| trail.session == ending) {
                    *slot = None;
                }
            }
            with_window(|window| window.hide(ending));
        } else {
            if let Some(trail) = PENDING.lock().ok().and_then(|mut slot| slot.take()) {
                with_window(|window| window.draw(&trail));
            }
        }
        DRAW_SCHEDULED.store(false, Ordering::SeqCst);
        // Re-check: a publish that landed while the flag was still set did not schedule anything,
        // so this task has to continue. The two atomics are the handshake that keeps that update.
        let pending = HIDE_PENDING.load(Ordering::SeqCst)
            || PENDING.lock().map(|slot| slot.is_some()).unwrap_or(false);
        if pending && !DRAW_SCHEDULED.swap(true, Ordering::SeqCst) {
            continue;
        }
        break;
    }
}

/// The window itself. Created lazily on first use, hidden (not destroyed) between gestures so a
/// second gesture does not pay for window creation during a drag.
struct TrailWindow {
    view: Option<TrailView>,
}

thread_local! {
    /// AppKit objects live on the main thread only; a thread-local keeps the ownership obvious.
    static WINDOW: std::cell::RefCell<Option<Box<TrailWindow>>> =
        const { std::cell::RefCell::new(None) };
}

/// Run \`f\` with the window, if AppKit is usable. On a non-main thread it declines rather than
/// touching AppKit: the tap thread calls only [[publish]] and [[hide]].
fn with_window(f: impl FnOnce(&mut TrailWindow)) {
    if !crate::is_main_thread() {
        return;
    }
    WINDOW.with(|cell| {
        let mut slot = cell.borrow_mut();
        let window = slot.get_or_insert_with(|| Box::new(TrailWindow { view: None }));
        f(window);
    });
}

/// The trail window's own view: a borderless, click-through overlay the stroke is drawn into.
///
/// The stroke is a CGPath in a CAShapeLayer -- one layer, replaced per frame, so a fast drag costs
/// one property write rather than a view-tree walk. The window covers the screen the pointer is on
/// because a gesture may run anywhere; `ignoresMouseEvents` keeps it from taking any of the input it
/// is describing.
struct TrailView {
    window: *mut AnyObject,
    view: *mut AnyObject,
    stroke: *mut AnyObject,
    /// The hint card (a dark plate), its glyph view and its text label.
    card: *mut AnyObject,
    glyph: *mut AnyObject,
    hint: *mut AnyObject,
    /// The screen frame the window was built for, and the CG->AppKit vertical baseline: the primary
    /// display's top edge in AppKit coordinates. CG global coordinates have their origin at the
    /// primary top-left with y growing down, so that is the only correct flip.
    screen_origin: NSPoint,
    primary_top: f64,
    /// Whether the window is ordered in right now. Ordering front on every frame is both wasteful and
    /// wrong; ordering in only at construction is why a SECOND gesture drew into a hidden window.
    shown: bool,
}

impl Default for TrailView {
    fn default() -> Self {
        Self {
            window: std::ptr::null_mut(),
            view: std::ptr::null_mut(),
            stroke: std::ptr::null_mut(),
            card: std::ptr::null_mut(),
            glyph: std::ptr::null_mut(),
            hint: std::ptr::null_mut(),
            screen_origin: NSPoint::new(0.0, 0.0),
            primary_top: 0.0,
            shown: false,
        }
    }
}

/// What the hint card says: the action the stroke would run, and the last direction it took.
///
/// The direction is part of the answer, not decoration: the card's arrow is how a user reads which
/// way the stroke is heading without looking away from the pointer.
///
/// A stroke no row matches has no direction to point at, so its card carries no arrow and
/// `direction` is `None`; the words alone say the stroke is not a gesture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HintText {
    pub(crate) direction: Option<char>,
    pub(crate) text: String,
}

impl HintText {
    /// Whether the card has an arrow to draw: only a direction names one.
    pub(crate) fn has_arrow(&self) -> bool {
        self.direction.is_some()
    }
}

/// CALayer's `lineCap` / `lineJoin` are NSString constants (`kCALineCapRound` ...), not integers.
const LINE_CAP_ROUND: &str = "round";
const LINE_JOIN_ROUND: &str = "round";

/// The frame of the screen the pointer is on, in AppKit global coordinates.
///
/// Not `NSScreen.mainScreen`: that follows the key window, not the pointer, and a gesture can start
/// on a display the settings window is not on.
unsafe fn pointer_screen_frame() -> Option<NSRect> {
    let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
    if screens.is_null() {
        return None;
    }
    let count: usize = msg_send![screens, count];
    if count == 0 {
        return None;
    }
    let pointer: NSPoint = msg_send![class!(NSEvent), mouseLocation];
    for index in 0..count {
        let screen: *mut AnyObject = msg_send![screens, objectAtIndex: index as isize];
        let frame: NSRect = msg_send![screen, frame];
        if pointer.x >= frame.origin.x
            && pointer.x <= frame.origin.x + frame.size.width
            && pointer.y >= frame.origin.y
            && pointer.y <= frame.origin.y + frame.size.height
        {
            return Some(frame);
        }
    }
    let first: *mut AnyObject = msg_send![screens, objectAtIndex: 0isize];
    Some(msg_send![first, frame])
}

impl TrailView {
    /// Create the window when it does not exist yet, and order it in when it is hidden.
    fn show(&mut self) {
        unsafe {
            // The pointer may have moved to another display since the window was built; the frame
            // and the coordinate baseline belong to the screen the stroke is on.
            if !self.window.is_null() && !self.covers_pointer_screen() {
                self.destroy();
            }
            if self.window.is_null() {
                self.build();
            }
            if self.window.is_null() {
                return;
            }
            if !self.shown {
                let _: () = msg_send![self.window, orderFrontRegardless];
                self.shown = true;
            }
        }
    }

    unsafe fn build(&mut self) {
        let Some(screen_frame) = pointer_screen_frame() else {
            return;
        };
        let primary_top = crate::overlay::hover::primary_screen_frame()
            .map(|frame| frame.origin.y + frame.size.height)
            .unwrap_or(screen_frame.origin.y + screen_frame.size.height);
        let window: *mut AnyObject = msg_send![class!(NSWindow), alloc];
        let window: *mut AnyObject = msg_send![
            window,
            initWithContentRect: screen_frame,
            styleMask: 0u64, // borderless
            backing: 2u64,   // buffered
            defer: false
        ];
        let _: () = msg_send![window, setOpaque: false];
        let clear: *mut AnyObject = msg_send![class!(NSColor), clearColor];
        let _: () = msg_send![window, setBackgroundColor: clear];
        let _: () = msg_send![window, setIgnoresMouseEvents: true];
        let _: () = msg_send![window, setReleasedWhenClosed: false];
        let _: () = msg_send![window, setHasShadow: false];
        // NSStatusWindowLevel: over every ordinary window, under the menu bar and the Dock.
        let _: () = msg_send![window, setLevel: 25isize];
        // CanJoinAllSpaces | Stationary | FullScreenAuxiliary: a high level alone does not put a
        // window on another Space or over a full-screen app.
        let behavior: u64 = (1u64 << 0) | (1u64 << 4) | (1u64 << 8);
        let _: () = msg_send![window, setCollectionBehavior: behavior];
        let content: *mut AnyObject = msg_send![window, contentView];
        let _: () = msg_send![content, setWantsLayer: true];
        let view: *mut AnyObject = msg_send![class!(NSView), alloc];
        let view: *mut AnyObject = msg_send![
            view,
            initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), screen_frame.size)
        ];
        let _: () = msg_send![view, setWantsLayer: true];
        let _: () = msg_send![content, addSubview: view];
        // The stroke: a stroked path layer, no fill.
        let stroke: *mut AnyObject = msg_send![class!(CAShapeLayer), layer];
        // The colours and the path are CG types, which objc2's own msg_send! cannot encode (it
        // sends a bare pointer as an object or void* and the runtime refuses the argument), so they
        // go through the raw senders in ffi alongside the other layer colours.
        crate::ffi::shape_layer_set_fill_color(stroke, std::ptr::null_mut());
        let accent = crate::theme::ui_palette().accent;
        let color = crate::ffi::hex_to_cg_color(accent);
        crate::ffi::shape_layer_set_stroke_color(stroke, color);
        // hex_to_cg_color hands back NSColor's own colour: the caller does not own it, so it is
        // not released here.
        let _: () = msg_send![stroke, setLineWidth: 5.0f64];
        let cap = make_nsstring(LINE_CAP_ROUND);
        let _: () = msg_send![stroke, setLineCap: cap];
        CFRelease(cap as *const c_void);
        let join = make_nsstring(LINE_JOIN_ROUND);
        let _: () = msg_send![stroke, setLineJoin: join];
        CFRelease(join as *const c_void);
        let layer_frame = NSRect::new(NSPoint::new(0.0, 0.0), screen_frame.size);
        let _: () = msg_send![stroke, setFrame: layer_frame];
        let view_layer: *mut AnyObject = msg_send![view, layer];
        let _: () = msg_send![view_layer, addSublayer: stroke];
        // The hint card: a dark rounded plate with an arrow and the action's name under it.
        //
        // Deliberately composed of AppKit's own controls -- a layer-backed plain NSView for the
        // plate, an NSImageView for the arrow and an NSTextField for the label -- because that is the
        // combination this app draws everywhere else. A hand-drawn card meant overriding drawRect:,
        // and every crash this module had came from that path: the view's layer repaints from its
        // contents, and a drawing that touches the wrong lifetime inside it takes the whole process
        // down in the display transaction.
        let card: *mut AnyObject = msg_send![class!(NSView), alloc];
        let card: *mut AnyObject = msg_send![
            card,
            initWithFrame: NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(
                    crate::theme::GESTURE_HINT_MIN_WIDTH,
                    crate::theme::GESTURE_HINT_HEIGHT
                )
            )
        ];
        let _: () = msg_send![card, setWantsLayer: true];
        let card_layer: *mut AnyObject = msg_send![card, layer];
        if !card_layer.is_null() {
            crate::ffi::layer_set_background(
                card_layer,
                crate::ffi::hex_to_cg_color(crate::theme::GESTURE_HINT_BG),
            );
            let _: () = msg_send![card_layer, setCornerRadius: crate::theme::GESTURE_HINT_RADIUS];
        }
        // The arrow: an image view whose image is resized to the box, the way every other symbol in
        // this app is drawn.
        let glyph: *mut AnyObject = msg_send![class!(NSImageView), alloc];
        let glyph: *mut AnyObject = msg_send![
            glyph,
            initWithFrame: NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(
                    crate::theme::GESTURE_HINT_GLYPH_SIZE,
                    crate::theme::GESTURE_HINT_GLYPH_SIZE
                )
            )
        ];
        let _: () = msg_send![glyph, setImageScaling: 3isize]; // proportionally, inside the box
        let ink = crate::ffi::hex_to_ns_color(crate::theme::GESTURE_HINT_INK);
        let _: () = msg_send![glyph, setContentTintColor: ink];
        crate::release_obj(ink);
        let _: () = msg_send![card, addSubview: glyph];
        // The label: a plain text field, centred, on one line.
        let hint: *mut AnyObject =
            msg_send![class!(NSTextField), labelWithString: make_nsstring("")];
        let _: () = msg_send![hint, setBezeled: false];
        let _: () = msg_send![hint, setDrawsBackground: false];
        let _: () = msg_send![hint, setEditable: false];
        let _: () = msg_send![hint, setSelectable: false];
        // A `labelWithString:` field carries its own cell metrics, and the alignment is set before the
        // frame is known: set both again after every layout below, or the string is drawn from its
        // cell's left inset and reads off-centre however wide the frame is.
        let _: () = msg_send![hint, setAlignment: 2isize]; // NSTextAlignmentCenter
        let _: () = msg_send![hint, setUsesSingleLineMode: true];
        let font: *mut AnyObject =
            msg_send![class!(NSFont), systemFontOfSize: crate::theme::GESTURE_HINT_FONT];
        let _: () = msg_send![hint, setFont: font];
        let text_color = crate::ffi::hex_to_ns_color(crate::theme::GESTURE_HINT_INK);
        let _: () = msg_send![hint, setTextColor: text_color];
        crate::release_obj(text_color);
        let _: () = msg_send![card, addSubview: hint];
        let _: () = msg_send![card, setHidden: true];
        let _: () = msg_send![view, addSubview: card];
        self.card = card;
        self.glyph = glyph;
        self.hint = hint;
        self.screen_origin = screen_frame.origin;
        self.primary_top = primary_top;
        self.window = window;
        self.view = view;
        self.stroke = stroke;
    }

    /// Draw one frame: the stroke through `points` (CG global points, top-left origin) and, when
    /// `hint` is Some, the text that says what the stroke would run.
    fn draw_stroke(
        &mut self,
        points: &[crate::mouse::gesture::recognizer::Point],
        hint: Option<HintText>,
    ) {
        unsafe {
            // The view's bounds on the FIRST frame of a freshly built window can still be the
            // pre-layout size; the window's own frame is what the screen says, so the card lands
            // centred from the first sample and not only from the second one.
            let window_frame: NSRect = msg_send![self.window, frame];
            let bounds: NSRect = if window_frame.size.width > 1.0 && window_frame.size.height > 1.0
            {
                NSRect::new(NSPoint::new(0.0, 0.0), window_frame.size)
            } else {
                msg_send![self.view, bounds]
            };
            if bounds.size.width < 1.0 || bounds.size.height < 1.0 {
                return;
            }
            let path = CGPathCreateMutable();
            for (index, point) in points.iter().enumerate() {
                let local = self.global_to_local(point.x, point.y);
                if index == 0 {
                    CGPathMoveToPoint(path, std::ptr::null(), local.x, local.y);
                } else {
                    CGPathAddLineToPoint(path, std::ptr::null(), local.x, local.y);
                }
            }
            crate::ffi::shape_layer_set_path(self.stroke, path);
            CFRelease(path as *const c_void);
            let Some(hint) = hint else {
                let _: () = msg_send![self.card, setHidden: true];
                return;
            };

            // The card's own class paints the arrow and the text (see `gesture_card_draw`), so
            // every position below is a painted coordinate, not a view frame AppKit may reinterpret.
            // The card is centred on the screen the gesture is drawn on: it is a status readout, and
            // one that follows the stroke is distracting while the hand is moving.
            let pad = crate::theme::GESTURE_HINT_PADDING;
            let glyph_box = crate::theme::GESTURE_HINT_GLYPH_SIZE;
            let line = crate::theme::GESTURE_HINT_LINE;
            let w = crate::theme::GESTURE_HINT_MIN_WIDTH;
            // A card with no arrow keeps the same plate and puts its single line in the middle: a
            // stroke nothing matches has no direction to show, only the words that say so.
            let has_arrow = hint.has_arrow();
            let h = if has_arrow {
                (glyph_box + crate::theme::GESTURE_HINT_GAP_INNER + line + pad * 2.0)
                    .max(crate::theme::GESTURE_HINT_HEIGHT)
            } else {
                crate::theme::GESTURE_HINT_HEIGHT
            };
            let x = ((bounds.size.width - w) / 2.0).max(0.0);
            let y = ((bounds.size.height - h) / 2.0).max(0.0);
            let _: () = msg_send![
                self.card,
                setFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
            ];
            // The arrow (when there is one) and the label are laid out inside the plate, top to
            // bottom; a text-only card centres its line instead.
            if !self.glyph.is_null() {
                let _: () = msg_send![self.glyph, setHidden: !has_arrow];
                if has_arrow {
                    let _: () = msg_send![
                        self.glyph,
                        setFrame: NSRect::new(
                            NSPoint::new((w - glyph_box) / 2.0, h - glyph_box - pad),
                            NSSize::new(glyph_box, glyph_box)
                        )
                    ];
                }
            }
            let label_y = if has_arrow {
                pad * 0.75
            } else {
                (h - line) / 2.0
            };
            if !self.hint.is_null() {
                let _: () = msg_send![
                    self.hint,
                    setFrame: NSRect::new(
                        NSPoint::new(pad, label_y),
                        NSSize::new(w - pad * 2.0, line)
                    )
                ];
            }
            let symbol = match hint.direction {
                Some('L') => "arrow.left",
                Some('R') => "arrow.right",
                Some('U') => "arrow.up",
                Some('D') => "arrow.down",
                _ => "arrow.right",
            };
            let symbol_ns = make_nsstring(symbol);
            let image: *mut AnyObject = msg_send![
                class!(NSImage),
                imageWithSystemSymbolName: symbol_ns,
                accessibilityDescription: std::ptr::null::<AnyObject>()
            ];
            CFRelease(symbol_ns as *const c_void);
            if has_arrow && !image.is_null() && !self.glyph.is_null() {
                let _: () = msg_send![image, setSize: NSSize::new(glyph_box, glyph_box)];
                let _: () = msg_send![self.glyph, setImage: image];
            }
            if !self.hint.is_null() {
                let title = make_nsstring(&hint.text);
                let _: () = msg_send![self.hint, setStringValue: title];
                CFRelease(title as *const c_void);
                // Re-assert the centring after the text and the frame: a cell that was given its
                // string while the frame was still the initial one lays the line out against that
                // frame's left edge.
                let _: () = msg_send![self.hint, setAlignment: 2isize];
                let _: () = msg_send![self.hint, sizeToFit];
                let fitted: NSRect = msg_send![self.hint, frame];
                let _: () = msg_send![
                    self.hint,
                    setFrame: NSRect::new(
                        NSPoint::new((w - fitted.size.width) / 2.0, label_y),
                        NSSize::new(fitted.size.width, line)
                    )
                ];
            }
            let _: () = msg_send![self.card, setHidden: false];
        }
    }
    /// A CG global point (origin at the primary display's top-left, y down) as a point in this view
    /// (origin at the window's bottom-left, y up).
    fn global_to_local(&self, x: f64, y: f64) -> NSPoint {
        let appkit_y = self.primary_top - y;
        NSPoint::new(x - self.screen_origin.x, appkit_y - self.screen_origin.y)
    }

    /// Whether the built window is the screen the pointer is on.
    fn covers_pointer_screen(&self) -> bool {
        match unsafe { pointer_screen_frame() } {
            Some(frame) => {
                (frame.origin.x - self.screen_origin.x).abs() < 1.0
                    && (frame.origin.y - self.screen_origin.y).abs() < 1.0
            }
            // The question could not be answered; keep the window rather than churn one per frame.
            None => true,
        }
    }

    /// Release a window built for another display. The layer and views go with it.
    fn destroy(&mut self) {
        if self.window.is_null() {
            return;
        }
        unsafe {
            let _: () = msg_send![self.window, orderOut: std::ptr::null_mut::<AnyObject>()];
            let _: () = msg_send![self.window, close];
        }
        self.window = std::ptr::null_mut();
        self.view = std::ptr::null_mut();
        self.stroke = std::ptr::null_mut();
        self.card = std::ptr::null_mut();
        self.glyph = std::ptr::null_mut();
        self.hint = std::ptr::null_mut();
        self.shown = false;
    }

    fn hide(&mut self) {
        if self.window.is_null() || !self.shown {
            return;
        }
        unsafe {
            let _: () = msg_send![self.window, orderOut: std::ptr::null_mut::<AnyObject>()];
        }
        self.shown = false;
    }
}
impl TrailWindow {
    fn draw(&mut self, trail: &Trail) {
        // The trail starts where the gesture engaged; the press origin is only the recognizer's
        // anchor, and drawing from it would show a segment the user never saw us take.
        if !trail.visible || trail.points.is_empty() {
            self.hide(trail.session);
            return;
        }
        // The trail and the hint are the user's own switches: with the trail off the window stays
        // hidden, and the request is still recorded as handled.
        let settings = crate::config::CONFIG.read().ok();
        let show_trail = settings
            .as_ref()
            .map(|cfg| cfg.mouse.gesture_show_trail)
            .unwrap_or(true);
        if !show_trail {
            self.hide(trail.session);
            crate::mouse::gesture::session_state::note_hud_request(trail.session);
            return;
        }
        let hint = settings
            .as_ref()
            .map(|cfg| cfg.mouse.gesture_show_hint)
            .unwrap_or(true);
        let hint = hint.then(crate::mouse::gesture::session_state::hint_card);
        self.ensure_window().show();
        self.ensure_window().draw_stroke(&trail.points, hint);
        DRAWN_SESSION.store(trail.session, Ordering::SeqCst);
        crate::e2e_state::record_gesture_trail(trail.session, trail.points.len());
        crate::mouse::gesture::session_state::set_hud_visible(true);
        crate::mouse::gesture::session_state::note_hud_request(trail.session);
    }

    /// The trail view, created on first use: building a window during a drag would cost a frame.
    fn ensure_window(&mut self) -> &mut TrailView {
        self.view.get_or_insert_with(TrailView::default)
    }

    /// Hide when this window is still drawing \`session\`. A stale hide (an older session) leaves a
    /// newer frame alone.
    fn hide(&mut self, session: u64) {
        let drawn = DRAWN_SESSION.load(Ordering::SeqCst);
        if drawn != 0 && drawn != session {
            return;
        }
        if let Some(view) = self.view.as_mut() {
            view.hide();
        }
        DRAWN_SESSION.store(0, Ordering::SeqCst);
        crate::e2e_state::record_gesture_trail(0, 0);
        crate::mouse::gesture::session_state::set_hud_visible(false);
    }
}
