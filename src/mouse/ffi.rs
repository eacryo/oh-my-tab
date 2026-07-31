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
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    // IOHIDServiceClient 属性读写(私有 SPI,多年未变,LinearMouse 依赖)。
    // IOHIDServiceClient property read/write (private SPI, stable for years; relied on by LinearMouse).
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

// HID 用途常量 / HID usage constants
/// kHIDPage_GenericDesktop = 0x01
pub(crate) const USAGE_PAGE_GENERIC_DESKTOP: i64 = 1;
/// kHIDUsage_GD_Pointer = 0x01
pub(crate) const USAGE_GD_POINTER: i64 = 1;
/// kHIDUsage_GD_Mouse = 0x02
pub(crate) const USAGE_GD_MOUSE: i64 = 2;
/// kHIDUsage_GD_Trackpad = 0x05
pub(crate) const USAGE_GD_TRACKPAD: i64 = 5;
