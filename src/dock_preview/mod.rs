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
use crate::dock_preview::windows::CollectedWindows;
use crate::ffi::{make_nsstring, MainThreadSlot};
use crate::{log_debug, log_info};

mod decisions;
mod dock_click;
mod layout;
mod observer;
mod panel;
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
type ActivationResult = (u64, i32, u32, bool, bool, bool);
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
    panel::hide();
    panel::flush_records();
    crate::thumbnail::begin_dock_capture_epoch();
    stop_tick_timer();
    if let Some(handle) = OBSERVER.lock().unwrap().as_ref() {
        handle.pause();
    }
    EVENTS.lock().unwrap().clear();
    RESULTS.lock().unwrap().clear();
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
pub(crate) fn handle_app_terminated(pid: i32) {
    if !is_enabled() {
        return;
    }
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
    let (pid, wid) = (target.pid, target.wid);
    let (minimized, other_desktop) = (window.minimized, window.other_desktop);
    let identity = (app_pid, identity_key, start_time_us);
    let request = target.request;
    let _ = std::thread::Builder::new()
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
                pid,
                wid,
                ok,
                minimized,
                other_desktop,
            ));
            post_event_to_main();
        });
}

fn drain_activate_results() {
    let results = {
        let mut queue = ACTIVATE_RESULTS.lock().unwrap();
        std::mem::take(&mut *queue)
    };
    for (request, pid, wid, ok, minimized, other_desktop) in results {
        if !decisions::event_is_current(request, REQUEST.load(Ordering::Acquire)) {
            continue;
        }
        if !ok {
            log_debug!("[dock-preview] activation target not confirmed; not raising");
            continue;
        }
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

fn hide_panel() {
    let was_visible = panel::is_visible();
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
    if preview == "null" {
        format!(
            "{{\"visible\": false, \"dock\": {dock_json}, \"press_dismissals\": {press_dismissals}}}"
        )
    } else {
        format!(
            "{{\"panel\": {preview}, \"dock\": {dock_json}, \"press_dismissals\": {press_dismissals}}}"
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
        return;
    }
    let now = Instant::now();
    let pointer = pointer_in_cg().unwrap_or((f64::NAN, f64::NAN));
    // Releasing the press and its latch happens here, before any decision this tick makes: leaving
    // the dismissed tile is what makes the next hover able to open a preview again.
    release_dismiss_state(pointer);
    if panel::is_visible() {
        // Moving along the Dock from the previewed icon to a different tile (including a tile
        // whose app is not running) ends the preview immediately: the observer's selection event
        // can lag or not change at all for a non-running tile, and a panel describing the previous
        // app must never outlive the hover.
        let other_tile = {
            let dock = *DOCK_STATE.lock().unwrap();
            match (dock.list, panel::anchor_rect()) {
                (Some(list), Some((anchor, _))) => {
                    decisions::pointer_is_on_other_dock_tile(list, anchor, pointer)
                }
                _ => false,
            }
        };
        if other_tile {
            hide_panel();
            return;
        }
        let outcome = panel::tick(now, pointer);
        panel::flush_records();
        if outcome == panel::TickOutcome::Empty {
            hide_panel();
            return;
        }
        let zone = panel::current_pointer_zone();
        let scheduled = *CLOSE_AT.lock().unwrap();
        match decisions::close_decision(zone, now, scheduled) {
            CloseDecision::Keep => *CLOSE_AT.lock().unwrap() = None,
            CloseDecision::Wait => {}
            CloseDecision::Schedule(at) => *CLOSE_AT.lock().unwrap() = Some(at),
            CloseDecision::HideNow => hide_panel(),
        }
    }
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
    // Each case states its own budget: a short, wide room for the list, a tall, narrow one for the
    // horizontal row. Measuring both from the display made the list fit on an ordinary screen and
    // the row fit on a wide one.
    set_case_room("smoke-dock-room-h", "smoke-dock-room-w");
    checks += smoke_list_scroll()?;
    set_case_room("smoke-dock-row-room-h", "smoke-dock-room-w");
    checks += smoke_horizontal_row()?;
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

/// The horizontal case: a single row that does not fit is clipped by a viewport no wider than the
/// screen, and scrolling it moves the content on x only.
fn smoke_horizontal_row() -> Result<u32, SmokeFailure> {
    let original = crate::config::CONFIG.read().unwrap().clone();
    let mut narrow = original.clone();
    narrow.dock_preview.cards_per_row = crate::config::DOCK_CARDS_PER_ROW_MAX;
    let _ = crate::runtime_config::smoke_apply_threshold(&original, &narrow);
    let titles = [
        "wide one",
        "wide two",
        "wide three",
        "wide four",
        "wide five",
        "wide six",
    ];
    // The horizontal case runs under a SHORT height and a NARROW width from the runner; a row that
    // still fits would prove nothing, so a card row is required to exist and to overflow its
    // viewport.
    let restore = |original: &crate::config::Config| {
        let current = crate::config::CONFIG.read().unwrap().clone();
        let _ = crate::runtime_config::smoke_apply_threshold(&current, original);
    };
    if !dev_show_windows(&titles, DockEdge::Bottom) {
        restore(&original);
        return Err(SmokeFailure::Failed(format!(
            "the panel did not open for the horizontal check (room {}pt)",
            crate::dev_flags::value("smoke-dock-room-h").unwrap_or_default()
        )));
    }
    pump(Duration::from_millis(80));
    let facts = panel::list_facts()
        .ok_or_else(|| SmokeFailure::Failed("the row panel published no facts".into()))?;
    let screen = primary_screen_bounds_cg();
    let mut checks = 0;
    if facts.mode != layout::PanelMode::Row {
        return Err(SmokeFailure::Failed(
            "six windows at the maximum threshold should be a single row".into(),
        ));
    }
    // The panel never grows past the screen, however wide the cards need to be.
    if facts.viewport.x < screen.x - 1.0 || facts.viewport.max_x() > screen.max_x() + 1.0 {
        return Err(SmokeFailure::Failed(format!(
            "the row's viewport {:?} leaves the screen {screen:?}",
            facts.viewport
        )));
    }
    checks += 1;
    // Six cards at the minimum width need more than the screen offers, so the row MUST scroll; a
    // case that does not overflow would only prove that nothing happened.
    if facts.max_scroll <= 0.0 {
        return Err(SmokeFailure::Failed(format!(
            "six cards fit the row without scrolling (max_scroll={}), so the horizontal case proves nothing",
            facts.max_scroll
        )));
    }
    {
        let before = facts.scroll;
        panel::scroll_by(layout::Axis::X, layout::CARD_MIN_W);
        panel::flush_records();
        let after = panel::list_facts()
            .ok_or_else(|| SmokeFailure::Failed("the row lost its facts".into()))?;
        if after.scroll <= before {
            return Err(SmokeFailure::Failed(
                "a horizontal step did not scroll the row".into(),
            ));
        }
        // Past the end: clamped, and the last card becomes reachable.
        panel::scroll_by(layout::Axis::X, 10_000.0);
        panel::flush_records();
        let end = panel::list_facts()
            .ok_or_else(|| SmokeFailure::Failed("the row lost its facts".into()))?;
        if (end.scroll - end.max_scroll).abs() > 0.5 {
            return Err(SmokeFailure::Failed(
                "the horizontal scroll is not clamped".into(),
            ));
        }
        let last = *end
            .item_screen
            .last()
            .ok_or_else(|| SmokeFailure::Failed("the row has no cards".into()))?;
        if !last.overlaps(end.viewport) {
            return Err(SmokeFailure::Failed(
                "the last card is unreachable at the end of the row".into(),
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

fn pointer_in_cg() -> Option<(f64, f64)> {
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
            // scrolling. The horizontal case needs a row that cannot fit: a tall enough room for a
            // card row, and a width under six cards at their floor width (`6*CARD_MIN_W`).
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
