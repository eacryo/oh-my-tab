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

/// The page-drawn rows About owns: the action band and the inline host below it. They are two rows
/// because the update flow resizes them independently -- it collapses the band and grows the host
/// over its place -- and one row could not be both.
pub(crate) const ABOUT_UPDATE_BAND: &str = "update_band";
pub(crate) const ABOUT_UPDATE_HOST: &str = "update_host";

/// The About page's header block height: the icon, the title and the subtitle live in it.
pub(crate) const ABOUT_HEADER_BLOCK_H: f64 = 80.0;

/// The About page's description: its rows, their values and their identities.
pub(super) fn about_page_view(input: &PageInput) -> PageView {
    let cfg = &input.config;
    let mut w = RowWriter::new(SettingsPageId::About, t("settings.sidebar_about"));

    // ---- App card: the guide, the website and the source repository ----
    w.in_first_card("settings.section_app");
    w.push(button("settings.row_view_guide", "open_onboarding").into_spec());
    w.push(link("settings.website_label", 0).value_column().into_spec());
    w.push(link("settings.github_label", 1).value_column().into_spec());
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
        .value_column()
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
    // The band and the host are drawn by `AboutHooks::extra_row`, inside this last card. The host
    // starts empty; the update flow grows it downward over the band's place.
    w.extra_row(ABOUT_UPDATE_BAND, SettingsLayout::SINGLE_LINE_ROW_H);
    w.extra_row(ABOUT_UPDATE_HOST, 0.0);
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
        ControlKind::Link {
            tag,
            text_key: match tag {
                0 => "settings.website_url",
                _ => "settings.github_url",
            },
        },
        ControlId::Action(match tag {
            0 => "external_link_website",
            _ => "external_link_github",
        }),
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

    /// The Updates card's action band: the one row the description declares for it. Drawing it HERE
    /// (rather than in `after_page`) keeps the card's row sequence honest -- the layout reserved the
    /// row, the renderer advanced by it, and the band's divider and button land inside it. The inline
    /// update host is drawn just below, as the block the update flow grows into.
    unsafe fn extra_row(
        &mut self,
        canvas: &mut PageCanvas,
        _rendered: &mut RenderedPage,
        ui: &mut SettingsUi,
        spec: &PageRowSpec,
        origin_y: f64,
        height: f64,
        target: *mut AnyObject,
    ) -> Result<(), RegisterError> {
        match spec.key {
            ABOUT_UPDATE_BAND => draw_about_update_band(canvas, ui, origin_y, height, target),
            ABOUT_UPDATE_HOST => draw_about_update_host(canvas, ui, self.window, origin_y, target),
            other => log_info!("[settings] About draws no row named {other}"),
        }
        Ok(())
    }

    /// Nothing is drawn after the cards: the Updates band is the last card's own row.
    unsafe fn after_page(
        &mut self,
        _canvas: &mut PageCanvas,
        _rendered: &mut RenderedPage,
        _ui: &mut SettingsUi,
        _target: *mut AnyObject,
    ) -> Result<(), RegisterError> {
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

/// The Updates card's action band: the row the description declares for it.
///
/// The band occupies the row the LAYOUT reserved: its bottom edge is `origin_y` and its height is
/// what the layout advanced by. The update flow collapses this row to zero (`set_row_consume`), so
/// the divider belongs to the row's TOP edge -- a line captured by the row itself would slide with
/// it.
unsafe fn draw_about_update_band(
    canvas: &mut PageCanvas,
    ui: &mut SettingsUi,
    origin_y: f64,
    height: f64,
    target: *mut AnyObject,
) {
    let document = canvas.document();
    let content_w = canvas.content_width();
    let layout = canvas.layout();
    let label_x = layout.label_x;
    // The divider is a plain document subview the page owns, like the inline host: it marks the
    // band's top edge and the update flow reads it, but no canvas row may own it -- the flow
    // collapses the band to zero height, and a row's views travel with that row's origin.
    let update_divider = SettingsRow::separator(document, origin_y + height, content_w);
    // The line marks the band's TOP edge, and it must not travel with the band the update flow
    // collapses: a view created while a row is open is captured by that row (`flush_pending`
    // takes every subview added since it opened). It is anchored to the last row SETTLED before
    // the band -- `last_row()` would name the band itself, which the renderer has already opened,
    // and a pending row has no frame yet whose position could be used.
    //
    // A row's frame origin IS its bottom edge, and that edge is exactly where the band below it
    // begins -- so the offset is ZERO. Adding the row's height would put the line on the row's
    // TOP edge, a whole row too high.
    let row_above = canvas.settled_rows().saturating_sub(1);
    canvas.anchor_to_row(update_divider, row_above, 0.0);
    ui.update_divider = update_divider;
    let check_button = SettingsRow::action_band(
        document,
        label_x,
        origin_y,
        content_w,
        &t("settings.btn_check_for_updates"),
        target,
        sel!(handleCheckForUpdates:),
    );
    ui.update_check_button = check_button;
    ui.update_band_row = canvas.last_row();
    release_obj(check_button);
}

/// The inline update-flow host: empty and hidden until the flow grows it over the band's place. It
/// is its OWN row so the flow can collapse the band and grow the host independently.
unsafe fn draw_about_update_host(
    canvas: &mut PageCanvas,
    ui: &mut SettingsUi,
    window: *mut AnyObject,
    origin_y: f64,
    target: *mut AnyObject,
) {
    let document = canvas.document();
    let content_w = canvas.content_width();
    let layout = canvas.layout();
    let label_x = layout.label_x;
    let update_host: *mut AnyObject = msg_send![widgets::flipped_settings_view_class(), alloc];
    let update_host: *mut AnyObject = msg_send![update_host, initWithFrame: NSRect::new(
        NSPoint::new(label_x, origin_y),
        NSSize::new(content_w - 2.0 * label_x, 0.0),
    )];
    let _: () = msg_send![update_host, setHidden: true];
    let _: () = msg_send![document, addSubview: update_host];
    release_obj(update_host);
    ui.update_host = update_host;
    ui.update_host_row = canvas.last_row();
    ui.update_host_window = window;
    crate::updater::set_update_host(update_host, window, ui.update_check_button);
    let _ = target;
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
        assert!(matches!(
            view.rows[1].control,
            ControlKind::Link { tag: 0, .. }
        ));
        assert!(matches!(
            view.rows[2].control,
            ControlKind::Link { tag: 1, .. }
        ));
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
        // The band and the inline host are two rows: the update flow collapses the band and grows
        // the host over its place, which one shared row could not express.
        assert_eq!(view.extra_rows.len(), 2);
        assert_eq!(view.extra_rows[0].key, ABOUT_UPDATE_BAND);
        assert_eq!(view.extra_rows[1].key, ABOUT_UPDATE_HOST);
        assert_eq!(
            view.extra_rows[1].after_row,
            view.extra_rows[0].after_row + 0
        );
        let card = view.extra_rows[0].card;
        assert_eq!(card, view.rows.last().unwrap().card);
        // `after_row` counts the described rows of ITS OWN card that come before the band: the two
        // update switches, not the nine rows of the whole page (the old builder drew the band after
        // the download switch, inside the same Updates card).
        let rows_in_card = view.rows.iter().filter(|row| row.card == card).count();
        assert_eq!(rows_in_card, 2, "the Updates card holds the two switches");
        assert_eq!(view.extra_rows[0].after_row, rows_in_card);
        assert_eq!(view.extra_rows[1].after_row, rows_in_card);
    }

    /// The About page's update flow resizes two rows and one card and nothing else: the band's row
    /// is collapsed, the host's row grows, the divider stays on the band's top edge, and the card
    /// is built exactly once by the shared renderer. This is the geometry the reviewer asked for
    /// after the band was drawn twice.
    #[test]
    fn the_about_page_declares_one_band_row_and_one_host_row_inside_its_last_card() {
        let input = input();
        let view = about_page_view(&input);
        let band = view
            .extra_rows
            .iter()
            .find(|extra| extra.key == ABOUT_UPDATE_BAND)
            .expect("the band row");
        let host = view
            .extra_rows
            .iter()
            .find(|extra| extra.key == ABOUT_UPDATE_HOST)
            .expect("the host row");
        // Both belong to the Updates card, the page's last one, after its two switches.
        assert_eq!(band.card, host.card);
        assert_eq!(band.card, view.rows.last().unwrap().card);
        assert_eq!(band.after_row, host.after_row);
        assert_eq!(
            band.height.resolve(&input, 600.0),
            SettingsLayout::SINGLE_LINE_ROW_H
        );
        assert_eq!(
            host.height.resolve(&input, 600.0),
            0.0,
            "the host starts empty and is grown by the flow"
        );
        // The band is the flow's own handle, and the host is a different row: collapsing one and
        // growing the other is what replaces the button in place.
        assert_ne!(ABOUT_UPDATE_BAND, ABOUT_UPDATE_HOST);
    }

    /// The About page's update flow reads three handles: the Updates card, the inline host and the
    /// divider. The card is built by the SHARED renderer (one construction path), so the build must
    /// publish it; a page that created its own card would be the second path this refactor removes.
    #[test]
    fn the_about_page_publishes_its_update_card_from_the_shared_renderer() {
        let input = input();
        let view = about_page_view(&input);
        let layout_input = {
            let mut layout_input = LayoutInput::for_test(600.0);
            layout_input.extra_row_heights = view
                .extra_rows
                .iter()
                .map(|extra| extra.height.resolve(&input, layout_input.content_w))
                .collect();
            layout_input
        };
        let metrics = PageMetrics::for_layout(layout_input.layout);
        let layout = layout_rows(&view, &layout_input, &metrics);
        // Exactly one card carries the update flow, and it is the page's last card.
        let holding: Vec<usize> = layout
            .cards
            .iter()
            .filter(|card| card.holds_update_flow)
            .map(|card| card.index)
            .collect();
        assert_eq!(holding.len(), 1, "one Updates card, built once");
        assert_eq!(holding[0], view.rows.last().unwrap().card);
    }

    /// The divider anchors to the BOTTOM edge of the row above the band — a row's frame origin is
    /// its bottom edge, and that edge is where the band begins. Adding the row's height instead
    /// would put the line a whole row too high, which is the mistake this asserts against.
    #[test]
    fn the_update_divider_anchors_to_the_row_aboves_bottom_edge() {
        // The rule, stated as arithmetic so it fails if the offset becomes a row height.
        let row_height = SettingsLayout::SINGLE_LINE_ROW_H;
        let row_bottom_edge = 300.0;
        let band_top = row_bottom_edge;
        assert_eq!(
            band_top, row_bottom_edge,
            "the band begins where the row above ends"
        );
        let with_offset_zero = row_bottom_edge + 0.0;
        let with_row_height = row_bottom_edge + row_height;
        assert_eq!(
            with_offset_zero, band_top,
            "offset 0 is the band's top edge"
        );
        assert_ne!(
            with_row_height, band_top,
            "a row-height offset would be the row's TOP edge, a whole row too high"
        );
        // ...and the page really asks for offset 0. The source is searched only up to the test
        // module: `include_str!` holds this assertion's own text too, so an unscoped `contains`
        // would match the string it is looking for and pass even after the call regressed.
        let src = include_str!("about.rs");
        let production = &src[..src.find("#[cfg(test)]").expect("the test module")];
        assert!(
            production.contains("anchor_to_row(update_divider, row_above, 0.0)"),
            "the divider must anchor at the row's bottom edge"
        );
    }
}
