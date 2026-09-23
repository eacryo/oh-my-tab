//! 窗口控制模块:Option+方向键模拟 Windows 的 Win+方向键窗口管理,Option+Shift+方向键跨显示器移动。
//! 独立 session 层 event tap(专用线程)拦截 Option+方向键及 Option+Shift+方向键,事件经有界输入聚合器
//! (GlobalEvent -> performSelectorOnMainThread)投递到主线程执行 AX 移动/缩放/最小化。
//! 状态(普通/最大化/上下半屏/左右半屏/四分屏)按当前 frame 与目标矩形匹配推断,无需持久状态;
//! 「原尺寸」在首次从普通状态进入 snap 时按 CGWindowID 记录,供后续恢复逻辑使用。
//!
//! Window control module: Option+arrow keys emulate Windows' Win+arrow window management, while
//! Option+Shift+arrow keys move a window to an adjacent display. A dedicated session-level event
//! tap (own thread) intercepts these combinations; events travel
//! through the bounded input aggregator (GlobalEvent -> performSelectorOnMainThread) and run on the main
//! thread, which moves/resizes/minimizes windows via AX. Snap states (normal/maximized/top-bottom
//! halves/left-right halves/quarters) are inferred by matching the current frame against target
//! rectangles, so nothing is
//! persisted; the "original size" is recorded per CGWindowID when a normal window first snaps.

use objc2::runtime::AnyObject;
use objc2::{class, msg_send, sel};
use objc2_foundation::NSRect;

use crate::event_monitor::GlobalEvent;
use crate::event_tap::{
    self, tap_location, tap_options, tap_placement, CFRunLoopGetCurrent, CGEventGetFlags,
    CGEventGetIntegerValueField, CGEventMask, CGEventRef, CGEventTapProxy, CGEventType,
    K_CG_EVENT_SOURCE_USER_DATA, SYNTHETIC_MARKER,
};
use crate::ffi::{
    kCFBooleanFalse, kCFBooleanTrue, AXError, AXUIElementCopyActionNames,
    AXUIElementCopyAttributeValue, AXUIElementCreateApplication, AXUIElementPerformAction,
    AXUIElementRef, AXUIElementSetAttributeValue, AXUIElementSetMessagingTimeout, AXValueCreate,
    AXValueGetValue, CFArrayGetCount, CFArrayGetValueAtIndex, CFBooleanGetValue, CFRelease,
    K_AX_SUCCESS,
};
use crate::window_collector::{ax_window_cgwid, cf_string_new, cf_to_rust_string};
use crate::{log_debug, log_info};
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::thread;
use std::time::Instant;

// ========== 键盘事件常量 / keyboard event constants ==========
// 见 CGEventTypes.h;键码来自 Carbon HIToolbox Events.h。
// See CGEventTypes.h; keycodes are from Carbon HIToolbox Events.h.
use crate::event_tap::keyboard::{
    EVENT_KEY_DOWN as K_CG_EVENT_KEY_DOWN, EVENT_KEY_UP as K_CG_EVENT_KEY_UP,
    FIELD_AUTOREPEAT as K_CG_KEYBOARD_EVENT_AUTOREPEAT,
    FIELD_KEYCODE as K_CG_KEYBOARD_EVENT_KEYCODE, FLAG_COMMAND as K_FLAG_COMMAND,
    FLAG_CONTROL as K_FLAG_CONTROL, FLAG_OPTION as K_FLAG_OPTION, FLAG_SHIFT as K_FLAG_SHIFT,
    VK_DOWN as K_VK_DOWN, VK_LEFT as K_VK_LEFT, VK_RIGHT as K_VK_RIGHT, VK_UP as K_VK_UP,
};

// ========== AX 属性名与常量 / AX attribute names and constants ==========
const K_AX_FOCUSED_WINDOW: &str = "AXFocusedWindow";
const K_AX_POSITION: &str = "AXPosition";
const K_AX_SIZE: &str = "AXSize";
const K_AX_MINIMIZED: &str = "AXMinimized";
const K_AX_SUBROLE: &str = "AXSubrole";
const K_AX_ZOOM_BUTTON: &str = "AXZoomButton";
const K_AX_PRESS: &str = "AXPress";
// AppKit 挂在缩放按钮上的私有动作名:执行窗口自己的缩放(`performZoom:`),与双击标题栏 /
// Option+点绿钮等价。公开头文件(AXActionConstants.h)里没有对应常量,只能当字符串用。
// Private action name AppKit attaches to the zoom button: performs the window's own zoom
// (`performZoom:`), the same thing double-clicking the title bar or Option-clicking the green
// button does. No public constant exists in AXActionConstants.h, so it stays a literal.
const K_AX_ZOOM_WINDOW: &str = "AXZoomWindow";
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
    /// 最大化到当前屏幕可视区;AX 精确设置失败时由执行层回退到原生缩放动作(`AXZoomWindow`)。
    /// Maximize to the current screen's visible area; execution falls back to the native zoom
    /// action (`AXZoomWindow`) when the AX exact-frame write is rejected.
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

/// 状态推断与最大化验收的容差(点)。App 会把窗口吸附到自己的内部网格——终端按文本行吸附,
/// 写 visibleFrame 得到 915 而不是 923,半屏目标 461.5 实测回来是 464。
/// 写入校验(FRAME_EPSILON)仍按精确值走,但**推断**必须容忍这种网格:否则吸附后的窗口会被判成
/// "普通",于是 ↓ 去最小化、↑ 重新最大化。相邻 snap 目标至少相差 ~230pt,这个容差不会混淆它们。
/// Tolerance for state inference and maximize acceptance, in points. Apps snap windows to an
/// internal grid: Terminal snaps to text rows and lands 915 where visibleFrame is 923, and returns
/// 464 where the half is 461.5. Write *verification* stays exact (FRAME_EPSILON), but *inference*
/// must tolerate that grid -- otherwise a snapped window reads as "normal", making Down minimize it
/// and Up re-maximize it. Adjacent snap targets are at least ~230pt apart, so this cannot confuse
/// them.
const SNAP_EPSILON: f64 = 16.0;

/// 矩形相似比较:每一对对应值都在 eps 内。
/// Rectangular closeness: every corresponding pair of values is within eps.
fn rect_close_with(a: AxRect, b: AxRect, eps: f64) -> bool {
    (a.x - b.x).abs() <= eps
        && (a.y - b.y).abs() <= eps
        && (a.w - b.w).abs() <= eps
        && (a.h - b.h).abs() <= eps
}

/// 窗口是否已铺满可视区(四边各自与 visibleFrame 的偏差都在 SNAP_EPSILON 内;纯函数,单测覆盖)。
/// Whether the window fills the visible area (each of its four edges is within SNAP_EPSILON of
/// visibleFrame's; pure, unit-tested).
fn fills_visible(a: AxRect, v: AxRect) -> bool {
    (a.x - v.x).abs() <= SNAP_EPSILON
        && (a.y - v.y).abs() <= SNAP_EPSILON
        && ((a.x + a.w) - (v.x + v.w)).abs() <= SNAP_EPSILON
        && ((a.y + a.h) - (v.y + v.h)).abs() <= SNAP_EPSILON
}

/// snap 写入的验收规则:最大化按"铺满可视区",其余按网格容差。收尾复核用同一套,避免两处口径漂移。
/// Acceptance for a snap write: "fills the visible area" for maximize, grid tolerance otherwise.
/// The follow-up re-check uses the same predicate so the two never drift apart.
fn snap_accepts(target: AxRect, maximized: bool) -> impl Fn(AxRect) -> bool {
    move |a| {
        if maximized {
            fills_visible(a, target)
        } else {
            rect_close_with(a, target, SNAP_EPSILON)
        }
    }
}

/// 按当前 frame 推断 snap 状态;都不匹配即普通窗口(纯函数,单测覆盖)。
/// Infer the snap state from the current frame; no match means normal (pure; unit-tested).
pub(crate) fn infer_state(frame: AxRect, visible: AxRect) -> SnapState {
    let f = snap_frames(visible);
    // 最大化先用宽松判定:部分 App(如终端)的"最大化"就是比 visibleFrame 短一行,严格匹配会
    // 把已最大化的窗口判成普通,于是每次 Option+↑ 都重新触发一遍最大化。
    // Maximize is matched loosely first: for some apps (Terminal) "maximized" is exactly one row
    // short of visibleFrame, so a strict match would call it Normal and re-trigger maximize on
    // every Option+Up.
    if fills_visible(frame, f.max) {
        SnapState::Maximized
    } else if rect_close_with(frame, f.top, SNAP_EPSILON) {
        SnapState::TopHalf
    } else if rect_close_with(frame, f.bottom, SNAP_EPSILON) {
        SnapState::BottomHalf
    } else if rect_close_with(frame, f.left, SNAP_EPSILON) {
        SnapState::LeftHalf
    } else if rect_close_with(frame, f.right, SNAP_EPSILON) {
        SnapState::RightHalf
    } else if rect_close_with(frame, f.top_left, SNAP_EPSILON) {
        SnapState::TopLeft
    } else if rect_close_with(frame, f.top_right, SNAP_EPSILON) {
        SnapState::TopRight
    } else if rect_close_with(frame, f.bottom_left, SNAP_EPSILON) {
        SnapState::BottomLeft
    } else if rect_close_with(frame, f.bottom_right, SNAP_EPSILON) {
        SnapState::BottomRight
    } else {
        SnapState::Normal
    }
}

/// 找 dir 方向上的相邻屏幕(纯函数,单测覆盖)。
/// 水平按 x 轴、垂直按 y 轴选择完全位于当前屏对应方向的最近屏幕。
/// Find the neighbor screen in `dir` (pure; unit-tested). Select the nearest screen fully in the
/// requested direction, using the x axis horizontally and the y axis vertically.
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
    let by_y = |a: &ScreenGeometry, b: &ScreenGeometry| {
        a.frame
            .y
            .partial_cmp(&b.frame.y)
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
        // AX 坐标 y 向下,因此上方屏幕的底边不超过当前屏顶边。
        // AX coordinates grow downward, so an upper screen ends no lower than the current top.
        Direction::Up => screens
            .iter()
            .enumerate()
            .filter(|(i, s)| *i != cur && s.frame.y + s.frame.h <= c.y + FRAME_EPSILON)
            .max_by(|a, b| by_y(a.1, b.1))
            .map(|(i, _)| i),
        Direction::Down => screens
            .iter()
            .enumerate()
            .filter(|(i, s)| *i != cur && s.frame.y >= c.y + c.h - FRAME_EPSILON)
            .min_by(|a, b| by_y(a.1, b.1))
            .map(|(i, _)| i),
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

/// 将普通窗口平移到目标屏幕,保留相对位置并限制在目标可视区内。
/// Translate a normal window to the target display, preserving relative position and clamping
/// it inside the target visible area.
fn translated_frame(frame: AxRect, from: AxRect, to: AxRect) -> AxRect {
    let w = frame.w.min(to.w);
    let h = frame.h.min(to.h);
    let x = (to.x + frame.x - from.x).clamp(to.x, to.x + to.w - w);
    let y = (to.y + frame.y - from.y).clamp(to.y, to.y + to.h - h);
    AxRect { x, y, w, h }
}

fn display_move_staging_frame(
    state: SnapState,
    frame: AxRect,
    from: AxRect,
    to: AxRect,
    target: AxRect,
) -> AxRect {
    match state {
        SnapState::Normal | SnapState::Minimized => target,
        _ => translated_frame(frame, from, to),
    }
}

/// 计算跨显示器移动后的目标 frame。
/// 最大化及各类 snap 状态在目标屏幕保持同一状态;普通窗口保留大小和相对位置。
/// Compute the target frame for a cross-display move. Maximized and snapped states keep the
/// same state on the destination display; normal windows preserve size and relative position.
pub(crate) fn display_move_target(
    state: SnapState,
    frame: AxRect,
    cur_screen: usize,
    dir: Direction,
    screens: &[ScreenGeometry],
) -> Option<(usize, AxRect)> {
    let target = neighbor_screen(screens, cur_screen, dir)?;
    let from = screens.get(cur_screen)?.visible;
    let to = screens.get(target)?.visible;
    let target_frames = snap_frames(to);
    let target_frame = match state {
        SnapState::Maximized => target_frames.max,
        SnapState::TopHalf => target_frames.top,
        SnapState::BottomHalf => target_frames.bottom,
        SnapState::LeftHalf => target_frames.left,
        SnapState::RightHalf => target_frames.right,
        SnapState::TopLeft => target_frames.top_left,
        SnapState::TopRight => target_frames.top_right,
        SnapState::BottomLeft => target_frames.bottom_left,
        SnapState::BottomRight => target_frames.bottom_right,
        SnapState::Normal | SnapState::Minimized => translated_frame(frame, from, to),
    };
    Some((target, target_frame))
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

fn display_move_enabled(dir: Direction) -> bool {
    crate::config::CONFIG
        .read()
        .map(|c| {
            c.window_control.enabled
                && match dir {
                    Direction::Up => c.window_control.display_up,
                    Direction::Down => c.window_control.display_down,
                    Direction::Left => c.window_control.display_left,
                    Direction::Right => c.window_control.display_right,
                }
        })
        .unwrap_or(false)
}

/// 跨屏 frame 写入后的延迟校验任务。跨屏时 AppKit/目标 App 可能在 AX setter 返回成功后
/// 仍异步调整窗口尺寸，因此任务必须带窗口身份和 token，避免迟到回调改动新窗口。
/// A deferred cross-display frame verification. AppKit/the target app may resize asynchronously
/// after AX setters report success, so the job carries window identity and a token to prevent a
/// late callback from touching a different or newer window.
#[derive(Clone, Copy)]
struct PendingDisplayMove {
    token: u64,
    pid: i32,
    cgwid: u32,
    state: SnapState,
    target_screen_frame: AxRect,
    target_frame: AxRect,
    attempt: u8,
    created_at: Instant,
}

static PENDING_DISPLAY_MOVE: LazyLock<Mutex<Option<PendingDisplayMove>>> =
    LazyLock::new(|| Mutex::new(None));
static NEXT_DISPLAY_MOVE_TOKEN: AtomicU64 = AtomicU64::new(1);
const MAX_DISPLAY_MOVE_RETRIES: u8 = 2;
const DISPLAY_MOVE_PENDING_TTL: std::time::Duration = std::time::Duration::from_millis(500);

fn pending_display_state(
    pid: i32,
    cgwid: Option<u32>,
    current_screen: usize,
    screens: &[ScreenGeometry],
) -> Option<SnapState> {
    let cgwid = cgwid?;
    let job = PENDING_DISPLAY_MOVE.lock().unwrap().as_ref().copied()?;
    if job.pid != pid
        || job.cgwid != cgwid
        || job.created_at.elapsed() > DISPLAY_MOVE_PENDING_TTL
        || !rect_close(job.target_screen_frame, screens.get(current_screen)?.frame)
    {
        return None;
    }
    Some(job.state)
}

fn cancel_pending_display_move(pid: i32, cgwid: Option<u32>) {
    let Some(cgwid) = cgwid else {
        return;
    };
    let mut pending = PENDING_DISPLAY_MOVE.lock().unwrap();
    if pending
        .as_ref()
        .is_some_and(|job| job.pid == pid && job.cgwid == cgwid)
    {
        *pending = None;
    }
}

fn update_pending_attempt(token: u64, attempt: u8) -> bool {
    let mut pending = PENDING_DISPLAY_MOVE.lock().unwrap();
    let Some(job) = pending.as_mut() else {
        return false;
    };
    if job.token != token {
        return false;
    }
    job.attempt = attempt;
    true
}

fn clear_pending_display_move(token: u64) {
    let mut pending = PENDING_DISPLAY_MOVE.lock().unwrap();
    if pending.as_ref().is_some_and(|job| job.token == token) {
        *pending = None;
    }
}

fn schedule_display_move_retry(token: u64, delay: f64) {
    let Some(ctrl) = crate::CONTROLLER.lock().unwrap().map(|target| target.0) else {
        clear_pending_display_move(token);
        return;
    };
    unsafe {
        let token: *mut AnyObject = msg_send![
            class!(NSNumber),
            numberWithUnsignedLongLong: token
        ];
        let _: () = msg_send![
            ctrl,
            performSelector: sel!(handleDisplayMoveRetry:),
            withObject: token,
            afterDelay: delay
        ];
    }
}

/// 延迟校验跨屏 frame；只在主线程执行，且最多进行两次重试。
/// Verify a cross-display frame after a delay on the main thread, with at most two retries.
pub(crate) fn on_display_move_retry(arg: *mut c_void) {
    if arg.is_null() {
        return;
    }
    let token: u64 = unsafe { msg_send![arg as *mut AnyObject, unsignedLongLongValue] };
    let Some(job) = PENDING_DISPLAY_MOVE.lock().unwrap().as_ref().copied() else {
        return;
    };
    if job.token != token {
        return;
    }

    unsafe {
        let app = AXUIElementCreateApplication(job.pid);
        if app.is_null() {
            clear_pending_display_move(token);
            return;
        }
        AXUIElementSetMessagingTimeout(app, 0.3);
        let win = copy_attribute(app, K_AX_FOCUSED_WINDOW);
        CFRelease(app);
        let Some(win) = win else {
            clear_pending_display_move(token);
            return;
        };
        let same_window = ax_window_cgwid(win) == Some(job.cgwid);
        let is_fullscreen = copy_string(win, K_AX_SUBROLE)
            .is_some_and(|subrole| subrole == K_AX_SUBROLE_FULL_SCREEN);
        let Some(actual) = read_frame(win) else {
            CFRelease(win);
            clear_pending_display_move(token);
            return;
        };
        let screens = screens_in_ax_space();
        let target_exists = screens
            .iter()
            .any(|screen| rect_close(screen.frame, job.target_screen_frame));
        if !same_window || is_fullscreen || !target_exists {
            log_debug!(
                "[winctl] display move retry cancelled: token={} same_window={} fullscreen={} target_exists={}",
                token,
                same_window,
                is_fullscreen,
                target_exists
            );
            CFRelease(win);
            clear_pending_display_move(token);
            return;
        }
        if rect_close(actual, job.target_frame) {
            log_debug!(
                "[winctl] display move settled: token={} attempts={} frame={:?}",
                token,
                job.attempt,
                actual
            );
            CFRelease(win);
            clear_pending_display_move(token);
            return;
        }
        if job.attempt >= MAX_DISPLAY_MOVE_RETRIES {
            log_info!(
                "[winctl] display move remained mismatched after retries: token={} target={:?} actual={:?}",
                token,
                job.target_frame,
                actual
            );
            CFRelease(win);
            clear_pending_display_move(token);
            return;
        }
        let next_attempt = job.attempt + 1;
        log_debug!(
            "[winctl] display move retry: token={} attempt={} target={:?} actual={:?}",
            token,
            next_attempt,
            job.target_frame,
            actual
        );
        let _ = set_frame(win, job.target_frame);
        CFRelease(win);
        if update_pending_attempt(token, next_attempt) {
            let delay = if next_attempt == MAX_DISPLAY_MOVE_RETRIES {
                0.10
            } else {
                0.04
            };
            schedule_display_move_retry(token, delay);
        }
    }
}

// ========== 动画型 App 的延迟收尾 / deferred settle for animating apps ==========
// 有的 App 把 frame 变更做成动画(实测 Ghostty/访达/PeachPic 缓动 ≈250ms,Telegram/Edge/
// ChatGPT/RustRover 则一帧到位)。动画期间 AX setter 照样报成功,但尺寸会按**旧原点**被夹到
// 屏幕内(宽度恰好是 screen_w - x),位置变更被丢掉——窗口只变高、不归位。
// 因此"回读不一致"不等于"写入被拒",只有真正报错或窗口完全没动才动缩放按钮;
// 其余先在动画结束前把位置重写回去,再把尺寸写延迟到动画之后。
//
// Some apps animate frame changes (measured ~250ms on Ghostty/Finder/PeachPic; Telegram/Edge/
// ChatGPT/RustRover land in a single frame). During that animation the AX setters still report
// success, but the size is clamped against the **old** origin (width lands on screen_w - x) and the
// position change is dropped, so the window only grows taller and never re-homes. A read-back
// mismatch therefore does not mean "the write was rejected": only a real error, or a window that
// did not move at all, justifies the zoom-button fallback. Everything else re-sends the position
// and postpones the size write until the animation has settled.

/// 一次 frame 写入的结果。
/// The result of one frame write.
#[derive(Clone, Copy)]
struct SnapWrite {
    /// 写入前的 frame(读不到为 None)。
    /// Frame before the write (None when unreadable).
    before: Option<AxRect>,
    /// 写入后的回读 frame(读不到为 None)。
    /// Frame read back after the write (None when unreadable).
    actual: Option<AxRect>,
    /// 是否至少有一条 AX 写返回非 0(AXValueCreate 失败也算)。
    /// Whether any AX write returned non-zero (an AXValueCreate failure counts).
    errored: bool,
}

impl SnapWrite {
    fn matched(&self, accept: impl Fn(AxRect) -> bool) -> bool {
        self.actual.is_some_and(accept)
    }

    /// 窗口相对写入前有没有任何变化。回读读不到时按"动过"处理:宁可什么也不做,也不要按那个
    /// toggle 式的缩放按钮。
    /// Whether the window changed at all. An unreadable frame counts as "moved": doing nothing is
    /// preferable to pressing the toggle-style zoom button.
    fn moved(&self) -> bool {
        match (self.before, self.actual) {
            (Some(b), Some(a)) => !rect_close(b, a),
            _ => true,
        }
    }
}

/// 一次 snap 写入之后的收尾动作(纯函数,单测覆盖)。
/// What to do after a snap write (pure; unit-tested).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SnapFollowUp {
    /// 已命中目标。
    /// The target was reached.
    Done,
    /// App 把变更做成了动画:位置重写回去,尺寸写入延迟到动画结束。
    /// The app animates the change: re-send the position and postpone the size write.
    RetryAfterSettle,
    /// 写入被明确拒绝("接受 AXPosition 却拒绝 AXSize")或窗口完全没动:换 App 自己的缩放收尾。
    /// The write was explicitly rejected ("accepts AXPosition but rejects AXSize") or the window
    /// did not move at all: finish with the app's own zoom action.
    ZoomFallback,
    /// 动过但只肯走到这里(网格吸附很宽之类):接受 App 的结果,不要再按 toggle。
    /// It moved but goes no further (a wide grid snap, say): accept the app's result, never press
    /// the toggle.
    GiveUp,
}

fn snap_follow_up(
    matched: bool,
    errored: bool,
    untouched: bool,
    settled_retry: bool,
) -> SnapFollowUp {
    if matched {
        return SnapFollowUp::Done;
    }
    if errored || untouched {
        return SnapFollowUp::ZoomFallback;
    }
    if settled_retry {
        return SnapFollowUp::GiveUp;
    }
    SnapFollowUp::RetryAfterSettle
}

/// 延迟收尾的阶段。实测顺序很讲究:位置写会启动 App 的缓动,缓动期间写尺寸会被顶掉;
/// 而尺寸刚写完就立刻写位置,尺寸又会被回退。所以必须是**位置 → 等停 → 尺寸**,且两段都要
/// 等缓动真正停下。
/// Phases of the deferred settle. Measured order matters: a position write starts the app's
/// animation and a size write during it is overridden, while a position write issued right after a
/// size write reverts the size. So the sequence must be **position -> wait -> size**, each waiting
/// for the animation to actually stop.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SnapPhase {
    /// 等首次写入留下的状态稳定下来,然后补写位置。
    /// Wait for the state left by the first attempt to settle, then re-send the position.
    Settle,
    /// 位置已补写,等它停下再补尺寸。
    /// The position was re-sent; wait for it to stop, then write the size.
    Moved,
    /// 尺寸已补写,再等它稳定后收尾。
    /// The size was re-sent; wait for it to settle, then finish.
    Resized,
}

/// 一次待收尾的 snap 写入。固定延时不够用:实测同一台机器上缓动从 ~250ms 到 ~600ms 都出现过,
/// 所以改成"轮询到 frame 不再变化"再推进下一步。
/// A pending snap finish. A fixed delay is not enough -- measured animations ranged from ~250ms to
/// ~600ms on the same machine -- so this polls until the frame stops changing before advancing.
#[derive(Clone, Copy)]
struct PendingSnap {
    token: u64,
    pid: i32,
    cgwid: u32,
    target: AxRect,
    /// 目标是"铺满可视区"还是普通 snap 目标。
    /// Whether the target is the full visible area or an ordinary snap target.
    maximized: bool,
    /// 正在逼近的状态:动画期间用它回答状态查询,免得读到中间 frame 误判。
    /// The state being approached; answers state queries during the animation so a mid-animation
    /// frame cannot be misread.
    state: SnapState,
    /// 首次写入前的 frame("窗口是否完全没动"用的基准)。
    /// The frame before the first attempt (the baseline for "did the window move at all").
    before: Option<AxRect>,
    /// 上一次回读的 frame。
    /// The previously read frame.
    last: Option<AxRect>,
    /// 连续多少次回读一致(≥2 才算缓动停下)。
    /// How many consecutive read-backs agreed (>=2 counts as settled).
    stable_rounds: u8,
    phase: SnapPhase,
    /// 已轮询的轮数(上限防死循环)。
    /// Poll rounds so far (bounded against an endless loop).
    waits: u8,
    created_at: Instant,
}

static PENDING_SNAP: LazyLock<Mutex<Option<PendingSnap>>> = LazyLock::new(|| Mutex::new(None));
static NEXT_SNAP_TOKEN: AtomicU64 = AtomicU64::new(1);
const SNAP_POLL_DELAY: f64 = 0.12;
/// 连续两次回读一致才算停下。
/// Two consecutive matching read-backs count as settled.
const SNAP_STABLE_ROUNDS: u8 = 2;
/// ≈2s:足够覆盖实测最长的缓动加上两段等待,又不至于长时间握着过期任务。
/// ~2s: covers the longest measured animation plus two waits without holding a stale job for long.
const SNAP_MAX_WAITS: u8 = 17;
const SNAP_PENDING_TTL: std::time::Duration = std::time::Duration::from_millis(2100);

/// 动画未决期间这个窗口应该被当成哪个状态(与 `pending_display_state` 同一思路)。
/// Which state the window should be treated as while the animation is pending (same idea as
/// `pending_display_state`).
fn pending_snap_state(pid: i32, cgwid: Option<u32>) -> Option<SnapState> {
    let cgwid = cgwid?;
    let job = PENDING_SNAP.lock().unwrap().as_ref().copied()?;
    if job.pid != pid || job.cgwid != cgwid || job.created_at.elapsed() > SNAP_PENDING_TTL {
        return None;
    }
    Some(job.state)
}

fn cancel_pending_snap(pid: i32, cgwid: Option<u32>) {
    let Some(cgwid) = cgwid else {
        return;
    };
    let mut pending = PENDING_SNAP.lock().unwrap();
    if pending
        .as_ref()
        .is_some_and(|job| job.pid == pid && job.cgwid == cgwid)
    {
        *pending = None;
    }
}

fn clear_pending_snap(token: u64) {
    let mut pending = PENDING_SNAP.lock().unwrap();
    if pending.as_ref().is_some_and(|job| job.token == token) {
        *pending = None;
    }
}

fn schedule_snap_verify(token: u64, delay: f64) {
    let Some(ctrl) = crate::CONTROLLER.lock().unwrap().map(|target| target.0) else {
        clear_pending_snap(token);
        return;
    };
    unsafe {
        let token: *mut AnyObject = msg_send![
            class!(NSNumber),
            numberWithUnsignedLongLong: token
        ];
        let _: () = msg_send![
            ctrl,
            performSelector: sel!(handleSnapVerify:),
            withObject: token,
            afterDelay: delay
        ];
    }
}

/// 安排一次延迟收尾:位置重写已经发出(它在缓动里会被顶掉,但能让窗口立刻朝目标移动),
/// 之后轮询到 frame 不再变化再补写尺寸。
/// Arm a deferred finish: the position has already been re-sent (the animation overrides it, but
/// the window starts heading for the target), then poll until the frame stops changing before
/// writing the size.
fn arm_pending_snap(
    pid: i32,
    cgwid: u32,
    target: AxRect,
    maximized: bool,
    state: SnapState,
    before: Option<AxRect>,
    last: Option<AxRect>,
) {
    let token = NEXT_SNAP_TOKEN.fetch_add(1, Ordering::Relaxed);
    *PENDING_SNAP.lock().unwrap() = Some(PendingSnap {
        token,
        pid,
        cgwid,
        target,
        maximized,
        state,
        before,
        last,
        stable_rounds: 0,
        phase: SnapPhase::Settle,
        waits: 0,
        created_at: Instant::now(),
    });
    schedule_snap_verify(token, SNAP_POLL_DELAY);
}

/// 延迟收尾:轮询到缓动停下 → 补写尺寸 → 再轮询确认。只在主线程执行。
/// Deferred settle: poll until the animation stops, write the size, then poll once more to confirm.
/// Main thread only.
pub(crate) fn on_snap_verify(arg: *mut c_void) {
    if arg.is_null() {
        return;
    }
    let token: u64 = unsafe { msg_send![arg as *mut AnyObject, unsignedLongLongValue] };
    let Some(job) = PENDING_SNAP.lock().unwrap().as_ref().copied() else {
        return;
    };
    if job.token != token {
        return;
    }

    unsafe {
        let app = AXUIElementCreateApplication(job.pid);
        if app.is_null() {
            clear_pending_snap(token);
            return;
        }
        AXUIElementSetMessagingTimeout(app, 0.3);
        let win = copy_attribute(app, K_AX_FOCUSED_WINDOW);
        CFRelease(app);
        let Some(win) = win else {
            clear_pending_snap(token);
            return;
        };
        let same_window = ax_window_cgwid(win) == Some(job.cgwid);
        let is_fullscreen = copy_string(win, K_AX_SUBROLE)
            .is_some_and(|subrole| subrole == K_AX_SUBROLE_FULL_SCREEN);
        if !same_window || is_fullscreen {
            log_debug!(
                "[winctl] snap settle cancelled: token={} same_window={} fullscreen={}",
                token,
                same_window,
                is_fullscreen
            );
            CFRelease(win);
            clear_pending_snap(token);
            return;
        }

        let actual = read_frame(win);
        if actual.is_some_and(snap_accepts(job.target, job.maximized)) {
            log_debug!(
                "[winctl] snap settled: token={} waits={} frame={:?}",
                token,
                job.waits,
                actual
            );
            CFRelease(win);
            clear_pending_snap(token);
            return;
        }
        // 缓动停下的判据:连续 SNAP_STABLE_ROUNDS 次回读一致(只看一次不够:缓动刚开始时
        // 连着两次读到同一个值很正常)。
        // Settled when SNAP_STABLE_ROUNDS consecutive read-backs agree; one is not enough, because
        // right before an animation starts two reads can match.
        let mut next = job;
        next.last = actual;
        next.waits = job.waits.saturating_add(1);
        next.stable_rounds = if matches!((job.last, actual), (Some(l), Some(a)) if rect_close(l, a))
        {
            job.stable_rounds.saturating_add(1)
        } else {
            0
        };
        let settled = next.stable_rounds >= SNAP_STABLE_ROUNDS;
        let sz = CgSize {
            w: job.target.w,
            h: job.target.h,
        };
        let pt = CgPoint {
            x: job.target.x,
            y: job.target.y,
        };
        match job.phase {
            SnapPhase::Settle if settled => {
                // 先位置:启动 App 的缓动。
                // Position first: it is what starts the app's animation.
                let err = set_ax_value(
                    win,
                    K_AX_POSITION,
                    K_AX_VALUE_CG_POINT,
                    &pt as *const CgPoint as *const c_void,
                );
                log_debug!(
                    "[winctl] snap follow-up position: token={} waits={} target={:?} err={}",
                    token,
                    next.waits,
                    job.target,
                    err
                );
                next.phase = SnapPhase::Moved;
                next.last = None;
                next.stable_rounds = 0;
            }
            SnapPhase::Moved if settled => {
                // 缓动停下后再写尺寸,此时才不会被顶掉。
                // Only once the move has settled does the size write stick.
                let err = set_ax_value(
                    win,
                    K_AX_SIZE,
                    K_AX_VALUE_CG_SIZE,
                    &sz as *const CgSize as *const c_void,
                );
                log_debug!(
                    "[winctl] snap follow-up size: token={} waits={} target={:?} err={}",
                    token,
                    next.waits,
                    job.target,
                    err
                );
                next.phase = SnapPhase::Resized;
                next.last = None;
                next.stable_rounds = 0;
            }
            SnapPhase::Resized if settled => {
                log_debug!(
                    "[winctl] snap follow-up done: token={} waits={} frame={:?}",
                    token,
                    next.waits,
                    actual
                );
                CFRelease(win);
                clear_pending_snap(token);
                return;
            }
            _ => {}
        }
        if next.waits >= SNAP_MAX_WAITS {
            // 收尾到底:基准是首次写入前的 frame——只要中途动过就说明 App 尽力了,
            // 不要再按那个 toggle 式的缩放按钮。
            // Give up gracefully: the baseline is the frame before the first attempt, and any
            // movement means the app did what it could, so the toggle must not be pressed.
            let untouched = match (job.before, actual) {
                (Some(b), Some(a)) => rect_close(b, a),
                _ => false,
            };
            if snap_follow_up(false, false, untouched, true) == SnapFollowUp::ZoomFallback {
                log_debug!("[winctl] snap settle rejected; trying native zoom fallback");
                if !press_native_zoom(win) {
                    log_info!("[winctl] native zoom fallback unavailable");
                }
            }
            log_debug!(
                "[winctl] snap follow-up abandoned: token={} waits={} frame={:?}",
                token,
                next.waits,
                actual
            );
            CFRelease(win);
            clear_pending_snap(token);
            return;
        }
        *PENDING_SNAP.lock().unwrap() = Some(next);
        CFRelease(win);
        schedule_snap_verify(token, SNAP_POLL_DELAY);
    }
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
        // 动画未决期间按目标状态作答:此时 frame 还在缓动,直接推断会误判(见 pending_snap_state)。
        // While a snap animation is pending, answer with the target state: the frame is still easing
        // and inferring from it would misread the window (see pending_snap_state).
        let pending_state = pending_snap_state(pid, cgwid);
        cancel_pending_display_move(pid, cgwid);
        cancel_pending_snap(pid, cgwid);
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
        } else if let Some(pending) = pending_state {
            pending
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
        // 本次动作要抵达的状态:目标矩形本身就是某个 snap 目标,回推即可。延迟收尾期间用它作答。
        // The state this action heads for: the target rect is itself a snap target, so inferring
        // from it is exact. The deferred finish reports it while the animation is pending.
        let desired = match p {
            Plan::Move(r) | Plan::Maximize(r) => infer_state(r, screens[cur_screen].visible),
            _ => effective,
        };
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
        execute(p, win, pid, desired, dir);
        CFRelease(win);
    }
}

/// 主线程:将前台窗口移动到相邻显示器,最大化窗口在目标屏幕保持最大化。
/// Main thread: move the frontmost window to an adjacent display, keeping maximized windows
/// maximized on the destination display.
pub(crate) fn apply_display_move(dir: Direction) {
    if !display_move_enabled(dir) {
        return;
    }
    let (app_name, pid) = crate::ffi::frontmost_app_info();
    if pid <= 0 || pid == std::process::id() as i32 {
        return;
    }
    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return;
        }
        AXUIElementSetMessagingTimeout(app, 0.3);
        let win = copy_attribute(app, K_AX_FOCUSED_WINDOW);
        CFRelease(app);
        let Some(win) = win else {
            log_debug!("[winctl] no focused window for display move pid {}", pid);
            return;
        };
        if let Some(subrole) = copy_string(win, K_AX_SUBROLE) {
            if subrole == K_AX_SUBROLE_FULL_SCREEN {
                log_debug!("[winctl] skip native fullscreen display move");
                CFRelease(win);
                return;
            }
        }
        let cgwid = ax_window_cgwid(win);
        let Some(frame) = read_frame(win) else {
            log_debug!("[winctl] failed to read window frame for display move");
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
        let pending_state = if minimized {
            None
        } else {
            pending_display_state(pid, cgwid, cur_screen, &screens)
        };
        let state = if minimized {
            SnapState::Normal
        } else if let Some(pending_state) = pending_state {
            pending_state
        } else {
            infer_state(frame, screens[cur_screen].visible)
        };
        if pending_state.is_none() {
            cancel_pending_display_move(pid, cgwid);
        }
        let Some((target_screen, target_frame)) =
            display_move_target(state, frame, cur_screen, dir, &screens)
        else {
            log_debug!("[winctl] no adjacent display for {:?}", dir);
            CFRelease(win);
            return;
        };
        if minimized {
            // 只有确认存在目标屏幕后才解除最小化,单屏按键不应改变窗口状态。
            // Restore only after confirming a destination display; a single-display no-op must
            // not change the window state.
            set_minimized(win, false);
        }
        log_debug!(
            "[winctl] display move app={:?} dir={:?} pid={} from={} to={} state={:?} frame={:?}",
            app_name,
            dir,
            pid,
            cur_screen,
            target_screen,
            state,
            target_frame
        );
        // 对需要保持 snap 状态的窗口先只迁移当前尺寸,让目标 App 先完成屏幕归属切换。
        // For snapped windows, first move the current size to the destination so the target app
        // can settle its screen association before we apply the destination snap rectangle.
        let staging_frame = display_move_staging_frame(
            state,
            frame,
            screens[cur_screen].visible,
            screens[target_screen].visible,
            target_frame,
        );
        let applied = set_frame(win, staging_frame);
        if !applied {
            log_debug!(
                "[winctl] display move staging frame mismatch; deferred target may be scheduled: cgwid={:?} staging={:?} target={:?}",
                cgwid,
                staging_frame,
                target_frame
            );
        }
        if let Some(cgwid) = cgwid {
            if !matches!(state, SnapState::Normal | SnapState::Minimized) {
                let token = NEXT_DISPLAY_MOVE_TOKEN.fetch_add(1, Ordering::Relaxed);
                let job = PendingDisplayMove {
                    token,
                    pid,
                    cgwid,
                    state,
                    target_screen_frame: screens[target_screen].frame,
                    target_frame,
                    attempt: 0,
                    created_at: Instant::now(),
                };
                // 即使第一次读取已经匹配,也延迟校验一次,防止目标 App 在 AX 返回后异步覆盖尺寸。
                // Verify once even after an immediate match, because the target app may asynchronously
                // overwrite the frame after the AX call returns.
                {
                    let mut pending = PENDING_DISPLAY_MOVE.lock().unwrap();
                    *pending = Some(job);
                }
                schedule_display_move_retry(token, if applied { 0.06 } else { 0.03 });
            }
        }
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
                    finish_snap(
                        win,
                        pid,
                        target,
                        true,
                        SnapState::Maximized,
                        set_frame_with(win, target, snap_accepts(target, true)),
                    );
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
unsafe fn execute(plan: Plan, win: AXUIElementRef, pid: i32, desired: SnapState, dir: Direction) {
    match plan {
        Plan::Move(r) => finish_snap(
            win,
            pid,
            r,
            false,
            desired,
            set_frame_with(win, r, snap_accepts(r, false)),
        ),
        Plan::Maximize(r) => finish_snap(
            win,
            pid,
            r,
            true,
            desired,
            set_frame_with(win, r, snap_accepts(r, true)),
        ),
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
/// `accept` 判定回读结果是否算成功(精确 snap 用 `rect_close`,最大化用 `fills_visible`)。
/// Set the window frame position-first to avoid transient off-screen overflow while growing;
/// retry in the reverse order if either AX write is rejected. `accept` decides whether the
/// read-back counts as success (exact snaps use `rect_close`, maximize uses `fills_visible`).
unsafe fn set_frame_with(
    win: AXUIElementRef,
    r: AxRect,
    accept: impl Fn(AxRect) -> bool,
) -> SnapWrite {
    let before = read_frame(win);
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
    let errored = pos_err != K_AX_SUCCESS || size_err != K_AX_SUCCESS;
    let Some(actual) = read_frame(win) else {
        log_info!(
            "[winctl] unable to verify frame after AX write; target={:?}",
            r
        );
        return SnapWrite {
            before,
            actual: None,
            errored,
        };
    };
    if !accept(actual) {
        log_info!(
            "[winctl] frame mismatch after AX write: target={:?} actual={:?}",
            r,
            actual
        );
    }
    SnapWrite {
        before,
        actual: Some(actual),
        errored,
    }
}

/// 精确 set_frame:跳屏 staging(容差 FRAME_EPSILON,验收结果会决定是否走回退重试)。
/// Exact set_frame for cross-display staging (FRAME_EPSILON tolerance; the verdict decides whether
/// a fallback retry runs).
unsafe fn set_frame(win: AXUIElementRef, r: AxRect) -> bool {
    set_frame_with(win, r, |a| rect_close(a, r)).matched(|a| rect_close(a, r))
}

/// snap 写入的收尾:命中即结束;否则按策略重写位置并把尺寸写入推迟到动画之后,或換 App 自己的
/// 缩放动作。详见上方"动画型 App 的延迟收尾"一节。
/// Finish a snap write: a match is final; otherwise re-send the position and postpone the size
/// write past the app's animation, or fall back to the app's own zoom action.
unsafe fn finish_snap(
    win: AXUIElementRef,
    pid: i32,
    target: AxRect,
    maximized: bool,
    desired: SnapState,
    write: SnapWrite,
) {
    let matched = write.matched(snap_accepts(target, maximized));
    match snap_follow_up(matched, write.errored, !write.moved(), false) {
        SnapFollowUp::Done => {}
        SnapFollowUp::RetryAfterSettle => {
            let Some(cgwid) = ax_window_cgwid(win) else {
                log_debug!("[winctl] snap settle skipped: no window id");
                return;
            };
            // 这里**不要**立刻补写任何东西:刚发完尺寸就写位置会把尺寸回退掉,而缓动期间写
            // 尺寸又会被顶掉(都是实测)。交给延迟收尾按"位置 → 等停 → 尺寸"的顺序来。
            // Nothing is written here on purpose: a position write right after the size write reverts
            // the size, and a size write during the animation is overridden (both measured). The
            // deferred settle runs the "position -> wait -> size" sequence instead.
            log_debug!(
                "[winctl] snap settle pending: pid={} target={:?} actual={:?}",
                pid,
                target,
                write.actual
            );
            arm_pending_snap(
                pid,
                cgwid,
                target,
                maximized,
                desired,
                write.before,
                write.actual,
            );
        }
        SnapFollowUp::ZoomFallback => {
            log_debug!(
                "[winctl] snap write rejected; trying native zoom fallback: target={:?}",
                target
            );
            if !press_native_zoom(win) {
                log_info!("[winctl] native zoom fallback unavailable");
            }
        }
        SnapFollowUp::GiveUp => {}
    }
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

/// 触发一次原生缩放:优先 `AXZoomWindow`(App 自己的缩放,与双击标题栏 / Option+点绿钮等价),
/// 缩放按钮上没有该动作时才退回 `AXPress`。
///
/// `AXPress` 是绿钮的**点击**动作,自 macOS 10.11 起是"切换全屏"——终端等 App 会因此进全屏,
/// 所以它只作最后兜底。`AXZoomWindow` 是私有动作名,且**即使生效也返回**
/// `kAXErrorAttributeUnsupported`(-25205),因此调用方不能拿返回码判断结果,只能按 frame 复核。
/// 返回是否发出了动作(缩放按钮不存在时为 false)。
///
/// Perform one native zoom, preferring `AXZoomWindow` (the app's own zoom, same as double-clicking
/// the title bar or Option-clicking the green button) and falling back to `AXPress` only when the
/// button does not expose it.
///
/// `AXPress` is the green button's **click** action, which toggles fullscreen since macOS 10.11 --
/// that is what puts Terminal and similar apps into fullscreen, so it is only the last resort.
/// `AXZoomWindow` is a private action name and returns `kAXErrorAttributeUnsupported` (-25205)
/// **even when it works**, so callers must verify via the frame, never the return code. Returns
/// whether an action was sent (false when the zoom button is missing).
unsafe fn press_native_zoom(win: AXUIElementRef) -> bool {
    let Some(btn) = copy_attribute(win, K_AX_ZOOM_BUTTON) else {
        log_info!("[winctl] AXZoomButton unavailable");
        return false;
    };
    let action = if has_action(btn, K_AX_ZOOM_WINDOW) {
        K_AX_ZOOM_WINDOW
    } else {
        K_AX_PRESS
    };
    let key = cf_string_new(action);
    let err = AXUIElementPerformAction(btn, key);
    CFRelease(key);
    CFRelease(btn);
    log_debug!("[winctl] native zoom {} -> {}", action, err);
    true
}

/// 元素是否支持某个动作名(用于探测私有的 `AXZoomWindow`)。
/// Whether the element supports an action name (probes the private `AXZoomWindow`).
unsafe fn has_action(element: AXUIElementRef, name: &str) -> bool {
    let mut names: *const c_void = std::ptr::null();
    if AXUIElementCopyActionNames(element, &mut names) != K_AX_SUCCESS || names.is_null() {
        return false;
    }
    let count = CFArrayGetCount(names).max(0);
    let found = (0..count).any(|i| {
        let item = CFArrayGetValueAtIndex(names, i);
        !item.is_null() && cf_to_rust_string(item).as_deref() == Some(name)
    });
    CFRelease(names);
    found
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

static TAP_CONTROL: event_tap::TapThreadControl = event_tap::TapThreadControl::new();
static WC_THREAD: Mutex<Option<thread::JoinHandle<()>>> = Mutex::new(None);

/// tap 回调:关心 Option+方向键及 Option+Shift+四方向键。启用时吞掉 keyDown/keyUp 并把
/// 非自动重复的 keyDown 投递给主线程;关闭时全部透传(功能关闭 = 组合键还给系统)。
/// 自己是前台 App 时也透传,设置窗口文本框的按词移动不受影响。
///
/// The tap callback: handles Option+arrows and Option+Shift+arrow keys. When enabled it
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
    if crate::input_monitor::handle_disabled_event(event_type, "winctl") {
        return event;
    }
    if !crate::input_monitor::taps_allowed() {
        return event;
    }
    if event_type != K_CG_EVENT_KEY_DOWN && event_type != K_CG_EVENT_KEY_UP {
        return event;
    }
    let keycode = CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_KEYCODE) as u16;
    let Some(dir) = Direction::from_keycode(keycode) else {
        return event;
    };
    let flags = CGEventGetFlags(event);
    if flags & K_FLAG_OPTION == 0 || flags & (K_FLAG_COMMAND | K_FLAG_CONTROL) != 0 {
        return event;
    }
    let display_move = flags & K_FLAG_SHIFT != 0;
    // 本应用合成的组合键(鼠标映射 Key Press post 到 HID 层后会回到 session tap):
    // 必须透传,否则映射了 Option+方向键的侧键会被这里劫持。
    // Our own synthesized combos (mouse Key Press mappings post at HID level and loop back
    // into session taps) must pass through, or a side button mapped to Option+arrow gets
    // hijacked here.
    if CGEventGetIntegerValueField(event, K_CG_EVENT_SOURCE_USER_DATA) == SYNTHETIC_MARKER {
        return event;
    }
    let enabled = if display_move {
        display_move_enabled(dir)
    } else {
        direction_enabled(dir)
    };
    if !enabled {
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
            if display_move {
                log_debug!("[winctl] keyDown Option+Shift+{:?}", dir);
                crate::enqueue_global_event(GlobalEvent::WindowDisplayMove(dir));
            } else {
                log_debug!("[winctl] keyDown Option+{:?}", dir);
                crate::enqueue_global_event(GlobalEvent::WindowControl(dir));
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
    if !crate::input_monitor::taps_allowed() {
        return;
    }
    let mut guard = WC_THREAD.lock().unwrap();
    if guard.as_ref().is_some_and(|h| !h.is_finished()) {
        return;
    }
    if let Some(finished) = guard.take() {
        let _ = finished.join();
    }
    TAP_CONTROL.prepare_start();
    *guard = Some(spawn_tap_thread());
    log_info!("Window control enabled.");
}

/// 运行时停用窗口控制(设置页热切换)。幂等。
/// Disable window control at runtime (settings hot-switch). Idempotent.
pub(crate) fn stop() {
    TAP_CONTROL.stop();
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
        // session 层 tap:与切换器同层,能拦截真实硬件按键;DEFAULT_TAP 才能吞事件。
        // Session-level tap: same layer as the switcher, sees real hardware keys; DEFAULT_TAP
        // is required to swallow events.
        let created = event_tap::create_tap_with_retry(
            tap_location::SESSION_EVENT_TAP,
            tap_placement::HEAD_INSERT,
            tap_options::DEFAULT_TAP,
            mask,
            Some(window_control_tap_callback),
            std::ptr::null_mut(),
            "winctl",
            Some(TAP_CONTROL.cancel_flag()),
        );
        let created = match created {
            Some(created) => created,
            None => return,
        };
        let rl = CFRunLoopGetCurrent();
        TAP_CONTROL.register(created.tap, rl);
        let watchdog = event_tap::start_tap_watchdog(created.tap, TAP_CONTROL.cancel_flag());
        if !TAP_CONTROL.cancel_flag().load(Ordering::SeqCst) && crate::input_monitor::taps_allowed()
        {
            log_debug!("Window control event tap started.");
            event_tap::CFRunLoopRun();
        }
        event_tap::stop_tap_watchdog(watchdog);
        event_tap::CGEventTapEnable(created.tap, false);
        TAP_CONTROL.clear(created.tap);
        event_tap::teardown_event_tap(rl, created);
    })
}

#[cfg(test)]
mod tests {
    use super::{
        display_move_staging_frame, display_move_target, fills_visible, infer_state,
        neighbor_screen, plan, rect_close_with, snap_follow_up, snap_frames, AxRect, Direction,
        Plan, ScreenGeometry, SnapFollowUp, SnapState, SnapWrite, SNAP_EPSILON,
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

    /// 三块屏幕:主屏 (0,1112),上方屏 y=0,下方屏 y=1112(AX 坐标 y 向下)。
    /// Three stacked screens: primary at (0,1112), upper at y=0, lower at y=1112 (AX y grows down).
    fn stacked_screens() -> Vec<ScreenGeometry> {
        let mk = |y: f64| ScreenGeometry {
            frame: rect(0.0, y, 1920.0, 1112.0),
            visible: rect(0.0, y + 25.0, 1920.0, 1055.0),
        };
        vec![mk(0.0), mk(1112.0), mk(2224.0)]
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
    fn snap_follow_up_covers_the_measured_app_classes() {
        // 一帧到位的 App(Telegram/Edge/ChatGPT/RustRover):命中即结束,不做多余动作。
        // Apps that land in one frame (Telegram/Edge/ChatGPT/RustRover): a match is final.
        assert_eq!(
            snap_follow_up(true, false, false, false),
            SnapFollowUp::Done
        );
        assert_eq!(snap_follow_up(true, true, true, true), SnapFollowUp::Done);
        // 动画型 App(Ghostty/访达/PeachPic):动过但还没到位 -> 等动画结束再补写尺寸。
        // Animating apps (Ghostty/Finder/PeachPic): it moved but has not arrived -> postpone the
        // size write past the animation.
        assert_eq!(
            snap_follow_up(false, false, false, false),
            SnapFollowUp::RetryAfterSettle
        );
        // 补写之后仍不到位(网格吸附很宽之类):接受 App 的结果,绝不按那个 toggle。
        // Still short after the postponed write (a wide grid snap, say): accept the app's result and
        // never press the toggle.
        assert_eq!(
            snap_follow_up(false, false, false, true),
            SnapFollowUp::GiveUp
        );
        // AX 写被明确拒绝("接受 AXPosition 却拒绝 AXSize"):换 App 自己的缩放收尾。
        // An explicitly rejected write ("accepts AXPosition but rejects AXSize"): finish with the
        // app's own zoom.
        assert_eq!(
            snap_follow_up(false, true, false, false),
            SnapFollowUp::ZoomFallback
        );
        // 窗口完全没动:它无视了 frame 写入,缩放才是有效动作。
        // The window did not move at all: it ignores frame writes, so the zoom action is the useful
        // one.
        assert_eq!(
            snap_follow_up(false, false, true, false),
            SnapFollowUp::ZoomFallback
        );
    }

    #[test]
    fn snap_write_reports_movement_conservatively() {
        let a = rect(0.0, 0.0, 100.0, 100.0);
        let grown = rect(0.0, 0.0, 100.0, 200.0);
        assert!(SnapWrite {
            before: Some(a),
            actual: Some(grown),
            errored: false
        }
        .moved());
        assert!(!SnapWrite {
            before: Some(a),
            actual: Some(a),
            errored: false
        }
        .moved());
        // 读不到 frame 时保守地当作"动过":不按 toggle。
        // An unreadable frame counts as "moved" so the toggle is never pressed.
        assert!(SnapWrite {
            before: Some(a),
            actual: None,
            errored: false
        }
        .moved());
    }

    #[test]
    fn maximize_accepts_app_grid_snapping() {
        // 终端实测:visibleFrame 高 923,它按文本行吸附后只能给 915(短一行 8pt)。
        // Measured on Terminal: visibleFrame is 923 tall, but its text-row grid only allows 915
        // (one 8pt row short).
        let v = rect(0.0, 33.0, 1470.0, 923.0);
        let terminal = rect(0.0, 33.0, 1472.0, 915.0);
        assert!(fills_visible(terminal, v));
        assert_eq!(infer_state(terminal, v), SnapState::Maximized);
        // 宽松判定不能吞掉半屏/四分屏。
        // The loose check must not swallow halves or quarters.
        let f = snap_frames(v);
        assert!(!fills_visible(f.top, v));
        assert_eq!(infer_state(f.top, v), SnapState::TopHalf);
        assert_eq!(infer_state(f.bottom, v), SnapState::BottomHalf);
        assert_eq!(infer_state(f.left, v), SnapState::LeftHalf);
        assert_eq!(infer_state(f.top_left, v), SnapState::TopLeft);
        assert_eq!(infer_state(f.bottom_right, v), SnapState::BottomRight);
        // 只写进去一半高度不是最大化(真正的写入失败仍要能识别)。
        // Half the height is not a maximize (a genuinely rejected write must stay detectable).
        assert!(!fills_visible(rect(0.0, 33.0, 1470.0, 461.5), v));
        assert_eq!(
            infer_state(rect(100.0, 100.0, 800.0, 600.0), v),
            SnapState::Normal
        );
        // 半屏同样会被网格吸附:终端实测半屏目标 461.5、实际 464,状态仍须判为上半屏。
        // Halves snap to the grid too: Terminal returns 464 where the half is 461.5, and the state
        // must still read as TopHalf.
        assert!(rect_close_with(
            rect(0.0, 33.0, 1472.0, 464.0),
            f.top,
            SNAP_EPSILON
        ));
        assert_eq!(
            infer_state(rect(0.0, 33.0, 1472.0, 464.0), v),
            SnapState::TopHalf
        );
        // 容差不会把相邻目标混淆:半屏与四等分相差 ~230pt。
        // The tolerance cannot confuse adjacent targets: half and quarter are ~230pt apart.
        assert!(!rect_close_with(f.top, f.top_left, SNAP_EPSILON));
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

    #[test]
    fn neighbor_screens_support_vertical_layouts() {
        let screens = stacked_screens();
        assert_eq!(neighbor_screen(&screens, 1, Direction::Up), Some(0));
        assert_eq!(neighbor_screen(&screens, 1, Direction::Down), Some(2));
        assert_eq!(neighbor_screen(&screens, 0, Direction::Up), None);
        assert_eq!(neighbor_screen(&screens, 2, Direction::Down), None);
    }

    #[test]
    fn display_move_preserves_snap_state_on_vertical_layouts() {
        let screens = stacked_screens();
        let current = snap_frames(screens[1].visible);
        let target = snap_frames(screens[0].visible);
        assert_eq!(
            display_move_target(
                SnapState::Maximized,
                current.max,
                1,
                Direction::Up,
                &screens,
            ),
            Some((0, target.max))
        );
        assert_eq!(
            display_move_target(
                SnapState::BottomRight,
                current.bottom_right,
                1,
                Direction::Up,
                &screens,
            ),
            Some((0, target.bottom_right))
        );
    }

    #[test]
    fn display_move_stages_snapped_windows_before_resizing() {
        let from = rect(0.0, 30.0, 1470.0, 923.0);
        let to = rect(0.0, 30.0, 1920.0, 1050.0);
        let current = from;
        let target = snap_frames(to).max;
        assert_eq!(
            display_move_staging_frame(SnapState::Maximized, current, from, to, target),
            current
        );
        assert_eq!(
            display_move_staging_frame(SnapState::Normal, current, from, to, target),
            target
        );
    }
}
