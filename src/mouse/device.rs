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
use objc2::{class, msg_send, sel};
use std::collections::HashMap;
use std::ffi::{c_void, CString};
use std::sync::atomic::{AtomicBool, Ordering};
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

/// (VID, PID) 组合,用作解析缓存与 per-device 状态的键。
/// (VID, PID) pair, used as the key for the resolve cache and per-device state.
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

/// 鼠标线程的 CFRunLoop(由 start_plug_monitor 记录)。注册表 client 要调度到这个
/// runloop 匹配才会常驻(见 enumerate_locked);enumeration 可能先于插拔监听执行,
/// 故用运行时读取而非启动时一次性传入。裸指针用 Send+Sync 包装(与 ManagerMutex 同款)。
/// The mouse thread's CFRunLoop (recorded by start_plug_monitor). The registry client must be
/// scheduled on it for matching to stay live (see enumerate_locked); enumeration can run before
/// the plug monitor starts, so the value is read at runtime, not passed in once at startup.
/// The raw pointer is wrapped with Send+Sync (same pattern as ManagerMutex).
struct RunloopMutex(Mutex<Option<crate::event_tap::CFRunLoopRef>>);
unsafe impl Send for RunloopMutex {}
unsafe impl Sync for RunloopMutex {}

static MOUSE_RUNLOOP: OnceLock<RunloopMutex> = OnceLock::new();

fn mouse_runloop_static() -> &'static Mutex<Option<crate::event_tap::CFRunLoopRef>> {
    &MOUSE_RUNLOOP.get_or_init(|| RunloopMutex(Mutex::new(None))).0
}

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

// ========== 蓝牙设备分类:GAP Appearance(NVRAM 缓存)/ Bluetooth classification ==========

/// 读取 NVRAM 中 bluetoothd 写入的蓝牙设备缓存(BluetoothInfo),解析出
/// 地址 -> GAP Appearance 映射。Appearance 是设备在蓝牙广播里自报的类别
/// (0x03C1 = 键盘 / 0x03C2 = 鼠标),与 macOS 蓝牙面板的图标同源。HID 描述符
/// 不可靠(部分键盘固件虚报鼠标用途,如 KZI I75),用这张表做精确分类。
/// 解析失败/缓存缺失时返回空表,调用方回退到纯 HID 判定。
///
/// Read bluetoothd's BluetoothInfo cache from NVRAM, parsing an address -> GAP Appearance
/// map. Appearance is the device's self-reported class in its Bluetooth advertisement
/// (0x03C1 keyboard / 0x03C2 mouse), the same source macOS's Bluetooth pane icons use.
/// HID descriptors are unreliable here (some keyboard firmware fakes mouse usages, e.g.
/// KZI I75), so this table performs the precise classification. On failure / missing cache
/// an empty map is returned and the caller falls back to HID-only classification.
fn bluetooth_appearance_map() -> HashMap<String, u16> {
    let mut map = HashMap::new();
    unsafe {
        // IORegistryEntryFromPath 返回 +1,用完 IOObjectRelease。
        // IORegistryEntryFromPath returns +1; IOObjectRelease when done.
        let path = match CString::new(IOSERVICE_OPTIONS_PATH) {
            Ok(p) => p,
            Err(_) => return map,
        };
        let entry = IORegistryEntryFromPath(0, path.as_ptr());
        if entry.is_null() {
            return map;
        }
        let mut props: *mut c_void = std::ptr::null_mut();
        let kr = IORegistryEntryCreateCFProperties(entry, &mut props, std::ptr::null(), 0);
        IOObjectRelease(entry);
        if kr != 0 || props.is_null() {
            return map;
        }
        let key = make_nsstring(KEY_BLUETOOTH_INFO);
        let data = CFDictionaryGetValue(props as *const c_void, key as *const c_void);
        CFRelease(key as *const c_void);
        if data.is_null() {
            CFRelease(props as *const c_void);
            return map;
        }
        let len = CFDataGetLength(data) as usize;
        let ptr = CFDataGetBytePtr(data);
        // 在 props 释放前完成解析(CFDataGetBytePtr 借用 props 持有的数据)。
        // Parse before releasing props (CFDataGetBytePtr borrows data owned by props).
        if !ptr.is_null() && len > 0 {
            parse_bluetooth_info(std::slice::from_raw_parts(ptr, len), &mut map);
        }
        CFRelease(props as *const c_void);
    }
    map
}

/// 解析 BluetoothInfo TLV 流(bluetoothd 私有格式,按实测稳定结构解析):
/// tag 0x02 = 设备名(记录起点),0x0e = 蓝牙地址(7 字节:1 字节标记 + 6 字节地址),
/// 0x11 = GAP Appearance(2 字节小端,实测:键盘存 c1 03,鼠标存 c2 03)。
/// 地址与 Appearance 按记录内顺序配对:0x0e 暂存地址,0x11 出现时写入 map。
/// 未知 tag 按 length 跳过,异常数据即停。
///
/// Parse the BluetoothInfo TLV stream (bluetoothd private format; stable structure verified
/// empirically): tag 0x02 = device name (record start), 0x0e = BT address (7 bytes: 1 flag
/// byte + 6 address bytes), 0x11 = GAP Appearance (2 bytes little-endian; measured: keyboard
/// is c1 03, mouse is c2 03). The address is paired with the next appearance in a record:
/// 0x0e stashes the address, 0x11 writes the map entry. Unknown tags are skipped by length;
/// malformed data stops parsing.
fn parse_bluetooth_info(bytes: &[u8], map: &mut HashMap<String, u16>) {
    let mut i = 0;
    let mut pending_addr: Option<String> = None;
    while i + 2 <= bytes.len() {
        let tag = bytes[i];
        let len = bytes[i + 1] as usize;
        i += 2;
        if i + len > bytes.len() {
            break;
        }
        match tag {
            0x0e if len == 7 => {
                pending_addr = Some(
                    bytes[i + 1..i + 7]
                        .iter()
                        .map(|b| format!("{b:02X}"))
                        .collect::<Vec<_>>()
                        .join("-"),
                );
            }
            0x11 if len == 2 => {
                if let Some(addr) = pending_addr.take() {
                    map.insert(addr, u16::from_le_bytes([bytes[i], bytes[i + 1]]));
                }
            }
            _ => {}
        }
        i += len;
    }
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
        // IOHIDEventSystemClient 的匹配是异步的:刚创建/重建后立刻 CopyServices 可能拿到
        // 空列表(实测枚举结果在 0/1 间摇摆)。等 ~30ms 让匹配完成,枚举才可靠。
        // 该竞态只在新建 client 的路径出现,复用旧 client 的热路径(事件归因)不受影响。
        // Matching on IOHIDEventSystemClient is asynchronous: CopyServices right after
        // creation/rebuild can return an empty list (measured 0/1 flapping). Waiting ~30ms
        // lets matching settle; only the fresh-client path has this race, so the hot
        // attribution path (reusing an old client) is unaffected.
        std::thread::sleep(std::time::Duration::from_millis(30));
        // 调度到鼠标线程 runloop:未调度的 client 的 registry-ID 映射不完整,归因用的
        // CopyServiceForRegistryID 会持续返回 nil(实测),滚动事件归因失败后回退到
        // "所有鼠标"档 —— per-device 设置(如反转滚动)在启动后不生效,直到重新枚举
        // 出可用的 client。调度后匹配常驻,归因可靠(LinearMouse 同款做法)。
        // Schedule the client on the mouse thread's runloop: an unscheduled client's
        // registry-ID map is incomplete and CopyServiceForRegistryID keeps returning nil
        // (measured), so scroll attribution fails and falls back to the "All Mice" profile --
        // per-device settings (e.g. reverse scrolling) don't apply after startup until a
        // working client is re-enumerated. Scheduling keeps the matching live and makes
        // attribution reliable (same approach as LinearMouse).
        if let Some(rl) = *mouse_runloop_static().lock().unwrap() {
            IOHIDEventSystemClientScheduleWithRunLoop(
                reg.client,
                rl,
                crate::event_tap::kCFRunLoopDefaultMode,
            );
        }
    }

    let services = IOHIDEventSystemClientCopyServices(reg.client);
    if services.is_null() {
        log_info!("[device] no services returned");
        reg.devices.clear();
        return;
    }
    reg.services = services;

    reg.devices.clear();

    // 蓝牙设备分类表(NVRAM 里 bluetoothd 的 BluetoothInfo 缓存)。解析失败时为空表,
    // 蓝牙设备全部回退 HID 判定,不影响原有行为。
    // Bluetooth classification table (bluetoothd's BluetoothInfo cache in NVRAM). On parse
    // failure it is empty and every Bluetooth device falls back to HID-only classification,
    // preserving the previous behavior.
    let appearance_map = bluetooth_appearance_map();

    let count = CFArrayGetCount(services);
    for i in 0..count {
        let service = CFArrayGetValueAtIndex(services, i) as *mut c_void;
        // 用 ConformsTo 判断是否指针/鼠标/触控板,不读 PrimaryUsage 单值。
        // 原因:有些真实鼠标(如 ATK A9 SE 这类 Nearlink/星闪设备)的 PrimaryUsage 被
        // 系统报成 Keyboard(6),白名单 {1,2,5} 会把它剔除;ConformsTo 检查整个
        // DeviceUsagePairs,能识别它声明过的 Mouse(1,2)/Pointer(1,1)/Trackpad(1,5)。
        // 副作用:少数键盘也声明了多余的 Mouse 用途(如 KZI I75),会一并纳入——
        // 蓝牙键盘随后由下方的 GAP Appearance 判定排除;归因靠 senderID 精确匹配,
        // 不影响功能正确性。
        //
        // Determine pointer/mouse/trackpad via ConformsTo instead of the PrimaryUsage scalar.
        // Some real mice (e.g. ATK A9 SE Nearlink devices) report PrimaryUsage = Keyboard(6),
        // and a {1,2,5} whitelist would drop them; ConformsTo inspects the full DeviceUsagePairs
        // and sees the Mouse(1,2)/Pointer(1,1)/Trackpad(1,5) usages they declare. Side effect:
        // a few keyboards also declare extra Mouse usages (e.g. KZI I75) and get included --
        // Bluetooth keyboards are then weeded out by the GAP Appearance check below; attribution
        // uses exact senderID matching, so this never affects correctness.
        let is_pointer = IOHIDServiceClientConformsTo(service, 1, USAGE_GD_POINTER as u32) != 0
            || IOHIDServiceClientConformsTo(service, 1, USAGE_GD_MOUSE as u32) != 0
            || IOHIDServiceClientConformsTo(service, 1, USAGE_GD_TRACKPAD as u32) != 0;
        if !is_pointer {
            continue;
        }
        let vid = prop_int(service, KEY_VENDOR_ID).unwrap_or(0) as u32;
        let pid = prop_int(service, KEY_PRODUCT_ID).unwrap_or(0) as u32;
        let name = prop_string(service, KEY_PRODUCT);
        let transport = prop_string(service, KEY_TRANSPORT);

        // 蓝牙设备按 GAP Appearance 排除键盘:一些键盘固件在 HID 描述符里虚报鼠标用途
        // (如 KZI I75),ConformsTo 判定会误收;Appearance 是设备在蓝牙广播里自报的类别,
        // 与 macOS 蓝牙面板同源(0x03C1 = 键盘)。DeviceAddress 只在蓝牙传输设备上存在;
        // 不在 NVRAM 缓存里的设备(如新配对尚未缓存)回退 HID 判定,不会误杀。
        // Exclude Bluetooth keyboards by GAP Appearance: some keyboard firmware fakes mouse
        // usages in the HID descriptor (e.g. KZI I75), fooling the ConformsTo filter;
        // Appearance is the self-reported class in the Bluetooth advertisement, the same
        // source as the macOS Bluetooth pane (0x03C1 = keyboard). DeviceAddress exists only
        // on Bluetooth-transport devices; devices absent from the NVRAM cache (e.g. freshly
        // paired) fall back to HID-only classification and are never false-positively dropped.
        let addr = prop_string(service, KEY_DEVICE_ADDRESS);
        let is_bt_keyboard = !addr.is_empty()
            && appearance_map.get(&addr.to_uppercase()) == Some(&GAP_APPEARANCE_KEYBOARD);
        if is_bt_keyboard {
            log_info!(
                "[device] excluding '{}' (Bluetooth GAP appearance = keyboard)",
                name
            );
            continue;
        }

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
    {
        let reg = registry().lock().unwrap();
        if reg.devices.is_empty() {
            // 枚举返回 0 但设备可能在场(陈旧 client / 异步匹配竞态):重建 client 重试,
            // 重建路径自带 30ms settle(见 enumerate_locked)。重试仍为空才认作真无设备。
            // Enumeration came back empty while devices may be present (stale client or the
            // async-matching race): retry with a rebuilt client (which carries the 30ms settle
            // in enumerate_locked). Only give up after the retries still return empty.
            drop(reg);
            for _ in 0..2 {
                std::thread::sleep(std::time::Duration::from_millis(50));
                let mut reg = registry().lock().unwrap();
                unsafe { enumerate_locked(&mut reg, true) };
                if !reg.devices.is_empty() {
                    break;
                }
            }
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

/// 插拔回调防抖:启动时 IOHIDManager 会对在场设备触发一连串 matching 回调,而每次处理
/// (重建 client + 指针重应用)又会反向触发下一次回调,形成 6-7 连发的反馈循环 —— 既
/// 刷日志噪音,又拉长启动繁忙窗口(macOS 对繁忙期未及时服务的 tap 会自动禁用,见
/// event_tap.rs 的看门狗)。500ms 内只处理第一次;被跳过的回调不会直接丢弃,而是
/// 调度一次延迟重查(schedule_recheck),保证 BLE 快速休眠-唤醒(断连与重连的间隔
/// 常常小于 500ms)不会被永久吞掉——设备移除能及时反映,重连却丢了就是这个问题。
///
/// Plug-callback debounce: at startup IOHIDManager fires a burst of matching callbacks for
/// on-screen devices, and each handling (client rebuild + pointer re-apply) triggers the next
/// callback -- a 6-7 round feedback loop that spams the log and lengthens the busy startup
/// window (macOS auto-disables taps that don't service events during that window; see the
/// watchdog in event_tap.rs). Only the first callback within 500ms is processed; the skipped
/// ones are not dropped -- they schedule a delayed recheck (schedule_recheck), so fast BLE
/// sleep-wake cycles (often well under 500ms between disconnect and reconnect) are never
/// permanently swallowed -- a device removal shows up promptly while its re-add was being
/// lost, which is exactly this bug.
static LAST_PLUG_HANDLE: Mutex<Option<std::time::Instant>> = Mutex::new(None);
const PLUG_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(500);

/// 最近一次已处理的插拔事件是否为 removal。removal 后紧跟的 matching 事件是 BLE
/// 休眠重连的典型模式:此时旧 client 的缓存已随连接失效(见 device_from_cgevent 的
/// 失败路径),便宜 diff 不可靠,必须强制完整重建。
/// Whether the last processed plug event was a removal. A matching event right after a
/// removal is the typical BLE sleep-wake pattern: the old client's cache is dead by then
/// (see the failure path in device_from_cgevent), so the cheap diff is unreliable and a
/// full rebuild must be forced.
static LAST_PROCESSED_REMOVAL: Mutex<bool> = Mutex::new(false);

/// 延迟重查的一次性调度状态:至多一个重查线程在途;force 标记"removal 后紧跟
/// matching"事件被防抖吞掉时强制完整重建(见 run_recheck)。
/// One-shot scheduling state for the delayed recheck: at most one recheck thread in flight;
/// the force flag demands a full rebuild when a removal-followed-by-matching pair was
/// swallowed by the debounce (see run_recheck).
static DEFERRED_RECHECK_PENDING: AtomicBool = AtomicBool::new(false);
static DEFERRED_RECHECK_FORCE: AtomicBool = AtomicBool::new(false);
/// 延迟窗口:被防抖吞掉的事件在 700ms 后复查,此时 BLE HID 服务通常已完成重注册。
/// Delayed window: debounced events are re-checked after 700ms, by which time the BLE HID
/// service has usually finished re-registering.
const RECHECK_DELAY: std::time::Duration = std::time::Duration::from_millis(700);

/// 通知设置界面刷新设备下拉:经 controller 的 handleDevicesChanged: 转到主线程执行
/// (见 main.rs 的 on_devices_changed)。插拔回调、延迟重查、归因自愈三处共用。
/// 窗口未打开时该通知是 no-op(下次打开时 load_settings_values 仍会重建)。
///
/// Notify the settings UI to refresh the device popup: hop to the main thread via the
/// controller's handleDevicesChanged: (see on_devices_changed in main.rs). Shared by the
/// plug callback, the delayed recheck, and the attribution self-heal. No-op when the
/// window is closed (it is rebuilt on next open via load_settings_values anyway).
fn notify_devices_changed() {
    if let Some(ctrl) = crate::CONTROLLER.lock().unwrap().map(|c| c.0) {
        unsafe {
            let _: () = msg_send![ctrl,
                performSelectorOnMainThread: sel!(handleDevicesChanged:),
                withObject: std::ptr::null::<AnyObject>(),
                waitUntilDone: false
            ];
        }
    }
}

/// 调度延迟重查:合并防抖窗口内的多次事件,至多一个重查线程在途。
/// force_rebuild = true 时,即使便宜 diff 显示设备集未变也强制完整重建。
///
/// Schedule the delayed recheck: coalesce multiple events inside the debounce window, at
/// most one recheck thread in flight. With force_rebuild = true the recheck rebuilds fully
/// even if the cheap diff shows an unchanged device set.
fn schedule_recheck(force_rebuild: bool) {
    if force_rebuild {
        DEFERRED_RECHECK_FORCE.store(true, Ordering::SeqCst);
    }
    if !DEFERRED_RECHECK_PENDING.swap(true, Ordering::SeqCst) {
        std::thread::spawn(|| {
            std::thread::sleep(RECHECK_DELAY);
            DEFERRED_RECHECK_PENDING.store(false, Ordering::SeqCst);
            let force = DEFERRED_RECHECK_FORCE.swap(false, Ordering::SeqCst);
            run_recheck(force);
        });
    }
}

/// 用现有 client 廉价枚举当前设备集(不重建、不等待匹配),仅供重查 diff 使用。
/// 不做蓝牙键盘排除(NVRAM 读取成本高,且 diff 只关心集合是否变化——键盘重连
/// 触发一次重建是安全的,enumerate_locked 仍会把它排除)。
///
/// Cheaply enumerate the current device set with the existing client (no rebuild, no
/// matching wait); used only by the recheck diff. Bluetooth keyboard exclusion is skipped
/// (NVRAM read is costly, and the diff only cares whether the set changed -- a keyboard
/// reconnect triggering one rebuild is harmless, enumerate_locked still excludes it).
unsafe fn enumerate_keys(client: *mut c_void) -> Vec<DeviceKey> {
    let mut keys = Vec::new();
    if client.is_null() {
        return keys;
    }
    let services = IOHIDEventSystemClientCopyServices(client);
    if services.is_null() {
        return keys;
    }
    let count = CFArrayGetCount(services);
    for i in 0..count {
        let service = CFArrayGetValueAtIndex(services, i) as *mut c_void;
        let is_pointer = IOHIDServiceClientConformsTo(service, 1, USAGE_GD_POINTER as u32) != 0
            || IOHIDServiceClientConformsTo(service, 1, USAGE_GD_MOUSE as u32) != 0
            || IOHIDServiceClientConformsTo(service, 1, USAGE_GD_TRACKPAD as u32) != 0;
        if !is_pointer {
            continue;
        }
        let vid = prop_int(service, KEY_VENDOR_ID).unwrap_or(0) as u32;
        let pid = prop_int(service, KEY_PRODUCT_ID).unwrap_or(0) as u32;
        keys.push((vid, pid));
    }
    CFRelease(services as *const c_void);
    keys
}

/// 延迟重查:防抖窗口内被跳过的插拔事件在这里补处理。先用现有 client 做便宜 diff,
/// 只有设备集真变化(或 force)才重建 client + 重应用指针设置 + 刷新设置 UI。
/// 启动时连发回调的 700ms 后复查会因设备集无变化而安静结束,不会重蹈反馈循环;
/// BLE 重连场景则在此刻设备已完成 HID 注册,重建能拿到完整列表。
///
/// Delayed recheck: plug events skipped by the debounce window are handled here. A cheap
/// diff with the existing client runs first; only a real device-set change (or force)
/// triggers a client rebuild + pointer re-apply + settings-UI refresh. At startup the
/// 700ms-after-the-burst recheck finds an unchanged set and quietly exits, so the feedback
/// loop does not return; for BLE reconnects the device has finished HID registration by
/// then, and the rebuild gets the complete list.
fn run_recheck(force_rebuild: bool) {
    let changed = if force_rebuild {
        true
    } else {
        let reg = registry().lock().unwrap();
        if reg.client.is_null() {
            return;
        }
        let before: Vec<DeviceKey> = reg.devices.iter().map(|d| d.key()).collect();
        let now = unsafe { enumerate_keys(reg.client) };
        before != now
    };
    if !changed {
        log_debug!("[device] delayed recheck: device set unchanged, skipping.");
        return;
    }
    {
        let mut reg = registry().lock().unwrap();
        unsafe { enumerate_locked(&mut reg, true) };
        log_debug!(
            "[device] delayed recheck: re-enumerated {} device(s).",
            reg.devices.len()
        );
    }
    // 释放 REGISTRY 锁后再 apply(pointer::apply 自建 client、不锁 REGISTRY;幂等可重复调用)。
    // Drop the REGISTRY lock before applying (pointer::apply creates its own client and never
    // locks REGISTRY; idempotent, safe to call repeatedly).
    crate::mouse::pointer::apply();
    notify_devices_changed();
}

/// IOHIDManager 回调:设备接入/移除时,强制重建注册表(重建 IOHIDEventSystemClient)。
/// 蓝牙鼠标休眠断连重连正是走这里——重连触发 removal+matching 回调,重建后归因链恢复,
/// 不必等下一次归因失败才自愈。重建后还要重新应用指针加速设置:加速度属性写在旧的
/// IOHIDServiceClient 实例上,断连后随实例消失,新实例恢复默认(加速度重新生效),必须
/// 在重连时重新 apply,否则"禁用指针加速"失效,只能靠用户进设置手动点一次恢复。
/// 被防抖吞掉的回调调度延迟重查兜底(见 LAST_PLUG_HANDLE 注释)。
///
/// IOHIDManager callback: on device attach/detach, force-rebuild the registry (recreate the
/// IOHIDEventSystemClient). A Bluetooth disconnect/reconnect fires these callbacks; the rebuild
/// restores the attribution chain without waiting for the next failed attribution. Pointer
/// acceleration must also be re-applied: the acceleration properties live on the old
/// IOHIDServiceClient instance, which disappears on disconnect; the new instance reverts to
/// defaults (acceleration back on). Without re-applying on reconnect, "disable pointer
/// acceleration" silently breaks until the user re-toggles it in Settings. Callbacks
/// swallowed by the debounce are covered by the delayed recheck (see LAST_PLUG_HANDLE).
unsafe extern "C" fn device_change_callback(
    _context: *mut c_void,
    _result: i32,
    _sender: *mut c_void,
    _callback: *mut c_void,
    is_removal: bool,
) {
    // 防抖(见 LAST_PLUG_HANDLE 注释):500ms 内的重复插拔回调只处理第一次。
    // Debounce (see LAST_PLUG_HANDLE): only the first plug callback within 500ms is handled.
    let debounced = {
        let mut last = LAST_PLUG_HANDLE.lock().unwrap();
        if last.is_some_and(|t| t.elapsed() < PLUG_DEBOUNCE) {
            true
        } else {
            *last = Some(std::time::Instant::now());
            false
        }
    };
    if debounced {
        // 被防抖吞掉:调度延迟重查。removal 后紧跟 matching = BLE 重连,旧 client
        // 缓存已失效,标记强制重建(便宜 diff 不可靠)。
        // Swallowed by the debounce: schedule a delayed recheck. A matching event right
        // after a removal is a BLE reconnect; the old client's cache is dead, so force a
        // full rebuild (the cheap diff would be unreliable).
        let last_removal = *LAST_PROCESSED_REMOVAL.lock().unwrap();
        schedule_recheck(!is_removal && last_removal);
        return;
    }
    *LAST_PROCESSED_REMOVAL.lock().unwrap() = is_removal;
    {
        let mut reg = registry().lock().unwrap();
        enumerate_locked(&mut reg, true);
        log_debug!("[device] plug/unplug event: re-enumerated {} device(s).", reg.devices.len());
    }
    // 释放 REGISTRY 锁后再 apply(pointer::apply 自建 client、不锁 REGISTRY,但避免
    // 持锁期间做重活)。apply 内部检查 mouse.enabled,未启用时自动跳过;幂等可重复调用。
    // Drop the REGISTRY lock before applying (pointer::apply creates its own client and never
    // locks REGISTRY, but avoid doing heavy work while holding the lock). apply checks
    // mouse.enabled internally and skips when disabled; idempotent, safe to call repeatedly.
    crate::mouse::pointer::apply();
    // 设置窗口开着时即时刷新设备下拉:回调在鼠标线程,经 controller 的
    // handleDevicesChanged: 转到主线程执行(见 main.rs 的 on_devices_changed)。
    // Refresh the settings device popup live when the window is open: this callback runs on
    // the mouse thread, so hop to main via the controller's handleDevicesChanged:
    // (see on_devices_changed in main.rs).
    notify_devices_changed();
    // 已处理的事件也调度一次延迟重查:重建后 30ms settle 可能漏掉刚重连的设备
    // (异步匹配竞态,见 enumerate_locked),700ms 后设备已完成注册,diff 检出变化即
    // 补齐。启动连发场景下 diff 无变化,安静结束,不会重启反馈循环。
    // Also schedule a delayed recheck after processed events: the 30ms settle after a
    // rebuild can miss a just-reconnected device (async-matching race, see enumerate_locked);
    // by 700ms the device has registered, and the diff picks up the change. In the startup
    // burst the diff finds no change and quietly exits, so the feedback loop stays broken.
    schedule_recheck(false);
}

/// IOHIDManager 接入(matching)回调包装:带方向标记进入统一处理。
/// Wrapper for IOHIDManager matching (attach) callbacks, entering the shared handler with
/// the direction flag.
unsafe extern "C" fn device_matching_callback(
    context: *mut c_void,
    result: i32,
    sender: *mut c_void,
    callback: *mut c_void,
) {
    device_change_callback(context, result, sender, callback, false);
}

/// IOHIDManager 移除(removal)回调包装:带方向标记进入统一处理。
/// Wrapper for IOHIDManager removal (detach) callbacks, entering the shared handler with
/// the direction flag.
unsafe extern "C" fn device_removal_callback(
    context: *mut c_void,
    result: i32,
    sender: *mut c_void,
    callback: *mut c_void,
) {
    device_change_callback(context, result, sender, callback, true);
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
    // 记录鼠标线程 runloop,供 enumerate_locked 调度注册表 client(归因可靠性)。
    // Record the mouse thread's runloop for enumerate_locked to schedule the registry client
    // (attribution reliability).
    *mouse_runloop_static().lock().unwrap() = Some(runloop);
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

    // 接入与移除分开注册,以区分事件方向(removal 后紧跟 matching = BLE 重连,
    // 防抖吞掉时触发强制重建,见 LAST_PROCESSED_REMOVAL 与 schedule_recheck)。
    // Register attach and removal separately so the event direction is known (a matching
    // event right after a removal is a BLE reconnect; if debounced it forces a full
    // rebuild, see LAST_PROCESSED_REMOVAL and schedule_recheck).
    let matching_cb: Option<unsafe extern "C" fn(*mut c_void, i32, *mut c_void, *mut c_void)> =
        Some(device_matching_callback as unsafe extern "C" fn(*mut c_void, i32, *mut c_void, *mut c_void));
    let removal_cb: Option<unsafe extern "C" fn(*mut c_void, i32, *mut c_void, *mut c_void)> =
        Some(device_removal_callback as unsafe extern "C" fn(*mut c_void, i32, *mut c_void, *mut c_void));
    IOHIDManagerRegisterDeviceMatchingCallback(manager_obj, matching_cb, std::ptr::null_mut());
    IOHIDManagerRegisterDeviceRemovalCallback(manager_obj, removal_cb, std::ptr::null_mut());
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
            let before: Vec<DeviceKey> = reg.devices.iter().map(|d| d.key()).collect();
            enumerate_locked(&mut reg, true);
            let (key, set_changed) = if let Some(idx) = lookup_service_index(&reg, sender) {
                let dev = &reg.devices[idx];
                *LAST_ACTIVE_KEY.lock().unwrap() = Some(dev.key());
                let after: Vec<DeviceKey> = reg.devices.iter().map(|d| d.key()).collect();
                (Some(dev.key()), before != after)
            } else {
                (None, false)
            };
            drop(reg);
            // 注册表在此自愈(重连设备找回):若设备集确实变化,补一次 apply + 设置 UI
            // 刷新——防抖/延迟重查漏检时的兜底,用户一动鼠标下拉立即恢复。每次恢复
            // 至多触发一次(后续事件命中首查路径)。此路径罕见(miss 才走)。
            // The registry self-healed here (reconnected device recovered): if the device set
            // actually changed, re-apply pointer settings and refresh the settings UI -- the
            // backstop when the debounce/delayed recheck missed, so the popup recovers as soon
            // as the user moves the mouse. At most one trigger per recovery (later events hit
            // the first-lookup path). This path is rare (only runs on a miss).
            if set_changed {
                crate::mouse::pointer::apply();
                notify_devices_changed();
            }
            if let Some(k) = key {
                return Some(k);
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
