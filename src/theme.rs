//! Theme and layout: config-derived colors (Colors), dark-mode detection, and card/window
//! size accessors. Depended on by overlay and other modules.

use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use std::ffi::c_void;
use std::ops::Range;

use crate::config::{self, CONFIG};
use crate::ffi::{make_nsstring, CFRelease};

pub(crate) const STATUS_H: f64 = 36.0;
/// horizontal padding inside the window
pub(crate) const H_PADDING: f64 = 32.0;
/// Maximum overlay width as a share of the screen; it still shrinks to the
/// natural content size when there is room.
pub(crate) const PANEL_MAX_WIDTH_RATIO: f64 = 0.92;
/// Overlay height uses the target screen's complete visible area; it still shrinks to the natural
/// content size when there is room.
/// Note: at 1.0 the panel may fill the visible area, covering an auto-hidden Dock (placement is
/// clamped by clamp_into_visible in cards.rs, so it never crosses the menu bar).
pub(crate) const PANEL_MAX_HEIGHT_RATIO: f64 = 1.0;
/// Base icon-only card dimensions; width stays fixed while height reserves room for larger content.
pub(crate) const ICON_CARD_W: f64 = 140.0;
pub(crate) const ICON_CARD_H: f64 = 180.0;
pub(crate) const ICON_CARD_GAP: f64 = 0.0;
pub(crate) const ICON_SIZE: f64 = 110.0;
pub(crate) const CARD_TEXT_BASE_SIZE: f64 = 12.0;
pub(crate) const CARD_TEXT_SIZE_MIN: f64 = 13.0;
pub(crate) const CARD_TEXT_SIZE_MAX: f64 = 20.0;
const STATUS_BAR_TEXT_BASE_SIZE: f64 = 13.0;

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

/// Settings and auxiliary panels use custom layer-backed surfaces, so they need a complete
/// palette instead of relying on AppKit semantic colors for only part of the hierarchy.
#[derive(Clone, Copy)]
pub(crate) struct UiPalette {
    pub(crate) dark: bool,
    pub(crate) window_bg: u32,
    pub(crate) sidebar_bg: u32,
    pub(crate) detail_bg: u32,
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
    pub(crate) destructive: u32,
    pub(crate) destructive_hover: u32,
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
            destructive: 0xFF453AFF,
            destructive_hover: 0xD93630FF,
            shadow: 0x00000042,
        }
    } else {
        UiPalette {
            dark: false,
            // Match the light reference surfaces: #f6f7f9 window/detail, #f1f2f4 sidebar,
            // and rgba(255,255,255,.82) grouped settings rows/cards.
            window_bg: 0xF6F7F9FF,
            sidebar_bg: 0xF1F2F4FF,
            detail_bg: 0xF6F7F9FF,
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
            selection_bg: 0x0A84FF16,
            hover_bg: 0x76768024,
            accent: 0x0A84FFFF,
            accent_hover: 0x0077EDFF,
            destructive: 0xFF3B30FF,
            destructive_hover: 0xD70015FF,
            shadow: 0x0000000A,
        }
    }
}

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
    }
}

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
pub(crate) fn resolved_is_dark() -> bool {
    match CONFIG.read().unwrap().appearance.theme.as_str() {
        "light" => false,
        "dark" => true,
        _ => system_dark_mode(),
    }
}

/// Resolve current colors per CONFIG.appearance.theme (auto follows system dark/light).
pub(crate) fn current_colors() -> Colors {
    colors_from_config(resolved_is_dark())
}

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

/// Window-thumbnail master switch (the thumbnail module additionally sleeps
/// without the Screen Recording permission).
pub(crate) fn thumbnails_enabled() -> bool {
    CONFIG.read().unwrap().layout.thumbnails_enabled
}

/// Whether the thumbnail card's caption prefixes the app name before the window title.
pub(crate) fn show_app_name_in_cards() -> bool {
    CONFIG.read().unwrap().layout.show_app_name_in_cards
}

/// Flow-layout base card width, hard-coded and fully independent of the legacy
/// icon grid's card_width: aligned with the references (BetterCmdTab effective
/// 312 / DockDoor 300 / mockup ~295).
pub(crate) const THUMB_CARD_BASE_W: f64 = 300.0;
/// Flow-layout row/inter-card gap (the mockup's .grid gap of 14px).
pub(crate) const THUMB_ROW_GAP: f64 = 14.0;
/// Width reserved for the scrollbar at the right edge of the scrolling thumbnail viewport.
pub(crate) const THUMB_SCROLLBAR_W: f64 = 14.0;
/// Fraction of the next row exposed at the bottom of an overflowing viewport.
pub(crate) const THUMB_SCROLL_TEASER_RATIO: f64 = 1.0 / 3.0;
/// Card inner padding (.item padding 8px).
pub(crate) const THUMB_PAD: f64 = 8.0;
/// Caption row height.
pub(crate) const THUMB_CAPTION_H: f64 = 24.0;
/// Gap between the caption icon and window title.
pub(crate) const THUMB_CAPTION_ICON_GAP: f64 = 6.0;
/// Gap between caption and preview.
pub(crate) const THUMB_GAP: f64 = 6.0;
/// Preview aspect ratio (16/10).
pub(crate) const THUMB_PREVIEW_RATIO: f64 = 1.6;
/// Maximum card enlargement for small window sets; 1.0 is the original thumbnail size.
pub(crate) const THUMB_MAX_SCALE: f64 = 1.2;
/// The floor of the card scale (relative to the base card width). It doubles as the last ladder step:
/// `thumb_card_h_for_scale` clamps its input with it, so adding a smaller step requires lowering
/// this too or the new step can never be chosen.
pub(crate) const THUMB_MIN_SCALE: f64 = 0.75;
/// Minimum blank strip between the overlay and the screen (or the menu-bar/Dock reservations) above
/// and below. It is subtracted from the height budget: the panel's height comes from the content
/// rows, so moving it alone would push it across the menu bar instead of making it shorter.
pub(crate) const PANEL_MARGIN: f64 = 24.0;
/// Top inset above the thumbnail card area.
const THUMB_TOP_INSET: f64 = 32.0;

/// The legacy paged layout stays frozen at its historical 1.5–0.85 range, decoupled from the
/// production ladder. It has no caller, so keeping these values lets its layout tests describe the
/// old behaviour without changing with every ladder tweak.
#[cfg(test)]
const LEGACY_THUMB_MAX_SCALE: f64 = 1.5;

#[cfg(test)]
const LEGACY_THUMB_MIN_SCALE: f64 = 0.85;

/// Thumbnail scale depends only on the total window count; wrapping and window
/// aspect ratios must not feed back into card size.
///
/// Still used by the legacy paged layout (`plan_thumb_flow_layout`, which no longer has a
/// production caller); the production path picks its step with `thumb_scale_for_panel`. Both
/// exist only for tests now.
#[cfg(test)]
pub(crate) fn thumb_scale_for_count(count: usize) -> f64 {
    match count {
        0 => 1.0,
        7.. => LEGACY_THUMB_MIN_SCALE,
        1 | 2 => LEGACY_THUMB_MAX_SCALE,
        3 => 1.4,
        4 => 1.3,
        5 => 1.2,
        6 => 1.1,
    }
}

/// Candidate card scales, largest first, from the maximum enlargement to the minimum floor.
pub(crate) const THUMB_SCALE_STEPS: [f64; 8] =
    [THUMB_MAX_SCALE, 1.1, 1.0, 0.95, 0.9, 0.85, 0.8, 0.75];

/// Pick the packing to use: balanced rows win (even counts per row) and greedy is only the fallback
/// when balanced exceeds the visible row budget (greedy needs the fewest rows, so it never shrinks
/// for nothing).
fn choose_thumb_rows(
    balanced: Vec<Vec<usize>>,
    greedy: Vec<Vec<usize>>,
    max_rows: usize,
) -> Vec<Vec<usize>> {
    if balanced.len() <= max_rows {
        balanced
    } else {
        greedy
    }
}

/// Pick the card step from the **available panel**: the largest candidate that fits, falling back to
/// the smallest step (scrolling) when nothing fits.
///
/// The trial run uses the **real window aspects and the real card-width cap**, i.e. exactly the
/// inputs of the final packing. That makes "the chosen step fits" a guarantee instead of an
/// estimate, and avoids leaving space unused because widths were assumed to be the base aspect.
/// The price is that card size follows window shapes (dragging one window very wide can drop the
/// whole set a step), which is what using the space requires.
pub(crate) fn thumb_scale_for_panel(
    aspects: &[f64],
    max_inner: f64,
    max_panel_w: f64,
    max_panel_h: f64,
    gap: f64,
    scrollbar_w: f64,
    max_card_w_for: &impl Fn(f64) -> f64,
) -> f64 {
    if aspects.is_empty() {
        return 1.0;
    }
    for &scale in THUMB_SCALE_STEPS.iter() {
        let card_h = thumb_card_h_for_scale(scale);
        let fits = !plan_thumb_scroll_layout_at_scale(
            aspects,
            scale,
            max_inner,
            max_panel_w,
            max_panel_h,
            gap,
            scrollbar_w,
            0.0,
            max_card_w_for(card_h),
        )
        .overflowed;
        if fits {
            return scale;
        }
    }
    THUMB_MIN_SCALE
}

/// Thumbnail card height derives from the base width per the mockup (pure,
/// testable): vertical paddings + caption + gap + a 16:10 preview.
fn thumb_card_h(card_width: f64) -> f64 {
    let preview_h = (card_width - THUMB_PAD * 2.0) / THUMB_PREVIEW_RATIO;
    THUMB_PAD * 2.0 + thumb_caption_h() + THUMB_GAP + preview_h
}

/// The flow layout's uniform card height: derived once from the base width;
/// every card shares it.
#[cfg(test)]
pub(crate) fn thumb_card_h_fixed() -> f64 {
    thumb_card_h(THUMB_CARD_BASE_W)
}

/// Scale is based on the base card width; caption and padding stay fixed instead
/// of mechanically enlarging text and controls.
pub(crate) fn thumb_card_h_for_scale(scale: f64) -> f64 {
    thumb_card_h(THUMB_CARD_BASE_W * scale.max(THUMB_MIN_SCALE))
}

/// Preview height = card height - vertical paddings - caption - gap (pure, testable).
pub(crate) fn thumb_preview_h(card_h: f64) -> f64 {
    (card_h - THUMB_PAD * 2.0 - thumb_caption_h() - THUMB_GAP).max(40.0)
}

/// Aspect clamp: windows can be extremely wide/tall; unclamped cards would become
/// unrecognizably narrow or hog an entire row.
pub(crate) fn clamp_aspect(aspect: f64) -> f64 {
    if !aspect.is_finite() || aspect <= 0.0 {
        return THUMB_PREVIEW_RATIO; // degenerate -> 16:10
    }
    aspect.clamp(0.7, 2.2)
}

/// Flow-layout card width = preview height × aspect + side paddings (pure, testable).
pub(crate) fn thumb_card_w_for_aspect(card_h: f64, aspect: f64) -> f64 {
    thumb_preview_h(card_h) * clamp_aspect(aspect) + THUMB_PAD * 2.0
}

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
    /// Stable coordinates for every card in the complete document; scrolling never rewrites them.
    pub(crate) document_placements: Vec<ThumbPlacement>,
    /// Document height excluding the status bar, matching the NSClipView document view height.
    pub(crate) document_h: f64,
    pub(crate) scale: f64,
    pub(crate) visible: Range<usize>,
    pub(crate) placements: Vec<ThumbPlacement>,
    /// Content inset; the post-close reflow reuses it so cards do not jump.
    pub(crate) content_inset: f64,
    pub(crate) overflowed: bool,
    pub(crate) page_index: usize,
    pub(crate) page_count: usize,
    /// Complete flow-layout row ranges; scrolling mode moves the viewport by these stable rows.
    pub(crate) row_ranges: Vec<Range<usize>>,
    pub(crate) row_start: usize,
    pub(crate) max_rows: usize,
    pub(crate) max_scroll_offset: f64,
}

#[cfg(test)]
fn thumb_widths(aspects: &[f64], range: &Range<usize>, card_h: f64, max_inner: f64) -> Vec<f64> {
    thumb_widths_with_max_card_w(aspects, range, card_h, max_inner, f64::INFINITY)
}

fn thumb_widths_with_max_card_w(
    aspects: &[f64],
    range: &Range<usize>,
    card_h: f64,
    max_inner: f64,
    max_card_w: f64,
) -> Vec<f64> {
    let max_card_w = max_card_w.max(1.0);
    aspects[range.clone()]
        .iter()
        // Clamp a single card on exceptionally narrow screens; the image remains
        // fully visible via aspect-fit and merely gains letterboxing.
        .map(|&aspect| {
            thumb_card_w_for_aspect(card_h, aspect)
                .min(max_inner)
                .min(max_card_w)
        })
        .collect()
}

fn thumb_max_rows(card_h: f64, max_panel_h: f64, gap: f64) -> usize {
    let available = (max_panel_h - THUMB_TOP_INSET - status_h()).max(card_h);
    ((available + gap) / (card_h + gap)).floor().max(1.0) as usize
}

/// Rows an overflowing viewport can show: the bottom teaser (one gap plus a third of a card) comes
/// out of the budget before whole rows are counted. Without this the overflow panel is a teaser
/// taller than the budget and eats the top/bottom margins (measured 2026-09-24: budget 875, panel
/// 921, margins down to 33/2).
fn thumb_viewport_rows(card_h: f64, max_panel_h: f64, gap: f64) -> usize {
    let teaser_h = gap + card_h * THUMB_SCROLL_TEASER_RATIO;
    let available = (max_panel_h - THUMB_TOP_INSET - status_h() - teaser_h).max(card_h);
    ((available + gap) / (card_h + gap)).floor().max(1.0) as usize
}

/// Whether the teaser fits without costing a whole row. If it does it is drawn (a sliver of the next
/// row peeks at the bottom); if not it is dropped in favour of one more full row.
pub(crate) fn thumb_teaser_fits(card_h: f64, max_panel_h: f64, gap: f64) -> bool {
    thumb_viewport_rows(card_h, max_panel_h, gap) == thumb_max_rows(card_h, max_panel_h, gap)
}

#[derive(Clone, Copy)]
struct ThumbFlowConstraints {
    card_h: f64,
    max_inner: f64,
    max_rows: usize,
    /// Whether the teaser fits without costing a whole row; if not it is dropped.
    teaser_fits: bool,
    /// Height budget (hard cap): the panel height still comes from the cards, it just may not exceed
    /// this.
    max_panel_h: f64,
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
        content_inset: THUMB_TOP_INSET,
        overflowed,
        page_index,
        page_count,
        row_ranges,
        row_start: 0,
        max_rows: constraints.max_rows,
        max_scroll_offset: 0.0,
    }
}

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
    // Rows first: show as many whole rows as fit, and only count the teaser when it does not push a
    // row out.
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
    let teaser_h = thumb_teaser_height(
        overflowed,
        constraints.teaser_fits,
        constraints.card_h,
        constraints.gap,
    );
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
    let (panel_h, content_inset) = thumb_panel_metrics(
        rendered_rows,
        constraints.card_h,
        constraints.gap,
        teaser_h,
        constraints.max_panel_h,
    );
    let card_area_w = if overflowed {
        (panel_w - scrollbar_w).max(1.0)
    } else {
        panel_w
    };
    // Center the card grid together with the right-hand scrollbar. Centering only within
    // the remaining area shifts the whole visual group left by half the scrollbar width.
    let scrollbar_centering_offset = if overflowed { scrollbar_w / 2.0 } else { 0.0 };
    let document_h = content_inset
        + all_rows.len() as f64 * constraints.card_h
        + all_rows.len().saturating_sub(1) as f64 * constraints.gap;
    let document_panel_h = document_h + status_h();
    let mut document_placements = Vec::new();
    for (row_index, row) in all_rows.iter().enumerate() {
        let row_w = row.iter().map(|&i| widths[i]).sum::<f64>()
            + row.len().saturating_sub(1) as f64 * constraints.gap;
        let mut x = (card_area_w - row_w) / 2.0 + scrollbar_centering_offset;
        let y = document_panel_h
            - content_inset
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
            - content_inset
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
        content_inset: THUMB_TOP_INSET,
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

/// Plan in-place coordinates after closing one card while keeping the current card size and
/// document top anchor stable during the transition.
#[allow(clippy::too_many_arguments)]
pub(crate) fn plan_thumb_close_reflow(
    widths: &[f64],
    card_h: f64,
    max_inner: f64,
    gap: f64,
    document_h: f64,
    content_inset: f64,
    overflowed: bool,
    max_rows: usize,
) -> (Vec<ThumbPlacement>, Vec<Range<usize>>, f64, bool) {
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
    let used_inner_w = rows
        .iter()
        .map(|row| {
            row.iter().map(|&index| widths[index]).sum::<f64>()
                + row.len().saturating_sub(1) as f64 * gap
        })
        .fold(0.0f64, f64::max);
    let final_overflowed = row_ranges.len() > max_rows.max(1);
    let final_scrollbar_w = if final_overflowed {
        THUMB_SCROLLBAR_W
    } else {
        0.0
    };
    let final_panel_w = if final_overflowed {
        used_inner_w + H_PADDING * 2.0 + final_scrollbar_w
    } else {
        used_inner_w.max(280.0_f64.min(max_inner)) + H_PADDING * 2.0
    };
    let final_card_area_w = (final_panel_w - final_scrollbar_w).max(1.0);
    let document_h = document_h.max(1.0);
    let mut placements = Vec::with_capacity(widths.len());
    for (row_index, row) in rows.iter().enumerate() {
        if row.is_empty() {
            continue;
        }
        let row_w = row.iter().map(|&index| widths[index]).sum::<f64>()
            + row.len().saturating_sub(1) as f64 * gap;
        let mut x = (final_card_area_w - row_w) / 2.0
            + if final_overflowed {
                final_scrollbar_w / 2.0
            } else {
                0.0
            };
        let y =
            document_h - content_inset - (row_index as f64 + 1.0) * card_h - row_index as f64 * gap;
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
    (placements, row_ranges, final_panel_w, final_overflowed)
}

/// Compute the post-close document height; it always covers the current clip view so shrinking
/// content never leaves the clip view with an invalid scroll range.
pub(crate) fn thumb_document_height_for_rows(
    row_count: usize,
    card_h: f64,
    gap: f64,
    content_inset: f64,
) -> f64 {
    content_inset + row_count.max(1) as f64 * card_h + row_count.saturating_sub(1) as f64 * gap
}

/// Height of the bottom teaser when overflowing; zero unless `overflowed && teaser_fits`.
pub(crate) fn thumb_teaser_height(
    overflowed: bool,
    teaser_fits: bool,
    card_h: f64,
    gap: f64,
) -> f64 {
    if overflowed && teaser_fits {
        gap + card_h * THUMB_SCROLL_TEASER_RATIO
    } else {
        0.0
    }
}

/// Rows + teaser + status bar -> (panel height, content inset). The panel height *is* the content
/// height, decided by the cards themselves (rows, card height, gap, teaser, status bar); `max_panel_h`
/// is only a hard cap. Stretching the panel to the cap from three rows up used to decouple it from the
/// cards and produced an extra blank band at the top (measured). The normal layout and the post-close
/// reflow must share this function, or closing a card makes the panel jump taller (measured 901 >
/// 875).
pub(crate) fn thumb_panel_metrics(
    rendered_rows: usize,
    card_h: f64,
    gap: f64,
    teaser_h: f64,
    max_panel_h: f64,
) -> (f64, f64) {
    let rows = rendered_rows.max(1);
    let content_h = THUMB_TOP_INSET
        + rows as f64 * card_h
        + rows.saturating_sub(1) as f64 * gap
        + teaser_h
        + status_h();
    // Step selection and the viewport row count already keep the content inside the budget (see
    // thumb_max_rows / thumb_viewport_rows). Clamping again is a backstop that only kicks in when the
    // budget is too small for even one row plus the teaser (max_rows floors at 1); showing a little
    // less beats crossing the cap the user asked for.
    (content_h.min(max_panel_h), THUMB_TOP_INSET)
}

/// (Panel height, content inset) after a close reflow: the viewport row count follows the same
/// "rows first, teaser only when free" rule and the height goes through the `thumb_panel_metrics`
/// the normal layout shares. `plan_thumb_close_reflow` only produces rows and coordinates; routing
/// the height through here is what stops a close from jumping the panel taller (measured 901 > 875).
pub(crate) fn thumb_close_panel_metrics(
    row_count: usize,
    overflowed: bool,
    card_h: f64,
    gap: f64,
    teaser_fits: bool,
    max_rows: usize,
    max_panel_h: f64,
) -> (f64, f64) {
    let teaser_h = thumb_teaser_height(overflowed, teaser_fits, card_h, gap);
    let rows = if overflowed {
        row_count.min(max_rows.max(1))
    } else {
        row_count
    };
    thumb_panel_metrics(rows, card_h, gap, teaser_h, max_panel_h)
}

/// Content height for exactly N rows (plus an optional teaser plus the status bar). Production goes
/// through `thumb_panel_metrics`; tests still use this form to build an "exactly N rows" budget.
#[cfg(test)]
pub(crate) fn thumb_panel_height_for_rows(
    row_count: usize,
    viewport_rows: usize,
    card_h: f64,
    gap: f64,
    overflowed: bool,
) -> f64 {
    let visible_rows = if overflowed {
        viewport_rows.max(1)
    } else {
        row_count.max(1)
    };
    let teaser_h = if overflowed {
        gap + card_h * THUMB_SCROLL_TEASER_RATIO
    } else {
        0.0
    };
    THUMB_TOP_INSET
        + visible_rows as f64 * card_h
        + visible_rows.saturating_sub(1) as f64 * gap
        + teaser_h
        + status_h()
}

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

/// Plan the thumbnail grid: total window count first determines the 1.0–1.2 scale,
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
        teaser_fits: thumb_teaser_fits(card_h, max_panel_h, gap),
        max_panel_h,
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

/// Plan the continuous thumbnail viewport: the card step comes from the available panel (see
/// `thumb_scale_for_panel`) and the per-card width cap is derived from that step, so callers never
/// have to guess a card height up front.
#[allow(clippy::too_many_arguments)]
pub(crate) fn plan_thumb_scroll_layout_with_max_card_w(
    aspects: &[f64],
    max_inner: f64,
    max_panel_w: f64,
    max_panel_h: f64,
    gap: f64,
    scrollbar_w: f64,
    scroll_offset: f64,
    max_card_w_for: impl Fn(f64) -> f64,
) -> ThumbFlowLayout {
    let max_inner = max_inner.max(1.0);
    let scale = thumb_scale_for_panel(
        aspects,
        max_inner,
        max_panel_w,
        max_panel_h,
        gap,
        scrollbar_w,
        &max_card_w_for,
    );
    plan_thumb_scroll_layout_at_scale(
        aspects,
        scale,
        max_inner,
        max_panel_w,
        max_panel_h,
        gap,
        scrollbar_w,
        scroll_offset,
        max_card_w_for(thumb_card_h_for_scale(scale)),
    )
}

/// Actual packing and viewport planning at one fixed step (step selection and packing are split so
/// candidate steps can be tried with pure geometry).
#[allow(clippy::too_many_arguments)]
fn plan_thumb_scroll_layout_at_scale(
    aspects: &[f64],
    scale: f64,
    max_inner: f64,
    max_panel_w: f64,
    max_panel_h: f64,
    gap: f64,
    scrollbar_w: f64,
    scroll_offset: f64,
    max_card_w: f64,
) -> ThumbFlowLayout {
    let max_inner = max_inner.max(1.0);
    let card_h = thumb_card_h_for_scale(scale);
    let constraints = ThumbFlowConstraints {
        card_h,
        max_inner,
        max_rows: thumb_max_rows(card_h, max_panel_h, gap),
        teaser_fits: thumb_teaser_fits(card_h, max_panel_h, gap),
        max_panel_h,
        gap,
    };
    let widths =
        thumb_widths_with_max_card_w(aspects, &(0..aspects.len()), card_h, max_inner, max_card_w);
    let rows = choose_thumb_rows(
        pack_rows(&widths, max_inner, gap),
        pack_rows_greedy(&widths, max_inner, gap),
        constraints.max_rows,
    );
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
        content_inset: THUMB_TOP_INSET,
        overflowed,
        page_index: 0,
        page_count: 1,
        row_ranges,
        row_start,
        max_rows: viewport_rows,
        max_scroll_offset,
    }
}

/// Initial icon-only overlay height = top 32 + rows * card height + status bar (pure, testable).
/// Used only for the initial window placeholder; summon-time layout recalculates it.
fn compute_window_height(count: usize, cards_per_row: usize, card_h: f64) -> f64 {
    let rows = count.max(1).div_ceil(cards_per_row);
    32.0 + rows as f64 * card_h + status_h()
}

/// Initial icon-only overlay height; the live layout computes columns from screen width.
pub(crate) fn window_height(count: usize) -> f64 {
    compute_window_height(count, 6, card_h())
}

/// Overlay width = cards * fixed card width + gaps + padding on both sides (pure, testable).
/// Floor is one card's width: the overlay must never degenerate into a thin strip.
fn compute_window_width(cards_in_row: usize, card_w: f64, card_gap: f64) -> f64 {
    let n = cards_in_row.max(1);
    n as f64 * card_w + (n - 1) as f64 * card_gap + H_PADDING * 2.0
}

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
        // Zero or one card: width = one card + both paddings (no gap).
        let w0 = compute_window_width(0, 100.0, 20.0);
        let w1 = compute_window_width(1, 100.0, 20.0);
        assert_eq!(w0, 100.0 + H_PADDING * 2.0);
        assert_eq!(w1, w0);
        // Three cards: 3 * card_w + 2 * gaps + padding.
        assert_eq!(
            compute_window_width(3, 100.0, 20.0),
            300.0 + 40.0 + H_PADDING * 2.0
        );
        // Even with a zero card width the floor keeps it non-degenerate.
        assert!(compute_window_width(0, 0.0, 0.0) > 0.0);
    }
}

#[cfg(test)]
mod flow_tests {
    use super::*;
    /// Test helper: an aspect vector of n base-ratio (16:10) windows.
    fn uniform_aspects(n: usize) -> Vec<f64> {
        vec![THUMB_PREVIEW_RATIO; n]
    }

    #[test]
    fn thumb_heights_derive_consistently() {
        // The uniform height derives from the configured card width; preview height
        // subtracts paddings + caption + gap.
        let h = thumb_card_h_fixed();
        assert!(h > 100.0, "sanity: {} too small", h);
        let ph = thumb_preview_h(h);
        assert!((h - (ph + THUMB_PAD * 2.0 + thumb_caption_h() + THUMB_GAP)).abs() < 1e-9);
        // The 40pt floor keeps extreme configs from collapsing.
        assert!(thumb_preview_h(50.0) >= 40.0);
    }

    #[test]
    fn card_width_scales_with_aspect_and_clamps() {
        let h = 160.0;
        let ph = thumb_preview_h(h);
        // A 16:10 window: preview width = preview height × 1.6; card adds side pads.
        let w = thumb_card_w_for_aspect(h, 1.6);
        assert!((w - (ph * 1.6 + THUMB_PAD * 2.0)).abs() < 1e-9);
        // Extreme aspects clamp: portrait >= 0.7, landscape <= 2.2.
        let tall = thumb_card_w_for_aspect(h, 0.2);
        assert!((tall - (ph * 0.7 + THUMB_PAD * 2.0)).abs() < 1e-9);
        let wide = thumb_card_w_for_aspect(h, 5.0);
        assert!((wide - (ph * 2.2 + THUMB_PAD * 2.0)).abs() < 1e-9);
        // Degenerate inputs (0 / NaN) fall back to 16:10.
        let deg = thumb_card_w_for_aspect(h, f64::NAN);
        assert!((deg - (ph * THUMB_PREVIEW_RATIO + THUMB_PAD * 2.0)).abs() < 1e-9);
    }

    #[test]
    fn pack_rows_wraps_on_overflow_and_keeps_order() {
        // Budget 250, widths [100x4]: two per row (210; a third would overflow).
        let rows = pack_rows(&[100.0, 100.0, 100.0, 100.0], 250.0, 10.0);
        assert_eq!(rows, vec![vec![0, 1], vec![2, 3]]);
        // An oversized single card gets its own row; order is preserved.
        let rows = pack_rows(&[50.0, 400.0, 50.0], 250.0, 10.0);
        assert_eq!(rows, vec![vec![0], vec![1], vec![2]]);
        // Empty input yields one empty row (the empty-state floor).
        assert_eq!(pack_rows(&[], 250.0, 10.0), vec![Vec::<usize>::new()]);
    }

    #[test]
    fn pack_rows_balances_mixed_widths_without_reordering() {
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
        assert_eq!(thumb_scale_for_count(7), LEGACY_THUMB_MIN_SCALE);
        assert_eq!(thumb_scale_for_count(30), LEGACY_THUMB_MIN_SCALE);
    }

    /// The ladder is the only input that decides card size: this test pins the floor, the cap and their
    /// geometric consequences, so a ladder tweak only ever touches this test.
    #[test]
    fn the_scale_ladder_keeps_its_floor_and_cap() {
        assert_eq!(THUMB_SCALE_STEPS[0], THUMB_MAX_SCALE);
        assert_eq!(THUMB_SCALE_STEPS.last().copied().unwrap(), THUMB_MIN_SCALE);
        assert!(THUMB_SCALE_STEPS
            .iter()
            .all(|&scale| scale <= THUMB_MAX_SCALE));
        // The floor must actually be smaller (otherwise it shrinks for nothing) while the preview area
        // stays above its legibility floor.
        const { assert!(LEGACY_THUMB_MIN_SCALE < 1.0) };
        let floor_card_h = thumb_card_h_for_scale(LEGACY_THUMB_MIN_SCALE);
        assert!(floor_card_h < thumb_card_h_for_scale(1.0));
        assert!(thumb_preview_h(floor_card_h) > 40.0);
    }

    #[test]
    fn card_step_follows_the_panel_not_the_window_count() {
        // The panel budget on the measured 1920x1080 screen: 92% width minus padding/scrollbar, full
        // visible height.
        let panel_w = 1920.0 * PANEL_MAX_WIDTH_RATIO;
        let inner = panel_w - H_PADDING * 2.0 - THUMB_SCROLLBAR_W;
        let at = |count: usize| {
            thumb_scale_for_panel(
                &uniform_aspects(count),
                inner,
                panel_w,
                1050.0,
                THUMB_ROW_GAP,
                THUMB_SCROLLBAR_W,
                &|_| f64::INFINITY,
            )
        };
        // Small sets still reach the largest step, as with the old ladder.
        assert!((at(1) - THUMB_MAX_SCALE).abs() < 1e-9); // The old ladder dropped from 1.1 to 0.85 at 6->7 (-23%); a step now only falls when the panel genuinely stops fitting.
                                                         // The old ladder dropped from 1.1 to 0.85 at 6->7 (-23%); now a step only falls when the
                                                         // panel genuinely stops fitting.
        assert!(
            at(7) > THUMB_MIN_SCALE,
            "7 windows must not fall to the old floor, got {}",
            at(7)
        );
        // Monotonic, and never falling by more than one candidate step between adjacent counts (no
        // cliff).
        // Monotonic: more windows can only keep or lower the step. Adjacent counts may fall two
        // candidate steps at once, because both the card width and the per-row capacity are
        // discrete (13 cards is exactly where the plan jumps from three rows to four); that is
        // packing arithmetic, not an artificial ladder cliff.
        for count in 1..40 {
            assert!(
                at(count + 1) <= at(count),
                "step grew with more windows at {count}"
            );
        }
        // A smaller panel never raises the step, and when nothing fits the smallest step is used and
        // scrolling takes over.
        let tiny = 600.0 - H_PADDING * 2.0 - THUMB_SCROLLBAR_W;
        assert!(
            thumb_scale_for_panel(
                &uniform_aspects(15),
                tiny,
                600.0,
                500.0,
                THUMB_ROW_GAP,
                THUMB_SCROLLBAR_W,
                &|_| f64::INFINITY,
            ) <= at(15)
        );
        assert_eq!(
            thumb_scale_for_panel(
                &uniform_aspects(60),
                tiny,
                600.0,
                300.0,
                THUMB_ROW_GAP,
                THUMB_SCROLLBAR_W,
                &|_| f64::INFINITY,
            ),
            THUMB_MIN_SCALE
        );
    }

    #[test]
    fn the_chosen_step_accounts_for_the_real_window_aspects() {
        // Since selection uses the real aspects, a set narrower than the base ratio should reach a
        // **larger** step (no space left unused), and the chosen step plus the real aspects must really
        // fit -- the point of giving the trial run the same inputs as the final packing.
        let panel_w = 1470.0 * PANEL_MAX_WIDTH_RATIO;
        let inner = panel_w - H_PADDING * 2.0 - THUMB_SCROLLBAR_W;
        let budget = 923.0 - 2.0 * PANEL_MARGIN;
        let cap = |_card_h: f64| f64::INFINITY;
        let narrow = vec![
            1.5, 1.5, 1.5, 1.5, 1.4, 1.4, 1.4, 1.4, 1.2, 1.2, 1.2, 1.2, 1.1, 1.1, 1.1, 1.1,
        ];
        let real = thumb_scale_for_panel(
            &narrow,
            inner,
            panel_w,
            budget,
            THUMB_ROW_GAP,
            THUMB_SCROLLBAR_W,
            &cap,
        );
        let blind = thumb_scale_for_panel(
            &uniform_aspects(narrow.len()),
            inner,
            panel_w,
            budget,
            THUMB_ROW_GAP,
            THUMB_SCROLLBAR_W,
            &cap,
        );
        assert!(
            real > blind,
            "a narrower window set must reach a larger step (real {real} vs base-aspect {blind})"
        );
        // The chosen step must not overflow with the real aspects.
        let plan = plan_thumb_scroll_layout_at_scale(
            &narrow,
            real,
            inner,
            panel_w,
            budget,
            THUMB_ROW_GAP,
            THUMB_SCROLLBAR_W,
            0.0,
            cap(thumb_card_h_for_scale(real)),
        );
        assert!(
            !plan.overflowed,
            "step {real} must not overflow with the real aspects"
        );
    }

    #[test]
    fn the_production_ladder_reaches_its_smallest_step() {
        // Lowering the floor must accompany a new smallest step: `thumb_card_h_for_scale` clamps its
        // input with THUMB_MIN_SCALE, so forgetting it silently pins the new step back to 0.85.
        assert!((THUMB_SCALE_STEPS.last().copied().unwrap() - THUMB_MIN_SCALE).abs() < 1e-9);
        assert!(
            THUMB_SCALE_STEPS.contains(&0.8),
            "the ladder must contain a 0.8 step"
        );
        assert!(thumb_card_h_for_scale(0.8) < thumb_card_h_for_scale(0.85));
        assert!(thumb_card_h_for_scale(0.75) < thumb_card_h_for_scale(0.8));
        // The smallest step stays legible: its preview area is above the floor.
        assert!(thumb_preview_h(thumb_card_h_for_scale(THUMB_MIN_SCALE)) > 40.0);
    }

    #[test]
    fn the_panel_margin_costs_a_step_not_a_row() {
        // The measured built-in screen (1470x956, 923 visible): after both margins the step should fall
        // to 0.75 instead of overflowing into scrolling.
        let panel_w = 1470.0 * PANEL_MAX_WIDTH_RATIO;
        let inner = panel_w - H_PADDING * 2.0 - THUMB_SCROLLBAR_W;
        let with_margin = thumb_scale_for_panel(
            &uniform_aspects(16),
            inner,
            panel_w,
            923.0 - 2.0 * PANEL_MARGIN,
            THUMB_ROW_GAP,
            THUMB_SCROLLBAR_W,
            &|_| f64::INFINITY,
        );
        let without_margin = thumb_scale_for_panel(
            &uniform_aspects(16),
            inner,
            panel_w,
            923.0,
            THUMB_ROW_GAP,
            THUMB_SCROLLBAR_W,
            &|_| f64::INFINITY,
        );
        assert!(
            (with_margin - 0.75).abs() < 1e-9,
            "with the margin subtracted the built-in display must land on 0.75, got {with_margin}"
        );
        assert!(
            with_margin <= without_margin,
            "the margin can only lower or keep the step ({with_margin} vs {without_margin})"
        );
        // On the measured 1920x1080 screen the margin is nearly free: the step stays 0.95.
        let wide = 1920.0 * PANEL_MAX_WIDTH_RATIO;
        let wide_inner = wide - H_PADDING * 2.0 - THUMB_SCROLLBAR_W;
        let on_wide = thumb_scale_for_panel(
            &uniform_aspects(16),
            wide_inner,
            wide,
            1050.0 - 2.0 * PANEL_MARGIN,
            THUMB_ROW_GAP,
            THUMB_SCROLLBAR_W,
            &|_| f64::INFINITY,
        );
        assert!(
            (on_wide - 0.95).abs() < 1e-9,
            "with the margin subtracted the wide screen must still land on 0.95, got {on_wide}"
        );
    }

    #[test]
    fn closing_cards_through_the_reflow_never_grows_the_panel() {
        // Measured: closing a window while the switcher is open made the panel taller (the reflow used
        // its own old formula: 4 rows + teaser = 901pt > budget 875). Closing from 25 cards down to 1
        // through the real entry points must keep the panel monotone non-increasing and inside the
        // budget.
        let budget = 923.0 - 2.0 * PANEL_MARGIN;
        let card_h = thumb_card_h_for_scale(THUMB_MIN_SCALE);
        let gap = THUMB_ROW_GAP;
        let panel_w = 1470.0 * PANEL_MAX_WIDTH_RATIO;
        let max_inner = panel_w - H_PADDING * 2.0 - THUMB_SCROLLBAR_W;
        let max_rows = thumb_max_rows(card_h, budget, gap);
        let teaser_fits = thumb_teaser_fits(card_h, budget, gap);
        let cap = thumb_card_w_for_aspect(card_h, THUMB_PREVIEW_RATIO).min(max_inner);
        let start = 25usize;
        let initial = plan_thumb_scroll_layout_at_scale(
            &uniform_aspects(start),
            THUMB_MIN_SCALE,
            max_inner,
            panel_w,
            budget,
            gap,
            THUMB_SCROLLBAR_W,
            0.0,
            cap,
        );
        assert!(initial.overflowed, "25 cards must take the scrolling path");
        let mut widths = vec![cap; start];
        let mut rows = initial.row_ranges.len();
        let mut overflowed = initial.overflowed;
        // The baseline is the panel height *before* the first close, which is exactly where the old
        // formula jumped (that is the growth the user saw).
        let mut previous = initial.panel_h;
        for remaining in (1..=start).rev() {
            let (_, row_ranges, _, next_overflowed) = plan_thumb_close_reflow(
                &widths,
                card_h,
                max_inner,
                gap,
                thumb_document_height_for_rows(rows, card_h, gap, THUMB_TOP_INSET),
                THUMB_TOP_INSET,
                overflowed,
                max_rows,
            );
            rows = row_ranges.len();
            overflowed = next_overflowed;
            let teaser_h = thumb_teaser_height(overflowed, teaser_fits, card_h, gap);
            let rendered_rows = if overflowed {
                rows.min(max_rows.max(1))
            } else {
                rows
            };
            let (panel_h, inset) = thumb_close_panel_metrics(
                rows,
                overflowed,
                card_h,
                gap,
                teaser_fits,
                max_rows,
                budget,
            );
            assert!(
                (inset - THUMB_TOP_INSET).abs() < 1e-9,
                "{remaining} left: content inset {inset:.1} must be {THUMB_TOP_INSET}"
            );
            assert!(
                panel_h <= budget + 1e-9,
                "{remaining} left: panel {panel_h:.1} exceeds the budget {budget:.1}"
            );
            // The panel height must be the content height: the cap is only a backstop and must not
            // silently hide a teaser that does not fit.
            let content_h = THUMB_TOP_INSET
                + rendered_rows as f64 * card_h
                + rendered_rows.saturating_sub(1) as f64 * gap
                + teaser_h
                + status_h();
            assert!(
                content_h <= budget + 1e-9,
                "{remaining} left: content {content_h:.1} does not fit the budget {budget:.1} (teaser/row rule bypassed)"
            );
            assert!(
                panel_h <= previous + 1e-9,
                "{remaining} left: panel {panel_h:.1} is taller than {previous:.1} before the close"
            );
            previous = panel_h;
            widths.pop();
        }
    }

    #[test]
    fn panels_hug_the_cards_and_stay_within_the_height_budget() {
        // Requested: the overlay is capped by the usable area (minus the margin), and inside that cap
        // its actual size follows the real card sizes. Three rows or more used to be stretched to the
        // cap, which decoupled the size from the cards and added a blank band at the top.
        let budget = 923.0 - 2.0 * PANEL_MARGIN;
        let card_h = thumb_card_h_for_scale(0.75);
        // Pure part: the panel height is the content height and the inset is always the mockup's top
        // inset, no matter the row count.
        for rows in 1..=thumb_max_rows(card_h, budget, THUMB_ROW_GAP) {
            let (panel_h, inset) = thumb_panel_metrics(rows, card_h, THUMB_ROW_GAP, 0.0, budget);
            assert!(
                (inset - THUMB_TOP_INSET).abs() < 1e-9,
                "{rows} rows: top inset {inset:.1} must be {THUMB_TOP_INSET} (no stretching)"
            );
            assert!(
                (panel_h - thumb_panel_height_for_rows(rows, rows, card_h, THUMB_ROW_GAP, false))
                    .abs()
                    < 1e-9,
                "{rows} rows: panel {panel_h:.1} must equal the content height"
            );
            assert!(panel_h <= budget + 1e-9);
        }
        // Backstop: with a budget too small for one row plus the teaser the panel still may not cross
        // the cap.
        let tiny = thumb_panel_metrics(
            1,
            card_h,
            THUMB_ROW_GAP,
            THUMB_ROW_GAP + card_h / 3.0,
            300.0,
        );
        assert!(
            tiny.0 <= 300.0 + 1e-9,
            "with a degenerate budget {:.1} exceeds the cap of 300",
            tiny.0
        );
        // Real layouts: never above the budget and never glued to it (the fill made it exactly the
        // budget).
        let panel_w = 1470.0 * PANEL_MAX_WIDTH_RATIO;
        let inner = panel_w - H_PADDING * 2.0 - THUMB_SCROLLBAR_W;
        let cap = thumb_card_w_for_aspect(card_h, THUMB_PREVIEW_RATIO).min(inner);
        for count in [1usize, 6, 12, 24] {
            let plan = plan_thumb_scroll_layout_at_scale(
                &uniform_aspects(count),
                0.75,
                inner,
                panel_w,
                budget,
                THUMB_ROW_GAP,
                THUMB_SCROLLBAR_W,
                0.0,
                cap,
            );
            assert!(
                plan.panel_h <= budget + 1e-9,
                "{count} windows: panel {:.1} exceeds the budget {budget:.1}",
                plan.panel_h
            );
            assert!(
                plan.panel_h < budget - 1.0,
                "{count} windows: panel {:.1} must hug the card content instead of stretching to the budget {budget:.1}",
                plan.panel_h
            );
        }
    }

    #[test]
    fn overflowing_panels_stay_within_the_height_budget() {
        // The measured pitfall (2026-09-24): with many windows the layout overflows and the panel used
        // to be one teaser taller than the budget, so the clamp ate the top/bottom margins (budget 875
        // -> panel 921 -> margins down to 33/2). This pins "never above the budget, overflow included".
        let panel_w = 1470.0 * PANEL_MAX_WIDTH_RATIO;
        let inner = panel_w - H_PADDING * 2.0 - THUMB_SCROLLBAR_W;
        let budget = 923.0 - 2.0 * PANEL_MARGIN;
        let cap = |card_h: f64| thumb_card_w_for_aspect(card_h, THUMB_PREVIEW_RATIO).min(inner);
        for (count, expect_overflow) in [(6usize, false), (24usize, true)] {
            let aspects = uniform_aspects(count);
            for step in [0.75, 0.8, 0.85, 1.0] {
                let layout = plan_thumb_scroll_layout_at_scale(
                    &aspects,
                    step,
                    inner,
                    panel_w,
                    budget,
                    THUMB_ROW_GAP,
                    THUMB_SCROLLBAR_W,
                    0.0,
                    cap(thumb_card_h_for_scale(step)),
                );
                assert!(
                    layout.panel_h <= budget + 1e-9,
                    "{count} windows / step {step}: panel {:.1} exceeds the budget {budget:.1}",
                    layout.panel_h
                );
                if count == 24 {
                    assert_eq!(
                        layout.overflowed, expect_overflow,
                        "24 windows must overflow"
                    );
                }
            }
        }
    }

    #[test]
    fn flow_layout_enlarges_small_sets_to_the_cap() {
        let layout = plan_thumb_flow_layout(&[1.6, 1.6], 1, 1200.0, 1000.0, THUMB_ROW_GAP);
        assert_eq!(layout.visible, 0..2);
        assert!(!layout.overflowed);
        assert!((layout.scale - LEGACY_THUMB_MAX_SCALE).abs() < 1e-9);
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
        let base_h = thumb_card_h_for_scale(thumb_scale_for_count(aspects.len()));
        let two_row_panel_h = thumb_panel_height_for_rows(2, 2, base_h, THUMB_ROW_GAP, true) + 0.1;
        let initial = plan_thumb_flow_layout(&aspects, 1, 900.0, two_row_panel_h, THUMB_ROW_GAP);
        assert!(initial.overflowed);
        // Only the use of the current scale is asserted here; the ladder values are pinned by
        // the_scale_ladder_keeps_its_floor_and_cap so packing tests cannot break on a ladder tweak.
        assert_eq!(initial.scale, thumb_scale_for_count(aspects.len()));
        // A page is two rows of whatever fits per row: derived from the layout instead of hardcoding
        // the four cards the old size produced.
        let per_row = initial.row_ranges.first().unwrap().len();
        assert_eq!(initial.row_ranges.len(), 2);
        assert!(
            per_row >= 2,
            "a row should hold at least two cards, got {per_row}"
        );
        assert_eq!(initial.visible, 0..per_row * 2);
        // Width follows the widest actual row instead of consuming the full 900pt packing budget.
        let card_w = thumb_card_w_for_aspect(thumb_card_h_for_scale(initial.scale), 1.6);
        assert_eq!(
            initial.panel_w,
            per_row as f64 * card_w + (per_row - 1) as f64 * THUMB_ROW_GAP + H_PADDING * 2.0
        );

        let next =
            plan_thumb_flow_layout(&aspects, per_row * 2, 900.0, two_row_panel_h, THUMB_ROW_GAP);
        // The second page starts where the first ends and covers the rest; the per-row count comes
        // from the ladder, so the page count is not hardcoded.
        assert_eq!(next.visible, per_row * 2..aspects.len());
        assert!(next.page_index > initial.page_index);
        // Eight cards always make two pages of two rows each (two per row -> four rows, three per row
        // -> three rows), independent of the ladder.
        assert_eq!(initial.page_count, 2);
        assert_eq!(next.page_count, 2);
        assert!(next.panel_w <= initial.panel_w);
        assert_eq!(next.panel_h, initial.panel_h);
        assert_eq!(
            next.placements
                .iter()
                .map(|placement| placement.index)
                .collect::<Vec<_>>(),
            (per_row * 2..aspects.len()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn wide_thumbnail_budget_fits_four_columns_without_changing_height() {
        let aspects = vec![1.6; 12];
        let card_h = thumb_card_h_for_scale(thumb_scale_for_count(aspects.len()));
        let three_row_panel_h =
            THUMB_TOP_INSET + card_h * 3.0 + THUMB_ROW_GAP * 2.0 + status_h() + 0.1;
        // Budgets derive from the *current* card width: the narrow one holds three columns (twelve
        // cards need four rows -> overflow) and the wide one holds four (twelve cards fit three rows ->
        // one page). The assertions therefore express the intent "horizontal budget decides the column
        // count" without depending on the ladder.
        let card_w = thumb_card_w_for_aspect(card_h, 1.6);
        let inner_for_columns = |columns: f64| columns * card_w + (columns - 1.0) * THUMB_ROW_GAP;
        let capped = plan_thumb_flow_layout(
            &aspects,
            1,
            inner_for_columns(3.0),
            three_row_panel_h,
            THUMB_ROW_GAP,
        );
        let wide = plan_thumb_flow_layout(
            &aspects,
            1,
            inner_for_columns(4.0),
            three_row_panel_h,
            THUMB_ROW_GAP,
        );

        // Under the old cap only three cards fit per row; with the wider budget four
        // columns fit and all twelve cards remain on one page.
        // Assert intent rather than the ranges the old card size produced: a wider horizontal budget
        // fits every card, at the same height.
        assert!(capped.overflowed);
        assert!(!wide.overflowed);
        assert_eq!(wide.page_count, 1);
        assert_eq!(wide.visible, 0..aspects.len());
        assert!(wide.visible.len() > capped.visible.len());
        // Only horizontal capacity changes; the three-row height remains identical.
        assert_eq!(wide.panel_h, capped.panel_h);
    }

    #[test]
    fn selecting_any_item_on_a_page_keeps_the_same_page_boundary() {
        let aspects = vec![1.6; 8];
        let base_h = thumb_card_h_for_scale(thumb_scale_for_count(aspects.len()));
        let max_h = thumb_panel_height_for_rows(2, 2, base_h, THUMB_ROW_GAP, true) + 0.1;
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
            teaser_fits: true,
            max_panel_h: thumb_panel_height_for_rows(2, 2, card_h, THUMB_ROW_GAP, true),
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
        let max_h = thumb_panel_height_for_rows(2, 2, card_h, THUMB_ROW_GAP, true) + 0.1;
        let initial = plan_thumb_flow_layout(&aspects, 1, 700.0, max_h, THUMB_ROW_GAP);
        // Two standard cards share a row while a wide card occupies its own, so
        // the leading slice naturally drops to three items.
        assert_eq!(initial.visible, 0..3);
    }

    #[test]
    fn scroll_layout_caps_wide_cards_to_the_reference_width() {
        let layout = plan_thumb_scroll_layout_with_max_card_w(
            &[1.6, 5.0, 10.0],
            900.0,
            1000.0,
            1000.0,
            THUMB_ROW_GAP,
            THUMB_SCROLLBAR_W,
            0.0,
            |_| 320.0,
        );

        assert!(layout
            .document_placements
            .iter()
            .all(|placement| placement.width <= 320.0));
        assert_eq!(
            layout
                .document_placements
                .iter()
                .map(|placement| placement.width)
                .max_by(|a, b| a.partial_cmp(b).unwrap())
                .unwrap(),
            320.0
        );
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
        let card_h = thumb_card_h_for_scale(thumb_scale_for_count(aspects.len()));
        let max_h = thumb_panel_height_for_rows(2, 2, card_h, THUMB_ROW_GAP, true) + 0.1;
        let first = plan_thumb_scroll_layout_at_scale(
            &aspects,
            thumb_scale_for_count(aspects.len()),
            1288.0,
            1400.0,
            max_h,
            THUMB_ROW_GAP,
            THUMB_SCROLLBAR_W,
            0.0,
            f64::INFINITY,
        );
        let next = plan_thumb_scroll_layout_at_scale(
            &aspects,
            thumb_scale_for_count(aspects.len()),
            1288.0,
            1400.0,
            max_h,
            THUMB_ROW_GAP,
            THUMB_SCROLLBAR_W,
            card_h + THUMB_ROW_GAP,
            f64::INFINITY,
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
        let card_h = thumb_card_h_for_scale(thumb_scale_for_count(aspects.len()));
        let max_h = thumb_panel_height_for_rows(2, 2, card_h, THUMB_ROW_GAP, true) + 0.1;
        let layout = plan_thumb_scroll_layout_at_scale(
            &aspects,
            thumb_scale_for_count(aspects.len()),
            900.0,
            1400.0,
            max_h,
            THUMB_ROW_GAP,
            THUMB_SCROLLBAR_W,
            0.0,
            f64::INFINITY,
        );

        assert!(layout.overflowed);
        // Row structure is asserted as intent: every row but the last is full, tiling all eight cards
        // (instead of hardcoding the row count the old card size produced).
        let per_row = layout.row_ranges.first().unwrap().len();
        assert!(layout
            .row_ranges
            .iter()
            .take(layout.row_ranges.len() - 1)
            .all(|row| row.len() == per_row));
        assert_eq!(layout.row_ranges.first().unwrap().start, 0);
        assert_eq!(layout.row_ranges.last().unwrap().end, aspects.len());
        // The width follows the widest visible row, with the card width derived from the layout's own
        // scale instead of a hardcoded base width.
        let card_w = thumb_card_w_for_aspect(thumb_card_h_for_scale(layout.scale), 1.6);
        assert_eq!(
            layout.panel_w,
            per_row as f64 * card_w
                + (per_row - 1) as f64 * THUMB_ROW_GAP
                + H_PADDING * 2.0
                + THUMB_SCROLLBAR_W
        );
        let first = layout.document_placements.first().unwrap();
        assert_eq!(first.x, H_PADDING + THUMB_SCROLLBAR_W / 2.0);
        // Right padding = panel width - (first card x + a full row of cards + in-row gaps), with the
        // card count taken from the layout's own per_row instead of a hardcoded two.
        assert_eq!(
            layout.panel_w
                - (first.x + first.width * per_row as f64 + (per_row - 1) as f64 * THUMB_ROW_GAP),
            H_PADDING + THUMB_SCROLLBAR_W / 2.0
        );
        assert!(layout.panel_w < 1400.0);
    }

    #[test]
    fn scrolling_overflow_fills_the_initial_viewport_greedily() {
        let aspects = vec![1.6; 13];
        let card_h = thumb_card_h_for_scale(thumb_scale_for_count(aspects.len()));
        let max_h = thumb_panel_height_for_rows(3, 3, card_h, THUMB_ROW_GAP, true) + 0.1;
        let layout = plan_thumb_scroll_layout_at_scale(
            &aspects,
            thumb_scale_for_count(aspects.len()),
            1288.0,
            1400.0,
            max_h,
            THUMB_ROW_GAP,
            THUMB_SCROLLBAR_W,
            0.0,
            f64::INFINITY,
        );

        assert!(layout.overflowed);
        assert_eq!(layout.row_ranges, vec![0..4, 4..8, 8..12, 12..13]);
        assert_eq!(layout.visible, 0..13);
    }

    #[test]
    fn scrolling_layout_keeps_balanced_packing_when_everything_fits() {
        let aspects = vec![2.2, 0.7, 0.7];
        let card_h = thumb_card_h_for_scale(thumb_scale_for_count(aspects.len()));
        let max_h = thumb_panel_height_for_rows(2, 2, card_h, THUMB_ROW_GAP, true) + 0.1;
        let layout = plan_thumb_scroll_layout_at_scale(
            &aspects,
            thumb_scale_for_count(aspects.len()),
            800.0,
            900.0,
            max_h,
            THUMB_ROW_GAP,
            THUMB_SCROLLBAR_W,
            0.0,
            f64::INFINITY,
        );

        assert!(!layout.overflowed);
        assert_eq!(layout.row_ranges, vec![0..1, 1..3]);
        assert_eq!(layout.visible, 0..3);
    }

    #[test]
    fn scrolling_layout_clamps_to_the_last_row() {
        let aspects = vec![1.6; 20];
        let card_h = thumb_card_h_for_scale(thumb_scale_for_count(aspects.len()));
        let max_h = thumb_panel_height_for_rows(2, 2, card_h, THUMB_ROW_GAP, true) + 0.1;
        let layout = plan_thumb_scroll_layout_at_scale(
            &aspects,
            thumb_scale_for_count(aspects.len()),
            1288.0,
            1400.0,
            max_h,
            THUMB_ROW_GAP,
            THUMB_SCROLLBAR_W,
            f64::MAX,
            f64::INFINITY,
        );
        assert_eq!(layout.row_start, 2);
        assert_eq!(layout.visible, 8..20);
    }

    #[test]
    fn scrolling_layout_keeps_fractional_offset_and_renders_partial_row() {
        let aspects = vec![1.6; 20];
        let card_h = thumb_card_h_for_scale(thumb_scale_for_count(aspects.len()));
        let row_pitch = card_h + THUMB_ROW_GAP;
        let max_h = thumb_panel_height_for_rows(2, 2, card_h, THUMB_ROW_GAP, true) + 0.1;
        let layout = plan_thumb_scroll_layout_at_scale(
            &aspects,
            thumb_scale_for_count(aspects.len()),
            1288.0,
            1400.0,
            max_h,
            THUMB_ROW_GAP,
            THUMB_SCROLLBAR_W,
            12.0,
            f64::INFINITY,
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

        let before_row_boundary = plan_thumb_scroll_layout_at_scale(
            &aspects,
            thumb_scale_for_count(aspects.len()),
            1288.0,
            1400.0,
            max_h,
            THUMB_ROW_GAP,
            THUMB_SCROLLBAR_W,
            row_pitch - 1.0,
            f64::INFINITY,
        );
        let at_row_boundary = plan_thumb_scroll_layout_at_scale(
            &aspects,
            thumb_scale_for_count(aspects.len()),
            1288.0,
            1400.0,
            max_h,
            THUMB_ROW_GAP,
            THUMB_SCROLLBAR_W,
            row_pitch,
            f64::INFINITY,
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
        let (placements, rows, panel_w, overflowed) =
            plan_thumb_close_reflow(&widths, 80.0, 220.0, 10.0, 180.0, THUMB_TOP_INSET, false, 3);

        assert_eq!(rows, vec![0..2, 2..4, 4..5]);
        assert_eq!(
            placements.iter().map(|p| p.index).collect::<Vec<_>>(),
            (0..5).collect::<Vec<_>>()
        );
        assert_eq!(placements[0].x, placements[2].x);
        assert_eq!(placements[1].x, placements[3].x);
        assert!(placements[2].y < placements[0].y);
        assert_eq!(panel_w, 284.0);
        assert!(!overflowed);
    }

    #[test]
    fn overflow_close_reflow_fills_rows_in_window_order() {
        let widths = vec![100.0; 9];
        let (placements, rows, panel_w, overflowed) =
            plan_thumb_close_reflow(&widths, 80.0, 320.0, 10.0, 260.0, THUMB_TOP_INSET, true, 2);

        assert_eq!(rows, vec![0..3, 3..6, 6..9]);
        assert_eq!(placements[3].y, placements[0].y - 90.0);
        assert_eq!(placements[6].y, placements[3].y - 90.0);
        assert_eq!(panel_w, 398.0);
        assert!(overflowed);
    }

    #[test]
    fn close_reflow_panel_height_shrinks_when_overflow_ends() {
        let overflowing = thumb_panel_height_for_rows(3, 2, 80.0, 10.0, true);
        let fitting = thumb_panel_height_for_rows(2, 2, 80.0, 10.0, false);

        assert!(overflowing > fitting);
        assert_eq!(fitting, THUMB_TOP_INSET + 2.0 * 80.0 + 10.0 + status_h());
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
