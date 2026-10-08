//! Pure decisions for the cross-desktop raise: which activation API to use, whether the target's
//! desktop has been reached, and how a raise ended.
//!
//! They live here — rather than inline in the raise path — so the tests cover the decisions the
//! production path actually makes, and so a stage question can be answered without a GUI session.
//!
//! Measurements behind them (macOS 27.0.1 / 26A434, 2026-10-08, see `docs/developer-notes-en.md`):
//! a same-desktop switch completes in 3-13ms; a cross-desktop one takes ~160ms when macOS accepts
//! the commit path's app activation and ~3.2s when it refuses (the exact-window front-switch rescue
//! then waits for the target to join the active desktop), and one observed case never landed at all
//! (`onscreen=false` after 4.6s).

use std::time::Duration;

/// Which activation call the commit path makes for a cross-desktop target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActivationApi {
    /// `NSRunningApplication.activateWithOptions:` — what this app has always used. macOS 14+
    /// treats activation as cooperative and may refuse a background caller
    /// (`activateWithOptions=false` in the user's log).
    WithOptions,
    /// The documented cooperative pair (`yieldActivationToApplication:` then
    /// `activateFromApplication:options:`), available from macOS 14. Passing an
    /// `NSRunningApplication` — not an `NSApplication` — is the API's own requirement.
    FromApplication,
}

/// What the `--activation-api` development switch asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum ActivationApiRequest {
    /// No switch given: keep the shipping behaviour (`WithOptions`) until the experiment shows the
    /// cooperative path is both allowed and faster.
    #[default]
    Auto,
    WithOptions,
    FromApplication,
}

impl ActivationApiRequest {
    /// Parse the development switch. An unknown value falls back to `Auto` rather than failing a
    /// launch: a mistyped experiment flag must not make the app unusable.
    pub(crate) fn parse(value: Option<&str>) -> Self {
        match value {
            Some("with-options") => Self::WithOptions,
            Some("from-application") => Self::FromApplication,
            _ => Self::Auto,
        }
    }
}

/// Resolve the request against what this macOS actually offers.
///
/// `Auto` stays on the shipping call, so nothing changes until the experiment says otherwise. An
/// explicitly requested `FromApplication` that the system lacks falls back to `WithOptions` instead
/// of doing nothing, because the raise must still be attempted.
pub(crate) fn select_activation_api(
    request: ActivationApiRequest,
    from_application_available: bool,
) -> ActivationApi {
    match request {
        ActivationApiRequest::Auto | ActivationApiRequest::WithOptions => {
            ActivationApi::WithOptions
        }
        ActivationApiRequest::FromApplication if from_application_available => {
            ActivationApi::FromApplication
        }
        ActivationApiRequest::FromApplication => ActivationApi::WithOptions,
    }
}

/// Whether the target's location has been reached: its Space is one of the currently active Spaces.
///
/// Both sides come from the WindowServer: the target's memberships and each display's current
/// Space. An empty list on either side is *not* evidence of arrival — a window the WindowServer
/// cannot place and a failed query both look empty, and treating either as "arrived" is how a raise
/// would claim a switch that never happened.
pub(crate) fn position_reached(target_spaces: &[u64], current_spaces: &[u64]) -> bool {
    !target_spaces.is_empty()
        && !current_spaces.is_empty()
        && target_spaces
            .iter()
            .any(|space| current_spaces.contains(space))
}

/// How a cross-desktop raise ended. Every value except `Landed` means the user did not get the
/// switch this call intended; `Unknown` is not a disguised success.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Terminal {
    /// All five success criteria held while this raise was still the current one.
    Landed,
    /// The observation window ended without confirming success. It does not claim the system will
    /// never arrive afterwards — only that nothing confirmed it in time.
    Timeout,
    /// Not confirmed, but the observation window had not ended: the switch may still be settling
    /// (a front-switch or an AX action that has only been enqueued). This is *not* a timeout, and
    /// calling it one would report a failure for a raise that is still in flight.
    Unconfirmed,
    /// A newer selection replaced this raise.
    Cancelled,
    /// The window is gone from the WindowServer's list (explicit evidence only).
    TargetGone,
    /// The window id now belongs to a different process: the id was recycled, so nothing this raise
    /// observed describes the intended window.
    IdentityMismatch,
    /// A necessary query failed. Unknown never counts as success.
    Unknown,
}

impl Terminal {
    /// The stable name `--e2e-state` publishes, so a scenario asserts on a value rather than on the
    /// `Debug` spelling.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Landed => "landed",
            Self::Timeout => "timeout",
            Self::Unconfirmed => "unconfirmed",
            Self::Cancelled => "cancelled",
            Self::TargetGone => "target_gone",
            Self::IdentityMismatch => "identity_mismatch",
            Self::Unknown => "unknown",
        }
    }
}

/// Turn a presence reading into `(visible, target_gone)`.
///
/// `None` is a failed query: it yields no visibility evidence and is **not** destruction. Only a
/// query that succeeded while omitting the window proves it is gone — activation being accepted
/// says nothing about the window's existence, which is why it is not an input here.
pub(crate) fn presence_evidence(presence: Option<(bool, Option<bool>)>) -> (Option<bool>, bool) {
    match presence {
        // The query failed: nothing is known, and this is not destruction.
        None => (None, false),
        // The query succeeded and omitted the window.
        Some((false, _)) => (Some(false), true),
        // Listed: the *fresh* read decides, and an entry without the field is unknown rather than
        // "not visible" -- the raise path may treat it as not shown, but a success verdict may not.
        Some((true, Some(onscreen))) => (Some(onscreen), false),
        Some((true, None)) => (None, false),
    }
}

/// Whether a failed-fast-path retry may still send its synthetic click.
///
/// The retry sleeps between attempts, and a raise can be delivered *during* that sleep (the main
/// thread records it as soon as it observes the target focused, frontmost and on screen). The click
/// is what takes the key focus, so the decision must be re-taken at every point where one would be
/// sent -- checking only when the retry starts leaves exactly the window this guards.
pub(crate) fn retry_click_allowed(generation_current: bool, delivered: bool) -> bool {
    generation_current && !delivered
}

/// The raise a focus notification may belong to: `(generation, pid, window, armed_at)`.
pub(crate) type PendingDelivery = (u64, i32, u32, std::time::Instant);

/// Which raise a WindowServer focus notification completes, if any.
///
/// The notification is the system *reporting* that this window became the focused one, which is the
/// evidence the AX-based observation cannot get without a round trip at commit time. `note_own_focus`
/// is the request intent, not this: only the notification proves the focus actually arrived.
pub(crate) fn focus_notification_delivery(
    pending: Option<PendingDelivery>,
    window_id: u32,
    owner_pid: i32,
    captured_at: std::time::Instant,
) -> Option<u64> {
    match pending {
        Some((generation, pid, window, armed_at))
            if pid == owner_pid && window == window_id && captured_at >= armed_at =>
        {
            Some(generation)
        }
        // A notification captured before the request was armed is an older arrival, not this one:
        // taking it would mark a request delivered that has not happened yet.
        _ => None,
    }
}

/// Whether the system is *now* showing this raise as exactly delivered.
///
/// All three facts are required. An application can call a window its focused window while that
/// window sits on another desktop: the user cannot see it and the Space never switched, so treating
/// it as delivered made the raise stand down exactly when it still had work to do -- the scenario
/// caught that as a rescue which reported success while the target stayed off the active desktop.
pub(crate) fn exact_delivery_observed(
    front_pid: Option<i32>,
    focused_window: Option<u32>,
    on_screen: bool,
    frontmost_window: Option<u32>,
    target_pid: i32,
    target_window: u32,
) -> bool {
    // `frontmost_window` is the first layer-0 window of the WindowServer's on-screen list: it is what
    // tells "the window is actually in front" apart from "the application calls it focused while it
    // still sits behind". Recording delivery on the weaker evidence made a raise stand down -- and at
    // one point skip its action -- while the window had not moved, which a user reported as "the title
    // bar changes but the window does not come forward".
    on_screen
        && front_pid == Some(target_pid)
        && focused_window == Some(target_window)
        && frontmost_window == Some(target_window)
}

/// Whether this raise has already been observed as *exactly delivered*.
///
/// Delivery is a fact about what the system showed, not about what a call returned: `SLPS` reporting
/// success, an AX lookup matching an element and an AX action being *enqueued* all say nothing about
/// whether the target window ever held focus. The record is written only where it can be observed --
/// on the main thread, with the target's application in front and its own focused window equal to the
/// target -- and read here, per generation, so a task queued for a raise that has already landed is a
/// post-delivery repair while one that has not is what completes the switch.
pub(crate) fn delivery_from_record(recorded_generation: Option<u64>, generation: u64) -> bool {
    recorded_generation == Some(generation)
}

/// Whether a queued raise may still act on the focus.
///
/// The two cases are different questions and must not share one rule:
///
/// * **Not delivered yet** -- the switch has not landed and the AX action exists to make it land.
///   Nothing here indicates the user acted, so it is allowed; the caller's recovery observation
///   window is what bounds it. Treating "not landed yet" as "the user left" is what made a target
///   that only became matchable after the grace window lose its repair.
/// * **Already delivered** -- the window has joined the active desktop, so any focus that is no
///   longer the target belongs to someone else's choice: clicking another application or picking
///   another window of the same application does not bump this project's raise generation, so the
///   generation guard cannot see either. A deliverd raise must stand down instead of taking the
///   focus back.
///
/// `front_pid` and `focused_window` are read on the main thread, where they are allowed; `None`
/// means "could not be read", which is not evidence that the user left.
pub(crate) fn focus_action_allowed(
    front_pid: Option<i32>,
    focused_window: Option<u32>,
    target_pid: i32,
    target_window: u32,
    delivered: bool,
) -> bool {
    if !delivered {
        return true;
    }
    match front_pid {
        None => true,
        Some(pid) if pid != target_pid => false,
        Some(_) => focused_window.is_none_or(|window| window == target_window),
    }
}

/// The evidence `classify_terminal` weighs. `None` means "could not be read", which is distinct
/// from a read that came back false.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TerminalEvidence {
    pub(crate) generation_current: bool,
    /// `None` = the owner could not be read; `Some(false)` = the id belongs to someone else.
    pub(crate) identity_matches: Option<bool>,
    pub(crate) visible: Option<bool>,
    pub(crate) frontmost_pid_matches: Option<bool>,
    pub(crate) focused_window_matches: Option<bool>,
    /// Only set when the window is positively absent from the WindowServer's list.
    pub(crate) target_gone: bool,
    /// Whether the observation window was actually exhausted. Without it, "not confirmed yet" and
    /// "never confirmed within the budget" are indistinguishable, and an in-flight switch reads as
    /// a failure.
    pub(crate) observation_expired: bool,
}

/// Classify one raise's outcome.
///
/// Order matters: cancellation and obvious failures are decided before success can be claimed, and
/// `Landed` requires all five criteria — the target being the *system* frontmost application, not
/// only its own app's focused window, because a window can be visible with its app's focus while
/// the user is still typing into another application.
pub(crate) fn classify_terminal(evidence: TerminalEvidence) -> Terminal {
    if !evidence.generation_current {
        return Terminal::Cancelled;
    }
    if evidence.target_gone {
        return Terminal::TargetGone;
    }
    if evidence.identity_matches == Some(false) {
        return Terminal::IdentityMismatch;
    }
    let criteria = [
        evidence.identity_matches,
        evidence.visible,
        evidence.frontmost_pid_matches,
        evidence.focused_window_matches,
    ];
    if criteria.iter().all(|value| *value == Some(true)) {
        return Terminal::Landed;
    }
    if criteria.iter().any(|value| value.is_none()) {
        return Terminal::Unknown;
    }
    if evidence.observation_expired {
        Terminal::Timeout
    } else {
        Terminal::Unconfirmed
    }
}

/// The observation window for a cross-desktop raise.
///
/// One definition, used by the raise path and pinned by the test below: two copies would drift and
/// the latency numbers measured against one would not describe the other.
pub(crate) const OBSERVATION_BUDGET: Duration = Duration::from_millis(3000);

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence() -> TerminalEvidence {
        TerminalEvidence {
            generation_current: true,
            identity_matches: Some(true),
            visible: Some(true),
            frontmost_pid_matches: Some(true),
            focused_window_matches: Some(true),
            target_gone: false,
            observation_expired: true,
        }
    }

    #[test]
    fn auto_keeps_the_shipping_call_even_where_the_new_one_exists() {
        // The point of the experiment: nothing changes until it shows a gain.
        assert_eq!(
            select_activation_api(ActivationApiRequest::Auto, true),
            ActivationApi::WithOptions
        );
        assert_eq!(
            select_activation_api(ActivationApiRequest::Auto, false),
            ActivationApi::WithOptions
        );
        // An explicit request on a system that lacks the API still attempts a raise.
        assert_eq!(
            select_activation_api(ActivationApiRequest::FromApplication, false),
            ActivationApi::WithOptions
        );
        assert_eq!(
            select_activation_api(ActivationApiRequest::FromApplication, true),
            ActivationApi::FromApplication
        );
        // A mistyped development value must not break the launch.
        assert_eq!(
            ActivationApiRequest::parse(Some("nonsense")),
            ActivationApiRequest::Auto
        );
        assert_eq!(
            ActivationApiRequest::parse(Some("from-application")),
            ActivationApiRequest::FromApplication
        );
    }

    #[test]
    fn position_needs_a_real_intersection_with_the_active_spaces() {
        assert!(position_reached(&[386], &[1, 386]));
        assert!(!position_reached(&[386], &[1]));
        // A window the WindowServer cannot place, and a failed current-Space query, are both
        // "unknown", never "arrived".
        assert!(!position_reached(&[], &[1]));
        assert!(!position_reached(&[386], &[]));
    }

    #[test]
    fn a_visible_window_whose_app_is_not_frontmost_is_not_a_landed_switch() {
        // The counter-example that keeps the success criteria honest: the window is on screen and
        // its own app reports it focused, but another application is still receiving input.
        let mut e = evidence();
        e.frontmost_pid_matches = Some(false);
        assert_eq!(classify_terminal(e), Terminal::Timeout);

        let mut e = evidence();
        e.focused_window_matches = Some(false);
        assert_eq!(classify_terminal(e), Terminal::Timeout);
    }

    #[test]
    fn only_a_notification_newer_than_the_request_completes_it() {
        use std::time::Instant;
        let armed_at = Instant::now();
        let pending = Some((9, 10, 700, armed_at));
        // Reported after the request was armed: this arrival is the request's.
        assert_eq!(
            focus_notification_delivery(
                pending,
                700,
                10,
                armed_at + std::time::Duration::from_millis(5)
            ),
            Some(9)
        );
        // The counter-example: the notification was captured *before* the request (it sat in the
        // bridge/queue while the user moved on and came back). It must not complete this request.
        assert_eq!(
            focus_notification_delivery(
                pending,
                700,
                10,
                armed_at - std::time::Duration::from_millis(5)
            ),
            None
        );
        // Same window id, another process (a recycled id) is not this raise.
        assert_eq!(
            focus_notification_delivery(
                pending,
                700,
                11,
                armed_at + std::time::Duration::from_millis(5)
            ),
            None
        );
        // Another window of the same process is not this raise.
        assert_eq!(
            focus_notification_delivery(
                pending,
                701,
                10,
                armed_at + std::time::Duration::from_millis(5)
            ),
            None
        );
        // Nothing pending.
        assert_eq!(
            focus_notification_delivery(
                None,
                700,
                10,
                armed_at + std::time::Duration::from_millis(5)
            ),
            None
        );
    }

    #[test]
    fn a_retry_stops_clicking_once_the_raise_is_delivered_during_its_wait() {
        // Entry: nothing delivered yet, so the retry may try (its own generation is current).
        assert!(retry_click_allowed(true, false));
        // The same retry, re-checked after a sleep: the main thread confirmed delivery meanwhile, so
        // the click would now take the focus back -- and this is the check position that matters.
        assert!(!retry_click_allowed(true, true));
        // Superseded by a newer raise.
        assert!(!retry_click_allowed(false, false));
        assert!(!retry_click_allowed(false, true));
    }

    #[test]
    fn exact_delivery_needs_the_window_actually_in_front() {
        // The user-visible regression as an assertion: the application is frontmost and calls this its
        // focused window, the window is on screen -- yet it is not the frontmost window, so the window
        // never came forward and nothing has been delivered.
        assert!(!exact_delivery_observed(
            Some(10),
            Some(700),
            true,
            Some(701),
            10,
            700
        ));
        // Another application owns the frontmost window.
        assert!(!exact_delivery_observed(
            Some(10),
            Some(700),
            true,
            Some(900),
            10,
            700
        ));
        // Not on screen (another desktop).
        assert!(!exact_delivery_observed(
            Some(10),
            Some(700),
            false,
            Some(700),
            10,
            700
        ));
        // Another application is frontmost.
        assert!(!exact_delivery_observed(
            Some(7),
            Some(700),
            true,
            Some(700),
            10,
            700
        ));
        // The application's own focus is a different window of its own.
        assert!(!exact_delivery_observed(
            Some(10),
            Some(701),
            true,
            Some(700),
            10,
            700
        ));
        // Unreadable front, focus or frontmost window is not delivery either.
        assert!(!exact_delivery_observed(
            None,
            Some(700),
            true,
            Some(700),
            10,
            700
        ));
        assert!(!exact_delivery_observed(
            Some(10),
            None,
            true,
            Some(700),
            10,
            700
        ));
        assert!(!exact_delivery_observed(
            Some(10),
            Some(700),
            true,
            None,
            10,
            700
        ));
        // All four: delivered.
        assert!(exact_delivery_observed(
            Some(10),
            Some(700),
            true,
            Some(700),
            10,
            700
        ));
    }

    #[test]
    fn delivery_is_a_recorded_observation_of_this_generation() {
        // Observed for this raise: a later task for it is a post-delivery repair.
        assert!(delivery_from_record(Some(7), 7));
        // Never observed, or observed for another raise: this task is what completes the switch.
        assert!(!delivery_from_record(None, 7));
        assert!(!delivery_from_record(Some(6), 7));
    }

    #[test]
    fn an_undelivered_raise_may_keep_trying_but_a_delivered_one_stands_down() {
        // Not delivered: the switch has not landed, so the repair is the original request still
        // settling -- allowed even long after the request, bounded by the caller's window.
        assert!(focus_action_allowed(Some(7), None, 10, 700, false));
        assert!(focus_action_allowed(Some(10), Some(701), 10, 700, false));
        assert!(focus_action_allowed(None, None, 10, 700, false));
    }

    #[test]
    fn a_delivered_raise_stops_when_anyone_else_holds_the_focus() {
        // Delivered and another application is in front: the user moved on.
        assert!(!focus_action_allowed(Some(7), None, 10, 700, true));
        // Delivered, the target application is in front, but its focus is another window of its own:
        // the frontmost pid alone cannot see this one.
        assert!(!focus_action_allowed(Some(10), Some(701), 10, 700, true));
        // Delivered and the target still holds the exact window.
        assert!(focus_action_allowed(Some(10), Some(700), 10, 700, true));
        // Unreadable front or focus is not evidence that the user left.
        assert!(focus_action_allowed(None, None, 10, 700, true));
        assert!(focus_action_allowed(Some(10), None, 10, 700, true));
    }

    #[test]
    fn an_in_flight_switch_is_not_reported_as_a_timeout() {
        // The AX action is only enqueued when the criteria are sampled, so focus legitimately does
        // not match yet. Before the deadline that is "unconfirmed", not "timed out".
        let mut e = evidence();
        e.focused_window_matches = Some(false);
        e.observation_expired = false;
        assert_eq!(classify_terminal(e), Terminal::Unconfirmed);
        // Once the window is exhausted, the same evidence *is* a timeout.
        e.observation_expired = true;
        assert_eq!(classify_terminal(e), Terminal::Timeout);
    }

    #[test]
    fn unknown_evidence_never_counts_as_success() {
        for field in 0..3 {
            let mut e = evidence();
            match field {
                0 => e.identity_matches = None,
                1 => e.visible = None,
                _ => e.frontmost_pid_matches = None,
            }
            assert_eq!(classify_terminal(e), Terminal::Unknown);
        }
    }

    #[test]
    fn cancellation_and_identity_failures_outrank_success() {
        let mut cancelled = evidence();
        cancelled.generation_current = false;
        assert_eq!(classify_terminal(cancelled), Terminal::Cancelled);

        let mut recycled = evidence();
        recycled.identity_matches = Some(false);
        assert_eq!(classify_terminal(recycled), Terminal::IdentityMismatch);

        let mut gone = evidence();
        gone.target_gone = true;
        assert_eq!(classify_terminal(gone), Terminal::TargetGone);
        // ...and destruction is reported even when every other criterion happens to hold.
        assert_ne!(classify_terminal(gone), Terminal::Landed);
    }

    #[test]
    fn a_failed_query_is_unknown_and_never_destruction() {
        // The counter-example that defeated the old evidence gathering: a query that returned
        // nothing was folded into "the window is gone".
        assert_eq!(presence_evidence(None), (None, false));
        // A successful query that omits the window *is* destruction...
        assert_eq!(presence_evidence(Some((false, None))), (Some(false), true));
        // ...while a listed window carries the visibility the query just observed.
        assert_eq!(
            presence_evidence(Some((true, Some(false)))),
            (Some(false), false)
        );
        assert_eq!(
            presence_evidence(Some((true, Some(true)))),
            (Some(true), false)
        );
    }

    #[test]
    fn the_fresh_visibility_wins_and_a_missing_field_stays_unknown() {
        // The old code passed the *earlier* wait's on-screen value in, so a window that had been
        // visible and then stopped being visible still counted as visible. The fresh read decides.
        assert_eq!(
            presence_evidence(Some((true, Some(false)))),
            (Some(false), false)
        );
        assert_eq!(
            presence_evidence(Some((true, Some(true)))),
            (Some(true), false)
        );
        // An entry without the on-screen field is unknown, never "not visible".
        assert_eq!(presence_evidence(Some((true, None))), (None, false));
    }

    #[test]
    fn terminal_labels_are_stable_and_distinct() {
        let labels = [
            Terminal::Landed,
            Terminal::Timeout,
            Terminal::Unconfirmed,
            Terminal::Cancelled,
            Terminal::TargetGone,
            Terminal::IdentityMismatch,
            Terminal::Unknown,
        ]
        .map(Terminal::label);
        assert_eq!(
            labels,
            [
                "landed",
                "timeout",
                "unconfirmed",
                "cancelled",
                "target_gone",
                "identity_mismatch",
                "unknown"
            ]
        );
    }

    #[test]
    fn the_observation_budget_is_the_one_the_tests_pin() {
        assert_eq!(OBSERVATION_BUDGET, Duration::from_millis(3000));
    }
}
