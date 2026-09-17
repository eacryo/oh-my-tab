//! 切换器浮窗与卡片 UI:浮窗/容器/状态栏的 static、卡片↔索引映射、键盘/鼠标回调,
//! 以及浮窗的显示/隐藏/刷新/卡片构建/主题应用等渲染逻辑。activate_and_raise 负责
//! 抬起目标窗口。KEY_* 为键盘导航键码。
//!
//! Switcher overlay & card UI: statics for the overlay/container/status bar, the card<->index
//! map, keyboard/mouse callbacks, and the overlay's show/hide/refresh/card-build/theme-apply
//! rendering. activate_and_raise raises the target window. KEY_* are keyboard-navigation key
//! codes.

use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::ffi::CString;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::Instant; // TIMING-DEBUG

use crate::config::{self, CONFIG};
use crate::event_tap;
use crate::ffi::*;
use crate::i18n::t;
use crate::icon_cache::extract_icon_to_cache;
use crate::theme::*;
use crate::window_collector::{
    bump_window_mru, raise_window_ax_async, raise_window_fast, sort_windows_by_mru, MruMap,
    WindowInfo,
};
use crate::window_server;
// 跨模块共享状态(由 main.rs 持有,这里读写)/ cross-module shared state (owned by main.rs)
use crate::window_refresh::request_window_refresh;
use crate::with_tab_state;
use crate::AppState;
use crate::{log_debug, log_info, WINDOW_COUNT};

// ========== 键盘键码 / keyboard key codes ==========

pub(crate) const KEY_TAB: u16 = 48;
pub(crate) const KEY_LEFT: u16 = 123;
pub(crate) const KEY_RIGHT: u16 = 124;
pub(crate) const KEY_DOWN: u16 = 125;
pub(crate) const KEY_UP: u16 = 126;
pub(crate) const KEY_ESCAPE: u16 = 53;
pub(crate) const KEY_RETURN: u16 = 36;
pub(crate) const KEY_DELETE: u16 = 51; // Backspace
/// NSEventModifierFlagShift，与 CGEvent 的 Shift 位一致。
/// NSEventModifierFlagShift; it shares the Shift bit with CGEvent flags.
const NSEVENT_MODIFIER_FLAG_SHIFT: u64 = 0x0002_0000;
const NSEVENT_MODIFIER_FLAG_OPTION: u64 = 0x0008_0000;
const NSEVENT_MODIFIER_FLAG_COMMAND: u64 = 0x0010_0000;
/// 卡片右上角关闭按钮的 tag(hover 显隐查找用;卡片 index 不存 tag)。
/// The close-button tag on a card (used to find it for hover show/hide; the card
/// index is NOT stored in the tag).
pub(crate) const CLOSE_BTN_TAG: isize = 0xE7F1;
/// 选中态位移用的图标视图 tag,避免依赖动态 ObjC 类的属性访问。
/// Tag used to find the icon view for the selected-state nudge without relying on
/// property accessors on the dynamically registered ObjC card class.
pub(crate) const ICON_VIEW_TAG: isize = 0xE7F2;
/// 缩略图模式预览区容器的 tag(选中描边与整卡上浮模式识别用)。
/// Tag for the thumbnail-mode preview container (used for its selected border and
/// to identify cards that receive the whole-card lift).
const THUMB_PREVIEW_TAG: isize = 0xE7F3;
/// 缩略图模式选中态的 2pt 外圈视图 tag。
/// Tag for the thumbnail-mode selected-state 2pt outer ring.
const THUMB_SELECTION_RING_TAG: isize = 0xE7F4;
/// Liquid Glass 会稀释设计稿 16% 的 accent-soft，提升到 38% 让选中态更明显。
/// Liquid Glass washes out the mockup's 16% accent-soft; use 38% for a clearer selection.
const SELECTION_RING_ALPHA: u8 = 0x61;
/// 外圈附加的零偏移柔光；与卡片自身的深色向下投影分层。
/// Zero-offset glow around the ring, layered separately from the card's dark drop shadow.
const SELECTION_GLOW_OPACITY: f32 = 0.35;
const SELECTION_GLOW_RADIUS: f64 = 4.0;
/// 设计稿选中预览描边 = rgba(...,.34),换算为 8 位 alpha。
/// Mockup selected-preview border = rgba(...,.34), converted to 8-bit alpha.
/// 旧版纯图标模式选中时仅图标上移的距离。
/// Distance that only the icon moves upward in legacy icon-only mode.
const SELECTED_CONTENT_NUDGE: f64 = 2.0;
/// 设计稿 `.item.selected { transform: translateY(-1px) }`：AppKit y 轴向上为正，
/// 因此缩略图卡片根层使用 +1pt，标题、预览和卡片表面作为整体上浮。
/// The mockup's `.item.selected { transform: translateY(-1px) }`: AppKit's y axis is
/// positive upward, so the thumbnail card root uses +1pt and lifts its caption, preview,
/// and surface as one unit.
const SELECTED_CARD_LIFT: f64 = 1.0;
/// 卡片收窄并补位的动画时长;整个过程保持在一次 AppKit 动画事务内。
/// Duration of the slot-collapse/reflow animation; the whole transition stays in one AppKit transaction.
const CARD_CLOSE_ANIMATION_DURATION: f64 = 0.16;

// ========== 浮窗相关全局状态 / overlay global state ==========

pub(crate) static OVERLAY_WINDOW: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
pub(crate) static CONTAINER: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
/// 持久的卡片 document view;滚动时只移动 CONTAINER 的 bounds,不重建卡片树。
/// Persistent card document view; scrolling moves CONTAINER bounds instead of rebuilding cards.
pub(crate) static CARD_DOCUMENT: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
pub(crate) static STATUS_LABEL: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
/// 缩略图溢出时显示的原生竖向滚动条。
/// Native vertical scroller shown when the thumbnail rows overflow the viewport.
pub(crate) static THUMB_SCROLLER: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
/// macOS 26+ 的 NSGlassEffectView 指针(用于设置热重载时重新应用玻璃属性)。
/// Pointer to the NSGlassEffectView on macOS 26+ (used to re-apply glass properties on hot reload).
pub(crate) static GLASS_VIEW: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
pub(crate) static CARD_CLASS: Mutex<Option<StaticClass>> = Mutex::new(None);

/// Copy a main-thread UI pointer out of its slot before calling AppKit.
///
/// 在调用 AppKit 前先把主线程 UI 指针复制出来并结束槽位借用。AppKit 的部分方法会同步
/// 触发 Objective-C 通知回调；如果回调再次访问同一个 `MainThreadSlot`，持有 `RefMut`
/// 就会触发 `BorrowMutError`。
///
/// Copy the main-thread UI pointer out of its slot before calling AppKit. Some AppKit methods
/// synchronously deliver Objective-C notifications; keeping the `RefMut` alive across such a
/// call lets a re-entered callback hit the same slot and panic with `BorrowMutError`.
pub(super) fn overlay_window_ptr() -> Option<*mut AnyObject> {
    OVERLAY_WINDOW
        .lock()
        .unwrap()
        .as_ref()
        .map(|window| window.0)
}

pub(super) fn overlay_container_ptr() -> Option<*mut AnyObject> {
    CONTAINER
        .lock()
        .unwrap()
        .as_ref()
        .map(|container| container.0)
}

/// 注册 OhMyTabCardView 卡片类(此前在 main.rs 注册、本模块使用,归属已收回)。
/// Register the OhMyTabCardView class (registration used to live in main.rs while
/// the class is owned/used here; ownership is now local).
pub(crate) fn register_card_class() {
    unsafe {
        let name = CString::new("OhMyTabCardView").unwrap();
        let superclass = class!(NSView) as *const _ as *mut AnyObject;
        let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
        let types_v_obj = CString::new("v@:@").unwrap();
        class_addMethod(
            cls,
            sel!(mouseDown:),
            card_mouse_down as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(mouseEntered:),
            card_mouse_entered as *mut c_void,
            types_v_obj.as_ptr(),
        );
        objc_registerClassPair(cls);
        *CARD_CLASS.lock().unwrap() = Some(StaticClass(cls as *const objc2::runtime::AnyClass));
    }
}
/// Maps card view pointer (as usize) -> card index, avoiding property accessor
/// msg_send! issues on dynamically-registered ObjC classes.
pub(crate) static CARD_INDEX_MAP: LazyLock<Mutex<HashMap<usize, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// Maps a card view pointer to its stable window identity.  Unlike the index map, this remains
/// valid when MRU sorting changes the order of `TAB_STATE.windows` between summons.
static CARD_KEY_MAP: LazyLock<Mutex<HashMap<usize, WindowKey>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// Content/layout signature for each rendered card.  A matching signature allows the view tree
/// to be reused; changes such as a new title, icon, minimized state, or card dimensions replace
/// only that card.
static CARD_SIGNATURES: LazyLock<Mutex<HashMap<usize, CardSignature>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// 缩略图模式当前实际渲染的全局窗口索引区间；窗口列表本身从不截断。
/// Global window-index range currently rendered in thumbnail mode; the authoritative
/// window list is never truncated.
static THUMB_VISIBLE_RANGE: Mutex<Option<Range<usize>>> = Mutex::new(None);
/// 完整流式布局的稳定行范围,用于按行滚动和把选中项带回视口。
/// Stable row ranges for the complete flow layout, used for row scrolling and keeping selection visible.
static THUMB_ROW_RANGES: Mutex<Option<Vec<Range<usize>>>> = Mutex::new(None);
/// 当前面板一次能显示的最大行数。
/// Maximum number of rows visible in the current panel.
static THUMB_MAX_ROWS: Mutex<usize> = Mutex::new(1);
/// 当前滚动视口的首行,0 表示从 MRU 列表顶部开始。
/// First row of the scrolling viewport; zero starts at the top of the MRU list.
static THUMB_SCROLL_ROW: Mutex<usize> = Mutex::new(0);
/// 当前滚动视口相对内容顶部的 point 偏移,支持卡片在边界处部分可见。
/// Point offset from the top of the scrolling content, allowing cards to cross viewport edges smoothly.
static THUMB_SCROLL_OFFSET: Mutex<f64> = Mutex::new(0.0);
/// 当前完整布局允许的最大 point 偏移。
/// Maximum point offset allowed by the current complete layout.
static THUMB_SCROLL_MAX_OFFSET: Mutex<f64> = Mutex::new(0.0);
/// 当前缩略图 document 的高度(不含状态栏),用于设置 NSClipView 的合法滚动范围。
/// Current thumbnail document height excluding the status bar, used for the clip-view range.
static THUMB_DOCUMENT_HEIGHT: Mutex<f64> = Mutex::new(0.0);
/// 当前卡片预览所需的像素高度,滚动进入新行时复用同一捕获规格。
/// Current preview pixel demand, reused when scrolling into a new row.
static THUMB_CAPTURE_TARGET_PX_H: Mutex<u32> = Mutex::new(512);
/// Last resolved palette used by `apply_theme`; prevents unrelated config changes from forcing
/// a full thumbnail recapture when the effective light/dark appearance is unchanged.
/// `apply_theme` 使用的上一次最终调色板;有效明暗未变化时,普通配置热重载不应触发全量重拍。
static LAST_APPLIED_THEME_DARK: Mutex<Option<bool>> = Mutex::new(None);
/// 当前完整布局的行间距(卡片高度 + 行间距),供键盘整行导航复用。
/// Current full-layout row pitch (card height + row gap), reused by whole-row keyboard navigation.
static THUMB_SCROLL_ROW_PITCH: Mutex<f64> = Mutex::new(1.0);
/// 自定义滚动条当前的显式拖拽状态。
/// Explicit drag state for the custom scrollbar.
#[derive(Clone, Copy)]
struct ThumbnailScrollDrag {
    start_y: f64,
    start_offset: f64,
    max_offset: f64,
    thumb_travel: f64,
}

static THUMB_SCROLL_DRAG: Mutex<Option<ThumbnailScrollDrag>> = Mutex::new(None);

/// 滚动条的悬停状态:视口悬停时提高滑块可见度,直接悬停滑块时再提高一级。
/// Scrollbar hover state: increase thumb visibility over the viewport, then one more level over the thumb.
#[derive(Clone, Copy, Default, PartialEq)]
struct ThumbnailScrollerHover {
    viewport: bool,
    knob: bool,
}

static THUMB_SCROLLER_HOVER: Mutex<ThumbnailScrollerHover> = Mutex::new(ThumbnailScrollerHover {
    viewport: false,
    knob: false,
});

/// 正在播放退出动画的窗口;使用稳定窗口身份,不依赖动画期间可能失效的数组索引。
/// Window currently playing its exit animation; uses stable identity instead of a transient index.
struct PendingCardClose {
    pid: i32,
    cgwid: u32,
    animation_finished: bool,
    ax_result: Option<bool>,
    original_panel_frame: NSRect,
    original_container_frame: NSRect,
    original_document_frame: NSRect,
    original_bounds_origin: NSPoint,
    original_frames: HashMap<WindowKey, NSRect>,
    final_frames: HashMap<WindowKey, NSRect>,
    final_row_ranges: Vec<Range<usize>>,
    final_panel_frame: NSRect,
    final_container_frame: NSRect,
    final_document_frame: NSRect,
    final_bounds_origin: NSPoint,
    final_overflowed: bool,
    original_document_h: f64,
    final_document_h: f64,
    final_scroll_max_offset: f64,
    final_scroll_offset: f64,
}

type WindowKey = (i32, u32);
static PENDING_CARD_CLOSE: Mutex<Option<PendingCardClose>> = Mutex::new(None);
/// 后台 AX 关闭结果的单槽值类型消息;worker 不直接修改主线程动画状态。
/// Single-slot value result from the background AX close; the worker never mutates the
/// main-thread animation state directly.
static CARD_CLOSE_AX_RESULT: Mutex<Option<(WindowKey, bool)>> = Mutex::new(None);

#[derive(Clone, Debug, PartialEq, Eq)]
struct CardSignature {
    app_name: String,
    window_title: String,
    icon_path: Option<String>,
    minimized: bool,
    /// Resolved light/dark state used when the card's text and layers were painted.
    /// 卡片文字和图层绘制时采用的最终明暗状态。
    theme_dark: bool,
    card_width_bits: u64,
    card_height_bits: u64,
    thumbnail_layout: bool,
    /// 卡片标题行是否包含应用名;开关变化必须走 Replace,否则复用会让旧标题留存。
    /// Whether the caption includes the app name; a toggle must force Replace, or reuse
    /// would keep the old caption.
    show_app_name_in_cards: bool,
    thumbnail_capture_allowed: bool,
    /// Cached-thumbnail version the card was painted with; a bump means the frame
    /// changed since and the card must be rebuilt instead of reused.
    /// 卡片绘制时所用的缓存帧版本;版本前进意味着帧已更换,必须重建而非复用。
    thumb_epoch: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CardReconcileAction {
    Create,
    Reuse,
    Replace,
}

fn card_reconcile_action(
    existing: Option<&CardSignature>,
    desired: &CardSignature,
) -> CardReconcileAction {
    match existing {
        None => CardReconcileAction::Create,
        Some(current) if current == desired => CardReconcileAction::Reuse,
        Some(_) => CardReconcileAction::Replace,
    }
}

fn card_signature(
    window: &WindowInfo,
    frame: NSRect,
    thumbnail_layout: bool,
    thumbnail_capture_allowed: bool,
) -> CardSignature {
    CardSignature {
        app_name: window.app_name.clone(),
        window_title: window.window_title.clone(),
        icon_path: window.icon_path.clone(),
        minimized: window.minimized,
        theme_dark: crate::theme::resolved_is_dark(),
        card_width_bits: frame.size.width.to_bits(),
        card_height_bits: frame.size.height.to_bits(),
        thumbnail_layout,
        show_app_name_in_cards: crate::theme::show_app_name_in_cards(),
        thumbnail_capture_allowed,
        // 帧版本入签名:种子→真实、激活补拍、外观重拍等任何一次换帧都会让下一次
        // 召唤的签名失配走 Replace,杜绝复用路径冻结旧图(图标模式下恒为 0,无扰动)。
        // The frame version joins the signature: any frame replacement (seed ->
        // real, activation refresh, appearance recapture) mismatches the next
        // summon's signature and forces a Replace, so reuse can never freeze a
        // stale image (constant 0 in icon mode, no churn).
        thumb_epoch: crate::thumbnail::frame_epoch(window.pid, window.window_id),
    }
}

fn clear_thumbnail_scroll_drag() {
    *THUMB_SCROLL_DRAG.lock().unwrap() = None;
}
/// 连续上下导航保持的水平中心；水平切换、鼠标选择和新召唤时重置。
/// Preferred horizontal center retained across consecutive vertical moves; reset
/// by horizontal navigation, mouse selection, and a fresh summon.
static THUMB_NAV_ANCHOR_X: Mutex<Option<f64>> = Mutex::new(None);
type CardPlacementFrame = (usize, f64, f64, f64);
/// Prevents hover-selection on the card under the cursor when the window first
/// opens. Reset on a fresh summon and flipped to true on the first mouse move.
pub(crate) static MOUSE_MOVED: AtomicBool = AtomicBool::new(false);

/// 当前浮窗的鼠标激活方式是否为悬停。
/// Whether the overlay currently activates windows on hover.
pub(crate) fn activates_on_hover() -> bool {
    crate::config::CONFIG
        .read()
        .map(|config| config.windows.activation_mode != "click")
        .unwrap_or(true)
}

pub(crate) fn thumbnail_scroller() -> Option<ObjPtr> {
    *THUMB_SCROLLER.lock().unwrap()
}

mod callbacks;
mod cancel;
mod card_close;
mod cards;
mod hover;
use callbacks::*;
use cancel::*;
use cards::*;

use card_close::*;
pub(crate) use card_close::{
    begin_close_window_at, card_close_in_progress, card_mouse_down, card_mouse_entered,
    on_card_close_ax_result, on_card_close_finished, on_close_card, on_cmd_release_diagnostic,
    on_cmd_released,
};
use hover::*;
pub(crate) use hover::{container_mouse_moved, on_deferred_scroll_hover};
// 对 crate 其他模块暴露的入口(内部子模块实现)。
// Entry points exposed to the rest of the crate (implemented in the child modules).
pub(crate) use callbacks::{
    commit_first_summon, container_accepts_first_responder, container_key_down,
    container_mouse_entered, container_mouse_exited, container_scroll_wheel,
    on_cmd_shift_tab_pressed, on_cmd_tab_pressed, on_first_summon_timeout,
    overlay_window_can_become_key, show_first_summon, thumbnail_scroller_accepts_first_mouse,
    thumbnail_scroller_draw_rect, thumbnail_scroller_geometry, thumbnail_scroller_mouse_down,
    thumbnail_scroller_mouse_dragged, thumbnail_scroller_mouse_entered,
    thumbnail_scroller_mouse_exited, thumbnail_scroller_mouse_moved, thumbnail_scroller_mouse_up,
};
pub(crate) use cancel::{
    apply_glass_properties, apply_theme, close_window_at, extract_uncached_icons,
    install_click_to_cancel, on_deferred_raise, on_delayed_order_out, refresh_highlight,
    refresh_thumbnail_previews, vanish_overlay,
};
pub(crate) use cards::{create_card_view, show_overlay};
// 仅缩略图单测经 crate::overlay::nsimage_from_cgimage 使用(cards.rs 内部另有调用点)。
// Only the thumbnail unit tests use crate::overlay::nsimage_from_cgimage (cards.rs
// also calls it internally).
#[cfg(test)]
pub(crate) use cards::nsimage_from_cgimage;

// ========== 卡片 ↔ 索引映射 / card <-> index map ==========

/// Read the card index from the card index map (keyed by view pointer).
/// This avoids msg_send! encoding issues with property accessors on
/// dynamically-registered ObjC classes.
pub(crate) fn get_card_index(view: *mut AnyObject) -> Option<usize> {
    let map = CARD_INDEX_MAP.lock().unwrap();
    map.get(&(view as usize)).copied()
}

fn card_document() -> Option<*mut AnyObject> {
    CARD_DOCUMENT.lock().unwrap().map(|document| document.0)
}

unsafe fn card_views(document: *mut AnyObject) -> Vec<*mut AnyObject> {
    let subviews: *mut AnyObject = msg_send![document, subviews];
    let count: usize = msg_send![subviews, count];
    (0..count)
        .map(|i| msg_send![subviews, objectAtIndex: i])
        .filter(|view| get_card_index(*view).is_some())
        .collect()
}

pub(crate) fn set_card_index(view: *mut AnyObject, idx: usize) {
    let mut map = CARD_INDEX_MAP.lock().unwrap();
    map.insert(view as usize, idx);
}

fn card_key(view: *mut AnyObject) -> Option<WindowKey> {
    CARD_KEY_MAP.lock().unwrap().get(&(view as usize)).copied()
}

fn set_card_key(view: *mut AnyObject, key: WindowKey) {
    CARD_KEY_MAP.lock().unwrap().insert(view as usize, key);
}

fn set_card_signature(view: *mut AnyObject, signature: CardSignature) {
    CARD_SIGNATURES
        .lock()
        .unwrap()
        .insert(view as usize, signature);
}

fn card_signature_for(view: *mut AnyObject) -> Option<CardSignature> {
    CARD_SIGNATURES
        .lock()
        .unwrap()
        .get(&(view as usize))
        .cloned()
}

/// 原位刷新预览后,把已展示帧的版本号同步进签名,避免下一次召唤做多余的
/// Replace 重建(期间若又有新帧入库,签名只会偏旧一版,至多多重建一次,自愈)。
/// After an in-place preview refresh, sync the displayed frame's version into the
/// signature so the next summon skips a redundant Replace rebuild (a newer frame
/// landing in between leaves the signature one version behind -- at most one extra
/// rebuild, always self-correcting).
fn sync_card_signature_epoch(view: *mut AnyObject, epoch: u64) {
    let mut signatures = CARD_SIGNATURES.lock().unwrap();
    if let Some(signature) = signatures.get_mut(&(view as usize)) {
        signature.thumb_epoch = epoch;
    }
}

pub(crate) fn remove_card_index(view: *mut AnyObject) {
    let mut map = CARD_INDEX_MAP.lock().unwrap();
    map.remove(&(view as usize));
    CARD_KEY_MAP.lock().unwrap().remove(&(view as usize));
    CARD_SIGNATURES.lock().unwrap().remove(&(view as usize));
}

pub(crate) fn clear_card_indices() {
    let mut map = CARD_INDEX_MAP.lock().unwrap();
    map.clear();
    CARD_KEY_MAP.lock().unwrap().clear();
    CARD_SIGNATURES.lock().unwrap().clear();
}

/// 缩略图捕获调度读取的可见区间快照。/ Visible-range snapshot for thumbnail scheduling.
pub(crate) fn thumbnail_visible_range() -> Option<Range<usize>> {
    THUMB_VISIBLE_RANGE.lock().unwrap().clone()
}

pub(crate) fn reset_thumbnail_visible_range() {
    *THUMB_VISIBLE_RANGE.lock().unwrap() = None;
    *THUMB_ROW_RANGES.lock().unwrap() = None;
}

pub(crate) fn reset_thumbnail_scroll() {
    *THUMB_SCROLL_ROW.lock().unwrap() = 0;
    *THUMB_SCROLL_OFFSET.lock().unwrap() = 0.0;
    *THUMB_SCROLL_MAX_OFFSET.lock().unwrap() = 0.0;
    *THUMB_DOCUMENT_HEIGHT.lock().unwrap() = 0.0;
    *THUMB_SCROLL_ROW_PITCH.lock().unwrap() = 1.0;
    clear_thumbnail_scroll_drag();
    set_thumbnail_scroller_hover(false, false);
}

/// 集合级变化(窗口增删)整树重建前的一次性复位:可视区间 + 滚动 + 导航锚点。
/// window_refresh 经 OverlayPresenter 钩子调用(单向触发,不反向依赖本模块)。
///
/// One-shot reset before a set-level (window added/removed) full rebuild: visible
/// range + scroll + navigation anchor. Called by window_refresh through the
/// OverlayPresenter hook (one-way triggering; it does not depend on this module).
pub(crate) fn reset_thumbnail_state() {
    reset_thumbnail_visible_range();
    reset_thumbnail_scroll();
    reset_thumbnail_nav_anchor();
}

fn thumbnail_scroller_alpha(viewport_hovered: bool, knob_hovered: bool, dragging: bool) -> f64 {
    if knob_hovered || dragging {
        0.58
    } else if viewport_hovered {
        0.42
    } else {
        0.26
    }
}

fn thumbnail_scroller_knob_contains(point_y: f64, geometry: ThumbnailScrollerGeometry) -> bool {
    point_y >= geometry.knob_y && point_y <= geometry.knob_y + geometry.knob_h
}

fn invalidate_thumbnail_scroller() {
    unsafe {
        if let Some(scroller) = thumbnail_scroller() {
            let _: () = msg_send![scroller.0, setNeedsDisplay: true];
        }
    }
}

fn set_thumbnail_scroller_hover(viewport: bool, knob: bool) {
    let mut state = THUMB_SCROLLER_HOVER.lock().unwrap();
    let next = ThumbnailScrollerHover { viewport, knob };
    if *state == next {
        return;
    }
    *state = next;
    drop(state);
    invalidate_thumbnail_scroller();
}

fn visible_range_for_scroll(
    rows: &[Range<usize>],
    max_rows: usize,
    row_pitch: f64,
    offset: f64,
) -> (Range<usize>, usize) {
    if rows.is_empty() || row_pitch <= 0.0 || !row_pitch.is_finite() {
        return (0..0, 0);
    }
    let viewport_rows = max_rows.max(1).min(rows.len());
    let max_row_start = rows.len().saturating_sub(viewport_rows);
    let row_start = (offset.max(0.0) / row_pitch).floor() as usize;
    let row_start = row_start.min(max_row_start);
    // The overflowing viewport intentionally exposes a clipped teaser row at
    // every non-terminal position, including exact row boundaries.
    let has_partial_row = row_start + viewport_rows < rows.len();
    let row_end = (row_start + viewport_rows + usize::from(has_partial_row)).min(rows.len());
    let visible = rows[row_start].start..rows[row_end - 1].end;
    (visible, row_start)
}

fn update_thumbnail_scroll_state(offset: f64) -> bool {
    let rows = THUMB_ROW_RANGES.lock().unwrap().clone().unwrap_or_default();
    let max_rows = *THUMB_MAX_ROWS.lock().unwrap();
    let row_pitch = *THUMB_SCROLL_ROW_PITCH.lock().unwrap();
    let (visible, row_start) = visible_range_for_scroll(&rows, max_rows, row_pitch, offset);
    let mut current = THUMB_VISIBLE_RANGE.lock().unwrap();
    let changed = current.as_ref() != Some(&visible);
    *current = Some(visible);
    drop(current);
    *THUMB_SCROLL_ROW.lock().unwrap() = row_start;
    changed
}

unsafe fn apply_thumbnail_clip_offset() {
    let Some(container) = (*CONTAINER.lock().unwrap()).map(|container| container.0) else {
        return;
    };
    let bounds: NSRect = msg_send![container, bounds];
    let requested_max = *THUMB_SCROLL_MAX_OFFSET.lock().unwrap();
    let document_h = *THUMB_DOCUMENT_HEIGHT.lock().unwrap();
    let legal_max = (document_h - bounds.size.height).max(0.0);
    let max_offset = if document_h > 0.0 {
        requested_max.min(legal_max)
    } else {
        requested_max
    };
    let offset = *THUMB_SCROLL_OFFSET.lock().unwrap();
    // AppKit 的 y 轴向上:逻辑 offset=0 对应 document 顶部,所以 bounds 从最大值开始。
    // AppKit's y axis grows upward: logical offset=0 is the document top, so bounds starts at max.
    let origin_y = (max_offset - offset).clamp(0.0, max_offset.max(0.0));
    let _: () = msg_send![
        container,
        setBoundsOrigin: NSPoint::new(bounds.origin.x, origin_y)
    ];
    let _: () = msg_send![container, setNeedsDisplay: true];
}

fn apply_thumbnail_scroll_offset() {
    let offset = *THUMB_SCROLL_OFFSET.lock().unwrap();
    update_thumbnail_scroll_state(offset);
    unsafe {
        apply_thumbnail_clip_offset();
    }
    // 内容和滑块必须在同一个 offset 更新中失效,否则滚轮只移动内容而滑块停在旧位置。
    // Invalidate the content and thumb in the same offset update, otherwise wheel scrolling moves
    // only the content while the thumb stays at its old position.
    invalidate_thumbnail_scroller();
}

/// 让选中项所在行进入视口;已在视口时不改变用户通过滚轮选择的滚动位置。
/// Bring the selected item into view; leave a user-scrolled viewport alone when it already contains it.
fn ensure_thumbnail_selection_visible(selected: usize) -> bool {
    let rows = THUMB_ROW_RANGES.lock().unwrap().clone();
    let Some(rows) = rows else {
        return false;
    };
    let max_rows = (*THUMB_MAX_ROWS.lock().unwrap()).max(1);
    let Some(selected_row) = rows.iter().position(|range| range.contains(&selected)) else {
        return false;
    };
    let mut scroll_row = THUMB_SCROLL_ROW.lock().unwrap();
    let current = *scroll_row;
    let next = scroll_start_for_selection(current, selected_row, max_rows);
    let changed = next != current;
    *scroll_row = next;
    if changed {
        let row_pitch = *THUMB_SCROLL_ROW_PITCH.lock().unwrap();
        *THUMB_SCROLL_OFFSET.lock().unwrap() = next as f64 * row_pitch;
    }
    changed
}

/// 选择项越过当前视口边缘时只移动一行,保留上一视口的重叠行。
/// Move only one row when selection crosses a viewport edge, preserving one overlapping row.
fn scroll_start_for_selection(current: usize, selected_row: usize, max_rows: usize) -> usize {
    let max_rows = max_rows.max(1);
    if selected_row < current {
        selected_row
    } else if selected_row >= current + max_rows {
        selected_row + 1 - max_rows
    } else {
        current
    }
}

/// 以 point 偏移移动缩略图视口,滚轮和触控板都通过此路径获得连续滚动。
/// Move the thumbnail viewport by a point offset; both mouse wheels and trackpads use this path
/// for continuous scrolling.
fn scroll_thumbnail_by_offset(delta: f64) {
    if !delta.is_finite() || delta.abs() < f64::EPSILON {
        return;
    }
    let current = *THUMB_SCROLL_OFFSET.lock().unwrap();
    let max_offset = *THUMB_SCROLL_MAX_OFFSET.lock().unwrap();
    set_thumbnail_scroll_offset(current + delta, max_offset);
}

pub(crate) fn set_thumbnail_scroll_offset(next: f64, max_offset: f64) {
    if !next.is_finite() || !max_offset.is_finite() {
        return;
    }
    let mut offset = THUMB_SCROLL_OFFSET.lock().unwrap();
    let next = next.clamp(0.0, max_offset.max(0.0));
    if (next - *offset).abs() < f64::EPSILON {
        return;
    }
    *offset = next;
    drop(offset);
    let visible_changed = update_thumbnail_scroll_state(next);
    apply_thumbnail_scroll_offset();
    if visible_changed && crate::theme::thumbnails_enabled() {
        let target_px_h = *THUMB_CAPTURE_TARGET_PX_H.lock().unwrap();
        crate::thumbnail::refresh_for_summon(target_px_h);
    }
    // 滚动停止约 50ms 后再命中固定指针,避免滚动途中高光随经过的卡片反复跳动。
    // Re-hit-test the stationary pointer about 50ms after scrolling stops, avoiding highlight
    // churn while cards pass under the cursor during a gesture.
    schedule_deferred_scroll_hover();
}

pub(crate) fn thumbnail_scroller_set_fraction_for_smoke(fraction: f64) {
    let max_offset = *THUMB_SCROLL_MAX_OFFSET.lock().unwrap();
    set_thumbnail_scroll_offset(fraction.clamp(0.0, 1.0) * max_offset, max_offset);
}

pub(crate) fn thumbnail_scroller_max_offset() -> f64 {
    *THUMB_SCROLL_MAX_OFFSET.lock().unwrap()
}

pub(crate) fn reset_thumbnail_nav_anchor() {
    *THUMB_NAV_ANCHOR_X.lock().unwrap() = None;
}

// ========== 文本 helper / text helpers ==========

/// 保留 RRGGBB,只替换 RRGGBBAA 的 alpha。
/// Preserve RRGGBB and replace only the alpha in RRGGBBAA.
fn color_with_alpha(color: u32, alpha: u8) -> u32 {
    (color & 0xFFFF_FF00) | u32::from(alpha)
}

fn thumbnail_card_lift_y(is_selected: bool) -> f64 {
    if is_selected {
        SELECTED_CARD_LIFT
    } else {
        0.0
    }
}

fn horizontal_nav_index(selected: usize, len: usize, backward: bool) -> usize {
    if len == 0 {
        return 0;
    }
    let selected = selected.min(len - 1);
    if backward {
        selected.checked_sub(1).unwrap_or(len - 1)
    } else {
        (selected + 1) % len
    }
}

/// 首次召唤前先把缓存数组同步到最新 MRU,并用前台 PID 找到当前窗口代理。
/// 若前台窗口因 AX 暂时漏报而没有卡片,正向切换从第 0 项开始,避免把旧的同 App 卡片
/// 当成“下一个窗口”。
/// 如果有上次精确焦点 key,优先按 `(pid,cgwid)` 匹配;精确窗口缺失时不再拿同 App 的其他窗口冒充当前窗口。
/// Sort the cached array by the latest MRU and use the exact `(pid,cgwid)` focus key when available.
/// If AX temporarily omitted that exact window, forward navigation starts at index 0 instead of
/// treating a stale same-app card as the current window.
fn prepare_first_summon(
    windows: &mut [WindowInfo],
    mru: &mut MruMap,
    backward: bool,
    frontmost_pid: Option<i32>,
    focus_key: Option<(i32, u32)>,
    now: Instant,
) -> usize {
    // 首帧先把当前前台窗口写回 MRU,使首帧顺序与随后后台刷新(summon-bump 也会
    // 写回前台窗口)一致,消除「旧序 → 重排」的翻转那一下。只对前台 pid 精确匹配
    // 的 focus_key 写回;窗口缺失时回退到前台 pid 的代表窗口。
    // Bump the frontmost window into MRU before the first frame so the initial ordering
    // matches the subsequent background refresh (whose summon-bump also writes the frontmost
    // window back) -- removing the visible re-order flank. Only write back when the exact
    // focus key (or the frontmost pid's representative window) is present.
    if let Some((pid, window_id)) = focus_key.or_else(|| {
        frontmost_pid.and_then(|pid| {
            windows
                .iter()
                .find(|w| w.pid == pid)
                .map(|w| (w.pid, w.window_id))
        })
    }) {
        bump_window_mru(mru, pid, window_id);
    }

    sort_windows_by_mru(windows, mru, now);

    for window in windows.iter_mut() {
        window.is_active = false;
    }
    let frontmost_index = match focus_key {
        Some(key) => windows
            .iter()
            .position(|window| (window.pid, window.window_id) == key)
            .or_else(|| frontmost_pid.and_then(|pid| windows.iter().position(|w| w.pid == pid))),
        None => frontmost_pid.and_then(|pid| windows.iter().position(|w| w.pid == pid)),
    };
    if let Some(index) = frontmost_index {
        // 有精确 key 时只把同一张窗口置首,避免同 App 的错误兄弟窗口成为当前窗口代理。
        // With an exact key, move only that window first so a wrong same-app sibling cannot act as the proxy.
        windows.swap(0, index);
        windows[0].is_active = true;
    } else if frontmost_pid.is_none() {
        // 无法取得前台 PID 时保留排序后的首项作为屏幕定位代理。
        // If the frontmost PID is unavailable, retain the sorted first item as the screen proxy.
        if let Some(first) = windows.first_mut() {
            first.is_active = true;
        }
    }

    if backward {
        windows.len().saturating_sub(1)
    } else if frontmost_pid.is_some() && frontmost_index.is_none() {
        0
    } else if windows.len() > 1 {
        1
    } else {
        0
    }
}

/// 窗口没有标题时(如 Microsoft To Do,AXTitle 为空)回退显示应用名。
/// 注意:仅用于显示。内部 `window_title` 仍保持空串,这样 raise_ax_window 仍能
/// 按空标题匹配到对应的 AX 窗口并聚焦。
/// Fall back to the app name for windows that expose no title (e.g. Microsoft
/// To Do, whose custom title bar yields an empty AXTitle). Display-only: the
/// internal `window_title` stays empty so raise_ax_window can still match the
/// AX window by its empty title.
fn display_title<'a>(title: &'a str, app_name: &'a str) -> &'a str {
    if title.is_empty() {
        app_name
    } else {
        title
    }
}

/// 缩略图卡片的标题行文本:开启「卡片显示应用名」后,应用名前置并以 " · " 与窗口标题分隔
/// (与底部状态栏同一分隔符)。窗口无标题、或标题与应用名文本完全相同(如访达的窗口)时
/// 只显示一份,不出现 "App · App"。
/// The thumbnail card's caption text: with "show app name in cards" enabled the app name
/// precedes the window title, separated by " · " (the footer's separator). A titleless window,
/// or one whose title is textually identical to the app name (e.g. a Finder window), renders a
/// single copy so it never reads "App · App".
fn card_caption(title: &str, app_name: &str, show_app_name: bool) -> String {
    if show_app_name && !title.is_empty() && title != app_name {
        format!("{} · {}", app_name, title)
    } else {
        display_title(title, app_name).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::card_caption;
    use super::card_reconcile_action;
    use super::cg_window_center_to_appkit_point;
    use super::color_with_alpha;
    use super::delayed_order_out_should_hide;
    use super::display_title;
    use super::edge_row_nav_index;
    use super::horizontal_nav_index;
    use super::prepare_first_summon;
    use super::scroll_start_for_selection;
    use super::thumbnail_scroll_offset_for_drag;
    use super::thumbnail_scroller_alpha;
    use super::thumbnail_scroller_geometry;
    use super::thumbnail_scroller_knob_contains;
    use super::vertical_nav_index;
    use super::CardReconcileAction;
    use super::CardSignature;
    use crate::window_collector::{MruMap, WindowInfo};
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use std::time::Instant;

    /// 构造一行卡片的 rects:y 固定,x 依次排开(宽 100 间距 10)。
    /// Build one row of rects: fixed y, sequential x (width 100, gap 10).
    fn row(indices: &[usize], y: f64) -> Vec<(usize, f64, f64, f64)> {
        indices
            .iter()
            .enumerate()
            .map(|(n, &i)| (i, n as f64 * 110.0, y, 100.0))
            .collect()
    }

    fn signature(title: &str) -> CardSignature {
        CardSignature {
            app_name: "App".into(),
            window_title: title.into(),
            icon_path: None,
            minimized: false,
            theme_dark: false,
            card_width_bits: 100.0f64.to_bits(),
            card_height_bits: 100.0f64.to_bits(),
            thumbnail_layout: true,
            show_app_name_in_cards: false,
            thumbnail_capture_allowed: true,
            thumb_epoch: 0,
        }
    }

    #[test]
    fn card_reconcile_action_only_replaces_changed_content() {
        let current = signature("same");
        assert_eq!(
            card_reconcile_action(Some(&current), &signature("same")),
            CardReconcileAction::Reuse
        );
        assert_eq!(
            card_reconcile_action(Some(&current), &signature("changed")),
            CardReconcileAction::Replace
        );
        assert_eq!(
            card_reconcile_action(None, &signature("new")),
            CardReconcileAction::Create
        );
    }

    #[test]
    fn card_reconcile_action_replaces_when_only_the_frame_epoch_advanced() {
        // 缓存帧在浮窗关闭期间被替换(种子→真实/激活补拍/外观重拍):其余字段全同,
        // 仅帧版本前进,也必须 Replace 重建,否则复用路径会持续展示旧图。
        // The cached frame was replaced while the overlay was closed (seed -> real /
        // activation refresh / appearance recapture): with every other field equal,
        // the frame version alone must still force a Replace, or the reuse path
        // keeps showing the stale image forever.
        let mut painted = signature("same");
        painted.thumb_epoch = 3;
        let mut current = signature("same");
        current.thumb_epoch = 3;
        assert_eq!(
            card_reconcile_action(Some(&painted), &current),
            CardReconcileAction::Reuse
        );
        current.thumb_epoch = 4;
        assert_eq!(
            card_reconcile_action(Some(&painted), &current),
            CardReconcileAction::Replace
        );
        // 缓存被清空(LRU 驱逐/关闭缩略图):版本回落到 0 同样触发重建。
        // Cache emptied (LRU eviction / thumbnails off): the version falling back
        // to 0 must rebuild as well.
        current.thumb_epoch = 0;
        assert_eq!(
            card_reconcile_action(Some(&painted), &current),
            CardReconcileAction::Replace
        );
    }

    #[test]
    fn color_with_alpha_preserves_rgb() {
        assert_eq!(color_with_alpha(0x4B7BECC7, 0x47), 0x4B7BEC47);
        assert_eq!(color_with_alpha(0x5577CCFF, 0x57), 0x5577CC57);
    }

    #[test]
    fn thumbnail_selection_lifts_the_whole_card_by_one_point() {
        assert_eq!(super::thumbnail_card_lift_y(true), 1.0);
        assert_eq!(super::thumbnail_card_lift_y(false), 0.0);
    }

    #[test]
    fn cg_window_center_converts_to_appkit_space_for_offset_display() {
        // 主屏较高、副屏底部对齐时,副屏的 AppKit y 原点高于主屏;直接比较 CG y
        // 会把副屏上方窗口误判为主屏或触发主屏回退。
        // When a shorter secondary display is bottom-aligned with a taller primary display,
        // its AppKit y origin is above the primary's; comparing CG y directly would misroute
        // an upper secondary window to the primary or trigger the primary fallback.
        let primary = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1512.0, 1440.0));
        let secondary = NSRect::new(NSPoint::new(1512.0, 458.0), NSSize::new(2560.0, 982.0));
        let center = cg_window_center_to_appkit_point((1512.0, 0.0, 1000.0, 400.0), primary);

        assert_eq!(center.x, 2012.0);
        assert_eq!(center.y, 1240.0);
        assert!(center.x >= secondary.origin.x);
        assert!(center.x <= secondary.origin.x + secondary.size.width);
        assert!(center.y >= secondary.origin.y);
        assert!(center.y <= secondary.origin.y + secondary.size.height);
    }

    #[test]
    fn thumbnail_scroller_geometry_has_a_real_drag_travel() {
        let top = thumbnail_scroller_geometry(100.0, 100.0, 0.0).unwrap();
        let bottom = thumbnail_scroller_geometry(100.0, 100.0, 100.0).unwrap();
        assert!(top.knob_h < 96.0);
        assert!(top.thumb_travel > 0.0);
        assert!(top.knob_y > bottom.knob_y);
        assert!((bottom.knob_y - 22.0).abs() < 1e-9);
        assert!((top.knob_y + top.knob_h - 78.0).abs() < 1e-9);
    }

    #[test]
    fn thumbnail_scroller_geometry_rejects_no_overflow() {
        assert!(thumbnail_scroller_geometry(100.0, 0.0, 0.0).is_none());
        assert!(thumbnail_scroller_geometry(0.0, 10.0, 0.0).is_none());
    }

    #[test]
    fn thumbnail_scroller_drag_maps_and_clamps_offset() {
        assert_eq!(
            thumbnail_scroll_offset_for_drag(0.0, 50.0, 25.0, 100.0, 50.0),
            50.0
        );
        assert_eq!(
            thumbnail_scroll_offset_for_drag(20.0, 50.0, 200.0, 100.0, 50.0),
            0.0
        );
        assert_eq!(
            thumbnail_scroll_offset_for_drag(80.0, 50.0, -200.0, 100.0, 50.0),
            100.0
        );
    }

    #[test]
    fn thumbnail_scroller_alpha_matches_html_hover_levels() {
        assert_eq!(thumbnail_scroller_alpha(false, false, false), 0.26);
        assert_eq!(thumbnail_scroller_alpha(true, false, false), 0.42);
        assert_eq!(thumbnail_scroller_alpha(true, true, false), 0.58);
        assert_eq!(thumbnail_scroller_alpha(false, false, true), 0.58);
    }

    #[test]
    fn thumbnail_scroller_knob_hit_test_uses_capsule_bounds() {
        let geometry = thumbnail_scroller_geometry(100.0, 100.0, 0.0).unwrap();
        assert!(thumbnail_scroller_knob_contains(geometry.knob_y, geometry));
        assert!(thumbnail_scroller_knob_contains(
            geometry.knob_y + geometry.knob_h,
            geometry
        ));
        assert!(!thumbnail_scroller_knob_contains(
            geometry.knob_y - 0.1,
            geometry
        ));
    }

    #[test]
    fn horizontal_navigation_wraps_in_both_directions() {
        assert_eq!(horizontal_nav_index(0, 5, false), 1);
        assert_eq!(horizontal_nav_index(4, 5, false), 0);
        assert_eq!(horizontal_nav_index(4, 5, true), 3);
        assert_eq!(horizontal_nav_index(0, 5, true), 4);
        assert_eq!(horizontal_nav_index(0, 0, true), 0);
    }

    #[test]
    fn first_summon_reorders_before_selecting_the_next_window() {
        fn window(pid: i32, window_id: u32) -> WindowInfo {
            WindowInfo {
                pid,
                window_id,
                app_name: format!("App {pid}"),
                window_title: format!("Window {window_id}"),
                icon_path: None,
                is_active: false,
                minimized: false,
                bounds: (0.0, 0.0, 100.0, 100.0),
            }
        }

        let now = Instant::now();
        let mut mru = MruMap::new();
        mru.insert((1, 100), now - std::time::Duration::from_secs(2));
        mru.insert((2, 200), now - std::time::Duration::from_secs(1));
        let mut windows = vec![window(1, 100), window(2, 200)];

        let selected = prepare_first_summon(&mut windows, &mut mru, false, Some(2), None, now);

        assert_eq!((windows[0].pid, windows[0].window_id), (2, 200));
        assert_eq!((windows[1].pid, windows[1].window_id), (1, 100));
        assert_eq!(selected, 1);
        assert_eq!(
            (windows[selected].pid, windows[selected].window_id),
            (1, 100)
        );
        assert!(windows[0].is_active);
        assert!(!windows[1].is_active);
    }

    #[test]
    fn first_summon_starts_at_zero_when_frontmost_card_is_missing() {
        fn window(pid: i32, window_id: u32) -> WindowInfo {
            WindowInfo {
                pid,
                window_id,
                app_name: format!("App {pid}"),
                window_title: format!("Window {window_id}"),
                icon_path: None,
                is_active: true,
                minimized: false,
                bounds: (0.0, 0.0, 100.0, 100.0),
            }
        }

        let now = Instant::now();
        let mut mru = MruMap::new();
        mru.insert((1, 100), now - std::time::Duration::from_secs(2));
        mru.insert((2, 200), now - std::time::Duration::from_secs(1));
        let mut windows = vec![window(1, 100), window(2, 200)];

        let selected = prepare_first_summon(&mut windows, &mut mru, false, Some(9), None, now);

        assert_eq!(selected, 0);
        assert_eq!((windows[0].pid, windows[0].window_id), (2, 200));
        assert!(!windows.iter().any(|window| window.is_active));
    }

    #[test]
    fn first_summon_keeps_last_card_for_reverse_navigation() {
        fn window(pid: i32, window_id: u32) -> WindowInfo {
            WindowInfo {
                pid,
                window_id,
                app_name: format!("App {pid}"),
                window_title: format!("Window {window_id}"),
                icon_path: None,
                is_active: false,
                minimized: false,
                bounds: (0.0, 0.0, 100.0, 100.0),
            }
        }

        let now = Instant::now();
        let mut windows = vec![window(1, 100), window(2, 200)];
        let selected =
            prepare_first_summon(&mut windows, &mut MruMap::new(), true, Some(9), None, now);

        assert_eq!(selected, windows.len() - 1);
    }

    #[test]
    fn first_summon_uses_exact_focus_key_not_same_pid_proxy() {
        fn window(pid: i32, window_id: u32) -> WindowInfo {
            WindowInfo {
                pid,
                window_id,
                app_name: format!("App {pid}"),
                window_title: format!("Window {window_id}"),
                icon_path: None,
                is_active: false,
                minimized: false,
                bounds: (0.0, 0.0, 100.0, 100.0),
            }
        }

        let now = Instant::now();
        let mut mru = MruMap::new();
        mru.insert((1, 100), now - std::time::Duration::from_secs(2));
        mru.insert((1, 101), now - std::time::Duration::from_secs(1));
        let mut windows = vec![window(1, 100), window(1, 101)];

        let selected =
            prepare_first_summon(&mut windows, &mut mru, false, Some(1), Some((1, 101)), now);

        assert_eq!((windows[0].pid, windows[0].window_id), (1, 101));
        assert_eq!(selected, 1);
        assert_eq!(
            (windows[selected].pid, windows[selected].window_id),
            (1, 100)
        );
    }

    #[test]
    fn first_summon_falls_back_to_frontmost_pid_window_when_exact_key_is_missing() {
        fn window(pid: i32, window_id: u32) -> WindowInfo {
            WindowInfo {
                pid,
                window_id,
                app_name: format!("App {pid}"),
                window_title: format!("Window {window_id}"),
                icon_path: None,
                is_active: true,
                minimized: false,
                bounds: (0.0, 0.0, 100.0, 100.0),
            }
        }

        let now = Instant::now();
        let mut windows = vec![window(1, 100), window(1, 101)];
        let mut mru = MruMap::new();

        let selected =
            prepare_first_summon(&mut windows, &mut mru, false, Some(1), Some((1, 999)), now);

        // focus_key=(1,999) 不在列表,回退到前台 pid(1) 的代表窗口 (1,100):置首、标记 active,
        // 选中跳到下一张。没有「精确窗口缺失就停在 0 无高亮」的僵死态。
        // focus_key=(1,999) is missing, so we fall back to the frontmost pid's (1) representative
        // window (1,100): moved to the front and marked active, selection advances to the next
        // card. This removes the "exact window missing -> stuck at index 0 with no highlight"
        // dead state.
        assert_eq!(selected, 1);
        assert_eq!((windows[0].pid, windows[0].window_id), (1, 100));
        assert!(windows[0].is_active);
        assert!(!windows[1].is_active);
    }

    #[test]
    fn selection_scrolls_one_row_with_overlap_at_viewport_edges() {
        assert_eq!(scroll_start_for_selection(0, 0, 3), 0);
        assert_eq!(scroll_start_for_selection(0, 2, 3), 0);
        assert_eq!(scroll_start_for_selection(0, 3, 3), 1);
        assert_eq!(scroll_start_for_selection(1, 6, 3), 4);
        assert_eq!(scroll_start_for_selection(4, 3, 3), 3);
        assert_eq!(scroll_start_for_selection(4, 5, 3), 4);
    }

    #[test]
    fn vertical_nav_picks_closest_center_in_adjacent_row() {
        // 首行 3 张(0,1,2),次行 4 张(3,4,5,6),第三行 2 张(7,8)——流式典型形态。
        // Rows of 3 / 4 / 2 -- the typical flow shape.
        let mut rects = row(&[0, 1, 2], 200.0);
        rects.extend(row(&[3, 4, 5, 6], 100.0));
        rects.extend(row(&[7, 8], 0.0));

        // 从 1(中心 160)下移:次行中心 110/220/330/440,最近 = 4(220)。
        // From 1 (center 160) down: row-2 centers 110/220/330/440 -> nearest is 4.
        assert_eq!(vertical_nav_index(&rects, 1, false, 160.0), Some(4));
        // 从 4(中心 220)上移:回到 1(160 比 110/330 更近)。
        // From 4 (center 220) up: back to 1 (160 beats 110/330).
        assert_eq!(vertical_nav_index(&rects, 4, true, 160.0), Some(1));
        // 从 0(中心 50)下移:最近 = 3(110)。
        // From 0 (center 50) down: nearest is 3 (110).
        assert_eq!(vertical_nav_index(&rects, 0, false, 50.0), Some(3));
        // 从 6(中心 440)下移:第三行中心 50/160,最近 = 8(160)。
        // From 6 (center 440) down: nearest in the last row is 8 (160).
        assert_eq!(vertical_nav_index(&rects, 6, false, 380.0), Some(8));
        // 保留 6 的水平锚点 380 后从 8 上移，会回到 6，而不是跟随 8 的当前中心漂到 4。
        // Retaining card 6's x anchor (380) makes 8 -> up return to 6 instead of
        // drifting toward card 4 from card 8's current center.
        assert_eq!(vertical_nav_index(&rects, 8, true, 380.0), Some(6));
    }

    #[test]
    fn vertical_nav_no_adjacent_row_is_no_op() {
        let mut rects = row(&[0, 1], 100.0);
        rects.extend(row(&[2, 3], 0.0));
        // 已在最上行:再往上无行 -> None(到边不动)。
        // Already on the top row: no row above -> None (edge = no-op).
        assert_eq!(vertical_nav_index(&rects, 0, true, 50.0), None);
        assert_eq!(vertical_nav_index(&rects, 1, true, 160.0), None);
        // 已在最下行:再往下无行 -> None。
        // Already on the bottom row: no row below -> None.
        assert_eq!(vertical_nav_index(&rects, 2, false, 50.0), None);
        // 单行场景上下都是 None。
        // A single row yields None both ways.
        let single = row(&[0, 1, 2], 100.0);
        assert_eq!(vertical_nav_index(&single, 1, true, 160.0), None);
        assert_eq!(vertical_nav_index(&single, 1, false, 160.0), None);
    }

    #[test]
    fn vertical_nav_unknown_current_is_no_op() {
        // 当前 index 不在 rects 里(理论不发生,防御) -> None。
        // A current index absent from rects (defensive) -> None.
        let rects = row(&[0, 1], 100.0);
        assert_eq!(vertical_nav_index(&rects, 99, true, 0.0), None);
    }

    #[test]
    fn page_edge_navigation_uses_the_same_horizontal_anchor() {
        let mut rects = row(&[4, 5, 6], 100.0);
        rects.extend(row(&[7, 8], 0.0));
        assert_eq!(edge_row_nav_index(&rects, true, 260.0), Some(6));
        assert_eq!(edge_row_nav_index(&rects, false, 260.0), Some(8));
    }

    #[test]
    fn empty_title_gets_app_name() {
        // 空标题只影响显示层;内部 title 不动(见函数注释,raise_ax_window 靠空标题匹配)。
        // Empty titles only affect display; the stored title is untouched (see the fn doc:
        // raise_ax_window matches by the empty title).
        assert_eq!(display_title("", "Microsoft To Do"), "Microsoft To Do");
        assert_eq!(display_title("   ", "Notes"), "   "); // 空白串不是空串 / whitespace is not empty
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
        assert_eq!(display_title("Safari — Apple", "Safari"), "Safari — Apple");
        assert_eq!(display_title("x", "App"), "x");
    }

    #[test]
    fn caption_prepends_app_name_only_when_enabled() {
        assert_eq!(card_caption("Inbox", "Mail", false), "Inbox");
        assert_eq!(card_caption("Inbox", "Mail", true), "Mail · Inbox");
        // 无标题窗口不重复显示应用名(避免 "Mail · Mail")。
        // A titleless window never repeats the app name ("Mail · Mail").
        assert_eq!(card_caption("", "Mail", true), "Mail");
        assert_eq!(card_caption("", "Mail", false), "Mail");
        // 标题与应用名文本相同时同样只显示一份,且不带分隔点(如访达的窗口)。
        // An identical title and app name also render a single copy, with no separator
        // (e.g. a Finder window).
        assert_eq!(card_caption("Finder", "Finder", true), "Finder");
        assert_eq!(card_caption("Finder", "Finder", false), "Finder");
    }

    #[test]
    fn card_reconcile_action_replaces_when_the_caption_flag_changes() {
        // 开关变化必须重建卡片:复用路径不会重绘标题行,旧标题会一直留存。
        // A toggle must rebuild the card: reuse never repaints the caption, so the old
        // title would linger.
        let mut painted = signature("same");
        painted.show_app_name_in_cards = false;
        let mut current = signature("same");
        current.show_app_name_in_cards = true;
        assert_eq!(
            card_reconcile_action(Some(&painted), &current),
            CardReconcileAction::Replace
        );
    }

    #[test]
    fn stale_delayed_order_out_does_not_hide_a_new_summon() {
        assert!(delayed_order_out_should_hide(false));
        assert!(!delayed_order_out_should_hide(true));
    }
}

// ========== 通用控件 helper / generic control helper ==========

/// 创建一个简单(非 attributed)NSTextField 标签,固定在 container_width 内水平居中,
/// 并按字体真实行高在给定区域内垂直居中。固定宽度很重要:长文本必须由 NSTextField
/// 在这个边界内尾部截断,不能用 sizeToFit 让它越过卡片边缘侵入相邻卡片。
/// Create a simple (non-attributed) NSTextField label, constrained to `container_width`,
/// centered horizontally, and vertically centered using the font's real line height. The fixed
/// width is important: long text must be tail-truncated inside the card instead of sizeToFit
/// letting it cross the card boundary and overlap the next card.
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
    let _: () = msg_send![label, setUsesSingleLineMode: true];
    let _: () = msg_send![label, setAlignment: 1isize]; // NSTextAlignmentCenter
    let _: () = msg_send![label, setFont: font];
    let _: () = msg_send![label, setTextColor: color];
    // Keep the label inside its container and truncate at the trailing edge when needed.
    // 保持标签不越过容器,超宽时从尾部截断。
    let _: () = msg_send![label, setLineBreakMode: 4isize]; // NSLineBreakByTruncatingTail
    let ascender: f64 = msg_send![font, ascender];
    let descender: f64 = msg_send![font, descender];
    let line_h = (ascender - descender + 1.0).max(11.0).min(height.max(1.0));
    let centered_y = y + (height - line_h) / 2.0;
    let _: () = msg_send![label, setFrame: NSRect::new(
        NSPoint::new(0.0, centered_y),
        NSSize::new(container_width.max(1.0), line_h)
    )];
    label
}

// ========== 窗口激活 / window activation ==========

/// 立即完成可见的快速抬窗,再把 AX 焦点兜底交给后台序列。
/// Complete the visible fast raise immediately, then enqueue the AX focus backstop.
pub(crate) fn activate_and_raise(pid: i32, cgwid: u32, minimized: bool) {
    let activation_started = Instant::now();
    window_server::note_own_focus(pid, cgwid);
    // 同应用窗口切换不会有 App 激活通知,808 又被 own-focus 静音,这里直接调度
    // 缩略图到达补拍;跨应用切换在函数内部因前台前提不成立而短路,仍走激活通知
    // 驱动的补拍链。
    // A same-app window switch produces no app-activation notification and its 808
    // is silenced as an own-focus echo, so schedule the thumbnail arrival refresh
    // right here; cross-app switches short-circuit inside (frontmost precondition
    // fails) and keep using the notification-driven refresh chain.
    crate::thumbnail::refresh_after_same_app_switch(pid, cgwid);

    let fast_path_ok = if minimized {
        false
    } else {
        let fast_started = Instant::now();
        let (slps_ok, click_ok) = raise_window_fast(pid, cgwid);
        log_debug!(
            "[raise] precise fast: pid={} cgwid={} slps={} click={} elapsed={}ms total={}ms",
            pid,
            cgwid,
            slps_ok,
            click_ok,
            fast_started.elapsed().as_millis(),
            activation_started.elapsed().as_millis()
        );
        slps_ok && click_ok
    };

    let generation = raise_window_ax_async(pid, cgwid, minimized, fast_path_ok);
    log_debug!(
        "[raise] activation enqueued: pid={} cgwid={} minimized={} gen={} total={}ms",
        pid,
        cgwid,
        minimized,
        generation,
        activation_started.elapsed().as_millis()
    );
}

// ========== 浮窗渲染 / overlay rendering ==========

pub(crate) fn update_status_label() {
    unsafe {
        let status_label = match *STATUS_LABEL.lock().unwrap() {
            Some(l) => l.0,
            None => return,
        };
        let Some(status_text) = with_tab_state(|state_opt| {
            let state = state_opt.as_ref()?;
            let selected = state.selected;
            // status_text 是窗口下面那一行长的应用名称;窗口列表为空时显示"没有可切换的窗口"提示
            // (召唤空窗口态,见 show_overlay)。
            // status_text is the long app/window title line below the cards; with an empty window
            // list it shows the "no windows to switch" hint (the empty-overlay state).
            Some(if state.windows.is_empty() {
                t("overlay.no_windows")
            } else {
                match state.windows.get(selected) {
                    Some(w) if w.window_title.is_empty() => {
                        display_title(&w.window_title, &w.app_name).to_string()
                    }
                    Some(w) => format!(
                        "{} · {}",
                        display_title(&w.window_title, &w.app_name),
                        w.app_name
                    ),
                    None => String::new(),
                }
            })
        }) else {
            return;
        };

        let colors = current_colors();
        let footer_h = status_h();
        let status_font: *mut AnyObject = {
            let status_bar_weight = CONFIG.read().unwrap().fonts.status_bar_weight;
            msg_send![class!(NSFont), systemFontOfSize: status_bar_text_size(), weight: status_bar_weight]
        };
        let status_color = hex_to_ns_color(colors.status_bar_text);
        let ns_stat = make_nsstring(&status_text);
        let _: () = msg_send![status_label, setStringValue: ns_stat];
        CFRelease(ns_stat as *const c_void);
        let _: () = msg_send![status_label, setFont: status_font];
        let _: () = msg_send![status_label, setTextColor: status_color];
        let container_w = {
            let container = CONTAINER.lock().unwrap();
            let c = container.unwrap().0;
            let f: NSRect = msg_send![c, frame];
            f.size.width
        };
        // Keep a fixed visual frame so native tail truncation is based on the actual font and
        // available width. Manual ASCII/CJK width estimates made long titles overflow or cut
        // Unicode grapheme clusters.
        // 使用固定可视 frame，让原生控件依据实际字体和可用宽度尾部截断。手工 ASCII/CJK
        // 宽度估算会导致长标题越界或切断 Unicode 组合字符。
        let _: () = msg_send![status_label, setUsesSingleLineMode: true];
        let _: () = msg_send![status_label, setLineBreakMode: 4isize]; // NSLineBreakByTruncatingTail
        let stat_w = (container_w - H_PADDING * 2.0).max(1.0);
        let stat_x = H_PADDING;
        let ascender: f64 = msg_send![status_font, ascender];
        let descender: f64 = msg_send![status_font, descender];
        let line_h = (ascender - descender + 1.0).clamp(11.0, footer_h);
        let _: () = msg_send![status_label, setFrame: NSRect::new(
            NSPoint::new(stat_x, (footer_h - line_h) / 2.0),
            NSSize::new(stat_w, line_h)
        )];
    }
}

pub(crate) fn hide_overlay() {
    stop_hover_timer();
    clear_thumbnail_scroll_drag();
    set_thumbnail_scroller_hover(false, false);
    // Drop the slot borrow before orderOut: AppKit can synchronously deliver resign-key here.
    // 在 orderOut 前结束槽位借用:AppKit 可能在这里同步派发 resign-key 通知。
    let window = overlay_window_ptr();
    unsafe {
        if let Some(window) = window {
            let _: () = msg_send![window, orderOut: std::ptr::null::<AnyObject>()];
        }
    }
    crate::performance::end_switcher_activity();
    crate::thumbnail::wake_capture_worker();
    crate::thumbnail::log_capture_metrics("dismiss");
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
    with_tab_state(|state_opt| {
        if let Some(state) = state_opt.as_mut() {
            state.visible = false;
            state.pending_first_show = false;
            state.pending_first_release = false;
        }
    });
}

// ========== 显示器配置变化 / display reconfiguration ==========

/// 显示器配置变化后延迟处理的去抖窗口:等窗口迁移/缩放动画收敛并合并连续通知
/// (一次模式切换可能连发多条),再执行一次完整刷新。
/// Debounce window before handling a display reconfiguration: let window
/// migration/scaling animations settle and coalesce notification bursts (one mode
/// switch may post several), then run a single full refresh.
const DISPLAY_RECONFIG_DELAY: f64 = 0.45;
/// 已调度去抖刷新的标记;刷新触发时清除,期间重复通知直接合并。
/// Marks a debounced refresh as already scheduled; cleared when it fires, so
/// repeated notifications within the window coalesce into one pass.
static DISPLAY_RECONFIG_REFRESH_SCHEDULED: AtomicBool = AtomicBool::new(false);
/// 窗口 bounds 后台快照落地后需要按新几何重排浮窗的标记(见 handle_display_reconfiguration)。
/// Set when the next background window-snapshot apply should re-lay out the overlay
/// against the new display geometry (see handle_display_reconfiguration).
static DISPLAY_RECONFIG_RELAYOUT_PENDING: AtomicBool = AtomicBool::new(false);

/// 通知入口(main 线程):去抖调度一次显示器配置变化后的完整刷新。
/// Notification entry point (main thread): schedule one debounced full refresh
/// after a display reconfiguration.
pub(crate) fn schedule_display_reconfiguration_refresh() {
    if DISPLAY_RECONFIG_REFRESH_SCHEDULED.swap(true, Ordering::SeqCst) {
        log_debug!("[display] reconfiguration refresh already pending; notification coalesced");
        return;
    }
    unsafe {
        let Some(controller) = *crate::CONTROLLER.lock().unwrap() else {
            DISPLAY_RECONFIG_REFRESH_SCHEDULED.store(false, Ordering::SeqCst);
            return;
        };
        log_debug!(
            "[display] reconfiguration refresh scheduled in {:.2}s",
            DISPLAY_RECONFIG_DELAY
        );
        let _: () = msg_send![
            controller.0,
            performSelector: sel!(handleDisplayReconfiguration:),
            withObject: std::ptr::null::<AnyObject>(),
            afterDelay: DISPLAY_RECONFIG_DELAY
        ];
    }
}

/// 去抖定时器到期(main 线程):执行显示器配置变化后的完整刷新。
/// Debounce timer fired (main thread): run the full post-reconfiguration refresh.
pub(crate) extern "C" fn on_display_reconfiguration(
    _self: *mut c_void,
    _cmd: Sel,
    _arg: *mut c_void,
) {
    DISPLAY_RECONFIG_REFRESH_SCHEDULED.store(false, Ordering::SeqCst);
    handle_display_reconfiguration();
}

/// 显示器配置变化(外接/内建切换、分辨率调整)后的统一刷新入口:
/// 1. 发起一次后台窗口快照,让 TAB_STATE 拿到变化后的窗口 bounds(宽高比/所属屏
///    会变);浮窗可见时标记快照落地后重排,卡片宽度按新比例精修。
/// 2. 浮窗可见时立即 show_overlay 重排:面板宽高与居中全部按实时屏幕几何重算
///    (拔掉显示器后面板可能悬在旧位置),滚动偏移在内部 clamp 到新范围。
/// 3. 强制重拍全部已知窗口缩略图:缓存帧是旧配置下的比例与像素高度,重拍后由
///    thumbnailReady 原位换卡。
///
/// Single refresh entry after a display reconfiguration (external/built-in
/// switch or resolution change):
/// 1. Kick a background window snapshot so TAB_STATE receives post-change
///    bounds (aspect and owning screen may change); when the overlay is
///    visible, mark the apply to re-layout so card widths follow the new
///    aspects.
/// 2. When visible, re-layout via show_overlay immediately: panel width/height
///    and centering are recomputed from live screen geometry (after an unplug
///    the panel could otherwise hover where the old screen was); the scroll
///    offset is clamped to the new range inside.
/// 3. Force a recapture of every known window thumbnail: cached frames carry
///    the old configuration's aspect and pixel height; deliveries swap cards
///    in place.
pub(crate) fn handle_display_reconfiguration() {
    let overlay_visible =
        with_tab_state(|state_opt| state_opt.as_ref().is_some_and(|state| state.visible));
    log_info!(
        "[overlay] display reconfiguration: overlay_visible={} screens={}",
        overlay_visible,
        unsafe {
            let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
            let count: usize = msg_send![screens, count];
            count
        }
    );
    // 必须在不持有 TAB_STATE 时发起快照(request_window_refresh 内部会再锁它)。
    // The snapshot request must run WITHOUT holding TAB_STATE (the refresh locks it again).
    request_window_refresh();
    if overlay_visible {
        // 先精修 bounds 再重排:快照落地时消费该标记,即使用户窗口集合未变也重排。
        // Refine bounds first: the snapshot apply consumes this flag and re-lays
        // out even when the window set itself is unchanged.
        DISPLAY_RECONFIG_RELAYOUT_PENDING.store(true, Ordering::SeqCst);
        // 立即按实时屏幕几何重排(bounds 仍是旧值,但面板尺寸/位置/捕获像素需求
        // 已经正确);落地后的第二次重排修正卡片比例。
        // Re-layout against live screen geometry right away (bounds are stale but
        // panel size/position and capture pixel demand are already correct); the
        // post-snapshot second pass corrects card aspects.
        show_overlay();
    }
    let target_px_h = *THUMB_CAPTURE_TARGET_PX_H.lock().unwrap();
    crate::thumbnail::refresh_for_display_change(target_px_h);
}

/// 快照落地路径消费:显示器配置变化后即使窗口集合未变也要重排一次浮窗。
/// Consumed by the snapshot-apply path: after a display reconfiguration the overlay
/// must re-layout once even when the window set is unchanged.
pub(crate) fn take_display_relayout_pending() -> bool {
    DISPLAY_RECONFIG_RELAYOUT_PENDING.swap(false, Ordering::SeqCst)
}
