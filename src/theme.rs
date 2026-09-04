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
/// 浮窗宽度的最大屏幕占比;内容不足时仍按自然尺寸收缩。
/// Maximum overlay width as a share of the screen; it still shrinks to the
/// natural content size when there is room.
pub(crate) const PANEL_MAX_WIDTH_RATIO: f64 = 0.92;
/// 浮窗高度使用目标屏幕的完整可视区域;内容不足时仍按自然尺寸收缩。
/// Overlay height uses the target screen's complete visible area; it still
/// shrinks to the natural content size when there is room.
pub(crate) const PANEL_MAX_HEIGHT_RATIO: f64 = 1.0;
/// 纯图标模式的基准卡片尺寸;宽度固定,高度会为放大的内容自动留出空间。
/// Base icon-only card dimensions; width stays fixed while height reserves room for larger content.
pub(crate) const ICON_CARD_W: f64 = 140.0;
pub(crate) const ICON_CARD_H: f64 = 180.0;
pub(crate) const ICON_CARD_GAP: f64 = 0.0;
pub(crate) const ICON_SIZE: f64 = 110.0;
pub(crate) const CARD_TEXT_BASE_SIZE: f64 = 12.0;
pub(crate) const CARD_TEXT_SIZE_MIN: f64 = 13.0;
pub(crate) const CARD_TEXT_SIZE_MAX: f64 = 20.0;
const STATUS_BAR_TEXT_BASE_SIZE: f64 = 13.0;

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

/// Settings and auxiliary panels use custom layer-backed surfaces, so they need a complete
/// palette instead of relying on AppKit semantic colors for only part of the hierarchy.
/// 设置界面和辅助面板包含自绘图层,需要完整调色板来保持所有层级的主题一致。
#[derive(Clone, Copy)]
pub(crate) struct UiPalette {
    pub(crate) dark: bool,
    pub(crate) window_bg: u32,
    pub(crate) sidebar_bg: u32,
    pub(crate) detail_bg: u32,
    pub(crate) footer_bg: u32,
    pub(crate) card_bg: u32,
    pub(crate) card_border: u32,
    pub(crate) separator: u32,
    pub(crate) field_bg: u32,
    pub(crate) primary_text: u32,
    pub(crate) secondary_text: u32,
    pub(crate) sidebar_text: u32,
    pub(crate) muted_text: u32,
    pub(crate) disabled_text: u32,
    pub(crate) button_bg: u32,
    pub(crate) button_text: u32,
    pub(crate) footer_button_bg: u32,
    pub(crate) selection_bg: u32,
    pub(crate) hover_bg: u32,
    pub(crate) accent: u32,
    pub(crate) accent_hover: u32,
    pub(crate) shadow: u32,
}

/// Palette for the native settings window and other custom UI surfaces.
pub(crate) fn ui_palette() -> UiPalette {
    if resolved_is_dark() {
        UiPalette {
            dark: true,
            window_bg: 0x1C1C1EE8,
            sidebar_bg: 0x2C2C2EDB,
            detail_bg: 0x1C1C1EE8,
            footer_bg: 0x2C2C2EDB,
            card_bg: 0x2C2C2EEA,
            card_border: 0xFFFFFF1C,
            separator: 0xFFFFFF20,
            field_bg: 0xFFFFFF1C,
            primary_text: 0xF5F5F7FF,
            secondary_text: 0xEBEBF5A3,
            sidebar_text: 0xEBEBF5A3,
            muted_text: 0xEBEBF56B,
            disabled_text: 0xEBEBF552,
            button_bg: 0xFFFFFF1C,
            button_text: 0xF5F5F7FF,
            footer_button_bg: 0xFFFFFF25,
            selection_bg: 0x0A84FF38,
            hover_bg: 0xFFFFFF22,
            accent: 0x0A84FFFF,
            accent_hover: 0x0077EDFF,
            shadow: 0x00000042,
        }
    } else {
        UiPalette {
            dark: false,
            // Match the light reference surfaces: #f6f7f9 window/detail, #f1f2f4 sidebar,
            // and rgba(255,255,255,.82) grouped settings rows/cards.
            // 对齐浅色参考图层：窗口/详情区为 #f6f7f9，侧栏为 #f1f2f4，分组设置条目/卡片为
            // rgba(255,255,255,.82)。
            window_bg: 0xF6F7F9FF,
            sidebar_bg: 0xF1F2F4FF,
            detail_bg: 0xF6F7F9FF,
            footer_bg: 0xF8F8F9D1,
            card_bg: 0xFFFFFFD1,
            card_border: 0x00000012,
            separator: 0x00000016,
            field_bg: 0x7676801C,
            primary_text: 0x2C2C30FF,
            secondary_text: 0x73737AFF,
            sidebar_text: 0x686970FF,
            muted_text: 0x9B9BA2FF,
            disabled_text: 0xAEAEB5FF,
            button_bg: 0xFFFFFFAD,
            button_text: 0x2E2E2EFF,
            footer_button_bg: 0xFFFFFFC7,
            // Keep the accent hue, but use a lighter wash so the selected sidebar row does not
            // compete with enabled switches and other blue controls.
            // 保留强调色相，同时降低底色强度，避免选中侧栏条目抢过启用开关等蓝色控件。
            selection_bg: 0x0A84FF16,
            hover_bg: 0x76768024,
            accent: 0x0A84FFFF,
            accent_hover: 0x0077EDFF,
            shadow: 0x0000000A,
        }
    }
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
        hint_text: if dark { 0xF5F5F7FF } else { 0x888888ff },
        hint_subtext: if dark { 0xB8B8C0FF } else { 0x666666ff },
        status_bar_bg: 0x00000000,
        // 底部标题栏与卡片窗口标题使用同一主文本色;应用名仍保留独立的次要文本色。
        // The footer and card window title share the same primary text color; the app name keeps
        // its separate secondary text color.
        status_bar_text: config::parse_hex8(&c.win_title),
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

/// Resolve the effective appearance once so every native panel follows the same light/dark rule.
/// 统一解析最终外观,让所有原生面板使用相同的浅色/深色判断。
pub(crate) fn resolved_is_dark() -> bool {
    match CONFIG.read().unwrap().appearance.theme.as_str() {
        "light" => false,
        "dark" => true,
        _ => system_dark_mode(),
    }
}

/// 按 CONFIG.appearance.theme 解析当前颜色(theme=auto 时跟随系统明暗)。
/// Resolve current colors per CONFIG.appearance.theme (auto follows system dark/light).
pub(crate) fn current_colors() -> Colors {
    colors_from_config(resolved_is_dark())
}

// ========== 固定布局访问器 / fixed layout accessors ==========

pub(crate) fn card_w() -> f64 {
    ICON_CARD_W
}
pub(crate) fn card_h() -> f64 {
    let scale = text_scale();
    // Keep the default card height stable. When text grows, add room for the two larger
    // rows below the unchanged large icon.
    ICON_CARD_H + ((18.0 * scale + 2.0 + 16.0 * scale) - 36.0).max(0.0)
}
pub(crate) fn card_gap() -> f64 {
    ICON_CARD_GAP
}
pub(crate) fn icon_px() -> f64 {
    ICON_SIZE
}
pub(crate) fn letter_px() -> f64 {
    icon_px() * 0.5
}

/// Scale shared by the card's window-title and app-name text.
pub(crate) fn text_scale() -> f64 {
    CONFIG
        .read()
        .unwrap()
        .layout
        .card_text_size
        .clamp(CARD_TEXT_SIZE_MIN, CARD_TEXT_SIZE_MAX)
        / CARD_TEXT_BASE_SIZE
}

pub(crate) fn card_title_font_size() -> f64 {
    let cfg = CONFIG.read().unwrap();
    cfg.fonts.title_size
        * cfg
            .layout
            .card_text_size
            .clamp(CARD_TEXT_SIZE_MIN, CARD_TEXT_SIZE_MAX)
        / CARD_TEXT_BASE_SIZE
}

pub(crate) fn card_app_name_font_size() -> f64 {
    let cfg = CONFIG.read().unwrap();
    cfg.fonts.app_name_size
        * cfg
            .layout
            .card_text_size
            .clamp(CARD_TEXT_SIZE_MIN, CARD_TEXT_SIZE_MAX)
        / CARD_TEXT_BASE_SIZE
}

/// Bottom title-bar text size, exposed through the switcher settings page.
pub(crate) fn status_bar_text_size() -> f64 {
    let size = CONFIG.read().unwrap().fonts.status_bar_size;
    if size.is_finite() {
        size.clamp(13.0, 20.0)
    } else {
        STATUS_BAR_TEXT_BASE_SIZE
    }
}

/// The footer grows with its text so the selected window title remains vertically centered.
pub(crate) fn status_h() -> f64 {
    status_bar_height_for_text_size(status_bar_text_size())
}

pub(crate) fn status_bar_height_for_text_size(size: f64) -> f64 {
    let size = if size.is_finite() {
        size.clamp(13.0, 20.0)
    } else {
        STATUS_BAR_TEXT_BASE_SIZE
    };
    STATUS_H * size / STATUS_BAR_TEXT_BASE_SIZE
}

/// Thumbnail captions use the same setting, but grow only as much as their caption row needs.
pub(crate) fn thumb_caption_h() -> f64 {
    (THUMB_CAPTION_H * text_scale()).clamp(20.0, 36.0)
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
/// 溢出滚动时在底部露出的下一行比例,用于提示下方仍有内容。
/// Fraction of the next row exposed at the bottom of an overflowing viewport.
pub(crate) const THUMB_SCROLL_TEASER_RATIO: f64 = 1.0 / 3.0;
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
    THUMB_PAD * 2.0 + thumb_caption_h() + THUMB_GAP + preview_h
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
    (card_h - THUMB_PAD * 2.0 - thumb_caption_h() - THUMB_GAP).max(40.0)
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
    /// 完整 document 中所有卡片的稳定坐标,滚动时不重新计算或改写。
    /// Stable coordinates for every card in the complete document; scrolling never rewrites them.
    pub(crate) document_placements: Vec<ThumbPlacement>,
    /// document 高度(不含状态栏,对应 NSClipView 的 document view 高度)。
    /// Document height excluding the status bar, matching the NSClipView document view height.
    pub(crate) document_h: f64,
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
    let available = (max_panel_h - THUMB_TOP_INSET - status_h()).max(card_h);
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
                + status_h(),
        )
    } else {
        (
            panel_inner_w + H_PADDING * 2.0,
            THUMB_TOP_INSET
                + n_rows as f64 * card_h
                + n_rows.saturating_sub(1) as f64 * gap
                + status_h(),
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
        document_placements: placements.clone(),
        document_h: (panel_h - status_h()).max(1.0),
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
    _max_panel_w: f64,
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
    // When content overflows, reserve one extra gap plus a third of a card so the next
    // row is visibly clipped at the bottom of the viewport. This is an intentional
    // discoverability affordance, not an additional fully visible row.
    let teaser_h = if overflowed {
        constraints.gap + constraints.card_h * THUMB_SCROLL_TEASER_RATIO
    } else {
        0.0
    };
    let viewport_h = viewport_row_count as f64 * constraints.card_h
        + viewport_row_count.saturating_sub(1) as f64 * constraints.gap
        + teaser_h;
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
    // The taller viewport exposes the next row even at an exact row boundary;
    // fractional scrolling continues to use the same clipped-row path.
    let has_partial_row = overflowed && row_start + viewport_row_count < all_rows.len();
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
    // 溢出只代表需要纵向滚动,不代表面板必须铺满最大屏幕宽度。之前这里直接
    // 取 max_panel_w,导致四列卡片被放在近乎整屏的面板中央,左右出现大块空白。
    // The need for vertical scrolling does not mean the panel must fill the maximum screen
    // width. Using max_panel_w here put four-card rows in an almost full-screen panel and left
    // large empty margins on both sides. The row packing already respects max_inner, so the
    // widest actual row is the natural width; only reserve the scrollbar beside it.
    let panel_w = if overflowed {
        used_inner_w + H_PADDING * 2.0 + scrollbar_w
    } else {
        used_inner_w.max(280.0_f64.min(constraints.max_inner)) + H_PADDING * 2.0
    };
    let rendered_rows = viewport_row_count.max(1);
    let panel_h = THUMB_TOP_INSET
        + rendered_rows as f64 * constraints.card_h
        + rendered_rows.saturating_sub(1) as f64 * constraints.gap
        + teaser_h
        + status_h();
    let card_area_w = if overflowed {
        (panel_w - scrollbar_w).max(1.0)
    } else {
        panel_w
    };
    // 把卡片网格与右侧滚动条作为一个整体居中。滚动条只占右侧空间时,
    // 单纯在剩余区域居中会让整个视觉组合向左偏半个滚动条宽度。
    // Center the card grid together with the right-hand scrollbar. Centering only within
    // the remaining area shifts the whole visual group left by half the scrollbar width.
    let scrollbar_centering_offset = if overflowed { scrollbar_w / 2.0 } else { 0.0 };
    let document_h = THUMB_TOP_INSET
        + all_rows.len() as f64 * constraints.card_h
        + all_rows.len().saturating_sub(1) as f64 * constraints.gap;
    let document_panel_h = document_h + status_h();
    let mut document_placements = Vec::new();
    for (row_index, row) in all_rows.iter().enumerate() {
        let row_w = row.iter().map(|&i| widths[i]).sum::<f64>()
            + row.len().saturating_sub(1) as f64 * constraints.gap;
        let mut x = (card_area_w - row_w) / 2.0 + scrollbar_centering_offset;
        let y = document_panel_h
            - THUMB_TOP_INSET
            - (row_index as f64 + 1.0) * constraints.card_h
            - row_index as f64 * constraints.gap;
        for &index in row {
            document_placements.push(ThumbPlacement {
                index,
                x,
                y,
                width: widths[index],
            });
            x += widths[index] + constraints.gap;
        }
    }
    let mut placements = Vec::with_capacity(visible.len());
    for (local_row, row) in all_rows[row_start..row_end].iter().enumerate() {
        let row_w = row.iter().map(|&i| widths[i]).sum::<f64>()
            + row.len().saturating_sub(1) as f64 * constraints.gap;
        let mut x = (card_area_w - row_w) / 2.0 + scrollbar_centering_offset;
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
        document_placements,
        document_h,
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

/// 计算关闭一张卡片后的原位重排坐标,保留当前卡片尺寸和 document 顶部锚点。
/// Plan in-place coordinates after closing one card while keeping the current card size and
/// document top anchor stable during the transition.
pub(crate) fn plan_thumb_close_reflow(
    widths: &[f64],
    card_h: f64,
    card_area_w: f64,
    max_inner: f64,
    gap: f64,
    document_h: f64,
    overflowed: bool,
) -> (Vec<ThumbPlacement>, Vec<Range<usize>>) {
    // 溢出布局必须继续按 MRU 顺序贪心填行,这样关闭后后续卡片会优先补进空位。
    // Overflow layouts must keep greedy MRU-order packing so following cards fill the released slot first.
    let rows = if overflowed {
        pack_rows_greedy(widths, max_inner, gap)
    } else {
        pack_rows(widths, max_inner, gap)
    };
    let row_ranges = rows
        .iter()
        .filter_map(|row| Some(row.first().copied()?..row.last().copied()? + 1))
        .collect::<Vec<_>>();
    let document_h = document_h.max(1.0);
    let mut placements = Vec::with_capacity(widths.len());
    for (row_index, row) in rows.iter().enumerate() {
        if row.is_empty() {
            continue;
        }
        let row_w = row.iter().map(|&index| widths[index]).sum::<f64>()
            + row.len().saturating_sub(1) as f64 * gap;
        let mut x = (card_area_w - row_w) / 2.0
            + if overflowed {
                THUMB_SCROLLBAR_W / 2.0
            } else {
                0.0
            };
        let y = document_h
            - THUMB_TOP_INSET
            - (row_index as f64 + 1.0) * card_h
            - row_index as f64 * gap;
        for &index in row {
            placements.push(ThumbPlacement {
                index,
                x,
                y,
                width: widths[index],
            });
            x += widths[index] + gap;
        }
    }
    (placements, row_ranges)
}

/// 计算关闭重排后的 document 高度;至少覆盖当前 clip view,避免内容变短后出现非法滚动范围。
/// Compute the post-close document height; it always covers the current clip view so shrinking
/// content never leaves the clip view with an invalid scroll range.
pub(crate) fn thumb_document_height_for_rows(row_count: usize, card_h: f64, gap: f64) -> f64 {
    THUMB_TOP_INSET + row_count.max(1) as f64 * card_h + row_count.saturating_sub(1) as f64 * gap
}

/// 同步 document 缩放后的最大偏移、当前偏移与坐标平移量。
/// Reconcile max/current scroll offsets and the coordinate delta after resizing the document.
pub(crate) fn rebase_thumb_scroll_after_document_resize(
    old_document_h: f64,
    new_document_h: f64,
    viewport_h: f64,
    old_offset: f64,
) -> (f64, f64, f64, f64) {
    let old_document_h = old_document_h.max(1.0);
    let viewport_h = viewport_h.max(1.0);
    let new_document_h = new_document_h.max(viewport_h).max(1.0);
    let max_offset = (new_document_h - viewport_h).max(0.0);
    let offset = old_offset.clamp(0.0, max_offset);
    let delta = new_document_h - old_document_h;
    (new_document_h, max_offset, offset, delta)
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

/// 把窗口均匀分配到指定数量的行,每行数量最多相差一张。
/// Distribute windows evenly across the requested rows; row sizes differ by at most one card.
fn balanced_icon_row_ranges(count: usize, row_count: usize) -> Vec<Range<usize>> {
    if count == 0 || row_count == 0 {
        return Vec::new();
    }
    let base = count / row_count;
    let remainder = count % row_count;
    let mut start = 0;
    (0..row_count)
        .map(|row| {
            let length = base + usize::from(row < remainder);
            let range = start..start + length;
            start += length;
            range
        })
        .collect()
}

/// 规划纯图标视口:宽度固定,高度随内容设置调整,列数由屏幕宽度计算,溢出后连续滚动。
/// Plan the icon-only viewport: width stays fixed, height follows the content setting, columns
/// come from screen width, and overflow becomes continuously scrollable.
pub(crate) fn plan_icon_scroll_layout(
    count: usize,
    screen_width: f64,
    max_panel_h: f64,
    scrollbar_w: f64,
    scroll_offset: f64,
) -> ThumbFlowLayout {
    let gap = ICON_CARD_GAP;
    let card_h = card_h();
    let max_panel_w = (screen_width.max(1.0) * PANEL_MAX_WIDTH_RATIO)
        .max(ICON_CARD_W + H_PADDING * 2.0 + scrollbar_w);
    let max_inner = (max_panel_w - H_PADDING * 2.0 - scrollbar_w).max(ICON_CARD_W);
    let max_columns = ((max_inner + gap) / (ICON_CARD_W + gap)).floor().max(1.0) as usize;
    // 少量窗口仍保留旧版三卡宽基线,但卡片本身始终保持固定尺寸。
    // Keep the legacy three-slot baseline for small sets, while card dimensions remain fixed.
    let baseline_columns = count.max(3).min(max_columns);
    let minimum_rows = if count == 0 {
        0
    } else {
        count.div_ceil(max_columns)
    };
    let max_rows = ((max_panel_h - THUMB_TOP_INSET - status_h()).max(card_h) / (card_h + gap))
        .floor()
        .max(1.0) as usize;
    let overflowed = minimum_rows > max_rows;
    let columns = if overflowed {
        max_columns
    } else if count < 3 {
        baseline_columns
    } else {
        count.div_ceil(minimum_rows.max(1))
    };
    let row_ranges: Vec<Range<usize>> = if count == 0 {
        Vec::new()
    } else if overflowed {
        (0..count)
            .step_by(max_columns)
            .map(|start| start..(start + max_columns).min(count))
            .collect()
    } else if count < 3 {
        std::iter::once(0..count).collect()
    } else {
        balanced_icon_row_ranges(count, minimum_rows)
    };
    let row_count = row_ranges.len();
    let visual_row_count = row_count.max(1);
    let viewport_rows = if overflowed {
        max_rows
    } else {
        visual_row_count
    };
    let panel_columns = if overflowed { max_columns } else { columns };
    let grid_w = panel_columns as f64 * ICON_CARD_W + panel_columns.saturating_sub(1) as f64 * gap;
    let panel_w = if overflowed {
        max_panel_w
    } else {
        grid_w + H_PADDING * 2.0
    };
    let card_area_w = if overflowed {
        (panel_w - scrollbar_w).max(ICON_CARD_W + H_PADDING * 2.0)
    } else {
        panel_w
    };
    let scrollbar_centering_offset = if overflowed { scrollbar_w / 2.0 } else { 0.0 };
    let row_pitch = card_h + gap;
    let total_content_h = THUMB_TOP_INSET
        + visual_row_count as f64 * card_h
        + visual_row_count.saturating_sub(1) as f64 * gap;
    let teaser_h = if overflowed {
        gap + card_h * THUMB_SCROLL_TEASER_RATIO
    } else {
        0.0
    };
    let viewport_content_h = THUMB_TOP_INSET
        + viewport_rows as f64 * card_h
        + viewport_rows.saturating_sub(1) as f64 * gap
        + teaser_h;
    let max_scroll_offset = (total_content_h - viewport_content_h).max(0.0);
    let scroll_offset = scroll_offset.clamp(0.0, max_scroll_offset);
    let max_row_start = row_count.saturating_sub(viewport_rows);
    let row_start = if row_count == 0 {
        0
    } else {
        (scroll_offset / row_pitch).floor() as usize
    }
    .min(max_row_start);
    let intra_row_offset = (scroll_offset - row_start as f64 * row_pitch).max(0.0);
    let has_partial_row = row_count > 0 && row_start + viewport_rows < row_count;
    let rendered_row_count = viewport_rows + usize::from(has_partial_row);
    let row_end = (row_start + rendered_row_count).min(row_count);
    let visible = match (
        row_ranges.get(row_start),
        row_ranges.get(row_end.saturating_sub(1)),
    ) {
        (Some(first), Some(last)) => first.start..last.end,
        _ => 0..0,
    };
    let panel_h = THUMB_TOP_INSET
        + viewport_rows as f64 * card_h
        + viewport_rows.saturating_sub(1) as f64 * gap
        + teaser_h
        + status_h();
    let document_h = total_content_h;
    let document_panel_h = document_h + status_h();
    let mut document_placements = Vec::with_capacity(count);
    for (row_index, row) in row_ranges.iter().enumerate() {
        let row_w = row.len() as f64 * ICON_CARD_W + row.len().saturating_sub(1) as f64 * gap;
        let row_x = (card_area_w - row_w) / 2.0 + scrollbar_centering_offset;
        let y = document_panel_h
            - THUMB_TOP_INSET
            - (row_index as f64 + 1.0) * card_h
            - row_index as f64 * gap;
        for (column, index) in row.clone().enumerate() {
            document_placements.push(ThumbPlacement {
                index,
                x: row_x + column as f64 * (ICON_CARD_W + gap),
                y,
                width: ICON_CARD_W,
            });
        }
    }
    let mut placements = Vec::new();
    for (local_row, row) in row_ranges[row_start..row_end].iter().enumerate() {
        let row_w = row.len() as f64 * ICON_CARD_W + row.len().saturating_sub(1) as f64 * gap;
        let row_x = (card_area_w - row_w) / 2.0 + scrollbar_centering_offset;
        let y =
            panel_h - THUMB_TOP_INSET - (local_row as f64 + 1.0) * card_h - local_row as f64 * gap
                + intra_row_offset;
        for (column, index) in row.clone().enumerate() {
            placements.push(ThumbPlacement {
                index,
                x: row_x + column as f64 * (ICON_CARD_W + gap),
                y,
                width: ICON_CARD_W,
            });
        }
    }
    ThumbFlowLayout {
        panel_w,
        panel_h,
        card_h,
        document_placements,
        document_h,
        scale: 1.0,
        visible,
        placements,
        overflowed,
        page_index: 0,
        page_count: 1,
        row_ranges,
        row_start,
        max_rows: viewport_rows,
        max_scroll_offset,
    }
}

/// 纯图标初始浮窗高度 = 顶部 32 + 行数 * 卡片高 + 状态栏高(纯函数,可测)。
/// 仅用于创建浮窗时的初始占位尺寸;实际召唤时由滚动布局重新计算。
/// Initial icon-only overlay height = top 32 + rows * card height + status bar (pure, testable).
/// Used only for the initial window placeholder; summon-time layout recalculates it.
fn compute_window_height(count: usize, cards_per_row: usize, card_h: f64) -> f64 {
    let rows = count.max(1).div_ceil(cards_per_row);
    32.0 + rows as f64 * card_h + status_h()
}

/// 纯图标初始浮窗高度;实际布局会按屏幕宽度动态计算列数。
/// Initial icon-only overlay height; the live layout computes columns from screen width.
pub(crate) fn window_height(count: usize) -> f64 {
    compute_window_height(count, 6, card_h())
}

/// 浮窗宽度 = 卡片数 * 固定卡片宽 + 间距 + 两侧内边距(纯函数,可测)。
/// 下限为单卡宽度:任何情况下都不允许出现细条状窗口。
/// Overlay width = cards * fixed card width + gaps + padding on both sides (pure, testable).
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
    fn status_bar_height_tracks_text_size() {
        assert_eq!(status_bar_height_for_text_size(13.0), STATUS_H);
        assert_eq!(
            status_bar_height_for_text_size(20.0),
            STATUS_H * 20.0 / STATUS_BAR_TEXT_BASE_SIZE
        );
        assert_eq!(
            status_bar_height_for_text_size(26.0),
            status_bar_height_for_text_size(20.0)
        );
        assert_eq!(
            status_bar_height_for_text_size(4.0),
            STATUS_H * 13.0 / STATUS_BAR_TEXT_BASE_SIZE
        );
        assert_eq!(status_bar_height_for_text_size(f64::NAN), STATUS_H);
    }

    #[test]
    fn height_uses_at_least_one_row() {
        // 0 个窗口:至少一行,高度兜底。
        // Zero windows: at least one row as the floor.
        assert_eq!(
            compute_window_height(0, 5, 100.0),
            32.0 + 100.0 + status_h()
        );
        assert_eq!(
            compute_window_height(1, 5, 100.0),
            32.0 + 100.0 + status_h()
        );
    }

    #[test]
    fn height_rounds_rows_up() {
        // 每行 4 个:5 个窗口 -> 2 行。
        // Four per row: five windows -> two rows.
        assert_eq!(
            compute_window_height(5, 4, 120.0),
            32.0 + 2.0 * 120.0 + status_h()
        );
        assert_eq!(
            compute_window_height(8, 4, 120.0),
            32.0 + 2.0 * 120.0 + status_h()
        );
        assert_eq!(
            compute_window_height(9, 4, 120.0),
            32.0 + 3.0 * 120.0 + status_h()
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
        assert!((h - (ph + THUMB_PAD * 2.0 + thumb_caption_h() + THUMB_GAP)).abs() < 1e-9);
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
        let two_row_panel_h = 32.0 + base_h * 2.0 + THUMB_ROW_GAP + status_h() + 0.1;
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
            THUMB_TOP_INSET + card_h * 3.0 + THUMB_ROW_GAP * 2.0 + status_h() + 0.1;
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
        let max_h = 32.0 + base_h * 2.0 + THUMB_ROW_GAP + status_h() + 0.1;
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
                        + status_h()
                        + 1.0,
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
        let max_h = 32.0 + card_h * 2.0 + THUMB_ROW_GAP + status_h() + 0.1;
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
        let max_h = THUMB_TOP_INSET + card_h * 2.0 + THUMB_ROW_GAP + status_h() + 0.1;
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
        assert_eq!(first.visible, 0..12);
        assert_eq!(next.visible, 4..16);
        assert_eq!(first.panel_w, next.panel_w);
        assert_eq!(first.panel_h, next.panel_h);
        assert_eq!(first.row_ranges, next.row_ranges);
        assert_eq!(
            next.placements
                .iter()
                .map(|placement| placement.index)
                .collect::<Vec<_>>(),
            (4..16).collect::<Vec<_>>()
        );
    }

    #[test]
    fn scrolling_layout_width_follows_the_widest_visible_grid_row() {
        let aspects = vec![1.6; 8];
        let card_h = thumb_card_h_fixed();
        let max_h = THUMB_TOP_INSET + card_h * 2.0 + THUMB_ROW_GAP + status_h() + 0.1;
        let layout = plan_thumb_scroll_layout(
            &aspects,
            900.0,
            1400.0,
            max_h,
            THUMB_ROW_GAP,
            THUMB_SCROLLBAR_W,
            0.0,
        );

        assert!(layout.overflowed);
        assert_eq!(layout.row_ranges, vec![0..2, 2..4, 4..6, 6..8]);
        assert_eq!(
            layout.panel_w,
            2.0 * 300.0 + THUMB_ROW_GAP + H_PADDING * 2.0 + THUMB_SCROLLBAR_W
        );
        let first = layout.document_placements.first().unwrap();
        assert_eq!(first.x, H_PADDING + THUMB_SCROLLBAR_W / 2.0);
        assert_eq!(
            layout.panel_w - (first.x + first.width * 2.0 + THUMB_ROW_GAP),
            H_PADDING + THUMB_SCROLLBAR_W / 2.0
        );
        assert!(layout.panel_w < 1400.0);
    }

    #[test]
    fn scrolling_overflow_fills_the_initial_viewport_greedily() {
        let aspects = vec![1.6; 13];
        let card_h = thumb_card_h_fixed();
        let max_h = THUMB_TOP_INSET + card_h * 3.0 + THUMB_ROW_GAP * 2.0 + status_h() + 0.1;
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
        assert_eq!(layout.visible, 0..13);
    }

    #[test]
    fn scrolling_layout_keeps_balanced_packing_when_everything_fits() {
        let aspects = vec![2.2, 0.7, 0.7];
        let card_h = thumb_card_h_for_scale(thumb_scale_for_count(aspects.len()));
        let max_h = THUMB_TOP_INSET + card_h * 2.0 + THUMB_ROW_GAP + status_h() + 0.1;
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
        let max_h = THUMB_TOP_INSET + card_h * 2.0 + THUMB_ROW_GAP + status_h() + 0.1;
        let layout = plan_thumb_scroll_layout(
            &aspects,
            1288.0,
            1400.0,
            max_h,
            THUMB_ROW_GAP,
            THUMB_SCROLLBAR_W,
            f64::MAX,
        );
        assert_eq!(layout.row_start, 2);
        assert_eq!(layout.visible, 8..20);
    }

    #[test]
    fn scrolling_layout_keeps_fractional_offset_and_renders_partial_row() {
        let aspects = vec![1.6; 20];
        let card_h = thumb_card_h_fixed();
        let row_pitch = card_h + THUMB_ROW_GAP;
        let max_h = THUMB_TOP_INSET + card_h * 2.0 + THUMB_ROW_GAP + status_h() + 0.1;
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

    #[test]
    fn close_reflow_keeps_order_and_fills_the_removed_slot() {
        let widths = vec![100.0; 5];
        let (placements, rows) =
            plan_thumb_close_reflow(&widths, 80.0, 350.0, 220.0, 10.0, 180.0, false);

        assert_eq!(rows, vec![0..2, 2..4, 4..5]);
        assert_eq!(
            placements.iter().map(|p| p.index).collect::<Vec<_>>(),
            (0..5).collect::<Vec<_>>()
        );
        assert_eq!(placements[0].x, placements[2].x);
        assert_eq!(placements[1].x, placements[3].x);
        assert!(placements[2].y < placements[0].y);
    }

    #[test]
    fn overflow_close_reflow_fills_rows_in_window_order() {
        let widths = vec![100.0; 9];
        let (placements, rows) =
            plan_thumb_close_reflow(&widths, 80.0, 430.0, 320.0, 10.0, 260.0, true);

        assert_eq!(rows, vec![0..3, 3..6, 6..9]);
        assert_eq!(placements[3].y, placements[0].y - 90.0);
        assert_eq!(placements[6].y, placements[3].y - 90.0);
    }

    #[test]
    fn close_scroll_rebase_preserves_visible_content_when_document_shrinks() {
        let (document_h, max_offset, offset, delta) =
            rebase_thumb_scroll_after_document_resize(500.0, 400.0, 300.0, 0.0);

        assert_eq!(document_h, 400.0);
        assert_eq!(max_offset, 100.0);
        assert_eq!(offset, 0.0);

        let old_origin = 500.0 - 300.0;
        let old_card_y = 420.0;
        let new_origin = max_offset - offset;
        let new_card_y = old_card_y + delta;
        assert!((old_card_y - old_origin - (new_card_y - new_origin)).abs() < 1e-9);
    }

    #[test]
    fn close_scroll_rebase_preserves_fractional_offset() {
        let (_, max_offset, offset, delta) =
            rebase_thumb_scroll_after_document_resize(700.0, 600.0, 300.0, 180.0);

        assert_eq!(max_offset, 300.0);
        assert_eq!(offset, 180.0);
        assert_eq!(delta, -100.0);
    }

    #[test]
    fn close_scroll_rebase_clamps_offset_after_document_shrink() {
        let (_, max_offset, offset, _) =
            rebase_thumb_scroll_after_document_resize(700.0, 400.0, 300.0, 400.0);

        assert_eq!(max_offset, 100.0);
        assert_eq!(offset, 100.0);
    }
}

#[cfg(test)]
mod icon_scroll_tests {
    use super::*;

    #[test]
    fn icon_layout_balances_rows_when_the_document_fits() {
        let layout = plan_icon_scroll_layout(10, 1200.0, 500.0, 14.0, 0.0);

        assert!(!layout.overflowed);
        assert_eq!(layout.row_ranges, vec![0..5, 5..10]);
        assert_eq!(layout.panel_w, 5.0 * ICON_CARD_W + H_PADDING * 2.0);
    }

    #[test]
    fn icon_cards_keep_fixed_size_and_three_card_baseline() {
        let layout = plan_icon_scroll_layout(2, 1440.0, 900.0, 14.0, 0.0);

        assert_eq!(layout.panel_w, 3.0 * ICON_CARD_W + H_PADDING * 2.0);
        assert_eq!(layout.card_h, card_h());
        assert_eq!(layout.document_placements.len(), 2);
        assert!(layout
            .document_placements
            .iter()
            .all(|placement| placement.width == ICON_CARD_W));
        assert_eq!(layout.max_scroll_offset, 0.0);
    }

    #[test]
    fn icon_layout_auto_columns_and_scrolls_by_rows() {
        let layout = plan_icon_scroll_layout(15, 1200.0, 500.0, 14.0, 0.0);

        assert_eq!(layout.row_ranges, vec![0..7, 7..14, 14..15]);
        assert_eq!(layout.max_rows, 2);
        assert!(layout.overflowed);
        assert_eq!(layout.visible, 0..15);
        assert!(layout.max_scroll_offset > 0.0);
        assert_eq!(layout.document_placements.len(), 15);

        let bottom = plan_icon_scroll_layout(15, 1200.0, 500.0, 14.0, layout.max_scroll_offset);
        assert_eq!(bottom.row_start, 0);
        assert_eq!(bottom.visible, 0..15);
        assert_eq!(bottom.placements.len(), 15);
    }

    #[test]
    fn icon_layout_preserves_card_width_when_screen_is_narrow() {
        let layout = plan_icon_scroll_layout(1, 200.0, 500.0, 14.0, 0.0);

        assert_eq!(layout.document_placements[0].width, ICON_CARD_W);
        assert!(layout.panel_w >= ICON_CARD_W + H_PADDING * 2.0);
    }
}
