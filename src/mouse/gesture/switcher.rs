//! Ownership of a switcher session a gesture opened.
//!
//! A gesture's "switcher" target only *opens* the switcher; the usual session lifecycle (Return or
//! a card click commits, Esc dismisses) finishes it. What the gesture must additionally guarantee is
//! that it never disturbs a session the user already had, and that its own session is cleaned up
//! when its authorization is withdrawn.
//!
//! The transition rules mirror the design's three-state claim:
//!
//! * `open` records a claim and posts `CmdTabPressed`. If a switcher is already on screen the press
//!   is not posted at all: the user's own session keeps the keyboard.
//! * Any later switcher trigger from another source hands the claim over (`HandedOff`), after which
//!   the gesture no longer owns that session and will not cancel it.
//! * `revoke` runs when the gesture's configuration is withdrawn or the module stops: a claim still
//!   `Active` dismisses the session it owns and touches nothing else.
//!
//! Two thread rules shape this module. `open` runs on the HID tap thread, so it may only read
//! atomics and published state -- `switcher_visible` belongs to the main thread and is asked by the
//! consumer instead. And the claim's id, state and authorization live in ONE record behind one lock:
//! a consumer that checked the id and then read the authorization separately could validate an old
//! request against a newer claim's authorization.

use std::sync::{Mutex, MutexGuard};

use crate::{enqueue_global_event, log_debug};

/// No gesture opened the current switcher session.
const CLAIM_NONE: u8 = 0;
/// A gesture opened it and still owns it.
const CLAIM_ACTIVE: u8 = 1;
/// Someone else took the session over; the gesture no longer owns it.
const CLAIM_HANDED_OFF: u8 = 2;
/// The session was withdrawn by the gesture's own revocation path.
const CLAIM_REVOKED: u8 = 3;

/// The claim over the current switcher session: which gesture requested it, what state it is in, and
/// the authorization it was registered under. Read as one value, so an old request can never be
/// validated against a newer claim's authorization -- a permission stop advances the mouse service
/// generation without moving the epoch, and only the authorization carries that fact.
struct Claim {
    id: u64,
    state: u8,
    auth: Option<super::executor::Authorization>,
}

static CLAIM: Mutex<Claim> = Mutex::new(Claim {
    id: 0,
    state: CLAIM_NONE,
    auth: None,
});

fn claim_state() -> MutexGuard<'static, Claim> {
    CLAIM.lock().unwrap_or_else(|error| error.into_inner())
}

/// Open the switcher on behalf of a gesture.
///
/// Whether a session is already showing is decided by `open_on_main`: `switcher_visible` reads
/// main-thread UI state (a pending first show), and this runs in the HID callback.
pub(crate) fn open(auth: super::executor::Authorization) {
    if super::session_state::epoch() != auth.epoch {
        // Withdrawn while this gesture was being decided: never open a session it may not own.
        log_debug!("[gesture] switcher target dropped: the gesture was revoked");
        return;
    }
    let claim = {
        let mut state = claim_state();
        // A revocation can land between the check above and this registration, and its `revoke`
        // would then have found no claim to mark. The publish path bumps the epoch BEFORE it
        // revokes, so the epoch is re-read below, with the claim already in place: either this sees
        // the new epoch and withdraws the claim itself, or the revoke that follows finds it here.
        state.id += 1;
        state.state = CLAIM_ACTIVE;
        state.auth = Some(auth);
        state.id
    };
    if super::session_state::epoch() != auth.epoch {
        log_debug!("[gesture] switcher target dropped: revoked while it was being registered");
        revoke();
        return;
    }
    // The token travels with the request so the consumer can tell the gesture's own open from a
    // trigger the user produced, and refuse one whose claim was withdrawn while it queued.
    enqueue_global_event(crate::event_monitor::GlobalEvent::GestureSwitcherOpen { claim });
}

/// Another source triggered the switcher: hand the session over.
pub(crate) fn hand_off() {
    let mut state = claim_state();
    if state.state == CLAIM_ACTIVE {
        state.state = CLAIM_HANDED_OFF;
    }
}

/// The gesture's authorization was withdrawn, or the module stopped.
pub(crate) fn revoke() {
    let revoked = {
        let mut state = claim_state();
        if state.state != CLAIM_ACTIVE {
            false
        } else {
            state.state = CLAIM_REVOKED;
            true
        }
    };
    if revoked {
        // Never hide the window here: this runs from the stop path, which a permission monitor can
        // reach on a background thread, and AppKit is main-thread only. The main thread performs the
        // dismissal for the session this claim owns.
        crate::schedule_gesture_hud();
    }
}

/// Main-thread half of revoke: dismiss the session a revoked claim owns without committing a
/// window. A claim handed off to the user's own session is left alone.
pub(crate) fn revoke_on_main() {
    {
        let mut state = claim_state();
        if state.state != CLAIM_REVOKED {
            return;
        }
        state.state = CLAIM_NONE;
        state.auth = None;
    }
    // Covers all three phases: a request still queued never opens (claim_still_active), a first show
    // awaiting its snapshot is cancelled, and a shown session is dismissed. None of them commits a
    // window.
    if super::session_state::switcher_visible() {
        crate::overlay::hide_overlay();
    }
}

/// The underlying switcher session ended (commit, cancel or hide): the claim ends with it.
pub(crate) fn end() {
    let mut state = claim_state();
    state.state = CLAIM_NONE;
    state.auth = None;
}

/// The main-thread consumer for a queued gesture open. Refuses the request when the claim was
/// withdrawn while it waited, so a revoked gesture cannot still open a session.
pub(crate) fn open_on_main(claim: u64) {
    if !claim_still_active(claim) {
        log_debug!("[gesture] queued switcher open refused: its claim is no longer authorized");
        return;
    }
    if super::session_state::switcher_visible() {
        // A session the user already has is not ours to drive: pressing Tab now would step its
        // selection and the gesture would have changed something it does not own. The claim was
        // registered on the tap thread, which may not read this state, so it is withdrawn here.
        log_debug!("[gesture] switcher target ignored: a switcher session is already showing");
        let mut state = claim_state();
        if state.id == claim {
            state.state = CLAIM_NONE;
            state.auth = None;
        }
        return;
    }
    // The step itself carries the origin: the enqueue point tags it, so no side channel has to guess
    // which queued step is the gesture's.
    crate::enqueue_gesture_switcher_step(claim);
}

/// Whether the claim a queued step carries is still the one an active session owns. A step whose
/// claim was revoked while it queued, or which belongs to an older claim, must not open anything.
pub(crate) fn claim_still_active(claim: u64) -> bool {
    let auth = {
        let state = claim_state();
        if state.state != CLAIM_ACTIVE || state.id != claim {
            return false;
        }
        state.auth
    };
    match auth {
        // The SAME three questions the release asked, asked again at consumption: the epoch for a
        // configuration change, the service generation for a stop the configuration does not
        // record, and the runtime gate for a recorder or lost permission.
        Some(auth) => super::runtime::still_authorized(auth.epoch, auth.generation),
        None => false,
    }
}

/// The gesture's own opener reached the switcher. Its claim stays Active; nothing is handed over.
pub(crate) fn own_open_consumed() {
    log_debug!("[gesture] the gesture's own switcher opener was consumed");
}

pub(crate) fn claim_active() -> bool {
    claim_state().state == CLAIM_ACTIVE
}

/// The claim's id, so a test can tell one gesture-opened session from the next.
pub(crate) fn claim_id() -> u64 {
    claim_state().id
}
