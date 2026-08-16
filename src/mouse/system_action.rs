//! 系统动作:经 Dock 私有 API 触发 Mission Control / Launchpad / 显示桌面 / App Expose。
//! `CoreDockSendNotification` 是 ApplicationServices 的私有符号(LinearMouse DockKitC
//! 同款声明)——直接向 Dock 发通知字符串,不经过键盘事件合成,所以不受"合成事件
//! 无法触发系统级快捷键"的限制。
//!
//! System actions: trigger Mission Control / Launchpad / Show Desktop / App Expose through
//! the Dock's private API. `CoreDockSendNotification` is a private ApplicationServices
//! symbol (same declaration as LinearMouse's DockKitC) -- it sends a notification string
//! straight to the Dock, bypassing keyboard-event synthesis, so it is not subject to the
//! "synthetic events can't trigger system-level shortcuts" limitation.

use crate::ffi::make_nsstring;
use crate::log_debug;
use std::ffi::c_void;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    // 私有 SPI:向 Dock 发送系统功能通知(第二个参数恒为 0)。
    // Private SPI: send a system-function notification to the Dock (2nd arg is always 0).
    fn CoreDockSendNotification(notification: *mut c_void, unknown: i32) -> i32;
}

/// 触发一个系统动作(按下侧键时调用一次;Dock 通知是 toggle 语义,无需配对释放)。
/// Fire a system action (called once on button press; Dock notifications are toggles, no
/// paired release needed).
pub(crate) fn fire(notification: &'static str) {
    unsafe {
        let ns = make_nsstring(notification);
        let _ = CoreDockSendNotification(ns as *mut c_void, 0);
        crate::ffi::CFRelease(ns as *const c_void);
    }
    log_debug!("[mouse] system action: {}", notification);
}
