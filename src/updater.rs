//! Sparkle 2 updater integration.
//!
//! Sparkle is loaded dynamically instead of being linked at Rust build time. This keeps
//! `cargo run`/unit tests usable on a checkout that does not contain the native framework yet,
//! while a bundled `.app` automatically gets the real updater when `Sparkle.framework` is copied
//! into `Contents/Frameworks`.

use crate::ffi::{class_addMethod, make_nsstring, objc_allocateClassPair, objc_registerClassPair};
use crate::i18n::t;
use crate::{ffi::release_obj, log_info};
use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::ffi::{c_char, c_void, CString};
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
}

static UPDATE_UI_STATE: LazyLock<Mutex<UpdateUiState>> = LazyLock::new(|| {
    Mutex::new(UpdateUiState {
        window: 0,
        cancellation: 0,
        acknowledgement: 0,
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

/// Call the inherited Sparkle implementation for three object arguments.
/// 调用 Sparkle 三个对象参数回调的父类实现。
unsafe fn call_super_three_objects(
    receiver: *mut c_void,
    selector: Sel,
    first: *mut c_void,
    second: *mut c_void,
    third: *mut c_void,
) {
    type Fn = unsafe extern "C" fn(*mut ObjcSuper, Sel, *mut c_void, *mut c_void, *mut c_void);
    let superclass = *CUSTOM_DRIVER_SUPERCLASS
        .get()
        .expect("Sparkle custom user-driver superclass is initialized");
    let mut objc_super = ObjcSuper {
        receiver,
        superclass: superclass as *mut c_void,
    };
    let f: Fn = std::mem::transmute(objc_msgSendSuper as *const ());
    f(&mut objc_super, selector, first, second, third);
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

unsafe fn close_custom_update_window() {
    let mut ui = UPDATE_UI_STATE.lock().unwrap();
    if ui.window != 0 {
        let window = ui.window as *mut AnyObject;
        let _: () = msg_send![window, orderOut: std::ptr::null_mut::<AnyObject>()];
        let _: () = msg_send![window, close];
        release_obj(window);
        ui.window = 0;
    }
    release_block(ui.cancellation);
    ui.cancellation = 0;
    release_block(ui.acknowledgement);
    ui.acknowledgement = 0;
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
    release_obj(label);

    let progress: *mut AnyObject = msg_send![class!(NSProgressIndicator), alloc];
    let progress: *mut AnyObject = msg_send![
        progress,
        initWithFrame: NSRect::new(NSPoint::new(32.0, 78.0), NSSize::new(456.0, 16.0))
    ];
    let _: () = msg_send![progress, setIndeterminate: true];
    let _: () = msg_send![progress, startAnimation: std::ptr::null_mut::<AnyObject>()];
    let _: () = msg_send![content, addSubview: progress];
    release_obj(progress);

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
    release_obj(cancel);

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
    release_obj(title);

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
    release_obj(message);

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
    release_obj(ok);

    let _: () = msg_send![window, center];
    let _: () = msg_send![window, makeKeyAndOrderFront: std::ptr::null_mut::<AnyObject>()];

    let copied_acknowledgement = copy_block(acknowledgement) as usize;
    let mut ui = UPDATE_UI_STATE.lock().unwrap();
    ui.window = window as usize;
    ui.acknowledgement = copied_acknowledgement;
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
    state: *mut c_void,
    reply: *mut c_void,
) {
    unsafe {
        close_custom_update_window();
        call_super_three_objects(
            this,
            sel!(showUpdateFoundWithAppcastItem:state:reply:),
            item,
            state,
            reply,
        );
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

/// Ask Sparkle to present the standard update UI. Returns `false` when Sparkle is not loaded.
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
