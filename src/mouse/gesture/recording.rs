//! Interactive gesture recording: capture one drag and turn it into a sequence.
//!
//! The recording runs on the settings side, not inside the gesture engine: the tap thread only
//! forwards the raw samples (see `src/settings/gesture_recorder.rs`), and this module owns the
//! state machine a person can see -- "press a button", "draw", "done", "cancelled".
//!
//! Why not reuse the engine: a recorded gesture must be visible while it is being drawn, and the
//! engine deliberately draws nothing until a press is ENGAGED (a stray click must never flash a
//! trail). Recording is the one place where showing the stroke from the first point is the point.

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Mutex;

use crate::mouse::gesture::recognizer::{Point, Sequence};

/// Where a recording is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stage {
    Idle,
    /// Waiting for the user to press one of the trigger buttons.
    WaitingPress,
    /// A button is held and samples are being collected.
    Drawing,
    /// The button came up: the sequence is ready to be accepted.
    Finished,
}

static STAGE: AtomicU8 = AtomicU8::new(0);
/// Set while a recording owns the pointer: the gesture runtime must not also claim the press.
static OWNING_BUTTON: AtomicU8 = AtomicU8::new(0);
static RESULT: Mutex<Option<Finished>> = Mutex::new(None);

/// A finished recording, published and consumed as one value: the sequence, the button it was drawn
/// with and the session it belongs to. Publishing them separately let the main thread observe a
/// sequence with no button (or a button left from the previous recording).
#[derive(Debug, Clone)]
pub(crate) struct Finished {
    pub(crate) sequence: Sequence,
    pub(crate) button: u32,
    pub(crate) session: u64,
}

fn stage_from_u8(value: u8) -> Stage {
    match value {
        1 => Stage::WaitingPress,
        2 => Stage::Drawing,
        3 => Stage::Finished,
        _ => Stage::Idle,
    }
}

fn u8_from_stage(stage: Stage) -> u8 {
    match stage {
        Stage::Idle => 0,
        Stage::WaitingPress => 1,
        Stage::Drawing => 2,
        Stage::Finished => 3,
    }
}

pub(crate) fn stage() -> Stage {
    stage_from_u8(STAGE.load(Ordering::SeqCst))
}

pub(crate) fn is_recording() -> bool {
    stage() != Stage::Idle
}

/// Begin waiting for a press. ### owning the pointer
///
/// The mouse tap runs on its own thread and would otherwise let the gesture engine claim the same
/// press, so the recorder announces itself: while `recording` is true the engine refuses every
/// press, exactly as it does for the settings' own button recorder.
/// Whether the settings side has finished arming the recording it just asked for.
///
/// `begin` is called from a settings callback, and that callback still holds the settings window's
/// RefCell when it returns. The tap thread reads the stage atomically, so a real stroke started the
/// instant the user reads "press a button" would be fed into a window that is busy -- its preview
/// would be dropped. The recorder therefore swallows the press and waits for [[mark_ui_ready]].
static UI_READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub(crate) fn begin() {
    UI_READY.store(false, Ordering::SeqCst);
    SESSION.fetch_add(1, Ordering::SeqCst);
    *RESULT.lock().unwrap() = None;
    OWNING_BUTTON.store(0, Ordering::SeqCst);
    STAGE.store(u8_from_stage(Stage::WaitingPress), Ordering::SeqCst);
    crate::mouse::event_tap::RECORDING.store(true, Ordering::SeqCst);
}

/// The settings callback that armed the recording has returned: its samples may be taken now.
pub(crate) fn mark_ui_ready() {
    UI_READY.store(true, Ordering::SeqCst);
}

/// Stop recording and forget whatever was collected.
pub(crate) fn cancel() {
    // A cancelled recording never reaches its armed state, so a later press must not be taken by
    // the delay [[UI_READY]] exists for.
    UI_READY.store(true, Ordering::SeqCst);
    // Advances the session: any result the tap thread published for the cancelled session is
    // now stale and the consumer will refuse it.
    SESSION.fetch_add(1, Ordering::SeqCst);
    *RESULT.lock().unwrap() = None;
    // The button whose Press this recording swallowed keeps its ownership until its release: letting
    // the Up through would hand the application a release for a press it never saw.
    if stage() == Stage::Drawing {
        OWNING_BUTTON.store(OWNING_BUTTON.load(Ordering::SeqCst), Ordering::SeqCst);
    } else {
        OWNING_BUTTON.store(0, Ordering::SeqCst);
    }
    STAGE.store(u8_from_stage(Stage::Idle), Ordering::SeqCst);
    crate::mouse::event_tap::RECORDING.store(false, Ordering::SeqCst);
}

/// Finish recording: the sequence stays available through [[take_result]].
pub(crate) fn finish() {
    OWNING_BUTTON.store(0, Ordering::SeqCst);
    STAGE.store(u8_from_stage(Stage::Finished), Ordering::SeqCst);
    crate::mouse::event_tap::RECORDING.store(false, Ordering::SeqCst);
}

/// Whether a finished recording is waiting for the main thread.
pub(crate) fn has_pending_result() -> bool {
    RESULT.lock().map(|slot| slot.is_some()).unwrap_or(false)
}

/// Take the recorded sequence, ending the recording.
pub(crate) fn take_result() -> Option<Finished> {
    // Consuming a result ENDS the recording, but must not invalidate the session: the caller
    // is about to commit exactly this result, and bumping the session here would make its own
    // staleness check reject it.
    let result = RESULT.lock().unwrap().take();
    OWNING_BUTTON.store(0, Ordering::SeqCst);
    STAGE.store(u8_from_stage(Stage::Idle), Ordering::SeqCst);
    crate::mouse::event_tap::RECORDING.store(false, Ordering::SeqCst);
    result
}

/// The recording session, so a result published by one recording cannot be committed into the next.
static SESSION: AtomicU64 = AtomicU64::new(1);

pub(crate) fn session() -> u64 {
    SESSION.load(Ordering::SeqCst)
}

/// A press forwarded from the tap. Returns whether the recorder consumed it.
pub(crate) fn on_press(button: u32) -> bool {
    match stage() {
        // An armed recorder takes the press even while it is still being set up: letting it
        // through would hand the frontmost application a click the user aimed at the recorder.
        Stage::WaitingPress if !UI_READY.load(Ordering::SeqCst) => true,
        Stage::WaitingPress => {
            OWNING_BUTTON.store(button.min(255) as u8, Ordering::SeqCst);
            STAGE.store(u8_from_stage(Stage::Drawing), Ordering::SeqCst);
            true
        }
        _ => false,
    }
}

/// A drag sample forwarded from the tap, while `button` is the one being recorded.
///
/// The sample goes into the buffer even when the button is not the recorded one: a recording is one
/// press, and a second button pressed meanwhile must not corrupt it.
pub(crate) fn on_drag(button: u32, point: Point) -> bool {
    if stage() != Stage::Drawing || OWNING_BUTTON.load(Ordering::SeqCst) as u32 != button {
        return false;
    }
    crate::settings::gesture_recorder::feed(point);
    true
}

/// A release forwarded from the tap.
pub(crate) fn on_release(button: u32) -> bool {
    // The tail check comes first: a cancelled recording still owns the Up of the press it took.
    if OWNING_BUTTON.load(Ordering::SeqCst) as u32 != button || button == 0 {
        return false;
    }
    if stage() != Stage::Drawing {
        OWNING_BUTTON.store(0, Ordering::SeqCst);
        return true;
    }
    let sequence = crate::settings::gesture_recorder::finish();
    let empty = sequence
        .as_ref()
        .map(|s| s.to_key().is_empty())
        .unwrap_or(true);
    if empty {
        // Nothing was drawn: the recording stays armed so a stray click does not end it.
        STAGE.store(u8_from_stage(Stage::WaitingPress), Ordering::SeqCst);
        crate::settings::gesture_recorder::reset();
        return true;
    }
    let Some(sequence) = sequence else {
        return true;
    };
    let button = OWNING_BUTTON.load(Ordering::SeqCst) as u32;
    let session = SESSION.load(Ordering::SeqCst);
    *RESULT.lock().unwrap() = Some(Finished {
        sequence,
        button,
        session,
    });
    finish();
    // This runs on the tap thread, so all it may do is publish: the main thread validates the
    // result against the live configuration and writes it there.
    crate::settings::gesture_recorder::publish_result();
    true
}

/// The tap's entry point for a press. Returns true when the recorder took it -- the engine and the
/// button mappings must then leave the event alone.
pub(crate) fn tap_press(button: u32, point: Point) -> bool {
    crate::log_debug!(
        "[gesture] recorder saw press button={} stage={:?}",
        button,
        stage()
    );
    if !on_press(button) {
        return false;
    }
    crate::settings::gesture_recorder::begin_at(point);
    crate::settings::gesture_recorder::request_ui_refresh();
    true
}

/// Ask for one preview redraw. Coalesced by the HUD scheduler, so a fast drag costs one main-thread
/// task at a time instead of one per sample.
pub(crate) fn request_preview() {
    crate::schedule_gesture_hud();
}

/// The tap's entry point for a drag sample.
pub(crate) fn tap_drag(button: u32, point: Point) -> bool {
    on_drag(button, point)
}

/// Whether an Up for \`button\` ends a press this recording swallowed. True even after the recording was
/// cancelled: the press was taken, so its release is ours to consume.
pub(crate) fn owns_tail(button: u32) -> bool {
    let owned = OWNING_BUTTON.load(Ordering::SeqCst) as u32;
    button != 0 && owned == button
}

/// The tap's entry point for a release.
pub(crate) fn tap_release(button: u32) -> bool {
    on_release(button)
}
