//! Running a gesture's target once it matched.
//!
//! Authorization was already re-checked on the tap thread at release (the epoch must still be the
//! one the press started under), and again here for recording, which can start between the two.
//! The work itself goes through the existing machinery: the key poster for shortcuts, the Dock
//! notification path for system actions, and the action registry for the app's own actions.

use super::table::{Row, Target};
use crate::key_bindings::Action;
use crate::mouse::keysim;
use crate::mouse::shortcut::Binding;
use crate::{event_monitor::GlobalEvent, log_debug};

/// Run one matched row. Called from inside the event-tap callback, so every path here queues rather
/// than blocks.
/// The authorization a gesture ran under. Carried all the way to the main thread so a decision made
/// at release is not re-made from a newer configuration: re-reading the epoch at dispatch would let
/// a stale action stamp itself with the current one and pass the consumer's check.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Authorization {
    pub(crate) epoch: u64,
    /// The mouse service generation of the gesture. A stop advances it, so a job that outlived the
    /// service is refused after a restart even when the configuration never changed.
    pub(crate) generation: u64,
    /// The HUD session the gesture belonged to, so the trail a scenario reads names the same
    /// gesture the action ran for.
    pub(crate) session: u64,
}

pub(crate) fn run(row: Row, auth: Authorization) {
    // The recorder changes what a keystroke means; a gesture must not fire into it.
    if crate::mouse::event_tap::RECORDING.load(std::sync::atomic::Ordering::Relaxed) {
        log_debug!(
            "[gesture] dropped row {}: recording is in progress",
            row.index
        );
        return;
    }
    log_debug!("[gesture] running row {} for key {}", row.index, row.key);
    match row.target {
        Target::Action(action) => run_action(action, auth),
        Target::Binding(binding) => run_binding(binding, auth),
    }
}

fn run_action(action: Action, auth: Authorization) {
    // \`execute\` touches AppKit (it resets the switcher) and window state, so it belongs on the main
    // thread. The tap thread only queues it, with no reply, and the main-thread consumer re-checks
    // the epoch the gesture actually ran under before doing anything.
    let epoch = auth.epoch;
    crate::enqueue_global_event(GlobalEvent::GestureAction {
        action: action.index(),
        epoch,
        session: auth.session,
        generation: auth.generation,
    });
}

/// Main-thread consumer for a gesture action. Drops it when the configuration changed since the
/// gesture started: the same revocation rule the release applies, enforced again where the work
/// actually happens.
pub(crate) fn run_action_on_main(index: u16, epoch: u64, session: u64, generation: u64) {
    crate::mouse::gesture::session_state::note_action(session);
    if crate::mouse::event_tap::RECORDING.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    if !super::runtime::still_authorized(epoch, generation) {
        log_debug!("[gesture] action dropped: the gesture is no longer authorized");
        return;
    }
    let Some(action) = Action::from_index(index) else {
        return;
    };
    crate::key_bindings::execute(action);
}

fn run_binding(binding: Binding, auth: Authorization) {
    match binding {
        Binding::Key(shortcut) => keysim::queue_gesture_keys(
            shortcut.keycode,
            shortcut.flags,
            auth.epoch,
            auth.generation,
        ),
        Binding::System(notification) => {
            keysim::queue_system_action_for_gesture(notification, auth.epoch, auth.generation)
        }
        Binding::Switcher => {
            // Ownership-aware: never drives a session the user already had, and its own session is
            // dismissed when the gesture's authorization is withdrawn.
            super::switcher::open(auth);
        }
        Binding::None => {}
    }
}
