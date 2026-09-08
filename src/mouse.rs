//! 鼠标增强功能模块。
//! 提供滚轮两分支模式:默认(透传+可反转)/按行(固定行数)。
//!
//! Mouse enhancement module.
//! Provides two scroll modes: Default (passthrough + optional reverse) and Line (fixed line
//! count).

pub(crate) mod device;
pub(crate) mod event_tap;
pub(crate) mod ffi;
pub(crate) mod keysim;
pub(crate) mod pointer;
pub(crate) mod resolve;
pub(crate) mod scrolling;
pub(crate) mod shortcut;
pub(crate) mod system_action;

use crate::log_info;

/// 鼠标事件线程运行状态。停止期间不允许新线程抢跑,避免共享取消标志发生竞态。
/// Mouse event-thread lifecycle state. A new thread cannot race ahead while stopping, avoiding a
/// race on the shared cancellation flag.
struct MouseRuntime {
    thread: Option<std::thread::JoinHandle<()>>,
    stopping: bool,
}

static MOUSE_RUNTIME: std::sync::Mutex<MouseRuntime> = std::sync::Mutex::new(MouseRuntime {
    thread: None,
    stopping: false,
});

/// 运行时启用鼠标控制(设置页热切换 / 启动路径共用)。
/// 幂等:已运行时再调用不重复建线程(线程可能已自然退出,如 tap 创建失败)。
///
/// Enable mouse control at runtime (shared by the settings hot-switch and the startup path).
/// Idempotent: no-op when already running (the thread may have exited naturally, e.g. tap
/// creation failed).
pub(crate) fn start() {
    let finished = {
        let mut runtime = MOUSE_RUNTIME.lock().unwrap();
        // 停止仍在后台收尾时,由收尾线程根据最新 CONFIG 决定是否启动。
        // While stopping is being finalized in the background, the reaper decides whether to
        // restart based on the latest CONFIG value.
        if runtime.stopping {
            return;
        }
        // 已运行且线程仍活着 -> 不重复启动。finished 句柄在锁外回收,避免持锁 join。
        // Already running and the thread is alive -> don't start again. Reap a finished handle
        // outside the lock so joining never blocks other lifecycle calls.
        let finished = runtime
            .thread
            .as_ref()
            .filter(|handle| handle.is_finished())
            .is_some();
        if !finished && runtime.thread.is_some() {
            return;
        }
        let finished = if finished {
            runtime.thread.take()
        } else {
            None
        };
        runtime.thread = Some(event_tap::start());
        finished
    };
    if let Some(handle) = finished {
        let _ = handle.join();
    }
    log_info!("Mouse control enabled.");
}

/// 运行时停用鼠标控制(设置页热切换)。
/// 停止信号即时发出,线程回收在后台完成,避免阻塞 AppKit 主线程。
///
/// Disable mouse control at runtime (settings hot-switch).
/// The stop signal is sent immediately and thread reclamation happens in the background so the
/// AppKit main thread never blocks.
pub(crate) fn stop() {
    let handle = {
        let mut runtime = MOUSE_RUNTIME.lock().unwrap();
        // 幂等:已经在后台停止时不重复创建回收线程。
        // Idempotent: do not spawn duplicate reapers while a stop is already in progress.
        if runtime.stopping {
            return;
        }
        runtime.stopping = true;
        runtime.thread.take()
    };

    // 只在当前线程发停止请求;耗时的 join 放到后台,避免阻塞 AppKit 的鼠标回调。
    // Signal stop on the current thread, but move the potentially blocking join to a background
    // reaper so AppKit's mouse callback never stalls.
    event_tap::stop();
    std::thread::spawn(move || {
        if let Some(handle) = handle {
            let _ = handle.join();
        }

        {
            let mut runtime = MOUSE_RUNTIME.lock().unwrap();
            runtime.stopping = false;
        }
        // 如果停止完成前用户又点了打开,这里会按最新配置补启,且此时旧线程已完全退出。
        // If the user re-enabled the feature before stopping finished, restart from the latest
        // config only after the old thread has fully exited.
        let should_restart = crate::config::CONFIG
            .read()
            .map(|cfg| cfg.mouse.enabled)
            .unwrap_or(false);
        if should_restart {
            start();
        }
    });
    log_info!("Mouse control disabled.");
}
