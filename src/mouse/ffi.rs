//! 鼠标模块的 IOKit 私有 SPI FFI 声明(集中管理,对应 docs/mouse-architecture.md §7)。
//! IOKit private SPI FFI declarations for the mouse module (centralized per §7).

use std::ffi::c_void;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    // IOHIDEventSystemClient:设备枚举入口(LinearMouse PointerDeviceManager 同款)。
    // IOHIDEventSystemClient: device enumeration entry (same as LinearMouse's PointerDeviceManager).
    pub(crate) fn IOHIDEventSystemClientCreate(allocator: *const c_void) -> *mut c_void;
    pub(crate) fn IOHIDEventSystemClientSetMatchingMultiple(
        client: *mut c_void,
        matchings: *const c_void,
    );
    // 返回 CFArrayRef(IOHIDServiceClient 列表),调用方持有 +1。
    // Returns a CFArrayRef of IOHIDServiceClient (+1 owned by caller).
    pub(crate) fn IOHIDEventSystemClientCopyServices(client: *mut c_void) -> *mut c_void;
    // 按 senderID 反查 IOHIDServiceClient(+1 owned,调用方 CFRelease)。
    // 这是归因链的关键:IOHIDEventGetSenderID 的返回值传给此函数,得到的 IOHIDServiceClient
    // 与枚举列表(CopyServices)中的对象是同一实例(指针可比对)。LinearMouse 同款链路。
    // Look up an IOHIDServiceClient by sender ID (+1 owned, caller CFReleases). This is the key
    // link of the attribution chain: the IOHIDEventGetSenderID result feeds this function, and
    // the returned IOHIDServiceClient is the same instance as in the enumerated list (pointer
    // comparison works). Same chain as LinearMouse.
    pub(crate) fn IOHIDEventSystemClientCopyServiceForRegistryID(
        client: *mut c_void,
        registry_id: u64,
    ) -> *mut c_void;
}

// ========== 事件归因:IOHIDEvent sender ID(私有 SPI)/ Event attribution ==========
// CGEventCopyIOHIDEvent 是公开 CoreGraphics 函数(在 event_tap.rs 声明),取出 CGEvent
// 内层 IOHIDEvent。以下两个是私有 SPI,用来从 IOHIDEvent 读 senderID 并映射回设备。
// CGEventCopyIOHIDEvent is a public CoreGraphics function (declared in event_tap.rs) that
// extracts the IOHIDEvent inside a CGEvent. The two below are private SPI for reading the
// sender ID from the IOHIDEvent and mapping it back to a device.
#[link(name = "IOKit", kind = "framework")]
extern "C" {
    /// 读 IOHIDEvent 的 sender ID(= 产生该事件的 IORegistry entry ID)。
    /// Read the sender ID of an IOHIDEvent (= the IORegistry entry ID of the producing device).
    pub(crate) fn IOHIDEventGetSenderID(event: *mut c_void) -> u64;
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    /// IOHIDManager:设备插拔通知(公开 API)。用 IOHIDManager 监听鼠标/触控板接入/移除,
    /// 触发注册表重建——蓝牙设备休眠断连重连等场景下,事件驱动地刷新归因链,避免
    /// 旧 IOHIDEventSystemClient 缓存过期导致归因持续失败。
    ///
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
    // CopyProperty 返回 +1 CFTypeRef,调用方负责 CFRelease。
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
}

// ========== CFArray 遍历 / CFArray iteration ==========
// 遍历 CFArrayRef 用 C 函数而不是 msg_send!(objectAtIndex:):后者会被 objc2 的
// 类型编码校验拦截(返回编码 @ vs Rust 声明 ^v,运行时 panic)。
// Iterating a CFArrayRef uses C functions instead of msg_send!(objectAtIndex:): the latter
// trips objc2's runtime type-encoding validation (method returns '@', Rust declares '^v').
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    /// CFIndex(设备数)。
    /// CFIndex (device count).
    pub(crate) fn CFArrayGetCount(the_array: *const c_void) -> isize;
    /// 按索引取元素(借用,不持有)。
    /// Element by index (borrowed, not owned).
    pub(crate) fn CFArrayGetValueAtIndex(the_array: *const c_void, idx: isize) -> *const c_void;
}

// ========== 属性键常量 / property key constants ==========

/// 私有键:线性缩放开关(macOS 14 Sonoma+)。1 = 线性跟踪(无加速)。
/// Private key: linear-scaling switch (macOS 14 Sonoma+). 1 = linear tracking (no acceleration).
pub(crate) const KEY_LINEAR_SCALING: &str = "HIDUseLinearScalingMouseAcceleration";
/// 每设备指针加速(现代 macOS)。
/// Per-device pointer acceleration (modern macOS).
pub(crate) const KEY_POINTER_ACCEL: &str = "HIDPointerAcceleration";
/// 鼠标加速类型键(旧系统回退)。
/// Mouse acceleration type key (legacy fallback).
pub(crate) const KEY_MOUSE_ACCEL: &str = "HIDMouseAcceleration";
/// 设备主用途页(用于过滤鼠标/触控板)。
/// Device primary usage page (for filtering mice/trackpads).
pub(crate) const KEY_PRIMARY_USAGE_PAGE: &str = "PrimaryUsagePage";
/// 设备主用途。
/// Device primary usage.
pub(crate) const KEY_PRIMARY_USAGE: &str = "PrimaryUsage";
/// 设备产品名(日志用)。
/// Device product name (for logs).
pub(crate) const KEY_PRODUCT: &str = "Product";
/// 设备厂商 ID(USB VID)。
/// Device vendor ID (USB VID).
pub(crate) const KEY_VENDOR_ID: &str = "VendorID";
/// 设备产品 ID(USB PID)。
/// Device product ID (USB PID).
pub(crate) const KEY_PRODUCT_ID: &str = "ProductID";
/// 设备连接方式(USB/Bluetooth/BLE)。
/// Device transport (USB/Bluetooth/BLE).
pub(crate) const KEY_TRANSPORT: &str = "Transport";

// HID 用途常量 / HID usage constants
/// kHIDPage_GenericDesktop = 0x01
pub(crate) const USAGE_PAGE_GENERIC_DESKTOP: i64 = 1;
/// kHIDUsage_GD_Pointer = 0x01
pub(crate) const USAGE_GD_POINTER: i64 = 1;
/// kHIDUsage_GD_Mouse = 0x02
pub(crate) const USAGE_GD_MOUSE: i64 = 2;
/// kHIDUsage_GD_Trackpad = 0x05
pub(crate) const USAGE_GD_TRACKPAD: i64 = 5;
