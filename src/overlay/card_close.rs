//! Card-close pipeline: close button class, close animation/reflow, async AX close, and commit.

use block2::RcBlock;

use super::*;

pub(super) unsafe fn card_views_by_key(
    _windows: &[WindowInfo],
) -> HashMap<WindowKey, *mut AnyObject> {
    let Some(document) = card_document() else {
        return HashMap::new();
    };
    card_views(document)
        .into_iter()
        .filter_map(|card| card_key(card).map(|key| (key, card)))
        .collect()
}

/// In one AppKit animation transaction, collapse the closing card horizontally while moving
/// every surviving card directly into its new slot.
pub(super) unsafe fn animate_card_close_reflow(
    pending: &PendingCardClose,
    views: &HashMap<WindowKey, *mut AnyObject>,
) {
    let _: () = msg_send![class!(NSAnimationContext), beginGrouping];
    let context: *mut AnyObject = msg_send![class!(NSAnimationContext), currentContext];
    let _: () = msg_send![context, setDuration: CARD_CLOSE_ANIMATION_DURATION];
    let timing_name = make_nsstring("easeInEaseOut");
    let timing: *mut AnyObject =
        msg_send![class!(CAMediaTimingFunction), functionWithName: timing_name];
    if !timing.is_null() {
        let _: () = msg_send![context, setTimingFunction: timing];
    }
    CFRelease(timing_name as *const c_void);

    // Use post-commit document coordinates during the animation and animate the containing
    // viewport and document alongside the cards.
    let document_delta = pending.final_document_h - pending.original_document_h;
    if let Some(container) = *CONTAINER.lock().unwrap() {
        let _: () = msg_send![container.0, setAutoresizingMask: 0u64];
        let animator: *mut AnyObject = msg_send![container.0, animator];
        let _: () = msg_send![animator, setFrame: pending.final_container_frame];
        let _: () = msg_send![animator, setBoundsOrigin: pending.final_bounds_origin];
    }
    if let Some(document) = *CARD_DOCUMENT.lock().unwrap() {
        let animator: *mut AnyObject = msg_send![document.0, animator];
        let _: () = msg_send![animator, setFrame: pending.final_document_frame];
    }
    for (&key, &card) in views {
        let Some(original) = pending.original_frames.get(&key) else {
            continue;
        };
        let animator: *mut AnyObject = msg_send![card, animator];
        if key == (pending.pid, pending.cgwid) {
            // Collapse the frame width instead of using transform.scale, so following cards can
            // occupy the released slot without a visual gap.
            let layer: *mut AnyObject = msg_send![card, layer];
            if !layer.is_null() {
                let _: () = msg_send![layer, removeAllAnimations];
                let _: () = msg_send![layer, setMasksToBounds: true];
            }
            let collapsed = NSRect::new(original.origin, NSSize::new(1.0, original.size.height));
            let _: () = msg_send![animator, setFrame: collapsed];
            let _: () = msg_send![animator, setAlphaValue: 0.0f64];
        } else if let Some(final_frame) = pending.final_frames.get(&key) {
            let rebased_frame = NSRect::new(
                NSPoint::new(final_frame.origin.x, final_frame.origin.y + document_delta),
                final_frame.size,
            );
            let _: () = msg_send![animator, setFrame: rebased_frame];
        }
    }
    if let Some(window) = *OVERLAY_WINDOW.lock().unwrap() {
        let animator: *mut AnyObject = msg_send![window.0, animator];
        let _: () = msg_send![animator, setFrame: pending.final_panel_frame, display: true];
    }
    let completion: RcBlock<dyn Fn()> = RcBlock::new(|| {
        // NSAnimationContext completion handlers run on the main thread, so invoke the
        // registered Rust callback directly instead of sending performSelector:withObject:.
        on_card_close_finished(
            std::ptr::null_mut(),
            sel!(handleCardCloseFinished:),
            std::ptr::null_mut(),
        );
    });
    let _: () = msg_send![context, setCompletionHandler: &*completion];
    let _: () = msg_send![class!(NSAnimationContext), endGrouping];
}

/// If AX rejects the close, reverse the same frame animation to restore every card.
pub(super) unsafe fn restore_card_close_reflow(pending: &PendingCardClose) {
    let windows = with_tab_state(|state_opt| {
        state_opt
            .as_ref()
            .map(|state| state.windows.clone())
            .unwrap_or_default()
    });
    let views = card_views_by_key(&windows);
    let _: () = msg_send![class!(NSAnimationContext), beginGrouping];
    let context: *mut AnyObject = msg_send![class!(NSAnimationContext), currentContext];
    let _: () = msg_send![context, setDuration: CARD_CLOSE_ANIMATION_DURATION];
    let timing_name = make_nsstring("easeInEaseOut");
    let timing: *mut AnyObject =
        msg_send![class!(CAMediaTimingFunction), functionWithName: timing_name];
    if !timing.is_null() {
        let _: () = msg_send![context, setTimingFunction: timing];
    }
    CFRelease(timing_name as *const c_void);
    if let Some(window) = *OVERLAY_WINDOW.lock().unwrap() {
        let animator: *mut AnyObject = msg_send![window.0, animator];
        let _: () = msg_send![animator, setFrame: pending.original_panel_frame, display: true];
    }
    if let Some(container) = *CONTAINER.lock().unwrap() {
        let _: () = msg_send![container.0, setAutoresizingMask: 0u64];
        let animator: *mut AnyObject = msg_send![container.0, animator];
        let _: () = msg_send![animator, setFrame: pending.original_container_frame];
        let _: () = msg_send![animator, setBoundsOrigin: pending.original_bounds_origin];
    }
    if let Some(document) = *CARD_DOCUMENT.lock().unwrap() {
        let animator: *mut AnyObject = msg_send![document.0, animator];
        let _: () = msg_send![animator, setFrame: pending.original_document_frame];
    }
    for (&key, &card) in &views {
        if let Some(frame) = pending.original_frames.get(&key) {
            let animator: *mut AnyObject = msg_send![card, animator];
            let _: () = msg_send![animator, setFrame: *frame];
            let _: () = msg_send![animator, setAlphaValue: 1.0f64];
            if key == (pending.pid, pending.cgwid) {
                let layer: *mut AnyObject = msg_send![card, layer];
                if !layer.is_null() {
                    let _: () = msg_send![layer, setMasksToBounds: false];
                }
            }
        }
    }
    if let Some(container) = *CONTAINER.lock().unwrap() {
        let _: () = msg_send![container.0, setAutoresizingMask: 18u64];
    }
    let _: () = msg_send![class!(NSAnimationContext), endGrouping];
    refresh_highlight();
}

/// Action of the card's top-right close button: animate the card first, then close the window.
pub(crate) extern "C" fn on_close_card(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    let card: *mut AnyObject = unsafe { msg_send![sender as *mut AnyObject, superview] };
    if card.is_null() {
        return;
    }
    let Some(idx) = get_card_index(card) else {
        return;
    };
    begin_close_window_at(idx, card);
}

/// Whether a close transition is active; structural refreshes must wait until it commits.
pub(crate) fn card_close_in_progress() -> bool {
    PENDING_CARD_CLOSE.lock().unwrap().is_some()
}

/// Start the slot-collapse/reflow animation; the actual AX close runs on a worker thread.
pub(crate) fn begin_close_window_at(idx: usize, card: *mut AnyObject) {
    if card_close_in_progress() {
        return;
    }
    let Some((pending, views)) = with_tab_state(|state_opt| {
        let state = state_opt.as_ref()?;
        if !state.visible {
            return None;
        }
        let window = state.windows.get(idx)?;
        let key = (window.pid, window.window_id);
        let views = unsafe { card_views_by_key(&state.windows) };
        if views.get(&key).copied() != Some(card) {
            return None;
        }
        let original_frames: HashMap<WindowKey, NSRect> = views
            .values()
            .map(|&view| {
                let frame: NSRect = unsafe { msg_send![view, frame] };
                let key = state
                    .windows
                    .iter()
                    .find_map(|window| {
                        let candidate = (window.pid, window.window_id);
                        (views.get(&candidate).copied() == Some(view)).then_some(candidate)
                    })
                    .unwrap();
                (key, frame)
            })
            .collect();
        let panel_frame = unsafe {
            OVERLAY_WINDOW
                .lock()
                .unwrap()
                .map(|window| msg_send![window.0, frame])
                .unwrap_or(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0)))
        };
        let (original_container_frame, original_document_frame, original_bounds_origin) = unsafe {
            let container = CONTAINER.lock().unwrap().map(|container| container.0);
            let document = CARD_DOCUMENT.lock().unwrap().map(|document| document.0);
            let container_frame = container
                .map(|container| msg_send![container, frame])
                .unwrap_or(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0)));
            let document_frame = document
                .map(|document| msg_send![document, frame])
                .unwrap_or(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0)));
            let bounds_origin = container
                .map(|container| {
                    let bounds: NSRect = msg_send![container, bounds];
                    bounds.origin
                })
                .unwrap_or(NSPoint::new(0.0, 0.0));
            (container_frame, document_frame, bounds_origin)
        };
        let panel_w = panel_frame.size.width;
        let overflowed = THUMB_ROW_RANGES
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|rows| rows.len() > *THUMB_MAX_ROWS.lock().unwrap());
        let scrollbar_w = if overflowed { THUMB_SCROLLBAR_W } else { 0.0 };
        let card_area_w = (panel_w - scrollbar_w).max(1.0);
        let max_inner = (card_area_w - H_PADDING * 2.0).max(1.0);
        let card_h = original_frames
            .get(&key)
            .map(|frame| frame.size.height)
            .unwrap_or(1.0);
        let gap = if crate::theme::thumbnails_enabled() {
            THUMB_ROW_GAP
        } else {
            ICON_CARD_GAP
        };
        let mut widths = Vec::with_capacity(state.windows.len().saturating_sub(1));
        let mut survivor_keys = Vec::with_capacity(state.windows.len().saturating_sub(1));
        for candidate in &state.windows {
            let candidate_key = (candidate.pid, candidate.window_id);
            if candidate_key == key {
                continue;
            }
            let frame = original_frames.get(&candidate_key)?;
            survivor_keys.push(candidate_key);
            widths.push(frame.size.width);
        }
        let document_h = (*THUMB_DOCUMENT_HEIGHT.lock().unwrap()).max(1.0);
        let max_rows = (*THUMB_MAX_ROWS.lock().unwrap()).max(1);
        let content_inset = *THUMB_CONTENT_INSET.lock().unwrap();
        let max_panel_h = *THUMB_PANEL_MAX_H.lock().unwrap();
        let teaser_fits = *THUMB_TEASER_FITS.lock().unwrap();
        let (placements, final_row_ranges, final_panel_w, final_overflowed) =
            plan_thumb_close_reflow(
                &widths,
                card_h,
                max_inner,
                gap,
                document_h,
                content_inset,
                overflowed,
                max_rows,
            );
        // The panel height must come from the same function the normal layout uses, or closing a card
        // makes the panel jump taller (measured 901 > 875).
        // The panel height is only computed in `thumb_close_panel_metrics`, which reuses the normal
        // layout's `thumb_panel_metrics`; building the formula here again is what re-introduced the
        // "panel grows while closing" bug (measured 901 > 875).
        let (final_panel_h, final_content_inset) = thumb_close_panel_metrics(
            final_row_ranges.len(),
            final_overflowed,
            card_h,
            gap,
            teaser_fits,
            max_rows,
            max_panel_h,
        );
        let content_h = thumb_document_height_for_rows(
            final_row_ranges.len(),
            card_h,
            gap,
            final_content_inset,
        );
        let final_viewport_h = (final_panel_h - status_h()).max(1.0);
        let final_document_h = content_h.max(final_viewport_h).max(1.0);
        let old_offset = *THUMB_SCROLL_OFFSET.lock().unwrap();
        let (final_document_h, final_scroll_max_offset, final_scroll_offset, _) =
            rebase_thumb_scroll_after_document_resize(
                document_h,
                final_document_h,
                final_viewport_h,
                old_offset,
            );
        let final_bounds_origin = NSPoint::new(
            original_bounds_origin.x,
            (final_scroll_max_offset - final_scroll_offset)
                .clamp(0.0, final_scroll_max_offset.max(0.0)),
        );
        let final_frames = placements
            .into_iter()
            .filter_map(|placement| {
                let key = *survivor_keys.get(placement.index)?;
                Some((
                    key,
                    NSRect::new(
                        NSPoint::new(placement.x, placement.y),
                        NSSize::new(placement.width, card_h),
                    ),
                ))
            })
            .collect();
        let final_panel_frame = NSRect::new(
            NSPoint::new(
                panel_frame.origin.x + (panel_frame.size.width - final_panel_w) / 2.0,
                panel_frame.origin.y + (panel_frame.size.height - final_panel_h) / 2.0,
            ),
            NSSize::new(final_panel_w, final_panel_h),
        );
        let final_container_frame = NSRect::new(
            NSPoint::new(original_container_frame.origin.x, status_h()),
            NSSize::new(final_panel_w, final_viewport_h),
        );
        let final_document_frame = NSRect::new(
            original_document_frame.origin,
            NSSize::new(final_panel_w, final_document_h),
        );
        Some((
            PendingCardClose {
                pid: window.pid,
                cgwid: window.window_id,
                animation_finished: false,
                ax_result: None,
                original_panel_frame: panel_frame,
                original_container_frame,
                original_document_frame,
                original_bounds_origin,
                original_frames,
                final_frames,
                final_row_ranges,
                final_panel_frame,
                final_container_frame,
                final_document_frame,
                final_bounds_origin,
                final_overflowed,
                original_document_h: document_h,
                final_document_h,
                final_scroll_max_offset,
                final_scroll_offset,
            },
            views,
        ))
    }) else {
        return;
    };
    let close_key = (pending.pid, pending.cgwid);
    {
        let mut closing = PENDING_CARD_CLOSE.lock().unwrap();
        if closing.is_some() {
            return;
        }
        *closing = Some(pending);
    }

    unsafe {
        let pending_ref = PENDING_CARD_CLOSE.lock().unwrap();
        if let Some(pending) = pending_ref.as_ref() {
            animate_card_close_reflow(pending, &views);
        }
        drop(pending_ref);
        start_async_ax_close(close_key);
    }
}

/// Run the AX close off the main thread so visible animation frames never wait on AX queries.
pub(super) fn start_async_ax_close(key: WindowKey) {
    // The settings window belongs to this process. Do not invoke its AX close action from the
    // worker thread: the custom close callback performs AppKit work and must stay on the main
    // thread. The pending card-close animation will consume this successful result normally.
    if key.0 == std::process::id() as i32 {
        crate::settings::close_settings_from_switcher();
        let mut closing = PENDING_CARD_CLOSE.lock().unwrap();
        if let Some(current) = closing
            .as_mut()
            .filter(|current| (current.pid, current.cgwid) == key)
        {
            current.ax_result = Some(true);
        }
        return;
    }
    std::thread::spawn(move || {
        let result = crate::window_collector::close_ax_window(key.0, key.1);
        // Publish only a value result; the main-thread callback validates the key and merges it
        // into the animation state.
        *CARD_CLOSE_AX_RESULT.lock().unwrap() = Some((key, result));
        unsafe {
            let Some(controller) = *crate::CONTROLLER.lock().unwrap() else {
                return;
            };
            let _: () = msg_send![
                controller.0,
                performSelectorOnMainThread: sel!(handleCardCloseAXResult:),
                withObject: std::ptr::null::<AnyObject>(),
                waitUntilDone: false
            ];
        }
    });
}

/// Commit the list update and reflow only after both the animation and AX result are ready.
pub(super) fn finish_pending_card_close() {
    let pending = {
        let mut closing = PENDING_CARD_CLOSE.lock().unwrap();
        let Some(current) = closing.as_ref() else {
            return;
        };
        if !current.animation_finished || current.ax_result.is_none() {
            return;
        }
        closing.take().unwrap()
    };
    if !pending.ax_result.unwrap_or(false) {
        unsafe {
            restore_card_close_reflow(&pending);
        }
        return;
    }
    commit_pending_card_close(pending);
}

/// Exit-animation completion callback; AX may already be done or still be running in the worker.
pub(crate) extern "C" fn on_card_close_finished(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    if let Some(pending) = PENDING_CARD_CLOSE.lock().unwrap().as_mut() {
        pending.animation_finished = true;
    }
    finish_pending_card_close();
}

/// AX worker result callback; joins the animation callback before triggering UI reflow.
pub(crate) extern "C" fn on_card_close_ax_result(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    let Some((key, result)) = CARD_CLOSE_AX_RESULT.lock().unwrap().take() else {
        return;
    };
    if let Some(pending) = PENDING_CARD_CLOSE.lock().unwrap().as_mut() {
        if (pending.pid, pending.cgwid) == key {
            pending.ax_result = Some(result);
        }
    }
    finish_pending_card_close();
}

/// Commit a successful close by removing one view only; surviving views are reused and rebound
/// to their new indices.
pub(super) fn commit_pending_card_close(pending: PendingCardClose) {
    let key = (pending.pid, pending.cgwid);
    let Some((old_windows, new_windows, selected, was_visible, became_empty)) =
        with_tab_state(|state_opt| {
            let state = state_opt.as_mut()?;
            let actual_idx = state
                .windows
                .iter()
                .position(|window| (window.pid, window.window_id) == key)?;
            let old_windows = state.windows.clone();
            let was_visible = state.visible;
            state.windows.remove(actual_idx);
            crate::WINDOW_COUNT.store(state.windows.len(), std::sync::atomic::Ordering::Release);
            state.mru.remove(&key);
            state.selected =
                remove_window_adjust_selection(state.selected, actual_idx, state.windows.len());
            state.selected_target_key = state
                .windows
                .get(state.selected)
                .map(|window| (window.pid, window.window_id));
            let became_empty = state.windows.is_empty();
            if became_empty {
                state.visible = false;
            }
            Some((
                old_windows,
                state.windows.clone(),
                state.selected,
                was_visible,
                became_empty,
            ))
        })
    else {
        return;
    };

    let views = unsafe { card_views_by_key(&old_windows) };
    // Rebase cards and the document together at commit; sharing one delta keeps visible content
    // stationary instead of making the page jump while the scrollbar stays at its old position.
    let document_h = pending.final_document_h;
    let max_offset = pending.final_scroll_max_offset;
    let rebased_offset = pending.final_scroll_offset;
    let document_delta = pending.final_document_h - pending.original_document_h;

    unsafe {
        if let Some(window) = *OVERLAY_WINDOW.lock().unwrap() {
            let _: () = msg_send![window.0, setFrame: pending.final_panel_frame, display: false];
        }
        if let Some(container) = *CONTAINER.lock().unwrap() {
            let _: () = msg_send![
                container.0,
                setFrame: NSRect::new(
                    pending.final_container_frame.origin,
                    pending.final_container_frame.size,
                )
            ];
            let _: () = msg_send![container.0, setBoundsOrigin: pending.final_bounds_origin];
            let _: () = msg_send![container.0, setAutoresizingMask: 18u64];
        }
        if let Some(closing_card) = views.get(&key).copied() {
            remove_card_index(closing_card);
            let _: () = msg_send![closing_card, removeFromSuperview];
        }
        for (index, window) in new_windows.iter().enumerate() {
            let survivor_key = (window.pid, window.window_id);
            if let Some(card) = views.get(&survivor_key).copied() {
                set_card_index(card, index);
                if let Some(frame) = pending.final_frames.get(&survivor_key) {
                    let rebased_frame = NSRect::new(
                        NSPoint::new(frame.origin.x, frame.origin.y + document_delta),
                        frame.size,
                    );
                    let _: () = msg_send![card, setFrame: rebased_frame];
                }
            }
        }

        if let Some(document) = card_document() {
            let _: () = msg_send![document, setFrame: pending.final_document_frame];
        }
        *THUMB_DOCUMENT_HEIGHT.lock().unwrap() = document_h;
    }

    if became_empty {
        if was_visible {
            hide_overlay();
        }
        reset_thumbnail_visible_range();
        reset_thumbnail_scroll();
        reset_thumbnail_nav_anchor();
        return;
    }
    if !was_visible {
        return;
    }

    // Animate the panel resize together with the close transition; atomically sync the document
    // and scroll metadata at commit so the content cannot jump independently of the scrollbar.
    let max_rows = (*THUMB_MAX_ROWS.lock().unwrap()).max(1);
    let row_count = pending.final_row_ranges.len();
    *THUMB_ROW_RANGES.lock().unwrap() = Some(pending.final_row_ranges);
    *THUMB_SCROLL_MAX_OFFSET.lock().unwrap() = max_offset;
    let offset = {
        let mut offset = THUMB_SCROLL_OFFSET.lock().unwrap();
        *offset = rebased_offset;
        *offset
    };
    update_thumbnail_scroll_state(offset);
    unsafe {
        apply_thumbnail_clip_offset();
        if let Some(window) = *OVERLAY_WINDOW.lock().unwrap() {
            let frame: NSRect = msg_send![window.0, frame];
            update_thumbnail_scroller(
                frame.size.width,
                frame.size.height,
                pending.final_overflowed,
                row_count,
                max_rows,
            );
        }
    }
    refresh_highlight();
    update_status_label();
    log_debug!(
        "[overlay] close reflow committed: pid={} cgwid={} remaining={} selected={}",
        pending.pid,
        pending.cgwid,
        new_windows.len(),
        selected
    );
}

pub(crate) extern "C" fn on_cmd_released(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    schedule_cmd_release_diagnostic();
    if card_close_in_progress() {
        return;
    }
    let visible = with_tab_state(|state_opt| {
        let Some(state) = state_opt.as_mut() else {
            return false;
        };
        if !state.visible {
            if state.pending_first_show {
                // The bridge delivered CmdReleased before the AX-backed first frame was ready.
                // Latch it so apply_window_refresh can commit the eventual default target.
                state.pending_first_release = true;
                log_debug!("[overlay] CmdReleased latched while first snapshot is pending");
            }
            return false;
        }
        true
    });
    if !visible {
        return;
    }

    commit_selected_window(true);
}

const CMD_RELEASE_DIAGNOSTIC_DELAY: f64 = 0.03;

/// Sample the modifier state after the release event has cleared the session tap. Only the
/// diagnostic is delayed; committing the selected window remains immediate.
fn schedule_cmd_release_diagnostic() {
    unsafe {
        let Some(controller) = *crate::CONTROLLER.lock().unwrap() else {
            return;
        };
        let _: () = msg_send![
            controller.0,
            performSelector: sel!(handleCmdReleaseDiagnostic:),
            withObject: std::ptr::null::<AnyObject>(),
            afterDelay: CMD_RELEASE_DIAGNOSTIC_DELAY
        ];
    }
}

pub(crate) extern "C" fn on_cmd_release_diagnostic(
    _self: *mut c_void,
    _cmd: Sel,
    _arg: *mut c_void,
) {
    let is_cmd = crate::event_monitor::SHORTCUT_IS_CMD.load(Ordering::SeqCst);
    let live_flags = event_tap::combined_session_flags();
    let live_down = live_flags
        & if is_cmd {
            NSEVENT_MODIFIER_FLAG_COMMAND
        } else {
            NSEVENT_MODIFIER_FLAG_OPTION
        }
        != 0;
    log_debug!(
        "[overlay] post-release modifier state: shortcut={} down={} delay_ms=30",
        if is_cmd { "command" } else { "option" },
        live_down
    );
}

pub(super) fn commit_selected_window(overlay_was_visible: bool) {
    let target = with_tab_state(|state_opt| {
        let state = state_opt.as_mut()?;
        if !state.visible {
            return None;
        }
        let selected = state.selected;
        let w = state.windows.get(selected)?;
        let target = (
            w.pid,
            w.window_id,
            w.minimized,
            w.app_name.clone(),
            selected,
        );
        state.focus_key = Some((w.pid, w.window_id));
        bump_window_mru(&mut state.mru, w.pid, w.window_id);
        state.visible = false;
        Some(target)
    });
    let Some((pid, cgwid, minimized, app_name, selected)) = target else {
        let hidden = with_tab_state(|state_opt| {
            let Some(state) = state_opt.as_mut() else {
                return false;
            };
            if !state.visible {
                return false;
            }
            log_info!(
                "CmdReleased: selected index {} out of bounds (windows={})",
                state.selected,
                state.windows.len()
            );
            state.visible = false;
            true
        });
        if hidden && overlay_was_visible {
            hide_overlay();
        }
        crate::performance::end_switcher_activity();
        return;
    };
    let release_started = Instant::now();
    log_debug!("Switching to '{}' (pid={} cgwid={})", app_name, pid, cgwid);
    // A2 E2E: records the raise target. `selected` and `windows` are still intact here, but
    // visibility was already set to false by the closure above, so this snapshot reports
    // visible: false -- which is why the scripts never assert visibility on a commit frame.
    crate::e2e_state::record_commit(pid, cgwid, &app_name, selected);
    // Vanish first (no orderOut), then activate the target, then delay orderOut.
    // Ordering out first disrupts WindowServer focus routing, leaving the target's
    // first-responder unset (caret stops blinking, etc.). Mirrors BetterCmdTab's
    // vanish() -> activate() -> dismiss() sequence.
    if overlay_was_visible {
        vanish_overlay();
    }
    // No settings-window handling is needed: this nonactivating panel never raises the settings
    // window, which remains in place after switching.
    activate_and_raise(pid, cgwid, minimized);
    log_debug!(
        "[raise] release path complete: pid={} cgwid={} elapsed={}ms",
        pid,
        cgwid,
        release_started.elapsed().as_millis()
    );
    if overlay_was_visible {
        schedule_delayed_order_out();
    }
    log_debug!(
        "commit: pid={} app=\"{}\" cgwid={} selected={}",
        pid,
        app_name,
        cgwid,
        selected
    );
    crate::performance::end_switcher_activity();
}

/// Apply the close button's base or hover tint and background.
pub(super) unsafe fn set_close_button_hover_style(button: *mut AnyObject, hovered: bool) {
    let tint = if hovered {
        // HTML .close:hover: rgba(195, 40, 35, .86)
        hex_to_ns_color(0xC32823DB)
    } else {
        // HTML .close: rgba(0, 0, 0, .30)
        hex_to_ns_color(0x0000004D)
    };
    let _: () = msg_send![button, setContentTintColor: tint];

    let layer: *mut AnyObject = msg_send![button, layer];
    if hovered {
        // HTML .close:hover background: rgba(195, 40, 35, .07)
        layer_set_background(layer, hex_to_cg_color(0xC3282312));
    } else {
        layer_set_background(layer, std::ptr::null_mut());
    }
}

/// Dynamic ObjC subclass for the close button, providing the HTML reference's red hover feedback.
pub(super) fn close_button_class() -> *mut AnyObject {
    static CLOSE_BUTTON_CLASS: OnceLock<StaticClass> = OnceLock::new();
    CLOSE_BUTTON_CLASS
        .get_or_init(|| unsafe {
            let name = CString::new("OhMyTabCloseButton").unwrap();
            let superclass = class!(NSButton) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types_v_obj = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(mouseEntered:),
                close_button_mouse_entered as *mut c_void,
                types_v_obj.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseExited:),
                close_button_mouse_exited as *mut c_void,
                types_v_obj.as_ptr(),
            );
            objc_registerClassPair(cls);
            StaticClass(cls as *const objc2::runtime::AnyClass)
        })
        .0 as *mut AnyObject
}

pub(super) extern "C" fn close_button_mouse_entered(
    _self: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) {
    unsafe {
        set_close_button_hover_style(_self as *mut AnyObject, true);
    }
}

pub(super) extern "C" fn close_button_mouse_exited(
    _self: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) {
    unsafe {
        set_close_button_hover_style(_self as *mut AnyObject, false);
    }
}

pub(crate) extern "C" fn card_mouse_down(_self: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    let Some(idx) = get_card_index(_self as *mut AnyObject) else {
        return;
    };
    let action = with_tab_state(|state_opt| {
        let state = state_opt.as_mut().unwrap();
        if let Some(w) = state.windows.get(idx) {
            let pid = w.pid;
            let cgwid = w.window_id;
            let minimized = w.minimized;
            state.focus_key = Some((pid, cgwid));
            bump_window_mru(&mut state.mru, pid, cgwid);
            state.visible = false;
            Some((pid, cgwid, minimized))
        } else {
            state.visible = false;
            None
        }
    });
    if let Some((pid, cgwid, minimized)) = action {
        vanish_overlay();
        // Same as on_cmd_released: no settings-window handling needed (see comment there);
        // the raise is deferred by one runloop turn so the vanish commits first.
        schedule_deferred_raise(pid, cgwid, minimized);
        schedule_delayed_order_out();
    } else {
        // Unreachable in practice (no cards when the list is empty); defensive dismiss,
        // same as on_cmd_released.
        hide_overlay();
    }
}

pub(crate) extern "C" fn card_mouse_entered(_self: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    // Ignore hover until the user has moved the mouse at least once.
    // Prevents selecting the card under the cursor when the window first opens.
    let Some(idx) = get_card_index(_self as *mut AnyObject) else {
        return;
    };
    if !activates_on_hover() {
        return;
    }
    if !MOUSE_MOVED.load(Ordering::Relaxed) {
        log_debug!(
            "[overlay] card {} mouseEntered (gated, mouse not moved yet)",
            idx
        );
        return;
    }
    let changed = with_tab_state(|state_opt| {
        let state = state_opt.as_mut().unwrap();
        if state.selected != idx {
            state.selected = idx;
            mark_user_picked(state);
            true
        } else {
            false
        }
    });
    if changed {
        reset_thumbnail_nav_anchor();
        refresh_highlight();
        update_status_label();
    }
}
