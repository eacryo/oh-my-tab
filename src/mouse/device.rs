//! 实时设备注册表 + CGEvent -> 产生事件的设备的归因链(对应 Phase 0+1)。
//!
//! 归因链(私有 SPI,沿用 LinearMouse 的做法):
//!   CGEventCopyIOHIDEvent(cgEvent) -> IOHIDEventRef
//!   IOHIDEventGetSenderID(ioHIDEvent) -> registry_id (uint64)
//!   by_registry_id 查表 -> Device
//! 失败时惰性重枚举一次;仍失败则回退到 last_active(匹配"所有鼠标"档)。
//!
//! Live device registry + CGEvent -> producing-device attribution chain (Phase 0+1).
//!
//! Attribution chain (private SPI, mirroring LinearMouse):
//!   CGEventCopyIOHIDEvent(cgEvent) -> IOHIDEventRef
//!   IOHIDEventGetSenderID(ioHIDEvent) -> registry_id (uint64)
//!   by_registry_id lookup -> Device
//! On failure, lazily re-enumerate once; if still failing, fall back to last_active
//! (which matches the "All Mice" profile).

use crate::ffi::{make_nsstring, nsstring_to_rust, CFRelease};
use crate::mouse::ffi::*;
use crate::{log_info, log_warn};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

// ========== 设备身份与运行时表示 / device identity & runtime handle ==========

/// 设备硬件身份(配置按 VID+PID 匹配;name/transport 仅展示用)。
/// Device hardware identity (config matches on VID+PID; name/transport for display only).
#[derive(Debug, Clone)]
pub(crate) struct DeviceIdentity {
    pub(crate) vendor_id: u32,
    pub(crate) product_id: u32,
    pub(crate) name: String,
    #[allow(dead_code)]
    pub(crate) transport: String,
}

/// 运行时设备句柄。`id` 是进程内单调计数器,拔插后变化(对象身份 ≠ 硬件身份,
/// 沿用 LinearMouse 的做法:每个插拔事件创建新的 Device 实例,硬件身份只在 DeviceIdentity 里)。
///
/// Runtime device handle. `id` is a process-local monotonic counter that changes on replug
/// (object identity != hardware identity, mirroring LinearMouse: each plug/unplug creates a new
/// Device instance; hardware identity lives only in DeviceIdentity).
pub(crate) struct Device {
    pub(crate) id: u32,
    pub(crate) identity: DeviceIdentity,
    /// IORegistry entry ID;与 IOHIDEventGetSenderID 返回值匹配,是归因链的桥接键。
    /// IORegistry entry ID; matches IOHIDEventGetSenderID's return, bridging the attribution chain.
    #[allow(dead_code)]
    pub(crate) registry_id: u64,
    #[allow(dead_code)]
    service_client: *mut c_void,
}

unsafe impl Send for Device {}
unsafe impl Sync for Device {}

/// (VID, PID) 组合,用作解析缓存与平滑引擎 per-device 状态的键。
/// (VID, PID) pair, used as the key for the resolve cache and per-device smooth-engine state.
pub(crate) type DeviceKey = (u32, u32);

impl Device {
    pub(crate) fn key(&self) -> DeviceKey {
        (self.identity.vendor_id, self.identity.product_id)
    }
}

// ========== 全局注册表 / global registry ==========

struct DeviceRegistry {
    /// 持有 event system client(保活 service client)。
    /// Holds the event system client (keeps service clients alive).
    client: *mut c_void,
    /// 持有 services CFArray(保活其中的 IOHIDServiceClient)。
    /// Holds the services CFArray (keeping its IOHIDServiceClients alive).
    services: *mut c_void,
    devices: Vec<Device>,
    by_registry_id: std::collections::HashMap<u64, usize>,
}

unsafe impl Send for DeviceRegistry {}
unsafe impl Sync for DeviceRegistry {}

static REGISTRY: OnceLock<Mutex<DeviceRegistry>> = OnceLock::new();
static NEXT_ID: AtomicU32 = AtomicU32::new(1);

/// last_active 的设备 id(归因失败时的回退)。由 event_tap 回调在每次成功归因后更新。
/// last-active device id (fallback when attribution fails). Updated by the event_tap callback
/// after each successful attribution.
static LAST_ACTIVE_ID: Mutex<Option<u32>> = Mutex::new(None);

fn registry() -> &'static Mutex<DeviceRegistry> {
    REGISTRY.get_or_init(|| Mutex::new(DeviceRegistry {
        client: std::ptr::null_mut(),
        services: std::ptr::null_mut(),
        devices: Vec::new(),
        by_registry_id: std::collections::HashMap::new(),
    }))
}

// ========== 属性读取 helper / property-read helpers ==========
// 复用 pointer.rs 的模式;这里只读不写,故更简单。

/// 读取 IOHIDServiceClient 的整数属性(CFNumber)。
/// Read an integer property from an IOHIDServiceClient (CFNumber).
unsafe fn prop_int(service: *mut c_void, key: &str) -> Option<i64> {
    let k = make_nsstring(key);
    let v = IOHIDServiceClientCopyProperty(service, k as *const c_void);
    CFRelease(k as *const c_void);
    if v.is_null() {
        return None;
    }
    let i: i64 = msg_send![v as *mut AnyObject, longLongValue];
    CFRelease(v as *const c_void);
    Some(i)
}

/// 读取 IOHIDServiceClient 的字符串属性(NSString)。
/// Read a string property from an IOHIDServiceClient (NSString).
unsafe fn prop_string(service: *mut c_void, key: &str) -> String {
    let k = make_nsstring(key);
    let v = IOHIDServiceClientCopyProperty(service, k as *const c_void);
    CFRelease(k as *const c_void);
    if v.is_null() {
        return String::new();
    }
    let s = nsstring_to_rust(v as *mut AnyObject);
    CFRelease(v as *const c_void);
    s
}

// 读取设备的 IORegistry entry ID(用于归因匹配)。
// 私有 SPI:IOHIDServiceClientGetRegistryID(LinearMouse 的 IOHIDServiceClient+Service 同款)。
// Read the device's IORegistry entry ID (used for attribution matching).
// Private SPI IOHIDServiceClientGetRegistryID (LinearMouse uses the same via
// IOHIDServiceClient+Service).
#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOHIDServiceClientGetRegistryID(service: *mut c_void) -> u64;
}

/// 读取一个 service client 的 registry ID。
/// Read a service client's registry ID.
unsafe fn registry_id_of(service: *mut c_void) -> u64 {
    IOHIDServiceClientGetRegistryID(service)
}

// ========== 枚举 / enumeration ==========

/// 枚举当前已连接的鼠标/触控板设备,填充注册表。
/// 调用方持有 REGISTRY 锁。
///
/// Enumerate currently-connected mouse/trackpad devices and populate the registry.
/// Caller holds the REGISTRY lock.
unsafe fn enumerate_locked(reg: &mut DeviceRegistry) {
    // 释放旧的 services 数组(若有)。
    // Release the previous services array (if any).
    if !reg.services.is_null() {
        CFRelease(reg.services as *const c_void);
        reg.services = std::ptr::null_mut();
    }
    if reg.client.is_null() {
        reg.client = IOHIDEventSystemClientCreate(std::ptr::null());
        if reg.client.is_null() {
            log_warn!("[device] failed to create IOHIDEventSystemClient");
            return;
        }
        // 匹配 Generic Desktop 页(后续按 usage 过滤)。
        // Match the Generic Desktop page (filtered by usage below).
        let page_key = make_nsstring(KEY_PRIMARY_USAGE_PAGE);
        let page_val: *mut AnyObject =
            msg_send![class!(NSNumber), numberWithInt: USAGE_PAGE_GENERIC_DESKTOP as i32];
        let dict: *mut AnyObject =
            msg_send![class!(NSDictionary), dictionaryWithObject: page_val, forKey: page_key];
        let arr: *mut AnyObject = msg_send![class!(NSArray), arrayWithObject: dict];
        IOHIDEventSystemClientSetMatchingMultiple(reg.client, arr as *const c_void);
        CFRelease(page_key as *const c_void);
    }

    let services = IOHIDEventSystemClientCopyServices(reg.client);
    if services.is_null() {
        log_warn!("[device] no services returned");
        reg.devices.clear();
        reg.by_registry_id.clear();
        return;
    }
    reg.services = services;

    reg.devices.clear();
    reg.by_registry_id.clear();

    let count = CFArrayGetCount(services);
    for i in 0..count {
        let service = CFArrayGetValueAtIndex(services, i) as *mut c_void;
        let page = prop_int(service, KEY_PRIMARY_USAGE_PAGE).unwrap_or(0);
        let usage = prop_int(service, KEY_PRIMARY_USAGE).unwrap_or(0);
        // 只处理指针/鼠标/触控板,跳过键盘等其他 Generic Desktop 设备。
        // Only handle pointer/mouse/trackpad; skip keyboards and other Generic Desktop devices.
        if page != USAGE_PAGE_GENERIC_DESKTOP
            || !(usage == USAGE_GD_POINTER
                || usage == USAGE_GD_MOUSE
                || usage == USAGE_GD_TRACKPAD)
        {
            continue;
        }
        let vid = prop_int(service, KEY_VENDOR_ID).unwrap_or(0) as u32;
        let pid = prop_int(service, KEY_PRODUCT_ID).unwrap_or(0) as u32;
        let name = prop_string(service, KEY_PRODUCT);
        let transport = prop_string(service, KEY_TRANSPORT);
        let registry_id = registry_id_of(service);

        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dev = Device {
            id,
            identity: DeviceIdentity {
                vendor_id: vid,
                product_id: pid,
                name,
                transport,
            },
            registry_id,
            service_client: service,
        };
        reg.by_registry_id.insert(registry_id, reg.devices.len());
        reg.devices.push(dev);
    }

    log_info!(
        "[device] enumerated {} pointer device(s).",
        reg.devices.len()
    );
}

/// 启动时枚举一次(惰性:首次归因失败时也会触发)。
/// Enumerate once at startup (also lazily triggered on first attribution failure).
pub(crate) fn ensure_enumerated() {
    let mut reg = registry().lock().unwrap();
    if reg.client.is_null() || reg.devices.is_empty() {
        unsafe { enumerate_locked(&mut reg) };
    }
}

/// 当前已连接设备列表的快照(VID/PID/名称),供设置 UI 的设备选择器使用。
/// 若 registry 为空,先触发一次枚举(即使 mouse.enabled=false 也能拿到设备列表)。
///
/// Snapshot of currently-connected devices (VID/PID/name) for the settings device picker.
/// Triggers enumeration if the registry is empty (works even when mouse.enabled=false).
pub(crate) fn connected_devices() -> Vec<DeviceIdentity> {
    {
        let reg = registry().lock().unwrap();
        if reg.client.is_null() || reg.devices.is_empty() {
            drop(reg);
            ensure_enumerated();
        }
    }
    let reg = registry().lock().unwrap();
    reg.devices.iter().map(|d| d.identity.clone()).collect()
}

// ========== 归因 / attribution ==========

/// 从 CGEvent 找到产生它的设备。
/// 归因链:CGEventCopyIOHIDEvent -> IOHIDEventGetSenderID -> by_registry_id 查表。
/// 失败时惰性重枚举一次再查;仍失败返回 last_active(若无则 None,调用方用"所有鼠标"档)。
///
/// Find the device that produced a CGEvent.
/// Chain: CGEventCopyIOHIDEvent -> IOHIDEventGetSenderID -> by_registry_id lookup.
/// On failure, lazily re-enumerate once and retry; if still failing, return last_active
/// (or None if there is none, in which case the caller uses the "All Mice" profile).
pub(crate) fn device_from_cgevent(cg_event: crate::event_tap::CGEventRef) -> Option<DeviceKey> {
    unsafe {
        let io = crate::event_tap::CGEventCopyIOHIDEvent(cg_event);
        if io.is_null() {
            return last_active_key();
        }
        let sender = IOHIDEventGetSenderID(io);
        CFRelease(io as *const c_void);
        if sender == 0 {
            return last_active_key();
        }
        // 第一次查表。
        // First lookup.
        {
            let reg = registry().lock().unwrap();
            if let Some(&idx) = reg.by_registry_id.get(&sender) {
                let dev = &reg.devices[idx];
                *LAST_ACTIVE_ID.lock().unwrap() = Some(dev.id);
                return Some(dev.key());
            }
        }
        // 未命中:可能新设备插入后注册表过期。惰性重枚举后重查一次。
        // Miss: a newly-plugged device may have made the registry stale. Re-enumerate + retry once.
        {
            let mut reg = registry().lock().unwrap();
            enumerate_locked(&mut reg);
            if let Some(&idx) = reg.by_registry_id.get(&sender) {
                let dev = &reg.devices[idx];
                *LAST_ACTIVE_ID.lock().unwrap() = Some(dev.id);
                return Some(dev.key());
            }
        }
        last_active_key()
    }
}

/// 回退:取 last_active 设备的 (VID,PID)。
/// Fallback: return the last-active device's (VID, PID).
fn last_active_key() -> Option<DeviceKey> {
    let id = *LAST_ACTIVE_ID.lock().unwrap();
    id.and_then(|id| {
        let reg = registry().lock().unwrap();
        reg.devices
            .iter()
            .find(|d| d.id == id)
            .map(|d| d.key())
    })
}
