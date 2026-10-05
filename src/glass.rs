//! Shared rounded material backdrop for passive floating panels.

use objc2::runtime::{AnyClass, AnyObject};
use objc2::{class, msg_send};
use objc2_foundation::{NSEdgeInsets, NSPoint, NSRect, NSSize};

use crate::ffi::{hex_to_cg_color, hex_to_ns_color, layer_set_background, release_obj, ObjPtr};

pub(crate) const PANEL_CORNER_RADIUS: f64 = 16.0;
/// AppKit darkens Liquid Glass in passive panels; this alpha matches the clipboard detail panel.
pub(crate) const INACTIVE_GLASS_COMPENSATION_ALPHA: u32 = 0x8D;
/// How a panel holds the contrast table for the text it draws.
///
/// Panels sit on the user's material, so their ink is the **system's vibrant label color** rather than
/// an absolute palette token: AppKit resolves it against the live backdrop and applies its own
/// contrast-preserving treatment, which is what makes text legible on glass at all. This is the third
/// way out of the contrast-vs-material trade-off (see `docs/design-style-en.md` §3): the surface stays
/// honest and the *ink* adapts.
///
/// Because the color is system-owned, the panel tier's floors are recorded as the reduced tier the
/// design allows there (`text_primary` ≥7:1, secondary/muted ≥4.5:1) and are verified on rendered
/// pixels, not by token arithmetic.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PanelInk {
    /// `NSColor.labelColor`.
    Primary,
    /// `NSColor.secondaryLabelColor`.
    Secondary,
    /// `NSColor.tertiaryLabelColor`.
    Muted,
}

impl PanelInk {
    /// The dynamic system color for this role. Must be resolved in a vibrant appearance (see
    /// [`vibrant_appearance_name`]) for the panel to get the vibrancy treatment.
    pub(crate) unsafe fn color(self) -> *mut AnyObject {
        match self {
            Self::Primary => msg_send![class!(NSColor), labelColor],
            Self::Secondary => msg_send![class!(NSColor), secondaryLabelColor],
            Self::Muted => msg_send![class!(NSColor), tertiaryLabelColor],
        }
    }
}

/// The appearance a floating panel runs in, so dynamic colors resolve to their vibrant variants and
/// AppKit applies vibrancy to the panel's text.
pub(crate) const fn vibrant_appearance_name(dark: bool) -> &'static str {
    if dark {
        "NSAppearanceNameVibrantDark"
    } else {
        "NSAppearanceNameVibrantLight"
    }
}

/// An `NSTextField` that opts into AppKit's vibrancy treatment.
///
/// `NSView.allowsVibrancy` is queried only while a vibrant appearance is in force, so the same class is
/// inert in the settings window and the onboarding guide (which keep the absolute palette tokens):
/// panel labels get vibrancy, everything else behaves exactly as before.
pub(crate) fn vibrant_label_class() -> *mut AnyObject {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CLASS.get_or_init(|| unsafe {
        let name = std::ffi::CString::new("OhMyTabVibrantLabel").unwrap();
        let superclass = class!(NSTextField) as *const _ as *mut AnyObject;
        let cls = crate::ffi::objc_allocateClassPair(superclass, name.as_ptr(), 0);
        let types = std::ffi::CString::new("B@:").unwrap();
        crate::ffi::class_addMethod(
            cls,
            objc2::sel!(allowsVibrancy),
            vibrant_label_allows_vibrancy as *mut std::ffi::c_void,
            types.as_ptr(),
        );
        crate::ffi::objc_registerClassPair(cls);
        cls as usize
    }) as *mut AnyObject
}

extern "C" fn vibrant_label_allows_vibrancy(
    _self: *mut std::ffi::c_void,
    _cmd: objc2::runtime::Sel,
) -> bool {
    true
}

/// Opacity of the theme-surface scrim.
///
/// The panel material cannot carry the contrast table: an `NSVisualEffectView` behind-window blur is
/// translucent, and measurable surfaces ran from `#868585` (captured) to `#A6A5A5` (reported over a
/// light desktop) while the dark palette assumes `#1C1C1E`. Measured dark-mode contrast collapsed
/// from 15.63/10.10/6.40:1 to 3.38/2.18/1.38:1, and a mid-gray surface caps *any* single text color
/// at 5.71:1 -- below the table's own 12:1 and 7:1 floors, so re-coloring the text cannot fix it.
///
/// A translucent surface cannot hold the palette's contrast floors: the material's tone sits away from
/// `window_bg` by construction (a mid-gray surface caps *any* single ink colour at 5.71:1, below this
/// app's 12:1 and 7:1 floors). Those panels therefore draw with the dynamic system label colours and are
/// held to the panel tier stated in the style document -- see `panel_ink` for the dispatch and the note on
/// the rendered-pixel measurement that tier still needs.
/// `NSVisualEffectMaterial` constants (raw AppKit values). The frost material follows the resolved theme.
///
/// Dark mode uses the legacy **ultraDark** material (raw 9), not `hudWindow` (13). It has to be the darker
/// one: the material's tone is what the palette's ink sits on now that the panel paints no text surface
/// (`panel_ink`), and `hudWindow` is lifted by whatever is behind it -- measured with
/// `scripts/e2e/panel-contrast.sh` on a *white* controlled backdrop, `hudWindow` leaves the dark surface at
/// 0.648 where the light ink reaches only 2.45:1, below the panel tier. alt-tab-macos uses the same legacy
/// materials for the same reason (semantic materials follow the view's appearance, which a user-preference
/// theme cannot rely on).
const FROST_MATERIAL_DARK: i64 = 9; // ultraDark (legacy)
const FROST_MATERIAL_LIGHT: i64 = 21; // underWindowBackground

/// One panel's backdrop options.
///
/// There is deliberately no "wash the panel until the text passes" option: pinning a *panel's* surface to
/// `window_bg` left only ~9% of the backdrop visible and made `frost` and `opaque` measure 3/255 apart on a
/// live panel -- three settings that rendered as one. The panels' text now rides their material and the ink
/// carries the contrast (see `panel_ink`), so the material stays honest by construction.
#[derive(Clone, Copy)]
pub(crate) struct BackdropOptions {
    /// Liquid Glass darkens in passive panels; this alpha matches the clipboard detail panel.
    pub(crate) compensation_alpha: Option<u32>,
}

impl BackdropOptions {
    /// Backdrop options for a panel that carries its own text surfaces.
    pub(crate) const fn new(compensation_alpha: Option<u32>) -> Self {
        Self { compensation_alpha }
    }
}

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
    /// Blur without a material: a `CABackdropLayer` carrying `gaussianBlur`. Measured against LiquidGlass on
    /// a controlled backdrop -- the same blur (both destroy the 0.5pt checkerboard, retention 0.000) at
    /// +8.6 of lift instead of +46.5, with the radius knob moving retention monotonically (0 -> 0.985,
    /// 5 -> 0.020, 10/20 -> 0.000) while the lift stays put. Transparency is not spent on blur here, which
    /// is the point. Development channel: `--panel-material=backdrop` until the look is judged.
    Backdrop,
}

impl PanelMaterial {
    /// Pure mapping from the config value; unit-tested, shared by the dev-flag check.
    pub(crate) fn from_config_value(value: &str) -> Option<Self> {
        match value {
            "liquid-glass" => Some(Self::LiquidGlass),
            "frost" => Some(Self::Frost),
            "opaque" => Some(Self::Opaque),
            // Development-only: no settings UI offers it, and the settings writer never emits it.
            "backdrop" => Some(Self::Backdrop),
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
        PanelMaterial::Backdrop => "backdrop",
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
    /// The blur-only surface's layer host view (`PanelMaterial::Backdrop`).
    pub(crate) backdrop_view: Option<ObjPtr>,
    /// Liquid Glass's inactive-panel darkening fill. Never a contrast device.
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
    options: BackdropOptions,
) -> InstalledBackdrop {
    let size = frame.size;
    let material = PanelMaterial::effective();
    let compensation_alpha = options.compensation_alpha;
    match material {
        PanelMaterial::LiquidGlass if AnyClass::get(c"NSGlassEffectView").is_some() => {
            let glass_class = AnyClass::get(c"NSGlassEffectView").unwrap();
            let glass: *mut AnyObject = msg_send![glass_class, alloc];
            let glass: *mut AnyObject = msg_send![glass, initWithFrame: frame];
            let _: () = msg_send![glass, setCornerRadius: corner_radius];
            // The look chooses only `style`; the tint is the config's value, verbatim -- its alpha is the
            // whole strength.
            let tuning = MaterialTuning::effective();
            let tint_hex = resolved_glass_tint_hex();
            let tint = hex_to_ns_color(tint_hex);
            let _: () = msg_send![
                glass,
                setStyle: glass_style_index(&crate::config::effective_glass_style())
            ];
            let _: () = msg_send![glass, setTintColor: tint];
            let _: () = msg_send![glass, setAlphaValue: tuning.opacity];
            // Installation follows alt-tab-macos: the glass view becomes the window's `contentView`
            // directly and the panel's content lives in `glass.contentView` -- nothing wraps it. That is
            // the shape their working glass uses, and wrapping it in a plain container (which this code
            // did) is the one structural difference that was never tested against the missing blur.
            //
            // The container survives only for the development-only blur underlay, which needs a layer of
            // its own to sit under the glass; the shipped path never creates it.
            let underlay_active = tuning.blur_radius > 0.0;
            // `_variant` is private, and forcing 0 is not the same as leaving it alone: measured, a glass
            // view with `_variant` set to 0 renders as a transparent, unblurred overlay, while the system's
            // own Command+Tab look (and alt-tab-macos's `.regular` look, which never sets `_variant`) blurs.
            // So the property is only touched when a variant is asked for explicitly, which today means the
            // development-only `--glass-variant=`; the shipped path leaves the system's own choice alone.
            if crate::dev_flags::value("glass-variant").is_some() {
                let variant = if underlay_active { 19 } else { tuning.variant };
                apply_glass_variant(glass, variant);
            }
            // Installation follows alt-tab-macos: the glass view is what the window hosts, with the
            // panel's content in `glass.contentView` and nothing wrapping it. A `NSVisualEffectView` was
            // briefly stacked under it to supply a sampled backdrop; that is exactly what their project
            // warns against ("do NOT set vibrancy alongside liquid glass, it will override and look
            // blurry") and on screen it turned all three looks into an opaque white surface, so it is gone.
            //
            // The container survives only for the development-only blur underlay, which needs a layer of
            // its own to sit under the glass; the shipped path never creates it.
            // `--glass-underlay`: stack the frost material under the glass for comparison. **Shipped path:
            // off.** Measured on a stripe ladder the stack does blur more (contrast retention over 2-16pt
            // detail 0.15-0.36 against the glass's own 0.24-0.57, a 48px stripe still arriving at 0.06), but
            // it buys that with the frost material's milk: on screen every look -- `clear` included -- stops
            // reading as transparent at all. Transparency is not something to spend on blur, and the
            // community warning ("do NOT set vibrancy alongside liquid glass") is right about this
            // direction. More blur *at the same transparency* needs a blur without a lightening layer -- a
            // `CABackdropLayer` carrying `gaussianBlur` and nothing else -- which is a separate experiment.
            let frost_underlay = crate::dev_flags::value("glass-underlay").is_some();
            let root = if frost_underlay {
                let under: *mut AnyObject = msg_send![class!(NSVisualEffectView), alloc];
                let under: *mut AnyObject = msg_send![under, initWithFrame: frame];
                let _: () = msg_send![under, setBlendingMode: 0u64]; // BehindWindow
                let _: () = msg_send![under, setMaterial: frost_material()];
                let _: () = msg_send![under, setState: 1u64]; // Active
                let _: () = msg_send![under, setAutoresizingMask: 18u64];
                let _: () = msg_send![under, setWantsLayer: true];
                let under_layer: *mut AnyObject = msg_send![under, layer];
                if !under_layer.is_null() {
                    // Radius only, never `masksToBounds`: a layer clip suppresses the blur; the rounded
                    // shape comes from the cap-inseted mask image, as the frost material does it.
                    let _: () = msg_send![under_layer, setCornerRadius: corner_radius];
                }
                let mask = rounded_effect_mask(corner_radius);
                if !mask.is_null() {
                    let _: () = msg_send![under, setMaskImage: mask];
                    release_obj(mask);
                }
                let _: () = msg_send![under, addSubview: glass];
                under
            } else if underlay_active {
                let container: *mut AnyObject = msg_send![class!(NSView), alloc];
                let container: *mut AnyObject = msg_send![container, initWithFrame: frame];
                let _: () = msg_send![container, setAutoresizingMask: 18u64];
                let _: () = msg_send![container, setWantsLayer: true];
                apply_blur_underlay(container, tuning.blur_radius, tuning.saturation);
                let _: () = msg_send![container, addSubview: glass];
                container
            } else {
                glass
            };
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
            // Layer-backed and self-clipped. Dropping this (an experiment to see whether the mask was what
            // suppressed the backdrop blur) made the panel's top edge get cut off inside the window, so it
            // is restored; the blur has to be chased without touching the glass's layer.
            let _: () = msg_send![glass, setWantsLayer: true];
            let glass_layer: *mut AnyObject = msg_send![glass, layer];
            if !glass_layer.is_null() {
                let _: () = msg_send![glass_layer, setCornerRadius: corner_radius];
                let _: () = msg_send![glass_layer, setMasksToBounds: true];
            }
            release_obj(glass);
            InstalledBackdrop {
                material: PanelMaterial::LiquidGlass,
                content_parent: content,
                // The behind-window blur that gives the glass something sampled to sit on; the glass is
                // its subview and the window hosts this view.
                root: Some(ObjPtr::new(root)),
                glass: Some(ObjPtr::new(glass)),
                effect_view: None,
                opaque_view: None,
                backdrop_view: None,
                compensation_view,
                compensation_layer,
            }
        }
        PanelMaterial::Backdrop => {
            let root: *mut AnyObject = msg_send![class!(NSView), alloc];
            let root: *mut AnyObject = msg_send![root, initWithFrame: frame];
            let _: () = msg_send![root, setAutoresizingMask: 18u64];
            let _: () = msg_send![root, setWantsLayer: true];
            let root_layer: *mut AnyObject = msg_send![root, layer];
            if !root_layer.is_null() {
                let _: () = msg_send![root_layer, setCornerRadius: corner_radius];
                let _: () = msg_send![root_layer, setMasksToBounds: true];
            }
            let tuning = MaterialTuning::effective();
            let radius = if tuning.blur_radius > 0.0 {
                tuning.blur_radius
            } else {
                BACKDROP_BLUR_RADIUS
            };
            // The layer lives on a child view: on the content view's own layer it renders nothing in this
            // window (measured: retention 1.000, the backdrop came through unblurred), one level deeper it
            // blurs.
            let host: *mut AnyObject = msg_send![class!(NSView), alloc];
            let host: *mut AnyObject = msg_send![host, initWithFrame: frame];
            let _: () = msg_send![host, setAutoresizingMask: 18u64];
            let _: () = msg_send![host, setWantsLayer: true];
            apply_blur_underlay(host, radius, tuning.saturation);
            let _: () = msg_send![root, addSubview: host];
            let content: *mut AnyObject = msg_send![class!(NSView), alloc];
            let content: *mut AnyObject = msg_send![content, initWithFrame: NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(size.width, size.height)
            )];
            let _: () = msg_send![content, setAutoresizingMask: 18u64];
            let _: () = msg_send![root, addSubview: content];
            release_obj(host);
            release_obj(root);
            InstalledBackdrop {
                material: PanelMaterial::Backdrop,
                content_parent: content,
                root: Some(ObjPtr::new(root)),
                glass: None,
                effect_view: None,
                opaque_view: None,
                backdrop_view: Some(ObjPtr::new(host)),
                compensation_view: None,
                compensation_layer: None,
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
                backdrop_view: None,
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
                backdrop_view: None,
                compensation_view: None,
                compensation_layer: None,
            }
        }
    }
}

/// The colour panel text is drawn in.
///
/// `opaque` draws with the palette token, which is exactly right because its surface *is* `window_bg`. Every
/// other material draws with the dynamic system label colours, because its surface follows the backdrop and
/// only an ink that resolves against it can be legible there.
///
/// **The two translucent materials are not equally justified in doing so, and the difference matters:**
/// AppKit's contrast-preserving vibrancy treatment is implemented by `NSVisualEffectView`, which frost's
/// hierarchy has and liquid glass's (`NSGlassEffectView`) does not. So for frost the treatment is a known
/// mechanism, while for glass the dynamic colours merely resolve per appearance with **no compensation this
/// project has verified** -- a mid-grey glass surface in dark mode is the case to check, and the panel tier
/// in the style document is a target until the rendered-pixel measurement exists (the smoke can only `log`
/// these, because the composite is invisible to a colour-vs-constant comparison).
pub(crate) unsafe fn panel_ink(palette_token: u32, role: PanelInk) -> *mut AnyObject {
    // Frost draws with the dynamic system label colours: AppKit resolves them against the live backdrop and
    // applies its contrast-preserving treatment, which `NSVisualEffectView` provides (measured: 5.16:1 light,
    // 5.90:1 dark on the panel's own material -- `scripts/e2e/panel-contrast.sh`).
    //
    // Liquid glass draws with the palette token instead. It has no such provider -- `NSGlassEffectView` is
    // not an `NSVisualEffectView` -- and the dynamic colours measured *worse* than the palette's on its
    // surface (1.20:1 against the palette's own ink in dark mode), so the deterministic colour is both
    // better and predictable.
    match PanelMaterial::effective() {
        PanelMaterial::Frost => role.color(),
        _ => hex_to_ns_color(palette_token),
    }
}

/// The blur-only surface's default radius. Five already blurs a half-point checkerboard to nothing on the
/// controlled backdrop, and the lift does not grow with the radius, so this is a *look* choice rather than
/// a masking one; `--glass-blur` overrides it.
const BACKDROP_BLUR_RADIUS: f64 = 12.0;

/// The liquid-glass tint, **per mode**, and both values are measured rather than chosen.
///
/// With no text surface on the panel the material *is* the surface (see `panel_ink`), so the glass's own tone
/// decides the ink's contrast. Measured with `scripts/e2e/panel-contrast.sh` (the picker's filter row on the
/// bare material): light `eeeeee66` -> surface 0.805 -> 5.30:1; dark `1c1c1e99` -> surface 0.164 -> 8.56:1.
///
/// One tint cannot serve both modes: the light tint in dark mode left the surface mid-grey (0.539), where the
/// palette's caption ink cannot exceed 2.07:1 -- no ink colour reaches the panel tier on a mid-grey surface.
/// Two earlier failures are recorded because both shipped during this work: re-lighting a *colour* per mode
/// (dark `window_bg` is near-black, so a colourful page's hue was all that survived and the panel read as
/// purple), and multiplying the strength by 255 twice (the alpha collapsed to 20%).
const GLASS_TINT_LIGHT: &str = "eeeeee66";
const GLASS_TINT_DARK: &str = "1c1c1e99";

/// The tint for the *resolved* mode.
pub(crate) fn resolved_glass_tint_hex() -> u32 {
    let hex = if crate::theme::resolved_is_dark() {
        GLASS_TINT_DARK
    } else {
        GLASS_TINT_LIGHT
    };
    crate::config::parse_hex8(hex)
}

/// The `NSGlassEffectViewStyle` for a configured look: `clear` is 1, anything else `regular` (0).
///
/// The catch-all is the historical mapping, so a value written by any other channel (this app briefly
/// shipped a third look named `system`) renders as the regular glass rather than as nothing.
pub(crate) fn glass_style_index(style: &str) -> i64 {
    if style == "clear" {
        1
    } else {
        0
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
    options: BackdropOptions,
) -> InstalledBackdrop {
    let backdrop = build_backdrop(frame, corner_radius, options);
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
    options: BackdropOptions,
) -> InstalledBackdrop {
    let new = build_backdrop(frame, corner_radius, options);
    let subviews: *mut AnyObject = msg_send![old.content_parent, subviews];
    let count: usize = msg_send![subviews, count];
    for index in 0..count {
        let child: *mut AnyObject = msg_send![subviews, objectAtIndex: index as isize];
        // The retired hierarchy's darkening fill is not panel content: leaving it behind keeps
        // frost/opaque untinted and stops a fresh fill from stacking on every material round trip.
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
    // The glass clips itself (its own layer); the container above it deliberately does not, because the
    // variants draw their edge outside the glass bounds and a clipping container cut the panel's top off.
    if let Some(root) = backdrop.root {
        let _: () = msg_send![root.0, setWantsLayer: true];
    }
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
pub(crate) unsafe fn apply_live_properties(backdrop: InstalledBackdrop, options: BackdropOptions) {
    let style: i64 = match crate::config::effective_glass_style().as_str() {
        // Only `clear` takes the system's clear material (this app's historical clear look, and the
        // thinnest of the three). `system` takes the system's *regular* material and adds no tint of
        // ours, i.e. the glass exactly as the system draws it.
        //
        // It used to be `clear` + no tint, which made "System default" the *most* transparent option.
        // That cannot be right for a setting whose reference is macOS's own Command+Tab panel: a
        // screenshot comparison puts that panel at least as substantial as our regular look, and a
        // pixel metric could not settle the difference (the same glass strip measured a text-contrast
        // attenuation anywhere between alpha 0.28 and 0.65 depending on where it was sampled, because
        // the two screenshots are separate captures at different scales). The ladder is therefore set by
        // principle: system (regular, system tint only) < regular (regular + our floored tint) <
        // clear (clear + our floored tint), and any further tuning needs a controlled A/B -- both looks
        // rendered in one screenshot over the same backdrop -- rather than screenshot forensics.
        "clear" => 1,
        _ => 0,
    };
    let tuning = MaterialTuning::effective();
    let tint_hex = resolved_glass_tint_hex();
    let tint = hex_to_ns_color(tint_hex);
    if let Some(glass) = backdrop.glass {
        let _: () = msg_send![glass.0, setStyle: style];
        let _: () = msg_send![glass.0, setTintColor: tint];
        let _: () = msg_send![glass.0, setAlphaValue: tuning.opacity];
        let variant = if tuning.blur_radius > 0.0 {
            19
        } else {
            tuning.variant
        };
        apply_glass_variant(glass.0, variant);
        if let Some(root) = backdrop.root {
            apply_blur_underlay(root.0, tuning.blur_radius, tuning.saturation);
        }
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
    if let Some(layer) = backdrop.compensation_layer {
        if let Some(alpha) = options.compensation_alpha {
            set_compensation_tint(layer.0, tint_hex, alpha);
        }
    }
}

/// The material's own strength knobs, i.e. everything AppKit does not expose as a property.
///
/// Two shipped macOS switchers solve the "the material looks wrong and I cannot tune it" problem the
/// same way, and this module follows them:
///
/// - **`glass_variant`**: `NSGlassEffectView`'s private `_variant`. alt-tab-macos documents that
///   `style = .clear` alone renders *nearly fully transparent* and that the variant is what makes the
///   panel visible, so its app-icons look is `clear` + variant 3; DockDoor exposes variants 0–19 plus a
///   synthetic one and defaults to 4.
/// - **`glass_blur_radius` / `glass_saturation`**: neither `NSVisualEffectView` nor
///   `NSGlassEffectView` exposes a blur radius or saturation, but the system's own backdrop layers
///   (`CABackdropLayer`, the private layer that samples what is behind the window) accept
///   `gaussianRadius` and `saturationFactor` by key. DockDoor drives exactly these; the values here
///   are the same two.
///
/// Everything is presence-checked at runtime and a miss is a no-op: on a system that drops either
/// private surface the panel keeps its public appearance instead of trapping.
pub(crate) struct MaterialTuning {
    pub(crate) opacity: f64,
    pub(crate) blur_radius: f64,
    pub(crate) saturation: f64,
    pub(crate) variant: i64,
}

impl MaterialTuning {
    /// The knobs as configured (each with its `--glass-*` launch override).
    pub(crate) fn effective() -> Self {
        Self {
            opacity: crate::config::effective_glass_opacity(),
            blur_radius: crate::config::effective_glass_blur_radius(),
            saturation: crate::config::effective_glass_saturation(),
            variant: crate::config::effective_glass_variant(),
        }
    }
}

/// Whether this system's `NSGlassEffectView` still answers the private `_variant` selector.
pub(crate) fn glass_variant_is_supported() -> bool {
    objc2::runtime::AnyClass::get(c"NSGlassEffectView")
        .is_some_and(|class| class.instance_method(objc2::sel!(set_variant:)).is_some())
}

/// `[glass set_variant: n]` -- the private look selector. No-op when it is absent or `variant <= 0`.
pub(crate) unsafe fn apply_glass_variant(glass: *mut AnyObject, variant: i64) {
    if glass.is_null() || variant <= 0 {
        return;
    }
    if !glass_variant_is_supported() {
        // Exactly one line, once per process: the user asked for a look this system does not have, and
        // silently rendering the default is the failure mode worth a log.
        static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !WARNED.swap(true, std::sync::atomic::Ordering::SeqCst) {
            crate::log_info!(
                "[glass] glass_variant={variant} requested, but NSGlassEffectView no longer answers set_variant:; keeping the system look"
            );
        }
        return;
    }
    let Some(class) = objc2::runtime::AnyClass::get(c"NSGlassEffectView") else {
        return;
    };
    let sel = objc2::sel!(set_variant:);
    let Some(method) = class.instance_method(sel) else {
        return;
    };
    let imp: unsafe extern "C-unwind" fn(*mut AnyObject, objc2::runtime::Sel, i64) =
        std::mem::transmute(method.implementation());
    imp(glass, sel, variant);
}

/// KVC write used by the (development-only) blur underlay.
/// `[CAFilter filterWithType: "..."]`, or null where the private class or selector is absent.
unsafe fn make_ca_filter(filter_type: &str) -> *mut AnyObject {
    let Some(class) = objc2::runtime::AnyClass::get(c"CAFilter") else {
        return std::ptr::null_mut();
    };
    if class.class_method(objc2::sel!(filterWithType:)).is_none()
        && class
            .instance_method(objc2::sel!(filterWithType:))
            .is_none()
    {
        return std::ptr::null_mut();
    }
    let name = crate::ffi::make_nsstring(filter_type);
    let filter: *mut AnyObject = msg_send![class, filterWithType: name];
    crate::ffi::CFRelease(name as *const std::ffi::c_void);
    filter
}

/// Give the glass an owned backdrop layer carrying the blur/saturation filters.
///
/// **Development channel only** (`--glass-blur` / `--glass-saturation`), and the only place this app
/// touches the system's private compositing: the layer is ours, inserted under the glass, with
/// `CAFilter`s for `gaussianBlur` / `colorSaturate` attached. Nothing here runs on the shipped path --
/// and the app must not poke the *system's* backdrop layers, which is what a previous version did on
/// every build (a KVC write of `saturationFactor` into whatever backdrop layers the glass view owned),
/// and which is exactly the kind of interference that can leave a system material unblurred.
pub(crate) unsafe fn apply_blur_underlay(
    container: *mut AnyObject,
    blur_radius: f64,
    saturation: f64,
) {
    if container.is_null() {
        return;
    }
    let container_layer: *mut AnyObject = msg_send![container, layer];
    if container_layer.is_null() {
        return;
    }
    if blur_radius <= 0.0 {
        return;
    }
    let Some(class) = objc2::runtime::AnyClass::get(c"CABackdropLayer") else {
        return;
    };
    let layer: *mut AnyObject = msg_send![class, alloc];
    let layer: *mut AnyObject = msg_send![layer, init];
    if layer.is_null() {
        return;
    }
    set_layer_filter_value_bool(layer, "windowServerAware", true);
    set_layer_filter_value(layer, "scale", 1.0);
    let bounds: NSRect = msg_send![container_layer, bounds];
    let _: () = msg_send![layer, setFrame: bounds];
    let radius: f64 = msg_send![container_layer, cornerRadius];
    let _: () = msg_send![layer, setCornerRadius: radius];
    let _: () = msg_send![layer, setMasksToBounds: true];
    let _: () = msg_send![container_layer, insertSublayer: layer, atIndex: 0u32];

    let filters: *mut AnyObject = msg_send![class!(NSMutableArray), array];
    let blur = make_ca_filter("gaussianBlur");
    if !blur.is_null() {
        set_layer_filter_value(blur, "inputRadius", blur_radius);
        set_layer_filter_value_bool(blur, "inputNormalizeEdges", true);
        let _: () = msg_send![filters, addObject: blur];
    }
    if (saturation - 1.0).abs() > f64::EPSILON {
        let saturate = make_ca_filter("colorSaturate");
        if !saturate.is_null() {
            set_layer_filter_value(saturate, "inputAmount", saturation);
            let _: () = msg_send![filters, addObject: saturate];
        }
    }
    let _: () = msg_send![layer, setFilters: filters];
    crate::ffi::release_obj(layer);
}

unsafe fn set_layer_filter_value_bool(layer: *mut AnyObject, key: &str, value: bool) {
    let key_ns = crate::ffi::make_nsstring(key);
    let number: *mut AnyObject = msg_send![class!(NSNumber), numberWithBool: value];
    let _: () = msg_send![layer, setValue: number, forKey: key_ns];
    crate::ffi::CFRelease(key_ns as *const std::ffi::c_void);
}

/// Push `blur_radius` and `saturation` into whatever backdrop layers the material owns.
///
unsafe fn set_layer_filter_value(layer: *mut AnyObject, key: &str, value: f64) {
    let key_ns = crate::ffi::make_nsstring(key);
    let number: *mut AnyObject = msg_send![class!(NSNumber), numberWithDouble: value];
    let _: () = msg_send![layer, setValue: number, forKey: key_ns];
    crate::ffi::CFRelease(key_ns as *const std::ffi::c_void);
}

unsafe fn set_compensation_tint(layer: *mut AnyObject, tint_hex: u32, alpha: u32) {
    let compensation_hex = (tint_hex & 0xFFFF_FF00) | (alpha & 0xFF);
    layer_set_background(layer, hex_to_cg_color(compensation_hex));
}

#[cfg(test)]
mod tests {
    use super::{glass_style_index, resolved_glass_tint_hex, PanelMaterial};

    /// The tint must not decide lightness: a tint that is far too light for dark mode (or too
    /// dark for light mode) has to be re-lit into the range the palette's contrast table assumes,
    /// while keeping the hue the user picked. Measured on the real panel, a tint resolved this
    /// way lands the dark surface at (30,30,31) and the light one at (249,249,249) -- both clear
    /// every floor in design-style §3.3, which the historical `eeeeee66` default did not in

    /// The tint is per mode: light keeps the shipped value, dark takes the measured dark one. Both
    /// numbers are pinned so a later "adjustment" has to face the measurement that produced them.
    #[test]
    fn the_glass_tint_is_derived_per_mode() {
        assert_eq!(super::GLASS_TINT_LIGHT, "eeeeee66");
        assert_eq!(super::GLASS_TINT_DARK, "1c1c1e99");
        assert_eq!(
            crate::config::parse_hex8(super::GLASS_TINT_LIGHT),
            0xEEEE_EE66
        );
        assert_eq!(
            crate::config::parse_hex8(super::GLASS_TINT_DARK),
            0x1C1C_1E99
        );
        // The accessor follows the resolved mode, which is what the panel draws with.
        let expected = if crate::theme::resolved_is_dark() {
            0x1C1C_1E99
        } else {
            0xEEEE_EE66
        };
        assert_eq!(resolved_glass_tint_hex(), expected);
    }

    /// Only `clear` selects the clear style. Everything else -- including the `system` value an earlier
    /// build wrote -- is the regular glass, which is the mapping both shipping looks rely on.
    #[test]
    fn only_clear_selects_the_clear_style() {
        assert_eq!(glass_style_index("clear"), 1);
        assert_eq!(glass_style_index("regular"), 0);
        assert_eq!(
            glass_style_index("system"),
            0,
            "a retired look must not blank the glass"
        );
        assert_eq!(glass_style_index(""), 0);
    }

    /// Backdrop options carry only the inactive-glass compensation now: there is no per-panel
    /// "wash the material" policy left to select (see [`BackdropOptions`]).
    #[test]
    fn backdrop_options_carry_the_compensation_alpha() {
        assert_eq!(super::BackdropOptions::new(None).compensation_alpha, None);
        assert_eq!(
            super::BackdropOptions::new(Some(crate::glass::INACTIVE_GLASS_COMPENSATION_ALPHA))
                .compensation_alpha,
            Some(crate::glass::INACTIVE_GLASS_COMPENSATION_ALPHA)
        );
    }

    #[test]
    fn material_ids_map_to_panel_materials() {
        assert_eq!(
            PanelMaterial::from_config_value("liquid-glass"),
            Some(PanelMaterial::LiquidGlass)
        );
        assert_eq!(
            PanelMaterial::from_config_value("backdrop"),
            Some(PanelMaterial::Backdrop),
            "the development-only blur surface must be reachable by its id"
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
