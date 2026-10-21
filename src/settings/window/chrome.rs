//! Native window classes and appearance surfaces for the settings window.

use super::*;

/// Identifier of the strip that carries the window title in the sidebar's left part. The smoke
/// runner finds it by this name instead of assuming a subview order.
pub(in crate::settings) const SETTINGS_TITLEBAR_IDENTIFIER: &str =
    "settings-titlebar-solid-surface";

/// Height of the window's title bar band, from the mockup's
/// `.settings-titlebar { height: 36px }`.
pub(in crate::settings) const TITLEBAR_H: f64 = 36.0;

// NSWindowCollectionBehaviorFullScreenNone | FullScreenDisallowsTiling.
pub(in crate::settings) const SETTINGS_ZOOM_COLLECTION_BEHAVIOR: u64 = (1 << 9) | (1 << 12);

fn vertical_zoom_frame(current: NSRect, visible: NSRect) -> NSRect {
    NSRect::new(
        NSPoint::new(current.origin.x, visible.origin.y),
        NSSize::new(current.size.width, visible.size.height),
    )
}

extern "C" fn settings_window_standard_frame(
    _this: *mut AnyObject,
    _cmd: Sel,
    window: *mut AnyObject,
    default_frame: NSRect,
) -> NSRect {
    crate::callback_guard::event("settings_window_standard_frame", default_frame, || unsafe {
        let current: NSRect = msg_send![window, frame];
        let screen: *mut AnyObject = msg_send![window, screen];
        let visible = if screen.is_null() {
            default_frame
        } else {
            msg_send![screen, visibleFrame]
        };
        vertical_zoom_frame(current, visible)
    })
}

/// Line box of the band's 12pt title (`.settings-title { font-size: var(--fs-12) }`).
fn titlebar_title_line_height() -> f64 {
    crate::theme::line_height(
        crate::theme::FONT_CAPTION,
        crate::theme::LINE_HEIGHT_CAPTION_RATIO,
    )
}

/// Frame of the centered window title inside a `width`-wide band.
///
/// The frame is the title's OWN width, centered: the label is positioned by geometry rather than by
/// a centered cell alignment, because the numeric value of `NSTextAlignmentCenter` differs between
/// AppKit generations (an alignment constant that means "center" when the code is written can draw
/// right-aligned on a newer framework -- measured: the title sat 45pt right of the window's middle).
pub(in crate::settings) fn titlebar_title_frame(width: f64, band_h: f64, text_w: f64) -> NSRect {
    let line_h = titlebar_title_line_height();
    let title_w = if text_w.is_finite() && text_w > 0.0 {
        text_w.min(TITLEBAR_TITLE_MAX_W).min(width.max(1.0))
    } else {
        TITLEBAR_TITLE_MAX_W.min(width.max(1.0))
    };
    NSRect::new(
        NSPoint::new(((width - title_w) / 2.0).max(0.0), (band_h - line_h) / 2.0),
        crate::settings::NSSize::new(title_w, line_h),
    )
}

/// Maximum width of the centered title label.
const TITLEBAR_TITLE_MAX_W: f64 = 120.0;

/// The band is laid out from its top edge, so its children (the title) use top-down y.
extern "C" fn settings_titlebar_view_is_flipped(
    _this: *mut AnyObject,
    _cmd: Sel,
) -> objc2::runtime::Bool {
    objc2::runtime::Bool::YES
}

extern "C" fn settings_titlebar_hit_test(
    _this: *mut AnyObject,
    _cmd: Sel,
    _point: NSPoint,
) -> *mut AnyObject {
    // Decorative chrome must not intercept AppKit's native window buttons.
    std::ptr::null_mut()
}

/// Class of the title bar band. It is flipped for its children and declines every hit: AppKit paints
/// the traffic lights into the window's frame view above all content, so the band only paints the
/// strip behind them on the sidebar's side and must never take a click away from the content below.
pub(in crate::settings) fn settings_titlebar_view_class() -> *mut AnyObject {
    static CLASS: OnceLock<crate::ffi::StaticClass> = OnceLock::new();
    crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabSettingsTitlebarView", || {
        crate::ffi::build_class(
            c"OhMyTabSettingsTitlebarView",
            class!(NSView),
            |builder| unsafe {
                builder.add_method::<AnyObject, _>(
                    sel!(isFlipped),
                    settings_titlebar_view_is_flipped
                        as unsafe extern "C" fn(*mut AnyObject, Sel) -> objc2::runtime::Bool,
                );
                builder.add_method::<AnyObject, _>(
                    sel!(hitTest:),
                    settings_titlebar_hit_test
                        as unsafe extern "C" fn(*mut AnyObject, Sel, NSPoint) -> *mut AnyObject,
                );
            },
        )
    })
    .0 as *mut AnyObject
}

/// Build the title bar band, which the mock draws across the full window width: solid
/// `window_bg` (`.settings-titlebar { background: var(--panel-solid) }`) over the sidebar's part
/// plus a 1pt `separator` hairline (`border-bottom: 1px solid var(--settings-divider)`). The title
/// label is a full-width centered field added separately, so the band itself carries no text.
///
/// `over_sidebar` is the part of the band that sits over the sidebar column; the root surface
/// already paints `window_bg` over the detail pane, so this strip exists only so that a glass
/// sidebar cannot show through the band. It is added to `content` and moves with it.
pub(in crate::settings) unsafe fn build_settings_titlebar_band(
    content: *mut AnyObject,
    content_h: f64,
    band_h: f64,
    width: f64,
    palette: UiPalette,
) {
    if width <= 0.0 || band_h <= 0.0 || content_h < band_h {
        return;
    }
    let band_class = settings_titlebar_view_class();
    let band: *mut AnyObject = msg_send![band_class, alloc];
    let band: *mut AnyObject = msg_send![
        band,
        initWithFrame: NSRect::new(
            NSPoint::new(0.0, content_h - band_h),
            crate::settings::NSSize::new(width, band_h)
        )
    ];
    let identifier = make_nsstring(SETTINGS_TITLEBAR_IDENTIFIER);
    let _: () = msg_send![band, setIdentifier: identifier];
    release_obj(identifier);
    let _: () = msg_send![band, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![band, layer];
    if !layer.is_null() {
        layer_set_background(layer, crate::ffi::hex_to_cg_color(palette.window_bg));
    }
    let _: () = msg_send![band, setAutoresizingMask: 13u64]; // width+height sizable, pinned top
    let _: () = msg_send![content, addSubview: band];
    // Retained by `content` until the rebuild drops the hierarchy; the title and the hairline are
    // added to it below.
    let band_view = band;

    // The same class: the hairline is a decoration too, so it must not take a click either.
    let hairline: *mut AnyObject = msg_send![band_class, alloc];
    let hairline: *mut AnyObject = msg_send![
        hairline,
        initWithFrame: NSRect::new(
            NSPoint::new(0.0, band_h - 1.0),
            crate::settings::NSSize::new(width, 1.0)
        )
    ];
    let _: () = msg_send![hairline, setWantsLayer: true];
    let hairline_layer: *mut AnyObject = msg_send![hairline, layer];
    if !hairline_layer.is_null() {
        layer_set_background(
            hairline_layer,
            crate::ffi::hex_to_cg_color(palette.separator),
        );
    }
    let _: () = msg_send![hairline, setAutoresizingMask: 11u64]; // stays on the band's bottom edge
    let _: () = msg_send![band_view, addSubview: hairline];
    release_obj(hairline);

    // The title is a child of the band (only the band is flipped), so its frame is measured from the
    // band's top edge and nothing the band covers can draw over it. The window's own title bar is
    // transparent and hidden, so this label is the only title the window shows.
    add_settings_titlebar_title(band_view, band_h, width, palette);
}

/// The traffic-light buttons AppKit puts in its title bar, in `NSWindowButton` order:
/// Close=0, Miniaturize=1, Zoom=2.
pub(in crate::settings) unsafe fn traffic_light_buttons(
    window: *mut AnyObject,
) -> [*mut AnyObject; 3] {
    std::array::from_fn(|tag| msg_send![window, standardWindowButton: tag as isize])
}

/// The band title's type (the mock's `.settings-title { font-size: var(--fs-12); font-weight: 500 }`).
///
/// Cached for the window's lifetime: the system font object is immutable, and every rebuild of the
/// settings content would otherwise allocate the same face again.
fn titlebar_title_font() -> *mut AnyObject {
    static FONT: OnceLock<usize> = OnceLock::new();
    *FONT.get_or_init(|| unsafe {
        let font: *mut AnyObject = msg_send![
            class!(NSFont),
            systemFontOfSize: crate::theme::FONT_CAPTION,
            weight: crate::theme::FONT_WEIGHT_MEDIUM
        ];
        font as usize
    }) as *mut AnyObject
}

/// Add the band's title label: 12pt medium, `muted_text` (`.settings-title { font-size:
/// var(--fs-12); font-weight: 500; color: var(--text-3) }`), centered on the window.
///
/// It is a sibling placed after the band and has no hit target of its own (`NSTextField` declines
/// the pointer), so the band underneath stays a pure decoration.
unsafe fn add_settings_titlebar_title(
    band: *mut AnyObject,
    band_h: f64,
    width: f64,
    palette: UiPalette,
) -> *mut AnyObject {
    // The band is flipped for its children, so the title frame's y is measured from the band's
    // top edge. The frame is settled after the text and font exist: its width is the title's own,
    // so the label sits on the window's midline by geometry (see titlebar_title_frame).
    let frame = titlebar_title_frame(width, band_h, 0.0);
    let field: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let field: *mut AnyObject = msg_send![field, initWithFrame: frame];
    set_field(field, t("settings.window_title"));
    let _: () = msg_send![field, setBezeled: false];
    let _: () = msg_send![field, setDrawsBackground: false];
    let _: () = msg_send![field, setEditable: false];
    let _: () = msg_send![field, setSelectable: false];
    let _: () = msg_send![field, setAlignment: 0isize]; // NSTextAlignmentLeft; the frame centers it
    let _: () = msg_send![field, setLineBreakMode: 4isize]; // NSLineBreakByTruncatingTail
    let _: () = msg_send![field, setFont: titlebar_title_font()];
    let _: () = msg_send![field, setTextColor: crate::ffi::hex_to_ns_color(palette.muted_text)];
    let cell: *mut AnyObject = msg_send![field, cell];
    let text_size: NSSize = msg_send![cell, cellSize];
    let _: () = msg_send![
        field,
        setFrame: titlebar_title_frame(width, band_h, text_size.width)
    ];
    // Margins on both sides keep the centered frame on the midline as the window resizes.
    let _: () = msg_send![field, setAutoresizingMask: 11u64];
    let _: () = msg_send![band, addSubview: field];
    release_obj(field);
    field
}

pub(in crate::settings) fn settings_effective_corner_radius(
    radii: Option<[f64; 4]>,
    fallback: f64,
) -> f64 {
    let Some(radii) = radii else {
        return fallback;
    };
    if radii.iter().all(|radius| radius.is_finite()) {
        radii.iter().copied().fold(0.0, f64::max).max(0.0)
    } else {
        fallback
    }
}

extern "C" fn settings_root_corner_configuration(
    _self: *mut AnyObject,
    _cmd: Sel,
) -> *mut AnyObject {
    unsafe {
        let Some(radius_cls) = AnyClass::get(c"NSViewCornerRadius") else {
            return std::ptr::null_mut();
        };
        let Some(config_cls) = AnyClass::get(c"NSViewCornerConfiguration") else {
            return std::ptr::null_mut();
        };
        let radius: *mut AnyObject = msg_send![
            radius_cls,
            containerConcentricRadiusWithMinimum: 0.0f64
        ];
        if radius.is_null() {
            return std::ptr::null_mut();
        }
        msg_send![config_cls, configurationWithRadius: radius]
    }
}

extern "C" fn settings_root_view_did_change_effective_corner_radii(
    this: *mut AnyObject,
    _cmd: Sel,
) {
    unsafe {
        let view = this;
        let radii: *mut AnyObject = msg_send![view, effectiveCornerRadii];
        let radius = if radii.is_null() {
            settings_effective_corner_radius(None, crate::theme::RADIUS_PANEL)
        } else {
            let top_left: f64 = msg_send![radii, topLeft];
            let top_right: f64 = msg_send![radii, topRight];
            let bottom_left: f64 = msg_send![radii, bottomLeft];
            let bottom_right: f64 = msg_send![radii, bottomRight];
            settings_effective_corner_radius(
                Some([top_left, top_right, bottom_left, bottom_right]),
                crate::theme::RADIUS_PANEL,
            )
        };
        let layer: *mut AnyObject = msg_send![view, layer];
        if !layer.is_null() {
            let _: () = msg_send![layer, setCornerRadius: radius];
            let _: () = msg_send![layer, setMasksToBounds: true];
        }
    }
}

extern "C" fn settings_window_key_down(_this: *mut AnyObject, _cmd: Sel, _event: *mut AnyObject) {
    // Unhandled keys should stop at the settings window instead of reaching NSApplication's
    // terminal responder, whose default noResponderFor:keyDown: implementation beeps.
}

/// Root view used by the settings window so AppKit can resolve macOS 27's container-relative
/// corner radii while the layer still clips every custom child into the same surface.
///
/// The two overrides only exist on systems that have the corner-radius classes at all; both are
/// added before registration, because a class cannot gain methods after it has been registered.
pub(in crate::settings) fn settings_root_view_class() -> *mut AnyObject {
    static CLASS: OnceLock<crate::ffi::StaticClass> = OnceLock::new();
    crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabSettingsRootView", || {
        crate::ffi::build_class(
            c"OhMyTabSettingsRootView",
            class!(NSView),
            |builder| unsafe {
                if AnyClass::get(c"NSViewCornerConfiguration").is_some()
                    && AnyClass::get(c"NSViewCornerRadius").is_some()
                {
                    builder.add_method::<AnyObject, _>(
                        sel!(cornerConfiguration),
                        settings_root_corner_configuration
                            as unsafe extern "C" fn(*mut AnyObject, Sel) -> *mut AnyObject,
                    );
                    builder.add_method::<AnyObject, _>(
                        sel!(viewDidChangeEffectiveCornerRadii),
                        settings_root_view_did_change_effective_corner_radii
                            as unsafe extern "C" fn(*mut AnyObject, Sel),
                    );
                }
            },
        )
    })
    .0 as *mut AnyObject
}

pub(in crate::settings) unsafe fn settings_root_view_for_host(
    host: *mut AnyObject,
) -> *mut AnyObject {
    if host.is_null() {
        return std::ptr::null_mut();
    }
    let subviews: *mut AnyObject = msg_send![host, subviews];
    if subviews.is_null() {
        return std::ptr::null_mut();
    }
    let root_class = settings_root_view_class();
    let count: usize = msg_send![subviews, count];
    for index in 0..count {
        let subview: *mut AnyObject = msg_send![subviews, objectAtIndex: index as isize];
        if !subview.is_null() && msg_send![subview, isKindOfClass: root_class] {
            return subview;
        }
    }
    std::ptr::null_mut()
}

/// Find the title bar band among a content host's subviews, by identifier.
///
/// The smoke runner locates the band this way instead of assuming a subview order that later
/// inserts would silently change.
pub(in crate::settings) unsafe fn settings_titlebar_view_for_host(
    host: *mut AnyObject,
) -> *mut AnyObject {
    if host.is_null() {
        return std::ptr::null_mut();
    }
    let subviews: *mut AnyObject = msg_send![host, subviews];
    if subviews.is_null() {
        return std::ptr::null_mut();
    }
    let count: usize = msg_send![subviews, count];
    for index in 0..count {
        let subview: *mut AnyObject = msg_send![subviews, objectAtIndex: index as isize];
        if subview.is_null() {
            continue;
        }
        let identifier: *mut AnyObject = msg_send![subview, identifier];
        if crate::ffi::nsstring_to_rust(identifier) == SETTINGS_TITLEBAR_IDENTIFIER {
            return subview;
        }
    }
    std::ptr::null_mut()
}

unsafe fn settings_root_view_for_window(window: *mut AnyObject) -> *mut AnyObject {
    if window.is_null() {
        return std::ptr::null_mut();
    }
    let host: *mut AnyObject = msg_send![window, contentView];
    settings_root_view_for_host(host)
}

/// Reapply the dynamic corner result after AppKit lays out a resized window.
pub(in crate::settings) unsafe fn refresh_settings_root_corner(window: *mut AnyObject) {
    if AnyClass::get(c"NSViewCornerConfiguration").is_none()
        || AnyClass::get(c"NSViewCornerRadius").is_none()
    {
        return;
    }
    let root = settings_root_view_for_window(window);
    if root.is_null() {
        return;
    }
    let _: () = msg_send![root, invalidateCornerConfiguration];
    let _: () = msg_send![root, layoutSubtreeIfNeeded];
    settings_root_view_did_change_effective_corner_radii(
        root,
        sel!(viewDidChangeEffectiveCornerRadii),
    );
}

pub(in crate::settings) unsafe fn apply_settings_root_surface(
    window: *mut AnyObject,
    content: *mut AnyObject,
    palette: UiPalette,
    fallback_radius: f64,
) {
    let _: () = msg_send![window, setOpaque: false];
    let clear_color: *mut AnyObject = msg_send![class!(NSColor), clearColor];
    let _: () = msg_send![window, setBackgroundColor: clear_color];
    let _: () = msg_send![content, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![content, layer];
    if layer.is_null() {
        return;
    }
    layer_set_background(layer, crate::ffi::hex_to_cg_color(palette.window_bg));
    let supports_concentric = AnyClass::get(c"NSViewCornerConfiguration").is_some()
        && AnyClass::get(c"NSViewCornerRadius").is_some();
    refresh_settings_root_corner(window);
    if !supports_concentric {
        let _: () = msg_send![layer, setCornerRadius: fallback_radius];
        let _: () = msg_send![layer, setMasksToBounds: true];
    }
}

/// The class handle itself, so the overrides that dispatch to super can name the class that
/// defined them: `ObjcSuper::for_defined_class` derives the superclass from this handle.
pub(in crate::settings) fn settings_window_class_handle() -> crate::ffi::StaticClass {
    static CLASS: OnceLock<crate::ffi::StaticClass> = OnceLock::new();
    crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabSettingsWindow", || {
        crate::ffi::build_class(
            c"OhMyTabSettingsWindow",
            class!(NSWindow),
            |builder| unsafe {
                builder.add_method::<AnyObject, _>(
                    sel!(windowWillUseStandardFrame:defaultFrame:),
                    settings_window_standard_frame
                        as unsafe extern "C" fn(
                            *mut AnyObject,
                            Sel,
                            *mut AnyObject,
                            NSRect,
                        ) -> NSRect,
                );
                builder.add_method::<AnyObject, _>(
                    sel!(performClose:),
                    settings_window_perform_close
                        as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
                builder.add_method::<AnyObject, _>(
                    sel!(keyDown:),
                    settings_window_key_down
                        as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
                builder.add_method::<AnyObject, _>(
                    sel!(close),
                    settings_window_close as unsafe extern "C" fn(*mut AnyObject, Sel),
                );
                builder.add_method::<AnyObject, _>(
                    sel!(sendEvent:),
                    settings_window_send_event
                        as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
                builder.add_method::<AnyObject, _>(
                    sel!(performKeyEquivalent:),
                    settings_window_perform_key_equivalent
                        as unsafe extern "C" fn(
                            *mut AnyObject,
                            Sel,
                            *mut AnyObject,
                        ) -> objc2::runtime::Bool,
                );
            },
        )
    })
}

pub(in crate::settings) fn settings_window_class() -> *mut AnyObject {
    settings_window_class_handle().0 as *mut AnyObject
}

/// Apply the resolved appearance to the settings window and its semantic AppKit controls.
pub(in crate::settings) unsafe fn apply_settings_window_appearance(window: *mut AnyObject) {
    let name = make_nsstring(if resolved_is_dark() {
        "NSAppearanceNameDarkAqua"
    } else {
        "NSAppearanceNameAqua"
    });
    let appearance: *mut AnyObject = msg_send![class!(NSAppearance), appearanceNamed: name];
    CFRelease(name as *const c_void);
    if !appearance.is_null() {
        let _: () = msg_send![window, setAppearance: appearance];
    }
}

#[cfg(test)]
mod titlebar_tests {
    use super::{titlebar_title_frame, TITLEBAR_H};

    #[test]
    fn vertical_zoom_preserves_width_and_x_on_each_screen() {
        use super::{vertical_zoom_frame, NSPoint, NSRect, NSSize};
        let current = NSRect::new(NSPoint::new(-1200.0, 180.0), NSSize::new(820.0, 720.0));
        for visible in [
            NSRect::new(NSPoint::new(0.0, 80.0), NSSize::new(1728.0, 1000.0)),
            NSRect::new(NSPoint::new(-1920.0, -900.0), NSSize::new(1920.0, 850.0)),
        ] {
            let zoomed = vertical_zoom_frame(current, visible);
            assert_eq!(zoomed.origin.x, current.origin.x);
            assert_eq!(zoomed.size.width, current.size.width);
            assert_eq!(zoomed.origin.y, visible.origin.y);
            assert_eq!(zoomed.size.height, visible.size.height);
        }
    }

    /// `.settings-title { left: 50% }`: the title's frame is its own text width, centered on the
    /// window, so the ink is centered whatever the cell's alignment constant means. A title too
    /// wide for the window is clamped to the band's reserved width.
    #[test]
    fn the_window_title_is_centered_and_clamped() {
        for width in [820.0, 876.0, 640.0] {
            let text_w = 42.0;
            let frame = titlebar_title_frame(width, TITLEBAR_H, text_w);
            assert_eq!(frame.size.width, text_w);
            assert_eq!(
                frame.origin.x + frame.size.width / 2.0,
                width / 2.0,
                "title must be centered on a {width}pt window"
            );
            assert!((frame.origin.y + frame.size.height / 2.0 - TITLEBAR_H / 2.0).abs() < 1e-9);
        }
        // A title wider than the band's reserved width is clamped and still centered.
        let wide = titlebar_title_frame(820.0, TITLEBAR_H, 500.0);
        assert_eq!(wide.size.width, super::TITLEBAR_TITLE_MAX_W);
        assert_eq!(wide.origin.x + wide.size.width / 2.0, 410.0);
    }
}
