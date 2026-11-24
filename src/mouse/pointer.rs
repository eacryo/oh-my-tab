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
use crate::ffi::{make_nsstring, nsstring_to_rust, CFRelease, CFRetain};
use crate::mouse::ffi::*;
use crate::mouse::resolve;
use crate::{log_debug, log_info};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use std::ffi::c_void;
use std::sync::Mutex;

/// The IOFixed scaling factor: property value × 65536 (same as LinearMouse's PointerKit).
const IOFIXED_SCALE: f64 = 65536.0;

/// The default tracking speed: **1.00**, macOS's factory default for the mouse key
/// (HIDMouseAcceleration) -- what the pointer felt like before this setting existed (it is used as
/// the slider's initial value, the double-click reset target, and the last-resort fallback when
/// neither the device nor the system value can be read).
///
/// Distinct from LinearMouse's `fallbackPointerAcceleration = 0.6875`: that is its last-resort
/// constant and equals the *trackpad/pointer* key default (45056); using it as a mouse default is
/// ~31% slower than the factory feel.
pub(crate) const FALLBACK_ACCELERATION: f64 = 1.0;

/// Pointer acceleration / tracking speed -> the raw IOFixed value (rounded). The config layer
/// already validates and clamps; this is a second safety net (same idea as scrolling's
/// line_count clamp).
pub(crate) fn acceleration_to_iofixed(acceleration: f64) -> i64 {
    let clamped = acceleration.clamp(MOUSE_ACCELERATION_MIN, MOUSE_ACCELERATION_MAX);
    (clamped * IOFIXED_SCALE).round() as i64
}

/// The raw IOFixed value -> pointer acceleration / tracking speed.
pub(crate) fn iofixed_to_acceleration(raw: i64) -> f64 {
    raw as f64 / IOFIXED_SCALE
}

/// A property modified on one device (for restore).
struct SavedProp {
    /// IOHIDServiceClientRef (borrowed from the services array; kept alive by PointerState).
    service: *mut c_void,
    /// NSString property key (+1, released on restore).
    key: *mut AnyObject,
    /// Whether the property existed before we wrote it: false = we created the key, so restore
    /// should remove it. Note the original **value** is deliberately not kept: restore writes the
    /// live macOS system value (see `restore`), same as LinearMouse -- a snapshot dies with the
    /// process, the system value is always readable.
    existed_before: bool,
    /// The device this property belongs to: restore gives back values per device, so a device
    /// a later enumeration cannot see is not restored with nothing written back. The registry
    /// id distinguishes identical-model devices that share a VID/PID.
    registry_id: Option<u64>,
    /// The record holds a +1 on `service` itself: a re-pointed (adopted) record then stays
    /// valid inside whatever batch lists it, instead of silently dying when the array that
    /// owned its old handle is released. Records in tests keep this false so fake pointers
    /// never reach CFRetain/CFRelease.
    service_owned: bool,
}

/// One enumeration generation's unrestored records plus the services array their service
/// objects live in (kept alive by the retained array). A batch dies when its last record is
/// restored, so a pass that wrote everything releases its array instead of accumulating it.
struct SavedBatch {
    /// CFArrayRef of IOHIDServiceClient (+1)
    services: *mut c_void,
    props: Vec<SavedProp>,
}

/// Applied pointer state: holds the services array the saved properties were written against
/// (keeping the service clients alive), plus earlier generations' unrestored batches.
struct PointerState {
    /// CFArrayRef of IOHIDServiceClient (+1)
    services: *mut c_void,
    saved: Vec<SavedProp>,
    /// Earlier generations whose records this pass could not restore: a device enumeration
    /// could not see keeps its applied values and its restore record, carried forward until a
    /// later pass (or `restore`) can write the system values back.
    prior: Vec<SavedBatch>,
}

// Raw pointers' Send/Sync (same pattern as SettingsUi/ObjPtr: Mutex guards all access).
unsafe impl Send for PointerState {}
unsafe impl Sync for PointerState {}

static POINTER_STATE: Mutex<Option<PointerState>> = Mutex::new(None);

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

/// Set an integer property.
unsafe fn set_prop_int(service: *mut c_void, key: &str, value: i64) -> bool {
    let k = make_nsstring(key);
    let n = nsnumber(value);
    let ok = IOHIDServiceClientSetProperty(service, k as *const c_void, n as *mut c_void);
    CFRelease(k as *const c_void);
    ok
}

/// An NSNumber (long long). CFNumber and NSNumber are toll-free bridged; used for both
/// IOHIDServiceClientSetProperty and the system-parameter writes.
unsafe fn nsnumber(value: i64) -> *mut AnyObject {
    msg_send![class!(NSNumber), numberWithLongLong: value]
}

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
    // CFNumber/CFBoolean both belong to the NSNumber family; longLongValue works for both.
    let v: i64 = msg_send![out as *mut AnyObject, longLongValue];
    CFRelease(out);
    Some(v)
}

/// The system-level acceleration (raw IOFixed): read the system value for the device's declared
/// key (LinearMouse reads the system value of the same key the device uses), fall back to the
/// mouse key, then to macOS's default 0.6875 (LinearMouse's fallback).
unsafe fn system_acceleration_raw(accel_key: &str) -> i64 {
    system_hid_param(accel_key)
        .or_else(|| system_hid_param(KEY_MOUSE_ACCEL))
        .unwrap_or_else(|| acceleration_to_iofixed(FALLBACK_ACCELERATION))
}

/// The system-level linear-scaling switch (0/1); falls back to 0 (acceleration on) when unreadable.
unsafe fn system_linear_flag() -> i64 {
    if system_hid_param(KEY_LINEAR_SCALING).unwrap_or(0) != 0 {
        1
    } else {
        0
    }
}

/// The desired property values for one device (pure, for unit tests; raw IOFixed/integers).
#[derive(Debug, PartialEq)]
struct DesiredPointerValues {
    /// Target value for HIDUseLinearScalingMouseAcceleration.
    linear: i64,
    /// Target value for the device's declared acceleration key.
    accel: i64,
    /// A configured tracking speed was ignored because linear mode is off (for logging only).
    accel_ignored: bool,
}

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

/// One write candidate's identity, bundled so `write_and_record` stays under the arity lint.
struct WriteTarget<'a> {
    service: *mut c_void,
    name: &'a str,
}

/// Write one property and record it (for restore). `saved` is `None` when the pass may not
/// take on more restore debt (an unidentifiable device while the prior-batch cap is full):
/// a write with no record behind it could never be undone, so the device is left alone.
/// `existed_before` is the caller's merged knowledge (rescued survivors first, then a fresh
/// `prop_exists`), not a bare read: the property existing because WE put it there must not be
/// recorded as "existed before". Returns whether the write succeeded.
unsafe fn write_and_record(
    target: &WriteTarget,
    key: &str,
    value: i64,
    label: &str,
    existed_before: bool,
    saved: Option<&mut Vec<SavedProp>>,
) -> bool {
    let Some(saved) = saved else {
        return false;
    };
    let ok = set_prop_int(target.service, key, value);
    log_debug!(
        "[pointer] {}: {} via {} (ok={})",
        target.name,
        label,
        key,
        ok
    );
    // Record only a write that took: a failed write left the device untouched, so nothing
    // needs restoring -- and a recorded failure would later claim `existed_before` knowledge
    // it never applied against.
    if !ok {
        return false;
    }
    // The record retains its own handle (+1): a record's service stays valid no matter which
    // array/batch lists it -- adoption can re-point in place and the record never outlives the
    // object it writes to.
    CFRetain(target.service as *const c_void);
    saved.push(SavedProp {
        service: target.service,
        key: make_nsstring(key),
        existed_before,
        registry_id: crate::mouse::device::service_registry_id(target.service),
        service_owned: true,
    });
    true
}

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

/// Resolve a device's acceleration property key; see `choose_accel_key`.
unsafe fn accel_property_key(service: *mut c_void) -> String {
    choose_accel_key(
        copy_prop_string(service, KEY_ACCEL_TYPE).as_deref(),
        prop_exists(service, KEY_POINTER_ACCEL),
    )
}

/// Whether a saved record verifiably names THIS service's device: the same service object is
/// the same device (a reused client returns the same objects), and registry ids decide when
/// both exist. VID/PID alone never qualifies: two same-model units are indistinguishable on
/// it, and restoring one unit's records because the OTHER enumerated would strip a device
/// this pass is not rewriting. Only a verified device is "identified": a speculative match
/// cannot prove the enumeration covers its device, so its fresh records stay capacity-gated
/// like an unknown one's.
fn record_verified(sp: &SavedProp, rid: Option<u64>, service: *mut c_void) -> bool {
    sp.service == service || (sp.registry_id.is_some() && sp.registry_id == rid)
}

/// Drop a rescued record for a (service, key) pair this pass just recorded freshly: the
/// fresh record's `existed_before` already merged the survivor's claim (see the `existed_of`
/// lookup in `disable`), and its pointer is owned by the confirmed array, so folding it
/// forward is safe and keeps the debt from duplicating.
unsafe fn consume_superseded(rescued: &mut Vec<SavedProp>, service: *mut c_void, key: &str) {
    for sp in take_props_for(rescued, |sp| {
        sp.service == service && nsstring_to_rust(sp.key) == key
    }) {
        release_record(sp);
    }
}

/// How many unrestored generations a state may carry before the oldest batch is let go.
/// Object-identity matching means a batch normally only survives until its device is next
/// enumerated; growth past the cap needs a permanently ambiguous device set, and the cap
/// bounds that retention at the documented cost of dropping the oldest restore records.
const PRIOR_BATCH_CAP: usize = 16;

/// Release a record's owned objects: its +1 key, and the service handle it retains when
/// `service_owned` (records created by `write_and_record` always do).
unsafe fn release_record(sp: SavedProp) {
    CFRelease(sp.key as *const c_void);
    if sp.service_owned {
        CFRelease(sp.service as *const c_void);
    }
}

/// Release a batch's owned objects: each record's +1 key (and self-owned service handle),
/// then the services array its service pointers live in.
unsafe fn release_batch(batch: SavedBatch) {
    for sp in batch.props {
        release_record(sp);
    }
    if !batch.services.is_null() {
        CFRelease(batch.services as *const c_void);
    }
}

/// Take the records matching `pred` out of `props`; returns them and leaves the rest.
fn take_props_for(props: &mut Vec<SavedProp>, pred: impl Fn(&SavedProp) -> bool) -> Vec<SavedProp> {
    let (for_this, rest): (Vec<SavedProp>, Vec<SavedProp>) =
        std::mem::take(props).into_iter().partition(|sp| pred(sp));
    *props = rest;
    for_this
}

/// Re-point a record at `live`: the record's own +1 transfers across (retain the new handle,
/// release the stale one), so the record stays valid in whatever batch lists it -- no move
/// into another array's batch, no batch split. Unowned records (tests) skip the CF dance and
/// just take the new pointer.
unsafe fn retarget(sp: &mut SavedProp, live: *mut c_void) {
    if sp.service == live {
        return;
    }
    if sp.service_owned {
        CFRetain(live as *const c_void);
        CFRelease(sp.service as *const c_void);
    }
    sp.service = live;
}

/// Take every record verified to belong to one of the `(registry id, live handle)` targets
/// out of `props`, re-pointing each at its verified handle (see `retarget`). The record's own
/// +1 keeps it valid wherever it lands, so the caller may drop it into any generation.
unsafe fn adopt_records(
    props: &mut Vec<SavedProp>,
    verified: &[(Option<u64>, *mut c_void)],
) -> Vec<SavedProp> {
    let mut adopted = Vec::new();
    for mut sp in take_props_for(props, |sp| {
        verified.iter().any(|&(r, t)| record_verified(sp, r, t))
    }) {
        let (_, live) = verified
            .iter()
            .find(|&&(r, t)| record_verified(&sp, r, t))
            .copied()
            .expect("predicate above already verified a target");
        retarget(&mut sp, live);
        adopted.push(sp);
    }
    adopted
}

/// Whether this pass must commit a fresh state at all: any record bound to the new backing
/// array -- written or rescued -- requires the commit so the record stays owned by the array
/// that keeps its handle alive. Only an entirely untouched pass may hand the old state back.
fn pass_commits(saved: &[SavedProp], rescued: &[SavedProp]) -> bool {
    !saved.is_empty() || !rescued.is_empty()
}

/// How many carried-batch slots remain AFTER this pass's merges: a batch survives only while
/// it still holds a record no target can claim, since verified records all merge into this
/// generation (restored, folded, or rescued). Computed once for the pass so a device's
/// admission cannot depend on the order its siblings happen to be visited.
fn projected_batch_slots(old: &PointerState, verified: &[(Option<u64>, *mut c_void)]) -> usize {
    let claimable = |sp: &SavedProp| verified.iter().any(|&(r, s)| record_verified(sp, r, s));
    let occupied = [&old.saved]
        .into_iter()
        .chain(old.prior.iter().map(|batch| &batch.props))
        .filter(|props| props.iter().any(|sp| !claimable(sp)))
        .count();
    PRIOR_BATCH_CAP.saturating_sub(occupied)
}

/// Apply the configured pointer properties to the device lane's CONFIRMED device set:
/// `targets` is the published device list's service clients (already usage- and
/// Bluetooth-keyboard-filtered -- a keyboard faking a Mouse usage never reaches a write),
/// and `backing` is the services array owning those objects (+1, ownership transferred).
/// Reusing the lane's enumeration rather than a second client of our own is what keeps the
/// write set identical to the device set the lane just published -- a fresh client's
/// unmatched window is exactly where a just-reconnected device used to be missed. A null
/// backing means the lane has no confirmed set yet: nothing is written and the applied
/// state is kept for a later pass.
unsafe fn disable(targets: &[*mut c_void], backing: *mut c_void) {
    let mut guard = POINTER_STATE.lock().unwrap();
    if backing.is_null() {
        log_debug!("[pointer] no confirmed services to apply to; state kept");
        return;
    }

    // The applied state is taken now but given back per device, just before that device's
    // first write: a service this enumeration cannot see keeps its applied values instead of
    // being restored with nothing written back, and an enumeration yielding nothing writable
    // restores nothing at all.
    let mut old_state = guard.take();

    let mut saved: Vec<SavedProp> = Vec::new();
    // Debt this pass re-homed onto `backing`: adopted cross-object records plus same-object
    // restore survivors. Every entry's service is one of `targets`, so the +1 on `backing`
    // keeps them valid even when their original batch's array is released.
    let mut rescued: Vec<SavedProp> = Vec::new();
    // Pointer devices enumerated: distinguishes "no device found at all" from "devices present
    // but none needs changing".
    let pointer_devices = targets.len();

    // The confirmed devices' registry ids, read once for adoption and the capacity verdict.
    let verified: Vec<(Option<u64>, *mut c_void)> = if old_state.is_some() {
        targets
            .iter()
            .map(|&s| (crate::mouse::device::service_registry_id(s), s))
            .collect()
    } else {
        Vec::new()
    };

    // The capacity verdict is computed ONCE for the whole pass, so admission cannot depend
    // on the order devices happen to be visited: a carried batch survives only while it
    // still holds a record no confirmed device can claim -- verified records all merge into
    // this generation (restored, folded, or rescued onto `backing`), emptying their batch.
    let batch_slots_left = old_state
        .as_ref()
        .map(|old| projected_batch_slots(old, &verified))
        .unwrap_or(PRIOR_BATCH_CAP);

    for (i, &service) in targets.iter().enumerate() {
        let name = device_name(service);

        // Read VID/PID and resolve whether this device needs any pointer property changed
        // (per-device config).
        let vid = prop_int(service, KEY_VENDOR_ID) as u32;
        let pid = prop_int(service, KEY_PRODUCT_ID) as u32;
        let rid = verified.get(i).and_then(|&(r, _)| r);

        // Give back THIS device's saved system values, and only this device's: a device the
        // enumeration did not return keeps its applied state untouched. Only verified
        // identity may claim records (see `record_verified`); records verified on a
        // different service object are adopted by re-pointing them at the live handle --
        // the matching registry id proved the handles name the same HID service, so the
        // debt merges into this generation instead of staying unmergeable in its old batch
        // or being restored through a stale handle.
        let mut for_this: Vec<SavedProp> = Vec::new();
        if let Some(old) = old_state.as_mut() {
            let claim = [(rid, service)];
            for_this.extend(adopt_records(&mut old.saved, &claim));
            for batch in &mut old.prior {
                for_this.extend(adopt_records(&mut batch.props, &claim));
            }
        }
        let identified = !for_this.is_empty();
        // Restores run for every claimed record: `identified` always qualifies below, so the
        // device is always rewritten afterwards. Survivors go to `rescued` -- their service
        // pointer is the live handle now, so `backing` keeps them valid and they merge into
        // this generation instead of pinning their old batch.
        rescued.extend(restore_entries(for_this));
        // An identified device may always rewrite (its records all sit on the live handle
        // and fold into this generation); an unknown one takes a slot only if the pass's
        // post-merge projection still has one -- skipping it keeps its configuration.
        let record_ok = identified || batch_slots_left > 0;
        // Merged "did the property exist before us" knowledge: a surviving record that says we
        // created the key beats a fresh `prop_exists`, which can only see that the key is
        // there -- possibly because our own earlier write put it there.
        let existed_of = |rescued: &[SavedProp], key: &str| {
            rescued
                .iter()
                .find(|sp| sp.service == service && nsstring_to_rust(sp.key) == key)
                .map(|sp| sp.existed_before)
                .unwrap_or_else(|| prop_exists(service, key))
        };
        let target = WriteTarget {
            service,
            name: &name,
        };
        let sink_allowed = record_ok;
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

        // macOS 14+ (Sonoma) has the linear-scaling switch; older systems only expose the
        // acceleration property (the -1 fallback below).
        if prop_exists(service, KEY_LINEAR_SCALING) {
            // Linear switch: 1 when disabling is requested, otherwise the system value (usually 0),
            // which clears a 1 left behind by a previous run/version (same as LinearMouse's
            // disablePointerAcceleration = false).
            if write_and_record(
                &target,
                KEY_LINEAR_SCALING,
                desired.linear,
                &format!("linear scaling -> {}", desired.linear),
                existed_of(&rescued, KEY_LINEAR_SCALING),
                sink_allowed.then_some(&mut saved),
            ) {
                consume_superseded(&mut rescued, service, KEY_LINEAR_SCALING);
            }
            // Acceleration / tracking speed: in linear mode the configured value (unconfigured ->
            // the system value); with linear mode off, the system value.
            if resolved.disable_acceleration && resolved.acceleration == Some(0.0) {
                // 0 is the bottom of the platform's [0,40] range (slowest), not the -1 "disabled"
                // sentinel; it does take effect, so the log spells the meaning out for
                // "pointer barely moves" reports.
                log_info!(
                    "[pointer] {}: tracking speed 0 = slowest setting in linear mode",
                    name
                );
            }
            // Human-readable value: the configured tracking speed in linear mode, else "system
            // value" (the tracking speed has no effect).
            let accel_desc = match (resolved.disable_acceleration, resolved.acceleration) {
                (true, Some(v)) => format!("tracking speed {v}"),
                _ => "acceleration -> system value".to_string(),
            };
            if write_and_record(
                &target,
                &accel_key,
                desired.accel,
                &format!("{accel_desc} (raw {})", desired.accel),
                existed_of(&rescued, &accel_key),
                sink_allowed.then_some(&mut saved),
            ) {
                consume_superseded(&mut rescued, service, &accel_key);
            }
        } else {
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
            if write_and_record(
                &target,
                &accel_key,
                value,
                &label,
                existed_of(&rescued, &accel_key),
                sink_allowed.then_some(&mut saved),
            ) {
                consume_superseded(&mut rescued, service, &accel_key);
            }
            if disabling && resolved.acceleration.is_some() {
                log_debug!(
                    "[pointer] {}: tracking speed ignored (no linear-scaling property on this system)",
                    name
                );
            }
        }
    }

    if !pass_commits(&saved, &rescued) {
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
        CFRelease(backing as *const c_void);
        // Nothing was written and no debt moved onto this array, so nothing about the applied
        // state changed: every saved entry (and its restore path on disable/quit) is put
        // back as it was.
        *guard = old_state;
        return;
    }

    log_debug!(
        "[pointer] applied pointer settings to {} device(s).",
        saved.len()
    );
    // Rescued debt (adopted records and restore survivors) belongs to this generation too:
    // its service pointers are the confirmed devices' own handles, which `backing` owns.
    saved.extend(rescued);
    // Records this pass could not restore stay recorded -- their applied values remain on the
    // device -- carried forward grouped with the services array that owns their service
    // objects. A batch whose last record just got restored releases its array here, so a
    // fully-written pass never retains anything.
    let mut prior: Vec<SavedBatch> = Vec::new();
    if let Some(old) = old_state {
        for batch in old.prior {
            if batch.props.is_empty() {
                unsafe { release_batch(batch) };
            } else {
                prior.push(batch);
            }
        }
        if old.saved.is_empty() {
            if !old.services.is_null() {
                unsafe { CFRelease(old.services as *const c_void) };
            }
        } else {
            prior.push(SavedBatch {
                services: old.services,
                props: old.saved,
            });
        }
    }
    // Ambiguous records (a same-model pair no enumeration can tell apart) could otherwise
    // keep one batch alive per apply; the cap bounds retention at the documented cost of
    // dropping the oldest unrestored records.
    while prior.len() > PRIOR_BATCH_CAP {
        unsafe { release_batch(prior.remove(0)) };
    }
    *guard = Some(PointerState {
        services: backing,
        saved,
        prior,
    });
}

/// Restore the system pointer settings (called on config disable, reload, or quit): write the
/// **live macOS system values** back instead of an in-process snapshot -- same as LinearMouse's
/// `restorePointerAcceleration()`: the system value is always readable while a snapshot dies with
/// the process (crashes included), and writing it back also clears stale values on the device.
/// Keys we created are removed (back to "never existed").
/// Write the live system values back for a set of saved properties (see `restore`): for keys
/// the device already had, the live system value; for keys we created, removal.
/// Returns the records whose write did not take: the device still carries our applied value,
/// so the record keeps both the +1 key and the `existed_before` knowledge for the next pass
/// (or for `restore`).
unsafe fn restore_entries(saved: Vec<SavedProp>) -> Vec<SavedProp> {
    let mut unwritten = Vec::new();
    for sp in saved {
        let key_cf = sp.key as *const c_void;
        let key = nsstring_to_rust(sp.key);
        let ok = if sp.existed_before {
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
            ok
        } else {
            // We created this key -> remove the property instead of writing 0. Writing 0
            // fabricates a "slowest/no-acceleration" state out of nothing: the old version did
            // exactly that for HIDPointerAcceleration, leaving a never-effective 0 on the
            // device that the settings page then read back as the "live" device value (0.00).
            let ok = IOHIDServiceClientSetProperty(sp.service, key_cf, std::ptr::null_mut());
            log_debug!("[pointer] removed property we created: {} (ok={})", key, ok);
            ok
        };
        if ok {
            release_record(sp);
        } else {
            unwritten.push(sp);
        }
    }
    unwritten
}

fn restore_locked(guard: &mut Option<PointerState>) {
    let Some(state) = guard.take() else {
        return;
    };
    unsafe {
        // A restore that fails keeps its record (and the array owning its service objects) so
        // a later pass can retry -- a runtime disable that dropped them would leave the device
        // at our applied values with nothing left to prove what it owed the restore.
        let unwritten = restore_entries(state.saved);
        let mut prior = Vec::new();
        for batch in state.prior {
            let left = restore_entries(batch.props);
            if left.is_empty() {
                CFRelease(batch.services as *const c_void);
            } else {
                prior.push(SavedBatch {
                    services: batch.services,
                    props: left,
                });
            }
        }
        if unwritten.is_empty() && prior.is_empty() {
            if !state.services.is_null() {
                CFRelease(state.services as *const c_void);
            }
        } else {
            let owed = unwritten.len() + prior.iter().map(|batch| batch.props.len()).sum::<usize>();
            log_info!(
                "[pointer] {} restore write(s) did not take; record(s) kept for retry",
                owed
            );
            // `saved`'s records point into this generation's array; `prior` carries the
            // survivors' own arrays, so a `services` kept only for an empty `saved` is
            // released rather than pinned. The array can be null already (a kept state whose
            // records all lived in `prior`), and CFRelease requires a non-null object.
            if unwritten.is_empty() && !state.services.is_null() {
                CFRelease(state.services as *const c_void);
            }
            *guard = Some(PointerState {
                services: if unwritten.is_empty() {
                    std::ptr::null_mut()
                } else {
                    state.services
                },
                saved: unwritten,
                prior,
            });
        }
    }
    log_debug!("[pointer] restored system acceleration settings.");
}

/// Re-point every record verified to belong to one of `targets` at that live handle, in
/// place: the recorded object may already be dead (the lane replaced its enumeration), but
/// the registry id still proves which live handle is the same HID service -- restoring
/// through the stale object would leave the real device at its applied values. The record
/// stays in its batch: `retarget` moves the record's own +1 onto the live handle, so adoption
/// never splits a batch under repeated disable/enable cycles.
unsafe fn adopt_onto(state: &mut PointerState, verified: &[(Option<u64>, *mut c_void)]) {
    for props in std::iter::once(&mut state.saved)
        .chain(state.prior.iter_mut().map(|batch| &mut batch.props))
    {
        for sp in props.iter_mut() {
            if let Some(&(_, live)) = verified.iter().find(|&&(r, t)| record_verified(sp, r, t)) {
                retarget(sp, live);
            }
        }
    }
}

/// Restore with the lane's confirmed set as rebind targets: rebind verified records onto the
/// live handles first so the restore writes reach objects that still exist, then restore.
/// `backing` is the services array owning `targets` (+1 released here once adoption no longer
/// needs the objects; re-pointed records retain their own handles). Null is fine -- records
/// then keep their recorded handles.
pub(crate) unsafe fn restore_adopted(targets: &[*mut c_void], backing: *mut c_void) {
    {
        let mut guard = POINTER_STATE.lock().unwrap();
        if !backing.is_null() {
            if let Some(state) = guard.as_mut() {
                let verified: Vec<(Option<u64>, *mut c_void)> = targets
                    .iter()
                    .map(|&s| (crate::mouse::device::service_registry_id(s), s))
                    .collect();
                adopt_onto(state, &verified);
            }
            CFRelease(backing as *const c_void);
        }
        restore_locked(&mut guard);
    }
}

/// Apply or restore pointer settings based on the current config. `targets` is the device
/// lane's confirmed device set (the published list's service clients) and `backing` the
/// services array owning them (+1, ownership transferred; null when the lane has no
/// confirmed set). When enabled, `disable` writes to exactly that set -- the same devices
/// the lane just published, not a second enumeration that could see a different one. When
/// disabled the confirmed set is used to rebind stale records to live handles, then
/// `restore` gives the applied state back.
pub(crate) unsafe fn apply(targets: &[*mut c_void], backing: *mut c_void) {
    let enabled = CONFIG.read().map(|c| c.mouse.enabled).unwrap_or(false);
    if enabled {
        disable(targets, backing)
    } else {
        restore_adopted(targets, backing);
    }
}

/// Read a device's current pointer acceleration / tracking speed, so the settings page can show
/// the device's live value when the config has none.
///
/// A negative value (the -1 sentinel written by the legacy fallback path) counts as unreadable
/// and yields None, keeping it out of the slider's 0..=40 range; a missing device or property
/// is None as well.
pub(crate) fn read_acceleration_from(
    snapshot: &crate::mouse::device::DeviceSnapshot,
    device: crate::mouse::device::DeviceKey,
) -> Option<f64> {
    // The key choice must match the write path (`accel_property_key`): otherwise this reads the
    // value left in the wrong key and shows a never-effective 0 to the user as the device's live
    // value (how the settings page ended up displaying 0.00).
    //
    // The values come from the device lane's published snapshot: the settings page must not read
    // IOHID properties itself (that read could wait on the lane's enumeration work). The caller
    // passes the same snapshot it used for the device list, so a fill cannot mix two publishes.
    let properties = snapshot.properties_for(device)?;
    let key = choose_accel_key(
        properties.accel_type.as_deref(),
        properties.has_pointer_acceleration(),
    );
    let raw = properties.accel_value(&key)?;
    if raw < 0 {
        return None;
    }
    Some(iofixed_to_acceleration(raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(rid: Option<u64>, service: *mut c_void) -> SavedProp {
        SavedProp {
            service,
            key: std::ptr::null_mut(),
            existed_before: true,
            registry_id: rid,
            // Fake pointers must never reach CFRetain/CFRelease.
            service_owned: false,
        }
    }

    /// Two same-model mice must never trade restore records: object identity and matching
    /// registry ids are the only cross-enumeration proofs, a one-sided id is not an identity
    /// match at all, and VID/PID alone (the test records all share one) never qualifies.
    #[test]
    fn record_verified_requires_verifiable_identity() {
        let service_a = 0x1000usize as *mut c_void;
        let service_b = 0x2000usize as *mut c_void;
        // The same service object is the same device, whatever the ids say.
        assert!(record_verified(&record(None, service_a), None, service_a));
        assert!(record_verified(
            &record(Some(7), service_a),
            Some(8),
            service_a
        ));
        // Registry ids match when both sides have one -- and only then.
        assert!(record_verified(
            &record(Some(7), service_a),
            Some(7),
            service_b
        ));
        assert!(!record_verified(
            &record(Some(7), service_a),
            Some(8),
            service_b
        ));
        // An id on one side only is not proof: the known id may belong to the other unit.
        for (saved_rid, new_rid) in [(Some(7), None), (None, Some(7))] {
            assert!(!record_verified(
                &record(saved_rid, service_a),
                new_rid,
                service_b
            ));
        }
        // Both ids unknown on different objects: same VID/PID (the helper sets it) is NOT
        // enough -- a same-model unit could claim the other unit's records, so the record
        // stays recorded instead of being restored and consumed.
        assert!(!record_verified(&record(None, service_a), None, service_b));
    }

    /// The commit decision must cover rescued debt, not just fresh writes: a pass whose only
    /// work was adopting records (all restores and config writes failed) still commits the new
    /// backing array so the adopted records never lose their owner.
    #[test]
    fn pass_commits_when_only_debt_was_rescued() {
        let dead = 0x1000usize as *mut c_void;
        let live = 0x2000usize as *mut c_void;
        // No writes, no rescued records: the pass keeps the old state as-is.
        assert!(!pass_commits(&[], &[]));
        // A rescued record alone (restore failed after adoption) forces the commit: restoring
        // the old state and releasing the new array would orphan the re-pointed handle.
        assert!(pass_commits(&[], &[record(Some(7), live)]));
        assert!(pass_commits(
            &[record(Some(7), live)],
            &[record(Some(8), live)]
        ));
        let _ = dead;
    }

    /// Ownership accounting needs REAL CF objects: `retarget` must move the record's own +1
    /// (retain the live handle, release the stale one) and `release_record` must drop it --
    /// exactly once each. Real CFStrings stand in for service clients so the retain/release
    /// path actually runs.
    #[test]
    fn owned_record_moves_its_retain_on_retarget_and_releases_on_drop() {
        extern "C" {
            fn CFGetRetainCount(cf: *const c_void) -> isize;
        }
        unsafe {
            let stale = make_nsstring("stale-service") as *mut c_void;
            let live = make_nsstring("live-service") as *mut c_void;
            // The +1 the record holds on its handle, as `write_and_record` leaves it.
            CFRetain(stale as *const c_void);
            let mut sp = SavedProp {
                service: stale,
                key: make_nsstring("key") as *mut _,
                existed_before: true,
                registry_id: Some(7),
                service_owned: true,
            };
            retarget(&mut sp, live);
            assert_eq!(sp.service, live);
            // The +1 moved: `live` gained it, `stale` lost it.
            assert_eq!(CFGetRetainCount(live as *const c_void), 2);
            assert_eq!(CFGetRetainCount(stale as *const c_void), 1);
            release_record(sp);
            // Dropping the record drops its service +1 too -- exactly once.
            assert_eq!(CFGetRetainCount(live as *const c_void), 1);
            CFRelease(stale as *const c_void);
            CFRelease(live as *const c_void);
        }
    }

    /// Adoption re-points verified records at the live handle that proved them; records with
    /// no verifiable identity stay where they are.
    #[test]
    fn adopt_records_repoints_only_verified_records() {
        let dead = 0x1000usize as *mut c_void;
        let live = 0x2000usize as *mut c_void;
        let mut props = vec![
            record(Some(7), dead), // verified by registry id -> adopted onto `live`
            record(Some(8), dead), // a different id: not this device -> stays
            record(None, dead),    // no id, different object -> stays
            record(None, live),    // already the live object -> adopted (no-op re-point)
        ];
        let adopted = unsafe { adopt_records(&mut props, &[(Some(7), live)]) };
        assert_eq!(adopted.len(), 2);
        assert!(adopted.iter().all(|sp| sp.service == live));
        assert_eq!(props.len(), 2);
        assert!(props.iter().all(|sp| sp.service == dead));
    }

    /// The projected slot count is a fixed point over the whole target set: a batch frees its
    /// slot when every record it holds is claimable, whichever order devices are visited in.
    #[test]
    fn projected_slots_are_visit_order_independent() {
        let stale = 0x1000usize as *mut c_void;
        let state = PointerState {
            services: std::ptr::null_mut(),
            saved: Vec::new(),
            prior: vec![
                SavedBatch {
                    services: std::ptr::null_mut(),
                    props: vec![record(Some(7), stale)],
                },
                SavedBatch {
                    services: std::ptr::null_mut(),
                    props: vec![record(Some(8), stale), record(Some(9), stale)],
                },
            ],
        };
        let a = 0x2000usize as *mut c_void;
        let b = 0x3000usize as *mut c_void;
        // Both orders admit the same verdict: batch 1 is fully claimable but batch 2 holds an
        // unclaimable record, so exactly one slot is still occupied.
        for verified in [
            vec![(Some(7), a), (Some(8), b)],
            vec![(Some(8), b), (Some(7), a)],
        ] {
            assert_eq!(
                projected_batch_slots(&state, &verified),
                PRIOR_BATCH_CAP - 1
            );
        }
        // Every record claimable -> both batches free -> the whole cap is open.
        assert_eq!(
            projected_batch_slots(&state, &[(Some(7), a), (Some(8), b), (Some(9), b)]),
            PRIOR_BATCH_CAP
        );
    }

    #[test]
    fn iofixed_roundtrip() {
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
        // Non-integral multiples round to the nearest IOFixed.
        assert_eq!(acceleration_to_iofixed(1.25), 81920);
        assert_eq!(acceleration_to_iofixed(0.0001), 7);
        // Out-of-range values clamp to 0..=10 (the config layer validates; this is a backstop).
        assert_eq!(acceleration_to_iofixed(-5.0), 0);
        assert_eq!(acceleration_to_iofixed(1000.0), 655_360);
    }

    #[test]
    fn accel_key_prefers_the_device_declaration() {
        // Measured case (MCHOSE G3 V2): the device declares HIDMouseAcceleration while
        // HIDPointerAcceleration also exists (created by the old version) -- the declared key
        // must win or the write is a no-op.
        assert_eq!(
            choose_accel_key(Some(KEY_MOUSE_ACCEL), true),
            KEY_MOUSE_ACCEL.to_string()
        );
        // Same for a trackpad declaring HIDTrackpadAcceleration.
        assert_eq!(
            choose_accel_key(Some("HIDTrackpadAcceleration"), false),
            "HIDTrackpadAcceleration".to_string()
        );
    }

    #[test]
    fn accel_key_falls_back_like_linearmouse() {
        // Nothing declared: HIDPointerAcceleration when present, else HIDMouseAcceleration (the
        // guess order of LinearMouse's PointerDevice.pointerAccelerationType).
        assert_eq!(choose_accel_key(None, true), KEY_POINTER_ACCEL.to_string());
        assert_eq!(choose_accel_key(None, false), KEY_MOUSE_ACCEL.to_string());
        // An empty declaration counts as "not declared".
        assert_eq!(
            choose_accel_key(Some(""), true),
            KEY_POINTER_ACCEL.to_string()
        );
    }

    #[test]
    fn desired_values_disable_path_uses_config_then_system() {
        let sys = acceleration_to_iofixed(0.6875);
        // Disable requested with a configured tracking speed -> linear 1, write the configured value.
        assert_eq!(
            desired_pointer_values(true, Some(3.5), 0, sys),
            DesiredPointerValues {
                linear: 1,
                accel: acceleration_to_iofixed(3.5),
                accel_ignored: false,
            }
        );
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
