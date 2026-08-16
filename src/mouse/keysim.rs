//! 键盘模拟器:把按钮映射的快捷键合成成键盘事件,post 到 HID 层。
//! post 到 HID 层(kCGHIDEventTap)的关键作用:事件会重新进入 HID 事件流,
//! **回到我们自己的 event tap**(窗口切换 tap / 鼠标 tap),因此:
//!   - 绑定 Cmd+Tab → 我们的浮窗打开(不是系统原生切换器);
//!   - 绑定 Option+V → 剪贴板面板;
//!   - 绑定普通快捷键(Cmd+C)→ 经过 tap 无匹配分支,透传给前台 app。
//!
//! 参考 LinearMouse 的 KeySimulator(Modules/KeyKit):修饰键以 flags 形式附着在
//! 每个事件上,不合成修饰键自身的按下/释放;合成事件打 userData 标记防回环误判。
//!
//! 两段式语义(按下/释放)是必须的:切换器靠「修饰键释放」提交切换——侧键按住时
//! 合成 keyDown(带修饰),浮窗打开;侧键释放时合成 keyUp + 修饰键 flagsChanged,
//! 我们自己的 tap 检测到修饰键松开 → 提交切换。
//!
//! Key simulator: synthesizes the mapped shortcut as keyboard events posted to the HID level.
//! Posting to the HID level re-injects the event into the HID stream, where **our own event
//! taps see it again**, so:
//!   - binding Cmd+Tab opens OUR overlay (not the system switcher);
//!   - binding Option+V opens the clipboard picker;
//!   - binding an ordinary shortcut (Cmd+C) passes through the taps to the frontmost app.
//!
//! Modeled on LinearMouse's KeySimulator: modifiers ride on each event as flags; synthetic
//! events carry a userData marker so the tap can recognize its own output.
//!
//! Two-phase down/up semantics are required: the switcher commits on modifier RELEASE -- while
//! the side button is held, a keyDown (with modifiers) is posted and the overlay opens; on
//! release, a keyUp + modifier flagsChanged is posted so our tap sees the modifier go up and
//! commits the switch.

use crate::event_tap::{
    tap_location, CGEventCreateKeyboardEvent, CGEventFlags, CGEventPost, CGEventSetFlags,
    CGEventSetIntegerValueField, CGEventSetType, K_CG_EVENT_SOURCE_USER_DATA, SYNTHETIC_MARKER,
};
use crate::log_debug;

/// flagsChanged 事件类型(kCGEventFlagsChanged)。
/// The flagsChanged event type (kCGEventFlagsChanged).
const K_CG_EVENT_FLAGS_CHANGED: i32 = 12;

/// 修饰键的虚拟键码(合成 flagsChanged 时用,事件类型本身不带键码语义,任选其一即可)。
/// Modifier key virtual keycodes (used when synthesizing flagsChanged; the event type carries
/// no keycode semantics, so any of them works).
const VK_CMD: u16 = 55;
const VK_ALT: u16 = 58;
const VK_CTRL: u16 = 59;
const VK_SHIFT: u16 = 56;

/// 侧键按下:合成目标组合的 keyDown(修饰位附着),post 到 HID 层。
/// 回环到我们的切换 tap → CmdTabPressed / ClipboardToggled 等既有检测生效。
///
/// Side button pressed: post the mapped keyDown (modifiers attached) to the HID level.
/// It loops back through our switcher tap, where the existing CmdTabPressed /
/// ClipboardToggled detection kicks in.
pub(crate) fn press_down(keycode: u16, flags: u32, desc: &str) {
    unsafe {
        post_key(keycode, flags, true);
    }
    log_debug!(
        "[mouse] button -> key down '{}' (keycode={} flags=0x{:x}) via HID tap",
        desc,
        keycode,
        flags
    );
}

/// 侧键释放:合成 keyUp + 修饰键 flagsChanged(修饰全松开),post 到 HID 层。
/// 回环到我们的切换 tap:flagsChanged 检测修饰键不在 → CmdReleased → 提交切换。
///
/// Side button released: post keyUp + a modifier flagsChanged (all modifiers up) to the HID
/// level. It loops back to the switcher tap, whose flagsChanged handler sees the modifier go
/// up and fires CmdReleased to commit the switch.
pub(crate) fn release_up(keycode: u16, flags: u32, desc: &str) {
    unsafe {
        post_key(keycode, flags, false);
        post_modifier_release(flags);
    }
    log_debug!(
        "[mouse] button -> key up '{}' (keycode={} flags=0x{:x}) via HID tap",
        desc,
        keycode,
        flags
    );
}

/// 合成单个键盘事件(keyDown/true 或 keyUp/false),打 userData 标记,post 到 HID 层。
/// Synthesize a single keyboard event (keyDown/true or keyUp/false), tagged with userData,
/// and post it to the HID level.
unsafe fn post_key(keycode: u16, flags: u32, key_down: bool) {
    let ev = CGEventCreateKeyboardEvent(std::ptr::null(), keycode, key_down);
    if ev.is_null() {
        return;
    }
    CGEventSetFlags(ev, flags as CGEventFlags);
    CGEventSetIntegerValueField(ev, K_CG_EVENT_SOURCE_USER_DATA, SYNTHETIC_MARKER);
    CGEventPost(tap_location::HID_EVENT_TAP, ev);
}

/// 合成一次「修饰键全部释放」的 flagsChanged 事件。
/// 用 flags 里的第一个修饰键作键码(类型为 flagsChanged,键码无语义),flags 置 0。
///
/// Synthesize one "all modifiers released" flagsChanged event. Uses the first modifier in
/// `flags` as the keycode (the type is flagsChanged, so the keycode carries no meaning);
/// flags is zeroed.
unsafe fn post_modifier_release(flags: u32) {
    let vk = if flags & crate::mouse::shortcut::FLAG_CMD != 0 {
        VK_CMD
    } else if flags & crate::mouse::shortcut::FLAG_ALT != 0 {
        VK_ALT
    } else if flags & crate::mouse::shortcut::FLAG_CTRL != 0 {
        VK_CTRL
    } else if flags & crate::mouse::shortcut::FLAG_SHIFT != 0 {
        VK_SHIFT
    } else {
        return; // 无修饰键:无需 flagsChanged
    };
    let ev = CGEventCreateKeyboardEvent(std::ptr::null(), vk, false);
    if ev.is_null() {
        return;
    }
    CGEventSetType(ev, K_CG_EVENT_FLAGS_CHANGED as u32);
    CGEventSetFlags(ev, 0);
    CGEventSetIntegerValueField(ev, K_CG_EVENT_SOURCE_USER_DATA, SYNTHETIC_MARKER);
    CGEventPost(tap_location::HID_EVENT_TAP, ev);
}
