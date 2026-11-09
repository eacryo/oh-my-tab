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
    // The press latch is NOT cleared here: a refused learning ends at its DOWN (the capture is over)
    // while the user may still be holding that button, and its tail has to stay owned until it is
    // released -- starting another capture must not turn that release into a bare one.
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
        .map(|presses| {
            presses
                .iter()
                .any(|press| press.button == button && press.device == device)
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
fn take_gesture_result() -> Option<(u64, u32, bool)> {
    GESTURE_RESULT.lock().unwrap().take()
}

/// Whether a learned press was REFUSED, so its tail must be swallowed with it.
///
/// An accepted learning passes the whole press through (the application is owed the click). A
/// refused one was swallowed at the down, so its up and its drags must go too: a bare release the
/// application never saw the press of is not owed to it, and the mapping that caused the refusal
/// must not fire after the app has already said it will not learn that button.
pub(crate) fn learn_press_refused(button: u32, device: u64) -> bool {
    LEARN_PRESS
        .lock()
        .map(|presses| {
            presses
                .iter()
                .any(|press| press.refused && press.button == button && press.device == device)
        })
        .unwrap_or(false)
}

/// Whether learning `button` RIGHT NOW would be refused, answered from the runtime's published
/// state: the recording tap cannot read the configuration, so it asks the gesture table the tap
/// consults and the mouse resolution the mapping path uses -- the same two facts the page's check
/// is built from.
///
/// The LAYER is the capture's own target device, fixed at `begin_trigger_capture`, not the device
/// the press came from: the commit writes the capture's layer, so a refusal decided against another
/// mouse's layer would swallow a press the commit then accepts (or accept one it refuses).
pub(crate) fn learn_would_refuse_now(button: u32) -> bool {
    let Some(capture) = pending_capture() else {
        return false;
    };
    let device = capture.device;
    // The layer is ACTIVE (it has rows the tap would run), not "this button is already its
    // trigger": learning a NEW button is exactly the case where the candidate is not claimed yet.
    let published = crate::mouse::gesture::table::snapshot();
    let resolved = crate::mouse::resolve::resolve(device);
    crate::mouse::gesture::conflict::hotkey_conflicts_with_mapping(
        crate::mouse::gesture::conflict::layer_is_active(published.table_for(device)),
        crate::mouse::gesture::conflict::mapping_runs_for(&resolved, button).as_deref(),
    )
}

/// The release of the press being learned: it has been passed through, so the latch goes.
///
/// The DEVICE is part of the identity: two mice can be holding the same button number, and freeing
/// one must not drop the other's tail ownership.
pub(crate) fn learn_press_released(button: u32, device: u64) {
    if let Ok(mut presses) = LEARN_PRESS.lock() {
        presses.retain(|press| !(press.button == button && press.device == device));
    }
}

/// Observe a mouse button DOWN from the recording tap. The event is never swallowed.
///
/// `false` means "not ours": the recording tap passes the event and keeps waiting.
pub(crate) fn observe_button_down(button: u32, device: u64, refused: bool) -> bool {
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
    *GESTURE_RESULT.lock().unwrap() = Some((capture.session, button, refused));
    if let Ok(mut presses) = LEARN_PRESS.lock() {
        // One entry per button and device: a second press of the same button replaces the first,
        // and a press whose release was lost is bounded rather than kept for the life of the
        // process. A DIFFERENT button's held press stays owned: it may still be refused and held
        // while this capture runs.
        presses.retain(|press| !(press.button == button && press.device == device));
        if presses.len() >= MAX_HELD_LEARN_PRESSES {
            presses.remove(0);
        }
        presses.push(LearnPress {
            button,
            device,
            refused,
        });
    }
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
        let Some((session, button, refused)) = take_gesture_result() else {
            log_debug!("[gesture] completion with no learned button");
            return;
        };
        if refused {
            // The press was refused AND swallowed while it was learned: nothing is written, the row
            // keeps its hotkey, and the bubble says why. The capture session is dropped with it.
            log_debug!("[gesture] trigger {button} refused: the button already carries a mapping");
            let _ = take_capture_for_session(session);
            dispatch::refresh_gesture_hotkey_control();
            crate::ui::toast::show_refused(&t("errors.gestures_hotkey_mapping_conflict"));
            return;
        }
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
    // The layer the capture started for, by identity: a device layer that no longer exists is not
    // recreated (the user deleted it), and the refusal below has to ask about THAT layer.
    let matcher = match device {
        None => None,
        Some(device) if device == crate::mouse::device::VIRTUAL_DEVICE_KEY => {
            Some(crate::config::DeviceMatcher {
                injected: Some(true),
                ..Default::default()
            })
        }
        Some((vendor_id, product_id)) => Some(crate::config::DeviceMatcher {
            vendor_id: Some(vendor_id),
            product_id: Some(product_id),
            ..Default::default()
        }),
    };
    let layer = match &matcher {
        None => cfg.mouse.shared_gesture_layer(),
        Some(matcher) => {
            let Some(layer) = cfg
                .mouse
                .gesture_devices
                .iter()
                .find(|l| l.device == *matcher)
            else {
                log_debug!("[gesture] the gesture layer is gone; result dropped");
                return;
            };
            layer.clone()
        }
    };
    // A button that already carries a mapping is REFUSED, the way the clipboard refuses a chord a
    // binding owns: the hotkey keeps its old button and the shared bubble says why. The check is
    // the standing warning's rule asked about this button, so the two cannot disagree.
    if crate::mouse::gesture::conflict::learning_would_shadow_mapping(&layer, &cfg, device, button)
    {
        log_debug!("[gesture] trigger {button} refused: the button already carries a mapping");
        // Nothing is written: the row keeps the hotkey it had, so put its idle copy and ring back.
        dispatch::refresh_gesture_hotkey_control();
        crate::ui::toast::show_refused(&t("errors.gestures_hotkey_mapping_conflict"));
        return;
    }
    match &matcher {
        None => cfg.mouse.gesture_button = trigger,
        Some(matcher) => {
            let Some(layer) = cfg
                .mouse
                .gesture_devices
                .iter_mut()
                .find(|layer| layer.device == *matcher)
            else {
                log_debug!("[gesture] the gesture layer is gone; result dropped");
                return;
            };
            layer.gesture_button = trigger;
        }
    }
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
        *GESTURE_RESULT.lock().unwrap() = Some((3, 1, false));
        // The up path: the pass-through latch goes, the result does not.
        LEARN_PRESS.lock().unwrap().clear();
        let (session, button, refused) =
            take_gesture_result().expect("the completion has a result");
        assert_eq!((session, button, refused), (3, 1, false));
        let capture = take_capture_for_session(session).expect("the capture it belongs to");
        assert_eq!(capture.session, 3);
        assert!(
            pending_capture().is_none(),
            "the slot is cleared by the commit"
        );
    }

    /// A refused press owns its tail until it is released, so its up and drags are swallowed with
    /// the down it never let through.
    #[test]
    fn a_refused_press_owns_its_tail_until_released() {
        let _guard = TEST_LOCK.lock().unwrap();
        LEARN_PRESS.lock().unwrap().push(LearnPress {
            button: 3,
            device: 7,
            refused: true,
        });
        assert!(learn_press_refused(3, 7), "the held press is refused");
        assert!(!learn_press_refused(2, 7), "another button is not");
        assert!(!learn_press_refused(3, 8), "another device is not");
        learn_press_released(3, 7);
        assert!(
            !learn_press_owned(3, 7),
            "the release ends the ownership of the tail"
        );
    }

    /// A second capture must not take the tail of a press that is still held.
    ///
    /// The reachable order: a side-button learning is refused and the user keeps holding it, another
    /// capture starts on the recorder button, a DIFFERENT button is pressed for it, and then the
    /// first button is released. With one latch slot the second press overwrote the first and its
    /// release escaped the swallow; every held press keeps its own entry until its own release.
    #[test]
    fn a_later_capture_does_not_take_over_a_held_refused_press() {
        let _guard = TEST_LOCK.lock().unwrap();
        LEARN_PRESS.lock().unwrap().push(LearnPress {
            button: 3,
            device: 7,
            refused: true,
        });
        // The next capture's press is a different button.
        LEARN_PRESS.lock().unwrap().push(LearnPress {
            button: 2,
            device: 7,
            refused: false,
        });
        assert!(
            learn_press_refused(3, 7),
            "the first, refused press still owns its tail"
        );
        assert!(
            !learn_press_refused(2, 7),
            "the accepted one is not refused"
        );
        learn_press_released(2, 7);
        assert!(
            learn_press_refused(3, 7),
            "releasing the second press must not free the first"
        );
        learn_press_released(3, 7);
        assert!(LEARN_PRESS.lock().unwrap().is_empty());
    }

    /// Two mice can hold the SAME button number: freeing one must not drop the other's tail.
    #[test]
    fn releasing_one_mouse_keeps_the_other_mouses_tail() {
        let _guard = TEST_LOCK.lock().unwrap();
        LEARN_PRESS.lock().unwrap().clear();
        for device in [7u64, 8u64] {
            LEARN_PRESS.lock().unwrap().push(LearnPress {
                button: 3,
                device,
                refused: true,
            });
        }
        learn_press_released(3, 7);
        assert!(
            !learn_press_refused(3, 7),
            "the released mouse no longer owns its tail"
        );
        assert!(
            learn_press_refused(3, 8),
            "the other mouse still holds the same button number"
        );
        learn_press_released(3, 8);
        assert!(LEARN_PRESS.lock().unwrap().is_empty());
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
        *GESTURE_RESULT.lock().unwrap() = Some((11, 1, false));
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
