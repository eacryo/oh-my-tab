//! The settings pages' M2 smoke runners.
//!
//! Each runner opens the real settings window, drives it and records a verdict through
//! smoke_verdict; lib.rs dispatches to them by flag. They live here rather than in mod.rs
//! because they are a quarter of the page-view module and no production code calls them.
//!
//! Nothing here is cfg(test): a smoke runner is a pub(crate) entry point the release binary
//! carries, so the tests behind cargo test cannot reach it.

use super::super::*;
use super::*;
/// A `SettingsUi` with no views behind it: the smoke runner builds pages on a detached
/// document to check what the renderer places, and the bindings it writes there are
/// discarded with it. Every field is empty, so a page hook writing one is harmless.
/// Whether a page-drawn caption can hold what its OWN CELL will draw.
///
/// Read from the live label's cell (`cellSizeForBounds:`), never from the measuring helper the page
/// used to size the row: a helper that under-measures computes the frame and the "needed" height
/// from the same wrong number, so a check built on it passes straight over the clipped line it was
/// written for. This is the assertion that fails when `caption_cell_height` starts snapping two
/// lines to one.
pub(crate) unsafe fn caption_frame_fits_its_cell(caption: *mut AnyObject) -> Result<(), String> {
    if caption.is_null() {
        return Err("the row owns no caption".to_string());
    }
    let frame: NSRect = msg_send![caption, frame];
    let cell: *mut AnyObject = msg_send![caption, cell];
    if cell.is_null() {
        return Err("the caption has no cell".to_string());
    }
    let needed: NSSize = msg_send![
        cell,
        cellSizeForBounds: NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(frame.size.width.max(1.0), 1.0e4)
        )
    ];
    if needed.height > frame.size.height + 0.5 {
        return Err(format!(
            "the caption needs {:.1}pt in a {:.1}pt-wide frame but the frame is {:.1}pt tall",
            needed.height, frame.size.width, frame.size.height
        ));
    }
    Ok(())
}

pub(crate) unsafe fn headless_settings_ui() -> SettingsUi {
    SettingsUi {
        permission_warning_label: std::ptr::null_mut(),
        permission_warning_button: std::ptr::null_mut(),
        page_viewport_h: 0.0,
        window: std::ptr::null_mut(),
        settings_search: sidebar::SettingsSearch::EMPTY,
        sidebar_highlight: std::ptr::null_mut(),
        sidebar_buttons: [std::ptr::null_mut(); SETTINGS_PAGE_COUNT],
        page_views: [std::ptr::null_mut(); SETTINGS_PAGE_COUNT],
        about_subtitle: std::ptr::null_mut(),
        accessibility_permission_status: std::ptr::null_mut(),
        accessibility_permission_button: std::ptr::null_mut(),
        screen_recording_permission_status: std::ptr::null_mut(),
        keychain_access_status: std::ptr::null_mut(),
        keychain_access_button: std::ptr::null_mut(),
        theme: std::ptr::null_mut(),
        glass_style: std::ptr::null_mut(),
        panel_material: std::ptr::null_mut(),
        thumbnails_enabled: std::ptr::null_mut(),
        focused_thumbnail_prewarm: std::ptr::null_mut(),
        show_app_name_in_cards: std::ptr::null_mut(),
        thumbnail_size: std::ptr::null_mut(),
        card_text_size: std::ptr::null_mut(),
        card_text_size_value_label: std::ptr::null_mut(),
        clipboard_image_rows: std::ptr::null_mut(),
        clipboard_image_rows_value_label: std::ptr::null_mut(),
        status_bar_text_size: std::ptr::null_mut(),
        status_bar_text_size_value_label: std::ptr::null_mut(),
        show_footer_title: std::ptr::null_mut(),
        modifier: std::ptr::null_mut(),
        locale: std::ptr::null_mut(),
        panel_close_enabled: std::ptr::null_mut(),
        panel_minimize_enabled: std::ptr::null_mut(),
        panel_hide_enabled: std::ptr::null_mut(),
        panel_quit_enabled: std::ptr::null_mut(),
        show_minimized: std::ptr::null_mut(),
        show_hidden_app_windows: std::ptr::null_mut(),
        show_other_desktops: std::ptr::null_mut(),
        windows_enabled: std::ptr::null_mut(),
        overlay_position: std::ptr::null_mut(),
        activation_mode: std::ptr::null_mut(),
        keystroke_display_enabled: std::ptr::null_mut(),
        keystroke_display_mode: std::ptr::null_mut(),
        keystroke_display_tap_level: std::ptr::null_mut(),
        keystroke_display_position: std::ptr::null_mut(),
        keystroke_display_initial_position: std::ptr::null_mut(),
        keystroke_display_max_extent: std::ptr::null_mut(),
        keystroke_display_max_extent_value: std::ptr::null_mut(),
        log_level: std::ptr::null_mut(),
        launch_at_login: std::ptr::null_mut(),
        hide_menu_bar_icon: std::ptr::null_mut(),
        reverse_scroll: std::ptr::null_mut(),
        enable_mouse: std::ptr::null_mut(),
        gestures_enabled: std::ptr::null_mut(),
        gesture_trigger_button: std::ptr::null_mut(),
        gesture_show_trail: std::ptr::null_mut(),
        gesture_show_hint: std::ptr::null_mut(),
        scroll_mode: std::ptr::null_mut(),
        line_count: std::ptr::null_mut(),
        line_count_label: std::ptr::null_mut(),
        line_count_value_label: std::ptr::null_mut(),
        disable_pointer_accel: std::ptr::null_mut(),
        pointer_accel_slider: std::ptr::null_mut(),
        pointer_accel_label: std::ptr::null_mut(),
        pointer_accel_value_label: std::ptr::null_mut(),
        smooth_scrolling_preset: std::ptr::null_mut(),
        smooth_scrolling_response: std::ptr::null_mut(),
        smooth_scrolling_response_value: std::ptr::null_mut(),
        smooth_scrolling_speed: std::ptr::null_mut(),
        smooth_scrolling_speed_value: std::ptr::null_mut(),
        smooth_scrolling_acceleration: std::ptr::null_mut(),
        smooth_scrolling_acceleration_value: std::ptr::null_mut(),
        smooth_scrolling_inertia: std::ptr::null_mut(),
        smooth_scrolling_inertia_value: std::ptr::null_mut(),
        mapping_layout_row: 0,
        update_host_row: 0,
        update_band_row: 0,
        mapping_scroll: std::ptr::null_mut(),
        mapping_doc: std::ptr::null_mut(),
        mapping_card: std::ptr::null_mut(),
        mapping_panel: std::ptr::null_mut(),
        mapping_rows: Vec::new(),
        clipboard_enabled: std::ptr::null_mut(),
        clipboard_shortcut: std::ptr::null_mut(),
        clipboard_shortcut_caption: std::ptr::null_mut(),
        gesture_hotkey_caption: std::ptr::null_mut(),
        clipboard_shortcut_row_height: 0.0,
        clipboard_pin_follow_row_height: 0.0,
        clipboard_row_gap: 0.0,
        key_bindings_enabled: std::ptr::null_mut(),
        key_bindings_doc: std::ptr::null_mut(),
        key_bindings_empty: std::ptr::null_mut(),
        key_bindings_footer: std::ptr::null_mut(),
        key_bindings_footer_button: std::ptr::null_mut(),
        key_bindings_row: 0,
        key_binding_rows: Vec::new(),
        key_binding_items_rendered: Vec::new(),
        clipboard_clear_on_quit: std::ptr::null_mut(),
        clipboard_separate_clear_all: std::ptr::null_mut(),
        clipboard_move_used_to_top: std::ptr::null_mut(),
        clipboard_delete_after_paste: std::ptr::null_mut(),
        clipboard_clear_system_pasteboard_after_paste: std::ptr::null_mut(),
        clipboard_max_entries: std::ptr::null_mut(),
        clipboard_auto_expire_days: std::ptr::null_mut(),
        clipboard_auto_expire_days_value_label: std::ptr::null_mut(),
        clipboard_show_source_app: std::ptr::null_mut(),
        clipboard_pin_follow: std::ptr::null_mut(),
        clipboard_picker_placement: std::ptr::null_mut(),
        clipboard_paused: std::ptr::null_mut(),
        clipboard_excluded_remove: Vec::new(),
        clipboard_excluded_add: std::ptr::null_mut(),
        clipboard_excluded_candidates: Vec::new(),
        clipboard_excluded_restore: std::ptr::null_mut(),
        add_mapping_button: std::ptr::null_mut(),
        mapping_enabled: std::ptr::null_mut(),
        mapping_empty: std::ptr::null_mut(),
        device_indicator: std::ptr::null_mut(),
        device_info_caption: std::ptr::null_mut(),
        restore_defaults: RestoreDefaultsControl::empty(),
        page_restores: std::array::from_fn(|_| RestoreDefaultsControl::empty()),
        page_canvases: std::array::from_fn(|_| PageCanvas::empty()),
        permission_warning_view: std::ptr::null_mut(),
        update_auto_check: std::ptr::null_mut(),
        update_auto_download: std::ptr::null_mut(),
        update_check_button: std::ptr::null_mut(),
        update_host: std::ptr::null_mut(),
        update_host_window: std::ptr::null_mut(),
        update_card: std::ptr::null_mut(),
        update_divider: std::ptr::null_mut(),
        update_card_expanded: false,
        finder_menu_status: std::ptr::null_mut(),
        finder_menu_enabled: std::ptr::null_mut(),
        finder_menu_layout: std::ptr::null_mut(),
        finder_menu_new_file_switches: Vec::new(),
        finder_menu_terminal_switches: Vec::new(),
        finder_menu_copy_path: std::ptr::null_mut(),
        finder_menu_copy_sha256: std::ptr::null_mut(),
        control_table: ControlTable::new(),
        migrated_pages: std::array::from_fn(|_| None),
    }
}

pub(crate) unsafe fn migrated_pages_smoke_runner() -> bool {
    let mut ok = true;
    for page in [
        SettingsPageId::Switcher,
        SettingsPageId::General,
        SettingsPageId::Clipboard,
        SettingsPageId::About,
        SettingsPageId::KeystrokeDisplay,
        SettingsPageId::FinderMenu,
        SettingsPageId::Gestures,
        SettingsPageId::KeyBindings,
        SettingsPageId::Mouse,
    ] {
        let document: *mut AnyObject = msg_send![class!(NSView), alloc];
        let document: *mut AnyObject = msg_send![
            document,
            initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(600.0, 800.0))
        ];
        let input = PageInput::from_config(
            crate::config::Config::default(),
            None,
            crate::theme::ThemeInput::current(),
        );
        let view = view_for(page, &input);
        let metrics = PageMetrics::for_layout(SettingsLayout::new(600.0));
        let mut canvas = PageCanvas::new(
            document,
            600.0,
            SettingsLayout::new(600.0),
            &t(view.title_key),
            document_fixture_height(),
            700.0,
        );
        let fixture_layout = SettingsLayout::new(600.0);
        let measured = MeasuredText::measure(&view, fixture_layout, 620.0);
        let layout_input = LayoutInput {
            content_w: 600.0,
            doc_top: document_fixture_height(),
            header_offset: canvas.header_offset(),
            banner_frame: BannerFrame::of(banner_parent_frame(
                28.0,
                700.0,
                600.0,
                banner_height(false),
            )),
            layout: fixture_layout,
            text: measured,
            extra_row_heights: view
                .extra_rows
                .iter()
                .map(|extra| extra.height.resolve(&input, 600.0))
                .collect(),
        };
        let layout = layout_rows(&view, &layout_input, &metrics);
        // The description half: one laid-out row per described row, one surface per card
        // (page-drawn-only cards included), and a sequence that places every row exactly once.
        let described_laid_out = layout.rows.len() == view.rows.len();
        let highest_card = view
            .rows
            .iter()
            .map(|row| row.card)
            .chain(view.extra_rows.iter().map(|extra| extra.card))
            .max()
            .map_or(0, |card| card + 1);
        let cards_laid_out =
            layout.cards.len() == highest_card && layout.card_tops.len() >= highest_card;
        let sequence_complete = layout.sequence.len() == view.rows.len() + view.extra_rows.len();
        // The rendered half: build the page's REAL views, with the page's own hooks, on the
        // detached document. This is what catches a control that never entered the view tree and a
        // placement that ignored the row's origin.
        let mut ui = headless_settings_ui();
        let mut hooks = page_hooks(page);
        let rendered = match render_page(
            &mut canvas,
            std::ptr::null_mut(),
            &view,
            &layout,
            &metrics,
            &mut ui,
            hooks.as_mut(),
        ) {
            Ok(rendered) => rendered,
            Err(error) => {
                log_info!("[smoke-migrated-pages] {page:?}: render refused ({error:?})");
                ok = false;
                continue;
            }
        };
        // The canvas settles its row bookkeeping when the page is finished: every view is placed and
        // every row flushed by then, which is what the placement checks read.
        let _: f64 = canvas.finish();
        // Every DESCRIBED row placed its control (by identity): the count is larger than the row
        // count whenever a page also draws rows of its own or a value row carries an action button,
        // and smaller when a row failed to build — which is the failure this catches.
        let described_ok = !view.rows.is_empty()
            && view
                .rows
                .iter()
                .all(|row| rendered.controls.iter().any(|control| control.id == row.id));
        if !described_ok {
            log_info!(
                "[smoke-migrated-pages] {page:?}: a described row placed no control ({} controls)",
                rendered.controls.len()
            );
        }
        let mut placed_ok = scene_holds_placements(&canvas, &view, &layout);
        if page == SettingsPageId::Gestures {
            // The hotkey row carries no inline readout any more: its mapping-conflict note is said in
            // the shared bubble, and an inline copy is what used to clip the sentence under the
            // button. A row that grows a readout again is the regression this fails on.
            let hotkey_row = rendered
                .controls
                .iter()
                .find(|control| matches!(&control.extra, RenderedExtra::GestureHotkey { .. }));
            // It owns its instruction caption and no inline readout: the mapping-conflict note is
            // said in the shared bubble, and the row's copy is what states how to record.
            let caption_ptr = hotkey_row.and_then(|control| match control.extra {
                RenderedExtra::GestureHotkey { caption } => Some(caption),
                _ => None,
            });
            let caption_bound = caption_ptr.is_some_and(|caption| !caption.is_null());
            // The caption's frame must hold what its own cell draws: the idle copy wraps to two
            // lines at a narrow column, and a frame sized for one clipped the second one.
            let caption_fits = caption_ptr
                .map(|caption| caption_frame_fits_its_cell(caption))
                .unwrap_or_else(|| Err("the hotkey row owns no caption".to_string()));
            if let Err(error) = caption_fits {
                log_info!("[smoke-migrated-pages] Gestures: {error}");
                placed_ok = false;
            }
            if !hotkey_row.is_some_and(|control| control.readout.is_null()) || !caption_bound {
                log_info!(
                    "[smoke-migrated-pages] Gestures: the hotkey row drew an inline readout again"
                );
                placed_ok = false;
            }
        }
        // Every page-drawn row placed at least one view, inside the row the layout reserved.
        for (index, spec) in view.extra_rows.iter().enumerate() {
            let Some(row) = canvas_row_of_drawn_row(&layout, index) else {
                log_info!(
                    "[smoke-migrated-pages] {page:?}: {} is not in the layout",
                    spec.key
                );
                placed_ok = false;
                continue;
            };
            let (_, origin, height) = layout.drawn_rows[index];
            let Some(views) = canvas.row_views(row) else {
                log_info!(
                    "[smoke-migrated-pages] {page:?}: page-drawn row {} ({}) placed nothing",
                    index,
                    spec.key
                );
                placed_ok = false;
                continue;
            };
            // A row the flow grows later (the About page's inline update host) starts at zero
            // height by design: what matters is that it placed its views inside the page.
            if views.is_empty() || height < 0.0 || origin > layout.first_card_top {
                log_info!(
                    "[smoke-migrated-pages] {page:?}: page-drawn row {} ({}) is empty or outside the page",
                    index,
                    spec.key
                );
                placed_ok = false;
            }
        }
        // DRIVE the production re-flow: collapse a conditional group, re-flow, re-assert; then move
        // the viewport, which shifts every row's origin, and re-assert again. A renderer that placed
        // its views once and never followed the canvas fails HERE.
        let groups = description_groups(&view);
        if let Some(group) = groups.first().copied() {
            canvas.set_group_visible(group, true);
            placed_ok &= scene_holds_placements(&canvas, &view, &layout);
            canvas.set_group_visible(group, false);
            placed_ok &= scene_holds_placements(&canvas, &view, &layout);
        }
        canvas.set_viewport(520.0);
        placed_ok &= scene_holds_placements(&canvas, &view, &layout);
        canvas.set_viewport(700.0);
        let cards_ok = view_cards_contain_their_rows(&view, &layout);
        if !(described_laid_out && cards_laid_out && sequence_complete && cards_ok) {
            log_info!(
                "[smoke-migrated-pages] {page:?}: described={described_laid_out} cards={cards_laid_out} sequence={sequence_complete} cards_cover={cards_ok}"
            );
            ok = false;
        }
        if !(described_ok && placed_ok) {
            log_info!(
                "[smoke-migrated-pages] {page:?}: controls={described_ok} placements={placed_ok}"
            );
            ok = false;
        }
    }
    ok
}

/// Each card's surface must cover every row inside it, described or page-drawn.
fn view_cards_contain_their_rows(view: &PageView, layout: &PageLayout) -> bool {
    for card in &layout.cards {
        for (index, row) in view.rows.iter().enumerate() {
            if row.card != card.index {
                continue;
            }
            let frame = &layout.rows[index].frame;
            if frame.y < card.frame.y - 1.0e-6
                || frame.y + frame.h > card.frame.y + card.frame.h + 1.0e-6
            {
                return false;
            }
        }
        for (position, extra) in view.extra_rows.iter().enumerate() {
            if extra.card != card.index {
                continue;
            }
            let (_, origin, height) = layout.drawn_rows[position];
            if origin < card.frame.y - 1.0e-6
                || origin + height > card.frame.y + card.frame.h + 1.0e-6
            {
                return false;
            }
        }
    }
    true
}

/// Every row the description declares has views in the detached document, with a real size: the
/// check the smoke runner drives after building a page's real views.
/// Every view a row placed is in the detached document, has a real size, and sits at its row's
/// CURRENT origin plus the offset the layout gave it — read from the CANVAS, which is what a
/// re-flow moves. A renderer that placed its views once and never followed a re-flow fails here.
/// Every view a row placed is a subview of the document with a real size, and its frame matches the
/// row's CURRENT canvas origin plus the offset the layout gave it. Reading the origin from the
/// CANVAS is the point: it is what a re-flow moves, so a renderer that placed its views once and
/// never followed a re-flow fails here.
/// Every row the description placed is still where the canvas's current re-flow says it is: the
/// canvas records each view's offset from its row's origin, applies the row's CURRENT origin, and
/// `placements_are_stale` reports a view that did not follow. Reading the ORIGIN from the canvas is
/// the point — a group collapse or a viewport change moves it, and a renderer that placed its views
/// once and never followed fails HERE.
unsafe fn scene_holds_placements(
    canvas: &PageCanvas,
    view: &PageView,
    layout: &PageLayout,
) -> bool {
    let document = canvas.document();
    if document.is_null() {
        return false;
    }
    // The canvas row is the row's position in the ONE sequence, never its index among described
    // rows: a page whose earlier cards hold page-drawn rows puts its later rows further down.
    let canvas_rows = canvas_rows(view);
    for (row, _spec) in view.rows.iter().enumerate() {
        let canvas_row = canvas_rows[row];
        let (Some(_origin), Some(placements)) = (
            canvas.current_row_origin(canvas_row),
            canvas.row_placements(canvas_row),
        ) else {
            return false;
        };
        // A described row always draws at least its copy and its control.
        if placements.len() < 2 {
            return false;
        }
        if canvas.placements_are_stale(canvas_row) {
            return false;
        }
        for (placed_view, _offset) in placements {
            if placed_view.is_null() {
                return false;
            }
            let frame: NSRect = msg_send![*placed_view, frame];
            if frame.size.width <= 0.0 || frame.size.height <= 0.0 {
                return false;
            }
            let parent: *mut AnyObject = msg_send![*placed_view, superview];
            if parent != document {
                return false;
            }
        }
    }
    // The layout must still describe the same page it was computed for.
    layout.rows.len() == view.rows.len()
}

/// Drive the About page's inline update flow on a detached document, through the SAME canvas calls
/// the production path uses, and assert its geometry.
///
/// The relations, not the absolute coordinates: the document is laid out from its TOP edge, so
/// growing it moves every row. What must hold is (a) the divider sits on the last row above the
/// band -- before, during and after the flow -- and (b) the expanded host's top edge is that same
/// line, so the panel takes the check button's place instead of starting a row lower.
pub(crate) unsafe fn about_update_flow_smoke_runner() -> bool {
    let document: *mut AnyObject = msg_send![class!(NSView), alloc];
    let document: *mut AnyObject = msg_send![
        document,
        initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(600.0, 900.0))
    ];
    let input = PageInput::from_config(
        crate::config::Config::default(),
        None,
        crate::theme::ThemeInput::current(),
    );
    let view = view_for(SettingsPageId::About, &input);
    let layout = SettingsLayout::new(600.0);
    let metrics = PageMetrics::for_layout(layout);
    let mut canvas = PageCanvas::new_at(
        document,
        600.0,
        layout,
        view.header_offset.unwrap_or(0.0),
        1200.0,
        900.0,
    );
    let mut ui = headless_settings_ui();
    let mut hooks = AboutHooks {
        window: std::ptr::null_mut(),
    };
    let layout_input = LayoutInput {
        content_w: 600.0,
        doc_top: 1200.0,
        header_offset: canvas.header_offset(),
        banner_frame: BannerFrame::of(banner_parent_frame(
            28.0,
            900.0,
            600.0,
            banner_height(false),
        )),
        layout,
        text: MeasuredText::measure(&view, layout, 620.0),
        extra_row_heights: view
            .extra_rows
            .iter()
            .map(|extra| extra.height.resolve(&input, 600.0))
            .collect(),
    };
    let page_layout = layout_rows(&view, &layout_input, &metrics);
    if render_page(
        &mut canvas,
        std::ptr::null_mut(),
        &view,
        &page_layout,
        &metrics,
        &mut ui,
        &mut hooks,
    )
    .is_err()
    {
        log_info!("[smoke-update-flow] the About page refused to build");
        return false;
    }
    let _: f64 = canvas.finish();
    let (band_row, host_row) = (ui.update_band_row, ui.update_host_row);
    let divider = ui.update_divider;
    if divider.is_null() || band_row == host_row || ui.update_host.is_null() {
        log_info!("[smoke-update-flow] the flow has no distinct rows, host and divider");
        return false;
    }
    // The row the divider marks the bottom edge of: the last row settled before the band.
    let row_above = band_row.saturating_sub(1);
    let divider_y = |_canvas: &PageCanvas| -> f64 {
        let frame: NSRect = msg_send![divider, frame];
        frame.origin.y
    };
    // The compact state: the divider is the row above's bottom edge.
    let _: f64 = canvas.finish();
    let row_above_origin = canvas.current_row_origin(row_above);
    let compact_ok = row_above_origin
        .map(|origin| (divider_y(&canvas) - origin).abs() <= 0.5)
        .unwrap_or(false);
    // The flow's own calls, in production's order.
    let host_h = 240.0;
    canvas.set_bottom_padding(SettingsPageHeader::BOTTOM_PADDING.min(24.0));
    canvas.set_row_consume(band_row, 0.0);
    canvas.set_row_consume(host_row, host_h);
    let host: NSRect = msg_send![ui.update_host, frame];
    let _: () = msg_send![
        ui.update_host,
        setFrameSize: NSSize::new(host.size.width, host_h)
    ];
    let _: f64 = canvas.finish();
    // (a) the divider still marks the row above's bottom edge.
    let divider_after = divider_y(&canvas);
    let anchored_ok = canvas
        .current_row_origin(row_above)
        .map(|origin| (divider_after - origin).abs() <= 0.5)
        .unwrap_or(false);
    // (b) the expanded host hangs from that same line.
    let host: NSRect = msg_send![ui.update_host, frame];
    let host_top = host.origin.y + host.size.height;
    let host_ok = (host_top - divider_after).abs() <= 1.0;
    // A PHASE CHANGE inside the flow: Sparkle resizes the host as its state advances, calling the
    // flow's expand again with a different height. The divider must stay on the row above's bottom
    // edge and the host's top edge must still meet it, at every height -- not only the first.
    let mut phase_ok = true;
    for phase_h in [180.0_f64, 300.0, 120.0] {
        canvas.set_row_consume(host_row, phase_h);
        let host: NSRect = msg_send![ui.update_host, frame];
        let _: () = msg_send![
            ui.update_host,
            setFrameSize: NSSize::new(host.size.width, phase_h)
        ];
        let _: f64 = canvas.finish();
        let divider_y = divider_y(&canvas);
        let anchored = canvas
            .current_row_origin(row_above)
            .map(|origin| (divider_y - origin).abs() <= 0.5)
            .unwrap_or(false);
        let host: NSRect = msg_send![ui.update_host, frame];
        let meets = ((host.origin.y + host.size.height) - divider_y).abs() <= 1.0;
        if !(anchored && meets) {
            log_info!(
                "[smoke-update-flow] phase height {phase_h}: anchored={anchored} meets={meets}"
            );
            phase_ok = false;
        }
    }
    // Collapsing restores the compact relation.
    let _: () = msg_send![ui.update_host, setFrameSize: NSSize::new(host.size.width, 0.0)];
    canvas.set_row_consume(host_row, 0.0);
    canvas.set_row_consume(band_row, SettingsLayout::SINGLE_LINE_ROW_H);
    let _: f64 = canvas.finish();
    let collapsed_ok = canvas
        .current_row_origin(row_above)
        .map(|origin| (divider_y(&canvas) - origin).abs() <= 0.5)
        .unwrap_or(false);
    let ok = compact_ok && anchored_ok && host_ok && phase_ok && collapsed_ok;
    if !ok {
        log_info!(
            "[smoke-update-flow] compact={compact_ok} anchored={anchored_ok} host={host_ok} (gap={:.1}) collapsed={collapsed_ok}",
            host_top - divider_after
        );
    } else {
        log_info!("[smoke-update-flow] 4 relations plus 3 phase heights checked");
    }
    ok
}

/// The height the detached smoke document is provisioned with.
fn document_fixture_height() -> f64 {
    900.0
}
