//! FFI and ObjC-bridging primitives: CF/CG function declarations, Send/Sync wrappers for raw
//! pointers, NSString conversion, and color/layer helpers. A leaf module depended on by all UI modules.

use crate::log_info;
use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send, sel};
use std::cell::{BorrowMutError, RefCell, RefMut};
use std::ffi::{c_char, c_void, CString};
use std::marker::PhantomData;
use std::rc::Rc;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub(crate) fn CFStringCreateWithCString(
        alloc: *const c_void,
        c_str: *const c_char,
        encoding: u32,
    ) -> *const c_void;
    pub(crate) fn CFRelease(cf: *const c_void);
    pub(crate) fn CFRetain(cf: *const c_void);
    // CFEqual: compares two CF objects for equality. IOHIDServiceClient equality is defined
    // by the system (typically by underlying object identity), not by raw pointer address --
    // the object returned by CopyServiceForRegistryID may not be the same instance as the one
    // enumerated by CopyServices, so CFEqual must be used.
    pub(crate) fn CFEqual(cf1: *const c_void, cf2: *const c_void) -> bool;
    pub(crate) fn CFRunLoopRunInMode(
        mode: *const c_void,
        seconds: f64,
        return_after_source_handled: u8,
    ) -> i32;
    pub(crate) static kCFRunLoopDefaultMode: *mut c_void;

    /// The standard retain/release call-backs a CFArray needs to own CF-typed elements
    /// (e.g. CGColorRefs handed to `CAGradientLayer.colors`).
    pub(crate) static kCFTypeArrayCallBacks: c_void;
    pub(crate) fn CFArrayCreate(
        alloc: *const c_void,
        values: *const *const c_void,
        num_values: isize,
        callbacks: *const c_void,
    ) -> *const c_void;
    pub(crate) fn CFArrayGetCount(array: *const c_void) -> isize;
    pub(crate) fn CFArrayGetValueAtIndex(array: *const c_void, index: isize) -> *const c_void;
    /// CFTypeID queries: a batch-read slot can be an AXValue placeholder, a CFArray or an
    /// AXUIElement, and only after checking the type id is it safe to treat a slot as an
    /// AXUIElement -- the private `_AXUIElementGetWindow` validates nothing.
    pub(crate) fn CFGetTypeID(cf: *const c_void) -> usize;
    pub(crate) fn CFArrayGetTypeID() -> usize;
    pub(crate) fn CFDictionaryGetTypeID() -> usize;
    pub(crate) fn CFDictionaryGetCount(dict: *const c_void) -> isize;
    pub(crate) fn CFNumberGetTypeID() -> usize;
    pub(crate) fn CFStringGetTypeID() -> usize;
    pub(crate) fn AXUIElementGetTypeID() -> usize;
    pub(crate) fn AXValueGetTypeID() -> usize;
    /// The AXValue's concrete type; kAXValueAXErrorType = 5 marks an error placeholder slot.
    pub(crate) fn AXValueGetType(value: *const c_void) -> i32;
    pub(crate) fn CFDictionaryGetValue(dict: *const c_void, key: *const c_void) -> *const c_void;
    pub(crate) fn CFDictionaryGetKeysAndValues(
        dict: *const c_void,
        keys: *mut *const c_void,
        values: *mut *const c_void,
    );
    pub(crate) fn CFNumberCreate(
        alloc: *const c_void,
        number_type: isize,
        value_ptr: *const c_void,
    ) -> *const c_void;
    pub(crate) fn CFNumberGetValue(
        number: *const c_void,
        the_type: isize,
        value: *mut c_void,
    ) -> bool;
    pub(crate) fn CFBooleanGetValue(boolean: *const c_void) -> bool;
    pub(crate) fn CFStringGetCString(
        string: *const c_void,
        buffer: *mut c_char,
        buffer_size: isize,
        encoding: u32,
    ) -> bool;
    pub(crate) fn CFStringGetLength(string: *const c_void) -> isize;
    pub(crate) fn CFStringGetRangeOfComposedCharactersAtIndex(
        string: *const c_void,
        index: isize,
    ) -> CFRange;
    pub(crate) fn CFStringCreateWithSubstring(
        alloc: *const c_void,
        string: *const c_void,
        range: CFRange,
    ) -> *const c_void;
    pub(crate) fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
    /// CFString value comparison: 0 when equal (kCFCompareEqualTo).
    pub(crate) fn CFStringCompare(a: *const c_void, b: *const c_void, options: usize) -> isize;
    pub(crate) fn CFUUIDCreateString(alloc: *const c_void, uuid: *const c_void) -> *const c_void;
    pub(crate) static kCFBooleanFalse: *const c_void;
    pub(crate) static kCFBooleanTrue: *const c_void;

    pub(crate) fn CFRunLoopSourceCreate(
        alloc: *const c_void,
        order: isize,
        ctx: *const CFRunLoopSourceContext,
    ) -> *mut c_void;
    pub(crate) fn CFRunLoopSourceSignal(src: *mut c_void);
    pub(crate) fn CFRunLoopRemoveSource(rl: *mut c_void, src: *mut c_void, mode: *const c_void);
    pub(crate) fn CFRunLoopWakeUp(rl: *mut c_void);
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct CFRange {
    location: isize,
    length: isize,
}

/// CFRunLoopSource context (only the perform field is used).
#[repr(C)]
pub(crate) struct CFRunLoopSourceContext {
    pub(crate) version: i64,
    pub(crate) info: *mut c_void,
    pub(crate) retain: *const c_void,
    pub(crate) release: *const c_void,
    pub(crate) copy_description: *const c_void,
    pub(crate) equal: *const c_void,
    pub(crate) hash: *const c_void,
    pub(crate) schedule: *const c_void,
    pub(crate) cancel: *const c_void,
    pub(crate) perform: Option<unsafe extern "C" fn(*mut c_void)>,
}

/// AX element handle and error codes, shared by every caller.
pub(crate) type AXUIElementRef = *const c_void;
pub(crate) type AxObserverRef = *mut c_void;
pub(crate) type AXError = i32;
pub(crate) const K_AX_SUCCESS: AXError = 0;
pub(crate) const K_AX_INVALID_UI_ELEMENT: AXError = -25205;
/// kAXErrorCannotComplete: the target app did not answer within the messaging timeout. This is
/// the code an unresponsive app returns (measured 2026-09-16: every AX query against PeachPic
/// burned the full timeout and came back -25204).
pub(crate) const K_AX_CANNOT_COMPLETE: AXError = -25204;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    pub(crate) fn AXIsProcessTrusted() -> bool;
    pub(crate) fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    pub(crate) fn AXUIElementGetPid(element: AXUIElementRef, pid: *mut i32) -> i32;
    pub(crate) fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: *const c_void,
        value: *mut *const c_void,
    ) -> AXError;
    pub(crate) fn AXUIElementCopyParameterizedAttributeValue(
        element: AXUIElementRef,
        attribute: *const c_void,
        parameter: *const c_void,
        value: *mut *const c_void,
    ) -> AXError;
    /// Reads several attributes in one IPC round trip. With empty options and no stopOnError the
    /// call ALWAYS returns an array: a slot the app could not answer holds an
    /// `kAXValueAXErrorType` AXValue placeholder, which callers must read as "did not answer"
    /// rather than "answered".
    pub(crate) fn AXUIElementCopyMultipleAttributeValues(
        element: AXUIElementRef,
        attributes: *const c_void,
        options: i32,
        values: *mut *const c_void,
    ) -> AXError;
    pub(crate) fn AXUIElementPerformAction(
        element: AXUIElementRef,
        action: *const c_void,
    ) -> AXError;
    /// Enumerate an element's supported action names (kAXActionNames). Used to probe for
    /// `AXZoomWindow`, the private action AppKit attaches to the zoom button; the public headers
    /// only declare `kAXPressAction`.
    pub(crate) fn AXUIElementCopyActionNames(
        element: AXUIElementRef,
        names: *mut *const c_void,
    ) -> AXError;
    pub(crate) fn AXUIElementSetAttributeValue(
        element: AXUIElementRef,
        attribute: *const c_void,
        value: *const c_void,
    ) -> AXError;
    /// The SDK declares `timeoutInSeconds` as `float`, not `double` (AXUIElement.h:402). Passing
    /// an `f64` put the value in a d-register the callee never read, so every timeout this app set
    /// was effectively undefined. Declared as `f32` to match the ABI.
    pub(crate) fn AXUIElementSetMessagingTimeout(element: AXUIElementRef, timeout: f32) -> AXError;
    // AXValue: wrapper type for geometry values (CGPoint/CGSize); used to read/write
    // AXPosition/AXSize for window control.
    pub(crate) fn AXValueCreate(value_type: i32, value_ptr: *const c_void) -> *const c_void;
    pub(crate) fn AXValueGetValue(
        value: *const c_void,
        value_type: i32,
        value_ptr: *mut c_void,
    ) -> bool;
    pub(crate) fn AXObserverCreate(
        pid: i32,
        callback: unsafe extern "C" fn(AxObserverRef, *const c_void, *const c_void, *mut c_void),
        out: *mut AxObserverRef,
    ) -> i32;
    pub(crate) fn AXObserverAddNotification(
        observer: AxObserverRef,
        element: *const c_void,
        notification: *const c_void,
        refcon: *mut c_void,
    ) -> i32;
    pub(crate) fn AXObserverGetRunLoopSource(observer: AxObserverRef) -> *mut c_void;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGWindowLevelForKey(key: i32) -> i32;
    pub(crate) fn CGWindowListCopyWindowInfo(option: u32, relative_to_window: u32)
        -> *const c_void;
    pub(crate) fn CGGetActiveDisplayList(
        max_displays: u32,
        active_displays: *mut u32,
        display_count: *mut u32,
    ) -> i32;
    pub(crate) fn CGDisplayBounds(display: u32) -> CGRect;
    pub(crate) fn CGMainDisplayID() -> u32;
    pub(crate) fn CGDisplayCreateUUIDFromDisplayID(display: u32) -> *const c_void;

    pub(crate) fn CGPreflightScreenCaptureAccess() -> bool;
    pub(crate) fn CGRequestScreenCaptureAccess() -> bool;
    pub(crate) fn CGImageGetWidth(image: *const c_void) -> usize;
    pub(crate) fn CGImageGetHeight(image: *const c_void) -> usize;
    pub(crate) fn CGColorSpaceCreateDeviceRGB() -> *const c_void;
    pub(crate) fn CGColorGetAlpha(color: *const c_void) -> f64;
    pub(crate) fn CGColorGetComponents(color: *const c_void) -> *const f64;
    pub(crate) fn CGColorGetNumberOfComponents(color: *const c_void) -> usize;
    pub(crate) fn CGColorEqualToColor(color1: *const c_void, color2: *const c_void) -> bool;
    pub(crate) fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: *const c_void,
        bitmap_info: u32,
    ) -> *mut c_void;
    pub(crate) fn CGContextDrawImage(ctx: *mut c_void, rect: CGRect, image: *const c_void);
    pub(crate) fn CGBitmapContextCreateImage(ctx: *mut c_void) -> *const c_void;
    pub(crate) fn CGBitmapContextGetData(ctx: *mut c_void) -> *mut c_void;
    /// A +1 rounded-rect path (pass a null transform for identity). Core Graphics object, not an
    /// Objective-C one: release it with `CFRelease`, never `release_obj`.
    pub(crate) fn CGPathCreateWithRoundedRect(
        rect: CGRect,
        corner_width: f64,
        corner_height: f64,
        transform: *const c_void,
    ) -> *mut c_void;
    /// The path's bounding box, for a smoke runner to check the *geometry* of a `shadowPath` rather than
    /// just its presence. Read-only: it takes a borrowed path.
    pub(crate) fn CGPathGetBoundingBox(path: *const c_void) -> CGRect;
}

/// CoreGraphics CGRect (C ABI: {origin:(x,y), size:(w,h)} -- four contiguous f64;
/// the flat fields are byte-identical to the C layout).
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct CGRect {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) w: f64,
    pub(crate) h: f64,
}

/// Return Apple's overlay window level rather than baking in its current numeric value.
/// `kCGOverlayWindowLevelKey` is the public CoreGraphics level intended for system-style
/// overlays. On the current macOS it is 102, just above Telegram's media viewer (101).
pub(crate) fn cg_overlay_window_level() -> isize {
    // CGWindowLevelKey::kCGOverlayWindowLevelKey has the numeric enum value 15.
    unsafe { CGWindowLevelForKey(15) as isize }
}

/// libc `struct tm` layout (shared by the logger's timestamps and the clipboard's
/// "copied at" formatting; the two previous definitions were field-identical).
#[repr(C)]
pub(crate) struct Tm {
    pub(crate) tm_sec: i32,
    pub(crate) tm_min: i32,
    pub(crate) tm_hour: i32,
    pub(crate) tm_mday: i32,
    pub(crate) tm_mon: i32,
    pub(crate) tm_year: i32,
    pub(crate) tm_wday: i32,
    pub(crate) tm_yday: i32,
    pub(crate) tm_isdst: i32,
    pub(crate) tm_gmtoff: i64,
    pub(crate) tm_zone: *const c_char,
}

extern "C" {
    pub(crate) fn localtime_r(time: *const i64, result: *mut Tm) -> *mut Tm;
}

/// A UNIX timestamp as a local-time filename stamp: `YYYY-MM-DD HH.MM.SS`.
///
/// Local, not UTC: a saved file is something the user looks at next to Finder's own clock, so it
/// has to name the time they made it. The time separators are dots rather than colons because
/// colons are reserved in HFS+/Finder names -- this is macOS's own convention for a saved file.
///
/// It lives beside `Tm`/`localtime_r` so the clipboard's save-as and the settings export share one
/// implementation and the two user-facing save paths cannot drift apart.
pub(crate) fn format_local_timestamp(unix_secs: u64) -> String {
    unsafe {
        let mut tm: Tm = std::mem::zeroed();
        let s = unix_secs as i64;
        localtime_r(&s, &mut tm);
        format!(
            "{:04}-{:02}-{:02} {:02}.{:02}.{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
}

/// The kernel `task_vm_info` C data layout is 372 bytes (93 u32 words). Rust's `repr(C)`
/// adds 4 bytes of trailing 8-byte alignment padding, so the Mach count must be based on
/// the C layout rather than `size_of::<TaskVmInfo>()`. The buffer has extra tail space and
/// cannot be overrun.
const TASK_VM_INFO_DATA_BYTES: usize = 372;
pub(crate) const TASK_VM_INFO_COUNT: u32 =
    (TASK_VM_INFO_DATA_BYTES / std::mem::size_of::<u32>()) as u32;

///   resident_size=16, resident_size_peak=24, internal=48, compressed=120,
/// Offsets verified against mach/task_info.h with a C compiler:
///   resident_size=16, resident_size_peak=24, internal=48, compressed=120,
///   phys_footprint=144 (the first group after the header holds the 32-bit basic_info
///   fields, not 64-bit ones). The layout must match the system header; new fields may
/// only be appended, never reordered.
#[repr(C)]
pub(crate) struct TaskVmInfo {
    // offset 0-15: four u32 header words (the 32-bit basic_info group).
    header: [u32; 4],
    /// Total resident bytes (RSS). Offset 16.
    pub(crate) resident_size: u64,
    /// Peak RSS (kernel-maintained). Offset 24.
    pub(crate) resident_size_peak: u64,
    // offset 32-47: the 32-bit counter group (region_count etc.).
    _counters: [u32; 4],
    /// Anonymous memory (our heap: Rust + malloc zones). Offset 48.
    pub(crate) internal: u64,
    // offset 56-119: the 64-bit group after internal (purgeable/alternate/...).
    _middle: [u64; 8],
    /// Memory absorbed by the compressor. Offset 120.
    pub(crate) compressed: u64,
    // offset 128-143: the 64-bit group after compressed.
    _late: [u64; 2],
    /// Physical footprint = Activity Monitor's "Memory" column (compressed + IOKit included).
    /// Offset 144.
    pub(crate) phys_footprint: u64,
    // offset 152-371: trailing unread fields. Pads the C data layout so task_info
    // cannot write past the buffer.
    _tail: [u8; TASK_VM_INFO_DATA_BYTES - 152],
}

const _: () = assert!(std::mem::size_of::<TaskVmInfo>() >= TASK_VM_INFO_DATA_BYTES);

// [u8; 220] exceeds the array length derive(Default) supports (<=32); hand-written.
impl Default for TaskVmInfo {
    fn default() -> Self {
        Self {
            header: [0; 4],
            resident_size: 0,
            resident_size_peak: 0,
            _counters: [0; 4],
            internal: 0,
            _middle: [0; 8],
            compressed: 0,
            _late: [0; 2],
            phys_footprint: 0,
            _tail: [0; 220],
        }
    }
}

/// Read the current process's task_vm_info. Returns None on failure (only plausible if the
/// kernel interface changes); the caller just skips that sample.
pub(crate) fn task_vm_info() -> Option<TaskVmInfo> {
    let mut info = TaskVmInfo::default();
    let mut count = TASK_VM_INFO_COUNT;
    let kr = task_info(std::ptr::addr_of_mut!(info), &mut count);
    if kr != 0 {
        return None;
    }
    Some(info)
}

#[cfg(test)]
impl TaskVmInfo {
    /// Test-only constructor: padding stays default, only the metrics of interest are set
    /// (private fields can't be constructed from outside the module).
    pub(crate) fn with_footprint(phys_footprint: u64) -> Self {
        Self {
            phys_footprint,
            ..Default::default()
        }
    }
}

fn task_info(info: *mut TaskVmInfo, count: &mut u32) -> i32 {
    extern "C" {
        fn mach_task_self() -> u32;
        fn task_info(
            target: u32,
            flavor: u32,
            info_out: *mut TaskVmInfo,
            info_out_count: *mut u32,
        ) -> i32;
    }
    const TASK_VM_INFO_FLAVOR: u32 = 22;
    unsafe { task_info(mach_task_self(), TASK_VM_INFO_FLAVOR, info, count) }
}

// AppKit framework link placeholder
#[link(name = "AppKit", kind = "framework")]
extern "C" {}

#[link(name = "objc", kind = "dylib")]
extern "C" {
    pub(crate) fn objc_allocateClassPair(
        superclass: *mut AnyObject,
        name: *const c_char,
        extra_bytes: usize,
    ) -> *mut AnyObject;
    pub(crate) fn objc_registerClassPair(cls: *mut AnyObject);
    pub(crate) fn class_addMethod(
        cls: *mut AnyObject,
        name: Sel,
        imp: *mut c_void,
        types: *const c_char,
    ) -> bool;
    // Defining an ivar goes through `ClassBuilder::add_ivar` (it must run before any
    // `add_method`, and it derives size/alignment/encoding from the Rust type instead of the
    // three hand-written parameters this declaration used to take). Reading one back uses
    // objc2::ffi directly at the one place that needs the offset.
    pub(crate) fn objc_getClass(name: *const c_char) -> *mut AnyObject;
    // objc2's msg_send! cannot express every signature; callers transmute the untyped symbol
    // into a concrete function pointer. This is the single declaration site -- do not inline
    // new externs at call sites.
    pub(crate) fn objc_msgSend();
    pub(crate) fn objc_msgSendSuper();
}

/// The objc_msgSendSuper receiver struct (objc_super; two pointers in the C ABI).
#[repr(C)]
pub(crate) struct ObjcSuper {
    pub(crate) receiver: *mut c_void,
    super_class: *mut c_void,
}

impl ObjcSuper {
    /// Build the receiver for an override defined in `defined`, deriving the superclass from it.
    ///
    /// `defined` must be the class that *defines* the override, never its superclass. The
    /// superclass is resolved here rather than passed in because `objc_super.super_class` is
    /// the class the search *starts* at: handing it `defined` itself would re-enter the
    /// override until the stack overflows, and handing it an unrelated class silently skips
    /// the implementation that should run.
    ///
    /// What this buys is that a call site no longer writes `super_class` at all, so those two
    /// mistakes cannot be made *there*. It does not verify that `defined` is the defining class:
    /// any non-null class passes, and passing the superclass by mistake still starts the search
    /// one level too high. Getting that argument right is the call site's job, which is why every
    /// converted entry is named in the gate in this module's tests.
    pub(crate) fn for_defined_class(receiver: *mut c_void, defined: StaticClass) -> Self {
        let defined_ptr = defined.0;
        debug_assert!(
            !defined_ptr.is_null(),
            "ObjcSuper::for_defined_class needs a registered class"
        );
        let superclass = unsafe { objc2::ffi::class_getSuperclass(defined_ptr) };
        debug_assert!(
            !superclass.is_null(),
            "the defined class has no superclass; it cannot dispatch super"
        );
        Self {
            receiver,
            super_class: superclass as *mut c_void,
        }
    }
}

/// `Rc` marker deliberately makes this type neither `Send` nor `Sync`; putting it in a
/// [`MainThreadSlot`] keeps the ownership boundary explicit without claiming that an
/// arbitrary Objective-C object is thread-safe.
///
/// Main-thread-only Objective-C object pointer. The `Rc` marker deliberately makes this type
/// neither `Send` nor `Sync`; storing it in [`MainThreadSlot`] keeps the ownership boundary
/// explicit without claiming that an arbitrary Objective-C object is thread-safe.
#[derive(Clone, Copy)]
pub(crate) struct ObjPtr(pub(crate) *mut AnyObject, PhantomData<Rc<()>>);

impl ObjPtr {
    pub(crate) const fn new(ptr: *mut AnyObject) -> Self {
        Self(ptr, PhantomData)
    }
}

/// A process-lifetime Objective-C class pointer. Dynamic classes are registered once and are
/// retained by the Objective-C runtime for the life of the process, so the class identity itself
/// is safe to share across threads; instances created from it remain main-thread objects.
#[derive(Clone, Copy)]
#[repr(transparent)]
///
/// The tuple field is reachable inside the crate on purpose (several modules convert a class
/// pointer they already hold), so this type does not make an arbitrary pointer a valid class: it
/// only gives the conversion one name, which is what `ObjcSuper` needs to take a class rather than
/// a bare pointer.
pub(crate) struct StaticClass(pub(crate) *const objc2::runtime::AnyClass);
unsafe impl Send for StaticClass {}
unsafe impl Sync for StaticClass {}

impl StaticClass {
    /// The handle of the class that defines the override being overridden.
    ///
    /// Used by call sites whose class was registered somewhere else (a module that only owns the
    /// callback).
    ///
    /// This narrows the search but does not prove the argument: a class pointer that is not null
    /// goes through, so the caller still has to pass the class that *defines* the override — the
    /// superclass and an instance both pass this check. What it removes is the shape where a call
    /// site writes `super_class` directly, and `ObjcSuper::for_defined_class` deriving from the
    /// result is where a wrong-but-non-null class would still do damage. Reviewing the argument at
    /// each call site is still the guarantee; the type only makes the mistake visible.
    pub(crate) fn defining(class_ptr: *mut AnyObject) -> Self {
        debug_assert!(
            !class_ptr.is_null(),
            "StaticClass::defining needs a class, not a null pointer"
        );
        Self(class_ptr as *const objc2::runtime::AnyClass)
    }

    /// Build the class for `name` with `build`, once per process.
    ///
    /// Dynamic classes are registered while the app starts, so registering the same name twice
    /// (a second call through a path that was supposed to be one-shot) used to mean
    /// `objc_allocateClassPair` returning nil and every following `class_addMethod` writing
    /// through it. Taking the `OnceLock` here makes "registered once" a property of the helper
    /// instead of a comment at each call site.
    pub(crate) fn register_once(
        slot: &'static std::sync::OnceLock<StaticClass>,
        _name: &'static std::ffi::CStr,
        build: impl FnOnce() -> StaticClass,
    ) -> StaticClass {
        *slot.get_or_init(build)
    }
}

/// Build and register a dynamic class, naming it in any failure.
///
/// `ClassBuilder` derives each method's type encoding from the callback's Rust signature and
/// compares it with the superclass implementation, so a callback whose signature disagrees with
/// the selector it is registered under fails at registration rather than misbehaving later. This
/// wrapper adds what objc2 cannot: the class name (its messages print the class only once one
/// exists, which on the allocation failure path it does not) and the main-thread assertion.
///
/// A nil allocation means a class of that name is already registered. That is a bug in this app
/// rather than a condition to recover from, so it panics: a class that silently failed to build
/// would leave a settings page or a panel missing instead of failing where the mistake is.
pub(crate) fn build_class(
    name: &'static std::ffi::CStr,
    superclass: &'static objc2::runtime::AnyClass,
    build: impl FnOnce(&mut objc2::runtime::ClassBuilder),
) -> StaticClass {
    let mut builder = objc2::runtime::ClassBuilder::new(name, superclass).unwrap_or_else(|| {
        panic!(
            "dynamic class {} could not be allocated; is it registered twice?",
            name.to_string_lossy()
        )
    });
    build(&mut builder);
    StaticClass(builder.register() as *const _)
}

/// Main-thread slot for UI objects that must remain in a `static` registry for callback lookup.
/// The slot is synchronized by the main-thread invariant, not by a cross-thread mutex.
pub(crate) struct MainThreadSlot<T> {
    value: RefCell<T>,
}

impl<T> MainThreadSlot<T> {
    pub(crate) const fn new(value: T) -> Self {
        Self {
            value: RefCell::new(value),
        }
    }

    pub(crate) fn lock(&self) -> Result<RefMut<'_, T>, BorrowMutError> {
        crate::debug_assert_main_thread();
        self.value.try_borrow_mut()
    }
}

// The wrapper is only reachable through main-thread callbacks; the contained value is never
// moved out to a worker thread. This is the one narrowly-scoped synchronization boundary for
// legacy static UI registries, instead of marking every raw pointer as Send/Sync.
unsafe impl<T> Sync for MainThreadSlot<T> {}
// The slot itself is a process-global registry cell and is never moved after initialization;
// only its borrow guard is exposed. This permits `LazyLock`/`OnceLock` initialization while the
// contained UI object remains non-Send.
unsafe impl<T> Send for MainThreadSlot<T> {}

/// A retained Objective-C callback target whose identity is handed to AppKit APIs such as
/// `NSTimer`/`NSNotificationCenter`. The runtime owns the object for the process lifetime;
/// callbacks themselves are still required to marshal UI work to the main thread.
#[derive(Clone, Copy)]
pub(crate) struct CallbackTarget(pub(crate) *mut AnyObject);
unsafe impl Send for CallbackTarget {}
unsafe impl Sync for CallbackTarget {}

impl CallbackTarget {
    pub(crate) const fn new(ptr: *mut AnyObject) -> Self {
        Self(ptr)
    }
}

/// Ownership-bearing Core Foundation reference. The constructor is only for APIs documented to
/// return a +1 object; `Drop` balances that retain exactly once.
pub(crate) struct RetainedCf<T> {
    pub(crate) ptr: *const T,
    _marker: PhantomData<T>,
}

/// Marker implemented only for CF object categories whose APIs are documented as immutable and
/// thread-safe in this project. Add a new implementation only after auditing that category.
pub(crate) trait ThreadSafeCf {}
impl ThreadSafeCf for c_void {}

unsafe impl<T: ThreadSafeCf> Send for RetainedCf<T> {}
unsafe impl<T: ThreadSafeCf> Sync for RetainedCf<T> {}

impl<T> RetainedCf<T> {
    pub(crate) const unsafe fn from_retained(ptr: *const T) -> Self {
        Self {
            ptr,
            _marker: PhantomData,
        }
    }
}

impl<T> Drop for RetainedCf<T> {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { CFRelease(self.ptr as *const c_void) };
        }
    }
}

/// Cross-thread handle for a CFRunLoop. Other threads may only signal/wake it; dereferencing
/// and source management remain confined to the owning run-loop thread.
#[derive(Clone, Copy)]
pub(crate) struct RunLoopHandle(pub(crate) *mut c_void);
unsafe impl Send for RunLoopHandle {}
unsafe impl Sync for RunLoopHandle {}

/// Cross-thread handle for a CFRunLoopSource. Other threads may signal it, while source
/// installation/removal remains on the observer thread.
#[derive(Clone, Copy)]
pub(crate) struct RunLoopSourceHandle(pub(crate) *mut c_void);
unsafe impl Send for RunLoopSourceHandle {}
unsafe impl Sync for RunLoopSourceHandle {}

/// Handle for an AXObserver retained by the observer run-loop thread. It is only moved through
/// the observer registry; AX messages and release remain on that owning thread.
#[derive(Clone, Copy)]
pub(crate) struct AxObserverHandle(pub(crate) *mut c_void);
unsafe impl Send for AxObserverHandle {}
unsafe impl Sync for AxObserverHandle {}

/// Read a string value from the running app bundle's Info.plist.
pub(crate) unsafe fn bundle_info_string(key: &str) -> String {
    let bundle: *mut AnyObject = msg_send![class!(NSBundle), mainBundle];
    let key_ns = make_nsstring(key);
    let value: *mut AnyObject = msg_send![bundle, objectForInfoDictionaryKey: key_ns];
    CFRelease(key_ns as *const c_void);
    nsstring_to_rust(value)
}

/// Build an NSString from a Rust &str (CFStringCreateWithCString returns +1; caller must release).
pub(crate) fn make_nsstring(s: &str) -> *mut AnyObject {
    unsafe {
        let c_str = CString::new(s).unwrap();
        let cf = CFStringCreateWithCString(std::ptr::null(), c_str.as_ptr(), 0x08000100u32);
        if cf.is_null() {
            log_info!("CFStringCreateWithCString failed for '{}'", s);
        }
        cf as *mut AnyObject
    }
}

/// Apply a paragraph line-fragment height to a multiline NSTextField's rendered text.
pub(crate) unsafe fn set_text_field_line_height(field: *mut AnyObject, line_height: f64) {
    if field.is_null() {
        return;
    }
    let value: *mut AnyObject = msg_send![field, stringValue];
    if value.is_null() {
        return;
    }
    let length: usize = msg_send![value, length];
    if length == 0 {
        return;
    }

    let style: *mut AnyObject = msg_send![class!(NSMutableParagraphStyle), alloc];
    let style: *mut AnyObject = msg_send![style, init];
    let _: () = msg_send![style, setMinimumLineHeight: line_height];
    let _: () = msg_send![style, setMaximumLineHeight: line_height];
    let alignment: isize = msg_send![field, alignment];
    let line_break_mode: isize = msg_send![field, lineBreakMode];
    let _: () = msg_send![style, setAlignment: alignment];
    let _: () = msg_send![style, setLineBreakMode: line_break_mode];

    // NSTextField ignores its font, color, alignment and line-break properties after receiving
    // an attributed value, so carry those current cell settings into the attributed string.
    let attributes: *mut AnyObject = msg_send![class!(NSMutableDictionary), alloc];
    let attributes: *mut AnyObject = msg_send![attributes, init];
    let font: *mut AnyObject = msg_send![field, font];
    let color: *mut AnyObject = msg_send![field, textColor];
    let font_key = make_nsstring("NSFont");
    let color_key = make_nsstring("NSColor");
    let paragraph_key = make_nsstring("NSParagraphStyle");
    if !font.is_null() {
        let _: () = msg_send![attributes, setObject: font, forKey: font_key];
    }
    if !color.is_null() {
        let _: () = msg_send![attributes, setObject: color, forKey: color_key];
    }
    let _: () = msg_send![attributes, setObject: style, forKey: paragraph_key];
    let attributed: *mut AnyObject = msg_send![class!(NSAttributedString), alloc];
    let attributed: *mut AnyObject =
        msg_send![attributed, initWithString: value, attributes: attributes];
    let _: () = msg_send![field, setAttributedStringValue: attributed];

    CFRelease(font_key as *const c_void);
    CFRelease(color_key as *const c_void);
    CFRelease(paragraph_key as *const c_void);
    CFRelease(attributed as *const c_void);
    CFRelease(attributes as *const c_void);
    CFRelease(style as *const c_void);
}

/// Apply letter spacing (tracking) to a text field's rendered text, the mock's `letter-spacing`
/// on `.section-title`. Same attributed-string approach as `set_text_field_line_height`: the
/// field's font, colour, alignment and line-break mode are carried into the attributed value,
/// which otherwise ignores the cell's own properties. No line-height forcing here -- callers that
/// need it compose both helpers.
pub(crate) unsafe fn set_text_field_kern(field: *mut AnyObject, kern: f64) {
    if field.is_null() {
        return;
    }
    let value: *mut AnyObject = msg_send![field, stringValue];
    if value.is_null() {
        return;
    }
    let length: usize = msg_send![value, length];
    if length == 0 {
        return;
    }

    let style: *mut AnyObject = msg_send![class!(NSMutableParagraphStyle), alloc];
    let style: *mut AnyObject = msg_send![style, init];
    let alignment: isize = msg_send![field, alignment];
    let line_break_mode: isize = msg_send![field, lineBreakMode];
    let _: () = msg_send![style, setAlignment: alignment];
    let _: () = msg_send![style, setLineBreakMode: line_break_mode];

    let attributes: *mut AnyObject = msg_send![class!(NSMutableDictionary), alloc];
    let attributes: *mut AnyObject = msg_send![attributes, init];
    let font: *mut AnyObject = msg_send![field, font];
    let color: *mut AnyObject = msg_send![field, textColor];
    let font_key = make_nsstring("NSFont");
    let color_key = make_nsstring("NSColor");
    let paragraph_key = make_nsstring("NSParagraphStyle");
    let kern_key = make_nsstring("NSKern");
    let kern_number: *mut AnyObject = msg_send![class!(NSNumber), numberWithDouble: kern];
    if !font.is_null() {
        let _: () = msg_send![attributes, setObject: font, forKey: font_key];
    }
    if !color.is_null() {
        let _: () = msg_send![attributes, setObject: color, forKey: color_key];
    }
    let _: () = msg_send![attributes, setObject: style, forKey: paragraph_key];
    if !kern_number.is_null() {
        let _: () = msg_send![attributes, setObject: kern_number, forKey: kern_key];
    }
    let attributed: *mut AnyObject = msg_send![class!(NSAttributedString), alloc];
    let attributed: *mut AnyObject =
        msg_send![attributed, initWithString: value, attributes: attributes];
    let _: () = msg_send![field, setAttributedStringValue: attributed];

    CFRelease(font_key as *const c_void);
    CFRelease(color_key as *const c_void);
    CFRelease(paragraph_key as *const c_void);
    CFRelease(kern_key as *const c_void);
    CFRelease(attributed as *const c_void);
    CFRelease(attributes as *const c_void);
    CFRelease(style as *const c_void);
}

/// The line height a paragraph style forced onto a text field's attributed string, or 0.0 when
/// the field carries none -- `set_text_field_line_height` is the only producer, and a plain field
/// reads back a zero `maximumLineHeight`. Callers must not hardcode the token: the field's own
/// style is the only source that cannot drift from what is drawn.
pub(crate) unsafe fn text_field_forced_line_height(field: *mut AnyObject) -> f64 {
    if field.is_null() {
        return 0.0;
    }
    let value: *mut AnyObject = msg_send![field, attributedStringValue];
    if value.is_null() {
        return 0.0;
    }
    let length: usize = msg_send![value, length];
    if length == 0 {
        return 0.0;
    }
    let key = make_nsstring("NSParagraphStyle");
    if key.is_null() {
        return 0.0;
    }
    let style: *mut AnyObject = msg_send![
        value,
        attribute: key,
        atIndex: 0usize,
        effectiveRange: std::ptr::null_mut::<objc2_foundation::NSRange>()
    ];
    CFRelease(key as *const c_void);
    if style.is_null() {
        return 0.0;
    }
    let height: f64 = msg_send![style, maximumLineHeight];
    if height.is_finite() && height > 0.0 {
        height
    } else {
        0.0
    }
}

/// A font's natural line height, which is where the line fragments would put the glyphs without
/// forcing. NSLayoutManager's `defaultLineHeightForFont:` is the value AppKit actually uses; a
/// missing layout manager falls back to `ascender - descender + leading`, and anything unusable
/// to 0.
unsafe fn font_natural_line_height(font: *mut AnyObject) -> f64 {
    // NSLayoutManager is not thread-safe; every caller is a main-thread settings builder, and
    // this keeps it that way.
    crate::debug_assert_main_thread();
    if font.is_null() {
        return 0.0;
    }
    let manager = {
        // Shared and lazily created: the value is a property of the font, so one layout manager
        // answers for every field that asks.
        static SHARED: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
        *SHARED.get_or_init(|| {
            let manager: *mut AnyObject = msg_send![class!(NSLayoutManager), alloc];
            let manager: *mut AnyObject = msg_send![manager, init];
            manager as usize
        }) as *mut AnyObject
    };
    if !manager.is_null() {
        let height: f64 = msg_send![manager, defaultLineHeightForFont: font];
        if height.is_finite() && height > 0.0 {
            return height;
        }
    }
    let ascender: f64 = msg_send![font, ascender];
    let descender: f64 = msg_send![font, descender];
    let leading: f64 = msg_send![font, leading];
    let height = ascender - descender + leading;
    if height.is_finite() && height > 0.0 {
        height
    } else {
        0.0
    }
}

/// How far a forced line box drops the field's glyphs below its frame's centre (see
/// `theme::forced_line_height_offset`). Returns 0.0 for a field whose line height was never
/// forced, which keeps centring callers a no-op on unforced labels.
pub(crate) unsafe fn text_field_line_height_offset(field: *mut AnyObject) -> f64 {
    let forced = text_field_forced_line_height(field);
    if forced <= 0.0 {
        return 0.0;
    }
    let font: *mut AnyObject = msg_send![field, font];
    crate::theme::forced_line_height_offset(forced, font_natural_line_height(font))
}

/// `text_field_line_height_offset` expressed in the field's superview's coordinate space, for
/// centring code that adds the result to the field's `frame.origin.y`. A flipped superview's y
/// axis points down, so the same visual lift of the ink is a subtraction there. Returns 0.0 for a
/// field with no superview -- a frame that is not positioned inside a parent has nothing to
/// correct.
pub(crate) unsafe fn text_field_line_height_correction(field: *mut AnyObject) -> f64 {
    if field.is_null() {
        return 0.0;
    }
    let superview: *mut AnyObject = msg_send![field, superview];
    if superview.is_null() {
        return 0.0;
    }
    let offset = text_field_line_height_offset(field);
    let flipped: bool = msg_send![superview, isFlipped];
    if flipped {
        -offset
    } else {
        offset
    }
}

/// Split a Rust string at Core Foundation's composed-character boundaries. CFString reports ranges
/// in UTF-16, so use its range API instead of Rust `char` iteration.
pub(crate) unsafe fn composed_character_clusters(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let string = make_nsstring(text);
    if string.is_null() {
        return vec![text.to_string()];
    }
    let cf_string = string as *const c_void;
    let length = CFStringGetLength(cf_string);
    let mut clusters = Vec::new();
    let mut index = 0isize;
    while index < length {
        let range = CFStringGetRangeOfComposedCharactersAtIndex(cf_string, index);
        if range.length <= 0 || range.location + range.length <= index {
            break;
        }
        if let Some(cluster) = cf_string_range_to_string(cf_string, range) {
            clusters.push(cluster);
        }
        index = range.location + range.length;
    }
    CFRelease(cf_string);
    clusters
}

/// Return the first user-perceived character, falling back to an empty string for empty input.
pub(crate) unsafe fn first_composed_character(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let string = make_nsstring(text);
    if string.is_null() {
        return String::new();
    }
    let cf_string = string as *const c_void;
    let range = CFStringGetRangeOfComposedCharactersAtIndex(cf_string, 0);
    if range.length <= 0 {
        CFRelease(cf_string);
        return String::new();
    }
    let result = cf_string_range_to_string(cf_string, range).unwrap_or_default();
    CFRelease(cf_string);
    result
}

unsafe fn cf_string_range_to_string(cf_string: *const c_void, range: CFRange) -> Option<String> {
    let substring = CFStringCreateWithSubstring(std::ptr::null(), cf_string, range);
    if substring.is_null() {
        return None;
    }
    let max_bytes = CFStringGetMaximumSizeForEncoding(range.length, 0x08000100u32);
    let result = if max_bytes >= 0 {
        let mut buffer = vec![0u8; max_bytes as usize + 1];
        if CFStringGetCString(
            substring,
            buffer.as_mut_ptr().cast::<c_char>(),
            buffer.len() as isize,
            0x08000100u32,
        ) {
            Some(
                std::ffi::CStr::from_ptr(buffer.as_ptr().cast::<c_char>())
                    .to_string_lossy()
                    .into_owned(),
            )
        } else {
            None
        }
    } else {
        None
    };
    CFRelease(substring);
    result
}

/// Release a +1 object obtained via alloc. objc2's msg_send! is raw MRC (no ARC):
/// alloc/init return +1 and must be released; addSubview:/setImage:/addTrackingArea:
/// only add their own retain and don't balance the alloc +1. Once the owning view
/// retains it, we drop our alloc +1.
pub(crate) unsafe fn release_obj(obj: *mut AnyObject) {
    if !obj.is_null() {
        let _: () = msg_send![obj, release];
    }
}

/// Whether the current process has Accessibility permission.
pub(crate) fn has_accessibility_permission() -> bool {
    unsafe { AXIsProcessTrusted() }
}

/// Convert an NSString to a Rust String.
pub(crate) unsafe fn nsstring_to_rust(ns: *mut AnyObject) -> String {
    if ns.is_null() {
        return String::new();
    }
    let utf8: *const c_char = msg_send![ns, UTF8String];
    if utf8.is_null() {
        return String::new();
    }
    std::ffi::CStr::from_ptr(utf8)
        .to_string_lossy()
        .into_owned()
}

/// The NSRunningApplication's localizedName (canonical UTF-8; empty = failure). Shared by the
/// window switcher (icon cache) and the clipboard (source app), so the UTF8String conversion
/// isn't hand-rolled twice.
pub(crate) unsafe fn ns_running_app_name(app: *mut AnyObject) -> String {
    if app.is_null() {
        return String::new();
    }
    let name: *mut AnyObject = msg_send![app, localizedName];
    nsstring_to_rust(name)
}

/// The frontmost app as (name, pid). The clipboard grabs both in one lookup at record time:
/// the name feeds the header text, the pid resolves the icon-cache identity
/// (resolve_app_identity) and extracts the small icon.
pub(crate) fn frontmost_app_info() -> (String, i32) {
    unsafe {
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let app: *mut AnyObject = msg_send![workspace, frontmostApplication];
        let name = ns_running_app_name(app);
        let pid: i32 = if app.is_null() {
            -1
        } else {
            msg_send![app, processIdentifier]
        };
        (name, pid)
    }
}

/// hex u32 -> NSColor.
pub(crate) fn hex_to_ns_color(hex: u32) -> *mut AnyObject {
    let r = ((hex >> 24) & 0xFF) as f64 / 255.0;
    let g = ((hex >> 16) & 0xFF) as f64 / 255.0;
    let b = ((hex >> 8) & 0xFF) as f64 / 255.0;
    let a = (hex & 0xFF) as f64 / 255.0;
    unsafe { msg_send![class!(NSColor), colorWithRed: r, green: g, blue: b, alpha: a] }
}

/// NSColor* -> its sRGB components as `[r, g, b, a]` in 0…1, or None when the color cannot be
/// expressed that way (a catalog or pattern color). Used by contrast assertions, which must read
/// the color a view actually carries rather than the token the call site meant to use.
pub(crate) unsafe fn ns_color_components(color: *mut AnyObject) -> Option<[f64; 4]> {
    if color.is_null() {
        return None;
    }
    // Convert first: a catalog color (labelColor and friends) is not directly convertible and
    // would otherwise report zeros, which reads as "black" and silently skews a contrast check.
    let converted: *mut AnyObject = msg_send![color, colorUsingColorSpace: {
        let space: *mut AnyObject = msg_send![class!(NSColorSpace), sRGBColorSpace];
        space
    }];
    if converted.is_null() {
        return None;
    }
    let mut r: f64 = 0.0;
    let mut g: f64 = 0.0;
    let mut b: f64 = 0.0;
    let mut a: f64 = 1.0;
    let _: () = msg_send![
        converted,
        getRed: &mut r as *mut f64,
        green: &mut g as *mut f64,
        blue: &mut b as *mut f64,
        alpha: &mut a as *mut f64
    ];
    Some([r, g, b, a])
}

/// NSColor* -> CGColorRef. Uses raw objc_msgSend because objc2's msg_send! can't encode CF/CG types.
pub(crate) unsafe fn ns_color_to_cg(ns: *mut AnyObject) -> *mut c_void {
    let sel = sel!(CGColor);
    extern "C" {
        fn objc_msgSend();
    }
    type F = unsafe extern "C" fn(*mut c_void, Sel) -> *mut c_void;
    let f: F = std::mem::transmute(objc_msgSend as *const ());
    f(ns as *mut c_void, sel)
}

/// Convert hex u32 -> CGColorRef for use with `CALayer.backgroundColor` / `borderColor` / `shadowColor`.
///
/// Those properties are declared without `strong`/`copy` in `QuartzCore/Headers/CALayer.h`, but Core Animation
/// holds CF-typed layer properties with CF ownership regardless (Apple QA1565), so the layer keeps the colour
/// alive and the returned pointer does not have to outlive the call. It is deliberately *not* cached: the
/// values include user-configured colours (`theme::colors_from_config`), so a cache keyed by hex would grow
/// without a bound as a user experiments with the colour pickers.
pub(crate) fn hex_to_cg_color(hex: u32) -> *mut c_void {
    let ns = hex_to_ns_color(hex);
    unsafe { ns_color_to_cg(ns) }
}

/// Set CALayer.backgroundColor using raw objc_msgSend (CGColorRef, not NSColor*).
pub(crate) unsafe fn layer_set_background(layer: *mut AnyObject, cg: *mut c_void) {
    let sel = sel!(setBackgroundColor:);
    extern "C" {
        fn objc_msgSend();
    }
    type F = unsafe extern "C" fn(*mut c_void, Sel, *mut c_void);
    let f: F = std::mem::transmute(objc_msgSend as *const ());
    f(layer as *mut c_void, sel, cg);
}

/// Read CALayer.backgroundColor as a CGColorRef without objc2's object-pointer signature check.
pub(crate) unsafe fn layer_background_color(layer: *mut AnyObject) -> *mut c_void {
    let sel = sel!(backgroundColor);
    extern "C" {
        fn objc_msgSend();
    }
    type F = unsafe extern "C" fn(*mut c_void, Sel) -> *mut c_void;
    let f: F = std::mem::transmute(objc_msgSend as *const ());
    f(layer as *mut c_void, sel)
}

/// Read CALayer.borderColor as a CGColorRef without objc2's object-pointer signature check.
pub(crate) unsafe fn layer_border_color(layer: *mut AnyObject) -> *mut c_void {
    let sel = sel!(borderColor);
    extern "C" {
        fn objc_msgSend();
    }
    type F = unsafe extern "C" fn(*mut c_void, Sel) -> *mut c_void;
    let f: F = std::mem::transmute(objc_msgSend as *const ());
    f(layer as *mut c_void, sel)
}

/// Set CALayer.cornerCurve to `continuous` -- the squircle Apple's own surfaces use, which is
/// what lets a rounded plate nest around an app icon without pinching its corners. A no-op
/// where the property does not exist (a CALayer before macOS 10.15).
pub(crate) unsafe fn layer_set_continuous_corner_curve(layer: *mut AnyObject) {
    let responds: bool = msg_send![layer, respondsToSelector: sel!(setCornerCurve:)];
    if !responds {
        return;
    }
    let curve = make_nsstring("continuous");
    let _: () = msg_send![layer, setCornerCurve: curve];
    CFRelease(curve as *const c_void);
}

/// Set CALayer.borderColor using raw objc_msgSend (CGColorRef, not NSColor*).
pub(crate) unsafe fn layer_set_border(layer: *mut AnyObject, cg: *mut c_void) {
    let sel = sel!(setBorderColor:);
    extern "C" {
        fn objc_msgSend();
    }
    type F = unsafe extern "C" fn(*mut c_void, Sel, *mut c_void);
    let f: F = std::mem::transmute(objc_msgSend as *const ());
    f(layer as *mut c_void, sel, cg);
}

/// Set CALayer.shadowColor using raw objc_msgSend (CGColorRef, not NSColor*).
pub(crate) unsafe fn layer_set_shadow_color(layer: *mut AnyObject, cg: *mut c_void) {
    let sel = sel!(setShadowColor:);
    extern "C" {
        fn objc_msgSend();
    }
    type F = unsafe extern "C" fn(*mut c_void, Sel, *mut c_void);
    let f: F = std::mem::transmute(objc_msgSend as *const ());
    f(layer as *mut c_void, sel, cg);
}

/// Set CALayer.shadowPath from a +1 path. The layer copies it (see
/// [`layer_set_rounded_shadow_path`]), so the caller still owns the reference it passed.
pub(crate) unsafe fn layer_set_shadow_path(layer: *mut AnyObject, path: *mut c_void) {
    let sel = sel!(setShadowPath:);
    extern "C" {
        fn objc_msgSend();
    }
    type F = unsafe extern "C" fn(*mut c_void, Sel, *mut c_void);
    let f: F = std::mem::transmute(objc_msgSend as *const ());
    f(layer as *mut c_void, sel, path);
}

/// Read CALayer.shadowPath as a **borrowed** reference: the layer owns what it holds, so a reader must
/// not release it. It is a Core Graphics object, so `release_obj` (which sends `release`) would be
/// wrong on it in every direction.
pub(crate) unsafe fn layer_shadow_path(layer: *mut AnyObject) -> *mut c_void {
    let sel = sel!(shadowPath);
    extern "C" {
        fn objc_msgSend();
    }
    type F = unsafe extern "C" fn(*mut c_void, Sel) -> *mut c_void;
    let f: F = std::mem::transmute(objc_msgSend as *const ());
    f(layer as *mut c_void, sel)
}

/// Give `layer` a rounded-rect shadow path covering `rect` **in the layer's own coordinate space**, and
/// release this function's own reference. `rect` is the shadow's silhouette -- the panel itself, not the
/// padded window: the padding only decides how much room the blur has before the window edges clip it.
///
/// `CALayer.shadowPath` copies what it is assigned ("Upon assignment the path is copied", in
/// `QuartzCore/Headers/CALayer.h`), so the +1 from `CGPathCreateWithRoundedRect` is released here and the
/// layer keeps its own copy. Core Graphics objects are not Objective-C objects: `CFRelease`, never
/// `release_obj`.
pub(crate) unsafe fn layer_set_rounded_shadow_path(
    layer: *mut AnyObject,
    rect: CGRect,
    radius: f64,
) {
    let path = CGPathCreateWithRoundedRect(rect, radius, radius, std::ptr::null());
    if path.is_null() {
        return;
    }
    layer_set_shadow_path(layer, path);
    CFRelease(path as *const c_void);
}

/// Set CALayer.contents from a CGImageRef (raw objc_msgSend: objc2 cannot encode CF/CG types).
pub(crate) unsafe fn layer_set_contents(layer: *mut AnyObject, cg: *mut c_void) {
    let sel = sel!(setContents:);
    extern "C" {
        fn objc_msgSend();
    }
    type F = unsafe extern "C" fn(*mut c_void, Sel, *mut c_void);
    let f: F = std::mem::transmute(objc_msgSend as *const ());
    f(layer as *mut c_void, sel, cg);
}

/// Set CALayer.contentsScale (CGFloat; raw for symmetry with [`layer_set_contents`]).
pub(crate) unsafe fn layer_set_contents_scale(layer: *mut AnyObject, scale: f64) {
    let sel = sel!(setContentsScale:);
    extern "C" {
        fn objc_msgSend();
    }
    type F = unsafe extern "C" fn(*mut c_void, Sel, f64);
    let f: F = std::mem::transmute(objc_msgSend as *const ());
    f(layer as *mut c_void, sel, scale);
}

/// Present an NSSavePanel (runModal) and return the chosen filesystem path; None on
/// cancel. Shared by clipboard "save as" and settings "export logs". See the lifetime
/// notes carried over from the original implementation: the URL/path property getters
/// return +0 (autoreleased) per Cocoa convention and sit on the surrounding pool --
/// never release them manually.
pub(crate) unsafe fn run_save_panel(suggested_name: &str) -> Option<String> {
    // Wrap in a pool to reclaim temporaries; objects autoreleased inside runModal's
    // nested event loop are managed by AppKit's own pools and stay untouched.
    let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
    let panel: *mut AnyObject = msg_send![class!(NSSavePanel), savePanel];
    let name_ns = make_nsstring(suggested_name);
    let _: () = msg_send![panel, setNameFieldStringValue: name_ns];
    CFRelease(name_ns as *const c_void);
    let resp: isize = msg_send![panel, runModal]; // NSModalResponseOK == 1
    let result = if resp == 1 {
        let url: *mut AnyObject = msg_send![panel, URL];
        if !url.is_null() {
            let path_ns: *mut AnyObject = msg_send![url, path];
            let path = nsstring_to_rust(path_ns);
            (!path.is_empty()).then_some(path)
        } else {
            None
        }
    } else {
        None
    };
    let _: () = msg_send![pool, drain];
    result
}

/// Present an NSOpenPanel (runModal) and return the chosen filesystem path; None on cancel.
/// The read-only sibling of the save-panel helper, sharing its lifetime rules: the URL/path
/// getters return +0 (autoreleased) values on the surrounding pool, so the Rust string is copied
/// out before anything is released.
pub(crate) unsafe fn run_open_panel() -> Option<String> {
    // Wrap in a pool to reclaim temporaries; objects autoreleased inside runModal's nested event
    // loop are managed by AppKit's own pools and stay untouched.
    let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
    let panel: *mut AnyObject = msg_send![class!(NSOpenPanel), openPanel];
    let _: () = msg_send![panel, setCanChooseFiles: true];
    let _: () = msg_send![panel, setCanChooseDirectories: false];
    let _: () = msg_send![panel, setAllowsMultipleSelection: false];
    let resp: isize = msg_send![panel, runModal]; // NSModalResponseOK == 1
    let result = if resp == 1 {
        let url: *mut AnyObject = msg_send![panel, URL];
        if !url.is_null() {
            let path_ns: *mut AnyObject = msg_send![url, path];
            let path = nsstring_to_rust(path_ns);
            (!path.is_empty()).then_some(path)
        } else {
            None
        }
    } else {
        None
    };
    let _: () = msg_send![pool, drain];
    result
}

/// Present an NSOpenPanel restricted to `.app` packages and return the chosen path.
///
/// Same lifetime rules as `run_open_panel`. `treatsFilePackagesAsDirectories` stays false so a
/// bundle is SELECTED as a file instead of being entered, and the file-type filter keeps a plain
/// document from being picked where an application is expected.
pub(crate) unsafe fn run_open_panel_for_app() -> Option<String> {
    let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
    let panel: *mut AnyObject = msg_send![class!(NSOpenPanel), openPanel];
    let _: () = msg_send![panel, setCanChooseFiles: true];
    let _: () = msg_send![panel, setCanChooseDirectories: false];
    let _: () = msg_send![panel, setAllowsMultipleSelection: false];
    let _: () = msg_send![panel, setTreatsFilePackagesAsDirectories: false];
    let extensions = make_nsstring("app");
    let types: *mut AnyObject = msg_send![class!(NSArray), arrayWithObject: extensions];
    CFRelease(extensions as *const c_void);
    let _: () = msg_send![panel, setAllowedFileTypes: types];
    let resp: isize = msg_send![panel, runModal]; // NSModalResponseOK == 1
    let result = if resp == 1 {
        let url: *mut AnyObject = msg_send![panel, URL];
        if !url.is_null() {
            let path_ns: *mut AnyObject = msg_send![url, path];
            let path = nsstring_to_rust(path_ns);
            (!path.is_empty()).then_some(path)
        } else {
            None
        }
    } else {
        None
    };
    let _: () = msg_send![pool, drain];
    result
}

/// Tests for the dynamic-class helpers: the properties this migration relies on, and the
/// counter-examples that must keep failing if someone reverts a registration site to a
/// hand-written `class_addMethod` with a wrong encoding.
#[cfg(test)]
mod dynamic_class_tests {
    use super::*;
    use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, Sel};
    use std::ffi::c_void;

    /// `ClassBuilder` derives the encoding from the callback's Rust signature, so an object
    /// parameter declared as `*mut c_void` encodes as '^v' and the superclass check rejects it.
    ///
    /// The override check only runs under `debug_assertions`, so the expectation differs by
    /// build: debug rejects, release accepts. Asserting one outcome unconditionally would make
    /// `cargo test --release` fail for a reason that is the dependency's designed behaviour.
    #[test]
    fn a_void_pointer_object_parameter_is_rejected_only_where_objc2_checks_encodings() {
        extern "C" fn wrong(_this: *mut AnyObject, _cmd: Sel, _event: *mut c_void) {}

        // The superclass must declare mouseDown: (NSView does, NSObject does not): objc2 only
        // compares encodings against an inherited method, so on NSObject this would be vacuous.
        let rejected = std::panic::catch_unwind(|| unsafe {
            let mut builder = ClassBuilder::new(c"OmTFfiTestWrongPointer", class!(NSView)).unwrap();
            builder.add_method::<AnyObject, _>(
                sel!(mouseDown:),
                wrong as unsafe extern "C" fn(*mut AnyObject, Sel, *mut c_void),
            );
            let _ = builder.register();
        });

        let checked = cfg!(debug_assertions);
        assert_eq!(
            rejected.is_err(),
            checked,
            "the override encoding check runs under debug_assertions only: \
             expected rejection={checked}, got rejection={}",
            rejected.is_err()
        );
    }

    /// The callback with the matching object type registers cleanly in every build, so the case
    /// above is about the encoding and not about the selector or the class.
    #[test]
    fn an_object_parameter_registers_cleanly() {
        extern "C" fn right(_this: *mut AnyObject, _cmd: Sel, _event: *mut AnyObject) {}
        unsafe {
            let mut builder = ClassBuilder::new(c"OmTFfiTestRightPointer", class!(NSView)).unwrap();
            builder.add_method::<AnyObject, _>(
                sel!(mouseDown:),
                right as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
            );
            let _ = builder.register();
        }
    }

    /// The argument count check runs regardless of `debug_assertions`: a selector that takes one
    /// argument cannot be registered with a callback that takes none. This is the half of the
    /// guarantee that also holds in release builds.
    #[test]
    fn a_callback_with_the_wrong_arity_is_rejected() {
        extern "C" fn no_arguments(_this: *mut AnyObject, _cmd: Sel) {}
        let rejected = std::panic::catch_unwind(|| unsafe {
            let mut builder = ClassBuilder::new(c"OmTFfiTestArity", class!(NSView)).unwrap();
            builder.add_method::<AnyObject, _>(
                sel!(mouseDown:),
                no_arguments as unsafe extern "C" fn(*mut AnyObject, Sel),
            );
            let _ = builder.register();
        });
        assert!(rejected.is_err(), "an arity mismatch must be rejected");
    }

    /// `ObjcSuper::for_defined_class` resolves the superclass from the class that defines the
    /// override. Handing it an instance would silently start the search at the wrong place, so
    /// the type takes a `StaticClass` — which only the class helpers produce.
    #[test]
    fn objc_super_derives_the_superclass_from_the_defined_class() {
        unsafe {
            let defined = StaticClass(class!(NSView) as *const AnyClass);
            let sup = ObjcSuper::for_defined_class(std::ptr::null_mut(), defined);
            let expected = objc2::ffi::class_getSuperclass(class!(NSView) as *const AnyClass);
            assert_eq!(
                sup.super_class, expected as *mut c_void,
                "objc_super.super_class must be the superclass of the defined class"
            );
            // The receiver is passed through untouched; only the superclass is derived.
            assert!(sup.receiver.is_null());
        }
    }

    /// The layout the C ABI sees: two pointers, in that order.
    #[test]
    fn objc_super_is_two_pointers() {
        use std::mem::{size_of, MaybeUninit};
        assert_eq!(size_of::<ObjcSuper>(), 2 * size_of::<*const c_void>());

        let mut sup = MaybeUninit::<ObjcSuper>::uninit();
        let base = sup.as_mut_ptr() as usize;
        // SAFETY: only addresses of the fields are taken, nothing is read.
        let receiver_at = unsafe { std::ptr::addr_of!((*sup.as_ptr()).receiver) } as usize;
        let super_at = unsafe { std::ptr::addr_of!((*sup.as_ptr()).super_class) } as usize;
        assert_eq!(receiver_at - base, 0);
        assert_eq!(super_at - base, size_of::<*const c_void>());
    }

    /// `StaticClass` is transparent over the class pointer, which is what lets it be embedded in
    /// and converted to the raw pointer a call site needs without a layout change.
    #[test]
    fn static_class_is_transparent_over_a_pointer() {
        use std::mem::size_of;
        assert_eq!(size_of::<StaticClass>(), size_of::<*const c_void>());
    }

    /// `register_once` runs its builder exactly once, so a call site that used to allocate the
    /// class on every invocation now allocates it on the first.
    #[test]
    fn register_once_builds_the_class_exactly_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static ONCE: std::sync::OnceLock<StaticClass> = std::sync::OnceLock::new();
        static RUNS: AtomicUsize = AtomicUsize::new(0);

        let first = StaticClass::register_once(&ONCE, c"OmTFfiTestOnce", || {
            RUNS.fetch_add(1, Ordering::SeqCst);
            build_class(c"OmTFfiTestOnce", class!(NSView), |_builder| {})
        });
        let second = StaticClass::register_once(&ONCE, c"OmTFfiTestOnce", || {
            RUNS.fetch_add(1, Ordering::SeqCst);
            build_class(c"OmTFfiTestOnce", class!(NSView), |_builder| {})
        });

        assert_eq!(RUNS.load(Ordering::SeqCst), 1, "the builder must run once");
        assert_eq!(first.0, second.0, "both calls must return the same class");
    }

    /// Registering a name twice is a bug (two one-shot paths both ran), and it must be loud:
    /// `objc_allocateClassPair` returns nil, and every following `class_addMethod` would write
    /// through it.
    #[test]
    fn building_the_same_class_name_twice_panics() {
        build_class(c"OmTFfiTestDuplicate", class!(NSView), |_builder| {});
        let second = std::panic::catch_unwind(|| {
            build_class(c"OmTFfiTestDuplicate", class!(NSView), |_builder| {});
        });
        assert!(second.is_err(), "a duplicate class name must panic");
    }

    /// A bounded check on the production registration entries.
    ///
    /// The tests above prove the helpers behave; they cannot notice a call site that went back to
    /// a hand-written `class_addMethod` with a literal encoding, or that dropped its `OnceLock`.
    /// This scans the source of the modules this migration converted and requires each converted
    /// class getter to go through the type-checked path. It is deliberately a source scan and not
    /// an analysis: it proves "these named entries use the helper", not "no encoding is written
    /// by hand anywhere".
    #[test]
    fn converted_class_getters_register_through_the_typed_helper() {
        // Files whose class getters were converted. Adding a file here is how a new conversion
        // joins the gate; a conversion that is reverted fails the assertion below.
        let sources: [(&str, &str); 11] = [
            ("pointer_locator.rs", include_str!("pointer_locator.rs")),
            ("glass.rs", include_str!("glass.rs")),
            ("settings/tooltip.rs", include_str!("settings/tooltip.rs")),
            ("clipboard/search.rs", include_str!("clipboard/search.rs")),
            (
                "settings/window/chrome.rs",
                include_str!("settings/window/chrome.rs"),
            ),
            ("settings/widgets.rs", include_str!("settings/widgets.rs")),
            ("clipboard/picker.rs", include_str!("clipboard/picker.rs")),
            (
                "overlay/footer_settings.rs",
                include_str!("overlay/footer_settings.rs"),
            ),
            (
                "clipboard/text_style.rs",
                include_str!("clipboard/text_style.rs"),
            ),
            (
                "keystroke_display/panel.rs",
                include_str!("keystroke_display/panel.rs"),
            ),
            ("ui/corner_glyph.rs", include_str!("ui/corner_glyph.rs")),
        ];

        // Each converted getter, by name. A getter that is renamed must be renamed here too;
        // that is what keeps the gate attached to the entries rather than to a file.
        let getters = [
            ConvertedGetter {
                file: "pointer_locator.rs",
                getter: "locator_panel_class",
            },
            ConvertedGetter {
                file: "pointer_locator.rs",
                getter: "locator_content_class",
            },
            ConvertedGetter {
                file: "glass.rs",
                getter: "vibrant_label_class",
            },
            ConvertedGetter {
                file: "glass.rs",
                getter: "decoration_view_class",
            },
            ConvertedGetter {
                file: "glass.rs",
                getter: "carrier_class",
            },
            ConvertedGetter {
                file: "settings/tooltip.rs",
                getter: "tooltip_bubble_view_class",
            },
            ConvertedGetter {
                file: "settings/tooltip.rs",
                getter: "disabled_cursor_target",
            },
            ConvertedGetter {
                file: "clipboard/search.rs",
                getter: "search_cell_class_handle",
            },
            ConvertedGetter {
                file: "settings/window/chrome.rs",
                getter: "settings_root_view_class",
            },
            ConvertedGetter {
                file: "settings/window/chrome.rs",
                getter: "settings_window_class_handle",
            },
            ConvertedGetter {
                file: "settings/widgets.rs",
                getter: "html_action_button_class_handle",
            },
            ConvertedGetter {
                file: "settings/widgets.rs",
                getter: "flipped_settings_view_class",
            },
            ConvertedGetter {
                file: "settings/widgets.rs",
                getter: "html_switch_class_handle",
            },
            ConvertedGetter {
                file: "settings/widgets.rs",
                getter: "centered_text_field_cell_class_handle",
            },
            ConvertedGetter {
                file: "clipboard/picker.rs",
                getter: "detail_text_view_class_handle",
            },
            ConvertedGetter {
                file: "overlay/footer_settings.rs",
                getter: "footer_settings_button_class_handle",
            },
            ConvertedGetter {
                file: "clipboard/text_style.rs",
                getter: "hover_button_class_handle",
            },
        ];

        if let Err(problem) = check_converted_sources(&sources, &getters) {
            panic!("{problem}");
        }
    }

    /// One converted class getter: the file it lives in and the getter's name.
    ///
    /// Naming each getter is what makes the gate about the entries rather than about a file: a
    /// file can keep one correctly converted getter while a second one regresses back to a bare
    /// class_addMethod, and a whole-file check would never notice.
    struct ConvertedGetter {
        file: &'static str,
        getter: &'static str,
    }

    /// The rule behind the scan, as a pure function so its counter-examples can be tested without
    /// editing the tree. A gate whose only evidence is "it passes today" cannot show it would
    /// catch the regression it exists for.
    fn check_converted_sources(
        sources: &[(&'static str, &str)],
        getters: &[ConvertedGetter],
    ) -> Result<(), String> {
        for (name, source) in sources {
            if source.contains("ObjcSuper {") {
                return Err(format!(
                    "{name} still builds ObjcSuper by hand; super dispatch must go through for_defined_class"
                ));
            }
        }

        for entry in getters {
            let Some((_, source)) = sources.iter().find(|(name, _)| *name == entry.file) else {
                return Err(format!("{} is not among the scanned sources", entry.file));
            };
            let Some(body) = function_body(source, entry.getter) else {
                return Err(format!(
                    "{}: getter {} was not found; the gate names it explicitly, so a rename must update the gate",
                    entry.file, entry.getter
                ));
            };
            if !body.contains("register_once") {
                return Err(format!(
                    "{}: getter {} no longer registers through StaticClass::register_once",
                    entry.file, entry.getter
                ));
            }
            if body.contains("objc_allocateClassPair") || body.contains("class_addMethod") {
                return Err(format!(
                    "{}: getter {} registers methods by hand instead of through build_class",
                    entry.file, entry.getter
                ));
            }
        }
        Ok(())
    }

    /// The text of `fn name(...)` up to the closing brace at the same depth.
    ///
    /// A bounded slice, not a parser: the gate only needs "is this string inside that function",
    /// and a getter the slice cannot isolate fails the "was not found" case rather than passing.
    fn function_body<'a>(source: &'a str, name: &str) -> Option<&'a str> {
        let needle = format!("fn {name}(");
        let start = source.find(&needle)?;
        let rest = &source[start..];
        let mut depth = 0usize;
        let mut seen_open = false;
        for (offset, ch) in rest.char_indices() {
            match ch {
                '{' => {
                    depth += 1;
                    seen_open = true;
                }
                '}' => {
                    depth -= 1;
                    if seen_open && depth == 0 {
                        return Some(&rest[..offset]);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Counter-examples for the scan above: each shape it must reject, plus the clean shape.
    #[test]
    fn the_conversion_gate_rejects_each_way_it_can_be_undone() {
        let getter =
            |file: &'static str, name: &'static str| ConvertedGetter { file, getter: name };

        // A hand-built super receiver is the regression the type change exists to prevent.
        assert!(
            check_converted_sources(
                &[(
                    "some.rs",
                    "let mut sup = ObjcSuper { receiver, super_class };"
                )],
                &[],
            )
            .is_err(),
            "a hand-built ObjcSuper must be rejected"
        );

        // The clean shape passes, so the rules above are about the violations.
        assert!(check_converted_sources(
            &[(
                "some.rs",
                "fn getter() { StaticClass::register_once(&C, c\"X\", b) }"
            )],
            &[getter("some.rs", "getter")],
        )
        .is_ok());

        // One file, one correct getter and one that went back to a bare allocation: the whole-file
        // shape of the first version of this gate passed here, and it must not.
        let mixed = "fn good() { StaticClass::register_once(&C, c\"X\", b) }\n                     fn bad() { let cls = objc_allocateClassPair(super, name, 0); }";
        assert!(
            check_converted_sources(
                &[("some.rs", mixed)],
                &[getter("some.rs", "good"), getter("some.rs", "bad")],
            )
            .is_err(),
            "a second getter in the same file must be checked on its own"
        );

        // The OnceLock kept but the method written by hand: the idempotence rule alone passes
        // here, so the encoding rule has to catch it.
        let kept_lock = "fn getter() { StaticClass::register_once(&C, c\"X\", || { \
                         class_addMethod(cls, sel, imp, types) }) }";
        assert!(
            check_converted_sources(&[("some.rs", kept_lock)], &[getter("some.rs", "getter")])
                .is_err(),
            "a getter that keeps register_once but writes the method by hand must be rejected"
        );

        // A renamed getter cannot silently drop out of the gate.
        assert!(
            check_converted_sources(
                &[("some.rs", "fn other() {}")],
                &[getter("some.rs", "getter")]
            )
            .is_err(),
            "a getter the gate names but cannot find must be an error"
        );

        // The corner_glyph exception is narrow: it may skip the register rule, but not the
        // hand-built-super rule.
        assert!(check_converted_sources(&[("ui/corner_glyph.rs", "fn f() {}")], &[]).is_ok());
        assert!(check_converted_sources(
            &[("ui/corner_glyph.rs", "ObjcSuper { receiver, super_class }")],
            &[]
        )
        .is_err());
    }

    /// The production pool helper must drain when the body unwinds.
    ///
    /// This is the defect the old hand-written `new -> body() -> drain` pair had: a panic inside
    /// `body` skipped the drain entirely. A test whose body returns normally passes against that
    /// old implementation too, so the observation has to be a panic — and it has to go through the
    /// production wrapper, not through `objc2::rc::autoreleasepool`, or it would keep passing
    /// while the wrapper regressed.
    #[test]
    fn the_panel_key_pool_helper_drains_while_unwinding() {
        use std::sync::atomic::Ordering;
        let _serial = POOL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        DEALLOCS.store(0, Ordering::SeqCst);

        let panicked = std::panic::catch_unwind(|| {
            crate::overlay::panel_keys::autorelease_pool_for_test(|| unsafe {
                let object = autocreate_probe();
                let _: *mut AnyObject = msg_send![object, autorelease];
                panic!("intentional: the helper's pool must still drain");
            })
        });

        assert!(panicked.is_err(), "the body was supposed to panic");
        assert_eq!(
            DEALLOCS.load(Ordering::SeqCst),
            1,
            "the production pool helper must drain the pool while the body unwinds"
        );
    }

    /// An autorelease pool must drain on the early-return path as well, which is the shape the
    /// hand-written new/drain pairs in `icon_cache` used to get wrong.
    ///
    /// The observation is the object's own `-dealloc`, counted in a static: a weak table was the
    /// first attempt and does not work here — its count stayed at 1 after the drain, so it would
    /// have reported "never released" for a pool that did release.
    static DEALLOCS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    /// The three pool tests share one counter and one probe class, so they must not overlap: the
    /// test harness runs tests on several threads, and a release from a neighbouring test lands in
    /// this counter.
    static POOL_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Count the deallocation and then let NSObject free the instance.
    ///
    /// An override of `-dealloc` that only counts would leave the object itself unfreed: "it owns
    /// no fields" is not the same as "the allocation needs no release". Calling super is also what
    /// makes the test prove a real destroy rather than merely that the callback was entered.
    extern "C" fn probe_dealloc(this: *mut AnyObject, _cmd: Sel) {
        DEALLOCS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut sup =
            ObjcSuper::for_defined_class(this as *mut c_void, StaticClass(probe_class_ptr()));
        type Dealloc = unsafe extern "C" fn(*mut ObjcSuper, Sel);
        let send: Dealloc =
            unsafe { std::mem::transmute(objc2::ffi::objc_msgSendSuper as *const ()) };
        unsafe { send(&mut sup, sel!(dealloc)) };
    }

    /// The class object itself, for the super dispatch above. It is registered by the time
    /// `-dealloc` can run; asking for it again here returns the same already-registered class
    /// (register_once is idempotent) rather than re-entering the builder.
    fn probe_class_ptr() -> *const objc2::runtime::AnyClass {
        probe_class().0
    }
    fn probe_class() -> StaticClass {
        static CLASS: std::sync::OnceLock<StaticClass> = std::sync::OnceLock::new();
        StaticClass::register_once(&CLASS, c"OmTFfiTestPoolProbe", || {
            build_class(c"OmTFfiTestPoolProbe", class!(NSObject), |builder| unsafe {
                builder.add_method::<AnyObject, _>(
                    sel!(dealloc),
                    probe_dealloc as unsafe extern "C" fn(*mut AnyObject, Sel),
                );
            })
        })
    }

    fn autocreate_probe() -> *mut AnyObject {
        unsafe {
            let object: *mut AnyObject = msg_send![probe_class().0 as *mut AnyObject, alloc];
            msg_send![object, init]
        }
    }

    #[test]
    fn autoreleasepool_drains_on_an_early_return() {
        use std::sync::atomic::Ordering;
        let _serial = POOL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        DEALLOCS.store(0, Ordering::SeqCst);

        unsafe {
            objc2::rc::autoreleasepool(|_pool| {
                let object = autocreate_probe();
                let _: *mut AnyObject = msg_send![object, autorelease];
                // Early return from the closure: the pool still has to drain what it owns.
                assert_eq!(
                    DEALLOCS.load(Ordering::SeqCst),
                    0,
                    "the object must still be alive inside the pool"
                );
            });
            assert_eq!(
                DEALLOCS.load(Ordering::SeqCst),
                1,
                "the pool must have released the object on the early-return path"
            );
        }
    }

    /// The same observation for the normal-return path, so the test above cannot pass merely
    /// because the probe was released for some unrelated reason.
    #[test]
    fn autoreleasepool_drains_on_a_normal_return() {
        use std::sync::atomic::Ordering;
        let _serial = POOL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        DEALLOCS.store(0, Ordering::SeqCst);

        unsafe {
            let kept = objc2::rc::autoreleasepool(|_pool| {
                let object = autocreate_probe();
                let _: *mut AnyObject = msg_send![object, autorelease];
                1
            });
            assert_eq!(kept, 1);
            assert_eq!(
                DEALLOCS.load(Ordering::SeqCst),
                1,
                "the pool must have released the object on the normal-return path"
            );
        }
    }

    /// A panic unwinds through the closure; the pool must drain there too, which the hand-written
    /// new/drain pairs never did.
    #[test]
    fn autoreleasepool_drains_while_unwinding() {
        use std::sync::atomic::Ordering;
        let _serial = POOL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        DEALLOCS.store(0, Ordering::SeqCst);

        let panicked = std::panic::catch_unwind(|| unsafe {
            objc2::rc::autoreleasepool(|_pool| {
                let object = autocreate_probe();
                let _: *mut AnyObject = msg_send![object, autorelease];
                panic!("intentional: the pool must still drain");
            })
        });
        assert!(panicked.is_err());
        assert_eq!(
            DEALLOCS.load(Ordering::SeqCst),
            1,
            "the pool must drain while a panic unwinds"
        );
    }
}

#[cfg(test)]
mod composed_character_tests {
    use super::{composed_character_clusters, first_composed_character};

    #[test]
    fn composed_character_helpers_keep_grapheme_clusters_intact() {
        unsafe {
            assert_eq!(first_composed_character("e\u{301}clair"), "e\u{301}");
            assert_eq!(first_composed_character("👩‍💻app"), "👩‍💻");
            assert_eq!(
                composed_character_clusters("e\u{301}👩‍💻x"),
                vec!["e\u{301}".to_string(), "👩‍💻".to_string(), "x".to_string()]
            );
            assert!(composed_character_clusters("").is_empty());
        }
    }
}
