//! Click-outside cancel (local event monitor + focus-loss dismissal).

use super::*;

/// Register click-outside cancel: the overlay is the key panel, so clicking another app's
/// window hands key to it and the panel fires NSWindowDidResignKeyNotification -> dismiss
/// the overlay without switching (same semantics as Esc).
/// Clicks inside the panel never fire it (the panel keeps key); empty areas of the panel
/// are handled by card events.
///
/// Why not a global mouse monitor: the resign-key notification inherently distinguishes
/// inside/outside clicks (it doesn't fire for our own events), needs no blocks and no
/// hit-testing; the clipboard picker already uses this exact pattern.
pub(crate) fn install_click_to_cancel() {
    unsafe {
        let win = match *OVERLAY_WINDOW.lock().unwrap() {
            Some(w) => w.0,
            None => return,
        };
        let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
        let name = make_nsstring("NSWindowDidResignKeyNotification");
        let _: () = msg_send![
            center,
            addObserver: overlay_observer(),
            selector: sel!(overlayWindowResigned:),
            name: name,
            object: win
        ];
        CFRelease(name as *const c_void);
    }
}

/// Singleton notification observer for the overlay (carries the resign-key callback only).
unsafe fn overlay_observer() -> *mut AnyObject {
    static OBSERVER: OnceLock<CallbackTarget> = OnceLock::new();
    OBSERVER
        .get_or_init(|| {
            let name = CString::new("OhMyTabOverlayObserver").unwrap();
            let superclass = class!(NSObject) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(overlayWindowResigned:),
                overlay_window_resigned as *mut c_void,
                types.as_ptr(),
            );
            objc_registerClassPair(cls);
            let inst: *mut AnyObject = msg_send![cls as *const AnyObject, new];
            CallbackTarget::new(inst)
        })
        .0
}

/// The overlay lost key -> cancel the switch.
extern "C" fn overlay_window_resigned(_self: *mut c_void, _cmd: Sel, _note: *mut c_void) {
    crate::callback_guard::void("overlay_window_resigned", || {
        overlay_window_resigned_inner();
    });
}

fn overlay_window_resigned_inner() {
    // Closing the settings card intentionally hides our settings window and can make the
    // nonactivating overlay resign key as a side effect. The close transition owns that focus
    // change; do not mistake it for a click outside and hide the overlay while it is reflowing.
    if card_close_in_progress() {
        return;
    }
    // Read and update the visibility in one short borrow. The release handler clears `visible`
    // before any AppKit call, so this callback can safely be re-entered by `hide_overlay`.
    let should_hide = with_tab_state(|state_opt| match state_opt.as_mut() {
        Some(state) if state.visible => {
            state.visible = false;
            true
        }
        _ => false,
    });
    if should_hide {
        // Resigning key is not synonymous with an outside click: a system panel, a transient
        // foreground-app window, or AppKit focus reassignment can all displace a nonactivating
        // panel. Capture only the non-sensitive state needed to distinguish those cases.
        unsafe {
            // The helper ends the slot borrow before any AppKit query can re-enter us.
            let window = overlay_window_ptr();
            let pointer_inside = window.is_some_and(|window| {
                let frame: NSRect = msg_send![window, frame];
                let mouse: NSPoint = msg_send![class!(NSEvent), mouseLocation];
                mouse.x >= frame.origin.x
                    && mouse.x <= frame.origin.x + frame.size.width
                    && mouse.y >= frame.origin.y
                    && mouse.y <= frame.origin.y + frame.size.height
            });
            let pressed_mouse_buttons: usize = msg_send![class!(NSEvent), pressedMouseButtons];
            let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let app_active: bool = msg_send![nsapp, isActive];
            let current_event: *mut AnyObject = msg_send![nsapp, currentEvent];
            let current_event_type: Option<usize> = if current_event.is_null() {
                None
            } else {
                Some(msg_send![current_event, type])
            };
            let own_key_window: *mut AnyObject = msg_send![nsapp, keyWindow];
            let own_key = match window {
                Some(window) if own_key_window == window => "overlay",
                _ if own_key_window.is_null() => "none",
                _ => "other",
            };
            let live_flags = event_tap::combined_session_flags();
            let command_down = live_flags & NSEVENT_MODIFIER_FLAG_COMMAND != 0;
            let option_down = live_flags & NSEVENT_MODIFIER_FLAG_OPTION != 0;
            let (frontmost_app, frontmost_pid) = frontmost_app_info();
            log_debug!(
                "[overlay] cancelled after key loss: pointer_inside={} mouse_buttons=0x{:x} current_event_type={:?} command_down={} option_down={} app_active={} own_key={} frontmost_pid={} frontmost_app=\"{}\"",
                pointer_inside,
                pressed_mouse_buttons,
                current_event_type,
                command_down,
                option_down,
                app_active,
                own_key,
                frontmost_pid,
                frontmost_app
            );
        }
        hide_overlay();
    }
}

/// Adjust the selection after closing the window at `removed_idx` (pure, unit-tested):
///
/// - a closed window BEFORE the selection shifts it back one (same window stays selected);
/// - closing the selection itself or anything after it leaves it (the former naturally
///   points at the next window);
/// - out of range -> the tail; an empty list -> 0.
pub(super) fn remove_window_adjust_selection(
    selected: usize,
    removed_idx: usize,
    new_len: usize,
) -> usize {
    let sel = if removed_idx < selected {
        selected - 1
    } else {
        selected
    };
    if new_len == 0 {
        0
    } else {
        sel.min(new_len - 1)
    }
}

/// Close the window of card `idx` (shared by the close button and Backspace): on a
/// successful AX close, remove it from the list and adjust selection; rebuild only as a
/// fallback when no card view exists. Closing the last one dismisses the overlay.
pub(crate) fn close_window_at(idx: usize) -> bool {
    let Some((pid, cgwid)) = with_tab_state(|state_opt| {
        let state = state_opt.as_ref()?;
        state.windows.get(idx).map(|w| (w.pid, w.window_id))
    }) else {
        return false;
    };
    // The settings window belongs to this process. Its custom AX close action would re-enter
    // AppKit from a background close worker and can crash; close it directly on the main thread.
    if pid == std::process::id() as i32 {
        crate::close_settings_for_switcher();
        return finish_window_close(idx, pid, cgwid);
    }
    if !crate::window_collector::close_ax_window(pid, cgwid) {
        log_info!(
            "close window FAILED (AX close rejected): pid={} cgwid={}",
            pid,
            cgwid
        );
        return false;
    }
    finish_window_close(idx, pid, cgwid)
}

/// Synchronous fallback for a close without a corresponding card; normal card closes use
/// commit_pending_card_close.
fn finish_window_close(idx: usize, pid: i32, cgwid: u32) -> bool {
    let valid = with_tab_state(|state_opt| {
        let Some(state) = state_opt.as_ref() else {
            return false;
        };
        let Some(window) = state.windows.get(idx) else {
            return false;
        };
        window.pid == pid && window.window_id == cgwid
    });
    if !valid {
        return false;
    }
    log_info!("close window: pid={} cgwid={}", pid, cgwid);
    let close_result = with_tab_state(|state_opt| {
        let state = match state_opt.as_mut() {
            Some(s) => s,
            None => return (false, false),
        };
        let was_visible = state.visible;
        let Some(actual_idx) = state
            .windows
            .iter()
            .position(|window| window.pid == pid && window.window_id == cgwid)
        else {
            return (false, false);
        };
        state.windows.remove(actual_idx);
        WINDOW_COUNT.store(state.windows.len(), std::sync::atomic::Ordering::Release);
        state.mru.remove(&(pid, cgwid));
        if state.windows.is_empty() {
            // All closed: dismiss the overlay, don't linger on an empty state.
            state.visible = false;
            return (true, was_visible);
        }
        state.selected =
            remove_window_adjust_selection(state.selected, actual_idx, state.windows.len());
        if !was_visible {
            return (true, false);
        }
        (true, false)
    });
    if !close_result.0 {
        return false;
    }
    if close_result.1 {
        hide_overlay();
        return true;
    }
    // The fallback may rebuild the overlay; the card close-button path never reaches it.
    reset_thumbnail_visible_range();
    reset_thumbnail_nav_anchor();
    show_overlay();
    refresh_highlight();
    true
}

/// Visually hide the overlay **without orderOut** (the window stays ordered).
/// Ordering out before activating the target lets WindowServer route focus to the wrong window,
/// leaving the target's key-window / first-responder unset (caret stops blinking, etc.).
/// Mirrors BetterCmdTab's vanish() -> activate() -> dismiss() sequence.
pub(crate) fn vanish_overlay() {
    stop_hover_timer();
    clear_thumbnail_scroll_drag();
    set_thumbnail_scroller_hover(false, false);
    // Copy both pointers before any AppKit call; resignKeyWindow can synchronously notify us.
    let window = overlay_window_ptr();
    let container = overlay_container_ptr();
    unsafe {
        if let Some(window) = window {
            // alphaValue=0 + contentView hidden: instant visual hide, window stays ordered.
            let _: () = msg_send![window, setAlphaValue: 0.0f64];
            if let Some(container) = container {
                let _: () = msg_send![container, setHidden: true];
            }
            // Ignore mouse events so the invisible panel doesn't swallow clicks (until the
            // delayed orderOut actually removes it).
            let _: () = msg_send![window, setIgnoresMouseEvents: true];
            // Resign the panel's key-window state: otherwise, when orderOut fires 0.2s later,
            // AppKit promotes the key to our app's next visible window (the settings window),
            // re-activating us and stealing focus from the target (grey traffic lights; the log
            // shows our app's activation notification repeatedly following switches). Resigning
            // key before activating the target lets the target take key focus cleanly.
            let _: () = msg_send![window, resignKeyWindow];
        }
    }
}

/// Delayed orderOut callback: called via performSelector:withObject:afterDelay: after
/// vanish_overlay, removing the overlay for real once the target window's activation has
/// settled and WindowServer focus routing is stable.
pub(crate) extern "C" fn on_delayed_order_out(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    crate::callback_guard::void("on_delayed_order_out", on_delayed_order_out_inner);
}

pub(super) fn delayed_order_out_should_hide(overlay_visible: bool) -> bool {
    !overlay_visible
}

fn on_delayed_order_out_inner() {
    let overlay_visible =
        with_tab_state(|state_opt| state_opt.as_ref().is_some_and(|state| state.visible));
    if !delayed_order_out_should_hide(overlay_visible) {
        // A new summon can happen before an older delayed callback fires. Do not order out the
        // new overlay; only restore the visual state left by the previous vanish.
        log_debug!("[overlay] skipped stale delayed orderOut while overlay is visible");
    } else {
        hide_overlay();
    }
    // Restore the overlay's alphaValue / contentView visibility / mouse events for the next
    // show_overlay call.
    let window = overlay_window_ptr();
    let container = overlay_container_ptr();
    unsafe {
        if let Some(window) = window {
            let _: () = msg_send![window, setAlphaValue: 1.0f64];
            let _: () = msg_send![window, setIgnoresMouseEvents: false];
        }
        if let Some(container) = container {
            let _: () = msg_send![container, setHidden: false];
        }
    }
}

/// Deferred-raise slot + scheduling. The release/click/Enter handlers vanish the overlay and
/// END their runloop turn so the render transaction commits (the vanish reaches the screen,
/// the overlay disappears instantly); the activate+raise chain runs on the NEXT runloop cycle.
/// Previously the vanish and the activate+AX chain shared one main-thread turn: while the AX
/// enumeration blocked it, the vanish could not commit -- the overlay lingered, frozen, over
/// the already-switched window.
struct DeferredRaise {
    pid: i32,
    cgwid: u32,
    minimized: bool,
    scheduled_at: Instant,
}

static DEFERRED_RAISE: LazyLock<Mutex<Option<DeferredRaise>>> = LazyLock::new(|| Mutex::new(None));

pub(super) fn schedule_deferred_raise(pid: i32, cgwid: u32, minimized: bool) {
    *DEFERRED_RAISE.lock().unwrap() = Some(DeferredRaise {
        pid,
        cgwid,
        minimized,
        scheduled_at: Instant::now(),
    });
    unsafe {
        let ctrl = crate::CONTROLLER.lock().unwrap().unwrap().0;
        // afterDelay:0 = run as soon as the current turn ends: commit the vanish first, then raise.
        let _: () = msg_send![
            ctrl,
            performSelector: sel!(handleDeferredRaise:),
            withObject: std::ptr::null::<AnyObject>(),
            afterDelay: 0.0f64
        ];
    }
}

pub(crate) extern "C" fn on_deferred_raise(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    let Some(job) = DEFERRED_RAISE.lock().unwrap().take() else {
        return;
    };
    // Release-to-callback gap = the extra delay paid for committing the vanish first;
    // normally a few milliseconds.
    log_debug!(
        "[raise] deferred fire: pid={} cgwid={} +{}ms after release",
        job.pid,
        job.cgwid,
        job.scheduled_at.elapsed().as_millis()
    );
    activate_and_raise(job.pid, job.cgwid, job.minimized);
}

pub(super) fn cancel_scheduled_order_out() {
    unsafe {
        let Some(ctrl) = crate::CONTROLLER
            .lock()
            .unwrap()
            .map(|controller| controller.0)
        else {
            return;
        };
        let _: () = msg_send![
            class!(NSObject),
            cancelPreviousPerformRequestsWithTarget: ctrl,
            selector: sel!(handleDelayedOrderOut:),
            object: std::ptr::null::<AnyObject>()
        ];
    }
}

/// Schedule a delayed orderOut on the main thread (via the controller's handleDelayedOrderOut:).
/// Called after vanish_overlay(): the target window's activation completes within 0.2s, after
/// which the overlay is removed for real, avoiding orderOut interfering with WindowServer focus.
pub(super) fn schedule_delayed_order_out() {
    unsafe {
        let ctrl = crate::CONTROLLER.lock().unwrap().unwrap().0;
        // performSelector:withObject:afterDelay: schedules on the main thread's RunLoop.
        let _: () = msg_send![
            ctrl,
            performSelector: sel!(handleDelayedOrderOut:),
            withObject: std::ptr::null::<AnyObject>(),
            afterDelay: 0.2f64
        ];
    }
}

pub(crate) fn refresh_highlight() {
    unsafe {
        let document = match card_document() {
            Some(document) => document,
            None => return,
        };
        let Some(selected) = with_tab_state(|state_opt| {
            let state = state_opt.as_ref()?;
            state.visible.then_some(state.selected)
        }) else {
            return;
        };
        let colors = current_colors();
        // Match the HTML reference with a subtle background and 1.5px inset-style border instead of
        // the previous heavy blue outline.
        let sel_bg_color = hex_to_cg_color(colors.card_bg_sel);
        let sel_border_color = hex_to_cg_color(colors.card_border_sel);

        for sv in card_views(document) {
            let layer: *mut AnyObject = msg_send![sv, layer];
            let Some(tag) = get_card_index(sv) else {
                continue;
            };
            // Read the card's title-label text to verify content matches the index (investigating
            // "shows Picview but opens Ghostty").
            let is_selected = tag == selected;
            let preview: *mut AnyObject = msg_send![sv, viewWithTag: THUMB_PREVIEW_TAG];
            if !preview.is_null() {
                // The HTML applies translateY(-1px) to the `.item.selected` root rather
                // than `.preview`; +1pt in AppKit coordinates lifts the caption row and
                // preview together as one card.
                layer_set_translation_y(layer, thumbnail_card_lift_y(is_selected));
            }
            if is_selected {
                // The mockup's .item.selected: a crisp 1.5px accent border
                // rgba(75,123,236,.78). A soft ring is invisible on the white
                // surface -- the outline needs the solid accent to show.
                let _: () = msg_send![layer, setBorderWidth: 1.5f64];
                layer_set_border(layer, sel_border_color);
                layer_set_background(layer, sel_bg_color);
                // Drop shadow: 0 10px 24px rgba(42,62,102,.12), straight from the mockup.
                // CSS blur 24 ≈ CALayer shadowRadius 12; CALayer's shadowOffset y is up-positive,
                // so a downward shadow takes -10.
                let shadow_color = hex_to_cg_color(0x2A3E66FF);
                // A CGColorRef cannot go through objc2's msg_send! ('@' vs
                // '^{CGColor=}' rejected at runtime -- crashed the summon, verified);
                // use raw objc_msgSend per the layer_set_background convention.
                layer_set_shadow_color(layer, shadow_color);
                let _: () = msg_send![layer, setShadowOpacity: 0.12f32];
                let _: () = msg_send![layer, setShadowRadius: 12.0f64];
                let _: () = msg_send![layer, setShadowOffset: NSSize::new(0.0, -10.0)];
            } else {
                let _: () = msg_send![layer, setBorderWidth: 0.0f64];
                layer_set_border(layer, std::ptr::null_mut());
                layer_set_background(layer, std::ptr::null_mut());
                let _: () = msg_send![layer, setShadowOpacity: 0.0f32];
            }

            // The first CSS box-shadow is a zero-blur 2px accent-soft ring outside the
            // card; it cannot share CALayer.shadow with the dark blurred drop shadow.
            // A dedicated ring view preserves the RGB, raises alpha to 38% for Liquid
            // Glass, adds a zero-offset blue glow, and appears only on the selected card.
            let ring: *mut AnyObject = msg_send![sv, viewWithTag: THUMB_SELECTION_RING_TAG];
            if !ring.is_null() {
                let ring_layer: *mut AnyObject = msg_send![ring, layer];
                layer_set_border(
                    ring_layer,
                    hex_to_cg_color(color_with_alpha(
                        colors.card_border_sel,
                        SELECTION_RING_ALPHA,
                    )),
                );
                layer_set_shadow_color(
                    ring_layer,
                    hex_to_cg_color(color_with_alpha(colors.card_border_sel, 0xFF)),
                );
                let glow_opacity = if is_selected {
                    SELECTION_GLOW_OPACITY
                } else {
                    0.0
                };
                let _: () = msg_send![ring_layer, setShadowOpacity: glow_opacity];
                let _: () = msg_send![ring_layer, setShadowRadius: SELECTION_GLOW_RADIUS];
                let _: () = msg_send![ring_layer, setShadowOffset: NSSize::new(0.0, 0.0)];
                let _: () = msg_send![ring, setHidden: !is_selected];
            }

            // Nudge the icon up by 2pt when selected; recompute from the baseline on every
            // refresh so repeated selection changes never accumulate the offset.
            let icon: *mut AnyObject = msg_send![sv, viewWithTag: ICON_VIEW_TAG];
            if !icon.is_null() {
                let icon_frame: NSRect = msg_send![icon, frame];
                let icon_px_now = icon_frame.size.height;
                let icon_bottom = card_h() - 8.0 - icon_px();
                let base_y = if (icon_px_now - icon_px()).abs() < 0.5 {
                    icon_bottom
                } else {
                    icon_bottom + (icon_px() - icon_px_now) / 2.0
                };
                let icon_y = base_y
                    + if is_selected {
                        SELECTED_CONTENT_NUDGE
                    } else {
                        0.0
                    };
                let _: () = msg_send![
                    icon,
                    setFrameOrigin: NSPoint::new(icon_frame.origin.x, icon_y)
                ];
            }

            // Thumbnail mode: the preview no longer moves independently because the card
            // root now lifts the caption and preview together; the transparent, borderless
            // preview is represented by the card's outer selection ring only.

            // The ⌫ close button follows the selection: the selected card shows it, the
            // rest hide it (visible whenever the card is selected, keyboard navigation
            // included -- not only while the mouse hovers).
            let btn: *mut AnyObject = msg_send![sv, viewWithTag: CLOSE_BTN_TAG];
            if !btn.is_null() {
                let _: () = msg_send![btn, setHidden: tag != selected];
            }
        }
    }
}

pub(crate) fn extract_uncached_icons() {
    let uncached: Vec<i32> = with_tab_state(|state_opt| {
        if let Some(state) = state_opt.as_ref() {
            state
                .windows
                .iter()
                .filter(|w| w.icon_path.is_none())
                .map(|w| w.pid)
                .collect::<HashSet<_>>()
                .into_iter()
                .collect()
        } else {
            Vec::new()
        }
    });

    // Record which window indices got a freshly cached icon so we can re-render
    // just those cards in place (otherwise the on-screen letter icons wouldn't
    // update until the next summon).
    let mut updated_indices: Vec<usize> = Vec::new();
    // TIMING-DEBUG per-PID icon timing: find which app's icon extraction slows the summon down.
    let mut icons_total_ms: u128 = 0; // TIMING-DEBUG
    for pid in uncached {
        let t_icon = Instant::now(); // TIMING-DEBUG
        if let Some(ref path) = extract_icon_to_cache(pid) {
            let path = path.clone();
            with_tab_state(|state_opt| {
                if let Some(state) = state_opt.as_mut() {
                    for (i, w) in state.windows.iter_mut().enumerate() {
                        if w.pid == pid && w.icon_path.is_none() {
                            w.icon_path = Some(path.clone());
                            updated_indices.push(i);
                        }
                    }
                }
            });
        }
        let icon_ms = t_icon.elapsed().as_millis(); // TIMING-DEBUG
        icons_total_ms += icon_ms;
        // TIMING-DEBUG flag slow extractions (>= 20ms).
        if icon_ms >= 20 {
            log_debug!("[overlay] icons: extract pid={} {}ms", pid, icon_ms);
        }
    }

    if !updated_indices.is_empty() {
        let t_rebuild = Instant::now(); // TIMING-DEBUG
        rebuild_cards(&updated_indices);
        // TIMING-DEBUG summary: total extraction time plus in-place card rebuild time.
        log_debug!(
            "[overlay] icons: extract_total={}ms rebuild_cards x={} {}ms",
            icons_total_ms,
            updated_indices.len(),
            t_rebuild.elapsed().as_millis()
        );
    }
}

/// Rebuild the card views for the given window indices in place, so newly
/// extracted icons appear immediately without re-summoning. Each affected card
/// is replaced by a fresh one built from the updated `WindowInfo` (which now has
/// an icon_path), preserving its frame and card index.
pub(crate) fn rebuild_cards(indices: &[usize]) {
    if indices.is_empty() || card_close_in_progress() {
        return;
    }
    let affected: HashSet<usize> = indices.iter().copied().collect();
    let to_rebuild: HashMap<usize, WindowInfo> = with_tab_state(|state_opt| {
        let Some(state) = state_opt.as_ref() else {
            return HashMap::new();
        };
        if !state.visible {
            return HashMap::new();
        }
        affected
            .iter()
            .filter_map(|&i| state.windows.get(i).map(|w| (i, w.clone())))
            .collect()
    });
    if to_rebuild.is_empty() {
        return;
    }

    unsafe {
        let thumbnail_capture_allowed =
            crate::theme::thumbnails_enabled() && crate::thumbnail::capture_allowed();
        let document = match card_document() {
            Some(document) => document,
            None => return,
        };

        // Collect affected card views + their frames first; don't mutate the
        // subview array while iterating it.
        let mut replacements: Vec<(*mut AnyObject, NSRect, usize)> = Vec::new();
        for sv in card_views(document) {
            let Some(idx) = get_card_index(sv) else {
                continue;
            };
            if to_rebuild.contains_key(&idx) {
                let frame: NSRect = msg_send![sv, frame];
                replacements.push((sv, frame, idx));
            }
        }

        for (old_view, frame, idx) in replacements {
            if let Some(w) = to_rebuild.get(&idx) {
                remove_card_index(old_view);
                // Reuse the old card frame's width AND height (in-place replacement:
                // after flow shrink both are per-card values).
                let new_card = create_card_view(
                    w,
                    idx,
                    frame.size.width,
                    frame.size.height,
                    thumbnail_capture_allowed,
                );
                let _: () = msg_send![new_card, setFrame: frame];
                let _: () = msg_send![old_view, removeFromSuperview];
                let _: () = msg_send![document, addSubview: new_card];
                release_obj(new_card); // container owns the card; drop create_card_view's alloc +1
            }
        }

        // New card views have no selection border; re-apply the highlight.
        refresh_highlight();
    }
}

/// Lightweight post-capture update: refresh only affected cards' preview containers,
/// without rebuilding captions, buttons, tracking areas, or selection layers. One
/// ready batch scans the container's subviews once.
pub(crate) fn refresh_thumbnail_previews(keys: &[(i32, u32)]) {
    if keys.is_empty() || card_close_in_progress() {
        return;
    }
    let affected: HashSet<WindowKey> = keys.iter().copied().collect();
    let windows: HashMap<WindowKey, WindowInfo> = with_tab_state(|state_opt| {
        let Some(state) = state_opt.as_ref() else {
            return HashMap::new();
        };
        if !state.visible {
            return HashMap::new();
        }
        state
            .windows
            .iter()
            .filter_map(|window| {
                let key = (window.pid, window.window_id);
                affected.contains(&key).then(|| (key, window.clone()))
            })
            .collect()
    });
    if windows.is_empty() {
        return;
    }

    unsafe {
        let started = Instant::now();
        let capture_allowed =
            crate::theme::thumbnails_enabled() && crate::thumbnail::capture_allowed();
        let colors = current_colors();
        let document = match card_document() {
            Some(document) => document,
            None => return,
        };
        let mut updated = 0usize;
        for card in card_views(document) {
            let Some(key) = card_key(card) else {
                continue;
            };
            let Some(window) = windows.get(&key) else {
                continue;
            };
            let preview: *mut AnyObject = msg_send![card, viewWithTag: THUMB_PREVIEW_TAG];
            if preview.is_null() {
                continue;
            }
            // Snapshot the frame version before populating and sync it into the
            // signature afterwards; under a race the signature lags at most one
            // version and the next summon self-heals via Replace.
            let epoch = crate::thumbnail::frame_epoch(key.0, key.1);
            populate_thumbnail_preview(preview, window, &colors, capture_allowed);
            sync_card_signature_epoch(card, epoch);
            updated += 1;
        }
        log_debug!(
            "[thumb] preview refresh: requested={} updated={} ms={}",
            windows.len(),
            updated,
            started.elapsed().as_millis()
        );
    }
}

/// Re-apply glass properties (style/tint/cornerRadius) from CONFIG to the existing
/// NSGlassEffectView, for hot reload. Only effective on macOS 26+ once the glass view
/// exists; otherwise a no-op.
pub(crate) unsafe fn apply_glass_properties() {
    let glass = match *GLASS_VIEW.lock().unwrap() {
        Some(g) => g.0,
        None => return,
    };
    if glass.is_null() {
        return;
    }
    let radius = CONFIG.read().unwrap().appearance.corner_radius;
    let style_name = config::effective_glass_style();
    let tint_hex = config::parse_hex8(&config::effective_glass_tint());
    let _: () = msg_send![glass, setCornerRadius: radius];
    // Mirror the layer hard-clip: cornerRadius rounds the tint but not the blur, so masksToBounds
    // is needed to clip the blur into the rounded shape (see (6.5) in create_overlay_window).
    let glass_layer: *mut AnyObject = msg_send![glass, layer];
    if !glass_layer.is_null() {
        let _: () = msg_send![glass_layer, setCornerRadius: radius];
        let _: () = msg_send![glass_layer, setMasksToBounds: true];
    }
    let style: i64 = match style_name.as_str() {
        "clear" => 1,
        _ => 0, // regular
    };
    let _: () = msg_send![glass, setStyle: style];
    let tint = hex_to_ns_color(tint_hex);
    let _: () = msg_send![glass, setTintColor: tint];
}

pub(crate) fn apply_theme() {
    // Rebuild visible cards as well as updating the window material. Card labels and preview
    // layers use concrete colors chosen at creation time, so changing only NSAppearance leaves
    // existing cards with the previous palette until the overlay is summoned again.
    let visible_indices = with_tab_state(|state_opt| {
        state_opt
            .as_ref()
            .filter(|state| state.visible)
            .map(|state| (0..state.windows.len()).collect::<Vec<_>>())
            .unwrap_or_default()
    });

    let is_dark = crate::theme::resolved_is_dark();
    let theme_changed = {
        let mut last = LAST_APPLIED_THEME_DARK.lock().unwrap();
        let changed = *last != Some(is_dark);
        *last = Some(is_dark);
        changed
    };

    unsafe {
        // The theme comes from the resolved config; explicit themes are saved by Settings, while
        // auto themes are refreshed from the system appearance notification.
        // Update window appearance for blur material tint
        if let Some(window) = *OVERLAY_WINDOW.lock().unwrap() {
            let appearance_name = if is_dark {
                make_nsstring("NSAppearanceNameDarkAqua")
            } else {
                make_nsstring("NSAppearanceNameAqua")
            };
            let appearance: *mut AnyObject =
                msg_send![class!(NSAppearance), appearanceNamed: appearance_name];
            CFRelease(appearance_name as *const c_void);
            if !appearance.is_null() {
                let _: () = msg_send![window.0, setAppearance: appearance];
            }
        }

        apply_glass_properties();
    }

    if visible_indices.is_empty() {
        refresh_highlight();
    } else {
        rebuild_cards(&visible_indices);
    }
    // Theme changes also alter the pixels inside captured application windows. Rebuild the card
    // tree immediately and force a fresh capture for every known window so the next preview does
    // not keep a frame from the previous light/dark appearance.
    if theme_changed {
        let target_px_h = *THUMB_CAPTURE_TARGET_PX_H.lock().unwrap();
        crate::thumbnail::refresh_for_theme(target_px_h);
    }
    update_status_label();
}
