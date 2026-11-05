//! The layout: where rows, cards and banners land.

use super::*;
/// A page canvas over a stand-in document (no window: the canvas's layout walk never messages
/// the document except to count and re-place subviews, which a null view does not answer).
#[test]
fn collapsing_a_group_moves_every_placed_view_with_its_row() {
    // The rule the re-flow applies, through the SAME function the canvas calls. Collapsing the
    // ThumbnailOnly group above a row moves that row up by the rows and gaps it consumed; the
    // row's copy/control/readout must follow it, keeping their offsets, and the control must
    // keep its distance to the card's top edge -- not stay at its build-time frame.
    let input = input();
    let view = switcher_page_view(&input);
    let layout_input = LayoutInput::for_test(600.0);
    let metrics = PageMetrics::for_layout(layout_input.layout);
    let layout = layout_rows(&view, &layout_input, &metrics);
    // The ThumbnailOnly rows are 5..=7 in the window-options card; a row below them in the
    // SAME card is the card-text-size row (index 8).
    let hidden_rows = 5..=7;
    let thumbnail_card = view.rows[5].card;
    let below = layout
        .rows
        .iter()
        .enumerate()
        .find(|(index, _)| {
            !hidden_rows.contains(index) && *index > 7 && view.rows[*index].card == thumbnail_card
        })
        .map(|(_, row)| row)
        .expect("a row below the thumbnail group");
    let moved_up: f64 = hidden_rows
        .clone()
        .map(|index| layout.rows[index].frame.h)
        .sum();
    let new_origin = below.frame.y + moved_up;
    let offsets: Vec<NSPoint> = {
        let mut out = Vec::new();
        // The offsets are what `place_pending_view` records: box origin minus row origin.
        for box_ in row_placement_boxes(&view.rows[8], &layout.rows[8]) {
            out.push(NSPoint::new(box_.x, box_.y - below.frame.y));
        }
        out
    };
    for (index, offset) in offsets.iter().enumerate() {
        let origin = crate::settings::page_canvas::placed_view_origin(new_origin, *offset);
        let built = row_placement_boxes(&view.rows[8], &layout.rows[8])[index];
        assert!(
            (origin.x - built.x).abs() < 1e-9,
            "view {index} keeps its column"
        );
        assert!(
                (origin.y - (built.y + moved_up)).abs() < 1e-9,
                "view {index} moved up by exactly the hidden rows' height, not stayed at its build frame"
            );
        assert!(
            (origin.y - built.y).abs() > 1.0,
            "view {index} must NOT still be at its build-time frame"
        );
    }
    // The control keeps its offset FROM ITS ROW: the row moved, and the control moved with it.
    let control_offset = offsets[1];
    let control_origin =
        crate::settings::page_canvas::placed_view_origin(new_origin, control_offset);
    assert!(
        (control_origin.y - (new_origin + control_offset.y)).abs() < 1e-9,
        "the control stays at the layout's offset inside its row"
    );
    assert!(
        (new_origin - below.frame.y - moved_up).abs() < 1e-9,
        "the row itself moved up by the hidden rows"
    );
}

#[test]
fn the_banner_children_lie_inside_their_strip() {
    // The label and the button are subviews of the strip, so their boxes are in the strip's
    // coordinates. Adding the strip's origin to them (the first version) put both children
    // one strip-height above the strip: this fails at origin.y >= strip.y for a zero-origin
    // strip and for every strip placed below the document top.
    let input = input();
    let view = general_page_view(&input);
    let layout_input = LayoutInput::for_test(600.0);
    let metrics = PageMetrics::for_layout(layout_input.layout);
    let layout = layout_rows(&view, &layout_input, &metrics);
    let banner = layout.banner.as_ref().expect("General carries the banner");
    for (name, child) in [("label", banner.label), ("button", banner.button)] {
        assert!(
            child.y >= -0.5 && child.y + child.h <= banner.strip.h + 0.5,
            "the {name} must lie inside the strip's height: child.y={} h={} strip.h={}",
            child.y,
            child.h,
            banner.strip.h
        );
        assert!(
            child.x >= -0.5 && child.x + child.w <= banner.strip.w + 0.5,
            "the {name} must lie inside the strip's width"
        );
    }
}

#[test]
fn a_rows_current_origin_is_what_a_placement_follows() {
    // #3: after a re-flow the canvas's current row origin differs from the build-time
    // PageLayout origin; a placement must follow the FORMER. The occurrence the runner checks
    // is AppKit-only, so this asserts the data it checks: `current_row_origin` is the value
    // `placements_are_stale` and the checker compare against, and it is not the layout's.
    let input = input();
    let view = switcher_page_view(&input);
    let layout_input = LayoutInput::for_test(600.0);
    let metrics = PageMetrics::for_layout(layout_input.layout);
    let layout = layout_rows(&view, &layout_input, &metrics);
    // The same arithmetic the canvas walks: a collapsed ThumbnailOnly group moves every row
    // below it up by the rows it hid.
    let hidden_h: f64 = (5..=7).map(|index| layout.rows[index].frame.h).sum();
    let below = 8;
    let reflowed_origin = layout.rows[below].frame.y + hidden_h;
    assert!(
            (reflowed_origin - layout.rows[below].frame.y).abs() > 1.0,
            "the re-flow origin differs from the build-time origin, which is why the checker must read the canvas"
        );
}

#[test]
fn the_banner_strip_is_placed_in_its_final_parents_coordinates() {
    // #2: the strip is re-parented to the settings CONTENT view, so its frame comes from
    // `banner_parent_frame` -- the inline builder's own box (page_x, the VISIBLE viewport's
    // top, content_w x banner_h) -- not from the page document's height and not from x = 0.
    let input = input();
    let view = general_page_view(&input);
    let mut layout_input = LayoutInput::for_test(600.0);
    let page_x = 340.0;
    let page_viewport_h = 700.0;
    layout_input.banner_frame = BannerFrame::of(banner_parent_frame(
        page_x,
        page_viewport_h,
        600.0,
        banner_height(false),
    ));
    let metrics = PageMetrics::for_layout(layout_input.layout);
    let layout = layout_rows(&view, &layout_input, &metrics);
    let banner = layout.banner.as_ref().expect("General carries the banner");
    assert_eq!(
        banner.strip.x, page_x,
        "the strip starts at the detail pane's inset"
    );
    assert_eq!(banner.strip.w, 600.0);
    assert_eq!(banner.strip.h, banner_height(false));
    let top = banner.strip.y + banner.strip.h;
    assert!(
        (top - (page_viewport_h - SettingsPageHeader::TOP_PADDING)).abs() < 1e-9,
        "the strip hangs from the visible viewport's top, not the page document's"
    );
}

#[test]
fn build_page_lays_the_general_banner_out_from_the_page_context() {
    // The PRODUCTION wiring: `page_layout_input` is the one place `build_page` builds a page's
    // layout inputs from its context. Given General's context values, the box it produces must
    // carry that page_x and that VISIBLE page_viewport_h -- read back from the returned
    // LayoutInput, not computed by this test.
    let input = input();
    let view = general_page_view(&input);
    let page_x = 412.0;
    let page_viewport_h = 634.0;
    let doc_h = 1320.0;
    let content_w = 600.0;
    let layout_input = page_layout_input(
        &view,
        &input,
        content_w,
        SettingsLayout::new(content_w),
        doc_h,
        111.0,
        page_x,
        page_viewport_h,
    );
    assert_eq!(
        layout_input.banner_frame.x, page_x,
        "the banner's box carries the context's page_x"
    );
    assert_eq!(
        layout_input.banner_frame.y,
        page_viewport_h - SettingsPageHeader::TOP_PADDING - banner_height(false),
        "the banner hangs from the VISIBLE viewport, not the page document"
    );
    assert_eq!(
        layout_input.doc_top, doc_h,
        "the rows are laid out from the document height"
    );
    // ... and the strip `layout_rows` places from it is exactly that box.
    let metrics = PageMetrics::for_layout(layout_input.layout);
    let layout = layout_rows(&view, &layout_input, &metrics);
    let strip = layout.banner.as_ref().unwrap().strip;
    assert_eq!(
        (strip.x, strip.y, strip.w, strip.h),
        (
            page_x,
            page_viewport_h - SettingsPageHeader::TOP_PADDING - banner_height(false),
            content_w,
            banner_height(false)
        )
    );
}

#[test]
fn layout_rows_places_the_switcher_rows_without_gaps_or_overlaps() {
    let view = switcher();
    let layout_input = LayoutInput::for_test(600.0);
    let metrics = PageMetrics::for_layout(layout_input.layout);
    let layout = layout_rows(&view, &layout_input, &metrics);
    assert_eq!(layout.rows.len(), view.rows.len());
    // One card per run of rows: the page's first 16 rows are one card and the modifier row
    // is the second (a card that opens no row is not drawn).
    let first_card = &layout.cards[0];
    assert!((first_card.frame.y + first_card.frame.h - layout.first_card_top).abs() < 1e-9);
    // A card exists exactly where its index changes, and it steps the cursor once.
    let mut changes = 0usize;
    let mut previous = view.rows[0].card;
    for row in &view.rows[1..] {
        if row.card != previous {
            changes += 1;
            previous = row.card;
        }
    }
    assert_eq!(layout.cards.len(), changes + 1);
    // The card surface ends at its last row's origin and tops out at its own section cursor
    // (its card index in the walk, not its position among the drawn cards).
    let mut start = 0usize;
    for card in &layout.cards {
        let card_index = view.rows[start].card;
        let end = view.rows[start..]
            .iter()
            .rposition(|row| row.card == card_index)
            .map(|position| position + start)
            .unwrap();
        assert!((card.frame.y - layout.rows[end].frame.y).abs() < 1e-9);
        assert!(
            (card.frame.y + card.frame.h - layout.card_tops[card_index]).abs() < 1e-9,
            "card {card_index}: bottom {} top {} expected {}",
            card.frame.y,
            card.frame.y + card.frame.h,
            layout.card_tops[card_index]
        );
        start = end + 1;
    }
}

#[test]
fn layout_rows_places_controls_inside_the_row_band() {
    let view = switcher();
    let layout_input = LayoutInput::for_test(600.0);
    let metrics = PageMetrics::for_layout(layout_input.layout);
    let layout = layout_rows(&view, &layout_input, &metrics);
    for (row, placed) in view.rows.iter().zip(layout.rows.iter()) {
        let control = &placed.control_frame;
        assert!(control.y >= placed.frame.y - 1e-9, "{row:?}");
        assert!(
            control.y + control.h <= placed.frame.y + placed.frame.h + 1e-9,
            "{row:?}"
        );
        // The copy never reaches into the control column.
        let label = &placed.label_frame;
        assert!(label.x + label.w <= layout_input.layout.control_x + 1e-9);
        if let Some(readout) = placed.readout_frame {
            assert!(readout.x >= control.x + control.w);
        }
    }
}

/// Every row — described or page-drawn — must belong to a card the layout produced, and the
/// layout must preserve the card's OWN index. A page whose cards mix described and page-drawn
/// rows (Mouse: 0,1,2,3,4,5 with 1 and 3 page-only) once lost every card after the gap because
/// the layout and the renderer both used a positional index.
#[test]
fn every_card_and_row_survives_the_layout_for_every_page() {
    let input =
        PageInput::from_config(Config::default(), None, crate::theme::ThemeInput::current());
    for page in [
        SettingsPageId::Switcher,
        SettingsPageId::General,
        SettingsPageId::Clipboard,
        SettingsPageId::About,
        SettingsPageId::KeystrokeDisplay,
        SettingsPageId::FinderMenu,
        SettingsPageId::KeyBindings,
        SettingsPageId::Mouse,
    ] {
        let view = view_for(page, &input);
        let mut layout_input = LayoutInput::for_test(700.0);
        layout_input.extra_row_heights = view
            .extra_rows
            .iter()
            .map(|extra| extra.height.resolve(&input, layout_input.content_w))
            .collect();
        let metrics = PageMetrics::for_layout(layout_input.layout);
        let layout = layout_rows(&view, &layout_input, &metrics);
        let highest = view
            .rows
            .iter()
            .map(|row| row.card)
            .chain(view.extra_rows.iter().map(|extra| extra.card))
            .max()
            .unwrap_or(0);
        assert_eq!(
            layout.cards.len(),
            highest + 1,
            "{page:?} must produce one card per card index, page-drawn-only cards included"
        );
        for card in &layout.cards {
            assert!(
                (0..=highest).contains(&card.index),
                "{page:?} produced a card with an out-of-range index"
            );
        }
        let mut indices: Vec<usize> = layout.cards.iter().map(|card| card.index).collect();
        indices.sort_unstable();
        assert_eq!(
            indices,
            (0..=highest).collect::<Vec<_>>(),
            "{page:?} must keep every card index, in order"
        );
        // A card that holds only page-drawn rows still names itself.
        for card in &layout.cards {
            let has_described = view.rows.iter().any(|row| row.card == card.index);
            if !has_described {
                assert!(
                    !card.section.is_empty(),
                    "{page:?}: a page-drawn-only card must still name its heading"
                );
            }
        }
    }
}

/// A page-drawn row must consume the height the LAYOUT reserved for it, at the origin the
/// layout put it, and a card must cover the rows the page draws inside it. Mouse is the page
/// this went wrong on: its smooth-slider row was handed the device row's height, and its
/// page-drawn-only cards had no surface.
#[test]
fn page_drawn_rows_are_laid_out_at_their_own_height_and_cards_cover_them() {
    let input =
        PageInput::from_config(Config::default(), None, crate::theme::ThemeInput::current());
    for page in [
        SettingsPageId::Switcher,
        SettingsPageId::General,
        SettingsPageId::Clipboard,
        SettingsPageId::About,
        SettingsPageId::KeystrokeDisplay,
        SettingsPageId::FinderMenu,
        SettingsPageId::KeyBindings,
        SettingsPageId::Mouse,
    ] {
        let view = view_for(page, &input);
        let mut layout_input = LayoutInput::for_test(700.0);
        layout_input.extra_row_heights = view
            .extra_rows
            .iter()
            .map(|extra| extra.height.resolve(&input, layout_input.content_w))
            .collect();
        let metrics = PageMetrics::for_layout(layout_input.layout);
        let layout = layout_rows(&view, &layout_input, &metrics);
        assert_eq!(layout.drawn_rows.len(), view.extra_rows.len());
        assert_eq!(
            layout.sequence.len(),
            view.rows.len() + view.extra_rows.len()
        );
        // The declared height reaches the layout unchanged, and the row's origin is the
        // position in the sequence, not a per-card index into a page-wide array.
        for (index, extra) in view.extra_rows.iter().enumerate() {
            let (row_index, origin, height) = layout.drawn_rows[index];
            assert_eq!(row_index, index);
            let declared = extra.height.resolve(&input, layout_input.content_w);
            assert_eq!(
                height, declared,
                "{page:?}: row {index} ({}) was laid out at a different height than declared",
                extra.key
            );
            assert!(origin <= layout.first_card_top);
            // The row sits inside the card's surface.
            let card = layout
                .cards
                .iter()
                .find(|card| card.index == extra.card)
                .expect("every card has a surface");
            assert!(
                origin >= card.frame.y && origin + height <= card.frame.y + card.frame.h + 1.0e-6,
                "{page:?}: row {} ({}) is outside its card's surface",
                index,
                extra.key
            );
        }
    }
}

/// The sequence must place every described row exactly once and every page-drawn row exactly
/// once, in card order: the renderer draws from it, so a missing entry is a row that never
/// appears.
#[test]
fn the_sequence_places_every_row_exactly_once() {
    let input =
        PageInput::from_config(Config::default(), None, crate::theme::ThemeInput::current());
    for page in [
        SettingsPageId::Switcher,
        SettingsPageId::General,
        SettingsPageId::Clipboard,
        SettingsPageId::About,
        SettingsPageId::KeystrokeDisplay,
        SettingsPageId::FinderMenu,
        SettingsPageId::KeyBindings,
        SettingsPageId::Mouse,
    ] {
        let view = view_for(page, &input);
        let mut layout_input = LayoutInput::for_test(700.0);
        layout_input.extra_row_heights = view
            .extra_rows
            .iter()
            .map(|extra| extra.height.resolve(&input, layout_input.content_w))
            .collect();
        let metrics = PageMetrics::for_layout(layout_input.layout);
        let layout = layout_rows(&view, &layout_input, &metrics);
        let mut described: Vec<usize> = Vec::new();
        let mut drawn: Vec<usize> = Vec::new();
        for placed in &layout.sequence {
            match placed {
                PlacedRow::Described(index) => described.push(*index),
                PlacedRow::Drawn(index) => drawn.push(*index),
            }
        }
        assert_eq!(
            described,
            (0..view.rows.len()).collect::<Vec<_>>(),
            "{page:?}: every described row appears once, in document order"
        );
        assert_eq!(
            drawn,
            (0..view.extra_rows.len()).collect::<Vec<_>>(),
            "{page:?}: every page-drawn row appears once, in document order"
        );

        // Rows are ordered top to bottom: the sequence's origins descend, and two rows may
        // share an origin only when the lower one takes no space (the About page's inline
        // update host starts empty and grows over the band's place).
        let mut last = layout.first_card_top;
        let mut previous_zero_height = false;
        for placed in &layout.sequence {
            let (origin, height) = match placed {
                PlacedRow::Described(index) => {
                    (layout.rows[*index].frame.y, layout.rows[*index].frame.h)
                }
                PlacedRow::Drawn(index) => {
                    let (_, origin, height) = layout.drawn_rows[*index];
                    (origin, height)
                }
            };
            assert!(
                    origin <= last,
                    "{page:?}: the sequence must descend (origin={origin}, previous={last}, placed={placed:?})"
                );
            let _ = (height, previous_zero_height);
            previous_zero_height = height <= 0.0;
            last = origin;
        }
    }
}

/// The canvas row a described row lands in is its position in the page's ONE sequence, not its
/// index among described rows: Mouse's `reverse_scroll` is described row 1 but canvas row 4,
/// because three rows of the device card are drawn by the page. The search scrolls to, and the
/// M2 checker inspects, the canvas row — so a mapping that ignores the pure-hook cards sends
/// both to a page-drawn row.
#[test]
fn the_canvas_row_mapping_counts_the_page_drawn_rows_of_every_card() {
    let input =
        PageInput::from_config(Config::default(), None, crate::theme::ThemeInput::current());
    for page in [
        SettingsPageId::Switcher,
        SettingsPageId::General,
        SettingsPageId::Clipboard,
        SettingsPageId::About,
        SettingsPageId::KeystrokeDisplay,
        SettingsPageId::FinderMenu,
        SettingsPageId::KeyBindings,
        SettingsPageId::Mouse,
    ] {
        let view = view_for(page, &input);
        let sequence = page_sequence(&view);
        let rows = canvas_rows(&view);
        // Every described row's canvas row is where the sequence puts it.
        for (index, row) in view.rows.iter().enumerate() {
            let position = sequence
                .iter()
                .position(|placed| matches!(placed, PlacedRow::Described(drawn) if *drawn == index))
                .expect("every described row is in the sequence");
            assert_eq!(
                rows[index], position,
                "{page:?}: described row {index} maps wrong"
            );
            assert_eq!(canvas_row_of_described_row(&view, index), position);
            let _ = row;
        }
        // The mapping is strictly increasing: the canvas rows follow the document order.
        assert!(rows.windows(2).all(|pair| pair[0] < pair[1]));
    }
    // The counterexample, named: Mouse's second described row sits after three page-drawn rows
    // (device, scroll mode, line count) and before the fourth card's own page-drawn rows.
    let mouse = view_for(SettingsPageId::Mouse, &input);
    assert_eq!(canvas_rows(&mouse), vec![0, 4, 10, 12]);
    assert_eq!(mouse.rows[1].label_key, "settings.row_reverse_scroll");
}

/// A read-only value is TEXT: it shares its row label's origin, not a control box's.
///
/// The inline builder fitted a value label to its own line height and centred that in the
/// row band (`center_readonly_text_control`). Drawing it in the layout's 32pt control box put
/// it 7.5pt low and 15pt too tall on every About value row. The layout still reserves a
/// control-sized box -- the RENDERER fits the label into it and records the fitted frame --
/// so what M1 can pin here is the box the layout hands over: a value row is a described row
/// when it carries an action, and a single-line row when it does not.
#[test]
fn a_value_row_reserves_a_box_the_renderer_fits_the_label_into() {
    let input = input();
    let view = about_page_view(&input);
    let layout_input = LayoutInput::for_test(620.0);
    let metrics = PageMetrics::for_layout(layout_input.layout);
    let layout = layout_rows(&view, &layout_input, &metrics);
    let mut values = 0;
    for (index, row) in view.rows.iter().enumerate() {
        let ControlKind::Value { action, .. } = row.control else {
            continue;
        };
        values += 1;
        let placed = &layout.rows[index];
        // The renderer fits the label into this box and centres it in `placed.frame`; the
        // box must therefore be at least as tall as the row band's usable height, and the
        // band must be a full row (the fit is centred in it, not in the box).
        assert!(
            placed.control_frame.h > 0.0,
            "About row {index} reserves no control box"
        );
        assert!(
            (placed.control_frame.h - metrics.control_h).abs() < 0.01,
            "About row {index} reserves {}pt, not the control height",
            placed.control_frame.h
        );
        // A value WITH an action keeps the described row height (the button makes it two
        // lines); a bare value is a single-line row.
        let want_band = if action.is_some() {
            layout_input.layout.described_row_h
        } else {
            SettingsLayout::SINGLE_LINE_ROW_H
        };
        assert!(
            (placed.frame.h - want_band).abs() < 0.01,
            "About row {index} band is {}pt, not {want_band}",
            placed.frame.h
        );
    }
    assert_eq!(values, 4, "the About page's four value rows");
}

/// The placement recorded for a view is THAT view's box, not the last box in its row.
///
/// `row_placement_boxes` ends with the SLIDER READOUT for a slider row, so taking its last element
/// as "the control's box" made the canvas record the readout's box as the slider's placement. The
/// canvas re-applies a recorded placement on every reflow, so the slider was moved onto its own
/// readout (136pt right on the Switcher page, past the document's edge) and the M2 layout smoke
/// caught it as `OhMyTabSlider escapes document` plus a control/label overlap.
#[test]
fn a_slider_records_its_own_box_not_its_readouts() {
    let input = input();
    let view = switcher_page_view(&input);
    let layout_input = LayoutInput::for_test(620.0);
    let metrics = PageMetrics::for_layout(layout_input.layout);
    let layout = layout_rows(&view, &layout_input, &metrics);
    let mut sliders = 0;
    for (index, row) in view.rows.iter().enumerate() {
        if !matches!(row.control, ControlKind::Slider { .. }) {
            continue;
        }
        sliders += 1;
        let placed = &layout.rows[index];
        let boxes = row_placement_boxes(row, placed);
        // The row's boxes end with the readout, and the control's own frame is NOT that box.
        let last = *boxes.last().expect("a row places at least one box");
        assert!(
            (last.x - placed.control_frame.x).abs() > 0.01,
            "row {index}: the last placement box IS the control's box, so taking the last one",
        );
        assert!(
            (last.x - placed.readout_frame.expect("a slider has a readout").x).abs() < 0.01,
            "row {index}: the last placement box is not the readout's"
        );
    }
    assert_eq!(sliders, 2, "the Switcher page's two size sliders");
}

/// A link is TEXT: the renderer sends it down the same fit-and-centre path as a value label.
///
/// The two About link rows drew their URL in the layout's full 32pt control box. An NSTextField
/// draws its cell from the top of its box, so the URL sat 7pt above the row's middle while the row
/// title beside it was centred -- the links read visibly high against their labels. The layout was
/// never the fault (its box IS centred in the band); the RENDERER was, because it fitted only
/// `ControlKind::Value` and drew a link at the bare control box.
///
/// M1 cannot run `center_readonly_text_control` (AppKit), so what it pins is the branch that
/// decides to call it: the production predicate `fits_as_text`. A test that re-derived the
/// centring formula here would pass with the link branch removed, and this one must not -- taking a
/// link out of `fits_as_text` is exactly the regression, and it fails here.
#[test]
fn a_link_row_is_fitted_and_centred_like_a_value_label() {
    let input = input();
    let view = about_page_view(&input);
    let layout_input = LayoutInput::for_test(620.0);
    let metrics = PageMetrics::for_layout(layout_input.layout);
    let layout = layout_rows(&view, &layout_input, &metrics);
    let mut links = 0;
    for (index, row) in view.rows.iter().enumerate() {
        if !matches!(row.control, ControlKind::Link { .. }) {
            continue;
        }
        links += 1;
        assert!(
            fits_as_text(&row.control),
            "About row {index}: a link does not take the text-fitting branch, so its glyphs are \
             drawn against the top of a control box instead of the row's centre line"
        );
        // The box the layout hands over IS centred in the band, and IS the full control height:
        // the defect is that a full-height field draws its ink at the box's top.
        let placed = &layout.rows[index];
        let band_mid = placed.frame.y + placed.frame.h / 2.0;
        assert!(
            (placed.control_frame.y + metrics.control_h / 2.0 - band_mid).abs() < 0.01,
            "About row {index}: the layout's control box is not centred in the band"
        );
        let unfitted_ink =
            placed.control_frame.y + metrics.control_h - layout_input.text.title_h / 2.0;
        assert!(
            (unfitted_ink - band_mid).abs() > 6.0,
            "the test cannot see the defect it guards: the full-height box already draws on the \
             centre line"
        );
        assert!(
            (placed.control_frame.h - metrics.control_h).abs() < 0.01,
            "About row {index}: the layout no longer hands the renderer a control-sized box"
        );
    }
    assert_eq!(links, 2, "the About page's two link rows");

    // The value rows `fits_as_text` already served keep it; a control that is not text does not.
    for row in view.rows.iter() {
        if matches!(row.control, ControlKind::Value { .. }) {
            assert!(fits_as_text(&row.control), "a value row lost its fit");
        }
    }
    assert!(!fits_as_text(&ControlKind::Switch));
    assert!(!fits_as_text(&ControlKind::Popup { options: vec![] }));
}

/// Every row of a card has a divider above it except the card's first row -- including the rows a
/// PAGE draws itself.
///
/// The divider was decided when a described row was pushed, from the row before it in the
/// description's own list, which holds no page-drawn row. So a described row after a page-drawn one
/// drew its divider at the right place, but the page-drawn row never drew the one the row above had
/// handed over: the Mouse page's device card (three page-drawn rows) had no divider at all, and the
/// clipboard page's "max entries" row sat under the "delete after paste" switch with no line between
/// them. `PageLayout::drawn_separators` is that decision for the page-drawn rows, taken from the
/// one sequence both kinds of row share.
#[test]
fn a_card_puts_a_divider_above_every_row_including_the_pages_own() {
    let input = input();
    for page in [
        SettingsPageId::Mouse,
        SettingsPageId::Clipboard,
        SettingsPageId::Switcher,
        SettingsPageId::General,
        SettingsPageId::About,
    ] {
        let view = view_for(page, &input);
        let layout_input = LayoutInput::for_test(620.0);
        let metrics = PageMetrics::for_layout(layout_input.layout);
        let layout = layout_rows(&view, &layout_input, &metrics);
        let mut previous_card: Option<usize> = None;
        let mut page_drawn_with_divider = 0;
        for placed in page_sequence(&view).iter().copied() {
            let (card, has_divider) = match placed {
                PlacedRow::Described(index) => {
                    (view.rows[index].card, view.rows[index].separator_above)
                }
                PlacedRow::Drawn(index) => {
                    (view.extra_rows[index].card, layout.drawn_separators[index])
                }
            };
            // A row that belongs to a conditional group is a CHILD of the row above it (the
            // clipboard page's "clear the system pasteboard" row under "delete after paste"):
            // it is the switch's child option, not a sibling, so it draws no seam -- while it is
            // hidden the row under it would show a divider under nothing, and while it is shown
            // the pair reads as one setting.
            let is_child_row = match placed {
                PlacedRow::Described(index) => view.rows[index].group.is_some(),
                PlacedRow::Drawn(_) => false,
            };
            // Only a card's FIRST row may go without a seam (it starts under the card's own edge).
            // A row after a page-drawn one is the case this test exists for: the page-drawn row
            // above it never drew the divider, and the described row below did not draw one either
            // because `RowWriter::push` still saw its own list's previous row. A child option is
            // the one other row that legitimately has none.
            assert!(
                has_divider || is_child_row || previous_card != Some(card),
                "{page:?}: {placed:?} of card {card} is neither the card's first row nor a child \
                 option, so it needs a divider above it and has none"
            );
            if matches!(placed, PlacedRow::Drawn(_)) && has_divider {
                page_drawn_with_divider += 1;
            }
            previous_card = Some(card);
        }
        if page == SettingsPageId::Mouse {
            // The device card and the pointer card are the two the user reported, and both are
            // page-drawn or followed by a page-drawn row: the counterexample the fix removes is a
            // page-drawn row that never got the divider.
            assert_eq!(
                page_drawn_with_divider, 7,
                "the Mouse page's page-drawn rows that continue their card: the device card's \
                 scroll mode and line count, the smooth preset and its four sliders, and the \
                 tracking speed. Nothing drew one before the layout decided them."
            );
        }
    }
}

/// The About page's Updates card keeps exactly ONE line over its boundary, drawn by the page.
///
/// The band draws that line itself, anchored to the row above, because the update flow collapses
/// the band: a line captured by the band would travel with it and slide over the row above. So the
/// card's divider must not also be drawn there -- two lines on one boundary -- and the empty,
/// hidden update host right below the band must not carry one either (a divider is a document
/// subview of its own, so hiding the host would not hide it).
///
/// What is asserted is the DECISION the renderer makes (`card_divider_above`), not the raw layout
/// flag: the layout says "this row continues its card" for both of these rows, so a test that only
/// read `drawn_separators` would go green while the renderer drew a stray line.
#[test]
fn about_draws_no_card_divider_over_its_update_band_or_host() {
    let input = input();
    let view = about_page_view(&input);
    let layout_input = LayoutInput::for_test(620.0);
    let metrics = PageMetrics::for_layout(layout_input.layout);
    let layout = layout_rows(&view, &layout_input, &metrics);
    let band = view
        .extra_rows
        .iter()
        .position(|extra| extra.key == crate::settings::page_view::about::ABOUT_UPDATE_BAND)
        .expect("the Updates band");
    let host = view
        .extra_rows
        .iter()
        .position(|extra| extra.key == crate::settings::page_view::about::ABOUT_UPDATE_HOST)
        .expect("the update host");
    // Both rows continue the card, so the layout would give each a divider on its own.
    assert!(layout.drawn_separators[band], "the band continues its card");
    assert!(layout.drawn_separators[host], "the host follows the band");
    // The page's own answer, which is what the renderer obeys:
    assert!(
        view.extra_rows[band].divides_with_own_line,
        "the band draws the one line over that boundary itself"
    );
    assert!(
        view.extra_rows[host].no_divider,
        "the host is part of the band's block and must draw no line of its own"
    );
    assert!(
        !card_divider_above(&view.extra_rows[band], &layout, band),
        "the band must not get a second line on its boundary"
    );
    assert!(
        !card_divider_above(&view.extra_rows[host], &layout, host),
        "the hidden host must not get a line under the band"
    );
    // And an ordinary page-drawn row still gets the card's line.
    let ordinary = view.extra_rows.iter().position(|extra| {
        extra.key != crate::settings::page_view::about::ABOUT_UPDATE_BAND
            && extra.key != crate::settings::page_view::about::ABOUT_UPDATE_HOST
    });
    assert!(
        ordinary.is_none(),
        "the About page's only page-drawn rows are the band and the host"
    );
}

/// A page-drawn row is never turned ABSOLUTE behind its page's back.
///
/// The canvas's Absolute walk re-places only the views a row REGISTERED through
/// \`place_pending_view\`. A page's hook draws its own views and registers some of them (the
/// Clipboard recorder's button) but not others (About's check-for-updates button and update host,
/// the Key Bindings list block); those only move because the row keeps the Relative walk, which
/// shifts everything captured since the row opened. Turning a page-drawn row absolute therefore
/// strands exactly the unregistered content -- it stays behind while the card and its divider move.
///
/// The divider this change adds is drawn into the row, so it does not need Absolute: it is captured
/// with the row and travels relatively like the row's other views.
#[test]
fn a_page_drawn_row_keeps_the_relative_walk() {
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/settings/page_view/render.rs"
    ))
    .expect("the page renderer's own source");
    let page_start = source.find("unsafe fn render_page(").expect("render_page");
    let page = &source[page_start..source.find("unsafe fn apply_control_update").unwrap()];
    let drawn_arm = page
        .find("PlacedRow::Drawn(index) =>")
        .expect("the page-drawn arm");
    let next_arm = page[drawn_arm..]
        .find("\n                }")
        .map(|offset| drawn_arm + offset)
        .unwrap_or(page.len());
    let body = &page[drawn_arm..next_arm];
    assert!(
        !body.contains("AdvanceState::Absolute"),
        "the page-drawn arm must not set an absolute advance: the page's own views are not all \
         registered, so they would stop following the row"
    );
    // The described arm still walks absolutely -- it re-places exactly what it registered.
    assert!(
        source.contains("let _: () = msg_send![control, setFrame: placed.control_frame.rect()];"),
        "the described rows keep their absolute placement"
    );
}
