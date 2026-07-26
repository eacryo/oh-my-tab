//! 开机自启:通过 SMAppService(macOS 13+)把本应用注册为系统登录项。
//! TOML 的 [startup] launch_at_login 是唯一事实源;sync() 在启动 / Reload / 设置 OK 时把它同步到系统。
//! 仅在以 .app 方式启动时有效——SMAppService.mainApp 依赖主 bundle,cargo run 跑裸二进制时不可用
//! (会记一条 warn,不影响其它功能)。
//!
//! Launch at login via SMAppService (macOS 13+): registers the app as a system login item.
//! TOML's [startup] launch_at_login is the source of truth; sync() applies it on startup / reload /
//! settings OK. Only effective when launched as a .app — SMAppService.mainApp relies on the main
//! bundle, which is absent when running the raw binary via `cargo run` (it logs a warn, no other
//! impact).

use objc2::runtime::{AnyObject, Sel};
use objc2::{msg_send, sel};
use std::ffi::{c_char, CString};

use crate::{log_info, log_warn};

#[link(name = "ServiceManagement", kind = "framework")]
extern "C" {}

// SMAppServiceStatus: 1 = SMAppServiceStatusRegistered
const STATUS_REGISTERED: isize = 1;

/// 裸查 ObjC 类(按名字),绕过 objc2 class! 宏的校验。查不到返回 nil。
/// Look up an ObjC class by name via raw objc_getClass, bypassing objc2's `class!` macro
/// verification. Returns nil if not found.
unsafe fn cls_id(name: &str) -> *mut AnyObject {
    extern "C" {
        fn objc_getClass(name: *const c_char) -> *mut AnyObject;
    }
    objc_getClass(CString::new(name).unwrap().as_ptr())
}

/// 裸发一个无参消息(返回 id),绕过 objc2 msg_send! 校验。
/// Send a no-arg message (returning id) via raw objc_msgSend, bypassing objc2's msg_send!
/// verification.
unsafe fn send_id(recv: *mut AnyObject, cmd: Sel) -> *mut AnyObject {
    extern "C" {
        fn objc_msgSend();
    }
    type F = unsafe extern "C" fn(*mut AnyObject, Sel) -> *mut AnyObject;
    let f: F = std::mem::transmute(objc_msgSend as *const ());
    f(recv, cmd)
}

/// 是否以 .app bundle 方式运行(主 bundle 有 bundleIdentifier)。
/// SMAppService.mainApp 在无主 bundle(裸二进制 cargo run)时会抛 ObjC 异常,Rust 捕获不了会
/// abort,所以必须先探测、没 bundle 就根本不碰 mainApp。
/// Whether we're running as a .app bundle (main bundle has a bundleIdentifier).
/// SMAppService.mainApp throws an ObjC exception when there's no main bundle (raw-binary cargo run);
/// Rust can't catch it and would abort, so we must probe first and never touch mainApp without a bundle.
unsafe fn has_main_bundle() -> bool {
    let cls = cls_id("NSBundle");
    if cls.is_null() {
        return false;
    }
    let bundle = send_id(cls, sel!(mainBundle));
    if bundle.is_null() {
        return false;
    }
    let bid = send_id(bundle, sel!(bundleIdentifier));
    !bid.is_null()
}

/// 取 SMAppService.mainApp 实例(主 bundle 的登录项服务)。
/// 无主 bundle / 查不到类时返回 nil,让上层 is_enabled / set_registered 走 null 分支优雅降级
/// (记 warn,不影响其它功能)。
/// Get the SMAppService.mainApp instance (the main bundle's login-item service).
/// Returns nil when there's no main bundle or the class can't be found, so is_enabled /
/// set_registered take the null branch and degrade gracefully (log a warn, no impact on other features).
unsafe fn main_app() -> *mut AnyObject {
    if !has_main_bundle() {
        return std::ptr::null_mut();
    }
    let cls = cls_id("SMAppService");
    if cls.is_null() {
        return std::ptr::null_mut();
    }
    send_id(cls, sel!(mainApp))
}

/// 当前是否已注册为登录项(读 SMAppService.status == registered)。
/// Whether the app is currently registered as a login item (status == registered).
pub fn is_enabled() -> bool {
    unsafe {
        let service = main_app();
        if service.is_null() {
            return false;
        }
        let status: isize = msg_send![service, status];
        status == STATUS_REGISTERED
    }
}

/// 注册(enabled=true)或注销(false)登录项。幂等。返回是否成功(失败时 err 已释放)。
/// 用原生 objc_msgSend:NSError** 出参在 objc2 msg_send! 里编码棘手,沿用项目里
/// hex_to_cg_color 等的 raw-FFI 逃逸口。
/// Register (enabled=true) or unregister (false) the login item. Idempotent. Returns whether it
/// succeeded (the NSError, if any, is released). Uses raw objc_msgSend because the NSError**
/// out-parameter is awkward to encode through objc2's msg_send! — same escape hatch the project
/// already uses for hex_to_cg_color et al.
unsafe fn set_registered(enabled: bool) -> bool {
    let service = main_app();
    if service.is_null() {
        return false;
    }
    extern "C" {
        fn objc_msgSend();
    }
    let sel = if enabled {
        sel!(registerWithError:)
    } else {
        sel!(unregisterWithError:)
    };
    type F = unsafe extern "C" fn(*mut AnyObject, Sel, *mut *mut AnyObject) -> bool;
    let f: F = std::mem::transmute(objc_msgSend as *const ());
    let mut err: *mut AnyObject = std::ptr::null_mut();
    let ok = f(service, sel, &mut err);
    if !err.is_null() {
        let _: () = msg_send![err, release];
    }
    ok
}

/// 按 enabled 同步系统登录项状态,并记录结果。
/// Sync the system login-item state to match `enabled`, logging the outcome.
pub fn sync(enabled: bool) {
    let ok = unsafe { set_registered(enabled) };
    if ok {
        log_info!(
            "autostart: {} (status={})",
            if enabled { "registered" } else { "unregistered" },
            if is_enabled() { "enabled" } else { "disabled" },
        );
    } else {
        log_warn!(
            "autostart: {} failed — run as .app? (ad-hoc signed apps may need a one-time approval \
             in System Settings > Login Items)",
            if enabled { "register" } else { "unregister" },
        );
    }
}
