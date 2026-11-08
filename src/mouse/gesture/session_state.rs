//! Published gesture state, for diagnostics, the settings page and e2e scenarios.
//!
//! Every field is written where the fact becomes true -- the phase by the tap thread, `hud_visible`
//! by the main thread *after* the window actually changes -- so a reader can tell "asked to hide"
//! from "hidden", which a single request flag cannot express.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};

static HUD_VISIBLE: AtomicBool = AtomicBool::new(false);
static SWITCHER_VISIBLE: AtomicBool = AtomicBool::new(false);
static HUD_LAST_REQUEST: AtomicU64 = AtomicU64::new(0);
static LAST_ACTION_SESSION: AtomicU64 = AtomicU64::new(0);

/// Record the HUD session whose gesture ran an action.
pub(crate) fn note_action(session: u64) {
    LAST_ACTION_SESSION.store(session, Ordering::SeqCst);
}

pub(crate) fn last_action_session() -> u64 {
    LAST_ACTION_SESSION.load(Ordering::SeqCst)
}

static PHASE: AtomicU8 = AtomicU8::new(0);

/// Idle / Held / Engaged / Abandoned, in the order the engine uses.
pub(crate) fn set_phase(name: &str) {
    let index = match name {
        "Held" => 1,
        "Engaged" => 2,
        "Abandoned" => 3,
        _ => 0,
    };
    PHASE.store(index, Ordering::SeqCst);
}

pub(crate) fn phase() -> &'static str {
    match PHASE.load(Ordering::SeqCst) {
        1 => "Held",
        2 => "Engaged",
        3 => "Abandoned",
        _ => "Idle",
    }
}

/// A trail request was handled by the main thread. Distinct from `hud_visible`: the request path
/// is implemented, the window is not.
pub(crate) fn note_hud_request(session: u64) {
    HUD_LAST_REQUEST.store(session, Ordering::SeqCst);
}

pub(crate) fn hud_last_request() -> u64 {
    HUD_LAST_REQUEST.load(Ordering::SeqCst)
}

pub(crate) fn set_hud_visible(visible: bool) {
    HUD_VISIBLE.store(visible, Ordering::SeqCst);
}

/// Whether the trail window is on screen right now -- the window's own state, not a pending request.
pub(crate) fn hud_visible() -> bool {
    HUD_VISIBLE.load(Ordering::SeqCst)
}

/// What the stroke in flight would run, in the user's words, for the HUD's hint line.
///
/// Read from the engine's CURRENT sequence against the live table, so the hint names the action the
/// gesture would take, not the one it took last time. A stroke no row matches says so instead of
/// borrowing an old answer: that is what the user needs to know before releasing the button.
/// What the hint card shows while a stroke is in flight: the action the stroke would run and the
/// direction it took last.
///
/// Read from the engine's CURRENT sequence against the live table, so the card names the action the
/// gesture would take, not the one it took last time. A stroke no row matches says so instead of
/// borrowing an old answer -- that is what the user needs to know before releasing the button.
pub(crate) fn hint_card() -> crate::mouse::gesture::hud::HintText {
    let (key, button, device) = super::runtime::with(|rt| {
        (
            rt.engine_sequence_key(),
            rt.engine_button(),
            rt.engine_device_key(),
        )
    });
    // The device that started the stroke decides which layer's action is named: a mouse with its
    // own list would otherwise be promised another mouse's action before the button is released.
    let snapshot = super::table::snapshot();
    hint_card_for(key.as_deref(), button, snapshot.table_for(device))
}

/// The card for one stroke: the action `sequence` would run on `button`, or the words that say
/// nothing matches it.
///
/// The card for a stroke nothing matches points nowhere -- `direction` is `None` -- because an
/// arrow would read as the direction of an action the stroke is about to run.
fn hint_card_for(
    sequence: Option<&str>,
    button: u32,
    table: &super::table::GestureTable,
) -> crate::mouse::gesture::hud::HintText {
    let direction = sequence.as_ref().and_then(|key| key.chars().last());
    match sequence.filter(|key| !key.is_empty()) {
        Some(key) => match table.lookup(button, key) {
            Some(row) => crate::mouse::gesture::hud::HintText {
                direction,
                text: super::table::row_label(row),
            },
            None => crate::mouse::gesture::hud::HintText {
                direction: None,
                text: crate::i18n::t("settings.gestures_hint_unmatched"),
            },
        },
        None => crate::mouse::gesture::hud::HintText {
            direction,
            text: String::new(),
        },
    }
}

/// A one-line summary for the settings page's gesture section: how many rows are live, which epoch
/// they belong to and whether the trail in flight is of good quality. Read when the page renders.
pub(crate) fn summary() -> String {
    let published = super::table::snapshot();
    format!(
        "{} row(s), epoch {}, phase {}, degraded {}",
        published.table.len(),
        published.epoch,
        phase(),
        TRAIL_DEGRADED.load(Ordering::SeqCst)
    ) + &format!(" origin {:?}", origin())
}

static TRAIL_DEGRADED: AtomicBool = AtomicBool::new(false);

/// The live gesture's press origin, in global points. Published so a scenario can tell where a
/// gesture started without reading the tap thread's state.
static ORIGIN_X: AtomicU64 = AtomicU64::new(u64::MAX);
static ORIGIN_Y: AtomicU64 = AtomicU64::new(u64::MAX);

pub(crate) fn set_origin(origin: Option<crate::mouse::gesture::recognizer::Point>) {
    match origin {
        Some(point) => {
            ORIGIN_X.store(point.x.to_bits(), Ordering::SeqCst);
            ORIGIN_Y.store(point.y.to_bits(), Ordering::SeqCst);
        }
        None => {
            ORIGIN_X.store(u64::MAX, Ordering::SeqCst);
            ORIGIN_Y.store(u64::MAX, Ordering::SeqCst);
        }
    }
}

pub(crate) fn origin() -> Option<crate::mouse::gesture::recognizer::Point> {
    let x = ORIGIN_X.load(Ordering::SeqCst);
    let y = ORIGIN_Y.load(Ordering::SeqCst);
    if x == u64::MAX || y == u64::MAX {
        return None;
    }
    Some(crate::mouse::gesture::recognizer::Point::new(
        f64::from_bits(x),
        f64::from_bits(y),
    ))
}

/// Publish the trail-quality fact of the session in flight (a sample gap was seen).
pub(crate) fn set_trail_degraded(degraded: bool) {
    TRAIL_DEGRADED.store(degraded, Ordering::SeqCst);
}

pub(crate) fn trail_degraded() -> bool {
    TRAIL_DEGRADED.load(Ordering::SeqCst)
}

/// Whether a switcher session is showing OR is on its way (a first show awaiting its snapshot).
///
/// The "is one already showing?" guard has to cover the pending phase too: a gesture opening the
/// switcher right after a summon would otherwise post a second press into a session it does not own.
/// Main-thread only: the pending phase belongs to the UI runtime.
pub(crate) fn switcher_visible() -> bool {
    crate::assert_main_thread("switcher pending visibility");
    switcher_shown()
        || crate::with_tab_state(|state| state.as_ref().is_some_and(|s| s.pending_first_show))
}

/// Published visibility for the tap thread's hover routing. A pending first show has no visible
/// cards to hover, and its main-thread UI state must not be read from the event callback.
pub(crate) fn switcher_shown() -> bool {
    SWITCHER_VISIBLE.load(Ordering::SeqCst)
}

/// Publish the switcher's visibility. Called by the overlay code that actually knows.
pub(crate) fn set_switcher_visible(visible: bool) {
    SWITCHER_VISIBLE.store(visible, Ordering::SeqCst);
    if !visible {
        // The session ended (commit, cancel or hide), so a gesture claim over it ends with it.
        super::switcher::end();
    }
}

/// The epoch a new session would start under. Exposed so a test can observe that a configuration
/// change actually advanced it.
pub(crate) fn epoch() -> u64 {
    super::table::current_epoch()
}

#[cfg(test)]
mod tests {
    #[test]
    fn published_switcher_visibility_is_readable_off_main_thread() {
        assert!(std::thread::spawn(super::switcher_shown).join().is_ok());
    }

    fn one_gesture() -> super::super::table::GestureTable {
        let mut mouse = crate::config::MouseSection::default();
        mouse.enabled = true;
        mouse.gestures_enabled = true;
        mouse.gestures = vec![crate::config::GestureBinding {
            gesture: "L".into(),
            button: 1,
            binding: "window.snap_left".into(),
            enabled: true,
        }];
        let (table, errors) = super::super::table::build(&mouse);
        assert!(errors.is_empty(), "the fixture gesture must be valid");
        table
    }

    /// A stroke nothing matches is not a gesture, and its card says so in words alone: the arrow
    /// points at the action the stroke would run, so there is no arrow to draw when nothing runs.
    #[test]
    fn an_unmatched_stroke_gets_a_card_with_no_arrow() {
        let table = one_gesture();
        let unmatched = super::hint_card_for(Some("RD"), 1, &table);
        assert_eq!(unmatched.direction, None, "no direction, so no arrow");
        assert!(!unmatched.has_arrow());
        assert_eq!(
            unmatched.text,
            crate::i18n::t("settings.gestures_hint_unmatched")
        );
        assert!(
            !unmatched.text.is_empty(),
            "the card must still say something"
        );
    }

    /// A matched stroke keeps the arrow: its direction is what the user reads without looking away.
    #[test]
    fn a_matched_stroke_keeps_its_arrow() {
        let table = one_gesture();
        let matched = super::hint_card_for(Some("L"), 1, &table);
        assert_eq!(matched.direction, Some('L'));
        assert!(matched.has_arrow());
        assert_eq!(
            matched.text,
            super::super::table::row_label(table.lookup(1, "L").expect("the fixture row"))
        );
    }
}
