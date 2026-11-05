//! The the description itself: what each page states.

use super::*;
#[test]
fn switcher_page_view_describes_every_row_with_its_identity_and_copy() {
    let view = switcher();
    assert_eq!(view.page, SettingsPageId::Switcher);
    assert_eq!(view.rows.len(), 17);
    // Every declared row carries a label key and an identity; the copy keys are the ones the
    // page's own labels used before the migration.
    let labels: Vec<&str> = view.rows.iter().map(|row| row.label_key).collect();
    assert_eq!(labels[0], "settings.row_windows_enabled");
    assert_eq!(labels[1], "settings.row_show_minimized");
    assert_eq!(labels[4], "settings.row_window_display_mode");
    assert_eq!(labels[8], "settings.row_card_text_size");
    assert_eq!(labels[16], "settings.row_modifier");
    // The field order is the page's own order: a reorder is a visible change, not a detail.
    assert_eq!(
        fields(&view),
        vec![
            ControlField::WindowsEnabled,
            ControlField::ShowMinimized,
            ControlField::ShowHiddenAppWindows,
            ControlField::ShowOtherDesktops,
            ControlField::ThumbnailsEnabled,
            ControlField::FocusedThumbnailPrewarm,
            ControlField::ShowAppNameInCards,
            ControlField::ThumbnailSize,
            ControlField::CardTextSize,
            ControlField::StatusBarTextSize,
            ControlField::OverlayPosition,
            ControlField::ActivationMode,
            ControlField::PanelCloseEnabled,
            ControlField::PanelMinimizeEnabled,
            ControlField::PanelHideEnabled,
            ControlField::PanelQuitEnabled,
            ControlField::Modifier,
        ]
    );
    // The control kinds are presentation only; the identity above is what the dispatcher uses.
    assert!(matches!(view.rows[0].control, ControlKind::Switch));
    assert!(matches!(view.rows[4].control, ControlKind::Popup { .. }));
    assert!(matches!(view.rows[8].control, ControlKind::Slider { .. }));
    // The cards are the inline builder's three, in its order: the overlay switch alone, then
    // every other window setting, then the keyboard row.
    let sections: Vec<&str> = view.rows.iter().map(|row| row.section).collect();
    assert_eq!(sections[0], "settings.header_windows");
    assert_eq!(sections[1], "settings.header_window_options");
    assert_eq!(sections[12], "settings.header_window_options");
    assert_eq!(sections[16], "settings.header_keyboard");
    assert_eq!(view.rows[0].card, 0);
    assert_eq!(view.rows[1].card, 1);
    assert_eq!(view.rows[16].card, 2);
    // The first row of every card has no divider above it; every later row in the card does.
    for (index, row) in view.rows.iter().enumerate() {
        let first_of_card = index == 0 || view.rows[index - 1].card != row.card;
        assert_eq!(
            row.separator_above, !first_of_card,
            "row {index} of card {} states the wrong divider",
            row.card
        );
    }
    // The thumbnail-only block is a conditional group, so its rows can be skipped together.
    let grouped: Vec<usize> = view
        .rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.group == Some(RowGroup::ThumbnailOnly))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(grouped, vec![5, 6, 7]);
}

#[test]
fn general_page_view_describes_every_row_with_its_identity_and_copy() {
    let view = general();
    assert_eq!(view.page, SettingsPageId::General);
    assert_eq!(view.rows.len(), 10);
    assert_eq!(
        fields(&view),
        vec![
            ControlField::LaunchAtLogin,
            ControlField::HideMenuBarIcon,
            ControlField::Theme,
            ControlField::PanelMaterial,
            ControlField::GlassStyle,
            ControlField::Locale,
            ControlField::LogLevel,
        ]
    );
    // The three action rows carry an action identity and no config field.
    let actions: Vec<&str> = view
        .rows
        .iter()
        .filter_map(|row| match row.id {
            ControlId::Action(action) => Some(action),
            ControlId::Field(_) => None,
        })
        .collect();
    assert_eq!(
        actions,
        vec!["export_logs", "export_settings", "import_settings"]
    );
    // The glass style is the Liquid-Glass block; the two backup rows are stacked copy.
    assert_eq!(view.rows[4].group, Some(RowGroup::GlassStyle));
    assert_eq!(view.rows[8].copy, RowCopy::Captioned);
    assert_eq!(view.rows[9].copy, RowCopy::Captioned);
    // The permission banner belongs to the page, with the action its button runs.
    let banner = view.banner.as_ref().expect("General carries the banner");
    assert_eq!(banner.action_id, "open_privacy");
    assert!(!t(banner.copy_key).is_empty());
}

#[test]
fn page_view_reads_only_its_input() {
    // The same config with a different device is still one description: the Switcher and
    // General pages do not depend on the selected mouse.
    let mut config = Config::default();
    config.windows.show_minimized = true;
    let view = switcher_page_view(&PageInput::from_config(
        config.clone(),
        Some((0x1234, 0x5678)),
        crate::theme::ThemeInput::current(),
    ));
    assert_eq!(view.rows[1].value, Value::Switch(true));
    // ... and a changed field changes the description's value, not its shape.
    config.keyboard.modifier = "control".to_string();
    let other = switcher_page_view(&PageInput::from_config(
        config,
        None,
        crate::theme::ThemeInput::current(),
    ));
    let modifier = other
        .rows
        .iter()
        .find(|row| row.field() == Some(ControlField::Modifier))
        .expect("the modifier row");
    assert_eq!(
        modifier.value,
        Value::Index(crate::config::switcher_modifier_index_of("control"))
    );
}

#[test]
fn page_view_search_entries_are_values_not_side_effects() {
    let _guard = sidebar::lock_search_index_for_test();
    let entries_before =
        sidebar::recorded_search_rows(settings_page_index(SettingsPageId::Switcher));
    let view = switcher();
    // A pure description; producing it never writes to the index.
    assert_eq!(
        sidebar::recorded_search_rows(settings_page_index(SettingsPageId::Switcher)),
        entries_before
    );
    assert_eq!(view.rows[0].search.label, t("settings.row_windows_enabled"));
    assert_eq!(
        view.rows[0].search.desc.as_deref(),
        Some(t("settings.desc_windows_enabled").as_str())
    );
    assert_eq!(view.rows[0].search.section, t("settings.header_windows"));
    // A dropdown's choices are part of what the search matches.
    let thumbnail_size = &view.rows[7];
    assert!(!thumbnail_size.search.options.is_empty());
    assert!(matches!(thumbnail_size.control, ControlKind::Popup { .. }));
}

#[test]
fn the_description_groups_are_the_ones_the_renderer_registers() {
    // A conditional row hides through the canvas's group list; a group the description names
    // but the renderer never registers would never hide, and one it registers without a row
    // would hide nothing. The renderer's list is asserted through the same function it uses.
    for (page, expected) in [
        (SettingsPageId::Switcher, vec![RowGroup::ThumbnailOnly]),
        (SettingsPageId::General, vec![RowGroup::GlassStyle]),
    ] {
        let view = view_for(page, &input());
        assert_eq!(description_groups(&view), expected, "{page:?}");
    }
}

#[test]
fn the_update_description_carries_the_control_value_and_its_display_separately() {
    // The status-bar slider's domain is stop indices (0 = hidden, 1..=8 = 13..=20pt) while its
    // readout shows the point size: a refill that wrote the index to both showed "1" where
    // the page says "13".
    let mut config = Config::default();
    config.fonts.status_bar_size = 13.0;
    let input = PageInput::from_config(config, None, crate::theme::ThemeInput::current());
    let view = switcher_page_view(&input);
    let updates = page_update_description(&input, &[SettingsPageId::Switcher]);
    let update = updates
        .iter()
        .find(|update| update.field == ControlField::StatusBarTextSize)
        .expect("the footer text-size row");
    assert_eq!(update.value, "1", "the slider rests on stop 1");
    assert_eq!(update.display, "13", "the readout shows the point size");
    assert_ne!(
        update.value, update.display,
        "stop index and point size differ"
    );
    // A field whose readout IS its value keeps the two equal.
    let card_text = updates
        .iter()
        .find(|update| update.field == ControlField::CardTextSize)
        .unwrap();
    assert_eq!(card_text.value, card_text.display);
    let _ = view;
}

#[test]
fn a_description_with_a_duplicate_identity_is_refused_before_any_view_exists() {
    // MEDIUM 3: the identity check reads the description, so the refusal happens before the
    // page builds a view. This is the production validator (`build_page`'s first step) applied
    // to a page whose rows repeat an identity.
    let input = input();
    let mut view = switcher_page_view(&input);
    view.rows[1].id = view.rows[0].id.clone();
    assert_eq!(
        description_identity_conflict(&view),
        Err(RegisterError::DuplicateId)
    );
    // The clean description passes the same check.
    let clean = switcher_page_view(&input);
    assert_eq!(description_identity_conflict(&clean), Ok(()));
}

#[test]
fn layout_rows_sizes_a_stacked_row_by_its_measured_caption() {
    let view = general();
    let mut layout_input = LayoutInput::for_test(600.0);
    let metrics = PageMetrics::for_layout(layout_input.layout);
    let index = view
        .rows
        .iter()
        .position(|row| row.caption_key.is_some())
        .unwrap();
    layout_input.text = view_measured(&view, index, 24.0);
    let layout = layout_rows(&view, &layout_input, &metrics);
    let placed = &layout.rows[index];
    // Title + gap + the caption that was measured (plus the row's own padding).
    assert!(placed.frame.h >= 18.0 + 3.0 + 24.0);
    let caption = placed.caption_frame.expect("a stacked row has a caption");
    assert!(caption.h >= 24.0 - 1e-9);
    assert!((caption.y + caption.h + 3.0 - placed.label_frame.y).abs() < 1e-9);
}

#[test]
fn the_update_description_matches_each_row_initial_value_item_by_item() {
    let input = input();
    for (page, view) in [
        (SettingsPageId::Switcher, switcher()),
        (SettingsPageId::General, general()),
    ] {
        let updates = page_update_description(&input, &[page]);
        let fields_with_values: Vec<ControlField> =
            view.rows.iter().filter_map(|row| row.field()).collect();
        assert_eq!(
            updates
                .iter()
                .map(|update| update.field)
                .collect::<Vec<_>>(),
            fields_with_values,
            "{page:?}: one entry per config field, in row order"
        );
        let rows: Vec<&RowSpec> = view
            .rows
            .iter()
            .filter(|row| row.field().is_some())
            .collect();
        for (row, update) in rows.iter().zip(updates.iter()) {
            assert_eq!(update.value, row.value.describe(), "{page:?}: {update:?}");
            match (&row.control, row.value.clone()) {
                (ControlKind::Switch, Value::Switch(_)) => {
                    assert!(update.value == "on" || update.value == "off");
                }
                (ControlKind::Popup { options }, Value::Index(index)) => {
                    assert!(
                        index < options.len(),
                        "the selected index is one of the choices"
                    );
                    assert_eq!(update.value, index.to_string());
                }
                (ControlKind::Slider { .. }, Value::Slider(value)) => {
                    assert_eq!(update.value, value.to_string());
                }
                (ControlKind::Button { .. }, Value::None) => {
                    panic!("an action row has no field")
                }
                _ => panic!("the kind and the value agree"),
            }
        }
    }
}

#[test]
fn every_static_state_control_of_a_migrated_page_appears_in_the_update_description() {
    let input = input();
    let updates: Vec<ControlField> =
        page_update_description(&input, &[SettingsPageId::Switcher, SettingsPageId::General])
            .into_iter()
            .map(|update| update.field)
            .collect();
    for page in [SettingsPageId::Switcher, SettingsPageId::General] {
        for row in view_for(page, &input).rows {
            if matches!(row.control, ControlKind::Button { .. }) {
                continue;
            }
            if let Some(field) = row.field() {
                assert!(
                    updates.contains(&field),
                    "{page:?} {field:?} is missing from the description"
                );
            }
        }
    }
}

/// Every page's real description must pass the identity check the build runs before creating a
/// view: a duplicate there refuses the whole page and it renders empty. This is the guard the
/// About page's two links needed and did not have.
#[test]
fn every_pages_description_passes_the_identity_check_the_build_runs() {
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
        assert!(
            description_identity_conflict(&view).is_ok(),
            "{page:?} declares a duplicate control identity, so its build would be refused"
        );
    }
}

/// A card's section heading sits ABOVE its card, and the card's first row starts below the
/// card's top edge. A heading that overlaps its own first row's control is the failure the
/// settings-layout smoke reports as "views overlap" -- reproduced here without a window.
#[test]
fn every_card_heading_hangs_above_its_first_row_for_every_page() {
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
        for card in &layout.cards {
            let header = card.header;
            // In the document's unflipped coordinates a larger y is HIGHER on screen: the
            // heading's bottom edge must sit above the card's top edge, and it must not
            // overlap the card's own surface.
            let card_top = card.frame.y + card.frame.h;
            assert!(
                header.y >= card_top - 1.0e-6,
                "{page:?}: card {} heading (y={}) is below its card top ({card_top}) with h={}",
                card.index,
                header.y,
                header.h
            );
            assert!(
                header.y >= card_top + header.h - 1.0e-6 || header.y > card_top,
                "{page:?}: card {} heading ({header:?}) overlaps the card surface",
                card.index
            );
        }
    }
}

/// A described row's label VIEW must sit inside its own row band.
///
/// The regression this pins: the renderer built the plain row's label at the row's band origin
/// with the band's height, while the band was also where the CONTROL was centred. The label's
/// frame therefore reached 11pt above the card's top edge on every page, and the settings-layout
/// smoke reported the first row of each card as a control overlapping the heading above it.
/// The layout states the label's centred box; the view is built in the band and centred, so the
/// box the smoke measures and the frame the reader sees agree.
#[test]
fn a_described_rows_label_box_sits_inside_its_row_band() {
    let input = input();
    for page in [
        SettingsPageId::Switcher,
        SettingsPageId::General,
        SettingsPageId::Mouse,
    ] {
        let view = view_for(page, &input);
        let layout_input = LayoutInput::for_test(620.0);
        let metrics = PageMetrics::for_layout(layout_input.layout);
        let layout = layout_rows(&view, &layout_input, &metrics);
        for (index, row) in layout.rows.iter().enumerate() {
            if row.frame.h <= 0.0 {
                continue;
            }
            let label_box = row.label_frame;
            assert!(
                label_box.y >= row.frame.y - 0.01,
                "{page:?} row {index}: the label box starts {:.2} below the band's bottom edge",
                row.frame.y - label_box.y
            );
            assert!(
                label_box.y + label_box.h <= row.frame.y + row.frame.h + 0.01,
                "{page:?} row {index}: the label box reaches {:.2} above the band's top edge",
                label_box.y + label_box.h - (row.frame.y + row.frame.h)
            );
        }
    }
}

/// A captioned row's control is centred in the row, not pinned to its band's top edge.
///
/// The inline builder centred every captioned row's control (`SettingsRow::captioned` calls
/// `center_control`). The migrated layout pinned it to `top - control_h` whenever the copy was
/// stacked, which lifted the switch 22pt above the row's middle on every captioned row --
/// visible as a switch sitting at the row's top edge instead of beside its two lines.
#[test]
fn a_captioned_rows_control_is_centred_in_its_band() {
    let input = input();
    for page in [SettingsPageId::General, SettingsPageId::Switcher] {
        let view = view_for(page, &input);
        let layout_input = LayoutInput::for_test(620.0);
        let metrics = PageMetrics::for_layout(layout_input.layout);
        let layout = layout_rows(&view, &layout_input, &metrics);
        for (index, row) in layout.rows.iter().enumerate() {
            if view.rows[index].copy != RowCopy::Captioned || row.control_frame.h <= 0.0 {
                continue;
            }
            let control_mid = row.control_frame.y + row.control_frame.h / 2.0;
            let band_mid = row.frame.y + row.frame.h / 2.0;
            assert!(
                (control_mid - band_mid).abs() <= 1.0,
                "{page:?} row {index}: the control's middle sits {:+.2}pt off the row's",
                control_mid - band_mid
            );
        }
    }
}

/// A card holding only page-drawn rows must still carry a translated heading.
///
/// Its heading is taken from its first page-drawn row's SECTION key; falling back to the row's
/// own key printed the internal identifier (device) on the Mouse page where the design's
/// heading (Device / 裝置) belongs.
#[test]
fn a_page_drawn_only_card_names_a_translated_heading() {
    let input = input();
    for page in [
        SettingsPageId::Mouse,
        SettingsPageId::Switcher,
        SettingsPageId::Clipboard,
    ] {
        let view = view_for(page, &input);
        let layout_input = LayoutInput::for_test(620.0);
        let metrics = PageMetrics::for_layout(layout_input.layout);
        let layout = layout_rows(&view, &layout_input, &metrics);
        for card in &layout.cards {
            let has_described = view.rows.iter().any(|row| row.card == card.index);
            if has_described {
                continue;
            }
            assert!(
                card.section.starts_with("settings."),
                "{page:?} card {} names {:?} instead of a copy key",
                card.index,
                card.section
            );
            assert!(
                t(card.section) != card.section,
                "{page:?} card {} has no translation for {}",
                card.index,
                card.section
            );
        }
    }
}

/// A captioned row's text block is the width the inline builder gave it.
///
/// The inline builder used two expressions: the default one that stops at the control column,
/// and one wide block for the General page's menu-bar switch. Drawing every captioned row at one
/// width wrapped the clipboard page's three-line warnings onto two lines and pushed the
/// menu-bar promise to three.
#[test]
fn a_captioned_row_states_which_text_block_it_takes() {
    let input = input();
    let narrow = 600.0 - 180.0 - 16.0 - 16.0 - 16.0;
    for page in [SettingsPageId::General, SettingsPageId::Clipboard] {
        let view = view_for(page, &input);
        let layout = SettingsLayout::new(600.0);
        for row in view
            .rows
            .iter()
            .filter(|row| row.copy == RowCopy::Captioned)
        {
            let width = captioned_text_width(layout, 600.0, row);
            if row.wide_caption {
                assert!(
                    width > narrow,
                    "{page:?} {}: a wide block must be wider than the default",
                    row.label_key
                );
            } else {
                assert!(!row.wide_caption);
            }
        }
    }
    // Exactly one row in the whole app takes the wide block.
    let wide: Vec<&str> = [SettingsPageId::General, SettingsPageId::Clipboard]
        .into_iter()
        .flat_map(|page| {
            let view = view_for(page, &input);
            view.rows
                .iter()
                .filter(|row| row.wide_caption)
                .map(|row| row.label_key)
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(wide, vec!["settings.row_hide_menu_bar_icon"]);
}

/// A row's control draws its OWN copy, not the row's label.
///
/// The inline builder passed the URL as the link's text and "Open" as the guide button's
/// title. Drawing the row's label instead put "Website" where the URL belongs and made the
/// guide button repeat its own row label.
#[test]
fn a_rows_control_draws_its_own_copy() {
    let input = input();
    let view = about_page_view(&input);
    let links: Vec<&'static str> = view
        .rows
        .iter()
        .filter_map(|row| match row.control {
            ControlKind::Link { text_key, .. } => Some(text_key),
            _ => None,
        })
        .collect();
    assert_eq!(
        links,
        vec!["settings.website_url", "settings.github_url"],
        "a link draws the URL, not the row label"
    );
    for key in links {
        assert_ne!(t(key), key, "{key} has no translation");
    }
    let buttons: Vec<&'static str> = view
        .rows
        .iter()
        .filter_map(|row| match row.control {
            ControlKind::Button { title_key } => Some(title_key),
            _ => None,
        })
        .collect();
    assert_eq!(buttons, vec!["settings.btn_open"]);
}

/// The About page's links and version sit in the page's own VALUE column.
///
/// The inline builder put them at `label_x + 145` and let them run to the card's label inset,
/// which is what fits a full URL; the shared control column is 180pt wide and truncated it.
#[test]
fn about_draws_its_value_column() {
    let input = input();
    let view = about_page_view(&input);
    let layout = SettingsLayout::new(620.0);
    let metrics = PageMetrics::for_layout(layout);
    let layout_input = LayoutInput::for_test(620.0);
    let rows = layout_rows(&view, &layout_input, &metrics);
    let value_x = layout.label_x + 145.0;
    let mut checked = 0;
    for (index, row) in view.rows.iter().enumerate() {
        if !row.value_column {
            continue;
        }
        checked += 1;
        assert!(
            (rows.rows[index].control_frame.x - value_x).abs() < 0.01,
            "About row {index} draws at x={}, not the value column's {value_x}",
            rows.rows[index].control_frame.x
        );
        let want_w = 620.0 - layout.label_x - value_x;
        assert!(
            (rows.rows[index].control_frame.w - want_w).abs() < 0.01,
            "About row {index} is {} wide, not {want_w}",
            rows.rows[index].control_frame.w
        );
    }
    assert!(
        checked >= 3,
        "the links and the version row take the value column"
    );
}

/// Every action button draws the copy its action names, not the row's label.
///
/// The inline builder passed the button's own string (`Export`, `Import`, `Open`). Drawing the
/// row label instead made the General page's three buttons all read "Open" and the About
/// page's guide button repeat "View guide".
#[test]
fn an_action_button_draws_its_own_copy() {
    let input = input();
    // The Clipboard page's restore button is drawn by its hook, not described, so it is not
    // in this list: a described copy of it drew the action twice.
    let expected: [(&str, &str); 4] = [
        ("settings.row_export_logs", "settings.btn_export_logs"),
        (
            "settings.row_export_settings",
            "settings.btn_export_settings",
        ),
        (
            "settings.row_import_settings",
            "settings.btn_import_settings",
        ),
        ("settings.row_view_guide", "settings.btn_open"),
    ];
    let mut seen: Vec<(&str, &str)> = Vec::new();
    for page in [
        SettingsPageId::General,
        SettingsPageId::About,
        SettingsPageId::Clipboard,
    ] {
        let view = view_for(page, &input);
        for row in &view.rows {
            if let ControlKind::Button { title_key } = row.control {
                seen.push((row.label_key, title_key));
                assert_ne!(
                    title_key, row.label_key,
                    "{page:?} button row {} draws its own row label",
                    row.label_key
                );
                assert_ne!(t(title_key), title_key, "{title_key} has no translation");
            }
        }
    }
    assert_eq!(seen, expected);
}

/// A page's description reads only its `PageInput`, and the input is captured from ONE config read.
///
/// Two regressions this pins, both found by an external review:
///
/// - `PageInput::capture` used to call `CONFIG.read()` twice inside one argument list. The first
///   guard lives until the end of the statement, so the second read was a NESTED read of a
///   `std::sync::RwLock` -- which parks when a writer queues between them -- and the outer guard
///   was held across the AppKit query `ThemeInput` makes for the "auto" appearance. A source
///   assertion is the only static form available: the deadlock needs a concurrent writer.
/// - a description that calls into the system (the bundle version, a user default) stops being a
///   function of its input, and the banner's HEIGHT -- a layout input -- depended on it.
#[test]
fn a_description_reads_only_its_input_and_capture_reads_the_config_once() {
    // The production module, one level up: this test lives in `tests/`, whose own `mod.rs` is
    // not the code it scans.
    let source = include_str!("../mod.rs");
    let general = include_str!("../general.rs");
    let capture_start = source
        .find("    pub(super) fn capture() -> Self {")
        .expect("capture exists");
    let capture = &source[capture_start..];
    let capture = &capture[..capture.find("\n    }\n").expect("capture ends")];
    assert_eq!(
        capture.matches("CONFIG.read()").count(),
        1,
        "capture must read the configuration exactly once: {capture}"
    );
    assert!(
        !capture.contains("ThemeInput::current()"),
        "capture must derive the theme from its own snapshot, not read CONFIG again"
    );
    // `capture` is the ONE place allowed to touch process state; every other part of the
    // description layer must be a function of the input it was handed.
    //
    // The description layer USED to be one file. The split moved it into model.rs (the types),
    // rows.rs (the row constructors), update.rs (the update description) and layout/ (the
    // measurement), so the scan follows it there: scanning only `mod.rs` would have covered the
    // facade and nothing else, which is the regression this test exists to prevent. The one file
    // NOT listed is `smoke.rs`, whose runners drive a real window by design.
    let model = include_str!("../model.rs");
    let rows = include_str!("../rows.rs");
    let update = include_str!("../update.rs");
    let layout = include_str!("../layout/mod.rs");
    let place = include_str!("../layout/place.rs");
    let outside_capture = source.replace(capture, "");
    for (name, text) in [
        ("page_view/mod.rs", outside_capture.as_str()),
        ("page_view/model.rs", model),
        ("page_view/rows.rs", rows),
        ("page_view/update.rs", update),
        ("page_view/layout/mod.rs", layout),
        ("page_view/layout/place.rs", place),
        ("general.rs", general),
    ] {
        for forbidden in [
            "needs_permission_migration_copy()",
            "system_dark_mode()",
            "NSUserDefaults",
        ] {
            assert!(
                !text.contains(forbidden),
                "{name} reads {forbidden} outside its PageInput"
            );
        }
    }
}
