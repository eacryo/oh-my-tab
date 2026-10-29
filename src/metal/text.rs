//! Text rasterization for the Metal renderer: a single line of an attributed
//! string becomes a premultiplied BGRA bitmap via CoreText (the same shaping the
//! AppKit rows get), uploaded to a texture and cached. This is the analogue of
//! mygo's text run -> glyph path, at per-string granularity: cheaper to build
//! than a glyph atlas and still shaped by CoreText, so CJK, emoji, link runs and
//! truncation ellipses come out identical to what the old NSTextField drew.

use super::render::Renderer;
use super::sys::*;
use crate::ffi::{
    make_nsstring, nsstring_to_rust, release_obj, CGBitmapContextCreate, CGBitmapContextGetData,
    CGColorSpaceCreateDeviceRGB, CGContextDrawImage, CFRelease, CGRect, MainThreadSlot,
};
use crate::hash::fnv1a64;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use std::collections::HashMap;
use std::ffi::c_void;

/// A rasterized single line: the texture plus the point metrics the painter
/// needs to sit it on a baseline. `w`/`h` include a one-point bleed pad on
/// every side so glyph overhang (italics, emoji) is not clipped.
#[derive(Clone, Copy)]
pub(crate) struct TextBitmap {
    pub(crate) texture: *mut AnyObject,
    pub(crate) w: f64,
    pub(crate) h: f64,
    /// Distance from the bitmap's top to the text baseline, in points
    /// (reserved for baseline-aligned layout when multi-line text lands).
    #[allow(dead_code)]
    pub(crate) ascent: f64,
}

struct Cache {
    textures: HashMap<u64, TextBitmap>,
}

static CACHE: MainThreadSlot<Option<Cache>> = unsafe { MainThreadSlot::new(None) };
const CACHE_CAP: usize = 1024;

fn with_cache<R>(f: impl FnOnce(&mut Cache) -> R) -> R {
    let mut slot = CACHE.lock().unwrap();
    let cache = slot.get_or_insert_with(|| Cache {
        textures: HashMap::new(),
    });
    f(cache)
}

/// Drop every cached bitmap (palette/material change, memory pressure).
#[allow(dead_code)]
pub(crate) fn clear_text_cache() {
    with_cache(|cache| unsafe {
        for (_, bmp) in cache.textures.drain() {
            release_obj(bmp.texture);
        }
    });
}

/// The bitmap context's drawing pad, in points (one on each side).
pub(crate) const PAD_PT: f64 = 1.0;

/// Rasterize one line of `attr` at `scale`, truncating with an ellipsis at
/// `max_w` points (`f64::INFINITY` for none). The cache key covers the string
/// content and `style_tag`, which must capture every visual input the caller
/// baked into `attr` (palette tokens, material, font choices).
pub(crate) unsafe fn rasterize_attributed(
    renderer: &Renderer,
    attr: *mut AnyObject,
    style_tag: u64,
    scale: f64,
    max_w: f64,
) -> Option<TextBitmap> {
    if attr.is_null() || scale <= 0.0 {
        return None;
    }
    let ns: *mut AnyObject = msg_send![attr, string];
    let content = if ns.is_null() {
        String::new()
    } else {
        nsstring_to_rust(ns)
    };
    if content.is_empty() {
        return None;
    }
    let key = fnv1a64(
        format!(
            "{}|{:016x}|{}|{}",
            content,
            style_tag,
            scale.to_bits(),
            max_w.to_bits()
        )
        .as_bytes(),
    );
    if let Some(hit) = with_cache(|cache| cache.textures.get(&key).copied()) {
        return Some(hit);
    }

    let line = CTLineCreateWithAttributedString(attr as *const c_void);
    if line.is_null() {
        return None;
    }
    let (mut ascent, mut descent, mut leading) = (0.0f64, 0.0f64, 0.0f64);
    let mut width = CTLineGetTypographicBounds(line, &mut ascent, &mut descent, &mut leading);
    let mut draw_line = line;
    if width > max_w && max_w.is_finite() && max_w > 0.0 {
        // Ellipsis token in the same style as the text it ends.
        let ell = make_nsstring("…");
        let token = CTLineCreateWithAttributedString(ell as *const c_void);
        CFRelease(ell as *const c_void);
        let truncated = CTLineCreateTruncatedLine(line, max_w, CT_LINE_TRUNCATION_END, token);
        if !token.is_null() {
            CFRelease(token);
        }
        if !truncated.is_null() {
            draw_line = truncated;
            width = CTLineGetTypographicBounds(draw_line, &mut ascent, &mut descent, &mut leading);
        }
    }
    let w_pt = width.ceil() + PAD_PT * 2.0;
    let h_pt = (ascent + descent).ceil() + PAD_PT * 2.0;
    let bmp = draw_bitmap(renderer, draw_line, w_pt, h_pt, ascent, descent, scale);
    if draw_line != line {
        CFRelease(draw_line);
    }
    CFRelease(line);
    let bmp = bmp?;
    with_cache(|cache| {
        if cache.textures.len() >= CACHE_CAP {
            unsafe {
                for (_, old) in cache.textures.drain() {
                    release_obj(old.texture);
                }
            }
        }
        cache.textures.insert(key, bmp)
    });
    Some(bmp)
}

unsafe fn draw_bitmap(
    renderer: &Renderer,
    line: *mut c_void,
    w_pt: f64,
    h_pt: f64,
    ascent: f64,
    _descent: f64,
    scale: f64,
) -> Option<TextBitmap> {
    let w_px = (w_pt * scale).ceil().max(1.0) as usize;
    let h_px = (h_pt * scale).ceil().max(1.0) as usize;
    let space = CGColorSpaceCreateDeviceRGB();
    let ctx = CGBitmapContextCreate(
        std::ptr::null_mut(),
        w_px,
        h_px,
        8,
        w_px * 4,
        space as *mut c_void,
        BITMAP_INFO_BGRA_PREMUL,
    );
    if !space.is_null() {
        CFRelease(space);
    }
    if ctx.is_null() {
        return None;
    }
    CGContextSetShouldAntialias(ctx, true);
    // Draw at the content scale: user space stays points.
    CGContextScaleCTM(ctx, scale, scale);
    CGContextSetTextMatrix(ctx, AFFINE_IDENTITY);
    // Baseline sits `ascent` below the pad in a y-up CG context: y = h - pad - ascent
    // measured from the bottom. Memory row 0 is the bottom row; the shader flips v.
    CGContextSetTextPosition(ctx, PAD_PT, h_pt - PAD_PT - ascent);
    CTLineDraw(line, ctx);
    let data = CGBitmapContextGetData(ctx);
    if data.is_null() {
        CFRelease(ctx);
        return None;
    }
    let pixels = std::slice::from_raw_parts(data as *const u8, w_px * h_px * 4).to_vec();
    CFRelease(ctx);
    let texture = renderer.make_texture(w_px, h_px, &pixels);
    if texture.is_null() {
        return None;
    }
    Some(TextBitmap {
        texture,
        w: w_px as f64 / scale,
        h: h_px as f64 / scale,
        ascent: ascent + PAD_PT,
    })
}

/// Rasterize an NSImage (the clipboard's composited row thumbnails) into a
/// texture of the image's point size times `scale`.
pub(crate) unsafe fn rasterize_nsimage(
    renderer: &Renderer,
    image: *mut AnyObject,
    scale: f64,
) -> Option<*mut AnyObject> {
    if image.is_null() || scale <= 0.0 {
        return None;
    }
    let size: objc2_foundation::NSSize = msg_send![image, size];
    if size.width <= 0.0 || size.height <= 0.0 {
        return None;
    }
    let w_px = (size.width * scale).ceil().max(1.0) as usize;
    let h_px = (size.height * scale).ceil().max(1.0) as usize;
    let space = CGColorSpaceCreateDeviceRGB();
    let ctx = CGBitmapContextCreate(
        std::ptr::null_mut(),
        w_px,
        h_px,
        8,
        w_px * 4,
        space as *mut c_void,
        BITMAP_INFO_BGRA_PREMUL,
    );
    if !space.is_null() {
        CFRelease(space);
    }
    if ctx.is_null() {
        return None;
    }
    let mut proposed = CGRect {
        x: 0.0,
        y: 0.0,
        w: size.width,
        h: size.height,
    };
    // CGImageForProposedRect takes a CGRect*: raw send (no RefEncode on ffi::CGRect,
    // and the runtime signature spells the pointer type differently anyway).
    type CgForRect = unsafe extern "C" fn(
        *mut AnyObject,
        objc2::runtime::Sel,
        *mut CGRect,
        *mut AnyObject,
        *mut AnyObject,
    ) -> *mut c_void;
    let f: CgForRect = std::mem::transmute(crate::ffi::objc_msgSend as *const ());
    let cg: *mut c_void = f(
        image,
        objc2::sel!(CGImageForProposedRect:context:hints:),
        &mut proposed,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
    );
    if cg.is_null() {
        CFRelease(ctx);
        return None;
    }
    CGContextDrawImage(
        ctx,
        CGRect {
            x: 0.0,
            y: 0.0,
            w: w_px as f64,
            h: h_px as f64,
        },
        cg,
    );
    let data = CGBitmapContextGetData(ctx);
    if data.is_null() {
        CFRelease(ctx);
        return None;
    }
    let pixels = std::slice::from_raw_parts(data as *const u8, w_px * h_px * 4).to_vec();
    CFRelease(ctx);
    let texture = renderer.make_texture(w_px, h_px, &pixels);
    if texture.is_null() {
        None
    } else {
        Some(texture)
    }
}

/// rgba of an NSColor as straight-alpha floats, resolved into the sRGB space.
pub(crate) unsafe fn nscolor_f4(color: *mut AnyObject) -> [f32; 4] {
    if color.is_null() {
        return [0.0; 4];
    }
    let space: *mut AnyObject = msg_send![class!(NSColorSpace), sRGBColorSpace];
    let c: *mut AnyObject = msg_send![color, colorUsingColorSpace: space];
    let c = if c.is_null() { color } else { c };
    let (mut r, mut g, mut b, mut a) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    let _: () = msg_send![
        c,
        getRed: &mut r as *mut f64,
        green: &mut g as *mut f64,
        blue: &mut b as *mut f64,
        alpha: &mut a as *mut f64
    ];
    [r as f32, g as f32, b as f32, a as f32]
}

/// A flat attributed string in the panel's typography, for ad-hoc labels the
/// picker draws (group headers, footer, placeholder). `color` is an NSColor so
/// the material-aware `glass::panel_ink` ink reaches the bitmap unchanged.
pub(crate) unsafe fn make_text_attr(
    text: &str,
    font: *mut AnyObject,
    color: *mut AnyObject,
) -> *mut AnyObject {
    let attrs: *mut AnyObject = msg_send![class!(NSMutableDictionary), alloc];
    let attrs: *mut AnyObject = msg_send![attrs, init];
    let font_key = make_nsstring("NSFont");
    let color_key = make_nsstring("NSColor");
    let _: () = msg_send![attrs, setObject: font, forKey: font_key];
    let _: () = msg_send![attrs, setObject: color, forKey: color_key];
    CFRelease(font_key as *const c_void);
    CFRelease(color_key as *const c_void);
    let ns = make_nsstring(text);
    let attr: *mut AnyObject = msg_send![class!(NSAttributedString), alloc];
    let attr: *mut AnyObject = msg_send![attr, initWithString: ns, attributes: attrs];
    CFRelease(ns as *const c_void);
    release_obj(attrs);
    attr // +1, caller releases
}
