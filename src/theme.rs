//! 主题与布局:从 CONFIG 派生的配色(Colors)、明暗模式检测、以及卡片/窗口尺寸访问器。
//! 被 overlay 等模块依赖。
//!
//! Theme and layout: config-derived colors (Colors), dark-mode detection, and card/window
//! size accessors. Depended on by overlay and other modules.

use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use std::ffi::c_void;
use std::ops::Range;

use crate::config::{self, CONFIG};
use crate::ffi::{make_nsstring, CFRelease};

// ========== 布局常量 / layout constants ==========

pub(crate) const STATUS_H: f64 = 36.0;
/// 窗口内水平内边距 / horizontal padding inside the window
pub(crate) const H_PADDING: f64 = 32.0;

// ========== 配色 / colors ==========

/// 当前主题解析出的全部颜色(u32 为 RRGGBBAA)。部分字段当前未使用,保留以备扩展。
/// All colors resolved for the current theme (u32 = RRGGBBAA). Some fields are currently
/// unused but kept for future use.
#[allow(dead_code)]
pub(crate) struct Colors {
    pub(crate) page_bg: u32,
    pub(crate) hint_bg: u32,
    pub(crate) hint_text: u32,
    pub(crate) hint_subtext: u32,
    pub(crate) status_bar_bg: u32,
    pub(crate) status_bar_text: u32,
    pub(crate) card_bg: u32,
    pub(crate) card_bg_sel: u32,
    pub(crate) card_border_sel: u32,
    pub(crate) icon_inner_bg: u32,
    pub(crate) icon_text: u32,
    pub(crate) app_name: u32,
    pub(crate) win_title: u32,
    /// 预览区 1px 描边(设计稿 rgba(15,22,32,.10);暗色取白色低透明度)。
    /// The preview's 1px border (mockup rgba(15,22,32,.10); dark uses dim white).
    pub(crate) preview_border: u32,
}

/// 按 dark/light 从 CONFIG 解析颜色。固定字段(页面背景等)目前写死为透明/占位。
/// Resolve colors from CONFIG for dark/light. Fixed fields (page bg, etc.) are hard-coded
/// to transparent / placeholder for now.
pub(crate) fn colors_from_config(dark: bool) -> Colors {
    let cfg = CONFIG.read().unwrap();
    let c = if dark {
        &cfg.colors.dark
    } else {
        &cfg.colors.light
    };
    Colors {
        page_bg: 0x00000000,
        hint_bg: 0x00000000,
        hint_text: 0x888888ff,
        hint_subtext: 0x666666ff,
        status_bar_bg: 0x00000000,
        status_bar_text: config::parse_hex8(&c.status_bar_text),
        card_bg: 0x00000000,
        card_bg_sel: config::parse_hex8(&c.card_bg_sel),
        card_border_sel: config::parse_hex8(&c.card_border_sel),
        icon_inner_bg: config::parse_hex8(&c.icon_inner_bg),
        icon_text: config::parse_hex8(&c.icon_text),
        app_name: config::parse_hex8(&c.app_name),
        win_title: config::parse_hex8(&c.win_title),
        preview_border: if dark { 0xFFFFFF24 } else { 0x0F16201A },
    }
}

/// 系统是否处于深色模式(查 NSUserDefaults AppleInterfaceStyle,非空即深色)。
/// Whether the system is in dark mode (NSUserDefaults AppleInterfaceStyle non-null => dark).
pub(crate) fn system_dark_mode() -> bool {
    unsafe {
        let key = make_nsstring("AppleInterfaceStyle");
        let defaults: *mut AnyObject = msg_send![class!(NSUserDefaults), standardUserDefaults];
        let style: *mut AnyObject = msg_send![defaults, stringForKey: key];
        CFRelease(key as *const c_void);
        !style.is_null()
    }
}

/// 按 CONFIG.appearance.theme 解析当前颜色(theme=auto 时跟随系统明暗)。
/// Resolve current colors per CONFIG.appearance.theme (auto follows system dark/light).
pub(crate) fn current_colors() -> Colors {
    let is_dark = match CONFIG.read().unwrap().appearance.theme.as_str() {
        "light" => false,
        "dark" => true,
        _ => system_dark_mode(),
    };
    colors_from_config(is_dark)
}

// ========== 布局访问器(运行时从 CONFIG 读取)/ layout accessors (read from CONFIG at runtime) ==========

pub(crate) fn cards_per_row() -> usize {
    CONFIG.read().unwrap().layout.cards_per_row
}
pub(crate) fn card_w() -> f64 {
    CONFIG.read().unwrap().layout.card_width
}
pub(crate) fn card_h() -> f64 {
    CONFIG.read().unwrap().layout.card_height
}
pub(crate) fn card_gap() -> f64 {
    CONFIG.read().unwrap().layout.card_gap
}
pub(crate) fn icon_px() -> f64 {
    CONFIG.read().unwrap().layout.icon_size
}
pub(crate) fn letter_px() -> f64 {
    icon_px() * 0.5
}

/// 窗口缩略图总开关(无屏幕录制权限时 thumbnail 模块内部还会二次休眠)。
/// Window-thumbnail master switch (the thumbnail module additionally sleeps
/// without the Screen Recording permission).
pub(crate) fn thumbnails_enabled() -> bool {
    CONFIG.read().unwrap().layout.thumbnails_enabled
}

// ========== 缩略图卡片布局(HTML 设计稿 preview (6).html)/ thumbnail card layout ==========

/// 流式布局基准卡宽,代码内写死、与旧版图标网格的 card_width 完全独立:
/// 对齐参考实现(BetterCmdTab 有效 312 / DockDoor 300 / 设计稿 ≈295)。
/// Flow-layout base card width, hard-coded and fully independent of the legacy
/// icon grid's card_width: aligned with the references (BetterCmdTab effective
/// 312 / DockDoor 300 / mockup ~295).
pub(crate) const THUMB_CARD_BASE_W: f64 = 300.0;
/// 流式布局行间距(设计稿 .grid gap 14px)。
/// Flow-layout row/inter-card gap (the mockup's .grid gap of 14px).
pub(crate) const THUMB_ROW_GAP: f64 = 14.0;
/// 滚动缩略图视口右侧滚动条的保留宽度。
/// Width reserved for the scrollbar at the right edge of the scrolling thumbnail viewport.
pub(crate) const THUMB_SCROLLBAR_W: f64 = 14.0;
/// 卡片内边距(设计稿 .item padding 8px)。/ Card inner padding (.item padding 8px).
pub(crate) const THUMB_PAD: f64 = 8.0;
/// 标题行高(设计稿 .caption 34px 含 7px 底距,取净高 24)。/ Caption row height.
pub(crate) const THUMB_CAPTION_H: f64 = 24.0;
/// 标题行与预览区的间距。/ Gap between caption and preview.
pub(crate) const THUMB_GAP: f64 = 6.0;
/// 预览区宽高比(设计稿 aspect-ratio 16/10)。/ Preview aspect ratio (16/10).
pub(crate) const THUMB_PREVIEW_RATIO: f64 = 1.6;
/// 少量窗口时的最大卡片放大倍数；1.0 是原有缩略图卡片尺寸。
/// Maximum card enlargement for small window sets; 1.0 is the original thumbnail size.
pub(crate) const THUMB_MAX_SCALE: f64 = 1.5;
/// 卡片区顶部留白。/ Top inset above the thumbnail card area.
const THUMB_TOP_INSET: f64 = 32.0;

/// 缩略图尺寸只由窗口总数决定，分行与窗口宽高比不能反向改变尺寸。
/// Thumbnail scale depends only on the total window count; wrapping and window
/// aspect ratios must not feed back into card size.
pub(crate) fn thumb_scale_for_count(count: usize) -> f64 {
    match count {
        0 | 7.. => 1.0,
        1 | 2 => THUMB_MAX_SCALE,
        3 => 1.4,
        4 => 1.3,
        5 => 1.2,
        6 => 1.1,
    }
}

/// 缩略图卡片高度按基准卡宽推导(纯函数,可测):
/// 上下 padding + 标题行 + 间距 + 16:10 预览区。
/// Thumbnail card height derives from the base width per the mockup (pure,
/// testable): vertical paddings + caption + gap + a 16:10 preview.
fn thumb_card_h(card_width: f64) -> f64 {
    let preview_h = (card_width - THUMB_PAD * 2.0) / THUMB_PREVIEW_RATIO;
    THUMB_PAD * 2.0 + THUMB_CAPTION_H + THUMB_GAP + preview_h
}

/// 流式布局的统一卡片高度:由基准卡宽推导一次,全网格等高。
/// The flow layout's uniform card height: derived once from the base width;
/// every card shares it.
#[cfg(test)]
pub(crate) fn thumb_card_h_fixed() -> f64 {
    thumb_card_h(THUMB_CARD_BASE_W)
}

/// scale 以基准卡宽为准，标题行和内边距保持固定，不机械放大文字与控件。
/// Scale is based on the base card width; caption and padding stay fixed instead
/// of mechanically enlarging text and controls.
pub(crate) fn thumb_card_h_for_scale(scale: f64) -> f64 {
    thumb_card_h(THUMB_CARD_BASE_W * scale.max(1.0))
}

/// 预览区高度 = 卡片高 - 上下 padding - 标题行 - 间距(纯函数,可测)。
/// Preview height = card height - vertical paddings - caption - gap (pure, testable).
pub(crate) fn thumb_preview_h(card_h: f64) -> f64 {
    (card_h - THUMB_PAD * 2.0 - THUMB_CAPTION_H - THUMB_GAP).max(40.0)
}

/// 宽高比钳制:窗口可能极端扁/极端高,预览宽度过窄会不可辨认、过宽会霸满一行。
/// Aspect clamp: windows can be extremely wide/tall; unclamped cards would become
/// unrecognizably narrow or hog an entire row.
pub(crate) fn clamp_aspect(aspect: f64) -> f64 {
    if !aspect.is_finite() || aspect <= 0.0 {
        return THUMB_PREVIEW_RATIO; // 退化输入回退 16:10 / degenerate -> 16:10
    }
    aspect.clamp(0.7, 2.2)
}

/// 等高流式布局的卡片宽度 = 预览高 × 宽高比 + 左右 padding(纯函数,可测)。
/// Flow-layout card width = preview height × aspect + side paddings (pure, testable).
pub(crate) fn thumb_card_w_for_aspect(card_h: f64, aspect: f64) -> f64 {
    thumb_preview_h(card_h) * clamp_aspect(aspect) + THUMB_PAD * 2.0
}

/// 平衡行装箱:保持输入顺序，先最小化行数，再最小化各行剩余宽度平方和并轻度
/// 惩罚孤立卡片。这样不会改变 MRU 顺序，但会把换行点从参差的贪心结果调整为
/// 更均衡的连续分段。
///
/// Balanced row packing: preserve input order, minimize row count first, then
/// minimize squared leftover width with a small singleton penalty. This keeps MRU
/// order while choosing more balanced contiguous line breaks than greedy wrapping.
pub(crate) fn pack_rows(widths: &[f64], max_inner_w: f64, gap: f64) -> Vec<Vec<usize>> {
    if widths.is_empty() {
        return vec![Vec::new()];
    }
    let max_inner_w = max_inner_w.max(1.0);
    let len = widths.len();
    let mut best_rows = vec![usize::MAX; len + 1];
    let mut best_cost = vec![f64::INFINITY; len + 1];
    let mut previous = vec![0usize; len + 1];
    best_rows[0] = 0;
    best_cost[0] = 0.0;

    for end in 1..=len {
        let mut row_w = 0.0;
        for start in (0..end).rev() {
            row_w += widths[start];
            if start + 1 < end {
                row_w += gap;
            }
            let single = start + 1 == end;
            if row_w > max_inner_w + 1e-9 && !single {
                break;
            }
            if best_rows[start] == usize::MAX {
                continue;
            }
            let rows = best_rows[start] + 1;
            let leftover = (max_inner_w - row_w.min(max_inner_w)).max(0.0);
            let singleton_penalty = if single && len > 1 {
                max_inner_w * max_inner_w * 0.05
            } else {
                0.0
            };
            let cost = best_cost[start] + leftover * leftover + singleton_penalty;
            if rows < best_rows[end] || (rows == best_rows[end] && cost < best_cost[end] - 1e-9) {
                best_rows[end] = rows;
                best_cost[end] = cost;
                previous[end] = start;
            }
        }
    }

    let mut rows = Vec::with_capacity(best_rows[len]);
    let mut end = len;
    while end > 0 {
        let start = previous[end];
        rows.push((start..end).collect());
        end = start;
    }
    rows.reverse();
    rows
}

/// 溢出滚动时按 MRU 顺序贪心填充每一行,优先让初始视口容纳最多窗口。
/// Greedy MRU-order packing for overflow scrolling, prioritizing the most windows in the initial viewport.
fn pack_rows_greedy(widths: &[f64], max_inner_w: f64, gap: f64) -> Vec<Vec<usize>> {
    if widths.is_empty() {
        return vec![Vec::new()];
    }
    let max_inner_w = max_inner_w.max(1.0);
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut row_w = 0.0;

    for (index, &width) in widths.iter().enumerate() {
        let next_w = if row.is_empty() {
            width
        } else {
            row_w + gap + width
        };
        if !row.is_empty() && next_w > max_inner_w + 1e-9 {
            rows.push(row);
            row = Vec::new();
            row_w = 0.0;
        }
        if row.is_empty() {
            row_w = width;
        } else {
            row_w += gap + width;
        }
        row.push(index);
    }
    rows.push(row);
    rows
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ThumbPlacement {
    pub(crate) index: usize,
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) width: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ThumbFlowLayout {
    pub(crate) panel_w: f64,
    pub(crate) panel_h: f64,
    pub(crate) card_h: f64,
    pub(crate) scale: f64,
    pub(crate) visible: Range<usize>,
    pub(crate) placements: Vec<ThumbPlacement>,
    pub(crate) overflowed: bool,
    pub(crate) page_index: usize,
    pub(crate) page_count: usize,
    /// 完整流式布局的行范围;滚动模式按这些稳定行切换视口。
    /// Complete flow-layout row ranges; scrolling mode moves the viewport by these stable rows.
    pub(crate) row_ranges: Vec<Range<usize>>,
    pub(crate) row_start: usize,
    pub(crate) max_rows: usize,
    pub(crate) max_scroll_offset: f64,
}

fn thumb_widths(aspects: &[f64], range: &Range<usize>, card_h: f64, max_inner: f64) -> Vec<f64> {
    aspects[range.clone()]
        .iter()
        // 极窄屏幕下钳制单卡宽度；图片仍按 aspect-fit 完整显示，只增加留白。
        // Clamp a single card on exceptionally narrow screens; the image remains
        // fully visible via aspect-fit and merely gains letterboxing.
        .map(|&aspect| thumb_card_w_for_aspect(card_h, aspect).min(max_inner))
        .collect()
}

fn thumb_max_rows(card_h: f64, max_panel_h: f64, gap: f64) -> usize {
    let available = (max_panel_h - THUMB_TOP_INSET - STATUS_H).max(card_h);
    ((available + gap) / (card_h + gap)).floor().max(1.0) as usize
}

#[derive(Clone, Copy)]
struct ThumbFlowConstraints {
    card_h: f64,
    max_inner: f64,
    max_rows: usize,
    gap: f64,
}

#[cfg(test)]
fn thumb_range_fits(
    aspects: &[f64],
    range: &Range<usize>,
    constraints: ThumbFlowConstraints,
) -> bool {
    pack_rows(
        &thumb_widths(aspects, range, constraints.card_h, constraints.max_inner),
        constraints.max_inner,
        constraints.gap,
    )
    .len()
        <= constraints.max_rows
}

#[cfg(test)]
fn maximal_prefix(
    aspects: &[f64],
    start: usize,
    constraints: ThumbFlowConstraints,
) -> Range<usize> {
    let mut end = start.min(aspects.len());
    while end < aspects.len() {
        let candidate = start..end + 1;
        if end == start || thumb_range_fits(aspects, &candidate, constraints) {
            end += 1;
        } else {
            break;
        }
    }
    start.min(end)..end
}

#[cfg(test)]
fn stable_pages(aspects: &[f64], constraints: ThumbFlowConstraints) -> Vec<Range<usize>> {
    if aspects.is_empty() {
        return std::iter::once(0..0).collect();
    }
    let mut pages = Vec::new();
    let mut start = 0;
    while start < aspects.len() {
        let page = maximal_prefix(aspects, start, constraints);
        start = page.end;
        pages.push(page);
    }
    pages
}

#[cfg(test)]
fn build_thumb_layout(
    aspects: &[f64],
    visible: Range<usize>,
    scale: f64,
    constraints: ThumbFlowConstraints,
    overflowed: bool,
    page_index: usize,
    page_count: usize,
) -> ThumbFlowLayout {
    let card_h = constraints.card_h;
    let max_inner = constraints.max_inner;
    let gap = constraints.gap;
    let widths = thumb_widths(aspects, &visible, card_h, max_inner);
    let rows = pack_rows(&widths, max_inner, gap);
    let n_rows = rows.len().max(1);
    let used_inner_w = rows
        .iter()
        .map(|row| {
            row.iter().map(|&i| widths[i]).sum::<f64>() + row.len().saturating_sub(1) as f64 * gap
        })
        .fold(0.0f64, f64::max);
    // 宽度始终跟随当前页最宽的一行；分页只固定页面边界和高度，不再为较窄页面
    // 保留整块最大网格的空白。
    // Width follows the widest row on the current page. Pagination still keeps stable
    // page boundaries and height, but no longer reserves the full grid for narrower pages.
    let panel_inner_w = used_inner_w.max(280.0_f64.min(max_inner));
    let (panel_w, panel_h) = if overflowed {
        let max_rows = constraints.max_rows;
        (
            panel_inner_w + H_PADDING * 2.0,
            THUMB_TOP_INSET
                + max_rows as f64 * card_h
                + max_rows.saturating_sub(1) as f64 * gap
                + STATUS_H,
        )
    } else {
        (
            panel_inner_w + H_PADDING * 2.0,
            THUMB_TOP_INSET
                + n_rows as f64 * card_h
                + n_rows.saturating_sub(1) as f64 * gap
                + STATUS_H,
        )
    };
    let mut placements = Vec::with_capacity(visible.len());
    let row_ranges = rows
        .iter()
        .filter_map(|row| Some(visible.start + row.first()?..visible.start + row.last()? + 1))
        .collect::<Vec<_>>();
    for (row_index, row) in rows.iter().enumerate() {
        if row.is_empty() {
            continue;
        }
        let row_w =
            row.iter().map(|&i| widths[i]).sum::<f64>() + row.len().saturating_sub(1) as f64 * gap;
        let mut x = (panel_w - row_w) / 2.0;
        let y =
            panel_h - THUMB_TOP_INSET - (row_index as f64 + 1.0) * card_h - row_index as f64 * gap;
        for &local_index in row {
            placements.push(ThumbPlacement {
                index: visible.start + local_index,
                x,
                y,
                width: widths[local_index],
            });
            x += widths[local_index] + gap;
        }
    }
    ThumbFlowLayout {
        panel_w,
        panel_h,
        card_h,
        scale,
        visible,
        placements,
        overflowed,
        page_index,
        page_count,
        row_ranges,
        row_start: 0,
        max_rows: constraints.max_rows,
        max_scroll_offset: 0.0,
    }
}

/// 以完整窗口列表进行一次稳定分行,再只取连续的可见行构建视口。
/// Pack the complete window list once, then build the viewport from a contiguous slice of rows.
fn build_thumb_scroll_layout(
    all_rows: &[Vec<usize>],
    widths: &[f64],
    scale: f64,
    constraints: ThumbFlowConstraints,
    max_panel_w: f64,
    scrollbar_w: f64,
    scroll_offset: f64,
) -> ThumbFlowLayout {
    let overflowed = all_rows.len() > constraints.max_rows;
    let viewport_row_count = if overflowed {
        constraints.max_rows
    } else {
        all_rows.len().max(1)
    };
    let row_pitch = constraints.card_h + constraints.gap;
    let total_content_h = all_rows.len().max(1) as f64 * constraints.card_h
        + all_rows.len().saturating_sub(1) as f64 * constraints.gap;
    let viewport_h = viewport_row_count as f64 * constraints.card_h
        + viewport_row_count.saturating_sub(1) as f64 * constraints.gap;
    let max_scroll_offset = (total_content_h - viewport_h).max(0.0);
    let scroll_offset = scroll_offset.clamp(0.0, max_scroll_offset);
    let max_row_start = all_rows.len().saturating_sub(viewport_row_count);
    let row_start = if row_pitch > 0.0 {
        (scroll_offset / row_pitch).floor() as usize
    } else {
        0
    }
    .min(max_row_start);
    let intra_row_offset = (scroll_offset - row_start as f64 * row_pitch).max(0.0);
    let has_partial_row = overflowed
        && intra_row_offset > f64::EPSILON
        && row_start + viewport_row_count < all_rows.len();
    let rendered_row_count = viewport_row_count + usize::from(has_partial_row);
    let row_end = (row_start + rendered_row_count).min(all_rows.len());
    let visible = match (
        all_rows.get(row_start),
        all_rows.get(row_end.saturating_sub(1)),
    ) {
        (Some(first), Some(last)) => {
            first.first().copied().unwrap_or(0)..last.last().map_or(0, |i| i + 1)
        }
        _ => 0..0,
    };
    let used_inner_w = all_rows
        .iter()
        .map(|row| {
            row.iter().map(|&i| widths[i]).sum::<f64>()
                + row.len().saturating_sub(1) as f64 * constraints.gap
        })
        .fold(0.0f64, f64::max);
    let panel_w = if overflowed {
        max_panel_w.max(used_inner_w + H_PADDING * 2.0 + scrollbar_w)
    } else {
        used_inner_w.max(280.0_f64.min(constraints.max_inner)) + H_PADDING * 2.0
    };
    let rendered_rows = viewport_row_count.max(1);
    let panel_h = THUMB_TOP_INSET
        + rendered_rows as f64 * constraints.card_h
        + rendered_rows.saturating_sub(1) as f64 * constraints.gap
        + STATUS_H;
    let card_area_w = if overflowed {
        (panel_w - scrollbar_w).max(1.0)
    } else {
        panel_w
    };
    let mut placements = Vec::with_capacity(visible.len());
    for (local_row, row) in all_rows[row_start..row_end].iter().enumerate() {
        let row_w = row.iter().map(|&i| widths[i]).sum::<f64>()
            + row.len().saturating_sub(1) as f64 * constraints.gap;
        let mut x = (card_area_w - row_w) / 2.0;
        let y = panel_h
            - THUMB_TOP_INSET
            - (local_row as f64 + 1.0) * constraints.card_h
            - local_row as f64 * constraints.gap
            + intra_row_offset;
        for &index in row {
            placements.push(ThumbPlacement {
                index,
                x,
                y,
                width: widths[index],
            });
            x += widths[index] + constraints.gap;
        }
    }
    ThumbFlowLayout {
        panel_w,
        panel_h,
        card_h: constraints.card_h,
        scale,
        visible,
        placements,
        overflowed,
        page_index: 0,
        page_count: 1,
        row_ranges: all_rows
            .iter()
            .filter_map(|row| Some(row.first().copied()?..row.last().copied()? + 1))
            .collect(),
        row_start,
        max_rows: viewport_row_count,
        max_scroll_offset,
    }
}

/// 规划缩略图网格：窗口总数先决定 1.0–1.5 倍尺寸，比例宽度随后平衡分行；
/// 放不下时保持该尺寸并使用从索引 0 开始的稳定分页。
/// Plan the thumbnail grid: total window count first determines the 1.0–1.5 scale,
/// then aspect-width cards are balanced into rows. Overflow retains that size and
/// uses deterministic pages beginning at index zero.
#[cfg(test)]
pub(crate) fn plan_thumb_flow_layout(
    aspects: &[f64],
    selected: usize,
    max_inner: f64,
    max_panel_h: f64,
    gap: f64,
) -> ThumbFlowLayout {
    let max_inner = max_inner.max(1.0);
    let scale = thumb_scale_for_count(aspects.len());
    let card_h = thumb_card_h_for_scale(scale);
    let constraints = ThumbFlowConstraints {
        card_h,
        max_inner,
        max_rows: thumb_max_rows(card_h, max_panel_h, gap),
        gap,
    };
    if aspects.is_empty() {
        return build_thumb_layout(aspects, 0..0, scale, constraints, false, 0, 1);
    }
    let pages = stable_pages(aspects, constraints);
    let selected = selected.min(aspects.len() - 1);
    let page_index = pages
        .iter()
        .position(|page| page.contains(&selected))
        .unwrap_or(0);
    let visible = pages[page_index].clone();
    let page_count = pages.len();
    build_thumb_layout(
        aspects,
        visible,
        scale,
        constraints,
        page_count > 1,
        page_index,
        page_count,
    )
}

/// 规划连续滚动的缩略图视口。单页时使用平衡分行;发生溢出时按 MRU 顺序优先填满前面的行,
/// 让初始视口容纳最多窗口。完整分行结果保持不变,滚动偏移按 point 计算,因此卡片可以平滑地
/// 经过视口边缘。
/// Plan the continuous thumbnail viewport. Use balanced rows when everything fits; when it
/// overflows, fill leading rows in MRU order so the initial viewport shows as many windows as
/// possible. The complete row plan stays unchanged, while point-based scrolling lets cards pass
/// smoothly through the viewport edges.
pub(crate) fn plan_thumb_scroll_layout(
    aspects: &[f64],
    max_inner: f64,
    max_panel_w: f64,
    max_panel_h: f64,
    gap: f64,
    scrollbar_w: f64,
    scroll_offset: f64,
) -> ThumbFlowLayout {
    let max_inner = max_inner.max(1.0);
    let scale = thumb_scale_for_count(aspects.len());
    let card_h = thumb_card_h_for_scale(scale);
    let constraints = ThumbFlowConstraints {
        card_h,
        max_inner,
        max_rows: thumb_max_rows(card_h, max_panel_h, gap),
        gap,
    };
    let widths = thumb_widths(aspects, &(0..aspects.len()), card_h, max_inner);
    let balanced_rows = pack_rows(&widths, max_inner, gap);
    let rows = if balanced_rows.len() > constraints.max_rows {
        pack_rows_greedy(&widths, max_inner, gap)
    } else {
        balanced_rows
    };
    build_thumb_scroll_layout(
        &rows,
        &widths,
        scale,
        constraints,
        max_panel_w,
        scrollbar_w,
        scroll_offset,
    )
}

/// 浮窗高度 = 顶部 32 + 行数 * 卡片高 + 状态栏高(纯函数,可测)。
/// 仅旧版(纯图标)网格使用;缩略图流式布局在 show_overlay 内自行计算。
/// Overlay height = top 32 + rows * card height + status bar (pure, testable).
/// Legacy icon-grid only; the thumbnail flow layout computes its own in show_overlay.
fn compute_window_height(count: usize, cards_per_row: usize, card_h: f64) -> f64 {
    let rows = count.max(1).div_ceil(cards_per_row);
    32.0 + rows as f64 * card_h + STATUS_H
}

/// 浮窗高度 = 顶部 32 + 行数 * 卡片高 + 状态栏高。
/// Overlay height = top 32 + rows * card height + status bar height.
pub(crate) fn window_height(count: usize) -> f64 {
    compute_window_height(count, cards_per_row(), card_h())
}

/// 浮窗宽度 = 卡片数 * 卡片宽 + 间距 + 两侧内边距(纯函数,可测)。
/// 下限为单卡宽度:任何情况下都不允许出现细条状窗口。
/// Overlay width = cards * card width + gaps + padding on both sides (pure, testable).
/// Floor is one card's width: the overlay must never degenerate into a thin strip.
fn compute_window_width(cards_in_row: usize, card_w: f64, card_gap: f64) -> f64 {
    let n = cards_in_row.max(1);
    n as f64 * card_w + (n - 1) as f64 * card_gap + H_PADDING * 2.0
}

/// 浮窗宽度 = 卡片数 * 卡片宽 + 间距 + 两侧内边距。
/// 下限为单卡宽度(cards_in_row.max(1)):任何情况下都不允许出现细条状窗口
/// (空窗口态由 show_overlay 单独取三卡宽度,这里是兜底)。
/// Overlay width = cards * card width + gaps + padding on both sides.
/// Floor is one card's width (cards_in_row.max(1)): the overlay must never degenerate into a
/// thin strip (the empty state takes the three-card width in show_overlay; this is the floor).
pub(crate) fn window_width(cards_in_row: usize) -> f64 {
    compute_window_width(cards_in_row, card_w(), card_gap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn height_uses_at_least_one_row() {
        // 0 个窗口:至少一行,高度兜底。
        // Zero windows: at least one row as the floor.
        assert_eq!(compute_window_height(0, 5, 100.0), 32.0 + 100.0 + STATUS_H);
        assert_eq!(compute_window_height(1, 5, 100.0), 32.0 + 100.0 + STATUS_H);
    }

    #[test]
    fn height_rounds_rows_up() {
        // 每行 4 个:5 个窗口 -> 2 行。
        // Four per row: five windows -> two rows.
        assert_eq!(
            compute_window_height(5, 4, 120.0),
            32.0 + 2.0 * 120.0 + STATUS_H
        );
        assert_eq!(
            compute_window_height(8, 4, 120.0),
            32.0 + 2.0 * 120.0 + STATUS_H
        );
        assert_eq!(
            compute_window_height(9, 4, 120.0),
            32.0 + 3.0 * 120.0 + STATUS_H
        );
    }

    #[test]
    fn width_floors_at_one_card() {
        // 0 或 1 张卡:宽度 = 单卡宽 + 两侧内边距(无间距)。
        // Zero or one card: width = one card + both paddings (no gap).
        let w0 = compute_window_width(0, 100.0, 20.0);
        let w1 = compute_window_width(1, 100.0, 20.0);
        assert_eq!(w0, 100.0 + H_PADDING * 2.0);
        assert_eq!(w1, w0);
        // 3 张卡:3 * 卡宽 + 2 * 间距 + 内边距。
        // Three cards: 3 * card_w + 2 * gaps + padding.
        assert_eq!(
            compute_window_width(3, 100.0, 20.0),
            300.0 + 40.0 + H_PADDING * 2.0
        );
        // 卡宽为 0 时依然不塌缩成负宽度(兜底单卡)。
        // Even with a zero card width the floor keeps it non-degenerate.
        assert!(compute_window_width(0, 0.0, 0.0) > 0.0);
    }
}

#[cfg(test)]
mod flow_tests {
    use super::*;

    #[test]
    fn thumb_heights_derive_consistently() {
        // 统一高度 = 由配置卡宽推导;预览高 = 高度 - 上下 padding - 标题行 - 间距。
        // The uniform height derives from the configured card width; preview height
        // subtracts paddings + caption + gap.
        let h = thumb_card_h_fixed();
        assert!(h > 100.0, "sanity: {} too small", h);
        let ph = thumb_preview_h(h);
        assert!((h - (ph + THUMB_PAD * 2.0 + THUMB_CAPTION_H + THUMB_GAP)).abs() < 1e-9);
        // 预览高有 40pt 下限,极端小配置不塌缩。
        // The 40pt floor keeps extreme configs from collapsing.
        assert!(thumb_preview_h(50.0) >= 40.0);
    }

    #[test]
    fn card_width_scales_with_aspect_and_clamps() {
        let h = 160.0;
        let ph = thumb_preview_h(h);
        // 16:10 窗口:预览宽 = 预览高 × 1.6,卡宽加左右 padding。
        // A 16:10 window: preview width = preview height × 1.6; card adds side pads.
        let w = thumb_card_w_for_aspect(h, 1.6);
        assert!((w - (ph * 1.6 + THUMB_PAD * 2.0)).abs() < 1e-9);
        // 极端比例被钳制:竖版不低于 0.7,横版不高于 2.2。
        // Extreme aspects clamp: portrait >= 0.7, landscape <= 2.2.
        let tall = thumb_card_w_for_aspect(h, 0.2);
        assert!((tall - (ph * 0.7 + THUMB_PAD * 2.0)).abs() < 1e-9);
        let wide = thumb_card_w_for_aspect(h, 5.0);
        assert!((wide - (ph * 2.2 + THUMB_PAD * 2.0)).abs() < 1e-9);
        // 退化输入(0/NaN)回退 16:10。
        // Degenerate inputs (0 / NaN) fall back to 16:10.
        let deg = thumb_card_w_for_aspect(h, f64::NAN);
        assert!((deg - (ph * THUMB_PREVIEW_RATIO + THUMB_PAD * 2.0)).abs() < 1e-9);
    }

    #[test]
    fn pack_rows_wraps_on_overflow_and_keeps_order() {
        // 预算 250,卡宽 [100, 100, 100, 100]:每行 2 张(100+10+100=210,再加第三张超)。
        // Budget 250, widths [100x4]: two per row (210; a third would overflow).
        let rows = pack_rows(&[100.0, 100.0, 100.0, 100.0], 250.0, 10.0);
        assert_eq!(rows, vec![vec![0, 1], vec![2, 3]]);
        // 单卡超预算独占一行;顺序保持输入序。
        // An oversized single card gets its own row; order is preserved.
        let rows = pack_rows(&[50.0, 400.0, 50.0], 250.0, 10.0);
        assert_eq!(rows, vec![vec![0], vec![1], vec![2]]);
        // 空输入返回一行空行(空窗口态保底)。
        // Empty input yields one empty row (the empty-state floor).
        assert_eq!(pack_rows(&[], 250.0, 10.0), vec![Vec::<usize>::new()]);
    }

    #[test]
    fn pack_rows_balances_mixed_widths_without_reordering() {
        // 贪心会排成 [宽+窄] / [窄]；平衡分行把宽卡独立放置，避免末行孤卡。
        // Greedy would produce [wide+narrow] / [narrow]; balanced wrapping keeps the
        // wide card alone and avoids an orphaned final row.
        let rows = pack_rows(&[400.0, 150.0, 150.0], 614.0, 14.0);
        assert_eq!(rows, vec![vec![0], vec![1, 2]]);
    }

    #[test]
    fn thumbnail_scale_depends_only_on_window_count() {
        assert_eq!(thumb_scale_for_count(0), 1.0);
        assert_eq!(thumb_scale_for_count(1), 1.5);
        assert_eq!(thumb_scale_for_count(2), 1.5);
        assert_eq!(thumb_scale_for_count(3), 1.4);
        assert_eq!(thumb_scale_for_count(4), 1.3);
        assert_eq!(thumb_scale_for_count(5), 1.2);
        assert_eq!(thumb_scale_for_count(6), 1.1);
        assert_eq!(thumb_scale_for_count(7), 1.0);
        assert_eq!(thumb_scale_for_count(30), 1.0);
    }

    #[test]
    fn flow_layout_enlarges_small_sets_to_the_cap() {
        let layout = plan_thumb_flow_layout(&[1.6, 1.6], 1, 1200.0, 1000.0, THUMB_ROW_GAP);
        assert_eq!(layout.visible, 0..2);
        assert!(!layout.overflowed);
        assert!((layout.scale - THUMB_MAX_SCALE).abs() < 1e-9);
        assert!(layout.card_h > thumb_card_h_fixed());
    }

    #[test]
    fn scale_is_unchanged_when_aspects_cause_different_wrapping() {
        let standard = plan_thumb_flow_layout(&[1.6, 1.6, 1.6], 1, 1200.0, 1000.0, THUMB_ROW_GAP);
        let mixed = plan_thumb_flow_layout(&[2.2, 0.7, 2.2], 1, 800.0, 1000.0, THUMB_ROW_GAP);
        assert_eq!(standard.scale, 1.4);
        assert_eq!(mixed.scale, 1.4);
    }

    #[test]
    fn overflow_uses_stable_pages_instead_of_sliding() {
        let aspects = vec![1.6; 8];
        let base_h = thumb_card_h_fixed();
        let two_row_panel_h = 32.0 + base_h * 2.0 + THUMB_ROW_GAP + STATUS_H + 0.1;
        let initial = plan_thumb_flow_layout(&aspects, 1, 900.0, two_row_panel_h, THUMB_ROW_GAP);
        assert!(initial.overflowed);
        assert_eq!(initial.scale, 1.0);
        assert_eq!(initial.visible, 0..4);
        // 两行各两张卡时，宽度按当前页最宽行收缩，而不是占满 900pt 预算。
        // With two rows of two cards, width follows the widest actual row instead of
        // consuming the full 900pt packing budget.
        assert_eq!(
            initial.panel_w,
            2.0 * 300.0 + THUMB_ROW_GAP + H_PADDING * 2.0
        );

        let next = plan_thumb_flow_layout(&aspects, 4, 900.0, two_row_panel_h, THUMB_ROW_GAP);
        assert_eq!(next.visible, 4..8);
        assert_eq!(next.page_index, 1);
        assert_eq!(next.page_count, 2);
        assert_eq!(next.panel_w, initial.panel_w);
        assert_eq!(next.panel_h, initial.panel_h);
        assert_eq!(
            next.placements
                .iter()
                .map(|placement| placement.index)
                .collect::<Vec<_>>(),
            vec![4, 5, 6, 7]
        );
    }

    #[test]
    fn wide_thumbnail_budget_fits_four_columns_without_changing_height() {
        let aspects = vec![1.6; 12];
        let card_h = thumb_card_h_fixed();
        let three_row_panel_h =
            THUMB_TOP_INSET + card_h * 3.0 + THUMB_ROW_GAP * 2.0 + STATUS_H + 0.1;
        let capped = plan_thumb_flow_layout(
            &aspects,
            1,
            1240.0 - H_PADDING * 2.0,
            three_row_panel_h,
            THUMB_ROW_GAP,
        );
        let wide = plan_thumb_flow_layout(&aspects, 1, 1288.0, three_row_panel_h, THUMB_ROW_GAP);

        // 旧上限下每行只能放三张；放宽横向预算后四列可用，12 张保持一页。
        // Under the old cap only three cards fit per row; with the wider budget four
        // columns fit and all twelve cards remain on one page.
        assert_eq!(capped.visible, 0..9);
        assert!(capped.overflowed);
        assert_eq!(wide.visible, 0..12);
        assert!(!wide.overflowed);
        assert_eq!(wide.page_count, 1);
        // 只改变横向容量，三行高度应保持一致。
        // Only horizontal capacity changes; the three-row height remains identical.
        assert_eq!(wide.panel_h, capped.panel_h);
    }

    #[test]
    fn selecting_any_item_on_a_page_keeps_the_same_page_boundary() {
        let aspects = vec![1.6; 8];
        let base_h = thumb_card_h_fixed();
        let max_h = 32.0 + base_h * 2.0 + THUMB_ROW_GAP + STATUS_H + 0.1;
        for selected in 4..8 {
            let layout = plan_thumb_flow_layout(&aspects, selected, 614.0, max_h, THUMB_ROW_GAP);
            assert_eq!(layout.visible, 4..8);
        }
    }

    #[test]
    fn mixed_aspect_pages_are_contiguous_exhaustive_and_stable() {
        let aspects = vec![2.2, 0.7, 1.6, 2.2, 1.0, 1.6, 0.7, 2.2, 1.6];
        let scale = thumb_scale_for_count(aspects.len());
        let card_h = thumb_card_h_for_scale(scale);
        let constraints = ThumbFlowConstraints {
            card_h,
            max_inner: 614.0,
            max_rows: 2,
            gap: THUMB_ROW_GAP,
        };
        let pages = stable_pages(&aspects, constraints);
        assert_eq!(pages.first().unwrap().start, 0);
        assert_eq!(pages.last().unwrap().end, aspects.len());
        for pair in pages.windows(2) {
            assert_eq!(pair[0].end, pair[1].start);
        }
        for page in &pages {
            assert!(!page.is_empty());
            assert!(thumb_range_fits(&aspects, page, constraints));
            for selected in page.clone() {
                let layout = plan_thumb_flow_layout(
                    &aspects,
                    selected,
                    constraints.max_inner,
                    THUMB_TOP_INSET
                        + constraints.max_rows as f64 * card_h
                        + THUMB_ROW_GAP
                        + STATUS_H,
                    THUMB_ROW_GAP,
                );
                assert_eq!(&layout.visible, page);
            }
        }
    }

    #[test]
    fn overflow_capacity_adapts_to_mixed_aspects() {
        let aspects = vec![1.6, 1.6, 2.2, 2.2, 2.2, 2.2];
        let card_h = thumb_card_h_for_scale(thumb_scale_for_count(aspects.len()));
        let max_h = 32.0 + card_h * 2.0 + THUMB_ROW_GAP + STATUS_H + 0.1;
        let initial = plan_thumb_flow_layout(&aspects, 1, 700.0, max_h, THUMB_ROW_GAP);
        // 两张标准卡可同排，宽卡只能独占一排，因此首段容量自然降为 3。
        // Two standard cards share a row while a wide card occupies its own, so
        // the leading slice naturally drops to three items.
        assert_eq!(initial.visible, 0..3);
    }

    #[test]
    fn empty_flow_layout_stays_at_base_size() {
        let layout = plan_thumb_flow_layout(&[], 0, 1200.0, 1000.0, THUMB_ROW_GAP);
        assert_eq!(layout.visible, 0..0);
        assert_eq!(layout.scale, 1.0);
        assert!(layout.placements.is_empty());
    }

    #[test]
    fn scrolling_layout_keeps_rows_and_panel_size_stable() {
        let aspects = vec![1.6; 20];
        let card_h = thumb_card_h_fixed();
        let max_h = THUMB_TOP_INSET + card_h * 2.0 + THUMB_ROW_GAP + STATUS_H + 0.1;
        let first = plan_thumb_scroll_layout(
            &aspects,
            1288.0,
            1400.0,
            max_h,
            THUMB_ROW_GAP,
            THUMB_SCROLLBAR_W,
            0.0,
        );
        let next = plan_thumb_scroll_layout(
            &aspects,
            1288.0,
            1400.0,
            max_h,
            THUMB_ROW_GAP,
            THUMB_SCROLLBAR_W,
            card_h + THUMB_ROW_GAP,
        );

        assert!(first.overflowed);
        assert_eq!(first.row_ranges.len(), 5);
        assert_eq!(first.visible, 0..8);
        assert_eq!(next.visible, 4..12);
        assert_eq!(first.panel_w, next.panel_w);
        assert_eq!(first.panel_h, next.panel_h);
        assert_eq!(first.row_ranges, next.row_ranges);
        assert_eq!(
            next.placements
                .iter()
                .map(|placement| placement.index)
                .collect::<Vec<_>>(),
            (4..12).collect::<Vec<_>>()
        );
    }

    #[test]
    fn scrolling_overflow_fills_the_initial_viewport_greedily() {
        let aspects = vec![1.6; 13];
        let card_h = thumb_card_h_fixed();
        let max_h = THUMB_TOP_INSET + card_h * 3.0 + THUMB_ROW_GAP * 2.0 + STATUS_H + 0.1;
        let layout = plan_thumb_scroll_layout(
            &aspects,
            1288.0,
            1400.0,
            max_h,
            THUMB_ROW_GAP,
            THUMB_SCROLLBAR_W,
            0.0,
        );

        assert!(layout.overflowed);
        assert_eq!(layout.row_ranges, vec![0..4, 4..8, 8..12, 12..13]);
        assert_eq!(layout.visible, 0..12);
    }

    #[test]
    fn scrolling_layout_keeps_balanced_packing_when_everything_fits() {
        let aspects = vec![2.2, 0.7, 0.7];
        let card_h = thumb_card_h_for_scale(thumb_scale_for_count(aspects.len()));
        let max_h = THUMB_TOP_INSET + card_h * 2.0 + THUMB_ROW_GAP + STATUS_H + 0.1;
        let layout = plan_thumb_scroll_layout(
            &aspects,
            800.0,
            900.0,
            max_h,
            THUMB_ROW_GAP,
            THUMB_SCROLLBAR_W,
            0.0,
        );

        assert!(!layout.overflowed);
        assert_eq!(layout.row_ranges, vec![0..1, 1..3]);
        assert_eq!(layout.visible, 0..3);
    }

    #[test]
    fn scrolling_layout_clamps_to_the_last_row() {
        let aspects = vec![1.6; 20];
        let card_h = thumb_card_h_fixed();
        let max_h = THUMB_TOP_INSET + card_h * 2.0 + THUMB_ROW_GAP + STATUS_H + 0.1;
        let layout = plan_thumb_scroll_layout(
            &aspects,
            1288.0,
            1400.0,
            max_h,
            THUMB_ROW_GAP,
            THUMB_SCROLLBAR_W,
            f64::MAX,
        );
        assert_eq!(layout.row_start, 3);
        assert_eq!(layout.visible, 12..20);
    }

    #[test]
    fn scrolling_layout_keeps_fractional_offset_and_renders_partial_row() {
        let aspects = vec![1.6; 20];
        let card_h = thumb_card_h_fixed();
        let row_pitch = card_h + THUMB_ROW_GAP;
        let max_h = THUMB_TOP_INSET + card_h * 2.0 + THUMB_ROW_GAP + STATUS_H + 0.1;
        let layout = plan_thumb_scroll_layout(
            &aspects,
            1288.0,
            1400.0,
            max_h,
            THUMB_ROW_GAP,
            THUMB_SCROLLBAR_W,
            12.0,
        );

        assert_eq!(layout.row_start, 0);
        assert_eq!(layout.visible, 0..12);
        assert!(layout
            .placements
            .iter()
            .any(|placement| placement.index == 8));
        assert!(layout
            .placements
            .iter()
            .any(|placement| placement.index == 11));
        assert!(layout.max_scroll_offset > 12.0);

        let before_row_boundary = plan_thumb_scroll_layout(
            &aspects,
            1288.0,
            1400.0,
            max_h,
            THUMB_ROW_GAP,
            THUMB_SCROLLBAR_W,
            row_pitch - 1.0,
        );
        let at_row_boundary = plan_thumb_scroll_layout(
            &aspects,
            1288.0,
            1400.0,
            max_h,
            THUMB_ROW_GAP,
            THUMB_SCROLLBAR_W,
            row_pitch,
        );
        let before_y = before_row_boundary
            .placements
            .iter()
            .find(|placement| placement.index == 4)
            .map(|placement| placement.y)
            .unwrap();
        let boundary_y = at_row_boundary
            .placements
            .iter()
            .find(|placement| placement.index == 4)
            .map(|placement| placement.y)
            .unwrap();
        assert!((boundary_y - before_y - 1.0).abs() < 1e-9);
    }
}
