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
    use super::should_flip;

    #[test]
    fn flip_equals_user_reverse() {
        // 反转关 -> 不取反(透传系统方向,含自然滚动)。
        assert!(!should_flip(false));
        // 反转开 -> 取反(相对系统的反转)。
        assert!(should_flip(true));
    }
}
