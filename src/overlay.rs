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
use crate::window_refresh::{log_window_ordering, request_window_refresh};
use crate::AppState;
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
/// NSEventModifierFlagShift，与 CGEvent 的 Shift 位一致。
/// NSEventModifierFlagShift; it shares the Shift bit with CGEvent flags.
const NSEVENT_MODIFIER_FLAG_SHIFT: u64 = 0x0002_0000;
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
const SELECTED_PREVIEW_BORDER_ALPHA: u8 = 0x57;
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

pub(crate) static OVERLAY_WINDOW: Mutex<Option<ObjPtr>> = Mutex::new(None);
pub(crate) static CONTAINER: Mutex<Option<ObjPtr>> = Mutex::new(None);
/// 持久的卡片 document view;滚动时只移动 CONTAINER 的 bounds,不重建卡片树。
/// Persistent card document view; scrolling moves CONTAINER bounds instead of rebuilding cards.
pub(crate) static CARD_DOCUMENT: Mutex<Option<ObjPtr>> = Mutex::new(None);
pub(crate) static STATUS_LABEL: Mutex<Option<ObjPtr>> = Mutex::new(None);
/// 缩略图溢出时显示的原生竖向滚动条。
/// Native vertical scroller shown when the thumbnail rows overflow the viewport.
pub(crate) static THUMB_SCROLLER: Mutex<Option<ObjPtr>> = Mutex::new(None);
/// macOS 26+ 的 NSGlassEffectView 指针(用于设置热重载时重新应用玻璃属性)。
/// Pointer to the NSGlassEffectView on macOS 26+ (used to re-apply glass properties on hot reload).
pub(crate) static GLASS_VIEW: Mutex<Option<ObjPtr>> = Mutex::new(None);
pub(crate) static CARD_CLASS: Mutex<Option<ObjClassPtr>> = Mutex::new(None);

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
        *CARD_CLASS.lock().unwrap() = Some(ObjClassPtr(cls as *const objc2::runtime::AnyClass));
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
    original_frames: HashMap<WindowKey, NSRect>,
    final_frames: HashMap<WindowKey, NSRect>,
    final_row_ranges: Vec<Range<usize>>,
    original_document_h: f64,
    final_document_h: f64,
}

static PENDING_CARD_CLOSE: Mutex<Option<PendingCardClose>> = Mutex::new(None);
type WindowKey = (i32, u32);

#[derive(Clone, Debug, PartialEq, Eq)]
struct CardSignature {
    app_name: String,
    window_title: String,
    icon_path: Option<String>,
    minimized: bool,
    card_width_bits: u64,
    card_height_bits: u64,
    thumbnail_layout: bool,
    thumbnail_capture_allowed: bool,
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
        card_width_bits: frame.size.width.to_bits(),
        card_height_bits: frame.size.height.to_bits(),
        thumbnail_layout,
        thumbnail_capture_allowed,
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

pub(crate) fn thumbnail_scroller() -> Option<ObjPtr> {
    *THUMB_SCROLLER.lock().unwrap()
}

mod card_close;
mod hover;

use card_close::*;
pub(crate) use card_close::{
    begin_close_window_at, card_close_in_progress, card_mouse_down, card_mouse_entered,
    on_card_close_ax_result, on_card_close_finished, on_close_card, on_cmd_released,
};
use hover::*;
pub(crate) use hover::{container_mouse_moved, on_deferred_scroll_hover};

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

#[cfg(test)]
mod tests {
    use super::card_reconcile_action;
    use super::color_with_alpha;
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
            card_width_bits: 100.0f64.to_bits(),
            card_height_bits: 100.0f64.to_bits(),
            thumbnail_layout: true,
            thumbnail_capture_allowed: true,
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

// ========== ObjC 回调实现 / ObjC callback implementations ==========

/// 首帧一次性显示:用当前(已刷新的)窗口列表做首次选中并弹出浮窗。
/// 由 apply_window_refresh 在消费 pending_first_show 时调用,保证「一次成图」——
/// 显示的就是刷新后的最终排序,不存在「先显示旧快照、再重排」的两段跳变。
/// First-frame single-shot show: pick the initial selection over the (refreshed) window list and
/// pop the overlay. Called by apply_window_refresh when it consumes pending_first_show so the
/// render is single-shot — the shown order is already the final one, no "stale then reorder" jump.
pub(crate) fn show_first_summon(backward: bool) {
    prepare_first_summon_state(backward);
    let t_show = Instant::now();
    show_overlay();
    // TIMING-DEBUG 端到端:tap 回调 → 收集完成 → show_overlay。
    log_debug!("[overlay] summon e2e={}ms", t_show.elapsed().as_millis());
}

/// Prepare the first summon selection without deciding whether the panel should be displayed.
///
/// 首帧选中状态的准备与显示分开,这样在等待快照时收到 CmdReleased 可以直接提交目标,
/// 而不必先短暂显示再隐藏浮窗。
fn prepare_first_summon_state(backward: bool) {
    let mut state_opt = TAB_STATE.lock().unwrap();
    let state = state_opt.as_mut().unwrap();
    state.visible = true;
    let (_, frontmost_pid) = frontmost_app_info();
    let frontmost_pid = (frontmost_pid > 0).then_some(frontmost_pid);
    let focus_key = state
        .focus_key
        .filter(|(pid, _)| frontmost_pid == Some(*pid));
    state.selected = prepare_first_summon(
        &mut state.windows,
        &mut state.mru,
        backward,
        frontmost_pid,
        focus_key,
        Instant::now(),
    );
    // 记录召唤瞬间的窗口 key 集合:浮窗打开后的刷新只知道哪些窗口「召唤时就在场」。
    state.summon_keys = Some(state.windows.iter().map(|w| (w.pid, w.window_id)).collect());
    // 首帧默认选中:锁定到「召唤时选中的目标窗口」,刷新不因 MRU 排序变化改选。
    state.user_picked = false;
    state.selected_target_key = state
        .windows
        .get(state.selected)
        .map(|w| (w.pid, w.window_id));
    log_debug!(
        "[overlay] first summon: frontmost_pid={:?} focus_key={:?} selected={} windows={}",
        frontmost_pid,
        focus_key,
        state.selected,
        state.windows.len()
    );
    log_window_ordering(
        "first summon order",
        &state.windows,
        &state.mru,
        state.selected,
    );
    drop(state_opt);
    reset_thumbnail_visible_range();
    reset_thumbnail_scroll();
    reset_thumbnail_nav_anchor();
    MOUSE_MOVED.store(false, Ordering::Relaxed);
    *HOVER_TICK_POS.lock().unwrap() = None;
}

/// Commit a first summon whose Cmd release arrived before the first frame was ready.
/// The target selection is prepared, but the overlay is never ordered on screen.
pub(crate) fn commit_first_summon(backward: bool) {
    prepare_first_summon_state(backward);
    commit_selected_window(false);
}

fn step_switcher(backward: bool) {
    let mut state_opt = TAB_STATE.lock().unwrap();
    let state_ref = state_opt.as_ref().unwrap();
    let pending = state_ref.pending_first_show;
    let first_show = !state_ref.visible && !pending;

    if pending {
        // 首帧快照仍在后台收集:本次浮窗尚未显示,重复 Tab 无法基于旧快照定位,先忽略,
        // 等 apply_window_refresh 一次性显示后再由用户续按。
        // The first snapshot is still being collected: the overlay isn't shown yet, so another Tab
        // can't be positioned over the stale list — ignore it; let the user continue once the
        // single-shot show lands.
        log_debug!("[overlay] re-Tab during pending first show ignored");
        return;
    }

    if first_show {
        // 首帧:不再先显示旧快照,而是发起后台刷新并标记「待显示」,等 apply_window_refresh
        // 拿到首帧快照后一次性显示(一次成图)。注意:发起刷新必须释放 TAB_STATE 锁,否则
        // request_window_refresh 内部同样要锁 TAB_STATE,造成自死锁(主线程永远阻塞)。
        // First frame: don't show the stale startup snapshot first. Kick off a background refresh
        // and mark pending_first_show; apply_window_refresh consumes it and shows once the first
        // snapshot is ready (single-shot render). NB: the refresh must be kicked off AFTER dropping
        // TAB_STATE, otherwise request_window_refresh re-locks it and deadlocks the main thread.
        drop(state_opt);
        crate::performance::begin_switcher_activity();
        request_window_refresh();
        let mut state_opt = TAB_STATE.lock().unwrap();
        let state = state_opt.as_mut().unwrap();
        state.visible = false;
        state.pending_first_show = true;
        state.pending_first_backward = backward;
        state.pending_first_release = false;
        schedule_first_summon_timeout();
        // TIMING-DEBUG 端到端:tap 回调 → 收集完成 → show_first_summon。
        log_debug!("[overlay] first summon pending (awaiting snapshot)");
        drop(state_opt);
    } else {
        // 用户主动导航(重复按 Tab):选中不再是首帧默认落点,标记 user_picked 并钉住当前目标。
        // User-initiated navigation (repeated Tab): the pick is no longer the first-frame default;
        // mark user_picked and pin to the current target.
        let state = state_opt.as_mut().unwrap();
        state.selected = horizontal_nav_index(state.selected, state.windows.len(), backward);
        mark_user_picked(state);
        drop(state_opt);
        reset_thumbnail_nav_anchor();
        refresh_after_selection_change(true);
    }
}

const FIRST_SUMMON_FALLBACK_DELAY: f64 = 0.12;

/// Give a slow AX refresh a short deadline so the switcher can still respond to a held/released
/// Cmd using the last coherent snapshot. A later refresh result reconciles the visible list.
fn schedule_first_summon_timeout() {
    unsafe {
        let Some(controller) = *crate::CONTROLLER.lock().unwrap() else {
            return;
        };
        let _: () = msg_send![
            controller.0,
            performSelector: sel!(handleFirstSummonTimeout:),
            withObject: std::ptr::null::<AnyObject>(),
            afterDelay: FIRST_SUMMON_FALLBACK_DELAY
        ];
    }
}

/// Main-thread deadline for a first summon. The callback is harmless when the real snapshot has
/// already arrived because apply_window_refresh clears pending_first_show first.
pub(crate) extern "C" fn on_first_summon_timeout(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    let request = {
        let mut state_opt = TAB_STATE.lock().unwrap();
        let Some(state) = state_opt.as_mut() else {
            return;
        };
        if !state.pending_first_show {
            return;
        }
        state.pending_first_show = false;
        let backward = state.pending_first_backward;
        let release_pending = state.pending_first_release;
        state.pending_first_release = false;
        Some((backward, release_pending))
    };

    if let Some((backward, release_pending)) = request {
        log_debug!(
            "[overlay] first summon deadline reached (release_pending={})",
            release_pending
        );
        if release_pending {
            commit_first_summon(backward);
        } else {
            show_first_summon(backward);
        }
    }
}

/// 用户主动改变了选中(导航/点击/悬停):标记 user_picked 并钉住当前选中窗口 key。
/// 此后刷新将按该目标窗口恢复选中,再也不随列表重排漂移。调用方须已持有 TAB_STATE。
/// User actively changed the selection (nav/click/hover): mark user_picked and pin to the newly
/// selected window key. Subsequent refreshes restore the pick to that target instead of drifting
/// with a reorder. Caller must already hold TAB_STATE.
fn mark_user_picked(state: &mut AppState) {
    state.user_picked = true;
    state.selected_target_key = state
        .windows
        .get(state.selected)
        .map(|w| (w.pid, w.window_id));
}

pub(crate) extern "C" fn on_cmd_tab_pressed(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    step_switcher(false);
}

pub(crate) extern "C" fn on_cmd_shift_tab_pressed(
    _self: *mut c_void,
    _cmd: Sel,
    _arg: *mut c_void,
) {
    step_switcher(true);
}

/// 选中项越过当前视口时只移动 clip bounds,不重建卡片树;两种布局共用。
/// Move clip bounds when selection leaves the viewport; both layouts share this path and never
/// rebuild the card tree.
fn refresh_after_selection_change(backfill_icons: bool) {
    let selected = TAB_STATE
        .lock()
        .unwrap()
        .as_ref()
        .map(|state| state.selected);
    let needs_relayout = selected.is_some_and(|index| {
        !THUMB_VISIBLE_RANGE
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|range| range.contains(&index))
    });
    if needs_relayout {
        if let Some(index) = selected {
            if ensure_thumbnail_selection_visible(index) {
                apply_thumbnail_scroll_offset();
            }
        }
    }
    refresh_highlight();
    update_status_label();
    if backfill_icons {
        extract_uncached_icons();
    }
}

/// 从浮窗容器收集每张卡片的 (index, x, y, width)(按实际 frame,跳过状态栏标签)。
/// Collect (index, x, y, width) for every card from the live container subviews
/// (actual frames; the status-bar labels are skipped).
unsafe fn collect_card_rects() -> Vec<(usize, f64, f64, f64)> {
    let document = match card_document() {
        Some(document) => document,
        None => return Vec::new(),
    };
    let mut out: Vec<(usize, f64, f64, f64)> = Vec::new();
    for sv in card_views(document) {
        let Some(idx) = get_card_index(sv) else {
            continue;
        };
        let f: NSRect = msg_send![sv, frame];
        out.push((idx, f.origin.x, f.origin.y, f.size.width));
    }
    out
}

/// 几何感知的垂直导航(纯函数,可单测):跳到相邻行中水平中心最接近固定锚点的
/// 那一张。锚点在连续上下移动期间不变，因此下再上可以回到原列附近。
/// 返回 None 表示该方向没有相邻行(保持"到边不动"的语义)。
/// 行聚类按 y 值 + 1.0pt 容差(同一行的卡片 y 完全相同,容差只防浮点漂移)。
///
/// Geometry-aware vertical navigation (pure, unit-testable): jump to the card in
/// the adjacent row whose horizontal center is closest to a stable anchor.
/// Flow rows hold different card counts, so a fixed step misaligns or runs off
/// the end. None = no adjacent row in that direction (edge = no-op semantics).
/// Rows cluster by y with a 1pt epsilon (same-row cards share y exactly; the
/// epsilon only guards float drift).
fn vertical_nav_index(
    rects: &[(usize, f64, f64, f64)],
    current: usize,
    up: bool,
    anchor_x: f64,
) -> Option<usize> {
    const ROW_EPS: f64 = 1.0;
    let (_, _, cy, _) = rects.iter().find(|(i, ..)| *i == current)?;
    let cur_y = cy;

    // 相邻行:同方向里 y 最接近当前行的那个。
    // The adjacent row: nearest y in the requested direction.
    let mut best_row_y: Option<f64> = None;
    for (_, _, y, _) in rects {
        let dy = y - cur_y;
        let in_direction = if up { dy > ROW_EPS } else { dy < -ROW_EPS };
        if !in_direction {
            continue;
        }
        best_row_y = Some(match best_row_y {
            Some(by) if up => by.min(*y),
            Some(by) => by.max(*y),
            None => *y,
        });
    }
    let target_y = best_row_y?;

    // 目标行内取水平中心最近者(平分取先出现者)。
    // Within the target row, pick the closest horizontal center (ties -> first).
    rects
        .iter()
        .filter(|(_, _, y, _)| (y - target_y).abs() <= ROW_EPS)
        .min_by(|a, b| {
            let da = ((a.1 + a.3 / 2.0) - anchor_x).abs();
            let db = ((b.1 + b.3 / 2.0) - anchor_x).abs();
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, ..)| *i)
}

fn card_center_x(rects: &[(usize, f64, f64, f64)], index: usize) -> Option<f64> {
    rects
        .iter()
        .find(|(i, ..)| *i == index)
        .map(|(_, x, _, width)| x + width / 2.0)
}

#[cfg(test)]
fn edge_row_nav_index(rects: &[(usize, f64, f64, f64)], top: bool, anchor_x: f64) -> Option<usize> {
    const ROW_EPS: f64 = 1.0;
    let target_y =
        rects
            .iter()
            .map(|(_, _, y, _)| *y)
            .reduce(|a, b| if top { a.max(b) } else { a.min(b) })?;
    rects
        .iter()
        .filter(|(_, _, y, _)| (y - target_y).abs() <= ROW_EPS)
        .min_by(|a, b| {
            let da = ((a.1 + a.3 / 2.0) - anchor_x).abs();
            let db = ((b.1 + b.3 / 2.0) - anchor_x).abs();
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(index, ..)| *index)
}

/// 两种布局的上下导航:完整 document 中按固定水平锚点移动,越过视口时只移动 clip bounds。
/// Vertical navigation for both layouts uses the complete document and a stable horizontal
/// anchor; crossing the viewport only moves clip bounds.
unsafe fn navigate_thumbnail_vertical(rects: &[(usize, f64, f64, f64)], up: bool) {
    let mut state_opt = TAB_STATE.lock().unwrap();
    let Some(state) = state_opt.as_mut() else {
        return;
    };
    if !state.visible || state.windows.is_empty() {
        return;
    }
    let Some(current_center) = card_center_x(rects, state.selected) else {
        return;
    };
    let anchor_x = {
        let mut anchor = THUMB_NAV_ANCHOR_X.lock().unwrap();
        *anchor.get_or_insert(current_center)
    };
    if let Some(index) = vertical_nav_index(rects, state.selected, up, anchor_x) {
        state.selected = index;
        mark_user_picked(state);
        drop(state_opt);
        if ensure_thumbnail_selection_visible(index) {
            apply_thumbnail_scroll_offset();
        }
        refresh_highlight();
        update_status_label();
    }
}

/// 设置 CALayer.shadowColor。CGColorRef 与 CGImageRef 同理不能进 objc2 的
/// msg_send!(参数编码 '@' vs '^{CGColor=}' 被运行时拒绝),照 layer_set_background
/// 惯例走裸 objc_msgSend。
/// Set CALayer.shadowColor. A CGColorRef, like CGImageRef, cannot go through
/// objc2's msg_send! (arg encoding '@' vs '^{CGColor=}' rejected at runtime);
/// raw objc_msgSend per the layer_set_background convention.
unsafe fn layer_set_shadow_color(layer: *mut AnyObject, cg: *mut c_void) {
    let sel = sel!(setShadowColor:);
    type F = unsafe extern "C" fn(*mut c_void, Sel, *mut c_void);
    let f: F = std::mem::transmute(objc_msgSend as *const ());
    f(layer as *mut c_void, sel, cg);
}

/// 用 CALayer 的 KVC 子键设置二维平移，避免把 CATransform3D 结构体传进 objc2
/// `msg_send!` 的运行时编码校验。父层变换会携带背景、描边、阴影与全部子视图，
/// 同时不改 NSView frame，因此导航几何和原位卡片重建仍使用稳定基准。
/// Set 2D translation through CALayer's KVC sub-key, avoiding CATransform3D in objc2's
/// runtime-checked `msg_send!`. Transforming the parent carries its background, border,
/// shadow, and all subviews without changing the NSView frame, so navigation geometry and
/// in-place card rebuilding retain a stable baseline.
unsafe fn layer_set_translation_y(layer: *mut AnyObject, y: f64) {
    let value: *mut AnyObject = msg_send![class!(NSNumber), numberWithDouble: y];
    let key = make_nsstring("transform.translation.y");
    let _: () = msg_send![layer, setValue: value, forKeyPath: key];
    CFRelease(key as *const c_void);
}

pub(crate) extern "C" fn container_key_down(_self: *mut c_void, _cmd: Sel, event: *mut c_void) {
    unsafe {
        let key_code: u16 = msg_send![event as *mut AnyObject, keyCode];
        let modifier_flags: u64 = msg_send![event as *mut AnyObject, modifierFlags];
        let shift_pressed = modifier_flags & NSEVENT_MODIFIER_FLAG_SHIFT != 0;
        // 几何导航的 frame 收集先于 TAB_STATE 加锁:避免 CONTAINER/TAB_STATE 交叉
        // 持有(锁序与其他路径相反会造成理论死锁)。
        // Collect nav frames BEFORE taking TAB_STATE: avoids holding CONTAINER and
        // TAB_STATE across each other (inverted lock order vs other paths).
        let nav_rects = collect_card_rects();
        let mut state_opt = TAB_STATE.lock().unwrap();
        let state = state_opt.as_mut().unwrap();

        if !state.visible {
            return;
        }

        match key_code {
            KEY_TAB => {
                if !state.windows.is_empty() {
                    state.selected =
                        horizontal_nav_index(state.selected, state.windows.len(), shift_pressed);
                    mark_user_picked(state);
                    drop(state_opt);
                    reset_thumbnail_nav_anchor();
                    refresh_after_selection_change(false);
                }
            }
            KEY_RIGHT => {
                if !state.windows.is_empty() {
                    state.selected =
                        horizontal_nav_index(state.selected, state.windows.len(), false);
                    mark_user_picked(state);
                    drop(state_opt);
                    reset_thumbnail_nav_anchor();
                    refresh_after_selection_change(false);
                }
            }
            KEY_LEFT => {
                if !state.windows.is_empty() {
                    state.selected =
                        horizontal_nav_index(state.selected, state.windows.len(), true);
                    mark_user_picked(state);
                    drop(state_opt);
                    reset_thumbnail_nav_anchor();
                    refresh_after_selection_change(false);
                }
            }
            KEY_UP => {
                if state.windows.is_empty() {
                    return;
                }
                drop(state_opt);
                navigate_thumbnail_vertical(&nav_rects, true);
            }
            KEY_DOWN => {
                if state.windows.is_empty() {
                    return;
                }
                drop(state_opt);
                navigate_thumbnail_vertical(&nav_rects, false);
            }
            KEY_DELETE => {
                // Backspace:关闭选中卡片对应的窗口,浮窗保持打开。
                // Backspace: close the selected card's window; the overlay stays open.
                if !state.windows.is_empty() {
                    let idx = state.selected;
                    drop(state_opt);
                    let card = card_document().and_then(|document| {
                        card_views(document)
                            .into_iter()
                            .find(|card| get_card_index(*card) == Some(idx))
                    });
                    if let Some(card) = card {
                        begin_close_window_at(idx, card);
                    } else {
                        close_window_at(idx);
                    }
                }
            }
            KEY_RETURN => {
                if let Some(w) = state.windows.get(state.selected) {
                    let pid = w.pid;
                    let cgwid = w.window_id;
                    let minimized = w.minimized;
                    vanish_overlay();
                    // 同 on_cmd_released:设置窗口无需特殊处理(见该处注释);抬升延迟一拍执行。
                    // Same as on_cmd_released: no settings-window handling needed (see comment
                    // there); the raise is deferred by one runloop turn so the vanish commits first.
                    schedule_deferred_raise(pid, cgwid, minimized);
                    schedule_delayed_order_out();
                    state.focus_key = Some((pid, cgwid));
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

/// 两种布局都接收鼠标滚轮和触控板滚动,保留 point 级增量而不是量化为整行。
/// Both layouts handle mouse-wheel and trackpad scrolling, preserving point-level deltas instead
/// of quantizing them to whole rows.
pub(crate) extern "C" fn container_scroll_wheel(_self: *mut c_void, _cmd: Sel, event: *mut c_void) {
    unsafe {
        let delta_y: f64 = msg_send![event as *mut AnyObject, scrollingDeltaY];
        if delta_y.abs() < f64::EPSILON {
            return;
        }
        let precise: bool = msg_send![event as *mut AnyObject, hasPreciseScrollingDeltas];
        if precise {
            scroll_thumbnail_by_offset(-delta_y);
        } else {
            // 离散鼠标滚轮仍按一个小的 point 步长前进,而不是直接跳到下一行。
            // Discrete mouse wheels still advance by a small point step instead of jumping to the
            // next row immediately.
            const DISCRETE_SCROLL_STEP: f64 = 40.0;
            scroll_thumbnail_by_offset(-delta_y.signum() * DISCRETE_SCROLL_STEP);
        }
    }
}

pub(crate) extern "C" fn container_mouse_entered(
    _self: *mut c_void,
    _cmd: Sel,
    event: *mut c_void,
) {
    unsafe {
        let point: NSPoint = msg_send![event as *mut AnyObject, locationInWindow];
        update_thumbnail_pointer_state(point);
    }
}

pub(crate) extern "C" fn container_mouse_exited(_self: *mut c_void, _cmd: Sel, event: *mut c_void) {
    unsafe {
        let point: NSPoint = msg_send![event as *mut AnyObject, locationInWindow];
        update_thumbnail_pointer_state(point);
    }
}

pub(crate) extern "C" fn thumbnail_scroller_mouse_entered(
    _self: *mut c_void,
    _cmd: Sel,
    event: *mut c_void,
) {
    unsafe {
        let point: NSPoint = msg_send![event as *mut AnyObject, locationInWindow];
        update_thumbnail_pointer_state(point);
    }
}

pub(crate) extern "C" fn thumbnail_scroller_mouse_moved(
    _self: *mut c_void,
    _cmd: Sel,
    event: *mut c_void,
) {
    unsafe {
        let point: NSPoint = msg_send![event as *mut AnyObject, locationInWindow];
        update_thumbnail_pointer_state(point);
    }
}

pub(crate) extern "C" fn thumbnail_scroller_mouse_exited(
    _self: *mut c_void,
    _cmd: Sel,
    event: *mut c_void,
) {
    unsafe {
        let point: NSPoint = msg_send![event as *mut AnyObject, locationInWindow];
        update_thumbnail_pointer_state(point);
    }
}

/// HTML 参考稿的可见滑块宽度;命中区域仍由外层 14pt 视图提供。
/// Visible thumb width from the HTML reference; the outer 14pt view remains the hit area.
const THUMB_SCROLLBAR_VISIBLE_W: f64 = 5.0;
/// HTML 参考稿的上下留白在原生浮窗中放大到 6pt,避免胶囊视觉上贴住边缘。
/// Increase the HTML reference's edge inset to 6pt in the native panel so the capsule never looks flush with the viewport.
const THUMB_SCROLLBAR_EDGE: f64 = 22.0;
const THUMB_SCROLLBAR_MIN_KNOB_H: f64 = 24.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ThumbnailScrollerGeometry {
    pub(crate) knob_y: f64,
    pub(crate) knob_h: f64,
    pub(crate) thumb_travel: f64,
}

/// 用完整滚动范围计算胶囊位置;绘制和拖拽必须共享这套几何。
/// Compute the capsule from the complete scroll range; drawing and dragging must share it.
pub(crate) fn thumbnail_scroller_geometry(
    track_h: f64,
    max_offset: f64,
    offset: f64,
) -> Option<ThumbnailScrollerGeometry> {
    if !track_h.is_finite() || !max_offset.is_finite() || max_offset <= f64::EPSILON {
        return None;
    }
    let track_h = track_h - THUMB_SCROLLBAR_EDGE * 2.0;
    if track_h <= 0.0 {
        return None;
    }
    let knob_h =
        (track_h * track_h / (track_h + max_offset)).clamp(THUMB_SCROLLBAR_MIN_KNOB_H, track_h);
    let thumb_travel = (track_h - knob_h).max(0.0);
    let progress = (offset / max_offset).clamp(0.0, 1.0);
    Some(ThumbnailScrollerGeometry {
        // AppKit coordinates grow upward: offset 0 is the visual top of the content.
        // AppKit 坐标向上增长:offset 0 对应内容视觉上的顶部。
        knob_y: THUMB_SCROLLBAR_EDGE + (1.0 - progress) * thumb_travel,
        knob_h,
        thumb_travel,
    })
}

unsafe fn update_thumbnail_pointer_state(window_point: NSPoint) {
    let Some(container) = (*CONTAINER.lock().unwrap()).map(|container| container.0) else {
        set_thumbnail_scroller_hover(false, false);
        return;
    };
    let container_point: NSPoint = msg_send![
        container,
        convertPoint: window_point,
        fromView: std::ptr::null::<AnyObject>()
    ];
    let container_bounds: NSRect = msg_send![container, bounds];
    let in_viewport = container_point.x >= container_bounds.origin.x
        && container_point.x <= container_bounds.origin.x + container_bounds.size.width
        && container_point.y >= container_bounds.origin.y
        && container_point.y <= container_bounds.origin.y + container_bounds.size.height;

    let Some(scroller) = thumbnail_scroller() else {
        set_thumbnail_scroller_hover(in_viewport, false);
        return;
    };
    let scroller_point: NSPoint = msg_send![
        scroller.0,
        convertPoint: window_point,
        fromView: std::ptr::null::<AnyObject>()
    ];
    let scroller_bounds: NSRect = msg_send![scroller.0, bounds];
    let in_scroller = scroller_point.x >= scroller_bounds.origin.x
        && scroller_point.x <= scroller_bounds.origin.x + scroller_bounds.size.width
        && scroller_point.y >= scroller_bounds.origin.y
        && scroller_point.y <= scroller_bounds.origin.y + scroller_bounds.size.height;
    let max_offset = *THUMB_SCROLL_MAX_OFFSET.lock().unwrap();
    let offset = *THUMB_SCROLL_OFFSET.lock().unwrap();
    let knob = thumbnail_scroller_geometry(scroller_bounds.size.height, max_offset, offset)
        .is_some_and(|geometry| {
            in_scroller && thumbnail_scroller_knob_contains(scroller_point.y, geometry)
        });
    set_thumbnail_scroller_hover(in_viewport || in_scroller, knob);
}

fn thumbnail_scroll_offset_for_drag(
    start_offset: f64,
    start_y: f64,
    current_y: f64,
    max_offset: f64,
    thumb_travel: f64,
) -> f64 {
    if max_offset <= 0.0 || thumb_travel <= 0.0 {
        return start_offset.clamp(0.0, max_offset.max(0.0));
    }
    (start_offset + (start_y - current_y) * max_offset / thumb_travel).clamp(0.0, max_offset)
}

/// 只绘制滚动条胶囊;透明的整个指示器视图负责命中和显式拖拽。
/// Draw only the scrollbar capsule; the transparent indicator view owns hit testing and explicit dragging.
pub(crate) extern "C" fn thumbnail_scroller_draw_rect(
    scroller: *mut c_void,
    _cmd: Sel,
    _dirty_rect: NSRect,
) {
    unsafe {
        let scroller = scroller as *mut AnyObject;
        let bounds: NSRect = msg_send![scroller, bounds];
        let offset = *THUMB_SCROLL_OFFSET.lock().unwrap();
        let max_offset = *THUMB_SCROLL_MAX_OFFSET.lock().unwrap();
        let Some(geometry) = thumbnail_scroller_geometry(bounds.size.height, max_offset, offset)
        else {
            return;
        };
        let inset_x = ((bounds.size.width - THUMB_SCROLLBAR_VISIBLE_W) / 2.0).max(0.0);
        let capsule = NSRect::new(
            NSPoint::new(inset_x, geometry.knob_y),
            NSSize::new(
                THUMB_SCROLLBAR_VISIBLE_W.min(bounds.size.width),
                geometry.knob_h,
            ),
        );
        let dark = match CONFIG.read().unwrap().appearance.theme.as_str() {
            "light" => false,
            "dark" => true,
            _ => system_dark_mode(),
        };
        let hover = *THUMB_SCROLLER_HOVER.lock().unwrap();
        let dragging = THUMB_SCROLL_DRAG.lock().unwrap().is_some();
        let alpha = thumbnail_scroller_alpha(hover.viewport, hover.knob, dragging);
        let color: *mut AnyObject = if dark {
            msg_send![class!(NSColor), colorWithWhite: 1.0f64, alpha: alpha]
        } else {
            let color = 0x464E5C00 | u32::from((alpha * 255.0).round() as u8);
            hex_to_ns_color(color)
        };
        let _: () = msg_send![color, set];
        let path: *mut AnyObject = msg_send![
            class!(NSBezierPath),
            bezierPathWithRoundedRect: capsule,
            xRadius: THUMB_SCROLLBAR_VISIBLE_W / 2.0,
            yRadius: THUMB_SCROLLBAR_VISIBLE_W / 2.0
        ];
        let _: () = msg_send![path, fill];
    }
}

/// 非激活浮窗第一次点击也必须交给滚动条,否则按住 Command 时首个拖拽按下会被窗口层丢弃。
/// A nonactivating panel must deliver the first click to the scroller, otherwise the initial
/// drag press is discarded while Command is held.
pub(crate) extern "C" fn thumbnail_scroller_accepts_first_mouse(
    _self: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) -> bool {
    true
}

/// 在非激活面板中显式开始拖拽,不依赖 NSScroller 的原生 tracking。
/// Start dragging explicitly inside the nonactivating panel instead of relying on NSScroller tracking.
pub(crate) extern "C" fn thumbnail_scroller_mouse_down(
    _self: *mut c_void,
    _cmd: Sel,
    event: *mut c_void,
) {
    unsafe {
        let scroller = _self as *mut AnyObject;
        let location: NSPoint = msg_send![event as *mut AnyObject, locationInWindow];
        update_thumbnail_pointer_state(location);
        let point: NSPoint = msg_send![
            scroller,
            convertPoint: location,
            fromView: std::ptr::null::<AnyObject>()
        ];
        let bounds: NSRect = msg_send![scroller, bounds];
        let max_offset = *THUMB_SCROLL_MAX_OFFSET.lock().unwrap();
        let current_offset = *THUMB_SCROLL_OFFSET.lock().unwrap();
        let Some(geometry) =
            thumbnail_scroller_geometry(bounds.size.height, max_offset, current_offset)
        else {
            return;
        };
        let inside_knob = thumbnail_scroller_knob_contains(point.y, geometry);
        let start_offset = if inside_knob || geometry.thumb_travel <= 0.0 {
            current_offset
        } else {
            let target_progress =
                ((geometry.knob_y + geometry.thumb_travel + geometry.knob_h / 2.0 - point.y)
                    / geometry.thumb_travel)
                    .clamp(0.0, 1.0);
            target_progress * max_offset
        };
        if !inside_knob {
            set_thumbnail_scroll_offset(start_offset, max_offset);
        }
        *THUMB_SCROLL_DRAG.lock().unwrap() = Some(ThumbnailScrollDrag {
            start_y: point.y,
            start_offset,
            max_offset,
            thumb_travel: geometry.thumb_travel,
        });
        invalidate_thumbnail_scroller();
    }
}

pub(crate) extern "C" fn thumbnail_scroller_mouse_dragged(
    _self: *mut c_void,
    _cmd: Sel,
    event: *mut c_void,
) {
    unsafe {
        let Some(drag) = *THUMB_SCROLL_DRAG.lock().unwrap() else {
            return;
        };
        let scroller = _self as *mut AnyObject;
        let location: NSPoint = msg_send![event as *mut AnyObject, locationInWindow];
        let point: NSPoint = msg_send![
            scroller,
            convertPoint: location,
            fromView: std::ptr::null::<AnyObject>()
        ];
        let next = thumbnail_scroll_offset_for_drag(
            drag.start_offset,
            drag.start_y,
            point.y,
            drag.max_offset,
            drag.thumb_travel,
        );
        set_thumbnail_scroll_offset(next, drag.max_offset);
    }
}

pub(crate) extern "C" fn thumbnail_scroller_mouse_up(
    _self: *mut c_void,
    _cmd: Sel,
    event: *mut c_void,
) {
    *THUMB_SCROLL_DRAG.lock().unwrap() = None;
    unsafe {
        let point: NSPoint = msg_send![event as *mut AnyObject, locationInWindow];
        update_thumbnail_pointer_state(point);
    }
    invalidate_thumbnail_scroller();
}

/// 更新滚动条的轨道、滑块比例和当前位置;无溢出时完全隐藏。
/// Update the scroller track, knob proportion, and position; hide it when there is no overflow.
unsafe fn update_thumbnail_scroller(
    panel_w: f64,
    panel_h: f64,
    overflowed: bool,
    row_count: usize,
    max_rows: usize,
) {
    let Some(scroller) = thumbnail_scroller() else {
        return;
    };
    if !overflowed || row_count <= max_rows || max_rows == 0 {
        let _: () = msg_send![scroller.0, setHidden: true];
        return;
    }
    let footer_h = status_h();
    let frame = NSRect::new(
        NSPoint::new(
            panel_w - H_PADDING - THUMB_SCROLLBAR_W + THUMB_SCROLLBAR_W / 2.0,
            footer_h,
        ),
        NSSize::new(THUMB_SCROLLBAR_W, (panel_h - footer_h).max(1.0)),
    );
    // 拖拽期间保持命中视图的 frame 不变;卡片重建只刷新胶囊绘制。
    // Keep the hit view's frame stable during dragging; card rebuilds only refresh the capsule.
    if THUMB_SCROLL_DRAG.lock().unwrap().is_none() {
        let _: () = msg_send![scroller.0, setFrame: frame];
    }
    let _: () = msg_send![scroller.0, setHidden: false];
    let _: () = msg_send![scroller.0, setNeedsDisplay: true];
}

/// borderless 浮窗重写:允许成为 key 窗口(否则收不到键盘事件)。
/// Override for the borderless overlay window: allow it to become key (otherwise it
/// receives no keyboard events).
pub(crate) extern "C" fn overlay_window_can_become_key(_self: *mut c_void, _cmd: Sel) -> bool {
    true
}

// ========== 窗口激活 / window activation ==========

/// 立即完成可见的快速抬窗,再把 AX 焦点兜底交给后台序列。
/// Complete the visible fast raise immediately, then enqueue the AX focus backstop.
pub(crate) fn activate_and_raise(pid: i32, cgwid: u32, minimized: bool) {
    let activation_started = Instant::now();
    window_server::note_own_focus(pid, cgwid);

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
                // 设计稿 .footer-current:选中的「标题 · 应用名」。
                // The mockup's .footer-current: the selected "title · app".
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
        };
        drop(state_opt);

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
    unsafe {
        if let Some(window) = *OVERLAY_WINDOW.lock().unwrap() {
            let _: () = msg_send![window.0, orderOut: std::ptr::null::<AnyObject>()];
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
    if let Some(state) = TAB_STATE.lock().unwrap().as_mut() {
        state.visible = false;
        state.pending_first_show = false;
        state.pending_first_release = false;
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
    // Closing the settings card intentionally hides our settings window and can make the
    // nonactivating overlay resign key as a side effect. The close transition owns that focus
    // change; do not mistake it for a click outside and hide the overlay while it is reflowing.
    //
    // 关闭设置卡片会主动隐藏设置窗口,可能连带让非激活切换面板失去 key。这个焦点变化属于
    // 关闭流程本身,不能在补位动画期间误判为点击外部并收起切换浮窗。
    if card_close_in_progress() {
        return;
    }
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
/// 从列表移除并调整选中;没有对应 view 时才使用重建兜底。全部关完 → 收起浮窗。
/// Close the window of card `idx` (shared by the close button and Backspace): on a
/// successful AX close, remove it from the list and adjust selection; rebuild only as a
/// fallback when no card view exists. Closing the last one dismisses the overlay.
pub(crate) fn close_window_at(idx: usize) -> bool {
    let (pid, cgwid) = {
        let state_opt = TAB_STATE.lock().unwrap();
        let state = match state_opt.as_ref() {
            Some(s) => s,
            None => return false,
        };
        match state.windows.get(idx) {
            Some(w) => (w.pid, w.window_id),
            None => return false,
        }
    };
    // The settings window belongs to this process. Its custom AX close action would re-enter
    // AppKit from a background close worker and can crash; close it directly on the main thread.
    //
    // 本进程的设置窗口不能走后台关闭线程:它的 AX 关闭动作会回调 AppKit,从后台线程重入
    // UI 可能崩溃。直接在主线程走设置窗口的关闭路径。
    if pid == std::process::id() as i32 {
        crate::settings::close_settings_from_switcher();
        return finish_window_close(idx, pid, cgwid);
    }
    if !crate::window_collector::close_ax_window(pid, cgwid) {
        log_info!(
            "close window FAILED (AX close rejected): pid={} cgwid={}",
            pid,
            cgwid
        );
        return false;
    }
    finish_window_close(idx, pid, cgwid)
}

/// 没有对应卡片时的同步关闭兜底;正常的卡片关闭走 commit_pending_card_close。
/// Synchronous fallback for a close without a corresponding card; normal card closes use
/// commit_pending_card_close.
fn finish_window_close(idx: usize, pid: i32, cgwid: u32) -> bool {
    {
        let state_opt = TAB_STATE.lock().unwrap();
        let Some(state) = state_opt.as_ref() else {
            return false;
        };
        let Some(window) = state.windows.get(idx) else {
            return false;
        };
        if window.pid != pid || window.window_id != cgwid {
            return false;
        }
    }
    log_info!("close window: pid={} cgwid={}", pid, cgwid);
    {
        let mut state_opt = TAB_STATE.lock().unwrap();
        let state = match state_opt.as_mut() {
            Some(s) => s,
            None => return false,
        };
        let was_visible = state.visible;
        let Some(actual_idx) = state
            .windows
            .iter()
            .position(|window| window.pid == pid && window.window_id == cgwid)
        else {
            return false;
        };
        state.windows.remove(actual_idx);
        state.mru.remove(&(pid, cgwid));
        if state.windows.is_empty() {
            // 全部关完:收起浮窗,不留在空态。
            // All closed: dismiss the overlay, don't linger on an empty state.
            if was_visible {
                hide_overlay();
            }
            state.visible = false;
            return true;
        }
        state.selected =
            remove_window_adjust_selection(state.selected, actual_idx, state.windows.len());
        if !was_visible {
            return true;
        }
    }
    // 兜底路径允许完整重建;卡片关闭按钮本身不会走到这里。
    // The fallback may rebuild the overlay; the card close-button path never reaches it.
    reset_thumbnail_visible_range();
    reset_thumbnail_nav_anchor();
    show_overlay();
    refresh_highlight();
    true
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
    clear_thumbnail_scroll_drag();
    set_thumbnail_scroller_hover(false, false);
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

/// 延迟一拍的切换抬升槽 + 调度。
/// 释放/点击/回车的处理函数先 vanish_overlay 并结束当前 runloop turn,让渲染事务提交
/// (vanish 真正上屏、浮窗立即消失),下一个 runloop 周期才执行激活+抬升链。
/// 之前 vanish 与激活+AX 链挤在同一次主线程 turn 里:AX 枚举阻塞主线程期间 vanish 无法
/// 提交,表现为「窗口已经切过去,浮窗冻结在上面顿一下才消失」。
///
/// Deferred-raise slot + scheduling. The release/click/Enter handlers vanish the overlay and
/// END their runloop turn so the render transaction commits (the vanish reaches the screen,
/// the overlay disappears instantly); the activate+raise chain runs on the NEXT runloop cycle.
/// Previously the vanish and the activate+AX chain shared one main-thread turn: while the AX
/// enumeration blocked it, the vanish could not commit -- the overlay lingered, frozen, over
/// the already-switched window.
struct DeferredRaise {
    pid: i32,
    cgwid: u32,
    minimized: bool,
    scheduled_at: Instant,
}

static DEFERRED_RAISE: LazyLock<Mutex<Option<DeferredRaise>>> = LazyLock::new(|| Mutex::new(None));

fn schedule_deferred_raise(pid: i32, cgwid: u32, minimized: bool) {
    *DEFERRED_RAISE.lock().unwrap() = Some(DeferredRaise {
        pid,
        cgwid,
        minimized,
        scheduled_at: Instant::now(),
    });
    unsafe {
        let ctrl = crate::CONTROLLER.lock().unwrap().unwrap().0;
        // afterDelay:0 = 当前 turn 结束后尽快执行——先提交 vanish,再抬升。
        // afterDelay:0 = run as soon as the current turn ends: commit the vanish first, then raise.
        let _: () = msg_send![
            ctrl,
            performSelector: sel!(handleDeferredRaise:),
            withObject: std::ptr::null::<AnyObject>(),
            afterDelay: 0.0f64
        ];
    }
}

pub(crate) extern "C" fn on_deferred_raise(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    let Some(job) = DEFERRED_RAISE.lock().unwrap().take() else {
        return;
    };
    // 释放→本回调的间隔 = 「先提交 vanish」付出的额外延迟,正常应为几毫秒。
    // Release-to-callback gap = the extra delay paid for committing the vanish first;
    // normally a few milliseconds.
    log_debug!(
        "[raise] deferred fire: pid={} cgwid={} +{}ms after release",
        job.pid,
        job.cgwid,
        job.scheduled_at.elapsed().as_millis()
    );
    activate_and_raise(job.pid, job.cgwid, job.minimized);
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
        let document = match card_document() {
            Some(document) => document,
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
        // 选中态采用 HTML 参考中的轻量背景和 1.5px 内描边,不再使用厚重的蓝色边框。
        // Match the HTML reference with a subtle background and 1.5px inset-style border instead of
        // the previous heavy blue outline.
        let sel_bg_color = hex_to_cg_color(colors.card_bg_sel);
        let sel_border_color = hex_to_cg_color(colors.card_border_sel);

        for sv in card_views(document) {
            let layer: *mut AnyObject = msg_send![sv, layer];
            let Some(tag) = get_card_index(sv) else {
                continue;
            };
            // 读卡片标题 label 文本,验证内容与索引对应(排查"显示 Picview 却打开 Ghostty")。
            // Read the card's title-label text to verify content matches the index (investigating
            // "shows Picview but opens Ghostty").
            let is_selected = tag == selected;
            let preview: *mut AnyObject = msg_send![sv, viewWithTag: THUMB_PREVIEW_TAG];
            if !preview.is_null() {
                // HTML 把 translateY(-1px) 施加在 `.item.selected` 根元素，而不是
                // `.preview`；在 AppKit 坐标中以 +1pt 平移卡片根层，标题行与预览区
                // 才会作为一个整体上浮。
                // The HTML applies translateY(-1px) to the `.item.selected` root rather
                // than `.preview`; +1pt in AppKit coordinates lifts the caption row and
                // preview together as one card.
                layer_set_translation_y(layer, thumbnail_card_lift_y(is_selected));
            }
            if is_selected {
                // 设计稿 .item.selected:1.5px 清晰 accent 描边 rgba(75,123,236,.78)。
                // 白底上柔色圈不可见,轮廓线必须用实色 accent 才能显形。
                // The mockup's .item.selected: a crisp 1.5px accent border
                // rgba(75,123,236,.78). A soft ring is invisible on the white
                // surface -- the outline needs the solid accent to show.
                let _: () = msg_send![layer, setBorderWidth: 1.5f64];
                layer_set_border(layer, sel_border_color);
                layer_set_background(layer, sel_bg_color);
                // 投影:0 10px 24px rgba(42,62,102,.12)(设计稿同款)。CSS blur 24 ≈
                // CALayer shadowRadius 12;CALayer shadowOffset y 正值向上,向下投影取 -10。
                // Drop shadow: 0 10px 24px rgba(42,62,102,.12), straight from the mockup.
                // CSS blur 24 ≈ CALayer shadowRadius 12; CALayer's shadowOffset y is up-positive,
                // so a downward shadow takes -10.
                let shadow_color = hex_to_cg_color(0x2A3E66FF);
                // CGColorRef 不能进 objc2 的 msg_send!('@' vs '^{CGColor=}' 运行时
                // 拒绝,实测召唤即崩),照 layer_set_background 惯例走裸 objc_msgSend。
                // A CGColorRef cannot go through objc2's msg_send! ('@' vs
                // '^{CGColor=}' rejected at runtime -- crashed the summon, verified);
                // use raw objc_msgSend per the layer_set_background convention.
                layer_set_shadow_color(layer, shadow_color);
                let _: () = msg_send![layer, setShadowOpacity: 0.12f32];
                let _: () = msg_send![layer, setShadowRadius: 12.0f64];
                let _: () = msg_send![layer, setShadowOffset: NSSize::new(0.0, -10.0)];
            } else {
                let _: () = msg_send![layer, setBorderWidth: 0.0f64];
                layer_set_border(layer, std::ptr::null_mut());
                layer_set_background(layer, std::ptr::null_mut());
                let _: () = msg_send![layer, setShadowOpacity: 0.0f32];
            }

            // CSS 的第一层 box-shadow 是卡片外侧 2px、零模糊的 accent-soft 圈,
            // 不能与下面的深色模糊投影共用 CALayer.shadow。独立 ring 视图保留
            // RGB 并把 alpha 提升到适合 Liquid Glass 的 38%,再叠一层零偏移蓝色柔光；
            // 只在缩略图模式选中时显示。
            // The first CSS box-shadow is a zero-blur 2px accent-soft ring outside the
            // card; it cannot share CALayer.shadow with the dark blurred drop shadow.
            // A dedicated ring view preserves the RGB, raises alpha to 38% for Liquid
            // Glass, adds a zero-offset blue glow, and appears only on the selected card.
            let ring: *mut AnyObject = msg_send![sv, viewWithTag: THUMB_SELECTION_RING_TAG];
            if !ring.is_null() {
                let ring_layer: *mut AnyObject = msg_send![ring, layer];
                layer_set_border(
                    ring_layer,
                    hex_to_cg_color(color_with_alpha(
                        colors.card_border_sel,
                        SELECTION_RING_ALPHA,
                    )),
                );
                layer_set_shadow_color(
                    ring_layer,
                    hex_to_cg_color(color_with_alpha(colors.card_border_sel, 0xFF)),
                );
                let glow_opacity = if is_selected {
                    SELECTION_GLOW_OPACITY
                } else {
                    0.0
                };
                let _: () = msg_send![ring_layer, setShadowOpacity: glow_opacity];
                let _: () = msg_send![ring_layer, setShadowRadius: SELECTION_GLOW_RADIUS];
                let _: () = msg_send![ring_layer, setShadowOffset: NSSize::new(0.0, 0.0)];
                let _: () = msg_send![ring, setHidden: !is_selected];
            }

            // 图标在选中态向上轻移 2pt;每次都从基准 y 重算,避免反复
            // 切换时累计位移。
            // Nudge the icon up by 2pt when selected; recompute from the baseline on every
            // refresh so repeated selection changes never accumulate the offset.
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
                let icon_y = base_y
                    + if is_selected {
                        SELECTED_CONTENT_NUDGE
                    } else {
                        0.0
                    };
                let _: () = msg_send![
                    icon,
                    setFrameOrigin: NSPoint::new(icon_frame.origin.x, icon_y)
                ];
            }

            // 缩略图模式:预览区自身不再单独位移，整卡根层已携带标题与预览共同上浮；
            // 此处只切换设计稿中的选中预览描边。
            // Thumbnail mode: the preview no longer moves independently because the card
            // root now lifts the caption and preview together; only its selected border
            // changes here.
            if !preview.is_null() {
                // 设计稿选中时把预览区 1px 描边切为 accent 34%;未选中恢复中性描边。
                // The mockup switches the preview's 1px border to accent at 34% when
                // selected; restore the neutral border otherwise.
                let preview_layer: *mut AnyObject = msg_send![preview, layer];
                let preview_border = if is_selected {
                    color_with_alpha(colors.card_border_sel, SELECTED_PREVIEW_BORDER_ALPHA)
                } else {
                    colors.preview_border
                };
                layer_set_border(preview_layer, hex_to_cg_color(preview_border));
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
    if indices.is_empty() || card_close_in_progress() {
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
        let thumbnail_capture_allowed = crate::thumbnail::capture_allowed();
        let document = match card_document() {
            Some(document) => document,
            None => return,
        };

        // Collect affected card views + their frames first; don't mutate the
        // subview array while iterating it.
        let mut replacements: Vec<(*mut AnyObject, NSRect, usize)> = Vec::new();
        for sv in card_views(document) {
            let Some(idx) = get_card_index(sv) else {
                continue;
            };
            if to_rebuild.contains_key(&idx) {
                let frame: NSRect = msg_send![sv, frame];
                replacements.push((sv, frame, idx));
            }
        }

        for (old_view, frame, idx) in replacements {
            if let Some(w) = to_rebuild.get(&idx) {
                remove_card_index(old_view);
                // 沿用旧卡 frame 的宽高(原位替换:流式布局缩卡后高宽都是逐卡值)。
                // Reuse the old card frame's width AND height (in-place replacement:
                // after flow shrink both are per-card values).
                let new_card = create_card_view(
                    w,
                    idx,
                    frame.size.width,
                    frame.size.height,
                    thumbnail_capture_allowed,
                );
                let _: () = msg_send![new_card, setFrame: frame];
                let _: () = msg_send![old_view, removeFromSuperview];
                let _: () = msg_send![document, addSubview: new_card];
                release_obj(new_card); // container owns the card; drop create_card_view's alloc +1
            }
        }

        // New card views have no selection border; re-apply the highlight.
        refresh_highlight();
    }
}

/// 缩略图捕获完成后的轻量更新：只刷新受影响卡片的预览容器，不重建标题、按钮、
/// tracking area 或选中图层。一次 ready 批次只扫描一次容器子视图。
/// Lightweight post-capture update: refresh only affected cards' preview containers,
/// without rebuilding captions, buttons, tracking areas, or selection layers. One
/// ready batch scans the container's subviews once.
pub(crate) fn refresh_thumbnail_previews(keys: &[(i32, u32)]) {
    if keys.is_empty() || card_close_in_progress() {
        return;
    }
    let affected: HashSet<WindowKey> = keys.iter().copied().collect();
    let windows: HashMap<WindowKey, WindowInfo> = {
        let state = TAB_STATE.lock().unwrap();
        let Some(state) = state.as_ref() else {
            return;
        };
        if !state.visible {
            return;
        }
        state
            .windows
            .iter()
            .filter_map(|window| {
                let key = (window.pid, window.window_id);
                affected.contains(&key).then(|| (key, window.clone()))
            })
            .collect()
    };
    if windows.is_empty() {
        return;
    }

    unsafe {
        let started = Instant::now();
        let capture_allowed =
            crate::theme::thumbnails_enabled() && crate::thumbnail::capture_allowed();
        let colors = current_colors();
        let document = match card_document() {
            Some(document) => document,
            None => return,
        };
        let mut updated = 0usize;
        for card in card_views(document) {
            let Some(key) = card_key(card) else {
                continue;
            };
            let Some(window) = windows.get(&key) else {
                continue;
            };
            let preview: *mut AnyObject = msg_send![card, viewWithTag: THUMB_PREVIEW_TAG];
            if preview.is_null() {
                continue;
            }
            populate_thumbnail_preview(preview, window, &colors, capture_allowed);
            updated += 1;
        }
        log_debug!(
            "[thumb] preview refresh: requested={} updated={} ms={}",
            windows.len(),
            updated,
            started.elapsed().as_millis()
        );
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
    let radius = CONFIG.read().unwrap().appearance.corner_radius;
    let style_name = config::effective_glass_style();
    let tint_hex = config::parse_hex8(&config::effective_glass_tint());
    let _: () = msg_send![glass, setCornerRadius: radius];
    // 同步 layer 的硬裁剪:cornerRadius 只圆着色不圆模糊,需 masksToBounds 把模糊也裁进圆角
    // (见 create_overlay_window 的 (6.5) 注释)。
    // Mirror the layer hard-clip: cornerRadius rounds the tint but not the blur, so masksToBounds
    // is needed to clip the blur into the rounded shape (see (6.5) in create_overlay_window).
    let glass_layer: *mut AnyObject = msg_send![glass, layer];
    if !glass_layer.is_null() {
        let _: () = msg_send![glass_layer, setCornerRadius: radius];
        let _: () = msg_send![glass_layer, setMasksToBounds: true];
    }
    let style: i64 = match style_name.as_str() {
        "clear" => 1,
        _ => 0, // regular
    };
    let _: () = msg_send![glass, setStyle: style];
    let tint = hex_to_ns_color(tint_hex);
    let _: () = msg_send![glass, setTintColor: tint];
}

pub(crate) fn apply_theme() {
    // Rebuild visible cards as well as updating the window material. Card labels and preview
    // layers use concrete colors chosen at creation time, so changing only NSAppearance leaves
    // existing cards with the previous palette until the overlay is summoned again.
    // 主题变化除了更新窗口材质,还要重建当前可见卡片。卡片文字和预览图层在创建时写入具体颜色,
    // 只设置 NSAppearance 会让已存在的卡片继续使用旧调色板,直到下次重新召唤。
    let visible_indices = TAB_STATE
        .lock()
        .unwrap()
        .as_ref()
        .filter(|state| state.visible)
        .map(|state| (0..state.windows.len()).collect::<Vec<_>>())
        .unwrap_or_default();

    unsafe {
        // 主题来源于 config 的解析结果;显式主题由设置页保存,auto 主题由系统外观通知触发刷新。
        // The theme comes from the resolved config; explicit themes are saved by Settings, while
        // auto themes are refreshed from the system appearance notification.
        let is_dark = crate::theme::resolved_is_dark();

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
    }

    if visible_indices.is_empty() {
        refresh_highlight();
    } else {
        rebuild_cards(&visible_indices);
    }
    update_status_label();
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

/// CGImageRef -> NSImage(指定 pt 尺寸)。CG/CF 类型 objc2 的 msg_send! 编不了——
/// 裸 c_void 会编出 '^v' 而方法期望 '^{CGImage=}',运行时直接 panic(实测把持有
/// TAB_STATE 锁的 show_overlay 炸掉、锁中毒后每次 Cmd+Tab 连环崩);照
/// layer_set_background 惯例走裸 objc_msgSend。
/// CGImageRef -> NSImage at a given point size. CG/CF types cannot go through
/// objc2's msg_send! -- a bare c_void encodes as '^v' while the method expects
/// '^{CGImage=}', and the runtime panics (verified: it blew up show_overlay while
/// it held the TAB_STATE lock, and the poisoned lock then crashed every following
/// Cmd+Tab). Follows the layer_set_background raw objc_msgSend convention.
pub(crate) unsafe fn nsimage_from_cgimage(cg: *const c_void, size: NSSize) -> *mut AnyObject {
    let sel = sel!(initWithCGImage:size:);
    type F = unsafe extern "C" fn(*mut AnyObject, Sel, *const c_void, NSSize) -> *mut AnyObject;
    let f: F = std::mem::transmute(objc_msgSend as *const ());
    let img: *mut AnyObject = msg_send![class!(NSImage), alloc];
    if img.is_null() {
        return std::ptr::null_mut();
    }
    f(img, sel, cg, size)
}

/// 左对齐标签(设计稿 .caption-title):固定宽度 + 尾部截断,不居中。
/// A left-aligned label (the mockup's .caption-title): fixed width + tail
/// truncation, no centering.
unsafe fn make_left_label(
    text: &str,
    font: *mut AnyObject,
    color: *mut AnyObject,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> *mut AnyObject {
    let ns_str = make_nsstring(text);
    let init_frame = NSRect::new(NSPoint::new(x, y), NSSize::new(width, height));
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let label: *mut AnyObject = msg_send![label, initWithFrame: init_frame];
    let _: () = msg_send![label, setStringValue: ns_str];
    CFRelease(ns_str as *const c_void);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let _: () = msg_send![label, setSelectable: false];
    let _: () = msg_send![label, setUsesSingleLineMode: true];
    let _: () = msg_send![label, setAlignment: 0isize]; // NSTextAlignmentLeft
    let _: () = msg_send![label, setFont: font];
    let _: () = msg_send![label, setTextColor: color];
    // 尾部截断(NSLineBreakByTruncatingTail = 4):超宽自动省略号。
    // Tail truncation (NSLineBreakByTruncatingTail = 4): ellipsis on overflow.
    let _: () = msg_send![label, setLineBreakMode: 4isize];
    let ascender: f64 = msg_send![font, ascender];
    let descender: f64 = msg_send![font, descender];
    let line_h = (ascender - descender + 1.0).max(11.0).min(height.max(1.0));
    let centered_y = y + (height - line_h) / 2.0;
    let _: () = msg_send![label, setFrame: NSRect::new(
        NSPoint::new(x, centered_y),
        NSSize::new(width, line_h),
    )];
    label
}

/// 预览区无缩略图时的兜底内容:居中的大应用图标(或首字母块),最小化烘焙灰度。
/// 在 container 本地坐标系里布局(container 尺寸 pw×ph)。
/// Fallback content for a preview without a thumbnail: a centered large app icon
/// (or first-letter block), grayscale-baked when minimized. Laid out in the
/// container's local coordinates (container is pw×ph).
unsafe fn add_preview_icon_fallback(
    container: *mut AnyObject,
    pw: f64,
    ph: f64,
    w: &WindowInfo,
    colors: &Colors,
) {
    let big = (ph - 16.0).clamp(36.0, 72.0);
    let cx = (pw - big) / 2.0;
    let cy = (ph - big) / 2.0;
    let frame = NSRect::new(NSPoint::new(cx, cy), NSSize::new(big, big));
    let mut loaded: Option<*mut AnyObject> = None;
    if let Some(ref icon_path) = w.icon_path {
        let ns_path = make_nsstring(icon_path);
        let img: *mut AnyObject = msg_send![class!(NSImage), alloc];
        let img: *mut AnyObject = msg_send![img, initWithContentsOfFile: ns_path];
        CFRelease(ns_path as *const c_void);
        if !img.is_null() {
            loaded = Some(img);
        }
    }
    if let Some(img) = loaded {
        let shown: *mut AnyObject = if w.minimized {
            let g = grayed_image(img, NSSize::new(big, big));
            release_obj(img);
            g
        } else {
            img
        };
        let iv: *mut AnyObject = msg_send![class!(NSImageView), alloc];
        let iv: *mut AnyObject = msg_send![iv, initWithFrame: frame];
        let _: () = msg_send![iv, setImage: shown];
        release_obj(shown);
        let _: () = msg_send![iv, setImageScaling: 3u64];
        let _: () = msg_send![container, addSubview: iv];
        release_obj(iv);
    } else {
        // 首字母块(圆角 + icon_inner_bg + 首字母),与旧版字母占位同款样式。
        // First-letter block (rounded + icon_inner_bg + initial), same style as the
        // legacy letter placeholder.
        let lv: *mut AnyObject = msg_send![class!(NSImageView), alloc];
        let lv: *mut AnyObject = msg_send![lv, initWithFrame: frame];
        let _: () = msg_send![lv, setWantsLayer: true];
        let ll: *mut AnyObject = msg_send![lv, layer];
        let _: () = msg_send![ll, setCornerRadius: 12.0f64];
        let _: () = msg_send![ll, setMasksToBounds: true];
        layer_set_background(ll, hex_to_cg_color(colors.icon_inner_bg));
        let init_char = w.app_name.chars().next().unwrap_or('?').to_string();
        let font: *mut AnyObject =
            msg_send![class!(NSFont), systemFontOfSize: 24.0f64, weight: 0.4f64];
        let label = make_centered_label(
            &init_char,
            font,
            hex_to_ns_color(colors.icon_text),
            0.0,
            big,
            big,
        );
        let _: () = msg_send![lv, addSubview: label];
        release_obj(label);
        if w.minimized {
            let dim: *mut AnyObject = msg_send![class!(NSView), alloc];
            let dim: *mut AnyObject = msg_send![dim, initWithFrame: frame];
            let _: () = msg_send![dim, setWantsLayer: true];
            let dl: *mut AnyObject = msg_send![dim, layer];
            let _: () = msg_send![dl, setCornerRadius: 12.0f64];
            let _: () = msg_send![dl, setMasksToBounds: true];
            layer_set_background(dl, hex_to_cg_color(0x808080AA));
            let _: () = msg_send![lv, addSubview: dim];
            release_obj(dim);
        }
        let _: () = msg_send![container, addSubview: lv];
        release_obj(lv);
    }
}

/// 只替换预览容器内部的图像/图标内容，保留卡片标题、按钮、tracking area、图层和
/// 选中态。缩略图异步到达时不再销毁重建整张卡片。
/// Replace only the image/icon content inside a preview container, preserving the
/// card caption, buttons, tracking area, layers, and selection state. Asynchronous
/// thumbnail delivery no longer destroys and rebuilds the whole card.
unsafe fn populate_thumbnail_preview(
    container: *mut AnyObject,
    w: &WindowInfo,
    colors: &Colors,
    capture_allowed: bool,
) {
    let old: *mut AnyObject = msg_send![container, subviews];
    let mut old_count: usize = msg_send![old, count];
    while old_count > 0 {
        old_count -= 1;
        let child: *mut AnyObject = msg_send![old, objectAtIndex: old_count];
        let _: () = msg_send![child, removeFromSuperview];
    }

    let bounds: NSRect = msg_send![container, bounds];
    let pw = bounds.size.width;
    let ph = bounds.size.height;
    let thumb = if capture_allowed && w.bounds.2 > 0.0 && w.bounds.3 > 0.0 {
        crate::thumbnail::lookup_retained(w.pid, w.window_id)
    } else {
        None
    };
    let Some((cg, w_px, h_px)) = thumb else {
        add_preview_icon_fallback(container, pw, ph, w, colors);
        return;
    };

    let (cw, ch) = crate::thumbnail::fit_size(w_px as f64, h_px as f64, pw, ph);
    let nsimg = nsimage_from_cgimage(cg, NSSize::new(cw, ch));
    CFRelease(cg); // lookup 给的 +1 已被 NSImage 持有 / NSImage retains its own copy
    if nsimg.is_null() {
        add_preview_icon_fallback(container, pw, ph, w, colors);
        return;
    }
    let shown: *mut AnyObject = if w.minimized {
        let grayed = grayed_image(nsimg, NSSize::new(cw, ch));
        release_obj(nsimg);
        grayed
    } else {
        nsimg
    };
    let iv: *mut AnyObject = msg_send![class!(NSImageView), alloc];
    let iv: *mut AnyObject = msg_send![iv, initWithFrame: NSRect::new(
        NSPoint::new((pw - cw) / 2.0, (ph - ch) / 2.0),
        NSSize::new(cw, ch)
    )];
    let _: () = msg_send![iv, setImage: shown];
    release_obj(shown);
    let _: () = msg_send![iv, setImageScaling: 2u64]; // exact size, no additional scaling
    let _: () = msg_send![container, addSubview: iv];
    release_obj(iv);
}

pub(crate) fn create_card_view(
    w: &WindowInfo,
    index: usize,
    card_width: f64,
    card_h: f64,
    thumbnail_capture_allowed: bool,
) -> *mut AnyObject {
    unsafe {
        let card_cls = CARD_CLASS.lock().unwrap().unwrap();
        let card_cls_ptr = card_cls.0 as *mut AnyObject;

        // 缩略图开 = 设计稿新布局(标题行 + 16:10 预览区);关 = 旧版布局
        // (居中大图标 + 两行文字)。高度由调用方传入(流式布局缩卡后各卡
        // 实际高度 < 基准值,内部几何必须按实际高度排布,否则标题行会被
        // masksToBounds 裁掉——实测)。
        // Thumbnails on = the mockup layout (caption + 16:10 preview); off = the
        // legacy layout (centered icon + two text lines). The height comes from the
        // caller: after the flow layout's shrink step each card's actual height is
        // smaller than the base, and the internal geometry MUST be laid out from
        // that actual height or masksToBounds clips the caption away (verified).
        let use_new = crate::theme::thumbnails_enabled();
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(card_width, card_h));
        let view: *mut AnyObject = msg_send![card_cls_ptr, alloc];
        let view: *mut AnyObject = msg_send![view, initWithFrame: frame];

        // Enable layer for selection border
        let _: () = msg_send![view, setWantsLayer: true];
        let layer: *mut AnyObject = msg_send![view, layer];
        // 新版圆角 16(设计稿 .item),旧版保持 14。
        // 16px radius for the new layout (.item), legacy keeps 14.
        let _: () = msg_send![layer, setCornerRadius: if use_new { 16.0f64 } else { 14.0f64 }];
        // masksToBounds 必须为 false:选中态的投影画在卡片边界之外,裁剪会把它
        // 整个吃掉。子元素(预览区/标题行/关闭按钮)均相对卡片边缘内缩,几何上
        // 没有内容超出圆角形状,关闭裁剪是安全的(cornerRadius 仍会圆出背景与描边)。
        // masksToBounds MUST be false: the selected-state shadow draws OUTSIDE the
        // card bounds and clipping would swallow it entirely. Children (preview /
        // caption / close button) are all inset from the card edges and stay inside
        // the rounded shape, so disabling the clip is safe (cornerRadius still rounds
        // the background and border).
        let _: () = msg_send![layer, setMasksToBounds: false];

        // Store card index in side map (avoids msg_send! issues on dynamic classes)
        set_card_index(view, index);
        set_card_key(view, (w.pid, w.window_id));
        set_card_signature(
            view,
            card_signature(w, frame, use_new, thumbnail_capture_allowed),
        );

        let colors = current_colors();

        if use_new {
            // 设计稿 box-shadow 的第一层是向外扩 2pt 的零模糊柔蓝圈。用独立透明
            // NSImageView 承载 2pt border:frame 每边扩 2pt,边框向内绘制后恰好覆盖
            // 卡片外侧 [-2,0] 区间。它先加入卡片,位于标题/预览内容下方；卡片关闭
            // masksToBounds 后外圈才能完整显示。refresh_highlight 控制显隐与主题色。
            // The mockup's first box-shadow is a zero-blur soft-blue ring spread 2pt
            // outward. A transparent NSImageView carries a 2pt border: expanding its
            // frame by 2pt per side makes the inward-drawn border cover exactly the
            // card's outer [-2,0] band. It is added before caption/preview content;
            // masksToBounds=false keeps the outer ring visible. refresh_highlight owns
            // visibility and theme color.
            let ring_frame = NSRect::new(
                NSPoint::new(-2.0, -2.0),
                NSSize::new(card_width + 4.0, card_h + 4.0),
            );
            let ring: *mut AnyObject = msg_send![class!(NSImageView), alloc];
            let ring: *mut AnyObject = msg_send![ring, initWithFrame: ring_frame];
            let _: () = msg_send![ring, setTag: THUMB_SELECTION_RING_TAG];
            let _: () = msg_send![ring, setWantsLayer: true];
            let ring_layer: *mut AnyObject = msg_send![ring, layer];
            let _: () = msg_send![ring_layer, setCornerRadius: 18.0f64];
            let _: () = msg_send![ring_layer, setMasksToBounds: false];
            let _: () = msg_send![ring_layer, setBorderWidth: 2.0f64];
            layer_set_border(
                ring_layer,
                hex_to_cg_color(color_with_alpha(
                    colors.card_border_sel,
                    SELECTION_RING_ALPHA,
                )),
            );
            let _: () = msg_send![ring, setHidden: true];
            let _: () = msg_send![view, addSubview: ring];
            release_obj(ring);

            let caption_h = thumb_caption_h();
            let preview_h = thumb_preview_h(card_h);
            let caption_y = card_h - THUMB_PAD - caption_h;

            // --- 标题行:迷你图标 22pt(圆角 5,加载失败用首字母块) ---
            // --- Caption row: 22pt mini icon (radius 5, letter block on failure) ---
            let mini_sz = (22.0 * text_scale())
                .clamp(16.0, 30.0)
                .min((caption_h - 2.0).max(1.0));
            let mini_frame = NSRect::new(
                NSPoint::new(THUMB_PAD, caption_y + (caption_h - mini_sz) / 2.0),
                NSSize::new(mini_sz, mini_sz),
            );
            let mut mini_img: Option<*mut AnyObject> = None;
            if let Some(ref icon_path) = w.icon_path {
                let ns_path = make_nsstring(icon_path);
                let img: *mut AnyObject = msg_send![class!(NSImage), alloc];
                let img: *mut AnyObject = msg_send![img, initWithContentsOfFile: ns_path];
                CFRelease(ns_path as *const c_void);
                if !img.is_null() {
                    mini_img = Some(img);
                }
            }
            let mini: *mut AnyObject = msg_send![class!(NSImageView), alloc];
            let mini: *mut AnyObject = msg_send![mini, initWithFrame: mini_frame];
            let _: () = msg_send![mini, setWantsLayer: true];
            let ml: *mut AnyObject = msg_send![mini, layer];
            let _: () = msg_send![ml, setCornerRadius: 5.0f64];
            let _: () = msg_send![ml, setMasksToBounds: true];
            match mini_img {
                Some(img) => {
                    let _: () = msg_send![mini, setImage: img];
                    release_obj(img);
                    let _: () = msg_send![mini, setImageScaling: 3u64];
                }
                None => {
                    layer_set_background(ml, hex_to_cg_color(colors.icon_inner_bg));
                    let init_char = w.app_name.chars().next().unwrap_or('?').to_string();
                    let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: (10.0 * text_scale()).clamp(8.0, 16.0), weight: 0.5f64];
                    let label = make_centered_label(
                        &init_char,
                        font,
                        hex_to_ns_color(colors.icon_text),
                        0.0,
                        mini_sz,
                        mini_sz,
                    );
                    let _: () = msg_send![mini, addSubview: label];
                    release_obj(label);
                }
            }
            let _: () = msg_send![view, addSubview: mini];
            release_obj(mini);

            // --- 标题(左对齐,尾部截断;应用名沉到底部状态栏) ---
            // --- Title (left-aligned, tail-truncated; the app name sinks into the
            // status footer) ---
            let title_x = THUMB_PAD + mini_sz + 8.0;
            let close_sz = 24.0;
            let title_w = (card_width - title_x - close_sz - THUMB_PAD).max(20.0);
            let title_size = card_title_font_size();
            let title_font: *mut AnyObject = {
                let cfg = CONFIG.read().unwrap();
                msg_send![class!(NSFont), systemFontOfSize: title_size, weight: cfg.fonts.title_weight]
            };
            let title_label = make_left_label(
                display_title(&w.window_title, &w.app_name),
                title_font,
                hex_to_ns_color(colors.win_title),
                title_x,
                caption_y + 2.0,
                title_w,
                (caption_h - 4.0).max(1.0),
            );
            let _: () = msg_send![view, addSubview: title_label];
            release_obj(title_label);

            // --- 预览区(16:10,圆角 4,1px 描边) ---
            // --- Preview area (16:10, radius 4, 1px border) ---
            let preview_frame = NSRect::new(
                NSPoint::new(THUMB_PAD, THUMB_PAD),
                NSSize::new(card_width - THUMB_PAD * 2.0, preview_h),
            );
            // 容器用 NSImageView 承载:refresh_highlight 的选中态 nudge 依赖
            // viewWithTag 定位,而 setTag: 只有 NSControl 系(含 NSImageView)提供,
            // 裸 NSView 的 tag 属性只读,objc2 调试期校验会直接 panic(与字母头像
            // 同一个坑)。无 image 的 NSImageView 什么都不画,子视图照常渲染。
            // The preview container is an NSImageView: refresh_highlight's
            // selected-state nudge locates it via viewWithTag, and setTag: only
            // exists on NSControl-derived classes -- a bare NSView's tag property is
            // readonly and objc2's debug check would panic (same pitfall as the
            // letter avatar). An image-less NSImageView draws nothing; subviews
            // render normally.
            let container: *mut AnyObject = msg_send![class!(NSImageView), alloc];
            let container: *mut AnyObject = msg_send![container, initWithFrame: preview_frame];
            let _: () = msg_send![container, setTag: THUMB_PREVIEW_TAG];
            let _: () = msg_send![container, setWantsLayer: true];
            let cl: *mut AnyObject = msg_send![container, layer];
            // 预览区圆角 4pt:预览是缩小的窗口(约 1/8 缩放),macOS 窗口真实圆角
            // (Tahoe 16pt / Sequoia 10pt)在此缩放下等效 ~2pt,加 1px 描边的视觉
            // 补偿取 4pt——10pt 会显得比窗口本身圆得多(实测反馈)。
            // Preview corner radius of 4pt: the preview is a shrunken window (~1/8
            // scale), where a real macOS window corner (Tahoe 16pt / Sequoia 10pt)
            // equates to ~2pt; 4pt adds compensation for the 1px border. 10pt read
            // far rounder than the window itself (user-reported).
            let _: () = msg_send![cl, setCornerRadius: 4.0f64];
            let _: () = msg_send![cl, setMasksToBounds: true];
            let _: () = msg_send![cl, setBorderWidth: 1.0f64];
            layer_set_border(cl, hex_to_cg_color(colors.preview_border));
            layer_set_background(cl, hex_to_cg_color(colors.icon_inner_bg));

            populate_thumbnail_preview(container, w, &colors, thumbnail_capture_allowed);

            let _: () = msg_send![view, addSubview: container];
            release_obj(container); // view owns the container; drop our alloc +1
        } else {
            // ===== 旧版布局(缩略图关闭):居中大图标 + 两行文字 =====
            // ===== Legacy layout (thumbnails off): centered icon + two text lines =====
            let icon_x = (card_width - icon_px()) / 2.0; // 16.0
            let icon_bottom = card_h - 8.0 - icon_px(); // 64.0

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

                // 字母头像容器用 NSImageView 承载:下方 viewWithTag 依赖 tag 定位,
                // 而 setTag: 只有 NSControl 系(含 NSImageView)提供,裸 NSView 的
                // tag 属性只读,objc2 调试期校验会直接 panic。
                // The letter-avatar container uses NSImageView: the refresh path locates it via
                // viewWithTag, and setTag: only exists on NSControl-derived classes -- a bare
                // NSView's tag property is readonly and objc2's debug check would panic.
                let letter_view: *mut AnyObject = msg_send![class!(NSImageView), alloc];
                let letter_view: *mut AnyObject =
                    msg_send![letter_view, initWithFrame: letter_frame];
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
            // 主行 = 窗口标题,次行 = 应用名:标题 12px medium 深色(win_title),
            // 应用名 10px regular 浅色(app_name)。
            // Primary line = window title, secondary = app name: title 12px medium
            // (win_title), app name 10px regular (app_name).
            let text_scale = text_scale();
            let primary_line_h = 18.0 * text_scale;
            let secondary_line_h = 16.0 * text_scale;
            let primary_bottom = icon_bottom - text_gap - primary_line_h;
            // 次行:16px 高,贴卡片底部。
            // Secondary line: 16px tall at the bottom.
            let secondary_bottom = primary_bottom - 2.0 - secondary_line_h;

            // --- 主行:窗口标题(12px medium 深色)---
            // --- Primary line: window title (12px medium, dark).
            let primary_font_size = card_title_font_size();
            let primary_font: *mut AnyObject = {
                let cfg = CONFIG.read().unwrap();
                msg_send![class!(NSFont), systemFontOfSize: primary_font_size, weight: cfg.fonts.title_weight]
            };
            let primary_color = hex_to_ns_color(colors.win_title);
            let title_label = make_centered_label(
                display_title(&w.window_title, &w.app_name),
                primary_font,
                primary_color,
                primary_bottom,
                card_width,
                primary_line_h,
            );
            let _: () = msg_send![view, addSubview: title_label];
            release_obj(title_label); // view owns the label; drop our alloc +1

            // --- 次行:应用名(10px regular 浅色)---
            // --- Secondary line: app name (10px regular, light).
            let secondary_font_size = card_app_name_font_size();
            let secondary_font: *mut AnyObject = {
                let cfg = CONFIG.read().unwrap();
                msg_send![class!(NSFont), systemFontOfSize: secondary_font_size, weight: cfg.fonts.app_name_weight]
            };
            let secondary_color = hex_to_ns_color(colors.app_name);
            let name_label = make_centered_label(
                &w.app_name,
                secondary_font,
                secondary_color,
                secondary_bottom,
                card_width,
                secondary_line_h,
            );
            let _: () = msg_send![view, addSubview: name_label];
            release_obj(name_label); // view owns the label; drop our alloc +1
        }

        // --- Tracking area for hover ---
        // NSTrackingMouseEnteredAndExited | NSTrackingActiveAlways
        // activeAlways:召唤时 app 未激活(nonactivating 面板),必须用 activeAlways 才能收
        // mouseEntered 悬停事件。activeInActiveApp(0x40) 在 app 非激活时不投递。
        let opts: u64 = 0x01 | 0x80;
        let ta: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
        let bounds = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(card_width, card_h));
        let ta: *mut AnyObject = msg_send![ta, initWithRect: bounds, options: opts, owner: view, userInfo: std::ptr::null::<AnyObject>()];
        let _: () = msg_send![view, addTrackingArea: ta];
        release_obj(ta); // view owns the tracking area; drop our alloc +1

        // --- 关闭按钮:新版在标题行右侧(圆形 24,设计稿 .close);旧版保持右上角 20 ---
        // --- 关闭按钮:两种模式统一样式(20×20、圆角 6、字号 12,与旧版图标模式
        // 完全一致——含悬停变红的观感);仅位置随布局:缩略图模式在标题行右侧
        // 垂直居中,旧版在卡片右上角。 ---
        // --- Close button: unified style across both layouts (20x20, radius 6,
        // font 12 -- identical to the legacy icon-mode button, hover-red included);
        // only the position follows the layout: caption-row right edge (centered)
        // in thumbnail mode, top-right corner in legacy. ---
        let (btn_frame, btn_radius, btn_font_sz) = if use_new {
            let caption_h = thumb_caption_h();
            let caption_y = card_h - THUMB_PAD - caption_h;
            (
                NSRect::new(
                    NSPoint::new(
                        card_width - THUMB_PAD - 20.0,
                        caption_y + (caption_h - 20.0) / 2.0,
                    ),
                    NSSize::new(20.0, 20.0),
                ),
                6.0f64,
                12.0f64,
            )
        } else {
            (
                NSRect::new(
                    NSPoint::new(card_width - 27.0, card_h - 27.0),
                    NSSize::new(20.0, 20.0),
                ),
                6.0f64,
                12.0f64,
            )
        };
        let btn: *mut AnyObject = msg_send![close_button_class(), alloc];
        let btn: *mut AnyObject = msg_send![btn, initWithFrame: btn_frame];
        let _: () = msg_send![btn, setBordered: false];
        let title_ns = make_nsstring("×");
        let _: () = msg_send![btn, setTitle: title_ns];
        CFRelease(title_ns as *const c_void);
        let close_font: *mut AnyObject =
            msg_send![class!(NSFont), systemFontOfSize: btn_font_sz, weight: 0.0f64];
        let _: () = msg_send![btn, setFont: close_font];
        let _: () = msg_send![btn, setAlignment: 1isize]; // NSTextAlignmentCenter on arm64
                                                          // HTML .close 的默认状态是透明背景 + 半透明黑色文字。
                                                          // The HTML .close base state uses a transparent background and translucent black text.
        let _: () = msg_send![btn, setWantsLayer: true];
        let bl: *mut AnyObject = msg_send![btn, layer];
        let _: () = msg_send![bl, setCornerRadius: btn_radius];
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
/// 返回 (目标屏幕 frame, visibleFrame, backingScaleFactor)。屏幕对象不跨召唤缓存，
/// 外接/拔出显示器或更改缩放模式后，下次召唤会读取新的实时倍率。
/// Returns (target screen frame, visibleFrame, backingScaleFactor). The NSScreen object is
/// never cached across summons, so display hot-plug/unplug and scaling-mode changes use the
/// new live scale on the next summon.
fn overlay_target_screen(windows: &[WindowInfo]) -> (NSRect, NSRect, f64) {
    unsafe {
        let metrics = |screen: *mut AnyObject| {
            let frame: NSRect = msg_send![screen, frame];
            let visible: NSRect = msg_send![screen, visibleFrame];
            let scale: f64 = msg_send![screen, backingScaleFactor];
            (frame, visible, if scale > 0.0 { scale } else { 1.0 })
        };
        let pos = CONFIG.read().unwrap().windows.overlay_position.clone();
        // 主显示器 = screens[0](系统保证首屏带菜单栏);screens 为空时回退 mainScreen。
        // Primary display = screens[0] (first entry hosts the menu bar); fall back to
        // mainScreen if the screens array is somehow empty.
        let main_screen_obj: *mut AnyObject = {
            let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
            let count: usize = msg_send![screens, count];
            if count > 0 {
                // objectAtIndex: 的参数编码是 'q'(signed long),必须传 isize/i64;
                // 传整数字面量会被推断为 i32('i'),objc2 运行时校验会 panic。
                // objectAtIndex: expects a 'q' (signed long) argument; pass isize/i64 or
                // objc2's runtime encoding check panics on an i32 literal.
                msg_send![screens, objectAtIndex: 0isize]
            } else {
                msg_send![class!(NSScreen), mainScreen]
            }
        };
        if pos != "active_window" {
            return metrics(main_screen_obj);
        }
        // 激活窗口:collect_windows 排序后 index 0 = 当前前台窗口(is_active 已置位)。
        // The active window: after collect_windows' sort, index 0 is the frontmost (is_active set).
        let Some(active) = windows.iter().find(|w| w.is_active) else {
            return metrics(main_screen_obj);
        };
        let (bx, by, bw, bh) = active.bounds;
        // bounds 全 0 = 未获取到,无法定位,回退主屏。
        // All-zero bounds = unavailable, can't locate, fall back to the main screen.
        if bw <= 0.0 || bh <= 0.0 {
            return metrics(main_screen_obj);
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
                return metrics(s);
            }
            i += 1;
        }
        metrics(main_screen_obj)
    }
}

#[derive(Default)]
struct CardReconcileStats {
    reused: usize,
    created: usize,
    replaced: usize,
    removed: usize,
}

/// Reconcile the persistent card document against the latest window snapshot. Cards are keyed
/// by `(pid, window_id)` rather than their current MRU index, so a reorder only updates frames
/// and side-map indices. A card is rebuilt only when its content or geometry signature changes.
unsafe fn reconcile_card_views(
    document: *mut AnyObject,
    windows: &[WindowInfo],
    placements: &[CardPlacementFrame],
    card_height: f64,
    thumbnail_capture_allowed: bool,
) -> CardReconcileStats {
    let use_new = crate::theme::thumbnails_enabled();
    let mut existing: HashMap<WindowKey, *mut AnyObject> = card_views(document)
        .into_iter()
        .filter_map(|card| card_key(card).map(|key| (key, card)))
        .collect();
    let mut stats = CardReconcileStats::default();

    for &(idx, card_x, card_y, card_w) in placements {
        let Some(window) = windows.get(idx) else {
            continue;
        };
        let key = (window.pid, window.window_id);
        let desired_frame = NSRect::new(
            NSPoint::new(card_x, card_y - status_h()),
            NSSize::new(card_w, card_height),
        );
        let desired_signature =
            card_signature(window, desired_frame, use_new, thumbnail_capture_allowed);

        let card = existing.remove(&key);
        let action = match card {
            None => CardReconcileAction::Create,
            Some(card) => {
                let signature = card_signature_for(card);
                card_reconcile_action(signature.as_ref(), &desired_signature)
            }
        };
        match action {
            CardReconcileAction::Create => {
                let card = create_card_view(
                    window,
                    idx,
                    desired_frame.size.width,
                    desired_frame.size.height,
                    thumbnail_capture_allowed,
                );
                let _: () = msg_send![card, setFrame: desired_frame];
                let _: () = msg_send![document, addSubview: card];
                release_obj(card);
                stats.created += 1;
            }
            CardReconcileAction::Reuse => {
                let card = card.unwrap();
                set_card_index(card, idx);
                let _: () = msg_send![card, setFrame: desired_frame];
                stats.reused += 1;
            }
            CardReconcileAction::Replace => {
                let card = card.unwrap();
                remove_card_index(card);
                let new_card = create_card_view(
                    window,
                    idx,
                    desired_frame.size.width,
                    desired_frame.size.height,
                    thumbnail_capture_allowed,
                );
                let _: () = msg_send![new_card, setFrame: desired_frame];
                let _: () = msg_send![card, removeFromSuperview];
                let _: () = msg_send![document, addSubview: new_card];
                release_obj(new_card);
                stats.replaced += 1;
            }
        }
    }

    for (_, card) in existing {
        remove_card_index(card);
        let _: () = msg_send![card, removeFromSuperview];
        stats.removed += 1;
    }
    stats
}

pub(crate) fn show_overlay() {
    if card_close_in_progress() {
        // 关闭补位期间保持现有 view 树稳定,避免刷新重新创建卡片导致闪烁。
        // Keep the existing view tree stable during close reflow to avoid rebuild flicker.
        return;
    }
    unsafe {
        // TIMING-DEBUG 阶段计时:定位 summon 卡顿——卡片构建 / 图标 / resize / 状态栏。
        let t0 = Instant::now();
        let state_opt = TAB_STATE.lock().unwrap();
        let state = state_opt.as_ref().unwrap();
        let windows = state.windows.clone();
        drop(state_opt);

        let window = OVERLAY_WINDOW.lock().unwrap().unwrap().0;
        let container = CONTAINER.lock().unwrap().unwrap().0;
        let document = CARD_DOCUMENT.lock().unwrap().unwrap().0;

        // 目标屏幕先行计算:流式布局需要屏宽作装箱上限,居中也复用。
        // The target screen comes first: the flow layout needs its width as the
        // packing budget; centering reuses it.
        let (screen_frame, screen_visible, screen_scale) = overlay_target_screen(&windows);
        let use_flow = crate::theme::thumbnails_enabled();
        // TCC preflight 一次覆盖本轮所有卡片，避免 create_card_view 对每张卡重复查询。
        // One TCC preflight covers every card in this render instead of querying once
        // per create_card_view call.
        let thumbnail_capture_allowed = use_flow && crate::thumbnail::capture_allowed();

        // 两种模式都使用同一个完整 document + 可滚动 viewport 模型。
        // Both modes use the same complete-document plus scrollable-viewport model.
        // 高度使用目标屏幕完整 visibleFrame;内容少时 panel_h 仍由实际行数自然收缩,
        // 内容过多时才进入滚动视口。
        // Height uses the target screen's complete visibleFrame; with fewer rows,
        // panel_h still shrinks to the natural row count, and only larger content scrolls.
        let max_panel_h = (screen_visible.size.height * PANEL_MAX_HEIGHT_RATIO).max(240.0);
        let scroll_offset = *THUMB_SCROLL_OFFSET.lock().unwrap();
        let layout = if use_flow {
            // 缩略图按窗口比例平衡分行,纯图标则固定卡片尺寸并自动算列数。
            // Thumbnails balance rows by window aspect; icon-only mode uses fixed cards and auto columns.
            let screen_inner = (screen_frame.size.width - H_PADDING * 2.0).max(160.0);
            let max_panel_w =
                (screen_frame.size.width * PANEL_MAX_WIDTH_RATIO).max(160.0 + H_PADDING * 2.0);
            let max_inner = (max_panel_w - H_PADDING * 2.0 - THUMB_SCROLLBAR_W)
                .min(screen_inner)
                .max(160.0);
            let aspects: Vec<f64> = windows
                .iter()
                .map(|wi| {
                    let (_, _, bw, bh) = wi.bounds;
                    if bw > 0.0 && bh > 0.0 {
                        bw / bh
                    } else {
                        THUMB_PREVIEW_RATIO
                    }
                })
                .collect();
            plan_thumb_scroll_layout(
                &aspects,
                max_inner,
                max_panel_w,
                max_panel_h,
                THUMB_ROW_GAP,
                THUMB_SCROLLBAR_W,
                scroll_offset,
            )
        } else {
            plan_icon_scroll_layout(
                windows.len(),
                screen_frame.size.width,
                max_panel_h,
                THUMB_SCROLLBAR_W,
                scroll_offset,
            )
        };
        *THUMB_VISIBLE_RANGE.lock().unwrap() = Some(layout.visible.clone());
        *THUMB_ROW_RANGES.lock().unwrap() = Some(layout.row_ranges.clone());
        *THUMB_MAX_ROWS.lock().unwrap() = layout.max_rows.max(1);
        *THUMB_SCROLL_ROW.lock().unwrap() = layout.row_start;
        *THUMB_SCROLL_OFFSET.lock().unwrap() = scroll_offset.clamp(0.0, layout.max_scroll_offset);
        *THUMB_SCROLL_MAX_OFFSET.lock().unwrap() = layout.max_scroll_offset;
        *THUMB_SCROLL_ROW_PITCH.lock().unwrap() = layout.card_h
            + if use_flow {
                THUMB_ROW_GAP
            } else {
                ICON_CARD_GAP
            };
        let thumb_scroll_metrics =
            Some((layout.overflowed, layout.row_ranges.len(), layout.max_rows));
        log_debug!(
            "[overlay] layout mode={} visible={}..{} of {} offset={:.1} row={} rows={} visible_rows={} overflow={}",
            if use_flow { "thumbnail" } else { "icon" },
            layout.visible.start,
            layout.visible.end,
            windows.len(),
            scroll_offset,
            layout.row_start,
            layout.row_ranges.len(),
            layout.max_rows,
            layout.overflowed
        );
        // 卡片全部放进 document,由 clip bounds 决定视口;不要只创建可见卡片,否则滚动后没有
        // 后续窗口可供显示。
        // Keep every card in the document and let clip bounds define the viewport; creating only
        // visible cards would leave no later windows to reveal while scrolling.
        let placements: Vec<CardPlacementFrame> = layout
            .document_placements
            .iter()
            .map(|p| (p.index, p.x, p.y, p.width))
            .collect();
        let h = layout.panel_h;
        let w = layout.panel_w;
        let card_h_use = layout.card_h;
        let document_h = layout.document_h;
        let card_h_outer = card_h_use;
        let x = (screen_frame.size.width - w) / 2.0 + screen_frame.origin.x;
        // 高度上限基于 visibleFrame,垂直居中也必须使用同一坐标空间。
        // Otherwise an external display's menu bar/Dock or origin offset leaves
        // asymmetric empty space and makes the overlay appear not to adapt.
        let y = (screen_visible.size.height - h) / 2.0 + screen_visible.origin.y;
        let new_frame = NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));
        // 截图像素需求必须在流式布局确定卡片实际高度后计算：同一块 2x 屏上，少窗口
        // 从 1.0 放大到 1.5 也会从 512px 升到 640px；屏幕热插拔则由本次实时 scale
        // 自然触发升级。旧高清缓存切回低需求屏时继续复用。
        // Compute capture demand only after flow layout determines the actual card height:
        // even on the same 2x screen, a small set growing from 1.0 to 1.5 can upgrade 512px
        // to 640px. Live screen scale naturally handles hot-plug; a higher cached frame remains
        // valid after returning to a lower-demand display.
        let capture_target_px_h = use_flow.then(|| {
            crate::thumbnail::target_px_height(thumb_preview_h(card_h_outer), screen_scale)
        });
        if let Some(target_px_h) = capture_target_px_h {
            *THUMB_CAPTURE_TARGET_PX_H.lock().unwrap() = target_px_h;
            log_debug!(
                "[overlay] thumbnail target_h={} preview_h={:.1}pt backing_scale={:.2}",
                target_px_h,
                thumb_preview_h(card_h_outer),
                screen_scale
            );
        }

        let t_reconcile = Instant::now(); // TIMING-DEBUG
        let reconcile_stats = reconcile_card_views(
            document,
            &windows,
            &placements,
            card_h_outer,
            thumbnail_capture_allowed,
        );
        let reconcile_ms = t_reconcile.elapsed().as_millis(); // TIMING-DEBUG
        let t_cards_ms = t0.elapsed().as_millis(); // TIMING-DEBUG

        let _: () = msg_send![window, setFrame: new_frame, display: false];

        // wrapper / VFX view / container all have autoresizingMask = 18
        // (width + height sizable), so they resize automatically when the
        // window frame changes. Keep the clip view above the status footer.
        let _: () = msg_send![
            container,
            setFrame: NSRect::new(
                NSPoint::new(0.0, status_h()),
                NSSize::new(w, (h - status_h()).max(1.0))
            )
        ];
        let _: () = msg_send![
            document,
            setFrame: NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(w, document_h.max(1.0))
            )
        ];
        *THUMB_DOCUMENT_HEIGHT.lock().unwrap() = document_h.max(1.0);
        apply_thumbnail_clip_offset();
        if let Some((overflowed, row_count, max_rows)) = thumb_scroll_metrics {
            update_thumbnail_scroller(w, h, overflowed, row_count, max_rows);
        } else if let Some(scroller) = thumbnail_scroller() {
            let _: () = msg_send![scroller.0, setHidden: true];
        }

        // 状态栏文本必须在窗口/容器 resize 之后居中:update_status_label 按容器当前宽度
        // 计算 x,若在 resize 前调用会拿旧宽度(启动初为最大宽度、之后为上次召唤的宽度)
        // 定位,容器缩小后文本就偏右/偏左(表现为标题栏不居中)。
        // The status text must be centered AFTER the window/container resize:
        // update_status_label computes x from the container's current width; if called before
        // the resize it uses the stale width (the initial max width at launch, or the previous
        // summon's width), leaving the text off-center once the container shrinks.
        update_status_label();

        let _: () = msg_send![window, setAcceptsMouseMovedEvents: true];
        // 召唤后刷新一次高亮/选中态:新卡片刚创建(⌫ 按钮默认隐藏),选中卡片的
        // 边框与 ⌫ 需要按当前选中项补上。
        // Refresh the highlight/selection once after summoning: fresh cards start with the
        // ⌫ button hidden, so the selected card's border and ⌫ must be applied now.
        refresh_highlight();
        let _: () = msg_send![window, displayIfNeeded];

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
        // NSTrackingMouseEnteredAndExited=0x01 | NSTrackingMouseMoved=0x02 |
        // NSTrackingActiveAlways=0x80 | NSTrackingInVisibleRect=0x200。
        // 注意激活模式(NSTrackingActive*)只能指定一个,多指定会抛 NSInvalidArgumentException。
        // 0x04 = mouseDragged:侧键物理按下期间(吞掉 down 后系统仍可能把移动当作
        // drag 事件)也能收到移动;0x02 = mouseMoved;0x80 = activeAlways;0x200 = inVisibleRect。
        // 0x04 = mouseDragged: while a side button is physically held (the system may still
        // treat moves as drags after the tap swallowed the down) moves still arrive;
        // 0x02 = mouseMoved; 0x80 = activeAlways; 0x200 = inVisibleRect.
        let mm_opts: u64 = 0x01 | 0x02 | 0x04 | 0x80 | 0x200;
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
        // 缩略图召唤期补拍:卡片已按缓存旧帧渲染(无帧走图标兜底),过期/缺失项
        // 入队异步重截,完成后 thumbnailReady → rebuild_cards 原位换卡。
        // Summon-time thumbnail refresh: cards already render their cached frames
        // (icon fallback when absent); stale/missing ones are re-captured async and
        // swapped in place via thumbnailReady -> rebuild_cards.
        if let Some(target_px_h) = capture_target_px_h {
            crate::thumbnail::refresh_for_summon(target_px_h);
        }
        // TIMING-DEBUG 汇总:各阶段耗时(排查 summon 卡顿用)。
        let total_ms = t0.elapsed().as_millis();
        log_debug!(
            "[overlay] show: reconcile={}ms reused={} created={} replaced={} removed={} layout+reconcile={}ms resize+status+highlight={}ms icons={}ms total={}ms",
            reconcile_ms,
            reconcile_stats.reused,
            reconcile_stats.created,
            reconcile_stats.replaced,
            reconcile_stats.removed,
            t_cards_ms,
            t_resize_ms - t_cards_ms,
            t_icons.elapsed().as_millis(),
            total_ms
        );
    }
}
