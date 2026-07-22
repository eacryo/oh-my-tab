use flume::Sender;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

#[derive(Debug, Clone, Copy)]
pub enum GlobalEvent {
    CmdTabPressed,
    CmdReleased,
    ThemeToggled,
}

type CGEventRef = *mut c_void;
type CGEventTapProxy = *mut c_void;
type CFMachPortRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
type CFRunLoopRef = *mut c_void;
type CFStringRef = *mut c_void;
type CFAllocatorRef = *mut c_void;
type CGEventType = u32;
type CGEventFlags = u64;
type CGEventMask = u64;

const K_CG_EVENT_KEY_DOWN: CGEventType = 10;
const K_CG_EVENT_FLAGS_CHANGED: CGEventType = 12;
const K_CG_KEYBOARD_EVENT_KEYCODE: i32 = 9;
const K_CG_EVENT_FLAG_MASK_COMMAND: CGEventFlags = 0x00100000;
const K_VK_TAB: u16 = 48;

#[link(name = "CoreGraphics", kind = "framework")]
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(
        tap: i32,
        place: i32,
        options: u32,
        events_of_interest: CGEventMask,
        callback: CGEventTapCallBack,
        user_info: *mut c_void,
    ) -> CFMachPortRef;

    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventGetIntegerValueField(event: CGEventRef, field: i32) -> i64;
    fn CGEventGetFlags(event: CGEventRef) -> CGEventFlags;

    fn CFMachPortCreateRunLoopSource(
        allocator: CFAllocatorRef,
        port: CFMachPortRef,
        order: i64,
    ) -> CFRunLoopSourceRef;

    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopRun();
    fn CFRelease(cf: *const c_void);

    static kCFRunLoopDefaultMode: CFStringRef;
}

// 标记是否已经发送过 CmdTabPressed，防止修饰键变化时误发 CmdReleased
// Tracks whether CmdTabPressed was sent, to avoid spurious CmdReleased
static TAB_PRESSED: AtomicBool = AtomicBool::new(false);

type CGEventTapCallBack =Option<
    unsafe extern "C" fn(
        proxy: CGEventTapProxy,
        event_type: CGEventType,
        event: CGEventRef,
        user_info: *mut c_void,
    ) -> CGEventRef,
>;

unsafe extern "C" fn event_tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef {
    let sender = &*(user_info as *const Sender<GlobalEvent>);

    match event_type {
        K_CG_EVENT_KEY_DOWN => {
            let keycode =
                CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_KEYCODE) as u16;
            let flags = CGEventGetFlags(event);

            if keycode == K_VK_TAB && (flags & K_CG_EVENT_FLAG_MASK_COMMAND) != 0 {
                TAB_PRESSED.store(true, Ordering::SeqCst);
                let _ = sender.send(GlobalEvent::CmdTabPressed);
                return std::ptr::null_mut();
            }
        }
        K_CG_EVENT_FLAGS_CHANGED => {
            let flags = CGEventGetFlags(event);
            if (flags & K_CG_EVENT_FLAG_MASK_COMMAND) == 0 && TAB_PRESSED.swap(false, Ordering::SeqCst) {
                let _ = sender.send(GlobalEvent::CmdReleased);
            }
        }
        _ => {}
    }

    event
}

pub fn start(sender: Sender<GlobalEvent>) -> thread::JoinHandle<()> {
    thread::spawn(move || unsafe {
        let sender_ptr = Box::into_raw(Box::new(sender)) as *mut c_void;

        let mask: CGEventMask =
            (1u64 << K_CG_EVENT_KEY_DOWN) | (1u64 << K_CG_EVENT_FLAGS_CHANGED);

        let tap = CGEventTapCreate(0, 0, 0, mask, Some(event_tap_callback), sender_ptr);

        if tap.is_null() {
            eprintln!(
                "[oh-my-tab] ERROR: Failed to create CGEventTap. \
                 Make sure the app has Accessibility permission."
            );
            let _ = Box::from_raw(sender_ptr as *mut Sender<GlobalEvent>);
            return;
        }

        let source = CFMachPortCreateRunLoopSource(std::ptr::null_mut(), tap, 0);
        CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopDefaultMode);
        CGEventTapEnable(tap, true);

        eprintln!("[oh-my-tab] Event monitor started. Listening for Command+Tab globally.");
        CFRunLoopRun();
    })
}
