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

/// Return a flipped NSView class for embedded flows that use top-down child coordinates.
/// 为使用自顶向下子坐标的内嵌流程提供真正 flipped 的 NSView 类。
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
/// 设置界面所有文字和控件统一使用语义化颜色角色。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SettingsTextRole {
    Primary,
    Secondary,
    Sidebar,
    Muted,
    Disabled,
    Accent,
    AccentHover,
}

/// Resolve one text role from the active light/dark palette.
/// 从当前明暗主题调色板解析一个文字角色。
pub(super) fn settings_text_color(role: SettingsTextRole) -> *mut AnyObject {
    let palette = settings_palette();
    let color = match role {
        SettingsTextRole::Primary => palette.primary_text,
        SettingsTextRole::Secondary => palette.secondary_text,
        SettingsTextRole::Sidebar => palette.sidebar_text,
        SettingsTextRole::Muted => palette.muted_text,
        SettingsTextRole::Disabled => palette.disabled_text,
        SettingsTextRole::Accent => palette.accent,
        SettingsTextRole::AccentHover => palette.accent_hover,
    };
    crate::ffi::hex_to_ns_color(color)
}

/// Apply a semantic text role to a text-bearing AppKit view.
/// 将语义化文字颜色应用到承载文字的 AppKit view。
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
        0xFF3B30FF => p.destructive,
        0xD70015FF => p.destructive_hover,
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
            -4 => 0xD70015FFu32, // destructive confirmation hover
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
            -4 => 0xFF3B30FFu32, // destructive confirmation
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

/// Configure a settings button to show wrapped text, returning the height needed for at most
/// `max_lines` lines. The native cell keeps the button interaction/bezel, while a disabled child
/// label owns multiline drawing so AppKit cannot collapse the title back to one line.
/// 将设置按钮配置为换行文本，并返回最多 `max_lines` 行所需的高度。原生 cell 继续负责按钮
/// 交互和背景，禁用的子 label 负责多行绘制，避免 AppKit 把标题重新压回单行。
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
        let _: () = msg_send![label, setTruncatesLastVisibleLine: false];
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
/// 将共享的多行 label 在按钮最终 frame 内垂直居中。
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
/// 按钮 frame 变化后重建 styled button 的 tracking area。
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
pub(super) static SIDEBAR_HOVER_VISIBLE: AtomicBool = AtomicBool::new(false);
pub(super) static SIDEBAR_HOVER_PRIMED: AtomicBool = AtomicBool::new(false);
pub(super) static SIDEBAR_HOVER_HIGHLIGHT: MainThreadSlot<Option<ObjPtr>> =
    MainThreadSlot::new(None);
pub(super) static SIDEBAR_TITLE_LABELS: LazyLock<MainThreadSlot<HashMap<usize, ObjPtr>>> =
    LazyLock::new(|| MainThreadSlot::new(HashMap::new()));
pub(super) static SIDEBAR_ICON_VIEWS: LazyLock<MainThreadSlot<HashMap<usize, ObjPtr>>> =
    LazyLock::new(|| MainThreadSlot::new(HashMap::new()));
pub(super) static SIDEBAR_UPDATE_DOTS: LazyLock<MainThreadSlot<HashMap<usize, ObjPtr>>> =
    LazyLock::new(|| MainThreadSlot::new(HashMap::new()));

pub(super) extern "C" fn external_link_mouse_entered(
    this: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) {
    unsafe {
        let color = settings_text_color(SettingsTextRole::AccentHover);
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
        let color = settings_text_color(SettingsTextRole::Accent);
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
    SIDEBAR_HOVER_PRIMED.store(false, Ordering::SeqCst);
    super::components::SettingsSidebar::hide_hover_highlight_immediately(sidebar_hover_highlight());
}

/// Keep a hidden hover origin at the clicked row for the next adjacent-row transition.
/// 点击后在当前行保留不可见的悬停起点，供下一次相邻条目切换使用。
pub(super) unsafe fn prime_sidebar_hover_after_selection(button: *mut AnyObject) {
    SIDEBAR_HOVERED.store(0, Ordering::SeqCst);
    let hover = sidebar_hover_highlight();
    if hover.is_null() || button.is_null() {
        SIDEBAR_HOVER_PRIMED.store(false, Ordering::SeqCst);
        return;
    }
    let frame: NSRect = msg_send![button, frame];
    SIDEBAR_HOVER_PRIMED.store(true, Ordering::SeqCst);
    SIDEBAR_HOVER_VISIBLE.store(true, Ordering::SeqCst);
    super::components::SettingsSidebar::prime_hover_highlight(hover, frame);
}

/// Find the visible sidebar button currently under the pointer instead of trusting a possibly
/// delayed tracking-area callback.
/// 根据当前指针位置查找可见的侧栏按钮，不直接信任可能延迟到达的 tracking area 回调。
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

/// Reconcile the shared hover pill with one concrete sidebar button.
/// 将共享悬停气泡与一个确定的侧栏按钮重新同步。
unsafe fn apply_sidebar_hover(button: *mut AnyObject, reentering_sidebar: bool) {
    if button.is_null() {
        return;
    }
    let tag: isize = msg_send![button, tag];
    if tag >= 0 && tag as usize == SIDEBAR_SELECTED.load(Ordering::SeqCst) {
        SIDEBAR_HOVERED.store(0, Ordering::SeqCst);
        SIDEBAR_HOVER_PRIMED.store(false, Ordering::SeqCst);
        super::components::SettingsSidebar::hide_hover_highlight(sidebar_hover_highlight());
        return;
    }
    SIDEBAR_HOVERED.store(button as usize, Ordering::SeqCst);
    let hover = sidebar_hover_highlight();
    if !hover.is_null() {
        let frame: NSRect = msg_send![button, frame];
        if SIDEBAR_HOVER_PRIMED.swap(false, Ordering::SeqCst) {
            super::components::SettingsSidebar::move_hover_highlight_after_selection(hover, frame);
        } else if reentering_sidebar {
            super::components::SettingsSidebar::move_hover_highlight_on_reentry(hover, frame);
        } else {
            super::components::SettingsSidebar::move_hover_highlight(hover, frame);
        }
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
        // 从详情区重新进入侧栏时可能漏掉子按钮的 mouseEntered 回调；根据当前指针位置恢复
        // 真实条目，避免悬停层停留在上一次的最后一栏。
        if let Some(button) = sidebar_button_under_pointer() {
            apply_sidebar_hover(button, true);
        } else {
            SIDEBAR_HOVERED.store(0, Ordering::SeqCst);
            super::components::SettingsSidebar::hide_hover_highlight(sidebar_hover_highlight());
        }
    }
}

pub(super) extern "C" fn sidebar_hover_tracker_mouse_exited(
    _this: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) {
    unsafe {
        SIDEBAR_HOVERED.store(0, Ordering::SeqCst);
        SIDEBAR_HOVER_PRIMED.store(false, Ordering::SeqCst);
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

/// Measure one shared row height for all localized sidebar titles.
/// 为所有本地化侧栏标题测量一套统一的行高。
pub(super) unsafe fn settings_sidebar_required_row_height(width: f64, titles: &[String]) -> f64 {
    let label_width = (width - 46.0 - 8.0).max(1.0);
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

    let mut required_height = 38.0f64;
    for title in titles {
        let title_ns = make_nsstring(title);
        let _: () = msg_send![field, setStringValue: title_ns];
        CFRelease(title_ns as *const c_void);
        let fonts: [*mut AnyObject; 2] = [
            msg_send![class!(NSFont), messageFontOfSize: 13.5f64],
            msg_send![class!(NSFont), boldSystemFontOfSize: 13.5f64],
        ];
        for font in fonts {
            let _: () = msg_send![field, setFont: font];
            let measured: NSSize =
                msg_send![field, sizeThatFits: NSSize::new(label_width, 10_000.0)];
            if measured.height.is_finite() && measured.height > 0.0 {
                // Keep a little vertical breathing room around two wrapped lines, while the
                // 38pt floor preserves the existing one-line sidebar rhythm.
                // 为两行文本保留少量上下空间，同时用 38pt 下限保持现有单行侧栏节奏。
                required_height = required_height.max((measured.height + 8.0).ceil());
            }
        }
    }
    release_obj(field);
    required_height
}

/// The tracker rect arrives precomputed against the live entry count (see
/// sidebar_tracking_rect in the component layer), so it can never drift behind the
/// sidebar's rows again -- the previous hardcoded 6-row rect left the 7th entry
/// outside the tracker, and leaving the sidebar through that last row stranded the
/// shared hover pill (no tracker exit fired to hide it).
/// tracker 矩形由调用方按实际条目数预先算好(见组件层的 sidebar_tracking_rect),
/// 不会再落后于侧栏行数——此前硬编码 6 行,第 7 个条目落在 tracker 之外,从末行
/// 底部离开侧栏时没有退出事件,共享悬停胶囊卡在末行不消失。
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
    // NSTrackingArea 不提供适合裸指针 registry 的所有权；显式保留一个 +1，重建侧栏时释放旧 tracker。
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
        // Reconcile against the current pointer before moving the shared hover surface.
        // tracking 回调可能在指针已移入其他区域或条目后才到达；移动共享悬停层前先按当前
        // 指针位置重新校准。
        let current = sidebar_button_under_pointer();
        if current != Some(button) {
            if let Some(current) = current {
                apply_sidebar_hover(current, false);
            } else {
                SIDEBAR_HOVERED.store(0, Ordering::SeqCst);
                super::components::SettingsSidebar::hide_hover_highlight(sidebar_hover_highlight());
            }
            return;
        }
        apply_sidebar_hover(button, false);
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
    // 高度按调用方给的值:滑块读数只有 18pt,满行高的控件由调用方自己决定(不在这里兜底)。
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
    let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 13.5f64];
    let _: () = msg_send![label, setFont: font];
    apply_settings_text_role(label, SettingsTextRole::Primary);
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
    apply_settings_text_role(link, SettingsTextRole::Accent);
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
    let field_text = settings_text_color(SettingsTextRole::Primary);
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

pub(super) use super::select::{
    clear_settings_select_registry, make_popup, settings_select_required_control_height,
    settings_select_set_item_symbol,
};

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
/// `default_value`:双击要恢复的默认值(None = 该行没有默认值,双击不特殊处理)。
/// Integer slider (NSSlider, min..=max, step 1). alloc +1; caller releases after adding to parent.
/// `default_value`: the value a double-click restores (None = this row has no default and a
/// double-click is left alone).
#[allow(clippy::too_many_arguments)] // 几何 + 区间 + 当前值 + 双击默认值,超过 7 个参数上限。/ geometry + range + value + default exceeds the 7-arg limit.
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
    // 拖动过程中连续发送 action:即时生效模式下运行时效果需实时跟随拖动。
    // Send the action continuously while dragging: in live-apply mode runtime effects must
    // follow the drag in real time.
    let _: () = msg_send![slider, setContinuous: true];
    let _: () = msg_send![slider, setMinValue: min as f64];
    let _: () = msg_send![slider, setMaxValue: max as f64];
    // 整数步进:1 格 = 1 个单位(线性 Mouse By Lines 滑块同款:0...10 step 1)。
    // Integer steps: 1 tick = 1 unit (same as LinearMouse's By Lines slider: 0...10 step 1).
    let _: () = msg_send![slider, setNumberOfTickMarks: (max - min + 1) as isize];
    let _: () = msg_send![slider, setAllowsTickMarkValuesOnly: true];
    let _: () = msg_send![slider, setIntegerValue: value];
    apply_slider_default(slider, default_value);
    slider
}

/// 连续取值的滑块(无刻度吸附),用于指针加速 / 跟踪速度这类小数区间。
///
/// 与整数滑块不同,这里刻意**不**连续发送 action:鼠标页的每次配置变更都会走
/// apply_config_change → pointer::apply(),而后者要重建 event system client 并等
/// ~30ms 让异步匹配完成。拖动中每个 mouse-dragged 都触发一次会让整个拖动卡死,
/// 因此数值在松手时(或点击轨道时)一次性生效 —— 对硬件属性来说这也是更合适的时机。
///
/// A continuous-value slider (no tick snapping), for fractional ranges such as pointer
/// acceleration / tracking speed.
///
/// Unlike the integer slider this deliberately does NOT send actions continuously: every mouse
/// config change goes through apply_config_change → pointer::apply(), which rebuilds the event
/// system client and waits ~30ms for asynchronous matching. Firing that on every mouse-dragged
/// event would stall the whole drag, so the value applies once on release (or on a track click)
/// -- which is also the better moment for a hardware property.
#[allow(clippy::too_many_arguments)] // 几何 + 区间 + 当前值 + 双击默认值,超过 7 个参数上限。/ geometry + range + value + default exceeds the 7-arg limit.
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
    // 不设刻度:NSSlider 默认即为连续取值,设了 allowsTickMarkValuesOnly 反而会吸附到整数。
    // No tick marks: NSSlider is continuous by default, and allowsTickMarkValuesOnly would
    // snap it to integers.
    let _: () = msg_send![slider, setDoubleValue: value];
    apply_slider_default(slider, default_value);
    slider
}

// ========== 滑杆:双击恢复默认值 / sliders: double-click restores the default ==========

/// NSSlider 的动态子类:双击(clickCount == 2)把值恢复为创建时登记的默认值,并复用既有的
/// target/action 链路(落盘 + 右侧只读读数刷新都由 handleControlChanged: 完成)。
///
/// 默认值存在 ObjC ivar(`ohMyTabDefaultValue`,f64)里,跟着控件生命周期走:不需要全局
/// 指针表,也就没有"滑杆销毁后指针复用读到脏默认值"的清理耦合。未登记默认值的滑杆,
/// 双击行为与普通 NSSlider 完全一致。
///
/// A dynamic NSSlider subclass: a double-click (clickCount == 2) restores the default registered
/// at creation and reuses the existing target/action chain (persisting the value and refreshing
/// the read-only readout both happen in handleControlChanged:).
///
/// The default lives in an ObjC ivar (`ohMyTabDefaultValue`, f64) so it follows the control's
/// lifetime -- no global pointer table, hence no cleanup coupling and no stale default if a
/// pointer is recycled. Sliders without a registered default behave exactly like a plain NSSlider.
static SETTINGS_SLIDER_CLS: OnceLock<usize> = OnceLock::new();
static SETTINGS_SLIDER_SUPERCLASS: OnceLock<usize> = OnceLock::new();

/// 默认值 ivar 名(注册类时添加);判"登记过默认值"用同名的 has-flag ivar。
/// The default-value ivar name (added at class registration); a has-flag ivar of the sibling name
/// distinguishes "registered a default" from "just a zeroed slot".
const SLIDER_DEFAULT_IVAR: &str = "ohMyTabDefaultValue";
const SLIDER_HAS_DEFAULT_IVAR: &str = "ohMyTabHasDefault";

/// 两个 ivar 在实例内的字节偏移(注册类后固定,一次性解析)。
/// 用偏移直访而不是 object_set/getInstanceVariable(见 ffi.rs 的说明)。
/// The two ivars' byte offsets inside the instance (fixed after registration, resolved once).
/// Direct offset access instead of object_set/getInstanceVariable (see the note in ffi.rs).
static SLIDER_IVAR_OFFSETS: OnceLock<(isize, isize)> = OnceLock::new();

fn settings_slider_class() -> *mut AnyObject {
    let cls = *SETTINGS_SLIDER_CLS.get_or_init(|| unsafe {
        let name = CString::new("OhMyTabSlider").unwrap();
        let superclass = class!(NSSlider) as *const _ as *mut AnyObject;
        let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
        // ivar 必须在 objc_registerClassPair 之前添加(对齐 8 字节 → log2 = 3;char 对齐 1)。
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
        // 注册后解析偏移(注册前 ivar_getOffset 不可用)。
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

/// 双击是否应恢复默认值(纯函数,便于单测):只有"确实是双击"且"登记过默认值"才接管。
/// Whether a double-click should restore the default (pure, for unit tests): we only take over
/// when it really is a double-click AND a default was registered.
fn slider_should_reset(click_count: isize, has_default: bool) -> bool {
    click_count == 2 && has_default
}

/// 默认值槽位(实例基址 + 偏移);类未初始化时返回 None。
/// The default-value slot (instance base + offset); None before the class is initialized.
unsafe fn slider_default_slot(slider: *mut AnyObject) -> Option<(*mut u8, *mut f64)> {
    let (flag_offset, value_offset) = *SLIDER_IVAR_OFFSETS.get()?;
    let base = slider as *mut u8;
    Some((
        base.offset(flag_offset),
        base.offset(value_offset) as *mut f64,
    ))
}

/// 读控件上登记的默认值(未登记 = None)。
/// Read the default registered on the control (None when absent).
unsafe fn slider_default_value(slider: *mut AnyObject) -> Option<f64> {
    let (flag, value) = slider_default_slot(slider)?;
    if *flag == 0 {
        None
    } else {
        Some(*value)
    }
}

/// 把默认值写进控件,并挂一条原生 tooltip 说明手势(双击是隐形手势,需要可发现性)。
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

/// 默认值的显示文本:整数值不带小数(3),否则保留 2 位(0.69)——与右侧读数风格一致。
/// Display text for the default: integral values without decimals (3), otherwise 2 decimals
/// (0.69) -- matching the readout's style.
fn settings_slider_display(value: f64) -> String {
    if (value.fract()).abs() < f64::EPSILON {
        format!("{}", value as i64)
    } else {
        format!("{value:.2}")
    }
}

/// mouseDown: 重写 —— 双击恢复默认值;其余情况交回 NSSlider(拖拽 / 点击轨道跳值)。
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
                // 走既有 action 链路:apply_control_field 落盘、on_control_changed 刷新读数。
                // 注意双击的第一次点击已经按普通点击生效过一次(可能有一次瞬时写入),这里
                // 的写入才是最终状态。action/target 实际都已绑定(见 bind_control),为空则跳过。
                // Reuse the existing action chain: apply_control_field persists the value and
                // on_control_changed refreshes the readout. Note the first click of the
                // double-click already took effect as a normal click (possibly one transient
                // write); this write is the final state. action/target are always bound in practice
                // (see bind_control); skip when either is null.
                let target: *mut AnyObject = msg_send![slider, target];
                // objc2 会按静态类型校验返回编码(`action` 是 SEL,编码 ':',不能用 `*const c_void`
                // 读),这里用原始 msgSend 取 action:既能拿到 nil(滑杆未绑定 action),也不触发校验。
                // objc2 validates the declared return encoding (`action` returns a SEL whose code is
                // ':', so it cannot be read as `*const c_void`). A raw msgSend both tolerates nil
                // (an unbound slider) and skips that check.
                type ActionFn = unsafe extern "C" fn(*mut AnyObject, Sel) -> *const c_void;
                let read_action: ActionFn = std::mem::transmute(objc_msgSend as *const ());
                let action: *const c_void = read_action(slider, sel!(action));
                if !action.is_null() && !target.is_null() {
                    // objc2 的 Sel 表示不了空选择器,这里把 action 按原始指针透传
                    // (该槽位本身就是 SEL)。
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
            // 不调 super:避免又开始一次拖拽跟踪。
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
    let button_frame: NSRect = msg_send![btn, frame];
    center_sidebar_label(label, button_frame.size.height);
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
    let color = settings_text_color(if hovered {
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

/// Fit the sidebar label to its measured cell height and center that frame in the shared row.
/// This compensates for AppKit's top-biased text drawing and is rerun when selection changes the
/// font weight.
/// 将侧栏文本 frame 收紧到 cell 测得的高度，再放到统一行高的垂直中心；这样可抵消 AppKit
/// 在大 frame 中偏上绘制文字的问题，并在选中态切换字重后重新计算。
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
/// 在侧边栏按钮图标右上角显示或隐藏小型更新标记。
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
    // 使用子 layer 而不是子 view，让标记只负责显示，不会拦截底层侧边栏按钮的点击。
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
                // 图标 backing layer 的 y 轴从底部向上，使用较大的 y 才会落在视觉右上角。
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
    let _: () = msg_send![dot, setCornerRadius: dot_size / 2.0];
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

/// Create the single shared hover surface used by all sidebar rows.
/// 创建由所有侧栏条目共用的悬浮背景层。
pub(super) unsafe fn make_sidebar_hover_highlight(
    parent: *mut AnyObject,
    x: f64,
    y: f64,
    w: f64,
    row_h: f64,
) -> *mut AnyObject {
    // Keep the hover surface below the buttons so it is purely visual and never intercepts input.
    // 将悬浮层放在按钮下方，使其只负责视觉效果，不拦截按钮输入。
    let hover: *mut AnyObject = msg_send![class!(NSView), alloc];
    let hover: *mut AnyObject = msg_send![
        hover,
        initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, row_h))
    ];
    let _: () = msg_send![hover, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![hover, layer];
    if !layer.is_null() {
        // HTML `.nav button:hover` = rgba(0,0,0,.045):4.5% black. palette.hover_bg is shared
        // with generic buttons' dark hover mapping, so the sidebar pill keeps its own token;
        // the dark palette (no HTML reference) keeps the previous white wash.
        // HTML `.nav button:hover` = rgba(0,0,0,.045):4.5% 黑。palette.hover_bg 与通用按钮的
        // 深色 hover 映射共用,侧栏胶囊改用自己的色值;深色无 HTML 参考,维持原白色浅底。
        let pill_hex = if settings_palette().dark {
            0xFFFFFF22u32
        } else {
            0x0000000Bu32
        };
        layer_set_background(layer, crate::ffi::hex_to_cg_color(pill_hex));
        let _: () = msg_send![layer, setCornerRadius: 10.0f64];
        let _: () = msg_send![layer, setMasksToBounds: true];
    }
    let _: () = msg_send![hover, setAlphaValue: 0.0f64];
    let _: () = msg_send![parent, addSubview: hover];
    SIDEBAR_HOVER_HIGHLIGHT
        .lock()
        .unwrap()
        .replace(ObjPtr::new(hover));
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
                                                         // 所有侧栏行使用统一测量高度，同时允许标题最多显示两行；更长的本地化标题只在两行后截断。
    let _: () = msg_send![label, setUsesSingleLineMode: false];
    let _: () = msg_send![label, setLineBreakMode: 0isize]; // NSLineBreakByWordWrapping
    if msg_send![label, respondsToSelector: sel!(setMaximumNumberOfLines:)] {
        let _: () = msg_send![label, setMaximumNumberOfLines: 2isize];
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
    // 自适应:贴顶、贴左、固定尺寸 / adaptive: top- and left-anchored, fixed size
    let _: () = msg_send![btn, setAutoresizingMask: 12u64];
    let _: () = msg_send![btn, setTarget: target];
    let _: () = msg_send![btn, setAction: sel!(handleSettingsSidebar:)];
    let _: () = msg_send![parent, addSubview: btn];
    release_obj(btn);
    btn
}

/// 区块小标题标签框的高度:`add_header` 把框的底边放在调用方给的 y 处,向上生长这么高。
/// 页面顶部节奏需要这个高度来换算游标,所以它由本模块导出,而不是各处再写一遍 20。
/// Height of the label box a section heading is drawn in: `add_header` places the box's bottom
/// edge at the caller's y and grows upwards by this much. The page-top rhythm needs the height to
/// convert cursors, so it is exported here instead of that 20 being written out again elsewhere.
pub(super) const SECTION_HEADER_H: f64 = 20.0;

/// 区块标题(加粗 label),加入父视图后 release。
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
    // 保留本地化原文：大写是英文拥有而 CJK 没有的层级通道，区块层级由字重、颜色和间距表达。
    let ns = make_nsstring(text);
    let _: () = msg_send![label, setStringValue: ns];
    CFRelease(ns as *const c_void);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let font: *mut AnyObject = msg_send![class!(NSFont), boldSystemFontOfSize: 12.0f64];
    let _: () = msg_send![label, setFont: font];
    apply_settings_text_role(label, SettingsTextRole::Secondary);
    // 自适应:宽度随父视图拉伸、顶部锚定(MinYMargin)。autoresizing = WidthSizable | MinYMargin = 2|8 = 10。
    // Adaptive: stretch width with the parent, stay top-anchored (MinYMargin).
    let _: () = msg_send![label, setAutoresizingMask: 10u64];
    let _: () = msg_send![parent, addSubview: label];
    release_obj(label);
}

/// Add a page title matching the HTML redesign's large, tight heading and return its height.
/// The 30pt size mirrors the mockup's `h1 { font-size: 30px }`; the top-padding metric itself
/// lives in the SettingsPageHeader component, which passes the adjusted cursor here.
/// 添加符合 HTML 设计的大号紧凑页面标题,并返回标题实际高度。30pt 对应设计稿的
/// `h1 { font-size: 30px }`;顶部留白度量归 SettingsPageHeader 组件所有,由其换算游标
/// 后传入。
///
/// `top_cursor` is the cursor used by the page layout. The title keeps the same top inset while
/// growing downward when the localized text needs additional wrapped lines.
/// `top_cursor` 是页面布局使用的游标。标题需要换行时保持顶部内缩不变，向下增长。
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
    set_field(label, text);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let font: *mut AnyObject = msg_send![class!(NSFont), boldSystemFontOfSize: 30.0f64];
    let _: () = msg_send![label, setFont: font];
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
    let _: () = msg_send![label, setAutoresizingMask: 10u64];
    let _: () = msg_send![parent, addSubview: label];
    release_obj(label);
    title_height
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

// Shadow clearance = key-shadow offset (8) + blur (24) = 32pt; 36 leaves margin.
// 阴影余量 = 主阴影偏移(8) + 模糊(24) = 32pt;取 36 留出余量。
pub(super) const SETTINGS_CARD_SHADOW_INSET: f64 = 36.0;

/// Draw the settings card shadow into pixels owned by the shadow view itself. This keeps the
/// blur inside the view's expanded frame instead of relying on a CALayer shadow crossing the
/// AppKit scroll/document hierarchy. Two layers mirror the HTML `.group` shadow:
/// `0 1px 2px rgba(0,0,0,.025)` (ambient) + `0 8px 24px rgba(0,0,0,.035)` (key).
/// 在阴影视图自身的像素范围内绘制设置卡片阴影。这样模糊区域位于扩大的视图边界内,
/// 不再依赖 CALayer 阴影穿过 AppKit 的滚动/文档视图层级。双层对应 HTML `.group` 阴影:
/// `0 1px 2px rgba(0,0,0,.025)`(环境)+ `0 8px 24px rgba(0,0,0,.035)`(主)。
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
        let path: *mut AnyObject = msg_send![
            class!(NSBezierPath),
            bezierPathWithRoundedRect: card_rect,
            xRadius: 14.0f64,
            yRadius: 14.0f64
        ];
        let fill = crate::ffi::hex_to_ns_color(settings_palette().card_bg);
        let _: () = msg_send![fill, set];
        // AppKit 的 y 轴向上,CSS 的 "0 Npx" 向下阴影对应 dy = -N。
        // 环境阴影固定 2.5% 黑;主阴影走 palette.shadow(浅色 0x0000000A ≈ 3.9%,即
        // HTML 的 .035;深色自动取更重的黑)。
        // AppKit's y axis points up, so a CSS "0 Npx" downward shadow maps to dy = -N.
        // The ambient layer is fixed 2.5% black; the key layer uses palette.shadow (light
        // 0x0000000A ≈ 3.9%, i.e. the HTML's .035; dark resolves to the heavier black).
        let key_color = crate::ffi::hex_to_ns_color(settings_palette().shadow);
        for (offset_y, blur, ambient) in [(-1.0f64, 2.0f64, true), (-8.0, 24.0, false)] {
            let shadow: *mut AnyObject = msg_send![class!(NSShadow), alloc];
            let shadow: *mut AnyObject = msg_send![shadow, init];
            let shadow_color = if ambient {
                crate::ffi::hex_to_ns_color(0x00000006u32)
            } else {
                key_color
            };
            let _: () = msg_send![shadow, setShadowColor: shadow_color];
            let _: () = msg_send![shadow, setShadowBlurRadius: blur];
            let _: () = msg_send![shadow, setShadowOffset: NSSize::new(0.0, offset_y)];
            let _: () = msg_send![shadow, set];
            let _: () = msg_send![path, fill];
            release_obj(shadow);
        }
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

/// Radial white highlight at the detail pane's top-right, mirroring the HTML `.main`
/// background: `radial-gradient(circle at 82% 0%, rgba(255,255,255,.96), transparent 34%)`.
/// The layer's flat `detail_bg` fill stays underneath; drawRect composites the glow over it.
/// 详情区右上角的径向白色高光,对应 HTML `.main` 背景:
/// `radial-gradient(circle at 82% 0%, rgba(255,255,255,.96), transparent 34%)`。
/// layer 的纯色 detail_bg 填充保留在下层,drawRect 把光斑叠在其上。
pub(super) extern "C" fn settings_pane_highlight_draw_rect(
    _self: *mut c_void,
    _cmd: Sel,
    _rect: NSRect,
) {
    unsafe {
        let bounds: NSRect = msg_send![_self as *mut AnyObject, bounds];
        if bounds.size.width <= 0.0 || bounds.size.height <= 0.0 {
            return;
        }
        let gfx: *mut AnyObject = msg_send![class!(NSGraphicsContext), currentContext];
        if gfx.is_null() {
            return;
        }
        // CGContext getter 返回 '^{CGContext=}',objc2 的 msg_send! 会为 *mut c_void 编出
        // '^v' 并在运行时编码校验中 panic(extern "C" drawRect 内 panic 即 abort);
        // 照 nsimage_from_cgimage 的惯例走裸 objc_msgSend 绕过校验。
        // The CGContext getter returns '^{CGContext=}', while objc2's msg_send! encodes a
        // *mut c_void return as '^v' and the runtime encoding check panics (a panic inside an
        // extern "C" drawRect aborts the process). Follow nsimage_from_cgimage's raw
        // objc_msgSend convention to bypass the check.
        let sel_cg_context = sel!(CGContext);
        type CGContextGetter = unsafe extern "C" fn(*mut AnyObject, Sel) -> *mut c_void;
        let send: CGContextGetter = std::mem::transmute(crate::ffi::objc_msgSend as *const ());
        let ctx: *mut c_void = send(gfx, sel_cg_context);
        if ctx.is_null() {
            return;
        }
        // drawRect 坐标系原点在左下;圆心在 82% 宽、顶边(y = h)。
        // drawRect's origin is bottom-left; the center sits at 82% width on the top edge.
        let center = crate::ffi::CGPoint {
            x: bounds.size.width * 0.82,
            y: bounds.size.height,
        };
        // CSS `circle` 默认射线 = 到最远角的距离(左下角);34% 色标 → endRadius。
        // CSS `circle`'s default ray reaches the farthest corner (bottom-left here);
        // the 34% color stop maps to endRadius.
        let farthest = (center.x * center.x + center.y * center.y).sqrt();
        let end_radius = farthest * 0.34;
        // 白 .96 → 白 0;option 2 = kCGGradientDrawsAfterEndLocation(终点外保持透明)。
        // 深色模式降到 5%:同样的白斑画在深底上是刺眼的白块,保留为极淡的一层冷光。
        // White .96 → white 0; option 2 = kCGGradientDrawsAfterEndLocation (transparent
        // beyond). Dark mode dials the start alpha down to 5%: the same blob reads as a
        // glaring white patch on the dark backdrop, so keep only a faint cool glow there.
        let start_alpha = if settings_palette().dark { 0.05 } else { 0.96 };
        let components: [f64; 8] = [1.0, 1.0, 1.0, start_alpha, 1.0, 1.0, 1.0, 0.0];
        let space = crate::ffi::CGColorSpaceCreateDeviceRGB();
        let gradient = crate::ffi::CGGradientCreateWithColorComponents(
            space,
            components.as_ptr(),
            std::ptr::null(),
            2,
        );
        if !gradient.is_null() {
            crate::ffi::CGContextDrawRadialGradient(
                ctx, gradient, center, 0.0, center, end_radius, 2,
            );
            crate::ffi::CGGradientRelease(gradient);
        }
        crate::ffi::CFRelease(space);
    }
}

pub(super) fn settings_pane_highlight_view_class() -> *mut AnyObject {
    static CLASS: OnceLock<usize> = OnceLock::new();
    *CLASS.get_or_init(|| unsafe {
        let name = CString::new("OhMyTabSettingsPaneHighlightView").unwrap();
        let superclass = class!(NSView) as *const _ as *mut AnyObject;
        let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
        let types_draw = CString::new("v@:{CGRect={CGPoint=dd}{CGSize=dd}}").unwrap();
        class_addMethod(
            cls,
            sel!(drawRect:),
            settings_pane_highlight_draw_rect as *mut c_void,
            types_draw.as_ptr(),
        );
        // 纯背景装饰:hitTest 返回 nil,绝不拦截内容区的鼠标事件。
        // Pure background decoration: hitTest returns nil so it never intercepts input.
        class_addMethod(
            cls,
            sel!(hitTest:),
            settings_card_shadow_hit_test as *mut c_void,
            CString::new("@@:{CGPoint=dd}").unwrap().as_ptr(),
        );
        objc_registerClassPair(cls);
        cls as usize
    }) as *mut AnyObject
}

/// Add a grouped card behind a section, matching the HTML redesign's light card surface.
/// The translucent white fill + hairline border live on the card's own layer; the two-layer
/// shadow (HTML `0 1px 2px` + `0 8px 24px`) is drawn by the dedicated shadow view behind it.
/// The HTML also stacks a backdrop blur under the surface, but a live blur on every scrolling
/// card re-samples the backdrop every frame and drags scrolling/window dragging down, while
/// over the pane's flat backdrop the blur is visually invisible anyway -- deliberately
/// omitted (the Plan-A tradeoff).
/// 在区块后添加分组卡片,对应 HTML 重设计的浅色卡片:半透明白填充 + 发丝边框由卡片自身
/// layer 承载;双层阴影(HTML `0 1px 2px` + `0 8px 24px`)由背后的专用阴影视图绘制。
/// HTML 在表面下还有 backdrop 模糊,但滚动卡片上的实时模糊每帧都要重新采样背景,拖累
/// 滚动和窗口拖动,而面板纯色背景下模糊本身在视觉上不可见——按方案 A 刻意省略。
pub(super) unsafe fn add_settings_card(
    parent: *mut AnyObject,
    frame: NSRect,
) -> (*mut AnyObject, *mut AnyObject) {
    if frame.size.width <= 0.0 || frame.size.height <= 0.0 {
        return (std::ptr::null_mut(), std::ptr::null_mut());
    }
    let palette = settings_palette();
    let card: *mut AnyObject = msg_send![class!(NSView), alloc];
    let card: *mut AnyObject = msg_send![card, initWithFrame: frame];
    let _: () = msg_send![card, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![card, layer];
    if !layer.is_null() {
        layer_set_background(layer, crate::ffi::hex_to_cg_color(palette.card_bg));
        let _: () = msg_send![layer, setCornerRadius: 14.0f64];
        // The outer shadow is a separate view, so the layer needs no clipping.
        // 外层阴影是独立视图,layer 自身无需裁切。
        let _: () = msg_send![layer, setMasksToBounds: false];
        crate::ffi::layer_set_border(layer, crate::ffi::hex_to_cg_color(palette.card_border));
        let _: () = msg_send![layer, setBorderWidth: 1.0f64];
    }
    // Insert below controls and labels so the card never intercepts their mouse events.
    // 整张卡片放在控件与标签下方,避免拦截它们的鼠标事件。
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
    let label_color = settings_text_color(SettingsTextRole::Primary);
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
    let title_color = settings_text_color(SettingsTextRole::Primary);
    let _: () = msg_send![title_label, setTextColor: title_color];
    let _: () = msg_send![title_label, setAlignment: -1isize]; // NSTextAlignmentNatural
                                                               // Long translated row titles use the shared row height as a bounded multi-line text block.
                                                               // 超长的翻译标题在统一 row 高度内使用有界多行文本块，避免单行尾部直接截断。
    let _: () = msg_send![title_label, setUsesSingleLineMode: false];
    let _: () = msg_send![title_label, setLineBreakMode: 0isize]; // NSLineBreakByWordWrapping
    if msg_send![title_label, respondsToSelector: sel!(setMaximumNumberOfLines:)] {
        let _: () = msg_send![title_label, setMaximumNumberOfLines: 3isize];
    }
    let _: () = msg_send![title_label, setPreferredMaxLayoutWidth: text_w.max(1.0)];
    let title_cell: *mut AnyObject = msg_send![title_label, cell];
    if !title_cell.is_null()
        && msg_send![title_cell, respondsToSelector: sel!(setTruncatesLastVisibleLine:)]
    {
        let _: () = msg_send![title_cell, setTruncatesLastVisibleLine: false];
    }
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
    let label_color = settings_text_color(SettingsTextRole::Primary);
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
    // 文档必须不低于视口:非翻转文档若矮于 clip 会被 AppKit 贴在视口底部,内容整体
    // 下移 (视口高-文档高)(快捷操作页 728<窗口 752 时整页下移 24pt 即此因)。
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
    // 文档**不随宽度自适应**(只用顶部锚定)。以前是 NSViewWidthSizable:系统把滚动条切成占宽的
    // legacy 时 clip 少 17pt,文档跟着缩,文档内宽度可伸缩的自绘控件(开关 38 → 21、行内按钮右缘
    // 左移 17pt)就被挤压变形。内容宽度由 scroller 的重排逻辑决定,不由 AppKit 的 tile 决定。
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

/// 把页面文档收紧到"真实内容 + 底部内边距",回收手写高度常量高估出来的底部死空白。
/// Tighten a page document to its real content plus the bottom padding, reclaiming the dead space
/// the hand-written page height constants left at the bottom.
///
/// 只改文档自身的高度,绝不手动搬子视图:顶部锚定(MinYMargin)的行会跟着保持贴顶,而底部锚定
/// 的页尾控件会跟着底边走 —— 两者都自动落到目标位置。手动搬会和 AppKit 的自动重排叠加成二次
/// 位移(见 `stable_document_height` 的说明)。
/// Only the document's own height changes; children are never moved by hand. Top-anchored
/// (MinYMargin) rows keep hugging the top and bottom-anchored page-foot controls follow the bottom,
/// so both land in place on their own; moving them by hand would stack a second shift on top of
/// AppKit's autoresize (see `stable_document_height`).
///
/// `minimum_height` 保证文档不短于页面视口:document 比 clip 矮时,非翻转的页面会把内容压到
/// 窗口底部。返回最终高度。
/// `minimum_height` keeps the document at least as tall as the page viewport: a document shorter
/// than its clip would push the content to the bottom of the window in a non-flipped page. Returns
/// the final height.
pub(super) unsafe fn fit_page_document_height(
    document: *mut AnyObject,
    minimum_height: f64,
    bottom_padding: f64,
) -> f64 {
    if document.is_null() {
        return minimum_height.max(1.0);
    }
    let frame: NSRect = msg_send![document, frame];
    let subviews: *mut AnyObject = msg_send![document, subviews];
    let count: usize = if subviews.is_null() {
        0
    } else {
        msg_send![subviews, count]
    };
    let mut children: Vec<(*mut AnyObject, NSRect, u64)> = Vec::with_capacity(count);
    let mut lowest = f64::INFINITY;
    for index in 0..count {
        let child: *mut AnyObject = msg_send![subviews, objectAtIndex: index as isize];
        if child.is_null() {
            continue;
        }
        let child_frame: NSRect = msg_send![child, frame];
        if child_frame.size.width <= 0.0 && child_frame.size.height <= 0.0 {
            continue;
        }
        let mask: u64 = msg_send![child, autoresizingMask];
        children.push((child, child_frame, mask));
        lowest = lowest.min(child_frame.origin.y);
    }
    if !lowest.is_finite() {
        return frame.size.height;
    }
    // surplus > 0:内容下方有多余空白;surplus < 0:内容比常量高,需要长高。
    // surplus > 0 means dead space below the content; surplus < 0 means the content outgrew the
    // constant and the document must grow.
    let surplus = lowest - bottom_padding;
    if surplus.abs() <= 0.5 {
        return frame.size.height;
    }
    let target = (frame.size.height - surplus).max(minimum_height);
    let shift = frame.size.height - target;
    let _: () = msg_send![document, setFrame: NSRect::new(
        frame.origin,
        NSSize::new(frame.size.width, target),
    )];
    // 文档变矮时,只有**顶部锚定**(MinYMargin)的子视图会被 AppKit 跟着顶边搬走;页面里的卡片
    // 和页尾控件是固定帧(mask 0x0 / MaxYMargin),原地不动。对后者补上同一个位移,整页内容才会
    // 像一块整体挪动;对前者再搬一次就是双重位移(这正是 stable_document_height 警告的坑)。
    // On shrink, AppKit only moves top-anchored (MinYMargin) subviews with the top edge; the page's
    // cards and foot controls keep fixed frames (mask 0x0 / MaxYMargin) and stay put. Shifting those
    // by the same delta moves the page as one block; shifting the top-anchored ones again would be
    // the double shift `stable_document_height` warns about.
    for (child, child_frame, mask) in children {
        if mask & 8 != 0 {
            continue;
        }
        let _: () = msg_send![child, setFrame: NSRect::new(
            NSPoint::new(child_frame.origin.x, child_frame.origin.y - shift),
            child_frame.size,
        )];
    }
    let mut lowest_after = f64::INFINITY;
    for index in 0..count {
        let child: *mut AnyObject = msg_send![subviews, objectAtIndex: index as isize];
        if child.is_null() {
            continue;
        }
        let child_frame: NSRect = msg_send![child, frame];
        if child_frame.size.width <= 0.0 && child_frame.size.height <= 0.0 {
            continue;
        }
        lowest_after = lowest_after.min(child_frame.origin.y);
    }
    log_debug!(
        "[settings] fit_page_document_height: doc_h={:.1} -> {:.1} lowest {:.1} -> {:.1} (bottom_padding={:.1} shift={:.1})",
        frame.size.height,
        target,
        lowest,
        lowest_after,
        bottom_padding,
        shift
    );
    target
}

/// Refit one settings page after an inline visibility change and refresh its scroller.
/// 条件行显隐后重新拟合单页文档高度，并刷新对应滚动条。
pub(super) unsafe fn refit_settings_page(scroll: *mut AnyObject) -> bool {
    if scroll.is_null() {
        return false;
    }
    let document: *mut AnyObject = msg_send![scroll, documentView];
    let clip: *mut AnyObject = msg_send![scroll, contentView];
    if document.is_null() || clip.is_null() {
        return false;
    }
    let clip_bounds: NSRect = msg_send![clip, bounds];
    let before: NSRect = msg_send![document, frame];
    let after = fit_page_document_height(
        document,
        clip_bounds.size.height,
        super::components::SettingsPageHeader::BOTTOM_PADDING,
    );
    if (after - before.size.height).abs() <= 0.5 {
        return false;
    }
    let _: () = msg_send![scroll, reflectScrolledClipView: clip];
    true
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
    if !cfg!(debug_assertions) {
        return;
    }
    // 开关走 argv:`--layout-debug` 显式开;`--smoke-settings-layout` 冒烟路径自带(它要校验
    // 的就是这套断言)。以前这里读环境变量、冒烟路径再 set_var 注入,现在两边都只看 argv。
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
        slider_should_reset, stable_document_height,
    };
    // 冒烟测试需要直接发 ObjC 消息(构造 NSEvent、驱动 mouseDown:)。
    // The smoke test sends ObjC messages directly (building an NSEvent, driving mouseDown:).
    use crate::ffi::release_obj;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send, sel};
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use std::ffi::c_void;

    #[test]
    fn slider_double_click_reset_only_applies_to_double_clicks_with_a_default() {
        // 只有"确实是双击"且"登记过默认值"才接管;其余情况交给 NSSlider 自己。
        // Only a real double-click with a registered default takes over; everything else goes to
        // NSSlider itself.
        assert!(slider_should_reset(2, true));
        assert!(!slider_should_reset(1, true));
        assert!(!slider_should_reset(3, true));
        assert!(!slider_should_reset(2, false));
        assert!(!slider_should_reset(0, false));
    }

    /// 冒烟(GUI/需要 AppKit):默认值 ivar 往返 + 由真实 NSEvent 驱动的双击恢复。
    /// 需要图形会话,故标 ignore;用 `cargo test -- --ignored slider_double_click_smoke` 跑。
    ///
    /// Smoke (GUI/AppKit): the default-value ivar round-trips and a real NSEvent double-click
    /// restores it. Requires a GUI session, hence #[ignore]; run with
    /// `cargo test -- --ignored slider_double_click_smoke`.
    #[test]
    #[ignore]
    fn slider_double_click_smoke() {
        unsafe {
            let slider = super::make_slider(0.0, 0.0, 120.0, 20.0, 0, 10, 7, Some(3.0));
            assert_eq!(super::slider_default_value(slider), Some(3.0));

            // 双击(clickCount = 2):值回到默认 3。
            // A double-click (clickCount = 2) restores the default 3.
            let double = make_click_event(2);
            super::settings_slider_mouse_down(slider as *mut c_void, sel!(mouseDown:), double);
            let value: f64 = msg_send![slider, doubleValue];
            assert_eq!(value, 3.0, "double-click must restore the default");

            // 单击仍交给 NSSlider(值不变;无窗口环境下超类只是不接收事件)。
            // A single click still goes to NSSlider (the value is untouched; with no window the
            // superclass simply does not track anything).
            let _: () = msg_send![slider, setDoubleValue: 7.0f64];
            let single = make_click_event(1);
            super::settings_slider_mouse_down(slider as *mut c_void, sel!(mouseDown:), single);
            let value: f64 = msg_send![slider, doubleValue];
            assert_eq!(value, 7.0, "a single click must not reset");

            // NSEvent 来自工厂方法(+0,autoreleased),不能手动释放;测试里没有 autorelease 池,
            // 这点泄漏对单次冒烟无影响。
            // The NSEvents come from a factory method (+0, autoreleased) and must not be released
            // manually; a test has no autorelease pool, and this one-off leak is irrelevant.
            release_obj(slider);
        }
    }

    /// 造一个左键按下事件(clickCount 可指定),供冒烟测试驱动 mouseDown:。
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
