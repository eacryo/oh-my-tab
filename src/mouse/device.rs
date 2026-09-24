//!   CGEventCopyIOHIDEvent(cgEvent) -> IOHIDEventRef
//!   IOHIDEventGetSenderID(ioHIDEvent) -> registry_id (uint64)
//!
//! Live device registry + CGEvent -> producing-device attribution chain (Phase 0+1).
//!
//! Attribution chain (private SPI, mirroring LinearMouse):
//!   CGEventCopyIOHIDEvent(cgEvent) -> IOHIDEventRef
//!   IOHIDEventGetSenderID(ioHIDEvent) -> registry_id (uint64)
//!   by_registry_id lookup -> Device
//! On attribution misses, schedule a full background enumeration and fall back to last_active
//! (which matches the "All Mice" profile).

use crate::ffi::{make_nsstring, nsstring_to_rust, CFRelease};
use crate::mouse::ffi::*;
use crate::{log_debug, log_info};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send, sel};
use std::collections::HashMap;
use std::ffi::{c_void, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};

/// Device hardware identity (config matches on VID+PID; name/transport for display only).
#[derive(Debug, Clone)]
pub(crate) struct DeviceIdentity {
    pub(crate) vendor_id: u32,
    pub(crate) product_id: u32,
    pub(crate) name: String,
    #[allow(dead_code)]
    pub(crate) transport: String,
}

/// Runtime device handle. Hardware identity lives only in DeviceIdentity; the service_client
/// pointer from enumeration is kept alive by the services array (used for CFEqual during
/// attribution).
pub(crate) struct Device {
    pub(crate) identity: DeviceIdentity,
    /// The IOHIDServiceClient pointer from enumeration (kept alive by the services array;
    /// used for CFEqual comparison during attribution).
    service_client: *mut c_void,
}

unsafe impl Send for Device {}
unsafe impl Sync for Device {}

/// (VID, PID) pair, used as the key for the resolve cache and per-device state.
pub(crate) type DeviceKey = (u32, u32);

impl Device {
    pub(crate) fn key(&self) -> DeviceKey {
        (self.identity.vendor_id, self.identity.product_id)
    }
}

/// Reserved key for the virtual pointer. No real device can produce it (VID/PID come from device
/// properties and never pair up as u32::MAX), so DeviceKey can stay a plain (u32, u32) instead of
/// becoming an enum and rippling through the resolve cache, the device popup and last_active.
pub(crate) const VIRTUAL_DEVICE_KEY: DeviceKey = (u32::MAX, u32::MAX);

/// The injecting process detected so far (a software KVM's virtual pointer, e.g. Deskflow). Written
/// by the mouse thread during attribution; the settings UI reads it to decide whether the virtual
/// mouse entry appears in the device popup -- it is hidden while no injector is around.
static INJECTOR_PID: Mutex<Option<i32>> = Mutex::new(None);
static INJECTOR_NOTICE_WORKER: OnceLock<Option<Sender<i32>>> = OnceLock::new();

fn injector_notice_sender() -> Option<&'static Sender<i32>> {
    INJECTOR_NOTICE_WORKER
        .get_or_init(|| {
            let (sender, receiver) = mpsc::channel::<i32>();
            std::thread::Builder::new()
                .name("mouse-injector-notice".into())
                .spawn(move || {
                    while let Ok(pid) = receiver.recv() {
                        let name = unsafe { injector_display_name(pid) }.unwrap_or_default();
                        if *INJECTOR_PID.lock().unwrap() != Some(pid) {
                            continue;
                        }
                        log_debug!(
                            "[device] virtual pointer detected: pid={} name={:?}; device picker now offers its own profile.",
                            pid,
                            name
                        );
                        notify_devices_changed();
                    }
                })
                .ok()
                .map(|_| sender)
        })
        .as_ref()
}

/// Whether the event was injected by another process, returning that process's pid.
/// 0 = hardware; our own pid is excluded too (our own synthetic events are not a virtual mouse).
pub(crate) fn injected_source_pid(cg_event: crate::event_tap::CGEventRef) -> Option<i32> {
    let pid = unsafe {
        crate::event_tap::CGEventGetIntegerValueField(
            cg_event,
            crate::event_tap::K_CG_EVENT_SOURCE_UNIX_PROCESS_ID,
        )
    } as i32;
    if pid == 0 || pid == std::process::id() as i32 {
        None
    } else {
        Some(pid)
    }
}

// libproc: whether a pid exists, and its executable path. NSRunningApplication only knows GUI
// apps registered with the window server (measured: a command-line process is not found), while an
// injector can be any process -- so liveness goes through libproc.
#[link(name = "System", kind = "dylib")]
extern "C" {
    fn proc_pidpath(pid: i32, buffer: *mut c_void, buffersize: u32) -> i32;
}

/// The process's executable path; None = the process does not exist (the liveness check).
unsafe fn process_path(pid: i32) -> Option<String> {
    // PROC_PIDPATHINFO_MAXSIZE == 4096.
    let mut buf = [0u8; 4096];
    let len = proc_pidpath(pid, buf.as_mut_ptr() as *mut c_void, buf.len() as u32);
    if len <= 0 {
        return None;
    }
    let bytes = &buf[..len as usize];
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    Some(String::from_utf8_lossy(&bytes[..end]).into_owned())
}

/// The injector's display name: the GUI app's localizedName (e.g. "Deskflow") when available,
/// otherwise the executable's file name. None = the process no longer exists.
unsafe fn injector_display_name(pid: i32) -> Option<String> {
    let path = process_path(pid)?;
    let friendly = running_app_name(pid).unwrap_or_default();
    if !friendly.is_empty() {
        return Some(friendly);
    }
    Some(path.rsplit('/').next().unwrap_or(path.as_str()).to_string())
}

/// A GUI app's localizedName; a non-GUI process (no bundle) yields an empty string or None.
unsafe fn running_app_name(pid: i32) -> Option<String> {
    // NSRunningApplication and localizedName are autoreleased, and attribution reaches this from
    // the mouse thread (when record_injector logs), so drain them in a pool -- the same
    // "background thread + autoreleasepool" precedent as icon caching / window collection.
    let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
    let app: *mut AnyObject = msg_send![
        class!(NSRunningApplication),
        runningApplicationWithProcessIdentifier: pid
    ];
    let name = if app.is_null() {
        None
    } else {
        Some(crate::ffi::ns_running_app_name(app))
    };
    let _: () = msg_send![pool, drain];
    name
}

/// Record the injecting process. Only a **change** logs and notifies the settings UI: attribution
/// runs for every event, so doing it unconditionally would spam the log and cross thread hops.
fn record_injector(pid: i32) {
    {
        let mut cur = INJECTOR_PID.lock().unwrap();
        if *cur == Some(pid) {
            return;
        }
        *cur = Some(pid);
    }
    // Name lookup may cross into NSRunningApplication/libproc; leave it to the worker so injected
    // events never make the HID callback wait on process discovery or a main-thread notification.
    if let Some(sender) = injector_notice_sender() {
        let _ = sender.send(pid);
    }
}

/// The virtual pointer's device identity, present only while the injector is alive; a dead
/// injector is cleared here so the popup entry disappears. Called by the settings UI on the main
/// thread (on open/refresh), which is the convenient place for the liveness check and the name.
pub(crate) fn virtual_device_identity() -> Option<DeviceIdentity> {
    let pid = (*INJECTOR_PID.lock().unwrap())?;
    match unsafe { injector_display_name(pid) } {
        Some(name) => Some(DeviceIdentity {
            vendor_id: VIRTUAL_DEVICE_KEY.0,
            product_id: VIRTUAL_DEVICE_KEY.1,
            name,
            // An injected pointer has no transport (neither USB nor Bluetooth); unused for display.
            transport: String::new(),
        }),
        None => {
            log_debug!(
                "[device] virtual pointer gone: injecting pid={} exited; dropping its picker entry.",
                pid
            );
            *INJECTOR_PID.lock().unwrap() = None;
            None
        }
    }
}

struct DeviceRegistry {
    /// Holds the event system client (keeps service clients alive).
    client: *mut c_void,
    /// Holds the services CFArray (keeping its IOHIDServiceClients alive).
    services: *mut c_void,
    devices: Vec<Device>,
}

unsafe impl Send for DeviceRegistry {}
unsafe impl Sync for DeviceRegistry {}

static REGISTRY: OnceLock<Mutex<DeviceRegistry>> = OnceLock::new();

/// The mouse thread's CFRunLoop (recorded by start_plug_monitor). The registry client must be
/// scheduled on it for matching to stay live (see enumerate_locked); enumeration can run before
/// the plug monitor starts, so the value is read at runtime, not passed in once at startup.
/// The raw pointer is wrapped with Send+Sync (same pattern as ManagerMutex).
struct RunloopMutex(Mutex<Option<crate::event_tap::CFRunLoopRef>>);
unsafe impl Send for RunloopMutex {}
unsafe impl Sync for RunloopMutex {}

static MOUSE_RUNLOOP: OnceLock<RunloopMutex> = OnceLock::new();

fn mouse_runloop_static() -> &'static Mutex<Option<crate::event_tap::CFRunLoopRef>> {
    &MOUSE_RUNLOOP
        .get_or_init(|| RunloopMutex(Mutex::new(None)))
        .0
}

/// Last-active device's (VID, PID) (fallback when attribution fails). Updated by the event_tap
/// callback after each successful attribution. Stored by hardware identity, not the process-local
/// id: the id drifts on re-enumeration (NEXT_ID is monotonic), while VID/PID are stable across a
/// Bluetooth disconnect/reconnect, so the hardware identity is the reliable fallback.
static LAST_ACTIVE_KEY: Mutex<Option<DeviceKey>> = Mutex::new(None);

fn registry() -> &'static Mutex<DeviceRegistry> {
    REGISTRY.get_or_init(|| {
        Mutex::new(DeviceRegistry {
            client: std::ptr::null_mut(),
            services: std::ptr::null_mut(),
            devices: Vec::new(),
        })
    })
}

// Mirrors the pointer.rs pattern; read-only here, so it stays simpler.

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

/// Read bluetoothd's BluetoothInfo cache from NVRAM, parsing an address -> GAP Appearance
/// map. Appearance is the device's self-reported class in its Bluetooth advertisement
/// (0x03C1 keyboard / 0x03C2 mouse), the same source macOS's Bluetooth pane icons use.
/// HID descriptors are unreliable here (some keyboard firmware fakes mouse usages, e.g.
/// KZI I75), so this table performs the precise classification. On failure / missing cache
/// an empty map is returned and the caller falls back to HID-only classification.
fn bluetooth_appearance_map() -> HashMap<String, u16> {
    let mut map = HashMap::new();
    unsafe {
        // IORegistryEntryFromPath returns +1; IOObjectRelease when done.
        let path = match CString::new(IOSERVICE_OPTIONS_PATH) {
            Ok(p) => p,
            Err(_) => return map,
        };
        let entry = IORegistryEntryFromPath(0, path.as_ptr());
        if entry == 0 {
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
        // Parse before releasing props (CFDataGetBytePtr borrows data owned by props).
        if !ptr.is_null() && len > 0 {
            parse_bluetooth_info(std::slice::from_raw_parts(ptr, len), &mut map);
        }
        CFRelease(props as *const c_void);
    }
    map
}

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

/// Enumerate currently-connected mouse/trackpad devices and populate the registry.
/// Caller holds the REGISTRY lock. With rebuild_client = true, the IOHIDEventSystemClient is
/// forcibly recreated (after a Bluetooth disconnect/reconnect the old client's registry cache is
/// stale, breaking the attribution chain -- see the failure path in device_from_cgevent).
unsafe fn enumerate_locked(reg: &mut DeviceRegistry, rebuild_client: bool) {
    // Release the previous services array (if any).
    if !reg.services.is_null() {
        CFRelease(reg.services as *const c_void);
        reg.services = std::ptr::null_mut();
    }
    if rebuild_client {
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
        // Match the Generic Desktop page (filtered by usage below).
        let page_key = make_nsstring(KEY_PRIMARY_USAGE_PAGE);
        let page_val: *mut AnyObject =
            msg_send![class!(NSNumber), numberWithInt: USAGE_PAGE_GENERIC_DESKTOP as i32];
        let dict: *mut AnyObject =
            msg_send![class!(NSDictionary), dictionaryWithObject: page_val, forKey: page_key];
        let arr: *mut AnyObject = msg_send![class!(NSArray), arrayWithObject: dict];
        IOHIDEventSystemClientSetMatchingMultiple(reg.client, arr as *const c_void);
        CFRelease(page_key as *const c_void);
        // Matching on IOHIDEventSystemClient is asynchronous: CopyServices right after
        // creation/rebuild can return an empty list (measured 0/1 flapping). Waiting ~30ms
        // lets matching settle; only the fresh-client path has this race, so the hot
        // attribution path (reusing an old client) is unaffected.
        std::thread::sleep(std::time::Duration::from_millis(30));
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
        log_debug!("[device] no services returned");
        reg.devices.clear();
        return;
    }
    reg.services = services;

    reg.devices.clear();

    // Bluetooth classification table (bluetoothd's BluetoothInfo cache in NVRAM). On parse
    // failure it is empty and every Bluetooth device falls back to HID-only classification,
    // preserving the previous behavior.
    let appearance_map = bluetooth_appearance_map();

    let count = CFArrayGetCount(services);
    for i in 0..count {
        let service = CFArrayGetValueAtIndex(services, i) as *mut c_void;
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
            log_debug!(
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

/// Enumerate once at startup (also lazily triggered on first attribution failure).
pub(crate) fn ensure_enumerated() {
    let _ = injector_notice_sender();
    let mut reg = registry().lock().unwrap();
    if reg.client.is_null() || reg.devices.is_empty() {
        unsafe { enumerate_locked(&mut reg, false) };
    }
}

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
    let mut out: Vec<DeviceIdentity> = {
        let reg = registry().lock().unwrap();
        reg.devices.iter().map(|d| d.identity.clone()).collect()
    };
    // The virtual pointer (the injecting process) comes last: it is not a HID device and only
    // appears after an injected event has been seen while that process is still alive, so the
    // picker's entry count follows whether a software KVM is running (see virtual_device_identity).
    if let Some(virtual_pointer) = virtual_device_identity() {
        out.push(virtual_pointer);
    }
    out
}

/// Read a device's current integer property (through the service client kept alive by the
/// registry). None when no device with that VID/PID is present or the property doesn't exist.
/// The registry lock is held throughout: the service client is kept alive by reg.services, so
/// a plug/unplug rebuild can never leave it dangling.
pub(crate) fn device_int_property(key: DeviceKey, property: &str) -> Option<i64> {
    {
        let reg = registry().lock().unwrap();
        if reg.devices.is_empty() {
            drop(reg);
            ensure_enumerated();
        }
    }
    let reg = registry().lock().unwrap();
    let device = reg
        .devices
        .iter()
        .find(|d| (d.identity.vendor_id, d.identity.product_id) == key)?;
    unsafe { prop_int(device.service_client, property) }
}

/// Read a device's current string property (same keep-alive/locking rules as
/// `device_int_property`); empty strings and non-string properties both yield None.
pub(crate) fn device_string_property(key: DeviceKey, property: &str) -> Option<String> {
    {
        let reg = registry().lock().unwrap();
        if reg.devices.is_empty() {
            drop(reg);
            ensure_enumerated();
        }
    }
    let reg = registry().lock().unwrap();
    let device = reg
        .devices
        .iter()
        .find(|d| (d.identity.vendor_id, d.identity.product_id) == key)?;
    let s = unsafe { prop_string(device.service_client, property) };
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// IOHIDManager instance (kept alive: releasing it would invalidate the callbacks). Created by
/// start_plug_monitor, once. Wrapped Mutex with Send+Sync (same pattern as DeviceRegistry;
/// statics need Send+Sync).
struct ManagerMutex(Mutex<*mut c_void>);
unsafe impl Send for ManagerMutex {}
unsafe impl Sync for ManagerMutex {}

static MANAGER: OnceLock<ManagerMutex> = OnceLock::new();

fn manager_static() -> &'static Mutex<*mut c_void> {
    &MANAGER
        .get_or_init(|| ManagerMutex(Mutex::new(std::ptr::null_mut())))
        .0
}

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

/// Whether the last processed plug event was a removal. A matching event right after a
/// removal is the typical BLE sleep-wake pattern: the old client's cache is dead by then
/// (see the failure path in device_from_cgevent), so the cheap diff is unreliable and a
/// full rebuild must be forced.
static LAST_PROCESSED_REMOVAL: Mutex<bool> = Mutex::new(false);

/// One-shot scheduling state for the delayed recheck: at most one recheck thread in flight;
/// the force flag demands a full rebuild when a removal-followed-by-matching pair was
/// swallowed by the debounce (see run_recheck).
static DEFERRED_RECHECK_PENDING: AtomicBool = AtomicBool::new(false);
static DEFERRED_RECHECK_FORCE: AtomicBool = AtomicBool::new(false);
/// Delayed window: debounced events are re-checked after 700ms, by which time the BLE HID
/// service has usually finished re-registering.
const RECHECK_DELAY: std::time::Duration = std::time::Duration::from_millis(700);

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
    // Drop the REGISTRY lock before applying (pointer::apply creates its own client and never
    // locks REGISTRY; idempotent, safe to call repeatedly).
    crate::mouse::pointer::apply();
    notify_devices_changed();
}

/// IOHIDManager callback: on device attach/detach, force-rebuild the registry (recreate the
/// IOHIDEventSystemClient). A Bluetooth disconnect/reconnect fires these callbacks; the rebuild
/// restores the attribution chain without waiting for the next failed attribution. Pointer
/// acceleration must also be re-applied: the acceleration properties live on the old
/// IOHIDServiceClient instance, which disappears on disconnect; the new instance reverts to
/// defaults (acceleration back on). Without re-applying on reconnect, "disable pointer
/// acceleration" silently breaks until the user re-toggles it in Settings. Callbacks
/// swallowed by the debounce are covered by the delayed recheck (see LAST_PLUG_HANDLE).
unsafe extern "C" fn device_change_callback(
    context: *mut c_void,
    result: i32,
    sender: *mut c_void,
    callback: *mut c_void,
    is_removal: bool,
) {
    crate::callback_guard::void("device_change_callback", || unsafe {
        device_change_callback_inner(context, result, sender, callback, is_removal);
    });
}

unsafe fn device_change_callback_inner(
    _context: *mut c_void,
    _result: i32,
    _sender: *mut c_void,
    _callback: *mut c_void,
    is_removal: bool,
) {
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
        log_debug!(
            "[device] plug/unplug event: re-enumerated {} device(s).",
            reg.devices.len()
        );
    }
    // Drop the REGISTRY lock before applying (pointer::apply creates its own client and never
    // locks REGISTRY, but avoid doing heavy work while holding the lock). apply checks
    // mouse.enabled internally and skips when disabled; idempotent, safe to call repeatedly.
    crate::mouse::pointer::apply();
    // Refresh the settings device popup live when the window is open: this callback runs on
    // the mouse thread, so hop to main via the controller's handleDevicesChanged:
    // (see on_devices_changed in main.rs).
    notify_devices_changed();
    // Also schedule a delayed recheck after processed events: the 30ms settle after a
    // rebuild can miss a just-reconnected device (async-matching race, see enumerate_locked);
    // by 700ms the device has registered, and the diff picks up the change. In the startup
    // burst the diff finds no change and quietly exits, so the feedback loop stays broken.
    schedule_recheck(false);
}

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
    // Record the mouse thread's runloop for enumerate_locked to schedule the registry client
    // (attribution reliability).
    *mouse_runloop_static().lock().unwrap() = Some(runloop);
    let manager_obj = IOHIDManagerCreate(std::ptr::null(), 0);
    if manager_obj.is_null() {
        log_info!("[device] failed to create IOHIDManager");
        return;
    }
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

    // Register attach and removal separately so the event direction is known (a matching
    // event right after a removal is a BLE reconnect; if debounced it forces a full
    // rebuild, see LAST_PROCESSED_REMOVAL and schedule_recheck).
    let matching_cb: Option<unsafe extern "C" fn(*mut c_void, i32, *mut c_void, *mut c_void)> =
        Some(
            device_matching_callback
                as unsafe extern "C" fn(*mut c_void, i32, *mut c_void, *mut c_void),
        );
    let removal_cb: Option<unsafe extern "C" fn(*mut c_void, i32, *mut c_void, *mut c_void)> = Some(
        device_removal_callback as unsafe extern "C" fn(*mut c_void, i32, *mut c_void, *mut c_void),
    );
    IOHIDManagerRegisterDeviceMatchingCallback(manager_obj, matching_cb, std::ptr::null_mut());
    IOHIDManagerRegisterDeviceRemovalCallback(manager_obj, removal_cb, std::ptr::null_mut());
    IOHIDManagerScheduleWithRunLoop(
        manager_obj,
        runloop,
        crate::event_tap::kCFRunLoopDefaultMode,
    );
    *m = manager_obj;
    log_debug!("[device] plug/unplug monitor started.");
}

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
    // Device count is tiny (a few mice/trackpads); a linear CFEqual scan is fast enough.
    let idx = reg
        .devices
        .iter()
        .position(|d| crate::ffi::CFEqual(d.service_client, svc));
    // Copy returns +1; release immediately (the index is already taken; devices are kept
    // alive by the services array).
    CFRelease(svc as *const c_void);
    idx
}

/// Find the device that produced a CGEvent.
/// Chain: CGEventCopyIOHIDEvent -> IOHIDEventGetSenderID ->
/// IOHIDEventSystemClientCopyServiceForRegistryID -> CFEqual against the enumerated list.
/// On failure, asynchronously schedule a registry rebuild and return last_active
/// (or None if there is none, in which case the caller uses the "All Mice" profile).
pub(crate) fn device_from_cgevent(cg_event: crate::event_tap::CGEventRef) -> Option<DeviceKey> {
    // Injected-by-another-process (a software KVM's virtual pointer) first: such events carry no
    // IOHIDEvent sender but do carry the injecting pid, so they resolve to the dedicated virtual
    // profile rather than falling back to last_active -- otherwise the virtual pointer's scroll and
    // buttons would be billed to the physical mouse's profile. (Injected events never enter
    // LAST_ACTIVE_KEY: that is the fallback target for failed *hardware* attribution, and writing
    // the virtual profile there would drag physical button events onto it.)
    if let Some(pid) = injected_source_pid(cg_event) {
        record_injector(pid);
        return Some(VIRTUAL_DEVICE_KEY);
    }
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
        // First lookup.
        {
            let reg = registry().lock().unwrap();
            if let Some(idx) = lookup_service_index(&reg, sender) {
                let dev = &reg.devices[idx];
                *LAST_ACTIVE_KEY.lock().unwrap() = Some(dev.key());
                return Some(dev.key());
            }
        }
        // A miss may mean a new device or Bluetooth reconnect. Full enumeration rebuilds the
        // IOHID client and must stay off the HID callback; coalesce a background recheck and use
        // the last-active device for this event.
        schedule_recheck(true);
        last_active_key()
    }
}

/// Fallback: return the last-active device's (VID, PID), matched by hardware identity so
/// re-enumeration id drift doesn't break it. Mouse BUTTON events frequently carry no
/// IOHIDEvent sender (scroll events do), so attribution lands here; when the process hasn't
/// attributed anything yet (LAST_ACTIVE_KEY is None), a single-device registry is used
/// directly -- making button bindings reliable on single-mouse setups.
fn last_active_key() -> Option<DeviceKey> {
    let key = *LAST_ACTIVE_KEY.lock().unwrap();
    let reg = registry().lock().unwrap();
    if let Some(k) = key {
        if let Some(d) = reg.devices.iter().find(|d| d.key() == k) {
            return Some(d.key());
        }
    }
    // Single-device fallback (the registry holds only mice/trackpads; Bluetooth keyboards
    // are already excluded).
    if reg.devices.len() == 1 {
        return Some(reg.devices[0].key());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // TLV helper: tag, len, payload.
    fn tlv(tag: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![tag, payload.len() as u8];
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn virtual_device_identity_tracks_the_injector_lifecycle() {
        // A live injector -> an identity carrying the reserved key (the picker shows the
        // virtual-mouse entry from this).
        let live_pid = std::process::id() as i32;
        *INJECTOR_PID.lock().unwrap() = Some(live_pid);
        let identity = virtual_device_identity().expect("live injector yields an identity");
        assert_eq!(
            (identity.vendor_id, identity.product_id),
            VIRTUAL_DEVICE_KEY
        );

        // A dead process -> cleared, and None comes back (the picker entry disappears).
        // 999999 exceeds macOS's pid ceiling, so it can never exist.
        *INJECTOR_PID.lock().unwrap() = Some(999_999);
        assert!(virtual_device_identity().is_none());
        assert!(INJECTOR_PID.lock().unwrap().is_none());
    }

    #[test]
    fn injected_source_pid_tells_injected_from_hardware() {
        unsafe {
            let event =
                crate::event_tap::CGEventCreateScrollWheelEvent2(std::ptr::null(), 1, 1, 1, 0, 0);
            // Hardware event: field 41 = 0.
            assert_eq!(injected_source_pid(event), None);
            // Injected by another process: its pid comes back.
            crate::event_tap::CGEventSetIntegerValueField(
                event,
                crate::event_tap::K_CG_EVENT_SOURCE_UNIX_PROCESS_ID,
                4242,
            );
            assert_eq!(injected_source_pid(event), Some(4242));
            // Our own synthetic events are not a virtual pointer.
            crate::event_tap::CGEventSetIntegerValueField(
                event,
                crate::event_tap::K_CG_EVENT_SOURCE_UNIX_PROCESS_ID,
                std::process::id() as i64,
            );
            assert_eq!(injected_source_pid(event), None);
            crate::ffi::CFRelease(event as *const c_void);
        }
    }

    #[test]
    fn bt_info_pairs_address_with_appearance() {
        // Measured layout: 0x0e 7 bytes (1 flag + 6 address), 0x11 2 bytes LE appearance.
        let mut bytes = Vec::new();
        bytes.extend(tlv(0x02, b"Mouse"));
        bytes.extend(tlv(0x0e, &[0x01, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]));
        bytes.extend(tlv(0x11, &[0xC2, 0x03])); // mouse appearance
        let mut map = HashMap::new();
        parse_bluetooth_info(&bytes, &mut map);
        // Address formatted dashed-uppercase; appearance parsed little-endian.
        assert_eq!(map.get("AA-BB-CC-DD-EE-FF"), Some(&0x03C2));
    }

    #[test]
    fn bt_info_keyboard_appearance_is_distinguishable() {
        // Keyboard appearance 0x03C1 vs mouse 0x03C2 — the basis for keyboard exclusion.
        let mut bytes = Vec::new();
        bytes.extend(tlv(0x0e, &[0x01, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66]));
        bytes.extend(tlv(0x11, &[0xC1, 0x03]));
        let mut map = HashMap::new();
        parse_bluetooth_info(&bytes, &mut map);
        assert_eq!(map.get("11-22-33-44-55-66"), Some(&0x03C1));
    }

    #[test]
    fn bt_info_appearance_without_address_is_dropped() {
        // A 0x11 without a pending address is ignored — no orphan entries.
        let mut bytes = Vec::new();
        bytes.extend(tlv(0x11, &[0xC1, 0x03]));
        let mut map = HashMap::new();
        parse_bluetooth_info(&bytes, &mut map);
        assert!(map.is_empty());
    }

    #[test]
    fn bt_info_unknown_tags_are_skipped_by_length() {
        // Unknown tags skipped by length; later records still parse.
        let mut bytes = Vec::new();
        bytes.extend(tlv(0x02, b"Device"));
        bytes.extend(tlv(0x99, &[0x00, 0x01, 0x02])); // unknown 3 bytes
        bytes.extend(tlv(0x0e, &[0x01, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]));
        bytes.extend(tlv(0x11, &[0xC2, 0x03]));
        let mut map = HashMap::new();
        parse_bluetooth_info(&bytes, &mut map);
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("AA-BB-CC-DD-EE-FF"));
    }

    #[test]
    fn bt_info_truncated_data_stops_cleanly() {
        // Truncated/malformed data never panics; parsing stops at the first bad record.
        let mut map = HashMap::new();
        parse_bluetooth_info(&[], &mut map);
        parse_bluetooth_info(&[0x0e], &mut map); // tag only
        parse_bluetooth_info(&[0x0e, 0x07, 0x01], &mut map); // len past end
        assert!(map.is_empty());
        // A truncated 0x11 (1 byte) produces no entry.
        let mut bytes = Vec::new();
        bytes.extend(tlv(0x0e, &[0x01, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]));
        bytes.extend(tlv(0x11, &[0xC2]));
        parse_bluetooth_info(&bytes, &mut map);
        assert!(map.is_empty());
    }

    #[test]
    fn bt_info_later_appearance_replaces_earlier() {
        // A later appearance for the same address overwrites the earlier one (last wins).
        let mut bytes = Vec::new();
        bytes.extend(tlv(0x0e, &[0x01, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]));
        bytes.extend(tlv(0x11, &[0xC1, 0x03]));
        bytes.extend(tlv(0x0e, &[0x01, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]));
        bytes.extend(tlv(0x11, &[0xC2, 0x03]));
        let mut map = HashMap::new();
        parse_bluetooth_info(&bytes, &mut map);
        assert_eq!(map.get("AA-BB-CC-DD-EE-FF"), Some(&0x03C2));
    }
}
