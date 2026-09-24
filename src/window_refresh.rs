//! Window-snapshot refresh pipeline: request merging (summon wins), the background
//! collector thread, generation guarding, main-thread application (MRU merge,
//! selection anchoring, single-shot first frame), and WindowServer event consumption.
//! Split out of main.rs; AppState/TAB_STATE/CONTROLLER remain owned by main.rs and
//! are accessed here through crate:: paths.

use objc2::runtime::{AnyObject, Sel};
use objc2::{msg_send, sel};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};

/// Overlay presentation hooks: window_refresh decides only WHEN the card surface must
/// re-render, never WHO renders it (it used to call overlay::* directly, forming a module
/// cycle against the overlay -> window_refresh request direction). main() injects the
/// overlay implementations at startup, making the dependency one-way.
pub(crate) struct OverlayPresenter {
    /// Whether a card-close reflow transition is running (snapshots defer while it does).
    pub(crate) card_close_in_progress: fn() -> bool,
    /// First snapshot ready with Cmd already released: commit the pick without ever showing.
    pub(crate) commit_first_summon: fn(backward: bool),
    /// First snapshot ready: show the overlay once (single-shot render).
    pub(crate) show_first_summon: fn(backward: bool),
    /// Take the pending display-relayout flag (re-layout to the new screen geometry).
    pub(crate) take_display_relayout_pending: fn() -> bool,
    /// Reset the thumbnail visible range/scroll/navigation anchor (before a full rebuild).
    pub(crate) reset_thumbnail_state: fn(),
    pub(crate) show_overlay: fn(),
    pub(crate) refresh_highlight: fn(),
}

static OVERLAY_PRESENTER: OnceLock<OverlayPresenter> = OnceLock::new();

/// Register the overlay presentation implementations at startup; when unregistered (unit
/// tests/smoke runs) query hooks return neutral values and action hooks no-op.
pub(crate) fn register_overlay_presenter(presenter: OverlayPresenter) {
    let _ = OVERLAY_PRESENTER.set(presenter);
}

fn overlay_presenter() -> Option<&'static OverlayPresenter> {
    OVERLAY_PRESENTER.get()
}
use std::thread;
use std::time::Duration;

use crate::ffi::frontmost_app_info;
use crate::performance;
use crate::thumbnail;
use crate::window_collector::{
    bump_window_mru, collect_windows_for_pid, collect_windows_with_frontmost_bump,
    forget_non_normal_window, owner_pid_for_cgwid, sort_windows_by_mru, window_server_candidates,
    MruMap, WindowInfo,
};
use crate::window_server;
use crate::{log_debug, with_tab_state, AppState, CONTROLLER, WINDOW_COUNT};

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WindowRefreshReason {
    Lifecycle = 1,
    Summon = 2,
}

impl WindowRefreshReason {
    fn bumps_frontmost(self) -> bool {
        matches!(self, Self::Summon)
    }

    fn merge(self, other: Self) -> Self {
        if self.bumps_frontmost() || other.bumps_frontmost() {
            Self::Summon
        } else {
            Self::Lifecycle
        }
    }

    fn from_raw(value: u8) -> Option<Self> {
        match value {
            value if value == Self::Lifecycle as u8 => Some(Self::Lifecycle),
            value if value == Self::Summon as u8 => Some(Self::Summon),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WindowRefreshRequest {
    Full(WindowRefreshReason),
    Focused { pid: i32, window_id: u32 },
}

struct WindowRefreshResult {
    generation: u64,
    windows: Vec<WindowInfo>,
    mru: MruMap,
    replace_pid: Option<i32>,
    active_key: Option<(i32, u32)>,
    // Exact frontmost window resolved during a first-summon refresh.  This must be carried
    // alongside the snapshot so the first frame does not reuse a stale same-PID focus_key.
    summon_focus_key: Option<(i32, u32)>,
}

static WINDOW_REFRESH_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
// Keep the highest-priority pending reason so a summon refresh cannot be downgraded to a lifecycle refresh.
static WINDOW_REFRESH_PENDING: AtomicU8 = AtomicU8::new(0);
static WINDOW_REFRESH_PENDING_FOCUS: LazyLock<Mutex<Option<(i32, u32)>>> =
    LazyLock::new(|| Mutex::new(None));
static WINDOW_REFRESH_GENERATION: AtomicU64 = AtomicU64::new(0);
static WINDOW_REFRESH_RESULT: LazyLock<Mutex<Option<WindowRefreshResult>>> =
    LazyLock::new(|| Mutex::new(None));
static LIFECYCLE_BACKSTOP_STARTED: OnceLock<()> = OnceLock::new();
static DEFERRED_REFRESH_HANDOFF_TOKEN: AtomicU64 = AtomicU64::new(0);
static DEFERRED_REFRESH_HANDOFF_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
const DEFERRED_REFRESH_WATCHDOG_TIMEOUT: Duration = Duration::from_secs(1);

/// Hand-off watchdog for the in-flight flag: the flag is set by the request path and the
/// release duty is passed along in stages (requester -> worker -> main-thread apply -> the
/// deferred re-entry apply schedules during a card-close transition) via hand_off(); if any
/// stage panics or returns early before passing it on, Drop resets the flag at once. A panic
/// therefore costs one refresh instead of parking WINDOW_REFRESH_IN_FLIGHT at true and silently
/// disabling the whole refresh pipeline -- which is what produced the ghost card.
///
/// The panic vectors are not limited to one, and the guard treats them alike: the one measured
/// in practice was the debug main-thread assertion raised when the backstop entered the request
/// path from its own thread; a poisoned `lock().unwrap()` in the worker stage or a panic inside
/// plain Rust (collection, sorting) belongs to the same class.
struct InFlightGuard {
    armed: bool,
}

impl InFlightGuard {
    fn new() -> Self {
        Self { armed: true }
    }

    /// Give up this release: the duty moves to a later stage (the main-thread apply after the
    /// worker posts, or the re-entry apply schedules during a card-close transition). The flag
    /// stays set; if that stage panics in turn, its own guard covers it.
    fn hand_off(mut self) {
        self.armed = false;
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        if self.armed {
            WINDOW_REFRESH_IN_FLIGHT.store(false, Ordering::Release);
        }
    }
}

/// Request one bounded background window snapshot; only one task may run at a time,
/// preventing rapid shortcut presses from creating a thread storm.
pub(crate) fn request_window_refresh() {
    request_window_refresh_for(WindowRefreshReason::Summon);
}

/// Periodically reconcile the window/thumbnail set when private WindowServer lifecycle
/// notifications are unavailable or an event is dropped.
pub(crate) fn start_lifecycle_backstop() {
    if LIFECYCLE_BACKSTOP_STARTED.set(()).is_err() {
        return;
    }
    thread::Builder::new()
        .name("window-lifecycle-backstop".into())
        .spawn(|| loop {
            thread::sleep(Duration::from_secs(5));
            let controller = CONTROLLER.lock().unwrap().map(|ptr| ptr.0);
            if let Some(controller) = controller {
                unsafe {
                    let _: () = msg_send![controller,
                        performSelectorOnMainThread: sel!(handleLifecycleBackstop:),
                        withObject: std::ptr::null::<AnyObject>(),
                        waitUntilDone: false
                    ];
                }
            }
        })
        .expect("spawn window-lifecycle-backstop thread");
}

pub(crate) extern "C" fn on_lifecycle_backstop(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    crate::callback_guard::void("on_lifecycle_backstop", request_lifecycle_window_refresh);
}

fn schedule_deferred_refresh_watchdog(generation: u64) -> bool {
    let token = {
        let _handoff = DEFERRED_REFRESH_HANDOFF_LOCK.lock().unwrap();
        let token = DEFERRED_REFRESH_HANDOFF_TOKEN.fetch_add(1, Ordering::AcqRel) + 1;
        let controller = CONTROLLER.lock().unwrap().map(|ptr| ptr.0);
        let Some(controller) = controller else {
            return false;
        };
        unsafe {
            let _: () = msg_send![
                controller,
                performSelector: sel!(handleWindowRefresh:),
                withObject: std::ptr::null::<AnyObject>(),
                afterDelay: 0.08f64
            ];
        }
        token
    };

    if let Err(error) = thread::Builder::new()
        .name("window-refresh-watchdog".into())
        .spawn(move || {
            thread::sleep(DEFERRED_REFRESH_WATCHDOG_TIMEOUT);
            recover_stalled_deferred_refresh(generation, token);
        })
    {
        log_debug!(
            "[windows] deferred refresh watchdog unavailable generation={} error={}",
            generation,
            error
        );
        return false;
    }
    true
}

fn recover_stalled_deferred_refresh(generation: u64, token: u64) -> bool {
    let _handoff = DEFERRED_REFRESH_HANDOFF_LOCK.lock().unwrap();
    if DEFERRED_REFRESH_HANDOFF_TOKEN.load(Ordering::Acquire) != token
        || !WINDOW_REFRESH_IN_FLIGHT.load(Ordering::Acquire)
    {
        return false;
    }
    let removed = {
        let mut result = WINDOW_REFRESH_RESULT.lock().unwrap();
        if result
            .as_ref()
            .is_some_and(|pending| pending.generation == generation)
        {
            result.take().is_some()
        } else {
            false
        }
    };
    if removed {
        WINDOW_REFRESH_IN_FLIGHT.store(false, Ordering::Release);
        log_debug!(
            "[windows] deferred refresh watchdog recovered stalled result generation={}",
            generation
        );
    }
    removed
}

fn request_lifecycle_window_refresh() {
    request_window_refresh_for(WindowRefreshReason::Lifecycle);
}

fn request_focused_window_refresh(pid: i32, window_id: u32) {
    request_window_refresh_request(WindowRefreshRequest::Focused { pid, window_id });
}

fn merge_pending_refresh_reason(reason: WindowRefreshReason) {
    let mut current = WINDOW_REFRESH_PENDING.load(Ordering::Acquire);
    loop {
        let merged =
            WindowRefreshReason::from_raw(current).map_or(reason, |pending| pending.merge(reason));
        let requested = merged as u8;
        if current == requested {
            return;
        }
        match WINDOW_REFRESH_PENDING.compare_exchange(
            current,
            requested,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return,
            Err(next) => current = next,
        }
    }
}

fn take_pending_refresh_reason() -> Option<WindowRefreshReason> {
    WindowRefreshReason::from_raw(WINDOW_REFRESH_PENDING.swap(0, Ordering::AcqRel))
}

fn take_pending_refresh_request() -> Option<WindowRefreshRequest> {
    if let Some(reason) = take_pending_refresh_reason() {
        return Some(WindowRefreshRequest::Full(reason));
    }
    WINDOW_REFRESH_PENDING_FOCUS
        .lock()
        .unwrap()
        .take()
        .map(|(pid, window_id)| WindowRefreshRequest::Focused { pid, window_id })
}

fn queue_refresh_request(request: WindowRefreshRequest) {
    match request {
        WindowRefreshRequest::Full(reason) => merge_pending_refresh_reason(reason),
        WindowRefreshRequest::Focused { pid, window_id } => {
            *WINDOW_REFRESH_PENDING_FOCUS.lock().unwrap() = Some((pid, window_id));
        }
    }
}

fn request_window_refresh_for(reason: WindowRefreshReason) {
    request_window_refresh_request(WindowRefreshRequest::Full(reason));
}

fn request_window_refresh_request(request: WindowRefreshRequest) {
    if WINDOW_REFRESH_IN_FLIGHT.swap(true, Ordering::AcqRel) {
        queue_refresh_request(request);
        return;
    }

    // The flag is now set and owned by the guard, which resets it on any exit that never
    // reaches the main-thread apply stage.
    start_window_refresh(request, InFlightGuard::new());
}

/// Prefer the exact focus key from the current summon snapshot over a cached key.  Both keys
/// can belong to the same PID when an app opens or focuses another window (for example Edge's
/// normal and InPrivate windows), so comparing only the PID is insufficient.
fn select_summon_focus_key(
    fresh: Option<(i32, u32)>,
    cached: Option<(i32, u32)>,
    windows: &[WindowInfo],
) -> Option<(i32, u32)> {
    let is_present = |key: (i32, u32)| {
        windows
            .iter()
            .any(|window| (window.pid, window.window_id) == key)
    };
    fresh
        .filter(|key| is_present(*key))
        .or_else(|| cached.filter(|key| is_present(*key)))
}

fn start_window_refresh(request: WindowRefreshRequest, in_flight: InFlightGuard) {
    // Return without state: the guard resets the flag on Drop instead of leaving it stuck.
    let Some((generation, mru)) = with_tab_state(|state_opt| {
        let state = state_opt.as_ref()?;
        Some((
            WINDOW_REFRESH_GENERATION.fetch_add(1, Ordering::AcqRel) + 1,
            state.mru.clone(),
        ))
    }) else {
        return;
    };
    // Once the overlay is shown, summon-bump must NOT rewrite the frontmost window into MRU:
    // otherwise every refresh hoists it to index 0 and reorders the displayed list (the jump).
    // The not-yet-shown first frame (visible=false) still may bump once so the head is the real
    // frontmost.
    let allow_bump =
        with_tab_state(|state_opt| !state_opt.as_ref().is_some_and(|state| state.visible));

    thread::Builder::new()
        .name("window-refresh".into())
        .spawn(move || {
            performance::set_current_thread_qos(match request {
                WindowRefreshRequest::Full(WindowRefreshReason::Summon) => {
                    performance::ThreadQos::UserInitiated
                }
                WindowRefreshRequest::Full(WindowRefreshReason::Lifecycle)
                | WindowRefreshRequest::Focused { .. } => performance::ThreadQos::Utility,
            });
            let mut mru = mru;
            let (windows, replace_pid, active_key, summon_focus_key) = match request {
                WindowRefreshRequest::Full(reason) => {
                    let bump_frontmost = reason.bumps_frontmost() && allow_bump;
                    let windows = collect_windows_with_frontmost_bump(&mut mru, bump_frontmost);
                    // collect_windows_with_frontmost_bump marks the exact frontmost window as
                    // the first active item after sorting. Preserve that key for first-summon
                    // selection; lifecycle refreshes intentionally do not provide this value.
                    let summon_focus_key = if bump_frontmost {
                        windows
                            .iter()
                            .find(|window| window.is_active)
                            .map(|window| (window.pid, window.window_id))
                    } else {
                        None
                    };
                    (windows, None, None, summon_focus_key)
                }
                WindowRefreshRequest::Focused { pid, window_id } => {
                    match collect_windows_for_pid(&mut mru, pid, window_id) {
                        Some(windows) => {
                            let active_key = windows
                                .iter()
                                .any(|window| window.window_id == window_id)
                                .then_some((pid, window_id));
                            (windows, Some(pid), active_key, None)
                        }
                        None => {
                            // Fall back to a full snapshot when the directed AX query fails, so
                            // a transient timeout cannot erase the target app's cards.
                            log_debug!(
                                "[windows] focused refresh fallback: pid={} cgwid={}",
                                pid,
                                window_id
                            );
                            (
                                collect_windows_with_frontmost_bump(&mut mru, false),
                                None,
                                None,
                                None,
                            )
                        }
                    }
                }
            };
            *WINDOW_REFRESH_RESULT.lock().unwrap() = Some(WindowRefreshResult {
                generation,
                windows,
                mru,
                replace_pid,
                active_key,
                summon_focus_key,
            });
            let controller = CONTROLLER.lock().unwrap().map(|ptr| ptr.0);
            if let Some(controller) = controller {
                unsafe {
                    let _: () = msg_send![controller,
                        performSelectorOnMainThread: sel!(handleWindowRefresh:),
                        withObject: std::ptr::null::<AnyObject>(),
                        waitUntilDone: false
                    ];
                }
                // The result is handed to the main-thread apply stage, which releases the flag.
                in_flight.hand_off();
            } else {
                WINDOW_REFRESH_RESULT.lock().unwrap().take();
                // Nobody will consume the result: the guard resets the flag first, then the
                // queued request is re-issued.
                drop(in_flight);
                if let Some(pending_request) = take_pending_refresh_request() {
                    request_window_refresh_request(pending_request);
                }
            }
        })
        .expect("spawn window-refresh thread");
}

fn merge_refreshed_windows(
    existing: &[WindowInfo],
    replace_pid: Option<i32>,
    mut refreshed: Vec<WindowInfo>,
) -> Vec<WindowInfo> {
    // AX is authoritative: a window enters the list only if the current snapshot's AX confirms
    // it; never resurrect from the previous list. For tab-per-surface apps (e.g. Ghostty) AX
    // reports only the focused surface, so retaining the old list would re-inject an unfocused
    // tab surface and show one window as many cards.
    if let Some(pid) = replace_pid {
        let mut merged: Vec<WindowInfo> = existing
            .iter()
            .filter(|window| window.pid != pid)
            .cloned()
            .collect();
        merged.append(&mut refreshed);
        merged
    } else {
        refreshed
    }
}

/// Keep the currently visible card order while applying refreshed window data.
///
/// MRU sorting is useful when the overlay is summoned, but replacing the order of
/// `TAB_STATE.windows` while the existing card tree remains in place breaks the
/// card-index invariant. Rebase the refreshed records onto the current order so
/// pure refreshes update metadata without moving visible cards.
fn preserve_existing_window_order(
    existing: &[WindowInfo],
    refreshed: Vec<WindowInfo>,
) -> Vec<WindowInfo> {
    let refresh_order: Vec<(i32, u32)> = refreshed
        .iter()
        .map(|window| (window.pid, window.window_id))
        .collect();
    let mut by_key: HashMap<(i32, u32), WindowInfo> = refreshed
        .into_iter()
        .map(|window| ((window.pid, window.window_id), window))
        .collect();
    let mut ordered = Vec::with_capacity(existing.len() + by_key.len());
    for current in existing {
        let key = (current.pid, current.window_id);
        if let Some(window) = by_key.remove(&key) {
            ordered.push(window);
        }
    }
    // New windows are not part of a pure reorder, but appending them makes this
    // helper safe for callers that use it across a concurrent add/remove. Keep
    // the refreshed order deterministic instead of iterating the hash map.
    for key in refresh_order {
        if let Some(window) = by_key.remove(&key) {
            ordered.push(window);
        }
    }
    ordered
}

fn selection_index_after_refresh(
    selected_key: Option<(i32, u32)>,
    previous_index: usize,
    windows: &[WindowInfo],
) -> usize {
    selected_key
        .and_then(|key| {
            windows
                .iter()
                .position(|window| (window.pid, window.window_id) == key)
        })
        .unwrap_or_else(|| previous_index.min(windows.len().saturating_sub(1)))
}

/// Selection index after a refresh: prefer the target window key over the live list index.
/// - user_picked = true: pin to target_key; keep its position if still shown, else clamp.
/// - user_picked = false: lock to the summon-time target so a refresh doesn't re-pick merely
///   because the MRU order was bumped by the frontmost window (a reorder with no add/remove).
fn select_index_after_refresh(
    user_picked: bool,
    target_key: Option<(i32, u32)>,
    live_selected_key: Option<(i32, u32)>,
    previous_index: usize,
    windows: &[WindowInfo],
) -> usize {
    let anchor = if user_picked {
        target_key
    } else {
        target_key.or(live_selected_key)
    };
    selection_index_after_refresh(anchor, previous_index, windows)
}

/// Apply a snapshot on the main thread and merge window-level MRU updates made after
/// the background task started.
fn apply_window_refresh() {
    run_apply_with_in_flight_guard(apply_window_refresh_inner);
}

fn run_apply_with_in_flight_guard(apply: impl FnOnce(InFlightGuard)) {
    apply(InFlightGuard::new());
}

fn apply_window_refresh_inner(in_flight: InFlightGuard) {
    // The flag was set by the request path and handed off stage by stage to here; this guard
    // covers any panic in this stage. Without it, callback_guard would swallow the panic into a
    // single log line while the flag stayed true forever -- the exact mirror of the bug fixed
    // above.
    if overlay_presenter().is_some_and(|p| (p.card_close_in_progress)()) {
        // Keep the snapshot pending while close reflow runs, so a refresh cannot rebuild the view tree.
        let generation = WINDOW_REFRESH_RESULT
            .lock()
            .unwrap()
            .as_ref()
            .map(|result| result.generation);
        let deferred = generation.is_some_and(schedule_deferred_refresh_watchdog);
        if deferred {
            // The snapshot stays pending for the re-entry 0.08s later, which holds the flag and
            // guards itself in turn.
            in_flight.hand_off();
        }
        return;
    }
    let Some(result) = WINDOW_REFRESH_RESULT.lock().unwrap().take() else {
        // Reset the flag first, then re-issue the queued request (the order matters).
        // If a timed-out delayed callback arrives after a newer worker has claimed the flag,
        // preserve that newer hand-off instead of clearing its in-flight state.
        if WINDOW_REFRESH_IN_FLIGHT.load(Ordering::Acquire) {
            in_flight.hand_off();
        } else if let Some(pending_request) = take_pending_refresh_request() {
            drop(in_flight);
            request_window_refresh_request(pending_request);
        }
        return;
    };
    if result.generation != WINDOW_REFRESH_GENERATION.load(Ordering::Acquire) {
        drop(in_flight);
        if let Some(pending_request) = take_pending_refresh_request() {
            request_window_refresh_request(pending_request);
        }
        return;
    }

    let summon_focus_key = result.summon_focus_key;
    let subscriptions = window_server_candidates();

    let Some((was_visible, set_changed)) = with_tab_state(|state_opt| {
        // Nothing to apply into: the guard resets the flag when the else branch returns.
        let state = state_opt.as_mut()?;
        let selected_key = state
            .windows
            .get(state.selected)
            .map(|window| (window.pid, window.window_id));
        let mut mru = state.mru.clone();
        for (key, timestamp) in result.mru {
            if mru.get(&key).is_none_or(|current| *current < timestamp) {
                mru.insert(key, timestamp);
            }
        }
        let replace_pid = result.replace_pid;
        let active_key = result.active_key;
        // A directed refresh replaces only the target PID; cards and ordering memory for every
        // other application remain intact.
        let mut sorted_windows =
            merge_refreshed_windows(&state.windows, replace_pid, result.windows);
        sort_windows_by_mru(&mut sorted_windows, &mru, std::time::Instant::now());
        if replace_pid.is_none() {
            // Re-establish one frontmost representative after a full merge so an old snapshot
            // cannot leave multiple stale is_active flags behind.
            for window in &mut sorted_windows {
                window.is_active = false;
            }
            if let Some(first) = sorted_windows.first_mut() {
                first.is_active = true;
            }
        } else if let Some(active_key) = active_key {
            for window in &mut sorted_windows {
                window.is_active = (window.pid, window.window_id) == active_key;
            }
        }
        let set_changed = replace_pid.is_some() || {
            let old: HashSet<(i32, u32)> =
                state.windows.iter().map(|w| (w.pid, w.window_id)).collect();
            let new: HashSet<(i32, u32)> = sorted_windows
                .iter()
                .map(|w| (w.pid, w.window_id))
                .collect();
            old != new
        };
        let was_visible = state.visible;
        let windows = if was_visible && !set_changed {
            preserve_existing_window_order(&state.windows, sorted_windows)
        } else {
            sorted_windows
        };
        // Selection follows the target window, not the live list index:
        // - user_picked=true (user navigated): pin to selected_target_key; reorders cannot move it.
        // - user_picked=false (still the first-frame default): lock to the summon-time target so a
        //   refresh doesn't re-pick just because the MRU order was bumped by the frontmost window.
        let selected = select_index_after_refresh(
            state.user_picked,
            state.selected_target_key,
            selected_key,
            state.selected,
            &windows,
        );
        // Set-level change (windows added/removed): only this needs a full overlay rebuild. A pure
        // reorder (MRU bumped by the frontmost, surface flip) only updates data without rebuilding
        // the list — otherwise the overlay "keeps jumping after it opened". A directed refresh
        // (replace_pid) swapping a PID's cards also counts as a set change.
        state.windows = windows;
        WINDOW_COUNT.store(state.windows.len(), std::sync::atomic::Ordering::Release);
        state.selected = selected;
        state.mru = mru;
        if state.windows.is_empty() {
            state.visible = false;
        }
        Some((was_visible, set_changed))
    }) else {
        return;
    };
    // The snapshot is applied, so the flag can go: a panic in the rebuild/subscription steps
    // below can no longer wedge the pipeline.
    drop(in_flight);
    let pending_request = take_pending_refresh_request();

    // First snapshot ready: consume pending_first_show and show once (single-shot render). The
    // list is already the final post-refresh order, so the "stale then reorder" jump is gone.
    let first_show_request = with_tab_state(|state_opt| {
        if let Some(state) = state_opt.as_mut() {
            if state.pending_first_show {
                state.pending_first_show = false;
                let backward = state.pending_first_backward;
                let release_pending = state.pending_first_release;
                state.pending_first_release = false;
                Some((backward, release_pending))
            } else {
                None
            }
        } else {
            None
        }
    });
    if let Some((backward, release_pending)) = first_show_request {
        // The snapshot already resolved the current window exactly. Update the persistent focus
        // key before prepare_first_summon_state runs; otherwise a stale sibling from the same
        // app (for example Edge's previous normal window) wins over the new focused window.
        with_tab_state(|state_opt| {
            if let Some(state) = state_opt.as_mut() {
                if let Some(focus_key) =
                    select_summon_focus_key(summon_focus_key, state.focus_key, &state.windows)
                {
                    state.focus_key = Some(focus_key);
                }
            }
        });
        if release_pending {
            // Cmd was released while the first snapshot was pending. Commit the selected target
            // without ever displaying the panel; on_cmd_released cannot do this itself because
            // the panel is intentionally still invisible until the snapshot is ready.
            if let Some(presenter) = overlay_presenter() {
                (presenter.commit_first_summon)(backward);
            }
            log_debug!(
                "[overlay] summon e2e: first snapshot committed after pending release (backward={})",
                backward
            );
        } else {
            if let Some(presenter) = overlay_presenter() {
                (presenter.show_first_summon)(backward);
            }
            log_debug!(
                "[overlay] summon e2e: first snapshot shown (backward={})",
                backward
            );
        }
    }

    // Post-display-reconfiguration relayout: the window set is unchanged
    // (set_changed=false) yet screen geometry and window bounds moved; a visible
    // overlay must still re-layout to the new aspects. Scroll position is kept
    // (show_overlay clamps it into the new range) and, with the set unchanged,
    // card indices do not drift, so navigation anchors need no reset.
    let relayout_for_display_change =
        was_visible && overlay_presenter().is_some_and(|p| (p.take_display_relayout_pending)());
    if relayout_for_display_change {
        log_debug!(
            "[display] post-reconfiguration relayout applying refreshed bounds (set_changed={})",
            set_changed
        );
    }
    if set_changed && was_visible {
        if let Some(presenter) = overlay_presenter() {
            (presenter.reset_thumbnail_state)();
        }
    }
    if was_visible && (set_changed || relayout_for_display_change) {
        if let Some(presenter) = overlay_presenter() {
            (presenter.show_overlay)();
            (presenter.refresh_highlight)();
        }
    }
    // WindowServer observes every CG candidate instead of only AX-confirmed display cards.
    window_server::update_subscriptions(&subscriptions);
    if let Some(request) = pending_request {
        request_window_refresh_request(request);
    }
}

pub(crate) extern "C" fn on_window_refresh(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    crate::callback_guard::void("on_window_refresh", apply_window_refresh);
}

pub(crate) extern "C" fn on_window_server_event(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    crate::callback_guard::void("on_window_server_event", on_window_server_event_inner);
}

fn on_window_server_event_inner() {
    let events = window_server::drain_main();
    if events.is_empty() {
        return;
    }
    // Process every destruction before the short-circuiting refresh check: a Created
    // event earlier in this batch must not prevent a later window's cache cleanup.
    for event in &events {
        if let window_server::WindowServerEvent::Destroyed(window_id) = event {
            // Read the owner from the current subscription index first; CG lookup usually fails
            // after destruction, so never guess a PID for cleanup.
            if let Some(pid) = window_server::owner_for_destroyed_window(*window_id) {
                thumbnail::forget_destroyed_window(pid, *window_id);
            } else {
                log_debug!(
                    "[windows] destroyed cgwid={} has no indexed owner PID; thumbnail cleanup skipped",
                    window_id
                );
            }
            window_server::forget_destroyed_window_owner(*window_id);
            forget_non_normal_window(*window_id);
        }
    }
    let should_refresh = events.iter().any(|event| match event {
        window_server::WindowServerEvent::Created
        | window_server::WindowServerEvent::Destroyed(_) => true,
        window_server::WindowServerEvent::Focused(window_id) => {
            let displayed_pid = with_tab_state(|state_opt| {
                state_opt
                    .as_ref()
                    .and_then(|state| {
                        state
                            .windows
                            .iter()
                            .find(|window| window.window_id == *window_id)
                    })
                    .map(|window| window.pid)
            });
            let pid = displayed_pid
                .or_else(|| window_server::owner_for_window(*window_id))
                .or_else(|| owner_pid_for_cgwid(*window_id));
            if let Some(pid) = pid {
                let frontmost_pid = frontmost_app_info().1;
                log_debug!(
                    "[windows] focused event: cgwid={} pid={} displayed={} frontmost={}",
                    window_id,
                    pid,
                    displayed_pid.is_some(),
                    frontmost_pid == pid
                );
                if frontmost_pid == pid {
                    let activation_token = window_server::activation_token(pid);
                    if window_server::focus_should_bump(pid, *window_id) {
                        with_tab_state(|state_opt| {
                            if let Some(state) = state_opt.as_mut() {
                                // Anchor the focus key only for an AX-confirmed, shown window; an
                                // undisplayed CG surface (the other Ghostty tab) must not be anchored.
                                best_effort_bump_focus_key(state, pid, *window_id);
                            }
                        });
                        // A same-app window switch brings no new app-activation
                        // notification, so the activation token may have expired away;
                        // mint one here so externally driven window switches also
                        // refresh the thumbnail.
                        let activated_at = activation_token
                            .unwrap_or_else(|| crate::window_collector::note_app_activated(pid));
                        thumbnail::refresh_after_activation(pid, *window_id, activated_at);
                    }
                    if displayed_pid.is_none() {
                        // Track an undisplayed CG window too, but only let a directed AX refresh
                        // promote it into the card list after confirmation.
                        log_debug!(
                            "[windows] focused unlisted cgwid={} pid={}; directed refresh",
                            window_id,
                            pid
                        );
                        request_focused_window_refresh(pid, *window_id);
                    }
                }
            } else {
                log_debug!(
                    "[windows] focused cgwid={} has no known owner PID",
                    window_id
                );
            }
            false
        }
    });
    if should_refresh {
        request_lifecycle_window_refresh();
    }
}

// Bump (pid, cgwid) into MRU, anchoring the focus key only when the window is actually shown
// in the current display list. An undisplayed CG surface (the other Ghostty tab of a
// single-window multi-tab app) is never anchored, so the first frame cannot mismatch and show
// "switched to tab A but tab B appears". Caller must hold TAB_STATE.
pub(crate) fn best_effort_bump_focus_key(state: &mut AppState, pid: i32, cgwid: u32) {
    bump_window_mru(&mut state.mru, pid, cgwid);
    let shown = state
        .windows
        .iter()
        .any(|window| window.pid == pid && window.window_id == cgwid);
    if shown {
        state.focus_key = Some((pid, cgwid));
    }
}

#[cfg(test)]
mod tests {
    use super::{
        merge_refreshed_windows, preserve_existing_window_order, run_apply_with_in_flight_guard,
        selection_index_after_refresh, WindowRefreshReason, WindowRefreshResult,
    };
    use crate::window_collector::WindowInfo;
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};

    static IN_FLIGHT_TEST_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    fn window(pid: i32, window_id: u32) -> WindowInfo {
        WindowInfo {
            pid,
            window_id,
            app_name: format!("App {pid}"),
            window_title: format!("Window {window_id}"),
            icon_path: None,
            is_active: false,
            minimized: false,
            app_hidden: false,
            bounds: (0.0, 0.0, 100.0, 100.0),
        }
    }

    #[test]
    fn in_flight_guard_releases_the_flag_unless_it_is_handed_off() {
        use super::{InFlightGuard, WINDOW_REFRESH_IN_FLIGHT};
        use std::panic::{catch_unwind, AssertUnwindSafe};
        use std::sync::atomic::Ordering;

        let _lock = IN_FLIGHT_TEST_LOCK.lock().unwrap();

        // A panic before the hand-off (the shape the main-thread debug assertion takes) must
        // reset the flag; otherwise one panic silences the pipeline for good and a dead app's
        // card stays in the switcher.
        let panicked = catch_unwind(AssertUnwindSafe(|| {
            WINDOW_REFRESH_IN_FLIGHT.store(true, Ordering::Release);
            let _guard = InFlightGuard::new();
            panic!("simulated refresh setup failure");
        }));
        assert!(panicked.is_err());
        assert!(!WINDOW_REFRESH_IN_FLIGHT.load(Ordering::Acquire));

        // After a successful hand-off the flag stays set for the next stage to release: the
        // worker to the main-thread apply, or apply to its deferred re-entry during a card-close
        // transition. Apply wraps itself in the same guard, so the post-hand-off stage cannot
        // wedge either.
        WINDOW_REFRESH_IN_FLIGHT.store(true, Ordering::Release);
        InFlightGuard::new().hand_off();
        assert!(WINDOW_REFRESH_IN_FLIGHT.load(Ordering::Acquire));
        WINDOW_REFRESH_IN_FLIGHT.store(false, Ordering::Release);
    }

    #[test]
    fn deferred_refresh_watchdog_recovers_only_the_matching_result() {
        use super::{
            recover_stalled_deferred_refresh, DEFERRED_REFRESH_HANDOFF_TOKEN,
            WINDOW_REFRESH_IN_FLIGHT, WINDOW_REFRESH_RESULT,
        };
        use std::sync::atomic::Ordering;

        let _lock = IN_FLIGHT_TEST_LOCK.lock().unwrap();
        let generation = 41;
        let token = 7;
        DEFERRED_REFRESH_HANDOFF_TOKEN.store(token, Ordering::Release);
        WINDOW_REFRESH_IN_FLIGHT.store(true, Ordering::Release);
        *WINDOW_REFRESH_RESULT.lock().unwrap() = Some(WindowRefreshResult {
            generation,
            windows: Vec::new(),
            mru: HashMap::new(),
            replace_pid: None,
            active_key: None,
            summon_focus_key: None,
        });

        assert!(recover_stalled_deferred_refresh(generation, token));
        assert!(!WINDOW_REFRESH_IN_FLIGHT.load(Ordering::Acquire));
        assert!(WINDOW_REFRESH_RESULT.lock().unwrap().is_none());

        WINDOW_REFRESH_IN_FLIGHT.store(true, Ordering::Release);
        *WINDOW_REFRESH_RESULT.lock().unwrap() = Some(WindowRefreshResult {
            generation,
            windows: Vec::new(),
            mru: HashMap::new(),
            replace_pid: None,
            active_key: None,
            summon_focus_key: None,
        });
        assert!(!recover_stalled_deferred_refresh(generation, token + 1));
        assert!(WINDOW_REFRESH_IN_FLIGHT.load(Ordering::Acquire));
        WINDOW_REFRESH_RESULT.lock().unwrap().take();
        WINDOW_REFRESH_IN_FLIGHT.store(false, Ordering::Release);
    }

    #[test]
    fn apply_guard_releases_after_an_apply_panic_and_preserves_new_handoff() {
        use super::{InFlightGuard, WINDOW_REFRESH_IN_FLIGHT};
        use std::sync::atomic::Ordering;

        let _lock = IN_FLIGHT_TEST_LOCK.lock().unwrap();
        WINDOW_REFRESH_IN_FLIGHT.store(true, Ordering::Release);
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_apply_with_in_flight_guard(|_in_flight| {
                panic!("simulated apply failure");
            });
        }));
        assert!(panicked.is_err());
        assert!(!WINDOW_REFRESH_IN_FLIGHT.load(Ordering::Acquire));

        // A delayed callback with no result must not clear a newer hand-off.
        WINDOW_REFRESH_IN_FLIGHT.store(true, Ordering::Release);
        let guard = InFlightGuard::new();
        if WINDOW_REFRESH_IN_FLIGHT.load(Ordering::Acquire) {
            guard.hand_off();
        }
        assert!(WINDOW_REFRESH_IN_FLIGHT.load(Ordering::Acquire));
        WINDOW_REFRESH_IN_FLIGHT.store(false, Ordering::Release);
    }

    #[test]
    fn lifecycle_refresh_does_not_bump_frontmost() {
        assert!(!WindowRefreshReason::Lifecycle.bumps_frontmost());
        assert!(WindowRefreshReason::Summon.bumps_frontmost());
    }

    #[test]
    fn pending_refresh_keeps_summon_priority() {
        assert_eq!(
            WindowRefreshReason::Lifecycle.merge(WindowRefreshReason::Lifecycle),
            WindowRefreshReason::Lifecycle
        );
        assert_eq!(
            WindowRefreshReason::Lifecycle.merge(WindowRefreshReason::Summon),
            WindowRefreshReason::Summon
        );
        assert_eq!(
            WindowRefreshReason::Summon.merge(WindowRefreshReason::Lifecycle),
            WindowRefreshReason::Summon
        );
    }

    #[test]
    fn focused_refresh_replaces_only_the_target_pid() {
        let existing = vec![window(10, 100), window(20, 200), window(10, 101)];
        let refreshed = vec![window(10, 102), window(10, 103)];

        let merged = merge_refreshed_windows(&existing, Some(10), refreshed);
        let keys: Vec<(i32, u32)> = merged
            .iter()
            .map(|window| (window.pid, window.window_id))
            .collect();

        assert_eq!(keys, vec![(20, 200), (10, 102), (10, 103)]);
    }

    #[test]
    fn full_refresh_is_ax_authoritative() {
        let existing = vec![window(10, 100), window(20, 200)];
        let refreshed = vec![window(20, 200)];

        let merged = merge_refreshed_windows(&existing, None, refreshed);
        let keys: Vec<(i32, u32)> = merged
            .iter()
            .map(|window| (window.pid, window.window_id))
            .collect();

        assert_eq!(keys, vec![(20, 200)]);
    }

    #[test]
    fn visible_refresh_preserves_existing_order_but_uses_refreshed_records() {
        let existing = vec![window(1, 10), window(2, 20), window(3, 30), window(4, 40)];
        let mut refreshed = vec![window(4, 40), window(1, 10), window(2, 20), window(3, 30)];
        refreshed[0].window_title = "updated Edge".into();

        let ordered = preserve_existing_window_order(&existing, refreshed);
        let keys: Vec<(i32, u32)> = ordered
            .iter()
            .map(|window| (window.pid, window.window_id))
            .collect();

        assert_eq!(keys, vec![(1, 10), (2, 20), (3, 30), (4, 40)]);
        assert_eq!(ordered[3].window_title, "updated Edge");
    }

    #[test]
    fn visible_order_helper_drops_removed_windows_and_appends_new_windows() {
        let existing = vec![window(1, 10), window(2, 20), window(3, 30)];
        let refreshed = vec![window(4, 40), window(3, 30), window(1, 10)];

        let ordered = preserve_existing_window_order(&existing, refreshed);
        let keys: Vec<(i32, u32)> = ordered
            .iter()
            .map(|window| (window.pid, window.window_id))
            .collect();

        assert_eq!(keys, vec![(1, 10), (3, 30), (4, 40)]);
    }

    #[test]
    fn fresh_summon_focus_key_overrides_stale_same_pid_key() {
        let windows = vec![window(23199, 2837), window(23199, 13512)];

        let selected =
            super::select_summon_focus_key(Some((23199, 13512)), Some((23199, 2837)), &windows);

        assert_eq!(selected, Some((23199, 13512)));
    }

    #[test]
    fn summon_focus_key_falls_back_to_cached_present_key() {
        let windows = vec![window(23199, 2837), window(23199, 13512)];

        let selected = super::select_summon_focus_key(None, Some((23199, 2837)), &windows);

        assert_eq!(selected, Some((23199, 2837)));
    }

    #[test]
    fn known_window_is_removed_when_its_cg_window_is_gone() {
        let existing = vec![window(10, 100), window(20, 200)];
        let refreshed = vec![window(20, 200)];

        let merged = merge_refreshed_windows(&existing, None, refreshed);
        let keys: Vec<(i32, u32)> = merged
            .iter()
            .map(|window| (window.pid, window.window_id))
            .collect();

        assert_eq!(keys, vec![(20, 200)]);
    }

    #[test]
    fn minimized_known_window_is_not_retained_when_hidden_windows_are_disabled() {
        let mut minimized = window(10, 100);
        minimized.minimized = true;
        let existing = vec![minimized];

        let merged = merge_refreshed_windows(&existing, None, Vec::new());

        assert!(merged.is_empty());
    }

    #[test]
    fn focused_refresh_drops_omitted_sibling() {
        let existing = vec![window(10, 100), window(10, 101), window(20, 200)];
        let refreshed = vec![window(10, 102)];

        let merged = merge_refreshed_windows(&existing, Some(10), refreshed);
        let keys: Vec<(i32, u32)> = merged
            .iter()
            .map(|window| (window.pid, window.window_id))
            .collect();

        assert_eq!(keys, vec![(20, 200), (10, 102)]);
    }

    #[test]
    fn refresh_selection_follows_the_exact_window_key_after_reordering() {
        let windows = vec![window(20, 200), window(10, 100), window(10, 101)];

        assert_eq!(
            selection_index_after_refresh(Some((10, 101)), 0, &windows),
            2
        );
    }

    #[test]
    fn refresh_selection_keeps_the_previous_slot_when_the_key_is_gone() {
        let windows = vec![window(20, 200), window(10, 100)];

        assert_eq!(
            selection_index_after_refresh(Some((10, 999)), 3, &windows),
            1
        );
    }
}
