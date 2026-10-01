//! Live glass-tint preview: the color well, preview panel, and preview-view syncing.

use super::*;

const GLASS_TINT_WELL_W: f64 = 52.0;
const GLASS_TINT_WELL_GAP: f64 = 8.0;

pub(super) struct GlassTintControl {
    pub(super) container: *mut AnyObject,
    pub(super) well: *mut AnyObject,
    pub(super) hex_caption: *mut AnyObject,
}

fn glass_tint_control_frames(width: f64, height: f64) -> (NSRect, NSRect) {
    let well_x = (width - GLASS_TINT_WELL_W).max(0.0);
    let caption_w = (well_x - GLASS_TINT_WELL_GAP).max(0.0);
    (
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(caption_w, height)),
        NSRect::new(
            NSPoint::new(well_x, 0.0),
            NSSize::new(GLASS_TINT_WELL_W.min(width), height),
        ),
    )
}

fn display_glass_tint_hex(value: &str) -> String {
    format!("#{}", value.trim_start_matches('#').to_ascii_uppercase())
}

pub(super) fn color_component_to_byte(component: f64) -> u8 {
    (component.clamp(0.0, 1.0) * 255.0).round() as u8
}

pub(super) fn rgba_hex_from_components(red: f64, green: f64, blue: f64, alpha: f64) -> String {
    format!(
        "{:02x}{:02x}{:02x}{:02x}",
        color_component_to_byte(red),
        color_component_to_byte(green),
        color_component_to_byte(blue),
        color_component_to_byte(alpha)
    )
}

/// Convert any NSColor to sRGB and encode it as the RRGGBBAA format used by the config.
pub(super) unsafe fn ns_color_to_hex(color: *mut AnyObject) -> Option<String> {
    if color.is_null() {
        return None;
    }
    let space: *mut AnyObject = msg_send![class!(NSColorSpace), sRGBColorSpace];
    let srgb: *mut AnyObject = msg_send![color, colorUsingColorSpace: space];
    if srgb.is_null() {
        return None;
    }
    let red: f64 = msg_send![srgb, redComponent];
    let green: f64 = msg_send![srgb, greenComponent];
    let blue: f64 = msg_send![srgb, blueComponent];
    let alpha: f64 = msg_send![srgb, alphaComponent];
    Some(rgba_hex_from_components(red, green, blue, alpha))
}

/// Native color well, kept as a compact right-side swatch instead of stretching like a text field.
pub(super) unsafe fn make_color_well(
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    value: &str,
    target: *mut AnyObject,
) -> GlassTintControl {
    let container: *mut AnyObject = msg_send![class!(NSView), alloc];
    let container: *mut AnyObject = msg_send![
        container,
        initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    ];
    let (caption_frame, well_frame) = glass_tint_control_frames(w, h);
    let caption_value = display_glass_tint_hex(value);
    let caption = widgets::make_value_label(
        caption_frame.origin.x,
        caption_frame.origin.y,
        caption_frame.size.width,
        caption_frame.size.height,
        &caption_value,
    );
    let font: *mut AnyObject = msg_send![
        class!(NSFont),
        systemFontOfSize: crate::theme::FONT_CAPTION,
        weight: crate::theme::FONT_WEIGHT_REGULAR
    ];
    let _: () = msg_send![caption, setFont: font];
    widgets::apply_settings_text_role(caption, widgets::SettingsTextRole::Muted);
    let _: () = msg_send![caption, setAlignment: 2isize]; // NSTextAlignmentRight
    let _: () = msg_send![container, addSubview: caption];
    crate::ffi::release_obj(caption);

    let well: *mut AnyObject = msg_send![glass_tint_well_class(), alloc];
    let well: *mut AnyObject = msg_send![
        well,
        initWithFrame: well_frame
    ];
    let _: () = msg_send![well, setColorWellStyle: 0isize]; // NSColorWellStyleDefault
    let _: () = msg_send![well, setBordered: true];
    let _: () = msg_send![well, setContinuous: true];
    let _: () = msg_send![well, setTarget: target];
    let _: () = msg_send![well, setAction: sel!(handleGlassTintChanged:)];
    let responds: bool = msg_send![well, respondsToSelector: sel!(setSupportsAlpha:)];
    if responds {
        let _: () = msg_send![well, setSupportsAlpha: true];
    }
    let color = crate::ffi::hex_to_ns_color(crate::config::parse_hex8(value));
    let _: () = msg_send![well, setColor: color];
    let _: () = msg_send![well, setAutoresizingMask: 0u64];
    let _: () = msg_send![container, addSubview: well];
    crate::ffi::release_obj(well);
    GlassTintControl {
        container,
        well,
        hex_caption: caption,
    }
}

/// Pure: center the settings window + color panel as one horizontal group, with the panel on
/// the right and vertically centered against the settings window.
pub(super) fn glass_tint_group_frames(
    settings: NSRect,
    panel: NSRect,
    screen: NSRect,
) -> (NSRect, NSRect) {
    let group_w = settings.size.width + GLASS_TINT_GROUP_GAP + panel.size.width;
    let min_x = screen.origin.x + GLASS_TINT_SCREEN_MARGIN;
    let max_x = screen.origin.x + screen.size.width - GLASS_TINT_SCREEN_MARGIN;
    let group_x = if group_w + 2.0 * GLASS_TINT_SCREEN_MARGIN <= screen.size.width {
        (screen.origin.x + (screen.size.width - group_w) / 2.0)
            .max(min_x)
            .min(max_x - group_w)
    } else {
        min_x
    };

    let min_y = screen.origin.y + GLASS_TINT_SCREEN_MARGIN;
    let max_y = screen.origin.y + screen.size.height - GLASS_TINT_SCREEN_MARGIN - panel.size.height;
    let centered_y = settings.origin.y + (settings.size.height - panel.size.height) / 2.0;
    let panel_y = if max_y >= min_y {
        centered_y.max(min_y).min(max_y)
    } else {
        min_y
    };

    (
        NSRect::new(NSPoint::new(group_x, settings.origin.y), settings.size),
        NSRect::new(
            NSPoint::new(
                group_x + settings.size.width + GLASS_TINT_GROUP_GAP,
                panel_y,
            ),
            panel.size,
        ),
    )
}

/// Get the visible frame of the settings window's screen, falling back to the main screen before
/// AppKit has assigned one.
pub(super) unsafe fn glass_tint_screen_frame(window: *mut AnyObject) -> NSRect {
    let screen: *mut AnyObject = msg_send![window, screen];
    if screen.is_null() {
        let main: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
        msg_send![main, visibleFrame]
    } else {
        msg_send![screen, visibleFrame]
    }
}

/// Move the settings window left before opening the color panel so the two windows are centered
/// as one group.
pub(super) unsafe fn position_glass_tint_group(save_original: bool) {
    let window = super::with_settings_ui(|ui| ui.as_ref().map(|ui| ui.window));
    let Some(window) = window else { return };
    let panel: *mut AnyObject = msg_send![class!(NSColorPanel), sharedColorPanel];
    if panel.is_null() {
        return;
    }

    let settings_frame: NSRect = msg_send![window, frame];
    if save_original {
        let mut original = GLASS_TINT_GROUP_ORIGINAL_ORIGIN.lock().unwrap();
        if original.is_none() {
            *original = Some(settings_frame.origin);
        }
    }
    let panel_frame: NSRect = msg_send![panel, frame];
    let screen_frame = glass_tint_screen_frame(window);
    let (settings_frame, panel_frame) =
        glass_tint_group_frames(settings_frame, panel_frame, screen_frame);
    let _: () = msg_send![window, setFrameOrigin: settings_frame.origin];
    let _: () = msg_send![panel, setFrameOrigin: panel_frame.origin];
}

/// Restore the settings window's pre-panel position after the color panel closes; repeated calls
/// are intentionally harmless.
pub(crate) fn restore_glass_tint_group() {
    let original = GLASS_TINT_GROUP_ORIGINAL_ORIGIN.lock().unwrap().take();
    let Some(origin) = original else { return };
    unsafe {
        let window = super::with_settings_ui(|ui| ui.as_ref().map(|ui| ui.window));
        if let Some(window) = window {
            let _: () = msg_send![window, setFrameOrigin: origin];
        }
    }
}

/// Custom NSColorWell positioning before AppKit displays the shared color panel, avoiding a
/// flash in the screen's lower-left corner.
pub(super) extern "C" fn glass_tint_well_activate(this: *mut c_void, _cmd: Sel, exclusive: bool) {
    unsafe {
        position_glass_tint_group(true);
        let superclass = AnyClass::get(c"NSColorWell").unwrap();
        let _: () = msg_send![
            super(this as *mut AnyObject, superclass),
            activate: exclusive
        ];
        // AppKit may restore the panel's remembered frame during activate:; apply the grouped
        // position once more after super so the final visible frame is deterministic.
        position_glass_tint_group(false);
    }
}

pub(super) fn glass_tint_well_class() -> *mut AnyObject {
    GLASS_TINT_WELL_CLASS
        .get_or_init(|| unsafe {
            let name = CString::new("OhMyTabGlassTintWell").unwrap();
            let superclass = class!(NSColorWell) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types = CString::new("v@:B").unwrap();
            class_addMethod(
                cls,
                sel!(activate:),
                glass_tint_well_activate as *mut c_void,
                types.as_ptr(),
            );
            objc_registerClassPair(cls);
            GlassTintWellClass(cls)
        })
        .0
}

/// Create an in-settings glass preview using abstract shapes only, never real windows or clipboard data.
pub(super) unsafe fn make_glass_preview(
    parent: *mut AnyObject,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    switcher: bool,
) -> *mut AnyObject {
    let palette = settings_palette();
    let stage_color = crate::theme::settings_preview_stage_color(palette.dark);
    let material = crate::glass::PanelMaterial::effective();
    // The reported preview surface is the material's actual backdrop color: the theme card
    // surface for translucent materials, the window surface for the opaque one.
    let preview_surface = if material == crate::glass::PanelMaterial::Opaque {
        palette.window_bg
    } else {
        palette.card_bg
    };
    crate::e2e_state::set_settings_preview_colors(stage_color, preview_surface);
    let stage: *mut AnyObject = msg_send![class!(NSView), alloc];
    let stage: *mut AnyObject = msg_send![
        stage,
        initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    ];
    let _: () = msg_send![stage, setWantsLayer: true];
    let stage_layer: *mut AnyObject = msg_send![stage, layer];
    if !stage_layer.is_null() {
        crate::ffi::layer_set_background(stage_layer, crate::ffi::hex_to_cg_color(stage_color));
        let _: () = msg_send![stage_layer, setCornerRadius: crate::theme::RADIUS_CARD];
        let _: () = msg_send![stage_layer, setMasksToBounds: true];
    }
    let stage_id = make_nsstring("settings-preview-stage");
    let _: () = msg_send![stage, setAccessibilityIdentifier: stage_id];
    CFRelease(stage_id as *const c_void);
    let _: () = msg_send![parent, addSubview: stage];
    release_obj(stage);

    let supports_glass = AnyClass::get(c"NSGlassEffectView").is_some();
    let content_parent: *mut AnyObject;
    let glass: *mut AnyObject;
    match material {
        crate::glass::PanelMaterial::LiquidGlass if supports_glass => {
            let glass_cls = AnyClass::get(c"NSGlassEffectView").unwrap();
            let view: *mut AnyObject = msg_send![glass_cls, alloc];
            glass = msg_send![
                view,
                initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
            ];
            let _: () = msg_send![glass, setCornerRadius: crate::theme::RADIUS_CARD];
            let style = if crate::config::effective_glass_style() == "clear" {
                1i64
            } else {
                0i64
            };
            let _: () = msg_send![glass, setStyle: style];
            let tint = crate::ffi::hex_to_ns_color(crate::config::parse_hex8(
                &crate::config::effective_glass_tint(),
            ));
            let _: () = msg_send![glass, setTintColor: tint];
            let _: () = msg_send![glass, setWantsLayer: true];
            let layer: *mut AnyObject = msg_send![glass, layer];
            if !layer.is_null() {
                let _: () = msg_send![layer, setCornerRadius: crate::theme::RADIUS_CARD];
                let _: () = msg_send![layer, setMasksToBounds: true];
            }
            let inner: *mut AnyObject = msg_send![class!(NSView), alloc];
            let inner: *mut AnyObject = msg_send![
                inner,
                initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h))
            ];
            let _: () = msg_send![inner, setAutoresizingMask: 18u64];
            let _: () = msg_send![glass, setContentView: inner];
            content_parent = inner;
        }
        // Frost — and Liquid Glass on pre-26 macOS, which never had the Glass class.
        crate::glass::PanelMaterial::LiquidGlass | crate::glass::PanelMaterial::Frost => {
            let view: *mut AnyObject = msg_send![class!(NSVisualEffectView), alloc];
            glass = msg_send![
                view,
                initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
            ];
            // BehindWindow: blur the stage underneath, like the real panels do.
            let _: () = msg_send![glass, setBlendingMode: 0u64];
            let _: () = msg_send![glass, setMaterial: crate::glass::frost_material()];
            let _: () = msg_send![glass, setState: 1u64];
            let _: () = msg_send![glass, setWantsLayer: true];
            let layer: *mut AnyObject = msg_send![glass, layer];
            if !layer.is_null() {
                let _: () = msg_send![layer, setCornerRadius: crate::theme::RADIUS_CARD];
                let _: () = msg_send![layer, setMasksToBounds: true];
            }
            // Same server-side blur as the real panels: the rounded mask keeps the
            // preview's corners square-blur-free too.
            let mask = crate::glass::rounded_effect_mask(crate::theme::RADIUS_CARD);
            if !mask.is_null() {
                let _: () = msg_send![glass, setMaskImage: mask];
                release_obj(mask);
            }
            content_parent = glass;
        }
        crate::glass::PanelMaterial::Opaque => {
            let view: *mut AnyObject = msg_send![class!(NSView), alloc];
            glass = msg_send![
                view,
                initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
            ];
            let _: () = msg_send![glass, setWantsLayer: true];
            let layer: *mut AnyObject = msg_send![glass, layer];
            if !layer.is_null() {
                crate::ffi::layer_set_background(
                    layer,
                    crate::ffi::hex_to_cg_color(palette.window_bg),
                );
                let _: () = msg_send![layer, setCornerRadius: crate::theme::RADIUS_CARD];
                let _: () = msg_send![layer, setMasksToBounds: true];
            }
            content_parent = glass;
        }
    }
    let _: () = msg_send![glass, setAutoresizingMask: 0u64];
    let _: () = msg_send![parent, addSubview: glass];
    release_obj(glass);

    if switcher {
        let tile_color = if palette.dark {
            crate::theme::PREVIEW_TILE_DARK
        } else {
            crate::theme::PREVIEW_TILE_LIGHT
        };
        let tile_border = if palette.dark {
            crate::theme::PREVIEW_TILE_BORDER_DARK
        } else {
            crate::theme::PREVIEW_TILE_BORDER_LIGHT
        };
        let tile_w = ((w - 42.0) / 2.0).max(56.0);
        add_preview_tile(
            content_parent,
            NSRect::new(NSPoint::new(14.0, 19.0), NSSize::new(tile_w, 52.0)),
            tile_color,
            crate::theme::RADIUS_CONTROL,
            tile_border,
        );
        add_preview_tile(
            content_parent,
            NSRect::new(
                NSPoint::new(w - 14.0 - tile_w, 19.0),
                NSSize::new(tile_w, 52.0),
            ),
            tile_color,
            crate::theme::RADIUS_CONTROL,
            tile_border,
        );
    } else {
        let tile_border = if palette.dark {
            crate::theme::PREVIEW_TILE_BORDER_DARK
        } else {
            crate::theme::PREVIEW_TILE_BORDER_LIGHT
        };
        add_preview_tile(
            content_parent,
            NSRect::new(NSPoint::new(12.0, h - 22.0), NSSize::new(w - 24.0, 10.0)),
            if palette.dark {
                crate::theme::PREVIEW_TILE_DARK
            } else {
                crate::theme::PREVIEW_TILE_LIGHT
            },
            crate::theme::RADIUS_CONTROL,
            tile_border,
        );
        add_preview_tile(
            content_parent,
            NSRect::new(NSPoint::new(12.0, 25.0), NSSize::new(w - 24.0, 12.0)),
            if palette.dark {
                crate::theme::PREVIEW_TILE_DARK_SECONDARY
            } else {
                crate::theme::PREVIEW_TILE_LIGHT_SECONDARY
            },
            crate::theme::RADIUS_CONTROL,
            tile_border,
        );
        add_preview_tile(
            content_parent,
            NSRect::new(NSPoint::new(12.0, 9.0), NSSize::new(w - 42.0, 8.0)),
            if palette.dark {
                crate::theme::PREVIEW_TILE_DARK_TERTIARY
            } else {
                crate::theme::PREVIEW_TILE_LIGHT_TERTIARY
            },
            crate::theme::RADIUS_CONTROL,
            tile_border,
        );
    }
    glass
}

pub(super) unsafe fn preview_stage_is_present(preview: *mut AnyObject) -> bool {
    if preview.is_null() {
        return false;
    }
    let parent: *mut AnyObject = msg_send![preview, superview];
    let siblings: *mut AnyObject = msg_send![parent, subviews];
    if siblings.is_null() {
        return false;
    }
    let count: usize = msg_send![siblings, count];
    for index in 0..count {
        let sibling: *mut AnyObject = msg_send![siblings, objectAtIndex: index];
        let identifier: *mut AnyObject = msg_send![sibling, accessibilityIdentifier];
        if crate::ffi::nsstring_to_rust(identifier) != "settings-preview-stage" {
            continue;
        }
        let stage_frame: NSRect = msg_send![sibling, frame];
        let preview_frame: NSRect = msg_send![preview, frame];
        let layer: *mut AnyObject = msg_send![sibling, layer];
        let radius: f64 = msg_send![layer, cornerRadius];
        if (stage_frame.origin.x - preview_frame.origin.x).abs() <= 0.5
            && (stage_frame.origin.y - preview_frame.origin.y).abs() <= 0.5
            && (stage_frame.size.width - preview_frame.size.width).abs() <= 0.5
            && (stage_frame.size.height - preview_frame.size.height).abs() <= 0.5
            && (radius - crate::theme::RADIUS_CARD).abs() <= 0.5
        {
            return true;
        }
    }
    false
}

pub(super) unsafe fn add_preview_tile(
    parent: *mut AnyObject,
    frame: NSRect,
    color_hex: u32,
    radius: f64,
    border_hex: u32,
) {
    let tile: *mut AnyObject = msg_send![class!(NSView), alloc];
    let tile: *mut AnyObject = msg_send![tile, initWithFrame: frame];
    let _: () = msg_send![tile, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![tile, layer];
    if !layer.is_null() {
        let _: () = msg_send![layer, setCornerRadius: radius];
        crate::ffi::layer_set_background(layer, crate::ffi::hex_to_cg_color(color_hex));
        crate::ffi::layer_set_border(layer, crate::ffi::hex_to_cg_color(border_hex));
        let _: () = msg_send![layer, setBorderWidth: 1.0f64];
    }
    let _: () = msg_send![parent, addSubview: tile];
    release_obj(tile);
}

pub(super) unsafe fn add_preview_caption(
    parent: *mut AnyObject,
    text: &str,
    x: f64,
    y: f64,
    w: f64,
) {
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let label: *mut AnyObject = msg_send![
        label,
        initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, 18.0))
    ];
    let ns = make_nsstring(text);
    let _: () = msg_send![label, setStringValue: ns];
    CFRelease(ns as *const c_void);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let color = settings_text_color(SettingsTextRole::Secondary);
    let font: *mut AnyObject =
        msg_send![class!(NSFont), messageFontOfSize: crate::theme::FONT_CAPTION];
    let _: () = msg_send![label, setTextColor: color];
    let _: () = msg_send![label, setFont: font];
    let _: () = msg_send![parent, addSubview: label];
    release_obj(label);
}

pub(super) unsafe fn configure_glass_tint_panel(target: *mut AnyObject) {
    let panel: *mut AnyObject = msg_send![class!(NSColorPanel), sharedColorPanel];
    let _: () = msg_send![panel, setShowsAlpha: true];
    let _: () = msg_send![panel, setContinuous: true];
    let _: () = msg_send![panel, setTarget: target];
    let _: () = msg_send![panel, setAction: sel!(handleGlassTintPanelChanged:)];
    if !GLASS_TINT_PANEL_OBSERVER_INSTALLED.load(Ordering::SeqCst) {
        let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
        let name = make_nsstring("NSWindowWillCloseNotification");
        let _: () = msg_send![
            center,
            addObserver: target,
            selector: sel!(handleGlassTintPanelWillClose:),
            name: name,
            object: panel
        ];
        CFRelease(name as *const c_void);
        GLASS_TINT_PANEL_OBSERVER_INSTALLED.store(true, Ordering::SeqCst);
    }

    // The accessory width must match the color panel; NSColorPanel does not widen itself for an
    // oversized accessory view.
    let panel_frame: NSRect = msg_send![panel, frame];
    let accessory_w = panel_frame.size.width.max(250.0);
    let accessory_margin = 8.0;
    let accessory: *mut AnyObject = msg_send![class!(NSView), alloc];
    let accessory: *mut AnyObject = msg_send![
        accessory,
        initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(accessory_w, 34.0))
    ];
    let reset = SettingsButton::action(
        NSRect::new(
            NSPoint::new(accessory_margin, 3.0),
            NSSize::new(accessory_w - accessory_margin * 2.0, 28.0),
        ),
        &t("settings.reset_glass_tint"),
        target,
        sel!(handleGlassTintReset:),
        SettingsButtonRole::Action,
    );
    // Use the native fitting width for the localized title so a full-width button does not distort
    // the system bezel's corner proportions.
    let fitting: NSSize = msg_send![reset, fittingSize];
    let max_reset_w = accessory_w - accessory_margin * 2.0;
    let reset_w = if fitting.width > 0.0 {
        fitting.width.clamp(80.0, max_reset_w)
    } else {
        max_reset_w.min(140.0)
    };
    let _: () = msg_send![
        reset,
        setFrame: NSRect::new(
            NSPoint::new((accessory_w - reset_w) / 2.0, 3.0),
            NSSize::new(reset_w, 28.0)
        )
    ];
    let _: () = msg_send![accessory, addSubview: reset];
    release_obj(reset);
    let _: () = msg_send![panel, setAccessoryView: accessory];
    release_obj(accessory);
}

/// Close and detach the system color panel so it cannot mutate a dangling color well after the
/// settings window is destroyed.
pub(super) unsafe fn close_glass_tint_panel(well: *mut AnyObject) {
    // NSColorPanel.sharedColorPanel is independent from the settings window, and AppKit can
    // report the color well as inactive while the shared panel is still visible. Always hide the
    // panel; `isActive` only decides whether the well needs an additional deactivate call.
    if !well.is_null() {
        let active: bool = msg_send![well, isActive];
        if active {
            let _: () = msg_send![well, deactivate];
        }
    }
    let panel: *mut AnyObject = msg_send![class!(NSColorPanel), sharedColorPanel];
    if !panel.is_null() {
        let _: () = msg_send![panel, orderOut: std::ptr::null::<AnyObject>()];
        let _: () = msg_send![panel, setAccessoryView: std::ptr::null::<AnyObject>()];
    }
    restore_glass_tint_group();
}

pub(super) unsafe fn update_settings_preview_views() {
    let style = if crate::config::effective_glass_style() == "clear" {
        1i64
    } else {
        0i64
    };
    let tint = crate::ffi::hex_to_ns_color(crate::config::parse_hex8(
        &crate::config::effective_glass_tint(),
    ));
    super::with_settings_ui(|ui| {
        let Some(ui) = ui.as_ref() else { return };
        for preview in [ui.glass_preview_switcher, ui.glass_preview_clipboard] {
            if preview.is_null() {
                continue;
            }
            let supports_style: bool = msg_send![preview, respondsToSelector: sel!(setStyle:)];
            if supports_style {
                let _: () = msg_send![preview, setStyle: style];
            }
            let supports_tint: bool = msg_send![preview, respondsToSelector: sel!(setTintColor:)];
            if supports_tint {
                let _: () = msg_send![preview, setTintColor: tint];
            }
        }
    });
}

/// Apply the temporary glass preview to the real overlays and the two in-settings mock overlays.
pub(crate) fn apply_glass_preview() {
    unsafe {
        crate::overlay::apply_glass_properties();
        crate::clipboard::apply_glass_properties();
        crate::keystroke_display::apply_glass_properties();
        update_settings_preview_views();
    }
}

/// The shared write path for the color well/panel: store the new color in CONFIG, schedule a
/// debounced persist, then apply it to the real overlays and the two in-settings mock
/// overlays.
unsafe fn update_glass_tint_from_color(color: *mut AnyObject, sync_well: bool) {
    if GLASS_UI_UPDATE.load(Ordering::SeqCst) {
        return;
    }
    let Some(hex) = ns_color_to_hex(color) else {
        return;
    };
    super::with_settings_ui(|ui| {
        if let Some(ui) = ui.as_ref() {
            if sync_well {
                GLASS_UI_UPDATE.store(true, Ordering::SeqCst);
                let _: () = msg_send![ui.glass_tint, setColor: color];
                GLASS_UI_UPDATE.store(false, Ordering::SeqCst);
            }
            set_glass_tint_hex_caption(ui.glass_tint_hex, &hex);
        }
    });
    if let Ok(mut w) = crate::config::CONFIG.write() {
        w.appearance.glass_tint = hex;
    }
    crate::config::schedule_config_persist();
    apply_glass_preview();
}

unsafe fn set_glass_tint_hex_caption(caption: *mut AnyObject, value: &str) {
    if caption.is_null() {
        return;
    }
    let text = crate::ffi::make_nsstring(&display_glass_tint_hex(value));
    let _: () = msg_send![caption, setStringValue: text];
    crate::ffi::CFRelease(text as *const c_void);
}

pub(crate) extern "C" fn on_glass_tint_changed(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    unsafe { update_glass_tint_from_color(sender as *mut AnyObject, false) }
}

pub(crate) extern "C" fn on_glass_tint_panel_changed(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    unsafe {
        let panel: *mut AnyObject = msg_send![class!(NSColorPanel), sharedColorPanel];
        let color: *mut AnyObject = msg_send![panel, color];
        update_glass_tint_from_color(color, true);
    }
}

pub(crate) extern "C" fn on_glass_tint_panel_will_close(
    _self: *mut c_void,
    _cmd: Sel,
    _notification: *mut c_void,
) {
    restore_glass_tint_group();
}

pub(crate) extern "C" fn on_glass_tint_reset(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    unsafe {
        let default_hex = Config::default().appearance.glass_tint;
        let color = crate::ffi::hex_to_ns_color(crate::config::parse_hex8(&default_hex));
        GLASS_UI_UPDATE.store(true, Ordering::SeqCst);
        super::with_settings_ui(|ui| {
            if let Some(ui) = ui.as_ref() {
                let _: () = msg_send![ui.glass_tint, setColor: color];
                set_glass_tint_hex_caption(ui.glass_tint_hex, &default_hex);
            }
        });
        let panel: *mut AnyObject = msg_send![class!(NSColorPanel), sharedColorPanel];
        let _: () = msg_send![panel, setColor: color];
        GLASS_UI_UPDATE.store(false, Ordering::SeqCst);
        // Reset = write the default straight back to CONFIG (matching live-apply semantics).
        if let Ok(mut w) = crate::config::CONFIG.write() {
            w.appearance.glass_tint = default_hex;
        }
        crate::config::persist_config_now();
        apply_glass_preview();
    }
}

#[cfg(test)]
mod tests {
    use super::{display_glass_tint_hex, glass_tint_control_frames};

    #[test]
    fn glass_tint_caption_formats_user_color_as_hex() {
        assert_eq!(display_glass_tint_hex("0a84ffcc"), "#0A84FFCC");
        assert_eq!(display_glass_tint_hex("#aabbccdd"), "#AABBCCDD");
    }

    #[test]
    fn glass_tint_caption_and_swatch_fit_the_control_column() {
        let (caption, well) = glass_tint_control_frames(200.0, 32.0);
        assert_eq!(caption.origin.x, 0.0);
        assert_eq!(caption.size.width + 8.0, well.origin.x);
        assert_eq!(well.origin.x + well.size.width, 200.0);
        assert_eq!(caption.size.height, well.size.height);
    }
}
