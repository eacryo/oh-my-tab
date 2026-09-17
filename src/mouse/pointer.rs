//! 指针设置:禁用系统鼠标加速(让光标 1:1 线性跟踪)+ 线性的跟踪速度数值。
//! 参考 LinearMouse DeviceManager.updatePointerSpeed:
//!
//! - macOS 14+ (Sonoma):设 HIDUseLinearScalingMouseAcceleration = 1
//! - 旧系统回退:设 HIDPointerAcceleration = -1(IOFixed 编码,值 × 65536;"acceleration and sensitivity are disabled")
//!
//! 跟踪速度写的是同一个 HIDPointerAcceleration 属性(IOFixed,值 × 65536),但**只在线性跟踪
//! 开启时才有意义**:线性缩放下这个属性就是跟踪速度本身;开关关闭时它是 macOS 加速曲线的
//! 强度,含义不同,因此那种情况下完全不写它(保持/恢复系统原值)。旧系统回退路径同样不支持
//! 该数值(那里 -1 已同时禁用加速与灵敏度)。
//!
//! 应用前保存每设备的原值,禁用配置或退出时恢复。
//!
//! Pointer settings: disable macOS pointer acceleration for 1:1 linear cursor tracking, plus
//! the tracking speed used in that linear mode. Mirrors LinearMouse's
//! DeviceManager.updatePointerSpeed:
//!
//! - macOS 14+ (Sonoma): set HIDUseLinearScalingMouseAcceleration = 1
//! - Legacy fallback: set HIDPointerAcceleration = -1 (IOFixed encoding, value × 65536;
//!   "-1 means acceleration and sensitivity are disabled")
//!
//! The tracking speed writes that same HIDPointerAcceleration property (IOFixed, value × 65536)
//! but **only means something while linear tracking is on**: under linear scaling the property is
//! the tracking speed itself, whereas with the switch off it is the strength of macOS's
//! acceleration curve -- a different meaning, so it is not written at all in that case (the
//! original system value is kept/restored). The legacy fallback path does not support the number
//! either (its -1 already disables acceleration and sensitivity).
//!
//! Original property values are saved before applying and restored when the config is
//! disabled or the app quits.

use crate::config::{CONFIG, MOUSE_ACCELERATION_MAX, MOUSE_ACCELERATION_MIN};
use crate::ffi::{make_nsstring, nsstring_to_rust, CFRelease};
use crate::mouse::ffi::*;
use crate::mouse::resolve;
use crate::{log_debug, log_info};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use std::ffi::c_void;
use std::sync::Mutex;

/// IOFixed 的缩放系数:属性值 × 65536(与 LinearMouse PointerKit 一致)。
/// The IOFixed scaling factor: property value × 65536 (same as LinearMouse's PointerKit).
const IOFIXED_SCALE: f64 = 65536.0;

/// 读不到设备现值时的兜底显示值(macOS 的系统默认加速值,LinearMouse 同款 fallback)。
/// Fallback shown when the device's live value can't be read (macOS's system default
/// acceleration, the same fallback LinearMouse uses).
pub(crate) const FALLBACK_ACCELERATION: f64 = 0.6875;

/// 指针加速 / 跟踪速度 -> IOFixed 原始值(四舍五入)。
/// 配置层已校验并 clamp,这里再兜一层(与 scrolling 的 line_count clamp 同一考虑)。
///
/// Pointer acceleration / tracking speed -> the raw IOFixed value (rounded). The config layer
/// already validates and clamps; this is a second safety net (same idea as scrolling's
/// line_count clamp).
pub(crate) fn acceleration_to_iofixed(acceleration: f64) -> i64 {
    let clamped = acceleration.clamp(MOUSE_ACCELERATION_MIN, MOUSE_ACCELERATION_MAX);
    (clamped * IOFIXED_SCALE).round() as i64
}

/// IOFixed 原始值 -> 指针加速 / 跟踪速度。
/// The raw IOFixed value -> pointer acceleration / tracking speed.
pub(crate) fn iofixed_to_acceleration(raw: i64) -> f64 {
    raw as f64 / IOFIXED_SCALE
}

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

/// 定位设备的加速属性键:优先现代键 HIDPointerAcceleration,不存在则回退 HIDMouseAcceleration。
/// Resolve a device's acceleration property key: prefer the modern HIDPointerAcceleration,
/// falling back to HIDMouseAcceleration.
unsafe fn accel_property_key(service: *mut c_void) -> &'static str {
    if prop_exists(service, KEY_POINTER_ACCEL) {
        KEY_POINTER_ACCEL
    } else {
        KEY_MOUSE_ACCEL
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
        log_info!("[pointer] failed to create IOHIDEventSystemClient");
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
    // 与 device.rs 同款竞态:新建 client 后立即 CopyServices 可能拿到空列表
    // (日志出现过"no devices found"但设备在场)。等 ~30ms 让异步匹配完成。
    // Same race as device.rs: CopyServices right after creating the client can return an
    // empty list (the log shows "no devices found" while the device was present). Wait
    // ~30ms for the asynchronous matching to settle.
    std::thread::sleep(std::time::Duration::from_millis(30));

    let services = IOHIDEventSystemClientCopyServices(client);
    if services.is_null() {
        log_debug!("[pointer] no services returned by IOHIDEventSystemClient");
        CFRelease(client as *const c_void);
        return;
    }

    // 遍历 services(CFArrayRef)。用 C 函数,不用 msg_send!(objectAtIndex:),
    // 避免 objc2 的类型编码校验 panic(方法返回 '@',Rust 声明 '^v')。
    // Iterate services (CFArrayRef) with C functions, not msg_send!(objectAtIndex:),
    // to avoid objc2's type-encoding panic (method returns '@', Rust declares '^v').
    let count = CFArrayGetCount(services);
    let mut saved: Vec<SavedProp> = Vec::new();
    // 枚举到的指针设备数:用于区分"一个设备都没找到"与"有设备但都不需要改动"。
    // Pointer devices enumerated: distinguishes "no device found at all" from "devices present
    // but none needs changing".
    let mut pointer_devices = 0usize;

    for i in 0..count {
        let service = CFArrayGetValueAtIndex(services, i) as *mut c_void;
        // 与 device.rs 的枚举判定保持一致:用 ConformsTo 而不是 PrimaryUsage 单值。
        // 有些真实鼠标(如 ATK A9 SE 这类 Nearlink/星闪设备)PrimaryUsage 被报成 Keyboard,
        // 白名单 {1,2,5} 会漏掉它们;ConformsTo 检查整个 DeviceUsagePairs。
        // Same pointer-mouse-trackpad test as device.rs enumeration: use ConformsTo, not the
        // PrimaryUsage scalar -- some real mice (e.g. ATK A9 SE Nearlink) report PrimaryUsage =
        // Keyboard, which a {1,2,5} whitelist would drop; ConformsTo inspects DeviceUsagePairs.
        let is_pointer = IOHIDServiceClientConformsTo(service, 1, USAGE_GD_POINTER as u32) != 0
            || IOHIDServiceClientConformsTo(service, 1, USAGE_GD_MOUSE as u32) != 0
            || IOHIDServiceClientConformsTo(service, 1, USAGE_GD_TRACKPAD as u32) != 0;
        if !is_pointer {
            continue;
        }
        pointer_devices += 1;
        let name = device_name(service);

        // 读取 VID/PID,解析该设备是否需要改动指针属性(per-device 配置)。
        // Read VID/PID and resolve whether this device needs any pointer property changed
        // (per-device config).
        let vid = prop_int(service, KEY_VENDOR_ID) as u32;
        let pid = prop_int(service, KEY_PRODUCT_ID) as u32;
        let resolved = resolve::resolve(Some((vid, pid)));
        // 未启用"禁用指针加速"时没有可做的改动:跟踪速度只在线性跟踪下才有意义,
        // 关闭时该属性是加速曲线强度,不属于这个设置的语义,一律不动(保留系统默认)。
        // With "disable pointer acceleration" off there is nothing to do: the tracking speed only
        // means something under linear tracking, and the property is otherwise the acceleration
        // curve's strength -- outside this setting's meaning, so it is left alone (system default
        // kept).
        if !resolved.disable_acceleration {
            log_debug!(
                "[pointer] {}: keeping acceleration (vid={:#x} pid={:#x})",
                name,
                vid,
                pid
            );
            continue;
        }

        // macOS 14+ (Sonoma):线性缩放开关。
        // macOS 14+ (Sonoma): the linear-scaling switch.
        if prop_exists(service, KEY_LINEAR_SCALING) {
            let original = copy_prop(service, KEY_LINEAR_SCALING).unwrap();
            let ok = set_prop_int(service, KEY_LINEAR_SCALING, 1);
            log_debug!(
                "[pointer] {}: linear scaling ON (original saved, ok={})",
                name,
                ok
            );
            saved.push(SavedProp {
                service,
                key: make_nsstring(KEY_LINEAR_SCALING),
                original: Some(original),
            });
        } else {
            // 旧系统回退:加速属性 = -1 (IOFixed:值 × 65536)。优先 HIDPointerAcceleration,
            // 不存在则用 HIDMouseAcceleration。
            // 该路径下 -1 同时禁用加速与灵敏度,不存在"线性 + 可调速度"的语义,
            // 因此配置里的跟踪速度在此不生效(与 LinearMouse 一致:旧系统不提供该控件)。
            //
            // Legacy fallback: acceleration = -1 (IOFixed: value × 65536). Prefers
            // HIDPointerAcceleration, else HIDMouseAcceleration. On this path -1 disables both
            // acceleration and sensitivity, so there is no "linear + adjustable speed" notion
            // and the configured tracking speed does not apply (same as LinearMouse, which
            // hides the control on older systems).
            let accel_key = accel_property_key(service);
            let original = copy_prop(service, accel_key);
            let ok = set_prop_int(service, accel_key, -IOFIXED_SCALE as i64);
            log_debug!(
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
            if resolved.acceleration.is_some() {
                log_debug!(
                    "[pointer] {}: tracking speed ignored (no linear-scaling property on this system)",
                    name
                );
            }
            continue;
        }

        // 跟踪速度:线性跟踪已开启,写入配置值。
        // Tracking speed: linear tracking is on, write the configured value.
        if let Some(acceleration) = resolved.acceleration {
            let accel_key = accel_property_key(service);
            let original = copy_prop(service, accel_key);
            let raw = acceleration_to_iofixed(acceleration);
            let ok = set_prop_int(service, accel_key, raw);
            log_debug!(
                "[pointer] {}: acceleration/tracking speed -> {} via {} (ok={})",
                name,
                acceleration,
                accel_key,
                ok
            );
            saved.push(SavedProp {
                service,
                key: make_nsstring(accel_key),
                original,
            });
        }
    }

    if saved.is_empty() {
        // 两种情况分开记录:枚举不到设备(常见于鼠标休眠/刚启动)与配置未要求改动。
        // Keep the two cases apart: nothing enumerated (common while the mouse sleeps or right
        // after launch) versus devices present but nothing configured.
        if pointer_devices == 0 {
            log_debug!("[pointer] no mouse/trackpad devices found; nothing applied");
        } else {
            log_debug!(
                "[pointer] {} device(s) found but none configured; nothing applied",
                pointer_devices
            );
        }
        CFRelease(services as *const c_void);
        CFRelease(client as *const c_void);
        return;
    }

    log_debug!(
        "[pointer] applied pointer settings to {} device(s).",
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
                    log_debug!("[pointer] restored original property (ok={})", ok);
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
    log_debug!("[pointer] restored original acceleration settings.");
}

/// 根据当前配置应用或恢复指针设置。
/// 启用时,disable() 内部按 per-device 解析结果决定哪些设备禁用加速;
/// 若所有已连接设备的解析结果都不要求禁用,则等同 restore(不改动任何设备)。
///
/// Apply or restore pointer settings based on the current config. When enabled, disable()
/// resolves per-device whether to disable acceleration; if no connected device asks for it,
/// this is equivalent to restore (no device is touched).
pub(crate) fn apply() {
    let enabled = CONFIG.read().map(|c| c.mouse.enabled).unwrap_or(false);
    if enabled {
        // 先 restore 已保存的原值,再 disable(让 per-device 决策从干净状态开始)。
        // Restore saved originals first, then disable (so per-device decisions start clean).
        restore();
        unsafe { disable() }
    } else {
        restore();
    }
}

/// 读取设备当前生效的指针加速 / 跟踪速度,供设置页在配置未设值时显示设备现值。
///
/// 负值(旧系统回退路径写的 -1 sentinel)视为"不可读",返回 None —— 让它不进入滑块的
/// 0..=40 区间;设备不在场或属性不存在同样是 None。
///
/// Read a device's current pointer acceleration / tracking speed, so the settings page can show
/// the device's live value when the config has none.
///
/// A negative value (the -1 sentinel written by the legacy fallback path) counts as unreadable
/// and yields None, keeping it out of the slider's 0..=40 range; a missing device or property
/// is None as well.
pub(crate) fn read_acceleration(device: crate::mouse::device::DeviceKey) -> Option<f64> {
    let raw = crate::mouse::device::device_int_property(device, KEY_POINTER_ACCEL)
        .or_else(|| crate::mouse::device::device_int_property(device, KEY_MOUSE_ACCEL))?;
    if raw < 0 {
        return None;
    }
    Some(iofixed_to_acceleration(raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iofixed_roundtrip() {
        // 0.6875 是 macOS 系统默认值,×65536 = 45056(整数,无精度损失)。
        // 0.6875 is macOS's default; ×65536 = 45056 (integral, no precision loss).
        assert_eq!(acceleration_to_iofixed(0.6875), 45056);
        assert_eq!(iofixed_to_acceleration(45056), 0.6875);
        assert_eq!(acceleration_to_iofixed(0.0), 0);
        assert_eq!(acceleration_to_iofixed(40.0), 2_621_440);
    }

    #[test]
    fn iofixed_rounds_and_clamps() {
        // 非整数倍:四舍五入到最近的 IOFixed。
        // Non-integral multiples round to the nearest IOFixed.
        assert_eq!(acceleration_to_iofixed(1.25), 81920);
        assert_eq!(acceleration_to_iofixed(0.0001), 7);
        // 越界值被 clamp 到 0..=40(配置层已校验,这里是兜底)。
        // Out-of-range values clamp to 0..=40 (the config layer validates; this is a backstop).
        assert_eq!(acceleration_to_iofixed(-5.0), 0);
        assert_eq!(acceleration_to_iofixed(1000.0), 2_621_440);
    }
}
