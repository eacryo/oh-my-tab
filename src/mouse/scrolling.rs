//! 滚轮滚动模式:默认(透传+可反转)/按行(固定行数)。
//!
//! 方向语义:HID 层 event tap 看到的事件已包含系统"自然滚动"的翻转(自然滚动在 HID 事件
//! 生成时已应用),合成事件 post 到 session 层后不会再被系统翻转。因此方向处理只需按
//! 用户反转开关取反即可(见 should_flip),与 LinearMouse 一致——不读自然滚动设置。
//!
//! Scroll modes: Default (passthrough + optional reverse) / Line (fixed line count).
//!
//! Direction semantics: events seen by the HID-level tap already include the system's natural-scroll
//! flip (applied when HID events are generated), and synthetic events posted to the session layer
//! are not flipped again by the system. Direction handling therefore only needs the user's reverse
//! toggle (see should_flip), matching LinearMouse -- no natural-scroll setting is read.

// ========== 方向处理 / direction handling ==========

/// 是否应对滚动 delta 取反:直接取用户反转开关。
/// HID tap 看到的事件已含系统自然滚动翻转,合成事件不再被翻转,所以:
/// - 反转关 -> 不取反(透传系统方向,含自然滚动)
/// - 反转开 -> 取反(相对系统的反转)
///
/// Whether to flip the scroll delta: directly the user's reverse toggle.
/// HID-tap events already carry the system natural-scroll flip and synthetic events aren't flipped
/// again, so:
/// - reverse off -> no flip (passthrough, including natural scrolling)
/// - reverse on  -> flip (reversed relative to the system)
pub(crate) fn should_flip(user_reverse: bool) -> bool {
    user_reverse
}

// ========== 滚动模式 / Scroll mode ==========

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ScrollMode {
    Default,
    Line,
}

impl ScrollMode {
    pub(crate) fn from_str(s: &str) -> Self {
        match s {
            "line" => Self::Line,
            _ => Self::Default,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Line => "line",
        }
    }

    #[allow(dead_code)]
    pub(crate) fn current() -> Self {
        // 兼容旧路径(无设备上下文):用"所有鼠标"解析。
        // Legacy path (no device context): resolve with the "All Mice" profile.
        let r = crate::mouse::resolve::resolve(None);
        r.scroll_mode
    }

    #[allow(dead_code)]
    pub(crate) fn all_labels() -> &'static [&'static str] {
        &["default", "line"]
    }
}

/// 根据解析后的配置计算要 post 的滚动 delta。
/// 处理反转 + 行模式的行数归一化。结果形参供 post_scroll_event 使用。
///
/// Compute the scroll delta to post from the resolved config.
/// Handles reversal + line-mode normalization.
pub(crate) fn compute_delta(
    dy: i64,
    dx: i64,
    r: &crate::mouse::resolve::ResolvedMouse,
) -> (i32, i32) {
    let mode = r.scroll_mode;
    let flip = should_flip(r.reverse_scroll);

    let (mut ndy, mut ndx) = match mode {
        ScrollMode::Default => (dy as i32, dx as i32),
        ScrollMode::Line => {
            let line_count = r.line_count.clamp(1, 10) as i64;
            let sign_y = if dy != 0 { dy.signum() } else { 0 };
            let sign_x = if dx != 0 { dx.signum() } else { 0 };
            ((sign_y * line_count) as i32, (sign_x * line_count) as i32)
        }
    };

    if flip {
        ndy = -ndy;
        ndx = -ndx;
    }

    (ndy, ndx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mouse::resolve::ResolvedMouse;

    #[test]
    fn flip_equals_user_reverse() {
        // 反转关 -> 不取反(透传系统方向,含自然滚动)。
        assert!(!should_flip(false));
        // 反转开 -> 取反(相对系统的反转)。
        assert!(should_flip(true));
    }

    fn resolved(mode: ScrollMode, reverse: bool, line_count: u32) -> ResolvedMouse {
        ResolvedMouse {
            reverse_scroll: reverse,
            scroll_mode: mode,
            line_count,
            disable_acceleration: false,
            button_mappings: std::collections::HashMap::new(),
            button_mappings_enabled: true,
        }
    }

    #[test]
    fn default_mode_passes_delta_through() {
        // 透传模式:原样返回(方向由 reverse 决定)。
        // Passthrough mode: delta returned verbatim (direction handled by reverse).
        let r = resolved(ScrollMode::Default, false, 3);
        assert_eq!(compute_delta(10, -5, &r), (10, -5));
        assert_eq!(compute_delta(0, 0, &r), (0, 0));
    }

    #[test]
    fn default_mode_with_reverse_flips_both_axes() {
        let r = resolved(ScrollMode::Default, true, 3);
        assert_eq!(compute_delta(10, -5, &r), (-10, 5));
    }

    #[test]
    fn line_mode_normalizes_by_sign() {
        // 行模式:任何幅度都归一化为 ±line_count,0 保持 0。
        // Line mode: any magnitude normalizes to ±line_count; zero stays zero.
        let r = resolved(ScrollMode::Line, false, 3);
        assert_eq!(compute_delta(1000, -1, &r), (3, -3));
        assert_eq!(compute_delta(0, 0, &r), (0, 0));
    }

    #[test]
    fn line_mode_line_count_is_clamped() {
        // 行数被 clamp 到 1..=10(配置层已校验,这里兜底)。
        // Line count clamps to 1..=10 (validated at the config layer; belt-and-braces here).
        let r = resolved(ScrollMode::Line, false, 0);
        assert_eq!(compute_delta(5, 0, &r), (1, 0));
        let r = resolved(ScrollMode::Line, false, 99);
        assert_eq!(compute_delta(5, 0, &r), (10, 0));
    }

    #[test]
    fn line_mode_with_reverse_flips_signs() {
        let r = resolved(ScrollMode::Line, true, 4);
        assert_eq!(compute_delta(100, 200, &r), (-4, -4));
    }

    #[test]
    fn scroll_mode_from_str_falls_back_to_default() {
        // 未知字符串回退 Default(配置校验后不应出现)。
        // Unknown strings fall back to Default (shouldn't happen after config validation).
        assert_eq!(ScrollMode::from_str("line"), ScrollMode::Line);
        assert_eq!(ScrollMode::from_str("default"), ScrollMode::Default);
        assert_eq!(ScrollMode::from_str("turbo"), ScrollMode::Default);
        assert_eq!(ScrollMode::from_str(""), ScrollMode::Default);
        assert_eq!(ScrollMode::as_str(&ScrollMode::Line), "line");
        assert_eq!(ScrollMode::as_str(&ScrollMode::Default), "default");
    }
}
