//! Opt-in keystroke display. Never log key text; the explicit e2e-state file includes badges only
//! when requested by `--e2e-state`.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI8, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Instant;

use crate::config::{Config, KeystrokeDisplaySection};
use crate::event_tap::TapThreadControl;
use hover::PanelHoverProtection;
use state::{BadgeKind, DisplayMode, KeyGlyph, StateMachine};

mod hover;
pub(crate) mod mapping;
mod panel;
mod secure;
mod state;
mod tap;

pub(crate) use state::Input;

/// Show the panel with virtual keycaps at `percent` while the settings cap slider is dragged.
/// Idempotent, writes no configuration, and does not touch the real event stream.
pub(crate) fn begin_max_extent_preview(percent: u32) {
    crate::debug_assert_main_thread();
    panel::begin_max_extent_preview(percent);
}

/// End the cap preview and hand the panel back to the real stream.
pub(crate) fn end_max_extent_preview() {
    crate::debug_assert_main_thread();
    panel::end_max_extent_preview();
}

/// Repaint the panel surface from the current palette (see `panel::apply_glass_properties`).
pub(crate) unsafe fn apply_glass_properties() {
    panel::apply_glass_properties();
}

pub(crate) unsafe fn apply_backdrop_material() {
    panel::apply_backdrop_material();
}

/// Repaint and retranslate the key panel's cached controls (the settings gear) after a theme or
/// locale change. The panel window is reused, so without this the gear keeps the old language and a
/// stale tint.
pub(crate) unsafe fn refresh_cached_controls() {
    panel::refresh_cached_controls();
}

/// A2 geometry for the key panel: the panel rect and its trailing settings gear, both in screen
/// points with a top-left origin. A named struct rather than an 8-tuple: two of the fields are the
/// same width/height pair, which a tuple lets a caller transpose silently.
#[derive(Clone, Copy, Debug)]
pub(crate) struct E2ePanelGeometry {
    pub(crate) panel_x: f64,
    pub(crate) panel_y: f64,
    pub(crate) panel_w: f64,
    pub(crate) panel_h: f64,
    pub(crate) gear_x: f64,
    pub(crate) gear_y: f64,
    pub(crate) gear_w: f64,
    pub(crate) gear_h: f64,
}

/// A2: the key panel's rect and its trailing settings gear, in screen points with a top-left origin
/// (the convention the clipboard picker's A2 state uses). AX cannot express a floating panel's
/// subview frame, so the app states the geometry instead of a scenario deriving it.
pub(crate) fn e2e_geometry() -> Option<E2ePanelGeometry> {
    crate::debug_assert_main_thread();
    panel::e2e_geometry()
}

/// Development helper (--show-keystroke-display): seed a visible key panel without a real keystroke,
/// so an A2 scenario can deliver a pointer gesture to its trailing settings gear. The panel
/// otherwise only appears from a key event, which a script cannot produce without stealing focus.
/// start() honours --keystroke-display-force-on, which the scenario passes.
pub(crate) fn show_for_development() {
    crate::debug_assert_main_thread();
    start();
    // Hold the panel open for the scenario's pointer gesture: without this it fades 1.8s after
    // the seeded key below, which is correct behaviour but removes the control under test.
    STATE.with(|state| state.borrow_mut().set_hold_open(true));
    let now = Instant::now();
    let mode =
        DisplayMode::from_config(&crate::config::CONFIG.read().unwrap().keystroke_display.mode);
    STATE.with(|state| {
        state.borrow_mut().apply(
            Input::KeyDown {
                keycode: crate::event_tap::keyboard::VK_SPACE,
                flags: 0,
                autorepeat: false,
                unicode: " ".to_string(),
                glyph: state::KeyGlyph::EventUnicode,
            },
            mode,
            now,
        );
    });
    panel::start_timer();
}

const EVENT_QUEUE_CAPACITY: usize = 256;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static WAKE_PENDING: AtomicBool = AtomicBool::new(false);
static SECURE_ACTIVE_HINT: AtomicBool = AtomicBool::new(false);
static SECURE_STATE_PENDING: AtomicI8 = AtomicI8::new(-1);
static TAP_CONTROL: TapThreadControl = TapThreadControl::new();
struct TapWorker {
    location: i32,
    handle: JoinHandle<()>,
}
static TAP_THREAD: OnceLock<Mutex<Option<TapWorker>>> = OnceLock::new();
static EVENT_QUEUE: OnceLock<Mutex<VecDeque<Input>>> = OnceLock::new();

thread_local! {
    static STATE: RefCell<StateMachine> = RefCell::new(StateMachine::default());
    static PANEL_HOVER: RefCell<PanelHoverProtection> = RefCell::new(PanelHoverProtection::default());
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct E2eBadge {
    pub(crate) text: String,
    pub(crate) kind: &'static str,
    pub(crate) repeats: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct E2eSnapshot {
    pub(crate) visible: bool,
    pub(crate) badges: Vec<E2eBadge>,
    pub(crate) secure_paused: bool,
    pub(crate) tap_level: String,
}

pub(crate) fn is_active() -> bool {
    ACTIVE.load(Ordering::SeqCst)
}

pub(super) fn tap_control() -> &'static TapThreadControl {
    &TAP_CONTROL
}

/// Called by the event tap and secure monitor. The queue is bounded and the bridge schedules only
/// one main-thread wake for a batch, never one AppKit dispatch per keystroke.
pub(super) fn enqueue(input: Input) {
    if !is_active() {
        return;
    }
    let queue = EVENT_QUEUE.get_or_init(|| Mutex::new(VecDeque::new()));
    let should_wake = {
        let mut events = queue.lock().unwrap_or_else(|error| error.into_inner());
        if !is_active() {
            return;
        }
        match input {
            Input::SecureActive(active) => {
                SECURE_ACTIVE_HINT.store(active, Ordering::Release);
                SECURE_STATE_PENDING.store(i8::from(active), Ordering::Release);
            }
            input => {
                if SECURE_ACTIVE_HINT.load(Ordering::Acquire) {
                    return;
                }
                if events.len() == EVENT_QUEUE_CAPACITY {
                    events.pop_front();
                }
                events.push_back(input);
            }
        }
        !WAKE_PENDING.swap(true, Ordering::AcqRel)
    };
    if should_wake {
        crate::enqueue_global_event(crate::event_monitor::GlobalEvent::KeystrokeDisplayWake);
    }
}

fn drain_events() -> Vec<Input> {
    let queue = EVENT_QUEUE.get_or_init(|| Mutex::new(VecDeque::new()));
    let mut events = queue.lock().unwrap_or_else(|error| error.into_inner());
    let mut drained: Vec<Input> = std::mem::take(&mut *events).into_iter().collect();
    let secure_pending = SECURE_STATE_PENDING.swap(-1, Ordering::AcqRel);
    if secure_pending == 1 {
        drained.insert(0, Input::SecureActive(true));
    } else if secure_pending == 0 {
        drained.push(Input::SecureActive(false));
    }
    WAKE_PENDING.store(false, Ordering::Release);
    drained
}

/// Main-thread entry from the global input aggregator. This starts the 16ms timer only while there
/// is visible content or a fade in progress.
pub(crate) fn wake() {
    crate::debug_assert_main_thread();
    if is_active() {
        panel::start_timer();
    } else {
        reset_on_main();
    }
}

fn process_tick() -> bool {
    crate::debug_assert_main_thread();
    // The preview ends with the pointer gesture, but a lost release (a drag that ended while the
    // app was busy, a synthetic gesture with no mouse-up) must not leave the panel up at a raised
    // level forever. The watchdog is the backstop; the gesture path is the normal exit. Ending it
    // here -- rather than tearing the session down -- lets the ordinary path below resume from the
    // real state, which the preview deliberately never touched.
    if panel::max_extent_preview_active() && panel::max_extent_preview_expired() {
        panel::end_max_extent_preview();
        crate::settings::end_max_extent_preview();
    }
    if panel::max_extent_preview_active() {
        // A preview owns the frame, and it must not replay a key stream the user never saw: key
        // presses are dropped rather than queued. Two inputs are still applied to the REAL state
        // machine, because they describe the session rather than the display, and losing them
        // would leave the machine wrong once the preview ends:
        //   * a secure-input transition -- otherwise the machine stays paused (or stays unpaused)
        //     after the preview, and the secure indicator disagrees with it;
        //   * a modifier transition -- otherwise a modifier released during the drag keeps its
        //     held keycap when the real stream comes back.
        // SECURE_ACTIVE_HINT is deliberately NOT rewritten from these drained events: the enqueue
        // path already maintains it, and a newer transition may have landed on another thread
        // since this drain, which an older event must not overwrite.
        let config = crate::config::CONFIG
            .read()
            .unwrap()
            .keystroke_display
            .clone();
        let mode = DisplayMode::from_config(&config.mode);
        let now = Instant::now();
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            for input in drain_events() {
                if matches!(input, Input::SecureActive(_) | Input::FlagsChanged { .. }) {
                    state.apply(input, mode, now);
                }
            }
        });
        // A drag that is still held keeps its watchdog alive; the timer is what proves the gesture
        // is live, since a paused drag produces no further actions.
        if crate::settings::left_mouse_button_is_down() {
            panel::refresh_max_extent_preview_deadline();
        }
        panel::render_preview_tick();
        return true;
    }
    if !is_active() {
        reset_on_main();
        return false;
    }

    let config = crate::config::CONFIG
        .read()
        .unwrap()
        .keystroke_display
        .clone();
    let mode = DisplayMode::from_config(&config.mode);
    let mut events = drain_events();
    let mut secure = SECURE_ACTIVE_HINT.load(Ordering::Acquire)
        || STATE.with(|state| state.borrow().secure_paused());
    for input in &mut events {
        match input {
            Input::SecureActive(active) => secure = *active,
            Input::KeyDown {
                keycode,
                flags,
                unicode,
                glyph,
                ..
            } if !secure
                && mode.accepts(*flags)
                && (*flags != 0
                    || *keycode == crate::event_tap::keyboard::VK_SPACE
                    || !mapping::is_printable(unicode)) =>
            {
                // Key events: a held modifier excludes FLAG_FN, which the system sets on every
                // function-key event (see MODIFIER_MASK). Passing it here would also suppress
                // the layout-glyph fallback for every arrow key.
                *glyph = mapping::key_glyph(*keycode, unicode, *flags & mapping::MODIFIER_MASK)
                    .map_or(KeyGlyph::Unavailable, KeyGlyph::Mapped);
            }
            _ => {}
        }
    }
    let now = Instant::now();
    let cursor_point = panel::current_cursor_appkit_point();
    let cursor_inside_panel = cursor_point.and_then(panel::cursor_inside_visible_panel);
    let mut became_secure = false;
    let (badges, visible, capped, changed, display_position) = STATE.with(|state| {
        let mut state = state.borrow_mut();
        let was_secure = state.secure_paused();
        let mut changed = false;
        for input in events {
            changed |= state.apply(input, mode, now);
        }
        if state.panel_visible() {
            if let Some(inside) = cursor_inside_panel {
                let activity =
                    PANEL_HOVER.with(|hover| hover.borrow_mut().observe(true, inside, now));
                if let Some(activity_at) = activity {
                    state.note_activity(activity_at);
                }
            }
        }
        changed |= state.tick(now);
        became_secure = !was_secure && state.secure_paused();
        let display_position = config.display_position.clone();
        let visible = state.panel_visible();
        if !visible {
            PANEL_HOVER.with(|hover| hover.borrow_mut().reset());
        }
        if visible {
            let orientation = state::Orientation::from_initial_position(&config.initial_position);
            // The same budget the renderer uses: it already has the drag handle and the trailing
            // settings button reserved, so the state never keeps a keycap the panel cannot show.
            let budget = panel::stream_budget_for_panel(
                &display_position,
                config.position,
                orientation,
                config.max_extent_percent,
            );
            changed |= state.trim_to_extent(budget, orientation);
        }
        (
            state.badges().to_vec(),
            visible,
            state.capped(),
            changed,
            display_position,
        )
    });

    if became_secure {
        crate::log_info!("[keystroke-display] secure input active; display paused.");
    }
    let first_hover_sample = panel::render(
        &badges,
        visible,
        capped,
        panel::PanelPlacement {
            display_position: &display_position,
            initial_position: &config.initial_position,
            position: config.position,
            max_extent_percent: config.max_extent_percent,
        },
        now,
        cursor_point,
    );
    if let Some(inside) = first_hover_sample {
        let activity = PANEL_HOVER.with(|hover| hover.borrow_mut().observe(true, inside, now));
        if let Some(activity_at) = activity {
            note_panel_activity(activity_at);
        }
    }
    if changed {
        crate::e2e_state::record("keystroke_display");
    }
    let keep_timer = visible || panel::fade_pending() || panel::drag_active();
    if !keep_timer {
        panel::stop_timer();
    }
    keep_timer
}

pub(super) fn note_panel_activity(now: Instant) {
    crate::debug_assert_main_thread();
    STATE.with(|state| state.borrow_mut().note_activity(now));
}

pub(super) fn timer_fired() {
    process_tick();
}

/// Run one tick body outside the run-loop timer. The panel smoke uses it to drive the real drain
/// path (the same function the timer calls) instead of calling the preview helpers directly, so a
/// regression in how preview input is handled fails the assertion. Compiled unconditionally like
/// the other smoke entry points, which live in the library rather than a test-only module.
#[allow(dead_code)] // only the smoke runners call it
pub(crate) fn process_tick_for_test() -> bool {
    process_tick()
}

fn reset_on_main() {
    crate::debug_assert_main_thread();
    panel::reset();
    // The development hold-open mode is a property of the launch, so a reset re-seeds it rather
    // than dropping it (see StateMachine::reset_for_launch).
    let hold_open = crate::dev_flags::present("show-keystroke-display");
    STATE.with(|state| state.borrow_mut().reset_for_launch(hold_open));
    PANEL_HOVER.with(|hover| hover.borrow_mut().reset());
    if crate::e2e_state::is_enabled() {
        crate::e2e_state::record("keystroke_display");
    }
}

/// Start or stop runtime services from the new configuration. The switcher tap's on-start
/// callback starts this tap, guaranteeing the requested head-insert order.
pub(crate) fn apply_config_change(old: &Config, new: &Config) {
    let force_on = crate::dev_flags::present("keystroke-display-force-on");
    let was_enabled = old.keystroke_display.enabled || force_on;
    let is_enabled = new.keystroke_display.enabled || force_on;
    // The service going away takes any running size preview with it, or the panel would be left
    // showing virtual keycaps at a raised window level after its feature was switched off.
    if was_enabled && !is_enabled {
        panel::end_max_extent_preview();
    }
    if was_enabled != is_enabled {
        if is_enabled {
            start();
        } else {
            stop();
        }
        return;
    }
    if is_enabled && old.keystroke_display.tap_level != new.keystroke_display.tap_level {
        restart_tap();
    }
}

pub(crate) fn start() {
    let config = crate::config::CONFIG
        .read()
        .unwrap()
        .keystroke_display
        .clone();
    if !config.enabled && !crate::dev_flags::present("keystroke-display-force-on") {
        if is_active() {
            stop();
        }
        return;
    }
    if !is_active() {
        SECURE_ACTIVE_HINT.store(false, Ordering::Release);
        SECURE_STATE_PENDING.store(-1, Ordering::Release);
    }
    ACTIVE.store(true, Ordering::SeqCst);
    let force_secure = crate::dev_flags::value("keystroke-display-secure-indicator")
        .is_some_and(|value| value == "always");
    secure::start(force_secure);
    if crate::event_monitor::tap_is_active() {
        ensure_tap_started(&config);
    }
}

/// The event monitor calls this only after its session tap is registered and enabled.
pub(crate) fn switcher_tap_started() {
    if !is_active() {
        return;
    }
    let config = crate::config::CONFIG
        .read()
        .unwrap()
        .keystroke_display
        .clone();
    ensure_tap_started(&config);
}

pub(crate) fn stop() {
    ACTIVE.store(false, Ordering::SeqCst);
    SECURE_ACTIVE_HINT.store(false, Ordering::Release);
    SECURE_STATE_PENDING.store(-1, Ordering::Release);
    secure::stop();
    stop_tap();
    if let Some(queue) = EVENT_QUEUE.get() {
        queue
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
    }
    WAKE_PENDING.store(false, Ordering::Release);
    if crate::is_main_thread() {
        reset_on_main();
    } else {
        crate::enqueue_global_event(crate::event_monitor::GlobalEvent::KeystrokeDisplayWake);
    }
}

fn effective_tap_level(config: &KeystrokeDisplaySection) -> String {
    match crate::dev_flags::value("keystroke-display-tap").as_deref() {
        Some("hid") => "hid".into(),
        Some("session") => "session".into(),
        _ if config.tap_level == "hid" => "hid".into(),
        _ => "session".into(),
    }
}

fn ensure_tap_started(config: &KeystrokeDisplaySection) {
    if !is_active() || !crate::event_monitor::tap_is_active() {
        return;
    }
    let level = effective_tap_level(config);
    let location = tap::location_from_level(&level);
    let slot = TAP_THREAD.get_or_init(|| Mutex::new(None));
    let mut thread = slot.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(running) = thread.as_ref() {
        if running.location == location && !running.handle.is_finished() {
            return;
        }
    }
    if let Some(finished) = thread.take() {
        TAP_CONTROL.stop();
        let _ = finished.handle.join();
    }
    *thread = Some(TapWorker {
        location,
        handle: tap::start(location),
    });
}

fn restart_tap() {
    stop_tap();
    let config = crate::config::CONFIG
        .read()
        .unwrap()
        .keystroke_display
        .clone();
    ensure_tap_started(&config);
}

fn stop_tap() {
    TAP_CONTROL.stop();
    let Some(slot) = TAP_THREAD.get() else {
        return;
    };
    let old = slot
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take();
    if let Some(thread) = old {
        let _ = thread.handle.join();
    }
}

pub(crate) fn smoke_panel_runner() -> bool {
    panel::smoke_runner()
}

pub(crate) fn e2e_snapshot() -> E2eSnapshot {
    crate::debug_assert_main_thread();
    let config = crate::config::CONFIG
        .read()
        .unwrap()
        .keystroke_display
        .clone();
    STATE.with(|state| {
        let state = state.borrow();
        let badges = state
            .badges()
            .iter()
            .map(|badge| E2eBadge {
                text: if badge.kind == BadgeKind::Indicator {
                    crate::i18n::t("keystroke_display.paused")
                } else {
                    badge.text.clone()
                },
                kind: match badge.kind {
                    BadgeKind::Modifier => "modifier",
                    BadgeKind::ModifierReleased => "modifier_released",
                    BadgeKind::Chord => "chord",
                    BadgeKind::Indicator => "indicator",
                },
                repeats: badge.repeats,
            })
            .collect();
        E2eSnapshot {
            visible: state.panel_visible(),
            badges,
            secure_paused: state.secure_paused(),
            tap_level: effective_tap_level(&config),
        }
    })
}

#[cfg(test)]
mod tests {
    use crate::config::Config;
    use crate::smoke_verdict::smoke_runner;

    #[test]
    fn development_overrides_do_not_require_environment_configuration() {
        assert!(!Config::default().keystroke_display.enabled);
        assert_eq!(Config::default().keystroke_display.tap_level, "session");
    }

    /// The ignored smoke spawns `target/debug/oh-my-tab`, which `cargo test` does not always rebuild:
    /// a deliberately broken refresh path once passed here on a stale binary. Refuse to spawn when the
    /// binary predates any file under `src/`, so the runner cannot report PASS against old code.
    fn app_binary_is_fresh(app: &std::path::Path) -> bool {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        binary_is_fresh_than_tree(app, &root)
    }

    fn binary_is_fresh_than_tree(app: &std::path::Path, root: &std::path::Path) -> bool {
        let Ok(binary_mtime) = std::fs::metadata(app).and_then(|meta| meta.modified()) else {
            return false;
        };
        match newest_mtime(root) {
            Ok(Some(newest)) => binary_mtime >= newest,
            // A source tree that cannot be read (or holds nothing) means the check did not run: fail
            // closed rather than let an unreadable tree pass the stale binary through.
            Ok(None) | Err(_) => false,
        }
    }

    fn newest_mtime(dir: &std::path::Path) -> std::io::Result<Option<std::time::SystemTime>> {
        let mut newest: Option<std::time::SystemTime> = None;
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let meta = entry.metadata()?;
            let candidate = if meta.is_dir() {
                newest_mtime(&entry.path())?
            } else {
                Some(meta.modified()?)
            };
            if let Some(candidate) = candidate {
                if newest.is_none_or(|current| candidate > current) {
                    newest = Some(candidate);
                }
            }
        }
        Ok(newest)
    }

    /// Counter-example: a tree that cannot be read must not be treated as "the binary is fresh".
    #[test]
    fn an_unreadable_source_tree_does_not_pass_the_freshness_check() {
        let app = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        assert!(!binary_is_fresh_than_tree(
            &app,
            std::path::Path::new("/nonexistent-oh-my-tab-source-tree")
        ));
    }

    #[test]
    #[ignore]
    fn panel_creation_and_badge_layout_smoke() {
        smoke_runner!("panel_creation_and_badge_layout_smoke", 1, {
            let app = crate::smoke_verdict::smoke_app_binary();
            assert!(
                app_binary_is_fresh(&app),
                "app binary {} is older than src/: run `cargo build` before this smoke, or it can pass \
                 against the previous implementation",
                app.display()
            );
            // The refusal probe needs exclusive ownership of the single-instance lock.
            let instance_guard = crate::single_instance::acquire()
                .expect("panel smoke requires no running app instance for its refusal probe");
            let refusal = std::process::Command::new(&app)
                // Keep this a smoke launch so a lock refusal has a failing exit status, but use
                // an entry point that remains protected by the single-instance guard.
                .arg("--smoke-single-instance-probe")
                .output()
                .expect("failed to spawn single-instance refusal probe");
            let refusal_stderr = String::from_utf8_lossy(&refusal.stderr);
            assert_eq!(
                refusal.status.code(),
                Some(1),
                "panel smoke must fail when startup is refused; exit={:?}, stderr:\n{}",
                refusal.status.code(),
                refusal_stderr
            );
            assert!(
                refusal_stderr.contains("[single-instance] startup refused")
                    && refusal_stderr.contains("another Oh-My-Tab instance is already running"),
                "panel smoke refusal probe did not report the single-instance short-circuit:\n{refusal_stderr}"
            );
            drop(instance_guard);
            // The child ran the panel assertions: take its verdict, and fail when it failed.
            crate::smoke_verdict::run_smoke_child(
                "panel_creation_and_badge_layout_smoke",
                &["--smoke-keystroke-display-panel"],
            );
        });
    }
}
