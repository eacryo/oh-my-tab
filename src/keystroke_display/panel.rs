//! Non-activating floating keycap panel and its active-only main-runloop timer.

use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{c_void, CString};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use super::state::{
    estimated_stream_width, repeat_suffix_row, split_chord_badges, Badge, BadgeCell, BadgeKind,
    Orientation, BADGE_GAP, BADGE_H, BADGE_HORIZONTAL_PADDING, BADGE_MIN_WIDTH,
    BADGE_REPEAT_SUFFIX_H, BADGE_SUFFIX_ROW_INSET_Y, KEYCAP_RAIL_W, PANEL_SIDE_PADDING,
};
use crate::config::KeystrokeDisplayPosition;
use crate::event_tap;
use crate::ffi::{
    class_addMethod, hex_to_cg_color, layer_set_background, layer_set_border, make_nsstring,
    objc_allocateClassPair, objc_registerClassPair, release_obj, AXUIElementCopyAttributeValue,
    AXUIElementCopyParameterizedAttributeValue, AXUIElementCreateApplication,
    AXUIElementSetMessagingTimeout, AXValueCreate, AXValueGetType, AXValueGetValue, CFRelease,
    CFStringCreateWithCString, CGRect,
};
use crate::ffi::{MainThreadSlot, StaticClass};

/// Panel extent across the stream: one badge plus this padding on each side (34 + 2*10).
const PANEL_H: f64 = 54.0;
/// Padding on the panel's cross axis, i.e. above and below a row / either side of a column.
const PANEL_CROSS_PADDING: f64 = 10.0;
/// Distance kept from the screen edge the panel is anchored to.
const PANEL_EDGE_MARGIN: f64 = 18.0;
const HIDE_FADE: Duration = Duration::from_millis(
    (crate::theme::ANIMATION_DURATION_MEDIUM * crate::theme::ANIMATION_EXIT_RATIO * 1000.0) as u64,
);
const PANEL_TIMER_INTERVAL: f64 = 0.016;
const MAX_MEASUREMENTS: usize = 512;
const MAX_TEXT_CENTROID_OFFSET: f64 = 2.0;
const BADGE_INLINE_FIELD_SLACK_X: f64 = 8.0;
/// Transparent drag target dimensions along the handle and across it; dots remain centered
/// within this hit area, which is intentionally larger than their visible footprint.
const GRIP_HIT_LENGTH: f64 = 48.0;
const GRIP_HIT_THICKNESS: f64 = 20.0;
/// Inset the vertical-stream handle from the panel's top edge for visible breathing room.
const GRIP_EDGE_INSET: f64 = 4.0;
/// Distance from the panel's anchored edge to the visible dot row's centre. It lines the handle up
/// with the panel's own side padding, so the dots read as an inset control instead of hugging the
/// edge while the reserve keeps the first keycap far away.
const GRIP_MARK_CENTER_INSET: f64 = PANEL_SIDE_PADDING;
const GRIP_MARK_WIDTH: f64 = 3.0;
const GRIP_MARK_HEIGHT: f64 = 3.0;
const GRIP_MARK_GAP: f64 = 3.0;
const DRAG_THRESHOLD: f64 = 4.0;
/// How long a preview may go without a live pointer or a refresh before the watchdog ends it.
/// Every tick refreshes this while the left button is still down, so a user who pauses mid-drag is
/// never cut off; it only fires for a preview whose release was lost.
const PREVIEW_WATCHDOG: Duration = Duration::from_millis(3000);

/// The level the settings window uses for its own dropdown panels (NSFloatingWindowLevel, 3). It is
/// also the level the panel itself ships at, which is why the main settings window -- a normal
/// window -- never covers it. A preview compares against this to decide whether lifting is needed at
/// all, rather than raising unconditionally and restoring a level it never measured.
const SETTINGS_WINDOW_LEVEL: isize = 3;

#[link(name = "AppKit", kind = "framework")]
extern "C" {
    static NSFontAttributeName: *mut AnyObject;
}

#[derive(Default)]
struct PanelState {
    panel: Option<*mut AnyObject>,
    timer: Option<event_tap::CFRunLoopTimerRef>,
    visible: bool,
    fade_deadline: Option<Instant>,
    target_frame: Option<ScreenGeometry>,
    pending_target_frame: Option<ScreenGeometry>,
    badge_container: Option<*mut AnyObject>,
    grip_view: Option<*mut AnyObject>,
    grip_cursor: GripCursor,
    drag: Option<GripDrag>,
    last_badges: Vec<Badge>,
    last_palette: Option<crate::theme::UiPalette>,
    /// The layout the badge views were last built for. A size change alone (the cap preview resizing
    /// the panel from the settings slider) moves every keycap, so the badges alone are not enough to
    /// decide a rebuild: identical content at a different size still has to be laid out again.
    last_layout: Option<(NSSize, Orientation, usize)>,
    measurements: HashMap<String, f64>,
    /// Layout direction of the live panel, refreshed on every render. The grip and hit-testing
    /// paths run outside `render` and need it to place the handle on the correct edge.
    orientation: Orientation,
    /// Set once the bar has hit the width cap this session; it stays there until the panel hides,
    /// so keys rolling off the front never make the length breathe.
    latched: bool,
    /// A column's latched width (0 while unset): the panel only grows while shown and never
    /// shrinks, mirroring `latched` on the cross axis. A row resets it.
    panel_cross_latched: f64,
    /// The trailing settings button, created with the panel and laid out on every render.
    settings_view: Option<*mut AnyObject>,
    /// Whether a press on the settings button is in flight. Keeps the window receiving input until
    /// the press finishes or is cancelled, like `drag` does for the handle.
    settings_armed: bool,
    /// While the settings cap slider is being dragged: the percentage to preview. The panel shows
    /// virtual keycaps at this size and ignores the real event stream entirely -- nothing from the
    /// preview reaches the state machine, the latches, or the saved position.
    preview_percent: Option<u32>,
    /// The window level to restore when the preview ends. Only ever written together with
    /// `preview_percent`, so the two cannot disagree about whether a level was raised.
    preview_saved_level: Option<isize>,
    /// The wall-clock deadline a preview must not outlive. Set when the preview starts and
    /// refreshed by every step: a live drag keeps extending it, while a policy watchdog ends a
    /// preview whose release never arrived (see `max_extent_preview_expired`).
    preview_deadline: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ScreenGeometry {
    frame: NSRect,
    visible_frame: NSRect,
}

impl Default for ScreenGeometry {
    fn default() -> Self {
        let bounds = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0));
        Self {
            frame: bounds,
            visible_frame: bounds,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum GripCursor {
    #[default]
    None,
    Open,
    Closed,
    /// The pointer is over the settings button, which is a control, not a drag handle.
    Pointing,
}

#[derive(Clone, Copy, Debug)]
struct GripDrag {
    mouse_down: NSPoint,
    initial_frame: NSRect,
    moved: bool,
}

thread_local! {
    static PANEL: RefCell<PanelState> = RefCell::new(PanelState::default());
}

static PANEL_BACKDROP: MainThreadSlot<Option<crate::glass::InstalledBackdrop>> =
    MainThreadSlot::new(None);

unsafe extern "C" fn timer_callback(_timer: event_tap::CFRunLoopTimerRef, _info: *mut c_void) {
    crate::callback_guard::void("keystroke_display_timer", super::timer_fired);
}

pub(super) fn start_timer() {
    crate::debug_assert_main_thread();
    PANEL.with(|panel| {
        let mut panel = panel.borrow_mut();
        if panel.timer.is_some() {
            return;
        }
        unsafe {
            let context = event_tap::CFRunLoopTimerContext {
                version: 0,
                info: std::ptr::null_mut(),
                retain: None,
                release: None,
                copy_description: None,
            };
            let timer = event_tap::CFRunLoopTimerCreate(
                std::ptr::null_mut(),
                0.0,
                PANEL_TIMER_INTERVAL,
                0,
                0,
                Some(timer_callback),
                &context as *const event_tap::CFRunLoopTimerContext as *mut c_void,
            );
            if timer.is_null() {
                crate::log_info!("[keystroke-display] failed to create the display timer.");
                return;
            }
            event_tap::CFRunLoopAddTimer(
                event_tap::CFRunLoopGetMain(),
                timer,
                event_tap::kCFRunLoopDefaultMode,
            );
            panel.timer = Some(timer);
        }
    });
}

pub(super) fn stop_timer() {
    crate::debug_assert_main_thread();
    PANEL.with(|panel| {
        if let Some(timer) = panel.borrow_mut().timer.take() {
            unsafe {
                event_tap::CFRunLoopTimerInvalidate(timer);
                crate::ffi::CFRelease(timer as *const c_void);
            }
        }
    });
}

pub(super) fn fade_pending() -> bool {
    crate::debug_assert_main_thread();
    PANEL.with(|panel| panel.borrow().fade_deadline.is_some())
}

/// The smallest stream-axis cap the panel may be given, in points. Below this the panel's own
/// controls (the leading drag handle and the trailing settings button) plus one minimum-width
/// keycap no longer fit: a 32pt keycap needs 32 + 24 of nominal panel padding +
/// STREAM_BUDGET_DEDUCTION (56) = 112pt of cap. A percentage that would ask for less is clamped
/// here, so a very small screen cannot produce a panel whose keycap sits under a control.
const MIN_PANEL_STREAM_EXTENT: f64 = 112.0;

/// The panel's stream-axis cap in points: the configured percentage of the target screen's
/// extent along that axis. Both the renderer and the keycap state machine call this, so the two
/// cannot disagree -- the state keeps exactly what the renderer can lay out.
///
/// The measured extent is the screen's full frame, not its visible area, so the default 50% keeps
/// the exact meaning it had before this was configurable. At 100% the window can be taller than
/// the visible area; placement still clamps the origin back on screen.
pub(super) fn max_extent_for_percent(screen_extent: f64, percent: u32) -> f64 {
    let percent = percent.clamp(
        crate::config::KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_MIN,
        crate::config::KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_MAX,
    ) as f64
        / 100.0;
    (screen_extent.max(1.0) * percent).max(MIN_PANEL_STREAM_EXTENT)
}

/// The stream-axis budget the keycap state machine may fill: the panel's cap, minus what the
/// panel's own controls reserve. The state machine trims to this BEFORE render runs, so both
/// layers must use the identical number -- otherwise the state keeps a keycap the renderer must
/// drop, or drops one the renderer had room for.
pub(super) fn stream_budget_for_panel(
    display_position: &str,
    position: Option<KeystrokeDisplayPosition>,
    orientation: Orientation,
    max_extent_percent: u32,
) -> f64 {
    (max_extent_for_percent(
        target_screen_extent(display_position, position, orientation),
        max_extent_percent,
    ) - STREAM_BUDGET_DEDUCTION)
        .max(32.0)
}

pub(super) fn target_screen_extent(
    display_position: &str,
    position: Option<KeystrokeDisplayPosition>,
    orientation: Orientation,
) -> f64 {
    crate::debug_assert_main_thread();
    let screens = unsafe { screen_geometries() };
    let current = PANEL.with(|panel| {
        let state = panel.borrow();
        (state.visible || state.drag.is_some())
            .then_some(state.target_frame)
            .flatten()
    });
    let selected = current
        .and_then(|frame| screens.iter().position(|screen| *screen == frame))
        .or_else(|| screen_for_saved_origin(position, &screens))
        .unwrap_or_else(|| target_screen_index_live(display_position, &screens));
    if let Some(screen) = screens.get(selected).copied() {
        PANEL.with(|panel| {
            let mut state = panel.borrow_mut();
            if !state.visible && state.drag.is_none() {
                state.pending_target_frame = Some(screen);
            }
        });
    }
    screens.get(selected).map_or(1.0, |screen| {
        if orientation.is_vertical() {
            screen.frame.size.height.max(1.0)
        } else {
            screen.frame.size.width.max(1.0)
        }
    })
}

pub(super) fn drag_active() -> bool {
    crate::debug_assert_main_thread();
    PANEL.with(|panel| panel.borrow().drag.is_some())
}

pub(super) fn cursor_inside_visible_panel(point: NSPoint) -> Option<bool> {
    crate::debug_assert_main_thread();
    PANEL.with(|panel| {
        let state = panel.borrow();
        if !state.visible {
            return None;
        }
        let window = state.panel?;
        let visible: bool = unsafe { msg_send![window, isVisible] };
        if !visible {
            return None;
        }
        let frame: NSRect = unsafe { crate::glass::panel_frame_of(window) };
        Some(contains(frame, point))
    })
}

unsafe fn grip_view_class() -> *mut AnyObject {
    static GRIP_VIEW_CLASS: OnceLock<StaticClass> = OnceLock::new();
    GRIP_VIEW_CLASS
        .get_or_init(|| {
            let name = CString::new("OhMyTabKeystrokeDisplayGripView").unwrap();
            let superclass = class!(NSView) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            if cls.is_null() {
                return StaticClass(class!(NSView));
            }
            let types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(mouseDown:),
                grip_mouse_down as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseDragged:),
                grip_mouse_dragged as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseUp:),
                grip_mouse_up as *mut c_void,
                types.as_ptr(),
            );
            objc_registerClassPair(cls);
            StaticClass(cls as *const objc2::runtime::AnyClass)
        })
        .0 as *mut AnyObject
}

extern "C" fn grip_mouse_down(_view: *mut c_void, _cmd: Sel, event: *mut c_void) {
    crate::callback_guard::void("keystroke_display_grip_mouse_down", || unsafe {
        let click_count: isize = msg_send![event as *mut AnyObject, clickCount];
        if let Some(point) = current_cursor_appkit_point() {
            begin_grip_interaction(point, click_count, Instant::now());
        }
    });
}

extern "C" fn grip_mouse_dragged(_view: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    crate::callback_guard::void("keystroke_display_grip_mouse_dragged", || {
        if let Some(point) = current_cursor_appkit_point() {
            drag_grip_to(point, Instant::now());
        }
    });
}

extern "C" fn grip_mouse_up(_view: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    crate::callback_guard::void("keystroke_display_grip_mouse_up", finish_grip_interaction);
}

fn begin_grip_interaction(point: NSPoint, click_count: isize, now: Instant) {
    crate::debug_assert_main_thread();
    let reset = PANEL.with(|panel| {
        let mut state = panel.borrow_mut();
        let Some(window) = state.panel else {
            return false;
        };
        let frame: NSRect = unsafe { crate::glass::panel_frame_of(window) };
        let local_grip = grip_frame(state.orientation, frame.size);
        let global_grip = NSRect::new(
            NSPoint::new(
                frame.origin.x + local_grip.origin.x,
                frame.origin.y + local_grip.origin.y,
            ),
            local_grip.size,
        );
        if !contains(global_grip, point) {
            return false;
        }
        state.fade_deadline = None;
        state.visible = true;
        unsafe {
            set_alpha_immediately(window, 1.0);
            let _: () = msg_send![window, setIgnoresMouseEvents: false];
        }
        if click_count >= 2 {
            state.drag = None;
            set_grip_cursor(&mut state, GripCursor::None);
            true
        } else {
            state.drag = Some(GripDrag {
                mouse_down: point,
                initial_frame: frame,
                moved: false,
            });
            set_grip_cursor(&mut state, GripCursor::Closed);
            super::note_panel_activity(now);
            false
        }
    });
    if reset {
        update_config_position(None);
        reposition_to_default();
    }
}

fn drag_grip_to(point: NSPoint, now: Instant) {
    crate::debug_assert_main_thread();
    super::note_panel_activity(now);
    PANEL.with(|panel| {
        let mut state = panel.borrow_mut();
        let Some(mut drag) = state.drag else {
            return;
        };
        let dx = point.x - drag.mouse_down.x;
        let dy = point.y - drag.mouse_down.y;
        if !drag.moved && dx.hypot(dy) < DRAG_THRESHOLD {
            return;
        }
        drag.moved = true;
        state.drag = Some(drag);
        if let Some(window) = state.panel {
            let frame = NSRect::new(
                NSPoint::new(
                    drag.initial_frame.origin.x + dx,
                    drag.initial_frame.origin.y + dy,
                ),
                drag.initial_frame.size,
            );
            unsafe {
                // The drag is in panel coordinates (the handle the user grabs is on the panel), so the same
                // rect both starts and lands the drag whatever padding the window carries.
                crate::glass::set_panel_frame(window, frame, true);
            }
        }
    });
}

fn finish_grip_interaction() {
    crate::debug_assert_main_thread();
    let position = PANEL.with(|panel| {
        let mut state = panel.borrow_mut();
        let drag = state.drag.take();
        let position = if drag.is_some_and(|drag| drag.moved) {
            state.panel.map(|window| unsafe {
                let frame: NSRect = crate::glass::panel_frame_of(window);
                KeystrokeDisplayPosition {
                    x: frame.origin.x,
                    y: frame.origin.y,
                }
            })
        } else {
            None
        };
        set_grip_mouse_state(&mut state, false, GripCursor::None);
        position
    });
    if let Some(position) = position {
        update_config_position(Some(position));
        let config = crate::config::CONFIG
            .read()
            .unwrap()
            .keystroke_display
            .clone();
        let screens = unsafe { screen_geometries() };
        let selected = screen_for_saved_origin(Some(position), &screens)
            .unwrap_or_else(|| target_screen_index_live(&config.display_position, &screens));
        if let Some(screen) = screens.get(selected).copied() {
            PANEL.with(|panel| panel.borrow_mut().target_frame = Some(screen));
        }
    }
}

fn update_config_position(position: Option<KeystrokeDisplayPosition>) {
    let updated = if let Ok(mut config) = crate::config::CONFIG.write() {
        config.keystroke_display.position = position;
        true
    } else {
        false
    };
    if updated {
        crate::config::schedule_config_persist();
    }
}

fn reposition_to_default() {
    let config = crate::config::CONFIG
        .read()
        .unwrap()
        .keystroke_display
        .clone();
    let screens = unsafe { screen_geometries() };
    if screens.is_empty() {
        return;
    }
    let target = target_screen_index_live(&config.display_position, &screens);
    let screen = screens[target];
    PANEL.with(|panel| {
        let mut state = panel.borrow_mut();
        state.target_frame = Some(screen);
        if let Some(window) = state.panel {
            unsafe {
                let frame: NSRect = crate::glass::panel_frame_of(window);
                let default_frame =
                    default_edge_frame(screen.visible_frame, frame.size, &config.initial_position);
                // Panel coordinates in, panel coordinates out: the padding belongs to the window, while
                // the HUD's placement is the user's.
                crate::glass::set_panel_frame(window, default_frame, true);
            }
        }
    });
}

/// Where the panel is placed and how its keys are laid out, resolved from the keystroke-display
/// config once per call.
#[derive(Clone, Copy, Debug)]
pub(super) struct PanelPlacement<'a> {
    /// Which screen: `main` or `caret`.
    pub(super) display_position: &'a str,
    /// Which edge of it: `top`, `bottom`, `left`, `right`.
    pub(super) initial_position: &'a str,
    /// A remembered drag origin, which outranks the edge until the setting changes.
    pub(super) position: Option<KeystrokeDisplayPosition>,
    /// The stream-axis cap as a percentage of the target screen (see max_extent_for_percent).
    pub(super) max_extent_percent: u32,
}

/// The virtual keycap stream a size preview draws, in the order it rolls in. One keycap per
/// character, cycling "Oh-My-Tab", so the stream always fills the panel and reads unmistakably as
/// a preview rather than the user's real input.
const PREVIEW_PHRASE: &str = "Oh-My-Tab";

/// How many preview keycaps the virtual stream cycles through at most. This bounds the content
/// while still over-filling the largest possible cap (a 5K screen at 100%, where even the widest
/// cell is far under this many), so the stream always reaches the cap and the trim path is real.
const PREVIEW_MAX_BADGES: usize = 200;

/// Build the virtual stream shown while the cap slider is dragged. Lone keycaps, so both the row
/// and the column paths size them exactly as real content.
fn preview_badges() -> Vec<Badge> {
    PREVIEW_PHRASE
        .chars()
        .cycle()
        .take(PREVIEW_MAX_BADGES)
        .map(|glyph| Badge {
            text: glyph.to_string(),
            kind: BadgeKind::Chord,
            repeats: 1,
            cells: Vec::new(),
        })
        .collect()
}

pub(super) fn render(
    badges: &[Badge],
    visible: bool,
    capped: bool,
    placement: PanelPlacement<'_>,
    now: Instant,
    cursor_point: Option<NSPoint>,
) -> Option<bool> {
    crate::debug_assert_main_thread();
    let PanelPlacement {
        display_position,
        initial_position,
        position,
        max_extent_percent,
    } = placement;
    let orientation = Orientation::from_initial_position(initial_position);
    // A preview outranks the real stream: while the cap slider is being dragged the panel shows
    // virtual keycaps at the previewed size. The substitution happens here, before anything is
    // measured, so the preview goes through exactly the sizing and trimming path real content
    // does -- and the real badges never touch this call.
    let preview = PANEL.with(|panel| panel.borrow().preview_percent);
    let preview_content = preview.map(|_| preview_badges());
    let badges: &[Badge] = preview_content.as_deref().unwrap_or(badges);
    let visible = visible || preview.is_some();
    if visible {
        // The panel window persists across shows: sync the backdrop material first (here,
        // outside the PANEL borrow below). The check is a cheap enum compare unless the
        // material actually changed.
        unsafe { apply_backdrop_material() };
    }
    PANEL.with(|panel| {
        let mut state = panel.borrow_mut();
        if !visible && state.drag.is_some() {
            state.fade_deadline = None;
            state.visible = true;
            if let Some(panel) = state.panel {
                unsafe {
                    set_alpha_immediately(panel, 1.0);
                    let _: () = msg_send![panel, orderFront: std::ptr::null::<AnyObject>()];
                }
            }
            update_grip_tracking(&mut state, cursor_point);
            return None;
        }
        if visible && !badges.is_empty() {
            let reopened = !state.visible;
            if reopened {
                state.latched = false;
                state.panel_cross_latched = 0.0;
            }
            let screens = if reopened {
                unsafe { screen_geometries() }
            } else {
                Vec::new()
            };
            let geometry = if reopened {
                state
                    .pending_target_frame
                    .take()
                    .filter(|pending| screens.contains(pending))
                    .or_else(|| {
                        let screen_index = screen_for_saved_origin(position, &screens)
                            .unwrap_or_else(|| target_screen_index_live(display_position, &screens))
                            .min(screens.len().saturating_sub(1));
                        screens.get(screen_index).copied()
                    })
                    .or(state.target_frame)
                    .unwrap_or_default()
            } else if let Some(target) = state.target_frame {
                target
            } else {
                let screens = unsafe { screen_geometries() };
                let target_index = target_screen_index_live(display_position, &screens);
                let screen_index = screen_for_saved_origin(position, &screens)
                    .unwrap_or(target_index)
                    .min(screens.len().saturating_sub(1));
                screens.get(screen_index).copied().unwrap_or_default()
            };
            let screen_frame = geometry.frame;
            state.target_frame = Some(geometry);
            let panel = if let Some(panel) = state.panel {
                panel
            } else {
                let (panel, badge_container, grip_view, settings_view) =
                    unsafe { create_panel(orientation) };
                state.panel = Some(panel);
                state.badge_container = Some(badge_container);
                state.grip_view = Some(grip_view);
                state.settings_view = Some(settings_view);
                panel
            };
            let badge_container = state
                .badge_container
                .expect("a created keystroke panel has a badge container");
            // Every keycap is drawn independently in either orientation: chords are split into
            // one lone keycap per key (modifiers keep their tint, no grouping tray), so the
            // whole stream is lone keycaps whether it runs as a row or as a column.
            let display = split_chord_badges(badges);
            let labels = badge_labels(&display);
            let mut extents = Vec::with_capacity(display.len());
            for badge in display.iter() {
                extents.push(cached_badge_extent(
                    &mut state.measurements,
                    badge,
                    orientation,
                ));
            }
            // The stream may only grow along its own axis, so the cap follows the panel's edge:
            // the configured percentage of the screen's height for a column, of its width for a
            // row. stream_budget_for_panel measures the same screen with the same percentage. A
            // live preview overrides the configured value: the slider is showing what the panel
            // would look like at the previewed size.
            let max_extent = max_extent_for_percent(
                if orientation.is_vertical() {
                    screen_frame.size.height
                } else {
                    screen_frame.size.width
                },
                preview.unwrap_or(max_extent_percent),
            );
            // The keycap stream shares the panel's stream axis with the two controls (the leading
            // drag handle and the trailing settings button), so their reserve comes off the budget
            // BEFORE the trim decisions below. Every layer that trims (the state machine, this
            // remove loop, the per-badge clamp and the latch) uses this same number: trimming
            // against a larger budget here would let the stream run under the button.
            let stream_budget = (max_extent - STREAM_BUDGET_DEDUCTION).max(32.0);
            let mut desired = estimated_stream_width(extents.iter().map(|extent| extent.length));
            let mut first = 0usize;
            while extents.len() > 1 && desired > stream_budget {
                extents.remove(0);
                first += 1;
                desired = estimated_stream_width(extents.iter().map(|extent| extent.length));
            }
            // Only this final per-badge clamp can ellipsize a genuinely oversized badge. It uses the
            // CONTENT budget: estimated_stream_width() already adds the panel's side padding, so a
            // badge allowed to fill stream_budget would overflow once that padding is added back.
            let content_budget = (stream_budget - PANEL_SIDE_PADDING * 2.0).max(32.0);
            let length_limit = content_budget;
            // A column is only as wide as its widest keycap, itself capped so one long key name
            // cannot turn the strip into a slab.
            let thickness_limit = if orientation.is_vertical() {
                (screen_frame.size.width * 0.5 - PANEL_CROSS_PADDING * 2.0).max(32.0)
            } else {
                BADGE_H
            };
            for extent in &mut extents {
                extent.length = extent.length.min(length_limit);
                extent.thickness = extent.thickness.min(thickness_limit);
            }
            // A column renders EVERY keycap at one fixed width -- lone keycaps and split
            // chord cells alike -- so the rail (and the panel behind it) never changes as
            // keys come and go. The display list is already all lone keycaps here, so the
            // pin needs no badge pairing at all. A row keeps content-sized keycaps.
            if orientation.is_vertical() {
                for extent in &mut extents {
                    extent.thickness = KEYCAP_RAIL_W;
                }
            }
            desired = estimated_stream_width(extents.iter().map(|extent| extent.length));
            // The stream filled the cap this session: pin the bar to the cap so keys rolling off
            // the front never shorten it (the length stops changing at the cap). `first > 0`
            // covers the panel's own measured trim, which can fire a little before the state's
            // estimate-based trim does.
            let pinned = capped || state.latched || first > 0;
            // A preview must not mutate the real session's latches: the panel is showing virtual
            // keycaps, so what it learned about them says nothing about the real stream when the
            // user is handed back to it.
            if preview.is_none() {
                state.latched = pinned;
            }
            // The panel's own length covers the keycap stream plus the controls' reserve: sizing it
            // from the stream alone would clip the trailing button once the stream reaches budget.
            let panel_length = if pinned {
                max_extent
            } else {
                content_sized_panel_length(desired)
                    .min(max_extent)
                    .max(max_extent.min(64.0))
            };
            // The cross axis is one badge thick plus padding on both sides. For a row this is
            // BADGE_H + 2*10 = 54, i.e. PANEL_H; a column mirrors it with the badge's own width.
            let panel_cross = extents
                .iter()
                .map(|extent| extent.thickness)
                .fold(BADGE_H, f64::max)
                + PANEL_CROSS_PADDING * 2.0;
            // A column's width only grows: a wider badge widens the panel once for its
            // lifetime, and it never shrinks while shown, so the shared edge and the panel
            // behind it never breathe as keys come and go.
            // Same rule as the stream latch: a preview's cross-axis measurement is about virtual
            // keycaps, so it neither grows nor clears the real column's width.
            let panel_cross = if preview.is_some() {
                panel_cross
            } else if orientation.is_vertical() {
                let latched = state.panel_cross_latched.max(panel_cross);
                state.panel_cross_latched = latched;
                latched
            } else {
                state.panel_cross_latched = 0.0;
                panel_cross
            };
            let size = if orientation.is_vertical() {
                NSSize::new(panel_cross, panel_length)
            } else {
                NSSize::new(panel_length, PANEL_H.max(panel_cross))
            };

            let frame = unsafe {
                if state.drag.is_some() {
                    let current: NSRect = crate::glass::panel_frame_of(panel);
                    NSRect::new(current.origin, size)
                } else if !reopened {
                    let current: NSRect = crate::glass::panel_frame_of(panel);
                    resize_frame_preserving_center(current, size, geometry.visible_frame)
                } else {
                    resolve_panel_frame(
                        position,
                        &[geometry],
                        0,
                        size,
                        orientation,
                        initial_position,
                    )
                    .1
                }
            };
            unsafe {
                if reopened {
                    state.last_badges.clear();
                    state.last_layout = None;
                    set_alpha_immediately(panel, 1.0);
                    let _: () = msg_send![panel, orderFront: std::ptr::null::<AnyObject>()];
                }
                // The window is the padded rect; `frame` here is the panel's. A raw `setFrame:` with the
                // panel rect shrinks the window, which then shrinks the material through its fixed
                // margins -- the keycaps are laid out for `size` and end up overflowing the strip.
                crate::glass::set_panel_frame(panel, frame, true);
                if let Some(grip_view) = state.grip_view {
                    let _: () = msg_send![grip_view, setFrame: grip_frame(orientation, size)];
                    update_grip_marker(grip_view, orientation);
                }
                if let Some(settings_view) = state.settings_view {
                    let _: () = msg_send![
                        settings_view,
                        setFrame: settings_button_frame(orientation, size)
                    ];
                }
                let palette = crate::theme::ui_palette();
                // The size and the visible slice are part of the layout key. A preview resizes the
                // panel while the virtual badges stay identical, so comparing the badges alone left
                // the old keycaps (their positions and their frames) in place while the window grew
                // or shrank around them.
                let layout = (size, orientation, first);
                if state.last_badges != badges
                    || state.last_palette != Some(palette)
                    || state.last_layout != Some(layout)
                {
                    rebuild_badges(
                        badge_container,
                        &display[first..],
                        &labels[first..],
                        &extents,
                        size,
                        orientation,
                        &palette,
                    );
                    state.last_badges = badges.to_vec();
                    state.last_palette = Some(palette);
                    state.last_layout = Some(layout);
                }
                let _ = state.fade_deadline.take();
            }
            state.visible = true;
            state.orientation = orientation;
            state.target_frame = Some(geometry);
            update_grip_tracking(&mut state, cursor_point);
            if reopened {
                cursor_point.map(|point| contains(frame, point))
            } else {
                None
            }
        } else {
            if state.visible {
                state.visible = false;
                state.last_badges.clear();
                state.last_layout = None;
                state.latched = false;
                state.panel_cross_latched = 0.0;
                let reduce_motion = crate::theme::reduce_motion_enabled();
                state.fade_deadline = (!reduce_motion).then_some(now + HIDE_FADE);
                if let Some(panel) = state.panel {
                    unsafe {
                        if reduce_motion {
                            let _: () = msg_send![panel, orderOut: std::ptr::null::<AnyObject>()];
                            let _: () = msg_send![panel, setAlphaValue: 1.0f64];
                        } else {
                            let _: () = msg_send![class!(NSAnimationContext), beginGrouping];
                            let context: *mut AnyObject =
                                msg_send![class!(NSAnimationContext), currentContext];
                            let _: () = msg_send![
                                context,
                                setDuration: crate::theme::animation_exit_duration(
                                    crate::theme::ANIMATION_DURATION_MEDIUM
                                )
                            ];
                            let timing = crate::theme::ease_standard_timing_function();
                            if !timing.is_null() {
                                let _: () = msg_send![context, setTimingFunction: timing];
                            }
                            let animator: *mut AnyObject = msg_send![panel, animator];
                            let _: () = msg_send![animator, setAlphaValue: 0.0f64];
                            let _: () = msg_send![class!(NSAnimationContext), endGrouping];
                        }
                    }
                } else {
                    state.fade_deadline = None;
                }
                if reduce_motion {
                    state.target_frame = None;
                    set_grip_mouse_state(&mut state, false, GripCursor::None);
                }
            } else if state
                .fade_deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                if let Some(panel) = state.panel {
                    unsafe {
                        let _: () = msg_send![panel, orderOut: std::ptr::null::<AnyObject>()];
                        let _: () = msg_send![panel, setAlphaValue: 1.0f64];
                    }
                }
                state.fade_deadline = None;
                state.target_frame = None;
                set_grip_mouse_state(&mut state, false, GripCursor::None);
            } else {
                update_grip_tracking(&mut state, cursor_point);
            }
            None
        }
    })
}

/// Show the panel with virtual keycaps at `percent`, for the settings cap slider drag. Idempotent:
/// the first call shows the panel and raises its window level, later calls only move the size.
/// The real event stream is not consulted, so previewing works even while the display is off, and
/// nothing here writes configuration, the latches, or the saved position.
pub(super) fn begin_max_extent_preview(percent: u32) {
    crate::debug_assert_main_thread();
    let percent = crate::config::keystroke_display_max_extent_percent_snap(percent);
    let first = PANEL.with(|panel| {
        let mut state = panel.borrow_mut();
        let first = state.preview_percent.is_none();
        state.preview_percent = Some(percent);
        // Refreshed on every step of a live drag; a stall past this ends the preview instead of
        // leaving the panel up at a raised level.
        state.preview_deadline = Some(Instant::now() + PREVIEW_WATCHDOG);
        first
    });
    if first {
        // The preview must not disturb the real session: it is not "visible" in the state
        // machine sense, so an active session keeps its badges and its own visibility.
        start_timer();
        raise_preview_window_level();
    }
    // Render immediately so the first frame already shows the previewed size; the timer keeps it
    // settled while the drag continues.
    render_preview_tick();
}

/// One preview frame. Shared by the first frame and by every timer tick while the preview runs, so
/// the panel keeps re-laying out from the current percentage even if no action arrives for a while.
/// The badges argument is ignored while previewing, so an empty slice is honest: render substitutes
/// the virtual stream itself.
pub(super) fn render_preview_tick() {
    crate::debug_assert_main_thread();
    if !max_extent_preview_active() {
        return;
    }
    let config = crate::config::CONFIG.read().unwrap();
    let placement = PanelPlacement {
        display_position: &config.keystroke_display.display_position,
        initial_position: &config.keystroke_display.initial_position,
        position: config.keystroke_display.position,
        max_extent_percent: config.keystroke_display.max_extent_percent,
    };
    let _ = render(
        &[],
        true,
        false,
        placement,
        Instant::now(),
        current_cursor_appkit_point(),
    );
}

/// Stop the size preview, restoring the window level and letting the real stream decide what the
/// panel shows next. Safe to call when no preview is running.
pub(super) fn end_max_extent_preview() {
    crate::debug_assert_main_thread();
    let was_previewing = PANEL.with(|panel| {
        let mut state = panel.borrow_mut();
        let was = state.preview_percent.take().is_some();
        state.preview_deadline = None;
        if was {
            state.last_badges.clear();
            state.last_layout = None;
        }
        was
    });
    if !was_previewing {
        return;
    }
    restore_preview_window_level();
    // Hand the panel back to the real state machine on the next tick: it re-renders from the
    // live badges, or fades the panel out when there are none.
    super::wake();
}

/// Refresh a running preview's watchdog. Called once per tick while the pointer is still down, so
/// a stationary drag is not mistaken for an abandoned one.
pub(super) fn refresh_max_extent_preview_deadline() {
    crate::debug_assert_main_thread();
    PANEL.with(|panel| {
        let mut state = panel.borrow_mut();
        if state.preview_percent.is_some() {
            state.preview_deadline = Some(Instant::now() + PREVIEW_WATCHDOG);
        }
    });
}

/// Whether a running preview has outlived its watchdog (its release never arrived).
pub(super) fn max_extent_preview_expired() -> bool {
    crate::debug_assert_main_thread();
    PANEL.with(|panel| {
        let state = panel.borrow();
        match (state.preview_percent, state.preview_deadline) {
            (Some(_), Some(deadline)) => Instant::now() >= deadline,
            (Some(_), None) => true,
            _ => false,
        }
    })
}

/// Whether a size preview is running (assertable state for the smoke runner).
pub(super) fn max_extent_preview_active() -> bool {
    crate::debug_assert_main_thread();
    PANEL.with(|panel| panel.borrow().preview_percent.is_some())
}

/// The percentage currently previewed, if any.
pub(super) fn max_extent_preview_percent() -> Option<u32> {
    crate::debug_assert_main_thread();
    PANEL.with(|panel| panel.borrow().preview_percent)
}

/// Read the panel window's current level, if the window exists.
fn panel_window_level() -> Option<isize> {
    PANEL
        .with(|panel| panel.borrow().panel)
        .map(|panel| unsafe { msg_send![panel, level] })
}

/// Lift the panel above the settings window while a preview runs. The settings window is a
/// normal window, so floating level already keeps the panel on top; only lift when the measured
/// level says otherwise, and remember the old value so the restore is exact.
fn raise_preview_window_level() {
    let Some(current) = panel_window_level() else {
        return;
    };
    if current > SETTINGS_WINDOW_LEVEL {
        return;
    }
    let Some(panel) = PANEL.with(|panel| panel.borrow().panel) else {
        return;
    };
    PANEL.with(|state| state.borrow_mut().preview_saved_level = Some(current));
    unsafe {
        let _: () = msg_send![panel, setLevel: SETTINGS_WINDOW_LEVEL + 1];
    }
}

/// Put the panel window back at the level it had before the preview.
fn restore_preview_window_level() {
    let saved = PANEL.with(|state| state.borrow_mut().preview_saved_level.take());
    let Some(level) = saved else {
        return;
    };
    if let Some(panel) = PANEL.with(|state| state.borrow().panel) {
        unsafe {
            let _: () = msg_send![panel, setLevel: level];
        }
    }
}

pub(super) fn reset() {
    crate::debug_assert_main_thread();
    stop_timer();
    let was_previewing = PANEL.with(|panel| panel.borrow_mut().preview_percent.take().is_some());
    PANEL.with(|panel| {
        let mut state = panel.borrow_mut();
        if let Some(panel) = state.panel {
            unsafe {
                let _: () = msg_send![panel, orderOut: std::ptr::null::<AnyObject>()];
                let _: () = msg_send![panel, setAlphaValue: 1.0f64];
            }
        }
        state.visible = false;
        state.fade_deadline = None;
        state.target_frame = None;
        state.pending_target_frame = None;
        state.drag = None;
        set_grip_mouse_state(&mut state, false, GripCursor::None);
        state.last_badges.clear();
        state.last_layout = None;
        state.last_palette = None;
        state.measurements.clear();
        state.latched = false;
        state.panel_cross_latched = 0.0;
        // No press survives the panel's end: a later mouseUp must not open settings for a session
        // that has already been torn down. The VIEW handle stays: the panel window is cached and
        // reused, so dropping the handle here would leave the button in the tree but lose the only
        // reference that re-lays it out and retints it on the next show.
        state.settings_armed = false;
    });
    // A reset while a preview is running (the feature was switched off, or the panel was torn
    // down) must not leave the window at the lifted level or the preview marker set.
    if was_previewing {
        restore_preview_window_level();
    }
}

pub(super) fn smoke_runner() -> bool {
    crate::debug_assert_main_thread();
    // This runner is a `--smoke-*` launch, and a smoke launch must not touch the developer's own
    // clipboard: reading the real keychain prompts for the login password (the cargo binary a smoke
    // spawns is not the build the key item's ACL admits), and recording the real pasteboard would
    // write that content into a temporary history encrypted with the test key published in the
    // source. Assert both here, in a runner that would otherwise do exactly that, so losing the
    // rule fails a tier-A check instead of a person's password dialog and their clipboard content.
    let key_provider = crate::clipboard::key_provider_label();
    let pasteboard_monitored = crate::clipboard::pasteboard_monitor_active();
    let smoke_process_isolated = key_provider != "keychain" && !pasteboard_monitored;
    if !smoke_process_isolated {
        eprintln!(
            "[keystroke-display-smoke] smoke launch isolation lost: key_provider={key_provider} pasteboard_monitored={pasteboard_monitored}"
        );
    }
    // A `--panel-material=` launch switch can name the development-only `backdrop` surface, whose checks
    // (system material present, palette surface painted) describe the shipped materials and not that
    // surface. Only that one is exempt: a switch naming a shipped material (`--panel-material=frost`) is
    // still held to the shipped checks, or forcing a material would be a way to skip them.
    let dev_only_material_forced =
        crate::dev_flags::value("panel-material").is_some_and(|value| value == "backdrop");
    super::mapping::refresh_layout_cache();
    let option_q_is_unmodified = super::mapping::key_glyph(
        crate::event_tap::keyboard::VK_Q,
        "œ",
        crate::event_tap::keyboard::FLAG_OPTION,
    )
    .as_deref()
        == Some("Q");
    let (initial_position, saved_position) = {
        let config = crate::config::CONFIG.read().unwrap();
        (
            config.keystroke_display.initial_position.clone(),
            config.keystroke_display.position,
        )
    };
    let smoke_now = Instant::now();
    // Exercise the multi-key cell path (one keycap per key) before the single-label checks.
    let cell_badge = Badge {
        text: "⌘⇧Q".into(),
        kind: BadgeKind::Chord,
        repeats: 1,
        cells: vec![
            BadgeCell::Modifier("⌘".into()),
            BadgeCell::Modifier("⇧".into()),
            BadgeCell::Key("Q".into()),
        ],
    };
    let _ = render(
        &[cell_badge],
        true,
        false,
        PanelPlacement {
            display_position: "main",
            initial_position: &initial_position,
            position: saved_position,
            max_extent_percent: crate::config::KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_DEFAULT,
        },
        smoke_now,
        None,
    );
    let badge = Badge {
        text: "⌘Q".into(),
        kind: BadgeKind::Chord,
        repeats: 2,
        cells: Vec::new(),
    };
    let _ = render(
        &[badge],
        true,
        false,
        PanelPlacement {
            display_position: "main",
            initial_position: &initial_position,
            position: saved_position,
            max_extent_percent: crate::config::KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_DEFAULT,
        },
        smoke_now,
        None,
    );
    let _ = render(
        &[],
        false,
        false,
        PanelPlacement {
            display_position: "main",
            initial_position: &initial_position,
            position: saved_position,
            max_extent_percent: crate::config::KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_DEFAULT,
        },
        smoke_now,
        None,
    );
    let _ = render(
        &[Badge {
            text: "⌘Q".into(),
            kind: BadgeKind::Chord,
            repeats: 2,
            cells: Vec::new(),
        }],
        true,
        false,
        PanelPlacement {
            display_position: "main",
            initial_position: &initial_position,
            position: saved_position,
            max_extent_percent: crate::config::KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_DEFAULT,
        },
        smoke_now,
        None,
    );
    let cjk_badges = [
        Badge {
            text: "中文".into(),
            kind: BadgeKind::Chord,
            repeats: 1,
            cells: Vec::new(),
        },
        Badge {
            // Keycap-length label: real named keys top out at three CJK chars ("空白鍵"), and
            // the smoke's fit check must measure a label the rail actually holds.
            text: "漢字".into(),
            kind: BadgeKind::Chord,
            repeats: 12,
            cells: Vec::new(),
        },
    ];
    let _ = render(
        &cjk_badges,
        true,
        false,
        PanelPlacement {
            display_position: "main",
            initial_position: &initial_position,
            position: saved_position,
            max_extent_percent: crate::config::KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_DEFAULT,
        },
        smoke_now,
        None,
    );
    let badge_container = PANEL.with(|panel| panel.borrow().badge_container);
    let centroid_offsets =
        badge_container.and_then(|content| unsafe { text_centroid_offsets(content, &cjk_badges) });
    eprintln!("[keystroke-display-smoke] text-centroid-offsets-pt={centroid_offsets:?}");
    let typical_glyph_advances = unsafe {
        let cjk = measure_text_width("中");
        let emoji = measure_text_width("🙂");
        (14.0..=18.0).contains(&cjk) && (16.0..=24.0).contains(&emoji)
    };
    let valid = PANEL.with(|panel| {
        let state = panel.borrow();
        let (Some(panel), Some(badge_container), Some(grip_view)) =
            (state.panel, state.badge_container, state.grip_view)
        else {
            return false;
        };
        unsafe {
            let visible: bool = msg_send![panel, isVisible];
            let frame: NSRect = crate::glass::panel_frame_of(panel);
            let alpha: f64 = msg_send![panel, alphaValue];
            let grip_actual: NSRect = msg_send![grip_view, frame];
            let screen_width = screen_geometries()
                .first()
                .map_or(frame.size.width * 2.0, |screen| screen.frame.size.width);
            let views: *mut AnyObject = msg_send![badge_container, subviews];
            let count: usize = msg_send![views, count];
            let mut badges_fit = count == cjk_badges.len();
            for (index, badge) in cjk_badges.iter().enumerate() {
                if index >= count {
                    badges_fit = false;
                    break;
                }
                let badge_view: *mut AnyObject = msg_send![views, objectAtIndex: index as isize];
                let badge_frame: NSRect = msg_send![badge_view, frame];
                // A column renders the merge count on its own row below the glyph, so the
                // keycap only has to fit the plain label; a row keeps the count inline.
                let text = if state.orientation.is_vertical() {
                    badge.text.clone()
                } else {
                    badge_display_text(&badge.text, badge.repeats)
                };
                let measured = measure_text_width(&text);
                if badge_frame.size.width + 0.5 < measured + BADGE_HORIZONTAL_PADDING {
                    badges_fit = false;
                    break;
                }
            }
            // Assert the orientation-independent hit-area shape; comparing to grip_frame here
            // would only verify that render used the same implementation under test.
            let grip_shape_valid = if state.orientation.is_vertical() {
                grip_actual.size.width > grip_actual.size.height
            } else {
                grip_actual.size.height > grip_actual.size.width
            };
            visible
                && alpha >= 0.99
                && grip_shape_valid
                && frame.size.width <= screen_width * 0.5 + 0.5
                && typical_glyph_advances
                && option_q_is_unmodified
                && backdrop_structure_valid(panel, badge_container)
                // The frost mask belongs to the system frost material; a launch switch that forces the
                // development-only blended surface has no material view to mask.
                && (dev_only_material_forced || backdrop_frost_mask_valid())
                // The floor veil belongs to frost in light mode: one there, none in dark mode, and never
                // more than one. This smoke already flips the theme below, so both directions are read.
                && (dev_only_material_forced || backdrop_floor_veil_valid())
                // The palette surface is painted by the shipped materials; the development-only blur
                // surface paints nothing (its own check is the backdrop layer in `backdrop_structure_valid`).
                && (dev_only_material_forced || backdrop_surface_matches_palette())
                // The window's appearance is what the gear's dynamic ink resolves against; without it the
                // panel shows its palette surface while the gear stays the inherited (Light) glyph.
                && panel_appearance_matches_mode()
                && centroid_offsets.as_ref().is_some_and(|offsets| {
                    offsets.len() == cjk_badges.len()
                        && offsets.iter().all(|(offset, pixels)| {
                            offset.abs() <= MAX_TEXT_CENTROID_OFFSET && *pixels > 0
                        })
                })
                && badges_fit
        }
    });
    // Every material must end up painted with the surface the palette calls for -- read back from the
    // real layer (or the glass tint), not from a value the painting function recorded. A material
    // whose check is skipped (the glass branch used to return true) is a check that cannot fail.
    let every_material_paints_its_surface = {
        let original = crate::config::CONFIG
            .read()
            .unwrap()
            .appearance
            .panel_material
            .clone();
        let mut ok = true;
        // A `--panel-material=` launch switch outranks the config writes this loop makes (that is what the
        // switch is for), so it observes exactly one material: its own. Checking the three here would assert
        // against surfaces the switch prevents from being installed.
        let forced = crate::dev_flags::value("panel-material");
        let materials: Vec<&str> = match forced.as_deref() {
            Some(value) => vec![value],
            None => vec!["frost", "opaque", "liquid-glass"],
        };
        for material in materials {
            {
                let mut config = crate::config::CONFIG.write().unwrap();
                config.appearance.panel_material = material.to_string();
            }
            unsafe {
                apply_backdrop_material();
                apply_glass_properties();
            }
            let painted = unsafe { backdrop_surface_matches_palette() };
            if !painted {
                eprintln!(
                    "[keystroke-display-smoke] material={material} did not paint its surface"
                );
            }
            ok &= painted;
        }
        {
            let mut config = crate::config::CONFIG.write().unwrap();
            config.appearance.panel_material = original;
        }
        unsafe {
            apply_backdrop_material();
            apply_glass_properties();
        }
        ok && unsafe { backdrop_surface_matches_palette() }
    };

    // A theme change refreshes the panel's surface: the keys re-read the palette on every render, so
    // a surface left in the previous theme shows light keys on a dark shell (or the reverse) -- the
    // reported light-mode bug. Switching the in-memory config and running the theme refresh is the
    // same path the settings page and the menu use.
    let theme_refresh_retints_surface = {
        let original = crate::config::CONFIG
            .read()
            .unwrap()
            .appearance
            .theme
            .clone();
        let original_material = crate::config::CONFIG
            .read()
            .unwrap()
            .appearance
            .panel_material
            .clone();
        // The theme switch must exercise frost whatever the user's config is: the material loop above
        // restored the original (default `liquid-glass`), and the frost read-back is what this check is
        // for. A `--panel-material=` launch switch is already in force, so it names the material under
        // test instead of exempting the check.
        let theme_material =
            crate::dev_flags::value("panel-material").unwrap_or_else(|| "frost".to_string());
        {
            let mut config = crate::config::CONFIG.write().unwrap();
            config.appearance.panel_material = theme_material;
        }
        unsafe {
            apply_backdrop_material();
            apply_glass_properties();
        }
        let other = if crate::theme::resolved_is_dark() {
            "light"
        } else {
            "dark"
        };
        {
            let mut config = crate::config::CONFIG.write().unwrap();
            config.appearance.theme = other.to_string();
        }
        crate::ui_coordinator::apply_theme_and_locale_refresh();
        let mut changed = unsafe { backdrop_surface_matches_palette() };
        // The gear's ink follows the window's appearance, which must follow the palette: a cached
        // window that kept the previous mode's appearance rendered a near-black gear on the dark
        // panel (and a near-white one on the light panel).
        changed &= unsafe { panel_appearance_matches_mode() };
        // Render once so the keys and the surface are both drawn in the new theme.
        let probe_badge = Badge {
            text: "⌘⇧Q".into(),
            kind: BadgeKind::Chord,
            repeats: 1,
            cells: vec![
                BadgeCell::Modifier("⌘".into()),
                BadgeCell::Modifier("⇧".into()),
                BadgeCell::Key("Q".into()),
            ],
        };
        let _ = render(
            &[probe_badge],
            true,
            false,
            PanelPlacement {
                display_position: "bottom",
                initial_position: &initial_position,
                position: None,
                max_extent_percent: crate::config::KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_DEFAULT,
            },
            smoke_now,
            None,
        );
        changed &= unsafe { backdrop_surface_matches_palette() };
        // Every repaint above is also where the floor veil must be added or dropped: the theme each step
        // applies is the one the veil's presence follows, so this reads both directions, in the light and
        // in the dark.
        changed &= unsafe { backdrop_floor_veil_valid() };
        // The system-appearance entry point (the "auto" theme path) must repaint it too: that
        // notification does not go through the config-change coordinator. Each step starts from the
        // opposite theme, so a missing repaint leaves the surface mismatched -- a step that set the
        // theme to what is already painted could not fail.
        for theme in [original.clone(), other.to_string()] {
            {
                let mut config = crate::config::CONFIG.write().unwrap();
                config.appearance.theme = theme;
            }
            crate::apply_system_appearance_refresh();
            changed &= unsafe { backdrop_surface_matches_palette() };
            changed &= unsafe { backdrop_floor_veil_valid() };
            changed &= unsafe { panel_appearance_matches_mode() };
        }
        {
            let mut config = crate::config::CONFIG.write().unwrap();
            config.appearance.theme = original;
            config.appearance.panel_material = original_material;
        }
        unsafe {
            apply_backdrop_material();
        }
        crate::ui_coordinator::apply_theme_and_locale_refresh();
        changed && unsafe { backdrop_surface_matches_palette() }
    };

    /// A1 smoke: the key panel's trailing settings button sits inside the panel on both orientations and
    /// never overlaps a keycap.
    ///
    /// The budget refactor is what this guards: if the stream were allowed to use the button's reserve,
    /// the last keycap would be laid out under the button, which no pure budget test would see.
    fn smoke_settings_button_clear_of_keycaps() -> bool {
        PANEL.with(|panel| {
        let state = panel.borrow();
        let (Some(window), Some(settings_view)) = (state.panel, state.settings_view) else {
            return false;
        };
        let panel_frame: NSRect = unsafe { crate::glass::panel_frame_of(window) };
        let button_frame: NSRect = unsafe { msg_send![settings_view, frame] };
        let inside = button_frame.origin.x >= -0.5
            && button_frame.origin.y >= -0.5
            && button_frame.origin.x + button_frame.size.width <= panel_frame.size.width + 0.5
            && button_frame.origin.y + button_frame.size.height <= panel_frame.size.height + 0.5;
        if !inside {
            eprintln!(
                "[keystroke-display-smoke] settings button {:.1},{:.1} {:.1}x{:.1} is outside the {:.1}x{:.1} panel",
                button_frame.origin.x,
                button_frame.origin.y,
                button_frame.size.width,
                button_frame.size.height,
                panel_frame.size.width,
                panel_frame.size.height
            );
            return false;
        }
        let Some(container) = state.badge_container else {
            return false;
        };
        let subviews: *mut AnyObject = unsafe { msg_send![container, subviews] };
        let count: usize = unsafe { msg_send![subviews, count] };
        for index in 0..count {
            let badge: *mut AnyObject =
                unsafe { msg_send![subviews, objectAtIndex: index as isize] };
            let badge_frame: NSRect = unsafe { msg_send![badge, frame] };
            let overlaps = button_frame.origin.x < badge_frame.origin.x + badge_frame.size.width
                && badge_frame.origin.x < button_frame.origin.x + button_frame.size.width
                && button_frame.origin.y < badge_frame.origin.y + badge_frame.size.height
                && badge_frame.origin.y < button_frame.origin.y + button_frame.size.height;
            if overlaps {
                eprintln!(
                    "[keystroke-display-smoke] keycap {index} overlaps the settings button"
                );
                return false;
            }
        }
        // The gear paints the SHARED corner-glyph wash when hovered and is transparent when not --
        // compared against the value the shared function computes, not merely against "something
        // different". The RGB-only reader is included as a documented counter-example: it accepts the
        // raw palette fill (whose RGB is identical to the wash), which is why the RGBA reader exists.
        let palette = crate::theme::ui_palette();
        let shared = crate::ui::corner_glyph::hover_wash(palette);
        let raw = palette.hover_bg;
        let layer: *mut AnyObject = unsafe { msg_send![settings_view, layer] };
        unsafe { set_settings_button_style(settings_view, true) };
        if !unsafe { layer_background_matches_rgba(layer, shared) } {
            eprintln!("[keystroke-display-smoke] the gear did not paint the shared hover wash");
            return false;
        }
        unsafe { set_settings_button_style(settings_view, false) };
        if !unsafe { layer_background_matches_rgba(layer, 0x0000_0000) } {
            eprintln!("[keystroke-display-smoke] the gear is not transparent when not hovered");
            return false;
        }
        // The counter-example: paint the raw fill and require the RGB-only reader to accept it while
        // the RGBA reader rejects it. Then restore the shared wash, leaving the panel normal.
        unsafe { crate::ffi::layer_set_background(layer, crate::ffi::hex_to_cg_color(raw)) };
        if raw & 0xFFFF_FF00 != shared & 0xFFFF_FF00 {
            eprintln!("[keystroke-display-smoke] the counter-example is not RGB-identical");
            return false;
        }
        if !unsafe { layer_background_matches(layer, shared) } {
            eprintln!("[keystroke-display-smoke] the RGB-only reader should be blind to the raw alpha");
            return false;
        }
        if unsafe { layer_background_matches_rgba(layer, shared) } {
            eprintln!("[keystroke-display-smoke] the RGBA reader accepted the raw fill; it cannot decide");
            return false;
        }
        unsafe { set_settings_button_style(settings_view, false) };
        true
    })
    }

    let settings_button_valid = smoke_settings_button_clear_of_keycaps();
    let grip_valid = smoke_grip_interaction(saved_position);
    let rail_geometry_valid = {
        // The rail geometry over the real render path, in a column: every keycap is
        // independent (a chord is split into lone keycaps, no tray), each sized to its
        // fixed width and centered as a group with equal outer margins. The
        // stream is long enough to roll keys off the front (extents.remove(0)) so the
        // trimming path is exercised too -- the badge/extent misalignment bug class only
        // fired once `first > 0`.
        let mut long_stream: Vec<Badge> = (0..40)
            .map(|i| Badge {
                text: format!("k{i}"),
                kind: BadgeKind::Chord,
                repeats: 1,
                cells: Vec::new(),
            })
            .collect();
        long_stream.push(Badge {
            text: "⌥Q".into(),
            kind: BadgeKind::Chord,
            repeats: 1,
            cells: vec![BadgeCell::Modifier("⌥".into()), BadgeCell::Key("Q".into())],
        });
        let _ = render(
            &long_stream,
            true,
            false,
            PanelPlacement {
                display_position: "main",
                initial_position: "right",
                position: saved_position,
                max_extent_percent: crate::config::KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_DEFAULT,
            },
            smoke_now,
            None,
        );
        PANEL.with(|panel| {
            let state = panel.borrow();
            let Some(container) = state.badge_container else {
                return false;
            };
            unsafe {
                let views: *mut AnyObject = msg_send![container, subviews];
                let count: usize = msg_send![views, count];
                // The chord exploded into two extra keycaps.
                let expected_max = long_stream.len() + 1;
                if count == 0 || count > expected_max {
                    eprintln!("[keystroke-display-smoke] rail count={count}");
                    return false;
                }
                let mut split_cells = 0usize;
                let bounds: NSRect = msg_send![container, bounds];
                let mut edge_min = f64::MAX;
                let mut edge_max = f64::MIN;
                let mut widths_ok = true;
                let mut margins_ok = true;
                for index in 0..count {
                    let view: *mut AnyObject = msg_send![views, objectAtIndex: index as isize];
                    let frame: NSRect = msg_send![view, frame];
                    // A lone keycap holds one label field (two when a merge count rides
                    // it); any more is a chord tray, which a column must no longer draw.
                    let subs: *mut AnyObject = msg_send![view, subviews];
                    let sub_count: usize = msg_send![subs, count];
                    if sub_count == 0 || sub_count > 2 {
                        eprintln!(
                            "[keystroke-display-smoke] rail view {index} has {sub_count} subviews (tray?)"
                        );
                        return false;
                    }
                    let left_gap = frame.origin.x - bounds.origin.x;
                    let right_gap = bounds.origin.x + bounds.size.width
                        - (frame.origin.x + frame.size.width);
                    margins_ok &= (left_gap - right_gap).abs() < 0.5;
                    // Every fixed-width keycap shares both rail edges.
                    edge_min = edge_min.min(frame.origin.x + frame.size.width);
                    edge_max = edge_max.max(frame.origin.x + frame.size.width);
                    // Fixed width: every keycap in a column renders at the rail width.
                    let field: *mut AnyObject = msg_send![subs, objectAtIndex: 0isize];
                    let value: *mut AnyObject = msg_send![field, stringValue];
                    let label = crate::ffi::nsstring_to_rust(value);
                    // `key_cap_text` upper-cases a keycap (mapping.rs asserts `key_cap_text("q") == "Q"`),
                    // so the rendered label is "Q". Comparing against the lower-case source form counted the
                    // modifier and missed the key, and the check then failed for a correct panel.
                    if label == "⌥" || label.eq_ignore_ascii_case("q") {
                        split_cells += 1;
                    }
                    if (frame.size.width - KEYCAP_RAIL_W).abs() > 0.5 {
                        eprintln!(
                            "[keystroke-display-smoke] rail {label:?} width {:.1} != {KEYCAP_RAIL_W:.1}",
                            frame.size.width
                        );
                        widths_ok = false;
                    }
                }
                let ok = widths_ok
                    && margins_ok
                    && split_cells == 2
                    && (edge_max - edge_min) < 0.5;
                eprintln!(
                    "[keystroke-display-smoke] rail count={count} edge_spread={:.2} margins_ok={margins_ok} split_cells={split_cells} ok={ok}",
                    edge_max - edge_min
                );
                ok
            }
        })
    };
    let row_split_valid = {
        // A row draws the same independent keycaps a column does. The tray this replaced nested
        // one bordered view per key inside a bordered container, so counting a view's own
        // subviews is what detects it: a lone keycap holds one label field, two when the merge
        // count rides it. A row also keeps content-sized keycaps, so neither cap may land on the
        // column rail width.
        let chord = Badge {
            text: "⌥Q".into(),
            kind: BadgeKind::Chord,
            repeats: 1,
            cells: vec![BadgeCell::Modifier("⌥".into()), BadgeCell::Key("Q".into())],
        };
        // Hide first: the panel keeps its size for the rest of a shown session once the stream
        // has hit the cap, and the earlier rail render is long enough to pin it.
        let _ = render(
            &[],
            false,
            false,
            PanelPlacement {
                display_position: "main",
                initial_position: "bottom",
                position: saved_position,
                max_extent_percent: crate::config::KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_DEFAULT,
            },
            smoke_now,
            None,
        );
        let _ = render(
            std::slice::from_ref(&chord),
            true,
            false,
            PanelPlacement {
                display_position: "main",
                initial_position: "bottom",
                position: saved_position,
                max_extent_percent: crate::config::KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_DEFAULT,
            },
            smoke_now,
            None,
        );
        PANEL.with(|panel| {
            let state = panel.borrow();
            let Some(container) = state.badge_container else {
                return false;
            };
            unsafe {
                let views: *mut AnyObject = msg_send![container, subviews];
                let count: usize = msg_send![views, count];
                if count != chord.cells.len() {
                    eprintln!("[keystroke-display-smoke] row count={count} (expected a keycap per key)");
                    return false;
                }
                let mut frames = Vec::with_capacity(count);
                for index in 0..count {
                    let view: *mut AnyObject = msg_send![views, objectAtIndex: index as isize];
                    let frame: NSRect = msg_send![view, frame];
                    let subs: *mut AnyObject = msg_send![view, subviews];
                    let sub_count: usize = msg_send![subs, count];
                    if sub_count == 0 || sub_count > 2 {
                        eprintln!(
                            "[keystroke-display-smoke] row view {index} has {sub_count} subviews (tray?)"
                        );
                        return false;
                    }
                    frames.push(frame);
                }
                let side_by_side = frames.len() == 2
                    && (frames[0].origin.y - frames[1].origin.y).abs() < 0.5
                    && frames[1].origin.x
                        >= frames[0].origin.x + frames[0].size.width + BADGE_GAP - 0.5;
                let content_sized = frames
                    .iter()
                    .all(|frame| frame.size.width < KEYCAP_RAIL_W - 0.5);
                let ok = side_by_side && content_sized;
                eprintln!(
                    "[keystroke-display-smoke] row count={count} widths={:?} side_by_side={side_by_side} content_sized={content_sized} ok={ok}",
                    frames.iter().map(|frame| frame.size.width).collect::<Vec<f64>>()
                );
                ok
            }
        })
    };
    let column_pair_margins_valid = {
        // A column of two keycaps must land on each end's own reserve: the handle's reserve at the
        // top, the gear's at the bottom. The unit test only covers a single keycap, and this stream
        // is two keycaps plus a gap. A panel pinned by a longer stream is not the shape being
        // measured, hence the hide first.
        let chord = Badge {
            text: "⌥Q".into(),
            kind: BadgeKind::Chord,
            repeats: 1,
            cells: vec![BadgeCell::Modifier("⌥".into()), BadgeCell::Key("Q".into())],
        };
        let _ = render(
            &[],
            false,
            false,
            PanelPlacement {
                display_position: "main",
                initial_position: "right",
                position: saved_position,
                max_extent_percent: crate::config::KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_DEFAULT,
            },
            smoke_now,
            None,
        );
        let _ = render(
            std::slice::from_ref(&chord),
            true,
            false,
            PanelPlacement {
                display_position: "main",
                initial_position: "right",
                position: saved_position,
                max_extent_percent: crate::config::KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_DEFAULT,
            },
            smoke_now,
            None,
        );
        PANEL.with(|panel| {
            let state = panel.borrow();
            let Some(container) = state.badge_container else {
                return false;
            };
            unsafe {
                let views: *mut AnyObject = msg_send![container, subviews];
                let count: usize = msg_send![views, count];
                if count != chord.cells.len() {
                    eprintln!(
                        "[keystroke-display-smoke] column pair count={count} (expected a keycap per key)"
                    );
                    return false;
                }
                let bounds: NSRect = msg_send![container, bounds];
                let mut top = f64::NEG_INFINITY;
                let mut bottom = f64::INFINITY;
                for index in 0..count {
                    let view: *mut AnyObject = msg_send![views, objectAtIndex: index as isize];
                    let frame: NSRect = msg_send![view, frame];
                    top = top.max(frame.origin.y + frame.size.height);
                    bottom = bottom.min(frame.origin.y);
                }
                let top_margin = bounds.origin.y + bounds.size.height - top;
                let bottom_margin = bottom - bounds.origin.y;
                let ok = (top_margin - HEAD_STREAM_RESERVE).abs() < 0.5
                    && (bottom_margin - TAIL_SETTINGS_RESERVE).abs() < 0.5;
                eprintln!(
                    "[keystroke-display-smoke] column pair count={count} top_margin={top_margin:.1} bottom_margin={bottom_margin:.1} expected={HEAD_STREAM_RESERVE:.1}/{TAIL_SETTINGS_RESERVE:.1} ok={ok}"
                );
                ok
            }
        })
    };
    let single_key = Badge {
        text: "J".into(),
        kind: BadgeKind::Chord,
        repeats: 1,
        cells: Vec::new(),
    };
    let stream_alignment_valid = ["bottom", "right"].into_iter().all(|position| {
        let _ = render(
            &[],
            false,
            false,
            PanelPlacement {
                display_position: "main",
                initial_position: position,
                position: saved_position,
            max_extent_percent: crate::config::KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_DEFAULT,
            },
            smoke_now,
            None,
        );
        let _ = render(
            std::slice::from_ref(&single_key),
            true,
            false,
            PanelPlacement {
                display_position: "main",
                initial_position: position,
                position: saved_position,
            max_extent_percent: crate::config::KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_DEFAULT,
            },
            smoke_now,
            None,
        );
        PANEL.with(|panel| {
            let state = panel.borrow();
            let Some(container) = state.badge_container else {
                return false;
            };
            unsafe {
                let views: *mut AnyObject = msg_send![container, subviews];
                let count: usize = msg_send![views, count];
                if count != 1 {
                    return false;
                }
                let view: *mut AnyObject = msg_send![views, objectAtIndex: 0isize];
                let frame: NSRect = msg_send![view, frame];
                let bounds: NSRect = msg_send![container, bounds];
                let (leading, trailing) = if state.orientation.is_vertical() {
                    // Non-flipped: a column counts down from the top, so the handle's end is the top
                    // edge and the gear's is the bottom one. The leading margin is the top gap.
                    (
                        bounds.origin.y + bounds.size.height
                            - (frame.origin.y + frame.size.height),
                        frame.origin.y - bounds.origin.y,
                    )
                } else {
                    (
                        frame.origin.x - bounds.origin.x,
                        bounds.origin.x + bounds.size.width
                            - (frame.origin.x + frame.size.width),
                    )
                };
                // The two ends carry different reserves by design, so each margin is checked
                // against its own control's reserve rather than against equality.
                let ok = (leading - HEAD_STREAM_RESERVE).abs() < 0.5
                    && (trailing - TAIL_SETTINGS_RESERVE).abs() < 0.5;
                eprintln!(
                    "[keystroke-display-smoke] single-key {position} margins={leading:.1}/{trailing:.1} expected={HEAD_STREAM_RESERVE:.1}/{TAIL_SETTINGS_RESERVE:.1} ok={ok}"
                );
                ok
            }
        })
    });
    update_config_position(saved_position);
    let config_restored = crate::config::flush_config_sync().is_ok();
    // The cap preview: it must draw the panel at the previewed size, report that it is running,
    // and leave no trace (no config write, no saved position) once it ends.
    let preview_valid = smoke_max_extent_preview(saved_position);
    reset();
    if !(valid
        && smoke_process_isolated
        && rail_geometry_valid
        && stream_alignment_valid
        && settings_button_valid
        && grip_valid
        && config_restored
        && preview_valid
        && (dev_only_material_forced || every_material_paints_its_surface)
        && (dev_only_material_forced || theme_refresh_retints_surface))
    {
        crate::log_info!(
            "[smoke-keystroke-display-panel] which: structure={valid} isolated={smoke_process_isolated} rail={rail_geometry_valid} row={row_split_valid} column_pair={column_pair_margins_valid} stream={stream_alignment_valid} grip={grip_valid} config={config_restored} surface={every_material_paints_its_surface} theme={theme_refresh_retints_surface}"
        );
    }
    valid
        && smoke_process_isolated
        && rail_geometry_valid
        && row_split_valid
        && column_pair_margins_valid
        && stream_alignment_valid
        && settings_button_valid
        && grip_valid
        && config_restored
        && preview_valid
        && (dev_only_material_forced || every_material_paints_its_surface)
        && (dev_only_material_forced || theme_refresh_retints_surface)
}

fn smoke_grip_interaction(original_position: Option<KeystrokeDisplayPosition>) -> bool {
    let Some((start_frame, grip_center)) = panel_frame_and_grip_center() else {
        return false;
    };
    let now = Instant::now();
    begin_grip_interaction(grip_center, 1, now);
    drag_grip_to(
        NSPoint::new(grip_center.x + 18.0, grip_center.y + 11.0),
        now + Duration::from_millis(5),
    );
    finish_grip_interaction();
    let moved_and_saved = PANEL.with(|panel| {
        let state = panel.borrow();
        let Some(window) = state.panel else {
            return false;
        };
        let frame: NSRect = unsafe { crate::glass::panel_frame_of(window) };
        let saved = crate::config::CONFIG
            .read()
            .unwrap()
            .keystroke_display
            .position;
        (frame.origin.x - start_frame.origin.x - 18.0).abs() < 0.5
            && (frame.origin.y - start_frame.origin.y - 11.0).abs() < 0.5
            && saved.is_some_and(|saved| {
                (saved.x - frame.origin.x).abs() < 0.5 && (saved.y - frame.origin.y).abs() < 0.5
            })
    });

    let Some((_, first_click_point)) = panel_frame_and_grip_center() else {
        update_config_position(original_position);
        return false;
    };
    let frame_after_drag = panel_frame_and_grip_center().map(|(frame, _)| frame);
    let click_time = now + Duration::from_millis(10);
    begin_grip_interaction(first_click_point, 1, click_time);
    drag_grip_to(
        NSPoint::new(first_click_point.x + 2.0, first_click_point.y + 1.0),
        click_time + Duration::from_millis(5),
    );
    finish_grip_interaction();
    let Some((frame_before_reset, second_click_point)) = panel_frame_and_grip_center() else {
        update_config_position(original_position);
        return false;
    };
    begin_grip_interaction(
        second_click_point,
        2,
        click_time + Duration::from_millis(100),
    );
    finish_grip_interaction();
    let reset_to_default = PANEL.with(|panel| {
        let state = panel.borrow();
        let Some(window) = state.panel else {
            return false;
        };
        let frame: NSRect = unsafe { crate::glass::panel_frame_of(window) };
        let ignores_mouse_events: bool = unsafe { msg_send![window, ignoresMouseEvents] };
        let config = crate::config::CONFIG
            .read()
            .unwrap()
            .keystroke_display
            .clone();
        let screens = unsafe { screen_geometries() };
        let target_index = target_screen_index_live(&config.display_position, &screens);
        let expected = screens.get(target_index).map(|screen| {
            default_edge_frame(screen.visible_frame, frame.size, &config.initial_position)
        });
        config.position.is_none()
            && ignores_mouse_events
            && frame_after_drag.is_some_and(|dragged| {
                (frame_before_reset.origin.x - dragged.origin.x).abs() < 0.5
                    && (frame_before_reset.origin.y - dragged.origin.y).abs() < 0.5
            })
            && expected.is_some_and(|expected| {
                // <= 0.5, not < 0.5: AppKit rounds a window origin onto the backing grid, so
                // a half-point round-trip (set 402.5, read 402.0) is alignment, not drift.
                (frame.origin.x - expected.origin.x).abs() <= 0.5
                    && (frame.origin.y - expected.origin.y).abs() <= 0.5
            })
            && (frame_before_reset.origin.x - frame.origin.x).abs() > 0.0
    });
    moved_and_saved && reset_to_default
}

/// The cap preview, asserted on the real AppKit tree: it draws the panel at the previewed size,
/// reports itself as running, writes no configuration, and leaves nothing behind when it ends.
/// `saved_position` is the position the run started with, restored by the caller.
fn smoke_max_extent_preview(saved_position: Option<KeystrokeDisplayPosition>) -> bool {
    let before_frames = crate::config::CONFIG
        .read()
        .unwrap()
        .keystroke_display
        .clone();
    begin_max_extent_preview(20);
    let small = e2e_geometry();
    let running = max_extent_preview_active();
    let reported = max_extent_preview_percent();
    // The virtual stream must be what the panel laid out, not the caller's badges: the preview
    // path ignores the real stream entirely.
    let virtual_content = preview_badges();
    let virtual_ok = virtual_content.len() == PREVIEW_MAX_BADGES
        && virtual_content.iter().all(|badge| badge.cells.is_empty());
    begin_max_extent_preview(80);
    let large = e2e_geometry();
    end_max_extent_preview();
    let stopped = !max_extent_preview_active();
    let after_frames = crate::config::CONFIG
        .read()
        .unwrap()
        .keystroke_display
        .clone();
    let config_untouched = before_frames == after_frames && after_frames.position == saved_position;
    // 80% must lay the panel out longer than 20% along the stream axis (the width of a row,
    // the height of a column), and the panel must be on screen for both.
    let orientation = Orientation::from_initial_position(&after_frames.initial_position);
    let grew = match (small, large) {
        (Some(small), Some(large)) if orientation.is_vertical() => large.panel_h > small.panel_h,
        (Some(small), Some(large)) => large.panel_w > small.panel_w,
        _ => false,
    };
    // The window growing is not enough: the keycap subviews have to be laid out again for the new
    // size, or the panel resizes around keycaps still positioned for the old one. Measure the real
    // subviews at both sizes, the way the rest of this runner measures the live view tree.
    begin_max_extent_preview(20);
    let small_caps = keycap_layout_snapshot();
    begin_max_extent_preview(80);
    let large_caps = keycap_layout_snapshot();
    end_max_extent_preview();
    // Both sizes must fill their panel with visible keycaps, and the larger cap must carry strictly
    // more of them along the stream axis (the trim keeps only what fits, so the count is the layout).
    let caps_relaid = match (small_caps, large_caps) {
        (Some(small), Some(large)) => {
            small.count > 0 && large.count > small.count && small.within_panel && large.within_panel
        }
        _ => false,
    };
    // A secure-input transition during a preview must still reach the REAL state machine: the
    // events are drained while the preview owns the frame, and dropping these two would leave the
    // machine paused (or holding a stale modifier) once the user is handed back to it.
    let security_synced = smoke_preview_keeps_security_state();
    if !(running
        && reported == Some(20)
        && virtual_ok
        && stopped
        && config_untouched
        && grew
        && caps_relaid)
    {
        crate::log_info!(
            "[smoke-keystroke-display-panel] preview running={running} reported={reported:?} virtual={virtual_ok} stopped={stopped} config={config_untouched} grew={grew} caps={caps_relaid}"
        );
    }
    running
        && reported == Some(20)
        && virtual_ok
        && stopped
        && config_untouched
        && grew
        && caps_relaid
        && security_synced
}

/// A secure-input transition and a modifier release both arrive while a preview is draining events.
/// Neither may be lost: the first decides whether the machine is paused when the real stream
/// resumes, the second decides whether a modifier still reads as held.
fn smoke_preview_keeps_security_state() -> bool {
    use crate::keystroke_display::state::Input as StateInput;
    // Start from a known state: not paused, no badges. The smoke runner has the display enabled.
    super::STATE.with(|state| {
        *state.borrow_mut() = super::StateMachine::default();
    });
    begin_max_extent_preview(50);
    let previewing = max_extent_preview_active();
    // Post through the same queue the tap uses, then drive the real tick body that drains it.
    super::enqueue(StateInput::SecureActive(true));
    crate::keystroke_display::process_tick_for_test();
    let paused_while_previewing = super::STATE.with(|state| state.borrow().secure_paused());
    let still_previewing = max_extent_preview_active();
    super::enqueue(StateInput::SecureActive(false));
    crate::keystroke_display::process_tick_for_test();
    let resumed = !super::STATE.with(|state| state.borrow().secure_paused());
    end_max_extent_preview();
    super::STATE.with(|state| {
        *state.borrow_mut() = super::StateMachine::default();
    });
    previewing && paused_while_previewing && still_previewing && resumed
}

/// What the preview's keycap subviews look like right now: how many there are, and whether every
/// one of them sits inside the badge container it was laid out for.
struct KeycapLayout {
    count: usize,
    within_panel: bool,
}

fn keycap_layout_snapshot() -> Option<KeycapLayout> {
    PANEL.with(|panel| {
        let state = panel.borrow();
        let container = state.badge_container?;
        unsafe {
            let subviews: *mut AnyObject = msg_send![container, subviews];
            let count: usize = msg_send![subviews, count];
            let bounds: NSRect = msg_send![container, bounds];
            let mut within_panel = true;
            for index in 0..count {
                let view: *mut AnyObject = msg_send![subviews, objectAtIndex: index as isize];
                let frame: NSRect = msg_send![view, frame];
                if frame.origin.x < -0.5
                    || frame.origin.y < -0.5
                    || frame.origin.x + frame.size.width > bounds.size.width + 0.5
                    || frame.origin.y + frame.size.height > bounds.size.height + 0.5
                {
                    within_panel = false;
                }
            }
            Some(KeycapLayout {
                count,
                within_panel,
            })
        }
    })
}
fn panel_frame_and_grip_center() -> Option<(NSRect, NSPoint)> {
    PANEL.with(|panel| {
        let state = panel.borrow();
        let window = state.panel?;
        let frame: NSRect = unsafe { crate::glass::panel_frame_of(window) };
        let grip = grip_frame(state.orientation, frame.size);
        Some((
            frame,
            NSPoint::new(
                frame.origin.x + grip.origin.x + grip.size.width / 2.0,
                frame.origin.y + grip.origin.y + grip.size.height / 2.0,
            ),
        ))
    })
}

unsafe fn text_centroid_offsets(
    content: *mut AnyObject,
    badges: &[Badge],
) -> Option<Vec<(f64, usize)>> {
    let bounds: NSRect = msg_send![content, bounds];
    let bitmap: *mut AnyObject = msg_send![content, bitmapImageRepForCachingDisplayInRect: bounds];
    if bitmap.is_null() {
        return None;
    }
    let _: () = msg_send![content, cacheDisplayInRect: bounds, toBitmapImageRep: bitmap];
    let pixels_wide: usize = msg_send![bitmap, pixelsWide];
    let pixels_high: usize = msg_send![bitmap, pixelsHigh];
    if pixels_wide == 0 || pixels_high == 0 || bounds.size.width <= 0.0 || bounds.size.height <= 0.0
    {
        return None;
    }
    let scale_x = pixels_wide as f64 / bounds.size.width;
    let scale_y = pixels_high as f64 / bounds.size.height;
    let views: *mut AnyObject = msg_send![content, subviews];
    let count: usize = msg_send![views, count];
    if count != badges.len() {
        return None;
    }
    let target = crate::theme::ui_palette().primary_text;
    let target_r = ((target >> 24) & 0xFF) as f64 / 255.0;
    let target_g = ((target >> 16) & 0xFF) as f64 / 255.0;
    let target_b = ((target >> 8) & 0xFF) as f64 / 255.0;
    let mut offsets = Vec::with_capacity(badges.len());
    for (index, badge) in badges.iter().enumerate().take(count) {
        let badge_view: *mut AnyObject = msg_send![views, objectAtIndex: index as isize];
        let badge_frame: NSRect = msg_send![badge_view, frame];
        let fields: *mut AnyObject = msg_send![badge_view, subviews];
        let field_count: usize = msg_send![fields, count];
        // A lone keycap holds one label field, or two when a column renders its merge count
        // on a suffix row below the glyph; the glyph field is always added first.
        if field_count == 0 || field_count > 2 {
            return None;
        }
        let field: *mut AnyObject = msg_send![fields, objectAtIndex: 0isize];
        let field_frame: NSRect = msg_send![field, frame];
        let label = if field_count == 2 {
            badge.text.clone()
        } else {
            badge_display_text(&badge.text, badge.repeats)
        };
        let text_width = measure_text_width(&label);
        let x_min = badge_frame.origin.x
            + field_frame.origin.x
            + (field_frame.size.width - text_width) / 2.0
            - 2.0;
        let x_max = x_min + text_width + 4.0;
        let y_min = badge_frame.origin.y + field_frame.origin.y;
        let y_max = y_min + field_frame.size.height;
        let px_min = (((x_min - bounds.origin.x) * scale_x).floor().max(0.0)) as usize;
        let px_max =
            (((x_max - bounds.origin.x) * scale_x).ceil().max(0.0) as usize).min(pixels_wide);
        let py_min = (((y_min - bounds.origin.y) * scale_y).floor().max(0.0)) as usize;
        let py_max =
            (((y_max - bounds.origin.y) * scale_y).ceil().max(0.0) as usize).min(pixels_high);
        let (mut weighted_y, mut pixel_count) = (0.0, 0usize);
        for py in py_min..py_max {
            // The bitmap's rows run top-down while container coordinates run bottom-up; read
            // the mirrored row or the sampled region lands on the wrong side of the panel.
            let row = pixels_high - 1 - py;
            for px in px_min..px_max {
                let color: *mut AnyObject =
                    msg_send![bitmap, colorAtX: px as isize, y: row as isize];
                if color.is_null() {
                    continue;
                }
                let red: f64 = msg_send![color, redComponent];
                let green: f64 = msg_send![color, greenComponent];
                let blue: f64 = msg_send![color, blueComponent];
                let distance =
                    (red - target_r).abs() + (green - target_g).abs() + (blue - target_b).abs();
                if distance < 0.45 {
                    weighted_y += py as f64 + 0.5;
                    pixel_count += 1;
                }
            }
        }
        if pixel_count == 0 {
            return None;
        }
        let centroid_y = bounds.origin.y + weighted_y / pixel_count as f64 / scale_y;
        // A merged keycap centers its glyph in the top cap zone (its count takes the row
        // below); every other badge centers text in its whole frame.
        let badge_center_y = if field_count == 2 {
            badge_frame.origin.y + badge_frame.size.height - BADGE_H / 2.0
        } else {
            badge_frame.origin.y + badge_frame.size.height / 2.0
        };
        offsets.push((centroid_y - badge_center_y, pixel_count));
    }
    Some(offsets)
}

/// The settings button's class: mouseDown arms the press (so a release outside the button cancels
/// rather than acting), and the hover pair carries the neutral corner-glyph wash.
unsafe fn settings_button_class() -> *mut AnyObject {
    settings_button_class_handle().0 as *mut AnyObject
}

/// The class handle itself, so the super dispatch inside `mouseDown:` can name the class that
/// defined that override.
fn settings_button_class_handle() -> StaticClass {
    static CLASS: OnceLock<StaticClass> = OnceLock::new();
    crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabKeyPanelSettingsButton", || {
        crate::ffi::build_class(
            c"OhMyTabKeyPanelSettingsButton",
            class!(NSButton),
            |builder| unsafe {
                builder.add_method::<AnyObject, _>(
                    sel!(mouseDown:),
                    settings_mouse_down
                        as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
                builder.add_method::<AnyObject, _>(
                    sel!(mouseEntered:),
                    settings_entered as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
                builder.add_method::<AnyObject, _>(
                    sel!(mouseExited:),
                    settings_exited as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
                // The click action MUST be registered on this class: the button targets itself and an
                // unregistered selector would abort the process on the first click.
                builder.add_method::<AnyObject, _>(
                    sel!(onKeyPanelSettings:),
                    on_key_panel_settings
                        as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
            },
        )
    })
}

extern "C" fn settings_mouse_down(_self: *mut AnyObject, _cmd: Sel, event: *mut AnyObject) {
    crate::callback_guard::void("settings_mouse_down", || unsafe {
        PANEL.with(|panel| panel.borrow_mut().settings_armed = true);
        set_settings_button_style(_self, true);
        // NSButton's own mouseDown: runs the highlight/press tracking loop without which the button
        // never tracks the press and never dispatches its action.
        crate::ui::corner_glyph::call_super_mouse_down(
            _self as *mut c_void,
            settings_button_class_handle(),
            sel!(mouseDown:),
            event as *mut c_void,
        );
    });
}

extern "C" fn settings_entered(_self: *mut AnyObject, _cmd: Sel, _event: *mut AnyObject) {
    crate::callback_guard::void("settings_entered", || unsafe {
        set_settings_button_style(_self, true);
    });
}

extern "C" fn settings_exited(_self: *mut AnyObject, _cmd: Sel, _event: *mut AnyObject) {
    crate::callback_guard::void("settings_exited", || {
        // Leaving during the press cancels it; AppKit still delivers the action on mouseUp, so the
        // armed flag is what tells a real click from a drag-out.
        PANEL.with(|panel| panel.borrow_mut().settings_armed = false);
        unsafe { set_settings_button_style(_self, false) };
    });
}

extern "C" fn on_key_panel_settings(_self: *mut AnyObject, _cmd: Sel, _sender: *mut AnyObject) {
    crate::callback_guard::void("on_key_panel_settings", || {
        // Two entry paths, with different guards:
        //   * a real mouse press arms on mouseDown; the flag separates this press from a stale
        //     mouseUp that belongs to a previous, already-dismissed panel;
        //   * an Accessibility press (AXPress) has no mouseDown, so requiring the arm would reject
        //     it. It is recognised by the absence of a current mouse event.
        let from_mouse = unsafe {
            let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let event: *mut AnyObject = msg_send![nsapp, currentEvent];
            !event.is_null() && {
                let kind: isize = msg_send![event, type];
                kind == 1 || kind == 2
            }
        };
        let armed = PANEL.with(|panel| {
            let mut state = panel.borrow_mut();
            let armed = state.settings_armed;
            state.settings_armed = false;
            armed
        });
        if from_mouse && !armed {
            return;
        }
        open_key_panel_settings();
    });
}

pub(super) fn open_key_panel_settings() {
    crate::debug_assert_main_thread();
    // Show settings first, then stop the panel: the panel would otherwise be gone for a frame while
    // the settings window is still being built, which reads as a stall. The panel is a passive HUD
    // with no session to retire, so there is no equivalent of the switcher's ordering constraint.
    crate::settings::show_keystroke_display_page();

    // The panel is a passive HUD: stop it in place. (A --show-keystroke-display launch freezes the
    // idle deadline, so the timer is what this stops; the badge state is cleared by reset_on_main
    // on the module's own path.)
    reset();
    super::reset_on_main();
}

/// A2 geometry: the panel and its gear in screen points, top-left origin. Returns None when the
/// panel or its gear does not exist.
pub(super) fn e2e_geometry() -> Option<super::E2ePanelGeometry> {
    PANEL.with(|panel| {
        let state = panel.borrow();
        let window = state.panel?;
        let gear = state.settings_view?;
        unsafe {
            let frame = crate::glass::panel_frame_of(window);
            let screen = screen_geometries().first()?.frame;
            let gear_frame: NSRect = msg_send![gear, frame];
            Some(super::E2ePanelGeometry {
                panel_x: frame.origin.x,
                panel_y: screen.size.height - (frame.origin.y + frame.size.height),
                panel_w: frame.size.width,
                panel_h: frame.size.height,
                gear_x: frame.origin.x + gear_frame.origin.x,
                gear_y: screen.size.height
                    - (frame.origin.y + gear_frame.origin.y + gear_frame.size.height),
                gear_w: gear_frame.size.width,
                gear_h: gear_frame.size.height,
            })
        }
    })
}
/// Refresh the key panel's cached controls after a theme or locale change. The panel window is
/// reused, so its settings gear would otherwise keep the previous language and an unhovered tint
/// from the previous palette.
pub(crate) unsafe fn refresh_cached_controls() {
    PANEL.with(|panel| {
        let state = panel.borrow();
        let Some(button) = state.settings_view else {
            return;
        };
        crate::ui::corner_glyph::refresh_glyph_button_strings(
            button,
            "keystroke_display.open_settings",
        );
        set_settings_button_style(button, false);
    });
}

/// Put the HUD on the vibrant appearance for the resolved mode, like the other floating panels.
///
/// The gear draws through `glass::panel_ink`, which under frost resolves to the dynamic system label
/// colours -- and those follow the WINDOW's appearance. Without this the panel kept whatever appearance
/// it inherited from the app (Aqua on a Light system), so with the dark palette the gear resolved to a
/// near-black ink on a dark surface: an invisible affordance. The keycaps never showed it because they
/// paint absolute palette tokens. `apply_glass_properties` re-applies it on every palette repaint,
/// because the cached window outlives the mode it was built in.
unsafe fn apply_panel_appearance(window: *mut AnyObject) {
    let name = make_nsstring(crate::glass::vibrant_appearance_name(
        crate::theme::resolved_is_dark(),
    ));
    let appearance: *mut AnyObject = msg_send![class!(NSAppearance), appearanceNamed: name];
    CFRelease(name as *const c_void);
    if !appearance.is_null() {
        let _: () = msg_send![window, setAppearance: appearance];
    }
}

/// Normal/hover for the HUD's settings gear: a neutral ink over one faint wash, matching the other
/// floating panels' corner glyph buttons.
unsafe fn set_settings_button_style(button: *mut AnyObject, hovered: bool) {
    // The shared corner-glyph recipe (was the raw hover token, which omits the per-mode tuning).
    crate::ui::corner_glyph::set_glyph_button_style(button, crate::theme::ui_palette(), hovered);
}

/// Build the HUD's settings gear. Returns a +1 reference; the caller adds it to the backdrop and
/// releases it.
unsafe fn make_settings_button(frame: NSRect) -> *mut AnyObject {
    let button: *mut AnyObject = msg_send![settings_button_class(), alloc];
    let button: *mut AnyObject = msg_send![button, initWithFrame: frame];
    let _: () = msg_send![button, setBordered: false];
    let _: () = msg_send![button, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![button, layer];
    if !layer.is_null() {
        let _: () = msg_send![layer, setCornerRadius: crate::theme::RADIUS_CONTROL];
        let _: () = msg_send![layer, setMasksToBounds: true];
    }
    let symbol_name = make_nsstring("gearshape");
    let image: *mut AnyObject = msg_send![
        class!(NSImage),
        imageWithSystemSymbolName: symbol_name,
        accessibilityDescription: std::ptr::null::<AnyObject>()
    ];
    CFRelease(symbol_name as *const c_void);
    if !image.is_null() {
        let config: *mut AnyObject = msg_send![
            class!(NSImageSymbolConfiguration),
            configurationWithPointSize: crate::theme::FONT_CAPTION,
            weight: crate::theme::FONT_WEIGHT_REGULAR
        ];
        let image: *mut AnyObject = msg_send![image, imageWithSymbolConfiguration: config];
        let _: () = msg_send![image, setTemplate: true];
        let _: () = msg_send![button, setImage: image];
    }
    let empty = make_nsstring("");
    let _: () = msg_send![button, setTitle: empty];
    CFRelease(empty as *const c_void);
    let _: () = msg_send![button, setTarget: button];
    let _: () = msg_send![button, setAction: sel!(onKeyPanelSettings:)];
    set_settings_button_style(button, false);
    let opts: u64 = 0x01 | 0x80;
    let tracking: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
    let tracking: *mut AnyObject = msg_send![
        tracking,
        initWithRect: NSRect::new(NSPoint::new(0.0, 0.0), frame.size),
        options: opts,
        owner: button,
        userInfo: std::ptr::null::<AnyObject>()
    ];
    let _: () = msg_send![button, addTrackingArea: tracking];
    release_obj(tracking);
    let label = make_nsstring(&crate::i18n::t("keystroke_display.open_settings"));
    let _: () = msg_send![button, setAccessibilityLabel: label];
    CFRelease(label as *const c_void);
    let tooltip = make_nsstring(&crate::i18n::t("keystroke_display.open_settings"));
    let _: () = msg_send![button, setToolTip: tooltip];
    CFRelease(tooltip as *const c_void);
    button
}

unsafe fn create_panel(
    orientation: Orientation,
) -> (
    *mut AnyObject,
    *mut AnyObject,
    *mut AnyObject,
    *mut AnyObject,
) {
    // A provisional non-empty frame: `render` assigns the real size immediately after creation.
    let provisional = if orientation.is_vertical() {
        NSSize::new(PANEL_H, 64.0)
    } else {
        NSSize::new(64.0, PANEL_H)
    };
    let panel: *mut AnyObject = msg_send![class!(NSPanel), alloc];
    let panel: *mut AnyObject = msg_send![
        panel,
        initWithContentRect: NSRect::new(NSPoint::new(0.0, 0.0), provisional),
        styleMask: (1u64 << 7),
        backing: 2u64,
        defer: false
    ];
    // Before any content is built: the gear's dynamic ink resolves against this appearance.
    apply_panel_appearance(panel);
    let _: () = msg_send![panel, setLevel: 3isize];
    let _: () = msg_send![panel, setOpaque: false];
    // The HUD keeps its native window shadow: it is placed `PANEL_EDGE_MARGIN` (18pt) from the visible
    // edge, and a `high`-level shadow needs ~64-76pt of window padding around the panel, which AppKit then
    // constrains back into the visible area -- measured, that moved the panel by 26pt and changed the
    // meaning of a saved position. Giving the HUD a carrier shadow is therefore a placement decision of its
    // own, not part of the switcher/clipboard change (see docs/review-backlog.md).
    let _: () = msg_send![panel, setHasShadow: true];
    let _: () = msg_send![panel, setIgnoresMouseEvents: true];
    let _: () = msg_send![panel, setHidesOnDeactivate: false];
    let _: () = msg_send![panel, setReleasedWhenClosed: false];
    let _: () = msg_send![panel, setCollectionBehavior: ((1u64 << 0) | (1u64 << 6) | (1u64 << 8))];
    let clear: *mut AnyObject = msg_send![class!(NSColor), clearColor];
    let _: () = msg_send![panel, setBackgroundColor: clear];
    let local_frame = NSRect::new(NSPoint::new(0.0, 0.0), provisional);
    let backdrop = crate::glass::install_backdrop(
        panel,
        local_frame,
        crate::glass::PANEL_CORNER_RADIUS,
        crate::glass::BackdropOptions::new(Some(crate::glass::INACTIVE_GLASS_COMPENSATION_ALPHA)),
    );
    let badge_container: *mut AnyObject = msg_send![class!(NSView), alloc];
    let badge_container: *mut AnyObject = msg_send![badge_container, initWithFrame: local_frame];
    let _: () = msg_send![badge_container, setWantsLayer: true];
    let _: () = msg_send![badge_container, setAutoresizingMask: 18u64];
    let _: () = msg_send![backdrop.content_parent, addSubview: badge_container];
    let grip_view: *mut AnyObject = msg_send![grip_view_class(), alloc];
    let grip_view: *mut AnyObject =
        msg_send![grip_view, initWithFrame: grip_frame(orientation, provisional)];
    let _: () = msg_send![grip_view, setWantsLayer: true];
    let grip_layer: *mut AnyObject = msg_send![grip_view, layer];
    layer_set_background(grip_layer, hex_to_cg_color(0x00000000));
    for _ in 0..3 {
        let mark: *mut AnyObject = msg_send![class!(NSView), alloc];
        let mark: *mut AnyObject = msg_send![mark, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(GRIP_MARK_WIDTH, GRIP_MARK_HEIGHT))];
        let _: () = msg_send![mark, setWantsLayer: true];
        let mark_layer: *mut AnyObject = msg_send![mark, layer];
        let _: () = msg_send![mark_layer, setCornerRadius: (GRIP_MARK_HEIGHT / 2.0)];
        let _: () = msg_send![grip_view, addSubview: mark];
        release_obj(mark);
    }
    let _: () = msg_send![backdrop.content_parent, addSubview: grip_view];
    update_grip_marker(grip_view, orientation);
    // The trailing settings button shares the panel with the keycap stream; its reserve is already
    // taken off the stream budget, so it can never overlap the last keycap.
    let settings_view: *mut AnyObject =
        make_settings_button(settings_button_frame(orientation, provisional));
    let _: () = msg_send![backdrop.content_parent, addSubview: settings_view];
    release_obj(settings_view);
    *PANEL_BACKDROP.lock().unwrap() = Some(backdrop);
    release_obj(badge_container);
    release_obj(grip_view);
    // Paint (and record) the surface the palette calls for right away: the panel may be created while
    // no key is pressed, and until a render or a theme refresh runs the surface would otherwise be
    // whatever the material installed.
    apply_glass_properties();
    (panel, badge_container, grip_view, settings_view)
}

/// Side of the key panel's trailing settings button.
const SETTINGS_BTN_SIZE: f64 = crate::theme::FOOTER_CONTROL_SIZE;
/// Gap kept between the last keycap and the settings button along the stream axis.
const SETTINGS_BTN_GAP: f64 = crate::theme::FOOTER_CONTROL_GAP;
/// Distance the settings button keeps from the panel's trailing edge on the cross axis.
const SETTINGS_BTN_CROSS_INSET: f64 = PANEL_SIDE_PADDING;

/// Stream-axis distance from the panel's trailing edge to where the keycap stream must stop: the
/// settings button's own cross inset, its width, and the gap that keeps the last keycap off it.
/// The inset is the part the earlier version forgot -- reserving only the button and gap left the
/// last keycap flush against the button (a visible gap of 0 while SETTINGS_BTN_GAP promised one).
pub(super) const TAIL_SETTINGS_RESERVE: f64 =
    SETTINGS_BTN_CROSS_INSET + SETTINGS_BTN_SIZE + SETTINGS_BTN_GAP;
/// Stream-axis length the leading drag handle removes from the keycap stream. The handle's hit area
/// is thicker than the side padding, so it intrudes into the content area and must be reserved, or
/// the first keycap sits under the pointer that opens the drag.
pub(super) const HEAD_GRIP_RESERVE: f64 = GRIP_HIT_THICKNESS + GRIP_EDGE_INSET;

/// Stream-axis gap kept between the handle's hit area and the first keycap, mirroring
/// `SETTINGS_BTN_GAP` so both ends breathe by the same amount.
const GRIP_STREAM_GAP: f64 = SETTINGS_BTN_GAP;
/// Stream-axis distance from the panel's leading edge to where the keycap stream must start: the
/// handle's hit area plus the gap above. The trailing end uses its own `TAIL_SETTINGS_RESERVE`
/// instead -- the gear needs more room than the three dots, and reserving the larger of the two on
/// BOTH sides is what used to leave 24pt of blank space beyond the handle.
pub(super) const HEAD_STREAM_RESERVE: f64 = HEAD_GRIP_RESERVE + GRIP_STREAM_GAP;

/// The stream-axis budget deduction: both ends' reserves, less the `PANEL_SIDE_PADDING` per end
/// that `estimated_stream_width` already counts, so a stream trimmed to the budget exactly fills
/// the content area between the two reserves.
pub(super) const STREAM_BUDGET_DEDUCTION: f64 =
    HEAD_STREAM_RESERVE + TAIL_SETTINGS_RESERVE - PANEL_SIDE_PADDING * 2.0;

/// The corner-safe inset for the gear on the stream's trailing edge, in the direction that
/// edge faces: horizontal for a row, vertical for a column.
///
/// A column's trailing edge is the bottom, so the frame is the row's rotated 90 degrees: the
/// panel's width becomes the extent the control is centred ACROSS. That cross extent is what the
/// [`crate::theme::footer_control_inset`]'s `band_h` argument means, so passing the panel's height
/// (the flow extent) there collapses the centring term and silently moves the control. A panel has
/// no separate footer band, unlike the switcher's overlay, which is why the rotation is adapted
/// here instead of widening the shared function.
fn trailing_edge_inset(orientation: Orientation, panel_size: NSSize, control: f64) -> f64 {
    let radius = crate::glass::PANEL_CORNER_RADIUS;
    if orientation.is_vertical() {
        crate::theme::footer_control_inset(
            radius,
            panel_size.height,
            panel_size.width,
            control,
            panel_size.width,
        )
    } else {
        crate::theme::footer_control_inset(
            radius,
            panel_size.width,
            panel_size.height,
            control,
            panel_size.height,
        )
    }
}

/// Where the settings button sits inside the panel: the stream's TRAILING end -- the trailing edge
/// of a row, the bottom of a column -- which is the corner a user reaches for.
fn settings_button_frame(orientation: Orientation, panel_size: NSSize) -> NSRect {
    if orientation.is_vertical() {
        // Column: the stream runs downward, so the trailing end is the bottom edge.
        let inset = trailing_edge_inset(orientation, panel_size, SETTINGS_BTN_SIZE);
        let m = SETTINGS_BTN_CROSS_INSET.max(inset);
        NSRect::new(
            NSPoint::new(((panel_size.width - SETTINGS_BTN_SIZE) / 2.0).max(0.0), m),
            NSSize::new(SETTINGS_BTN_SIZE, SETTINGS_BTN_SIZE),
        )
    } else {
        // Row: the stream runs rightward, so the trailing end is the right edge.
        let inset = trailing_edge_inset(orientation, panel_size, SETTINGS_BTN_SIZE);
        let m = SETTINGS_BTN_CROSS_INSET.max(inset);
        NSRect::new(
            NSPoint::new(
                (panel_size.width - m - SETTINGS_BTN_SIZE).max(0.0),
                ((panel_size.height - SETTINGS_BTN_SIZE) / 2.0).max(0.0),
            ),
            NSSize::new(SETTINGS_BTN_SIZE, SETTINGS_BTN_SIZE),
        )
    }
}

/// The drag handle's frame inside the panel. A row carries it as a vertical dotted bar on its
/// leading edge; a column carries it as a horizontal dotted bar along its top edge, so it always
/// sits on the side the panel is anchored to.
fn grip_frame(orientation: Orientation, panel_size: NSSize) -> NSRect {
    if orientation.is_vertical() {
        NSRect::new(
            NSPoint::new(
                (panel_size.width - GRIP_HIT_LENGTH) / 2.0,
                (panel_size.height - GRIP_HIT_THICKNESS - GRIP_EDGE_INSET).max(0.0),
            ),
            NSSize::new(GRIP_HIT_LENGTH, GRIP_HIT_THICKNESS),
        )
    } else {
        NSRect::new(
            NSPoint::new(0.0, (panel_size.height - GRIP_HIT_LENGTH) / 2.0),
            NSSize::new(GRIP_HIT_THICKNESS, GRIP_HIT_LENGTH),
        )
    }
}

fn grip_marker_frame(orientation: Orientation, index: usize) -> NSRect {
    if orientation.is_vertical() {
        let total_width = 3.0 * GRIP_MARK_HEIGHT + 2.0 * GRIP_MARK_GAP;
        let y = GRIP_HIT_THICKNESS - GRIP_MARK_CENTER_INSET - GRIP_MARK_HEIGHT / 2.0;
        let start_x = (GRIP_HIT_LENGTH - total_width) / 2.0;
        NSRect::new(
            NSPoint::new(
                start_x + index as f64 * (GRIP_MARK_HEIGHT + GRIP_MARK_GAP),
                y,
            ),
            NSSize::new(GRIP_MARK_HEIGHT, GRIP_MARK_HEIGHT),
        )
    } else {
        let total_height = 3.0 * GRIP_MARK_HEIGHT + 2.0 * GRIP_MARK_GAP;
        let x = GRIP_MARK_CENTER_INSET - GRIP_MARK_WIDTH / 2.0;
        let start_y = (GRIP_HIT_LENGTH - total_height) / 2.0;
        NSRect::new(
            NSPoint::new(
                x,
                start_y + index as f64 * (GRIP_MARK_HEIGHT + GRIP_MARK_GAP),
            ),
            NSSize::new(GRIP_MARK_WIDTH, GRIP_MARK_HEIGHT),
        )
    }
}

unsafe fn update_grip_marker(grip_view: *mut AnyObject, orientation: Orientation) {
    let marks: *mut AnyObject = msg_send![grip_view, subviews];
    let count: usize = msg_send![marks, count];
    let mark_color = hex_to_cg_color(crate::theme::ui_palette().secondary_text);
    for index in 0..count.min(3) {
        let mark: *mut AnyObject = msg_send![marks, objectAtIndex: index as isize];
        let _: () = msg_send![mark, setFrame: grip_marker_frame(orientation, index)];
        let layer: *mut AnyObject = msg_send![mark, layer];
        layer_set_background(layer, mark_color);
    }
}

/// What the pointer is over inside the key panel. One decision function answers this, so the window's
/// input state is written from exactly ONE place -- two pollers each calling
/// setIgnoresMouseEvents: would overwrite each other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PanelControl {
    None,
    Grip,
    Settings,
}

/// Which control, if any, the cursor is over. `captured` is an in-flight gesture (a grip drag or a
/// settings press): while one runs, the panel keeps receiving input wherever the pointer goes, or a
/// mouseUp delivered outside the control could never finish the gesture.
fn panel_control_under_cursor(
    state: &PanelState,
    cursor_point: Option<NSPoint>,
    captured: bool,
) -> PanelControl {
    if cursor_point.is_none() {
        return PanelControl::None;
    }
    let point = cursor_point.unwrap();
    let Some(window) = state.panel else {
        return PanelControl::None;
    };
    let frame: NSRect = unsafe { crate::glass::panel_frame_of(window) };
    if captured {
        // A gesture in flight keeps input enabled; the control it started on is what to report.
        if state.drag.is_some() {
            return PanelControl::Grip;
        }
        if state.settings_armed {
            return PanelControl::Settings;
        }
    }
    let local = panel_control_rects(state.orientation, frame.size);
    for (control, rect) in [
        (PanelControl::Settings, local.settings),
        (PanelControl::Grip, local.grip),
    ] {
        let global = NSRect::new(
            NSPoint::new(
                frame.origin.x + rect.origin.x,
                frame.origin.y + rect.origin.y,
            ),
            rect.size,
        );
        if contains(global, point) {
            return control;
        }
    }
    PanelControl::None
}

/// The two controls' rects in panel coordinates, computed together so the input decision and the
/// renderer can never disagree about where they are.
struct PanelControlRects {
    grip: NSRect,
    settings: NSRect,
}

/// Pure: the caller passes the orientation it already has from the borrowed state. It must NOT
/// reach into PANEL itself -- render and the tracking path call this while already holding
/// PANEL.borrow_mut(), and a second borrow there panics (RefCell) on the ordinary in-panel cursor
/// path, which is the common case. Both ends report the SAME symmetric reserve, which is what keeps
/// the stream centred while still clearing the trailing button.
fn panel_control_rects(orientation: Orientation, panel_size: NSSize) -> PanelControlRects {
    PanelControlRects {
        grip: grip_frame(orientation, panel_size),
        settings: settings_button_frame(orientation, panel_size),
    }
}

fn update_grip_tracking(state: &mut PanelState, cursor_point: Option<NSPoint>) {
    if state.drag.is_some() {
        set_grip_mouse_state(state, true, GripCursor::Closed);
        return;
    }
    let panel_active = state.visible || state.fade_deadline.is_some();
    // The gesture-in-flight question is asked of the state itself, not of the pointer: a press that
    // left the control must keep receiving events until it finishes or is cancelled.
    let captured = state.drag.is_some() || state.settings_armed;
    let control = if panel_active {
        panel_control_under_cursor(state, cursor_point, captured)
    } else {
        PanelControl::None
    };
    // ONE write of the window's input state, driven by the single decision above.
    let (receive, cursor) = match control {
        PanelControl::Grip => (
            true,
            if state.drag.is_some() {
                GripCursor::Closed
            } else {
                GripCursor::Open
            },
        ),
        PanelControl::Settings => (true, GripCursor::Pointing),
        PanelControl::None => (false, GripCursor::None),
    };
    set_grip_mouse_state(state, receive, cursor);
}

fn set_grip_mouse_state(state: &mut PanelState, receive_events: bool, cursor: GripCursor) {
    // NSWindow input is window-wide, so polling opens it only over the grip and a mouse-down
    // keeps it open for the duration of the captured drag.
    if let Some(panel) = state.panel {
        unsafe {
            let _: () = msg_send![panel, setIgnoresMouseEvents: !receive_events];
        }
    }
    set_grip_cursor(state, cursor);
}

fn set_grip_cursor(state: &mut PanelState, cursor: GripCursor) {
    if state.grip_cursor == cursor {
        return;
    }
    unsafe {
        if state.grip_cursor != GripCursor::None {
            let _: () = msg_send![class!(NSCursor), pop];
        }
        if cursor != GripCursor::None {
            let cursor_object: *mut AnyObject = match cursor {
                GripCursor::Open => msg_send![class!(NSCursor), openHandCursor],
                GripCursor::Closed => msg_send![class!(NSCursor), closedHandCursor],
                GripCursor::Pointing => msg_send![class!(NSCursor), pointingHandCursor],
                GripCursor::None => std::ptr::null_mut(),
            };
            if !cursor_object.is_null() {
                let _: () = msg_send![cursor_object, push];
            }
        }
    }
    state.grip_cursor = cursor;
}

pub(super) fn current_cursor_appkit_point() -> Option<NSPoint> {
    unsafe {
        let event = event_tap::CGEventCreate(std::ptr::null());
        if event.is_null() {
            return None;
        }
        let point = event_tap::CGEventGetLocation(event);
        CFRelease(event as *const c_void);
        let screens = screen_geometries();
        let primary = screens.first()?.frame;
        Some(NSPoint::new(
            point.x,
            primary.origin.y + primary.size.height - point.y,
        ))
    }
}

fn installed_backdrop() -> Option<crate::glass::InstalledBackdrop> {
    *PANEL_BACKDROP.lock().unwrap()
}

unsafe fn backdrop_structure_valid(panel: *mut AnyObject, badge_container: *mut AnyObject) -> bool {
    let Some(backdrop) = installed_backdrop() else {
        return false;
    };
    let root: *mut AnyObject = msg_send![panel, contentView];
    if let Some(glass) = backdrop.glass {
        let Some(glass_class) = objc2::runtime::AnyClass::get(c"NSGlassEffectView") else {
            return false;
        };
        // The glass carries the rounded clip itself.
        let glass_is_hosted: bool = view_contains_subview(root, glass.0);
        let is_glass: bool = msg_send![root, isKindOfClass: glass_class];
        let radius: f64 = msg_send![glass.0, cornerRadius];
        let glass_layer: *mut AnyObject = msg_send![glass.0, layer];
        let glass_clips: bool = !glass_layer.is_null() && msg_send![glass_layer, masksToBounds];
        // Whatever hosts the glass must not **clip**. The panel's top edge was once cut off by a plain,
        // masking container between the window and the glass, because the glass's variants draw their edge
        // outside the glass's own bounds. This check used to read the host's *class* instead (the glass
        // itself, or a `NSVisualEffectView`), which was a proxy for that mask and rejected any non-masking
        // host -- including the shadow carrier, which deliberately does not clip exactly so the glass's edge
        // is not cut. The property the bug was about is asserted directly now, and directly is stricter: a
        // masking host fails whatever its class is, while a material view that rounds and clips *itself*
        // (frost) is judged on the host above it, which is where the bug lived.
        let host_clips: bool = if is_glass {
            false
        } else {
            let root_layer: *mut AnyObject = msg_send![root, layer];
            !root_layer.is_null() && msg_send![root_layer, masksToBounds]
        };
        let wrapper_ok: bool = if crate::dev_flags::value("glass-blur").is_some() {
            // The development-only blur underlay hosts its `CABackdropLayer` on a plain container that
            // insets the glass; that shape never ships (`build_backdrop`).
            true
        } else {
            !host_clips
        };
        let inner: *mut AnyObject = msg_send![glass.0, contentView];
        let Some(fill) = backdrop.compensation_view else {
            return false;
        };
        let fill_layer = backdrop.compensation_layer;
        let Some(fill_layer) = fill_layer else {
            return false;
        };
        let fill_is_child = view_contains_subview(inner, fill.0);
        let badges_are_child = view_contains_subview(inner, badge_container);
        (is_glass || glass_is_hosted)
            && glass_clips
            && wrapper_ok
            && (radius - crate::glass::PANEL_CORNER_RADIUS).abs() < 0.01
            && !inner.is_null()
            && fill_is_child
            && badges_are_child
            && !fill_layer.0.is_null()
    } else if let Some(host) = backdrop.backdrop_view {
        // The blur-only surface: the window hosts the rounding root, and the child view below the content
        // must really carry a `CABackdropLayer` with filters on it -- a surface that only *looks* like it
        // has a blur would otherwise pass.
        let root_layer: *mut AnyObject = msg_send![root, layer];
        let radius: f64 = if root_layer.is_null() {
            -1.0
        } else {
            msg_send![root_layer, cornerRadius]
        };
        let host_layer: *mut AnyObject = msg_send![host.0, layer];
        let mut carries_blur = false;
        if !host_layer.is_null() {
            let sublayers: *mut AnyObject = msg_send![host_layer, sublayers];
            if !sublayers.is_null() {
                let count: usize = msg_send![sublayers, count];
                if count > 0 {
                    let first: *mut AnyObject = msg_send![sublayers, objectAtIndex: 0usize];
                    let class_name =
                        std::ffi::CStr::from_ptr(objc2::ffi::object_getClassName(first));
                    let filters: *mut AnyObject = msg_send![first, filters];
                    let filter_count: usize = if filters.is_null() {
                        0
                    } else {
                        msg_send![filters, count]
                    };
                    carries_blur =
                        class_name.to_string_lossy() == "CABackdropLayer" && filter_count >= 1;
                }
            }
        }
        let host_is_child = view_contains_subview(root, host.0);
        // The app's own contract for every material: the panel puts its content in `content_parent`, and
        // that in turn sits inside the view the window hosts.
        let content_in_root = view_contains_subview(root, backdrop.content_parent);
        let badges_ok = view_contains_subview(backdrop.content_parent, badge_container);
        let radius_ok = (radius - crate::glass::PANEL_CORNER_RADIUS).abs() < 0.01;
        if !(host_is_child && content_in_root && badges_ok && radius_ok && carries_blur) {
            crate::log_info!(
                "[smoke-backdrop] host_is_child={host_is_child} content_in_root={content_in_root} badges={badges_ok} radius={radius} radius_ok={radius_ok} blur={carries_blur}"
            );
        }
        host_is_child && content_in_root && badges_ok && host.0 != root && radius_ok && carries_blur
    } else if let Some(effect) = backdrop.effect_view {
        let Some(effect_class) = objc2::runtime::AnyClass::get(c"NSVisualEffectView") else {
            return false;
        };
        let is_effect: bool = msg_send![effect.0, isKindOfClass: effect_class];
        let effect_layer: *mut AnyObject = msg_send![effect.0, layer];
        let radius: f64 = msg_send![effect_layer, cornerRadius];
        // The panel keeps the system blur as its surface; the keycaps carry the text (see
        // `neutral_keycap_surface`), and nothing else is drawn on it.
        //
        // Every material installs its root view as the window's content view directly.
        is_effect
            && effect.0 == root
            && view_contains_subview(effect.0, badge_container)
            && (radius - crate::glass::PANEL_CORNER_RADIUS).abs() < 0.01
    } else if let Some(plain) = backdrop.opaque_view {
        let plain_layer: *mut AnyObject = msg_send![plain.0, layer];
        let radius: f64 = msg_send![plain_layer, cornerRadius];
        plain.0 == root
            && view_contains_subview(plain.0, badge_container)
            && (radius - crate::glass::PANEL_CORNER_RADIUS).abs() < 0.01
    } else {
        false
    }
}

unsafe fn view_contains_subview(parent: *mut AnyObject, candidate: *mut AnyObject) -> bool {
    let subviews: *mut AnyObject = msg_send![parent, subviews];
    let count: usize = msg_send![subviews, count];
    (0..count).any(|index| {
        let child: *mut AnyObject = msg_send![subviews, objectAtIndex: index as isize];
        child == candidate
    })
}

/// Whether the panel window runs the vibrant appearance for the *resolved* mode -- the one that makes
/// the gear's dynamic ink resolve to a light glyph on the dark palette.
///
/// Read back from the window's `effectiveAppearance`, not from the constant the setter passed: the
/// defect this guards was a window that kept the app's inherited (Light) appearance while the panel
/// painted the dark palette, which made the gear a near-black glyph on a dark surface. Comparing the
/// constant would pass with the setter never having run.
pub(super) unsafe fn panel_appearance_matches_mode() -> bool {
    PANEL.with(|panel| {
        let state = panel.borrow();
        let Some(window) = state.panel else {
            return false;
        };
        let appearance: *mut AnyObject = msg_send![window, effectiveAppearance];
        if appearance.is_null() {
            return false;
        }
        let name: *mut AnyObject = msg_send![appearance, name];
        if name.is_null() {
            return false;
        }
        let actual = crate::ffi::nsstring_to_rust(name);
        let expected = crate::glass::vibrant_appearance_name(crate::theme::resolved_is_dark());
        if actual != expected {
            eprintln!(
                "[keystroke-display-smoke] panel appearance is {actual}, expected {expected} for dark={}",
                crate::theme::resolved_is_dark()
            );
        }
        actual == expected
    })
}

pub(super) unsafe fn apply_glass_properties() {
    // The window's appearance decides what a dynamic panel ink (the gear) resolves to, so it follows
    // every palette repaint -- including the "auto" theme's system-appearance entry point, which does
    // not run the config-change coordinator (see `apply_panel_appearance`).
    //
    // `try_borrow`, not `borrow`: `render` holds this state mutably for its whole body and `create_panel`
    // (which calls here) runs inside it, where the window is already being styled explicitly. A hard
    // borrow there would panic on the first summon.
    let window = PANEL.with(|panel| panel.try_borrow().ok().and_then(|state| state.panel));
    if let Some(window) = window {
        apply_panel_appearance(window);
    }
    let Some(backdrop) = installed_backdrop() else {
        return;
    };
    crate::glass::apply_live_properties(
        backdrop,
        crate::glass::BackdropOptions::new(Some(crate::glass::INACTIVE_GLASS_COMPENSATION_ALPHA)),
    );
}

/// The frost material's blur is composited by the window server and ignores layer
/// clipping: without the rounded maskImage the square blur bleeds past the corners (white
/// corner frames over light backgrounds). The smoke asserts the mask exists on frost.
unsafe fn backdrop_frost_mask_valid() -> bool {
    let Some(backdrop) = installed_backdrop() else {
        return false;
    };
    let Some(effect) = backdrop.effect_view else {
        return !matches!(backdrop.material, crate::glass::PanelMaterial::Frost);
    };
    let is_frost: bool = msg_send![effect.0, isKindOfClass: class!(NSVisualEffectView)];
    if !is_frost {
        return true;
    }
    let mask: *mut AnyObject = msg_send![effect.0, maskImage];
    !mask.is_null()
}

/// Whether the installed frost carries the light-mode floor veil exactly when it should.
///
/// The veil is added and dropped by the theme path and excluded from a material swap, and each of those was
/// a real defect: a stale veil in dark mode, or two of them after a theme flip or a round trip, is what this
/// fails on. Its colour is pinned by the pure `glass::floor_veil_hex` test instead.
unsafe fn backdrop_floor_veil_valid() -> bool {
    let Some(backdrop) = installed_backdrop() else {
        return false;
    };
    let Some(effect) = backdrop.effect_view else {
        // No effect view means this installation is not frost; the veil is a frost-only decoration.
        return !matches!(backdrop.material, crate::glass::PanelMaterial::Frost);
    };
    let wanted = !crate::theme::resolved_is_dark() && crate::config::effective_glass_floor() > 0.0;
    crate::glass::floor_veil_count(effect.0) == usize::from(wanted)
}

/// Whether the installed effect view carries the pinned frost material for the *resolved* mode.
///
/// Read back from the view's own `material`, not from the constant the builder passed: the light side
/// shipped `underWindowBackground` (21), which only the rendered panel showed to be a shade dark, so the
/// pin has to be asserted on what is installed. The theme path re-sets the material in
/// `glass::apply_live_properties`, and the surface checks below call this so a refresh that skipped it
/// fails here instead of on screen.
unsafe fn backdrop_frost_material_matches() -> bool {
    let Some(backdrop) = installed_backdrop() else {
        return false;
    };
    let Some(effect) = backdrop.effect_view else {
        // No effect view means this installation is not frost; the material read-back is a frost check.
        return !matches!(backdrop.material, crate::glass::PanelMaterial::Frost);
    };
    let installed: i64 = msg_send![effect.0, material];
    installed == crate::glass::frost_material()
}

/// Whether the panel's surface currently carries the *current* theme's scrim / tint.
///
/// The surface is written by `apply_glass_properties` from the palette, while the keycaps re-read the
/// palette on every render. A theme change that refreshed one and not the other produced light keycaps
/// on a dark shell, so the theme refresh must reach this panel too
/// (`ui_coordinator::apply_theme_and_locale_refresh`). The smoke asserts this after switching themes.
///
/// This panel owns its text surfaces (keycaps carry `card_bg` fills), so its material is deliberately
/// left unwashed: the frost case asserts the *absence* of the theme wash, which is what makes the
/// material the user selected actually visible here.
unsafe fn backdrop_surface_matches_palette() -> bool {
    let Some(backdrop) = installed_backdrop() else {
        return false;
    };
    match (expected_surface(backdrop.material), backdrop.material) {
        // Frost paints no surface of its own: the reader's surface is the system blur, and the text
        // rides on the keycap chips. The blur's structure is asserted separately
        // (`backdrop_frost_mask_valid`), which is what can actually break here.
        (Some(SurfaceExpectation::SystemBlur), crate::glass::PanelMaterial::Frost) => {
            // A live effect view is the structure; the pinned material is the look, and only reading it
            // back catches a build or theme-refresh path that installs the retired value (21).
            backdrop.effect_view.is_some() && backdrop_frost_material_matches()
        }
        (Some(SurfaceExpectation::SystemBlur), _) => false,
        (Some(SurfaceExpectation::Layer(expected)), _) => backdrop
            .opaque_view
            .is_some_and(|view| layer_background_matches(msg_send![view.0, layer], expected)),
        (Some(SurfaceExpectation::GlassTint(expected)), _) => backdrop
            .glass
            .is_some_and(|glass| glass_tint_matches(glass.0, expected)),
        (None, _) => false,
    }
}

/// The surface the current theme and material call for, read from the palette.
fn expected_surface(material: crate::glass::PanelMaterial) -> Option<SurfaceExpectation> {
    match material {
        // Both blurring surfaces that the app does not paint: the system's material, and the blur-only
        // backdrop layer (whose own layer carries the blur filters, checked separately).
        crate::glass::PanelMaterial::Frost | crate::glass::PanelMaterial::Backdrop => {
            Some(SurfaceExpectation::SystemBlur)
        }
        crate::glass::PanelMaterial::Opaque => Some(SurfaceExpectation::Layer(
            crate::theme::ui_palette().window_bg,
        )),
        // The glass surface is the system view plus the user's tint: the tint is a palette-free
        // value, and it is the only part of that surface this app sets.
        crate::glass::PanelMaterial::LiquidGlass => Some(SurfaceExpectation::GlassTint(
            crate::glass::resolved_glass_tint_hex(),
        )),
    }
}

/// What a panel surface is expected to hold, per material.
#[derive(Clone, Copy, Debug, PartialEq)]
enum SurfaceExpectation {
    /// The system's own blur is the surface (frost): the app paints nothing, so there is no layer
    /// token to compare. See `backdrop_surface_matches_palette`.
    SystemBlur,
    /// Frost wash / opaque background, as a palette token written to a CALayer.
    Layer(u32),
    /// The user's glass tint, as an RRGGBBAA token written to the system glass view.
    GlassTint(u32),
}

/// Read a CALayer's background CGColor and compare it with a token (RGB, 1/255 precision).
/// The color is fetched through raw FFI: objc2's `msg_send!` refuses `backgroundColor` (it returns a
/// CGColor, not an object pointer) and would trap.
unsafe fn layer_background_matches(layer: *mut AnyObject, expected: u32) -> bool {
    if layer.is_null() {
        return false;
    }
    let color = crate::ffi::layer_background_color(layer);
    if color.is_null() {
        return false;
    }
    let count = crate::ffi::CGColorGetNumberOfComponents(color);
    if count < 3 {
        return false;
    }
    let components = crate::ffi::CGColorGetComponents(color);
    if components.is_null() {
        return false;
    }
    let close = |value: f64, byte: u32| (value - (byte as f64 / 255.0)).abs() <= 1.5 / 255.0;
    close(*components, (expected >> 24) & 0xFF)
        && close(*components.add(1), (expected >> 16) & 0xFF)
        && close(*components.add(2), (expected >> 8) & 0xFF)
}

/// Read a CALayer's background CGColor and compare RGBA, alpha included.
///
/// Separate from [`layer_background_matches`] rather than replacing it: every existing caller
/// compares an opaque palette surface, where the alpha is always 1.0 and comparing it would add a
/// false failure mode. The corner-glyph wash is the one assertion whose entire point is an alpha
/// that differs from a candidate implementation's while the RGB is identical -- the raw palette fill
/// and the mode-tuned wash share their three bytes and differ only here -- so it needs the stricter
/// reader. Comparing RGB alone would pass against either, which is a gate that cannot decide.
unsafe fn layer_background_matches_rgba(layer: *mut AnyObject, expected: u32) -> bool {
    if layer.is_null() {
        return false;
    }
    let color = crate::ffi::layer_background_color(layer);
    if color.is_null() {
        return false;
    }
    // RGBA is what ffi::hex_to_cg_color produces (NSColor -> CGColor); a 3-component colour cannot
    // be the wash this compares against, so it is not a match.
    if crate::ffi::CGColorGetNumberOfComponents(color) < 4 {
        return false;
    }
    let components = crate::ffi::CGColorGetComponents(color);
    if components.is_null() {
        return false;
    }
    let close = |value: f64, byte: u32| (value - (byte as f64 / 255.0)).abs() <= 1.5 / 255.0;
    close(*components, (expected >> 24) & 0xFF)
        && close(*components.add(1), (expected >> 16) & 0xFF)
        && close(*components.add(2), (expected >> 8) & 0xFF)
        && close(*components.add(3), expected & 0xFF)
}

/// Read the system glass view's tint color and compare it with the configured tint token.
unsafe fn glass_tint_matches(glass: *mut AnyObject, expected: u32) -> bool {
    if glass.is_null() {
        return false;
    }
    let color: *mut AnyObject = msg_send![glass, tintColor];
    if color.is_null() {
        return false;
    }
    let (mut r, mut g, mut b, mut a) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    let _: () = msg_send![color, getRed: &mut r, green: &mut g, blue: &mut b, alpha: &mut a];
    let close = |value: f64, byte: u32| (value - (byte as f64 / 255.0)).abs() <= 1.5 / 255.0;
    close(r, (expected >> 24) & 0xFF)
        && close(g, (expected >> 16) & 0xFF)
        && close(b, (expected >> 8) & 0xFF)
}

/// Bring the panel's backdrop in line with the effective material. Called when the
/// panel-material setting changes and when the panel becomes visible; the swap reparents
/// the badge container and grip into the new hierarchy, so only the slot and surface
/// styles need refreshing afterwards.
pub(super) unsafe fn apply_backdrop_material() {
    let Some(panel) = PANEL.with(|panel| panel.borrow().panel) else {
        return;
    };
    let Some(old) = installed_backdrop() else {
        return;
    };
    if old.material == crate::glass::PanelMaterial::effective() {
        return;
    }
    // The panel rect, not the window's: the window is padded for the shadow, and the hierarchy a swap
    // builds is the material's, which is the panel.
    let panel_rect = crate::glass::panel_frame_of(panel);
    let new = crate::glass::swap_backdrop(
        panel,
        &old,
        panel_rect,
        crate::glass::PANEL_CORNER_RADIUS,
        crate::glass::BackdropOptions::new(Some(crate::glass::INACTIVE_GLASS_COMPENSATION_ALPHA)),
    );
    *PANEL_BACKDROP.lock().unwrap() = Some(new);
    apply_glass_properties();
}

unsafe fn set_alpha_immediately(panel: *mut AnyObject, alpha: f64) {
    let _: () = msg_send![panel, setAlphaValue: alpha];
}

/// A badge's measured footprint. `length` runs along the stream axis (left-to-right in a row,
/// top-to-bottom in a column) and `thickness` is the cross-axis size.
#[derive(Clone, Copy, Debug, PartialEq)]
struct BadgeExtent {
    length: f64,
    thickness: f64,
}

/// Every badge reaching this point is a lone keycap: chords were already split into one keycap
/// per key. Its natural size is the same in both orientations, but which of that size is "along
/// the stream" swaps: a row advances across its width, a column down its height. In a column the
/// merge count takes its own row below the glyph (the render mirrors it), so the height grows by
/// that row while the width stays the plain label's; in a row the count rides inline and widens
/// the cap instead.
fn cached_badge_extent(
    cache: &mut HashMap<String, f64>,
    badge: &Badge,
    orientation: Orientation,
) -> BadgeExtent {
    crate::debug_assert_main_thread();
    let width = cached_label_width(cache, &badge.text) + BADGE_HORIZONTAL_PADDING;
    if orientation.is_vertical() {
        BadgeExtent {
            length: BADGE_H + repeat_suffix_row(badge.repeats),
            thickness: width,
        }
    } else {
        let length = (cached_label_width(cache, &badge_display_text(&badge.text, badge.repeats))
            + BADGE_HORIZONTAL_PADDING)
            .max(BADGE_MIN_WIDTH);
        BadgeExtent {
            length,
            thickness: BADGE_H,
        }
    }
}

fn cached_label_width(cache: &mut HashMap<String, f64>, label: &str) -> f64 {
    if let Some(width) = cache.get(label) {
        return *width;
    }
    let width = unsafe { measure_text_width(label) };
    if cache.len() >= MAX_MEASUREMENTS {
        cache.clear();
    }
    cache.insert(label.to_string(), width);
    width
}

unsafe fn measure_text_width(text: &str) -> f64 {
    let value = make_nsstring(text);
    let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CONTROL, weight: crate::theme::FONT_WEIGHT_REGULAR];
    let attributes: *mut AnyObject =
        msg_send![class!(NSDictionary), dictionaryWithObject: font, forKey: NSFontAttributeName];
    let size: NSSize = msg_send![value, sizeWithAttributes: attributes];
    CFRelease(value as *const c_void);
    size.width.ceil()
}

fn inline_badge_label_start(badge_width: f64, label_width: f64, count_width: f64) -> f64 {
    (badge_width - label_width - count_width) / 2.0 - BADGE_INLINE_FIELD_SLACK_X / 2.0
}

fn badge_display_text(label: &str, repeats: u32) -> String {
    // No separator between glyph and count: "A×9", not "A ×9" -- the count is part of the
    // keycap's reading, and the separator only widened it.
    if repeats > 1 {
        format!("{label}×{repeats}")
    } else {
        label.to_string()
    }
}

/// Add one text field, vertically centered in `frame`.
unsafe fn add_badge_label(parent: *mut AnyObject, frame: NSRect, text: &str, text_color: u32) {
    let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CONTROL, weight: crate::theme::FONT_WEIGHT_REGULAR];
    let ascender: f64 = msg_send![font, ascender];
    let descender: f64 = msg_send![font, descender];
    let leading: f64 = msg_send![font, leading];
    let line_height = (ascender - descender + leading).ceil();
    // A badge-height NSTextField puts its glyph ink about 8 pt off-center; center a font-height
    // frame in the capsule instead.
    let field_y = frame.origin.y + ((frame.size.height - line_height) / 2.0).max(0.0);
    let field: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let field: *mut AnyObject = msg_send![field, initWithFrame: NSRect::new(NSPoint::new(frame.origin.x, field_y), NSSize::new(frame.size.width.max(1.0), line_height))];
    let _: () = msg_send![field, setEditable: false];
    let _: () = msg_send![field, setSelectable: false];
    let _: () = msg_send![field, setBezeled: false];
    let _: () = msg_send![field, setDrawsBackground: false];
    let _: () = msg_send![field, setAlignment: 1isize];
    let _: () = msg_send![field, setLineBreakMode: 4isize];
    let _: () = msg_send![field, setFont: font];
    let color = crate::ffi::hex_to_ns_color(text_color);
    let _: () = msg_send![field, setTextColor: color];
    let title = make_nsstring(text);
    let _: () = msg_send![field, setStringValue: title];
    CFRelease(title as *const c_void);
    let _: () = msg_send![parent, addSubview: field];
    release_obj(field);
}

/// The panel's stream-axis length when it sizes to its content: the keycap stream plus each end's
/// own reserve. `desired` is the stream's estimated width, which already includes
/// `PANEL_SIDE_PADDING` at both ends; the reserves cover that padding, so it is subtracted here
/// rather than counted a second time (which is what left 12pt of extra blank per end).
fn content_sized_panel_length(desired: f64) -> f64 {
    desired - PANEL_SIDE_PADDING * 2.0 + HEAD_STREAM_RESERVE + TAIL_SETTINGS_RESERVE
}

/// Where the keycap stream starts along the panel's stream axis.
///
/// The stream is centred in the CONTENT area -- the panel minus the leading drag handle's reserve and
/// the trailing settings button's reserve -- not in the whole panel. Centring it in the whole panel
/// put the last keycap exactly on the gear's edge (a visible gap of 0) even though
/// SETTINGS_BTN_GAP is part of the reserve, because the reserve was subtracted from the budget but
/// not from the space the stream was laid out in.
///
/// In non-flipped coordinates a row is measured from the leading (left) edge while a column is
/// counted down from the top, so a column returns the content area's trailing edge.
fn centered_stream_cursor(
    panel_length: f64,
    estimated_total: f64,
    vertical: bool,
    head_reserve: f64,
    tail_reserve: f64,
) -> f64 {
    let stream_length = (estimated_total - PANEL_SIDE_PADDING * 2.0).max(0.0);
    let content_length = (panel_length - head_reserve - tail_reserve).max(0.0);
    let margin = ((content_length - stream_length) / 2.0).max(0.0);
    if vertical {
        // Non-flipped: a column counts DOWN from the top, and the handle sits at the top
        // (leading) edge while the settings button sits at the bottom (trailing) one.
        panel_length - head_reserve - margin
    } else {
        head_reserve + margin
    }
}

/// The stream-axis reserve each end applies: the leading handle's own need and the trailing gear's
/// own need. The two differ, so the keycaps sit nearer the handle than the gear; sizing each end
/// from its own control keeps the stream on the reserve line instead of carrying the gear's larger
/// reserve as blank space at the handle. Both ends' reserves come off the panel's length and the
/// stream budget alike, so the last keycap keeps `SETTINGS_BTN_GAP` from the gear.
fn stream_axis_reserves(_orientation: Orientation, _panel_size: NSSize) -> (f64, f64) {
    (HEAD_STREAM_RESERVE, TAIL_SETTINGS_RESERVE)
}

unsafe fn rebuild_badges(
    content: *mut AnyObject,
    badges: &[Badge],
    labels: &[String],
    extents: &[BadgeExtent],
    panel_size: NSSize,
    orientation: Orientation,
    // The caller already resolved the palette to decide whether a rebuild was needed; resolving
    // it again here doubled the CONFIG read (and, under theme=auto, the NSUserDefaults query) on
    // every render tick that rebuilt.
    palette: &crate::theme::UiPalette,
) {
    let old: *mut AnyObject = msg_send![content, subviews];
    let count: usize = msg_send![old, count];
    for index in (0..count).rev() {
        let view: *mut AnyObject = msg_send![old, objectAtIndex: index as isize];
        let _: () = msg_send![view, removeFromSuperview];
    }

    let total = estimated_stream_width(extents.iter().map(|extent| extent.length));
    // `total` includes nominal outer padding; center the actual keycap-and-gap stream so
    // any extra room in the panel is split evenly instead of being added to only one end.
    let panel_length = if orientation.is_vertical() {
        panel_size.height
    } else {
        panel_size.width
    };
    // Non-flipped coordinates count a column down from the top and a row up from the left.
    let (head_reserve, tail_reserve) = stream_axis_reserves(orientation, panel_size);
    let mut cursor = centered_stream_cursor(
        panel_length,
        total,
        orientation.is_vertical(),
        head_reserve,
        tail_reserve,
    );
    for ((badge, label), extent) in badges.iter().zip(labels).zip(extents) {
        // `length` runs along the stream axis and `thickness` across it, so the on-screen box
        // swaps them for a column.
        let badge_size = if orientation.is_vertical() {
            NSSize::new(extent.thickness, extent.length)
        } else {
            NSSize::new(extent.length, extent.thickness)
        };
        let origin = if orientation.is_vertical() {
            cursor -= extent.length;
            // Center the complete keycap rail in the backing strip, deriving both outer gaps
            // from its left and right bounds rather than biasing toward one anchored edge.
            let x = (panel_size.width - badge_size.width) / 2.0;
            NSPoint::new(x, cursor)
        } else {
            NSPoint::new(
                cursor,
                ((panel_size.height - badge_size.height) / 2.0).max(0.0),
            )
        };
        let badge_view: *mut AnyObject = msg_send![class!(NSView), alloc];
        let badge_view: *mut AnyObject = msg_send![
            badge_view,
            initWithFrame: NSRect::new(origin, badge_size)
        ];
        let _: () = msg_send![badge_view, setWantsLayer: true];
        let layer: *mut AnyObject = msg_send![badge_view, layer];
        let _: () = msg_send![layer, setCornerRadius: crate::theme::RADIUS_CONTROL];
        let _: () = msg_send![layer, setBorderWidth: 1.0f64];

        let (background, border, text_color) = if uses_accent_fill(badge.kind) {
            (
                accent_keycap_surface(palette),
                palette.keycap_accent_border,
                palette.keycap_accent_text,
            )
        } else {
            let (fill, border) = neutral_keycap_surface(palette);
            (fill, border, palette.primary_text)
        };
        // The chip is an opaque surface: the panel behind it is the user's material, so the text
        // floors cannot depend on what the panel happens to cover (see `opaque_over`).
        layer_set_background(layer, hex_to_cg_color(background));
        layer_set_border(layer, hex_to_cg_color(border));
        if orientation.is_vertical() && badge.repeats > 1 {
            // A merged keycap in a column keeps its glyph in the top cap-height zone and
            // puts the count on its own small row below (non-flipped coordinates: the top
            // zone starts at height - BADGE_H), so the count never widens the keycap.
            add_badge_label(
                badge_view,
                NSRect::new(
                    NSPoint::new(0.0, badge_size.height - BADGE_H),
                    NSSize::new(badge_size.width, BADGE_H),
                ),
                label,
                text_color,
            );
            let suffix = format!("×{}", badge.repeats);
            // Full-width centered field: a frame of exactly the measured text width
            // clips the count's last digit (the text cell keeps a small inset of its own).
            add_badge_label(
                badge_view,
                NSRect::new(
                    NSPoint::new(0.0, BADGE_SUFFIX_ROW_INSET_Y),
                    NSSize::new(badge_size.width, BADGE_REPEAT_SUFFIX_H),
                ),
                &suffix,
                palette.muted_text,
            );
        } else if badge.repeats > 1 {
            // Merged inline: the name keeps the keycap's text color and the count rides
            // ADJACENT to it (no gap) in muted text -- the count is metadata about the
            // press, not part of the key's name.
            let count = format!("×{}", badge.repeats);
            let label_w = measure_text_width(label);
            let count_w = measure_text_width(&count);
            // Each centered NSTextField has 8pt of extra width to absorb its cell inset;
            // account for half that slack here so the actual text run stays capsule-centered.
            let start = inline_badge_label_start(badge_size.width, label_w, count_w);
            add_badge_label(
                badge_view,
                NSRect::new(
                    NSPoint::new(start, 0.0),
                    NSSize::new(
                        label_w + BADGE_INLINE_FIELD_SLACK_X,
                        badge_size.height.min(BADGE_H),
                    ),
                ),
                label,
                text_color,
            );
            add_badge_label(
                badge_view,
                NSRect::new(
                    NSPoint::new(start + label_w, 0.0),
                    NSSize::new(
                        count_w + BADGE_INLINE_FIELD_SLACK_X,
                        badge_size.height.min(BADGE_H),
                    ),
                ),
                &count,
                palette.muted_text,
            );
        } else {
            let display_text = badge_display_text(label, badge.repeats);
            add_badge_label(
                badge_view,
                NSRect::new(
                    NSPoint::new(0.0, 0.0),
                    NSSize::new(badge_size.width, badge_size.height.min(BADGE_H)),
                ),
                &display_text,
                text_color,
            );
        }
        let _: () = msg_send![content, addSubview: badge_view];
        release_obj(badge_view);
        // Advance past this keycap and one stream gap. The row also adds the gap after the last
        // keycap; the cursor is past the stream either way, and every position comes from an
        // `origin` computed above, so the extra step moves nothing.
        if orientation.is_vertical() {
            cursor -= BADGE_GAP;
        } else {
            cursor += extent.length + BADGE_GAP;
        }
    }
}

/// The surface of a neutral keycap: `(fill, border)`.
///
/// The two modes need different fills, and the reason is measurable text contrast:
/// - **Light**: the card surface at 80% alpha composited to white on the near-white panel (1.05:1),
///   so the cap had no visible surface at all. The inset field surface reads at 1.12:1 there, and its
///   text measures 11.6:1 primary / 4.6:1 muted (the values the design doc records for light keycaps).
/// - **Dark**: the same lighter inset surface dropped the cap's text to 9.77:1 primary and 4.00:1
///   muted, under the palette's 12:1 and 4.5:1 floors, because light text needs a *dark* cap. Dark
///   therefore keeps the card surface at 80% (12.67:1 / 5.19:1).
fn neutral_keycap_surface(palette: &crate::theme::UiPalette) -> (u32, u32) {
    if palette.dark {
        (palette.card_bg, palette.card_border)
    } else {
        (
            opaque_over(palette.field_bg, palette.card_bg),
            palette.card_border,
        )
    }
}

/// A translucent token flattened onto `base`, so a chip is opaque and independent of the panel.
///
/// The keycaps used to be translucent *because* the panel surface was pinned to `window_bg` for them:
/// the panel now carries the user's material (design-style §3), so a chip that let the
/// desktop through would take its own text color with it -- light text on a chip that goes light over
/// a white desktop. The flattened value keeps exactly the RGB the translucent token produced over the
/// card surface, which is what the tuned contrast numbers were measured against.
fn opaque_over(token: u32, base: u32) -> u32 {
    crate::theme::flatten_token(token, base)
}

/// The accent chip's surface: the accent fill flattened onto the card surface (see [`opaque_over`]).
fn accent_keycap_surface(palette: &crate::theme::UiPalette) -> u32 {
    opaque_over(palette.keycap_accent_bg, palette.card_bg)
}

fn uses_accent_fill(kind: BadgeKind) -> bool {
    matches!(kind, BadgeKind::Modifier | BadgeKind::Indicator)
}

fn badge_labels(badges: &[Badge]) -> Vec<String> {
    badges
        .iter()
        .map(|badge| {
            if badge.kind == BadgeKind::Indicator {
                crate::i18n::t("keystroke_display.paused")
            } else {
                badge.text.clone()
            }
        })
        .collect()
}

unsafe fn screen_geometries() -> Vec<ScreenGeometry> {
    let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
    if screens.is_null() {
        return Vec::new();
    }
    let count: usize = msg_send![screens, count];
    if count > 0 {
        return (0..count)
            .map(|index| {
                let screen: *mut AnyObject = msg_send![screens, objectAtIndex: index as isize];
                ScreenGeometry {
                    frame: msg_send![screen, frame],
                    visible_frame: msg_send![screen, visibleFrame],
                }
            })
            .collect();
    }

    let main: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
    if main.is_null() {
        return Vec::new();
    }
    vec![ScreenGeometry {
        frame: msg_send![main, frame],
        visible_frame: msg_send![main, visibleFrame],
    }]
}

fn target_screen_index(
    display_position: &str,
    screens: &[ScreenGeometry],
    caret_point: Option<NSPoint>,
    window_point: Option<NSPoint>,
) -> usize {
    if screens.is_empty() || display_position == "main" {
        return 0;
    }
    let screen_for_point = |point: Option<NSPoint>| {
        point.and_then(|point| {
            screens
                .iter()
                .position(|screen| contains(screen.frame, point))
        })
    };
    let screen = if display_position == "caret" {
        screen_for_point(caret_point).or_else(|| screen_for_point(window_point))
    } else {
        screen_for_point(window_point)
    };
    screen.unwrap_or(0)
}

fn target_screen_index_live(display_position: &str, screens: &[ScreenGeometry]) -> usize {
    if display_position == "main" {
        return 0;
    }
    let caret_point = (display_position == "caret")
        .then(insertion_caret_appkit_point)
        .flatten();
    target_screen_index(
        display_position,
        screens,
        caret_point,
        frontmost_window_center(screens),
    )
}

#[repr(C)]
struct AxTextRange {
    location: isize,
    length: isize,
}

fn insertion_caret_appkit_point() -> Option<NSPoint> {
    let pid = crate::ffi::frontmost_app_info().1;
    if pid <= 0 {
        return None;
    }
    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return None;
        }
        // This synchronous lookup runs only when the panel opens; bound waits on unresponsive apps.
        let _ = AXUIElementSetMessagingTimeout(app, 0.05);
        let Some(focused_key) = ax_string("AXFocusedUIElement") else {
            CFRelease(app);
            return None;
        };
        let mut focused = std::ptr::null();
        let focused_result = AXUIElementCopyAttributeValue(app, focused_key, &mut focused);
        CFRelease(focused_key);
        if focused_result != crate::ffi::K_AX_SUCCESS || focused.is_null() {
            CFRelease(app);
            return None;
        }
        let _ = AXUIElementSetMessagingTimeout(focused, 0.05);

        let Some(range_key) = ax_string("AXSelectedTextRange") else {
            CFRelease(focused);
            CFRelease(app);
            return None;
        };
        let mut selected_range = std::ptr::null();
        let range_result = AXUIElementCopyAttributeValue(focused, range_key, &mut selected_range);
        CFRelease(range_key);
        let range = if range_result == crate::ffi::K_AX_SUCCESS
            && !selected_range.is_null()
            && AXValueGetType(selected_range) == 4
        {
            let mut range = AxTextRange {
                location: 0,
                length: 0,
            };
            AXValueGetValue(
                selected_range,
                4,
                &mut range as *mut AxTextRange as *mut c_void,
            )
            .then_some(range)
        } else {
            None
        };
        let point = if let Some(range) = range {
            let insertion_range = AxTextRange {
                location: range.location,
                length: 0,
            };
            let insertion_range_value =
                AXValueCreate(4, &insertion_range as *const AxTextRange as *const c_void);
            let bounds_key = ax_string("AXBoundsForRange");
            match (insertion_range_value.is_null(), bounds_key) {
                (false, Some(bounds_key)) => {
                    let mut bounds_value = std::ptr::null();
                    let result = AXUIElementCopyParameterizedAttributeValue(
                        focused,
                        bounds_key,
                        insertion_range_value,
                        &mut bounds_value,
                    );
                    CFRelease(bounds_key);
                    CFRelease(insertion_range_value);
                    if result == crate::ffi::K_AX_SUCCESS
                        && !bounds_value.is_null()
                        && AXValueGetType(bounds_value) == 3
                    {
                        let mut bounds = CGRect {
                            x: 0.0,
                            y: 0.0,
                            w: 0.0,
                            h: 0.0,
                        };
                        let valid = AXValueGetValue(
                            bounds_value,
                            3,
                            &mut bounds as *mut CGRect as *mut c_void,
                        );
                        CFRelease(bounds_value);
                        let screens = screen_geometries();
                        let primary = screens.first().map(|screen| screen.frame);
                        (valid
                            && bounds.x.is_finite()
                            && bounds.y.is_finite()
                            && bounds.w.is_finite()
                            && bounds.h.is_finite()
                            && bounds.h >= 0.0)
                            .then_some(primary)
                            .flatten()
                            .map(|primary| {
                                NSPoint::new(
                                    bounds.x + bounds.w / 2.0,
                                    primary.origin.y + primary.size.height
                                        - (bounds.y + bounds.h / 2.0),
                                )
                            })
                    } else {
                        if !bounds_value.is_null() {
                            CFRelease(bounds_value);
                        }
                        None
                    }
                }
                (range_is_null, bounds_key) => {
                    if !range_is_null {
                        CFRelease(insertion_range_value);
                    }
                    if let Some(bounds_key) = bounds_key {
                        CFRelease(bounds_key);
                    }
                    None
                }
            }
        } else {
            None
        };
        if !selected_range.is_null() {
            CFRelease(selected_range);
        }
        CFRelease(focused);
        CFRelease(app);
        point
    }
}

unsafe fn ax_string(value: &str) -> Option<*const c_void> {
    let value = CString::new(value).ok()?;
    let string = CFStringCreateWithCString(std::ptr::null(), value.as_ptr(), 0x08000100);
    (!string.is_null()).then_some(string)
}

fn screen_for_saved_origin(
    position: Option<KeystrokeDisplayPosition>,
    screens: &[ScreenGeometry],
) -> Option<usize> {
    let position = position?;
    if !position.x.is_finite() || !position.y.is_finite() {
        return None;
    }
    let point = NSPoint::new(position.x, position.y);
    screens
        .iter()
        .position(|screen| contains(screen.frame, point))
}

fn clamp_origin_to_visible(origin: NSPoint, size: NSSize, visible: NSRect) -> NSPoint {
    let max_x = (visible.origin.x + visible.size.width - size.width).max(visible.origin.x);
    let max_y = (visible.origin.y + visible.size.height - size.height).max(visible.origin.y);
    NSPoint::new(
        origin.x.max(visible.origin.x).min(max_x),
        origin.y.max(visible.origin.y).min(max_y),
    )
}

/// The frame the panel takes when nothing has been dragged: centered on the edge named by
/// `initial_position`, keeping `PANEL_EDGE_MARGIN` from that edge. `top`/`bottom` center
/// horizontally, `left`/`right` center vertically, so the panel is always parallel to its edge.
fn default_edge_frame(visible: NSRect, size: NSSize, initial_position: &str) -> NSRect {
    let centered_x = visible.origin.x + (visible.size.width - size.width) / 2.0;
    let centered_y = visible.origin.y + (visible.size.height - size.height) / 2.0;
    let origin = match initial_position {
        "top" => NSPoint::new(
            centered_x,
            visible.origin.y + visible.size.height - size.height - PANEL_EDGE_MARGIN,
        ),
        "left" => NSPoint::new(visible.origin.x + PANEL_EDGE_MARGIN, centered_y),
        "right" => NSPoint::new(
            visible.origin.x + visible.size.width - size.width - PANEL_EDGE_MARGIN,
            centered_y,
        ),
        // "bottom", and anything unrecognised: the documented default.
        _ => NSPoint::new(centered_x, visible.origin.y + PANEL_EDGE_MARGIN),
    };
    NSRect::new(clamp_origin_to_visible(origin, size, visible), size)
}

fn resize_frame_preserving_center(frame: NSRect, size: NSSize, visible: NSRect) -> NSRect {
    let center = NSPoint::new(
        frame.origin.x + frame.size.width / 2.0,
        frame.origin.y + frame.size.height / 2.0,
    );
    let origin = NSPoint::new(center.x - size.width / 2.0, center.y - size.height / 2.0);
    NSRect::new(clamp_origin_to_visible(origin, size, visible), size)
}

fn resolve_panel_frame(
    position: Option<KeystrokeDisplayPosition>,
    screens: &[ScreenGeometry],
    target_index: usize,
    size: NSSize,
    orientation: Orientation,
    initial_position: &str,
) -> (usize, NSRect) {
    if screens.is_empty() {
        return (0, NSRect::new(NSPoint::new(0.0, 0.0), size));
    }
    let target_index = target_index.min(screens.len().saturating_sub(1));
    let _ = orientation;
    let Some(screen_index) = screen_for_saved_origin(position, screens) else {
        return (
            target_index,
            default_edge_frame(screens[target_index].visible_frame, size, initial_position),
        );
    };
    let position = position.expect("a saved position selected a screen");
    let screen = screens[screen_index];
    let origin = clamp_origin_to_visible(
        NSPoint::new(position.x, position.y),
        size,
        screen.visible_frame,
    );
    (screen_index, NSRect::new(origin, size))
}

fn frontmost_window_center(screens: &[ScreenGeometry]) -> Option<NSPoint> {
    let primary = screens.first()?.frame;
    let (_, front_pid) = crate::ffi::frontmost_app_info();
    crate::with_tab_state(|state| {
        let state = state.as_ref()?;
        state
            .windows
            .iter()
            .find(|window| window.pid == front_pid && window.is_active)
            .or_else(|| state.windows.iter().find(|window| window.pid == front_pid))
            .or_else(|| state.windows.iter().find(|window| window.is_active))
            .map(|window| {
                let (x, y, width, height) = window.bounds;
                NSPoint::new(
                    x + width / 2.0,
                    primary.origin.y + primary.size.height - (y + height / 2.0),
                )
            })
    })
}

fn contains(frame: NSRect, point: NSPoint) -> bool {
    point.x >= frame.origin.x
        && point.x <= frame.origin.x + frame.size.width
        && point.y >= frame.origin.y
        && point.y <= frame.origin.y + frame.size.height
}

#[cfg(test)]
mod tests {
    use super::{
        accent_keycap_surface, centered_stream_cursor, content_sized_panel_length,
        default_edge_frame, max_extent_for_percent, neutral_keycap_surface, opaque_over,
        preview_badges, resize_frame_preserving_center, resolve_panel_frame, stream_axis_reserves,
        target_screen_index, trailing_edge_inset, uses_accent_fill, Badge, BadgeCell, Orientation,
        ScreenGeometry, BADGE_GAP, BADGE_H, HEAD_GRIP_RESERVE, HEAD_STREAM_RESERVE,
        MIN_PANEL_STREAM_EXTENT, PANEL_SIDE_PADDING, PREVIEW_MAX_BADGES, PREVIEW_PHRASE,
        SETTINGS_BTN_CROSS_INSET, SETTINGS_BTN_GAP, SETTINGS_BTN_SIZE, STREAM_BUDGET_DEDUCTION,
        TAIL_SETTINGS_RESERVE,
    };
    use crate::config::KeystrokeDisplayPosition;
    use crate::keystroke_display::state::{
        estimated_badge_width, estimated_stream_width, BadgeKind, KEYCAP_RAIL_W,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize};

    #[test]
    fn max_extent_for_percent_scales_the_screen_and_keeps_a_usable_floor() {
        // The documented default is exactly the old hard-coded half.
        assert_eq!(
            max_extent_for_percent(1440.0, 50),
            720.0,
            "the default must equal the old screen/2 cap"
        );
        assert_eq!(max_extent_for_percent(1440.0, 10), 144.0);
        assert_eq!(max_extent_for_percent(1440.0, 100), 1440.0);
        // A very small screen clamps to the floor that keeps a keycap clear of the controls.
        assert_eq!(max_extent_for_percent(200.0, 10), MIN_PANEL_STREAM_EXTENT);
        // The floor holds one minimum keycap inside both reserves: the keycap, the nominal padding
        // per end that estimated_stream_width counts, and the reserve deduction.
        assert_eq!(
            MIN_PANEL_STREAM_EXTENT,
            32.0 + PANEL_SIDE_PADDING * 2.0 + STREAM_BUDGET_DEDUCTION
        );
        // Out-of-range input from a stale caller is clamped, never extrapolated.
        assert_eq!(max_extent_for_percent(1440.0, 0), 144.0);
        assert_eq!(max_extent_for_percent(1440.0, 500), 1440.0);
    }

    #[test]
    fn preview_badges_cycle_the_phrase_and_stay_bounded() {
        let badges = preview_badges();
        assert_eq!(badges.len(), PREVIEW_MAX_BADGES);
        let text: String = badges.iter().map(|badge| badge.text.clone()).collect();
        assert_eq!(
            text.chars().take(PREVIEW_PHRASE.len()).collect::<String>(),
            PREVIEW_PHRASE
        );
        assert_eq!(badges[0].kind, BadgeKind::Chord);
        // Every entry is a lone keycap: a cell list would draw a chord the preview does not have.
        assert!(badges.iter().all(|badge| badge.cells.is_empty()));
        // Even the widest possible cell cannot over-fill the cap in this many badges.
        assert!(estimated_stream_width(badges.iter().map(Badge::estimated_width)) > 5000.0);
    }
    #[test]
    fn single_key_streams_are_centered_with_equal_end_margins_in_both_orientations() {
        // A one-letter row uses a 32pt keycap in a 64pt panel; a column uses a 34pt
        // keycap in a 64pt panel. `estimated_total` includes 12pt nominal padding per end.
        // Zero reserves is the case the old implementation always took (the panel with no
        // controls), so it stays the reference for "centred with equal margins".
        let horizontal_cursor = centered_stream_cursor(64.0, 56.0, false, 0.0, 0.0);
        assert_eq!(horizontal_cursor, 16.0);
        assert_eq!(64.0 - horizontal_cursor - 32.0, horizontal_cursor);

        let vertical_cursor = centered_stream_cursor(64.0, 58.0, true, 0.0, 0.0);
        let vertical_keycap_start = vertical_cursor - 34.0;
        assert_eq!(vertical_keycap_start, 15.0);
        assert_eq!(vertical_keycap_start, 64.0 - vertical_cursor);
    }

    /// Each end's reserve is sized from its own control, and a content-sized panel lands the
    /// keycaps exactly on those reserve lines: the trailing keycap stops `SETTINGS_BTN_GAP` short of
    /// the gear (the bug this replaced put it on the gear's edge, gap 0), and the leading one clears
    /// the handle's hit area. The two margins differ by design -- that asymmetry is what removed the
    /// gear's larger reserve from the handle's end.
    #[test]
    fn the_stream_keeps_its_gap_from_both_controls() {
        // The reserves the layout actually applies: the handle's own need at the leading end, the
        // gear's at the trailing one.
        let (head, tail) = stream_axis_reserves(Orientation::Horizontal, NSSize::new(0.0, 0.0));
        assert_eq!(head, HEAD_STREAM_RESERVE);
        assert_eq!(tail, TAIL_SETTINGS_RESERVE);

        // A row: one 32pt keycap; estimated_total includes the 12pt nominal padding per end.
        let total = 32.0 + PANEL_SIDE_PADDING * 2.0;
        let panel = content_sized_panel_length(total);
        let cursor = centered_stream_cursor(panel, total, false, head, tail);
        assert_eq!(
            cursor, head,
            "the leading keycap sits on the handle's reserve line"
        );
        assert_eq!(
            panel - (cursor + 32.0),
            tail,
            "the trailing keycap sits on the gear's reserve line"
        );
        // The trailing reserve is the gear's inset, its width and its gap, so the promised gap
        // survives: the keycap stops SETTINGS_BTN_GAP short of the gear's leading edge.
        assert_eq!(
            TAIL_SETTINGS_RESERVE - SETTINGS_BTN_CROSS_INSET - SETTINGS_BTN_SIZE,
            SETTINGS_BTN_GAP
        );
        // The leading reserve clears the handle's hit area, so the first keycap never sits under
        // the pointer that opens the drag.
        assert!(cursor >= HEAD_GRIP_RESERVE);

        // A column counts down from the top: the handle's reserve is the top margin, the gear's the
        // bottom one.
        let (head, tail) = stream_axis_reserves(Orientation::Vertical, NSSize::new(0.0, 0.0));
        let column_total = 34.0 + PANEL_SIDE_PADDING * 2.0;
        let column_panel = content_sized_panel_length(column_total);
        let column_cursor = centered_stream_cursor(column_panel, column_total, true, head, tail);
        assert_eq!(
            column_panel - column_cursor,
            head,
            "the column's first keycap tops out on the handle's reserve line"
        );
        assert_eq!(
            column_cursor - 34.0,
            tail,
            "the column's last keycap starts on the gear's reserve line"
        );
    }

    #[test]
    fn keycap_text_is_a_recorded_class_over_both_extreme_backdrops() {
        // The panel floats over arbitrary content and the frost wash is only 0xE9 opaque, so the cap
        // surface moves with the backdrop: a black backdrop lands the light panel at (225,226,228), a
        // white one at (247,248,250). Everything a keycap shows is therefore measured over the FULL
        // chain -- backdrop -> wash -> cap -> text -- at both extremes, for both cap kinds.
        //
        // Keycap text is a recorded class of its own rather than `text_primary`'s 12:1 floor, which
        // applies to text on `window_bg`/`card_bg`. The muted floor is the §3.3 hard floor (3.5:1):
        // an earlier translucent-cap build measured muted at 3.37:1 on the worst backdrop, which is
        // why the caps are now opaque (see `opaque_over`) and why the floor can be the real one.
        // This is also why the light neutral cap keeps the inset surface
        // (1.12:1 against the panel) even though it cannot reach 12:1: a cap dark enough to read as a
        // surface on a near-white panel cannot also carry 12:1 text.
        const KEYCAP_TEXT_PRIMARY_FLOOR: f64 = 8.0;
        const KEYCAP_TEXT_MUTED_FLOOR: f64 = 3.5;
        for dark in [false, true] {
            let palette = crate::theme::ui_palette_for_mode(dark);
            let (fill, border) = neutral_keycap_surface(&palette);
            let cap_text = palette.primary_text;
            let accent_text = palette.keycap_accent_text;
            let keycaps = [
                ("neutral", fill, cap_text),
                ("accent", accent_keycap_surface(&palette), accent_text),
            ];
            for backdrop in [0xFFFF_FFFFu32, 0x0000_00FF] {
                // This panel owns its text surfaces (keycaps carry their own fills), so its material
                // is unwashed and the panel colour is the blurred backdrop. A blur can only move the
                // surface *toward* the backdrop, so the backdrop itself is the worst case at each end.
                let panel = crate::theme::color_rgb(backdrop);
                for (kind, cap_fill, text) in keycaps {
                    let cap = crate::theme::composite_on(cap_fill, panel);
                    let primary = crate::theme::contrast_on(text, cap);
                    let muted = crate::theme::contrast_on(palette.muted_text, cap);
                    assert!(
                        primary >= KEYCAP_TEXT_PRIMARY_FLOOR,
                        "dark={dark} backdrop={backdrop:#010x} {kind}: primary {primary:.2}",
                    );
                    assert!(
                        muted >= KEYCAP_TEXT_MUTED_FLOOR,
                        "dark={dark} backdrop={backdrop:#010x} {kind}: muted {muted:.2}",
                    );
                }
            }
            // The fill identity per mode (the choice this test documents).
            assert_eq!(border, palette.card_border, "dark={dark}");
            if dark {
                assert_eq!(fill, palette.card_bg, "dark keycaps are the card surface");
            } else {
                assert_eq!(
                    fill,
                    opaque_over(palette.field_bg, palette.card_bg),
                    "light keycaps use the inset surface, flattened"
                );
            }
            // Every chip is opaque, which is what makes the text floors independent of the panel
            // material the user selected: a chip that took its colour from the panel would carry its
            // own text colour away with it (light text on a chip that goes light over a white
            // desktop). Separation from the panel comes from the fill where it exists and otherwise
            // from the chip's hairline border -- the same treatment dark mode always had.
            for (kind, fill, _) in keycaps {
                assert_eq!(
                    fill & 0xFF,
                    0xFF,
                    "dark={dark} {kind}: chip surfaces must be opaque"
                );
            }
        }
    }

    #[test]
    fn released_modifier_badges_use_the_neutral_keycap_fill() {
        assert!(uses_accent_fill(BadgeKind::Modifier));
        assert!(!uses_accent_fill(BadgeKind::ModifierReleased));
        assert!(uses_accent_fill(BadgeKind::Indicator));
    }

    #[test]
    fn inline_repeat_text_accounts_for_field_slack_when_centering() {
        let badge_width = 110.0;
        let label_width = 40.0;
        let count_width = 28.0;
        let start = super::inline_badge_label_start(badge_width, label_width, count_width);
        let actual_text_start = start + super::BADGE_INLINE_FIELD_SLACK_X / 2.0;
        let actual_text_end = actual_text_start + label_width + count_width;
        assert_eq!(
            (actual_text_start + actual_text_end) / 2.0,
            badge_width / 2.0
        );
    }

    #[test]
    fn keycap_hide_fade_uses_the_shared_exit_duration() {
        assert_eq!(super::HIDE_FADE.as_millis(), 285);
    }

    #[test]
    fn grip_hit_area_is_larger_than_the_visible_dots_and_rotates_with_orientation() {
        let panel_size = NSSize::new(80.0, 240.0);
        let vertical = super::grip_frame(Orientation::Vertical, panel_size);
        assert_eq!(
            vertical.origin.y,
            panel_size.height - vertical.size.height - super::GRIP_EDGE_INSET
        );
        assert_eq!(vertical.size, NSSize::new(48.0, 20.0));
        let first_dot = super::grip_marker_frame(Orientation::Vertical, 0);
        let dot_center_from_panel_top = panel_size.height
            - (vertical.origin.y + first_dot.origin.y + first_dot.size.height / 2.0);
        assert_eq!(
            dot_center_from_panel_top,
            super::GRIP_EDGE_INSET + super::GRIP_MARK_CENTER_INSET
        );
        // The dots stay inside the transparent hit area, clear of the first keycap.
        assert!(dot_center_from_panel_top < super::GRIP_EDGE_INSET + vertical.size.height);

        let horizontal = super::grip_frame(Orientation::Horizontal, panel_size);
        assert_eq!(horizontal.size, NSSize::new(20.0, 48.0));
        assert_eq!(horizontal.origin.x, 0.0);
        let first_horizontal_dot = super::grip_marker_frame(Orientation::Horizontal, 0);
        let horizontal_dot_center = horizontal.origin.x
            + first_horizontal_dot.origin.x
            + first_horizontal_dot.size.width / 2.0;
        assert_eq!(horizontal_dot_center, super::GRIP_MARK_CENTER_INSET);
        assert!(horizontal_dot_center < horizontal.origin.x + horizontal.size.width);
    }

    /// The column's fixed keycap width must hold every shipped locale's keycap text (the
    /// merge count renders on its own row in a column, so it does not count toward the
    /// width), so truncation never fires for a real key in a language we ship (§11.5:
    /// widths come from measurement, not character counts). The page-up/down legends are
    /// short ("Pg Dn" / "下页") precisely so they stay under the width the common keys need
    /// anyway.
    #[test]
    fn keycap_rail_width_fits_the_widest_shipped_key_names() {
        for label in [
            "Paused",
            "已暂停",
            "Space",
            "空格",
            "空白鍵",
            "Pg Up",
            "Pg Dn",
            "上页",
            "下頁",
            "Home",
            "Clear",
            "清除",
            "F20",
            "esc",
            "Tab",
            "⌫",
        ] {
            let width =
                unsafe { super::measure_text_width(label) } + super::BADGE_HORIZONTAL_PADDING;
            assert!(
                width <= KEYCAP_RAIL_W,
                "{label:?} measures {width:.1}pt, over the {KEYCAP_RAIL_W}pt fixed rail width"
            );
        }
    }

    fn virtual_screens() -> [ScreenGeometry; 2] {
        [
            ScreenGeometry {
                frame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1000.0, 800.0)),
                visible_frame: NSRect::new(NSPoint::new(0.0, 20.0), NSSize::new(1000.0, 760.0)),
            },
            ScreenGeometry {
                frame: NSRect::new(NSPoint::new(-1280.0, 0.0), NSSize::new(1280.0, 900.0)),
                visible_frame: NSRect::new(NSPoint::new(-1280.0, 20.0), NSSize::new(1280.0, 860.0)),
            },
        ]
    }

    #[test]
    fn display_position_uses_main_or_insertion_caret_screen_with_window_fallback() {
        let screens = virtual_screens();
        let main_point = Some(NSPoint::new(200.0, 200.0));
        let secondary_point = Some(NSPoint::new(-600.0, 300.0));

        assert_eq!(
            target_screen_index("main", &screens, secondary_point, secondary_point),
            0
        );
        assert_eq!(
            target_screen_index("caret", &screens, secondary_point, main_point),
            1
        );
        assert_eq!(
            target_screen_index("caret", &screens, None, secondary_point),
            1
        );
        assert_eq!(
            target_screen_index(
                "caret",
                &screens,
                Some(NSPoint::new(5000.0, 5000.0)),
                secondary_point
            ),
            1
        );
        assert_eq!(target_screen_index("caret", &screens, None, main_point), 0);
    }

    #[test]
    fn saved_origin_stays_on_its_screen_and_clamps_to_its_visible_frame() {
        let (screen_index, frame) = resolve_panel_frame(
            Some(KeystrokeDisplayPosition { x: 950.0, y: 790.0 }),
            &virtual_screens(),
            1,
            NSSize::new(300.0, 54.0),
            Orientation::Horizontal,
            "bottom",
        );
        assert_eq!(screen_index, 0);
        assert_eq!(frame.origin, NSPoint::new(700.0, 726.0));
    }

    #[test]
    fn offscreen_saved_origin_uses_follow_target_without_mutating_config() {
        let saved = Some(KeystrokeDisplayPosition {
            x: 5000.0,
            y: 100.0,
        });
        let (screen_index, frame) = resolve_panel_frame(
            saved,
            &virtual_screens(),
            1,
            NSSize::new(300.0, 54.0),
            Orientation::Horizontal,
            "bottom",
        );
        assert_eq!(screen_index, 1);
        assert_eq!(frame.origin, NSPoint::new(-790.0, 38.0));
        assert_eq!(
            saved,
            Some(KeystrokeDisplayPosition {
                x: 5000.0,
                y: 100.0
            })
        );
    }

    #[test]
    fn absent_saved_origin_uses_bottom_center_of_follow_target() {
        let (screen_index, frame) = resolve_panel_frame(
            None,
            &virtual_screens(),
            1,
            NSSize::new(300.0, 54.0),
            Orientation::Horizontal,
            "bottom",
        );
        assert_eq!(screen_index, 1);
        assert_eq!(frame.origin, NSPoint::new(-790.0, 38.0));
    }

    /// The four edges: `top`/`bottom` center horizontally, `left`/`right` center vertically,
    /// and each keeps the same margin from the edge it is anchored to.
    #[test]
    fn each_initial_position_anchors_to_its_own_edge_and_centers_on_the_other_axis() {
        let visible = virtual_screens()[0].visible_frame;
        let size = NSSize::new(300.0, 54.0);
        let margin = 18.0;
        let centered_x = visible.origin.x + (visible.size.width - size.width) / 2.0;
        let centered_y = visible.origin.y + (visible.size.height - size.height) / 2.0;

        let bottom = default_edge_frame(visible, size, "bottom");
        assert_eq!(
            bottom.origin,
            NSPoint::new(centered_x, visible.origin.y + margin)
        );

        let top = default_edge_frame(visible, size, "top");
        assert_eq!(
            top.origin,
            NSPoint::new(
                centered_x,
                visible.origin.y + visible.size.height - size.height - margin
            )
        );

        // A column is tall, so its own height is what the edge math has to clear.
        let tall = NSSize::new(64.0, 300.0);
        let left = default_edge_frame(visible, tall, "left");
        assert_eq!(left.origin.x, visible.origin.x + margin);
        assert_eq!(
            left.origin.y,
            visible.origin.y + (visible.size.height - tall.height) / 2.0
        );

        let right = default_edge_frame(visible, tall, "right");
        assert_eq!(
            right.origin.x,
            visible.origin.x + visible.size.width - tall.width - margin
        );
        assert_eq!(
            right.origin.y,
            visible.origin.y + (visible.size.height - tall.height) / 2.0
        );

        // An unrecognised value still lands on the documented default rather than nowhere.
        assert_eq!(
            default_edge_frame(visible, size, "sideways").origin,
            bottom.origin
        );
        // `centered_y` documents the horizontal panel's unused axis; keep it meaningful.
        assert!(centered_y > visible.origin.y);
    }

    /// A chord measures as the split keycaps it is drawn as, along either axis: a row lays them
    /// out sideways with the stream gap between them, a column stacks them downward. A lone
    /// keycap keeps its size and only swaps which axis is "along the stream".
    #[test]
    fn orientation_decides_which_axis_a_chord_grows_along() {
        assert_eq!(
            Orientation::from_initial_position("top"),
            Orientation::Horizontal
        );
        assert_eq!(
            Orientation::from_initial_position("bottom"),
            Orientation::Horizontal
        );
        assert_eq!(
            Orientation::from_initial_position("left"),
            Orientation::Vertical
        );
        assert_eq!(
            Orientation::from_initial_position("right"),
            Orientation::Vertical
        );
        // Unknown config values fall back to the historical shape.
        assert_eq!(
            Orientation::from_initial_position("sideways"),
            Orientation::Horizontal
        );

        let chord = Badge {
            text: "⌘Q".into(),
            kind: BadgeKind::Chord,
            repeats: 1,
            cells: vec![BadgeCell::Modifier("⌘".into()), BadgeCell::Key("Q".into())],
        };
        assert_eq!(
            chord.estimated_extent(Orientation::Horizontal),
            estimated_badge_width("⌘", 1) + BADGE_GAP + estimated_badge_width("Q", 1),
            "a row splits a chord into standalone keycaps with stream spacing"
        );
        assert_eq!(
            chord.estimated_extent(Orientation::Vertical),
            2.0 * BADGE_H + BADGE_GAP,
            "a column splits a chord into standalone keycaps with stream spacing"
        );
        let triple_chord = Badge {
            cells: vec![
                BadgeCell::Modifier("⌘".into()),
                BadgeCell::Modifier("⇧".into()),
                BadgeCell::Key("Q".into()),
            ],
            ..chord.clone()
        };
        assert_eq!(
            triple_chord.estimated_extent(Orientation::Horizontal),
            estimated_badge_width("⌘", 1)
                + BADGE_GAP
                + estimated_badge_width("⇧", 1)
                + BADGE_GAP
                + estimated_badge_width("Q", 1)
        );
        assert_eq!(
            triple_chord.estimated_extent(Orientation::Vertical),
            3.0 * BADGE_H + 2.0 * BADGE_GAP
        );

        // A lone keycap is the same size either way, but its extent along the stream swaps:
        // a row advances across its width, a column down its height. A multi-character
        // label keeps the keycap wider than it is tall, which the assertion relies on.
        let lone = Badge {
            text: "Space".into(),
            kind: BadgeKind::Chord,
            repeats: 1,
            cells: Vec::new(),
        };
        let lone_row = lone.estimated_extent(Orientation::Horizontal);
        let lone_column = lone.estimated_extent(Orientation::Vertical);
        assert_eq!(
            lone_column, BADGE_H,
            "a column advances by one keycap height"
        );
        assert!(
            lone_row > lone_column,
            "a keycap is wider than it is tall ({lone_row} vs {lone_column})"
        );
    }

    #[test]
    fn growing_badge_stream_keeps_panel_centered_and_inside_visible_frame() {
        let visible = virtual_screens()[0].visible_frame;
        let initial = NSRect::new(NSPoint::new(468.0, 38.0), NSSize::new(64.0, 54.0));
        let expanded = resize_frame_preserving_center(initial, NSSize::new(300.0, 54.0), visible);

        assert_eq!(expanded.origin, NSPoint::new(350.0, 38.0));
        assert_eq!(
            expanded.origin.x + expanded.size.width / 2.0,
            initial.origin.x + initial.size.width / 2.0
        );
    }

    #[test]
    fn centered_resize_clamps_when_panel_grows_near_screen_edge() {
        let visible = virtual_screens()[0].visible_frame;
        let initial = NSRect::new(NSPoint::new(900.0, 38.0), NSSize::new(64.0, 54.0));
        let expanded = resize_frame_preserving_center(initial, NSSize::new(300.0, 54.0), visible);

        assert_eq!(expanded.origin, NSPoint::new(700.0, 38.0));
        assert!(expanded.origin.x + expanded.size.width <= visible.origin.x + visible.size.width);
    }

    /// A column's trailing edge is the row's rotated 90 degrees, so the inset computed for a
    /// transposed panel must be identical. This is the correspondence the frame relies on.
    #[test]
    fn the_hud_trailing_inset_rotates_with_the_orientation() {
        for (w, h) in [(152.0, 54.0), (54.0, 152.0), (300.0, 40.0), (40.0, 300.0)] {
            let column =
                trailing_edge_inset(Orientation::Vertical, NSSize::new(w, h), SETTINGS_BTN_SIZE);
            let row = trailing_edge_inset(
                Orientation::Horizontal,
                NSSize::new(h, w),
                SETTINGS_BTN_SIZE,
            );
            assert!(
                (column - row).abs() < 1e-9,
                "column {column} vs the transposed row {row} for {w}x{h}"
            );
        }
    }

    /// The counter-example the review asked for: the band argument must be the CROSS extent. With a
    /// narrow panel the two arguments disagree, so passing the flow extent moves the gear.
    ///
    /// A narrow cross extent is load-bearing: at the shipped radius and a >= 32pt cross extent the
    /// per-axis clamp makes the two arguments agree, which is why the defect stayed latent.
    #[test]
    fn the_hud_trailing_inset_uses_the_cross_extent_as_the_band() {
        let (width, height) = (28.0, 400.0);
        let control = SETTINGS_BTN_SIZE;
        let radius = crate::glass::PANEL_CORNER_RADIUS;
        let panel = NSSize::new(width, height);
        let correct = trailing_edge_inset(Orientation::Vertical, panel, control);
        // Same radius, same panel and same control -- only the band argument differs: the flow
        // extent (height) instead of the cross extent (width).
        let wrong = crate::theme::footer_control_inset(radius, height, width, control, height);
        assert!(
            correct > wrong + 1.0,
            "the cross-extent band must inset more than the flow-extent band: {correct} vs {wrong}"
        );
    }
}
