//! Clipboard subsystem · picker: the history overlay (window, rows, keyboard navigation).

use super::*;
use crate::overlay::reset_close_button_hover;

/// Desktop columns share one panel; narrow screens replace the list with detail.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct PickerLayout {
    pub panel_w: f64,
    pub list_w: f64,
    pub detail_w: f64,
    pub split: bool,
}

pub(super) fn picker_layout(available_width: f64, expanded: bool) -> PickerLayout {
    let available = (available_width - PICKER_EDGE_MARGIN * 2.0).max(1.0);
    let split = expanded && available_width > DETAIL_IN_PANE_BELOW_W;
    let panel_w = if split {
        DETAIL_SIDE_PANEL_MAX_W
    } else {
        PICKER_W
    }
    .min(available);
    let list_w = if split { 340.0 } else { panel_w };
    PickerLayout {
        panel_w,
        list_w,
        detail_w: if split {
            panel_w - list_w - 1.0
        } else {
            panel_w
        },
        split,
    }
}

static DETAIL_SPLIT: AtomicBool = AtomicBool::new(false);
static DETAIL_DIVIDER: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);

pub(super) fn picker_panel_width() -> f64 {
    let window = *PICKER_WINDOW.lock().unwrap();
    window.map_or(PICKER_W, |window| unsafe {
        crate::glass::panel_frame_of(window.0).size.width
    })
}

pub(super) fn picker_list_width() -> f64 {
    if DETAIL_SPLIT.load(Ordering::SeqCst) {
        340.0
    } else {
        picker_panel_width()
    }
}

/// Reflow chrome from the live panel geometry; row layout uses its own list width.
unsafe fn layout_picker_chrome(size: NSSize) {
    let (search, pause, options, detail, pin, close) = picker_header_frames_for(size.width);
    for (view, frame) in [
        (*SEARCH_FIELD_SURFACE.lock().unwrap(), search),
        (*SEARCH_FIELD.lock().unwrap(), search),
        (*PICKER_PAUSE_BUTTON.lock().unwrap(), pause),
        (*PICKER_OPTIONS_ORB.lock().unwrap(), options),
        (*PICKER_DETAIL_BUTTON.lock().unwrap(), detail),
        (*PICKER_PIN_BUTTON.lock().unwrap(), pin),
        (*PICKER_CLOSE_BUTTON.lock().unwrap(), close),
    ] {
        if let Some(view) = view {
            let _: () = msg_send![view.0, setFrame: frame];
        }
    }
    if let Some(clear) = *SEARCH_CLEAR_BUTTON.lock().unwrap() {
        let _: () = msg_send![clear.0, setFrameOrigin: NSPoint::new(search.origin.x + search.size.width - SEARCH_PAD_IN - SEARCH_CLEAR_W, search.origin.y + (SEARCH_H - ACTION_H) / 2.0)];
    }
    if let Some(strip) = *PICKER_HEADER_STRIP.lock().unwrap() {
        let _: () = msg_send![strip.0, setFrame: NSRect::new(NSPoint::new(0.0, size.height - header_strip_h()), NSSize::new(size.width, header_strip_h()))];
    }
    let list_scroll = *SCROLL_VIEW.lock().unwrap();
    if let Some(scroll) = list_scroll {
        let _: () = msg_send![scroll.0, setAutoresizingMask: 0u64];
        let _: () = msg_send![scroll.0, setFrame: NSRect::new(NSPoint::new(0.0, FOOTER_H), NSSize::new(picker_list_width(), (size.height - header_strip_h() - FOOTER_H).max(0.0)))];
    }
    let content_parent = *PICKER_CONTENT_PARENT.lock().unwrap();
    if let Some(parent) = content_parent {
        // Rebuild only when the footer width changes; selection changes must not recreate chrome.
        static FOOTER_WIDTH: AtomicU64 = AtomicU64::new(PICKER_W.to_bits());
        if FOOTER_WIDTH.swap(size.width.to_bits(), Ordering::SeqCst) != size.width.to_bits() {
            relayout_footer(parent.0, size.width);
        }
    }
    update_scroll_indicator();
}

unsafe fn install_detail_divider(parent: *mut AnyObject, body: NSRect, list_w: f64, visible: bool) {
    let existing = *DETAIL_DIVIDER.lock().unwrap();
    let divider = if let Some(view) = existing {
        view.0
    } else {
        let view: *mut AnyObject = msg_send![class!(NSView), new];
        let _: () = msg_send![view, setWantsLayer: true];
        let _: () = msg_send![parent, addSubview: view];
        release_obj(view);
        *DETAIL_DIVIDER.lock().unwrap() = Some(ObjPtr::new(view));
        view
    };
    let _: () = msg_send![divider, setFrame: NSRect::new(NSPoint::new(list_w, body.origin.y), NSSize::new(1.0, body.size.height))];
    let layer: *mut AnyObject = msg_send![divider, layer];
    crate::ffi::layer_set_background(layer, crate::ffi::hex_to_cg_color(picker_hairline()));
    let _: () = msg_send![divider, setHidden: !visible];
}

extern "C" fn detail_back_action(_self: *mut AnyObject, _cmd: Sel, _sender: *mut AnyObject) {
    crate::callback_guard::void("clipboard_detail_back", hide_detail);
}

unsafe fn add_detail_back_button(content: *mut AnyObject) {
    static TARGET: OnceLock<CallbackTarget> = OnceLock::new();
    let target = TARGET
        .get_or_init(|| {
            let cls =
                crate::ffi::build_class(c"OhMyTabClipboardBack", class!(NSObject), |builder| {
                    builder.add_method::<AnyObject, _>(
                        sel!(goBack:),
                        detail_back_action as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                    );
                });
            let obj: *mut AnyObject = msg_send![cls.0 as *mut AnyObject, new];
            CallbackTarget::new(obj)
        })
        .0;
    let button: *mut AnyObject = msg_send![hover_button_class(), alloc];
    let button: *mut AnyObject = msg_send![button, initWithFrame: NSRect::new(NSPoint::new(18.0, 6.0), NSSize::new(76.0, 28.0))];
    let text = make_nsstring(&t("clipboard.detail_back"));
    let _: () = msg_send![button, setTitle: text];
    CFRelease(text as *const c_void);
    let _: () = msg_send![button, setBordered: false];
    let _: () = msg_send![button, setTarget: target];
    let _: () = msg_send![button, setAction: sel!(goBack:)];
    let _: () = msg_send![content, addSubview: button];
    release_obj(button);
    add_hover_tracking(button);
}

/// The picker's offset from the cursor (16pt to the bottom-right; flips to the left/top
/// when there isn't room).
pub(super) const PICKER_CURSOR_OFF: f64 = 16.0;
/// minimum margin from the screen edge.
pub(super) const PICKER_EDGE_MARGIN: f64 = 8.0;

/// Pure: the screen frame containing the cursor (None when no screen contains it).
pub(super) fn screen_containing(cursor: NSPoint, frames: &[NSRect]) -> Option<NSRect> {
    frames.iter().copied().find(|f| {
        f.origin.x <= cursor.x
            && cursor.x < f.origin.x + f.size.width
            && f.origin.y <= cursor.y
            && cursor.y < f.origin.y + f.size.height
    })
}

/// Pure: compute the picker frame -- offset to the cursor's bottom-right; flip to the
/// left/top when the right/bottom side lacks room; clamp to the screen edge otherwise.
/// Never placed outside the screen.
pub(super) fn picker_frame_for(cursor: NSPoint, screen: NSRect, w: f64, h: f64) -> NSRect {
    let min_x = screen.origin.x;
    let max_x = screen.origin.x + screen.size.width;
    let min_y = screen.origin.y;
    let max_y = screen.origin.y + screen.size.height;

    // x: prefer the cursor's right; flip to the left when tight; clamp to the edges.
    let mut x = cursor.x + PICKER_CURSOR_OFF;
    if x + w > max_x {
        x = cursor.x - w - PICKER_CURSOR_OFF;
    }
    if x < min_x {
        x = min_x + PICKER_EDGE_MARGIN;
    }
    if x + w > max_x {
        x = max_x - w - PICKER_EDGE_MARGIN;
    }

    // y: prefer below the cursor (the panel's top edge sits 16pt under it); flip above when
    // tight; clamp to the edges.
    let mut y = cursor.y - h - PICKER_CURSOR_OFF;
    if y < min_y {
        y = cursor.y + PICKER_CURSOR_OFF;
    }
    if y + h > max_y {
        y = max_y - h - PICKER_EDGE_MARGIN;
    }
    if y < min_y {
        y = min_y + PICKER_EDGE_MARGIN;
    }

    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

/// Pure: keep the detail panel on the picker's right and align it to the selected row within
/// the picker's vertical bounds, so neither detail edge can exceed the picker.
#[cfg(test)]
pub(super) fn detail_frame_for(
    picker: NSRect,
    align_top_y: f64,
    screen: NSRect,
    w: f64,
    h: f64,
) -> NSRect {
    let min_x = screen.origin.x + PICKER_EDGE_MARGIN;
    let max_x = screen.origin.x + screen.size.width - PICKER_EDGE_MARGIN;

    // x: keep detail on the picker's right; clamp to the right-side region when the group is wide.
    let preferred_x = picker.origin.x + picker.size.width + DETAIL_GAP;
    let x = if w + 2.0 * PICKER_EDGE_MARGIN >= screen.size.width {
        min_x
    } else {
        preferred_x.min(max_x - w).max(min_x)
    };

    // y: prefer aligning to the selected row's top; long details shift upward, but always
    // clamp within the picker's top and bottom edges.
    let picker_min_y = picker.origin.y;
    let picker_max_y = picker.origin.y + picker.size.height;
    // Only finiteness is an invariant. A pane minimum can exceed a SHORT picker (this app's meta
    // table is 317pt tall, the shortest picker is 280pt), so a height above the picker is a layout
    // fact the caller caps, never a reason to abort the callback that asked for the pane.
    debug_assert!(
        h.is_finite() && w.is_finite(),
        "detail frame size is not finite"
    );
    let y = (align_top_y - h).max(picker_min_y).min(picker_max_y - h);

    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

/// Pure: lay out the picker and detail as one group, with the picker always left of detail.
///
/// `anchor_x` is where the picker already is when the caller has a position of its own — the anchor a
/// drag ended on this showing — and `None` means "derive it": centered on the main screen, or centered
/// on the cursor. Ignoring the anchor here was a real defect: dragging the panel and then opening the
/// detail snapped it back to the derived x, and so did every selection change while the detail was open.
#[cfg(test)]
pub(super) fn detail_group_frames(
    picker: NSRect,
    align_top_y: f64,
    screen: NSRect,
    detail_size: NSSize,
    anchor_x: Option<f64>,
    center_on_main: bool,
    cursor_x: f64,
) -> (NSRect, NSRect) {
    let group_w = picker.size.width + DETAIL_GAP + detail_size.width;
    let min_x = screen.origin.x + PICKER_EDGE_MARGIN;
    let max_x = screen.origin.x + screen.size.width - PICKER_EDGE_MARGIN;
    let mut picker_x = match anchor_x {
        Some(x) => x,
        None if center_on_main => screen.origin.x + (screen.size.width - group_w) / 2.0,
        None => cursor_x - group_w / 2.0,
    };

    // Clamp the whole group when it fits; if it does not, keep the picker whole and leave
    // detail in the right-side region.
    if group_w + 2.0 * PICKER_EDGE_MARGIN <= screen.size.width {
        picker_x = picker_x.max(min_x).min(max_x - group_w);
    } else {
        picker_x = min_x;
    }

    let picker_frame = NSRect::new(NSPoint::new(picker_x, picker.origin.y), picker.size);
    let detail_frame = detail_frame_for(
        picker_frame,
        align_top_y,
        screen,
        detail_size.width,
        detail_size.height,
    );
    (picker_frame, detail_frame)
}

/// The frame of the picker's screen (follows it across screens; falls back to the main
/// screen when unavailable).
pub(super) unsafe fn picker_screen_frame(picker_win: *mut AnyObject) -> NSRect {
    let sc: *mut AnyObject = msg_send![picker_win, screen];
    if sc.is_null() {
        let main: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
        msg_send![main, visibleFrame]
    } else {
        msg_send![sc, visibleFrame]
    }
}

/// Where `clipboard.picker_position` puts the picker: `(centered on the main screen, the cursor,
/// the target screen's visible frame)`. One derivation shared by the summon and by the blank-area
/// double-click's reset, so the two can never disagree about "the configured position".
pub(super) fn configured_picker_area() -> (bool, NSPoint, NSRect) {
    unsafe {
        let cursor: NSPoint = msg_send![class!(NSEvent), mouseLocation];
        let center_on_main = CONFIG.read().unwrap().clipboard.picker_position == "main";
        // The PRIMARY display, not `NSScreen.mainScreen`: that follows the key window, and the picker is a
        // panel that becomes key -- so after a drag to a secondary display, `mainScreen` would be that
        // display and "centred on the main screen" would centre the reset where the panel already is (the
        // trap `overlay::hover::primary_screen_frame` documents). The fallback keeps the old behaviour if
        // AppKit reports no screens at all.
        let main_frame: NSRect = match crate::overlay::hover::primary_screen_visible_frame() {
            Some(frame) => frame,
            None => {
                let main_screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
                msg_send![main_screen, visibleFrame]
            }
        };
        let screen_frame = if center_on_main {
            main_frame
        } else {
            let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
            let count: usize = msg_send![screens, count];
            let mut frames: Vec<NSRect> = Vec::with_capacity(count);
            for i in 0..count {
                // objectAtIndex: expects 'q' (signed long); pass isize.
                let s: *mut AnyObject = msg_send![screens, objectAtIndex: i as isize];
                frames.push(msg_send![s, visibleFrame]);
            }
            screen_containing(cursor, &frames).unwrap_or(main_frame)
        };
        (center_on_main, cursor, screen_frame)
    }
}

/// The frame the configured mode wants for a picker of `size`: centered on its screen, or offset
/// from the cursor with the edge flip/clamp.
pub(super) fn configured_picker_frame(
    center_on_main: bool,
    cursor: NSPoint,
    screen_frame: NSRect,
    size: NSSize,
) -> NSRect {
    if center_on_main {
        // Always centered on the main screen; no flip/clamp.
        NSRect::new(
            NSPoint::new(
                screen_frame.origin.x + (screen_frame.size.width - size.width) / 2.0,
                screen_frame.origin.y + (screen_frame.size.height - size.height) / 2.0,
            ),
            size,
        )
    } else {
        picker_frame_for(cursor, screen_frame, size.width, size.height)
    }
}

/// Put the picker back where the configuration asks for it, at its current size: the blank-area
/// double-click, i.e. the undo for "the panel is somewhere awkward now".
///
/// With the detail panel open the pair is laid out again from the configured origin instead of
/// moving the picker alone, so the group stays intact and the origin restored when the detail
/// closes is the configured one.
pub(super) fn reset_picker_to_configured_position() {
    // The reset means "this showing is back at the configured position", so the anchor a drag left has to
    // go with it: keeping it would make the next grab start from a position the panel is no longer at
    // (and the detail group lay out from that stale x). The double-click path relied on clearing it
    // itself before this call, which left any other caller -- the smoke runner drives this directly --
    // with a stale anchor. One owner: this function.
    super::drag::forget_session_position();
    unsafe {
        let Some(picker_win) = PICKER_WINDOW.lock().unwrap().map(|w| w.0) else {
            return;
        };
        let size = crate::glass::panel_frame_of(picker_win).size;
        let (center_on_main, cursor, screen_frame) = configured_picker_area();
        let target = configured_picker_frame(center_on_main, cursor, screen_frame, size);
        crate::glass::set_panel_frame(picker_win, target, true);
        *DETAIL_PICKER_ORIGINAL_ORIGIN.lock().unwrap() = None;
        // The reset moved the panel too, so the state document has to say where it is (same reason as
        // the end of a drag; see `drag::finish_drag`).
        crate::e2e_state::record("clipboard_picker_reset");
    }
}

/// Single-panel columns move together without a separate alignment pass.
pub(super) fn reposition_detail() {
    // Both columns now share the picker window and move together.
}

/// Cancel a pending detail-close callback so a quick reopen cannot hide the new panel.
unsafe fn cancel_detail_close(window: *mut AnyObject) {
    let _: () = msg_send![
        class!(NSObject),
        cancelPreviousPerformRequestsWithTarget: window,
        selector: sel!(finishDetailClose:),
        object: std::ptr::null::<AnyObject>()
    ];
}

/// Animate the existing detail content horizontally without rebuilding views or images.
unsafe fn animate_detail_content(content: *mut AnyObject, opening: bool) {
    let _: () = msg_send![content, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![content, layer];
    if layer.is_null() {
        return;
    }

    let transform_key = make_nsstring("transform.translation.x");
    let opacity_key = make_nsstring("opacity");
    let transform_animation_key = make_nsstring("clipboard-detail-content-slide");
    let opacity_animation_key = make_nsstring("clipboard-detail-content-fade");
    let _: () = msg_send![layer, removeAnimationForKey: transform_animation_key];
    let _: () = msg_send![layer, removeAnimationForKey: opacity_animation_key];

    let (from_x, to_x, from_opacity, to_opacity) = if opening {
        (DETAIL_CONTENT_ANIMATION_OFFSET, 0.0, 0.0f32, 1.0f32)
    } else {
        (0.0, DETAIL_CONTENT_ANIMATION_OFFSET, 1.0f32, 0.0f32)
    };
    let from_x_value: *mut AnyObject = msg_send![class!(NSNumber), numberWithDouble: from_x];
    let to_x_value: *mut AnyObject = msg_send![class!(NSNumber), numberWithDouble: to_x];
    let _: () = msg_send![layer, setValue: from_x_value, forKeyPath: transform_key];
    let _: () = msg_send![layer, setOpacity: from_opacity];

    let transform_animation: *mut AnyObject = msg_send![
        class!(CABasicAnimation),
        animationWithKeyPath: transform_key
    ];
    let _: () = msg_send![transform_animation, setFromValue: from_x_value];
    let _: () = msg_send![transform_animation, setToValue: to_x_value];
    let _: () = msg_send![
        transform_animation,
        setDuration: if opening {
            crate::theme::ANIMATION_DURATION_MEDIUM
        } else {
            crate::theme::animation_exit_duration(crate::theme::ANIMATION_DURATION_MEDIUM)
        }
    ];
    let timing = crate::theme::ease_standard_timing_function();
    if !timing.is_null() {
        let _: () = msg_send![transform_animation, setTimingFunction: timing];
    }

    let from_opacity_value: *mut AnyObject =
        msg_send![class!(NSNumber), numberWithFloat: from_opacity];
    let to_opacity_value: *mut AnyObject = msg_send![class!(NSNumber), numberWithFloat: to_opacity];
    if crate::theme::reduce_motion_enabled() {
        let _: () = msg_send![class!(CATransaction), begin];
        let _: () = msg_send![class!(CATransaction), setDisableActions: true];
        let _: () = msg_send![layer, setValue: to_x_value, forKeyPath: transform_key];
        let _: () = msg_send![layer, setOpacity: to_opacity];
        let _: () = msg_send![class!(CATransaction), commit];
        CFRelease(transform_key as *const c_void);
        CFRelease(opacity_key as *const c_void);
        CFRelease(transform_animation_key as *const c_void);
        CFRelease(opacity_animation_key as *const c_void);
        return;
    }
    let opacity_animation: *mut AnyObject = msg_send![
        class!(CABasicAnimation),
        animationWithKeyPath: opacity_key
    ];
    let _: () = msg_send![opacity_animation, setFromValue: from_opacity_value];
    let _: () = msg_send![opacity_animation, setToValue: to_opacity_value];
    let _: () = msg_send![
        opacity_animation,
        setDuration: if opening {
            crate::theme::ANIMATION_DURATION_MEDIUM
        } else {
            crate::theme::animation_exit_duration(crate::theme::ANIMATION_DURATION_MEDIUM)
        }
    ];
    let timing = crate::theme::ease_standard_timing_function();
    if !timing.is_null() {
        let _: () = msg_send![opacity_animation, setTimingFunction: timing];
    }

    // Commit final model values before adding explicit animations so the layer does not snap
    // back to its starting point when Core Animation removes them.
    let _: () = msg_send![layer, setValue: to_x_value, forKeyPath: transform_key];
    let _: () = msg_send![layer, setOpacity: to_opacity];
    let _: () =
        msg_send![layer, addAnimation: transform_animation, forKey: transform_animation_key];
    let _: () = msg_send![layer, addAnimation: opacity_animation, forKey: opacity_animation_key];

    CFRelease(transform_key as *const c_void);
    CFRelease(opacity_key as *const c_void);
    CFRelease(transform_animation_key as *const c_void);
    CFRelease(opacity_animation_key as *const c_void);
}

/// Keep the list tree alive so keyboard focus and scroll position survive a narrow swap.
pub(super) fn set_picker_pane_mode(mode: PickerPaneMode) {
    *PICKER_PANE_MODE.lock().unwrap() = mode;
    unsafe {
        let list_scroll = *SCROLL_VIEW.lock().unwrap();
        if let Some(scroll) = list_scroll {
            let _: () = msg_send![scroll.0, setHidden: mode == PickerPaneMode::Detail];
        }
    }
}

/// Whether the picker body is currently showing the in-pane detail.
pub(super) fn picker_shows_detail_in_pane() -> bool {
    *PICKER_PANE_MODE.lock().unwrap() == PickerPaneMode::Detail
}

/// A press anywhere in the detail pane ends the picker's search editing session. The pane is a
/// separate, never-key window, so the picker's own `sendEvent:` does not see its clicks; without
/// this the search field would stay in its editing state while the user works in the detail.
extern "C" fn detail_window_send_event(this: *mut AnyObject, _cmd: Sel, event: *mut AnyObject) {
    unsafe {
        let event_type: u64 = msg_send![event, type];
        if super::is_mouse_press_type(event_type) {
            super::end_search_editing();
        }
        type SendEvent = unsafe extern "C" fn(*mut ObjcSuper, Sel, *mut AnyObject);
        let mut sup =
            ObjcSuper::for_defined_class(this as *mut c_void, detail_window_class_handle());
        let send_event: SendEvent = std::mem::transmute(objc_msgSendSuper as *const ());
        send_event(&mut sup, sel!(sendEvent:), event);
    }
}

extern "C" fn detail_finish_close(this: *mut AnyObject, _cmd: Sel, _sender: *mut AnyObject) {
    if detail_visible() {
        return;
    }
    unsafe {
        let _: () = msg_send![this, orderOut: std::ptr::null::<AnyObject>()];
        let _: () = msg_send![this, setAlphaValue: 1.0f64];
    }
}

/// Toggle the picker on the configured global history shortcut (called on the main thread by the
/// bridge).
pub(crate) extern "C" fn on_clipboard_toggle(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    // Ignore the summon when the master switch is off (the configured shortcut must not open the
    // picker after the user disabled the feature in Settings).
    if !CONFIG.read().unwrap().clipboard.enabled {
        log_debug!("[clip] toggle ignored: clipboard history disabled");
        return;
    }
    // Decision B: while pinned the shortcut does nothing at all - it neither hides nor unpins the
    // panel. (An earlier design let it re-raise; the user chose a strict no-op.)
    if is_pinned() {
        log_debug!("[clip] toggle ignored: the panel is pinned");
        return;
    }
    // `PICKER_VISIBLE` records our intent, but AppKit can hide an unpinned panel as the app loses
    // activation without delivering the resign-key callback (for example, while it is already
    // non-key). Treat the actual window state as authoritative so Option+V reopens a hidden panel
    // instead of consuming the shortcut to hide it again.
    if picker_is_visible() {
        hide_picker();
        return;
    }
    PICKER_VISIBLE.store(false, Ordering::SeqCst);
    // Show the picker even with an empty history (the empty-state hint lives in
    // rebuild_rows' empty branch).
    set_picker_selection(0);
    show_picker();
}

/// Show the picker for the `--show-clipboard` development switch. Identical to the hotkey path
/// except that a disabled clipboard module is not a refusal: the switch exists to inspect the
/// laid-out UI, which must be verifiable regardless of the current config.
/**
 * Re-open the clipboard picker until the encrypted history has loaded.
 *
 * `--show-clipboard-detail` is a development entry point: on a cold start the panel can open
 * before the history file is read, and the retry re-runs the same show (which resets the search
 * and re-renders the rows) once an entry exists, then opens the detail pane on the first one.
 */
fn schedule_dev_detail_retry() {
    std::thread::spawn(|| {
        for _ in 0..40 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let ready = {
                let history = CLIP_HISTORY.lock().unwrap();
                !history.is_empty()
            };
            if !ready {
                continue;
            }
            let Some(controller) = crate::CONTROLLER.lock().unwrap().map(|ptr| ptr.0) else {
                return;
            };
            // The window work stays on the main thread, like every other picker entry.
            unsafe {
                let _: () = msg_send![
                    controller,
                    performSelectorOnMainThread: sel!(handleDevShowClipboardDetail:),
                    withObject: std::ptr::null::<AnyObject>(),
                    waitUntilDone: false
                ];
            }
            return;
        }
    });
}

/// The retry's main-thread entry: show the picker (which re-renders the now-loaded rows) and open
/// the detail pane on the first entry.
pub(crate) fn show_picker_detail_for_development() {
    show_picker();
    set_picker_selection(0);
    unsafe { show_detail_for_sel() };
}

pub(crate) fn show_picker_for_development() {
    if !CONFIG.read().unwrap().clipboard.enabled {
        crate::log_info!("[clip] --show-clipboard: clipboard module is disabled in config");
    }
    show_picker();
    // The detail panel is reached through a key or the header's toggle, neither of which a script
    // can press without taking focus from the panel, so the development switch can also open it
    // directly: what it needs is the laid-out pane, not the gesture. The history is loaded
    // asynchronously on first launch, so a panel that opened before it arrived reopens once it is
    // there (a prewarmed picker reuses its rows and would otherwise stay empty).
    if crate::dev_flags::present("show-clipboard-detail") {
        schedule_dev_detail_retry();
    }
}

/// The menu bar's "show and pin clipboard history": idempotent, never a toggle.
///
/// - unpinned or hidden: show the panel, then pin it at the height it actually has;
/// - pinned and visible: do nothing (the menu is the way BACK to a hidden pinned panel, not a
///   second toggle that could strand the user);
/// - pinned and hidden: show it again at the frozen height.
///
/// The pin's height is read after the show, so "unpinned" freezes the real panel height rather than
/// a stale value from a previous showing.
pub(crate) fn show_and_pin_for_menu() {
    if !PICKER_VISIBLE.load(Ordering::SeqCst) || !picker_is_visible() {
        show_picker();
    }
    if is_pinned() {
        return;
    }
    let height = unsafe {
        match *PICKER_WINDOW.lock().unwrap() {
            Some(window) => crate::glass::panel_frame_of(window.0).size.height,
            None => return,
        }
    };
    if !pin(height) {
        return;
    }
    unsafe {
        // The same precondition as the corner button: a pin that cannot arm the click monitor cannot
        // honour the "next outside click closes it" contract, so it is refused rather than left in a
        // state the panel cannot keep.
        if !super::sync_outside_click_monitor() {
            super::refuse_pin_unavailable();
            return;
        }
        super::refresh_pin_button();
        super::apply_pin_window_properties();
    }
    crate::menu::refresh_clipboard_pin_item();
}

/// Show the picker (built once, reused; the window height follows the visible row count).
pub(super) fn show_picker() {
    unsafe {
        // Capture the destination before makeKeyWindow gives the nonactivating panel keyboard
        // ownership. A row action can run after AppKit has made our own process frontmost.
        super::paste_handback::capture_external_target();
        let show_started = Instant::now();
        super::set_picker_glass_inactive(false);
        // Start each summon without a hovered row; the old rows are gone, so their index must
        // never leak into the rebuilt list.
        *HOVER_ROW.lock().unwrap() = NO_SELECTION;
        // Expire before summon (entries that aged out while the user wasn't copying are
        // removed here; rebuild_rows renders the fresh list). Pinned never expire.
        {
            let mut hist = CLIP_HISTORY.lock().unwrap();
            let removed = expire_entries(&mut hist, now_secs(), ttl_secs());
            if removed > 0 {
                log_debug!("[clip] show picker: expired {} entries", removed);
            }
        }
        let ensure_started = Instant::now();
        ensure_picker_window();
        let ensure_window_ms = ensure_started.elapsed().as_millis();
        // The picker window persists: sync its backdrop with the effective material so a
        // Reduce Transparency toggle (or a config edit) is honored by this summon. This
        // also refreshes PICKER_CONTENT_PARENT after a material swap, before the footer
        // rebuild below can hang anything off it.
        crate::clipboard::apply_backdrop_material();
        // Reset the search on every summon (a clean slate); a stale detail panel goes too.
        hide_detail();
        clear_search();
        let window = match *PICKER_WINDOW.lock().unwrap() {
            Some(w) => w.0,
            None => return,
        };
        let filter = *CLIP_FILTER.lock().unwrap();
        let show_source = show_source_app();

        let render_key_started = Instant::now();
        let render_key = picker_rows_key(history_revision(), "", filter, show_source);
        let render_key_ms = render_key_started.elapsed().as_millis();
        let rows_ready = with_clipboard_ui(|ui| {
            ui.rendered_rows
                .as_ref()
                .is_some_and(|key| key == &render_key)
        });
        let hist_len = CLIP_HISTORY.lock().unwrap().len();
        log_debug!("[clip] show picker: history={} entries", hist_len);

        let frame_started = Instant::now();
        // Window height = paddings + the sum of the visible rows' pitches (each pitch follows
        // the entry's wrapped line count).
        let pitches = {
            let hist = CLIP_HISTORY.lock().unwrap();
            compute_pitches(&hist)
        };
        // Resolve the position mode first: "mouse" follows the cursor on its screen;
        // "main" centers the picker on the main screen (the height cap uses that screen
        // too).
        let (center_on_main, cursor, screen_frame) = configured_picker_area();

        // Max height: the 640pt hard cap, shrunk on small screens (120pt kept for the menu
        // bar / cursor offset / edge margins).
        // The window is the padded rect, so the panel's budget has to leave the elevation's padding room:
        // a window taller than the screen is constrained by AppKit, which moves the panel.
        let pad = crate::glass::panel_elevation_padding();
        let max_h = PICKER_MAX_HEIGHT.min(screen_frame.size.height - 120.0 - pad.top - pad.bottom);
        // The visible row count derives from the height cap, floored to whole rows (no
        // half-cut row at the window's bottom). Pitches are accumulated per actual row
        // rather than estimated at ROW_H each: image entries can occupy several text rows
        // (clipboard.image_rows), so a fixed-ROW_H estimate would let the panel exceed
        // the cap once tall rows are in play.
        let list_budget = max_h - header_strip_h() - FOOTER_H - PAD_Y;
        let list_h = if hist_len == 0 {
            40.0
        } else {
            list_fill_for_budget(&pitches, list_budget).1.max(ROW_H)
        };
        // Use the same three-record visual minimum for every state, including one or zero rows,
        // and never exceed the cap: enough tall image rows must not push the panel past max_h.
        let computed_h = (header_strip_h() + list_h + FOOTER_H + PAD_Y)
            .max(picker_min_height())
            .min(max_h);
        // A pinned panel keeps the height it was pinned at (decision E): the row set can change
        // underneath it (a new copy, a filter, an expiry) without the panel resizing under the
        // pointer. The frozen value is a PANEL height, which is what the frame here is built from.
        let h = frozen_height().unwrap_or(computed_h);

        let frame = configured_picker_frame(
            center_on_main,
            cursor,
            screen_frame,
            NSSize::new(picker_layout(screen_frame.size.width, false).panel_w, h),
        );
        log_debug!(
            "[clip] picker frame: ({:.0},{:.0}) {}x{} on screen ({:.0},{:.0}) mode={}",
            frame.origin.x,
            frame.origin.y,
            frame.size.width,
            frame.size.height,
            screen_frame.origin.x,
            screen_frame.origin.y,
            center_on_main
        );
        crate::glass::set_panel_frame(window, frame, true);
        layout_picker_chrome(frame.size);
        let frame_ms = frame_started.elapsed().as_millis();

        let render_summary = if rows_ready {
            log_debug!("[clip] picker rows reused");
            let cached = with_clipboard_ui(|ui| ui.last_rebuild_timing);
            cached.map(|cached| PickerTimingSummary {
                // Reused rows were not rebuilt during this summon; retain only the counts of
                // the currently cached view, not the previous rebuild's timing measurements.
                elapsed_ms: 0,
                history_len: cached.history_len,
                filtered_len: cached.filtered_len,
                image_rows: cached.image_rows,
                code_rows: cached.code_rows,
                empty: cached.empty,
                ..PickerTimingSummary::default()
            })
        } else {
            // The model may have just changed on the pasteboard-monitor path; finish the
            // long-lived row tree before showing it, so stale or empty rows never flash.
            // The tree is warmed at startup and after every history change; this is only a
            // race fallback.
            rebuild_rows()
        };
        // Scroll to the top on every summon (the newest entry).
        let display_prep_started = Instant::now();
        if let Some(container) = picker_container_ptr() {
            let _: () = msg_send![container, scrollPoint: NSPoint::new(0.0, 0.0)];
        }
        // Hidden refreshes may leave physical slots for the old bottom viewport; materialize
        // the top viewport after resetting the scroll position.
        if picker_materialized_range_changed() {
            rebuild_rows();
        }
        // An empty list has a fixed minimal height, while a reused row tree may have been laid out
        // at another height (the rows key carries no geometry) and the clear actions rebuild in
        // place without resizing the panel. Re-derive the hint's position for the frame this summon
        // just applied, before the indicator geometry reads the document height.
        refresh_empty_state_layout();
        // Update the scroll indicator right on the first summon (shown immediately when the
        // content overflows, not only after scrolling).
        update_scroll_indicator();
        // The row tree may not yet be in the backing store after startup or a hidden refresh;
        // draw it before ordering front so the first presentation does not pay that cost.
        let _: () = msg_send![window, displayIfNeeded];
        let display_prep_ms = display_prep_started.elapsed().as_millis();
        let order_front_started = Instant::now();
        let order_front_call_started = Instant::now();
        let _: () = msg_send![window, orderFrontRegardless];
        let order_front_call_ms = order_front_call_started.elapsed().as_millis();
        let make_key_window_started = Instant::now();
        let _: () = msg_send![window, makeKeyWindow];
        let make_key_window_ms = make_key_window_started.elapsed().as_millis();
        // Keyboard focus to the container (arrows / Enter / Esc).
        let first_responder_lock_started = Instant::now();
        let container = picker_container_ptr();
        let first_responder_lock_ms = first_responder_lock_started.elapsed().as_millis();
        let mut make_first_responder_ms = 0;
        if let Some(c) = container {
            // makeFirstResponder: returns BOOL ('B').
            let make_first_responder_started = Instant::now();
            let _: bool = msg_send![window, makeFirstResponder: c];
            make_first_responder_ms = make_first_responder_started.elapsed().as_millis();
        }
        let visible_store_started = Instant::now();
        PICKER_VISIBLE.store(true, Ordering::SeqCst);
        // A new showing invalidates any dismiss queued against the previous one, and the click
        // monitor now has a panel to guard. Both happen after the window is really up, before any
        // click can arrive.
        begin_showing();
        super::apply_pin_window_properties();
        super::sync_outside_click_monitor();
        let visible_store_ms = visible_store_started.elapsed().as_millis();
        let order_front_ms = order_front_started.elapsed().as_millis();
        let order_front_residual_ms = order_front_ms.saturating_sub(
            order_front_call_ms
                + make_key_window_ms
                + first_responder_lock_ms
                + make_first_responder_ms
                + visible_store_ms,
        );

        let elapsed_ms = show_started.elapsed().as_millis();
        if elapsed_ms >= CLIPBOARD_SLOW_PATH_MS {
            let filtered_len = with_clipboard_ui(|ui| ui.filtered.len());
            let summary = render_summary.unwrap_or(PickerTimingSummary {
                history_len: hist_len,
                filtered_len,
                empty: filtered_len == 0,
                ..PickerTimingSummary::default()
            });
            let slowest_row_index = summary
                .slowest_row_index
                .map_or_else(|| "none".to_owned(), |index| index.to_string());
            log_debug!(
                "[clip] picker_show_slow elapsed_ms={} ensure_window_ms={} render_key_ms={} frame_ms={} rebuild_rows_ms={} display_prep_ms={} order_front_ms={} order_front_call_ms={} make_key_window_ms={} first_responder_lock_ms={} make_first_responder_ms={} visible_store_ms={} order_front_residual_ms={} history_len={} filtered_len={} image_rows={} code_rows={} counts_scope={} reused={} empty={} slowest_row_ms={} slowest_row_index={}",
                elapsed_ms,
                ensure_window_ms,
                render_key_ms,
                frame_ms,
                summary.elapsed_ms,
                display_prep_ms,
                order_front_ms,
                order_front_call_ms,
                make_key_window_ms,
                first_responder_lock_ms,
                make_first_responder_ms,
                visible_store_ms,
                order_front_residual_ms,
                summary.history_len,
                summary.filtered_len,
                summary.image_rows,
                summary.code_rows,
                if rows_ready {
                    "cached_view"
                } else {
                    "rebuilt_view"
                },
                rows_ready,
                summary.empty,
                summary.slowest_row_ms,
                slowest_row_index,
            );
        }
    }
}

pub(super) fn hide_picker() {
    PICKER_VISIBLE.store(false, Ordering::SeqCst);
    *SCROLL_DRAG.lock().unwrap() = None;
    // Hiding does not reliably deliver mouseExited to every child button; clear the row hover
    // state explicitly (the persistent corner × would otherwise come back red).
    *HOVER_ROW.lock().unwrap() = NO_SELECTION;
    if let Some(button) = *PICKER_CLOSE_BUTTON.lock().unwrap() {
        unsafe { reset_close_button_hover(button.0) };
    }
    if let Some(button) = *PICKER_OPTIONS_ORB.lock().unwrap() {
        unsafe { set_settings_button_style(button.0, false) };
    }
    // The recording orb needs the same treatment AND its recorded hover state cleared: it is the one
    // header orb whose style is re-applied later (on every pause/resume), so a stale "hovered" would
    // keep painting the hover wash onto the next showing.
    unsafe {
        set_pause_orb_hovered(false);
        if let Some(button) = *PICKER_PAUSE_BUTTON.lock().unwrap() {
            set_settings_button_style(button.0, false);
        }
    }
    dismiss_filter_popover();
    // Same reason for a blank-area drag: an in-flight grab loses its mouseUp, the pushed hand
    // cursor has to be popped by hand, and this showing's dragged position dies with the showing.
    super::drag::on_picker_hidden();
    hide_detail();
    // The showing is over: a dismiss queued against it is stale from here on, the click monitor has
    // nothing to guard, and this is where the properties pinning changed are restored (never at
    // unpin, which must leave a still-visible panel exactly where it is).
    super::begin_showing();
    // The panel is going away: an armed paste hand-back can never land now, and its burn
    // suppression token must not outlive it.
    super::paste_handback::cancel();
    // The panel is genuinely hidden here, so this is where the properties pinning changed are
    // restored - not at unpin, which must leave a still-visible panel exactly where it is. The
    // monitor is removed by force: `picker_is_visible()` can still answer true this early in the
    // hide (the orderOut is deliberately outside the lock, below).
    unsafe {
        super::set_picker_glass_inactive(false);
        super::apply_pin_window_properties_for_hidden_panel();
        super::remove_outside_click_monitor_now();
    }

    // Take the pointer under the lock but orderOut outside it: orderOut synchronously fires
    // NSWindowDidResignKeyNotification, whose callback re-enters hide_picker and locks the
    // same non-reentrant Mutex -- a self-deadlock (the process used to hang).
    let win = *PICKER_WINDOW.lock().unwrap();
    unsafe {
        if let Some(w) = win {
            let _: () = msg_send![w.0, orderOut: std::ptr::null::<AnyObject>()];
        }
    }
}

/// panel never becomes key so orderOut fires no resign-key notification, but the
/// pointer-outside-the-lock discipline is kept anyway).
pub(super) fn hide_detail() {
    if !take_detail_visible() {
        return;
    }
    unsafe {
        let detail_content = *DETAIL_CONTENT.lock().unwrap();
        let picker_window = *PICKER_WINDOW.lock().unwrap();
        if let (Some(content), Some(window)) = (detail_content, picker_window) {
            let responder: *mut AnyObject = msg_send![window.0, firstResponder];
            if !responder.is_null() {
                let is_view: bool = msg_send![responder, isKindOfClass: class!(NSView)];
                if is_view {
                    let in_detail: bool = msg_send![responder, isDescendantOf: content.0];
                    if in_detail {
                        if let Some(container) = picker_container_ptr() {
                            let _: bool = msg_send![window.0, makeFirstResponder: container];
                        }
                    }
                }
            }
            let _: () = msg_send![content.0, setHidden: true];
        }
        if let Some(divider) = *DETAIL_DIVIDER.lock().unwrap() {
            let _: () = msg_send![divider.0, setHidden: true];
        }
        DETAIL_SPLIT.store(false, Ordering::SeqCst);
        set_picker_pane_mode(PickerPaneMode::List);
        let picker_window = *PICKER_WINDOW.lock().unwrap();
        if let Some(window) = picker_window {
            let mut frame = crate::glass::panel_frame_of(window.0);
            let screen = picker_screen_frame(window.0);
            frame.size.width =
                PICKER_W.min((screen.size.width - PICKER_EDGE_MARGIN * 2.0).max(1.0));
            let origin = DETAIL_PICKER_ORIGINAL_ORIGIN
                .lock()
                .unwrap()
                .take()
                .unwrap_or(frame.origin);
            frame.origin = super::drag::clamp_picker_origin(origin, frame.size, screen);
            crate::glass::set_panel_frame(window.0, frame, true);
            layout_picker_chrome(frame.size);
        }
        rebuild_rows();
        if let Some(container) = picker_container_ptr() {
            scroll_selection_into_view(container, picker_selection());
        }
    }
    refresh_detail_action_visuals();
}

/// The detail panel's window class, as a handle: it defines `finishDetailClose:`, the delayed
/// callback the close animation schedules on the window.
fn detail_window_class_handle() -> StaticClass {
    static CLASS: OnceLock<StaticClass> = OnceLock::new();
    crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabClipDetailWindow", || {
        crate::ffi::build_class(
            c"OhMyTabClipDetailWindow",
            class!(NSPanel),
            |builder| unsafe {
                builder.add_method::<AnyObject, _>(
                    sel!(canBecomeKeyWindow),
                    detail_window_can_not_become_key
                        as unsafe extern "C" fn(*mut AnyObject, Sel) -> objc2::runtime::Bool,
                );
                builder.add_method::<AnyObject, _>(
                    sel!(finishDetailClose:),
                    detail_finish_close
                        as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
                // A press inside the detail pane is a press outside the picker's search field: the
                // pane is a second window, so the picker's sendEvent: never sees these events.
                builder.add_method::<AnyObject, _>(
                    sel!(sendEvent:),
                    detail_window_send_event
                        as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
            },
        )
    })
}

/// The detail content container's class. Flipped and click-consuming, as its two overrides say.
fn detail_content_class_handle() -> StaticClass {
    static CLASS: OnceLock<StaticClass> = OnceLock::new();
    crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabClipDetailContent", || {
        crate::ffi::build_class(
            c"OhMyTabClipDetailContent",
            class!(NSView),
            |builder| unsafe {
                builder.add_method::<AnyObject, _>(
                    sel!(mouseDown:),
                    detail_content_mouse_down
                        as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
                builder.add_method::<AnyObject, _>(
                    sel!(isFlipped),
                    detail_content_is_flipped
                        as unsafe extern "C" fn(*mut AnyObject, Sel) -> objc2::runtime::Bool,
                );
            },
        )
    })
}

/// Build the detail panel window (once): a Nonactivating NSPanel with the same glass
/// backdrop as the picker. KEY: canBecomeKeyWindow is overridden to NO, so the panel never
/// becomes key -- keyboard focus stays in the picker's container (all keys keep going
/// through container_key_down); the detail is a passive display only.
unsafe fn ensure_detail_window() {
    if DETAIL_WINDOW.lock().unwrap().is_some() {
        return;
    }
    let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
    let screen_frame: NSRect = msg_send![screen, visibleFrame];
    let w = DETAIL_MAX_W;
    let h = (screen_frame.size.height - DETAIL_SCREEN_MARGIN * 2.0)
        .max(detail_pane_min_height(w, meta_table_field_count()));
    let x = (screen_frame.size.width - w) / 2.0 + screen_frame.origin.x;
    let y = (screen_frame.size.height - h) / 2.0 + screen_frame.origin.y;
    let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));

    // Same as the picker: NSWindowStyleMaskNonactivatingPanel (1<<7), no app activation.
    let style: u64 = 1 << 7;

    let window_cls = detail_window_class_handle().0 as *mut AnyObject;
    let window: *mut AnyObject = msg_send![window_cls, alloc];
    let window: *mut AnyObject = msg_send![window, initWithContentRect: frame, styleMask: style, backing: 2u64, defer: false];
    apply_panel_appearance(window);
    let _: () = msg_send![window, setLevel: 3u64];
    let _: () = msg_send![window, setOpaque: false];
    let _: () = msg_send![window, setReleasedWhenClosed: false];
    let clear: *mut AnyObject = msg_send![class!(NSColor), clearColor];
    let _: () = msg_send![window, setBackgroundColor: clear];
    let _: () = msg_send![window, setHasShadow: false];

    // This passive detail panel uses the shared backdrop and its inactive-window compensation.
    let backdrop = crate::glass::install_backdrop(
        window,
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h)),
        super::PICKER_PANEL_RADIUS,
        crate::clipboard::detail_backdrop_options(),
    );
    *DETAIL_BACKDROP.lock().unwrap() = Some(backdrop);
    let content_parent = backdrop.content_parent;

    // The content container is flipped and top-aligned. Detail text is selectable, and an
    // ordinary image click must not dismiss the panel either; Esc/←/→ or list actions close it.
    let content = {
        let content_cls = detail_content_class_handle().0 as *mut AnyObject;
        let content: *mut AnyObject = msg_send![content_cls, alloc];
        let content: *mut AnyObject = msg_send![
            content,
            initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h))
        ];
        let _: () = msg_send![content, setWantsLayer: true];
        let _: () = msg_send![content, setAutoresizingMask: 18u64];
        let _: () = msg_send![content_parent, addSubview: content];
        release_obj(content);
        // Every string in this panel is user data drawn over the panel material, and the text view
        // itself is transparent, so the read surface is the container.
        content
    };

    *DETAIL_CONTENT.lock().unwrap() = Some(ObjPtr::new(content));
    *DETAIL_WINDOW.lock().unwrap() = Some(ObjPtr::new(window));
    // A detail created while the panel is pinned must carry the pinned window state too: the property
    // pass only reaches windows that already exist when it runs. It must copy what the PICKER is
    // actually carrying rather than recompute from the pin, because after an unpin the picker is
    // deliberately still holding the pinned properties until the panel actually hides - recomputing
    // would restore them early and break the unpin transition.
    super::notifications::inherit_window_properties_from_picker(window);
}

/// The detail panel never becomes key (keyboard focus stays in the picker's container).
extern "C" fn detail_window_can_not_become_key(
    _self: *mut AnyObject,
    _cmd: Sel,
) -> objc2::runtime::Bool {
    // The `B` encoding AppKit declares for this selector, not a Rust `bool`: objc2's signature
    // check compares the registered method with NSWindow's own before registering it.
    objc2::runtime::Bool::NO
}

/// The content container is flipped.
extern "C" fn detail_content_is_flipped(_self: *mut AnyObject, _cmd: Sel) -> objc2::runtime::Bool {
    objc2::runtime::Bool::YES
}

/// Consume ordinary clicks on the detail container. NSTextView already consumes clicks for
/// selection, whereas image clicks reach the container; both content kinds must behave alike
/// and never accidentally dismiss the detail panel.
extern "C" fn detail_content_mouse_down(_self: *mut AnyObject, _cmd: Sel, _event: *mut AnyObject) {}

/// Toggle code-detail soft wrapping and rebuild in place; the new scroll view naturally resets
/// its horizontal position to the leading edge.
pub(super) extern "C" fn toggle_detail_soft_wrap(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut AnyObject,
) {
    DETAIL_SOFT_WRAP_ENABLED.fetch_xor(true, Ordering::SeqCst);
    unsafe { show_detail_for_sel() };
}

/// Placeholder share action; the button currently provides visual and hover feedback only.
/// Map a UTI to a common image file extension (png fallback).
fn ext_for_image_uti(uti: &str) -> &'static str {
    let u = uti.to_ascii_lowercase();
    if u.contains("jpeg") {
        "jpg"
    } else if u.contains("gif") {
        "gif"
    } else if u.contains("heic") || u.contains("heif") {
        "heic"
    } else if u.contains("tiff") {
        "tif"
    } else if u.contains("webp") {
        "webp"
    } else if u.contains("bmp") {
        "bmp"
    } else {
        "png"
    }
}

/// Save-as: text entries are written as .txt; image entries take bytes from the data-cache copy,
/// then the copied source file, then the preview PNG as a fallback (the extension follows the source).
unsafe fn run_detail_save_as(entry: &ClipEntry) {
    match &entry.image {
        Some(img) => {
            // Content priority: data-cache bytes > the source file of a file copy > the preview PNG.
            let (bytes, ext): (Vec<u8>, &'static str) =
                if img.data_path.as_os_str().is_empty() && img.source_path.is_none() {
                    match cache_read_preview(img.hash) {
                        Some(preview) => (preview, "png"),
                        None => {
                            log_info!("[clip] save-as image failed: no cached bytes");
                            return;
                        }
                    }
                } else {
                    let raw = img
                        .source_path
                        .as_deref()
                        .and_then(|p| std::fs::read(p).ok())
                        .or_else(|| image_bytes_for_hash(img.hash).map(|bytes| (*bytes).clone()));
                    match raw {
                        Some(data) => (data, ext_for_image_uti(&img.uti)),
                        None => match cache_read_preview(img.hash) {
                            Some(preview) => (preview, "png"),
                            None => {
                                log_info!("[clip] save-as image failed: source unreadable");
                                return;
                            }
                        },
                    }
                };
            // The suggested filename carries the entry's COPY timestamp (not the save
            // moment): "Clipboard Image 2026-08-23 08.31.42.png".
            let stamp = save_stamp_for(entry);
            let suggested = format!(
                "{} {stamp}.{ext}",
                t("clipboard.detail_save_image_name"),
                ext = ext
            );
            let Some(dest) = run_save_panel(&suggested) else {
                return;
            };
            match std::fs::write(&dest, &bytes) {
                Ok(()) => log_debug!("[clip] image saved to {dest}"),
                Err(e) => log_info!("[clip] image save to {dest} failed: {e}"),
            }
        }
        None => {
            let stamp = save_stamp_for(entry);
            let suggested = format!("{} {stamp}.txt", t("clipboard.detail_save_text_name"));
            let Some(dest) = run_save_panel(&suggested) else {
                return;
            };
            match std::fs::write(&dest, entry.text.as_bytes()) {
                Ok(()) => log_debug!("[clip] text saved to {dest}"),
                Err(e) => log_info!("[clip] text save to {dest} failed: {e}"),
            }
        }
    }
}

/// Save-as action: the detail view follows the selected list entry; text saves as txt and images save by source.
pub(super) extern "C" fn detail_save_as_action(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut AnyObject,
) {
    let sel = picker_selection();
    if sel == NO_SELECTION {
        return;
    }
    let Some(h_idx) = mapped_index(sel) else {
        return;
    };
    let Some(entry) = CLIP_HISTORY.lock().unwrap().get(h_idx).cloned() else {
        return;
    };
    // This action is invoked synchronously inside the button's mouseDown tracking
    // session; runModal must NOT start here (a nested modal leaves the save panel's
    // name field without keyboard focus). Stash the entry and hop to the next runloop
    // turn; detail_save_as_deferred runs once tracking has unwound.
    *PENDING_SAVE_AS.lock().unwrap() = Some(entry);
    let target = unsafe { observer() };
    unsafe {
        let _: () = msg_send![
            target,
            performSelectorOnMainThread: sel!(detailSaveAsDeferred:),
            withObject: std::ptr::null_mut::<AnyObject>(),
            waitUntilDone: false
        ];
    }
}

/// Main-thread re-entry for save-as (arrives after hopping out of the button tracking
/// loop via performSelectorOnMainThread): activate the app first (reliable panel
/// keyboard focus in accessory mode), then run the save flow.
pub(super) extern "C" fn detail_save_as_deferred(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut AnyObject,
) {
    let Some(entry) = PENDING_SAVE_AS.lock().unwrap().take() else {
        return;
    };
    unsafe {
        // An accessory app may be inactive; activate ourselves so the NSSavePanel can
        // reliably establish its key window and field editor (the precondition for an
        // editable name field). activateIgnoringOtherApps is deprecated yet still
        // functional with no raw-FFI replacement.
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![app, activateIgnoringOtherApps: true];
        run_detail_save_as(&entry);
    }
}

/// Main-thread completion callback for a generated hi-res detail preview: re-validate
/// freshness (detail visible AND the same entry still selected); on a hit, rebuild the
//  whole panel -- show_detail_for_sel consumes the DETAIL_PENDING_HD slot directly. On a
/// miss, drop the slot ({hash}.detail is already cached, so the next open takes the fast
/// disk path).
pub(super) extern "C" fn detail_preview_ready(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut AnyObject,
) {
    // Read the slot under a scoped lock and release it before touching the slot again: this mutex is
    // not reentrant, and a `match *SLOT.lock()` scrutinee keeps its guard alive for the whole match,
    // so locking inside an arm deadlocks the main thread.
    let (job_hash, generation, epoch) = {
        let slot = DETAIL_PENDING_HD.lock().unwrap();
        match slot.as_ref() {
            Some((hash, generation, epoch, _)) => (*hash, *generation, *epoch),
            None => return,
        }
    };
    // A preview generated before a discard belongs to a deleted entry: refuse it here as well, so
    // the last step of the pipeline cannot show it (the bytes are released with the slot).
    let wanted = detail_result_still_wanted(detail_visible(), detail_current_hash(), job_hash);
    if !super::detail_slot_deliverable(
        generation,
        super::cache_generation(),
        epoch,
        super::current_hash_epoch(job_hash),
        wanted,
    ) {
        // The disk cache keeps the data for a later open, so dropping the slot bytes is safe.
        *DETAIL_PENDING_HD.lock().unwrap() = None;
        return;
    }
    unsafe { show_detail_for_sel() };
}

// ---------------------------------------------------------------------------------------------
// Detail actions (floating, the mock's `.btn` treatment): the mock's detail pane has no
// toolbar and no footer, but soft wrap and save-as are real features, so they float over the
// top-right of the pane instead of reserving a strip. They sit in the mock's pane padding band,
// so no body line is covered at rest; a long line scrolled under them keeps its text through a
// translucent chip on the same glass material.
// ---------------------------------------------------------------------------------------------

/// A detail action's hit target and its chip for bubble layout.
struct DetailActionSide {
    control: *mut AnyObject,
    chip: *mut AnyObject,
    w: f64,
}

/// Build the detail actions for an entry: save-as always, the code soft-wrap switch only for
/// code (it has nothing to switch for text or images).
///
/// They are views over the scrolling column, not floats inside it. The mock's `.detail-scroll`
/// (the pane's only scroller) scrolls its content under everything above it, so a chip floating
/// over the column would sit on the panel material while a body scrolled past it; outside the
/// scroll they stay put, which is the mock's behaviour with no chrome of its own in the column.
unsafe fn add_detail_actions(content: *mut AnyObject, entry: &ClipEntry, width: f64) {
    let is_code = entry.image.is_none() && classify_text(&entry.text) == TextKind::Code;
    let mut actions: Vec<DetailActionSide> = Vec::new();
    if is_code {
        if let Some(action) = add_wrap_action(content) {
            actions.push(action);
        }
    }
    actions.push(add_save_as_action(content, entry.image.is_some()));
    // Trailing, right-aligned inside the pane's inset: save-as is the outer (rightmost) action.
    let mut x = width - DETAIL_INSET_RIGHT;
    for action in actions.iter().rev() {
        x -= action.w;
        let _: () = msg_send![action.control, setFrameOrigin: NSPoint::new(x, DETAIL_INSET_TOP)];
        let _: () = msg_send![action.chip, setFrameOrigin: NSPoint::new(x, DETAIL_INSET_TOP)];
        x -= DETAIL_ACTION_GAP;
    }
}

/// A translucent chip behind a floating action (the mock's `.btn` background), so a line
/// scrolled underneath keeps its contrast. Rebuilt with the action, so a mode change is a
/// rebuild and the chip follows the new palette.
unsafe fn add_chip(content: *mut AnyObject, width: f64) -> *mut AnyObject {
    let chip: *mut AnyObject = msg_send![class!(NSView), alloc];
    let chip: *mut AnyObject = msg_send![
        chip,
        initWithFrame: NSRect::new(
            NSPoint::new(0.0, DETAIL_INSET_TOP),
            NSSize::new(width, DETAIL_ACTION_H)
        )
    ];
    let _: () = msg_send![chip, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![chip, layer];
    crate::ffi::layer_set_background(layer, crate::ffi::hex_to_cg_color(detail_action_fill()));
    let _: () = msg_send![layer, setCornerRadius: crate::theme::RADIUS_CONTROL];
    let _: () = msg_send![content, addSubview: chip];
    chip
}

/// The floating actions' chip fill: a light glass wash over the panel material, or the palette's
/// field fill in dark mode, so the chip never turns into a bright patch on the dark material.
fn detail_action_fill() -> u32 {
    if clipboard_palette().dark {
        clipboard_palette().field_bg
    } else {
        DETAIL_ACTION_FILL_LIGHT
    }
}

/// The soft-wrap switch, with its label as the tooltip: a 28pt pill inside the pane's padding
/// band. Returns the switch and the chip behind it.
unsafe fn add_wrap_action(content: *mut AnyObject) -> Option<DetailActionSide> {
    let enabled = DETAIL_SOFT_WRAP_ENABLED.load(Ordering::SeqCst);
    // Reuse the existing switch during detail refreshes and create it only once, so rebuilding
    // the detail body cannot interrupt its animation.
    let button = detail_wrap_button(content);
    let newly_created = button.is_null();
    let chip = add_chip(content, DETAIL_SWITCH_CHIP_W);
    let button = if newly_created {
        crate::settings::make_shared_switch(
            DETAIL_SWITCH_CHIP_W - DETAIL_SWITCH_INSET,
            DETAIL_INSET_TOP,
            DETAIL_ACTION_H,
            enabled,
            observer(),
            sel!(toggleDetailSoftWrap:),
        )
    } else {
        button
    };
    let tooltip_key = if enabled {
        "clipboard.detail_soft_wrap_on"
    } else {
        "clipboard.detail_soft_wrap_off"
    };
    let tooltip_ns = make_nsstring(&t(tooltip_key));
    let _: () = msg_send![button, setToolTip: tooltip_ns];
    CFRelease(tooltip_ns as *const c_void);
    if newly_created {
        let _: () = msg_send![content, addSubview: button];
        release_obj(button);
    }
    Some(DetailActionSide {
        control: button,
        chip,
        w: DETAIL_SWITCH_CHIP_W,
    })
}

/// The save-as button: the same glyph and hover behaviour as before, floated over the pane's
/// top-right corner with a chip under it. Its tooltip says text or image file.
unsafe fn add_save_as_action(content: *mut AnyObject, is_image: bool) -> DetailActionSide {
    let chip = add_chip(content, DETAIL_ACTION_H);
    let button: *mut AnyObject = msg_send![hover_button_class(), alloc];
    let button: *mut AnyObject = msg_send![
        button,
        initWithFrame: NSRect::new(
            NSPoint::new(0.0, DETAIL_INSET_TOP),
            NSSize::new(DETAIL_ACTION_H, DETAIL_ACTION_H)
        )
    ];
    let empty = make_nsstring("");
    let _: () = msg_send![button, setTitle: empty];
    CFRelease(empty as *const c_void);
    let _: () = msg_send![button, setBordered: false];
    let _: () = msg_send![button, setTarget: observer()];
    let _: () = msg_send![button, setAction: sel!(detailSaveAs:)];
    let _: () = msg_send![button, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![button, layer];
    let _: () = msg_send![layer, setCornerRadius: crate::theme::RADIUS_CONTROL];
    let icon = make_detail_save_icon(false);
    let _: () = msg_send![button, setImage: icon];
    let _: () = msg_send![button, setImagePosition: 1u64]; // NSImageOnly
    release_obj(icon);
    let tooltip_key = if is_image {
        "clipboard.detail_save_as_image"
    } else {
        "clipboard.detail_save_as_text"
    };
    let tooltip = make_nsstring(&t(tooltip_key));
    let _: () = msg_send![button, setToolTip: tooltip];
    CFRelease(tooltip as *const c_void);
    add_hover_tracking(button);
    let _: () = msg_send![content, addSubview: button];
    release_obj(button);
    DetailActionSide {
        control: button,
        chip,
        w: DETAIL_ACTION_H,
    }
}

/// Select the entry the detail follows, or `None` when nothing is selected (search-field focus)
/// or the list is empty.
fn selected_detail_entry() -> Option<ClipEntry> {
    let sel = picker_selection();
    if sel == NO_SELECTION {
        return None;
    }
    let h_idx = mapped_index(sel)?;
    let history = CLIP_HISTORY.lock().unwrap();
    history.get(h_idx).cloned()
}

/// Show the selected entry's detail: the expanding pane when [`detail_expands_in_pane`] says the
/// panel is narrow, else the side panel right of the picker.
pub(super) unsafe fn show_detail_for_sel() {
    // Every step below sends AppKit messages, and objc2's debug `msg_send!` verification traps
    // (a panic that cannot unwind out of the AppKit callback that called here, i.e. an abort)
    // when a send lands off the main thread: an NSImage's size, for instance, only answers on
    // it. Refusing here turns that abort into the no-op a caller already handles.
    if !crate::is_main_thread() {
        log_info!("[clip] detail open skipped: not on the main thread");
        return;
    }
    let Some(entry) = selected_detail_entry() else {
        return;
    };
    let detail_was_visible = detail_visible();
    ensure_detail_window();
    let window = match *DETAIL_WINDOW.lock().unwrap() {
        Some(w) => w.0,
        None => return,
    };
    let content = match *DETAIL_CONTENT.lock().unwrap() {
        Some(c) => c.0,
        None => return,
    };
    cancel_detail_close(window);
    let picker_win = match *PICKER_WINDOW.lock().unwrap() {
        Some(w) => w.0,
        None => return,
    };
    // Panel coordinates: the picker's window is padded for its elevation shadow, and a detail sized or
    // aligned against the padded rect would be off by the padding and would re-add it on the way back.
    let picker_frame = crate::glass::panel_frame_of(picker_win);
    if !detail_was_visible {
        *DETAIL_PICKER_ORIGINAL_ORIGIN.lock().unwrap() = Some(picker_frame.origin);
    }
    let screen_frame = picker_screen_frame(picker_win);
    // The narrow panel expands INSIDE itself (the mock's `@media (max-width: 840px)` branch: the
    // list is replaced by the detail in the same panel); any other panel keeps the side pane. Both
    // lay the body out for a panel rect, which is the only difference between the two modes.
    let layout = picker_layout(screen_frame.size.width, true);
    let in_pane = !layout.split;
    let width_changed = (picker_frame.size.width - layout.panel_w).abs() > 0.5
        || DETAIL_SPLIT.load(Ordering::SeqCst) != layout.split;
    DETAIL_SPLIT.store(layout.split, Ordering::SeqCst);
    set_picker_pane_mode(if in_pane {
        PickerPaneMode::Detail
    } else {
        PickerPaneMode::List
    });
    let mut target_frame = picker_frame;
    target_frame.size.width = layout.panel_w;
    // Widening keeps the panel's centre: the mock's `.panel` stays centred on `.desk` when it
    // expands (`.is-expanded` grows 640 -> 960 symmetrically), so both edges move out rather
    // than the detail pushing the whole panel right. Closing restores the captured origin.
    // The shift applies only when the width actually changed: `picker_frame` is read back from
    // AppKit, and arrow-key re-entries while the detail is open would otherwise accumulate
    // sub-pixel frame-alignment error into a real drift.
    if width_changed {
        target_frame.origin.x += (picker_frame.size.width - layout.panel_w) / 2.0;
    }
    target_frame.origin =
        super::drag::clamp_picker_origin(target_frame.origin, target_frame.size, screen_frame);
    crate::glass::set_panel_frame(picker_win, target_frame, true);
    layout_picker_chrome(target_frame.size);
    let body = detail_content_frame(target_frame.size);
    let target_w = layout.detail_w;
    let target_h = body.size.height;
    let detail_frame = NSRect::new(
        NSPoint::new(
            if layout.split {
                layout.list_w + 1.0
            } else {
                0.0
            },
            FOOTER_H,
        ),
        NSSize::new(target_w, target_h),
    );
    let _: () = msg_send![content, setAutoresizingMask: 0u64];
    let _: () = msg_send![content, setFrame: detail_frame];
    let parent = PICKER_CONTENT_PARENT
        .lock()
        .unwrap()
        .expect("picker content parent")
        .0;
    let _: () = msg_send![parent, addSubview: content];
    let _: () = msg_send![content, setAutoresizingMask: 0u64];
    let _: () = msg_send![content, setHidden: false];
    let _: () = msg_send![window, orderOut: std::ptr::null::<AnyObject>()];
    let preserved_wrap_button =
        if entry.image.is_none() && classify_text(&entry.text) == TextKind::Code {
            detail_wrap_button(content)
        } else {
            std::ptr::null_mut()
        };

    // Clear the old content: removeFromSuperview releases it (parent-owned; never released
    // again -- the same discipline as rebuild_rows). The detail text-view pointer is
    // cleared too (no dangling pointer).
    *DETAIL_TEXT_VIEW.lock().unwrap() = None;
    *DETAIL_SOFT_WRAP_TEXT_VIEW.lock().unwrap() = None;
    *DETAIL_SOURCE_MAP.lock().unwrap() = None;
    // Detail content rebuilds the scroll view each time, so clear stale pointers before
    // removing the old views and avoid wheel/drag callbacks touching them.
    *DETAIL_SCROLL_VIEW.lock().unwrap() = None;
    *DETAIL_SCROLL_INDICATOR.lock().unwrap() = None;
    *DETAIL_HORIZONTAL_SCROLL_INDICATOR.lock().unwrap() = None;
    *SCROLL_DRAG.lock().unwrap() = None;
    let subs: *mut AnyObject = msg_send![content, subviews];
    let count: usize = msg_send![subs, count];
    for i in 0..count {
        let v: *mut AnyObject = msg_send![subs, objectAtIndex: i as isize];
        // Preserve the toolbar switch while toggling code wrapping so the settings component
        // can finish its state animation.
        if v == preserved_wrap_button {
            continue;
        }
        let _: () = msg_send![v, removeFromSuperview];
    }

    // The pane body: shared by both modes, laid out at the target panel's width and height.
    let (w, h) = build_detail_body(content, &entry, target_w, target_h);

    let _ = (w, h);
    if !detail_was_visible {
        animate_detail_content(content, true);
    }
    if width_changed {
        rebuild_rows();
    }
    install_detail_divider(parent, body, layout.list_w, layout.split);
    super::drag::raise_drag_surface(parent);
    set_detail_visible(true);
    // Once full document layout and the final window frame are both applied, unconditionally
    // set AppKit's actual constrained top. Mouse detail clicks, keyboard Right, and Up/Down
    // navigation while detail is open all converge here and therefore behave identically.
    if let Some(scroll) = *DETAIL_SCROLL_VIEW.lock().unwrap() {
        scroll_detail_to_top(scroll.0);
        let clip: *mut AnyObject = msg_send![scroll.0, contentView];
        let bounds: NSRect = msg_send![clip, bounds];
        if let Some((min_y, max_y)) = detail_scroll_range(scroll.0) {
            log_debug!(
                "[clip] detail scroll initialized: y={:.1} range={:.1}..{:.1}",
                bounds.origin.y,
                min_y,
                max_y
            );
        }
    }
    refresh_detail_action_visuals();
}

/// Build the detail body into `content`: the full untruncated text for text entries, the large
/// preview for images, then the floating actions. Returns the panel size the body asks for at
/// the target panel width.
unsafe fn build_detail_body(
    content: *mut AnyObject,
    entry: &ClipEntry,
    panel_w: f64,
    max_detail_h: f64,
) -> (f64, f64) {
    let pane = detail_pane_layout_with(
        detail_insets(picker_shows_detail_in_pane(), panel_w),
        panel_w,
        max_detail_h,
        meta_table_field_count(),
    );
    if let Some(img) = &entry.image {
        let mut installed = false;
        if let Some(png) = ensure_detail_preview(img) {
            let data: *mut AnyObject = msg_send![class!(NSData), dataWithBytes: png.as_ptr() as *const c_void, length: png.len()];
            let image: *mut AnyObject = msg_send![class!(NSImage), alloc];
            let image: *mut AnyObject = msg_send![image, initWithData: data];
            if !image.is_null() {
                let size: NSSize = msg_send![image, size];
                add_detail_image(content, image, (size.width, size.height), &pane, entry);
                release_obj(image);
                installed = true;
            }
        }
        if !installed {
            add_detail_text(
                content,
                &entry.text,
                panel_w,
                max_detail_h,
                DETAIL_LINE_H,
                entry,
                TextKind::Plain,
                None,
                false,
                TextKind::Plain,
            );
        }
    } else {
        let kind = classify_text(&entry.text);
        let wrap = kind == TextKind::Code && DETAIL_SOFT_WRAP_ENABLED.load(Ordering::SeqCst);
        let prepared = if kind == TextKind::Code {
            Some(if wrap {
                prepare_code_for_soft_wrap(&entry.text, detail_code_max_columns(panel_w))
            } else {
                prepare_code_no_wrap_display(&entry.text)
            })
        } else {
            None
        };
        add_detail_text(
            content,
            &entry.text,
            panel_w,
            max_detail_h,
            DETAIL_LINE_H,
            entry,
            TextKind::Plain,
            prepared.as_deref(),
            wrap,
            kind,
        );
    }
    add_detail_actions(content, entry, panel_w);
    if picker_shows_detail_in_pane() {
        add_detail_back_button(content);
    }
    (panel_w, max_detail_h)
}

/// The responsive breakpoint is based on available screen width, as the HTML viewport is.
#[cfg(test)]
pub(super) fn detail_expands_at_width(available_width: f64) -> bool {
    !picker_layout(available_width, true).split
}

/// The rect the picker body occupies, in the panel's flipped coordinates: under the header strip
/// and above the footer. The list scroll view and the in-pane detail both stand in it, so switching
/// between them moves the body from one surface to the other without moving the chrome.
pub(super) fn detail_content_frame(panel: NSSize) -> NSRect {
    NSRect::new(
        NSPoint::new(0.0, FOOTER_H),
        NSSize::new(
            panel.width,
            (panel.height - header_strip_h() - FOOTER_H).max(0.0),
        ),
    )
}

/// The detail pane's height cap: the picker's own height. A floor of the pane minimum
/// (the mock's meta table plus a line of body) is NOT usable here: clamping the panel up to it
/// produced a detail taller than the picker, and `detail_frame_for` asserts the opposite.
#[cfg(test)]
pub(super) fn detail_max_height(picker: NSRect) -> f64 {
    picker.size.height
}

/// The panel height for a body whose preview is `content_h` tall: the pane minimum when there is
/// room for it, otherwise the cap itself.
///
/// This maximizes before it minimizes. `clamp` panics when its floor exceeds its ceiling, and it
/// really does: the pane minimum is the meta table's 317pt (five rows of two body lines, the
/// mock's 28pt top pad and 11pt gaps) plus a 78pt body floor, so about 395pt, while the shortest
/// picker is 280pt -- a cap smaller than the floor used to abort the whole open. The cap is a hard
/// bound (`detail_frame_for` keeps the panel inside the picker), so it wins; the pane then has no
/// body above the table, which the scrolling column already handles.

/// The largest body section at a given panel height cap.
#[cfg(test)]
fn pane_max_body_h(max_detail_h: f64) -> f64 {
    detail_pane_layout(DETAIL_MAX_W, max_detail_h, meta_table_field_count()).body_h
}

/// The body's own height for `lines` visual lines, capped at the pane's body share.
#[cfg(test)]
fn detail_body_height(lines: usize, max_height: f64) -> f64 {
    (lines as f64 * DETAIL_LINE_H).min(pane_max_body_h(max_height))
}

/// Map the detail code column's width to soft-wrap sizing columns; no characters are inserted
/// into the display. The budget uses the REAL code column (the content column minus the mock's
/// 26pt gutter and 6pt gap) and the measured `CODE_ADVANCE_PT`; SAFETY absorbs rounding and
/// small font-metric drift.
fn detail_code_max_columns(width: f64) -> usize {
    let container = width
        - DETAIL_INSET_LEFT
        - DETAIL_INSET_RIGHT
        - DETAIL_CODE_GUTTER_W
        - DETAIL_CODE_GUTTER_GAP;
    let columns = (container / CODE_ADVANCE_PT).floor().max(0.0) as usize;
    columns.saturating_sub(DETAIL_CODE_WRAP_SAFETY).max(24)
}

/// Size the panel from the prepared soft-wrap model. U+2028 already represents final visual
/// wraps, so line widths are not scanned again. The second value is the BODY's own height; the
/// panel adds the meta table and the pane insets around it.
#[cfg(test)]
fn detail_prepared_code_size(prepared: &PreparedCodeDisplay, max_height: f64) -> (f64, f64) {
    let lines = prepared
        .text
        .chars()
        .filter(|ch| matches!(ch, '\n' | '\u{2028}'))
        .count()
        + 1;
    (DETAIL_CODE_MAX_W, detail_body_height(lines, max_height))
}

/// Size unwrapped code from hard lines only; native horizontal scrolling handles excess width.
#[cfg(test)]
pub(super) fn detail_unwrapped_code_size(text: &str, max_height: f64) -> (f64, f64) {
    let lines = text.split('\n').count().max(1);
    (DETAIL_CODE_MAX_W, detail_body_height(lines, max_height))
}

/// Compute detail text-panel dimensions (type-specific width, body height capped by the picker).
#[cfg(test)]
pub(super) fn detail_text_size(text: &str, kind: TextKind, max_height: f64) -> (f64, f64) {
    if kind == TextKind::Code {
        let prepared = prepare_code_for_soft_wrap(text, detail_code_max_columns(DETAIL_CODE_MAX_W));
        return detail_prepared_code_size(&prepared, max_height);
    }
    let w = DETAIL_MAX_W;
    // The wrap width is the CONTENT column, not the panel: the pane insets and the meta table's
    // capsule clip are not text area (the mock's `.detail-text` lives inside the 22/26 padding).
    let avail_w = detail_pane_layout(w, 0.0, meta_table_field_count()).content_w;
    let lines = estimate_lines(text, detail_text_units(avail_w));
    (w, detail_body_height(lines, max_height))
}

/// Build the detail text view: plain text wraps naturally; code inserts prioritized U+2028 soft
/// wraps, with markers remaining drawing-only decorations. Text remains mouse-selectable; copying
/// uses the native context menu or Cmd+C forwarded by the picker because the detail never becomes key.
pub(super) fn detail_text_view_class() -> *mut AnyObject {
    detail_text_view_class_handle().0 as *mut AnyObject
}

/// The class handle itself, so the `drawRect:` super dispatch can name the class that defined
/// that override instead of the superclass it calls into.
fn detail_text_view_class_handle() -> StaticClass {
    static CLASS: OnceLock<StaticClass> = OnceLock::new();
    crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabClipDetailTextView", || {
        crate::ffi::build_class(
            c"OhMyTabClipDetailTextView",
            class!(NSTextView),
            |builder| unsafe {
                builder.add_method::<AnyObject, _>(
                    sel!(copy:),
                    detail_text_view_copy
                        as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
                builder.add_method::<AnyObject, _>(
                    sel!(drawRect:),
                    detail_text_view_draw_rect as unsafe extern "C" fn(*mut AnyObject, Sel, NSRect),
                );
            },
        )
    })
}

/// The native Copy menu must also pass through the source mapping, never copying formatted text.
extern "C" fn detail_text_view_copy(_self: *mut AnyObject, _cmd: Sel, _sender: *mut AnyObject) {
    copy_detail_selection();
}

struct SoftWrapGlyphs {
    end: CallbackTarget,
    continuation: CallbackTarget,
    end_size: NSSize,
    continuation_size: NSSize,
}

/// The soft-wrap glyphs for the current appearance *and material*. Cached per (mode, material): `drawRect`
/// can run frequently, so it must not rebuild attributed strings on every repaint, but a cache keyed on the
/// mode alone keeps the colour it was first built with and goes stale on a material switch -- the detail
/// panel is long-lived and the ink now depends on the material.
unsafe fn soft_wrap_glyphs() -> &'static SoftWrapGlyphs {
    /// One lazily built entry per material: only the current material's slot is ever built, because
    /// `panel_ink` resolves against the material in effect when the string is built.
    struct Slots {
        liquid_glass: OnceLock<SoftWrapGlyphs>,
        frost: OnceLock<SoftWrapGlyphs>,
        opaque: OnceLock<SoftWrapGlyphs>,
        backdrop: OnceLock<SoftWrapGlyphs>,
    }
    impl Slots {
        fn slot(&self, material: u8) -> &OnceLock<SoftWrapGlyphs> {
            match material {
                1 => &self.frost,
                2 => &self.opaque,
                3 => &self.backdrop,
                _ => &self.liquid_glass,
            }
        }
    }
    static LIGHT: Slots = Slots {
        liquid_glass: OnceLock::new(),
        frost: OnceLock::new(),
        opaque: OnceLock::new(),
        backdrop: OnceLock::new(),
    };
    static DARK: Slots = Slots {
        liquid_glass: OnceLock::new(),
        frost: OnceLock::new(),
        opaque: OnceLock::new(),
        backdrop: OnceLock::new(),
    };
    let dark = crate::theme::resolved_is_dark();
    let cache = if dark { &DARK } else { &LIGHT };
    let material = super::text_style::material_cache_key();
    cache
        .slot(material)
        .get_or_init(|| build_soft_wrap_glyphs(dark, material))
}

unsafe fn build_soft_wrap_glyphs(dark: bool, material: u8) -> SoftWrapGlyphs {
    // The slot's key must be the material the ink is about to be resolved for, or the cache would store a
    // string built for a different one (the whole reason this cache is keyed by material).
    debug_assert_eq!(material, super::text_style::material_cache_key());
    let attrs: *mut AnyObject = msg_send![class!(NSMutableDictionary), alloc];
    let attrs: *mut AnyObject = msg_send![attrs, init];
    let font_key = make_nsstring("NSFont");
    let color_key = make_nsstring("NSColor");
    let font: *mut AnyObject = msg_send![class!(NSFont), monospacedSystemFontOfSize: crate::theme::FONT_CAPTION, weight: crate::theme::FONT_WEIGHT_REGULAR];
    // An annotation mark, so muted; the palette is passed in so the two caches cannot be
    // built from a mode other than the one that keyed them, and `material` is the slot's own key.
    let color = crate::glass::panel_ink(
        crate::theme::ui_palette_for_mode(dark).muted_text,
        crate::glass::PanelInk::Muted,
    );
    let _: () = msg_send![attrs, setObject: font, forKey: font_key];
    let _: () = msg_send![attrs, setObject: color, forKey: color_key];
    CFRelease(font_key as *const c_void);
    CFRelease(color_key as *const c_void);
    let end_ns = make_nsstring("↵");
    let end: *mut AnyObject = msg_send![class!(NSAttributedString), alloc];
    let end: *mut AnyObject = msg_send![end, initWithString: end_ns, attributes: attrs];
    CFRelease(end_ns as *const c_void);
    let continuation_ns = make_nsstring("↪");
    let continuation: *mut AnyObject = msg_send![class!(NSAttributedString), alloc];
    let continuation: *mut AnyObject = msg_send![
        continuation,
        initWithString: continuation_ns,
        attributes: attrs
    ];
    CFRelease(continuation_ns as *const c_void);
    release_obj(attrs);
    SoftWrapGlyphs {
        end: CallbackTarget::new(end),
        continuation: CallbackTarget::new(continuation),
        end_size: msg_send![end, size],
        continuation_size: msg_send![continuation, size],
    }
}

/// Draw soft-wrap markers after NSTextView finishes, reading layout results without modifying
/// textStorage.
extern "C" fn detail_text_view_draw_rect(_self: *mut AnyObject, _cmd: Sel, rect: NSRect) {
    unsafe {
        type Draw = unsafe extern "C" fn(*mut ObjcSuper, Sel, NSRect);
        // The class that defines this override, not NSTextView: objc_super starts its search there,
        // so passing the superclass would skip the implementation that must run first.
        let mut sup =
            ObjcSuper::for_defined_class(_self as *mut c_void, detail_text_view_class_handle());
        let draw: Draw = std::mem::transmute(objc_msgSendSuper as *const ());
        draw(&mut sup, sel!(drawRect:), rect);

        let view = _self;
        let is_code = DETAIL_SOFT_WRAP_TEXT_VIEW
            .lock()
            .unwrap()
            .is_some_and(|code_view| code_view.0 == view);
        if !is_code {
            return;
        }
        let layout: *mut AnyObject = msg_send![view, layoutManager];
        let container: *mut AnyObject = msg_send![view, textContainer];
        let string: *mut AnyObject = msg_send![view, string];
        let text_len: usize = msg_send![string, length];
        let glyph_count: usize = msg_send![layout, numberOfGlyphs];
        if glyph_count == 0 || text_len == 0 {
            return;
        }
        let text_origin: NSPoint = msg_send![view, textContainerOrigin];
        // Only inspect glyphs intersecting the invalidated rectangle, plus one preceding line
        // to recover whether the first visible line is a soft-wrap continuation.
        let layout_rect = NSRect::new(
            NSPoint::new(rect.origin.x - text_origin.x, rect.origin.y - text_origin.y),
            rect.size,
        );
        let visible_glyphs: NSRange = msg_send![
            layout,
            glyphRangeForBoundingRect: layout_rect,
            inTextContainer: container
        ];
        if visible_glyphs.length == 0 {
            return;
        }
        let mut glyph = visible_glyphs.location;
        if glyph > 0 {
            let mut previous = NSRange::new(0, 0);
            let _: NSRect = msg_send![
                layout,
                lineFragmentUsedRectForGlyphAtIndex: glyph - 1,
                effectiveRange: &mut previous
            ];
            glyph = previous.location;
        }
        let glyph_end = visible_glyphs
            .location
            .saturating_add(visible_glyphs.length)
            .min(glyph_count);
        let glyphs = soft_wrap_glyphs();
        let end_attr = glyphs.end.0;
        let continuation_attr = glyphs.continuation.0;
        let mut continuation = false;
        while glyph < glyph_end {
            let mut effective = NSRange::new(0, 0);
            let fragment: NSRect = msg_send![
                layout,
                lineFragmentUsedRectForGlyphAtIndex: glyph,
                effectiveRange: &mut effective
            ];
            if effective.length == 0 {
                glyph += 1;
                continue;
            }
            let character_range: NSRange = msg_send![
                layout,
                characterRangeForGlyphRange: effective,
                actualGlyphRange: std::ptr::null_mut::<NSRange>()
            ];
            let char_start = character_range.location;
            let char_end = character_range
                .location
                .saturating_add(character_range.length)
                .min(text_len);
            let line = NSRect::new(
                NSPoint::new(
                    fragment.origin.x + text_origin.x,
                    fragment.origin.y + text_origin.y,
                ),
                fragment.size,
            );
            let visible = line.origin.y + line.size.height >= rect.origin.y
                && line.origin.y <= rect.origin.y + rect.size.height;
            if visible && continuation {
                let x = (line.origin.x - glyphs.continuation_size.width - 2.0).max(0.0);
                let y = line.origin.y + (line.size.height - glyphs.continuation_size.height) / 2.0;
                let _: () = msg_send![continuation_attr, drawAtPoint: NSPoint::new(x, y)];
            }

            // A soft wrap is a line-fragment boundary without a hard '\n' at either side.
            let char_at_end: u16 = if char_end < text_len {
                msg_send![string, characterAtIndex: char_end]
            } else {
                0
            };
            let char_before_end: u16 = if char_end > char_start {
                msg_send![string, characterAtIndex: char_end - 1]
            } else {
                0
            };
            let hard_break = char_end >= text_len
                || char_at_end == '\n' as u16
                || char_before_end == '\n' as u16;
            let soft_wrap = char_end < text_len && !hard_break;
            if visible && soft_wrap {
                // Draw the marker just OUTSIDE the fragment's right edge with a 2pt gap instead
                // of on top of the trailing glyph -- the old right-aligned placement brushed
                // against the last glyph and was most visible after commas. The wrap budget
                // (68 columns) is deliberately below container capacity (~72 columns), so every
                // soft-wrapped line keeps >= 4 spare columns: the marker can neither clip at the
                // panel edge nor cover any content.
                let x = line.origin.x + line.size.width + 2.0;
                let y = line.origin.y + (line.size.height - glyphs.end_size.height) / 2.0;
                let _: () = msg_send![end_attr, drawAtPoint: NSPoint::new(x, y)];
            }
            continuation = soft_wrap;
            glyph = effective
                .location
                .saturating_add(effective.length)
                .max(glyph.saturating_add(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // The pane's geometry lives in the clipboard module's own (re-exported) helpers; the tests
    // pin against them so a padding or row-height change fails here rather than on screen.
    use crate::clipboard::{detail_pane_layout, detail_pane_min_height, meta_table_field_count};

    #[test]
    fn ext_for_image_uti_maps_known_families_case_insensitively() {
        assert_eq!(ext_for_image_uti("public.jpeg"), "jpg");
        assert_eq!(ext_for_image_uti("public.gif"), "gif");
        assert_eq!(ext_for_image_uti("public.heic"), "heic");
        assert_eq!(ext_for_image_uti("public.heif"), "heic");
        assert_eq!(ext_for_image_uti("public.tiff"), "tif");
        assert_eq!(ext_for_image_uti("org.webmproject.webp"), "webp");
        assert_eq!(ext_for_image_uti("com.microsoft.bmp"), "bmp");
        // The lookup lowercases before matching family substrings.
        assert_eq!(ext_for_image_uti("PUBLIC.JPEG"), "jpg");
        assert_eq!(ext_for_image_uti("Public.Heic"), "heic");
        assert_eq!(ext_for_image_uti("PUBLIC.WEBP"), "webp");
        // PNG and unrecognized UTIs share the fallback.
        assert_eq!(ext_for_image_uti("public.png"), "png");
        assert_eq!(ext_for_image_uti("com.example.foo"), "png");
        assert_eq!(ext_for_image_uti(""), "png");
    }

    #[test]
    fn detail_code_max_columns_reserves_safety_and_floors_at_24() {
        // The code column is the content column minus the mock's gutter. The expected number is
        // derived from the same constants the function reads, so a padding or gutter change
        // updates both instead of leaving a hand-counted literal behind.
        assert!(detail_code_max_columns(DETAIL_CODE_MAX_W) >= 24);
        // The budget bounds the code column: its widest wrapped line must fit inside it.
        assert!(
            detail_code_max_columns(DETAIL_CODE_MAX_W) as f64 * CODE_ADVANCE_PT
                <= DETAIL_CODE_MAX_W
                    - DETAIL_INSET_LEFT
                    - DETAIL_INSET_RIGHT
                    - DETAIL_CODE_GUTTER_W
                    - DETAIL_CODE_GUTTER_GAP
        );
        // The same rule at a narrow panel width, derived from the same terms: the four-column
        // reserve is consumed twice over on the 48pt shorter column metric.
        let narrow_budget = 400.0
            - DETAIL_INSET_LEFT
            - DETAIL_INSET_RIGHT
            - DETAIL_CODE_GUTTER_W
            - DETAIL_CODE_GUTTER_GAP;
        let narrow_fitted =
            (narrow_budget / CODE_ADVANCE_PT).floor() as usize - DETAIL_CODE_WRAP_SAFETY;
        assert_eq!(detail_code_max_columns(400.0), narrow_fitted.max(24));
        assert!(detail_code_max_columns(400.0) < detail_code_max_columns(DETAIL_CODE_MAX_W));
        // A zero or negative budget never underflows below the floor.
        assert_eq!(detail_code_max_columns(0.0), 24);
        assert_eq!(detail_code_max_columns(-100.0), 24);
        assert!(detail_code_max_columns(DETAIL_CODE_MAX_W) > detail_code_max_columns(400.0));
    }

    #[test]
    fn detail_prepared_code_size_tracks_visual_lines_and_clamps() {
        let cap = 2000.0;
        let columns = detail_code_max_columns(DETAIL_CODE_MAX_W);

        let one_line = prepare_code_for_soft_wrap("let x = 1;", columns);
        let (w, h) = detail_prepared_code_size(&one_line, cap);
        assert_eq!(w, DETAIL_CODE_MAX_W);
        // The body reports its own height: one line. The panel adds the meta table around it
        // (the old version folded the toolbar/footer into this value, which the mock has not).
        assert_eq!(h, DETAIL_LINE_H);

        let two_lines = prepare_code_for_soft_wrap(
            "let a = 1;\nlet b = 2;\nlet c = 3;\nlet d = 4;\nlet e = 5;\nlet f = 6;",
            columns,
        );
        let (_, h_six) = detail_prepared_code_size(&two_lines, cap);
        assert_eq!(h_six, DETAIL_LINE_H * 6.0);
        assert!(h_six > h, "more hard lines must make the body taller");

        // The cap still clamps downward: never past what the pane leaves for the body.
        let cap_at_min = detail_pane_min_height(DETAIL_CODE_MAX_W, meta_table_field_count());
        let (_, capped) = detail_prepared_code_size(&two_lines, cap_at_min);
        assert!(
            capped <= pane_max_body_h(cap_at_min) + 1e-9,
            "a capped body must fit the pane's body share, got {capped}"
        );

        // Soft wraps insert U+2028, which the sizing must count as visual lines.
        let wrapped = prepare_code_for_soft_wrap(&"a".repeat(4000), columns);
        assert!(
            wrapped.text.contains('\u{2028}'),
            "a long line must soft-wrap at the given budget"
        );
        let (_, h_wrapped) = detail_prepared_code_size(&wrapped, cap);
        assert!(
            h_wrapped > h_six,
            "visual wraps must add body height at the same cap"
        );
    }

    #[test]
    fn detail_unwrapped_code_size_counts_hard_lines_only() {
        let cap = 2000.0;
        let (w, h) = detail_unwrapped_code_size("a\nb", cap);
        assert_eq!(w, DETAIL_CODE_MAX_W);
        assert_eq!(h, DETAIL_LINE_H * 2.0);
        // A long single line stays one line; native horizontal scrolling handles width.
        assert_eq!(
            detail_unwrapped_code_size(&"x".repeat(4000), cap).1,
            DETAIL_LINE_H
        );
    }

    /// The mock's detail pane has no toolbar and no footer: what the panel holds is the scrolling
    /// column (inset by the pane padding) and the meta table at its bottom.
    #[test]
    fn detail_panel_is_a_padding_inset_column_with_a_bottom_meta_table() {
        let cap = 900.0;
        let pane_min_h = detail_pane_min_height(DETAIL_MAX_W, meta_table_field_count());
        // A one-line body still sizes the panel to at least the table plus a minimum body.
        let (w, body_h) = detail_text_size("hi", TextKind::Plain, cap);
        let h = (body_h
            + detail_pane_layout(w, cap, meta_table_field_count())
                .meta
                .height
            + DETAIL_INSET_TOP
            + DETAIL_INSET_BOTTOM)
            .clamp(pane_min_h, cap);
        let pane = detail_pane_layout(w, h, meta_table_field_count());
        // The scrolling column is the panel minus the mock's padding on each side.
        assert_eq!(pane.scroll.origin.x, DETAIL_INSET_LEFT);
        assert_eq!(pane.scroll.origin.y, DETAIL_INSET_TOP);
        assert_eq!(
            pane.scroll.size.width,
            DETAIL_MAX_W - DETAIL_INSET_LEFT - DETAIL_INSET_RIGHT
        );
        assert_eq!(
            pane.scroll.size.height,
            h - DETAIL_INSET_TOP - DETAIL_INSET_BOTTOM
        );
        // The document is exactly the pane for a short body: the table then rests on the bottom
        // edge (the mock's margin-top: auto). The row frames that result are pinned where they
        // live, in the detail module's own tests.
        let doc_h = detail_document_height(pane.scroll.size.height, body_h, pane.meta.height);
        assert_eq!(doc_h, pane.scroll.size.height);
        assert!(pane.meta.height > 0.0);
        assert!(pane.content_w > 0.0);
    }
}
