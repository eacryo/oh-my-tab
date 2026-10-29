//! Metal/QuartzCore/CoreText FFI for the native-UI renderer. Mirrors how mygo's
//! `internal/gpu/metal` reaches Metal without cgo: a few C entry points plus ObjC messaging.
#![allow(non_snake_case)]

use objc2::runtime::AnyObject;
use std::ffi::c_void;

#[link(name = "Metal", kind = "framework")]
extern "C" {
    /// The shared system GPU. Returns a retained-less-than-singleton handle: it is an
    /// autoreleased-style object that stays valid for the process; keep it in a Retained
    /// slot anyway by retaining at the call site.
    pub(crate) fn MTLCreateSystemDefaultDevice() -> *mut AnyObject;
}

// QuartzCore hosts CAMetalLayer/CAMetalDrawable; every member is reached by selector.
#[link(name = "QuartzCore", kind = "framework")]
extern "C" {}

#[link(name = "CoreText", kind = "framework")]
extern "C" {
    /// +1 line object from an attributed string (CFAttributedStringRef is toll-free with
    /// NSAttributedString). Release with CFRelease.
    pub(crate) fn CTLineCreateWithAttributedString(attr: *const c_void) -> *mut c_void;
    /// Typographic metrics; returns the line's width in points.
    pub(crate) fn CTLineGetTypographicBounds(
        line: *const c_void,
        ascent: *mut f64,
        descent: *mut f64,
        leading: *mut f64,
    ) -> f64;
    /// +1 truncated copy of `line` fitting `width` points. `truncationType` 2 = end; the
    /// token is another CTLine (usually an ellipsis). Release with CFRelease.
    pub(crate) fn CTLineCreateTruncatedLine(
        line: *mut c_void,
        width: f64,
        truncation_type: u32,
        truncation_token: *mut c_void,
    ) -> *mut c_void;
    pub(crate) fn CTLineDraw(line: *mut c_void, ctx: *mut c_void);
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    pub(crate) fn CGContextSetTextPosition(ctx: *mut c_void, x: f64, y: f64);
    pub(crate) fn CGContextSetShouldAntialias(ctx: *mut c_void, antialias: bool);
    pub(crate) fn CGContextSetTextMatrix(ctx: *mut c_void, t: CGAffineTransform);
    pub(crate) fn CGContextScaleCTM(ctx: *mut c_void, sx: f64, sy: f64);
    pub(crate) fn CGColorSpaceCreateWithName(name: *const c_void) -> *const c_void;
    /// CoreGraphics exports the name constants as CFStringRef globals, not
    /// string literals — read the pointer's pointee.
    pub(crate) static kCGColorSpaceSRGB: *const c_void;
}

/// CoreGraphics affine transform (a,b,c,d,tx,ty).
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct CGAffineTransform {
    pub(crate) a: f64,
    pub(crate) b: f64,
    pub(crate) c: f64,
    pub(crate) d: f64,
    pub(crate) tx: f64,
    pub(crate) ty: f64,
}

pub(crate) const AFFINE_IDENTITY: CGAffineTransform = CGAffineTransform {
    a: 1.0,
    b: 0.0,
    c: 0.0,
    d: 1.0,
    tx: 0.0,
    ty: 0.0,
};

/// MTLClearColor {red,green,blue,alpha} — set via the raw sender below because the struct's
/// type tag would otherwise have to be spelled exactly in the signature check.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct MTLClearColor {
    pub(crate) red: f64,
    pub(crate) green: f64,
    pub(crate) blue: f64,
    pub(crate) alpha: f64,
}

// MTLPixelFormat
pub(crate) const MTL_PIXEL_FORMAT_BGRA8_UNORM: u64 = 80;
// MTLTextureUsage
pub(crate) const MTL_TEXTURE_USAGE_SHADER_READ: u64 = 1;
// MTLLoadAction / MTLStoreAction
pub(crate) const MTL_LOAD_ACTION_CLEAR: u64 = 2;
pub(crate) const MTL_STORE_ACTION_STORE: u64 = 1;
// MTLPrimitiveType
pub(crate) const MTL_PRIMITIVE_TRIANGLE_STRIP: u64 = 4;
// MTLBlendFactor (premultiplied "over": one / one-minus-src-alpha)
pub(crate) const MTL_BLEND_FACTOR_ONE: u64 = 1;
pub(crate) const MTL_BLEND_FACTOR_ONE_MINUS_SOURCE_ALPHA: u64 = 5;
// MTLSamplerMinMagFilter / AddressMode / MipFilter (for completeness if a state is needed)
// kCTLineTruncationEnd
pub(crate) const CT_LINE_TRUNCATION_END: u32 = 2;

/// CGBitmapInfo for the context text/images rasterize into:
/// kCGImageAlphaPremultipliedLast alone, the codebase's known-good combination
/// (thumbnail/dev_backdrop). Byte-order-default packs a 32-bit RGBA pixel
/// memory-order R,G,B,A — the texture uploads as MTLPixelFormatRGBA8Unorm.
/// (kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little reads as the
/// BGRA pair on paper but CGBitmapContextCreate rejects it with NULL here.)
pub(crate) const BITMAP_INFO_RGBA_PREMUL: u32 = 1;

/// MTLPixelFormatRGBA8Unorm — the memory order CGBitmapContext writes.
pub(crate) const MTL_PIXEL_FORMAT_RGBA8_UNORM: u64 = 70;

/// kCALayerWidthSizable | kCALayerHeightSizable — the metal layer follows its
/// superlayer's size (mygo's `layerWidthSizable|layerHeightSizable`).
pub(crate) const LAYER_AUTORESIZE_WH: u64 = (1 << 1) | (1 << 4);


