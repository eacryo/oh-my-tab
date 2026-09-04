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
            _ if tag >= 0 => 0x7676801Fu32, // mapping/edit compact action
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

/// Create a settings action button with a semantic normal/hover style. All buttons use the same
/// tracking area and dynamic AppKit subclass; the tag only selects the hover palette and remains
/// compatible with existing positive tags used by mapping rows.
/// 创建带语义常态/悬停样式的设置操作按钮。所有按钮共用 tracking area 和动态 AppKit 子类；
/// tag 只选择 hover 调色板，并兼容按键映射行已有的正数 tag。
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

pub(super) struct ExternalLinkButtonClass(*mut AnyObject);
unsafe impl Send for ExternalLinkButtonClass {}
unsafe impl Sync for ExternalLinkButtonClass {}

pub(super) static EXTERNAL_LINK_BUTTON_CLASS: OnceLock<ExternalLinkButtonClass> = OnceLock::new();
pub(super) static SIDEBAR_BUTTON_CLASS: OnceLock<SidebarButtonClass> = OnceLock::new();
pub(super) static SIDEBAR_SELECTED: AtomicUsize = AtomicUsize::new(0);
pub(super) static SIDEBAR_HOVERED: AtomicUsize = AtomicUsize::new(0);
pub(super) static SIDEBAR_HOVER_VISIBLE: AtomicBool = AtomicBool::new(false);
pub(super) static SIDEBAR_HOVER_HIGHLIGHT: LazyLock<Mutex<Option<ObjPtr>>> =
    LazyLock::new(|| Mutex::new(None));
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

pub(super) struct SidebarHoverTrackerClass(*mut AnyObject);
unsafe impl Send for SidebarHoverTrackerClass {}
unsafe impl Sync for SidebarHoverTrackerClass {}

pub(super) static SIDEBAR_HOVER_TRACKER_CLASS: OnceLock<SidebarHoverTrackerClass> = OnceLock::new();
pub(super) static SIDEBAR_HOVER_TRACKER: LazyLock<Mutex<Option<ObjPtr>>> =
    LazyLock::new(|| Mutex::new(None));

/// Return the shared hover view without duplicating its ownership logic at each event site.
/// 读取共享悬浮 view，避免每个事件回调重复处理指针状态。
unsafe fn sidebar_hover_highlight() -> *mut AnyObject {
    SIDEBAR_HOVER_HIGHLIGHT
        .lock()
        .unwrap()
        .as_ref()
        .map(|p| p.0)
        .unwrap_or(std::ptr::null_mut())
}

/// Check whether a sidebar button is the item currently carrying the hover surface.
/// 判断指定侧栏按钮是否正承载当前悬停背景。
pub(super) fn sidebar_button_is_hovered(button: *mut AnyObject) -> bool {
    !button.is_null() && SIDEBAR_HOVERED.load(Ordering::SeqCst) == button as usize
}

/// Clear hover state after a sidebar click has established the selected row.
/// 侧栏点击完成选中态切换后清理悬浮状态。
pub(super) unsafe fn clear_sidebar_hover() {
    SIDEBAR_HOVERED.store(0, Ordering::SeqCst);
    super::components::SettingsSidebar::hide_hover_highlight_immediately(sidebar_hover_highlight());
}

pub(super) extern "C" fn sidebar_hover_tracker_mouse_entered(
    _this: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) {
    // The row callbacks own the current item; the container callback only establishes the
    // boundary that decides when the shared hover surface may disappear.
    // 条目回调负责当前条目；容器回调只定义共享悬浮层何时可以消失的边界。
}

pub(super) extern "C" fn sidebar_hover_tracker_mouse_exited(
    _this: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) {
    unsafe {
        SIDEBAR_HOVERED.store(0, Ordering::SeqCst);
        let hover = sidebar_hover_highlight();
        super::components::SettingsSidebar::hide_hover_highlight(hover);
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

/// Track the whole row group so gaps between buttons do not start a hide animation.
/// 跟踪整个条目区域，避免按钮间隙触发隐藏动画。
pub(super) unsafe fn make_sidebar_hover_tracking(parent: *mut AnyObject, x: f64, y: f64, w: f64) {
    let tracker: *mut AnyObject = msg_send![sidebar_hover_tracker_class(), alloc];
    let tracker: *mut AnyObject = msg_send![tracker, init];
    let rows_h = 38.0 + 5.0 * 42.0;
    let tracking: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
    let tracking: *mut AnyObject = msg_send![
        tracking,
        initWithRect: NSRect::new(
            NSPoint::new(x, y - 5.0 * 42.0),
            NSSize::new(w, rows_h)
        ),
        options: 0x01u64 | 0x80u64,
        owner: tracker,
        userInfo: std::ptr::null::<AnyObject>()
    ];
    let _: () = msg_send![parent, addTrackingArea: tracking];
    release_obj(tracking);

    // NSTrackingArea does not provide ownership suitable for this raw-pointer registry; keep one
    // explicit +1 until the next sidebar is built, then release the previous tracker.
    // NSTrackingArea 不提供适合裸指针 registry 的所有权；显式保留一个 +1，重建侧栏时释放旧 tracker。
    if let Some(previous) = SIDEBAR_HOVER_TRACKER
        .lock()
        .unwrap()
        .replace(ObjPtr(tracker))
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
        let tag: isize = msg_send![button, tag];
        if tag >= 0 && tag as usize == SIDEBAR_SELECTED.load(Ordering::SeqCst) {
            SIDEBAR_HOVERED.store(0, Ordering::SeqCst);
            super::components::SettingsSidebar::hide_hover_highlight(sidebar_hover_highlight());
            return;
        }
        SIDEBAR_HOVERED.store(button as usize, Ordering::SeqCst);
        let hover = sidebar_hover_highlight();
        if !hover.is_null() {
            let frame: NSRect = msg_send![button, frame];
            super::components::SettingsSidebar::move_hover_highlight(hover, frame);
        }
        set_sidebar_hovered(button, true);
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
/// 构造可放入标准设置行的只读值文本。
pub(super) unsafe fn make_value_label(
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    value: &str,
) -> *mut AnyObject {
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let label: *mut AnyObject = msg_send![
        label,
        initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h.max(34.0)))
    ];
    set_field(label, value);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let _: () = msg_send![label, setSelectable: false];
    let _: () = msg_send![label, setUsesSingleLineMode: true];
    let _: () = msg_send![label, setLineBreakMode: 4isize]; // NSLineBreakByTruncatingTail
    let _: () = msg_send![label, setAlignment: -1isize]; // NSTextAlignmentNatural
    let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 13.5f64];
    let _: () = msg_send![label, setFont: font];
    let color: *mut AnyObject = msg_send![class!(NSColor), secondaryLabelColor];
    let _: () = msg_send![label, setTextColor: color];
    label
}

/// Build a read-only external-link control for a standard settings row.
/// 构造可放入标准设置行的只读外部链接控件。
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
    let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 13.5f64];
    let _: () = msg_send![link, setFont: font];
    let color: *mut AnyObject = msg_send![class!(NSColor), linkColor];
    let _: () = msg_send![link, setTextColor: color];
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
/// 构造一个显式指定尺寸、供菜单项和设置行共同复用的 SF Symbol 图像。
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
/// 构造设置行内使用的 SF Symbol 图标视图。
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

struct SettingsSelectState {
    items: Vec<String>,
    item_symbols: Vec<Option<String>>,
    selected: isize,
    panel: usize,
    open: bool,
}

static SETTINGS_SELECT_STATES: LazyLock<Mutex<HashMap<usize, SettingsSelectState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static SETTINGS_SELECT_ARROW_VIEWS: LazyLock<Mutex<HashMap<usize, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static ACTIVE_SETTINGS_SELECT: Mutex<Option<usize>> = Mutex::new(None);

struct SettingsSelectClass(*mut AnyObject);
unsafe impl Send for SettingsSelectClass {}
unsafe impl Sync for SettingsSelectClass {}

static SETTINGS_SELECT_CLASS: OnceLock<SettingsSelectClass> = OnceLock::new();

struct SettingsSelectItemClass(*mut AnyObject);
unsafe impl Send for SettingsSelectItemClass {}
unsafe impl Sync for SettingsSelectItemClass {}

static SETTINGS_SELECT_ITEM_CLASS: OnceLock<SettingsSelectItemClass> = OnceLock::new();

/// Clear runtime state before the settings window and its controls are destroyed.
/// 设置窗口及其控件销毁前清理运行时状态。
pub(super) fn clear_settings_select_registry() {
    SETTINGS_SELECT_STATES.lock().unwrap().clear();
    SETTINGS_SELECT_ARROW_VIEWS.lock().unwrap().clear();
    *ACTIVE_SETTINGS_SELECT.lock().unwrap() = None;
}

unsafe fn settings_select_set_title(button: *mut AnyObject, title: &str) {
    // NSButton has no reliable AppKit content-inset API. Keep the native button hit area and
    // add a small typographic inset to its title instead of shrinking the control frame.
    // NSButton 没有可靠的 AppKit 内容内边距 API；保留按钮点击区域，只给标题增加轻微字面内缩。
    let padded_title = format!("   {title}");
    let title_ns = make_nsstring(&padded_title);
    let _: () = msg_send![button, setTitle: title_ns];
    CFRelease(title_ns as *const c_void);
}

/// Select surfaces deliberately use opaque colors; only the surrounding settings window remains
/// translucent. 下拉框表面使用不透明颜色，只有外层设置窗口保留透明效果。
fn settings_select_surface_color(palette: UiPalette) -> u32 {
    if palette.dark {
        0x151515FF
    } else {
        0xFCFCFCFF
    }
}

fn settings_select_item_active_color(palette: UiPalette) -> u32 {
    if palette.dark {
        0x1C1C1CFF
    } else {
        0xF5F5F5FF
    }
}

/// Update the trigger surface and arrow without changing the selected value.
/// 更新触发器表面和箭头,但不改变选中值。
unsafe fn settings_select_apply_visual(button: *mut AnyObject, open: bool) {
    let (title, enabled) = SETTINGS_SELECT_STATES
        .lock()
        .unwrap()
        .get(&(button as usize))
        .map(|state| {
            let title = state
                .items
                .get(state.selected.max(0) as usize)
                .cloned()
                .unwrap_or_default();
            (title, msg_send![button, isEnabled])
        })
        .unwrap_or_else(|| (String::new(), msg_send![button, isEnabled]));
    settings_select_set_title(button, &title);

    // Keep one downward chevron and rotate its layer so opening/closing is continuous.
    // 始终使用同一个向下箭头，通过图层旋转实现连续的展开/收起动画。
    let symbol = "chevron.down";
    // Keep the arrow outside NSButton's title/image layout. AppKit otherwise lets the
    // symbol's intrinsic size affect the button's layout, which can make the arrow huge
    // and move the title's baseline. 将箭头从 NSButton 的标题/图片布局中分离，避免
    // SF Symbol 的固有尺寸撑大控件并导致文字基线偏移。
    let bounds: NSRect = msg_send![button, bounds];
    let arrow_size = 16.0;
    let arrow_frame = NSRect::new(
        NSPoint::new(
            bounds.origin.x + (bounds.size.width - arrow_size - 12.0).max(0.0),
            bounds.origin.y + (bounds.size.height - arrow_size).max(0.0) / 2.0,
        ),
        NSSize::new(arrow_size, arrow_size),
    );
    let existing_arrow_view = SETTINGS_SELECT_ARROW_VIEWS
        .lock()
        .unwrap()
        .get(&(button as usize))
        .copied()
        .unwrap_or(0) as *mut AnyObject;
    let arrow_view = if existing_arrow_view.is_null() {
        let view = make_symbol_image_view(symbol, arrow_frame);
        let _: () = msg_send![button, addSubview: view];
        SETTINGS_SELECT_ARROW_VIEWS
            .lock()
            .unwrap()
            .insert(button as usize, view as usize);
        release_obj(view);
        view
    } else {
        existing_arrow_view
    };
    let _: () = msg_send![arrow_view, setFrame: arrow_frame];
    let image = make_symbol_image(symbol, NSSize::new(arrow_size, arrow_size));
    if !image.is_null() {
        let _: () = msg_send![arrow_view, setImage: image];
    }

    // Rotate a dedicated layer around the icon center. Without an explicit layer-backed view
    // and anchor point, AppKit can apply the transform in the parent button's coordinate space.
    // 使用独立图层并固定中心锚点旋转；否则 AppKit 可能把变换应用到父按钮坐标系，导致箭头跑位。
    let _: () = msg_send![arrow_view, setWantsLayer: true];
    let arrow_layer: *mut AnyObject = msg_send![arrow_view, layer];
    if !arrow_layer.is_null() {
        let _: () = msg_send![arrow_layer, setAnchorPoint: NSPoint::new(0.5, 0.5)];
        let _: () = msg_send![
            arrow_layer,
            setPosition: NSPoint::new(
                arrow_frame.origin.x + arrow_frame.size.width / 2.0,
                arrow_frame.origin.y + arrow_frame.size.height / 2.0,
            )
        ];
        let target_angle = if open { std::f64::consts::PI } else { 0.0 };
        let key_path = make_nsstring("transform.rotation.z");
        let target_value: *mut AnyObject =
            msg_send![class!(NSNumber), numberWithDouble: target_angle];
        let _: () = msg_send![arrow_layer, setValue: target_value, forKeyPath: key_path];

        if !existing_arrow_view.is_null() {
            let presentation: *mut AnyObject = msg_send![arrow_layer, presentationLayer];
            let from_angle = if presentation.is_null() {
                if open {
                    0.0
                } else {
                    std::f64::consts::PI
                }
            } else {
                let value: *mut AnyObject = msg_send![presentation, valueForKeyPath: key_path];
                if value.is_null() {
                    if open {
                        0.0
                    } else {
                        std::f64::consts::PI
                    }
                } else {
                    msg_send![value, doubleValue]
                }
            };
            let from_value: *mut AnyObject =
                msg_send![class!(NSNumber), numberWithDouble: from_angle];
            let animation: *mut AnyObject = msg_send![
                class!(CASpringAnimation),
                animationWithKeyPath: key_path
            ];
            let _: () = msg_send![animation, setFromValue: from_value];
            let _: () = msg_send![animation, setToValue: target_value];
            let _: () = msg_send![animation, setMass: 1.0f64];
            let _: () = msg_send![animation, setStiffness: 300.0f64];
            let _: () = msg_send![animation, setDamping: 25.0f64];
            let _: () = msg_send![animation, setInitialVelocity: 0.0f64];
            let duration: f64 = msg_send![animation, settlingDuration];
            let _: () = msg_send![animation, setDuration: duration.max(0.32)];
            let animation_key = make_nsstring("settings-select-arrow-rotation");
            let _: () = msg_send![arrow_layer, addAnimation: animation, forKey: animation_key];
            CFRelease(animation_key as *const c_void);
        }
        CFRelease(key_path as *const c_void);
    }

    let palette = settings_palette();
    let tint = crate::ffi::hex_to_ns_color(if enabled {
        palette.primary_text
    } else {
        palette.muted_text
    });
    let _: () = msg_send![button, setContentTintColor: tint];
    let _: () = msg_send![arrow_view, setContentTintColor: tint];
    let layer: *mut AnyObject = msg_send![button, layer];
    if !layer.is_null() {
        let background = settings_select_surface_color(palette);
        crate::ffi::layer_set_background(layer, crate::ffi::hex_to_cg_color(background));
        crate::ffi::layer_set_border(layer, crate::ffi::hex_to_cg_color(palette.card_border));
        let _: () = msg_send![layer, setBorderWidth: 1.0f64];
        let _: () = msg_send![layer, setCornerRadius: 12.0f64];
        let _: () = msg_send![layer, setMasksToBounds: true];
    }
}

unsafe fn settings_select_cancel_item_reveals(panel: *mut AnyObject) {
    let subviews: *mut AnyObject = msg_send![panel, subviews];
    if subviews.is_null() {
        return;
    }
    let count: usize = msg_send![subviews, count];
    for index in 0..count {
        let item: *mut AnyObject = msg_send![subviews, objectAtIndex: index];
        let _: () = msg_send![
            class!(NSObject),
            cancelPreviousPerformRequestsWithTarget: item,
            selector: sel!(reveal),
            object: std::ptr::null::<AnyObject>()
        ];
    }
}

unsafe fn settings_select_close(button: *mut AnyObject) {
    let panel = {
        let mut states = SETTINGS_SELECT_STATES.lock().unwrap();
        let Some(state) = states.get_mut(&(button as usize)) else {
            return;
        };
        state.open = false;
        state.panel as *mut AnyObject
    };
    if !panel.is_null() {
        settings_select_cancel_item_reveals(panel);
        let panel_layer: *mut AnyObject = msg_send![panel, layer];
        if !panel_layer.is_null() {
            let presentation: *mut AnyObject = msg_send![panel_layer, presentationLayer];
            // CALayer.opacity is a CGFloat on macOS, which is f32 in this objc2 ABI.
            // CALayer.opacity 在 macOS 上是 CGFloat，在当前 objc2 ABI 中对应 f32。
            let from_opacity: f32 = if presentation.is_null() {
                msg_send![panel_layer, opacity]
            } else {
                msg_send![presentation, opacity]
            };
            let open_key = make_nsstring("settings-select-open");
            let _: () = msg_send![panel_layer, removeAnimationForKey: open_key];
            CFRelease(open_key as *const c_void);
            // Commit the hidden end state to the model layer before adding the fade. This avoids
            // a one-frame return to opacity 1 when Core Animation removes the animation.
            // 先把隐藏终态提交到模型层，再添加淡出动画，避免动画移除时闪回不透明。
            let _: () = msg_send![panel_layer, setOpacity: 0.0f32];
            let key_path = make_nsstring("opacity");
            let animation: *mut AnyObject = msg_send![
                class!(CABasicAnimation),
                animationWithKeyPath: key_path
            ];
            CFRelease(key_path as *const c_void);
            let from: *mut AnyObject =
                msg_send![class!(NSNumber), numberWithFloat: from_opacity.clamp(0.0, 1.0)];
            let to: *mut AnyObject = msg_send![class!(NSNumber), numberWithFloat: 0.0f32];
            let _: () = msg_send![animation, setFromValue: from];
            let _: () = msg_send![animation, setToValue: to];
            let _: () = msg_send![animation, setDuration: 0.16f64];
            let animation_key = make_nsstring("settings-select-close");
            let _: () = msg_send![panel_layer, addAnimation: animation, forKey: animation_key];
            CFRelease(animation_key as *const c_void);
        }
        let _: () = msg_send![
            button,
            performSelector: sel!(finishClose:),
            withObject: panel,
            afterDelay: 0.16f64
        ];
    }
    if ACTIVE_SETTINGS_SELECT
        .lock()
        .unwrap()
        .is_some_and(|active| active == button as usize)
    {
        *ACTIVE_SETTINGS_SELECT.lock().unwrap() = None;
    }
    settings_select_apply_visual(button, false);
}

extern "C" fn settings_select_finish_close(this: *mut c_void, _cmd: Sel, panel: *mut c_void) {
    unsafe {
        let button = this as *mut AnyObject;
        let panel = panel as *mut AnyObject;
        let should_remove = SETTINGS_SELECT_STATES
            .lock()
            .unwrap()
            .get_mut(&(button as usize))
            .is_some_and(|state| {
                if !state.open && state.panel == panel as usize {
                    state.panel = 0;
                    true
                } else {
                    false
                }
            });
        if should_remove && !panel.is_null() {
            let _: () = msg_send![panel, removeFromSuperview];
        }
    }
}

/// Paint an option row according to its selected/hovered state.
/// 根据选中/悬停状态绘制选择器选项行。
unsafe fn settings_select_item_apply_background(item: *mut AnyObject, hovered: bool) {
    let select: *mut AnyObject = msg_send![item, target];
    let index: isize = msg_send![item, tag];
    let selected = !select.is_null()
        && SETTINGS_SELECT_STATES
            .lock()
            .unwrap()
            .get(&(select as usize))
            .is_some_and(|state| state.selected == index);
    let layer: *mut AnyObject = msg_send![item, layer];
    if layer.is_null() {
        return;
    }
    let palette = settings_palette();
    let background = if selected || hovered {
        settings_select_item_active_color(palette)
    } else {
        0x00000000
    };
    crate::ffi::layer_set_background(layer, crate::ffi::hex_to_cg_color(background));
    let tint = crate::ffi::hex_to_ns_color(if selected || hovered {
        palette.primary_text
    } else {
        palette.muted_text
    });
    let _: () = msg_send![item, setContentTintColor: tint];
}

extern "C" fn settings_select_item_mouse_entered(
    this: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) {
    unsafe {
        settings_select_item_apply_background(this as *mut AnyObject, true);
    }
}

extern "C" fn settings_select_item_mouse_exited(this: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    unsafe {
        settings_select_item_apply_background(this as *mut AnyObject, false);
    }
}

extern "C" fn settings_select_item_reveal(this: *mut c_void, _cmd: Sel, _object: *mut c_void) {
    unsafe {
        let item = this as *mut AnyObject;
        let _: () = msg_send![item, setAlphaValue: 1.0f64];
        let layer: *mut AnyObject = msg_send![item, layer];
        if !layer.is_null() {
            let key_path = make_nsstring("opacity");
            let animation: *mut AnyObject = msg_send![
                class!(CABasicAnimation),
                animationWithKeyPath: key_path
            ];
            CFRelease(key_path as *const c_void);
            let from: *mut AnyObject = msg_send![class!(NSNumber), numberWithDouble: 0.0f64];
            let to: *mut AnyObject = msg_send![class!(NSNumber), numberWithDouble: 1.0f64];
            let _: () = msg_send![animation, setFromValue: from];
            let _: () = msg_send![animation, setToValue: to];
            let _: () = msg_send![animation, setDuration: 0.16f64];
            let animation_key = make_nsstring("settings-select-item-reveal");
            let _: () = msg_send![layer, addAnimation: animation, forKey: animation_key];
            CFRelease(animation_key as *const c_void);
        }
    }
}

fn settings_select_item_class() -> *mut AnyObject {
    SETTINGS_SELECT_ITEM_CLASS
        .get_or_init(|| unsafe {
            let name = CString::new("OhMyTabSettingsSelectItem").unwrap();
            let superclass = class!(NSButton) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(mouseEntered:),
                settings_select_item_mouse_entered as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseExited:),
                settings_select_item_mouse_exited as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(reveal),
                settings_select_item_reveal as *mut c_void,
                CString::new("v@:").unwrap().as_ptr(),
            );
            objc_registerClassPair(cls);
            SettingsSelectItemClass(cls)
        })
        .0
}

unsafe fn settings_select_make_item(
    select: *mut AnyObject,
    panel: *mut AnyObject,
    index: usize,
    title: &str,
    selected: bool,
    width: f64,
    y: f64,
) {
    let item: *mut AnyObject = msg_send![settings_select_item_class(), alloc];
    let item: *mut AnyObject = msg_send![
        item,
        initWithFrame: NSRect::new(
            NSPoint::new(4.0, y),
            NSSize::new((width - 8.0).max(1.0), 32.0)
        )
    ];
    let _: () = msg_send![item, setButtonType: 0isize];
    let _: () = msg_send![item, setBordered: false];
    // The row already starts 4pt inside the panel, so one extra space matches the trigger's
    // visual inset without pushing option text too far inward.
    // 选项行已经从面板内缩 4pt，因此只增加一个空格即可与触发器的视觉内边距保持一致。
    let padded_title = format!(" {title}");
    let title_ns = make_nsstring(&padded_title);
    let _: () = msg_send![item, setTitle: title_ns];
    CFRelease(title_ns as *const c_void);
    let _: () = msg_send![item, setAlignment: 0isize]; // NSTextAlignmentLeft
    let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 13.0f64];
    let _: () = msg_send![item, setFont: font];
    let _: () = msg_send![item, setTag: index as isize];
    let _: () = msg_send![item, setTarget: select];
    let _: () = msg_send![item, setAction: sel!(selectOption:)];
    let symbol = SETTINGS_SELECT_STATES
        .lock()
        .unwrap()
        .get(&(select as usize))
        .and_then(|state| state.item_symbols.get(index))
        .and_then(|symbol| symbol.as_deref())
        .map(str::to_owned);
    if let Some(symbol) = symbol {
        let image = make_symbol_image(&symbol, NSSize::new(16.0, 16.0));
        if !image.is_null() {
            let _: () = msg_send![item, setImage: image];
            let _: () = msg_send![item, setImagePosition: 2isize]; // NSImageLeft
        }
    }
    let _: () = msg_send![item, setWantsLayer: true];
    let item_layer: *mut AnyObject = msg_send![item, layer];
    if !item_layer.is_null() {
        let _: () = msg_send![item_layer, setCornerRadius: 8.0f64];
        let _: () = msg_send![item_layer, setMasksToBounds: true];
    }
    settings_select_item_apply_background(item, false);
    if selected {
        let check_frame = NSRect::new(
            NSPoint::new((width - 8.0 - 24.0).max(0.0), 8.0),
            NSSize::new(14.0, 14.0),
        );
        let check = make_symbol_image_view("checkmark", check_frame);
        let _: () = msg_send![item, addSubview: check];
        release_obj(check);
    }
    let tracking: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
    let tracking: *mut AnyObject = msg_send![
        tracking,
        initWithRect: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width - 8.0, 32.0)),
        options: 0x01u64 | 0x80u64 | 0x200u64,
        owner: item,
        userInfo: std::ptr::null::<AnyObject>()
    ];
    let _: () = msg_send![item, addTrackingArea: tracking];
    release_obj(tracking);
    let _: () = msg_send![panel, addSubview: item];
    let _: () = msg_send![item, setAlphaValue: 0.0f64];
    let reveal_delay = 0.05 + index as f64 * 0.035;
    let _: () = msg_send![
        item,
        performSelector: sel!(reveal),
        withObject: std::ptr::null::<AnyObject>(),
        afterDelay: reveal_delay
    ];
    release_obj(item);
}

unsafe fn settings_select_open(button: *mut AnyObject) {
    let window: *mut AnyObject = msg_send![button, window];
    let content: *mut AnyObject = if window.is_null() {
        std::ptr::null_mut()
    } else {
        msg_send![window, contentView]
    };
    if content.is_null() {
        return;
    }

    if let Some(active) = *ACTIVE_SETTINGS_SELECT.lock().unwrap() {
        if active != button as usize {
            settings_select_close(active as *mut AnyObject);
        }
    }

    // A fast reopen can happen while the close fade is still pending. Remove only the stale
    // panel that belongs to this control and cancel its delayed cleanup callback.
    // 快速重新打开可能发生在关闭淡出尚未结束时；这里只清理本控件的旧面板并取消旧回调。
    let stale_panel = {
        let mut states = SETTINGS_SELECT_STATES.lock().unwrap();
        states
            .get_mut(&(button as usize))
            .filter(|state| !state.open)
            .map(|state| {
                let panel = state.panel;
                state.panel = 0;
                panel as *mut AnyObject
            })
            .unwrap_or(std::ptr::null_mut())
    };
    if !stale_panel.is_null() {
        let _: () = msg_send![
            class!(NSObject),
            cancelPreviousPerformRequestsWithTarget: button,
            selector: sel!(finishClose:),
            object: stale_panel
        ];
        let _: () = msg_send![stale_panel, removeFromSuperview];
    }

    let (items, selected) = {
        let states = SETTINGS_SELECT_STATES.lock().unwrap();
        let Some(state) = states.get(&(button as usize)) else {
            return;
        };
        if state.items.is_empty() || state.open {
            return;
        }
        (state.items.clone(), state.selected.max(0) as usize)
    };

    let bounds: NSRect = msg_send![button, bounds];
    let trigger: NSRect = msg_send![button, convertRect: bounds, toView: content];
    let content_bounds: NSRect = msg_send![content, bounds];
    let row_h = 32.0;
    let panel_h = items.len() as f64 * row_h + 8.0;
    let below = trigger.origin.y - content_bounds.origin.y;
    let above = content_bounds.origin.y + content_bounds.size.height
        - (trigger.origin.y + trigger.size.height);
    let opens_above = below < panel_h + 8.0 && above > below;
    let panel_y = if opens_above {
        trigger.origin.y + trigger.size.height + 8.0
    } else {
        trigger.origin.y - panel_h - 8.0
    };
    let panel_y = panel_y.clamp(
        content_bounds.origin.y + 4.0,
        (content_bounds.origin.y + content_bounds.size.height - panel_h - 4.0)
            .max(content_bounds.origin.y + 4.0),
    );
    let panel_w = trigger.size.width.max(120.0);

    let panel: *mut AnyObject = msg_send![class!(NSView), alloc];
    let panel: *mut AnyObject = msg_send![
        panel,
        initWithFrame: NSRect::new(
            NSPoint::new(trigger.origin.x, panel_y),
            NSSize::new(panel_w, panel_h)
        )
    ];
    let _: () = msg_send![panel, setWantsLayer: true];
    let panel_layer: *mut AnyObject = msg_send![panel, layer];
    if !panel_layer.is_null() {
        let palette = settings_palette();
        crate::ffi::layer_set_background(
            panel_layer,
            crate::ffi::hex_to_cg_color(settings_select_surface_color(palette)),
        );
        crate::ffi::layer_set_border(
            panel_layer,
            crate::ffi::hex_to_cg_color(palette.card_border),
        );
        let _: () = msg_send![panel_layer, setBorderWidth: 1.0f64];
        let _: () = msg_send![panel_layer, setCornerRadius: 12.0f64];
        // Keep the panel's shadow outside its bounds; option rows already clip themselves to
        // their own rounded layers. 让阴影绘制在面板边界外，选项行自行裁剪圆角内容。
        let _: () = msg_send![panel_layer, setMasksToBounds: false];
        let shadow_color = crate::ffi::hex_to_cg_color(0x000000FF);
        crate::ffi::layer_set_shadow_color(panel_layer, shadow_color);
        let _: () = msg_send![panel_layer, setShadowOpacity: 0.12f32];
        let _: () = msg_send![panel_layer, setShadowRadius: 8.0f64];
        let _: () = msg_send![panel_layer, setShadowOffset: NSSize::new(0.0, -4.0)];
    }

    for (index, title) in items.iter().enumerate() {
        let item_y = panel_h - 4.0 - (index as f64 + 1.0) * row_h;
        settings_select_make_item(
            button,
            panel,
            index,
            title,
            index == selected,
            panel_w,
            item_y,
        );
    }
    let _: () = msg_send![content, addSubview: panel];
    release_obj(panel);
    {
        let mut states = SETTINGS_SELECT_STATES.lock().unwrap();
        if let Some(state) = states.get_mut(&(button as usize)) {
            state.panel = panel as usize;
            state.open = true;
        }
    }
    *ACTIVE_SETTINGS_SELECT.lock().unwrap() = Some(button as usize);
    settings_select_apply_visual(button, true);

    if !window.is_null() {
        let _: bool = msg_send![window, makeFirstResponder: button];
    }

    // The reference unfolds with opacity rather than scaling the whole panel, keeping text and
    // rounded edges crisp while it appears. 参考组件使用透明度展开而不是整体缩放，避免
    // 文字和圆角在出现时变糊。
    if !panel_layer.is_null() {
        let _: () = msg_send![panel_layer, setOpacity: 0.0f32];
        let key_path = make_nsstring("opacity");
        let animation: *mut AnyObject = msg_send![
            class!(CABasicAnimation),
            animationWithKeyPath: key_path
        ];
        CFRelease(key_path as *const c_void);
        let from: *mut AnyObject = msg_send![class!(NSNumber), numberWithDouble: 0.0f64];
        let to: *mut AnyObject = msg_send![class!(NSNumber), numberWithDouble: 1.0f64];
        let _: () = msg_send![animation, setFromValue: from];
        let _: () = msg_send![animation, setToValue: to];
        let _: () = msg_send![animation, setDuration: 0.18f64];
        let _: () = msg_send![panel_layer, setOpacity: 1.0f32];
        let animation_key = make_nsstring("settings-select-open");
        let _: () = msg_send![panel_layer, addAnimation: animation, forKey: animation_key];
        CFRelease(animation_key as *const c_void);

        // Separate the panel from the trigger with a short spring translation, matching the
        // reference's attached-then-detached unfold without scaling the panel contents.
        // 用轻微弹性位移让面板先贴合触发器再分离，模拟参考组件的展开而不缩放内容。
        let key_path = make_nsstring("transform.translation.y");
        let spring: *mut AnyObject = msg_send![
            class!(CASpringAnimation),
            animationWithKeyPath: key_path
        ];
        CFRelease(key_path as *const c_void);
        let from_y = if opens_above { -8.0 } else { 8.0 };
        let from: *mut AnyObject = msg_send![class!(NSNumber), numberWithDouble: from_y];
        let to: *mut AnyObject = msg_send![class!(NSNumber), numberWithDouble: 0.0f64];
        let _: () = msg_send![spring, setFromValue: from];
        let _: () = msg_send![spring, setToValue: to];
        let _: () = msg_send![spring, setMass: 1.0f64];
        let _: () = msg_send![spring, setStiffness: 260.0f64];
        let _: () = msg_send![spring, setDamping: 24.0f64];
        let _: () = msg_send![spring, setInitialVelocity: 0.0f64];
        let duration: f64 = msg_send![spring, settlingDuration];
        let _: () = msg_send![spring, setDuration: duration.max(0.32)];
        let animation_key = make_nsstring("settings-select-open-translation");
        let _: () = msg_send![panel_layer, addAnimation: spring, forKey: animation_key];
        CFRelease(animation_key as *const c_void);
    }
}

pub(super) extern "C" fn settings_select_select_option(
    this: *mut c_void,
    _cmd: Sel,
    sender: *mut c_void,
) {
    unsafe {
        let select = this as *mut AnyObject;
        let item = sender as *mut AnyObject;
        let index: isize = msg_send![item, tag];
        let valid = SETTINGS_SELECT_STATES
            .lock()
            .unwrap()
            .get(&(select as usize))
            .is_some_and(|state| index >= 0 && (index as usize) < state.items.len());
        if !valid {
            return;
        }
        if let Some(state) = SETTINGS_SELECT_STATES
            .lock()
            .unwrap()
            .get_mut(&(select as usize))
        {
            state.selected = index;
        }
        settings_select_close(select);
        let target: *mut AnyObject = msg_send![select, target];
        if !target.is_null() {
            let action: Sel = msg_send![select, action];
            let _: bool = msg_send![select, sendAction: action, to: target];
        }
    }
}

extern "C" fn settings_select_mouse_down(this: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    unsafe {
        let button = this as *mut AnyObject;
        if !msg_send![button, isEnabled] {
            return;
        }
        let open = SETTINGS_SELECT_STATES
            .lock()
            .unwrap()
            .get(&(button as usize))
            .is_some_and(|state| state.open);
        if open {
            settings_select_close(button);
        } else {
            settings_select_open(button);
        }
    }
}

extern "C" fn settings_select_key_down(this: *mut c_void, _cmd: Sel, event: *mut c_void) {
    unsafe {
        let button = this as *mut AnyObject;
        let key_code: u16 = msg_send![event as *mut AnyObject, keyCode];
        if matches!(key_code, 36 | 49 | 125 | 126) {
            let open = SETTINGS_SELECT_STATES
                .lock()
                .unwrap()
                .get(&(button as usize))
                .is_some_and(|state| state.open);
            if key_code == 36 || key_code == 49 {
                if open {
                    settings_select_close(button);
                } else {
                    settings_select_open(button);
                }
            } else if !open {
                settings_select_open(button);
            }
            return;
        }
        if key_code == 53 {
            settings_select_close(button);
            return;
        }
        let events: *mut AnyObject = msg_send![class!(NSArray), arrayWithObject: event];
        let _: () = msg_send![button, interpretKeyEvents: events];
    }
}

extern "C" fn settings_select_accepts_first_responder(_this: *mut c_void, _cmd: Sel) -> bool {
    true
}

extern "C" fn settings_select_index(this: *mut c_void, _cmd: Sel) -> isize {
    SETTINGS_SELECT_STATES
        .lock()
        .unwrap()
        .get(&(this as usize))
        .map(|state| state.selected)
        .unwrap_or(-1)
}

extern "C" fn settings_select_set_index(this: *mut c_void, _cmd: Sel, index: isize) {
    unsafe {
        let button = this as *mut AnyObject;
        if let Some(state) = SETTINGS_SELECT_STATES
            .lock()
            .unwrap()
            .get_mut(&(button as usize))
        {
            if index >= 0 && (index as usize) < state.items.len() {
                state.selected = index;
            }
        }
        let open = SETTINGS_SELECT_STATES
            .lock()
            .unwrap()
            .get(&(button as usize))
            .is_some_and(|state| state.open);
        settings_select_apply_visual(button, open);
    }
}

extern "C" fn settings_select_remove_all(this: *mut c_void, _cmd: Sel) {
    unsafe {
        let button = this as *mut AnyObject;
        let open = SETTINGS_SELECT_STATES
            .lock()
            .unwrap()
            .get(&(button as usize))
            .is_some_and(|state| state.open);
        if open {
            settings_select_close(button);
        }
        if let Some(state) = SETTINGS_SELECT_STATES
            .lock()
            .unwrap()
            .get_mut(&(button as usize))
        {
            state.items.clear();
            state.item_symbols.clear();
            state.selected = -1;
        }
        settings_select_apply_visual(button, false);
    }
}

extern "C" fn settings_select_add_item(this: *mut c_void, _cmd: Sel, title: *mut c_void) {
    unsafe {
        let button = this as *mut AnyObject;
        let title = nsstring_to_rust(title as *mut AnyObject);
        if let Some(state) = SETTINGS_SELECT_STATES
            .lock()
            .unwrap()
            .get_mut(&(button as usize))
        {
            state.items.push(title);
            state.item_symbols.push(None);
            if state.selected < 0 {
                state.selected = 0;
            }
        }
        let open = SETTINGS_SELECT_STATES
            .lock()
            .unwrap()
            .get(&(button as usize))
            .is_some_and(|state| state.open);
        settings_select_apply_visual(button, open);
    }
}

/// Close the active select when a settings-window click lands outside its trigger or panel.
/// 设置窗口点击落在触发器和面板之外时关闭当前选择器。
pub(super) unsafe fn settings_select_handle_window_mouse_down(
    window: *mut AnyObject,
    event: *mut AnyObject,
) {
    let active = *ACTIVE_SETTINGS_SELECT.lock().unwrap();
    let Some(active) = active else { return };
    let button = active as *mut AnyObject;
    let panel = SETTINGS_SELECT_STATES
        .lock()
        .unwrap()
        .get(&active)
        .map(|state| state.panel)
        .unwrap_or(0) as *mut AnyObject;
    if panel.is_null() || button.is_null() || window.is_null() {
        return;
    }
    let content: *mut AnyObject = msg_send![window, contentView];
    if content.is_null() {
        return;
    }
    let location: NSPoint = msg_send![event, locationInWindow];
    let point: NSPoint = msg_send![
        content,
        convertPoint: location,
        fromView: std::ptr::null::<AnyObject>()
    ];
    let panel_frame: NSRect = msg_send![panel, frame];
    let button_bounds: NSRect = msg_send![button, bounds];
    let button_frame: NSRect = msg_send![button, convertRect: button_bounds, toView: content];
    let contains = |frame: NSRect| {
        point.x >= frame.origin.x
            && point.x <= frame.origin.x + frame.size.width
            && point.y >= frame.origin.y
            && point.y <= frame.origin.y + frame.size.height
    };
    if !contains(panel_frame) && !contains(button_frame) {
        settings_select_close(button);
    }
}

/// Attach an optional SF Symbol to one item in the next rendered options panel.
/// 为下次渲染的指定选项附加可选 SF Symbol。
pub(super) fn settings_select_set_item_symbol(select: *mut AnyObject, index: usize, symbol: &str) {
    let mut states = SETTINGS_SELECT_STATES.lock().unwrap();
    let Some(state) = states.get_mut(&(select as usize)) else {
        return;
    };
    if let Some(slot) = state.item_symbols.get_mut(index) {
        *slot = Some(symbol.to_owned());
    }
}

extern "C" fn settings_select_set_enabled(this: *mut c_void, _cmd: Sel, enabled: bool) {
    unsafe {
        let mut sup = ObjcSuper {
            receiver: this,
            super_class: class!(NSButton) as *const _ as *mut c_void,
        };
        type SetEnabled = unsafe extern "C" fn(*mut ObjcSuper, Sel, bool);
        let send: SetEnabled = std::mem::transmute(objc_msgSendSuper as *const ());
        send(&mut sup, sel!(setEnabled:), enabled);
        let open = SETTINGS_SELECT_STATES
            .lock()
            .unwrap()
            .get(&(this as usize))
            .is_some_and(|state| state.open);
        settings_select_apply_visual(this as *mut AnyObject, open);
    }
}

fn settings_select_class() -> *mut AnyObject {
    SETTINGS_SELECT_CLASS
        .get_or_init(|| unsafe {
            let name = CString::new("OhMyTabSettingsSelect").unwrap();
            let superclass = class!(NSButton) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types_void_event = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(mouseDown:),
                settings_select_mouse_down as *mut c_void,
                types_void_event.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(keyDown:),
                settings_select_key_down as *mut c_void,
                types_void_event.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(selectOption:),
                settings_select_select_option as *mut c_void,
                types_void_event.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(finishClose:),
                settings_select_finish_close as *mut c_void,
                types_void_event.as_ptr(),
            );
            let types_bool = CString::new("B@:").unwrap();
            class_addMethod(
                cls,
                sel!(acceptsFirstResponder),
                settings_select_accepts_first_responder as *mut c_void,
                types_bool.as_ptr(),
            );
            let types_index = CString::new("q@:").unwrap();
            class_addMethod(
                cls,
                sel!(indexOfSelectedItem),
                settings_select_index as *mut c_void,
                types_index.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(selectItemAtIndex:),
                settings_select_set_index as *mut c_void,
                CString::new("v@:q").unwrap().as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(removeAllItems),
                settings_select_remove_all as *mut c_void,
                CString::new("v@:").unwrap().as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(addItemWithTitle:),
                settings_select_add_item as *mut c_void,
                types_void_event.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(setEnabled:),
                settings_select_set_enabled as *mut c_void,
                CString::new("v@:B").unwrap().as_ptr(),
            );
            objc_registerClassPair(cls);
            SettingsSelectClass(cls)
        })
        .0
}

/// Custom settings select with a bouncy, position-aware options panel (alloc +1).
/// 自定义设置选择器：带弹性动画、根据空间选择展开方向的选项面板(alloc +1)。
pub(super) unsafe fn make_popup(
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    items: &[&str],
    selected: usize,
) -> *mut AnyObject {
    let popup: *mut AnyObject = msg_send![settings_select_class(), alloc];
    let popup: *mut AnyObject = msg_send![
        popup,
        initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    ];
    let _: () = msg_send![popup, setButtonType: 0isize];
    let _: () = msg_send![popup, setBordered: false];
    let _: () = msg_send![popup, setAlignment: 0isize]; // NSTextAlignmentLeft
    let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 13.5f64];
    let _: () = msg_send![popup, setFont: font];
    let _: () = msg_send![popup, setFocusRingType: 1isize]; // NSFocusRingTypeNone
    let _: () = msg_send![popup, setWantsLayer: true];
    SETTINGS_SELECT_STATES.lock().unwrap().insert(
        popup as usize,
        SettingsSelectState {
            items: items.iter().map(|item| (*item).to_owned()).collect(),
            item_symbols: vec![None; items.len()],
            selected: selected as isize,
            panel: 0,
            open: false,
        },
    );
    let _: () = msg_send![popup, setEnabled: true];
    settings_select_apply_visual(popup, false);
    if let Some(first) = items.first() {
        settings_select_set_title(popup, first);
        let _: () = msg_send![popup, selectItemAtIndex: selected as isize];
    }
    popup
}

pub(super) const HTML_SWITCH_W: f64 = 38.0;
pub(super) const HTML_SWITCH_H: f64 = 22.0;
pub(super) const HTML_SWITCH_KNOB_D: f64 = 18.0;
// Keep switches on the same trailing edge as popup fields in the settings column.
// 开关与设置列中的下拉框共用同一条右侧边界。
pub(super) const HTML_SWITCH_TRAILING_INSET: f64 = 0.0;
const HTML_SWITCH_SPRING_MASS: f64 = 4.0;
const HTML_SWITCH_SPRING_STIFFNESS: f64 = 800.0;
const HTML_SWITCH_SPRING_DAMPING: f64 = 80.0;
const HTML_SWITCH_PRESS_SCALE: f64 = 0.9;

pub(super) struct HtmlSwitchClass(*mut AnyObject);
unsafe impl Send for HtmlSwitchClass {}
unsafe impl Sync for HtmlSwitchClass {}

pub(super) static HTML_SWITCH_CLASS: OnceLock<HtmlSwitchClass> = OnceLock::new();

/// Return the custom knob layer after the switch has been initialized.
/// 返回开关初始化后创建的自定义滑块图层。
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
        let _: () = msg_send![animation, setDuration: settling_duration.max(0.18)];
        let animation_key = make_nsstring("html-switch-position");
        let _: () = msg_send![knob, addAnimation: animation, forKey: animation_key];
        CFRelease(animation_key as *const c_void);
    }
}

/// Give the knob a short press-and-release response when the custom button is clicked.
/// 自绘开关点击时让滑块短暂压缩并回弹,提供轻微的按下反馈。
unsafe fn html_switch_animate_press(button: *mut AnyObject) {
    let knob = html_switch_knob(button);
    if knob.is_null() {
        return;
    }

    // A keyframe keeps the model transform unchanged, so the switch is ready for the next
    // click even if the next state change arrives before this feedback finishes.
    // 使用关键帧而不修改模型变换,即使下一次点击提前到来,也不会累积缩放状态。
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
    let _: () = msg_send![animation, setDuration: 0.22f64];
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
/// 自绘开关的 enabled 状态变化时同步刷新轨道颜色。
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
        html_switch_animate_press(button);
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

/// Apply a sidebar title's font/color and refresh the label's vertical optical alignment.
/// 应用侧边栏标题的字形/颜色，并刷新文字的垂直光学对齐。
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
    center_sidebar_label(label, 38.0);
    CFRelease(title_ns as *const c_void);
}

/// 设侧边栏按钮标题为 attributed title:未选中用次要文本色,选中用系统强调色。
/// Set the sidebar button title as an attributed title, using the secondary text color when
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
        crate::ffi::hex_to_ns_color(settings_palette().secondary_text)
    };
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

/// Apply the sidebar hover surface and foreground transition. The selected item owns its highlight
/// and is intentionally left untouched by hover tracking.
/// 应用侧栏悬浮背景和前景色过渡；选中项由自己的高亮状态控制，悬浮事件不改动它。
unsafe fn set_sidebar_hovered(btn: *mut AnyObject, hovered: bool) {
    if btn.is_null() {
        return;
    }
    let tag: isize = msg_send![btn, tag];
    if tag >= 0 && tag as usize == SIDEBAR_SELECTED.load(Ordering::SeqCst) {
        return;
    }
    let palette = settings_palette();
    let color = crate::ffi::hex_to_ns_color(if hovered {
        palette.primary_text
    } else {
        palette.secondary_text
    });
    let label = SIDEBAR_TITLE_LABELS
        .lock()
        .unwrap()
        .get(&(btn as usize))
        .map(|p| p.0);
    if let Some(label) = label {
        // Only change the existing label color. Rebuilding attributed strings and measuring the
        // cell on every mouse event blocks the main thread and makes the shared pill stutter.
        // 这里只更新现有 label 的颜色；每次鼠标事件重建 attributed string 并测量 cell 会阻塞主线程，
        // 导致共享悬浮层卡顿。
        let _: () = msg_send![label, setTextColor: color];
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

/// Fit the sidebar label to its measured single-line cell height and center that frame in the
/// 38pt tab. This compensates for AppKit's top-biased text drawing when the label frame is taller
/// than the actual line height, and is rerun when selection changes the font weight.
/// 将侧栏文本 frame 收紧到 cell 测得的单行高度，再把它放到 38pt tab 的垂直中心；这样可抵消
/// AppKit 在大 frame 中偏上绘制文字的问题，并在选中态切换字重后重新计算。
unsafe fn center_sidebar_label(label: *mut AnyObject, row_h: f64) {
    if label.is_null() {
        return;
    }
    let cell: *mut AnyObject = msg_send![label, cell];
    if cell.is_null() {
        return;
    }
    let bounds: NSRect = msg_send![label, bounds];
    let measured: NSSize = msg_send![cell, cellSizeForBounds: bounds];
    if !measured.height.is_finite() || measured.height <= 0.0 {
        return;
    }
    let mut frame: NSRect = msg_send![label, frame];
    frame.origin.y = (row_h - measured.height).max(0.0) / 2.0;
    frame.size.height = measured.height;
    let _: () = msg_send![label, setFrame: frame];
}

/// Create the single shared hover surface used by all sidebar rows.
/// 创建由所有侧栏条目共用的悬浮背景层。
pub(super) unsafe fn make_sidebar_hover_highlight(
    parent: *mut AnyObject,
    x: f64,
    y: f64,
    w: f64,
) -> *mut AnyObject {
    // Keep the hover surface below the buttons so it is purely visual and never intercepts input.
    // 将悬浮层放在按钮下方，使其只负责视觉效果，不拦截按钮输入。
    let hover: *mut AnyObject = msg_send![class!(NSView), alloc];
    let hover: *mut AnyObject = msg_send![
        hover,
        initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, 38.0))
    ];
    let _: () = msg_send![hover, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![hover, layer];
    if !layer.is_null() {
        layer_set_background(
            layer,
            crate::ffi::hex_to_cg_color(settings_palette().hover_bg),
        );
        let _: () = msg_send![layer, setCornerRadius: 10.0f64];
        let _: () = msg_send![layer, setMasksToBounds: true];
    }
    let _: () = msg_send![hover, setAlphaValue: 0.0f64];
    let _: () = msg_send![parent, addSubview: hover];
    SIDEBAR_HOVER_HIGHLIGHT
        .lock()
        .unwrap()
        .replace(ObjPtr(hover));
    SIDEBAR_HOVERED.store(0, Ordering::SeqCst);
    SIDEBAR_HOVER_VISIBLE.store(false, Ordering::SeqCst);
    release_obj(hover);
    hover
}

/// 侧边栏按钮(borderless NSButton,左对齐图标+文字,tag 区分页)。
/// Sidebar button (borderless NSButton; left-aligned icon + title; tag selects the page).
/// The component layer supplies the child frames so every item shares one alignment contract.
/// 子视图 frame 由组件层传入，保证所有条目遵守同一套对齐契约。
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
    icon_frame: NSRect,
    label_frame: NSRect,
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
            .insert(btn as usize, ObjPtr(icon_view));
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
    let label_color = crate::ffi::hex_to_ns_color(settings_palette().secondary_text);
    let _: () = msg_send![label, setTextColor: label_color];
    let _: () = msg_send![label, setSelectable: false];
    let _: () = msg_send![label, setAlignment: -1isize]; // NSTextAlignmentNatural
                                                         // Sidebar tabs are one-line labels. Single-line mode makes NSTextField use its control-size
                                                         // baseline instead of pinning a multi-line cell's glyphs to the top of the frame.
                                                         // 侧栏 tab 都是单行标签；单行模式使用控件尺寸决定的 baseline，避免多行 cell 把字形顶到 frame 顶部。
    let _: () = msg_send![label, setUsesSingleLineMode: true];
    let _: () = msg_send![label, setLineBreakMode: 4isize]; // NSLineBreakByTruncatingTail
    if msg_send![label, respondsToSelector: sel!(setMaximumNumberOfLines:)] {
        let _: () = msg_send![label, setMaximumNumberOfLines: 1isize];
    }
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
    // Keep localized wording intact: uppercase is a hierarchy channel English has but CJK does
    // not. Weight, color, and spacing carry the section level instead.
    // 保留本地化原文：大写是英文拥有而 CJK 没有的层级通道，区块层级由字重、颜色和间距表达。
    let ns = make_nsstring(text);
    let _: () = msg_send![label, setStringValue: ns];
    CFRelease(ns as *const c_void);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let font: *mut AnyObject = msg_send![class!(NSFont), boldSystemFontOfSize: 12.0f64];
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
        msg_send![label, initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, 44.0))];
    set_field(label, text);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let font: *mut AnyObject = msg_send![class!(NSFont), boldSystemFontOfSize: 25.0f64];
    let _: () = msg_send![label, setFont: font];
    let _: () = msg_send![label, setAlignment: -1isize]; // NSTextAlignmentNatural
    let _: () = msg_send![label, setUsesSingleLineMode: false];
    let _: () = msg_send![label, setLineBreakMode: 0isize]; // NSLineBreakByWordWrapping
    if msg_send![label, respondsToSelector: sel!(setMaximumNumberOfLines:)] {
        let _: () = msg_send![label, setMaximumNumberOfLines: 2isize];
    }
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
        // The card is inserted below its siblings with addSubview:positioned:. Keep its layer at
        // the default z-position so the border remains visible above the document background.
        // 卡片通过 addSubview:positioned: 放在 sibling 下方；layer 保持默认 z，确保边框不会
        // 被 document 背景盖住。
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

/// Add a standard row and also return its label pointer for conditional visibility.
/// label/control 加入父视图后由父视图持有,release 后指针仍有效。
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
    // 根据控件的 leading edge 推导标签可用宽度。调用方传入的旧宽度仅为兼容保留，翻译文案
    // 不应再依赖每条字符串的 150/220pt 修补值。
    let control_frame: NSRect = msg_send![control, frame];
    let effective_label_w = derived_label_width(control_frame.origin.x, label_x, 18.0);
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
    // 左对齐:设置项标签贴在内容区左侧(NSTextAlignmentLeft = 0,arm64/x86_64 一致)。
    // Left-aligned: the row label hugs the content area's left edge (NSTextAlignmentLeft = 0,
    // identical on arm64 and x86_64).
    let _: () = msg_send![label, setAlignment: -1isize]; // NSTextAlignmentNatural
    let label_color = crate::ffi::hex_to_ns_color(settings_palette().primary_text);
    let _: () = msg_send![label, setTextColor: label_color];
    let _: () = msg_send![label, setUsesSingleLineMode: false];
    let _: () = msg_send![label, setLineBreakMode: 0isize]; // NSLineBreakByWordWrapping
    if msg_send![label, respondsToSelector: sel!(setMaximumNumberOfLines:)] {
        let _: () = msg_send![label, setMaximumNumberOfLines: 2isize];
    }
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

/// Add a compact settings row. The legacy subtitle argument is intentionally ignored so callers
/// can migrate incrementally without rendering long explanatory paragraphs inside cards.
/// 创建紧凑设置行。旧的 subtitle 参数刻意忽略，调用方可以渐进迁移，同时卡片内不再渲染大段说明。
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
    let title_color = crate::ffi::hex_to_ns_color(settings_palette().primary_text);
    let _: () = msg_send![title_label, setTextColor: title_color];
    let _: () = msg_send![title_label, setAlignment: -1isize]; // NSTextAlignmentNatural
    let _: () = msg_send![title_label, setUsesSingleLineMode: true];
    let _: () = msg_send![title_label, setLineBreakMode: 4isize]; // NSLineBreakByTruncatingTail
    let title_font: *mut AnyObject = msg_send![class!(NSFont), messageFontOfSize: 13.5f64];
    let _: () = msg_send![title_label, setFont: title_font];
    let _: () = msg_send![parent, addSubview: title_label];
    release_obj(title_label);

    let _: () = msg_send![control, setAutoresizingMask: 10u64];
    let _: () = msg_send![parent, addSubview: control];
    release_obj(control);
    (title_label, control)
}

/// A single-line settings row; the component layer centers its label and control inside the
/// shared visual height. Rows that genuinely need wrapping should use a dedicated multi-line layout.
/// 单行设置 row；组件层会在统一的视觉行高内居中标题和控件。确实需要换行的内容应使用独立的多行布局。
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
    let label_color = crate::ffi::hex_to_ns_color(settings_palette().primary_text);
    let _: () = msg_send![label, setTextColor: label_color];
    let font: *mut AnyObject = msg_send![class!(NSFont), messageFontOfSize: 13.5f64];
    let _: () = msg_send![label, setFont: font];
    let _: () = msg_send![label, setUsesSingleLineMode: false];
    let _: () = msg_send![label, setLineBreakMode: 0isize]; // NSLineBreakByWordWrapping
    if msg_send![label, respondsToSelector: sel!(setMaximumNumberOfLines:)] {
        let _: () = msg_send![label, setMaximumNumberOfLines: 2isize];
    }
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

/// Grow a provisionally-sized page when content exceeds it, without moving existing children.
/// 页面先按宽松高度构建；内容超出时只增长文档，不整体搬移已有子视图。
///
/// This keeps translated rows from being clipped while avoiding a second, conflicting layout
/// pass with AppKit's top-anchored autoresizing masks. The helper is also safe to call after an
/// inline update expands a card.
pub(super) unsafe fn fit_settings_document_height(
    document: *mut AnyObject,
    minimum_height: f64,
    top_padding: f64,
    bottom_padding: f64,
) -> f64 {
    if document.is_null() {
        return minimum_height.max(1.0);
    }
    let subviews: *mut AnyObject = msg_send![document, subviews];
    let count: usize = if subviews.is_null() {
        0
    } else {
        msg_send![subviews, count]
    };
    let mut max_y = 0.0f64;
    for index in 0..count {
        let child: *mut AnyObject = msg_send![subviews, objectAtIndex: index as isize];
        if child.is_null() {
            continue;
        }
        let frame: NSRect = msg_send![child, frame];
        max_y = max_y.max(frame.origin.y + frame.size.height);
    }
    let required_height =
        required_document_height(max_y, minimum_height, top_padding, bottom_padding);
    let current_frame: NSRect = msg_send![document, frame];
    // Do not mutate child frames here. Children use MinYMargin autoresizing masks, so changing
    // the document frame already gives AppKit an opportunity to reposition them; manually doing
    // the same shift caused the title and rows to move twice after the first layout pass.
    // 这里不能再修改子视图 frame。子视图使用 MinYMargin，自身高度变化时 AppKit 已可能重排；
    // 再手动移动一次会让标题和行在首次布局后发生双重位移。
    let final_height = stable_document_height(current_frame.size.height, required_height);
    let final_frame = NSRect::new(
        current_frame.origin,
        NSSize::new(current_frame.size.width, final_height),
    );
    if (final_height - current_frame.size.height).abs() > 0.5 {
        let _: () = msg_send![document, setFrame: final_frame];
    }
    final_height
}

/// Pure counterpart of the document fitting rule, kept separate so expansion behavior can be
/// covered without constructing AppKit views in headless tests.
/// 文档高度拟合规则的纯函数版本，便于在无 AppKit 的测试中覆盖长文本/短文本两种情况。
pub(super) fn required_document_height(
    content_max_y: f64,
    minimum_height: f64,
    top_padding: f64,
    bottom_padding: f64,
) -> f64 {
    (content_max_y + bottom_padding)
        .max(minimum_height)
        .max(top_padding + bottom_padding + 1.0)
}

/// Keep an already-laid-out document stable unless content requires growth. Shrinking is unsafe
/// for top-anchored manual frames because AppKit may apply the autoresizing delta to children.
/// 已排版的 document 只在内容不足时增长；收缩会触发顶部锚定子视图的自动位移，因此禁止收缩。
pub(super) fn stable_document_height(current_height: f64, required_height: f64) -> f64 {
    current_height.max(required_height).max(1.0)
}

/// Pure rectangle predicates shared by the debug validator and headless layout tests.
/// 调试验证器和无头布局测试共用的纯矩形判断。
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

#[derive(Clone, Copy)]
struct DebugLayoutEntry {
    index: usize,
    frame: NSRect,
    interactive: bool,
    text_required_height: Option<f64>,
}

/// Collect descendant frames in document coordinates. Manual settings layout uses several
/// nested AppKit views, so comparing each child's local frame against the document directly is
/// incorrect; accumulating the parent origins mirrors `convertRect:toView:` without introducing
/// another FFI conversion in the debug-only path.
/// 递归收集 document 坐标系中的后代 frame。设置页包含多层 AppKit view，不能直接把子 view 的
/// local frame 与 document 比较；累加父坐标等价于 convertRect:toView:，且只影响 debug 路径。
unsafe fn collect_debug_layout(
    view: *mut AnyObject,
    parent_origin: NSPoint,
    document_width: f64,
    entries: &mut Vec<DebugLayoutEntry>,
    separators: &mut Vec<(usize, NSRect, f64)>,
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
        let frame = NSRect::new(
            NSPoint::new(
                parent_origin.x + local.origin.x,
                parent_origin.y + local.origin.y,
            ),
            local.size,
        );
        let interactive = msg_send![child, isKindOfClass: class!(NSButton)]
            || msg_send![child, isKindOfClass: class!(NSSlider)]
            || msg_send![child, isKindOfClass: class!(NSColorWell)]
            || msg_send![child, isKindOfClass: class!(NSPopUpButton)];
        let is_text = msg_send![child, isKindOfClass: class!(NSTextField)];
        if interactive || is_text {
            let text_required_height = if is_text {
                let cell: *mut AnyObject = msg_send![child, cell];
                if cell.is_null() {
                    None
                } else {
                    let measured: NSSize = msg_send![cell, cellSizeForBounds: local];
                    Some(measured.height)
                }
            } else {
                None
            };
            entries.push(DebugLayoutEntry {
                index,
                frame,
                interactive,
                text_required_height,
            });
        }
        if frame.size.height <= 1.5 && frame.size.width > document_width * 0.5 {
            let z = {
                let layer: *mut AnyObject = msg_send![child, layer];
                if layer.is_null() {
                    0.0
                } else {
                    msg_send![layer, zPosition]
                }
            };
            separators.push((index, frame, z));
        }
        collect_debug_layout(child, frame.origin, document_width, entries, separators);
    }
}

/// Validate the real AppKit page tree when explicitly requested during development. This catches
/// the failures pure geometry tests cannot see: descendant controls crossing, frames escaping the
/// document, and separators rendered above content because of view/layer order.
/// 开发阶段显式开启时验证真实 AppKit 页面树，捕获纯几何测试看不到的问题：后代控件相交、frame 越出
/// document，以及因 view/layer 顺序错误而绘制到内容上方的分隔线。
pub(super) unsafe fn debug_validate_settings_page(scroll: *mut AnyObject, name: &str) {
    if !cfg!(debug_assertions) || std::env::var_os("OH_MY_TAB_LAYOUT_DEBUG").is_none() {
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
    let mut separators = Vec::new();
    collect_debug_layout(
        document,
        NSPoint::new(0.0, 0.0),
        document_bounds.size.width,
        &mut entries,
        &mut separators,
    );
    let mut errors = Vec::new();
    let document_rect = NSRect::new(NSPoint::new(0.0, 0.0), document_bounds.size);
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
                    "text[{0}] needs {1:.1}pt but frame is {2:.1}pt high: {3:?}",
                    entry.index, required_height, entry.frame.size.height, entry.frame
                ));
            }
        }
    }
    for (left_index, left) in entries.iter().enumerate() {
        for right in entries.iter().skip(left_index + 1) {
            // Text labels may overlap another label in a deliberately stacked description row,
            // but an interactive control must never intersect a label or another control.
            // 描述行中的文字 label 允许按设计上下堆叠；交互控件不能与 label 或其他控件相交。
            if (left.interactive || right.interactive)
                && rects_overlap(left.frame, right.frame, 0.5)
            {
                errors.push(format!(
                    "views overlap: [{}] {:?} × [{}] {:?}",
                    left.index, left.frame, right.index, right.frame
                ));
            }
        }
    }
    for (index, frame, z) in separators {
        if z >= -0.1 {
            errors.push(format!(
                "separator[{index}] {:?} zPosition={z:.2} (must be below controls)",
                frame
            ));
        }
    }
    if !errors.is_empty() {
        panic!("[settings-layout] {name}:\n{}", errors.join("\n"));
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
        derived_label_width, rect_inside, rects_overlap, required_document_height,
        stable_document_height,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize};

    #[test]
    fn label_width_follows_control_leading_edge() {
        assert_eq!(derived_label_width(319.0, 12.0, 18.0), 289.0);
        assert_eq!(derived_label_width(20.0, 12.0, 18.0), 1.0);
    }

    #[test]
    fn document_height_tracks_content_without_dropping_below_viewport() {
        assert_eq!(required_document_height(840.0, 600.0, 24.0, 24.0), 864.0);
        assert_eq!(required_document_height(420.0, 600.0, 24.0, 24.0), 600.0);
        assert_eq!(required_document_height(0.0, 0.0, 24.0, 24.0), 49.0);
    }

    #[test]
    fn stable_document_height_never_shrinks_after_children_are_laid_out() {
        assert_eq!(stable_document_height(1_120.0, 840.0), 1_120.0);
        assert_eq!(stable_document_height(700.0, 840.0), 840.0);
        assert_eq!(stable_document_height(0.0, 0.0), 1.0);
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
}
