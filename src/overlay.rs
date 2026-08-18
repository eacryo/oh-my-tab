//! 切换器浮窗与卡片 UI:浮窗/容器/状态栏的 static、卡片↔索引映射、键盘/鼠标回调,
//! 以及浮窗的显示/隐藏/刷新/卡片构建/主题应用等渲染逻辑。activate_and_raise 负责
//! 激活 App 并抬起目标窗口。KEY_* 为键盘导航键码。
//!
//! Switcher overlay & card UI: statics for the overlay/container/status bar, the card<->index
//! map, keyboard/mouse callbacks, and the overlay's show/hide/refresh/card-build/theme-apply
//! rendering. activate_and_raise activates the app and raises the target window. KEY_* are
//! keyboard-navigation key codes.

use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::ffi::CString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::Instant; // TIMING-DEBUG

use crate::config::{self, CONFIG};
use crate::event_tap;
use crate::ffi::*;
use crate::i18n::t;
use crate::theme::*;
use crate::window_collector::{
    bump_window_mru, extract_icon_to_cache, raise_ax_window, WindowInfo,
};
// 跨模块共享状态(由 main.rs 持有,这里读写)/ cross-module shared state (owned by main.rs)
use crate::TAB_STATE;
use crate::{log_debug, log_info};

// ========== 键盘键码 / keyboard key codes ==========

pub(crate) const KEY_TAB: u16 = 48;
pub(crate) const KEY_LEFT: u16 = 123;
pub(crate) const KEY_RIGHT: u16 = 124;
pub(crate) const KEY_DOWN: u16 = 125;
pub(crate) const KEY_UP: u16 = 126;
pub(crate) const KEY_ESCAPE: u16 = 53;
pub(crate) const KEY_RETURN: u16 = 36;
pub(crate) const KEY_DELETE: u16 = 51; // Backspace
/// 卡片右上角关闭按钮的 tag(hover 显隐查找用;卡片 index 不存 tag)。
/// The close-button tag on a card (used to find it for hover show/hide; the card
/// index is NOT stored in the tag).
pub(crate) const CLOSE_BTN_TAG: isize = 0xE7F1;
/// 选中态位移用的图标视图 tag,避免依赖动态 ObjC 类的属性访问。
/// Tag used to find the icon view for the selected-state nudge without relying on
/// property accessors on the dynamically registered ObjC card class.
pub(crate) const ICON_VIEW_TAG: isize = 0xE7F2;

// ========== 浮窗相关全局状态 / overlay global state ==========

pub(crate) static OVERLAY_WINDOW: Mutex<Option<ObjPtr>> = Mutex::new(None);
pub(crate) static CONTAINER: Mutex<Option<ObjPtr>> = Mutex::new(None);
pub(crate) static STATUS_LABEL: Mutex<Option<ObjPtr>> = Mutex::new(None);
/// macOS 26+ 的 NSGlassEffectView 指针(用于设置热重载时重新应用玻璃属性)。
/// Pointer to the NSGlassEffectView on macOS 26+ (used to re-apply glass properties on hot reload).
pub(crate) static GLASS_VIEW: Mutex<Option<ObjPtr>> = Mutex::new(None);
pub(crate) static CARD_CLASS: Mutex<Option<ObjClassPtr>> = Mutex::new(None);
/// Maps card view pointer (as usize) -> card index, avoiding property accessor
/// msg_send! issues on dynamically-registered ObjC classes.
pub(crate) static CARD_INDEX_MAP: LazyLock<Mutex<HashMap<usize, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// Prevents hover-selection on the card under the cursor when the window first
/// opens. Set to false in show_overlay(), flipped to true on first mouseMoved:.
pub(crate) static MOUSE_MOVED: AtomicBool = AtomicBool::new(false);

// ========== 卡片 ↔ 索引映射 / card <-> index map ==========

/// Read the card index from the card index map (keyed by view pointer).
/// This avoids msg_send! encoding issues with property accessors on
/// dynamically-registered ObjC classes.
pub(crate) fn get_card_index(view: *mut AnyObject) -> usize {
    let map = CARD_INDEX_MAP.lock().unwrap();
    map.get(&(view as usize)).copied().unwrap_or(0)
}

pub(crate) fn set_card_index(view: *mut AnyObject, idx: usize) {
    let mut map = CARD_INDEX_MAP.lock().unwrap();
    map.insert(view as usize, idx);
}

pub(crate) fn remove_card_index(view: *mut AnyObject) {
    let mut map = CARD_INDEX_MAP.lock().unwrap();
    map.remove(&(view as usize));
}

pub(crate) fn clear_card_indices() {
    let mut map = CARD_INDEX_MAP.lock().unwrap();
    map.clear();
}

// ========== 文本 helper / text helpers ==========

/// 截断文本到指定显示宽度(ASCII 计 1、其余计 2),超出加省略号。
/// Truncate text to a display width (ASCII=1, others=2), appending an ellipsis if exceeded.
fn truncate_text(text: &str, max_width: usize) -> String {
    let mut width: usize = 0;
    for (i, c) in text.char_indices() {
        let w = if c.is_ascii() { 1 } else { 2 };
        if width + w > max_width {
            let t: String = text[..i].chars().collect();
            return format!("{}…", t);
        }
        width += w;
    }
    text.to_string()
}

/// 占位符:窗口没有标题时(如 Microsoft To Do,AXTitle 为空)显示一个短横线。
/// 注意:仅用于显示。内部 `window_title` 仍保持空串,这样 raise_ax_window 仍能
/// 按空标题匹配到对应的 AX 窗口并聚焦。
/// Placeholder shown for windows that expose no title (e.g. Microsoft To Do,
/// whose custom title bar yields an empty AXTitle). Display-only: the internal
/// `window_title` stays empty so raise_ax_window can still match the AX window
/// by its empty title.
fn display_title(title: &str) -> String {
    if title.is_empty() {
        "-".to_string()
    } else {
        title.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::display_title;

    #[test]
    fn empty_title_gets_placeholder() {
        // 空标题只影响显示层;内部 title 不动(见函数注释,raise_ax_window 靠空标题匹配)。
        // Empty titles only affect display; the stored title is untouched (see the fn doc:
        // raise_ax_window matches by the empty title).
        assert_eq!(display_title(""), "-");
        assert_eq!(display_title("   "), "   "); // 空白串不是空串 / whitespace is not empty
    }

    #[test]
    fn remove_window_adjust_selection_keeps_a_sane_selection() {
        use super::remove_window_adjust_selection;
        // 关的是选中项之后 → 选中不动。
        // Closing something after the selection leaves it.
        assert_eq!(remove_window_adjust_selection(1, 3, 4), 1);
        // 关的是选中项之前 → 前移一格(保持指向同一张窗口)。
        // Closing something before it shifts back one (same window stays selected).
        assert_eq!(remove_window_adjust_selection(3, 1, 4), 2);
        // 关的正是选中项 → 指向下一张(原位置就是新列表的同位)。
        // Closing the selection itself -> the next window (the same slot).
        assert_eq!(remove_window_adjust_selection(1, 1, 4), 1);
        // 关的是末张且选中末张 → 钳到新末张。
        // Closing the tail while it is selected -> clamps to the new tail.
        assert_eq!(remove_window_adjust_selection(4, 4, 4), 3);
        // 空列表 → 0。
        assert_eq!(remove_window_adjust_selection(0, 0, 0), 0);
    }

    #[test]
    fn non_empty_title_passes_through() {
        assert_eq!(display_title("Safari — Apple"), "Safari — Apple");
        assert_eq!(display_title("x"), "x");
    }
}

// ========== 通用控件 helper / generic control helper ==========

/// 创建一个简单(非 attributed)NSTextField 标签,sizeToFit 后在 container_width 内水平居中。
/// 被 create_card_view 与 main 的 create_overlay_window(状态栏)共用。
/// Create a simple (non-attributed) NSTextField label, size it to fit text,
/// then center it horizontally within `container_width`. Shared by create_card_view
/// and main's create_overlay_window (status bar).
pub(crate) unsafe fn make_centered_label(
    text: &str,
    font: *mut AnyObject,
    color: *mut AnyObject,
    y: f64,
    container_width: f64,
    height: f64,
) -> *mut AnyObject {
    let ns_str = make_nsstring(text);
    // Create with a wide enough frame
    let init_frame = NSRect::new(NSPoint::new(0.0, y), NSSize::new(container_width, height));
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let label: *mut AnyObject = msg_send![label, initWithFrame: init_frame];
    let _: () = msg_send![label, setStringValue: ns_str];
    CFRelease(ns_str as *const c_void);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let _: () = msg_send![label, setSelectable: false];
    let _: () = msg_send![label, setFont: font];
    let _: () = msg_send![label, setTextColor: color];
    // Size to fit content, then center horizontally
    let _: () = msg_send![label, sizeToFit];
    let fitted: NSRect = msg_send![label, frame];
    let text_w = fitted.size.width;
    let center_x = ((container_width - text_w) / 2.0).max(0.0);
    let _: () = msg_send![label, setFrame: NSRect::new(NSPoint::new(center_x, y), NSSize::new(text_w, height))];
    label
}

// ========== ObjC 回调实现 / ObjC callback implementations ==========

pub(crate) extern "C" fn on_cmd_tab_pressed(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    // TIMING-DEBUG 端到端计时:tap 回调 → collect → show_overlay(定位卡顿段)。
    let t_end = Instant::now();
    let mut state_opt = TAB_STATE.lock().unwrap();
    let state = state_opt.as_mut().unwrap();

    if !state.visible {
        state.refresh();
        state.visible = true;
        state.selected = if state.windows.len() > 1 { 1 } else { 0 };
        drop(state_opt);
        show_overlay();
        // TIMING-DEBUG 端到端:tap 回调 → collect_windows → show_overlay 完成。
        log_debug!("[overlay] summon e2e={}ms", t_end.elapsed().as_millis());
    } else {
        state.selected = (state.selected + 1) % state.windows.len().max(1);
        drop(state_opt);
        refresh_highlight();
        update_status_label();
        extract_uncached_icons();
    }
}

/// 卡片右上角关闭按钮的 action(sender = 关闭按钮):取按钮所在卡片(superview)
/// 的 index,关闭该窗口。浮窗保持打开。
/// Action of the card's top-right close button (sender = the button): resolve the
/// card via the button's superview, close that window. The overlay stays open.
pub(crate) extern "C" fn on_close_card(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    let card: *mut AnyObject = unsafe { msg_send![sender as *mut AnyObject, superview] };
    if card.is_null() {
        return;
    }
    let idx = get_card_index(card);
    close_window_at(idx);
}

pub(crate) extern "C" fn on_cmd_released(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    let mut state_opt = TAB_STATE.lock().unwrap();
    let state = state_opt.as_mut().unwrap();
    if !state.visible {
        return;
    }

    if let Some(w) = state.windows.get(state.selected) {
        let pid = w.pid;
        let cgwid = w.window_id;
        let wt = w.window_title.clone();
        log_debug!(
            "Switching to '{}' (pid={} cgwid={})",
            w.app_name,
            pid,
            cgwid
        );
        // 先视觉隐藏(不 orderOut),再激活目标窗口,最后延迟 orderOut。
        // 先 orderOut 会干扰 WindowServer 焦点路由,导致目标窗口的 first-responder 未确立
        // (光标停止闪烁等)。对齐 BetterCmdTab 的 vanish() -> activate() -> dismiss() 时序。
        // Vanish first (no orderOut), then activate the target, then delay orderOut.
        // Ordering out first disrupts WindowServer focus routing, leaving the target's
        // first-responder unset (caret stops blinking, etc.). Mirrors BetterCmdTab's
        // vanish() -> activate() -> dismiss() sequence.
        vanish_overlay();
        // 设置窗口无需特殊处理:浮窗是 nonactivating 面板,召唤时 app 未激活,设置窗口
        // 从未被抬升(从别的 App 召唤时被其窗口盖住;从设置召唤时透过玻璃可见),切走后
        // 留在原位。与 BetterCmdTab 行为一致,无 stash/restore 机制。
        // No settings-window handling needed: the overlay is a nonactivating panel, so the app
        // stays inactive during summon and the settings window is never raised (covered by the
        // active app's windows when summoning from elsewhere; visible through the glass when
        // summoning from settings). It stays put after the switch. Matches BetterCmdTab --
        // no stash/restore machinery.
        activate_and_raise(pid, cgwid);
        schedule_delayed_order_out();
        bump_window_mru(&mut state.mru, pid, cgwid);
        log_debug!(
            "commit: pid={} app=\"{}\" cgwid={} title=\"{}\" selected={}",
            pid,
            w.app_name,
            cgwid,
            wt,
            state.selected
        );
    } else {
        // 空窗口/选中越界:没有可切换的目标,直接收起浮窗(否则会停留在桌面上)。
        // Empty list / out-of-range selection: no switchable target, dismiss the overlay
        // (otherwise it would stay stuck on the desktop).
        log_info!(
            "CmdReleased: selected index {} out of bounds (windows={})",
            state.selected,
            state.windows.len()
        );
        hide_overlay();
    }
    state.visible = false;
}

// --- Card View ---

/// 设置关闭按钮的基础/悬停颜色与背景。
/// Apply the close button's base or hover tint and background.
unsafe fn set_close_button_hover_style(button: *mut AnyObject, hovered: bool) {
    let tint = if hovered {
        // HTML .close:hover: rgba(195, 40, 35, .86)
        hex_to_ns_color(0xC32823DB)
    } else {
        // HTML .close: rgba(0, 0, 0, .30)
        hex_to_ns_color(0x0000004D)
    };
    let _: () = msg_send![button, setContentTintColor: tint];

    let layer: *mut AnyObject = msg_send![button, layer];
    if hovered {
        // HTML .close:hover background: rgba(195, 40, 35, .07)
        layer_set_background(layer, hex_to_cg_color(0xC3282312));
    } else {
        layer_set_background(layer, std::ptr::null_mut());
    }
}

/// 关闭按钮的动态 ObjC 子类,用于实现 HTML 参考中的悬停红色反馈。
/// Dynamic ObjC subclass for the close button, providing the HTML reference's red hover feedback.
fn close_button_class() -> *mut AnyObject {
    static CLOSE_BUTTON_CLASS: OnceLock<ObjClassPtr> = OnceLock::new();
    CLOSE_BUTTON_CLASS
        .get_or_init(|| unsafe {
            let name = CString::new("OhMyTabCloseButton").unwrap();
            let superclass = class!(NSButton) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types_v_obj = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(mouseEntered:),
                close_button_mouse_entered as *mut c_void,
                types_v_obj.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseExited:),
                close_button_mouse_exited as *mut c_void,
                types_v_obj.as_ptr(),
            );
            objc_registerClassPair(cls);
            ObjClassPtr(cls as *const objc2::runtime::AnyClass)
        })
        .0 as *mut AnyObject
}

extern "C" fn close_button_mouse_entered(_self: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    unsafe {
        set_close_button_hover_style(_self as *mut AnyObject, true);
    }
}

extern "C" fn close_button_mouse_exited(_self: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    unsafe {
        set_close_button_hover_style(_self as *mut AnyObject, false);
    }
}

pub(crate) extern "C" fn card_mouse_down(_self: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    let idx = get_card_index(_self as *mut AnyObject);
    let mut state_opt = TAB_STATE.lock().unwrap();
    let state = state_opt.as_mut().unwrap();
    if let Some(w) = state.windows.get(idx) {
        let pid = w.pid;
        let cgwid = w.window_id;
        vanish_overlay();
        // 同 on_cmd_released:设置窗口无需特殊处理(见该处注释)。
        // Same as on_cmd_released: no settings-window handling needed (see comment there).
        activate_and_raise(pid, cgwid);
        schedule_delayed_order_out();
        bump_window_mru(&mut state.mru, pid, cgwid);
        state.visible = false;
    } else {
        // 空窗口时无卡片可点,理论上不可达;防御性收起浮窗(与 on_cmd_released 一致)。
        // Unreachable in practice (no cards when the list is empty); defensive dismiss,
        // same as on_cmd_released.
        hide_overlay();
        state.visible = false;
    }
}

pub(crate) extern "C" fn card_mouse_entered(_self: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    // Ignore hover until the user has moved the mouse at least once.
    // Prevents selecting the card under the cursor when the window first opens.
    let idx = get_card_index(_self as *mut AnyObject);
    if !MOUSE_MOVED.load(Ordering::Relaxed) {
        log_debug!(
            "[overlay] card {} mouseEntered (gated, mouse not moved yet)",
            idx
        );
        return;
    }
    log_debug!("[overlay] card {} mouseEntered", idx);
    let mut state_opt = TAB_STATE.lock().unwrap();
    let state = state_opt.as_mut().unwrap();
    if state.selected != idx {
        state.selected = idx;
        drop(state_opt);
        refresh_highlight();
        update_status_label();
    } else {
        drop(state_opt);
    }
}

// --- Container View ---

pub(crate) extern "C" fn container_key_down(_self: *mut c_void, _cmd: Sel, event: *mut c_void) {
    unsafe {
        let key_code: u16 = msg_send![event as *mut AnyObject, keyCode];
        let mut state_opt = TAB_STATE.lock().unwrap();
        let state = state_opt.as_mut().unwrap();

        if !state.visible {
            return;
        }

        match key_code {
            KEY_TAB | KEY_RIGHT => {
                if !state.windows.is_empty() {
                    state.selected = (state.selected + 1) % state.windows.len();
                    drop(state_opt);
                    refresh_highlight();
                    update_status_label();
                }
            }
            KEY_LEFT => {
                if !state.windows.is_empty() {
                    state.selected = if state.selected == 0 {
                        state.windows.len() - 1
                    } else {
                        state.selected - 1
                    };
                    drop(state_opt);
                    refresh_highlight();
                    update_status_label();
                }
            }
            KEY_UP => {
                if !state.windows.is_empty() && state.selected >= cards_per_row() {
                    state.selected -= cards_per_row();
                    drop(state_opt);
                    refresh_highlight();
                    update_status_label();
                }
            }
            KEY_DOWN => {
                if !state.windows.is_empty() {
                    let new_idx = state.selected + cards_per_row();
                    if new_idx < state.windows.len() {
                        state.selected = new_idx;
                        drop(state_opt);
                        refresh_highlight();
                        update_status_label();
                    }
                }
            }
            KEY_DELETE => {
                // Backspace:关闭选中卡片对应的窗口,浮窗保持打开。
                // Backspace: close the selected card's window; the overlay stays open.
                if !state.windows.is_empty() {
                    let idx = state.selected;
                    drop(state_opt);
                    close_window_at(idx);
                }
            }
            KEY_RETURN => {
                if let Some(w) = state.windows.get(state.selected) {
                    let pid = w.pid;
                    let cgwid = w.window_id;
                    vanish_overlay();
                    // 同 on_cmd_released:设置窗口无需特殊处理(见该处注释)。
                    // Same as on_cmd_released: no settings-window handling needed.
                    activate_and_raise(pid, cgwid);
                    schedule_delayed_order_out();
                    bump_window_mru(&mut state.mru, pid, cgwid);
                } else {
                    // 空窗口/选中越界:无目标,直接收起浮窗(防御,与 on_cmd_released 一致)。
                    // Empty list / out-of-range: no target, dismiss the overlay (defensive,
                    // same as on_cmd_released).
                    hide_overlay();
                }
                state.visible = false;
            }
            KEY_ESCAPE => {
                state.visible = false;
                hide_overlay();
                // 取消:设置窗口从未被触碰(nonactivating 面板不激活 app),无需恢复。
                // Cancelled: the settings window was never touched (the nonactivating panel
                // never activated the app), so nothing to restore.
            }
            _ => {}
        }
    }
}

pub(crate) extern "C" fn container_accepts_first_responder(_self: *mut c_void, _cmd: Sel) -> bool {
    true
}

/// borderless 浮窗重写:允许成为 key 窗口(否则收不到键盘事件)。
/// Override for the borderless overlay window: allow it to become key (otherwise it
/// receives no keyboard events).
pub(crate) extern "C" fn overlay_window_can_become_key(_self: *mut c_void, _cmd: Sel) -> bool {
    true
}

/// 按浮窗窗口坐标命中卡片并更新选中(主线程调用)。
/// 鼠标来源有两种:container 的 tracking area(mouseMoved:)与鼠标事件 tap(经
/// performSelectorOnMainThread 跳转)——两者都收敛到这里,坐标已转成浮窗窗口坐标。
/// Select the card under a point in the overlay's window space (main thread).
/// Two mouse sources converge here: the container's tracking area (mouseMoved:) and the
/// mouse event tap (hopped here via performSelectorOnMainThread), both with the point
/// already converted into the overlay's window space.
pub(crate) fn handle_hover_at(loc: NSPoint) {
    // 移动本身即"开门"信号,同时按鼠标当前位置补选中。
    // 为什么要补:浮窗打开瞬间鼠标可能已在卡片下,那次 mouseEntered 被门控吞掉且不会
    // 重发(已 inside)——若只靠 mouseEntered,侧键召唤场景 hover 永远不选中(实测)。
    // A move is itself the "gate open" signal; also select the card under the cursor.
    // Why: the overlay may open with the cursor already over a card -- that mouseEntered
    // gets swallowed by the gate and never re-fires (already inside), so side-button
    // summons would never hover-select if we only relied on mouseEntered (verified).
    MOUSE_MOVED.store(true, Ordering::Relaxed);
    unsafe {
        let container = match *CONTAINER.lock().unwrap() {
            Some(c) => c.0,
            None => return,
        };
        let subviews: *mut AnyObject = msg_send![container, subviews];
        let sv_count: usize = msg_send![subviews, count];
        for i in 0..sv_count {
            let sv: *mut AnyObject = msg_send![subviews, objectAtIndex: i];
            // 跳过状态栏 label(与 refresh_highlight 同款判断)。
            // Skip the status label (same check as refresh_highlight).
            let is_label: bool = msg_send![sv, isKindOfClass: class!(NSTextField)];
            if is_label {
                continue;
            }
            let frame: NSRect = msg_send![sv, frame];
            let inside = loc.x >= frame.origin.x
                && loc.x <= frame.origin.x + frame.size.width
                && loc.y >= frame.origin.y
                && loc.y <= frame.origin.y + frame.size.height;
            if !inside {
                continue;
            }
            let idx = get_card_index(sv);
            let mut state_opt = TAB_STATE.lock().unwrap();
            if let Some(state) = state_opt.as_mut() {
                if state.selected != idx {
                    log_debug!("[overlay] mm select {} -> {}", state.selected, idx);
                    state.selected = idx;
                    drop(state_opt);
                    refresh_highlight();
                    update_status_label();
                }
            }
            break;
        }
    }
}

/// 上次上报的鼠标屏幕坐标(hover 轮询 tick 用):位置未变化不重复命中(保持"移动才选中"
/// 的门控语义,浮窗打开瞬间鼠标下的卡片不会被误选)。
/// Last reported cursor screen point (hover poll tick): unchanged positions don't re-run the
/// hit test (keeping the "select only after a move" gate, so the card under the cursor at
/// summon time isn't auto-selected).
static HOVER_TICK_POS: Mutex<Option<(f64, f64)>> = Mutex::new(None);

/// hover 轮询定时器(主线程 runloop)。浮窗显示期间每 16ms 读一次 NSEvent.mouseLocation
/// 命中卡片——不依赖任何事件投递(侧键按住期间移动事件是 OtherMouseDragged 且投递给
/// 非浮窗目标,所有 tap/tracking 方案都收不到,实测;轮询直接查全局鼠标位置,与按钮
/// 状态、事件流完全无关)。
/// Hover poll timer (main-thread runloop). While the overlay is shown, NSEvent.mouseLocation
/// is read every 16ms to hit-test the cards -- independent of event delivery (moves while a
/// side button is held are OtherMouseDragged delivered to a non-overlay target; every
/// tap/tracking approach failed to see them, verified; polling reads the global cursor
/// directly, unrelated to button state or event routing).
// Send+Sync 包装(与 device.rs 的 ManagerMutex 同模式):static Mutex 需要 Send+Sync。
// Send+Sync wrapper (same pattern as device.rs's ManagerMutex): statics need Send+Sync.
struct TimerMutex(Mutex<Option<event_tap::CFRunLoopTimerRef>>);
unsafe impl Send for TimerMutex {}
unsafe impl Sync for TimerMutex {}

static HOVER_TIMER: TimerMutex = TimerMutex(Mutex::new(None));

/// hover 轮询 tick(主线程):读全局鼠标位置,位置变化时按卡片选中。
/// Hover poll tick (main thread): reads the global cursor; on movement, selects the card.
/// tick 计数器(心跳日志用,每 50 tick 打一次确认 timer 存活)。
/// Tick counter (heartbeat log every 50 ticks to confirm the timer is alive).
static TICK_N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

unsafe extern "C" fn hover_tick_callback(_timer: event_tap::CFRunLoopTimerRef, _info: *mut c_void) {
    let n = TICK_N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // 用 CGEventCreate 读当前鼠标位置:不依赖 mouseMoved 事件流(侧键按住期间系统
    // 不产生 mouseMoved,NSEvent.mouseLocation 冻结;CGEventCreate 直接查系统状态)。
    // Read the cursor via CGEventCreate: independent of the mouseMoved stream (while a side
    // button is held the system emits no mouseMoved, freezing NSEvent.mouseLocation;
    // CGEventCreate queries the system state directly).
    let ev = event_tap::CGEventCreate(std::ptr::null_mut());
    if ev.is_null() {
        return;
    }
    let pos = event_tap::CGEventGetLocation(ev);
    CFRelease(ev as *const c_void);
    // CGEventGetLocation 是 CG 坐标系(主屏左上原点),浮窗 frame 是 AppKit 坐标系
    // (主屏左下原点)——y 轴必须翻转,否则鼠标在上半屏时命中的是下半屏对称位置的
    // 卡片(实测错位)。
    // CGEventGetLocation uses the CG coordinate space (main-display top-left origin),
    // while window frames use the AppKit space (main-display bottom-left origin) -- the
    // y axis must be flipped, or the cursor in the upper half hits the mirrored card in
    // the lower half (verified misalignment).
    let main_h: f64 = {
        let main: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
        let mf: NSRect = msg_send![main, frame];
        mf.size.height
    };
    let pos = NSPoint::new(pos.x, main_h - pos.y);
    if n.is_multiple_of(50) {
        log_debug!(
            "[overlay] hover tick #{} pos=({:.0},{:.0})",
            n,
            pos.x,
            pos.y
        );
    }
    let mut last = HOVER_TICK_POS.lock().unwrap();
    match *last {
        // 首次 tick:last 还没基准,只记录位置不选中(保持"移动后才选中"的门控
        // 语义,浮窗打开瞬间鼠标下的卡片不被误选)。原实现用 map_or(pos) 计算 dx,
        // None 时 dx=dy=0 永远被 <4.0 挡掉,last 永不更新、选中永不触发(实测)。
        // First tick: no baseline yet -- record the position without selecting (keeping
        // the "select only after a move" gate; the card under the cursor at summon time
        // isn't auto-selected). The old code computed dx via map_or(pos), which made
        // dx=dy=0 when None, forever tripping the <4.0 check -- the baseline was never
        // stored and selection never fired (verified).
        None => {
            *last = Some((pos.x, pos.y));
            return;
        }
        Some((px, py)) => {
            let dx = pos.x - px;
            let dy = pos.y - py;
            if dx * dx + dy * dy < 4.0 {
                return;
            }
            *last = Some((pos.x, pos.y));
        }
    }
    drop(last);
    let container = match *CONTAINER.lock().unwrap() {
        Some(c) => c.0,
        None => return,
    };
    let win: *mut AnyObject = msg_send![container, window];
    let win_frame: NSRect = msg_send![win, frame];
    let loc = NSPoint::new(pos.x - win_frame.origin.x, pos.y - win_frame.origin.y);
    handle_hover_at(loc);
}

/// 浮窗显示期间启动 hover 轮询定时器(先清旧的)。由 show_overlay 调用。
/// Start the hover poll timer while the overlay is shown (invalidating any stale one first).
/// Called from show_overlay.
pub(crate) fn start_hover_timer() {
    unsafe {
        // 先停掉上次可能残留的定时器,避免重复 tick。
        // Invalidate any stale timer from a previous summon first.
        let old = HOVER_TIMER.0.lock().unwrap().take();
        if let Some(t) = old {
            event_tap::CFRunLoopTimerInvalidate(t);
        }
        let ctx = crate::event_tap::CFRunLoopTimerContext {
            version: 0,
            info: std::ptr::null_mut(),
            retain: None,
            release: None,
            copy_description: None,
        };
        let timer = event_tap::CFRunLoopTimerCreate(
            std::ptr::null_mut(),
            0.0,   // 立即触发一次(浮窗打开时若有移动立即选中)/ fire immediately
            0.032, // 之后每 ~32ms(≈30fps,足够 hover 且不给主线程加负载)/
            // then every ~32ms (~30fps; enough for hover without main-thread load)
            0,
            0,
            Some(hover_tick_callback),
            &ctx as *const crate::event_tap::CFRunLoopTimerContext as *mut c_void,
        );
        if !timer.is_null() {
            event_tap::CFRunLoopAddTimer(
                event_tap::CFRunLoopGetMain(),
                timer,
                event_tap::kCFRunLoopDefaultMode,
            );
            *HOVER_TIMER.0.lock().unwrap() = Some(timer);
        }
    }
}

/// 浮窗消失时停止 hover 轮询。由 vanish_overlay / hide_overlay 调用。
/// Stop the hover poll when the overlay disappears. Called from vanish_overlay / hide_overlay.
pub(crate) fn stop_hover_timer() {
    unsafe {
        let old = HOVER_TIMER.0.lock().unwrap().take();
        if let Some(t) = old {
            event_tap::CFRunLoopTimerInvalidate(t);
        }
    }
}

pub(crate) extern "C" fn container_mouse_moved(_self: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    log_debug!("[overlay] container mouseMoved:");
    // locationInWindow 相对事件源窗口 —— nonactivating 面板下 mouseMoved 事件可能
    // 挂在下方 app 的窗口上,坐标不可靠(实测 loc 不随鼠标移动变化)。改用屏幕坐标
    // (NSEvent.mouseLocation,左下原点)转浮窗窗口坐标,与卡片 frame 同基准。
    // locationInWindow is relative to the event's source window -- with the
    // nonactivating panel the mouseMoved event may attach to the app window below, so
    // the coordinate is unreliable (verified: loc never changed while moving). Use the
    // screen coordinate (NSEvent.mouseLocation, bottom-left origin) converted into the
    // overlay's window space, the same base as the card frames.
    unsafe {
        let mouse_screen: NSPoint = msg_send![class!(NSEvent), mouseLocation];
        let container = match *CONTAINER.lock().unwrap() {
            Some(c) => c.0,
            None => return,
        };
        let win: *mut AnyObject = msg_send![container, window];
        let win_frame: NSRect = msg_send![win, frame];
        let loc = NSPoint::new(
            mouse_screen.x - win_frame.origin.x,
            mouse_screen.y - win_frame.origin.y,
        );
        handle_hover_at(loc);
    }
}

// ========== 窗口激活 / window activation ==========

pub(crate) fn activate_pid(pid: i32) {
    unsafe {
        let app: *mut AnyObject =
            msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid];
        if !app.is_null() {
            // 激活该 App 但不抬起它所有窗口(不用 AllWindows)--只由 raise_ax_window 里的
            // SLPS 抬起目标那一个窗口,避免"同 App 多窗口全被拉到前面"。activate 仍触发
            // 激活通知、更新 LAST_ACTIVATED,MRU 不受影响。
            // Activate the app without raising all its windows (no AllWindows) -- only
            // raise_ax_window's SLPS call raises the single target window, avoiding "all
            // same-app windows jump forward". activate still fires the activation notification
            // and updates LAST_ACTIVATED, so MRU ordering is unaffected.
            let _: bool = msg_send![app, activateWithOptions: 0usize];
        } else {
            log_info!("activate_pid: no running app for pid {}", pid);
        }
    }
}

/// 激活 App 并把指定窗口抬到最前(用 CGWindowID 精确定位 + SLPS 只抬一个窗口)。
/// Activate the app and raise the target window (located by CGWindowID + raised
/// individually via SLPS, not all-windows).
pub(crate) fn activate_and_raise(pid: i32, cgwid: u32) {
    activate_pid(pid);
    raise_ax_window(pid, cgwid);
}

// ========== 浮窗渲染 / overlay rendering ==========

pub(crate) fn update_status_label() {
    unsafe {
        let status_label = match *STATUS_LABEL.lock().unwrap() {
            Some(l) => l.0,
            None => return,
        };
        let state_opt = TAB_STATE.lock().unwrap();
        let state = match state_opt.as_ref() {
            Some(s) => s,
            None => return,
        };
        let selected = state.selected;
        // status_text 是窗口下面那一行长的应用名称;窗口列表为空时显示"没有可切换的窗口"提示
        // (召唤空窗口态,见 show_overlay)。
        // status_text is the long app/window title line below the cards; with an empty window
        // list it shows the "no windows to switch" hint (the empty-overlay state, see show_overlay).
        let status_text = if state.windows.is_empty() {
            t("overlay.no_windows")
        } else {
            match state.windows.get(selected) {
                Some(w) => truncate_text(&display_title(&w.window_title), 126),
                None => String::new(),
            }
        };
        drop(state_opt);

        let colors = current_colors();
        let status_font: *mut AnyObject = {
            let cfg = CONFIG.read().unwrap();
            msg_send![class!(NSFont), systemFontOfSize: cfg.fonts.status_bar_size, weight: cfg.fonts.status_bar_weight]
        };
        let status_color = hex_to_ns_color(colors.status_bar_text);
        let ns_stat = make_nsstring(&status_text);
        let _: () = msg_send![status_label, setStringValue: ns_stat];
        CFRelease(ns_stat as *const c_void);
        let _: () = msg_send![status_label, setFont: status_font];
        let _: () = msg_send![status_label, setTextColor: status_color];
        // Size to fit + recenter horizontally
        let _: () = msg_send![status_label, sizeToFit];
        let fitted: NSRect = msg_send![status_label, frame];
        let stat_w = fitted.size.width;
        let container_w = {
            let container = CONTAINER.lock().unwrap();
            let c = container.unwrap().0;
            let f: NSRect = msg_send![c, frame];
            f.size.width
        };
        let stat_x = ((container_w - stat_w) / 2.0).max(0.0);
        let _: () = msg_send![status_label, setFrame: NSRect::new(NSPoint::new(stat_x, 0.0), NSSize::new(stat_w, STATUS_H))];
    }
}

pub(crate) fn hide_overlay() {
    stop_hover_timer();
    unsafe {
        if let Some(window) = *OVERLAY_WINDOW.lock().unwrap() {
            let _: () = msg_send![window.0, orderOut: std::ptr::null::<AnyObject>()];
        }
    }
    // 设置窗口从不被 stash/restore:nonactivating 面板不激活 app,设置窗口全程留在
    // 原位(z-order 不受召唤影响),切换器只负责收它作卡片与抬起目标窗口。
    // The settings window is never stashed/restored: the nonactivating panel never activates
    // the app, so the settings window stays at its natural z-order throughout the summon;
    // the switcher only collects it as a card and raises the target window.
}

/// 关闭窗口切换开关时调用:收起浮窗(orderOut)并复位 TAB_STATE.visible,
/// 避免残留状态导致下次开启后误触发。
/// Called when the switcher master switch is turned off: dismiss the overlay (orderOut)
/// and reset TAB_STATE.visible, so no stale state trips the next re-enable.
pub(crate) fn reset_switcher() {
    hide_overlay();
    if let Some(state) = TAB_STATE.lock().unwrap().as_mut() {
        state.visible = false;
    }
}

// ========== 点击外部取消 / click-outside cancel ==========

/// 注册「点击浮窗外部 → 取消本次切换」:浮窗是 key 面板,点击其他 app 的窗口时
/// WindowServer 把 key 转给新窗口 → 面板收到 NSWindowDidResignKeyNotification →
/// 收起浮窗且不切换(与 Esc 取消同语义)。
/// 点击浮窗内部不会触发(面板保持 key);点击面板自身的空白区/卡片由卡片事件处理。
///
/// 为什么不用全局鼠标监听:resign-key 通知天然区分「点击面板内/外」(事件属于本 app
/// 时不通知),无需 block、无需位置判断;且剪贴板面板已用同一模式,行为一致。
///
/// Register click-outside cancel: the overlay is the key panel, so clicking another app's
/// window hands key to it and the panel fires NSWindowDidResignKeyNotification -> dismiss
/// the overlay without switching (same semantics as Esc).
/// Clicks inside the panel never fire it (the panel keeps key); empty areas of the panel
/// are handled by card events.
///
/// Why not a global mouse monitor: the resign-key notification inherently distinguishes
/// inside/outside clicks (it doesn't fire for our own events), needs no blocks and no
/// hit-testing; the clipboard picker already uses this exact pattern.
pub(crate) fn install_click_to_cancel() {
    unsafe {
        let win = match *OVERLAY_WINDOW.lock().unwrap() {
            Some(w) => w.0,
            None => return,
        };
        let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
        let name = make_nsstring("NSWindowDidResignKeyNotification");
        let _: () = msg_send![
            center,
            addObserver: overlay_observer(),
            selector: sel!(overlayWindowResigned:),
            name: name,
            object: win
        ];
        CFRelease(name as *const c_void);
    }
}

/// overlay 专用的通知观察者单例(只承载 resign-key 回调)。
/// Singleton notification observer for the overlay (carries the resign-key callback only).
unsafe fn overlay_observer() -> *mut AnyObject {
    static OBSERVER: OnceLock<ObjPtr> = OnceLock::new();
    OBSERVER
        .get_or_init(|| {
            let name = CString::new("OhMyTabOverlayObserver").unwrap();
            let superclass = class!(NSObject) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(overlayWindowResigned:),
                overlay_window_resigned as *mut c_void,
                types.as_ptr(),
            );
            objc_registerClassPair(cls);
            let inst: *mut AnyObject = msg_send![cls as *const AnyObject, new];
            ObjPtr(inst)
        })
        .0
}

/// 浮窗失去 key → 取消切换。
/// The overlay lost key -> cancel the switch.
extern "C" fn overlay_window_resigned(_self: *mut c_void, _cmd: Sel, _note: *mut c_void) {
    // try_lock 是必须的:切换进行中(activate 目标 app → key 转移)会同步重入本回调,
    // 而 on_cmd_released 全程持 TAB_STATE 锁(非重入)——拿不到锁就跳过:切换本来
    // 就在结束浮窗,无需再取消。同理 hide_overlay 的 orderOut 也会触发本回调,
    // visible 已置 false 后重入直接返回。
    // try_lock is required: an in-flight switch (activating the target app steals key)
    // re-enters this callback synchronously while on_cmd_released holds the non-reentrant
    // TAB_STATE lock -- skip when busy, since the switch is dismissing the overlay anyway.
    // hide_overlay's orderOut also fires this callback; the re-entry returns early once
    // visible is false.
    let should_hide = match TAB_STATE.try_lock() {
        Ok(mut s) => match s.as_mut() {
            Some(st) if st.visible => {
                st.visible = false;
                true
            }
            _ => false,
        },
        Err(_) => return,
    };
    if should_hide {
        log_debug!("[overlay] cancelled by click outside (window resigned key)");
        hide_overlay();
    }
}

/// 关闭索引 removed_idx 的窗口后调整选中索引(纯函数,单测覆盖):
///
/// - 被关窗口在选中项之前 → 选中前移一格(保持指向同一张窗口);
/// - 被关窗口就是选中项或在其后 → 不动(前者自然指向下一张);
/// - 越界 → 钳到末条;空列表 → 0。
///
/// Adjust the selection after closing the window at `removed_idx` (pure, unit-tested):
///
/// - a closed window BEFORE the selection shifts it back one (same window stays selected);
/// - closing the selection itself or anything after it leaves it (the former naturally
///   points at the next window);
/// - out of range -> the tail; an empty list -> 0.
fn remove_window_adjust_selection(selected: usize, removed_idx: usize, new_len: usize) -> usize {
    let sel = if removed_idx < selected {
        selected - 1
    } else {
        selected
    };
    if new_len == 0 {
        0
    } else {
        sel.min(new_len - 1)
    }
}

/// 关闭第 idx 张卡片对应的窗口(小叉按钮 / Backspace 共用):AX 关闭成功后
/// 从列表移除、调整选中、重建浮窗;失败则列表不动(日志)。全部关完 → 收起浮窗。
/// Close the window of card `idx` (shared by the close button and Backspace): on a
/// successful AX close, remove it from the list, adjust the selection and rebuild the
/// overlay; on failure the list stays (logged). Closing the last one dismisses the overlay.
pub(crate) fn close_window_at(idx: usize) {
    let (pid, cgwid, title) = {
        let state_opt = TAB_STATE.lock().unwrap();
        let state = match state_opt.as_ref() {
            Some(s) => s,
            None => return,
        };
        match state.windows.get(idx) {
            Some(w) => (w.pid, w.window_id, w.window_title.clone()),
            None => return,
        }
    };
    if !crate::window_collector::close_ax_window(pid, cgwid) {
        log_info!(
            "close window FAILED (AX close rejected): pid={} cgwid={} title=\"{}\"",
            pid,
            cgwid,
            title
        );
        return;
    }
    log_info!(
        "close window: pid={} cgwid={} title=\"{}\"",
        pid,
        cgwid,
        title
    );
    {
        let mut state_opt = TAB_STATE.lock().unwrap();
        let state = match state_opt.as_mut() {
            Some(s) => s,
            None => return,
        };
        state.windows.remove(idx);
        state.mru.remove(&(pid, cgwid));
        if state.windows.is_empty() {
            // 全部关完:收起浮窗,不留在空态。
            // All closed: dismiss the overlay, don't linger on an empty state.
            hide_overlay();
            state.visible = false;
            return;
        }
        state.selected = remove_window_adjust_selection(state.selected, idx, state.windows.len());
    }
    // 全量重建(布局/窗口尺寸可能随行数变化),再刷新高亮。
    // Full rebuild (layout/window size may change with the row count), then the highlight.
    show_overlay();
    refresh_highlight();
}

/// 视觉隐藏浮窗但**不 orderOut**(窗口保持 ordered)。
/// 切换窗口时不能先 orderOut 再激活目标:面板 orderOut 后 WindowServer 可能把焦点路由到
/// 错误窗口,导致目标窗口的 key-window / first-responder 未被正确确立(光标停止闪烁等)。
/// 对齐 BetterCmdTab 的 vanish() -> activate() -> dismiss() 时序。
///
/// Visually hide the overlay **without orderOut** (the window stays ordered).
/// Ordering out before activating the target lets WindowServer route focus to the wrong window,
/// leaving the target's key-window / first-responder unset (caret stops blinking, etc.).
/// Mirrors BetterCmdTab's vanish() -> activate() -> dismiss() sequence.
pub(crate) fn vanish_overlay() {
    stop_hover_timer();
    unsafe {
        if let Some(window) = *OVERLAY_WINDOW.lock().unwrap() {
            // alphaValue=0 + contentView hidden:即时视觉消失,但窗口保持 ordered。
            // alphaValue=0 + contentView hidden: instant visual hide, window stays ordered.
            let _: () = msg_send![window.0, setAlphaValue: 0.0f64];
            if let Some(container) = *CONTAINER.lock().unwrap() {
                let _: () = msg_send![container.0, setHidden: true];
            }
            // 忽略鼠标事件,防止隐形面板吞点击(直到 delayed orderOut 真正移除它)。
            // Ignore mouse events so the invisible panel doesn't swallow clicks (until the
            // delayed orderOut actually removes it).
            let _: () = msg_send![window.0, setIgnoresMouseEvents: true];
            // 释放面板的 key window 状态:否则 0.2s 后 orderOut 时 AppKit 会把 key 提升给
            // 我们 app 的下一个可见窗口(设置窗口),重新激活我们,把目标窗口的焦点抢走
            // (目标红绿灯变灰,日志里可见切换后我们 app 的激活通知反复出现)。
            // 先释放 key 再激活目标,目标才能干净地拿到 key 焦点。
            // Resign the panel's key-window state: otherwise, when orderOut fires 0.2s later,
            // AppKit promotes the key to our app's next visible window (the settings window),
            // re-activating us and stealing focus from the target (grey traffic lights; the log
            // shows our app's activation notification repeatedly following switches). Resigning
            // key before activating the target lets the target take key focus cleanly.
            let _: () = msg_send![window.0, resignKeyWindow];
        }
    }
}

/// 延迟 orderOut 回调:vanish_overlay 之后由 performSelector:withObject:afterDelay: 调用,
/// 在目标窗口激活完成后真正移除浮窗。此时 WindowServer 焦点路由已稳定,orderOut 不会干扰。
///
/// Delayed orderOut callback: called via performSelector:withObject:afterDelay: after
/// vanish_overlay, removing the overlay for real once the target window's activation has
/// settled and WindowServer focus routing is stable.
pub(crate) extern "C" fn on_delayed_order_out(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    hide_overlay();
    // 恢复浮窗的 alphaValue / contentView 可见性 / 鼠标事件,下次 show_overlay 时正常显示。
    // Restore the overlay's alphaValue / contentView visibility / mouse events for the next
    // show_overlay call.
    unsafe {
        if let Some(window) = *OVERLAY_WINDOW.lock().unwrap() {
            let _: () = msg_send![window.0, setAlphaValue: 1.0f64];
            let _: () = msg_send![window.0, setIgnoresMouseEvents: false];
        }
        if let Some(container) = *CONTAINER.lock().unwrap() {
            let _: () = msg_send![container.0, setHidden: false];
        }
    }
}

/// 在主线程上延迟 0.2s 执行 orderOut(通过 controller 的 handleDelayedOrderOut:)。
/// vanish_overlay() 之后调用此函数:目标窗口的激活会在 0.2s 内完成,之后才真正移除浮窗,
/// 避免 orderOut 干扰 WindowServer 焦点路由。
///
/// Schedule a delayed orderOut on the main thread (via the controller's handleDelayedOrderOut:).
/// Called after vanish_overlay(): the target window's activation completes within 0.2s, after
/// which the overlay is removed for real, avoiding orderOut interfering with WindowServer focus.
fn schedule_delayed_order_out() {
    unsafe {
        let ctrl = crate::CONTROLLER.lock().unwrap().unwrap().0;
        // performSelector:withObject:afterDelay: 在主线程 RunLoop 上延迟调度。
        // performSelector:withObject:afterDelay: schedules on the main thread's RunLoop.
        let _: () = msg_send![
            ctrl,
            performSelector: sel!(handleDelayedOrderOut:),
            withObject: std::ptr::null::<AnyObject>(),
            afterDelay: 0.2f64
        ];
    }
}

pub(crate) fn refresh_highlight() {
    unsafe {
        let container = match *CONTAINER.lock().unwrap() {
            Some(c) => c.0,
            None => return,
        };
        let state_opt = TAB_STATE.lock().unwrap();
        let state = match state_opt.as_ref() {
            Some(s) => s,
            None => return,
        };
        if !state.visible {
            return;
        }
        let selected = state.selected;
        let colors = current_colors();
        // 选中态采用 HTML 参考中的轻量背景和 1px 内描边,不再使用厚重的蓝色边框。
        // Match the HTML reference with a subtle background and 1px inset-style border instead of
        // the previous heavy blue outline.
        let sel_bg_color = hex_to_cg_color(colors.card_bg_sel);
        let sel_border_color = hex_to_cg_color(colors.card_border_sel);

        let subviews: *mut AnyObject = msg_send![container, subviews];
        let sv_count: usize = msg_send![subviews, count];

        for i in 0..sv_count {
            let sv: *mut AnyObject = msg_send![subviews, objectAtIndex: i];
            // Only operate on card views (skip status label which is NSTextField)
            let is_nstextfield: bool = msg_send![sv, isKindOfClass: class!(NSTextField)];
            if is_nstextfield {
                continue;
            }
            let layer: *mut AnyObject = msg_send![sv, layer];
            let tag = get_card_index(sv);
            // 读卡片标题 label 文本,验证内容与索引对应(排查"显示 Picview 却打开 Ghostty")。
            // Read the card's title-label text to verify content matches the index (investigating
            // "shows Picview but opens Ghostty").
            let is_selected = tag == selected;
            if is_selected {
                let _: () = msg_send![layer, setBorderWidth: 1.0f64];
                layer_set_border(layer, sel_border_color);
                layer_set_background(layer, sel_bg_color);
            } else {
                let _: () = msg_send![layer, setBorderWidth: 0.0f64];
                layer_set_border(layer, std::ptr::null_mut());
                layer_set_background(layer, std::ptr::null_mut());
            }

            // HTML 参考中的图标在选中态向上轻移 1px;每次都从基准 y 重算,避免反复
            // 切换时累计位移。
            // The HTML reference nudges the icon up by 1px when selected; recompute from the
            // baseline on every refresh so repeated selection changes never accumulate the offset.
            let icon: *mut AnyObject = msg_send![sv, viewWithTag: ICON_VIEW_TAG];
            if !icon.is_null() {
                let icon_frame: NSRect = msg_send![icon, frame];
                let icon_px_now = icon_frame.size.height;
                let icon_bottom = card_h() - 8.0 - icon_px();
                let base_y = if (icon_px_now - icon_px()).abs() < 0.5 {
                    icon_bottom
                } else {
                    icon_bottom + (icon_px() - icon_px_now) / 2.0
                };
                let icon_y = base_y + if is_selected { 1.0 } else { 0.0 };
                let _: () = msg_send![
                    icon,
                    setFrameOrigin: NSPoint::new(icon_frame.origin.x, icon_y)
                ];
            }

            // ⌫ 关闭按钮随选中态显隐:选中卡片显示、其余隐藏(选中即出现,
            // 不限于鼠标悬停——键盘导航选中同样可见)。
            // The ⌫ close button follows the selection: the selected card shows it, the
            // rest hide it (visible whenever the card is selected, keyboard navigation
            // included -- not only while the mouse hovers).
            let btn: *mut AnyObject = msg_send![sv, viewWithTag: CLOSE_BTN_TAG];
            if !btn.is_null() {
                let _: () = msg_send![btn, setHidden: tag != selected];
            }
        }
    }
}

pub(crate) fn extract_uncached_icons() {
    let uncached: Vec<i32> = {
        let state_opt = TAB_STATE.lock().unwrap();
        if let Some(ref state) = *state_opt {
            state
                .windows
                .iter()
                .filter(|w| w.icon_path.is_none())
                .map(|w| w.pid)
                .collect::<HashSet<_>>()
                .into_iter()
                .collect()
        } else {
            return;
        }
    };

    // Record which window indices got a freshly cached icon so we can re-render
    // just those cards in place (otherwise the on-screen letter icons wouldn't
    // update until the next summon).
    let mut updated_indices: Vec<usize> = Vec::new();
    // TIMING-DEBUG 逐 PID 提取计时:定位是哪个 app 的图标提取拖慢 summon。
    let mut icons_total_ms: u128 = 0; // TIMING-DEBUG
    for pid in uncached {
        let t_icon = Instant::now(); // TIMING-DEBUG
        if let Some(ref path) = extract_icon_to_cache(pid) {
            let path = path.clone();
            let mut state_opt = TAB_STATE.lock().unwrap();
            if let Some(ref mut state) = *state_opt {
                for (i, w) in state.windows.iter_mut().enumerate() {
                    if w.pid == pid && w.icon_path.is_none() {
                        w.icon_path = Some(path.clone());
                        updated_indices.push(i);
                    }
                }
            }
        }
        let icon_ms = t_icon.elapsed().as_millis(); // TIMING-DEBUG
        icons_total_ms += icon_ms;
        // TIMING-DEBUG 标记慢提取(≥20ms)。
        if icon_ms >= 20 {
            log_debug!("[overlay] icons: extract pid={} {}ms", pid, icon_ms);
        }
    }

    if !updated_indices.is_empty() {
        let t_rebuild = Instant::now(); // TIMING-DEBUG
        rebuild_cards(&updated_indices);
        // TIMING-DEBUG 汇总:提取总耗时 + 卡片就地重建耗时。
        log_debug!(
            "[overlay] icons: extract_total={}ms rebuild_cards x={} {}ms",
            icons_total_ms,
            updated_indices.len(),
            t_rebuild.elapsed().as_millis()
        );
    }
}

/// Rebuild the card views for the given window indices in place, so newly
/// extracted icons appear immediately without re-summoning. Each affected card
/// is replaced by a fresh one built from the updated `WindowInfo` (which now has
/// an icon_path), preserving its frame and card index.
pub(crate) fn rebuild_cards(indices: &[usize]) {
    if indices.is_empty() {
        return;
    }
    let affected: HashSet<usize> = indices.iter().copied().collect();
    let to_rebuild: HashMap<usize, WindowInfo> = {
        let state_opt = TAB_STATE.lock().unwrap();
        let state = match state_opt.as_ref() {
            Some(s) => s,
            None => return,
        };
        if !state.visible {
            return;
        }
        affected
            .iter()
            .filter_map(|&i| state.windows.get(i).map(|w| (i, w.clone())))
            .collect()
    };
    if to_rebuild.is_empty() {
        return;
    }

    unsafe {
        let container = match *CONTAINER.lock().unwrap() {
            Some(c) => c.0,
            None => return,
        };
        let subviews: *mut AnyObject = msg_send![container, subviews];
        let sv_count: usize = msg_send![subviews, count];

        // Collect affected card views + their frames first; don't mutate the
        // subview array while iterating it.
        let mut replacements: Vec<(*mut AnyObject, NSRect, usize)> = Vec::new();
        for i in 0..sv_count {
            let sv: *mut AnyObject = msg_send![subviews, objectAtIndex: i];
            let is_label: bool = msg_send![sv, isKindOfClass: class!(NSTextField)];
            if is_label {
                continue;
            }
            let idx = get_card_index(sv);
            if to_rebuild.contains_key(&idx) {
                let frame: NSRect = msg_send![sv, frame];
                replacements.push((sv, frame, idx));
            }
        }

        for (old_view, frame, idx) in replacements {
            if let Some(w) = to_rebuild.get(&idx) {
                remove_card_index(old_view);
                // 沿用旧卡 frame 的宽(图标异步加载后原位替换,卡宽可能已是拉伸值)。
                // Reuse the old card frame's width (in-place icon replacement after async
                // extraction; the width may already be a stretched value).
                let new_card = create_card_view(w, idx, frame.size.width);
                let _: () = msg_send![new_card, setFrame: frame];
                let _: () = msg_send![old_view, removeFromSuperview];
                let _: () = msg_send![container, addSubview: new_card];
                release_obj(new_card); // container owns the card; drop create_card_view's alloc +1
            }
        }

        // New card views have no selection border; re-apply the highlight.
        refresh_highlight();
    }
}

/// 把 CONFIG 里的玻璃属性(style/tint/cornerRadius)重新应用到已存在的 NSGlassEffectView,
/// 用于设置热重载。仅 macOS 26+ 且玻璃视图已创建时生效;否则空操作。
/// Re-apply glass properties (style/tint/cornerRadius) from CONFIG to the existing
/// NSGlassEffectView, for hot reload. Only effective on macOS 26+ once the glass view
/// exists; otherwise a no-op.
pub(crate) unsafe fn apply_glass_properties() {
    let glass = match *GLASS_VIEW.lock().unwrap() {
        Some(g) => g.0,
        None => return,
    };
    if glass.is_null() {
        return;
    }
    let cfg = CONFIG.read().unwrap();
    let _: () = msg_send![glass, setCornerRadius: cfg.appearance.corner_radius];
    // 同步 layer 的硬裁剪:cornerRadius 只圆着色不圆模糊,需 masksToBounds 把模糊也裁进圆角
    // (见 create_overlay_window 的 (6.5) 注释)。
    // Mirror the layer hard-clip: cornerRadius rounds the tint but not the blur, so masksToBounds
    // is needed to clip the blur into the rounded shape (see (6.5) in create_overlay_window).
    let glass_layer: *mut AnyObject = msg_send![glass, layer];
    if !glass_layer.is_null() {
        let _: () = msg_send![glass_layer, setCornerRadius: cfg.appearance.corner_radius];
        let _: () = msg_send![glass_layer, setMasksToBounds: true];
    }
    let style: i64 = match cfg.appearance.glass_style.as_str() {
        "clear" => 1,
        _ => 0, // regular
    };
    let _: () = msg_send![glass, setStyle: style];
    let tint_hex = config::parse_hex8(&cfg.appearance.glass_tint);
    let tint = hex_to_ns_color(tint_hex);
    let _: () = msg_send![glass, setTintColor: tint];
}

pub(crate) fn apply_theme() {
    unsafe {
        // 主题来源只有 config(界面上的切换入口已移除;手动改 config 仍生效)。
        // The theme now comes from config only (the UI toggle is gone; manual config
        // edits still apply).
        let is_dark = crate::config::CONFIG
            .read()
            .map(|c| c.appearance.theme.as_str() != "light")
            .unwrap_or(false);

        // Update window appearance for blur material tint
        if let Some(window) = *OVERLAY_WINDOW.lock().unwrap() {
            let appearance_name = if is_dark {
                make_nsstring("NSAppearanceNameDarkAqua")
            } else {
                make_nsstring("NSAppearanceNameAqua")
            };
            let appearance: *mut AnyObject =
                msg_send![class!(NSAppearance), appearanceNamed: appearance_name];
            CFRelease(appearance_name as *const c_void);
            if !appearance.is_null() {
                let _: () = msg_send![window.0, setAppearance: appearance];
            }
        }

        apply_glass_properties();
        refresh_highlight();
    }
}

/// 把图标烘焙成灰度版:在原图上以 NSCompositeSourceAtop 叠浅灰,灰只落在图标的 alpha
/// 区域,不会在透明边缘形成方框。用于最小化窗口的图标视觉变灰。
/// Bake a grayed version: composite a light gray over the original with NSCompositeSourceAtop,
/// so the gray is confined to the icon's alpha and doesn't form a box on transparent edges.
/// Used to gray out minimized windows' icons.
unsafe fn grayed_image(orig: *mut AnyObject, size: NSSize) -> *mut AnyObject {
    let img: *mut AnyObject = msg_send![class!(NSImage), alloc];
    let img: *mut AnyObject = msg_send![img, initWithSize: size];
    let _: () = msg_send![img, lockFocus];
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), size);
    // 先画原图(NSCompositeSourceOver = 2)。
    let zero_rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
    let _: () =
        msg_send![orig, drawInRect: rect, fromRect: zero_rect, operation: 2isize, fraction: 1.0f64];
    // 再以 SourceAtop(=5)叠浅灰:只在已有 alpha 的地方着色,不超出图标范围。
    let ctx: *mut AnyObject = msg_send![class!(NSGraphicsContext), currentContext];
    let _: () = msg_send![ctx, setCompositingOperation: 5isize];
    let gray = hex_to_ns_color(0x808080AA);
    let _: () = msg_send![gray, setFill];
    let _: () = msg_send![class!(NSBezierPath), fillRect: rect];
    let _: () = msg_send![ctx, setCompositingOperation: 2isize]; // 恢复 SourceOver / restore
    let _: () = msg_send![img, unlockFocus];
    img
}

pub(crate) fn create_card_view(w: &WindowInfo, index: usize, card_width: f64) -> *mut AnyObject {
    unsafe {
        let card_cls = CARD_CLASS.lock().unwrap().unwrap();
        let card_cls_ptr = card_cls.0 as *mut AnyObject;

        // 卡宽由调用方传入:正常态 = 配置 card_w();卡片不足一行时拉伸填满(见 show_overlay)。
        // 内部元素(图标/标签)全部按实际卡宽居中,拉伸后不会偏左。
        // The card width comes from the caller: config card_w() normally, stretched to fill the
        // row when fewer cards than slots (see show_overlay). All inner elements (icon/labels)
        // are centered against the actual width, so a stretched card stays balanced.
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(card_width, card_h()));
        let view: *mut AnyObject = msg_send![card_cls_ptr, alloc];
        let view: *mut AnyObject = msg_send![view, initWithFrame: frame];

        // Enable layer for selection border
        let _: () = msg_send![view, setWantsLayer: true];
        let layer: *mut AnyObject = msg_send![view, layer];
        // 卡片圆角与参考样式一致,选中态背景和描边都沿用 14px 圆角。
        // Match the reference style with a 14px radius for the selected background and border.
        let _: () = msg_send![layer, setCornerRadius: 14.0f64];
        let _: () = msg_send![layer, setMasksToBounds: true];

        // Store card index in side map (avoids msg_send! issues on dynamic classes)
        set_card_index(view, index);

        let colors = current_colors();
        let icon_x = (card_width - icon_px()) / 2.0; // 16.0
                                                     // Standard coords: y=0 at bottom, y=200 at top.
                                                     // Icon: 8px from top -> y = 200 - 8 - 128 = 64
        let icon_bottom = card_h() - 8.0 - icon_px(); // 64.0

        // --- Icon ---
        if let Some(ref icon_path) = w.icon_path {
            let ns_path = make_nsstring(icon_path);
            let ns_image: *mut AnyObject = msg_send![class!(NSImage), alloc];
            let ns_image: *mut AnyObject = msg_send![ns_image, initWithContentsOfFile: ns_path];
            CFRelease(ns_path as *const c_void);

            if !ns_image.is_null() {
                let img_frame = NSRect::new(
                    NSPoint::new(icon_x, icon_bottom),
                    NSSize::new(icon_px(), icon_px()),
                );
                let img_view: *mut AnyObject = msg_send![class!(NSImageView), alloc];
                let img_view: *mut AnyObject = msg_send![img_view, initWithFrame: img_frame];
                // 最小化:把图标烘焙成灰度版(灰只落在图标 alpha 区域,不形成方框);否则用原图。
                // Minimized: bake a grayed version (gray confined to the icon's alpha, no box); else original.
                let image_to_show: *mut AnyObject = if w.minimized {
                    let g = grayed_image(ns_image, NSSize::new(icon_px(), icon_px()));
                    release_obj(ns_image); // 原图用完释放 / original no longer needed
                    g
                } else {
                    ns_image
                };
                let _: () = msg_send![img_view, setImage: image_to_show];
                release_obj(image_to_show); // img_view owns the image now; drop our alloc +1
                                            // NSImageScaleProportionallyUpOrDown = 3
                let _: () = msg_send![img_view, setImageScaling: 3u64];
                let _: () = msg_send![img_view, setTag: ICON_VIEW_TAG];
                let _: () = msg_send![view, addSubview: img_view];
                release_obj(img_view); // view owns the image view now; drop our alloc +1
            }
        } else {
            // Letter icon: rounded square with first letter
            let letter_sq = letter_px();
            let letter_x = icon_x + (icon_px() - letter_sq) / 2.0;
            // Center the 64x64 square within the 128x128 icon area
            let letter_y = icon_bottom + (icon_px() - letter_sq) / 2.0;
            let letter_frame = NSRect::new(
                NSPoint::new(letter_x, letter_y),
                NSSize::new(letter_sq, letter_sq),
            );

            let letter_view: *mut AnyObject = msg_send![class!(NSView), alloc];
            let letter_view: *mut AnyObject = msg_send![letter_view, initWithFrame: letter_frame];
            let _: () = msg_send![letter_view, setWantsLayer: true];
            let _: () = msg_send![letter_view, setTag: ICON_VIEW_TAG];
            let ll: *mut AnyObject = msg_send![letter_view, layer];
            let _: () = msg_send![ll, setCornerRadius: 14.0f64];
            let _: () = msg_send![ll, setMasksToBounds: true];
            let bg_color = hex_to_cg_color(colors.icon_inner_bg);
            layer_set_background(ll, bg_color);

            let init = w.app_name.chars().next().unwrap_or('?').to_string();
            let font: *mut AnyObject =
                msg_send![class!(NSFont), systemFontOfSize: 28.0f64, weight: 0.4f64];
            let text_color = hex_to_ns_color(colors.icon_text);
            let label = make_centered_label(&init, font, text_color, 0.0, letter_sq, letter_sq);
            let _: () = msg_send![letter_view, addSubview: label];
            release_obj(label); // letter_view owns the label; drop our alloc +1
            let _: () = msg_send![view, addSubview: letter_view];
            release_obj(letter_view); // view owns the letter view; drop our alloc +1
            if w.minimized {
                // 最小化窗口:在字母图标上叠浅灰半透明遮罩(圆角与字母背景一致)。
                // Minimized window: overlay a light wash on the letter icon (radius matches the bg).
                let dim: *mut AnyObject = msg_send![class!(NSView), alloc];
                let dim: *mut AnyObject = msg_send![dim, initWithFrame: letter_frame];
                let _: () = msg_send![dim, setWantsLayer: true];
                let dl: *mut AnyObject = msg_send![dim, layer];
                let _: () = msg_send![dl, setCornerRadius: 14.0f64];
                let _: () = msg_send![dl, setMasksToBounds: true];
                layer_set_background(dl, hex_to_cg_color(0x808080AA));
                let _: () = msg_send![view, addSubview: dim];
                release_obj(dim);
            }
        }

        // Gap below icon before text starts
        let text_gap: f64 = 6.0;
        // App name: 18px tall, 2px above window title
        let name_bottom = icon_bottom - text_gap - 18.0; // 64 - 6 - 18 = 40
                                                         // Window title: 16px tall, sits at bottom
        let title_bottom = name_bottom - 2.0 - 16.0; // 40 - 2 - 16 = 22

        // --- App name label ---
        let name_font: *mut AnyObject = {
            let cfg = CONFIG.read().unwrap();
            msg_send![class!(NSFont), systemFontOfSize: cfg.fonts.app_name_size, weight: cfg.fonts.app_name_weight]
        };
        let name_color = hex_to_ns_color(colors.app_name);
        let name_label = make_centered_label(
            &truncate_text(&w.app_name, 17),
            name_font,
            name_color,
            name_bottom,
            card_width,
            18.0,
        );
        let _: () = msg_send![view, addSubview: name_label];
        release_obj(name_label); // view owns the label; drop our alloc +1

        // --- Window title label ---
        let title_font: *mut AnyObject = {
            let cfg = CONFIG.read().unwrap();
            msg_send![class!(NSFont), systemFontOfSize: cfg.fonts.title_size, weight: cfg.fonts.title_weight]
        };
        let win_color = hex_to_ns_color(colors.win_title);
        let title_label = make_centered_label(
            &truncate_text(&display_title(&w.window_title), 20),
            title_font,
            win_color,
            title_bottom,
            card_width,
            16.0,
        );
        let _: () = msg_send![view, addSubview: title_label];
        release_obj(title_label); // view owns the label; drop our alloc +1

        // --- Tracking area for hover ---
        // NSTrackingMouseEnteredAndExited | NSTrackingActiveAlways
        // activeAlways:召唤时 app 未激活(nonactivating 面板),必须用 activeAlways 才能收
        // mouseEntered 悬停事件。activeInActiveApp(0x40) 在 app 非激活时不投递。
        let opts: u64 = 0x01 | 0x80;
        let ta: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
        let bounds = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(card_width, card_h()));
        let ta: *mut AnyObject = msg_send![ta, initWithRect: bounds, options: opts, owner: view, userInfo: std::ptr::null::<AnyObject>()];
        let _: () = msg_send![view, addTrackingArea: ta];
        release_obj(ta); // view owns the tracking area; drop our alloc +1

        // --- 右上角关闭按钮:按 HTML 参考使用 ×,选中/悬停显示,按钮悬停变红 ---
        // Top-right close button: use the HTML reference's ×, show on selection/hover, and turn red on button hover.
        let btn: *mut AnyObject = msg_send![close_button_class(), alloc];
        let btn: *mut AnyObject = msg_send![btn, initWithFrame: NSRect::new(
            NSPoint::new(card_width - 27.0, card_h() - 27.0),
            NSSize::new(20.0, 20.0)
        )];
        let _: () = msg_send![btn, setBordered: false];
        let title_ns = make_nsstring("×");
        let _: () = msg_send![btn, setTitle: title_ns];
        CFRelease(title_ns as *const c_void);
        let close_font: *mut AnyObject =
            msg_send![class!(NSFont), systemFontOfSize: 12.0f64, weight: 0.0f64];
        let _: () = msg_send![btn, setFont: close_font];
        let _: () = msg_send![btn, setAlignment: 1isize]; // NSTextAlignmentCenter on arm64
                                                          // HTML .close 的默认状态是透明背景 + 半透明黑色文字。
                                                          // The HTML .close base state uses a transparent background and translucent black text.
        let _: () = msg_send![btn, setWantsLayer: true];
        let bl: *mut AnyObject = msg_send![btn, layer];
        let _: () = msg_send![bl, setCornerRadius: 6.0f64];
        let _: () = msg_send![bl, setMasksToBounds: true];
        set_close_button_hover_style(btn, false);
        let _: () = msg_send![btn, setTag: CLOSE_BTN_TAG];
        let _: () = msg_send![btn, setTarget: crate::CONTROLLER.lock().unwrap().unwrap().0];
        let _: () = msg_send![btn, setAction: sel!(closeCard:)];
        let _: () = msg_send![btn, setHidden: true];

        // 给按钮单独添加 tracking area,让悬停颜色只在指针进入 × 按钮时变化。
        // Add a tracking area to the button itself so the red hover style only applies while
        // the pointer is over the × button.
        let opts: u64 = 0x01 | 0x80; // NSTrackingMouseEnteredAndExited | ActiveAlways
        let ta: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
        let ta: *mut AnyObject = msg_send![ta, initWithRect: NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(20.0, 20.0)
        ), options: opts, owner: btn, userInfo: std::ptr::null::<AnyObject>()];
        let _: () = msg_send![btn, addTrackingArea: ta];
        release_obj(ta);

        let _: () = msg_send![view, addSubview: btn];
        release_obj(btn); // view owns the button; drop our alloc +1

        view
    }
}

/// 按配置选择浮窗目标屏幕的 frame(全局坐标系)。
/// - "main":始终主显示器(NSScreen.screens 的 index 0,系统保证首屏带菜单栏)。
/// - "active_window":跟随激活窗口——取激活窗口 bounds 中心点所在的屏幕;激活窗口
///   bounds 不可用(全 0 / 无窗口)或中心不在任何屏幕上时,回退主显示器。
///
/// 注意不能用 NSScreen.mainScreen 当"主屏":它的语义是"包含键盘焦点窗口的屏幕",
/// 召唤浮窗时焦点在激活应用上,若激活应用在副屏,mainScreen 返回副屏,"始终主屏"
/// 就会表现成跟随激活窗口。主显示器 = screens[0]。
///
/// Pick the target screen frame for the overlay (global coords) per config:
/// - "main": always the primary display (index 0 of NSScreen.screens; the first entry is
///   guaranteed to host the menu bar).
/// - "active_window": follow the active window -- the screen containing the center of the
///   active window's bounds; falls back to the primary display when the bounds are unavailable
///   (all zeros / no windows) or the center isn't on any screen.
///
/// Note: NSScreen.mainScreen must NOT be used as "the primary screen" -- it returns the screen
/// containing the key window, so summoning while the active app sits on a secondary display
/// would resolve to that display, making "always on main screen" behave like "follow active
/// window". The primary display is screens[0].
fn overlay_target_screen(windows: &[WindowInfo]) -> NSRect {
    unsafe {
        let pos = CONFIG.read().unwrap().windows.overlay_position.clone();
        // 主显示器 = screens[0](系统保证首屏带菜单栏);screens 为空时回退 mainScreen。
        // Primary display = screens[0] (first entry hosts the menu bar); fall back to
        // mainScreen if the screens array is somehow empty.
        let main_frame: NSRect = {
            let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
            let count: usize = msg_send![screens, count];
            if count > 0 {
                // objectAtIndex: 的参数编码是 'q'(signed long),必须传 isize/i64;
                // 传整数字面量会被推断为 i32('i'),objc2 运行时校验会 panic。
                // objectAtIndex: expects a 'q' (signed long) argument; pass isize/i64 or
                // objc2's runtime encoding check panics on an i32 literal.
                let s: *mut AnyObject = msg_send![screens, objectAtIndex: 0isize];
                msg_send![s, frame]
            } else {
                let main: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
                msg_send![main, frame]
            }
        };
        if pos != "active_window" {
            return main_frame;
        }
        // 激活窗口:collect_windows 排序后 index 0 = 当前前台窗口(is_active 已置位)。
        // The active window: after collect_windows' sort, index 0 is the frontmost (is_active set).
        let Some(active) = windows.iter().find(|w| w.is_active) else {
            return main_frame;
        };
        let (bx, by, bw, bh) = active.bounds;
        // bounds 全 0 = 未获取到,无法定位,回退主屏。
        // All-zero bounds = unavailable, can't locate, fall back to the main screen.
        if bw <= 0.0 || bh <= 0.0 {
            return main_frame;
        }
        let cx = bx + bw / 2.0;
        let cy = by + bh / 2.0;
        // 遍历所有屏幕,找包含激活窗口中心的那个。
        // Iterate all screens, find the one containing the active window's center.
        let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
        let count: usize = msg_send![screens, count];
        let mut i = 0usize;
        while i < count {
            // 同 934 行:objectAtIndex: 参数编码 'q',传 isize(usize 编码 'Q' 也会校验失败)。
            // Same as line 934: objectAtIndex: wants 'q'; usize ('Q') would fail the check too.
            let s: *mut AnyObject = msg_send![screens, objectAtIndex: i as isize];
            let f: NSRect = msg_send![s, frame];
            if cx >= f.origin.x
                && cx <= f.origin.x + f.size.width
                && cy >= f.origin.y
                && cy <= f.origin.y + f.size.height
            {
                return f;
            }
            i += 1;
        }
        main_frame
    }
}

pub(crate) fn show_overlay() {
    unsafe {
        // TIMING-DEBUG 阶段计时:定位 summon 卡顿——卡片构建 / 图标 / resize / 状态栏。
        let t0 = Instant::now();
        let state_opt = TAB_STATE.lock().unwrap();
        let state = state_opt.as_ref().unwrap();
        let count = state.windows.len();
        let windows = state.windows.clone();
        drop(state_opt);

        let window = OVERLAY_WINDOW.lock().unwrap().unwrap().0;
        let container = CONTAINER.lock().unwrap().unwrap().0;

        // Remove old card subviews (keep status label)
        let subviews: *mut AnyObject = msg_send![container, subviews];
        let sv_count: usize = msg_send![subviews, count];
        // Iterate in reverse since we're removing from the array
        let mut i = sv_count;
        while i > 0 {
            i -= 1;
            let sv: *mut AnyObject = msg_send![subviews, objectAtIndex: i];
            let is_label: bool = msg_send![sv, isKindOfClass: class!(NSTextField)];
            if !is_label {
                let _: () = msg_send![sv, removeFromSuperview];
            }
        }
        let t_remove_ms = t0.elapsed().as_millis(); // TIMING-DEBUG

        // Clear old card index mappings, then create new card views
        clear_card_indices();
        let h = window_height(count);
        // 窗口宽按「槽位」计算:最少 3 个槽位(count<3 时也保持三卡宽,空窗口态同样)。
        // 槽位 = min(每行卡数配置, max(3, count))。
        // The window width is based on "slots": at least 3 (count<3 and the empty state keep
        // the three-card width). slots = min(cards-per-row config, max(3, count)).
        let slots = cards_per_row().min(count.max(3));
        let w = window_width(slots);
        // 卡片不足槽位(1-2 卡)时拉伸填满整行,不留右空白;卡片内部元素按实际卡宽居中
        // (见 create_card_view 的 card_width 参数)。其余情况用配置卡宽、行内居中。
        // With fewer cards than slots (1-2), cards stretch to fill the row -- no right-side
        // blank; inner elements center on the actual card width (see create_card_view's
        // card_width). Otherwise the configured width applies and the row is centered.
        let (card_w_eff, pitch, start_x) = if count > 0 && count < slots {
            let inner = w - H_PADDING * 2.0;
            let cw = (inner - (count as f64 - 1.0) * card_gap()) / count as f64;
            (cw, cw + card_gap(), H_PADDING)
        } else {
            let row_width = slots as f64 * card_w() + (slots.saturating_sub(1)) as f64 * card_gap();
            (card_w(), card_w() + card_gap(), (w - row_width) / 2.0)
        };

        let mut cards_total_ms: u128 = 0; // TIMING-DEBUG
        for (idx, w) in windows.iter().enumerate() {
            let t_card = Instant::now(); // TIMING-DEBUG
            let card = create_card_view(w, idx, card_w_eff);
            let card_ms = t_card.elapsed().as_millis(); // TIMING-DEBUG
            cards_total_ms += card_ms;
            // TIMING-DEBUG 单卡构建 >5ms:标记慢卡(图标加载/文本通常是耗时大头)。
            if card_ms >= 5 {
                log_debug!(
                    "[overlay] card #{} slow: {}ms app=\"{}\"",
                    idx,
                    card_ms,
                    w.app_name
                );
            }

            // Standard coords: y=0 at bottom. Cards stack from top down.
            let col = idx % cards_per_row();
            let row = idx / cards_per_row();
            let card_x = start_x + col as f64 * pitch;
            // topmost card origin_y = h - 32.0 - card_h() (32 = top padding area)
            let card_y = h - 32.0 - (row + 1) as f64 * card_h();
            let card_frame = NSRect::new(
                NSPoint::new(card_x, card_y),
                NSSize::new(card_w_eff, card_h()),
            );
            let _: () = msg_send![card, setFrame: card_frame];

            let _: () = msg_send![container, addSubview: card];
            release_obj(card); // container owns the card; drop create_card_view's alloc +1
        }
        let t_cards_ms = t0.elapsed().as_millis(); // TIMING-DEBUG

        // Resize window (h computed above). Target screen per config (follow active window / main).
        let screen_frame = overlay_target_screen(&windows);
        let x = (screen_frame.size.width - w) / 2.0 + screen_frame.origin.x;
        let y = (screen_frame.size.height - h) / 2.0 + screen_frame.origin.y;
        let new_frame = NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));
        let _: () = msg_send![window, setFrame: new_frame, display: true];

        // wrapper / VFX view / container all have autoresizingMask = 18
        // (width + height sizable), so they resize automatically when the
        // window frame changes. Just update the container explicitly.
        let _: () = msg_send![container, setFrameSize: NSSize::new(w, h)];

        // 状态栏文本必须在窗口/容器 resize 之后居中:update_status_label 按容器当前宽度
        // 计算 x,若在 resize 前调用会拿旧宽度(启动初为最大宽度、之后为上次召唤的宽度)
        // 定位,容器缩小后文本就偏右/偏左(表现为标题栏不居中)。
        // The status text must be centered AFTER the window/container resize:
        // update_status_label computes x from the container's current width; if called before
        // the resize it uses the stale width (the initial max width at launch, or the previous
        // summon's width), leaving the text off-center once the container shrinks.
        update_status_label();

        // Ignore initial mouse position - require a real mouse movement before
        // hover-selection kicks in (matches native Cmd+Tab behaviour).
        MOUSE_MOVED.store(false, Ordering::Relaxed);
        // 轮询基准同步重置:否则上一次召唤残留的基准会让本次第一个 tick 误判"已移动"
        // (召唤间隙鼠标动过),浮窗一打开就选中鼠标下的卡片(实测)。
        // Reset the poll baseline too: a stale baseline from the previous summon would make
        // the first tick of this summon misjudge "moved" (cursor moved between summons),
        // selecting the card under the cursor the moment the overlay opens (verified).
        *HOVER_TICK_POS.lock().unwrap() = None;
        let _: () = msg_send![window, setAcceptsMouseMovedEvents: true];
        // 召唤后刷新一次高亮/选中态:新卡片刚创建(⌫ 按钮默认隐藏),选中卡片的
        // 边框与 ⌫ 需要按当前选中项补上。
        // Refresh the highlight/selection once after summoning: fresh cards start with the
        // ⌫ button hidden, so the selected card's border and ⌫ must be applied now.
        refresh_highlight();

        // Show window. NSPanel + nonactivatingPanel: the panel becomes key (keyboard works)
        // WITHOUT activating our app -- do NOT call activateIgnoringOtherApps, or the settings
        // window would be raised above the active app again. App stays inactive during the
        // whole summon, so the settings window is never raised (and no stash is needed).
        let _: () = msg_send![window, makeKeyAndOrderFront: std::ptr::null::<AnyObject>()];
        let _: bool = msg_send![window, makeFirstResponder: container];
        // 启动 hover 轮询:浮窗显示期间每 16ms 读全局鼠标位置命中卡片(侧键按住期间
        // 移动事件无法经 tap/tracking 获取,轮询是唯一可靠来源)。
        // Start the hover poll: while shown, read the global cursor every 16ms to hit-test
        // (moves while a side button is held can't be seen via taps/tracking; polling is
        // the only reliable source).
        start_hover_timer();

        // App 未激活时 NSView 的 mouseMoved: 可能不投递(即使面板是 key),所以给容器加一个
        // activeAlways 的 tracking area(mouseMoved|activeAlways|inVisibleRect)兜底,保证
        // MOUSE_MOVED 标志能置位——否则悬停门控永远不开启。对齐 BetterCmdTab 的做法
        // (SwitcherView 用 .mouseMoved + .activeAlways)。
        // When the app is inactive, NSView mouseMoved: may not be delivered even to the key
        // panel, so add an activeAlways tracking area (mouseMoved|activeAlways|inVisibleRect)
        // to the container to guarantee the MOUSE_MOVED gate flips -- otherwise hover selection
        // never enables. Same approach as BetterCmdTab's SwitcherView (.mouseMoved + .activeAlways).
        // App 未激活时 NSView 的 mouseMoved: 可能不投递(即使面板是 key),所以给容器加一个
        // activeAlways 的 tracking area(mouseMoved|activeAlways|inVisibleRect)兜底,保证
        // MOUSE_MOVED 标志能置位——否则悬停门控永远不开启。对齐 BetterCmdTab 的做法
        // (SwitcherView 用 .mouseMoved + .activeAlways)。
        // When the app is inactive, NSView mouseMoved: may not be delivered even to the key
        // panel, so add an activeAlways tracking area (mouseMoved|activeAlways|inVisibleRect)
        // to the container to guarantee the MOUSE_MOVED gate flips -- otherwise hover selection
        // never enables. Same approach as BetterCmdTab's SwitcherView (.mouseMoved + .activeAlways).
        // 先清掉旧 tracking areas(每次召唤都 add 会堆积,旧的可能失效导致 mouseMoved
        // 不再投递 —— 实测部分召唤后 hover 完全无响应)。
        // Clear stale tracking areas first (adding on every summon piles them up and old
        // ones can go stale, killing mouseMoved delivery -- verified: some summons had no
        // hover response at all).
        let old_areas: *mut AnyObject = msg_send![container, trackingAreas];
        let old_cnt: usize = msg_send![old_areas, count];
        for i in 0..old_cnt {
            let area: *mut AnyObject = msg_send![old_areas, objectAtIndex: i];
            let _: () = msg_send![container, removeTrackingArea: area];
        }
        let mm_ta: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
        // NSTrackingMouseMoved=0x02 | NSTrackingActiveAlways=0x80 | NSTrackingInVisibleRect=0x200。
        // 注意激活模式(NSTrackingActive*)只能指定一个,多指定会抛 NSInvalidArgumentException。
        // 0x04 = mouseDragged:侧键物理按下期间(吞掉 down 后系统仍可能把移动当作
        // drag 事件)也能收到移动;0x02 = mouseMoved;0x80 = activeAlways;0x200 = inVisibleRect。
        // 0x04 = mouseDragged: while a side button is physically held (the system may still
        // treat moves as drags after the tap swallowed the down) moves still arrive;
        // 0x02 = mouseMoved; 0x80 = activeAlways; 0x200 = inVisibleRect.
        let mm_opts: u64 = 0x02 | 0x04 | 0x80 | 0x200;
        let container_bounds: NSRect = msg_send![container, bounds];
        let mm_ta: *mut AnyObject = msg_send![mm_ta, initWithRect: container_bounds, options: mm_opts, owner: container, userInfo: std::ptr::null::<AnyObject>()];
        let _: () = msg_send![container, addTrackingArea: mm_ta];
        release_obj(mm_ta); // container owns the tracking area; drop our alloc +1

        // Highlight selected card
        refresh_highlight();
        let t_resize_ms = t0.elapsed().as_millis(); // TIMING-DEBUG

        // 补提取缺失图标(启动时未缓存/启动通知提取失败的应用,如刚启动 icon 未就绪的
        // LinearMouse)。每次召唤都触发,而不是只在浮窗已可见时连按 Tab——否则这些 app
        // 会一直显示字母占位,直到用户碰巧连续按 Tab。提取成功会 rebuild_cards 就地刷新。
        // Backfill missing icons (apps not cached at startup / whose launch-notification extract
        // failed, e.g. LinearMouse when its icon wasn't ready yet). Runs on every summon instead of
        // only on repeated Tab while visible -- otherwise such apps show the letter placeholder
        // until the user happens to press Tab again. Successful extracts rebuild cards in place.
        let t_icons = Instant::now(); // TIMING-DEBUG
        extract_uncached_icons();
        // TIMING-DEBUG 汇总:各阶段耗时(排查 summon 卡顿用)。
        let total_ms = t0.elapsed().as_millis();
        log_debug!(
            "[overlay] show: remove={}ms cards={}ms (sum={}ms) resize+status+highlight={}ms icons={}ms total={}ms",
            t_remove_ms,
            t_cards_ms - t_remove_ms,
            cards_total_ms,
            t_resize_ms - t_cards_ms,
            t_icons.elapsed().as_millis(),
            total_ms
        );
    }
}
