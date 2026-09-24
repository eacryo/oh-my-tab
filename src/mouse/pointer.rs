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
}

/// Applied pointer state: holds the event system client + services array (keeping service
/// clients alive), plus the saved-properties list.
struct PointerState {
    /// IOHIDEventSystemClientRef (+1)
    client: *mut c_void,
    /// CFArrayRef of IOHIDServiceClient (+1)
    services: *mut c_void,
    saved: Vec<SavedProp>,
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

/// Disable pointer acceleration: enumerate mouse/trackpad devices, save originals, set the
/// linear-scaling switch.
unsafe fn disable() {
    let mut guard = POINTER_STATE.lock().unwrap();
    if guard.is_some() {
        return; // already applied
    }

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
    // dictionaryWithObject:/arrayWithObject: return autoreleased (+0); must not be released
    // manually - the autorelease pool handles them.
    let dict: *mut AnyObject =
        msg_send![class!(NSDictionary), dictionaryWithObject: page_val, forKey: page_key];
    let arr: *mut AnyObject = msg_send![class!(NSArray), arrayWithObject: dict];
    IOHIDEventSystemClientSetMatchingMultiple(client, arr as *const c_void);
    CFRelease(page_key as *const c_void);
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

    // Iterate services (CFArrayRef) with C functions, not msg_send!(objectAtIndex:),
    // to avoid objc2's type-encoding panic (method returns '@', Rust declares '^v').
    let count = CFArrayGetCount(services);
    let mut saved: Vec<SavedProp> = Vec::new();
    // Pointer devices enumerated: distinguishes "no device found at all" from "devices present
    // but none needs changing".
    let mut pointer_devices = 0usize;

    for i in 0..count {
        let service = CFArrayGetValueAtIndex(services, i) as *mut c_void;
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

        // macOS 14+ (Sonoma) has the linear-scaling switch; older systems only expose the
        // acceleration property (the -1 fallback below).
        if prop_exists(service, KEY_LINEAR_SCALING) {
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
            write_and_record(
                service,
                &accel_key,
                desired.accel,
                &format!("{accel_desc} (raw {})", desired.accel),
                &name,
                &mut saved,
            );
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

/// Apply or restore pointer settings based on the current config. When enabled, disable()
/// resolves per-device whether to disable acceleration; if no connected device asks for it,
/// this is equivalent to restore (no device is touched).
pub(crate) fn apply() {
    let enabled = CONFIG.read().map(|c| c.mouse.enabled).unwrap_or(false);
    if enabled {
        // Restore saved originals first, then disable (so per-device decisions start clean).
        restore();
        unsafe { disable() }
    } else {
        restore();
    }
}

/// Read a device's current pointer acceleration / tracking speed, so the settings page can show
/// the device's live value when the config has none.
///
/// A negative value (the -1 sentinel written by the legacy fallback path) counts as unreadable
/// and yields None, keeping it out of the slider's 0..=40 range; a missing device or property
/// is None as well.
pub(crate) fn read_acceleration(device: crate::mouse::device::DeviceKey) -> Option<f64> {
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
