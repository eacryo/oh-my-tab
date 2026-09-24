//! Sparkle 2 updater integration.
//!
//! Sparkle is loaded dynamically instead of being linked at Rust build time. This keeps
//! `cargo run`/unit tests usable on a checkout that does not contain the native framework yet,
//! while a bundled `.app` automatically gets the real updater when `Sparkle.framework` is copied
//! into `Contents/Frameworks`.

use crate::ffi::{
    bundle_info_string, class_addMethod, make_nsstring, objc_allocateClassPair, objc_msgSend,
    objc_msgSendSuper, objc_registerClassPair, CallbackTarget, ObjcSuper,
};
use crate::i18n::{t, tf};
use crate::skylight;
use crate::{ffi::release_obj, log_debug, log_info};
use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSPoint, NSRange, NSRect, NSSize};
use std::ffi::{c_char, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

// objc_msgSend / objc_msgSendSuper / dlopen now live in ffi.rs and skylight.rs.

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
    host_view: usize,
    host_window: usize,
    check_button: usize,
    check_loading_timer: usize,
    check_loading_frame: usize,
    cancellation: usize,
    acknowledgement: usize,
    update_reply: usize,
    information_url: String,
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
        host_view: 0,
        host_window: 0,
        check_button: 0,
        check_loading_timer: 0,
        check_loading_frame: 0,
        cancellation: 0,
        acknowledgement: 0,
        update_reply: 0,
        information_url: String::new(),
        permission_reply: 0,
        retry_termination: 0,
        progress: 0,
        status_label: 0,
        cancel_button: 0,
        expected_length: 0,
        received_length: 0,
    })
});

/// When the last inline "checking" phase began, for a timeout fallback so the button never gets
/// stuck if Sparkle never calls back.
static CHECK_TIMER: LazyLock<Mutex<Option<Instant>>> = LazyLock::new(|| Mutex::new(None));

const CHECK_TIMEOUT: Duration = Duration::from_secs(20);
const INLINE_UPDATE_HEIGHT: f64 = 140.0;
const UPDATE_RELEASE_NOTES_MAX_HEIGHT: f64 = 180.0;
const CHECK_LOADING_FRAMES: [&str; 8] = ["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"];
const CHECK_LOADING_CYCLE_SECONDS: f64 = 1.0;

const RELEASE_NOTES_LOCALE_END: &str = "<!-- /locale -->";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UpdatePromptKind {
    Available,
    Downloaded,
    Installing,
    InformationOnly,
}

fn update_prompt_kind(stage: isize, information_only: bool) -> UpdatePromptKind {
    if information_only {
        return UpdatePromptKind::InformationOnly;
    }
    match stage {
        1 => UpdatePromptKind::Downloaded,
        2 => UpdatePromptKind::Installing,
        _ => UpdatePromptKind::Available,
    }
}

fn is_safe_update_info_url(url: &str) -> bool {
    url.get(..8)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
}

/// The dynamically registered subclass is retained by the Objective-C runtime forever.
struct CustomDriverClass(*mut AnyObject);

unsafe impl Send for CustomDriverClass {}
unsafe impl Sync for CustomDriverClass {}

static CUSTOM_DRIVER_CLASS: OnceLock<CustomDriverClass> = OnceLock::new();
static CUSTOM_DRIVER_SUPERCLASS: OnceLock<usize> = OnceLock::new();
static CHECK_LOADING_TIMER_TARGET: OnceLock<CallbackTarget> = OnceLock::new();

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
        let handle = skylight::dlopen_path(&path_string);
        if !handle.is_null() {
            return handle;
        }
    }
    std::ptr::null_mut()
}

unsafe fn call_super_no_arguments(receiver: *mut c_void, selector: Sel) {
    type Fn = unsafe extern "C" fn(*mut ObjcSuper, Sel);
    let superclass = *CUSTOM_DRIVER_SUPERCLASS
        .get()
        .expect("Sparkle custom user-driver superclass is initialized");
    let mut objc_super = ObjcSuper {
        receiver,
        super_class: superclass as *mut c_void,
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

/// Read the current bundle's SUFeedURL from Info.plist; empty when absent. The log uses this so it
/// reflects the feed Sparkle actually reads from the host bundle instead of a misleading constant.
unsafe fn bundle_feed_url() -> String {
    let bundle = send_id(
        class!(NSBundle) as *const _ as *mut AnyObject,
        sel!(mainBundle),
    );
    let key_ns = make_nsstring("SUFeedURL");
    let value: *mut AnyObject = msg_send![bundle, objectForInfoDictionaryKey: key_ns];
    crate::ffi::CFRelease(key_ns as *const c_void);
    nsstring_to_string(value)
}

// bundle_info_string now lives in ffi.rs

/// Record the useful NSError fields so network failures are not reduced to a generic label.
unsafe fn log_sparkle_error(context: &str, error: *mut c_void) {
    if error.is_null() {
        return;
    }

    let error = error as *mut AnyObject;
    let domain: *mut AnyObject = msg_send![error, domain];
    let code: isize = msg_send![error, code];
    let localized_description: *mut AnyObject = msg_send![error, localizedDescription];
    let description: *mut AnyObject = msg_send![error, description];
    let user_info: *mut AnyObject = msg_send![error, userInfo];
    let failing_url = if user_info.is_null() {
        String::new()
    } else {
        let key = make_nsstring("NSErrorFailingURLStringKey");
        let value: *mut AnyObject = msg_send![user_info, objectForKey: key];
        crate::ffi::CFRelease(key as *const c_void);
        nsstring_to_string(value)
    };

    log_info!(
        "Sparkle {}: NSError domain='{}' code={} localizedDescription='{}' description='{}' failingURL='{}'",
        context,
        nsstring_to_string(domain),
        code,
        nsstring_to_string(localized_description),
        nsstring_to_string(description),
        failing_url
    );
}

/// Record the bundle and proxy environment used by Sparkle without logging proxy URLs or secrets.
fn log_update_network_context() {
    let feed_url = unsafe { bundle_feed_url() };
    let bundle_version = unsafe { bundle_info_string("CFBundleVersion") };
    log_debug!(
        "Sparkle update request context: feed={}, bundle-version={}",
        feed_url,
        bundle_version
    );
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

/// Render target: the About page host view when inline, else a standalone window.
#[derive(Clone, Copy)]
struct RenderTarget {
    /// Points at the host view when inline, null otherwise.
    host: *mut AnyObject,
    /// The parent view receiving subviews (host or window contentView).
    parent: *mut AnyObject,
    /// Inline host width; 0 for standalone windows (use the window's native coordinates).
    width: f64,
}

/// Decide the render target: inline when a host is registered, else fall back to a window. Inline
/// sizes the host to the screen's required height, keeping its top fixed below the check button
/// row so the top-down flip is compact without extra blank.
unsafe fn render_target(window_h: f64) -> RenderTarget {
    let ui = UPDATE_UI_STATE.lock().unwrap();
    if ui.host_view != 0 {
        // Host coordinates start at (0,0); width is the host frame width, so inline layout fits.
        // When hosted inline, expand the card to this screen's height and size the host to match,
        // keeping the host top fixed so the top-down flip is exact.
        crate::settings::expand_update_section(window_h);
        let frame: NSRect = msg_send![ui.host_view as *mut AnyObject, frame];
        RenderTarget {
            host: ui.host_view as *mut AnyObject,
            parent: ui.host_view as *mut AnyObject,
            width: frame.size.width,
        }
    } else {
        RenderTarget {
            host: std::ptr::null_mut(),
            parent: std::ptr::null_mut(),
            width: 0.0,
        }
    }
}

/// Map a standalone-window frame onto the host width, scaling x and width proportionally.
fn scale_frame(target: RenderTarget, window_w: f64, frame: NSRect) -> NSRect {
    if target.host.is_null() || window_w <= 0.0 {
        return frame;
    }
    let scale = target.width / window_w;
    NSRect::new(
        NSPoint::new(frame.origin.x * scale, frame.origin.y),
        NSSize::new(frame.size.width * scale, frame.size.height),
    )
}

/// Add a control to the render target; inline scales its frame to the host width, sizes the host to
/// this screen's height, and flips the window's bottom-up y to the host's top-down so titles sit
/// near the host top and buttons near the bottom, compactly starting below the check button row.
unsafe fn add_control(
    target: RenderTarget,
    window_w: f64,
    control: *mut AnyObject,
    frame: NSRect,
    parent: *mut AnyObject,
) {
    if !target.host.is_null() {
        let _: () = msg_send![target.host, setHidden: false];
        let scaled = scale_frame(target, window_w, frame);
        let host_frame: NSRect = msg_send![target.host, frame];
        let host_h = host_frame.size.height;
        // Flip the window's bottom-up y to the host's top-down: titles near the top, buttons near
        // the bottom, content flowing below the check-button row.
        let flipped = NSRect::new(
            NSPoint::new(
                scaled.origin.x,
                host_h - scaled.origin.y - scaled.size.height,
            ),
            scaled.size,
        );
        let _: () = msg_send![control, setFrame: flipped];
    }
    let _: () = msg_send![parent, addSubview: control];
}

/// Clear the update controls inside the host view (the host itself stays owned by the About page).
unsafe fn clear_host_subviews(host: *mut AnyObject) {
    if host.is_null() {
        return;
    }
    loop {
        let subviews: *mut AnyObject = msg_send![host, subviews];
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

unsafe fn close_custom_update_window() {
    let check_button = UPDATE_UI_STATE.lock().unwrap().check_button as *mut AnyObject;
    stop_check_loading_indicator(check_button);
    let mut ui = UPDATE_UI_STATE.lock().unwrap();
    if ui.host_view != 0 {
        // Inline mode: clear the host's controls, leave the settings window untouched.
        clear_host_subviews(ui.host_view as *mut AnyObject);
        ui.window = 0;
    } else if ui.window != 0 {
        let window = ui.window as *mut AnyObject;
        // Remove subviews before releasing their alloc ownership to avoid AppKit over-release.
        let content: *mut AnyObject = msg_send![window, contentView];
        if !content.is_null() {
            clear_host_subviews(content);
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
    ui.information_url.clear();
    release_block(ui.permission_reply);
    ui.permission_reply = 0;
    release_block(ui.retry_termination);
    ui.retry_termination = 0;
    ui.progress = 0;
    ui.status_label = 0;
    ui.cancel_button = 0;
    ui.check_loading_timer = 0;
    ui.check_loading_frame = 0;
    ui.expected_length = 0;
    ui.received_length = 0;
}

/// Register the About page's host view and check-updates button so update steps render inline and
/// the button can report its state (checking / up to date).
pub(crate) fn set_update_host(
    host: *mut AnyObject,
    window: *mut AnyObject,
    check_button: *mut AnyObject,
) {
    let mut ui = UPDATE_UI_STATE.lock().unwrap();
    ui.host_view = host as usize;
    ui.host_window = window as usize;
    ui.check_button = check_button as usize;
}

/// Clear the update host and check-button references; called before the settings window is
/// destroyed so the updater never writes to a deallocated view.
pub(crate) fn clear_update_host() {
    unsafe { close_custom_update_window() };
    let mut ui = UPDATE_UI_STATE.lock().unwrap();
    ui.host_view = 0;
    ui.host_window = 0;
    ui.check_button = 0;
    ui.check_loading_timer = 0;
    ui.check_loading_frame = 0;
}

/// Stop the Braille glyph animation on the check-updates button.
unsafe fn stop_check_loading_indicator(button: *mut AnyObject) {
    if button.is_null() {
        return;
    }
    let timer = {
        let mut ui = UPDATE_UI_STATE.lock().unwrap();
        ui.check_loading_frame = 0;
        std::mem::take(&mut ui.check_loading_timer)
    };
    if timer != 0 {
        let _: () = msg_send![timer as *mut AnyObject, invalidate];
    }
}

fn check_loading_title(frame: usize, label: &str) -> String {
    format!(
        "{} {label}",
        CHECK_LOADING_FRAMES[frame % CHECK_LOADING_FRAMES.len()]
    )
}

unsafe fn set_check_loading_frame(button: *mut AnyObject, frame: usize) {
    if button.is_null() {
        return;
    }
    let title = make_nsstring(&check_loading_title(frame, &t("settings.update_checking")));
    let _: () = msg_send![button, setTitle: title];
    crate::ffi::CFRelease(title as *const c_void);

    // Braille and CJK resolve to different fonts. Preserve the button-generated attributes, then
    // give only the first glyph a slightly larger monospaced font and a 1pt upward optical shift.
    let attributed: *mut AnyObject = msg_send![button, attributedTitle];
    if attributed.is_null() {
        return;
    }
    let adjusted: *mut AnyObject = msg_send![attributed, mutableCopy];
    if adjusted.is_null() {
        return;
    }
    let button_font: *mut AnyObject = msg_send![button, font];
    let button_font_size: f64 = if button_font.is_null() {
        13.0
    } else {
        msg_send![button_font, pointSize]
    };
    let loader_font: *mut AnyObject = msg_send![
        class!(NSFont),
        monospacedSystemFontOfSize: button_font_size + 1.0,
        weight: 0.0f64
    ];
    let baseline_offset: *mut AnyObject = msg_send![class!(NSNumber), numberWithDouble: 1.0f64];
    let font_key = make_nsstring("NSFont");
    let baseline_key = make_nsstring("NSBaselineOffset");
    let loader_range = NSRange::new(0, 1);
    let _: () =
        msg_send![adjusted, addAttribute: font_key, value: loader_font, range: loader_range];
    let _: () = msg_send![adjusted, addAttribute: baseline_key, value: baseline_offset, range: loader_range];
    crate::ffi::CFRelease(font_key as *const c_void);
    crate::ffi::CFRelease(baseline_key as *const c_void);
    let _: () = msg_send![button, setAttributedTitle: adjusted];
    release_obj(adjusted);
}

extern "C" fn advance_check_loading_frame(_this: *mut c_void, _cmd: Sel, _timer: *mut c_void) {
    let (button, frame) = {
        let mut ui = UPDATE_UI_STATE.lock().unwrap();
        if ui.check_loading_timer == 0 || ui.check_button == 0 {
            return;
        }
        let frame = ui.check_loading_frame;
        ui.check_loading_frame = (frame + 1) % CHECK_LOADING_FRAMES.len();
        (ui.check_button as *mut AnyObject, frame)
    };
    unsafe { set_check_loading_frame(button, frame) };
}

/// NSTimer target used to cycle the button's Braille glyph on the main run loop.
unsafe fn check_loading_timer_target() -> *mut AnyObject {
    CHECK_LOADING_TIMER_TARGET
        .get_or_init(|| {
            let name = CString::new("OhMyTabUpdateCheckLoadingTimerTarget").unwrap();
            let superclass = class!(NSObject) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(advanceCheckLoadingFrame:),
                advance_check_loading_frame as *mut c_void,
                types.as_ptr(),
            );
            let timeout_types = CString::new("v@:").unwrap();
            class_addMethod(
                cls,
                sel!(handleUpdateCheckTimeout),
                handle_update_check_timeout as *mut c_void,
                timeout_types.as_ptr(),
            );
            objc_registerClassPair(cls);
            let target: *mut AnyObject = msg_send![cls as *const AnyObject, new];
            CallbackTarget::new(target)
        })
        .0
}

/// Start the ASCII Braille animation centered together with the localized status label.
unsafe fn start_check_loading_indicator(button: *mut AnyObject) {
    if button.is_null() {
        return;
    }
    stop_check_loading_indicator(button);
    set_check_loading_frame(button, 0);
    let accessibility_label = make_nsstring(&t("settings.update_checking"));
    let _: () = msg_send![button, setAccessibilityLabel: accessibility_label];
    crate::ffi::CFRelease(accessibility_label as *const c_void);

    let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
    let reduce_motion: bool = msg_send![workspace, accessibilityDisplayShouldReduceMotion];
    let cycle_seconds = if reduce_motion {
        CHECK_LOADING_CYCLE_SECONDS * 2.5
    } else {
        CHECK_LOADING_CYCLE_SECONDS
    };
    let interval = cycle_seconds / CHECK_LOADING_FRAMES.len() as f64;
    UPDATE_UI_STATE.lock().unwrap().check_loading_frame = 1;
    let timer: *mut AnyObject = msg_send![
        class!(NSTimer),
        scheduledTimerWithTimeInterval: interval,
        target: check_loading_timer_target(),
        selector: sel!(advanceCheckLoadingFrame:),
        userInfo: std::ptr::null::<AnyObject>(),
        repeats: true
    ];
    let _: () = msg_send![timer, setTolerance: interval * 0.15];
    UPDATE_UI_STATE.lock().unwrap().check_loading_timer = timer as usize;
}

/// Update the About page check-updates button title and enabled state.
pub(crate) fn set_check_button_status(title: &str, enabled: bool) {
    let button = UPDATE_UI_STATE.lock().unwrap().check_button;
    if button == 0 {
        return;
    }
    unsafe {
        let btn = button as *mut AnyObject;
        if enabled {
            stop_check_loading_indicator(btn);
        }
        let ns = make_nsstring(title);
        let _: () = msg_send![btn, setTitle: ns];
        let _: () = msg_send![btn, setAccessibilityLabel: ns];
        crate::ffi::CFRelease(ns as *const c_void);
        let _: () = msg_send![btn, setEnabled: enabled];
    }
}

/// Restore the About check button to its default "Check for Updates…" title and enabled state.
fn reset_check_button() {
    clear_inline_check();
    set_check_button_status(&t("settings.btn_check_for_updates"), true);
}

/// Enter the inline checking phase: set the button to that label and disable it, record the start
/// time, and arm a timeout guard thread so the button cannot get stuck if Sparkle is silent.
pub(crate) fn begin_inline_check() {
    // No inline check button means there is nothing to guard.
    if UPDATE_UI_STATE.lock().unwrap().check_button == 0 {
        return;
    }

    // Arm the guard before touching AppKit so a stuck UI update cannot prevent the fallback from
    // ever being scheduled.
    *CHECK_TIMER.lock().unwrap() = Some(Instant::now());
    let timeout_target = unsafe { check_loading_timer_target() } as usize;
    std::thread::spawn(move || {
        std::thread::sleep(CHECK_TIMEOUT);
        let stale = {
            let timer = CHECK_TIMER.lock().unwrap();
            match *timer {
                Some(start) => start.elapsed() >= CHECK_TIMEOUT,
                None => false,
            }
        };
        if stale {
            log_info!(
                "Sparkle update check timed out after {}s; scheduling main-thread recovery",
                CHECK_TIMEOUT.as_secs()
            );
            unsafe {
                let target = timeout_target as *mut AnyObject;
                if !target.is_null() {
                    let _: () = msg_send![
                        target,
                        performSelectorOnMainThread: sel!(handleUpdateCheckTimeout),
                        withObject: std::ptr::null::<AnyObject>(),
                        waitUntilDone: false
                    ];
                }
            }
        }
    });

    set_check_button_status(&t("settings.update_checking"), false);
    let check_button = UPDATE_UI_STATE.lock().unwrap().check_button as *mut AnyObject;
    unsafe {
        // End the previous statement so the MutexGuard is released before entering the animator.
        start_check_loading_indicator(check_button)
    };
}

/// Recover the inline check on the main thread after Sparkle stays silent.
extern "C" fn handle_update_check_timeout(_this: *mut c_void, _cmd: Sel) {
    if CHECK_TIMER.lock().unwrap().is_none() {
        return;
    }
    clear_inline_check();
    set_check_button_status(&t("settings.btn_retry_update_check"), true);
}

/// Clear the inline checking timer to mark that a result has arrived.
fn clear_inline_check() {
    *CHECK_TIMER.lock().unwrap() = None;
}

unsafe fn make_custom_update_window(driver: *mut c_void, cancellation: *mut c_void) {
    close_custom_update_window();

    let target = render_target(190.0);
    let window_w = 520.0;
    let (content, window) = if target.host.is_null() {
        let window_frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(window_w, 190.0));
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
        (msg_send![window, contentView], window)
    } else {
        (target.parent, std::ptr::null_mut())
    };
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
    add_control(
        target,
        window_w,
        label,
        NSRect::new(NSPoint::new(32.0, 118.0), NSSize::new(456.0, 28.0)),
        content,
    );

    let progress: *mut AnyObject = msg_send![class!(NSProgressIndicator), alloc];
    let progress: *mut AnyObject = msg_send![
        progress,
        initWithFrame: NSRect::new(NSPoint::new(32.0, 78.0), NSSize::new(456.0, 16.0))
    ];
    let _: () = msg_send![progress, setIndeterminate: true];
    let _: () = msg_send![progress, startAnimation: std::ptr::null_mut::<AnyObject>()];
    add_control(
        target,
        window_w,
        progress,
        NSRect::new(NSPoint::new(32.0, 78.0), NSSize::new(456.0, 16.0)),
        content,
    );

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
    add_control(
        target,
        window_w,
        cancel,
        NSRect::new(NSPoint::new(350.0, 24.0), NSSize::new(138.0, 34.0)),
        content,
    );

    if !window.is_null() {
        let _: () = msg_send![window, center];
        let _: () = msg_send![window, makeKeyAndOrderFront: std::ptr::null_mut::<AnyObject>()];
    }

    let copied_cancellation = copy_block(cancellation) as usize;
    let mut ui = UPDATE_UI_STATE.lock().unwrap();
    // Inline: window is null so ui.window stays 0 (the host is tracked via host_view); focus and
    // title use host_window.
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

    let target = render_target(250.0);
    let window_w = 520.0;
    let (content, window) = if target.host.is_null() {
        let window_frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(window_w, 250.0));
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
        (msg_send![window, contentView], window)
    } else {
        (target.parent, std::ptr::null_mut())
    };
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
    add_control(
        target,
        window_w,
        title,
        NSRect::new(NSPoint::new(32.0, 164.0), NSSize::new(456.0, 32.0)),
        content,
    );

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
    add_control(
        target,
        window_w,
        message,
        NSRect::new(NSPoint::new(32.0, 106.0), NSSize::new(456.0, 44.0)),
        content,
    );

    let ok = crate::settings::components::SettingsButton::action(
        NSRect::new(NSPoint::new(350.0, 24.0), NSSize::new(138.0, 34.0)),
        &t("settings.btn_ok"),
        driver as *mut AnyObject,
        sel!(acknowledgeCustomUpdateResult:),
        crate::settings::components::SettingsButtonRole::Action,
    );
    add_control(
        target,
        window_w,
        ok,
        NSRect::new(NSPoint::new(350.0, 24.0), NSSize::new(138.0, 34.0)),
        content,
    );
    crate::settings::widgets::refresh_settings_button_tracking(ok);

    if !window.is_null() {
        let _: () = msg_send![window, center];
        let _: () = msg_send![window, makeKeyAndOrderFront: std::ptr::null_mut::<AnyObject>()];
    }

    let copied_acknowledgement = copy_block(acknowledgement) as usize;
    let mut ui = UPDATE_UI_STATE.lock().unwrap();
    ui.window = window as usize;
    ui.acknowledgement = copied_acknowledgement;
}

/// Build the first-run update permission window without Sparkle's standard icon-bearing UI.
unsafe fn make_custom_permission_window(driver: *mut c_void, reply: *mut c_void) {
    close_custom_update_window();
    let target = render_target(240.0);
    let window_w = 560.0;
    let (content, window) = if target.host.is_null() {
        let window_frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(window_w, 240.0));
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
        (msg_send![window, contentView], window)
    } else {
        (target.parent, std::ptr::null_mut())
    };
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
    add_control(
        target,
        window_w,
        title,
        NSRect::new(NSPoint::new(32.0, 172.0), NSSize::new(496.0, 32.0)),
        content,
    );

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
    add_control(
        target,
        window_w,
        message,
        NSRect::new(NSPoint::new(32.0, 112.0), NSSize::new(496.0, 44.0)),
        content,
    );

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
    add_control(
        target,
        window_w,
        later,
        NSRect::new(NSPoint::new(32.0, 28.0), NSSize::new(180.0, 36.0)),
        content,
    );

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
    add_control(
        target,
        window_w,
        enable,
        NSRect::new(NSPoint::new(348.0, 28.0), NSSize::new(180.0, 36.0)),
        content,
    );

    let copied_reply = copy_block(reply) as usize;
    let mut ui = UPDATE_UI_STATE.lock().unwrap();
    ui.window = window as usize;
    ui.permission_reply = copied_reply;

    if !window.is_null() {
        let _: () = msg_send![window, center];
        let _: () = msg_send![window, makeKeyAndOrderFront: std::ptr::null_mut::<AnyObject>()];
    }
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

/// Build the custom update window's release-notes view from Sparkle's appcast item description.
unsafe fn make_release_notes_view(
    item: *mut AnyObject,
    width: f64,
) -> Option<(*mut AnyObject, f64)> {
    if item.is_null() {
        return None;
    }

    let description: *mut AnyObject = msg_send![item, itemDescription];
    let description = nsstring_to_string(description);
    let description = select_release_notes_locale(&description, &crate::i18n::current_locale());
    if description.trim().is_empty() {
        return None;
    }

    let format: *mut AnyObject = msg_send![item, itemDescriptionFormat];
    let format = nsstring_to_string(format);
    let document = if format.eq_ignore_ascii_case("markdown") {
        render_release_notes_markdown(&description)
    } else {
        ReleaseNotesDocument {
            text: description,
            heading_ranges: Vec::new(),
        }
    };
    if document.text.trim().is_empty() {
        return None;
    }

    let source = make_nsstring(&document.text);
    let attributed: *mut AnyObject = msg_send![class!(NSMutableAttributedString), alloc];
    let attributed: *mut AnyObject = msg_send![attributed, initWithString: source];
    crate::ffi::CFRelease(source as *const c_void);
    if attributed.is_null() {
        return None;
    }

    let length: usize = msg_send![attributed, length];
    if length > 0 {
        let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 14.0f64];
        let color: *mut AnyObject = msg_send![class!(NSColor), labelColor];
        let font_key = make_nsstring("NSFont");
        let color_key = make_nsstring("NSForegroundColor");
        let _: () = msg_send![
            attributed,
            addAttribute: font_key,
            value: font,
            range: NSRange::new(0, length)
        ];
        let _: () = msg_send![
            attributed,
            addAttribute: color_key,
            value: color,
            range: NSRange::new(0, length)
        ];
        for heading in &document.heading_ranges {
            let size = if heading.level == 1 { 18.0 } else { 16.0 };
            let heading_font: *mut AnyObject =
                msg_send![class!(NSFont), boldSystemFontOfSize: size];
            let _: () = msg_send![
                attributed,
                addAttribute: font_key,
                value: heading_font,
                range: heading.range
            ];
        }
        crate::ffi::CFRelease(font_key as *const c_void);
        crate::ffi::CFRelease(color_key as *const c_void);
    }

    let text_view: *mut AnyObject = msg_send![class!(NSTextView), alloc];
    let text_view: *mut AnyObject = msg_send![
        text_view,
        initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, 1.0))
    ];
    if text_view.is_null() {
        release_obj(attributed);
        return None;
    }
    let _: () = msg_send![text_view, setEditable: false];
    let _: () = msg_send![text_view, setSelectable: true];
    let _: () = msg_send![text_view, setDrawsBackground: false];
    let _: () = msg_send![text_view, setTextContainerInset: NSSize::new(0.0, 0.0)];
    let text_container: *mut AnyObject = msg_send![text_view, textContainer];
    let _: () = msg_send![text_container, setLineFragmentPadding: 0.0f64];
    let _: () = msg_send![text_container, setWidthTracksTextView: true];
    let _: () = msg_send![text_container, setContainerSize: NSSize::new(width, 1_000_000.0)];
    let _: () = msg_send![text_view, setHorizontallyResizable: false];
    let _: () = msg_send![text_view, setVerticallyResizable: true];
    // NSTextView receives rich text through its text storage; it has no setAttributedString:
    // selector of its own.
    let text_storage: *mut AnyObject = msg_send![text_view, textStorage];
    let _: () = msg_send![text_storage, setAttributedString: attributed];
    release_obj(attributed);

    let layout: *mut AnyObject = msg_send![text_view, layoutManager];
    let _: () = msg_send![layout, ensureLayoutForTextContainer: text_container];
    let used: NSRect = msg_send![layout, usedRectForTextContainer: text_container];
    let content_height = (used.size.height.ceil() + 4.0).max(24.0);
    let visible_height = content_height.min(UPDATE_RELEASE_NOTES_MAX_HEIGHT);

    let scroll: *mut AnyObject = msg_send![class!(NSScrollView), alloc];
    let scroll: *mut AnyObject = msg_send![
        scroll,
        initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, visible_height))
    ];
    if scroll.is_null() {
        release_obj(text_view);
        return None;
    }
    let _: () = msg_send![scroll, setBorderType: 0u64];
    let _: () = msg_send![scroll, setDrawsBackground: false];
    let _: () = msg_send![scroll, setHasHorizontalScroller: false];
    let _: () = msg_send![scroll, setHasVerticalScroller: content_height > visible_height];
    let _: () = msg_send![scroll, setAutohidesScrollers: true];
    let _: () = msg_send![scroll, setScrollerStyle: 1isize];
    let _: () = msg_send![text_view, setFrameSize: NSSize::new(width, content_height)];
    let _: () = msg_send![text_view, setVerticallyResizable: false];
    let _: () = msg_send![scroll, setDocumentView: text_view];
    release_obj(text_view);
    Some((scroll, visible_height))
}

struct ReleaseNotesDocument {
    text: String,
    heading_ranges: Vec<ReleaseNotesHeadingRange>,
}

struct ReleaseNotesHeadingRange {
    range: NSRange,
    level: u8,
}

/// Render the block-level Markdown used by release notes with explicit line breaks and styles.
///
/// Foundation's Markdown initializer stores headings and lists as presentation-intent
/// attributes. A plain NSTextView does not consistently lay those attributes out, so the
/// release-note subset is normalized here before it enters TextKit.
fn render_release_notes_markdown(source: &str) -> ReleaseNotesDocument {
    let mut text = String::new();
    let mut heading_ranges = Vec::new();

    for line in source.lines() {
        let (line, heading_level) = markdown_heading(line);
        let (line, is_list) = markdown_list_item(line);
        let line = if is_list {
            format!("• {line}")
        } else {
            line.to_string()
        };
        let start = text.encode_utf16().count();
        text.push_str(&line);
        let length = line.encode_utf16().count();
        if let Some(level) = heading_level {
            heading_ranges.push(ReleaseNotesHeadingRange {
                range: NSRange::new(start, length),
                level,
            });
        }
        text.push('\n');
    }

    ReleaseNotesDocument {
        text,
        heading_ranges,
    }
}

fn markdown_heading(line: &str) -> (&str, Option<u8>) {
    let hash_count = line.chars().take_while(|ch| *ch == '#').count();
    if !(1..=6).contains(&hash_count) {
        return (line, None);
    }
    let Some(rest) = line.get(hash_count..) else {
        return (line, None);
    };
    if !rest.starts_with(char::is_whitespace) {
        return (line, None);
    }
    (rest.trim_start(), Some(hash_count as u8))
}

fn markdown_list_item(line: &str) -> (&str, bool) {
    if let Some(rest) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
        (rest, true)
    } else {
        (line, false)
    }
}

/// Select one locale section from a combined release-notes Markdown document.
///
/// Sections use HTML comments so the complete document can be embedded in Sparkle's single
/// `<description>` element without adding visible marker text to the rendered Markdown:
/// `<!-- locale: zh-Hans -->` ... `<!-- /locale -->`.
fn select_release_notes_locale(source: &str, locale: &str) -> String {
    let mut sections: Vec<(String, String)> = Vec::new();
    let mut active: Option<(String, String)> = None;

    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(tag) = trimmed.strip_prefix("<!-- locale:").and_then(|rest| {
            rest.strip_suffix("-->")
                .map(str::trim)
                .filter(|tag| !tag.is_empty())
        }) {
            if let Some(section) = active.take() {
                sections.push(section);
            }
            active = Some((tag.to_string(), String::new()));
            continue;
        }
        if trimmed == RELEASE_NOTES_LOCALE_END {
            if let Some(section) = active.take() {
                sections.push(section);
            }
            continue;
        }
        if let Some((_, content)) = active.as_mut() {
            content.push_str(line);
            content.push('\n');
        }
    }
    if let Some(section) = active {
        sections.push(section);
    }

    // A legacy single-language document remains valid and is displayed as-is.
    if sections.is_empty() {
        return source.to_string();
    }

    let locale_matches = |tag: &str, wanted: &str| {
        tag.eq_ignore_ascii_case(wanted) || (wanted == "zh-Hans" && tag.eq_ignore_ascii_case("zh"))
    };
    sections
        .iter()
        .find(|(tag, _)| locale_matches(tag, locale))
        .or_else(|| sections.iter().find(|(tag, _)| locale_matches(tag, "en")))
        .or_else(|| sections.first())
        .map(|(_, content)| content.trim().to_string())
        .unwrap_or_default()
}

/// Build the update-available prompt without using Sparkle's standard alert.
unsafe fn make_custom_update_found_window(
    driver: *mut c_void,
    item: *mut c_void,
    stage: isize,
    information_only: bool,
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
    let prompt_kind = update_prompt_kind(stage, information_only);
    let title_key = match prompt_kind {
        UpdatePromptKind::Available => "settings.update_available_title",
        UpdatePromptKind::Downloaded => "settings.update_downloaded_title",
        UpdatePromptKind::Installing => "settings.update_installing_title",
        UpdatePromptKind::InformationOnly => "settings.update_information_title",
    };
    let title_text = tf(title_key, &[("app", &app)]);
    let message_key = match prompt_kind {
        UpdatePromptKind::Available => "settings.update_available_message",
        UpdatePromptKind::Downloaded => "settings.update_downloaded_message",
        UpdatePromptKind::Installing => "settings.update_installing_message",
        UpdatePromptKind::InformationOnly => "settings.update_information_message",
    };
    let message_text = tf(message_key, &[("app", &app), ("version", &version)]);
    let information_url = if prompt_kind == UpdatePromptKind::InformationOnly {
        let url: *mut AnyObject = msg_send![item, infoURL];
        let absolute_string: *mut AnyObject = msg_send![url, absoluteString];
        nsstring_to_string(absolute_string)
    } else {
        String::new()
    };

    let window_w = 640.0;
    let button_y = 14.0;
    let button_gap = 4.0;
    let message_h = 44.0;
    let title_h = 32.0;
    let layout_scale = {
        let ui = UPDATE_UI_STATE.lock().unwrap();
        if ui.host_view == 0 {
            1.0
        } else {
            let frame: NSRect = msg_send![ui.host_view as *mut AnyObject, frame];
            (frame.size.width / window_w).max(0.1)
        }
    };
    let skip_w = if prompt_kind == UpdatePromptKind::InformationOnly {
        180.0 * layout_scale
    } else {
        166.0 * layout_scale
    };
    let later_w = 166.0 * layout_scale;
    let install_w = if prompt_kind == UpdatePromptKind::InformationOnly {
        180.0 * layout_scale
    } else {
        200.0 * layout_scale
    };

    // Measure all actions through the shared settings button helper. The three buttons stay the
    // same height, so a long localized action cannot make only one control look misaligned.
    let skip_title = match prompt_kind {
        UpdatePromptKind::Installing => t("settings.btn_cancel_update_installation"),
        UpdatePromptKind::InformationOnly => t("settings.btn_remind_later"),
        _ => t("settings.btn_skip_version"),
    };
    let skip_action = if prompt_kind == UpdatePromptKind::InformationOnly {
        sel!(dismissCustomUpdate:)
    } else {
        sel!(skipCustomUpdate:)
    };
    let skip = crate::settings::components::SettingsButton::action(
        NSRect::new(NSPoint::new(32.0, button_y), NSSize::new(166.0, 36.0)),
        &skip_title,
        driver as *mut AnyObject,
        skip_action,
        crate::settings::components::SettingsButtonRole::Action,
    );
    let skip_h = crate::settings::widgets::configure_settings_button_wrapping(skip, skip_w, 3);

    let later_title = t("settings.btn_remind_later");
    let later = crate::settings::components::SettingsButton::action(
        NSRect::new(NSPoint::new(220.0, button_y), NSSize::new(166.0, 36.0)),
        &later_title,
        driver as *mut AnyObject,
        sel!(dismissCustomUpdate:),
        crate::settings::components::SettingsButtonRole::Action,
    );
    let later_h = crate::settings::widgets::configure_settings_button_wrapping(later, later_w, 3);
    if prompt_kind == UpdatePromptKind::InformationOnly {
        let _: () = msg_send![later, setHidden: true];
    }

    let install_title = match prompt_kind {
        UpdatePromptKind::Installing => t("settings.btn_install_relaunch"),
        UpdatePromptKind::InformationOnly => t("settings.btn_open_update_info"),
        _ => t("settings.btn_install_update"),
    };
    let install_action = if prompt_kind == UpdatePromptKind::InformationOnly {
        sel!(openInformationUpdate:)
    } else {
        sel!(installCustomUpdate:)
    };
    let install = crate::settings::components::SettingsButton::action(
        NSRect::new(NSPoint::new(408.0, button_y), NSSize::new(200.0, 36.0)),
        &install_title,
        driver as *mut AnyObject,
        install_action,
        crate::settings::components::SettingsButtonRole::Primary,
    );
    if prompt_kind == UpdatePromptKind::InformationOnly
        && !is_safe_update_info_url(&information_url)
    {
        let _: () = msg_send![install, setEnabled: false];
    }
    let key_equivalent = make_nsstring("\r");
    let _: () = msg_send![install, setKeyEquivalent: key_equivalent];
    crate::ffi::CFRelease(key_equivalent as *const c_void);
    let install_h =
        crate::settings::widgets::configure_settings_button_wrapping(install, install_w, 3);

    let button_h = [skip_h, later_h, install_h]
        .into_iter()
        .fold(36.0f64, f64::max);
    let release_notes = make_release_notes_view(item, 576.0);
    let release_notes_h = release_notes.map_or(0.0, |(_, height)| height);
    let release_notes_y = button_y + button_h + button_gap;
    let message_y = if release_notes_h > 0.0 {
        release_notes_y + release_notes_h + button_gap
    } else {
        release_notes_y
    };
    let title_y = message_y + message_h + 10.0;
    let window_h = title_y + title_h + 8.0;
    let target = render_target(window_h);
    let (content, window) = if target.host.is_null() {
        let window_frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(window_w, window_h));
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
        (msg_send![window, contentView], window)
    } else {
        (target.parent, std::ptr::null_mut())
    };
    let title: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let title: *mut AnyObject = msg_send![
        title,
        initWithFrame: NSRect::new(NSPoint::new(32.0, title_y), NSSize::new(576.0, title_h))
    ];
    let title_ns = make_nsstring(&title_text);
    let _: () = msg_send![title, setStringValue: title_ns];
    crate::ffi::CFRelease(title_ns as *const c_void);
    let _: () = msg_send![title, setBezeled: false];
    let _: () = msg_send![title, setDrawsBackground: false];
    let _: () = msg_send![title, setEditable: false];
    let _: () = msg_send![title, setSelectable: false];
    let _: () = msg_send![title, setAlignment: 1isize]; // centered
    let title_font: *mut AnyObject = msg_send![class!(NSFont), boldSystemFontOfSize: 22.0f64];
    let _: () = msg_send![title, setFont: title_font];
    add_control(
        target,
        window_w,
        title,
        NSRect::new(NSPoint::new(32.0, title_y), NSSize::new(576.0, title_h)),
        content,
    );

    let message: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let message: *mut AnyObject = msg_send![
        message,
        initWithFrame: NSRect::new(NSPoint::new(32.0, message_y), NSSize::new(576.0, message_h))
    ];
    let message_ns = make_nsstring(&message_text);
    let _: () = msg_send![message, setStringValue: message_ns];
    crate::ffi::CFRelease(message_ns as *const c_void);
    let _: () = msg_send![message, setBezeled: false];
    let _: () = msg_send![message, setDrawsBackground: false];
    let _: () = msg_send![message, setEditable: false];
    let _: () = msg_send![message, setSelectable: false];
    let _: () = msg_send![message, setAlignment: 1isize]; // centered
    let message_font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 16.0f64];
    let _: () = msg_send![message, setFont: message_font];
    let _: () = msg_send![message, setLineBreakMode: 0u64];
    let _: () = msg_send![message, setMaximumNumberOfLines: 0isize];
    add_control(
        target,
        window_w,
        message,
        NSRect::new(NSPoint::new(32.0, message_y), NSSize::new(576.0, message_h)),
        content,
    );

    if let Some((release_notes, release_notes_h)) = release_notes {
        let release_notes_frame = NSRect::new(
            NSPoint::new(32.0, release_notes_y),
            NSSize::new(576.0, release_notes_h),
        );
        let _: () = msg_send![release_notes, setFrame: release_notes_frame];
        add_control(
            target,
            window_w,
            release_notes,
            release_notes_frame,
            content,
        );
    }

    let (skip_frame, later_frame, install_frame) =
        if prompt_kind == UpdatePromptKind::InformationOnly {
            (
                NSRect::new(NSPoint::new(128.0, button_y), NSSize::new(180.0, button_h)),
                NSRect::new(NSPoint::new(0.0, button_y), NSSize::new(1.0, button_h)),
                NSRect::new(NSPoint::new(332.0, button_y), NSSize::new(180.0, button_h)),
            )
        } else {
            (
                NSRect::new(NSPoint::new(32.0, button_y), NSSize::new(166.0, button_h)),
                NSRect::new(NSPoint::new(220.0, button_y), NSSize::new(166.0, button_h)),
                NSRect::new(NSPoint::new(408.0, button_y), NSSize::new(200.0, button_h)),
            )
        };
    let _: () = msg_send![skip, setFrame: skip_frame];
    add_control(target, window_w, skip, skip_frame, content);
    crate::settings::widgets::center_settings_button_label(skip, button_h);

    let _: () = msg_send![later, setFrame: later_frame];
    add_control(target, window_w, later, later_frame, content);
    crate::settings::widgets::center_settings_button_label(later, button_h);

    let _: () = msg_send![install, setFrame: install_frame];
    add_control(target, window_w, install, install_frame, content);
    crate::settings::widgets::center_settings_button_label(install, button_h);

    if !window.is_null() {
        let _: () = msg_send![window, center];
        let _: () = msg_send![window, makeKeyAndOrderFront: std::ptr::null_mut::<AnyObject>()];
    }

    let copied_reply = copy_block(reply) as usize;
    let mut ui = UPDATE_UI_STATE.lock().unwrap();
    ui.window = window as usize;
    ui.update_reply = copied_reply;
    ui.information_url = information_url;
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

/// Build the download/extraction window without Sparkle's standard icon-bearing window.
unsafe fn make_custom_download_window(driver: *mut c_void, cancellation: *mut c_void) {
    close_custom_update_window();

    let app = app_display_name();
    let target = render_target(INLINE_UPDATE_HEIGHT);
    let window_w = 560.0;
    let (content, window) = if target.host.is_null() {
        let window_frame = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(window_w, INLINE_UPDATE_HEIGHT),
        );
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
        (msg_send![window, contentView], window)
    } else {
        (target.parent, std::ptr::null_mut())
    };
    let title: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let title: *mut AnyObject = msg_send![
        title,
        initWithFrame: NSRect::new(NSPoint::new(32.0, 100.0), NSSize::new(496.0, 32.0))
    ];
    set_string_value(title, &t("settings.update_downloading"));
    let _: () = msg_send![title, setBezeled: false];
    let _: () = msg_send![title, setDrawsBackground: false];
    let _: () = msg_send![title, setEditable: false];
    let _: () = msg_send![title, setSelectable: false];
    let title_font: *mut AnyObject = msg_send![class!(NSFont), boldSystemFontOfSize: 22.0f64];
    let _: () = msg_send![title, setFont: title_font];
    add_control(
        target,
        window_w,
        title,
        NSRect::new(NSPoint::new(32.0, 100.0), NSSize::new(496.0, 32.0)),
        content,
    );

    let progress: *mut AnyObject = msg_send![class!(NSProgressIndicator), alloc];
    let progress: *mut AnyObject = msg_send![
        progress,
        initWithFrame: NSRect::new(NSPoint::new(32.0, 68.0), NSSize::new(496.0, 18.0))
    ];
    let _: () = msg_send![progress, setIndeterminate: false];
    let _: () = msg_send![progress, setMinValue: 0.0f64];
    let _: () = msg_send![progress, setMaxValue: 1.0f64];
    let _: () = msg_send![progress, setDoubleValue: 0.0f64];
    add_control(
        target,
        window_w,
        progress,
        NSRect::new(NSPoint::new(32.0, 68.0), NSSize::new(496.0, 18.0)),
        content,
    );

    let status: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let status: *mut AnyObject = msg_send![
        status,
        initWithFrame: NSRect::new(NSPoint::new(32.0, 40.0), NSSize::new(496.0, 24.0))
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
    add_control(
        target,
        window_w,
        status,
        NSRect::new(NSPoint::new(32.0, 40.0), NSSize::new(496.0, 24.0)),
        content,
    );

    let cancel: *mut AnyObject = msg_send![class!(NSButton), alloc];
    let cancel: *mut AnyObject = msg_send![
        cancel,
        initWithFrame: NSRect::new(NSPoint::new(390.0, 4.0), NSSize::new(138.0, 30.0))
    ];
    let cancel_title = make_nsstring(&t("settings.btn_cancel"));
    let _: () = msg_send![cancel, setTitle: cancel_title];
    crate::ffi::CFRelease(cancel_title as *const c_void);
    let _: () = msg_send![cancel, setBezelStyle: 1u64];
    let _: () = msg_send![cancel, setTarget: driver as *mut AnyObject];
    let _: () = msg_send![cancel, setAction: sel!(cancelCustomDownload:)];
    add_control(
        target,
        window_w,
        cancel,
        NSRect::new(NSPoint::new(390.0, 4.0), NSSize::new(138.0, 30.0)),
        content,
    );

    let copied_cancellation = copy_block(cancellation) as usize;
    let mut ui = UPDATE_UI_STATE.lock().unwrap();
    ui.window = window as usize;
    ui.cancellation = copied_cancellation;
    ui.progress = progress as usize;
    ui.status_label = status as usize;
    ui.cancel_button = cancel as usize;
    ui.expected_length = 0;
    ui.received_length = 0;

    if !window.is_null() {
        let _: () = msg_send![window, center];
        let _: () = msg_send![window, makeKeyAndOrderFront: std::ptr::null_mut::<AnyObject>()];
    }
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
    let ui = UPDATE_UI_STATE.lock().unwrap();
    // Inline mode has no window title bar, so this is a no-op.
    if ui.window != 0 {
        let title_ns = make_nsstring(text);
        let _: () = msg_send![ui.window as *mut AnyObject, setTitle: title_ns];
        crate::ffi::CFRelease(title_ns as *const c_void);
    }
}

/// Build the ready-to-install choice window while preserving Sparkle's three choices.
unsafe fn make_custom_choice_window(
    driver: *mut c_void,
    reply: *mut c_void,
    title_text: &str,
    message_text: &str,
) {
    close_custom_update_window();
    // Use the same 560pt design width as the download phase so inline content keeps a stable width.
    let window_w = 560.0;
    let layout_scale = {
        let ui = UPDATE_UI_STATE.lock().unwrap();
        if ui.host_view == 0 {
            1.0
        } else {
            let frame: NSRect = msg_send![ui.host_view as *mut AnyObject, frame];
            (frame.size.width / window_w).max(0.1)
        }
    };
    let button_y = 14.0;
    let button_w = 300.0 * layout_scale;
    let button = crate::settings::components::SettingsButton::action(
        NSRect::new(NSPoint::new(130.0, button_y), NSSize::new(300.0, 36.0)),
        &t("settings.btn_install_update"),
        driver as *mut AnyObject,
        sel!(installCustomUpdate:),
        crate::settings::components::SettingsButtonRole::Primary,
    );
    let button_h =
        crate::settings::widgets::configure_settings_button_wrapping(button, button_w, 3).max(36.0);
    let message_h = 44.0;
    let title_h = 32.0;
    let message_y = button_y + button_h + 4.0;
    let title_y = message_y + message_h + 10.0;
    // Keep a small top inset after the title instead of reserving the old empty check-button
    // area above the ready-to-install content.
    let window_h = title_y + title_h + 8.0;
    let target = render_target(window_h);
    let (content, window) = if target.host.is_null() {
        let window_frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(window_w, window_h));
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
        (msg_send![window, contentView], window)
    } else {
        (target.parent, std::ptr::null_mut())
    };
    let title: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let title: *mut AnyObject = msg_send![
        title,
        initWithFrame: NSRect::new(NSPoint::new(32.0, title_y), NSSize::new(496.0, title_h))
    ];
    set_string_value(title, title_text);
    let _: () = msg_send![title, setBezeled: false];
    let _: () = msg_send![title, setDrawsBackground: false];
    let _: () = msg_send![title, setEditable: false];
    let _: () = msg_send![title, setSelectable: false];
    let title_font: *mut AnyObject = msg_send![class!(NSFont), boldSystemFontOfSize: 20.0f64];
    let _: () = msg_send![title, setFont: title_font];
    add_control(
        target,
        window_w,
        title,
        NSRect::new(NSPoint::new(32.0, title_y), NSSize::new(496.0, title_h)),
        content,
    );

    let message: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let message: *mut AnyObject = msg_send![
        message,
        initWithFrame: NSRect::new(NSPoint::new(32.0, message_y), NSSize::new(496.0, message_h))
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
    add_control(
        target,
        window_w,
        message,
        NSRect::new(NSPoint::new(32.0, message_y), NSSize::new(496.0, message_h)),
        content,
    );

    // The update is already downloaded, so keep only the centered, wider install action.
    let install = button;
    let install_frame = NSRect::new(NSPoint::new(130.0, button_y), NSSize::new(300.0, button_h));
    let _: () = msg_send![install, setFrame: install_frame];
    add_control(target, window_w, install, install_frame, content);
    crate::settings::widgets::center_settings_button_label(install, button_h);
    crate::settings::widgets::refresh_settings_button_tracking(install);

    let copied_reply = copy_block(reply) as usize;
    let mut ui = UPDATE_UI_STATE.lock().unwrap();
    ui.window = window as usize;
    ui.update_reply = copied_reply;

    if !window.is_null() {
        let _: () = msg_send![window, center];
        let _: () = msg_send![window, makeKeyAndOrderFront: std::ptr::null_mut::<AnyObject>()];
    }
}

unsafe fn choose_custom_update(choice: isize) {
    let reply = {
        let mut ui = UPDATE_UI_STATE.lock().unwrap();
        let reply = ui.update_reply;
        ui.update_reply = 0;
        reply
    };
    // An explicit choice acknowledges the update marker even while the About page remains open.
    crate::settings::set_update_available(false);
    close_custom_update_window();
    // skip(0)/dismiss(2) end the flow and collapse the About page; install(1) continues downloading.
    if choice != 1 {
        crate::settings::collapse_update_section();
    }
    invoke_choice_reply(reply, choice);
    release_block(reply);
}

extern "C" fn install_custom_update(_this: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    unsafe { choose_custom_update(1) };
}

extern "C" fn open_information_update(_this: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    unsafe {
        let (reply, url) = {
            let mut ui = UPDATE_UI_STATE.lock().unwrap();
            (
                std::mem::take(&mut ui.update_reply),
                std::mem::take(&mut ui.information_url),
            )
        };
        crate::settings::collapse_update_section();
        close_custom_update_window();
        if is_safe_update_info_url(&url) {
            let url_ns = make_nsstring(&url);
            let url_obj: *mut AnyObject = msg_send![class!(NSURL), URLWithString: url_ns];
            let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
            if !url_obj.is_null() {
                let _: bool = msg_send![workspace, openURL: url_obj];
            }
            crate::ffi::CFRelease(url_ns as *const c_void);
        }
        invoke_choice_reply(reply, 2);
        release_block(reply);
    }
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
        crate::settings::collapse_update_section();
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
        // Installation starts: record the current version as the pending marker; the new
        // instance announces via update_notice at startup (the Sparkle
        // showUpdateInstalledAndRelaunched callback is unreachable for automatic installs).
        crate::update_notice::mark_install_started(&bundle_info_string("CFBundleVersion"));
        crate::update_notice::mark_permission_migration_source(&bundle_info_string(
            "CFBundleShortVersionString",
        ));
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
        log_debug!("[update-notice] driver callback showUpdateInstalledAndRelaunched fired (relaunched={})", _relaunched);
        crate::settings::set_update_available(false);
        // Sparkle calls this on the freshly relaunched instance after an install; besides
        // the in-app result window, post a system notification so a menu-bar app that
        // updated in the background is still visible to the user.
        let app = app_display_name();
        let version = bundle_info_string("CFBundleShortVersionString");
        crate::update_notice::post_update_installed(&app, &version);
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
        let ui = UPDATE_UI_STATE.lock().unwrap();
        // Inline mode focuses the host's settings window; otherwise the update popup.
        let window = if ui.host_view != 0 {
            ui.host_window
        } else {
            ui.window
        };
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
    unsafe {
        log_debug!("Sparkle update check started");
        log_update_network_context();
        // When inline, just switch the button to "Checking…" and disable it; no popup or extras.
        if UPDATE_UI_STATE.lock().unwrap().host_view != 0 {
            begin_inline_check();
            return;
        }
        make_custom_update_window(this, cancellation)
    };
}

extern "C" fn cancel_custom_update_check(_this: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    unsafe {
        let cancellation = UPDATE_UI_STATE.lock().unwrap().cancellation;
        invoke_block(cancellation);
        close_custom_update_window();
        reset_check_button();
        crate::settings::collapse_update_section();
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
        crate::settings::collapse_update_section();
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
        let information_only: bool = msg_send![item as *mut AnyObject, isInformationOnlyUpdate];
        let stage: isize = msg_send![state as *mut AnyObject, stage];
        // A scheduled background check that finds an update must not pop a window: post a
        // system notification instead and reply Later (reminded at the next check). Clicking
        // the banner opens Settings > About and starts a user-initiated check there.
        let user_initiated: bool = msg_send![state as *mut AnyObject, userInitiated];
        // Informational updates have no installable payload; keep them out of the actionable badge.
        crate::settings::set_update_available(!information_only);
        let auto_download = crate::config::CONFIG
            .read()
            .unwrap()
            .updates
            .automatically_download;
        if !user_initiated && (!auto_download || information_only) {
            let app = app_display_name();
            let display_version: *mut AnyObject =
                msg_send![item as *mut AnyObject, displayVersionString];
            let display_version = nsstring_to_string(display_version);
            crate::update_notice::post_update_available(&app, &display_version);
            invoke_choice_reply(reply as usize, 2); // Later / remind me later
            return;
        }
        // Inline found an update: end the checking phase and restore the button default; the
        // update controls remain rendered in the About page.
        if UPDATE_UI_STATE.lock().unwrap().host_view != 0 {
            clear_inline_check();
            reset_check_button();
        }
        make_custom_update_found_window(this, item, stage, information_only, reply);
    }
}

extern "C" fn show_update_not_found(
    this: *mut c_void,
    _cmd: Sel,
    _error: *mut c_void,
    acknowledgement: *mut c_void,
) {
    unsafe {
        log_sparkle_error("showUpdateNotFoundWithError", _error);
        crate::settings::set_update_available(false);
        // When inline, switch the button to "You're up to date" and re-enable it; no popup.
        if UPDATE_UI_STATE.lock().unwrap().host_view != 0 {
            clear_inline_check();
            set_check_button_status(&t("settings.btn_up_to_date"), true);
            // Acknowledge the inline result so Sparkle can finish its session and accept later checks.
            invoke_block(acknowledgement as usize);
            crate::settings::collapse_update_section();
            return;
        }
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
        log_sparkle_error("showUpdaterError", _error);
        // When inline, offer a retry on the button and re-enable it; no popup is shown.
        if UPDATE_UI_STATE.lock().unwrap().host_view != 0 {
            clear_inline_check();
            set_check_button_status(&t("settings.btn_retry_update_check"), true);
            // Acknowledge the inline error so Sparkle can finish its session and accept retries.
            invoke_block(acknowledgement as usize);
            crate::settings::collapse_update_section();
            return;
        }
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
        crate::settings::collapse_update_section();
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
                sel!(openInformationUpdate:),
                open_information_update as *mut c_void,
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
    // If the previous session completed an update install, announce it here.
    crate::update_notice::check_pending();
    // Test hook: `--test-update-notice` (optionally `=available`) posts the update notification
    // at startup so the pipeline (authorization/banner/i18n copy) can be verified without a real
    // Sparkle update.
    if let Some(mode) = crate::dev_flags::value("test-update-notice")
        // A bare switch means `=1` (the historical environment semantics); `=0` disables it.
        .map(|mode| {
            if mode.is_empty() {
                "1".to_string()
            } else {
                mode
            }
        })
        .filter(|mode| mode != "0")
    {
        let app = unsafe { app_display_name() };
        let version = unsafe { bundle_info_string("CFBundleShortVersionString") };
        // =available exercises the update-AVAILABLE notice (clicking it opens the update
        // window); any other value exercises the update-INSTALLED notice.
        if mode == "available" {
            crate::update_notice::post_update_available(&app, &version);
        } else {
            crate::update_notice::post_update_installed(&app, &version);
        }
    }
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
        // Apply the "automatically download and install" preference.
        let automatically_download = crate::config::CONFIG
            .read()
            .map(|cfg| cfg.updates.automatically_download)
            .unwrap_or(false);
        send_void_bool(
            updater,
            sel!(setAutomaticallyDownloadsUpdates:),
            automatically_download,
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
    // Sparkle reads the feed from the host bundle's SUFeedURL; log that actual value instead of a
    // misleading constant.
    let feed_url = unsafe { bundle_feed_url() };
    log_debug!(
        "Sparkle updater started with custom progress UI (automatic checks: {}, feed: {})",
        automatically_check,
        feed_url
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
    log_debug!("Sparkle automatic update checks set to {}", enabled);
}

/// Apply the About-page automatic-download setting to a running Sparkle updater.
pub(crate) fn set_automatic_downloads(enabled: bool) {
    let guard = state().lock().unwrap();
    let Some(current) = guard.as_ref() else {
        return;
    };
    unsafe {
        send_void_bool(
            current.updater,
            sel!(setAutomaticallyDownloadsUpdates:),
            enabled,
        );
    }
    log_debug!("Sparkle automatic update downloads set to {}", enabled);
}

/// Ask Sparkle to check for updates; the custom user driver presents the update UI.
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
    let updater = state()
        .lock()
        .unwrap()
        .as_ref()
        .map(|current| current.updater);
    let Some(updater) = updater else {
        return false;
    };
    // Do not invoke Sparkle synchronously from the button action. A feed/network stall must not
    // keep the AppKit event handler on the stack; Sparkle still receives the call on main.
    log_debug!("Sparkle checkForUpdates selector scheduled");
    unsafe {
        let _: () = msg_send![
            updater,
            performSelectorOnMainThread: sel!(checkForUpdates),
            withObject: std::ptr::null::<AnyObject>(),
            waitUntilDone: false
        ];
    }
    true
}

#[cfg(test)]
mod tests {
    use super::{
        is_safe_update_info_url, render_release_notes_markdown, select_release_notes_locale,
        update_prompt_kind, UpdatePromptKind,
    };

    #[test]
    fn update_prompt_matches_sparkle_stage_and_information_only_state() {
        assert_eq!(update_prompt_kind(0, false), UpdatePromptKind::Available);
        assert_eq!(update_prompt_kind(1, false), UpdatePromptKind::Downloaded);
        assert_eq!(update_prompt_kind(2, false), UpdatePromptKind::Installing);
        assert_eq!(update_prompt_kind(99, false), UpdatePromptKind::Available);
        assert_eq!(
            update_prompt_kind(2, true),
            UpdatePromptKind::InformationOnly
        );
    }

    #[test]
    fn information_update_links_only_open_https_urls() {
        assert!(is_safe_update_info_url("https://example.com/release"));
        assert!(is_safe_update_info_url("HTTPS://example.com/release"));
        assert!(!is_safe_update_info_url("http://example.com/release"));
        assert!(!is_safe_update_info_url("file:///tmp/update"));
    }

    #[test]
    fn renders_release_note_blocks_with_real_line_breaks() {
        let document = render_release_notes_markdown(
            "# 0.1.8 Dev\n\n## What's New\n\n- First change\n- Second change",
        );

        assert_eq!(
            document.text,
            "0.1.8 Dev\n\nWhat's New\n\n• First change\n• Second change\n"
        );
        assert_eq!(document.heading_ranges.len(), 2);
        assert_eq!(document.heading_ranges[0].level, 1);
        assert_eq!(document.heading_ranges[1].level, 2);
    }

    #[test]
    fn selects_requested_locale_from_combined_notes() {
        let source = "<!-- locale: en -->\nEnglish\n<!-- /locale -->\n\n<!-- locale: zh-Hans -->\n简体中文\n<!-- /locale -->";

        assert_eq!(select_release_notes_locale(source, "zh-Hans"), "简体中文");
        assert_eq!(select_release_notes_locale(source, "en"), "English");
    }

    #[test]
    fn falls_back_to_english_then_first_section() {
        let with_english = "<!-- locale: zh-Hans -->\n简体中文\n<!-- /locale -->\n<!-- locale: en -->\nEnglish\n<!-- /locale -->";
        let without_english = "<!-- locale: zh-Hant -->\n繁體中文\n<!-- /locale -->";

        assert_eq!(
            select_release_notes_locale(with_english, "zh-Hant"),
            "English"
        );
        assert_eq!(
            select_release_notes_locale(without_english, "en"),
            "繁體中文"
        );
    }

    #[test]
    fn keeps_legacy_single_language_notes_unchanged() {
        let source = "# Release notes\n\n- One change";
        assert_eq!(select_release_notes_locale(source, "zh-Hans"), source);
    }
}
