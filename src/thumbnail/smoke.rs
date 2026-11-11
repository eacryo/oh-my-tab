//! `--smoke-offscreen-capture`: the M2 regression for the full-window capture path.
//!
//! The defect it pins: a window dragged past the display edge was captured only up to the display
//! edge, so its card showed half a window inside a frame sized for the whole one. Size alone cannot
//! assert the fix -- a full-size frame with the off-screen part filled black would pass a size
//! check -- so the test window paints a marker in the part that lies outside every display and the
//! captured frame must still contain it. That assertion fails on the clipped private path.
//!
//! It runs as an app child (`run_smoke_child`) because creating and showing a window is AppKit
//! main-thread work, which a libtest worker thread cannot do.

use super::*;
use objc2_foundation::{NSPoint, NSRect, NSSize};

/// The test window's size in points. Wide enough that the captured frame is downscaled (so the
/// sampling path is the production one) and tall enough that a half of it is a real off-screen area.
const SMOKE_WINDOW_W: f64 = 640.0;
const SMOKE_WINDOW_H: f64 = 420.0;
/// The share of the window that hangs past the lowest display's bottom edge. The marker covers
/// exactly this part, so a clipped frame cannot contain it.
const OFFSCREEN_SHARE: f64 = 0.5;
/// The marker colour as sampled: opaque magenta, distinct from any plausible window chrome.
const MARKER_MIN_RED: u32 = 150;
const MARKER_MIN_BLUE: u32 = 150;
const MARKER_MAX_GREEN: u32 = 120;
/// The marker's share of the sampled bottom half, in percent: at least this much is the marker,
/// and less than this much in the top half is the body (the two-tone precondition).
const MARKER_MIN_SHARE: usize = 20;

/// Run the child. Records the verdict through the smoke channel and returns whether the process
/// should exit successfully: NOT RUN is a successful exit (the environment could not make the
/// assertions), a failed assertion is not.
pub(crate) fn smoke_offscreen_capture() -> bool {
    if !capture_allowed() {
        crate::smoke_verdict::record_for_child("NOT RUN Screen Recording not granted");
        return true;
    }
    unsafe { run() }
}

unsafe fn run() -> bool {
    let Some((frame, display_height)) = lowest_display_frame() else {
        crate::smoke_verdict::record_for_child("NOT RUN no screen is available");
        return true;
    };
    // The window's bottom share hangs below the lowest display, so no display can show that part.
    let origin = NSPoint::new(
        frame.origin.x + 40.0,
        frame.origin.y - SMOKE_WINDOW_H * OFFSCREEN_SHARE,
    );
    let window_frame = NSRect::new(origin, NSSize::new(SMOKE_WINDOW_W, SMOKE_WINDOW_H));
    let window = match make_marked_window(window_frame) {
        Some(window) => window,
        None => {
            crate::smoke_verdict::record_for_child(
                "NOT RUN could not create a layer-backed window",
            );
            return true;
        }
    };
    // Let the WindowServer composite the window before asking for its pixels.
    let _: () = msg_send![window, orderFrontRegardless];
    pump_run_loop(0.3);
    let _: () = msg_send![window, displayIfNeeded];
    pump_run_loop(0.2);

    let window_number: isize = msg_send![window, windowNumber];
    let wid = window_number as u32;
    let bounds = crate::window_collector::window_public_bounds(wid);
    let Some(bounds) = bounds else {
        close_window(window);
        crate::smoke_verdict::record_for_child(
            "FAILED the test window is not in the window list, so its bounds are unknown",
        );
        return false;
    };
    log_info!(
        "[smoke-offscreen] test window wid={} bounds=({:.1},{:.1},{:.1},{:.1}) display_height={:.1}",
        wid,
        bounds.0,
        bounds.1,
        bounds.2,
        bounds.3,
        display_height
    );
    // The precondition is the point of the test: a window that ended up fully on screen would make
    // the assertions meaningless, so report NOT RUN rather than a pass that proved nothing.
    if current_window_extent(Some(bounds)) != WindowExtent::PartlyOffScreen {
        close_window(window);
        crate::smoke_verdict::record_for_child(
            "NOT RUN the test window does not extend past every display",
        );
        return true;
    }

    let target_px_h = BASE_TARGET_PX_H;
    // The counter-example the assertion must beat: the private list call sees only the visible part.
    let private_size = capture_private_window(wid).map(|image| {
        let size = (
            CGImageGetWidth(image) as u32,
            CGImageGetHeight(image) as u32,
        );
        CFRelease(image);
        size
    });
    log_info!(
        "[smoke-offscreen] private-call frame={:?} (expected clipped); window bounds={:?}",
        private_size,
        bounds
    );

    let captured = capture_window(wid, target_px_h, Some(bounds));
    let Some(captured) = captured else {
        close_window(window);
        crate::smoke_verdict::record_for_child("FAILED capture_window returned no frame");
        return false;
    };
    let source = (captured.source_w_px, captured.source_h_px);
    let backend = captured.backend;
    let matches = capture_matches_window_aspect(captured.source_w_px, captured.source_h_px, bounds);
    let shares = marker_shares(captured.thumb.img, captured.thumb.w_px, captured.thumb.h_px);
    unsafe { CFRelease(captured.thumb.img) };
    close_window(window);

    log_info!(
        "[smoke-offscreen] capture backend={:?} source={}x{} aspect_match={:?} marker={:?}",
        backend,
        source.0,
        source.1,
        matches,
        shares
    );
    let Some(shares) = shares else {
        crate::smoke_verdict::record_for_child("FAILED the captured frame could not be sampled");
        return false;
    };
    if backend == CaptureBackend::FullWindowFallback {
        // A missing symbol is an environment limitation (the app degraded as designed); a symbol
        // that resolved but could not capture our own window is a real failure, and reporting it as
        // "unavailable" would hide it.
        if full_window_capture_symbol_available() {
            crate::smoke_verdict::record_for_child(
                "FAILED the full-window call returned no usable frame for the test window",
            );
            return false;
        }
        crate::smoke_verdict::record_for_child(
            "NOT RUN CGWindowListCreateImage is unavailable, so the full-window path did not run",
        );
        return true;
    }
    if backend != CaptureBackend::FullWindow {
        crate::smoke_verdict::record_for_child(
            "FAILED the window was not captured through the full-window path",
        );
        return false;
    }
    if matches != Some(true) {
        crate::smoke_verdict::record_for_child(
            "FAILED the captured frame does not have the window's aspect ratio",
        );
        return false;
    }
    // The two tones are the precondition, not the assertion: a frame painted one colour cannot
    // distinguish a clipped capture from a complete one, so say NOT RUN instead of passing.
    if shares.top >= MARKER_MIN_SHARE {
        crate::smoke_verdict::record_for_child(
            "NOT RUN the test window rendered as one colour, so the marker cannot be located",
        );
        return true;
    }
    if shares.bottom < MARKER_MIN_SHARE {
        crate::smoke_verdict::record_for_child(
            "FAILED the off-screen part of the window is missing from the captured frame",
        );
        return false;
    }
    crate::smoke_verdict::record_for_child("PASSED 3");
    true
}

/// The frame and height of the lowest display in Cocoa's global coordinate space: a window placed
/// below it hangs past every display, which is the state the test needs.
unsafe fn lowest_display_frame() -> Option<(NSRect, f64)> {
    let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
    if screens.is_null() {
        return None;
    }
    let count: usize = msg_send![screens, count];
    if count == 0 {
        return None;
    }
    let mut lowest: Option<NSRect> = None;
    for index in 0..count {
        let screen: *mut AnyObject = msg_send![screens, objectAtIndex: index];
        if screen.is_null() {
            continue;
        }
        let frame: NSRect = msg_send![screen, frame];
        lowest = Some(match lowest {
            Some(current) if current.origin.y <= frame.origin.y => current,
            _ => frame,
        });
    }
    lowest.map(|frame| (frame, frame.size.height))
}

/// A borderless window whose bottom `OFFSCREEN_SHARE` is a magenta marker under a white body.
///
/// Both halves are layer backgrounds on plain views, the one rendering path that does not need a
/// custom drawRect:. None when AppKit refused to create either layer -- the caller reports NOT RUN
/// rather than assert on a window it could not paint.
unsafe fn make_marked_window(frame: NSRect) -> Option<*mut AnyObject> {
    let window: *mut AnyObject = msg_send![class!(NSWindow), alloc];
    let window: *mut AnyObject = msg_send![
        window,
        initWithContentRect: frame,
        styleMask: 0u64, // borderless: the CG bounds are the frame, no title bar to account for
        backing: 2u64,
        defer: false
    ];
    if window.is_null() {
        return None;
    }
    let _: () = msg_send![window, setReleasedWhenClosed: false];
    let content: *mut AnyObject = msg_send![window, contentView];
    if content.is_null() {
        close_window(window);
        return None;
    }
    let magenta: *mut AnyObject = msg_send![class!(NSColor), magentaColor];
    let white: *mut AnyObject = msg_send![class!(NSColor), whiteColor];
    if !set_layer_background(content, magenta) {
        close_window(window);
        return None;
    }
    let body_rect = NSRect::new(
        NSPoint::new(0.0, frame.size.height * OFFSCREEN_SHARE),
        NSSize::new(
            frame.size.width,
            frame.size.height * (1.0 - OFFSCREEN_SHARE),
        ),
    );
    let body: *mut AnyObject = msg_send![class!(NSView), alloc];
    let body: *mut AnyObject = msg_send![body, initWithFrame: body_rect];
    if !set_layer_background(body, white) {
        crate::ffi::release_obj(body);
        close_window(window);
        return None;
    }
    let _: () = msg_send![content, addSubview: body];
    // addSubview retains the view; the alloc'd reference is ours to drop.
    crate::ffi::release_obj(body);
    Some(window)
}

/// Give a view a layer with `color` as its background; false when the layer could not be created.
///
/// The colour goes through `ffi::ns_color_to_cg` / `ffi::layer_set_background`: objc2's `msg_send!`
/// cannot encode a CGColorRef, and its signature check traps on the return type.
unsafe fn set_layer_background(view: *mut AnyObject, color: *mut AnyObject) -> bool {
    let _: () = msg_send![view, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![view, layer];
    if layer.is_null() {
        return false;
    }
    let cg_color = crate::ffi::ns_color_to_cg(color);
    if cg_color.is_null() {
        return false;
    }
    crate::ffi::layer_set_background(layer, cg_color);
    true
}

unsafe fn close_window(window: *mut AnyObject) {
    let _: () = msg_send![window, orderOut: std::ptr::null::<AnyObject>()];
    let _: () = msg_send![window, close];
    crate::ffi::release_obj(window);
}

/// Pump the main run loop for at least `seconds`: `runUntilDate:` may return as soon as it has
/// drained the current sources, so a single call is not a wait.
unsafe fn pump_run_loop(seconds: f64) {
    let deadline = Instant::now() + Duration::from_secs_f64(seconds.max(0.0));
    while Instant::now() < deadline {
        let remaining = (deadline - Instant::now()).as_secs_f64().min(0.05);
        let run_loop: *mut AnyObject = msg_send![class!(NSRunLoop), currentRunLoop];
        let date: *mut AnyObject =
            msg_send![class!(NSDate), dateWithTimeIntervalSinceNow: remaining];
        let _: () = msg_send![run_loop, runUntilDate: date];
    }
}

/// The marker's share of the sampled frame's bottom and top halves.
#[derive(Clone, Copy, Debug)]
struct MarkerShares {
    bottom: usize,
    top: usize,
}

/// Measure how much of each half of the captured frame is the marker colour. None when the frame
/// could not be sampled (the caller reports a failure rather than reading "no sample" as "no
/// marker").
unsafe fn marker_shares(img: *const c_void, w_px: u32, h_px: u32) -> Option<MarkerShares> {
    with_frame_sample(img, w_px, h_px, |sample, sw, sh| {
        let rgba = sample?;
        let middle = sh / 2;
        let mut shares = MarkerShares { bottom: 0, top: 0 };
        let mut bottom_total = 0usize;
        let mut top_total = 0usize;
        for y in 0..sh {
            for x in 0..sw {
                let index = (y * sw + x) * 4;
                let (red, green, blue) = (
                    rgba[index] as u32,
                    rgba[index + 1] as u32,
                    rgba[index + 2] as u32,
                );
                let is_marker =
                    red >= MARKER_MIN_RED && blue >= MARKER_MIN_BLUE && green <= MARKER_MAX_GREEN;
                if y < middle {
                    top_total += 1;
                    if is_marker {
                        shares.top += 1;
                    }
                } else {
                    bottom_total += 1;
                    if is_marker {
                        shares.bottom += 1;
                    }
                }
            }
        }
        if bottom_total == 0 || top_total == 0 {
            return None;
        }
        shares.bottom = shares.bottom * 100 / bottom_total;
        shares.top = shares.top * 100 / top_total;
        Some(shares)
    })
    .flatten()
}
