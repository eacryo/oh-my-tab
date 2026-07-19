use std::sync::mpsc::Sender;
use std::ffi::c_void;

#[derive(Debug, Clone)]
pub enum KeyEvent {
    CmdDown,
    CmdUp,
    TabDown,
    TabUp,
    ShiftDown,
    ShiftUp,
    EscDown,
}

type CGEventRef = *mut c_void;
type CGEventTapProxy = *mut c_void;
type CFMachPortRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;

const KCG_SESSION_EVENT_TAP: u32 = 1;
const KCG_HEAD_INSERT_EVENT_TAP: u32 = 0;

const EVENT_MASK: u64 = (1 << 10) | (1 << 11) | (1 << 12);
const EVENT_TYPE_FLAGS_CHANGED: u32 = 12;
const EVENT_TYPE_KEY_DOWN: u32 = 10;
const EVENT_TYPE_KEY_UP: u32 = 11;

const CMD_FLAG: u64 = 1 << 20;
const SHIFT_FLAG: u64 = 1 << 17;

const KEY_TAB: u16 = 48;
const KEY_ESC: u16 = 53;

type CGEventTapCallBack = unsafe extern "C" fn(
    proxy: CGEventTapProxy,
    event_type: u32,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: CGEventTapCallBack,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    fn CGEventGetFlags(event: CGEventRef) -> u64;
    fn CGEventGetType(event: CGEventRef) -> u32;
    fn CFMachPortCreateRunLoopSource(
        allocator: *mut c_void,
        port: CFMachPortRef,
        order: isize,
    ) -> CFRunLoopSourceRef;
    fn CFRunLoopAddSource(rl: *mut c_void, source: CFRunLoopSourceRef, mode: *mut c_void);
    fn CFRunLoopGetMain() -> *mut c_void;
}

struct TapContext {
    sender: Sender<KeyEvent>,
    prev_flags: u64,
}

unsafe extern "C" fn tap_callback(
    _proxy: CGEventTapProxy,
    event_type: u32,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef {
    let ctx = &mut *(user_info as *mut TapContext);

    let flags = CGEventGetFlags(event);
    let prev = ctx.prev_flags;
    ctx.prev_flags = flags;

    let cmd_changed = (flags & CMD_FLAG) != (prev & CMD_FLAG);
    let shift_changed = (flags & SHIFT_FLAG) != (prev & SHIFT_FLAG);

    if event_type == EVENT_TYPE_FLAGS_CHANGED {
        if cmd_changed {
            if (flags & CMD_FLAG) != 0 {
                ctx.sender.send(KeyEvent::CmdDown).ok();
            } else {
                ctx.sender.send(KeyEvent::CmdUp).ok();
            }
        }
        if shift_changed {
            if (flags & SHIFT_FLAG) != 0 {
                ctx.sender.send(KeyEvent::ShiftDown).ok();
            } else {
                ctx.sender.send(KeyEvent::ShiftUp).ok();
            }
        }
    }

    if event_type == EVENT_TYPE_KEY_DOWN {
        let keycode = CGEventGetIntegerValueField(event, 9) as u16;
        match keycode {
            KEY_TAB => {
                ctx.sender.send(KeyEvent::TabDown).ok();
                if (flags & CMD_FLAG) != 0 {
                    return std::ptr::null_mut();
                }
            }
            KEY_ESC => {
                ctx.sender.send(KeyEvent::EscDown).ok();
            }
            _ => {}
        }
    }

    if event_type == EVENT_TYPE_KEY_UP {
        let keycode = CGEventGetIntegerValueField(event, 9) as u16;
        if keycode == KEY_TAB && (flags & CMD_FLAG) != 0 {
            return std::ptr::null_mut();
        }
    }

    event
}

pub fn start_event_monitor(
    tx: Sender<KeyEvent>,
) -> Result<*mut c_void, String> {
    let ctx = Box::new(TapContext {
        sender: tx,
        prev_flags: 0,
    });

    let ctx_ptr = Box::into_raw(ctx) as *mut c_void;

    let tap = unsafe {
        CGEventTapCreate(
            KCG_SESSION_EVENT_TAP,
            KCG_HEAD_INSERT_EVENT_TAP,
            0,
            EVENT_MASK,
            tap_callback,
            ctx_ptr,
        )
    };

    if tap.is_null() {
        eprintln!("[event_monitor] Failed to create event tap");
        return Err("Failed to create event tap".into());
    }

    unsafe {
        let main_run_loop = CFRunLoopGetMain();
        let source = CFMachPortCreateRunLoopSource(std::ptr::null_mut(), tap, 0);
        CFRunLoopAddSource(main_run_loop, source, std::ptr::null_mut());
        CGEventTapEnable(tap, true);
    }

    println!("[event_monitor] Event tap registered on main run loop");
    Ok(ctx_ptr)
}
