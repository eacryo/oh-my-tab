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
use crate::{log_debug, log_info};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use std::ffi::c_void;
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

/// 运行时设备句柄。硬件身份只在 DeviceIdentity 里;枚举时的 service_client 指针由
/// services 数组保活(归因时做 CFEqual 比对)。
///
/// Runtime device handle. Hardware identity lives only in DeviceIdentity; the service_client
/// pointer from enumeration is kept alive by the services array (used for CFEqual during
/// attribution).
pub(crate) struct Device {
    pub(crate) identity: DeviceIdentity,
    /// 枚举时的 IOHIDServiceClient 指针(由 services 数组保活;用于归因时的 CFEqual 比对)。
    /// The IOHIDServiceClient pointer from enumeration (kept alive by the services array;
    /// used for CFEqual comparison during attribution).
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
}

unsafe impl Send for DeviceRegistry {}
unsafe impl Sync for DeviceRegistry {}

static REGISTRY: OnceLock<Mutex<DeviceRegistry>> = OnceLock::new();

/// last_active 的设备 (VID, PID)(归因失败时的回退)。由 event_tap 回调在每次成功归因后更新。
/// 存硬件身份而非进程内 id:设备重枚举后 id 会漂移(NEXT_ID 单调递增),而 VID/PID 稳定
/// (蓝牙断连重连后不变),按硬件身份兜底才可靠。
///
/// Last-active device's (VID, PID) (fallback when attribution fails). Updated by the event_tap
/// callback after each successful attribution. Stored by hardware identity, not the process-local
/// id: the id drifts on re-enumeration (NEXT_ID is monotonic), while VID/PID are stable across a
/// Bluetooth disconnect/reconnect, so the hardware identity is the reliable fallback.
static LAST_ACTIVE_KEY: Mutex<Option<DeviceKey>> = Mutex::new(None);

fn registry() -> &'static Mutex<DeviceRegistry> {
    REGISTRY.get_or_init(|| Mutex::new(DeviceRegistry {
        client: std::ptr::null_mut(),
        services: std::ptr::null_mut(),
        devices: Vec::new(),
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

// ========== 枚举 / enumeration ==========

/// 枚举当前已连接的鼠标/触控板设备,填充注册表。
/// 调用方持有 REGISTRY 锁。rebuild_client = true 时强制重建 IOHIDEventSystemClient
/// (蓝牙设备休眠断连重连后,旧 client 的注册表缓存过期,导致归因链失效——见
/// device_from_cgevent 的失败路径)。
///
/// Enumerate currently-connected mouse/trackpad devices and populate the registry.
/// Caller holds the REGISTRY lock. With rebuild_client = true, the IOHIDEventSystemClient is
/// forcibly recreated (after a Bluetooth disconnect/reconnect the old client's registry cache is
/// stale, breaking the attribution chain -- see the failure path in device_from_cgevent).
unsafe fn enumerate_locked(reg: &mut DeviceRegistry, rebuild_client: bool) {
    // 释放旧的 services 数组(若有)。
    // Release the previous services array (if any).
    if !reg.services.is_null() {
        CFRelease(reg.services as *const c_void);
        reg.services = std::ptr::null_mut();
    }
    if rebuild_client {
        // 蓝牙断连重连后旧 client 已失效:释放并置 null,强制重建。
        // After a Bluetooth reconnect the old client is stale: release and null it so it's
        // recreated below.
        if !reg.client.is_null() {
            CFRelease(reg.client as *const c_void);
            reg.client = std::ptr::null_mut();
        }
    }
    if reg.client.is_null() {
        reg.client = IOHIDEventSystemClientCreate(std::ptr::null());
        if reg.client.is_null() {
            log_info!("[device] failed to create IOHIDEventSystemClient");
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
        log_info!("[device] no services returned");
        reg.devices.clear();
        return;
    }
    reg.services = services;

    reg.devices.clear();

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

        let dev = Device {
            identity: DeviceIdentity {
                vendor_id: vid,
                product_id: pid,
                name,
                transport,
            },
            service_client: service,
        };
        reg.devices.push(dev);
    }

    log_debug!(
        "[device] enumerated {} pointer device(s).",
        reg.devices.len()
    );
}

/// 启动时枚举一次(惰性:首次归因失败时也会触发)。
/// Enumerate once at startup (also lazily triggered on first attribution failure).
pub(crate) fn ensure_enumerated() {
    let mut reg = registry().lock().unwrap();
    if reg.client.is_null() || reg.devices.is_empty() {
        unsafe { enumerate_locked(&mut reg, false) };
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

// ========== 设备插拔监听 / device plug/unplug monitoring ==========

/// IOHIDManager 实例(保活:释放后回调即失效)。由 start_plug_monitor 创建,仅一次。
/// 用 Send+Sync 包装的 Mutex(与 DeviceRegistry 同模式,static 需要 Send+Sync)。
/// IOHIDManager instance (kept alive: releasing it would invalidate the callbacks). Created by
/// start_plug_monitor, once. Wrapped Mutex with Send+Sync (same pattern as DeviceRegistry;
/// statics need Send+Sync).
struct ManagerMutex(Mutex<*mut c_void>);
unsafe impl Send for ManagerMutex {}
unsafe impl Sync for ManagerMutex {}

static MANAGER: OnceLock<ManagerMutex> = OnceLock::new();

fn manager_static() -> &'static Mutex<*mut c_void> {
    &MANAGER.get_or_init(|| ManagerMutex(Mutex::new(std::ptr::null_mut()))).0
}

/// IOHIDManager 回调:设备接入/移除时,强制重建注册表(重建 IOHIDEventSystemClient)。
/// 蓝牙鼠标休眠断连重连正是走这里——重连触发 removal+matching 回调,重建后归因链恢复,
/// 不必等下一次归因失败才自愈。
///
/// IOHIDManager callback: on device attach/detach, force-rebuild the registry (recreate the
/// IOHIDEventSystemClient). A Bluetooth disconnect/reconnect fires these callbacks; the rebuild
/// restores the attribution chain without waiting for the next failed attribution.
unsafe extern "C" fn device_change_callback(
    _context: *mut c_void,
    _result: i32,
    _sender: *mut c_void,
    _callback: *mut c_void,
) {
    let mut reg = registry().lock().unwrap();
    enumerate_locked(&mut reg, true);
    log_debug!("[device] plug/unplug event: re-enumerated {} device(s).", reg.devices.len());
}

/// 启动设备插拔监听:创建 IOHIDManager,注册接入/移除回调,挂到指定 RunLoop。
/// 由鼠标线程调用(传该线程的 CFRunLoop),回调即在那个线程执行,与 event tap 同线程,
/// 对 REGISTRY 加锁安全。matching 与枚举一致:Generic Desktop 页的指针/鼠标/触控板。
///
/// Start device plug/unplug monitoring: create an IOHIDManager, register attach/detach callbacks,
/// and schedule it on the given RunLoop (the mouse thread's). Callbacks then run on that thread,
/// same as the event tap, so locking REGISTRY is safe. Matching mirrors enumeration: pointer/mouse/
/// trackpad on the Generic Desktop page.
pub(crate) unsafe fn start_plug_monitor(runloop: crate::event_tap::CFRunLoopRef) {
    let m = manager_static();
    let mut m = m.lock().unwrap();
    if !m.is_null() {
        return;
    }
    let manager_obj = IOHIDManagerCreate(std::ptr::null(), 0);
    if manager_obj.is_null() {
        log_info!("[device] failed to create IOHIDManager");
        return;
    }
    // matching:PrimaryUsagePage=Generic Desktop + PrimaryUsage in {Pointer, Mouse, Trackpad}。
    // 与 enumerate_locked 的过滤一致。
    // Matching: PrimaryUsagePage = Generic Desktop + PrimaryUsage in {Pointer, Mouse, Trackpad},
    // mirroring enumerate_locked's filter.
    let page_key = make_nsstring(KEY_PRIMARY_USAGE_PAGE);
    let page_val: *mut AnyObject =
        msg_send![class!(NSNumber), numberWithInt: USAGE_PAGE_GENERIC_DESKTOP as i32];
    let dict: *mut AnyObject =
        msg_send![class!(NSDictionary), dictionaryWithObject: page_val, forKey: page_key];
    let arr: *mut AnyObject = msg_send![class!(NSArray), arrayWithObject: dict];
    IOHIDManagerSetDeviceMatchingMultiple(manager_obj, arr as *const c_void);
    CFRelease(page_key as *const c_void);

    let cb: Option<unsafe extern "C" fn(*mut c_void, i32, *mut c_void, *mut c_void)> =
        Some(device_change_callback as unsafe extern "C" fn(*mut c_void, i32, *mut c_void, *mut c_void));
    IOHIDManagerRegisterDeviceMatchingCallback(manager_obj, cb, std::ptr::null_mut());
    IOHIDManagerRegisterDeviceRemovalCallback(manager_obj, cb, std::ptr::null_mut());
    IOHIDManagerScheduleWithRunLoop(manager_obj, runloop, crate::event_tap::kCFRunLoopDefaultMode);
    *m = manager_obj;
    log_info!("[device] plug/unplug monitor started.");
}

// ========== 归因 / attribution ==========

/// 用 senderID 反查设备索引。
/// 链路:IOHIDEventSystemClientCopyServiceForRegistryID(client, senderID) 得到 IOHIDServiceClient,
/// 再用 CFEqual 与枚举列表逐项比对(Swift 字典对 CF key 也用 CFEqual,而非裸指针地址——
/// Copy 返回的对象与枚举出的可能不是同一实例地址)。
///
/// Look up the device index by sender ID. The chain:
/// IOHIDEventSystemClientCopyServiceForRegistryID(client, senderID) yields an IOHIDServiceClient,
/// then CFEqual matches it against the enumerated list (Swift dictionaries also compare CF keys
/// with CFEqual, not raw pointer addresses -- the Copy-returned object may not be the same
/// instance address as the enumerated one).
unsafe fn lookup_service_index(reg: &DeviceRegistry, sender: u64) -> Option<usize> {
    if reg.client.is_null() {
        return None;
    }
    let svc = IOHIDEventSystemClientCopyServiceForRegistryID(reg.client, sender);
    if svc.is_null() {
        return None;
    }
    // 设备数极少(鼠标/触控板几个),线性 CFEqual 遍历足够快。
    // Device count is tiny (a few mice/trackpads); a linear CFEqual scan is fast enough.
    let idx = reg
        .devices
        .iter()
        .position(|d| crate::ffi::CFEqual(d.service_client, svc));
    // Copy 返回 +1,立即释放(索引已取出,设备由 services 数组保活)。
    // Copy returns +1; release immediately (the index is already taken; devices are kept
    // alive by the services array).
    CFRelease(svc as *const c_void);
    idx
}

/// 从 CGEvent 找到产生它的设备。
/// 归因链:CGEventCopyIOHIDEvent -> IOHIDEventGetSenderID ->
/// IOHIDEventSystemClientCopyServiceForRegistryID -> CFEqual 匹配枚举列表。
/// 失败时惰性重枚举一次再查;仍失败返回 last_active(若无则 None,调用方用"所有鼠标"档)。
///
/// Find the device that produced a CGEvent.
/// Chain: CGEventCopyIOHIDEvent -> IOHIDEventGetSenderID ->
/// IOHIDEventSystemClientCopyServiceForRegistryID -> CFEqual against the enumerated list.
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
            if let Some(idx) = lookup_service_index(&reg, sender) {
                let dev = &reg.devices[idx];
                *LAST_ACTIVE_KEY.lock().unwrap() = Some(dev.key());
                return Some(dev.key());
            }
        }
        // 未命中:可能新设备插入或蓝牙断连重连后注册表过期。强制重建 client + 重枚举后重查。
        // 只重枚举不重建 client 是不够的:蓝牙重连后旧 client 的缓存已失效,重枚举拿到的
        // 仍是过期列表,归因会持续失败(表现为每次事件都重枚举仍不命中)。
        // Miss: a newly-plugged device or a Bluetooth reconnect may have made the registry stale.
        // Force-rebuild the client + re-enumerate, then retry. Re-enumerating with the old client
        // is not enough: after a Bluetooth reconnect the old client's cache is dead, so the
        // re-enumeration still yields a stale list and attribution keeps failing (observable as a
        // re-enumeration on every event that still misses).
        {
            let mut reg = registry().lock().unwrap();
            enumerate_locked(&mut reg, true);
            if let Some(idx) = lookup_service_index(&reg, sender) {
                let dev = &reg.devices[idx];
                *LAST_ACTIVE_KEY.lock().unwrap() = Some(dev.key());
                return Some(dev.key());
            }
        }
        last_active_key()
    }
}

/// 回退:取 last_active 设备的 (VID,PID)。按硬件身份匹配,不受重枚举 id 漂移影响。
/// Fallback: return the last-active device's (VID, PID), matched by hardware identity so
/// re-enumeration id drift doesn't break it.
fn last_active_key() -> Option<DeviceKey> {
    let key = *LAST_ACTIVE_KEY.lock().unwrap();
    key.and_then(|k| {
        let reg = registry().lock().unwrap();
        reg.devices
            .iter()
            .find(|d| d.key() == k)
            .map(|d| d.key())
    })
}
