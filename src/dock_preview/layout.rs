//! Pure geometry for the Dock preview panel.
//!
//! Everything here is expressed in **panel-local, top-left** coordinates (x grows right, y grows
//! down), the same convention as CoreGraphics screen rectangles, so the anchor math and the card
//! layout can be asserted headlessly. The AppKit boundary converts once per view: the card host is
//! a flipped view, so its subviews take these rectangles unchanged.
//!
//! The panel shape follows the design document's token set rather than the reference screenshot's
//! pixels: card padding, header height, gaps and radii are stated here once and every caller reads
//! them, so the measured layout, the drawn frames and the e2e answer cannot disagree.

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
pub(crate) const PANEL_RADIUS: f64 = 14.0;
pub(crate) const CARD_RADIUS: f64 = 10.0;
pub(crate) const THUMB_RADIUS: f64 = 6.0;
pub(crate) const MAX_COLUMNS: usize = 4;
pub(crate) const MAX_ROWS: usize = 4;
/// The most window cards one panel shows. Beyond it the tail windows have no direct entry; the
/// e2e/state answer carries `hidden_window_count` so the limitation is observable instead of silent.
pub(crate) const MAX_CARDS: usize = MAX_COLUMNS * MAX_ROWS;
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

/// The width one card takes. Multi-window panels keep the fixed design width so their cards line
/// up; a single window instead takes the width of its own thumbnail, so a portrait or an
/// ultra-wide window does not leave letterbox bars inside a card shaped for the average window.
pub(crate) fn card_width_for(count: usize, aspect: Option<f64>) -> f64 {
    if count != 1 {
        return CARD_W;
    }
    let Some(aspect) = aspect.filter(|aspect| aspect.is_finite() && *aspect > 0.05) else {
        return CARD_W;
    };
    // The thumbnail keeps its height; its width follows the window's own aspect ratio, and the card
    // adds the two inner paddings back. Clamped so a degenerate report cannot produce a panel that
    // is too narrow to hold the header or wider than the screen can be expected to show.
    let thumb_w = (THUMB_H * aspect).round().clamp(120.0, 360.0);
    thumb_w + CARD_PAD * 2.0
}

/// The card grid the panel can fit: at most [`MAX_COLUMNS`] columns and [`MAX_ROWS`] rows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Grid {
    pub(crate) columns: usize,
    pub(crate) rows: usize,
    pub(crate) visible_cards: usize,
    /// The width every card in this grid takes (see `card_width_for`).
    pub(crate) card_w: f64,
}

impl Grid {
    pub(crate) fn panel_size(self) -> (f64, f64) {
        let w = PANEL_PAD * 2.0
            + self.columns as f64 * self.card_w
            + (self.columns.saturating_sub(1)) as f64 * CARD_GAP;
        let h = PANEL_PAD * 2.0
            + self.rows as f64 * CARD_H
            + (self.rows.saturating_sub(1)) as f64 * CARD_GAP;
        (w, h)
    }
}

/// The grid for `count` cards inside an available area. A narrow screen falls back to fewer
/// columns; the caller can therefore trust the returned panel size to fit the area it was given.
pub(crate) fn grid_for(count: usize, available_w: f64, available_h: f64, card_w: f64) -> Grid {
    let usable_w = (available_w - PANEL_PAD * 2.0).max(card_w);
    let by_width = ((usable_w + CARD_GAP) / (card_w + CARD_GAP))
        .floor()
        .max(1.0) as usize;
    // Fewer cards than columns do not stretch the panel: one window is one column, three are
    // three, and only the fourth begins a second row.
    let columns = by_width.max(1).min(count.max(1)).clamp(1, MAX_COLUMNS);

    let usable_h = (available_h - PANEL_PAD * 2.0).max(CARD_H);
    let rows_by_height = ((usable_h + CARD_GAP) / (CARD_H + CARD_GAP))
        .floor()
        .max(1.0) as usize;
    let max_rows = rows_by_height.clamp(1, MAX_ROWS);

    let cards = count.min(MAX_CARDS);
    let rows = cards.div_ceil(columns).clamp(1, max_rows);
    let visible_cards = cards.min(columns * rows);
    Grid {
        columns,
        rows,
        visible_cards,
        card_w,
    }
}

/// The card rectangles in panel-local coordinates, row-major from the top-left.
pub(crate) fn card_rects(grid: Grid) -> Vec<Rect> {
    let mut rects = Vec::with_capacity(grid.visible_cards);
    for index in 0..grid.visible_cards {
        let column = index % grid.columns;
        let row = index / grid.columns;
        rects.push(Rect::new(
            PANEL_PAD + column as f64 * (grid.card_w + CARD_GAP),
            PANEL_PAD + row as f64 * (CARD_H + CARD_GAP),
            grid.card_w,
            CARD_H,
        ));
    }
    rects
}

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

/// The full-title tooltip's screen rectangle: centred over the panel, above it, and flipped below
/// when the top of the screen has no room.
pub(crate) fn tooltip_frame(
    panel: Rect,
    tooltip: (f64, f64),
    edge: DockEdge,
    _screen: Rect,
    visible: Rect,
) -> Rect {
    let (w, h) = tooltip;
    let x = panel.x + panel.w / 2.0 - w / 2.0;
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

/// What the pointer is over, in the panel's own terms. Decision code only reads this.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PointerZone {
    /// Over a card's close slot (and the slot is visible).
    Close(usize),
    /// Over a card's title box.
    Title(usize),
    /// Over a card, outside its title box.
    Card(usize),
    /// Over the panel but not over a card (the panel padding / gaps).
    Panel,
    /// In the crossing between the Dock tile and the panel.
    Transition,
    /// Over the Dock tile that opened the panel.
    Anchor,
    Outside,
}

/// Classify a pointer point against the panel, its cards, the transition band and the anchor tile.
///
/// `close_visible` is the card whose close slot is currently shown; a hidden slot is not hit, so a
/// click where the button would be falls through to the card body (the documented behaviour).
pub(crate) fn pointer_zone(
    point: (f64, f64),
    panel: Rect,
    cards: &[Rect],
    anchor: Rect,
    band: Rect,
    close_visible: Option<usize>,
) -> PointerZone {
    for (index, card) in cards.iter().enumerate() {
        if !card.contains(point) {
            continue;
        }
        if close_visible == Some(index) && close_rect(*card).contains(point) {
            return PointerZone::Close(index);
        }
        if title_rect(*card).contains(point) {
            return PointerZone::Title(index);
        }
        return PointerZone::Card(index);
    }
    if panel.contains(point) {
        return PointerZone::Panel;
    }
    if band.contains(point) {
        return PointerZone::Transition;
    }
    if anchor.contains(point) {
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

    #[test]
    fn one_card_panel_fits_the_card_and_its_padding() {
        let grid = grid_for(1, 1000.0, 800.0, CARD_W);
        assert_eq!(grid.columns, 1);
        assert_eq!(grid.rows, 1);
        assert_eq!(grid.visible_cards, 1);
        assert_eq!(
            grid.panel_size(),
            (CARD_W + PANEL_PAD * 2.0, CARD_H + PANEL_PAD * 2.0)
        );
    }

    #[test]
    fn cards_sit_edge_to_edge_with_no_gutter() {
        let grid = grid_for(2, 1000.0, 800.0, CARD_W);
        let rects = card_rects(grid);
        assert_eq!(rects.len(), 2);
        assert!(
            (rects[1].x - rects[0].max_x()).abs() < 0.001,
            "adjacent cards share an edge: {} vs {}",
            rects[1].x,
            rects[0].max_x()
        );
        assert_eq!(
            grid.panel_size(),
            (PANEL_PAD * 2.0 + 2.0 * CARD_W, PANEL_PAD * 2.0 + CARD_H)
        );
    }

    #[test]
    fn a_single_card_takes_its_thumbnail_width_so_there_is_no_letterbox() {
        // A 3:2 window: the thumbnail is 124 high, so it should be 186 wide and the panel should be
        // exactly card + panel padding -- the defect was a fixed 216pt card leaving blank space at
        // the right of a narrower preview.
        let aspect = 1.5;
        let card_w = card_width_for(1, Some(aspect));
        let panel_w = grid_for(1, 2000.0, 800.0, card_w).panel_size().0;
        let thumb_w = card_w - CARD_PAD * 2.0;
        assert!(
            (thumb_w / THUMB_H - aspect).abs() < 0.01,
            "thumbnail {thumb_w}x{THUMB_H} must keep the window's aspect"
        );
        assert_eq!(panel_w, card_w + PANEL_PAD * 2.0);
        let rects = card_rects(grid_for(1, 2000.0, 800.0, card_w));
        assert_eq!(rects[0].w, card_w);
        // Multi-window panels keep the design width regardless of one window's aspect.
        assert_eq!(card_width_for(2, Some(aspect)), CARD_W);
        // A missing aspect falls back to the design width.
        assert_eq!(card_width_for(1, None), CARD_W);
    }

    #[test]
    fn cards_wrap_at_four_columns_and_the_panel_grows_down() {
        let grid = grid_for(7, 1300.0, 900.0, CARD_W);
        assert_eq!(grid.columns, 4);
        assert_eq!(grid.rows, 2);
        let (w, h) = grid.panel_size();
        assert!((w - (PANEL_PAD * 2.0 + 4.0 * CARD_W + 3.0 * CARD_GAP)).abs() < 0.001);
        assert!((h - (PANEL_PAD * 2.0 + 2.0 * CARD_H + CARD_GAP)).abs() < 0.001);
        let rects = card_rects(grid);
        assert_eq!(rects.len(), 7);
        // The fourth card starts the second row at the panel's left padding.
        assert!((rects[4].x - PANEL_PAD).abs() < 0.001);
        assert!((rects[4].y - (PANEL_PAD + CARD_H + CARD_GAP)).abs() < 0.001);
    }

    #[test]
    fn a_narrow_screen_degrades_to_fewer_columns() {
        let grid = grid_for(6, 260.0, 900.0, CARD_W);
        assert_eq!(grid.columns, 1);
        assert_eq!(grid.visible_cards, 4, "four rows at most");
        let (w, _) = grid.panel_size();
        assert!(w <= 260.0 + 0.001);
    }

    #[test]
    fn more_windows_than_the_panel_can_show_are_counted_not_dropped_silently() {
        let grid = grid_for(20, 1300.0, 900.0, CARD_W);
        assert_eq!(grid.visible_cards, MAX_CARDS);
        assert!(grid.visible_cards < 20);
        assert_eq!(card_rects(grid).len(), MAX_CARDS);
    }

    #[test]
    fn card_children_sit_inside_the_card_without_overlap() {
        let card = card_rects(grid_for(2, 1000.0, 800.0, CARD_W))[0];
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
    fn pointer_zones_prefer_the_close_slot_then_the_title_then_the_card() {
        let panel = Rect::new(100.0, 100.0, 240.0, 180.0);
        let card = Rect::new(108.0, 108.0, CARD_W, CARD_H);
        let cards = vec![card];
        let anchor = Rect::new(180.0, 300.0, 44.0, 56.0);
        let band = transition_band(DockEdge::Bottom, anchor, panel);
        let close = close_rect(card);
        let title = title_rect(card);
        let thumb = thumb_rect(card);

        assert_eq!(
            pointer_zone(
                (close.x + 1.0, close.y + 1.0),
                panel,
                &cards,
                anchor,
                band,
                Some(0)
            ),
            PointerZone::Close(0)
        );
        // Hidden slot: the same point is the card body.
        assert_eq!(
            pointer_zone(
                (close.x + 1.0, close.y + 1.0),
                panel,
                &cards,
                anchor,
                band,
                None
            ),
            PointerZone::Card(0)
        );
        assert_eq!(
            pointer_zone(
                (title.x + 1.0, title.y + 1.0),
                panel,
                &cards,
                anchor,
                band,
                Some(0)
            ),
            PointerZone::Title(0)
        );
        assert_eq!(
            pointer_zone(
                (thumb.x + 1.0, thumb.y + 1.0),
                panel,
                &cards,
                anchor,
                band,
                Some(0)
            ),
            PointerZone::Card(0)
        );
        assert_eq!(
            pointer_zone(
                (panel.x + 1.0, panel.max_y() - 1.0),
                panel,
                &cards,
                anchor,
                band,
                None
            ),
            PointerZone::Panel
        );
        assert_eq!(
            pointer_zone((200.0, 290.0), panel, &cards, anchor, band, None),
            PointerZone::Transition
        );
        assert_eq!(
            pointer_zone((200.0, 320.0), panel, &cards, anchor, band, None),
            PointerZone::Anchor
        );
        assert_eq!(
            pointer_zone((0.0, 0.0), panel, &cards, anchor, band, None),
            PointerZone::Outside
        );
    }

    #[test]
    fn truncation_is_a_width_comparison_and_ignores_unmeasurable_text() {
        assert!(!title_is_truncated(100.0, 152.0));
        assert!(title_is_truncated(152.5, 152.0));
        assert!(!title_is_truncated(f64::NAN, 152.0));
        assert!(!title_is_truncated(f64::INFINITY, 152.0));
    }

    #[test]
    fn the_tooltip_sits_above_the_panel_and_flips_below_near_the_screen_top() {
        let visible = visible();
        let panel = Rect::new(500.0, 500.0, 300.0, 180.0);
        let above = tooltip_frame(panel, (200.0, 60.0), DockEdge::Bottom, screen(), visible);
        assert!(above.max_y() <= panel.y);
        assert!((above.x + above.w / 2.0 - panel.x - panel.w / 2.0).abs() < 0.001);

        let near_top = Rect::new(500.0, 30.0, 300.0, 180.0);
        let flipped = tooltip_frame(near_top, (200.0, 60.0), DockEdge::Bottom, screen(), visible);
        assert!(flipped.y >= near_top.max_y() - 0.001);
    }
}
