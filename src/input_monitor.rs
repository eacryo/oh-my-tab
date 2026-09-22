//! Runtime Accessibility permission supervision for every global input tap.
//!
//! 所有全局输入 tap 共用一份运行时权限状态。撤权时先关闭创建入口，再通知各 tap
//! 禁用自身并退出；恢复授权后按当前配置重新启动。UserInput 禁用按本次进程生命周期
//! 的终止信号处理，避免看门狗与系统反复争抢 tap。
//!
//! All global input taps share one runtime permission state. Revocation closes the creation
//! gate before asking each tap to disable itself and exit. Restored permission restarts services
//! from current configuration. UserInput disable is terminal for this process so the watchdog
//! cannot fight the system by repeatedly re-enabling the tap.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use std::time::Instant;

static ACCESSIBILITY_TRUSTED: AtomicBool = AtomicBool::new(false);
static USER_INPUT_DISABLED: AtomicBool = AtomicBool::new(false);
static STOP_SERVICES_PENDING: AtomicBool = AtomicBool::new(false);
static STARTED: AtomicBool = AtomicBool::new(false);

const POLL_INTERVAL: Duration = Duration::from_millis(200);

pub(crate) fn start() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }

    ACCESSIBILITY_TRUSTED.store(crate::ffi::has_accessibility_permission(), Ordering::SeqCst);
    std::thread::Builder::new()
        .name("accessibility-permission-monitor".into())
        .spawn(supervise)
        .expect("failed to start Accessibility permission monitor");
}

pub(crate) fn taps_allowed() -> bool {
    ACCESSIBILITY_TRUSTED.load(Ordering::SeqCst) && !USER_INPUT_DISABLED.load(Ordering::SeqCst)
}

pub(crate) fn watchdog_may_enable_tap() -> bool {
    let trusted = crate::ffi::has_accessibility_permission();
    if !trusted {
        if ACCESSIBILITY_TRUSTED.swap(false, Ordering::SeqCst) {
            crate::log_info!("Accessibility permission was revoked; stopping global input taps.");
            STOP_SERVICES_PENDING.store(true, Ordering::SeqCst);
        }
        return false;
    }
    taps_allowed()
}

/// Called for Core Graphics' two tap-disabled pseudo-events. Returns true when the event
/// should be passed through as a pseudo-event rather than treated as ordinary input.
pub(crate) fn handle_disabled_event(event_type: crate::event_tap::CGEventType, name: &str) -> bool {
    match event_type {
        crate::event_tap::TAP_DISABLED_BY_TIMEOUT => {
            crate::log_info!(
                "[{}] event tap disabled after timeout; watchdog may recover it.",
                name
            );
            true
        }
        crate::event_tap::TAP_DISABLED_BY_USER_INPUT => {
            if !USER_INPUT_DISABLED.swap(true, Ordering::SeqCst) {
                crate::log_info!(
                    "[{}] event tap disabled by user input; stopping all global input taps until restart.",
                    name
                );
                STOP_SERVICES_PENDING.store(true, Ordering::SeqCst);
            }
            true
        }
        _ => false,
    }
}

fn supervise() {
    let mut next_reconcile = Instant::now() + Duration::from_secs(2);
    loop {
        let trusted = crate::ffi::has_accessibility_permission();
        let previous = ACCESSIBILITY_TRUSTED.swap(trusted, Ordering::SeqCst);

        if previous && !trusted {
            crate::log_info!("Accessibility permission was revoked; stopping global input taps.");
            STOP_SERVICES_PENDING.store(true, Ordering::SeqCst);
        }

        if STOP_SERVICES_PENDING.swap(false, Ordering::SeqCst) {
            stop_services();
        }

        if !previous && trusted {
            if USER_INPUT_DISABLED.load(Ordering::SeqCst) {
                // Never re-enable the disabled tap. A fresh process clears this terminal latch
                // and creates new taps only after the OS reports Accessibility as trusted.
                // 不重新启用已被系统禁用的 tap。通过新进程清除此终止标记，并在系统确认授权后创建新 tap。
                crate::restart::accessibility_restored_after_terminal_tap();
            } else {
                crate::log_info!(
                    "Accessibility permission was restored; restarting configured input taps."
                );
                start_configured_services();
                next_reconcile = Instant::now() + Duration::from_secs(2);
            }
        } else if trusted
            && !USER_INPUT_DISABLED.load(Ordering::SeqCst)
            && Instant::now() >= next_reconcile
        {
            // 定期重试可覆盖旧 tap 线程仍在收尾或创建临时失败的情况。
            // Periodic reconciliation retries services whose old tap thread was still unwinding
            // or whose tap creation failed transiently.
            start_configured_services();
            next_reconcile = Instant::now() + Duration::from_secs(2);
        }

        std::thread::sleep(POLL_INTERVAL);
    }
}

fn stop_services() {
    // Store the global gate before this function is called, so callbacks already in flight
    // immediately become pass-through while each module disables its Mach port.
    crate::event_monitor::stop();
    crate::mouse::stop();
    crate::window_management::stop();
    crate::quick_actions::stop();
    crate::settings::cancel_recording_from_main();
}

fn start_configured_services() {
    crate::event_monitor::start();
    let config = match crate::config::CONFIG.read() {
        Ok(config) => config.clone(),
        Err(_) => return,
    };
    if config.mouse.enabled {
        crate::mouse::start();
    }
    if config.window_control.enabled {
        crate::window_management::start();
    }
    if config.quick_actions.enabled {
        crate::quick_actions::start();
    }
}
