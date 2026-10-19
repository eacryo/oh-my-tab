//! Native window classes and appearance surfaces for the settings window.

use super::*;

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
            settings_effective_corner_radius(None, crate::theme::SETTINGS_WINDOW_RADIUS)
        } else {
            let top_left: f64 = msg_send![radii, topLeft];
            let top_right: f64 = msg_send![radii, topRight];
            let bottom_left: f64 = msg_send![radii, bottomLeft];
            let bottom_right: f64 = msg_send![radii, bottomRight];
            settings_effective_corner_radius(
                Some([top_left, top_right, bottom_left, bottom_right]),
                crate::theme::SETTINGS_WINDOW_RADIUS,
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
