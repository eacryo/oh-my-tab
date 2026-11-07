//! The sample buffer behind an interactive recording, and the tap-thread hooks that feed it.
//!
//! Kept separate from [[super::recording]] so the state machine stays pure-ish: this file owns the
//! mutable recognizer, the live preview the settings window draws, and nothing else.

use std::sync::Mutex;
use std::sync::OnceLock;

use crate::mouse::gesture::recognizer::{Params, Point, Recognizer, Sequence};
use crate::mouse::gesture::recording::Stage;

/// The recognizer of the recording in progress.
static LIVE: OnceLock<Mutex<Option<Recognizer>>> = OnceLock::new();
/// The sequence drawn so far, for the preview line.
static PREVIEW: OnceLock<Mutex<String>> = OnceLock::new();

fn live() -> &'static Mutex<Option<Recognizer>> {
    LIVE.get_or_init(|| Mutex::new(None))
}

fn preview() -> &'static Mutex<String> {
    PREVIEW.get_or_init(|| Mutex::new(String::new()))
}

/// Start collecting from `origin`.
pub(crate) fn begin_at(origin: Point) {
    let params = Params::default();
    *live().lock().unwrap() = Some(Recognizer::new(params, origin, 0.0));
    preview().lock().unwrap().clear();
}

/// Feed one sample. Updates the preview with the directions adopted so far.
pub(crate) fn feed(point: Point) {
    let mut guard = live().lock().unwrap();
    let Some(recognizer) = guard.as_mut() else {
        return;
    };
    // The recognizer's own clock: recording is a single short interaction, and only the relative
    // gap between samples decides the degraded flag.
    let now = monotonic_ms();
    if recognizer.push(point, now) {
        if let Ok(mut text) = preview().lock() {
            *text = recognizer.snapshot_key();
        }
    }
}

/// Finish and take the sequence.
pub(crate) fn finish() -> Option<Sequence> {
    let mut guard = live().lock().unwrap();
    let recognizer = guard.as_mut()?;
    let sequence = recognizer.finish();
    *guard = None;
    Some(sequence)
}

/// Drop everything collected.
pub(crate) fn reset() {
    *live().lock().unwrap() = None;
    preview().lock().unwrap().clear();
}

/// The directions drawn so far, e.g. "RD". Empty until the first one is adopted.
pub(crate) fn preview_text() -> String {
    preview()
        .lock()
        .map(|text| text.clone())
        .unwrap_or_default()
}

/// Ask the settings window to redraw its recording row.
pub(crate) fn request_ui_refresh() {
    crate::schedule_gesture_hud();
}

/// Publish a finished recording for the main thread. Called from the tap thread: it queues, it does
/// not write anything.
pub(crate) fn publish_result() {
    crate::schedule_gesture_hud();
}

/// The main-thread half: take a published recording, validate it against the configuration as it is
/// NOW, and append it. A result that no longer makes sense (the recorder was cancelled, the feature
/// was switched off, the configuration changed) is dropped rather than written.
pub(crate) fn commit_published_result() {
    if !crate::mouse::gesture::recording::has_pending_result() {
        return;
    }
    let Some(finished) = crate::mouse::gesture::recording::take_result() else {
        return;
    };
    // A committed recording is finished, whatever the checks below decide about it: the row must
    // not keep saying "drawing" because the stroke was refused.
    crate::settings::page_view::set_record_row_text("");
    let sequence = finished.sequence;
    if sequence.overlong {
        // The runtime would refuse it, so saving it would record a gesture that can never fire.
        return;
    }
    let key = sequence.to_key();
    if key.is_empty() {
        return;
    }
    // The button the user actually drew with: no default, because a wrong button would silently
    // bind the gesture to the wrong input.
    if finished.button == 0 || finished.button > 4 {
        return;
    }
    let button = finished.button;
    // A result from a recording that has since been superseded belongs to a session the user is no
    // longer in: committing it would add a row for a stroke they cancelled.
    if finished.session != crate::mouse::gesture::recording::session() && finished.session != 0 {
        return;
    }
    let old = crate::config::CONFIG.read().unwrap().clone();
    if !old.mouse.enabled || !old.mouse.gestures_enabled {
        return;
    }
    let mut updated = old.clone();
    // The button is the one the user actually pressed, so a side-button recording binds a side
    // button.
    updated.mouse.gestures.push(crate::config::GestureBinding {
        gesture: key,
        button,
        binding: String::new(),
        enabled: false,
    });
    if let Ok(mut config) = crate::config::CONFIG.write() {
        *config = updated.clone();
    }
    crate::settings::schedule_config_persist();
    crate::runtime_config::apply_config_change(
        &old,
        &updated,
        crate::runtime_config::ConfigChangeSource::Settings,
    );
}

/// What the record row shows for a recording in `stage`, given the directions drawn so far.
///
/// Pure, and the single source for both the page description and the in-place refresh: the two
/// disagreeing is how a started recording could redraw "nothing" and read as a dead button.
pub(crate) fn record_row_text(stage: Stage, preview: &str) -> String {
    match stage {
        Stage::Idle | Stage::Finished => String::new(),
        Stage::WaitingPress if preview.is_empty() => {
            crate::i18n::t("settings.gestures_record_waiting")
        }
        Stage::Drawing | Stage::WaitingPress => {
            format!(
                "{} — {}",
                preview,
                crate::i18n::t("settings.gestures_record_drawing")
            )
        }
    }
}

/// Update the record row's text from the recording's current state.
///
/// A rebuild per sample would tear down and re-create the whole page sixty times a second, so the
/// page's own row is rewritten instead. The control is looked up by its identity, exactly as a
/// field update would be. An ENDED recording clears the row: without that, the last preview line
/// stayed on screen after the sequence was committed.
pub(crate) fn refresh_recording_row_on_main() {
    if !crate::is_main_thread() {
        return;
    }
    // The settings window's state is a RefCell, and this runs from the gesture HUD task and from
    // the record button's own action -- either may already be inside a settings callback that
    // holds it. Borrowing again there panics, so the row keeps its previous text and the next
    // update (or the rebuild on the next page load) writes the truth.
    if crate::settings::settings_ui_borrowed() {
        return;
    }
    crate::settings::page_view::sync_record_row_with_recorder();
}

/// Write the record row's text from a settings callback that already holds the window.
///
/// Shares the writer with [[refresh_recording_row_on_main]], which cannot be used there: it goes
/// through the RefCell this callback is holding.
pub(crate) fn show_record_row_text(text: &str) {
    crate::settings::page_view::set_record_row_text(text);
}

fn monotonic_ms() -> f64 {
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    let start = START.get_or_init(std::time::Instant::now);
    start.elapsed().as_secs_f64() * 1000.0
}
