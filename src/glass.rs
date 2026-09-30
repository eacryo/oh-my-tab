//! Shared rounded material backdrop for passive floating panels.

use objc2::runtime::{AnyClass, AnyObject};
use objc2::{class, msg_send};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use crate::ffi::{hex_to_cg_color, hex_to_ns_color, layer_set_background, release_obj, ObjPtr};

pub(crate) const PANEL_CORNER_RADIUS: f64 = 16.0;
/// AppKit darkens Liquid Glass in passive panels; this alpha matches the clipboard detail panel.
pub(crate) const INACTIVE_GLASS_COMPENSATION_ALPHA: u32 = 0x8D;

#[derive(Clone, Copy)]
pub(crate) struct InstalledBackdrop {
    pub(crate) content_parent: *mut AnyObject,
    pub(crate) glass: Option<ObjPtr>,
    pub(crate) effect_view: Option<ObjPtr>,
    pub(crate) compensation_view: Option<ObjPtr>,
    pub(crate) compensation_layer: Option<ObjPtr>,
}

/// Install the shared glass/effect-view hierarchy and return the view that owns panel content.
/// `compensation_alpha` is applied only on macOS 26+, where passive Glass panels are darkened.
pub(crate) unsafe fn install_backdrop(
    window: *mut AnyObject,
    frame: NSRect,
    corner_radius: f64,
    compensation_alpha: Option<u32>,
) -> InstalledBackdrop {
    let size = frame.size;
    if AnyClass::get(c"NSGlassEffectView").is_some() {
        let glass_class = AnyClass::get(c"NSGlassEffectView").unwrap();
        let glass: *mut AnyObject = msg_send![glass_class, alloc];
        let glass: *mut AnyObject = msg_send![glass, initWithFrame: frame];
        let _: () = msg_send![glass, setCornerRadius: corner_radius];
        let style: i64 = match crate::config::effective_glass_style().as_str() {
            "clear" => 1,
            _ => 0,
        };
        let tint_hex = crate::config::parse_hex8(&crate::config::effective_glass_tint());
        let tint = hex_to_ns_color(tint_hex);
        let _: () = msg_send![glass, setStyle: style];
        let _: () = msg_send![glass, setTintColor: tint];
        let _: () = msg_send![glass, setAutoresizingMask: 18u64];
        let _: () = msg_send![window, setContentView: glass];

        let content: *mut AnyObject = msg_send![class!(NSView), alloc];
        let content: *mut AnyObject = msg_send![content, initWithFrame: NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(size.width, size.height)
        )];
        let _: () = msg_send![content, setAutoresizingMask: 18u64];

        let (compensation_view, compensation_layer) = if let Some(alpha) = compensation_alpha {
            let fill: *mut AnyObject = msg_send![class!(NSView), alloc];
            let fill: *mut AnyObject = msg_send![fill, initWithFrame: NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(size.width, size.height)
            )];
            let _: () = msg_send![fill, setWantsLayer: true];
            let fill_layer: *mut AnyObject = msg_send![fill, layer];
            set_compensation_tint(fill_layer, tint_hex, alpha);
            let _: () = msg_send![fill, setAutoresizingMask: 18u64];
            let _: () = msg_send![content, addSubview: fill];
            release_obj(fill);
            (Some(ObjPtr::new(fill)), Some(ObjPtr::new(fill_layer)))
        } else {
            (None, None)
        };

        let _: () = msg_send![glass, setContentView: content];
        let _: () = msg_send![glass, setWantsLayer: true];
        let glass_layer: *mut AnyObject = msg_send![glass, layer];
        if !glass_layer.is_null() {
            let _: () = msg_send![glass_layer, setCornerRadius: corner_radius];
            let _: () = msg_send![glass_layer, setMasksToBounds: true];
        }
        release_obj(content);
        release_obj(glass);
        InstalledBackdrop {
            content_parent: content,
            glass: Some(ObjPtr::new(glass)),
            effect_view: None,
            compensation_view,
            compensation_layer,
        }
    } else {
        let effect: *mut AnyObject = msg_send![class!(NSVisualEffectView), alloc];
        let effect: *mut AnyObject = msg_send![effect, initWithFrame: frame];
        let _: () = msg_send![effect, setBlendingMode: 1u64]; // WithinWindow
        let _: () = msg_send![effect, setMaterial: 12u64]; // Dark
        let _: () = msg_send![effect, setState: 1u64]; // Active
        let _: () = msg_send![effect, setAutoresizingMask: 18u64];
        let _: () = msg_send![effect, setWantsLayer: true];
        let effect_layer: *mut AnyObject = msg_send![effect, layer];
        if !effect_layer.is_null() {
            let _: () = msg_send![effect_layer, setCornerRadius: corner_radius];
            let _: () = msg_send![effect_layer, setMasksToBounds: true];
        }
        let root_content: *mut AnyObject = msg_send![window, contentView];
        let _: () = msg_send![root_content, addSubview: effect];
        release_obj(effect);
        InstalledBackdrop {
            content_parent: effect,
            glass: None,
            effect_view: Some(ObjPtr::new(effect)),
            compensation_view: None,
            compensation_layer: None,
        }
    }
}

/// Apply live style/tint changes without rebuilding any panel content.
pub(crate) unsafe fn apply_live_properties(
    glass: Option<ObjPtr>,
    compensation_layer: Option<ObjPtr>,
    compensation_alpha: Option<u32>,
) {
    let style: i64 = match crate::config::effective_glass_style().as_str() {
        "clear" => 1,
        _ => 0,
    };
    let tint_hex = crate::config::parse_hex8(&crate::config::effective_glass_tint());
    let tint = hex_to_ns_color(tint_hex);
    if let Some(glass) = glass {
        let _: () = msg_send![glass.0, setStyle: style];
        let _: () = msg_send![glass.0, setTintColor: tint];
    }
    if let (Some(layer), Some(alpha)) = (compensation_layer, compensation_alpha) {
        set_compensation_tint(layer.0, tint_hex, alpha);
    }
}

unsafe fn set_compensation_tint(layer: *mut AnyObject, tint_hex: u32, alpha: u32) {
    let compensation_hex = (tint_hex & 0xFFFF_FF00) | (alpha & 0xFF);
    layer_set_background(layer, hex_to_cg_color(compensation_hex));
}
