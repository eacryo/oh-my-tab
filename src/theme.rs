//! Theme and layout: config-derived colors (Colors), dark-mode detection, and card/window
//! size accessors. Depended on by overlay and other modules.

use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::{NSPoint, NSRect, NSSize};
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
/// Icon-only card width; the height comes from the card's content (see `card_h`).
pub(crate) const ICON_CARD_W: f64 = 140.0;
pub(crate) const ICON_CARD_GAP: f64 = 0.0;
pub(crate) const ICON_SIZE: f64 = 110.0;
/// Gap between the card's top edge and the icon canvas in the icon-only layout.
pub(crate) const ICON_TOP_INSET: f64 = 4.0;
/// Gap between the icon canvas and the first caption line in the icon-only layout.
pub(crate) const ICON_TEXT_GAP: f64 = 4.5;
/// Gap between the two caption lines in the icon-only layout.
pub(crate) const ICON_LINE_GAP: f64 = 2.0;
/// Gap the card keeps below its last caption line.
pub(crate) const ICON_BOTTOM_INSET: f64 = 4.0;
pub(crate) const CARD_TEXT_BASE_SIZE: f64 = 12.0;
pub(crate) const CARD_TEXT_SIZE_MIN: f64 = 13.0;
pub(crate) const CARD_TEXT_SIZE_MAX: f64 = 20.0;
const STATUS_BAR_TEXT_BASE_SIZE: f64 = 13.0;

/// The small heading above each settings card.
/// (`.section-title { font-size: 11px }`).
/// The design system's `section-header` role: 12pt, 600, secondary.
///
/// It was 11.0, taken from the mock's `.section-title`, which is not on the 12/14/20/26 scale
/// `docs/design-style-en.md` fixes and which `--smoke-settings-layout` rejects (that mismatch was
/// backlog #41). The document wins: the code is the bug.
pub(crate) const FONT_SECTION_TITLE: f64 = 12.0;
pub(crate) const FONT_CAPTION: f64 = 12.0;
/// The mock's `--fs-13`: field text and labels that sit one step above a caption and one below a
/// control's own 14pt (`.field-shell input`, the settings search field). Kept as its own step rather
/// than rounded to 12 or 14, because the mock draws that size and rounding it moves text metrics
/// in every field the setting appears in.
pub(crate) const FONT_BODY: f64 = 13.0;
pub(crate) const FONT_CONTROL: f64 = 14.0;
pub(crate) const FONT_SIDEBAR_TITLE: f64 = 20.0;
pub(crate) const FONT_PAGE_TITLE: f64 = 26.0;
pub(crate) const LINE_HEIGHT_CAPTION_RATIO: f64 = 1.35;

/// A settings row's description line (`.setting-desc` in the mock).
///
/// It shares the caption's 12pt face but not its line height: the mock sets 1.45 for these
/// multi-line rows, while the caption role's 1.35 is tuned for single-line labels elsewhere.
pub(crate) const FONT_SETTING_DESC: f64 = FONT_CAPTION;
pub(crate) const LINE_HEIGHT_SETTING_DESC_RATIO: f64 = 1.45;
pub(crate) const LINE_HEIGHT_BODY_RATIO: f64 = 1.4;
pub(crate) const LINE_HEIGHT_TITLE_RATIO: f64 = 1.2;
pub(crate) const fn line_height(size: f64, ratio: f64) -> f64 {
    size * ratio
}

/// How far a forced line box drops a text field's glyphs below the box's centre: TextKit places
/// the extra leading above the glyphs, so half the difference between the forced and the font's
/// natural line height is the distance the ink sinks. Returns 0 when no forcing applies -- a
/// missing, non-finite or non-positive value, or a forced height no taller than the natural one.
pub(crate) fn forced_line_height_offset(forced: f64, natural: f64) -> f64 {
    if forced.is_finite() && natural.is_finite() && natural > 0.0 && forced > natural {
        (forced - natural) / 2.0
    } else {
        0.0
    }
}

#[cfg(test)]
mod line_height_tests {
    use super::*;

    #[test]
    fn line_height_ratios_multiply_font_size() {
        assert!((line_height(FONT_CAPTION, LINE_HEIGHT_CAPTION_RATIO) - 16.2).abs() < 1e-10);
        assert!((line_height(FONT_CONTROL, LINE_HEIGHT_BODY_RATIO) - 19.6).abs() < 1e-10);
        assert!((line_height(FONT_PAGE_TITLE, LINE_HEIGHT_TITLE_RATIO) - 31.2).abs() < 1e-10);
    }

    #[test]
    fn forced_line_height_offset_is_half_the_extra_leading() {
        // The real pair: the body-ratio token forces 19.6pt on a font whose natural line is
        // ~16.49pt, sinking the ink ~1.555pt below the box centre.
        let forced = line_height(FONT_CONTROL, LINE_HEIGHT_BODY_RATIO);
        assert!((forced_line_height_offset(forced, 16.49) - 1.555).abs() < 1e-10);
        // A height equal to or below the natural line adds no leading.
        assert_eq!(forced_line_height_offset(16.49, 16.49), 0.0);
        assert_eq!(forced_line_height_offset(16.0, 19.6), 0.0);
        // Broken inputs correct nothing rather than producing a bogus shift.
        assert_eq!(forced_line_height_offset(f64::NAN, 16.49), 0.0);
        assert_eq!(forced_line_height_offset(19.6, 0.0), 0.0);
        assert_eq!(forced_line_height_offset(19.6, f64::INFINITY), 0.0);
    }
}
pub(crate) const FONT_WEIGHT_REGULAR: f64 = 0.0;
pub(crate) const FONT_WEIGHT_MEDIUM: f64 = 0.23;
/// Settings row labels (`.setting-label` in the mock: CSS weight 520).
///
/// 520 sits just above SF's medium face, and at this UI's 14pt label size the semibold-leaning
/// 0.25 drew labels visibly heavier than the rest of the row (measured stroke width 3.5-4.1px
/// against 2.5px for the description beside them). SF medium (0.23) is the nearest face to 520
/// that keeps the hierarchy by size and colour rather than by weight.
pub(crate) const FONT_WEIGHT_SETTING_LABEL: f64 = 0.23;
pub(crate) const FONT_WEIGHT_SEMIBOLD: f64 = 0.3;
/// Visual match for the page title on the native Chinese fallback face; semibold still looked too heavy.
pub(crate) const FONT_WEIGHT_PAGE_TITLE: f64 = 0.23;
pub(crate) const FONT_WEIGHT_BOLD: f64 = 0.4;
pub(crate) const ANIMATION_DURATION_FAST: f64 = 0.175;
pub(crate) const ANIMATION_DURATION_MEDIUM: f64 = 0.380;
pub(crate) const ANIMATION_EXIT_RATIO: f64 = 0.75;
pub(crate) const ANIMATION_EASE_STANDARD: (f32, f32, f32, f32) = (0.24, 1.0, 0.4, 1.0);
pub(crate) const ELEVATION_MED_SHADOW_COLOR: u32 = 0x000000FF;
pub(crate) const ELEVATION_MED_SHADOW_OPACITY: f32 = 0.10;
pub(crate) const ELEVATION_MED_SHADOW_RADIUS: f64 = 12.0;
pub(crate) const ELEVATION_MED_SHADOW_OFFSET_Y: f64 = -4.0;
/// The style document's `high` level: CSS `0 12px 32px rgba(0,0,0,.18)`, used by the floating panels
/// (see docs/design-style-en.md §7). The document's blur radius maps 1:1 to `shadowRadius`, as the med
/// level above already does, and a negative y offset moves the shadow down (AppKit's y axis is up).
pub(crate) const ELEVATION_HIGH_SHADOW_COLOR: u32 = 0x000000FF;
pub(crate) const ELEVATION_HIGH_SHADOW_OPACITY: f32 = 0.18;
pub(crate) const ELEVATION_HIGH_SHADOW_RADIUS: f64 = 32.0;
pub(crate) const ELEVATION_HIGH_SHADOW_OFFSET_Y: f64 = -12.0;

/// One elevation level's shadow parameters, so a surface asks for a level instead of four numbers.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Elevation {
    pub(crate) color: u32,
    pub(crate) opacity: f32,
    pub(crate) radius: f64,
    pub(crate) offset_y: f64,
}

pub(crate) const ELEVATION_MED: Elevation = Elevation {
    color: ELEVATION_MED_SHADOW_COLOR,
    opacity: ELEVATION_MED_SHADOW_OPACITY,
    radius: ELEVATION_MED_SHADOW_RADIUS,
    offset_y: ELEVATION_MED_SHADOW_OFFSET_Y,
};

pub(crate) const ELEVATION_HIGH: Elevation = Elevation {
    color: ELEVATION_HIGH_SHADOW_COLOR,
    opacity: ELEVATION_HIGH_SHADOW_OPACITY,
    radius: ELEVATION_HIGH_SHADOW_RADIUS,
    offset_y: ELEVATION_HIGH_SHADOW_OFFSET_Y,
};

/// How far a panel's window extends beyond the panel itself, per edge.
///
/// The panel outline's width. §7: a border is `card_border`, not a shadow and not a darker background.
pub(crate) const PANEL_OUTLINE_WIDTH: f64 = 1.0;

/// The panel rect stays the semantic rect: layout, hit-testing, the saved HUD position and
/// `--e2e-state` are all in panel coordinates, so enlarging a window must not move any of them. The
/// window rect exists only because the WindowServer clips whatever exceeds a window's own frame --
/// including a layer shadow, which is what needs the room.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct PanelInsets {
    pub(crate) top: f64,
    pub(crate) right: f64,
    pub(crate) bottom: f64,
    pub(crate) left: f64,
}

/// The window padding a level's shadow starts from.
///
/// Deliberately an estimate, not a verified value: a blur's tail is not guaranteed to have decayed at
/// `radius`, so this is where a measurement begins. `scripts/e2e/panel-contrast.sh` measures the
/// outermost ring of the window against the pinned backdrop and fails while it is still darker than the
/// backdrop, which is the check that decides whether this is enough.
pub(crate) fn elevation_insets(level: Elevation) -> PanelInsets {
    PanelInsets {
        top: level.radius + level.offset_y.max(0.0) + ELEVATION_SHADOW_TAIL_ALLOWANCE,
        right: level.radius + ELEVATION_SHADOW_TAIL_ALLOWANCE,
        // The shadow is offset downward (negative y), so only the bottom edge needs the offset's room.
        bottom: level.radius + (-level.offset_y).max(0.0) + ELEVATION_SHADOW_TAIL_ALLOWANCE,
        left: level.radius + ELEVATION_SHADOW_TAIL_ALLOWANCE,
    }
}

/// How far past the blur's nominal reach the window is padded.
///
/// `radius + |offset|` is where the shadow's *core* ends, not where it reaches zero: a layer shadow's tail
/// continues past it, so padding by the radius alone left the tail still 8 tone units dark at the window's
/// outermost ring (measured with `scripts/e2e/panel-contrast.sh`, which fails while that ring is darker than
/// the pinned backdrop). This allowance is what the measurement asked for: at 24pt the worst column of the
/// bottom ring still read -2.2 tone units, and at 32pt it reads under -1.5 across the whole edge. It is a
/// measurement result, not a derivation, and the scenario is the command that reproduces it.
pub(crate) const ELEVATION_SHADOW_TAIL_ALLOWANCE: f64 = 32.0;

/// The window frame (screen coordinates) holding a panel at `panel` (also screen coordinates).
pub(crate) fn window_frame_for_panel(panel: NSRect, insets: PanelInsets) -> NSRect {
    NSRect::new(
        NSPoint::new(panel.origin.x - insets.left, panel.origin.y - insets.bottom),
        NSSize::new(
            panel.size.width + insets.left + insets.right,
            panel.size.height + insets.top + insets.bottom,
        ),
    )
}

/// The panel's rect inside a window of `window_size` (window-local coordinates).
///
/// This is the rect a shadow path encloses, and it is derived from the *window* size on purpose: padding
/// already sits outside it, so deriving it from the panel size would let a caller expand it a second
/// time and put the shadow's silhouette over the padding instead of over the panel.
pub(crate) fn panel_rect_in_window(window_size: NSSize, insets: PanelInsets) -> NSRect {
    NSRect::new(
        NSPoint::new(insets.left, insets.bottom),
        NSSize::new(
            (window_size.width - insets.left - insets.right).max(0.0),
            (window_size.height - insets.top - insets.bottom).max(0.0),
        ),
    )
}
pub(crate) const OVERLAY_SYMBOL_SHADOW_OPACITY: f32 = 0.85;
pub(crate) const OVERLAY_SYMBOL_SHADOW_RADIUS: f64 = 2.0;
pub(crate) const OVERLAY_SYMBOL_SHADOW_OFFSET_Y: f64 = -1.0;
/// Thumbnail corner badges: a circular `badge_scrim` chip sits behind the white symbol so
/// it stays legible over bright captured frames. The glyph keeps the 22pt mark; the chip
/// pads it by 4pt (spacing-4) per side and is a circle per the `radius-full` badge rule.
/// The symbol shadow moves to the chip layer; the glyph itself casts nothing.
pub(crate) const OVERLAY_BADGE_GLYPH: f64 = 22.0;
pub(crate) const OVERLAY_BADGE_GLYPH_INSET: f64 = 4.0;

pub(crate) const RADIUS_CONTROL: f64 = 8.0;
pub(crate) const RADIUS_CARD: f64 = 12.0;
pub(crate) const RADIUS_PANEL: f64 = 18.0;
pub(crate) const RADIUS_FULL: f64 = 9999.0;

// ---- Gesture hint card ----
//
// The card the trail shows while a gesture is being drawn: a dark plate under the pointer with the
// stroke's direction as a large arrow and the action it would run below it. The reference is the
// browser gesture-hint bubble; the numbers here are the ones it uses, at 2x the 4px grid.
pub(crate) const GESTURE_HINT_BG: u32 = 0x3C3C3CFF;
pub(crate) const GESTURE_HINT_INK: u32 = 0xFFFFFFFF;
pub(crate) const GESTURE_HINT_RADIUS: f64 = 14.0;
/// The plate is sized to its label: this is the minimum width, up to GESTURE_HINT_MAX_WIDTH.
pub(crate) const GESTURE_HINT_MIN_WIDTH: f64 = 200.0;
pub(crate) const GESTURE_HINT_MAX_WIDTH: f64 = 320.0;
/// The minimum height; a label that wraps to two lines grows the plate.
pub(crate) const GESTURE_HINT_HEIGHT: f64 = 140.0;
pub(crate) const GESTURE_HINT_PADDING: f64 = 16.0;
pub(crate) const GESTURE_HINT_LINE: f64 = 22.0;
/// Between the arrow and its label inside the card.
pub(crate) const GESTURE_HINT_GAP_INNER: f64 = 10.0;
pub(crate) const GESTURE_HINT_GLYPH_SIZE: f64 = 96.0;
pub(crate) const GESTURE_HINT_FONT: f64 = 19.0;
pub(crate) const CLIPBOARD_EDGE_OUTER_WIDTH: f64 = 0.5;
pub(crate) const CLIPBOARD_EDGE_INNER_WIDTH: f64 = 0.5;
pub(crate) const CLIPBOARD_EDGE_OUTSET: f64 = 0.5;
pub(crate) const CLIPBOARD_EDGE_OUTER_LIGHT: u32 = 0x00000045;
pub(crate) const CLIPBOARD_EDGE_OUTER_DARK: u32 = 0x00000045;
pub(crate) const CLIPBOARD_EDGE_HIGHLIGHT: u32 = 0xFFFFFF70;
pub(crate) const CLIPBOARD_EDGE_SHADE: u32 = 0xFFFFFF18;
/// The compact clear HUD needs a wider lit bevel to read at the same strength as a large panel.
pub(crate) const HUD_CLEAR_EDGE_WIDTH: f64 = 1.25;
pub(crate) const HUD_CLEAR_EDGE_HIGHLIGHT: u32 = 0xFFFFFFE0;
/// Passive HUD glass needs a brighter veil than key-window glass in light mode.
pub(crate) const HUD_GLASS_COMPENSATION_LIGHT: u32 = 0xFFFFFFE8;
pub(crate) const HUD_GLASS_COMPENSATION_DARK: u32 = 0x1C1C1E99;

pub(crate) const CLIPBOARD_CONTACT_SHADOW: Elevation = Elevation {
    color: 0x000000FF,
    opacity: 0.18,
    radius: 8.0,
    offset_y: -2.0,
};
/// Corner radius of a shortcut legend's chip (the clipboard footer's key symbols). The chip is
/// a grouping aid, not a control: `radius-control` is sized for 32pt controls and reads as a
/// pill on a small chip, so the chip derives its radius from its own height.
/// See docs/design-style-en.md §6.
pub(crate) const RADIUS_LEGEND_CHIP: f64 = 5.0;
/// A shortcut legend's chip fill: a low-alpha wash applied *toward* the surface's opposite, so one
/// value separates the chip in both modes (darkening a light surface, lightening a dark one) rather
/// than a fixed gray that vanishes in one of them. The chip keeps no border -- a border is what makes
/// a chip read as a pressable control (style doc §6.1).
pub(crate) const LEGEND_CHIP_ALPHA: u32 = 0x0E;
/// Horizontal padding on each side of a legend chip's label. A chip is keyed to a *key*, not to a
/// character, so a long name ("Page Down") widens its own chip and needs breathing room around its
/// text. It is half the 4pt grid's smallest step: a single-glyph chip is already a 20pt square, and
/// this only keeps the glyph from touching the wash's edge.
/// See docs/design-style-en.md §6.1.
pub(crate) const LEGEND_CHIP_TEXT_PAD_X: f64 = 2.0;
/// The wash behind a modal settings panel: it dims the page under the panel so the floating editor
/// reads as modal. 25% black, the same value both settings panels use, so they match.
/// See docs/design-style-en.md §7 (a panel floats; the scrim is what puts the page behind it).
pub(crate) const MODAL_SCRIM: u32 = 0x00000040;
/// A disabled control's fill opacity: the style document's §10 rule is "50% opacity on the
/// control's fill", and `setEnabled:` alone does not dim a layer-filled button.
pub(crate) const DISABLED_FILL_OPACITY: f64 = 0.5;
pub(crate) const OVERLAY_RING_INSET: f64 = 3.0;
/// Captured window pixels keep square corners inside the rounded card.
pub(crate) const THUMBNAIL_PREVIEW_RADIUS: f64 = 0.0;
/// How far the icon-only layout's selection plate sits outside the *card*: the plate wraps the
/// whole card -- icon and both caption rows -- so it marks the same unit of selection the thumbnail
/// layout's ring does.
///
/// Zero, and that is geometric rather than timid: the icon grid packs cards edge to edge
/// (`ICON_CARD_GAP == 0`), so a card's own boundary is already the midpoint between its icon and its
/// neighbour's, 15pt from each. Any positive inset puts the plate nearer the neighbouring icon than
/// its own -- at 7pt the plate's edge sat 22pt from its own icon and 8pt from the neighbour's --
/// which reads as selecting the wrong card. The thumbnail grid keeps a 14pt gap, which is why its
/// ring can sit `OVERLAY_RING_INSET` outside its cards.
pub(crate) const SELECTION_CARD_INSET: f64 = 0.0;
/// The plates' stroke, drawn inward from the plate's edge (CALayer draws a border inside the
/// frame). What is left of the inset is the wash the rim sits on.
pub(crate) const SELECTION_RING_STROKE: f64 = 2.0;

fn linear_channel(value: f64) -> f64 {
    let value = value / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

/// A token's sRGB channels. `pub(crate)` because the panel-surface tests model the material itself
/// (an unwashed panel passes the backdrop through, so the backdrop's channels *are* the surface).
pub(crate) fn color_rgb(color: u32) -> [f64; 3] {
    [
        ((color >> 24) & 0xff) as f64,
        ((color >> 16) & 0xff) as f64,
        ((color >> 8) & 0xff) as f64,
    ]
}

fn composite_rgb(color: u32, background: u32) -> [f64; 3] {
    let alpha = (color & 0xff) as f64 / 255.0;
    let foreground = color_rgb(color);
    let background = color_rgb(background);
    std::array::from_fn(|index| foreground[index] * alpha + background[index] * (1.0 - alpha))
}

fn relative_luminance(rgb: [f64; 3]) -> f64 {
    0.2126 * linear_channel(rgb[0])
        + 0.7152 * linear_channel(rgb[1])
        + 0.0722 * linear_channel(rgb[2])
}

/// WCAG contrast ratio between two already-composited (post-alpha) colors. Shared with the
/// clipboard smoke runner, which measures legend text against the surface it is drawn on.
pub(crate) fn contrast_ratio(foreground: [f64; 3], background: [f64; 3]) -> f64 {
    let foreground = relative_luminance(foreground);
    let background = relative_luminance(background);
    (foreground.max(background) + 0.05) / (foreground.min(background) + 0.05)
}

/// Contrast of a token composited over `background` against that background: the question "does this
/// surface read as a surface on top of that one" (a translucent token is composited first).
/// Used by the assertions that pin surface-on-surface relationships the palette must keep.
/// Composite a token over another, for chained surfaces (a translucent cap over a panel, say).
#[cfg(test)]
pub(crate) fn composite_over(foreground: u32, background: u32) -> [f64; 3] {
    composite_rgb(foreground, background)
}

/// A translucent token flattened onto an opaque `base`, as an opaque token.
///
/// Panels own their text surfaces, so a chip that carries text cannot take its colour from whatever
/// the panel material happens to cover: the flattened value is backdrop-independent and keeps exactly
/// the RGB the translucent token produced over `base`, which is what the contrast table measured.
pub(crate) fn flatten_token(token: u32, base: u32) -> u32 {
    let channels = composite_rgb(token, base);
    let round = |value: f64| (value.clamp(0.0, 255.0)).round() as u32;
    (round(channels[0]) << 24) | (round(channels[1]) << 16) | (round(channels[2]) << 8) | 0xFF
}

/// Composite a token over an already-composited surface (the next link of an alpha chain).
#[cfg(test)]
pub(crate) fn composite_on(foreground: u32, surface: [f64; 3]) -> [f64; 3] {
    let alpha = (foreground & 0xFF) as f64 / 255.0;
    let foreground = color_rgb(foreground);
    std::array::from_fn(|index| foreground[index] * alpha + surface[index] * (1.0 - alpha))
}

/// Contrast between a token drawn on an already-composited surface and that surface: the
/// text-on-a-surface case, where the surface is the *result* of the alpha chain.
#[cfg(test)]
pub(crate) fn contrast_on(foreground: u32, surface: [f64; 3]) -> f64 {
    let alpha = (foreground & 0xFF) as f64 / 255.0;
    let foreground = color_rgb(foreground);
    let blended =
        std::array::from_fn(|index| foreground[index] * alpha + surface[index] * (1.0 - alpha));
    contrast_ratio(blended, surface)
}

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
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct UiPalette {
    pub(crate) dark: bool,
    pub(crate) window_bg: u32,
    pub(crate) sidebar_bg: u32,
    pub(crate) detail_bg: u32,
    pub(crate) card_bg: u32,
    pub(crate) card_border: u32,
    pub(crate) separator: u32,
    pub(crate) field_bg: u32,
    /// The mock's `--field-hover`: the hovered fill for buttons and fields that rest on
    /// `field_bg` normally. Light mode deepens the same warm ink a step past the sheet's
    /// .085 -- .085 composites to ~1.05:1 against the field fill, on the 1.05 hover floor
    /// with no margin; .10 clears it.
    pub(crate) field_hover: u32,
    pub(crate) primary_text: u32,
    pub(crate) secondary_text: u32,
    pub(crate) sidebar_text: u32,
    pub(crate) muted_text: u32,
    /// A settings row's description line: the mock's `--text-3`. It is deliberately NOT
    /// `muted_text`: that token is shared with version strings and subtitles, while the row
    /// description has its own value in the sheet (light #706D68, dark #8B8B93).
    pub(crate) setting_desc_text: u32,
    /// The custom scrollbar knob. A translucent neutral that must stay on the visible side of the
    /// surface in both modes: dark over a light panel, light over a dark one. A fixed black knob
    /// (as this once was) is invisible in dark mode.
    pub(crate) scroll_indicator: u32,
    pub(crate) disabled_text: u32,
    pub(crate) success_text: u32,
    pub(crate) warning_text: u32,
    pub(crate) error_text: u32,
    pub(crate) keycap_accent_bg: u32,
    pub(crate) keycap_accent_border: u32,
    pub(crate) keycap_accent_text: u32,
    pub(crate) button_bg: u32,
    pub(crate) button_text: u32,
    pub(crate) footer_button_bg: u32,
    /// The footer action's own hover step, so it deepens its surface like every other button
    /// instead of taking the row-hover wash (`hover_bg`), whose tint is a different material.
    pub(crate) footer_button_hover: u32,
    pub(crate) accent_text: u32,
    /// Ink drawn on an `accent` fill (primary button, active accent chips). `accent_text` stays
    /// the ink for the destructive fill, the badge scrim and the close chip, which are not
    /// accent surfaces.
    pub(crate) accent_ink: u32,
    /// The frost panel's 1pt rim: a bright line reads as a glass edge, where the dark
    /// `card_border` stroke reads as a drawn rectangle.
    pub(crate) glass_rim: u32,
    /// The selected sidebar row: a flat tinted fill (`--nav-active-bg`) with its own ink
    /// (`--nav-active-ink`), the mock's `.nav-item.active`. It replaces accent-on-`selection_bg`
    /// -- the flat tint reads as a nav item, the wash read as a selection.
    pub(crate) nav_active_bg: u32,
    pub(crate) nav_active_ink: u32,
    pub(crate) switch_on_disabled_track: u32,
    pub(crate) switch_off_track: u32,
    pub(crate) switch_off_disabled_track: u32,
    pub(crate) switch_knob: u32,
    pub(crate) selection_bg: u32,
    /// The switcher's icon plate: the surface a selected icon rides on. Heavier than the row
    /// wash above, because it has to read as a filled plate at the size of an icon rather than as
    /// a row highlight, and because the panel behind it is translucent glass -- `selection_bg`'s
    /// light-mode 8% composites to almost nothing there, which left the icon's own white
    /// background indistinguishable from the panel.
    pub(crate) selection_plate_bg: u32,
    pub(crate) hover_bg: u32,
    pub(crate) accent: u32,
    pub(crate) accent_hover: u32,
    /// Body text colored as a link: clipboard URL rows, their detail view, and settings
    /// external links. Links are text, not an accent fill, so this role carries the 4.5:1 text
    /// floor that `accent` -- a 3:1 non-text role, 3.40:1 on the light window -- cannot; a
    /// fixed link blue once composited to 1.88:1 in dark mode. The one remaining accent-as-text
    /// site is the selected sidebar title, a documented design-style §3.3 exception (state
    /// text on the selection wash, held to 2.5:1).
    pub(crate) link_text: u32,
    /// Hover counterpart of `link_text`, related to it as `accent_hover` is to `accent`:
    /// darker in light mode, brighter in dark mode. `accent_hover` cannot serve here -- in
    /// light mode it measures 4.03:1 as text and is *lighter* than `link_text`, so hover
    /// would reduce contrast instead of strengthening it.
    pub(crate) link_text_hover: u32,
    pub(crate) destructive: u32,
    pub(crate) destructive_hover: u32,
    pub(crate) symbol_shadow: u32,
    pub(crate) badge_scrim: u32,
    pub(crate) shadow: u32,
}

/// Palette for the native settings window and other custom UI surfaces.
pub(crate) fn ui_palette() -> UiPalette {
    ui_palette_for_mode(resolved_is_dark())
}

pub(crate) fn ui_palette_for_mode(dark: bool) -> UiPalette {
    if dark {
        UiPalette {
            dark: true,
            // The design sheet's dark surfaces: #121215 page, #17171b sidebar, and the raised
            // card at rgba(32,32,37,.92) over the page.
            window_bg: 0x121215FF,
            sidebar_bg: 0x17171BFF,
            detail_bg: 0x121215FF,
            // rgba(32,32,37,.92) composited over the #121215 page, for the same reason.
            card_bg: 0x1F1F24FF,
            card_border: 0xFFFFFF17,
            separator: 0xFFFFFF0F,
            // .055 is the sheet's value, but the same alpha separates far less on a near-black
            // card than on a white one (measured on screenshots: 5.5% white over #1F1F24 moved
            // 12/255 and read as "no background"), so dark mode takes the next step up that keeps
            // the control visibly its own surface: 8.6% white, and hover two steps past it.
            field_bg: 0xFFFFFF1A,
            field_hover: 0xFFFFFF26,
            primary_text: 0xF4F4F5FF,
            secondary_text: 0xC7C7CCFF,
            sidebar_text: 0x9E9EA6FF,
            muted_text: 0x9E9EA6FF,
            setting_desc_text: 0x8B8B93FF,
            scroll_indicator: 0xFFFFFF59,
            disabled_text: 0x7C7C84FF,
            success_text: 0x30D158FF,
            warning_text: 0xFF9F0AFF,
            error_text: 0xFF6961FF,
            keycap_accent_bg: 0x0A84FF38,
            keycap_accent_border: 0x0A84FFB0,
            keycap_accent_text: 0xF8F9FAFF,
            button_bg: 0x2C2C2EFF,
            button_text: 0xF5F5F7FF,
            footer_button_bg: 0x2C2C2EFF,
            footer_button_hover: 0x3A3A3CFF,
            accent_text: 0xFFFFFFFF,
            accent_ink: 0x08111FFF,
            glass_rim: 0xFFFFFF17,
            nav_active_bg: 0x2A4A7DFF,
            nav_active_ink: 0xF4F4F5FF,
            switch_on_disabled_track: 0x0A84FF73,
            switch_off_track: 0xFFFFFF2B,
            switch_off_disabled_track: 0x63636673,
            switch_knob: 0xF5F5F7F5,
            selection_bg: 0x5B9BFF24,
            selection_plate_bg: 0x5B9BFF47,
            hover_bg: 0xFFFFFF1A,
            accent: 0x5B9BFFFF,
            accent_hover: 0x7DB2FFFF,
            link_text: 0x409CFFFF,
            link_text_hover: 0x66ADFFFF,
            // A step darker than the system red: white text on #FF453A computes 3.41:1, under
            // the 3.5:1 hard floor; #F53B30 reaches 3.78:1 and keeps the sheet's hue.
            destructive: 0xF53B30FF,
            destructive_hover: 0xD93630FF,
            symbol_shadow: 0x000000B3,
            badge_scrim: 0x0000008C,
            shadow: 0x00000059,
        }
    } else {
        UiPalette {
            dark: false,
            // The design sheet's light surfaces: #ececec page, #f5f5f5 sidebar, and the raised card
            // at rgba(255,255,255,.66) over the page. Text is the sheet's warm near-black, not a
            // neutral grey: #1d1c1a / #4f4c48 / #706d68.
            window_bg: 0xECECECFF,
            sidebar_bg: 0xF5F5F5FF,
            detail_bg: 0xECECECFF,
            // rgba(255,255,255,.66) composited over the #ececec page: opaque on purpose, because
            // some surfaces (keycap chips) must be a real fill rather than a wash.
            card_bg: 0xF9F9F9FF,
            card_border: 0x2D28241A,
            separator: 0x2D282412,
            field_bg: 0x3C342E0E,
            field_hover: 0x3C342E1A,
            primary_text: 0x1D1C1AFF,
            secondary_text: 0x4F4C48FF,
            sidebar_text: 0x6B6862FF,
            muted_text: 0x6B6862FF,
            setting_desc_text: 0x706D68FF,
            scroll_indicator: 0x00000059,
            // Two steps darker than the sheet's disabled grey: #9b9ba2 measured 2.34:1 and
            // #93909a 2.65:1 on the #ececec page -- both under the 3.5:1 hard floor every text
            // role must clear (§3.3). #7e7b83 is the lightest of the family that computes >= 3.5
            // on both `window_bg` (3.52:1) and `card_bg` (3.95:1).
            disabled_text: 0x7E7B83FF,
            success_text: 0x176B3AFF,
            warning_text: 0xA63D0AFF,
            error_text: 0xB42318FF,
            keycap_accent_bg: 0x0A84FF38,
            keycap_accent_border: 0x0A84FFB0,
            keycap_accent_text: 0x2C2C30FF,
            button_bg: 0xFFFFFFFF,
            button_text: 0x2C2C30FF,
            footer_button_bg: 0xFFFFFFFF,
            footer_button_hover: 0xF0F0F0FF,
            accent_text: 0xFFFFFFFF,
            accent_ink: 0xFFFFFFFF,
            glass_rim: 0xFFFFFF80,
            nav_active_bg: 0xD7E3F8FF,
            nav_active_ink: 0x173A6BFF,
            switch_on_disabled_track: 0x0A84FF73,
            switch_off_track: 0x3C342E33,
            switch_off_disabled_track: 0xC7C7CC73,
            switch_knob: 0xFFFFFFF5,
            selection_bg: 0x2F6FED1A,
            selection_plate_bg: 0x2F6FED47,
            hover_bg: 0x7676801F,
            accent: 0x2F6FEDFF,
            accent_hover: 0x1F5CDAFF,
            link_text: 0x1F5CDAFF,
            link_text_hover: 0x17439FFF,
            destructive: 0xFF3B30FF,
            destructive_hover: 0xD70015FF,
            symbol_shadow: 0x000000B3,
            badge_scrim: 0x0000008C,
            shadow: 0x0000000F,
        }
    }
}

/// The custom palette as stored in the configuration: the two `ThemeColors` structs, kept as
/// strings so a snapshot stays a cheap clone.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CustomPalette {
    pub(crate) dark: config::ThemeColors,
    pub(crate) light: config::ThemeColors,
}

/// Every input the pure theme and layout layer takes from the configuration, as ONE snapshot.
///
/// One transaction takes one `ThemeInput` and keeps it: a single layout or refresh pass must not
/// observe two different configurations, and the lock it was copied under is released before any
/// AppKit work.
///
/// The field list is closed, not a convenient subset. It is every field the layer read through
/// `theme.rs`: 14 `CONFIG.read()` sites (one of them, in a geometry test helper, reads only the
/// corner radius) plus `text_scale()`, whose `CONFIG` and `.read()` are split across lines, for 15
/// lock-taking sites in all. Field by field:
///
/// - `appearance.theme` -> `dark` (one of "light" / "dark" / "auto"; "auto" resolves through the
///   system appearance, see [`ThemeInput::current`]).
/// - `colors.dark.*` and `colors.light.*` -> `palette`, each struct's complete field list:
///   `status_bar_text`, `app_name`, `win_title`, `icon_inner_bg`, `icon_text`, `card_bg_sel`,
///   `card_border_sel`. (`colors_from` reads `win_title` for both the footer text and the card
///   title and never reads `status_bar_text`; both are inputs of the snapshot regardless.)
/// - `appearance.corner_radius` -> `corner_radius`.
/// - `layout.card_text_size` -> `card_text_size`.
/// - `layout.thumbnail_size` -> `thumbnail_size` (parsed by `config::thumbnail_scale_of`).
/// - `layout.thumbnails_enabled` -> `thumbnails_enabled`.
/// - `layout.show_app_name_in_cards` -> `show_app_name_in_cards`.
/// - `fonts.status_bar_size` -> `status_bar_font_size` (the footer text size, which is also what
///   decides whether a footer band exists at all).
/// - `fonts.title_size` -> `title_font_size`.
/// - `fonts.app_name_size` -> `app_name_font_size`.
///
/// Not an input: `layout.show_footer_title`, which nothing here reads. The band follows
/// `fonts.status_bar_size` (see [`footer_band_h`]); the stored switch is carried for older configs
/// and the export format only.
///
/// `ThemeInput::current()` is the single transitional entry point that still reads the global. Its
/// replacement is each entry taking one snapshot when its transaction starts and passing it down,
/// and its exit condition is narrow: only the pure theme, layout and description layers are barred
/// from reading the global -- settings event handlers and refresh entries may keep reading the
/// business config they act on.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ThemeInput {
    pub(crate) dark: bool,
    pub(crate) palette: CustomPalette,
    pub(crate) corner_radius: f64,
    pub(crate) card_text_size: f64,
    pub(crate) thumbnail_size: String,
    pub(crate) thumbnails_enabled: bool,
    pub(crate) show_app_name_in_cards: bool,
    pub(crate) title_font_size: f64,
    pub(crate) app_name_font_size: f64,
    pub(crate) status_bar_font_size: f64,
}

impl ThemeInput {
    /// The transition entry point: one snapshot of the current configuration.
    ///
    /// Every caller of the pure layer goes through here today; no pure function reads `CONFIG`
    /// itself. The exit condition is narrow: only the pure theme, layout and description layers are
    /// barred from reading the global -- the settings event handlers and refresh entries keep
    /// reading the business config they act on. What replaces this function is each entry taking one
    /// snapshot when its transaction starts and passing it down, so nothing here is a destination.
    pub(crate) fn current() -> ThemeInput {
        // One statement, one guard: the clone completes before `from_config` runs, so no caller can
        // hold a config lock across the AppKit query inside it.
        let config = CONFIG.read().unwrap().clone();
        Self::from_config(&config)
    }

    /// The same snapshot, taken from a config the caller already owns.
    ///
    /// A caller that has just read `CONFIG` must not read it a second time: a nested read of a
    /// `std::sync::RwLock` parks when a writer is queued between the two, and the outer guard is
    /// then held by the thread waiting for itself. This constructor also keeps the AppKit query
    /// below outside every config lock.
    pub(crate) fn from_config(cfg: &crate::config::Config) -> ThemeInput {
        let palette = CustomPalette {
            dark: cfg.colors.dark.clone(),
            light: cfg.colors.light.clone(),
        };
        let theme = cfg.appearance.theme.clone();
        let corner_radius = cfg.appearance.corner_radius;
        let card_text_size = cfg.layout.card_text_size;
        let thumbnail_size = cfg.layout.thumbnail_size.clone();
        let thumbnails_enabled = cfg.layout.thumbnails_enabled;
        let show_app_name_in_cards = cfg.layout.show_app_name_in_cards;
        let title_font_size = cfg.fonts.title_size;
        let app_name_font_size = cfg.fonts.app_name_size;
        let status_bar_font_size = cfg.fonts.status_bar_size;
        // Resolved here, with no lock held: "auto" asks NSUserDefaults, and AppKit work must never
        // run while the config is locked.
        ThemeInput {
            dark: dark_for_theme(&theme),
            palette,
            corner_radius,
            card_text_size,
            thumbnail_size,
            thumbnails_enabled,
            show_app_name_in_cards,
            title_font_size,
            app_name_font_size,
            status_bar_font_size,
        }
    }
}

/// Resolve `appearance.theme` to the effective appearance; "auto" (and anything unknown) follows
/// the system.
fn dark_for_theme(theme: &str) -> bool {
    match theme {
        "light" => false,
        "dark" => true,
        _ => system_dark_mode(),
    }
}

/// Resolve colors from the snapshot for dark/light. Fixed fields (page bg, etc.) are hard-coded
/// to transparent / placeholder for now.
pub(crate) fn colors_from(input: &ThemeInput, dark: bool) -> Colors {
    let c = if dark {
        &input.palette.dark
    } else {
        &input.palette.light
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
    let theme = CONFIG.read().unwrap().appearance.theme.clone();
    dark_for_theme(&theme)
}

/// The custom overlay colors for the current appearance, from one snapshot.
pub(crate) fn current_colors() -> Colors {
    let input = ThemeInput::current();
    colors_from(&input, input.dark)
}

pub(crate) fn card_w() -> f64 {
    ICON_CARD_W
}
/// The icon-only card's height: exactly what its content needs, with no slack. The previous
/// form added a fixed reserve on top of a base height that already exceeded its content, which
/// left ~27pt of blank inside the card's own bounds -- the plate ended up crammed 2pt above the
/// caption while a band of nothing sat at the card's bottom.
pub(crate) fn card_h(input: &ThemeInput) -> f64 {
    let (primary_line_h, secondary_line_h) = icon_caption_line_heights(input);
    ICON_TOP_INSET
        + icon_px()
        + ICON_TEXT_GAP
        + primary_line_h
        + ICON_LINE_GAP
        + secondary_line_h
        + ICON_BOTTOM_INSET
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

/// The icon-only selection plate in card-local points (x, y, w, h), with y measured from
/// the card's bottom (AppKit's non-flipped origin). It wraps the *whole card* -- icon and both
/// caption lines -- `SELECTION_CARD_INSET` outside it, the same unit of selection the thumbnail
/// layout's ring marks.
pub(crate) fn icon_selection_plate(card_w: f64, card_h: f64) -> (f64, f64, f64, f64) {
    (
        -SELECTION_CARD_INSET,
        -SELECTION_CARD_INSET,
        card_w + SELECTION_CARD_INSET * 2.0,
        card_h + SELECTION_CARD_INSET * 2.0,
    )
}

/// The icon-only plate's corner radius.
///
/// Its own token rather than the card's radius: the card paints nothing (the plate carries the fill
/// and the rim), so there is no second visible curve for it to stay parallel to, and
/// `appearance.corner_radius` is the *card's* radius (capped at `RADIUS_PANEL`) plus the panel's.
/// `24` is the icon it wraps: an app icon's visible artwork is ~88.5pt with a corner radius of
/// `88.5 * 0.2597 = 23`, and its canvas is `110 * 0.223 = 24.5` on the measured circular
/// equivalent -- so the rim runs at the artwork's own curvature instead of a boxier 16.
pub(crate) const SELECTION_PLATE_RADIUS: f64 = 24.0;

pub(crate) fn icon_selection_plate_radius() -> f64 {
    SELECTION_PLATE_RADIUS
}

/// The icon-only card's own vertical layout, shared by the card builder and the hover target,
/// so the pointer-to-select area cannot drift from what the card actually draws. Card-local
/// points, y measured from the card's bottom (AppKit's non-flipped origin).
pub(crate) struct IconCardLayout {
    pub(crate) icon_x: f64,
    pub(crate) icon_bottom: f64,
    pub(crate) primary_line_h: f64,
    pub(crate) secondary_line_h: f64,
    /// Bottom of the window-title line.
    pub(crate) primary_bottom: f64,
    /// Bottom of the app-name line: the lowest ink the card draws.
    pub(crate) secondary_bottom: f64,
}

/// The two caption lines' heights, from the configured font sizes.
fn icon_caption_line_heights(input: &ThemeInput) -> (f64, f64) {
    (
        line_height(card_title_font_size(input), LINE_HEIGHT_TITLE_RATIO),
        line_height(card_app_name_font_size(input), LINE_HEIGHT_CAPTION_RATIO),
    )
}

pub(crate) fn icon_card_layout(input: &ThemeInput, card_w: f64, card_h: f64) -> IconCardLayout {
    let icon_bottom = card_h - ICON_TOP_INSET - icon_px();
    let (primary_line_h, secondary_line_h) = icon_caption_line_heights(input);
    let primary_bottom = icon_bottom - ICON_TEXT_GAP - primary_line_h;
    IconCardLayout {
        icon_x: (card_w - icon_px()) / 2.0,
        icon_bottom,
        primary_line_h,
        secondary_line_h,
        primary_bottom,
        secondary_bottom: primary_bottom - ICON_LINE_GAP - secondary_line_h,
    }
}

#[cfg(test)]
mod selection_plate_tests {
    use super::*;

    /// The icon-only plate wraps the whole card: the same unit of selection the thumbnail
    /// layout's ring marks (caption rows included), `SELECTION_CARD_INSET` outside it.
    #[test]
    fn icon_plate_wraps_the_whole_card() {
        let input = ThemeInput::current();
        let card_w = card_w();
        let card_h = card_h(&input);
        let (x, y, w, h) = icon_selection_plate(card_w, card_h);
        assert!((x - -SELECTION_CARD_INSET).abs() < 1e-9);
        assert!((y - -SELECTION_CARD_INSET).abs() < 1e-9);
        assert!((w - (card_w + SELECTION_CARD_INSET * 2.0)).abs() < 1e-9);
        assert!((h - (card_h + SELECTION_CARD_INSET * 2.0)).abs() < 1e-9);
        // The plate's own token, decoupled from the card radius: `appearance.corner_radius` stays
        // the card's and the panel's knob.
        assert_eq!(icon_selection_plate_radius(), SELECTION_PLATE_RADIUS);
        assert_ne!(
            SELECTION_PLATE_RADIUS,
            overlay_card_radius(&input) + SELECTION_CARD_INSET
        );
        // The caption rows and the icon canvas are all inside it.
        let layout = icon_card_layout(&input, card_w, card_h);
        // At the card's own edge, not outside it: the icon grid packs cards edge to edge, so the
        // card boundary is already the midpoint between two icons.
        assert!(
            (y - 0.0).abs() < 1e-9 && (y + h - card_h).abs() < 1e-9,
            "exactly the card"
        );
        assert!(layout.secondary_bottom > y, "the app-name line is inside");
    }
}

/// Scale shared by the card's window-title and app-name text.
pub(crate) fn text_scale(input: &ThemeInput) -> f64 {
    input
        .card_text_size
        .clamp(CARD_TEXT_SIZE_MIN, CARD_TEXT_SIZE_MAX)
        / CARD_TEXT_BASE_SIZE
}

pub(crate) fn card_title_font_size(input: &ThemeInput) -> f64 {
    overlay_text_size_for(input.title_font_size, input.card_text_size)
}

pub(crate) fn card_app_name_font_size(input: &ThemeInput) -> f64 {
    overlay_text_size_for(input.app_name_font_size, input.card_text_size)
}

/// Bottom title-bar text size, exposed through the switcher settings page.
pub(crate) fn status_bar_text_size(input: &ThemeInput) -> f64 {
    status_bar_text_size_for(input.status_bar_font_size)
}

/// The legal footer-size stops as a stored value lands on them: the settings slider's domain is
/// 0 plus 13..=20pt integers, so anything else snaps to the nearest stop -- below the 6.5 midpoint
/// the hidden stop, otherwise the nearest integer clamped into 13..=20. `show_footer_title`,
/// `status_bar_text_size` and the slider readout all consult this one rule, so a hand-edited
/// `13.6` renders at the 14pt the settings page reports.
pub(crate) fn normalize_status_bar_size(size: f64) -> f64 {
    if !size.is_finite() || size < 6.5 {
        0.0
    } else {
        size.round().clamp(13.0, 20.0)
    }
}

fn status_bar_text_size_for(configured_size: f64) -> f64 {
    // 0 is not a text size: it is the "no footer" stop. Everything above 0 is a real size, so 1..12
    // must render at 1..12 rather than silently snapping to 13 while the slider's readout says so.
    //
    // 0 itself resolves to the base size rather than to a 1pt clamp: the empty-overlay hint is drawn
    // through this same accessor even when the footer band is gone (it is a state message, not a window
    // title, and `update_status_label` returns it before the visibility check), so clamping it to 1pt
    // would make that hint unreadable.
    let size = normalize_status_bar_size(configured_size);
    if size <= 0.0 {
        STATUS_BAR_TEXT_BASE_SIZE
    } else {
        size
    }
}

fn overlay_text_size_for(base_size: f64, card_text_size: f64) -> f64 {
    let base_size = if base_size.is_finite() && base_size > 0.0 {
        base_size
    } else {
        CARD_TEXT_BASE_SIZE
    };
    let card_text_size = if card_text_size.is_finite() {
        card_text_size
    } else {
        CARD_TEXT_BASE_SIZE
    };
    (base_size * card_text_size / CARD_TEXT_BASE_SIZE).clamp(13.0, 20.0)
}

/// The corner-safe inset for a control in the footer band's trailing corner.
///
/// The gear sits in the panel's bottom-right rounded corner, so the button's own bottom-right corner
/// must stay inside the corner arc. The model this follows: a rounded-rect path clamps its radius
/// PER AXIS to that axis's half-extent, i.e. `rx = min(r, panel_w / 2)` and
/// `ry = min(r, panel_h / 2)`, so the corner centre is at `(W - rx, ry)` and the button's
/// bottom-right corner at `(W - X, m)` is inside the corner when
/// `((rx - X) / rx)^2 + ((ry - m) / ry)^2 <= 1`, i.e.
/// `X_min = rx * (1 - sqrt(1 - ((ry - m) / ry)^2))` for `m < ry` (and 0 otherwise).
///
/// EVIDENCE BOUNDARY. The per-axis clamp and the formula are what this function implements, and the
/// unit tests below pin THAT. What no test here observes is the shipped panels' actual clipped
/// shape: the frost path draws a template then stretches it through cap insets, the glass path uses
/// `setCornerRadius:`, and reading a view's `cornerRadius`/mask back does not say what was
/// clipped. Until an assertion measures a real panel's corner (and a deliberately overshooting
/// counter-example), treat this as the model the layout is computed against, not as a verified
/// statement about every material. Note also that `rx` and `ry` clamp independently: with a panel
/// wider than it is tall, `ry` caps while `rx` keeps growing, so "a radius above half the height
/// behaves like one equal to it" is NOT true in general -- only each axis on its own.
///
/// The control is vertically CENTRED in the band, so `m = (band_h - control_h) / 2`. Centring is
/// load-bearing: `ry - m` then grows with the band, so raising the band lowers `X_min`. A fixed `m`
/// would raise `X_min` as the panel grows, inverting the fallback.
///
/// The `inset` clamps to `[0, panel_w / 2]`: a caller cannot be told to reserve more than half the
/// panel, which is what keeps the reserve symmetric and the footer text's centre on the panel's.
pub(crate) fn footer_control_inset(
    corner_radius: f64,
    panel_w: f64,
    panel_h: f64,
    control_h: f64,
    band_h: f64,
) -> f64 {
    let rx = effective_corner_extent(corner_radius, panel_w);
    let ry = effective_corner_extent(corner_radius, panel_h);
    if rx <= 0.0 || ry <= 0.0 {
        return 0.0;
    }
    let m = ((band_h - control_h) / 2.0).max(0.0);
    if m >= ry {
        return 0.0;
    }
    let ratio = (ry - m) / ry;
    let x = rx * (1.0 - (1.0 - ratio * ratio).max(0.0).sqrt());
    if x.is_finite() {
        x.max(0.0).min(panel_w / 2.0)
    } else {
        0.0
    }
}

/// One axis's effective corner extent: AppKit clamps a rounded rect's radius to half that axis.
fn effective_corner_extent(corner_radius: f64, axis: f64) -> f64 {
    if !corner_radius.is_finite() || !axis.is_finite() || axis <= 0.0 {
        return 0.0;
    }
    corner_radius.max(0.0).min(axis / 2.0)
}

/// Minimum footer band height that holds the trailing settings control with its vertical margin.
/// The band otherwise follows the footer text size, which bottoms out at a 28pt band (13pt footer)
/// where a 28pt control has no margin at all; the floor gives it the 4pt grid's breathing room.
pub(crate) const FOOTER_CONTROL_MIN_H: f64 = 36.0;
/// Side of the footer's square settings control.
pub(crate) const FOOTER_CONTROL_SIZE: f64 = 28.0;
/// Gap between the settings control and the footer text's reserved area.
pub(crate) const FOOTER_CONTROL_GAP: f64 = 8.0;
/// Bottom margin of the settings control when the footer title is off and the band is gone.
///
/// The control then anchors to the panel's bottom-right corner arc instead of being centred in a
/// band. At the shipped 32pt corner radius this margin is what keeps the control's trailing edge
/// clear of the card area's own right inset (`H_PADDING`), so a full-width row cannot slide under it.
pub(crate) const FOOTER_FLOAT_MARGIN: f64 = 16.0;

// Test-only override of the corner radius the layout geometry uses.
//
// Kept because a test that exercises two radii from one snapshot is clearer than two snapshots; it
// is read ONLY when a test sets it, so production geometry is always the snapshot's own radius.
#[cfg(test)]
thread_local! {
    static TEST_CORNER_RADIUS: std::cell::Cell<Option<f64>> = const { std::cell::Cell::new(None) };
}

/// Pin the corner radius used by layout geometry for the current test thread.
#[cfg(test)]
pub(crate) fn set_test_corner_radius(radius: Option<f64>) {
    TEST_CORNER_RADIUS.with(|cell| cell.set(radius));
}

/// The corner radius that layout geometry must be computed against.
pub(crate) fn layout_corner_radius(input: &ThemeInput) -> f64 {
    #[cfg(test)]
    {
        if let Some(forced) = TEST_CORNER_RADIUS.with(|cell| cell.get()) {
            return forced;
        }
    }
    input.corner_radius
}

/// The footer band's height with the test thread's pin applied (`set_test_footer_band`).
///
/// Production code calls [`footer_band_h`]; a test that needs "no band" for geometry it does not
/// otherwise describe can pin it instead of handing its snapshot a footer size.
pub(crate) fn footer_band_h_test_pinned(input: &ThemeInput) -> f64 {
    #[cfg(test)]
    {
        if let Some(forced) = TEST_FOOTER_BAND.with(|cell| cell.get()) {
            return forced;
        }
    }
    footer_band_h(input)
}

// Test-only override of the footer band height.
//
// The suite's geometry fixtures take the band from their own snapshot; this exists because a test
// that only needs "band absent" says so with one line instead of building a second snapshot. It is
// read ONLY when a test sets it.
#[cfg(test)]
thread_local! {
    static TEST_FOOTER_BAND: std::cell::Cell<Option<f64>> = const { std::cell::Cell::new(None) };
}

/// Pin the footer band height for the current test thread (`None` restores the snapshot's value).
#[cfg(test)]
pub(crate) fn set_test_footer_band(band: Option<f64>) {
    TEST_FOOTER_BAND.with(|cell| cell.set(band));
}

/// The footer band's height, with the floor the settings control needs. Every consumer of the band
/// (the card viewport, the document, the scroller, the footer text and the control) must use THIS,
/// or the control and the viewport disagree about where the band starts.
///
/// With the title turned off the band disappears: the 36pt floor exists to hold the settings
/// control, not the text (15pt text needs 33.5pt, below that floor), so keeping any band would keep
/// an empty strip. The control moves to the panel's corner instead -- see
/// `overlay::footer_settings_frame` -- and every consumer follows this function, so the panel, the
/// card viewport and the scroller all shrink by the band together.
pub(crate) fn footer_band_h(input: &ThemeInput) -> f64 {
    footer_band_h_for(show_footer_title(input), status_bar_text_size(input))
}

/// The band height for one switch state and one footer text size (pure): the floor the settings
/// control needs, or nothing at all while the title is off.
///
/// The text band grows with the footer text, and its top `THUMB_BOTTOM_INSET` belongs to the card
/// viewport instead: the viewport ends where the band starts and the selected card draws outside its
/// own bounds (see `THUMB_BOTTOM_INSET`). Taking that room from the band's own top padding rather than
/// from the panel height is what keeps the panel's footprint -- and with it the automatic card-size
/// step and the row budget -- what it was before the inset existed.
fn footer_band_h_for(show_title: bool, footer_text_size: f64) -> f64 {
    if !show_title {
        return 0.0;
    }
    let size = status_bar_text_size_for(footer_text_size);
    (status_bar_height_for_text_size(size) - THUMB_BOTTOM_INSET).max(FOOTER_CONTROL_MIN_H)
}

/// The footer text's line height, clamped into the footer band when there is one.
///
/// The clamp exists so the line cannot outgrow the band. With the title switched off the band is zero
/// and the label may still carry the empty-state hint, so `clamp(11.0, band_h)` would be
/// `clamp(11.0, 0.0)` -- which panics (min > max) instead of clamping, on the plain empty-overlay
/// path. The upper bound is therefore applied only when a band exists.
pub(crate) fn footer_line_height(natural_h: f64, band_h: f64) -> f64 {
    if band_h > 0.0 {
        natural_h.clamp(11.0, band_h)
    } else {
        natural_h.max(11.0)
    }
}

/// How much room above the panel's bottom edge the trailing column must leave free for the settings
/// control, beyond the footer band.
///
/// Zero while the band exists (the control lives in it). With the title off the control floats over
/// the panel's bottom-right corner, in the same trailing column as the overflow scrollbar, and it is
/// added to the view hierarchy *before* the scrollbar -- so a shared area would hand the control's
/// clicks to the scrollbar. The scrollbar's track starts above this reserve instead of at the panel
/// edge, which keeps the two hit areas disjoint without relying on z-order.
pub(crate) fn footer_control_bottom_reserve(input: &ThemeInput) -> f64 {
    if footer_band_h(input) > 0.0 {
        return 0.0;
    }
    FOOTER_FLOAT_MARGIN + FOOTER_CONTROL_SIZE + FOOTER_CONTROL_GAP
}

/// Horizontal room the floating settings control takes out of the card grid when the footer title is
/// off, so no card can slide under it: the control is drawn on top and would take that part of the
/// card's clicks, and a larger configured corner radius pushes it further in.
///
/// Zero while the band exists (the control is inside it). Also zero while the intrusion stays under a
/// point -- at the shipped 32pt radius it is 0.3pt, and re-deriving row packing for a sub-point would
/// perturb the layout for no visible gain. Measured against the widest allowed panel, so it is never
/// an underestimate for the panel the layout finally picks.
pub(crate) fn footer_control_right_reserve(
    input: &ThemeInput,
    max_panel_w: f64,
    max_panel_h: f64,
) -> f64 {
    // The pin-aware read, as layout_corner_radius already honours its own pin: a suite that pins
    // "no band" must get the reserve a band-less panel needs. Outside a test the two are the same
    // function, so production geometry is unchanged.
    if footer_band_h_test_pinned(input) > 0.0 {
        return 0.0;
    }
    footer_control_right_reserve_for(layout_corner_radius(input), max_panel_w, max_panel_h)
}

/// The pure half of [`footer_control_right_reserve`]: the reserve a given corner radius implies.
///
/// Separate from the config read so the boundary can be asserted without a configured radius: at the
/// shipped 32pt the arc-derived inset still fits inside `H_PADDING`, and a radius large enough to push
/// the control past it must produce a reserve that grows with the radius.
fn footer_control_right_reserve_for(radius: f64, max_panel_w: f64, max_panel_h: f64) -> f64 {
    let inset = footer_control_inset(
        radius,
        max_panel_w,
        max_panel_h,
        FOOTER_CONTROL_SIZE,
        FOOTER_CONTROL_SIZE + FOOTER_FLOAT_MARGIN * 2.0,
    );
    let intrusion = inset + FOOTER_CONTROL_SIZE - H_PADDING;
    if intrusion > 1.0 {
        intrusion
    } else {
        0.0
    }
}

/// The trailing horizontal inset reserved for the footer's settings control: the corner-safe inset
/// plus the control's own width and its gap to the text. The footer text keeps the SAME reserve on
/// both sides, so its centre stays the panel's centre.
pub(crate) fn footer_text_reserve(input: &ThemeInput, panel_w: f64, panel_h: f64) -> f64 {
    let inset = footer_control_inset(
        layout_corner_radius(input),
        panel_w,
        panel_h,
        FOOTER_CONTROL_SIZE,
        footer_band_h(input),
    )
    .max(H_PADDING);
    inset + FOOTER_CONTROL_SIZE + FOOTER_CONTROL_GAP
}

/// The footer text's frame: symmetric about the panel centre.
///
/// KNOWN LIMIT: at an extreme configured `appearance.corner_radius` the symmetric reserve can
/// consume the panel, and the `max(1.0)` floor then keeps a 1pt frame that cannot render a title.
/// The button stays inside the corner in that case, but the footer text is effectively unreadable.
/// That configuration has no agreed fallback yet (the choice between a taller band, a narrower text
/// area and hiding the entry is the user's); see `docs/review-backlog.md`. This function does NOT
/// claim a readable minimum -- the clamp only keeps the rect valid.
pub(crate) fn footer_text_frame(
    input: &ThemeInput,
    panel_w: f64,
    panel_h: f64,
    line_h: f64,
) -> (f64, f64, f64) {
    let reserve = footer_text_reserve(input, panel_w, panel_h);
    let width = (panel_w - reserve * 2.0).max(1.0);
    let x = ((panel_w - width) / 2.0).max(0.0);
    let y = (footer_band_h(input) - line_h) / 2.0;
    (x, y, width)
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
pub(crate) fn thumb_caption_h(input: &ThemeInput) -> f64 {
    (THUMB_CAPTION_H * text_scale(input)).clamp(20.0, 36.0)
}

pub(crate) fn thumb_caption_h_for_card(input: &ThemeInput, card_h: f64) -> f64 {
    thumb_caption_h(input).min((card_h.max(0.0) / 3.0).max(1.0))
}

pub(crate) fn overlay_card_radius(input: &ThemeInput) -> f64 {
    effective_overlay_card_radius(input.corner_radius)
}

pub(crate) fn effective_overlay_card_radius(configured_radius: f64) -> f64 {
    if configured_radius.is_finite() {
        configured_radius.clamp(0.0, RADIUS_PANEL)
    } else {
        RADIUS_PANEL
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct OverlayRadii {
    pub(crate) card: f64,
    pub(crate) selection_ring: f64,
    pub(crate) thumbnail_preview: f64,
    pub(crate) thumbnail_icon: f64,
    pub(crate) close_button: f64,
}

pub(crate) fn overlay_radii(card_radius: f64, thumbnail_padding: f64) -> OverlayRadii {
    let inner = (card_radius - thumbnail_padding.max(0.0)).max(0.0);
    OverlayRadii {
        card: card_radius.max(0.0),
        // The ring's frame sits `OVERLAY_RING_INSET` outside the card (see `overlay_radii`'s caller
        // and docs/design-style: the ring keeps the card's own radius rather than running parallel).
        selection_ring: card_radius.max(0.0),
        thumbnail_preview: THUMBNAIL_PREVIEW_RADIUS,
        thumbnail_icon: RADIUS_CONTROL,
        close_button: rounded_inset_radius(inner, 20.0, 20.0),
    }
}

pub(crate) fn rounded_inset_radius(radius: f64, width: f64, height: f64) -> f64 {
    radius
        .max(0.0)
        .min(width.max(0.0).min(height.max(0.0)) / 2.0)
}

pub(crate) fn reduce_motion_enabled() -> bool {
    let forced = crate::dev_flags::enabled("reduce-motion");
    let system = unsafe {
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        !workspace.is_null()
            && msg_send![workspace, respondsToSelector: objc2::sel!(accessibilityDisplayShouldReduceMotion)]
            && msg_send![workspace, accessibilityDisplayShouldReduceMotion]
    };
    !animation_allowed(system, forced)
}

/// Whether the system's Reduce Transparency accessibility setting asks apps to drop
/// backdrop blur. Queried on demand — at backdrop build time and at each panel
/// show/summon (the persistent windows sync their installed material there) — rather than
/// observed, so no permanent NSWorkspace observer is needed.
pub(crate) fn reduce_transparency_enabled() -> bool {
    let forced = crate::dev_flags::enabled("reduce-transparency");
    forced
        || unsafe {
            let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
            !workspace.is_null()
                && msg_send![workspace, respondsToSelector: objc2::sel!(accessibilityDisplayShouldReduceTransparency)]
                && msg_send![workspace, accessibilityDisplayShouldReduceTransparency]
        }
}

pub(crate) const fn animation_allowed(system_reduce_motion: bool, forced: bool) -> bool {
    !system_reduce_motion && !forced
}

pub(crate) unsafe fn ease_standard_timing_function() -> *mut AnyObject {
    extern "C" {
        fn objc_msgSend();
    }
    type MakeTimingFunction = unsafe extern "C" fn(
        *mut AnyObject,
        objc2::runtime::Sel,
        f32,
        f32,
        f32,
        f32,
    ) -> *mut AnyObject;
    let make_timing: MakeTimingFunction = std::mem::transmute(objc_msgSend as *const ());
    make_timing(
        class!(CAMediaTimingFunction) as *const _ as *mut AnyObject,
        objc2::sel!(functionWithControlPoints::::),
        ANIMATION_EASE_STANDARD.0,
        ANIMATION_EASE_STANDARD.1,
        ANIMATION_EASE_STANDARD.2,
        ANIMATION_EASE_STANDARD.3,
    )
}

pub(crate) fn animation_exit_duration(entrance: f64) -> f64 {
    entrance * ANIMATION_EXIT_RATIO
}

/// Window-thumbnail master switch (the thumbnail module additionally sleeps
/// without the Screen Recording permission).
/// The card scale the user pinned, or `None` for the automatic step. Thumbnail mode only: the
/// icon-only layout has its own fixed card size.
pub(crate) fn thumbnail_size_scale_override(input: &ThemeInput) -> Option<f64> {
    crate::config::thumbnail_scale_of(&input.thumbnail_size)
}

pub(crate) fn thumbnails_enabled(input: &ThemeInput) -> bool {
    input.thumbnails_enabled
}

/// The same switch, read straight from the configuration.
///
/// Thumbnail gating runs on capture workers, the startup observer thread and the pregen path, and
/// it needs ONE boolean from the config. Building a `ThemeInput` for that clones the whole config
/// and, for the "auto" appearance, queries NSUserDefaults -- off the main thread. This reads the
/// single field under the lock instead, which is what the pre-snapshot code did.
///
/// It is deliberately not a general shortcut: any caller that needs more than this one field takes
/// a snapshot, because reading several fields would no longer be one consistent read.
pub(crate) fn thumbnails_enabled_setting() -> bool {
    CONFIG.read().unwrap().layout.thumbnails_enabled
}

/// Whether the thumbnail card's caption prefixes the app name before the window title.
pub(crate) fn show_app_name_in_cards(input: &ThemeInput) -> bool {
    input.show_app_name_in_cards
}

/// Whether the switcher's bottom footer draws the selected window's title.
///
/// This switch also decides whether the footer band exists at all (`footer_band_h`), because the
/// band's floor is the settings control's size rather than the text's: with the title off there is
/// nothing left for the band to hold except a control that can move to the panel's corner.
pub(crate) fn show_footer_title(input: &ThemeInput) -> bool {
    // One control, one source of truth: the footer band exists while its own text size is above
    // zero, which is the settings slider's far-left stop. `layout.show_footer_title` is still stored
    // (older configs and the export configs carry it) but no longer gates the panel.
    // The slider's legal stops are 0 and 13..=20pt; normalization snaps any in-between stored
    // value the same way the settings readout does.
    normalize_status_bar_size(input.status_bar_font_size) > 0.0
}

/// Flow-layout base card width, hard-coded and fully independent of the legacy
/// icon grid's card_width: aligned with the references (BetterCmdTab effective
/// 312 / DockDoor 300 / mockup ~295).
pub(crate) const THUMB_CARD_BASE_W: f64 = 300.0;
/// Flow-layout row/inter-card gap (the mockup's .grid gap of 14px).
pub(crate) const THUMB_ROW_GAP: f64 = 14.0;
/// Width reserved for the scrollbar at the right edge of the scrolling thumbnail viewport.
pub(crate) const THUMB_SCROLLBAR_W: f64 = 14.0;
/// Fraction of the next row exposed at the bottom of an overflowing viewport. The exposed sliver is
/// this fraction of a card plus `THUMB_BOTTOM_INSET`, the card area's own inset below its last row.
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
/// Bottom inset below the card area: the blank strip the last row keeps to the status footer.
///
/// The card viewport (the clip view) ends where the footer begins, and the selected card draws
/// outside its own bounds: the ring sits `OVERLAY_RING_INSET` (3pt) out and its zero-offset glow
/// (`overlay::SELECTION_GLOW_RADIUS`, 4pt) reaches 4pt further. A flush last row therefore loses the
/// ring's bottom edge to the viewport clip -- read as "the footer band covers the selection". 3 + 4
/// = 7, rounded up to the 4pt spacing grid; `overlay` asserts that it still covers both at compile
/// time.
pub(crate) const THUMB_BOTTOM_INSET: f64 = 8.0;

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

/// Pick the card step for the automatic size: the largest candidate whose **count-based** estimate
/// fits the panel, falling back to the smallest step (scrolling) when nothing fits.
///
/// A pinned size skips the search entirely and always uses its own step: the user asked for that
/// card size, so the panel wraps and scrolls instead of shrinking it.
///
/// The estimate is order-independent on purpose. The previous version laid the real packing out at
/// each candidate step, and `pack_rows` minimises leftover width *in input order*, so the same set
/// of windows could need three rows in one MRU order and four in another -- measured 2026-10-07:
/// eleven identical windows chose 1.10 (248pt) when a maximized window was frontmost and 1.00
/// (230pt) when a narrow one was. Card size must not depend on which window happened to be in
/// front, so the estimate uses the count and the **widest** card only.
pub(crate) fn thumb_scale_for_panel(
    input: &ThemeInput,
    aspects: &[f64],
    max_inner: f64,
    max_panel_h: f64,
    gap: f64,
    pinned: Option<f64>,
) -> f64 {
    if let Some(scale) = pinned {
        return scale.max(THUMB_MIN_SCALE);
    }
    if aspects.is_empty() {
        return 1.0;
    }
    for &scale in THUMB_SCALE_STEPS.iter() {
        let card_h = thumb_card_h_for_scale(input, scale);
        if thumb_count_estimate_fits(input, aspects.len(), card_h, max_inner, max_panel_h, gap) {
            return scale;
        }
    }
    THUMB_MIN_SCALE
}

/// Whether the panel can hold `count` cards at this card height, decided from the count and the
/// panel budget **alone**.
///
/// The reference card is the base preview ratio, not the widest window in the set: window shapes and
/// the set-wide width cap must not move the automatic step, or the same number of windows would
/// change size when one of them was resized or maximized. They still decide how the cards wrap, so a
/// set whose windows are much wider than the base ratio can need a row more than this estimate and
/// the panel scrolls at the chosen size -- the pinned percent is the way to guarantee both.
fn thumb_count_estimate_fits(
    input: &ThemeInput,
    count: usize,
    card_h: f64,
    max_inner: f64,
    max_panel_h: f64,
    gap: f64,
) -> bool {
    let reference_w = thumb_card_w_for_aspect(input, card_h, THUMB_PREVIEW_RATIO);
    if reference_w <= 0.0 {
        return true;
    }
    let per_row = (((max_inner + gap) / (reference_w + gap)).floor() as usize).max(1);
    let rows = count.div_ceil(per_row);
    rows <= thumb_max_rows(input, card_h, max_panel_h, gap)
}

/// Thumbnail card height derives from the base width per the mockup (pure,
/// testable): vertical paddings + caption + gap + a 16:10 preview.
fn thumb_card_h(input: &ThemeInput, card_width: f64) -> f64 {
    let preview_h = (card_width - THUMB_PAD * 2.0) / THUMB_PREVIEW_RATIO;
    THUMB_PAD * 2.0 + thumb_caption_h(input) + THUMB_GAP + preview_h
}

/// The flow layout's uniform card height: derived once from the base width;
/// every card shares it.
#[cfg(test)]
pub(crate) fn thumb_card_h_fixed(input: &ThemeInput) -> f64 {
    thumb_card_h(input, THUMB_CARD_BASE_W)
}

/// Scale is based on the base card width; caption and padding stay fixed instead
/// of mechanically enlarging text and controls.
pub(crate) fn thumb_card_h_for_scale(input: &ThemeInput, scale: f64) -> f64 {
    thumb_card_h(input, THUMB_CARD_BASE_W * scale.max(THUMB_MIN_SCALE))
}

/// Preview height = card height - vertical paddings - caption - gap (pure, testable).
pub(crate) fn thumb_preview_h(input: &ThemeInput, card_h: f64) -> f64 {
    (card_h - THUMB_PAD * 2.0 - thumb_caption_h_for_card(input, card_h) - THUMB_GAP).max(40.0)
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
pub(crate) fn thumb_card_w_for_aspect(input: &ThemeInput, card_h: f64, aspect: f64) -> f64 {
    thumb_preview_h(input, card_h) * clamp_aspect(aspect) + THUMB_PAD * 2.0
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
fn thumb_widths(
    input: &ThemeInput,
    aspects: &[f64],
    range: &Range<usize>,
    card_h: f64,
    max_inner: f64,
) -> Vec<f64> {
    thumb_widths_with_max_card_w(input, aspects, range, card_h, max_inner, f64::INFINITY)
}

fn thumb_widths_with_max_card_w(
    input: &ThemeInput,
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
            thumb_card_w_for_aspect(input, card_h, aspect)
                .min(max_inner)
                .min(max_card_w)
        })
        .collect()
}

fn thumb_max_rows(input: &ThemeInput, card_h: f64, max_panel_h: f64, gap: f64) -> usize {
    let available =
        (max_panel_h - THUMB_TOP_INSET - THUMB_BOTTOM_INSET - footer_band_h(input)).max(card_h);
    ((available + gap) / (card_h + gap)).floor().max(1.0) as usize
}

/// Rows an overflowing viewport can show: the bottom teaser (one gap plus a third of a card) comes
/// out of the budget before whole rows are counted. Without this the overflow panel is a teaser
/// taller than the budget and eats the top/bottom margins (measured 2026-09-24: budget 875, panel
/// 921, margins down to 33/2).
fn thumb_viewport_rows(input: &ThemeInput, card_h: f64, max_panel_h: f64, gap: f64) -> usize {
    let teaser_h = gap + card_h * THUMB_SCROLL_TEASER_RATIO;
    let available =
        (max_panel_h - THUMB_TOP_INSET - THUMB_BOTTOM_INSET - footer_band_h(input) - teaser_h)
            .max(card_h);
    ((available + gap) / (card_h + gap)).floor().max(1.0) as usize
}

/// Whether the teaser fits without costing a whole row. If it does it is drawn (a sliver of the next
/// row peeks at the bottom); if not it is dropped in favour of one more full row.
pub(crate) fn thumb_teaser_fits(
    input: &ThemeInput,
    card_h: f64,
    max_panel_h: f64,
    gap: f64,
) -> bool {
    thumb_viewport_rows(input, card_h, max_panel_h, gap)
        == thumb_max_rows(input, card_h, max_panel_h, gap)
}

/// The measured metrics a panel height is computed from, shared by the normal layout's
/// [`thumb_panel_metrics`] and the close reflow's [`thumb_close_panel_metrics`].
///
/// The two entry points differ in how many rows and whether a teaser is counted, not in what "a
/// card" or "the budget" means, so they take the same struct rather than repeating five floats.
#[derive(Clone, Copy)]
pub(crate) struct ThumbPanelMetrics {
    pub(crate) card_h: f64,
    pub(crate) gap: f64,
    /// Whether the teaser fits without costing a whole row; if not it is dropped.
    pub(crate) teaser_fits: bool,
    /// Height budget (hard cap): the panel height still comes from the cards, it just may not exceed
    /// this.
    pub(crate) max_panel_h: f64,
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

impl ThumbFlowConstraints {
    /// The panel metrics these constraints add up to: the row budget and the packing width are not
    /// part of a height.
    fn panel_metrics(self) -> ThumbPanelMetrics {
        ThumbPanelMetrics {
            card_h: self.card_h,
            gap: self.gap,
            teaser_fits: self.teaser_fits,
            max_panel_h: self.max_panel_h,
        }
    }
}

#[cfg(test)]
fn thumb_range_fits(
    input: &ThemeInput,
    aspects: &[f64],
    range: &Range<usize>,
    constraints: ThumbFlowConstraints,
) -> bool {
    pack_rows(
        &thumb_widths(
            input,
            aspects,
            range,
            constraints.card_h,
            constraints.max_inner,
        ),
        constraints.max_inner,
        constraints.gap,
    )
    .len()
        <= constraints.max_rows
}

#[cfg(test)]
fn maximal_prefix(
    input: &ThemeInput,
    aspects: &[f64],
    start: usize,
    constraints: ThumbFlowConstraints,
) -> Range<usize> {
    let mut end = start.min(aspects.len());
    while end < aspects.len() {
        let candidate = start..end + 1;
        if end == start || thumb_range_fits(input, aspects, &candidate, constraints) {
            end += 1;
        } else {
            break;
        }
    }
    start.min(end)..end
}

#[cfg(test)]
fn stable_pages(
    input: &ThemeInput,
    aspects: &[f64],
    constraints: ThumbFlowConstraints,
) -> Vec<Range<usize>> {
    if aspects.is_empty() {
        return std::iter::once(0..0).collect();
    }
    let mut pages = Vec::new();
    let mut start = 0;
    while start < aspects.len() {
        let page = maximal_prefix(input, aspects, start, constraints);
        start = page.end;
        pages.push(page);
    }
    pages
}

/// The page one legacy flow layout describes: which windows are visible, at which step, and where
/// that page sits in the page sequence. Grouped so [`build_thumb_layout`] takes a plan rather than
/// six loose values.
#[cfg(test)]
struct ThumbPagePlan<'a> {
    aspects: &'a [f64],
    visible: Range<usize>,
    scale: f64,
    overflowed: bool,
    page_index: usize,
    page_count: usize,
}

#[cfg(test)]
fn build_thumb_layout(
    input: &ThemeInput,
    plan: ThumbPagePlan,
    constraints: ThumbFlowConstraints,
) -> ThumbFlowLayout {
    let ThumbPagePlan {
        aspects,
        visible,
        scale,
        overflowed,
        page_index,
        page_count,
    } = plan;
    let card_h = constraints.card_h;
    let max_inner = constraints.max_inner;
    let gap = constraints.gap;
    let widths = thumb_widths(input, aspects, &visible, card_h, max_inner);
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
                + THUMB_BOTTOM_INSET
                + max_rows as f64 * card_h
                + max_rows.saturating_sub(1) as f64 * gap
                + footer_band_h(input),
        )
    } else {
        (
            panel_inner_w + H_PADDING * 2.0,
            THUMB_TOP_INSET
                + THUMB_BOTTOM_INSET
                + n_rows as f64 * card_h
                + n_rows.saturating_sub(1) as f64 * gap
                + footer_band_h(input),
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
        document_h: (panel_h - footer_band_h(input)).max(1.0),
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
#[allow(clippy::too_many_arguments)] // the layout inputs are independent geometry terms.
fn build_thumb_scroll_layout(
    input: &ThemeInput,
    all_rows: &[Vec<usize>],
    widths: &[f64],
    scale: f64,
    constraints: ThumbFlowConstraints,
    max_panel_w: f64,
    scrollbar_w: f64,
    scroll_offset: f64,
    trailing_reserve: f64,
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
    let total_content_h = THUMB_BOTTOM_INSET
        + all_rows.len().max(1) as f64 * constraints.card_h
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
    let viewport_h = THUMB_BOTTOM_INSET
        + viewport_row_count as f64 * constraints.card_h
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
        (used_inner_w + H_PADDING * 2.0 + scrollbar_w + trailing_reserve).min(max_panel_w)
    } else {
        used_inner_w.max(280.0_f64.min(constraints.max_inner)) + H_PADDING * 2.0 + trailing_reserve
    };
    let rendered_rows = viewport_row_count.max(1);
    let (panel_h, content_inset) =
        thumb_panel_metrics(input, rendered_rows, constraints.panel_metrics(), teaser_h);
    let card_area_w = if overflowed {
        (panel_w - scrollbar_w).max(1.0)
    } else {
        panel_w
    };
    // Center the card grid together with the right-hand scrollbar. Centering only within
    // the remaining area shifts the whole visual group left by half the scrollbar width.
    let scrollbar_centering_offset = if overflowed { scrollbar_w / 2.0 } else { 0.0 };
    let document_h = content_inset
        + THUMB_BOTTOM_INSET
        + all_rows.len() as f64 * constraints.card_h
        + all_rows.len().saturating_sub(1) as f64 * constraints.gap;
    let document_panel_h = document_h + footer_band_h(input);
    let mut document_placements = Vec::new();
    for (row_index, row) in all_rows.iter().enumerate() {
        let row_w = row.iter().map(|&i| widths[i]).sum::<f64>()
            + row.len().saturating_sub(1) as f64 * constraints.gap;
        let mut x = (card_area_w - trailing_reserve - row_w) / 2.0 + scrollbar_centering_offset;
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
        let mut x = (card_area_w - trailing_reserve - row_w) / 2.0 + scrollbar_centering_offset;
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
    trailing_reserve: f64,
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
        used_inner_w + H_PADDING * 2.0 + final_scrollbar_w + trailing_reserve
    } else {
        used_inner_w.max(280.0_f64.min(max_inner)) + H_PADDING * 2.0 + trailing_reserve
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
        let mut x = (final_card_area_w - trailing_reserve - row_w) / 2.0
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
    content_inset
        + THUMB_BOTTOM_INSET
        + row_count.max(1) as f64 * card_h
        + row_count.saturating_sub(1) as f64 * gap
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
/// height, decided by the cards themselves (rows, card height, gap, teaser, both card-area insets,
/// status bar); `max_panel_h` is only a hard cap. Stretching the panel to the cap from three rows up
/// used to decouple it from the cards and produced an extra blank band at the top (measured). The
/// normal layout and the post-close reflow must share this function, or closing a card makes the panel
/// jump taller (measured 901 > 875).
pub(crate) fn thumb_panel_metrics(
    input: &ThemeInput,
    rendered_rows: usize,
    metrics: ThumbPanelMetrics,
    teaser_h: f64,
) -> (f64, f64) {
    let rows = rendered_rows.max(1);
    let content_h = THUMB_TOP_INSET
        + THUMB_BOTTOM_INSET
        + rows as f64 * metrics.card_h
        + rows.saturating_sub(1) as f64 * metrics.gap
        + teaser_h
        + footer_band_h(input);
    // Step selection and the viewport row count already keep the content inside the budget (see
    // thumb_max_rows / thumb_viewport_rows). Clamping again is a backstop that only kicks in when the
    // budget is too small for even one row plus the teaser (max_rows floors at 1); showing a little
    // less beats crossing the cap the user asked for.
    (content_h.min(metrics.max_panel_h), THUMB_TOP_INSET)
}

/// (Panel height, content inset) after a close reflow: the viewport row count follows the same
/// "rows first, teaser only when free" rule and the height goes through the `thumb_panel_metrics`
/// the normal layout shares. `plan_thumb_close_reflow` only produces rows and coordinates; routing
/// the height through here is what stops a close from jumping the panel taller (measured 901 > 875).
pub(crate) fn thumb_close_panel_metrics(
    input: &ThemeInput,
    row_count: usize,
    overflowed: bool,
    metrics: ThumbPanelMetrics,
    max_rows: usize,
) -> (f64, f64) {
    let teaser_h =
        thumb_teaser_height(overflowed, metrics.teaser_fits, metrics.card_h, metrics.gap);
    let rows = if overflowed {
        row_count.min(max_rows.max(1))
    } else {
        row_count
    };
    thumb_panel_metrics(input, rows, metrics, teaser_h)
}

/// Content height for exactly N rows (plus an optional teaser plus the status bar). Production goes
/// through `thumb_panel_metrics`; tests still use this form to build an "exactly N rows" budget.
#[cfg(test)]
pub(crate) fn thumb_panel_height_for_rows(
    input: &ThemeInput,
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
        + THUMB_BOTTOM_INSET
        + visible_rows as f64 * card_h
        + visible_rows.saturating_sub(1) as f64 * gap
        + teaser_h
        + footer_band_h(input)
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
    input: &ThemeInput,
    aspects: &[f64],
    selected: usize,
    max_inner: f64,
    max_panel_h: f64,
    gap: f64,
) -> ThumbFlowLayout {
    let max_inner = max_inner.max(1.0);
    let scale = thumb_scale_for_count(aspects.len());
    let card_h = thumb_card_h_for_scale(input, scale);
    let constraints = ThumbFlowConstraints {
        card_h,
        max_inner,
        max_rows: thumb_max_rows(input, card_h, max_panel_h, gap),
        teaser_fits: thumb_teaser_fits(input, card_h, max_panel_h, gap),
        max_panel_h,
        gap,
    };
    if aspects.is_empty() {
        return build_thumb_layout(
            input,
            ThumbPagePlan {
                aspects,
                visible: 0..0,
                scale,
                overflowed: false,
                page_index: 0,
                page_count: 1,
            },
            constraints,
        );
    }
    let pages = stable_pages(input, aspects, constraints);
    let selected = selected.min(aspects.len() - 1);
    let page_index = pages
        .iter()
        .position(|page| page.contains(&selected))
        .unwrap_or(0);
    let visible = pages[page_index].clone();
    let page_count = pages.len();
    build_thumb_layout(
        input,
        ThumbPagePlan {
            aspects,
            visible,
            scale,
            overflowed: page_count > 1,
            page_index,
            page_count,
        },
        constraints,
    )
}

/// Plan the continuous thumbnail viewport: the card step comes from the available panel (see
/// `thumb_scale_for_panel`) and the per-card width cap is derived from that step, so callers never
/// have to guess a card height up front.
#[allow(clippy::too_many_arguments)]
pub(crate) fn plan_thumb_scroll_layout_with_max_card_w(
    input: &ThemeInput,
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
        input,
        aspects,
        max_inner,
        max_panel_h,
        gap,
        thumbnail_size_scale_override(input),
    );
    plan_thumb_scroll_layout_at_scale(
        input,
        aspects,
        scale,
        max_inner,
        max_panel_w,
        max_panel_h,
        gap,
        scrollbar_w,
        scroll_offset,
        max_card_w_for(thumb_card_h_for_scale(input, scale)),
    )
}

/// Actual packing and viewport planning at one fixed step (step selection and packing are split so
/// candidate steps can be tried with pure geometry).
#[allow(clippy::too_many_arguments)]
fn plan_thumb_scroll_layout_at_scale(
    input: &ThemeInput,
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
    // Reserve the floating control's trailing room out of the packing budget: appending it to the
    // panel afterwards would exceed `max_panel_w`.
    let trailing_reserve = footer_control_right_reserve(input, max_panel_w, max_panel_h);
    let max_inner = (max_inner - trailing_reserve).max(1.0);
    let card_h = thumb_card_h_for_scale(input, scale);
    let constraints = ThumbFlowConstraints {
        card_h,
        max_inner,
        max_rows: thumb_max_rows(input, card_h, max_panel_h, gap),
        teaser_fits: thumb_teaser_fits(input, card_h, max_panel_h, gap),
        max_panel_h,
        gap,
    };
    let widths = thumb_widths_with_max_card_w(
        input,
        aspects,
        &(0..aspects.len()),
        card_h,
        max_inner,
        max_card_w,
    );
    let rows = choose_thumb_rows(
        pack_rows(&widths, max_inner, gap),
        pack_rows_greedy(&widths, max_inner, gap),
        constraints.max_rows,
    );
    build_thumb_scroll_layout(
        input,
        &rows,
        &widths,
        scale,
        constraints,
        max_panel_w,
        scrollbar_w,
        scroll_offset,
        trailing_reserve,
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
    input: &ThemeInput,
    count: usize,
    screen_width: f64,
    max_panel_h: f64,
    scrollbar_w: f64,
    scroll_offset: f64,
) -> ThumbFlowLayout {
    let gap = ICON_CARD_GAP;
    let card_h = card_h(input);
    let max_panel_w = (screen_width.max(1.0) * PANEL_MAX_WIDTH_RATIO)
        .max(ICON_CARD_W + H_PADDING * 2.0 + scrollbar_w);
    // The floating settings control's trailing room comes out of the width budget first, so the panel
    // never grows past `max_panel_w`: packing a row, then appending the reserve, would push the panel
    // (and the control with it) off the screen.
    let trailing_reserve = footer_control_right_reserve(input, max_panel_w, max_panel_h);
    let packing_inner =
        (max_panel_w - H_PADDING * 2.0 - scrollbar_w - trailing_reserve).max(ICON_CARD_W);
    let max_columns = ((packing_inner + gap) / (ICON_CARD_W + gap))
        .floor()
        .max(1.0) as usize;
    // Keep the legacy three-slot baseline for small sets, while card dimensions remain fixed.
    let baseline_columns = count.max(3).min(max_columns);
    let minimum_rows = if count == 0 {
        0
    } else {
        count.div_ceil(max_columns)
    };
    let max_rows = ((max_panel_h - THUMB_TOP_INSET - THUMB_BOTTOM_INSET - footer_band_h(input))
        .max(card_h)
        / (card_h + gap))
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
        grid_w + H_PADDING * 2.0 + trailing_reserve
    };
    let card_area_w = if overflowed {
        (panel_w - scrollbar_w).max(ICON_CARD_W + H_PADDING * 2.0)
    } else {
        panel_w
    };
    let scrollbar_centering_offset = if overflowed { scrollbar_w / 2.0 } else { 0.0 };
    let row_pitch = card_h + gap;
    let total_content_h = THUMB_TOP_INSET
        + THUMB_BOTTOM_INSET
        + visual_row_count as f64 * card_h
        + visual_row_count.saturating_sub(1) as f64 * gap;
    let teaser_h = if overflowed {
        gap + card_h * THUMB_SCROLL_TEASER_RATIO
    } else {
        0.0
    };
    let viewport_content_h = THUMB_TOP_INSET
        + THUMB_BOTTOM_INSET
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
        + THUMB_BOTTOM_INSET
        + viewport_rows as f64 * card_h
        + viewport_rows.saturating_sub(1) as f64 * gap
        + teaser_h
        + footer_band_h(input);
    let document_h = total_content_h;
    let document_panel_h = document_h + footer_band_h(input);
    let mut document_placements = Vec::with_capacity(count);
    for (row_index, row) in row_ranges.iter().enumerate() {
        let row_w = row.len() as f64 * ICON_CARD_W + row.len().saturating_sub(1) as f64 * gap;
        let row_x = (card_area_w - trailing_reserve - row_w) / 2.0 + scrollbar_centering_offset;
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
        let row_x = (card_area_w - trailing_reserve - row_w) / 2.0 + scrollbar_centering_offset;
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

/// Initial icon-only overlay height = both card-area insets + rows * card height + status bar (pure,
/// testable). Used only for the initial window placeholder; summon-time layout recalculates it.
fn compute_window_height(
    input: &ThemeInput,
    count: usize,
    cards_per_row: usize,
    card_h: f64,
) -> f64 {
    let rows = count.max(1).div_ceil(cards_per_row);
    THUMB_TOP_INSET + THUMB_BOTTOM_INSET + rows as f64 * card_h + footer_band_h(input)
}

/// Initial icon-only overlay height; the live layout computes columns from screen width.
pub(crate) fn window_height(input: &ThemeInput, count: usize) -> f64 {
    compute_window_height(input, count, 6, card_h(input))
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

    /// The two ways to read the thumbnail switch must agree.
    ///
    /// The snapshot form is what the pure layer sees; the direct form is what the capture workers,
    /// the observer thread and the pregen path read, because cloning a whole `ThemeInput` there
    /// costs a config clone and -- for the "auto" appearance -- an NSUserDefaults query off the main
    /// thread. They must not drift: a page that says thumbnails are on while the scheduler says off
    /// is a switch that does nothing.
    #[test]
    fn both_thumbnail_switch_reads_agree() {
        let on = ThemeInput {
            thumbnails_enabled: true,
            ..fixed_input()
        };
        let off = ThemeInput {
            thumbnails_enabled: false,
            ..fixed_input()
        };
        // The snapshot form follows its own field, not the global.
        assert!(thumbnails_enabled(&on));
        assert!(!thumbnails_enabled(&off));
        // The direct form answers from the configuration the process is running, and it is the
        // same field `from_config` copies into the snapshot.
        let live = CONFIG.read().unwrap().layout.thumbnails_enabled;
        assert_eq!(thumbnails_enabled_setting(), live);
        // Take the copy and drop the guard BEFORE constructing the snapshot: `from_config` queries
        // NSUserDefaults for the "auto" appearance, and this very test must not be the example
        // that shows the lock being held across it.
        let config = CONFIG.read().unwrap().clone();
        assert_eq!(ThemeInput::from_config(&config).thumbnails_enabled, live);
    }

    /// A snapshot with every field set explicitly, so a layout assertion describes fixed inputs
    /// rather than whatever the developer's on-disk config file happens to contain.
    pub(crate) fn fixed_input() -> ThemeInput {
        let palette = |win_title: &str, card_bg_sel: &str, icon_text: &str| config::ThemeColors {
            status_bar_text: "000000FF".to_string(),
            app_name: "444444FF".to_string(),
            win_title: win_title.to_string(),
            icon_inner_bg: "EEEEEEFF".to_string(),
            icon_text: icon_text.to_string(),
            card_bg_sel: card_bg_sel.to_string(),
            card_border_sel: "2F6FEDFF".to_string(),
        };
        ThemeInput {
            dark: false,
            palette: CustomPalette {
                dark: palette("F5F5F7FF", "5B9BFF24", "F8F9FAFF"),
                light: palette("111111FF", "2F6FED1A", "222222FF"),
            },
            corner_radius: 32.0,
            card_text_size: CARD_TEXT_BASE_SIZE,
            thumbnail_size: "auto".to_string(),
            thumbnails_enabled: true,
            show_app_name_in_cards: false,
            title_font_size: 12.0,
            app_name_font_size: 10.0,
            status_bar_font_size: 15.0,
        }
    }

    /// P0's before/after baseline: the key geometry, computed from a snapshot built from the current
    /// configuration, must equal what the same functions returned while they read that configuration
    /// themselves. The reference numbers below are the values the pre-migration functions produced
    /// for these inputs (`theme.rs` before `ThemeInput`); they are literals on purpose, because a
    /// reference that read the config would only prove the new code agrees with itself.
    #[test]
    fn key_geometry_from_a_config_snapshot_matches_the_pre_migration_values() {
        let mut input = fixed_input();
        input.card_text_size = 15.0;
        input.title_font_size = 12.0;
        input.app_name_font_size = 10.0;
        input.status_bar_font_size = 15.0;

        assert_eq!(card_w(), 140.0);
        // 110pt icon + 4 + 4.5 + title line (15 * 1.2) + 2 + app-name line (13 * 1.35) + 4:
        // the caption lines are the *clamped* card font sizes, not the configured ones.
        assert_eq!(card_h(&input), 160.05);
        // Footer text 15 -> 36pt status bar, minus the card viewport's 8pt bottom inset, floor 36.
        assert_eq!(status_bar_text_size(&input), 15.0);
        assert_eq!(footer_band_h(&input), 36.0);
        // The title scales with the card text size: 12 * 15 / 12, inside the 13..=20 clamp.
        assert_eq!(card_title_font_size(&input), 15.0);
        // The app name scales the same way but the clamp lifts it: 10 * 15 / 12 = 12.5 -> 13.
        assert_eq!(card_app_name_font_size(&input), 13.0);

        // And the same values through the transition entry point, which is what the callers use:
        // its snapshot must agree with a snapshot built by hand from the same fields.
        let current = ThemeInput::current();
        let by_hand = ThemeInput {
            dark: current.dark,
            palette: CustomPalette {
                dark: current.palette.dark.clone(),
                light: current.palette.light.clone(),
            },
            corner_radius: current.corner_radius,
            card_text_size: current.card_text_size,
            thumbnail_size: current.thumbnail_size.clone(),
            thumbnails_enabled: current.thumbnails_enabled,
            show_app_name_in_cards: current.show_app_name_in_cards,
            title_font_size: current.title_font_size,
            app_name_font_size: current.app_name_font_size,
            status_bar_font_size: current.status_bar_font_size,
        };
        assert_eq!(current, by_hand);
        assert_eq!(card_h(&current), card_h(&by_hand));
        assert_eq!(footer_band_h(&current), footer_band_h(&by_hand));
    }

    /// The dependency relation P0 promises: changing one field of the snapshot changes ONLY the
    /// outputs whose consumer is documented for that field, and every other output stays
    /// bit-identical. The outputs are collected as exact f64 bits, so a change anywhere else fails.
    #[test]
    fn changing_one_snapshot_field_changes_only_its_documented_consumers() {
        let base = fixed_input();
        let outputs = |input: &ThemeInput| -> Vec<(&'static str, u64)> {
            vec![
                ("card_w", card_w().to_bits()),
                ("card_h", card_h(input).to_bits()),
                (
                    "icon_card_layout.icon_bottom",
                    icon_card_layout(input, card_w(), card_h(input))
                        .icon_bottom
                        .to_bits(),
                ),
                (
                    "card_title_font_size",
                    card_title_font_size(input).to_bits(),
                ),
                (
                    "card_app_name_font_size",
                    card_app_name_font_size(input).to_bits(),
                ),
                (
                    "status_bar_text_size",
                    status_bar_text_size(input).to_bits(),
                ),
                ("footer_band_h", footer_band_h(input).to_bits()),
                ("text_scale", text_scale(input).to_bits()),
                ("thumb_caption_h", thumb_caption_h(input).to_bits()),
                (
                    "thumb_card_h_base",
                    thumb_card_h_for_scale(input, 1.0).to_bits(),
                ),
                ("overlay_card_radius", overlay_card_radius(input).to_bits()),
                (
                    "layout_corner_radius",
                    layout_corner_radius(input).to_bits(),
                ),
                (
                    "overlay_radii.close_button",
                    overlay_radii(overlay_card_radius(input), THUMB_PAD)
                        .close_button
                        .to_bits(),
                ),
                ("thumbnails_enabled", u64::from(thumbnails_enabled(input))),
                (
                    "show_app_name_in_cards",
                    u64::from(show_app_name_in_cards(input)),
                ),
                ("show_footer_title", u64::from(show_footer_title(input))),
                (
                    "thumbnail_size_scale_override",
                    thumbnail_size_scale_override(input)
                        .map(f64::to_bits)
                        .unwrap_or(0),
                ),
                (
                    "colors.status_bar_text",
                    u64::from(colors_from(input, true).status_bar_text),
                ),
                (
                    "colors.card_bg_sel",
                    u64::from(colors_from(input, false).card_bg_sel),
                ),
                (
                    "colors.icon_text_dark",
                    u64::from(colors_from(input, true).icon_text),
                ),
                ("icon_px", icon_px().to_bits()),
                ("letter_px", letter_px().to_bits()),
            ]
        };
        let changed = |mutate: &dyn Fn(&mut ThemeInput)| {
            let mut input = base.clone();
            mutate(&mut input);
            outputs(&base)
                .into_iter()
                .zip(outputs(&input))
                .filter(|((name, a), (other, b))| name == other && a != b)
                .map(|((name, _), _)| name)
                .collect::<Vec<_>>()
        };
        let expect = |mutate: &dyn Fn(&mut ThemeInput), names: &[&str]| {
            let mut got = changed(mutate);
            got.sort_unstable();
            let mut wanted = names.to_vec();
            wanted.sort_unstable();
            assert_eq!(got, wanted);
        };

        expect(
            &|input| input.card_text_size = 18.0,
            &[
                "card_h",
                "icon_card_layout.icon_bottom",
                "card_title_font_size",
                "card_app_name_font_size",
                "text_scale",
                "thumb_caption_h",
                "thumb_card_h_base",
            ],
        );
        expect(
            &|input| input.title_font_size = 16.0,
            &[
                "card_h",
                "icon_card_layout.icon_bottom",
                "card_title_font_size",
            ],
        );
        expect(
            &|input| input.app_name_font_size = 14.0,
            &[
                "card_h",
                "icon_card_layout.icon_bottom",
                "card_app_name_font_size",
            ],
        );
        expect(
            &|input| input.status_bar_font_size = 0.0,
            &["status_bar_text_size", "footer_band_h", "show_footer_title"],
        );
        expect(
            &|input| input.status_bar_font_size = 20.0,
            &["status_bar_text_size", "footer_band_h"],
        );
        expect(
            &|input| input.corner_radius = 8.0,
            &[
                "overlay_card_radius",
                "layout_corner_radius",
                "overlay_radii.close_button",
            ],
        );
        expect(
            &|input| input.thumbnails_enabled = false,
            &["thumbnails_enabled"],
        );
        expect(
            &|input| input.show_app_name_in_cards = true,
            &["show_app_name_in_cards"],
        );
        expect(
            &|input| input.thumbnail_size = "110".to_string(),
            &["thumbnail_size_scale_override"],
        );
        expect(
            &|input| input.palette.dark.win_title = "ABCDEFFF".to_string(),
            &["colors.status_bar_text"],
        );
        expect(
            &|input| input.palette.light.card_bg_sel = "00000000".to_string(),
            &["colors.card_bg_sel"],
        );
        expect(
            &|input| input.palette.dark.icon_text = "00000000".to_string(),
            &["colors.icon_text_dark"],
        );
        // The palette's light half is not a consumer of the dark flag and vice versa: only the
        // resolve switch changes when the mode does, and changing the mode alone is out of the
        // snapshot's control (it is an input, not a switch). Assert the negative explicitly so the
        // palette case above cannot pass by leaking into the other mode.
        let other_mode = colors_from(&base, false).icon_text;
        assert_eq!(other_mode, colors_from(&base, false).icon_text);
    }
    /// A snapshot whose footer band exists, at the shipped typography the pre-migration tests
    /// measured through the global.
    ///
    /// The shipped config is a 15pt card text size and a 15pt footer text (a 36pt band): the base
    /// 12pt faces come out as 12 * 15 / 12 = 15 for the title and 10 * 15 / 12 = 12.5, clamped to
    /// 13, for the app name. `CONFIG`'s built-in defaults are exactly those values, and the
    /// before/after baseline test below pins the resulting card height.
    pub(crate) fn band_present_input() -> ThemeInput {
        let mut input = fixed_input();
        input.card_text_size = 15.0;
        assert_eq!(footer_band_h(&input), FOOTER_CONTROL_MIN_H);
        input
    }

    /// A snapshot with a pinned corner radius and the band present (a positive value) or absent (0).
    pub(crate) fn input_with(band: f64, radius: f64) -> ThemeInput {
        let mut input = fixed_input();
        input.corner_radius = radius;
        // The band follows the footer text size; 0 is the slider's hidden stop.
        input.status_bar_font_size = if band > 0.0 { 15.0 } else { 0.0 };
        assert_eq!(footer_band_h(&input), band, "the fixture must pin the band");
        input
    }
    /// The footer settings control must stay inside the panel's bottom-right corner arc at every
    /// shipped radius, and the assertion must be able to FAIL: the counter-example below repeats the
    /// containment test with an inset one grid step too small and requires it to be outside.
    #[test]
    fn footer_control_inset_keeps_the_gear_inside_the_panel_corner() {
        // Containment of the button's bottom-right corner in the clamped ellipse-corner.
        fn inside(inset: f64, r: f64, w: f64, h: f64, control_h: f64, band_h: f64) -> bool {
            let rx = r.min(w / 2.0);
            let ry = r.min(h / 2.0);
            if rx <= 0.0 || ry <= 0.0 {
                return true;
            }
            let m = (band_h - control_h) / 2.0;
            (((rx - inset) / rx).powi(2) + ((ry - m) / ry).powi(2)) <= 1.0 + 1e-9
        }

        let band = FOOTER_CONTROL_MIN_H;
        // Card-area heights for 1..4 rows at the default card height, plus the real band.
        let card_area = |rows: f64| 32.0 + 8.0 + rows * 180.0;
        for radius in [0.0, 16.0, 32.0, 48.0, 60.0, 100.0, 200.0, 400.0] {
            for width in [484.0, 560.0, 900.0] {
                for rows in [1.0, 2.0, 3.0, 4.0] {
                    let height = card_area(rows) + band;
                    let inset =
                        footer_control_inset(radius, width, height, FOOTER_CONTROL_SIZE, band);
                    assert!(
                        inside(inset, radius, width, height, FOOTER_CONTROL_SIZE, band),
                        "radius={radius} width={width} rows={rows}: inset {inset} is outside the corner"
                    );
                    // The counter-example: one 4pt step tighter must be OUTSIDE, or this test is
                    // asserting nothing about the corner (it would pass for any inset).
                    if inset > 4.0 {
                        assert!(
                            !inside(
                                inset - 4.0,
                                radius,
                                width,
                                height,
                                FOOTER_CONTROL_SIZE,
                                band
                            ) || inset - 4.0 <= 0.0,
                            "radius={radius} width={width} rows={rows}: the containment test passes \
                             even one grid step inside {inset}, so it does not bound the corner"
                        );
                    }
                }
            }
        }
    }

    /// Raising the band must not raise the required inset: the control is centred, so \`ry - m\`
    /// grows with the band. This is the property that makes the bounded fallback work, and it is
    /// false for a fixed vertical margin.
    #[test]
    fn raising_the_footer_band_never_increases_the_required_inset() {
        let radius = 400.0;
        let (width, rows) = (560.0, 1.0);
        let card_area = 32.0 + 8.0 + rows * 180.0;
        let mut previous = f64::INFINITY;
        for step in 0..20 {
            let band = FOOTER_CONTROL_MIN_H + step as f64 * 32.0;
            let height = card_area + band;
            let inset = footer_control_inset(radius, width, height, FOOTER_CONTROL_SIZE, band);
            assert!(
                inset <= previous + 1e-9,
                "a taller band (band={band}) required a larger inset ({inset} > {previous})"
            );
            previous = inset;
        }
        // And the counter-example: with the margin pinned at 4pt instead of centred, the same sweep
        // must INCREASE the requirement -- that is the model this design rejects.
        let fixed_margin_inset = |band: f64| {
            let height = card_area + band;
            let rx = radius.min(width / 2.0);
            let ry = radius.min(height / 2.0);
            let m = 4.0;
            if m >= ry {
                return 0.0;
            }
            rx * (1.0 - (1.0 - ((ry - m) / ry).powi(2)).max(0.0).sqrt())
        };
        assert!(
            fixed_margin_inset(FOOTER_CONTROL_MIN_H + 20.0 * 32.0)
                > fixed_margin_inset(FOOTER_CONTROL_MIN_H),
            "the fixed-margin counter-example did not behave as documented"
        );
    }

    /// The footer text stays centred on the panel at every reserve, and the reserve never exceeds
    /// half the panel (so the text can never be squeezed out of existence by the reserve itself).
    #[test]
    fn footer_text_stays_centred_under_the_symmetric_reserve() {
        // One snapshot with the band present, fed to both calls: the assertion must describe one
        // configuration, not whatever the on-disk config happens to offer between the two calls.
        let input = band_present_input();
        for width in [484.0, 560.0, 900.0] {
            for rows in [1.0, 2.0] {
                let height = 32.0 + 8.0 + rows * 180.0 + FOOTER_CONTROL_MIN_H;
                let (x, _y, w) = footer_text_frame(&input, width, height, 20.0);
                assert!(
                    (x + w / 2.0 - width / 2.0).abs() < 1e-9,
                    "width={width} is off-centre"
                );
                assert!(w >= 1.0);
                assert!(footer_text_reserve(&input, width, height) <= width / 2.0 + 1e-9);
            }
        }
    }

    /// The footer line height is bounded by the band while one exists, and must not panic when it
    /// does not: with the title switched off the band is zero and the empty-overlay path still draws
    /// the hint, where `clamp(11.0, 0.0)` would panic (min > max). The zero-band case is the
    /// counter-example that fails against the previous implementation.
    #[test]
    fn footer_line_height_survives_a_missing_band() {
        // Band present: floor, pass-through and ceiling.
        assert_eq!(footer_line_height(5.0, 36.0), 11.0);
        assert_eq!(footer_line_height(18.0, 36.0), 18.0);
        assert_eq!(footer_line_height(40.0, 36.0), 36.0);
        // Band gone: the natural line, floored but never clamped from above.
        assert_eq!(footer_line_height(18.0, 0.0), 18.0);
        assert_eq!(footer_line_height(5.0, 0.0), 11.0);
    }

    /// The switch-to-band mapping, asserted on the pure function: the band exists to hold the settings
    /// control, so it is the control's floor at small footer sizes, grows with the footer text, and is
    /// gone entirely when the title is off -- at every footer size, which is what makes hiding the
    /// title reclaim space instead of leaving an empty strip.
    #[test]
    fn the_footer_band_vanishes_only_when_the_title_is_off() {
        assert_eq!(footer_band_h_for(true, 15.0), FOOTER_CONTROL_MIN_H);
        assert_eq!(footer_band_h_for(false, 15.0), 0.0);
        assert!(
            footer_band_h_for(true, 20.0) > FOOTER_CONTROL_MIN_H,
            "a large footer text must grow the band it is drawn in"
        );
        assert_eq!(footer_band_h_for(false, 20.0), 0.0);
    }

    /// The floating settings control only takes room out of the card grid once the configured corner
    /// radius pushes it past the grid's own trailing inset. At the shipped 32pt it must take nothing
    /// (re-deriving row packing for a sub-point would change the layout for no visible gain), and a
    /// radius large enough to intrude must produce a reserve that clears the grid.
    #[test]
    fn footer_control_right_reserve_only_appears_past_the_shipped_radius() {
        let (panel_w, panel_h) = (900.0, 600.0);
        assert_eq!(
            footer_control_right_reserve_for(32.0, panel_w, panel_h),
            0.0,
            "the shipped radius must not perturb the card grid"
        );
        let large = footer_control_right_reserve_for(64.0, panel_w, panel_h);
        assert!(
            large > 10.0,
            "a 64pt radius intrudes well past H_PADDING, got {large}"
        );
        // The reserve is exactly how far the control's left edge would sit past the grid's boundary,
        // so the grid shrunk by it cannot reach the control.
        let inset = footer_control_inset(
            64.0,
            panel_w,
            panel_h,
            FOOTER_CONTROL_SIZE,
            FOOTER_CONTROL_SIZE + FOOTER_FLOAT_MARGIN * 2.0,
        );
        assert!((large - (inset + FOOTER_CONTROL_SIZE - H_PADDING)).abs() < 1e-9);
    }

    /// The floating settings control must not sit over a card in the real layout, in either display
    /// mode. This compares the layout's own placements against the control's frame: an earlier
    /// version of this fix only shrank the packing budget, and the panel then shrank back around the
    /// widest row, so the card still ended exactly where the control begins.
    #[test]
    fn the_floating_settings_control_never_sits_over_a_card() {
        // Both inputs are parameters of the snapshot: at the shipped radius the reserve is zero (sub
        // point), which would make this assertion vacuous, so the fixture pins a 64pt radius and no
        // band -- the same values the layout is given, not a global to write.
        let input = input_with(0.0, 64.0);
        let max_panel_h = 700.0;

        let icon = plan_icon_scroll_layout(&input, 6, 1400.0, max_panel_h, THUMB_SCROLLBAR_W, 0.0);
        assert!(!icon.overflowed, "the icon fixture must stay in one row");
        assert_no_card_under_the_control(&input, &icon, "icon");

        let aspects = vec![THUMB_PREVIEW_RATIO; 4];
        let thumbs = plan_thumb_scroll_layout_with_max_card_w(
            &input,
            &aspects,
            1200.0,
            1400.0,
            max_panel_h,
            THUMB_ROW_GAP,
            THUMB_SCROLLBAR_W,
            0.0,
            |_| f64::INFINITY,
        );
        assert!(
            !thumbs.overflowed,
            "the thumbnail fixture must stay non-overflow"
        );
        assert_no_card_under_the_control(&input, &thumbs, "thumbnail");
    }

    /// The trailing reserve is taken out of the width budget, not appended to it: packing a row and
    /// then widening the panel for the control would push the panel -- and the control with it -- past
    /// the screen budget that `clamp_into_visible` only re-positions, never shrinks.
    #[test]
    fn a_large_corner_radius_never_pushes_the_panel_past_its_width_budget() {
        let input = input_with(0.0, 512.0);
        let max_panel_h = 800.0;

        let screen_w = 1440.0;
        let icon_budget = (screen_w * PANEL_MAX_WIDTH_RATIO)
            .max(ICON_CARD_W + H_PADDING * 2.0 + THUMB_SCROLLBAR_W);
        let icon =
            plan_icon_scroll_layout(&input, 40, screen_w, max_panel_h, THUMB_SCROLLBAR_W, 0.0);
        assert!(
            icon.overflowed,
            "the icon fixture must reach the overflow branch"
        );
        assert!(
            icon.panel_w <= icon_budget + 1e-9,
            "the icon panel is {:.1}pt, past its {:.1}pt width budget",
            icon.panel_w,
            icon_budget
        );

        let thumb_budget = 1400.0;
        let aspects = vec![THUMB_PREVIEW_RATIO; 40];
        let thumbs = plan_thumb_scroll_layout_with_max_card_w(
            &input,
            &aspects,
            thumb_budget - H_PADDING * 2.0 - THUMB_SCROLLBAR_W,
            thumb_budget,
            max_panel_h,
            THUMB_ROW_GAP,
            THUMB_SCROLLBAR_W,
            0.0,
            |_| f64::INFINITY,
        );
        assert!(
            thumbs.overflowed,
            "the thumbnail fixture must reach the overflow branch"
        );
        assert!(
            thumbs.panel_w <= thumb_budget + 1e-9,
            "the thumbnail panel is {:.1}pt, past its {:.1}pt width budget",
            thumbs.panel_w,
            thumb_budget
        );
    }

    /// Every card in `layout` ends left of the floating control's frame. One point of slack: the grid
    /// deliberately does not give way for sub-point intrusions (see `footer_control_right_reserve`).
    fn assert_no_card_under_the_control(input: &ThemeInput, layout: &ThumbFlowLayout, label: &str) {
        let size = FOOTER_CONTROL_SIZE;
        let inset = footer_control_inset(
            layout_corner_radius(input),
            layout.panel_w,
            layout.panel_h,
            size,
            size + FOOTER_FLOAT_MARGIN * 2.0,
        );
        let control_left = layout.panel_w - inset - size;
        let widest_right = layout
            .placements
            .iter()
            .map(|p| p.x + p.width)
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(
            widest_right <= control_left + 1.0,
            "{label}: the widest card ends at {widest_right:.1}, the floating control starts at \
             {control_left:.1} (panel {:.1}pt)",
            layout.panel_w
        );
    }

    /// The style document states the elevation table; these are its numbers, so a re-tuned blur radius
    /// cannot drift away from the document silently. `med` had no assertion at all before this.
    #[test]
    fn elevation_levels_match_the_style_document() {
        assert_eq!(ELEVATION_MED.radius, 12.0);
        assert_eq!(ELEVATION_MED.offset_y, -4.0);
        assert!((ELEVATION_MED.opacity - 0.10).abs() < 1e-6);
        assert_eq!(ELEVATION_HIGH.radius, 32.0);
        assert_eq!(ELEVATION_HIGH.offset_y, -12.0);
        assert!((ELEVATION_HIGH.opacity - 0.18).abs() < 1e-6);
        for level in [ELEVATION_MED, ELEVATION_HIGH] {
            assert_eq!(level.color, 0x000000FF);
        }
    }

    /// The padding only ever adds room on the side the shadow is offset towards, and the panel's own
    /// rect is never widened by it.
    #[test]
    fn elevation_insets_leave_the_panel_rect_alone() {
        let insets = elevation_insets(ELEVATION_HIGH);
        // radius 32 + the measured tail allowance 32; the bottom also carries the 12pt downward offset.
        assert_eq!(insets.top, 64.0);
        assert_eq!(insets.left, 64.0);
        assert_eq!(insets.right, 64.0);
        assert_eq!(insets.bottom, 76.0);

        let panel_size = NSSize::new(500.0, 420.0);
        let window_size = NSSize::new(
            panel_size.width + insets.left + insets.right,
            panel_size.height + insets.top + insets.bottom,
        );
        let local = panel_rect_in_window(window_size, insets);
        assert_eq!(local.size.width, panel_size.width);
        assert_eq!(local.size.height, panel_size.height);
        assert_eq!(local.origin.x, insets.left);
        assert_eq!(local.origin.y, insets.bottom);
    }

    /// Panel coordinates are what every caller and every stored value means, so a panel rect must survive
    /// the trip out to a window frame and back unchanged.
    #[test]
    fn panel_and_window_rects_round_trip() {
        for insets in [
            elevation_insets(ELEVATION_MED),
            elevation_insets(ELEVATION_HIGH),
        ] {
            let panel = NSRect::new(NSPoint::new(-320.5, 118.0), NSSize::new(500.0, 420.0));
            let window = window_frame_for_panel(panel, insets);
            assert_eq!(
                window.size.width,
                panel.size.width + insets.left + insets.right
            );
            assert_eq!(
                window.size.height,
                panel.size.height + insets.top + insets.bottom
            );
            assert_eq!(window.origin.x + insets.left, panel.origin.x);
            assert_eq!(window.origin.y + insets.bottom, panel.origin.y);
            // The rect a shadow path encloses must come back as the panel itself, never wider.
            let local = panel_rect_in_window(window.size, insets);
            assert_eq!(local.size, panel.size);
            assert_eq!(
                NSPoint::new(
                    window.origin.x + local.origin.x,
                    window.origin.y + local.origin.y
                ),
                panel.origin
            );
        }
    }

    #[test]
    fn palette_text_and_accent_meet_documented_contrast_floors() {
        for palette in [ui_palette_for_mode(false), ui_palette_for_mode(true)] {
            let roles = [
                (palette.primary_text, 12.0),
                (palette.secondary_text, 7.0),
                (palette.muted_text, 4.5),
                // Links are body text colored as links (clipboard URL rows and detail, settings
                // external links). A fixed link blue composited to 1.88:1 in dark mode, so the
                // token is held to the text floor on both surfaces in both modes.
                (palette.link_text, 4.5),
                (palette.link_text_hover, 4.5),
                (palette.disabled_text, TEXT_HARD_FLOOR),
                (palette.success_text, 4.5),
                (palette.warning_text, 4.5),
                (palette.error_text, 4.5),
            ];
            for (color, floor) in roles {
                for background in [palette.window_bg, palette.card_bg] {
                    assert!(
                        contrast_ratio(composite_rgb(color, background), color_rgb(background))
                            >= floor,
                        "color {color:#010x} misses {floor}:1 on {background:#010x}"
                    );
                }
            }
            // The selected sidebar row is the mock's flat nav tint with its own ink
            // (`nav_active_bg`/`nav_active_ink`), an opaque pairing held to the 3.5:1 hard floor.
            assert!(
                contrast_ratio(
                    color_rgb(palette.nav_active_ink),
                    color_rgb(palette.nav_active_bg),
                ) >= TEXT_HARD_FLOOR,
                "selected sidebar title measures under 3.5:1 on its nav tint"
            );
            for background in [palette.window_bg, palette.card_bg] {
                assert!(
                    contrast_ratio(
                        composite_rgb(palette.accent, background),
                        color_rgb(background),
                    ) >= 3.0
                );
            }
        }
    }

    #[test]
    fn documented_structural_boundary_colors_keep_their_measured_contrast() {
        let light = ui_palette_for_mode(false);
        let card_border = contrast_ratio(
            composite_rgb(light.card_border, light.card_bg),
            color_rgb(light.card_bg),
        );
        assert!((card_border - 1.20).abs() < 0.02);
        assert!((contrast_ratio(color_rgb(0xC7C7CCFF), color_rgb(0xFFFFFFFF)) - 1.68).abs() < 0.02);

        let dark = ui_palette_for_mode(true);
        assert_eq!(dark.detail_bg, 0x121215FF);
        assert!(
            (contrast_ratio(color_rgb(0x636366FF), color_rgb(dark.card_bg)) - 2.74).abs() < 0.03
        );
    }

    #[test]
    fn selection_background_alpha_matches_the_documented_opacity() {
        for (palette, expected) in [
            (ui_palette_for_mode(false), 0.102),
            (ui_palette_for_mode(true), 0.141),
        ] {
            let actual = f64::from(palette.selection_bg & 0xFF) / 255.0;
            assert!((actual - expected).abs() < 0.002);
        }
    }

    /// The icon plate is a heavier wash than a list row: it has to carry a whole icon on
    /// translucent glass, where the row wash's 8% light-mode alpha reads as nothing.
    #[test]
    fn selection_plate_wash_is_a_heavier_accent_than_a_row_wash() {
        for (palette, expected) in [
            (ui_palette_for_mode(false), 0.278),
            (ui_palette_for_mode(true), 0.278),
        ] {
            let alpha = f64::from(palette.selection_plate_bg & 0xFF) / 255.0;
            assert!((alpha - expected).abs() < 0.002, "{alpha}");
            // The ring's own hue, so the plate and its rim cannot drift apart.
            assert_eq!(
                palette.selection_plate_bg & 0xFFFF_FF00,
                palette.accent & 0xFFFF_FF00
            );
            assert!(alpha > f64::from(palette.selection_bg & 0xFF) / 255.0);
            // ...and still a wash rather than an opaque fill.
            assert!(alpha <= 0.35);
        }
    }

    #[test]
    fn documented_sidebar_destructive_and_symbol_roles_match_the_palette() {
        let light = ui_palette_for_mode(false);
        assert_eq!(light.sidebar_text, 0x6B6862FF);
        assert_eq!(light.destructive_hover, 0xD70015FF);
        assert_eq!(light.symbol_shadow, 0x000000B3);
        assert_eq!(light.badge_scrim, 0x0000008C);

        let dark = ui_palette_for_mode(true);
        assert_eq!(dark.sidebar_text, 0x9E9EA6FF);
        assert_eq!(dark.destructive_hover, 0xD93630FF);
        assert_eq!(dark.symbol_shadow, 0x000000B3);
        assert_eq!(dark.badge_scrim, 0x0000008C);
        assert_eq!(OVERLAY_SYMBOL_SHADOW_OPACITY, 0.85);
        assert_eq!(OVERLAY_SYMBOL_SHADOW_RADIUS, 2.0);
        assert_eq!(OVERLAY_SYMBOL_SHADOW_OFFSET_Y, -1.0);
        assert_eq!(OVERLAY_BADGE_GLYPH, 22.0);
        assert_eq!(OVERLAY_BADGE_GLYPH_INSET, 4.0);
    }

    /// A control filled with `field_bg` must read as its own surface on the card it sits on.
    ///
    /// Dark mode measured 12/255 of separation at the sheet's .055 wash and looked unfilled, which
    /// is why the dark token is raised; this bound is what keeps a later tidy-up from lowering it
    /// back to a value the eye cannot see. Light mode is far above it, so the same bound covers
    /// both.
    /// The floors are per mode because the same ratio does not look the same on both: the sheet's
    /// .055 light wash measures 1.10:1 and separates clearly, while the same alpha in dark mode
    /// measured 1.165:1 and read as no fill at all. Each bound sits just under the shipped value.
    fn field_on_card_floor(dark: bool) -> f64 {
        if dark {
            1.34
        } else {
            1.08
        }
    }

    #[test]
    fn field_fill_stays_visible_on_its_card_in_both_modes() {
        for dark in [false, true] {
            let palette = ui_palette_for_mode(dark);
            let card = color_rgb(palette.card_bg);
            let field = composite_on(palette.field_bg, card);
            let ratio = contrast_ratio(field, card);
            let floor = field_on_card_floor(dark);
            assert!(
                ratio >= floor,
                "{} mode: field_bg over card_bg is {ratio:.3}:1, under the {floor:.2}:1 the control needs to read as its own surface",
                if dark { "dark" } else { "light" }
            );
        }
    }

    /// The §3.3 hard floor: no text or meaning-carrying glyph ships under 3.5:1 in either mode.
    /// Role minimums may be higher targets; this is the bound below which nothing may fall.
    const TEXT_HARD_FLOOR: f64 = 3.5;

    /// Every text or meaning-carrying-glyph pairing in the palette, asserted against the hard
    /// floor so the §3.3 rule holds for the whole table, not just the pairs a review happened to
    /// check. Structural boundaries (`card_border`, `separator`, switch tracks) are deliberately
    /// absent: clause 2 keeps its own 1.2:1 rule.
    #[test]
    fn every_text_pairing_clears_the_hard_floor() {
        for palette in [ui_palette_for_mode(false), ui_palette_for_mode(true)] {
            let mode = if palette.dark { "dark" } else { "light" };
            // Surfaces that are themselves translucent tokens are composited onto the surface
            // they render on, the way the code flattens them before drawing.
            let badge_over_white = composite_rgb(palette.badge_scrim, 0xFFFFFFFF);
            let accent_keycap = composite_rgb(palette.keycap_accent_bg, palette.card_bg);
            // The keystroke panel's neutral keycap (`keystroke_display::panel` flattens it the
            // same way): field_bg over card_bg in light mode, the card surface in dark.
            let neutral_keycap = if palette.dark {
                color_rgb(palette.card_bg)
            } else {
                composite_rgb(palette.field_bg, palette.card_bg)
            };
            let opaque = |token: u32| color_rgb(token);
            let pairs: [(&str, u32, [f64; 3]); 27] = [
                (
                    "primary on window_bg",
                    palette.primary_text,
                    opaque(palette.window_bg),
                ),
                (
                    "primary on card_bg",
                    palette.primary_text,
                    opaque(palette.card_bg),
                ),
                (
                    "secondary on window_bg",
                    palette.secondary_text,
                    opaque(palette.window_bg),
                ),
                (
                    "secondary on card_bg",
                    palette.secondary_text,
                    opaque(palette.card_bg),
                ),
                (
                    "muted on window_bg",
                    palette.muted_text,
                    opaque(palette.window_bg),
                ),
                (
                    "muted on card_bg",
                    palette.muted_text,
                    opaque(palette.card_bg),
                ),
                (
                    "disabled on window_bg",
                    palette.disabled_text,
                    opaque(palette.window_bg),
                ),
                (
                    "disabled on card_bg",
                    palette.disabled_text,
                    opaque(palette.card_bg),
                ),
                (
                    "link on window_bg",
                    palette.link_text,
                    opaque(palette.window_bg),
                ),
                (
                    "link on card_bg",
                    palette.link_text,
                    opaque(palette.card_bg),
                ),
                (
                    "link_hover on window_bg",
                    palette.link_text_hover,
                    opaque(palette.window_bg),
                ),
                (
                    "link_hover on card_bg",
                    palette.link_text_hover,
                    opaque(palette.card_bg),
                ),
                (
                    "sidebar on sidebar_bg",
                    palette.sidebar_text,
                    opaque(palette.sidebar_bg),
                ),
                (
                    "accent_ink on accent",
                    palette.accent_ink,
                    opaque(palette.accent),
                ),
                (
                    "accent_ink on accent_hover",
                    palette.accent_ink,
                    opaque(palette.accent_hover),
                ),
                (
                    "accent_text on destructive",
                    palette.accent_text,
                    opaque(palette.destructive),
                ),
                (
                    "accent_text on destructive_hover",
                    palette.accent_text,
                    opaque(palette.destructive_hover),
                ),
                (
                    "accent_text on badge_scrim/white",
                    palette.accent_text,
                    badge_over_white,
                ),
                (
                    "nav ink on nav active bg",
                    palette.nav_active_ink,
                    opaque(palette.nav_active_bg),
                ),
                (
                    // Action/ControlField buttons draw `field_bg` over `card_bg`; the pairing
                    // is the composited surface, not the raw wash token.
                    "button_text on field fill over card",
                    palette.button_text,
                    composite_on(palette.field_bg, color_rgb(palette.card_bg)),
                ),
                (
                    "button_text on footer_button_bg",
                    palette.button_text,
                    opaque(palette.footer_button_bg),
                ),
                (
                    "success on window_bg",
                    palette.success_text,
                    opaque(palette.window_bg),
                ),
                (
                    "warning on window_bg",
                    palette.warning_text,
                    opaque(palette.window_bg),
                ),
                (
                    "error on window_bg",
                    palette.error_text,
                    opaque(palette.window_bg),
                ),
                (
                    "keycap_accent_text on keycap",
                    palette.keycap_accent_text,
                    accent_keycap,
                ),
                (
                    "muted on neutral keycap",
                    palette.muted_text,
                    neutral_keycap,
                ),
                ("muted on accent keycap", palette.muted_text, accent_keycap),
            ];
            for (name, ink, surface) in pairs {
                let ratio = contrast_ratio(color_rgb(ink), surface);
                assert!(
                    ratio >= TEXT_HARD_FLOOR,
                    "{name} measures {ratio:.2}:1 in {mode} mode (hard floor {TEXT_HARD_FLOOR}:1)"
                );
            }
        }
    }

    /// Ink on an `accent` fill is a documented §3.3 deviation: the role target is higher, but
    /// the relaxed 3.5:1 hard floor still applies to both the normal and the hover fill.
    #[test]
    fn primary_button_ink_clears_the_relaxed_floor_on_both_states() {
        for palette in [ui_palette_for_mode(false), ui_palette_for_mode(true)] {
            for fill in [palette.accent, palette.accent_hover] {
                let ratio = contrast_ratio(color_rgb(palette.accent_ink), color_rgb(fill));
                assert!(
                    ratio >= 3.5,
                    "accent ink contrast on its fill is too low: {ratio:.2}:1"
                );
            }
        }
    }

    /// The white badge glyph must read over the scrim even when the captured thumbnail
    /// behind it is pure white: the composited chip is the worst-case backdrop.
    #[test]
    fn badge_glyph_meets_contrast_on_its_scrim_over_a_white_thumbnail() {
        for palette in [ui_palette_for_mode(false), ui_palette_for_mode(true)] {
            let background = composite_rgb(palette.badge_scrim, 0xFFFFFFFF);
            let ratio = contrast_ratio(color_rgb(palette.accent_text), background);
            assert!(
                ratio >= TEXT_HARD_FLOOR,
                "badge glyph contrast on its scrim is too low: {ratio:.2}:1"
            );
        }
    }

    /// The close button's hover chip fills with `destructive` while keeping the white glyph, so
    /// that pairing has to clear the 3.5:1 hard floor in both modes -- the glyph is the whole
    /// affordance. (Turning the glyph red over the chip's mid grey instead measured ~1.4:1; dark
    /// `destructive` was deepened for exactly this bound.)
    #[test]
    fn hovered_close_chip_glyph_meets_contrast_on_its_destructive_fill() {
        for palette in [ui_palette_for_mode(false), ui_palette_for_mode(true)] {
            let ratio = contrast_ratio(
                color_rgb(palette.accent_text),
                color_rgb(palette.destructive),
            );
            assert!(
                ratio >= TEXT_HARD_FLOOR,
                "hovered close glyph contrast is too low: {ratio:.2}:1"
            );
        }
    }

    /// Hover and selection fills must be visible against the surface in both modes. They are
    /// translucent washes, so a literal that only suits one mode disappears in the other: the
    /// detail panel's save-as button filled its hover state with black at 5%, which measures
    /// 1.01:1 against the dark panel -- no feedback at all. `hover_bg` is the shared token for
    /// that surface, so its visibility is asserted here.
    #[test]
    fn hover_and_selection_fills_are_visible_against_the_surface_in_both_modes() {
        // Deliberately low: the palette's own washes range from 1.097:1 (light selection) to
        // 1.343:1 (dark hover), so this floor only rules out a fill that is *effectively
        // invisible* -- the black-at-5% literal measured 1.015:1 -- without rejecting the
        // documented tokens.
        const MIN_FILL_CONTRAST: f64 = 1.05;
        for dark in [false, true] {
            let palette = ui_palette_for_mode(dark);
            let surface = color_rgb(palette.window_bg);
            for (role, token) in [
                ("hover_bg", palette.hover_bg),
                ("selection_bg", palette.selection_bg),
                ("selection_plate_bg", palette.selection_plate_bg),
            ] {
                let fill = composite_rgb(token, palette.window_bg);
                let contrast = contrast_ratio(fill, surface);
                assert!(
                    contrast >= MIN_FILL_CONTRAST,
                    "{role} measures {contrast:.2}:1 on the {} surface (min {MIN_FILL_CONTRAST})",
                    if dark { "dark" } else { "light" }
                );
                // And it must move the surface toward the text it sits under, i.e. be a visible
                // change rather than a no-op.
                assert!(
                    (fill[0] - surface[0]).abs()
                        + (fill[1] - surface[1]).abs()
                        + (fill[2] - surface[2]).abs()
                        > 4.0,
                    "{role} does not visibly change the {} surface",
                    if dark { "dark" } else { "light" }
                );
            }
        }
    }

    /// Settings external links are body text and must draw with the `link_text` roles. They
    /// used `SettingsTextRole::Accent`, which measures 3.40:1 on the light window and 3.65:1
    /// on the light card -- under the 4.5:1 text floor in both modes' card surfaces. The fix
    /// is call-site-shaped, so nothing structural forces a future link to keep the Link role;
    /// pin all three states (normal, hover-entered, hover-exited) here.
    #[test]
    fn settings_external_links_use_the_link_text_roles() {
        let source = include_str!("settings/widgets.rs");
        let builder = source
            .split("pub(super) unsafe fn make_external_link")
            .nth(1)
            .unwrap()
            .split("pub(super) unsafe fn make_symbol_image")
            .next()
            .unwrap();
        assert!(
            builder.contains("apply_settings_text_role(link, SettingsTextRole::Link)"),
            "make_external_link must paint its label with the Link role"
        );
        assert!(
            !builder.contains("SettingsTextRole::Accent"),
            "make_external_link must not fall back to the accent-as-text role"
        );
        let entered = source
            .split("pub(super) extern \"C\" fn external_link_mouse_entered")
            .nth(1)
            .unwrap()
            .split("pub(super) extern \"C\" fn external_link_mouse_exited")
            .next()
            .unwrap();
        assert!(entered.contains("SettingsTextRole::LinkHover"));
        let exited = source
            .split("pub(super) extern \"C\" fn external_link_mouse_exited")
            .nth(1)
            .unwrap()
            .split("pub(super) extern \"C\" fn external_link_mouse_down")
            .next()
            .unwrap();
        assert!(exited.contains("SettingsTextRole::Link"));
        assert!(!exited.contains("SettingsTextRole::Accent"));
    }

    /// The custom scrollbar knob must sit on the visible side of the panel surface in both modes:
    /// darker than a light panel, lighter than a dark one. It was a fixed black at 35% alpha, which
    /// is invisible against the dark panel (#1C1C1E) while looking correct in light mode -- the
    /// error only showed in one mode, so both are asserted.
    #[test]
    fn the_scroll_indicator_stays_visible_against_the_panel_in_both_modes() {
        // A knob this faint is still the accepted look (it matches the native overlay scroller),
        // so the floor is deliberately modest; the point is the *side*, not a large ratio.
        const MIN_INDICATOR_CONTRAST: f64 = 2.0;
        for dark in [false, true] {
            let palette = ui_palette_for_mode(dark);
            let surface = color_rgb(palette.window_bg);
            let knob = composite_rgb(palette.scroll_indicator, palette.window_bg);
            let contrast = contrast_ratio(knob, surface);
            assert!(
                contrast >= MIN_INDICATOR_CONTRAST,
                "scroll indicator measures {contrast:.2}:1 in {} mode (min {MIN_INDICATOR_CONTRAST})",
                if dark { "dark" } else { "light" }
            );
            let knob_luma = relative_luminance(knob);
            let surface_luma = relative_luminance(surface);
            if dark {
                assert!(
                    knob_luma > surface_luma,
                    "in dark mode the knob must be lighter than the panel"
                );
            } else {
                assert!(
                    knob_luma < surface_luma,
                    "in light mode the knob must be darker than the panel"
                );
            }
        }
    }

    /// The clipboard footer's shortcut legends draw their text with palette tokens over a glass
    /// backdrop. Both roles must stay legible in both modes: the labels were once a literal black
    /// at 34% alpha, which was invisible on the dark panel (1.16:1) and already under the text
    /// floor in light mode (2.32:1). The footer surface is glass, so it is represented here by a
    /// light and a dark bound rather than by one palette value.
    #[test]
    fn clipboard_footer_legend_text_clears_the_text_contrast_floor_in_both_modes() {
        const FOOTER_LIGHT: [f64; 3] = [249.0, 249.0, 251.0];
        const FOOTER_DARK: [f64; 3] = [42.0, 42.0, 44.0];
        const MIN_TEXT_CONTRAST: f64 = 4.5;
        for (dark, surface) in [(false, FOOTER_LIGHT), (true, FOOTER_DARK)] {
            let palette = ui_palette_for_mode(dark);
            // The key symbol is the stronger of the two; the label is the subordinate one.
            for (role, token) in [
                ("key symbol", palette.secondary_text),
                ("label", palette.muted_text),
            ] {
                let contrast = contrast_ratio(color_rgb(token), surface);
                assert!(
                    contrast >= MIN_TEXT_CONTRAST,
                    "clipboard footer legend {role} measures {contrast:.2}:1 on the {} footer \
                     (min {MIN_TEXT_CONTRAST}:1)",
                    if dark { "dark" } else { "light" }
                );
            }
        }
    }

    #[test]
    fn accent_keycap_text_meets_contrast_on_its_translucent_fill() {
        for palette in [ui_palette_for_mode(false), ui_palette_for_mode(true)] {
            let background = composite_rgb(palette.keycap_accent_bg, palette.card_bg);
            assert!(
                contrast_ratio(color_rgb(palette.keycap_accent_text), background) >= 4.5,
                "keycap accent contrast is too low in {} mode",
                if palette.dark { "dark" } else { "light" }
            );
        }
    }

    #[test]
    fn settings_and_overlay_use_the_documented_type_scale() {
        let sizes = [
            FONT_CAPTION,
            FONT_CONTROL,
            FONT_SIDEBAR_TITLE,
            FONT_PAGE_TITLE,
        ];
        assert_eq!(sizes, [12.0, 14.0, 20.0, 26.0]);
        assert_eq!(
            [FONT_WEIGHT_REGULAR, FONT_WEIGHT_SEMIBOLD, FONT_WEIGHT_BOLD],
            [0.0, 0.3, 0.4]
        );
    }

    #[test]
    fn overlay_roles_share_the_multiplier_and_clamp_the_result() {
        assert_eq!(overlay_text_size_for(12.0, 15.0), 15.0);
        assert_eq!(overlay_text_size_for(10.0, 15.0), 13.0);
        assert_eq!(overlay_text_size_for(30.0, 15.0), 20.0);
        assert_eq!(overlay_text_size_for(10.0, 25.0), 20.0);
        assert_eq!(overlay_text_size_for(10.0, 5.0), 13.0);
        assert_eq!(overlay_text_size_for(f64::NAN, f64::NAN), 13.0);
    }

    #[test]
    fn status_bar_text_uses_its_configured_size_without_card_scaling() {
        assert_eq!(status_bar_text_size_for(15.0), 15.0);
        assert_eq!(status_bar_text_size_for(13.0), 13.0);
        assert_eq!(status_bar_text_size_for(20.0), 20.0);
        assert_eq!(
            status_bar_text_size_for(f64::NAN),
            STATUS_BAR_TEXT_BASE_SIZE
        );
    }

    /// A size of 0 means "no footer band", but the empty-overlay hint is still drawn through this
    /// accessor. It must stay readable rather than collapsing to the 1pt floor.
    #[test]
    fn a_hidden_footer_still_renders_the_empty_state_hint_at_the_base_size() {
        assert_eq!(status_bar_text_size_for(0.0), STATUS_BAR_TEXT_BASE_SIZE);
        assert_eq!(status_bar_text_size_for(-3.0), STATUS_BAR_TEXT_BASE_SIZE);
        // Below the 6.5 midpoint the band is hidden entirely (`show_footer_title`), so the
        // size accessor is only reached for the empty-state hint; above it the value snaps to
        // the slider's 13..=20pt stops.
        assert_eq!(status_bar_text_size_for(1.0), 13.0);
        assert_eq!(status_bar_text_size_for(12.0), 13.0);
        assert_eq!(status_bar_text_size_for(20.0), 20.0);
        assert_eq!(status_bar_text_size_for(21.0), 20.0);
        // Fractional stored values snap to the same integer stops the slider reports --
        // a 13.6 must render at the 14pt the readout claims.
        assert_eq!(normalize_status_bar_size(6.4), 0.0);
        assert_eq!(normalize_status_bar_size(6.5), 13.0);
        assert_eq!(status_bar_text_size_for(13.6), 14.0);
        assert_eq!(normalize_status_bar_size(19.6), 20.0);
        assert_eq!(normalize_status_bar_size(f64::NAN), 0.0);
    }

    #[test]
    fn thumbnail_caption_never_exceeds_a_third_of_its_card() {
        // The shipped typography, passed in instead of read from the global: every expected value
        // below is the one the pre-migration test measured against that configuration.
        let input = band_present_input();
        for card_h in [24.0, 60.0, 90.0, 160.0, 260.0] {
            let caption_h = thumb_caption_h_for_card(&input, card_h);
            assert!(caption_h <= card_h / 3.0 + f64::EPSILON);
        }
        assert_eq!(thumb_caption_h_for_card(&input, 90.0), 30.0);
    }

    #[test]
    fn overlay_radii_are_derived_from_the_configured_card_radius() {
        let radii = overlay_radii(effective_overlay_card_radius(32.0), 8.0);
        assert_eq!(radii.card, RADIUS_PANEL);
        // The ring keeps the card's own radius (its frame still insets `OVERLAY_RING_INSET`).
        assert_eq!(radii.selection_ring, RADIUS_PANEL);
        assert_eq!(radii.thumbnail_preview, 0.0);
        assert_eq!(radii.thumbnail_icon, RADIUS_CONTROL);
        // Concentric: the button sits 8pt inside a RADIUS_PANEL corner (18 - 8 = 10).
        assert_eq!(radii.close_button, RADIUS_PANEL - 8.0);
        assert_eq!(rounded_inset_radius(24.0, 20.0, 18.0), 9.0);
    }

    #[test]
    fn full_radius_clamps_to_half_the_smaller_side() {
        // CALayer does not clamp a too-large corner radius, so shape helpers must: a status dot
        // (6x6) and the clipboard scrollbar (6pt wide) stay circles/pills instead of squares.
        assert_eq!(rounded_inset_radius(RADIUS_FULL, 6.0, 6.0), 3.0);
        assert_eq!(rounded_inset_radius(RADIUS_FULL, 6.0, 40.0), 3.0);
        assert_eq!(rounded_inset_radius(RADIUS_FULL, 38.0, 22.0), 11.0);
        assert_eq!(rounded_inset_radius(RADIUS_FULL, 18.0, 18.0), 9.0);
    }

    #[test]
    fn overlay_card_radius_stays_within_the_documented_radius_scale() {
        assert_eq!(effective_overlay_card_radius(32.0), RADIUS_PANEL);
        assert_eq!(effective_overlay_card_radius(12.0), 12.0);
        assert_eq!(effective_overlay_card_radius(-4.0), 0.0);
        assert_eq!(effective_overlay_card_radius(f64::NAN), RADIUS_PANEL);
    }

    #[test]
    fn motion_uses_two_durations_one_curve_and_honors_reduce_motion() {
        assert_eq!(ANIMATION_DURATION_FAST, 0.175);
        assert_eq!(ANIMATION_DURATION_MEDIUM, 0.380);
        assert!((animation_exit_duration(ANIMATION_DURATION_MEDIUM) - 0.285).abs() < f64::EPSILON);
        assert_eq!(ANIMATION_EASE_STANDARD, (0.24, 1.0, 0.4, 1.0));
        assert!(animation_allowed(false, false));
        assert!(!animation_allowed(true, false));
        assert!(!animation_allowed(false, true));
    }

    #[test]
    fn motion_call_sites_use_tokens_and_check_reduce_motion() {
        let sources = [
            include_str!("settings/components.rs"),
            include_str!("settings/select.rs"),
            include_str!("settings/widgets.rs"),
            include_str!("clipboard/picker.rs"),
            include_str!("overlay/card_close.rs"),
            include_str!("keystroke_display/panel.rs"),
        ];
        for source in sources {
            for line in source.lines().filter(|line| line.contains("setDuration:")) {
                let operand = line.split_once("setDuration:").unwrap().1.trim();
                assert!(
                    !operand.starts_with(|c: char| c.is_ascii_digit()),
                    "animation duration is hardcoded: {line}"
                );
            }
        }

        for source in [
            include_str!("settings/components.rs"),
            include_str!("settings/select.rs"),
            include_str!("settings/widgets.rs"),
            include_str!("clipboard/picker.rs"),
            include_str!("overlay/card_close.rs"),
            include_str!("keystroke_display/panel.rs"),
            include_str!("updater.rs"),
        ] {
            assert!(source.contains("reduce_motion_enabled()"));
        }

        let components = include_str!("settings/components.rs");
        let sidebar_hover = include_str!("settings/widgets.rs")
            .split("unsafe fn set_sidebar_hovered")
            .nth(1)
            .unwrap()
            .split("pub(super) unsafe fn sidebar_hover_style_smoke")
            .next()
            .unwrap();
        assert!(sidebar_hover.contains("sidebar_hover_fill(palette)"));
        assert!(!sidebar_hover.contains("Animation"));
        assert!(!sidebar_hover.contains("setDuration:"));
        assert!(!components.contains("move_hover_highlight"));
        // The filter pill underline is gone (the options popover replaced the chip row); its
        // style fn carried no animation, so there is nothing left to pin here.
    }

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
        let input = band_present_input();
        // Zero windows: at least one row as the floor.
        assert_eq!(
            compute_window_height(&input, 0, 5, 100.0),
            THUMB_TOP_INSET + THUMB_BOTTOM_INSET + 100.0 + footer_band_h(&input)
        );
        assert_eq!(
            compute_window_height(&input, 1, 5, 100.0),
            THUMB_TOP_INSET + THUMB_BOTTOM_INSET + 100.0 + footer_band_h(&input)
        );
    }

    #[test]
    fn height_rounds_rows_up() {
        let input = band_present_input();
        // Four per row: five windows -> two rows.
        assert_eq!(
            compute_window_height(&input, 5, 4, 120.0),
            THUMB_TOP_INSET + THUMB_BOTTOM_INSET + 2.0 * 120.0 + footer_band_h(&input)
        );
        assert_eq!(
            compute_window_height(&input, 8, 4, 120.0),
            THUMB_TOP_INSET + THUMB_BOTTOM_INSET + 2.0 * 120.0 + footer_band_h(&input)
        );
        assert_eq!(
            compute_window_height(&input, 9, 4, 120.0),
            THUMB_TOP_INSET + THUMB_BOTTOM_INSET + 3.0 * 120.0 + footer_band_h(&input)
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
    use super::tests::{band_present_input, fixed_input};
    use super::*;
    /// Test helper: an aspect vector of n base-ratio (16:10) windows.
    fn uniform_aspects(n: usize) -> Vec<f64> {
        vec![THUMB_PREVIEW_RATIO; n]
    }

    #[test]
    fn thumb_heights_derive_consistently() {
        // The uniform height derives from the configured card width; preview height
        // subtracts paddings + caption + gap.
        let input = fixed_input();
        let h = thumb_card_h_fixed(&input);
        assert!(h > 100.0, "sanity: {} too small", h);
        let ph = thumb_preview_h(&input, h);
        assert!((h - (ph + THUMB_PAD * 2.0 + thumb_caption_h(&input) + THUMB_GAP)).abs() < 1e-9);
        // The 40pt floor keeps extreme configs from collapsing.
        assert!(thumb_preview_h(&input, 50.0) >= 40.0);
    }

    #[test]
    fn card_width_scales_with_aspect_and_clamps() {
        let input = fixed_input();
        let h = 160.0;
        let ph = thumb_preview_h(&input, h);
        // A 16:10 window: preview width = preview height × 1.6; card adds side pads.
        let w = thumb_card_w_for_aspect(&input, h, 1.6);
        assert!((w - (ph * 1.6 + THUMB_PAD * 2.0)).abs() < 1e-9);
        // Extreme aspects clamp: portrait >= 0.7, landscape <= 2.2.
        let tall = thumb_card_w_for_aspect(&input, h, 0.2);
        assert!((tall - (ph * 0.7 + THUMB_PAD * 2.0)).abs() < 1e-9);
        let wide = thumb_card_w_for_aspect(&input, h, 5.0);
        assert!((wide - (ph * 2.2 + THUMB_PAD * 2.0)).abs() < 1e-9);
        // Degenerate inputs (0 / NaN) fall back to 16:10.
        let deg = thumb_card_w_for_aspect(&input, h, f64::NAN);
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
        let input = fixed_input();
        let floor_card_h = thumb_card_h_for_scale(&input, LEGACY_THUMB_MIN_SCALE);
        assert!(floor_card_h < thumb_card_h_for_scale(&input, 1.0));
        assert!(thumb_preview_h(&input, floor_card_h) > 40.0);
    }

    #[test]
    fn card_step_follows_the_panel_not_the_window_count() {
        // The panel budget on the measured 1920x1080 screen: 92% width minus padding/scrollbar, full
        // visible height.
        let panel_w = 1920.0 * PANEL_MAX_WIDTH_RATIO;
        let inner = panel_w - H_PADDING * 2.0 - THUMB_SCROLLBAR_W;
        let input = band_present_input();
        let at = |count: usize| {
            thumb_scale_for_panel(
                &input,
                &vec![THUMB_PREVIEW_RATIO; count],
                inner,
                1050.0,
                THUMB_ROW_GAP,
                None,
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
                &input,
                &uniform_aspects(15),
                tiny,
                500.0,
                THUMB_ROW_GAP,
                None
            ) <= at(15)
        );
        assert_eq!(
            thumb_scale_for_panel(
                &input,
                &uniform_aspects(60),
                tiny,
                300.0,
                THUMB_ROW_GAP,
                None
            ),
            THUMB_MIN_SCALE
        );
    }

    #[test]
    fn the_automatic_step_depends_on_the_count_not_the_window_shapes() {
        // The reference card is the base preview ratio, so the same count reaches the same step
        // whatever the windows' shapes are, and whatever the set-wide width cap is: shapes only
        // decide how the cards wrap. This is the contract the user asked for -- resizing or
        // maximizing one window must not resize every card.
        let input = band_present_input();
        let panel_w = 1470.0 * PANEL_MAX_WIDTH_RATIO;
        let inner = panel_w - H_PADDING * 2.0 - THUMB_SCROLLBAR_W;
        let budget = 923.0 - 2.0 * PANEL_MARGIN;
        let narrow: Vec<f64> = vec![1.5, 1.4, 1.2, 1.1]
            .into_iter()
            .cycle()
            .take(16)
            .collect();
        let wide: Vec<f64> = vec![2.2; 16];
        for count in 1..=16 {
            let base = thumb_scale_for_panel(
                &input,
                &uniform_aspects(count),
                inner,
                budget,
                THUMB_ROW_GAP,
                None,
            );
            for (label, aspects) in [("narrow", &narrow), ("wide", &wide)] {
                let step = thumb_scale_for_panel(
                    &input,
                    &aspects[..count],
                    inner,
                    budget,
                    THUMB_ROW_GAP,
                    None,
                );
                assert_eq!(
                    step, base,
                    "a {label} set of {count} windows must reach the same step as the base ratio"
                );
            }
            // The width cap (which one maximized window sets for the whole set) must not move it
            // either.
            let capped_step =
                thumb_scale_for_panel(&input, &narrow[..count], inner, budget, THUMB_ROW_GAP, None);
            assert_eq!(
                capped_step, base,
                "the set-wide width cap moved the step at count {count}"
            );
        }
        // The estimate is exact for base-ratio cards, so the chosen step never overflows them.
        for count in 1..=16 {
            let aspects = uniform_aspects(count);
            let step = thumb_scale_for_panel(&input, &aspects, inner, budget, THUMB_ROW_GAP, None);
            let plan = plan_thumb_scroll_layout_at_scale(
                &input,
                &aspects,
                step,
                inner,
                panel_w,
                budget,
                THUMB_ROW_GAP,
                THUMB_SCROLLBAR_W,
                0.0,
                f64::INFINITY,
            );
            assert!(
                !plan.overflowed,
                "step {step} overflowed base-ratio cards at count {count}"
            );
        }
        // A set much wider than the reference can need a row more than the estimate and therefore
        // scrolls at the chosen size: the documented trade-off of keeping the step count-only. The
        // step itself stays the same, which is the point.
        let wide_step = thumb_scale_for_panel(&input, &wide, inner, budget, THUMB_ROW_GAP, None);
        assert_eq!(
            wide_step,
            thumb_scale_for_panel(
                &input,
                &uniform_aspects(16),
                inner,
                budget,
                THUMB_ROW_GAP,
                None
            )
        );
    }

    #[test]
    fn the_automatic_step_ignores_the_window_order() {
        // `pack_rows` minimises leftover width in input order, so the previous selection could give
        // the same set two different steps depending on which window was frontmost (measured
        // 2026-10-07: 11 windows chose 1.10 in one MRU order and 1.00 in another). The estimate must
        // be a function of the multiset only.
        let panel_w = 1470.0 * PANEL_MAX_WIDTH_RATIO;
        let inner = panel_w - H_PADDING * 2.0 - THUMB_SCROLLBAR_W;
        let budget = 923.0 - 2.0 * PANEL_MARGIN;
        let set = [
            1.593, 1.593, 1.341, 1.593, 1.593, 0.838, 1.593, 1.593, 1.593, 1.593, 1.2,
        ];
        let input = band_present_input();
        let chosen = thumb_scale_for_panel(&input, &set, inner, budget, THUMB_ROW_GAP, None);
        // Every rotation and the reversed order are the same set and must choose the same step.
        for shift in 0..set.len() {
            let mut rotated = set;
            rotated.rotate_left(shift);
            assert_eq!(
                thumb_scale_for_panel(&input, &rotated, inner, budget, THUMB_ROW_GAP, None),
                chosen,
                "rotation by {shift} changed the step"
            );
        }
        let mut reversed = set;
        reversed.reverse();
        assert_eq!(
            thumb_scale_for_panel(&input, &reversed, inner, budget, THUMB_ROW_GAP, None),
            chosen,
            "reversing the order changed the step"
        );
    }

    #[test]
    fn a_pinned_size_skips_the_search() {
        let panel_w = 1470.0 * PANEL_MAX_WIDTH_RATIO;
        let inner = panel_w - H_PADDING * 2.0 - THUMB_SCROLLBAR_W;
        let budget = 923.0 - 2.0 * PANEL_MARGIN;
        // A pinned step is returned as-is even where the automatic search would refuse it (here the
        // largest step with 40 windows, which overflows and therefore scrolls).
        let input = band_present_input();
        let many = uniform_aspects(40);
        assert_eq!(
            thumb_scale_for_panel(&input, &many, inner, budget, THUMB_ROW_GAP, Some(1.2)),
            1.2
        );
        assert_eq!(
            thumb_scale_for_panel(&input, &many, inner, budget, THUMB_ROW_GAP, Some(0.75)),
            0.75
        );
        // The floor still applies, so a hand-edited value below it cannot produce an unreadable card.
        assert_eq!(
            thumb_scale_for_panel(&input, &many, inner, budget, THUMB_ROW_GAP, Some(0.1)),
            THUMB_MIN_SCALE
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
        let input = fixed_input();
        assert!(thumb_card_h_for_scale(&input, 0.8) < thumb_card_h_for_scale(&input, 0.85));
        assert!(thumb_card_h_for_scale(&input, 0.75) < thumb_card_h_for_scale(&input, 0.8));
        // The smallest step stays legible: its preview area is above the floor.
        assert!(thumb_preview_h(&input, thumb_card_h_for_scale(&input, THUMB_MIN_SCALE)) > 40.0);
    }

    #[test]
    fn the_panel_margin_costs_a_step_not_a_row() {
        // Pins the band-present case (the numbers below are the 36pt footer band's): 36pt of extra
        // room legitimately buys a bigger step, so the snapshot carries the band rather than leaving
        // it to the developer's config file. The card text size is the base size the reference
        // numbers were measured at; a larger configured one makes taller cards and legitimately
        // buys a step back.
        let input = band_present_input();
        // The measured built-in screen (1470x956, 923 visible): after both margins the step should fall
        // to 0.75 instead of overflowing into scrolling.
        let panel_w = 1470.0 * PANEL_MAX_WIDTH_RATIO;
        let inner = panel_w - H_PADDING * 2.0 - THUMB_SCROLLBAR_W;
        let with_margin = thumb_scale_for_panel(
            &input,
            &uniform_aspects(16),
            inner,
            923.0 - 2.0 * PANEL_MARGIN,
            THUMB_ROW_GAP,
            None,
        );
        let without_margin = thumb_scale_for_panel(
            &input,
            &uniform_aspects(16),
            inner,
            923.0,
            THUMB_ROW_GAP,
            None,
        );
        assert!(
            (with_margin - 0.75).abs() < 1e-9,
            "with the margin subtracted the built-in display must land on 0.75, got {with_margin}"
        );
        assert!(
            with_margin <= without_margin,
            "the margin can only lower or keep the step ({with_margin} vs {without_margin})"
        );
        // The independent 15pt footer leaves enough room for a 0.95 card step on this display.
        let wide = 1920.0 * PANEL_MAX_WIDTH_RATIO;
        let wide_inner = wide - H_PADDING * 2.0 - THUMB_SCROLLBAR_W;
        let on_wide = thumb_scale_for_panel(
            &input,
            &uniform_aspects(16),
            wide_inner,
            1050.0 - 2.0 * PANEL_MARGIN,
            THUMB_ROW_GAP,
            None,
        );
        assert!(
            (on_wide - 0.95).abs() < 1e-9,
            "with the margin subtracted the wide screen must land on 0.95, got {on_wide}"
        );
    }

    #[test]
    fn closing_cards_through_the_reflow_never_grows_the_panel() {
        // Measured: closing a window while the switcher is open made the panel taller (the reflow used
        // its own old formula: 4 rows + teaser = 901pt > budget 875). Closing from 25 cards down to 1
        // through the real entry points must keep the panel monotone non-increasing and inside the
        // budget.
        let input = band_present_input();
        let budget = 923.0 - 2.0 * PANEL_MARGIN;
        let card_h = thumb_card_h_for_scale(&input, THUMB_MIN_SCALE);
        let gap = THUMB_ROW_GAP;
        let panel_w = 1470.0 * PANEL_MAX_WIDTH_RATIO;
        let max_inner = panel_w - H_PADDING * 2.0 - THUMB_SCROLLBAR_W;
        let max_rows = thumb_max_rows(&input, card_h, budget, gap);
        let teaser_fits = thumb_teaser_fits(&input, card_h, budget, gap);
        let cap = thumb_card_w_for_aspect(&input, card_h, THUMB_PREVIEW_RATIO).min(max_inner);
        let start = 25usize;
        let initial = plan_thumb_scroll_layout_at_scale(
            &input,
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
                0.0,
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
                &input,
                rows,
                overflowed,
                ThumbPanelMetrics {
                    card_h,
                    gap,
                    teaser_fits,
                    max_panel_h: budget,
                },
                max_rows,
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
                + THUMB_BOTTOM_INSET
                + rendered_rows as f64 * card_h
                + rendered_rows.saturating_sub(1) as f64 * gap
                + teaser_h
                + footer_band_h(&input);
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

    /// The bug this pins: the last row used to sit flush against the status footer, and the card
    /// viewport -- which ends where the footer begins -- clipped the bottom of the ring the selected
    /// card draws outside itself. Every production layout must leave its selection plate's outward
    /// extent below its bottom-most card, and it must survive scrolling to the document's end (where
    /// the document's bottom edge is the viewport's).
    #[test]
    fn the_bottom_row_clears_the_footer() {
        // The card frame's own y in document coordinates is what the clip view compares against:
        // placements carry panel coordinates (the footer included), so subtracting `footer_band_h()`
        // gives the offset above the viewport's bottom edge. Each layout's requirement comes from its
        // own plate's geometry -- `SELECTION_CARD_INSET` for the icon-only plate, `OVERLAY_RING_INSET`
        // for the thumbnail ring -- not from the inset constant, or shrinking the constant would
        // satisfy the assertion.
        // The icon-only plate sits at the card's own edge, so only the thumbnail ring draws
        // outside a card; that is what the requirement below comes from.
        let input = band_present_input();
        let drawn_outside_the_card = OVERLAY_RING_INSET;
        let room_below_the_last_card = |placements: &[ThumbPlacement]| {
            placements
                .iter()
                .map(|placement| placement.y)
                .fold(f64::INFINITY, f64::min)
                - footer_band_h(&input)
        };

        // Icon-only, everything fits: the document's bottom-most card is on screen and no scrolling
        // can move it away from the viewport's edge.
        let icon = plan_icon_scroll_layout(&input, 4, 1512.0, 900.0, THUMB_SCROLLBAR_W, 0.0);
        assert!(!icon.overflowed);
        let icon_room = room_below_the_last_card(&icon.document_placements);
        assert!(
            icon_room >= 0.0,
            "icon layout: only {icon_room:.1}pt below the last row"
        );

        // Thumbnails, scrolled to the very end: the same edge is reached by scrolling, so the inset
        // belongs to the document height and not only to the initial viewport.
        let aspects = vec![THUMB_PREVIEW_RATIO; 12];
        let scrolled = plan_thumb_scroll_layout_with_max_card_w(
            &input,
            &aspects,
            1200.0,
            1400.0,
            600.0,
            THUMB_ROW_GAP,
            THUMB_SCROLLBAR_W,
            f64::MAX,
            |_| f64::INFINITY,
        );
        assert!(scrolled.overflowed);
        let scrolled_room = room_below_the_last_card(&scrolled.document_placements);
        assert!(
            scrolled_room >= drawn_outside_the_card - 1e-9,
            "thumbnail layout: only {scrolled_room:.1}pt below the last row, the selection ring \
             draws {drawn_outside_the_card:.1}pt below the card"
        );
    }

    #[test]
    fn panels_hug_the_cards_and_stay_within_the_height_budget() {
        // Requested: the overlay is capped by the usable area (minus the margin), and inside that cap
        // its actual size follows the real card sizes. Three rows or more used to be stretched to the
        // cap, which decoupled the size from the cards and added a blank band at the top.
        let input = band_present_input();
        let budget = 923.0 - 2.0 * PANEL_MARGIN;
        let card_h = thumb_card_h_for_scale(&input, 0.75);
        // Pure part: the panel height is the content height and the inset is always the mockup's top
        // inset, no matter the row count.
        let metrics = ThumbPanelMetrics {
            card_h,
            gap: THUMB_ROW_GAP,
            teaser_fits: false,
            max_panel_h: budget,
        };
        for rows in 1..=thumb_max_rows(&input, card_h, budget, THUMB_ROW_GAP) {
            let (panel_h, inset) = thumb_panel_metrics(&input, rows, metrics, 0.0);
            assert!(
                (inset - THUMB_TOP_INSET).abs() < 1e-9,
                "{rows} rows: top inset {inset:.1} must be {THUMB_TOP_INSET} (no stretching)"
            );
            assert!(
                (panel_h
                    - thumb_panel_height_for_rows(
                        &input,
                        rows,
                        rows,
                        card_h,
                        THUMB_ROW_GAP,
                        false
                    ))
                .abs()
                    < 1e-9,
                "{rows} rows: panel {panel_h:.1} must equal the content height"
            );
            assert!(panel_h <= budget + 1e-9);
        }
        // Backstop: with a budget too small for one row plus the teaser the panel still may not cross
        // the cap.
        let tiny = thumb_panel_metrics(
            &input,
            1,
            ThumbPanelMetrics {
                max_panel_h: 300.0,
                ..metrics
            },
            THUMB_ROW_GAP + card_h / 3.0,
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
        let cap = thumb_card_w_for_aspect(&input, card_h, THUMB_PREVIEW_RATIO).min(inner);
        for count in [1usize, 6, 12, 24] {
            let plan = plan_thumb_scroll_layout_at_scale(
                &input,
                &vec![THUMB_PREVIEW_RATIO; count],
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
        let input = band_present_input();
        let budget = 923.0 - 2.0 * PANEL_MARGIN;
        let cap =
            |card_h: f64| thumb_card_w_for_aspect(&input, card_h, THUMB_PREVIEW_RATIO).min(inner);
        for (count, expect_overflow) in [(6usize, false), (24usize, true)] {
            let aspects = uniform_aspects(count);
            for step in [0.75, 0.8, 0.85, 1.0] {
                let layout = plan_thumb_scroll_layout_at_scale(
                    &input,
                    &aspects,
                    step,
                    inner,
                    panel_w,
                    budget,
                    THUMB_ROW_GAP,
                    THUMB_SCROLLBAR_W,
                    0.0,
                    cap(thumb_card_h_for_scale(&input, step)),
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
        let input = fixed_input();
        let layout = plan_thumb_flow_layout(&input, &[1.6, 1.6], 1, 1200.0, 1000.0, THUMB_ROW_GAP);
        assert_eq!(layout.visible, 0..2);
        assert!(!layout.overflowed);
        assert!((layout.scale - LEGACY_THUMB_MAX_SCALE).abs() < 1e-9);
        assert!(layout.card_h > thumb_card_h_fixed(&input));
    }

    #[test]
    fn scale_is_unchanged_when_aspects_cause_different_wrapping() {
        let input = fixed_input();
        let standard =
            plan_thumb_flow_layout(&input, &[1.6, 1.6, 1.6], 1, 1200.0, 1000.0, THUMB_ROW_GAP);
        let mixed =
            plan_thumb_flow_layout(&input, &[2.2, 0.7, 2.2], 1, 800.0, 1000.0, THUMB_ROW_GAP);
        assert_eq!(standard.scale, 1.4);
        assert_eq!(mixed.scale, 1.4);
    }

    #[test]
    fn overflow_uses_stable_pages_instead_of_sliding() {
        let input = fixed_input();
        let aspects = vec![1.6; 8];
        let base_h = thumb_card_h_for_scale(&input, thumb_scale_for_count(aspects.len()));
        let two_row_panel_h =
            thumb_panel_height_for_rows(&input, 2, 2, base_h, THUMB_ROW_GAP, true) + 0.1;
        let initial =
            plan_thumb_flow_layout(&input, &aspects, 1, 900.0, two_row_panel_h, THUMB_ROW_GAP);
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
        let card_w =
            thumb_card_w_for_aspect(&input, thumb_card_h_for_scale(&input, initial.scale), 1.6);
        assert_eq!(
            initial.panel_w,
            per_row as f64 * card_w + (per_row - 1) as f64 * THUMB_ROW_GAP + H_PADDING * 2.0
        );

        let next = plan_thumb_flow_layout(
            &input,
            &aspects,
            per_row * 2,
            900.0,
            two_row_panel_h,
            THUMB_ROW_GAP,
        );
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
        let input = fixed_input();
        let aspects = vec![1.6; 12];
        let card_h = thumb_card_h_for_scale(&input, thumb_scale_for_count(aspects.len()));
        let three_row_panel_h = THUMB_TOP_INSET
            + THUMB_BOTTOM_INSET
            + card_h * 3.0
            + THUMB_ROW_GAP * 2.0
            + footer_band_h(&input)
            + 0.1;
        // Budgets derive from the *current* card width: the narrow one holds three columns (twelve
        // cards need four rows -> overflow) and the wide one holds four (twelve cards fit three rows ->
        // one page). The assertions therefore express the intent "horizontal budget decides the column
        // count" without depending on the ladder.
        let card_w = thumb_card_w_for_aspect(&input, card_h, 1.6);
        let inner_for_columns = |columns: f64| columns * card_w + (columns - 1.0) * THUMB_ROW_GAP;
        let capped = plan_thumb_flow_layout(
            &input,
            &aspects,
            1,
            inner_for_columns(3.0),
            three_row_panel_h,
            THUMB_ROW_GAP,
        );
        let wide = plan_thumb_flow_layout(
            &input,
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
        let input = fixed_input();
        let aspects = vec![1.6; 8];
        let base_h = thumb_card_h_for_scale(&input, thumb_scale_for_count(aspects.len()));
        let max_h = thumb_panel_height_for_rows(&input, 2, 2, base_h, THUMB_ROW_GAP, true) + 0.1;
        for selected in 4..8 {
            let layout =
                plan_thumb_flow_layout(&input, &aspects, selected, 614.0, max_h, THUMB_ROW_GAP);
            assert_eq!(layout.visible, 4..8);
        }
    }

    #[test]
    fn mixed_aspect_pages_are_contiguous_exhaustive_and_stable() {
        let input = fixed_input();
        let aspects = vec![2.2, 0.7, 1.6, 2.2, 1.0, 1.6, 0.7, 2.2, 1.6];
        let scale = thumb_scale_for_count(aspects.len());
        let card_h = thumb_card_h_for_scale(&input, scale);
        let constraints = ThumbFlowConstraints {
            card_h,
            max_inner: 614.0,
            max_rows: 2,
            teaser_fits: true,
            max_panel_h: thumb_panel_height_for_rows(&input, 2, 2, card_h, THUMB_ROW_GAP, true),
            gap: THUMB_ROW_GAP,
        };
        let pages = stable_pages(&input, &aspects, constraints);
        assert_eq!(pages.first().unwrap().start, 0);
        assert_eq!(pages.last().unwrap().end, aspects.len());
        for pair in pages.windows(2) {
            assert_eq!(pair[0].end, pair[1].start);
        }
        for page in &pages {
            assert!(!page.is_empty());
            assert!(thumb_range_fits(&input, &aspects, page, constraints));
            for selected in page.clone() {
                let layout = plan_thumb_flow_layout(
                    &input,
                    &aspects,
                    selected,
                    constraints.max_inner,
                    THUMB_TOP_INSET
                        + THUMB_BOTTOM_INSET
                        + constraints.max_rows as f64 * card_h
                        + THUMB_ROW_GAP
                        + footer_band_h(&input)
                        + 1.0,
                    THUMB_ROW_GAP,
                );
                assert_eq!(&layout.visible, page);
            }
        }
    }

    #[test]
    fn overflow_capacity_adapts_to_mixed_aspects() {
        let input = fixed_input();
        let aspects = vec![1.6, 1.6, 2.2, 2.2, 2.2, 2.2];
        let card_h = thumb_card_h_for_scale(&input, thumb_scale_for_count(aspects.len()));
        let max_h = thumb_panel_height_for_rows(&input, 2, 2, card_h, THUMB_ROW_GAP, true) + 0.1;
        let initial = plan_thumb_flow_layout(&input, &aspects, 1, 700.0, max_h, THUMB_ROW_GAP);
        // Two standard cards share a row while a wide card occupies its own, so
        // the leading slice naturally drops to three items.
        assert_eq!(initial.visible, 0..3);
    }

    #[test]
    fn scroll_layout_caps_wide_cards_to_the_reference_width() {
        let layout = plan_thumb_scroll_layout_with_max_card_w(
            &fixed_input(),
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
        let layout = plan_thumb_flow_layout(&fixed_input(), &[], 0, 1200.0, 1000.0, THUMB_ROW_GAP);
        assert_eq!(layout.visible, 0..0);
        assert_eq!(layout.scale, 1.0);
        assert!(layout.placements.is_empty());
    }

    #[test]
    fn scrolling_layout_keeps_rows_and_panel_size_stable() {
        let input = fixed_input();
        let aspects = vec![1.6; 20];
        let card_h = thumb_card_h_for_scale(&input, thumb_scale_for_count(aspects.len()));
        let max_h = thumb_panel_height_for_rows(&input, 2, 2, card_h, THUMB_ROW_GAP, true) + 0.1;
        let first = plan_thumb_scroll_layout_at_scale(
            &input,
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
            &input,
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
        let input = fixed_input();
        let aspects = vec![1.6; 8];
        let card_h = thumb_card_h_for_scale(&input, thumb_scale_for_count(aspects.len()));
        let max_h = thumb_panel_height_for_rows(&input, 2, 2, card_h, THUMB_ROW_GAP, true) + 0.1;
        let layout = plan_thumb_scroll_layout_at_scale(
            &input,
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
        let card_w =
            thumb_card_w_for_aspect(&input, thumb_card_h_for_scale(&input, layout.scale), 1.6);
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
        let input = fixed_input();
        let aspects = vec![1.6; 13];
        let card_h = thumb_card_h_for_scale(&input, thumb_scale_for_count(aspects.len()));
        let max_h = thumb_panel_height_for_rows(&input, 3, 3, card_h, THUMB_ROW_GAP, true) + 0.1;
        let layout = plan_thumb_scroll_layout_at_scale(
            &input,
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
        let input = fixed_input();
        let aspects = vec![2.2, 0.7, 0.7];
        let card_h = thumb_card_h_for_scale(&input, thumb_scale_for_count(aspects.len()));
        let max_h = thumb_panel_height_for_rows(&input, 2, 2, card_h, THUMB_ROW_GAP, true) + 0.1;
        let layout = plan_thumb_scroll_layout_at_scale(
            &input,
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
        let input = fixed_input();
        let aspects = vec![1.6; 20];
        let card_h = thumb_card_h_for_scale(&input, thumb_scale_for_count(aspects.len()));
        let max_h = thumb_panel_height_for_rows(&input, 2, 2, card_h, THUMB_ROW_GAP, true) + 0.1;
        let layout = plan_thumb_scroll_layout_at_scale(
            &input,
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
        let input = fixed_input();
        let aspects = vec![1.6; 20];
        let card_h = thumb_card_h_for_scale(&input, thumb_scale_for_count(aspects.len()));
        let row_pitch = card_h + THUMB_ROW_GAP;
        let max_h = thumb_panel_height_for_rows(&input, 2, 2, card_h, THUMB_ROW_GAP, true) + 0.1;
        let layout = plan_thumb_scroll_layout_at_scale(
            &input,
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
            &input,
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
            &input,
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
        let (placements, rows, panel_w, overflowed) = plan_thumb_close_reflow(
            &widths,
            80.0,
            220.0,
            10.0,
            180.0,
            THUMB_TOP_INSET,
            false,
            3,
            0.0,
        );

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
        let (placements, rows, panel_w, overflowed) = plan_thumb_close_reflow(
            &widths,
            80.0,
            320.0,
            10.0,
            260.0,
            THUMB_TOP_INSET,
            true,
            2,
            0.0,
        );

        assert_eq!(rows, vec![0..3, 3..6, 6..9]);
        assert_eq!(placements[3].y, placements[0].y - 90.0);
        assert_eq!(placements[6].y, placements[3].y - 90.0);
        assert_eq!(panel_w, 398.0);
        assert!(overflowed);
    }

    #[test]
    fn close_reflow_panel_height_shrinks_when_overflow_ends() {
        let input = band_present_input();
        let overflowing = thumb_panel_height_for_rows(&input, 3, 2, 80.0, 10.0, true);
        let fitting = thumb_panel_height_for_rows(&input, 2, 2, 80.0, 10.0, false);

        assert!(overflowing > fitting);
        assert_eq!(
            fitting,
            THUMB_TOP_INSET + THUMB_BOTTOM_INSET + 2.0 * 80.0 + 10.0 + footer_band_h(&input)
        );
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
    use super::tests::band_present_input;
    use super::*;

    #[test]
    fn icon_layout_balances_rows_when_the_document_fits() {
        let layout = plan_icon_scroll_layout(&band_present_input(), 10, 1200.0, 500.0, 14.0, 0.0);

        assert!(!layout.overflowed);
        assert_eq!(layout.row_ranges, vec![0..5, 5..10]);
        assert_eq!(layout.panel_w, 5.0 * ICON_CARD_W + H_PADDING * 2.0);
    }

    #[test]
    fn icon_cards_keep_fixed_size_and_three_card_baseline() {
        let input = band_present_input();
        let layout = plan_icon_scroll_layout(&input, 2, 1440.0, 900.0, 14.0, 0.0);

        assert_eq!(layout.panel_w, 3.0 * ICON_CARD_W + H_PADDING * 2.0);
        assert_eq!(layout.card_h, card_h(&input));
        assert_eq!(layout.document_placements.len(), 2);
        assert!(layout
            .document_placements
            .iter()
            .all(|placement| placement.width == ICON_CARD_W));
        assert_eq!(layout.max_scroll_offset, 0.0);
    }

    #[test]
    fn icon_layout_auto_columns_and_scrolls_by_rows() {
        let input = band_present_input();
        let layout = plan_icon_scroll_layout(&input, 15, 1200.0, 500.0, 14.0, 0.0);

        assert_eq!(layout.row_ranges, vec![0..7, 7..14, 14..15]);
        assert_eq!(layout.max_rows, 2);
        assert!(layout.overflowed);
        assert_eq!(layout.visible, 0..15);
        assert!(layout.max_scroll_offset > 0.0);
        assert_eq!(layout.document_placements.len(), 15);

        let bottom =
            plan_icon_scroll_layout(&input, 15, 1200.0, 500.0, 14.0, layout.max_scroll_offset);
        assert_eq!(bottom.row_start, 0);
        assert_eq!(bottom.visible, 0..15);
        assert_eq!(bottom.placements.len(), 15);
    }

    #[test]
    fn icon_layout_preserves_card_width_when_screen_is_narrow() {
        let layout = plan_icon_scroll_layout(&band_present_input(), 1, 200.0, 500.0, 14.0, 0.0);

        assert_eq!(layout.document_placements[0].width, ICON_CARD_W);
        assert!(layout.panel_w >= ICON_CARD_W + H_PADDING * 2.0);
    }
}
