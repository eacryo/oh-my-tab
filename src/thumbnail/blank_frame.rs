//! Blank-frame detection for background-suspended WKWebView windows.

use super::*;

// Blank-frame detection for background-suspended WKWebView windows: the page of a
// WKWebView-based app (Tauri/Electron/wry) renders in a separate WebContent
// process; once the window stays backgrounded/occluded, macOS suspends it and drops
// the WindowServer-side content surface, so captures degrade to the host-drawn
// title bar (traffic lights) over a pixel-uniform solid body (usually white). No
// public API can force another process to redraw; the answer adopted here is to
// capture while frontmost and never let a blank frame overwrite the
// last-known-good thumbnail. Before any
// cache write each frame is classified:
// - background + cached frame: dropped, keeping the last-known-good image (one-way)
// - background + empty cache: stored as a placeholder seed (frontmost captures
//   upgrade it automatically afterwards)
// - frontmost: stored as-is (the user is literally looking at it)
// - still blank on an activation refresh: dropped with one delayed retry
// - appearance-transition recapture: blank overwrites too (a stale-appearance
//   frame amid the new theme looks worse than a placeholder)

/// The Device RGB color space is immutable and thread-safe; one process-wide
/// instance is shared by downscaling and blank analysis.
pub(super) static DEVICE_RGB_COLOR_SPACE: LazyLock<RetainedCf<c_void>> =
    LazyLock::new(|| unsafe { RetainedCf::from_retained(CGColorSpaceCreateDeviceRGB()) });

/// Sampling longest edge for blank analysis: the frame is redrawn into a small
/// (<=64px) RGBA bitmap first; per-frame cost stays in the microsecond range.
const BLANK_SAMPLE_MAX_DIM: u32 = 64;
/// Content rows start below the title bar / toolbar strip: a suspended WebView's
/// title bar is host-drawn and still renders normally, so it must be excluded from
/// the near-uniform test. A 28pt title bar spans 2.3%~7% of 400~1200pt-tall windows;
/// 12% covers the bar plus a common toolbar.
const BLANK_TITLE_STRIP_FRACTION: f64 = 0.12;
/// A single quantized color bucket covering >=99% of content rows classifies as
/// blank: suspended WebView bodies are pixel-uniform (coverage ~1.0) while real UIs
/// (sidebars/text/controls/borders) never approach 99%. Buckets quantize channels
/// to 5 bits so mild compression noise cannot fake a negative.
pub(super) const BLANK_MODAL_COVERAGE_MIN: f64 = 0.99;
/// Retry delay after a blank activation refresh: gives the WebContent process time
/// to restore and finish a redraw pass (a blank frame at the initial 350ms means
/// restoration is slow; the retry lands at a ~1.25s total window).
pub(super) const ACTIVATION_BLANK_RETRY_MS: u64 = 900;

/// Window keys with a delayed retry scheduled. A slot is released when a frame is finally
/// stored, the task terminates unsuccessfully/stale, the retry is abandoned (focus lost /
/// generation changed), or the app terminates; each activation chain retries at most once.
pub(super) static PENDING_BLANK_RETRIES: LazyLock<Mutex<HashSet<ThumbKey>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// Coverage of the single most common color bucket over the content rows (below the
/// title strip); None when there is nothing to measure. Pure function over an RGBA
/// byte buffer so it is unit-testable without CoreGraphics.
pub(super) fn blank_modal_coverage(
    rgba: &[u8],
    w: usize,
    skip_rows: usize,
    h: usize,
) -> Option<f64> {
    if w == 0 || h <= skip_rows || rgba.len() < w * h * 4 {
        return None;
    }
    let mut counts: HashMap<u16, usize> = HashMap::new();
    let mut total = 0usize;
    for y in skip_rows..h {
        let row = y * w * 4;
        for x in 0..w {
            let i = row + x * 4;
            let bucket = (((rgba[i] as u16) >> 3) << 10)
                | (((rgba[i + 1] as u16) >> 3) << 5)
                | ((rgba[i + 2] as u16) >> 3);
            *counts.entry(bucket).or_default() += 1;
            total += 1;
        }
    }
    let modal = counts.values().copied().max()?;
    Some(modal as f64 / total as f64)
}

/// What to do with a blank frame (pure function for matrix testing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BlankFrameAction {
    /// A blank FRONTMOST window is what the user literally sees; store it.
    Store,
    /// The FIRST frame of a background window (empty cache): store as a placeholder
    /// seed. A suspended WebView only yields title bar + solid body, yet a
    /// placeholder beats an icon card -- any later frontmost capture (activation /
    /// frontmost summon) upgrades it to the real page, and the gating makes that
    /// upgrade one-way.
    StoreSeed,
    /// Appearance (light/dark) transition recapture: blank overwrites too. The old
    /// frame carries stale-appearance pixels that clash with every other card worse
    /// than a temporary blank; the real page returns on the next frontmost capture.
    StoreAppearanceRefresh,
    /// A blank BACKGROUND frame means a suspended WebView; drop it and keep the
    /// cached last-known-good image.
    DiscardKeepLastGood,
    /// A blank frame on an activation refresh means Web content has not repainted
    /// yet; drop it and schedule one delayed recapture.
    DiscardRetryActivation,
}

pub(super) fn blank_frame_action(
    frontmost: bool,
    activation_job: bool,
    cache_has_frame: bool,
    retry_slot_acquired: bool,
    appearance_refresh: bool,
) -> BlankFrameAction {
    // The activation blank retry outranks the appearance overwrite: the retried
    // real frame satisfies appearance consistency too, while storing now would
    // skip the 1.4s backstop retry and freeze an unfinished repaint as the
    // placeholder (a theme job merged with an activation request carries both
    // flags on one frame, so the order must be settled here).
    if frontmost && activation_job && cache_has_frame && retry_slot_acquired {
        return BlankFrameAction::DiscardRetryActivation;
    }
    if appearance_refresh {
        // Appearance consistency wins over content fidelity: blank frames store for
        // both the frontmost and background cases through this single branch.
        return BlankFrameAction::StoreAppearanceRefresh;
    }
    if !frontmost {
        return if cache_has_frame {
            BlankFrameAction::DiscardKeepLastGood
        } else {
            BlankFrameAction::StoreSeed
        };
    }
    BlankFrameAction::Store
}

/// Redraw the frame into a small (<=64px) RGBA bitmap and run the blank test.
/// Analysis failures return None and the caller treats the frame as non-blank
/// (conservatively preserving the original store behavior).
pub(super) unsafe fn frame_blankness(img: *const c_void, w_px: u32, h_px: u32) -> Option<bool> {
    if img.is_null() || w_px == 0 || h_px == 0 {
        return None;
    }
    let scale = BLANK_SAMPLE_MAX_DIM as f64 / u32::max(w_px, h_px) as f64;
    let sw = (((w_px as f64) * scale).round() as usize).max(1);
    let sh = (((h_px as f64) * scale).round() as usize).max(1);
    let ctx = CGBitmapContextCreate(
        std::ptr::null_mut(),
        sw,
        sh,
        8,
        sw * 4,
        DEVICE_RGB_COLOR_SPACE.ptr,
        BITMAP_PREMULTIPLIED_LAST,
    );
    if ctx.is_null() {
        return None;
    }
    CGContextDrawImage(
        ctx,
        CGRect {
            x: 0.0,
            y: 0.0,
            w: sw as f64,
            h: sh as f64,
        },
        img,
    );
    let data = CGBitmapContextGetData(ctx) as *const u8;
    let coverage = if data.is_null() {
        None
    } else {
        let skip_rows = ((sh as f64) * BLANK_TITLE_STRIP_FRACTION).round() as usize;
        blank_modal_coverage(
            std::slice::from_raw_parts(data, sw * sh * 4),
            sw,
            skip_rows,
            sh,
        )
    };
    CFRelease(ctx);
    Some(coverage.is_some_and(|c| c >= BLANK_MODAL_COVERAGE_MIN))
}

/// Drop a terminated app's pending blank-retry slots (called from pregen's
/// termination path).
pub(crate) fn forget_blank_retries_for_pid(pid: i32) {
    PENDING_BLANK_RETRIES
        .lock()
        .unwrap()
        .retain(|key| key.pid != pid);
}
