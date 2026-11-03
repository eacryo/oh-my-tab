//! One settings page as data: its rows, their values and their identities.
//!
//! A page module is pure: it reads `PageInput`, writes `RowSpec`s through `RowWriter` and returns a
//! `PageView`. It creates no view, reads no global and records no search index -- the shared
//! renderer does all of that from the description.
//!
//! Every row of this page is described: the Keystroke Display page owns no canvas block, so nothing
//! here is built by the page itself.

use super::*;

/// The Keystroke Display page's description: its rows, their values and their identities.
///
/// The master switch keeps a card of its own, so it reads as the feature's on/off row instead of the
/// first row of the options list; the options below it open a second card.
pub(super) fn keystroke_display_page_view(input: &PageInput) -> PageView {
    let display = &input.config.keystroke_display;
    let mut w = RowWriter::new(
        SettingsPageId::KeystrokeDisplay,
        t("settings.sidebar_keystroke_display"),
    );
    w.in_first_card("settings.header_keystroke_display");
    w.push(
        switch(
            "settings.row_keystroke_display_enabled",
            ControlField::KeystrokeDisplayEnabled,
            display.enabled,
        )
        .desc("settings.desc_keystroke_display_enabled")
        .into_spec(),
    );
    w.card("settings.header_keystroke_display_options");
    // The option rows list the config's accepted values in the order the change callback and
    // `KEYSTROKE_DISPLAY_*_VALUES` map them back, so an index means the same thing in every path.
    let modes = vec![
        t("settings.keystroke_display_mode_all"),
        t("settings.keystroke_display_mode_shortcuts"),
        t("settings.keystroke_display_mode_commands"),
    ];
    w.push(
        popup(
            "settings.row_keystroke_display_mode",
            &modes,
            ControlField::KeystrokeDisplayMode,
            match display.mode.as_str() {
                "shortcuts" => 1,
                "commands" => 2,
                _ => 0,
            },
        )
        .into_spec(),
    );
    let tap_levels = vec![
        t("settings.keystroke_display_tap_session"),
        t("settings.keystroke_display_tap_hid"),
    ];
    w.push(
        popup(
            "settings.row_keystroke_display_tap_level",
            &tap_levels,
            ControlField::KeystrokeDisplayTapLevel,
            usize::from(display.tap_level == "hid"),
        )
        .into_spec(),
    );
    let display_positions = vec![
        t("settings.keystroke_display_position_main"),
        t("settings.keystroke_display_position_caret"),
    ];
    // An unrecognised stored value shows the field's own default, which is the choice the reload
    // path falls back on, so the page never claims a mode the picker is not using.
    let display_position = crate::settings::KEYSTROKE_DISPLAY_POSITION_VALUES
        .iter()
        .position(|value| *value == display.display_position)
        .unwrap_or(1);
    w.push(
        popup(
            "settings.row_keystroke_display_position",
            &display_positions,
            ControlField::KeystrokeDisplayPosition,
            display_position,
        )
        .into_spec(),
    );
    let initial_positions = vec![
        t("settings.keystroke_display_initial_position_top"),
        t("settings.keystroke_display_initial_position_bottom"),
        t("settings.keystroke_display_initial_position_left"),
        t("settings.keystroke_display_initial_position_right"),
    ];
    // Index 1 is "bottom", the documented default.
    let initial_position = crate::settings::KEYSTROKE_DISPLAY_INITIAL_POSITION_VALUES
        .iter()
        .position(|value| *value == display.initial_position)
        .unwrap_or(1);
    w.push(
        popup(
            "settings.row_keystroke_display_initial_position",
            &initial_positions,
            ControlField::KeystrokeDisplayInitialPosition,
            initial_position,
        )
        .into_spec(),
    );
    // The panel's stream-axis cap: one value covers both orientations, whole tens only, so the
    // readout, the saved value and the config validation all speak the same steps.
    let max_extent =
        crate::config::keystroke_display_max_extent_percent_snap(display.max_extent_percent);
    w.push(
        slider(
            "settings.row_keystroke_display_max_extent",
            ControlField::KeystrokeDisplayMaxExtent,
            (
                crate::settings::page_builder::max_extent_slider_min(),
                crate::settings::page_builder::max_extent_slider_max(),
                crate::config::KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_STEP as i64,
            ),
            crate::settings::page_builder::max_extent_slider_value(max_extent),
            // Double-click restores the 50% default, the panel's pre-setting behaviour.
            Some(crate::config::KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_DEFAULT as f64),
            crate::settings::page_builder::max_extent_readout(max_extent),
        )
        .into_spec(),
    );
    w.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> PageInput {
        PageInput::from_config(Config::default(), None, crate::theme::ThemeInput::current())
    }

    fn fields(view: &PageView) -> Vec<ControlField> {
        view.rows.iter().filter_map(|row| row.field()).collect()
    }

    fn slot_options(view: &PageView, row: usize) -> Vec<String> {
        view.rows[row].search.options.clone()
    }

    #[test]
    fn keystroke_display_page_view_describes_every_row_with_its_identity_and_copy() {
        let view = keystroke_display_page_view(&input());
        assert_eq!(view.page, SettingsPageId::KeystrokeDisplay);
        assert_eq!(view.rows.len(), 6);
        assert_eq!(view.title_key, "settings.sidebar_keystroke_display");
        // Every row carries the copy key the inline builder used, in the page's own order.
        let labels: Vec<&str> = view.rows.iter().map(|row| row.label_key).collect();
        assert_eq!(
            labels,
            vec![
                "settings.row_keystroke_display_enabled",
                "settings.row_keystroke_display_mode",
                "settings.row_keystroke_display_tap_level",
                "settings.row_keystroke_display_position",
                "settings.row_keystroke_display_initial_position",
                "settings.row_keystroke_display_max_extent",
            ]
        );
        // The identity is the config field, and the order is the page's: a reorder is a visible
        // change, not a detail.
        assert_eq!(
            fields(&view),
            vec![
                ControlField::KeystrokeDisplayEnabled,
                ControlField::KeystrokeDisplayMode,
                ControlField::KeystrokeDisplayTapLevel,
                ControlField::KeystrokeDisplayPosition,
                ControlField::KeystrokeDisplayInitialPosition,
                ControlField::KeystrokeDisplayMaxExtent,
            ]
        );
        // The control kinds are presentation only; the identities above are what the dispatcher uses.
        assert!(matches!(view.rows[0].control, ControlKind::Switch));
        for row in &view.rows[1..5] {
            assert!(matches!(row.control, ControlKind::Popup { .. }), "{row:?}");
        }
        assert!(matches!(view.rows[5].control, ControlKind::Slider { .. }));
        // The master switch's card and the options card, in that order.
        assert_eq!(view.rows[0].section, "settings.header_keystroke_display");
        for row in &view.rows[1..] {
            assert_eq!(row.section, "settings.header_keystroke_display_options");
        }
        // The first row of every card has no divider above it; every later row in the card does.
        assert!(!view.rows[0].separator_above);
        assert!(!view.rows[1].separator_above);
        for row in &view.rows[2..] {
            assert!(row.separator_above);
        }
        // Only the master switch carries a description line; the slider carries a readout instead.
        assert_eq!(
            view.rows[0].desc_key,
            Some("settings.desc_keystroke_display_enabled")
        );
        assert!(view.rows[1..].iter().all(|row| row.desc_key.is_none()));
        // The description is free of duplicate identities, so a build can never be refused here.
        assert_eq!(description_identity_conflict(&view), Ok(()));
    }

    #[test]
    fn keystroke_display_page_view_reads_its_config_values() {
        // The defaults: scope "all", session input, follow-caret, bottom edge, 50% cap.
        let view = keystroke_display_page_view(&input());
        assert_eq!(view.rows[0].value, Value::Switch(false));
        assert_eq!(view.rows[1].value, Value::Index(0));
        assert_eq!(view.rows[2].value, Value::Index(0));
        assert_eq!(view.rows[3].value, Value::Index(1));
        assert_eq!(view.rows[4].value, Value::Index(1));
        assert_eq!(view.rows[5].value, Value::Slider(50));
        assert!(matches!(
            &view.rows[5].control,
            ControlKind::Slider { min: 10, max: 100, step: 10, reset: Some(reset), readout }
                if *reset == 50.0 && readout == "50%"
        ));

        // A saved value the popups and the slider show, one field at a time.
        let mut config = Config::default();
        config.keystroke_display.enabled = true;
        config.keystroke_display.mode = "commands".into();
        config.keystroke_display.tap_level = "hid".into();
        config.keystroke_display.display_position = "main".into();
        config.keystroke_display.initial_position = "left".into();
        config.keystroke_display.max_extent_percent = 80;
        let view = keystroke_display_page_view(&PageInput::from_config(
            config,
            None,
            crate::theme::ThemeInput::current(),
        ));
        assert_eq!(view.rows[0].value, Value::Switch(true));
        assert_eq!(view.rows[1].value, Value::Index(2));
        assert_eq!(view.rows[2].value, Value::Index(1));
        assert_eq!(view.rows[3].value, Value::Index(0));
        assert_eq!(view.rows[4].value, Value::Index(2));
        assert_eq!(view.rows[5].value, Value::Slider(80));
        let readout = match &view.rows[5].control {
            ControlKind::Slider { readout, .. } => readout.clone(),
            other => panic!("the cap row is a slider: {other:?}"),
        };
        assert_eq!(readout, "80%");

        // An unrecognised stored value falls back exactly where the reload path falls back, so the
        // picker never shows an index the saved value does not mean.
        let mut config = Config::default();
        config.keystroke_display.mode = "unknown".into();
        config.keystroke_display.display_position = "unknown".into();
        config.keystroke_display.initial_position = "unknown".into();
        let view = keystroke_display_page_view(&PageInput::from_config(
            config,
            None,
            crate::theme::ThemeInput::current(),
        ));
        assert_eq!(view.rows[1].value, Value::Index(0));
        assert_eq!(view.rows[3].value, Value::Index(1));
        assert_eq!(view.rows[4].value, Value::Index(1));
    }

    #[test]
    fn keystroke_display_search_entries_are_values_not_side_effects() {
        let _guard = sidebar::lock_search_index_for_test();
        let page = settings_page_index(SettingsPageId::KeystrokeDisplay);
        sidebar::begin_settings_search_index();
        let view = keystroke_display_page_view(&input());
        // Producing the description never writes to the search index.
        assert!(sidebar::recorded_search_rows(page).is_empty());
        assert_eq!(
            view.rows[0].search.label,
            t("settings.row_keystroke_display_enabled")
        );
        assert_eq!(
            view.rows[0].search.desc.as_deref(),
            Some(t("settings.desc_keystroke_display_enabled").as_str())
        );
        assert_eq!(
            view.rows[0].search.section,
            t("settings.header_keystroke_display")
        );
        assert_eq!(
            view.rows[1].search.section,
            t("settings.header_keystroke_display_options")
        );
        // A dropdown's choices are part of what the search matches.
        assert_eq!(slot_options(&view, 1).len(), 3);
        assert_eq!(slot_options(&view, 2).len(), 2);
        assert_eq!(slot_options(&view, 3).len(), 2);
        assert_eq!(slot_options(&view, 4).len(), 4);
        assert!(slot_options(&view, 5).is_empty());
        // User-facing text, never a raw translation key.
        for row in &view.rows {
            assert!(!row.search.label.contains("settings."), "{row:?}");
            for option in &row.search.options {
                assert!(!option.contains("settings."), "{row:?}");
            }
        }
    }

    #[test]
    fn keystroke_display_update_description_matches_each_row_initial_value_item_by_item() {
        let mut config = Config::default();
        config.keystroke_display.enabled = true;
        config.keystroke_display.mode = "shortcuts".into();
        config.keystroke_display.max_extent_percent = 70;
        let input = PageInput::from_config(config, None, crate::theme::ThemeInput::current());
        let view = keystroke_display_page_view(&input);
        let updates = page_update_description(&input, &[SettingsPageId::KeystrokeDisplay]);
        assert_eq!(
            updates
                .iter()
                .map(|update| update.field)
                .collect::<Vec<_>>(),
            fields(&view),
            "one entry per config field, in row order"
        );
        for (row, update) in view.rows.iter().zip(updates.iter()) {
            assert_eq!(update.value, row.value.describe(), "{update:?}");
        }
        // The cap slider's domain is whole percent stops and its readout is the same number plus
        // the unit, so value and display happen to agree here -- unlike the status-bar slider.
        let cap = updates
            .iter()
            .find(|update| update.field == ControlField::KeystrokeDisplayMaxExtent)
            .expect("the cap row");
        assert_eq!(cap.value, "70");
        assert_eq!(cap.display, "70%");
        // The switch and the popups describe what the control takes.
        let enabled = updates
            .iter()
            .find(|update| update.field == ControlField::KeystrokeDisplayEnabled)
            .unwrap();
        assert_eq!(enabled.value, "on");
        let mode = updates
            .iter()
            .find(|update| update.field == ControlField::KeystrokeDisplayMode)
            .unwrap();
        assert_eq!(mode.value, "1");
    }

    #[test]
    fn keystroke_display_layout_places_every_row_and_the_slider_readout() {
        let view = keystroke_display_page_view(&input());
        let layout_input = LayoutInput::for_test(600.0);
        let metrics = PageMetrics::for_layout(layout_input.layout);
        let layout = layout_rows(&view, &layout_input, &metrics);
        assert_eq!(layout.rows.len(), view.rows.len());
        // Two cards: the master switch's and the options'.
        assert_eq!(layout.cards.len(), 2);
        assert_eq!(layout.cards[0].section, "settings.header_keystroke_display");
        assert_eq!(
            layout.cards[1].section,
            "settings.header_keystroke_display_options"
        );
        // Every row is one single-line band, contiguous because the design's row gap is 0, and its
        // control stays inside the band.
        for (row, placed) in view.rows.iter().zip(layout.rows.iter()) {
            assert_eq!(placed.frame.h, SettingsLayout::SINGLE_LINE_ROW_H, "{row:?}");
            let control = placed.control_frame;
            assert!(control.y >= placed.frame.y - 1e-9, "{row:?}");
            assert!(
                control.y + control.h <= placed.frame.y + placed.frame.h + 1e-9,
                "{row:?}"
            );
        }
        // Inside one card the rows are contiguous (the design's row gap is 0); the card boundary
        // steps a whole section down, which is the heading's room.
        for (index, pair) in layout.rows.windows(2).enumerate() {
            let same_card = view.rows[index].card == view.rows[index + 1].card;
            let gap = pair[0].frame.y - (pair[1].frame.y + pair[1].frame.h);
            if same_card {
                assert!(
                    gap.abs() < 1e-9,
                    "row {} is not contiguous: {gap}",
                    index + 1
                );
            } else {
                assert!(gap > 0.0, "the card boundary steps forward: {gap}");
            }
        }
        // The slider is narrower than the control column (its readout takes the rest) and the
        // readout sits to its right, inside the row band.
        let capacity = &layout.rows[5];
        assert!(matches!(view.rows[5].control, ControlKind::Slider { .. }));
        assert!(
            (capacity.control_frame.w
                - (layout_input.layout.control_w - SLIDER_READOUT_W - SLIDER_READOUT_GAP))
                .abs()
                < 1e-9
        );
        let readout = capacity.readout_frame.expect("the cap row has a readout");
        assert!(readout.x >= capacity.control_frame.x + capacity.control_frame.w);
        assert!(
            readout.x + readout.w
                <= layout_input.layout.control_x + layout_input.layout.control_w + 1e-9
        );
        assert!(readout.y >= capacity.frame.y - 1e-9);
        assert!(readout.y + readout.h <= capacity.frame.y + capacity.frame.h + 1e-9);
        // No other row has a readout.
        assert!(layout.rows[..5]
            .iter()
            .all(|row| row.readout_frame.is_none()));
    }

    #[test]
    fn every_keystroke_display_field_has_a_settings_ui_slot() {
        // After the inline builder is deleted, the only thing that fills `ui.keystroke_display_*`
        // is the binding table at publish. A field without a slot would leave the dispatcher
        // reading NULL and writing the switch's "off" state back into the config.
        let view = keystroke_display_page_view(&input());
        for row in view.rows.iter().filter(|row| row.field().is_some()) {
            let field = row.field().unwrap();
            assert!(
                crate::settings::page_view::settings_ui_slot_field(field).is_some(),
                "{field:?} has a row but no SettingsUi slot, so its consumer reads NULL"
            );
        }
        // The cap slider's readout is a second pointer field the change callback writes.
        assert!(crate::settings::page_view::settings_ui_readout_field(
            ControlField::KeystrokeDisplayMaxExtent
        ));
    }

    #[test]
    fn a_keystroke_display_field_change_makes_the_in_place_sync_needed() {
        // The trigger the checklist calls step 13 and the description step 14, asserted together:
        // an external change must make an open window re-read, and the re-read must carry the new
        // value (otherwise the page shows the old one after a config-file edit).
        let old = Config::default();
        let mut new = Config::default();
        new.keystroke_display.max_extent_percent = 90;
        assert!(
            crate::runtime_config::change_needs_in_place_control_sync(&old, &new),
            "changing a Keystroke Display field must re-read the open settings window"
        );
        let input = PageInput::from_config(new, None, crate::theme::ThemeInput::current());
        let update = page_update_description(&input, &[SettingsPageId::KeystrokeDisplay])
            .into_iter()
            .find(|update| update.field == ControlField::KeystrokeDisplayMaxExtent)
            .expect("the changed field is in the description");
        assert_eq!(update.value, "90");
        assert_eq!(update.display, "90%");
    }
}
