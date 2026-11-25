//! Pure geometry for the Dock preview panel.
//!
//! Everything here is expressed in **panel-local, top-left** coordinates (x grows right, y grows
//! down), the same convention as CoreGraphics screen rectangles, so the anchor math and the card
//! layout can be asserted headlessly. The AppKit boundary converts once per view: the card host is
//! a flipped view, so its subviews take these rectangles unchanged.
//!
//! The panel has two modes, both chosen from the window count alone (never from the space left on
//! screen): a single row of thumbnail cards while the count is at or under `cards_per_row`, and a
//! scrolling list of compact rows above it. Screen space decides how much of that one layout is
//! visible and how much can scroll, not which layout is used.
//!
//! The size derivation runs in one chain and each step subtracts only its own allowance:
//!
//! ```text
//! space_budget(anchor, edge, visible)  -> the largest glass area the Dock leaves room for
//! panel_size(...)                      -> the glass area itself (never larger than the budget)
//! viewport                             -> glass minus PANEL_PAD on each side (the one padding step)
//! row_boxes(row_w)                     -> icon, close slot, then whatever is left for the title
//! ```
//!
//! `CARD_H`, the header height and the thumbnail height are derived here so every caller reads the
//! same numbers and the measured layout, the drawn frames and the e2e answer cannot disagree.

/// One rectangle in top-left coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rect {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) w: f64,
    pub(crate) h: f64,
}

impl Rect {
    pub(crate) const fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    pub(crate) fn max_x(self) -> f64 {
        self.x + self.w
    }

    pub(crate) fn max_y(self) -> f64 {
        self.y + self.h
    }

    pub(crate) fn contains(self, point: (f64, f64)) -> bool {
        point.0 >= self.x && point.0 < self.max_x() && point.1 >= self.y && point.1 < self.max_y()
    }

    /// Whether two rectangles share any positive area. Touching edges do not count.
    pub(crate) fn overlaps(self, other: Rect) -> bool {
        self.x < other.max_x()
            && other.x < self.max_x()
            && self.y < other.max_y()
            && other.y < self.max_y()
    }
}

/// Which edge of the screen the Dock sits on. macOS offers left, bottom and right; the top variant
/// exists for the layout tests and for a Dock forced to the top by a side effect of display setup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DockEdge {
    Bottom,
    Top,
    Left,
    Right,
}

impl DockEdge {
    /// The edge as the e2e layer states it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Bottom => "bottom",
            Self::Top => "top",
            Self::Left => "left",
            Self::Right => "right",
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------------------------

// Card metrics: one 4pt-grid stack of padding / header / thumbnail. Sizes are points. The padding
// is deliberately the small step: the cards have no fill of their own (the panel material is the
// surface), so anything larger reads as empty space around the thumbnail rather than around a card.
pub(crate) const CARD_W: f64 = 216.0;
pub(crate) const CARD_H: f64 = 164.0;
pub(crate) const CARD_PAD: f64 = 4.0;
pub(crate) const HEADER_H: f64 = 32.0;
pub(crate) const THUMB_H: f64 = CARD_H - CARD_PAD * 2.0 - HEADER_H;
pub(crate) const CARD_ICON: f64 = 16.0;
pub(crate) const CARD_ICON_GAP: f64 = 6.0;
pub(crate) const CLOSE_SIZE: f64 = 20.0;
pub(crate) const CLOSE_MARGIN: f64 = 4.0;
pub(crate) const PANEL_PAD: f64 = 4.0;
/// Cards sit edge to edge, like the reference: the panel's material is the surface, and a card is
/// a header plus its thumbnail rather than a separate filled tile with a gutter around it.
pub(crate) const CARD_GAP: f64 = 0.0;
/// The gap between two ROWS of cards. The panel's own surface shows through it, so cards in
/// different rows do not read as one tall card.
pub(crate) const GRID_ROW_GAP: f64 = 8.0;
// Nearly square corners: the reference taskbar preview's own corner is a few pixels on a card
// several hundred wide, so the Dock panel keeps only a hint of rounding (the switcher's 8pt
// control step) rather than the rounder panel shape its glass can carry.
pub(crate) const PANEL_RADIUS: f64 = 8.0;
pub(crate) const CARD_RADIUS: f64 = 6.0;
/// The hovered card's plate: brightness only, no accent fill and no rim. The wash is deliberately
/// stronger than the shared `hover_bg` (0xFFFFFF1A) so it reads as a brighter card against the
/// glass -- the design's "Windows-style" selected state (a little brighter than the unselected
/// card) instead of a blue selection. It draws no border: the whole card brightens as one piece,
/// and a hairline around it read as a separate outlined box rather than the card's own state.
pub(crate) const HOVER_PLATE_FILL: u32 = 0xFFFFFF8C;
pub(crate) const THUMB_RADIUS: f64 = 4.0;
/// The narrowest a single-row card may be compressed to: a 120pt thumbnail plus its padding, which
/// is what keeps a truncated title and the close slot readable side by side. Compressing past this
/// switches the row to horizontal scrolling instead.
pub(crate) const CARD_MIN_W: f64 = 120.0 + CARD_PAD * 2.0;

/// The gap between the glass and the edge of the visible frame. It is what keeps the panel off a
/// permanent Dock, which `visible_frame_of` does not carve out (it reserves the menu bar only, and
/// an auto-hidden Dock parks itself outside the screen).
pub(crate) const PANEL_MARGIN: f64 = 8.0;
/// Distance between the Dock tile and the panel's facing edge.
///
/// Zero, not a positive gap: the Dock draws its app-name tooltip above the tile, and the tooltip's
/// small tail sits in the space between the tile and the panel. A gap left that tail visible below
/// the cards as a floating arrow. Touching the tile's top edge covers the tail without covering the
/// icon itself, so a pointer resting on the icon still hits the Dock.
pub(crate) const ANCHOR_GAP: f64 = 0.0;
pub(crate) const TOOLTIP_GAP: f64 = 8.0;
pub(crate) const TOOLTIP_MAX_W: f64 = 360.0;
pub(crate) const TOOLTIP_PAD: f64 = 10.0;
/// A dock icon can be as narrow as 32pt while the panel is hundreds of points wide; the transition
/// band is widened by this much on both sides of the panel so a diagonal move from the tile into
/// the panel stays inside it.
pub(crate) const TRANSITION_SLACK: f64 = 24.0;

// List metrics. One row is the settings' single-line rhythm (a 32pt control band), which is what a
// taskbar jump list reads as: an icon, one line of title, and room for the close slot.
pub(crate) const ROW_H: f64 = 32.0;
pub(crate) const ROW_PAD: f64 = 8.0;
pub(crate) const ROW_ICON: f64 = 16.0;
pub(crate) const ROW_GAP: f64 = 8.0;
/// The list's fixed width: a narrow panel rather than one that grows with the widest window title.
pub(crate) const LIST_W: f64 = 304.0;
/// Below this the list is drawn in the compact arrangement instead: the title band shrinks to what
/// is left after the fixed pieces, and may disappear entirely (the row body then carries the
/// tooltip).
pub(crate) const LIST_W_MIN: f64 = 240.0;
/// The narrowest title band that is still worth drawing; under it the title gives up its space.
pub(crate) const MIN_TITLE_W: f64 = 32.0;
/// The fade drawn at a viewport edge that has more content beyond it.
pub(crate) const LIST_FADE_H: f64 = 12.0;
/// The smallest space in which the panel is still operable at all; below either of these it is not
/// shown, and the mode is NOT changed to fit (the mode follows the window count).
///
/// Each is derived from the controls that mode has to draw, not picked:
/// - width: a row's fixed pieces (padding, icon, two gaps, close slot) plus a one-point title band,
///   plus the glass padding both sides, since the budget is measured against the glass. A native
///   macOS Dock is usually 44-70pt thick, so anything narrower is not a Dock at all; the compact row
///   is what makes a narrow-but-real case still work.
/// - height: half a list row, which is the smallest viewport that can still scroll towards the rest.
///   The single-row layout is far taller (`CARD_H`), and its own minimum is checked after this one:
///   a budget that cannot hold a card row does not silently become a list, it hides the panel.
pub(crate) const MIN_INTERACTIVE_W: f64 = ROW_FIXED_W + 1.0 + PANEL_PAD * 2.0;
pub(crate) const MIN_INTERACTIVE_H: f64 = ROW_H / 2.0 + PANEL_PAD * 2.0;

/// One card's fixed pieces on a list row, before the remainder is handed to the title.
const ROW_FIXED_W: f64 = ROW_PAD + ROW_ICON + ROW_GAP + CLOSE_SIZE + ROW_GAP + ROW_PAD;

// ---------------------------------------------------------------------------------------------
// Mode and space
// ---------------------------------------------------------------------------------------------

/// Which of the two layouts the panel uses. Chosen from the window count alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PanelMode {
    /// One row of thumbnail cards, as wide as their windows' shapes make them.
    Row,
    /// A vertical list of compact rows, scrolling inside its viewport.
    List,
}

impl PanelMode {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Row => "row",
            Self::List => "list",
        }
    }
}

/// Whether the count exceeds the configured threshold. `cards_per_row` is the user's setting; a
/// threshold of 0 or 1 is legal (one window stays a row, two already need the list).
pub(crate) fn mode_for(card_count: usize, cards_per_row: usize) -> PanelMode {
    if card_count > cards_per_row {
        PanelMode::List
    } else {
        PanelMode::Row
    }
}

/// The room the Dock leaves for the panel, already reduced by [`PANEL_MARGIN`].
///
/// `visible` is the screen's visible frame as the Dock preview computes it: the menu bar is carved
/// out, a permanent Dock is not, and an auto-hidden one is outside the screen. The budget therefore
/// measures from the anchor tile towards the opposite edge, which is the direction the panel grows
/// in; it never measures the side the tile itself occupies.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SpaceBudget {
    /// The widest glass area the anchor leaves.
    pub(crate) inner_w: f64,
    /// The tallest glass area the anchor leaves.
    pub(crate) inner_h: f64,
}

pub(crate) fn space_budget(anchor: Rect, edge: DockEdge, visible: Rect) -> SpaceBudget {
    let margin = PANEL_MARGIN;
    match edge {
        DockEdge::Bottom => SpaceBudget {
            inner_w: (visible.w - margin * 2.0).max(0.0),
            inner_h: (anchor.y - visible.y - margin).max(0.0),
        },
        DockEdge::Top => SpaceBudget {
            inner_w: (visible.w - margin * 2.0).max(0.0),
            inner_h: (visible.max_y() - anchor.max_y() - margin).max(0.0),
        },
        // A left-hand Dock grows the panel to the RIGHT of the tile.
        DockEdge::Left => SpaceBudget {
            inner_w: (visible.max_x() - anchor.max_x() - margin).max(0.0),
            inner_h: (visible.h - margin * 2.0).max(0.0),
        },
        DockEdge::Right => SpaceBudget {
            inner_w: (anchor.x - visible.x - margin).max(0.0),
            inner_h: (visible.h - margin * 2.0).max(0.0),
        },
    }
}

/// Which axis the card layout scrolls along.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowAxis {
    /// Everything fits: no scrolling.
    None,
    /// One row of cards is wider than the panel, and it scrolls sideways.
    Horizontal,
    /// Cards wrapped into rows that are taller than the panel, and it scrolls down.
    Vertical,
}

impl RowAxis {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Horizontal => "horizontal",
            Self::Vertical => "vertical",
        }
    }
}

/// One scrolling axis, as `scroll_axis_for` names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Axis {
    X,
    Y,
}

/// The card rectangles of the card layout, in content coordinates.
///
/// Cards wrap into as many rows as the panel's width needs: one row while they fit, more when they
/// do not. `columns` is the number of cards on the widest row, which is what the hover plates need
/// to know which edges of a card are interior.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RowLayout {
    pub(crate) cards: Vec<Rect>,
    pub(crate) axis: RowAxis,
    pub(crate) content_w: f64,
    pub(crate) content_h: f64,
    /// Cards on the widest row (at least 1).
    pub(crate) columns: usize,
}

/// One list row's box in content coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RowSlot {
    pub(crate) row: Rect,
    pub(crate) icon: Rect,
    pub(crate) title: Rect,
    pub(crate) close: Rect,
    /// The title band is too narrow to draw; the whole row body carries the tooltip instead.
    pub(crate) title_hidden: bool,
}

/// The list layout inside its viewport.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ListLayout {
    pub(crate) content_w: f64,
    pub(crate) content_h: f64,
    pub(crate) viewport_h: f64,
    /// Rows that fit entirely inside the viewport. Zero is legal: the viewport then shows part of
    /// a row and scrolling still reaches it.
    pub(crate) full_rows: usize,
    pub(crate) max_scroll: f64,
    pub(crate) compact: bool,
}

/// Everything the panel needs to place itself and its content for one request.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PanelLayout {
    /// The glass area in screen coordinates (the window frame adds the elevation insets).
    pub(crate) panel: Rect,
    /// The scrolling viewport, in panel-local coordinates.
    pub(crate) viewport: Rect,
    pub(crate) mode: PanelMode,
    pub(crate) row: Option<RowLayout>,
    pub(crate) list: Option<ListLayout>,
}

/// The card width one window takes in a single row: the card's height is fixed, so the thumbnail
/// keeps its height and the width follows the window -- the same rule the app switcher uses, at
/// this module's smaller scale. A missing/degenerate aspect falls back to the design width. The
/// clamp keeps an ultra-wide or portrait window from producing an unusable card.
pub(crate) fn card_width_for(aspect: Option<f64>) -> f64 {
    let Some(aspect) = aspect.filter(|aspect| aspect.is_finite() && *aspect > 0.05) else {
        return CARD_W;
    };
    let thumb_w = (THUMB_H * aspect)
        .round()
        .clamp(CARD_MIN_W - CARD_PAD * 2.0, 360.0);
    thumb_w + CARD_PAD * 2.0
}

pub(crate) fn widths_for_aspects(aspects: &[Option<f64>]) -> Vec<f64> {
    aspects
        .iter()
        .map(|aspect| card_width_for(*aspect))
        .collect()
}

/// The full layout for one request, or `None` when the Dock leaves no operable space at all.
pub(crate) fn layout_for(
    mode: PanelMode,
    aspects: &[Option<f64>],
    card_count: usize,
    anchor: Rect,
    edge: DockEdge,
    visible: Rect,
) -> Option<PanelLayout> {
    let budget = space_budget(anchor, edge, visible);
    if budget.inner_w < MIN_INTERACTIVE_W || budget.inner_h < MIN_INTERACTIVE_H {
        return None;
    }
    match mode {
        // A row of cards is a fixed 164pt tall plus its padding; under that it cannot be drawn, and
        // the panel hides rather than switching to the other layout.
        PanelMode::Row if budget.inner_h < CARD_H + PANEL_PAD * 2.0 + PANEL_MARGIN => None,
        PanelMode::Row => row_panel(aspects, card_count, anchor, edge, visible, budget),
        PanelMode::List => list_panel(card_count, anchor, edge, visible, budget),
    }
}

/// The card layout: cards in a row, wrapping into more rows when the width needs it.
///
/// One row is the common case and is laid out exactly as before (natural widths, compressed
/// proportionally only when the widest row would overflow). What changed is the overflow: instead
/// of scrolling a single row sideways, cards that do not fit wrap onto another row, and only a
/// grid taller than the room under the Dock scrolls -- downwards.
fn row_panel(
    aspects: &[Option<f64>],
    card_count: usize,
    anchor: Rect,
    edge: DockEdge,
    visible: Rect,
    budget: SpaceBudget,
) -> Option<PanelLayout> {
    let mut widths = widths_for_aspects(&aspects[..aspects.len().min(card_count)]);
    if widths.is_empty() {
        widths.push(CARD_W);
    }
    let usable = (budget.inner_w - PANEL_PAD * 2.0).max(1.0);
    // A card wider than the whole usable width is squeezed to it, and never under the minimum. When
    // even the minimum does not fit, the row keeps its cards at the minimum and scrolls sideways:
    // that is the only case where this layout still scrolls horizontally.
    let widths: Vec<f64> = widths
        .into_iter()
        .map(|width| {
            if width > usable {
                width.max(CARD_MIN_W).min(usable.max(CARD_MIN_W))
            } else {
                width
            }
        })
        .collect();
    // Wrap greedily: a card joins the current row while it still fits, otherwise it starts the next.
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut current_w = 0.0;
    for (index, width) in widths.iter().enumerate() {
        let gap = if current.is_empty() { 0.0 } else { CARD_GAP };
        if !current.is_empty() && current_w + gap + width > usable + 0.001 {
            rows.push(std::mem::take(&mut current));
            current_w = 0.0;
        }
        current_w += if current.is_empty() { 0.0 } else { CARD_GAP } + width;
        current.push(index);
    }
    if !current.is_empty() {
        rows.push(current);
    }
    let row_count = rows.len().max(1);
    let row_width: Vec<f64> = rows
        .iter()
        .map(|row| {
            row.iter().map(|&index| widths[index]).sum::<f64>()
                + (row.len().saturating_sub(1)) as f64 * CARD_GAP
        })
        .collect();
    let natural_w = row_width.iter().copied().fold(0.0, f64::max);
    let columns = rows.iter().map(Vec::len).max().unwrap_or(1).max(1);
    // Content coordinates are viewport-relative, exactly like the list layout's rows: the first
    // card starts at 0 and the glass padding comes solely from the viewport's own inset. Baking
    // the padding into the card positions as well shifted the whole row one padding step right and
    // down -- the leftover strip read wider on the left, which is what an off-centre thumbnail
    // actually was.
    let content_h = row_count as f64 * CARD_H + (row_count.saturating_sub(1)) as f64 * GRID_ROW_GAP;
    let scrolls_down = content_h + PANEL_PAD * 2.0 > budget.inner_h + 0.001;
    let scrolls_sideways = natural_w > usable + 0.001;
    let content_w = natural_w;
    let mut cards: Vec<Rect> = Vec::with_capacity(widths.len());
    for (row, indices) in rows.iter().enumerate() {
        let mut x = 0.0;
        let y = row as f64 * (CARD_H + GRID_ROW_GAP);
        for &index in indices {
            cards.push(Rect::new(x, y, widths[index], CARD_H));
            x += widths[index] + CARD_GAP;
        }
    }
    let panel_w = (content_w + PANEL_PAD * 2.0)
        .min(budget.inner_w)
        .max(MIN_INTERACTIVE_W);
    let panel_h = (content_h + PANEL_PAD * 2.0).min(budget.inner_h);
    let viewport_h = (panel_h - PANEL_PAD * 2.0).max(1.0);
    let axis = if scrolls_down {
        RowAxis::Vertical
    } else if scrolls_sideways {
        RowAxis::Horizontal
    } else {
        RowAxis::None
    };
    Some(PanelLayout {
        panel: panel_frame(anchor, (panel_w, panel_h), edge, visible),
        viewport: Rect::new(
            PANEL_PAD,
            PANEL_PAD,
            (panel_w - PANEL_PAD * 2.0).max(1.0),
            viewport_h,
        ),
        mode: PanelMode::Row,
        row: Some(RowLayout {
            cards,
            axis,
            content_w,
            content_h,
            columns,
        }),
        list: None,
    })
}

fn list_panel(
    card_count: usize,
    anchor: Rect,
    edge: DockEdge,
    visible: Rect,
    budget: SpaceBudget,
) -> Option<PanelLayout> {
    let compact = budget.inner_w < LIST_W_MIN;
    let width = budget.inner_w.clamp(1.0, LIST_W);
    let content_h = (card_count.max(1) as f64 * ROW_H).max(ROW_H);
    let glass_h = budget.inner_h.min(content_h + PANEL_PAD * 2.0);
    let viewport_h = (glass_h - PANEL_PAD * 2.0).max(0.0);
    if viewport_h < ROW_H / 2.0 {
        // Part of a row has to be visible, or there is nothing to scroll towards.
        return None;
    }
    let full_rows = (viewport_h / ROW_H).floor() as usize;
    Some(PanelLayout {
        panel: panel_frame(anchor, (width, glass_h), edge, visible),
        viewport: Rect::new(PANEL_PAD, PANEL_PAD, width - PANEL_PAD * 2.0, viewport_h),
        mode: PanelMode::List,
        row: None,
        list: Some(ListLayout {
            content_w: width - PANEL_PAD * 2.0,
            content_h,
            viewport_h,
            full_rows,
            max_scroll: (content_h - viewport_h).max(0.0),
            compact,
        }),
    })
}

/// The fixed pieces of one list row and the title band between them.
pub(crate) fn row_boxes(row_w: f64) -> RowSlot {
    let row = Rect::new(0.0, 0.0, row_w, ROW_H);
    let icon = Rect::new(ROW_PAD, (ROW_H - ROW_ICON) / 2.0, ROW_ICON, ROW_ICON);
    let close = Rect::new(
        row_w - ROW_PAD - CLOSE_SIZE,
        (ROW_H - CLOSE_SIZE) / 2.0,
        CLOSE_SIZE,
        CLOSE_SIZE,
    );
    let title_x = icon.max_x() + ROW_GAP;
    let title_w = (close.x - ROW_GAP - title_x).max(0.0);
    let title_hidden = title_w < MIN_TITLE_W;
    let title = if title_hidden {
        Rect::new(title_x, icon.y, 0.0, icon.h)
    } else {
        Rect::new(title_x, icon.y, title_w, icon.h)
    };
    RowSlot {
        row,
        icon,
        title,
        close,
        title_hidden,
    }
}

/// The screen x-offset at which a row of `index` starts inside the content view.
pub(crate) fn row_top(index: usize) -> f64 {
    index as f64 * ROW_H
}

/// Strictly clamp a scroll offset so no part of the content leaves its viewport.
pub(crate) fn scroll_offset(current: f64, delta: f64, max_scroll: f64) -> f64 {
    if !current.is_finite() || !delta.is_finite() || max_scroll <= 0.0 {
        return 0.0;
    }
    (current + delta).clamp(0.0, max_scroll)
}

/// Which axis a wheel or trackpad gesture scrolls, and by how much.
///
/// The absolute value only selects the axis (a trackpad reports both deltas and the horizontal one
/// may be tiny); the delta that reaches [`scroll_offset`] keeps its sign, so the two wheel
/// directions still travel to opposite ends.
pub(crate) fn scroll_axis_for(
    mode: PanelMode,
    row_axis: RowAxis,
    pointer_in_viewport: bool,
    max_scroll: f64,
    dx: f64,
    dy: f64,
) -> Option<(Axis, f64)> {
    if !pointer_in_viewport || max_scroll <= 0.0 {
        return None;
    }
    match (mode, row_axis) {
        (PanelMode::Row, RowAxis::Horizontal) => {
            if dx.abs() >= 1.0 {
                Some((Axis::X, dx))
            } else if dy.abs() >= 1.0 {
                Some((Axis::X, dy))
            } else {
                None
            }
        }
        // A wrapped grid scrolls down, and the wheel's vertical delta is the one that drives it.
        (PanelMode::Row, RowAxis::Vertical) => (dy.abs() >= 1.0).then_some((Axis::Y, dy)),
        (PanelMode::List, _) => (dy.abs() >= 1.0).then_some((Axis::Y, dy)),
        _ => None,
    }
}

/// Which viewport edges have content beyond them.
pub(crate) fn edge_fades(scroll: f64, max_scroll: f64) -> (bool, bool) {
    if max_scroll <= 0.0 {
        return (false, false);
    }
    (scroll > 0.5, scroll < max_scroll - 0.5)
}

/// Whether a panel-local point lies inside the viewport.
pub(crate) fn pointer_in_viewport(point: (f64, f64), viewport: Rect) -> bool {
    viewport.contains(point)
}

/// The content view's origin inside the viewport: the scroll offset and nothing else.
///
/// The content is a child of the viewport, so its own coordinates already start at the viewport's
/// origin. Adding that origin again would shift everything by the padding a second time -- the
/// defect that made a drawn close button and its hit area disagree.
pub(crate) fn content_origin(scroll: (f64, f64)) -> (f64, f64) {
    (-scroll.0, -scroll.1)
}

// ---------------------------------------------------------------------------------------------
// Cards inside a row
// ---------------------------------------------------------------------------------------------

/// The card's header band: the icon, the title and the close-slot live here.
pub(crate) fn header_rect(card: Rect) -> Rect {
    Rect::new(card.x, card.y, card.w, HEADER_H + CARD_PAD)
}

/// The app-icon slot, vertically centred in the header.
pub(crate) fn icon_rect(card: Rect) -> Rect {
    let header = header_rect(card);
    Rect::new(
        card.x + CARD_PAD,
        header.y + (header.h - CARD_ICON) / 2.0,
        CARD_ICON,
        CARD_ICON,
    )
}

/// The close slot. It is reserved whether or not the button is visible, so showing the button on
/// hover never moves the title; a hidden slot is not part of hit-testing (see `hit`).
pub(crate) fn close_rect(card: Rect) -> Rect {
    let header = header_rect(card);
    Rect::new(
        card.max_x() - CARD_PAD - CLOSE_SIZE,
        header.y + (header.h - CLOSE_SIZE) / 2.0,
        CLOSE_SIZE,
        CLOSE_SIZE,
    )
}

/// The single-line title's box: from after the icon to just before the close slot. The slot is
/// reserved whether or not the button is visible, so a hover never reflows the title.
pub(crate) fn title_rect(card: Rect) -> Rect {
    let icon = icon_rect(card);
    let close = close_rect(card);
    let x = icon.max_x() + CARD_ICON_GAP;
    Rect::new(x, icon.y, (close.x - CLOSE_MARGIN - x).max(1.0), icon.h)
}

/// The thumbnail box below the header.
pub(crate) fn thumb_rect(card: Rect) -> Rect {
    Rect::new(
        card.x + CARD_PAD,
        card.y + CARD_PAD + HEADER_H,
        (card.w - CARD_PAD * 2.0).max(1.0),
        THUMB_H,
    )
}

/// The frame a captured image takes inside its thumbnail box: scaled down to fit with its aspect
/// kept, then centred on the box on both axes.
///
/// The panel positions the image view itself instead of relying on `resizeAspect` layer gravity,
/// because that is the one thing the gravity does not guarantee in a layer-backed view -- measured
/// on macOS 26 the contents sat several points off centre, so the leftover space around a
/// thumbnail read wider on one side. Doing the fit here makes the centring exact by construction,
/// the same answer the switcher's `fit_size` placement arrives at.
pub(crate) fn centered_thumb_frame(content_w: f64, content_h: f64, thumb: Rect) -> Rect {
    if content_w <= 0.0 || content_h <= 0.0 || thumb.w <= 0.0 || thumb.h <= 0.0 {
        return thumb;
    }
    let scale = (thumb.w / content_w).min(thumb.h / content_h);
    let w = content_w * scale;
    let h = content_h * scale;
    Rect::new(
        thumb.x + (thumb.w - w) / 2.0,
        thumb.y + (thumb.h - h) / 2.0,
        w,
        h,
    )
}

/// The hovered card's plate: the card rect grown out to the panel edge on the sides it touches.
///
/// The plate covers the tile, not just the thumbnail: a bare card rect leaves the panel's padding
/// visible as a glass ring above/below every card (and beside the first/last one), which read as a
/// selection that stops short of the card. The grown plate also takes the panel's corner radius when
/// it reaches an edge, so it follows the glass instead of leaving a sliver in the corner.
pub(crate) fn hover_plate_rect(
    card: Rect,
    row: usize,
    column: usize,
    row_len: usize,
    grid_rows: usize,
    panel: (f64, f64),
) -> (Rect, f64) {
    let (panel_w, panel_h) = panel;
    let mut plate = card;
    let top_edge = row == 0;
    let bottom_edge = row + 1 >= grid_rows;
    let left_edge = column == 0;
    let right_edge = column + 1 >= row_len;
    // Content coordinates are viewport-relative, so the glass edge sits one PANEL_PAD beyond the
    // content origin: an edge card's plate grows that one step outward and the wash reaches the
    // glass instead of stopping at the viewport, which is what left an unhighlighted ring around
    // the hovered card. The plate can only draw there because the viewport stops clipping when
    // the panel does not scroll; a scrolling panel keeps the clip and the wash ends at the
    // viewport as before.
    if top_edge {
        plate.y = -PANEL_PAD;
        plate.h += PANEL_PAD;
    }
    if bottom_edge {
        plate.h += PANEL_PAD;
    }
    if left_edge {
        plate.x = -PANEL_PAD;
        plate.w += PANEL_PAD;
    }
    if right_edge {
        plate.w += PANEL_PAD;
    }
    // Clamp so an odd measurement can never push the plate outside the glass.
    plate.w = plate.w.min(panel_w - PANEL_PAD - plate.x);
    plate.h = plate.h.min(panel_h - PANEL_PAD - plate.y);
    let radius = if top_edge || bottom_edge || left_edge || right_edge {
        PANEL_RADIUS
    } else {
        CARD_RADIUS
    };
    (plate, radius)
}

/// The plates for the card layout, which may be a single row or several wrapped ones.
///
/// `columns` is the number of cards on the widest row: the plate of a card grows towards the
/// panel's edge only when the card sits on that edge (the first/last column, the first/last row),
/// so a wrapped grid keeps the same "the tile owns the strip out to the edge" look as one row.
pub(crate) fn plates_for_grid(
    cards: &[Rect],
    columns: usize,
    panel: (f64, f64),
) -> Vec<(Rect, f64)> {
    let columns = columns.max(1);
    let rows = cards.len().div_ceil(columns).max(1);
    cards
        .iter()
        .enumerate()
        .map(|(index, card)| {
            let (row, column) = (index / columns, index % columns);
            hover_plate_rect(*card, row, column, columns, rows, panel)
        })
        .collect()
}

/// The title and close boxes of one item, in CONTENT coordinates.
///
/// One function for both layouts, used by the drawing code and by the hit test: two boxes computed
/// in two places is how a click on a visible close button ended up charging the activate path.
pub(crate) fn boxes_for(mode: PanelMode, item: Rect) -> RowSlot {
    match mode {
        PanelMode::Row => RowSlot {
            row: item,
            icon: icon_rect(item),
            title: title_rect(item),
            close: close_rect(item),
            title_hidden: false,
        },
        PanelMode::List => {
            let boxes = row_boxes(item.w);
            let offset = |rect: Rect| Rect::new(rect.x + item.x, rect.y + item.y, rect.w, rect.h);
            RowSlot {
                row: item,
                icon: offset(boxes.icon),
                title: offset(boxes.title),
                close: offset(boxes.close),
                title_hidden: boxes.title_hidden,
            }
        }
    }
}

/// Whether a single-line title is wider than its box (and therefore needs the full-title tooltip).
pub(crate) fn title_is_truncated(measured_w: f64, box_w: f64) -> bool {
    measured_w.is_finite() && measured_w > box_w
}

/// Clamp `rect` so it lies inside `bounds`, when it is smaller. A rectangle larger than the bounds
/// keeps its top-left corner inside them; the caller decides whether to shrink the content.
pub(crate) fn clamp_inside(rect: Rect, bounds: Rect) -> Rect {
    let max_x = (bounds.max_x() - rect.w).max(bounds.x);
    let max_y = (bounds.max_y() - rect.h).max(bounds.y);
    Rect::new(
        rect.x.clamp(bounds.x, max_x),
        rect.y.clamp(bounds.y, max_y),
        rect.w,
        rect.h,
    )
}

/// The panel's screen rectangle (top-left CoreGraphics coordinates) for an anchor tile.
///
/// `anchor` is the Dock tile's rectangle, `size` the panel size, `screen` the screen's full frame
/// and `visible` its visible frame (menu bar and a permanent Dock excluded), all in the same
/// top-left coordinate space. The panel faces the Dock edge on the tile's centre line and is
/// clamped into the visible frame, so a tile near a screen corner still yields a fully visible panel.
pub(crate) fn panel_frame(anchor: Rect, size: (f64, f64), edge: DockEdge, visible: Rect) -> Rect {
    let (w, h) = size;
    let raw = match edge {
        DockEdge::Bottom => Rect::new(
            anchor.x + anchor.w / 2.0 - w / 2.0,
            anchor.y - ANCHOR_GAP - h,
            w,
            h,
        ),
        DockEdge::Top => Rect::new(
            anchor.x + anchor.w / 2.0 - w / 2.0,
            anchor.y + anchor.h + ANCHOR_GAP,
            w,
            h,
        ),
        DockEdge::Left => Rect::new(
            anchor.x + anchor.w + ANCHOR_GAP,
            anchor.y + anchor.h / 2.0 - h / 2.0,
            w,
            h,
        ),
        DockEdge::Right => Rect::new(
            anchor.x - ANCHOR_GAP - w,
            anchor.y + anchor.h / 2.0 - h / 2.0,
            w,
            h,
        ),
    };
    clamp_inside(raw, visible)
}

/// The full-title tooltip's screen rectangle: centred over the hovered item, above the panel, and
/// flipped below when the top of the screen has no room.
///
/// `item` is the hovered card (or list row) in screen coordinates, and it is the horizontal anchor
/// -- the panel is only the vertical reference. A row of three cards centred the tooltip over the
/// panel's middle, so hovering the third card showed the tooltip above the second one and it read
/// as that card's full title. Vertically the panel's facing edge stays the reference because it is
/// above every card even when the row has wrapped into a grid; with a single row it is above the
/// hovered card itself.
pub(crate) fn tooltip_frame(
    item: Rect,
    panel: Rect,
    tooltip: (f64, f64),
    edge: DockEdge,
    visible: Rect,
) -> Rect {
    let (w, h) = tooltip;
    let x = item.x + item.w / 2.0 - w / 2.0;
    let above = Rect::new(x, panel.y - TOOLTIP_GAP - h, w, h);
    let below = Rect::new(x, panel.max_y() + TOOLTIP_GAP, w, h);
    // Above is the default and the only option for a Dock on the bottom or the two sides. A top
    // Dock places the tooltip under its panel, and a panel near the top of the screen flips it too.
    let fits_above = above.y >= visible.y - 0.5;
    let fits_below = below.max_y() <= visible.max_y() + 0.5;
    let chosen = if edge != DockEdge::Top && fits_above {
        above
    } else if fits_below {
        below
    } else if fits_above {
        above
    } else {
        below
    };
    clamp_inside(chosen, visible)
}

/// The free gap between the Dock tile and the panel, widened sideways by [`TRANSITION_SLACK`].
///
/// The band deliberately stops at the tile's own edge on the Dock side and at the panel's edge on
/// the other, so it covers the crossing only: it never covers a neighbouring Dock tile, which is
/// what keeps a move over a separator/folder/tile decided by the tile hit instead of by the band.
pub(crate) fn transition_band(edge: DockEdge, anchor: Rect, panel: Rect) -> Rect {
    match edge {
        DockEdge::Bottom => {
            let y0 = panel.max_y().min(anchor.y);
            let y1 = anchor.y.max(panel.max_y());
            Rect::new(
                panel.x - TRANSITION_SLACK,
                y0,
                panel.w + TRANSITION_SLACK * 2.0,
                (y1 - y0).max(0.0),
            )
        }
        DockEdge::Top => {
            let y0 = anchor.max_y().min(panel.y);
            let y1 = panel.y.max(anchor.max_y());
            Rect::new(
                panel.x - TRANSITION_SLACK,
                y0,
                panel.w + TRANSITION_SLACK * 2.0,
                (y1 - y0).max(0.0),
            )
        }
        DockEdge::Left => Rect::new(
            anchor.max_x().min(panel.x),
            panel.y - TRANSITION_SLACK,
            (panel.x - anchor.max_x()).max(0.0),
            panel.h + TRANSITION_SLACK * 2.0,
        ),
        DockEdge::Right => Rect::new(
            panel.max_x().min(anchor.x),
            panel.y - TRANSITION_SLACK,
            (anchor.x - panel.max_x()).max(0.0),
            panel.h + TRANSITION_SLACK * 2.0,
        ),
    }
}

/// Which edge a Dock tile list hugs, from the list's frame and the screen's frame.
///
/// The Dock's list is a thin band along one edge; the smallest gap to a screen edge names it. A
/// list that is not on any edge (mid-reveal, another display's stale frame) answers `None`, which
/// the caller treats as "not usable for anchoring".
pub(crate) fn dock_edge_from_frames(list: Rect, screen: Rect) -> Option<DockEdge> {
    if list.w <= 1.0 || list.h <= 1.0 {
        return None;
    }
    // The list's own orientation already says which axis the Dock runs along: a wide, short list
    // hugs the top or the bottom, a tall, narrow one the left or the right. Testing both axes made
    // a mid-reveal horizontal list "hit" the side edges through its margin.
    let candidates = if list.w >= list.h {
        [
            ((list.y - screen.y).abs(), DockEdge::Top),
            ((screen.max_y() - list.max_y()).abs(), DockEdge::Bottom),
        ]
    } else {
        [
            ((list.x - screen.x).abs(), DockEdge::Left),
            ((screen.max_x() - list.max_x()).abs(), DockEdge::Right),
        ]
    };
    let (gap, edge) = if candidates[0].0 <= candidates[1].0 {
        candidates[0]
    } else {
        candidates[1]
    };
    // A list that is not within a dock's thickness of its edge is not an edge dock yet.
    (gap <= 64.0).then_some(edge)
}

/// Whether the Dock's list is actually on screen. An auto-hidden Dock parks its list just outside
/// the screen (measured: a 56pt list starting exactly at the screen's bottom edge), so an
/// intersection test alone is not enough -- most of the list must be inside.
pub(crate) fn dock_revealed(list: Rect, screen: Rect) -> bool {
    let overlapping_w = (list.max_x().min(screen.max_x()) - list.x.max(screen.x)).max(0.0);
    let overlapping_h = (list.max_y().min(screen.max_y()) - list.y.max(screen.y)).max(0.0);
    let area = list.w * list.h;
    area > 1.0 && (overlapping_w * overlapping_h) / area >= 0.8
}

/// The full-title tooltip's size for a wrapped title, bounded by [`TOOLTIP_MAX_W`].
pub(crate) fn tooltip_size_for(measured: (f64, f64), wrap_w: f64) -> (f64, f64) {
    let w = (measured.0 + TOOLTIP_PAD * 2.0)
        .min(TOOLTIP_MAX_W)
        .max(wrap_w.min(TOOLTIP_MAX_W));
    // Height follows the measured wrap at the width the tooltip will actually take.
    (w, measured.1 + TOOLTIP_PAD * 2.0)
}

const _: () = assert!(
    ROW_FIXED_W <= LIST_W_MIN,
    "a compact row still has to fit its icon and close slot"
);

/// What the pointer is over, in the panel's own terms. Decision code only reads this.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PointerZone {
    /// Over a row's or card's close slot (and the slot is visible).
    Close(usize),
    /// Over a card's title box, or a row whose title band is too narrow to carry it.
    Title(usize),
    /// Over a card or row, outside its title box.
    Card(usize),
    /// Over the panel but not over a card (the panel padding / gaps).
    Panel,
    /// In the crossing between the Dock tile and the panel.
    Transition,
    /// Over the Dock tile that opened the panel.
    Anchor,
    Outside,
}

/// The hit geometry of one panel, as the decision code reads it.
///
/// The viewport bounds what can be hit *through a row or a card*: a row clipped by the viewport edge
/// is only hit inside what is actually drawn. Every other class (the panel's own padding, the
/// transition band, the anchor tile) keeps its meaning, so a pointer resting on the Dock icon or
/// crossing between it and the panel never reads as "left".
#[derive(Clone, Copy, Debug)]
pub(crate) struct HitGeometry<'a> {
    pub(crate) panel: Rect,
    pub(crate) viewport: Rect,
    /// How far the content view is scrolled (x, y), in points.
    pub(crate) scroll: (f64, f64),
    /// The rows/cards in content coordinates.
    pub(crate) items: &'a [Rect],
    pub(crate) anchor: Rect,
    pub(crate) band: Rect,
    /// The index whose close slot is currently shown.
    pub(crate) close_visible: Option<usize>,
    /// Which layout the items belong to: a card's boxes are not a row's, and drawing and hit
    /// testing have to ask the same function for both.
    pub(crate) mode: PanelMode,
    /// Indices whose title band is too narrow to draw: the row body carries the tooltip.
    pub(crate) body_is_title: &'a [usize],
}

/// Classify a pointer point against the panel, its rows/cards, the transition band and the anchor.
pub(crate) fn pointer_zone(point: (f64, f64), geometry: HitGeometry<'_>) -> PointerZone {
    if geometry.viewport.contains(point) {
        let local = (
            point.0 - geometry.viewport.x + geometry.scroll.0,
            point.1 - geometry.viewport.y + geometry.scroll.1,
        );
        for (index, item) in geometry.items.iter().enumerate() {
            if !item.contains(local) {
                continue;
            }
            // The SAME boxes the drawing uses for this mode, so a click lands on the control it
            // looks like it is on (a card's close slot is not where a row's would be).
            let boxes = boxes_for(geometry.mode, *item);
            if geometry.close_visible == Some(index) && boxes.close.contains(local) {
                return PointerZone::Close(index);
            }
            if geometry.body_is_title.contains(&index) {
                return PointerZone::Title(index);
            }
            if boxes.title.contains(local) {
                return PointerZone::Title(index);
            }
            return PointerZone::Card(index);
        }
    }
    if geometry.panel.contains(point) {
        return PointerZone::Panel;
    }
    if geometry.band.contains(point) {
        return PointerZone::Transition;
    }
    if geometry.anchor.contains(point) {
        return PointerZone::Anchor;
    }
    PointerZone::Outside
}
#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> Rect {
        Rect::new(0.0, 0.0, 1470.0, 956.0)
    }

    fn visible() -> Rect {
        Rect::new(0.0, 24.0, 1470.0, 862.0)
    }

    /// A 56pt tile sitting on a bottom Dock, top edge at the visible frame's bottom.
    fn bottom_anchor() -> Rect {
        Rect::new(700.0, 830.0, 44.0, 56.0)
    }

    fn geometry(_point: (f64, f64), items: &[Rect], viewport: Rect) -> HitGeometry<'_> {
        HitGeometry {
            panel: Rect::new(0.0, 0.0, 300.0, 200.0),
            viewport,
            scroll: (0.0, 0.0),
            mode: PanelMode::List,
            items,
            // Below the panel and over the tile's own rectangle: the tile hangs under a bottom Dock.
            anchor: Rect::new(200.0, 400.0, 44.0, 56.0),
            band: Rect::new(100.0, 200.0, 150.0, 200.0),
            close_visible: Some(0),
            body_is_title: &[],
        }
    }

    #[test]
    fn the_mode_follows_the_window_count_only() {
        assert_eq!(mode_for(0, 5), PanelMode::Row);
        assert_eq!(mode_for(5, 5), PanelMode::Row);
        assert_eq!(mode_for(6, 5), PanelMode::List);
        // Threshold 1: one window is a row, two already need the list.
        assert_eq!(mode_for(1, 1), PanelMode::Row);
        assert_eq!(mode_for(2, 1), PanelMode::List);
    }

    #[test]
    fn the_budget_measures_towards_the_direction_the_panel_grows() {
        let visible = visible();
        // A bottom tile: the room is above it.
        let bottom = space_budget(
            Rect::new(700.0, 830.0, 44.0, 56.0),
            DockEdge::Bottom,
            visible,
        );
        assert!((bottom.inner_h - (830.0 - 24.0 - PANEL_MARGIN)).abs() < 0.001);
        assert!((bottom.inner_w - (visible.w - 2.0 * PANEL_MARGIN)).abs() < 0.001);

        // A left tile grows RIGHT: the room is to its right. (The v3 formula measured leftwards and
        // returned a negative number for an ordinary side Dock; this is that regression.)
        let left_tile = Rect::new(0.0, 400.0, 60.0, 44.0);
        let left = space_budget(left_tile, DockEdge::Left, visible);
        assert!(left.inner_w > 0.0, "a left Dock leaves room to the right");
        assert!((left.inner_w - (visible.max_x() - 60.0 - PANEL_MARGIN)).abs() < 0.001);
        assert!((left.inner_h - (visible.h - 2.0 * PANEL_MARGIN)).abs() < 0.001);

        let right_tile = Rect::new(1410.0, 400.0, 60.0, 44.0);
        let right = space_budget(right_tile, DockEdge::Right, visible);
        assert!((right.inner_w - (1410.0 - visible.x - PANEL_MARGIN)).abs() < 0.001);

        let top_tile = Rect::new(700.0, 24.0, 44.0, 56.0);
        let top = space_budget(top_tile, DockEdge::Top, visible);
        assert!((top.inner_h - (visible.max_y() - 80.0 - PANEL_MARGIN)).abs() < 0.001);
    }

    #[test]
    fn padding_is_subtracted_once_between_the_glass_and_the_viewport() {
        // A budget of exactly two rows plus the glass padding must yield a two-row viewport. A
        // second padding subtraction (v5's mistake) would leave 56 and report a single full row.
        let room_h = 2.0 * ROW_H + PANEL_PAD * 2.0;
        // `room_h` is the distance from the visible frame's top to the tile's TOP edge, less the
        // margin; the tile then hangs 56pt below that edge.
        let anchor = Rect::new(700.0, 24.0 + room_h + PANEL_MARGIN, 44.0, 56.0);
        let visible = Rect::new(0.0, 24.0, 1470.0, anchor.max_y() + 200.0 - 24.0);
        let budget = space_budget(anchor, DockEdge::Bottom, visible);
        assert!(
            (budget.inner_h - room_h).abs() < 0.001,
            "budget {budget:?} visible {visible:?}"
        );
        let layout = layout_for(PanelMode::List, &[], 7, anchor, DockEdge::Bottom, visible)
            .expect("two rows still fit");
        let list = layout.list.expect("list mode");
        assert!(
            (layout.panel.h - room_h).abs() < 0.001,
            "the glass is the budget"
        );
        assert!((list.viewport_h - 2.0 * ROW_H).abs() < 0.001);
        assert_eq!(list.full_rows, 2);
        assert!((list.max_scroll - (7.0 * ROW_H - 2.0 * ROW_H)).abs() < 0.001);
    }

    #[test]
    fn a_low_screen_keeps_the_list_and_reports_how_much_of_it_fits() {
        // A 40pt budget leaves a 32pt viewport: exactly one row, because the glass padding is what
        // this budget has to give up. The mode stays the list -- it is chosen by the window count,
        // never by the space left -- and the rest is reached by scrolling.
        let anchor = Rect::new(700.0, 24.0 + 40.0 + PANEL_MARGIN, 44.0, 56.0);
        let visible = Rect::new(0.0, 24.0, 1470.0, anchor.max_y() + 200.0 - 24.0);
        assert_eq!(
            space_budget(anchor, DockEdge::Bottom, visible).inner_h,
            40.0
        );
        let layout =
            layout_for(PanelMode::List, &[], 7, anchor, DockEdge::Bottom, visible).expect("fits");
        let list = layout.list.expect("list mode");
        assert_eq!(layout.mode, PanelMode::List);
        assert_eq!(list.full_rows, 1);
        assert!((list.viewport_h - ROW_H).abs() < 0.001);
        assert!((list.max_scroll - 6.0 * ROW_H).abs() < 0.001);

        // Half a row less: still the list, still scrollable, and no complete row is claimed.
        let anchor = Rect::new(700.0, 24.0 + 36.0 + PANEL_MARGIN, 44.0, 56.0);
        let visible = Rect::new(0.0, 24.0, 1470.0, anchor.max_y() + 200.0 - 24.0);
        let layout =
            layout_for(PanelMode::List, &[], 7, anchor, DockEdge::Bottom, visible).expect("fits");
        let list = layout.list.expect("list mode");
        assert_eq!(list.full_rows, 0);
        assert!(
            list.max_scroll > 0.0,
            "the rest is still reachable by scrolling"
        );
        assert!(list.viewport_h < ROW_H);
    }

    #[test]
    fn too_little_space_hides_the_panel_instead_of_changing_its_mode() {
        let visible = visible();
        // Under the minimum height for either mode.
        let low = Rect::new(700.0, visible.y + 20.0, 44.0, 56.0);
        assert!(layout_for(PanelMode::List, &[], 9, low, DockEdge::Bottom, visible).is_none());
        assert!(layout_for(PanelMode::Row, &[], 2, low, DockEdge::Bottom, visible).is_none());
        // Under the minimum width for either mode: a row cannot fit its icon and close slot.
        let narrow = Rect::new(10.0, visible.max_y() - 56.0, 44.0, 56.0);
        let narrower_than_a_row =
            Rect::new(0.0, 24.0, MIN_INTERACTIVE_W + PANEL_MARGIN - 1.0, 900.0);
        assert!(layout_for(
            PanelMode::List,
            &[],
            9,
            narrow,
            DockEdge::Bottom,
            narrower_than_a_row
        )
        .is_none());
        // Just wide enough: the compact row is what makes that legal, and it still fits.
        let just_a_row = Rect::new(0.0, 24.0, MIN_INTERACTIVE_W + PANEL_MARGIN + 8.0, 900.0);
        let layout = layout_for(
            PanelMode::List,
            &[],
            9,
            narrow,
            DockEdge::Bottom,
            just_a_row,
        )
        .expect("fits");
        assert!(layout.list.expect("list mode").compact);
    }

    #[test]
    fn the_row_is_always_one_row_and_never_drops_a_card() {
        let visible = visible();
        for count in 1..=6usize {
            let aspects = vec![Some(1.6); count];
            let layout = layout_for(
                PanelMode::Row,
                &aspects,
                count,
                bottom_anchor(),
                DockEdge::Bottom,
                visible,
            )
            .expect("a row fits");
            let row = layout.row.expect("row mode");
            assert_eq!(row.cards.len(), count, "every window keeps its card");
            let ys: Vec<f64> = row.cards.iter().map(|card| card.y).collect();
            assert!(ys.windows(2).all(|pair| pair[0] == pair[1]), "one row");
            assert!((layout.panel.h - (CARD_H + PANEL_PAD * 2.0)).abs() < 0.001);
        }
    }

    #[test]
    fn a_wide_row_compresses_before_it_scrolls_and_never_past_the_floor() {
        let visible = Rect::new(0.0, 24.0, 900.0, 862.0);
        let aspects = vec![Some(1.6); 4];
        let layout = layout_for(
            PanelMode::Row,
            &aspects,
            4,
            bottom_anchor(),
            DockEdge::Bottom,
            visible,
        )
        .expect("a row fits");
        let row = layout.row.expect("row mode");
        assert_eq!(row.axis, RowAxis::None);
        assert!(row.cards.iter().all(|card| card.w >= CARD_MIN_W));
        assert!(layout.panel.w <= 900.0 - 2.0 * PANEL_MARGIN + 0.001);

        // Nine cards are wider than the 840pt a 900pt screen offers, so they WRAP instead of
        // compressing under the floor: no card is squeezed below the minimum, and the extra cards
        // move to a second row rather than pushing the row sideways.
        let aspects = vec![Some(1.6); 9];
        let layout = layout_for(
            PanelMode::Row,
            &aspects,
            9,
            bottom_anchor(),
            DockEdge::Bottom,
            visible,
        )
        .expect("the cards fit");
        let row = layout.row.expect("row mode");
        assert!(row.cards.iter().all(|card| card.w >= CARD_MIN_W - 0.001));
        let mut ys: Vec<f64> = row.cards.iter().map(|card| card.y).collect();
        ys.dedup();
        assert!(ys.len() > 1, "the cards wrapped into more than one row");
        assert_eq!(row.axis, RowAxis::None, "two rows still fit under the Dock");
        assert!(layout.panel.w <= 900.0 - 2.0 * PANEL_MARGIN + 0.001);
    }

    #[test]
    fn the_row_boxes_leave_the_remainder_to_the_title_and_hide_it_when_empty() {
        let wide = row_boxes(LIST_W - PANEL_PAD * 2.0);
        assert!(!wide.title_hidden);
        assert!((wide.icon.w - ROW_ICON).abs() < 0.001);
        assert!((wide.close.w - CLOSE_SIZE).abs() < 0.001);
        assert!(wide.title.x > wide.icon.max_x());
        assert!(wide.title.max_x() < wide.close.x);

        // Just wide enough for a 32pt title band.
        let exact = row_boxes(ROW_FIXED_W + MIN_TITLE_W);
        assert!(!exact.title_hidden);
        assert!((exact.title.w - MIN_TITLE_W).abs() < 0.001);

        // One point narrower: the title gives up its band, and the row body carries the tooltip.
        let tight = row_boxes(ROW_FIXED_W + MIN_TITLE_W - 1.0);
        assert!(tight.title_hidden);
        assert!((tight.title.w).abs() < 0.001);
        assert!(tight.close.max_x() <= ROW_FIXED_W + MIN_TITLE_W - 1.0 + 0.001);
    }

    #[test]
    fn a_scroll_offset_is_clamped_and_keeps_its_sign() {
        assert_eq!(scroll_offset(0.0, -40.0, 200.0), 0.0);
        assert_eq!(scroll_offset(0.0, 40.0, 200.0), 40.0);
        assert_eq!(scroll_offset(180.0, 40.0, 200.0), 200.0);
        assert_eq!(scroll_offset(10.0, 40.0, 0.0), 0.0, "nothing to scroll");
        assert_eq!(scroll_offset(f64::NAN, 40.0, 200.0), 0.0);
    }

    #[test]
    fn the_axis_is_chosen_by_magnitude_but_the_delta_keeps_its_sign() {
        // A trackpad reports a tiny vertical delta during a horizontal swipe.
        assert_eq!(
            scroll_axis_for(PanelMode::Row, RowAxis::Horizontal, true, 100.0, 30.0, 0.2),
            Some((Axis::X, 30.0))
        );
        // A plain mouse wheel on a horizontal row drives the row, both directions.
        assert_eq!(
            scroll_axis_for(PanelMode::Row, RowAxis::Horizontal, true, 100.0, 0.0, 8.0),
            Some((Axis::X, 8.0))
        );
        assert_eq!(
            scroll_axis_for(PanelMode::Row, RowAxis::Horizontal, true, 100.0, 0.0, -8.0),
            Some((Axis::X, -8.0))
        );
        // A list never scrolls sideways.
        assert_eq!(
            scroll_axis_for(PanelMode::List, RowAxis::None, true, 100.0, 30.0, 0.0),
            None
        );
        assert_eq!(
            scroll_axis_for(PanelMode::List, RowAxis::None, true, 100.0, 0.0, -8.0),
            Some((Axis::Y, -8.0))
        );
        // Nothing to scroll, or the pointer outside the viewport: not consumed.
        assert_eq!(
            scroll_axis_for(PanelMode::List, RowAxis::None, true, 0.0, 0.0, 8.0),
            None
        );
        assert_eq!(
            scroll_axis_for(PanelMode::List, RowAxis::None, false, 100.0, 0.0, 8.0),
            None
        );
        // A row that does not scroll consumes nothing.
        assert_eq!(
            scroll_axis_for(PanelMode::Row, RowAxis::None, true, 100.0, 0.0, 8.0),
            None
        );
    }

    #[test]
    fn the_edge_fades_follow_the_scroll_position() {
        assert_eq!(edge_fades(0.0, 100.0), (false, true));
        assert_eq!(edge_fades(50.0, 100.0), (true, true));
        assert_eq!(edge_fades(100.0, 100.0), (true, false));
        assert_eq!(edge_fades(0.0, 0.0), (false, false));
    }

    #[test]
    fn the_content_origin_is_the_scroll_and_nothing_else() {
        // The content view is a CHILD of the viewport, so its origin is the scroll offset inside
        // that viewport. Adding the viewport's own origin (the padding) again shifted every row by
        // PANEL_PAD against everything that placed it -- the drawn box and the hit box disagreed.
        assert_eq!(content_origin((0.0, 0.0)), (0.0, 0.0));
        assert_eq!(content_origin((0.0, 32.0)), (0.0, -32.0));
        assert_eq!(content_origin((16.0, 0.0)), (-16.0, 0.0));
    }

    #[test]
    fn a_cards_close_slot_is_hit_where_it_is_drawn() {
        // Row mode: the close box comes from `close_rect`, the same function the card is drawn
        // with. Asking a row's boxes about a card made a click on the visible × charge the card.
        let viewport = Rect::new(0.0, 0.0, CARD_W + 8.0, CARD_H + 8.0);
        let card = Rect::new(PANEL_PAD, PANEL_PAD, CARD_W, CARD_H);
        let items = vec![card];
        let drawn_close = close_rect(card);
        let hit = pointer_zone(
            (
                drawn_close.x + drawn_close.w / 2.0,
                drawn_close.y + drawn_close.h / 2.0,
            ),
            HitGeometry {
                panel: Rect::new(0.0, 0.0, viewport.w, viewport.h),
                viewport,
                scroll: (0.0, 0.0),
                mode: PanelMode::Row,
                items: &items,
                anchor: Rect::new(500.0, 500.0, 44.0, 56.0),
                band: Rect::new(0.0, 0.0, 0.0, 0.0),
                close_visible: Some(0),
                body_is_title: &[],
            },
        );
        assert_eq!(
            hit,
            PointerZone::Close(0),
            "the visible × must answer Close"
        );

        let drawn_title = title_rect(card);
        let hit = pointer_zone(
            (drawn_title.x + 4.0, drawn_title.y + 4.0),
            HitGeometry {
                panel: Rect::new(0.0, 0.0, viewport.w, viewport.h),
                viewport,
                scroll: (0.0, 0.0),
                mode: PanelMode::Row,
                items: &items,
                anchor: Rect::new(500.0, 500.0, 44.0, 56.0),
                band: Rect::new(0.0, 0.0, 0.0, 0.0),
                close_visible: Some(0),
                body_is_title: &[],
            },
        );
        assert_eq!(hit, PointerZone::Title(0));
    }

    #[test]
    fn a_tall_grid_keeps_its_panel_inside_the_budget_and_scrolls_down() {
        // The glass never grows past the room the Dock leaves, whatever the cards need: a grid
        // taller than that is clipped by a viewport that is itself inside the budget, and scrolls.
        // A short screen: the same grid that fits a 862pt-tall visible frame is taller than this.
        let visible = Rect::new(0.0, 24.0, 900.0, 400.0);
        let anchor = Rect::new(700.0, 400.0 - 56.0, 44.0, 56.0);
        let aspects = vec![Some(1.6); 16];
        let layout = layout_for(
            PanelMode::Row,
            &aspects,
            16,
            anchor,
            DockEdge::Bottom,
            visible,
        )
        .expect("the cards fit");
        let budget = space_budget(anchor, DockEdge::Bottom, visible);
        assert!(layout.panel.w <= budget.inner_w + 0.001, "panel width");
        assert!(layout.panel.h <= budget.inner_h + 0.001, "panel height");
        let row = layout.row.expect("row mode");
        assert_eq!(row.axis, RowAxis::Vertical);
        // The scroll range covers the CARDS, not the glass padding: content minus viewport.
        assert!(row.content_h > layout.viewport.h);
        assert!(row.cards.last().expect("cards").max_y() > layout.viewport.h);
        // The first row starts at the content origin, so nothing is cut off at the top when the
        // grid is at scroll 0: the glass padding is the viewport's own inset, not a baked offset
        // (which used to double-count it and shift the whole grid one padding step right and down).
        assert!(row.cards[0].y.abs() < 0.001);
        assert!(row.cards[0].x.abs() < 0.001);
    }

    #[test]
    fn the_minimum_width_leaves_room_for_the_row_and_its_padding() {
        // The minimum is measured against the GLASS budget, so it has to include the padding both
        // sides; otherwise a budget at the minimum yields a viewport narrower than a row's fixed
        // pieces and the close slot is drawn outside it.
        let viewport_w = MIN_INTERACTIVE_W - PANEL_PAD * 2.0;
        assert!(
            viewport_w > ROW_FIXED_W,
            "the row fits inside a minimum viewport"
        );
        let boxes = row_boxes(viewport_w);
        assert!(boxes.close.max_x() <= viewport_w + 0.001);
        assert!(boxes.icon.x >= 0.0);
    }

    #[test]
    fn a_row_clipped_by_the_viewport_is_only_hit_where_it_is_drawn() {
        let viewport = Rect::new(PANEL_PAD, PANEL_PAD, 200.0, 64.0);
        let items = vec![
            Rect::new(0.0, 0.0, 200.0, ROW_H),
            Rect::new(0.0, ROW_H, 200.0, ROW_H),
            Rect::new(0.0, 2.0 * ROW_H, 200.0, ROW_H),
        ];
        // Fully inside the viewport: the first row answers.
        let zone = pointer_zone(
            (viewport.x + 100.0, viewport.y + 10.0),
            geometry((0.0, 0.0), &items, viewport),
        );
        assert!(matches!(zone, PointerZone::Title(0) | PointerZone::Card(0)));

        // The third row lies beyond the viewport: the same local point is panel, not a row.
        let zone = pointer_zone(
            (viewport.x + 100.0, viewport.y + 10.0),
            HitGeometry {
                scroll: (0.0, 64.0),
                ..geometry((0.0, 0.0), &items, viewport)
            },
        );
        // Scrolled to 64pt the third row's top sits at the viewport's top edge.
        assert!(matches!(zone, PointerZone::Title(2) | PointerZone::Card(2)));

        // Below the viewport, still inside the panel: never a row, even if a row overlaps it.
        let short = Rect::new(PANEL_PAD, PANEL_PAD, 200.0, 20.0);
        let zone = pointer_zone(
            (short.x + 100.0, short.max_y() + 4.0),
            geometry((0.0, 0.0), &items, short),
        );
        assert_eq!(zone, PointerZone::Panel);
    }

    #[test]
    fn the_anchor_and_the_band_keep_their_meaning_outside_the_viewport() {
        // A pointer resting on the Dock tile, and one crossing from the tile to the panel, must not
        // read as "left the panel": that is what the close grace would otherwise act on.
        let viewport = Rect::new(PANEL_PAD, PANEL_PAD, 200.0, 64.0);
        let items = vec![Rect::new(0.0, 0.0, 200.0, ROW_H)];
        let on_anchor = pointer_zone((210.0, 420.0), geometry((0.0, 0.0), &items, viewport));
        // The helper's anchor is at (200, 400); the point above is inside it, below the panel.
        assert_eq!(on_anchor, PointerZone::Anchor);
        let in_band = pointer_zone((150.0, 300.0), geometry((0.0, 0.0), &items, viewport));
        assert_eq!(in_band, PointerZone::Transition);
        // Not in the panel (which ends at y = 200), the band or the tile: the one point that still
        // means "left".
        let outside = pointer_zone((280.0, 205.0), geometry((0.0, 0.0), &items, viewport));
        assert_eq!(outside, PointerZone::Outside);
    }

    #[test]
    fn a_row_without_a_title_band_answers_with_the_title_zone() {
        let viewport = Rect::new(PANEL_PAD, PANEL_PAD, ROW_FIXED_W + 4.0, 64.0);
        let items = vec![Rect::new(0.0, 0.0, ROW_FIXED_W + 4.0, ROW_H)];
        let body_is_title = [0usize];
        let zone = pointer_zone(
            (viewport.x + 40.0, viewport.y + 10.0),
            HitGeometry {
                body_is_title: &body_is_title,
                ..geometry((0.0, 0.0), &items, viewport)
            },
        );
        assert_eq!(zone, PointerZone::Title(0));
    }

    #[test]
    fn the_hover_plate_covers_the_tile_out_to_the_panel_edge() {
        // A single card owns the whole panel: the plate covers the glass end to end (the content
        // origin sits one padding step inside it) and follows its corner radius, so no glass ring
        // is left around the selection.
        let panel = width_one_card();
        let card = Rect::new(0.0, 0.0, CARD_W, CARD_H);
        let (plate, radius) = hover_plate_rect(card, 0, 0, 1, 1, panel);
        assert_eq!(plate, Rect::new(-PANEL_PAD, -PANEL_PAD, panel.0, panel.1));
        assert_eq!(radius, PANEL_RADIUS);

        // In a row of three the middle card grows to the panel's top and bottom edges but keeps its
        // own width: the selection must not bleed sideways into a neighbour.
        let panel = (PANEL_PAD * 2.0 + 3.0 * CARD_W, PANEL_PAD * 2.0 + CARD_H);
        let middle = Rect::new(CARD_W, 0.0, CARD_W, CARD_H);
        let (plate, radius) = hover_plate_rect(middle, 0, 1, 3, 1, panel);
        assert_eq!(plate.x, middle.x);
        assert_eq!(plate.w, middle.w);
        assert_eq!(plate.y, -PANEL_PAD);
        assert!((plate.h - (CARD_H + PANEL_PAD * 2.0)).abs() < 0.001);
        assert_eq!(radius, PANEL_RADIUS);
    }

    #[test]
    fn plates_come_from_the_row_so_a_reflow_cannot_shrink_them_back() {
        let cards: Vec<Rect> = (0..3)
            .map(|index| Rect::new(index as f64 * CARD_W, 0.0, CARD_W, CARD_H))
            .collect();
        let panel = (PANEL_PAD * 2.0 + 3.0 * CARD_W, PANEL_PAD * 2.0 + CARD_H);
        let plates = plates_for_grid(&cards, cards.len(), panel);
        assert_eq!(plates.len(), 3);
        for (plate, card) in plates.iter().zip(&cards) {
            assert!(plate.0.x <= card.x + 0.001 && plate.0.max_x() >= card.max_x() - 0.001);
            assert!(plate.0.y <= card.y + 0.001 && plate.0.max_y() >= card.max_y() - 0.001);
        }
        // The leading card's plate reaches one padding step past the content origin: that is the
        // glass edge in viewport-relative coordinates, so the wash covers the padding ring too.
        assert_eq!(plates[0].0.x, -PANEL_PAD);
        assert!((plates[2].0.max_x() - (panel.0 - PANEL_PAD)).abs() < 0.001);
        assert!(plates[1].0.x > 0.0);
    }

    fn width_one_card() -> (f64, f64) {
        (CARD_W + PANEL_PAD * 2.0, CARD_H + PANEL_PAD * 2.0)
    }

    #[test]
    fn the_hover_plate_is_a_brightness_wash_not_an_accent_selection() {
        // Windows-style selected state: the plate is a white wash that reads brighter than the
        // unselected glass. Any blue channel bias would make it an accent selection again, which
        // is the style this replaced; and it carries no rim, because a hairline around the wash
        // read as a separate outlined box instead of the card's own hover state.
        let (r, g, b, alpha) = (
            (HOVER_PLATE_FILL >> 24) & 0xFF,
            (HOVER_PLATE_FILL >> 16) & 0xFF,
            (HOVER_PLATE_FILL >> 8) & 0xFF,
            HOVER_PLATE_FILL & 0xFF,
        );
        assert_eq!((r, g, b), (0xFF, 0xFF, 0xFF), "the plate is pure white");
        assert!(
            alpha > 0x1A,
            "the wash must be stronger than the shared hover_bg to read as brighter"
        );
    }

    #[test]
    fn the_centered_thumb_frame_shares_the_leftover_space_evenly() {
        // A 1:2 portrait image in a wide box: the horizontal leftover is split evenly, and the
        // vertical fit touches both ends. An off-centre frame is what made thumbnails read
        // shifted inside their cards.
        let thumb = Rect::new(10.0, 20.0, 200.0, 100.0);
        let frame = centered_thumb_frame(50.0, 100.0, thumb);
        assert!((frame.h - 100.0).abs() < 0.001);
        assert!((frame.w - 50.0).abs() < 0.001);
        assert!((frame.x - (10.0 + 75.0)).abs() < 0.001);
        assert_eq!(frame.y, 20.0);
        // A wide image in the same box fits by width instead, centring the vertical leftover.
        let frame = centered_thumb_frame(400.0, 100.0, thumb);
        assert!((frame.w - 200.0).abs() < 0.001);
        assert!((frame.h - 50.0).abs() < 0.001);
        assert_eq!(frame.x, 10.0);
        assert!((frame.y - (20.0 + 25.0)).abs() < 0.001);
        // An exact fit is the box itself, and degenerate input never moves the box.
        assert_eq!(centered_thumb_frame(200.0, 100.0, thumb), thumb);
        assert_eq!(centered_thumb_frame(0.0, 100.0, thumb), thumb);
    }

    #[test]
    fn every_card_takes_its_own_window_aspect_so_there_is_no_letterbox() {
        // Fixed card height, width follows the window aspect -- the app switcher's rule. A mixed
        // row (portrait / 16:10 / ultra-wide) must keep each thumbnail's aspect and leave no
        // fixed-width card frame around a narrower preview.
        let aspects = [1.0, 1.6, 2.0];
        let widths = widths_for_aspects(&aspects.map(Some));
        for (width, aspect) in widths.iter().zip(aspects) {
            let thumb_w = width - CARD_PAD * 2.0;
            assert!(
                (thumb_w / THUMB_H - aspect).abs() < 0.02,
                "thumbnail {thumb_w}x{THUMB_H} must keep aspect {aspect}"
            );
        }
        // A missing aspect falls back to the design width, and a portrait window is floored at the
        // smallest card that still holds the header.
        assert_eq!(card_width_for(None), CARD_W);
        assert_eq!(card_width_for(Some(0.3)), CARD_MIN_W);
    }

    #[test]
    fn card_children_sit_inside_the_card_without_overlap() {
        let card = Rect::new(PANEL_PAD, PANEL_PAD, CARD_W, CARD_H);
        let icon = icon_rect(card);
        let title = title_rect(card);
        let close = close_rect(card);
        let thumb = thumb_rect(card);
        for child in [icon, title, close, thumb] {
            assert!(card.contains((child.x + 0.5, child.y + 0.5)));
            assert!(child.max_x() <= card.max_x() + 0.001);
        }
        assert!(icon.max_x() <= title.x);
        assert!(title.max_x() <= close.x + 0.001);
        assert!(thumb.max_y() <= card.max_y() + 0.001);
        assert!(thumb.y >= card.y + HEADER_H);
    }

    #[test]
    fn the_panel_faces_each_dock_edge_on_the_tile_centre_line() {
        // The tile's top is high enough that the panel's bottom edge (which now touches the tile,
        // ANCHOR_GAP = 0) still fits inside the visible frame.
        let anchor = Rect::new(700.0, 840.0, 44.0, 56.0);
        let size = (300.0, 180.0);

        let bottom = panel_frame(anchor, size, DockEdge::Bottom, visible());
        assert!((bottom.x + bottom.w / 2.0 - anchor.x - anchor.w / 2.0).abs() < 0.001);
        assert!((bottom.y - (anchor.y - ANCHOR_GAP - size.1)).abs() < 0.001);

        let top_anchor = Rect::new(700.0, 0.0, 44.0, 56.0);
        let top = panel_frame(
            top_anchor,
            size,
            DockEdge::Top,
            Rect::new(0.0, 0.0, 1470.0, 930.0),
        );
        assert!((top.y - (top_anchor.max_y() + ANCHOR_GAP)).abs() < 0.001);

        let left_anchor = Rect::new(0.0, 400.0, 60.0, 44.0);
        let left = panel_frame(left_anchor, size, DockEdge::Left, visible());
        assert!((left.x - (left_anchor.max_x() + ANCHOR_GAP)).abs() < 0.001);
        assert!((left.y + left.h / 2.0 - left_anchor.y - left_anchor.h / 2.0).abs() < 0.001);

        let right_anchor = Rect::new(1410.0, 400.0, 60.0, 44.0);
        let right = panel_frame(
            right_anchor,
            size,
            DockEdge::Right,
            Rect::new(0.0, 24.0, 1470.0, 932.0),
        );
        assert!((right.x - (right_anchor.x - ANCHOR_GAP - size.0)).abs() < 0.001);
    }

    #[test]
    fn a_panel_near_a_corner_is_clamped_into_the_visible_frame() {
        let anchor = Rect::new(2.0, 890.0, 44.0, 56.0);
        let panel = panel_frame(anchor, (600.0, 200.0), DockEdge::Bottom, visible());
        assert!(panel.x >= visible().x - 0.001);
        assert!(panel.max_x() <= visible().max_x() + 0.001);
        assert!(panel.y >= visible().y - 0.001);
    }

    #[test]
    fn a_side_dock_band_covers_only_the_crossing() {
        let anchor = Rect::new(0.0, 400.0, 60.0, 44.0);
        let panel = Rect::new(68.0, 300.0, 300.0, 180.0);
        let band = transition_band(DockEdge::Left, anchor, panel);
        assert!((band.x - anchor.max_x()).abs() < 0.001);
        assert!((band.w - (panel.x - anchor.max_x())).abs() < 0.001);
        // A point on a neighbouring tile (below the anchor's band) is not in the band.
        assert!(!band.contains((30.0, 500.0)));
        // A diagonal move from the tile towards the panel's lower half is.
        assert!(band.contains((64.0, 390.0)));
    }

    #[test]
    fn the_bottom_band_does_not_cover_the_dock_row() {
        let anchor = Rect::new(700.0, 890.0, 44.0, 56.0);
        let panel = Rect::new(600.0, 690.0, 300.0, 192.0);
        let band = transition_band(DockEdge::Bottom, anchor, panel);
        assert!(band.y >= panel.max_y() - 0.001);
        assert!(band.max_y() <= anchor.y + 0.001);
        // A neighbouring tile sits at the anchor's y and is outside the band.
        assert!(!band.contains((660.0, 900.0)));
    }

    #[test]
    fn dock_edges_are_read_from_which_screen_edge_the_list_hugs() {
        let s = screen();
        assert_eq!(
            dock_edge_from_frames(Rect::new(35.0, 890.0, 1400.0, 56.0), s),
            Some(DockEdge::Bottom)
        );
        assert_eq!(
            dock_edge_from_frames(Rect::new(35.0, 0.0, 1400.0, 56.0), s),
            Some(DockEdge::Top)
        );
        assert_eq!(
            dock_edge_from_frames(Rect::new(0.0, 300.0, 56.0, 400.0), s),
            Some(DockEdge::Left)
        );
        assert_eq!(
            dock_edge_from_frames(Rect::new(1414.0, 300.0, 56.0, 400.0), s),
            Some(DockEdge::Right)
        );
        // Mid-reveal: not against any edge.
        assert_eq!(
            dock_edge_from_frames(Rect::new(35.0, 700.0, 1400.0, 56.0), s),
            None
        );
    }

    #[test]
    fn a_hidden_dock_is_not_treated_as_revealed() {
        let s = screen();
        // Measured hidden frame: parked with its top edge exactly on the screen's bottom.
        let hidden = Rect::new(35.0, 956.0, 1400.0, 56.0);
        assert!(!dock_revealed(hidden, s));
        let revealed = Rect::new(35.0, 890.0, 1400.0, 56.0);
        assert!(dock_revealed(revealed, s));
        // Mostly off screen counts as hidden.
        assert!(!dock_revealed(Rect::new(35.0, 940.0, 1400.0, 56.0), s));
    }

    #[test]
    fn truncation_is_a_width_comparison_and_ignores_unmeasurable_text() {
        assert!(!title_is_truncated(100.0, 152.0));
        assert!(title_is_truncated(152.5, 152.0));
        assert!(!title_is_truncated(f64::NAN, 152.0));
        assert!(!title_is_truncated(f64::INFINITY, 152.0));
    }

    #[test]
    fn the_tooltip_is_centred_over_the_hovered_item_not_the_panel() {
        let visible = visible();
        // Three equal cards: the panel's centre is the SECOND card's centre, so the old formula put
        // the tooltip exactly above the card next to the hovered one.
        let panel = Rect::new(
            500.0,
            500.0,
            PANEL_PAD * 2.0 + CARD_W * 3.0,
            CARD_H + PANEL_PAD * 2.0,
        );
        let third = Rect::new(500.0 + PANEL_PAD + CARD_W * 2.0, 504.0, CARD_W, CARD_H);
        let above = tooltip_frame(third, panel, (200.0, 60.0), DockEdge::Bottom, visible);
        assert!((above.x + above.w / 2.0 - third.x - third.w / 2.0).abs() < 0.001);
        assert!((above.x + above.w / 2.0 - panel.x - panel.w / 2.0).abs() > CARD_W / 2.0);
        assert!(above.max_y() <= panel.y);
    }

    #[test]
    fn the_tooltip_follows_the_dock_edge_and_flips_inside_the_visible_frame() {
        let visible = visible();
        let panel = Rect::new(500.0, 500.0, 300.0, 180.0);
        let item = Rect::new(596.0, 504.0, 120.0, CARD_H);
        // A bottom Dock keeps the tooltip above the panel.
        let above = tooltip_frame(item, panel, (200.0, 60.0), DockEdge::Bottom, visible);
        assert!(above.max_y() <= panel.y);
        // A top Dock puts it under the panel even when the space above would fit.
        let under = tooltip_frame(item, panel, (200.0, 60.0), DockEdge::Top, visible);
        assert!(under.y >= panel.max_y() - 0.001);
        // A panel near the screen top flips it below; the item moves with the panel.
        let near_top = Rect::new(500.0, 30.0, 300.0, 180.0);
        let flipped = tooltip_frame(
            Rect::new(596.0, 34.0, 120.0, CARD_H),
            near_top,
            (200.0, 60.0),
            DockEdge::Bottom,
            visible,
        );
        assert!(flipped.y >= near_top.max_y() - 0.001);
        assert!(flipped.max_y() <= visible.max_y() + 0.001);
    }

    #[test]
    fn a_tooltip_on_an_edge_item_is_clamped_inside_the_visible_frame() {
        let visible = visible();
        // The item sits near the right edge, and the tooltip is wider than the room left of it: the
        // clamp wins over centring, but the tooltip stays wholly on screen.
        let panel = Rect::new(1000.0, 500.0, 460.0, CARD_H + PANEL_PAD * 2.0);
        let item = Rect::new(1330.0, 504.0, 120.0, CARD_H);
        let frame = tooltip_frame(item, panel, (360.0, 60.0), DockEdge::Bottom, visible);
        assert!(frame.x >= visible.x - 0.001);
        assert!(frame.max_x() <= visible.max_x() + 0.001);
        assert!((frame.max_x() - visible.max_x()).abs() < 0.001);
    }

    #[test]
    fn the_mode_name_is_stable_for_e2e() {
        assert_eq!(PanelMode::Row.name(), "row");
        assert_eq!(PanelMode::List.name(), "list");
        assert_eq!(RowAxis::Horizontal.name(), "horizontal");
    }
}
