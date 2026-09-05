//! 窗口控制模块:Option+方向键模拟 Windows 的 Win+方向键窗口管理。
//! 独立 session 层 event tap(专用线程)拦截 Option+方向键,事件经既有 bridge
//! (GlobalEvent -> performSelectorOnMainThread)投递到主线程执行 AX 移动/缩放/最小化。
//! 状态(普通/最大化/上下半屏/左右半屏/四分屏)按当前 frame 与目标矩形匹配推断,无需持久状态;
//! 「原尺寸」在首次从普通状态进入 snap 时按 CGWindowID 记录,供后续恢复逻辑使用。
//!
//! Window control module: Option+arrow keys emulate Windows' Win+arrow window management.
//! A dedicated session-level event tap (own thread) intercepts Option+arrows; events travel
//! through the existing bridge (GlobalEvent -> performSelectorOnMainThread) and run on the main
//! thread, which moves/resizes/minimizes windows via AX. Snap states (normal/maximized/top-bottom
//! halves/left-right halves/quarters) are inferred by matching the current frame against target
//! rectangles, so nothing is
//! persisted; the "original size" is recorded per CGWindowID when a normal window first snaps.

use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::NSRect;

use crate::event_monitor::GlobalEvent;
use crate::event_tap::{
    self, tap_location, tap_options, tap_placement, CFRunLoopGetCurrent, CFRunLoopRef,
    CGEventFlags, CGEventGetFlags, CGEventGetIntegerValueField, CGEventMask, CGEventRef,
    CGEventTapProxy, CGEventType, K_CG_EVENT_SOURCE_USER_DATA, SYNTHETIC_MARKER,
};
use crate::ffi::{
    kCFBooleanFalse, kCFBooleanTrue, AXError, AXUIElementCopyAttributeValue,
    AXUIElementCreateApplication, AXUIElementPerformAction, AXUIElementRef,
    AXUIElementSetAttributeValue, AXUIElementSetMessagingTimeout, AXValueCreate, AXValueGetValue,
    CFBooleanGetValue, CFRelease, K_AX_SUCCESS,
};
use crate::window_collector::{ax_window_cgwid, cf_string_new, cf_to_rust_string};
use crate::{log_debug, log_info};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;

// ========== 键盘事件常量 / keyboard event constants ==========
// 见 CGEventTypes.h;键码来自 Carbon HIToolbox Events.h。
// See CGEventTypes.h; keycodes are from Carbon HIToolbox Events.h.
const K_CG_EVENT_KEY_DOWN: CGEventType = 10;
const K_CG_EVENT_KEY_UP: CGEventType = 11;
const K_CG_KEYBOARD_EVENT_KEYCODE: i32 = 9;
const K_CG_KEYBOARD_EVENT_AUTOREPEAT: i32 = 8;
// 方向键键码 / arrow keycodes.
const K_VK_LEFT: u16 = 123;
const K_VK_RIGHT: u16 = 124;
const K_VK_DOWN: u16 = 125;
const K_VK_UP: u16 = 126;
// 修饰键位掩码:必须恰好是 Option(带其他修饰键的组合透传,与 Option+V 同规则)。
// Modifier masks: exactly Option is required; combos with extra modifiers pass through
// (same rule as Option+V).
const K_FLAG_OPTION: CGEventFlags = 0x00080000;
const K_FLAG_COMMAND: CGEventFlags = 0x00100000;
const K_FLAG_SHIFT: CGEventFlags = 0x00020000;
const K_FLAG_CONTROL: CGEventFlags = 0x00040000;

// ========== AX 属性名与常量 / AX attribute names and constants ==========
const K_AX_FOCUSED_WINDOW: &str = "AXFocusedWindow";
const K_AX_POSITION: &str = "AXPosition";
const K_AX_SIZE: &str = "AXSize";
const K_AX_MINIMIZED: &str = "AXMinimized";
const K_AX_SUBROLE: &str = "AXSubrole";
const K_AX_ZOOM_BUTTON: &str = "AXZoomButton";
const K_AX_PRESS: &str = "AXPress";
// 全屏窗口(AXFullScreen)不参与 snap:原生全屏有独立的空间管理。
// Fullscreen windows (AXFullScreen) never snap: native fullscreen has its own space management.
const K_AX_SUBROLE_FULL_SCREEN: &str = "AXFullScreen";
// kAXValueCGPointType / kAXValueCGSizeType(HIServices)。
// kAXValueCGPointType / kAXValueCGSizeType (HIServices).
const K_AX_VALUE_CG_POINT: i32 = 1;
const K_AX_VALUE_CG_SIZE: i32 = 2;

/// AXValue 的 C 结构(布局与 CoreGraphics CGPoint/CGSize 逐字节一致)。
/// C structs for AXValue (byte-identical to CoreGraphics CGPoint/CGSize).
#[repr(C)]
struct CgPoint {
    x: f64,
    y: f64,
}
#[repr(C)]
struct CgSize {
    w: f64,
    h: f64,
}

/// 方向。数值顺序经 NSNumber 跨线程传递(bridge -> 主线程),只能追加不能重排。
/// Direction. The numeric order crosses threads via NSNumber (bridge -> main thread);
/// append-only, never reorder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    Left = 0,
    Right = 1,
    Up = 2,
    Down = 3,
}

impl Direction {
    /// 从 bridge 传来的整数还原方向(未知值静默丢弃)。
    /// Rebuild a direction from the bridge integer (unknown values are dropped).
    pub(crate) fn from_isize(v: isize) -> Option<Self> {
        match v {
            0 => Some(Self::Left),
            1 => Some(Self::Right),
            2 => Some(Self::Up),
            3 => Some(Self::Down),
            _ => None,
        }
    }

    fn from_keycode(code: u16) -> Option<Self> {
        match code {
            K_VK_LEFT => Some(Self::Left),
            K_VK_RIGHT => Some(Self::Right),
            K_VK_UP => Some(Self::Up),
            K_VK_DOWN => Some(Self::Down),
            _ => None,
        }
    }
}

/// AX 全局坐标系(主屏左上原点,y 向下,点单位)下的矩形。
/// A rectangle in the AX global coordinate space (primary-display top-left origin, y down,
/// points).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct AxRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl AxRect {
    fn center(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    fn contains_point(&self, px: f64, py: f64) -> bool {
        px >= self.x && px <= self.x + self.w && py >= self.y && py <= self.y + self.h
    }
}

/// 一块屏幕的几何(frame 含菜单栏/Dock,visible 为可视区),均为 AX 坐标。
/// One screen's geometry (frame includes the menu bar/Dock; visible is the work area),
/// both in AX coordinates.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ScreenGeometry {
    pub frame: AxRect,
    pub visible: AxRect,
}

/// 窗口 snap 状态(frame 推断,无需持久化)。
/// Window snap state (inferred from the frame; nothing persisted).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SnapState {
    Normal,
    Maximized,
    TopHalf,
    BottomHalf,
    LeftHalf,
    RightHalf,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Minimized,
}

/// 主线程待执行的动作。
/// The action to run on the main thread.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Plan {
    /// 把窗口设置为目标矩形(AX 坐标)。
    /// Set the window frame to the target rect (AX coordinates).
    Move(AxRect),
    /// 最大化到当前屏幕可视区;AX 精确设置失败时由执行层回退到原生缩放按钮。
    /// Maximize to the current screen's visible area; execution falls back to the native zoom
    /// button when the AX exact-frame write is rejected.
    Maximize(AxRect),
    /// 最小化(AXMinimized = true)。
    /// Minimize (AXMinimized = true).
    Minimize,
    /// 无操作(如单屏最左侧半屏继续向左)。
    /// No-op (e.g. moving left from the left half on a single screen).
    Nothing,
}

/// 一块可视区对应的全部 snap 目标矩形(最大化、上下/左右半屏、四分屏)。
/// All snap target rects for one visible area (maximize, top/bottom, left/right, and quarters).
pub(crate) struct SnapFrames {
    pub max: AxRect,
    pub top: AxRect,
    pub bottom: AxRect,
    pub left: AxRect,
    pub right: AxRect,
    pub top_left: AxRect,
    pub top_right: AxRect,
    pub bottom_left: AxRect,
    pub bottom_right: AxRect,
}

/// 由可视区算出最大化、上下/左右半屏和四分屏目标(纯函数,单测覆盖)。
/// Compute maximize, top/bottom, left/right, and quarter targets from a visible area (pure;
/// unit-tested).
pub(crate) fn snap_frames(v: AxRect) -> SnapFrames {
    let hw = v.w / 2.0;
    let hh = v.h / 2.0;
    let mx = v.x + hw;
    let my = v.y + hh;
    SnapFrames {
        max: AxRect {
            x: v.x,
            y: v.y,
            w: v.w,
            h: v.h,
        },
        top: AxRect {
            x: v.x,
            y: v.y,
            w: v.w,
            h: hh,
        },
        bottom: AxRect {
            x: v.x,
            y: my,
            w: v.w,
            h: hh,
        },
        left: AxRect {
            x: v.x,
            y: v.y,
            w: hw,
            h: v.h,
        },
        right: AxRect {
            x: mx,
            y: v.y,
            w: hw,
            h: v.h,
        },
        top_left: AxRect {
            x: v.x,
            y: v.y,
            w: hw,
            h: hh,
        },
        top_right: AxRect {
            x: mx,
            y: v.y,
            w: hw,
            h: hh,
        },
        bottom_left: AxRect {
            x: v.x,
            y: my,
            w: hw,
            h: hh,
        },
        bottom_right: AxRect {
            x: mx,
            y: my,
            w: hw,
            h: hh,
        },
    }
}

/// frame 比对容差:我们设置的矩形是精确的,但部分 App 应用后会微调 1pt 内。
/// Frame-match tolerance: we set exact rects, but some apps nudge them within ~1pt.
const FRAME_EPSILON: f64 = 1.5;

fn rect_close(a: AxRect, b: AxRect) -> bool {
    (a.x - b.x).abs() <= FRAME_EPSILON
        && (a.y - b.y).abs() <= FRAME_EPSILON
        && (a.w - b.w).abs() <= FRAME_EPSILON
        && (a.h - b.h).abs() <= FRAME_EPSILON
}

/// 按当前 frame 推断 snap 状态;都不匹配即普通窗口(纯函数,单测覆盖)。
/// Infer the snap state from the current frame; no match means normal (pure; unit-tested).
pub(crate) fn infer_state(frame: AxRect, visible: AxRect) -> SnapState {
    let f = snap_frames(visible);
    if rect_close(frame, f.max) {
        SnapState::Maximized
    } else if rect_close(frame, f.top) {
        SnapState::TopHalf
    } else if rect_close(frame, f.bottom) {
        SnapState::BottomHalf
    } else if rect_close(frame, f.left) {
        SnapState::LeftHalf
    } else if rect_close(frame, f.right) {
        SnapState::RightHalf
    } else if rect_close(frame, f.top_left) {
        SnapState::TopLeft
    } else if rect_close(frame, f.top_right) {
        SnapState::TopRight
    } else if rect_close(frame, f.bottom_left) {
        SnapState::BottomLeft
    } else if rect_close(frame, f.bottom_right) {
        SnapState::BottomRight
    } else {
        SnapState::Normal
    }
}

/// 找 dir 方向上的相邻屏幕(纯函数,单测覆盖)。
/// 左:完全在当前屏左侧的屏里取最靠右的;右:完全在右侧的屏里取最靠左的。
/// Find the neighbor screen in `dir` (pure; unit-tested). Left: the rightmost screen fully to
/// the left of the current one; Right: the leftmost fully to the right.
pub(crate) fn neighbor_screen(
    screens: &[ScreenGeometry],
    cur: usize,
    dir: Direction,
) -> Option<usize> {
    let c = screens.get(cur)?.frame;
    let by_x = |a: &ScreenGeometry, b: &ScreenGeometry| {
        a.frame
            .x
            .partial_cmp(&b.frame.x)
            .unwrap_or(std::cmp::Ordering::Equal)
    };
    match dir {
        Direction::Left => screens
            .iter()
            .enumerate()
            .filter(|(i, s)| *i != cur && s.frame.x + s.frame.w <= c.x + FRAME_EPSILON)
            .max_by(|a, b| by_x(a.1, b.1))
            .map(|(i, _)| i),
        Direction::Right => screens
            .iter()
            .enumerate()
            .filter(|(i, s)| *i != cur && s.frame.x >= c.x + c.w - FRAME_EPSILON)
            .min_by(|a, b| by_x(a.1, b.1))
            .map(|(i, _)| i),
        _ => None,
    }
}

/// 状态机:当前状态 + 方向 -> 主线程动作(纯函数,单测覆盖)。
/// Windows 语义:普通窗口四方向分别 左半/右半/最大化/最小化;最大化 ↑/↓ 进入全宽上下半屏;
/// 上下半屏可继续上下切换或进入左右半屏;左半屏 ← 继续向左遍历(上一块屏幕的右半屏,
/// 单屏无操作)、→ 右半屏、↑↓ 进入同侧四分屏;四分屏 ↑↓ 在同侧上下四分屏间移动、
/// 顶部 ↑ 最大化、最底行 ↓ 最小化、←→ 回到对应半屏;最小化时 ↓ 无操作、其余方向先
/// 还原再按普通窗口处理(调用方负责解除最小化)。
///
/// The state machine: current state + direction -> main-thread action (pure; unit-tested).
/// Windows semantics: a normal window snaps left/right, maximizes, or minimizes; maximized
/// Up/Down enter the full-width top/bottom halves; top/bottom halves move vertically or snap
/// left/right; left half keeps traversing leftward on Left (the right half of the previous
/// screen, no-op with a single screen), Right goes to the right half, Up/Down enter same-side
/// quarters; quarters move vertically within their side, Up from top quarters maximizes, Down
/// from bottom quarters minimizes, and Left/Right return to the matching half; a minimized
/// window no-ops on Down and is first restored for other directions (the caller un-minimizes).
pub(crate) fn plan(
    state: SnapState,
    dir: Direction,
    cur_screen: usize,
    screens: &[ScreenGeometry],
) -> Plan {
    // 当前屏可视区;screens 为空时返回零矩形(上游保证非空,这里只做纯函数防御)。
    // The current screen's visible area; zero rect when screens is empty (upstream guarantees
    // non-empty; pure-function defensiveness only).
    let visible = |i: usize| {
        screens.get(i).map(|s| s.visible).unwrap_or(AxRect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        })
    };
    let f = |i: usize| snap_frames(visible(i));
    let normal_plan = |dir| match dir {
        Direction::Left => Plan::Move(f(cur_screen).left),
        Direction::Right => Plan::Move(f(cur_screen).right),
        Direction::Up => Plan::Maximize(f(cur_screen).max),
        Direction::Down => Plan::Minimize,
    };
    match state {
        SnapState::Normal => normal_plan(dir),
        SnapState::Maximized => match dir {
            Direction::Left => Plan::Move(f(cur_screen).left),
            Direction::Right => Plan::Move(f(cur_screen).right),
            Direction::Up => Plan::Move(f(cur_screen).top),
            Direction::Down => Plan::Move(f(cur_screen).bottom),
        },
        SnapState::TopHalf => match dir {
            Direction::Left => Plan::Move(f(cur_screen).left),
            Direction::Right => Plan::Move(f(cur_screen).right),
            Direction::Up => Plan::Maximize(f(cur_screen).max),
            Direction::Down => Plan::Move(f(cur_screen).bottom),
        },
        SnapState::BottomHalf => match dir {
            Direction::Left => Plan::Move(f(cur_screen).left),
            Direction::Right => Plan::Move(f(cur_screen).right),
            Direction::Up => Plan::Move(f(cur_screen).top),
            Direction::Down => Plan::Minimize,
        },
        SnapState::LeftHalf => match dir {
            // 继续向左遍历:上一块屏幕的右半屏(单屏时无操作)。
            // Keep traversing leftward: the right half of the previous screen (no-op on one
            // screen).
            Direction::Left => neighbor_screen(screens, cur_screen, Direction::Left)
                .map_or(Plan::Nothing, |i| Plan::Move(f(i).right)),
            Direction::Right => Plan::Move(f(cur_screen).right),
            Direction::Up => Plan::Move(f(cur_screen).top_left),
            Direction::Down => Plan::Move(f(cur_screen).bottom_left),
        },
        SnapState::RightHalf => match dir {
            Direction::Left => Plan::Move(f(cur_screen).left),
            // 继续向右遍历:下一块屏幕的左半屏(单屏时无操作)。
            // Keep traversing rightward: the left half of the next screen (no-op on one screen).
            Direction::Right => neighbor_screen(screens, cur_screen, Direction::Right)
                .map_or(Plan::Nothing, |i| Plan::Move(f(i).left)),
            Direction::Up => Plan::Move(f(cur_screen).top_right),
            Direction::Down => Plan::Move(f(cur_screen).bottom_right),
        },
        SnapState::TopLeft => match dir {
            Direction::Left => Plan::Move(f(cur_screen).left),
            Direction::Right => Plan::Move(f(cur_screen).right),
            Direction::Up => Plan::Maximize(f(cur_screen).max),
            Direction::Down => Plan::Move(f(cur_screen).bottom_left),
        },
        SnapState::TopRight => match dir {
            Direction::Left => Plan::Move(f(cur_screen).left),
            Direction::Right => Plan::Move(f(cur_screen).right),
            Direction::Up => Plan::Maximize(f(cur_screen).max),
            Direction::Down => Plan::Move(f(cur_screen).bottom_right),
        },
        SnapState::BottomLeft => match dir {
            Direction::Left => Plan::Move(f(cur_screen).left),
            Direction::Right => Plan::Move(f(cur_screen).right),
            Direction::Up => Plan::Move(f(cur_screen).top_left),
            Direction::Down => Plan::Minimize,
        },
        SnapState::BottomRight => match dir {
            Direction::Left => Plan::Move(f(cur_screen).left),
            Direction::Right => Plan::Move(f(cur_screen).right),
            Direction::Up => Plan::Move(f(cur_screen).top_right),
            Direction::Down => Plan::Minimize,
        },
        // 最小化窗口:↓ 保持最小化;其余方向按普通窗口处理(已先解除最小化)。
        // Minimized: Down stays put; other directions act as normal (already un-minimized).
        SnapState::Minimized => match dir {
            Direction::Down => Plan::Nothing,
            _ => normal_plan(dir),
        },
    }
}

/// 找包含窗口中心的屏幕;不在任何屏内时取中心距离最近的屏。
/// Find the screen containing the window center; fall back to the nearest center.
fn screen_index_for(frame: AxRect, screens: &[ScreenGeometry]) -> usize {
    let (cx, cy) = frame.center();
    if let Some(i) = screens.iter().position(|s| s.frame.contains_point(cx, cy)) {
        return i;
    }
    let mut best = 0usize;
    let mut best_d = f64::MAX;
    for (i, s) in screens.iter().enumerate() {
        let (sx, sy) = s.frame.center();
        let d = (cx - sx) * (cx - sx) + (cy - sy) * (cy - sy);
        if d < best_d {
            best_d = d;
            best = i;
        }
    }
    best
}

fn direction_enabled(dir: Direction) -> bool {
    crate::config::CONFIG
        .read()
        .map(|c| {
            c.window_control.enabled
                && match dir {
                    Direction::Up => c.window_control.up,
                    Direction::Down => c.window_control.down,
                    Direction::Left => c.window_control.left,
                    Direction::Right => c.window_control.right,
                }
        })
        .unwrap_or(false)
}

/// 主线程:执行一次窗口控制(bridge 投递过来的方向)。
/// Main thread: run one window-control step (a direction delivered by the bridge).
pub(crate) fn apply_direction(dir: Direction) {
    // 事件可能排队到功能关闭之后才被主线程执行,先复核开关。
    // The event may land on the main thread after the feature was switched off; re-check.
    if !direction_enabled(dir) {
        return;
    }
    let (app_name, pid) = crate::ffi::frontmost_app_info();
    // 无前台应用,或前台就是我们自己(设置窗口的文本框保留 Option+方向键原语义)。
    // No frontmost app, or the frontmost app is ourselves (our settings text fields keep the
    // move-by-word semantics of Option+arrows).
    if pid <= 0 || pid == std::process::id() as i32 {
        return;
    }
    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return;
        }
        // 300ms 超时:目标 App 无响应时不卡主线程(切换路径用 50ms,这里动作更重)。
        // 300ms timeout so an unresponsive target app cannot stall the main thread (the
        // switcher uses 50ms; these actions are heavier).
        AXUIElementSetMessagingTimeout(app, 0.3);
        let win = copy_attribute(app, K_AX_FOCUSED_WINDOW);
        CFRelease(app);
        let Some(win) = win else {
            log_debug!("[winctl] no focused window for pid {}", pid);
            return;
        };
        // 全屏窗口跳过(原生全屏有自己的空间管理,设置 frame 无意义)。
        // Skip fullscreen windows (native fullscreen manages its own space; setting frames is
        // meaningless there).
        if let Some(subrole) = copy_string(win, K_AX_SUBROLE) {
            let fullscreen = subrole == K_AX_SUBROLE_FULL_SCREEN;
            if fullscreen {
                log_debug!("[winctl] skip fullscreen window");
                CFRelease(win);
                return;
            }
        }
        let cgwid = ax_window_cgwid(win);
        let Some(frame) = read_frame(win) else {
            log_debug!("[winctl] failed to read window frame");
            CFRelease(win);
            return;
        };
        let minimized = read_bool(win, K_AX_MINIMIZED).unwrap_or(false);
        let screens = screens_in_ax_space();
        if screens.is_empty() {
            CFRelease(win);
            return;
        }
        let cur_screen = screen_index_for(frame, &screens);
        let state = if minimized {
            SnapState::Minimized
        } else {
            infer_state(frame, screens[cur_screen].visible)
        };
        // 最小化 + 非 ↓:先解除最小化,再按普通窗口的目标执行。
        // Minimized + not Down: un-minimize first, then act as a normal window.
        let effective = match state {
            SnapState::Minimized if dir != Direction::Down => {
                set_minimized(win, false);
                SnapState::Normal
            }
            other => other,
        };
        let p = plan(effective, dir, cur_screen, &screens);
        // 诊断:方向、前台 pid、推断状态与最终计划(dev 日志,便于排查个别 App 拒写)。
        // Diagnostics: direction, front pid, inferred state and the final plan (debug log;
        // helps triage per-app write refusals).
        log_debug!(
            "[winctl] app={:?} dir={:?} pid={} cgwid={:?} frame={:?} state={:?} plan={:?}",
            app_name,
            dir,
            pid,
            cgwid,
            frame,
            effective,
            p
        );
        execute(p, win, dir);
        CFRelease(win);
    }
}

/// 快捷操作:读取指定进程焦点窗口的 CGWindowID(无焦点窗口 / 读取失败时 None)。
/// 用于把 Cmd+N 刚创建的新窗口与旧窗口区分开。
/// Quick actions: read the CGWindowID of the process's focused window (None when it has no
/// focused window or the read fails). Used to tell the freshly created window from the old.
pub(crate) fn focused_cgwid_of_pid(pid: i32) -> Option<u32> {
    if pid <= 0 {
        return None;
    }
    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return None;
        }
        AXUIElementSetMessagingTimeout(app, 0.3);
        let win = copy_attribute(app, K_AX_FOCUSED_WINDOW);
        CFRelease(app);
        let cgwid = win.and_then(|w| ax_window_cgwid(w));
        if let Some(w) = win {
            CFRelease(w);
        }
        cgwid
    }
}

/// 快捷操作:把指定进程的焦点窗口最大化(等效绿色缩放按钮,非全屏)。
/// 不经过窗口控制总开关;已是最大化/原生全屏的窗口原样保留(Option+E 连按不抖动)。
/// 返回是否找到并处理了焦点窗口。
///
/// Quick actions: maximize the process's focused window (zoom, NOT fullscreen). Bypasses the
/// window-control master switch; already-maximized / native-fullscreen windows are left as
/// they are (repeated Option+E never flickers). Returns whether a focused window was found
/// and handled.
pub(crate) fn maximize_focused_window_of_pid(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return false;
        }
        AXUIElementSetMessagingTimeout(app, 0.3);
        let Some(win) = copy_attribute(app, K_AX_FOCUSED_WINDOW) else {
            CFRelease(app);
            return false;
        };
        // 原生全屏窗口跳过(全屏有自己的空间管理,设置 frame 无意义),视为已处理。
        // Skip native fullscreen windows (they manage their own space); treat as handled.
        if let Some(subrole) = copy_string(win, K_AX_SUBROLE) {
            if subrole == K_AX_SUBROLE_FULL_SCREEN {
                CFRelease(win);
                CFRelease(app);
                return true;
            }
        }
        let mut handled = false;
        if let Some(frame) = read_frame(win) {
            let screens = screens_in_ax_space();
            if !screens.is_empty() {
                let cur = screen_index_for(frame, &screens);
                if infer_state(frame, screens[cur].visible) != SnapState::Maximized {
                    let target = snap_frames(screens[cur].visible).max;
                    if !set_frame(win, target) {
                        // AX 精确写被拒时退回原生缩放按钮(与窗口控制同策略)。
                        // Fall back to the native zoom button when the exact AX write is
                        // rejected (same policy as window control).
                        press_native_zoom(win);
                    }
                }
                handled = true;
            }
        }
        CFRelease(win);
        CFRelease(app);
        handled
    }
}

/// 执行主线程动作(AX 调用;错误只记 debug 日志,不打断流程)。
/// Run a main-thread plan (AX calls; errors are debug-logged and never interrupt the flow).
unsafe fn execute(plan: Plan, win: AXUIElementRef, dir: Direction) {
    match plan {
        Plan::Move(r) => {
            let _ = set_frame(win, r);
        }
        Plan::Maximize(r) => {
            if !set_frame(win, r) {
                // 某些 App 会接受 AXPosition 却拒绝 AXSize;此时原生缩放按钮仍能完成
                // 系统级最大化,避免出现“窗口只移到顶端、底部没铺满”的半成功状态。
                // Some apps accept AXPosition but reject AXSize; the native zoom button can
                // still perform the system-level maximize and avoids a position-only result.
                log_debug!("[winctl] exact maximize frame rejected; trying native zoom fallback");
                let zoom_err = press_native_zoom(win);
                if zoom_err != K_AX_SUCCESS {
                    log_info!("[winctl] native zoom fallback failed: {}", zoom_err);
                }
            }
        }
        Plan::Minimize => set_minimized(win, true),
        Plan::Nothing => {
            log_debug!("[winctl] direction {:?} is a no-op for this state", dir);
        }
    }
}

// ========== AX 读写 helper / AX read/write helpers ==========

/// 读取一个 AX 属性值(CF 对象,+1 引用,调用方 CFRelease)。
/// Copy an AX attribute value (+1 CF reference; caller CFReleases).
unsafe fn copy_attribute(element: AXUIElementRef, name: &str) -> Option<*const c_void> {
    let key = cf_string_new(name);
    let mut value: *const c_void = std::ptr::null();
    let err = AXUIElementCopyAttributeValue(element, key, &mut value);
    CFRelease(key);
    if err == K_AX_SUCCESS && !value.is_null() {
        Some(value)
    } else {
        None
    }
}

/// 读取字符串属性(CFString -> Rust String;+1 引用由本函数释放)。
/// Read a string attribute (CFString -> Rust String; the +1 reference is released here).
unsafe fn copy_string(element: AXUIElementRef, name: &str) -> Option<String> {
    let v = copy_attribute(element, name)?;
    let s = cf_to_rust_string(v);
    CFRelease(v);
    s
}

/// 读取布尔属性(CFBoolean)。
/// Read a boolean attribute (CFBoolean).
unsafe fn read_bool(element: AXUIElementRef, name: &str) -> Option<bool> {
    let v = copy_attribute(element, name)?;
    let b = CFBooleanGetValue(v);
    CFRelease(v);
    Some(b)
}

/// 读窗口 frame(AXPosition + AXSize)。
/// Read the window frame (AXPosition + AXSize).
unsafe fn read_frame(win: AXUIElementRef) -> Option<AxRect> {
    let pos = copy_attribute(win, K_AX_POSITION)?;
    let mut pt = CgPoint { x: 0.0, y: 0.0 };
    let ok = AXValueGetValue(
        pos,
        K_AX_VALUE_CG_POINT,
        &mut pt as *mut CgPoint as *mut c_void,
    );
    CFRelease(pos);
    if !ok {
        return None;
    }
    let size = copy_attribute(win, K_AX_SIZE)?;
    let mut sz = CgSize { w: 0.0, h: 0.0 };
    let ok = AXValueGetValue(
        size,
        K_AX_VALUE_CG_SIZE,
        &mut sz as *mut CgSize as *mut c_void,
    );
    CFRelease(size);
    if !ok {
        return None;
    }
    Some(AxRect {
        x: pt.x,
        y: pt.y,
        w: sz.w,
        h: sz.h,
    })
}

/// 写一个 AXValue 属性,返回 AX 错误码(AXValueCreate 失败按 -1 报告)。
/// Set one AXValue attribute, returning the AX error (AXValueCreate failure reports -1).
unsafe fn set_ax_value(
    win: AXUIElementRef,
    name: &str,
    value_type: i32,
    bytes: *const c_void,
) -> AXError {
    let key = cf_string_new(name);
    let value = AXValueCreate(value_type, bytes);
    let err: AXError = if value.is_null() {
        -1
    } else {
        let e = AXUIElementSetAttributeValue(win, key, value);
        CFRelease(value);
        e
    };
    CFRelease(key);
    err
}

/// 写窗口 frame:先位置后尺寸,避免放大时暂时越过屏幕边界;失败时再用反向顺序重试。
/// Set the window frame position-first to avoid transient off-screen overflow while growing;
/// retry in the reverse order if either AX write is rejected.
unsafe fn set_frame(win: AXUIElementRef, r: AxRect) -> bool {
    let sz = CgSize { w: r.w, h: r.h };
    let pt = CgPoint { x: r.x, y: r.y };
    // 先移动再放大,确保扩展后的窗口不会因为暂时越过屏幕边界而被 App 拒绝。
    // Move first, then grow, so the enlarged window does not temporarily cross a screen edge
    // and get rejected by the target app.
    let mut pos_err = set_ax_value(
        win,
        K_AX_POSITION,
        K_AX_VALUE_CG_POINT,
        &pt as *const CgPoint as *const c_void,
    );
    let mut size_err = set_ax_value(
        win,
        K_AX_SIZE,
        K_AX_VALUE_CG_SIZE,
        &sz as *const CgSize as *const c_void,
    );
    if pos_err != K_AX_SUCCESS || size_err != K_AX_SUCCESS {
        // 反向顺序再试一次:Electron 等 App 对 AXPosition/AXSize 的接受顺序不一致。
        // Retry in the opposite order: Electron-based apps differ in which AX write order they
        // accept.
        size_err = set_ax_value(
            win,
            K_AX_SIZE,
            K_AX_VALUE_CG_SIZE,
            &sz as *const CgSize as *const c_void,
        );
        pos_err = set_ax_value(
            win,
            K_AX_POSITION,
            K_AX_VALUE_CG_POINT,
            &pt as *const CgPoint as *const c_void,
        );
        if size_err == K_AX_SUCCESS {
            // 尺寸成功后再补一次位置,修正 App 在 resize 时对窗口位置的自动调整。
            // Re-apply position after a successful resize because some apps reposition the
            // window while changing its size.
            pos_err = set_ax_value(
                win,
                K_AX_POSITION,
                K_AX_VALUE_CG_POINT,
                &pt as *const CgPoint as *const c_void,
            );
        }
    }
    if size_err != K_AX_SUCCESS {
        log_debug!("[winctl] set AXSize failed: {} target={:?}", size_err, r);
    }
    if pos_err != K_AX_SUCCESS {
        log_debug!("[winctl] set AXPosition failed: {} target={:?}", pos_err, r);
    }
    let Some(actual) = read_frame(win) else {
        log_info!(
            "[winctl] unable to verify frame after AX write; target={:?}",
            r
        );
        return false;
    };
    let matches = rect_close(actual, r);
    if !matches {
        log_info!(
            "[winctl] frame mismatch after AX write: target={:?} actual={:?}",
            r,
            actual
        );
    }
    matches
}

/// 设置 AXMinimized。
/// Set AXMinimized.
unsafe fn set_minimized(win: AXUIElementRef, minimized: bool) {
    let key = cf_string_new(K_AX_MINIMIZED);
    // AXMinimized 只接受 kCFBooleanTrue/False 常量。
    // AXMinimized only accepts the kCFBooleanTrue/False constants.
    let value = if minimized {
        kCFBooleanTrue
    } else {
        kCFBooleanFalse
    };
    let err = AXUIElementSetAttributeValue(win, key, value);
    if err != K_AX_SUCCESS {
        log_info!("[winctl] set AXMinimized failed: {}", err);
    }
    CFRelease(key);
}

/// 按一次原生缩放按钮(无 snap 前记录时的恢复兜底)。
/// Press the native zoom button once (restore fallback when nothing was recorded pre-snap).
unsafe fn press_native_zoom(win: AXUIElementRef) -> AXError {
    let Some(btn) = copy_attribute(win, K_AX_ZOOM_BUTTON) else {
        log_info!("[winctl] AXZoomButton unavailable");
        return -1;
    };
    let action = cf_string_new(K_AX_PRESS);
    let err = AXUIElementPerformAction(btn, action);
    CFRelease(action);
    CFRelease(btn);
    if err != K_AX_SUCCESS {
        log_info!("[winctl] AXPress on zoom button failed: {}", err);
    }
    err
}

/// 枚举屏幕并把 frame/visibleFrame 换算到 AX 坐标(主线程调用:NSScreen 仅主线程安全)。
/// Cocoa 全局坐标是主屏左下原点、y 向上;AX 是主屏左上原点、y 向下。换算用主屏 Cocoa 高度。
/// Enumerate screens and convert frame/visibleFrame into AX coordinates (main-thread only:
/// NSScreen is main-thread-only). Cocoa's global space is primary-bottom-left origin, y up;
/// AX's is primary-top-left origin, y down. The conversion uses the primary screen's Cocoa
/// height.
unsafe fn screens_in_ax_space() -> Vec<ScreenGeometry> {
    let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
    if screens.is_null() {
        return Vec::new();
    }
    let count: usize = msg_send![screens, count];
    let mut out = Vec::with_capacity(count);
    if count == 0 {
        return out;
    }
    let primary: *mut AnyObject = msg_send![screens, objectAtIndex: 0isize];
    let pf: NSRect = msg_send![primary, frame];
    let primary_top = pf.origin.y + pf.size.height;
    for i in 0..count {
        let s: *mut AnyObject = msg_send![screens, objectAtIndex: i as isize];
        let f: NSRect = msg_send![s, frame];
        let v: NSRect = msg_send![s, visibleFrame];
        out.push(ScreenGeometry {
            frame: cocoa_to_ax(f, primary_top),
            visible: cocoa_to_ax(v, primary_top),
        });
    }
    out
}

fn cocoa_to_ax(r: NSRect, primary_top: f64) -> AxRect {
    AxRect {
        x: r.origin.x,
        y: primary_top - r.origin.y - r.size.height,
        w: r.size.width,
        h: r.size.height,
    }
}

// ========== event tap 与线程管理 / event tap and thread management ==========
// 结构与 mouse/event_tap.rs 相同:专用线程 + RunLoop 引用 + 停止标志。
// Same shape as mouse/event_tap.rs: dedicated thread + RunLoop reference + stop flag.

struct RunLoopMutex(Mutex<Option<CFRunLoopRef>>);
unsafe impl Send for RunLoopMutex {}
unsafe impl Sync for RunLoopMutex {}
static RUNLOOP: OnceLock<RunLoopMutex> = OnceLock::new();
static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);
static WC_THREAD: Mutex<Option<thread::JoinHandle<()>>> = Mutex::new(None);

fn runloop_static() -> &'static Mutex<Option<CFRunLoopRef>> {
    &RUNLOOP.get_or_init(|| RunLoopMutex(Mutex::new(None))).0
}

/// tap 回调:只关心 Option+方向键(不带其他修饰键)。启用时吞掉 keyDown/keyUp 并把
/// 非自动重复的 keyDown 投递给主线程;关闭时全部透传(功能关闭 = 组合键还给系统)。
/// 自己是前台 App 时也透传,设置窗口文本框的按词移动不受影响。
///
/// The tap callback: only cares about Option+arrows (no extra modifiers). When enabled it
/// swallows matching keyDown/keyUp and forwards non-autorepeat keyDowns to the main thread;
/// when disabled everything passes through (a disabled feature returns the combo to the
/// system). Also passes through when we are the frontmost app, keeping move-by-word intact in
/// our settings text fields.
unsafe extern "C" fn window_control_tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    _user_info: *mut c_void,
) -> CGEventRef {
    if event_type != K_CG_EVENT_KEY_DOWN && event_type != K_CG_EVENT_KEY_UP {
        return event;
    }
    let keycode = CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_KEYCODE) as u16;
    let Some(dir) = Direction::from_keycode(keycode) else {
        return event;
    };
    let flags = CGEventGetFlags(event);
    if flags & K_FLAG_OPTION == 0 || flags & (K_FLAG_COMMAND | K_FLAG_SHIFT | K_FLAG_CONTROL) != 0 {
        return event;
    }
    // 本应用合成的组合键(鼠标映射 Key Press post 到 HID 层后会回到 session tap):
    // 必须透传,否则映射了 Option+方向键的侧键会被这里劫持。
    // Our own synthesized combos (mouse Key Press mappings post at HID level and loop back
    // into session taps) must pass through, or a side button mapped to Option+arrow gets
    // hijacked here.
    if CGEventGetIntegerValueField(event, K_CG_EVENT_SOURCE_USER_DATA) == SYNTHETIC_MARKER {
        return event;
    }
    if !direction_enabled(dir) {
        return event;
    }
    let (_name, pid) = crate::ffi::frontmost_app_info();
    if pid == std::process::id() as i32 {
        return event;
    }
    if event_type == K_CG_EVENT_KEY_DOWN {
        // 忽略系统自动重复:按住不放会在状态间往返弹跳,只响应实体按键。
        // Ignore system autorepeat: holding the key would ping-pong between states; only
        // physical presses act.
        let autorepeat = CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_AUTOREPEAT);
        if autorepeat == 0 {
            log_debug!("[winctl] keyDown Option+{:?}", dir);
            if let Some(tx) = crate::STATUS_EVENT_TX.get() {
                let _ = tx.send(GlobalEvent::WindowControl(dir));
            } else {
                log_info!(
                    "[winctl] keyDown Option+{:?} dropped: event bridge unavailable",
                    dir
                );
            }
        }
    }
    // 吞掉匹配的 keyDown/keyUp(含自动重复),应用看不到这组组合键。
    // Swallow matching keyDown/keyUp (autorepeat included); apps never see the combo.
    std::ptr::null_mut()
}

/// 运行时启用窗口控制(设置页热切换 / 启动路径共用)。幂等。
/// Enable window control at runtime (shared by the settings hot-switch and the startup path).
/// Idempotent.
pub(crate) fn start() {
    let mut guard = WC_THREAD.lock().unwrap();
    if guard.as_ref().is_some_and(|h| !h.is_finished()) {
        return;
    }
    *guard = Some(spawn_tap_thread());
    log_info!("Window control enabled.");
}

/// 运行时停用窗口控制(设置页热切换)。幂等。
/// Disable window control at runtime (settings hot-switch). Idempotent.
pub(crate) fn stop() {
    STOP_REQUESTED.store(true, Ordering::Relaxed);
    let rl = runloop_static().lock().unwrap().take();
    if let Some(rl) = rl {
        unsafe {
            event_tap::CFRunLoopStop(rl);
        }
    }
    let handle = WC_THREAD.lock().unwrap().take();
    if let Some(h) = handle {
        let _ = h.join();
    }
    log_info!("Window control disabled.");
}

fn spawn_tap_thread() -> thread::JoinHandle<()> {
    // 监听掩码:keyDown + keyUp。
    // Listen mask: keyDown + keyUp.
    let mask: CGEventMask = (1u64 << K_CG_EVENT_KEY_DOWN) | (1u64 << K_CG_EVENT_KEY_UP);
    thread::spawn(move || unsafe {
        crate::performance::set_current_thread_qos(crate::performance::ThreadQos::UserInteractive);
        // 新线程首件事:清掉上次运行残留的停止标志。
        // First thing in the new thread: clear any stale stop flag.
        STOP_REQUESTED.store(false, Ordering::Relaxed);
        // session 层 tap:与切换器同层,能拦截真实硬件按键;DEFAULT_TAP 才能吞事件。
        // Session-level tap: same layer as the switcher, sees real hardware keys; DEFAULT_TAP
        // is required to swallow events.
        let tap = event_tap::create_tap_with_retry(
            tap_location::SESSION_EVENT_TAP,
            tap_placement::HEAD_INSERT,
            tap_options::DEFAULT_TAP,
            mask,
            Some(window_control_tap_callback),
            std::ptr::null_mut(),
            "winctl",
            Some(&STOP_REQUESTED),
        );
        let tap = match tap {
            Some(t) => t,
            None => return,
        };
        let rl = CFRunLoopGetCurrent();
        let source = event_tap::CFMachPortCreateRunLoopSource(std::ptr::null_mut(), tap, 0);
        event_tap::CFRunLoopAddSource(rl, source, event_tap::kCFRunLoopDefaultMode);
        event_tap::CGEventTapEnable(tap, true);
        // 存入 RunLoop 后复查停止标志,关闭“存入后、run 前置位”的竞态窗口。
        // Re-check the stop flag after storing the RunLoop to close the store-vs-run race.
        *runloop_static().lock().unwrap() = Some(rl);
        if !STOP_REQUESTED.load(Ordering::Relaxed) {
            log_debug!("Window control event tap started.");
            event_tap::CFRunLoopRun();
        }
        *runloop_static().lock().unwrap() = None;
    })
}

#[cfg(test)]
mod tests {
    use super::{
        infer_state, neighbor_screen, plan, snap_frames, AxRect, Direction, Plan, ScreenGeometry,
        SnapState,
    };

    fn rect(x: f64, y: f64, w: f64, h: f64) -> AxRect {
        AxRect { x, y, w, h }
    }

    /// 三块屏幕:主屏 (0,0),左侧屏 x=-1920,右侧屏 x=1920。
    /// Three screens: primary at (0,0), a left one at x=-1920, a right one at x=1920.
    fn screens() -> Vec<ScreenGeometry> {
        let mk = |x: f64| ScreenGeometry {
            frame: rect(x, 0.0, 1920.0, 1112.0),
            visible: rect(x, 25.0, 1920.0, 1055.0),
        };
        vec![mk(-1920.0), mk(0.0), mk(1920.0)]
    }

    #[test]
    fn snap_frames_split_the_visible_area() {
        let v = rect(0.0, 25.0, 1920.0, 1055.0);
        let f = snap_frames(v);
        assert_eq!(f.left, rect(0.0, 25.0, 960.0, 1055.0));
        assert_eq!(f.right, rect(960.0, 25.0, 960.0, 1055.0));
        assert_eq!(f.top, rect(0.0, 25.0, 1920.0, 527.5));
        assert_eq!(f.bottom, rect(0.0, 25.0 + 527.5, 1920.0, 527.5));
        assert_eq!(f.top_left, rect(0.0, 25.0, 960.0, 527.5));
        assert_eq!(f.bottom_right, rect(960.0, 25.0 + 527.5, 960.0, 527.5));
        assert_eq!(f.max, v);
    }

    #[test]
    fn infer_state_matches_each_target() {
        let v = rect(0.0, 25.0, 1920.0, 1055.0);
        let f = snap_frames(v);
        assert_eq!(infer_state(f.max, v), SnapState::Maximized);
        assert_eq!(infer_state(f.top, v), SnapState::TopHalf);
        assert_eq!(infer_state(f.bottom, v), SnapState::BottomHalf);
        assert_eq!(infer_state(f.left, v), SnapState::LeftHalf);
        assert_eq!(infer_state(f.right, v), SnapState::RightHalf);
        assert_eq!(infer_state(f.top_left, v), SnapState::TopLeft);
        assert_eq!(infer_state(f.top_right, v), SnapState::TopRight);
        assert_eq!(infer_state(f.bottom_left, v), SnapState::BottomLeft);
        assert_eq!(infer_state(f.bottom_right, v), SnapState::BottomRight);
        // 1pt 内的微调仍应匹配(部分 App 会微调)。
        // Sub-pt nudges must still match (some apps adjust).
        assert_eq!(
            infer_state(rect(f.left.x + 1.0, f.left.y, f.left.w, f.left.h), v),
            SnapState::LeftHalf
        );
        // 普通窗口。
        // A normal window.
        assert_eq!(
            infer_state(rect(100.0, 100.0, 800.0, 600.0), v),
            SnapState::Normal
        );
    }

    #[test]
    fn normal_window_follows_windows_semantics() {
        let screens = screens();
        let f = snap_frames(screens[1].visible);
        // 普通:← 左半 → 右半 ↑ 最大化 ↓ 最小化。
        // Normal: Left/right halves, Up maximizes, Down minimizes.
        assert_eq!(
            plan(SnapState::Normal, Direction::Left, 1, &screens),
            Plan::Move(f.left)
        );
        assert_eq!(
            plan(SnapState::Normal, Direction::Right, 1, &screens),
            Plan::Move(f.right)
        );
        assert_eq!(
            plan(SnapState::Normal, Direction::Up, 1, &screens),
            Plan::Maximize(f.max)
        );
        assert_eq!(
            plan(SnapState::Normal, Direction::Down, 1, &screens),
            Plan::Minimize
        );
    }

    #[test]
    fn maximized_moves_to_vertical_halves() {
        let screens = screens();
        let f = snap_frames(screens[1].visible);
        assert_eq!(
            plan(SnapState::Maximized, Direction::Up, 1, &screens),
            Plan::Move(f.top)
        );
        assert_eq!(
            plan(SnapState::Maximized, Direction::Down, 1, &screens),
            Plan::Move(f.bottom)
        );
        assert_eq!(
            plan(SnapState::Maximized, Direction::Left, 1, &screens),
            Plan::Move(f.left)
        );
        assert_eq!(
            plan(SnapState::Maximized, Direction::Right, 1, &screens),
            Plan::Move(f.right)
        );
        assert_eq!(
            plan(SnapState::TopHalf, Direction::Up, 1, &screens),
            Plan::Maximize(f.max)
        );
        assert_eq!(
            plan(SnapState::TopHalf, Direction::Down, 1, &screens),
            Plan::Move(f.bottom)
        );
        assert_eq!(
            plan(SnapState::BottomHalf, Direction::Up, 1, &screens),
            Plan::Move(f.top)
        );
    }

    #[test]
    fn left_half_traverses_to_the_previous_screen() {
        let screens = screens();
        let f_prev = snap_frames(screens[0].visible);
        let f_cur = snap_frames(screens[1].visible);
        // 左半屏 ← -> 上一屏(x=-1920)的右半屏;→ -> 本屏右半屏。
        // Left half + Left -> the previous screen's (x=-1920) right half; Right -> this
        // screen's right half.
        assert_eq!(
            plan(SnapState::LeftHalf, Direction::Left, 1, &screens),
            Plan::Move(f_prev.right)
        );
        assert_eq!(
            plan(SnapState::LeftHalf, Direction::Right, 1, &screens),
            Plan::Move(f_cur.right)
        );
        assert_eq!(
            plan(SnapState::LeftHalf, Direction::Up, 1, &screens),
            Plan::Move(f_cur.top_left)
        );
        assert_eq!(
            plan(SnapState::LeftHalf, Direction::Down, 1, &screens),
            Plan::Move(f_cur.bottom_left)
        );
        // 单屏时 ← 无操作。
        // Single screen: Left is a no-op.
        let single = vec![screens[1]];
        assert_eq!(
            plan(SnapState::LeftHalf, Direction::Left, 0, &single),
            Plan::Nothing
        );
    }

    #[test]
    fn right_half_traverses_to_the_next_screen() {
        let screens = screens();
        let f_next = snap_frames(screens[2].visible);
        let f_cur = snap_frames(screens[1].visible);
        assert_eq!(
            plan(SnapState::RightHalf, Direction::Right, 1, &screens),
            Plan::Move(f_next.left)
        );
        assert_eq!(
            plan(SnapState::RightHalf, Direction::Left, 1, &screens),
            Plan::Move(f_cur.left)
        );
        assert_eq!(
            plan(SnapState::RightHalf, Direction::Up, 1, &screens),
            Plan::Move(f_cur.top_right)
        );
        assert_eq!(
            plan(SnapState::RightHalf, Direction::Down, 1, &screens),
            Plan::Move(f_cur.bottom_right)
        );
    }

    #[test]
    fn quarters_move_vertically_and_return_to_halves() {
        let screens = screens();
        let f = snap_frames(screens[1].visible);
        // 左上:↑ 最大化 ↓ 左下;→ 右半。
        // Top-left: Up maximizes, Down to bottom-left; Right returns to the right half.
        assert_eq!(
            plan(SnapState::TopLeft, Direction::Up, 1, &screens),
            Plan::Maximize(f.max)
        );
        assert_eq!(
            plan(SnapState::TopLeft, Direction::Down, 1, &screens),
            Plan::Move(f.bottom_left)
        );
        assert_eq!(
            plan(SnapState::TopLeft, Direction::Right, 1, &screens),
            Plan::Move(f.right)
        );
        // 左下:↑ 左上 ↓ 最小化。
        // Bottom-left: Up to top-left, Down minimizes.
        assert_eq!(
            plan(SnapState::BottomLeft, Direction::Up, 1, &screens),
            Plan::Move(f.top_left)
        );
        assert_eq!(
            plan(SnapState::BottomLeft, Direction::Down, 1, &screens),
            Plan::Minimize
        );
        // 右上/右下:↑↓ 在同侧四分屏间移动。
        // Top-right / bottom-right: Up/Down move within the right-side quarters.
        assert_eq!(
            plan(SnapState::BottomRight, Direction::Up, 1, &screens),
            Plan::Move(f.top_right)
        );
        assert_eq!(
            plan(SnapState::TopRight, Direction::Down, 1, &screens),
            Plan::Move(f.bottom_right)
        );
        // 最底行 ↓ 最小化;← 回左半屏。
        // Down from the bottom row minimizes; Left returns to the left half.
        assert_eq!(
            plan(SnapState::BottomRight, Direction::Down, 1, &screens),
            Plan::Minimize
        );
        assert_eq!(
            plan(SnapState::TopRight, Direction::Left, 1, &screens),
            Plan::Move(f.left)
        );
    }

    #[test]
    fn minimized_stays_put_on_down_and_restores_otherwise() {
        let screens = screens();
        let f = snap_frames(screens[1].visible);
        assert_eq!(
            plan(SnapState::Minimized, Direction::Down, 1, &screens),
            Plan::Nothing
        );
        assert_eq!(
            plan(SnapState::Minimized, Direction::Up, 1, &screens),
            Plan::Maximize(f.max)
        );
    }

    #[test]
    fn neighbor_screens_pick_the_adjacent_display() {
        let screens = screens();
        assert_eq!(neighbor_screen(&screens, 1, Direction::Left), Some(0));
        assert_eq!(neighbor_screen(&screens, 1, Direction::Right), Some(2));
        assert_eq!(neighbor_screen(&screens, 0, Direction::Left), None);
        assert_eq!(neighbor_screen(&screens, 2, Direction::Right), None);
        assert_eq!(neighbor_screen(&screens, 1, Direction::Up), None);
    }
}
