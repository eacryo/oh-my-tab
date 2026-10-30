//! Click-outside cancel (local event monitor + focus-loss dismissal).

use super::*;
use crate::callback_guard;

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

/// The observer instance other overlay controls use as their target, so the overlay owns its own
/// selectors instead of reaching into another module's private target.
pub(super) fn overlay_observer_target() -> *mut AnyObject {
    unsafe { overlay_observer() }
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
            // The footer gear's action. It lives on this observer so the overlay owns its own
            // target, rather than reaching into another module's private selector.
            class_addMethod(
                cls,
                sel!(openSwitcherSettings:),
                on_open_switcher_settings as *mut c_void,
                types.as_ptr(),
            );
            objc_registerClassPair(cls);
            let inst: *mut AnyObject = msg_send![cls as *const AnyObject, new];
            CallbackTarget::new(inst)
        })
        .0
}

/// The footer gear's click: retire this summon, then open the App Switcher settings page.
///
/// Order is load-bearing. show_settings_page activates the app and can synchronously resign this
/// panel's key status, so the session must already be over by the time it runs -- otherwise the
/// resign path could hide the panel mid-click or a trigger release could commit the highlighted
/// card. Each step therefore holds no borrow across the next:
///
///   1. retire the session logically (short borrow), clearing sticky and the published claim;
///   2. present the settings window (no borrow held);
///   3. perform the visual hide and schedule the deferred order-out.
///
/// A trigger release arriving after step 1 finds state.visible == false, so it neither commits a
/// card nor opens settings a second time.
extern "C" fn on_open_switcher_settings(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    callback_guard::void("on_open_switcher_settings", open_switcher_settings_inner);
}

fn open_switcher_settings_inner() {
    crate::debug_assert_main_thread();
    // Two entry paths reach this action, and they need different guards:
    //
    //   * A real mouse press arms on mouseDown. The arm flag is what separates THIS press from a
    //     stale mouseUp belonging to a previous session (the session ended and a new one was
    //     summoned in between), so a mouse-sourced action without the arm is ignored.
    //   * An Accessibility press (VoiceOver's AXPress) has no mouseDown at all, so requiring the arm
    //     would reject it outright. It is identified by the absence of a current mouse event and is
    //     allowed through; the live-session check below still applies to it.
    let from_mouse = unsafe {
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let event: *mut AnyObject = msg_send![nsapp, currentEvent];
        !event.is_null() && {
            let kind: isize = msg_send![event, type];
            kind == 1 /* NSEventTypeLeftMouseDown */ || kind == 2 /* NSEventTypeLeftMouseUp */
        }
    };
    if from_mouse && !footer_settings::is_armed() {
        log_debug!("[overlay] stale footer-settings click ignored");
        return;
    }
    footer_settings::disarm();
    let live = with_tab_state(|state_opt| {
        let state = state_opt.as_mut()?;
        if !state.visible {
            return None;
        }
        // Step 1: end the session LOGICALLY. No AppKit call happens under this borrow.
        state.visible = false;
        state.pending_first_show = false;
        state.pending_first_release = false;
        Some(())
    });
    if live.is_none() {
        log_debug!("[overlay] footer-settings click ignored: the session already ended");
        return;
    }
    // Retire the session's published claim and sticky flag before any focus-changing call, so a
    // re-summon cannot inherit them and a release cannot commit.
    clear_sticky();
    publish_panel_snapshot(false);
    // Step 2: present settings. This may synchronously resign this panel's key status; the session
    // is already over, so the dismiss path re-entering here finds nothing to do.
    crate::settings::show_switcher_page();
    // Step 3: the visual hide, deferred like every other commit so the settings window is composed
    // before this panel disappears.
    vanish_overlay();
    schedule_delayed_order_out();
    crate::performance::end_switcher_activity();
}

/// The overlay lost key -> cancel the switch.
extern "C" fn overlay_window_resigned(_self: *mut c_void, _cmd: Sel, _note: *mut c_void) {
    callback_guard::void("overlay_window_resigned", || {
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
    // A panel action that changed the frontmost app (hide/quit) makes this panel resign key as a
    // side effect. That is not the user clicking away, and dismissing here would close the panel
    // the feature promises to keep open.
    if take_action_focus() {
        log_debug!("[overlay] resign-key attributed to a panel action; the panel stays open");
        // Losing key does not restore itself: the panel must be able to receive the very next
        // key (Return, Esc, another action). Re-make it key so the session stays usable.
        unsafe {
            if let Some(window) = overlay_window_ptr() {
                let _: () = msg_send![window, makeKeyWindow];
            }
        }
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
                // The panel, not the padded window: the padding is not part of the panel the pointer can
                // be "inside of".
                let frame = crate::glass::panel_frame_of(window);
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
    callback_guard::void("on_delayed_order_out", on_delayed_order_out_inner);
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
    on_other_desktop: bool,
    scheduled_at: Instant,
}

static DEFERRED_RAISE: LazyLock<Mutex<Option<DeferredRaise>>> = LazyLock::new(|| Mutex::new(None));

pub(super) fn schedule_deferred_raise(
    pid: i32,
    cgwid: u32,
    minimized: bool,
    on_other_desktop: bool,
) {
    *DEFERRED_RAISE.lock().unwrap() = Some(DeferredRaise {
        pid,
        cgwid,
        minimized,
        on_other_desktop,
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
    callback_guard::void("on_deferred_raise", || {
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
        activate_and_raise(job.pid, job.cgwid, job.minimized, job.on_other_desktop);
    });
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
        // Both layouts mark the selection with a plate, using the design system's own tokens
        // (design-style §3/§12): `accent` for the rim and `selection_plate_bg` for the wash.
        // `colors.card_bg_sel` / `card_border_sel` no longer colour the switcher at all -- the
        // plate hugs the icon or the preview rather than the card, so a card-shaped fill, border
        // and drop shadow are gone.
        let palette = ui_palette();
        let plate_bg_color = hex_to_cg_color(palette.selection_plate_bg);
        let plate_border_color = hex_to_cg_color(palette.accent);

        for sv in card_views(document) {
            let layer: *mut AnyObject = msg_send![sv, layer];
            let Some(tag) = get_card_index(sv) else {
                continue;
            };
            // Read the card's title-label text to verify content matches the index (investigating
            // "shows Picview but opens Ghostty").
            let is_selected = tag == selected;
            // A thumbnail card carries the preview view; the icon-only layout does not. That is a
            // per-card fact, so a layout swap mid-session cannot leave a card decorated the old
            // way, and it is the same signal the thumbnail lift below already relies on.
            let preview: *mut AnyObject = msg_send![sv, viewWithTag: THUMB_PREVIEW_TAG];
            let icon_only = preview.is_null();
            // The plate below is the whole selection decoration in both layouts: the card itself
            // never paints, so the panel's material stays behind the icons, the previews and the
            // caption rows. Stated unconditionally rather than only on selection, so a reused card
            // cannot inherit a previous decoration.
            let _: () = msg_send![layer, setBorderWidth: 0.0f64];
            layer_set_border(layer, std::ptr::null_mut());
            layer_set_background(layer, std::ptr::null_mut());
            let _: () = msg_send![layer, setShadowOpacity: 0.0f32];
            if !icon_only {
                // The HTML applies translateY(-1px) to the `.item.selected` root rather
                // than `.preview`; +1pt in AppKit coordinates lifts the caption row and
                // preview together as one card. The plate is a subview, so it lifts with them.
                layer_set_translation_y(layer, thumbnail_card_lift_y(is_selected));
            }

            let ring: *mut AnyObject = msg_send![sv, viewWithTag: THUMB_SELECTION_RING_TAG];
            if !ring.is_null() {
                let ring_layer: *mut AnyObject = msg_send![ring, layer];
                // One rim and one wash, no glow and no drop shadow: the plate marks the icon or
                // the window by nesting around it, and a glow would only re-add the halo this
                // replaced.
                layer_set_border(ring_layer, plate_border_color);
                layer_set_background(ring_layer, plate_bg_color);
                let _: () = msg_send![ring_layer, setShadowOpacity: 0.0f32];
                if icon_only {
                    // The icon-only plate takes the same SELECTED_CONTENT_NUDGE the icon below
                    // takes, so the two stay concentric instead of the icon drifting toward the
                    // plate's top edge. The thumbnail plate is a subview of the lifted card and
                    // follows it without help.
                    let frame: NSRect = msg_send![sv, frame];
                    let (px, py, _, _) =
                        crate::theme::icon_selection_plate(frame.size.width, frame.size.height);
                    let lift = if is_selected {
                        SELECTED_CONTENT_NUDGE
                    } else {
                        0.0
                    };
                    let _: () = msg_send![ring, setFrameOrigin: NSPoint::new(px, py + lift)];
                }
                let _: () = msg_send![ring, setHidden: !is_selected];
            }

            // Nudge the icon up when selected; recompute from the baseline on every refresh so
            // repeated selection changes never accumulate the offset. The icon-only plate above
            // takes the same SELECTED_CONTENT_NUDGE, which is what keeps the icon centred inside
            // it -- moving one without the other pulls the icon off its plate.
            let icon: *mut AnyObject = msg_send![sv, viewWithTag: ICON_VIEW_TAG];
            if !icon.is_null() {
                let icon_frame: NSRect = msg_send![icon, frame];
                let icon_px_now = icon_frame.size.height;
                let icon_bottom = card_h() - crate::theme::ICON_TOP_INSET - icon_px();
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
            // The icon-only button's scrim follows it; the thumbnail layout has none.
            let scrim: *mut AnyObject = msg_send![sv, viewWithTag: CLOSE_SCRIM_TAG];
            if !scrim.is_null() {
                let _: () = msg_send![scrim, setHidden: tag != selected];
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

/// The switcher's backdrop policy. Every card carries a text surface for its caption, and the status
/// footer carries one too, so the material is left honest (design-style §3); the
/// switcher is the active panel, so no inactive-glass compensation.
/// The remaining knobs are the clipboard picker's (`picker_backdrop_options`): the two panels are the
/// same kind of surface floating over another app's content and read as one product when their glass
/// style, edge and blur posture agree.
pub(crate) const fn switcher_backdrop_options() -> crate::glass::BackdropOptions {
    // `high`: §7's row for a surface above the whole UI, which is what a switcher summoned over another
    // app's content is.
    crate::glass::BackdropOptions::new(None)
        .with_elevation(Some(crate::theme::ELEVATION_HIGH))
        // Share the configured glass style and native edge with the clipboard picker.
        .with_blur(0.0, 1.0)
        .with_native_glass_edge()
}

/// The bevel ring's view, kept so a live theme/material pass can ask for a repaint.
pub(crate) static OVERLAY_EDGE: MainThreadSlot<Option<ObjPtr>> =
    unsafe { MainThreadSlot::new(None) };

/// The same bevel the clipboard picker draws (`picker_edge_draw`), re-keyed to the switcher's
/// corner radius and palette so the two panels carry the identical rim.
extern "C" fn overlay_edge_draw(view: *mut AnyObject, _cmd: Sel, _dirty: NSRect) {
    callback_guard::void("overlay_edge_draw", || unsafe {
        if crate::glass::PanelMaterial::effective() != crate::glass::PanelMaterial::LiquidGlass {
            return;
        }
        use crate::theme::*;
        let bounds: NSRect = msg_send![view, bounds];
        let panel_radius = CONFIG.read().unwrap().appearance.corner_radius;
        let ring = |inset: f64, width: f64| -> *mut AnyObject {
            let rounded = |inset: f64| -> *mut AnyObject {
                let rect = NSRect::new(
                    NSPoint::new(bounds.origin.x + inset, bounds.origin.y + inset),
                    NSSize::new(
                        (bounds.size.width - inset * 2.0).max(0.0),
                        (bounds.size.height - inset * 2.0).max(0.0),
                    ),
                );
                let radius = (panel_radius + CLIPBOARD_EDGE_OUTSET - inset).max(0.0);
                msg_send![class!(NSBezierPath), bezierPathWithRoundedRect: rect, xRadius: radius, yRadius: radius]
            };
            let path = rounded(inset);
            let _: () = msg_send![path, appendBezierPath: rounded(inset + width)];
            let _: () = msg_send![path, setWindingRule: 1isize];
            path
        };
        let outer = ring(0.0, CLIPBOARD_EDGE_OUTER_WIDTH);
        let color = crate::ffi::hex_to_ns_color(if crate::theme::ui_palette().dark {
            CLIPBOARD_EDGE_OUTER_DARK
        } else {
            CLIPBOARD_EDGE_OUTER_LIGHT
        });
        let _: () = msg_send![color, setFill];
        let _: () = msg_send![outer, fill];
        let inner = ring(CLIPBOARD_EDGE_OUTER_WIDTH, CLIPBOARD_EDGE_INNER_WIDTH);
        let gradient: *mut AnyObject = msg_send![class!(NSGradient), alloc];
        let gradient: *mut AnyObject = msg_send![gradient,
            initWithStartingColor: crate::ffi::hex_to_ns_color(CLIPBOARD_EDGE_SHADE),
            endingColor: crate::ffi::hex_to_ns_color(CLIPBOARD_EDGE_HIGHLIGHT)
        ];
        let _: () = msg_send![gradient, drawInBezierPath: inner, angle: 90.0f64];
        release_obj(gradient);
    });
}

/// The ring never answers hits: it hangs 0.5pt outside the material clip and would otherwise
/// swallow the bottom-right pixel of a click that belongs to the card grid.
extern "C" fn overlay_edge_hit_test(
    _view: *mut AnyObject,
    _cmd: Sel,
    _point: NSPoint,
) -> *mut AnyObject {
    callback_guard::nil("overlay_edge_hit_test", std::ptr::null_mut)
}

/// Hang the bevel ring on the panel silhouette — same shape as `install_picker_edge`: the view
/// rides the root (the shadow carrier when elevation installed one) and reaches 0.5pt past the
/// material root so the gradient sits just outside the system specular.
pub(crate) unsafe fn install_overlay_edge(backdrop: &crate::glass::InstalledBackdrop) {
    let Some(parent) = backdrop.root else {
        return;
    };
    let Some(material) = backdrop.material_root else {
        return;
    };
    let frame: NSRect = if parent.0 == material.0 {
        msg_send![material.0, bounds]
    } else {
        msg_send![material.0, frame]
    };
    let outset = crate::theme::CLIPBOARD_EDGE_OUTSET;
    let frame = NSRect::new(
        NSPoint::new(frame.origin.x - outset, frame.origin.y - outset),
        NSSize::new(
            frame.size.width + outset * 2.0,
            frame.size.height + outset * 2.0,
        ),
    );
    static CLASS: OnceLock<StaticClass> = OnceLock::new();
    let cls = StaticClass::register_once(&CLASS, c"OhMyTabOverlayEdge", || {
        crate::ffi::build_class(c"OhMyTabOverlayEdge", class!(NSView), |builder| {
            builder.add_method::<AnyObject, _>(
                sel!(drawRect:),
                overlay_edge_draw as extern "C" fn(*mut AnyObject, Sel, NSRect),
            );
            builder.add_method::<AnyObject, _>(
                sel!(hitTest:),
                overlay_edge_hit_test
                    as extern "C" fn(*mut AnyObject, Sel, NSPoint) -> *mut AnyObject,
            );
        })
    });
    let edge: *mut AnyObject = msg_send![cls.0 as *mut AnyObject, alloc];
    let edge: *mut AnyObject = msg_send![edge, initWithFrame: frame];
    let _: () = msg_send![edge, setAutoresizingMask: 18u64];
    let _: () = msg_send![parent.0, addSubview: edge];
    *OVERLAY_EDGE.lock().unwrap() = Some(ObjPtr::new(edge));
    release_obj(edge);
}

/// Re-apply surface properties from CONFIG to the installed backdrop, for hot reload:
/// glass style/tint, frost material, opaque surface color. Corner radius is re-asserted on
/// whichever root view is installed (glass also carries it as its own rounding property).
pub(crate) unsafe fn apply_glass_properties() {
    if let Some(edge) = *OVERLAY_EDGE.lock().unwrap() {
        let _: () = msg_send![edge.0, setNeedsDisplay: true];
    }
    let Some(backdrop) = *OVERLAY_BACKDROP.lock().unwrap() else {
        return;
    };
    crate::glass::apply_live_properties(backdrop, switcher_backdrop_options());
    let Some(root) = backdrop.root else {
        return;
    };
    let radius = CONFIG.read().unwrap().appearance.corner_radius;
    if backdrop.glass.is_some() {
        let _: () = msg_send![root.0, setCornerRadius: radius];
    }
    // Mirror the layer hard-clip: the glass cornerRadius rounds the tint but not the blur, so
    // masksToBounds is needed to clip the blur into the rounded shape (see (6.5) notes).
    let root_layer: *mut AnyObject = msg_send![root.0, layer];
    if !root_layer.is_null() {
        let _: () = msg_send![root_layer, setCornerRadius: radius];
        let _: () = msg_send![root_layer, setMasksToBounds: true];
    }
}

/// Bring the switcher's backdrop in line with the effective material. Called when the
/// panel-material setting changes and on every summon: the window is created once and
/// reused, so show-time sync is what picks up Reduce Transparency toggles.
pub(crate) unsafe fn apply_backdrop_material() {
    let Some(window) = overlay_window_ptr() else {
        return;
    };
    let Some(old) = *OVERLAY_BACKDROP.lock().unwrap() else {
        return;
    };
    if old.material == crate::glass::PanelMaterial::effective() {
        return;
    }
    let radius = CONFIG.read().unwrap().appearance.corner_radius;
    // The panel rect: a swap rebuilds the material hierarchy, which is the panel's, not the padded
    // window's.
    let panel_rect = crate::glass::panel_frame_of(window);
    let new = crate::glass::swap_backdrop(
        window,
        &old,
        panel_rect,
        radius,
        switcher_backdrop_options(),
    );
    // The swap retired the root the bevel ring was hanging on; rebuild it on the new one.
    install_overlay_edge(&new);
    *OVERLAY_BACKDROP.lock().unwrap() = Some(new);
    apply_glass_properties();
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
            // Vibrant, not plain: this panel's text is drawn on the user's material, so it uses the
            // system's vibrant label colors and AppKit's contrast-preserving treatment
            // (design-style §3, "the ink adapts").
            let appearance_name = make_nsstring(crate::glass::vibrant_appearance_name(is_dark));
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
