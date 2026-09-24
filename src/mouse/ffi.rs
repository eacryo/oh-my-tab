//! IOKit private SPI FFI declarations for the mouse module (centralized).

use std::ffi::c_void;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    // IORegistryEntryFromPath returns +1; release with IOObjectRelease.
    // Note io_registry_entry_t / io_object_t are mach_port_t (u32), not pointers: declaring them
    // as pointers reads a 32-bit return as a 64-bit pointer (works in practice, but is UB).
    pub(crate) fn IORegistryEntryFromPath(main_port: u32, path: *const std::ffi::c_char) -> u32;
    // properties comes back +1 (CFMutableDictionaryRef).
    pub(crate) fn IORegistryEntryCreateCFProperties(
        entry: u32,
        properties: *mut *mut c_void,
        allocator: *const c_void,
        options: u32,
    ) -> i32;
    pub(crate) fn IOObjectRelease(object: u32) -> i32;
    // IOHIDEventSystemClient: device enumeration entry (same as LinearMouse's PointerDeviceManager).
    pub(crate) fn IOHIDEventSystemClientCreate(allocator: *const c_void) -> *mut c_void;
    pub(crate) fn IOHIDEventSystemClientSetMatchingMultiple(
        client: *mut c_void,
        matchings: *const c_void,
    );
    // Returns a CFArrayRef of IOHIDServiceClient (+1 owned by caller).
    pub(crate) fn IOHIDEventSystemClientCopyServices(client: *mut c_void) -> *mut c_void;
    // Look up an IOHIDServiceClient by sender ID (+1 owned, caller CFReleases). This is the key
    // link of the attribution chain: the IOHIDEventGetSenderID result feeds this function, and
    // the returned IOHIDServiceClient is the same instance as in the enumerated list (pointer
    // comparison works). Same chain as LinearMouse.
    pub(crate) fn IOHIDEventSystemClientCopyServiceForRegistryID(
        client: *mut c_void,
        registry_id: u64,
    ) -> *mut c_void;
    // Schedule the client on a runloop: an unscheduled client's matching / registry-ID map is
    // incomplete and CopyServiceForRegistryID keeps failing (measured). Scheduling keeps the
    // matching live and makes attribution reliable.
    pub(crate) fn IOHIDEventSystemClientScheduleWithRunLoop(
        client: *mut c_void,
        runloop: crate::event_tap::CFRunLoopRef,
        mode: *const c_void,
    );
}

// HID system parameters: read the macOS **system-level** pointer values (what System Settings
// holds) so device properties can be restored to the system value -- the same chain LinearMouse
// uses in DeviceManager.getSystemProperty.
//
// Note io_service_t / io_connect_t are mach_port_t (u32), not pointers: declared as u32 here.
// (The existing *mut c_void IORegistryEntryFromPath belongs to the device module; this module
// declares its own correctly-typed alias via link_name rather than mixing the two.)

pub(crate) const K_IOHID_PARAM_CONNECT_TYPE: u32 = 1;
pub(crate) const KERN_SUCCESS: i32 = 0;
/// IORegistry path of IOHIDSystem.
pub(crate) const IOSERVICE_IOHID_SYSTEM_PATH: &str = "IOService:/IOResources/IOHIDSystem";

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    pub(crate) fn IOServiceOpen(
        service: u32,
        owning_task: u32,
        connect_type: u32,
        connection: *mut u32,
    ) -> i32;
    pub(crate) fn IOServiceClose(connection: u32) -> i32;
    /// Read a system parameter (CFTypeRef +1). On KERN_SUCCESS with a non-null pointer the
    /// caller owns the reference.
    pub(crate) fn IOHIDCopyCFTypeParameter(
        handle: u32,
        key: *const c_void,
        value: *mut *mut c_void,
    ) -> i32;
    pub(crate) fn mach_task_self() -> u32;
}

// CGEventCopyIOHIDEvent is a public CoreGraphics function (declared in event_tap.rs) that
// extracts the IOHIDEvent inside a CGEvent. The two below are private SPI for reading the
// sender ID from the IOHIDEvent and mapping it back to a device.
#[link(name = "IOKit", kind = "framework")]
extern "C" {
    /// Read the sender ID of an IOHIDEvent (= the IORegistry entry ID of the producing device).
    pub(crate) fn IOHIDEventGetSenderID(event: *mut c_void) -> u64;
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    /// IOHIDManager: device plug/unplug notifications (public API). Used to rebuild the registry
    /// on mouse/trackpad attach/detach -- event-driven refresh of the attribution chain for cases
    /// like Bluetooth disconnect/reconnect, where the stale IOHIDEventSystemClient cache would
    /// otherwise keep attribution failing.
    pub(crate) fn IOHIDManagerCreate(allocator: *const c_void, options: u32) -> *mut c_void;
    pub(crate) fn IOHIDManagerSetDeviceMatchingMultiple(
        manager: *mut c_void,
        multiple: *const c_void,
    );
    // IOHIDDeviceCallback: void (*)(void *context, IOReturn result, void *sender, IOHIDDeviceCallbackRef callback)
    pub(crate) fn IOHIDManagerRegisterDeviceMatchingCallback(
        manager: *mut c_void,
        callback: Option<unsafe extern "C" fn(*mut c_void, i32, *mut c_void, *mut c_void)>,
        context: *mut c_void,
    );
    pub(crate) fn IOHIDManagerRegisterDeviceRemovalCallback(
        manager: *mut c_void,
        callback: Option<unsafe extern "C" fn(*mut c_void, i32, *mut c_void, *mut c_void)>,
        context: *mut c_void,
    );
    pub(crate) fn IOHIDManagerScheduleWithRunLoop(
        manager: *mut c_void,
        runloop: *const c_void,
        mode: *const c_void,
    );
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    // CopyProperty returns +1 CFTypeRef; caller must CFRelease.
    pub(crate) fn IOHIDServiceClientCopyProperty(
        client: *mut c_void,
        key: *const c_void,
    ) -> *mut c_void;
    pub(crate) fn IOHIDServiceClientSetProperty(
        client: *mut c_void,
        key: *const c_void,
        value: *mut c_void,
    ) -> bool;
    // Check whether a HID service conforms to a (usage page, usage) pair (public API from the
    // system SDK). More reliable than reading PrimaryUsage alone: some real mice (e.g. ATK A9 SE
    // Nearlink devices) report PrimaryUsage = Keyboard(6), yet declare Mouse(1,2) in their
    // DeviceUsagePairs, which ConformsTo sees; conversely some keyboards declare extra Mouse
    // usages and get included too (same behavior as LinearMouse; attribution uses exact senderID
    // matching, so this doesn't affect correctness).
    pub(crate) fn IOHIDServiceClientConformsTo(
        client: *mut c_void,
        usage_page: u32,
        usage: u32,
    ) -> i32;
}

// Iterating a CFArrayRef uses C functions instead of msg_send!(objectAtIndex:): the latter
// trips objc2's runtime type-encoding validation (method returns '@', Rust declares '^v').
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    /// CFIndex (device count).
    pub(crate) fn CFArrayGetCount(the_array: *const c_void) -> isize;
    /// Element by index (borrowed, not owned).
    pub(crate) fn CFArrayGetValueAtIndex(the_array: *const c_void, idx: isize) -> *const c_void;
    /// Dictionary lookup (borrowed, not owned).
    pub(crate) fn CFDictionaryGetValue(
        the_dict: *const c_void,
        key: *const c_void,
    ) -> *const c_void;
    /// CFData length and byte pointer (borrowed; valid while the CFData lives).
    pub(crate) fn CFDataGetLength(data: *const c_void) -> isize;
    pub(crate) fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
}

/// Private key: linear-scaling switch (macOS 14 Sonoma+). 1 = linear tracking (no acceleration).
pub(crate) const KEY_LINEAR_SCALING: &str = "HIDUseLinearScalingMouseAcceleration";
/// Per-device pointer acceleration (modern macOS).
pub(crate) const KEY_POINTER_ACCEL: &str = "HIDPointerAcceleration";
/// Mouse acceleration type key (legacy fallback).
pub(crate) const KEY_MOUSE_ACCEL: &str = "HIDMouseAcceleration";
/// The device-declared "which key holds my acceleration" property (a string such as
/// "HIDMouseAcceleration" / "HIDTrackpadAcceleration"). macOS reads/writes that key; writing
/// any other key has no effect (measured on a MCHOSE G3 V2, which declares
/// HIDMouseAcceleration while a HIDPointerAcceleration write did nothing).
pub(crate) const KEY_ACCEL_TYPE: &str = "HIDPointerAccelerationType";
/// Device primary usage page (for matching: filter to the Generic Desktop page, then decide
/// pointer devices via ConformsTo).
pub(crate) const KEY_PRIMARY_USAGE_PAGE: &str = "PrimaryUsagePage";
/// Device product name (for logs).
pub(crate) const KEY_PRODUCT: &str = "Product";
/// Device vendor ID (USB VID).
pub(crate) const KEY_VENDOR_ID: &str = "VendorID";
/// Device product ID (USB PID).
pub(crate) const KEY_PRODUCT_ID: &str = "ProductID";
/// Device transport (USB/Bluetooth/BLE).
pub(crate) const KEY_TRANSPORT: &str = "Transport";
/// BT address property (present only on Bluetooth-transport devices, e.g. "cc-d7-81-0a-f6-62").
pub(crate) const KEY_DEVICE_ADDRESS: &str = "DeviceAddress";
/// bluetoothd's device-cache key written to NVRAM (an IODTNVRAM registry property, private format).
pub(crate) const KEY_BLUETOOTH_INFO: &str = "BluetoothInfo";
/// IORegistry path of the IODTNVRAM node (where BluetoothInfo lives).
pub(crate) const IOSERVICE_OPTIONS_PATH: &str = "IOService:/options";
/// GAP Appearance: keyboard (0x03C1). Mouse is 0x03C2, touchpad is 0x03C9.
pub(crate) const GAP_APPEARANCE_KEYBOARD: u16 = 0x03C1;

/// kHIDPage_GenericDesktop = 0x01
pub(crate) const USAGE_PAGE_GENERIC_DESKTOP: i64 = 1;
/// kHIDUsage_GD_Pointer = 0x01
pub(crate) const USAGE_GD_POINTER: i64 = 1;
/// kHIDUsage_GD_Mouse = 0x02
pub(crate) const USAGE_GD_MOUSE: i64 = 2;
/// kHIDUsage_GD_Trackpad = 0x05
pub(crate) const USAGE_GD_TRACKPAD: i64 = 5;
