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
    let Some(capture) = GESTURE_CAPTURE.lock().unwrap().take() else {
        return false;
    };
    *REC_BUTTON.lock().unwrap() = button;
    *LEARN_PRESS.lock().unwrap() = Some(LearnPress {
        session: capture.session,
        button,
        device,
    });
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

/// The main-thread completion: commit the learned button to the layer the capture started on.
pub(crate) extern "C" fn handle_gesture_trigger_captured(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    crate::callback_guard::void("handle_gesture_trigger_captured", || {
        let button = *REC_BUTTON.lock().unwrap();
        let session = LEARN_PRESS
            .lock()
            .unwrap()
            .as_ref()
            .map(|press| press.session)
            .unwrap_or(0);
        let capture = *GESTURE_CAPTURE.lock().unwrap();
        let Some(capture) = capture.filter(|capture| capture.session == session) else {
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
    log_debug!("[gesture] trigger learned: button {button}");
}
