//!   CGEventCopyIOHIDEvent(cgEvent) -> IOHIDEventRef
//!   IOHIDEventGetSenderID(ioHIDEvent) -> registry_id (uint64)
//!
//! Live device registry + CGEvent -> producing-device attribution chain (Phase 0+1).
//!
//! **Threading: one slow lane.** IOHID enumeration is not cheap -- creating a client waits ~30ms for
//! asynchronous matching, the recovery batch sleeps another 2x50ms, and every property read is a CF
//! call -- so all of it runs on one dedicated worker thread. `DeviceRegistry` (client/services/device
//! pointers, recovery state) is owned by that lane; no other thread locks it. Everyone else reads an
//! immutable [`DeviceSnapshot`] and submits a request:
//!
//! - the HID tap's attribution ([`device_from_cgevent`]) is a map lookup in the snapshot plus a
//!   non-blocking miss submission -- it never waits for IOKit;
//! - the settings window ([`connected_devices`], [`devices_with_virtual`]) reads the same snapshot, so
//!   opening a page or scrolling it cannot pay for enumeration;
//! - the plug monitor submits a change request instead of enumerating inside the IOHID callback.
//!
//! Attribution chain (private SPI, mirroring LinearMouse):
//!   CGEventCopyIOHIDEvent(cgEvent) -> IOHIDEventRef
//!   IOHIDEventGetSenderID(ioHIDEvent) -> registry_id (uint64)
//!   snapshot map registry_id -> DeviceKey
//! A miss submits one coalesced rebuild request (admission in [`miss_rebuild_is_due`]) and falls
//! back to last_active (which resolves the "All Mice" profile).

use crate::ffi::{make_nsstring, nsstring_to_rust, CFRelease};
use crate::mouse::ffi::*;
use crate::{log_debug, log_info};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send, sel};
use std::collections::HashMap;
use std::ffi::{c_void, CString};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Device hardware identity (config matches on VID+PID; name/transport for display only).
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// The pointer properties the settings page reads back, captured while the lane already holds the
/// service client.
///
/// `accel_values` holds the acceleration keys a device can declare (`HIDPointerAcceleration`,
/// `HIDMouseAcceleration`, and the device's own declared key), so `pointer::read_acceleration` can
/// pick with `choose_accel_key` without touching IOKit from the main thread. A key absent from the
/// map means the device does not expose it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DeviceProperties {
    pub(crate) accel_type: Option<String>,
    pub(crate) accel_values: HashMap<String, i64>,
}

impl DeviceProperties {
    /// Whether the device exposes `HIDPointerAcceleration` (the second step of `choose_accel_key`).
    pub(crate) fn has_pointer_acceleration(&self) -> bool {
        self.accel_values.contains_key(KEY_POINTER_ACCEL)
    }

    /// The raw IOFixed value stored under `key`, when the device has it.
    pub(crate) fn accel_value(&self, key: &str) -> Option<i64> {
        self.accel_values.get(key).copied()
    }
}

/// What the lane's last batch knows about the device set.
///
/// `NotQueried` and `Ready(empty)` are deliberately distinct: the settings page must be able to say
/// "we have not asked yet" instead of claiming no device is connected, and a reader that sees
/// `NotQueried` is what triggers the request that fills it in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SnapshotStatus {
    /// The lane has not completed a batch yet (startup, or the worker could not start).
    NotQueried,
    /// The last batch completed; `devices` is its answer, empty when nothing enumerated.
    Ready,
    /// The last batch failed. The device list is the previous valid one, if any.
    Failed,
}

/// Immutable answer to "which pointer devices are connected, and what do they declare".
///
/// Published by the lane after every batch; readers clone the `Arc`. It is plain owned data (no CF
/// pointers, no service clients), so a reader can hold it for as long as it likes while the lane
/// rebuilds everything underneath.
#[derive(Debug, Clone)]
pub(crate) struct DeviceSnapshot {
    /// Devices in enumeration order, paired with `properties` by index.
    pub(crate) devices: Vec<DeviceIdentity>,
    pub(crate) properties: Vec<DeviceProperties>,
    /// `registry_id -> (VID, PID)`, the attribution map the HID tap looks events up in.
    attribution: Vec<(u64, DeviceKey)>,
    pub(crate) status: SnapshotStatus,
    /// Increments on every publish; a batch that would publish an older version must be dropped.
    pub(crate) generation: u64,
}

impl DeviceSnapshot {
    fn empty(status: SnapshotStatus) -> Self {
        Self {
            devices: Vec::new(),
            properties: Vec::new(),
            attribution: Vec::new(),
            status,
            generation: 0,
        }
    }

    /// The device that produced an event, by IOHID registry ID (the sender id). Pure: the HID tap
    /// calls this per event, so it must stay a lookup.
    fn device_for_registry_id(&self, sender: u64) -> Option<DeviceKey> {
        self.attribution
            .iter()
            .find(|(id, _)| *id == sender)
            .map(|(_, key)| *key)
    }

    pub(crate) fn properties_for(&self, key: DeviceKey) -> Option<&DeviceProperties> {
        self.devices
            .iter()
            .position(|d| (d.vendor_id, d.product_id) == key)
            .and_then(|index| self.properties.get(index))
    }

    /// Whether a device with this hardware identity is present (the virtual pointer excluded: it is
    /// appended by the reader, not part of the HID set).
    fn holds(&self, key: DeviceKey) -> bool {
        self.devices
            .iter()
            .any(|d| (d.vendor_id, d.product_id) == key)
    }
}

/// The selection after a device refresh. `None` means "leave the current choice alone": the list is
/// not an answer (not queried / the query failed), so an empty list must not clear the user's device.
/// `Some(None)` is "no device is connected" (a successful empty answer); `Some(Some(key))` is the
/// device to select (the first one, when the current choice is gone or there was none).
pub(crate) fn selection_after_refresh(
    current: Option<DeviceKey>,
    connected: &[DeviceIdentity],
    status: SnapshotStatus,
) -> Option<Option<DeviceKey>> {
    if status != SnapshotStatus::Ready {
        return None;
    }
    if connected.is_empty() {
        return Some(None);
    }
    if let Some(current) = current {
        let still_connected = connected
            .iter()
            .any(|d| (d.vendor_id, d.product_id) == current);
        if still_connected {
            return None;
        }
    }
    connected.first().map(|d| Some((d.vendor_id, d.product_id)))
}

/// The published snapshot. The lock is held only to clone/replace the `Arc` -- never across IOKit.
static SNAPSHOT: Mutex<Option<Arc<DeviceSnapshot>>> = Mutex::new(None);

/// The answer every reader gets before the lane's first publish.
static NOT_QUERIED: OnceLock<Arc<DeviceSnapshot>> = OnceLock::new();

/// The newest published answer; `NotQueried` before the lane's first publish.
pub(crate) fn snapshot() -> Arc<DeviceSnapshot> {
    let published = {
        let guard = SNAPSHOT.lock().unwrap();
        guard.as_ref().map(Arc::clone)
    };
    match published {
        Some(snapshot) => snapshot,
        None => Arc::clone(
            NOT_QUERIED.get_or_init(|| Arc::new(DeviceSnapshot::empty(SnapshotStatus::NotQueried))),
        ),
    }
}

/// Publish one batch's answer and hand it to readers; returns the generation it published.
fn publish(snapshot: DeviceSnapshot) -> u64 {
    let mut guard = SNAPSHOT.lock().unwrap();
    let next_generation = guard
        .as_ref()
        .map(|current| current.generation)
        .unwrap_or(0)
        + 1;
    let mut snapshot = snapshot;
    snapshot.generation = next_generation;
    *guard = Some(Arc::new(snapshot));
    next_generation
}

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
    /// When the last full *query recovery batch* ran (see `recovery_is_due`). Deliberately NOT touched by
    /// `ensure_enumerated`'s single low-level enumeration: the recovery batch is what recovers from a stale
    /// client, and recording a plain enumeration as an attempt would let the startup case skip it.
    recovery_at: Option<Instant>,
    /// Counts device-change invalidations. A recovery batch captures this before it retries and only records
    /// its attempt if nothing invalidated it meanwhile, so a plug/unplug that lands during the batch is not
    /// swallowed by the batch's own timestamp.
    invalidations: u64,
    /// When the last attribution-miss rebuild ran (see `miss_rebuild_is_due`). Separate from
    /// `recovery_at`: a miss rebuild must not clear the recovery cooldown the settings path uses.
    miss_rebuild_at: Option<Instant>,
    /// Whether the last enumeration could reach the system at all (client creation / CopyServices).
    /// A failed pass publishes `SnapshotStatus::Failed` and keeps the previous device list, so a
    /// transient IOKit failure is never reported as "no devices connected".
    last_enumeration_failed: bool,
    /// Whether the current client was scheduled on the mouse thread's runloop. A client created
    /// before the plug monitor recorded that runloop would otherwise never be scheduled, and an
    /// unscheduled client's matching does not settle (measured).
    client_scheduled: bool,
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
            recovery_at: None,
            invalidations: 0,
            miss_rebuild_at: None,
            last_enumeration_failed: false,
            client_scheduled: false,
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

/// Schedule the current client on the mouse thread's runloop when that is known and not done yet.
/// Pure bookkeeping around one CF call; safe to call on every batch.
unsafe fn schedule_client_if_needed(reg: &mut DeviceRegistry) {
    if reg.client.is_null() || reg.client_scheduled {
        return;
    }
    let runloop = *mouse_runloop_static().lock().unwrap();
    let Some(runloop) = runloop else {
        return;
    };
    IOHIDEventSystemClientScheduleWithRunLoop(
        reg.client,
        runloop,
        crate::event_tap::kCFRunLoopDefaultMode,
    );
    reg.client_scheduled = true;
}

/// Enumerate currently-connected mouse/trackpad devices and populate the registry.
///
/// **Transactional.** The pass builds a candidate client, services array and device list and installs
/// them only when it succeeded. A failed pass leaves the registry (and therefore the last published
/// snapshot) exactly as it was: the devices' `service_client` pointers are owned by the services array,
/// so releasing that array before the replacement is known good would turn a transient IOKit error into
/// a use-after-free on the next property read, and clearing the device list would present a failed query
/// as "every device was unplugged".
///
/// Caller holds the REGISTRY lock. With rebuild_client = true the IOHIDEventSystemClient is recreated
/// (after a Bluetooth disconnect/reconnect the old client's registry cache is stale).
unsafe fn enumerate_locked(reg: &mut DeviceRegistry, rebuild_client: bool) -> bool {
    if skip_device_enumeration_in_smoke() {
        // Short-lived GUI smokes manipulate settings state directly and do not test physical
        // devices. Avoid a scheduled IOHIDEventSystemClient whose exit-time release can race its
        // run-loop unschedule (observed as a CoreFoundation PAC trap).
        reg.devices.clear();
        reg.last_enumeration_failed = false;
        return true;
    }

    // The mouse thread's runloop, if it has been recorded yet (see `schedule_client_if_needed`).
    let runloop = *mouse_runloop_static().lock().unwrap();
    let mut candidate_client = reg.client;
    // Matching on IOHIDEventSystemClient is asynchronous: CopyServices right after creation can
    // return an empty list (measured 0/1 flapping), so a fresh client waits for matching to settle.
    // Only the fresh-client path pays it; reusing an existing client does not.
    let created_client = if rebuild_client || candidate_client.is_null() {
        let client = IOHIDEventSystemClientCreate(std::ptr::null());
        if client.is_null() {
            log_info!("[device] failed to create IOHIDEventSystemClient");
            reg.last_enumeration_failed = true;
            return false;
        }
        // Match the Generic Desktop page (filtered by usage below).
        let page_key = make_nsstring(KEY_PRIMARY_USAGE_PAGE);
        let page_val: *mut AnyObject =
            msg_send![class!(NSNumber), numberWithInt: USAGE_PAGE_GENERIC_DESKTOP as i32];
        let dict: *mut AnyObject =
            msg_send![class!(NSDictionary), dictionaryWithObject: page_val, forKey: page_key];
        let arr: *mut AnyObject = msg_send![class!(NSArray), arrayWithObject: dict];
        IOHIDEventSystemClientSetMatchingMultiple(client, arr as *const c_void);
        CFRelease(page_key as *const c_void);
        std::thread::sleep(Duration::from_millis(30));
        if let Some(runloop) = runloop {
            IOHIDEventSystemClientScheduleWithRunLoop(
                client,
                runloop,
                crate::event_tap::kCFRunLoopDefaultMode,
            );
        }
        candidate_client = client;
        true
    } else {
        // A client created before the runloop was recorded is scheduled here, on the first pass that
        // can see it; matching never settles without it (measured).
        if !reg.client_scheduled {
            if let Some(runloop) = runloop {
                IOHIDEventSystemClientScheduleWithRunLoop(
                    candidate_client,
                    runloop,
                    crate::event_tap::kCFRunLoopDefaultMode,
                );
                reg.client_scheduled = true;
            }
        }
        false
    };

    let services = IOHIDEventSystemClientCopyServices(candidate_client);
    if services.is_null() {
        log_debug!("[device] no services returned");
        if created_client {
            CFRelease(candidate_client as *const c_void);
        }
        reg.last_enumeration_failed = true;
        return false;
    }

    let mut devices: Vec<Device> = Vec::new();

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

        devices.push(Device {
            identity: DeviceIdentity {
                vendor_id: vid,
                product_id: pid,
                name,
                transport,
            },
            service_client: service,
        });
    }

    // Commit: only now may the previous pair be released, because the old device list pointed into
    // the old services array.
    if !reg.services.is_null() {
        CFRelease(reg.services as *const c_void);
    }
    if created_client && !reg.client.is_null() {
        CFRelease(reg.client as *const c_void);
    }
    if created_client {
        reg.client_scheduled = runloop.is_some();
    }
    reg.client = candidate_client;
    reg.services = services;
    reg.devices = devices;
    reg.last_enumeration_failed = false;
    log_debug!(
        "[device] enumerated {} pointer device(s).",
        reg.devices.len()
    );
    true
}

/// Enumerate once at startup (also lazily triggered on first attribution failure).
/// How long a failed query recovery stays authoritative. Repeated queries inside this window reuse the
/// (empty) device list instead of rebuilding the client and re-enumerating with the two retries below.
const RECOVERY_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(2);

/// Whether a query must run the recovery batch: rebuild the client and retry enumeration.
///
/// Pure, so the policy is testable without IOKit. The batch exists for the startup / stale-client /
/// async-matching race (`enumerate_locked`'s 30ms settle can miss a device that is still registering), and
/// running it again for every caller made "which mice are connected" cost ~170ms of sleeps per call on a
/// machine where nothing enumerates -- the settings window called it three times per open. A non-empty
/// list never needs recovery.
fn recovery_is_due(last: Option<Instant>, now: Instant, devices_empty: bool) -> bool {
    devices_empty && last.is_none_or(|last| now.duration_since(last) >= RECOVERY_COOLDOWN)
}

/// A device actually changed, so the next query must be allowed to recover again -- and a miss-driven
/// rebuild is due immediately, because the change is the evidence a plain attribution miss lacks.
fn invalidate_recovery(reg: &mut DeviceRegistry) {
    reg.recovery_at = None;
    reg.miss_rebuild_at = None;
    reg.invalidations = reg.invalidations.wrapping_add(1);
}

/// Whether a finished recovery batch may record its attempt time: only when no device change invalidated
/// the registry while the batch was running. Pure, so the interleaving is testable.
fn may_record_recovery(batch_generation: u64, current_generation: u64) -> bool {
    batch_generation == current_generation
}

/// How often an attribution miss may force a client rebuild.
///
/// A miss is not a device change: it is the *symptom* of a stale client (a Bluetooth reconnect whose
/// plug events were lost, or a device that appeared after the last enumeration). The rebuild is the
/// self-heal, but reacting to every event turns the lane into a feedback loop -- on a machine whose
/// enumeration returns nothing, every wheel event misses, and re-arming the rebuild per event kept
/// the settings window re-rendering several times a second. One rebuild per window still heals a
/// stale client within two seconds, and a real plug clears the window (`invalidate_recovery`).
const MISS_REBUILD_COOLDOWN: Duration = Duration::from_secs(2);

/// Pure admission for a miss-driven rebuild; `last` is when the previous miss rebuild ran.
fn miss_rebuild_is_due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|last| now.duration_since(last) >= MISS_REBUILD_COOLDOWN)
}

/// What woke the lane. Every public entry point submits one of these; none of them touches IOKit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaneRequest {
    /// Enumerate once, without rebuilding the client (startup, or a first query).
    EnsureEnumerated,
    /// The settings page reads devices or pointer properties: keep the answer current.
    SettingsQuery,
    /// The plug monitor saw a real attach/detach.
    DeviceChanged { is_removal: bool },
    /// The delayed recheck: cheap diff first, full rebuild when forced or when the set changed.
    Recheck { force: bool },
    /// Attribution met a sender the published snapshot does not hold.
    AttributionMiss,
    /// Re-apply the configured pointer properties (a config reload) and refresh their snapshot.
    PointerReapply,
}

/// Requests coalesced while the lane was busy. Reasons are kept rather than the raw requests: a
/// batch may satisfy several callers with one enumeration, and none may be silently dropped.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct PendingRequests {
    ensure: bool,
    settings: bool,
    /// A real device change. Any plug outranks a recheck; a removal outranks a matching event.
    changed: Option<bool>,
    recheck: bool,
    recheck_force: bool,
    /// How many attribution misses arrived; a count is enough (one rebuild answers all of them) but
    /// the number belongs in the batch report.
    misses: u32,
    pointer_reapply: bool,
}

/// Attribution misses seen since the process started: an aggregate counter for the batch report,
/// because a miss that does not trigger a rebuild is deliberately silent (it can arrive per event).
static MISSES_SEEN: AtomicUsize = AtomicUsize::new(0);

/// One-shot diagnostic for the attribution map: when the first miss happens on a snapshot that
/// holds devices but no registry ids, the SPI the map is built from has failed -- worth one line,
/// because every event would then fall back to the last-active device.
static MISS_MAP_WARNED: AtomicBool = AtomicBool::new(false);

impl PendingRequests {
    fn is_empty(&self) -> bool {
        !self.ensure
            && !self.settings
            && self.changed.is_none()
            && !self.recheck
            && self.misses == 0
            && !self.pointer_reapply
    }

    /// Whether attribution misses are the *only* reason. A miss that is not admitted is dropped
    /// without publishing, so a co-batched reason must keep the batch alive.
    fn only_misses(&self) -> bool {
        self.misses > 0
            && !self.ensure
            && !self.settings
            && self.changed.is_none()
            && !self.recheck
            && !self.pointer_reapply
    }

    fn merge(&mut self, request: LaneRequest) {
        match request {
            LaneRequest::EnsureEnumerated => self.ensure = true,
            LaneRequest::SettingsQuery => {
                self.ensure = true;
                self.settings = true;
                // A settings refresh must also notice a *changed* device set: with the mouse feature
                // off there is no plug monitor, so the page opening is the only thing that can.
                self.recheck = true;
            }
            LaneRequest::DeviceChanged { is_removal } => {
                self.changed = Some(self.changed.is_some_and(|removal| removal) || is_removal);
            }
            LaneRequest::Recheck { force } => {
                self.recheck = true;
                self.recheck_force |= force;
            }
            LaneRequest::AttributionMiss => {
                self.misses = self.misses.saturating_add(1);
                MISSES_SEEN.fetch_add(1, Ordering::Relaxed);
            }
            LaneRequest::PointerReapply => self.pointer_reapply = true,
        }
    }
}

static PENDING: Mutex<PendingRequests> = Mutex::new(PendingRequests {
    ensure: false,
    settings: false,
    changed: None,
    recheck: false,
    recheck_force: false,
    misses: 0,
    pointer_reapply: false,
});

/// One wake token for the lane, never the payload: the payload is `PENDING`, so a token that
/// arrives while the lane is executing only makes the next iteration take the bits again.
enum LaneState {
    NotStarted,
    Running(SyncSender<()>),
    /// The spawn failed. Kept so the failure is not retried per event, but it is retried after
    /// `LANE_SPAWN_RETRY`: a transient resource failure must not disable device support for the
    /// life of the process.
    Failed {
        last_attempt: Instant,
    },
}

impl LaneState {
    /// Whether another start attempt may run now. Pure, so the backoff is testable.
    fn may_start(&self, now: Instant) -> bool {
        match self {
            Self::Running(_) => false,
            Self::NotStarted => true,
            Self::Failed { last_attempt } => now.duration_since(*last_attempt) >= LANE_SPAWN_RETRY,
        }
    }
}

static LANE: Mutex<LaneState> = Mutex::new(LaneState::NotStarted);
const LANE_SPAWN_RETRY: Duration = Duration::from_secs(5);

/// Set once the process is shutting down: the lane must not write pointer settings after this, or the
/// quit path's `pointer::restore()` could be followed by a queued apply that puts the device back into
/// the state it was trying to undo.
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);
/// Serializes the lane's one write path with the quit path's final restore. The quit path holds it
/// across `restore()`, so an apply that was admitted just before the shutdown flag flipped cannot
/// land after the restore.
static POINTER_WRITE: Mutex<()> = Mutex::new(());

/// Whether a pointer write may run now. Pure, so the quit boundary is a decision, not a comment.
fn pointer_write_allowed(shutting_down: bool) -> bool {
    !shutting_down
}

/// Run one pointer write, re-checking the quit flag **under the write lock**.
///
/// `admitted` is the caller's first read. Without the re-check there is a window between that read
/// and the write: the lane loads "allowed", the quit path sets the flag and restores, and the lane
/// then writes on top of the restore. With it, either the lane takes the lock first (and refuses,
/// because the flag is set) or the quit path takes it first (and the lane refuses after `restore()`).
fn pointer_write(admitted: bool, write: impl FnOnce()) -> bool {
    if !admitted {
        return false;
    }
    let _guard = POINTER_WRITE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if SHUTTING_DOWN.load(Ordering::SeqCst) {
        return false;
    }
    write();
    true
}

/// Quit-path boundary: refuse further pointer writes, wait for an apply already inside the lock, and
/// restore the system values while still holding it. `restore()` is then the last write to the device.
pub(crate) fn restore_pointer_settings_on_quit() {
    SHUTTING_DOWN.store(true, Ordering::SeqCst);
    let _guard = POINTER_WRITE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    crate::mouse::pointer::restore();
}

fn take_pending() -> PendingRequests {
    let mut pending = PENDING.lock().unwrap();
    std::mem::take(&mut *pending)
}

/// The lane thread, started on first use. `None` means the thread could not be spawned, in which
/// case requests stay pending and the snapshot stays `NotQueried` (the reader reports no devices
/// rather than blocking or claiming an empty set is the truth).
fn lane_sender() -> Option<SyncSender<()>> {
    let mut state = LANE.lock().unwrap();
    if let LaneState::Running(sender) = &*state {
        return Some(sender.clone());
    }
    if !state.may_start(Instant::now()) {
        return None;
    }
    let (sender, receiver) = mpsc::sync_channel::<()>(1);
    let spawned = std::thread::Builder::new()
        .name("mouse-device-lane".into())
        .spawn(move || lane_loop(receiver));
    match spawned {
        Ok(_) => {
            *state = LaneState::Running(sender.clone());
            Some(sender)
        }
        Err(error) => {
            log_info!(
                "[device] could not start the device lane: {error}; retrying in {}s",
                LANE_SPAWN_RETRY.as_secs()
            );
            *state = LaneState::Failed {
                last_attempt: Instant::now(),
            };
            drop(state);
            // Make the state visible instead of leaving readers on "not queried" forever: the
            // settings page can then say the device list is unavailable rather than show none.
            publish_unavailable();
            None
        }
    }
}

/// The lane's own loop: take a batch of reasons, run it, wait for the next token.
fn lane_loop(receiver: std::sync::mpsc::Receiver<()>) {
    crate::performance::set_current_thread_qos(crate::performance::ThreadQos::Utility);
    loop {
        let pending = take_pending();
        if pending.is_empty() {
            if receiver.recv().is_err() {
                return;
            }
            continue;
        }
        // IOKit work creates autoreleased NSDictionary/NSArray/NSNumber objects (pointer::apply);
        // drain them per batch, like the other background workers.
        let pool: *mut AnyObject = unsafe { msg_send![class!(NSAutoreleasePool), new] };
        // A panic here would end the lane thread and silently leave every device query unanswered;
        // contain it like any other callback boundary.
        crate::callback_guard::void("mouse_device_lane_batch", || unsafe { run_batch(pending) });
        unsafe {
            let _: () = msg_send![pool, drain];
        }
    }
}

/// Publish `Failed` while keeping the last valid device list, so a dead lane is not reported as
/// "no devices connected".
fn publish_unavailable() {
    let previous = snapshot();
    if previous.status == SnapshotStatus::Failed {
        return;
    }
    let mut snapshot = (*previous).clone();
    snapshot.status = SnapshotStatus::Failed;
    let _ = publish_if_changed(snapshot);
}

/// Submit a request and wake the lane. Never blocks and never touches IOKit: this is what the HID
/// tap, the plug callback and the settings window are allowed to do.
fn submit(request: LaneRequest) {
    {
        let mut pending = PENDING.lock().unwrap();
        pending.merge(request);
    }
    let Some(sender) = lane_sender() else {
        return;
    };
    // Full means a token is already queued; the bits set above will be seen when it is consumed.
    if let Err(TrySendError::Disconnected(_)) = sender.try_send(()) {
        log_info!("[device] the device lane stopped; device queries stay unanswered.");
    }
}

/// Enumerate once at startup (also the settings page's first query). Asynchronous by design: the
/// caller gets the current snapshot, and the lane's publish notifies the settings window.
pub(crate) fn ensure_enumerated() {
    submit(LaneRequest::EnsureEnumerated);
}

/// Currently-connected devices (VID/PID/name) for the settings device picker. Reads the published
/// snapshot -- no IOKit, no lock that enumeration holds -- and asks the lane for a fresh answer when
/// there is none yet. Works even when `mouse.enabled` is false.
pub(crate) fn connected_devices() -> Vec<DeviceIdentity> {
    if skip_device_enumeration_in_smoke() {
        return Vec::new();
    }
    let snapshot = snapshot();
    if snapshot.status != SnapshotStatus::Ready {
        // Not asked yet, or the last pass failed: ask again. A failure is not an answer the page
        // should keep showing until the user reopens Settings.
        submit(LaneRequest::SettingsQuery);
    }
    devices_with_virtual(&snapshot)
}

/// Ask the lane to re-read the device set: the settings page calls this when it opens, so a device
/// plugged in while the mouse feature was off (no plug monitor exists) is still picked up.
pub(crate) fn request_device_refresh() {
    submit(LaneRequest::SettingsQuery);
}

/// Re-apply the configured pointer properties (a config reload) through the lane, so the write and the
/// property snapshot stay in one serialized order.
pub(crate) fn request_pointer_reapply() {
    submit(LaneRequest::PointerReapply);
}

/// The published devices plus the virtual pointer, whose presence is a main-thread liveness check (the
/// injecting process can exit at any time). The picker lists what this returns.
pub(crate) fn devices_with_virtual(snapshot: &DeviceSnapshot) -> Vec<DeviceIdentity> {
    with_virtual_pointer(snapshot.devices.clone())
}

/// One batch on the lane: at most one rebuild, at most one pointer apply, one publish.
unsafe fn run_batch(pending: PendingRequests) {
    #[cfg(test)]
    lane_harness::hold_if_requested();
    #[cfg(test)]
    if let Some(hold) = lane_harness::active_hold() {
        // The test process must not create real IOHID clients; the batch still publishes, so the
        // publish side of the lane is exercised.
        let snapshot = {
            let reg = registry().lock().unwrap();
            build_snapshot(&reg, SnapshotStatus::Ready)
        };
        let _ = publish_if_changed(snapshot);
        lane_harness::finished(&hold);
        return;
    }
    let started = Instant::now();
    // A miss that is not admitted must not even publish: during a fast scroll this is the common
    // path, and waking the lane per event just to compare an unchanged snapshot is waste.
    if pending.only_misses() {
        let due = {
            let reg = registry().lock().unwrap();
            miss_rebuild_is_due(reg.miss_rebuild_at, Instant::now())
        };
        if !due {
            #[cfg(test)]
            if let Some(hold) = lane_harness::active_hold() {
                lane_harness::finished(&hold);
            }
            return;
        }
    }
    let mut rebuilt = false;
    let mut device_change = false;
    let mut reason = "ensure";

    if let Some(is_removal) = pending.changed {
        reason = if is_removal {
            "plug-removal"
        } else {
            "plug-matching"
        };
        rebuilt = true;
        device_change = true;
    } else if pending.recheck {
        // The cheap diff decides whether the delayed recheck costs a rebuild; force skips the diff
        // (a removal followed by a matching event inside the debounce window means the old client's
        // cache is dead, so the diff is not evidence).
        let changed = pending.recheck_force || {
            let reg = registry().lock().unwrap();
            if reg.client.is_null() {
                true
            } else {
                let before: Vec<DeviceKey> = reg.devices.iter().map(|d| d.key()).collect();
                let now = enumerate_keys(reg.client);
                before != now
            }
        };
        if changed {
            reason = "recheck";
            rebuilt = true;
            device_change = true;
        }
    }

    // A miss rebuild is the stale-client self-heal, but it is not a device change: it must not
    // clear the recovery cooldown that the settings path relies on, and it runs at most once per
    // MISS_REBUILD_COOLDOWN however many events missed in between.
    let mut miss_rebuild = false;
    if !rebuilt && pending.misses > 0 {
        let due = {
            let reg = registry().lock().unwrap();
            miss_rebuild_is_due(reg.miss_rebuild_at, Instant::now())
        };
        if due {
            reason = "attribution-miss";
            rebuilt = true;
            miss_rebuild = true;
        }
    }

    if rebuilt {
        let mut reg = registry().lock().unwrap();
        let _ = enumerate_locked(&mut reg, true);
        if device_change {
            // A real change may have brought a device the last recovery batch could not see.
            invalidate_recovery(&mut reg);
        } else {
            reg.miss_rebuild_at = Some(Instant::now());
        }
    } else if pending.ensure || pending.settings {
        reason = if pending.settings {
            "settings"
        } else {
            "ensure"
        };
        ensure_enumerated_in_lane();
    }

    // The pointer properties live on the IOHIDServiceClient instance, which a rebuild replaces;
    // re-applying is what restores them after a reconnect (and it is cheap when nothing changed).
    // A config reload asks for the same step explicitly, through the lane, so the write and the
    // property snapshot below cannot interleave with another apply.
    if device_change || miss_rebuild || pending.pointer_reapply {
        if !rebuilt && pending.pointer_reapply {
            reason = "pointer-reapply";
        }
        let admitted = pointer_write_allowed(SHUTTING_DOWN.load(Ordering::SeqCst));
        if !pointer_write(admitted, crate::mouse::pointer::apply) {
            log_debug!("[device] pointer write skipped: the process is shutting down");
        }
    }

    let snapshot = {
        let reg = registry().lock().unwrap();
        let status = if reg.last_enumeration_failed {
            // The list is whatever the last successful pass found; readers keep it instead of
            // being told "no devices" because IOKit failed once.
            SnapshotStatus::Failed
        } else {
            SnapshotStatus::Ready
        };
        build_snapshot(&reg, status)
    };
    let devices = snapshot.devices.len();
    // Zero entries with devices present means the registry-id SPI gave us nothing: every event
    // would fall back to the last-active device, which is worth seeing in a debug log.
    let attribution_entries = snapshot.attribution.len();
    let published_generation = publish_if_changed(snapshot);
    log_debug!(
        "[device] lane batch: reason={reason} rebuilt={rebuilt} device_change={device_change} misses={} misses_total={} merged={} devices={devices} attribution={attribution_entries} elapsed_ms={} generation={}",
        pending.misses,
        MISSES_SEEN.load(Ordering::Relaxed),
        pending.ensure as u8 + pending.settings as u8 + pending.changed.is_some() as u8 + pending.recheck as u8,
        started.elapsed().as_millis(),
        published_generation.map_or(0, |generation| generation),
    );
    if published_generation.is_some() {
        notify_devices_changed();
    }
    #[cfg(test)]
    if let Some(hold) = lane_harness::active_hold() {
        lane_harness::finished(&hold);
    }
}

/// Enumerate (and, when the recovery batch is due, retry with a rebuilt client): the settings and
/// startup path. Runs on the lane, so the matching settle and the two retries are affordable here.
unsafe fn ensure_enumerated_in_lane() {
    let _ = injector_notice_sender();
    let (needs_recovery, batch_generation) = {
        let mut reg = registry().lock().unwrap();
        if reg.client.is_null() || reg.devices.is_empty() {
            let _ = enumerate_locked(&mut reg, false);
        }
        // The plug monitor may have recorded the runloop after this client was created.
        schedule_client_if_needed(&mut reg);
        (
            recovery_is_due(reg.recovery_at, Instant::now(), reg.devices.is_empty()),
            reg.invalidations,
        )
    };
    if !needs_recovery {
        return;
    }
    for _ in 0..2 {
        std::thread::sleep(Duration::from_millis(50));
        let mut reg = registry().lock().unwrap();
        let _ = enumerate_locked(&mut reg, true);
        if !reg.devices.is_empty() {
            break;
        }
    }
    // Record the attempt whether or not it found anything: an empty result must still suppress the
    // next batch inside the cooldown. Not when a device change invalidated the registry during the
    // batch: that change must keep the next query free to recover.
    let mut reg = registry().lock().unwrap();
    if may_record_recovery(batch_generation, reg.invalidations) {
        reg.recovery_at = Some(Instant::now());
    }
}

/// Capture the registry as an immutable answer: plain device facts plus the two things readers need
/// (the attribution map for the tap, the pointer properties for the settings page).
unsafe fn build_snapshot(reg: &DeviceRegistry, status: SnapshotStatus) -> DeviceSnapshot {
    let mut devices = Vec::with_capacity(reg.devices.len());
    let mut properties = Vec::with_capacity(reg.devices.len());
    let mut attribution = Vec::with_capacity(reg.devices.len());
    for device in &reg.devices {
        devices.push(device.identity.clone());
        properties.push(read_device_properties(device.service_client));
        let registry_id = IOHIDServiceClientGetRegistryID(device.service_client);
        if !registry_id.is_null() {
            let id: i64 = msg_send![registry_id as *mut AnyObject, longLongValue];
            attribution.push((id as u64, device.key()));
        }
    }
    DeviceSnapshot {
        devices,
        properties,
        attribution,
        status,
        generation: 0,
    }
}

/// Read the pointer properties the settings page shows back, while the service client is in hand.
unsafe fn read_device_properties(service: *mut c_void) -> DeviceProperties {
    let accel_type = {
        let declared = prop_string(service, KEY_ACCEL_TYPE);
        (!declared.is_empty()).then_some(declared)
    };
    let mut accel_values = HashMap::new();
    for key in [KEY_POINTER_ACCEL, KEY_MOUSE_ACCEL] {
        if let Some(value) = prop_int(service, key) {
            accel_values.insert(key.to_string(), value);
        }
    }
    // The key macOS actually reads for this device (pointer::choose_accel_key) may be neither of
    // the two conventions above: the device declares its own.
    if let Some(declared) = accel_type.as_deref() {
        if !accel_values.contains_key(declared) {
            if let Some(value) = prop_int(service, declared) {
                accel_values.insert(declared.to_string(), value);
            }
        }
    }
    DeviceProperties {
        accel_type,
        accel_values,
    }
}

/// Publish a batch's answer when readers would see something new; returns the published generation.
fn publish_if_changed(snapshot: DeviceSnapshot) -> Option<u64> {
    let changed = {
        let current = SNAPSHOT.lock().unwrap();
        match current.as_ref() {
            None => true,
            Some(previous) => {
                previous.devices != snapshot.devices
                    || previous.properties != snapshot.properties
                    // The registry ids are re-read per pass: a reconnect can keep VID/PID while the
                    // sender ids change, and a stale map would miss every event from the new instance.
                    || previous.attribution != snapshot.attribution
                    || previous.status != snapshot.status
            }
        }
    };
    if changed {
        Some(publish(snapshot))
    } else {
        None
    }
}

/// Append the virtual pointer (the injecting process) last: it is not a HID device and only appears after an
/// injected event has been seen while that process is still alive, so the picker's entry count follows
/// whether a software KVM is running (see `virtual_device_identity`).
fn with_virtual_pointer(mut out: Vec<DeviceIdentity>) -> Vec<DeviceIdentity> {
    if let Some(virtual_pointer) = virtual_device_identity() {
        out.push(virtual_pointer);
    }
    out
}

/// Keep every device-enumeration entry point on the same argv-only smoke policy.
fn skip_device_enumeration_in_smoke() -> bool {
    crate::dev_flags::any_prefix("--smoke")
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

/// Delayed-recheck handoff: at most one thread, one pending reason set, and a forced reason that must
/// never be dropped (the BLE reconnect promise depends on it). The transitions are pure so the
/// interleaving that used to strand a `force` is pinned by a test.
#[derive(Default)]
struct RecheckSchedule {
    thread_running: bool,
    pending: bool,
    force: bool,
}

impl RecheckSchedule {
    /// Record a reason; true when a recheck thread has to be started.
    fn schedule(&mut self, force: bool) -> bool {
        self.pending = true;
        self.force |= force;
        if self.thread_running {
            false
        } else {
            self.thread_running = true;
            true
        }
    }

    /// Take the next reason, or end the thread when nothing is pending (atomic with the exit).
    fn take(&mut self) -> Option<bool> {
        if !self.pending {
            self.thread_running = false;
            return None;
        }
        self.pending = false;
        Some(std::mem::take(&mut self.force))
    }
}

static RECHECK: Mutex<RecheckSchedule> = Mutex::new(RecheckSchedule {
    thread_running: false,
    pending: false,
    force: false,
});
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

/// Schedule the delayed recheck: coalesce the events inside the debounce window and hand the reason
/// to the lane. With force_rebuild = true the lane rebuilds fully even if the cheap diff shows an
/// unchanged device set.
fn schedule_recheck(force_rebuild: bool) {
    let start = RECHECK.lock().unwrap().schedule(force_rebuild);
    if !start {
        return;
    }
    std::thread::spawn(|| loop {
        std::thread::sleep(RECHECK_DELAY);
        // The take decides atomically whether this thread continues or exits, so a reason recorded
        // while the thread was sleeping is never left behind for a later, unrelated event.
        let Some(force) = RECHECK.lock().unwrap().take() else {
            return;
        };
        submit(LaneRequest::Recheck { force });
    });
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
    // This callback runs on the mouse thread, which is also the HID tap's thread: everything slow
    // (client rebuild, the 30ms matching settle, pointer re-apply, the settings notification) is the
    // lane's job. Submitting is a mutex plus a try_send.
    submit(LaneRequest::DeviceChanged { is_removal });
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
    crate::callback_guard::void("device_matching_callback", || {
        device_change_callback(context, result, sender, callback, false);
    });
}

/// Wrapper for IOHIDManager removal (detach) callbacks, entering the shared handler with
/// the direction flag.
unsafe extern "C" fn device_removal_callback(
    context: *mut c_void,
    result: i32,
    sender: *mut c_void,
    callback: *mut c_void,
) {
    crate::callback_guard::void("device_removal_callback", || {
        device_change_callback(context, result, sender, callback, true);
    });
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
    // Drop the manager lock first: a submit may spawn the lane thread, and this lock guards the
    // monitor object, not the request queue.
    drop(m);
    // The monitor is the app's first device entry point: ask the lane for the initial enumeration
    // here so the snapshot is warm before the settings page and the first attribution need it.
    submit(LaneRequest::EnsureEnumerated);
}

/// Find the device that produced a CGEvent.
/// Chain: CGEventCopyIOHIDEvent -> IOHIDEventGetSenderID -> the published snapshot's
/// registry_id -> (VID, PID) map. Pure Rust: the HID tap must never wait for IOKit, and a miss is
/// submitted to the lane rather than rebuilt here.
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
        let snapshot = snapshot();
        if let Some(key) = snapshot.device_for_registry_id(sender) {
            *LAST_ACTIVE_KEY.lock().unwrap() = Some(key);
            return Some(key);
        }
        // A miss may mean a new device or a Bluetooth reconnect. The lane rebuilds at most once per
        // MISS_REBUILD_COOLDOWN, so a scroll storm that misses every event cannot turn itself into a
        // rebuild loop; this event still resolves through the last-active device.
        if !snapshot.devices.is_empty()
            && snapshot.attribution.is_empty()
            && !MISS_MAP_WARNED.swap(true, Ordering::Relaxed)
        {
            log_info!(
                "[device] no registry ids for {} known device(s); attribution falls back to the last-active device.",
                snapshot.devices.len()
            );
        }
        submit(LaneRequest::AttributionMiss);
        last_active_key()
    }
}

/// Fallback: return the last-active device's (VID, PID), matched by hardware identity so
/// re-enumeration id drift doesn't break it. Mouse BUTTON events frequently carry no
/// IOHIDEvent sender (scroll events do), so attribution lands here; when the process hasn't
/// attributed anything yet (LAST_ACTIVE_KEY is None), a single-device snapshot is used
/// directly -- making button bindings reliable on single-mouse setups.
fn last_active_key() -> Option<DeviceKey> {
    let key = *LAST_ACTIVE_KEY.lock().unwrap();
    let snapshot = snapshot();
    if let Some(k) = key {
        if snapshot.holds(k) {
            return Some(k);
        }
    }
    // Single-device fallback (the snapshot holds only mice/trackpads; Bluetooth keyboards are
    // already excluded).
    if snapshot.devices.len() == 1 {
        let device = &snapshot.devices[0];
        return Some((device.vendor_id, device.product_id));
    }
    None
}

/// Test-only control of the lane: a hold that parks the lane inside a batch so a test can prove
/// the readers and the tap path never wait for it, and a counter that says a batch finished.
/// Test-only constructor: the snapshot fields are private, so a consumer's test cannot build one
/// without going through here.
#[cfg(test)]
pub(crate) fn snapshot_for_test(
    status: SnapshotStatus,
    devices: Vec<DeviceIdentity>,
) -> DeviceSnapshot {
    let properties = vec![DeviceProperties::default(); devices.len()];
    DeviceSnapshot {
        devices,
        properties,
        attribution: Vec::new(),
        status,
        generation: 1,
    }
}

#[cfg(test)]
mod lane_harness {
    use super::*;
    use std::sync::Condvar;

    pub(super) struct Hold {
        entered: (Mutex<bool>, Condvar),
        release: (Mutex<bool>, Condvar),
        completed: (Mutex<u64>, Condvar),
    }

    impl Hold {
        pub(super) fn new() -> Arc<Self> {
            Arc::new(Self {
                entered: (Mutex::new(false), Condvar::new()),
                release: (Mutex::new(false), Condvar::new()),
                completed: (Mutex::new(0), Condvar::new()),
            })
        }

        pub(super) fn wait_entered(&self) {
            let mut entered = self.entered.0.lock().unwrap();
            while !*entered {
                entered = self.entered.1.wait(entered).unwrap();
            }
        }

        pub(super) fn release(&self) {
            *self.release.0.lock().unwrap() = true;
            self.release.1.notify_all();
        }

        /// Wait (bounded) for the batch this hold released to finish.
        pub(super) fn wait_idle(&self) {
            let completed = self.completed.0.lock().unwrap();
            let (completed, timeout) = self
                .completed
                .1
                .wait_timeout_while(completed, Duration::from_secs(5), |done| *done == 0)
                .unwrap();
            assert!(
                !timeout.timed_out() || *completed > 0,
                "the lane never finished its batch"
            );
        }

        fn finished(&self) {
            *self.completed.0.lock().unwrap() += 1;
            self.completed.1.notify_all();
        }
    }

    static CURRENT: Mutex<Option<Arc<Hold>>> = Mutex::new(None);

    /// Serializes the tests that submit to the lane or replace the published snapshot.
    pub(super) static SERIAL: Mutex<()> = Mutex::new(());

    pub(super) fn install(hold: Option<Arc<Hold>>) {
        *CURRENT.lock().unwrap() = hold;
    }

    /// Park the lane at the top of its batch, before any slow work.
    pub(super) fn hold_if_requested() {
        let hold = CURRENT.lock().unwrap().clone();
        let Some(hold) = hold else {
            return;
        };
        *hold.entered.0.lock().unwrap() = true;
        hold.entered.1.notify_all();
        let mut released = hold.release.0.lock().unwrap();
        while !*released {
            released = hold.release.1.wait(released).unwrap();
        }
    }

    /// Whether this batch must stay out of IOKit (a test process still has real devices).
    pub(super) fn active_hold() -> Option<Arc<Hold>> {
        CURRENT.lock().unwrap().clone()
    }

    pub(super) fn finished(hold: &Arc<Hold>) {
        hold.finished();
    }
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
    /// The recovery policy, as a pure decision: the retry batch is paid once per startup / device change,
    /// not once per caller. Every case here is a state the old code got wrong -- it rebuilt the client and
    /// slept 2x50ms for EVERY query while the device list was empty.
    #[test]
    fn recovery_runs_once_per_startup_or_device_change() {
        let now = Instant::now();
        // Startup: nothing has been tried yet, and nothing is enumerated -> the batch is due.
        assert!(recovery_is_due(None, now, true));
        // A caller right after that batch: inside the cooldown -> not due (this is the saving).
        let recent = now - std::time::Duration::from_millis(200);
        assert!(!recovery_is_due(Some(recent), now, true));
        // Still inside the cooldown, just short of it.
        let almost = now - (RECOVERY_COOLDOWN - std::time::Duration::from_millis(1));
        assert!(!recovery_is_due(Some(almost), now, true));
        // The cooldown expired -> due again, so a device that appeared late is still picked up.
        let expired = now - RECOVERY_COOLDOWN;
        assert!(recovery_is_due(Some(expired), now, true));
        // A non-empty list never needs recovery, however long ago the last batch ran.
        assert!(!recovery_is_due(None, now, false));
        assert!(!recovery_is_due(Some(expired), now, false));
    }

    /// The invalidation wiring: a device change clears both budgets, so the next query may recover
    /// again and the next attribution miss may rebuild -- that is the BLE-reconnect promise.
    #[test]
    fn a_device_change_makes_the_next_query_recover_again() {
        let now = Instant::now();
        let mut reg = DeviceRegistry {
            client: std::ptr::null_mut(),
            services: std::ptr::null_mut(),
            devices: Vec::new(),
            recovery_at: Some(now),
            invalidations: 0,
            miss_rebuild_at: Some(now),
            last_enumeration_failed: false,
            client_scheduled: false,
        };
        assert!(!recovery_is_due(reg.recovery_at, now, true));
        assert!(!miss_rebuild_is_due(reg.miss_rebuild_at, now));
        invalidate_recovery(&mut reg);
        assert!(reg.recovery_at.is_none());
        assert!(reg.miss_rebuild_at.is_none());
        assert!(recovery_is_due(reg.recovery_at, now, true));
        assert!(miss_rebuild_is_due(reg.miss_rebuild_at, now));
    }

    /// A miss rebuild is admitted once per cooldown, and the cooldown is independent of the recovery
    /// one: the miss path must not be able to clear the settings path's cooldown, which is what made
    /// the old code pay the full recovery batch on the main thread every time it ran.
    #[test]
    fn an_attribution_miss_rebuilds_once_per_cooldown() {
        let now = Instant::now();
        // First miss after startup: nothing has rebuilt yet -> admitted.
        assert!(miss_rebuild_is_due(None, now));
        // Every miss inside the window is admitted by the same pending rebuild: not one rebuild each.
        assert!(!miss_rebuild_is_due(
            Some(now - Duration::from_millis(1)),
            now
        ));
        assert!(!miss_rebuild_is_due(
            Some(now - (MISS_REBUILD_COOLDOWN - Duration::from_millis(1))),
            now
        ));
        // Past the window a stale client is healed again.
        assert!(miss_rebuild_is_due(Some(now - MISS_REBUILD_COOLDOWN), now));
        // The two budgets are separate: a miss rebuild does not re-open the recovery batch.
        let recent_attempt = Some(now - Duration::from_millis(100));
        assert!(!recovery_is_due(recent_attempt, now, true));
        assert!(miss_rebuild_is_due(Some(now - MISS_REBUILD_COOLDOWN), now));
    }

    /// Requests coalesce, and none of them may be dropped: the lane answers a batch once, and a
    /// reason that disappeared would silently leave a caller (or the picker) stale.
    #[test]
    fn pending_requests_keep_every_reason_and_coalesce_repeats() {
        let mut pending = PendingRequests::default();
        assert!(pending.is_empty());
        pending.merge(LaneRequest::AttributionMiss);
        pending.merge(LaneRequest::AttributionMiss);
        pending.merge(LaneRequest::EnsureEnumerated);
        pending.merge(LaneRequest::DeviceChanged { is_removal: false });
        pending.merge(LaneRequest::DeviceChanged { is_removal: true });
        pending.merge(LaneRequest::Recheck { force: false });
        pending.merge(LaneRequest::Recheck { force: true });
        assert_eq!(pending.misses, 2);
        assert!(pending.ensure);
        assert_eq!(
            pending.changed,
            Some(true),
            "a removal outranks a matching event (the client cache is dead after it)"
        );
        assert!(pending.recheck && pending.recheck_force);
        assert!(!pending.is_empty());
        // A settings query implies the enumeration it needs; it does not imply a device change.
        let mut settings = PendingRequests::default();
        settings.merge(LaneRequest::SettingsQuery);
        assert!(settings.ensure && settings.settings);
        assert_eq!(settings.changed, None);
    }

    fn identity(vendor: u32, product: u32, name: &str) -> DeviceIdentity {
        DeviceIdentity {
            vendor_id: vendor,
            product_id: product,
            name: name.to_string(),
            transport: String::new(),
        }
    }

    /// The snapshot is the tap's attribution table and the settings page's property source, so both
    /// mappings are pinned: a registry id resolves to its device, and a device's properties stay
    /// paired with it by index.
    #[test]
    fn a_snapshot_maps_registry_ids_and_keeps_properties_paired_with_devices() {
        let properties = DeviceProperties {
            accel_type: Some(KEY_MOUSE_ACCEL.to_string()),
            accel_values: HashMap::from([(KEY_MOUSE_ACCEL.to_string(), 65_536)]),
        };
        let snapshot = DeviceSnapshot {
            devices: vec![identity(0x046D, 0xC52B, "MX Master")],
            properties: vec![properties.clone()],
            attribution: vec![(42, (0x046D, 0xC52B))],
            status: SnapshotStatus::Ready,
            generation: 1,
        };
        assert_eq!(snapshot.device_for_registry_id(42), Some((0x046D, 0xC52B)));
        assert_eq!(snapshot.device_for_registry_id(43), None);
        assert!(snapshot.holds((0x046D, 0xC52B)));
        assert!(!snapshot.holds(VIRTUAL_DEVICE_KEY));
        let paired = snapshot
            .properties_for((0x046D, 0xC52B))
            .expect("the device's own properties");
        assert_eq!(paired, &properties);
        assert!(!paired.has_pointer_acceleration());
        assert_eq!(paired.accel_value(KEY_MOUSE_ACCEL), Some(65_536));
        assert_eq!(paired.accel_value(KEY_POINTER_ACCEL), None);
        assert!(snapshot.properties_for((0x1234, 0x5678)).is_none());
    }

    /// "We have not asked yet" and "we asked and there are none" are different answers; the settings
    /// page must be able to tell them apart (and take different copy) instead of claiming no device.
    #[test]
    fn not_queried_is_not_an_empty_answer() {
        let unknown = DeviceSnapshot::empty(SnapshotStatus::NotQueried);
        let empty = DeviceSnapshot::empty(SnapshotStatus::Ready);
        assert_eq!(unknown.status, SnapshotStatus::NotQueried);
        assert_eq!(empty.status, SnapshotStatus::Ready);
        assert!(unknown.devices.is_empty() && empty.devices.is_empty());
    }

    /// A publish is the settings page's wake-up: an identical batch must not re-render it, and a
    /// failure or a device change must.
    #[test]
    fn a_publish_only_notifies_when_something_changed() {
        let _serial = lane_harness::SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *SNAPSHOT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        assert!(publish_if_changed(DeviceSnapshot::empty(SnapshotStatus::Ready)).is_some());
        assert!(
            publish_if_changed(DeviceSnapshot::empty(SnapshotStatus::Ready)).is_none(),
            "an identical batch must not wake the settings window"
        );
        assert!(
            publish_if_changed(DeviceSnapshot::empty(SnapshotStatus::Failed)).is_some(),
            "a failed enumeration is news: the page must not keep showing a stale list as current"
        );
        let with_device = DeviceSnapshot {
            devices: vec![identity(0x046D, 0xC52B, "MX Master")],
            properties: vec![DeviceProperties::default()],
            attribution: vec![(42, (0x046D, 0xC52B))],
            status: SnapshotStatus::Ready,
            generation: 0,
        };
        assert!(publish_if_changed(with_device).is_some());
        *SNAPSHOT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }

    /// The counter-example to the defect this change fixes: while the lane is inside a batch, both
    /// the settings reader and the tap's miss path must return immediately. They used to run the
    /// enumeration themselves (the main thread) or arm a rebuild per event (the tap thread).
    #[test]
    fn a_busy_lane_never_blocks_a_reader_or_the_tap() {
        let _serial = lane_harness::SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *SNAPSHOT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        let hold = lane_harness::Hold::new();
        // The guard releases the lane even if an assertion below panics, so no later test hangs.
        struct Release(Arc<lane_harness::Hold>);
        impl Drop for Release {
            fn drop(&mut self) {
                self.0.release();
                self.0.wait_idle();
                lane_harness::install(None);
                *SNAPSHOT
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
            }
        }
        lane_harness::install(Some(Arc::clone(&hold)));
        let _release = Release(Arc::clone(&hold));
        // Park the lane inside its first batch.
        ensure_enumerated();
        hold.wait_entered();

        let started = Instant::now();
        let _devices = connected_devices();
        let properties = snapshot().properties_for((0x046D, 0xC52B)).cloned();
        let reader_elapsed = started.elapsed();
        // No HID device can be published while the lane is parked, so there is nothing to report;
        // the point of the assertion is that the reader did not wait for the lane to say so.
        assert!(properties.is_none());
        assert!(
            reader_elapsed < Duration::from_millis(50),
            "the settings reader waited {reader_elapsed:?} for a busy lane"
        );

        let started = Instant::now();
        submit(LaneRequest::AttributionMiss);
        let tap_elapsed = started.elapsed();
        assert!(
            tap_elapsed < Duration::from_millis(50),
            "the tap's miss path waited {tap_elapsed:?} for a busy lane"
        );
    }

    /// The delayed-recheck handoff must not strand a forced reason. The old implementation cleared its
    /// pending flag before acting, so a `force` that arrived in that window was accepted (flag already
    /// set) and then dropped; this replays that interleaving against the state machine.
    #[test]
    fn recheck_schedule_never_drops_a_forced_reason() {
        let mut schedule = RecheckSchedule::default();
        // First reason starts the thread.
        assert!(schedule.schedule(true));
        assert_eq!(schedule.take(), Some(true));
        // A second forced reason arrives while the thread is still running: no second thread may
        // start, and the reason must survive to the next take.
        assert!(!schedule.schedule(true));
        assert_eq!(
            schedule.take(),
            Some(true),
            "a force recorded while the thread was running must not be lost"
        );
        // An ordinary (unforced) reason is still a reason.
        assert!(!schedule.schedule(false));
        assert_eq!(schedule.take(), Some(false));
        // Nothing pending: the thread ends, and the next reason starts a new one.
        assert_eq!(schedule.take(), None);
        assert!(schedule.schedule(false));
    }

    /// A failed lane start is retried after a backoff instead of being cached for the process: one
    /// transient resource failure must not disable device discovery until relaunch.
    #[test]
    fn a_failed_lane_start_is_retried_after_the_backoff() {
        let now = Instant::now();
        assert!(LaneState::NotStarted.may_start(now));
        let failed = LaneState::Failed {
            last_attempt: now - Duration::from_millis(100),
        };
        assert!(!failed.may_start(now), "no spawn attempt per event");
        let retry = LaneState::Failed {
            last_attempt: now - LANE_SPAWN_RETRY,
        };
        assert!(retry.may_start(now), "the failure is not cached forever");
        let (sender, _receiver) = mpsc::sync_channel::<()>(1);
        assert!(!LaneState::Running(sender).may_start(now));
    }

    /// A lane that cannot run publishes `Failed` while keeping the last known devices: a dead lane is
    /// not "no devices connected" (which would clear the selection and hide the mappings).
    #[test]
    fn an_unavailable_lane_keeps_the_last_known_devices() {
        let _serial = lane_harness::SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *SNAPSHOT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        let known = DeviceSnapshot {
            devices: vec![identity(0x046D, 0xC52B, "MX Master")],
            properties: vec![DeviceProperties::default()],
            attribution: vec![(42, (0x046D, 0xC52B))],
            status: SnapshotStatus::Ready,
            generation: 0,
        };
        assert!(publish_if_changed(known).is_some());
        publish_unavailable();
        let snapshot = snapshot();
        assert_eq!(snapshot.status, SnapshotStatus::Failed);
        assert_eq!(
            snapshot.devices.len(),
            1,
            "a failure must not be presented as every device being unplugged"
        );
        *SNAPSHOT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }

    /// A pointer-reapply is work like any other: it must survive the empty-batch test and the
    /// miss-only shortcut, or a config change would be accepted and never applied.
    #[test]
    fn a_pointer_reapply_is_never_dropped_as_an_empty_batch() {
        let mut pending = PendingRequests::default();
        pending.merge(LaneRequest::PointerReapply);
        assert!(!pending.is_empty(), "a lone re-apply is a batch");
        assert!(!pending.only_misses(), "a re-apply is not a miss");
        // Merged with misses that may be throttled, the apply still keeps the batch alive.
        pending.merge(LaneRequest::AttributionMiss);
        assert!(!pending.only_misses());
        // Misses alone keep the shortcut.
        let mut misses = PendingRequests::default();
        misses.merge(LaneRequest::AttributionMiss);
        assert!(misses.only_misses());
    }

    /// The selection policy: only a successful answer may clear or move it. A list that has not been
    /// queried yet, or whose query failed, is not evidence that the device is gone.
    #[test]
    fn only_an_answer_may_change_the_device_selection() {
        let device = identity(0x046D, 0xC52B, "MX Master");
        let key = (0x046D, 0xC52B);
        let list = [device];
        // Not queried / failed: keep whatever the user had, even when the list is empty.
        assert_eq!(
            selection_after_refresh(Some(key), &[], SnapshotStatus::NotQueried),
            None
        );
        assert_eq!(
            selection_after_refresh(Some(key), &[], SnapshotStatus::Failed),
            None
        );
        // A successful empty answer is "nothing is connected".
        assert_eq!(
            selection_after_refresh(Some(key), &[], SnapshotStatus::Ready),
            Some(None)
        );
        // An answer that still holds the choice leaves it alone.
        assert_eq!(
            selection_after_refresh(Some(key), &list, SnapshotStatus::Ready),
            None
        );
        // An answer without it falls back to the first device.
        assert_eq!(
            selection_after_refresh(Some((1, 2)), &list, SnapshotStatus::Ready),
            Some(Some(key))
        );
        assert_eq!(
            selection_after_refresh(None, &list, SnapshotStatus::Ready),
            Some(Some(key))
        );
    }

    /// The quit boundary is a decision the lane consults before every write, and the refusal is
    /// re-checked under the write lock: an admission that happened before the flag flipped must still
    /// not write after the quit path took the lock and restored.
    #[test]
    fn a_pointer_write_admitted_before_the_quit_boundary_is_still_refused() {
        assert!(pointer_write_allowed(false));
        assert!(!pointer_write_allowed(true));
        SHUTTING_DOWN.store(true, Ordering::SeqCst);
        let mut written = false;
        // `admitted = true` is the lane's first read, taken before the flag flipped.
        let allowed = pointer_write(true, || written = true);
        assert!(!allowed, "the re-check under the lock must refuse");
        assert!(
            !written,
            "nothing may be written after the restore boundary"
        );
        SHUTTING_DOWN.store(false, Ordering::SeqCst);
        // The boundary is not a permanent off switch for a process that is still running: with the
        // flag clear, an admitted write runs.
        assert!(pointer_write(true, || written = true));
        assert!(written);
    }

    /// A recovery batch that lost its race against a device change must not record its attempt: the change
    /// has to keep the next query free to recover.
    #[test]
    fn a_batch_that_lost_its_race_does_not_record_its_attempt() {
        assert!(may_record_recovery(4, 4));
        assert!(!may_record_recovery(4, 5));
        assert!(!may_record_recovery(5, 4));
    }
}
