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
//! A miss submits one coalesced recovery request (admission in [`miss_recovery_is_due`]) and falls
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
    /// When the last attribution-miss recovery attempt ran (see `miss_recovery_is_due`).
    /// Separate from `recovery_at`: a miss attempt must not clear the recovery cooldown the
    /// settings path uses.
    miss_recovery_at: Option<Instant>,
    /// Consecutive miss-driven attempts that did not attribute every missed sender. Drives both
    /// the cooldown backoff (`miss_recovery_cooldown`) and the rebuild escalation
    /// (`miss_attempt_wants_rebuild`): a miss the enumeration cannot heal is stale
    /// enumeration whatever it returned -- an empty list, or a non-empty list whose cache
    /// still lacks a sender's device.
    miss_unhealed_streak: u32,
    /// Whether the last enumeration could reach the system at all (client creation / CopyServices).
    /// A failed pass publishes `SnapshotStatus::Failed` and keeps the previous device list, so a
    /// transient IOKit failure is never reported as "no devices connected".
    last_enumeration_failed: bool,
    /// The (vid, pid, instance tag) keys last CONFIRMED by a successful read -- a conclusive
    /// recheck diff or a committed enumeration. Unlike the committed list itself it survives
    /// a failed commit: a device the diff saw but the rebuild could not publish still counts
    /// as confirmed on the next inconclusive pass, which must not let a partial fresh answer
    /// publish over it.
    confirmed_keys: Vec<(u32, u32, u64)>,
    /// Whether the settled client's cache may stand in for a fresh enumeration. A forced
    /// recheck exists because that cache presumptively died mid-event, so it marks the
    /// client distrusted; while the flag holds, every pass -- recheck, settings, miss,
    /// ensure -- takes the create path instead of reading or falling back to the
    /// possibly-dead answer, until a fresh client commits and clears it.
    client_distrusted: bool,
}

unsafe impl Send for DeviceRegistry {}
unsafe impl Sync for DeviceRegistry {}

static REGISTRY: OnceLock<Mutex<DeviceRegistry>> = OnceLock::new();

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
            miss_recovery_at: None,
            miss_unhealed_streak: 0,
            last_enumeration_failed: false,
            confirmed_keys: Vec::new(),
            client_distrusted: false,
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

/// The device lane's own runloop. The registry client is scheduled here rather than on the
/// event tap's: the lane's loop outlives every tap restart, so a scheduled client's matching
/// can always be pumped (`copy_services_settled` runs it while waiting) and enumeration
/// never depends on whether a tap runloop exists right now.
unsafe fn lane_runloop() -> crate::event_tap::CFRunLoopRef {
    crate::event_tap::CFRunLoopGetCurrent()
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
/// Returns whether the pass created an IOHIDEventSystemClient -- explicit rebuild or a
/// null-client implicit create. The lane counts it against the batch's single-creation
/// budget: a null client turns every later retry into another create, which is the same
/// settle race a rebuild pays. Failure itself is reported through `last_enumeration_failed`.
/// `expected_senders` carries identities the settle poll must see among the kept services
/// before it may exit early: unattributed senders a miss batch is trying to heal, or the
/// registry ids a recheck already observed. It is an IDENTITY check only -- a different set
/// with the same count does not satisfy it.
/// `required_keys` is the regression set: the (vid, pid, tag) keys the settled client just
/// confirmed, where rid'd keys match by identity and id-less ones by VID/PID (see
/// `services_cover_keys`). A just-created client whose answer does not cover the whole set is
/// provably partial, so the pass commits the settled client's own current answer instead.
/// A merely ADVISORY expectation -- an unattributed sender the enumeration is still looking
/// for -- does not belong here: its absence must not discard devices the candidate found.
/// `min_kept` is the settle poll's count floor outright (the caller composes it: previously
/// confirmed count, recheck-observed count, or `usize::MAX`, which disables the early exit
/// entirely so the pass waits out the whole matching window).
/// Returns `(created, installed)`: a client was created (budget accounting) and whether the
/// commit actually installed it -- a fallback keeps the settled client while adopting its
/// fresh service list. A distrusted settled client (`reg.client_distrusted`, marked by the
/// forced recheck) may not serve as the enumeration source nor the arbitration answer
/// either: its cache is presumptively dead, so every pass takes the create path and a
/// failed create or read keeps the last list and flags the pass instead of publishing
/// the possibly-stale one.
unsafe fn enumerate_locked(
    reg: &mut DeviceRegistry,
    rebuild_client: bool,
    expected_senders: &[u64],
    required_keys: &[(u32, u32, u64)],
    min_kept: usize,
) -> (bool, bool) {
    if skip_device_enumeration_in_smoke() {
        // Short-lived GUI smokes manipulate settings state directly and do not test physical
        // devices. Avoid a scheduled IOHIDEventSystemClient whose exit-time release can race its
        // run-loop unschedule (observed as a CoreFoundation PAC trap).
        reg.devices.clear();
        reg.confirmed_keys.clear();
        reg.last_enumeration_failed = false;
        return (false, false);
    }

    let mut candidate_client = reg.client;
    // Whether the settled client may stand in for a fresh enumeration. A distrusted one
    // cannot: every pass then takes the create path, and the fallback gate below refuses
    // its arbitration answer for the same reason.
    let settled_trusted = !reg.client_distrusted;
    // Whether THIS pass scheduled the client: only then does matching still need the bounded
    // settle window `copy_services_settled` provides. An already-scheduled client settled long
    // ago on the same lane runloop, which never goes away.
    let mut scheduled_this_pass = false;
    // Matching on IOHIDEventSystemClient is asynchronous: CopyServices right after creation can
    // return an empty list (measured 0/1 flapping), so a fresh client waits for matching to settle.
    // Only the fresh-client path pays it; reusing an existing client does not.
    let mut created_client = false;
    if enumeration_would_create(reg, rebuild_client) {
        let client = IOHIDEventSystemClientCreate(std::ptr::null());
        if client.is_null() {
            log_info!("[device] failed to create IOHIDEventSystemClient");
            if candidate_client.is_null() || !settled_trusted {
                // Nothing usable to read: a null candidate has no client, and a pass that
                // distrusts the settled cache may not publish its possibly-dead answer.
                reg.last_enumeration_failed = true;
                return (false, false);
            }
            // Keep the settled client as the candidate: the reuse read below answers with the
            // freshest list it holds -- a failed create must not strand devices that read
            // already confirmed.
        } else {
            // Match the Generic Desktop page (filtered by usage below).
            let page_key = make_nsstring(KEY_PRIMARY_USAGE_PAGE);
            let page_val: *mut AnyObject =
                msg_send![class!(NSNumber), numberWithInt: USAGE_PAGE_GENERIC_DESKTOP as i32];
            let dict: *mut AnyObject =
                msg_send![class!(NSDictionary), dictionaryWithObject: page_val, forKey: page_key];
            let arr: *mut AnyObject = msg_send![class!(NSArray), arrayWithObject: dict];
            IOHIDEventSystemClientSetMatchingMultiple(client, arr as *const c_void);
            CFRelease(page_key as *const c_void);
            // Schedule BEFORE waiting: matching is asynchronous and an unscheduled client's
            // matching does not settle (measured), so a sleep ahead of this line gave the client
            // nothing to settle and the CopyServices after it raced the delivery (measured 0/1
            // flapping). The client schedules on the LANE's runloop (see `lane_runloop`), and the
            // settle wait is `copy_services_settled`'s bounded poll, which pumps it.
            IOHIDEventSystemClientScheduleWithRunLoop(
                client,
                lane_runloop(),
                crate::event_tap::kCFRunLoopDefaultMode,
            );
            scheduled_this_pass = true;
            candidate_client = client;
            created_client = true;
        }
    }

    // Bluetooth classification table (bluetoothd's BluetoothInfo cache in NVRAM). On parse
    // failure it is empty and every Bluetooth device falls back to HID-only classification,
    // preserving the previous behavior. Built before the settle poll so the poll's early-exit
    // test sees the same keep-filter the device list below applies.
    let appearance_map = bluetooth_appearance_map();

    // CopyServices answers what matching has delivered so far: an existing client was scheduled
    // long ago and answers immediately, a just-scheduled one needs a bounded settle window. The
    // early-exit target is the caller-composed count floor (`min_kept`), plus every expected
    // identity actually arriving among the kept services: an answer short of either can be
    // matching still delivering, so the poll keeps waiting rather than publish a list that
    // drops a device which only arrived late. `usize::MAX` for a full-window pass: the count
    // test can never pass, so the poll waits out the entire matching window. A distrusted pass
    // always waits the whole window: its premise is a change inside the debounce, so the count
    // floor itself may describe a membership that is still arriving.
    let expected_kept = if settled_trusted {
        min_kept.max(1)
    } else {
        usize::MAX
    };
    let (mut services, _senders_met) = copy_services_settled(
        candidate_client,
        scheduled_this_pass,
        &appearance_map,
        expected_kept,
        expected_senders,
    );
    // A just-created client whose answer does not cover every REQUIRED key produced a provably
    // partial answer -- or no answer at all: a null read loses the same confirmed devices. The
    // settled client's own current answer is the freshest truth it holds (the recheck diff
    // itself trusts that list), so prefer it over a partial or missing candidate answer:
    // rebuilding again would only restart matching from zero.
    //
    // Ambiguous coverage -- an id-less required key satisfied by VID/PID alone -- does NOT
    // trigger this: a fresh client that waited out its full matching window is the most
    // authoritative single answer (it cannot be stale-cache-blind to a live device, while
    // the settled client's cache is the documented failure a rebuild exists to repair). A
    // confirmed device the new client genuinely cannot see is recovered by the miss-heal
    // path on its next event, without user input.
    let required_met =
        !services.is_null() && services_cover_keys(services, required_keys, &appearance_map);
    let mut committed_created = created_client;
    if should_fallback_to_settled_client(
        settled_trusted,
        created_client,
        required_met,
        reg.client.is_null(),
    ) {
        let (fallback, _) = copy_services_settled(reg.client, false, &appearance_map, 1, &[]);
        match partial_verdict(!fallback.is_null()) {
            PartialVerdict::CommitSettled => {
                log_info!(
                    "[device] rebuild's fresh client missed confirmed device(s); committing the settled client's answer"
                );
                if !services.is_null() {
                    CFRelease(services as *const c_void);
                }
                CFRelease(candidate_client as *const c_void);
                services = fallback;
                committed_created = false;
            }
            PartialVerdict::KeepLastList => {
                // No settled answer either: committing the provably-partial candidate would
                // publish a regression as Ready. Keep the last list (its devices' service
                // pointers still point at the still-installed client) and flag the pass
                // failed, so the next recheck takes a full pass instead of trusting the
                // diff.
                log_info!(
                    "[device] rebuild's fresh client is partial and the settled client could not answer; keeping the last list"
                );
                if !services.is_null() {
                    CFRelease(services as *const c_void);
                }
                CFRelease(candidate_client as *const c_void);
                reg.last_enumeration_failed = true;
                return (created_client, false);
            }
        }
    }
    if services.is_null() {
        log_debug!("[device] no services returned");
        if created_client {
            CFRelease(candidate_client as *const c_void);
        }
        reg.last_enumeration_failed = true;
        return (created_client, false);
    }

    let mut devices: Vec<Device> = Vec::new();

    let count = CFArrayGetCount(services);
    for i in 0..count {
        let service = CFArrayGetValueAtIndex(services, i) as *mut c_void;
        // Determine pointer/mouse/trackpad via ConformsTo (see `service_is_pointer`) instead of
        // the PrimaryUsage scalar; Bluetooth keyboards that fake a Mouse usage are weeded out
        // by the GAP Appearance check below.
        if !service_is_pointer(service) {
            continue;
        }
        let vid = prop_int(service, KEY_VENDOR_ID).unwrap_or(0) as u32;
        let pid = prop_int(service, KEY_PRODUCT_ID).unwrap_or(0) as u32;
        let name = prop_string(service, KEY_PRODUCT);
        let transport = prop_string(service, KEY_TRANSPORT);

        if is_bluetooth_keyboard(service, &appearance_map) {
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

    // Swap first and release the replaced CF objects only after the swap: the old device list
    // pointed into the old services array, so it may be dropped once nothing references it.
    let old_services = std::mem::replace(&mut reg.services, services);
    let old_client = if committed_created {
        // The fresh client is installed: whatever doubt the forced pass had about the
        // settled cache dies with it.
        reg.client_distrusted = false;
        std::mem::replace(&mut reg.client, candidate_client)
    } else {
        std::ptr::null_mut()
    };
    reg.devices = devices;
    reg.last_enumeration_failed = false;
    // A committed list is the freshest confirmed key set: it replaces both a conclusive
    // diff read and any older floor an inconclusive pass would otherwise fall back to.
    reg.confirmed_keys = reg
        .devices
        .iter()
        .map(|d| {
            (
                d.identity.vendor_id,
                d.identity.product_id,
                device_instance_tag(d.service_client),
            )
        })
        .collect();
    if !old_services.is_null() {
        CFRelease(old_services as *const c_void);
    }
    if !old_client.is_null() {
        CFRelease(old_client as *const c_void);
    }
    log_debug!(
        "[device] enumerated {} pointer device(s) from {} service(s).",
        reg.devices.len(),
        count
    );
    (created_client, committed_created)
}

/// Whether a service declares a pointer usage in its full DeviceUsagePairs. Some real mice
/// (e.g. ATK A9 SE Nearlink devices) report PrimaryUsage = Keyboard(6), and a {1,2,5} whitelist
/// would drop them; ConformsTo inspects the full pairs and sees the Mouse(1,2)/Pointer(1,1)/
/// Trackpad(1,5) usages they declare. Side effect: a few keyboards also declare extra Mouse
/// usages (e.g. KZI I75) and get included -- Bluetooth keyboards are weeded out by the GAP
/// Appearance check at the call site; attribution uses exact senderID matching, so this never
/// affects correctness.
unsafe fn service_is_pointer(service: *mut c_void) -> bool {
    IOHIDServiceClientConformsTo(service, 1, USAGE_GD_POINTER as u32) != 0
        || IOHIDServiceClientConformsTo(service, 1, USAGE_GD_MOUSE as u32) != 0
        || IOHIDServiceClientConformsTo(service, 1, USAGE_GD_TRACKPAD as u32) != 0
}

/// Whether a service is excluded by its Bluetooth GAP Appearance: some keyboard firmware fakes
/// mouse usages in the HID descriptor (e.g. KZI I75), fooling the ConformsTo filter; Appearance
/// is the self-reported class in the Bluetooth advertisement, the same source as the macOS
/// Bluetooth pane (0x03C1 = keyboard). DeviceAddress exists only on Bluetooth-transport
/// devices; devices absent from the NVRAM cache (e.g. freshly paired) fall back to HID-only
/// classification and are never false-positively dropped.
unsafe fn is_bluetooth_keyboard(
    service: *mut c_void,
    appearance_map: &HashMap<String, u16>,
) -> bool {
    let addr = prop_string(service, KEY_DEVICE_ADDRESS);
    !addr.is_empty() && appearance_map.get(&addr.to_uppercase()) == Some(&GAP_APPEARANCE_KEYBOARD)
}

/// Whether a service survives every device-list filter: the usage check plus the exclusions
/// applied when the list is built. The settle poll's early-exit test uses the same filter, so
/// a service that would be dropped anyway (a keyboard, or a Bluetooth keyboard faking a Mouse
/// usage) does not end the settle window early.
unsafe fn service_kept(service: *mut c_void, appearance_map: &HashMap<String, u16>) -> bool {
    service_is_pointer(service) && !is_bluetooth_keyboard(service, appearance_map)
}

/// How many services in the array survive the device-list filter. The client matches the
/// whole Generic Desktop page, so services that will be dropped (keyboards) can arrive ahead
/// of the pointer ones -- a list that holds only dropped services is still partial.
unsafe fn services_kept_count(
    services: *mut c_void,
    appearance_map: &HashMap<String, u16>,
) -> usize {
    let count = CFArrayGetCount(services);
    (0..count)
        .filter(|i| {
            service_kept(
                CFArrayGetValueAtIndex(services, *i) as *mut c_void,
                appearance_map,
            )
        })
        .count()
}

/// How long a just-scheduled client may poll for matching to settle before its result is
/// accepted as-is. Replaces the previous fixed 30ms sleep, which ran before the client was
/// scheduled (where matching could not advance) and still raced the first CopyServices read.
const CLIENT_SETTLE_ATTEMPTS: u32 = 10;
const CLIENT_SETTLE_STEP: Duration = Duration::from_millis(15);

/// Whether the next enumeration pass must create a client rather than reuse the settled one:
/// the batch's one-create budget is charged for exactly these passes, so callers gate on the
/// same predicate the enumeration itself applies. A distrusted settled client counts as a
/// create -- its cache may not serve as the enumeration source, whatever was planned.
/// Pure, so the sharing is testable.
fn enumeration_would_create(reg: &DeviceRegistry, rebuild_planned: bool) -> bool {
    rebuild_planned || reg.client.is_null() || reg.client_distrusted
}

/// Whether a rebuild whose fresh client could not confirm every REQUIRED key should commit
/// the settled client's answer instead: only a just-created client can regress on devices
/// the settled one already knows about (a reuse read shares its client's view, and there is
/// no settled client to fall back to on first enumeration). `required_met` is false for a
/// null answer too: a failed read loses the same confirmed devices a partial one does.
/// A pass that distrusts the settled cache (`settled_trusted == false`, the forced recheck)
/// never arbitrates at all -- the possibly-dead list it could hand back is the very thing
/// the pass exists to replace. Coverage that merely rests on a VID/PID match does not count
/// as unmet -- see the comment at the call site for why the fresh full-window answer wins
/// ambiguous cases.
/// Pure, so the decision is testable without IOKit.
fn should_fallback_to_settled_client(
    settled_trusted: bool,
    created: bool,
    required_met: bool,
    old_client_null: bool,
) -> bool {
    settled_trusted && created && !required_met && !old_client_null
}

/// What a provably-partial rebuild candidate leaves behind once the settled client's
/// arbitration answer is known: commit it when it answered (the freshest truth for the
/// missing devices), or keep the last committed list when even it failed -- publishing a
/// provably-partial answer as Ready would drop confirmed devices on one bad read.
/// Pure, so the verdict is testable without IOKit.
#[derive(Debug, PartialEq, Eq)]
enum PartialVerdict {
    CommitSettled,
    KeepLastList,
}

fn partial_verdict(settled_answered: bool) -> PartialVerdict {
    if settled_answered {
        PartialVerdict::CommitSettled
    } else {
        PartialVerdict::KeepLastList
    }
}

/// Whether every key the settled client just confirmed is covered by a kept service in this
/// array. Each service contributes its diff key (vid, pid, instance tag) to `keys_cover`.
unsafe fn services_cover_keys(
    services: *mut c_void,
    keys: &[(u32, u32, u64)],
    appearance_map: &HashMap<String, u16>,
) -> bool {
    let count = CFArrayGetCount(services);
    let available: Vec<(u32, u32, u64)> = (0..count)
        .filter_map(|i| {
            let service = CFArrayGetValueAtIndex(services, i) as *mut c_void;
            service_kept(service, appearance_map).then(|| {
                (
                    prop_int(service, KEY_VENDOR_ID).unwrap_or(0) as u32,
                    prop_int(service, KEY_PRODUCT_ID).unwrap_or(0) as u32,
                    device_instance_tag(service),
                )
            })
        })
        .collect();
    keys_cover(keys, &available)
}

/// Pure core of `services_cover_keys`: whether `required` is covered by `available`, each
/// available key used at most once. A rid'd required key (bit 63 clear) matches an available
/// key with the same tag; an id-less required key can only match on VID/PID -- its
/// pointer-tag differs across clients by construction, so two same-model arrivals cannot
/// both claim a single seen service, and an answer with the right COUNT but a different set
/// is still provably partial. Rid'd requirements claim first: an id-less requirement's
/// VID/PID-only match could otherwise consume the very service a rid'd requirement needs
/// when two same-model devices are present, making the verdict order-dependent.
fn keys_cover(required: &[(u32, u32, u64)], available: &[(u32, u32, u64)]) -> bool {
    let mut used = vec![false; available.len()];
    let mut ordered = required
        .iter()
        .filter(|key| key.2 & (1 << 63) == 0)
        .chain(required.iter().filter(|key| key.2 & (1 << 63) != 0));
    ordered.all(|&(rv, rp, rtag)| {
        available.iter().enumerate().any(|(i, &(av, ap, atag))| {
            if used[i] {
                return false;
            }
            let hit = if rtag & (1 << 63) == 0 {
                atag == rtag
            } else {
                (av, ap) == (rv, rp)
            };
            used[i] = hit;
            hit
        })
    })
}

/// Whether the array holds a kept service answering for `sender`. A missed sender counts by
/// identity, not count: an unrelated device arriving early must not end the wait for it.
unsafe fn services_hold_sender(
    services: *mut c_void,
    sender: u64,
    appearance_map: &HashMap<String, u16>,
) -> bool {
    let count = CFArrayGetCount(services);
    (0..count).any(|i| {
        let service = CFArrayGetValueAtIndex(services, i) as *mut c_void;
        service_kept(service, appearance_map) && service_registry_id(service) == Some(sender)
    })
}

/// IOHIDEventSystemClientCopyServices with a settle window for a just-scheduled client.
/// An already-scheduled client is read once: its matching settled long ago. A just-scheduled
/// one polls until the answer holds at least `expected_kept` pointer services AND every
/// `expected_senders` id has arrived among them -- a keyboard-only answer is matching still
/// delivering, not an empty machine, and an answer short of the previously confirmed count
/// leaves the late siblings still in flight -- or the window ends; a machine that enumerates
/// no pointer waits out the window, paid only on the just-scheduled path, which stays rare
/// (see `miss_attempt_wants_rebuild`).
/// Returns the services array plus whether every expected identity was found in it: a rebuild
/// whose expected senders never arrived produced a provably partial answer (the recheck just
/// saw those devices on the old client), which the caller may decline to commit.
unsafe fn copy_services_settled(
    client: *mut c_void,
    scheduled_this_pass: bool,
    appearance_map: &HashMap<String, u16>,
    expected_kept: usize,
    expected_senders: &[u64],
) -> (*mut c_void, bool) {
    let senders_met = |services: *mut c_void| {
        !services.is_null()
            && expected_senders
                .iter()
                .all(|sender| services_hold_sender(services, *sender, appearance_map))
    };
    // A full-window pass (an inconclusive recheck) waits out the whole matching window
    // instead of polling for an early answer. The wait must be bounded by a monotonic
    // deadline, not by call count: a handled source makes RunInMode return immediately, so
    // counting calls could burn the whole window in microseconds while arrivals are still
    // queued -- precisely the backlog that triggered this pass.
    if expected_kept == usize::MAX {
        let deadline = Instant::now() + CLIENT_SETTLE_STEP * CLIENT_SETTLE_ATTEMPTS;
        loop {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let remaining = deadline.duration_since(now).as_secs_f64();
            crate::ffi::CFRunLoopRunInMode(
                crate::event_tap::kCFRunLoopDefaultMode as *const c_void,
                remaining.min(CLIENT_SETTLE_STEP.as_secs_f64()),
                1,
            );
        }
        let services = IOHIDEventSystemClientCopyServices(client);
        return (services, senders_met(services));
    }
    if scheduled_this_pass {
        // A just-scheduled client polls until the answer satisfies the count floor and every
        // expected identity -- bounded by a monotonic deadline, not a call count: a handled
        // source makes RunInMode return immediately, so counting calls could burn the whole
        // window in microseconds while the arrival it just delivered is still queued.
        let deadline = Instant::now() + CLIENT_SETTLE_STEP * CLIENT_SETTLE_ATTEMPTS;
        loop {
            let now = Instant::now();
            if now >= deadline {
                let services = IOHIDEventSystemClientCopyServices(client);
                return (services, senders_met(services));
            }
            let remaining = deadline.duration_since(now).as_secs_f64();
            crate::ffi::CFRunLoopRunInMode(
                crate::event_tap::kCFRunLoopDefaultMode as *const c_void,
                remaining.min(CLIENT_SETTLE_STEP.as_secs_f64()),
                1,
            );
            let services = IOHIDEventSystemClientCopyServices(client);
            let settled = !services.is_null()
                && services_kept_count(services, appearance_map) >= expected_kept
                && senders_met(services);
            if settled {
                return (services, true);
            }
            if !services.is_null() {
                CFRelease(services);
            }
        }
    }
    // A reuse read gets two pumped iterations: matching events delivered while the lane was
    // idle only land once the runloop runs, so the first bounded pump drains that backlog
    // and the second (zero-timeout) read answers with what actually arrived. A zero-timeout
    // single read would keep reporting the list from before the backlog.
    for attempt in 0..2 {
        let wait = if attempt == 0 {
            CLIENT_SETTLE_STEP.as_secs_f64()
        } else {
            0.0
        };
        crate::ffi::CFRunLoopRunInMode(
            crate::event_tap::kCFRunLoopDefaultMode as *const c_void,
            wait,
            1,
        );
        let services = IOHIDEventSystemClientCopyServices(client);
        let settled = !services.is_null()
            && services_kept_count(services, appearance_map) >= expected_kept
            && senders_met(services);
        if settled || attempt == 1 {
            return (services, senders_met(services));
        }
        if !services.is_null() {
            CFRelease(services);
        }
    }
    unreachable!()
}

/// Enumerate once at startup (also lazily triggered on first attribution failure).
/// How long a failed query recovery stays authoritative. Repeated queries inside this window reuse the
/// (empty) device list instead of rebuilding the client and re-enumerating with the two retries below.
const RECOVERY_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(2);

/// Whether a query must run the recovery batch: rebuild the client and retry enumeration.
///
/// Pure, so the policy is testable without IOKit. The batch exists for the startup / stale-client /
/// async-matching race (`enumerate_locked`'s settle window can miss a device that is still
/// registering), and
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
    reg.miss_recovery_at = None;
    reg.miss_unhealed_streak = 0;
    reg.invalidations = reg.invalidations.wrapping_add(1);
}

/// Whether a finished recovery batch may record its attempt time: only when no device change invalidated
/// the registry while the batch was running. Pure, so the interleaving is testable.
fn may_record_recovery(batch_generation: u64, current_generation: u64) -> bool {
    batch_generation == current_generation
}

/// How often an attribution miss may trigger a recovery attempt.
///
/// A miss is not a device change: it is the *symptom* of a stale client (a Bluetooth reconnect whose
/// plug events were lost, or a device that appeared after the last enumeration). Recovery
/// re-enumerates the existing, already-scheduled client first -- a rebuild re-enters the
/// fresh-client settle race, so on a machine whose enumeration kept racing, every wheel event
/// missed and every rebuild returned empty again. Consecutive *unhealed* attempts -- the missed
/// sender is still not attributed after the pass, whatever the device list holds -- stretch the
/// cooldown (`miss_recovery_cooldown`) and escalate to one rebuild per episode
/// (`miss_attempt_wants_rebuild`); a healed miss or a real plug clears both (`invalidate_recovery`).
const MISS_RECOVERY_COOLDOWN: Duration = Duration::from_secs(2);
/// The furthest an unhealed streak may stretch the recovery cooldown.
const MISS_RECOVERY_COOLDOWN_MAX: Duration = Duration::from_secs(60);
/// After this many consecutive unhealed attempts, an attempt rebuilds the client instead of
/// re-enumerating it: enumeration itself is then the suspect.
const MISS_REBUILD_AFTER_UNHEALED: u32 = 2;
/// Once the escalation is armed, every this-many further unhealed attempts rebuild again:
/// bounded retries for a stale client, spread by the still-stretching cooldown, without a
/// rebuild every attempt on a machine whose emptiness is legitimate.
const MISS_REBUILD_EVERY: u32 = 4;
/// Timed re-checks one miss episode may run before an attempt created a client or the
/// senders healed: bounds a permanently-failing create from waking the lane forever, while
/// still letting a single event reach several create attempts (the cooldown stretches each
/// retry, so eight spans minutes, not seconds).
const MISS_EPISODE_RETRIES: u8 = 8;
/// Whether an attempted-but-unhealed pass may park its still-missing senders for a timed
/// retry: the episode's budget is bound to the REQUEST, not a global streak -- a sender's
/// timed retries run until one attempt actually INSTALLED a fresh client's answer, the
/// strongest escalation this layer can perform yielding a usable list. A create that ran
/// but whose read failed (`created && !installed`) exercised nothing yet, so the senders
/// stay parked for the next attempt within the remaining budget. The retry counter then
/// bounds how long that persistence may last: once it runs out (or a pass did install)
/// a sender that can never be found stops waking the lane -- fresh input may start a new
/// episode. Pure, so the budget boundary is testable.
fn may_redefer_unhealed(attempt_installed_client: bool, retries_left: u8) -> bool {
    !attempt_installed_client && retries_left > 0
}

/// The cooldown after `unhealed_streak` consecutive unhealed attempts: 2s, 4s, 8s, ..., capped.
/// Pure, so the backoff sequence is testable.
fn miss_recovery_cooldown(unhealed_streak: u32) -> Duration {
    MISS_RECOVERY_COOLDOWN
        .checked_mul(1u32 << unhealed_streak.min(5))
        .unwrap_or(MISS_RECOVERY_COOLDOWN_MAX)
        .min(MISS_RECOVERY_COOLDOWN_MAX)
}

/// Pure admission for a miss-driven recovery attempt; `last` is when the previous one ran.
fn miss_recovery_is_due(last: Option<Instant>, unhealed_streak: u32, now: Instant) -> bool {
    last.is_none_or(|last| now.duration_since(last) >= miss_recovery_cooldown(unhealed_streak))
}

/// Whether the next miss-driven attempt rebuilds the client (true) or re-enumerates the settled
/// one. Once the streak arms the escalation, every `MISS_REBUILD_EVERY`-th further unhealed
/// attempt rebuilds again: a stale client gets bounded retries while the cooldown keeps
/// stretching, and a heal resets the streak to re-arm from scratch.
fn miss_attempt_wants_rebuild(unhealed_streak: u32) -> bool {
    unhealed_streak >= MISS_REBUILD_AFTER_UNHEALED
        && (unhealed_streak - MISS_REBUILD_AFTER_UNHEALED).is_multiple_of(MISS_REBUILD_EVERY)
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
    /// Attribution met a sender the published snapshot does not hold. The sender id rides
    /// along so the batch can tell whether its enumeration actually healed the miss.
    AttributionMiss { sender: u64 },
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
    /// How many attribution misses arrived as real input events; a count is enough (one
    /// recovery answers all of them) but the number belongs in the batch report.
    misses: u32,
    /// Missed senders resubmitted by a deferred promotion: they drive the same recovery but
    /// are not input events, so they count separately from `misses`.
    miss_retries: u32,
    /// The distinct sender ids that missed, deduplicated and capped. A batch heals only when
    /// *every* recorded sender resolves: one device recovering while another stays missing is
    /// still a stale enumeration, not a healed one.
    miss_senders: [u64; 4],
    miss_sender_count: u8,
    /// A distinct sender arrived after the set filled: it is not recorded, so an all-attributed
    /// answer for the recorded ones must not be read as the whole batch healing.
    miss_senders_overflow: bool,
    /// Timed re-checks this batch's miss episode may still run (`MISS_EPISODE_RETRIES` for a
    /// fresh request, the deferred entry's remainder after a promotion). A consumed attempt
    /// decrements it; a fresh input event re-arms it, so the budget belongs to the newest
    /// request rather than a global streak.
    miss_retry_budget: u8,
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
    /// Whether the batch carries any miss work: fresh input events or deferred resubmissions.
    fn has_misses(&self) -> bool {
        self.misses > 0 || self.miss_retries > 0
    }

    fn is_empty(&self) -> bool {
        !self.ensure
            && !self.settings
            && self.changed.is_none()
            && !self.recheck
            && !self.has_misses()
            && !self.pointer_reapply
    }

    /// Whether attribution misses are the *only* reason. A miss that is not admitted is dropped
    /// without publishing, so a co-batched reason must keep the batch alive.
    fn only_misses(&self) -> bool {
        self.has_misses()
            && !self.ensure
            && !self.settings
            && self.changed.is_none()
            && !self.recheck
            && !self.pointer_reapply
    }

    /// The distinct senders recorded so far, newest appended; empty when no miss coalesced.
    fn missed_senders(&self) -> &[u64] {
        &self.miss_senders[..self.miss_sender_count as usize]
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
            LaneRequest::AttributionMiss { sender } => {
                self.merge_miss(sender, true);
            }
            LaneRequest::PointerReapply => self.pointer_reapply = true,
        }
    }

    /// Record a missed sender in this batch's pending state. `fresh` marks a real input
    /// event: only those count toward `misses` and the lifetime `MISSES_SEEN` total, so a
    /// deferred sender's promotion is billed as a resubmission, not the same event twice.
    fn merge_miss(&mut self, sender: u64, fresh: bool) {
        if fresh {
            self.misses = self.misses.saturating_add(1);
            MISSES_SEEN.fetch_add(1, Ordering::Relaxed);
            // A fresh input event starts a new episode: it re-arms the timed-retry budget
            // rather than inheriting whatever an older, already-spent request left.
            self.miss_retry_budget = MISS_EPISODE_RETRIES;
        } else {
            self.miss_retries = self.miss_retries.saturating_add(1);
        }
        let known = &self.miss_senders[..self.miss_sender_count as usize];
        if !known.contains(&sender) {
            if (self.miss_sender_count as usize) < self.miss_senders.len() {
                self.miss_senders[self.miss_sender_count as usize] = sender;
                self.miss_sender_count += 1;
            } else {
                self.miss_senders_overflow = true;
            }
        }
    }
}

/// Senders that missed while the recovery cooldown had not elapsed. Dropping a throttled
/// request loses it for good -- the lane wakes only on new events, and none may come -- so
/// the senders park here until the cooldown ends, when the lane's timed wait promotes them
/// into the batch that is due then.
struct DeferredMiss {
    senders: [u64; 4],
    count: u8,
    /// A distinct sender arrived after the set filled: it is not recorded, so an
    /// all-attributed answer for the recorded ones must not read as healed.
    overflow: bool,
    /// When the cooldown that throttled these senders ends.
    due_at: Instant,
    /// Timed re-checks this episode may still run after a promotion. It crosses the park
    /// boundary here because the pending batch dies with each run; a consumed attempt
    /// decrements it before parking again, and a fresh event re-arms it upstream.
    retries_left: u8,
}

impl DeferredMiss {
    /// Record a throttled batch's senders, deduplicated and capped; the deadline always moves
    /// to the most recent cooldown boundary. Pure, so the set semantics are testable.
    fn record(&mut self, senders: &[u64], overflow: bool, due_at: Instant) {
        self.due_at = due_at;
        for &sender in senders {
            if self.senders[..self.count as usize].contains(&sender) {
                continue;
            }
            if (self.count as usize) < self.senders.len() {
                self.senders[self.count as usize] = sender;
                self.count += 1;
            } else {
                self.overflow = true;
            }
        }
        self.overflow |= overflow;
    }
}

static DEFERRED_MISS: Mutex<Option<DeferredMiss>> = Mutex::new(None);

/// Park the senders of a throttled miss batch until `due_at`. Later deferrals of the same
/// senders deduplicate; the deadline always moves to the most recent cooldown boundary.
fn defer_misses(senders: &[u64], overflow: bool, due_at: Instant, retries_left: u8) {
    // An overflow flag alone still schedules the retry: the unrecorded senders it stands for
    // cannot be verified, so the entry must exist to get them their bounded re-check.
    if senders.is_empty() && !overflow {
        return;
    }
    let mut deferred = DEFERRED_MISS.lock().unwrap();
    let entry = deferred.get_or_insert_with(|| DeferredMiss {
        senders: [0; 4],
        count: 0,
        overflow: false,
        due_at,
        retries_left: 0,
    });
    entry.record(senders, overflow, due_at);
    // Two parks coexisting merge into one episode: the newer request's budget wins, never
    // the spent-down remainder of an older one.
    entry.retries_left = entry.retries_left.max(retries_left);
}

/// Move the parked senders into this batch once their cooldown has ended. Not-yet-due
/// senders stay parked for a later batch.
fn promote_due_deferred(pending: &mut PendingRequests, now: Instant) {
    let mut deferred = DEFERRED_MISS.lock().unwrap();
    let due = deferred.as_ref().is_some_and(|entry| now >= entry.due_at);
    if !due {
        return;
    }
    let entry = deferred.take().unwrap();
    for &sender in &entry.senders[..entry.count as usize] {
        // Not a fresh input event: the miss was already counted when it was submitted.
        pending.merge_miss(sender, false);
    }
    if entry.overflow {
        // The overflow flag stands for senders that were never recorded: they cannot be
        // verified attributed, so the flag itself must drive one bounded re-check.
        pending.miss_retries = pending.miss_retries.saturating_add(1);
        pending.miss_senders_overflow = true;
    }
    // The parked episode's remaining retries ride into the batch that consumes them.
    pending.miss_retry_budget = pending.miss_retry_budget.max(entry.retries_left);
}

/// Drop the parked senders an enumeration resolved: a confirmed sender needs no deferred
/// retry, while the ones still missing keep their deadline. Returns whether the entry
/// survives -- it does on any remainder, or on the overflow flag whose unrecorded senders
/// can never be verified.
fn prune_deferred_senders(entry: &mut DeferredMiss, is_attributed: impl Fn(u64) -> bool) -> bool {
    let mut kept = [0u64; 4];
    let mut kept_count = 0u8;
    for &sender in &entry.senders[..entry.count as usize] {
        if !is_attributed(sender) {
            kept[kept_count as usize] = sender;
            kept_count += 1;
        }
    }
    entry.senders = kept;
    entry.count = kept_count;
    kept_count > 0 || entry.overflow
}

/// Remove the parked senders this batch's enumeration answered. A failed or partial
/// enumeration keeps them, so an unrelated batch cannot lose a sender's scheduled retry.
fn prune_attributed_deferred(reg: &DeviceRegistry) {
    let mut deferred = DEFERRED_MISS.lock().unwrap();
    let Some(entry) = deferred.as_mut() else {
        return;
    };
    let survives = prune_deferred_senders(entry, |sender| {
        reg.devices
            .iter()
            .any(|device| unsafe { device_registry_id(device) } == Some(sender))
    });
    if !survives {
        *deferred = None;
    }
}

/// How long the lane may sleep before the parked senders' cooldown ends; `None` when none
/// are parked.
fn deferred_wait(now: Instant) -> Option<Duration> {
    DEFERRED_MISS
        .lock()
        .unwrap()
        .as_ref()
        .map(|entry| entry.due_at.saturating_duration_since(now))
}

static PENDING: Mutex<PendingRequests> = Mutex::new(PendingRequests {
    ensure: false,
    settings: false,
    changed: None,
    recheck: false,
    recheck_force: false,
    misses: 0,
    miss_retries: 0,
    miss_senders: [0; 4],
    miss_sender_count: 0,
    miss_senders_overflow: false,
    miss_retry_budget: 0,
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
/// across `restore_adopted()`, so an apply that was admitted just before the shutdown flag flipped
/// cannot land after the restore.
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

/// Run the serialized pointer apply/restore once. The writer gets the lane's OWN confirmed
/// services array (+1): a second enumeration through a fresh client could not have settled
/// its matching -- exactly how a reconnected device used to be missed while the registry
/// already listed it. The retained array is released when the write is refused.
/// The lane's confirmed device set for pointer work: the published device list's service
/// clients plus a +1 on the services array that owns them (released by the callee, or by the
/// caller when the write is refused). Null backing when the lane has no confirmed set.
unsafe fn confirmed_services() -> (Vec<*mut c_void>, *mut c_void) {
    let reg = registry().lock().unwrap();
    confirmed_services_locked(&reg)
}

/// `confirmed_services` for a caller that already holds the registry lock -- keeping it held
/// across the subsequent write is how the quit path stops an enumeration commit swapping the
/// installed client (and releasing the very array it snapshotted) mid-restore.
unsafe fn confirmed_services_locked(reg: &DeviceRegistry) -> (Vec<*mut c_void>, *mut c_void) {
    if reg.services.is_null() {
        (Vec::new(), std::ptr::null_mut())
    } else {
        crate::ffi::CFRetain(reg.services as *const c_void);
        (
            reg.devices
                .iter()
                .map(|d| d.service_client)
                .collect::<Vec<*mut c_void>>(),
            reg.services,
        )
    }
}

unsafe fn run_pointer_apply() {
    let admitted = pointer_write_allowed(SHUTTING_DOWN.load(Ordering::SeqCst));
    // The write set is the lane's CONFIRMED device list -- the same service clients the
    // published snapshot names -- not the raw matching array, whose unfiltered entries
    // (e.g. a Bluetooth keyboard faking a Mouse usage) must never receive pointer writes.
    // The +1 on `services` keeps the objects alive for the call and for PointerState.
    let (targets, backing) = confirmed_services();
    if !pointer_write(admitted, move || unsafe {
        crate::mouse::pointer::apply(&targets, backing)
    }) {
        if !backing.is_null() {
            CFRelease(backing as *const c_void);
        }
        log_debug!("[device] pointer write skipped: the process is shutting down");
    }
}

/// Quit-path boundary: refuse further pointer writes, wait for an apply already inside the lock, and
/// restore the system values while still holding it. `restore_adopted` is then the last write to
/// the device -- records first rebind onto live handles from the confirmed set, so a service
/// object the lane replaced since the write is not what the restore targets.
/// The registry lock is held across the snapshot AND the restore: an enumeration commit is
/// the only path that swaps the installed client or releases its services array, and it
/// cannot interleave here -- the restore writes exactly the handles the current list names
/// (the +1 retain alone would keep stale handles alive but could not keep them current).
pub(crate) fn restore_pointer_settings_on_quit() {
    SHUTTING_DOWN.store(true, Ordering::SeqCst);
    let _guard = POINTER_WRITE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    unsafe {
        let reg = registry().lock().unwrap();
        let (targets, backing) = confirmed_services_locked(&reg);
        crate::mouse::pointer::restore_adopted(&targets, backing);
    }
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
        let mut pending = take_pending();
        // Senders parked by a throttled miss are due once their cooldown boundary passes;
        // promoting them here means a timeout alone can produce the retry batch.
        promote_due_deferred(&mut pending, Instant::now());
        if pending.is_empty() {
            match deferred_wait(Instant::now()) {
                // Parked senders give the wait a deadline; an early token still means a new
                // request arrived, so either return wakes the loop.
                Some(wait) => match receiver.recv_timeout(wait) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                },
                None => {
                    if receiver.recv().is_err() {
                        return;
                    }
                }
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
    // path, and waking the lane per event just to compare an unchanged snapshot is waste. Its
    // senders are not dropped either: they park until the cooldown ends so the retry happens
    // even when no further events arrive.
    if pending.only_misses() {
        let (due, retry_at) = {
            let reg = registry().lock().unwrap();
            let now = Instant::now();
            (
                miss_recovery_is_due(reg.miss_recovery_at, reg.miss_unhealed_streak, now),
                reg.miss_recovery_at
                    .map(|at| at + miss_recovery_cooldown(reg.miss_unhealed_streak))
                    .unwrap_or(now),
            )
        };
        if !due {
            defer_misses(
                pending.missed_senders(),
                pending.miss_senders_overflow,
                retry_at,
                pending.miss_retry_budget,
            );
            #[cfg(test)]
            if let Some(hold) = lane_harness::active_hold() {
                lane_harness::finished(&hold);
            }
            return;
        }
    }
    let mut enumerated = false;
    let mut rebuilt = false;
    let mut device_change = false;
    let mut miss_attempted = false;
    let mut reason = "ensure";
    // The pointer properties live on the service-client objects: a batch that swaps the object
    // set (a rebuild, or a re-enumeration that returns new ones) is what loses the applied
    // settings. Captured before any enumeration runs, compared before the apply below.
    let services_before = {
        let reg = registry().lock().unwrap();
        service_ptrs(&reg)
    };
    // The batch's client-creation budget: explicit rebuilds and a null client's implicit
    // create both count, so a batch creates at most one IOHIDEventSystemClient however many
    // reasons it carries.
    let mut client_created = false;

    if pending.recheck_force {
        let mut reg = registry().lock().unwrap();
        // A forced recheck exists because the settled cache presumptively died inside the
        // debounce window: mark it BEFORE any branch enumerates, so a co-batched plug event
        // cannot read or fall back to the possibly-dead answer either.
        reg.client_distrusted = true;
    }

    if let Some(is_removal) = pending.changed {
        reason = if is_removal {
            "plug-removal"
        } else {
            "plug-matching"
        };
        let mut reg = registry().lock().unwrap();
        let floor = reg.devices.len().max(1);
        let (created, installed) = enumerate_locked(&mut reg, true, &[], &[], floor);
        client_created |= created;
        enumerated = true;
        rebuilt = installed;
        device_change = true;
        // A real change may have brought a device the last recovery batch could not see.
        invalidate_recovery(&mut reg);
    } else if pending.recheck {
        // The cheap diff decides whether the delayed recheck costs a rebuild; force skips the diff
        // (a removal followed by a matching event inside the debounce window means the old client's
        // cache is dead, so the diff is not evidence).
        // `inconclusive` marks a diff that could not prove the set either way (drain budget
        // exhausted): the pass it triggers waits out the full matching window so a device
        // still arriving cannot be missed by the count-based early exit. `observed` holds
        // how many pointer services a CONCLUSIVE diff saw, so a changed-set rebuild also
        // waits for the devices the recheck already knows about.
        let mut inconclusive = false;
        // Registry ids the recheck observed on the OLD client: the rebuild's settle poll waits
        // for them by identity, so a new client answering with the same count but different
        // devices cannot publish early.
        let mut observed_senders: Vec<u64> = Vec::new();
        // The regression set a just-created candidate must cover before it may commit
        // (`services_cover_keys`): the diff's fresh key set when the read succeeds, else the
        // last CONFIRMED set -- a device the diff saw but the rebuild could not publish still
        // keeps the next inconclusive pass from publishing a partial answer over it.
        let (changed, observed_keys) = {
            let mut reg = registry().lock().unwrap();
            // The diff keys on (VID, PID, instance tag): VID/PID alone cannot see a
            // same-model swap that happened while the lane was idle.
            let before: Vec<(u32, u32, u64)> = reg
                .devices
                .iter()
                .map(|d| {
                    (
                        d.identity.vendor_id,
                        d.identity.product_id,
                        device_instance_tag(d.service_client),
                    )
                })
                .collect();
            if reg.client_distrusted {
                // Distrust -- set by this batch's forced recheck, or still standing from a
                // failed forced pass -- means the settled answer cannot serve as evidence:
                // the diff must not run on the suspect client at all (its stale read would
                // also pollute `confirmed_keys`). The committed list is no regression floor
                // either (its stale ids would veto a reconnected device's fresh key): wait
                // the whole window, then take what the fresh client actually finds.
                inconclusive = true;
                (true, Vec::new())
            } else if reg.client.is_null() {
                // A null client has nothing to read: the confirmed set is the regression
                // floor (there is no settled client to fall back to, so it only gates the
                // fresh candidate).
                (true, reg.confirmed_keys.clone())
            } else {
                let appearance_map = bluetooth_appearance_map();
                match enumerate_keys(reg.client, &appearance_map) {
                    // A drained backlog gives a trustworthy list; a budget-exhausted drain is
                    // inconclusive -- treat it as changed and let the full pass settle.
                    Some(now) => {
                        // A real registry id (sign bit clear) is comparable across clients;
                        // a pointer-tagged id-less device can only wait via the count.
                        observed_senders = now
                            .iter()
                            .filter_map(|&(_, _, tag)| (tag & (1 << 63) == 0).then_some(tag))
                            .collect();
                        // A conclusive read is the freshest confirmation yet -- keep it for
                        // later inconclusive passes even when the rebuild below fails.
                        reg.confirmed_keys = now.clone();
                        // A failed last pass cannot trust its own list -- the diff is no
                        // evidence, so the pass runs regardless -- but a live read still
                        // names the confirmed set the rebuild must cover before it may
                        // publish (regressions stay impossible, even on a retry pass).
                        (reg.last_enumeration_failed || before != now, now)
                    }
                    None => {
                        // An inconclusive read cannot refresh the confirmed set: the last
                        // confirmation keeps the regression floor while the pass takes
                        // the full-window path.
                        inconclusive = true;
                        (true, reg.confirmed_keys.clone())
                    }
                }
            }
        };
        if changed {
            reason = "recheck";
            let mut reg = registry().lock().unwrap();
            // A confirmed device with no comparable identity (pointer-tagged id-less) cannot
            // be proven present mid-window: the count floor and every rid'd sender can be met
            // by a different membership while it is still arriving. Only waiting out the
            // whole matching window bounds its arrival, so the poll must not exit early.
            // Otherwise the floor is the confirmed set's size -- `observed_keys` may hold an
            // older confirmed set when the diff was inconclusive or the client is dead.
            let min_kept = if inconclusive
                || observed_keys
                    .iter()
                    .any(|&(_, _, tag)| tag & (1 << 63) != 0)
            {
                usize::MAX
            } else {
                observed_keys.len()
            };
            let (created, installed) =
                enumerate_locked(&mut reg, true, &observed_senders, &observed_keys, min_kept);
            client_created |= created;
            enumerated = true;
            rebuilt = installed;
            device_change = true;
            invalidate_recovery(&mut reg);
        }
    }

    // A miss-driven recovery re-enumerates the existing client first: it was scheduled long ago,
    // so its matching has settled and CopyServices answers for real. A rebuild re-enters the
    // fresh-client settle race -- on a machine that kept racing, every miss rebuilt the client,
    // the enumeration came back empty, and the next event missed again. The streak counts
    // attempts that did not heal every missed sender -- an empty answer, a stale non-empty one,
    // or an IOKit failure all count -- and earns a bounded rebuild escalation
    // (`miss_attempt_wants_rebuild`). Not a device change: it must not touch the
    // recovery cooldown the settings path relies on.
    if !enumerated && pending.has_misses() {
        let (due, retry_at) = {
            let reg = registry().lock().unwrap();
            let now = Instant::now();
            (
                miss_recovery_is_due(reg.miss_recovery_at, reg.miss_unhealed_streak, now),
                reg.miss_recovery_at
                    .map(|at| at + miss_recovery_cooldown(reg.miss_unhealed_streak))
                    .unwrap_or(now),
            )
        };
        if due {
            reason = "attribution-miss";
            let mut reg = registry().lock().unwrap();
            let rebuild = miss_attempt_wants_rebuild(reg.miss_unhealed_streak);
            let senders = pending.missed_senders();
            // Every missed sender is unattributed by definition: the settle poll should keep
            // waiting until each one's registry id shows up among the kept services (or the
            // window ends), so a rebuilt list published early cannot drop a late arrival.
            let floor = reg.devices.len().saturating_add(senders.len()).max(1);
            let (created, installed) = enumerate_locked(&mut reg, rebuild, senders, &[], floor);
            client_created |= created;
            reg.miss_recovery_at = Some(Instant::now());
            enumerated = true;
            rebuilt = installed;
            miss_attempted = true;
            // Recovery worked when every sender that missed now resolves to a device. An IOKit
            // failure leaves the list exactly as it was, so it counts as unhealed -- a dead
            // client is precisely what the rebuild escalation exists for.
            let healed = !senders.is_empty()
                && !pending.miss_senders_overflow
                && senders.iter().all(|sender| {
                    reg.devices
                        .iter()
                        .any(|device| device_registry_id(device) == Some(*sender))
                });
            if healed {
                reg.miss_unhealed_streak = 0;
            } else {
                reg.miss_unhealed_streak = reg.miss_unhealed_streak.saturating_add(1);
                // An attempted-but-unhealed pass parks the still-missing senders until the
                // next cooldown boundary: the lane otherwise sleeps until the next input
                // event, and a device that stays silent would never reach a client install.
                // The episode ends once an attempt actually installed a fresh client's
                // answer -- a create whose read failed produced no usable list and does
                // not count -- or once the request's retry budget is spent, so a create
                // that never yields a list stops waking the lane instead of parking forever.
                if may_redefer_unhealed(installed, pending.miss_retry_budget) {
                    let still_missing: Vec<u64> = senders
                        .iter()
                        .copied()
                        .filter(|sender| {
                            !reg.devices
                                .iter()
                                .any(|device| device_registry_id(device) == Some(*sender))
                        })
                        .collect();
                    defer_misses(
                        &still_missing,
                        pending.miss_senders_overflow,
                        Instant::now() + miss_recovery_cooldown(reg.miss_unhealed_streak),
                        pending.miss_retry_budget - 1,
                    );
                }
            }
        } else {
            // Not admitted yet: dropping the request would lose it for good -- the lane only
            // wakes on new events, and if none come the sender is never retried. Park the
            // senders until the cooldown ends; the lane's timed wait promotes them into the
            // batch that is due then. The budget rides along unspent: no attempt ran yet.
            defer_misses(
                pending.missed_senders(),
                pending.miss_senders_overflow,
                retry_at,
                pending.miss_retry_budget,
            );
        }
    }

    // A miss pass that already enumerated still answers the ensure/settings question -- but it
    // is only one pass: when the list is still empty and the settings recovery is due, the
    // recovery batch (its retries and rebuild) is still owed, so it runs unconditionally and
    // gates itself on `recovery_is_due`.
    if pending.ensure || pending.settings {
        if !enumerated {
            reason = if pending.settings {
                "settings"
            } else {
                "ensure"
            };
        }
        rebuilt |= ensure_enumerated_in_lane(&mut client_created);
    }

    // Senders parked by a throttled miss get their retry removed only when this batch's
    // enumeration actually attributed them; a failed or unrelated enumeration keeps them
    // parked for their deadline.
    {
        let reg = registry().lock().unwrap();
        prune_attributed_deferred(&reg);
    }

    // A co-batched enumeration may have answered the miss without the miss branch running:
    // senders it attributed count as healed; the ones it did not reach keep their request --
    // re-park them for the next due batch instead of dropping them with this batch.
    if !miss_attempted && pending.has_misses() {
        let mut reg = registry().lock().unwrap();
        let senders = pending.missed_senders();
        let attributed = |sender: u64| {
            reg.devices
                .iter()
                .any(|device| unsafe { device_registry_id(device) } == Some(sender))
        };
        let all_healed = !senders.is_empty()
            && !pending.miss_senders_overflow
            && senders.iter().all(|sender| attributed(*sender));
        if all_healed {
            reg.miss_unhealed_streak = 0;
        } else {
            let missing: Vec<u64> = senders
                .iter()
                .copied()
                .filter(|sender| !attributed(*sender))
                .collect();
            let retry_at = reg
                .miss_recovery_at
                .map(|at| at + miss_recovery_cooldown(reg.miss_unhealed_streak))
                .unwrap_or_else(Instant::now);
            drop(reg);
            defer_misses(
                &missing,
                pending.miss_senders_overflow,
                retry_at,
                pending.miss_retry_budget,
            );
        }
    }

    // Re-applying restores the pointer properties on the new service-client objects after a
    // reconnect (and it is cheap when nothing changed). It runs on every real device change
    // even when the enumeration failed and the object set is unchanged -- a transactional
    // failure keeps the old list, and the apply is the idempotent repair -- and whenever the
    // object set actually changed, so a routine miss re-enumeration does not pay for an apply
    // that writes nothing new. A config reload asks for the same step explicitly, through the
    // lane, so the write and the property snapshot below cannot interleave with another apply.
    let services_changed = {
        let reg = registry().lock().unwrap();
        service_ptrs(&reg) != services_before
    };
    if device_change || services_changed || pending.pointer_reapply {
        if !rebuilt && pending.pointer_reapply {
            reason = "pointer-reapply";
        }
        run_pointer_apply();
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
        "[device] lane batch: reason={reason} rebuilt={rebuilt} device_change={device_change} misses={} retries={} misses_total={} merged={} devices={devices} attribution={attribution_entries} elapsed_ms={} generation={}",
        pending.misses,
        pending.miss_retries,
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
/// Returns whether it rebuilt the client, so the batch log's `rebuilt` field stays honest.
/// `client_created` is the batch's client-creation budget, not just permission: every actual
/// creation is recorded into it immediately, so a batch creates at most one
/// IOHIDEventSystemClient however the reads and retries interleave. When it is already spent
/// (an explicit rebuild or a null client's implicit create), retries re-enumerate on
/// whatever client exists instead of paying another fresh-client settle window.
unsafe fn ensure_enumerated_in_lane(client_created: &mut bool) -> bool {
    let _ = injector_notice_sender();
    // Whether any pass below installed its client -- the first read counts too: a startup
    // enumeration that creates and installs a client is a rebuild for the batch log even
    // when no recovery retry follows.
    let mut rebuilt = false;
    let (needs_recovery, batch_generation) = {
        let mut reg = registry().lock().unwrap();
        // A pass that must create turns this read into a budget consumer -- the same
        // predicate the enumeration itself applies (`enumeration_would_create`): a null
        // client or a distrusted one both mean create, so a spent budget keeps this read
        // from paying a second settle window or publishing an early partial answer.
        // A failed last pass keeps the old list but asks for a full re-read: otherwise a
        // non-empty stale list would pin the snapshot on `Failed` forever.
        if (reg.devices.is_empty() || reg.last_enumeration_failed)
            && (!enumeration_would_create(&reg, false) || !*client_created)
        {
            let (created, installed) = enumerate_locked(&mut reg, false, &[], &[], 0);
            *client_created |= created;
            rebuilt |= installed;
        }
        (
            recovery_is_due(reg.recovery_at, Instant::now(), reg.devices.is_empty()),
            reg.invalidations,
        )
    };
    if !needs_recovery {
        return rebuilt;
    }
    for attempt in 0..2 {
        std::thread::sleep(Duration::from_millis(50));
        let mut reg = registry().lock().unwrap();
        // Reuse the settled client first; only the last retry may rebuild it, since a fresh
        // client re-enters the settle race that produced the empty list. A spent budget stops
        // the retries entirely when the next pass must create -- a null or a distrusted
        // client alike: every retry would be another implicit create.
        if enumeration_would_create(&reg, false) && *client_created {
            break;
        }
        let rebuild = attempt == 1 && !*client_created;
        let (created, installed) = enumerate_locked(&mut reg, rebuild, &[], &[], 0);
        *client_created |= created;
        rebuilt |= installed;
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
    rebuilt
}

/// The IOHID registry id a service produces its events under -- the same id an event carries
/// as its sender ID. The settle poll uses this to tell whether a missed sender has arrived;
/// pointer::apply uses it to tell two identical-model devices apart.
pub(crate) unsafe fn service_registry_id(service: *mut c_void) -> Option<u64> {
    let registry_id = IOHIDServiceClientGetRegistryID(service);
    if registry_id.is_null() {
        return None;
    }
    let id: i64 = msg_send![registry_id as *mut AnyObject, longLongValue];
    Some(id as u64)
}

/// The IOHID registry id a device produces its events under; the attribution map's key.
unsafe fn device_registry_id(device: &Device) -> Option<u64> {
    service_registry_id(device.service_client)
}

/// The service-client pointers backing the current device list, sorted for comparison. The
/// pointer properties live on these objects: a pass that returns a different set of objects
/// (a rebuild, or a re-enumeration that swaps them) is what loses the applied settings.
fn service_ptrs(reg: &DeviceRegistry) -> Vec<usize> {
    let mut ptrs: Vec<usize> = reg
        .devices
        .iter()
        .map(|device| device.service_client as usize)
        .collect();
    ptrs.sort_unstable();
    ptrs
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
        if let Some(id) = device_registry_id(device) {
            attribution.push((id, device.key()));
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

/// The recheck diff's per-device instance tag: the registry id when the SPI answers, else
/// the service object's address with the sign bit set (registry ids never set it, so the two
/// namespaces cannot collide). A reused client hands back the same service objects, so an
/// id-less same-model swap still changes the key instead of comparing equal.
unsafe fn device_instance_tag(service: *mut c_void) -> u64 {
    service_registry_id(service).unwrap_or((service as u64) | (1u64 << 63))
}

/// `CFRunLoopRunInMode`'s result when a source was handled (kCFRunLoopRunHandledSource).
const RUNLOOP_HANDLED_SOURCE: i32 = 4;
/// Upper bound on the zero-timeout drain passes before the recheck diff's `CopyServices`:
/// each pass handles one already-signaled source, so a handful covers any queued backlog
/// while keeping the diff cheap.
const RUNLOOP_DRAIN_PASSES: usize = 8;

/// Cheaply enumerate the current device set with the existing client (no rebuild); used only
/// by the recheck diff. Queued matching events are drained first (a lane that sat idle never
/// pumped them in, and the diff must not judge a stale list as "unchanged"), but no settle
/// wait is done; exhausting the drain budget returns `None` -- "possibly more events queued"
/// -- so the caller treats the diff as inconclusive rather than unchanged. Each key carries
/// the registry id alongside VID/PID: a same-model swap while the lane slept keeps the
/// VID/PID pair but changes the instance id, and a key-level diff must see that. The same
/// keep-filter as the publish path applies (`service_kept`), so a service the device list
/// would drop -- a Bluetooth keyboard faking a Mouse usage -- is invisible to the diff too.
unsafe fn enumerate_keys(
    client: *mut c_void,
    appearance_map: &HashMap<String, u16>,
) -> Option<Vec<(u32, u32, u64)>> {
    let mut keys = Vec::new();
    if client.is_null() {
        return Some(keys);
    }
    // Matching events queued while the lane was idle only reach the client's service list
    // once this runloop runs: drain every already-signaled source with bounded zero-timeout
    // passes before judging the device set, or a fresh arrival reads as "unchanged" and the
    // recheck that would rebuild the list is skipped.
    let mut drained = false;
    for _ in 0..RUNLOOP_DRAIN_PASSES {
        if crate::ffi::CFRunLoopRunInMode(
            crate::event_tap::kCFRunLoopDefaultMode as *const c_void,
            0.0,
            1,
        ) != RUNLOOP_HANDLED_SOURCE
        {
            drained = true;
            break;
        }
    }
    // Every pass handled a source: the backlog may not be drained, so the answer cannot be
    // trusted as "unchanged".
    if !drained {
        return None;
    }
    let services = IOHIDEventSystemClientCopyServices(client);
    if services.is_null() {
        // A failed read is not an empty device set: answering Some([]) would diff "unchanged"
        // against an equally empty published list and skip the full enumeration the
        // still-arriving device needs.
        return None;
    }
    let count = CFArrayGetCount(services);
    for i in 0..count {
        let service = CFArrayGetValueAtIndex(services, i) as *mut c_void;
        if !service_kept(service, appearance_map) {
            continue;
        }
        let vid = prop_int(service, KEY_VENDOR_ID).unwrap_or(0) as u32;
        let pid = prop_int(service, KEY_PRODUCT_ID).unwrap_or(0) as u32;
        keys.push((vid, pid, device_instance_tag(service)));
    }
    CFRelease(services as *const c_void);
    Some(keys)
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
        // A restarted tap brings a NEW RunLoop. The manager lives for the process (releasing it would
        // invalidate its callbacks), so re-schedule it on the new loop: plug events left on the
        // previous, exited thread's run loop are never delivered, and a Bluetooth reconnect would
        // then not re-apply that device's settings.
        IOHIDManagerScheduleWithRunLoop(*m, runloop, crate::event_tap::kCFRunLoopDefaultMode);
        drop(m);
        submit(LaneRequest::EnsureEnumerated);
        return;
    }
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
        // A miss may mean a new device or a Bluetooth reconnect. The lane recovers at most once
        // per `miss_recovery_cooldown`, so a scroll storm that misses every event cannot turn
        // itself into a recovery loop; this event still resolves through the last-active device.
        if !snapshot.devices.is_empty()
            && snapshot.attribution.is_empty()
            && !MISS_MAP_WARNED.swap(true, Ordering::Relaxed)
        {
            log_info!(
                "[device] no registry ids for {} known device(s); attribution falls back to the last-active device.",
                snapshot.devices.len()
            );
        }
        submit(LaneRequest::AttributionMiss { sender });
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

    /// The invalidation wiring: a device change clears both budgets and the empty streak, so the
    /// next query may recover again and the next attribution miss may recover -- that is the
    /// BLE-reconnect promise.
    #[test]
    fn a_device_change_makes_the_next_query_recover_again() {
        let now = Instant::now();
        let mut reg = DeviceRegistry {
            client: std::ptr::null_mut(),
            services: std::ptr::null_mut(),
            devices: Vec::new(),
            recovery_at: Some(now),
            invalidations: 0,
            miss_recovery_at: Some(now),
            miss_unhealed_streak: 5,
            last_enumeration_failed: false,
            confirmed_keys: Vec::new(),
            client_distrusted: false,
        };
        assert!(!recovery_is_due(reg.recovery_at, now, true));
        assert!(!miss_recovery_is_due(
            reg.miss_recovery_at,
            reg.miss_unhealed_streak,
            now
        ));
        invalidate_recovery(&mut reg);
        assert!(reg.recovery_at.is_none());
        assert!(reg.miss_recovery_at.is_none());
        assert_eq!(reg.miss_unhealed_streak, 0);
        assert!(recovery_is_due(reg.recovery_at, now, true));
        assert!(miss_recovery_is_due(
            reg.miss_recovery_at,
            reg.miss_unhealed_streak,
            now
        ));
    }

    /// A miss recovery is admitted once per cooldown, and the cooldown is independent of the
    /// recovery batch's: the miss path must not be able to clear the settings path's cooldown,
    /// which is what made the old code pay the full recovery batch on the main thread every time
    /// it ran.
    #[test]
    fn an_attribution_miss_recovers_once_per_cooldown() {
        let now = Instant::now();
        // First miss after startup: nothing has run yet -> admitted.
        assert!(miss_recovery_is_due(None, 0, now));
        // Every miss inside the window is admitted by the same pending attempt: not one each.
        assert!(!miss_recovery_is_due(
            Some(now - Duration::from_millis(1)),
            0,
            now
        ));
        assert!(!miss_recovery_is_due(
            Some(now - (MISS_RECOVERY_COOLDOWN - Duration::from_millis(1))),
            0,
            now
        ));
        // Past the window a stale client is healed again.
        assert!(miss_recovery_is_due(
            Some(now - MISS_RECOVERY_COOLDOWN),
            0,
            now
        ));
        // The two budgets are separate: a miss attempt does not re-open the recovery batch.
        let recent_attempt = Some(now - Duration::from_millis(100));
        assert!(!recovery_is_due(recent_attempt, now, true));
        assert!(miss_recovery_is_due(
            Some(now - MISS_RECOVERY_COOLDOWN),
            0,
            now
        ));
    }

    /// Consecutive unhealed attempts stretch the cooldown (2s, 4s, 8s, ... capped at 60s) and
    /// arm a bounded rebuild escalation at the threshold -- one rebuild every
    /// MISS_REBUILD_EVERY further attempts, so a stale client gets retries spread by the
    /// still-stretching cooldown instead of a rebuild every attempt.
    #[test]
    fn unhealed_attempts_back_off_and_escalate_periodically() {
        let now = Instant::now();
        let last = Some(now);
        // Streak 0..5: the cooldown doubles each time, capped at the max.
        assert_eq!(miss_recovery_cooldown(0), Duration::from_secs(2));
        assert_eq!(miss_recovery_cooldown(1), Duration::from_secs(4));
        assert_eq!(miss_recovery_cooldown(2), Duration::from_secs(8));
        assert_eq!(miss_recovery_cooldown(5), MISS_RECOVERY_COOLDOWN_MAX);
        assert_eq!(
            miss_recovery_cooldown(10),
            MISS_RECOVERY_COOLDOWN_MAX,
            "the streak cannot push past the cap"
        );
        // An attempt 4s ago is not due while the streak demands 8s, and is due once it wants 4s.
        let four_ago = Some(now - Duration::from_secs(4));
        assert!(!miss_recovery_is_due(four_ago, 2, now));
        assert!(miss_recovery_is_due(four_ago, 1, now));
        // Never-run is always due, however deep the streak.
        assert!(miss_recovery_is_due(None, 7, now));
        // The escalation: armed at the threshold, then every MISS_REBUILD_EVERY-th attempt --
        // bounded retries for a stale client, not a rebuild every attempt.
        assert!(!miss_attempt_wants_rebuild(0));
        assert!(!miss_attempt_wants_rebuild(1));
        assert!(miss_attempt_wants_rebuild(MISS_REBUILD_AFTER_UNHEALED));
        for streak in
            (MISS_REBUILD_AFTER_UNHEALED + 1)..(MISS_REBUILD_AFTER_UNHEALED + MISS_REBUILD_EVERY)
        {
            assert!(!miss_attempt_wants_rebuild(streak));
        }
        assert!(miss_attempt_wants_rebuild(
            MISS_REBUILD_AFTER_UNHEALED + MISS_REBUILD_EVERY
        ));
        assert!(miss_attempt_wants_rebuild(
            MISS_REBUILD_AFTER_UNHEALED + 2 * MISS_REBUILD_EVERY
        ));
        let _ = last;
    }

    /// A rebuild whose fresh client never saw a just-observed device commits the settled
    /// client's answer -- B arriving after the poll window is published by the settled client,
    /// not dropped with the partial new one. The same holds when the candidate reads null:
    /// `required_met` is false for a missing answer too.
    #[test]
    fn unsatisfied_rebuild_falls_back_to_the_settled_client() {
        // Created + required set unmet (partial OR null answer) + settled client -> fall back.
        assert!(should_fallback_to_settled_client(true, true, false, false));
        // A fully-answered rebuild commits its own list -- no regression to fix. Coverage met
        // via a VID/PID match counts as met: after a full matching window the fresh client's
        // answer is the most authoritative single source, and a confirmed device it still
        // cannot see is recovered by the miss-heal path, not by holding the old list.
        assert!(!should_fallback_to_settled_client(true, true, true, false));
        // A reuse read never triggers it: only a fresh client can regress on identities its
        // predecessor already knew.
        assert!(!should_fallback_to_settled_client(
            true, false, false, false
        ));
        // First-ever enumeration has no settled client to fall back to: the partial answer is
        // the best available and commits.
        assert!(!should_fallback_to_settled_client(true, true, false, true));
        // A pass that distrusts the settled cache (the forced recheck) never arbitrates: the
        // possibly-dead list the settled client could hand back is exactly what the pass
        // exists to replace, so a failed create/read keeps the last list and flags failure.
        assert!(!should_fallback_to_settled_client(
            false, true, false, false
        ));
        // When the settled client's arbitration read also fails, the pass must keep the last
        // committed list and flag failure -- a provably-partial answer never publishes as
        // Ready.
        assert_eq!(
            partial_verdict(true),
            PartialVerdict::CommitSettled,
            "a live settled answer wins for a provably-partial candidate"
        );
        assert_eq!(
            partial_verdict(false),
            PartialVerdict::KeepLastList,
            "a failed settled read keeps the last list instead of committing the regression"
        );
    }

    /// The one-create budget and the enumeration itself share one predicate: a pass creates
    /// when a rebuild was planned, when there is no client, or when the settled client is
    /// distrusted -- a co-batched second pass charged under the same rule creates at most
    /// once per batch.
    #[test]
    fn the_create_budget_shares_the_enumerations_own_predicate() {
        let mut reg = DeviceRegistry {
            client: std::ptr::dangling_mut::<c_void>(),
            services: std::ptr::null_mut(),
            devices: Vec::new(),
            recovery_at: None,
            invalidations: 0,
            miss_recovery_at: None,
            miss_unhealed_streak: 0,
            last_enumeration_failed: false,
            confirmed_keys: Vec::new(),
            client_distrusted: false,
        };
        // A settled, trusted client answers by reuse: no create, no budget consumed.
        assert!(!enumeration_would_create(&reg, false));
        // An explicit rebuild plans a create.
        assert!(enumeration_would_create(&reg, true));
        // A null client has nothing to reuse: the read is an implicit create.
        reg.client = std::ptr::null_mut();
        assert!(enumeration_would_create(&reg, false));
        // A distrusted client cannot serve as the source either: the pass creates, and the
        // budget check sees it before the enumeration runs.
        reg.client = std::ptr::dangling_mut::<c_void>();
        reg.client_distrusted = true;
        assert!(enumeration_would_create(&reg, false));
    }

    /// A single input event must still reach the rebuild escalation without further input:
    /// an unhealed attempt re-parks its still-missing senders until one attempt actually
    /// rebuilt the client, whatever the global streak had already accumulated -- a fresh
    /// sender never inherits another sender's exhausted budget.
    #[test]
    fn unhealed_referral_budget_reaches_a_rebuild_per_episode() {
        // An attempt that produced no fresh client's committed list may re-park its senders:
        // create failed, read failed, or no create was due -- the escalation was not
        // exercised yet, and the deferral only carries the wait.
        assert!(may_redefer_unhealed(false, 1));
        // Once an attempt actually installed a fresh client's answer, the escalation is
        // spent: a never-found sender stops waking the lane until it emits again.
        assert!(!may_redefer_unhealed(true, 1));
        // And a request whose retries ran out re-parks no more even before any install --
        // a permanently-failing create must not wake the lane forever.
        assert!(!may_redefer_unhealed(false, 0));
        // The streak escalation itself: armed at MISS_REBUILD_AFTER_UNHEALED and every
        // MISS_REBUILD_EVERY thereafter, never before.
        assert!(miss_attempt_wants_rebuild(MISS_REBUILD_AFTER_UNHEALED));
        assert!(miss_attempt_wants_rebuild(
            MISS_REBUILD_AFTER_UNHEALED + MISS_REBUILD_EVERY
        ));
        assert!(!miss_attempt_wants_rebuild(MISS_REBUILD_AFTER_UNHEALED - 1));
        assert!(!miss_attempt_wants_rebuild(MISS_REBUILD_AFTER_UNHEALED + 1));
    }

    /// Coalesced misses keep the distinct senders, deduplicated and capped: a batch that heals
    /// one sender while another stays missing is still unhealed.
    #[test]
    fn missed_senders_dedupe_and_cap() {
        let mut pending = PendingRequests::default();
        for sender in [7, 7, 9, 11, 13, 17] {
            pending.merge(LaneRequest::AttributionMiss { sender });
        }
        assert_eq!(pending.misses, 6);
        assert_eq!(pending.missed_senders(), &[7, 9, 11, 13]);
        assert!(
            pending.miss_senders_overflow,
            "the fifth distinct sender was dropped; the batch must not read 'all attributed' as healed"
        );
    }

    /// Throttled misses park their senders instead of dropping them: later deferrals dedupe,
    /// the deadline moves to the newest cooldown boundary, and the cap sets overflow like the
    /// batch's own set so a dropped sender is never read as healed.
    #[test]
    fn deferred_misses_dedupe_cap_and_track_the_newest_deadline() {
        let now = Instant::now();
        let mut entry = DeferredMiss {
            senders: [0; 4],
            count: 0,
            overflow: false,
            due_at: now,
            retries_left: 0,
        };
        entry.record(&[7, 9], false, now);
        entry.record(&[9, 11, 13, 17], true, now + Duration::from_secs(4));
        assert_eq!(&entry.senders[..entry.count as usize], &[7, 9, 11, 13]);
        assert!(
            entry.overflow,
            "the fifth distinct sender was dropped; the deferral must not read as healed"
        );
        assert_eq!(entry.due_at, now + Duration::from_secs(4));
    }

    /// A batch that answered some parked senders prunes only those: the still-missing ones
    /// keep their deferred retry, and the entry survives on its overflow flag even when every
    /// recorded sender resolved (the unrecorded ones can never be verified).
    #[test]
    fn deferred_prune_keeps_only_the_still_missing_senders() {
        let now = Instant::now();
        let mut entry = DeferredMiss {
            senders: [0; 4],
            count: 0,
            overflow: false,
            due_at: now,
            retries_left: 0,
        };
        entry.record(&[7, 9, 11], false, now);
        let attributed = |sender: u64| sender == 9;
        assert!(prune_deferred_senders(&mut entry, attributed));
        assert_eq!(&entry.senders[..entry.count as usize], &[7, 11]);
        assert!(!prune_deferred_senders(&mut entry, |_| true));
        // Overflow holds the entry even with every recorded sender attributed.
        entry.record(&[1, 2, 3, 4, 5], false, now);
        assert!(entry.overflow);
        assert!(prune_deferred_senders(&mut entry, |_| true));
        assert_eq!(entry.count, 0);
    }

    /// Requests coalesce, and none of them may be dropped: the lane answers a batch once, and a
    /// reason that disappeared would silently leave a caller (or the picker) stale.
    #[test]
    fn pending_requests_keep_every_reason_and_coalesce_repeats() {
        let mut pending = PendingRequests::default();
        assert!(pending.is_empty());
        pending.merge(LaneRequest::AttributionMiss { sender: 0 });
        pending.merge(LaneRequest::AttributionMiss { sender: 0 });
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
        submit(LaneRequest::AttributionMiss { sender: 0 });
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
        pending.merge(LaneRequest::AttributionMiss { sender: 0 });
        assert!(!pending.only_misses());
        // Misses alone keep the shortcut.
        let mut misses = PendingRequests::default();
        misses.merge(LaneRequest::AttributionMiss { sender: 0 });
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

    /// The regression set a just-created client must cover: equal count with a different
    /// membership is still uncovered.
    #[test]
    fn required_keys_reject_a_same_count_different_set() {
        const RID_A: u64 = 42;
        let idless = |vid: u32, pid: u32, ptr: u64| (vid, pid, ptr | (1 << 63));
        // The settled client confirmed A (rid'd) plus B (no registry id).
        let required = [(1, 2, RID_A), idless(3, 4, 0xAAAA)];
        // Candidate holds A and the same B (under its own tag).
        assert!(keys_cover(
            &required,
            &[(1, 2, RID_A), idless(3, 4, 0xBBBB)]
        ));
        // Candidate holds A and a DIFFERENT model C: same count, wrong set.
        assert!(!keys_cover(
            &required,
            &[(1, 2, RID_A), idless(9, 9, 0xCCCC)]
        ));
        // Candidate holds only A.
        assert!(!keys_cover(&required, &[(1, 2, RID_A)]));
        // Two same-model id-less devices need two services, not one counted twice.
        let required2 = [idless(3, 4, 0xAAAA), idless(3, 4, 0xDDDD)];
        assert!(!keys_cover(&required2, &[idless(3, 4, 0xEEEE)]));
        assert!(keys_cover(
            &required2,
            &[idless(3, 4, 0xEEEE), idless(3, 4, 0xFFFF)]
        ));
        // A rid'd key matches by identity only, never by VID/PID.
        assert!(!keys_cover(&[(1, 2, RID_A)], &[(1, 2, 999)]));
        // Order must not matter: A and B are the same model, A rid'd and B id-less.
        // Processed naively B's VID/PID match would claim A's service and A's exact
        // match would then fail -- rid'd requirements claim first, so the verdict is
        // the same for every ordering of required and available.
        let req_orders = [
            vec![idless(3, 4, 0xAAAA), (3, 4, RID_A)],
            vec![(3, 4, RID_A), idless(3, 4, 0xAAAA)],
        ];
        let avail_orders = [
            vec![(3, 4, RID_A), idless(3, 4, 0xBBBB), (9, 9, 7)],
            vec![idless(3, 4, 0xBBBB), (3, 4, RID_A), (9, 9, 7)],
            vec![(9, 9, 7), (3, 4, RID_A), idless(3, 4, 0xBBBB)],
        ];
        for req in &req_orders {
            for avail in &avail_orders {
                assert!(keys_cover(req, avail), "req={req:?} avail={avail:?}");
            }
        }
        // And when the id-less device's model is genuinely absent, no ordering covers.
        let avail_no_b = [
            vec![(3, 4, RID_A), (9, 9, 7)],
            vec![(9, 9, 7), (3, 4, RID_A)],
        ];
        for req in &req_orders {
            for avail in &avail_no_b {
                assert!(!keys_cover(req, avail), "req={req:?} avail={avail:?}");
            }
        }
    }
}
