//! 设置窗口 · 控件构造 helper:按钮/文本框/开关/滑杆/弹出自定义类与卡片、行布局 builder。
//! Control builders: button/text-field/switch/slider/popup custom classes plus card and row layout builders.

use super::*;

/// 设置控件标题并释放临时 NSString。
/// Set a control's title and release the temporary NSString.
pub(super) unsafe fn set_control_title(obj: *mut AnyObject, title: &str) {
    let ns = make_nsstring(title);
    let _: () = msg_send![obj, setTitle: ns];
    CFRelease(ns as *const c_void);
}

pub(super) fn settings_palette() -> UiPalette {
    ui_palette()
}

/// Map legacy HTML reference colors to the corresponding role in the active palette. Keeping
/// this compatibility layer lets the many settings controls share one dark/light implementation
/// without changing their layout-specific call sites.
/// 将旧版 HTML 参考色映射到当前主题的调色板角色,让现有设置控件共享明暗主题实现。
pub(super) fn themed_settings_color(hex: u32) -> u32 {
    let p = settings_palette();
    if !p.dark {
        return hex;
    }
    match hex {
        0xFFFFFFAD | 0x7676801F | 0x7676801E => p.button_bg,
        0xFFFFFFC7 => p.footer_button_bg,
        0x76768024 | 0x7676802B => p.hover_bg,
        0x0A84FFFF => p.accent,
        0x0077EDFF => p.accent_hover,
        0xFFFFFFFF | 0x2E2E2EFF | 0x2C2C30FF | 0x44444AFF => p.button_text,
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
        let _: () = msg_send![layer, setCornerRadius: 8.0f64];
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
            objc_registerClassPair(cls);
            HtmlActionButtonClass(cls)
        })
        .0
}

pub(super) extern "C" fn html_action_button_mouse_entered(
    this: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) {
    unsafe {
        let button = this as *mut AnyObject;
        let tag: isize = msg_send![button, tag];
        let hover = match tag {
            -2 => 0x0077EDFFu32, // HTML footer `.ok:hover`
            -1 => 0x76768024u32, // HTML footer `button:hover`
            _ => 0x7676802Bu32,  // HTML small/tiny/full action hover
        };
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
        let normal = match tag {
            -2 => 0x0A84FFFFu32,
            -1 => 0xFFFFFFC7u32,
            -3 => 0xFFFFFFADu32, // HTML `.full-action` normal background
            _ => 0xFFFFFFADu32,
        };
        let layer: *mut AnyObject = msg_send![button, layer];
        if !layer.is_null() {
            layer_set_background(
                layer,
                crate::ffi::hex_to_cg_color(themed_settings_color(normal)),
            );
        }
    }
}

/// 创建设置窗口统一使用的原生圆角操作按钮;调用方负责尺寸、自适应和父视图归属。
/// Create the native rounded action button shared by Settings; callers own frame, autoresizing,
/// and parent-view placement.
pub(super) unsafe fn make_settings_action_button(
    frame: NSRect,
    title: &str,
    target: *mut AnyObject,
    action: Sel,
) -> *mut AnyObject {
    let button: *mut AnyObject = msg_send![html_action_button_class(), alloc];
    let button: *mut AnyObject = msg_send![button, initWithFrame: frame];
    set_control_title(button, title);
    let _: () = msg_send![button, setControlSize: 0isize]; // NSControlSizeRegular
                                                           // HTML .small-btn / footer buttons: translucent white surface with a hairline border.
    style_html_button(button, 0xFFFFFFADu32, 0x2E2E2EFFu32);
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

pub(super) struct ExternalLinkButtonClass(*mut AnyObject);
unsafe impl Send for ExternalLinkButtonClass {}
unsafe impl Sync for ExternalLinkButtonClass {}

pub(super) static EXTERNAL_LINK_BUTTON_CLASS: OnceLock<ExternalLinkButtonClass> = OnceLock::new();
pub(super) static SIDEBAR_BUTTON_CLASS: OnceLock<SidebarButtonClass> = OnceLock::new();
pub(super) static SIDEBAR_SELECTED: AtomicUsize = AtomicUsize::new(0);
pub(super) static SIDEBAR_TITLE_LABELS: LazyLock<Mutex<HashMap<usize, ObjPtr>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
pub(super) static SIDEBAR_ICON_VIEWS: LazyLock<Mutex<HashMap<usize, ObjPtr>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) extern "C" fn external_link_mouse_entered(
    this: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) {
    unsafe {
        let color: *mut AnyObject = msg_send![class!(NSColor), systemBlueColor];
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
        let color: *mut AnyObject = msg_send![class!(NSColor), linkColor];
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

pub(super) extern "C" fn sidebar_button_mouse_entered(
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
        let layer: *mut AnyObject = msg_send![button, layer];
        if !layer.is_null() {
            layer_set_background(
                layer,
                crate::ffi::hex_to_cg_color(settings_palette().hover_bg),
            );
        }
    }
}

pub(super) extern "C" fn sidebar_button_mouse_exited(
    this: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) {
    unsafe {
        let button = this as *mut AnyObject;
        let layer: *mut AnyObject = msg_send![button, layer];
        if !layer.is_null() {
            layer_set_background(layer, crate::ffi::hex_to_cg_color(0x00000000u32));
        }
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

pub(super) struct AboutHeaderClickViewClass(*mut AnyObject);
unsafe impl Send for AboutHeaderClickViewClass {}
unsafe impl Sync for AboutHeaderClickViewClass {}

pub(super) static ABOUT_HEADER_CLICK_VIEW_CLASS: OnceLock<AboutHeaderClickViewClass> =
    OnceLock::new();

// bundle_info_string 已统一到 ffi.rs / bundle_info_string now lives in ffi.rs

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

/// 统计 About 头部点击，连续五次后显示 bundle 的 build number。
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
        let ui = SETTINGS_UI.lock().unwrap();
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
    }
}

pub(super) extern "C" fn about_header_click_view_mouse_down(
    _self: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) {
    on_about_header_click(std::ptr::null_mut(), sel!(mouseDown:), std::ptr::null_mut());
}

/// 用一个数值/字符串填进文本框,并释放临时 NSString。
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
    static CELL_CLASS: OnceLock<ObjPtr> = OnceLock::new();
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
            // selectWithFrame:... 实际只有 view、editor、delegate 三个对象参数。
            // The selector has exactly three object parameters: view, editor, and delegate.
            let select_types = CString::new("v@:{CGRect=dddd}@@@qq").unwrap();
            class_addMethod(
                cls,
                sel!(selectWithFrame:inView:editor:delegate:start:length:),
                centered_text_field_cell_select as *mut c_void,
                select_types.as_ptr(),
            );
            // AppKit 返回配置后的 NSText；返回值编码和 IMP ABI 必须保持一致。
            // AppKit returns the configured NSText; its type encoding must match the IMP ABI.
            let editor_types = CString::new("@@:@").unwrap();
            class_addMethod(
                cls,
                sel!(setUpFieldEditorAttributes:),
                centered_text_field_cell_setup_editor as *mut c_void,
                editor_types.as_ptr(),
            );
            objc_registerClassPair(cls);
            ObjPtr(cls)
        })
        .0
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
            // The field editor's glyph baseline sits about one point above the cell's normal
            // drawing baseline; add one point of vertical inset so edit and display states line up.
            let _: () = msg_send![editor, setTextContainerInset: NSSize::new(8.0, 8.0)];
        }
        configured_editor
    }
}

/// 可编辑文本框(alloc +1,由调用方持有或交给父视图后 release)。
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
    // The HTML input has no native focus ring; keep the caret while removing AppKit's
    // blue outline that otherwise appears around a borderless NSTextField when editing.
    let _: () = msg_send![field, setFocusRingType: 1isize]; // NSFocusRingTypeNone
                                                            // Treat the value as a single line so AppKit centers its baseline in the 34pt row,
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
    let field_text = crate::ffi::hex_to_ns_color(settings_palette().primary_text);
    let _: () = msg_send![field, setTextColor: field_text];
    let _: () = msg_send![field, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![field, layer];
    if !layer.is_null() {
        layer_set_background(
            layer,
            crate::ffi::hex_to_cg_color(settings_palette().field_bg),
        );
        let _: () = msg_send![layer, setCornerRadius: 9.0f64];
        let _: () = msg_send![layer, setMasksToBounds: true];
    }
    field
}

/// 下拉选择控件(alloc +1)。
/// Pop-up button (alloc +1).
pub(super) unsafe fn make_popup(
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    items: &[&str],
    selected: usize,
) -> *mut AnyObject {
    let popup: *mut AnyObject = msg_send![class!(NSPopUpButton), alloc];
    let popup: *mut AnyObject = msg_send![popup, initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h)), pullsDown: false];
    for &item in items {
        let ns = make_nsstring(item);
        let _: () = msg_send![popup, addItemWithTitle: ns];
        CFRelease(ns as *const c_void);
    }
    let _: () = msg_send![popup, selectItemAtIndex: selected as isize];
    let _: () = msg_send![popup, setBezelStyle: 0isize];
    let _: () = msg_send![popup, setControlSize: 0isize]; // Regular
    let _: () = msg_send![popup, setBordered: false];
    let palette = settings_palette();
    let _: () = msg_send![popup, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![popup, layer];
    if !layer.is_null() {
        layer_set_background(layer, crate::ffi::hex_to_cg_color(palette.field_bg));
        crate::ffi::layer_set_border(layer, crate::ffi::hex_to_cg_color(palette.card_border));
        let _: () = msg_send![layer, setBorderWidth: 1.0f64];
        let _: () = msg_send![layer, setCornerRadius: 9.0f64];
        let _: () = msg_send![layer, setMasksToBounds: true];
    }
    let tint = crate::ffi::hex_to_ns_color(palette.primary_text);
    let _: () = msg_send![popup, setContentTintColor: tint];
    let cell: *mut AnyObject = msg_send![popup, cell];
    if !cell.is_null() && msg_send![cell, respondsToSelector: sel!(setTextColor:)] {
        let _: () = msg_send![cell, setTextColor: tint];
    }
    popup
}

pub(super) const HTML_SWITCH_W: f64 = 38.0;
pub(super) const HTML_SWITCH_H: f64 = 22.0;
pub(super) const HTML_SWITCH_KNOB_D: f64 = 18.0;
// Keep switches on the same trailing edge as popup fields in the settings column.
// 开关与设置列中的下拉框共用同一条右侧边界。
pub(super) const HTML_SWITCH_TRAILING_INSET: f64 = 0.0;

pub(super) struct HtmlSwitchClass(*mut AnyObject);
unsafe impl Send for HtmlSwitchClass {}
unsafe impl Sync for HtmlSwitchClass {}

pub(super) static HTML_SWITCH_CLASS: OnceLock<HtmlSwitchClass> = OnceLock::new();

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
            0x0A84FF73
        }
    } else if enabled {
        if palette.dark {
            0x636366FF
        } else {
            0xC7C7CCFF
        }
    } else {
        if palette.dark {
            0x63636673
        } else {
            0xC7C7CC73
        }
    };
    crate::ffi::layer_set_background(layer, crate::ffi::hex_to_cg_color(track_hex));
    let _: () = msg_send![layer, setCornerRadius: HTML_SWITCH_H / 2.0];
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
        crate::ffi::layer_set_background(
            knob,
            crate::ffi::hex_to_cg_color(if palette.dark { 0xF5F5F7F5 } else { 0xFFFFFFF5 }),
        );
        let _: () = msg_send![knob, setCornerRadius: HTML_SWITCH_KNOB_D / 2.0];
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
        let key_path = make_nsstring("position.x");
        let animation: *mut AnyObject = msg_send![
            class!(CABasicAnimation),
            animationWithKeyPath: key_path
        ];
        CFRelease(key_path as *const c_void);
        let from_value: *mut AnyObject =
            msg_send![class!(NSNumber), numberWithDouble: from_x + HTML_SWITCH_KNOB_D / 2.0];
        let to_value: *mut AnyObject =
            msg_send![class!(NSNumber), numberWithDouble: to_x + HTML_SWITCH_KNOB_D / 2.0];
        let _: () = msg_send![animation, setFromValue: from_value];
        let _: () = msg_send![animation, setToValue: to_value];
        let _: () = msg_send![animation, setDuration: 0.18f64];
        let animation_key = make_nsstring("html-switch-position");
        let _: () = msg_send![knob, addAnimation: animation, forKey: animation_key];
        CFRelease(animation_key as *const c_void);
    }
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

/// The switch is rendered entirely by its layer, so toggle the state and dispatch the action
/// explicitly instead of relying on the hidden NSButtonCell drawing/tracking state.
/// 自绘开关由 Layer 完成视觉呈现,点击时显式切换状态并分发 Action,不依赖隐藏的 Cell 跟踪状态。
pub(super) extern "C" fn html_switch_mouse_down(this: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    unsafe {
        let button = this as *mut AnyObject;
        let enabled: bool = msg_send![button, isEnabled];
        if !enabled {
            return;
        }

        let current: isize = msg_send![button, state];
        let next = if current == 0 { 1isize } else { 0isize };
        let _: () = msg_send![button, setState: next];

        // Most settings switches only need their state collected when OK is pressed. The two
        // switches with live behavior have an explicit target/action; dispatch those here.
        // 大多数设置开关在点击 OK 时统一读取状态,只有需要实时生效的开关绑定了 target/action。
        let target: *mut AnyObject = msg_send![button, target];
        if !target.is_null() {
            let action: Sel = msg_send![button, action];
            let _: bool = msg_send![button, sendAction: action, to: target];
        }
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
            let mouse_types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(mouseDown:),
                html_switch_mouse_down as *mut c_void,
                mouse_types.as_ptr(),
            );
            objc_registerClassPair(cls);
            HtmlSwitchClass(cls)
        })
        .0
}

/// HTML reference switch implemented as a custom-drawn NSButton.
/// alloc +1; caller releases after adding to the parent view.
/// 参数 right_x = 控件列的右边界;开关与同一行的下拉框右边缘对齐。
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

/// 整数滑块(NSSlider, min..=max, step 1)。alloc +1,加入父视图后由调用方 release。
/// Integer slider (NSSlider, min..=max, step 1). alloc +1; caller releases after adding to parent.
pub(super) unsafe fn make_slider(
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    min: i64,
    max: i64,
    value: i64,
) -> *mut AnyObject {
    let slider: *mut AnyObject = msg_send![class!(NSSlider), alloc];
    let slider: *mut AnyObject =
        msg_send![slider, initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))];
    let _: () = msg_send![slider, setMinValue: min as f64];
    let _: () = msg_send![slider, setMaxValue: max as f64];
    // 整数步进:1 格 = 1 个单位(线性 Mouse By Lines 滑块同款:0...10 step 1)。
    // Integer steps: 1 tick = 1 unit (same as LinearMouse's By Lines slider: 0...10 step 1).
    let _: () = msg_send![slider, setNumberOfTickMarks: (max - min + 1) as isize];
    let _: () = msg_send![slider, setAllowsTickMarkValuesOnly: true];
    let _: () = msg_send![slider, setIntegerValue: value];
    slider
}

/// 设侧边栏按钮标题为 attributed title:未选中用 labelColor,选中用系统强调色。
/// Set the sidebar button title as an attributed title, using the normal label color when
/// unselected and the system accent color when selected.
pub(super) unsafe fn set_sidebar_title(btn: *mut AnyObject, title: &str, selected: bool) {
    let font: *mut AnyObject = if selected {
        msg_send![class!(NSFont), boldSystemFontOfSize: 13.5f64]
    } else {
        msg_send![class!(NSFont), messageFontOfSize: 13.5f64]
    };
    let color: *mut AnyObject = if selected {
        msg_send![class!(NSColor), controlAccentColor]
    } else {
        msg_send![class!(NSColor), labelColor]
    };
    let attrs: *mut AnyObject = msg_send![class!(NSMutableDictionary), alloc];
    let attrs: *mut AnyObject = msg_send![attrs, init];
    let k_font = make_nsstring("NSFont");
    let _: () = msg_send![attrs, setObject: font, forKey: k_font];
    CFRelease(k_font as *const c_void);
    let k_color = make_nsstring("NSColor");
    let _: () = msg_send![attrs, setObject: color, forKey: k_color];
    CFRelease(k_color as *const c_void);
    let title_ns = make_nsstring(title);
    let attr_str: *mut AnyObject = msg_send![class!(NSAttributedString), alloc];
    let attr_str: *mut AnyObject = msg_send![attr_str, initWithString: title_ns, attributes: attrs];
    let label = SIDEBAR_TITLE_LABELS
        .lock()
        .unwrap()
        .get(&(btn as usize))
        .map(|p| p.0)
        .unwrap_or(btn);
    let _: () = msg_send![label, setStringValue: title_ns];
    let _: () = msg_send![label, setAttributedStringValue: attr_str];
    if let Some(icon) = SIDEBAR_ICON_VIEWS
        .lock()
        .unwrap()
        .get(&(btn as usize))
        .map(|p| p.0)
    {
        let _: () = msg_send![icon, setContentTintColor: color];
    }
    let _: () = msg_send![btn, setContentTintColor: color];
    CFRelease(title_ns as *const c_void);
    release_obj(attr_str);
    release_obj(attrs);
}

/// 侧边栏按钮(borderless NSButton,左对齐图标+文字,tag 区分页)。
/// Sidebar button (borderless NSButton; left-aligned icon + title; tag selects the page).
pub(super) unsafe fn make_sidebar_button(
    parent: *mut AnyObject,
    target: *mut AnyObject,
    title: &str,
    tag: isize,
    x: f64,
    y: f64,
    w: f64,
) -> *mut AnyObject {
    let h = 38.0;
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
        let _: () = msg_send![btn_layer, setCornerRadius: 10.0f64];
        let _: () = msg_send![btn_layer, setMasksToBounds: true];
    }
    let _: () = msg_send![btn, setTag: tag];
    let symbol = match tag {
        0 => "gearshape",
        1 => "rectangle.on.rectangle",
        2 => "computermouse",
        3 => "doc.on.clipboard",
        4 => "info.circle",
        _ => "circle",
    };
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
            // SF Symbols have a little optical padding at the bottom; lift the image view
            // so the glyph's visual center shares the text baseline in the 38pt row.
            initWithFrame: NSRect::new(NSPoint::new(15.5, 10.0), NSSize::new(17.0, 17.0))
        ];
        let _: () = msg_send![icon_view, setImage: image];
        let _: () = msg_send![icon_view, setImageScaling: 3isize];
        let _: () = msg_send![icon_view, setEditable: false];
        let _: () = msg_send![icon_view, setWantsLayer: false];
        let _: () = msg_send![btn, addSubview: icon_view];
        SIDEBAR_ICON_VIEWS
            .lock()
            .unwrap()
            .insert(btn as usize, ObjPtr(icon_view));
        release_obj(icon_view);
    }
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let label: *mut AnyObject = msg_send![
        label,
        initWithFrame: NSRect::new(
            NSPoint::new(44.5, 11.0),
            NSSize::new((w - 51.0).max(1.0), 24.0),
        )
    ];
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let label_color = crate::ffi::hex_to_ns_color(settings_palette().primary_text);
    let _: () = msg_send![label, setTextColor: label_color];
    let _: () = msg_send![label, setSelectable: false];
    let _: () = msg_send![label, setAlignment: 0isize];
    let _: () = msg_send![label, setUsesSingleLineMode: true];
    let _: () = msg_send![label, setEnabled: false];
    let _: () = msg_send![btn, addSubview: label];
    SIDEBAR_TITLE_LABELS
        .lock()
        .unwrap()
        .insert(btn as usize, ObjPtr(label));
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
    // 自适应:贴顶、贴左、固定尺寸 / adaptive: top- and left-anchored, fixed size
    let _: () = msg_send![btn, setAutoresizingMask: 12u64];
    let _: () = msg_send![btn, setTarget: target];
    let _: () = msg_send![btn, setAction: sel!(handleSettingsSidebar:)];
    let _: () = msg_send![parent, addSubview: btn];
    release_obj(btn);
    btn
}

/// 区块标题(加粗 label),加入父视图后 release。
/// Bold section header label; released after being added to the parent.
pub(super) unsafe fn add_header(parent: *mut AnyObject, text: &str, x: f64, y: f64, w: f64) {
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let label: *mut AnyObject =
        msg_send![label, initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, 20.0))];
    let section_text = text.to_uppercase();
    let ns = make_nsstring(&section_text);
    let _: () = msg_send![label, setStringValue: ns];
    CFRelease(ns as *const c_void);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let font: *mut AnyObject = msg_send![class!(NSFont), boldSystemFontOfSize: 11.0f64];
    let _: () = msg_send![label, setFont: font];
    let color: *mut AnyObject = msg_send![class!(NSColor), secondaryLabelColor];
    let _: () = msg_send![label, setTextColor: color];
    // 自适应:宽度随父视图拉伸、顶部锚定(MinYMargin)。autoresizing = WidthSizable | MinYMargin = 2|8 = 10。
    // Adaptive: stretch width with the parent, stay top-anchored (MinYMargin).
    let _: () = msg_send![label, setAutoresizingMask: 10u64];
    let _: () = msg_send![parent, addSubview: label];
    release_obj(label);
}

/// Add a page title matching the HTML redesign's large, tight heading.
pub(super) unsafe fn add_page_title(parent: *mut AnyObject, text: &str, x: f64, y: f64, w: f64) {
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let label: *mut AnyObject =
        msg_send![label, initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, 32.0))];
    set_field(label, text);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let font: *mut AnyObject = msg_send![class!(NSFont), boldSystemFontOfSize: 25.0f64];
    let _: () = msg_send![label, setFont: font];
    let color: *mut AnyObject = msg_send![class!(NSColor), labelColor];
    let _: () = msg_send![label, setTextColor: color];
    let _: () = msg_send![label, setAutoresizingMask: 10u64];
    let _: () = msg_send![parent, addSubview: label];
    release_obj(label);
}

/// Build the About header icon directly from the source PNG so AppKit does not reinterpret the
/// bundled `.icns` representation.
pub(super) unsafe fn add_about_app_icon(parent: *mut AnyObject, x: f64, y: f64) {
    let icon: *mut AnyObject = msg_send![class!(NSView), alloc];
    let icon: *mut AnyObject = msg_send![
        icon,
        initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(58.0, 58.0))
    ];

    // 直接使用 PNG，避免 NSApplicationIcon/.icns 在深色背景下产生额外的系统图标边缘。
    // Use the PNG directly so NSApplicationIcon/.icns cannot add a system-rendered edge on dark backgrounds.
    let image = crate::load_embedded_app_icon();
    if !image.is_null() {
        let image_view: *mut AnyObject = msg_send![class!(NSImageView), alloc];
        let image_view: *mut AnyObject = msg_send![
            image_view,
            // Let the source PNG occupy the whole slot; it already contains its own rounded silhouette.
            // 让源 PNG 占满图标槽位；它本身已经包含圆角底座。
            initWithFrame: NSRect::new(NSPoint::new(-2.0, -2.0), NSSize::new(62.0, 62.0))
        ];
        let _: () = msg_send![image_view, setImage: image];
        let _: () = msg_send![image_view, setImageScaling: 3isize];
        let _: () = msg_send![image_view, setImageFrameStyle: 0isize];
        let _: () = msg_send![icon, addSubview: image_view];
        release_obj(image_view);
        release_obj(image);
    }
    let _: () = msg_send![parent, addSubview: icon];
    release_obj(icon);
}

pub(super) const SETTINGS_CARD_SHADOW_INSET: f64 = 18.0;

/// Return the origin that centers a control inside a card's vertical bounds.
/// 根据卡片上下边界计算控件的垂直居中起点。
pub(super) fn centered_control_y(card_bottom: f64, card_top: f64, control_h: f64) -> f64 {
    card_bottom + (card_top - card_bottom - control_h) / 2.0
}

/// Draw the settings card shadow into pixels owned by the shadow view itself. This keeps the
/// blur inside the view's expanded frame instead of relying on a CALayer shadow crossing the
/// AppKit scroll/document hierarchy.
/// 在阴影视图自身的像素范围内绘制设置卡片阴影。这样模糊区域位于扩大的视图边界内,
/// 不再依赖 CALayer 阴影穿过 AppKit 的滚动/文档视图层级。
pub(super) extern "C" fn settings_card_shadow_draw_rect(
    _self: *mut c_void,
    _cmd: Sel,
    _rect: NSRect,
) {
    unsafe {
        let view = _self as *mut AnyObject;
        let bounds: NSRect = msg_send![view, bounds];
        let card_rect = NSRect::new(
            NSPoint::new(SETTINGS_CARD_SHADOW_INSET, SETTINGS_CARD_SHADOW_INSET),
            NSSize::new(
                (bounds.size.width - SETTINGS_CARD_SHADOW_INSET * 2.0).max(1.0),
                (bounds.size.height - SETTINGS_CARD_SHADOW_INSET * 2.0).max(1.0),
            ),
        );
        let shadow: *mut AnyObject = msg_send![class!(NSShadow), alloc];
        let shadow: *mut AnyObject = msg_send![shadow, init];
        let shadow_color = crate::ffi::hex_to_ns_color(settings_palette().shadow);
        let _: () = msg_send![shadow, setShadowColor: shadow_color];
        let _: () = msg_send![shadow, setShadowBlurRadius: 8.0f64];
        let _: () = msg_send![shadow, setShadowOffset: NSSize::new(0.0, -1.0)];
        let _: () = msg_send![shadow, set];

        let path: *mut AnyObject = msg_send![
            class!(NSBezierPath),
            bezierPathWithRoundedRect: card_rect,
            xRadius: 14.0f64,
            yRadius: 14.0f64
        ];
        let fill = crate::ffi::hex_to_ns_color(settings_palette().card_bg);
        let _: () = msg_send![fill, set];
        let _: () = msg_send![path, fill];
        release_obj(shadow);
    }
}

pub(super) extern "C" fn settings_card_shadow_hit_test(
    _self: *mut c_void,
    _cmd: Sel,
    _point: NSPoint,
) -> *mut AnyObject {
    std::ptr::null_mut()
}

pub(super) fn settings_card_shadow_view_class() -> *mut AnyObject {
    static CLASS: OnceLock<usize> = OnceLock::new();
    *CLASS.get_or_init(|| unsafe {
        let name = CString::new("OhMyTabSettingsCardShadowView").unwrap();
        let superclass = class!(NSView) as *const _ as *mut AnyObject;
        let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
        let types_draw = CString::new("v@:{CGRect={CGPoint=dd}{CGSize=dd}}").unwrap();
        class_addMethod(
            cls,
            sel!(drawRect:),
            settings_card_shadow_draw_rect as *mut c_void,
            types_draw.as_ptr(),
        );
        let types_hit = CString::new("@@:{CGPoint=dd}").unwrap();
        class_addMethod(
            cls,
            sel!(hitTest:),
            settings_card_shadow_hit_test as *mut c_void,
            types_hit.as_ptr(),
        );
        objc_registerClassPair(cls);
        cls as usize
    }) as *mut AnyObject
}

/// Add a grouped card behind a section, matching the HTML redesign's light card surface.
pub(super) unsafe fn add_settings_card(
    parent: *mut AnyObject,
    frame: NSRect,
) -> (*mut AnyObject, *mut AnyObject) {
    if frame.size.width <= 0.0 || frame.size.height <= 0.0 {
        return (std::ptr::null_mut(), std::ptr::null_mut());
    }
    let card: *mut AnyObject = msg_send![class!(NSView), alloc];
    let card: *mut AnyObject = msg_send![card, initWithFrame: frame];
    let _: () = msg_send![card, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![card, layer];
    if !layer.is_null() {
        let palette = settings_palette();
        layer_set_background(layer, crate::ffi::hex_to_cg_color(palette.card_bg));
        let _: () = msg_send![layer, setCornerRadius: 14.0f64];
        // Keep the outer shadow visible. The card has no child content that needs clipping.
        let _: () = msg_send![layer, setMasksToBounds: false];
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

    // Put the self-contained shadow behind the card. Its expanded frame provides enough room
    // for the blur, while hitTest: keeps the shadow outside the card non-interactive.
    // 将自包含的阴影视图放在卡片下方。扩大的边界为模糊留出空间,hitTest: 保证卡片外的
    // 阴影区域不会拦截交互。
    let shadow_inset = SETTINGS_CARD_SHADOW_INSET;
    let shadow: *mut AnyObject = msg_send![settings_card_shadow_view_class(), alloc];
    let shadow: *mut AnyObject = msg_send![
        shadow,
        initWithFrame: NSRect::new(
            NSPoint::new(frame.origin.x - shadow_inset, frame.origin.y - shadow_inset),
            NSSize::new(
                frame.size.width + shadow_inset * 2.0,
                frame.size.height + shadow_inset * 2.0,
            ),
        )
    ];
    let _: () = msg_send![
        parent,
        addSubview: shadow,
        positioned: -1isize,
        relativeTo: card
    ];
    release_obj(shadow);

    release_obj(card);
    (card, shadow)
}

/// Draw the HTML `.row + .row` hairline inside a grouped card.
pub(super) unsafe fn add_row_separator(
    parent: *mut AnyObject,
    x: f64,
    y: f64,
    w: f64,
) -> *mut AnyObject {
    // Keep the hairline inside the card's rounded frame. Grouped cards are
    // inset by the same six points, so their row separators need that inset
    // as well instead of reaching the content pane edge.
    let line_x = x + 6.0;
    let line_w = (w - 12.0).max(1.0);
    let line: *mut AnyObject = msg_send![class!(NSView), alloc];
    let line: *mut AnyObject = msg_send![
        line,
        initWithFrame: NSRect::new(NSPoint::new(line_x, y), NSSize::new(line_w, 1.0))
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

/// 加一行:右对齐 label + 控件。控件由调用方创建并传入;加入父视图后 release,返回该控件指针。
/// Add a row: right-aligned label + control. The control is created by the caller;
/// it is released after being added to the parent. Returns the control pointer.
pub(super) unsafe fn add_row(
    parent: *mut AnyObject,
    label_x: f64,
    y: f64,
    label_w: f64,
    h: f64,
    label_text: &str,
    control: *mut AnyObject,
) -> *mut AnyObject {
    add_row_with_label(parent, label_x, y, label_w, h, label_text, control).1
}

/// 同 add_row,但额外返回 label 指针(供条件显隐等需要隐藏整行的场景)。
/// label/control 加入父视图后由父视图持有,release 后指针仍有效(与 add_row 返回 control
/// 的约定一致)。
///
/// Same as add_row, but also returns the label pointer (for cases that need to hide the whole
/// row, e.g. conditional visibility). The label/control are retained by the parent view after
/// addSubview, so the pointers remain valid after release (same convention as add_row's
/// returned control).
pub(super) unsafe fn add_row_with_label(
    parent: *mut AnyObject,
    label_x: f64,
    y: f64,
    label_w: f64,
    h: f64,
    label_text: &str,
    control: *mut AnyObject,
) -> (*mut AnyObject, *mut AnyObject) {
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let label: *mut AnyObject = msg_send![label, initWithFrame: NSRect::new(
        NSPoint::new(label_x, y),
        NSSize::new(label_w, (h - 12.0).max(1.0)),
    )];
    let ns = make_nsstring(label_text);
    let _: () = msg_send![label, setStringValue: ns];
    CFRelease(ns as *const c_void);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    // 左对齐:设置项标签贴在内容区左侧(NSTextAlignmentLeft = 0,arm64/x86_64 一致)。
    // Left-aligned: the row label hugs the content area's left edge (NSTextAlignmentLeft = 0,
    // identical on arm64 and x86_64).
    let _: () = msg_send![label, setAlignment: 0isize]; // NSTextAlignmentLeft
    let label_color = crate::ffi::hex_to_ns_color(settings_palette().primary_text);
    let _: () = msg_send![label, setTextColor: label_color];
    // 自适应:标签固定宽、顶部+左侧锚定(MinYMargin|MaxXMargin = 8|4 = 12)。
    // Adaptive: label keeps fixed width, stays top- and left-anchored.
    let _: () = msg_send![label, setAutoresizingMask: 12u64];
    let _: () = msg_send![parent, addSubview: label];
    release_obj(label);
    // 自适应:控件宽度随父视图拉伸、顶部锚定(WidthSizable|MinYMargin = 2|8 = 10)。
    // Adaptive: control stretches its width with the parent, stays top-anchored.
    let _: () = msg_send![control, setAutoresizingMask: 10u64];
    let _: () = msg_send![parent, addSubview: control];
    release_obj(control);
    (label, control)
}

/// Add a taller settings row with the two-level title/subtitle hierarchy used by the HTML
/// reference. The caller positions the native control inside the row; this helper only builds
/// the leading text stack and returns the control after attaching it.
#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn add_described_row(
    parent: *mut AnyObject,
    x: f64,
    y: f64,
    text_w: f64,
    row_h: f64,
    title: &str,
    subtitle: &str,
    control: *mut AnyObject,
) -> *mut AnyObject {
    let title_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let title_label: *mut AnyObject = msg_send![
        title_label,
        initWithFrame: NSRect::new(
            NSPoint::new(x, y + row_h - 27.0),
            NSSize::new(text_w, 18.0),
        )
    ];
    set_field(title_label, title);
    let _: () = msg_send![title_label, setBezeled: false];
    let _: () = msg_send![title_label, setDrawsBackground: false];
    let _: () = msg_send![title_label, setEditable: false];
    let title_color = crate::ffi::hex_to_ns_color(settings_palette().primary_text);
    let _: () = msg_send![title_label, setTextColor: title_color];
    let _: () = msg_send![title_label, setUsesSingleLineMode: true];
    let title_font: *mut AnyObject = msg_send![class!(NSFont), messageFontOfSize: 13.5f64];
    let _: () = msg_send![title_label, setFont: title_font];
    let _: () = msg_send![parent, addSubview: title_label];
    release_obj(title_label);

    let subtitle_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let subtitle_label: *mut AnyObject = msg_send![
        subtitle_label,
        initWithFrame: NSRect::new(
            NSPoint::new(x, y + 7.0),
            NSSize::new(text_w, 17.0),
        )
    ];
    set_field(subtitle_label, subtitle);
    let _: () = msg_send![subtitle_label, setBezeled: false];
    let _: () = msg_send![subtitle_label, setDrawsBackground: false];
    let _: () = msg_send![subtitle_label, setEditable: false];
    let _: () = msg_send![subtitle_label, setUsesSingleLineMode: true];
    let subtitle_font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 11.5f64];
    let _: () = msg_send![subtitle_label, setFont: subtitle_font];
    let subtitle_color = crate::ffi::hex_to_ns_color(settings_palette().secondary_text);
    let _: () = msg_send![subtitle_label, setTextColor: subtitle_color];
    let _: () = msg_send![parent, addSubview: subtitle_label];
    release_obj(subtitle_label);

    let _: () = msg_send![control, setAutoresizingMask: 10u64];
    let _: () = msg_send![parent, addSubview: control];
    release_obj(control);
    control
}

/// HTML `.row` at 54pt but single-line: the label is vertically centered and the control
/// right-aligned. Used for the Device/Scroll-mode rows that have no subtitle.
#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn add_tall_row(
    parent: *mut AnyObject,
    label_x: f64,
    y: f64,
    label_w: f64,
    label_text: &str,
    control: *mut AnyObject,
) -> (*mut AnyObject, *mut AnyObject) {
    let h = 54.0; // matched described_row_h
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let label: *mut AnyObject = msg_send![label, initWithFrame: NSRect::new(NSPoint::new(label_x, y + (h - 22.0) / 2.0), NSSize::new(label_w, 22.0))];
    let ns = make_nsstring(label_text);
    let _: () = msg_send![label, setStringValue: ns];
    CFRelease(ns as *const c_void);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let _: () = msg_send![label, setAlignment: 0isize]; // left
    let label_color = crate::ffi::hex_to_ns_color(settings_palette().primary_text);
    let _: () = msg_send![label, setTextColor: label_color];
    let font: *mut AnyObject = msg_send![class!(NSFont), messageFontOfSize: 13.5f64];
    let _: () = msg_send![label, setFont: font];
    let _: () = msg_send![label, setAutoresizingMask: 12u64];
    let _: () = msg_send![parent, addSubview: label];
    release_obj(label);
    let _: () = msg_send![control, setAutoresizingMask: 10u64];
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
        let _: () = msg_send![layer, setCornerRadius: 9.0f64];
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
    let document: *mut AnyObject = msg_send![
        document,
        initWithFrame: NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(frame.size.width, document_h),
        )
    ];
    let _: () = msg_send![document, setAutoresizingMask: 2u64];
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
