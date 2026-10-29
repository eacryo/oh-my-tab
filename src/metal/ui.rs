//! The immediate-mode layer the surfaces build their frame through, the mygo
//! `ui` pattern: one build pass a frame, and a widget answers its own input
//! where it is constructed (`ui.button(rect, "×")` returns whether the pending
//! click landed on that rect) instead of a second hit-test pass repeating the
//! same geometry.
//!
//! Input arrives as two Option<NSPoint>s for the frame: the cursor's latest
//! position (drives hover) and the pending mouse-down (drives `clicked`,
//! consumed once by the first widget that claims it — the topmost wins since
//! widgets are built front-to-back). Actions are collected as an `Action`
//! during the build and executed after the frame encodes, so a click's effect
//! is one frame behind — how immediate-mode toolkits work too.

use super::{render, text};
use crate::ffi::release_obj;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSPoint, NSRect, NSSize};

pub(crate) fn in_rect(p: NSPoint, r: NSRect) -> bool {
    p.x >= r.origin.x
        && p.x <= r.origin.x + r.size.width
        && p.y >= r.origin.y
        && p.y <= r.origin.y + r.size.height
}

/// The frame's build context: carries the scene being painted, the input
/// points, and the style inputs (palette-derived colors are resolved by the
/// caller, which knows the material).
pub(crate) struct Ui<'a> {
    pub(crate) scene: &'a mut render::Scene,
    pub(crate) renderer: &'a render::Renderer,
    pub(crate) scale: f64,
    cursor: Option<NSPoint>,
    click: Option<NSPoint>,
}

impl<'a> Ui<'a> {
    pub(crate) fn new(
        scene: &'a mut render::Scene,
        renderer: &'a render::Renderer,
        scale: f64,
        cursor: Option<NSPoint>,
        click: Option<NSPoint>,
    ) -> Self {
        Ui {
            scene,
            renderer,
            scale,
            cursor,
            click,
        }
    }

    /// Whether the cursor is over `rect` this frame.
    pub(crate) fn hot(&self, rect: NSRect) -> bool {
        self.cursor.map(|p| in_rect(p, rect)).unwrap_or(false)
    }

    /// Whether the pending click landed on `rect` (consumes it if so).
    pub(crate) fn clicked(&mut self, rect: NSRect) -> bool {
        match self.click {
            Some(p) if in_rect(p, rect) => {
                self.click = None;
                true
            }
            _ => false,
        }
    }

    // ---- primitives (the "base" widgets' drawn parts) ----------------------

    /// A rounded fill (hover tiles, chips, washes).
    pub(crate) fn tile(&mut self, rect: NSRect, radius: f64, color: [f32; 4]) {
        self.scene
            .fill([rect.origin.x, rect.origin.y, rect.size.width, rect.size.height], radius, color);
    }

    /// A text line rasterized via CoreText into a cached texture, drawn at
    /// `x`,`y` (top-left). Returns the bitmap for callers that measure.
    pub(crate) fn label(
        &mut self,
        s: &str,
        font: *mut AnyObject,
        color: *mut AnyObject,
        style_tag: u64,
        x: f64,
        y: f64,
        max_w: f64,
    ) -> Option<text::TextBitmap> {
        let bmp = unsafe {
            let attr = text::make_text_attr(s, font, color);
            if attr.is_null() {
                return None;
            }
            let bmp = text::rasterize_attributed(self.renderer, attr, style_tag, self.scale, max_w);
            release_obj(attr);
            bmp
        }?;
        self.scene.image(
            bmp.texture,
            [x - text::PAD_PT, y - text::PAD_PT, bmp.w, bmp.h],
            1.0,
            None,
        );
        Some(bmp)
    }

    /// A label centred inside `rect` (button glyphs, chips' caps).
    pub(crate) fn centered_label(
        &mut self,
        s: &str,
        font: *mut AnyObject,
        color: *mut AnyObject,
        style_tag: u64,
        rect: NSRect,
    ) {
        let bmp = unsafe {
            let attr = text::make_text_attr(s, font, color);
            if attr.is_null() {
                return;
            }
            let bmp = text::rasterize_attributed(
                self.renderer,
                attr,
                style_tag,
                self.scale,
                rect.size.width,
            );
            release_obj(attr);
            bmp
        };
        if let Some(bmp) = bmp {
            self.scene.image(
                bmp.texture,
                [
                    rect.origin.x + (rect.size.width - bmp.w) / 2.0 - text::PAD_PT,
                    rect.origin.y + (rect.size.height - bmp.h) / 2.0 - text::PAD_PT,
                    bmp.w,
                    bmp.h,
                ],
                1.0,
                None,
            );
        }
    }

    // ---- widgets -----------------------------------------------------------

    /// An icon button: hover tile when hot, centred glyph, returns whether the
    /// pending click landed on it — the mygo `Button(...).Clicked()` pattern.
    pub(crate) fn icon_button(
        &mut self,
        rect: NSRect,
        glyph: &str,
        font: *mut AnyObject,
        ink: *mut AnyObject,
        style_tag: u64,
        hover: [f32; 4],
        hover_radius: f64,
    ) -> bool {
        let hot = self.hot(rect);
        if hot {
            self.scene.fill(
                [
                    rect.origin.x,
                    rect.origin.y,
                    rect.size.width,
                    rect.size.height,
                ],
                hover_radius,
                hover,
            );
        }
        self.centered_label(glyph, font, ink, style_tag, rect);
        self.clicked(rect)
    }
}

/// `NSRect` shorthand for the build call sites.
pub(crate) fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}


