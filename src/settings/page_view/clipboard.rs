//! One settings page as data: its rows, their values and their identities.
//!
//! A page module is pure: it reads \`PageInput\`, writes \`RowSpec\`s through \`RowWriter\` and
//! returns a \`PageView\`. It creates no view, reads no global and records no search index --
//! the shared renderer does all of that from the description.
//!
//! The **excluded-applications list** is deliberately NOT part of the description. It is a
//! variable list whose remove buttons have no \`ControlField\`: the handler finds the entry by
//! comparing the SENDER POINTER against \`ui.clipboard_excluded_remove\`, and its add popup's
//! candidates are captured once per build from the running apps. Expressing those rows as
//! \`RowSpec\`s would mean a new action identity per entry (the model carries only static action
//! names), a control kind for a popup whose choice is an action, and a renderer that owns the
//! candidate list -- a new feature of the shared machinery, not a migration. So the page builds
//! that card itself, and every static row around it comes from this description.
//!
//! Two more rows stay page-built for the same reason: the shortcut recorder (a button whose title
//! is the recorded chord, with a capture state machine and an inline error label underneath) and the
//! numeric "maximum entries" field (the model's controls are switches, popups, sliders, values and
//! buttons; a numeric text field is not one of them).

use super::*;

/// The Clipboard page's static rows: its rows, their values and their identities.
///
/// The page's cards and rows are declared here; \`clipboard_dynamic_rows\` describes which of them
/// the page builds itself (see the module doc), and \`build_clipboard_page\` sequences the two.
pub(super) fn clipboard_page_view(input: &PageInput) -> PageView {
    let cfg = &input.config;
    let clipboard = &cfg.clipboard;
    let mut w = RowWriter::new(SettingsPageId::Clipboard, t("settings.sidebar_clipboard"));

    // ---- The feature's own card: the master switch and its standing pause ----
    w.in_first_card("settings.header_clipboard");
    w.push(
        switch(
            "settings.row_clipboard_enabled",
            ControlField::ClipboardEnabled,
            clipboard.enabled,
        )
        .caption("settings.desc_clipboard_enabled")
        .into_spec(),
    );
    w.push(
        switch(
            "settings.row_clipboard_paused",
            ControlField::ClipboardPaused,
            clipboard.paused,
        )
        .caption("settings.desc_clipboard_paused")
        .into_spec(),
    );

    // ---- History: the shortcut recorder stays page-built (a button with a capture state), then
    // where the picker opens ----
    w.card("settings.header_clipboard_options");
    let pin_labels = vec![
        t("settings.pin_follow_entry"),
        t("settings.pin_keep_position"),
    ];
    w.push(
        popup(
            "settings.row_clipboard_pin_follow",
            &pin_labels,
            ControlField::ClipboardPinFollow,
            usize::from(!clipboard.pin_follow_selection),
        )
        .into_spec(),
    );
    let placements = vec![
        t("settings.picker_placement_cursor"),
        t("settings.picker_placement_center"),
    ];
    w.push(
        popup(
            "settings.row_clipboard_picker_placement",
            &placements,
            ControlField::ClipboardPickerPlacement,
            crate::settings::dispatch::picker_placement_index(&clipboard.picker_placement) as usize,
        )
        .into_spec(),
    );

    // ---- Retention and behaviour ----
    w.push(
        switch(
            "settings.row_clipboard_clear_on_quit",
            ControlField::ClipboardClearOnQuit,
            clipboard.clear_on_quit,
        )
        .into_spec(),
    );
    w.push(
        switch(
            "settings.row_clipboard_separate_clear_all",
            ControlField::ClipboardSeparateClearAll,
            clipboard.separate_clear_all,
        )
        .into_spec(),
    );
    w.push(
        switch(
            "settings.row_clipboard_show_source_app",
            ControlField::ClipboardShowSourceApp,
            clipboard.show_source_app,
        )
        .into_spec(),
    );
    w.push(
        switch(
            "settings.row_clipboard_move_used_to_top",
            ControlField::ClipboardMoveUsedToTop,
            clipboard.move_used_to_top,
        )
        .into_spec(),
    );
    w.push(
        switch(
            "settings.row_clipboard_delete_after_paste",
            ControlField::ClipboardDeleteAfterPaste,
            clipboard.delete_after_paste,
        )
        .caption("settings.desc_clipboard_delete_after_paste")
        .into_spec(),
    );
    w.push(
        switch(
            "settings.row_clipboard_clear_system_pasteboard_after_paste",
            ControlField::ClipboardClearSystemPasteboardAfterPaste,
            clipboard.clear_system_pasteboard_after_paste,
        )
        .group(RowGroup::ClipboardDeleteChild)
        .into_spec(),
    );
    let auto_expire = (clipboard.auto_expire_days as i64)
        .clamp(CLIPBOARD_AUTO_EXPIRE_MIN, CLIPBOARD_AUTO_EXPIRE_MAX);
    w.push(
        slider(
            "settings.row_clipboard_auto_expire_days",
            ControlField::ClipboardAutoExpireDays,
            (CLIPBOARD_AUTO_EXPIRE_MIN, CLIPBOARD_AUTO_EXPIRE_MAX, 1),
            auto_expire,
            Some(CLIPBOARD_AUTO_EXPIRE_DEFAULT as f64),
            auto_expire.to_string(),
        )
        .into_spec(),
    );
    let image_rows = (clipboard.image_rows as i64).clamp(1, 3);
    w.push(
        slider(
            "settings.row_clipboard_image_rows",
            ControlField::ClipboardImageRows,
            (1, 3, 1),
            image_rows,
            Some(1.0),
            image_rows.to_string(),
        )
        .into_spec(),
    );

    // ---- Excluded applications: the card's list is page-built (see the module doc); its
    // restore action is a static row ----
    w.card("settings.header_clipboard_excluded");
    w.push(
        button(
            "settings.row_clipboard_excluded_restore",
            "restore_excluded",
        )
        .into_spec(),
    );
    w.finish()
}
#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> PageInput {
        PageInput::from_config(
            crate::config::Config::default(),
            None,
            crate::theme::ThemeInput::current(),
        )
    }

    #[test]
    fn clipboard_page_view_describes_every_static_row_with_its_identity() {
        let view = clipboard_page_view(&input());
        assert_eq!(view.page, SettingsPageId::Clipboard);
        let fields: Vec<ControlField> = view.rows.iter().filter_map(|row| row.field()).collect();
        assert_eq!(
            fields,
            vec![
                ControlField::ClipboardEnabled,
                ControlField::ClipboardPaused,
                ControlField::ClipboardPinFollow,
                ControlField::ClipboardPickerPlacement,
                ControlField::ClipboardClearOnQuit,
                ControlField::ClipboardSeparateClearAll,
                ControlField::ClipboardShowSourceApp,
                ControlField::ClipboardMoveUsedToTop,
                ControlField::ClipboardDeleteAfterPaste,
                ControlField::ClipboardClearSystemPasteboardAfterPaste,
                ControlField::ClipboardAutoExpireDays,
                ControlField::ClipboardImageRows,
            ]
        );
        // The switches that carry a drawn caption (the feature warning and the destructive gesture
        // hint), and the one that is a plain switch.
        assert_eq!(view.rows[0].copy, RowCopy::Captioned);
        assert_eq!(view.rows[1].copy, RowCopy::Captioned);
        assert_eq!(view.rows[8].copy, RowCopy::Captioned);
        assert_eq!(view.rows[2].copy, RowCopy::Plain);
        // The numeric entry limit is page-built: it is not a described row at all.
        let ids: Vec<&str> = view.rows.iter().map(|row| row.label_key).collect();
        assert!(!ids.contains(&"settings.row_clipboard_max_entries"));
        // The clear-system-pasteboard row is the child of "delete after paste".
        assert_eq!(
            view.rows[9].group,
            Some(RowGroup::ClipboardDeleteChild),
            "the clear-pasteboard row is the child of delete-after-paste"
        );
        // The action rows carry action identities, never a field.
        let actions: Vec<&str> = view
            .rows
            .iter()
            .filter_map(|row| match row.id {
                ControlId::Action(action) => Some(action),
                ControlId::Field(_) => None,
            })
            .collect();
        assert_eq!(actions, vec!["restore_excluded"]);
        // The cards are the page's own, in order.
        assert_eq!(view.rows[0].section, "settings.header_clipboard");
        assert_eq!(view.rows[2].section, "settings.header_clipboard_options");
        assert_eq!(
            view.rows.last().unwrap().section,
            "settings.header_clipboard_excluded"
        );
    }

    #[test]
    fn clipboard_values_come_from_the_config() {
        let mut config = crate::config::Config::default();
        config.clipboard.paused = true;
        config.clipboard.max_entries = 37;
        config.clipboard.auto_expire_days = 5;
        config.clipboard.pin_follow_selection = false;
        let input = PageInput::from_config(config, None, crate::theme::ThemeInput::current());
        let view = clipboard_page_view(&input);
        let value = |field: ControlField| {
            view.rows
                .iter()
                .find(|row| row.field() == Some(field))
                .map(|row| row.value.clone())
                .expect("the field's row")
        };
        assert_eq!(value(ControlField::ClipboardPaused), Value::Switch(true));
        // The numeric entry limit is not in the description: the page builds it.
        assert!(view
            .rows
            .iter()
            .all(|row| row.field() != Some(ControlField::ClipboardMaxEntries)));
        // The auto-expire readout is the value itself (days, not a stop index).
        assert_eq!(
            value(ControlField::ClipboardAutoExpireDays),
            Value::Slider(5)
        );
        // "Keep current position" is the second item, i.e. index 1.
        assert_eq!(value(ControlField::ClipboardPinFollow), Value::Index(1));
    }

    #[test]
    fn every_clipboard_row_has_a_search_entry() {
        let _guard = crate::settings::sidebar::lock_search_index_for_test();
        let view = clipboard_page_view(&input());
        for row in &view.rows {
            assert!(
                !row.search.label.is_empty(),
                "{} has no search label",
                row.label_key
            );
            assert!(
                !row.search.section.is_empty(),
                "{} has no section",
                row.label_key
            );
        }
    }
}
