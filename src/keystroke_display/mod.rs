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

pub(crate) unsafe fn apply_glass_properties() {
    panel::apply_glass_properties();
}

pub(crate) unsafe fn apply_backdrop_material() {
    panel::apply_backdrop_material();
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
                *glyph = mapping::key_glyph(*keycode, unicode, *flags & mapping::modifier_mask())
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
            let max_width = panel::target_screen_width(&display_position, config.position) * 0.5;
            changed |= state.trim_to_width(max_width);
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
    if !is_active() {
        reset_on_main();
        return false;
    }
    let first_hover_sample = panel::render(
        &badges,
        visible,
        capped,
        &display_position,
        config.position,
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

fn reset_on_main() {
    crate::debug_assert_main_thread();
    panel::reset();
    STATE.with(|state| *state.borrow_mut() = StateMachine::default());
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

    #[test]
    fn development_overrides_do_not_require_environment_configuration() {
        assert!(!Config::default().keystroke_display.enabled);
        assert_eq!(Config::default().keystroke_display.tap_level, "session");
    }

    #[test]
    #[ignore]
    fn panel_creation_and_badge_layout_smoke() {
        let exe = std::env::current_exe().expect("current exe");
        let app = exe
            .parent()
            .and_then(|parent| parent.parent())
            .map(|parent| parent.join("oh-my-tab"))
            .expect("app binary path");
        assert!(
            app.exists(),
            "app binary missing at {}: run `cargo build` first",
            app.display()
        );
        let instance_guard = crate::single_instance::acquire()
            .expect("panel smoke requires no running app instance for its refusal probe");
        let refusal = std::process::Command::new(&app)
            .arg("--smoke-keystroke-display-panel")
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

        let output = std::process::Command::new(&app)
            .arg("--smoke-keystroke-display-panel")
            .output()
            .expect("failed to spawn app");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("[single-instance] startup refused"),
            "keystroke panel smoke short-circuited before panel logic:\n{stderr}"
        );
        assert!(
            output.status.success(),
            "keystroke panel smoke failed (exit {:?})\nstderr:\n{}",
            output.status.code(),
            stderr
        );
    }
}
