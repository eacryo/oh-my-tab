//! One settings page as data: its rows, their values and their identities.
//!
//! A page module is pure: it reads `PageInput`, writes `RowSpec`s through `RowWriter` and returns a
//! `PageView`. It creates no view, reads no global and records no search index -- the shared
//! renderer does all of that from the description.
//!
//! Every row of this page is described, including the two variable blocks (one switch per advertised
//! file type, one per known terminal), so the page owns no canvas block of its own. The one thing it
//! needs a hook for is legacy state: its `SettingsUi` pointers are not in the shared slot table (no
//! other code reads them), so `FinderMenuHooks::after_page` binds them from the controls the
//! renderer recorded, exactly as the inline builder assigned them.

use super::*;

/// The page's extension-status row. It reports System Settings state, which has no config field,
/// so its identity is an action: the row is the label plus the button that opens the pane where
/// the extension is enabled, and nothing about it is persisted.
pub(super) const FINDER_MENU_STATUS_ACTION: &str = "finder_menu_status";

/// The button beside the status row: the action its own button runs.
pub(super) const FINDER_MENU_STATUS_BUTTON_ACTION: &str = "open_finder_extension_settings";

/// The Finder Menu page's description: its rows, their values and their identities.
///
/// The status row opens the "Finder Integration" card, the master switch shares that card, and the
/// four option cards follow in the order the menu model reads them: arrangement, "New File",
/// "Open in Terminal", "Menu Items".
pub(super) fn finder_menu_page_view(input: &PageInput) -> PageView {
    let menu = &input.config.finder_menu;
    let mut w = RowWriter::new(
        SettingsPageId::FinderMenu,
        t("settings.sidebar_finder_menu"),
    );

    // The status row reports whether the extension is elected in System Settings. A Finder Sync
    // extension cannot switch itself on: the app only restores an election the user had enabled
    // *before our own update* replaced the bundle (finder_menu::recovery), so first-time enabling
    // and re-enabling after a deliberate disable stay the user's decision. The row therefore keeps
    // the state read-only and pairs it with the button that opens the pane where the user can.
    w.in_first_card("settings.finder_menu.header_integration");
    w.push(
        RowDraft::new(
            "settings.finder_menu.row_status",
            ControlKind::Value {
                align: ValueAlign::Right,
                action: Some(ValueAction::new(
                    FINDER_MENU_STATUS_BUTTON_ACTION,
                    "settings.finder_menu.open_extension_settings",
                )),
            },
            ControlId::Action(FINDER_MENU_STATUS_ACTION),
            Value::None,
        )
        .desc("settings.finder_menu.desc_status")
        .dynamic(input.finder_menu_status.clone())
        .into_spec(),
    );
    // The master switch: it owns whether any item below reaches Finder's context menu. It shares
    // the "Finder Integration" card with the status row above, so the card bookkeeping gives it
    // the internal divider every multi-row card draws above its second row.
    w.push(
        switch(
            "settings.finder_menu.row_enabled",
            ControlField::FinderMenuEnabled,
            menu.enabled,
        )
        .desc("settings.finder_menu.desc_enabled")
        .into_spec(),
    );

    // Arrangement popup: index order is FINDER_MENU_LAYOUTS, which is also the order the change
    // callback maps an index back through.
    w.card("settings.finder_menu.header_layout");
    let layouts = vec![
        t("settings.finder_menu.layout_flat"),
        t("settings.finder_menu.layout_grouped"),
        t("settings.finder_menu.layout_submenu"),
    ];
    w.push(
        popup(
            "settings.finder_menu.row_layout",
            &layouts,
            ControlField::FinderMenuLayout,
            crate::config::FINDER_MENU_LAYOUTS
                .iter()
                .position(|layout| *layout == menu.layout.as_str())
                .unwrap_or(0),
        )
        .desc("settings.finder_menu.desc_layout")
        .into_spec(),
    );

    // "New File": one switch per extension the menu can create, in FINDER_MENU_FILE_TYPES order.
    // The switch's identity is its catalog index, so the config list is rebuilt in that order
    // rather than in click order.
    w.card("settings.finder_menu.header_new_file");
    for (index, label_key) in NEW_FILE_LABEL_KEYS
        .iter()
        .take(crate::config::FINDER_MENU_FILE_TYPES.len())
        .enumerate()
    {
        w.push(
            switch(
                label_key,
                ControlField::FinderMenuNewFileType(index),
                file_type_checked(menu, index),
            )
            .into_spec(),
        );
    }

    // "Open in Terminal": one switch per known terminal, in finder_menu::terminals() order. The
    // terminal product name is data, not UI chrome, so it is not translated.
    w.card("settings.finder_menu.header_open_in_terminal");
    for (index, spec) in crate::finder_menu::terminals().iter().enumerate() {
        w.push(
            switch(
                spec.name,
                ControlField::FinderMenuTerminal(index),
                terminal_checked(menu, index),
            )
            .into_spec(),
        );
    }

    // "Copy path" and "Copy SHA-256" share the "Menu Items" card.
    w.card("settings.finder_menu.header_menu_items");
    w.push(
        switch(
            "settings.finder_menu.row_copy_path",
            ControlField::FinderMenuCopyPath,
            menu.copy_path,
        )
        .desc("settings.finder_menu.desc_copy_path")
        .into_spec(),
    );
    w.push(
        switch(
            "settings.finder_menu.row_copy_sha256",
            ControlField::FinderMenuCopySha256,
            menu.copy_sha256,
        )
        .desc("settings.finder_menu.desc_copy_sha256")
        .into_spec(),
    );
    w.finish()
}

/// The copy keys of the "New File" rows, in catalog order. The label is copy and `RowSpec::label_key`
/// is a key, not the extension itself -- the extension is data and appears nowhere in the UI.
const NEW_FILE_LABEL_KEYS: [&str; 3] = [
    "settings.finder_menu.new_file_md",
    "settings.finder_menu.new_file_txt",
    "settings.finder_menu.new_file_json",
];

/// Whether the config has the file type at `index`, in catalog order.
///
/// Membership only: the switch's state is "this type is in the list", and the list order is the
/// catalog's (the menu renders the stored vec as its item order).
fn file_type_checked(menu: &crate::config::FinderMenuSection, index: usize) -> bool {
    crate::config::FINDER_MENU_FILE_TYPES
        .get(index)
        .is_some_and(|extension| menu.new_file_types.iter().any(|value| value == extension))
}

/// Whether the config has the terminal at `index`, in `finder_menu::terminals()` order.
fn terminal_checked(menu: &crate::config::FinderMenuSection, index: usize) -> bool {
    crate::finder_menu::terminals()
        .get(index)
        .is_some_and(|spec| {
            menu.terminals
                .iter()
                .any(|bundle_id| bundle_id == spec.bundle_id)
        })
}

/// What the Finder Menu page draws itself: nothing. Its rows are all described; the hook binds the
/// legacy `SettingsUi` pointers the page's callbacks and the app's refresh paths still read, which
/// the shared slot table does not cover.
#[derive(Default)]
pub(crate) struct FinderMenuHooks {}

impl PageHooks for FinderMenuHooks {
    unsafe fn after_page(
        &mut self,
        _canvas: &mut PageCanvas,
        rendered: &mut RenderedPage,
        ui: &mut SettingsUi,
        _target: *mut AnyObject,
    ) -> Result<(), RegisterError> {
        // The page's own consumers, by identity. Every one of them is read by a callback or by
        // `refresh_finder_menu_status_if_visible` / `update_finder_menu_controls_enabled`, and none
        // is in `settings_ui_slot_present`, so a null here would make a page that looks alive but
        // is not wired. A row a later edit removes leaves its pointer null, which its consumers
        // already tolerate.
        let mut new_file_switches = Vec::with_capacity(crate::config::FINDER_MENU_FILE_TYPES.len());
        let mut terminal_switches = Vec::with_capacity(crate::finder_menu::terminals().len());
        for control in &rendered.controls {
            let ControlId::Field(field) = control.id else {
                // The status row's identity is an action: its label is the value the page shows,
                // and the activation refresh writes it by pointer.
                if control.id == ControlId::Action(FINDER_MENU_STATUS_ACTION) {
                    ui.finder_menu_status = control.control;
                }
                continue;
            };
            match field {
                ControlField::FinderMenuEnabled => ui.finder_menu_enabled = control.control,
                ControlField::FinderMenuLayout => ui.finder_menu_layout = control.control,
                ControlField::FinderMenuCopyPath => ui.finder_menu_copy_path = control.control,
                ControlField::FinderMenuCopySha256 => ui.finder_menu_copy_sha256 = control.control,
                ControlField::FinderMenuNewFileType(index) => {
                    let len = new_file_switches.len().max(index + 1);
                    new_file_switches.resize(len, std::ptr::null_mut());
                    new_file_switches[index] = control.control;
                }
                ControlField::FinderMenuTerminal(index) => {
                    let len = terminal_switches.len().max(index + 1);
                    terminal_switches.resize(len, std::ptr::null_mut());
                    terminal_switches[index] = control.control;
                }
                _ => {}
            }
        }
        ui.finder_menu_new_file_switches = new_file_switches;
        ui.finder_menu_terminal_switches = terminal_switches;
        // Settle the master-switch freeze and the per-terminal installed gate now, so the page is
        // already honest before the open-path fill runs.
        update_finder_menu_controls_enabled(ui);
        Ok(())
    }
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

    fn ids(view: &PageView) -> Vec<ControlId> {
        view.rows.iter().map(|row| row.id.clone()).collect()
    }

    fn row_of(view: &PageView, field: ControlField) -> &RowSpec {
        view.rows
            .iter()
            .find(|row| row.field() == Some(field))
            .unwrap_or_else(|| panic!("{field:?} has a row"))
    }

    #[test]
    fn finder_menu_page_view_describes_every_row_with_its_identity_and_copy() {
        let view = finder_menu_page_view(&input());
        assert_eq!(view.page, SettingsPageId::FinderMenu);
        assert_eq!(view.title_key, "settings.sidebar_finder_menu");
        // The page is fixed furniture plus two catalog-sized blocks, in the order the menu model
        // reads them, so a reorder is a visible change, not a detail.
        assert_eq!(
            view.rows.len(),
            5 + crate::config::FINDER_MENU_FILE_TYPES.len() + crate::finder_menu::terminals().len()
        );
        let labels: Vec<&str> = view.rows.iter().map(|row| row.label_key).collect();
        assert_eq!(labels[0], "settings.finder_menu.row_status");
        assert_eq!(labels[1], "settings.finder_menu.row_enabled");
        assert_eq!(labels[2], "settings.finder_menu.row_layout");
        assert_eq!(labels[3], "settings.finder_menu.new_file_md");
        assert_eq!(
            labels[3 + crate::config::FINDER_MENU_FILE_TYPES.len()],
            crate::finder_menu::terminals()[0].name
        );
        // A terminal row's label is the product name (data), so its search label is not a key.
        let terminal_rows = 3 + crate::config::FINDER_MENU_FILE_TYPES.len();
        for (offset, spec) in crate::finder_menu::terminals().iter().enumerate() {
            let row = &view.rows[terminal_rows + offset];
            assert_eq!(row.label_key, spec.name);
            assert_eq!(row.search.label, spec.name);
        }
        assert_eq!(
            labels[view.rows.len() - 2],
            "settings.finder_menu.row_copy_path"
        );
        assert_eq!(
            labels[view.rows.len() - 1],
            "settings.finder_menu.row_copy_sha256"
        );
        // The identity is the field (or the status row's action), and the order is the page's.
        let mut expected: Vec<ControlId> = vec![
            ControlId::Action(FINDER_MENU_STATUS_ACTION),
            ControlId::Field(ControlField::FinderMenuEnabled),
            ControlId::Field(ControlField::FinderMenuLayout),
        ];
        expected.extend(
            (0..crate::config::FINDER_MENU_FILE_TYPES.len())
                .map(ControlField::FinderMenuNewFileType)
                .map(ControlId::Field),
        );
        expected.extend(
            (0..crate::finder_menu::terminals().len())
                .map(ControlField::FinderMenuTerminal)
                .map(ControlId::Field),
        );
        expected.push(ControlId::Field(ControlField::FinderMenuCopyPath));
        expected.push(ControlId::Field(ControlField::FinderMenuCopySha256));
        assert_eq!(ids(&view), expected);
        // The control kinds are presentation only. The status row is a value with its own action
        // button, the layout is a popup, and every switch row is a switch.
        assert!(matches!(
            &view.rows[0].control,
            ControlKind::Value {
                action: Some(action),
                ..
            } if action.action == FINDER_MENU_STATUS_BUTTON_ACTION
                    && action.button_key == "settings.finder_menu.open_extension_settings"
        ));
        assert!(matches!(view.rows[2].control, ControlKind::Popup { .. }));
        for row in view
            .rows
            .iter()
            .skip(1)
            .filter(|row| row.label_key != "settings.finder_menu.row_layout")
        {
            assert!(matches!(row.control, ControlKind::Switch), "{row:?}");
        }
        // The cards are the page's own, in order, and the variable blocks each open one.
        let sections: Vec<&str> = view.rows.iter().map(|row| row.section).collect();
        assert_eq!(sections[0], "settings.finder_menu.header_integration");
        assert_eq!(sections[1], "settings.finder_menu.header_integration");
        assert_eq!(sections[2], "settings.finder_menu.header_layout");
        assert_eq!(sections[3], "settings.finder_menu.header_new_file");
        assert_eq!(
            sections[3 + crate::config::FINDER_MENU_FILE_TYPES.len()],
            "settings.finder_menu.header_open_in_terminal"
        );
        assert_eq!(
            sections[view.rows.len() - 1],
            "settings.finder_menu.header_menu_items"
        );
        // The first row of every card has no divider above it; every later row in the card does.
        assert!(!view.rows[0].separator_above);
        assert!(view.rows[1].separator_above);
        assert!(!view.rows[2].separator_above);
        assert!(!view.rows[3].separator_above);
        assert!(view.rows[4].separator_above);
        assert!(
            !view.rows[3 + crate::config::FINDER_MENU_FILE_TYPES.len()].separator_above,
            "the first terminal row opens its card"
        );
        assert!(!view.rows[view.rows.len() - 2].separator_above);
        assert!(view.rows[view.rows.len() - 1].separator_above);
        // The description declares no duplicate identity, so a build can never be refused here.
        assert_eq!(description_identity_conflict(&view), Ok(()));
    }

    #[test]
    fn finder_menu_page_view_reads_its_config_values() {
        // The default config: menu off, grouped layout, every file type, Terminal only, both copy
        // items on. Membership, in catalog order, is exactly what each switch shows.
        let view = finder_menu_page_view(&input());
        assert_eq!(
            row_of(&view, ControlField::FinderMenuEnabled).value,
            Value::Switch(false)
        );
        assert_eq!(
            row_of(&view, ControlField::FinderMenuLayout).value,
            Value::Index(1),
            "grouped is the default layout"
        );
        for index in 0..crate::config::FINDER_MENU_FILE_TYPES.len() {
            assert_eq!(
                row_of(&view, ControlField::FinderMenuNewFileType(index)).value,
                Value::Switch(true),
                "every advertised file type is in the default config"
            );
        }
        for index in 0..crate::finder_menu::terminals().len() {
            let expected = index == 0;
            assert_eq!(
                row_of(&view, ControlField::FinderMenuTerminal(index)).value,
                Value::Switch(expected),
                "only the first terminal is in the default config"
            );
        }
        assert_eq!(
            row_of(&view, ControlField::FinderMenuCopyPath).value,
            Value::Switch(true)
        );
        assert_eq!(
            row_of(&view, ControlField::FinderMenuCopySha256).value,
            Value::Switch(true)
        );

        // A saved config the page shows one field at a time, including the newest SHA-256 row.
        let mut config = Config::default();
        config.finder_menu.enabled = true;
        config.finder_menu.layout = "submenu".into();
        config.finder_menu.new_file_types = vec!["md".into(), "json".into()];
        config.finder_menu.terminals = vec![crate::config::FINDER_MENU_TERMINALS[2].into()];
        config.finder_menu.copy_path = false;
        config.finder_menu.copy_sha256 = false;
        let view = finder_menu_page_view(&PageInput::from_config(
            config,
            None,
            crate::theme::ThemeInput::current(),
        ));
        assert_eq!(
            row_of(&view, ControlField::FinderMenuEnabled).value,
            Value::Switch(true)
        );
        assert_eq!(
            row_of(&view, ControlField::FinderMenuLayout).value,
            Value::Index(2),
            "submenu is the third layout"
        );
        // The switches are membership, not list order: "md" and "json" are checked, "txt" is not.
        assert_eq!(
            row_of(&view, ControlField::FinderMenuNewFileType(0)).value,
            Value::Switch(true)
        );
        assert_eq!(
            row_of(&view, ControlField::FinderMenuNewFileType(1)).value,
            Value::Switch(false)
        );
        assert_eq!(
            row_of(&view, ControlField::FinderMenuNewFileType(2)).value,
            Value::Switch(true)
        );
        for index in 0..crate::finder_menu::terminals().len() {
            assert_eq!(
                row_of(&view, ControlField::FinderMenuTerminal(index)).value,
                Value::Switch(index == 2),
                "only the stored terminal is checked"
            );
        }
        assert_eq!(
            row_of(&view, ControlField::FinderMenuCopyPath).value,
            Value::Switch(false)
        );
        assert_eq!(
            row_of(&view, ControlField::FinderMenuCopySha256).value,
            Value::Switch(false)
        );
    }

    #[test]
    fn finder_menu_status_row_carries_the_captured_text_and_its_action() {
        // The status text is an input of the description (the extension's election is process
        // state, so the page cannot read it): whatever was captured is what the row shows, and it
        // reaches the search index as the row's displayed text.
        let mut config = Config::default();
        config.finder_menu.enabled = true;
        let mut input = PageInput::from_config(config, None, crate::theme::ThemeInput::current());
        input.finder_menu_status = "Enabled".to_string();
        let view = finder_menu_page_view(&input);
        assert_eq!(
            view.rows[0].id,
            ControlId::Action(FINDER_MENU_STATUS_ACTION)
        );
        assert_eq!(view.rows[0].value, Value::Dynamic("Enabled".to_string()));
        assert_eq!(view.rows[0].value.describe(), "Enabled");
        // The row is not a config field, so IT contributes no update entry: nothing about the
        // extension state is persisted. The master switch next to it does have one, and that is
        // the correct behaviour for a described config field.
        let updates = page_update_description(&input, &[SettingsPageId::FinderMenu]);
        assert!(view.rows[0].field().is_none());
        assert!(
            updates
                .iter()
                .any(|update| update.field == ControlField::FinderMenuEnabled),
            "the master switch is a described config field and must be refilled"
        );
        assert_eq!(
            updates.len(),
            fields(&view).len(),
            "one entry per described config field, and the status row is an action"
        );
    }

    #[test]
    fn finder_menu_update_description_matches_each_row_initial_value_item_by_item() {
        let mut config = Config::default();
        config.finder_menu.enabled = true;
        config.finder_menu.layout = "flat".into();
        config.finder_menu.new_file_types = vec!["txt".into()];
        config.finder_menu.copy_sha256 = false;
        let input = PageInput::from_config(config, None, crate::theme::ThemeInput::current());
        let view = finder_menu_page_view(&input);
        let updates = page_update_description(&input, &[SettingsPageId::FinderMenu]);
        // The description carries one entry per config field, in row order; the status row is an
        // action and produces none.
        assert_eq!(
            updates
                .iter()
                .map(|update| update.field)
                .collect::<Vec<_>>(),
            fields(&view)
        );
        for (row, update) in view
            .rows
            .iter()
            .filter(|row| row.field().is_some())
            .zip(updates.iter())
        {
            assert_eq!(update.value, row.value.describe(), "{update:?}");
            match (&row.control, &row.value) {
                (ControlKind::Switch, Value::Switch(_)) => {
                    assert!(update.value == "on" || update.value == "off");
                }
                (ControlKind::Popup { options }, Value::Index(index)) => {
                    assert!(*index < options.len(), "the selected index is a choice");
                    assert_eq!(update.value, index.to_string());
                }
                other => panic!("a described field row is a switch or a popup: {other:?}"),
            }
        }
        // The values are the config's own: the master switch, the layout index and the newest field.
        let by_field = |field: ControlField| {
            updates
                .iter()
                .find(|update| update.field == field)
                .unwrap_or_else(|| panic!("{field:?} is in the description"))
        };
        assert_eq!(by_field(ControlField::FinderMenuEnabled).value, "on");
        assert_eq!(by_field(ControlField::FinderMenuLayout).value, "0");
        assert_eq!(
            by_field(ControlField::FinderMenuNewFileType(0)).value,
            "off",
            "only txt is stored"
        );
        assert_eq!(by_field(ControlField::FinderMenuNewFileType(1)).value, "on");
        assert_eq!(by_field(ControlField::FinderMenuCopySha256).value, "off");
    }

    #[test]
    fn finder_menu_layout_places_every_row_inside_its_card() {
        let view = finder_menu_page_view(&input());
        let layout_input = LayoutInput::for_test(600.0);
        let metrics = PageMetrics::for_layout(layout_input.layout);
        let layout = layout_rows(&view, &layout_input, &metrics);
        assert_eq!(layout.rows.len(), view.rows.len());
        // Five cards, one per described group, in the page's order.
        let sections: Vec<&str> = layout.cards.iter().map(|card| card.section).collect();
        assert_eq!(
            sections,
            vec![
                "settings.finder_menu.header_integration",
                "settings.finder_menu.header_layout",
                "settings.finder_menu.header_new_file",
                "settings.finder_menu.header_open_in_terminal",
                "settings.finder_menu.header_menu_items",
            ]
        );
        // Every row is one single-line band, and both views the renderer places for it lie inside
        // the band: the copy column and the control (or, for the status row, the value and the
        // action button beside it).
        for (row, placed) in view.rows.iter().zip(layout.rows.iter()) {
            assert_eq!(placed.frame.h, SettingsLayout::SINGLE_LINE_ROW_H, "{row:?}");
            for box_ in row_placement_boxes(row, placed) {
                assert!(box_.y >= placed.frame.y - 1e-9, "{row:?} {box_:?}");
                assert!(
                    box_.y + box_.h <= placed.frame.y + placed.frame.h + 1e-9,
                    "{row:?} {box_:?}"
                );
            }
        }
        // The status row's action button is its own box inside the band, at the control column's
        // right edge; only that row has one.
        let status_action = layout.rows[0]
            .action_frame
            .expect("the status row has an action");
        let right_x = layout_input.layout.control_x + layout_input.layout.control_w;
        assert!((status_action.x + status_action.w - right_x).abs() < 1e-9);
        assert!(status_action.y >= layout.rows[0].frame.y - 1e-9);
        assert!(
            status_action.y + status_action.h
                <= layout.rows[0].frame.y + layout.rows[0].frame.h + 1e-9
        );
        // The value takes the room the button leaves, and stops clear of it.
        let value = layout.rows[0].control_frame;
        assert!(value.x + value.w <= status_action.x + 1e-9);
        assert!(matches!(
            view.rows[0].control,
            ControlKind::Value {
                action: Some(_),
                ..
            }
        ));
        assert!(layout.rows[1..]
            .iter()
            .all(|row| row.action_frame.is_none()));
        // Inside one card the rows are contiguous (the design's row gap is 0); a card boundary
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
    }

    #[test]
    fn finder_menu_search_entries_are_values_not_side_effects() {
        let _guard = sidebar::lock_search_index_for_test();
        let page = settings_page_index(SettingsPageId::FinderMenu);
        sidebar::begin_settings_search_index();
        let view = finder_menu_page_view(&input());
        // Producing the description never writes to the search index.
        assert!(sidebar::recorded_search_rows(page).is_empty());
        // The status row's entry is its copy, and it is not mistaken for a config field.
        assert_eq!(
            view.rows[0].search.label,
            t("settings.finder_menu.row_status")
        );
        assert_eq!(
            view.rows[0].search.desc.as_deref(),
            Some(t("settings.finder_menu.desc_status").as_str())
        );
        assert_eq!(
            view.rows[0].search.section,
            t("settings.finder_menu.header_integration")
        );
        // A terminal row is searched by its product name under its own card, and a file-type row
        // by its copy.
        let terminal_rows = 3 + crate::config::FINDER_MENU_FILE_TYPES.len();
        for (offset, spec) in crate::finder_menu::terminals().iter().enumerate() {
            assert_eq!(view.rows[terminal_rows + offset].search.label, spec.name);
            assert_eq!(
                view.rows[terminal_rows + offset].search.section,
                t("settings.finder_menu.header_open_in_terminal")
            );
        }
        assert_eq!(
            view.rows[3].search.label,
            t("settings.finder_menu.new_file_md")
        );
        // A dropdown's choices are part of what the search matches, and every row has a label.
        assert_eq!(view.rows[2].search.options.len(), 3);
        for row in &view.rows {
            assert!(!row.search.label.is_empty(), "{row:?}");
            assert!(!row.search.section.is_empty(), "{row:?}");
        }
        // User-facing text, never a raw translation key.
        for row in &view.rows {
            assert!(!row.search.label.contains("settings."), "{row:?}");
            for option in &row.search.options {
                assert!(!option.contains("settings."), "{row:?}");
            }
        }
    }

    #[test]
    fn a_finder_menu_field_change_makes_the_in_place_sync_needed() {
        // The trigger the checklist calls step 13 and the description step 14, asserted together:
        // an external change must make an open window re-read, and the re-read must carry the new
        // value (otherwise the page shows the old one after a config-file edit or a menu toggle).
        let old = Config::default();
        let mut new = Config::default();
        new.finder_menu.copy_sha256 = !old.finder_menu.copy_sha256;
        assert!(
            crate::runtime_config::change_needs_in_place_control_sync(&old, &new),
            "changing a Finder Menu field must re-read the open settings window"
        );
        let input = PageInput::from_config(new, None, crate::theme::ThemeInput::current());
        let update = page_update_description(&input, &[SettingsPageId::FinderMenu])
            .into_iter()
            .find(|update| update.field == ControlField::FinderMenuCopySha256)
            .expect("the changed field is in the description");
        assert_eq!(update.value, "off");
    }

    #[test]
    fn every_finder_menu_legacy_pointer_a_consumer_reads_is_bound_by_the_hooks() {
        // The page's legacy `SettingsUi` pointers are NOT in the shared slot table, so the hooks
        // fill them after the build. Every row whose pointer a consumer dereferences must be one
        // the hook assigns: a missing assignment would make the page look alive while the
        // dispatcher, the freeze list or the activation refresh reads NULL.
        let view = finder_menu_page_view(&input());
        let bound: Vec<ControlField> = view
            .rows
            .iter()
            .filter_map(|row| row.field())
            .filter(|field| {
                matches!(
                    field,
                    ControlField::FinderMenuEnabled
                        | ControlField::FinderMenuLayout
                        | ControlField::FinderMenuCopyPath
                        | ControlField::FinderMenuCopySha256
                        | ControlField::FinderMenuNewFileType(_)
                        | ControlField::FinderMenuTerminal(_)
                )
            })
            .collect();
        // Every field this page declares is one the page knows. The scalar fields are now bound by
        // the shared slot table (the legacy consumers read `SettingsUi` directly); the POSITIONAL
        // ones — one control per file type and per terminal — are bound into their Vec fields by
        // the hook, because a one-slot-per-field table cannot hold a list.
        assert_eq!(bound.len(), fields(&view).len());
        for field in &bound {
            let positional = matches!(
                field,
                ControlField::FinderMenuNewFileType(_) | ControlField::FinderMenuTerminal(_)
            );
            if positional {
                assert!(
                    crate::settings::page_view::settings_ui_slot_field(*field).is_none(),
                    "{field:?} is positional and must not be in the one-slot-per-field table"
                );
            } else {
                assert!(
                    crate::settings::page_view::settings_ui_slot_field(*field).is_some(),
                    "{field:?} is described but no SettingsUi slot is bound for it"
                );
            }
        }
        // The status row is an action, and the activation refresh writes it by pointer.
        assert_eq!(
            view.rows[0].id,
            ControlId::Action(FINDER_MENU_STATUS_ACTION)
        );
        assert!(view.rows[0].field().is_none());
    }
}
