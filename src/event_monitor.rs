use crate::ffi::has_accessibility_permission;
use crate::{log_error, log_info, log_warn};
use flume::Sender;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

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
const K_CG_EVENT_FLAG_MASK_ALTERNATE: CGEventFlags = 0x00080000;
const K_VK_TAB: u16 = 48;

#[link(name = "CoreGraphics", kind = "framework")]
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

    static kCFRunLoopDefaultMode: CFStringRef;
}

// 标记是否已经发送过 CmdTabPressed，防止修饰键变化时误发 CmdReleased
// Tracks whether CmdTabPressed was sent, to avoid spurious CmdReleased
static TAB_PRESSED: AtomicBool = AtomicBool::new(false);

// 当前快捷键模式：true = Command+Tab, false = Option+Tab
// Shortcut mode: true = Command+Tab, false = Option+Tab
pub static SHORTCUT_IS_CMD: AtomicBool = AtomicBool::new(false);

type CGEventTapCallBack = Option<
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
            let keycode = CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_KEYCODE) as u16;
            let flags = CGEventGetFlags(event);

            if keycode == K_VK_TAB {
                let mod_mask = if SHORTCUT_IS_CMD.load(Ordering::SeqCst) {
                    K_CG_EVENT_FLAG_MASK_COMMAND
                } else {
                    K_CG_EVENT_FLAG_MASK_ALTERNATE
                };
                if (flags & mod_mask) != 0 {
                    TAB_PRESSED.store(true, Ordering::SeqCst);
                    let _ = sender.send(GlobalEvent::CmdTabPressed);
                    return std::ptr::null_mut();
                }
            }
        }
        K_CG_EVENT_FLAGS_CHANGED => {
            let flags = CGEventGetFlags(event);
            let mod_mask = if SHORTCUT_IS_CMD.load(Ordering::SeqCst) {
                K_CG_EVENT_FLAG_MASK_COMMAND
            } else {
                K_CG_EVENT_FLAG_MASK_ALTERNATE
            };
            if (flags & mod_mask) == 0 && TAB_PRESSED.swap(false, Ordering::SeqCst) {
                let _ = sender.send(GlobalEvent::CmdReleased);
            }
        }
        _ => {}
    }

    event
}

// 缺 Accessibility 权限时,event tap 创建会失败。每隔 RETRY_INTERVAL 重试一次,最多 RETRY_MAX 次
// (约 2 分钟),期间用户可在系统设置里授权;超过上限就记日志放弃(快捷键在重启前失效,下次启动再试)。
// 设上限是为了避免无限轮询;用户授权后下次重试即建成,无需重启。
// When Accessibility permission is missing, CGEventTapCreate fails. Retry every RETRY_INTERVAL up to
// RETRY_MAX times (~2 min), during which the user can grant permission in System Settings; once the
// limit is exhausted, log and give up (the shortcut stays dead until restart; next launch retries).
// The cap avoids infinite polling; once granted, the next retry succeeds - no restart needed.
const RETRY_INTERVAL: Duration = Duration::from_secs(3);
const RETRY_MAX: u32 = 40;

pub fn start(sender: Sender<GlobalEvent>) -> thread::JoinHandle<()> {
    thread::spawn(move || unsafe {
        let sender_ptr = Box::into_raw(Box::new(sender)) as *mut c_void;

        let mask: CGEventMask = (1u64 << K_CG_EVENT_KEY_DOWN) | (1u64 << K_CG_EVENT_FLAGS_CHANGED);

        let mut tap = CGEventTapCreate(0, 0, 0, mask, Some(event_tap_callback), sender_ptr);

        // 首次创建失败(通常是缺 Accessibility 权限):有限次重试,给用户时间去系统设置授权。
        // First creation failed (usually missing Accessibility): retry a bounded number of times
        // to give the user time to grant permission in System Settings.
        if tap.is_null() {
            log_warn!(
                "No Accessibility permission yet; event tap will retry every {:?} up to {} times (~{}s).",
                RETRY_INTERVAL,
                RETRY_MAX,
                RETRY_INTERVAL.as_secs() * RETRY_MAX as u64
            );
            let mut granted = false;
            for _ in 0..RETRY_MAX {
                std::thread::sleep(RETRY_INTERVAL);
                if has_accessibility_permission() {
                    tap = CGEventTapCreate(0, 0, 0, mask, Some(event_tap_callback), sender_ptr);
                    if !tap.is_null() {
                        granted = true;
                        break;
                    }
                }
            }
            if granted {
                log_info!("Accessibility permission granted; event tap created.");
            } else {
                log_error!(
                    "Event tap retry exhausted ({}x). Shortcut disabled until restart. \
                     Grant Accessibility in System Settings and relaunch.",
                    RETRY_MAX
                );
                let _ = Box::from_raw(sender_ptr as *mut Sender<GlobalEvent>);
                return;
            }
        }

        let source = CFMachPortCreateRunLoopSource(std::ptr::null_mut(), tap, 0);
        CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopDefaultMode);
        CGEventTapEnable(tap, true);

        // 快捷键可能被菜单/设置切换,按当前 SHORTCUT_IS_CMD 打印实际监听的组合键。
        // The shortcut can be toggled via menu/settings; print the actual combo from SHORTCUT_IS_CMD.
        let shortcut = if SHORTCUT_IS_CMD.load(Ordering::SeqCst) {
            "Command+Tab"
        } else {
            "Option+Tab"
        };
        log_info!(
            "Event monitor started. Listening for {} globally.",
            shortcut
        );
        CFRunLoopRun();
    })
}
