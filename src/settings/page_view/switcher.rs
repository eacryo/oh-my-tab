//! One settings page as data: its rows, their values and their identities.
//!
//! A page module is pure: it reads `PageInput`, writes `RowSpec`s through `RowWriter` and
//! returns a `PageView`. It creates no view, reads no global and records no search index --
//! the shared renderer does all of that from the description.

use super::*;
/// The Switcher page's description: its rows, their values and their identities.
pub(super) fn switcher_page_view(input: &PageInput) -> PageView {
    let cfg = &input.config;
    let mut w = RowWriter::new(SettingsPageId::Switcher, t("settings.sidebar_switcher"));
    w.in_first_card("settings.header_windows");
    w.push(
        switch(
            "settings.row_windows_enabled",
            ControlField::WindowsEnabled,
            cfg.windows.enabled,
        )
        .desc("settings.desc_windows_enabled")
        .into_spec(),
    );
    // The overlay switch is its own card; every remaining window setting is the next one. The
    // inline builder split here (\`canvas.card(header_windows)\` then \`next_section()\`), and a
    // description that opens the second card at the end puts every row under the first heading.
    w.card("settings.header_window_options");
    w.push(
        switch(
            "settings.row_show_minimized",
            ControlField::ShowMinimized,
            cfg.windows.show_minimized,
        )
        .into_spec(),
    );
    w.push(
        switch(
            "settings.row_show_hidden_app_windows",
            ControlField::ShowHiddenAppWindows,
            cfg.windows.show_hidden_app_windows,
        )
        .into_spec(),
    );
    w.push(
        switch(
            "settings.row_show_other_desktops",
            ControlField::ShowOtherDesktops,
            cfg.windows.show_other_desktops,
        )
        .desc("settings.desc_show_other_desktops")
        .into_spec(),
    );
    let display_modes = vec![
        t("settings.window_display_mode_icons"),
        t("settings.window_display_mode_icons_thumbnails"),
    ];
    w.push(
        popup(
            "settings.row_window_display_mode",
            &display_modes,
            ControlField::ThumbnailsEnabled,
            usize::from(cfg.layout.thumbnails_enabled),
        )
        .into_spec(),
    );
    w.push(
        switch(
            "settings.row_focused_thumbnail_prewarm",
            ControlField::FocusedThumbnailPrewarm,
            cfg.layout.focused_thumbnail_prewarm,
        )
        .group(RowGroup::ThumbnailOnly)
        .into_spec(),
    );
    w.push(
        switch(
            "settings.row_show_app_name_in_cards",
            ControlField::ShowAppNameInCards,
            cfg.layout.show_app_name_in_cards,
        )
        .group(RowGroup::ThumbnailOnly)
        .into_spec(),
    );
    let thumbnail_sizes: Vec<String> = crate::config::THUMBNAIL_SIZE_VALUES
        .iter()
        .map(|value| match *value {
            "auto" => t("settings.thumbnail_size_auto"),
            percent => tf("settings.thumbnail_size_percent", &[("percent", percent)]),
        })
        .collect();
    w.push(
        popup(
            "settings.row_thumbnail_size",
            &thumbnail_sizes,
            ControlField::ThumbnailSize,
            crate::config::thumbnail_size_index_of(&cfg.layout.thumbnail_size).unwrap_or(0),
        )
        .desc("settings.desc_thumbnail_size")
        .group(RowGroup::ThumbnailOnly)
        .into_spec(),
    );
    let card_text_size = text_size_slider_value(cfg.layout.card_text_size);
    w.push(
        slider(
            "settings.row_card_text_size",
            ControlField::CardTextSize,
            (TEXT_SIZE_MIN, TEXT_SIZE_MAX, 1),
            card_text_size,
            Some(TEXT_SIZE_DEFAULT as f64),
            card_text_size.to_string(),
        )
        .desc("settings.desc_card_text_size")
        .into_spec(),
    );
    let status_bar_stop = status_bar_size_slider_value(cfg.fonts.status_bar_size);
    w.push(
        slider(
            "settings.row_status_bar_text_size",
            ControlField::StatusBarTextSize,
            (STATUS_BAR_SIZE_MIN, STATUS_BAR_SIZE_STOPS - 1, 1),
            status_bar_stop,
            Some(STATUS_BAR_SIZE_DEFAULT_INDEX as f64),
            status_bar_size_for_index(status_bar_stop).to_string(),
        )
        .desc("settings.desc_status_bar_text_size")
        .into_spec(),
    );
    let overlay_positions = vec![
        t("settings.overlay_position_follow_active"),
        t("settings.overlay_position_main_screen"),
    ];
    w.push(
        popup(
            "settings.row_overlay_position",
            &overlay_positions,
            ControlField::OverlayPosition,
            usize::from(cfg.windows.overlay_position.as_str() == "main"),
        )
        .into_spec(),
    );
    let activation_modes = vec![
        t("settings.activation_mode_hover"),
        t("settings.activation_mode_click"),
    ];
    w.push(
        popup(
            "settings.row_activation_mode",
            &activation_modes,
            ControlField::ActivationMode,
            usize::from(cfg.windows.activation_mode.as_str() == "click"),
        )
        .into_spec(),
    );
    for (label_key, desc_key, field, on) in [
        (
            "settings.row_panel_close",
            "settings.desc_panel_close",
            ControlField::PanelCloseEnabled,
            cfg.windows.panel_close_enabled,
        ),
        (
            "settings.row_panel_minimize",
            "settings.desc_panel_minimize",
            ControlField::PanelMinimizeEnabled,
            cfg.windows.panel_minimize_enabled,
        ),
        (
            "settings.row_panel_hide",
            "settings.desc_panel_hide",
            ControlField::PanelHideEnabled,
            cfg.windows.panel_hide_enabled,
        ),
        (
            "settings.row_panel_quit",
            "settings.desc_panel_quit",
            ControlField::PanelQuitEnabled,
            cfg.windows.panel_quit_enabled,
        ),
    ] {
        w.push(switch(label_key, field, on).desc(desc_key).into_spec());
    }
    // The modifier popup lives in a THIRD card with its own heading ("Keyboard"), not at the tail
    // of Window Options: the inline builder opened a section for it after closing the options card,
    // and merging the two put a keyboard setting under a window-options heading.
    w.card("settings.header_keyboard");
    let modifiers = vec![
        t("settings.modifier_option"),
        t("settings.modifier_command"),
        t("settings.modifier_control"),
    ];
    w.push(
        popup(
            "settings.row_modifier",
            &modifiers,
            ControlField::Modifier,
            crate::config::switcher_modifier_index_of(&cfg.keyboard.modifier),
        )
        .into_spec(),
    );
    w.finish()
}
