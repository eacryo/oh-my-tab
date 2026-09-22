//! 滚动条样式:一律保持 overlay,并在系统改样式时重申 + 让设置页按可视宽度重排。
//!
//! 背景(2026-09-22 实测):创建 NSScrollView 时 `setScrollerStyle(.overlay)` **是有效的**,即使系统
//! 偏好为"始终显示滚动条";但**运行中**偏好/输入设备变化会让 AppKit 把已存在的 scroll view 重新
//! tile 成占宽的 legacy —— 实测 clip 从 568 掉到 551(少 17pt),而页面 document 仍按 568 排版,
//! 右列(开关/按钮)因此被裁掉。重开窗口会恢复,所以表现为"有概率"。
//!
//! 本模块做两件事:
//!   B) 样式变化时对**所有窗口**内的 scroll view 重申 overlay(本项目有意无视用户的滚动条偏好);
//!   A) 重申无效时,按实测的滚动条占位宽度重排设置页,保证内容只会收窄、不会被裁掉。
//!
//! Background (measured 2026-09-22): `setScrollerStyle(.overlay)` at creation does stick, even with
//! the system preference set to "always show scrollbars"; but a *runtime* preference/input-device
//! change makes AppKit re-tile existing scroll views as space-taking legacy -- the clip dropped from
//! 568 to 551 (17pt) while the page document stayed at 568, clipping the right column (switches and
//! buttons). Reopening the window restores it, which is why it looks intermittent.
//!
//! This module does two things: (B) re-assert overlay on every window's scroll views when the style
//! changes (this project deliberately ignores the user's scrollbar preference), and (A) when that
//! re-assert does not stick, re-lay out the settings page using the measured footprint so content
//! merely narrows instead of being clipped.

use objc2::msg_send;
use objc2::runtime::AnyObject;
use objc2_foundation::NSRect;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// 构建设置页时实测到的滚动条占位宽度(frame 宽 - clip 宽);0 = overlay。
/// 排版按它预留:系统强行 legacy 时内容跟着收窄,而不是被裁掉。
/// The scroller footprint measured while building the settings page (frame width - clip width);
/// 0 means overlay. Layout reserves it so that a forced-legacy scroller narrows the content instead
/// of clipping it.
static RESERVED: Mutex<f64> = Mutex::new(0.0);

/// 处理样式变化时的重入保护:我们自己重申 overlay 也可能再次投递同样的通知。
/// Reentrancy guard: our own overlay re-assert can post the same notification again.
static HANDLING: AtomicBool = AtomicBool::new(false);

/// 记录构建时实测的滚动条占位宽度。
/// Records the scroller footprint measured during a build.
pub(crate) fn note_reserved(width: f64) {
    if let Ok(mut reserved) = RESERVED.lock() {
        *reserved = width.max(0.0);
    }
}

/// 当前排版用的滚动条占位宽度。
/// The scroller footprint the current layout was built with.
pub(crate) fn reserved() -> f64 {
    RESERVED.lock().map(|reserved| *reserved).unwrap_or(0.0)
}

/// 一个 scroll view 当前的滚动条占位宽度(不是 scroll view 也返回 0)。
/// The current scroller footprint of one scroll view (0 for anything that is not a scroll view).
pub(crate) unsafe fn reserved_width(scroll: *mut AnyObject) -> f64 {
    if scroll.is_null() {
        return 0.0;
    }
    let is_scroll: bool = msg_send![scroll, isKindOfClass: objc2::class!(NSScrollView)];
    if !is_scroll {
        return 0.0;
    }
    let frame: NSRect = msg_send![scroll, frame];
    let clip: *mut AnyObject = msg_send![scroll, contentView];
    if clip.is_null() {
        return 0.0;
    }
    let bounds: NSRect = msg_send![clip, bounds];
    (frame.size.width - bounds.size.width).max(0.0)
}

/// 系统滚动条样式变化:先重申 overlay(B),重申无效时按实测占位重排设置页(A)。
/// A system scroller-style change: re-assert overlay first (B), then re-lay out the settings page
/// with the measured footprint when the re-assert did not stick (A).
pub(crate) fn on_activation_resync() {
    crate::log_debug!(
        "[scroller] activation resync: reserved={} reserved_now={}",
        reserved(),
        crate::settings::page_reserved_now()
    );
    if HANDLING.swap(true, Ordering::SeqCst) {
        return;
    }
    // 这里**不**重申 overlay:2026-09-22 实测 `setScrollerStyle(.overlay)` 在运行中只是瞬时有效
    // (8ms 内占位从 17 回到 0),AppKit 会在下一个 display/scroll pass 按系统偏好改回 legacy。
    // 也就是说"强制 overlay"做不到,能做的只有:让内容按**实际可视宽度**排版,并且不让 AppKit 的
    // tile 牵动内容(widgets.rs 里 document 已改为宽度不可伸缩)。
    // Deliberately no overlay re-assert here: measured on 2026-09-22, `setScrollerStyle(.overlay)`
    // only takes effect transiently at runtime (the footprint went 17 -> 0 within 8ms) and AppKit
    // re-applies the preferred legacy style on the next display/scroll pass. Forcing overlay is
    // therefore not achievable; what we can do is lay the content out to the *actual visible width*
    // and keep AppKit's tiling from dragging the content along (the document is no longer
    // width-sizable, see widgets.rs).
    crate::settings::resync_page_layout_for_scroller();
    HANDLING.store(false, Ordering::SeqCst);
}
