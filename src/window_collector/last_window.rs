//! Quit an application when a close made from this app turned out to be its last window.
//!
//! The promise is narrow on purpose: this module acts only on closes this app initiated (the
//! switcher's card button / Backspace / configured panel key, and the Dock preview's close button).
//! It never watches other applications' windows, so an app closed from inside itself is left
//! running -- that is the system-wide behaviour of a dedicated quit-on-close utility, not this one.
//!
//! # What counts as proof
//!
//! Closing a window is a request: the AX close being accepted does not mean the window is gone (a
//! save sheet can hold it), and an app may answer by HIDING the window rather than destroying it
//! (TextEdit does; so do several document apps). Both reference implementations (DockDoor,
//! vorssaint) therefore judge "the app has no window left" from what the screen and Accessibility
//! say, not from a destruction notification, and this module does the same:
//!
//! 1. a pre-check *before* the close: the target is a user window and the app's ONLY user window, so
//!    the close being attributed to this transaction is the one that empties the app;
//! 2. two fresh readings, `RECHECK_DELAY` apart: the target is no longer on screen and no other user
//!    window is left. Two readings because a transition can destroy and recreate a window (Office
//!    apps and Chrome PWAs do).
//!
//! "User window" is deliberate: a window the screen shows, or one Accessibility reports as
//! minimized (the screen does not show a minimized window, but the user can restore it). An app's own
//! hidden leftovers -- the window it just hid on close, or the untitled helper surfaces TextEdit
//! keeps off screen -- are not user windows. Anything unreadable or unidentifiable is `Unknown`, and
//! `Unknown` never quits.
//!
//! Windows on ANOTHER desktop are the opposite case: the screen does not show them and the AX list
//! cannot see them (it is Space-filtered), so the CG pass asks the same SkyLight Space query the
//! switcher's admission policy uses. A window parked on another desktop (or a fullscreen Space) keeps
//! the app alive; one with no Space at all is an internal surface; evidence that cannot be read makes
//! the transaction decline to quit.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use super::*;

/// How long between the two post-close readings. The delay is what absorbs the destroy-then-recreate
/// window transitions the reference implementations were bitten by; it is policy, not a measurement,
/// so it lives here as a named value a test can read.
const RECHECK_DELAY: Duration = Duration::from_millis(500);
/// The whole transaction's budget, measured from the pre-check: pre-check + close + two readings +
/// the destruction wait all have to fit, and a late result may not request a quit.
const TOTAL_BUDGET: Duration = Duration::from_millis(2000);
/// How long the pre-check may take. It runs before the close, so it must stay short enough to be a
/// detail of the close rather than a delay on it; evidence gathered past this budget describes a
/// moment too far from the close to be used.
const PRECHECK_BUDGET: Duration = Duration::from_millis(250);
/// `kAXErrorNoValue`: the attribute exists but has no value, which for `AXWindows` means the app
/// has no window. Not an error for this module, while a failure to complete the query is.
const K_AX_NO_VALUE: AXError = -25212;
/// AX messaging timeout for this module's reads. Same bounded-AX rule as the rest of the collector:
/// an unresponsive app fails fast instead of stalling a worker for the default ten seconds.
const AX_TIMEOUT: f32 = 0.2;
/// The app's own Finder: its Quit is disabled in the system UI, and a quit request is either
/// ignored or answered by launchd relaunching it. Never a candidate.
const FINDER_BUNDLE_ID: &str = "com.apple.finder";

/// The switch and identity frozen on the main thread when a close starts.
///
/// The identity travels with the transaction because a PID is not identity: the process that owns a
/// reused PID is a different application, and the quit request must reach the app whose window was
/// closed.
#[derive(Clone, Debug)]
pub(crate) struct CloseTarget {
    pub(crate) pid: i32,
    pub(crate) cgwid: u32,
    identity_key: String,
    process_start_time_us: Option<u64>,
}

/// What one reading can prove about the pre-check's question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Judgement {
    /// Both sources answered and the only window is the one named.
    SoleWindow,
    /// Another user window is still there.
    Present,
    /// A source could not be read or a window could not be identified: no decision.
    Unknown,
}

/// A close this module has taken responsibility for.
pub(crate) struct PendingQuit {
    target: CloseTarget,
    ticket: Ticket,
    deadline: Instant,
}

/// Everything one fresh reading can say about the windows a process still has.
///
/// "User window" here is the measured rule both reference implementations converged on: a window the
/// screen is showing, or one Accessibility reports as minimized. An app that answers a close by
/// HIDING its window (TextEdit does; so do several document apps) leaves an AX element behind that is
/// neither on screen nor minimized, and that leftover must not keep the app running -- otherwise the
/// feature would never fire for those apps, and a second close of the same app would be blocked by the
/// first close's leftover. A minimized window is one the user can restore, so it always counts.
#[derive(Default, Clone, Debug)]
struct Evidence {
    /// Windows Accessibility reports as minimized.
    minimized: HashSet<u32>,
    /// Windows the window server says are on screen (any layer above the desktop's own).
    on_screen: HashSet<u32>,
    /// Off-screen windows the window server places on a DIFFERENT desktop: the user still has them, so
    /// this app must not be quit.
    other_desktop: HashSet<u32>,
    /// A source failed or a window could not be identified: no decision may be drawn from this.
    unknown: bool,
}

impl Evidence {
    /// The windows the user still has, or `None` when the evidence cannot say.
    fn user_windows(&self) -> Option<HashSet<u32>> {
        if self.unknown {
            return None;
        }
        let mut users = self.on_screen.clone();
        users.extend(self.minimized.iter().copied());
        users.extend(self.other_desktop.iter().copied());
        Some(users)
    }

    /// Whether the window this close targeted is still on screen.
    fn target_on_screen(&self, target: u32) -> Option<bool> {
        Some(self.on_screen.contains(&target))
    }
}

/// Capture what a close needs, or `None` when the feature is off for that surface.
pub(crate) fn target_for_switcher_close(pid: i32, cgwid: u32) -> Option<CloseTarget> {
    if !CONFIG.read().unwrap().windows.quit_on_last_window_close {
        return None;
    }
    target_for(pid, cgwid)
}

/// The Dock preview's form: the panel already holds the identity it verified, so it hands it over
/// instead of resolving it a second time.
pub(crate) fn target_for_dock_close(
    pid: i32,
    cgwid: u32,
    identity_key: String,
    process_start_time_us: Option<u64>,
) -> Option<CloseTarget> {
    let cfg = CONFIG.read().unwrap();
    if !cfg.dock_preview.enabled || !cfg.dock_preview.quit_on_last_window_close {
        return None;
    }
    drop(cfg);
    let mut target = target_for(pid, cgwid)?;
    target.identity_key = identity_key;
    target.process_start_time_us = process_start_time_us;
    Some(target)
}

fn target_for(pid: i32, cgwid: u32) -> Option<CloseTarget> {
    if pid <= 0 || cgwid == 0 || pid == std::process::id() as i32 {
        return None;
    }
    // Resolved here, on the thread that starts the close, so the worker never has to re-derive an
    // identity the transaction is supposed to be bound to.
    let identity = unsafe { crate::app_identity::resolve_app_identity(pid) };
    Some(CloseTarget {
        pid,
        cgwid,
        identity_key: identity.key,
        process_start_time_us: identity.process_start_time_us,
    })
}

/// The bounded pre-check, run on the close worker *before* the AX close is sent.
///
/// Returns `Some` only when the target window is part of fresh evidence and is the app's only user
/// window. Anything less disqualifies the close from auto-quitting, and the close itself proceeds
/// exactly as it did before.
pub(crate) fn precheck(target: CloseTarget) -> Option<PendingQuit> {
    let started = Instant::now();
    // The worker thread has no run loop to drain Objective-C temporaries for it.
    objc2::rc::autoreleasepool(|_| {
        if !eligible(&target) {
            log_debug!(
                "[last-window] pre-check skipped pid={} cgwid={}: not an eligible instance",
                target.pid,
                target.cgwid
            );
            return None;
        }
        let precheck_deadline = started + PRECHECK_BUDGET;
        let evidence = read_evidence(target.pid, Some(precheck_deadline));
        let judgement = judge_precheck(&evidence, target.cgwid);
        // The pre-check is bounded like the rest of the transaction: a reading that took too long
        // means the evidence describes a moment too far from the close to be used.
        let within_budget = started.elapsed() <= PRECHECK_BUDGET;
        if !precheck_accepts(judgement, within_budget) {
            log_debug!(
                "[last-window] pre-check disqualified pid={} cgwid={}: judgement={:?} within_budget={} elapsed={:?} evidence={:?}",
                target.pid,
                target.cgwid,
                judgement,
                within_budget,
                started.elapsed(),
                evidence
            );
            return None;
        }
        let deadline = started + TOTAL_BUDGET;
        // No permission while another request for this instance is being sent: one instance gets
        // one request per overlapping close, and a request in flight must not be duplicated.
        let ticket = registry().lock().unwrap().begin(target.pid, deadline)?;
        Some(PendingQuit {
            target,
            ticket,
            deadline,
        })
    })
}

/// The pre-check's accept rule, pure so that "another window is open" and "the budget is spent" are
/// assertable without a target application: neither may arm a quit.
fn precheck_accepts(judgement: Judgement, within_budget: bool) -> bool {
    judgement == Judgement::SoleWindow && within_budget
}

/// The post-close half: two delayed readings must agree that the app has no user window left, then
/// the quit is requested. Runs on the close worker *after* the AX result was published, so the card
/// commit never waits for it.
pub(crate) fn finish(pending: PendingQuit) {
    let source = LiveSource::new();
    let outcome = complete(&source, &pending.target, &pending);
    release(pending, outcome);
}

/// Drop a transaction whose close was refused: the instance must be free for the next close
/// immediately.
fn abandon(pending: PendingQuit, reason: &'static str) {
    release(pending, reason);
}

fn release(pending: PendingQuit, outcome: &'static str) {
    let PendingQuit { target, ticket, .. } = pending;
    registry().lock().unwrap().end(ticket);
    log_debug!(
        "[last-window] pid={} cgwid={}: {}",
        target.pid,
        target.cgwid,
        outcome
    );
}

/// The transaction's contact with the machine, separated so the "request a quit or not" decision can
/// be asserted without a screen or a target application.
trait TransactionSource {
    fn alive(&self, pid: i32) -> bool;
    fn sleep_before(&self, delay: Duration, deadline: Instant) -> bool;
    fn evidence(&self, target: &CloseTarget) -> Evidence;
    /// Resolve and verify the instance the request would go to. Runs BEFORE the ticket is spent, so a
    /// replaced instance cannot burn the one request.
    fn prepare_quit(&self, target: &CloseTarget) -> bool;
    fn request_quit(&self, target: &CloseTarget) -> bool;
}

/// The live implementation. `prepared` OWNS the `NSRunningApplication` object `prepare_quit` resolved
/// and verified: the pool that yielded it may drain before the send, so a bare pointer would dangle.
struct LiveSource {
    prepared: std::cell::RefCell<Option<objc2::rc::Retained<AnyObject>>>,
}

impl LiveSource {
    fn new() -> Self {
        Self {
            prepared: std::cell::RefCell::new(None),
        }
    }
}

impl TransactionSource for LiveSource {
    fn alive(&self, pid: i32) -> bool {
        process_alive(pid)
    }

    fn sleep_before(&self, delay: Duration, deadline: Instant) -> bool {
        sleep_within(delay, deadline)
    }

    fn evidence(&self, target: &CloseTarget) -> Evidence {
        read_evidence(target.pid, None)
    }

    fn prepare_quit(&self, target: &CloseTarget) -> bool {
        objc2::rc::autoreleasepool(|_| unsafe {
            let app: *mut AnyObject = msg_send![
                class!(NSRunningApplication),
                runningApplicationWithProcessIdentifier: target.pid
            ];
            if app.is_null() {
                return false;
            }
            let Some(owned) = objc2::rc::Retained::retain_autoreleased(app) else {
                return false;
            };
            let identity = crate::app_identity::identity_of_app(app, target.pid);
            if identity.key != target.identity_key
                || identity.process_start_time_us != target.process_start_time_us
                || target.process_start_time_us.is_none()
            {
                return false;
            }
            *self.prepared.borrow_mut() = Some(owned);
            true
        })
    }

    fn request_quit(&self, _target: &CloseTarget) -> bool {
        let prepared = self.prepared.borrow();
        let Some(app) = prepared.as_ref() else {
            return false;
        };
        objc2::rc::autoreleasepool(|_| unsafe { msg_send![&**app, terminate] })
    }
}

fn complete(
    source: &dyn TransactionSource,
    target: &CloseTarget,
    pending: &PendingQuit,
) -> &'static str {
    objc2::rc::autoreleasepool(|_| complete_within(source, target, pending))
}

fn complete_within(
    source: &dyn TransactionSource,
    target: &CloseTarget,
    pending: &PendingQuit,
) -> &'static str {
    let deadline = pending.deadline;
    if !source.alive(target.pid) {
        return "aborted, the app already quit";
    }
    if !source.sleep_before(RECHECK_DELAY, deadline) {
        return "aborted, the recheck budget was spent before the first reading";
    }
    if !close_evidence_holds(&source.evidence(target), target.cgwid) {
        return "aborted, the app still has a user window at the first recheck";
    }
    if !source.sleep_before(RECHECK_DELAY, deadline) {
        return "aborted, the recheck budget was spent before the second reading";
    }
    if !close_evidence_holds(&source.evidence(target), target.cgwid) {
        return "aborted, a window appeared at the second recheck";
    }
    // The instance is prepared BEFORE the ticket is spent: a replaced or dead instance must not burn
    // this instance's one request, and preparing takes an identity round trip that must not happen
    // after the budget was claimed.
    if !source.prepare_quit(target) {
        return "aborted, the process is no longer the instance this close targeted";
    }
    // The generation, the deadline and "not already committed" are one decision: a superseded
    // transaction cannot slip a request in after a newer close took over.
    if !registry()
        .lock()
        .unwrap()
        .commit(pending.ticket, Instant::now())
    {
        return "aborted, the transaction was superseded or its budget had expired";
    }
    if source.request_quit(target) {
        "quit requested"
    } else {
        "aborted, the quit request could not be delivered"
    }
}

/// What "closed" means for this feature: the screen no longer shows the window this close targeted,
/// and no other user window of the app is left. Read twice, `RECHECK_DELAY` apart, so a window a
/// transition destroyed and recreated is caught by the second reading.
fn close_evidence_holds(evidence: &Evidence, target: u32) -> bool {
    let Some(users) = evidence.user_windows() else {
        return false;
    };
    if users.contains(&target) {
        // Still showing: a save sheet, or the app answered by doing nothing.
        return false;
    }
    users.is_empty()
}

/// Whether the instance may be asked to quit at all.
fn eligible(target: &CloseTarget) -> bool {
    if !super::is_regular_running_app(target.pid) {
        return false;
    }
    if unsafe { bundle_identifier(target.pid) }.as_deref() == Some(FINDER_BUNDLE_ID) {
        return false;
    }
    crate::app_identity::is_same_instance(
        target.pid,
        &target.identity_key,
        target.process_start_time_us,
    )
}

fn process_alive(pid: i32) -> bool {
    // Called in a polling loop on a worker thread: drain the autoreleased application handle per call
    // instead of letting a two-second wait accumulate them.
    objc2::rc::autoreleasepool(|_| unsafe {
        let app: *mut AnyObject = msg_send![
            class!(NSRunningApplication),
            runningApplicationWithProcessIdentifier: pid
        ];
        if app.is_null() {
            return false;
        }
        let terminated: bool = msg_send![app, isTerminated];
        !terminated
    })
}

unsafe fn bundle_identifier(pid: i32) -> Option<String> {
    let app: *mut AnyObject = msg_send![
        class!(NSRunningApplication),
        runningApplicationWithProcessIdentifier: pid
    ];
    if app.is_null() {
        return None;
    }
    let bundle: *mut AnyObject = msg_send![app, bundleIdentifier];
    if bundle.is_null() {
        return None;
    }
    // `bundleIdentifier` is a copy-property getter: the returned object is autoreleased (+0), so it
    // is only read, never released here.
    cf_to_rust_string(bundle as *const c_void)
}

fn sleep_within(duration: Duration, deadline: Instant) -> bool {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining < duration {
        return false;
    }
    std::thread::sleep(duration);
    true
}

/// The app's windows as one fresh reading describes them.
///
/// AX is read raw (no collector cache, no display filter): it names minimized windows, which the
/// screen does not show. The CG pass contributes what is actually on screen -- including a floating
/// HUD or palette, which AX may not list -- and deliberately ignores off-screen rows: those are either
/// the app's own leftovers (TextEdit keeps several) or windows on another desktop, which AX cannot see
/// (the documented blind spot of counting this way; a minimized one is still caught through AX).
fn read_evidence(pid: i32, deadline: Option<Instant>) -> Evidence {
    let mut evidence = Evidence::default();
    if !read_ax_windows(pid, deadline, &mut evidence) {
        evidence.unknown = true;
        return evidence;
    }
    if !read_cg_windows(pid, &mut evidence) {
        evidence.unknown = true;
    }
    evidence
}

/// Fill `minimized` with the app's minimized windows; false = the AX list could not be read.
fn read_ax_windows(pid: i32, deadline: Option<Instant>, evidence: &mut Evidence) -> bool {
    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return false;
        }
        AXUIElementSetMessagingTimeout(app, AX_TIMEOUT);
        let windows_key = cf_string_new("AXWindows");
        let mut value: *const c_void = std::ptr::null();
        let err = AXUIElementCopyAttributeValue(app, windows_key, &mut value);
        CFRelease(windows_key);
        CFRelease(app);
        if err == K_AX_CANNOT_COMPLETE || err == K_AX_INVALID_UI_ELEMENT {
            return false;
        }
        // `no value` is the documented "this app has no window" answer. Anything else that is not a
        // successful array -- a null value, a different type -- is unreadable, never "empty".
        if err == K_AX_NO_VALUE {
            return true;
        }
        if err != K_AX_SUCCESS || value.is_null() {
            return false;
        }
        if CFGetTypeID(value) != CFArrayGetTypeID() {
            CFRelease(value);
            return false;
        }
        let count = CFArrayGetCount(value);
        let minimized_key = cf_string_new("AXMinimized");
        for index in 0..count {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                CFRelease(minimized_key);
                CFRelease(value);
                return false;
            }
            let element = CFArrayGetValueAtIndex(value, index);
            if element.is_null() || CFGetTypeID(element) != AXUIElementGetTypeID() {
                evidence.unknown = true;
                continue;
            }
            let element = element as AXUIElementRef;
            AXUIElementSetMessagingTimeout(element, AX_TIMEOUT);
            let Some(window_id) = ax_window_cgwid(element) else {
                evidence.unknown = true;
                continue;
            };
            let mut minimized: *const c_void = std::ptr::null();
            let minimized = if AXUIElementCopyAttributeValue(element, minimized_key, &mut minimized)
                == K_AX_SUCCESS
                && !minimized.is_null()
            {
                let value = if CFGetTypeID(minimized) == crate::ffi::CFBooleanGetTypeID() {
                    Some(CFBooleanGetValue(minimized as *const _))
                } else {
                    evidence.unknown = true;
                    None
                };
                CFRelease(minimized);
                value
            } else {
                // No readable minimized state: not "not minimized".
                evidence.unknown = true;
                None
            };
            if minimized == Some(true) {
                evidence.minimized.insert(window_id);
            }
        }
        CFRelease(minimized_key);
        CFRelease(value);
        true
    }
}

/// Fill `on_screen` with the app's on-screen windows; false = the window list could not be read.
fn read_cg_windows(pid: i32, evidence: &mut Evidence) -> bool {
    // A hidden application shows nothing and every one of its windows is off screen: judging it now
    // would read "hidden" as "closed". Never request a quit for it (the next close can be judged).
    if app_hidden(pid) == Some(true) {
        evidence.unknown = true;
        return true;
    }
    let mut off_screen = Vec::new();
    unsafe {
        let array = CGWindowListCopyWindowInfo(K_C_G_WINDOW_LIST_OPTION_ALL, 0);
        if array.is_null() {
            return false;
        }
        let count = CFArrayGetCount(array);
        for index in 0..count {
            let dict = CFArrayGetValueAtIndex(array, index);
            if dict.is_null() {
                continue;
            }
            let Some(owner) = cf_dict_get_i32(dict, CgKey::OwnerPid) else {
                // A row whose owner cannot be read could be this app's window.
                CFRelease(array);
                return false;
            };
            if owner != pid {
                continue;
            }
            let Some(number) = cf_dict_get_u32(dict, CgKey::Number) else {
                evidence.unknown = true;
                continue;
            };
            let Some(layer) = cf_dict_get_i32(dict, CgKey::Layer) else {
                evidence.unknown = true;
                continue;
            };
            let Some(alpha) = cf_dict_get_f64(dict, CgKey::Alpha) else {
                evidence.unknown = true;
                continue;
            };
            let Some((_, _, width, height)) = cf_dict_get_bounds(dict, CgKey::Bounds) else {
                evidence.unknown = true;
                continue;
            };
            // A window with no area or no opacity is not something the user can restore.
            if alpha <= 0.0 || width <= 0.0 || height <= 0.0 {
                continue;
            }
            // Below layer 0 are the desktop's own levels, never an application's user window.
            if layer < 0 {
                continue;
            }
            if cf_dict_get_bool(dict, CgKey::IsOnscreen) == Some(true) {
                evidence.on_screen.insert(number);
            } else {
                off_screen.push(number);
            }
        }
        CFRelease(array);
    }
    classify_off_screen(pid, &off_screen, evidence);
    true
}

/// Ask the window server where each off-screen window lives.
///
/// On the CURRENT desktop an off-screen window is the app's own hidden surface -- the window it just
/// hid on close, or a palette it keeps out of sight -- and does not keep it running. On ANOTHER desktop
/// it is a window the user still has, and must block the quit. The Space query is the same SkyLight
/// evidence the switcher's admission policy uses; when it cannot answer, the transaction declines to
/// quit rather than guess.
fn classify_off_screen(_pid: i32, off_screen: &[u32], evidence: &mut Evidence) {
    use crate::space_groups::MembershipScope;
    if off_screen.is_empty() {
        return;
    }
    let rows = crate::skylight::window_rows(off_screen);
    let Ok(snapshot) = super::space_membership::query_with_provider(
        &super::space_membership::SkyLightMembershipProvider,
        off_screen,
        &rows,
    ) else {
        evidence.unknown = true;
        return;
    };
    for window_id in off_screen {
        let Some(memberships) = snapshot.window_space_ids.get(window_id) else {
            evidence.unknown = true;
            continue;
        };
        if memberships.is_empty() {
            // No Space at all: an internal surface the window server does not place on any desktop,
            // so it cannot be a window the user has.
            continue;
        }
        match snapshot.topology.membership_scope(memberships) {
            // The app's own off-screen surface on the desktop the user is looking at.
            MembershipScope::CurrentSpace => {}
            // A window parked on another desktop (or a fullscreen Space): the user still has it.
            MembershipScope::OtherDesktop | MembershipScope::FullscreenSpace => {
                evidence.other_desktop.insert(*window_id);
            }
            // No accepted evidence about where it lives: decline to quit.
            MembershipScope::Unknown => evidence.unknown = true,
        }
    }
}

/// Whether the application is hidden, or `None` when that cannot be read.
fn app_hidden(pid: i32) -> Option<bool> {
    objc2::rc::autoreleasepool(|_| unsafe {
        let app: *mut AnyObject = msg_send![
            class!(NSRunningApplication),
            runningApplicationWithProcessIdentifier: pid
        ];
        if app.is_null() {
            return None;
        }
        let hidden: bool = msg_send![app, isHidden];
        Some(hidden)
    })
}

/// Judge the pre-check: the target must be present and be the app's only user window.
fn judge_precheck(evidence: &Evidence, target: u32) -> Judgement {
    let Some(mut users) = evidence.user_windows() else {
        return Judgement::Unknown;
    };
    if !users.remove(&target) {
        // The window this close is about is not a user window in the evidence at all: the reading
        // cannot prove that closing it empties the app.
        return Judgement::Unknown;
    }
    if users.is_empty() {
        Judgement::SoleWindow
    } else {
        Judgement::Present
    }
}

/// Run one close through the whole transaction: bounded pre-check, the caller's close, the caller's
/// publication of the result, and only then the evidence work that may request a quit.
///
/// This is the one body both surfaces run, and it is the unit the M2 smoke runner drives too. The
/// caller keeps its own bookkeeping in `publish` -- the switcher publishes its AX result and asks
/// the main thread to commit the card, the Dock preview queues its close outcome -- because neither
/// of those may wait for the two delayed readings.
pub(crate) fn close_and_maybe_quit(
    target: Option<CloseTarget>,
    close: impl FnOnce() -> bool,
    publish: impl FnOnce(bool),
) -> bool {
    close_and_maybe_quit_with(precheck, target, close, publish)
}

/// The entry with the pre-check as a parameter, so an entry-level test can refuse or arm a
/// transaction without a target application and still assert what the entry does.
fn close_and_maybe_quit_with(
    precheck: impl FnOnce(CloseTarget) -> Option<PendingQuit>,
    target: Option<CloseTarget>,
    close: impl FnOnce() -> bool,
    publish: impl FnOnce(bool),
) -> bool {
    let pending = target.and_then(precheck);
    let accepted = close();
    publish(accepted);
    match (accepted, pending) {
        (true, Some(pending)) => finish(pending),
        // The close was refused: nothing may stay registered for a window that was never closed.
        (false, Some(pending)) => abandon(pending, "aborted, the close itself was refused"),
        (_, None) => {}
    }
    accepted
}

// ---------------------------------------------------------------------------
// M2 smoke runner
// ---------------------------------------------------------------------------

/// Why a smoke run could not reach (or hold) its assertions.
pub(crate) enum SmokeFailure {
    /// An environment limit: a real instance of the controlled target is already running.
    NotRun(String),
    Failed(String),
}

/// The controlled target for the smoke: a document app that stays open when its last window closes,
/// which is exactly the macOS behaviour this feature exists to override.
const SMOKE_TARGET_BUNDLE: &str = "com.apple.TextEdit";

/// Drive the real transaction against a controlled target in a child process.
///
/// Two runs, because the switch has to be shown to do both of its jobs: with both switches off the
/// window closes and the app stays (the behaviour every user has today), and with them on the same
/// close quits the app once the destruction proof and the two empty readings hold. Between the two,
/// the switcher and Dock switches are moved independently, so each surface's `target_for_*` is shown
/// to read its own switch rather than to follow the other one.
///
/// The close runs on a worker thread while this thread keeps the main run loop turning: the
/// WindowServer destruction notification the decision needs is dispatched by that loop, and the
/// production shape is the same (a close worker, a live main thread).
pub(crate) fn smoke_runner() -> Result<usize, SmokeFailure> {
    crate::assert_main_thread("last_window::smoke_runner");
    if !textedit_pids().is_empty() {
        return Err(SmokeFailure::NotRun(
            "TextEdit is already running; the smoke will not touch a user instance".into(),
        ));
    }
    let original = CONFIG
        .read()
        .map(|cfg| {
            (
                cfg.windows.quit_on_last_window_close,
                cfg.dock_preview.quit_on_last_window_close,
                cfg.dock_preview.enabled,
            )
        })
        .unwrap_or((false, false, true));
    let _guard = SwitchGuard {
        switcher: original.0,
        dock_switch: original.1,
        dock_enabled: original.2,
    };
    // The preview panel's close button only exists while the feature is on; the user's own master
    // switch is restored on every path and never persisted.
    set_dock_enabled(true);
    let mut started = StartedTargets::default();
    let mut checks = 0usize;

    // --- Both switches off: the window closes, the app stays. ---
    set_switches(false, false);
    let (candidate, document_stem) = start_target(&mut started, "a")?;
    let pid = candidate.0.pid;
    checks += 1; // the controlled target started
    let wid = target_window(pid, &document_stem, Duration::from_secs(8)).ok_or_else(|| {
        SmokeFailure::Failed("the controlled target did not show this run's document".into())
    })?;
    // Only now is the instance registered for cleanup: until the document title ties this process to
    // the window, it could be a user instance that started at the same moment.
    if !started.register(candidate) {
        return Err(SmokeFailure::NotRun(
            "the controlled target was replaced before its window could be bound".into(),
        ));
    }
    log_debug!("[last-window] smoke case A target pid={} wid={}", pid, wid);
    checks += 1; // and this run's document window is up
    if target_for_switcher_close(pid, wid).is_some() {
        return Err(SmokeFailure::Failed(
            "the switcher switch is off but produced a close target".into(),
        ));
    }
    checks += 1;
    if target_for_dock_close(pid, wid, identity_key(pid), identity_start(pid)).is_some() {
        return Err(SmokeFailure::Failed(
            "the Dock switch is off but produced a close target".into(),
        ));
    }
    checks += 1;
    let target = started
        .target(pid)
        .ok_or_else(|| SmokeFailure::Failed("the registered target disappeared".into()))?;
    run_close(target, wid, false)?;
    if !wait_window_gone(pid, wid, Duration::from_secs(3)) {
        return Err(SmokeFailure::Failed(
            "the window did not close, so the rest of the run would prove nothing".into(),
        ));
    }
    checks += 1;
    // Longer than the whole enabled-path budget: an implementation that quit with the switch off has
    // finished by now.
    pump(2.2);
    if !process_alive(pid) {
        return Err(SmokeFailure::Failed(
            "the app quit with the switch off".into(),
        ));
    }
    checks += 1;
    started.terminate(pid);

    // --- The switcher on, the Dock switch still off: each surface reads its own switch. ---
    set_switches(true, false);
    let (candidate, document_stem) = start_target(&mut started, "b")?;
    let pid = candidate.0.pid;
    let wid = target_window(pid, &document_stem, Duration::from_secs(8)).ok_or_else(|| {
        SmokeFailure::Failed("the second target did not show this run's document".into())
    })?;
    if !started.register(candidate) {
        return Err(SmokeFailure::NotRun(
            "the second target was replaced before its window could be bound".into(),
        ));
    }
    log_debug!("[last-window] smoke case B target pid={} wid={}", pid, wid);
    checks += 1;
    if target_for_switcher_close(pid, wid).is_none() {
        return Err(SmokeFailure::Failed(
            "the switcher switch is on but produced no close target".into(),
        ));
    }
    checks += 1;
    if target_for_dock_close(pid, wid, identity_key(pid), identity_start(pid)).is_some() {
        return Err(SmokeFailure::Failed(
            "the Dock switch is off but followed the switcher's".into(),
        ));
    }
    checks += 1;
    // Turn the Dock switch on for the same instance: its own read must now produce a target.
    set_switches(true, true);
    if target_for_dock_close(pid, wid, identity_key(pid), identity_start(pid)).is_none() {
        return Err(SmokeFailure::Failed(
            "the Dock switch is on but produced no close target".into(),
        ));
    }
    checks += 1;
    let target = started
        .target(pid)
        .ok_or_else(|| SmokeFailure::Failed("the second registered target disappeared".into()))?;
    run_close(target, wid, true)?;
    if !wait_window_gone(pid, wid, Duration::from_secs(3)) {
        return Err(SmokeFailure::Failed(
            "the window did not close with the switch on".into(),
        ));
    }
    checks += 1;
    let deadline = Instant::now() + Duration::from_secs(8);
    while process_alive(pid) && Instant::now() < deadline {
        pump(0.05);
    }
    if process_alive(pid) {
        return Err(SmokeFailure::Failed(
            "the app did not quit after its last window closed".into(),
        ));
    }
    checks += 1;
    started.terminate(pid);

    // --- Two windows of one instance: the FIRST close must not quit, the LAST must. ---
    //
    // This is the promise's actual wording ("its last window"), which a one-window target cannot
    // distinguish from "any close". The pre-check refuses to arm while another window remains, so
    // the first close is a plain close and the second one quits.
    set_switches(true, false);
    let (candidate, stems) = start_target_with_documents(&mut started, "c", 2)?;
    let pid = candidate.0.pid;
    let mut windows = Vec::with_capacity(stems.len());
    for stem in &stems {
        match target_window(pid, stem, Duration::from_secs(10)) {
            Some(wid) => windows.push(wid),
            None => {
                return Err(SmokeFailure::NotRun(
                    "the two documents did not appear as two separate windows (the platform may have tabbed them)"
                        .into(),
                ));
            }
        }
    }
    if !started.register(candidate) {
        return Err(SmokeFailure::NotRun(
            "the two-window target was replaced before its windows could be bound".into(),
        ));
    }
    checks += 1; // two document windows bound to one instance
    let target = started
        .target(pid)
        .ok_or_else(|| SmokeFailure::Failed("the two-window target disappeared".into()))?;
    // First window: another one remains, so this close must not arm a quit.
    run_close(target, windows[0], true)?;
    if !wait_window_gone(pid, windows[0], Duration::from_secs(3)) {
        return Err(SmokeFailure::Failed(
            "the first of two windows did not close".into(),
        ));
    }
    checks += 1;
    // Longer than the enabled-path budget: a quit caused by the first close would have landed.
    pump(2.5);
    if !process_alive(pid) {
        return Err(SmokeFailure::Failed(
            "the app quit after closing the first of its two windows".into(),
        ));
    }
    checks += 1;
    // Last window: the same close now quits.
    run_close(target, windows[1], true)?;
    if !wait_window_gone(pid, windows[1], Duration::from_secs(3)) {
        return Err(SmokeFailure::Failed("the last window did not close".into()));
    }
    checks += 1;
    let deadline = Instant::now() + Duration::from_secs(8);
    while process_alive(pid) && Instant::now() < deadline {
        pump(0.05);
    }
    if process_alive(pid) {
        return Err(SmokeFailure::Failed(
            "the app did not quit after its last window closed".into(),
        ));
    }
    checks += 1;
    Ok(checks)
}

/// Restores the in-memory switches when the runner returns, on every path.
struct SwitchGuard {
    switcher: bool,
    dock_switch: bool,
    dock_enabled: bool,
}

impl Drop for SwitchGuard {
    fn drop(&mut self) {
        set_switches(self.switcher, self.dock_switch);
        set_dock_enabled(self.dock_enabled);
    }
}

/// A controlled target this run started, by instance and not just by PID: cleanup must never reach a
/// process that merely inherited a reused PID.
struct StartedTarget {
    pid: i32,
    identity_key: String,
    process_start_time_us: Option<u64>,
}

/// The instance this run started, captured at spawn and before anything has been proven about its
/// window. It is NOT registered for cleanup: only `StartedTargets::register` may do that, and only
/// after the document title tied the window to this identity.
struct StartedCandidate(StartedTarget);

impl StartedTarget {
    fn terminate(&self) {
        if process_alive(self.pid) {
            crate::window_collector::terminate_application_verified(
                self.pid,
                &self.identity_key,
                self.process_start_time_us,
                &|| true,
            );
        }
    }
}

/// The target instances this run started, terminated on every path.
#[derive(Default)]
struct StartedTargets(Vec<StartedTarget>, Vec<std::path::PathBuf>);

impl StartedTargets {
    /// Register an instance for cleanup. Called only after the window was tied to this run's
    /// document, and only while the process is still the instance that was started: a reused PID must
    /// never be terminated on behalf of this run. Returns false when the instance changed.
    fn register(&mut self, candidate: StartedCandidate) -> bool {
        let target = candidate.0;
        if !crate::app_identity::is_same_instance(
            target.pid,
            &target.identity_key,
            target.process_start_time_us,
        ) {
            return false;
        }
        self.0.push(target);
        true
    }

    fn target(&self, pid: i32) -> Option<&StartedTarget> {
        self.0.iter().find(|target| target.pid == pid)
    }

    fn terminate(&mut self, pid: i32) {
        if let Some(started) = self.0.iter().find(|started| started.pid == pid) {
            started.terminate();
        }
        self.0.retain(|started| started.pid != pid);
    }
}

impl Drop for StartedTargets {
    fn drop(&mut self) {
        for target in std::mem::take(&mut self.0) {
            target.terminate();
        }
        for path in std::mem::take(&mut self.1) {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn set_switches(switcher: bool, dock: bool) {
    if let Ok(mut cfg) = CONFIG.write() {
        cfg.windows.quit_on_last_window_close = switcher;
        cfg.dock_preview.quit_on_last_window_close = dock;
    }
}

fn set_dock_enabled(on: bool) {
    if let Ok(mut cfg) = CONFIG.write() {
        cfg.dock_preview.enabled = on;
    }
}

fn identity_key(pid: i32) -> String {
    unsafe { crate::app_identity::resolve_app_identity(pid).key }
}

fn identity_start(pid: i32) -> Option<u64> {
    unsafe { crate::app_identity::resolve_app_identity(pid).process_start_time_us }
}

fn textedit_pids() -> Vec<i32> {
    // The smoke runner runs before the application's run loop drains anything, so the autoreleased
    // application handles this enumerates are drained here.
    objc2::rc::autoreleasepool(|_| unsafe {
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let apps: *mut AnyObject = msg_send![workspace, runningApplications];
        if apps.is_null() {
            return Vec::new();
        }
        let count: usize = msg_send![apps, count];
        let mut pids = Vec::new();
        for index in 0..count {
            let app: *mut AnyObject = msg_send![apps, objectAtIndex: index as isize];
            if app.is_null() {
                continue;
            }
            let pid: i32 = msg_send![app, processIdentifier];
            if pid > 0 && bundle_identifier(pid).as_deref() == Some(SMOKE_TARGET_BUNDLE) {
                pids.push(pid);
            }
        }
        pids
    })
}

/// Start a fresh instance of the controlled target with one empty document.
fn start_target(
    started: &mut StartedTargets,
    tag: &str,
) -> Result<(StartedCandidate, String), SmokeFailure> {
    let (candidate, mut stems) = start_target_with_documents(started, tag, 1)?;
    let stem = stems.remove(0);
    Ok((candidate, stem))
}

/// Start one instance of the controlled target with `count` documents, each named uniquely for this
/// run: the names are how a window is later tied to this instance.
fn start_target_with_documents(
    started: &mut StartedTargets,
    tag: &str,
    count: usize,
) -> Result<(StartedCandidate, Vec<String>), SmokeFailure> {
    let mut paths = Vec::with_capacity(count);
    let mut stems = Vec::with_capacity(count);
    for index in 0..count {
        let path = std::env::temp_dir().join(format!(
            "omt-last-window-smoke-{}-{tag}-{index}.txt",
            std::process::id()
        ));
        std::fs::write(&path, "oh-my-tab smoke target\n").map_err(|error| {
            SmokeFailure::Failed(format!("could not write the smoke document: {error}"))
        })?;
        stems.push(
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
        paths.push(path);
    }
    let before = textedit_pids();
    // `/usr/bin/open -n` asks for a NEW instance, so a stale one (from a crashed run) can never be
    // mistaken for this run's target; every document is handed to that one instance.
    let mut command = std::process::Command::new("/usr/bin/open");
    command.args(["-n", "-a", "TextEdit"]);
    for path in &paths {
        command.arg(path);
    }
    command
        .status()
        .map_err(|error| SmokeFailure::Failed(format!("could not start the target: {error}")))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        pump(0.05);
        if let Some(pid) = textedit_pids()
            .into_iter()
            .find(|pid| !before.contains(pid))
        {
            // The identity is captured NOW, at spawn, and travels with the candidate: re-resolving it
            // after the title check would let a reused PID smuggle another process into cleanup.
            let identity = unsafe { crate::app_identity::resolve_app_identity(pid) };
            // Not registered yet: the caller registers the instance only after the document titles
            // tie it to this run's windows. Until then, cleanup must not touch it.
            // The documents stay until the run ends: their names are what proves the windows belong
            // to the instance this run started.
            started.1.extend(paths.iter().cloned());
            return Ok((
                StartedCandidate(StartedTarget {
                    pid,
                    identity_key: identity.key,
                    process_start_time_us: identity.process_start_time_us,
                }),
                stems,
            ));
        }
        if Instant::now() >= deadline {
            for path in &paths {
                let _ = std::fs::remove_file(path);
            }
            return Err(SmokeFailure::Failed(
                "the controlled target did not start".into(),
            ));
        }
    }
}

/// The window that shows this run's document.
///
/// Binding by the DOCUMENT's name, not by "the first new TextEdit window": a user instance started at
/// the same moment must never be mistaken for the target, and a window that cannot be tied to this
/// run's document ends the run instead. The titles are read FRESH from the target's own AX window
/// list: the collector's copy is a cache filled by refresh passes this smoke process never runs.
fn target_window(pid: i32, document_stem: &str, timeout: Duration) -> Option<u32> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        pump(0.05);
        if let Some(window) = window_titled(pid, document_stem, deadline) {
            return Some(window);
        }
    }
    None
}

/// The id of the target's window whose AX title names `needle`, read fresh and without the collector's
/// cache or display filters.
fn window_titled(pid: i32, needle: &str, deadline: Instant) -> Option<u32> {
    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return None;
        }
        AXUIElementSetMessagingTimeout(app, AX_TIMEOUT);
        let windows_key = cf_string_new("AXWindows");
        let mut value: *const c_void = std::ptr::null();
        let err = AXUIElementCopyAttributeValue(app, windows_key, &mut value);
        CFRelease(windows_key);
        CFRelease(app);
        if err != K_AX_SUCCESS || value.is_null() || CFGetTypeID(value) != CFArrayGetTypeID() {
            if !value.is_null() {
                CFRelease(value);
            }
            return None;
        }
        let title_key = cf_string_new("AXTitle");
        let count = CFArrayGetCount(value);
        let mut found = None;
        for index in 0..count {
            // The whole search shares the caller's deadline, not just each AX call's timeout.
            if Instant::now() >= deadline {
                break;
            }
            let element = CFArrayGetValueAtIndex(value, index);
            if element.is_null() || CFGetTypeID(element) != AXUIElementGetTypeID() {
                continue;
            }
            let element = element as AXUIElementRef;
            AXUIElementSetMessagingTimeout(element, AX_TIMEOUT);
            let mut title: *const c_void = std::ptr::null();
            let ok = AXUIElementCopyAttributeValue(element, title_key, &mut title) == K_AX_SUCCESS
                && !title.is_null();
            let title = if ok {
                // The value is only read as a string when it IS one: an unexpected type would crash
                // the smoke process instead of failing the runner.
                let text = if CFGetTypeID(title) == crate::ffi::CFStringGetTypeID() {
                    cf_to_rust_string(title)
                } else {
                    None
                };
                CFRelease(title);
                text
            } else {
                None
            };
            if title.is_some_and(|title| title.contains(needle)) {
                // A match that took past the deadline is not returned: the caller's budget bounds the
                // whole search, including the last element's read.
                if Instant::now() >= deadline {
                    break;
                }
                found = ax_window_cgwid(element);
                if found.is_some() {
                    break;
                }
            }
        }
        CFRelease(title_key);
        CFRelease(value);
        // The mapping itself can cross the deadline: a match that finished late is not returned.
        if Instant::now() >= deadline {
            return None;
        }
        found
    }
}

fn wait_window_gone(pid: i32, wid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        pump(0.05);
        // A quit request may already have ended the process; then every window of it is gone.
        if !process_alive(pid) {
            return true;
        }
        // Otherwise the specific window this run closed must stop being on screen.
        if read_evidence(pid, None).target_on_screen(wid) == Some(false) {
            return true;
        }
    }
    // The last word comes from the process: a dead target has no windows left.
    !process_alive(pid)
}

/// Run `close_and_maybe_quit` the way production does: the target is captured on the main thread,
/// the transaction runs on a worker, and this thread keeps the run loop turning until it ends.
///
/// The target comes from the instance this run started and had bound to its document. It is NOT
/// re-derived by PID: a replaced process must stop the run rather than be closed or quit on behalf of
/// the old one.
fn run_close(target: &StartedTarget, wid: u32, switch_on: bool) -> Result<(), SmokeFailure> {
    let pid = target.pid;
    if !crate::app_identity::is_same_instance(
        pid,
        &target.identity_key,
        target.process_start_time_us,
    ) {
        return Err(SmokeFailure::NotRun(
            "the controlled target was replaced before its window could be closed".into(),
        ));
    }
    let identity_key = target.identity_key.clone();
    let identity_start = target.process_start_time_us;
    let quit_target = if switch_on {
        Some(CloseTarget {
            pid,
            cgwid: wid,
            identity_key: identity_key.clone(),
            process_start_time_us: identity_start,
        })
    } else {
        None
    };
    let done = std::sync::Arc::new(AtomicBool::new(false));
    let flag = done.clone();
    let handle = std::thread::spawn(move || {
        close_and_maybe_quit(
            quit_target,
            || {
                // Same instance right before the action, so a replaced PID cannot be closed here.
                crate::app_identity::is_same_instance(pid, &identity_key, identity_start)
                    && crate::window_collector::close_ax_window(pid, wid)
            },
            |_| {},
        );
        flag.store(true, Ordering::Release);
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done.load(Ordering::Acquire) && Instant::now() < deadline {
        pump(0.02);
    }
    let finished = handle.join().is_ok() && done.load(Ordering::Acquire);
    if !finished {
        return Err(SmokeFailure::Failed(
            "the close transaction did not finish within its budget".into(),
        ));
    }
    Ok(())
}

/// Turn the main run loop for a while, so WindowServer notifications are dispatched.
///
/// A bare `CFRunLoopRunInMode` is NOT enough: the WindowServer notification port is serviced by
/// AppKit's own event loop, so the child has to drain AppKit's queue for the destruction events to
/// arrive at all (without this the runner sees zero callbacks and waits out its whole budget).
fn pump(seconds: f64) {
    objc2::rc::autoreleasepool(|_| unsafe {
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        if nsapp.is_null() {
            return;
        }
        let mode = crate::ffi::make_nsstring("kCFRunLoopDefaultMode");
        let deadline = Instant::now() + Duration::from_secs_f64(seconds.max(0.0));
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let date: *mut AnyObject = msg_send![
                class!(NSDate),
                dateWithTimeIntervalSinceNow: remaining.as_secs_f64()
            ];
            let event: *mut AnyObject = msg_send![
                nsapp,
                nextEventMatchingMask: u64::MAX,
                untilDate: date,
                inMode: mode,
                dequeue: true
            ];
            if event.is_null() {
                // The run loop had nothing to hand over until the deadline.
                break;
            }
            let _: () = msg_send![nsapp, sendEvent: event];
        }
        crate::ffi::CFRelease(mode as *const std::ffi::c_void);
        // Let non-AppKit main-thread sources (the event bridge's delivery selector) run as well.
        crate::ffi::CFRunLoopRunInMode(crate::ffi::kCFRunLoopDefaultMode, 0.0, 1u8);
    })
}

// ---------------------------------------------------------------------------
// Transaction registry
// ---------------------------------------------------------------------------

/// One close transaction's registration. A newer close of the same instance supersedes it: the
/// superseded ticket can no longer commit, and its own finish path just cleans up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ticket {
    pid: i32,
    generation: u64,
}

#[derive(Debug)]
struct Active {
    generation: u64,
    deadline: Instant,
    committed: bool,
}

#[derive(Debug, Default)]
struct Registry {
    next_generation: u64,
    active: HashMap<i32, Active>,
}

impl Registry {
    /// Register a new transaction for `pid`, unless this instance's request is already being sent.
    ///
    /// An entry stays until its `end`: while it is `committed` the request is in flight, and a newer
    /// close must not register a second one for the same instance. It still closes its window as
    /// usual; only the auto-quit is skipped.
    fn begin(&mut self, pid: i32, deadline: Instant) -> Option<Ticket> {
        if self.active.get(&pid).is_some_and(|active| active.committed) {
            return None;
        }
        self.next_generation += 1;
        let generation = self.next_generation;
        self.active.insert(
            pid,
            Active {
                generation,
                deadline,
                committed: false,
            },
        );
        Some(Ticket { pid, generation })
    }

    /// Take this transaction's one and only permission to request a quit.
    ///
    /// The generation check, the deadline check and the "not already committed" check happen in one
    /// critical section: a superseded transaction cannot slip a request in after a newer one took
    /// over, and neither can a transaction that ran past its budget.
    fn commit(&mut self, ticket: Ticket, now: Instant) -> bool {
        let Some(active) = self.active.get_mut(&ticket.pid) else {
            return false;
        };
        if active.generation != ticket.generation || active.committed || now > active.deadline {
            return false;
        }
        active.committed = true;
        true
    }

    fn end(&mut self, ticket: Ticket) {
        if self
            .active
            .get(&ticket.pid)
            .is_some_and(|active| active.generation == ticket.generation)
        {
            self.active.remove(&ticket.pid);
        }
    }
}

static REGISTRY: LazyLock<Mutex<Registry>> = LazyLock::new(|| Mutex::new(Registry::default()));

fn registry() -> &'static Mutex<Registry> {
    &REGISTRY
}

#[cfg(test)]
mod smoke_tests {
    use crate::smoke_verdict::smoke_runner;

    /// Drive the real close-and-quit transaction against a controlled document app in a child
    /// process: with the switch off the window closes and the app stays, with it on the same close
    /// quits the app. Needs a live session (it launches and closes a real application).
    // SMOKE_CELL=M2
    // SMOKE_DISTURBS=focus
    #[test]
    #[ignore]
    fn last_window_quit_smoke() {
        smoke_runner!("last_window_quit_smoke", 17, {
            crate::smoke_verdict::run_smoke_child(
                "last_window_quit_smoke",
                &["--smoke-last-window-quit"],
            );
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    fn evidence_with(on_screen: &[u32]) -> Evidence {
        Evidence {
            minimized: HashSet::new(),
            on_screen: on_screen.iter().copied().collect(),
            other_desktop: HashSet::new(),
            unknown: false,
        }
    }

    fn evidence_with_minimized(minimized: &[u32], on_screen: &[u32]) -> Evidence {
        Evidence {
            minimized: minimized.iter().copied().collect(),
            on_screen: on_screen.iter().copied().collect(),
            other_desktop: HashSet::new(),
            unknown: false,
        }
    }

    fn unreadable() -> Evidence {
        Evidence {
            unknown: true,
            ..Evidence::default()
        }
    }

    #[test]
    fn the_precheck_needs_the_target_and_no_other_user_window() {
        assert_eq!(
            judge_precheck(&evidence_with(&[7]), 7),
            Judgement::SoleWindow
        );
        // A window the pre-check never saw means the close cannot be attributed to this transaction.
        assert_eq!(judge_precheck(&evidence_with(&[8]), 7), Judgement::Unknown);
        // Another window keeps the app alive.
        assert_eq!(
            judge_precheck(&evidence_with(&[7, 9]), 7),
            Judgement::Present
        );
        // A MINIMIZED window is one the user can restore: it counts even though the screen does not
        // show it.
        assert_eq!(
            judge_precheck(&evidence_with_minimized(&[9], &[7]), 7),
            Judgement::Present
        );
        // Unreadable evidence is not "no windows".
        assert_eq!(judge_precheck(&unreadable(), 7), Judgement::Unknown);
    }

    #[test]
    fn a_hidden_leftover_does_not_keep_the_app_alive() {
        // TextEdit answers a close by hiding the window: its AX element survives, but it is neither on
        // screen nor minimized. That leftover is the window this close targeted, so nothing is left.
        let evidence = evidence_with(&[]);
        assert!(close_evidence_holds(&evidence, 7));
        // A leftover of a PREVIOUS close (another id, also hidden) must not block the next one
        // either: it is not a user window.
        assert!(close_evidence_holds(&evidence, 9));
    }

    #[test]
    fn a_window_on_another_desktop_blocks_the_quit() {
        // The user still has it; the switcher's own admission policy can see it too.
        let mut evidence = evidence_with(&[7]);
        evidence.other_desktop.insert(9);
        assert_eq!(judge_precheck(&evidence, 7), Judgement::Present);
        assert!(!close_evidence_holds(&evidence, 7));
    }

    #[test]
    fn a_minimized_second_window_blocks_the_quit() {
        // Two windows: one on screen (the one being closed), one minimized.
        assert!(!close_evidence_holds(
            &evidence_with_minimized(&[9], &[7]),
            7
        ));
    }

    #[test]
    fn a_target_still_on_screen_is_not_closed() {
        // The AX close was accepted but the window is still showing (a save sheet, or nothing happened).
        assert!(!close_evidence_holds(&evidence_with(&[7]), 7));
        // And an unreadable reading proves nothing.
        assert!(!close_evidence_holds(&unreadable(), 7));
    }

    #[test]
    fn the_precheck_arms_a_quit_only_for_the_only_window_and_only_inside_its_budget() {
        assert!(precheck_accepts(Judgement::SoleWindow, true));
        assert!(!precheck_accepts(Judgement::Present, true));
        assert!(!precheck_accepts(Judgement::Unknown, true));
        // A reading that took too long describes a moment too far from the close.
        assert!(!precheck_accepts(Judgement::SoleWindow, false));
    }

    #[test]
    fn a_newer_close_supersedes_an_older_one() {
        let now = Instant::now();
        let mut registry = Registry::default();
        let older = registry.begin(42, now + TOTAL_BUDGET).expect("registered");
        let newer = registry.begin(42, now + TOTAL_BUDGET).expect("registered");
        assert!(!registry.commit(older, now));
        assert!(registry.commit(newer, now));
        // Each ticket may request a quit once.
        assert!(!registry.commit(newer, now));
        // Ending the superseded ticket must not clear the newer transaction.
        registry.end(older);
        assert!(registry.active.contains_key(&42));
    }

    #[test]
    fn a_request_already_in_flight_is_not_duplicated() {
        let now = Instant::now();
        let mut registry = Registry::default();
        let first = registry.begin(42, now + TOTAL_BUDGET).expect("registered");
        assert!(registry.commit(first, now));
        // While the request is being sent, a second close of the same instance gets no permission.
        assert!(registry.begin(42, now + TOTAL_BUDGET).is_none());
        registry.end(first);
        assert!(registry.begin(42, now + TOTAL_BUDGET).is_some());
    }

    #[test]
    fn a_transaction_past_its_budget_cannot_commit() {
        let now = Instant::now();
        let mut registry = Registry::default();
        let ticket = registry.begin(7, now + TOTAL_BUDGET).expect("registered");
        assert!(!registry.commit(ticket, now + TOTAL_BUDGET + Duration::from_millis(1)));
    }

    #[test]
    fn ending_a_transaction_frees_the_instance_for_the_next_close() {
        let now = Instant::now();
        let mut registry = Registry::default();
        let first = registry.begin(7, now + TOTAL_BUDGET).expect("registered");
        assert!(registry.commit(first, now));
        registry.end(first);
        let second = registry.begin(7, now + TOTAL_BUDGET).expect("registered");
        assert!(registry.commit(second, now));
    }

    // --- the send decision, driven through the real completion path with scripted evidence ---

    struct Scripted {
        /// The evidence each reading returns, in order.
        readings: RefCell<Vec<Evidence>>,
        identity_matches: bool,
        requested: Cell<usize>,
    }

    impl Scripted {
        fn clean() -> Self {
            Self {
                readings: RefCell::new(vec![evidence_with(&[]), evidence_with(&[])]),
                identity_matches: true,
                requested: Cell::new(0),
            }
        }

        fn with_readings(readings: Vec<Evidence>) -> Self {
            Self {
                readings: RefCell::new(readings),
                ..Self::clean()
            }
        }
    }

    impl TransactionSource for Scripted {
        fn alive(&self, _pid: i32) -> bool {
            true
        }

        fn sleep_before(&self, _delay: Duration, _deadline: Instant) -> bool {
            true
        }

        fn evidence(&self, _target: &CloseTarget) -> Evidence {
            let mut readings = self.readings.borrow_mut();
            if readings.is_empty() {
                evidence_with(&[])
            } else {
                readings.remove(0)
            }
        }

        fn prepare_quit(&self, _target: &CloseTarget) -> bool {
            self.identity_matches
        }

        fn request_quit(&self, _target: &CloseTarget) -> bool {
            self.requested.set(self.requested.get() + 1);
            true
        }
    }

    fn scripted_target(pid: i32, cgwid: u32) -> CloseTarget {
        CloseTarget {
            pid,
            cgwid,
            identity_key: "test".to_string(),
            process_start_time_us: Some(1),
        }
    }

    fn run_scripted(source: &Scripted, pid: i32, cgwid: u32, deadline: Instant) -> &'static str {
        let ticket = registry()
            .lock()
            .unwrap()
            .begin(pid, deadline)
            .expect("a fresh instance registers");
        let pending = PendingQuit {
            target: scripted_target(pid, cgwid),
            ticket,
            deadline,
        };
        let outcome = complete(source, &pending.target, &pending);
        registry().lock().unwrap().end(pending.ticket);
        outcome
    }

    #[test]
    fn a_window_at_the_first_recheck_never_requests_a_quit() {
        let source = Scripted::with_readings(vec![evidence_with(&[9]), evidence_with(&[])]);
        let outcome = run_scripted(&source, 91001, 7001, Instant::now() + TOTAL_BUDGET);
        assert!(outcome.contains("first recheck"), "{outcome}");
        assert_eq!(source.requested.get(), 0);
    }

    #[test]
    fn a_target_still_on_screen_at_the_first_recheck_never_requests_a_quit() {
        let source = Scripted::with_readings(vec![evidence_with(&[7]), evidence_with(&[])]);
        let outcome = run_scripted(&source, 91005, 7, Instant::now() + TOTAL_BUDGET);
        assert!(outcome.contains("first recheck"), "{outcome}");
        assert_eq!(source.requested.get(), 0);
    }

    #[test]
    fn a_window_appearing_at_the_second_recheck_never_requests_a_quit() {
        let source = Scripted::with_readings(vec![evidence_with(&[]), evidence_with(&[9])]);
        let outcome = run_scripted(&source, 91002, 7002, Instant::now() + TOTAL_BUDGET);
        assert!(outcome.contains("second recheck"), "{outcome}");
        assert_eq!(source.requested.get(), 0);
    }

    #[test]
    fn unreadable_evidence_at_a_recheck_never_requests_a_quit() {
        let source = Scripted::with_readings(vec![unreadable(), evidence_with(&[])]);
        let outcome = run_scripted(&source, 91006, 7006, Instant::now() + TOTAL_BUDGET);
        assert!(outcome.contains("first recheck"), "{outcome}");
        assert_eq!(source.requested.get(), 0);
    }

    #[test]
    fn a_transaction_that_ran_past_its_budget_never_requests_a_quit() {
        // The send gate is asked with the REAL time: an expired ticket must be refused, which a clamped
        // "now" (the old bug) would have let through.
        let source = Scripted::clean();
        let deadline = Instant::now() - Duration::from_millis(1);
        let outcome = run_scripted(&source, 91007, 7007, deadline);
        assert!(outcome.contains("superseded or its budget"), "{outcome}");
        assert_eq!(source.requested.get(), 0);
    }

    #[test]
    fn a_replaced_instance_never_requests_a_quit() {
        let mut source = Scripted::clean();
        source.identity_matches = false;
        let outcome = run_scripted(&source, 91003, 7003, Instant::now() + TOTAL_BUDGET);
        assert!(outcome.contains("no longer the instance"), "{outcome}");
        assert_eq!(source.requested.get(), 0);
    }

    #[test]
    fn the_proven_case_requests_the_quit_exactly_once() {
        // Every condition holds, so the production orchestration -- not a direct call -- must reach
        // the send exactly once. Deleting the send from `complete` fails here.
        let source = Scripted::clean();
        let outcome = run_scripted(&source, 91004, 7004, Instant::now() + TOTAL_BUDGET);
        assert!(outcome.contains("quit requested"), "{outcome}");
        assert_eq!(source.requested.get(), 1);
    }

    #[test]
    fn an_entry_whose_precheck_refuses_still_closes_and_publishes_without_a_transaction() {
        let published = Cell::new(None);
        let closed = Cell::new(false);
        let accepted = close_and_maybe_quit_with(
            |_target| {
                assert!(
                    !precheck_accepts(Judgement::Present, true),
                    "another window must not arm a quit"
                );
                None
            },
            Some(scripted_target(92001, 7201)),
            || {
                closed.set(true);
                true
            },
            |result| published.set(Some(result)),
        );
        assert!(accepted);
        assert!(
            closed.get(),
            "the close must run whatever the pre-check said"
        );
        assert_eq!(published.get(), Some(true));
        assert!(
            !registry().lock().unwrap().active.contains_key(&92001),
            "a refused pre-check must not arm a transaction"
        );
    }

    #[test]
    fn an_entry_whose_close_is_refused_releases_its_registration() {
        let now = Instant::now();
        let accepted = close_and_maybe_quit_with(
            |_target| {
                let deadline = now + TOTAL_BUDGET;
                let ticket = registry()
                    .lock()
                    .unwrap()
                    .begin(92002, deadline)
                    .expect("a fresh instance registers");
                Some(PendingQuit {
                    target: scripted_target(92002, 7202),
                    ticket,
                    deadline,
                })
            },
            Some(scripted_target(92002, 7202)),
            || false,
            |_| {},
        );
        assert!(!accepted);
        assert!(
            !registry().lock().unwrap().active.contains_key(&92002),
            "a refused close must free the instance for the next one"
        );
    }
}
