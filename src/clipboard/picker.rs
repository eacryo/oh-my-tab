//! Clipboard subsystem · picker: the history overlay (window, rows, keyboard navigation).

use super::*;

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
    debug_assert!(h <= picker.size.height);
    let y = (align_top_y - h).max(picker_min_y).min(picker_max_y - h);

    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

/// Pure: lay out the picker and detail as one group, with the picker always left of detail.
pub(super) fn detail_group_frames(
    picker: NSRect,
    align_top_y: f64,
    screen: NSRect,
    detail_w: f64,
    detail_h: f64,
    center_on_main: bool,
    cursor_x: f64,
) -> (NSRect, NSRect) {
    let group_w = picker.size.width + DETAIL_GAP + detail_w;
    let min_x = screen.origin.x + PICKER_EDGE_MARGIN;
    let max_x = screen.origin.x + screen.size.width - PICKER_EDGE_MARGIN;
    let mut picker_x = if center_on_main {
        screen.origin.x + (screen.size.width - group_w) / 2.0
    } else {
        cursor_x - group_w / 2.0
    };

    // Clamp the whole group when it fits; if it does not, keep the picker whole and leave
    // detail in the right-side region.
    if group_w + 2.0 * PICKER_EDGE_MARGIN <= screen.size.width {
        picker_x = picker_x.max(min_x).min(max_x - group_w);
    } else {
        picker_x = min_x;
    }

    let picker_frame = NSRect::new(NSPoint::new(picker_x, picker.origin.y), picker.size);
    let detail_frame = detail_frame_for(picker_frame, align_top_y, screen, detail_w, detail_h);
    (picker_frame, detail_frame)
}

/// The frame of the picker's screen (follows it across screens; falls back to the main
/// screen when unavailable).
unsafe fn picker_screen_frame(picker_win: *mut AnyObject) -> NSRect {
    let sc: *mut AnyObject = msg_send![picker_win, screen];
    if sc.is_null() {
        let main: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
        msg_send![main, visibleFrame]
    } else {
        msg_send![sc, visibleFrame]
    }
}

/// The selected row's screen y (AppKit coords -- where the detail panel's top aligns):
/// the window top + the header strip + the row's flipped y within the document - the
/// current scroll offset. Locks are taken only to copy pointers/values, never held
/// across msg_send calls.
fn selected_row_screen_y(picker: NSRect) -> Option<f64> {
    let sel = picker_selection();
    if sel == NO_SELECTION {
        return None;
    }
    let pitches = ROW_PITCHES.lock().unwrap();
    if pitches.is_empty() {
        return None;
    }
    let row_idx = sel.min(pitches.len() - 1);
    // The group header is part of the row pitch but not the record content; align the detail
    // with the content block instead of the top of the group header.
    let group_header_h = {
        let filtered = with_clipboard_ui(|ui| ui.filtered.clone());
        let history = CLIP_HISTORY.lock().unwrap();
        let &history_idx = filtered.get(row_idx)?;
        let entry = history.get(history_idx)?;
        let current_group = day_group(entry.copied_at);
        let previous_group = filtered[..row_idx]
            .iter()
            .rev()
            .find_map(|&idx| history.get(idx).map(|e| day_group(e.copied_at)));
        if previous_group != Some(current_group) {
            GROUP_H
        } else {
            0.0
        }
    };
    let row_flipped = header_strip_h() + row_top(row_idx, &pitches) + group_header_h;
    drop(pitches);
    // Scroll offset: the clip view's bounds.origin.y (flipped). Read via SCROLL_VIEW,
    // NOT PICKER_CONTAINER: during key-driven scrollRectToVisible the container lock is
    // still held by the if-let temporary guard -- locking it here would self-deadlock
    // (a lesson this repo has already learned the hard way).
    let scroll_offset = {
        let sv = SCROLL_VIEW.lock().unwrap();
        match *sv {
            Some(s) => unsafe {
                let clip: *mut AnyObject = msg_send![s.0, contentView];
                if clip.is_null() {
                    0.0
                } else {
                    let b: NSRect = msg_send![clip, bounds];
                    b.origin.y
                }
            },
            None => 0.0,
        }
    };
    Some(picker.origin.y + picker.size.height - (row_flipped - scroll_offset))
}

/// Reposition the detail panel while it is open when the list scrolls (the selected row
/// moves): recompute the alignment y and setFrame only, no content rebuild. Hooked onto
/// the clip-view bounds-change notification; skipped during rebuild_rows -- its trailing
/// scroll restore fires the notification synchronously while ROW_PITCHES is still held,
/// and locking it here would self-deadlock on the same non-reentrant mutex.
pub(super) fn reposition_detail() {
    if !detail_visible() || REBUILDING.load(Ordering::SeqCst) {
        return;
    }
    unsafe {
        let picker_win = match *PICKER_WINDOW.lock().unwrap() {
            Some(w) => w.0,
            None => return,
        };
        let detail_win = match *DETAIL_WINDOW.lock().unwrap() {
            Some(w) => w.0,
            None => return,
        };
        let pf: NSRect = msg_send![picker_win, frame];
        let Some(align_top_y) = selected_row_screen_y(pf) else {
            return;
        };
        let cf: NSRect = msg_send![detail_win, frame];
        let sf = picker_screen_frame(picker_win);
        let frame = detail_frame_for(pf, align_top_y, sf, cf.size.width, cf.size.height);
        let _: () = msg_send![detail_win, setFrame: frame, display: true];
    }
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
        setDuration: DETAIL_PANEL_ANIMATION_DURATION
    ];

    let from_opacity_value: *mut AnyObject =
        msg_send![class!(NSNumber), numberWithFloat: from_opacity];
    let to_opacity_value: *mut AnyObject = msg_send![class!(NSNumber), numberWithFloat: to_opacity];
    let opacity_animation: *mut AnyObject = msg_send![
        class!(CABasicAnimation),
        animationWithKeyPath: opacity_key
    ];
    let _: () = msg_send![opacity_animation, setFromValue: from_opacity_value];
    let _: () = msg_send![opacity_animation, setToValue: to_opacity_value];
    let _: () = msg_send![
        opacity_animation,
        setDuration: DETAIL_PANEL_ANIMATION_DURATION
    ];

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

/// Open the detail panel by expanding a narrow panel horizontally while the picker moves to
/// the final combined layout.
unsafe fn animate_detail_open(
    picker_window: *mut AnyObject,
    detail_window: *mut AnyObject,
    detail_content: *mut AnyObject,
    target_picker_frame: NSRect,
    target_detail_frame: NSRect,
) {
    let collapsed_frame = NSRect::new(
        target_detail_frame.origin,
        NSSize::new(1.0, target_detail_frame.size.height),
    );
    let _: () = msg_send![detail_window, setFrame: collapsed_frame, display: false];
    let _: () = msg_send![detail_window, setAlphaValue: 0.0f64];
    let _: () = msg_send![detail_window, orderFrontRegardless];

    let _: () = msg_send![class!(NSAnimationContext), beginGrouping];
    let context: *mut AnyObject = msg_send![class!(NSAnimationContext), currentContext];
    let _: () = msg_send![context, setDuration: DETAIL_PANEL_ANIMATION_DURATION];
    let timing_name = make_nsstring("easeOut");
    let timing: *mut AnyObject =
        msg_send![class!(CAMediaTimingFunction), functionWithName: timing_name];
    if !timing.is_null() {
        let _: () = msg_send![context, setTimingFunction: timing];
    }
    CFRelease(timing_name as *const c_void);

    let picker_animator: *mut AnyObject = msg_send![picker_window, animator];
    let _: () = msg_send![picker_animator, setFrame: target_picker_frame, display: true];
    let detail_animator: *mut AnyObject = msg_send![detail_window, animator];
    let _: () = msg_send![detail_animator, setFrame: target_detail_frame, display: true];
    let _: () = msg_send![detail_animator, setAlphaValue: 1.0f64];
    let _: () = msg_send![class!(NSAnimationContext), endGrouping];

    animate_detail_content(detail_content, true);
}

/// Close the detail panel by shrinking it to a sliver and fading it out before orderOut.
unsafe fn animate_detail_close(
    picker_window: *mut AnyObject,
    detail_window: *mut AnyObject,
    detail_content: *mut AnyObject,
    restored_picker_frame: Option<NSRect>,
) {
    let current_detail_frame: NSRect = msg_send![detail_window, frame];
    let collapsed_frame = NSRect::new(
        current_detail_frame.origin,
        NSSize::new(1.0, current_detail_frame.size.height),
    );

    let _: () = msg_send![class!(NSAnimationContext), beginGrouping];
    let context: *mut AnyObject = msg_send![class!(NSAnimationContext), currentContext];
    let _: () = msg_send![context, setDuration: DETAIL_PANEL_ANIMATION_DURATION];
    let timing_name = make_nsstring("easeInEaseOut");
    let timing: *mut AnyObject =
        msg_send![class!(CAMediaTimingFunction), functionWithName: timing_name];
    if !timing.is_null() {
        let _: () = msg_send![context, setTimingFunction: timing];
    }
    CFRelease(timing_name as *const c_void);

    if let Some(frame) = restored_picker_frame {
        let picker_animator: *mut AnyObject = msg_send![picker_window, animator];
        let _: () = msg_send![picker_animator, setFrame: frame, display: true];
    }
    let detail_animator: *mut AnyObject = msg_send![detail_window, animator];
    let _: () = msg_send![detail_animator, setFrame: collapsed_frame, display: true];
    let _: () = msg_send![detail_animator, setAlphaValue: 0.0f64];
    let _: () = msg_send![class!(NSAnimationContext), endGrouping];

    animate_detail_content(detail_content, false);
    let _: () = msg_send![
        detail_window,
        performSelector: sel!(finishDetailClose:),
        withObject: std::ptr::null::<AnyObject>(),
        afterDelay: DETAIL_PANEL_ANIMATION_DURATION
    ];
}

/// Hide the detail window after the close animation; reopening during the delay keeps the old
/// callback from hiding the new panel.
extern "C" fn detail_finish_close(this: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    if detail_visible() {
        return;
    }
    unsafe {
        let window = this as *mut AnyObject;
        let _: () = msg_send![window, orderOut: std::ptr::null::<AnyObject>()];
        let _: () = msg_send![window, setAlphaValue: 1.0f64];
    }
}

/// Toggle the picker on Option+V (called on the main thread by the bridge).
pub(crate) extern "C" fn on_clipboard_toggle(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    // Ignore the summon when the master switch is off (Option+V must not open the picker
    // after the user disabled the feature in Settings).
    if !CONFIG.read().unwrap().clipboard.enabled {
        log_debug!("[clip] toggle ignored: clipboard history disabled");
        return;
    }
    if PICKER_VISIBLE.load(Ordering::SeqCst) {
        hide_picker();
        return;
    }
    // Show the picker even with an empty history (the empty-state hint lives in
    // rebuild_rows' empty branch).
    set_picker_selection(0);
    show_picker();
}

/// Show the picker (built once, reused; the window height follows the visible row count).
pub(super) fn show_picker() {
    unsafe {
        let show_started = Instant::now();
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
        let cursor: NSPoint = msg_send![class!(NSEvent), mouseLocation];
        let picker_pos = CONFIG.read().unwrap().clipboard.picker_position.clone();
        let center_on_main = picker_pos == "main";
        let main_screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
        let main_frame: NSRect = msg_send![main_screen, visibleFrame];
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

        // Max height: the 640pt hard cap, shrunk on small screens (120pt kept for the menu
        // bar / cursor offset / edge margins).
        let max_h = PICKER_MAX_HEIGHT.min(screen_frame.size.height - 120.0);
        // The visible row count derives from the height cap, floored to whole rows (no
        // half-cut row at the window's bottom). The estimate uses the plain (header-less)
        // row pitch -- the first row carries a 26pt group header, which would undercount.
        // The window height = the strip + the list + the bottom padding, so the list's
        // budget = max_h - strip - padding.
        // Rows are a uniform 61pt (ROW_H); the window height = the header + the list +
        // the footer + padding.
        let visible = if hist_len == 0 {
            0
        } else {
            (((max_h - header_strip_h() - FOOTER_H - PAD_Y) / ROW_H).floor() as usize)
                .min(hist_len)
                .max(1)
        };
        // With an empty history the list area is one hint row tall.
        let list_h = if hist_len == 0 {
            40.0
        } else {
            pitches.iter().take(visible).sum::<f64>()
        };
        // Use the same three-record visual minimum for every state, including one or zero rows.
        let h = (header_strip_h() + list_h + FOOTER_H + PAD_Y).max(picker_min_height());

        let frame = if center_on_main {
            // Always centered on the main screen; no flip/clamp.
            NSRect::new(
                NSPoint::new(
                    main_frame.origin.x + (main_frame.size.width - PICKER_W) / 2.0,
                    main_frame.origin.y + (main_frame.size.height - h) / 2.0,
                ),
                NSSize::new(PICKER_W, h),
            )
        } else {
            picker_frame_for(cursor, screen_frame, PICKER_W, h)
        };
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
        let _: () = msg_send![window, setFrame: frame, display: true];
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
                if rows_ready { "cached_view" } else { "rebuilt_view" },
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
    set_clear_history_confirmation_expanded(false);
    *SCROLL_DRAG.lock().unwrap() = None;
    // Hiding does not reliably deliver mouseExited to every child button; clear the row hover
    // state explicitly.
    *HOVER_ROW.lock().unwrap() = NO_SELECTION;
    hide_detail();

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
    // Remove the source row's filled detail icon on close without rebuilding the list.
    refresh_detail_action_visuals();
    // The picker may have shifted left for the combined layout; restore its original origin
    // while preserving its current height.
    let original_origin = DETAIL_PICKER_ORIGINAL_ORIGIN.lock().unwrap().take();
    // The content views get removed once the panel hides; clear the text-view pointer so
    // Cmd+C never dereferences a dangling one.
    *DETAIL_TEXT_VIEW.lock().unwrap() = None;
    *DETAIL_SOFT_WRAP_TEXT_VIEW.lock().unwrap() = None;
    *DETAIL_SOURCE_MAP.lock().unwrap() = None;
    let win = *DETAIL_WINDOW.lock().unwrap();
    let content = *DETAIL_CONTENT.lock().unwrap();
    unsafe {
        if let (Some(w), Some(picker), Some(content)) =
            (win, *PICKER_WINDOW.lock().unwrap(), content)
        {
            // If the cursor still sits on the text as the panel hides, restore the arrow
            // explicitly (cursor regions re-evaluate only on mouse movement); skip when
            // the cursor is elsewhere so the search field's own I-beam is never clobbered.
            let loc: NSPoint = msg_send![class!(NSEvent), mouseLocation];
            let f: NSRect = msg_send![w.0, frame];
            if f.origin.x <= loc.x
                && loc.x <= f.origin.x + f.size.width
                && f.origin.y <= loc.y
                && loc.y <= f.origin.y + f.size.height
            {
                let arrow: *mut AnyObject = msg_send![class!(NSCursor), arrowCursor];
                let _: () = msg_send![arrow, set];
            }
            let current_picker: NSRect = msg_send![picker.0, frame];
            let restored_picker =
                original_origin.map(|origin| NSRect::new(origin, current_picker.size));
            cancel_detail_close(w.0);
            animate_detail_close(picker.0, w.0, content.0, restored_picker);
        } else if let Some(w) = win {
            cancel_detail_close(w.0);
            let _: () = msg_send![w.0, orderOut: std::ptr::null::<AnyObject>()];
        }
    }
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
    let h = (screen_frame.size.height - DETAIL_SCREEN_MARGIN * 2.0).max(DETAIL_PANEL_MIN_H);
    let x = (screen_frame.size.width - w) / 2.0 + screen_frame.origin.x;
    let y = (screen_frame.size.height - h) / 2.0 + screen_frame.origin.y;
    let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));

    // Same as the picker: NSWindowStyleMaskNonactivatingPanel (1<<7), no app activation.
    let style: u64 = 1 << 7;

    let window_cls = {
        let name = CString::new("OhMyTabClipDetailWindow").unwrap();
        let superclass = class!(NSPanel) as *const _ as *mut AnyObject;
        let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
        let types_bool = CString::new("B@:").unwrap();
        class_addMethod(
            cls,
            sel!(canBecomeKeyWindow),
            detail_window_can_not_become_key as *mut c_void,
            types_bool.as_ptr(),
        );
        let types_finish = CString::new("v@:@").unwrap();
        class_addMethod(
            cls,
            sel!(finishDetailClose:),
            detail_finish_close as *mut c_void,
            types_finish.as_ptr(),
        );
        objc_registerClassPair(cls);
        cls
    };
    let window: *mut AnyObject = msg_send![window_cls, alloc];
    let window: *mut AnyObject = msg_send![window, initWithContentRect: frame, styleMask: style, backing: 2u64, defer: false];
    apply_panel_appearance(window);
    let _: () = msg_send![window, setLevel: 3u64];
    let _: () = msg_send![window, setOpaque: false];
    let _: () = msg_send![window, setReleasedWhenClosed: false];
    let clear: *mut AnyObject = msg_send![class!(NSColor), clearColor];
    let _: () = msg_send![window, setBackgroundColor: clear];
    let _: () = msg_send![window, setHasShadow: false];

    // Glass backdrop (Liquid Glass), same as the picker.
    let is_macos_26 = AnyClass::get(c"NSGlassEffectView").is_some();

    let content_parent: *mut AnyObject;
    if is_macos_26 {
        let glass_cls = AnyClass::get(c"NSGlassEffectView").unwrap();
        let glass: *mut AnyObject = msg_send![glass_cls, alloc];
        let glass: *mut AnyObject =
            msg_send![glass, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h))];
        // NSGlassEffectView's corner radius participates in the glass-material rendering, so
        // it must match the picker. Its own layer hard-clips afterward to prevent blur leaks.
        let _: () = msg_send![glass, setCornerRadius: CORNER_R];
        let style_i: i64 = match crate::config::effective_glass_style().as_str() {
            "clear" => 1,
            _ => 0,
        };
        let _: () = msg_send![glass, setStyle: style_i];
        let tint_hex = crate::config::parse_hex8(&crate::config::effective_glass_tint());
        let tint = crate::ffi::hex_to_ns_color(tint_hex);
        let _: () = msg_send![glass, setTintColor: tint];
        let _: () = msg_send![glass, setAutoresizingMask: 18u64];
        // Use the picker's contentView hierarchy directly. An extra clip container changes
        // Liquid Glass compositing and makes it resemble a selected row's darker backdrop.
        let _: () = msg_send![window, setContentView: glass];
        let inner: *mut AnyObject = msg_send![class!(NSView), alloc];
        let inner: *mut AnyObject =
            msg_send![inner, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h))];
        let _: () = msg_send![inner, setAutoresizingMask: 18u64];
        // The detail panel cannot become key, so the system darkens its Glass. Reuse the same
        // configured tint with a higher alpha as base-surface compensation, never the selected
        // row's dark tile.
        let fill: *mut AnyObject = msg_send![class!(NSView), alloc];
        let fill: *mut AnyObject = msg_send![
            fill,
            initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h))
        ];
        let _: () = msg_send![fill, setWantsLayer: true];
        let fill_layer: *mut AnyObject = msg_send![fill, layer];
        let compensation_hex = (tint_hex & 0xFFFF_FF00) | DETAIL_INACTIVE_GLASS_COMPENSATION_A;
        crate::ffi::layer_set_background(fill_layer, crate::ffi::hex_to_cg_color(compensation_hex));
        *DETAIL_GLASS_FILL_LAYER.lock().unwrap() = Some(ObjPtr::new(fill_layer));
        let _: () = msg_send![fill, setAutoresizingMask: 18u64];
        let _: () = msg_send![inner, addSubview: fill];
        release_obj(fill);
        let _: () = msg_send![glass, setContentView: inner];
        // Same hard clipping as the picker: the glass material owns the corner while the
        // layer only prevents blur from leaking beyond it.
        let _: () = msg_send![glass, setWantsLayer: true];
        let glass_layer: *mut AnyObject = msg_send![glass, layer];
        if !glass_layer.is_null() {
            let _: () = msg_send![glass_layer, setCornerRadius: CORNER_R];
            let _: () = msg_send![glass_layer, setMasksToBounds: true];
        }
        *DETAIL_GLASS.lock().unwrap() = Some(ObjPtr::new(glass));
        release_obj(glass);
        content_parent = inner;
    } else {
        let ve: *mut AnyObject = msg_send![class!(NSVisualEffectView), alloc];
        let ve: *mut AnyObject =
            msg_send![ve, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h))];
        let _: () = msg_send![ve, setBlendingMode: 1u64]; // WithinWindow
        let _: () = msg_send![ve, setMaterial: 12u64]; // Dark
        let _: () = msg_send![ve, setState: 1u64]; // Active
        let _: () = msg_send![ve, setAutoresizingMask: 18u64];
        let content: *mut AnyObject = msg_send![window, contentView];
        let _: () = msg_send![content, addSubview: ve];
        content_parent = ve;
    }

    // The content container is flipped and top-aligned. Detail text is selectable, and an
    // ordinary image click must not dismiss the panel either; Esc/←/→ or list actions close it.
    let content = {
        let name = CString::new("OhMyTabClipDetailContent").unwrap();
        let superclass = class!(NSView) as *const _ as *mut AnyObject;
        let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
        let types_v = CString::new("v@:@").unwrap();
        class_addMethod(
            cls,
            sel!(mouseDown:),
            detail_content_mouse_down as *mut c_void,
            types_v.as_ptr(),
        );
        let types_bool = CString::new("B@:").unwrap();
        class_addMethod(
            cls,
            sel!(isFlipped),
            detail_content_is_flipped as *mut c_void,
            types_bool.as_ptr(),
        );
        objc_registerClassPair(cls);
        let content: *mut AnyObject = msg_send![cls, alloc];
        let content: *mut AnyObject = msg_send![
            content,
            initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h))
        ];
        let _: () = msg_send![content, setWantsLayer: true];
        let _: () = msg_send![content, setAutoresizingMask: 18u64];
        let _: () = msg_send![content_parent, addSubview: content];
        release_obj(content);
        content
    };

    *DETAIL_CONTENT.lock().unwrap() = Some(ObjPtr::new(content));
    *DETAIL_WINDOW.lock().unwrap() = Some(ObjPtr::new(window));
}

/// The detail panel never becomes key (keyboard focus stays in the picker's container).
extern "C" fn detail_window_can_not_become_key(_self: *mut c_void, _cmd: Sel) -> bool {
    false
}

/// The content container is flipped.
extern "C" fn detail_content_is_flipped(_self: *mut c_void, _cmd: Sel) -> bool {
    true
}

/// Consume ordinary clicks on the detail container. NSTextView already consumes clicks for
/// selection, whereas image clicks reach the container; both content kinds must behave alike
/// and never accidentally dismiss the detail panel.
extern "C" fn detail_content_mouse_down(_self: *mut c_void, _cmd: Sel, _event: *mut c_void) {}

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
    let job_hash = match *DETAIL_PENDING_HD.lock().unwrap() {
        Some((h, _)) => h,
        None => return,
    };
    if !detail_result_still_wanted(detail_visible(), detail_current_hash(), job_hash) {
        // Stale: release the slot bytes (the disk cache is written; later opens do not
        // need the slot).
        *DETAIL_PENDING_HD.lock().unwrap() = None;
        return;
    }
    unsafe { show_detail_for_sel() };
}

unsafe fn add_detail_separator(content: *mut AnyObject, y: f64, width: f64) {
    let line: *mut AnyObject = msg_send![class!(NSView), alloc];
    let line: *mut AnyObject = msg_send![
        line,
        initWithFrame: NSRect::new(NSPoint::new(0.0, y), NSSize::new(width, 1.0))
    ];
    let _: () = msg_send![line, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![line, layer];
    crate::ffi::layer_set_background(layer, crate::ffi::hex_to_cg_color(0x0000000B));
    let _: () = msg_send![content, addSubview: line];
    release_obj(line);
}

unsafe fn add_detail_wrap_control(content: *mut AnyObject, width: f64) {
    let enabled = DETAIL_SOFT_WRAP_ENABLED.load(Ordering::SeqCst);
    let share_x = width - 42.0;
    // Reuse the existing switch during detail refreshes and create it only once, so rebuilding
    // the detail body cannot interrupt its animation.
    let button = detail_wrap_button(content);
    let newly_created = button.is_null();
    let button = if newly_created {
        crate::settings::make_shared_switch(
            share_x - 8.0,
            0.0,
            DETAIL_TOOLBAR_H,
            enabled,
            observer(),
            sel!(toggleDetailSoftWrap:),
        )
    } else {
        button
    };
    let button_frame: NSRect = msg_send![button, frame];
    let x = button_frame.origin.x;
    let tooltip_key = if enabled {
        "clipboard.detail_soft_wrap_on"
    } else {
        "clipboard.detail_soft_wrap_off"
    };
    let tooltip = make_nsstring(&t(tooltip_key));
    let _: () = msg_send![button, setToolTip: tooltip];
    CFRelease(tooltip as *const c_void);
    if newly_created {
        let _: () = msg_send![content, addSubview: button];
        release_obj(button);
    }

    // Label sits left of the switch, right-aligned; alpha raised to 0.5 so the
    // label-to-switch association reads clearly.
    const LABEL_H: f64 = 16.0;
    let label_y = (DETAIL_TOOLBAR_H - LABEL_H) / 2.0;
    let label_ns = make_nsstring(&t("clipboard.detail_soft_wrap"));
    let label: *mut AnyObject = msg_send![class!(NSTextField), labelWithString: label_ns];
    CFRelease(label_ns as *const c_void);
    let _: () = msg_send![label, setFrame: NSRect::new(
        NSPoint::new(x - 6.0 - 70.0, label_y),
        NSSize::new(70.0, LABEL_H)
    )];
    let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 12.0f64];
    let color: *mut AnyObject = msg_send![class!(NSColor), colorWithWhite: 0.0f64, alpha: 0.5f64];
    let _: () = msg_send![label, setFont: font];
    let _: () = msg_send![label, setTextColor: color];
    let _: () = msg_send![label, setAlignment: 2isize]; // NSTextAlignmentRight
    release_obj(font);
    release_obj(color);
    let _: () = msg_send![content, addSubview: label];
}

/// Draw the save-as icon (download into a tray): a downward arrow plus a bottom tray,
/// stroked with the same parameters as the other toolbar buttons -- no SF Symbol variant.
pub(super) unsafe fn make_detail_save_icon(alpha: f64) -> *mut AnyObject {
    let image: *mut AnyObject = msg_send![class!(NSImage), alloc];
    let image: *mut AnyObject = msg_send![image, initWithSize: NSSize::new(18.0, 18.0)];
    let color: *mut AnyObject = msg_send![class!(NSColor), colorWithWhite: 0.0f64, alpha: alpha];
    let _: () = msg_send![image, lockFocus];
    let _: () = msg_send![color, set];

    // AppKit's origin is bottom-left: visual "down" means smaller y. The tray hugs the
    // bottom with its opening up; the arrow points into it.
    let tray: *mut AnyObject = msg_send![class!(NSBezierPath), bezierPath];
    let _: () = msg_send![tray, moveToPoint: NSPoint::new(4.5, 7.5)];
    let _: () = msg_send![tray, lineToPoint: NSPoint::new(4.5, 4.5)];
    let _: () = msg_send![tray, lineToPoint: NSPoint::new(13.5, 4.5)];
    let _: () = msg_send![tray, lineToPoint: NSPoint::new(13.5, 7.5)];

    let stem: *mut AnyObject = msg_send![class!(NSBezierPath), bezierPath];
    let _: () = msg_send![stem, moveToPoint: NSPoint::new(9.0, 14.5)];
    let _: () = msg_send![stem, lineToPoint: NSPoint::new(9.0, 7.5)];

    let head: *mut AnyObject = msg_send![class!(NSBezierPath), bezierPath];
    let _: () = msg_send![head, moveToPoint: NSPoint::new(6.2, 10.2)];
    let _: () = msg_send![head, lineToPoint: NSPoint::new(9.0, 7.4)];
    let _: () = msg_send![head, lineToPoint: NSPoint::new(11.8, 10.2)];

    for path in [tray, stem, head] {
        let _: () = msg_send![path, setLineWidth: 1.45f64];
        let _: () = msg_send![path, setLineCapStyle: 1isize]; // NSLineCapStyleRound
        let _: () = msg_send![path, setLineJoinStyle: 1isize]; // NSLineJoinStyleRound
        let _: () = msg_send![path, stroke];
    }
    let _: () = msg_send![image, unlockFocus];
    let _: () = msg_send![image, setTemplate: false];
    image
}

/// Reserve the share entry point: its placeholder action performs no business behavior while the
/// button retains the mockup's hover feedback.
/// Save-as button: the tooltip reads "save as text file" for text entries and "save as
/// image file" for image entries.
unsafe fn add_detail_save_as_button(content: *mut AnyObject, width: f64, is_image: bool) {
    let button: *mut AnyObject = msg_send![hover_button_class(), alloc];
    // The y offset derives from the toolbar height for vertical centering (same logic as
    // the soft-wrap switch) instead of a hardcoded value that drifts on resize.
    let save_y = (DETAIL_TOOLBAR_H - 28.0) / 2.0;
    let button: *mut AnyObject = msg_send![
        button,
        initWithFrame: NSRect::new(
            NSPoint::new(width - 42.0, save_y),
            NSSize::new(28.0, 28.0)
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
    let _: () = msg_send![layer, setCornerRadius: 6.0f64];
    let icon = make_detail_save_icon(0.34);
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
}

pub(super) unsafe fn detail_wrap_button(content: *mut AnyObject) -> *mut AnyObject {
    let subviews: *mut AnyObject = msg_send![content, subviews];
    let count: usize = msg_send![subviews, count];
    for index in 0..count {
        let view: *mut AnyObject = msg_send![subviews, objectAtIndex: index as isize];
        let is_button: bool = msg_send![view, isKindOfClass: class!(NSButton)];
        if !is_button {
            continue;
        }
        // The wrap control reuses the settings page's HTML switch; locate it by action so
        // this remains valid when the detail panel is rebuilt.
        let action: Sel = msg_send![view, action];
        if action == sel!(toggleDetailSoftWrap:) {
            return view;
        }
    }
    std::ptr::null_mut()
}

pub(super) unsafe fn detail_save_as_button(content: *mut AnyObject) -> *mut AnyObject {
    let subviews: *mut AnyObject = msg_send![content, subviews];
    let count: usize = msg_send![subviews, count];
    for index in 0..count {
        let view: *mut AnyObject = msg_send![subviews, objectAtIndex: index as isize];
        let is_button: bool = msg_send![view, isKindOfClass: class!(NSButton)];
        if is_button {
            let action: Sel = msg_send![view, action];
            if action == sel!(detailSaveAs:) {
                return view;
            }
        }
    }
    std::ptr::null_mut()
}

/// Add the fixed toolbar and source/statistics footer. Without language detection, the toolbar's
/// leading side intentionally remains empty.
unsafe fn add_detail_chrome(
    content: *mut AnyObject,
    entry: &ClipEntry,
    kind: TextKind,
    width: f64,
    height: f64,
) {
    add_detail_separator(content, DETAIL_TOOLBAR_H - 1.0, width);
    add_detail_separator(content, height - DETAIL_FOOTER_H, width);

    if kind == TextKind::Code {
        add_detail_wrap_control(content, width);
    }
    // The save-as tooltip depends on the entry type: text saves as a text file, images as an image file.
    add_detail_save_as_button(content, width, entry.image.is_some());

    let source_attr = make_meta_footer_attributed(entry, true);
    let source: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let source: *mut AnyObject = msg_send![source, initWithFrame: NSRect::new(
        NSPoint::new(15.0, height - DETAIL_FOOTER_H + 12.0),
        NSSize::new(width * 0.55, 18.0)
    )];
    let _: () = msg_send![source, setBezeled: false];
    let _: () = msg_send![source, setEditable: false];
    let _: () = msg_send![source, setSelectable: false];
    let _: () = msg_send![source, setDrawsBackground: false];
    let _: () = msg_send![source, setAttributedStringValue: source_attr];
    release_obj(source_attr);
    let _: () = msg_send![content, addSubview: source];
    release_obj(source);

    if entry.image.is_none() {
        let line_count = entry.text.split('\n').count().max(1);
        let char_count = entry.text.chars().count();
        let lines = t_count("clipboard.detail_lines", line_count);
        let chars = t_count("clipboard.detail_chars", char_count);
        let stats_text = format!("{lines}  ·  {chars}");
        let stats_ns = make_nsstring(&stats_text);
        let stats: *mut AnyObject = msg_send![
            class!(NSTextField),
            labelWithString: stats_ns
        ];
        CFRelease(stats_ns as *const c_void);
        let _: () = msg_send![stats, setFrame: NSRect::new(
            NSPoint::new(width - 250.0, height - DETAIL_FOOTER_H + 12.0),
            NSSize::new(235.0, 18.0)
        )];
        let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 12.0f64];
        let color: *mut AnyObject =
            msg_send![class!(NSColor), colorWithWhite: 0.0f64, alpha: 0.30f64];
        let _: () = msg_send![stats, setFont: font];
        let _: () = msg_send![stats, setTextColor: color];
        let _: () = msg_send![stats, setAlignment: 2isize]; // NSTextAlignmentRight
        let _: () = msg_send![content, addSubview: stats];
    }
}

/// Open/refresh the detail panel: content follows the selected entry -- full untruncated
/// text for text entries; the large detail preview (lazy `.detail`, see ensure_detail_preview)
/// for images. It has a fixed width to the right of the picker, with its height and vertical
/// position both contained within the picker.
pub(super) unsafe fn show_detail_for_sel() {
    // No-op without a selection (search-field focus) or an empty list.
    let sel = picker_selection();
    if sel == NO_SELECTION {
        return;
    }
    let Some(h_idx) = mapped_index(sel) else {
        return;
    };
    let entry = {
        let hist = CLIP_HISTORY.lock().unwrap();
        hist.get(h_idx).cloned()
    };
    let Some(entry) = entry else {
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
    let screen_frame = picker_screen_frame(picker_win);
    let picker_frame: NSRect = msg_send![picker_win, frame];
    let max_detail_h = detail_max_height(picker_frame);
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

    // Build the content: every branch uses the fixed width, computes only its dynamic height,
    // fills the container, and falls through to the shared positioning code below.
    let (w, h): (f64, f64);
    if let Some(img) = &entry.image {
        // Image entry: the detail preview, fit proportionally into the max box.
        if let Some(png) = ensure_detail_preview(img) {
            let data: *mut AnyObject = msg_send![
                class!(NSData),
                dataWithBytes: png.as_ptr() as *const c_void,
                length: png.len()
            ];
            let image: *mut AnyObject = msg_send![class!(NSImage), alloc];
            let image: *mut AnyObject = msg_send![image, initWithData: data];
            if image.is_null() {
                return;
            }
            let img_size: NSSize = msg_send![image, size];
            let (iw, ih) = (img_size.width, img_size.height);
            // Keep the detail inside the picker height: the image's inner height is the
            // picker's height minus the detail's vertical padding.
            let max_image_h = (max_detail_h - DETAIL_CHROME_H - DETAIL_PAD * 2.0).max(0.0);
            let fit_scale = (DETAIL_IMAGE_MAX_W / iw).min(max_image_h / ih).min(1.0);
            let (fit_w, fit_h) = if iw > 0.0 && ih > 0.0 {
                (iw * fit_scale, ih * fit_scale)
            } else {
                (DETAIL_IMAGE_MAX_W, max_image_h)
            };
            // Keep the outer panel fixed-width; scale the image inside its usable area.
            w = DETAIL_MAX_W;
            h = (fit_h + DETAIL_PAD * 2.0 + DETAIL_CHROME_H)
                .clamp(DETAIL_PANEL_MIN_H, max_detail_h);
            let view: *mut AnyObject = msg_send![class!(NSImageView), alloc];
            let view: *mut AnyObject = msg_send![
                view,
                initWithFrame: NSRect::new(
                    NSPoint::new(DETAIL_PAD, DETAIL_TOOLBAR_H + DETAIL_PAD),
                    NSSize::new(fit_w, fit_h)
                )
            ];
            let _: () = msg_send![view, setImage: image];
            let _: () = msg_send![view, setImageScaling: 3u64]; // NSImageScaleProportionallyUpOrDown
            let _: () = msg_send![view, setEditable: false];
            let _: () = msg_send![content, addSubview: view];
            release_obj(view);
            release_obj(image);
        } else {
            // Degenerate entry (no preview, no source file): the detail falls back to the
            // filename text (same fallback as the row body).
            let (tw, th) = detail_text_size(&entry.text, TextKind::Plain, max_detail_h);
            add_detail_text(content, &entry.text, tw, th, TextKind::Plain, None, false);
            w = tw;
            h = th;
        }
    } else {
        // Text entry: the full untruncated text; scrolls inside detail beyond picker height.
        let kind = classify_text(&entry.text);
        // Code-detail sizing and content share one Arc<PreparedCodeDisplay>; cache hits only
        // increment the reference count instead of copying long text or its source map.
        let code_soft_wrap =
            kind == TextKind::Code && DETAIL_SOFT_WRAP_ENABLED.load(Ordering::SeqCst);
        // Both code-detail modes go through the preparation pipeline: soft wrap breaks at structural points
        // and marks midpoints; no wrap stays on one line (horizontal scrolling) with the same markers plus a
        // source map so copying stays lossless.
        let prepared_code = if kind == TextKind::Code {
            Some(if code_soft_wrap {
                prepare_code_for_soft_wrap(&entry.text, detail_code_max_columns(DETAIL_CODE_MAX_W))
            } else {
                prepare_code_no_wrap_display(&entry.text)
            })
        } else {
            None
        };
        let (tw, th) = if let Some(prepared) = &prepared_code {
            detail_prepared_code_size(prepared, max_detail_h)
        } else if kind == TextKind::Code {
            detail_unwrapped_code_size(&entry.text, max_detail_h)
        } else {
            detail_text_size(&entry.text, kind, max_detail_h)
        };
        add_detail_text(
            content,
            &entry.text,
            tw,
            th,
            kind,
            prepared_code.as_deref(),
            code_soft_wrap,
        );
        w = tw;
        h = th;
    }
    let chrome_kind = if entry.image.is_some() {
        TextKind::Plain
    } else {
        classify_text(&entry.text)
    };
    add_detail_chrome(content, &entry, chrome_kind, w, h);

    // Position: right of the picker, top-aligned with the SELECTED ROW / flip / clamp.
    // Row alignment instead of the window top: the top strip holds the search/clear bar
    // and with few entries the window is floored at the min height, so a window-top
    // alignment floats the panel above the row (user-reported misalignment).
    let Some(align_top_y) = selected_row_screen_y(picker_frame) else {
        return;
    };
    if !detail_was_visible {
        *DETAIL_PICKER_ORIGINAL_ORIGIN.lock().unwrap() = Some(picker_frame.origin);
    }
    // Lay out the picker + detail as one group, keeping detail on the right and aligned to
    // the selected row.
    let center_on_main = CONFIG.read().unwrap().clipboard.picker_position == "main";
    let cursor: NSPoint = msg_send![class!(NSEvent), mouseLocation];
    let (target_picker_frame, target_detail_frame) = detail_group_frames(
        picker_frame,
        align_top_y,
        screen_frame,
        w,
        h,
        center_on_main,
        cursor.x,
    );
    if detail_was_visible {
        let _: () = msg_send![picker_win, setFrame: target_picker_frame, display: true];
        let _: () = msg_send![window, setFrame: target_detail_frame, display: true];
        let _: () = msg_send![window, orderFrontRegardless];
    } else {
        animate_detail_open(
            picker_win,
            window,
            content,
            target_picker_frame,
            target_detail_frame,
        );
    }
    log_debug!(
        "[clip] detail group: picker=({:.0},{:.0}) detail=({:.0},{:.0}) {}x{}",
        target_picker_frame.origin.x,
        target_picker_frame.origin.y,
        target_detail_frame.origin.x,
        target_detail_frame.origin.y,
        target_detail_frame.size.width,
        target_detail_frame.size.height
    );
    // orderFrontRegardless: never takes key (canBecomeKeyWindow=NO keeps the picker key).
    set_detail_visible(true);
    // Once full document layout and the final window frame are both applied, unconditionally
    // set AppKit's actual constrained top. Mouse detail clicks, keyboard Right, and Up/Down
    // navigation while detail is open all converge here and therefore behave identically.
    let detail_scroll = *DETAIL_SCROLL_VIEW.lock().unwrap();
    if let Some(scroll) = detail_scroll {
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

/// The detail-height cap equals the picker height, keeping both detail edges inside it.
pub(super) fn detail_max_height(picker: NSRect) -> f64 {
    picker.size.height.max(DETAIL_PANEL_MIN_H)
}

/// Map the detail code width to soft-wrap sizing columns; no characters are inserted into
/// the display. The budget uses the REAL container width (the scroll view extends to the
/// panel's right edge, so container = panel width - left padding) and the measured
/// `CODE_ADVANCE_PT`; SAFETY absorbs rounding and small font-metric drift.
fn detail_code_max_columns(width: f64) -> usize {
    let container = width - DETAIL_PAD;
    let columns = (container / CODE_ADVANCE_PT).floor().max(0.0) as usize;
    columns.saturating_sub(DETAIL_CODE_WRAP_SAFETY).max(24)
}

/// Size the panel from the prepared soft-wrap model. U+2028 already represents final visual
/// wraps, so line widths are not scanned again.
fn detail_prepared_code_size(prepared: &PreparedCodeDisplay, max_height: f64) -> (f64, f64) {
    let lines = prepared
        .text
        .chars()
        .filter(|ch| matches!(ch, '\n' | '\u{2028}'))
        .count()
        + 1;
    let h =
        (lines as f64 * DETAIL_LINE_H + DETAIL_PAD * 2.0 + DETAIL_TEXT_INSET_H + DETAIL_CHROME_H)
            .clamp(DETAIL_PANEL_MIN_H, max_height);
    (DETAIL_CODE_MAX_W, h)
}

/// Size unwrapped code from hard lines only; native horizontal scrolling handles excess width.
pub(super) fn detail_unwrapped_code_size(text: &str, max_height: f64) -> (f64, f64) {
    let lines = text.split('\n').count().max(1);
    let h =
        (lines as f64 * DETAIL_LINE_H + DETAIL_PAD * 2.0 + DETAIL_TEXT_INSET_H + DETAIL_CHROME_H)
            .clamp(DETAIL_PANEL_MIN_H, max_height);
    (DETAIL_CODE_MAX_W, h)
}

/// Compute detail text-panel dimensions (type-specific width, height capped by the picker).
pub(super) fn detail_text_size(text: &str, kind: TextKind, max_height: f64) -> (f64, f64) {
    if kind == TextKind::Code {
        let prepared = prepare_code_for_soft_wrap(text, detail_code_max_columns(DETAIL_CODE_MAX_W));
        return detail_prepared_code_size(&prepared, max_height);
    }
    let w = DETAIL_MAX_W;
    let avail_w = w - DETAIL_PAD * 2.0;
    let lines = estimate_lines(text, detail_text_units(avail_w));
    let h =
        (lines as f64 * DETAIL_LINE_H + DETAIL_PAD * 2.0 + DETAIL_TEXT_INSET_H + DETAIL_CHROME_H)
            .clamp(DETAIL_PANEL_MIN_H, max_height);
    (w, h)
}

/// Build the detail text view: plain text wraps naturally; code inserts prioritized U+2028 soft
/// wraps, with markers remaining drawing-only decorations. Text remains mouse-selectable; copying
/// uses the native context menu or Cmd+C forwarded by the picker because the detail never becomes key.
pub(super) fn detail_text_view_class() -> *mut AnyObject {
    static CLASS: OnceLock<usize> = OnceLock::new();
    *CLASS.get_or_init(|| unsafe {
        let name = CString::new("OhMyTabClipDetailTextView").unwrap();
        let superclass = class!(NSTextView) as *const _ as *mut AnyObject;
        let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
        let types = CString::new("v@:@").unwrap();
        class_addMethod(
            cls,
            sel!(copy:),
            detail_text_view_copy as *mut c_void,
            types.as_ptr(),
        );
        let types_draw = CString::new("v@:{CGRect={CGPoint=dd}{CGSize=dd}}").unwrap();
        class_addMethod(
            cls,
            sel!(drawRect:),
            detail_text_view_draw_rect as *mut c_void,
            types_draw.as_ptr(),
        );
        objc_registerClassPair(cls);
        cls as usize
    }) as *mut AnyObject
}

/// The native Copy menu must also pass through the source mapping, never copying formatted text.
extern "C" fn detail_text_view_copy(_self: *mut c_void, _cmd: Sel, _sender: *mut AnyObject) {
    copy_detail_selection();
}

struct SoftWrapGlyphs {
    end: CallbackTarget,
    continuation: CallbackTarget,
    end_size: NSSize,
    continuation_size: NSSize,
}

/// Create the soft-wrap glyphs once; drawRect can run frequently, so it must not copy large text
/// or rebuild attributed strings on every repaint.
unsafe fn soft_wrap_glyphs() -> &'static SoftWrapGlyphs {
    static GLYPHS: OnceLock<SoftWrapGlyphs> = OnceLock::new();
    GLYPHS.get_or_init(|| {
        let attrs: *mut AnyObject = msg_send![class!(NSMutableDictionary), alloc];
        let attrs: *mut AnyObject = msg_send![attrs, init];
        let font_key = make_nsstring("NSFont");
        let color_key = make_nsstring("NSColor");
        let font: *mut AnyObject =
            msg_send![class!(NSFont), monospacedSystemFontOfSize: 10.0f64, weight: 0.0f64];
        let color: *mut AnyObject =
            msg_send![class!(NSColor), colorWithWhite: 0.0f64, alpha: 0.36f64];
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
    })
}

/// Draw soft-wrap markers after NSTextView finishes, reading layout results without modifying
/// textStorage.
extern "C" fn detail_text_view_draw_rect(_self: *mut c_void, _cmd: Sel, rect: NSRect) {
    unsafe {
        type Draw = unsafe extern "C" fn(*mut ObjcSuper, Sel, NSRect);
        let mut sup = ObjcSuper {
            receiver: _self,
            super_class: class!(NSTextView) as *const _ as *mut c_void,
        };
        let draw: Draw = std::mem::transmute(objc_msgSendSuper as *const ());
        draw(&mut sup, sel!(drawRect:), rect);

        let view = _self as *mut AnyObject;
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
