//! 主题与布局:从 CONFIG 派生的配色(Colors)、明暗模式检测、以及卡片/窗口尺寸访问器。
//! 被 overlay 等模块依赖。
//!
//! Theme and layout: config-derived colors (Colors), dark-mode detection, and card/window
//! size accessors. Depended on by overlay and other modules.

use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use std::ffi::c_void;

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
/// 卡片内边距(设计稿 .item padding 8px)。/ Card inner padding (.item padding 8px).
pub(crate) const THUMB_PAD: f64 = 8.0;
/// 标题行高(设计稿 .caption 34px 含 7px 底距,取净高 24)。/ Caption row height.
pub(crate) const THUMB_CAPTION_H: f64 = 24.0;
/// 标题行与预览区的间距。/ Gap between caption and preview.
pub(crate) const THUMB_GAP: f64 = 6.0;
/// 预览区宽高比(设计稿 aspect-ratio 16/10)。/ Preview aspect ratio (16/10).
pub(crate) const THUMB_PREVIEW_RATIO: f64 = 1.6;

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
pub(crate) fn thumb_card_h_fixed() -> f64 {
    thumb_card_h(THUMB_CARD_BASE_W)
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

/// 贪心行装箱:按给定顺序把卡片塞进行,放不下就换行(纯函数,可测)。
/// 返回每行的索引列表(保持输入顺序)。
///
/// Greedy row packing: fill rows in the given order, wrap when the next card
/// would overflow (pure, testable). Returns per-row index lists preserving the
/// input order.
pub(crate) fn pack_rows(widths: &[f64], max_inner_w: f64, gap: f64) -> Vec<Vec<usize>> {
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    let mut cur_w = 0.0;
    for (i, &w) in widths.iter().enumerate() {
        let needed = if cur.is_empty() { w } else { cur_w + gap + w };
        if !cur.is_empty() && needed > max_inner_w {
            rows.push(std::mem::take(&mut cur));
            cur_w = w;
            cur.push(i);
        } else {
            cur_w = needed;
            cur.push(i);
        }
    }
    if !cur.is_empty() {
        rows.push(cur);
    }
    if rows.is_empty() {
        rows.push(Vec::new());
    }
    rows
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
}
