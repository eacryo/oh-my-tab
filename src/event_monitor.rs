//! 窗口切换专用的事件监听:CGEventTap 拦截 Cmd/Opt+Tab 全局快捷键。
//! 通用 CGEventTap 基础设施(类型/FFI/启动流程)已抽至 `event_tap.rs`,本模块只保留
//! 键盘快捷键的匹配逻辑与 GlobalEvent 枚举。
//!
//! Window-switcher-specific event monitoring: CGEventTap intercepts the Cmd/Opt+Tab global shortcut.
//! Common CGEventTap infrastructure (types/FFI/start helper) has been extracted to `event_tap.rs`;
//! this module keeps only the keyboard-shortcut matching logic and the GlobalEvent enum.

use crate::event_tap::{self, tap_location, tap_options, tap_placement};
use crate::log_info;
use flume::Sender;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Copy)]
pub enum GlobalEvent {
    CmdTabPressed,
    CmdReleased,
    ThemeToggled,
}

// 窗口切换专用常量 / window-switcher-specific constants
const K_CG_EVENT_KEY_DOWN: crate::event_tap::CGEventType = 10;
const K_CG_EVENT_FLAGS_CHANGED: crate::event_tap::CGEventType = 12;
const K_CG_KEYBOARD_EVENT_KEYCODE: i32 = 9;
const K_CG_EVENT_FLAG_MASK_COMMAND: crate::event_tap::CGEventFlags = 0x00100000;
const K_CG_EVENT_FLAG_MASK_ALTERNATE: crate::event_tap::CGEventFlags = 0x00080000;
const K_VK_TAB: u16 = 48;

// 标记是否已经发送过 CmdTabPressed，防止修饰键变化时误发 CmdReleased
// Tracks whether CmdTabPressed was sent, to avoid spurious CmdReleased
static TAB_PRESSED: AtomicBool = AtomicBool::new(false);

// 当前快捷键模式：true = Command+Tab, false = Option+Tab
// Shortcut mode: true = Command+Tab, false = Option+Tab
pub static SHORTCUT_IS_CMD: AtomicBool = AtomicBool::new(false);

unsafe extern "C" fn event_tap_callback(
    _proxy: crate::event_tap::CGEventTapProxy,
    event_type: crate::event_tap::CGEventType,
    event: crate::event_tap::CGEventRef,
    user_info: *mut c_void,
) -> crate::event_tap::CGEventRef {
    let sender = &*(user_info as *const Sender<GlobalEvent>);

    match event_type {
        K_CG_EVENT_KEY_DOWN => {
            let keycode =
                crate::event_tap::CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_KEYCODE)
                    as u16;
            let flags = crate::event_tap::CGEventGetFlags(event);

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
            let flags = crate::event_tap::CGEventGetFlags(event);
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

pub fn start(sender: Sender<GlobalEvent>) -> std::thread::JoinHandle<()> {
    let sender_ptr = Box::into_raw(Box::new(sender)) as *mut c_void;

    let mask: crate::event_tap::CGEventMask =
        (1u64 << K_CG_EVENT_KEY_DOWN) | (1u64 << K_CG_EVENT_FLAGS_CHANGED);

    // 窗口切换 tap 建在 session 层:既能看到真实硬件事件,也能看到鼠标映射软件在 session 层
    // 合成的 Cmd+Tab(HID 层 tap 看不到 session 层注入的合成事件,会导致侧键映射的 Cmd+Tab 漏过)。
    // options 传 DEFAULT_TAP:需要能吞掉 Cmd+Tab 事件(返回 null),所以必须可改。
    //
    // The switcher tap sits at the session level: sees real hardware events AND session-synthesized
    // Cmd+Tab from mouse-remapper software (a HID-level tap can't see session-posted synthetic events,
    // so a side-button-mapped Cmd+Tab would slip past). options = DEFAULT_TAP: must be able to swallow
    // the Cmd+Tab event (return null), so a mutable tap is required.
    event_tap::start_event_tap_thread(
        tap_location::SESSION_EVENT_TAP,
        tap_placement::HEAD_INSERT,
        tap_options::DEFAULT_TAP,
        mask,
        Some(event_tap_callback),
        sender_ptr as usize,
        "kbd",
        || {
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
        },
    )
}
