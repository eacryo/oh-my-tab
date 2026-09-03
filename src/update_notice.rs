//! 更新完成后的系统通知:Sparkle 安装更新并重启后,经 user driver 的
//! `showUpdateInstalledAndRelaunched` 回调走到这里,用 UNUserNotificationCenter
//! 发一条本地通知(标题/正文走 i18n)。
//!
//! 授权与回调:首次使用需 requestAuthorization;completion handler 是 ObjC block,
//! 本模块用"无捕获全局 block"的最小手工构造(isa/flags/invoke/descriptor),
//! 不引入 block2 依赖。重 Sparkle 重启后本应用大概率在前台,系统默认会压住横幅,
//! 因此实现 willPresent delegate 让横幅在前台同样弹出。
//!
//! Post-install system notification. Sparkle invokes the user driver's
//! `showUpdateInstalledAndRelaunched` on the freshly relaunched instance; this module
//! turns that into a UNUserNotificationCenter local notification (localized title/body).
//! Authorization needs requestAuthorization whose completion handler is an ObjC block --
//! built here as a hand-rolled no-capture global block (isa/flags/invoke/descriptor) to
//! avoid a block2 dependency. The app is usually FRONTMOST right after Sparkle relaunches
//! it, and macOS suppresses banners for the active app, so a willPresent delegate makes
//! the banner show anyway.

// UserNotifications.framework 必须显式链接:objc2 的 class! 是运行时按名字查类,
// 没有这条链接指令,框架不会被 dyld 加载,类表里查不到 UNUserNotificationCenter,
// msg_send! 的发送前断言会 panic(实测 "method not found")。
// UserNotifications.framework must be force-linked: objc2's class! resolves classes by
// name at runtime, and without this link hint dyld never loads the framework, so the
// class is missing and msg_send!'s pre-send assertion panics ("method not found").
#[link(name = "UserNotifications", kind = "framework")]
extern "C" {}

use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send, sel};
use std::ffi::c_void;
use std::sync::{Mutex, Once, OnceLock};

use crate::ffi::{bundle_info_string, make_nsstring, nsstring_to_rust, objc_msgSend, release_obj};
use crate::i18n::tf;
use crate::log_debug;

// block ABI:全局 block(无捕获)只需要 isa/flags/reserved/invoke/descriptor。
// Block ABI: a no-capture global block needs only isa/flags/reserved/invoke/descriptor.
#[repr(C)]
struct BlockDescriptor {
    reserved: usize,
    size: usize,
}

// BLOCK_IS_GLOBAL(1 << 28):不进堆,实例为静态常量。
// BLOCK_IS_GLOBAL (1 << 28): statically allocated, never copied to the heap.
const BLOCK_IS_GLOBAL: i32 = 1 << 28;

extern "C" {
    static _NSConcreteGlobalBlock: c_void;
}

#[repr(C)]
struct AuthCompletionBlock {
    isa: *const c_void,
    flags: i32,
    reserved: i32,
    invoke: unsafe extern "C" fn(*const AuthCompletionBlock, bool, *mut c_void),
    descriptor: *const BlockDescriptor,
}

#[repr(C)]
struct AddCompletionBlock {
    isa: *const c_void,
    flags: i32,
    reserved: i32,
    invoke: unsafe extern "C" fn(*const AddCompletionBlock, *mut c_void),
    descriptor: *const BlockDescriptor,
}

// 全局 block 实例是只读静态(字段全为裸指针,仅声明 Sync 供 static 存放)。
// Global block instances are read-only statics (raw pointers; Sync is declared only so
// they can live in statics).
unsafe impl Sync for AuthCompletionBlock {}
unsafe impl Sync for AddCompletionBlock {}

static AUTH_DESCRIPTOR: BlockDescriptor = BlockDescriptor {
    reserved: 0,
    size: std::mem::size_of::<AuthCompletionBlock>(),
};
static ADD_DESCRIPTOR: BlockDescriptor = BlockDescriptor {
    reserved: 0,
    size: std::mem::size_of::<AddCompletionBlock>(),
};

// 待投递的 (标题, 正文, 标识符):授权完成回调里取用(全局 block 无法捕获,状态只能走静态槽)。
// Pending (title, body, identifier) consumed by the authorization completion (a global
// block cannot capture, so the handoff goes through this slot).
static PENDING_NOTICE: Mutex<Option<(String, String, String)>> = Mutex::new(None);

// 两类通知各自使用固定标识符:同 ID 重投会替换历史里的旧未读通知,不堆积。
// Two fixed identifiers: re-posting the same ID replaces the previous unread notice
// instead of stacking up.
const ID_UPDATE_INSTALLED: &str = "oh-my-tab-update-installed";
const ID_UPDATE_AVAILABLE: &str = "oh-my-tab-update-available";

static DELEGATE_REGISTERED: Once = Once::new();

/// 授权完成:granted 才投递;结果只进日志。
/// Authorization finished: deliver only when granted; the outcome is logged.
unsafe extern "C" fn auth_completion(
    _block: *const AuthCompletionBlock,
    granted: bool,
    error: *mut c_void,
) {
    if !granted {
        log_debug!("[update-notice] notification authorization denied; skipping");
        *PENDING_NOTICE.lock().unwrap() = None;
        return;
    }
    if !error.is_null() {
        log_debug!("[update-notice] authorization returned an error object despite granted=YES");
    }
    let Some((title, body, identifier)) = PENDING_NOTICE.lock().unwrap().take() else {
        return;
    };
    add_notification_request(&title, &body, &identifier);
}

/// add 的完成回调:仅用于把失败写进日志。
/// add completion: only logs failures.
unsafe extern "C" fn add_completion(_block: *const AddCompletionBlock, error: *mut c_void) {
    if !error.is_null() {
        log_debug!("[update-notice] addNotificationRequest returned an error");
    }
}

static AUTH_BLOCK: AuthCompletionBlock = AuthCompletionBlock {
    isa: std::ptr::addr_of!(_NSConcreteGlobalBlock),
    flags: BLOCK_IS_GLOBAL,
    reserved: 0,
    invoke: auth_completion,
    descriptor: std::ptr::addr_of!(AUTH_DESCRIPTOR),
};

static ADD_BLOCK: AddCompletionBlock = AddCompletionBlock {
    isa: std::ptr::addr_of!(_NSConcreteGlobalBlock),
    flags: BLOCK_IS_GLOBAL,
    reserved: 0,
    invoke: add_completion,
    descriptor: std::ptr::addr_of!(ADD_DESCRIPTOR),
};

/// willPresent 的呈现选项:banner | sound。前台也弹横幅。
/// willPresent presentation options: banner | sound, so the banner shows even frontmost.
const PRESENT_BANNER_SOUND: usize = (1 << 2) | (1 << 1);

/// didReceive:点击横幅 → 匹配"新版本可用"标识符 → 跳主线程打开设置 About 更新页。
/// didReceive: a banner click on the "update available" notification hops to the main
/// thread and opens the About update section in Settings.
unsafe extern "C" fn did_receive_notification_response(
    _this: *mut c_void,
    _cmd: Sel,
    _center: *mut c_void,
    response: *mut c_void,
    completion: *mut c_void,
) {
    let notification: *mut AnyObject = msg_send![response as *mut AnyObject, notification];
    let request: *mut AnyObject = msg_send![notification, request];
    let identifier: *mut AnyObject = msg_send![request, identifier];
    let ours = make_nsstring(ID_UPDATE_AVAILABLE);
    let matched: bool = msg_send![identifier, isEqualToString: ours];
    release_obj(ours);
    if matched {
        let _: () = msg_send![_this as *mut AnyObject,
            performSelectorOnMainThread: sel!(handleUpdateAvailableClick),
            withObject: std::ptr::null::<AnyObject>(),
            waitUntilDone: false
        ];
    }
    if !completion.is_null() {
        // 0 参数 block:直接调用其 invoke 槽。
        // A zero-argument block: call its invoke slot directly.
        let invoke = *(completion as *const *const c_void).add(2);
        let f: unsafe extern "C" fn(*mut c_void) = std::mem::transmute(invoke);
        f(completion);
    }
}

/// 通知点击(主线程):激活应用并在设置 About 页做一次用户级检查。
/// Notification click (main thread): activate the app and run a user-initiated check in
/// the Settings About page.
unsafe extern "C" fn handle_update_available_click(_this: *mut c_void, _cmd: Sel) {
    // 打开设置窗口的 About 页并内联检查;激活与置前由 show_settings 处理
    // (NSApplication 的 activateIgnoringOtherApps: 在 macOS 26 仍然可用)。
    // Open the settings window's About page with an inline check; show_settings handles
    // activation and raising (NSApplication's activateIgnoringOtherApps: still works on
    // macOS 26, unlike the NSRunningApplication variants).
    crate::settings::open_about_updates();
}

/// UNUserNotificationCenterDelegate.willPresentNotification:withCompletionHandler:.
/// 直接调用传入的 completion block(invoke 位于 block 头部第 3 个指针位)。
/// Delegate callback: invokes the passed completion block (invoke sits at the third
/// pointer slot of the block header).
unsafe extern "C" fn will_present_notification(
    _this: *mut c_void,
    _cmd: Sel,
    _notification: *mut c_void,
    completion: *mut c_void,
) {
    if completion.is_null() {
        return;
    }
    type PresentFn = unsafe extern "C" fn(*mut c_void, usize);
    let invoke = *(completion as *const *const c_void).add(2);
    let invoke: PresentFn = std::mem::transmute(invoke);
    invoke(completion, PRESENT_BANNER_SOUND);
}

/// 注册一次 delegate 类与实例(center.delegate 是 weak,实例必须常驻)。
/// Register the delegate class/instance once (center.delegate is weak, so the instance
/// must outlive the call).
unsafe fn ensure_delegate_registered() -> *mut AnyObject {
    static DELEGATE: OnceLock<crate::ffi::ObjPtr> = OnceLock::new();
    DELEGATE_REGISTERED.call_once(|| {
        let name = std::ffi::CString::new("OhMyTabUpdateNoticeDelegate").unwrap();
        let superclass = class!(NSObject) as *const _ as *mut AnyObject;
        let cls = crate::ffi::objc_allocateClassPair(superclass, name.as_ptr(), 0);
        // block 参数在方法签名里按对象类型编码('@')。
        // Block parameters are encoded as objects ('@') in method signatures.
        let will_present_types = std::ffi::CString::new("v@:@@@").unwrap();
        // 注意 delegate 方法的完整选择器带 "userNotificationCenter:" 前缀,
        // 写漏了运行时会查不到,前台横幅会被静默压掉(实测踩坑)。
        // The delegate selector must include the "userNotificationCenter:" prefix; a
        // missing prefix silently never fires and the frontmost banner is suppressed.
        crate::ffi::class_addMethod(
            cls,
            sel!(userNotificationCenter:willPresentNotification:withCompletionHandler:),
            will_present_notification as *mut c_void,
            will_present_types.as_ptr(),
        );
        let did_receive_types = std::ffi::CString::new("v@:@@@").unwrap();
        crate::ffi::class_addMethod(
            cls,
            sel!(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:),
            did_receive_notification_response as *mut c_void,
            did_receive_types.as_ptr(),
        );
        // 点击通知的主线程跳板(后台队列不能直接驱动 Sparkle)。
        // Main-thread trampoline for notification clicks (Sparkle is main-thread only).
        // 这是无参数 selector; `performSelectorOnMainThread:withObject:` 传入的
        // withObject 只对应 performSelector API,不会成为目标 selector 的参数。
        // This selector takes no arguments; the `withObject:` belongs to the
        // performSelector API and is not an argument to the target selector.
        let click_types = std::ffi::CString::new("v@:").unwrap();
        crate::ffi::class_addMethod(
            cls,
            sel!(handleUpdateAvailableClick),
            handle_update_available_click as *mut c_void,
            click_types.as_ptr(),
        );
        crate::ffi::objc_registerClassPair(cls);
        let obj: *mut AnyObject = msg_send![cls as *const AnyObject, new];
        let _ = DELEGATE.set(crate::ffi::ObjPtr(obj));
    });
    DELEGATE.get().map(|p| p.0).unwrap_or(std::ptr::null_mut())
}

/// 构建 content + request 并投递(在授权 granted 之后调用)。
/// Build content + request and deliver (called once authorization is granted).
unsafe fn add_notification_request(title: &str, body: &str, identifier: &str) {
    let center: *mut AnyObject =
        msg_send![class!(UNUserNotificationCenter), currentNotificationCenter];
    if center.is_null() {
        return;
    }
    let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];

    let content: *mut AnyObject = msg_send![class!(UNMutableNotificationContent), alloc];
    let content: *mut AnyObject = msg_send![content, init];
    let title_ns = make_nsstring(title);
    let body_ns = make_nsstring(body);
    let _: () = msg_send![content, setTitle: title_ns];
    let _: () = msg_send![content, setBody: body_ns];
    let sound: *mut AnyObject = msg_send![class!(UNNotificationSound), defaultSound];
    let _: () = msg_send![content, setSound: sound];

    let id_ns = make_nsstring(identifier);
    let request: *mut AnyObject = msg_send![class!(UNNotificationRequest),
        requestWithIdentifier: id_ns,
        content: content,
        trigger: std::ptr::null::<AnyObject>()
    ];
    // block 参数走手工 objc_msgSend:objc2 的发送前断言要求 block 参数带 '@?' 编码,
    // 裸指针会被拒绝;用具体签名的 msgSend 绕开检查(全局 block 本身符合 block ABI)。
    // Block-taking sends go through a hand-transmuted objc_msgSend: objc2's pre-send
    // assertion demands the '@?' encoding for block args and rejects raw pointers; the
    // global block itself is a valid block, so the concrete-signature call is safe.
    type AddNotificationFn =
        unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject, *const AddCompletionBlock);
    let send: AddNotificationFn = std::mem::transmute(objc_msgSend as *const ());
    send(
        center,
        sel!(addNotificationRequest:withCompletionHandler:),
        request,
        std::ptr::addr_of!(ADD_BLOCK),
    );

    release_obj(title_ns);
    release_obj(body_ns);
    release_obj(id_ns);
    let _: () = msg_send![pool, drain];
    log_debug!("[update-notice] notification delivered: {}", identifier);
}

// ---------- 跨启动标记(NSUserDefaults) / cross-launch marker (NSUserDefaults) ----------
//
// Sparkle 的 showUpdateInstalledAndRelaunched 回调在「更新器进程仍存活」时才会被调用,
// 而自动安装流程里旧实例早已退出——所以"安装完成"的通知不能挂在它上面。改为:
// 安装开始时(旧实例)写标记,新实例启动时读标记发通知。
// Sparkle only invokes showUpdateInstalledAndRelaunched while the updater process is
// still alive -- the old instance is long gone during automatic installs. So instead of
// that callback, the old instance writes a marker when installation starts and the new
// instance announces the update at startup.

const PENDING_MARKER_KEY: &str = "update_notice_pending_from_version";

unsafe fn defaults_set_string(key: &str, value: &str) {
    let defaults: *mut AnyObject = msg_send![class!(NSUserDefaults), standardUserDefaults];
    let key_ns = make_nsstring(key);
    let val_ns = make_nsstring(value);
    let _: () = msg_send![defaults, setObject: val_ns, forKey: key_ns];
    release_obj(val_ns);
    release_obj(key_ns);
}

unsafe fn defaults_get_string(key: &str) -> String {
    let defaults: *mut AnyObject = msg_send![class!(NSUserDefaults), standardUserDefaults];
    let key_ns = make_nsstring(key);
    let value: *mut AnyObject = msg_send![defaults, stringForKey: key_ns];
    release_obj(key_ns);
    nsstring_to_rust(value)
}

unsafe fn defaults_remove(key: &str) {
    let defaults: *mut AnyObject = msg_send![class!(NSUserDefaults), standardUserDefaults];
    let key_ns = make_nsstring(key);
    let _: () = msg_send![defaults, removeObjectForKey: key_ns];
    release_obj(key_ns);
}

/// 安装开始时(旧实例,showInstallingUpdate 回调)记录"当前版本"。
/// Announce-prep: the old instance records its CURRENT build version when installation
/// starts.
pub(crate) fn mark_install_started(from_version: &str) {
    unsafe { defaults_set_string(PENDING_MARKER_KEY, from_version) };
    log_debug!(
        "[update-notice] install started; pending marker set (from {})",
        from_version
    );
}

/// 新实例启动时检查标记:版本确实变了才发通知,并清掉标记。
/// At startup, announce the update when the marker exists AND the build version changed;
/// the marker is always consumed.
pub(crate) fn check_pending() {
    let bundle_id = unsafe { bundle_info_string("CFBundleIdentifier") };
    if bundle_id.is_empty() {
        return;
    }
    let from_version = unsafe { defaults_get_string(PENDING_MARKER_KEY) };
    if from_version.is_empty() {
        return;
    }
    unsafe { defaults_remove(PENDING_MARKER_KEY) };
    let current = unsafe { bundle_info_string("CFBundleVersion") };
    if current.is_empty() || current == from_version {
        // 安装未完成/版本未变(异常路径):只清标记,不打扰。
        // Install aborted or version unchanged: consume the marker quietly.
        log_debug!("[update-notice] pending marker consumed without version change");
        return;
    }
    let app = unsafe {
        let display = bundle_info_string("CFBundleDisplayName");
        if display.is_empty() {
            bundle_info_string("CFBundleName")
        } else {
            display
        }
    };
    log_debug!(
        "[update-notice] pending marker matched: {} -> {}, posting",
        from_version,
        current
    );
    post_update_installed(&app, &current);
}

/// 更新安装并重启后发一条系统通知(仅 bundled app;未授权时由系统弹一次性授权框)。
/// Post the "updated" notification (bundled apps only; the first run shows the one-time
/// system authorization prompt).
/// 定时(后台)检查发现新版本时通知用户;点击横幅由 delegate 跳转打开更新窗口。
/// Notify the user when a scheduled background check finds a new version; clicking the
/// banner opens the update window via the delegate.
pub(crate) fn post_update_available(app: &str, version: &str) {
    let bundle_id = unsafe { bundle_info_string("CFBundleIdentifier") };
    if bundle_id.is_empty() || version.is_empty() {
        return;
    }
    if objc2::runtime::AnyClass::get(&std::ffi::CString::new("UNUserNotificationCenter").unwrap())
        .is_none()
    {
        log_debug!("[update-notice] UNUserNotificationCenter class unavailable; skipping");
        return;
    }
    let title = tf("settings.update_available_notify_title", &[("app", app)]);
    let body = tf(
        "settings.update_available_notify_body",
        &[("version", version)],
    );
    unsafe {
        let center: *mut AnyObject =
            msg_send![class!(UNUserNotificationCenter), currentNotificationCenter];
        if center.is_null() {
            return;
        }
        let delegate = ensure_delegate_registered();
        if !delegate.is_null() {
            let _: () = msg_send![center, setDelegate: delegate];
        }
        *PENDING_NOTICE.lock().unwrap() = Some((title, body, ID_UPDATE_AVAILABLE.to_string()));
        type RequestAuthFn =
            unsafe extern "C" fn(*mut AnyObject, Sel, usize, *const AuthCompletionBlock);
        let send: RequestAuthFn = std::mem::transmute(objc_msgSend as *const ());
        send(
            center,
            sel!(requestAuthorizationWithOptions:completionHandler:),
            3,
            std::ptr::addr_of!(AUTH_BLOCK),
        );
    }
}

/// 更新安装并重启后发一条系统通知(仅 bundled app;未授权时由系统弹一次性授权框)。
/// Post the "updated" notification (bundled apps only; the first run shows the one-time
/// system authorization prompt).
pub(crate) fn post_update_installed(app: &str, version: &str) {
    // 无 bundle id(裸 cargo run)时 UNUserNotificationCenter 会抛异常,先守卫。
    // UNUserNotificationCenter raises when the bundle id is missing (raw cargo run);
    // guard before touching it.
    let bundle_id = unsafe { bundle_info_string("CFBundleIdentifier") };
    if bundle_id.is_empty() {
        log_debug!("[update-notice] no bundle id; skipping update notification");
        return;
    }
    // 框架守卫:类不在类表(框架未加载)时 objc2 的断言会 panic,先安全降级。
    // Framework guard: objc2's assertion panics when the class is absent (framework not
    // loaded); degrade gracefully instead.
    let uncenter_cname = std::ffi::CString::new("UNUserNotificationCenter").unwrap();
    if objc2::runtime::AnyClass::get(&uncenter_cname).is_none() {
        log_debug!("[update-notice] UNUserNotificationCenter class unavailable; skipping");
        return;
    }
    if version.is_empty() {
        return;
    }
    let title = tf("settings.update_installed_notify_title", &[("app", app)]);
    let body = tf(
        "settings.update_installed_notify_body",
        &[("version", version)],
    );

    unsafe {
        let center: *mut AnyObject =
            msg_send![class!(UNUserNotificationCenter), currentNotificationCenter];
        if center.is_null() {
            log_debug!("[update-notice] UNUserNotificationCenter unavailable");
            return;
        }
        let delegate = ensure_delegate_registered();
        if !delegate.is_null() {
            let _: () = msg_send![center, setDelegate: delegate];
        }
        *PENDING_NOTICE.lock().unwrap() = Some((title, body, ID_UPDATE_INSTALLED.to_string()));
        // alert|sound;系统只在第一次弹授权框,之后直接走回调。
        // alert|sound; the system prompts once, then the callback runs immediately.
        // (block 参数同上,走手工 msgSend。/ the block arg goes through the raw send too.)
        type RequestAuthFn =
            unsafe extern "C" fn(*mut AnyObject, Sel, usize, *const AuthCompletionBlock);
        let send: RequestAuthFn = std::mem::transmute(objc_msgSend as *const ());
        send(
            center,
            sel!(requestAuthorizationWithOptions:completionHandler:),
            3,
            std::ptr::addr_of!(AUTH_BLOCK),
        );
    }
}
