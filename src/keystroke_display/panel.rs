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
    estimated_stream_width, Badge, BadgeCell, BadgeKind, BADGE_CELL_GAP, BADGE_CELL_INSET_Y,
    BADGE_CELL_PADDING_X, BADGE_CONTAINER_PADDING_X, BADGE_GAP, BADGE_HORIZONTAL_PADDING,
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

const PANEL_H: f64 = 54.0;
const PANEL_BOTTOM_MARGIN: f64 = 18.0;
const BADGE_H: f64 = 34.0;
const HIDE_FADE: Duration = Duration::from_millis(
    (crate::theme::ANIMATION_DURATION_MEDIUM * crate::theme::ANIMATION_EXIT_RATIO * 1000.0) as u64,
);
const PANEL_TIMER_INTERVAL: f64 = 0.016;
const MAX_MEASUREMENTS: usize = 512;
const MAX_TEXT_CENTROID_OFFSET: f64 = 2.0;
const KEYCAP_FILL_ALPHA: u32 = 0xCC;
const GRIP_WIDTH: f64 = 10.0;
const GRIP_HEIGHT: f64 = 40.0;
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
    /// Set once the bar has hit the width cap this session; it stays there until the panel hides,
    /// so keys rolling off the front never make the length breathe.
    latched: bool,
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

pub(super) fn target_screen_width(
    display_position: &str,
    position: Option<KeystrokeDisplayPosition>,
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
    screens
        .get(selected)
        .map_or(1.0, |screen| screen.frame.size.width.max(1.0))
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
        let frame: NSRect = unsafe { msg_send![window, frame] };
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
        let frame: NSRect = unsafe { msg_send![window, frame] };
        let local_grip = grip_frame();
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
                let _: () = msg_send![window, setFrame: frame, display: true];
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
                let frame: NSRect = msg_send![window, frame];
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
                let frame: NSRect = msg_send![window, frame];
                let default_frame = default_bottom_center_frame(screen.visible_frame, frame.size);
                let _: () = msg_send![window, setFrame: default_frame, display: true];
            }
        }
    });
}

pub(super) fn render(
    badges: &[Badge],
    visible: bool,
    capped: bool,
    display_position: &str,
    position: Option<KeystrokeDisplayPosition>,
    now: Instant,
    cursor_point: Option<NSPoint>,
) -> Option<bool> {
    crate::debug_assert_main_thread();
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
                let (panel, badge_container, grip_view) = unsafe { create_panel() };
                state.panel = Some(panel);
                state.badge_container = Some(badge_container);
                state.grip_view = Some(grip_view);
                panel
            };
            let badge_container = state
                .badge_container
                .expect("a created keystroke panel has a badge container");
            let labels = badge_labels(badges);
            let mut widths = Vec::with_capacity(badges.len());
            for badge in badges.iter() {
                widths.push(cached_badge_width(&mut state.measurements, badge));
            }
            let max_width = screen_frame.size.width * 0.5;
            let mut desired = estimated_stream_width(widths.iter().copied());
            let mut first = 0usize;
            while widths.len() > 1 && desired > max_width {
                widths.remove(0);
                first += 1;
                desired = estimated_stream_width(widths.iter().copied());
            }
            let shown_badges = &badges[first..];
            let shown_labels = &labels[first..];
            // Only this final per-badge clamp can ellipsize a genuinely oversized badge.
            for width in &mut widths {
                *width = width.min((max_width - PANEL_SIDE_PADDING * 2.0).max(32.0));
            }
            desired = estimated_stream_width(widths.iter().copied());
            // The stream filled the cap this session: pin the bar to the cap so keys rolling off
            // the front never shorten it (the length stops changing at the cap). `first > 0`
            // covers the panel's own measured trim, which can fire a little before the state's
            // estimate-based trim does.
            let pinned = capped || state.latched || first > 0;
            state.latched = pinned;
            let panel_w = if pinned {
                max_width
            } else {
                desired.min(max_width).max(max_width.min(64.0))
            };
            let size = NSSize::new(panel_w, PANEL_H);
            let frame = unsafe {
                if state.drag.is_some() {
                    let current: NSRect = msg_send![panel, frame];
                    NSRect::new(current.origin, size)
                } else if !reopened {
                    let current: NSRect = msg_send![panel, frame];
                    resize_frame_preserving_center(current, size, geometry.visible_frame)
                } else {
                    resolve_panel_frame(position, &[geometry], 0, size).1
                }
            };
            unsafe {
                if reopened {
                    state.last_badges.clear();
                    set_alpha_immediately(panel, 1.0);
                    let _: () = msg_send![panel, orderFront: std::ptr::null::<AnyObject>()];
                }
                let _: () = msg_send![panel, setFrame: frame, display: true];
                if let Some(grip_view) = state.grip_view {
                    let _: () = msg_send![grip_view, setFrame: grip_frame()];
                    update_grip_marker(grip_view);
                }
                let palette = crate::theme::ui_palette();
                if state.last_badges != badges || state.last_palette != Some(palette) {
                    rebuild_badges(
                        badge_container,
                        shown_badges,
                        shown_labels,
                        &widths,
                        panel_w,
                    );
                    state.last_badges = badges.to_vec();
                    state.last_palette = Some(palette);
                }
                let _ = state.fade_deadline.take();
            }
            state.visible = true;
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
    });
}

pub(super) fn smoke_runner() -> bool {
    crate::debug_assert_main_thread();
    super::mapping::refresh_layout_cache();
    let option_q_is_unmodified = super::mapping::key_glyph(
        crate::event_tap::keyboard::VK_Q,
        "œ",
        crate::event_tap::keyboard::FLAG_OPTION,
    )
    .as_deref()
        == Some("q");
    let initial_position = crate::config::CONFIG
        .read()
        .unwrap()
        .keystroke_display
        .position;
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
        "main",
        initial_position,
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
        "main",
        initial_position,
        smoke_now,
        None,
    );
    let _ = render(&[], false, false, "main", initial_position, smoke_now, None);
    let _ = render(
        &[Badge {
            text: "⌘Q".into(),
            kind: BadgeKind::Chord,
            repeats: 2,
            cells: Vec::new(),
        }],
        true,
        false,
        "main",
        initial_position,
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
            text: "漢字かな".into(),
            kind: BadgeKind::Chord,
            repeats: 12,
            cells: Vec::new(),
        },
    ];
    let _ = render(
        &cjk_badges,
        true,
        false,
        "main",
        initial_position,
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
            let frame: NSRect = msg_send![panel, frame];
            let alpha: f64 = msg_send![panel, alphaValue];
            let grip_frame: NSRect = msg_send![grip_view, frame];
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
                let text = badge_display_text(&badge.text, badge.repeats);
                let measured = measure_text_width(&text);
                if badge_frame.size.width + 0.5 < measured + BADGE_HORIZONTAL_PADDING {
                    badges_fit = false;
                    break;
                }
            }
            visible
                && alpha >= 0.99
                && grip_frame.origin.x.abs() < 0.01
                && (grip_frame.origin.y - (PANEL_H - GRIP_HEIGHT) / 2.0).abs() < 0.01
                && frame.size.width <= screen_width * 0.5 + 0.5
                && typical_glyph_advances
                && option_q_is_unmodified
                && backdrop_structure_valid(panel, badge_container)
                && backdrop_frost_mask_valid()
                && centroid_offsets.as_ref().is_some_and(|offsets| {
                    offsets.len() == cjk_badges.len()
                        && offsets.iter().all(|(offset, pixels)| {
                            offset.abs() <= MAX_TEXT_CENTROID_OFFSET && *pixels > 0
                        })
                })
                && badges_fit
        }
    });
    let grip_valid = smoke_grip_interaction(initial_position);
    update_config_position(initial_position);
    let config_restored = crate::config::flush_config_sync().is_ok();
    reset();
    valid && grip_valid && config_restored
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
        let frame: NSRect = unsafe { msg_send![window, frame] };
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
        let frame: NSRect = unsafe { msg_send![window, frame] };
        let ignores_mouse_events: bool = unsafe { msg_send![window, ignoresMouseEvents] };
        let config = crate::config::CONFIG
            .read()
            .unwrap()
            .keystroke_display
            .clone();
        let screens = unsafe { screen_geometries() };
        let target_index = target_screen_index_live(&config.display_position, &screens);
        let expected = screens
            .get(target_index)
            .map(|screen| default_bottom_center_frame(screen.visible_frame, frame.size));
        config.position.is_none()
            && ignores_mouse_events
            && frame_after_drag.is_some_and(|dragged| {
                (frame_before_reset.origin.x - dragged.origin.x).abs() < 0.5
                    && (frame_before_reset.origin.y - dragged.origin.y).abs() < 0.5
            })
            && expected.is_some_and(|expected| {
                (frame.origin.x - expected.origin.x).abs() < 0.5
                    && (frame.origin.y - expected.origin.y).abs() < 0.5
            })
            && (frame_before_reset.origin.x - frame.origin.x).abs() > 0.0
    });
    moved_and_saved && reset_to_default
}

fn panel_frame_and_grip_center() -> Option<(NSRect, NSPoint)> {
    PANEL.with(|panel| {
        let state = panel.borrow();
        let window = state.panel?;
        let frame: NSRect = unsafe { msg_send![window, frame] };
        let grip = grip_frame();
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
        if field_count != 1 {
            return None;
        }
        let field: *mut AnyObject = msg_send![fields, objectAtIndex: 0isize];
        let field_frame: NSRect = msg_send![field, frame];
        let label = badge_display_text(&badge.text, badge.repeats);
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
            for px in px_min..px_max {
                let color: *mut AnyObject =
                    msg_send![bitmap, colorAtX: px as isize, y: py as isize];
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
        let badge_center_y = badge_frame.origin.y + badge_frame.size.height / 2.0;
        offsets.push((centroid_y - badge_center_y, pixel_count));
    }
    Some(offsets)
}

unsafe fn create_panel() -> (*mut AnyObject, *mut AnyObject, *mut AnyObject) {
    let panel: *mut AnyObject = msg_send![class!(NSPanel), alloc];
    let panel: *mut AnyObject = msg_send![
        panel,
        initWithContentRect: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(64.0, PANEL_H)),
        styleMask: (1u64 << 7),
        backing: 2u64,
        defer: false
    ];
    let _: () = msg_send![panel, setLevel: 3isize];
    let _: () = msg_send![panel, setOpaque: false];
    let _: () = msg_send![panel, setHasShadow: true];
    let _: () = msg_send![panel, setIgnoresMouseEvents: true];
    let _: () = msg_send![panel, setHidesOnDeactivate: false];
    let _: () = msg_send![panel, setReleasedWhenClosed: false];
    let _: () = msg_send![panel, setCollectionBehavior: ((1u64 << 0) | (1u64 << 6) | (1u64 << 8))];
    let clear: *mut AnyObject = msg_send![class!(NSColor), clearColor];
    let _: () = msg_send![panel, setBackgroundColor: clear];
    let local_frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(64.0, PANEL_H));
    let backdrop = crate::glass::install_backdrop(
        panel,
        local_frame,
        crate::glass::PANEL_CORNER_RADIUS,
        Some(crate::glass::INACTIVE_GLASS_COMPENSATION_ALPHA),
    );
    let badge_container: *mut AnyObject = msg_send![class!(NSView), alloc];
    let badge_container: *mut AnyObject = msg_send![badge_container, initWithFrame: local_frame];
    let _: () = msg_send![badge_container, setWantsLayer: true];
    let _: () = msg_send![badge_container, setAutoresizingMask: 18u64];
    let _: () = msg_send![backdrop.content_parent, addSubview: badge_container];
    let grip_view: *mut AnyObject = msg_send![grip_view_class(), alloc];
    let grip_view: *mut AnyObject = msg_send![grip_view, initWithFrame: grip_frame()];
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
    update_grip_marker(grip_view);
    *PANEL_BACKDROP.lock().unwrap() = Some(backdrop);
    release_obj(badge_container);
    release_obj(grip_view);
    (panel, badge_container, grip_view)
}

fn grip_frame() -> NSRect {
    NSRect::new(
        NSPoint::new(0.0, (PANEL_H - GRIP_HEIGHT) / 2.0),
        NSSize::new(GRIP_WIDTH, GRIP_HEIGHT),
    )
}

unsafe fn update_grip_marker(grip_view: *mut AnyObject) {
    let marks: *mut AnyObject = msg_send![grip_view, subviews];
    let count: usize = msg_send![marks, count];
    let total_height = 3.0 * GRIP_MARK_HEIGHT + 2.0 * GRIP_MARK_GAP;
    let x = (GRIP_WIDTH - GRIP_MARK_WIDTH) / 2.0;
    let start_y = (GRIP_HEIGHT - total_height) / 2.0;
    let mark_color = hex_to_cg_color(crate::theme::ui_palette().secondary_text);
    for index in 0..count.min(3) {
        let mark: *mut AnyObject = msg_send![marks, objectAtIndex: index as isize];
        let _: () = msg_send![mark, setFrame: NSRect::new(
            NSPoint::new(x, start_y + index as f64 * (GRIP_MARK_HEIGHT + GRIP_MARK_GAP)),
            NSSize::new(GRIP_MARK_WIDTH, GRIP_MARK_HEIGHT)
        )];
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
                let frame: NSRect = msg_send![window, frame];
                let local = grip_frame();
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
        let is_glass: bool = msg_send![root, isKindOfClass: glass_class];
        let radius: f64 = msg_send![glass.0, cornerRadius];
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
        is_glass
            && (radius - crate::glass::PANEL_CORNER_RADIUS).abs() < 0.01
            && !inner.is_null()
            && fill_is_child
            && badges_are_child
            && !fill_layer.0.is_null()
    } else if let Some(effect) = backdrop.effect_view {
        let Some(effect_class) = objc2::runtime::AnyClass::get(c"NSVisualEffectView") else {
            return false;
        };
        let is_effect: bool = msg_send![effect.0, isKindOfClass: effect_class];
        let effect_layer: *mut AnyObject = msg_send![effect.0, layer];
        let radius: f64 = msg_send![effect_layer, cornerRadius];
        // Every material now installs its root view as the window's content view directly.
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
        Some(crate::glass::INACTIVE_GLASS_COMPENSATION_ALPHA),
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
    let frame_rect: NSRect = msg_send![panel, frame];
    let content_rect: NSRect = msg_send![panel, contentRectForFrameRect: frame_rect];
    let new = crate::glass::swap_backdrop(
        panel,
        &old,
        content_rect,
        crate::glass::PANEL_CORNER_RADIUS,
        Some(crate::glass::INACTIVE_GLASS_COMPENSATION_ALPHA),
    );
    *PANEL_BACKDROP.lock().unwrap() = Some(new);
    apply_glass_properties();
}

unsafe fn set_alpha_immediately(panel: *mut AnyObject, alpha: f64) {
    let _: () = msg_send![panel, setAlphaValue: alpha];
}

fn cached_badge_width(cache: &mut HashMap<String, f64>, badge: &Badge) -> f64 {
    crate::debug_assert_main_thread();
    if badge.cells.len() > 1 {
        // A container holding one keycap cell per key, plus the repeat suffix after the cells.
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
    } else {
        cached_label_width(cache, &badge_display_text(&badge.text, badge.repeats))
            + BADGE_HORIZONTAL_PADDING
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

fn badge_display_text(label: &str, repeats: u32) -> String {
    if repeats > 1 {
        format!("{}  ×{}", label, repeats)
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
) {
    let cell_h = (BADGE_H - BADGE_CELL_INSET_Y * 2.0).max(1.0);
    let inner_radius = crate::theme::rounded_inset_radius(
        crate::theme::RADIUS_CONTROL - BADGE_CONTAINER_PADDING_X,
        100.0,
        cell_h,
    );
    let mut x = BADGE_CONTAINER_PADDING_X;
    for cell in &badge.cells {
        let cell_w = measure_text_width(cell.text()) + BADGE_CELL_PADDING_X * 2.0;
        let cell_view: *mut AnyObject = msg_send![class!(NSView), alloc];
        let cell_view: *mut AnyObject = msg_send![cell_view, initWithFrame: NSRect::new(NSPoint::new(x, BADGE_CELL_INSET_Y), NSSize::new(cell_w, cell_h))];
        let _: () = msg_send![cell_view, setWantsLayer: true];
        let cell_layer: *mut AnyObject = msg_send![cell_view, layer];
        let _: () = msg_send![cell_layer, setCornerRadius: inner_radius];
        let _: () = msg_send![cell_layer, setBorderWidth: 1.0f64];
        let (background, border, text_color) = if cell.is_modifier() {
            (
                palette.keycap_accent_bg,
                palette.keycap_accent_border,
                palette.keycap_accent_text,
            )
        } else {
            (
                keycap_fill(palette.card_bg),
                palette.card_border,
                palette.primary_text,
            )
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
        x += cell_w + BADGE_CELL_GAP;
    }
    if badge.repeats > 1 {
        let suffix = format!("×{}", badge.repeats);
        let suffix_w = measure_text_width(&suffix);
        add_badge_label(
            parent,
            NSRect::new(NSPoint::new(x, 0.0), NSSize::new(suffix_w, BADGE_H)),
            &suffix,
            palette.primary_text,
        );
    }
}

unsafe fn rebuild_badges(
    content: *mut AnyObject,
    badges: &[Badge],
    labels: &[String],
    widths: &[f64],
    panel_width: f64,
) {
    let old: *mut AnyObject = msg_send![content, subviews];
    let count: usize = msg_send![old, count];
    for index in (0..count).rev() {
        let view: *mut AnyObject = msg_send![old, objectAtIndex: index as isize];
        let _: () = msg_send![view, removeFromSuperview];
    }

    let total = estimated_stream_width(widths.iter().copied());
    let mut x = ((panel_width - total) / 2.0).max(PANEL_SIDE_PADDING);
    let palette = crate::theme::ui_palette();
    for ((badge, label), width) in badges.iter().zip(labels).zip(widths) {
        let badge_view: *mut AnyObject = msg_send![class!(NSView), alloc];
        let badge_view: *mut AnyObject = msg_send![badge_view, initWithFrame: NSRect::new(NSPoint::new(x, 10.0), NSSize::new(*width, BADGE_H))];
        let _: () = msg_send![badge_view, setWantsLayer: true];
        let layer: *mut AnyObject = msg_send![badge_view, layer];
        let _: () = msg_send![layer, setCornerRadius: crate::theme::RADIUS_CONTROL];
        let _: () = msg_send![layer, setBorderWidth: 1.0f64];

        if badge.cells.len() > 1 {
            // One container holding one keycap per key. The container is a subtle tray so the
            // cells and their per-role tints stay legible against it.
            layer_set_background(layer, hex_to_cg_color(palette.field_bg));
            layer_set_border(layer, hex_to_cg_color(palette.card_border));
            layout_badge_cells(badge_view, badge, &palette);
        } else {
            let (background, border, text_color) = if uses_accent_fill(badge.kind) {
                (
                    palette.keycap_accent_bg,
                    palette.keycap_accent_border,
                    palette.keycap_accent_text,
                )
            } else {
                (
                    keycap_fill(palette.card_bg),
                    palette.card_border,
                    palette.primary_text,
                )
            };
            // Dark text stays readable on light capsules and white text on dark ones at 90% alpha;
            // the glass shows through subtly without changing either theme's tuned RGB values.
            layer_set_background(layer, hex_to_cg_color(background));
            layer_set_border(layer, hex_to_cg_color(border));
            let display_text = badge_display_text(label, badge.repeats);
            add_badge_label(
                badge_view,
                NSRect::new(
                    NSPoint::new(6.0, 0.0),
                    NSSize::new((*width - 12.0).max(1.0), BADGE_H),
                ),
                &display_text,
                text_color,
            );
        }
        let _: () = msg_send![content, addSubview: badge_view];
        release_obj(badge_view);
        x += *width + BADGE_GAP;
    }
}

fn uses_accent_fill(kind: BadgeKind) -> bool {
    matches!(kind, BadgeKind::Modifier | BadgeKind::Indicator)
}

fn keycap_fill(color: u32) -> u32 {
    (color & 0xFFFF_FF00) | KEYCAP_FILL_ALPHA
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

fn default_bottom_center_frame(visible: NSRect, size: NSSize) -> NSRect {
    let origin = NSPoint::new(
        visible.origin.x + (visible.size.width - size.width) / 2.0,
        visible.origin.y + PANEL_BOTTOM_MARGIN,
    );
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
) -> (usize, NSRect) {
    if screens.is_empty() {
        return (0, NSRect::new(NSPoint::new(0.0, 0.0), size));
    }
    let target_index = target_index.min(screens.len().saturating_sub(1));
    let Some(screen_index) = screen_for_saved_origin(position, screens) else {
        return (
            target_index,
            default_bottom_center_frame(screens[target_index].visible_frame, size),
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
        keycap_fill, resize_frame_preserving_center, resolve_panel_frame, target_screen_index,
        uses_accent_fill, ScreenGeometry, KEYCAP_FILL_ALPHA,
    };
    use crate::config::KeystrokeDisplayPosition;
    use crate::keystroke_display::state::BadgeKind;
    use objc2_foundation::{NSPoint, NSRect, NSSize};

    #[test]
    fn keycap_fills_use_eighty_percent_alpha_and_preserve_theme_rgb() {
        for source in [0x2C2C2EEA, 0xFFFFFFD1] {
            let fill = keycap_fill(source);
            assert_eq!(fill & 0xFFFF_FF00, source & 0xFFFF_FF00);
            assert_eq!(fill & 0xFF, KEYCAP_FILL_ALPHA);
        }
    }

    #[test]
    fn released_modifier_badges_use_the_neutral_keycap_fill() {
        assert!(uses_accent_fill(BadgeKind::Modifier));
        assert!(!uses_accent_fill(BadgeKind::ModifierReleased));
        assert!(uses_accent_fill(BadgeKind::Indicator));
    }

    #[test]
    fn keycap_hide_fade_uses_the_shared_exit_duration() {
        assert_eq!(super::HIDE_FADE.as_millis(), 285);
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
        let (screen_index, frame) =
            resolve_panel_frame(saved, &virtual_screens(), 1, NSSize::new(300.0, 54.0));
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
        let (screen_index, frame) =
            resolve_panel_frame(None, &virtual_screens(), 1, NSSize::new(300.0, 54.0));
        assert_eq!(screen_index, 1);
        assert_eq!(frame.origin, NSPoint::new(-790.0, 38.0));
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
