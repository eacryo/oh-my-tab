//! 指针设置:禁用系统鼠标加速,让光标 1:1 线性跟踪(对应 docs/mouse-architecture.md §4.6)。
//! 参考 LinearMouse DeviceManager.updatePointerSpeed:
//!
//! - macOS 14+ (Sonoma):设 HIDUseLinearScalingMouseAcceleration = 1
//! - 旧系统回退:设 HIDPointerAcceleration = -1(IOFixed 编码,值 × 65536;"acceleration and sensitivity are disabled")
//!
//! 应用前保存每设备的原值,禁用配置或退出时恢复。
//!
//! Pointer settings: disable macOS pointer acceleration for 1:1 linear cursor tracking.
//! Mirrors LinearMouse's DeviceManager.updatePointerSpeed:
//!
//! - macOS 14+ (Sonoma): set HIDUseLinearScalingMouseAcceleration = 1
//! - Legacy fallback: set HIDPointerAcceleration = -1 (IOFixed encoding, value × 65536;
//!   "-1 means acceleration and sensitivity are disabled")
//!
//! Original property values are saved before applying and restored when the config is
//! disabled or the app quits.

use crate::config::CONFIG;
use crate::ffi::{make_nsstring, nsstring_to_rust, CFRelease};
use crate::mouse::ffi::*;
use crate::{log_info, log_warn};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use std::ffi::c_void;
use std::sync::Mutex;

/// 一个设备上被改动的属性(用于恢复)。
/// A property modified on one device (for restore).
struct SavedProp {
    /// IOHIDServiceClientRef(借用自 services 数组,由 PointerState 保活)。
    /// IOHIDServiceClientRef (borrowed from the services array; kept alive by PointerState).
    service: *mut c_void,
    /// NSString 属性键(+1,restore 时 release)。
    /// NSString property key (+1, released on restore).
    key: *mut AnyObject,
    /// 原值(+1,restore 时 set 回再 release);None = 属性原本不存在。
    /// Original value (+1, set back then released on restore); None = property didn't exist.
    original: Option<*mut c_void>,
}

/// 已应用的指针状态:持有 event system client 与 services 数组(保活 service client),外加 saved 列表。
/// Applied pointer state: holds the event system client + services array (keeping service
/// clients alive), plus the saved-properties list.
struct PointerState {
    /// IOHIDEventSystemClientRef (+1)
    client: *mut c_void,
    /// CFArrayRef of IOHIDServiceClient (+1)
    services: *mut c_void,
    saved: Vec<SavedProp>,
}

// 裸指针的 Send/Sync(与 SettingsUi/ObjPtr 同一模式:Mutex 守卫所有访问)。
// Raw pointers' Send/Sync (same pattern as SettingsUi/ObjPtr: Mutex guards all access).
unsafe impl Send for PointerState {}
unsafe impl Sync for PointerState {}

static POINTER_STATE: Mutex<Option<PointerState>> = Mutex::new(None);

/// 读取 IOHIDServiceClient 的整数属性(CFNumber, toll-free NSNumber)。
/// Read an integer property from an IOHIDServiceClient (CFNumber, toll-free NSNumber).
unsafe fn prop_int(service: *mut c_void, key: &str) -> i64 {
    let k = make_nsstring(key);
    let v = IOHIDServiceClientCopyProperty(service, k as *const c_void);
    CFRelease(k as *const c_void);
    if v.is_null() {
        return 0;
    }
    let i: i64 = msg_send![v as *mut AnyObject, longLongValue];
    CFRelease(v as *const c_void);
    i
}

/// 拷贝属性(+1),不存在返回 None。
/// Copy a property (+1); None if absent.
unsafe fn copy_prop(service: *mut c_void, key: &str) -> Option<*mut c_void> {
    let k = make_nsstring(key);
    let v = IOHIDServiceClientCopyProperty(service, k as *const c_void);
    CFRelease(k as *const c_void);
    if v.is_null() {
        None
    } else {
        Some(v)
    }
}

/// 属性是否存在。
/// Whether the property exists.
unsafe fn prop_exists(service: *mut c_void, key: &str) -> bool {
    match copy_prop(service, key) {
        Some(v) => {
            CFRelease(v as *const c_void);
            true
        }
        None => false,
    }
}

/// 设置整数属性。
/// Set an integer property.
unsafe fn set_prop_int(service: *mut c_void, key: &str, value: i64) -> bool {
    let k = make_nsstring(key);
    let n: *mut AnyObject = msg_send![class!(NSNumber), numberWithLongLong: value];
    let ok = IOHIDServiceClientSetProperty(service, k as *const c_void, n as *mut c_void);
    CFRelease(k as *const c_void);
    ok
}

/// 读取设备产品名(日志用)。
/// Read the device product name (for logs).
unsafe fn device_name(service: *mut c_void) -> String {
    match copy_prop(service, KEY_PRODUCT) {
        Some(v) => {
            let name = nsstring_to_rust(v as *mut AnyObject);
            CFRelease(v as *const c_void);
            name
        }
        None => "unknown".into(),
    }
}

/// 禁用鼠标加速:枚举鼠标/触控板设备,保存原值并设线性开关。
/// Disable pointer acceleration: enumerate mouse/trackpad devices, save originals, set the
/// linear-scaling switch.
unsafe fn disable() {
    let mut guard = POINTER_STATE.lock().unwrap();
    if guard.is_some() {
        return; // 已应用 / already applied
    }

    // 创建 event system client,匹配 Generic Desktop 页(鼠标/键盘/触控板等,后续按 usage 过滤)。
    // Create the event system client, matching the Generic Desktop page (mice/keyboards/trackpads;
    // filtered by usage below).
    let client = IOHIDEventSystemClientCreate(std::ptr::null());
    if client.is_null() {
        log_warn!("[pointer] failed to create IOHIDEventSystemClient");
        return;
    }
    let page_key = make_nsstring(KEY_PRIMARY_USAGE_PAGE);
    let page_val: *mut AnyObject =
        msg_send![class!(NSNumber), numberWithInt: USAGE_PAGE_GENERIC_DESKTOP as i32];
    // dictionaryWithObject:/arrayWithObject: 返回 autoreleased(+0),不能手动 release,交给自动释放池。
    // dictionaryWithObject:/arrayWithObject: return autoreleased (+0); must not be released
    // manually - the autorelease pool handles them.
    let dict: *mut AnyObject =
        msg_send![class!(NSDictionary), dictionaryWithObject: page_val, forKey: page_key];
    let arr: *mut AnyObject = msg_send![class!(NSArray), arrayWithObject: dict];
    IOHIDEventSystemClientSetMatchingMultiple(client, arr as *const c_void);
    CFRelease(page_key as *const c_void);

    let services = IOHIDEventSystemClientCopyServices(client);
    if services.is_null() {
        log_warn!("[pointer] no services returned by IOHIDEventSystemClient");
        CFRelease(client as *const c_void);
        return;
    }

    // 遍历 services(CFArrayRef)。用 C 函数,不用 msg_send!(objectAtIndex:),
    // 避免 objc2 的类型编码校验 panic(方法返回 '@',Rust 声明 '^v')。
    // Iterate services (CFArrayRef) with C functions, not msg_send!(objectAtIndex:),
    // to avoid objc2's type-encoding panic (method returns '@', Rust declares '^v').
    let count = CFArrayGetCount(services);
    let mut saved: Vec<SavedProp> = Vec::new();

    for i in 0..count {
        let service = CFArrayGetValueAtIndex(services, i) as *mut c_void;
        let page = prop_int(service, KEY_PRIMARY_USAGE_PAGE);
        let usage = prop_int(service, KEY_PRIMARY_USAGE);
        // 只处理指针/鼠标/触控板,跳过键盘等其他 Generic Desktop 设备。
        // Only handle pointer/mouse/trackpad; skip keyboards and other Generic Desktop devices.
        if page != USAGE_PAGE_GENERIC_DESKTOP
            || !(usage == USAGE_GD_POINTER
                || usage == USAGE_GD_MOUSE
                || usage == USAGE_GD_TRACKPAD)
        {
            continue;
        }
        let name = device_name(service);

        // macOS 14+ (Sonoma):线性缩放开关。
        // macOS 14+ (Sonoma): the linear-scaling switch.
        if prop_exists(service, KEY_LINEAR_SCALING) {
            let original = copy_prop(service, KEY_LINEAR_SCALING).unwrap();
            let ok = set_prop_int(service, KEY_LINEAR_SCALING, 1);
            log_info!(
                "[pointer] {}: linear scaling ON (original saved, ok={})",
                name,
                ok
            );
            saved.push(SavedProp {
                service,
                key: make_nsstring(KEY_LINEAR_SCALING),
                original: Some(original),
            });
            continue;
        }

        // 旧系统回退:加速属性 = -1 (IOFixed:值 × 65536)。优先 HIDPointerAcceleration,
        // 不存在则用 HIDMouseAcceleration。
        // Legacy fallback: acceleration = -1 (IOFixed: value × 65536). Prefer
        // HIDPointerAcceleration, else HIDMouseAcceleration.
        let accel_key = if prop_exists(service, KEY_POINTER_ACCEL) {
            KEY_POINTER_ACCEL
        } else {
            KEY_MOUSE_ACCEL
        };
        let original = copy_prop(service, accel_key);
        let ok = set_prop_int(service, accel_key, -65536);
        log_info!(
            "[pointer] {}: acceleration -> -1 via {} (ok={})",
            name,
            accel_key,
            ok
        );
        saved.push(SavedProp {
            service,
            key: make_nsstring(accel_key),
            original,
        });
    }

    if saved.is_empty() {
        log_warn!("[pointer] no mouse/trackpad devices found; nothing applied");
        CFRelease(services as *const c_void);
        CFRelease(client as *const c_void);
        return;
    }

    log_info!(
        "[pointer] disabled acceleration on {} device(s).",
        saved.len()
    );
    *guard = Some(PointerState {
        client,
        services,
        saved,
    });
}

/// 恢复原始加速设置(禁用配置、Reload 或退出时调用)。
/// Restore the original acceleration settings (called on config disable, reload, or quit).
pub(crate) fn restore() {
    let mut guard = POINTER_STATE.lock().unwrap();
    let Some(state) = guard.take() else {
        return;
    };
    unsafe {
        for sp in state.saved {
            let key_cf = sp.key as *const c_void;
            match sp.original {
                Some(orig) => {
                    let ok = IOHIDServiceClientSetProperty(sp.service, key_cf, orig);
                    log_info!("[pointer] restored original property (ok={})", ok);
                    CFRelease(orig as *const c_void);
                }
                None => {
                    // 原值不存在:设 0(等效关闭我们设置的开关);极少见的旧系统边界情况。
                    // Original didn't exist: set 0 (equivalent to disabling what we set);
                    // a rare legacy edge case.
                    let zero: *mut AnyObject = msg_send![class!(NSNumber), numberWithInt: 0];
                    IOHIDServiceClientSetProperty(sp.service, key_cf, zero as *mut c_void);
                }
            }
            CFRelease(key_cf);
        }
        CFRelease(state.services as *const c_void);
        CFRelease(state.client as *const c_void);
    }
    log_info!("[pointer] restored original acceleration settings.");
}

/// 根据当前配置应用或恢复指针设置。
/// Apply or restore pointer settings based on the current config.
pub(crate) fn apply() {
    let want = CONFIG
        .read()
        .map(|c| c.mouse.enabled && c.mouse.pointer.disable_acceleration)
        .unwrap_or(false);
    if want {
        unsafe { disable() }
    } else {
        restore();
    }
}
