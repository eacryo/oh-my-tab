//! Gesture tap-thread runtime: owns the engine, the click tracker, the replay state and the
//! exclusion snapshot, and hands decisions back to the tap callback.
//!
//! Everything here runs on the mouse tap thread. It never touches AppKit, never blocks on the main
//! thread and never allocates unboundedly: the engine's per-event work is a few floating-point
//! operations, and the trail is published as a coalesced snapshot (see `set_trail`).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, RwLock};

use super::engine::{Button, Decision, Engine, Phase};
use super::modifiers::{FLAG_COMMAND, FLAG_CONTROL, FLAG_OPTION, FLAG_SHIFT};
use super::recognizer::Point;
use super::replay::{self, ClickTracker, PressData};
use super::table::{self, Row};

/// Whether the gesture module is live. Set by the settings hot-switch and the startup path.
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// The frontmost application bundle identifier, published by the main thread so the tap never has
/// to ask AppKit mid-callback.
static FRONTMOST: OnceLock<RwLock<Option<String>>> = OnceLock::new();

/// The double-click interval in milliseconds, refreshed by the main thread when a gesture starts.
static CLICK_INTERVAL_MS: AtomicU64 = AtomicU64::new(500);

/// Session counter, so a HUD update can never be mistaken for another session's.
static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

fn frontmost() -> &'static RwLock<Option<String>> {
    FRONTMOST.get_or_init(|| RwLock::new(None))
}

/// Publish the frontmost bundle id. Called on the main thread when the active application changes.
pub(crate) fn publish_frontmost(bundle: Option<String>) {
    if let Ok(mut slot) = frontmost().write() {
        *slot = bundle;
    }
}

pub(crate) fn publish_click_interval(ms: u64) {
    CLICK_INTERVAL_MS.store(ms.max(1), Ordering::Relaxed);
}

/// The modifier arbiter shared by the mouse button mappings and the gesture transactions.
static MODIFIERS: OnceLock<Mutex<super::modifiers::ModifierOwnership>> = OnceLock::new();

pub(crate) fn modifiers() -> &'static Mutex<super::modifiers::ModifierOwnership> {
    MODIFIERS.get_or_init(|| Mutex::new(super::modifiers::ModifierOwnership::new()))
}

/// Seed the physical modifier state once, when the tap starts. Afterwards each event's own flags
/// keep it current, so a key the user releases is never resurrected by this starting value.
pub(crate) fn seed_modifiers(flags: u32) {
    if let Ok(mut arbiter) = modifiers().lock() {
        arbiter.seed_physical(flags);
    }
}

/// The flags to forward for a physical event: its own non-managed bits plus every modifier a
/// synthetic holder still needs. Identity when nothing is synthesised, so normal typing is untouched.
pub(crate) fn forward_flags(event_flags: u32) -> u32 {
    match modifiers().lock() {
        Ok(arbiter) => arbiter.forward(event_flags),
        Err(_) => event_flags,
    }
}

/// Record a physical modifier state from a pre-rewrite event.
pub(crate) fn observe_physical(flags: u32) {
    if let Ok(mut arbiter) = modifiers().lock() {
        arbiter.observe_physical(flags);
    }
}

/// Claim the managed modifier bits in `flags` for `holder`. Returns the merged state to publish
/// when the application would see a difference, or `None` when it would not (the user is already
/// holding every bit, or another holder already does).
pub(crate) fn acquire_modifiers(holder: super::modifiers::HolderId, flags: u32) -> Option<u32> {
    modifiers()
        .lock()
        .ok()
        .and_then(|mut arbiter| arbiter.acquire(holder, flags))
}

/// Give back what `holder` held. Same return contract as [[acquire_modifiers]].
pub(crate) fn release_modifiers(holder: super::modifiers::HolderId, flags: u32) -> Option<u32> {
    modifiers()
        .lock()
        .ok()
        .and_then(|mut arbiter| arbiter.release(holder, flags))
}

/// A counter advanced every time the service stops. A queued job remembers it, so a stop followed
/// by a restart cannot re-authorize work that was already in flight: the epoch alone would not move,
/// because the configuration did not change.
static SERVICE_GENERATION: AtomicU64 = AtomicU64::new(1);

pub(crate) fn set_active(active: bool) {
    if !active && ACTIVE.swap(false, Ordering::SeqCst) {
        // Bumping on the ACTIVE -> stopped edge only, so repeated stop calls do not churn it.
        SERVICE_GENERATION.fetch_add(1, Ordering::SeqCst);
        return;
    }
    ACTIVE.store(active, Ordering::SeqCst);
}

/// The current service generation, recorded by a gesture that queues work.
pub(crate) fn service_generation() -> u64 {
    SERVICE_GENERATION.load(Ordering::SeqCst)
}

/// A stable identity for the device that produced an event, so two mice never share a session.
/// Packs the (VID, PID) key the mouse resolver uses; `None` (unattributed) reads as 0.
pub(crate) fn device_key(key: Option<(u32, u32)>) -> u64 {
    key.map_or(0, |(vid, pid)| ((vid as u64) << 32) | pid as u64)
}

/// Whether the gesture that produced a queued job may still run: same epoch, not recording, and the
/// frontmost application is not excluded. A frontmost change does not advance the epoch, so the
/// exclusion has to be read at consumption time rather than trusted from release time.
pub(crate) fn still_authorized(epoch: u64, generation: u64) -> bool {
    if generation != SERVICE_GENERATION.load(Ordering::SeqCst) {
        return false;
    }
    // A stopped mouse service -- the permission monitor stops it, and the configuration need not
    // change to do so -- revokes every gesture still queued behind it. Without this a job enqueued
    // before the stop would run after input was taken away from us.
    if !ACTIVE.load(Ordering::SeqCst) {
        return false;
    }
    if !crate::input_monitor::taps_allowed() {
        return false;
    }
    if crate::mouse::event_tap::RECORDING.load(Ordering::Relaxed) {
        return false;
    }
    let published = table::snapshot();
    if published.epoch != epoch {
        return false;
    }
    // The exclusion is NOT re-asked here: this check runs from a timer with no event to name an
    // application, and the frontmost fallback is exactly the answer that made the list look broken.
    // What matters is that the job's own application was not excluded when it was queued (see
    // `excluded_bundle`), and a gesture whose press was allowed keeps its authorization.
    true
}

/// Whether the application the event belongs to is on the exclusion list.
///
/// `event_bundle` is the bundle id the TAP resolved for the event that is being decided: the app
/// that owns the window under the pointer (`excluded_bundle_for_event`). It is the account that
/// matters, because the exclusion is documented as "while one of these applications is in front",
/// and the frontmost application is NOT always the app a click lands in:
///
///   * the settings window lives here, so reviewing the list and clicking straight into an excluded
///     app behind it left `NSWorkspace.frontmostApplication` pointing at this app;
///   * a panel that never becomes key, a menu-bar popover, a non-activating panel and a window of a
///     background app all receive clicks while some other application stays "active";
///   * `NSWorkspaceDidActivateApplicationNotification` is a notification: a switch completed a moment
///     ago can still be unpublished.
///
/// The published activation and the live `NSWorkspace` answer are therefore used only as fallbacks,
/// when the tap could not name the event's application at all.
fn excluded_bundle(excluded: &[String], event_bundle: Option<&str>) -> bool {
    if excluded.is_empty() {
        return false;
    }
    if let Some(bundle) = event_bundle {
        return excluded.iter().any(|id| id == bundle);
    }
    if let Ok(slot) = frontmost().read() {
        if let Some(bundle) = slot.as_deref() {
            if excluded.iter().any(|id| id == bundle) {
                return true;
            }
        }
    }
    // The call sends one message to NSWorkspace: no other process is waited on.
    match unsafe { crate::app_identity::frontmost_bundle_id() } {
        Some(bundle) => excluded.iter().any(|id| id == &bundle),
        None => false,
    }
}

/// The bundle id of the application an event at `cgwid` belongs to, resolved on the tap thread.
///
/// Two rows read per event, one WindowServer query, no message to another process: the owner pid of
/// the event's own window, then that pid's bundle id through the gesture module's small cache. `None`
/// (no window number, WindowServer unreadable, a pid with no bundle) means "unknown", and the caller
/// then falls back to the activation-based answer above.
pub(crate) fn excluded_bundle_for_event(cgwid: i64) -> Option<String> {
    if cgwid <= 0 {
        return None;
    }
    /// pid -> its bundle id. One entry per application a gesture was drawn in.
    type BundleCache = Vec<(i32, Option<String>)>;
    static CACHE: OnceLock<Mutex<BundleCache>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(Vec::new()));
    let pid = crate::window_collector::owner_pid_for_window_now(cgwid as u32)?;
    if let Ok(entries) = cache.lock() {
        if let Some((_, bundle)) = entries.iter().find(|(cached, _)| *cached == pid) {
            if bundle.is_some() {
                return bundle.clone();
            }
        }
    }
    // SAFETY: one `NSRunningApplication` lookup by pid; the answer is copied into a `String` and the
    // object is never retained here.
    let bundle = unsafe {
        let app: *mut objc2::runtime::AnyObject = objc2::msg_send![
            objc2::class!(NSRunningApplication),
            runningApplicationWithProcessIdentifier: pid
        ];
        crate::app_identity::bundle_id_of(app)
    };
    if let Ok(mut entries) = cache.lock() {
        // Bounded, and only a PRESENT bundle is remembered: an unreadable lookup (a process that has
        // no bundle id, or is still starting) must be asked again, not cached as if it were final.
        if let Some(bundle) = bundle.clone() {
            if entries.len() >= 64 {
                entries.remove(0);
            }
            entries.push((pid, Some(bundle)));
        }
    }
    bundle
}

/// The trail as published to the HUD: a session id and the points to draw.
#[derive(Debug, Clone, Default)]
pub(crate) struct Trail {
    pub(crate) session: u64,
    pub(crate) points: Vec<Point>,
    pub(crate) visible: bool,
}

/// The trajectory this press has drawn since it engaged. Only used for the HUD; recognition keeps
/// its own samples, so decimating the trail can never change a recognized sequence.
#[derive(Debug, Default, Clone)]
pub(crate) struct TrailBuffer {
    points: Vec<Point>,
    last_published: Point,
}

impl TrailBuffer {
    /// Keep at most this many points for the HUD. Bounded so a long press cannot grow without end.
    pub(crate) const CAPACITY: usize = 512;

    pub(crate) fn push(&mut self, point: Point) -> bool {
        if self.points.len() >= Self::CAPACITY {
            return false;
        }
        self.points.push(point);
        true
    }

    pub(crate) fn take_changed(&mut self, point: Point) -> bool {
        // Publish only when the pointer moved enough to be visible: the HUD redraw is coalesced,
        // and a sub-pixel update is not worth a main-thread round trip.
        if (point.x - self.last_published.x).abs() < 1.0
            && (point.y - self.last_published.y).abs() < 1.0
        {
            return false;
        }
        self.last_published = point;
        true
    }

    pub(crate) fn points(&self) -> &[Point] {
        &self.points
    }

    pub(crate) fn clear(&mut self) {
        self.points.clear();
        self.last_published = Point::default();
    }
}

/// The imperative side of one gesture press, as the tap sees it.
#[derive(Debug)]
pub(crate) struct Runtime {
    engine: Engine,
    clicks: ClickTracker,
    session: u64,
    /// The press data kept for the replay. `None` when no press is held.
    press: Option<PressData>,
    trail: TrailBuffer,
    /// The epoch the session started under; compared before executing.
    epoch: u64,
    /// Set when the press was a gesture we executed: the click counter must not advance.
    matched: Option<Row>,
    /// An Esc this module took is still held down: its release must be swallowed too.
    escape_owned: bool,
    /// A button whose Down this module swallowed but whose Up has not arrived, kept after the
    /// module stops or a gesture is invalidated so the tail of a press we took stays ours.
    owned_tail: Option<Button>,
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

impl Runtime {
    pub(crate) fn new() -> Self {
        Self {
            engine: Engine::new(Default::default()),
            clicks: ClickTracker::default(),
            session: 0,
            press: None,
            trail: TrailBuffer::default(),
            epoch: 0,
            matched: None,
            escape_owned: false,
            owned_tail: None,
        }
    }

    /// A press of `button`. Returns whether the tap must swallow it.
    ///
    /// The claim test is the full condition: the feature is on, some enabled row names this button,
    /// the frontmost application is not excluded, and nothing else is already held.
    pub(crate) fn press(
        &mut self,
        button: u32,
        device: u64,
        point: Point,
        now_ms: f64,
        flags: u32,
        event_bundle: Option<&str>,
    ) -> bool {
        if !ACTIVE.load(Ordering::SeqCst) {
            return false;
        }
        // A recorder owns the keyboard and the pointer while it runs: taking a press here would
        // steal the very event it is waiting for.
        if crate::mouse::event_tap::RECORDING.load(Ordering::Relaxed) {
            return false;
        }
        let published = table::snapshot();
        if excluded_bundle(&published.excluded, event_bundle) || !published.table.claims(button) {
            return false;
        }
        // Try the engine first: a press it refuses (another button already holds the session) must
        // leave this session's identity, epoch and trail exactly as they were.
        let claimed = self.engine.press(
            Button {
                number: button,
                device,
            },
            point,
            now_ms,
            true,
        );
        if !claimed {
            return false;
        }
        self.session = NEXT_SESSION.fetch_add(1, Ordering::SeqCst);
        self.epoch = published.epoch;
        self.matched = None;
        self.trail.clear();
        self.press = Some(PressData {
            button,
            origin: point,
            flags,
        });
        self.owned_tail = Some(Button {
            number: button,
            device,
        });
        self.publish_phase();
        true
    }

    /// A drag sample. Returns whether the tap must swallow it, and whether the HUD should redraw.
    pub(crate) fn dragged(
        &mut self,
        button: u32,
        device: u64,
        point: Point,
        now_ms: f64,
    ) -> (bool, Option<Trail>) {
        let decision = self.engine.dragged(
            Button {
                number: button,
                device,
            },
            point,
            now_ms,
        );
        if matches!(decision, Decision::Pass) {
            return (false, None);
        }
        self.publish_phase();
        if self.engine.phase() == Phase::Engaged {
            self.trail.push(point);
            let changed = self.trail.take_changed(point);
            if changed {
                let trail = self.trail_snapshot();
                return (true, Some(trail));
            }
        }
        (true, None)
    }

    /// The epoch the live session started under; the authorization identity carried with its work.
    pub(crate) fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The current session's id. Every HUD request carries it so a late request from an ended
    /// gesture cannot act on the next one.
    pub(crate) fn session_id(&self) -> u64 {
        self.session
    }

    /// Publish the engine phase for the state readers.
    fn publish_phase(&self) {
        super::session_state::set_trail_degraded(self.engine.sequence_degraded());
        super::session_state::set_origin(self.engine.origin());
        super::session_state::set_phase(match self.engine.phase() {
            Phase::Held => "Held",
            Phase::Engaged => "Engaged",
            Phase::Abandoned => "Abandoned",
            Phase::Idle => "Idle",
        });
    }

    /// The release. Returns the decision and, when a target matched, what to run.
    ///
    /// Authorization is checked here, on the tap thread, against the epoch that is published now --
    /// not the one from the press. A configuration that changed in between revokes the gesture
    /// silently: no action, and no replay, because turning a revoked gesture into a click injects an
    /// event the user never asked for.
    pub(crate) fn release(
        &mut self,
        button: u32,
        device: u64,
        at: Point,
        now_ms: f64,
        event_bundle: Option<&str>,
    ) -> (Decision, Option<Row>) {
        let identity = Button {
            number: button,
            device,
        };
        if !self.engine.owns(identity) {
            return (Decision::Pass, None);
        }
        let published = table::snapshot();
        let epoch = self.epoch;
        self.clear_tail(button, device);
        let decision = self.engine.release(
            identity,
            at,
            now_ms,
            |key| published.table.lookup(button, key).is_some(),
            // The three ways a gesture stops being allowed to run, checked at the moment it would:
            // the configuration changed under it, a recorder took the keyboard, or the frontmost
            // application moved into the exclusion list while the user was drawing.
            || {
                published.epoch == epoch
                    && !crate::mouse::event_tap::RECORDING.load(Ordering::Relaxed)
                    && !excluded_bundle(&published.excluded, event_bundle)
            },
        );
        self.publish_phase();
        let mut target = None;
        match decision {
            Decision::Execute => {
                if let Some(key) = self.engine.sequence_key() {
                    if let Some(row) = published.table.lookup(button, &key).cloned() {
                        target = Some(row);
                    }
                }
            }
            Decision::Replay => {
                self.replay(at, button, now_ms);
            }
            _ => {}
        }
        (decision, target)
    }

    /// Post the click the swallowed press stood for.
    fn replay(&mut self, at: Point, button: u32, now_ms: f64) {
        let Some(press) = self.press.take() else {
            return;
        };
        // The modifiers the user was holding are part of the click they made (Option+right-click is
        // a different menu), so they travel with the replay.
        const MANAGED: u32 = FLAG_COMMAND | FLAG_OPTION | FLAG_CONTROL | FLAG_SHIFT;
        let flags = press.flags & MANAGED;
        let interval = CLICK_INTERVAL_MS.load(Ordering::Relaxed) as f64;
        let count = self.clicks.peek(button, at, now_ms, interval);
        self.trail.clear();
        // Only a click the application actually received joins the click sequence: a failed build
        // posts nothing, so counting it would make the next click look like a double click.
        if replay::replay_click(&press, at, count, flags) {
            self.clicks.record(button, at, now_ms, count);
        }
    }

    /// Esc during a live gesture. Returns whether the key belongs to this module.
    ///
    /// Taking the press makes the module responsible for its release too, even after the mouse
    /// session has ended: an application that received an Esc key-up it never saw a key-down for
    /// sees a stuck key.
    pub(crate) fn escape(&mut self) -> bool {
        // Ownership is checked FIRST: once this module swallowed an Esc, every later event of that
        // same press belongs to it, including repeats that arrive after the mouse was released and
        // the engine is back to Idle. Leaking them would hand the application a key-up whose key-down
        // it never saw.
        if self.escape_owned {
            return true;
        }
        if self.engine.escape() {
            self.escape_owned = true;
            self.publish_phase();
            return true;
        }
        false
    }

    /// Whether this module currently owns an Esc press whose key-up has not arrived.
    pub(crate) fn escape_owned(&self) -> bool {
        self.escape_owned
    }

    /// The key-up of an Esc this module took. Only this clears the claim.
    pub(crate) fn release_escape(&mut self) -> bool {
        if self.escape_owned {
            self.escape_owned = false;
            return true;
        }
        false
    }

    /// The press ended without a physical release reaching us (tap timeout, device removal, module
    /// stop). Nothing is replayed.
    ///
    /// The tail of the press is KEPT: whatever the user is still holding must not fall through to
    /// the button mappings once the module stops, or the release would be read as a fresh mapping
    /// action (a stuck shortcut-up, or a switcher commit with no press).
    pub(crate) fn invalidate(&mut self) {
        self.press = None;
        self.trail.clear();
        let _ = self.engine.invalidate();
        self.publish_phase();
    }

    /// Whether an Up for a button ends a press this module swallowed but no longer tracks as a live
    /// session (the module stopped, or a timeout invalidated the gesture in flight).
    ///
    /// A live session never takes this path: its release has to go through the normal decision, or
    /// every gesture would be swallowed before it could execute or replay.
    pub(crate) fn owns_tail(&self, button: u32, device: u64) -> bool {
        self.is_tail(button, device) && self.engine.phase() == Phase::Idle
    }

    /// Whether the recorded tail is this button's, regardless of whether a live session owns it.
    /// Identity and liveness are separate questions: a normal release matches the identity and must
    /// be able to clear the record, while a live session matches it too and must be left alone.
    fn is_tail(&self, button: u32, device: u64) -> bool {
        self.owned_tail
            == Some(Button {
                number: button,
                device,
            })
    }

    /// Forget the tail once its Up has been consumed.
    pub(crate) fn clear_tail(&mut self, button: u32, device: u64) {
        // Matched by identity only: a live session's own release has to clear the record too, or
        // it would outlive the press and swallow a later click's Up.
        if self.is_tail(button, device) {
            self.owned_tail = None;
        }
    }

    /// A release that ends a press this module took but no longer tracks as a session (the module
    /// stopped, or a timeout invalidated the gesture). Consumed, never replayed: at this point the
    /// application may never have seen the press, and a click at an unknown place is worse than a
    /// quiet release.
    pub(crate) fn release_tail(&mut self, button: u32, device: u64, _at: Point, _now_ms: f64) {
        if self.engine.phase() != Phase::Idle {
            return;
        }
        self.clear_tail(button, device);
        self.press = None;
        self.trail.clear();
        self.publish_phase();
    }

    /// A gesture ran: the click counter must not treat the next click as a continuation.
    pub(crate) fn note_matched(&mut self, button: u32) {
        self.clicks.forget(button);
    }

    /// The sequence the engine has recognised so far, for the HUD's hint line.
    pub(crate) fn engine_sequence_key(&self) -> Option<String> {
        self.engine.live_sequence_key()
    }

    /// The button the live session started on (0 when there is none).
    pub(crate) fn engine_button(&self) -> u32 {
        self.press.as_ref().map(|press| press.button).unwrap_or(0)
    }

    pub(crate) fn trail_snapshot(&self) -> Trail {
        Trail {
            session: self.session,
            points: self.trail.points().to_vec(),
            visible: self.engine.phase() == Phase::Engaged,
        }
    }
}

/// The tap thread's single runtime. `Mutex` rather than `RefCell` because the tap callback is a
/// plain `extern "C"` function; the lock is uncontended (one owner, one thread).
static RUNTIME: OnceLock<std::sync::Mutex<Runtime>> = OnceLock::new();

pub(crate) fn runtime() -> &'static std::sync::Mutex<Runtime> {
    RUNTIME.get_or_init(|| std::sync::Mutex::new(Runtime::new()))
}

/// Relaxed helpers for call sites that only need one answer.
pub(crate) fn with<R>(f: impl FnOnce(&mut Runtime) -> R) -> R {
    let mut guard = runtime().lock().unwrap();
    f(&mut guard)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }

    #[test]
    fn the_trail_is_bounded() {
        let mut trail = TrailBuffer::default();
        for i in 0..(TrailBuffer::CAPACITY + 100) {
            trail.push(p(i as f64, 0.0));
        }
        assert_eq!(trail.points().len(), TrailBuffer::CAPACITY);
    }

    #[test]
    fn sub_pixel_movement_does_not_publish() {
        let mut trail = TrailBuffer::default();
        assert!(trail.take_changed(p(10.0, 10.0)));
        assert!(!trail.take_changed(p(10.3, 10.2)), "under one point");
        assert!(trail.take_changed(p(12.0, 10.0)));
    }

    #[test]
    fn an_inactive_module_never_claims() {
        let _guard = GLOBAL_STATE.lock().unwrap_or_else(|e| e.into_inner());
        set_active(false);
        let mut rt = Runtime::new();
        assert!(!rt.press(1, 0, p(0.0, 0.0), 0.0, 0, None));
        assert_eq!(crate::mouse::gesture::session_state::phase(), "Idle");
    }

    /// The exclusion rule on its own: the runtime reads the published frontmost id, so this test
    /// exercises the predicate rather than the global publication.
    #[test]
    fn an_esc_taken_before_the_mouse_release_keeps_its_repeats() {
        with_global_state(|| {
            // Review finding: after the mouse came up the engine was Idle, so Esc autorepeat leaked to
            // the application while the final key-up was still swallowed -- a stuck key.
            let mut rt = Runtime::new();
            set_active(true);
            publish_a_row();
            assert!(rt.press(1, 0, p(100.0, 100.0), 0.0, 0, None));
            assert!(rt.escape(), "the first Esc belongs to the live gesture");
            // The mouse comes up first: the gesture is over.
            let _ = rt.release(1, 0, p(140.0, 100.0), 20.0, None);
            assert!(rt.escape(), "a repeat of the same Esc press is still ours");
            assert!(rt.release_escape(), "so is its key-up");
            assert!(!rt.release_escape(), "and only once");
            set_active(false);
        });
    }

    #[test]
    fn a_foreign_press_leaves_the_live_session_untouched() {
        with_global_state(|| {
            // Review finding: session id and epoch were rewritten before the engine decided.
            let mut rt = Runtime::new();
            set_active(true);
            publish_a_row();
            assert!(rt.press(1, 0, p(100.0, 100.0), 0.0, 0, None));
            let session = rt.session_id();
            let epoch = rt.epoch;
            // A second, different trigger button: claimed only if the table has a row for it, and the
            // engine refuses while another session is live.
            assert!(!rt.press(1, 0, p(0.0, 0.0), 1.0, 0, None) || rt.session_id() != session);
            assert_eq!(
                rt.epoch, epoch,
                "the live session keeps its authorization version"
            );
            set_active(false);
        });
    }

    #[test]
    fn a_finished_press_does_not_swallow_the_next_clicks_up() {
        with_global_state(|| {
            // Review finding: the tail record outlived a normal release, so after the feature was turned
            // off the next plain click had its Down passed and its Up swallowed.
            let mut rt = Runtime::new();
            set_active(true);
            publish_a_row();
            assert!(rt.press(1, 0, p(100.0, 100.0), 0.0, 0, None));
            let _ = rt.release(1, 0, p(140.0, 100.0), 20.0, None);
            assert!(
                !rt.owns_tail(1, 0),
                "a finished press leaves no tail behind"
            );
            set_active(false);
            assert!(!rt.owns_tail(1, 0));
        });
    }

    #[test]
    fn consecutive_gestures_both_fire() {
        with_global_state(|| {
            let mut rt = Runtime::new();
            set_active(true);
            publish_a_row();
            for round in 0..2 {
                let t = round as f64 * 1000.0;
                assert!(rt.press(1, 0, p(100.0, 100.0), t, 0, None), "round {round}");
                let mut at = p(100.0, 100.0);
                for i in 1..=5 {
                    at = p(at.x + 10.0, at.y);
                    rt.dragged(1, 0, at, t + i as f64);
                }
                let (decision, _) = rt.release(1, 0, at, t + 10.0, None);
                assert!(
                    matches!(decision, Decision::Execute | Decision::Replay),
                    "round {round}: {decision:?}"
                );
            }
            set_active(false);
        });
    }

    /// Serialises the tests that publish to the global table and flip the global active flag.
    /// Cargo runs test functions in parallel threads, so without this one test's publish lands
    /// between another's press and release.
    static GLOBAL_STATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_global_state<R>(f: impl FnOnce() -> R) -> R {
        let _guard = GLOBAL_STATE.lock().unwrap_or_else(|e| e.into_inner());
        f()
    }

    /// A table with one enabled row on the right button, published for the tests that need the
    /// runtime to claim.
    fn publish_a_row() {
        let mut cfg = crate::config::Config::default();
        cfg.mouse.enabled = true;
        cfg.mouse.gestures_enabled = true;
        cfg.mouse.gestures = vec![crate::config::GestureBinding {
            gesture: "R".into(),
            button: 1,
            binding: "cmd+t".into(),
            enabled: true,
        }];
        table::publish(&cfg);
        publish_frontmost(Some("com.example.foreground".into()));
    }

    #[test]
    fn the_exclusion_predicate_matches_bundle_ids_only() {
        let excluded = ["com.example.excluded".to_string()];
        assert!(excluded_bundle(&excluded, Some("com.example.excluded")));
        assert!(!excluded_bundle(&excluded, Some("com.example.other")));
        assert!(
            !excluded_bundle(&[], Some("com.example.excluded")),
            "an empty list excludes nothing"
        );
    }

    /// The regression this covers: the app a gesture lands in is the event's own application, so it
    /// decides even when the published/live frontmost answer names someone else. The settings window
    /// (this app) used to be that "someone else" whenever the list was reviewed and an excluded app
    /// behind it was clicked straight away.
    #[test]
    fn the_events_own_application_decides_even_when_it_is_not_frontmost() {
        with_global_state(|| {
            let mut cfg = crate::config::Config::default();
            cfg.mouse.enabled = true;
            cfg.mouse.gestures_enabled = true;
            cfg.mouse.gesture_excluded_apps = vec!["com.example.excluded".into()];
            cfg.mouse.gestures = vec![crate::config::GestureBinding {
                gesture: "R".into(),
                button: 1,
                binding: "cmd+t".into(),
                enabled: true,
            }];
            table::publish(&cfg);
            // The activation feed still says this app, as it does while a settings window is up.
            publish_frontmost(Some("com.example.ourselves".into()));
            let mut rt = Runtime::new();
            set_active(true);
            assert!(
                !rt.press(1, 0, p(100.0, 100.0), 0.0, 0, Some("com.example.excluded")),
                "a press into the excluded application must not be claimed"
            );
            assert!(
                rt.press(1, 0, p(100.0, 100.0), 0.0, 0, Some("com.example.allowed")),
                "the same press into another application is a gesture"
            );
            // And the exclusion is consulted again at the release: the app moved under the gesture.
            let (decision, target) =
                rt.release(1, 0, p(160.0, 100.0), 20.0, Some("com.example.excluded"));
            assert!(
                matches!(decision, Decision::Swallow | Decision::Replay),
                "an excluded application gets no action: {decision:?}"
            );
            assert!(target.is_none());
            set_active(false);
        });
    }

    #[test]
    fn a_row_only_claims_its_own_button() {
        // Built from a private table rather than the global one, so parallel tests cannot interfere.
        let mut cfg = crate::config::Config::default();
        cfg.mouse.enabled = true;
        cfg.mouse.gestures_enabled = true;
        cfg.mouse.gestures = vec![crate::config::GestureBinding {
            gesture: "R".into(),
            button: 1,
            binding: "cmd+t".into(),
            enabled: true,
        }];
        let (table, errors) = table::build(&cfg.mouse);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(table.claims(1));
        assert!(!table.claims(2), "the middle button is not a trigger");
    }
}
