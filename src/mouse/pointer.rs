//! 指针设置:禁用系统鼠标加速(让光标 1:1 线性跟踪)+ 线性的跟踪速度数值。
//! 参考 LinearMouse DeviceManager.updatePointerSpeed:
//!
//! - macOS 14+ (Sonoma):设 HIDUseLinearScalingMouseAcceleration = 1
//! - 旧系统回退:设 HIDPointerAcceleration = -1(IOFixed 编码,值 × 65536;"acceleration and sensitivity are disabled")
//!
//! **加速属性键必须取设备声明的那个**(`HIDPointerAccelerationType`,通常是
//! HIDMouseAcceleration 或 HIDTrackpadAcceleration):macOS 按声明取值/写值,写声明之外的键
//! 不会生效——旧版本写死 HIDPointerAcceleration,在声明 HIDMouseAcceleration 的鼠标上
//! 完全是空操作("跟踪速度没生效"的根因)。选择逻辑见 `choose_accel_key`。
//!
//! 跟踪速度写的是同一个加速属性(IOFixed,值 × 65536),但**只在线性跟踪开启时才有意义**:
//! 线性缩放下这个属性就是跟踪速度本身;开关关闭时它是 macOS 加速曲线的强度,含义不同,因此
//! 那种情况下完全不写它(保持/恢复系统原值)。旧系统回退路径同样不支持该数值(那里 -1 已
//! 同时禁用加速与灵敏度)。
//!
//! 取值区间 [0, 40] ∪ {-1}(与 LinearMouse PointerKit 一致):**-1 才是"禁用加速与灵敏度"
//! 的哨兵值,0 是正常区间的最低端(最慢)**。0 会真实生效,所以它不是"未设置"——
//! `None`(配置未设置)才表示不动设备现值。
//!
//! 应用前保存每设备的原值,禁用配置或退出时恢复;**我们自己创建出来的属性在恢复时删除**
//! (而不是写 0 —— 那会凭空造出一个假的"最慢"状态,见 `restore`)。
//!
//! Pointer settings: disable macOS pointer acceleration for 1:1 linear cursor tracking, plus
//! the tracking speed used in that linear mode. Mirrors LinearMouse's
//! DeviceManager.updatePointerSpeed:
//!
//! - macOS 14+ (Sonoma): set HIDUseLinearScalingMouseAcceleration = 1
//! - Legacy fallback: set HIDPointerAcceleration = -1 (IOFixed encoding, value × 65536;
//!   "-1 means acceleration and sensitivity are disabled")
//!
//! **The acceleration property key must be the one the device declares**
//! (`HIDPointerAccelerationType`, usually HIDMouseAcceleration or HIDTrackpadAcceleration):
//! macOS reads/writes that key, and a write to any other key has no effect -- the old version
//! hard-coded HIDPointerAcceleration, which was a no-op on a mouse declaring HIDMouseAcceleration
//! (the root cause of "tracking speed has no effect"). See `choose_accel_key`.
//!
//! The tracking speed writes that same property (IOFixed, value × 65536) but **only means
//! something while linear tracking is on**: under linear scaling the property is the tracking
//! speed itself, whereas with the switch off it is the strength of macOS's acceleration curve --
//! a different meaning, so a configured value is never written in that case; the system value is
//! written back instead. The legacy fallback path does not support the number either (its -1
//! already disables acceleration and sensitivity).
//!
//! Value range [0, 40] ∪ {-1} (same as LinearMouse's PointerKit): **-1 is the "acceleration and
//! sensitivity disabled" sentinel while 0 is the bottom of the normal range (slowest)**. 0 does
//! take effect, so it is not "unset" -- only `None` (no configured value) writes the system value
//! back instead of a configured one.
//!
//! **Unset means "write the macOS system value back", not "leave the device alone"** (the same
//! semantics as LinearMouse's `restorePointerAcceleration()` / `disablePointerAcceleration = false`):
//! any property this feature could have written is actively reset to the system value whenever the
//! config has nothing to say about that device. That is what makes values left behind by a
//! previous run -- including one that crashed -- disappear on the next apply instead of sticking
//! to the device forever.
//!
//! Original property values are saved before applying and restored when the config is disabled
//! or the app quits; a property **we created** is removed on restore (instead of being written
//! as 0, which fabricates a fake "slowest" state -- see `restore`).

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

/// 默认跟踪速度:**1.00**,即 macOS 给鼠标键(HIDMouseAcceleration)的出厂默认值——功能上线
/// 前不写该属性,手感就是这个值(触发路径:滑杆初值、双击恢复默认、读不到设备/系统值时的兜底)。
///
/// 注意与 LinearMouse 的 `fallbackPointerAcceleration = 0.6875` 区分:那是它"连系统值都读不到"
/// 时的最后兜底,数值正好等于**触控板/指针键**(HIDPointerAcceleration/HIDTrackpadAcceleration)
/// 的默认 45056;拿它当鼠标的默认会偏慢约 31%。
/// The default tracking speed: **1.00**, macOS's factory default for the mouse key
/// (HIDMouseAcceleration) -- what the pointer felt like before this setting existed (it is used as
/// the slider's initial value, the double-click reset target, and the last-resort fallback when
/// neither the device nor the system value can be read).
///
/// Distinct from LinearMouse's `fallbackPointerAcceleration = 0.6875`: that is its last-resort
/// constant and equals the *trackpad/pointer* key default (45056); using it as a mouse default is
/// ~31% slower than the factory feel.
pub(crate) const FALLBACK_ACCELERATION: f64 = 1.0;

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
    /// 我们写入前该属性是否已存在:false = 这个键是我们创建出来的,恢复时应删除它。
    /// 注意这里**不保存原值**:恢复写回的是"现场读到的 macOS 系统值"(见 restore),
    /// 对齐 LinearMouse;快照崩溃后就没了,而系统值永远可读。
    /// Whether the property existed before we wrote it: false = we created the key, so restore
    /// should remove it. Note the original **value** is deliberately not kept: restore writes the
    /// live macOS system value (see `restore`), same as LinearMouse -- a snapshot dies with the
    /// process, the system value is always readable.
    existed_before: bool,
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
    let n = nsnumber(value);
    let ok = IOHIDServiceClientSetProperty(service, k as *const c_void, n as *mut c_void);
    CFRelease(k as *const c_void);
    ok
}

/// NSNumber(长整型)。CFNumber 与 NSNumber toll-free 互通,IOHIDServiceClientSetProperty 两处都用它。
/// An NSNumber (long long). CFNumber and NSNumber are toll-free bridged; used for both
/// IOHIDServiceClientSetProperty and the system-parameter writes.
unsafe fn nsnumber(value: i64) -> *mut AnyObject {
    msg_send![class!(NSNumber), numberWithLongLong: value]
}

/// 读 HID 系统参数(IOHIDSystem / kIOHIDParamConnectType 连接)——即「系统设置」里的值。
/// LinearMouse DeviceManager.getSystemProperty 同款链路;read-only,失败返回 None。
///
/// Read an HID system parameter (the IOHIDSystem kIOHIDParamConnectType connection) -- i.e. the
/// value System Settings holds. Same chain as LinearMouse's DeviceManager.getSystemProperty;
/// read-only, None on failure.
unsafe fn system_hid_param(key: &str) -> Option<i64> {
    let path = std::ffi::CString::new(IOSERVICE_IOHID_SYSTEM_PATH).ok()?;
    let service = IORegistryEntryFromPath(0, path.as_ptr());
    if service == 0 {
        return None;
    }
    let mut handle: u32 = 0;
    let kr = IOServiceOpen(
        service,
        mach_task_self(),
        K_IOHID_PARAM_CONNECT_TYPE,
        &mut handle,
    );
    IOObjectRelease(service);
    if kr != KERN_SUCCESS || handle == 0 {
        return None;
    }
    let k = make_nsstring(key);
    let mut out: *mut c_void = std::ptr::null_mut();
    let kr = IOHIDCopyCFTypeParameter(handle, k as *const c_void, &mut out);
    CFRelease(k as *const c_void);
    IOServiceClose(handle);
    if kr != KERN_SUCCESS || out.is_null() {
        return None;
    }
    // CFNumber/CFBoolean 都是 NSNumber 家族,longLongValue 通用。
    // CFNumber/CFBoolean both belong to the NSNumber family; longLongValue works for both.
    let v: i64 = msg_send![out as *mut AnyObject, longLongValue];
    CFRelease(out);
    Some(v)
}

/// 系统级加速值(原始 IOFixed):按设备声明的键读系统值(LinearMouse 读与设备同键的系统值),
/// 读不到再退到鼠标键;都读不到用 macOS 默认 0.6875(LinearMouse 同款兜底)。
///
/// The system-level acceleration (raw IOFixed): read the system value for the device's declared
/// key (LinearMouse reads the system value of the same key the device uses), fall back to the
/// mouse key, then to macOS's default 0.6875 (LinearMouse's fallback).
unsafe fn system_acceleration_raw(accel_key: &str) -> i64 {
    system_hid_param(accel_key)
        .or_else(|| system_hid_param(KEY_MOUSE_ACCEL))
        .unwrap_or_else(|| acceleration_to_iofixed(FALLBACK_ACCELERATION))
}

/// 系统级线性缩放开关(0/1)。读不到回退 0(系统默认:加速开启)。
/// The system-level linear-scaling switch (0/1); falls back to 0 (acceleration on) when unreadable.
unsafe fn system_linear_flag() -> i64 {
    if system_hid_param(KEY_LINEAR_SCALING).unwrap_or(0) != 0 {
        1
    } else {
        0
    }
}

/// 单台设备的目标属性值(纯计算,便于单测;均为原始 IOFixed/整数)。
/// The desired property values for one device (pure, for unit tests; raw IOFixed/integers).
#[derive(Debug, PartialEq)]
struct DesiredPointerValues {
    /// HIDUseLinearScalingMouseAcceleration 的目标值。
    /// Target value for HIDUseLinearScalingMouseAcceleration.
    linear: i64,
    /// 设备声明的加速键的目标值。
    /// Target value for the device's declared acceleration key.
    accel: i64,
    /// 配置了跟踪速度但因未启用线性模式而被忽略(仅用于日志说明)。
    /// A configured tracking speed was ignored because linear mode is off (for logging only).
    accel_ignored: bool,
}

/// 计算目标值(对齐 LinearMouse 的 DeviceManager.updatePointerSpeed):
/// - 要求禁用加速:线性开关 = 1;跟踪速度 = 配置值,未配置则写回系统值;
/// - 否则:线性开关与加速值**都写回系统值**。LinearMouse 对未配置项就是"写回系统值"
///   (`restorePointerAcceleration()` / `disablePointerAcceleration = false`),不是"不动设备":
///   这样上一轮(甚至崩溃前)残留在设备上的值会在下次应用时被清掉。
///   注意未启用线性模式时即使配置了跟踪速度也不写该数值——此时加速属性是 macOS 加速曲线的
///   强度,语义不同(见模块头),写系统值保持"该设置未生效"的诚实状态。
///
/// Compute the target values (same as LinearMouse's DeviceManager.updatePointerSpeed):
/// - acceleration disabled: linear switch = 1; tracking speed = configured value, or the system
///   value when unconfigured;
/// - otherwise: **both the linear switch and the acceleration are written back to the system
///   values**. LinearMouse treats "unset" exactly that way (`restorePointerAcceleration()` /
///   `disablePointerAcceleration = false`) rather than leaving the device alone, so values left by
///   a previous run (even a crashed one) get cleared on the next apply. Note a configured tracking
///   speed is not written while linear mode is off -- the property is then the strength of macOS's
///   acceleration curve, a different meaning (see the module docs) -- writing the system value
///   keeps the honest "that setting has no effect" state.
fn desired_pointer_values(
    disable_acceleration: bool,
    configured_accel: Option<f64>,
    system_linear: i64,
    system_accel_raw: i64,
) -> DesiredPointerValues {
    if disable_acceleration {
        DesiredPointerValues {
            linear: 1,
            accel: configured_accel
                .map(acceleration_to_iofixed)
                .unwrap_or(system_accel_raw),
            accel_ignored: false,
        }
    } else {
        DesiredPointerValues {
            linear: system_linear,
            accel: system_accel_raw,
            accel_ignored: configured_accel.is_some(),
        }
    }
}

/// 写入一个属性并记录(供恢复用)。返回是否写成功。
/// Write one property and record it (for restore). Returns whether the write succeeded.
unsafe fn write_and_record(
    service: *mut c_void,
    key: &str,
    value: i64,
    label: &str,
    device: &str,
    saved: &mut Vec<SavedProp>,
) -> bool {
    let existed_before = prop_exists(service, key);
    let ok = set_prop_int(service, key, value);
    log_debug!("[pointer] {}: {} via {} (ok={})", device, label, key, ok);
    saved.push(SavedProp {
        service,
        key: make_nsstring(key),
        existed_before,
    });
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

/// 读取字符串属性(NSString);不存在/非字符串返回 None。
/// Read a string property (NSString); None when absent or not a string.
unsafe fn copy_prop_string(service: *mut c_void, key: &str) -> Option<String> {
    let v = copy_prop(service, key)?;
    let s = nsstring_to_rust(v as *mut AnyObject);
    CFRelease(v as *const c_void);
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// 选择加速属性键(纯函数,便于单测):设备声明的键 > LinearMouse 式猜测。
///
/// 平台按设备自己的 `HIDPointerAccelerationType` 声明取值/写值,声明之外的键写了也不生效:
/// 实测 MCHOSE G3 V2 声明 `HIDMouseAcceleration`,而旧版本写死的 `HIDPointerAcceleration`
/// 完全不起作用("跟踪速度没生效"的根因)。因此:
/// 1. 设备声明了就用它(LinearMouse PointerDevice.pointerAccelerationType 同款);
/// 2. 没声明时才猜:存在 `HIDPointerAcceleration` 就用它;
/// 3. 否则回退 `HIDMouseAcceleration`(鼠标的惯例键)。
///
/// Pick the acceleration property key (pure, for unit tests): the device-declared key wins,
/// then the LinearMouse-style guess.
///
/// macOS reads/writes the key named by the device's own `HIDPointerAccelerationType`
/// declaration; a write to any other key has no effect (measured on a MCHOSE G3 V2: it declares
/// `HIDMouseAcceleration` and the previously hard-coded `HIDPointerAcceleration` write did
/// nothing -- the root cause of "tracking speed has no effect"). So:
/// 1. use the declared key when present (same as LinearMouse's
///    `PointerDevice.pointerAccelerationType`);
/// 2. only guess when nothing is declared: `HIDPointerAcceleration` when it exists;
/// 3. otherwise fall back to `HIDMouseAcceleration` (the mouse convention).
fn choose_accel_key(declared: Option<&str>, pointer_accel_exists: bool) -> String {
    if let Some(key) = declared.filter(|k| !k.is_empty()) {
        return key.to_string();
    }
    if pointer_accel_exists {
        KEY_POINTER_ACCEL.to_string()
    } else {
        KEY_MOUSE_ACCEL.to_string()
    }
}

/// 定位设备的加速属性键:见 `choose_accel_key`。
/// Resolve a device's acceleration property key; see `choose_accel_key`.
unsafe fn accel_property_key(service: *mut c_void) -> String {
    choose_accel_key(
        copy_prop_string(service, KEY_ACCEL_TYPE).as_deref(),
        prop_exists(service, KEY_POINTER_ACCEL),
    )
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
        let accel_key = accel_property_key(service);
        let desired = desired_pointer_values(
            resolved.disable_acceleration,
            resolved.acceleration,
            system_linear_flag(),
            system_acceleration_raw(&accel_key),
        );

        if desired.accel_ignored {
            log_info!(
                "[pointer] {}: tracking speed ignored while linear tracking is off (vid={:#x} pid={:#x})",
                name,
                vid,
                pid
            );
        }

        // macOS 14+ (Sonoma) 有线性缩放开关;老系统只有加速属性(下面走 -1 回退)。
        // macOS 14+ (Sonoma) has the linear-scaling switch; older systems only expose the
        // acceleration property (the -1 fallback below).
        if prop_exists(service, KEY_LINEAR_SCALING) {
            // 线性开关:要求禁用 -> 1;否则写回系统值(通常是 0)——把上一轮/上个版本遗留在
            // 设备上的 1 清掉(对齐 LinearMouse 的 disablePointerAcceleration = false)。
            // Linear switch: 1 when disabling is requested, otherwise the system value (usually 0),
            // which clears a 1 left behind by a previous run/version (same as LinearMouse's
            // disablePointerAcceleration = false).
            write_and_record(
                service,
                KEY_LINEAR_SCALING,
                desired.linear,
                &format!("linear scaling -> {}", desired.linear),
                &name,
                &mut saved,
            );
            // 加速 / 跟踪速度:线性模式下 = 配置值(未配置 -> 系统值);非线模式 -> 系统值。
            // Acceleration / tracking speed: in linear mode the configured value (unconfigured ->
            // the system value); with linear mode off, the system value.
            if resolved.disable_acceleration && resolved.acceleration == Some(0.0) {
                // 0 = 平台区间 [0,40] 的最低端(最慢),不是 -1 那种"禁用"哨兵;它真的会生效,
                // 所以打印时把单位语义一并写清楚,便于排查"指针几乎不动"的反馈。
                // 0 is the bottom of the platform's [0,40] range (slowest), not the -1 "disabled"
                // sentinel; it does take effect, so the log spells the meaning out for
                // "pointer barely moves" reports.
                log_info!(
                    "[pointer] {}: tracking speed 0 = slowest setting in linear mode",
                    name
                );
            }
            // 人读值:线性模式下是配置的跟踪速度,否则是"系统值"(跟踪速度未生效)。
            // Human-readable value: the configured tracking speed in linear mode, else "system
            // value" (the tracking speed has no effect).
            let accel_desc = match (resolved.disable_acceleration, resolved.acceleration) {
                (true, Some(v)) => format!("tracking speed {v}"),
                _ => "acceleration -> system value".to_string(),
            };
            write_and_record(
                service,
                &accel_key,
                desired.accel,
                &format!("{accel_desc} (raw {})", desired.accel),
                &name,
                &mut saved,
            );
        } else {
            // 旧系统回退:要求禁用 -> 加速属性 = -1(IOFixed:值 × 65536);否则写回系统值。
            // 该路径下 -1 同时禁用加速与灵敏度,不存在"线性 + 可调速度"的语义,
            // 因此配置里的跟踪速度在此不生效(与 LinearMouse 一致:旧系统不提供该控件)。
            //
            // Legacy fallback: -1 (IOFixed: value × 65536) when disabling is requested; otherwise
            // the system value. The key comes from the device's declaration (see
            // `choose_accel_key`). On this path -1 disables both acceleration and sensitivity, so
            // there is no "linear + adjustable speed" notion and a configured tracking speed does
            // not apply (same as LinearMouse, which hides the control on older systems).
            let disabling = resolved.disable_acceleration;
            let value = if disabling {
                -IOFIXED_SCALE as i64
            } else {
                desired.accel
            };
            let label = if disabling {
                format!("acceleration -> -1 (raw {value}, legacy)")
            } else {
                format!("acceleration -> system value (raw {value}, legacy)")
            };
            write_and_record(service, &accel_key, value, &label, &name, &mut saved);
            if disabling && resolved.acceleration.is_some() {
                log_debug!(
                    "[pointer] {}: tracking speed ignored (no linear-scaling property on this system)",
                    name
                );
            }
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

/// 恢复系统指针设置(禁用配置、Reload 或退出时调用):把改动过的属性**写回现场读到的
/// macOS 系统值**,而不是进程内快照——对齐 LinearMouse 的
/// `restorePointerAcceleration()`:系统值永远可读,而快照会随进程(尤其是崩溃)消失;
/// 写回系统值还能顺手清掉设备上遗留的旧值。我们创建出来的键则删除(回到"从未存在")。
///
/// Restore the system pointer settings (called on config disable, reload, or quit): write the
/// **live macOS system values** back instead of an in-process snapshot -- same as LinearMouse's
/// `restorePointerAcceleration()`: the system value is always readable while a snapshot dies with
/// the process (crashes included), and writing it back also clears stale values on the device.
/// Keys we created are removed (back to "never existed").
pub(crate) fn restore() {
    let mut guard = POINTER_STATE.lock().unwrap();
    let Some(state) = guard.take() else {
        return;
    };
    unsafe {
        for sp in state.saved {
            let key_cf = sp.key as *const c_void;
            let key = nsstring_to_rust(sp.key);
            if sp.existed_before {
                let value = if key == KEY_LINEAR_SCALING {
                    system_linear_flag()
                } else {
                    system_acceleration_raw(&key)
                };
                let n = nsnumber(value);
                let ok = IOHIDServiceClientSetProperty(sp.service, key_cf, n as *mut c_void);
                log_debug!(
                    "[pointer] restored system value for {} (value={}, ok={})",
                    key,
                    value,
                    ok
                );
            } else {
                // 这个键是我们首次创建出来的 -> 删除属性,而不是写 0。
                // 写 0 会凭空造出一个"最慢/无加速"的假状态:旧版本对 HIDPointerAcceleration
                // 就是写 0,导致设备上留下一个从未生效过的 0,又反过来被设置页当作"设备现值"
                // 读出来(0.00)。
                // We created this key -> remove the property instead of writing 0. Writing 0
                // fabricates a "slowest/no-acceleration" state out of nothing: the old version did
                // exactly that for HIDPointerAcceleration, leaving a never-effective 0 on the
                // device that the settings page then read back as the "live" device value (0.00).
                let ok = IOHIDServiceClientSetProperty(sp.service, key_cf, std::ptr::null_mut());
                log_debug!("[pointer] removed property we created: {} (ok={})", key, ok);
            }
            CFRelease(key_cf);
        }
        CFRelease(state.services as *const c_void);
        CFRelease(state.client as *const c_void);
    }
    log_debug!("[pointer] restored system acceleration settings.");
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
    // 键的选择必须与写入端(`accel_property_key`)一致:否则会读到我们写错键留下的值,
    // 把"从未生效的 0"当成设备现值显示给用户(设置页显示 0.00 的由来)。
    // The key choice must match the write path (`accel_property_key`): otherwise this reads the
    // value left in the wrong key and shows a never-effective 0 to the user as the device's live
    // value (how the settings page ended up displaying 0.00).
    let declared = crate::mouse::device::device_string_property(device, KEY_ACCEL_TYPE);
    let key = choose_accel_key(
        declared.as_deref(),
        crate::mouse::device::device_int_property(device, KEY_POINTER_ACCEL).is_some(),
    );
    let raw = crate::mouse::device::device_int_property(device, &key)?;
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
        // 0.6875(触控板/指针键默认)×65536 = 45056;1.00(鼠标键默认)×65536 = 65536。
        // 0.6875 (the trackpad/pointer key default) × 65536 = 45056; 1.00 (the mouse key default)
        // × 65536 = 65536.
        assert_eq!(acceleration_to_iofixed(FALLBACK_ACCELERATION), 65_536);
        assert_eq!(iofixed_to_acceleration(65_536), 1.0);
        assert_eq!(acceleration_to_iofixed(0.6875), 45056);
        assert_eq!(iofixed_to_acceleration(45056), 0.6875);
        assert_eq!(acceleration_to_iofixed(0.0), 0);
        assert_eq!(acceleration_to_iofixed(10.0), 655_360);
    }

    #[test]
    fn iofixed_rounds_and_clamps() {
        // 非整数倍:四舍五入到最近的 IOFixed。
        // Non-integral multiples round to the nearest IOFixed.
        assert_eq!(acceleration_to_iofixed(1.25), 81920);
        assert_eq!(acceleration_to_iofixed(0.0001), 7);
        // 越界值被 clamp 到 0..=10(配置层已校验,这里是兜底)。
        // Out-of-range values clamp to 0..=10 (the config layer validates; this is a backstop).
        assert_eq!(acceleration_to_iofixed(-5.0), 0);
        assert_eq!(acceleration_to_iofixed(1000.0), 655_360);
    }

    #[test]
    fn accel_key_prefers_the_device_declaration() {
        // 实测场景(MCHOSE G3 V2):设备声明 HIDMouseAcceleration,而 HIDPointerAcceleration
        // 也存在(旧版本自己写出来的)——必须用声明键,否则写入无效。
        // Measured case (MCHOSE G3 V2): the device declares HIDMouseAcceleration while
        // HIDPointerAcceleration also exists (created by the old version) -- the declared key
        // must win or the write is a no-op.
        assert_eq!(
            choose_accel_key(Some(KEY_MOUSE_ACCEL), true),
            KEY_MOUSE_ACCEL.to_string()
        );
        // 触控板声明 HIDTrackpadAcceleration 时同理。
        // Same for a trackpad declaring HIDTrackpadAcceleration.
        assert_eq!(
            choose_accel_key(Some("HIDTrackpadAcceleration"), false),
            "HIDTrackpadAcceleration".to_string()
        );
    }

    #[test]
    fn accel_key_falls_back_like_linearmouse() {
        // 无声明:存在 HIDPointerAcceleration 就用它,否则回退 HIDMouseAcceleration
        // (LinearMouse PointerDevice.pointerAccelerationType 的猜测顺序)。
        // Nothing declared: HIDPointerAcceleration when present, else HIDMouseAcceleration (the
        // guess order of LinearMouse's PointerDevice.pointerAccelerationType).
        assert_eq!(choose_accel_key(None, true), KEY_POINTER_ACCEL.to_string());
        assert_eq!(choose_accel_key(None, false), KEY_MOUSE_ACCEL.to_string());
        // 空声明(空字符串)等同"未声明"。
        // An empty declaration counts as "not declared".
        assert_eq!(
            choose_accel_key(Some(""), true),
            KEY_POINTER_ACCEL.to_string()
        );
    }

    #[test]
    fn desired_values_disable_path_uses_config_then_system() {
        let sys = acceleration_to_iofixed(0.6875);
        // 要求禁用 + 配置了跟踪速度 -> 线性开关 1,写入配置值。
        // Disable requested with a configured tracking speed -> linear 1, write the configured value.
        assert_eq!(
            desired_pointer_values(true, Some(3.5), 0, sys),
            DesiredPointerValues {
                linear: 1,
                accel: acceleration_to_iofixed(3.5),
                accel_ignored: false,
            }
        );
        // 要求禁用但未配置跟踪速度 -> 写系统值(LinearMouse 的 restorePointerAcceleration)。
        // Disable requested without a configured speed -> the system value (LinearMouse's
        // restorePointerAcceleration).
        assert_eq!(
            desired_pointer_values(true, None, 0, sys),
            DesiredPointerValues {
                linear: 1,
                accel: sys,
                accel_ignored: false,
            }
        );
    }

    #[test]
    fn desired_values_unset_restores_system_values() {
        let sys = acceleration_to_iofixed(0.6875);
        // 未启用禁用:开关与加速都写回系统值(不是"不动设备")——设备上残留的 1 会被清成
        // 系统值 0,残留的速度值也会被系统值覆盖。
        // Not disabling: both the switch and the acceleration go back to the system values (not
        // "leave the device alone") -- a leftover 1 on the device is cleared to the system's 0 and
        // a leftover speed is overwritten by the system value.
        assert_eq!(
            desired_pointer_values(false, None, 0, sys),
            DesiredPointerValues {
                linear: 0,
                accel: sys,
                accel_ignored: false,
            }
        );
        // 系统自身把线性缩放打开时,我们照样跟随系统值(不强行关掉别人的设置)。
        // When the system itself has linear scaling on, we follow the system value (never override
        // someone else's setting).
        assert_eq!(
            desired_pointer_values(false, None, 1, sys),
            DesiredPointerValues {
                linear: 1,
                accel: sys,
                accel_ignored: false,
            }
        );
        // 未启用禁用 + 配置了数值:仍然写系统值,并标记该数值被忽略(非线性模式下语义不同)。
        // Not disabling with a configured value: still the system value, flagged as ignored (the
        // value has a different meaning while linear tracking is off).
        assert_eq!(
            desired_pointer_values(false, Some(2.5), 0, sys),
            DesiredPointerValues {
                linear: 0,
                accel: sys,
                accel_ignored: true,
            }
        );
    }
}
