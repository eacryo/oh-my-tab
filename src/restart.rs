//! Controlled app relaunch after Accessibility is restored while an event tap is terminally disabled.

use objc2::runtime::AnyObject;
use objc2::{class, msg_send, sel};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const HELPER_ARGUMENT: &str = "--oh-my-tab-relaunch-helper";
const RESTART_NOTICE_DELAY: Duration = Duration::from_secs(5);
const OLD_PROCESS_WAIT_LIMIT: Duration = Duration::from_secs(60);
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(100);

static RESTART_REQUIRED: AtomicBool = AtomicBool::new(false);
static RESTART_LAUNCHING: AtomicBool = AtomicBool::new(false);

pub(crate) fn restart_required() -> bool {
    RESTART_REQUIRED.load(Ordering::SeqCst)
}

/// Called only after the supervisor observed Accessibility transition from untrusted to trusted.
pub(crate) fn accessibility_restored_after_terminal_tap() {
    if RESTART_REQUIRED.swap(true, Ordering::SeqCst) {
        return;
    }

    crate::log_info!(
        "Accessibility permission was restored after a terminal tap disable; scheduling one app relaunch."
    );
    schedule_main_selector(sel!(handlePermissionRestartRequired:));

    if let Err(error) = std::thread::Builder::new()
        .name("accessibility-relaunch-countdown".into())
        .spawn(|| {
            std::thread::sleep(RESTART_NOTICE_DELAY);
            if restart_required() && crate::ffi::has_accessibility_permission() {
                schedule_main_selector(sel!(handlePermissionRestartNow:));
            }
        })
    {
        crate::log_info!(
            "Could not schedule automatic permission-recovery relaunch: {}",
            error
        );
    }
}

fn schedule_main_selector(selector: objc2::runtime::Sel) {
    let controller = crate::CONTROLLER.lock().unwrap().map(|target| target.0);
    let Some(controller) = controller else {
        crate::log_info!("Permission-recovery relaunch could not reach the AppKit main thread.");
        return;
    };
    unsafe {
        let _: () = msg_send![
            controller,
            performSelectorOnMainThread: selector,
            withObject: std::ptr::null::<AnyObject>(),
            waitUntilDone: false
        ];
    }
}

pub(crate) extern "C" fn on_permission_restart_required(
    _this: *mut std::ffi::c_void,
    _cmd: objc2::runtime::Sel,
    _sender: *mut std::ffi::c_void,
) {
    crate::callback_guard::void("on_permission_restart_required", || {
        crate::settings::refresh_permission_restart_state();
    });
}

pub(crate) extern "C" fn on_permission_restart_now(
    _this: *mut std::ffi::c_void,
    _cmd: objc2::runtime::Sel,
    _sender: *mut std::ffi::c_void,
) {
    crate::callback_guard::void("on_permission_restart_now", || {
        begin_relaunch("automatic");
    });
}

pub(crate) extern "C" fn on_restart_button(
    _this: *mut std::ffi::c_void,
    _cmd: objc2::runtime::Sel,
    _sender: *mut std::ffi::c_void,
) {
    crate::callback_guard::void("on_restart_button", || {
        begin_relaunch("settings button");
    });
}

fn begin_relaunch(reason: &str) {
    if !restart_required() || !crate::ffi::has_accessibility_permission() {
        return;
    }
    if RESTART_LAUNCHING.swap(true, Ordering::SeqCst) {
        return;
    }

    let executable = match std::env::current_exe() {
        Ok(path) => path,
        Err(error) => {
            RESTART_LAUNCHING.store(false, Ordering::SeqCst);
            crate::log_info!("Cannot resolve the app executable for relaunch: {}", error);
            return;
        }
    };
    let Some(bundle) = enclosing_app_bundle(&executable) else {
        RESTART_LAUNCHING.store(false, Ordering::SeqCst);
        crate::log_info!(
            "Cannot relaunch Accessibility recovery from an unbundled executable: {}",
            executable.display()
        );
        return;
    };

    let helper = match Command::new(&executable)
        .arg(HELPER_ARGUMENT)
        .arg(std::process::id().to_string())
        .arg(&bundle)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            RESTART_LAUNCHING.store(false, Ordering::SeqCst);
            crate::log_info!(
                "Cannot start the permission-recovery relaunch helper: {}",
                error
            );
            return;
        }
    };

    crate::log_info!(
        "Relaunching Oh My Tab after Accessibility recovery (reason={}, helper_pid={}, bundle={}).",
        reason,
        helper.id(),
        bundle.display()
    );
    if let Err(error) = crate::config::flush_config_sync() {
        crate::log_info!(
            "Config flush before permission-recovery relaunch failed: {}",
            error
        );
    }

    // The existing termination observer restores system pointer values before the old process exits.
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![app, terminate: std::ptr::null::<AnyObject>()];
    }
}

fn enclosing_app_bundle(executable: &Path) -> Option<PathBuf> {
    executable
        .ancestors()
        .find(|ancestor| {
            ancestor
                .extension()
                .is_some_and(|extension| extension == "app")
        })
        .map(Path::to_path_buf)
}

/// Consume the private helper mode before normal AppKit startup. Returns true when handled.
pub fn run_relaunch_helper_if_requested(args: &[String]) -> bool {
    if args.get(1).map(String::as_str) != Some(HELPER_ARGUMENT) {
        return false;
    }

    let result = match (
        args.get(2).and_then(|value| value.parse::<i32>().ok()),
        args.get(3),
    ) {
        (Some(pid), Some(bundle)) if pid > 0 => wait_and_open(pid, Path::new(bundle)),
        _ => Err("invalid relaunch-helper arguments".to_string()),
    };
    if let Err(error) = result {
        eprintln!("[permission-relaunch-helper] {error}");
    }
    true
}

fn wait_and_open(old_pid: i32, bundle: &Path) -> Result<(), String> {
    let started = Instant::now();
    while process_is_running(old_pid)? {
        if started.elapsed() >= OLD_PROCESS_WAIT_LIMIT {
            return Err(format!("old process {old_pid} did not exit in time"));
        }
        std::thread::sleep(PROCESS_POLL_INTERVAL);
    }

    let status = Command::new("/usr/bin/open")
        .arg(bundle)
        .status()
        .map_err(|error| {
            format!(
                "could not ask LaunchServices to open {}: {error}",
                bundle.display()
            )
        })?;
    if !status.success() {
        return Err(format!(
            "LaunchServices failed to open {} (status {status})",
            bundle.display()
        ));
    }
    Ok(())
}

#[link(name = "System")]
extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}

fn process_is_running(pid: i32) -> Result<bool, String> {
    // Signal zero checks process existence without delivering a signal.
    if unsafe { kill(pid, 0) } == 0 {
        return Ok(true);
    }
    match std::io::Error::last_os_error().kind() {
        std::io::ErrorKind::NotFound => Ok(false),
        std::io::ErrorKind::PermissionDenied => Ok(true),
        error => Err(format!("could not check old process {pid}: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_enclosing_app_bundle() {
        let executable = Path::new("/Applications/Oh My Tab.app/Contents/MacOS/oh-my-tab");
        assert_eq!(
            enclosing_app_bundle(executable),
            Some(PathBuf::from("/Applications/Oh My Tab.app"))
        );
    }

    #[test]
    fn does_not_treat_an_unbundled_binary_as_an_app() {
        assert_eq!(enclosing_app_bundle(Path::new("/tmp/oh-my-tab")), None);
    }
}
