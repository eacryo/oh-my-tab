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
use std::time::{Duration, Instant};

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
    host_view: usize,
    host_window: usize,
    check_button: usize,
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
        host_view: 0,
        host_window: 0,
        check_button: 0,
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

/// 最近一次内联「检查中」的开始时间;用于超时兜底,防止 Sparkle 无回调时按钮永远卡住。
/// When the last inline "checking" phase began, for a timeout fallback so the button never gets
/// stuck if Sparkle never calls back.
static CHECK_TIMER: LazyLock<Mutex<Option<Instant>>> = LazyLock::new(|| Mutex::new(None));

const CHECK_TIMEOUT: Duration = Duration::from_secs(20);

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

/// 读取当前 bundle Info.plist 的 SUFeedURL；缺失时返回空串。日志用它反映 Sparkle 实际使用的
/// feed（而非常量,避免误导)——Sparkle 通过 host bundle 的这个键取 feed。
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

/// 渲染目标:内联时用 About 页宿主视图,否则用独立窗口。
/// Render target: the About page host view when inline, else a standalone window.
#[derive(Clone, Copy)]
struct RenderTarget {
    /// 内联时指向宿主视图,否则为 null。
    /// Points at the host view when inline, null otherwise.
    host: *mut AnyObject,
    /// 添加到子视图的父视图(宿主或窗口 contentView)。
    /// The parent view receiving subviews (host or window contentView).
    parent: *mut AnyObject,
    /// 内联宿主宽度;独立窗口时为 0(用窗口原始坐标)。
    /// Inline host width; 0 for standalone windows (use the window's native coordinates).
    width: f64,
}

/// 决定当前渲染目标:有宿主则内联,否则回退独立窗口。内联时把宿主高度设为该屏幕所需高度,
/// 保持顶边固定在按钮行下方,使顶向下翻转紧凑无空白。
/// Decide the render target: inline when a host is registered, else fall back to a window. Inline
/// sizes the host to the screen's required height, keeping its top fixed below the check button
/// row so the top-down flip is compact without extra blank.
unsafe fn render_target(window_h: f64) -> RenderTarget {
    let ui = UPDATE_UI_STATE.lock().unwrap();
    if ui.host_view != 0 {
        // Host 坐标从 (0,0) 开始,宽度取宿主帧宽,便于内联排布。
        // Host coordinates start at (0,0); width is the host frame width, so inline layout fits.
        // 有宿主(About 页内联)时展开卡片到该屏幕高度,并把宿主设为同高、顶边固定。
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

/// 把独立窗口的一个 frame 内联映射到宿主宽度(按比例缩放 x 与宽)。
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

/// 把一个控件加入渲染目标;内联时按宿主宽度缩放坐标、把宿主高度设为该屏幕高度,并把窗口自底向
/// 上的 y 翻转为宿主顶向下,使标题贴近宿主顶部、按钮贴近宿主底部,内容从按钮行正下方紧凑排布。
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
        // 把窗口自底向上的 y 翻转为宿主顶向下:标题贴近顶部、按钮贴近底部,内容从按钮行下方排布。
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

/// 清除宿主视图内的更新控件(不释放宿主本身,宿主归 About 页父视图所有)。
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
    let mut ui = UPDATE_UI_STATE.lock().unwrap();
    if ui.host_view != 0 {
        // 内联模式:清除宿主内控件,不改动设置窗口。
        // Inline mode: clear the host's controls, leave the settings window untouched.
        clear_host_subviews(ui.host_view as *mut AnyObject);
        ui.window = 0;
    } else if ui.window != 0 {
        let window = ui.window as *mut AnyObject;
        // 关闭窗口前先解除父视图对控件的引用，再释放 alloc 所有权，避免 AppKit 过度释放。
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

/// 设置 About 页的 update host 宿主视图与检查按钮(内联渲染入口)。
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

/// 作废 update host 与检查按钮引用;在设置窗口销毁前调用,避免写已释放视图。
/// Clear the update host and check-button references; called before the settings window is
/// destroyed so the updater never writes to a deallocated view.
pub(crate) fn clear_update_host() {
    unsafe { close_custom_update_window() };
    let mut ui = UPDATE_UI_STATE.lock().unwrap();
    ui.host_view = 0;
    ui.host_window = 0;
    ui.check_button = 0;
}

/// 更新 About 页「检查更新」按钮的文案与可用态;按钮尺寸保持不变。
/// Update the About page check-updates button title and enabled state; its size stays fixed.
pub(crate) fn set_check_button_status(title: &str, enabled: bool) {
    let button = UPDATE_UI_STATE.lock().unwrap().check_button;
    if button == 0 {
        return;
    }
    unsafe {
        let btn = button as *mut AnyObject;
        let ns = make_nsstring(title);
        let _: () = msg_send![btn, setTitle: ns];
        crate::ffi::CFRelease(ns as *const c_void);
        let _: () = msg_send![btn, setEnabled: enabled];
    }
}

/// 恢复 About 页检查按钮为默认「检查更新…」文案并可用。
/// Restore the About check button to its default "Check for Updates…" title and enabled state.
fn reset_check_button() {
    clear_inline_check();
    set_check_button_status(&t("settings.btn_check_for_updates"), true);
}

/// 进入内联「检查中」:把按钮切到该文案并禁用,记录开始时间并启动超时守卫线程。
/// Enter the inline checking phase: set the button to that label and disable it, record the start
/// time, and arm a timeout guard thread so the button cannot get stuck if Sparkle is silent.
pub(crate) fn begin_inline_check() {
    // 非内联(无 About 检查按钮)时无需计时兜底。
    // No inline check button means there is nothing to guard.
    if UPDATE_UI_STATE.lock().unwrap().check_button == 0 {
        return;
    }
    set_check_button_status(&t("settings.update_checking"), false);
    *CHECK_TIMER.lock().unwrap() = Some(Instant::now());
    // 守卫生程:若超时后按钮仍处于禁用(即尚无任何回调恢复),恢复为「已是最新版本」。
    // Guard thread: if the button is still disabled after the timeout (no callback restored it),
    // restore it to "You're up to date".
    std::thread::spawn(|| {
        std::thread::sleep(CHECK_TIMEOUT);
        let stale = {
            let timer = CHECK_TIMER.lock().unwrap();
            match *timer {
                Some(start) => start.elapsed() >= CHECK_TIMEOUT,
                None => false,
            }
        };
        if stale {
            set_check_button_status(&t("settings.btn_up_to_date"), true);
            clear_inline_check();
        }
    });
}

/// 清除内联「检查中」计时,表示已得到结果(无论成功/失败/无更新)。
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
    // 内联时 window 为 null,ui.window 保持 0(宿主由 host_view 记录);聚焦/标题走 host_window。
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
    add_control(
        target,
        window_w,
        ok,
        NSRect::new(NSPoint::new(350.0, 24.0), NSSize::new(138.0, 34.0)),
        content,
    );

    if !window.is_null() {
        let _: () = msg_send![window, center];
        let _: () = msg_send![window, makeKeyAndOrderFront: std::ptr::null_mut::<AnyObject>()];
    }

    let copied_acknowledgement = copy_block(acknowledgement) as usize;
    let mut ui = UPDATE_UI_STATE.lock().unwrap();
    ui.window = window as usize;
    ui.acknowledgement = copied_acknowledgement;
}

/// 创建首次运行的更新权限窗口，避免 Sparkle 的标准权限界面带出应用图标。
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

    let target = render_target(140.0);
    let window_w = 640.0;
    let (content, window) = if target.host.is_null() {
        let window_frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(window_w, 140.0));
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
        initWithFrame: NSRect::new(NSPoint::new(32.0, 108.0), NSSize::new(576.0, 32.0))
    ];
    let title_ns = make_nsstring(&title_text);
    let _: () = msg_send![title, setStringValue: title_ns];
    crate::ffi::CFRelease(title_ns as *const c_void);
    let _: () = msg_send![title, setBezeled: false];
    let _: () = msg_send![title, setDrawsBackground: false];
    let _: () = msg_send![title, setEditable: false];
    let _: () = msg_send![title, setSelectable: false];
    let _: () = msg_send![title, setAlignment: 1isize]; // 居中 / centered
    let title_font: *mut AnyObject = msg_send![class!(NSFont), boldSystemFontOfSize: 22.0f64];
    let _: () = msg_send![title, setFont: title_font];
    add_control(
        target,
        window_w,
        title,
        NSRect::new(NSPoint::new(32.0, 108.0), NSSize::new(576.0, 32.0)),
        content,
    );

    let message: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let message: *mut AnyObject = msg_send![
        message,
        initWithFrame: NSRect::new(NSPoint::new(32.0, 54.0), NSSize::new(576.0, 44.0))
    ];
    let message_ns = make_nsstring(&message_text);
    let _: () = msg_send![message, setStringValue: message_ns];
    crate::ffi::CFRelease(message_ns as *const c_void);
    let _: () = msg_send![message, setBezeled: false];
    let _: () = msg_send![message, setDrawsBackground: false];
    let _: () = msg_send![message, setEditable: false];
    let _: () = msg_send![message, setSelectable: false];
    let _: () = msg_send![message, setAlignment: 1isize]; // 居中 / centered
    let message_font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 16.0f64];
    let _: () = msg_send![message, setFont: message_font];
    let _: () = msg_send![message, setLineBreakMode: 0u64];
    let _: () = msg_send![message, setMaximumNumberOfLines: 0isize];
    add_control(
        target,
        window_w,
        message,
        NSRect::new(NSPoint::new(32.0, 54.0), NSSize::new(576.0, 44.0)),
        content,
    );

    let skip: *mut AnyObject = msg_send![class!(NSButton), alloc];
    let skip: *mut AnyObject = msg_send![
        skip,
        initWithFrame: NSRect::new(NSPoint::new(32.0, 14.0), NSSize::new(166.0, 36.0))
    ];
    let skip_title = make_nsstring(&t("settings.btn_skip_version"));
    let _: () = msg_send![skip, setTitle: skip_title];
    crate::ffi::CFRelease(skip_title as *const c_void);
    let _: () = msg_send![skip, setBezelStyle: 1u64];
    let _: () = msg_send![skip, setTarget: driver as *mut AnyObject];
    let _: () = msg_send![skip, setAction: sel!(skipCustomUpdate:)];
    add_control(
        target,
        window_w,
        skip,
        NSRect::new(NSPoint::new(32.0, 14.0), NSSize::new(166.0, 36.0)),
        content,
    );

    let later: *mut AnyObject = msg_send![class!(NSButton), alloc];
    let later: *mut AnyObject = msg_send![
        later,
        initWithFrame: NSRect::new(NSPoint::new(220.0, 14.0), NSSize::new(166.0, 36.0))
    ];
    let later_title = make_nsstring(&t("settings.btn_remind_later"));
    let _: () = msg_send![later, setTitle: later_title];
    crate::ffi::CFRelease(later_title as *const c_void);
    let _: () = msg_send![later, setBezelStyle: 1u64];
    let _: () = msg_send![later, setTarget: driver as *mut AnyObject];
    let _: () = msg_send![later, setAction: sel!(dismissCustomUpdate:)];
    add_control(
        target,
        window_w,
        later,
        NSRect::new(NSPoint::new(220.0, 14.0), NSSize::new(166.0, 36.0)),
        content,
    );

    let install: *mut AnyObject = msg_send![class!(NSButton), alloc];
    let install: *mut AnyObject = msg_send![
        install,
        initWithFrame: NSRect::new(NSPoint::new(408.0, 14.0), NSSize::new(200.0, 36.0))
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
    add_control(
        target,
        window_w,
        install,
        NSRect::new(NSPoint::new(408.0, 14.0), NSSize::new(200.0, 36.0)),
        content,
    );

    if !window.is_null() {
        let _: () = msg_send![window, center];
        let _: () = msg_send![window, makeKeyAndOrderFront: std::ptr::null_mut::<AnyObject>()];
    }

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
    let target = render_target(238.0);
    let window_w = 560.0;
    let (content, window) = if target.host.is_null() {
        let window_frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(window_w, 238.0));
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
        initWithFrame: NSRect::new(NSPoint::new(32.0, 168.0), NSSize::new(496.0, 32.0))
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
        NSRect::new(NSPoint::new(32.0, 168.0), NSSize::new(496.0, 32.0)),
        content,
    );

    let progress: *mut AnyObject = msg_send![class!(NSProgressIndicator), alloc];
    let progress: *mut AnyObject = msg_send![
        progress,
        initWithFrame: NSRect::new(NSPoint::new(32.0, 116.0), NSSize::new(496.0, 18.0))
    ];
    let _: () = msg_send![progress, setIndeterminate: false];
    let _: () = msg_send![progress, setMinValue: 0.0f64];
    let _: () = msg_send![progress, setMaxValue: 1.0f64];
    let _: () = msg_send![progress, setDoubleValue: 0.0f64];
    add_control(
        target,
        window_w,
        progress,
        NSRect::new(NSPoint::new(32.0, 116.0), NSSize::new(496.0, 18.0)),
        content,
    );

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
    add_control(
        target,
        window_w,
        status,
        NSRect::new(NSPoint::new(32.0, 82.0), NSSize::new(496.0, 24.0)),
        content,
    );

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
    add_control(
        target,
        window_w,
        cancel,
        NSRect::new(NSPoint::new(390.0, 28.0), NSSize::new(138.0, 36.0)),
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
    // 内联时宿主无窗口标题,直接跳过。
    // Inline mode has no window title bar, so this is a no-op.
    if ui.window != 0 {
        let title_ns = make_nsstring(text);
        let _: () = msg_send![ui.window as *mut AnyObject, setTitle: title_ns];
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
    let target = render_target(250.0);
    let window_w = 580.0;
    let (content, window) = if target.host.is_null() {
        let window_frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(window_w, 250.0));
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
        initWithFrame: NSRect::new(NSPoint::new(32.0, 178.0), NSSize::new(516.0, 32.0))
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
        NSRect::new(NSPoint::new(32.0, 178.0), NSSize::new(516.0, 32.0)),
        content,
    );

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
    add_control(
        target,
        window_w,
        message,
        NSRect::new(NSPoint::new(32.0, 118.0), NSSize::new(516.0, 44.0)),
        content,
    );

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
    add_control(
        target,
        window_w,
        skip,
        NSRect::new(NSPoint::new(32.0, 28.0), NSSize::new(150.0, 36.0)),
        content,
    );

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
    add_control(
        target,
        window_w,
        later,
        NSRect::new(NSPoint::new(194.0, 28.0), NSSize::new(150.0, 36.0)),
        content,
    );

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
    add_control(
        target,
        window_w,
        install,
        NSRect::new(NSPoint::new(356.0, 28.0), NSSize::new(192.0, 36.0)),
        content,
    );

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
    close_custom_update_window();
    // skip(0) / dismiss(2) 会结束更新流程,收起 About 页;install(1) 继续下载,保持展开。
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
        let ui = UPDATE_UI_STATE.lock().unwrap();
        // 内联时聚焦宿主所属的设置窗口,否则聚焦更新弹窗。
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
        // 内联(About 页)时只把按钮切到「检查中…」并禁用,不弹窗、不加其他信息。
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
    _state: *mut c_void,
    reply: *mut c_void,
) {
    unsafe {
        // 内联(About 页)发现可用更新:结束「检查中」并恢复按钮默认,后续由更新弹窗呈现。
        // Inline found an update: end the checking phase and restore the button default; the update
        // popup takes over presentation.
        if UPDATE_UI_STATE.lock().unwrap().host_view != 0 {
            clear_inline_check();
            reset_check_button();
        }
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
        // 内联(About 页)时把按钮切到「已是最新版本」并恢复可用,不弹窗。
        // When inline, switch the button to "You're up to date" and re-enable it; no popup.
        if UPDATE_UI_STATE.lock().unwrap().host_view != 0 {
            clear_inline_check();
            set_check_button_status(&t("settings.btn_up_to_date"), true);
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
        // 内联(About 页)时把按钮恢复到默认「检查更新…」并可用,不弹窗。
        // When inline, restore the button to default and re-enable it; no popup.
        if UPDATE_UI_STATE.lock().unwrap().host_view != 0 {
            reset_check_button();
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
        // 应用「自动下载并安装更新」设置。
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
    // Sparkle 实际通过 host bundle 的 SUFeedURL 取 feed；日志读取它,避免打印误导性的常量。
    // Sparkle reads the feed from the host bundle's SUFeedURL; log that actual value instead of a
    // misleading constant.
    let feed_url = unsafe { bundle_feed_url() };
    log_info!(
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
    log_info!("Sparkle automatic update checks set to {}", enabled);
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
    log_info!("Sparkle automatic update downloads set to {}", enabled);
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
