//! Native window classes and appearance surfaces for the settings window.
//! 设置窗口的原生窗口类与外观表面。

use super::*;

struct SettingsWindowClass(*mut AnyObject);
unsafe impl Send for SettingsWindowClass {}
unsafe impl Sync for SettingsWindowClass {}

/// Root view used by the settings window so AppKit can resolve macOS 27's container-relative
/// corner radii while the layer still clips every custom child into the same surface.
/// 设置窗口根视图：让 AppKit 在 macOS 27 上解析相对于窗口的圆角，同时由同一图层裁切所有自绘子视图。
struct SettingsRootViewClass(*mut AnyObject);
unsafe impl Send for SettingsRootViewClass {}
unsafe impl Sync for SettingsRootViewClass {}

static SETTINGS_ROOT_VIEW_CLS: OnceLock<SettingsRootViewClass> = OnceLock::new();

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

extern "C" fn settings_root_corner_configuration(_self: *mut c_void, _cmd: Sel) -> *mut AnyObject {
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

extern "C" fn settings_root_view_did_change_effective_corner_radii(this: *mut c_void, _cmd: Sel) {
    unsafe {
        let view = this as *mut AnyObject;
        let radii: *mut AnyObject = msg_send![view, effectiveCornerRadii];
        let radius = if radii.is_null() {
            settings_effective_corner_radius(None, 26.0)
        } else {
            let top_left: f64 = msg_send![radii, topLeft];
            let top_right: f64 = msg_send![radii, topRight];
            let bottom_left: f64 = msg_send![radii, bottomLeft];
            let bottom_right: f64 = msg_send![radii, bottomRight];
            settings_effective_corner_radius(
                Some([top_left, top_right, bottom_left, bottom_right]),
                26.0,
            )
        };
        let layer: *mut AnyObject = msg_send![view, layer];
        if !layer.is_null() {
            let _: () = msg_send![layer, setCornerRadius: radius];
            let _: () = msg_send![layer, setMasksToBounds: true];
        }
    }
}

pub(in crate::settings) fn settings_root_view_class() -> *mut AnyObject {
    SETTINGS_ROOT_VIEW_CLS
        .get_or_init(|| unsafe {
            let name = CString::new("OhMyTabSettingsRootView").unwrap();
            let superclass = class!(NSView) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            if AnyClass::get(c"NSViewCornerConfiguration").is_some()
                && AnyClass::get(c"NSViewCornerRadius").is_some()
            {
                class_addMethod(
                    cls,
                    sel!(cornerConfiguration),
                    settings_root_corner_configuration as *mut c_void,
                    CString::new("@@:").unwrap().as_ptr(),
                );
                class_addMethod(
                    cls,
                    sel!(viewDidChangeEffectiveCornerRadii),
                    settings_root_view_did_change_effective_corner_radii as *mut c_void,
                    CString::new("v@:").unwrap().as_ptr(),
                );
            }
            objc_registerClassPair(cls);
            SettingsRootViewClass(cls)
        })
        .0
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
/// 窗口 resize 后重新应用 AppKit 计算出的动态圆角。
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
        root as *mut c_void,
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

static SETTINGS_WINDOW_CLS: OnceLock<SettingsWindowClass> = OnceLock::new();

pub(in crate::settings) fn settings_window_class() -> *mut AnyObject {
    SETTINGS_WINDOW_CLS
        .get_or_init(|| unsafe {
            let name = CString::new("OhMyTabSettingsWindow").unwrap();
            let superclass = class!(NSWindow) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types = CString::new("v@:@").unwrap(); // -performClose:(id)sender -> void
            class_addMethod(
                cls,
                sel!(performClose:),
                settings_window_perform_close as *mut c_void,
                types.as_ptr(),
            );
            let types_close = CString::new("v@:").unwrap(); // -close -> void
            class_addMethod(
                cls,
                sel!(close),
                settings_window_close as *mut c_void,
                types_close.as_ptr(),
            );
            let types_event = CString::new("v@:@").unwrap(); // -sendEvent:(NSEvent*) -> void
            class_addMethod(
                cls,
                sel!(sendEvent:),
                settings_window_send_event as *mut c_void,
                types_event.as_ptr(),
            );
            let types_key = CString::new("B@:@").unwrap(); // -performKeyEquivalent:(NSEvent*) -> BOOL
            class_addMethod(
                cls,
                sel!(performKeyEquivalent:),
                settings_window_perform_key_equivalent as *mut c_void,
                types_key.as_ptr(),
            );
            let types_resize = CString::new("v@:{CGSize=dd}").unwrap(); // -resizeSubviewsWithOldSize:(NSSize) -> void
            class_addMethod(
                cls,
                sel!(resizeSubviewsWithOldSize:),
                settings_window_resize_subviews as *mut c_void,
                types_resize.as_ptr(),
            );
            objc_registerClassPair(cls);
            SettingsWindowClass(cls)
        })
        .0
}

/// Apply the resolved appearance to the settings window and its semantic AppKit controls.
/// 将解析后的主题应用到设置窗口及其依赖语义颜色的 AppKit 控件。
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
