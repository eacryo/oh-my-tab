//! Development demo playback: parse an ordered semantic-action script and execute it without
//! synthesizing keyboard or pointer events. The driver thread owns ordering and pacing only; every
//! action and condition runs on the AppKit main thread through the controller hop.

use block2::RcBlock;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send, sel};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::log_info;

const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(40);
const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(25);
const DEFAULT_WAIT_TIMEOUT: Duration = Duration::from_millis(2_000);
const DEMO_SCRIPT_MAX_BYTES: u64 = 1024 * 1024;
/// `at_ms` paces a demo meant to be recorded; an hour of timeline is far past any such script and
/// its absence hints at a units mistake (the nanolight promo's longest film is forty seconds).
const MAX_AT_MS: u64 = 3_600_000;
/// The clipboard fixture replaces the picker wholesale, so more rows than the picker itself can
/// hold are never meaningful.
const MAX_FIXTURE_ENTRIES: usize = 500;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawScript {
    step: Vec<RawStep>,
    fixture: Option<RawFixture>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFixture {
    #[serde(default)]
    entry: Vec<RawFixtureEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFixtureEntry {
    text: Option<String>,
    pinned: Option<bool>,
    source_app: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStep {
    action: Option<String>,
    target: Option<String>,
    count: Option<u32>,
    interval_ms: Option<u64>,
    delay_ms: Option<u64>,
    at_ms: Option<u64>,
    wait_for: Option<String>,
    timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum DemoAction {
    OverlayOpen,
    OverlayNext,
    OverlayPrev,
    OverlayUp,
    OverlayDown,
    OverlayCommit,
    OverlayCancel,
    ClipboardOpen,
    ClipboardClose,
    ClipboardNext,
    ClipboardPrev,
    ClipboardDetail,
    ClipboardPaste,
    SettingsOpen(usize),
    SettingsClose,
}

impl DemoAction {
    fn label(&self) -> String {
        match self {
            Self::OverlayOpen => "overlay.open".to_string(),
            Self::OverlayNext => "overlay.next".to_string(),
            Self::OverlayPrev => "overlay.prev".to_string(),
            Self::OverlayUp => "overlay.up".to_string(),
            Self::OverlayDown => "overlay.down".to_string(),
            Self::OverlayCommit => "overlay.commit".to_string(),
            Self::OverlayCancel => "overlay.cancel".to_string(),
            Self::ClipboardOpen => "clipboard.open".to_string(),
            Self::ClipboardClose => "clipboard.close".to_string(),
            Self::ClipboardNext => "clipboard.next".to_string(),
            Self::ClipboardPrev => "clipboard.prev".to_string(),
            Self::ClipboardDetail => "clipboard.detail".to_string(),
            Self::ClipboardPaste => "clipboard.paste".to_string(),
            Self::SettingsOpen(page) => format!("settings.open({page})"),
            Self::SettingsClose => "settings.close".to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DemoWait {
    OverlayVisible,
    OverlayHidden,
    ClipboardVisible,
    ClipboardHidden,
    ClipboardDetailVisible,
    ClipboardDetailHidden,
    SettingsVisible,
    SettingsHidden,
}

impl DemoWait {
    fn label(self) -> &'static str {
        match self {
            Self::OverlayVisible => "overlay.visible",
            Self::OverlayHidden => "overlay.hidden",
            Self::ClipboardVisible => "clipboard.visible",
            Self::ClipboardHidden => "clipboard.hidden",
            Self::ClipboardDetailVisible => "clipboard.detail.visible",
            Self::ClipboardDetailHidden => "clipboard.detail.hidden",
            Self::SettingsVisible => "settings.visible",
            Self::SettingsHidden => "settings.hidden",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DemoStep {
    action: Option<DemoAction>,
    count: u32,
    interval: Duration,
    delay: Duration,
    /// Timeline pacing: when this step fires, measured from playback start rather than from the
    /// previous step's completion. Steps without it run sequentially as before; a step that is
    /// behind the clock when reached fires immediately (the clock never waits backwards).
    at: Option<Duration>,
    wait_for: Option<DemoWait>,
    timeout: Duration,
}

struct DemoScript {
    steps: Vec<DemoStep>,
    fixture: Vec<crate::clipboard::DemoFixtureEntry>,
}

fn parse_script(source: &str) -> Result<DemoScript, String> {
    let raw: RawScript = toml::from_str(source).map_err(|error| error.to_string())?;
    if raw.step.is_empty() {
        return Err("the script has no [[step]] entries".to_string());
    }
    let steps: Vec<DemoStep> = raw
        .step
        .iter()
        .enumerate()
        .map(|(index, step)| parse_step(index + 1, step))
        .collect::<Result<_, _>>()?;
    // Steps execute in script order, so a clock that jumps backwards could never be honoured:
    // refuse the script instead of silently firing that step late.
    let mut last_at: Option<Duration> = None;
    for (index, step) in steps.iter().enumerate() {
        if let Some(at) = step.at {
            if let Some(previous) = last_at {
                if at < previous {
                    return Err(format!(
                        "step {}: at_ms must not decrease within a script",
                        index + 1
                    ));
                }
            }
            last_at = Some(at);
        }
    }
    let fixture = parse_fixture(raw.fixture)?;
    Ok(DemoScript { steps, fixture })
}

fn parse_fixture(
    raw: Option<RawFixture>,
) -> Result<Vec<crate::clipboard::DemoFixtureEntry>, String> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    if raw.entry.is_empty() {
        return Err("[fixture] declares no [[fixture.entry]] rows".to_string());
    }
    if raw.entry.len() > MAX_FIXTURE_ENTRIES {
        return Err(format!(
            "the fixture declares {} entries; the most a picker holds is {MAX_FIXTURE_ENTRIES}",
            raw.entry.len()
        ));
    }
    raw.entry
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let text = entry
                .text
                .as_deref()
                .filter(|text| !text.is_empty())
                .ok_or_else(|| {
                    format!(
                        "fixture entry {}: text is required and must not be empty",
                        index + 1
                    )
                })?;
            Ok(crate::clipboard::DemoFixtureEntry {
                text: text.to_string(),
                pinned: entry.pinned.unwrap_or(false),
                source_app: entry.source_app.clone().unwrap_or_default(),
            })
        })
        .collect()
}

fn parse_step(index: usize, raw: &RawStep) -> Result<DemoStep, String> {
    let action = parse_action(index, raw)?;
    let wait_for = raw
        .wait_for
        .as_deref()
        .map(str::trim)
        .map(|name| parse_wait(name).map_err(|error| format!("step {index}: {error}")))
        .transpose()?;

    let count = raw.count.unwrap_or(1);
    if count == 0 {
        return Err(format!("step {index}: count must be at least 1"));
    }
    if action.is_none() && raw.count.is_some() {
        return Err(format!("step {index}: count requires an action"));
    }
    if raw.interval_ms.is_some() && (action.is_none() || count == 1) {
        return Err(format!(
            "step {index}: interval_ms requires an action repeated at least twice"
        ));
    }
    if raw.timeout_ms.is_some() && wait_for.is_none() {
        return Err(format!("step {index}: timeout_ms requires wait_for"));
    }
    if raw.at_ms.is_some() && raw.delay_ms.is_some() {
        return Err(format!(
            "step {index}: at_ms and delay_ms both pace the step's start; use one"
        ));
    }
    if let Some(at_ms) = raw.at_ms {
        if at_ms > MAX_AT_MS {
            return Err(format!(
                "step {index}: at_ms {at_ms} is past the one-hour timeline bound"
            ));
        }
    }
    let delay = Duration::from_millis(raw.delay_ms.unwrap_or(0));
    if action.is_none() && wait_for.is_none() && delay.is_zero() && raw.at_ms.is_none() {
        return Err(format!(
            "step {index}: one of action, wait_for, delay_ms or at_ms is required"
        ));
    }

    Ok(DemoStep {
        action,
        count,
        interval: Duration::from_millis(raw.interval_ms.unwrap_or(0)),
        delay,
        at: raw.at_ms.map(Duration::from_millis),
        wait_for,
        timeout: raw
            .timeout_ms
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_WAIT_TIMEOUT),
    })
}

fn parse_action(index: usize, raw: &RawStep) -> Result<Option<DemoAction>, String> {
    let Some(name) = raw.action.as_deref().map(str::trim) else {
        if raw.target.is_some() {
            return Err(format!("step {index}: target requires an action"));
        }
        return Ok(None);
    };
    if name.is_empty() {
        return Err(format!("step {index}: action must not be empty"));
    }
    if name != "settings.open" && raw.target.is_some() {
        return Err(format!(
            "step {index}: target is only valid for settings.open"
        ));
    }
    let action = match name {
        "overlay.open" => DemoAction::OverlayOpen,
        "overlay.next" => DemoAction::OverlayNext,
        "overlay.prev" => DemoAction::OverlayPrev,
        "overlay.up" => DemoAction::OverlayUp,
        "overlay.down" => DemoAction::OverlayDown,
        "overlay.commit" => DemoAction::OverlayCommit,
        "overlay.cancel" => DemoAction::OverlayCancel,
        "clipboard.open" => DemoAction::ClipboardOpen,
        "clipboard.close" => DemoAction::ClipboardClose,
        "clipboard.next" => DemoAction::ClipboardNext,
        "clipboard.prev" => DemoAction::ClipboardPrev,
        "clipboard.detail" => DemoAction::ClipboardDetail,
        "clipboard.paste" => DemoAction::ClipboardPaste,
        "settings.close" => DemoAction::SettingsClose,
        "settings.open" => {
            let target = raw
                .target
                .as_deref()
                .map(str::trim)
                .filter(|target| !target.is_empty())
                .ok_or_else(|| format!("step {index}: settings.open requires target=<page>"))?;
            let Some(page) = crate::settings::parse_settings_page_spec(target) else {
                return Err(format!(
                    "step {index}: settings.open target {target:?} names no settings page"
                ));
            };
            DemoAction::SettingsOpen(page)
        }
        _ => return Err(format!("step {index}: unknown action {name:?}")),
    };
    Ok(Some(action))
}

fn parse_wait(name: &str) -> Result<DemoWait, String> {
    match name {
        "overlay.visible" => Ok(DemoWait::OverlayVisible),
        "overlay.hidden" => Ok(DemoWait::OverlayHidden),
        "clipboard.visible" => Ok(DemoWait::ClipboardVisible),
        "clipboard.hidden" => Ok(DemoWait::ClipboardHidden),
        "clipboard.detail.visible" => Ok(DemoWait::ClipboardDetailVisible),
        "clipboard.detail.hidden" => Ok(DemoWait::ClipboardDetailHidden),
        "settings.visible" => Ok(DemoWait::SettingsVisible),
        "settings.hidden" => Ok(DemoWait::SettingsHidden),
        _ => Err(format!("unknown wait_for {name:?}")),
    }
}

#[derive(Clone, Debug)]
enum DemoRequest {
    Action(DemoAction),
    Wait(DemoWait),
    Shutdown,
}

struct DemoExchange {
    request: DemoRequest,
    result: Option<Result<bool, String>>,
}

struct DemoInputMonitor {
    monitor: *mut AnyObject,
    /// The monitor borrows the block pointer rather than retaining it, so the block lives here for
    /// exactly the monitor's installed lifetime (the same ownership shape as the picker monitors).
    #[allow(dead_code)]
    handler: RcBlock<dyn Fn(*mut AnyObject) -> *mut AnyObject>,
}

unsafe impl Send for DemoInputMonitor {}
unsafe impl Sync for DemoInputMonitor {}

static EXCHANGE: Mutex<Option<DemoExchange>> = Mutex::new(None);
static PENDING_SCRIPT: Mutex<Option<DemoScript>> = Mutex::new(None);
static INPUT_MONITOR: Mutex<Option<DemoInputMonitor>> = Mutex::new(None);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static CANCEL: AtomicBool = AtomicBool::new(false);

/// Read a demo script without letting a special file block application startup: FIFOs, devices and
/// directories are refused before opening, and regular scripts are bounded because both the read and
/// the TOML parse happen before the run loop starts.
fn read_script(path: &std::path::Path) -> Result<String, String> {
    let metadata = std::fs::metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("the demo script is not a regular file".to_string());
    }
    // Bounded at the reader rather than by the stat above: the file can grow or be replaced
    // between the metadata call and the read, and the cap is what keeps a scripting mistake
    // out of the TOML parser, not the directory listing.
    use std::io::Read as _;
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut bounded = file.take(DEMO_SCRIPT_MAX_BYTES + 1);
    let mut bytes = Vec::new();
    bounded
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > DEMO_SCRIPT_MAX_BYTES {
        return Err(format!(
            "the demo script is larger than {} bytes",
            DEMO_SCRIPT_MAX_BYTES
        ));
    }
    String::from_utf8(bytes).map_err(|error| error.to_string())
}

/// Start the script named by `--demo=<path>`. Missing or malformed input is reported and the normal
/// launch continues: a demo is a development aid, not a startup dependency.
pub(crate) fn start_from_dev_flag() {
    let Some(spec) = crate::dev_flags::value("demo") else {
        return;
    };
    if spec.trim().is_empty() {
        log_info!("[demo] --demo requires a script path");
        return;
    }
    let path = PathBuf::from(spec.trim());
    let source = match read_script(&path) {
        Ok(source) => source,
        Err(error) => {
            log_info!("[demo] could not read {}: {error}", path.display());
            return;
        }
    };
    let script = match parse_script(&source) {
        Ok(script) => script,
        Err(error) => {
            log_info!(
                "[demo] {} is not a valid demo script: {error}",
                path.display()
            );
            return;
        }
    };
    log_info!(
        "[demo] queued {} step(s) from {}",
        script.steps.len(),
        path.display()
    );
    *PENDING_SCRIPT.lock().unwrap() = Some(script);
    // `run()` reaches this switch before NSApplication's run loop spins. Start the driver from a
    // delayed selector instead of letting its first synchronous performSelectorOnMainThread race
    // the run loop; the timer fires only once main-thread event processing is really active.
    let Some(controller) = crate::CONTROLLER.lock().unwrap().map(|target| target.0) else {
        PENDING_SCRIPT.lock().unwrap().take();
        log_info!("[demo] could not start: the app controller is not available");
        return;
    };
    unsafe {
        let _: () = msg_send![
            controller,
            performSelector: sel!(handleDemoStart:),
            withObject: std::ptr::null::<AnyObject>(),
            afterDelay: 0.0f64
        ];
    }
}

/// Start the queued driver once the main run loop is active. The spawn itself is cheap and the
/// driver's first action can then block on the main thread without depending on startup timing.
pub(crate) fn start_pending_script() {
    crate::debug_assert_main_thread();
    let Some(script) = PENDING_SCRIPT.lock().unwrap().take() else {
        return;
    };
    log_info!("[demo] starting {} step(s)", script.steps.len());
    if !install_input_monitor() {
        log_info!("[demo] could not install the local input monitor; playback refused");
        return;
    }
    let fixture_count = script.fixture.len();
    if fixture_count > 0 {
        crate::clipboard::install_demo_fixture(script.fixture);
        log_info!(
            "[demo] clipboard fixture installed: {fixture_count} row(s), the real history is parked"
        );
    }
    let steps = script.steps;
    CANCEL.store(false, Ordering::SeqCst);
    ACTIVE.store(true, Ordering::SeqCst);
    if let Err(error) = std::thread::Builder::new()
        .name("oh-my-tab-demo".to_string())
        .spawn(move || driver(steps))
    {
        ACTIVE.store(false, Ordering::SeqCst);
        crate::clipboard::remove_demo_fixture();
        remove_input_monitor();
        log_info!("[demo] could not start the driver thread: {error}");
    }
}

/// Install a local key/mouse monitor for the duration of playback. It observes events before
/// AppKit dispatches them, so cancellation covers panel keyDown, the clipboard search field's
/// editor, settings controls and mouse-downs with one entry point. The event is always returned
/// unchanged; this monitor only yields control to the user.
fn install_input_monitor() -> bool {
    crate::debug_assert_main_thread();
    if INPUT_MONITOR.lock().unwrap().is_some() {
        return true;
    }
    let handler: RcBlock<dyn Fn(*mut AnyObject) -> *mut AnyObject> =
        RcBlock::new(|event: *mut AnyObject| -> *mut AnyObject {
            cancel_for_user_input();
            event
        });
    // LeftMouseDown(1) | RightMouseDown(3) | KeyDown(10) | ScrollWheel(22) | OtherMouseDown(25);
    // NSEvent masks are bit 1 << type. KeyUp adds nothing once KeyDown has yielded control.
    let mask: u64 = (1u64 << 1) | (1u64 << 3) | (1u64 << 10) | (1u64 << 22) | (1u64 << 25);
    let monitor: *mut AnyObject = unsafe {
        msg_send![
            class!(NSEvent),
            addLocalMonitorForEventsMatchingMask: mask,
            handler: &*handler
        ]
    };
    if monitor.is_null() {
        return false;
    }
    *INPUT_MONITOR.lock().unwrap() = Some(DemoInputMonitor { monitor, handler });
    true
}

fn remove_input_monitor() {
    crate::debug_assert_main_thread();
    let Some(installed) = INPUT_MONITOR.lock().unwrap().take() else {
        return;
    };
    unsafe {
        let _: () = msg_send![class!(NSEvent), removeMonitor: installed.monitor];
    }
}

/// Cancel playback because real input arrived. The semantic actions below never enqueue global
/// input, so anything reaching the input drain is a user taking over.
pub(crate) fn cancel_for_user_input() {
    if ACTIVE.load(Ordering::SeqCst) && !CANCEL.swap(true, Ordering::SeqCst) {
        log_info!("[demo] cancelled by real input");
    }
}

/// Execute the exchange the driver parked for the main thread. Registered as a controller callback;
/// the driver blocks in performSelectorOnMainThread until this stores the result.
pub(crate) fn invoke_pending_on_main() {
    crate::debug_assert_main_thread();
    let request = {
        let exchange = EXCHANGE.lock().unwrap();
        exchange.as_ref().map(|exchange| exchange.request.clone())
    };
    let Some(request) = request else {
        return;
    };
    let result = match request {
        // Shutdown is housekeeping, not playback: it must remain executable after cancellation so
        // the input monitor cannot leak past the end of the script.
        DemoRequest::Shutdown => {
            if crate::clipboard::demo_fixture_active() {
                crate::clipboard::remove_demo_fixture();
                log_info!("[demo] clipboard fixture removed: the real history is restored");
            }
            remove_input_monitor();
            Ok(true)
        }
        _ if CANCEL.load(Ordering::SeqCst) => {
            Err("playback was cancelled by user input".to_string())
        }
        DemoRequest::Action(action) => execute_action(&action),
        DemoRequest::Wait(wait) => Ok(evaluate_wait(wait)),
    };
    let mut exchange = EXCHANGE.lock().unwrap();
    if let Some(exchange) = exchange.as_mut() {
        exchange.result = Some(result);
    }
}

fn invoke_on_main(request: DemoRequest) -> Result<bool, String> {
    let Some(controller) = crate::CONTROLLER.lock().unwrap().map(|target| target.0) else {
        return Err("the app controller is not available".to_string());
    };
    {
        let mut exchange = EXCHANGE.lock().unwrap();
        if exchange.is_some() {
            return Err("another demo request is already in flight".to_string());
        }
        *exchange = Some(DemoExchange {
            request,
            result: None,
        });
    }
    unsafe {
        let _: () = msg_send![
            controller,
            performSelectorOnMainThread: sel!(handleDemoInvoke:),
            withObject: std::ptr::null::<AnyObject>(),
            waitUntilDone: true
        ];
    }
    let exchange = EXCHANGE.lock().unwrap().take();
    exchange
        .and_then(|exchange| exchange.result)
        .ok_or_else(|| "the main thread did not return a demo result".to_string())?
}

fn execute_action(action: &DemoAction) -> Result<bool, String> {
    match action {
        DemoAction::OverlayOpen => {
            if crate::overlay::switcher_session_is_active() {
                Ok(false)
            } else {
                crate::overlay::show_overlay_for_development();
                Ok(true)
            }
        }
        DemoAction::OverlayNext => {
            if crate::overlay::step_visible_switcher(false) {
                Ok(true)
            } else {
                Err("the switcher has no visible windows to navigate".to_string())
            }
        }
        DemoAction::OverlayPrev => {
            if crate::overlay::step_visible_switcher(true) {
                Ok(true)
            } else {
                Err("the switcher has no visible windows to navigate".to_string())
            }
        }
        DemoAction::OverlayUp | DemoAction::OverlayDown => {
            if !crate::overlay::switcher_session_is_visible() {
                return Err("the switcher is not visible".to_string());
            }
            let up = matches!(action, DemoAction::OverlayUp);
            Ok(unsafe { crate::overlay::navigate_visible_switcher_vertical(up) })
        }
        DemoAction::OverlayCommit => {
            if crate::overlay::commit_switcher_selection() {
                Ok(true)
            } else {
                Err("the switcher is not visible".to_string())
            }
        }
        DemoAction::OverlayCancel => Ok(crate::overlay::cancel_switcher_session()),
        DemoAction::ClipboardOpen => crate::clipboard::open_picker_for_demo(),
        DemoAction::ClipboardClose => crate::clipboard::close_picker_for_demo(),
        DemoAction::ClipboardNext => crate::clipboard::move_selection_for_demo(true),
        DemoAction::ClipboardPrev => crate::clipboard::move_selection_for_demo(false),
        DemoAction::ClipboardDetail => crate::clipboard::toggle_detail_for_demo(),
        DemoAction::ClipboardPaste => crate::clipboard::paste_selection_for_demo(),
        DemoAction::SettingsOpen(page) => {
            crate::settings::show_settings_page(*page);
            Ok(true)
        }
        DemoAction::SettingsClose => {
            if crate::settings::settings_window_is_visible() {
                crate::settings::close_settings_from_switcher();
                Ok(true)
            } else {
                Ok(false)
            }
        }
    }
}

fn evaluate_wait(wait: DemoWait) -> bool {
    match wait {
        DemoWait::OverlayVisible => {
            crate::overlay::switcher_session_is_visible()
                && crate::overlay::panel_is_visible_for_e2e()
        }
        DemoWait::OverlayHidden => !crate::overlay::panel_is_visible_for_e2e(),
        DemoWait::ClipboardVisible => crate::clipboard::picker_is_visible(),
        DemoWait::ClipboardHidden => !crate::clipboard::picker_is_visible(),
        DemoWait::ClipboardDetailVisible => crate::clipboard::picker_detail_is_visible(),
        DemoWait::ClipboardDetailHidden => !crate::clipboard::picker_detail_is_visible(),
        DemoWait::SettingsVisible => crate::settings::settings_window_is_visible(),
        DemoWait::SettingsHidden => !crate::settings::settings_window_is_visible(),
    }
}

fn driver(steps: Vec<DemoStep>) {
    let total = steps.len();
    let mut failed = false;
    // The timeline clock starts as the driver does. A step with `at` fires when the clock reaches
    // it -- never earlier, and a step reached late fires at once rather than stretching the rest
    // of the timeline to absorb the drift.
    let playback_start = Instant::now();
    'steps: for (index, step) in steps.into_iter().enumerate() {
        let step_number = index + 1;
        let start_delay = match step.at {
            Some(at) => playback_start
                .checked_add(at)
                .and_then(|target| target.checked_duration_since(Instant::now()))
                .unwrap_or(Duration::ZERO),
            None => step.delay,
        };
        if !interruptible_sleep(start_delay) {
            break;
        }
        if let Some(action) = step.action {
            let label = action.label();
            for repetition in 1..=step.count {
                if CANCEL.load(Ordering::SeqCst) {
                    break 'steps;
                }
                match invoke_on_main(DemoRequest::Action(action.clone())) {
                    Ok(changed) => log_info!(
                        "[demo] step {step_number}/{total}: {label} ({repetition}/{}) changed={changed}",
                        step.count
                    ),
                    Err(error) => {
                        log_info!("[demo] step {step_number}/{total}: {label} failed: {error}");
                        failed = true;
                        break 'steps;
                    }
                }
                if repetition < step.count && !interruptible_sleep(step.interval) {
                    break 'steps;
                }
            }
        }
        if let Some(wait) = step.wait_for {
            if !wait_until(step_number, total, wait, step.timeout) {
                failed = true;
                break;
            }
        }
    }
    // Playback has already reached its terminal state here. Retire it before removing the monitor
    // so an input event in that tiny housekeeping window cannot rewrite a finished run as cancelled.
    ACTIVE.store(false, Ordering::SeqCst);
    if let Err(error) = invoke_on_main(DemoRequest::Shutdown) {
        log_info!("[demo] could not remove the input monitor cleanly: {error}");
    }
    let cancelled = CANCEL.load(Ordering::SeqCst);
    if cancelled {
        log_info!("[demo] playback cancelled");
    } else if failed {
        log_info!("[demo] playback failed");
    } else {
        log_info!("[demo] playback finished");
    }
}

fn wait_until(step_number: usize, total: usize, wait: DemoWait, timeout: Duration) -> bool {
    let started = Instant::now();
    loop {
        if CANCEL.load(Ordering::SeqCst) {
            return false;
        }
        if started.elapsed() >= timeout {
            log_info!(
                "[demo] step {step_number}/{total}: timed out waiting for {}",
                wait.label()
            );
            return false;
        }
        match invoke_on_main(DemoRequest::Wait(wait)) {
            Ok(true) if started.elapsed() < timeout => {
                log_info!(
                    "[demo] step {step_number}/{total}: {} reached",
                    wait.label()
                );
                return true;
            }
            Ok(true) => {
                log_info!(
                    "[demo] step {step_number}/{total}: timed out waiting for {}",
                    wait.label()
                );
                return false;
            }
            Ok(false) => {}
            Err(error) => {
                log_info!(
                    "[demo] step {step_number}/{total}: {} could not be evaluated: {error}",
                    wait.label()
                );
                return false;
            }
        }
        if started.elapsed() >= timeout {
            log_info!(
                "[demo] step {step_number}/{total}: timed out waiting for {}",
                wait.label()
            );
            return false;
        }
        if !interruptible_sleep(WAIT_POLL_INTERVAL) {
            return false;
        }
    }
}

fn interruptible_sleep(duration: Duration) -> bool {
    let mut remaining = duration;
    while !remaining.is_zero() {
        if CANCEL.load(Ordering::SeqCst) {
            return false;
        }
        let chunk = remaining.min(CANCEL_POLL_INTERVAL);
        std::thread::sleep(chunk);
        remaining = remaining.saturating_sub(chunk);
    }
    !CANCEL.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_an_ordered_script() {
        let script = parse_script(
            r#"
[[step]]
action = "overlay.open"

[[step]]
wait_for = "overlay.visible"
timeout_ms = 1500

[[step]]
action = "overlay.next"
count = 2
interval_ms = 350

[[step]]
delay_ms = 200
action = "overlay.cancel"
"#,
        )
        .unwrap();
        let steps = script.steps;
        assert_eq!(steps.len(), 4);
        assert_eq!(steps[0].action, Some(DemoAction::OverlayOpen));
        assert_eq!(steps[1].wait_for, Some(DemoWait::OverlayVisible));
        assert_eq!(steps[1].timeout, Duration::from_millis(1500));
        assert_eq!(steps[2].count, 2);
        assert_eq!(steps[2].interval, Duration::from_millis(350));
        assert_eq!(steps[3].delay, Duration::from_millis(200));
        assert!(steps.iter().all(|step| step.at.is_none()));
        assert!(script.fixture.is_empty());
    }

    #[test]
    fn parses_settings_page_targets() {
        let script = parse_script(
            r#"
[[step]]
action = "settings.open"
target = "key-bindings"
"#,
        )
        .unwrap();
        assert!(matches!(
            script.steps[0].action,
            Some(DemoAction::SettingsOpen(_))
        ));
    }

    #[test]
    fn parses_a_timeline_script() {
        let script = parse_script(
            r#"
[[step]]
action = "clipboard.open"
at_ms = 1000

[[step]]
action = "clipboard.next"
at_ms = 1500
count = 3
interval_ms = 400

[[step]]
action = "clipboard.close"
at_ms = 1500
"#,
        )
        .unwrap();
        assert_eq!(script.steps[0].at, Some(Duration::from_millis(1000)));
        assert_eq!(script.steps[1].at, Some(Duration::from_millis(1500)));
        assert_eq!(script.steps[1].count, 3);
        // Equal timestamps are fine: the second step fires right after the first completes.
        assert_eq!(script.steps[2].at, Some(Duration::from_millis(1500)));
    }

    #[test]
    fn rejects_timeline_misuse() {
        // at_ms and delay_ms both pace the step's start.
        assert!(
            parse_script("[[step]]\naction = \"overlay.open\"\nat_ms = 100\ndelay_ms = 100\n")
                .is_err()
        );
        // A clock that jumps backwards in script order could never be honoured.
        assert!(parse_script(
            "[[step]]\nat_ms = 2000\ndelay_ms = 0\naction = \"overlay.open\"\n[[step]]\nat_ms = 1000\naction = \"overlay.cancel\"\n"
        )
        .is_err());
        // Past the one-hour bound.
        assert!(parse_script("[[step]]\naction = \"overlay.open\"\nat_ms = 3600001\n").is_err());
    }

    #[test]
    fn parses_a_clipboard_fixture() {
        let script = parse_script(
            r#"
[[fixture.entry]]
text = "https://example.com/release-notes"
source_app = "Safari"

[[fixture.entry]]
text = "pinned boilerplate"
pinned = true

[[fixture.entry]]
text = "a second copied line"

[[step]]
action = "clipboard.open"
"#,
        )
        .unwrap();
        assert_eq!(script.fixture.len(), 3);
        assert_eq!(script.fixture[0].text, "https://example.com/release-notes");
        assert_eq!(script.fixture[0].source_app, "Safari");
        assert!(!script.fixture[0].pinned);
        assert!(script.fixture[1].pinned);
        assert!(script.fixture[2].source_app.is_empty());
    }

    #[test]
    fn rejects_unknown_top_level_keys() {
        // A mistyped [[fixtrue.entry]] must not silently degrade to playback without the
        // fixture: clipboard steps would then run against the real history, which is exactly
        // what the fixture exists to keep out of a recorded demo.
        assert!(parse_script("[[fixtrue.entry]]\ntext = \"x\"\n[[step]]\ndelay_ms = 1\n").is_err());
        assert!(parse_script("unknown = 1\n[[step]]\ndelay_ms = 1\n").is_err());
    }

    #[test]
    fn rejects_a_malformed_fixture() {
        // A [fixture] table without any rows.
        assert!(parse_script("[fixture]\n[[step]]\ndelay_ms = 1\n").is_err());
        // A row without text.
        assert!(
            parse_script("[[fixture.entry]]\npinned = true\n[[step]]\ndelay_ms = 1\n").is_err()
        );
        // A row whose text is empty.
        assert!(parse_script("[[fixture.entry]]\ntext = \"\"\n[[step]]\ndelay_ms = 1\n").is_err());
        // Unknown keys are refused rather than ignored.
        assert!(parse_script(
            "[[fixture.entry]]\ntext = \"x\"\nimage = \"y\"\n[[step]]\ndelay_ms = 1\n"
        )
        .is_err());
    }

    #[test]
    fn rejects_unknown_actions_and_waits() {
        assert!(parse_script("[[step]]\naction = \"overlay.explode\"\n").is_err());
        assert!(parse_script("[[step]]\nwait_for = \"overlay.somewhat\"\n").is_err());
    }

    #[test]
    fn rejects_fields_that_do_not_apply() {
        assert!(parse_script("[[step]]\nwait_for = \"overlay.visible\"\ncount = 2\n").is_err());
        assert!(parse_script("[[step]]\naction = \"overlay.open\"\ntimeout_ms = 100\n").is_err());
        assert!(parse_script("[[step]]\naction = \"overlay.open\"\ntarget = \"about\"\n").is_err());
        assert!(parse_script("[[step]]\naction = \"overlay.open\"\ncount = 0\n").is_err());
    }

    #[test]
    fn rejects_empty_steps_and_empty_scripts() {
        assert!(parse_script("").is_err());
        assert!(parse_script("[[step]]\n").is_err());
    }

    #[test]
    fn script_reading_refuses_non_regular_and_oversized_input() {
        let unique = format!(
            "oh-my-tab-demo-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = std::env::temp_dir().join(unique);
        std::fs::create_dir(&dir).unwrap();
        assert!(read_script(&dir).is_err());

        let regular = dir.join("demo.toml");
        std::fs::write(&regular, "[[step]]\ndelay_ms = 1\n").unwrap();
        assert!(read_script(&regular).is_ok());

        let oversized = dir.join("oversized.toml");
        std::fs::write(&oversized, " ").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&oversized)
            .unwrap()
            .set_len(DEMO_SCRIPT_MAX_BYTES + 1)
            .unwrap();
        assert!(read_script(&oversized).is_err());

        std::fs::remove_file(&regular).unwrap();
        std::fs::remove_file(&oversized).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }
}
