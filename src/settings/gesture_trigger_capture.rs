//! Learning the Gestures page's trigger button.
//!
//! The user asks the page to learn a button, presses one on their mouse, and the page stores it as
//! that layer's trigger. Three rules shape this file:
//!
//! * The press being learned PASSES THROUGH. The application underneath must still see the click the
//!   user made. The whole press (down, drags, up) is passed until the button is released, even when
//!   the capture was cancelled in between -- the gesture path and the button mappings only see a
//!   complete pair.
//! * The layer is fixed at START, by stable identity. A config reload during the capture must not
//!   redirect the result to another mouse, so the result carries the session it was recorded for
//!   and a late one is dropped.
//! * The commit happens on the main thread, through the same config-change path as any other
//!   settings edit.

use super::*;

/// Start learning a trigger for the layer the Gestures page is editing.
///
/// Refuses while another recorder is running: they share one temporary tap and one `RECORDING`
/// flag, and two sessions on that state would each clear the other's.
pub(crate) fn begin_trigger_capture() {
    if *REC_STAGE.lock().unwrap() != RecStage::Idle {
        return;
    }
    let device = current_selected_device();
    let session = GESTURE_CAPTURE_SESSION.fetch_add(1, Ordering::SeqCst) + 1;
    *GESTURE_CAPTURE.lock().unwrap() = Some(GestureCapture { session, device });
    *LEARN_PRESS.lock().unwrap() = None;
    *REC_BUTTON.lock().unwrap() = 0;
    REC_COMPLETED_DESC.lock().unwrap().clear();
    *REC_MODE.lock().unwrap() = RecMode::GestureTrigger;
    *REC_STAGE.lock().unwrap() = RecStage::WaitingButton;
    REC_CANCEL.store(false, Ordering::SeqCst);
    // The gesture path and the button mappings stand down while the capture runs, exactly as
    // they do for the panel's own recorder.
    crate::mouse::event_tap::RECORDING.store(true, Ordering::Relaxed);
    page_view::set_gesture_capture_waiting(true);
    dispatch::refresh_gesture_hotkey_control();
    log_debug!("[gesture] waiting for the trigger button");
    // The shared recorder tap observes the press; this mode ends the capture AT the down, and the
    // press itself keeps passing through until it is released (see observe_button_down).
    mapping::spawn_recording_thread();
}

/// Abandon the capture. A press already being learned still passes through until it is released.
pub(crate) fn cancel_trigger_capture(reason: &str) {
    if *REC_MODE.lock().unwrap() != RecMode::GestureTrigger {
        return;
    }
    *GESTURE_CAPTURE.lock().unwrap() = None;
    log_debug!("[gesture] trigger capture cancelled: {reason}");
    mapping::cancel_recording_from_main();
    page_view::set_gesture_capture_waiting(false);
    dispatch::refresh_gesture_hotkey_control();
}

/// Whether a learned press is still held: its drags and its up must be left alone.
pub(crate) fn learn_press_owned(button: u32, device: u64) -> bool {
    LEARN_PRESS
        .lock()
        .map(|press| {
            press
                .as_ref()
                .is_some_and(|press| press.button == button && press.device == device)
        })
        .unwrap_or(false)
}

/// The capture that is waiting for its press, if any.
///
/// A COPY of the slot, never the slot itself: learning a button must leave the capture where the
/// main-thread completion finds it. Consuming it here is what made every learned trigger report as
/// "late" and get dropped -- the press was seen, the capture ended, and nothing was committed.
fn pending_capture() -> Option<GestureCapture> {
    *GESTURE_CAPTURE.lock().unwrap()
}

/// Take the capture a result belongs to. `None` leaves the slot ALONE: a result from an older
/// session must not steal a capture that started after it.
fn take_capture_for_session(session: u64) -> Option<GestureCapture> {
    let mut slot = GESTURE_CAPTURE.lock().unwrap();
    if slot
        .as_ref()
        .is_none_or(|capture| capture.session != session)
    {
        return None;
    }
    slot.take()
}

/// Invalidate the capture from the ESCAPE that cancels it, on the recording thread.
///
/// The tap cannot wait for the main thread: a button down that lands between the Escape and the
/// main-thread teardown would otherwise complete the capture and commit a hotkey the user has just
/// cancelled. Dropping the session here makes that press report "not ours" and pass through.
pub(crate) fn abandon_gesture_capture() {
    *GESTURE_CAPTURE.lock().unwrap() = None;
    *GESTURE_RESULT.lock().unwrap() = None;
}

/// Take the completion result: the button and the session it was learned for.
fn take_gesture_result() -> Option<(u64, u32)> {
    GESTURE_RESULT.lock().unwrap().take()
}

/// The release of the press being learned: it has been passed through, so the latch goes.
pub(crate) fn learn_press_released(button: u32) {
    let mut press = LEARN_PRESS.lock().unwrap();
    if press.as_ref().is_some_and(|press| press.button == button) {
        *press = None;
    }
}

/// Observe a mouse button DOWN from the recording tap. The event is never swallowed.
///
/// `false` means "not ours": the recording tap passes the event and keeps waiting.
pub(crate) fn observe_button_down(button: u32, device: u64) -> bool {
    if button == 0 {
        // The left button starts every ordinary click and can never be a gesture trigger.
        return false;
    }
    let Some(capture) = pending_capture() else {
        return false;
    };
    *REC_BUTTON.lock().unwrap() = button;
    // The RESULT carries the session to the main thread; the latch below only owns the pass-through
    // of the press, and the mouse-up clears it without touching the result.
    *GESTURE_RESULT.lock().unwrap() = Some((capture.session, button));
    *LEARN_PRESS.lock().unwrap() = Some(LearnPress { button, device });
    // The capture is COMPLETE at the down. The tap and the flags stand down now, but the press
    // stays latched, so its drags and its up are still passed through by the gesture path.
    mapping::finish_capture_now();
    page_view::set_gesture_capture_waiting(false);
    // The commit (main thread) redraws the page with the new hotkey on its button.
    // The commit is a config write: main thread, through the page's own apply path.
    mapping::notify_main(sel!(handleGestureTriggerCaptured:));
    log_debug!("[gesture] captured button {button}");
    true
}

/// The main-thread Escape cancel: the recording tap saw a bare Escape while a button was being
/// learned. It runs here because cancelling touches the page and the settings window.
pub(crate) extern "C" fn handle_gesture_trigger_capture_cancelled(
    _self: *mut c_void,
    _cmd: Sel,
    _arg: *mut c_void,
) {
    crate::callback_guard::void("handle_gesture_trigger_capture_cancelled", || {
        cancel_trigger_capture("escape");
    });
}

/// The main-thread completion: commit the learned button to the layer the capture started on.
pub(crate) extern "C" fn handle_gesture_trigger_captured(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    crate::callback_guard::void("handle_gesture_trigger_captured", || {
        let Some((session, button)) = take_gesture_result() else {
            log_debug!("[gesture] completion with no learned button");
            return;
        };
        let Some(capture) = take_capture_for_session(session) else {
            // A newer capture (or none): this result belongs to nothing.
            log_debug!("[gesture] late trigger result {button} (session {session}) dropped");
            return;
        };
        commit_trigger(button, capture.device);
    });
}

/// Write the learned trigger into the layer the capture was started for.
///
/// The layer is looked up by IDENTITY now, not by the index it had at start: the configuration may
/// have changed while the capture ran. A device layer that no longer exists is not recreated -- the
/// user deleted it, and recreating it would resurrect a choice they removed.
fn commit_trigger(button: u32, device: Option<crate::mouse::device::DeviceKey>) {
    let old = CONFIG.read().unwrap().clone();
    let mut cfg = old.clone();
    let trigger = crate::config::GestureTrigger::Learned {
        learned_button: button,
    };
    let matcher = match device {
        None => {
            cfg.mouse.gesture_button = trigger;
            finish_commit(&old, &cfg, button);
            return;
        }
        Some(device) if device == crate::mouse::device::VIRTUAL_DEVICE_KEY => {
            crate::config::DeviceMatcher {
                injected: Some(true),
                ..Default::default()
            }
        }
        Some((vendor_id, product_id)) => crate::config::DeviceMatcher {
            vendor_id: Some(vendor_id),
            product_id: Some(product_id),
            ..Default::default()
        },
    };
    let Some(layer) = cfg
        .mouse
        .gesture_devices
        .iter_mut()
        .find(|layer| layer.device == matcher)
    else {
        log_debug!("[gesture] the gesture layer is gone; result dropped");
        return;
    };
    layer.gesture_button = trigger;
    finish_commit(&old, &cfg, button);
}

/// Persist and publish a commit, and redraw the page on the row it changed.
fn finish_commit(old: &crate::config::Config, cfg: &crate::config::Config, button: u32) {
    if let Ok(mut w) = CONFIG.write() {
        *w = cfg.clone();
    }
    schedule_config_persist();
    apply_config_change(
        old,
        cfg,
        crate::runtime_config::ConfigChangeSource::Settings,
    );
    unsafe {
        crate::settings::window::rebuild_settings_content_now_preserving_scroll(
            SettingsPageId::Gestures,
        );
    }
    // Learning a hotkey onto a button that already carries a mapping is exactly when the mapping
    // stops running; say it in the shared bubble, the way the refused chords are said.
    crate::settings::dispatch::announce_gesture_mapping_conflict();
    log_debug!("[gesture] trigger learned: button {button}");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The capture state is process-global, so these tests must not interleave: a Mutex only
    /// serializes one access, and two tests writing the same slot in parallel made the gate flaky.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn capture_with(session: u64) -> GestureCapture {
        GestureCapture {
            session,
            device: None,
        }
    }

    /// A quick click still commits: the result must not come from the press latch.
    ///
    /// The bug this pins: the completion read its session from `LEARN_PRESS`, and the mouse-up
    /// releases that latch. A press whose up arrived before the main-thread callback reported
    /// session 0 and the learned button was dropped -- the same silent failure as before.
    #[test]
    fn a_quick_click_keeps_its_result_after_the_press_is_released() {
        let _guard = TEST_LOCK.lock().unwrap();
        *GESTURE_CAPTURE.lock().unwrap() = Some(capture_with(3));
        *GESTURE_RESULT.lock().unwrap() = Some((3, 1));
        // The up path: the pass-through latch goes, the result does not.
        *LEARN_PRESS.lock().unwrap() = None;
        let (session, button) = take_gesture_result().expect("the completion has a result");
        assert_eq!((session, button), (3, 1));
        let capture = take_capture_for_session(session).expect("the capture it belongs to");
        assert_eq!(capture.session, 3);
        assert!(
            pending_capture().is_none(),
            "the slot is cleared by the commit"
        );
    }

    /// Learning a button must not consume the capture: the completion still finds it.
    #[test]
    fn learning_a_button_leaves_the_capture_for_the_completion() {
        let _guard = TEST_LOCK.lock().unwrap();
        *GESTURE_CAPTURE.lock().unwrap() = Some(capture_with(7));
        // What the recording tap does on the press: it READS the capture.
        let seen = pending_capture().expect("the waiting capture");
        assert_eq!(seen.session, 7);
        let taken = take_capture_for_session(7).expect("the completion commits the capture");
        assert_eq!(taken.session, 7);
        assert!(
            pending_capture().is_none(),
            "the slot is cleared by the commit"
        );
    }

    /// A result from an older session must not steal a newer capture.
    #[test]
    fn a_stale_result_leaves_a_newer_capture_alone() {
        let _guard = TEST_LOCK.lock().unwrap();
        *GESTURE_CAPTURE.lock().unwrap() = Some(capture_with(9));
        assert!(take_capture_for_session(8).is_none());
        assert_eq!(
            pending_capture().map(|capture| capture.session),
            Some(9),
            "the newer capture is still waiting"
        );
        *GESTURE_CAPTURE.lock().unwrap() = None;
    }

    /// Escape invalidates the session before the main thread runs, so a press that lands in between
    /// cannot commit a hotkey the user cancelled.
    #[test]
    fn escape_abandons_the_capture_before_the_main_thread_runs() {
        let _guard = TEST_LOCK.lock().unwrap();
        *GESTURE_CAPTURE.lock().unwrap() = Some(capture_with(11));
        *GESTURE_RESULT.lock().unwrap() = Some((11, 1));
        abandon_gesture_capture();
        assert!(pending_capture().is_none(), "nothing waits to be learned");
        assert!(
            take_gesture_result().is_none(),
            "the queued result is dropped too"
        );
        assert!(
            take_capture_for_session(11).is_none(),
            "so a late press cannot commit"
        );
    }
}
