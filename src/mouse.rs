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

use crate::log_info;

/// 鼠标事件线程句柄,供 start()/stop() 幂等启停与 join。
/// The mouse event thread handle, for idempotent start()/stop() and join.
static MOUSE_THREAD: std::sync::Mutex<Option<std::thread::JoinHandle<()>>> =
    std::sync::Mutex::new(None);

/// 运行时启用鼠标控制(设置页热切换 / 启动路径共用)。
/// 幂等:已运行时再调用不重复建线程(线程可能已自然退出,如 tap 创建失败)。
///
/// Enable mouse control at runtime (shared by the settings hot-switch and the startup path).
/// Idempotent: no-op when already running (the thread may have exited naturally, e.g. tap
/// creation failed).
pub(crate) fn start() {
    let mut guard = MOUSE_THREAD.lock().unwrap();
    // 已运行且线程仍活着 -> 不重复启动。is_finished 判断避免"句柄在但线程已死"时重启失败。
    // Already running and the thread is alive -> don't start again. is_finished covers the
    // "handle exists but thread already dead" case.
    if guard.as_ref().is_some_and(|h| !h.is_finished()) {
        return;
    }
    *guard = Some(event_tap::start());
    log_info!("Mouse control enabled.");
}

/// 运行时停用鼠标控制(设置页热切换)。
/// 幂等:未运行时无操作;线程已结束时 join() 立即返回。
///
/// Disable mouse control at runtime (settings hot-switch).
/// Idempotent: no-op when not running; join() returns immediately if the thread already ended.
pub(crate) fn stop() {
    event_tap::stop();
    let handle = MOUSE_THREAD.lock().unwrap().take();
    if let Some(h) = handle {
        let _ = h.join();
    }
    log_info!("Mouse control disabled.");
}
