//! The rendering and building a page.

use super::*;
#[test]
fn a_migrated_page_indexes_every_row_the_inline_sweep_used_to() {
    // The sweep the inline pages ran filled a row's entry from the strings its views render.
    // A migrated page must index the same rows with the same baseline text, or settings
    // search silently stops finding a setting it used to find.
    let view = switcher();
    let page = settings_page_index(SettingsPageId::Switcher);
    let _guard = sidebar::lock_search_index_for_test();
    sidebar::begin_settings_search_index();
    sidebar::begin_settings_search_section();
    for (row, spec) in view.rows.iter().enumerate() {
        sidebar::record_settings_search_entry(
            crate::settings::sidebar::SearchEntry::row(
                page,
                row,
                &spec.search.label,
                spec.search.desc.as_deref(),
                &spec.search.section,
            )
            .with_options(&spec.search.options),
        );
    }
    // The inline sweep's own invariant: every row of the page has exactly one entry.
    let recorded = sidebar::recorded_search_rows(page);
    assert_eq!(recorded.len(), view.rows.len());
    for row in 0..view.rows.len() {
        assert!(recorded.contains(&row), "row {row} is not searchable");
    }
    // And each entry carries the description's OWN copy, not the sweep's fallback label: a
    // query for a row's label finds exactly that row.
    for (row, spec) in view.rows.iter().enumerate() {
        let hits = sidebar::match_settings_search_for_page(page, &spec.search.label);
        assert!(
            hits.contains(&row),
            "row {row} ({}) is not found by its own label",
            spec.search.label
        );
    }
    sidebar::begin_settings_search_index();
}

#[test]
fn every_view_the_renderer_creates_is_released_exactly_once() {
    // #3/#1 rule: each +1 from alloc/init is given up EXACTLY ONCE. The banner strip's is
    // given up by `finalize_settings_pages` (it re-parents the strip to the content view and
    // releases the reference the page build handed over), so `render_page` must NOT release
    // it too: a second release would steal the parent's own reference.
    let view_src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/settings/page_view/render.rs"
    ))
    .expect("the page renderer's own source");
    let page_start = view_src.find("unsafe fn render_page(").unwrap();
    // The function's own body, by brace matching: the previous anchor was a loop in a function
    // that merely FOLLOWED render_page in the same file, so it vanished the moment the two moved
    // to different files -- and before that it would have accepted a match from that neighbour.
    let page_end = braced_end(view_src.as_bytes(), page_start);
    let body = &view_src[page_start..page_end];
    let banner_start = body.find("if let (Some(banner), Some(spec))").unwrap();
    let banner_end = braced_end(body.as_bytes(), banner_start);
    let banner = &body[banner_start..banner_end];
    let mut release_sites: Vec<&str> = Vec::new();
    for pattern in ["release_obj(label)", "release_obj(button)"] {
        let count = banner.matches(pattern).count();
        release_sites.push(pattern);
        assert!(
            count <= 1,
            "{pattern} appears {count} times: a view must be released once"
        );
    }
    let strip_release = format!("release_obj({})", "strip");
    assert!(
        !body.contains(&strip_release),
        "the strip's reference belongs to finalize_settings_pages, not to render_page"
    );
    // The strip's one release lives in the finalize step.
    let builder_src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/settings/page_builder.rs"
    ))
    .expect("the page builder's own source");
    assert_eq!(
        builder_src
            .matches("release_obj(ui.permission_warning_view)")
            .count(),
        1,
        "the banner strip is released exactly once, by finalize_permission_banner"
    );
    // The other created views are balanced inside the renderer.
    let allocs = body.matches("class!(NSTextField), alloc]").count()
        + body.matches("class!(NSView), alloc]").count()
        - 1; // the strip, released by finalize
    let releases = body.matches("release_obj(label)").count()
        + body.matches("release_obj(button)").count()
        + body.matches("release_obj(control)").count();
    assert!(
        releases >= allocs,
        "render_page allocates {allocs} views itself but balances only {releases}"
    );
    let widgets = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/settings/page_widgets.rs"
    ))
    .expect("the page widgets' own source");
    for name in ["stacked_row_labels", "plain_row_label"] {
        let start = widgets.find(&format!("fn {name}(")).unwrap();
        let end = widgets[start..]
            .find("\n}")
            .map(|offset| start + offset)
            .unwrap();
        let body = &widgets[start..end];
        let allocs = body.matches("class!(NSTextField), alloc]").count()
            + body.matches("class!(NSView), alloc]").count();
        let releases = body.matches("release_obj(").count();
        assert!(
            releases >= allocs,
            "{name}: {allocs} allocs, {releases} releases"
        );
    }
}

#[test]
fn every_migrated_row_uses_the_shared_label_association() {
    // The row path must reuse SettingsRow::register_label (accessibility label + the
    // ROW_LABELS pairing the freeze lists read), not re-implement it. Asserted over the
    // renderer's own source, because the pairing itself lives in an AppKit-only registry the
    // M2 runner drives.
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/settings/page_view/render.rs"
    ))
    .expect("the page renderer's own source");
    let start = source.find("unsafe fn render_row(").expect("render_row");
    let end = source[start..]
        .find("\n/// Put a view in the box the layout computed.")
        .map(|offset| start + offset)
        .expect("the end of render_row");
    let body = &source[start..end];
    assert!(
        body.contains("SettingsRow::register_label(title_label, control)"),
        "a migrated row must register its label with its control"
    );
    assert!(
        !body.contains("setAccessibilityLabel"),
        "the row path must not re-implement the accessibility label"
    );
}

#[test]
fn the_separator_belongs_to_the_row_below_it() {
    // #4: the divider is created AFTER its row is opened and handed to the row's own
    // placements, so a re-flow moves it and a hidden group hides it. Both facts are asserted:
    // its box (the row's top edge) and that `render_row` places it through the row.
    let input = input();
    let view = switcher_page_view(&input);
    let layout_input = LayoutInput::for_test(600.0);
    let metrics = PageMetrics::for_layout(layout_input.layout);
    let layout = layout_rows(&view, &layout_input, &metrics);
    let row = view
        .rows
        .iter()
        .position(|row| row.separator_above)
        .expect("a row with a divider above it");
    let placed = &layout.rows[row];
    let separator = separator_frame(placed);
    assert_eq!(separator.origin.y, placed.frame.y + placed.frame.h);
    assert_eq!(separator.size.width, placed.frame.w);
    // The renderer places it on the row's own capture, not before the row opens.
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/settings/page_view/render.rs"
    ))
    .expect("the page renderer's own source");
    let start = source.find("unsafe fn render_row(").expect("render_row");
    // render_row's own body, not the span up to the next function: that span ends where the
    // neighbour starts, so it changes the moment the neighbour's visibility is adapted.
    let end = braced_end(source.as_bytes(), start);
    let body = &source[start..end];
    let open = body.find("canvas.next_row(").expect("the row opens");
    let divider = body
        .find("SettingsRow::separator_above_row(")
        .expect("the divider is built");
    assert!(
            divider > open,
            "the divider must be created after its row opened, or the canvas captures it for the previous row"
        );
    assert!(
            body.contains("canvas.place_pending_view(separator, separator_frame(placed))"),
            "the divider must go through the row's placements, so a re-flow moves it and its group hides it"
        );
    // And the card loop no longer builds dividers before opening their row.
    let page_start = source.find("unsafe fn render_page(").expect("render_page");
    let page = &source[page_start..source.find("unsafe fn apply_control_update").unwrap()];
    assert!(
        !page.contains("SettingsRow::separator_above_row("),
        "render_page must not create dividers: they belong to the row's own capture"
    );
}

/// No described row duplicates an action the page's own hook draws.
///
/// The Clipboard page draws its excluded-applications card end to end -- the list, the add
/// popup and the restore action. A static restore ROW in the description was drawn BEFORE the
/// hook, so the same action appeared twice: once directly under the card heading and once at
/// the card's end.
#[test]
fn no_described_row_duplicates_a_page_drawn_action() {
    let input = input();
    // Actions a page's hooks draw themselves, by page.
    let hook_drawn: [(SettingsPageId, &str); 1] = [(SettingsPageId::Clipboard, "restore_excluded")];
    for (page, action) in hook_drawn {
        let view = view_for(page, &input);
        let duplicated = view
            .rows
            .iter()
            .any(|row| matches!(row.id, ControlId::Action(id) if id == action));
        assert!(
            !duplicated,
            "{page:?} describes {action}, which its hook already draws"
        );
    }
}

#[test]
fn dbg_switcher_slider_frames() {
    let input = input();
    let view = switcher_page_view(&input);
    let layout_input = LayoutInput::for_test(620.0);
    let metrics = PageMetrics::for_layout(layout_input.layout);
    let layout = layout_rows(&view, &layout_input, &metrics);
    for (i, row) in view.rows.iter().enumerate() {
        if matches!(row.control, ControlKind::Slider { .. }) {
            let p = &layout.rows[i];
            println!(
                "row {i} {} slider x={} w={} h={} readout={:?} content_w={}",
                row.label_key,
                p.control_frame.x,
                p.control_frame.w,
                p.control_frame.h,
                p.readout_frame.map(|r| (r.x, r.y, r.w, r.h)),
                layout_input.content_w
            );
        }
    }
}

/// A control that needs a NUMBER takes it from the description's typed value.
///
/// `ControlUpdate` used to carry only text, so the applier parsed a popup's index and a
/// slider's stop back out of the string they had just been formatted into. The typed value is
/// what the description asserted; the string stays for display and for the tests that read the
/// description. This pins that no branch of the applier parses.
#[test]
fn the_applier_never_parses_a_controls_value_out_of_its_text() {
    let source = include_str!("../render.rs");
    let start = source
        .find("unsafe fn apply_control_update")
        .expect("the applier exists");
    let applier = &source[start..];
    let applier = &applier[..applier.find("\n}\n").expect("the applier ends")];
    assert!(
        !applier.contains(".parse::<"),
        "the applier parses a value out of text instead of using the typed one: {applier}"
    );
}
