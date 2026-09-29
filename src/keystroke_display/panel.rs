//! Non-activating floating keycap panel and its active-only main-runloop timer.

use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::time::{Duration, Instant};

use super::state::{
    estimated_stream_width, Badge, BadgeKind, BADGE_GAP, BADGE_HORIZONTAL_PADDING,
    PANEL_SIDE_PADDING,
};
use crate::event_tap;
use crate::ffi::{
    hex_to_cg_color, layer_set_background, layer_set_border, make_nsstring, release_obj, CFRelease,
};

const PANEL_H: f64 = 54.0;
const PANEL_BOTTOM_MARGIN: f64 = 18.0;
const BADGE_H: f64 = 34.0;
const HIDE_FADE: Duration = Duration::from_millis(200);
const PANEL_TIMER_INTERVAL: f64 = 0.016;
const MAX_MEASUREMENTS: usize = 512;
const MAX_TEXT_CENTROID_OFFSET: f64 = 2.0;

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
    target_frame: Option<(NSRect, NSRect)>,
    last_badges: Vec<Badge>,
    last_palette: Option<crate::theme::UiPalette>,
    measurements: HashMap<String, f64>,
}

thread_local! {
    static PANEL: RefCell<PanelState> = RefCell::new(PanelState::default());
}

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

pub(super) fn target_screen_width(follow_frontmost: bool) -> f64 {
    crate::debug_assert_main_thread();
    unsafe { target_screen(follow_frontmost).0.size.width.max(1.0) }
}

pub(super) fn render(badges: &[Badge], visible: bool, follow_frontmost: bool) {
    crate::debug_assert_main_thread();
    PANEL.with(|panel| {
        let mut state = panel.borrow_mut();
        if visible && !badges.is_empty() {
            let reopened = !state.visible;
            let (screen_frame, visible_frame) = if reopened {
                let target = unsafe { target_screen(follow_frontmost) };
                state.target_frame = Some(target);
                target
            } else {
                if let Some(target) = state.target_frame {
                    target
                } else {
                    let target = unsafe { target_screen(follow_frontmost) };
                    state.target_frame = Some(target);
                    target
                }
            };
            let panel = *state.panel.get_or_insert_with(|| unsafe { create_panel() });
            let labels = badge_labels(badges);
            let mut widths = Vec::with_capacity(badges.len());
            for (badge, text) in badges.iter().zip(&labels) {
                widths.push(cached_badge_width(
                    &mut state.measurements,
                    text,
                    badge.repeats,
                ));
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
            let panel_w = desired.min(max_width).max(max_width.min(64.0));
            let frame = NSRect::new(
                NSPoint::new(
                    visible_frame.origin.x + (visible_frame.size.width - panel_w) / 2.0,
                    visible_frame.origin.y + PANEL_BOTTOM_MARGIN,
                ),
                NSSize::new(panel_w, PANEL_H),
            );
            unsafe {
                if reopened {
                    state.last_badges.clear();
                    set_alpha_immediately(panel, 1.0);
                    let _: () = msg_send![panel, orderFront: std::ptr::null::<AnyObject>()];
                }
                let _: () = msg_send![panel, setFrame: frame, display: true];
                let content: *mut AnyObject = msg_send![panel, contentView];
                let layer: *mut AnyObject = msg_send![content, layer];
                let palette = crate::theme::ui_palette();
                if state.last_badges != badges || state.last_palette != Some(palette) {
                    layer_set_background(layer, hex_to_cg_color(palette.window_bg));
                    rebuild_badges(panel, shown_badges, shown_labels, &widths, panel_w);
                    state.last_badges = badges.to_vec();
                    state.last_palette = Some(palette);
                }
                let _ = state.fade_deadline.take();
            }
            state.visible = true;
            state.target_frame = Some((screen_frame, visible_frame));
        } else {
            if state.visible {
                state.visible = false;
                state.last_badges.clear();
                state.fade_deadline = Some(Instant::now() + HIDE_FADE);
                if let Some(panel) = state.panel {
                    unsafe {
                        let _: () = msg_send![class!(NSAnimationContext), beginGrouping];
                        let context: *mut AnyObject =
                            msg_send![class!(NSAnimationContext), currentContext];
                        let _: () = msg_send![context, setDuration: HIDE_FADE.as_secs_f64()];
                        let animator: *mut AnyObject = msg_send![panel, animator];
                        let _: () = msg_send![animator, setAlphaValue: 0.0f64];
                        let _: () = msg_send![class!(NSAnimationContext), endGrouping];
                    }
                } else {
                    state.fade_deadline = None;
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
            }
        }
    });
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
        state.last_badges.clear();
        state.last_palette = None;
        state.measurements.clear();
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
    let badge = Badge {
        text: "⌘Q".into(),
        kind: BadgeKind::Chord,
        repeats: 2,
    };
    render(&[badge], true, false);
    render(&[], false, false);
    render(
        &[Badge {
            text: "⌘Q".into(),
            kind: BadgeKind::Chord,
            repeats: 2,
        }],
        true,
        false,
    );
    let cjk_badges = [
        Badge {
            text: "中文".into(),
            kind: BadgeKind::TextRun,
            repeats: 1,
        },
        Badge {
            text: "漢字かな".into(),
            kind: BadgeKind::Chord,
            repeats: 12,
        },
    ];
    render(&cjk_badges, true, false);
    let centroid_offsets = PANEL.with(|panel| {
        let state = panel.borrow();
        state
            .panel
            .and_then(|panel| unsafe { text_centroid_offsets(panel, &cjk_badges) })
    });
    eprintln!("[keystroke-display-smoke] text-centroid-offsets-pt={centroid_offsets:?}");
    let typical_glyph_advances = unsafe {
        let cjk = measure_text_width("中");
        let emoji = measure_text_width("🙂");
        (14.0..=18.0).contains(&cjk) && (16.0..=24.0).contains(&emoji)
    };
    let valid = PANEL.with(|panel| {
        let state = panel.borrow();
        let Some(panel) = state.panel else {
            return false;
        };
        unsafe {
            let visible: bool = msg_send![panel, isVisible];
            let frame: NSRect = msg_send![panel, frame];
            let alpha: f64 = msg_send![panel, alphaValue];
            let screen_width = target_screen(false).0.size.width;
            let content: *mut AnyObject = msg_send![panel, contentView];
            let views: *mut AnyObject = msg_send![content, subviews];
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
                && frame.size.width <= screen_width * 0.5 + 0.5
                && typical_glyph_advances
                && option_q_is_unmodified
                && centroid_offsets.as_ref().is_some_and(|offsets| {
                    offsets.len() == cjk_badges.len()
                        && offsets.iter().all(|(offset, pixels)| {
                            offset.abs() <= MAX_TEXT_CENTROID_OFFSET && *pixels > 0
                        })
                })
                && badges_fit
        }
    });
    reset();
    valid
}

unsafe fn text_centroid_offsets(
    panel: *mut AnyObject,
    badges: &[Badge],
) -> Option<Vec<(f64, usize)>> {
    let content: *mut AnyObject = msg_send![panel, contentView];
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

unsafe fn create_panel() -> *mut AnyObject {
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
    let content: *mut AnyObject = msg_send![panel, contentView];
    let _: () = msg_send![content, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![content, layer];
    let _: () = msg_send![layer, setCornerRadius: 13.0f64];
    layer_set_background(layer, hex_to_cg_color(0x202024E8));
    panel
}

unsafe fn set_alpha_immediately(panel: *mut AnyObject, alpha: f64) {
    let _: () = msg_send![class!(NSAnimationContext), beginGrouping];
    let context: *mut AnyObject = msg_send![class!(NSAnimationContext), currentContext];
    let _: () = msg_send![context, setDuration: 0.0f64];
    let animator: *mut AnyObject = msg_send![panel, animator];
    let _: () = msg_send![animator, setAlphaValue: alpha];
    let _: () = msg_send![class!(NSAnimationContext), endGrouping];
}

fn cached_badge_width(cache: &mut HashMap<String, f64>, label: &str, repeats: u32) -> f64 {
    crate::debug_assert_main_thread();
    let display_text = badge_display_text(label, repeats);
    if let Some(width) = cache.get(&display_text) {
        return *width;
    }

    let width = unsafe { measure_text_width(&display_text) } + BADGE_HORIZONTAL_PADDING;
    if cache.len() >= MAX_MEASUREMENTS {
        cache.clear();
    }
    cache.insert(display_text, width);
    width
}

unsafe fn measure_text_width(text: &str) -> f64 {
    let value = make_nsstring(text);
    let font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: 16.0f64, weight: 0.23f64];
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

unsafe fn rebuild_badges(
    panel: *mut AnyObject,
    badges: &[Badge],
    labels: &[String],
    widths: &[f64],
    panel_width: f64,
) {
    let content: *mut AnyObject = msg_send![panel, contentView];
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
        let _: () = msg_send![layer, setCornerRadius: 9.0f64];
        let _: () = msg_send![layer, setBorderWidth: 1.0f64];
        let (background, border, text_color) =
            if badge.kind == BadgeKind::Modifier || badge.kind == BadgeKind::Indicator {
                (0x0A84FF38, 0x0A84FFB0, 0xF8F9FAFF)
            } else {
                (palette.card_bg, palette.card_border, palette.primary_text)
            };
        layer_set_background(layer, hex_to_cg_color(background));
        layer_set_border(layer, hex_to_cg_color(border));

        let font: *mut AnyObject =
            msg_send![class!(NSFont), systemFontOfSize: 16.0f64, weight: 0.23f64];
        let ascender: f64 = msg_send![font, ascender];
        let descender: f64 = msg_send![font, descender];
        let leading: f64 = msg_send![font, leading];
        let line_height = (ascender - descender + leading).ceil();
        // A badge-height NSTextField puts its glyph ink about 8 pt off-center; center a
        // font-height frame in the capsule instead.
        let field_y = ((BADGE_H - line_height) / 2.0).max(0.0);
        let field: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let field: *mut AnyObject = msg_send![field, initWithFrame: NSRect::new(NSPoint::new(6.0, field_y), NSSize::new((*width - 12.0).max(1.0), line_height))];
        let _: () = msg_send![field, setEditable: false];
        let _: () = msg_send![field, setSelectable: false];
        let _: () = msg_send![field, setBezeled: false];
        let _: () = msg_send![field, setDrawsBackground: false];
        let _: () = msg_send![field, setAlignment: 1isize];
        let _: () = msg_send![field, setLineBreakMode: 4isize];
        let _: () = msg_send![field, setFont: font];
        let text_color = crate::ffi::hex_to_ns_color(text_color);
        let _: () = msg_send![field, setTextColor: text_color];
        let display_text = badge_display_text(label, badge.repeats);
        let title = make_nsstring(&display_text);
        let _: () = msg_send![field, setStringValue: title];
        CFRelease(title as *const c_void);
        let _: () = msg_send![content, addSubview: badge_view];
        let _: () = msg_send![badge_view, addSubview: field];
        release_obj(field);
        release_obj(badge_view);
        x += *width + BADGE_GAP;
    }
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

unsafe fn target_screen(follow_frontmost: bool) -> (NSRect, NSRect) {
    let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
    let count: usize = msg_send![screens, count];
    let main: *mut AnyObject = if count > 0 {
        msg_send![screens, objectAtIndex: 0isize]
    } else {
        msg_send![class!(NSScreen), mainScreen]
    };
    let main_frame: NSRect = msg_send![main, frame];
    if follow_frontmost {
        if let Some(bounds) = frontmost_window_bounds() {
            let (x, y, width, height) = bounds;
            if width > 0.0 && height > 0.0 {
                let appkit_center = NSPoint::new(
                    x + width / 2.0,
                    main_frame.origin.y + main_frame.size.height - (y + height / 2.0),
                );
                for index in 0..count {
                    let screen: *mut AnyObject = msg_send![screens, objectAtIndex: index as isize];
                    let frame: NSRect = msg_send![screen, frame];
                    if contains(frame, appkit_center) {
                        let visible: NSRect = msg_send![screen, visibleFrame];
                        return (frame, visible);
                    }
                }
            }
        }
    }
    let visible: NSRect = msg_send![main, visibleFrame];
    (main_frame, visible)
}

fn frontmost_window_bounds() -> Option<(f64, f64, f64, f64)> {
    let (_, front_pid) = crate::ffi::frontmost_app_info();
    crate::with_tab_state(|state| {
        let state = state.as_ref()?;
        state
            .windows
            .iter()
            .find(|window| window.pid == front_pid && window.is_active)
            .or_else(|| state.windows.iter().find(|window| window.pid == front_pid))
            .or_else(|| state.windows.iter().find(|window| window.is_active))
            .map(|window| window.bounds)
    })
}

fn contains(frame: NSRect, point: NSPoint) -> bool {
    point.x >= frame.origin.x
        && point.x <= frame.origin.x + frame.size.width
        && point.y >= frame.origin.y
        && point.y <= frame.origin.y + frame.size.height
}
