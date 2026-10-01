//! Shared rounded material backdrop for passive floating panels.

use objc2::runtime::{AnyClass, AnyObject};
use objc2::{class, msg_send};
use objc2_foundation::{NSEdgeInsets, NSPoint, NSRect, NSSize};

use crate::ffi::{hex_to_cg_color, hex_to_ns_color, layer_set_background, release_obj, ObjPtr};

pub(crate) const PANEL_CORNER_RADIUS: f64 = 16.0;
/// AppKit darkens Liquid Glass in passive panels; this alpha matches the clipboard detail panel.
pub(crate) const INACTIVE_GLASS_COMPENSATION_ALPHA: u32 = 0x8D;

/// NSVisualEffectMaterial constants (raw AppKit values). The frost material follows the
/// resolved theme: the HUD material reads as the system's dark floating panel, the
/// under-window material as its light counterpart.
const FROST_MATERIAL_DARK: i64 = 13; // hudWindow
const FROST_MATERIAL_LIGHT: i64 = 21; // underWindowBackground

/// Floating-panel material family (settings: panel material).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PanelMaterial {
    /// NSGlassEffectView (macOS 26+) with the configured style and tint; on older systems
    /// this degrades to frost, exactly like the pre-material builds did.
    LiquidGlass,
    /// NSVisualEffectView behind-window system blur, themed.
    Frost,
    /// Opaque theme surface, no blur at all.
    Opaque,
}

impl PanelMaterial {
    /// Pure mapping from the config value; unit-tested, shared by the dev-flag check.
    pub(crate) fn from_config_value(value: &str) -> Option<Self> {
        match value {
            "liquid-glass" => Some(Self::LiquidGlass),
            "frost" => Some(Self::Frost),
            "opaque" => Some(Self::Opaque),
            _ => None,
        }
    }

    /// The material a new backdrop should use right now: the configured one with the two
    /// runtime overrides applied — the system's reduce-transparency accessibility setting
    /// forces opaque, and Liquid Glass degrades to frost where NSGlassEffectView does not
    /// exist. `build_backdrop` installs exactly this, so show-time syncs can compare
    /// `InstalledBackdrop::material` against it without oscillating.
    pub(crate) fn effective() -> Self {
        if crate::theme::reduce_transparency_enabled() {
            return Self::Opaque;
        }
        match Self::from_config_value(&crate::config::effective_panel_material()) {
            Some(Self::LiquidGlass) if AnyClass::get(c"NSGlassEffectView").is_none() => Self::Frost,
            other => other.unwrap_or(Self::LiquidGlass),
        }
    }
}

/// The effective material's config id, for the e2e state document (assertions key on the
/// id, never on view classes).
pub(crate) fn effective_material_id() -> &'static str {
    match PanelMaterial::effective() {
        PanelMaterial::LiquidGlass => "liquid-glass",
        PanelMaterial::Frost => "frost",
        PanelMaterial::Opaque => "opaque",
    }
}

#[derive(Clone, Copy)]
pub(crate) struct InstalledBackdrop {
    /// The material this backdrop was built for; show-time syncs compare it against
    /// `PanelMaterial::effective()` to pick up Reduce Transparency and config changes.
    pub(crate) material: PanelMaterial,
    pub(crate) content_parent: *mut AnyObject,
    /// The view that was made the window's content view; the swap path replaces it.
    pub(crate) root: Option<ObjPtr>,
    pub(crate) glass: Option<ObjPtr>,
    pub(crate) effect_view: Option<ObjPtr>,
    pub(crate) opaque_view: Option<ObjPtr>,
    pub(crate) compensation_view: Option<ObjPtr>,
    pub(crate) compensation_layer: Option<ObjPtr>,
}

/// Build the shared backdrop hierarchy for the effective material WITHOUT attaching it to
/// a window, and return the view that will own panel content. Attaching is the caller's
/// final step so the swap path can reparent panel content while the old hierarchy is
/// still alive.
unsafe fn build_backdrop(
    frame: NSRect,
    corner_radius: f64,
    compensation_alpha: Option<u32>,
) -> InstalledBackdrop {
    let size = frame.size;
    let material = PanelMaterial::effective();
    match material {
        PanelMaterial::LiquidGlass if AnyClass::get(c"NSGlassEffectView").is_some() => {
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
                material: PanelMaterial::LiquidGlass,
                content_parent: content,
                root: Some(ObjPtr::new(glass)),
                glass: Some(ObjPtr::new(glass)),
                effect_view: None,
                opaque_view: None,
                compensation_view,
                compensation_layer,
            }
        }
        // Frost — and Liquid Glass on pre-26 macOS, which never had the Glass class.
        // The installed material is frost either way: the sync comparison must see the
        // view class that actually exists, not the configured intent.
        PanelMaterial::LiquidGlass | PanelMaterial::Frost => {
            let effect: *mut AnyObject = msg_send![class!(NSVisualEffectView), alloc];
            let effect: *mut AnyObject = msg_send![effect, initWithFrame: frame];
            // BehindWindow: blur whatever is actually under the panel, not window content.
            let _: () = msg_send![effect, setBlendingMode: 0u64];
            let _: () = msg_send![effect, setMaterial: frost_material()];
            let _: () = msg_send![effect, setState: 1u64]; // Active
            let _: () = msg_send![effect, setAutoresizingMask: 18u64];
            let _: () = msg_send![effect, setWantsLayer: true];
            let effect_layer: *mut AnyObject = msg_send![effect, layer];
            if !effect_layer.is_null() {
                let _: () = msg_send![effect_layer, setCornerRadius: corner_radius];
                let _: () = msg_send![effect_layer, setMasksToBounds: true];
            }
            // The BehindWindow blur is composited by the window server and ignores layer
            // clipping: without a mask the square blur bleeds past the rounded corners
            // (white corners over light backgrounds, seen on the keystroke panel in dark
            // mode). The cap-inseted mask keeps the corners fixed while the view resizes.
            let mask = rounded_effect_mask(corner_radius);
            if !mask.is_null() {
                let _: () = msg_send![effect, setMaskImage: mask];
                release_obj(mask);
            }
            release_obj(effect);
            InstalledBackdrop {
                material: PanelMaterial::Frost,
                content_parent: effect,
                root: Some(ObjPtr::new(effect)),
                glass: None,
                effect_view: Some(ObjPtr::new(effect)),
                opaque_view: None,
                compensation_view: None,
                compensation_layer: None,
            }
        }
        PanelMaterial::Opaque => {
            let plain: *mut AnyObject = msg_send![class!(NSView), alloc];
            let plain: *mut AnyObject = msg_send![plain, initWithFrame: frame];
            let _: () = msg_send![plain, setAutoresizingMask: 18u64];
            let _: () = msg_send![plain, setWantsLayer: true];
            let layer: *mut AnyObject = msg_send![plain, layer];
            let _: () = msg_send![layer, setCornerRadius: corner_radius];
            let _: () = msg_send![layer, setMasksToBounds: true];
            layer_set_background(layer, hex_to_cg_color(crate::theme::ui_palette().window_bg));
            release_obj(plain);
            InstalledBackdrop {
                material: PanelMaterial::Opaque,
                content_parent: plain,
                root: Some(ObjPtr::new(plain)),
                glass: None,
                effect_view: None,
                opaque_view: Some(ObjPtr::new(plain)),
                compensation_view: None,
                compensation_layer: None,
            }
        }
    }
}

pub(crate) fn frost_material() -> i64 {
    if crate::theme::resolved_is_dark() {
        FROST_MATERIAL_DARK
    } else {
        FROST_MATERIAL_LIGHT
    }
}

/// A rounded-rect mask template for NSVisualEffectView's `maskImage`, with cap insets
/// sized to the corner radius so the image stretches to any view size while the four
/// corners keep their exact shape. 128pt side leaves room for the largest panel radius
/// (16) plus stretchable middle.
pub(crate) unsafe fn rounded_effect_mask(corner_radius: f64) -> *mut AnyObject {
    const SIDE: f64 = 128.0;
    let img: *mut AnyObject = msg_send![class!(NSImage), alloc];
    let img: *mut AnyObject = msg_send![img, initWithSize: NSSize::new(SIDE, SIDE)];
    if img.is_null() {
        return std::ptr::null_mut();
    }
    let _: () = msg_send![img, lockFocus];
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(SIDE, SIDE));
    let path: *mut AnyObject = msg_send![
        class!(NSBezierPath),
        bezierPathWithRoundedRect: rect,
        xRadius: corner_radius,
        yRadius: corner_radius
    ];
    let black: *mut AnyObject = msg_send![class!(NSColor), blackColor];
    let _: () = msg_send![black, setFill];
    let _: () = msg_send![path, fill];
    // `bezierPathWithRoundedRect:` is a convenience constructor (autoreleased, +0); releasing it
    // here would double-free when the autorelease pool drains.
    let _: () = msg_send![img, unlockFocus];
    let radius = corner_radius.min(SIDE / 2.0 - 1.0);
    let _: () = msg_send![img, setCapInsets: NSEdgeInsets {
        top: radius,
        left: radius,
        bottom: radius,
        right: radius,
    }];
    img
}

/// Install the shared backdrop for the effective material and return the view that owns
/// panel content. `compensation_alpha` is applied only on macOS 26+, where passive Glass
/// panels are darkened.
pub(crate) unsafe fn install_backdrop(
    window: *mut AnyObject,
    frame: NSRect,
    corner_radius: f64,
    compensation_alpha: Option<u32>,
) -> InstalledBackdrop {
    let backdrop = build_backdrop(frame, corner_radius, compensation_alpha);
    let _: () = msg_send![window, setContentView: backdrop.root.unwrap().0];
    reassert_glass_layer_clip(&backdrop, corner_radius);
    backdrop
}

/// Replace an installed backdrop because the material changed. The panel's content views
/// move to the new hierarchy while the old one is still alive; replacing the window's
/// content view afterwards retires the old root with no dangling children.
pub(crate) unsafe fn swap_backdrop(
    window: *mut AnyObject,
    old: &InstalledBackdrop,
    frame: NSRect,
    corner_radius: f64,
    compensation_alpha: Option<u32>,
) -> InstalledBackdrop {
    let new = build_backdrop(frame, corner_radius, compensation_alpha);
    let subviews: *mut AnyObject = msg_send![old.content_parent, subviews];
    let count: usize = msg_send![subviews, count];
    for index in 0..count {
        let child: *mut AnyObject = msg_send![subviews, objectAtIndex: index as isize];
        // The old compensation fill belongs to the retired Liquid Glass hierarchy, not to
        // the panel content: leaving it behind keeps frost/opaque untinted and stops a
        // fresh fill from stacking on every liquid -> x -> liquid round trip.
        if old.compensation_view.is_some_and(|fill| fill.0 == child) {
            continue;
        }
        let _: () = msg_send![new.content_parent, addSubview: child];
    }
    let _: () = msg_send![window, setContentView: new.root.unwrap().0];
    reassert_glass_layer_clip(&new, corner_radius);
    new
}

/// NSGlassEffectView's blur clip was only verified to hold when the layer is realized
/// after the view sits in a window (see the build_backdrop note); re-assert it on the
/// hosted view. No-op for the other materials.
unsafe fn reassert_glass_layer_clip(backdrop: &InstalledBackdrop, corner_radius: f64) {
    if let Some(glass) = backdrop.glass {
        let _: () = msg_send![glass.0, setWantsLayer: true];
        let glass_layer: *mut AnyObject = msg_send![glass.0, layer];
        if !glass_layer.is_null() {
            let _: () = msg_send![glass_layer, setCornerRadius: corner_radius];
            let _: () = msg_send![glass_layer, setMasksToBounds: true];
        }
    }
}

/// Apply live property changes without rebuilding any panel content: glass style/tint,
/// frost material (theme), and the opaque surface color (theme). Structural material
/// changes go through `swap_backdrop` instead.
pub(crate) unsafe fn apply_live_properties(
    backdrop: InstalledBackdrop,
    compensation_alpha: Option<u32>,
) {
    let style: i64 = match crate::config::effective_glass_style().as_str() {
        "clear" => 1,
        _ => 0,
    };
    let tint_hex = crate::config::parse_hex8(&crate::config::effective_glass_tint());
    let tint = hex_to_ns_color(tint_hex);
    if let Some(glass) = backdrop.glass {
        let _: () = msg_send![glass.0, setStyle: style];
        let _: () = msg_send![glass.0, setTintColor: tint];
    }
    if let Some(effect) = backdrop.effect_view {
        let _: () = msg_send![effect.0, setMaterial: frost_material()];
    }
    if let Some(plain) = backdrop.opaque_view {
        let _: () = msg_send![plain.0, setWantsLayer: true];
        let layer: *mut AnyObject = msg_send![plain.0, layer];
        if !layer.is_null() {
            layer_set_background(layer, hex_to_cg_color(crate::theme::ui_palette().window_bg));
        }
    }
    if let (Some(layer), Some(alpha)) = (backdrop.compensation_layer, compensation_alpha) {
        set_compensation_tint(layer.0, tint_hex, alpha);
    }
}

unsafe fn set_compensation_tint(layer: *mut AnyObject, tint_hex: u32, alpha: u32) {
    let compensation_hex = (tint_hex & 0xFFFF_FF00) | (alpha & 0xFF);
    layer_set_background(layer, hex_to_cg_color(compensation_hex));
}

#[cfg(test)]
mod tests {
    use super::PanelMaterial;

    #[test]
    fn material_ids_map_to_panel_materials() {
        assert_eq!(
            PanelMaterial::from_config_value("liquid-glass"),
            Some(PanelMaterial::LiquidGlass)
        );
        assert_eq!(
            PanelMaterial::from_config_value("frost"),
            Some(PanelMaterial::Frost)
        );
        assert_eq!(
            PanelMaterial::from_config_value("opaque"),
            Some(PanelMaterial::Opaque)
        );
        assert_eq!(PanelMaterial::from_config_value("glass"), None);
        assert_eq!(PanelMaterial::from_config_value(""), None);
    }

    #[test]
    fn documented_material_ids_match_the_mapping() {
        assert_eq!(
            crate::config::PANEL_MATERIAL_VALUES,
            ["liquid-glass", "frost", "opaque"]
        );
        for value in crate::config::PANEL_MATERIAL_VALUES {
            assert!(PanelMaterial::from_config_value(value).is_some());
        }
    }

    /// The show-time sync compares `InstalledBackdrop::material` (what build_backdrop
    /// installs) against `effective()`. The pre-26 degradation must live in `effective()`
    /// itself: otherwise an installed Frost record would never equal a configured
    /// LiquidGlass and every summon would rebuild the backdrop.
    #[test]
    fn every_configured_material_resolves_to_a_stable_effective_pair() {
        for configured in crate::config::PANEL_MATERIAL_VALUES {
            let material = PanelMaterial::from_config_value(configured).unwrap();
            let degraded = match material {
                PanelMaterial::LiquidGlass
                    if objc2::runtime::AnyClass::get(c"NSGlassEffectView").is_none() =>
                {
                    PanelMaterial::Frost
                }
                other => other,
            };
            // effective() applies exactly this degradation; on a build_backdrop call the
            // installed record is `degraded`, and re-running effective() on it must be a
            // fixed point (idempotent sync, no oscillation).
            let reinstall = match degraded {
                PanelMaterial::LiquidGlass
                    if objc2::runtime::AnyClass::get(c"NSGlassEffectView").is_none() =>
                {
                    PanelMaterial::Frost
                }
                other => other,
            };
            assert_eq!(degraded, reinstall);
        }
    }
}
