//! This module writes a JSON snapshot of internal state for the A2 end-to-end layer (script +
//! cua-driver CLI; see the testing tiers in AGENTS.md). AX cannot express CALayer content (the
//! sidebar highlight pill) and cannot express "which card is selected" or "which window was just
//! raised" at all, so the app states the checkable facts itself: the script asserts on JSON and cua
//! is left with input and a few pixel probes. Enabled by `--e2e-state=<path>`; without it every
//! function returns immediately. Writes go through `<path>.tmp` + rename so a reader never sees
//! half a document, and `seq` lets a script wait for a *new* frame instead of sleeping.

use objc2::msg_send;
use objc2::runtime::AnyObject;
use objc2::sel;
use objc2_foundation::NSRect;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::OnceLock;
use std::sync::{LazyLock, Mutex};

use crate::log_debug;

static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
static SEQ: AtomicU64 = AtomicU64::new(0);
static SMOOTH_TICKS: AtomicU64 = AtomicU64::new(0);
static SPACE_RECOVERED_ACCEPTED: AtomicU64 = AtomicU64::new(0);
static SPACE_GATE_REJECTED: AtomicU64 = AtomicU64::new(0);
static OTHER_DESKTOP_ACCEPTED: AtomicU64 = AtomicU64::new(0);
/// The last cross-desktop raise, as one record: count, target identity, generation and outcome are
/// replaced together under this lock, so a snapshot never mixes one raise's identity with another's
/// result (atomics would allow exactly that interleaving).
static OTHER_DESKTOP_RAISE: std::sync::LazyLock<
    std::sync::Mutex<Option<(u64, OtherDesktopRaise)>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

/// The last commit this process made, kept sticky for the same reason the raise record is: the
/// commit *frame* is transient. The app writes the keystroke display's hide frame in the same
/// main-thread turn, so a scenario polling the state file can miss the commit frame entirely and a
/// commit assertion turns into a race. The frame still exists for readers that catch it.
static LAST_COMMIT: std::sync::LazyLock<std::sync::Mutex<Option<(u64, CommitRecord)>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

#[derive(Clone, Copy)]
struct CommitRecord {
    pid: i32,
    window_id: u32,
    index: usize,
}

/// One cross-desktop raise's outcome, as the raise path observed it.
///
/// A struct rather than a parameter list: the experiment added several timing fields, and a
/// positional call with eight plus six arguments is where a wrong field silently lands in the wrong
/// slot. `first_*` are `None` when the event never happened — a missing arrival must not be written
/// as `0`, which would read as "instant success".
pub(crate) struct RaiseOutcome {
    pub(crate) pid: i32,
    pub(crate) window_id: u32,
    pub(crate) generation: u64,
    pub(crate) onscreen: bool,
    pub(crate) ax_matched: bool,
    pub(crate) activation: bool,
    pub(crate) rescue_attempted: bool,
    pub(crate) rescue: bool,
    /// Front-switch attempts this raise made (1 in the rescue path today).
    pub(crate) attempts: u32,
    /// How the raise ended: `landed` / `timeout` / `cancelled` / `targetgone` / `identitymismatch`
    /// / `unknown`. Only `landed` means the switch happened.
    pub(crate) terminal: String,
    /// Milliseconds from the raise's submit to each first event, when it happened.
    pub(crate) first_rescue_ms: Option<u128>,
    pub(crate) first_ax_attempt_ms: Option<u128>,
    pub(crate) first_arrival_ms: Option<u128>,
    /// Submit to observation end.
    pub(crate) elapsed_ms: u128,
}

#[derive(Clone)]
struct OtherDesktopRaise {
    pid: i32,
    window_id: u32,
    generation: u64,
    onscreen: bool,
    ax_matched: bool,
    /// Whether the commit path's app activation was accepted by macOS. This is the branch that
    /// decides whether the exact-window front-switch rescue had to run, so it is published rather
    /// than left in the log.
    activation: bool,
    /// Whether the rescue front-switch was applied, and whether it reported success.
    rescue_attempted: bool,
    rescue: bool,
    attempts: u32,
    terminal: String,
    first_rescue_ms: Option<u128>,
    first_ax_attempt_ms: Option<u128>,
    first_arrival_ms: Option<u128>,
    elapsed_ms: u128,
}
static SPACE_MEMBERSHIP_SOURCE: AtomicU8 = AtomicU8::new(0);
static SPACE_IN_TRANSITION: AtomicBool = AtomicBool::new(false);
/// The last Space context this module published. A Space switch that the grouping feature handles
/// correctly does not change the candidate set, so nothing else would emit a frame for it; A2 needs
/// one to assert the app's own view of which Space is active.
static LAST_SPACE_CONTEXT: OnceLock<std::sync::Mutex<Option<String>>> = OnceLock::new();
static SPACE_TRANSITION_DEADLINE_MS: AtomicU64 = AtomicU64::new(0);
static SMOOTH_PHASES: [AtomicU64; 6] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

/// A held-Tab repeat cannot be synthesized (macOS generates the repeat stream from a physically
/// held key), so the accepted and throttled counts are the state that makes the behaviour
/// checkable: hold Tab, then read the snapshot.
static TAB_REPEAT_STEPS: AtomicU64 = AtomicU64::new(0);
static TAB_REPEAT_THROTTLED: AtomicU64 = AtomicU64::new(0);

pub(crate) fn tab_repeat_step() {
    if is_enabled() {
        TAB_REPEAT_STEPS.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn tab_repeat_throttled() {
    if is_enabled() {
        TAB_REPEAT_THROTTLED.fetch_add(1, Ordering::Relaxed);
    }
}

/// Resolves `--e2e-state=<path>` once, then serves it from cache.
fn state_path() -> Option<&'static PathBuf> {
    PATH.get_or_init(|| crate::dev_flags::value("e2e-state").map(PathBuf::from))
        .as_ref()
}

pub(crate) fn is_enabled() -> bool {
    state_path().is_some()
}

pub(crate) fn smooth_scroll_tick() {
    if is_enabled() {
        SMOOTH_TICKS.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn smooth_scroll_phase(phase: crate::mouse::smooth::engine::Phase) {
    if !is_enabled() {
        return;
    }
    let index = match phase {
        crate::mouse::smooth::engine::Phase::TouchBegan => 0,
        crate::mouse::smooth::engine::Phase::TouchChanged => 1,
        crate::mouse::smooth::engine::Phase::TouchEnded => 2,
        crate::mouse::smooth::engine::Phase::MomentumBegan => 3,
        crate::mouse::smooth::engine::Phase::MomentumChanged => 4,
        crate::mouse::smooth::engine::Phase::MomentumEnded => 5,
    };
    SMOOTH_PHASES[index].fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn space_recovered_accepted() {
    if is_enabled() {
        SPACE_RECOVERED_ACCEPTED.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn space_gate_rejected() {
    if is_enabled() {
        SPACE_GATE_REJECTED.fetch_add(1, Ordering::Relaxed);
    }
}

/// Why a CG window of this pass did not become a card. The scenario needs the *target's* own reason
/// rather than an inference from other cards: only `AxExcluded` is the accepted "this desktop has not
/// been visited in this process" narrowing, and anything else must stay a failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CardRejection {
    /// AX answered about the app and named another window, and never named this one.
    AxExcluded,
    /// The Space verdict refused it (another desktop with the switch off, or no managed Space).
    Space,
    /// No title of its own.
    Title,
    /// Not the ordinary layer-0 window shape.
    Shape,
    /// The app's AX answer identified no window at all, so its CG entries are panels.
    Unpaired,
}

impl CardRejection {
    fn as_str(self) -> &'static str {
        match self {
            CardRejection::AxExcluded => "ax_excluded",
            CardRejection::Space => "space",
            CardRejection::Title => "title",
            CardRejection::Shape => "shape",
            CardRejection::Unpaired => "unpaired",
        }
    }
}

// The rejections recorded by the collection running on *this* thread. Staged rather than published
// on the spot, for the same reason the AX evidence is: a superseded pass, a prewarm collection or a
// directed refresh must never move the evidence an applied frame's cards were built from.
thread_local! {
    static STAGED_REJECTIONS: std::cell::RefCell<Vec<(i32, u32, CardRejection)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}
static REJECTED_WINDOWS: LazyLock<Mutex<Vec<(i32, u32, CardRejection)>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// Start a collection pass on this thread: the staging describes the pass about to run.
///
/// Staging is not gated on `is_enabled`: it is a thread-local clear plus a bounded push per refused
/// window, and keeping it unconditional means the recording path has no second behaviour to test.
/// Only publishing and serialization are gated, so a normal run writes nothing.
pub(crate) fn begin_collection_pass() {
    STAGED_REJECTIONS.with(|staged| staged.borrow_mut().clear());
}

pub(crate) fn card_rejected(pid: i32, cgwid: u32, reason: CardRejection) {
    if cgwid == 0 {
        return;
    }
    STAGED_REJECTIONS.with(|staged| {
        let mut staged = staged.borrow_mut();
        if staged.len() < 128 {
            staged.push((pid, cgwid, reason));
        }
    });
}

/// Publish an accepted pass's rejections, next to the AX evidence they belong with. A full pass
/// replaces the list (even with nothing recorded: its cards are the truth); a directed pass replaces
/// only that pid's entries, so the other cards keep the reasons they were built from.
pub(crate) fn publish_staged_rejections(replace_pid: Option<i32>) {
    let staged = STAGED_REJECTIONS.with(|staged| std::mem::take(&mut *staged.borrow_mut()));
    let mut published = REJECTED_WINDOWS.lock().unwrap();
    match replace_pid {
        None => *published = staged,
        Some(pid) => {
            published.retain(|(entry_pid, _, _)| *entry_pid != pid);
            published.extend(
                staged
                    .into_iter()
                    .filter(|(entry_pid, _, _)| *entry_pid == pid),
            );
        }
    }
}

/// A card admitted only because Space membership places its window on another desktop. Counted
/// apart from `space_recovered_accepted` so a scenario can tell the switch's admissions from the
/// key/main recovery it replaces.
///
/// This counts admission *decisions*, not distinct cards: one summon runs several collection passes
/// (the full pass, directed refresh passes, and the refresh after the summon), so the number is
/// larger than the card count. Assert `> 0` here and use each card's `other_desktop` flag for the
/// exact set.
pub(crate) fn other_desktop_accepted() {
    if is_enabled() {
        OTHER_DESKTOP_ACCEPTED.fetch_add(1, Ordering::Relaxed);
    }
}

/// Record the outcome of a cross-desktop raise: whether the target window had become part of the
/// active desktop by the time the AX phase ran, and whether the app's AX answer then contained the
/// exact window and its raise was requested.
///
/// `ax_matched` is deliberately narrow: it means the exact window element was found in the app's AX
/// answer and the raise was *submitted* through `raise_ax_element`. It is not "the SLPS call returned
/// 0" (the fast path reports success against a window that is not on the active desktop at all), it
/// is not proof that the submission was enqueued (the main thread drops it when a newer switch
/// supersedes this one), and it is not proof that the window took focus -- that is what the
/// system-side AX focus read in the A2 scenario is for. The record carries the target
/// (pid, window id) and the raise generation so a scenario can bind it to one commit.
///
/// Called from the `ax-raiser` thread, so this only publishes a record: writing a snapshot reads the
/// main-thread runtime (`with_tab_state`) and touches AppKit views, which a worker thread must never
/// do. The record rides out on the next frame the main thread writes, exactly like the other
/// background-produced counters here, and one lock covers the whole record so a reader can never mix
/// one raise's identity with another's result.
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_other_desktop_raise(outcome: RaiseOutcome) {
    if !is_enabled() {
        return;
    }
    // One lock covers the whole record: the count and every field it describes are replaced
    // together, so a reader can never pair this raise's identity with another raise's result.
    let mut slot = OTHER_DESKTOP_RAISE.lock().unwrap();
    let count = slot.as_ref().map_or(1, |(count, _)| count + 1);
    *slot = Some((
        count,
        OtherDesktopRaise {
            pid: outcome.pid,
            window_id: outcome.window_id,
            generation: outcome.generation,
            onscreen: outcome.onscreen,
            ax_matched: outcome.ax_matched,
            activation: outcome.activation,
            rescue_attempted: outcome.rescue_attempted,
            rescue: outcome.rescue,
            attempts: outcome.attempts,
            terminal: outcome.terminal,
            first_rescue_ms: outcome.first_rescue_ms,
            first_ax_attempt_ms: outcome.first_ax_attempt_ms,
            first_arrival_ms: outcome.first_arrival_ms,
            elapsed_ms: outcome.elapsed_ms,
        },
    ));
}

/// The pids whose `kAXWindows` answer was non-empty (`published`) and whose key/main slots named a
/// window (`recovered`) in the collection the last accepted frame was built from.
///
/// A scenario needs both to tell the cross-desktop admission cases apart: an app with a published
/// window may legitimately show a real window of another desktop (the CG-only exception), while an
/// app whose list is Space-filtered *and* whose key/main slots named a window must not have other
/// windows invented for it. Staged by the collector and published only when the result is accepted,
/// so a discarded pass cannot label the cards of another one; the snapshot reads without consuming.
#[derive(Default, Clone)]
struct AxPidEvidence {
    published: Vec<i32>,
    recovered: Vec<i32>,
    /// Apps whose AX query failed. They are a different state from "AX answered with nothing":
    /// they keep the CG fallback, so an assertion about the empty case must not flag them.
    failed: Vec<i32>,
}

thread_local! {
    /// The evidence staged by the collection running on *this* thread. Thread-local on purpose: a
    /// prewarm collection on another thread must not be taken by the refresh worker as the evidence
    /// for its own windows.
    static STAGED_AX_PID_EVIDENCE: std::cell::RefCell<Option<AxPidEvidence>> =
        const { std::cell::RefCell::new(None) };
}
static AX_PID_EVIDENCE: std::sync::LazyLock<std::sync::Mutex<AxPidEvidence>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(AxPidEvidence::default()));

pub(crate) fn stage_ax_pid_evidence(
    published: impl IntoIterator<Item = i32>,
    recovered: impl IntoIterator<Item = i32>,
    failed: impl IntoIterator<Item = i32>,
) {
    if !is_enabled() {
        return;
    }
    let sort = |pids: Vec<i32>| {
        let mut pids = pids;
        pids.sort_unstable();
        pids.dedup();
        pids
    };
    let published = sort(published.into_iter().collect());
    let recovered = sort(recovered.into_iter().collect());
    let failed = sort(failed.into_iter().collect());
    STAGED_AX_PID_EVIDENCE.with(|slot| {
        *slot.borrow_mut() = Some(AxPidEvidence {
            published,
            recovered,
            failed,
        })
    });
}

/// Take the staged evidence (the worker takes it right after the collection, so the result carries
/// exactly the pass that produced its windows).
pub(crate) fn take_staged_ax_pid_evidence() -> Option<(Vec<i32>, Vec<i32>, Vec<i32>)> {
    STAGED_AX_PID_EVIDENCE
        .with(|slot| slot.borrow_mut().take())
        .map(|evidence| (evidence.published, evidence.recovered, evidence.failed))
}

/// Publish an accepted result's evidence. A full pass replaces both sets; a directed pass (one pid)
/// updates only that pid, so the other pids keep the evidence their cards were built from.
pub(crate) fn publish_ax_pid_evidence(
    evidence: Option<(Vec<i32>, Vec<i32>, Vec<i32>)>,
    replace_pid: Option<i32>,
) {
    let Some((published, recovered, failed)) = evidence else {
        return;
    };
    let mut known = AX_PID_EVIDENCE.lock().unwrap();
    match replace_pid {
        None => {
            known.published = published;
            known.recovered = recovered;
            known.failed = failed;
        }
        Some(pid) => {
            let update = |list: &mut Vec<i32>, present: bool| {
                list.retain(|known_pid| *known_pid != pid);
                if present {
                    list.push(pid);
                }
                list.sort_unstable();
            };
            update(&mut known.published, published.contains(&pid));
            update(&mut known.recovered, recovered.contains(&pid));
            update(&mut known.failed, failed.contains(&pid));
        }
    }
}

pub(crate) fn set_space_membership_source(skylight: bool) {
    if is_enabled() {
        SPACE_MEMBERSHIP_SOURCE.store(u8::from(skylight), Ordering::Relaxed);
    }
}

/// A stable string for the current per-display Space context: display, active Space, kind, origin.
fn space_context_signature() -> String {
    crate::space_groups::with_tracker(|tracker| {
        let mut contexts: Vec<_> = tracker
            .topology()
            .displays
            .iter()
            .map(|(display_id, display)| {
                let kind = tracker.topology().kind(display.current);
                let origin = tracker
                    .fullscreen_origins()
                    .get(&display.current)
                    .map(|origin| origin.ordinary_space);
                format!("{display_id}:{}:{kind:?}:{origin:?}", display.current)
            })
            .collect();
        contexts.sort();
        contexts.join("|")
    })
}

pub(crate) fn set_space_transition(in_transition: bool, deadline_unix_ms: u64) {
    if is_enabled() {
        SPACE_TRANSITION_DEADLINE_MS.store(deadline_unix_ms, Ordering::Relaxed);
        SPACE_IN_TRANSITION.store(in_transition, Ordering::Relaxed);
    }
}

/// Records one snapshot. Main thread only (it borrows AppState internally).
pub(crate) fn record(event: &str) {
    write(event, None);
}

/// Writes a frame when the app's Space context changed since the last published frame, even when
/// the candidate set did not. Called after a refresh is applied, so the frame's cards are the set
/// computed for the context it reports.
pub(crate) fn record_if_space_context_changed() {
    if !is_enabled() {
        return;
    }
    let signature = Some(space_context_signature());
    let last = LAST_SPACE_CONTEXT.get_or_init(|| std::sync::Mutex::new(None));
    let changed = {
        let mut last = last.lock().unwrap();
        if *last == signature {
            false
        } else {
            *last = signature;
            true
        }
    };
    if changed {
        write("refresh_context", None);
    }
}

/// Records a commit snapshot carrying the window this release targets. Must run *before* the
/// selection is cleared: once the overlay hides, AppState no longer holds a selected index.
/// Count of AX raise actions the main-thread drain actually performed.
///
/// A scenario asserts this moves across a commit: the user-visible regression was the drain silently
/// skipping its action (the title bar changed, the window never came forward), which every existing
/// check tolerated because it only looked at frontmost pid, focus and on-screen state.
static AX_ACTIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn record_ax_action() {
    AX_ACTIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn record_commit(pid: i32, window_id: u32, app: &str, index: usize) {
    if is_enabled() {
        let mut slot = LAST_COMMIT.lock().unwrap();
        let count = slot.as_ref().map_or(1, |(count, _)| count + 1);
        *slot = Some((
            count,
            CommitRecord {
                pid,
                window_id,
                index,
            },
        ));
    }
    write("commit", Some((pid, window_id, app.to_string(), index)));
}

/// A card snapshot (only the fields assertions need, so the JSON is not a mirror of internals).
struct Card {
    pid: i32,
    window_id: u32,
    app: String,
    title: String,
    active: bool,
    minimized: bool,
    fullscreen: bool,
    other_desktop: bool,
    /// Whether the cache already holds a frame for this window.
    thumbnail_ready: bool,
    /// Whether a card actually rendered a thumbnail for this window (as opposed to the icon
    /// fallback). This is the field a "the card stopped showing its thumbnail" regression trips,
    /// where availability alone would still read true.
    thumbnail_rendered: bool,
    /// Whether AX has ever identified this window as one of its app's windows. The rule it supports
    /// (asserted by `scripts/e2e/space-desktops.sh` together with `ax_published_pids`): an app whose
    /// `kAXWindows` answer was empty while its key/main slots named a window must not have other
    /// windows invented from the CG list -- 微信's off-screen second window used to appear as a
    /// second, dead card that way. An app that does have a published window may legitimately show a
    /// real window of another desktop that AX has not identified yet.
    ax_identified: bool,
    /// Whether the window is on screen right now, measured only for cards of an app whose AX query
    /// failed (the state that admits a CG-only card without AX evidence). `null` elsewhere: the A2
    /// assertion is "a card of an AX-failed app must be visible", the rule that keeps a closed
    /// menu-bar panel out of that fallback.
    on_screen: Option<bool>,
    bounds: (f64, f64, f64, f64),
    /// Which evidence decided each presented state flag ("ax" / "window_server" / "geometry" /
    /// "appkit" / "unknown"). A scenario proving the WindowServer path produced a value must read
    /// these: the boolean alone is satisfied by a fallback that happens to agree.
    minimized_source: &'static str,
    fullscreen_source: &'static str,
    app_hidden_source: &'static str,
    /// Ordered-in as the WindowServer reported it (`null` = that field could not be read).
    ordered_in: Option<bool>,
    /// Which AX route paired this window in the pass that produced the card: "published" /
    /// "recovered" / "unavailable" / "unpublished" (`null` = not recorded, test fixtures only).
    /// This, not `ax_identified` (which is a history), is how a scenario proves a card came through
    /// the no-element route.
    ax_pairing: Option<&'static str>,
    /// The WindowServer's own hidden-app tag, published for cross-checking the AppKit-derived
    /// `app_hidden` above. `null` = that field could not be read this pass.
    window_server_hidden: Option<bool>,
    /// The raw WindowServer row fields this card was decoded from, as hex strings. Present so a
    /// failed assertion can name the bits instead of only the decoded booleans.
    ws_attributes: Option<String>,
    ws_tags: Option<String>,
    ws_space_type_mask: Option<String>,
}

/// One node of the view tree. `frame` is in the parent's coordinate space, so cross-level
/// comparisons are meaningless: scripts must only compare nodes sharing a `parent`.
struct ViewNode {
    root: &'static str,
    parent: i64,
    depth: usize,
    class: String,
    frame: (f64, f64, f64, f64),
    text: Option<String>,
}

/// Walks the view tree recursively. `text` comes from `stringValue` (button title / text-field
/// content) and stays null when the view has none.
unsafe fn walk_views(
    view: *mut AnyObject,
    root: &'static str,
    parent: i64,
    depth: usize,
    out: &mut Vec<ViewNode>,
) {
    /// Depth and node caps: the settings page builds its own view tree, so bound the JSON growth.
    const MAX_DEPTH: usize = 8;
    const MAX_NODES: usize = 3000;
    if view.is_null() || depth > MAX_DEPTH || out.len() >= MAX_NODES {
        return;
    }
    let frame: NSRect = msg_send![view, frame];
    let class: *mut AnyObject = msg_send![view, class];
    let class_name = if class.is_null() {
        String::new()
    } else {
        let description: *mut AnyObject = msg_send![class, description];
        crate::ffi::nsstring_to_rust(description)
    };
    let text = {
        let responds: bool = msg_send![view, respondsToSelector: sel!(stringValue)];
        if responds {
            let value: *mut AnyObject = msg_send![view, stringValue];
            let text = crate::ffi::nsstring_to_rust(value);
            (!text.is_empty()).then_some(text)
        } else {
            None
        }
    };
    let index = out.len() as i64;
    out.push(ViewNode {
        root,
        parent,
        depth,
        class: class_name,
        frame: (
            frame.origin.x,
            frame.origin.y,
            frame.size.width,
            frame.size.height,
        ),
        text,
    });
    let subviews: *mut AnyObject = msg_send![view, subviews];
    if subviews.is_null() {
        return;
    }
    let count: usize = msg_send![subviews, count];
    for position in 0..count {
        let child: *mut AnyObject = msg_send![subviews, objectAtIndex: position];
        walk_views(child, root, index, depth + 1, out);
    }
}

/// Settings scroll geometry: the moment the page's document width differs from the visible width is
/// the failure moment (a legacy scroller takes space, the clip narrows while the document does not,
/// so the right column gets clipped). All three levels are recorded so nothing assumes which level
/// is the scroll view. `scroller_style`: 0 = legacy, 1 = overlay, -1 = not a scroll view / unknown.
struct PageGeometry {
    root: &'static str,
    self_frame: NSRect,
    parent_frame: NSRect,
    grandparent_frame: NSRect,
    /// The clip (contentView) bounds width is the truly visible content width. With a legacy
    /// scroller it is narrower than the document by the scroller width, while the document is still
    /// laid out to the window width -- which clips the right column.
    clip_bounds: NSRect,
    /// Page document height and the content's top/bottom edges (frames of the document's direct
    /// subviews, in document coordinates). A2 asserts "no dead space below the content / content is
    /// not clipped" from these; all three are measured off the document view, never a height
    /// constant.
    doc_height: f64,
    content_top: f64,
    content_bottom: f64,
    style_self: isize,
    style_parent: isize,
    style_grandparent: isize,
}

/// Document height plus the y range its direct subviews cover (content top / bottom edges). An empty
/// document yields (0, 0, 0).
unsafe fn document_extent(scroll: *mut AnyObject) -> (f64, f64, f64) {
    let document: *mut AnyObject = msg_send![scroll, documentView];
    if document.is_null() {
        return (0.0, 0.0, 0.0);
    }
    let frame: NSRect = msg_send![document, frame];
    let subviews: *mut AnyObject = msg_send![document, subviews];
    if subviews.is_null() {
        return (frame.size.height, 0.0, 0.0);
    }
    let count: usize = msg_send![subviews, count];
    let mut top = f64::NEG_INFINITY;
    let mut bottom = f64::INFINITY;
    for index in 0..count {
        let child: *mut AnyObject = msg_send![subviews, objectAtIndex: index as isize];
        if child.is_null() {
            continue;
        }
        let child_frame: NSRect = msg_send![child, frame];
        if child_frame.size.width <= 0.0 && child_frame.size.height <= 0.0 {
            continue;
        }
        top = top.max(child_frame.origin.y + child_frame.size.height);
        bottom = bottom.min(child_frame.origin.y);
    }
    if !top.is_finite() || !bottom.is_finite() {
        return (frame.size.height, 0.0, 0.0);
    }
    (frame.size.height, top, bottom)
}

fn zero_rect() -> NSRect {
    NSRect::new(
        objc2_foundation::NSPoint::new(0.0, 0.0),
        objc2_foundation::NSSize::new(0.0, 0.0),
    )
}

fn collect_pages() -> Vec<PageGeometry> {
    let mut pages = Vec::new();
    for (root, view) in crate::settings::e2e_view_roots() {
        if !root.starts_with("page_") || view.is_null() {
            continue;
        }
        unsafe {
            let scroller_style = |object: *mut AnyObject| -> isize {
                if object.is_null() {
                    return -1;
                }
                let responds: bool = msg_send![object, respondsToSelector: sel!(scrollerStyle)];
                if !responds {
                    return -1;
                }
                msg_send![object, scrollerStyle]
            };
            let frame_of = |object: *mut AnyObject| -> NSRect {
                if object.is_null() {
                    return zero_rect();
                }
                msg_send![object, frame]
            };
            let parent: *mut AnyObject = msg_send![view, superview];
            let grandparent: *mut AnyObject = if parent.is_null() {
                std::ptr::null_mut()
            } else {
                msg_send![parent, superview]
            };
            let clip: *mut AnyObject = {
                let responds: bool = msg_send![view, respondsToSelector: sel!(contentView)];
                if responds {
                    msg_send![view, contentView]
                } else {
                    std::ptr::null_mut()
                }
            };
            let clip_bounds: NSRect = if clip.is_null() {
                zero_rect()
            } else {
                msg_send![clip, bounds]
            };
            let (doc_height, content_top, content_bottom) = document_extent(view);
            pages.push(PageGeometry {
                root,
                self_frame: frame_of(view),
                parent_frame: frame_of(parent),
                grandparent_frame: frame_of(grandparent),
                clip_bounds,
                doc_height,
                content_top,
                content_bottom,
                style_self: scroller_style(view),
                style_parent: scroller_style(parent),
                style_grandparent: scroller_style(grandparent),
            });
        }
    }
    pages
}

fn collect_views() -> Vec<ViewNode> {
    let mut nodes = Vec::new();
    for (root, view) in crate::settings::e2e_view_roots() {
        unsafe { walk_views(view, root, -1, 0, &mut nodes) };
    }
    nodes
}

struct Snapshot {
    visible: bool,
    selected: usize,
    windows: Vec<Card>,
}

fn collect() -> Snapshot {
    // Which apps' AX read failed in the pass these cards came from; only their cards get the
    // on-screen measurement (one targeted query each, and only for diagnostics).
    let failed_pids: Vec<i32> = AX_PID_EVIDENCE.lock().unwrap().failed.clone();
    crate::with_tab_state(|state_opt| match state_opt.as_ref() {
        Some(state) => Snapshot {
            visible: state.visible,
            selected: state.selected,
            windows: state
                .windows
                .iter()
                .map(|w| Card {
                    pid: w.pid,
                    window_id: w.window_id,
                    app: w.app_name.clone(),
                    title: w.window_title.clone(),
                    active: w.is_active,
                    minimized: w.minimized,
                    fullscreen: w.fullscreen,
                    other_desktop: w.on_other_desktop,
                    thumbnail_ready: crate::thumbnail::frame_available(w.pid, w.window_id),
                    thumbnail_rendered: crate::thumbnail::frame_was_rendered(w.pid, w.window_id),
                    ax_identified: crate::window_collector::is_ax_identified_window_for_pid(
                        w.pid,
                        w.window_id,
                    ),
                    on_screen: failed_pids
                        .contains(&w.pid)
                        .then(|| crate::window_collector::window_is_onscreen_now(w.window_id)),
                    bounds: w.bounds,
                    minimized_source: state_source_label(w.state.minimized_source),
                    fullscreen_source: state_source_label(w.state.fullscreen_source),
                    app_hidden_source: state_source_label(w.state.app_hidden_source),
                    ordered_in: w.state.ordered_in,
                    ax_pairing: w.state.pairing.map(|pairing| pairing.label()),
                    window_server_hidden: crate::window_collector::ax_app_hidden_tag(
                        w.state.row.as_ref(),
                    ),
                    ws_attributes: ws_hex(w.state.row.as_ref(), |row| row.attributes),
                    ws_tags: ws_hex(w.state.row.as_ref(), |row| row.tags),
                    ws_space_type_mask: ws_hex(w.state.row.as_ref(), |row| row.space_type_mask),
                })
                .collect(),
        },
        None => Snapshot {
            visible: false,
            selected: 0,
            windows: Vec::new(),
        },
    })
}

/// The stable name of a state source, for scenario assertions.
fn state_source_label(source: crate::window_collector::StateSource) -> &'static str {
    use crate::window_collector::StateSource;
    match source {
        StateSource::Unknown => "unknown",
        StateSource::Ax => "ax",
        StateSource::WindowServer => "window_server",
        StateSource::Geometry => "geometry",
        StateSource::AppKit => "appkit",
    }
}

/// One raw WindowServer row field as a hex string, or None when the row or the field is missing.
fn ws_hex(
    row: Option<&crate::skylight::WsWindowRow>,
    field: impl Fn(&crate::skylight::WsWindowRow) -> Option<u64>,
) -> Option<String> {
    row.and_then(&field).map(|value| format!("0x{value:x}"))
}

/// `null` for "the event never happened" — writing 0 would read as "it happened instantly".
fn json_opt_u128(value: Option<u128>) -> String {
    value.map_or_else(|| "null".to_string(), |value| value.to_string())
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

fn write(event: &str, committed: Option<(i32, u32, String, usize)>) {
    let Some(path) = state_path() else {
        return;
    };
    let snapshot = collect();
    let seq = SEQ.fetch_add(1, Ordering::SeqCst) + 1;
    let (front_app, front_pid) = crate::ffi::frontmost_app_info();
    let thumbnails = crate::overlay::thumbnail_visible_range();

    let mut json = String::with_capacity(2048);
    json.push_str("{\n");
    json.push_str(&format!("  \"seq\": {seq},\n"));
    // The writing process. A scenario restarts the app between runs, so a snapshot left on disk by
    // the previous process can carry a higher `seq` than the new one has reached yet; scoping the
    // baseline to the pid is what makes "wait for a newer frame" mean a frame from *this* run.
    json.push_str(&format!("  \"pid\": {},\n", std::process::id()));
    json.push_str(&format!("  \"event\": {},\n", json_string(event)));
    json.push_str(&format!(
        "  \"ax_actions\": {},\n",
        AX_ACTIONS.load(std::sync::atomic::Ordering::Relaxed)
    ));
    json.push_str(&format!("  \"visible\": {},\n", snapshot.visible));
    json.push_str(&format!(
        "  \"settings_window_visible\": {},\n",
        crate::settings::settings_window_is_visible()
    ));
    // The installed panel material (after the Reduce Transparency override) — AX cannot tell
    // a glass view from a plain layer, so the app states the fact itself.
    json.push_str(&format!(
        "  \"panel_material\": {},\n",
        json_string(crate::glass::effective_material_id())
    ));
    // Where the clipboard picker actually is, top-left based, so a screenshot can measure inside it
    // instead of guessing from the cursor position. `footer_band` is the footer legend band in the panel's
    // own points: the footer captions are a different text role from the filter row's, so the A2 contrast
    // scenario needs their region to measure each role separately (a whole-row extent would report only the
    // strongest ink and could hide a dim one).
    // One region per footer caption: a union of them is a bounding box that also spans the keycaps, and a
    // region has to hold one text role before its contrast number means anything.
    let hints = {
        let frames = crate::clipboard::picker_footer_hint_frames();
        if frames.is_empty() {
            "null".to_string()
        } else {
            let items: Vec<String> = frames
                .iter()
                .map(|(x, y, w, h)| {
                    format!("{{\"x\": {x:.1}, \"y\": {y:.1}, \"w\": {w:.1}, \"h\": {h:.1}}}")
                })
                .collect();
            format!("[{}]", items.join(", "))
        }
    };
    json.push_str(&format!(
        "  \"clipboard_picker\": {},\n",
        match crate::clipboard::picker_frame_top_left() {
            Some((x, y, w, h)) => format!(
                "{{\"visible\": {}, \"x\": {:.1}, \"y\": {:.1}, \"w\": {:.1}, \"h\": {:.1}, \"footer_hints\": {}}}",
                crate::clipboard::picker_is_visible(),
                x,
                y,
                w,
                h,
                hints
            ),
            None => "null".to_string(),
        }
    ));
    // The picker *window* rect: the panel rect above is what layout and the user mean, this one is the
    // padded rect the shadow needs. An A2 scenario that measures the shadow's outer tail needs the window's
    // edge, and one that measures the outline or the panel's interior needs the panel's -- reporting both is
    // what keeps the two measurements from being taken against the wrong rect.
    json.push_str(&format!(
        "  \"clipboard_picker_window\": {},\n",
        match crate::clipboard::picker_window_frame_top_left() {
            Some((x, y, w, h)) =>
                format!("{{\"x\": {x:.1}, \"y\": {y:.1}, \"w\": {w:.1}, \"h\": {h:.1}}}"),
            None => "null".to_string(),
        }
    ));
    // The decorations actually in effect, so a scenario can tell "the switch did not take" from "the
    // measurement is wrong" without inferring either from pixels.
    json.push_str(&format!(
        "  \"panel_decorations\": {{\"outline\": {}, \"outline_width\": {:.1}, \"elevation\": \"{}\"}},\n",
        crate::glass::panel_outline_enabled(),
        crate::theme::PANEL_OUTLINE_WIDTH,
        crate::glass::effective_panel_elevation_id()
    ));
    // The effective glass look and the tint it resolved to, verbatim from the config. AX cannot express a
    // tint and `NSGlassEffectView.tintColor` reports a system default, so the app states the value itself:
    // this is what makes "the panel shipped with the wrong tint (or an opaque white sheet)" assertable.
    json.push_str(&format!(
        "  \"glass_look\": {}, \"glass_tint\": \"{:08x}\",\n",
        json_string(&crate::config::effective_glass_style()),
        crate::glass::resolved_glass_tint_hex()
    ));
    // The material-strength knobs, and whether the private `_variant` selector exists on this system:
    // no accessibility tree can express either fact, and the look depends on both.
    json.push_str(&format!(
        "  \"panel_material_tuning\": {{\"opacity\": {:.3}, \"tint\": \"{:08x}\", \"blur_radius\": {:.1}, \"saturation\": {:.2}, \"variant\": {}, \"variant_supported\": {}}},\n",
        crate::config::effective_glass_opacity(),
        crate::glass::resolved_glass_tint_hex(),
        crate::config::effective_glass_blur_radius(),
        crate::config::effective_glass_saturation(),
        crate::config::effective_glass_variant(),
        crate::glass::glass_variant_is_supported()
    ));
    json.push_str(&format!(
        "  \"smooth_scroll\": {{\"ticks\": {}, \"touch_began\": {}, \"touch_changed\": {}, \"touch_ended\": {}, \"momentum_began\": {}, \"momentum_changed\": {}, \"momentum_ended\": {}}},\n",
        SMOOTH_TICKS.load(Ordering::Relaxed),
        SMOOTH_PHASES[0].load(Ordering::Relaxed),
        SMOOTH_PHASES[1].load(Ordering::Relaxed),
        SMOOTH_PHASES[2].load(Ordering::Relaxed),
        SMOOTH_PHASES[3].load(Ordering::Relaxed),
        SMOOTH_PHASES[4].load(Ordering::Relaxed),
        SMOOTH_PHASES[5].load(Ordering::Relaxed),
    ));
    let space_group_state = crate::space_groups::with_tracker(|tracker| {
        let unknown_active = tracker
            .topology()
            .displays
            .iter()
            .filter(|(display_id, display)| {
                tracker.topology().kind(display.current)
                    == crate::space_groups::SpaceKind::Fullscreen
                    && tracker
                        .topology()
                        .effective_origin(display_id, display.current, tracker.fullscreen_origins())
                        .is_none()
            })
            .count();
        // Every Space the topology knows, with its kind: a scenario has to tell an ordinary other
        // desktop from a fullscreen Space that belongs to the current desktop's group.
        let mut spaces: Vec<(u64, &str)> = tracker
            .topology()
            .displays
            .values()
            .flat_map(|display| display.spaces.iter())
            .map(|(space, kind)| {
                (
                    *space,
                    match kind {
                        crate::space_groups::SpaceKind::Ordinary => "ordinary",
                        crate::space_groups::SpaceKind::Fullscreen => "fullscreen",
                        crate::space_groups::SpaceKind::Unknown => "unknown",
                    },
                )
            })
            .collect();
        spaces.sort_unstable();
        let mut contexts: Vec<_> = tracker
            .topology()
            .displays
            .iter()
            .map(|(display_id, display)| {
                let kind = tracker.topology().kind(display.current);
                // The origin that actually governs this Space: a learned association when one was
                // observed, otherwise the one the native Space order implies.
                let origin = tracker.topology().effective_origin(
                    display_id,
                    display.current,
                    tracker.fullscreen_origins(),
                );
                (display_id.clone(), display.current, kind, origin)
            })
            .collect();
        contexts.sort_by(|a, b| a.0.cmp(&b.0));
        (
            tracker.topology().displays.len(),
            tracker.fullscreen_origins().len(),
            unknown_active,
            tracker.evidence_contiguous(),
            crate::window_server::space_membership_tracking_available(),
            contexts,
            spaces,
        )
    });
    let spaces = space_group_state
        .6
        .iter()
        .map(|(space, kind)| format!("{{\"id\": {space}, \"kind\": \"{kind}\"}}"))
        .collect::<Vec<_>>()
        .join(", ");
    json.push_str(&format!("  \"spaces\": [{}],\n", spaces));
    let space_contexts = space_group_state
        .5
        .iter()
        .map(|(display_id, space_id, kind, origin)| {
            let kind = match kind {
                crate::space_groups::SpaceKind::Ordinary => "ordinary",
                crate::space_groups::SpaceKind::Fullscreen => "fullscreen",
                crate::space_groups::SpaceKind::Unknown => "unknown",
            };
            let origin = origin
                .map(|origin| origin.to_string())
                .unwrap_or_else(|| "null".into());
            format!(
                "{{\"display\": \"{}\", \"space\": {}, \"kind\": \"{}\", \"origin\": {}}}",
                display_id, space_id, kind, origin
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    json.push_str(&format!("  \"space_contexts\": [{}],\n", space_contexts));
    let rejected_windows = REJECTED_WINDOWS
        .lock()
        .unwrap()
        .iter()
        .map(|(_, cgwid, reason)| {
            format!(
                "{{\"window_id\": {cgwid}, \"reason\": \"{}\"}}",
                reason.as_str()
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    json.push_str(&format!(
        "  \"rejected_windows\": [{}],\n",
        rejected_windows
    ));
    json.push_str(&format!(
        "  \"space_filter\": {{\"recovered_accepted\": {}, \"gate_rejected\": {}, \"other_desktop_accepted\": {}, \"show_other_desktops\": {}}},\n",
        SPACE_RECOVERED_ACCEPTED.load(Ordering::Relaxed),
        SPACE_GATE_REJECTED.load(Ordering::Relaxed),
        OTHER_DESKTOP_ACCEPTED.load(Ordering::Relaxed),
        crate::config::CONFIG
            .read()
            .map(|cfg| cfg.windows.show_other_desktops)
            .unwrap_or(false),
    ));
    let pid_evidence = AX_PID_EVIDENCE.lock().unwrap().clone();
    let as_json = |pids: &[i32]| {
        pids.iter()
            .map(|pid| pid.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    json.push_str(&format!(
        "  \"ax_published_pids\": [{}],\n",
        as_json(&pid_evidence.published)
    ));
    json.push_str(&format!(
        "  \"ax_recovered_pids\": [{}],\n",
        as_json(&pid_evidence.recovered)
    ));
    json.push_str(&format!(
        "  \"ax_failed_pids\": [{}],\n",
        as_json(&pid_evidence.failed)
    ));
    // Copy both records under their locks: count and identity are one consistent snapshot each.
    let (commit_count, commit) = LAST_COMMIT.lock().unwrap().unwrap_or((
        0,
        CommitRecord {
            pid: 0,
            window_id: 0,
            index: 0,
        },
    ));
    // Cloned, not copied: the record now carries a `String` terminal (`OtherDesktopRaise` is no
    // longer `Copy`), and the read must not hold the lock while the JSON is formatted.
    let raise = OTHER_DESKTOP_RAISE.lock().unwrap().clone();
    let (raise_count, raise) = raise.unwrap_or((
        0,
        OtherDesktopRaise {
            pid: 0,
            window_id: 0,
            generation: 0,
            onscreen: false,
            ax_matched: false,
            activation: false,
            rescue_attempted: false,
            rescue: false,
            attempts: 0,
            terminal: String::new(),
            first_rescue_ms: None,
            first_ax_attempt_ms: None,
            first_arrival_ms: None,
            elapsed_ms: 0,
        },
    ));
    json.push_str(&format!(
        "  \"last_commit\": {{\"count\": {}, \"pid\": {}, \"window_id\": {}, \"index\": {}}},\n",
        commit_count, commit.pid, commit.window_id, commit.index
    ));
    json.push_str(&format!(
        "  \"other_desktop_raise\": {{\"count\": {}, \"pid\": {}, \"window_id\": {}, \"generation\": {}, \"onscreen\": {}, \"ax_matched\": {}, \"activation\": {}, \"rescue_attempted\": {}, \"rescue\": {}, \"attempts\": {}, \"terminal\": {}, \"first_rescue_ms\": {}, \"first_ax_attempt_ms\": {}, \"first_arrival_ms\": {}, \"elapsed_ms\": {}}},\n",
        raise_count,
        raise.pid,
        raise.window_id,
        raise.generation,
        raise.onscreen,
        raise.ax_matched,
        raise.activation,
        raise.rescue_attempted,
        raise.rescue,
        raise.attempts,
        json_string(&raise.terminal),
        json_opt_u128(raise.first_rescue_ms),
        json_opt_u128(raise.first_ax_attempt_ms),
        json_opt_u128(raise.first_arrival_ms),
        raise.elapsed_ms,
    ));
    json.push_str(&format!(
        "  \"space_groups\": {{\"displays\": {}, \"confirmed_fullscreen_origins\": {}, \"unknown_active_fullscreen_spaces\": {}, \"evidence_contiguous\": {}, \"source_learning_available\": {}}},\n",
        space_group_state.0,
        space_group_state.1,
        space_group_state.2,
        space_group_state.3,
        space_group_state.4,
    ));
    json.push_str(&format!(
        "  \"membership_source\": \"{}\",\n",
        if SPACE_MEMBERSHIP_SOURCE.load(Ordering::Relaxed) == 1 {
            "skylight"
        } else {
            "legacy"
        }
    ));
    json.push_str(&format!(
        "  \"space_transition\": {{\"in_transition\": {}, \"deadline_ms\": {}}},\n",
        SPACE_IN_TRANSITION.load(Ordering::Relaxed),
        SPACE_TRANSITION_DEADLINE_MS.load(Ordering::Relaxed),
    ));
    json.push_str(&format!(
        "  \"tab_repeat\": {{\"steps\": {}, \"throttled\": {}}},\n",
        TAB_REPEAT_STEPS.load(Ordering::Relaxed),
        TAB_REPEAT_THROTTLED.load(Ordering::Relaxed),
    ));
    json.push_str(&format!("  \"selected_index\": {},\n", snapshot.selected));
    json.push_str(&format!("  \"cards_count\": {},\n", snapshot.windows.len()));
    let selected_key = snapshot.windows.get(snapshot.selected);
    match selected_key {
        Some(card) => json.push_str(&format!(
            "  \"selected_key\": {{\"pid\": {}, \"window_id\": {}}},\n",
            card.pid, card.window_id
        )),
        None => json.push_str("  \"selected_key\": null,\n"),
    }
    match committed {
        Some((pid, window_id, ref app, index)) => json.push_str(&format!(
            "  \"committed\": {{\"pid\": {pid}, \"window_id\": {window_id}, \"app\": {}, \"index\": {index}}},\n",
            json_string(app)
        )),
        None => json.push_str("  \"committed\": null,\n"),
    }
    json.push_str(&format!(
        "  \"frontmost\": {{\"app\": {}, \"pid\": {front_pid}}},\n",
        json_string(&front_app)
    ));
    json.push_str(&format!(
        "  \"permissions\": {{\"accessibility\": {}, \"screen_recording\": {}}},\n",
        crate::ffi::has_accessibility_permission(),
        crate::thumbnail::capture_allowed()
    ));
    // Stopping any service must never latch the terminal disable; A2 asserts this stays false
    // after toggling a feature off (a self-inflicted `CGEventTapEnable(false)` pseudo-event).
    json.push_str(&format!(
        "  \"taps\": {{\"user_input_disabled\": {}, \"allowed\": {}, \"switcher_active\": {}}},\n",
        crate::input_monitor::user_input_disabled(),
        crate::input_monitor::taps_allowed(),
        // A scenario that synthesizes events must wait for the switcher tap: a frame can already be
        // on disk while the tap thread is still installing, and events posted then are lost.
        crate::event_monitor::tap_is_active()
    ));
    json.push_str(&format!(
        "  \"selected_sidebar\": {},\n",
        crate::settings::e2e_selected_sidebar()
    ));
    json.push_str("  \"pages\": [");
    for (index, page) in collect_pages().iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            "\n    {{\"root\": {}, \"self\": [{}, {}, {}, {}], \"parent\": [{}, {}, {}, {}], \"grandparent\": [{}, {}, {}, {}], \"clip\": [{}, {}, {}, {}], \"styles\": [{}, {}, {}], \"doc\": [{}, {}, {}]}}",
            json_string(page.root),
            page.self_frame.origin.x,
            page.self_frame.origin.y,
            page.self_frame.size.width,
            page.self_frame.size.height,
            page.parent_frame.origin.x,
            page.parent_frame.origin.y,
            page.parent_frame.size.width,
            page.parent_frame.size.height,
            page.grandparent_frame.origin.x,
            page.grandparent_frame.origin.y,
            page.grandparent_frame.size.width,
            page.grandparent_frame.size.height,
            page.clip_bounds.origin.x,
            page.clip_bounds.origin.y,
            page.clip_bounds.size.width,
            page.clip_bounds.size.height,
            page.style_self,
            page.style_parent,
            page.style_grandparent,
            page.doc_height,
            page.content_top,
            page.content_bottom
        ));
    }
    json.push_str("\n  ],\n");
    json.push_str("  \"views\": [");
    for (index, node) in collect_views().iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            "\n    {{\"root\": {}, \"parent\": {}, \"depth\": {}, \"class\": {}, \"frame\": [{}, {}, {}, {}], \"text\": {}}}",
            json_string(node.root),
            node.parent,
            node.depth,
            json_string(&node.class),
            node.frame.0,
            node.frame.1,
            node.frame.2,
            node.frame.3,
            match &node.text {
                Some(text) => json_string(text),
                None => "null".to_string(),
            }
        ));
    }
    json.push_str("\n  ],\n");
    match thumbnails {
        Some(range) => json.push_str(&format!(
            "  \"thumbnail_range\": [{}, {}],\n",
            range.start, range.end
        )),
        None => json.push_str("  \"thumbnail_range\": null,\n"),
    }
    // The last summon's capturable candidate set: a scenario asserts a card on another desktop was
    // never a capture candidate instead of trusting an eligibility flag.
    let workset = crate::thumbnail::e2e_summon_workset();
    let workset = workset
        .iter()
        .map(|(pid, wid)| format!("[{pid}, {wid}]"))
        .collect::<Vec<_>>()
        .join(", ");
    json.push_str(&format!("  \"thumbnail_workset\": [{workset}],\n"));
    json.push_str("  \"cards\": [");
    for (index, card) in snapshot.windows.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            "\n    {{\"index\": {index}, \"pid\": {}, \"window_id\": {}, \"app\": {}, \"title\": {}, \"active\": {}, \"minimized\": {}, \"fullscreen\": {}, \"other_desktop\": {}, \"thumbnail_ready\": {}, \"thumbnail_rendered\": {}, \"ax_identified\": {}, \"on_screen\": {}, \"bounds\": [{}, {}, {}, {}], \"minimized_source\": {}, \"fullscreen_source\": {}, \"app_hidden_source\": {}, \"ordered_in\": {}, \"ax_pairing\": {}, \"window_server_hidden\": {}, \"ws_attributes\": {}, \"ws_tags\": {}, \"ws_space_type_mask\": {}}}",
            card.pid,
            card.window_id,
            json_string(&card.app),
            json_string(&card.title),
            card.active,
            card.minimized,
            card.fullscreen,
            card.other_desktop,
            card.thumbnail_ready,
            card.thumbnail_rendered,
            card.ax_identified,
            card.on_screen.map_or("null".to_string(), |value| value.to_string()),
            card.bounds.0,
            card.bounds.1,
            card.bounds.2,
            card.bounds.3,
            json_string(card.minimized_source),
            json_string(card.fullscreen_source),
            json_string(card.app_hidden_source),
            card.ordered_in.map_or("null".to_string(), |value| value.to_string()),
            card.ax_pairing.map_or("null".to_string(), json_string),
            card.window_server_hidden
                .map_or("null".to_string(), |value| value.to_string()),
            card.ws_attributes
                .as_deref()
                .map_or("null".to_string(), json_string),
            card.ws_tags.as_deref().map_or("null".to_string(), json_string),
            card.ws_space_type_mask
                .as_deref()
                .map_or("null".to_string(), json_string),
        ));
    }
    json.push_str("\n  ],\n");
    let keystroke = crate::keystroke_display::e2e_snapshot();
    json.push_str(&format!(
        "  \"keystroke_display\": {{\"visible\": {}, \"badges\": [",
        keystroke.visible
    ));
    for (index, badge) in keystroke.badges.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            "{{\"text\": {}, \"kind\": {}, \"repeats\": {}}}",
            json_string(&badge.text),
            json_string(badge.kind),
            badge.repeats
        ));
    }
    json.push_str(&format!(
        "], \"secure_paused\": {}, \"tap_level\": {}}}\n}}\n",
        keystroke.secure_paused,
        json_string(&keystroke.tap_level)
    ));

    write_atomically(path, &json);
}

fn write_atomically(path: &Path, contents: &str) {
    let tmp = path.with_extension("tmp");
    let result = std::fs::File::create(&tmp)
        .and_then(|mut file| {
            file.write_all(contents.as_bytes())?;
            file.sync_all()
        })
        .and_then(|()| std::fs::rename(&tmp, path));
    if let Err(error) = result {
        log_debug!("[e2e-state] write failed for {}: {error}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Window titles and app names are external data, so they must be escaped: a quote in a title
    /// would otherwise corrupt the document.
    #[test]
    fn json_string_escapes_external_text() {
        assert_eq!(json_string("plain"), "\"plain\"");
        assert_eq!(json_string("a\"b"), "\"a\\\"b\"");
        assert_eq!(json_string("a\\b"), "\"a\\\\b\"");
        assert_eq!(json_string("a\nb\tc"), "\"a\\nb\\tc\"");
        assert_eq!(json_string("bell\u{7}"), "\"bell\\u0007\"");
        // Non-ASCII titles stay literal rather than \\u-escaped so scripts can grep them directly.
        assert_eq!(json_string("微信 — 聊天"), "\"微信 — 聊天\"");
    }
}

#[cfg(test)]
mod ax_pid_evidence_tests {
    use super::*;

    /// A directed refresh must move only its own pid's evidence; a full pass replaces both sets.
    #[test]
    fn rejection_reasons_are_published_with_the_accepted_result_only() {
        // A discarded pass must not rewrite what an applied frame's cards carry: stage without
        // publishing and the previous reasons stay.
        begin_collection_pass();
        card_rejected(7, 20, CardRejection::AxExcluded);
        publish_staged_rejections(None);
        assert_eq!(
            *REJECTED_WINDOWS.lock().unwrap(),
            vec![(7, 20, CardRejection::AxExcluded)]
        );
        begin_collection_pass();
        card_rejected(7, 20, CardRejection::Space);
        assert_eq!(
            *REJECTED_WINDOWS.lock().unwrap(),
            vec![(7, 20, CardRejection::AxExcluded)],
            "an unpublished pass leaves the published reasons alone"
        );
        // The accepted pass replaces them: an old `ax_excluded` must never survive a new reason, or a
        // scenario would report the accepted narrowing for a window refused for a real reason.
        publish_staged_rejections(None);
        assert_eq!(
            *REJECTED_WINDOWS.lock().unwrap(),
            vec![(7, 20, CardRejection::Space)]
        );
        // A full pass that recorded nothing still clears the list: its cards are the truth.
        begin_collection_pass();
        publish_staged_rejections(None);
        assert!(REJECTED_WINDOWS.lock().unwrap().is_empty());
    }

    #[test]
    fn a_directed_pass_replaces_only_its_own_pid() {
        begin_collection_pass();
        card_rejected(7, 20, CardRejection::AxExcluded);
        card_rejected(8, 30, CardRejection::Title);
        publish_staged_rejections(None);
        begin_collection_pass();
        card_rejected(7, 20, CardRejection::Space);
        card_rejected(7, 21, CardRejection::Unpaired);
        publish_staged_rejections(Some(7));
        assert_eq!(
            *REJECTED_WINDOWS.lock().unwrap(),
            vec![
                (8, 30, CardRejection::Title),
                (7, 20, CardRejection::Space),
                (7, 21, CardRejection::Unpaired),
            ],
            "pid 8 keeps the reason its card was built from"
        );
    }

    #[test]
    fn evidence_follows_the_pass_kind() {
        let _guard = EVIDENCE_TEST_LOCK.lock().unwrap();
        publish_ax_pid_evidence(Some((vec![1, 2], vec![2], vec![3])), None);
        {
            let known = AX_PID_EVIDENCE.lock().unwrap();
            assert_eq!(known.published, vec![1, 2]);
            assert_eq!(known.recovered, vec![2]);
            assert_eq!(known.failed, vec![3]);
        }

        // Directed pass for pid 1: it published nothing and recovered nothing, so pid 1 leaves
        // those sets while pid 2 keeps the evidence its cards were built from -- and pid 3, which
        // this pass says nothing about, keeps its AX-failed mark (a directed pass only moves its own
        // pid in every list).
        publish_ax_pid_evidence(Some((vec![], vec![], vec![])), Some(1));
        {
            let known = AX_PID_EVIDENCE.lock().unwrap();
            assert_eq!(known.published, vec![2]);
            assert_eq!(known.recovered, vec![2]);
            assert_eq!(known.failed, vec![3]);
        }

        // Directed pass for pid 3 with a published window and no longer AX-failed.
        publish_ax_pid_evidence(Some((vec![3], vec![], vec![])), Some(3));
        {
            let known = AX_PID_EVIDENCE.lock().unwrap();
            assert_eq!(known.published, vec![2, 3]);
            assert_eq!(known.recovered, vec![2]);
            assert!(known.failed.is_empty());
        }

        // A discarded result (None) leaves everything as it was.
        publish_ax_pid_evidence(None, None);
        {
            let known = AX_PID_EVIDENCE.lock().unwrap();
            assert_eq!(known.published, vec![2, 3]);
            assert_eq!(known.recovered, vec![2]);
            assert!(known.failed.is_empty());
        }
        AX_PID_EVIDENCE.lock().unwrap().published.clear();
        AX_PID_EVIDENCE.lock().unwrap().recovered.clear();
        AX_PID_EVIDENCE.lock().unwrap().failed.clear();
    }

    static EVIDENCE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
}
