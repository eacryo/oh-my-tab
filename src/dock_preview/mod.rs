//! Dock hover previews: the public surface and the main-thread orchestration.
//!
//! The feature is one interaction: hovering an application tile in the Dock shows that app's
//! windows as thumbnail cards above the Dock. This module owns the lifetime and the timing; the
//! observer thread owns Accessibility, the collector thread owns window discovery, and `panel`
//! owns every view.
//!
//! Threading: every function here except the observer's sink and the collector's thread runs on the
//! AppKit main thread. The two background paths deliver plain data through a queue and wake the
//! main thread with one `performSelectorOnMainThread:` hop; a run-loop tick then makes every
//! decision, so the panel and its timers have exactly one owner.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use objc2::runtime::AnyObject;
use objc2::{class, msg_send, sel};

use crate::dock_preview::decisions::{CloseDecision, SwitchDecision, TICK_INTERVAL};
use crate::dock_preview::layout::{DockEdge, Rect};
use crate::dock_preview::observer::{DockEvent, Selection};
use crate::dock_preview::peek::{
    ClearReason, Frontmost, Mod, PeekDecision, PeekInput, PeekTarget, PeekWatch,
};
use crate::dock_preview::windows::CollectedWindows;
use crate::ffi::{make_nsstring, MainThreadSlot};
use crate::{log_debug, log_info};

mod decisions;
mod dock_click;
mod layout;
mod observer;
mod panel;
mod peek;
mod reveal;
mod windows;

static ENABLED: AtomicBool = AtomicBool::new(false);
/// Observer generation. An event produced under an older generation belongs to a retired
/// subscription and must not reach the panel.
static SESSION: AtomicU64 = AtomicU64::new(0);
/// Hover request generation: a new value invalidates every collector result and capture request
/// made for the previous panel.
static REQUEST: AtomicU64 = AtomicU64::new(0);
static EVENT_POSTED: AtomicBool = AtomicBool::new(false);

static EVENTS: LazyLock<Mutex<Vec<DockEvent>>> = LazyLock::new(|| Mutex::new(Vec::new()));
type CollectorResult = (u64, Option<CollectedWindows>);
static RESULTS: LazyLock<Mutex<Vec<CollectorResult>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static PENDING_OPEN: LazyLock<Mutex<Option<PendingOpen>>> = LazyLock::new(|| Mutex::new(None));
static CURRENT_APP: LazyLock<Mutex<Option<CurrentApp>>> = LazyLock::new(|| Mutex::new(None));
static CLOSE_AT: LazyLock<Mutex<Option<Instant>>> = LazyLock::new(|| Mutex::new(None));

/// Whether a smoke case has switched the controlled room on (see `visible_frame_of`).
static SMOKE_ROOM_H: AtomicBool = AtomicBool::new(false);
/// The room the next development case opens with, when a smoke case set one (see `set_case_room`).
static CASE_ROOM_H: Mutex<Option<f64>> = Mutex::new(None);
static CASE_ROOM_W: Mutex<Option<f64>> = Mutex::new(None);
/// The Dock tile the observer last reported. A collector result is applied only while this still
/// names the same app: a slow Accessibility pass must not open a preview the pointer has left.
static DOCK_SELECTION: LazyLock<Mutex<Option<(i32, bool)>>> = LazyLock::new(|| Mutex::new(None));
/// AX close attempts launched off the main thread: (request epoch, card index, close accepted).
static CLOSE_RESULTS: LazyLock<Mutex<Vec<CloseOutcome>>> = LazyLock::new(|| Mutex::new(Vec::new()));
/// Activation checks that had to run off the main thread: (request epoch, pid, wid, ok, minimized,
/// other desktop).
type ActivationResult = (u64, u64, i32, u32, bool, bool, bool);
static ACTIVATE_RESULTS: LazyLock<Mutex<Vec<ActivationResult>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

struct CloseOutcome {
    request: u64,
    pid: i32,
    wid: u32,
    accepted: bool,
}
static DOCK_STATE: LazyLock<Mutex<DockState>> = LazyLock::new(|| Mutex::new(DockState::default()));
/// What the collector run for one request was asked for: needed to place its result, because the
/// hover path's pending entry is consumed before the collector returns and the development path
/// never had one.
static COLLECT_CONTEXT: LazyLock<Mutex<Option<CollectContext>>> =
    LazyLock::new(|| Mutex::new(None));
static OBSERVER: MainThreadSlot<Option<observer::ObserverHandle>> =
    unsafe { MainThreadSlot::new(None) };
static TICK_TIMER: MainThreadSlot<Option<*mut AnyObject>> = unsafe { MainThreadSlot::new(None) };
/// Keeps an auto-hidden Dock on screen while the panel is open (see `reveal`).
static DOCK_REVEAL: MainThreadSlot<Option<reveal::Handle>> = unsafe { MainThreadSlot::new(None) };
/// Reports the Dock's own mouse presses, so a press on the previewed icon closes the preview before
/// the switch starts (see `dock_click`).
static DOCK_CLICK: MainThreadSlot<Option<dock_click::Handle>> =
    unsafe { MainThreadSlot::new(None) };
/// A press on the Dock was reported from the watcher thread and the main thread has not handled it
/// yet. The value is the observer session the press belongs to (0 = nothing pending): a press
/// reported while an older session was current must not act on the new one.
static DOCK_CLICK_PENDING: AtomicU64 = AtomicU64::new(0);
/// The Dock tile a press dismissed, with the app it belonged to. While the pointer stays on that
/// tile no preview opens again: Windows does not re-show the flyout for the button it just closed,
/// and the Dock keeps reporting the same selected tile after a click. A notification for another app
/// (or for a non-app tile) means the pointer moved on, and the tick also clears the latch as soon as
/// the pointer leaves the tile.
static DISMISSED_TILE: LazyLock<Mutex<Option<(i32, Rect)>>> = LazyLock::new(|| Mutex::new(None));
/// The last press on the Dock. A press can reach the main thread in the same turn as the hover
/// notification for the tile it pressed, when there is no panel, pending open or collection yet; the
/// session, point and time are what let the open that follows recognize that press.
static DOCK_PRESS: LazyLock<Mutex<Option<DockPress>>> = LazyLock::new(|| Mutex::new(None));
/// Presses that dismissed a preview. `--e2e-state` publishes it as a delta so a scenario can tell a
/// press-dismissal from a hover-leave one instead of reading a single overwritten event name.
static PRESS_DISMISSALS: AtomicU64 = AtomicU64::new(0);

/// Whether hovering a preview card may bring its window forward (`[dock_preview] peek_on_hover`).
/// Mirrored from the config so the tick does not take the config read lock twenty times a second.
static PEEK_ENABLED: AtomicBool = AtomicBool::new(false);
/// A press on the Dock was reported and the tick has not answered it yet. The press is the user's
/// own switch, so no peek may start between it and the activation it causes.
static PEEK_PRESSED: AtomicBool = AtomicBool::new(false);
/// The peek's own generation. A worker result from an older value belongs to a peek that has since
/// been restored, replaced or forgotten.
static PEEK_GENERATION: AtomicU64 = AtomicU64::new(0);

/// The front a peek took the window from.
#[derive(Clone, Debug)]
struct Origin {
    pid: i32,
    /// The front application's focused window, so a restore puts back the window the user was
    /// actually looking at rather than whichever one ends up first.
    focused_window: Option<u32>,
}

#[derive(Clone, Copy)]
struct DockPress {
    session: u64,
    at: Instant,
    point: (f64, f64),
}

#[derive(Clone)]
struct PendingOpen {
    at: Instant,
    pid: i32,
    bundle: String,
    icon: Rect,
    edge: DockEdge,
    request: u64,
}

#[derive(Clone)]
struct CurrentApp {
    pid: i32,
    identity_key: String,
    process_start_time_us: Option<u64>,
    anchor: Rect,
    edge: DockEdge,
    request: u64,
}

#[derive(Clone)]
struct CollectContext {
    request: u64,
    bundle: String,
    anchor: Rect,
    edge: DockEdge,
    visible: Rect,
    /// A development open (`--show-dock-preview`) has no real hover to revalidate against.
    dev: bool,
    pid: i32,
}

#[derive(Clone, Copy, Default)]
struct DockState {
    list: Option<Rect>,
    /// When `list` was pushed by the observer. A boundary observed before a press says nothing
    /// about where the pointer is after it (an auto-hidden Dock reveals in the meantime).
    list_at: Option<Instant>,
    screen: Option<Rect>,
    edge: Option<DockEdge>,
    revealed: bool,
}

/// Whether the feature is on. The settings switch is the only writer.
pub(crate) fn is_enabled() -> bool {
    ENABLED.load(Ordering::Acquire)
}

/// Start the observer, the tick and (through the tick) the panel.
pub(crate) fn start() {
    if ENABLED.swap(true, Ordering::AcqRel) {
        return;
    }
    // Seeded from the stored switch, the same way the observer's on/off state is: a live change goes
    // through `apply_peek_config_change`, and a start after a stop must not lose the stored value.
    PEEK_ENABLED.store(
        crate::config::CONFIG
            .read()
            .map(|config| config.dock_preview.peek_on_hover)
            .unwrap_or(false),
        Ordering::Release,
    );
    let session = SESSION.fetch_add(1, Ordering::AcqRel) + 1;
    crate::assert_main_thread("dock_preview::start");
    *DISMISSED_TILE.lock().unwrap() = None;
    DOCK_CLICK_PENDING.store(0, Ordering::Release);
    *DOCK_PRESS.lock().unwrap() = None;
    let sink: observer::EventSink = Arc::new(|event| {
        EVENTS.lock().unwrap().push(event);
        post_event_to_main();
    });
    let mut slot = OBSERVER.lock().unwrap();
    if slot.is_none() {
        *slot = observer::spawn(sink);
    }
    let Some(handle) = slot.as_ref() else {
        drop(slot);
        ENABLED.store(false, Ordering::Release);
        log_info!("Dock preview: observer thread could not start");
        return;
    };
    // A parked thread from the previous on period is reused, so a fast off/on cannot overlap two
    // subscriptions.
    handle.rebuild(session);
    drop(slot);
    start_tick_timer();
    start_dock_click_watcher();
    log_info!("Dock preview: observer started");
}

/// Stop the observer, hide the panel and cancel every pending decision.
pub(crate) fn stop() {
    if !ENABLED.swap(false, Ordering::AcqRel) {
        return;
    }
    crate::assert_main_thread("dock_preview::stop");
    SESSION.fetch_add(1, Ordering::AcqRel);
    REQUEST.fetch_add(1, Ordering::AcqRel);
    PENDING_OPEN.lock().unwrap().take();
    *CLOSE_AT.lock().unwrap() = None;
    *CURRENT_APP.lock().unwrap() = None;
    stop_dock_reveal_keeper();
    stop_dock_click_watcher();
    DOCK_CLICK_PENDING.store(0, Ordering::Release);
    *DISMISSED_TILE.lock().unwrap() = None;
    *DOCK_PRESS.lock().unwrap() = None;
    // The peek is torn down through the same path as a hidden panel. `stop()` used to call
    // `panel::hide()` directly, which left a peeked window in front after the feature was turned
    // off, and left a raise prepared by a worker able to commit after the feature was gone. The
    // session reset afterwards drops the model (a hold included) instead of carrying it into the
    // next start.
    // The teardown makes EXACTLY ONE synchronous Accessibility attempt, however many restores are
    // waiting (one peek's, plus whatever a click parked). Each attempt can wait out the AX timeout, and
    // this runs inside the settings callback. So the requests are collected first, handed to the
    // queue once, and drained once; everything that survives goes to the timer below.
    collect_peek_then_park_restores();
    peek_session_reset();
    // The one synchronous pass. It re-queues whatever could not be read yet, and the schedule below
    // takes over from there.
    drain_peek_restores();
    PEEK_PRESSED.store(false, Ordering::Release);
    panel::hide();
    panel::flush_records();
    crate::thumbnail::begin_dock_capture_epoch();
    stop_tick_timer();
    if let Some(handle) = OBSERVER.lock().unwrap().as_ref() {
        handle.pause();
    }
    EVENTS.lock().unwrap().clear();
    RESULTS.lock().unwrap().clear();
    // A worker that was still validating when the feature stopped would otherwise match a
    // `raising` value and activate a window after the feature went away. The generation is what a
    // queued result is checked against, so bumping it retires every one of them.
    PEEK_GENERATION.fetch_add(1, Ordering::AcqRel);
    PEEK_RAISES_READY.lock().unwrap().clear();
    // The single schedule for this teardown: after `stop()` there is no tick left to retry a restore,
    // and the activation notification that would otherwise wake one returns early once the feature is
    // off. Called once -- two calls could see the same non-empty queue and start two workers that
    // would each hammer the same request.
    schedule_peek_restore_settle();
    log_info!("Dock preview: stopped");
}

/// The settings switch changed (or a reload restored it).
pub(crate) fn apply_config_change(enabled: bool) {
    if enabled == is_enabled() {
        return;
    }
    if enabled {
        start();
    } else {
        stop();
    }
}

/// The app quit or the screens changed: anything on screen no longer describes the moment.
/// Every application activation the system reports reaches the peek too.
///
/// An activation that belongs to the peek is the front arriving at the window the peek just asked
/// for; any other activation is someone else's, and the peek must stand aside rather than fight it.
/// This covers the switcher's own Command+Tab and a Dock click, which both go through the same
/// notification and would otherwise leave a peek holding a front it no longer owns.
pub(crate) fn handle_app_activation(pid: i32) {
    if !is_enabled() {
        return;
    }
    // A restore that could not read the front is retried here first: this notification is exactly
    // the moment the front becomes readable, and it can arrive after the peek's own tick is gone.
    if is_main_thread() {
        drain_peek_restores();
    }
    let Ok(slot) = PEEK.lock() else {
        return;
    };
    // A raise is still being prepared: the peek is not up yet, but the pointer may already have
    // been abandoned, so the prepared switch is retired and the card held until the pointer leaves.
    if slot.raising.is_some() && slot.model.peeked().is_none() {
        // The card cannot come from the model here: issuing the raise disarmed it. The card under
        // the pointer is the one the raise was for as long as the pointer has not moved.
        drop(slot);
        hold_peek_card(ClearReason::UserSwitched);
        PEEK_GENERATION.fetch_add(1, Ordering::AcqRel);
        return;
    }
    // Nothing is up. An activation that arrives while a card is only ARMED is the user choosing
    // somewhere else before the dwell ran out: the timer must be cancelled here, or it would fire a
    // few milliseconds later and take the front back from the application they just picked. The
    // peeked case below is the same idea for a peek that is already up.
    let Some(target) = slot.model.peeked() else {
        if slot.model.armed().is_some() || slot.model.held().is_some() {
            drop(slot);
            hold_peek_card(ClearReason::UserSwitched);
        }
        return;
    };
    let expected = target.pid == pid || slot.origin.as_ref().map(|origin| origin.pid) == Some(pid);
    drop(slot);
    if expected {
        return;
    }
    hold_peek_card(ClearReason::UserSwitched);
}

pub(crate) fn handle_app_terminated(pid: i32) {
    if !is_enabled() {
        return;
    }
    // Either end of a peek can quit: the peeked application loses the show, the origin loses the
    // application the front is owed back to.
    handle_peek_app_terminated(pid);
    let current = CURRENT_APP.lock().unwrap().as_ref().map(|app| app.pid);
    if current == Some(pid) {
        hide_panel();
    }
    let cancel = {
        let pending = PENDING_OPEN.lock().unwrap();
        pending.as_ref().map(|open| open.pid) == Some(pid)
    };
    if cancel {
        PENDING_OPEN.lock().unwrap().take();
    }
}

/// Drop an open panel because its layout settings changed (the row/list threshold). The observer
/// keeps running: the next hover opens the panel again with the new layout.
pub(crate) fn hide_for_layout_change() {
    if panel::is_visible() {
        hide_panel();
        panel::flush_records();
    }
}

pub(crate) fn handle_screen_params_changed() {
    if is_enabled() {
        hide_panel();
    }
}

/// The configured threshold: the most windows a single row shows before the panel becomes a list.
pub(crate) fn cards_per_row() -> usize {
    crate::config::CONFIG
        .read()
        .map(|config| config.dock_preview.cards_per_row as usize)
        .unwrap_or(crate::config::DOCK_CARDS_PER_ROW_DEFAULT as usize)
}

/// The app the open panel belongs to, with the identity and request its targets are checked against.
fn current_identity() -> Option<(i32, String, Option<u64>)> {
    let app = CURRENT_APP.lock().unwrap().clone()?;
    Some((app.pid, app.identity_key, app.process_start_time_us))
}

/// A row's body was clicked: activate its window, then leave. Identity and window presence are
/// verified on a worker: both need Accessibility for some windows, and the main thread must not
/// block on a target that does not answer.
///
/// The target is the one resolved under the panel lock (`panel::hit_target`), never an index: a
/// scroll or a removal between the click and this call would otherwise pick another row.
pub(crate) fn activate_target(target: panel::HitTarget) {
    let Some((app_pid, identity_key, start_time_us)) = current_identity() else {
        hide_panel();
        return;
    };
    if target.pid != app_pid {
        hide_panel();
        return;
    }
    let Some(window) = panel::item_window(target.pid, target.wid) else {
        return;
    };
    // Every check the main thread can make has passed, so the click is one this panel can carry
    // out. Only now does the peek get paused: an earlier pause would be suspended by a return that
    // never started a worker.
    //
    // The click is an explicit choice, so it PAUSES the peek instead of letting the panel's own
    // teardown restore first: the front must not stop at the origin on its way to the window the
    // user picked. Paused rather than forgotten, because the validation below can still fail -- and
    // a failed click must give the peek back, or a window it raised would stay in front with no
    // restore left to answer for it. A newer click supersedes an older paused one.
    let intent = next_click_intent();
    park_paused_for(intent, pause_peek_for_click());
    let (pid, wid) = (target.pid, target.wid);
    let (minimized, other_desktop) = (window.minimized, window.other_desktop);
    let identity = (app_pid, identity_key, start_time_us);
    let request = target.request;
    let spawn = std::thread::Builder::new()
        .name("com.eacryo.oh-my-tab.dock-preview-activate".to_string())
        .spawn(move || {
            let identity_ok = windows::process_identity_matches(identity.0, &identity.1, identity.2);
            let presence = windows::window_presence(pid, wid);
            let ok = identity_ok && presence == decisions::WindowPresence::Present;
            log_debug!(
                "[dock-preview] activation check pid={pid} wid={wid} identity={identity_ok} presence={presence:?}"
            );
            ACTIVATE_RESULTS.lock().unwrap().push((
                request,
                intent,
                pid,
                wid,
                ok,
                minimized,
                other_desktop,
            ));
            post_event_to_main();
        });
    if spawn.is_err() {
        // No worker means no validation, and therefore no click: put the paused restore back
        // instead of leaving the window the peek raised in front for good.
        log_debug!(
            "[dock-preview] activation worker could not start; the peek restore is restored"
        );
        requeue_paused_restores(unpark_restores(intent));
        hide_panel();
    }
}

fn drain_activate_results() {
    let results = {
        let mut queue = ACTIVATE_RESULTS.lock().unwrap();
        std::mem::take(&mut *queue)
    };
    for (request, intent, pid, wid, ok, minimized, other_desktop) in results {
        if !decisions::event_is_current(request, REQUEST.load(Ordering::Acquire)) {
            // The hover this click belonged to is gone: the parked restore comes back (unless a
            // later click already decided the front), so a window the peek raised cannot be left in
            // front with nobody to answer for it.
            requeue_paused_restores(unpark_restores(intent));
            continue;
        }
        if intent != CLICK_INTENT.load(Ordering::Acquire) {
            // Two clicks on the same panel share the hover request, so `request` alone cannot tell
            // them apart. Only the NEWEST click may decide: the user's last click is their choice,
            // and a slower earlier validation coming back first must not take it away. The parked
            // restore stays parked (the newer click owns it) until that click reports.
            log_debug!(
                "[dock-preview] click intent {intent} superseded by {}; not activating",
                CLICK_INTENT.load(Ordering::Acquire)
            );
            crate::e2e_state::record("dock_preview_click_superseded");
            continue;
        }
        if !ok {
            log_debug!("[dock-preview] activation target not confirmed; not raising");
            // The user's click did not land, so nothing supersedes the peek's own restore: put the
            // parked request back and let it apply under the usual ownership rules.
            requeue_paused_restores(unpark_restores(intent));
            continue;
        }
        // The validation confirmed this click, and it is the newest one. Retire every restore parked
        // by an older click whose validation may still be in flight: the committed intent is what
        // stops that late result from undoing this switch.
        CLICK_COMMITTED.store(intent, Ordering::Release);
        supersede_pending_restores("an explicit card click");
        hide_panel();
        crate::overlay::activate_and_raise(pid, wid, minimized, other_desktop);
    }
}

/// A row's close button was clicked: ask the system to close that window and keep the row until
/// the window is confirmed gone (a save dialog, or a failed query, keeps it).
pub(crate) fn close_target(target: panel::HitTarget) {
    let Some((app_pid, identity_key, start_time_us)) = current_identity() else {
        return;
    };
    if target.pid != app_pid {
        return;
    }
    let (pid, wid) = (target.pid, target.wid);
    // The row stops taking clicks now; a second press on the same generation is refused here.
    if !panel::begin_close_pending(pid, wid) {
        return;
    }
    panel::flush_records();
    let identity = (app_pid, identity_key.clone(), start_time_us);
    let request = target.request;
    // Captured here, where the close starts: the panel may hide (and advance its request
    // generation) while the close is still being confirmed, and that must not cancel the quit.
    let quit_target = crate::window_collector::last_window::target_for_dock_close(
        pid,
        wid,
        identity_key,
        start_time_us,
    );
    let spawn = std::thread::Builder::new()
        .name("com.eacryo.oh-my-tab.dock-preview-close".to_string())
        .spawn(move || {
            let verified = windows::process_identity_matches(identity.0, &identity.1, identity.2)
                && windows::window_presence(pid, wid) == decisions::WindowPresence::Present;
            // The whole close runs through the shared transaction: the enabled switch's pre-check
            // happens before the close, the outcome is published to the panel right after it, and
            // the evidence work that may request a quit stays independent of the panel's own
            // request generation (the panel can hide while this is still being confirmed).
            crate::window_collector::last_window::close_and_maybe_quit(
                quit_target,
                || verified && crate::window_collector::close_ax_window(pid, wid),
                |accepted| {
                    CLOSE_RESULTS.lock().unwrap().push(CloseOutcome {
                        request,
                        pid,
                        wid,
                        accepted,
                    });
                    post_event_to_main();
                },
            );
        });
    if spawn.is_err() {
        // No worker means no AX call: give the row back instead of leaving it stuck pending.
        panel::cancel_close_matching(pid, wid);
        panel::flush_records();
    }
}

fn drain_close_results() {
    let results: Vec<CloseOutcome> = {
        let mut queue = CLOSE_RESULTS.lock().unwrap();
        std::mem::take(&mut *queue)
    };
    for outcome in results {
        if !decisions::event_is_current(outcome.request, REQUEST.load(Ordering::Acquire)) {
            continue;
        }
        if outcome.accepted {
            panel::confirm_close_matching(outcome.pid, outcome.wid);
        } else {
            log_debug!("[dock-preview] AX close refused; returning the card to normal");
            panel::cancel_close_matching(outcome.pid, outcome.wid);
        }
        panel::flush_records();
    }
}

/// Start (or re-point) the Dock reveal keeper for an open panel.
fn start_dock_reveal_keeper(anchor: Rect, edge: DockEdge) {
    if !reveal::dock_auto_hide_enabled() {
        return;
    }
    let Some(screen) = DOCK_STATE.lock().unwrap().screen else {
        return;
    };
    let Some(pid) = running_pid_for_bundle("com.apple.dock") else {
        return;
    };
    let point = reveal::reveal_point(edge, anchor, screen);
    let mut slot = DOCK_REVEAL.lock().unwrap();
    if slot.is_none() {
        *slot = Some(reveal::spawn());
    }
    if let Some(handle) = slot.as_ref() {
        handle.start(pid, point);
    }
}

/// Follow the Dock icon when it moves under magnification while the panel is open.
fn update_dock_reveal_keeper(anchor: Rect, edge: DockEdge) {
    let Some(screen) = DOCK_STATE.lock().unwrap().screen else {
        return;
    };
    let point = reveal::reveal_point(edge, anchor, screen);
    if let Some(handle) = DOCK_REVEAL.lock().unwrap().as_ref() {
        handle.update(point);
    }
}

/// Release the Dock so it can hide again once the panel is gone. A pointer that is back on the
/// Dock keeps it visible by itself, so no farewell event is posted in that case.
fn stop_dock_reveal_keeper() {
    let pointer_on_dock = pointer_in_cg()
        .zip(DOCK_STATE.lock().unwrap().list)
        .is_some_and(|(pointer, list)| list.contains(pointer));
    if let Some(handle) = DOCK_REVEAL.lock().unwrap().as_ref() {
        handle.stop(!pointer_on_dock);
    }
}

fn other_panel_visible() -> bool {
    crate::overlay::panel_is_visible_for_e2e() || crate::clipboard::picker_is_visible()
}

// ---------------------------------------------------------------------------------------------
// Press on the Dock
// ---------------------------------------------------------------------------------------------

/// Start (or re-point) the Dock press watcher. Called when the feature starts and from the hover
/// path: a press often lands inside the open delay, before any panel exists, and the watcher has to
/// be listening by then. Re-pointing covers a Dock that restarted since the last call.
fn start_dock_click_watcher() {
    let session = SESSION.load(Ordering::Acquire);
    if session == 0 {
        return;
    }
    let Some(pid) = running_pid_for_bundle("com.apple.dock") else {
        return;
    };
    let mut slot = DOCK_CLICK.lock().unwrap();
    if slot.is_none() {
        *slot = Some(dock_click::spawn());
    }
    if let Some(handle) = slot.as_ref() {
        handle.start(pid, session);
    }
}

fn stop_dock_click_watcher() {
    if let Some(handle) = DOCK_CLICK.lock().unwrap().as_ref() {
        handle.stop();
    }
}

/// A press landed on the Dock at `point`, reported by the tap installed for `session`. Called on the
/// watcher thread: record the fact and wake the main thread, which owns the panel.
pub(crate) fn note_dock_click(point: (f64, f64), session: u64) {
    if session == 0 {
        return;
    }
    *DOCK_PRESS.lock().unwrap() = Some(DockPress {
        session,
        at: Instant::now(),
        point,
    });
    // A later press replaces the pending one: only the newest press describes the pointer, and the
    // main thread judges its session when it runs.
    DOCK_CLICK_PENDING.store(session, Ordering::Release);
    post_event_to_main();
}

/// A press on the Dock closes the preview on the press itself, the way the Windows taskbar closes
/// its thumbnail flyout; the switch the press starts is not waited for.
fn dismiss_for_dock_click(session: u64) {
    if session != SESSION.load(Ordering::Acquire) {
        log_debug!("[dock-preview] ignoring a press from a retired session");
        return;
    }
    // The press is the user's own switch, and the Dock is about to activate the pressed tile. A peek
    // that is up gives the front back at once, and no new peek may start until the tick has answered
    // this press -- otherwise the two activations would race and the restore could come second.
    PEEK_PRESSED.store(true, Ordering::Release);
    restore_peek();
    let pending_target = PENDING_OPEN
        .lock()
        .unwrap()
        .as_ref()
        .map(|open| (open.pid, open.icon));
    // A hover between its open delay and its panel has neither a panel nor a pending entry: its
    // collector is already running. That press must dismiss the preview too, or the panel would
    // appear just after the click that was meant to close it. A context left behind by a request
    // that is already invalid describes nothing.
    let collecting_target = {
        let context = COLLECT_CONTEXT.lock().unwrap();
        context
            .as_ref()
            .filter(|context| {
                decisions::event_is_current(context.request, REQUEST.load(Ordering::Acquire))
            })
            .map(|context| (context.pid, context.anchor))
    };
    let visible = panel::is_visible();
    if !visible && pending_target.is_none() && collecting_target.is_none() {
        return;
    }
    // The panel's geometry survives a hide, so trust the hover's own record first: after a hide it
    // is the only thing that still describes this hover.
    let target = pending_target.or(collecting_target).or_else(|| {
        CURRENT_APP
            .lock()
            .unwrap()
            .as_ref()
            .map(|app| (app.pid, app.anchor))
    });
    // The press stays for the pressed tile's own open. Pressing tile B while tile A's preview is
    // shown dismisses A here, and B's hover notification may arrive after this: the press point is
    // what keeps the preview from opening under the finger that is still pressing B. It is consumed
    // when it matches such an open, when a post-press notification shows the pointer on another
    // tile, and when the tick sees the pointer leave the Dock.
    if visible {
        // Before the hide, so the frame `dock_preview_hide` writes carries the new count: with no
        // later event the file would otherwise keep the old value forever, and a scenario reads it.
        PRESS_DISMISSALS.fetch_add(1, Ordering::AcqRel);
    }
    hide_panel();
    if let Some(target) = target {
        *DISMISSED_TILE.lock().unwrap() = Some(target);
    }
    log_debug!("[dock-preview] Dock press dismissed the preview");
}

/// Apply one tick's pointer sample to the press/latch state. The tick is the only observer whose
/// sample describes where the pointer is *now*; a queued Dock notification describes an older
/// position, which is why nothing here is driven by notifications.
fn release_dismiss_state(pointer: (f64, f64)) {
    let dock = *DOCK_STATE.lock().unwrap();
    let mut latch = DISMISSED_TILE.lock().unwrap();
    let mut press = DOCK_PRESS.lock().unwrap();
    // The cached boundary is evidence only when it was observed after the press. With an auto-hidden
    // Dock, the reveal that follows a press has not updated the rectangle yet, and an absent or
    // older rectangle would otherwise read as "the pointer left the Dock" and drop a live press.
    let on_dock = match (dock.list, dock.list_at, *press) {
        (Some(list), Some(at), Some(press)) if at > press.at => Some(list.contains(pointer)),
        (Some(list), _, None) => Some(list.contains(pointer)),
        _ => None,
    };
    let press_tuple = press.map(|press| (press.session, press.at, press.point));
    let (latched_now, press_now) =
        decisions::released_by_pointer(*latch, press_tuple, pointer, on_dock);
    *latch = latched_now;
    *press = press_now.map(|(session, at, point)| DockPress { session, at, point });
}

// ---------------------------------------------------------------------------------------------
// Hover peek: bringing a card's window forward while the pointer rests on it
// ---------------------------------------------------------------------------------------------
//
// What this is, and why it is not a z-order trick: macOS has no way to lift another application's
// window without activating that application (measured 2026-11-24: the SLPS front-switch and the
// synthetic key click reorder nothing while the target is not frontmost). So a peek is one real
// activation, and leaving the card is a second one back. Every rule that keeps that safe lives in
// `peek`; this section owns only the state, the worker and the system calls.

/// The peek's state. Main thread only.
struct PeekState {
    /// The pure decision state: which card is armed, which card is up.
    model: Mod,
    armed_at: Option<Instant>,
    /// The front the peek took, while it is up.
    origin: Option<Origin>,
    /// A worker is preparing the raise for this generation, the card it is for, and whether it is a
    /// RETARGET of a peek that is already up (which keeps the origin instead of taking a new one).
    /// The model's `armed` is cleared when the worker starts, so this is the only thing that can tell
    /// a tick whether the pointer still rests on the card the raise belongs to.
    raising: Option<(u64, PeekTarget, bool)>,
}

impl PeekState {
    const EMPTY: Self = Self {
        model: Mod::new(),
        armed_at: None,
        origin: None,
        raising: None,
    };
}

/// A raise a worker prepared: the peek state it belongs to and whether the target was confirmed.
struct PeekRaise {
    generation: u64,
    target: PeekTarget,
    /// This raise continues an existing peek instead of starting one: the origin stays.
    retarget: bool,
    /// The front the peek will take the window from. Sampled on the main thread BEFORE the worker
    /// runs: reading it afterwards could read the target itself.
    origin: Origin,
    /// The worker's identity and presence check: an unconfirmed target is not activated.
    verified: bool,
}

/// One restore the main thread performs: give the front back to `origin` (and to its window).
#[derive(Clone, Copy, Debug)]
struct PeekRestore {
    /// Whether this callback has already spent its one Accessibility attempt on this request: the
    /// teardown collects everything and drains once, and a request that just came back "not yet" must
    /// not be queried again inside the same callback.
    tried_in_this_callback: bool,
    /// The window the restore is undoing. Its application must still hold the front when the
    /// restore runs, or the restore is dropped rather than dragging the front back.
    peeked: PeekTarget,
    origin: i32,
    focused_window: Option<u32>,
    /// When the restore was requested. A restore that cannot confirm the front keeps its request
    /// for a bounded window and then gives up, so an unreadable front can neither strand the peek
    /// nor let the restore run long after the user moved on.
    requested_at: Instant,
}

static PEEK: MainThreadSlot<PeekState> = unsafe { MainThreadSlot::new(PeekState::EMPTY) };
static PEEK_RAISES_READY: LazyLock<Mutex<Vec<PeekRaise>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));
static PEEK_RESTORES_READY: LazyLock<Mutex<Vec<PeekRestore>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));
/// How many attempts the post-`stop()` restore schedule makes, and the longest it sleeps between
/// two of them. The schedule is aimed at the oldest request's deadline; these only bound how finely.
const RESTORE_SETTLE_ATTEMPTS: usize = 4;
/// One live settle schedule at a time (see `schedule_peek_restore_settle`).
static RESTORE_SETTLE_ACTIVE: AtomicBool = AtomicBool::new(false);
/// Room left before the deadline for the main-thread hop and one Accessibility read.
const RESTORE_SETTLE_MARGIN: Duration = Duration::from_millis(120);

/// Peeks committed since launch. A scenario asserts the delta, not the value.
static PEEK_RAISES: AtomicU64 = AtomicU64::new(0);
/// Restores performed since launch. The counter next to the raises: a raise without a restore is
/// exactly the failure mode that leaves a window in front for good.
static PEEK_RESTORES: AtomicU64 = AtomicU64::new(0);

/// Whether the peek is switched on. Read by the tick, the e2e report and the settings page.
pub(crate) fn peek_is_enabled() -> bool {
    PEEK_ENABLED.load(Ordering::Acquire)
}

/// The settings switch (or a reload) changed the peek. Turning it off while a peek is up gives the
/// front back first: the switch must never strand an activation whose card is gone.
pub(crate) fn apply_peek_config_change(enabled: bool) {
    PEEK_ENABLED.store(enabled, Ordering::Release);
    if !enabled {
        restore_peek();
    }
}

/// The front application as the peek's rules read it. Main thread only: `NSWorkspace`
/// `frontmostApplication` is an AppKit call.
fn peek_frontmost() -> Frontmost {
    let pid = crate::ffi::frontmost_app_info().1;
    if pid <= 0 {
        return Frontmost::Unknown;
    }
    if pid == std::process::id() as i32 {
        return Frontmost::Own;
    }
    Frontmost::Other {
        pid,
        focused_window: None,
    }
}

/// Why a card cannot be peeked at, if it cannot.
fn peek_refusal_for(window: Option<&windows::PreviewWindow>) -> Option<peek::PeekRefusal> {
    let window = window?;
    if window.wid == 0 {
        return Some(peek::PeekRefusal::NoWindow);
    }
    if window.app_hidden {
        return Some(peek::PeekRefusal::AppHidden);
    }
    if window.minimized {
        return Some(peek::PeekRefusal::Minimized);
    }
    if window.other_desktop {
        return Some(peek::PeekRefusal::OtherDesktop);
    }
    None
}

/// Run the peek's decision for this tick and act on it.
///
/// Called from `tick` after the panel sampled the pointer, so the card this reads is the card the
/// hover plate draws.
fn step_peek(now: Instant, zone: layout::PointerZone) {
    let Ok(mut slot) = PEEK.lock() else {
        return;
    };
    let state = &mut *slot;
    // A press on the Dock is the user's own switch. A peek that is up gives the front back at once;
    // no new peek starts before the Dock's own activation lands.
    if PEEK_PRESSED.swap(false, Ordering::AcqRel) {
        if state.model.peeked().is_some() {
            drop(slot);
            restore_peek();
        } else {
            // Nothing is up: the pointer is on the card the user just pressed, and the Dock's own
            // activation is on its way. That card stays quiet until the pointer leaves it.
            state.model.clear();
            state.origin = None;
            if let Some((pid, wid)) = panel::peek_body_index(zone).and_then(panel::window_at_index)
            {
                state.model.hold(PeekTarget { pid, wid });
            }
            state.armed_at = None;
        }
        return;
    }
    // A peek that is up is watched against the system first: the user switching away themselves
    // must win over anything the pointer does next.
    if let Some(target) = state.model.peeked() {
        let origin = state.origin.as_ref().map(|origin| origin.pid);
        let origin_running = origin.is_some_and(windows::process_is_running);
        // The window, not only its process: an application that closed or minimized the window it
        // was previewing no longer owns the front for us, even though its process is alive.
        // The window has to still be something the peek may show: a minimized window is still
        // listed by the WindowServer, so presence alone would keep a peek up for a card that no
        // longer describes anything on screen.
        // Only a confirmed "gone" ends a live peek: a query that cannot answer is not evidence that
        // the window disappeared, and discarding the peek on one failed read would throw away the
        // origin the later restore needs.
        let target_alive = windows::process_is_running(target.pid)
            && windows::previewability_now(target.pid, target.wid) != windows::Previewability::Gone;
        match peek::watch_peek(
            target,
            origin,
            origin_running,
            target_alive,
            peek_frontmost(),
        ) {
            // A live peek continues into the SAME pointer decision the rest of this function
            // makes: the pointer leaving its card is what ends a peek, and an early return here
            // was what kept a peek up forever after the pointer had moved to the title box, the
            // panel padding or another card.
            PeekWatch::Keep => {}
            PeekWatch::Forget(reason) => {
                drop(slot);
                hold_peek_card(reason);
                return;
            }
        }
    }
    // A raise is being prepared. The user may have left its card while the worker ran, and that
    // leave retires the request HERE: the model has no `armed` entry to compare while the worker
    // owns it, and a stale request would otherwise commit after the pointer had left and come back.
    if let Some((generation, target, retarget)) = state.raising {
        let still_there = panel::peek_body_index(zone)
            .and_then(panel::window_at_index)
            .is_some_and(|(pid, wid)| pid == target.pid && wid == target.wid);
        if !still_there {
            PEEK_GENERATION.fetch_add(1, Ordering::AcqRel);
            state.raising = None;
            state.armed_at = None;
            if retarget {
                // A peek is still up on its previous card: only the switch to this one is cancelled.
                // The origin and the peek survive, or leaving would have nothing to give back.
                log_debug!(
                    "[dock-preview] peek retarget cancelled: the pointer left pid={} wid={} gen={generation}",
                    target.pid,
                    target.wid
                );
            } else {
                state.origin = None;
                state.model.hold(target);
                log_debug!(
                    "[dock-preview] peek raise retired: the pointer left pid={} wid={} gen={generation}",
                    target.pid,
                    target.wid
                );
            }
        }
        // While a raise is being prepared nothing new is armed and nothing is replaced.
        return;
    }
    let hovered_window = panel::peek_body_index(zone)
        .and_then(panel::window_at_index)
        .and_then(|(pid, wid)| panel::item_window(pid, wid));
    let hovered = hovered_window.as_ref().map(|window| PeekTarget {
        pid: window.pid,
        wid: window.wid,
    });
    let input = PeekInput {
        enabled: peek_is_enabled() && is_enabled(),
        zone,
        hovered,
        hovered_previewable: peek_refusal_for(hovered_window.as_ref()),
        frontmost: peek_frontmost(),
        press_pending: false,
        armed_at: state.armed_at,
        now,
        armed_on_this_card: state.model.armed().is_some() && state.model.armed() == hovered,
    };
    match peek::peek_decision(&state.model, &input) {
        PeekDecision::Idle => {}
        PeekDecision::Arm { target, at } => {
            state.model.arm(target);
            state.armed_at = Some(at);
        }
        PeekDecision::Peek { target } => {
            let generation = PEEK_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
            state.armed_at = None;
            state.model.disarm();
            // Sampled before the worker can start, and never the card's own application (a card of
            // the front application is refused by the decision itself).
            let Some(origin) = peek_origin_now() else {
                return;
            };
            let identity = CURRENT_APP
                .lock()
                .unwrap()
                .as_ref()
                .map(|app| (app.identity_key.clone(), app.process_start_time_us));
            let Some((identity_key, process_start_time_us)) = identity else {
                return;
            };
            state.raising = Some((generation, target, false));
            state.origin = Some(origin.clone());
            if !start_peek_raise(
                generation,
                target,
                origin.clone(),
                identity_key,
                process_start_time_us,
                false,
            ) {
                state.raising = None;
                state.origin = None;
            }
        }
        PeekDecision::Retarget { target } => {
            // The peek stays up; only the raised window changes. The origin is NOT re-sampled: it is
            // still the application the user came from, and leaving the panel must return there.
            if state.origin.is_none() {
                return;
            }
            let generation = PEEK_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
            state.armed_at = None;
            state.model.disarm();
            let identity = CURRENT_APP
                .lock()
                .unwrap()
                .as_ref()
                .map(|app| (app.identity_key.clone(), app.process_start_time_us));
            let Some((identity_key, process_start_time_us)) = identity else {
                return;
            };
            let origin = state.origin.clone().expect("checked above");
            state.raising = Some((generation, target, true));
            if !start_peek_raise(
                generation,
                target,
                origin,
                identity_key,
                process_start_time_us,
                true,
            ) {
                state.raising = None;
            }
        }
        PeekDecision::Restore => {
            drop(slot);
            restore_peek();
        }
        PeekDecision::Forget => {
            state.model.disarm();
            state.armed_at = None;
        }
    }
}

/// The front an activation would take the window from, or `None` when there is nothing sensible to
/// give the front back to (no front, ourselves, or the card's own application).
fn peek_origin_now() -> Option<Origin> {
    let front = crate::ffi::frontmost_app_info().1;
    if front <= 0 || front == std::process::id() as i32 {
        return None;
    }
    Some(Origin {
        pid: front,
        // One Accessibility round trip with a short timeout, on the main thread right before the
        // activation: a restore should put back the window the user was looking at, not whichever
        // window of that application ends up first.
        focused_window: unsafe { crate::window_collector::focused_window_cgwid(front) },
    })
}

/// Prepare the raise on a worker: identity and presence both need Accessibility for some windows,
/// and the main thread must not wait on an application that does not answer.
fn start_peek_raise(
    generation: u64,
    target: PeekTarget,
    origin: Origin,
    identity_key: String,
    process_start_time_us: Option<u64>,
    retarget: bool,
) -> bool {
    let spawn = std::thread::Builder::new()
        .name("com.eacryo.oh-my-tab.dock-preview-peek".to_string())
        .spawn(move || {
            // The identity check reads `NSRunningApplication` and autoreleased strings, and this is a
            // fresh thread with no pool of its own: every hover would otherwise leak that thread's
            // Objective-C temporaries.
            let verified = objc2::rc::autoreleasepool(|_pool| {
                windows::process_identity_matches(target.pid, &identity_key, process_start_time_us)
                    && windows::window_presence(target.pid, target.wid)
                        == decisions::WindowPresence::Present
            });
            if !verified {
                log_debug!(
                    "[dock-preview] peek target not confirmed pid={} wid={}",
                    target.pid,
                    target.wid
                );
            }
            PEEK_RAISES_READY.lock().unwrap().push(PeekRaise {
                generation,
                target,
                origin,
                verified,
                retarget,
            });
            post_event_to_main();
        });
    spawn.is_ok()
}

/// Drain the workers' validated raises and perform the switch on the main thread.
fn drain_peek_raises() {
    let ready: Vec<PeekRaise> = {
        let mut queue = PEEK_RAISES_READY.lock().unwrap();
        std::mem::take(&mut *queue)
    };
    for raise in ready {
        let current = PEEK_GENERATION.load(Ordering::Acquire);
        let Ok(mut slot) = PEEK.lock() else {
            return;
        };
        let state = &mut *slot;
        if raise.generation != current
            || state.raising.map(|(generation, _, _)| generation) != Some(raise.generation)
        {
            // The peek was restored, replaced or forgotten while the worker ran: nothing to do,
            // and the state that replaced it must not be touched.
            if state.raising.map(|(generation, _, _)| generation) == Some(raise.generation) {
                state.raising = None;
                // A cancelled RETARGET leaves the peek up: only a first raise drops the origin.
                if !raise.retarget {
                    state.origin = None;
                }
            }
            continue;
        }
        let retarget = state
            .raising
            .map(|(_, _, retarget)| retarget)
            .unwrap_or(false);
        state.raising = None;
        if !raise.verified {
            if !retarget {
                state.origin = None;
            }
            state.armed_at = None;
            continue;
        }
        let target = raise.target;
        // The validation ran on a worker and the panel kept ticking while it ran. Between the arm
        // and this commit the pointer may have left the card (the raise then belongs to nobody), the
        // card may have become unpokeable (a minimized or other-desktop window), or the user may have
        // switched applications themselves (the origin sampled at arm time is stale, and committing
        // would take the front back from where they went).
        // The pointer is hit against the panel's geometry and the real cursor rather than the zone
        // the last tick stored: a worker can return between two ticks, and the stored zone is then
        // up to one frame stale. The window's current state is read from the system too -- the
        // card's copy of "minimized / hidden / other desktop" is as old as the panel.
        let hovered_now =
            panel::window_under_pointer().and_then(|(pid, wid)| panel::item_window(pid, wid));
        let still_hovered = hovered_now
            .as_ref()
            .is_some_and(|window| window.pid == target.pid && window.wid == target.wid);
        // The switch itself waits for a confirmed "present": an unreadable window is not raised.
        let still_pokeable = peek_refusal_for(hovered_now.as_ref()).is_none()
            && windows::previewability_now(target.pid, target.wid)
                == windows::Previewability::Present;
        let front_now = peek_frontmost();
        // A first raise expects the front to still be the origin. A RETARGET expects it to still be
        // the application whose window is currently peeked: that is the switch this one continues.
        let front_ok = match front_now {
            Frontmost::Other { pid, .. } => {
                if retarget {
                    state.model.peeked().map(|peeked| peeked.pid) == Some(pid)
                } else {
                    pid == raise.origin.pid
                }
            }
            _ => false,
        };
        if !still_hovered || !still_pokeable || !front_ok {
            log_debug!(
                "[dock-preview] peek raise dropped: hovered={still_hovered} pokeable={still_pokeable} front_ok={front_ok} retarget={retarget}"
            );
            state.armed_at = None;
            // A retarget that cannot land leaves the previous peek up, so the origin stays owned.
            if !retarget {
                state.origin = None;
                state.model.hold(target);
            }
            continue;
        }
        state.model.commit(target);
        drop(slot);
        if retarget {
            log_debug!(
                "[dock-preview] peek retarget pid={} wid={} (the previous window gives way in place)",
                target.pid,
                target.wid
            );
            crate::e2e_state::record("dock_preview_peek_retarget");
        } else {
            log_debug!(
                "[dock-preview] peek pid={} wid={} from pid={}",
                target.pid,
                target.wid,
                raise.origin.pid
            );
            PEEK_RAISES.fetch_add(1, Ordering::Relaxed);
            crate::e2e_state::record("dock_preview_peek");
        }
        // The activation pair and the exact-window landing are the switcher's own; a peek is the
        // same switch with a shorter life, so it goes through the same call.
        crate::overlay::activate_and_raise(target.pid, target.wid, false, false);
        // A peeked application comes forward and its windows are ordered above ours: the preview
        // must stay visible over it, or the pointer would be hovering a panel the user cannot see.
        panel::order_front();
    }
}

/// Reset the peek for a new panel session.
///
/// A hidden panel and a disabled feature both end the session's peek state: the model (including a hold keyed on a
/// card of the old panel), the origin, the in-flight raise and the prepared queues. The generation
/// is bumped so a worker that was running across the reset cannot commit into the next session.
fn peek_session_reset() {
    if let Ok(mut slot) = PEEK.lock() {
        slot.model.clear();
        slot.armed_at = None;
        slot.origin = None;
        slot.raising = None;
    }
    PEEK_GENERATION.fetch_add(1, Ordering::AcqRel);
    PEEK_RAISES_READY.lock().unwrap().clear();
    // The restore queue is deliberately NOT cleared: a restore was already decided for a peek that
    // took the front, and the panel closing is exactly when it matters. It carries its own request
    // time (and the window it is undoing) so it can decide without the model, and it stops at
    // RESTORE_RETRY_WINDOW, so it cannot outlive the interaction.
}

/// Release the peek's hold once the pointer has left the card it was holding.
fn release_peek_hold_if_left(zone: layout::PointerZone) {
    let Ok(mut slot) = PEEK.lock() else {
        return;
    };
    let state = &mut *slot;
    let Some(held) = state.model.held() else {
        return;
    };
    let hovered = panel::peek_body_index(zone).and_then(panel::window_at_index);
    let still_on_card = hovered.is_some_and(|(pid, wid)| pid == held.pid && wid == held.wid);
    if !still_on_card {
        state.model.release_hold();
    }
}

/// Release anything the peek holds while no panel is up: a restore for a peek that did not end on
/// its own, and an arm that can never fire because its card is gone.
fn release_peek() {
    let holds_empty = PEEK
        .lock()
        .map(|slot| slot.model.is_idle() && slot.raising.is_none())
        .unwrap_or(true);
    if holds_empty {
        return;
    }
    restore_peek();
}

/// Either end of a peek quit: the peeked application loses the show, the origin loses the
/// application the front is owed back to.
fn handle_peek_app_terminated(pid: i32) {
    let (peeked_pid, origin_pid) = match PEEK.lock() {
        Ok(slot) => (
            slot.model.peeked().map(|target| target.pid),
            slot.origin.as_ref().map(|origin| origin.pid),
        ),
        Err(_) => return,
    };
    if peeked_pid == Some(pid) {
        forget_peek(ClearReason::TargetGone);
    } else if origin_pid == Some(pid) {
        forget_peek(ClearReason::OriginGone);
    }
}

/// One peek target as `--e2e-state` publishes it.
fn json_target(target: PeekTarget) -> String {
    format!("{{\"pid\": {}, \"wid\": {}}}", target.pid, target.wid)
}

/// Take the peek's own restore and every parked one into the queue, without draining it.
///
/// This is the collection half of the teardown: `restore_peek` would drain immediately, which is the
/// second synchronous AX pass this function exists to avoid.
fn collect_peek_then_park_restores() {
    let (target, origin) = {
        let Ok(mut slot) = PEEK.lock() else {
            return;
        };
        let state = &mut *slot;
        let target = state.model.forget();
        (target, state.origin.take())
    };
    if let (Some(target), Some(origin)) = (target, origin) {
        PEEK_RESTORES_READY.lock().unwrap().push(PeekRestore {
            tried_in_this_callback: false,
            peeked: target,
            origin: origin.pid,
            focused_window: origin.focused_window,
            requested_at: Instant::now(),
        });
    }
    let committed = CLICK_COMMITTED.load(Ordering::Acquire);
    let now = Instant::now();
    let parked: Vec<PeekRestore> = match PARKED_RESTORES.lock() {
        Ok(mut parked) => parked
            .drain(..)
            .filter(|(intent, _)| *intent >= committed)
            .map(|(_, mut restore)| {
                restore.requested_at = now;
                restore
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    if !parked.is_empty() {
        PEEK_RESTORES_READY.lock().unwrap().extend(parked);
    }
}

/// Keep draining a pending restore after the feature stopped.
///
/// A restore is bounded by `RESTORE_RETRY_WINDOW` and matters most exactly when the feature is being
/// torn down (the switch was turned off while a peek held the front). The tick that normally retries
/// it has just been stopped, so a short bounded schedule takes over: a handful of delayed attempts,
/// each one main-thread work, none of them after the window closes.
fn schedule_peek_restore_settle() {
    // Idempotent: a restore that is already being retried by a live schedule does not need a second
    // worker, and two workers would drain the same request twice in near-lockstep.
    if RESTORE_SETTLE_ACTIVE.swap(true, Ordering::AcqRel) {
        return;
    }
    if PEEK_RESTORES_READY
        .lock()
        .map(|q| q.is_empty())
        .unwrap_or(true)
    {
        return;
    }
    // The schedule follows the OLDEST request's own deadline rather than a fixed number of polls. A
    // fixed count could stop short of the deadline (a front that becomes readable at 500ms was
    // abandoned at 480ms), and a fixed count that ran to the deadline would poll a dead queue for no
    // reason. The last attempt lands just before `RESTORE_RETRY_WINDOW` closes.
    let deadline = PEEK_RESTORES_READY
        .lock()
        .map(|q| {
            q.iter()
                .map(|restore| restore.requested_at + peek::RESTORE_RETRY_WINDOW)
                .min()
        })
        .ok()
        .flatten();
    let Some(deadline) = deadline else {
        return;
    };
    let spawn = std::thread::Builder::new()
        .name("com.eacryo.oh-my-tab.dock-preview-restore-settle".to_string())
        .spawn(move || {
            // A handful of attempts, each aimed at a fraction of the remaining window, so the last
            // one is inside the deadline instead of before it. This thread only times and hops; the
            // drain itself is main-thread work (it reads NSWorkspace and Accessibility).
            for _ in 0..RESTORE_SETTLE_ATTEMPTS {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    RESTORE_SETTLE_ACTIVE.store(false, Ordering::Release);
                    return;
                }
                // A step derived from the WHOLE window, not from what is left: dividing each round's
                // remaining time by the attempt count shortened every sleep and finished the schedule
                // around 60% of the window (399ms of 600ms), so a front that became readable at 500ms
                // was still dropped.
                //
                // The last attempt is placed `RESTORE_SETTLE_MARGIN` before the deadline, not at it:
                // the drain hops to the main thread and then spends an Accessibility round trip, and
                // a request that arrives at the deadline is dropped before its read for having
                // expired -- which is exactly how a front that became readable at 500ms was still
                // missed.
                std::thread::sleep(
                    (peek::RESTORE_RETRY_WINDOW / (RESTORE_SETTLE_ATTEMPTS as u32 + 1))
                        .min(remaining),
                );
                if PEEK_RESTORES_READY
                    .lock()
                    .map(|q| q.is_empty())
                    .unwrap_or(true)
                {
                    RESTORE_SETTLE_ACTIVE.store(false, Ordering::Release);
                    return;
                }
                settle_peek_restores_on_main();
            }
            // The final attempt, placed inside the window so the hop and the read still fit before
            // the request expires. Anything later is cleanup, not a query.
            let left = deadline.saturating_duration_since(Instant::now());
            std::thread::sleep(left.saturating_sub(RESTORE_SETTLE_MARGIN));
            settle_peek_restores_on_main();
            // Expired requests are removed after the schedule, so they do not sit in the queue until
            // an unrelated event happens to drain them.
            expire_stale_restores();
            RESTORE_SETTLE_ACTIVE.store(false, Ordering::Release);
            // A request that arrived while this schedule was running deserves its own window.
            if PEEK_RESTORES_READY
                .lock()
                .map(|q| !q.is_empty())
                .unwrap_or(false)
            {
                schedule_peek_restore_settle();
            }
        });
    if spawn.is_err() {
        log_debug!("[dock-preview] peek restore settle thread could not start");
        RESTORE_SETTLE_ACTIVE.store(false, Ordering::Release);
        // No synchronous drain here: the caller has already spent this callback's one Accessibility
        // attempt, and a second one is exactly what the budget in `stop()` exists to prevent. The
        // request keeps its window and is retried by the next event or schedule.
    }
}

extern "C" fn on_peek_restore_settle(
    _self: *mut AnyObject,
    _cmd: objc2::runtime::Sel,
    _arg: *mut AnyObject,
) {
    crate::callback_guard::void("dock_preview_peek_restore_settle", || {
        settle_peek_restores_on_main();
    });
}

fn settle_peek_restores_on_main() {
    if !is_main_thread() {
        // Hop, never call across threads: the drain reads AppKit.
        crate::callback_guard::void("dock_preview_peek_restore_settle_hop", || unsafe {
            let target = controller_target();
            let _: () = msg_send![
                target,
                performSelectorOnMainThread: sel!(dockPreviewPeekRestoreSettle:),
                withObject: std::ptr::null::<AnyObject>(),
                waitUntilDone: false
            ];
        });
        return;
    }
    drain_peek_restores();
}

/// The click intent counter. Every click takes a new value, and a paused restore is tagged with the
/// click that paused it: a LATER click that succeeds supersedes it permanently, which is the only
/// way a slow older validation can be stopped from undoing a switch that has already happened.
static CLICK_INTENT: AtomicU64 = AtomicU64::new(0);
/// Restores parked by a click that has not been validated yet: (intent, restore). Owned by the main
/// thread, so `stop()` can see them (a worker holding them alone would be invisible to the teardown,
/// which would then schedule nothing to bring them back).
static PARKED_RESTORES: LazyLock<Mutex<Vec<(u64, PeekRestore)>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));
/// The newest click whose validation SUCCEEDED. A parked restore from an older intent is obsolete.
static CLICK_COMMITTED: AtomicU64 = AtomicU64::new(0);

fn next_click_intent() -> u64 {
    CLICK_INTENT.fetch_add(1, Ordering::AcqRel) + 1
}

/// Park restores for `intent`, dropping anything a newer successful click has already superseded.
fn park_restores(intent: u64, restores: Vec<PeekRestore>) {
    let committed = CLICK_COMMITTED.load(Ordering::Acquire);
    if intent < committed {
        // A later click already landed: this restore would undo it.
        crate::e2e_state::record("dock_preview_peek_restore_superseded");
        return;
    }
    let mut parked = PARKED_RESTORES.lock().unwrap();
    // A NEWER click takes over everything an older one still holds, even when it has nothing of its
    // own to pause (the first click already took the peek apart). Without the transfer, the older
    // request would be orphaned the moment its result is skipped for being superseded, and a window
    // the peek raised could stay in front with nobody to answer for it.
    parked.retain_mut(|(parked_intent, _)| {
        if *parked_intent < intent {
            return false;
        }
        *parked_intent = intent;
        true
    });
    parked.extend(restores.into_iter().map(|restore| (intent, restore)));
}

/// Take back the restores parked for `intent` (the click failed or its worker never ran).
fn unpark_restores(intent: u64) -> Vec<PeekRestore> {
    let mut parked = PARKED_RESTORES.lock().unwrap();
    let mut mine = Vec::new();
    let mut others = Vec::new();
    for (parked_intent, restore) in parked.drain(..) {
        if parked_intent == intent {
            mine.push(restore);
        } else {
            others.push((parked_intent, restore));
        }
    }
    *parked = others;
    let committed = CLICK_COMMITTED.load(Ordering::Acquire);
    if intent < committed {
        crate::e2e_state::record("dock_preview_peek_restore_superseded");
        return Vec::new();
    }
    mine
}

/// Drop every parked restore: a newer click has decided the front.
fn drop_parked_restores() {
    let dropped = PARKED_RESTORES.lock().map(|mut p| p.drain(..).count());
    if dropped.is_ok_and(|n| n > 0) {
        crate::e2e_state::record("dock_preview_peek_restore_superseded");
    }
}

/// Pause the peek for an explicit click, returning what a failed click has to put back.
///
/// `restore_peek` would immediately hand the front to the origin, which is exactly what an explicit
/// click must not do on its way to the window the user picked. So the peek is taken apart WITHOUT
/// building that request: the model is cleared, the origin is moved out, and anything already queued
/// is taken off the queue and handed back to the caller. A successful click drops it; a failed one
/// requeues it, so the front the peek took still has an owner.
fn pause_peek_for_click() -> Vec<PeekRestore> {
    let mut paused: Vec<PeekRestore> = {
        let mut queue = PEEK_RESTORES_READY.lock().unwrap();
        std::mem::take(&mut *queue)
    };
    if let Ok(mut slot) = PEEK.lock() {
        let state = &mut *slot;
        if let Some(target) = state.model.forget() {
            // The peek was up: its own restore is what a failed click owes the origin.
            if let Some(origin) = state.origin.take() {
                paused.push(PeekRestore {
                    tried_in_this_callback: false,
                    peeked: target,
                    origin: origin.pid,
                    focused_window: origin.focused_window,
                    requested_at: Instant::now(),
                });
            }
        }
        state.origin = None;
        state.armed_at = None;
        state.raising = None;
        state.model.clear();
    }
    PEEK_GENERATION.fetch_add(1, Ordering::AcqRel);
    PEEK_RAISES_READY.lock().unwrap().clear();
    if !paused.is_empty() {
        log_debug!(
            "[dock-preview] peek paused for a click: {} restore(s) parked",
            paused.len()
        );
        crate::e2e_state::record("dock_preview_peek_paused");
    }
    paused
}

/// Park the restores a click suspended, tagged with that click's intent. Ownership stays on the main
/// thread, so `stop()` can still see them; the worker only reports whether the click validated.
fn park_paused_for(intent: u64, paused: Vec<PeekRestore>) {
    park_restores(intent, paused);
}

/// Put a paused restore back after the click it was paused for did not happen.
fn requeue_paused_restores(paused: Vec<PeekRestore>) {
    if paused.is_empty() {
        return;
    }
    let now = Instant::now();
    let mut queue = PEEK_RESTORES_READY.lock().unwrap();
    queue.extend(paused.into_iter().map(|mut restore| {
        // The pause is this feature's own bookkeeping, not the user waiting: the request keeps the
        // rest of its window from now, so a slow validation cannot expire it on the way through.
        restore.requested_at = now;
        restore
    }));
    drop(queue);
    // Whatever woke this (a failed click, a stale result) may have happened while the feature was
    // being turned off: in that state nothing else drives the queue, so the bounded schedule takes it.
    if !is_enabled() {
        schedule_peek_restore_settle();
    }
}

/// Remove requests whose window has closed. They are not queries: nothing reads the front for them.
fn expire_stale_restores() {
    let now = Instant::now();
    if let Ok(mut queue) = PEEK_RESTORES_READY.lock() {
        let before = queue.len();
        queue.retain(|restore| {
            now.duration_since(restore.requested_at) <= peek::RESTORE_RETRY_WINDOW
        });
        if queue.len() != before {
            log_debug!(
                "[dock-preview] {} expired peek restore(s) cleaned up",
                before - queue.len()
            );
        }
    }
}

/// Drop restores that have not been performed yet, because something newer decided the front.
///
/// A restore is only ever performed while the peeked application still holds the front, but that
/// test cannot tell "our peek still owns it" from "the user just chose this application". When the
/// user chooses explicitly -- a card click, the switcher, another window -- the pending undo is
/// obsolete and must not run later.
fn supersede_pending_restores(why: &str) {
    // A successful click retires everything older, including restores parked by an earlier click
    // whose validation is still in flight.
    CLICK_COMMITTED.fetch_max(CLICK_INTENT.load(Ordering::Acquire), Ordering::AcqRel);
    drop_parked_restores();
    let dropped = {
        let mut queue = PEEK_RESTORES_READY.lock().unwrap();
        std::mem::take(&mut *queue).len()
    };
    if dropped > 0 {
        log_debug!("[dock-preview] {dropped} pending peek restore(s) dropped: {why}");
        crate::e2e_state::record("dock_preview_peek_restore_superseded");
    }
}

/// Give the front back to the application the peek took it from.
fn restore_peek() {
    let (target, origin) = {
        let Ok(mut slot) = PEEK.lock() else {
            return;
        };
        let state = &mut *slot;
        let target = state.model.forget();
        let origin = state.origin.take();
        state.armed_at = None;
        state.raising = None;
        (target, origin)
    };
    let Some(target) = target else {
        return;
    };
    let Some(origin) = origin else {
        log_debug!(
            "[dock-preview] peek pid={} wid={} dropped without a restore",
            target.pid,
            target.wid
        );
        return;
    };
    PEEK_RESTORES_READY.lock().unwrap().push(PeekRestore {
        tried_in_this_callback: false,
        peeked: target,
        origin: origin.pid,
        focused_window: origin.focused_window,
        requested_at: Instant::now(),
    });
    // A queued hop is lost when the run loop is busy or the controller is not up yet (an early quit,
    // a shutdown): the restore is the one step of this feature that must not depend on a delivery.
    // Performing it here, synchronously, is the same main-thread call the drain would make.
    if is_main_thread() {
        drain_peek_restores();
    } else {
        post_event_to_main();
    }
}

/// Whether the caller is on the AppKit main thread. The peek's teardown can be reached from a
/// notification that arrives on another thread, and the restore is main-thread work.
fn is_main_thread() -> bool {
    unsafe {
        let main: bool = msg_send![class!(NSThread), isMainThread];
        main
    }
}

/// Hold the card under the pointer after the user's own switch ended a peek.
///
/// Every "the user went somewhere themselves" exit goes through this, whether the peek was up, a
/// raise was in flight, or the card was only armed. Without it the next tick armed the same card
/// again and took the front back from the application the user had just chosen -- and the in-flight
/// case had no card to hold at all, because the arm was cleared when the worker started.
fn hold_peek_card(reason: ClearReason) {
    let was_raising = PEEK
        .lock()
        .map(|slot| slot.raising.is_some())
        .unwrap_or(false);
    forget_peek(reason);
    if let Some((pid, wid)) =
        panel::peek_body_index(panel::current_pointer_zone()).and_then(panel::window_at_index)
    {
        hold_card(PeekTarget { pid, wid });
    }
    log_debug!(
        "[dock-preview] peek ended by the user ({reason:?}) raising={was_raising}; the hovered card is held"
    );
}

/// Put one card on hold until the pointer leaves it.
fn hold_card(target: PeekTarget) {
    if let Ok(mut slot) = PEEK.lock() {
        slot.model.hold(target);
        slot.armed_at = None;
        slot.origin = None;
        slot.raising = None;
    }
}

/// Forget a peek that is up without restoring it.
fn forget_peek(reason: ClearReason) {
    let Ok(mut slot) = PEEK.lock() else {
        return;
    };
    let state = &mut *slot;
    if let Some(target) = state.model.forget() {
        log_debug!(
            "[dock-preview] peek forgotten pid={} wid={} reason={reason:?}",
            target.pid,
            target.wid
        );
        crate::e2e_state::record("dock_preview_peek_forget");
    }
    state.armed_at = None;
    state.origin = None;
    state.raising = None;
}

/// Perform the restores the tick asked for. Main thread only, and deliberately the same call the
/// peek itself uses: fronting the origin application and landing the window it was showing.
fn drain_peek_restores() {
    drain_peek_restores_inner(0);
}

/// `retries` counts the requeue rounds made INSIDE this drain. Each requeued request is retried once
/// per call; a caller with a long bounded frame (the panel teardown) may ask for a few rounds so a
/// restore whose front becomes readable a moment later still lands without waiting for a tick.
fn drain_peek_restores_inner(_max_rounds: usize) {
    // One pass, always: every request gets at most one Accessibility attempt per callback. Requests
    // that could not be read are re-queued and retried by the tick or the settle schedule, never in a
    // loop on the caller's thread.
    drain_peek_restores_once();
}

/// One pass over the queue. Returns whether anything was requeued.
fn drain_peek_restores_once() -> bool {
    let restores: Vec<PeekRestore> = {
        let mut queue = PEEK_RESTORES_READY.lock().unwrap();
        std::mem::take(&mut *queue)
    };
    let mut requeued: Vec<PeekRestore> = Vec::new();
    for restore in restores {
        if !windows::process_is_running(restore.origin) {
            log_debug!(
                "[dock-preview] peek restore skipped: origin pid={} is gone",
                restore.origin
            );
            continue;
        }
        // The ownership sample is taken TWICE: before the Accessibility read, which decides whether
        // the focused window is worth asking for, and again after it, which is what the restore is
        // authorised by. The read itself is a synchronous round trip, and the user can switch
        // applications during it; authorising on the pre-read sample would then restore over the
        // application they switched to.
        // An expired request is dropped before the Accessibility read: its window has closed, and a
        // round trip on its behalf would only block the main thread for an answer nobody uses.
        if Instant::now().duration_since(restore.requested_at) > peek::RESTORE_RETRY_WINDOW {
            log_debug!(
                "[dock-preview] peek restore expired before its read: pid={}",
                restore.peeked.pid
            );
            continue;
        }
        if restore.tried_in_this_callback {
            // Already attempted in this callback (the teardown drains once, but a request can come
            // back through this function twice): carry it to the next one without another AX read.
            let mut restore = restore;
            restore.tried_in_this_callback = false;
            requeued.push(restore);
            continue;
        }
        let front_before = peek_frontmost();
        let focused_from_peeked = match front_before {
            Frontmost::Other { pid, .. } if pid == restore.peeked.pid => unsafe {
                crate::window_collector::focused_window_cgwid(pid)
            },
            _ => None,
        };
        let front = match peek_frontmost() {
            Frontmost::Other { pid, .. } => Some(peek::FrontNow {
                pid,
                focused_window: if pid == restore.peeked.pid {
                    focused_from_peeked
                } else {
                    None
                },
            }),
            Frontmost::Own => Some(peek::FrontNow {
                pid: std::process::id() as i32,
                focused_window: None,
            }),
            Frontmost::Unknown => None,
        };
        if matches!(front, Some(peek::FrontNow { pid, .. }) if pid != restore.peeked.pid)
            && matches!(front_before, Frontmost::Other { pid, .. } if pid == restore.peeked.pid)
        {
            // The front changed hands while the Accessibility read ran: that is the user's switch,
            // and the restore must not undo it.
            log_debug!("[dock-preview] peek restore abandoned: the front changed during the read");
            crate::e2e_state::record("dock_preview_peek_restore_skipped");
            continue;
        }
        match peek::restore_step(
            restore.requested_at,
            Instant::now(),
            restore.peeked,
            restore.origin,
            front,
        ) {
            peek::RestoreStep::Proceed => {}
            peek::RestoreStep::Retry => {
                // The front could not be read, or it is already where the restore would put it.
                // Keep the request for the rest of its window instead of dropping it on one
                // unreadable sample.
                requeued.push(restore);
                continue;
            }
            peek::RestoreStep::Abandon => {
                log_debug!(
                    "[dock-preview] peek restore abandoned: pid={} wid={}",
                    restore.peeked.pid,
                    restore.peeked.wid
                );
                crate::e2e_state::record("dock_preview_peek_restore_skipped");
                continue;
            }
        }
        log_debug!(
            "[dock-preview] peek restore -> pid={} wid={:?}",
            restore.origin,
            restore.focused_window
        );
        match restore.focused_window {
            // The origin's own focused window, when it was readable: the switch lands that window
            // rather than whichever one of the application's windows comes up first.
            Some(wid) => crate::overlay::activate_and_raise(restore.origin, wid, false, false),
            None => {
                crate::window_collector::activate_pid(restore.origin);
            }
        }
        PEEK_RESTORES.fetch_add(1, Ordering::Relaxed);
        crate::e2e_state::record("dock_preview_peek_restore");
    }
    if requeued.is_empty() {
        return false;
    }
    {
        let mut queue = PEEK_RESTORES_READY.lock().unwrap();
        queue.extend(requeued);
    }
    true
    // NOT `post_event_to_main()`: the pump clears its coalescing flag on entry and drains this queue
    // again immediately, so an unreadable front would spin the main thread. The 50ms tick drains it
    // too, and `RESTORE_RETRY_WINDOW` bounds how long that can go on.
}

fn hide_panel() {
    let was_visible = panel::is_visible();
    // A peek whose panel is gone has no card left to return to: give the front back before the
    // panel and its state disappear. The hold is then dropped as well -- it describes a card of a
    // panel that is about to be rebuilt, and carrying it into the next session would leave a card
    // quiet for a reason the user can no longer see.
    restore_peek();
    peek_session_reset();
    stop_dock_reveal_keeper();
    panel::hide();
    panel::flush_records();
    *CLOSE_AT.lock().unwrap() = None;
    *CURRENT_APP.lock().unwrap() = None;
    PENDING_OPEN.lock().unwrap().take();
    REQUEST.fetch_add(1, Ordering::AcqRel);
    // The press latch is not cleared here: hide_panel also runs for a queued notification that can
    // predate the press, and the tick's pointer sample is what releases the latch.
    // Invalidate every capture request the closed panel queued: a hidden preview must not keep
    // capturing windows for a panel that no longer exists.
    crate::thumbnail::begin_dock_capture_epoch();
    if was_visible {
        crate::e2e_state::record("dock_preview_hide");
    }
}

/// Synthetic entries for the development entries: the window ids are distinct and non-zero so a
/// scenario can tell one row from another, but nothing behind them exists, so a click on one is
/// refused by the identity check exactly like a stale real target.
pub(crate) fn synthetic_windows(titles: &[&str]) -> Vec<windows::PreviewWindow> {
    let pid = std::process::id() as i32;
    titles
        .iter()
        .enumerate()
        .map(|(index, title)| windows::PreviewWindow {
            pid,
            wid: 900_001 + index as u32,
            title: (*title).to_string(),
            minimized: false,
            app_hidden: false,
            other_desktop: false,
            active: index == 0,
            aspect: Some(1.6),
            may_capture: false,
        })
        .collect()
}

/// Development entry: show the panel for an explicit window list, so the list layout and its
/// scrolling can be driven without an app that happens to have that many windows.
pub(crate) fn dev_show_windows(titles: &[&str], edge: DockEdge) -> bool {
    if !is_enabled() {
        return false;
    }
    let screen = primary_screen_bounds_cg();
    // The smoke CONSTRUCTS the budget it needs instead of depending on the display: the tile sits
    // on the visible frame's bottom edge, and that frame is exactly the requested room tall and
    // wide, so the layout's budget is the requested room (minus its own margin). A real run uses
    // the display's own visible frame.
    let visible = match SMOKE_ROOM_H.load(Ordering::Acquire) {
        true => {
            let room = |slot: &Mutex<Option<f64>>, name: &str, fallback: f64| {
                slot.lock()
                    .ok()
                    .and_then(|room| *room)
                    .or_else(|| {
                        crate::dev_flags::value(name)
                            .and_then(|value| value.trim().parse::<f64>().ok())
                            .filter(|room| room.is_finite() && *room > 0.0)
                    })
                    .unwrap_or(fallback)
            };
            // The flags state the BUDGET, which is what reaches the layout: a bottom Dock measures
            // from the tile's top edge to the visible frame's top and subtracts `PANEL_MARGIN` ONCE.
            // The frame therefore ends at the tile's top edge, not `PANEL_MARGIN` above it, and the
            // height is the budget plus that single margin -- adding it twice left the viewport 8pt
            // taller than the two rows the case claims.
            let room_h = room(&CASE_ROOM_H, "smoke-dock-room-h", 400.0);
            // A card row needs `CARD_H` plus its padding; a narrower fallback would make the
            // horizontal case impossible to construct on any display.
            let room_w = room(
                &CASE_ROOM_W,
                "smoke-dock-room-w",
                layout::CARD_W + layout::PANEL_PAD * 4.0,
            );
            // Height and width compensate DIFFERENTLY, because `space_budget` measures them
            // differently: a bottom Dock measures width across the screen (a margin on each side)
            // and height from the tile's top edge (one margin, where the frame ends).
            Rect::new(
                screen.x,
                screen.y,
                (room_w + layout::PANEL_MARGIN * 2.0).min(screen.w),
                (room_h + layout::PANEL_MARGIN).min(screen.h),
            )
        }
        false => visible_frame_of(screen),
    };
    // The controlled room is measured to the tile's TOP edge, which is what the layout budget
    // measures: the anchor's own height hangs below that edge.
    let bottom = visible.max_y();
    let anchor = match edge {
        DockEdge::Bottom => Rect::new(screen.x + screen.w / 2.0 - 22.0, bottom, 44.0, 56.0),
        DockEdge::Top => Rect::new(screen.x + screen.w / 2.0 - 22.0, screen.y, 44.0, 56.0),
        DockEdge::Left => Rect::new(screen.x, screen.y + screen.h / 2.0 - 22.0, 56.0, 44.0),
        DockEdge::Right => Rect::new(
            screen.max_x() - 56.0,
            screen.y + screen.h / 2.0 - 22.0,
            56.0,
            44.0,
        ),
    };
    let pid = std::process::id() as i32;
    let request = REQUEST.fetch_add(1, Ordering::AcqRel) + 1;
    let ticket = crate::thumbnail::begin_dock_capture_epoch();
    let shown = panel::show(
        panel::PanelRequest {
            pid,
            thumbnail_generation: crate::thumbnail::pid_generation_now(pid),
            bundle: String::new(),
            identity_key: String::new(),
            process_start_time_us: None,
            windows: synthetic_windows(titles),
            anchor,
            edge,
            visible,
        },
        request,
        ticket,
    );
    if shown {
        *CURRENT_APP.lock().unwrap() = Some(CurrentApp {
            pid,
            identity_key: String::new(),
            process_start_time_us: None,
            anchor,
            edge,
            request,
        });
        *CLOSE_AT.lock().unwrap() = None;
    }
    shown
}

/// Development entry: show the panel with a synthetic window list, so the panel's own views and
/// geometry can be asserted without depending on which app happens to have windows. The cards
/// carry no real window ids; this path is presentation-only and no click is driven through it.
pub(crate) fn dev_show_synthetic(titles: &[&str]) -> bool {
    if !is_enabled() {
        return false;
    }
    let screen = primary_screen_bounds_cg();
    let visible = visible_frame_of(screen);
    let anchor = Rect::new(
        screen.x + screen.w / 2.0 - 22.0,
        screen.max_y() - 60.0,
        44.0,
        56.0,
    );
    let pid = std::process::id() as i32;
    let windows = synthetic_windows(titles);
    let request = REQUEST.fetch_add(1, Ordering::AcqRel) + 1;
    let ticket = crate::thumbnail::begin_dock_capture_epoch();
    let app = CurrentApp {
        pid,
        identity_key: String::new(),
        process_start_time_us: None,
        anchor,
        edge: DockEdge::Bottom,
        request,
    };
    let shown = panel::show(
        panel::PanelRequest {
            pid,
            thumbnail_generation: crate::thumbnail::pid_generation_now(pid),
            bundle: String::new(),
            identity_key: String::new(),
            process_start_time_us: None,
            windows,
            anchor,
            edge: DockEdge::Bottom,
            visible,
        },
        request,
        ticket,
    );
    if shown {
        *CURRENT_APP.lock().unwrap() = Some(app);
        *CLOSE_AT.lock().unwrap() = None;
    }
    shown
}

/// Development entry: show the panel for a bundle id or PID without a real hover.
pub(crate) fn dev_show(target: &str) {
    if !is_enabled() {
        log_info!("[dock-preview] --show-dock-preview ignored: the feature is off");
        return;
    }
    let resolved = if let Ok(pid) = target.parse::<i32>() {
        running_pid(pid).map(|pid| (pid, bundle_for_pid(pid)))
    } else {
        running_pid_for_bundle(target).map(|pid| (pid, Some(target.to_string())))
    };
    let Some((pid, bundle)) = resolved else {
        log_info!("[dock-preview] --show-dock-preview: no running app for {target}");
        return;
    };
    let bundle = bundle.unwrap_or_default();
    let dock = *DOCK_STATE.lock().unwrap();
    let anchor = dock
        .list
        .map(|list| Rect::new(list.x + list.w / 2.0 - 22.0, list.y - 56.0, 44.0, 56.0))
        .unwrap_or(Rect::new(700.0, 820.0, 44.0, 56.0));
    let edge = dock.edge.unwrap_or(DockEdge::Bottom);
    let screen = dock.screen.unwrap_or_else(primary_screen_bounds_cg);
    let visible = visible_frame_of(screen);
    let request = REQUEST.fetch_add(1, Ordering::AcqRel) + 1;
    *COLLECT_CONTEXT.lock().unwrap() = Some(CollectContext {
        request,
        bundle: bundle.clone(),
        anchor,
        edge,
        visible,
        dev: true,
        pid,
    });
    spawn_collector(request, pid, bundle);
}

/// The `dock_preview` section of `--e2e-state`: the panel facts plus the Dock's tile list, so a
/// scenario can move the real pointer onto a known tile and then click a card.
pub(crate) fn e2e_json() -> String {
    let dock = crate::dock_preview::observer::current_snapshot();
    let items: Vec<String> = dock
        .items
        .iter()
        .map(|(bundle, rect)| {
            format!(
                "{{\"bundle\": {}, \"frame\": {{\"x\": {:.1}, \"y\": {:.1}, \"w\": {:.1}, \"h\": {:.1}}}}}",
                json_string(bundle),
                rect.x,
                rect.y,
                rect.w,
                rect.h
            )
        })
        .collect();
    let rect_json = |rect: Rect| {
        format!(
            "{{\"x\": {:.1}, \"y\": {:.1}, \"w\": {:.1}, \"h\": {:.1}}}",
            rect.x, rect.y, rect.w, rect.h
        )
    };
    let dock_json = format!(
        "{{\"revealed\": {}, \"edge\": {}, \"list\": {}, \"screen\": {}, \"items\": [{}]}}",
        dock.revealed,
        dock.edge
            .map(|edge| json_string(edge.name()))
            .unwrap_or_else(|| "null".to_string()),
        dock.list
            .map(rect_json)
            .unwrap_or_else(|| "null".to_string()),
        dock.screen
            .map(rect_json)
            .unwrap_or_else(|| "null".to_string()),
        items.join(", ")
    );
    let preview = crate::dock_preview::panel::e2e_json();
    // Monotonic, so a scenario asserts a delta across its injected press instead of reading a
    // single event name that the next write would overwrite.
    let press_dismissals = PRESS_DISMISSALS.load(Ordering::Relaxed);
    // The peek, as a scenario reads it: the two monotonic counters (a raise without its restore is
    // the failure this feature is judged on), what is armed or up right now, and the front the peek
    // will give back.
    let (peek_armed, peeked, peek_origin) = PEEK
        .lock()
        .map(|slot| {
            (
                slot.model.armed(),
                slot.model.peeked(),
                slot.origin.as_ref().map(|origin| origin.pid),
            )
        })
        .unwrap_or((None, None, None));
    let peek_json = format!(
        "{{\"enabled\": {}, \"armed\": {}, \"peeked\": {}, \"origin\": {}, \"raises\": {}, \"restores\": {}, \"pressed\": {}}}",
        peek_is_enabled(),
        peek_armed.map_or("null".to_string(), json_target),
        peeked.map_or("null".to_string(), json_target),
        peek_origin.map_or("null".to_string(), |pid| pid.to_string()),
        PEEK_RAISES.load(Ordering::Relaxed),
        PEEK_RESTORES.load(Ordering::Relaxed),
        PEEK_PRESSED.load(Ordering::Relaxed),
    );
    if preview == "null" {
        format!(
            "{{\"visible\": false, \"dock\": {dock_json}, \"press_dismissals\": {press_dismissals}, \"peek\": {peek_json}}}"
        )
    } else {
        format!(
            "{{\"panel\": {preview}, \"dock\": {dock_json}, \"press_dismissals\": {press_dismissals}, \"peek\": {peek_json}}}"
        )
    }
}

// ---------------------------------------------------------------------------------------------
// Main-thread event pump
// ---------------------------------------------------------------------------------------------

fn post_event_to_main() {
    if EVENT_POSTED.swap(true, Ordering::AcqRel) {
        return;
    }
    crate::callback_guard::void("dock_preview_post", || unsafe {
        let target = controller_target();
        let _: () = msg_send![
            target,
            performSelectorOnMainThread: sel!(dockPreviewEvent:),
            withObject: std::ptr::null::<AnyObject>(),
            waitUntilDone: false
        ];
    });
}

extern "C" fn on_event(_self: *mut AnyObject, _cmd: objc2::runtime::Sel, _arg: *mut AnyObject) {
    crate::callback_guard::void("dock_preview_event", || {
        EVENT_POSTED.store(false, Ordering::Release);
        // The press is handled before the queued results: it happened first, and the dismissal it
        // recorded must be able to reject a collector result from the same turn.
        let press_session = DOCK_CLICK_PENDING.swap(0, Ordering::AcqRel);
        if press_session != 0 {
            dismiss_for_dock_click(press_session);
        }
        drain_events();
        drain_results();
        drain_close_results();
        drain_activate_results();
        drain_peek_raises();
        drain_peek_restores();
    });
}

extern "C" fn on_tick(_self: *mut AnyObject, _cmd: objc2::runtime::Sel, _timer: *mut AnyObject) {
    crate::callback_guard::void("dock_preview_tick", || {
        // Safety net for a press whose main-thread hop was lost: the tick already runs at the
        // feature's 50ms cadence, so the dismissal cannot be missed entirely.
        let press_session = DOCK_CLICK_PENDING.swap(0, Ordering::AcqRel);
        if press_session != 0 {
            dismiss_for_dock_click(press_session);
        }
        if is_enabled() {
            tick();
        }
    });
}

fn drain_events() {
    let events: Vec<DockEvent> = {
        let mut queue = EVENTS.lock().unwrap();
        std::mem::take(&mut *queue)
    };
    for event in events {
        handle_event(event);
    }
}

fn handle_event(event: DockEvent) {
    if !decisions::event_is_current(event.session, SESSION.load(Ordering::Acquire)) {
        return;
    }
    {
        let mut state = DOCK_STATE.lock().unwrap();
        state.list = event.list_frame;
        if event.list_frame.is_some() {
            state.list_at = Some(event.sampled_at);
        }
        state.screen = event.screen_frame;
        state.revealed = event.revealed;
        if let Selection::App { edge, .. } = &event.selection {
            state.edge = Some(*edge);
        }
    }
    let selection_pid = match &event.selection {
        Selection::App { pid, .. } => Some(*pid),
        Selection::NonApp | Selection::None => None,
    };
    *DOCK_SELECTION.lock().unwrap() = selection_pid.map(|pid| (pid, event.revealed));
    // Leaving the tile cancels a preview that has not opened yet; the panel's own close grace
    // still decides what happens to one that is already on screen.
    let pending_stale = {
        let pending = PENDING_OPEN.lock().unwrap();
        match (pending.as_ref(), selection_pid) {
            (Some(open), Some(pid)) => open.pid != pid,
            (Some(_), None) => true,
            (None, _) => false,
        }
    };
    if pending_stale {
        PENDING_OPEN.lock().unwrap().take();
    }
    match event.selection {
        Selection::App {
            pid,
            bundle,
            icon,
            edge,
            ..
        } => {
            if pid == std::process::id() as i32 {
                return;
            }
            if other_panel_visible() {
                hide_panel();
                return;
            }
            // A sample that *started* after the press describes a position the pointer has taken
            // since then: if it is not the pressed tile, the press can no longer explain an open. A
            // sample that started before the press may reach the queue afterwards and still carry
            // an older selection, so it must not clear it.
            {
                let mut press = DOCK_PRESS.lock().unwrap();
                if press
                    .is_some_and(|press| event.sampled_at > press.at && !icon.contains(press.point))
                {
                    *press = None;
                }
            }
            // The press that dismissed this tile also keeps it dismissed: the pointer is still on
            // the icon, and the Dock keeps reporting it as selected, so a notification (or its own
            // forced re-read) must not re-open what the press just closed.
            if decisions::dismissed_tile_suppresses(
                *DISMISSED_TILE.lock().unwrap(),
                pid,
                pointer_in_cg(),
            ) {
                return;
            }
            // Same app: keep the panel and follow the tile's current rectangle.
            let current_pid = CURRENT_APP.lock().unwrap().as_ref().map(|app| app.pid);
            if panel::is_visible()
                && decisions::switch_decision(current_pid, pid) == SwitchDecision::Keep
            {
                if let Some(visible) = event.screen_frame.map(visible_frame_of) {
                    if !panel::update_anchor(icon, edge, visible) {
                        // The tile moved somewhere the panel no longer fits: hide it instead of
                        // leaving a stale frame behind.
                        log_debug!("[dock-preview] tile moved past the room the panel needs");
                        hide_panel();
                        return;
                    }
                    update_dock_reveal_keeper(icon, edge);
                    if let Some(app) = CURRENT_APP.lock().unwrap().as_mut() {
                        app.anchor = icon;
                        app.edge = edge;
                        app.request = REQUEST.load(Ordering::Acquire);
                    }
                }
                return;
            }
            let same_pending = {
                let pending = PENDING_OPEN.lock().unwrap();
                pending.as_ref().map(|open| open.pid) == Some(pid)
            };
            if same_pending {
                let mut pending = PENDING_OPEN.lock().unwrap();
                if let Some(pending) = pending.as_mut() {
                    pending.icon = icon;
                    pending.edge = edge;
                }
                return;
            }
            // The press that produced this notification may have been handled first, when there was
            // nothing to dismiss yet: the point it carried is what ties the two together. The match
            // and the consumption happen under one lock, so a press the watcher writes in between
            // cannot be dropped unseen.
            let suppressed = {
                let mut press = DOCK_PRESS.lock().unwrap();
                let snapshot = press.map(|press| (press.session, press.at, press.point));
                if decisions::press_suppresses_open(
                    snapshot,
                    SESSION.load(Ordering::Acquire),
                    icon,
                    Instant::now(),
                ) {
                    *press = None;
                    true
                } else {
                    false
                }
            };
            if suppressed {
                log_debug!("[dock-preview] open suppressed: this tile was just pressed");
                *DISMISSED_TILE.lock().unwrap() = Some((pid, icon));
                return;
            }
            hide_panel();
            // A press can land inside the open delay, when no panel exists yet: the watcher has to
            // be listening before the panel is, or that press would show a preview afterwards.
            start_dock_click_watcher();
            let request = REQUEST.fetch_add(1, Ordering::AcqRel) + 1;
            *PENDING_OPEN.lock().unwrap() = Some(PendingOpen {
                at: Instant::now() + decisions::OPEN_DELAY,
                pid,
                bundle,
                icon,
                edge,
                request,
            });
        }
        // A non-application tile or no tile: the panel's close timer decides using the real
        // pointer, so a Dock that hides under a card does not dismiss it. `Selection::None` also
        // covers a failed selection read and a teardown, so it is not evidence that the pointer
        // left; the press and the latch are released by the tick's pointer sample.
        Selection::NonApp | Selection::None => {}
    }
}

fn visible_frame_of(screen: Rect) -> Rect {
    // The menu bar is the only reserved band the preview has to respect: the Dock either hides
    // itself or is already excluded by the anchor's edge.
    let visible = Rect::new(
        screen.x,
        screen.y + 24.0,
        screen.w,
        (screen.h - 24.0).max(1.0),
    );
    // `--smoke-dock-room-h=<pt>` shrinks the room ABOVE a bottom tile so a scrolling case can be
    // CONSTRUCTED instead of hoped for (a full-height screen fits every window list a test can
    // reasonably open). It is read only while a smoke case has switched it on: the ordinary smoke
    // previews run with the screen's own room, so the room cannot make them disappear.
    match SMOKE_ROOM_H
        .load(Ordering::Acquire)
        .then(|| crate::dev_flags::value("smoke-dock-room-h"))
        .flatten()
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|room| room.is_finite() && *room > 0.0)
    {
        Some(room) => {
            let bottom = screen.max_y();
            Rect::new(
                visible.x,
                (bottom - room).max(visible.y),
                visible.w,
                (bottom - (bottom - room).max(visible.y)).max(1.0),
            )
        }
        None => visible,
    }
}

fn tick() {
    if other_panel_visible() {
        hide_panel();
        // A restore that was decided before the other panel came up still has to run for the rest of
        // its window: this early return used to be the one path that dropped it.
        drain_peek_restores();
        return;
    }
    let now = Instant::now();
    let pointer = pointer_in_cg().unwrap_or((f64::NAN, f64::NAN));
    // Releasing the press and its latch happens here, before any decision this tick makes: leaving
    // the dismissed tile is what makes the next hover able to open a preview again.
    release_dismiss_state(pointer);
    if panel::is_visible() {
        // Moving along the Dock from the previewed icon onto a different tile (including a tile
        // whose app is not running, where the observer's selection event never arrives) ends the
        // preview, but not on the tick it is first seen: the trip towards the cards is often
        // diagonal and the pointer is inside the Dock's list for all of it, because the panel's
        // bottom edge sits on the icon's top edge. The verdict takes the leave grace instead, so a
        // crossing is cancelled by the panel and a real hover still ends the preview.
        let other_tile = {
            let dock = *DOCK_STATE.lock().unwrap();
            match (dock.list, panel::anchor_rect()) {
                (Some(list), Some((anchor, _))) => {
                    decisions::pointer_is_on_other_dock_tile(list, anchor, pointer)
                }
                _ => false,
            }
        };
        let outcome = panel::tick(now, pointer);
        panel::flush_records();
        if outcome == panel::TickOutcome::Empty {
            hide_panel();
            return;
        }
        let zone = panel::current_pointer_zone();
        let scheduled = *CLOSE_AT.lock().unwrap();
        match decisions::tick_decision(zone, other_tile, now, scheduled) {
            CloseDecision::Keep => *CLOSE_AT.lock().unwrap() = None,
            CloseDecision::Wait => {}
            CloseDecision::Schedule(at) => {
                // Logged where the grace is armed, not on every sample: a pointer the panel itself
                // claims keeps the verdict true without ever starting a close.
                if other_tile {
                    log_debug!(
                        "[dock-preview] pointer left the anchor tile; the leave grace decides"
                    );
                }
                *CLOSE_AT.lock().unwrap() = Some(at);
            }
            CloseDecision::HideNow => hide_panel(),
        }
        // Only while the panel is up: the peek acts on its cards, and a hidden panel has none.
        if panel::is_visible() {
            step_peek(now, zone);
            release_peek_hold_if_left(zone);
        }
    } else {
        // No panel, no card: anything the peek was holding goes back.
        release_peek();
    }
    // A pending restore is retried on the tick's own cadence, WHETHER OR NOT the panel is up: a peek
    // whose front was unreadable when the pointer left often ends exactly because the panel is
    // closing, and gating this on `panel::is_visible()` left that request queued until it expired.
    drain_peek_restores();
    let due = {
        let mut pending = PENDING_OPEN.lock().unwrap();
        match pending.as_ref() {
            Some(open) if now >= open.at => pending.take(),
            _ => None,
        }
    };
    if let Some(open) = due {
        let state = *DOCK_STATE.lock().unwrap();
        let Some(screen) = state.screen else {
            return;
        };
        // A press that dismissed this tile holds it closed until the pointer leaves it. The pending
        // entry is already consumed, so this only drops the open; the latch stays for the next
        // notification.
        if decisions::dismissed_tile_suppresses(
            *DISMISSED_TILE.lock().unwrap(),
            open.pid,
            pointer_in_cg(),
        ) {
            log_debug!("[dock-preview] open suppressed: the tile was pressed");
            return;
        }
        // An unreadable pointer is not a hover: only a pointer that is actually inside the tile may
        // open a preview, or a notification that lagged behind a slide to the neighbouring tile
        // would still open this one.
        let pointer_near_tile = pointer_in_cg().is_some_and(|point| open.icon.contains(point));
        if !decisions::should_open(
            is_enabled(),
            state.revealed,
            pointer_near_tile,
            true,
            other_panel_visible(),
        ) {
            log_debug!("[dock-preview] open cancelled: the pointer is no longer on the tile");
            hide_panel();
            return;
        }
        let visible = visible_frame_of(screen);
        *COLLECT_CONTEXT.lock().unwrap() = Some(CollectContext {
            request: open.request,
            bundle: open.bundle.clone(),
            anchor: open.icon,
            edge: open.edge,
            visible,
            dev: false,
            pid: open.pid,
        });
        spawn_collector(open.request, open.pid, open.bundle);
    }
}

fn spawn_collector(request: u64, pid: i32, bundle: String) {
    log_debug!("[dock-preview] collecting windows for pid={pid} bundle={bundle}");
    let _ = std::thread::Builder::new()
        .name("com.eacryo.oh-my-tab.dock-preview-collect".to_string())
        .spawn(move || {
            let collected = windows::collect(pid);
            RESULTS.lock().unwrap().push((request, collected));
            post_event_to_main();
        });
}

fn drain_results() {
    let results: Vec<CollectorResult> = {
        let mut queue = RESULTS.lock().unwrap();
        std::mem::take(&mut *queue)
    };
    for (request, collected) in results {
        if !decisions::event_is_current(request, REQUEST.load(Ordering::Acquire)) {
            continue;
        }
        let Some(collected) = collected else {
            log_debug!("[dock-preview] collection returned nothing");
            continue;
        };
        if collected.windows.is_empty() {
            log_debug!("[dock-preview] app has no previewable windows");
            continue;
        }
        let context = {
            let mut slot = COLLECT_CONTEXT.lock().unwrap();
            match slot.as_ref() {
                Some(context) if context.request == request => slot.take(),
                _ => None,
            }
        };
        let Some(context) = context else {
            continue;
        };
        if context.pid != collected.pid {
            continue;
        }
        if !context.dev {
            // A slow Accessibility pass can finish after the pointer left the tile or after another
            // floating surface took the screen; a request id alone is not evidence of hover.
            let still_hovered = {
                let selection = *DOCK_SELECTION.lock().unwrap();
                let pointer_near_tile =
                    pointer_in_cg().is_some_and(|point| context.anchor.contains(point));
                matches!(selection, Some((pid, true)) if pid == collected.pid) && pointer_near_tile
            };
            if !still_hovered || other_panel_visible() {
                log_debug!(
                    "[dock-preview] collected windows are stale for pid={}",
                    collected.pid
                );
                continue;
            }
        }
        let (anchor, edge, visible) = (context.anchor, context.edge, context.visible);
        let bundle = context.bundle.clone();
        PENDING_OPEN.lock().unwrap().take();
        let ticket = crate::thumbnail::begin_dock_capture_epoch();
        let app = CurrentApp {
            pid: collected.pid,
            identity_key: collected.identity_key.clone(),
            process_start_time_us: collected.process_start_time_us,
            anchor,
            edge,
            request,
        };
        let panel_request = panel::PanelRequest {
            pid: collected.pid,
            thumbnail_generation: collected.thumbnail_generation,
            bundle: bundle.clone(),
            identity_key: collected.identity_key.clone(),
            process_start_time_us: collected.process_start_time_us,
            windows: collected.windows,
            anchor,
            edge,
            visible,
        };
        if panel::show(panel_request, request, ticket) {
            *CURRENT_APP.lock().unwrap() = Some(app);
            *CLOSE_AT.lock().unwrap() = None;
            if !context.dev {
                start_dock_reveal_keeper(anchor, edge);
            }
            log_info!("[dock-preview] panel shown for pid={}", collected.pid);
            crate::e2e_state::record("dock_preview_show");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Tick timer
// ---------------------------------------------------------------------------------------------

fn start_tick_timer() {
    crate::assert_main_thread("dock_preview::start_tick_timer");
    let target = controller_target();
    let interval = TICK_INTERVAL.as_secs_f64();
    unsafe {
        let timer: *mut AnyObject = msg_send![
            class!(NSTimer),
            scheduledTimerWithTimeInterval: interval,
            target: target,
            selector: sel!(dockPreviewTick:),
            userInfo: std::ptr::null::<AnyObject>(),
            repeats: true
        ];
        if !timer.is_null() {
            *TICK_TIMER.lock().unwrap() = Some(timer);
        }
    }
}

fn stop_tick_timer() {
    if let Ok(mut slot) = TICK_TIMER.lock() {
        if let Some(timer) = slot.take() {
            unsafe {
                let _: () = msg_send![timer, invalidate];
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Objective-C target
// ---------------------------------------------------------------------------------------------

fn controller_target() -> *mut AnyObject {
    static CLASS: std::sync::OnceLock<crate::ffi::StaticClass> = std::sync::OnceLock::new();
    static INSTANCE: std::sync::OnceLock<crate::ffi::CallbackTarget> = std::sync::OnceLock::new();
    INSTANCE
        .get_or_init(|| {
            let class = crate::ffi::StaticClass::register_once(
                &CLASS,
                c"OhMyTabDockPreviewController",
                || {
                    crate::ffi::build_class(
                        c"OhMyTabDockPreviewController",
                        class!(NSObject),
                        |builder| unsafe {
                            builder.add_method::<AnyObject, _>(
                                sel!(dockPreviewEvent:),
                                on_event
                                    as unsafe extern "C" fn(
                                        *mut AnyObject,
                                        objc2::runtime::Sel,
                                        *mut AnyObject,
                                    ),
                            );
                            builder.add_method::<AnyObject, _>(
                                sel!(dockPreviewPeekRestoreSettle:),
                                on_peek_restore_settle
                                    as unsafe extern "C" fn(
                                        *mut AnyObject,
                                        objc2::runtime::Sel,
                                        *mut AnyObject,
                                    ),
                            );
                            builder.add_method::<AnyObject, _>(
                                sel!(dockPreviewTick:),
                                on_tick
                                    as unsafe extern "C" fn(
                                        *mut AnyObject,
                                        objc2::runtime::Sel,
                                        *mut AnyObject,
                                    ),
                            );
                        },
                    )
                },
            );
            let instance: *mut AnyObject = unsafe { msg_send![class.0 as *mut AnyObject, new] };
            crate::ffi::CallbackTarget::new(instance)
        })
        .0
}

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

fn running_pid(pid: i32) -> Option<i32> {
    unsafe {
        let app: *mut AnyObject = msg_send![
            class!(NSRunningApplication),
            runningApplicationWithProcessIdentifier: pid
        ];
        if app.is_null() {
            None
        } else {
            Some(pid)
        }
    }
}

fn bundle_for_pid(pid: i32) -> Option<String> {
    unsafe { crate::app_identity::bundle_id_for_pid(pid) }
}

fn running_pid_for_bundle(bundle: &str) -> Option<i32> {
    unsafe {
        let ns_bundle = make_nsstring(bundle);
        let apps: *mut AnyObject = msg_send![
            class!(NSRunningApplication),
            runningApplicationsWithBundleIdentifier: ns_bundle
        ];
        crate::ffi::release_obj(ns_bundle);
        if apps.is_null() {
            return None;
        }
        let count: usize = msg_send![apps, count];
        if count == 0 {
            return None;
        }
        let app: *mut AnyObject = msg_send![apps, objectAtIndex: 0usize];
        if app.is_null() {
            None
        } else {
            Some(msg_send![app, processIdentifier])
        }
    }
}

fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Whether the Dock preview's own panel is visible, for the smoke runner.
pub(crate) fn panel_is_visible() -> bool {
    panel::is_visible()
}

/// The card count in the visible panel (smoke assertions).
pub(crate) fn visible_card_count() -> usize {
    panel::visible_card_count()
}

/// The panel's screen rectangle in top-left coordinates (smoke assertions).
pub(crate) fn panel_rect() -> Option<Rect> {
    panel::panel_frame()
}

/// Presses that dismissed a preview, for the smoke assertion.
pub(crate) fn press_dismissals() -> u64 {
    PRESS_DISMISSALS.load(Ordering::Relaxed)
}

/// Whether the panel's cards are all with a frame or an icon placeholder (never a blank card).
pub(crate) fn cards_are_renderable() -> bool {
    panel::cards_are_renderable()
}

// ---------------------------------------------------------------------------------------------
// M2 smoke runner (child process; reports its own verdict)
// ---------------------------------------------------------------------------------------------

/// Drive the real panel for a controlled window of this process and assert the facts the panel
/// owns. Runs inside the smoke child (a real NSApplication on the main thread). Returns the number
/// of checks that held.
pub(crate) enum SmokeFailure {
    /// The machine cannot run the check (for example no Screen Recording); not a product defect.
    NotRun(String),
    /// A product assertion failed.
    Failed(String),
}

/// The switch value the smoke found, restored on cleanup so the run cannot leave the in-memory
/// configuration flipped for whatever follows it.
static SMOKE_ORIGINAL_DOCK_ENABLED: Mutex<Option<bool>> = Mutex::new(None);
/// The threshold the smoke found, restored on cleanup on the same rule.
static SMOKE_ORIGINAL_THRESHOLD: Mutex<Option<u32>> = Mutex::new(None);

/// Drive the real panel for this process and assert the facts it owns. Runs inside the smoke child
/// (a real NSApplication on the main thread). Returns the number of checks that held.
pub(crate) fn smoke_runner() -> Result<u32, SmokeFailure> {
    crate::assert_main_thread("dock_preview::smoke_runner");
    // A headless session has no display to place the panel on; that is an environment limit, not
    // a product failure. Ask the display server rather than this module's synthetic bounds helper.
    if active_display_count() == 0 {
        return Err(SmokeFailure::NotRun(
            "no active display to place the preview panel on".into(),
        ));
    }
    let long_title = "Dock preview smoke title that is deliberately far wider than one card header";
    // Establish a real false -> true (and later true -> false) transition for this process: the
    // `Settings` source only acts on a changed value, so the in-memory configuration has to be the
    // one the runtime entry reads. The original value is restored on cleanup.
    //
    // The threshold is pinned for the run as well: a two-window row case is a row only while the
    // threshold is at least 2, and a user whose configuration says 1 must not turn it into a list.
    let (original, original_threshold) = crate::config::CONFIG
        .read()
        .map(|config| {
            (
                config.dock_preview.enabled,
                config.dock_preview.cards_per_row,
            )
        })
        .unwrap_or((true, crate::config::DOCK_CARDS_PER_ROW_DEFAULT));
    *SMOKE_ORIGINAL_DOCK_ENABLED.lock().unwrap() = Some(original);
    *SMOKE_ORIGINAL_THRESHOLD.lock().unwrap() = Some(original_threshold);
    if let Ok(mut config) = crate::config::CONFIG.write() {
        config.dock_preview.enabled = false;
        config.dock_preview.cards_per_row = crate::config::DOCK_CARDS_PER_ROW_DEFAULT;
    }
    // The switch is exercised through the runtime entry the settings row reaches, not by calling
    // the module directly: the same flags, the same startup/stop path, the same UI refresh.
    set_switch_through_runtime(true, crate::runtime_config::ConfigChangeSource::Startup)?;
    if let Ok(mut config) = crate::config::CONFIG.write() {
        config.dock_preview.enabled = true;
    }
    if !is_enabled() {
        return Err(SmokeFailure::Failed(
            "the Dock preview observer did not start".into(),
        ));
    }
    let window = unsafe { control_window("Dock preview smoke target") };
    if window.is_null() {
        return Err(SmokeFailure::Failed(
            "could not create the controlled window".into(),
        ));
    }
    if !dev_show_synthetic(&[long_title, "Second window"]) {
        return Err(SmokeFailure::Failed(
            "the panel did not appear for the synthetic window list".into(),
        ));
    }
    pump(Duration::from_millis(120));
    let mut checks: u32 = 0;
    if !panel_is_visible() {
        return Err(SmokeFailure::Failed(
            "the panel reported invisible right after it appeared".into(),
        ));
    }
    checks += 1;
    if visible_card_count() != 2 {
        return Err(SmokeFailure::Failed(
            "the panel did not show one card per window".into(),
        ));
    }
    checks += 1;
    let Some(rect) = panel_rect() else {
        return Err(SmokeFailure::Failed(
            "the panel has no screen rectangle".into(),
        ));
    };
    let screen = primary_screen_bounds_cg();
    if rect.w <= 0.0
        || rect.h <= 0.0
        || rect.x < screen.x - 1.0
        || rect.y < screen.y - 1.0
        || rect.max_x() > screen.max_x() + 1.0
        || rect.max_y() > screen.max_y() + 1.0
    {
        return Err(SmokeFailure::Failed(format!(
            "the panel is outside the screen: {rect:?} in {screen:?}"
        )));
    }
    checks += 1;
    if !cards_are_renderable() {
        return Err(SmokeFailure::Failed(
            "a card is neither a frame nor a placeholder".into(),
        ));
    }
    checks += 1;
    // The title tooltip: a truncated title, the pointer resting on its own box, then the pointer
    // leaving it. This is the real tooltip panel and the real decision path, driven by a position
    // the way the tick reads one.
    if !panel::title_is_truncated(0) {
        return Err(SmokeFailure::Failed(
            "the long title was not detected as truncated; the tooltip check cannot decide".into(),
        ));
    }
    let Some(title_rect) = panel::title_rect_for_smoke(0) else {
        return Err(SmokeFailure::Failed(
            "the title box has no screen rectangle".into(),
        ));
    };
    let title_center = (
        title_rect.x + title_rect.w / 2.0,
        title_rect.y + title_rect.h / 2.0,
    );
    let t0 = Instant::now();
    let _ = panel::tick(t0, title_center);
    let _ = panel::tick(
        t0 + crate::dock_preview::decisions::TOOLTIP_DELAY + Duration::from_millis(40),
        title_center,
    );
    panel::flush_records();
    let (tooltip_visible, tooltip_text) = panel::tooltip_state();
    if !tooltip_visible || tooltip_text != long_title {
        return Err(SmokeFailure::Failed(
            "the full-title tooltip did not appear over a truncated title".into(),
        ));
    }
    checks += 1;
    let _ = panel::tick(
        t0 + Duration::from_secs(2),
        (screen.x + 5.0, screen.y + 5.0),
    );
    panel::flush_records();
    if panel::tooltip_state().0 {
        return Err(SmokeFailure::Failed(
            "the tooltip stayed visible after the pointer left the title".into(),
        ));
    }
    checks += 1;
    // A press on the Dock dismisses the preview on the press itself. The check drives the entry the
    // Dock tap calls, so the panel's teardown is asserted without a real Dock click; the pump stays
    // well inside the pointer-left close grace, so only the press can explain a hidden panel.
    if !dev_show_synthetic(&[long_title, "Second window"]) {
        return Err(SmokeFailure::Failed(
            "the panel did not reappear for the Dock-press check".into(),
        ));
    }
    pump(Duration::from_millis(60));
    if !panel_is_visible() {
        return Err(SmokeFailure::Failed(
            "the panel was already hidden before the Dock-press check".into(),
        ));
    }
    let dismissals = press_dismissals();
    let session = SESSION.load(Ordering::Acquire);
    if session == 0 {
        return Err(SmokeFailure::Failed(
            "the smoke has no observer session to report a press for".into(),
        ));
    }
    note_dock_click((screen.x + screen.w / 2.0, screen.max_y() - 30.0), session);
    pump(Duration::from_millis(60));
    if panel_is_visible() {
        return Err(SmokeFailure::Failed(
            "the panel stayed open after a press on the Dock".into(),
        ));
    }
    if press_dismissals() != dismissals + 1 {
        return Err(SmokeFailure::Failed(
            "the press did not take the dismissal path".into(),
        ));
    }
    checks += 1;
    // The list and the threshold run BEFORE the disable below: turning the feature off leaves it
    // off for the rest of the process, and every later case has to re-enable it to show anything.
    checks += smoke_list_mode()?;
    checks += smoke_threshold_change()?;
    // Turning the feature off through the same runtime entry must tear both panels down.
    if !dev_show_synthetic(&[long_title, "Second window"]) {
        return Err(SmokeFailure::Failed(
            "the panel did not reappear for the disable check".into(),
        ));
    }
    pump(Duration::from_millis(120));
    // Put the tooltip back on screen before the disable check: the earlier assertion took it down
    // by moving the pointer away, so a teardown that forgot the tooltip would go unnoticed.
    let _ = panel::tick(t0 + Duration::from_secs(3), title_center);
    let _ = panel::tick(
        t0 + Duration::from_secs(3) + Duration::from_millis(100),
        title_center,
    );
    panel::flush_records();
    if !panel::tooltip_window_visible() {
        return Err(SmokeFailure::Failed(
            "the tooltip could not be shown for the disable check".into(),
        ));
    }
    checks += 1;
    // Disable through the Settings source: the change flag must be what starts the teardown, not
    // the unconditional Startup seed path.
    set_switch_through_runtime(false, crate::runtime_config::ConfigChangeSource::Settings)?;
    pump(Duration::from_millis(200));
    if panel_is_visible() || panel::tooltip_window_visible() {
        return Err(SmokeFailure::Failed(
            "the panel or its tooltip stayed open after the feature was turned off".into(),
        ));
    }
    checks += 1;
    unsafe {
        let _: () = msg_send![window, orderOut: std::ptr::null::<AnyObject>()];
        crate::ffi::release_obj(window);
    }
    Ok(checks)
}

/// The eight titles the list case opens with (distinct, so a row can be told from its neighbour).
const LIST_ROW_TITLES: [&str; 8] = [
    "list row one",
    "list row two",
    "list row three",
    "list row four",
    "list row five",
    "list row six",
    "list row seven",
    "list row eight",
];

/// Construct a scrolling case and assert the evidence for it.
///
/// The overflow is CONSTRUCTED, not hoped for: the controlled room is switched on here (and only
/// here, so the ordinary smoke previews keep the screen's own room), `max_scroll > 0` is asserted
/// before any behaviour is read from it, and the case is a six-window row at the minimum threshold
/// as well as a list.
fn smoke_list_mode() -> Result<u32, SmokeFailure> {
    if crate::dev_flags::value("smoke-dock-room-h").is_none()
        || crate::dev_flags::value("smoke-dock-room-w").is_none()
        || crate::dev_flags::value("smoke-dock-row-room-h").is_none()
    {
        return Err(SmokeFailure::NotRun(
            "--smoke-dock-room-h/-w and --smoke-dock-row-room-h were not all given, so at least one \
             case has no controlled overflow"
                .into(),
        ));
    }
    let mut checks = 0;
    SMOKE_ROOM_H.store(true, Ordering::Release);
    // Each case states its own budget: a short, wide room for the list, a narrow one for the card
    // grid. Measuring both from the display made the list fit on an ordinary screen and the cards
    // fit on a wide one.
    set_case_room("smoke-dock-room-h", "smoke-dock-room-w");
    checks += smoke_list_scroll()?;
    set_case_room("smoke-dock-row-room-h", "smoke-dock-room-w");
    checks += smoke_wrapped_rows()?;
    SMOKE_ROOM_H.store(false, Ordering::Release);
    hide_panel();
    Ok(checks)
}

/// The room the NEXT development case is opened with.
///
/// `dev_show_windows` reads these two statics, so one runner can drive cases with different budgets
/// without re-launching: its argv is the process's only configuration channel and cannot change
/// mid-run.
fn set_case_room(height_flag: &str, width_flag: &str) {
    let parse = |flag: &str| {
        crate::dev_flags::value(flag)
            .and_then(|value| value.trim().parse::<f64>().ok())
            .filter(|room| room.is_finite() && *room > 0.0)
    };
    *CASE_ROOM_H.lock().unwrap() = parse(height_flag);
    *CASE_ROOM_W.lock().unwrap() = parse(width_flag);
}

/// The list layout: eight windows, a two-row viewport, and the three facts that make the layout
/// work -- a real overflow, AppKit's own clipping, and a scroll that moves only the content view.
fn smoke_list_scroll() -> Result<u32, SmokeFailure> {
    let titles: Vec<&str> = (1..=8).map(|index| LIST_ROW_TITLES[index - 1]).collect();
    if !dev_show_windows(&titles, DockEdge::Bottom) {
        return Err(SmokeFailure::Failed(
            "the panel did not open for the list check".into(),
        ));
    }
    pump(Duration::from_millis(120));
    let mut checks = 0;
    let facts = panel::list_facts()
        .ok_or_else(|| SmokeFailure::Failed("the open panel published no layout facts".into()))?;
    if facts.mode != layout::PanelMode::List {
        return Err(SmokeFailure::Failed(format!(
            "eight windows with a threshold of {} opened as {:?}, not a list",
            crate::dock_preview::cards_per_row(),
            facts.mode
        )));
    }
    checks += 1;
    // The proof that the case is a real overflow, before any behaviour is read from it, and that
    // the budget produced the viewport it promised: exactly two rows (a third would be the padding
    // allowance leaking back in, and would still "pass" a bare max_scroll check).
    if facts.max_scroll <= 0.0 || facts.full_rows != 2 {
        return Err(SmokeFailure::Failed(format!(
            "the controlled room produced max_scroll={} and full_rows={}, expected 2 complete rows \
             over a scrollable list",
            facts.max_scroll, facts.full_rows
        )));
    }
    // `full_rows` rounds down, so it hides a viewport a few points taller than two rows (which
    // would show a sliver of the third). Assert the height itself.
    let expected_viewport = 2.0 * crate::dock_preview::layout::ROW_H;
    let viewport_h = facts.viewport.h;
    if (viewport_h - expected_viewport).abs() > 0.5 {
        return Err(SmokeFailure::Failed(format!(
            "the controlled room produced a {viewport_h}pt viewport, expected exactly {expected_viewport}pt"
        )));
    }
    checks += 1;
    let (row_frame, viewport_frame, clipped) =
        panel::clip_evidence(0).map_err(SmokeFailure::Failed)?;
    let close_enough = |a: Rect, b: Rect| {
        (a.x - b.x).abs() <= 0.5
            && (a.y - b.y).abs() <= 0.5
            && (a.w - b.w).abs() <= 0.5
            && (a.h - b.h).abs() <= 0.5
    };
    if !close_enough(row_frame, facts.item_screen[0])
        || !close_enough(viewport_frame, facts.viewport)
    {
        return Err(SmokeFailure::Failed(format!(
            "AppKit reports row {row_frame:?} / viewport {viewport_frame:?}, the panel published {:?} / {:?}",
            facts.item_screen[0], facts.viewport
        )));
    }
    if !clipped {
        return Err(SmokeFailure::Failed(
            "the row's clipping ancestor is not masksToBounds".into(),
        ));
    }
    checks += 1;
    let before = facts.scroll;
    panel::scroll_by(layout::Axis::Y, layout::ROW_H);
    panel::flush_records();
    let after = panel::list_facts()
        .ok_or_else(|| SmokeFailure::Failed("the panel facts vanished after a scroll".into()))?;
    if after.scroll <= before {
        return Err(SmokeFailure::Failed(format!(
            "a downward step did not scroll the list ({before} -> {})",
            after.scroll
        )));
    }
    checks += 1;
    panel::scroll_by(layout::Axis::Y, 10_000.0);
    panel::flush_records();
    let end = panel::list_facts()
        .ok_or_else(|| SmokeFailure::Failed("the panel facts vanished at the end".into()))?;
    if (end.scroll - end.max_scroll).abs() > 0.5 {
        return Err(SmokeFailure::Failed(format!(
            "the scroll is not clamped to the maximum ({} vs {})",
            end.scroll, end.max_scroll
        )));
    }
    let last = *end
        .item_screen
        .last()
        .ok_or_else(|| SmokeFailure::Failed("the list has no rows".into()))?;
    if !last.overlaps(end.viewport) {
        return Err(SmokeFailure::Failed(format!(
            "the last row {last:?} is outside the viewport {:?} at the end",
            end.viewport
        )));
    }
    checks += 1;
    hide_panel();
    Ok(checks)
}

/// The wrapped case: cards that do not fit one row wrap onto more rows, and a stack taller than the
/// room under the Dock scrolls downwards.
///
/// This replaced the old "one row scrolls sideways" case. With wrapping, a narrow room makes the row
/// wrap first; the sideways scroll is only reachable when a single card cannot fit the width at all.
fn smoke_wrapped_rows() -> Result<u32, SmokeFailure> {
    let original = crate::config::CONFIG.read().unwrap().clone();
    let mut roomy = original.clone();
    roomy.dock_preview.cards_per_row = crate::config::DOCK_CARDS_PER_ROW_MAX;
    let _ = crate::runtime_config::smoke_apply_threshold(&original, &roomy);
    let titles = [
        "wide one",
        "wide two",
        "wide three",
        "wide four",
        "wide five",
        "wide six",
    ];
    let restore = |original: &crate::config::Config| {
        let current = crate::config::CONFIG.read().unwrap().clone();
        let _ = crate::runtime_config::smoke_apply_threshold(&current, original);
    };
    if !dev_show_windows(&titles, DockEdge::Bottom) {
        restore(&original);
        return Err(SmokeFailure::Failed(format!(
            "the panel did not open for the wrapped check (room {}pt)",
            crate::dev_flags::value("smoke-dock-row-room-h").unwrap_or_default()
        )));
    }
    pump(Duration::from_millis(80));
    let facts = panel::list_facts()
        .ok_or_else(|| SmokeFailure::Failed("the card panel published no facts".into()))?;
    let screen = primary_screen_bounds_cg();
    let mut checks = 0;
    if facts.mode != layout::PanelMode::Row {
        return Err(SmokeFailure::Failed(
            "six windows at the maximum threshold should be cards, not a list".into(),
        ));
    }
    // The whole point of the case: the room is narrower than six cards, so they MUST have wrapped.
    if facts.card_rows < 2 {
        return Err(SmokeFailure::Failed(format!(
            "six cards in the controlled room opened as {} row(s), so the case proves nothing",
            facts.card_rows
        )));
    }
    if facts.columns == 0 || facts.columns >= titles.len() {
        return Err(SmokeFailure::Failed(format!(
            "the wrapped grid reports {} columns for {} cards",
            facts.columns,
            titles.len()
        )));
    }
    // The panel never grows past the screen, however many rows the cards need.
    if facts.viewport.x < screen.x - 1.0 || facts.viewport.max_x() > screen.max_x() + 1.0 {
        return Err(SmokeFailure::Failed(format!(
            "the grid's viewport {:?} leaves the screen {screen:?}",
            facts.viewport
        )));
    }
    checks += 1;
    // The controlled room is shorter than the wrapped stack, so the grid MUST scroll; a case that
    // does not overflow would only prove that nothing happened.
    if facts.max_scroll <= 0.0 {
        return Err(SmokeFailure::Failed(
            "the wrapped grid fits the controlled room without scrolling, so the case proves nothing"
                .into(),
        ));
    }
    {
        let before = facts.scroll;
        panel::scroll_by(layout::Axis::Y, layout::CARD_H);
        panel::flush_records();
        let after = panel::list_facts()
            .ok_or_else(|| SmokeFailure::Failed("the grid lost its facts".into()))?;
        if after.scroll <= before {
            return Err(SmokeFailure::Failed(
                "a vertical step did not scroll the wrapped grid".into(),
            ));
        }
        // Past the end: clamped, and the last row becomes reachable.
        panel::scroll_by(layout::Axis::Y, 10_000.0);
        panel::flush_records();
        let end = panel::list_facts()
            .ok_or_else(|| SmokeFailure::Failed("the grid lost its facts".into()))?;
        if (end.scroll - end.max_scroll).abs() > 0.5 {
            return Err(SmokeFailure::Failed(
                "the vertical scroll is not clamped".into(),
            ));
        }
        let last = *end
            .item_screen
            .last()
            .ok_or_else(|| SmokeFailure::Failed("the grid has no cards".into()))?;
        if !last.overlaps(end.viewport) {
            return Err(SmokeFailure::Failed(
                "the last card is unreachable at the end of the grid".into(),
            ));
        }
    }
    checks += 1;
    hide_panel();
    let current = crate::config::CONFIG.read().unwrap().clone();
    let _ = crate::runtime_config::smoke_apply_threshold(&current, &original);
    Ok(checks)
}

/// A threshold change while the panel is on screen: the real runtime entry hides the panel, and
/// the same windows re-open with the other layout.
fn smoke_threshold_change() -> Result<u32, SmokeFailure> {
    hide_panel();
    if !dev_show_synthetic(&["threshold one", "threshold two"]) {
        return Err(SmokeFailure::Failed(
            "the panel did not open for the threshold check".into(),
        ));
    }
    pump(Duration::from_millis(80));
    let row_case = panel::list_facts()
        .ok_or_else(|| SmokeFailure::Failed("the row panel published no facts".into()))?;
    if row_case.mode != layout::PanelMode::Row {
        return Err(SmokeFailure::Failed(
            "two windows did not open as a row".into(),
        ));
    }
    let original = crate::config::CONFIG.read().unwrap().clone();
    let mut changed = original.clone();
    changed.dock_preview.cards_per_row = crate::config::DOCK_CARDS_PER_ROW_MIN;
    let mut checks = 0;
    // The capture epoch has to advance: a frame still in flight for the old layout must not land
    // in a panel built for the new one.
    let epoch_before = crate::thumbnail::current_dock_capture_epoch();
    let hidden = crate::runtime_config::smoke_apply_threshold(&original, &changed);
    pump(Duration::from_millis(120));
    let epoch_after = crate::thumbnail::current_dock_capture_epoch();
    if epoch_after <= epoch_before {
        return Err(SmokeFailure::Failed(
            "the panel was hidden but the capture epoch did not advance".into(),
        ));
    }
    checks += 1;
    if !hidden || panel_is_visible() {
        return Err(SmokeFailure::Failed(
            "the panel stayed open after the threshold changed under it".into(),
        ));
    }
    checks += 1;
    // The same two windows now need the list: the threshold is re-read when the panel opens.
    if !dev_show_synthetic(&["threshold one", "threshold two"]) {
        return Err(SmokeFailure::Failed(
            "the panel did not reopen after the threshold change".into(),
        ));
    }
    pump(Duration::from_millis(80));
    let reopened = panel::list_facts()
        .ok_or_else(|| SmokeFailure::Failed("the reopened panel published no facts".into()))?;
    if reopened.mode != layout::PanelMode::List {
        return Err(SmokeFailure::Failed(format!(
            "with a threshold of {} two windows should open as a list, got {:?}",
            crate::config::DOCK_CARDS_PER_ROW_MIN,
            reopened.mode
        )));
    }
    checks += 1;
    // Restore what the run found, so a failure cannot leave the threshold at its minimum.
    let current = crate::config::CONFIG.read().unwrap().clone();
    let _ = crate::runtime_config::smoke_apply_threshold(&current, &original);
    hide_panel();
    Ok(checks)
}

/// Flip the switch through `runtime_config::apply_config_change`, the path the settings dispatch
/// uses, so the smoke covers the real enable/disable wiring rather than the module's own toggle.
fn set_switch_through_runtime(
    enabled: bool,
    source: crate::runtime_config::ConfigChangeSource,
) -> Result<(), SmokeFailure> {
    let old = match crate::config::CONFIG.read() {
        Ok(config) => config.clone(),
        Err(_) => {
            return Err(SmokeFailure::Failed(
                "the configuration could not be read for the runtime switch".into(),
            ))
        }
    };
    let mut new = old.clone();
    new.dock_preview.enabled = enabled;
    // `Startup` is the source that makes the runtime entry seed the switch from the config
    // snapshot; the settings page uses `Settings` and the same branch. The smoke child runs before
    // the app's own startup apply, so this is also what starts the observer for the first check.
    crate::runtime_config::apply_config_change(&old, &new, source);
    Ok(())
}

/// Leave the feature off again after the smoke, so a failed run cannot leave the switch on.
pub(crate) fn smoke_cleanup() {
    let _ = set_switch_through_runtime(false, crate::runtime_config::ConfigChangeSource::Settings);
    if let Some(original) = SMOKE_ORIGINAL_DOCK_ENABLED.lock().unwrap().take() {
        if let Ok(mut config) = crate::config::CONFIG.write() {
            config.dock_preview.enabled = original;
        }
    }
    if let Some(threshold) = SMOKE_ORIGINAL_THRESHOLD.lock().unwrap().take() {
        if let Ok(mut config) = crate::config::CONFIG.write() {
            config.dock_preview.cards_per_row = threshold;
        }
    }
}

unsafe fn control_window(title: &str) -> *mut AnyObject {
    let panel: *mut AnyObject = msg_send![class!(NSWindow), alloc];
    let panel: *mut AnyObject = msg_send![
        panel,
        initWithContentRect: objc2_foundation::NSRect::new(
            objc2_foundation::NSPoint::new(240.0, 240.0),
            objc2_foundation::NSSize::new(420.0, 320.0)
        ),
        styleMask: 3u64, // titled | closable
        backing: 2u64,
        defer: false
    ];
    if panel.is_null() {
        return panel;
    }
    let _: () = msg_send![panel, setReleasedWhenClosed: false];
    let ns_title = make_nsstring(title);
    let _: () = msg_send![panel, setTitle: ns_title];
    crate::ffi::release_obj(ns_title);
    let _: () = msg_send![panel, orderFrontRegardless];
    panel
}

fn pump(duration: Duration) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        unsafe { crate::ffi::CFRunLoopRunInMode(crate::event_tap::kCFRunLoopDefaultMode, 0.02, 1) };
    }
}

/// The real pointer in top-left screen coordinates. `NSEvent.mouseLocation` is AppKit's live
/// pointer (bottom-left global); the hardware-only CGEvent source can lag a synthetic move.
/// The number of active displays, from the display server (0 in a truly headless session).
fn active_display_count() -> u32 {
    let mut count: u32 = 0;
    let status = unsafe { crate::ffi::CGGetActiveDisplayList(0, std::ptr::null_mut(), &mut count) };
    if status == 0 {
        count
    } else {
        0
    }
}

pub(crate) fn pointer_in_cg() -> Option<(f64, f64)> {
    unsafe {
        let point: objc2_foundation::NSPoint = msg_send![class!(NSEvent), mouseLocation];
        let height = primary_screen_bounds_cg().h;
        if height <= 0.0 {
            return None;
        }
        Some((point.x, height - point.y))
    }
}

fn primary_screen_bounds_cg() -> Rect {
    unsafe {
        let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
        if screens.is_null() {
            return Rect::new(0.0, 0.0, 1470.0, 956.0);
        }
        let count: usize = msg_send![screens, count];
        if count == 0 {
            return Rect::new(0.0, 0.0, 1470.0, 956.0);
        }
        let screen: *mut AnyObject = msg_send![screens, objectAtIndex: 0usize];
        let frame: objc2_foundation::NSRect = msg_send![screen, frame];
        Rect::new(
            frame.origin.x,
            frame.origin.y,
            frame.size.width,
            frame.size.height,
        )
    }
}

#[cfg(test)]
mod smoke_tests {
    use crate::smoke_verdict::smoke_runner;

    /// Run the real Dock preview panel in a child process and assert what it owns.
    // SMOKE_CELL=M2
    // SMOKE_DISTURBS=focus
    #[test]
    #[ignore]
    fn dock_preview_smoke() {
        smoke_runner!("dock_preview_smoke", 1, {
            // Both budgets are CONSTRUCTED, so neither case depends on the display. The list needs a
            // two-row viewport (`2*ROW_H` plus the glass padding plus the layout margin), reached by
            // scrolling. The card case needs a width under six cards at their floor width
            // (`4*CARD_MIN_W`) so they wrap, and a room shorter than the wrapped stack so it scrolls.
            // Exactly a two-row viewport: the glass the layout makes is the budget, and only its
            // padding comes off the viewport, so the budget is `2*ROW_H + 2*PAD`.
            let room_h = format!(
                "--smoke-dock-room-h={}",
                2.0 * crate::dock_preview::layout::ROW_H
                    + 2.0 * crate::dock_preview::layout::PANEL_PAD
            );
            let room_w = format!(
                "--smoke-dock-room-w={}",
                4.0 * crate::dock_preview::layout::CARD_MIN_W
                    + crate::dock_preview::layout::PANEL_MARGIN
            );
            // Shorter than two wrapped card rows: the grid has to scroll to be complete.
            let row_h = format!(
                "--smoke-dock-row-room-h={}",
                crate::dock_preview::layout::CARD_H
                    + crate::dock_preview::layout::PANEL_PAD * 4.0
                    + crate::dock_preview::layout::PANEL_MARGIN
            );
            crate::smoke_verdict::run_smoke_child(
                "dock_preview_smoke",
                &["--smoke-dock-preview", &room_h, &room_w, &row_h],
            );
        });
    }
}
