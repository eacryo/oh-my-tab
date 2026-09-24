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

/// The scroller footprint measured while building the settings page (frame width - clip width);
/// 0 means overlay. Layout reserves it so that a forced-legacy scroller narrows the content instead
/// of clipping it.
static RESERVED: Mutex<f64> = Mutex::new(0.0);

/// Reentrancy guard: our own overlay re-assert can post the same notification again.
static HANDLING: AtomicBool = AtomicBool::new(false);

/// Records the scroller footprint measured during a build.
pub(crate) fn note_reserved(width: f64) {
    if let Ok(mut reserved) = RESERVED.lock() {
        *reserved = width.max(0.0);
    }
}

/// The scroller footprint the current layout was built with.
pub(crate) fn reserved() -> f64 {
    RESERVED.lock().map(|reserved| *reserved).unwrap_or(0.0)
}

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
    // Deliberately no overlay re-assert here: measured on 2026-09-22, `setScrollerStyle(.overlay)`
    // only takes effect transiently at runtime (the footprint went 17 -> 0 within 8ms) and AppKit
    // re-applies the preferred legacy style on the next display/scroll pass. Forcing overlay is
    // therefore not achievable; what we can do is lay the content out to the *actual visible width*
    // and keep AppKit's tiling from dragging the content along (the document is no longer
    // width-sizable, see widgets.rs).
    crate::settings::resync_page_layout_for_scroller();
    HANDLING.store(false, Ordering::SeqCst);
}
