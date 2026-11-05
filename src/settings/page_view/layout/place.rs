//! Where each row goes: the placing half of the layout.
//!
//! `layout_rows` walks the description once and records a frame per row; the helpers below map
//! a described row to the canvas row it occupies and give the page sequence.

use super::*;
/// One position in a page's document: a described row, or a row the page draws itself. The layout
/// and the renderer both walk a page as a sequence of these, so a page that interleaves the two
/// cannot have one of them laid out at a different offset than it is drawn at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::settings) enum PlacedRow {
    /// Index into `PageView::rows`.
    Described(usize),
    /// Index into `PageView::extra_rows`.
    Drawn(usize),
}

/// Everything `layout_rows` computes for one page: where every row, card and panel sits.
///
/// Coordinates are the page document's (unflipped), and they come from the same walk the canvas
/// uses: the first section cursor, the section steps and the row heights. The renderer hands the
/// boxes to the canvas, which is what keeps a page's build and its later re-flow in step.
#[derive(Clone, Debug, PartialEq, Default)]
pub(in crate::settings) struct PageLayout {
    /// The page's rows in document order, top to bottom: the ONE sequence the renderer walks.
    pub(in crate::settings::page_view) sequence: Vec<PlacedRow>,
    /// Every page-drawn row's origin and the height the layout reserved for it, in `extra_rows`
    /// order, so drawing one uses the number the layout above it assumed (index, origin, height).
    pub(in crate::settings::page_view) drawn_rows: Vec<(usize, f64, f64)>,
    /// The height each page-drawn row took, in `PageView::extra_rows` order: the number the layout
    /// reserved, so the renderer advances by exactly that instead of measuring a second time.
    pub(in crate::settings::page_view) extra_row_heights: Vec<f64>,
    /// Whether the divider belongs above each page-drawn row, in `PageView::extra_rows` order.
    ///
    /// A described row decides this when it is pushed (`RowWriter::push` sets `separator_above`
    /// from the row before it in ITS OWN list, which only holds described rows). A page-drawn row
    /// is not in that list, so a described row following one still drew its divider -- but the row
    /// ABOVE the page-drawn one had already handed its divider to the page-drawn row, which never
    /// drew it. The seams around every page-drawn row were therefore missing: the Mouse page's
    /// device card (three page-drawn rows) had no divider at all, and the clipboard page's
    /// "max entries" row lost the line under the switch above it.
    ///
    /// The sequence is the only place both kinds of row exist in document order, so the answer is
    /// computed once here and the renderer draws the divider for either kind from it.
    pub(in crate::settings::page_view) drawn_separators: Vec<bool>,
    pub(in crate::settings::page_view) banner: Option<BannerRect>,
    /// The first card's top edge (the first section cursor).
    pub(in crate::settings::page_view) first_card_top: f64,
    /// One entry per row, in document order.
    pub(in crate::settings::page_view) rows: Vec<RowLayout>,
    /// One entry per card, in card order.
    pub(in crate::settings::page_view) cards: Vec<CardRect>,
    /// The top edge of every card, indexed by card.
    pub(in crate::settings::page_view) card_tops: Vec<f64>,
    /// The run of the page's content the canvas has to re-flow: from the first row's origin (or
    /// the first card's top) down to the last row's origin.
    pub(in crate::settings::page_view) content: BoxRect,
}

/// A row's height: its base height, grown when the copy box it carries needs more room.
fn row_height(
    row: &RowSpec,
    measured_subtitle_h: f64,
    _metrics: &PageMetrics,
    input: &LayoutInput,
) -> f64 {
    let base = match row.control {
        ControlKind::Popup { .. } => SettingsLayout::SINGLE_LINE_ROW_H,
        ControlKind::Switch
        | ControlKind::Button { .. }
        | ControlKind::Slider { .. }
        | ControlKind::Link { .. } => input.layout.described_row_h,
        ControlKind::Value { action, .. } => {
            // A value row with a button is a described row (the button makes it two lines tall,
            // like every other row carrying a control).
            if action.is_some() {
                input.layout.described_row_h
            } else {
                SettingsLayout::SINGLE_LINE_ROW_H
            }
        }
    };
    match row.copy {
        RowCopy::Plain => base,
        RowCopy::Captioned => {
            let stack = input.text.title_h + input.text.title_caption_gap + measured_subtitle_h;
            (stack + 19.0).max(base)
        }
    }
}

/// Place a page description in its measured page. The ONLY layout computation: `render_page`
/// applies these frames and never derives a frame of its own.
pub(in crate::settings::page_view) fn layout_rows(
    view: &PageView,
    input: &LayoutInput,
    metrics: &PageMetrics,
) -> PageLayout {
    let layout = input.layout;
    let content_w = input.content_w;
    let doc_top = input.doc_top;
    let text = &input.text;
    // The banner hangs from the document top, right below the header the canvas drew.
    let banner = view.banner.as_ref().map(|spec| {
        // The strip's FINAL parent is the settings content view (the page build hands it over and
        // the window re-parents it), so the caller states its frame in THAT view's coordinates --
        // the same box the inline builder used (banner_parent_frame).
        let strip = BoxRect::new(
            input.banner_frame.x,
            input.banner_frame.y,
            input.banner_frame.w.max(1.0),
            input.banner_frame.h.max(1.0),
        );
        // The label and the button are SUBVIEWS OF THE STRIP, so their boxes are stated in the
        // strip's own coordinates: adding the strip's origin here would double the parent offset.
        BannerRect {
            strip,
            label: BoxRect::new(
                12.0,
                spec.gap + 6.0,
                (strip.w - 160.0).max(1.0),
                (spec.content_h - 12.0).max(1.0),
            ),
            button: BoxRect::new(
                strip.w - 150.0,
                spec.gap + (spec.content_h - 28.0) / 2.0,
                140.0,
                28.0,
            ),
        }
    });
    let banner_h = banner.as_ref().map_or(0.0, |b| b.strip.h);
    // The first card hangs from the first section cursor, one banner further down when there is
    // one (the banner is a sibling of the page, not part of the header block).
    // ONE walk drives the whole page: each card's described rows and the rows the page draws
    // itself, in document order. The card surfaces, the described rows' frames and the heights the
    // renderer advances by therefore all come from one sequence, so a page that mixes the two (the
    // Mouse page: cards 1 and 3 hold only page-drawn rows) cannot have a row laid out at a
    // different offset than it is drawn at.
    let highest_card = view
        .rows
        .iter()
        .map(|row| row.card)
        .chain(view.extra_rows.iter().map(|extra| extra.card))
        .max()
        .unwrap_or(0);
    let sequence = page_sequence(view);
    let mut cursor = doc_top - input.header_offset - banner_h;
    let first_card_top = cursor;
    let mut card_tops = vec![first_card_top];
    // Each card's lowest edge, described or page-drawn: the surface must cover both kinds of row.
    let mut card_bottoms = vec![first_card_top; highest_card + 1];
    // Every page-drawn row's origin and the height reserved for it, in extra_rows order: the
    // renderer draws it at exactly that origin with exactly that height.
    let mut drawn_rows: Vec<(usize, f64, f64)> =
        vec![(0, first_card_top, 0.0); view.extra_rows.len()];
    // Which rows the divider belongs above, decided by the ONE walk that holds both kinds of row in
    // document order (see `PageLayout::drawn_separators`): the first row of a card has none, every
    // other row has one on its top edge. A page's own divider for a block it draws itself -- the
    // clipboard page's excluded-applications card -- is created outside this list on purpose and is
    // not counted here.
    let mut drawn_separators = vec![false; view.extra_rows.len()];
    let mut previous_card: Option<usize> = None;
    let mut placed_rows: Vec<Option<RowLayout>> = vec![None; view.rows.len()];
    for placed in sequence.iter().copied() {
        let card = match placed {
            PlacedRow::Described(index) => view.rows[index].card,
            PlacedRow::Drawn(index) => view.extra_rows[index].card,
        };
        // A card boundary at the page's start has no step: the first card IS the first cursor.
        while card_tops.len() <= card {
            cursor -= metrics.section_step;
            card_tops.push(cursor);
        }
        cursor -= metrics.row_gap;
        let separator_above = previous_card.is_some_and(|previous| previous == card);
        previous_card = Some(card);
        match placed {
            PlacedRow::Drawn(index) => {
                drawn_separators[index] = separator_above;
                let height = input.extra_row_heights.get(index).copied().unwrap_or(0.0);
                cursor -= height;
                drawn_rows[index] = (index, cursor, height);
                card_bottoms[card] = card_bottoms[card].min(cursor);
            }
            PlacedRow::Described(index) => {
                let row = &view.rows[index];
                let subtitle_h = text.subtitle_heights.get(index).copied().unwrap_or(0.0);
                let height = row_height(row, subtitle_h, metrics, input);
                cursor -= height;
                card_bottoms[card] = card_bottoms[card].min(cursor);

                // The card view spans the page's whole content width (x = 0, width = content_w),
                // so a row's copy column starts at the label inset INSIDE the card -- the same
                // \`label_x\` the inline builder used. Adding the card inset again indented every
                // row's text 28pt further right than the inline pages drew it.
                let copy_x = metrics.label_x;
                // A captioned row's text block is WIDER than a plain row's single label: the inline
                // builder measured and drew it up to the control column's leading edge
                // (`content_w - label_x * 2 - 58`), which is what keeps the design's two-line
                // description on two lines. Measuring it at the plain label width wrapped the same
                // copy to three lines and grew the row, so the page stopped matching the design.
                let copy_w = match row.copy {
                    RowCopy::Plain => layout.label_w.max(1.0),
                    RowCopy::Captioned => captioned_text_width(layout, content_w, row),
                };
                let (label, caption, stacked) = match row.copy {
                    RowCopy::Plain => (
                        BoxRect::new(
                            copy_x,
                            cursor + (height - text.title_h) / 2.0,
                            copy_w,
                            text.title_h,
                        ),
                        None,
                        false,
                    ),
                    RowCopy::Captioned => {
                        let stack_h = text.title_h + text.title_caption_gap + subtitle_h;
                        let caption_y = cursor + (height - stack_h.max(0.0)) / 2.0;
                        (
                            BoxRect::new(
                                copy_x,
                                caption_y + subtitle_h + text.title_caption_gap,
                                copy_w,
                                text.title_h,
                            ),
                            Some(BoxRect::new(copy_x, caption_y, copy_w, subtitle_h)),
                            true,
                        )
                    }
                };
                let (control, readout) = match &row.control {
                    ControlKind::Switch => {
                        let right_x = layout.control_x + layout.control_w;
                        // The switch is centred in the row's BAND, stacked copy or not: the inline
                        // builder centred every captioned row's control (`center_control`), and
                        // pinning it to the band's top edge put it 22pt above the row's middle on
                        // every captioned row.
                        let y = cursor + (height - metrics.control_h) / 2.0;
                        (
                            BoxRect::new(
                                right_x - metrics.switch_w,
                                y + (metrics.control_h - metrics.switch_h) / 2.0,
                                metrics.switch_w,
                                metrics.switch_h,
                            ),
                            None,
                        )
                    }
                    ControlKind::Popup { .. } => (
                        BoxRect::new(
                            layout.control_x,
                            cursor + (height - metrics.control_h) / 2.0,
                            layout.control_w,
                            metrics.control_h,
                        ),
                        None,
                    ),
                    // A hyperlink sits at the control column's leading edge, like a value: it is text, not a
                    // right-aligned control.
                    ControlKind::Link { .. } => {
                        let (x, w) = if row.value_column {
                            value_column_box(layout, content_w)
                        } else {
                            (layout.control_x, layout.control_w)
                        };
                        (
                            BoxRect::new(
                                x,
                                cursor + (height - metrics.control_h) / 2.0,
                                w,
                                metrics.control_h,
                            ),
                            None,
                        )
                    }
                    ControlKind::Slider { .. } => {
                        let h = layout.row_h;
                        let w =
                            (layout.control_w - metrics.readout_w - metrics.readout_gap).max(1.0);
                        let y = cursor + (height - h) / 2.0;
                        (
                            BoxRect::new(layout.control_x, y, w, h),
                            Some(BoxRect::new(
                                layout.control_x + w + metrics.readout_gap,
                                y + (h - metrics.readout_h) / 2.0,
                                metrics.readout_w,
                                metrics.readout_h,
                            )),
                        )
                    }
                    ControlKind::Button { .. } => {
                        let right_x = layout.control_x + layout.control_w;
                        // A row's action button is centred in the band, like every other control:
                        // the inline builder centred it, and pinning it to the band's top edge put
                        // a captioned row's button 12pt above the row's middle.
                        (
                            BoxRect::new(
                                right_x - metrics.action_btn_w,
                                cursor + (height - metrics.action_btn_h) / 2.0,
                                metrics.action_btn_w,
                                metrics.action_btn_h,
                            ),
                            None,
                        )
                    }
                    // A read-only value is drawn where the control column puts a control, left-aligned so a
                    // status line reads as text rather than as a disabled button. With an action beside it,
                    // the value takes the room the button leaves.
                    ControlKind::Value { action, .. } => {
                        let y = cursor + (height - metrics.control_h) / 2.0;
                        // A page with its own value column (About) draws its read-only values
                        // there: that column is wider than the shared control column, which is
                        // what lets a version string or a URL read in full.
                        let (column_x, column_w) = if row.value_column && action.is_none() {
                            value_column_box(layout, content_w)
                        } else {
                            (layout.control_x, layout.control_w)
                        };
                        match action {
                            None => (BoxRect::new(column_x, y, column_w, metrics.control_h), None),
                            Some(_) => (
                                BoxRect::new(
                                    layout.control_x,
                                    y,
                                    (layout.control_w
                                        - super::components::ROW_ACTION_BTN_W
                                        - VALUE_ACTION_GAP)
                                        .max(1.0),
                                    metrics.control_h,
                                ),
                                None,
                            ),
                        }
                    }
                };
                let action_frame = match &row.control {
                    ControlKind::Value {
                        action: Some(_), ..
                    } => Some(BoxRect::new(
                        layout.control_x + layout.control_w - super::components::ROW_ACTION_BTN_W,
                        cursor + (height - super::components::ROW_ACTION_BTN_H) / 2.0,
                        super::components::ROW_ACTION_BTN_W,
                        super::components::ROW_ACTION_BTN_H,
                    )),
                    _ => None,
                };
                let _ = &action_frame;
                placed_rows[index] = Some(RowLayout {
                    frame: BoxRect::new(0.0, cursor, content_w, height),
                    action_frame,
                    height,
                    label_frame: label,
                    stack_frame: label,
                    caption_frame: caption,
                    control_frame: control,
                    readout_frame: readout,
                    control_h: control.h,
                    stacked,
                });
            }
        }
    }
    // A card's surface spans from its section cursor down past its last row's bottom edge.
    // Every card index gets a section cursor, including a card that drew no DESCRIBED row: the
    // canvas walks sections, and a page may put rows it draws itself into a card of their own.
    let highest_card = view
        .rows
        .iter()
        .map(|row| row.card)
        .chain(view.extra_rows.iter().map(|extra| extra.card))
        .max()
        .unwrap_or(0);
    while card_tops.len() <= highest_card {
        let next = card_tops[card_tops.len() - 1] - metrics.section_step;
        card_tops.push(next);
    }
    let mut cards = Vec::new();
    for (card_index, card_top) in card_tops.iter().enumerate().take(highest_card + 1) {
        // The rows of this card, described or page-drawn, are what the surface spans.
        let described: Vec<usize> = view
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.card == card_index)
            .map(|(index, _)| index)
            .collect();
        let section = described
            .first()
            .map(|index| view.rows[*index].section)
            // A card with no described row names itself through its first page-drawn row's
            // SECTION -- the heading key the page opened it under. Falling back to the row's own
            // key printed the internal identifier ("device") where the heading belonged.
            .or_else(|| {
                view.extra_rows
                    .iter()
                    .find(|extra| extra.card == card_index)
                    .map(|extra| extra.section)
            })
            .unwrap_or("");
        let top = *card_top;
        // The card's lowest row, described or page-drawn: a card may hold only rows the page draws
        // itself, and its surface has to cover them.
        let bottom = card_bottoms
            .get(card_index)
            .copied()
            .unwrap_or(top)
            .min(top);
        cards.push(CardRect {
            holds_update_flow: view.extra_rows.iter().any(|extra| {
                extra.card == card_index
                    && extra.key == crate::settings::page_view::about::ABOUT_UPDATE_BAND
            }),
            index: card_index,
            section,
            frame: BoxRect::new(0.0, bottom, content_w, (top - bottom).max(0.0)),
            header: BoxRect::new(
                metrics.card_inset,
                layout.section_header_y(top),
                content_w - 2.0 * metrics.card_inset,
                text.heading_h,
            ),
        });
    }
    // The described rows, back in `PageView::rows` order: that is the order the renderer indexes,
    // the search index reads and a canvas lookup names.
    let rows: Vec<RowLayout> = placed_rows
        .into_iter()
        .map(|row| row.expect("every described row was laid out"))
        .collect();
    let content_top = first_card_top;
    let content_bottom = rows
        .last()
        .map(|row| row.frame.y)
        .into_iter()
        .chain(drawn_rows.iter().map(|(_, origin, _)| *origin))
        .fold(content_top, f64::min);
    PageLayout {
        sequence,
        drawn_rows,
        extra_row_heights: input.extra_row_heights.clone(),
        drawn_separators,
        banner,
        first_card_top,
        rows,
        cards,
        card_tops,
        content: BoxRect::new(0.0, content_bottom, content_w, content_top - content_bottom),
    }
}
/// Each card's row range (start, end-exclusive), in card order: one range per run of rows that
/// share a card index. A page that opens a card and draws no row in it draws no card.
/// The canvas row a described row lands in.
///
/// A described row takes one canvas row; a page-drawn row (`extra_row`) takes another. Every
/// described row after one of those is shifted by the count of page-drawn rows opened before it, so
/// this is the index the search index and any canvas lookup must use.
/// The canvas row a described row lands in.
///
/// The canvas opens one row per POSITION in the page's `PlacedRow` sequence, so the canvas row is
/// the row's position there -- not its index among described rows. A page whose earlier cards hold
/// rows it draws itself (Mouse: three device-card rows before its second described row) would
/// otherwise map every later described row to the wrong canvas row, and the search would scroll to
/// a page-drawn row instead of the match.
pub(in crate::settings) fn canvas_row_of_described_row(view: &PageView, row: usize) -> usize {
    canvas_rows(view).get(row).copied().unwrap_or(row)
}

/// The canvas row of every described row, in `PageView::rows` order, derived from the ONE sequence
/// the layout and the renderer walk.
pub(in crate::settings::page_view) fn canvas_rows(view: &PageView) -> Vec<usize> {
    let mut rows: Vec<Option<usize>> = vec![None; view.rows.len()];
    for (position, placed) in page_sequence(view).iter().enumerate() {
        if let PlacedRow::Described(index) = placed {
            rows[*index] = Some(position);
        }
    }
    rows.into_iter()
        .map(|row| row.expect("every described row is in the sequence"))
        .collect()
}

/// The page's rows in document order: every card's described rows and the rows the page draws
/// itself, exactly as `layout_rows` and `render_page` walk them.
pub(in crate::settings::page_view) fn page_sequence(view: &PageView) -> Vec<PlacedRow> {
    let highest_card = view
        .rows
        .iter()
        .map(|row| row.card)
        .chain(view.extra_rows.iter().map(|extra| extra.card))
        .max()
        .unwrap_or(0);
    let mut sequence: Vec<PlacedRow> = Vec::with_capacity(view.rows.len() + view.extra_rows.len());
    for card_index in 0..=highest_card {
        let mut drawn_in_card = 0usize;
        for (index, row) in view.rows.iter().enumerate() {
            if row.card != card_index {
                continue;
            }
            for (extra_index, extra) in view.extra_rows.iter().enumerate() {
                if extra.card == card_index && extra.after_row == drawn_in_card {
                    sequence.push(PlacedRow::Drawn(extra_index));
                }
            }
            sequence.push(PlacedRow::Described(index));
            drawn_in_card += 1;
        }
        // A page-drawn row declared after the card's last described row still belongs to it.
        for (extra_index, extra) in view.extra_rows.iter().enumerate() {
            if extra.card == card_index && extra.after_row >= drawn_in_card {
                sequence.push(PlacedRow::Drawn(extra_index));
            }
        }
    }
    sequence
}
