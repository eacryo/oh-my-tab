//! Sparkle 2 updater integration.
//!
//! Sparkle is loaded dynamically instead of being linked at Rust build time. This keeps
//! `cargo run`/unit tests usable on a checkout that does not contain the native framework yet,
//! while a bundled `.app` automatically gets the real updater when `Sparkle.framework` is copied
//! into `Contents/Frameworks`.

use crate::ffi::{class_addMethod, make_nsstring, objc_allocateClassPair, objc_registerClassPair};
use crate::i18n::{t, tf};
use crate::{ffi::release_obj, log_info};
use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::ffi::{c_char, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, OnceLock};

/// The stable HTTPS endpoint that will host the appcast on the project's R2 custom domain.
pub(crate) const FEED_URL: &str = "https://download.oh-my-tab.app/appcast.xml";

const RTLD_NOW: i32 = 2;

extern "C" {
    fn objc_msgSend();
    fn objc_msgSendSuper();
    fn dlopen(filename: *const c_char, mode: i32) -> *mut c_void;
}

/// Sparkle keeps the updater and its user driver alive for the lifetime of the process. The
/// framework handle must remain open as well; unloading an Objective-C framework while its
/// objects are alive is unsafe.
struct UpdaterState {
    _framework_handle: *mut c_void,
    updater: *mut AnyObject,
}

unsafe impl Send for UpdaterState {}
unsafe impl Sync for UpdaterState {}

static STATE: OnceLock<Mutex<Option<UpdaterState>>> = OnceLock::new();

fn state() -> &'static Mutex<Option<UpdaterState>> {
    STATE.get_or_init(|| Mutex::new(None))
}

/// The custom progress window is owned by the user driver until Sparkle reports a result.
/// Pointers are represented as usize so the mutex can safely cross Rust's static Sync boundary.
struct UpdateUiState {
    window: usize,
    cancellation: usize,
    acknowledgement: usize,
    update_reply: usize,
    permission_reply: usize,
    retry_termination: usize,
    progress: usize,
    status_label: usize,
    cancel_button: usize,
    expected_length: u64,
    received_length: u64,
}

static UPDATE_UI_STATE: LazyLock<Mutex<UpdateUiState>> = LazyLock::new(|| {
    Mutex::new(UpdateUiState {
        window: 0,
        cancellation: 0,
        acknowledgement: 0,
        update_reply: 0,
        permission_reply: 0,
        retry_termination: 0,
        progress: 0,
        status_label: 0,
        cancel_button: 0,
        expected_length: 0,
        received_length: 0,
    })
});

/// The dynamically registered subclass is retained by the Objective-C runtime forever.
struct CustomDriverClass(*mut AnyObject);

unsafe impl Send for CustomDriverClass {}
unsafe impl Sync for CustomDriverClass {}

static CUSTOM_DRIVER_CLASS: OnceLock<CustomDriverClass> = OnceLock::new();
static CUSTOM_DRIVER_SUPERCLASS: OnceLock<usize> = OnceLock::new();

unsafe fn send_id(receiver: *mut AnyObject, selector: Sel) -> *mut AnyObject {
    type Fn = unsafe extern "C" fn(*mut AnyObject, Sel) -> *mut AnyObject;
    let f: Fn = std::mem::transmute(objc_msgSend as *const ());
    f(receiver, selector)
}

unsafe fn send_id4(
    receiver: *mut AnyObject,
    selector: Sel,
    first: *mut AnyObject,
    second: *mut AnyObject,
    third: *mut AnyObject,
    fourth: *mut AnyObject,
) -> *mut AnyObject {
    type Fn = unsafe extern "C" fn(
        *mut AnyObject,
        Sel,
        *mut AnyObject,
        *mut AnyObject,
        *mut AnyObject,
        *mut AnyObject,
    ) -> *mut AnyObject;
    let f: Fn = std::mem::transmute(objc_msgSend as *const ());
    f(receiver, selector, first, second, third, fourth)
}

unsafe fn send_void(receiver: *mut AnyObject, selector: Sel) {
    type Fn = unsafe extern "C" fn(*mut AnyObject, Sel);
    let f: Fn = std::mem::transmute(objc_msgSend as *const ());
    f(receiver, selector)
}

unsafe fn send_void_bool(receiver: *mut AnyObject, selector: Sel, value: bool) {
    type Fn = unsafe extern "C" fn(*mut AnyObject, Sel, bool);
    let f: Fn = std::mem::transmute(objc_msgSend as *const ());
    f(receiver, selector, value)
}

unsafe fn send_bool(receiver: *mut AnyObject, selector: Sel) -> bool {
    type Fn = unsafe extern "C" fn(*mut AnyObject, Sel) -> bool;
    let f: Fn = std::mem::transmute(objc_msgSend as *const ());
    f(receiver, selector)
}

unsafe fn send_bool_ptr(receiver: *mut AnyObject, selector: Sel, value: *mut c_void) -> bool {
    type Fn = unsafe extern "C" fn(*mut AnyObject, Sel, *mut c_void) -> bool;
    let f: Fn = std::mem::transmute(objc_msgSend as *const ());
    f(receiver, selector, value)
}

fn framework_candidates() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(path) = std::env::var("SPARKLE_FRAMEWORK_PATH") {
        if !path.trim().is_empty() {
            let path = PathBuf::from(path);
            paths.push(if path.extension().is_some_and(|ext| ext == "framework") {
                path.join("Sparkle")
            } else {
                path
            });
        }
    }

    if let Ok(executable) = std::env::current_exe() {
        if let Some(contents) = executable.parent().and_then(Path::parent) {
            paths.push(contents.join("Frameworks/Sparkle.framework/Sparkle"));
            paths.push(contents.join("PrivateFrameworks/Sparkle.framework/Sparkle"));
        }
    }

    paths.push(PathBuf::from(
        "/Library/Frameworks/Sparkle.framework/Sparkle",
    ));
    paths
}

unsafe fn load_framework() -> *mut c_void {
    // The class may already be registered when a host app loaded Sparkle for us.
    if AnyClass::get(c"SPUStandardUserDriver").is_some() {
        return std::ptr::null_mut();
    }

    for path in framework_candidates() {
        let path_string = path.to_string_lossy();
        let Ok(c_path) = CString::new(path_string.as_bytes()) else {
            continue;
        };
        let handle = dlopen(c_path.as_ptr(), RTLD_NOW);
        if !handle.is_null() {
            return handle;
        }
    }
    std::ptr::null_mut()
}

#[repr(C)]
struct ObjcSuper {
    receiver: *mut c_void,
    superclass: *mut c_void,
}

unsafe fn call_super_no_arguments(receiver: *mut c_void, selector: Sel) {
    type Fn = unsafe extern "C" fn(*mut ObjcSuper, Sel);
    let superclass = *CUSTOM_DRIVER_SUPERCLASS
        .get()
        .expect("Sparkle custom user-driver superclass is initialized");
    let mut objc_super = ObjcSuper {
        receiver,
        superclass: superclass as *mut c_void,
    };
    let f: Fn = std::mem::transmute(objc_msgSendSuper as *const ());
    f(&mut objc_super, selector);
}

#[repr(C)]
struct BlockLiteral {
    _isa: *mut c_void,
    _flags: i32,
    _reserved: i32,
    invoke: unsafe extern "C" fn(*mut c_void),
}

#[repr(C)]
struct ChoiceReplyBlockLiteral {
    _isa: *mut c_void,
    _flags: i32,
    _reserved: i32,
    invoke: unsafe extern "C" fn(*mut c_void, isize),
}

#[repr(C)]
struct ObjectReplyBlockLiteral {
    _isa: *mut c_void,
    _flags: i32,
    _reserved: i32,
    invoke: unsafe extern "C" fn(*mut c_void, *mut c_void),
}

unsafe fn copy_block(block: *mut c_void) -> *mut c_void {
    if block.is_null() {
        return std::ptr::null_mut();
    }
    send_id(block as *mut AnyObject, sel!(copy)) as *mut c_void
}

unsafe fn release_block(block: usize) {
    if block != 0 {
        release_obj(block as *mut AnyObject);
    }
}

unsafe fn invoke_block(block: usize) {
    if block != 0 {
        let literal = block as *mut BlockLiteral;
        ((*literal).invoke)(literal as *mut c_void);
    }
}

unsafe fn invoke_choice_reply(block: usize, choice: isize) {
    if block != 0 {
        let literal = block as *mut ChoiceReplyBlockLiteral;
        ((*literal).invoke)(literal as *mut c_void, choice);
    }
}

unsafe fn invoke_object_reply(block: usize, object: *mut c_void) {
    if block != 0 {
        let literal = block as *mut ObjectReplyBlockLiteral;
        ((*literal).invoke)(literal as *mut c_void, object);
    }
}

unsafe fn nsstring_to_string(value: *mut AnyObject) -> String {
    if value.is_null() {
        return String::new();
    }
    let utf8: *const c_char = msg_send![value, UTF8String];
    if utf8.is_null() {
        return String::new();
    }
    CStr::from_ptr(utf8).to_string_lossy().into_owned()
}

unsafe fn set_string_value(object: *mut AnyObject, value: &str) {
    let value_ns = make_nsstring(value);
    let _: () = msg_send![object, setStringValue: value_ns];
    crate::ffi::CFRelease(value_ns as *const c_void);
}

unsafe fn app_display_name() -> String {
    let bundle = send_id(
        class!(NSBundle) as *const _ as *mut AnyObject,
        sel!(mainBundle),
    );
    for key in ["CFBundleDisplayName", "CFBundleName"] {
        let key_ns = make_nsstring(key);
        let value: *mut AnyObject = msg_send![bundle, objectForInfoDictionaryKey: key_ns];
        crate::ffi::CFRelease(key_ns as *const c_void);
        let value = nsstring_to_string(value);
        if !value.is_empty() {
            return value;
        }
    }
    "Oh My Tab".to_string()
}

unsafe fn close_custom_update_window() {
    let mut ui = UPDATE_UI_STATE.lock().unwrap();
    if ui.window != 0 {
        let window = ui.window as *mut AnyObject;
        // 关闭窗口前先解除父视图对控件的引用，再释放 alloc 所有权，避免 AppKit 过度释放。
        // Remove subviews before releasing their alloc ownership to avoid AppKit over-release.
        let content: *mut AnyObject = msg_send![window, contentView];
        if !content.is_null() {
            loop {
                let subviews: *mut AnyObject = msg_send![content, subviews];
                let count: usize = if subviews.is_null() {
                    0
                } else {
                    msg_send![subviews, count]
                };
                if count == 0 {
                    break;
                }
                let child: *mut AnyObject = msg_send![subviews, objectAtIndex: 0usize];
                let _: () = msg_send![child, removeFromSuperview];
                release_obj(child);
            }
        }
        let _: () = msg_send![window, orderOut: std::ptr::null_mut::<AnyObject>()];
        let _: () = msg_send![window, close];
        release_obj(window);
        ui.window = 0;
    }
    release_block(ui.cancellation);
    ui.cancellation = 0;
    release_block(ui.acknowledgement);
    ui.acknowledgement = 0;
    release_block(ui.update_reply);
    ui.update_reply = 0;
    release_block(ui.permission_reply);
    ui.permission_reply = 0;
    release_block(ui.retry_termination);
    ui.retry_termination = 0;
    ui.progress = 0;
    ui.status_label = 0;
    ui.cancel_button = 0;
    ui.expected_length = 0;
    ui.received_length = 0;
}

unsafe fn make_custom_update_window(driver: *mut c_void, cancellation: *mut c_void) {
    close_custom_update_window();

    let window_frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(520.0, 190.0));
    // NSWindowStyleMaskTitled = 1; NSBackingStoreBuffered = 2.
    let window: *mut AnyObject = msg_send![class!(NSWindow), alloc];
    let window: *mut AnyObject = msg_send![
        window,
        initWithContentRect: window_frame,
        styleMask: 1u64,
        backing: 2u64,
        defer: false
    ];
    if window.is_null() {
        return;
    }
    let title = make_nsstring(&t("settings.update_window_title"));
    let _: () = msg_send![window, setTitle: title];
    crate::ffi::CFRelease(title as *const c_void);
    let _: () = msg_send![window, setReleasedWhenClosed: false];

    let content: *mut AnyObject = msg_send![window, contentView];

    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let label: *mut AnyObject = msg_send![
        label,
        initWithFrame: NSRect::new(NSPoint::new(32.0, 118.0), NSSize::new(456.0, 28.0))
    ];
    let text = make_nsstring(&t("settings.update_checking"));
    let _: () = msg_send![label, setStringValue: text];
    crate::ffi::CFRelease(text as *const c_void);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let _: () = msg_send![label, setSelectable: false];
    let font: *mut AnyObject = msg_send![class!(NSFont), boldSystemFontOfSize: 18.0f64];
    let _: () = msg_send![label, setFont: font];
    let _: () = msg_send![content, addSubview: label];

    let progress: *mut AnyObject = msg_send![class!(NSProgressIndicator), alloc];
    let progress: *mut AnyObject = msg_send![
        progress,
        initWithFrame: NSRect::new(NSPoint::new(32.0, 78.0), NSSize::new(456.0, 16.0))
    ];
    let _: () = msg_send![progress, setIndeterminate: true];
    let _: () = msg_send![progress, startAnimation: std::ptr::null_mut::<AnyObject>()];
    let _: () = msg_send![content, addSubview: progress];

    let cancel: *mut AnyObject = msg_send![class!(NSButton), alloc];
    let cancel: *mut AnyObject = msg_send![
        cancel,
        initWithFrame: NSRect::new(NSPoint::new(350.0, 24.0), NSSize::new(138.0, 34.0))
    ];
    let cancel_title = make_nsstring(&t("settings.btn_cancel"));
    let _: () = msg_send![cancel, setTitle: cancel_title];
    crate::ffi::CFRelease(cancel_title as *const c_void);
    let _: () = msg_send![cancel, setBezelStyle: 1u64];
    let _: () = msg_send![cancel, setTarget: driver as *mut AnyObject];
    let _: () = msg_send![cancel, setAction: sel!(cancelCustomUpdateCheck:)];
    let _: () = msg_send![content, addSubview: cancel];

    let _: () = msg_send![window, center];
    let _: () = msg_send![window, makeKeyAndOrderFront: std::ptr::null_mut::<AnyObject>()];

    let copied_cancellation = copy_block(cancellation) as usize;
    let mut ui = UPDATE_UI_STATE.lock().unwrap();
    ui.window = window as usize;
    ui.cancellation = copied_cancellation;
}

unsafe fn make_custom_result_window(
    driver: *mut c_void,
    acknowledgement: *mut c_void,
    title_text: &str,
    message_text: &str,
) {
    close_custom_update_window();

    let window_frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(520.0, 250.0));
    // NSWindowStyleMaskTitled = 1; NSBackingStoreBuffered = 2.
    let window: *mut AnyObject = msg_send![class!(NSWindow), alloc];
    let window: *mut AnyObject = msg_send![
        window,
        initWithContentRect: window_frame,
        styleMask: 1u64,
        backing: 2u64,
        defer: false
    ];
    if window.is_null() {
        return;
    }
    let window_title = make_nsstring(&t("settings.update_window_title"));
    let _: () = msg_send![window, setTitle: window_title];
    crate::ffi::CFRelease(window_title as *const c_void);
    let _: () = msg_send![window, setReleasedWhenClosed: false];

    let content: *mut AnyObject = msg_send![window, contentView];

    let title: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let title: *mut AnyObject = msg_send![
        title,
        initWithFrame: NSRect::new(NSPoint::new(32.0, 164.0), NSSize::new(456.0, 32.0))
    ];
    let title_ns = make_nsstring(title_text);
    let _: () = msg_send![title, setStringValue: title_ns];
    crate::ffi::CFRelease(title_ns as *const c_void);
    let _: () = msg_send![title, setBezeled: false];
    let _: () = msg_send![title, setDrawsBackground: false];
    let _: () = msg_send![title, setEditable: false];
    let _: () = msg_send![title, setSelectable: false];
    let title_font: *mut AnyObject = msg_send![class!(NSFont), boldSystemFontOfSize: 22.0f64];
    let _: () = msg_send![title, setFont: title_font];
    let _: () = msg_send![content, addSubview: title];

    let message: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let message: *mut AnyObject = msg_send![
        message,
        initWithFrame: NSRect::new(NSPoint::new(32.0, 106.0), NSSize::new(456.0, 44.0))
    ];
    let message_ns = make_nsstring(message_text);
    let _: () = msg_send![message, setStringValue: message_ns];
    crate::ffi::CFRelease(message_ns as *const c_void);
    let _: () = msg_send![message, setBezeled: false];
    let _: () = msg_send![message, setDrawsBackground: false];
    let _: () = msg_send![message, setEditable: false];
    let _: () = msg_send![message, setSelectable: false];
    let message_font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 16.0f64];
    let _: () = msg_send![message, setFont: message_font];
    let _: () = msg_send![message, setLineBreakMode: 0u64];
    let _: () = msg_send![message, setMaximumNumberOfLines: 0isize];
    let _: () = msg_send![content, addSubview: message];

    let ok: *mut AnyObject = msg_send![class!(NSButton), alloc];
    let ok: *mut AnyObject = msg_send![
        ok,
        initWithFrame: NSRect::new(NSPoint::new(350.0, 24.0), NSSize::new(138.0, 34.0))
    ];
    let ok_title = make_nsstring(&t("settings.btn_ok"));
    let _: () = msg_send![ok, setTitle: ok_title];
    crate::ffi::CFRelease(ok_title as *const c_void);
    let _: () = msg_send![ok, setBezelStyle: 1u64];
    let _: () = msg_send![ok, setTarget: driver as *mut AnyObject];
    let _: () = msg_send![ok, setAction: sel!(acknowledgeCustomUpdateResult:)];
    let _: () = msg_send![content, addSubview: ok];

    let _: () = msg_send![window, center];
    let _: () = msg_send![window, makeKeyAndOrderFront: std::ptr::null_mut::<AnyObject>()];

    let copied_acknowledgement = copy_block(acknowledgement) as usize;
    let mut ui = UPDATE_UI_STATE.lock().unwrap();
    ui.window = window as usize;
    ui.acknowledgement = copied_acknowledgement;
}

/// 创建首次运行的更新权限窗口，避免 Sparkle 的标准权限界面带出应用图标。
/// Build the first-run update permission window without Sparkle's standard icon-bearing UI.
unsafe fn make_custom_permission_window(driver: *mut c_void, reply: *mut c_void) {
    close_custom_update_window();
    let window_frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(560.0, 240.0));
    let window: *mut AnyObject = msg_send![class!(NSWindow), alloc];
    let window: *mut AnyObject = msg_send![
        window,
        initWithContentRect: window_frame,
        styleMask: 1u64,
        backing: 2u64,
        defer: false
    ];
    if window.is_null() {
        return;
    }
    let window_title = make_nsstring(&t("settings.update_window_title"));
    let _: () = msg_send![window, setTitle: window_title];
    crate::ffi::CFRelease(window_title as *const c_void);
    let _: () = msg_send![window, setReleasedWhenClosed: false];
    let content: *mut AnyObject = msg_send![window, contentView];
    let app = app_display_name();

    let title: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let title: *mut AnyObject = msg_send![
        title,
        initWithFrame: NSRect::new(NSPoint::new(32.0, 172.0), NSSize::new(496.0, 32.0))
    ];
    set_string_value(title, &t("settings.update_permission_title"));
    let _: () = msg_send![title, setBezeled: false];
    let _: () = msg_send![title, setDrawsBackground: false];
    let _: () = msg_send![title, setEditable: false];
    let _: () = msg_send![title, setSelectable: false];
    let title_font: *mut AnyObject = msg_send![class!(NSFont), boldSystemFontOfSize: 20.0f64];
    let _: () = msg_send![title, setFont: title_font];
    let _: () = msg_send![content, addSubview: title];

    let message: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let message: *mut AnyObject = msg_send![
        message,
        initWithFrame: NSRect::new(NSPoint::new(32.0, 112.0), NSSize::new(496.0, 44.0))
    ];
    let permission_message = tf("settings.update_permission_message", &[("app", &app)]);
    set_string_value(message, &permission_message);
    let _: () = msg_send![message, setBezeled: false];
    let _: () = msg_send![message, setDrawsBackground: false];
    let _: () = msg_send![message, setEditable: false];
    let _: () = msg_send![message, setSelectable: false];
    let message_font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 15.0f64];
    let _: () = msg_send![message, setFont: message_font];
    let _: () = msg_send![message, setLineBreakMode: 0u64];
    let _: () = msg_send![message, setMaximumNumberOfLines: 0isize];
    let _: () = msg_send![content, addSubview: message];

    let later: *mut AnyObject = msg_send![class!(NSButton), alloc];
    let later: *mut AnyObject = msg_send![
        later,
        initWithFrame: NSRect::new(NSPoint::new(32.0, 28.0), NSSize::new(180.0, 36.0))
    ];
    let later_title = make_nsstring(&t("settings.btn_not_now"));
    let _: () = msg_send![later, setTitle: later_title];
    crate::ffi::CFRelease(later_title as *const c_void);
    let _: () = msg_send![later, setBezelStyle: 1u64];
    let _: () = msg_send![later, setTarget: driver as *mut AnyObject];
    let _: () = msg_send![later, setAction: sel!(deferAutomaticUpdate:)];
    let _: () = msg_send![content, addSubview: later];

    let enable: *mut AnyObject = msg_send![class!(NSButton), alloc];
    let enable: *mut AnyObject = msg_send![
        enable,
        initWithFrame: NSRect::new(NSPoint::new(348.0, 28.0), NSSize::new(180.0, 36.0))
    ];
    let enable_title = make_nsstring(&t("settings.btn_enable_auto_check"));
    let _: () = msg_send![enable, setTitle: enable_title];
    crate::ffi::CFRelease(enable_title as *const c_void);
    let _: () = msg_send![enable, setBezelStyle: 1u64];
    let _: () = msg_send![enable, setTarget: driver as *mut AnyObject];
    let _: () = msg_send![enable, setAction: sel!(allowAutomaticUpdate:)];
    let _: () = msg_send![content, addSubview: enable];

    let copied_reply = copy_block(reply) as usize;
    let mut ui = UPDATE_UI_STATE.lock().unwrap();
    ui.window = window as usize;
    ui.permission_reply = copied_reply;

    let _: () = msg_send![window, center];
    let _: () = msg_send![window, makeKeyAndOrderFront: std::ptr::null_mut::<AnyObject>()];
}

unsafe fn answer_update_permission(enabled: bool) {
    let reply = {
        let mut ui = UPDATE_UI_STATE.lock().unwrap();
        let reply = ui.permission_reply;
        ui.permission_reply = 0;
        reply
    };
    close_custom_update_window();

    let response_class = AnyClass::get(c"SUUpdatePermissionResponse")
        .expect("Sparkle update permission response class is loaded");
    let response_allocated = send_id(response_class as *const _ as *mut AnyObject, sel!(alloc));
    type Fn = unsafe extern "C" fn(*mut AnyObject, Sel, bool, bool) -> *mut AnyObject;
    let f: Fn = std::mem::transmute(objc_msgSend as *const ());
    let response = f(
        response_allocated,
        sel!(initWithAutomaticUpdateChecks:sendSystemProfile:),
        enabled,
        false,
    );
    invoke_object_reply(reply, response as *mut c_void);
    release_obj(response);
    release_block(reply);
}

extern "C" fn show_update_permission(
    this: *mut c_void,
    _cmd: Sel,
    _request: *mut c_void,
    reply: *mut c_void,
) {
    unsafe { make_custom_permission_window(this, reply) };
}

extern "C" fn allow_automatic_update(_this: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    unsafe { answer_update_permission(true) };
}

extern "C" fn defer_automatic_update(_this: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    unsafe { answer_update_permission(false) };
}

/// 创建自定义更新提示，完全绕过 Sparkle 默认会显示应用图标的弹窗。
/// Build the update-available prompt without using Sparkle's standard alert.
unsafe fn make_custom_update_found_window(
    driver: *mut c_void,
    item: *mut c_void,
    reply: *mut c_void,
) {
    close_custom_update_window();

    let app = app_display_name();
    let item = item as *mut AnyObject;
    let version = nsstring_to_string(msg_send![item, displayVersionString]);
    let version = if version.is_empty() {
        nsstring_to_string(msg_send![item, versionString])
    } else {
        version
    };
    let version = if version.is_empty() {
        "?".to_string()
    } else {
        version
    };
    let title_text = tf("settings.update_available_title", &[("app", &app)]);
    let message_text = tf(
        "settings.update_available_message",
        &[("app", &app), ("version", &version)],
    );

    let updater = state()
        .lock()
        .unwrap()
        .as_ref()
        .map_or(std::ptr::null_mut(), |current| current.updater);
    let automatically_downloads = if updater.is_null() {
        false
    } else {
        send_bool(updater, sel!(automaticallyDownloadsUpdates))
    };
    let allows_automatic_updates = if updater.is_null() {
        false
    } else {
        send_bool(updater, sel!(allowsAutomaticUpdates))
    };

    let window_frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(640.0, 292.0));
    // NSWindowStyleMaskTitled = 1; NSBackingStoreBuffered = 2.
    let window: *mut AnyObject = msg_send![class!(NSWindow), alloc];
    let window: *mut AnyObject = msg_send![
        window,
        initWithContentRect: window_frame,
        styleMask: 1u64,
        backing: 2u64,
        defer: false
    ];
    if window.is_null() {
        return;
    }
    let window_title = make_nsstring(&t("settings.update_window_title"));
    let _: () = msg_send![window, setTitle: window_title];
    crate::ffi::CFRelease(window_title as *const c_void);
    let _: () = msg_send![window, setReleasedWhenClosed: false];

    let content: *mut AnyObject = msg_send![window, contentView];

    let title: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let title: *mut AnyObject = msg_send![
        title,
        initWithFrame: NSRect::new(NSPoint::new(32.0, 228.0), NSSize::new(576.0, 32.0))
    ];
    let title_ns = make_nsstring(&title_text);
    let _: () = msg_send![title, setStringValue: title_ns];
    crate::ffi::CFRelease(title_ns as *const c_void);
    let _: () = msg_send![title, setBezeled: false];
    let _: () = msg_send![title, setDrawsBackground: false];
    let _: () = msg_send![title, setEditable: false];
    let _: () = msg_send![title, setSelectable: false];
    let title_font: *mut AnyObject = msg_send![class!(NSFont), boldSystemFontOfSize: 22.0f64];
    let _: () = msg_send![title, setFont: title_font];
    let _: () = msg_send![content, addSubview: title];

    let message: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let message: *mut AnyObject = msg_send![
        message,
        initWithFrame: NSRect::new(NSPoint::new(32.0, 165.0), NSSize::new(576.0, 48.0))
    ];
    let message_ns = make_nsstring(&message_text);
    let _: () = msg_send![message, setStringValue: message_ns];
    crate::ffi::CFRelease(message_ns as *const c_void);
    let _: () = msg_send![message, setBezeled: false];
    let _: () = msg_send![message, setDrawsBackground: false];
    let _: () = msg_send![message, setEditable: false];
    let _: () = msg_send![message, setSelectable: false];
    let message_font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 16.0f64];
    let _: () = msg_send![message, setFont: message_font];
    let _: () = msg_send![message, setLineBreakMode: 0u64];
    let _: () = msg_send![message, setMaximumNumberOfLines: 0isize];
    let _: () = msg_send![content, addSubview: message];

    let automatic: *mut AnyObject = msg_send![class!(NSButton), alloc];
    let automatic: *mut AnyObject = msg_send![
        automatic,
        initWithFrame: NSRect::new(NSPoint::new(32.0, 122.0), NSSize::new(576.0, 28.0))
    ];
    let automatic_title = make_nsstring(&t("settings.update_automatically_download"));
    let _: () = msg_send![automatic, setTitle: automatic_title];
    crate::ffi::CFRelease(automatic_title as *const c_void);
    // NSButtonTypeSwitch = 3，macOS 原生复选框样式。
    // NSButtonTypeSwitch = 3, the native macOS checkbox style.
    let _: () = msg_send![automatic, setButtonType: 3isize];
    let _: () = msg_send![
        automatic,
        setState: if automatically_downloads { 1isize } else { 0isize }
    ];
    let _: () = msg_send![automatic, setEnabled: allows_automatic_updates];
    let _: () = msg_send![automatic, setTarget: driver as *mut AnyObject];
    let _: () = msg_send![automatic, setAction: sel!(toggleAutomaticUpdate:)];
    let _: () = msg_send![content, addSubview: automatic];

    let skip: *mut AnyObject = msg_send![class!(NSButton), alloc];
    let skip: *mut AnyObject = msg_send![
        skip,
        initWithFrame: NSRect::new(NSPoint::new(32.0, 32.0), NSSize::new(166.0, 36.0))
    ];
    let skip_title = make_nsstring(&t("settings.btn_skip_version"));
    let _: () = msg_send![skip, setTitle: skip_title];
    crate::ffi::CFRelease(skip_title as *const c_void);
    let _: () = msg_send![skip, setBezelStyle: 1u64];
    let _: () = msg_send![skip, setTarget: driver as *mut AnyObject];
    let _: () = msg_send![skip, setAction: sel!(skipCustomUpdate:)];
    let _: () = msg_send![content, addSubview: skip];

    let later: *mut AnyObject = msg_send![class!(NSButton), alloc];
    let later: *mut AnyObject = msg_send![
        later,
        initWithFrame: NSRect::new(NSPoint::new(220.0, 32.0), NSSize::new(166.0, 36.0))
    ];
    let later_title = make_nsstring(&t("settings.btn_remind_later"));
    let _: () = msg_send![later, setTitle: later_title];
    crate::ffi::CFRelease(later_title as *const c_void);
    let _: () = msg_send![later, setBezelStyle: 1u64];
    let _: () = msg_send![later, setTarget: driver as *mut AnyObject];
    let _: () = msg_send![later, setAction: sel!(dismissCustomUpdate:)];
    let _: () = msg_send![content, addSubview: later];

    let install: *mut AnyObject = msg_send![class!(NSButton), alloc];
    let install: *mut AnyObject = msg_send![
        install,
        initWithFrame: NSRect::new(NSPoint::new(408.0, 32.0), NSSize::new(200.0, 36.0))
    ];
    let install_title = make_nsstring(&t("settings.btn_install_update"));
    let _: () = msg_send![install, setTitle: install_title];
    crate::ffi::CFRelease(install_title as *const c_void);
    let _: () = msg_send![install, setBezelStyle: 1u64];
    let key_equivalent = make_nsstring("\r");
    let _: () = msg_send![install, setKeyEquivalent: key_equivalent];
    crate::ffi::CFRelease(key_equivalent as *const c_void);
    let _: () = msg_send![install, setTarget: driver as *mut AnyObject];
    let _: () = msg_send![install, setAction: sel!(installCustomUpdate:)];
    let _: () = msg_send![content, addSubview: install];

    let _: () = msg_send![window, center];
    let _: () = msg_send![window, makeKeyAndOrderFront: std::ptr::null_mut::<AnyObject>()];

    let copied_reply = copy_block(reply) as usize;
    let mut ui = UPDATE_UI_STATE.lock().unwrap();
    ui.window = window as usize;
    ui.update_reply = copied_reply;
}

fn format_download_bytes(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    if bytes == 0 {
        return "0 MB".to_string();
    }
    format!("{:.1} MB", bytes as f64 / MB)
}

unsafe fn update_download_progress_ui() {
    let (progress, status_label, expected, received) = {
        let ui = UPDATE_UI_STATE.lock().unwrap();
        (
            ui.progress,
            ui.status_label,
            ui.expected_length,
            ui.received_length,
        )
    };
    if progress == 0 {
        return;
    }

    let progress = progress as *mut AnyObject;
    if expected > 0 {
        let fraction = (received as f64 / expected as f64).clamp(0.0, 1.0);
        let _: () = msg_send![progress, setIndeterminate: false];
        let _: () = msg_send![progress, setDoubleValue: fraction];
    }
    if status_label != 0 {
        let downloaded = format_download_bytes(received);
        let total = if expected > 0 {
            format_download_bytes(expected)
        } else {
            "—".to_string()
        };
        let text = tf(
            "settings.update_download_progress",
            &[("downloaded", &downloaded), ("total", &total)],
        );
        set_string_value(status_label as *mut AnyObject, &text);
    }
}

/// 创建下载/解压进度窗口，不使用 Sparkle 标准窗口，因此不会显示应用图标。
/// Build the download/extraction window without Sparkle's standard icon-bearing window.
unsafe fn make_custom_download_window(driver: *mut c_void, cancellation: *mut c_void) {
    close_custom_update_window();

    let app = app_display_name();
    let window_frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(560.0, 238.0));
    let window: *mut AnyObject = msg_send![class!(NSWindow), alloc];
    let window: *mut AnyObject = msg_send![
        window,
        initWithContentRect: window_frame,
        styleMask: 1u64,
        backing: 2u64,
        defer: false
    ];
    if window.is_null() {
        return;
    }
    let window_title = tf("settings.update_downloading_window_title", &[("app", &app)]);
    let window_title_ns = make_nsstring(&window_title);
    let _: () = msg_send![window, setTitle: window_title_ns];
    crate::ffi::CFRelease(window_title_ns as *const c_void);
    let _: () = msg_send![window, setReleasedWhenClosed: false];
    let content: *mut AnyObject = msg_send![window, contentView];

    let title: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let title: *mut AnyObject = msg_send![
        title,
        initWithFrame: NSRect::new(NSPoint::new(32.0, 168.0), NSSize::new(496.0, 32.0))
    ];
    set_string_value(title, &t("settings.update_downloading"));
    let _: () = msg_send![title, setBezeled: false];
    let _: () = msg_send![title, setDrawsBackground: false];
    let _: () = msg_send![title, setEditable: false];
    let _: () = msg_send![title, setSelectable: false];
    let title_font: *mut AnyObject = msg_send![class!(NSFont), boldSystemFontOfSize: 22.0f64];
    let _: () = msg_send![title, setFont: title_font];
    let _: () = msg_send![content, addSubview: title];

    let progress: *mut AnyObject = msg_send![class!(NSProgressIndicator), alloc];
    let progress: *mut AnyObject = msg_send![
        progress,
        initWithFrame: NSRect::new(NSPoint::new(32.0, 116.0), NSSize::new(496.0, 18.0))
    ];
    let _: () = msg_send![progress, setIndeterminate: false];
    let _: () = msg_send![progress, setMinValue: 0.0f64];
    let _: () = msg_send![progress, setMaxValue: 1.0f64];
    let _: () = msg_send![progress, setDoubleValue: 0.0f64];
    let _: () = msg_send![content, addSubview: progress];

    let status: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let status: *mut AnyObject = msg_send![
        status,
        initWithFrame: NSRect::new(NSPoint::new(32.0, 82.0), NSSize::new(496.0, 24.0))
    ];
    let initial_status = tf(
        "settings.update_download_progress",
        &[("downloaded", "0 MB"), ("total", "—")],
    );
    set_string_value(status, &initial_status);
    let _: () = msg_send![status, setBezeled: false];
    let _: () = msg_send![status, setDrawsBackground: false];
    let _: () = msg_send![status, setEditable: false];
    let _: () = msg_send![status, setSelectable: false];
    let status_font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 14.0f64];
    let _: () = msg_send![status, setFont: status_font];
    let _: () = msg_send![content, addSubview: status];

    let cancel: *mut AnyObject = msg_send![class!(NSButton), alloc];
    let cancel: *mut AnyObject = msg_send![
        cancel,
        initWithFrame: NSRect::new(NSPoint::new(390.0, 28.0), NSSize::new(138.0, 36.0))
    ];
    let cancel_title = make_nsstring(&t("settings.btn_cancel"));
    let _: () = msg_send![cancel, setTitle: cancel_title];
    crate::ffi::CFRelease(cancel_title as *const c_void);
    let _: () = msg_send![cancel, setBezelStyle: 1u64];
    let _: () = msg_send![cancel, setTarget: driver as *mut AnyObject];
    let _: () = msg_send![cancel, setAction: sel!(cancelCustomDownload:)];
    let _: () = msg_send![content, addSubview: cancel];

    let copied_cancellation = copy_block(cancellation) as usize;
    let mut ui = UPDATE_UI_STATE.lock().unwrap();
    ui.window = window as usize;
    ui.cancellation = copied_cancellation;
    ui.progress = progress as usize;
    ui.status_label = status as usize;
    ui.cancel_button = cancel as usize;
    ui.expected_length = 0;
    ui.received_length = 0;

    let _: () = msg_send![window, center];
    let _: () = msg_send![window, makeKeyAndOrderFront: std::ptr::null_mut::<AnyObject>()];
}

unsafe fn set_download_status(text: &str, indeterminate: bool) {
    let (progress, status_label, cancel_button) = {
        let ui = UPDATE_UI_STATE.lock().unwrap();
        (ui.progress, ui.status_label, ui.cancel_button)
    };
    if progress != 0 {
        let progress = progress as *mut AnyObject;
        let _: () = msg_send![progress, setIndeterminate: indeterminate];
        if indeterminate {
            let _: () = msg_send![progress, startAnimation: std::ptr::null_mut::<AnyObject>()];
        }
    }
    if status_label != 0 {
        set_string_value(status_label as *mut AnyObject, text);
    }
    if !indeterminate && cancel_button != 0 {
        let _: () = msg_send![cancel_button as *mut AnyObject, setEnabled: false];
    }
}

unsafe fn clear_download_cancellation() {
    let (cancel_button, cancellation) = {
        let mut ui = UPDATE_UI_STATE.lock().unwrap();
        let cancellation = ui.cancellation;
        ui.cancellation = 0;
        (ui.cancel_button, cancellation)
    };
    if cancel_button != 0 {
        let _: () = msg_send![cancel_button as *mut AnyObject, setEnabled: false];
    }
    release_block(cancellation);
}

unsafe fn set_custom_window_title(text: &str) {
    let window = UPDATE_UI_STATE.lock().unwrap().window;
    if window != 0 {
        let title_ns = make_nsstring(text);
        let _: () = msg_send![window as *mut AnyObject, setTitle: title_ns];
        crate::ffi::CFRelease(title_ns as *const c_void);
    }
}

/// 创建准备安装的选择窗口，保留 Sparkle 的三个选择语义但不使用标准 UI。
/// Build the ready-to-install choice window while preserving Sparkle's three choices.
unsafe fn make_custom_choice_window(
    driver: *mut c_void,
    reply: *mut c_void,
    title_text: &str,
    message_text: &str,
) {
    close_custom_update_window();
    let window_frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(580.0, 250.0));
    let window: *mut AnyObject = msg_send![class!(NSWindow), alloc];
    let window: *mut AnyObject = msg_send![
        window,
        initWithContentRect: window_frame,
        styleMask: 1u64,
        backing: 2u64,
        defer: false
    ];
    if window.is_null() {
        return;
    }
    let window_title = make_nsstring(&t("settings.update_window_title"));
    let _: () = msg_send![window, setTitle: window_title];
    crate::ffi::CFRelease(window_title as *const c_void);
    let _: () = msg_send![window, setReleasedWhenClosed: false];
    let content: *mut AnyObject = msg_send![window, contentView];

    let title: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let title: *mut AnyObject = msg_send![
        title,
        initWithFrame: NSRect::new(NSPoint::new(32.0, 178.0), NSSize::new(516.0, 32.0))
    ];
    set_string_value(title, title_text);
    let _: () = msg_send![title, setBezeled: false];
    let _: () = msg_send![title, setDrawsBackground: false];
    let _: () = msg_send![title, setEditable: false];
    let _: () = msg_send![title, setSelectable: false];
    let title_font: *mut AnyObject = msg_send![class!(NSFont), boldSystemFontOfSize: 20.0f64];
    let _: () = msg_send![title, setFont: title_font];
    let _: () = msg_send![content, addSubview: title];

    let message: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let message: *mut AnyObject = msg_send![
        message,
        initWithFrame: NSRect::new(NSPoint::new(32.0, 118.0), NSSize::new(516.0, 44.0))
    ];
    set_string_value(message, message_text);
    let _: () = msg_send![message, setBezeled: false];
    let _: () = msg_send![message, setDrawsBackground: false];
    let _: () = msg_send![message, setEditable: false];
    let _: () = msg_send![message, setSelectable: false];
    let message_font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 15.0f64];
    let _: () = msg_send![message, setFont: message_font];
    let _: () = msg_send![message, setLineBreakMode: 0u64];
    let _: () = msg_send![message, setMaximumNumberOfLines: 0isize];
    let _: () = msg_send![content, addSubview: message];

    let skip: *mut AnyObject = msg_send![class!(NSButton), alloc];
    let skip: *mut AnyObject = msg_send![
        skip,
        initWithFrame: NSRect::new(NSPoint::new(32.0, 28.0), NSSize::new(150.0, 36.0))
    ];
    let skip_title = make_nsstring(&t("settings.btn_skip_version"));
    let _: () = msg_send![skip, setTitle: skip_title];
    crate::ffi::CFRelease(skip_title as *const c_void);
    let _: () = msg_send![skip, setBezelStyle: 1u64];
    let _: () = msg_send![skip, setTarget: driver as *mut AnyObject];
    let _: () = msg_send![skip, setAction: sel!(skipCustomUpdate:)];
    let _: () = msg_send![content, addSubview: skip];

    let later: *mut AnyObject = msg_send![class!(NSButton), alloc];
    let later: *mut AnyObject = msg_send![
        later,
        initWithFrame: NSRect::new(NSPoint::new(194.0, 28.0), NSSize::new(150.0, 36.0))
    ];
    let later_title = make_nsstring(&t("settings.btn_remind_later"));
    let _: () = msg_send![later, setTitle: later_title];
    crate::ffi::CFRelease(later_title as *const c_void);
    let _: () = msg_send![later, setBezelStyle: 1u64];
    let _: () = msg_send![later, setTarget: driver as *mut AnyObject];
    let _: () = msg_send![later, setAction: sel!(dismissCustomUpdate:)];
    let _: () = msg_send![content, addSubview: later];

    let install: *mut AnyObject = msg_send![class!(NSButton), alloc];
    let install: *mut AnyObject = msg_send![
        install,
        initWithFrame: NSRect::new(NSPoint::new(356.0, 28.0), NSSize::new(192.0, 36.0))
    ];
    let install_title = make_nsstring(&t("settings.btn_install_update"));
    let _: () = msg_send![install, setTitle: install_title];
    crate::ffi::CFRelease(install_title as *const c_void);
    let _: () = msg_send![install, setBezelStyle: 1u64];
    let _: () = msg_send![install, setTarget: driver as *mut AnyObject];
    let _: () = msg_send![install, setAction: sel!(installCustomUpdate:)];
    let _: () = msg_send![content, addSubview: install];

    let copied_reply = copy_block(reply) as usize;
    let mut ui = UPDATE_UI_STATE.lock().unwrap();
    ui.window = window as usize;
    ui.update_reply = copied_reply;

    let _: () = msg_send![window, center];
    let _: () = msg_send![window, makeKeyAndOrderFront: std::ptr::null_mut::<AnyObject>()];
}

unsafe fn choose_custom_update(choice: isize) {
    let reply = {
        let mut ui = UPDATE_UI_STATE.lock().unwrap();
        let reply = ui.update_reply;
        ui.update_reply = 0;
        reply
    };
    close_custom_update_window();
    invoke_choice_reply(reply, choice);
    release_block(reply);
}

extern "C" fn install_custom_update(_this: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    unsafe { choose_custom_update(1) };
}

extern "C" fn skip_custom_update(_this: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    unsafe { choose_custom_update(0) };
}

extern "C" fn dismiss_custom_update(_this: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    unsafe { choose_custom_update(2) };
}

extern "C" fn cancel_custom_download(_this: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    unsafe {
        let cancellation = {
            let mut ui = UPDATE_UI_STATE.lock().unwrap();
            let cancellation = ui.cancellation;
            ui.cancellation = 0;
            cancellation
        };
        close_custom_update_window();
        invoke_block(cancellation);
        release_block(cancellation);
    }
}

extern "C" fn show_download_initiated(this: *mut c_void, _cmd: Sel, cancellation: *mut c_void) {
    unsafe { make_custom_download_window(this, cancellation) };
}

extern "C" fn show_download_expected_length(_this: *mut c_void, _cmd: Sel, length: u64) {
    unsafe {
        let mut ui = UPDATE_UI_STATE.lock().unwrap();
        ui.expected_length = length;
        drop(ui);
        update_download_progress_ui();
    }
}

extern "C" fn show_download_received_data(_this: *mut c_void, _cmd: Sel, length: u64) {
    unsafe {
        let mut ui = UPDATE_UI_STATE.lock().unwrap();
        ui.received_length = ui.received_length.saturating_add(length);
        drop(ui);
        update_download_progress_ui();
    }
}

extern "C" fn show_download_started_extracting(_this: *mut c_void, _cmd: Sel) {
    unsafe {
        clear_download_cancellation();
        set_download_status(&t("settings.update_preparing"), true);
    }
}

extern "C" fn show_extraction_progress(_this: *mut c_void, _cmd: Sel, progress: f64) {
    unsafe {
        let progress_view = UPDATE_UI_STATE.lock().unwrap().progress;
        if progress_view != 0 {
            let progress_view = progress_view as *mut AnyObject;
            let _: () = msg_send![progress_view, setIndeterminate: false];
            let _: () = msg_send![progress_view, setDoubleValue: progress.clamp(0.0, 1.0)];
        }
        set_download_status(&t("settings.update_extracting"), false);
    }
}

extern "C" fn show_ready_to_install(this: *mut c_void, _cmd: Sel, reply: *mut c_void) {
    unsafe {
        let app = app_display_name();
        let message = tf("settings.update_ready_message", &[("app", &app)]);
        make_custom_choice_window(this, reply, &t("settings.update_ready_title"), &message);
    }
}

extern "C" fn retry_custom_installation(_this: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    unsafe {
        let retry = {
            let mut ui = UPDATE_UI_STATE.lock().unwrap();
            let retry = ui.retry_termination;
            ui.retry_termination = 0;
            retry
        };
        invoke_block(retry);
        release_block(retry);
    }
}

extern "C" fn show_installing_update(
    this: *mut c_void,
    _cmd: Sel,
    application_terminated: i8,
    retry_terminating_application: *mut c_void,
) {
    unsafe {
        let app = app_display_name();
        make_custom_download_window(this, std::ptr::null_mut());
        let window_title = tf("settings.update_installing_window_title", &[("app", &app)]);
        set_custom_window_title(&window_title);
        let status_key = if application_terminated != 0 {
            "settings.update_installing"
        } else {
            "settings.update_waiting_for_quit"
        };
        set_download_status(&t(status_key), true);

        let (cancel_button, retry) = {
            let mut ui = UPDATE_UI_STATE.lock().unwrap();
            ui.retry_termination = copy_block(retry_terminating_application) as usize;
            (ui.cancel_button, ui.retry_termination)
        };
        if cancel_button != 0 {
            let button = cancel_button as *mut AnyObject;
            if application_terminated != 0 {
                let _: () = msg_send![button, setHidden: true];
            } else {
                let title = make_nsstring(&t("settings.btn_try_again"));
                let _: () = msg_send![button, setTitle: title];
                crate::ffi::CFRelease(title as *const c_void);
                let _: () = msg_send![button, setEnabled: retry != 0];
                let _: () = msg_send![button, setAction: sel!(retryCustomInstallation:)];
            }
        }
    }
}

extern "C" fn show_update_installed(
    this: *mut c_void,
    _cmd: Sel,
    _relaunched: i8,
    acknowledgement: *mut c_void,
) {
    unsafe {
        make_custom_result_window(
            this,
            acknowledgement,
            &t("settings.update_installed_title"),
            &t("settings.update_installed_message"),
        );
    }
}

extern "C" fn show_update_release_notes(
    _this: *mut c_void,
    _cmd: Sel,
    _download_data: *mut c_void,
) {
}

extern "C" fn show_update_release_notes_failed(_this: *mut c_void, _cmd: Sel, _error: *mut c_void) {
}

extern "C" fn show_update_in_focus(_this: *mut c_void, _cmd: Sel) {
    unsafe {
        let window = UPDATE_UI_STATE.lock().unwrap().window;
        if window != 0 {
            let window = window as *mut AnyObject;
            let _: () = msg_send![window, makeKeyAndOrderFront: std::ptr::null_mut::<AnyObject>()];
        }
    }
}

extern "C" fn toggle_automatic_update(_this: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    unsafe {
        let sender = sender as *mut AnyObject;
        let checked: isize = msg_send![sender, state];
        let updater = state()
            .lock()
            .unwrap()
            .as_ref()
            .map_or(std::ptr::null_mut(), |current| current.updater);
        if !updater.is_null() {
            send_void_bool(
                updater,
                sel!(setAutomaticallyDownloadsUpdates:),
                checked != 0,
            );
        }
    }
}

extern "C" fn show_user_initiated_update_check(
    this: *mut c_void,
    _cmd: Sel,
    cancellation: *mut c_void,
) {
    unsafe { make_custom_update_window(this, cancellation) };
}

extern "C" fn cancel_custom_update_check(_this: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    unsafe {
        let cancellation = UPDATE_UI_STATE.lock().unwrap().cancellation;
        invoke_block(cancellation);
        close_custom_update_window();
    }
}

extern "C" fn acknowledge_custom_update_result(
    _this: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    unsafe {
        let acknowledgement = UPDATE_UI_STATE.lock().unwrap().acknowledgement;
        invoke_block(acknowledgement);
        close_custom_update_window();
    }
}

extern "C" fn show_update_found(
    this: *mut c_void,
    _cmd: Sel,
    item: *mut c_void,
    _state: *mut c_void,
    reply: *mut c_void,
) {
    unsafe {
        make_custom_update_found_window(this, item, reply);
    }
}

extern "C" fn show_update_not_found(
    this: *mut c_void,
    _cmd: Sel,
    _error: *mut c_void,
    acknowledgement: *mut c_void,
) {
    unsafe {
        make_custom_result_window(
            this,
            acknowledgement,
            &t("settings.update_up_to_date_title"),
            &t("settings.update_up_to_date_message"),
        );
    }
}

extern "C" fn show_updater_error(
    this: *mut c_void,
    _cmd: Sel,
    _error: *mut c_void,
    acknowledgement: *mut c_void,
) {
    unsafe {
        make_custom_result_window(
            this,
            acknowledgement,
            &t("settings.update_check_error_title"),
            &t("settings.update_check_error_message"),
        );
    }
}

extern "C" fn dismiss_update_installation(this: *mut c_void, _cmd: Sel) {
    unsafe {
        close_custom_update_window();
        call_super_no_arguments(this, sel!(dismissUpdateInstallation));
    }
}

unsafe fn custom_driver_class() -> *mut AnyObject {
    CUSTOM_DRIVER_CLASS
        .get_or_init(|| {
            let superclass = AnyClass::get(c"SPUStandardUserDriver")
                .expect("Sparkle standard user driver class is loaded");
            let superclass_ptr = superclass as *const AnyClass as *mut AnyObject;
            CUSTOM_DRIVER_SUPERCLASS
                .set(superclass_ptr as usize)
                .expect("Sparkle user driver superclass initialized once");

            let class_name = CString::new("OhMyTabSparkleUserDriver")
                .expect("custom Sparkle user driver class name is valid");
            let cls = objc_allocateClassPair(superclass_ptr, class_name.as_ptr(), 0);
            assert!(
                !cls.is_null(),
                "failed to allocate Sparkle user driver subclass"
            );

            let types_one_object = CString::new("v@:@").unwrap();
            let types_two_objects = CString::new("v@:@@").unwrap();
            let types_three_objects = CString::new("v@:@@@").unwrap();
            let types_no_arguments = CString::new("v@:").unwrap();
            let types_uint64 = CString::new("v@:Q").unwrap();
            let types_double = CString::new("v@:d").unwrap();
            let types_bool_object = CString::new("v@:c@").unwrap();
            class_addMethod(
                cls,
                sel!(showUpdatePermissionRequest:reply:),
                show_update_permission as *mut c_void,
                types_two_objects.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(showUserInitiatedUpdateCheckWithCancellation:),
                show_user_initiated_update_check as *mut c_void,
                types_one_object.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(cancelCustomUpdateCheck:),
                cancel_custom_update_check as *mut c_void,
                types_one_object.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(acknowledgeCustomUpdateResult:),
                acknowledge_custom_update_result as *mut c_void,
                types_one_object.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(showUpdateFoundWithAppcastItem:state:reply:),
                show_update_found as *mut c_void,
                types_three_objects.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(showDownloadInitiatedWithCancellation:),
                show_download_initiated as *mut c_void,
                types_one_object.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(cancelCustomDownload:),
                cancel_custom_download as *mut c_void,
                types_one_object.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(showDownloadDidReceiveExpectedContentLength:),
                show_download_expected_length as *mut c_void,
                types_uint64.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(showDownloadDidReceiveDataOfLength:),
                show_download_received_data as *mut c_void,
                types_uint64.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(showDownloadDidStartExtractingUpdate),
                show_download_started_extracting as *mut c_void,
                types_no_arguments.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(showExtractionReceivedProgress:),
                show_extraction_progress as *mut c_void,
                types_double.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(showReadyToInstallAndRelaunch:),
                show_ready_to_install as *mut c_void,
                types_one_object.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(showInstallingUpdateWithApplicationTerminated:retryTerminatingApplication:),
                show_installing_update as *mut c_void,
                types_bool_object.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(showUpdateInstalledAndRelaunched:acknowledgement:),
                show_update_installed as *mut c_void,
                types_bool_object.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(showUpdateReleaseNotesWithDownloadData:),
                show_update_release_notes as *mut c_void,
                types_one_object.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(showUpdateReleaseNotesFailedToDownloadWithError:),
                show_update_release_notes_failed as *mut c_void,
                types_one_object.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(showUpdateInFocus),
                show_update_in_focus as *mut c_void,
                types_no_arguments.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(installCustomUpdate:),
                install_custom_update as *mut c_void,
                types_one_object.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(skipCustomUpdate:),
                skip_custom_update as *mut c_void,
                types_one_object.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(dismissCustomUpdate:),
                dismiss_custom_update as *mut c_void,
                types_one_object.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(toggleAutomaticUpdate:),
                toggle_automatic_update as *mut c_void,
                types_one_object.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(retryCustomInstallation:),
                retry_custom_installation as *mut c_void,
                types_one_object.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(allowAutomaticUpdate:),
                allow_automatic_update as *mut c_void,
                types_one_object.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(deferAutomaticUpdate:),
                defer_automatic_update as *mut c_void,
                types_one_object.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(showUpdateNotFoundWithError:acknowledgement:),
                show_update_not_found as *mut c_void,
                types_two_objects.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(showUpdaterError:acknowledgement:),
                show_updater_error as *mut c_void,
                types_two_objects.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(dismissUpdateInstallation),
                dismiss_update_installation as *mut c_void,
                types_no_arguments.as_ptr(),
            );
            objc_registerClassPair(cls);
            CustomDriverClass(cls)
        })
        .0
}

/// Start Sparkle if its framework is available in the current app bundle.
///
/// This is idempotent. Returning `false` means the app is running without Sparkle (for example,
/// a raw `cargo run` or a dev bundle built before the framework was copied), not that the app
/// itself failed to start.
pub(crate) fn initialize(automatically_check: bool) -> bool {
    let mut guard = state().lock().unwrap();
    if let Some(current) = guard.as_ref() {
        unsafe {
            send_void_bool(
                current.updater,
                sel!(setAutomaticallyChecksForUpdates:),
                automatically_check,
            );
        }
        return true;
    }

    let framework_handle = unsafe { load_framework() };
    if AnyClass::get(c"SPUStandardUserDriver").is_none() {
        log_info!(
            "Sparkle updater unavailable: Sparkle.framework not found; expected {}",
            framework_candidates().first().map_or_else(
                || "Contents/Frameworks/Sparkle.framework".to_string(),
                |p| p.display().to_string()
            )
        );
        return false;
    }
    let Some(updater_class) = AnyClass::get(c"SPUUpdater") else {
        log_info!("Sparkle updater unavailable: SPUUpdater class not found");
        return false;
    };

    let updater = unsafe {
        let main_bundle = send_id(
            class!(NSBundle) as *const _ as *mut AnyObject,
            sel!(mainBundle),
        );
        let driver_class = custom_driver_class();
        let driver_allocated = send_id(driver_class, sel!(alloc));
        let driver = {
            type Fn = unsafe extern "C" fn(
                *mut AnyObject,
                Sel,
                *mut AnyObject,
                *mut AnyObject,
            ) -> *mut AnyObject;
            let f: Fn = std::mem::transmute(objc_msgSend as *const ());
            f(
                driver_allocated,
                sel!(initWithHostBundle:delegate:),
                main_bundle,
                std::ptr::null_mut(),
            )
        };
        if driver.is_null() {
            log_info!("Sparkle updater failed to initialize its custom user driver");
            return false;
        }

        let updater_class_object = updater_class as *const AnyClass as *mut AnyObject;
        let updater_allocated = send_id(updater_class_object, sel!(alloc));
        let updater = send_id4(
            updater_allocated,
            sel!(initWithHostBundle:applicationBundle:userDriver:delegate:),
            main_bundle,
            main_bundle,
            driver,
            std::ptr::null_mut(),
        );
        release_obj(driver);
        updater
    };

    if updater.is_null() {
        log_info!("Sparkle updater failed to initialize SPUUpdater");
        return false;
    }

    unsafe {
        send_void_bool(
            updater,
            sel!(setAutomaticallyChecksForUpdates:),
            automatically_check,
        );
        if !send_bool_ptr(updater, sel!(startUpdater:), std::ptr::null_mut()) {
            log_info!("Sparkle updater failed to start");
            release_obj(updater);
            return false;
        }
    }

    *guard = Some(UpdaterState {
        _framework_handle: framework_handle,
        updater,
    });
    log_info!(
        "Sparkle updater started with custom progress UI (automatic checks: {}, feed: {})",
        automatically_check,
        FEED_URL
    );
    true
}

/// Apply the About-page automatic-check setting to a running Sparkle updater.
pub(crate) fn set_automatic_checks(enabled: bool) {
    let guard = state().lock().unwrap();
    let Some(current) = guard.as_ref() else {
        return;
    };
    unsafe {
        send_void_bool(
            current.updater,
            sel!(setAutomaticallyChecksForUpdates:),
            enabled,
        );
    }
    log_info!("Sparkle automatic update checks set to {}", enabled);
}

/// Ask Sparkle to check for updates; the custom user driver presents the update UI.
/// 请求 Sparkle 检查更新，由自定义 user driver 负责显示更新界面。
pub(crate) fn check_for_updates() -> bool {
    // Be defensive for smoke tests or an unusual launch path that invokes the About action
    // before the normal startup sequence has reached updater initialization.
    if state().lock().unwrap().is_none() {
        let automatically_check = crate::config::CONFIG
            .read()
            .map(|cfg| cfg.updates.automatically_check)
            .unwrap_or(true);
        if !initialize(automatically_check) {
            return false;
        }
    }
    let guard = state().lock().unwrap();
    let Some(current) = guard.as_ref() else {
        return false;
    };
    unsafe { send_void(current.updater, sel!(checkForUpdates)) };
    true
}
