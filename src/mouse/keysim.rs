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

use crate::event_monitor::{GlobalEvent, SHORTCUT_IS_CMD};
use crate::event_tap::{
    tap_location, CGEventCreateKeyboardEvent, CGEventFlags, CGEventPost, CGEventSetFlags,
    CGEventSetIntegerValueField, CGEventSetType, K_CG_EVENT_SOURCE_USER_DATA, SYNTHETIC_MARKER,
};
use crate::mouse::shortcut::{FLAG_ALT, FLAG_CMD, FLAG_CTRL, FLAG_SHIFT};
use crate::{log_debug, STATUS_EVENT_TX};
use std::sync::atomic::Ordering;

/// 侧键按下:合成目标组合的 keyDown(修饰位附着),post 到 HID 层。
/// 回环到我们的切换 tap → CmdTabPressed / ClipboardToggled 等既有检测生效。
///
/// Side button pressed: post the mapped keyDown (modifiers attached) to the HID level.
/// It loops back through our switcher tap, where the existing CmdTabPressed /
/// ClipboardToggled detection kicks in.
pub(crate) fn press_down(keycode: u16, flags: u32, desc: &str) {
    // 命中我们自己的全局快捷键(切换器/剪贴板)→ 内部派发,不合成事件:
    // session 层合成无法回环到我们的 tap,而 HID 层合成的键盘事件会被系统丢弃
    // (无 IOHIDEvent,实测收不到)。
    // Binding our own global shortcuts (switcher/clipboard) dispatches internally instead
    // of synthesizing: session-level posts can't loop back to our tap, and HID-level
    // keyboard posts are dropped by the system (no IOHIDEvent, verified).
    if internal_dispatch(keycode, flags, true) {
        return;
    }
    // 完整按键序列(LinearMouse postKeyLocked 同款):先按修饰键的真实 flagsChanged
    // (flags 逐步累积),再按主键。系统级快捷键(如 Ctrl+↑ = Mission Control)要求看到
    // 真实的修饰键状态变化,单纯在 keyDown 上附着 flags 不认。
    // Full key sequence (same as LinearMouse's postKeyLocked): first the modifiers' real
    // flagsChanged presses (flags accumulating step by step), then the main keyDown.
    // System-level shortcuts (e.g. Ctrl+Up = Mission Control) require the real modifier
    // state transitions; flags attached to a bare keyDown are not recognized.
    let mut acc: u32 = 0;
    for (bit, vk) in MOD_KEYS {
        if flags & bit != 0 {
            acc |= bit;
            unsafe {
                post_modifier_change(vk, acc);
            }
        }
    }
    unsafe {
        post_key(keycode, flags, true);
    }
    log_debug!(
        "[mouse] button -> key down '{}' (keycode={} flags=0x{:x}) via session tap",
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
    // 内部派发的组合:切换器在松开时提交切换,剪贴板无需松开动作。
    // Internally dispatched combos: the switcher commits on release; the clipboard needs
    // no release action.
    if internal_dispatch(keycode, flags, false) {
        return;
    }
    // 主键 keyUp,然后修饰键按倒序释放(flags 逐步递减,最后归零)——与按下序列配对。
    // Main keyUp, then the modifiers release in reverse order (flags winding down to zero),
    // pairing with the press sequence.
    unsafe {
        post_key(keycode, flags, false);
    }
    let mut acc = flags;
    for (bit, vk) in MOD_KEYS.iter().rev() {
        if flags & bit != 0 {
            acc &= !bit;
            unsafe {
                post_modifier_change(*vk, acc);
            }
        }
    }
    log_debug!(
        "[mouse] button -> key up '{}' (keycode={} flags=0x{:x}) via session tap",
        desc,
        keycode,
        flags
    );
}

/// 检测目标组合是否命中我们自己的全局快捷键,命中则走内部通道发送 GlobalEvent。
/// 返回 true 表示已内部派发(调用方不应再合成事件)。
///
/// Detect whether the target combo is one of our own global shortcuts; if so, send the
/// GlobalEvent through the internal channel. Returns true when dispatched internally (the
/// caller must not synthesize).
fn internal_dispatch(keycode: u16, flags: u32, down: bool) -> bool {
    // 切换器当前组合:Tab + 当前模式修饰位(绑定如 "cmd+tab" / "alt+tab",不夹带其他修饰)。
    // The switcher's current combo: Tab + the active mode's modifier (a binding like
    // "cmd+tab" / "alt+tab", without extra modifiers).
    let sw_mod = if SHORTCUT_IS_CMD.load(Ordering::SeqCst) {
        FLAG_CMD
    } else {
        FLAG_ALT
    };
    if keycode == 48 && flags == sw_mod {
        if let Some(tx) = STATUS_EVENT_TX.get() {
            if down {
                let _ = tx.send(GlobalEvent::CmdTabPressed);
            } else {
                // 切换器在松开时提交切换(与键盘释放修饰键的语义一致)。
                // The switcher commits on release (same semantics as releasing the modifier).
                let _ = tx.send(GlobalEvent::CmdReleased);
            }
            return true;
        }
    }
    // 剪贴板呼出:Option+V(仅当功能启用;按下即 toggle,松开无动作)。
    // Clipboard summon: Option+V (only while enabled; toggles on press, nothing on release).
    if keycode == 9 && flags == FLAG_ALT && down {
        let enabled = crate::config::CONFIG
            .read()
            .map(|c| c.clipboard.enabled)
            .unwrap_or(false);
        if enabled {
            if let Some(tx) = STATUS_EVENT_TX.get() {
                let _ = tx.send(GlobalEvent::ClipboardToggled);
                return true;
            }
        }
    }
    false
}

/// 修饰键的 (位, 键码),固定顺序:cmd → alt → ctrl → shift。
/// Modifier (bit, keycode) pairs, fixed order: cmd → alt → ctrl → shift.
const MOD_KEYS: [(u32, u16); 4] = [
    (FLAG_CMD, 55),
    (FLAG_ALT, 58),
    (FLAG_CTRL, 59),
    (FLAG_SHIFT, 56),
];

/// 合成一个修饰键的 flagsChanged 事件(flags = 该时刻的累积修饰状态),post 到 session 层。
/// Synthesize a modifier flagsChanged event (flags = the accumulated modifier state at that
/// moment), posted to the session level.
unsafe fn post_modifier_change(vk: u16, flags: u32) {
    let ev = CGEventCreateKeyboardEvent(std::ptr::null(), vk, false);
    if ev.is_null() {
        return;
    }
    CGEventSetType(ev, 12); // kCGEventFlagsChanged
    CGEventSetFlags(ev, flags as CGEventFlags);
    CGEventSetIntegerValueField(ev, K_CG_EVENT_SOURCE_USER_DATA, SYNTHETIC_MARKER);
    CGEventPost(tap_location::SESSION_EVENT_TAP, ev);
}

/// 合成单个键盘事件(keyDown/true 或 keyUp/false),打 userData 标记,post 到 session 层。
/// (session 层 = 剪贴板粘贴验证过的路径;HID 层 post 的键盘事件会被系统丢弃。)
/// Synthesize a single keyboard event (keyDown/true or keyUp/false), tagged with userData,
/// posted to the session level. (The session level is the clipboard-paste-proven path;
/// HID-level keyboard posts get dropped by the system.)
/// Synthesize a single keyboard event (keyDown/true or keyUp/false), tagged with userData,
/// and post it to the HID level.
unsafe fn post_key(keycode: u16, flags: u32, key_down: bool) {
    let ev = CGEventCreateKeyboardEvent(std::ptr::null(), keycode, key_down);
    if ev.is_null() {
        return;
    }
    CGEventSetFlags(ev, flags as CGEventFlags);
    CGEventSetIntegerValueField(ev, K_CG_EVENT_SOURCE_USER_DATA, SYNTHETIC_MARKER);
    CGEventPost(tap_location::SESSION_EVENT_TAP, ev);
}
