//! 窗口切换专用的事件监听:CGEventTap 拦截 Cmd/Opt+Tab 全局快捷键。
//! 通用 CGEventTap 基础设施(类型/FFI/启动流程)已抽至 `event_tap.rs`,本模块只保留
//! 键盘快捷键的匹配逻辑与 GlobalEvent 枚举。
//!
//! Window-switcher-specific event monitoring: CGEventTap intercepts the Cmd/Opt+Tab global shortcut.
//! Common CGEventTap infrastructure (types/FFI/start helper) has been extracted to `event_tap.rs`;
//! this module keeps only the keyboard-shortcut matching logic and the GlobalEvent enum.

use crate::event_tap::{self, tap_location, tap_options, tap_placement};
use crate::{log_debug, log_info};
use flume::Sender;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobalEvent {
    CmdTabPressed,
    CmdShiftTabPressed,
    CmdReleased,
    ClipboardToggled,
    // 窗口控制:Option+方向键(方向经 bridge 的 NSNumber 传到主线程)。
    // Window control: Option+arrow (the direction crosses to the main thread via NSNumber
    // in the bridge).
    WindowControl(crate::window_management::Direction),
}

// 窗口切换专用常量 / window-switcher-specific constants
const K_CG_EVENT_KEY_DOWN: crate::event_tap::CGEventType = 10;
const K_CG_EVENT_FLAGS_CHANGED: crate::event_tap::CGEventType = 12;
const K_CG_KEYBOARD_EVENT_AUTOREPEAT: i32 = 8;
const K_CG_KEYBOARD_EVENT_KEYCODE: i32 = 9;
const K_CG_EVENT_FLAG_MASK_COMMAND: crate::event_tap::CGEventFlags = 0x00100000;
const K_CG_EVENT_FLAG_MASK_ALTERNATE: crate::event_tap::CGEventFlags = 0x00080000;
const K_CG_EVENT_FLAG_MASK_CONTROL: crate::event_tap::CGEventFlags = 0x00040000;
const K_CG_EVENT_FLAG_MASK_SHIFT: crate::event_tap::CGEventFlags = 0x00020000;
const K_VK_TAB: u16 = 48;
// 历史剪贴板呼出键:Option+V(V 键码 9)。
// History-clipboard summon key: Option+V (V keycode 9).
const K_VK_V: u16 = 9;

fn switcher_tab_event(flags: crate::event_tap::CGEventFlags) -> GlobalEvent {
    if flags & K_CG_EVENT_FLAG_MASK_SHIFT != 0 {
        GlobalEvent::CmdShiftTabPressed
    } else {
        GlobalEvent::CmdTabPressed
    }
}

/// 只忽略系统对同一次 Tab 按住产生的重复事件;独立的实体按键仍可连续导航。
/// Ignore only the system-generated repeat for one held Tab; separate physical presses still navigate continuously.
fn should_ignore_tab_autorepeat(autorepeat: i64) -> bool {
    autorepeat != 0
}

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

            // 隐私:debug 日志绝不记录用户的按键内容——除 Tab / Command / Option 外的
            // 按键一律只打 "Other"(不记键码、不记修饰位),密码、正文等输入不会泄漏
            // 到日志文件。其余行仍承担原诊断职责:有 keyDown 行 = tap 存活;有召唤行
            // 但没反应 = 下游(bridge/主线程)问题。
            // Privacy: debug logs never record the user's keystrokes -- any key other than
            // Tab / Command / Option is logged as plain "Other" (no keycode, no flags), so
            // passwords and typed text never leak into the log. The remaining lines keep the
            // old diagnostic value: any keyDown line proves the tap is alive; a summon line
            // with no reaction means the issue is downstream (bridge/main thread).
            if keycode == K_VK_TAB {
                let is_cmd = SHORTCUT_IS_CMD.load(Ordering::SeqCst);
                let mod_mask = if is_cmd {
                    K_CG_EVENT_FLAG_MASK_COMMAND
                } else {
                    K_CG_EVENT_FLAG_MASK_ALTERNATE
                };
                if (flags & mod_mask) != 0 {
                    // 窗口切换总开关:关闭时透传给系统(原生 Cmd+Tab 接管),不吞、不发。
                    // 与 Option+V 的 passthrough 同哲学:功能关闭 = 把组合键还给系统/其他应用。
                    // Master switch: when off, pass the event through (the native Cmd+Tab
                    // takes over) -- no swallow, no event. Same philosophy as the Option+V
                    // passthrough: a disabled feature returns the combo to the system.
                    if !crate::config::CONFIG
                        .read()
                        .map(|c| c.windows.enabled)
                        .unwrap_or(true)
                    {
                        log_debug!("[kbd] Tab+Command passthrough (switcher disabled)");
                        return event;
                    }
                    let autorepeat = crate::event_tap::CGEventGetIntegerValueField(
                        event,
                        K_CG_KEYBOARD_EVENT_AUTOREPEAT,
                    );
                    if should_ignore_tab_autorepeat(autorepeat) {
                        // 重复事件必须吞掉而不是透传,否则系统原生 Cmd+Tab 会与本应用同时响应。
                        // Repeat events must be swallowed rather than passed through, or the system's native Cmd+Tab responds alongside us.
                        log_debug!("[kbd] summon autorepeat ignored");
                        return std::ptr::null_mut();
                    }
                    // 召唤组合:只打组合名(不敏感),不打键码/修饰位细节。
                    // The summon combo: log only the combo name (not sensitive), never raw keycode/flags.
                    let combo = if is_cmd { "Tab+Command" } else { "Tab+Option" };
                    log_debug!("[kbd] summon keyDown {}", combo);
                    TAB_PRESSED.store(true, Ordering::SeqCst);
                    let _ = sender.send(switcher_tab_event(flags));
                    return std::ptr::null_mut();
                }
            } else if keycode == K_VK_V && (flags & K_CG_EVENT_FLAG_MASK_ALTERNATE) != 0 {
                // 历史剪贴板呼出:Option+V(始终走 Option 修饰,不随快捷键模式切换)。
                // 吞掉事件,与 Win+V 行为一致。只打组合名(隐私约定)。
                // History-clipboard summon: Option+V (always Option, independent of the
                // shortcut mode). The event is swallowed, mirroring Win+V. Only the combo
                // name is logged (privacy convention).
                //
                // 必须精确匹配:flags 是当前所有按下的修饰键的位掩码,只要"含 Option"
                // 就把 Cmd+Option+V(粘贴并匹配样式)等组合误认为呼出并吞掉,系统
                // 快捷键随之失效。带上其他修饰键的组合一律透传。
                // Precise match is required: flags is the bitmask of ALL currently held
                // modifiers, so a bare "contains Option" check would swallow combos like
                // Cmd+Option+V (paste-and-match-style) and break the system shortcut.
                // Combos carrying any other modifier pass through untouched.
                let other_mods = flags
                    & (K_CG_EVENT_FLAG_MASK_COMMAND
                        | K_CG_EVENT_FLAG_MASK_SHIFT
                        | K_CG_EVENT_FLAG_MASK_CONTROL);
                if other_mods != 0 {
                    // 带其他修饰键(如 Cmd+Option+V):透传,让系统/应用处理。
                    // Combos with extra modifiers (e.g. Cmd+Option+V) pass through.
                    log_debug!("[kbd] Option+V passthrough (extra modifiers)");
                } else if !crate::config::CONFIG.read().unwrap().clipboard.enabled {
                    // 功能关闭时不拦截:其他应用可能需要 Option+V 组合键,必须透传。
                    // When the feature is disabled, do NOT swallow Option+V -- other apps may
                    // need the combo, so it passes through untouched.
                    log_debug!("[kbd] Option+V passthrough (clipboard disabled)");
                } else {
                    log_debug!("[kbd] summon keyDown V+Option (clipboard)");
                    let _ = sender.send(GlobalEvent::ClipboardToggled);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_tab_emits_the_backward_switcher_event() {
        assert_eq!(switcher_tab_event(0), GlobalEvent::CmdTabPressed);
        assert_eq!(
            switcher_tab_event(K_CG_EVENT_FLAG_MASK_SHIFT),
            GlobalEvent::CmdShiftTabPressed
        );
        assert_eq!(
            switcher_tab_event(K_CG_EVENT_FLAG_MASK_SHIFT | K_CG_EVENT_FLAG_MASK_COMMAND),
            GlobalEvent::CmdShiftTabPressed
        );
    }

    #[test]
    fn only_autorepeat_tab_events_are_ignored() {
        assert!(!should_ignore_tab_autorepeat(0));
        assert!(should_ignore_tab_autorepeat(1));
    }
}
