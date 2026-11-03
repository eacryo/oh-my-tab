//! One settings page as data: its rows, their values and their identities.
//!
//! A page module is pure: it reads `PageInput`, writes `RowSpec`s through `RowWriter` and returns a
//! `PageView`. It creates no view, reads no global and records no search index -- the shared
//! renderer does all of that from the description.
//!
//! About is the one page with furniture of its own: it draws its header (app icon, title, version
//! subtitle and the build-version hit view) and its Updates card ends in an action band and an
//! inline update host. The header is [`PageHooks::before_page`], the band and the host are
//! [`PageHooks::after_page`]; every row between them comes from this description.

use super::*;

/// The page-drawn rows and blocks About owns.
pub(crate) const ABOUT_UPDATE_BAND: &str = "update_band";

/// The About page's header block height: the icon, the title and the subtitle live in it.
pub(crate) const ABOUT_HEADER_BLOCK_H: f64 = 80.0;

/// The About page's description: its rows, their values and their identities.
pub(super) fn about_page_view(input: &PageInput) -> PageView {
    let cfg = &input.config;
    let mut w = RowWriter::new(SettingsPageId::About, t("settings.sidebar_about"));

    // ---- App card: the guide, the website and the source repository ----
    w.in_first_card("settings.section_app");
    w.push(button("settings.row_view_guide", "open_onboarding").into_spec());
    w.push(link("settings.website_label", 0).into_spec());
    w.push(link("settings.github_label", 1).into_spec());
    w.push(
        RowDraft::new(
            "settings.version_label_short",
            ControlKind::Value {
                align: ValueAlign::Natural,
                action: None,
            },
            ControlId::Action("about_version"),
            Value::Dynamic(env!("CARGO_PKG_VERSION").to_string()),
        )
        .into_spec(),
    );

    // ---- Permissions: a status line and the action that opens the matching pane ----
    w.card("settings.section_permissions");
    w.push(
        RowDraft::new(
            "settings.permission_accessibility_label",
            ControlKind::Value {
                align: ValueAlign::Right,
                action: Some(ValueAction::new(
                    "open_accessibility_permission",
                    "settings.btn_open_permission_settings",
                )),
            },
            ControlId::Action("open_accessibility_permission"),
            Value::Dynamic(t("settings.permission_status_missing")),
        )
        .into_spec(),
    );
    w.push(
        RowDraft::new(
            "settings.permission_screen_recording_label",
            ControlKind::Value {
                align: ValueAlign::Right,
                action: Some(ValueAction::new(
                    "open_screen_recording_permission",
                    "settings.btn_open_permission_settings",
                )),
            },
            ControlId::Action("open_screen_recording_permission"),
            Value::Dynamic(t("settings.permission_status_missing")),
        )
        .into_spec(),
    );
    w.push(
        RowDraft::new(
            "settings.permission_keychain_label",
            ControlKind::Value {
                align: ValueAlign::Right,
                action: Some(ValueAction::new(
                    "retry_keychain_access",
                    "settings.btn_grant_keychain_access",
                )),
            },
            ControlId::Action("retry_keychain_access"),
            Value::Dynamic(t("settings.keychain_status_missing")),
        )
        .into_spec(),
    );

    // ---- Updates: two switches, then the page's action band and inline host ----
    w.card("settings.section_updates");
    w.push(
        switch(
            "settings.row_update_auto_check",
            ControlField::UpdateAutoCheck,
            cfg.updates.automatically_check,
        )
        .desc("settings.desc_update_auto_check")
        .into_spec(),
    );
    w.push(
        switch(
            "settings.row_update_auto_download",
            ControlField::UpdateAutoDownload,
            cfg.updates.automatically_download,
        )
        .desc("settings.desc_update_auto_download")
        .into_spec(),
    );
    // The band and the host are drawn by `AboutHooks::after_page`, inside this last card.
    w.extra_row(ABOUT_UPDATE_BAND, SettingsLayout::SINGLE_LINE_ROW_H);
    let mut view = w.finish();
    // The page draws its own header instead of the shared page title, so the rows start below it:
    // the header's top padding, its 80pt block and one section step.
    view.header_offset = Some(
        SettingsPageHeader::TOP_PADDING
            + ABOUT_HEADER_BLOCK_H
            + SettingsLayout::new(0.0).section_step,
    );
    view
}

/// A link row: an external link in the control column, selected by its own tag.
fn link(label_key: &'static str, tag: isize) -> RowDraft {
    RowDraft::new(
        label_key,
        ControlKind::Link { tag },
        ControlId::Action("external_link"),
        Value::None,
    )
}

/// The page hooks About owns: its header block above the rows and its update band plus inline host
/// inside the last card.
pub(crate) struct AboutHooks {
    /// The settings window the inline update flow drives.
    pub(crate) window: *mut AnyObject,
}

unsafe impl Send for AboutHooks {}

impl PageHooks for AboutHooks {
    /// The About header: the app icon, the product title, the version subtitle and the transparent
    /// hit view for the build-version easter egg -- exactly the block the inline builder drew, pinned
    /// to the document's top edge so it travels with the document height.
    unsafe fn before_page(
        &mut self,
        canvas: &mut PageCanvas,
        ui: &mut SettingsUi,
        _target: *mut AnyObject,
    ) -> Result<(), RegisterError> {
        draw_about_header(canvas, ui);
        Ok(())
    }

    /// The Updates card's action band and the inline update host below it. Both are blocks rather
    /// than rows: the band's control spans the card and its title is flow state, and the host is the
    /// region the update flow grows into.
    unsafe fn after_page(
        &mut self,
        canvas: &mut PageCanvas,
        _rendered: &mut RenderedPage,
        ui: &mut SettingsUi,
        target: *mut AnyObject,
    ) -> Result<(), RegisterError> {
        draw_about_update_blocks(canvas, ui, self.window, target);
        Ok(())
    }
}
/// The About header block, drawn inside the page document and pinned to its top edge.
unsafe fn draw_about_header(canvas: &mut PageCanvas, ui: &mut SettingsUi) {
    let document = canvas.document();
    let content_w = canvas.content_width();
    let layout = canvas.layout();
    // The header hangs from the document's top edge, which the canvas measured.
    let doc_top: NSRect = msg_send![document, frame];
    let header_top = doc_top.size.height;
    let header_mark = canvas.view_mark();
    const ABOUT_APP_ICON_TEXT_INSET: f64 = 73.0;
    // Offset the icon slot by its render overflow so the visible image aligns with the cards below.
    let slot_x = widgets::ABOUT_HEADER_CONTENT_LEADING_X + widgets::ABOUT_APP_ICON_RENDER_OVERFLOW;
    let text_x = slot_x + ABOUT_APP_ICON_TEXT_INSET;
    widgets::add_about_app_icon(document, slot_x, header_top - 58.0);
    let title: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let title: *mut AnyObject = msg_send![title, initWithFrame: NSRect::new(
        NSPoint::new(text_x, header_top - 33.0),
        NSSize::new(content_w - text_x, 32.0),
    )];
    let _: () = msg_send![title, setBezeled: false];
    let _: () = msg_send![title, setDrawsBackground: false];
    let _: () = msg_send![title, setEditable: false];
    widgets::set_page_title_text(title, "Oh My Tab");
    let _: () = msg_send![document, addSubview: title];
    release_obj(title);

    let subtitle: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let subtitle: *mut AnyObject = msg_send![subtitle, initWithFrame: NSRect::new(
        NSPoint::new(text_x, header_top - 53.0),
        NSSize::new(content_w - text_x, 18.0),
    )];
    set_field(
        subtitle,
        tf(
            "settings.version_label",
            &[("version", env!("CARGO_PKG_VERSION"))],
        ),
    );
    let _: () = msg_send![subtitle, setBezeled: false];
    let _: () = msg_send![subtitle, setDrawsBackground: false];
    let _: () = msg_send![subtitle, setEditable: false];
    let subtitle_font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CAPTION];
    let _: () = msg_send![subtitle, setFont: subtitle_font];
    let subtitle_color = settings_text_color(SettingsTextRole::Muted);
    let _: () = msg_send![subtitle, setTextColor: subtitle_color];
    let _: () = msg_send![document, addSubview: subtitle];
    release_obj(subtitle);
    ui.about_subtitle = subtitle;

    // Transparent hit area for the five-click build-version easter egg: added after the labels so it
    // receives clicks across the whole header without changing its visuals.
    let hit: *mut AnyObject = msg_send![widgets::about_header_click_view_class(), alloc];
    let hit: *mut AnyObject = msg_send![hit, initWithFrame: NSRect::new(
        NSPoint::new(0.0, header_top - 64.0),
        NSSize::new(content_w, 64.0),
    )];
    let _: () = msg_send![document, addSubview: hit];
    release_obj(hit);
    canvas.pin_from_mark(header_mark);
    let _ = layout;
}

/// The Updates card's action band and inline update host, drawn on the page canvas.
unsafe fn draw_about_update_blocks(
    canvas: &mut PageCanvas,
    ui: &mut SettingsUi,
    window: *mut AnyObject,
    target: *mut AnyObject,
) {
    let document = canvas.document();
    let content_w = canvas.content_width();
    let layout = canvas.layout();
    let label_x = layout.label_x;
    // The divider that starts the card's action band. Deliberately drawn while the download row is
    // still the row being captured: the update flow collapses the band to zero height, and a row's
    // views travel with its origin, so a line captured by the band would slide up a whole row height
    // the moment a flow starts.
    // The divider that starts the card's action band sits on the download row's top edge, exactly as
    // the inline builder drew it: that row's frame is the canvas's record of where it ended up.
    let download_row_y = canvas
        .row_frame(canvas.last_row())
        .map(|frame| frame.origin.y + frame.size.height)
        .unwrap_or(0.0);
    let update_divider = SettingsRow::separator(document, download_row_y, content_w);
    ui.update_divider = update_divider;
    // Check for updates: a full-width action band on the shared row rhythm (56pt band, 32pt control,
    // 12pt above and below). The inline update flow replaces it in place by collapsing this block
    // and growing the panel below.
    let band_y = canvas.next_block(SettingsLayout::SINGLE_LINE_ROW_H);
    let check_button = SettingsRow::action_band(
        document,
        label_x,
        band_y,
        content_w,
        &t("settings.btn_check_for_updates"),
        target,
        sel!(handleCheckForUpdates:),
    );
    ui.update_check_button = check_button;
    ui.update_band_row = canvas.last_row();
    release_obj(check_button);
    // Inline update-flow host: empty and hidden by default, grown downward by the flow so the panel
    // takes over the check button's position.
    let compact_host_h = 0.0;
    let host_top_y = canvas.next_block(compact_host_h);
    ui.update_host_row = canvas.last_row();
    let update_host: *mut AnyObject = msg_send![widgets::flipped_settings_view_class(), alloc];
    let update_host: *mut AnyObject = msg_send![update_host, initWithFrame: NSRect::new(
        NSPoint::new(label_x, host_top_y),
        NSSize::new(content_w - 2.0 * label_x, compact_host_h),
    )];
    let _: () = msg_send![update_host, setHidden: true];
    let _: () = msg_send![document, addSubview: update_host];
    release_obj(update_host);
    ui.update_host = update_host;
    ui.update_host_window = window;
    crate::updater::set_update_host(update_host, window, check_button);
    ui.update_card = canvas.card(&t("settings.section_updates")).card;
    set_settings_search_section(&t("settings.section_updates"));
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
    fn about_page_view_describes_every_row_with_its_identity() {
        let view = about_page_view(&input());
        assert_eq!(view.page, SettingsPageId::About);
        assert_eq!(view.rows.len(), 9);
        let labels: Vec<&str> = view.rows.iter().map(|row| row.label_key).collect();
        assert_eq!(
            labels,
            vec![
                "settings.row_view_guide",
                "settings.website_label",
                "settings.github_label",
                "settings.version_label_short",
                "settings.permission_accessibility_label",
                "settings.permission_screen_recording_label",
                "settings.permission_keychain_label",
                "settings.row_update_auto_check",
                "settings.row_update_auto_download",
            ]
        );
        assert_eq!(
            view.rows
                .iter()
                .filter_map(|row| row.field())
                .collect::<Vec<_>>(),
            vec![
                ControlField::UpdateAutoCheck,
                ControlField::UpdateAutoDownload
            ]
        );
    }

    #[test]
    fn about_permission_rows_are_value_rows_with_their_own_action() {
        let view = about_page_view(&input());
        let expected = [
            (
                "open_accessibility_permission",
                "settings.btn_open_permission_settings",
            ),
            (
                "open_screen_recording_permission",
                "settings.btn_open_permission_settings",
            ),
            (
                "retry_keychain_access",
                "settings.btn_grant_keychain_access",
            ),
        ];
        for (index, (action, button_key)) in expected.into_iter().enumerate() {
            let row = &view.rows[4 + index];
            match &row.control {
                ControlKind::Value {
                    action: Some(value_action),
                    ..
                } => {
                    assert_eq!(value_action.action, action);
                    assert_eq!(value_action.button_key, button_key);
                }
                other => panic!("permission row {index} is not a value-with-action: {other:?}"),
            }
            assert!(matches!(row.value, Value::Dynamic(_)));
        }
        match &view.rows[3].control {
            ControlKind::Value {
                align: ValueAlign::Natural,
                action: None,
            } => {}
            other => panic!("version row is not a plain value: {other:?}"),
        }
    }

    #[test]
    fn about_links_keep_their_old_tags() {
        let view = about_page_view(&input());
        assert_eq!(view.rows[1].control, ControlKind::Link { tag: 0 });
        assert_eq!(view.rows[2].control, ControlKind::Link { tag: 1 });
    }

    #[test]
    fn about_states_where_its_own_header_ends() {
        let view = about_page_view(&input());
        let step = SettingsLayout::new(600.0).section_step;
        assert_eq!(
            view.header_offset,
            Some(SettingsPageHeader::TOP_PADDING + ABOUT_HEADER_BLOCK_H + step)
        );
    }

    #[test]
    fn about_update_switches_carry_their_config_values() {
        let mut config = crate::config::Config::default();
        config.updates.automatically_check = false;
        config.updates.automatically_download = true;
        let input = PageInput::from_config(config, None, crate::theme::ThemeInput::current());
        let view = about_page_view(&input);
        let value = |field: ControlField| {
            view.rows
                .iter()
                .find(|row| row.field() == Some(field))
                .map(|row| row.value.clone())
                .expect("the switch row")
        };
        assert_eq!(value(ControlField::UpdateAutoCheck), Value::Switch(false));
        assert_eq!(value(ControlField::UpdateAutoDownload), Value::Switch(true));
        let updates = page_update_description(&input, &[SettingsPageId::About]);
        assert_eq!(
            updates
                .iter()
                .map(|update| (update.field, update.value.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (ControlField::UpdateAutoCheck, "off"),
                (ControlField::UpdateAutoDownload, "on"),
            ]
        );
    }

    #[test]
    fn about_declares_its_own_update_band_row() {
        let view = about_page_view(&input());
        assert_eq!(view.extra_rows.len(), 1);
        assert_eq!(view.extra_rows[0].key, ABOUT_UPDATE_BAND);
        let card = view.extra_rows[0].card;
        assert_eq!(card, view.rows.last().unwrap().card);
        // `after_row` counts the described rows of ITS OWN card that come before the band: the two
        // update switches, not the nine rows of the whole page (the old builder drew the band after
        // the download switch, inside the same Updates card).
        let rows_in_card = view.rows.iter().filter(|row| row.card == card).count();
        assert_eq!(rows_in_card, 2, "the Updates card holds the two switches");
        assert_eq!(view.extra_rows[0].after_row, rows_in_card);
    }
}
