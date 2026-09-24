//! Summon-time refresh (called at the end of show_overlay).

use super::*;

pub(super) fn capture_range_for_visible(visible: Option<Range<usize>>, len: usize) -> Range<usize> {
    let visible = visible.unwrap_or(0..len);
    visible
        .start
        .min(len)
        .saturating_sub(VISIBLE_PREFETCH_MARGIN)
        ..visible
            .end
            .min(len)
            .saturating_add(VISIBLE_PREFETCH_MARGIN)
            .min(len)
}

/// Submit the selected window at the highest priority before the panel orders in, giving
/// the capture a chance to finish before the first visible frame; show_overlay still
/// performs the complete visible-range refresh after showing.
pub(crate) fn refresh_selected_for_summon(required_px_h: u32) {
    if !crate::theme::thumbnails_enabled() || !capture_allowed() {
        return;
    }
    let Some((pid, wid)) = crate::with_tab_state(|state_opt| {
        let state = state_opt.as_ref()?;
        if !state.visible {
            return None;
        }
        state
            .windows
            .get(state.selected)
            .map(|window| (window.pid, window.window_id))
    }) else {
        return;
    };
    let enqueued = enqueue_job(pid, wid, required_px_h, CapturePriority::Selected);
    log_debug!(
        "[perf] thumbnail summon selected prequeue pid={} wid={} enqueued={} target_h={}",
        pid,
        wid,
        enqueued,
        required_px_h
    );
}

/// Summon-time refresh: for non-minimized windows with valid bounds in the visible
/// slice plus its prefetch margins, request async recaptures for missing frames and
/// stale frontmost-app frames. Existing background frames survive TTL and display-scale
/// changes. enqueue_job coalesces keys already pending/in-flight; the selected window
/// is requested first.
/// The selection is read from TAB_STATE internally; callers just invoke once at
/// the end of show_overlay.
pub(crate) fn refresh_for_summon(required_px_h: u32) {
    if !crate::theme::thumbnails_enabled() {
        return;
    }
    if !capture_allowed() {
        // Unauthorized: skip silently; request permission once if never prompted.
        request_permission_once();
        return;
    }
    // Match the worker's overlay_wants lock order: visible range before TAB_STATE.
    let visible_snapshot = crate::overlay::thumbnail_visible_range();
    let interaction_active = crate::performance::switcher_interaction_active();
    let Some((jobs, missing, frontmost_stale, background_last_good, deferred_prefetch, workset)) =
        crate::with_tab_state(|state_opt| {
            let state = state_opt.as_ref()?;
            if !state.visible {
                return None;
            }
            let selected = state
                .windows
                .get(state.selected)
                .map(|w| (w.pid, w.window_id));
            // is_active marks one representative window only; sibling windows from the
            // same frontmost PID must be eligible for refresh too.
            let frontmost_pid = state.windows.iter().find(|w| w.is_active).map(|w| w.pid);
            let capture_range =
                capture_range_for_visible(visible_snapshot.clone(), state.windows.len());
            let decisions: Vec<(usize, i32, u32, SummonRefreshDecision)> = state
                .windows
                .iter()
                .enumerate()
                .filter(|(index, _)| capture_range.contains(index))
                .filter(|(_, w)| !w.minimized && w.bounds.2 > 0.0 && w.bounds.3 > 0.0)
                .map(|(index, w)| {
                    (
                        index,
                        w.pid,
                        w.window_id,
                        cached_summon_refresh_decision(
                            w.pid,
                            w.window_id,
                            required_px_h,
                            frontmost_pid == Some(w.pid),
                            // The focused window (is_active) ignores the TTL and is
                            // recaptured on every summon; same-PID siblings keep the
                            // TTL rules so multi-window apps do not recapture in bulk.
                            w.is_active,
                        ),
                    )
                })
                .collect();
            let missing = decisions
                .iter()
                .filter(|(_, _, _, decision)| *decision == SummonRefreshDecision::Missing)
                .count();
            let frontmost_stale = decisions
                .iter()
                .filter(|(_, _, _, decision)| *decision == SummonRefreshDecision::FrontmostStale)
                .count();
            let background_last_good = decisions
                .iter()
                .filter(|(_, _, _, decision)| {
                    *decision == SummonRefreshDecision::BackgroundLastGood
                })
                .count();
            let workset = decisions
                .iter()
                .map(|(_, pid, wid, _)| ThumbKey {
                    pid: *pid,
                    wid: *wid,
                })
                .collect::<Vec<_>>();
            let mut deferred_prefetch = 0usize;
            let jobs: Vec<(i32, u32, CapturePriority)> = decisions
                .into_iter()
                .filter(|(_, _, _, decision)| {
                    matches!(
                        decision,
                        SummonRefreshDecision::Missing | SummonRefreshDecision::FrontmostStale
                    )
                })
                .filter_map(|(index, pid, wid, _)| {
                    let priority = if Some((pid, wid)) == selected {
                        CapturePriority::Selected
                    } else if visible_snapshot
                        .as_ref()
                        .is_some_and(|range| range.contains(&index))
                    {
                        CapturePriority::Visible
                    } else {
                        CapturePriority::Prefetch
                    };
                    if interaction_active && priority < CapturePriority::Visible {
                        deferred_prefetch += 1;
                        None
                    } else {
                        Some((pid, wid, priority))
                    }
                })
                .collect();
            Some((
                jobs,
                missing,
                frontmost_stale,
                background_last_good,
                deferred_prefetch,
                workset,
            ))
        })
    else {
        return;
    };
    let requested = jobs.len();
    let (pending_before, in_flight_before, ready_before) = capture_pipeline_stats();
    let (cache_items_before, cache_bytes_before) = cache_stats();
    update_summon_workset(workset.iter().copied());
    if requested > 1 || cache_bytes_before >= CACHE_MAX_COST.saturating_mul(3) / 4 {
        log_debug!(
            "[perf] thumbnail summon workset count={} keys={}",
            workset.len(),
            format_workset(&workset),
        );
    }
    log_debug!(
        "[perf] thumbnail summon start requested={} target_h={} cache_items={} cache_bytes={} pending={} in_flight={} ready={}",
        requested,
        required_px_h,
        cache_items_before,
        cache_bytes_before,
        pending_before,
        in_flight_before,
        ready_before,
    );
    crate::mem::log_debug_snapshot("thumb-summon-before-enqueue");
    let mut enqueued = 0;
    for (pid, wid, priority) in jobs {
        enqueued += usize::from(enqueue_job(pid, wid, required_px_h, priority));
    }
    log_debug!(
        "[perf] thumbnail summon: active={} missing={} frontmost_stale={} background_last_good={} deferred_prefetch={} requested={} enqueued={} target_h={}",
        interaction_active,
        missing,
        frontmost_stale,
        background_last_good,
        deferred_prefetch,
        requested,
        enqueued,
        required_px_h
    );
    log_capture_metrics("summon");
    crate::mem::log_debug_snapshot("thumb-summon-after-enqueue");
}

/// Force a recapture of the current window set after a theme change, bypassing
/// summon-time TTL and frontmost/background freshness decisions.
///
/// The cache stores real window pixels, so changing the system appearance can
/// leave a dark/light surface stale even though the card itself is rebuilt.
/// These jobs carry the appearance-refresh permit: a blank recapture of a
/// suspended WebView still replaces the old frame -- one stale-appearance card
/// amid the new theme looks worse than a temporary placeholder.
pub(crate) fn refresh_for_theme(required_px_h: u32) {
    if !crate::theme::thumbnails_enabled() {
        return;
    }
    if !capture_allowed() {
        request_permission_once();
        return;
    }

    // Snapshot keys before enqueueing: enqueue_job takes CAPTURE_STATE and may
    // wake the worker, so never hold TAB_STATE across the queue operations.
    let (selected, state_keys): (Option<ThumbKey>, Vec<ThumbKey>) =
        crate::with_tab_state(|state_opt| match state_opt.as_ref() {
            Some(state) => {
                let selected = state.windows.get(state.selected).map(|window| ThumbKey {
                    pid: window.pid,
                    wid: window.window_id,
                });
                let keys = state
                    .windows
                    .iter()
                    .filter(|window| {
                        !window.minimized && window.bounds.2 > 0.0 && window.bounds.3 > 0.0
                    })
                    .map(|window| ThumbKey {
                        pid: window.pid,
                        wid: window.window_id,
                    })
                    .collect();
                (selected, keys)
            }
            None => (None, Vec::new()),
        });

    // Include pre-generated/cache-only windows as well. The settings window can change theme
    // before the first switcher summon, when TAB_STATE has no current snapshot yet.
    let keys: Vec<ThumbKey> = {
        let mut keys: HashSet<ThumbKey> = state_keys.into_iter().collect();
        keys.extend(CACHE.lock().unwrap().keys());
        keys.into_iter().collect()
    };

    let target_px_h = required_px_h.max(BASE_TARGET_PX_H);
    let requested = keys.len();
    let (pending_before, in_flight_before, ready_before) = capture_pipeline_stats();
    let (cache_items_before, cache_bytes_before) = cache_stats();
    log_debug!(
        "[perf] thumbnail theme start requested={} target_h={} cache_items={} cache_bytes={} pending={} in_flight={} ready={}",
        requested,
        target_px_h,
        cache_items_before,
        cache_bytes_before,
        pending_before,
        in_flight_before,
        ready_before,
    );
    crate::mem::log_debug_snapshot("thumb-theme-before-enqueue");
    let mut enqueued = 0usize;
    for key in keys {
        let priority = if selected == Some(key) {
            CapturePriority::Selected
        } else {
            // Visible priority deliberately bypasses the interaction gate so
            // every card is refreshed as part of one theme transition.
            CapturePriority::Visible
        };
        // Appearance jobs carry the blank-overwrite permit: even a suspended
        // WebView's blank recapture (new-appearance title bar) must replace the
        // stale-appearance frame, or one light frame lingers among dark cards.
        enqueued += usize::from(enqueue_appearance_job(
            key.pid,
            key.wid,
            target_px_h,
            priority,
        ));
    }
    log_debug!(
        "[thumb] theme refresh: requested={} enqueued={} target_h={}",
        requested,
        enqueued,
        target_px_h
    );
    log_capture_metrics("theme");
    crate::mem::log_debug_snapshot("thumb-theme-after-enqueue");
}

/// Forced recapture after a display reconfiguration (external/built-in switch or
/// resolution change). Cached frames carry the old configuration's window aspect and
/// pixel height: a resolution change rewrites window bounds and a display switch
/// changes the backing scale, so an old frame letterboxes wrongly inside a card laid
/// out for the new aspect. Unlike the theme refresh these jobs use the normal
/// channel: a suspended WebView's blank frame must NOT overwrite the last-known-good
/// image (a real frame with the old aspect beats a correctly-shaped blank), and the
/// geometry guard still drops frames captured mid-animation.
pub(crate) fn refresh_for_display_change(required_px_h: u32) {
    if !crate::theme::thumbnails_enabled() {
        return;
    }
    if !capture_allowed() {
        request_permission_once();
        return;
    }

    // Same key collection as refresh_for_theme: non-minimized windows with bounds
    // from TAB_STATE, unioned with cache-only windows (pre-generated frames), so
    // every known target is covered while the overlay is not summoned.
    let (selected, state_keys): (Option<ThumbKey>, Vec<ThumbKey>) =
        crate::with_tab_state(|state_opt| match state_opt.as_ref() {
            Some(state) => {
                let selected = state.windows.get(state.selected).map(|window| ThumbKey {
                    pid: window.pid,
                    wid: window.window_id,
                });
                let keys = state
                    .windows
                    .iter()
                    .filter(|window| {
                        !window.minimized && window.bounds.2 > 0.0 && window.bounds.3 > 0.0
                    })
                    .map(|window| ThumbKey {
                        pid: window.pid,
                        wid: window.window_id,
                    })
                    .collect();
                (selected, keys)
            }
            None => (None, Vec::new()),
        });
    let keys: Vec<ThumbKey> = {
        let mut keys: HashSet<ThumbKey> = state_keys.into_iter().collect();
        keys.extend(CACHE.lock().unwrap().keys());
        keys.into_iter().collect()
    };

    let target_px_h = required_px_h.max(BASE_TARGET_PX_H);
    let requested = keys.len();
    let (pending_before, in_flight_before, ready_before) = capture_pipeline_stats();
    let (cache_items_before, cache_bytes_before) = cache_stats();
    log_debug!(
        "[perf] thumbnail display-change start requested={} target_h={} cache_items={} cache_bytes={} pending={} in_flight={} ready={}",
        requested,
        target_px_h,
        cache_items_before,
        cache_bytes_before,
        pending_before,
        in_flight_before,
        ready_before,
    );
    crate::mem::log_debug_snapshot("thumb-display-change-before-enqueue");
    let mut enqueued = 0usize;
    for key in keys {
        // Both Selected and Visible priorities bypass the interaction gate, so
        // these jobs are never deferred away.
        let priority = if selected == Some(key) {
            CapturePriority::Selected
        } else {
            CapturePriority::Visible
        };
        enqueued += usize::from(enqueue_job(key.pid, key.wid, target_px_h, priority));
    }
    log_debug!(
        "[thumb] display-change refresh: requested={} enqueued={} target_h={}",
        requested,
        enqueued,
        target_px_h
    );
    log_capture_metrics("display-change");
    crate::mem::log_debug_snapshot("thumb-display-change-after-enqueue");
}
