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
use std::sync::OnceLock;

use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use std::ffi::c_void;

// The framework's own attribute names: AppKit resolves these by identity, not by spelling.
#[link(name = "AppKit", kind = "framework")]
extern "C" {
    static NSFontAttributeName: *mut AnyObject;
    static NSParagraphStyleAttributeName: *mut AnyObject;
    static NSForegroundColorAttributeName: *mut AnyObject;
}

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
    /// The hint card, its glyph view and its text label.
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HintText {
    pub(crate) direction: Option<char>,
    pub(crate) text: String,
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
        // The hint card: a dark plate with a large arrow standing for the stroke's direction and
        // the action it would run below it.
        //
        // Both children are DRAWN, not laid out by AppKit: an SF Symbol image view and a text field
        // each position their own content inside their frame, and neither honoured the box this code
        // gave it (the arrow stayed small inside a large frame, the label sat off its centre). The
        // card owns the backing layer and paints the arrow and the text itself, so what the numbers
        // say is what the pixels are.
        let card: *mut AnyObject = msg_send![gesture_card_class(), alloc];
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
            let _: () = msg_send![card_layer, setMasksToBounds: true];
            // A layer-backed view whose layer has a background paints BOTH the layer fill and this
            // drawRect: over it; the content must be opaque enough to sit on that fill, which it is.
            let _: () = msg_send![card, setLayerContentsRedrawPolicy: 2isize]; // on setNeedsDisplay
        }
        let _: () = msg_send![card, setHidden: true];
        let _: () = msg_send![view, addSubview: card];
        self.card = card;
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
            let pad = crate::theme::GESTURE_HINT_PADDING;
            let line = crate::theme::GESTURE_HINT_LINE;
            let glyph = crate::theme::GESTURE_HINT_GLYPH_SIZE;
            // Width follows the text, measured with the font the card draws it with.
            let text_w = measure_hint_text(&hint.text);
            let w = (text_w + pad * 2.0).clamp(
                crate::theme::GESTURE_HINT_MIN_WIDTH,
                crate::theme::GESTURE_HINT_MAX_WIDTH,
            );
            let content = glyph + crate::theme::GESTURE_HINT_GAP_INNER + line;
            let h = (content + pad * 2.0).max(crate::theme::GESTURE_HINT_HEIGHT);
            // The card sits at the CENTRE of the screen the gesture is drawn on, not under the
            // pointer: it is a status readout, and one that follows the stroke is both distracting
            // and hard to read while the hand is moving.
            let x = ((bounds.size.width - w) / 2.0).max(0.0);
            let y = ((bounds.size.height - h) / 2.0).max(0.0);
            let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));
            let _: () = msg_send![self.card, setFrame: frame];
            // The painted content is stored on the card and used by its drawRect:.
            let direction = hint.direction.unwrap_or('R');
            set_card_content(self.card, &hint.text, direction);
            let _: () = msg_send![self.card, setNeedsDisplay: true];
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
// ---- The hint card's painted content -------------------------------------------------------
//
// An SF Symbol image view and an NSTextField each decide for themselves where their content goes
// inside the frame they are given: the arrow kept its natural size inside a large box, and the
// label did not sit where its frame said. The card therefore paints both itself in drawRect:, from
// the numbers the layout computed, so the plate, the arrow and the text cannot disagree.

/// The arrow's proportions, as fractions of its width.
const ARROW_HEAD_LENGTH: f64 = 0.34;
const ARROW_HEAD_HEIGHT: f64 = 0.80;
const ARROW_SHAFT: f64 = 0.14;

static CARD_TEXT: OnceLock<std::sync::Mutex<String>> = OnceLock::new();
static CARD_DIRECTION: OnceLock<std::sync::Mutex<char>> = OnceLock::new();

fn card_text() -> &'static std::sync::Mutex<String> {
    CARD_TEXT.get_or_init(|| std::sync::Mutex::new(String::new()))
}

fn card_direction() -> &'static std::sync::Mutex<char> {
    CARD_DIRECTION.get_or_init(|| std::sync::Mutex::new('R'))
}

static GESTURE_CARD_CLASS: OnceLock<crate::ffi::StaticClass> = OnceLock::new();

fn gesture_card_class() -> *mut AnyObject {
    crate::ffi::StaticClass::register_once(&GESTURE_CARD_CLASS, c"OhMyTabGestureHintCard", || {
        crate::ffi::build_class(
            c"OhMyTabGestureHintCard",
            class!(NSView),
            |builder| unsafe {
                builder.add_method::<AnyObject, _>(
                    sel!(drawRect:),
                    gesture_card_draw as unsafe extern "C" fn(*mut AnyObject, Sel, NSRect),
                );
            },
        )
    })
    .0 as *mut AnyObject
}

/// Store what the card paints on its next redraw.
unsafe fn set_card_content(card: *mut AnyObject, text: &str, direction: char) {
    let _ = card;
    if let Ok(mut slot) = card_text().lock() {
        *slot = text.to_string();
    }
    if let Ok(mut slot) = card_direction().lock() {
        *slot = direction;
    }
}

/// The seven corners of an arrow of `size`, pointing along `(dx, dy)`, centred on the origin.
fn arrow_polygon(size: f64, dx: f64, dy: f64) -> [(f64, f64); 7] {
    let half = size / 2.0;
    let head = size * ARROW_HEAD_LENGTH;
    let barb = size * ARROW_HEAD_HEIGHT / 2.0;
    let shaft = size * ARROW_SHAFT / 2.0;
    // `along` runs from the tail to the tip, `across` is perpendicular to it.
    let point = |along: f64, across: f64| (dx * along - dy * across, dy * along + dx * across);
    [
        point(half, 0.0),
        point(half - head, barb),
        point(half - head, shaft),
        point(-half, shaft),
        point(-half, -shaft),
        point(half - head, -shaft),
        point(half - head, -barb),
    ]
}

/// The width a single line of the hint text needs, in the font the card draws it with.
unsafe fn measure_hint_text(text: &str) -> f64 {
    if text.is_empty() {
        return 0.0;
    }
    let attributes = hint_text_attributes();
    let title = make_nsstring(text);
    let attributed: *mut AnyObject = msg_send![class!(NSAttributedString), alloc];
    let attributed: *mut AnyObject =
        msg_send![attributed, initWithString: title, attributes: attributes];
    CFRelease(title as *const c_void);
    let size: NSSize = msg_send![attributed, size];
    crate::release_obj(attributed);
    if size.width.is_finite() {
        size.width.ceil()
    } else {
        0.0
    }
}

/// The attributes the label is drawn with: the card's font, ink and a centred paragraph.
///
/// The keys are the framework's own attribute NAMES (the extern statics AppKit exports), picked
/// through their addresses. Building a dictionary out of made-up string keys crashed the process
/// inside the text layout: AppKit looks the keys up by identity, found an unknown one, and
/// dereferenced it.
unsafe fn hint_text_attributes() -> *mut AnyObject {
    let font: *mut AnyObject = msg_send![
        class!(NSFont),
        systemFontOfSize: crate::theme::GESTURE_HINT_FONT
    ];
    let style: *mut AnyObject = msg_send![class!(NSMutableParagraphStyle), alloc];
    let style: *mut AnyObject = msg_send![style, init];
    let _: () = msg_send![style, setAlignment: 2isize]; // NSTextAlignmentCenter
    let ink = crate::ffi::hex_to_ns_color(crate::theme::GESTURE_HINT_INK);
    let attributes: *mut AnyObject = msg_send![class!(NSMutableDictionary), alloc];
    let attributes: *mut AnyObject = msg_send![attributes, init];
    let _: () = msg_send![attributes, setObject: font, forKey: NSFontAttributeName];
    let _: () = msg_send![attributes, setObject: style, forKey: NSParagraphStyleAttributeName];
    let _: () = msg_send![attributes, setObject: ink, forKey: NSForegroundColorAttributeName];
    let attributes: *mut AnyObject = msg_send![attributes, autorelease];
    let _ = ink;
    attributes
}

/// Paint the card: its arrow, centred, then its label in the band under it.
unsafe extern "C" fn gesture_card_draw(this: *mut AnyObject, _cmd: Sel, dirty: NSRect) {
    crate::callback_guard::void("gesture_card_draw", || unsafe {
        let bounds: NSRect = msg_send![this, bounds];
        // drawRect: is clipped to its dirty rect, and a drawing that ignores it is drawn once and
        // then clipped away on the next partial redraw. Pain in the view's own coordinates and stop
        // when the dirty rect does not touch the card.
        let paint = intersect_rects(bounds, dirty);
        if paint.size.width < 1.0 || paint.size.height < 1.0 {
            return;
        }
        let w = bounds.size.width.max(1.0);
        let h = bounds.size.height.max(1.0);
        let direction = card_direction().lock().map(|d| *d).unwrap_or('R');
        let text = card_text().lock().map(|t| t.clone()).unwrap_or_default();
        let pad = crate::theme::GESTURE_HINT_PADDING;
        let glyph = crate::theme::GESTURE_HINT_GLYPH_SIZE.min(w - pad * 2.0);
        let line = crate::theme::GESTURE_HINT_LINE;
        let content = glyph + crate::theme::GESTURE_HINT_GAP_INNER + line;
        let top = ((h - content) / 2.0).max(0.0);
        let (dx, dy) = match direction {
            'L' => (-1.0f64, 0.0f64),
            'U' => (0.0, 1.0),
            'D' => (0.0, -1.0),
            _ => (1.0, 0.0),
        };
        // The arrow is a path, not a glyph: its geometry is exact, so its centre is the plate's
        // centre and its size is the size asked for.
        let cx = w / 2.0;
        let cy = h - top - glyph / 2.0;
        let ink = crate::ffi::hex_to_ns_color(crate::theme::GESTURE_HINT_INK);
        let path = CGPathCreateMutable();
        for (index, (px, py)) in arrow_polygon(glyph, dx, dy).iter().enumerate() {
            if index == 0 {
                CGPathMoveToPoint(path, std::ptr::null(), cx + px, cy + py);
            } else {
                CGPathAddLineToPoint(path, std::ptr::null(), cx + px, cy + py);
            }
        }
        crate::ffi::CGPathCloseSubpath(path);
        let _: () = msg_send![ink, setFill];
        let path_obj = path as *mut AnyObject;
        let _: () = msg_send![path_obj, fill];
        CFRelease(path as *const c_void);
        crate::release_obj(ink);
        // The label, in the band below the arrow, centred on the plate.
        if !text.is_empty() {
            let attributes = hint_text_attributes();
            let title = make_nsstring(&text);
            let attributed: *mut AnyObject = msg_send![class!(NSAttributedString), alloc];
            let attributed: *mut AnyObject =
                msg_send![attributed, initWithString: title, attributes: attributes];
            CFRelease(title as *const c_void);
            let text_w = w - pad * 2.0;
            let measured: NSSize = msg_send![attributed, size];
            let draw_h = measured.height.max(line);
            let rect = NSRect::new(
                NSPoint::new(pad, top + (line - draw_h) / 2.0),
                NSSize::new(text_w, draw_h),
            );
            let _: () = msg_send![attributed, drawInRect: rect];
            crate::release_obj(attributed);
        }
    });
}

/// The overlap of two rectangles; empty when they do not meet.
fn intersect_rects(a: NSRect, b: NSRect) -> NSRect {
    let left = a.origin.x.max(b.origin.x);
    let right = (a.origin.x + a.size.width).min(b.origin.x + b.size.width);
    let bottom = a.origin.y.max(b.origin.y);
    let top = (a.origin.y + a.size.height).min(b.origin.y + b.size.height);
    NSRect::new(
        NSPoint::new(left, bottom),
        NSSize::new((right - left).max(0.0), (top - bottom).max(0.0)),
    )
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
