//! Development-only controlled backdrops for the A2 pixel instruments.
//!
//! `--panel-backdrop=black|white|gray|#RRGGBB` pins a solid window behind the panels so their material
//! samples a known tone instead of the desktop. A solid tone cannot answer "is the material still
//! blurring?": both a blur and an opaque fill leave a flat surface where a texture used to be, and a
//! measurement on a flat tone therefore cannot tell the two apart.
//!
//! `--panel-backdrop=texture` paints a square-wave ladder instead. Its coarse bars (32/64px) survive a
//! blur while its fine bars (1-8px) do not, so a blurred surface keeps the ladder's *low*-frequency
//! structure and loses its high-frequency energy; a surface covered by a fill keeps neither. The A2
//! instrument (`scripts/e2e/lib/png_stats.py --hf-retention`) reads exactly that difference, and
//! `panel-contrast.sh` uses it as the gate that a hierarchy change must not break the blur.

use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::{NSRect, NSSize};
use std::ffi::c_void;

use crate::ffi::{
    layer_set_contents, layer_set_contents_scale, CFRelease, CGBitmapContextCreate,
    CGBitmapContextCreateImage, CGBitmapContextGetData, CGColorSpaceCreateDeviceRGB,
};

/// Bar widths in device pixels, alternating white/black and repeating.
///
/// Every bar's edge is 1-2px detail, so a blur removes the high-frequency energy whatever the bar width is;
/// that is what the `hf` band reads. The range is set by the *control* region instead: it is the only region
/// that is unblurred, it lives in the window's padding (64pt at most), and it needs several bars to have any
/// variance at all -- 1-8px bars give it plenty, and the widest bar the instrument's `lf` band could still
/// see through this material's blur (measured: a live blur leaves lf_control inflated, so `blurred` and
/// `covered` are not separable on this ladder) is a question the translucency check in
/// `scripts/e2e/panel-edge.sh` answers directly instead.
const BARS: [usize; 7] = [1, 2, 4, 8, 16, 32, 64];

/// `kCGImageAlphaPremultipliedLast`, i.e. R,G,B,A in memory order (`thumbnail.rs` uses the same value
/// through its own constant).
const BITMAP_PREMULTIPLIED_LAST: u32 = 1;

/// Pure: the ladder's luminance at horizontal device pixel `x`. Kept free of AppKit so the pattern the
/// A2 instrument measures is pinned by a unit test rather than by whatever the drawing code happens to
/// produce.
pub(crate) fn luminance_at(x: usize) -> u8 {
    let cycle = 2 * BARS.iter().sum::<usize>();
    let mut offset = x % cycle;
    let mut white = true;
    for width in BARS.iter().chain(BARS.iter()) {
        if offset < *width {
            return if white { 255 } else { 0 };
        }
        offset -= *width;
        white = !white;
    }
    0
}

/// Build the ladder as a +1 CGImageRef covering `size` points at `scale` device pixels per point.
/// The caller owns the returned image and releases it with `CFRelease`.
unsafe fn pattern_image(size: NSSize, scale: f64) -> *mut AnyObject {
    let width_px = ((size.width * scale).round() as usize).max(1);
    let height_px = ((size.height * scale).round() as usize).max(1);
    let stride = width_px * 4;
    let space = CGColorSpaceCreateDeviceRGB();
    if space.is_null() {
        return std::ptr::null_mut();
    }
    let context = CGBitmapContextCreate(
        std::ptr::null_mut(),
        width_px,
        height_px,
        8,
        stride,
        space,
        BITMAP_PREMULTIPLIED_LAST,
    );
    CFRelease(space as *const c_void);
    if context.is_null() {
        return std::ptr::null_mut();
    }
    let data = CGBitmapContextGetData(context) as *mut u8;
    if !data.is_null() {
        // The ladder has no vertical structure, so one row is built and blitted down: the buffer is
        // width*height*4 bytes (about 59 MB on a 5K display at 2x), which is worth not touching twice.
        let mut row = vec![0u8; stride];
        for x in 0..width_px {
            let level = luminance_at(x);
            row[x * 4] = level;
            row[x * 4 + 1] = level;
            row[x * 4 + 2] = level;
            row[x * 4 + 3] = 0xFF;
        }
        for y in 0..height_px {
            std::ptr::copy_nonoverlapping(row.as_ptr(), data.add(y * stride), stride);
        }
    }
    let image = CGBitmapContextCreateImage(context);
    CFRelease(context as *const c_void);
    image as *mut AnyObject
}

/// Show the ladder across the whole screen at the normal window level, i.e. below every panel (they sit
/// at `normal + 3`), so the material samples the ladder rather than the desktop.
pub(crate) unsafe fn show_texture() {
    let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
    let frame: NSRect = msg_send![screen, frame];
    let scale: f64 = msg_send![screen, backingScaleFactor];
    let window: *mut AnyObject = msg_send![class!(NSWindow), alloc];
    let window: *mut AnyObject =
        msg_send![window, initWithContentRect: frame, styleMask: 0u64, backing: 2u64, defer: false];
    let _: () = msg_send![window, setOpaque: true];
    let _: () = msg_send![window, setReleasedWhenClosed: false];
    let content: *mut AnyObject = msg_send![window, contentView];
    let _: () = msg_send![content, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![content, layer];
    if !layer.is_null() {
        // The image is exactly bounds*scale pixels and the layer's contentsScale is the display's, so
        // one image pixel is one device pixel: the bar widths the instrument measures are the bar widths
        // `luminance_at` produced, with no resampling in between.
        let image = pattern_image(frame.size, scale);
        if !image.is_null() {
            layer_set_contents(layer, image as *mut c_void);
            layer_set_contents_scale(layer, scale);
            CFRelease(image as *const c_void);
        }
    }
    let _: () = msg_send![window, orderFrontRegardless];
    crate::log_info!(
        "[dev-backdrop] controlled texture backdrop up ({}x{} px at scale {scale})",
        (frame.size.width * scale).round() as i64,
        (frame.size.height * scale).round() as i64
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ladder must alternate every bar and repeat exactly once per cycle: the instrument's
    /// "blur alive" judgement rests on the coarse bars still being there, which a pattern that drifted
    /// (an odd cycle length, two same-polarity neighbours) would quietly break.
    #[test]
    fn ladder_alternates_every_bar_and_repeats_once_per_cycle() {
        let cycle = 2 * BARS.iter().sum::<usize>();
        assert_eq!(cycle, 254);

        let mut expected = Vec::new();
        let mut white = true;
        for _ in 0..2 {
            for width in BARS {
                let level = if white { 255 } else { 0 };
                expected.extend(std::iter::repeat_n(level, width));
                white = !white;
            }
        }
        assert_eq!(expected.len(), cycle);
        for (x, level) in expected.iter().enumerate() {
            assert_eq!(luminance_at(x), *level, "x={x}");
            assert_eq!(luminance_at(x + cycle), *level, "x={x} (second cycle)");
        }
    }

    /// A 1px bar has to exist: it is the finest detail a blur can be asked to destroy, and the control region
    /// (which is the only unblurred one) needs several bars inside its ~40pt to have any variance at all.
    #[test]
    fn finest_bar_is_one_pixel_wide() {
        assert_eq!(BARS[0], 1);
        assert_ne!(luminance_at(0), luminance_at(1));
    }
}
