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
    column_display_badges, estimated_stream_width, repeat_suffix_row, Badge, BadgeCell, BadgeKind,
    Orientation, BADGE_CELL_GAP, BADGE_CELL_INSET_Y, BADGE_CELL_PADDING_X,
    BADGE_CONTAINER_PADDING_X, BADGE_CONTAINER_PADDING_Y, BADGE_GAP, BADGE_H,
    BADGE_HORIZONTAL_PADDING, BADGE_MIN_WIDTH, BADGE_REPEAT_SUFFIX_H, KEYCAP_RAIL_W,
    PANEL_SIDE_PADDING,
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
/// Keep the visible dot row near the outer edge of the larger hit area, clear of the first cap.
const GRIP_MARK_CENTER_INSET: f64 = 5.0;
const GRIP_MARK_WIDTH: f64 = 3.0;
const GRIP_MARK_HEIGHT: f64 = 3.0;
const GRIP_MARK_GAP: f64 = 3.0;
const DRAG_THRESHOLD: f64 = 4.0;

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

/// The target screen's extent along the panel's stream axis: the width the cap is measured
/// against when keys run in a row, the height when they stack in a column.
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
    } = placement;
    let orientation = Orientation::from_initial_position(initial_position);
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
                let (panel, badge_container, grip_view) = unsafe { create_panel(orientation) };
                state.panel = Some(panel);
                state.badge_container = Some(badge_container);
                state.grip_view = Some(grip_view);
                panel
            };
            let badge_container = state
                .badge_container
                .expect("a created keystroke panel has a badge container");
            // A column draws every keycap independently: chords are split into one lone
            // keycap per key (modifiers keep their tint, no grouping tray), so the whole
            // stream is lone keycaps. A row keeps trayed chord cells.
            let column_badges = orientation
                .is_vertical()
                .then(|| column_display_badges(badges));
            let display: &[Badge] = column_badges.as_deref().unwrap_or(badges);
            let labels = badge_labels(display);
            let mut extents = Vec::with_capacity(display.len());
            for badge in display.iter() {
                extents.push(cached_badge_extent(
                    &mut state.measurements,
                    badge,
                    orientation,
                ));
            }
            // The stream may only grow along its own axis, so the cap follows the panel's edge:
            // half the screen's width for a row, half its height for a column.
            let max_extent = if orientation.is_vertical() {
                screen_frame.size.height * 0.5
            } else {
                screen_frame.size.width * 0.5
            };
            let mut desired = estimated_stream_width(extents.iter().map(|extent| extent.length));
            let mut first = 0usize;
            while extents.len() > 1 && desired > max_extent {
                extents.remove(0);
                first += 1;
                desired = estimated_stream_width(extents.iter().map(|extent| extent.length));
            }
            // Only this final per-badge clamp can ellipsize a genuinely oversized badge.
            let length_limit = (max_extent - PANEL_SIDE_PADDING * 2.0).max(32.0);
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
            state.latched = pinned;
            let panel_length = if pinned {
                max_extent
            } else {
                desired.min(max_extent).max(max_extent.min(64.0))
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
            let panel_cross = if orientation.is_vertical() {
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
                let palette = crate::theme::ui_palette();
                if state.last_badges != badges || state.last_palette != Some(palette) {
                    rebuild_badges(
                        badge_container,
                        &display[first..],
                        &labels[first..],
                        &extents,
                        size,
                        orientation,
                    );
                    state.last_badges = badges.to_vec();
                    state.last_palette = Some(palette);
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

pub(super) fn reset() {
    crate::debug_assert_main_thread();
    stop_timer();
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
        state.last_palette = None;
        state.measurements.clear();
        state.latched = false;
        state.panel_cross_latched = 0.0;
    });
}

pub(super) fn smoke_runner() -> bool {
    crate::debug_assert_main_thread();
    // A `--panel-material=` launch switch selects exactly one surface, including development-only ones
    // whose checks (system material present, palette surface painted) describe the shipped materials and
    // not this surface. Structure and geometry are asserted either way; the shipped set is asserted in
    // full only when no switch forced a material for this launch.
    let dev_material_forced = crate::dev_flags::value("panel-material").is_some();
    super::mapping::refresh_layout_cache();
    let option_q_is_unmodified = super::mapping::key_glyph(
        crate::event_tap::keyboard::VK_Q,
        "œ",
        crate::event_tap::keyboard::FLAG_OPTION,
    )
    .as_deref()
        == Some("q");
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
                && (dev_material_forced || backdrop_frost_mask_valid())
                // The palette surface is painted by the shipped materials; the development-only blur
                // surface paints nothing (its own check is the backdrop layer in `backdrop_structure_valid`).
                && (dev_material_forced || backdrop_surface_matches_palette())
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
            },
            smoke_now,
            None,
        );
        changed &= unsafe { backdrop_surface_matches_palette() };
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
        }
        {
            let mut config = crate::config::CONFIG.write().unwrap();
            config.appearance.theme = original;
        }
        crate::ui_coordinator::apply_theme_and_locale_refresh();
        changed && unsafe { backdrop_surface_matches_palette() }
    };

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
            text: "⌥q".into(),
            kind: BadgeKind::Chord,
            repeats: 1,
            cells: vec![BadgeCell::Modifier("⌥".into()), BadgeCell::Key("q".into())],
        });
        let _ = render(
            &long_stream,
            true,
            false,
            PanelPlacement {
                display_position: "main",
                initial_position: "right",
                position: saved_position,
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
                    if label == "⌥" || label == "q" {
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
                    (
                        frame.origin.y - bounds.origin.y,
                        bounds.origin.y + bounds.size.height
                            - (frame.origin.y + frame.size.height),
                    )
                } else {
                    (
                        frame.origin.x - bounds.origin.x,
                        bounds.origin.x + bounds.size.width
                            - (frame.origin.x + frame.size.width),
                    )
                };
                let ok = (leading - trailing).abs() < 0.5;
                eprintln!(
                    "[keystroke-display-smoke] single-key {position} margins={leading:.1}/{trailing:.1} ok={ok}"
                );
                ok
            }
        })
    });
    update_config_position(saved_position);
    let config_restored = crate::config::flush_config_sync().is_ok();
    reset();
    if !(valid
        && rail_geometry_valid
        && stream_alignment_valid
        && grip_valid
        && config_restored
        && (dev_material_forced || every_material_paints_its_surface)
        && (dev_material_forced || theme_refresh_retints_surface))
    {
        crate::log_info!(
            "[smoke-keystroke-display-panel] which: structure={valid} rail={rail_geometry_valid} stream={stream_alignment_valid} grip={grip_valid} config={config_restored} surface={every_material_paints_its_surface} theme={theme_refresh_retints_surface}"
        );
    }
    valid
        && rail_geometry_valid
        && stream_alignment_valid
        && grip_valid
        && config_restored
        && (dev_material_forced || every_material_paints_its_surface)
        && (dev_material_forced || theme_refresh_retints_surface)
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

unsafe fn create_panel(
    orientation: Orientation,
) -> (*mut AnyObject, *mut AnyObject, *mut AnyObject) {
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
    *PANEL_BACKDROP.lock().unwrap() = Some(backdrop);
    release_obj(badge_container);
    release_obj(grip_view);
    // Paint (and record) the surface the palette calls for right away: the panel may be created while
    // no key is pressed, and until a render or a theme refresh runs the surface would otherwise be
    // whatever the material installed.
    apply_glass_properties();
    (panel, badge_container, grip_view)
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

fn update_grip_tracking(state: &mut PanelState, cursor_point: Option<NSPoint>) {
    if state.drag.is_some() {
        set_grip_mouse_state(state, true, GripCursor::Closed);
        return;
    }
    let panel_active = state.visible || state.fade_deadline.is_some();
    let inside_grip = if panel_active {
        cursor_point.is_some_and(|point| {
            state.panel.is_some_and(|window| unsafe {
                let frame: NSRect = crate::glass::panel_frame_of(window);
                let local = grip_frame(state.orientation, frame.size);
                contains(
                    NSRect::new(
                        NSPoint::new(
                            frame.origin.x + local.origin.x,
                            frame.origin.y + local.origin.y,
                        ),
                        local.size,
                    ),
                    point,
                )
            })
        })
    } else {
        false
    };
    set_grip_mouse_state(
        state,
        inside_grip,
        if inside_grip {
            GripCursor::Open
        } else {
            GripCursor::None
        },
    );
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

pub(super) unsafe fn apply_glass_properties() {
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
            backdrop.effect_view.is_some()
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

fn cached_badge_extent(
    cache: &mut HashMap<String, f64>,
    badge: &Badge,
    orientation: Orientation,
) -> BadgeExtent {
    crate::debug_assert_main_thread();
    if badge.cells.len() > 1 {
        // A container holding one keycap per key: side by side in a row, stacked in a column.
        let cells_width = cached_cells_width(cache, badge);
        let cells_height = cached_cells_height(cache, badge);
        if orientation.is_vertical() {
            let cell_w = badge
                .cells
                .iter()
                .map(|cell| cached_label_width(cache, cell.text()) + BADGE_CELL_PADDING_X * 2.0)
                .fold(0.0_f64, f64::max);
            let suffix_w = if badge.repeats > 1 {
                cached_label_width(cache, &format!("×{}", badge.repeats))
            } else {
                0.0
            };
            BadgeExtent {
                length: cells_height,
                thickness: BADGE_CONTAINER_PADDING_X * 2.0 + cell_w.max(suffix_w),
            }
        } else {
            let thickness = BADGE_H;
            let _ = cells_height;
            BadgeExtent {
                length: cells_width,
                thickness,
            }
        }
    } else {
        // A lone keycap keeps its natural size in both orientations, but which of that size is
        // "along the stream" swaps with the orientation: a row advances across its width, a
        // column down its height. In a column the merge count takes its own row below the
        // glyph (the render mirrors a chord's suffix row), so the height grows by that row
        // while the width stays the plain label's.
        let width = cached_label_width(cache, &badge.text) + BADGE_HORIZONTAL_PADDING;
        if orientation.is_vertical() {
            BadgeExtent {
                length: BADGE_H + repeat_suffix_row(badge.repeats),
                thickness: width,
            }
        } else {
            let length =
                (cached_label_width(cache, &badge_display_text(&badge.text, badge.repeats))
                    + BADGE_HORIZONTAL_PADDING)
                    .max(BADGE_MIN_WIDTH);
            BadgeExtent {
                length,
                thickness: BADGE_H,
            }
        }
    }
}

fn cached_cells_width(cache: &mut HashMap<String, f64>, badge: &Badge) -> f64 {
    let mut width = BADGE_CONTAINER_PADDING_X * 2.0;
    for (index, cell) in badge.cells.iter().enumerate() {
        if index > 0 {
            width += BADGE_CELL_GAP;
        }
        width += cached_label_width(cache, cell.text()) + BADGE_CELL_PADDING_X * 2.0;
    }
    if badge.repeats > 1 {
        width += BADGE_CELL_GAP + cached_label_width(cache, &format!("×{}", badge.repeats));
    }
    width
}

fn cached_cells_height(cache: &mut HashMap<String, f64>, badge: &Badge) -> f64 {
    let cell_h = (BADGE_H - BADGE_CELL_INSET_Y * 2.0).max(1.0);
    let mut height = BADGE_CONTAINER_PADDING_Y * 2.0;
    for index in 0..badge.cells.len() {
        if index > 0 {
            height += BADGE_CELL_GAP;
        }
        height += cell_h;
    }
    if badge.repeats > 1 {
        let _ = cache;
        height += BADGE_CELL_GAP + BADGE_REPEAT_SUFFIX_H;
    }
    height
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

/// Draw the keycap cells of a multi-key badge inside its container, one per key: modifiers keep
/// the accent tint while the key they combine with stays neutral.
unsafe fn layout_badge_cells(
    parent: *mut AnyObject,
    badge: &Badge,
    palette: &crate::theme::UiPalette,
    orientation: Orientation,
    badge_size: NSSize,
) {
    let cell_h = (BADGE_H - BADGE_CELL_INSET_Y * 2.0).max(1.0);
    let inner_radius = crate::theme::rounded_inset_radius(
        crate::theme::RADIUS_CONTROL - BADGE_CONTAINER_PADDING_X,
        100.0,
        cell_h,
    );
    // Each cell keeps its natural keycap size in both orientations; only the axis they advance
    // along changes. A column therefore holds the same keycaps a row would, stacked.
    if orientation.is_vertical() {
        // Center each keycap horizontally in the container so a column of differing widths reads
        // as one stack rather than a ragged left edge. Coordinates are non-flipped, so the first
        // keycap sits at the top and each subsequent one steps down; the repeat suffix, when
        // present, takes the row below the stack.
        let mut y = badge_size.height - BADGE_CONTAINER_PADDING_Y - cell_h;
        for cell in &badge.cells {
            let cell_w = (measure_text_width(cell.text()) + BADGE_CELL_PADDING_X * 2.0)
                .min((badge_size.width - BADGE_CONTAINER_PADDING_X * 2.0).max(1.0));
            let x = (badge_size.width - cell_w) / 2.0;
            add_badge_cell(
                parent,
                cell,
                NSRect::new(NSPoint::new(x, y), NSSize::new(cell_w, cell_h)),
                inner_radius,
                palette,
            );
            y -= cell_h + BADGE_CELL_GAP;
        }
        if badge.repeats > 1 {
            let suffix = format!("×{}", badge.repeats);
            // Full-width centered field: a frame of exactly the measured text width clips
            // the count's last digit (the text cell keeps a small inset of its own).
            add_badge_label(
                parent,
                NSRect::new(
                    NSPoint::new(0.0, BADGE_CONTAINER_PADDING_Y),
                    NSSize::new(badge_size.width, BADGE_REPEAT_SUFFIX_H),
                ),
                &suffix,
                palette.muted_text,
            );
        }
        return;
    }
    let mut x = BADGE_CONTAINER_PADDING_X;
    let y = BADGE_CELL_INSET_Y;
    for cell in &badge.cells {
        let cell_w = measure_text_width(cell.text()) + BADGE_CELL_PADDING_X * 2.0;
        add_badge_cell(
            parent,
            cell,
            NSRect::new(NSPoint::new(x, y), NSSize::new(cell_w, cell_h)),
            inner_radius,
            palette,
        );
        x += cell_w + BADGE_CELL_GAP;
    }
    if badge.repeats > 1 {
        let suffix = format!("×{}", badge.repeats);
        let suffix_w = measure_text_width(&suffix);
        // +8pt slack over the measured text: the text cell keeps a small inset of its own,
        // and an exact-width field clips the count's last digit.
        add_badge_label(
            parent,
            NSRect::new(NSPoint::new(x, 0.0), NSSize::new(suffix_w + 8.0, BADGE_H)),
            &suffix,
            palette.muted_text,
        );
    }
}

/// Draw one keycap cell: tinted by role, with its glyph centered inside it.
unsafe fn add_badge_cell(
    parent: *mut AnyObject,
    cell: &BadgeCell,
    frame: NSRect,
    inner_radius: f64,
    palette: &crate::theme::UiPalette,
) {
    let cell_w = frame.size.width;
    let cell_h = frame.size.height;
    let cell_view: *mut AnyObject = msg_send![class!(NSView), alloc];
    let cell_view: *mut AnyObject = msg_send![cell_view, initWithFrame: frame];
    let _: () = msg_send![cell_view, setWantsLayer: true];
    let cell_layer: *mut AnyObject = msg_send![cell_view, layer];
    let _: () = msg_send![cell_layer, setCornerRadius: inner_radius];
    let _: () = msg_send![cell_layer, setBorderWidth: 1.0f64];
    let (background, border, text_color) = if cell.is_accented_modifier() {
        (
            accent_keycap_surface(palette),
            palette.keycap_accent_border,
            palette.keycap_accent_text,
        )
    } else {
        let (fill, border, _) = neutral_keycap_surface(palette);
        (fill, border, palette.primary_text)
    };
    layer_set_background(cell_layer, hex_to_cg_color(background));
    layer_set_border(cell_layer, hex_to_cg_color(border));
    add_badge_label(
        cell_view,
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(cell_w, cell_h)),
        cell.text(),
        text_color,
    );
    let _: () = msg_send![parent, addSubview: cell_view];
    release_obj(cell_view);
}

fn centered_stream_cursor(panel_length: f64, estimated_total: f64, vertical: bool) -> f64 {
    let stream_length = (estimated_total - PANEL_SIDE_PADDING * 2.0).max(0.0);
    let leading_margin = ((panel_length - stream_length) / 2.0).max(0.0);
    if vertical {
        panel_length - leading_margin
    } else {
        leading_margin
    }
}

unsafe fn rebuild_badges(
    content: *mut AnyObject,
    badges: &[Badge],
    labels: &[String],
    extents: &[BadgeExtent],
    panel_size: NSSize,
    orientation: Orientation,
) {
    let old: *mut AnyObject = msg_send![content, subviews];
    let count: usize = msg_send![old, count];
    for index in (0..count).rev() {
        let view: *mut AnyObject = msg_send![old, objectAtIndex: index as isize];
        let _: () = msg_send![view, removeFromSuperview];
    }

    let total = estimated_stream_width(extents.iter().map(|extent| extent.length));
    let palette = crate::theme::ui_palette();
    // `total` includes nominal outer padding; center the actual keycap-and-gap stream so
    // any extra room in the panel is split evenly instead of being added to only one end.
    let panel_length = if orientation.is_vertical() {
        panel_size.height
    } else {
        panel_size.width
    };
    // Non-flipped coordinates count a column down from the top and a row up from the left.
    let mut cursor = centered_stream_cursor(panel_length, total, orientation.is_vertical());
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

        if badge.cells.len() > 1 {
            // One container holding one keycap per key. The container is a subtle tray so the
            // cells and their per-role tints stay legible against it.
            // The tray is the surface *behind* the keys, so the keys stay visible against it in
            // both modes (see `neutral_keycap_surface`).
            layer_set_background(layer, hex_to_cg_color(neutral_keycap_surface(&palette).2));
            layer_set_border(layer, hex_to_cg_color(palette.card_border));
            layout_badge_cells(badge_view, badge, &palette, orientation, badge_size);
        } else {
            let (background, border, text_color) = if uses_accent_fill(badge.kind) {
                (
                    accent_keycap_surface(&palette),
                    palette.keycap_accent_border,
                    palette.keycap_accent_text,
                )
            } else {
                let (fill, border, _) = neutral_keycap_surface(&palette);
                (fill, border, palette.primary_text)
            };
            // The chip is an opaque surface: the panel behind it is the user's material, so the text
            // floors cannot depend on what the panel happens to cover (see `opaque_over`).
            layer_set_background(layer, hex_to_cg_color(background));
            layer_set_border(layer, hex_to_cg_color(border));
            if orientation.is_vertical() && badge.repeats > 1 {
                // A merged keycap in a column keeps its glyph in the top cap-height zone and
                // puts the count on its own small row below (non-flipped coordinates: the top
                // zone starts at height - BADGE_H), mirroring a chord's suffix row, so the
                // count never widens the keycap.
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
                        NSPoint::new(0.0, BADGE_CONTAINER_PADDING_Y),
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
        }
        let _: () = msg_send![content, addSubview: badge_view];
        release_obj(badge_view);
        if orientation.is_vertical() {
            cursor -= BADGE_GAP;
        } else {
            cursor += extent.length + BADGE_GAP;
        }
    }
}

/// The surface of a neutral keycap and of the tray a chord's keys sit in: `(fill, border, tray)`.
///
/// The two modes need different fills, and the reason is measurable text contrast:
/// - **Light**: the card surface at 80% alpha composited to white on the near-white panel (1.05:1),
///   so the cap had no visible surface at all. The inset field surface reads at 1.12:1 there, and its
///   text measures 11.6:1 primary / 4.6:1 muted (the values the design doc records for light keycaps).
/// - **Dark**: the same lighter inset surface dropped the cap's text to 9.77:1 primary and 4.00:1
///   muted, under the palette's 12:1 and 4.5:1 floors, because light text needs a *dark* cap. Dark
///   therefore keeps the card surface at 80% (12.67:1 / 5.19:1), and its tray stays `field_bg`.
fn neutral_keycap_surface(palette: &crate::theme::UiPalette) -> (u32, u32, u32) {
    if palette.dark {
        (
            palette.card_bg,
            palette.card_border,
            opaque_over(palette.field_bg, palette.card_bg),
        )
    } else {
        (
            opaque_over(palette.field_bg, palette.card_bg),
            palette.card_border,
            palette.card_bg,
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
        accent_keycap_surface, centered_stream_cursor, default_edge_frame, neutral_keycap_surface,
        opaque_over, resize_frame_preserving_center, resolve_panel_frame, target_screen_index,
        uses_accent_fill, Badge, BadgeCell, Orientation, ScreenGeometry, BADGE_H,
    };
    use crate::config::KeystrokeDisplayPosition;
    use crate::keystroke_display::state::{BadgeKind, KEYCAP_RAIL_W};
    use objc2_foundation::{NSPoint, NSRect, NSSize};

    #[test]
    fn single_key_streams_are_centered_with_equal_end_margins_in_both_orientations() {
        // A one-letter row uses a 32pt keycap in a 64pt panel; a column uses a 34pt
        // keycap in a 64pt panel. `estimated_total` includes 12pt nominal padding per end.
        let horizontal_cursor = centered_stream_cursor(64.0, 56.0, false);
        assert_eq!(horizontal_cursor, 16.0);
        assert_eq!(64.0 - horizontal_cursor - 32.0, horizontal_cursor);

        let vertical_cursor = centered_stream_cursor(64.0, 58.0, true);
        let vertical_keycap_start = vertical_cursor - 34.0;
        assert_eq!(vertical_keycap_start, 15.0);
        assert_eq!(vertical_keycap_start, 64.0 - vertical_cursor);
    }

    #[test]
    fn keycap_text_is_a_recorded_class_over_both_extreme_backdrops() {
        // The panel floats over arbitrary content and the frost wash is only 0xE9 opaque, so the cap
        // surface moves with the backdrop: a black backdrop lands the light panel at (225,226,228), a
        // white one at (247,248,250). Everything a keycap shows is therefore measured over the FULL
        // chain -- backdrop -> wash -> cap -> text -- at both extremes, for both cap kinds.
        //
        // Keycap text is a recorded class of its own rather than `text_primary`'s 12:1 floor, which
        // applies to text on `window_bg`/`card_bg`: the shipped accent keycaps measure 8.48:1 primary
        // and 3.37:1 muted on the worst backdrop (a light cap over dark content), and the two floors
        // below sit just under those. This is also why the light neutral cap keeps the inset surface
        // (1.12:1 against the panel) even though it cannot reach 12:1: a cap dark enough to read as a
        // surface on a near-white panel cannot also carry 12:1 text.
        const KEYCAP_TEXT_PRIMARY_FLOOR: f64 = 8.0;
        const KEYCAP_TEXT_MUTED_FLOOR: f64 = 3.25;
        for dark in [false, true] {
            let palette = crate::theme::ui_palette_for_mode(dark);
            let (fill, border, tray) = neutral_keycap_surface(&palette);
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
            // The fill and tray identity per mode (the choice this test documents).
            assert_eq!(border, palette.card_border, "dark={dark}");
            if dark {
                assert_eq!(fill, palette.card_bg, "dark keycaps are the card surface");
                assert_eq!(
                    tray,
                    opaque_over(palette.field_bg, palette.card_bg),
                    "dark chord tray"
                );
            } else {
                assert_eq!(
                    fill,
                    opaque_over(palette.field_bg, palette.card_bg),
                    "light keycaps use the inset surface, flattened"
                );
                assert_eq!(tray, palette.card_bg, "light chord tray");
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
        assert_eq!(dot_center_from_panel_top, 9.0);

        let horizontal = super::grip_frame(Orientation::Horizontal, panel_size);
        assert_eq!(horizontal.size, NSSize::new(20.0, 48.0));
        assert_eq!(horizontal.origin.x, 0.0);
        let first_horizontal_dot = super::grip_marker_frame(Orientation::Horizontal, 0);
        assert_eq!(
            horizontal.origin.x
                + first_horizontal_dot.origin.x
                + first_horizontal_dot.size.width / 2.0,
            super::GRIP_MARK_CENTER_INSET
        );
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

    /// Horizontal and vertical extents differ for a chord: a row grows sideways, a column grows
    /// downward, and a lone keycap is the same either way.
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
        let row = chord.estimated_extent(Orientation::Horizontal);
        let column = chord.estimated_extent(Orientation::Vertical);
        assert!(
            column > row,
            "a stacked chord must be taller than it is wide ({column} vs {row})"
        );
        assert_eq!(
            column,
            2.0 * BADGE_H + super::BADGE_GAP,
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
            triple_chord.estimated_extent(Orientation::Vertical),
            3.0 * BADGE_H + 2.0 * super::BADGE_GAP
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
}
