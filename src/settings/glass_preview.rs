//! 设置窗口 · 玻璃色调实时预览:取色器、预览面板与预览视图同步。
//! Live glass-tint preview: the color well, preview panel, and preview-view syncing.

use super::*;

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

/// 将任意 NSColor 转换到 sRGB 后编码为配置使用的 RRGGBBAA。
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

/// 原生取色器色块,固定为右侧小色块而不是拉伸成文本框宽度。
/// Native color well, kept as a compact right-side swatch instead of stretching like a text field.
pub(super) unsafe fn make_color_well(
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    value: &str,
    target: *mut AnyObject,
) -> *mut AnyObject {
    let well: *mut AnyObject = msg_send![glass_tint_well_class(), alloc];
    let well: *mut AnyObject = msg_send![
        well,
        initWithFrame: NSRect::new(NSPoint::new(x + w - 52.0, y), NSSize::new(52.0, h))
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
    well
}

/// 纯逻辑:水平居中设置窗口+颜色面板的整体,面板在设置窗口右侧并垂直居中。
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

/// 取设置窗口所在屏幕的可见区域;窗口尚未绑定屏幕时回退到主屏。
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

/// 打开取色器前把设置窗口向左移,让两个窗口作为一个整体居中。
/// Move the settings window left before opening the color panel so the two windows are centered
/// as one group.
pub(super) unsafe fn position_glass_tint_group(save_original: bool) {
    let window = match SETTINGS_UI.lock().unwrap().as_ref() {
        Some(ui) => ui.window,
        None => return,
    };
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

/// 颜色面板关闭后恢复设置窗口打开前的位置;重复调用必须安全。
/// Restore the settings window's pre-panel position after the color panel closes; repeated calls
/// are intentionally harmless.
pub(crate) fn restore_glass_tint_group() {
    let original = GLASS_TINT_GROUP_ORIGINAL_ORIGIN.lock().unwrap().take();
    let Some(origin) = original else { return };
    unsafe {
        let window = SETTINGS_UI.lock().unwrap().as_ref().map(|ui| ui.window);
        if let Some(window) = window {
            let _: () = msg_send![window, setFrameOrigin: origin];
        }
    }
}

/// 自定义 NSColorWell 在 AppKit 显示共享颜色面板前先调整窗口位置,避免左下角闪现。
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
        // AppKit 可能在 activate:期间恢复面板记忆位置,因此 super 返回后再应用一次整体布局。
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

/// 创建设置页内的玻璃预览块,内容只使用抽象形状,不暴露真实窗口或剪贴板数据。
/// Create an in-settings glass preview using abstract shapes only, never real windows or clipboard data.
pub(super) unsafe fn make_glass_preview(
    parent: *mut AnyObject,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    switcher: bool,
) -> *mut AnyObject {
    let is_macos_26 = AnyClass::get(c"NSGlassEffectView").is_some();
    let content_parent: *mut AnyObject;
    let glass: *mut AnyObject;
    if is_macos_26 {
        let glass_cls = AnyClass::get(c"NSGlassEffectView").unwrap();
        let view: *mut AnyObject = msg_send![glass_cls, alloc];
        glass = msg_send![
            view,
            initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
        ];
        let _: () = msg_send![glass, setCornerRadius: 12.0f64];
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
            let _: () = msg_send![layer, setCornerRadius: 12.0f64];
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
    } else {
        let view: *mut AnyObject = msg_send![class!(NSVisualEffectView), alloc];
        glass = msg_send![
            view,
            initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
        ];
        let _: () = msg_send![glass, setBlendingMode: 1u64]; // WithinWindow
        let _: () = msg_send![glass, setMaterial: 12u64]; // Dark
        let _: () = msg_send![glass, setState: 1u64];
        content_parent = glass;
    }
    let _: () = msg_send![glass, setAutoresizingMask: 0u64];
    let _: () = msg_send![parent, addSubview: glass];
    release_obj(glass);

    if switcher {
        let tile_w = ((w - 42.0) / 2.0).max(56.0);
        add_preview_tile(
            content_parent,
            NSRect::new(NSPoint::new(14.0, 19.0), NSSize::new(tile_w, 52.0)),
            0xFFFFFF78,
            10.0,
        );
        add_preview_tile(
            content_parent,
            NSRect::new(
                NSPoint::new(w - 14.0 - tile_w, 19.0),
                NSSize::new(tile_w, 52.0),
            ),
            0xFFFFFF90,
            10.0,
        );
    } else {
        add_preview_tile(
            content_parent,
            NSRect::new(NSPoint::new(12.0, h - 22.0), NSSize::new(w - 24.0, 10.0)),
            0xFFFFFF70,
            5.0,
        );
        add_preview_tile(
            content_parent,
            NSRect::new(NSPoint::new(12.0, 25.0), NSSize::new(w - 24.0, 12.0)),
            0xFFFFFF62,
            5.0,
        );
        add_preview_tile(
            content_parent,
            NSRect::new(NSPoint::new(12.0, 9.0), NSSize::new(w - 42.0, 8.0)),
            0xFFFFFF48,
            4.0,
        );
    }
    glass
}

pub(super) unsafe fn add_preview_tile(
    parent: *mut AnyObject,
    frame: NSRect,
    color_hex: u32,
    radius: f64,
) {
    let tile: *mut AnyObject = msg_send![class!(NSView), alloc];
    let tile: *mut AnyObject = msg_send![tile, initWithFrame: frame];
    let _: () = msg_send![tile, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![tile, layer];
    if !layer.is_null() {
        let _: () = msg_send![layer, setCornerRadius: radius];
        crate::ffi::layer_set_background(layer, crate::ffi::hex_to_cg_color(color_hex));
        crate::ffi::layer_set_border(layer, crate::ffi::hex_to_cg_color(0x0000000Au32));
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
    let font: *mut AnyObject = msg_send![class!(NSFont), messageFontOfSize: 11.0f64];
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

    // accessory 宽度必须匹配颜色面板本身;NSColorPanel 不会因 accessory 超宽而自动扩窗。
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
    // 按本地化标题使用原生固有宽度,避免全宽按钮让系统圆角比例失真。
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

/// 关闭并解绑系统取色面板,避免设置窗口销毁后面板继续改动悬空的 color well。
/// Close and detach the system color panel so it cannot mutate a dangling color well after the
/// settings window is destroyed.
pub(super) unsafe fn close_glass_tint_panel(well: *mut AnyObject) {
    // NSColorPanel.sharedColorPanel is independent from the settings window, and AppKit can
    // report the color well as inactive while the shared panel is still visible. Always hide the
    // panel; `isActive` only decides whether the well needs an additional deactivate call.
    //
    // NSColorPanel.sharedColorPanel 独立于设置窗口,而且 AppKit 可能在共享面板仍可见时把
    // color well 报告为非 active。必须无条件隐藏面板;`isActive` 只能决定是否额外停用色块。
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
    let ui = SETTINGS_UI.lock().unwrap();
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
}

/// 应用临时玻璃预览到真实浮窗和设置页内的两个模拟浮窗。
/// Apply the temporary glass preview to the real overlays and the two in-settings mock overlays.
pub(crate) fn apply_glass_preview() {
    unsafe {
        crate::overlay::apply_glass_properties();
        crate::clipboard::apply_glass_properties();
        update_settings_preview_views();
    }
}

/// 取色器/颜色面板的统一写入路径:把新颜色写进 CONFIG 并调度防抖落盘,
/// 随后把预览应用到真实浮窗与设置页内的两个模拟浮窗。
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
    if sync_well {
        if let Some(ui) = SETTINGS_UI.lock().unwrap().as_ref() {
            GLASS_UI_UPDATE.store(true, Ordering::SeqCst);
            let _: () = msg_send![ui.glass_tint, setColor: color];
            GLASS_UI_UPDATE.store(false, Ordering::SeqCst);
        }
    }
    if let Ok(mut w) = crate::config::CONFIG.write() {
        w.appearance.glass_tint = hex;
    }
    crate::config::schedule_config_persist();
    apply_glass_preview();
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
        if let Some(ui) = SETTINGS_UI.lock().unwrap().as_ref() {
            let _: () = msg_send![ui.glass_tint, setColor: color];
        }
        let panel: *mut AnyObject = msg_send![class!(NSColorPanel), sharedColorPanel];
        let _: () = msg_send![panel, setColor: color];
        GLASS_UI_UPDATE.store(false, Ordering::SeqCst);
        // 重置 = 立即写回 CONFIG 默认值(与即时生效语义一致)。
        // Reset = write the default straight back to CONFIG (matching live-apply semantics).
        if let Ok(mut w) = crate::config::CONFIG.write() {
            w.appearance.glass_tint = default_hex;
        }
        crate::config::persist_config_now();
        apply_glass_preview();
    }
}
