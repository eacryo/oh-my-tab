//! One settings page as data: its rows, their values and their identities.
//!
//! A page module is pure: it reads `PageInput`, writes `RowSpec`s through `RowWriter` and
//! returns a `PageView`. It creates no view, reads no global and records no search index --
//! the shared renderer does all of that from the description.

use super::*;
/// The General page's description: its rows, their values and their identities.
pub(super) fn general_page_view(input: &PageInput) -> PageView {
    let cfg = &input.config;
    let mut w = RowWriter::new(SettingsPageId::General, t("settings.sidebar_general"));
    w.in_first_card("settings.header_startup");
    w.push(
        switch(
            "settings.row_launch_at_login",
            ControlField::LaunchAtLogin,
            cfg.startup.launch_at_login,
        )
        .desc("settings.desc_launch_at_login")
        .into_spec(),
    );
    w.card("settings.header_menu_bar_icon");
    w.push(
        switch(
            "settings.row_hide_menu_bar_icon",
            ControlField::HideMenuBarIcon,
            cfg.startup.hide_menu_bar_icon,
        )
        .caption("settings.desc_hide_menu_bar_icon")
        .into_spec(),
    );
    w.card("settings.header_appearance");
    let themes = vec![
        t("settings.theme_dark"),
        t("settings.theme_light"),
        t("settings.theme_auto"),
    ];
    let theme_index = match cfg.appearance.theme.as_str() {
        "dark" => 0,
        "light" => 1,
        _ => 2,
    };
    w.push(
        popup(
            "settings.row_theme",
            &themes,
            ControlField::Theme,
            theme_index,
        )
        .desc("settings.desc_theme")
        .into_spec(),
    );
    let materials = vec![
        t("settings.panel_material_liquid_glass"),
        t("settings.panel_material_frost"),
        t("settings.panel_material_opaque"),
    ];
    w.push(
        popup(
            "settings.row_panel_material",
            &materials,
            ControlField::PanelMaterial,
            crate::config::panel_material_index_of(&cfg.appearance.panel_material).unwrap_or(0),
        )
        .desc("settings.desc_panel_material")
        .into_spec(),
    );
    let glass_styles = vec![
        t("settings.glass_style_regular"),
        t("settings.glass_style_clear"),
    ];
    w.push(
        popup(
            "settings.row_glass_style",
            &glass_styles,
            ControlField::GlassStyle,
            crate::glass::glass_style_index(&cfg.appearance.glass_style) as usize,
        )
        .desc("settings.desc_glass_style")
        .group(RowGroup::GlassStyle)
        .into_spec(),
    );
    w.card("settings.header_language");
    let locales: Vec<String> = LOCALE_LABELS
        .iter()
        .map(|label| label.to_string())
        .collect();
    w.push(
        popup(
            "settings.row_locale",
            &locales,
            ControlField::Locale,
            LOCALE_VALUES
                .iter()
                .position(|value| *value == cfg.i18n.locale.as_str())
                .unwrap_or(0),
        )
        .into_spec(),
    );
    w.card("settings.header_logging");
    let log_levels = vec![t("settings.log_level_debug"), t("settings.log_level_info")];
    w.push(
        popup(
            "settings.row_log_level",
            &log_levels,
            ControlField::LogLevel,
            usize::from(cfg.logging.level.as_str() != "debug"),
        )
        .desc("settings.desc_log_level")
        .into_spec(),
    );
    w.push(
        button("settings.row_export_logs", "export_logs")
            .desc("settings.desc_export_logs")
            .into_spec(),
    );
    w.card("settings.header_backup");
    w.push(
        button("settings.row_export_settings", "export_settings")
            .caption("settings.desc_export_settings")
            .into_spec(),
    );
    w.push(
        button("settings.row_import_settings", "import_settings")
            .caption("settings.desc_import_settings")
            .into_spec(),
    );
    let mut view = w.finish();
    // The storage/permission banner: the page reserves its strip, the refresh re-texts it.
    let is_permission_migration = crate::update_notice::needs_permission_migration_copy();
    view.banner = Some(BannerSpec {
        copy_key: if is_permission_migration {
            "settings.permission_migration_warning"
        } else {
            "settings.accessibility_warning"
        },
        action_id: "open_privacy",
        content_h: if is_permission_migration { 60.0 } else { 48.0 },
        gap: 12.0,
    });
    view
}
