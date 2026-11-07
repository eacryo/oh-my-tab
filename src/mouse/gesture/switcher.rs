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

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};

use crate::{enqueue_global_event, log_debug};

/// No gesture opened the current switcher session.
const CLAIM_NONE: u8 = 0;
/// A gesture opened it and still owns it.
const CLAIM_ACTIVE: u8 = 1;
/// Someone else took the session over; the gesture no longer owns it.
const CLAIM_HANDED_OFF: u8 = 2;
/// The session was withdrawn by the gesture's own revocation path.
const CLAIM_REVOKED: u8 = 3;

static CLAIM: AtomicU8 = AtomicU8::new(CLAIM_NONE);
static CLAIM_ID: AtomicU64 = AtomicU64::new(0);

/// Open the switcher on behalf of a gesture, unless one is already showing.
pub(crate) fn open(auth: super::executor::Authorization) {
    if super::session_state::epoch() != auth.epoch {
        // Withdrawn while this gesture was being decided: never open a session it may not own.
        log_debug!("[gesture] switcher target dropped: the gesture was revoked");
        return;
    }
    if super::session_state::switcher_visible() {
        // A session the user already has is not ours to drive: pressing Tab now would step its
        // selection and the gesture would have changed something it does not own.
        log_debug!("[gesture] switcher target ignored: a switcher session is already showing");
        return;
    }
    let claim = CLAIM_ID.fetch_add(1, Ordering::SeqCst) + 1;
    CLAIM.store(CLAIM_ACTIVE, Ordering::SeqCst);
    // The token travels with the request so the consumer can tell the gesture's own open from a
    // trigger the user produced, and refuse one whose claim was withdrawn while it queued.
    enqueue_global_event(crate::event_monitor::GlobalEvent::GestureSwitcherOpen { claim });
}

/// Another source triggered the switcher: hand the session over.
pub(crate) fn hand_off() {
    let _ = CLAIM.compare_exchange(
        CLAIM_ACTIVE,
        CLAIM_HANDED_OFF,
        Ordering::SeqCst,
        Ordering::SeqCst,
    );
}

/// The gesture's authorization was withdrawn, or the module stopped.
pub(crate) fn revoke() {
    if CLAIM
        .compare_exchange(
            CLAIM_ACTIVE,
            CLAIM_REVOKED,
            Ordering::SeqCst,
            Ordering::SeqCst,
        )
        .is_ok()
    {
        // Never hide the window here: this runs from the stop path, which a permission monitor can
        // reach on a background thread, and AppKit is main-thread only. The main thread performs the
        // dismissal for the session this claim owns.
        crate::schedule_gesture_hud();
    }
}

/// Main-thread half of revoke: dismiss the session a revoked claim owns without committing a
/// window. A claim handed off to the user's own session is left alone.
pub(crate) fn revoke_on_main() {
    if CLAIM.load(Ordering::SeqCst) != CLAIM_REVOKED {
        return;
    }
    // Covers all three phases: a request still queued never opens (claim_still_active), a first show
    // awaiting its snapshot is cancelled, and a shown session is dismissed. None of them commits a
    // window.
    if super::session_state::switcher_visible() {
        crate::overlay::hide_overlay();
    }
    CLAIM.store(CLAIM_NONE, Ordering::SeqCst);
}

/// The underlying switcher session ended (commit, cancel or hide): the claim ends with it.
pub(crate) fn end() {
    CLAIM.store(CLAIM_NONE, Ordering::SeqCst);
}

/// The main-thread consumer for a queued gesture open. Refuses the request when the claim was
/// withdrawn while it waited, so a revoked gesture cannot still open a session.
pub(crate) fn open_on_main(claim: u64) {
    if CLAIM.load(Ordering::SeqCst) == CLAIM_REVOKED && CLAIM_ID.load(Ordering::SeqCst) == claim {
        log_debug!("[gesture] queued switcher open refused: its claim was withdrawn");
        return;
    }
    if super::session_state::switcher_visible() {
        return;
    }
    // The step itself carries the origin: the enqueue point tags it, so no side channel has to guess
    // which queued step is the gesture's.
    crate::enqueue_gesture_switcher_step(claim);
}

/// Whether the claim a queued step carries is still the one an active session owns. A step whose
/// claim was revoked while it queued, or which belongs to an older claim, must not open anything.
pub(crate) fn claim_still_active(claim: u64) -> bool {
    CLAIM.load(Ordering::SeqCst) == CLAIM_ACTIVE && CLAIM_ID.load(Ordering::SeqCst) == claim
}

/// The gesture's own opener reached the switcher. Its claim stays Active; nothing is handed over.
pub(crate) fn own_open_consumed() {
    log_debug!("[gesture] the gesture's own switcher opener was consumed");
}

pub(crate) fn claim_active() -> bool {
    CLAIM.load(Ordering::SeqCst) == CLAIM_ACTIVE
}

/// The claim's id, so a test can tell one gesture-opened session from the next.
pub(crate) fn claim_id() -> u64 {
    CLAIM_ID.load(Ordering::SeqCst)
}
