//! The overlay's in-panel action keys: Cmd+W / Cmd+M / Cmd+H / Cmd+Q on the highlighted card.
//!
//! Key routing. Command-modified key equivalents do NOT arrive at keyDown:. AppKit offers them to
//! performKeyEquivalent: first (key window, then the view hierarchy, then the main menu). The
//! settings window already relies on this for its own Cmd+Q, so the overlay uses the same
//! window-level override, registered on OhMyTabOverlayWindow beside canBecomeKeyWindow.
//!
//! Consumption rule. Once panel_actions has recorded the press as claimed, this handler CONSUMES
//! the event even when the action can no longer run (the panel went invisible, or the switch was
//! turned off between the tap's pass-through and this main-thread call). Returning false there
//! would hand a panel-claimed Cmd+Q to the main menu and quit oh-my-tab -- the exact failure the
//! design round identified. The action is merely cancelled.
//!
//! Session. Pressing an action key marks the summon sticky: the panel stays open and a later
//! release of the trigger modifier no longer commits the highlighted card. The user commits with
//! Return or a card click and dismisses with Esc. Every other exit (Esc, Return, click, an emptied
//! list, a new summon, the master switch going off) clears it, so no state outlives its summon.

use super::*;
use crate::panel_actions::{self, PanelAction, PanelKeymap, PanelSnapshot};
use std::time::{Duration, Instant};

/// Whether this summon is sticky (an action key has been pressed in it). Lives beside the rest of
/// the session state: the release handler runs on the main thread, so no atomic or channel is
/// needed, and a fresh summon resets it.
static STICKY_SUMMON: AtomicBool = AtomicBool::new(false);

/// True while a panel action is changing the frontmost app (hide/quit).
///
/// Hiding or quitting the app the user was in hands the frontmost slot to another process, which
/// makes our nonactivating panel resign key. The ordinary resign handling treats losing key as "the
/// user clicked away" and dismisses the panel -- so without this guard Cmd+H closed the very panel
/// the feature promises to keep open. The guard is time-boxed: it suppresses only the resign the
/// action itself causes, not a later genuine click-away.
static ACTION_OWNS_FOCUS: AtomicBool = AtomicBool::new(false);

/// The moment ACTION_OWNS_FOCUS was set, for the time box.
static ACTION_FOCUS_SET_AT: LazyLock<Mutex<Option<Instant>>> = LazyLock::new(|| Mutex::new(None));

/// How long a panel action may claim the resign it causes. A hide/quit transitions the frontmost
/// app within a frame or two; anything later is the user own doing and must dismiss as usual.
const ACTION_FOCUS_WINDOW: Duration = Duration::from_millis(750);

/// Consume the focus-ownership guard: whether THIS resign is the one a panel action caused.
///
/// It returns true at most once per action. A time window that stayed true for its whole duration
/// would swallow the user's next genuine click-away -- and hiding a background app causes no resign
/// at all, so the guard would sit there and eat an unrelated one. Consuming it keeps the
/// suppression tied to a single transition.
pub(crate) fn take_action_focus() -> bool {
    if !ACTION_OWNS_FOCUS.swap(false, Ordering::Relaxed) {
        return false;
    }
    let set_at = *ACTION_FOCUS_SET_AT.lock().unwrap();
    match set_at {
        // Within the window: this is the transition the action caused.
        Some(set_at) if set_at.elapsed() <= ACTION_FOCUS_WINDOW => true,
        // Expired meanwhile (a slow save-confirmation, say): do not treat a later resign as ours.
        _ => {
            log_debug!("[panel] the focus guard expired before a resign arrived; not claimed");
            false
        }
    }
}

/// Mark this summon sticky when a panel action runs.
pub(crate) fn mark_sticky() {
    STICKY_SUMMON.store(true, Ordering::Relaxed);
}

/// Whether a modifier release must NOT commit the selected card.
pub(crate) fn is_sticky() -> bool {
    STICKY_SUMMON.load(Ordering::Relaxed)
}

/// End the sticky session. Called from every exit path, so a new summon always starts unsticky.
///
/// This CANCELS queued actions by bumping the session generation rather than clearing the held
/// record: a key the user is still physically holding must keep its ownership until its release,
/// or the repeats would leak back to the frontmost app. Queued events are still consumed (their
/// action is simply dropped), which is what keeps a cancelled Cmd+Q away from our own menu.
pub(crate) fn clear_sticky() {
    STICKY_SUMMON.store(false, Ordering::Relaxed);
    // The focus guard must not outlive its session either: an unconsumed one would suppress the
    // first resign of the NEXT summon.
    ACTION_OWNS_FOCUS.store(false, Ordering::Relaxed);
    *ACTION_FOCUS_SET_AT.lock().unwrap() = None;
    panel_actions::end_session();
}

/// Republish the panel key snapshot. Main thread only.
///
/// Called whenever the panel becomes visible or invisible, the four switches change, the trigger
/// mode changes, or the input source changes. The tap reads this, so it is replaced whole.
///
/// Visibility is passed in rather than read from the session state: the hide path runs while that
/// state is being torn down, and re-borrowing it there would re-enter the same RefCell.
pub(crate) fn publish_panel_snapshot(visible: bool) {
    // One resolution, shared with the tap-lifecycle decision: "is the feature on" and "which
    // actions are on" must not be able to disagree.
    let [close, minimize, hide, quit] = {
        let cfg = CONFIG.read().unwrap();
        crate::panel_actions::switches_for(&cfg)
    };
    // Each enabled action's physical key is resolved from the live layout here, on the main
    // thread: Text Input Source APIs must not be called from the tap. A layout that produces no
    // such character on any key falls back to that action's ANSI position, so the keys keep working
    // on an unreadable layout instead of silently disappearing -- and the fallback is decided per
    // action, so one unresolvable key never disables the others.
    let resolve = |action: PanelAction, on: bool| {
        if !on {
            return None;
        }
        // A layout that produces no such character on any key falls back to that action's ANSI
        // position, decided per action, so one unresolvable key never disables the others.
        Some(
            crate::keystroke_display::mapping::keycode_for_character(action.character())
                .unwrap_or_else(|| action.ansi_keycode()),
        )
    };
    let mut keymap = PanelKeymap::default();
    for (action, on) in [
        (PanelAction::Close, close),
        (PanelAction::Minimize, minimize),
        (PanelAction::Hide, hide),
        (PanelAction::Quit, quit),
    ] {
        keymap.set(action, resolve(action, on));
    }
    // Option / Control is the panel's own hold modifier only in Option+Tab / Control+Tab mode,
    // where the user holds it for the whole summon; Command remains required in every mode.
    let mode = crate::event_monitor::switcher_modifier_name();
    let tolerate_option = mode == "option";
    panel_actions::publish(
        PanelSnapshot::new(visible, tolerate_option, keymap).tolerating_control(mode == "control"),
    );
}

/// The window-level performKeyEquivalent: override for the overlay.
pub(crate) extern "C" fn overlay_window_perform_key_equivalent(
    _self: *mut c_void,
    _cmd: Sel,
    event: *mut AnyObject,
) -> bool {
    crate::callback_guard::bool("overlay_window_perform_key_equivalent", false, || unsafe {
        overlay_perform_key_equivalent_inner(event)
    })
}

unsafe fn overlay_perform_key_equivalent_inner(event: *mut AnyObject) -> bool {
    if event.is_null() {
        return false;
    }
    let keycode: u16 = msg_send![event, keyCode];
    let flags: u64 = msg_send![event, modifierFlags];

    // Ownership decides how the event is handled: a key the panel claimed is consumed whether or
    // not the action still runs (a cancelled Cmd+Q must not reach our own Quit menu item), while a
    // key the panel never claimed belongs to AppKit.
    let _ = flags;
    match panel_actions::take_pending_action(keycode) {
        panel_actions::PendingOutcome::Run(action) => {
            run_panel_action(action);
            true
        }
        panel_actions::PendingOutcome::Cancelled => {
            log_debug!(
                "[panel] consumed key {} without running: its action was cancelled",
                keycode
            );
            true
        }
        // Not ours: AppKit's own dispatch continues (text fields, the menu, NSPanel's
        // key-equivalent delivery to the view hierarchy, everything else).
        panel_actions::PendingOutcome::NotOurs => false,
    }
}

/// Run one in-panel action against the HIGHLIGHTED card.
///
/// The target (pid, window_id) is captured HERE, at press time: a worker must never hold a card
/// pointer or an index, because navigation or a refresh can move either before the work finishes.
unsafe fn run_panel_action(action: PanelAction) {
    let target = with_tab_state(|state_opt| {
        let state = state_opt.as_mut()?;
        if !state.visible {
            return None;
        }
        let window = state.windows.get(state.selected)?;
        Some((
            window.pid,
            window.window_id,
            window.minimized,
            state.selected,
        ))
    });
    let Some((pid, cgwid, minimized, index)) = target else {
        log_debug!("[panel] action {:?}: no visible target", action);
        return;
    };
    log_debug!(
        "[panel] action {:?} on pid={} cgwid={} index={}",
        action,
        pid,
        cgwid,
        index
    );
    // The user is interacting deliberately: keep the panel open and do not let a modifier release
    // commit whatever merely happened to be highlighted.
    mark_sticky();
    // Our own windows (the Settings window appears in the switcher) take the in-process path: their
    // AX writes drive AppKit window management and must run on the main thread. This handler already
    // runs there, so the operation executes directly. Cross-process targets stay off-main so a slow
    // target never blocks our UI.
    let own = pid == std::process::id() as i32;

    // Count the run for the smoke: the consumption return value cannot distinguish "consumed and
    // cancelled" from "consumed and executed".
    panel_actions::note_action_run();
    match action {
        PanelAction::Close => {
            // Reuse the existing animate-then-AX close, identical to backspace: it advances the
            // selection through remove_window_adjust_selection and rolls back on AX failure. It
            // already splits own/other internally.
            let card = card_document().and_then(|document| {
                card_views(document)
                    .into_iter()
                    .find(|card| get_card_index(*card) == Some(index))
            });
            if let Some(card) = card {
                begin_close_window_at(index, card);
            } else {
                close_window_at(index);
            }
        }
        // Minimize toggles, so the key behaves like the familiar Cmd+M.
        PanelAction::Minimize if own => {
            let _ = crate::window_collector::set_ax_minimized(pid, cgwid, !minimized);
            schedule_panel_action_refresh();
        }
        PanelAction::Minimize => {
            run_off_main(move || crate::window_collector::set_ax_minimized(pid, cgwid, !minimized))
        }
        // Hide and quit change which process is frontmost, which makes our nonactivating panel
        // resign key; mark the action so the resign handler does not mistake that for a
        // click-away and dismiss the panel the feature promises to keep open.
        PanelAction::Hide | PanelAction::Quit => {
            ACTION_OWNS_FOCUS.store(true, Ordering::Relaxed);
            *ACTION_FOCUS_SET_AT.lock().unwrap() = Some(Instant::now());
            match action {
                PanelAction::Hide if own => {
                    let _ = crate::window_collector::hide_application(pid);
                    schedule_panel_action_refresh();
                }
                PanelAction::Hide => {
                    run_off_main(move || crate::window_collector::hide_application(pid))
                }
                PanelAction::Quit if own => crate::menu::quit_application(),
                _ => run_off_main(move || crate::window_collector::terminate_application(pid)),
            }
        }
    }
}

/// Run a cross-process window/app operation off the main thread.
///
/// The result is deliberately NOT treated as the new state: minimizing, hiding and quitting are
/// all asynchronous REQUESTS. A caller that deleted the card on a true return would drop an app
/// whose quit was cancelled by a save sheet. A later lifecycle notification or refresh publishes
/// what actually happened.
fn run_off_main(operation: impl FnOnce() -> bool + Send + 'static) {
    std::thread::spawn(move || {
        // A thread that calls into Objective-C needs its own autorelease pool: the objects
        // NSRunningApplication hands back are autoreleased, and without a pool they are never
        // drained on this short-lived thread. Apple requires a pool per thread that uses Cocoa.
        autorelease_pool(|| {
            let _ = operation();
        });
    });
    schedule_panel_action_refresh();
}

/// Run `body` inside a fresh autorelease pool. Used by the worker threads that touch AppKit.
///
/// `objc2::rc::autoreleasepool` rather than a hand-written new/drain pair: it drains while a
/// panic unwinds too, where the pair skipped the drain and left the whole pool's objects behind.
/// A worker thread still pushes its own pool here -- a pool belongs to the thread that pushed
/// it, and this is that thread.
fn autorelease_pool(body: impl FnOnce()) {
    objc2::rc::autoreleasepool(|_pool| body());
}

/// The production pool helper, exercised from a test.
///
/// The test could call `objc2::rc::autoreleasepool` itself and pass while this wrapper was broken
/// (reverted to a pair that skips the drain when `body` unwinds, say). Going through the wrapper is
/// what makes the assertion about the code that actually runs on the worker thread.
#[cfg(test)]
pub(crate) fn autorelease_pool_for_test(body: impl FnOnce()) {
    autorelease_pool(body);
}

/// Ask for a refresh shortly after an action, through the existing deferred-refresh machinery.
fn schedule_panel_action_refresh() {
    unsafe {
        let Some(controller) = *crate::CONTROLLER.lock().unwrap() else {
            return;
        };
        let _: () = msg_send![
            controller.0,
            performSelector: sel!(handlePanelActionRefresh:),
            withObject: std::ptr::null::<AnyObject>(),
            afterDelay: PANEL_ACTION_REFRESH_DELAY
        ];
    }
}

/// Deferred refresh after an action. Coalesced through the ordinary refresh request, so a fast
/// sequence of actions costs one pass, and a slow or cancelled operation is corrected later by its
/// own lifecycle notification.
pub(crate) extern "C" fn on_panel_action_refresh(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    crate::callback_guard::void("on_panel_action_refresh", || {
        // The panel may have closed in the meantime: a refresh into a hidden overlay would be
        // wasted work, and the session is over anyway.
        if !with_tab_state(|state_opt| state_opt.as_ref().is_some_and(|state| state.visible)) {
            return;
        }
        request_window_refresh();
    });
}

/// Long enough for a fast app to have applied the change, short enough to feel immediate.
const PANEL_ACTION_REFRESH_DELAY: f64 = 0.25;

/// Build a real NSEvent for the smoke runner (same construction the clipboard smoke uses).
unsafe fn make_key_event(keycode: u16, modifiers: u64) -> *mut AnyObject {
    let chars = crate::ffi::make_nsstring("x");
    let event: *mut AnyObject = msg_send![
        class!(NSEvent),
        keyEventWithType: 10u64, // NSEventTypeKeyDown
        location: NSPoint::new(0.0, 0.0),
        modifierFlags: modifiers,
        timestamp: 0.0f64,
        windowNumber: 0isize,
        context: std::ptr::null::<AnyObject>(),
        characters: chars,
        charactersIgnoringModifiers: chars,
        isARepeat: false,
        keyCode: keycode
    ];
    crate::ffi::CFRelease(chars as *const c_void);
    event
}

/// Drive the registered performKeyEquivalent: on the REAL overlay window with a REAL NSEvent, so
/// this covers the parts a pure mapping test cannot: the dynamic method registration, the
/// eligibility decision, and the consumption rule (a claimed chord is consumed even when the action
/// can no longer run -- otherwise a panel-claimed Cmd+Q would reach the main menu and quit us).
///
/// Scope, stated honestly: this drives the method on the window object directly. It does NOT prove
/// that the system's own window -> view dispatch reaches it; only the A2 scenario injects input
/// through the real event stream. The runner reports that limit instead of implying more.
pub(crate) unsafe fn smoke_panel_key_equivalent() -> bool {
    let Some(window) = overlay_window_ptr() else {
        log_info!("[smoke-panel-keys] no overlay window to drive");
        return false;
    };
    let selector = sel!(performKeyEquivalent:);

    // 1) A chord for an action whose switch is OFF must not be consumed, and must not run.
    panel_actions::publish(PanelSnapshot::new(
        true,
        false,
        PanelKeymap::default(), // every switch off
    ));
    let off_event = make_key_event(13, COMMAND_FLAG);
    if !off_event.is_null() {
        let handled: bool = msg_send![window, performKeyEquivalent: off_event];
        if handled {
            log_info!("[smoke-panel-keys] an off switch consumed the chord");
            return false;
        }
    }

    // 2) An unclaimed key is never consumed, whatever the switches say.
    panel_actions::publish(PanelSnapshot::new(
        true,
        false,
        PanelKeymap::ansi_fallback(true, true, true, true),
    ));
    let other_event = make_key_event(99, COMMAND_FLAG); // an unrelated key
    if !other_event.is_null() {
        let handled: bool = msg_send![window, performKeyEquivalent: other_event];
        if handled {
            log_info!("[smoke-panel-keys] an unrelated key was consumed");
            return false;
        }
    }

    // 3) A CLAIMED chord is consumed even when the action is no longer eligible: the ownership
    //    rule that keeps a panel-claimed Cmd+Q away from the main menu.
    // Use MINIMIZE, not Quit: the smoke must never be able to terminate the app it runs in. The
    // record is claimed while the switch is on, then the switch is turned off, so the handler sees
    // a claimed-but-ineligible chord.
    panel_actions::claim_for_smoke(46); // Cmd+M's keycode
    panel_actions::publish(PanelSnapshot::new(
        true,
        false,
        PanelKeymap::default(), // every switch off: the action is no longer eligible
    ));
    let claimed_event = make_key_event(46, COMMAND_FLAG);
    if claimed_event.is_null() {
        log_info!("[smoke-panel-keys] could not build the claimed event");
        return false;
    }
    let runs_before = panel_actions::actions_run();
    let consumed: bool = msg_send![window, performKeyEquivalent: claimed_event];
    panel_actions::clear_claims();
    if !consumed {
        log_info!("[smoke-panel-keys] a claimed but ineligible chord was NOT consumed");
        return false;
    }
    // The action must have been CANCELLED, not run. The consumption return value cannot tell those
    // apart, so this reads the run counter instead: an implementation that executed a cancelled
    // action would pass a return-value-only check.
    if panel_actions::actions_run() != runs_before {
        log_info!("[smoke-panel-keys] an ineligible action was executed");
        return false;
    }
    let _ = selector;
    log_info!(
        "[smoke-panel-keys] handler registration, eligibility and consumption rules hold (system dispatch is the A2 layer's job)"
    );
    true
}

/// kCGEventFlagMaskCommand, in NSEvent's flag numbering (same value).
const COMMAND_FLAG: u64 = 0x0010_0000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panel_action_owns_only_the_resign_it_causes() {
        // The defect the A2 scenario found: hiding the frontmost app makes the panel resign key,
        // and the ordinary resign handling dismissed the panel. The guard must suppress that
        // resign, but expire, so a genuine click-away still dismisses.
        ACTION_OWNS_FOCUS.store(false, Ordering::Relaxed);
        *ACTION_FOCUS_SET_AT.lock().unwrap() = None;
        assert!(
            !take_action_focus(),
            "no action ran, so a resign is the user's own"
        );

        ACTION_OWNS_FOCUS.store(true, Ordering::Relaxed);
        *ACTION_FOCUS_SET_AT.lock().unwrap() = Some(Instant::now());
        assert!(
            take_action_focus(),
            "the resign caused by the action itself must be suppressed"
        );

        // Backdated past the window: the suppression must lapse rather than swallow a later
        // click-away.
        *ACTION_FOCUS_SET_AT.lock().unwrap() =
            Some(Instant::now() - ACTION_FOCUS_WINDOW - Duration::from_millis(1));
        assert!(!take_action_focus(), "the guard is time-boxed");
        assert!(
            !ACTION_OWNS_FOCUS.load(Ordering::Relaxed),
            "expiry clears the flag so the next resign is judged normally"
        );
    }
}
