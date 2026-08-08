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

/// 浮窗高度 = 顶部 32 + 行数 * 卡片高 + 状态栏高(纯函数,可测)。
/// Overlay height = top 32 + rows * card height + status bar height (pure, testable).
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
