//! Control builders: button/text-field/switch/slider/popup custom classes plus card and row layout builders.

use super::*;

#[link(name = "AppKit", kind = "framework")]
extern "C" {
    pub(super) static NSFontAttributeName: *mut AnyObject;
    static NSKernAttributeName: *mut AnyObject;
}

/// Set a control's title and release the temporary NSString.
pub(super) unsafe fn set_control_title(obj: *mut AnyObject, title: &str) {
    let ns = make_nsstring(title);
    let _: () = msg_send![obj, setTitle: ns];
    CFRelease(ns as *const c_void);
}

pub(super) fn settings_palette() -> UiPalette {
    ui_palette()
}

/// 2pt inset accent focus ring (design-style §9/§10). CALayer draws its border inside the layer
/// bounds, which is the documented inset treatment; `focused == false` restores the idle border.
pub(super) unsafe fn apply_inset_focus_ring(
    view: *mut AnyObject,
    focused: bool,
    idle_border: u32,
    idle_width: f64,
) {
    if view.is_null() {
        return;
    }
    let layer: *mut AnyObject = msg_send![view, layer];
    if layer.is_null() {
        return;
    }
    if focused {
        crate::ffi::layer_set_border(
            layer,
            crate::ffi::hex_to_cg_color(settings_palette().accent),
        );
        let _: () = msg_send![layer, setBorderWidth: 2.0f64];
    } else {
        if idle_width > 0.0 {
            crate::ffi::layer_set_border(layer, crate::ffi::hex_to_cg_color(idle_border));
        }
        let _: () = msg_send![layer, setBorderWidth: idle_width];
    }
}

/// Shared NSTextField delegate: shows the inset accent ring while the field is being edited.
extern "C" fn settings_text_field_focus_did_begin(
    _self: *mut c_void,
    _cmd: Sel,
    notification: *mut AnyObject,
) {
    unsafe {
        let field: *mut AnyObject = msg_send![notification, object];
        apply_inset_focus_ring(field, true, 0, 0.0);
    }
}

extern "C" fn settings_text_field_focus_did_end(
    _self: *mut c_void,
    _cmd: Sel,
    notification: *mut AnyObject,
) {
    unsafe {
        let field: *mut AnyObject = msg_send![notification, object];
        apply_inset_focus_ring(field, false, 0, 0.0);
    }
}

fn settings_text_field_delegate() -> *mut AnyObject {
    static DELEGATE: OnceLock<usize> = OnceLock::new();
    *DELEGATE.get_or_init(|| unsafe {
        let name = CString::new("OhMyTabSettingsTextFieldDelegate").unwrap();
        let superclass = class!(NSObject) as *const _ as *mut AnyObject;
        let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
        let types = CString::new("v@:@").unwrap();
        class_addMethod(
            cls,
            sel!(controlTextDidBeginEditing:),
            settings_text_field_focus_did_begin as *mut c_void,
            types.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(controlTextDidEndEditing:),
            settings_text_field_focus_did_end as *mut c_void,
            types.as_ptr(),
        );
        objc_registerClassPair(cls);
        let delegate: *mut AnyObject = msg_send![cls, new];
        delegate as usize
    }) as *mut AnyObject
}

/// Return a flipped NSView class for embedded flows that use top-down child coordinates.
pub(crate) fn flipped_settings_view_class() -> *mut AnyObject {
    static CLASS: OnceLock<usize> = OnceLock::new();
    *CLASS.get_or_init(|| unsafe {
        let name = CString::new("OhMyTabFlippedSettingsView").unwrap();
        let superclass = class!(NSView) as *const _ as *mut AnyObject;
        let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
        let types = CString::new("B@:").unwrap();
        class_addMethod(
            cls,
            sel!(isFlipped),
            flipped_settings_view_is_flipped as *mut c_void,
            types.as_ptr(),
        );
        objc_registerClassPair(cls);
        cls as usize
    }) as *mut AnyObject
}

extern "C" fn flipped_settings_view_is_flipped(_this: *mut c_void, _cmd: Sel) -> bool {
    true
}

/// Semantic text roles used by every settings label and control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SettingsTextRole {
    Primary,
    Secondary,
    Sidebar,
    Muted,
    Disabled,
    Accent,
    /// Link-colored body text: external links. Text, so it carries the 4.5:1 text floor that
    /// `Accent` (3.40:1 on the light window) cannot.
    Link,
    /// Hover state of `Link`, related to it as the palette's `accent_hover` is to `accent`.
    LinkHover,
    Destructive,
}

/// Resolve one text role from the active light/dark palette.
pub(super) fn settings_text_color(role: SettingsTextRole) -> *mut AnyObject {
    let palette = settings_palette();
    let color = match role {
        SettingsTextRole::Primary => palette.primary_text,
        SettingsTextRole::Secondary => palette.secondary_text,
        SettingsTextRole::Sidebar => palette.sidebar_text,
        SettingsTextRole::Muted => palette.muted_text,
        SettingsTextRole::Disabled => palette.disabled_text,
        SettingsTextRole::Accent => palette.accent,
        SettingsTextRole::Link => palette.link_text,
        SettingsTextRole::LinkHover => palette.link_text_hover,
        SettingsTextRole::Destructive => palette.destructive,
    };
    crate::ffi::hex_to_ns_color(color)
}

/// Apply a semantic text role to a text-bearing AppKit view.
pub(super) unsafe fn apply_settings_text_role(view: *mut AnyObject, role: SettingsTextRole) {
    if view.is_null() {
        return;
    }
    let color = settings_text_color(role);
    if msg_send![view, respondsToSelector: sel!(setTextColor:)] {
        let _: () = msg_send![view, setTextColor: color];
    } else if msg_send![view, respondsToSelector: sel!(setContentTintColor:)] {
        let _: () = msg_send![view, setContentTintColor: color];
    }
}

/// Map legacy HTML reference colors to the corresponding role in the active palette. Keeping
/// this compatibility layer lets the many settings controls share one dark/light implementation
/// without changing their layout-specific call sites.
pub(super) fn themed_settings_color(hex: u32) -> u32 {
    let p = settings_palette();
    if !p.dark {
        return hex;
    }
    match hex {
        0xFFFFFFAD => p.button_bg,
        0x7676801F | 0x7676801E => p.field_bg,
        0xFFFFFFC7 => p.footer_button_bg,
        0x76768024 | 0x7676802B => p.hover_bg,
        0x0A84FFFF => p.accent,
        0x0077EDFF => p.accent_hover,
        0xFF3B30FF => p.destructive,
        0xD70015FF => p.destructive_hover,
        0xFFFFFFFF => p.accent_text,
        0x2E2E2EFF | 0x2C2C30FF | 0x44444AFF => p.button_text,
        _ => hex,
    }
}

/// Apply the HTML button surface to native NSButton instances.
pub(super) unsafe fn style_html_button(button: *mut AnyObject, background_hex: u32, text_hex: u32) {
    let _: () = msg_send![button, setBezelStyle: 0isize];
    let _: () = msg_send![button, setBordered: false];
    let _: () = msg_send![button, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![button, layer];
    if !layer.is_null() {
        let palette = settings_palette();
        layer_set_background(
            layer,
            crate::ffi::hex_to_cg_color(themed_settings_color(background_hex)),
        );
        crate::ffi::layer_set_border(layer, crate::ffi::hex_to_cg_color(palette.card_border));
        let _: () = msg_send![layer, setBorderWidth: 1.0f64];
        let _: () = msg_send![layer, setCornerRadius: crate::theme::RADIUS_CONTROL];
        let _: () = msg_send![layer, setMasksToBounds: true];
    }
    let text_color = crate::ffi::hex_to_ns_color(themed_settings_color(text_hex));
    let _: () = msg_send![button, setContentTintColor: text_color];
}

pub(super) struct HtmlActionButtonClass(*mut AnyObject);
unsafe impl Send for HtmlActionButtonClass {}
unsafe impl Sync for HtmlActionButtonClass {}

pub(super) static HTML_ACTION_BUTTON_CLASS: OnceLock<HtmlActionButtonClass> = OnceLock::new();

pub(super) fn html_action_button_class() -> *mut AnyObject {
    HTML_ACTION_BUTTON_CLASS
        .get_or_init(|| unsafe {
            let name = CString::new("OhMyTabHtmlActionButton").unwrap();
            let superclass = class!(NSButton) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(mouseEntered:),
                html_action_button_mouse_entered as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseExited:),
                html_action_button_mouse_exited as *mut c_void,
                types.as_ptr(),
            );
            // become/resignFirstResponder return BOOL ('B'), not void.
            let focus_types = CString::new("B@:").unwrap();
            class_addMethod(
                cls,
                sel!(becomeFirstResponder),
                html_action_button_become_first_responder as *mut c_void,
                focus_types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(resignFirstResponder),
                html_action_button_resign_first_responder as *mut c_void,
                focus_types.as_ptr(),
            );
            objc_registerClassPair(cls);
            HtmlActionButtonClass(cls)
        })
        .0
}

/// Resolve the fill a `SettingsButton` shows while hovered, from the palette-selecting `tag`.
///
/// `tag` carries the role's palette slot, not an action id (see `SettingsButtonRole::style`).
/// Roles with a dedicated hover token use it; the rest use the generic `hover_bg` tint, which
/// layers over whatever surface the role's normal color already provides.
pub(super) fn action_button_hover_color(tag: isize, palette: crate::theme::UiPalette) -> u32 {
    match tag {
        -2 => palette.accent_hover,
        -4 => palette.destructive_hover,
        _ => palette.hover_bg,
    }
}

/// Resolve the fill a `SettingsButton` returns to when the pointer leaves.
///
/// This must reproduce the role's own normal color exactly; `SettingsButtonRole::style` is the
/// authority for that mapping, and the test below pins the two together so a new role cannot
/// leave a button stuck on its hover fill.
pub(super) fn action_button_normal_color(tag: isize, palette: crate::theme::UiPalette) -> u32 {
    match tag {
        -2 => palette.accent,
        -1 => palette.footer_button_bg,
        -3 => palette.button_bg,
        -4 => palette.destructive,
        // `Compact` uses tag 0 as its normal, non-negative slot.
        _ if tag >= 0 => palette.field_bg,
        _ => palette.button_bg,
    }
}

pub(super) extern "C" fn html_action_button_mouse_entered(
    this: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) {
    unsafe {
        let button = this as *mut AnyObject;
        let tag: isize = msg_send![button, tag];
        let hover = action_button_hover_color(tag, settings_palette());
        let layer: *mut AnyObject = msg_send![button, layer];
        if !layer.is_null() {
            layer_set_background(
                layer,
                crate::ffi::hex_to_cg_color(themed_settings_color(hover)),
            );
        }
    }
}

pub(super) extern "C" fn html_action_button_mouse_exited(
    this: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) {
    unsafe {
        let button = this as *mut AnyObject;
        let tag: isize = msg_send![button, tag];
        let normal = action_button_normal_color(tag, settings_palette());
        let layer: *mut AnyObject = msg_send![button, layer];
        if !layer.is_null() {
            layer_set_background(
                layer,
                crate::ffi::hex_to_cg_color(themed_settings_color(normal)),
            );
        }
    }
}

extern "C" fn html_action_button_become_first_responder(this: *mut c_void, _cmd: Sel) -> bool {
    unsafe {
        type F = unsafe extern "C" fn(*mut ObjcSuper, Sel) -> bool;
        let mut sup = ObjcSuper {
            receiver: this,
            super_class: class!(NSButton) as *const _ as *mut c_void,
        };
        let send: F = std::mem::transmute(objc_msgSendSuper as *const ());
        let accepted = send(&mut sup, sel!(becomeFirstResponder));
        if accepted {
            // Idle border is the 1pt card_border; focus swaps it for the 2pt accent ring.
            apply_inset_focus_ring(
                this as *mut AnyObject,
                true,
                settings_palette().card_border,
                1.0,
            );
        }
        accepted
    }
}

extern "C" fn html_action_button_resign_first_responder(this: *mut c_void, _cmd: Sel) -> bool {
    unsafe {
        type F = unsafe extern "C" fn(*mut ObjcSuper, Sel) -> bool;
        let mut sup = ObjcSuper {
            receiver: this,
            super_class: class!(NSButton) as *const _ as *mut c_void,
        };
        let send: F = std::mem::transmute(objc_msgSendSuper as *const ());
        let accepted = send(&mut sup, sel!(resignFirstResponder));
        if accepted {
            apply_inset_focus_ring(
                this as *mut AnyObject,
                false,
                settings_palette().card_border,
                1.0,
            );
        }
        accepted
    }
}

/// Create a settings action button with a semantic normal/hover style. All buttons use the same
/// tracking area and dynamic AppKit subclass; the tag only selects the hover palette and remains
/// compatible with existing positive tags used by mapping rows.
#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn make_settings_styled_button(
    frame: NSRect,
    title: &str,
    target: *mut AnyObject,
    action: Sel,
    background_hex: u32,
    text_hex: u32,
    hover_tag: isize,
) -> *mut AnyObject {
    let button: *mut AnyObject = msg_send![html_action_button_class(), alloc];
    let button: *mut AnyObject = msg_send![button, initWithFrame: frame];
    set_control_title(button, title);
    let _: () = msg_send![button, setControlSize: 0isize]; // NSControlSizeRegular
    let font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CONTROL];
    let _: () = msg_send![button, setFont: font];
    // HTML .small-btn / footer buttons: translucent white surface with a hairline border.
    style_html_button(button, background_hex, text_hex);
    let _: () = msg_send![button, setTag: hover_tag];
    let tracking: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
    let tracking: *mut AnyObject = msg_send![
        tracking,
        initWithRect: NSRect::new(NSPoint::new(0.0, 0.0), frame.size),
        options: 0x01u64 | 0x80u64 | 0x200u64,
        owner: button,
        userInfo: std::ptr::null::<AnyObject>()
    ];
    let _: () = msg_send![button, addTrackingArea: tracking];
    release_obj(tracking);
    let _: () = msg_send![button, setTarget: target];
    let _: () = msg_send![button, setAction: action];
    button
}

/// Configure a settings button to show wrapped text, returning the height needed for at most
/// `max_lines` lines. The native cell keeps the button interaction/bezel, while a disabled child
/// label owns multiline drawing so AppKit cannot collapse the title back to one line.
pub(crate) unsafe fn configure_settings_button_wrapping(
    button: *mut AnyObject,
    width: f64,
    max_lines: usize,
) -> f64 {
    if button.is_null() {
        return 30.0;
    }
    let title: *mut AnyObject = msg_send![button, title];
    let title_utf8: *const std::ffi::c_char = msg_send![title, UTF8String];
    let title = if title_utf8.is_null() {
        String::new()
    } else {
        std::ffi::CStr::from_ptr(title_utf8)
            .to_string_lossy()
            .into_owned()
    };
    let label_w = (width - 16.0).max(1.0);
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let label: *mut AnyObject = msg_send![
        label,
        initWithFrame: NSRect::new(NSPoint::new(8.0, 0.0), NSSize::new(label_w, 36.0))
    ];
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let _: () = msg_send![label, setSelectable: false];
    let _: () = msg_send![label, setEnabled: false];
    let _: () = msg_send![label, setAlignment: 1isize]; // NSTextAlignmentCenter
    let _: () = msg_send![label, setUsesSingleLineMode: false];
    let _: () = msg_send![label, setLineBreakMode: 0isize]; // NSLineBreakByWordWrapping
    if msg_send![label, respondsToSelector: sel!(setMaximumNumberOfLines:)] {
        let _: () = msg_send![label, setMaximumNumberOfLines: max_lines.max(1) as isize];
    }
    if msg_send![label, respondsToSelector: sel!(setTruncatesLastVisibleLine:)] {
        let _: () = msg_send![label, setTruncatesLastVisibleLine: true];
    }
    let _: () = msg_send![label, setPreferredMaxLayoutWidth: label_w];
    let title_ns = make_nsstring(&title);
    let _: () = msg_send![label, setStringValue: title_ns];
    CFRelease(title_ns as *const c_void);

    let button_cell: *mut AnyObject = msg_send![button, cell];
    if !button_cell.is_null() && msg_send![button_cell, respondsToSelector: sel!(font)] {
        let font: *mut AnyObject = msg_send![button_cell, font];
        if !font.is_null() {
            let _: () = msg_send![label, setFont: font];
        }
    }
    let tint: *mut AnyObject = msg_send![button, contentTintColor];
    if !tint.is_null() {
        let _: () = msg_send![label, setTextColor: tint];
    }
    crate::ffi::set_text_field_line_height(
        label,
        crate::theme::line_height(
            crate::theme::FONT_CONTROL,
            crate::theme::LINE_HEIGHT_BODY_RATIO,
        ),
    );
    let label_cell: *mut AnyObject = msg_send![label, cell];
    if !label_cell.is_null()
        && msg_send![label_cell, respondsToSelector: sel!(setVerticalAlignment:)]
    {
        let _: () = msg_send![label_cell, setVerticalAlignment: 1isize];
    }
    let _: () = msg_send![button, addSubview: label];
    release_obj(label);
    let empty_title = make_nsstring("");
    let _: () = msg_send![button, setTitle: empty_title];
    CFRelease(empty_title as *const c_void);

    if msg_send![label, respondsToSelector: sel!(setMaximumNumberOfLines:)] {
        let _: () = msg_send![label, setMaximumNumberOfLines: 1isize];
    }
    let single_line: NSSize = msg_send![label, sizeThatFits: NSSize::new(label_w, 10_000.0)];
    if msg_send![label, respondsToSelector: sel!(setMaximumNumberOfLines:)] {
        let _: () = msg_send![label, setMaximumNumberOfLines: max_lines.max(1) as isize];
    }
    let measured: NSSize = msg_send![label, sizeThatFits: NSSize::new(label_w, 10_000.0)];
    let line_height = if single_line.height.is_finite() && single_line.height > 0.0 {
        single_line.height
    } else {
        17.0
    };
    let max_lines = max_lines.max(1) as f64;
    let max_height = (line_height * max_lines + 8.0).ceil();
    let measured_height = if measured.height.is_finite() && measured.height > 0.0 {
        (measured.height + 8.0).ceil()
    } else {
        30.0
    };
    let required_height = measured_height.clamp(30.0, max_height.max(30.0));
    center_settings_button_label_for_width(button, width, required_height);
    required_height
}

/// Center the shared multiline label inside the button's final frame.
pub(crate) unsafe fn center_settings_button_label(button: *mut AnyObject, height: f64) {
    if button.is_null() {
        return;
    }
    let bounds: NSRect = msg_send![button, bounds];
    center_settings_button_label_for_width(button, bounds.size.width, height);
}

unsafe fn center_settings_button_label_for_width(button: *mut AnyObject, width: f64, height: f64) {
    if button.is_null() {
        return;
    }
    let subviews: *mut AnyObject = msg_send![button, subviews];
    if subviews.is_null() {
        return;
    }
    let count: usize = msg_send![subviews, count];
    let label = (0..count).find_map(|index| {
        let child: *mut AnyObject = msg_send![subviews, objectAtIndex: index as isize];
        if msg_send![child, isKindOfClass: class!(NSTextField)] {
            Some(child)
        } else {
            None
        }
    });
    let Some(label) = label else { return };

    let label_w = (width - 16.0).max(1.0);
    let measured: NSSize = msg_send![label, sizeThatFits: NSSize::new(label_w, 10_000.0)];
    let text_h = if measured.height.is_finite() && measured.height > 0.0 {
        measured.height.min(height.max(1.0))
    } else {
        height.max(1.0)
    };
    let label_frame = NSRect::new(
        NSPoint::new(8.0, (height - text_h).max(0.0) / 2.0),
        NSSize::new(label_w, text_h),
    );
    let _: () = msg_send![label, setFrame: label_frame];
}

/// Rebuild the styled button's tracking area after its frame changes.
pub(crate) unsafe fn refresh_settings_button_tracking(button: *mut AnyObject) {
    if button.is_null() {
        return;
    }
    let areas: *mut AnyObject = msg_send![button, trackingAreas];
    if !areas.is_null() {
        let count: usize = msg_send![areas, count];
        for index in (0..count).rev() {
            let area: *mut AnyObject = msg_send![areas, objectAtIndex: index as isize];
            let _: () = msg_send![button, removeTrackingArea: area];
        }
    }
    let bounds: NSRect = msg_send![button, bounds];
    let tracking: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
    let tracking: *mut AnyObject = msg_send![
        tracking,
        initWithRect: bounds,
        options: 0x01u64 | 0x80u64 | 0x200u64,
        owner: button,
        userInfo: std::ptr::null::<AnyObject>()
    ];
    let _: () = msg_send![button, addTrackingArea: tracking];
    release_obj(tracking);
}

pub(super) struct ExternalLinkButtonClass(*mut AnyObject);
unsafe impl Send for ExternalLinkButtonClass {}
unsafe impl Sync for ExternalLinkButtonClass {}

pub(super) static EXTERNAL_LINK_BUTTON_CLASS: OnceLock<ExternalLinkButtonClass> = OnceLock::new();
pub(super) static SIDEBAR_BUTTON_CLASS: OnceLock<SidebarButtonClass> = OnceLock::new();
pub(super) static SIDEBAR_SELECTED: AtomicUsize = AtomicUsize::new(0);
pub(super) static SIDEBAR_HOVERED: AtomicUsize = AtomicUsize::new(0);
pub(super) static SIDEBAR_TITLE_LABELS: LazyLock<MainThreadSlot<HashMap<usize, ObjPtr>>> =
    LazyLock::new(|| MainThreadSlot::new(HashMap::new()));
pub(super) static SIDEBAR_ICON_VIEWS: LazyLock<MainThreadSlot<HashMap<usize, ObjPtr>>> =
    LazyLock::new(|| MainThreadSlot::new(HashMap::new()));
pub(super) static SIDEBAR_UPDATE_DOTS: LazyLock<MainThreadSlot<HashMap<usize, ObjPtr>>> =
    LazyLock::new(|| MainThreadSlot::new(HashMap::new()));

/// Forget the hovered row WITHOUT messaging it.
///
/// `clear_sidebar_hover` deliberately resets the fill on the stored button, which is only sound
/// while that view is alive. Here the view is being replaced, so that message is the very thing we
/// must not send; dropping the address is the only safe action, and the old view is destroyed
/// moments later with nothing left painted.
///
/// `SIDEBAR_HOVERED` is read back out as a raw pointer by `apply_sidebar_hover` (to un-hover the
/// previously hovered row) and by `clear_sidebar_hover`, so leaving a freed address in it turns the
/// next sidebar mouse-enter or mouse-exit into a message to a deallocated object.
pub(super) fn forget_sidebar_hover() {
    SIDEBAR_HOVERED.store(0, Ordering::SeqCst);
}

/// Drop every sidebar registry that caches a raw view address, plus the hovered-row pointer.
///
/// Call this before (re)building the sidebar and before the content hierarchy is released. The maps
/// hold `usize` addresses of the label, icon and dot views with no retain of their own, so once a
/// rebuild releases those views the keys are dangling; the next hover hit-test would message freed
/// memory. `SIDEBAR_TITLE_LABELS` is additionally *iterated as the list of sidebar buttons* by
/// `sidebar_button_under_pointer`, so a stale key there is dereferenced directly.
pub(super) fn clear_sidebar_view_registries() {
    forget_sidebar_hover();
    SIDEBAR_TITLE_LABELS.lock().unwrap().clear();
    SIDEBAR_ICON_VIEWS.lock().unwrap().clear();
    SIDEBAR_UPDATE_DOTS.lock().unwrap().clear();
}

pub(super) extern "C" fn external_link_mouse_entered(
    this: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) {
    unsafe {
        let color = settings_text_color(SettingsTextRole::LinkHover);
        let _: () = msg_send![this as *mut AnyObject, setTextColor: color];
        let cursor: *mut AnyObject = msg_send![class!(NSCursor), pointingHandCursor];
        let _: () = msg_send![cursor, set];
    }
}

pub(super) extern "C" fn external_link_mouse_exited(
    this: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) {
    unsafe {
        let color = settings_text_color(SettingsTextRole::Link);
        let _: () = msg_send![this as *mut AnyObject, setTextColor: color];
        let cursor: *mut AnyObject = msg_send![class!(NSCursor), arrowCursor];
        let _: () = msg_send![cursor, set];
    }
}

pub(super) extern "C" fn external_link_mouse_down(
    this: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) {
    unsafe {
        let tag: isize = msg_send![this as *mut AnyObject, tag];
        if tag == 1 {
            handle_open_github(
                std::ptr::null_mut(),
                sel!(handleOpenGithub:),
                std::ptr::null_mut(),
            );
        } else {
            handle_open_official_website(
                std::ptr::null_mut(),
                sel!(handleOpenOfficialWebsite:),
                std::ptr::null_mut(),
            );
        }
    }
}

pub(super) struct SidebarButtonClass(*mut AnyObject);
unsafe impl Send for SidebarButtonClass {}
unsafe impl Sync for SidebarButtonClass {}

pub(super) struct SidebarHoverTrackerClass(*mut AnyObject);
unsafe impl Send for SidebarHoverTrackerClass {}
unsafe impl Sync for SidebarHoverTrackerClass {}

pub(super) static SIDEBAR_HOVER_TRACKER_CLASS: OnceLock<SidebarHoverTrackerClass> = OnceLock::new();
pub(super) static SIDEBAR_HOVER_TRACKER: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);

/// Clear the active row fill when the settings window hides or changes pages.
pub(super) unsafe fn clear_sidebar_hover() {
    let hovered = SIDEBAR_HOVERED.swap(0, Ordering::SeqCst) as *mut AnyObject;
    if !hovered.is_null() {
        set_sidebar_hovered(hovered, false);
    }
}

/// Find the visible sidebar button currently under the pointer instead of trusting a possibly
/// delayed tracking-area callback.
unsafe fn sidebar_button_under_pointer() -> Option<*mut AnyObject> {
    let buttons: Vec<*mut AnyObject> = SIDEBAR_TITLE_LABELS
        .lock()
        .unwrap()
        .keys()
        .copied()
        .map(|button| button as *mut AnyObject)
        .collect();

    buttons.into_iter().find(|&button| {
        let window: *mut AnyObject = msg_send![button, window];
        if window.is_null() {
            return false;
        }
        let visible: bool = msg_send![window, isVisible];
        let hidden: bool = msg_send![button, isHidden];
        if !visible || hidden {
            return false;
        }
        let mouse: NSPoint = msg_send![window, mouseLocationOutsideOfEventStream];
        let local: NSPoint = msg_send![
            button,
            convertPoint: mouse,
            fromView: std::ptr::null::<AnyObject>()
        ];
        let bounds: NSRect = msg_send![button, bounds];
        local.x >= bounds.origin.x
            && local.x <= bounds.origin.x + bounds.size.width
            && local.y >= bounds.origin.y
            && local.y <= bounds.origin.y + bounds.size.height
    })
}

/// Apply the semantic row fill and foreground to one concrete sidebar button.
unsafe fn apply_sidebar_hover(button: *mut AnyObject) {
    if button.is_null() {
        return;
    }
    let tag: isize = msg_send![button, tag];
    if tag >= 0 && tag as usize == SIDEBAR_SELECTED.load(Ordering::SeqCst) {
        clear_sidebar_hover();
        set_sidebar_hovered(button, false);
        return;
    }
    let previous = SIDEBAR_HOVERED.swap(button as usize, Ordering::SeqCst) as *mut AnyObject;
    if !previous.is_null() && previous != button {
        set_sidebar_hovered(previous, false);
    }
    set_sidebar_hovered(button, true);
}

pub(super) extern "C" fn sidebar_hover_tracker_mouse_entered(
    _this: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) {
    unsafe {
        // Re-entering from the detail pane can skip a child button's mouseEntered callback. Use
        // the current pointer location to restore the actual row instead of the stale last row.
        if let Some(button) = sidebar_button_under_pointer() {
            apply_sidebar_hover(button);
        } else {
            clear_sidebar_hover();
        }
    }
}

pub(super) extern "C" fn sidebar_hover_tracker_mouse_exited(
    _this: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) {
    unsafe {
        clear_sidebar_hover();
    }
}

pub(super) fn sidebar_hover_tracker_class() -> *mut AnyObject {
    SIDEBAR_HOVER_TRACKER_CLASS
        .get_or_init(|| unsafe {
            let name = CString::new("OhMyTabSidebarHoverTracker").unwrap();
            let superclass = class!(NSObject) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(mouseEntered:),
                sidebar_hover_tracker_mouse_entered as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseExited:),
                sidebar_hover_tracker_mouse_exited as *mut c_void,
                types.as_ptr(),
            );
            objc_registerClassPair(cls);
            SidebarHoverTrackerClass(cls)
        })
        .0
}

/// Measure one shared row height for all localized sidebar titles.
pub(super) unsafe fn settings_sidebar_required_row_height(width: f64, titles: &[String]) -> f64 {
    let label_width = (width - 48.0 - 8.0).max(1.0);
    let field: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let field: *mut AnyObject = msg_send![
        field,
        initWithFrame: NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(label_width, 38.0),
        )
    ];
    let _: () = msg_send![field, setBezeled: false];
    let _: () = msg_send![field, setDrawsBackground: false];
    let _: () = msg_send![field, setEditable: false];
    let _: () = msg_send![field, setSelectable: false];
    let _: () = msg_send![field, setUsesSingleLineMode: false];
    let _: () = msg_send![field, setLineBreakMode: 0isize]; // NSLineBreakByWordWrapping
    if msg_send![field, respondsToSelector: sel!(setMaximumNumberOfLines:)] {
        let _: () = msg_send![field, setMaximumNumberOfLines: 2isize];
    }
    let _: () = msg_send![field, setPreferredMaxLayoutWidth: label_width];

    let mut required_height = 40.0f64;
    for title in titles {
        let title_ns = make_nsstring(title);
        let _: () = msg_send![field, setStringValue: title_ns];
        CFRelease(title_ns as *const c_void);
        let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CONTROL, weight: crate::theme::FONT_WEIGHT_REGULAR];
        let _: () = msg_send![field, setFont: font];
        let measured: NSSize = msg_send![field, sizeThatFits: NSSize::new(label_width, 10_000.0)];
        if measured.height.is_finite() && measured.height > 0.0 {
            // Keep a little vertical breathing room around two wrapped lines, while the
            // 38pt floor preserves the existing one-line sidebar rhythm.
            required_height = required_height.max((measured.height + 8.0).ceil());
        }
    }
    release_obj(field);
    required_height
}

/// Measure the full, wrapped text through TextKit using the label's actual attributed paragraph
/// styles, so the layout smoke catches both clipping and a missing line-height attribute.
pub(crate) unsafe fn full_wrapped_text_size(label: *mut AnyObject, width: f64) -> Option<NSSize> {
    if label.is_null() {
        return None;
    }
    let attributed: *mut AnyObject = msg_send![label, attributedStringValue];
    let (used, fragments) = textkit_line_fragments(attributed, width)?;
    // A multiline role may use a short localized string that occupies only one line. The style
    // guide assigns its larger line box to wrapped text; single rendered lines keep their native
    // frame and are not failures of the multiline height check.
    if fragments.len() <= 1 {
        return None;
    }
    (used.size.width.is_finite() && used.size.height.is_finite()).then_some(used.size)
}

unsafe fn textkit_line_fragments(
    attributed: *mut AnyObject,
    width: f64,
) -> Option<(NSRect, Vec<NSRect>)> {
    if attributed.is_null() {
        return None;
    }
    let text_length: usize = msg_send![attributed, length];
    if text_length == 0 {
        return None;
    }
    let storage: *mut AnyObject = msg_send![class!(NSTextStorage), alloc];
    let storage: *mut AnyObject = msg_send![storage, initWithAttributedString: attributed];
    let layout: *mut AnyObject = msg_send![class!(NSLayoutManager), alloc];
    let layout: *mut AnyObject = msg_send![layout, init];
    let container: *mut AnyObject = msg_send![class!(NSTextContainer), alloc];
    let container: *mut AnyObject = msg_send![
        container,
        initWithContainerSize: NSSize::new(width.max(1.0), 100_000.0)
    ];
    let _: () = msg_send![container, setLineFragmentPadding: 0.0f64];
    let _: () = msg_send![storage, addLayoutManager: layout];
    let _: () = msg_send![layout, addTextContainer: container];
    let _: () = msg_send![layout, ensureLayoutForTextContainer: container];
    let used: NSRect = msg_send![layout, usedRectForTextContainer: container];
    let glyph_count: usize = msg_send![layout, numberOfGlyphs];
    let mut fragments = Vec::new();
    let mut glyph_index = 0usize;
    while glyph_index < glyph_count {
        let mut glyph_range = NSRange::new(0, 0);
        let fragment: NSRect = msg_send![
            layout,
            lineFragmentRectForGlyphAtIndex: glyph_index,
            effectiveRange: &mut glyph_range as *mut NSRange
        ];
        if glyph_range.length == 0 {
            break;
        }
        fragments.push(fragment);
        glyph_index = glyph_range.location + glyph_range.length;
    }
    release_obj(container);
    release_obj(layout);
    release_obj(storage);
    Some((used, fragments))
}

/// Exercise the same NSTextField attributed-string path used by wrapping UI without opening a
/// window or taking keyboard focus. The settings and update-prompt smoke runners share this gate.
pub(crate) unsafe fn smoke_text_field_line_fragments() -> bool {
    crate::debug_assert_main_thread();
    let text = make_nsstring("中文 wrapping line 中文 wrapping line");
    let paragraph_key = make_nsstring("NSParagraphStyle");
    let mut ok = true;
    for (size, ratio) in [
        (
            crate::theme::FONT_CAPTION,
            crate::theme::LINE_HEIGHT_CAPTION_RATIO,
        ),
        (
            crate::theme::FONT_CONTROL,
            crate::theme::LINE_HEIGHT_BODY_RATIO,
        ),
        (
            crate::theme::FONT_PAGE_TITLE,
            crate::theme::LINE_HEIGHT_TITLE_RATIO,
        ),
    ] {
        let field: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let field: *mut AnyObject = msg_send![
            field,
            initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(90.0, 100.0))
        ];
        let _: () = msg_send![field, setStringValue: text];
        let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: size];
        let _: () = msg_send![field, setFont: font];
        let _: () = msg_send![field, setUsesSingleLineMode: false];
        let _: () = msg_send![field, setLineBreakMode: 0isize];
        let _: () = msg_send![field, setMaximumNumberOfLines: 0isize];
        crate::ffi::set_text_field_line_height(field, crate::theme::line_height(size, ratio));

        let attributed: *mut AnyObject = msg_send![field, attributedStringValue];
        let mut attribute_range = NSRange::new(0, 0);
        let style: *mut AnyObject = msg_send![
            attributed,
            attribute: paragraph_key,
            atIndex: 0usize,
            effectiveRange: &mut attribute_range as *mut NSRange
        ];
        let expected = crate::theme::line_height(size, ratio);
        let actual_min = if style.is_null() {
            f64::NAN
        } else {
            msg_send![style, minimumLineHeight]
        };
        let actual_max = if style.is_null() {
            f64::NAN
        } else {
            msg_send![style, maximumLineHeight]
        };
        let Some((_, fragments)) = textkit_line_fragments(attributed, 90.0) else {
            log_info!("[line-height-smoke] no TextKit fragments for {size:.1}pt field");
            ok = false;
            release_obj(field);
            continue;
        };
        if fragments.len() < 2
            || (actual_min - expected).abs() > 0.1
            || (actual_max - expected).abs() > 0.1
            || fragments
                .iter()
                .any(|fragment| (fragment.size.height - expected).abs() > 0.1)
        {
            log_info!(
                "[line-height-smoke] field={size:.1}pt expected={expected:.2}, paragraph={actual_min:.2}/{actual_max:.2}, fragments={:?}",
                fragments.iter().map(|r| r.size.height).collect::<Vec<_>>()
            );
            ok = false;
        }
        release_obj(field);
    }

    // Mirror the sidebar lifecycle: the label starts empty, receives its title through the
    // appearance setter, then is restyled when hover changes its foreground color.
    let sidebar_title = "侧边栏标题 Sidebar title wrapping check";
    let sidebar_field: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let sidebar_field: *mut AnyObject = msg_send![
        sidebar_field,
        initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(90.0, 80.0))
    ];
    let _: () = msg_send![sidebar_field, setUsesSingleLineMode: false];
    let _: () = msg_send![sidebar_field, setLineBreakMode: 0isize];
    let _: () = msg_send![sidebar_field, setMaximumNumberOfLines: 2isize];
    if msg_send![sidebar_field, respondsToSelector: sel!(setTruncatesLastVisibleLine:)] {
        let _: () = msg_send![sidebar_field, setTruncatesLastVisibleLine: true];
    }
    let sidebar_font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CONTROL];
    let mut sidebar_ok = true;
    for color in [
        settings_text_color(SettingsTextRole::Sidebar),
        settings_text_color(SettingsTextRole::Primary),
    ] {
        set_sidebar_title_appearance(sidebar_field, sidebar_title, sidebar_font, color);
        let attributed: *mut AnyObject = msg_send![sidebar_field, attributedStringValue];
        let mut attribute_range = NSRange::new(0, 0);
        let style: *mut AnyObject = msg_send![
            attributed,
            attribute: paragraph_key,
            atIndex: 0usize,
            effectiveRange: &mut attribute_range as *mut NSRange
        ];
        let expected = crate::theme::line_height(
            crate::theme::FONT_CONTROL,
            crate::theme::LINE_HEIGHT_BODY_RATIO,
        );
        let actual_min = if style.is_null() {
            f64::NAN
        } else {
            msg_send![style, minimumLineHeight]
        };
        let actual_max = if style.is_null() {
            f64::NAN
        } else {
            msg_send![style, maximumLineHeight]
        };
        let fragments = textkit_line_fragments(attributed, 90.0);
        let fits = fragments.is_some_and(|(_, fragments)| {
            fragments.len() >= 2
                && fragments
                    .iter()
                    .all(|fragment| (fragment.size.height - expected).abs() <= 0.1)
        });
        if (actual_min - expected).abs() > 0.1 || (actual_max - expected).abs() > 0.1 || !fits {
            log_info!(
                "[line-height-smoke] sidebar title lost paragraph style after appearance update: paragraph={actual_min:.2}/{actual_max:.2}, expected={expected:.2}, wraps={fits}"
            );
            sidebar_ok = false;
        }
    }
    if !sidebar_ok {
        ok = false;
    }
    release_obj(sidebar_field);
    CFRelease(paragraph_key as *const c_void);
    CFRelease(text as *const c_void);
    ok
}

/// The tracker rect arrives precomputed against the live entry count (see
/// sidebar_tracking_rect in the component layer), so it can never drift behind the
/// sidebar's rows again -- the previous hardcoded 6-row rect left the 7th entry
/// outside the tracker, leaving a row's hover fill active after the pointer left.
pub(super) unsafe fn make_sidebar_hover_tracking(parent: *mut AnyObject, rect: NSRect) {
    let tracker: *mut AnyObject = msg_send![sidebar_hover_tracker_class(), alloc];
    let tracker: *mut AnyObject = msg_send![tracker, init];
    let tracking: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
    let tracking: *mut AnyObject = msg_send![
        tracking,
        initWithRect: rect,
        options: 0x01u64 | 0x80u64,
        owner: tracker,
        userInfo: std::ptr::null::<AnyObject>()
    ];
    let _: () = msg_send![parent, addTrackingArea: tracking];
    release_obj(tracking);

    // NSTrackingArea does not provide ownership suitable for this raw-pointer registry; keep one
    // explicit +1 until the next sidebar is built, then release the previous tracker.
    if let Some(previous) = SIDEBAR_HOVER_TRACKER
        .lock()
        .unwrap()
        .replace(ObjPtr::new(tracker))
    {
        release_obj(previous.0);
    }
}

pub(super) extern "C" fn sidebar_button_mouse_entered(
    this: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) {
    unsafe {
        let button = this as *mut AnyObject;
        // Tracking callbacks may arrive after the pointer has moved into another pane or row.
        let current = sidebar_button_under_pointer();
        if current != Some(button) {
            if let Some(current) = current {
                apply_sidebar_hover(current);
            } else {
                clear_sidebar_hover();
            }
            return;
        }
        apply_sidebar_hover(button);
    }
}

pub(super) extern "C" fn sidebar_button_mouse_exited(
    this: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) {
    unsafe {
        let button = this as *mut AnyObject;
        let tag: isize = msg_send![button, tag];
        if tag >= 0 && tag as usize == SIDEBAR_SELECTED.load(Ordering::SeqCst) {
            return;
        }
        set_sidebar_hovered(button, false);
        SIDEBAR_HOVERED
            .compare_exchange(button as usize, 0, Ordering::SeqCst, Ordering::SeqCst)
            .ok();
    }
}

pub(super) fn sidebar_button_class() -> *mut AnyObject {
    SIDEBAR_BUTTON_CLASS
        .get_or_init(|| unsafe {
            let name = CString::new("OhMyTabSidebarButton").unwrap();
            let superclass = class!(NSButton) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(mouseEntered:),
                sidebar_button_mouse_entered as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseExited:),
                sidebar_button_mouse_exited as *mut c_void,
                types.as_ptr(),
            );
            objc_registerClassPair(cls);
            SidebarButtonClass(cls)
        })
        .0
}

pub(super) fn external_link_button_class() -> *mut AnyObject {
    EXTERNAL_LINK_BUTTON_CLASS
        .get_or_init(|| unsafe {
            let name = CString::new("OhMyTabExternalLinkButton").unwrap();
            let superclass = class!(NSTextField) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(mouseDown:),
                external_link_mouse_down as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseEntered:),
                external_link_mouse_entered as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseExited:),
                external_link_mouse_exited as *mut c_void,
                types.as_ptr(),
            );
            objc_registerClassPair(cls);
            ExternalLinkButtonClass(cls)
        })
        .0
}

/// Build a read-only value label for a standard settings row.
pub(super) unsafe fn make_value_label(
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    value: &str,
) -> *mut AnyObject {
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    // Height is whatever the caller asks for: a slider readout is only 18pt, so no full-control
    // minimum is enforced here.
    let label: *mut AnyObject =
        msg_send![label, initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))];
    set_field(label, value);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let _: () = msg_send![label, setSelectable: false];
    let _: () = msg_send![label, setUsesSingleLineMode: true];
    let _: () = msg_send![label, setLineBreakMode: 4isize]; // NSLineBreakByTruncatingTail
    let _: () = msg_send![label, setAlignment: -1isize]; // NSTextAlignmentNatural
    if !value.is_empty() {
        let tooltip = make_nsstring(value);
        let _: () = msg_send![label, setToolTip: tooltip];
        CFRelease(tooltip as *const c_void);
    }
    let font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CONTROL];
    let _: () = msg_send![label, setFont: font];
    apply_settings_text_role(label, SettingsTextRole::Primary);
    label
}

/// Set a page heading with the tracking specified by the page-title type role.
pub(super) unsafe fn set_page_title_text(label: *mut AnyObject, text: &str) {
    let font: *mut AnyObject =
        msg_send![class!(NSFont), boldSystemFontOfSize: crate::theme::FONT_PAGE_TITLE];
    let tracking: *mut AnyObject = msg_send![class!(NSNumber), numberWithDouble: -0.4f64];
    let attributes: *mut AnyObject = msg_send![class!(NSMutableDictionary), alloc];
    let attributes: *mut AnyObject = msg_send![attributes, init];
    let paragraph: *mut AnyObject = msg_send![class!(NSMutableParagraphStyle), alloc];
    let paragraph: *mut AnyObject = msg_send![paragraph, init];
    let line_height = crate::theme::line_height(
        crate::theme::FONT_PAGE_TITLE,
        crate::theme::LINE_HEIGHT_TITLE_RATIO,
    );
    let _: () = msg_send![paragraph, setMinimumLineHeight: line_height];
    let _: () = msg_send![paragraph, setMaximumLineHeight: line_height];
    let _: () = msg_send![attributes, setObject: font, forKey: NSFontAttributeName];
    let _: () = msg_send![attributes, setObject: tracking, forKey: NSKernAttributeName];
    let paragraph_key = make_nsstring("NSParagraphStyle");
    let _: () = msg_send![attributes, setObject: paragraph, forKey: paragraph_key];
    CFRelease(paragraph_key as *const c_void);
    let value = make_nsstring(text);
    let attributed: *mut AnyObject = msg_send![class!(NSAttributedString), alloc];
    let attributed: *mut AnyObject =
        msg_send![attributed, initWithString: value, attributes: attributes];
    CFRelease(value as *const c_void);
    release_obj(attributes);
    release_obj(paragraph);
    let _: () = msg_send![label, setAttributedStringValue: attributed];
    release_obj(attributed);
}

/// Build a read-only external-link control for a standard settings row.
pub(super) unsafe fn make_external_link(
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    title: &str,
    tag: isize,
) -> *mut AnyObject {
    let link: *mut AnyObject = msg_send![external_link_button_class(), alloc];
    let link: *mut AnyObject = msg_send![
        link,
        initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    ];
    set_field(link, title);
    let _: () = msg_send![link, setTag: tag];
    let _: () = msg_send![link, setBezeled: false];
    let _: () = msg_send![link, setDrawsBackground: false];
    let _: () = msg_send![link, setEditable: false];
    let _: () = msg_send![link, setSelectable: false];
    let _: () = msg_send![link, setAlignment: -1isize]; // NSTextAlignmentNatural
    let _: () = msg_send![link, setUsesSingleLineMode: true];
    let _: () = msg_send![link, setLineBreakMode: 4isize]; // NSLineBreakByTruncatingTail
    let font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CONTROL];
    let _: () = msg_send![link, setFont: font];
    apply_settings_text_role(link, SettingsTextRole::Link);
    let tracking: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
    let tracking: *mut AnyObject = msg_send![
        tracking,
        initWithRect: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h)),
        options: 0x01u64 | 0x80u64 | 0x200u64,
        owner: link,
        userInfo: std::ptr::null::<AnyObject>()
    ];
    let _: () = msg_send![link, addTrackingArea: tracking];
    release_obj(tracking);
    link
}

/// Create a system-symbol image with an explicit size shared by menu items and row views.
pub(super) unsafe fn make_symbol_image(symbol: &str, size: NSSize) -> *mut AnyObject {
    let symbol_ns = make_nsstring(symbol);
    let image: *mut AnyObject = msg_send![
        class!(NSImage),
        imageWithSystemSymbolName: symbol_ns,
        accessibilityDescription: std::ptr::null::<AnyObject>()
    ];
    CFRelease(symbol_ns as *const c_void);
    if !image.is_null() {
        let _: () = msg_send![image, setSize: size];
    }
    image
}

/// Create an image view for a system symbol used inside a settings row.
pub(super) unsafe fn make_symbol_image_view(symbol: &str, frame: NSRect) -> *mut AnyObject {
    let image = make_symbol_image(symbol, frame.size);
    let icon_view: *mut AnyObject = msg_send![class!(NSImageView), alloc];
    let icon_view: *mut AnyObject = msg_send![icon_view, initWithFrame: frame];
    if !image.is_null() {
        let _: () = msg_send![icon_view, setImage: image];
    }
    let _: () = msg_send![icon_view, setImageScaling: 3isize];
    let _: () = msg_send![icon_view, setEditable: false];
    let _: () = msg_send![icon_view, setWantsLayer: false];
    let tint = crate::ffi::hex_to_ns_color(settings_palette().primary_text);
    let _: () = msg_send![icon_view, setContentTintColor: tint];
    icon_view
}

pub(super) struct AboutHeaderClickViewClass(*mut AnyObject);
unsafe impl Send for AboutHeaderClickViewClass {}
unsafe impl Sync for AboutHeaderClickViewClass {}

pub(super) static ABOUT_HEADER_CLICK_VIEW_CLASS: OnceLock<AboutHeaderClickViewClass> =
    OnceLock::new();

// bundle_info_string now lives in ffi.rs

pub(super) fn about_header_click_view_class() -> *mut AnyObject {
    ABOUT_HEADER_CLICK_VIEW_CLASS
        .get_or_init(|| unsafe {
            let name = CString::new("OhMyTabAboutHeaderClickView").unwrap();
            let superclass = class!(NSView) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(mouseDown:),
                about_header_click_view_mouse_down as *mut c_void,
                types.as_ptr(),
            );
            objc_registerClassPair(cls);
            AboutHeaderClickViewClass(cls)
        })
        .0
}

/// Count clicks on the About header and reveal the bundle build number after five consecutive clicks.
pub(crate) extern "C" fn on_about_header_click(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    let now = Instant::now();
    let revealed = {
        let mut clicks = ABOUT_HEADER_CLICKS.lock().unwrap();
        let within_window = clicks
            .1
            .is_some_and(|last| now.duration_since(last) <= ABOUT_HEADER_CLICK_WINDOW);
        clicks.0 = if within_window {
            clicks.0.saturating_add(1)
        } else {
            1
        };
        clicks.1 = Some(now);
        if clicks.0 >= 5 {
            clicks.0 = 0;
            clicks.1 = None;
            true
        } else {
            false
        }
    };

    if !revealed {
        return;
    }

    unsafe {
        let build_version = bundle_info_string("CFBundleVersion");
        if build_version.is_empty() {
            return;
        }
        super::with_settings_ui(|ui| {
            let Some(ui) = ui.as_ref() else {
                return;
            };
            set_field(
                ui.about_subtitle,
                tf(
                    "settings.version_label_with_build",
                    &[
                        ("version", env!("CARGO_PKG_VERSION")),
                        ("build", &build_version),
                    ],
                ),
            );
        });
    }
}

pub(super) extern "C" fn about_header_click_view_mouse_down(
    _self: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) {
    on_about_header_click(std::ptr::null_mut(), sel!(mouseDown:), std::ptr::null_mut());
}

/// Set a text field's value from anything Displayable, releasing the temp NSString.
pub(super) unsafe fn set_field(field: *mut AnyObject, val: impl std::fmt::Display) {
    let s = format!("{}", val);
    let ns = make_nsstring(&s);
    let _: () = msg_send![field, setStringValue: ns];
    CFRelease(ns as *const c_void);
}

/// NSTextFieldCell keeps a fixed baseline for single-line controls. Our settings rows are taller
/// than that standard control height, so use a small cell subclass that gives AppKit a centered
/// 22pt drawing rect inside the full row. The field editor must still receive AppKit's original
/// bounding rect: `selectWithFrame:` is also used for double-click word selection, and passing the
/// compact drawing rect makes the editor jump toward the cell's upper-left corner.
pub(super) unsafe fn centered_text_field_cell_class() -> *mut AnyObject {
    static CELL_CLASS: OnceLock<StaticClass> = OnceLock::new();
    CELL_CLASS
        .get_or_init(|| {
            let name = CString::new("OhMyTabCenteredTextFieldCell").unwrap();
            let superclass = class!(NSTextFieldCell) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let draw_types = CString::new("v@:{CGRect=dddd}@").unwrap();
            class_addMethod(
                cls,
                sel!(drawInteriorWithFrame:inView:),
                centered_text_field_cell_draw_interior as *mut c_void,
                draw_types.as_ptr(),
            );
            // The selector has exactly three object parameters: view, editor, and delegate.
            let select_types = CString::new("v@:{CGRect=dddd}@@@qq").unwrap();
            class_addMethod(
                cls,
                sel!(selectWithFrame:inView:editor:delegate:start:length:),
                centered_text_field_cell_select as *mut c_void,
                select_types.as_ptr(),
            );
            // AppKit returns the configured NSText; its type encoding must match the IMP ABI.
            let editor_types = CString::new("@@:@").unwrap();
            class_addMethod(
                cls,
                sel!(setUpFieldEditorAttributes:),
                centered_text_field_cell_setup_editor as *mut c_void,
                editor_types.as_ptr(),
            );
            objc_registerClassPair(cls);
            StaticClass(cls as *const objc2::runtime::AnyClass)
        })
        .0 as *mut AnyObject
}

pub(super) fn centered_text_field_cell_frame(bounds: NSRect) -> NSRect {
    let text_h = bounds.size.height.min(22.0);
    // The cell draws its baseline a few points above the geometric center of the 22pt rect, so
    // after centering the rect in the taller settings row a POSITIVE offset shifts the drawing
    // rect DOWN to compensate (the old +1 left the glyphs a touch high; a negative value pushed
    // them further up).
    let baseline_offset = if bounds.size.height > text_h {
        2.0
    } else {
        0.0
    };
    let horizontal_inset = bounds.size.width.min(8.0);
    NSRect::new(
        NSPoint::new(
            bounds.origin.x + horizontal_inset,
            bounds.origin.y + (bounds.size.height - text_h) / 2.0 + baseline_offset,
        ),
        NSSize::new(
            (bounds.size.width - horizontal_inset * 2.0).max(1.0),
            text_h,
        ),
    )
}

fn centered_text_field_editor_vertical_inset(bounds: NSRect) -> f64 {
    centered_text_field_cell_frame(bounds).origin.y - bounds.origin.y
}

pub(super) unsafe fn centered_text_field_cell_super_draw(
    cell: *mut c_void,
    rect: NSRect,
    view: *mut c_void,
) {
    type F = unsafe extern "C" fn(*mut ObjcSuper, Sel, NSRect, *mut c_void) -> ();
    let super_class =
        objc2::runtime::AnyClass::get(c"NSTextFieldCell").unwrap() as *const _ as *mut c_void;
    let mut sup = ObjcSuper {
        receiver: cell,
        super_class,
    };
    let send: F = std::mem::transmute(objc_msgSendSuper as *const ());
    send(&mut sup, sel!(drawInteriorWithFrame:inView:), rect, view);
}

pub(super) extern "C" fn centered_text_field_cell_draw_interior(
    this: *mut c_void,
    _cmd: Sel,
    bounds: NSRect,
    view: *mut c_void,
) {
    unsafe {
        centered_text_field_cell_super_draw(this, centered_text_field_cell_frame(bounds), view);
    }
}

pub(super) extern "C" fn centered_text_field_cell_select(
    this: *mut c_void,
    _cmd: Sel,
    bounds: NSRect,
    view: *mut c_void,
    editor: *mut c_void,
    delegate: *mut c_void,
    start: isize,
    length: isize,
) {
    unsafe {
        type F = unsafe extern "C" fn(
            *mut ObjcSuper,
            Sel,
            NSRect,
            *mut c_void,
            *mut c_void,
            *mut c_void,
            isize,
            isize,
        ) -> ();
        let super_class =
            objc2::runtime::AnyClass::get(c"NSTextFieldCell").unwrap() as *const _ as *mut c_void;
        let mut sup = ObjcSuper {
            receiver: this,
            super_class,
        };
        let send: F = std::mem::transmute(objc_msgSendSuper as *const ());
        send(
            &mut sup,
            sel!(selectWithFrame:inView:editor:delegate:start:length:),
            // `bounds` is the cell's full bounding rectangle. Do not pass the compact drawing
            // rect here: AppKit reuses this method for double-click selection and positions the
            // field editor from the rectangle it receives.
            bounds,
            view,
            editor,
            delegate,
            start,
            length,
        );
    }
}

/// Keep AppKit's field editor aligned with the cell's normal drawing baseline. The editor is an
/// NSTextView and otherwise draws a single-line value from its own top-left origin, which is most
/// visible after a double-click when AppKit reuses the editor for word selection.
pub(super) extern "C" fn centered_text_field_cell_setup_editor(
    this: *mut c_void,
    _cmd: Sel,
    editor: *mut c_void,
) -> *mut c_void {
    unsafe {
        type F = unsafe extern "C" fn(*mut ObjcSuper, Sel, *mut c_void) -> *mut c_void;
        let super_class =
            objc2::runtime::AnyClass::get(c"NSTextFieldCell").unwrap() as *const _ as *mut c_void;
        let mut sup = ObjcSuper {
            receiver: this,
            super_class,
        };
        let send: F = std::mem::transmute(objc_msgSendSuper as *const ());
        let configured_editor = send(&mut sup, sel!(setUpFieldEditorAttributes:), editor);

        let editor = configured_editor as *mut AnyObject;
        if editor.is_null() {
            return configured_editor;
        }
        let _: () = msg_send![editor, setAlignment: 0isize]; // NSTextAlignmentLeft
        let _: () = msg_send![editor, setVerticallyResizable: false];
        let _: () = msg_send![editor, setHorizontallyResizable: true];
        if msg_send![editor, respondsToSelector: sel!(setTextContainerInset:)] {
            // Match the editor's vertical inset to the idle cell's draw-rect origin so changing a
            // control's height cannot shift its text between display and editing states.
            let control_view: *mut AnyObject = msg_send![this as *mut AnyObject, controlView];
            let bounds = if control_view.is_null() {
                NSRect::new(
                    NSPoint::new(0.0, 0.0),
                    NSSize::new(1.0, super::components::SettingsLayout::CONTROL_H),
                )
            } else {
                msg_send![control_view, bounds]
            };
            let vertical_inset = centered_text_field_editor_vertical_inset(bounds);
            let _: () = msg_send![editor, setTextContainerInset: NSSize::new(8.0, vertical_inset)];
        }
        configured_editor
    }
}

/// Editable text field (alloc +1; caller owns or releases after adding to a parent).
pub(super) unsafe fn make_text_input(
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    value: &str,
) -> *mut AnyObject {
    let field: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let field: *mut AnyObject =
        msg_send![field, initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))];
    let ns = make_nsstring(value);
    let cell: *mut AnyObject = msg_send![centered_text_field_cell_class(), alloc];
    let cell: *mut AnyObject = msg_send![cell, initTextCell: ns];
    let _: () = msg_send![field, setCell: cell];
    release_obj(cell);
    let _: () = msg_send![field, setStringValue: ns];
    CFRelease(ns as *const c_void);
    let _: () = msg_send![field, setBezeled: false];
    // Replacing NSTextField's cell resets the field's editability flags on some macOS versions.
    // Restore them explicitly so a click still opens the field editor and accepts typing.
    let _: () = msg_send![field, setEditable: true];
    let _: () = msg_send![field, setSelectable: true];
    // Keep the caret while replacing AppKit's borderless blue outline with the documented 2pt
    // inset accent focus ring (design-style §9).
    let _: () = msg_send![field, setFocusRingType: 1isize]; // NSFocusRingTypeNone
    let _: () = msg_send![field, setDelegate: settings_text_field_delegate()];
    // Treat the value as a single line so AppKit centers its baseline in the 32pt row,
    // matching the vertical alignment of the popup controls beside it.
    let _: () = msg_send![field, setUsesSingleLineMode: true];
    // `scrollable` belongs to NSTextFieldCell rather than NSTextField.  A single-line,
    // scrollable cell uses AppKit's vertically centered editor layout; guard the selector so an
    // older macOS implementation cannot turn this styling hint into a startup crash.
    let cell: *mut AnyObject = msg_send![field, cell];
    if !cell.is_null() {
        let supports_scrollable: bool = msg_send![cell, respondsToSelector: sel!(setScrollable:)];
        if supports_scrollable {
            let _: () = msg_send![cell, setScrollable: true];
        }
    }
    let _: () = msg_send![field, setAlignment: 0isize]; // NSTextAlignmentLeft
                                                        // The rounded layer below is the sole background surface. Keeping the cell background off
                                                        // avoids a second, darker strip when the custom cell draws inside its centered rect.
    let _: () = msg_send![field, setDrawsBackground: false];
    let field_text = settings_text_color(SettingsTextRole::Primary);
    let _: () = msg_send![field, setTextColor: field_text];
    let _: () = msg_send![field, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![field, layer];
    if !layer.is_null() {
        layer_set_background(
            layer,
            crate::ffi::hex_to_cg_color(settings_palette().field_bg),
        );
        let _: () = msg_send![layer, setCornerRadius: crate::theme::RADIUS_CONTROL];
        let _: () = msg_send![layer, setMasksToBounds: true];
    }
    field
}

pub(super) use super::select::{
    clear_settings_select_registry, make_popup, settings_select_set_item_symbol,
};

pub(super) const HTML_SWITCH_W: f64 = 38.0;
pub(super) const HTML_SWITCH_H: f64 = 22.0;
pub(super) const HTML_SWITCH_KNOB_D: f64 = 18.0;
// Keep switches on the same trailing edge as popup fields in the settings column.
pub(super) const HTML_SWITCH_TRAILING_INSET: f64 = 0.0;
const HTML_SWITCH_SPRING_MASS: f64 = 4.0;
const HTML_SWITCH_SPRING_STIFFNESS: f64 = 800.0;
const HTML_SWITCH_SPRING_DAMPING: f64 = 80.0;
const HTML_SWITCH_PRESS_SCALE: f64 = 0.9;
const HTML_SWITCH_MIN_SETTLING_DURATION: f64 = 0.18;
const HTML_SWITCH_PRESS_DURATION: f64 = 0.22;

fn html_switch_spring_duration(settling_duration: f64) -> f64 {
    settling_duration.max(HTML_SWITCH_MIN_SETTLING_DURATION)
}

fn html_switch_track_radius() -> f64 {
    crate::theme::rounded_inset_radius(crate::theme::RADIUS_FULL, HTML_SWITCH_W, HTML_SWITCH_H)
}

fn html_switch_knob_radius() -> f64 {
    crate::theme::rounded_inset_radius(
        crate::theme::RADIUS_FULL,
        HTML_SWITCH_KNOB_D,
        HTML_SWITCH_KNOB_D,
    )
}

pub(super) struct HtmlSwitchClass(*mut AnyObject);
unsafe impl Send for HtmlSwitchClass {}
unsafe impl Sync for HtmlSwitchClass {}

pub(super) static HTML_SWITCH_CLASS: OnceLock<HtmlSwitchClass> = OnceLock::new();

/// Return the custom knob layer after the switch has been initialized.
unsafe fn html_switch_knob(button: *mut AnyObject) -> *mut AnyObject {
    let layer: *mut AnyObject = msg_send![button, layer];
    if layer.is_null() {
        return std::ptr::null_mut();
    }
    let sublayers: *mut AnyObject = msg_send![layer, sublayers];
    if sublayers.is_null() {
        return std::ptr::null_mut();
    }
    let count: usize = msg_send![sublayers, count];
    if count == 0 {
        std::ptr::null_mut()
    } else {
        msg_send![sublayers, objectAtIndex: 0usize]
    }
}

pub(super) unsafe fn html_switch_apply_visual(
    button: *mut AnyObject,
    previous_state: Option<isize>,
) {
    let layer: *mut AnyObject = msg_send![button, layer];
    if layer.is_null() {
        return;
    }
    let state: isize = msg_send![button, state];
    let enabled: bool = msg_send![button, isEnabled];
    let palette = settings_palette();
    let track_hex = if state != 0 {
        if enabled {
            palette.accent
        } else {
            palette.switch_on_disabled_track
        }
    } else if enabled {
        palette.switch_off_track
    } else {
        palette.switch_off_disabled_track
    };
    crate::ffi::layer_set_background(layer, crate::ffi::hex_to_cg_color(track_hex));
    let _: () = msg_send![layer, setCornerRadius: html_switch_track_radius()];
    let _: () = msg_send![layer, setMasksToBounds: false];

    let sublayers: *mut AnyObject = msg_send![layer, sublayers];
    let count: usize = if sublayers.is_null() {
        0
    } else {
        msg_send![sublayers, count]
    };
    let knob: *mut AnyObject = if count > 0 {
        msg_send![sublayers, objectAtIndex: 0usize]
    } else {
        let knob: *mut AnyObject = msg_send![class!(CALayer), layer];
        crate::ffi::layer_set_background(knob, crate::ffi::hex_to_cg_color(palette.switch_knob));
        let _: () = msg_send![knob, setCornerRadius: html_switch_knob_radius()];
        let _: () = msg_send![layer, addSublayer: knob];
        knob
    };
    let knob_y = (HTML_SWITCH_H - HTML_SWITCH_KNOB_D) / 2.0;
    let to_x = if state != 0 {
        HTML_SWITCH_W - HTML_SWITCH_KNOB_D - 2.0
    } else {
        2.0
    };
    let from_x = previous_state.map(|previous| {
        if previous != 0 {
            HTML_SWITCH_W - HTML_SWITCH_KNOB_D - 2.0
        } else {
            2.0
        }
    });
    let _: () = msg_send![
        knob,
        setFrame: NSRect::new(
            NSPoint::new(to_x, knob_y),
            NSSize::new(HTML_SWITCH_KNOB_D, HTML_SWITCH_KNOB_D)
        )
    ];

    if let Some(from_x) = from_x.filter(|x| *x != to_x) {
        if crate::theme::reduce_motion_enabled() {
            let knob_layer: *mut AnyObject = msg_send![knob, layer];
            if !knob_layer.is_null() {
                let _: () = msg_send![knob_layer, removeAllAnimations];
            }
            return;
        }
        let key_path = make_nsstring("position.x");
        let animation: *mut AnyObject = msg_send![
            class!(CASpringAnimation),
            animationWithKeyPath: key_path
        ];
        CFRelease(key_path as *const c_void);
        let from_value: *mut AnyObject =
            msg_send![class!(NSNumber), numberWithDouble: from_x + HTML_SWITCH_KNOB_D / 2.0];
        let to_value: *mut AnyObject =
            msg_send![class!(NSNumber), numberWithDouble: to_x + HTML_SWITCH_KNOB_D / 2.0];
        let _: () = msg_send![animation, setFromValue: from_value];
        let _: () = msg_send![animation, setToValue: to_value];
        let _: () = msg_send![animation, setMass: HTML_SWITCH_SPRING_MASS];
        let _: () = msg_send![animation, setStiffness: HTML_SWITCH_SPRING_STIFFNESS];
        let _: () = msg_send![animation, setDamping: HTML_SWITCH_SPRING_DAMPING];
        let _: () = msg_send![animation, setInitialVelocity: 0.0f64];
        let settling_duration: f64 = msg_send![animation, settlingDuration];
        let _: () =
            msg_send![animation, setDuration: html_switch_spring_duration(settling_duration)];
        let animation_key = make_nsstring("html-switch-position");
        let _: () = msg_send![knob, addAnimation: animation, forKey: animation_key];
        CFRelease(animation_key as *const c_void);
    }
}

/// Give the knob a short press-and-release response when the custom button is clicked.
unsafe fn html_switch_animate_press(button: *mut AnyObject) {
    if crate::theme::reduce_motion_enabled() {
        return;
    }
    let knob = html_switch_knob(button);
    if knob.is_null() {
        return;
    }

    // A keyframe keeps the model transform unchanged, so the switch is ready for the next
    // click even if the next state change arrives before this feedback finishes.
    let key_path = make_nsstring("transform.scale");
    let animation: *mut AnyObject = msg_send![
        class!(CAKeyframeAnimation),
        animationWithKeyPath: key_path
    ];
    CFRelease(key_path as *const c_void);

    let values: *mut AnyObject = msg_send![class!(NSMutableArray), array];
    for scale in [1.0, HTML_SWITCH_PRESS_SCALE, 1.0] {
        let value: *mut AnyObject = msg_send![class!(NSNumber), numberWithDouble: scale];
        let _: () = msg_send![values, addObject: value];
    }
    let _: () = msg_send![animation, setValues: values];
    let _: () = msg_send![animation, setDuration: HTML_SWITCH_PRESS_DURATION];
    let animation_key = make_nsstring("html-switch-press");
    let _: () = msg_send![knob, addAnimation: animation, forKey: animation_key];
    CFRelease(animation_key as *const c_void);
}

pub(super) extern "C" fn html_switch_set_state(this: *mut c_void, _cmd: Sel, state: isize) {
    unsafe {
        let button = this as *mut AnyObject;
        let previous_state: isize = msg_send![button, state];
        type SetState = unsafe extern "C" fn(*mut ObjcSuper, Sel, isize);
        let mut sup = ObjcSuper {
            receiver: this,
            super_class: class!(NSButton) as *const _ as *mut c_void,
        };
        let send: SetState = std::mem::transmute(objc_msgSendSuper as *const ());
        send(&mut sup, sel!(setState:), state);
        html_switch_apply_visual(button, (previous_state != state).then_some(previous_state));
    }
}

/// Refresh the custom switch track whenever its enabled state changes.
pub(super) extern "C" fn html_switch_set_enabled(this: *mut c_void, _cmd: Sel, enabled: bool) {
    unsafe {
        let mut sup = ObjcSuper {
            receiver: this,
            super_class: class!(NSButton) as *const _ as *mut c_void,
        };
        type SetEnabled = unsafe extern "C" fn(*mut ObjcSuper, Sel, bool);
        let send: SetEnabled = std::mem::transmute(objc_msgSendSuper as *const ());
        send(&mut sup, sel!(setEnabled:), enabled);
        html_switch_apply_visual(this as *mut AnyObject, None);
    }
}

/// The switch is rendered entirely by its layer, so toggle the state and dispatch the action
/// explicitly instead of relying on the hidden NSButtonCell drawing/tracking state.
pub(super) extern "C" fn html_switch_mouse_down(this: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    unsafe {
        let button = this as *mut AnyObject;
        let enabled: bool = msg_send![button, isEnabled];
        if !enabled {
            return;
        }

        let current: isize = msg_send![button, state];
        let next = if current == 0 { 1isize } else { 0isize };
        html_switch_animate_press(button);
        let _: () = msg_send![button, setState: next];

        // Most settings switches only need their state collected when OK is pressed. The two
        // switches with live behavior have an explicit target/action; dispatch those here.
        let target: *mut AnyObject = msg_send![button, target];
        if !target.is_null() {
            let action: Sel = msg_send![button, action];
            let _: bool = msg_send![button, sendAction: action, to: target];
        }
    }
}

/// Design-style §10 requires a hit target of at least 28x28 while §9 fixes the visible track at
/// 38x22. AppKit routes `mouseDown:` to the view whose `hitTest:` contains the point, so accept
/// points inside a transparent 28pt-tall rect centred on the track; the track stays 38x22.
pub(super) extern "C" fn html_switch_hit_test(
    this: *mut c_void,
    _cmd: Sel,
    point: NSPoint,
) -> *mut AnyObject {
    unsafe {
        let button = this as *mut AnyObject;
        let superview: *mut AnyObject = msg_send![button, superview];
        let local: NSPoint = if superview.is_null() {
            point
        } else {
            msg_send![button, convertPoint: point, fromView: superview]
        };
        let bounds: NSRect = msg_send![button, bounds];
        let hit = switch_hit_bounds(bounds);
        let inside = local.x >= hit.origin.x
            && local.x <= hit.origin.x + hit.size.width
            && local.y >= hit.origin.y
            && local.y <= hit.origin.y + hit.size.height;
        if inside {
            button
        } else {
            std::ptr::null_mut()
        }
    }
}

/// The switch's clickable rect: the visible 38x22 track, transparently padded to at least 28x28
/// (design-style §10) and centred on the track (§9 fixes the visible size).
pub(super) fn switch_hit_bounds(bounds: NSRect) -> NSRect {
    const MIN_HIT: f64 = 28.0;
    let pad_x = ((MIN_HIT - bounds.size.width) / 2.0).max(0.0);
    let pad_y = ((MIN_HIT - bounds.size.height) / 2.0).max(0.0);
    NSRect::new(
        NSPoint::new(bounds.origin.x - pad_x, bounds.origin.y - pad_y),
        NSSize::new(
            bounds.size.width + pad_x * 2.0,
            bounds.size.height + pad_y * 2.0,
        ),
    )
}

extern "C" fn html_switch_become_first_responder(this: *mut c_void, _cmd: Sel) -> bool {
    unsafe {
        type F = unsafe extern "C" fn(*mut ObjcSuper, Sel) -> bool;
        let mut sup = ObjcSuper {
            receiver: this,
            super_class: class!(NSButton) as *const _ as *mut c_void,
        };
        let send: F = std::mem::transmute(objc_msgSendSuper as *const ());
        let accepted = send(&mut sup, sel!(becomeFirstResponder));
        if accepted {
            // The track layer has no idle border; the 2pt accent ring is the focus indicator.
            apply_inset_focus_ring(this as *mut AnyObject, true, 0, 0.0);
        }
        accepted
    }
}

extern "C" fn html_switch_resign_first_responder(this: *mut c_void, _cmd: Sel) -> bool {
    unsafe {
        type F = unsafe extern "C" fn(*mut ObjcSuper, Sel) -> bool;
        let mut sup = ObjcSuper {
            receiver: this,
            super_class: class!(NSButton) as *const _ as *mut c_void,
        };
        let send: F = std::mem::transmute(objc_msgSendSuper as *const ());
        let accepted = send(&mut sup, sel!(resignFirstResponder));
        if accepted {
            apply_inset_focus_ring(this as *mut AnyObject, false, 0, 0.0);
        }
        accepted
    }
}

pub(super) fn html_switch_class() -> *mut AnyObject {
    HTML_SWITCH_CLASS
        .get_or_init(|| unsafe {
            let name = CString::new("OhMyTabHtmlSwitch").unwrap();
            let superclass = class!(NSButton) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let state_types = CString::new("v@:q").unwrap();
            class_addMethod(
                cls,
                sel!(setState:),
                html_switch_set_state as *mut c_void,
                state_types.as_ptr(),
            );
            let enabled_types = CString::new("v@:B").unwrap();
            class_addMethod(
                cls,
                sel!(setEnabled:),
                html_switch_set_enabled as *mut c_void,
                enabled_types.as_ptr(),
            );
            let mouse_types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(mouseDown:),
                html_switch_mouse_down as *mut c_void,
                mouse_types.as_ptr(),
            );
            let hit_types = CString::new("@@:{CGPoint=dd}").unwrap();
            class_addMethod(
                cls,
                sel!(hitTest:),
                html_switch_hit_test as *mut c_void,
                hit_types.as_ptr(),
            );
            // become/resignFirstResponder return BOOL ('B'), not void.
            let focus_types = CString::new("B@:").unwrap();
            class_addMethod(
                cls,
                sel!(becomeFirstResponder),
                html_switch_become_first_responder as *mut c_void,
                focus_types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(resignFirstResponder),
                html_switch_resign_first_responder as *mut c_void,
                focus_types.as_ptr(),
            );
            objc_registerClassPair(cls);
            HtmlSwitchClass(cls)
        })
        .0
}

/// HTML reference switch implemented as a custom-drawn NSButton.
/// alloc +1; caller releases after adding to the parent view.
/// The right_x parameter is the control column's right edge; the switch aligns with the popup
/// field's trailing edge on the same settings column.
pub(super) unsafe fn make_switch(right_x: f64, y: f64, h: f64, checked: bool) -> *mut AnyObject {
    let switch_right_x = right_x - HTML_SWITCH_TRAILING_INSET;
    let sw: *mut AnyObject = msg_send![html_switch_class(), alloc];
    let sw: *mut AnyObject =
        msg_send![sw, initWithFrame: NSRect::new(NSPoint::new(right_x, y), NSSize::new(0.0, 0.0))];
    let _: () = msg_send![sw, setButtonType: 1isize]; // NSButtonTypePushOnPushOff
    let empty_title = make_nsstring("");
    let _: () = msg_send![sw, setTitle: empty_title];
    CFRelease(empty_title as *const c_void);
    // Custom-drawn control: give VoiceOver a role (design-style §11); the label is associated by
    // SettingsRow::register_label from the row's own label.
    let role = make_nsstring("AXCheckBox");
    let _: () = msg_send![sw, setAccessibilityRole: role];
    CFRelease(role as *const c_void);
    let _: () = msg_send![sw, setBordered: false];
    let _: () = msg_send![
        sw,
        setFrame: NSRect::new(
            NSPoint::new(
                switch_right_x - HTML_SWITCH_W,
                y + (h - HTML_SWITCH_H) / 2.0,
            ),
            NSSize::new(HTML_SWITCH_W, HTML_SWITCH_H)
        )
    ];
    let _: () = msg_send![sw, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![sw, layer];
    if !layer.is_null() {
        html_switch_apply_visual(sw, None);
    }
    let _: () = msg_send![sw, setState: if checked { 1isize } else { 0isize }];
    sw
}

/// Integer slider (NSSlider, min..=max, step 1). alloc +1; caller releases after adding to parent.
/// `default_value`: the value a double-click restores (None = this row has no default and a
/// double-click is left alone).
#[allow(clippy::too_many_arguments)] // geometry + range + value + default exceeds the 7-arg limit.
pub(super) unsafe fn make_slider(
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    min: i64,
    max: i64,
    value: i64,
    default_value: Option<f64>,
) -> *mut AnyObject {
    let slider: *mut AnyObject = msg_send![settings_slider_class(), alloc];
    let slider: *mut AnyObject =
        msg_send![slider, initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))];
    // Send the action continuously while dragging: in live-apply mode runtime effects must
    // follow the drag in real time.
    let _: () = msg_send![slider, setContinuous: true];
    let _: () = msg_send![slider, setMinValue: min as f64];
    let _: () = msg_send![slider, setMaxValue: max as f64];
    // Integer steps: 1 tick = 1 unit (same as LinearMouse's By Lines slider: 0...10 step 1).
    let _: () = msg_send![slider, setNumberOfTickMarks: 0isize];
    let _: () = msg_send![slider, setAllowsTickMarkValuesOnly: false];
    let _: () = msg_send![slider, setIntegerValue: value];
    apply_slider_default(slider, default_value);
    slider
}

/// A continuous-value slider (no tick snapping), for fractional ranges such as pointer
/// acceleration / tracking speed.
///
/// Unlike the integer slider this deliberately does NOT send actions continuously: every mouse
/// config change goes through apply_config_change → pointer::apply(), which rebuilds the event
/// system client and waits ~30ms for asynchronous matching. Firing that on every mouse-dragged
/// event would stall the whole drag, so the value applies once on release (or on a track click)
/// -- which is also the better moment for a hardware property.
#[allow(clippy::too_many_arguments)] // geometry + range + value + default exceeds the 7-arg limit.
pub(super) unsafe fn make_double_slider(
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    min: f64,
    max: f64,
    value: f64,
    default_value: Option<f64>,
) -> *mut AnyObject {
    let slider: *mut AnyObject = msg_send![settings_slider_class(), alloc];
    let slider: *mut AnyObject =
        msg_send![slider, initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))];
    let _: () = msg_send![slider, setContinuous: false];
    let _: () = msg_send![slider, setMinValue: min];
    let _: () = msg_send![slider, setMaxValue: max];
    // No tick marks: NSSlider is continuous by default, and allowsTickMarkValuesOnly would
    // snap it to integers.
    let _: () = msg_send![slider, setDoubleValue: value];
    apply_slider_default(slider, default_value);
    slider
}

/// A dynamic NSSlider subclass: a double-click (clickCount == 2) restores the default registered
/// at creation and reuses the existing target/action chain (persisting the value and refreshing
/// the read-only readout both happen in handleControlChanged:).
///
/// The default lives in an ObjC ivar (`ohMyTabDefaultValue`, f64) so it follows the control's
/// lifetime -- no global pointer table, hence no cleanup coupling and no stale default if a
/// pointer is recycled. Sliders without a registered default behave exactly like a plain NSSlider.
static SETTINGS_SLIDER_CLS: OnceLock<usize> = OnceLock::new();
static SETTINGS_SLIDER_SUPERCLASS: OnceLock<usize> = OnceLock::new();

/// The default-value ivar name (added at class registration); a has-flag ivar of the sibling name
/// distinguishes "registered a default" from "just a zeroed slot".
const SLIDER_DEFAULT_IVAR: &str = "ohMyTabDefaultValue";
const SLIDER_HAS_DEFAULT_IVAR: &str = "ohMyTabHasDefault";

/// The two ivars' byte offsets inside the instance (fixed after registration, resolved once).
/// Direct offset access instead of object_set/getInstanceVariable (see the note in ffi.rs).
static SLIDER_IVAR_OFFSETS: OnceLock<(isize, isize)> = OnceLock::new();

fn settings_slider_class() -> *mut AnyObject {
    let cls = *SETTINGS_SLIDER_CLS.get_or_init(|| unsafe {
        let name = CString::new("OhMyTabSlider").unwrap();
        let superclass = class!(NSSlider) as *const _ as *mut AnyObject;
        let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
        // Ivars must be added before objc_registerClassPair (8-byte alignment -> log2 = 3;
        // char alignment 1).
        class_addIvar(
            cls,
            CString::new(SLIDER_DEFAULT_IVAR).unwrap().as_ptr(),
            std::mem::size_of::<f64>(),
            3,
            CString::new("d").unwrap().as_ptr(),
        );
        class_addIvar(
            cls,
            CString::new(SLIDER_HAS_DEFAULT_IVAR).unwrap().as_ptr(),
            1,
            0,
            CString::new("c").unwrap().as_ptr(),
        );
        let types = CString::new("v@:@").unwrap(); // -mouseDown:(NSEvent*) -> void
        class_addMethod(
            cls,
            sel!(mouseDown:),
            settings_slider_mouse_down as *mut c_void,
            types.as_ptr(),
        );
        objc_registerClassPair(cls);
        let _ = SETTINGS_SLIDER_SUPERCLASS.set(superclass as usize);
        // Resolve the offsets after registration (ivar_getOffset needs a registered class).
        let value_ivar =
            class_getInstanceVariable(cls, CString::new(SLIDER_DEFAULT_IVAR).unwrap().as_ptr());
        let flag_ivar =
            class_getInstanceVariable(cls, CString::new(SLIDER_HAS_DEFAULT_IVAR).unwrap().as_ptr());
        assert!(
            !value_ivar.is_null() && !flag_ivar.is_null(),
            "settings slider ivars must exist after registration"
        );
        let _ = SLIDER_IVAR_OFFSETS.set((ivar_getOffset(flag_ivar), ivar_getOffset(value_ivar)));
        cls as usize
    });
    cls as *mut AnyObject
}

/// Whether a double-click should restore the default (pure, for unit tests): we only take over
/// when it really is a double-click AND a default was registered.
fn slider_should_reset(click_count: isize, has_default: bool) -> bool {
    click_count == 2 && has_default
}

/// The default-value slot (instance base + offset); None before the class is initialized.
unsafe fn slider_default_slot(slider: *mut AnyObject) -> Option<(*mut u8, *mut f64)> {
    let (flag_offset, value_offset) = *SLIDER_IVAR_OFFSETS.get()?;
    let base = slider as *mut u8;
    Some((
        base.offset(flag_offset),
        base.offset(value_offset) as *mut f64,
    ))
}

/// Read the default registered on the control (None when absent).
unsafe fn slider_default_value(slider: *mut AnyObject) -> Option<f64> {
    let (flag, value) = slider_default_slot(slider)?;
    if *flag == 0 {
        None
    } else {
        Some(*value)
    }
}

/// Write the default into the control and attach a native tooltip describing the gesture
/// (a double-click is invisible, so it needs discoverability).
unsafe fn apply_slider_default(slider: *mut AnyObject, default_value: Option<f64>) {
    let Some(value) = default_value else {
        return;
    };
    if let Some((flag, slot)) = slider_default_slot(slider) {
        *slot = value;
        *flag = 1;
    }
    let text = tf(
        "settings.hint_double_click_default",
        &[("value", &settings_slider_display(value))],
    );
    let ns = make_nsstring(&text);
    let _: () = msg_send![slider, setToolTip: ns];
    CFRelease(ns as *const c_void);
}

/// Display text for the default: integral values without decimals (3), otherwise 2 decimals
/// (0.69) -- matching the readout's style.
fn settings_slider_display(value: f64) -> String {
    if (value.fract()).abs() < f64::EPSILON {
        format!("{}", value as i64)
    } else {
        format!("{value:.2}")
    }
}

/// The `mouseDown:` override -- a double-click restores the default; everything else goes back to
/// NSSlider (dragging / jump-to-click).
extern "C" fn settings_slider_mouse_down(this: *mut c_void, _cmd: Sel, event: *mut AnyObject) {
    crate::callback_guard::void("settings_slider_mouse_down", || unsafe {
        let slider = this as *mut AnyObject;
        let click_count: isize = msg_send![event, clickCount];
        let default = slider_default_value(slider);
        if slider_should_reset(click_count, default.is_some()) {
            if let Some(value) = default {
                let _: () = msg_send![slider, setDoubleValue: value];
                // Reuse the existing action chain: apply_control_field persists the value and
                // on_control_changed refreshes the readout. Note the first click of the
                // double-click already took effect as a normal click (possibly one transient
                // write); this write is the final state. action/target are always bound in practice
                // (see bind_control); skip when either is null.
                let target: *mut AnyObject = msg_send![slider, target];
                // objc2 validates the declared return encoding (`action` returns a SEL whose code is
                // ':', so it cannot be read as `*const c_void`). A raw msgSend both tolerates nil
                // (an unbound slider) and skips that check.
                type ActionFn = unsafe extern "C" fn(*mut AnyObject, Sel) -> *const c_void;
                let read_action: ActionFn = std::mem::transmute(objc_msgSend as *const ());
                let action: *const c_void = read_action(slider, sel!(action));
                if !action.is_null() && !target.is_null() {
                    // objc2's Sel cannot represent a null selector, so the action is passed through
                    // as a raw pointer (the slot itself is a SEL).
                    type SendActionFn = unsafe extern "C" fn(
                        *mut AnyObject,
                        Sel,
                        *const c_void,
                        *mut AnyObject,
                        *mut AnyObject,
                    ) -> bool;
                    let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
                    let f: SendActionFn = std::mem::transmute(objc_msgSend as *const ());
                    f(app, sel!(sendAction:to:from:), action, target, slider);
                }
            }
            // Do not call super: that would start another drag-tracking loop.
            return;
        }
        type MouseDownFn = unsafe extern "C" fn(*mut ObjcSuper, Sel, *mut AnyObject);
        let superclass = *SETTINGS_SLIDER_SUPERCLASS
            .get()
            .expect("settings slider superclass is initialized")
            as *mut c_void;
        let mut objc_super = ObjcSuper {
            receiver: this,
            super_class: superclass,
        };
        let f: MouseDownFn = std::mem::transmute(objc_msgSendSuper as *const ());
        f(&mut objc_super, sel!(mouseDown:), event);
    });
}

/// Apply a sidebar title's font/color and refresh the label's vertical optical alignment.
unsafe fn set_sidebar_title_appearance(
    btn: *mut AnyObject,
    title: &str,
    font: *mut AnyObject,
    color: *mut AnyObject,
) {
    let title_ns = make_nsstring(title);
    let label = SIDEBAR_TITLE_LABELS
        .lock()
        .unwrap()
        .get(&(btn as usize))
        .map(|p| p.0)
        .unwrap_or(btn);
    let _: () = msg_send![label, setFont: font];
    let _: () = msg_send![label, setTextColor: color];
    let _: () = msg_send![label, setStringValue: title_ns];
    crate::ffi::set_text_field_line_height(
        label,
        crate::theme::line_height(
            crate::theme::FONT_CONTROL,
            crate::theme::LINE_HEIGHT_BODY_RATIO,
        ),
    );
    let button_frame: NSRect = msg_send![btn, frame];
    center_sidebar_label(label, button_frame.size.height);
    CFRelease(title_ns as *const c_void);
}

/// Set the sidebar button title as an attributed title, using the secondary text color when
/// unselected and the system accent color when selected.
pub(super) unsafe fn set_sidebar_title(btn: *mut AnyObject, title: &str, selected: bool) {
    let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CONTROL, weight: crate::theme::FONT_WEIGHT_REGULAR];
    let color = settings_text_color(if selected {
        SettingsTextRole::Accent
    } else {
        SettingsTextRole::Sidebar
    });
    set_sidebar_title_appearance(btn, title, font, color);
    if let Some(icon) = SIDEBAR_ICON_VIEWS
        .lock()
        .unwrap()
        .get(&(btn as usize))
        .map(|p| p.0)
    {
        let _: () = msg_send![icon, setContentTintColor: color];
    }
    let _: () = msg_send![btn, setContentTintColor: color];
}

/// Apply the sidebar hover fill and foreground. The selected item owns its separate highlight.
unsafe fn set_sidebar_hovered(btn: *mut AnyObject, hovered: bool) {
    if btn.is_null() {
        return;
    }
    let tag: isize = msg_send![btn, tag];
    let selected = tag >= 0 && tag as usize == SIDEBAR_SELECTED.load(Ordering::SeqCst);
    let hovered = hovered && !selected;
    let palette = settings_palette();
    let layer: *mut AnyObject = msg_send![btn, layer];
    if !layer.is_null() {
        let fill = if hovered {
            sidebar_hover_fill(palette)
        } else {
            0x00000000
        };
        layer_set_background(layer, crate::ffi::hex_to_cg_color(fill));
    }
    let color = settings_text_color(if selected {
        SettingsTextRole::Accent
    } else if hovered {
        SettingsTextRole::Primary
    } else {
        SettingsTextRole::Sidebar
    });
    let label = SIDEBAR_TITLE_LABELS
        .lock()
        .unwrap()
        .get(&(btn as usize))
        .map(|p| p.0);
    if let Some(label) = label {
        let title = crate::ffi::nsstring_to_rust(msg_send![label, stringValue]);
        let font: *mut AnyObject = msg_send![label, font];
        set_sidebar_title_appearance(btn, &title, font, color);
    }
    if let Some(icon) = SIDEBAR_ICON_VIEWS
        .lock()
        .unwrap()
        .get(&(btn as usize))
        .map(|p| p.0)
    {
        let _: () = msg_send![icon, setContentTintColor: color];
    }
    let _: () = msg_send![btn, setContentTintColor: color];
}

pub(super) unsafe fn sidebar_hover_style_smoke(button: *mut AnyObject) -> bool {
    if button.is_null() {
        return false;
    }
    let tag: isize = msg_send![button, tag];
    if tag >= 0 && tag as usize == SIDEBAR_SELECTED.load(Ordering::SeqCst) {
        return false;
    }
    let layer: *mut AnyObject = msg_send![button, layer];
    if layer.is_null() {
        return false;
    }
    set_sidebar_hovered(button, true);
    let fill = crate::ffi::layer_background_color(layer);
    let observed_alpha = if fill.is_null() {
        0.0
    } else {
        crate::ffi::CGColorGetAlpha(fill)
    };
    let expected_alpha = f64::from(sidebar_hover_fill(settings_palette()) & 0xFF) / 255.0;
    set_sidebar_hovered(button, false);
    let cleared: *mut c_void = crate::ffi::layer_background_color(layer);
    let cleared_alpha = if cleared.is_null() {
        1.0
    } else {
        crate::ffi::CGColorGetAlpha(cleared)
    };
    (observed_alpha - expected_alpha).abs() <= 0.005 && cleared_alpha <= 0.005
}

/// Fit the sidebar label to its measured cell height and center that frame in the shared row.
/// This compensates for AppKit's top-biased text drawing and is rerun when selection changes the
/// font weight.
unsafe fn center_sidebar_label(label: *mut AnyObject, row_h: f64) {
    if label.is_null() {
        return;
    }
    let cell: *mut AnyObject = msg_send![label, cell];
    if cell.is_null() {
        return;
    }
    let bounds: NSRect = msg_send![label, bounds];
    let bounds = NSRect::new(
        bounds.origin,
        NSSize::new(bounds.size.width, row_h.max(1.0)),
    );
    let measured: NSSize = msg_send![cell, cellSizeForBounds: bounds];
    if !measured.height.is_finite() || measured.height <= 0.0 {
        return;
    }
    let mut frame: NSRect = msg_send![label, frame];
    frame.origin.y = (row_h - measured.height).max(0.0) / 2.0;
    frame.size.height = measured.height;
    let _: () = msg_send![label, setFrame: frame];
}

/// Show or hide the small update marker attached to a sidebar button's icon.
pub(super) unsafe fn set_sidebar_update_indicator(btn: *mut AnyObject, visible: bool) {
    if btn.is_null() {
        return;
    }

    let key = btn as usize;
    if let Some(dot) = SIDEBAR_UPDATE_DOTS.lock().unwrap().get(&key).map(|p| p.0) {
        let _: () = msg_send![dot, setHidden: !visible];
        return;
    }
    if !visible {
        return;
    }

    let icon_view = SIDEBAR_ICON_VIEWS
        .lock()
        .unwrap()
        .get(&key)
        .map(|p| p.0)
        .unwrap_or(std::ptr::null_mut());
    let dot_size = 6.0;
    // Use a sublayer instead of a subview so the marker stays purely visual and never steals
    // clicks from the sidebar button beneath it.
    let dot: *mut AnyObject = msg_send![class!(CALayer), layer];
    let (dot_parent, dot_frame) = if !icon_view.is_null() {
        let _: () = msg_send![icon_view, setWantsLayer: true];
        let icon_layer: *mut AnyObject = msg_send![icon_view, layer];
        let bounds: NSRect = msg_send![icon_view, bounds];
        (
            icon_layer,
            NSRect::new(
                // The icon's backing layer uses a bottom-up y axis; the larger y places the dot
                // at the icon's visual top edge rather than beside its vertical center.
                NSPoint::new(
                    bounds.size.width - dot_size * 0.7,
                    bounds.size.height - dot_size * 0.7,
                ),
                NSSize::new(dot_size, dot_size),
            ),
        )
    } else {
        let bounds: NSRect = msg_send![btn, bounds];
        (
            msg_send![btn, layer],
            NSRect::new(
                NSPoint::new(
                    bounds.size.width - dot_size * 2.0,
                    bounds.size.height - dot_size * 2.0,
                ),
                NSSize::new(dot_size, dot_size),
            ),
        )
    };
    if dot_parent.is_null() {
        return;
    }
    let _: () = msg_send![dot, setFrame: dot_frame];
    // CALayer does not clamp a too-large corner radius: RADIUS_FULL renders an unrounded square.
    // The status dot must stay circular, so cap the radius at half its bounds.
    let _: () = msg_send![dot, setCornerRadius: crate::theme::rounded_inset_radius(
        crate::theme::RADIUS_FULL,
        dot_size,
        dot_size,
    )];
    layer_set_background(
        dot,
        crate::ffi::hex_to_cg_color(settings_palette().destructive),
    );
    let _: () = msg_send![dot_parent, addSublayer: dot];
    SIDEBAR_UPDATE_DOTS
        .lock()
        .unwrap()
        .insert(key, ObjPtr::new(dot));
}

fn sidebar_hover_fill(palette: crate::theme::UiPalette) -> u32 {
    palette.hover_bg
}

/// Sidebar button (borderless NSButton; left-aligned icon + title; tag selects the page).
/// The component layer supplies the child frames so every item shares one alignment contract.
#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn make_sidebar_button(
    parent: *mut AnyObject,
    target: *mut AnyObject,
    title: &str,
    symbol: &str,
    tag: isize,
    x: f64,
    y: f64,
    w: f64,
    row_h: f64,
    icon_frame: NSRect,
    label_frame: NSRect,
) -> *mut AnyObject {
    let h = row_h;
    let btn: *mut AnyObject = msg_send![sidebar_button_class(), alloc];
    let btn: *mut AnyObject =
        msg_send![btn, initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))];
    let _: () = msg_send![btn, setButtonType: 0isize]; // NSPushInPushButton
    let _: () = msg_send![btn, setBordered: false];
    // NSButton starts with the default title "Button".  The sidebar title is rendered by
    // the fixed-column NSTextField below, so clear the native title to avoid drawing it twice.
    let empty_title = make_nsstring("");
    let _: () = msg_send![btn, setTitle: empty_title];
    CFRelease(empty_title as *const c_void);
    let _: () = msg_send![btn, setAlignment: 0isize]; // NSTextAlignmentLeft
    let _: () = msg_send![btn, setWantsLayer: true];
    let btn_layer: *mut AnyObject = msg_send![btn, layer];
    if !btn_layer.is_null() {
        layer_set_background(btn_layer, crate::ffi::hex_to_cg_color(0x00000000u32));
        let _: () = msg_send![btn_layer, setCornerRadius: crate::theme::RADIUS_CONTROL];
        let _: () = msg_send![btn_layer, setMasksToBounds: true];
    }
    let _: () = msg_send![btn, setTag: tag];
    let symbol_ns = make_nsstring(symbol);
    let image: *mut AnyObject = msg_send![
        class!(NSImage),
        imageWithSystemSymbolName: symbol_ns,
        accessibilityDescription: std::ptr::null::<AnyObject>()
    ];
    CFRelease(symbol_ns as *const c_void);
    if !image.is_null() {
        let icon_view: *mut AnyObject = msg_send![class!(NSImageView), alloc];
        let icon_view: *mut AnyObject = msg_send![
            icon_view,
            initWithFrame: icon_frame
        ];
        let _: () = msg_send![icon_view, setImage: image];
        let _: () = msg_send![icon_view, setImageScaling: 3isize];
        let _: () = msg_send![icon_view, setEditable: false];
        let _: () = msg_send![icon_view, setWantsLayer: false];
        let _: () = msg_send![btn, addSubview: icon_view];
        SIDEBAR_ICON_VIEWS
            .lock()
            .unwrap()
            .insert(btn as usize, ObjPtr::new(icon_view));
        release_obj(icon_view);
    }
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let label: *mut AnyObject = msg_send![
        label,
        initWithFrame: label_frame
    ];
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let label_color = settings_text_color(SettingsTextRole::Sidebar);
    let _: () = msg_send![label, setTextColor: label_color];
    let _: () = msg_send![label, setSelectable: false];
    let _: () = msg_send![label, setAlignment: -1isize]; // NSTextAlignmentNatural
                                                         // Keep every sidebar row at the same measured height, while allowing its label to use at
                                                         // most two lines. Longer localized titles are truncated only after the shared row is full.
    let _: () = msg_send![label, setUsesSingleLineMode: false];
    let _: () = msg_send![label, setLineBreakMode: 0isize]; // NSLineBreakByWordWrapping
    if msg_send![label, respondsToSelector: sel!(setMaximumNumberOfLines:)] {
        let _: () = msg_send![label, setMaximumNumberOfLines: 2isize];
    }
    if msg_send![label, respondsToSelector: sel!(setTruncatesLastVisibleLine:)] {
        let _: () = msg_send![label, setTruncatesLastVisibleLine: true];
    }
    let _: () = msg_send![label, setPreferredMaxLayoutWidth: label_frame.size.width];
    let _: () = msg_send![label, setEnabled: false];
    let _: () = msg_send![btn, addSubview: label];
    SIDEBAR_TITLE_LABELS
        .lock()
        .unwrap()
        .insert(btn as usize, ObjPtr::new(label));
    release_obj(label);
    let tracking: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
    let tracking: *mut AnyObject = msg_send![
        tracking,
        initWithRect: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h)),
        options: 0x01u64 | 0x80u64 | 0x200u64,
        owner: btn,
        userInfo: std::ptr::null::<AnyObject>()
    ];
    let _: () = msg_send![btn, addTrackingArea: tracking];
    release_obj(tracking);
    set_sidebar_title(btn, title, false);
    // adaptive: top- and left-anchored, fixed size
    let _: () = msg_send![btn, setAutoresizingMask: 12u64];
    let _: () = msg_send![btn, setTarget: target];
    let _: () = msg_send![btn, setAction: sel!(handleSettingsSidebar:)];
    let _: () = msg_send![parent, addSubview: btn];
    release_obj(btn);
    btn
}

/// Height of the label box a section heading is drawn in: `add_header` places the box's bottom
/// edge at the caller's y and grows upwards by this much. The page-top rhythm needs the height to
/// convert cursors, so it is exported here instead of that 20 being written out again elsewhere.
pub(super) const SECTION_HEADER_H: f64 = 20.0;

/// Bold section header label; released after being added to the parent.
pub(super) unsafe fn add_header(parent: *mut AnyObject, text: &str, x: f64, y: f64, w: f64) {
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let label: *mut AnyObject = msg_send![
        label,
        initWithFrame: NSRect::new(
            NSPoint::new(x, y),
            NSSize::new(w, SECTION_HEADER_H)
        )
    ];
    // Keep localized wording intact: uppercase is a hierarchy channel English has but CJK does
    // not. Weight, color, and spacing carry the section level instead.
    let ns = make_nsstring(text);
    let _: () = msg_send![label, setStringValue: ns];
    CFRelease(ns as *const c_void);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CAPTION, weight: crate::theme::FONT_WEIGHT_SEMIBOLD];
    let _: () = msg_send![label, setFont: font];
    apply_settings_text_role(label, SettingsTextRole::Secondary);
    // Adaptive: stretch width with the parent, stay top-anchored (MinYMargin).
    // Vertical placement belongs to the page layout owner (no MinYMargin).
    let _: () = msg_send![label, setAutoresizingMask: 2u64];
    let _: () = msg_send![parent, addSubview: label];
    release_obj(label);
}

/// Add a page title matching the HTML redesign's large, tight heading and return its height.
/// The 26pt size mirrors the design system's page-title role; the top-padding metric itself
/// lives in the SettingsPageHeader component, which passes the adjusted cursor here.
///
/// `top_cursor` is the cursor used by the page layout. The title keeps the same top inset while
/// growing downward when the localized text needs additional wrapped lines.
pub(super) unsafe fn add_page_title(
    parent: *mut AnyObject,
    text: &str,
    x: f64,
    top_cursor: f64,
    w: f64,
) -> f64 {
    const MIN_HEIGHT: f64 = 44.0;
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let label: *mut AnyObject = msg_send![
        label,
        initWithFrame: NSRect::new(NSPoint::new(x, 0.0), NSSize::new(w, MIN_HEIGHT))
    ];
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    set_page_title_text(label, text);
    let _: () = msg_send![label, setAlignment: -1isize]; // NSTextAlignmentNatural
    let _: () = msg_send![label, setUsesSingleLineMode: false];
    let _: () = msg_send![label, setLineBreakMode: 0isize]; // NSLineBreakByWordWrapping
    if msg_send![label, respondsToSelector: sel!(setMaximumNumberOfLines:)] {
        let _: () = msg_send![label, setMaximumNumberOfLines: 3isize];
    }
    let _: () = msg_send![label, setPreferredMaxLayoutWidth: w.max(1.0)];
    let cell: *mut AnyObject = msg_send![label, cell];
    if !cell.is_null() && msg_send![cell, respondsToSelector: sel!(setTruncatesLastVisibleLine:)] {
        let _: () = msg_send![cell, setTruncatesLastVisibleLine: true];
    }
    let measured: NSSize = msg_send![label, sizeThatFits: NSSize::new(w.max(1.0), 10_000.0)];
    let title_height = if measured.height.is_finite() && measured.height > 0.0 {
        measured.height.ceil().max(MIN_HEIGHT)
    } else {
        MIN_HEIGHT
    };
    let _: () = msg_send![
        label,
        setFrame: NSRect::new(
            NSPoint::new(x, top_cursor + 10.0 - title_height),
            NSSize::new(w, title_height),
        )
    ];
    apply_settings_text_role(label, SettingsTextRole::Primary);
    // Vertical placement belongs to the page layout owner (no MinYMargin).
    let _: () = msg_send![label, setAutoresizingMask: 2u64];
    let _: () = msg_send![parent, addSubview: label];
    release_obj(label);
    title_height
}

pub(super) const ABOUT_APP_ICON_IDENTIFIER: &str = "settings-about-app-icon";
pub(super) const ABOUT_HEADER_CONTENT_LEADING_X: f64 = 0.0;
pub(super) const ABOUT_APP_ICON_RENDER_OVERFLOW: f64 = 2.0;

/// Draw the app's bundled icon in the About header.
pub(super) unsafe fn add_about_app_icon(parent: *mut AnyObject, x: f64, y: f64) {
    const ICON_SLOT_SIZE: f64 = 58.0;
    const ICON_RENDER_SIZE: f64 = 62.0;

    let slot: *mut AnyObject = msg_send![class!(NSView), alloc];
    let slot: *mut AnyObject = msg_send![
        slot,
        initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(ICON_SLOT_SIZE, ICON_SLOT_SIZE))
    ];
    let image = crate::load_embedded_app_icon();
    if !image.is_null() {
        let image_view: *mut AnyObject = msg_send![class!(NSImageView), alloc];
        let image_view: *mut AnyObject = msg_send![
            image_view,
            initWithFrame: NSRect::new(
                NSPoint::new(-ABOUT_APP_ICON_RENDER_OVERFLOW, -ABOUT_APP_ICON_RENDER_OVERFLOW),
                NSSize::new(ICON_RENDER_SIZE, ICON_RENDER_SIZE)
            )
        ];
        let _: () = msg_send![image_view, setImage: image];
        let _: () = msg_send![image_view, setImageScaling: 3isize];
        let _: () = msg_send![image_view, setImageFrameStyle: 0isize];
        let identifier = make_nsstring(ABOUT_APP_ICON_IDENTIFIER);
        let _: () = msg_send![image_view, setIdentifier: identifier];
        release_obj(identifier);
        let _: () = msg_send![slot, addSubview: image_view];
        release_obj(image_view);
        release_obj(image);
    }
    let _: () = msg_send![parent, addSubview: slot];
    release_obj(slot);
}

pub(super) extern "C" fn settings_decoration_hit_test(
    _self: *mut c_void,
    _cmd: Sel,
    _point: NSPoint,
) -> *mut AnyObject {
    std::ptr::null_mut()
}

/// Flat detail-pane surface. The pane background is a solid `detail_bg` layer fill (no radial
/// gradient); the class only overrides `hitTest:` so the decoration never intercepts input.
pub(super) fn settings_pane_surface_view_class() -> *mut AnyObject {
    static CLASS: OnceLock<usize> = OnceLock::new();
    *CLASS.get_or_init(|| unsafe {
        let name = CString::new("OhMyTabSettingsPaneSurfaceView").unwrap();
        let superclass = class!(NSView) as *const _ as *mut AnyObject;
        let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
        class_addMethod(
            cls,
            sel!(hitTest:),
            settings_decoration_hit_test as *mut c_void,
            CString::new("@@:{CGPoint=dd}").unwrap().as_ptr(),
        );
        objc_registerClassPair(cls);
        cls as usize
    }) as *mut AnyObject
}

pub(super) const SETTINGS_CARD_STYLE_IDENTIFIER: &str = "com.ohmytab.settings-card";

/// Add a grouped settings card. Its surface and border define the container; settings cards do
/// not cast an elevation shadow.
pub(super) unsafe fn add_settings_card(parent: *mut AnyObject, frame: NSRect) -> *mut AnyObject {
    if frame.size.width <= 0.0 || frame.size.height <= 0.0 {
        return std::ptr::null_mut();
    }
    let palette = settings_palette();
    let card: *mut AnyObject = msg_send![class!(NSView), alloc];
    let card: *mut AnyObject = msg_send![card, initWithFrame: frame];
    let identifier = make_nsstring(SETTINGS_CARD_STYLE_IDENTIFIER);
    let _: () = msg_send![card, setIdentifier: identifier];
    CFRelease(identifier as *const c_void);
    let _: () = msg_send![card, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![card, layer];
    if !layer.is_null() {
        layer_set_background(layer, crate::ffi::hex_to_cg_color(palette.card_bg));
        let _: () = msg_send![layer, setCornerRadius: crate::theme::RADIUS_CARD];
        let _: () = msg_send![layer, setMasksToBounds: true];
        crate::ffi::layer_set_border(layer, crate::ffi::hex_to_cg_color(palette.card_border));
        let _: () = msg_send![layer, setBorderWidth: 1.0f64];
    }
    // Insert below controls and labels so the card never intercepts their mouse events.
    let _: () = msg_send![
        parent,
        addSubview: card,
        positioned: -1isize,
        relativeTo: std::ptr::null::<AnyObject>()
    ];

    release_obj(card);
    card
}

/// The card uses the section frame directly so its eight-point bottom inset remains visible.
pub(super) fn settings_card_rect(frame: NSRect) -> NSRect {
    frame
}

/// Draw the HTML `.row + .row` hairline inside a grouped card.
pub(super) unsafe fn add_row_separator(
    parent: *mut AnyObject,
    x: f64,
    y: f64,
    w: f64,
) -> *mut AnyObject {
    let frame = settings_row_separator_frame(x, y, w);
    let line: *mut AnyObject = msg_send![class!(NSView), alloc];
    let line: *mut AnyObject = msg_send![
        line,
        initWithFrame: frame
    ];
    let _: () = msg_send![line, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![line, layer];
    if !layer.is_null() {
        layer_set_background(
            layer,
            crate::ffi::hex_to_cg_color(settings_palette().separator),
        );
    }
    let _: () = msg_send![parent, addSubview: line];
    release_obj(line);
    line
}

fn settings_row_separator_frame(x: f64, y: f64, width: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(width.max(1.0), 1.0))
}

/// Add a standard row and also return its label pointer for conditional visibility.
///
/// The label/control are retained by the parent view after `addSubview`, so the pointers remain
/// valid after the local ownership is released.
fn derived_label_width(control_x: f64, label_x: f64, gap: f64) -> f64 {
    (control_x - label_x - gap).max(1.0)
}

pub(super) unsafe fn add_row_with_label(
    parent: *mut AnyObject,
    label_x: f64,
    y: f64,
    _label_w: f64,
    h: f64,
    label_text: &str,
    control: *mut AnyObject,
) -> (*mut AnyObject, *mut AnyObject) {
    // Derive the label's visual width from the control's leading edge. Call sites may pass a
    // legacy width for compatibility, but translated labels should not depend on per-string
    // 150/220pt patches.
    let control_frame: NSRect = msg_send![control, frame];
    let effective_label_w = derived_label_width(
        control_frame.origin.x,
        label_x,
        super::SETTINGS_CONTROL_LABEL_GAP,
    );
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let label: *mut AnyObject = msg_send![label, initWithFrame: NSRect::new(
        NSPoint::new(label_x, y),
        NSSize::new(effective_label_w, (h - 8.0).max(1.0)),
    )];
    let ns = make_nsstring(label_text);
    let _: () = msg_send![label, setStringValue: ns];
    CFRelease(ns as *const c_void);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CONTROL];
    let _: () = msg_send![label, setFont: font];
    // Left-aligned: the row label hugs the content area's left edge (NSTextAlignmentLeft = 0,
    // identical on arm64 and x86_64).
    let _: () = msg_send![label, setAlignment: -1isize]; // NSTextAlignmentNatural
    let label_color = settings_text_color(SettingsTextRole::Primary);
    let _: () = msg_send![label, setTextColor: label_color];
    let _: () = msg_send![label, setUsesSingleLineMode: false];
    let _: () = msg_send![label, setLineBreakMode: 0isize]; // NSLineBreakByWordWrapping
    if msg_send![label, respondsToSelector: sel!(setMaximumNumberOfLines:)] {
        let _: () = msg_send![label, setMaximumNumberOfLines: 2isize];
    }
    crate::ffi::set_text_field_line_height(
        label,
        crate::theme::line_height(
            crate::theme::FONT_CONTROL,
            crate::theme::LINE_HEIGHT_BODY_RATIO,
        ),
    );
    // Adaptive: label keeps fixed width, stays top- and left-anchored.
    // Vertical placement belongs to the page layout owner (no MinYMargin).
    let _: () = msg_send![label, setAutoresizingMask: 4u64];
    let _: () = msg_send![parent, addSubview: label];
    release_obj(label);
    // Adaptive: control stretches its width with the parent, stays top-anchored.
    // Vertical placement belongs to the page layout owner (no MinYMargin).
    let _: () = msg_send![control, setAutoresizingMask: 2u64];
    let _: () = msg_send![parent, addSubview: control];
    release_obj(control);
    (label, control)
}

/// Add a compact settings row. The legacy subtitle argument is intentionally ignored so callers
/// can migrate incrementally without rendering long explanatory paragraphs inside cards.
#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn add_described_row(
    parent: *mut AnyObject,
    x: f64,
    y: f64,
    text_w: f64,
    row_h: f64,
    title: &str,
    _subtitle: &str,
    control: *mut AnyObject,
) -> (*mut AnyObject, *mut AnyObject) {
    let title_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let title_label: *mut AnyObject = msg_send![
        title_label,
        initWithFrame: NSRect::new(
            NSPoint::new(x, y + 4.0),
            NSSize::new(text_w, (row_h - 8.0).max(1.0)),
        )
    ];
    set_field(title_label, title);
    let _: () = msg_send![title_label, setBezeled: false];
    let _: () = msg_send![title_label, setDrawsBackground: false];
    let _: () = msg_send![title_label, setEditable: false];
    let title_color = settings_text_color(SettingsTextRole::Primary);
    let _: () = msg_send![title_label, setTextColor: title_color];
    let _: () = msg_send![title_label, setAlignment: -1isize]; // NSTextAlignmentNatural
                                                               // Long translated row titles use the shared row height as a bounded multi-line text block.
    let _: () = msg_send![title_label, setUsesSingleLineMode: false];
    let _: () = msg_send![title_label, setLineBreakMode: 0isize]; // NSLineBreakByWordWrapping
    if msg_send![title_label, respondsToSelector: sel!(setMaximumNumberOfLines:)] {
        let _: () = msg_send![title_label, setMaximumNumberOfLines: 2isize];
    }
    let _: () = msg_send![title_label, setPreferredMaxLayoutWidth: text_w.max(1.0)];
    let title_cell: *mut AnyObject = msg_send![title_label, cell];
    if !title_cell.is_null()
        && msg_send![title_cell, respondsToSelector: sel!(setTruncatesLastVisibleLine:)]
    {
        // Ellipsize instead of silently clipping text that overflows the fixed-height row.
        let _: () = msg_send![title_cell, setTruncatesLastVisibleLine: true];
    }
    let title_font: *mut AnyObject =
        msg_send![class!(NSFont), messageFontOfSize: crate::theme::FONT_CONTROL];
    let _: () = msg_send![title_label, setFont: title_font];
    crate::ffi::set_text_field_line_height(
        title_label,
        crate::theme::line_height(
            crate::theme::FONT_CONTROL,
            crate::theme::LINE_HEIGHT_BODY_RATIO,
        ),
    );
    let _: () = msg_send![parent, addSubview: title_label];
    release_obj(title_label);

    // Vertical placement belongs to the page layout owner (no MinYMargin).
    let _: () = msg_send![control, setAutoresizingMask: 2u64];
    let _: () = msg_send![parent, addSubview: control];
    release_obj(control);
    (title_label, control)
}

#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn add_captioned_row(
    parent: *mut AnyObject,
    x: f64,
    y: f64,
    text_w: f64,
    row_h: f64,
    title: &str,
    caption: &str,
    control: *mut AnyObject,
) -> (*mut AnyObject, *mut AnyObject, *mut AnyObject) {
    let title_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let title_label: *mut AnyObject = msg_send![
        title_label,
        initWithFrame: NSRect::new(
            NSPoint::new(x, y + row_h - 24.0),
            NSSize::new(text_w, 18.0),
        )
    ];
    set_field(title_label, title);
    let _: () = msg_send![title_label, setBezeled: false];
    let _: () = msg_send![title_label, setDrawsBackground: false];
    let _: () = msg_send![title_label, setEditable: false];
    let _: () = msg_send![title_label, setSelectable: false];
    let _: () = msg_send![title_label, setUsesSingleLineMode: true];
    let _: () = msg_send![title_label, setLineBreakMode: 4isize];
    let title_font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CONTROL];
    let _: () = msg_send![title_label, setFont: title_font];
    apply_settings_text_role(title_label, SettingsTextRole::Primary);

    let caption_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let caption_label: *mut AnyObject = msg_send![
        caption_label,
        initWithFrame: NSRect::new(
            NSPoint::new(x, y + 4.0),
            NSSize::new(text_w, 16.0),
        )
    ];
    set_field(caption_label, caption);
    let _: () = msg_send![caption_label, setBezeled: false];
    let _: () = msg_send![caption_label, setDrawsBackground: false];
    let _: () = msg_send![caption_label, setEditable: false];
    let _: () = msg_send![caption_label, setSelectable: false];
    let _: () = msg_send![caption_label, setUsesSingleLineMode: true];
    let _: () = msg_send![caption_label, setLineBreakMode: 4isize];
    let caption_font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CAPTION];
    let _: () = msg_send![caption_label, setFont: caption_font];
    apply_settings_text_role(caption_label, SettingsTextRole::Muted);
    let caption_ns = make_nsstring(caption);
    let _: () = msg_send![caption_label, setToolTip: caption_ns];
    CFRelease(caption_ns as *const c_void);
    let title_ns = make_nsstring(title);
    let _: () = msg_send![title_label, setToolTip: title_ns];
    CFRelease(title_ns as *const c_void);
    let _: () = msg_send![title_label, setAutoresizingMask: 4u64];
    let _: () = msg_send![caption_label, setAutoresizingMask: 4u64];
    let _: () = msg_send![control, setAutoresizingMask: 2u64];
    let _: () = msg_send![parent, addSubview: caption_label];
    let _: () = msg_send![parent, addSubview: title_label];
    let _: () = msg_send![parent, addSubview: control];
    release_obj(caption_label);
    release_obj(title_label);
    release_obj(control);
    (title_label, caption_label, control)
}

/// A single-line settings row; the component layer centers its label and control inside the
/// shared visual height. Rows that genuinely need wrapping should use a dedicated multi-line layout.
#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn add_tall_row(
    parent: *mut AnyObject,
    label_x: f64,
    y: f64,
    label_w: f64,
    h: f64,
    label_text: &str,
    control: *mut AnyObject,
) -> (*mut AnyObject, *mut AnyObject) {
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let label: *mut AnyObject = msg_send![label, initWithFrame: NSRect::new(NSPoint::new(label_x, y + 4.0), NSSize::new(label_w, h - 8.0))];
    let ns = make_nsstring(label_text);
    let _: () = msg_send![label, setStringValue: ns];
    CFRelease(ns as *const c_void);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let _: () = msg_send![label, setAlignment: -1isize]; // NSTextAlignmentNatural
    let label_color = settings_text_color(SettingsTextRole::Primary);
    let _: () = msg_send![label, setTextColor: label_color];
    let font: *mut AnyObject =
        msg_send![class!(NSFont), messageFontOfSize: crate::theme::FONT_CONTROL];
    let _: () = msg_send![label, setFont: font];
    let _: () = msg_send![label, setUsesSingleLineMode: false];
    let _: () = msg_send![label, setLineBreakMode: 0isize]; // NSLineBreakByWordWrapping
    if msg_send![label, respondsToSelector: sel!(setMaximumNumberOfLines:)] {
        let _: () = msg_send![label, setMaximumNumberOfLines: 2isize];
    }
    if msg_send![label, respondsToSelector: sel!(setTruncatesLastVisibleLine:)] {
        let _: () = msg_send![label, setTruncatesLastVisibleLine: true];
    }
    crate::ffi::set_text_field_line_height(
        label,
        crate::theme::line_height(
            crate::theme::FONT_CONTROL,
            crate::theme::LINE_HEIGHT_BODY_RATIO,
        ),
    );
    // Vertical placement belongs to the page layout owner (no MinYMargin).
    let _: () = msg_send![label, setAutoresizingMask: 4u64];
    let _: () = msg_send![parent, addSubview: label];
    release_obj(label);
    // Vertical placement belongs to the page layout owner (no MinYMargin).
    let _: () = msg_send![control, setAutoresizingMask: 2u64];
    let _: () = msg_send![parent, addSubview: control];
    release_obj(control);
    (label, control)
}

/// Style an NSPopUpButton with the HTML `.field` look: a rounded light-gray surface, no
/// bezel, so the Device/Scroll-mode dropdowns match the flat reference control.
pub(super) unsafe fn style_flat_popup(popup: *mut AnyObject) {
    let _: () = msg_send![popup, setBezelStyle: 0isize];
    let _: () = msg_send![popup, setControlSize: 0isize]; // Regular
    let _: () = msg_send![popup, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![popup, layer];
    if !layer.is_null() {
        let _: () = msg_send![layer, setCornerRadius: crate::theme::RADIUS_CONTROL];
        let _: () = msg_send![layer, setMasksToBounds: true];
        let palette = settings_palette();
        crate::ffi::layer_set_background(layer, crate::ffi::hex_to_cg_color(palette.field_bg));
        crate::ffi::layer_set_border(layer, crate::ffi::hex_to_cg_color(palette.card_border));
        let _: () = msg_send![layer, setBorderWidth: 1.0f64];
    }
    let palette = settings_palette();
    let tint = crate::ffi::hex_to_ns_color(palette.primary_text);
    let _: () = msg_send![popup, setContentTintColor: tint];
    let cell: *mut AnyObject = msg_send![popup, cell];
    if !cell.is_null() && msg_send![cell, respondsToSelector: sel!(setTextColor:)] {
        let _: () = msg_send![cell, setTextColor: tint];
    }
}

/// Create one transparent, vertically scrolling settings page above the fixed footer. The
/// document view keeps AppKit's normal bottom-left coordinate system so the existing layout
/// code can continue to position controls from a top cursor.
pub(super) unsafe fn make_settings_page(
    parent: *mut AnyObject,
    frame: NSRect,
    document_h: f64,
    hidden: bool,
) -> (*mut AnyObject, *mut AnyObject) {
    let scroll: *mut AnyObject = msg_send![class!(NSScrollView), alloc];
    let scroll: *mut AnyObject = msg_send![scroll, initWithFrame: frame];
    // FullSizeContentView lets the scroll view reach into the title bar, so AppKit auto-adds a
    // top content inset. Without disabling it, the real scrollable top sits below the geometric
    // doc top, so scrollToPoint(0, doc_h - clip_h) never lands at the very top (the Mouse title
    // looks flush yet the scrollbar can still rise). Disable the auto inset and force zero inset.
    let _: () = msg_send![scroll, setAutomaticallyAdjustsContentInsets: false];
    let _: () = msg_send![scroll, setContentInsets: NSEdgeInsets { top: 0.0, left: 0.0, bottom: 0.0, right: 0.0 }];
    let _: () = msg_send![scroll, setBorderType: 0u64];
    let _: () = msg_send![scroll, setDrawsBackground: false];
    let _: () = msg_send![scroll, setHasHorizontalScroller: false];
    let _: () = msg_send![scroll, setHasVerticalScroller: true];
    let _: () = msg_send![scroll, setAutohidesScrollers: true];
    let _: () = msg_send![scroll, setScrollerStyle: 1isize]; // overlay
    let _: () = msg_send![scroll, setAutoresizingMask: 18u64];
    let _: () = msg_send![scroll, setHidden: hidden];

    let clip: *mut AnyObject = msg_send![scroll, contentView];
    let _: () = msg_send![clip, setDrawsBackground: false];

    let document: *mut AnyObject = msg_send![class!(NSView), alloc];
    // The document must never be shorter than the viewport: a non-flipped document shorter
    // than its clip gets pinned to the clip's BOTTOM by AppKit, shifting all content down by
    // (clip - document) -- the quick-actions page (728 < window 752) shifted 24pt for exactly
    // this reason.
    let document_h = document_h.max(frame.size.height);
    let document: *mut AnyObject = msg_send![
        document,
        initWithFrame: NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(frame.size.width, document_h),
        )
    ];
    // The document is NOT width-sizable (top-anchored only). It used to be NSViewWidthSizable: when the
    // system switched the scroller to space-taking legacy, the clip lost 17pt, the document shrank with
    // it, and width-flexible self-drawn controls inside (the switch 38 -> 21, action buttons shifted
    // 17pt left) got squeezed out of shape. Content width is decided by the scroller resync logic, not
    // by AppKit's tiling.
    let _: () = msg_send![document, setAutoresizingMask: 8u64];
    let _: () = msg_send![scroll, setDocumentView: document];
    let _: () = msg_send![parent, addSubview: scroll];

    // Scroll to the TOP of the document so the page opens with the title flush at the top edge.
    // Measure the clip's real bounds height AFTER the doc view is attached (a frame-based guess
    // can be stale before layout and leaves the scrollbar mid-track).
    let clip_bounds: NSRect = msg_send![clip, bounds];
    let top_origin = (document_h - clip_bounds.size.height).max(0.0);
    let _: () = msg_send![clip, scrollToPoint: NSPoint::new(0.0, top_origin)];
    let _: () = msg_send![scroll, reflectScrolledClipView: clip];

    release_obj(document);
    release_obj(scroll);
    (scroll, document)
}

/// Pure rectangle predicates shared by the debug validator and headless layout tests.
pub(super) fn rects_overlap(a: NSRect, b: NSRect, epsilon: f64) -> bool {
    let left = a.origin.x.max(b.origin.x);
    let right = (a.origin.x + a.size.width).min(b.origin.x + b.size.width);
    let bottom = a.origin.y.max(b.origin.y);
    let top = (a.origin.y + a.size.height).min(b.origin.y + b.size.height);
    right - left > epsilon && top - bottom > epsilon
}

pub(super) fn rect_inside(outer: NSRect, inner: NSRect, margin: f64) -> bool {
    inner.origin.x >= outer.origin.x + margin
        && inner.origin.y >= outer.origin.y + margin
        && inner.origin.x + inner.size.width <= outer.origin.x + outer.size.width - margin
        && inner.origin.y + inner.size.height <= outer.origin.y + outer.size.height - margin
}

#[derive(Clone)]
struct DebugLayoutEntry {
    index: usize,
    frame: NSRect,
    interactive: bool,
    embedded_in_button: bool,
    text_required_height: Option<f64>,
    text_value: Option<String>,
}

/// Collect descendant frames in document coordinates.
///
/// Manual settings layout uses several nested AppKit views, and some of them are flipped (the Mouse
/// page's binding table, the About page's update host), so accumulating local origins would report
/// a flipped container's children at nonsense positions. Asking the document to convert each frame
/// handles flips, offsets and future nesting in one call.
unsafe fn collect_debug_layout(
    document: *mut AnyObject,
    view: *mut AnyObject,
    entries: &mut Vec<DebugLayoutEntry>,
    inside_native_control: bool,
    inside_button: bool,
) {
    if view.is_null() {
        return;
    }
    let subviews: *mut AnyObject = msg_send![view, subviews];
    let count: usize = if subviews.is_null() {
        0
    } else {
        msg_send![subviews, count]
    };
    for index in 0..count {
        let child: *mut AnyObject = msg_send![subviews, objectAtIndex: index as isize];
        if child.is_null() || msg_send![child, isHidden] {
            continue;
        }
        let local: NSRect = msg_send![child, frame];
        // `frame` is expressed in the superview's coordinates, so convert from there (AppKit's
        // conversion also accounts for a flipped container, which a naive sum cannot).
        let superview: *mut AnyObject = msg_send![child, superview];
        let frame: NSRect = if superview.is_null() {
            local
        } else {
            msg_send![document, convertRect: local, fromView: superview]
        };
        let interactive = msg_send![child, isKindOfClass: class!(NSButton)]
            || msg_send![child, isKindOfClass: class!(NSSlider)]
            || msg_send![child, isKindOfClass: class!(NSColorWell)]
            || msg_send![child, isKindOfClass: class!(NSPopUpButton)];
        let is_button: bool = msg_send![child, isKindOfClass: class!(NSButton)];
        let is_text = msg_send![child, isKindOfClass: class!(NSTextField)];
        let is_wrapping_text = if is_text {
            let single_line: bool = msg_send![child, usesSingleLineMode];
            let line_break_mode: isize = msg_send![child, lineBreakMode];
            !single_line && line_break_mode == 0 // NSLineBreakByWordWrapping
        } else {
            false
        };
        // Native controls other than NSButton contain internal text views that are implementation
        // details. Custom settings buttons, however, draw their localized title in a child label;
        // inspect that label's fit while excluding it from peer-overlap checks.
        if !inside_native_control && (interactive || is_text) {
            let text_value = if is_text {
                let value: *mut AnyObject = msg_send![child, stringValue];
                let utf8: *const std::ffi::c_char = msg_send![value, UTF8String];
                if utf8.is_null() {
                    None
                } else {
                    Some(
                        std::ffi::CStr::from_ptr(utf8)
                            .to_string_lossy()
                            .into_owned(),
                    )
                }
            } else {
                None
            };
            let text_required_height = if is_wrapping_text {
                full_wrapped_text_size(child, local.size.width).map(|measured| measured.height)
            } else {
                None
            };
            entries.push(DebugLayoutEntry {
                index,
                frame,
                interactive: interactive && !inside_button,
                embedded_in_button: is_text && inside_button,
                text_required_height,
                text_value,
            });
        }
        let next_inside_native_control = inside_native_control || (interactive && !is_button);
        collect_debug_layout(
            document,
            child,
            entries,
            next_inside_native_control,
            inside_button || is_button,
        );
    }
}

/// Validate the real AppKit page tree when explicitly requested during development. This catches
/// controls crossing peer labels or controls and frames escaping the document. Native controls'
/// internal descendants are excluded because they are not peer layout items.
pub(super) unsafe fn debug_validate_settings_page(scroll: *mut AnyObject, name: &str) {
    if !cfg!(debug_assertions) {
        return;
    }
    // The switch rides argv: `--layout-debug` opts in explicitly, and the `--smoke-settings-layout`
    // smoke path implies it (these assertions are exactly what it validates). This used to read an
    // environment variable that the smoke path set with set_var; both sides are argv-only now.
    if !crate::dev_flags::present("layout-debug")
        && !crate::dev_flags::present("smoke-settings-layout")
    {
        return;
    }
    if scroll.is_null() {
        panic!("[settings-layout] {name}: scroll view is null");
    }
    let document: *mut AnyObject = msg_send![scroll, documentView];
    if document.is_null() {
        panic!("[settings-layout] {name}: document view is null");
    }
    let document_bounds: NSRect = msg_send![document, bounds];
    let mut entries = Vec::new();
    collect_debug_layout(document, document, &mut entries, false, false);
    let mut errors = Vec::new();
    let document_rect = NSRect::new(NSPoint::new(0.0, 0.0), document_bounds.size);
    collect_switch_visual_errors(document, &mut errors);
    for entry in &entries {
        if !rect_inside(document_rect, entry.frame, -1.0) {
            errors.push(format!(
                "view[{}] escapes document: {:?}",
                entry.index, entry.frame
            ));
        }
        if let Some(required_height) = entry.text_required_height {
            if required_height > entry.frame.size.height + 1.0 {
                errors.push(format!(
                    "text[{}] {:?} needs {:.1}pt but frame is {:.1}pt high: {:?}",
                    entry.index,
                    entry.text_value,
                    required_height,
                    entry.frame.size.height,
                    entry.frame
                ));
            }
        }
    }
    for (left_index, left) in entries.iter().enumerate() {
        for right in entries.iter().skip(left_index + 1) {
            // Text labels may overlap another label in a deliberately stacked description row,
            // but an interactive control must never intersect a label or another control.
            if (left.interactive || right.interactive)
                && !left.embedded_in_button
                && !right.embedded_in_button
                && rects_overlap(left.frame, right.frame, 0.5)
            {
                errors.push(format!(
                    "views overlap: [{}] {:?} × [{}] {:?}",
                    left.index, left.frame, right.index, right.frame
                ));
            }
        }
    }
    if !errors.is_empty() {
        // Log the details before panicking: the panic message itself is a single multi-line write
        // that the logger's stderr capture does not keep intact.
        for error in &errors {
            crate::log_info!("[settings-layout] {name}: {error}");
        }
        panic!("[settings-layout] {name}:\n{}", errors.join("\n"));
    }
}

unsafe fn collect_switch_visual_errors(view: *mut AnyObject, errors: &mut Vec<String>) {
    if view.is_null() {
        return;
    }
    if msg_send![view, isKindOfClass: html_switch_class()] {
        let bounds: NSRect = msg_send![view, bounds];
        let layer: *mut AnyObject = msg_send![view, layer];
        if layer.is_null() {
            errors.push("switch track layer is missing".into());
        } else {
            let track_radius: f64 = msg_send![layer, cornerRadius];
            let track_limit = bounds.size.width.min(bounds.size.height) / 2.0;
            if bounds.size.width <= 0.0
                || bounds.size.height <= 0.0
                || !track_radius.is_finite()
                || track_radius > track_limit + 0.01
            {
                errors.push(format!(
                    "switch track is not drawable: bounds={bounds:?} radius={track_radius:.1}"
                ));
            }
        }

        let knob = html_switch_knob(view);
        if knob.is_null() {
            errors.push("switch knob layer is missing".into());
        } else {
            let knob_frame: NSRect = msg_send![knob, frame];
            let knob_radius: f64 = msg_send![knob, cornerRadius];
            let knob_limit = knob_frame.size.width.min(knob_frame.size.height) / 2.0;
            if knob_frame.size.width <= 0.0
                || knob_frame.size.height <= 0.0
                || !knob_radius.is_finite()
                || knob_radius > knob_limit + 0.01
            {
                errors.push(format!(
                    "switch knob is not drawable: frame={knob_frame:?} radius={knob_radius:.1}"
                ));
            }
        }
    }

    let subviews: *mut AnyObject = msg_send![view, subviews];
    if subviews.is_null() {
        return;
    }
    let count: usize = msg_send![subviews, count];
    for index in 0..count {
        let child: *mut AnyObject = msg_send![subviews, objectAtIndex: index];
        collect_switch_visual_errors(child, errors);
    }
}

/// Scroll a settings page's clip view to the top. Call this after the window has been laid out:
/// a frame-time scrollToPoint gets reset by AppKit's first layout pass, leaving the scrollbar
/// mid-track. The page scroll views are the same views stored on SettingsUi (general_view, etc.).
pub(super) unsafe fn scroll_page_to_top(scroll: *mut AnyObject) {
    if scroll.is_null() {
        return;
    }
    let clip: *mut AnyObject = msg_send![scroll, contentView];
    let doc: *mut AnyObject = msg_send![scroll, documentView];
    let doc_frame: NSRect = msg_send![doc, frame];
    let clip_bounds: NSRect = msg_send![clip, bounds];
    let top_origin = (doc_frame.size.height - clip_bounds.size.height).max(0.0);
    let _: () = msg_send![clip, scrollToPoint: NSPoint::new(0.0, top_origin)];
    let _: () = msg_send![scroll, reflectScrolledClipView: clip];
    let after: NSRect = msg_send![clip, bounds];
    log_debug!(
        "[settings] scroll_page_to_top: doc_h={:.1} clip_h={:.1} top_origin={:.1} -> clip_oy_after={:.1}",
        doc_frame.size.height,
        clip_bounds.size.height,
        top_origin,
        after.origin.y
    );
}

#[cfg(test)]
mod tests {
    use super::{
        derived_label_width, html_switch_knob_radius, html_switch_track_radius, rect_inside,
        rects_overlap, sidebar_hover_fill, slider_should_reset, switch_hit_bounds, HTML_SWITCH_H,
        HTML_SWITCH_KNOB_D, HTML_SWITCH_W, SIDEBAR_HOVERED,
    };
    // The smoke test sends ObjC messages directly (building an NSEvent, driving mouseDown:).
    use crate::ffi::release_obj;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send, sel};
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use std::ffi::c_void;

    #[test]
    fn slider_double_click_reset_only_applies_to_double_clicks_with_a_default() {
        // Only a real double-click with a registered default takes over; everything else goes to
        // NSSlider itself.
        assert!(slider_should_reset(2, true));
        assert!(!slider_should_reset(1, true));
        assert!(!slider_should_reset(3, true));
        assert!(!slider_should_reset(2, false));
        assert!(!slider_should_reset(0, false));
    }

    /// `SIDEBAR_HOVERED` caches a raw button address, so dropping it must NOT go through
    /// `clear_sidebar_hover` -- that one messages the stored button to reset its fill, which is
    /// precisely the freed object the teardown path must not touch.
    ///
    /// The sentinel below is not a real object. If `forget_sidebar_hover` regressed into
    /// messaging it, this test would trap in objc's receiver check instead of failing an
    /// assertion: that hard stop is the point, because it is the crash this guards.
    #[test]
    fn forget_sidebar_hover_drops_the_address_without_messaging_it() {
        const SENTINEL: usize = 0xDEAD_BEEF;
        SIDEBAR_HOVERED.store(SENTINEL, std::sync::atomic::Ordering::SeqCst);
        super::forget_sidebar_hover();
        assert_eq!(
            SIDEBAR_HOVERED.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the hovered address must not survive a content rebuild"
        );
    }

    /// Every `SettingsButtonRole` must round-trip: the fill `mouseExited` restores has to be
    /// exactly the role's own normal color, or the button stays tinted after the pointer leaves.
    /// This is the reverse direction of `button_roles_keep_normal_and_hover_semantics_distinct`,
    /// which only checks the forward `style()` mapping.
    #[test]
    fn action_button_exit_restores_every_role_normal_color() {
        use crate::settings::components::SettingsButtonRole;
        for palette in [
            crate::theme::ui_palette_for_mode(false),
            crate::theme::ui_palette_for_mode(true),
        ] {
            for role in [
                SettingsButtonRole::Action,
                SettingsButtonRole::Compact,
                SettingsButtonRole::Footer,
                SettingsButtonRole::Primary,
                SettingsButtonRole::Destructive,
            ] {
                let (normal, _text, tag) = role.style(palette);
                assert_eq!(
                    super::action_button_normal_color(tag, palette),
                    normal,
                    "{role:?} (tag {tag}) must restore its own normal color on mouse-exit"
                );
            }
        }
    }

    /// A hover fill that equals the normal color would make the button look inert; every role
    /// needs a visibly distinct hover in both palettes.
    #[test]
    fn action_button_hover_is_distinct_from_normal_for_every_role() {
        use crate::settings::components::SettingsButtonRole;
        for palette in [
            crate::theme::ui_palette_for_mode(false),
            crate::theme::ui_palette_for_mode(true),
        ] {
            for role in [
                SettingsButtonRole::Action,
                SettingsButtonRole::Compact,
                SettingsButtonRole::Footer,
                SettingsButtonRole::Primary,
                SettingsButtonRole::Destructive,
            ] {
                let (normal, _text, tag) = role.style(palette);
                let hover = super::action_button_hover_color(tag, palette);
                assert_ne!(
                    hover, normal,
                    "{role:?} (tag {tag}) hover {hover:#010X} equals its normal {normal:#010X}"
                );
            }
        }
    }

    /// Pins *which* hover token each role resolves to, not merely that it differs from its normal
    /// color. Distinctness alone is too weak: pointing `Compact` at `accent` still differs from
    /// `field_bg`, yet repaints a subtle field button in accent blue on hover.
    #[test]
    fn action_button_hover_uses_the_expected_token_per_role() {
        use crate::settings::components::SettingsButtonRole;
        for palette in [
            crate::theme::ui_palette_for_mode(false),
            crate::theme::ui_palette_for_mode(true),
        ] {
            // Roles with a dedicated hover token use it; the rest layer the generic `hover_bg`
            // tint over whatever surface their normal color provides.
            let expected = [
                (SettingsButtonRole::Action, palette.hover_bg),
                (SettingsButtonRole::Compact, palette.hover_bg),
                (SettingsButtonRole::Footer, palette.hover_bg),
                (SettingsButtonRole::Primary, palette.accent_hover),
                (SettingsButtonRole::Destructive, palette.destructive_hover),
            ];
            for (role, want) in expected {
                let (_normal, _text, tag) = role.style(palette);
                assert_eq!(
                    super::action_button_hover_color(tag, palette),
                    want,
                    "{role:?} (tag {tag}) resolved the wrong hover token"
                );
            }
        }
    }

    /// Smoke (GUI/AppKit): the default-value ivar round-trips and a real NSEvent double-click
    /// restores it. Requires a GUI session, hence #[ignore]; run with
    /// `cargo test -- --ignored slider_double_click_smoke`.
    #[test]
    #[ignore]
    fn slider_double_click_smoke() {
        unsafe {
            let slider = super::make_slider(0.0, 0.0, 120.0, 20.0, 0, 10, 7, Some(3.0));
            assert_eq!(super::slider_default_value(slider), Some(3.0));

            // A double-click (clickCount = 2) restores the default 3.
            let double = make_click_event(2);
            super::settings_slider_mouse_down(slider as *mut c_void, sel!(mouseDown:), double);
            let value: f64 = msg_send![slider, doubleValue];
            assert_eq!(value, 3.0, "double-click must restore the default");

            // A single click still goes to NSSlider (the value is untouched; with no window the
            // superclass simply does not track anything).
            let _: () = msg_send![slider, setDoubleValue: 7.0f64];
            let single = make_click_event(1);
            super::settings_slider_mouse_down(slider as *mut c_void, sel!(mouseDown:), single);
            let value: f64 = msg_send![slider, doubleValue];
            assert_eq!(value, 7.0, "a single click must not reset");

            // The NSEvents come from a factory method (+0, autoreleased) and must not be released
            // manually; a test has no autorelease pool, and this one-off leak is irrelevant.
            release_obj(slider);
        }
    }

    /// Build a left-mouse-down event (with the given clickCount) for the smoke test to feed into
    /// mouseDown:.
    unsafe fn make_click_event(click_count: isize) -> *mut AnyObject {
        msg_send![
            class!(NSEvent),
            mouseEventWithType: 1isize, // NSEventTypeLeftMouseDown
            location: objc2_foundation::NSPoint::new(0.0, 0.0),
            modifierFlags: 0u64,
            timestamp: 0.0f64,
            windowNumber: 0isize,
            context: std::ptr::null_mut::<AnyObject>(),
            eventNumber: 0isize,
            clickCount: click_count,
            pressure: 1.0f32,
        ]
    }

    #[test]
    fn label_width_follows_control_leading_edge() {
        assert_eq!(derived_label_width(319.0, 16.0, 16.0), 287.0);
        assert_eq!(derived_label_width(20.0, 16.0, 16.0), 1.0);
    }

    #[test]
    #[ignore]
    fn switch_hit_test_and_focus_ring_smoke() {
        unsafe {
            // Switch hit-test: the padded click rect answers, points outside it do not.
            let sw = super::make_switch(200.0, 0.0, 52.0, false);
            let inside: *mut AnyObject = msg_send![sw, hitTest: NSPoint::new(1.0, 1.0)];
            assert_eq!(
                inside, sw,
                "a click inside the padded hit rect must reach the switch"
            );
            let outside: *mut AnyObject = msg_send![sw, hitTest: NSPoint::new(-200.0, -200.0)];
            assert!(
                outside.is_null(),
                "a click far outside must miss the switch"
            );
            release_obj(sw);

            // Select focus: become/resignFirstResponder run the super call and must not crash. The
            // open/close focus behavior needs a real window on the AppKit main thread, so it is
            // asserted by the in-app settings smoke (`--smoke-settings-layout`); creating a window
            // here throws, because this test thread is not the main thread.
            let popup = super::make_popup(0.0, 0.0, 200.0, 32.0, &["A", "B"], 0);
            let _: bool = msg_send![popup, becomeFirstResponder];
            let _: bool = msg_send![popup, resignFirstResponder];
            release_obj(popup);

            // Switch focus: the 2pt accent ring is drawn on the track layer while focused.
            let sw = super::make_switch(200.0, 0.0, 52.0, false);
            let layer: *mut AnyObject = msg_send![sw, layer];
            let _: bool = msg_send![sw, becomeFirstResponder];
            let focused_width: f64 = msg_send![layer, borderWidth];
            assert_eq!(
                focused_width, 2.0,
                "switch focus must draw the 2pt inset ring"
            );
            let _: bool = msg_send![sw, resignFirstResponder];
            let idle_width: f64 = msg_send![layer, borderWidth];
            assert_eq!(idle_width, 0.0, "switch blur must clear the ring");
            release_obj(sw);

            // Action-button focus: idle 1pt card_border swaps to the 2pt accent ring and back.
            let button = super::SettingsButton::action(
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(120.0, 32.0)),
                "OK",
                std::ptr::null_mut(),
                sel!(noop:),
                super::SettingsButtonRole::Action,
            );
            let layer: *mut AnyObject = msg_send![button, layer];
            let idle_before: f64 = msg_send![layer, borderWidth];
            assert_eq!(idle_before, 1.0);
            let _: bool = msg_send![button, becomeFirstResponder];
            let focused_width: f64 = msg_send![layer, borderWidth];
            assert_eq!(
                focused_width, 2.0,
                "button focus must draw the 2pt inset ring"
            );
            let _: bool = msg_send![button, resignFirstResponder];
            let idle_after: f64 = msg_send![layer, borderWidth];
            assert_eq!(idle_after, 1.0, "button blur must restore the idle border");
            release_obj(button);

            // Text-field focus ring: the shared delegate toggles the 2pt accent border.
            let field = super::make_text_input(0.0, 0.0, 200.0, 32.0, "x");
            let name = crate::ffi::make_nsstring("NSControlTextDidBeginEditingNotification");
            let note: *mut AnyObject = msg_send![
                class!(NSNotification),
                notificationWithName: name,
                object: field
            ];
            crate::ffi::CFRelease(name as *const c_void);
            let delegate: *mut AnyObject = msg_send![field, delegate];
            assert!(
                !delegate.is_null(),
                "the field must have the focus-ring delegate"
            );
            let _: () = msg_send![delegate, controlTextDidBeginEditing: note];
            let layer: *mut AnyObject = msg_send![field, layer];
            let focused_width: f64 = msg_send![layer, borderWidth];
            assert_eq!(focused_width, 2.0, "focus must draw the 2pt inset ring");
            let _: () = msg_send![delegate, controlTextDidEndEditing: note];
            let idle_width: f64 = msg_send![layer, borderWidth];
            assert_eq!(idle_width, 0.0, "losing focus must clear the ring");
            release_obj(field);
        }
    }

    #[test]
    fn switch_pill_radii_fit_their_layer_bounds() {
        assert_eq!(html_switch_track_radius(), HTML_SWITCH_H / 2.0);
        assert_eq!(html_switch_knob_radius(), HTML_SWITCH_KNOB_D / 2.0);
    }

    #[test]
    fn switch_hit_target_meets_the_documented_minimum_size() {
        // The visible track stays 38x22 (design-style §9); the click target is padded to at
        // least 28x28 (§10) and stays centred on the track.
        let track = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(HTML_SWITCH_W, HTML_SWITCH_H),
        );
        let hit = switch_hit_bounds(track);
        assert!(hit.size.width >= 28.0 && hit.size.height >= 28.0);
        assert_eq!(hit.size.width, HTML_SWITCH_W);
        assert_eq!(hit.origin.x, track.origin.x);
        assert_eq!(hit.origin.y, (HTML_SWITCH_H - hit.size.height) / 2.0);
    }

    #[test]
    fn switch_hit_target_never_shrinks_a_larger_track() {
        let track = NSRect::new(NSPoint::new(4.0, 8.0), NSSize::new(40.0, 30.0));
        assert_eq!(switch_hit_bounds(track), track);
    }

    #[test]
    fn switch_keeps_its_original_spring_and_press_timings() {
        assert_eq!(super::html_switch_spring_duration(0.1), 0.18);
        assert_eq!(super::html_switch_spring_duration(0.31), 0.31);
        assert_eq!(super::HTML_SWITCH_PRESS_DURATION, 0.22);
    }

    #[test]
    fn text_field_editor_vertical_inset_tracks_the_cell_draw_rect() {
        for (height, expected) in [(32.0, 7.0), (34.0, 8.0), (40.0, 11.0)] {
            let bounds = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(200.0, height));
            assert_eq!(
                super::centered_text_field_editor_vertical_inset(bounds),
                expected
            );
        }
    }

    #[test]
    fn layout_rect_predicates_catch_crossing_rows_and_escape() {
        let page = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(400.0, 300.0));
        let first = NSRect::new(NSPoint::new(20.0, 100.0), NSSize::new(160.0, 34.0));
        let second = NSRect::new(NSPoint::new(20.0, 130.0), NSSize::new(160.0, 34.0));
        let outside = NSRect::new(NSPoint::new(20.0, 280.0), NSSize::new(160.0, 34.0));
        assert!(rects_overlap(first, second, 0.5));
        assert!(!rects_overlap(first, outside, 0.5));
        assert!(rect_inside(page, first, 0.0));
        assert!(!rect_inside(page, outside, 0.0));
    }

    #[test]
    fn row_divider_runs_full_bleed_across_its_card() {
        let card = NSRect::new(NSPoint::new(12.0, 0.0), NSSize::new(520.0, 300.0));
        let y = 124.0;
        let divider = super::settings_row_separator_frame(card.origin.x, y, card.size.width);
        assert_eq!(divider.origin.x, card.origin.x);
        assert_eq!(divider.origin.y, y);
        assert_eq!(divider.size.width, card.size.width);
        assert_eq!(
            divider.origin.x + divider.size.width,
            card.origin.x + card.size.width
        );
    }

    #[test]
    fn sidebar_hover_uses_the_semantic_palette_role() {
        for palette in [
            crate::theme::ui_palette_for_mode(false),
            crate::theme::ui_palette_for_mode(true),
        ] {
            assert_eq!(sidebar_hover_fill(palette), palette.hover_bg);
        }
    }
}
