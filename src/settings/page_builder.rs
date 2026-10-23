//! Shared context for building settings pages.

use super::*;

/// Common geometry and runtime inputs shared by every settings page builder.
pub(super) struct SettingsPageBuildContext {
    pub(super) content: *mut AnyObject,
    pub(super) content_w: f64,
    pub(super) page_x: f64,
    pub(super) page_frame: NSRect,
    pub(super) page_viewport_h: f64,
    pub(super) palette: UiPalette,
    pub(super) layout: SettingsLayout,
    pub(super) target: *mut AnyObject,
}

pub(super) struct SettingsPageFinalization {
    pub(super) roots: [*mut AnyObject; SETTINGS_PAGE_COUNT],
    pub(super) documents: [*mut AnyObject; SETTINGS_PAGE_COUNT],
    pub(super) bottoms: [f64; SETTINGS_PAGE_COUNT],
}

/// Build the Switcher page and return its final keyboard-card bottom.
pub(super) unsafe fn build_switcher_page(
    context: &SettingsPageBuildContext,
    switcher_view: *mut AnyObject,
    switcher_doc_h: f64,
    ui: &mut SettingsUi,
) -> f64 {
    let content_w = context.content_w;
    let layout = context.layout;
    let target = context.target;
    let label_x = layout.label_x;
    let label_w = layout.label_w;
    let ctrl_w = layout.control_w;
    let ctrl_x = layout.control_x;
    let row_h = layout.row_h;
    let described_row_h = layout.described_row_h;
    // The page's layout owner: rows, cards, the title and the document height all follow from it.
    // The page's rows fall back to this page's title as their section until a card sets one.
    begin_settings_search_page();
    let mut canvas = PageCanvas::new(
        switcher_view,
        content_w,
        layout,
        &t("settings.sidebar_switcher"),
        switcher_doc_h,
        context.page_frame.size.height,
    );
    let y = canvas.next_row(described_row_h);
    // App-switcher master switch: off = Cmd+Tab passes through to the system.
    record_settings_search_row(
        SettingsPageId::Switcher,
        canvas.last_row(),
        &t("settings.row_windows_enabled"),
        Some(&t("settings.desc_windows_enabled")),
    );
    ui.windows_enabled = SettingsRow::described(
        switcher_view,
        label_x,
        y,
        ctrl_x - label_x - super::SETTINGS_CONTROL_LABEL_GAP,
        described_row_h,
        &t("settings.row_windows_enabled"),
        &t("settings.desc_windows_enabled"),
        SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
    );
    let _: () = msg_send![ui.windows_enabled, setTarget: target];
    let _: () = msg_send![
        ui.windows_enabled,
        setAction: sel!(handleWindowsEnabledToggle:)
    ];
    canvas.card(&t("settings.header_windows"));
    set_settings_search_section(&t("settings.header_windows"));
    // The remaining window settings form a second card with its own section title.
    canvas.next_section();
    let y = canvas.next_row(described_row_h);
    // Let the label fill the space before the control column, adapting to the available page width.
    record_settings_search_row(
        SettingsPageId::Switcher,
        canvas.last_row(),
        &t("settings.row_show_minimized"),
        None,
    );
    ui.show_minimized = SettingsRow::tall_before_control(
        switcher_view,
        label_x,
        y,
        ctrl_x,
        super::SETTINGS_CONTROL_LABEL_GAP,
        &t("settings.row_show_minimized"),
        SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
    )
    .1;
    bind_control(target, ui.show_minimized);
    let y = canvas.next_row(described_row_h);
    SettingsRow::separator_above_row(switcher_view, y, described_row_h, content_w);
    record_settings_search_row(
        SettingsPageId::Switcher,
        canvas.last_row(),
        &t("settings.row_show_hidden_app_windows"),
        None,
    );
    ui.show_hidden_app_windows = SettingsRow::tall_before_control(
        switcher_view,
        label_x,
        y,
        ctrl_x,
        super::SETTINGS_CONTROL_LABEL_GAP,
        &t("settings.row_show_hidden_app_windows"),
        SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
    )
    .1;
    bind_control(target, ui.show_hidden_app_windows);
    let y = canvas.next_row(described_row_h);
    SettingsRow::separator_above_row(switcher_view, y, described_row_h, content_w);
    // Described row: the switch carries real caveats (icon-only cards, the Screen Recording
    // dependency for titles, and the unreadable minimized state on another desktop).
    record_settings_search_row(
        SettingsPageId::Switcher,
        canvas.last_row(),
        &t("settings.row_show_other_desktops"),
        Some(&t("settings.desc_show_other_desktops")),
    );
    ui.show_other_desktops = SettingsRow::described(
        switcher_view,
        label_x,
        y,
        ctrl_x - label_x - super::SETTINGS_CONTROL_LABEL_GAP,
        described_row_h,
        &t("settings.row_show_other_desktops"),
        &t("settings.desc_show_other_desktops"),
        SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
    );
    bind_control(target, ui.show_other_desktops);
    // Window display mode: icons only or icons and thumbnails; the config remains stored as
    // the thumbnails_enabled boolean.
    let window_display_mode_labels = [
        t("settings.window_display_mode_icons"),
        t("settings.window_display_mode_icons_thumbnails"),
    ];
    let window_display_mode_refs: Vec<&str> = window_display_mode_labels
        .iter()
        .map(|s| s.as_str())
        .collect();
    let display_mode_metrics = SettingsSelect::metrics();
    // Keep the trigger on the fixed 32pt control grid inside the standard 52pt settings row.
    let y = canvas.next_row(display_mode_metrics.row_h);
    SettingsRow::separator_above_row(switcher_view, y, display_mode_metrics.row_h, content_w);
    record_settings_search_row(
        SettingsPageId::Switcher,
        canvas.last_row(),
        &t("settings.row_window_display_mode"),
        None,
    );
    record_settings_search_options(&window_display_mode_refs);
    ui.thumbnails_enabled = SettingsRow::tall_with_height(
        switcher_view,
        label_x,
        y,
        220.0,
        display_mode_metrics.row_h,
        &t("settings.row_window_display_mode"),
        SettingsControl::popup(
            ctrl_x,
            y + 10.0,
            ctrl_w,
            display_mode_metrics.control_h,
            &window_display_mode_refs,
            0,
        ),
    )
    .1;
    bind_control(target, ui.thumbnails_enabled);
    // These two rows only mean something in icons-and-thumbnails mode: there is no thumbnail to
    // prewarm in icon-only mode, and the app name already gets its own line there (see below), so
    // they are one row group the page layout owner skips (both rows and their dividers go away and
    // the sections below close the gap).
    canvas.group_begin(RowGroup::ThumbnailOnly);
    let y = canvas.next_row(described_row_h);
    SettingsRow::separator_above_row(switcher_view, y, described_row_h, content_w);
    record_settings_search_row(
        SettingsPageId::Switcher,
        canvas.last_row(),
        &t("settings.row_focused_thumbnail_prewarm"),
        None,
    );
    let (_, prewarm_switch) = SettingsRow::tall_with_height(
        switcher_view,
        label_x,
        y,
        ctrl_x - label_x - super::SETTINGS_CONTROL_LABEL_GAP,
        described_row_h,
        &t("settings.row_focused_thumbnail_prewarm"),
        SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
    );
    ui.focused_thumbnail_prewarm = prewarm_switch;
    bind_control(target, ui.focused_thumbnail_prewarm);
    // App name in card titles: the switch controls whether the thumbnail card's caption
    // shows the app name before the window title, separated by " · "; icon-only mode
    // already shows the app name on its own line below the title, so it is unaffected.
    let y = canvas.next_row(described_row_h);
    SettingsRow::separator_above_row(switcher_view, y, described_row_h, content_w);
    record_settings_search_row(
        SettingsPageId::Switcher,
        canvas.last_row(),
        &t("settings.row_show_app_name_in_cards"),
        None,
    );
    let (_, app_name_switch) = SettingsRow::tall(
        switcher_view,
        label_x,
        y,
        220.0,
        &t("settings.row_show_app_name_in_cards"),
        SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
    );
    ui.show_app_name_in_cards = app_name_switch;
    bind_control(target, ui.show_app_name_in_cards);
    // Thumbnail card size: "auto" picks the step from the window count, a percent pins it so adding
    // or removing a window only reflows rows and columns. Thumbnail mode only, like the two rows
    // above (the group is hidden in icon-only mode).
    let thumbnail_size_labels: Vec<String> = crate::config::THUMBNAIL_SIZE_VALUES
        .iter()
        .map(|value| match *value {
            "auto" => t("settings.thumbnail_size_auto"),
            percent => tf("settings.thumbnail_size_percent", &[("percent", percent)]),
        })
        .collect();
    let thumbnail_size_refs: Vec<&str> = thumbnail_size_labels.iter().map(|s| s.as_str()).collect();
    let y = canvas.next_row(described_row_h);
    SettingsRow::separator_above_row(switcher_view, y, described_row_h, content_w);
    record_settings_search_row(
        SettingsPageId::Switcher,
        canvas.last_row(),
        &t("settings.row_thumbnail_size"),
        Some(&t("settings.desc_thumbnail_size")),
    );
    record_settings_search_options(&thumbnail_size_refs);
    ui.thumbnail_size = SettingsRow::described(
        switcher_view,
        label_x,
        y,
        ctrl_x - label_x - super::SETTINGS_CONTROL_LABEL_GAP,
        described_row_h,
        &t("settings.row_thumbnail_size"),
        &t("settings.desc_thumbnail_size"),
        SettingsControl::popup(
            ctrl_x,
            y + 10.0,
            ctrl_w,
            SettingsSelect::metrics().control_h,
            &thumbnail_size_refs,
            0,
        ),
    );
    bind_control(target, ui.thumbnail_size);
    canvas.group_end();
    let y = canvas.next_row(described_row_h);
    SettingsRow::separator_above_row(switcher_view, y, described_row_h, content_w);
    record_settings_search_row(
        SettingsPageId::Switcher,
        canvas.last_row(),
        &t("settings.row_card_text_size"),
        Some(&t("settings.desc_card_text_size")),
    );
    ui.card_text_size = SettingsRow::described(
        switcher_view,
        label_x,
        y,
        ctrl_x - label_x - super::SETTINGS_CONTROL_LABEL_GAP,
        described_row_h,
        &t("settings.row_card_text_size"),
        &t("settings.desc_card_text_size"),
        SettingsControl::stepped_slider(
            ctrl_x,
            y + 10.0,
            SettingsRow::slider_width(ctrl_w),
            row_h,
            TEXT_SIZE_MIN,
            TEXT_SIZE_MAX,
            1,
            TEXT_SIZE_DEFAULT,
            // Double-click restores the default size (15pt).
            Some(TEXT_SIZE_DEFAULT as f64),
        ),
    );
    ui.card_text_size_value_label =
        SettingsRow::attach_slider_readout(switcher_view, ui.card_text_size, TEXT_SIZE_DEFAULT);
    bind_control(target, ui.card_text_size);
    let y = canvas.next_row(described_row_h);
    SettingsRow::separator_above_row(switcher_view, y, described_row_h, content_w);
    record_settings_search_row(
        SettingsPageId::Switcher,
        canvas.last_row(),
        &t("settings.row_status_bar_text_size"),
        Some(&t("settings.desc_status_bar_text_size")),
    );
    ui.status_bar_text_size = SettingsRow::described(
        switcher_view,
        label_x,
        y,
        ctrl_x - label_x - super::SETTINGS_CONTROL_LABEL_GAP,
        described_row_h,
        &t("settings.row_status_bar_text_size"),
        &t("settings.desc_status_bar_text_size"),
        SettingsControl::stepped_slider(
            ctrl_x,
            y + 10.0,
            SettingsRow::slider_width(ctrl_w),
            row_h,
            STATUS_BAR_SIZE_MIN,
            STATUS_BAR_SIZE_STOPS - 1,
            1,
            // The domain is stop indices (0 = hidden, 1..=8 = 13..=20pt); the loaded value is
            // synced by load_settings_values like every other control.
            STATUS_BAR_SIZE_DEFAULT_INDEX,
            // Double-click restores the default size (15pt, stop 3).
            Some(STATUS_BAR_SIZE_DEFAULT_INDEX as f64),
        ),
    );
    ui.status_bar_text_size_value_label = SettingsRow::attach_slider_readout(
        switcher_view,
        ui.status_bar_text_size,
        TEXT_SIZE_DEFAULT,
    );
    bind_control(target, ui.status_bar_text_size);
    // overlay_position popup: [Follow Active Window, Always on Main Screen]; default index 0.
    let op_labels = [
        t("settings.overlay_position_follow_active"),
        t("settings.overlay_position_main_screen"),
    ];
    let op_label_refs: Vec<&str> = op_labels.iter().map(|s| s.as_str()).collect();
    let op_metrics = SettingsSelect::metrics();
    let y = canvas.next_row(op_metrics.row_h);
    SettingsRow::separator_above_row(switcher_view, y, op_metrics.row_h, content_w);
    record_settings_search_row(
        SettingsPageId::Switcher,
        canvas.last_row(),
        &t("settings.row_overlay_position"),
        None,
    );
    record_settings_search_options(&op_label_refs);
    ui.overlay_position = SettingsRow::tall_with_height(
        switcher_view,
        label_x,
        y,
        label_w,
        op_metrics.row_h,
        &t("settings.row_overlay_position"),
        SettingsControl::popup(
            ctrl_x,
            y + (op_metrics.row_h - op_metrics.control_h) / 2.0,
            ctrl_w,
            op_metrics.control_h,
            &op_label_refs,
            0,
        ),
    )
    .1;
    bind_control(target, ui.overlay_position);
    // Window activation mode popup: index 0 = activate on hover, 1 = activate on click;
    // default index 0.
    let activation_labels = [
        t("settings.activation_mode_hover"),
        t("settings.activation_mode_click"),
    ];
    let activation_label_refs: Vec<&str> = activation_labels.iter().map(|s| s.as_str()).collect();
    let activation_metrics = SettingsSelect::metrics();
    let y = canvas.next_row(activation_metrics.row_h);
    SettingsRow::separator_above_row(switcher_view, y, activation_metrics.row_h, content_w);
    record_settings_search_row(
        SettingsPageId::Switcher,
        canvas.last_row(),
        &t("settings.row_activation_mode"),
        None,
    );
    record_settings_search_options(&activation_label_refs);
    ui.activation_mode = SettingsRow::tall_with_height(
        switcher_view,
        label_x,
        y,
        label_w,
        activation_metrics.row_h,
        &t("settings.row_activation_mode"),
        SettingsControl::popup(
            ctrl_x,
            y + (activation_metrics.row_h - activation_metrics.control_h) / 2.0,
            ctrl_w,
            activation_metrics.control_h,
            &activation_label_refs,
            0,
        ),
    )
    .1;
    bind_control(target, ui.activation_mode);
    // In-overlay action keys: one switch per key, all default-off. They only apply while the
    // overlay is the key window (Command is held throughout the summon) and each acts on the
    // HIGHLIGHTED card. Every one of them is a described row: the chord is real information the
    // user cannot guess from the label alone.
    let panel_rows: [(&str, &str); 4] = [
        ("settings.row_panel_close", "settings.desc_panel_close"),
        (
            "settings.row_panel_minimize",
            "settings.desc_panel_minimize",
        ),
        ("settings.row_panel_hide", "settings.desc_panel_hide"),
        ("settings.row_panel_quit", "settings.desc_panel_quit"),
    ];
    for (title_key, desc_key) in panel_rows {
        let y = canvas.next_row(described_row_h);
        SettingsRow::separator_above_row(switcher_view, y, described_row_h, content_w);
        record_settings_search_row(
            SettingsPageId::Switcher,
            canvas.last_row(),
            &t(title_key),
            Some(&t(desc_key)),
        );
        let control = SettingsRow::described(
            switcher_view,
            label_x,
            y,
            ctrl_x - label_x - super::SETTINGS_CONTROL_LABEL_GAP,
            described_row_h,
            &t(title_key),
            &t(desc_key),
            SettingsControl::switch(
                ctrl_x + ctrl_w,
                y + super::SETTINGS_DESCRIBED_CONTROL_OFFSET,
                row_h,
                false,
            ),
        );
        bind_control(target, control);
        match title_key {
            "settings.row_panel_close" => ui.panel_close_enabled = control,
            "settings.row_panel_minimize" => ui.panel_minimize_enabled = control,
            "settings.row_panel_hide" => ui.panel_hide_enabled = control,
            _ => ui.panel_quit_enabled = control,
        }
    }
    canvas.card(&t("settings.header_window_options"));
    set_settings_search_section(&t("settings.header_window_options"));
    canvas.next_section();
    // Modifier popup items follow config::SWITCHER_MODIFIER_VALUES (option / command / control).
    let mod_labels = [
        t("settings.modifier_option"),
        t("settings.modifier_command"),
        t("settings.modifier_control"),
    ];
    let mod_label_refs: Vec<&str> = mod_labels.iter().map(|s| s.as_str()).collect();
    let mod_metrics = SettingsSelect::metrics();
    let y = canvas.next_row(mod_metrics.row_h);
    record_settings_search_row(
        SettingsPageId::Switcher,
        canvas.last_row(),
        &t("settings.row_modifier"),
        None,
    );
    record_settings_search_options(&mod_label_refs);
    ui.modifier = SettingsRow::tall_with_height(
        switcher_view,
        label_x,
        y,
        label_w,
        mod_metrics.row_h,
        &t("settings.row_modifier"),
        SettingsControl::popup(
            ctrl_x,
            y + (mod_metrics.row_h - mod_metrics.control_h) / 2.0,
            ctrl_w,
            mod_metrics.control_h,
            &mod_label_refs,
            0,
        ),
    )
    .1;
    bind_control(target, ui.modifier);
    canvas.card(&t("settings.header_keyboard"));
    set_settings_search_section(&t("settings.header_keyboard"));

    sweep_unindexed_rows(&canvas, SettingsPageId::Switcher);
    let content_bottom = canvas.finish();
    ui.page_canvases[SETTINGS_SWITCHER_PAGE_INDEX] = canvas;
    content_bottom
}

/// Build the Keystroke Display page and return its card bottom.
pub(super) unsafe fn build_keystroke_display_page(
    context: &SettingsPageBuildContext,
    keystroke_display_view: *mut AnyObject,
    keystroke_display_doc_h: f64,
    ui: &mut SettingsUi,
) -> f64 {
    let content_w = context.content_w;
    let layout = context.layout;
    let target = context.target;
    let label_x = layout.label_x;
    let label_w = layout.label_w;
    let ctrl_w = layout.control_w;
    let ctrl_x = layout.control_x;
    let row_h = layout.row_h;
    let described_row_h = layout.described_row_h;
    // The page's rows fall back to this page's title as their section until a card sets one.
    begin_settings_search_page();
    let mut canvas = PageCanvas::new(
        keystroke_display_view,
        content_w,
        layout,
        &t("settings.sidebar_keystroke_display"),
        keystroke_display_doc_h,
        context.page_frame.size.height,
    );

    let y = canvas.next_row(described_row_h);
    record_settings_search_row(
        SettingsPageId::KeystrokeDisplay,
        canvas.last_row(),
        &t("settings.row_keystroke_display_enabled"),
        Some(&t("settings.desc_keystroke_display_enabled")),
    );
    ui.keystroke_display_enabled = SettingsRow::described(
        keystroke_display_view,
        label_x,
        y,
        ctrl_x - label_x - super::SETTINGS_CONTROL_LABEL_GAP,
        described_row_h,
        &t("settings.row_keystroke_display_enabled"),
        &t("settings.desc_keystroke_display_enabled"),
        SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
    );
    bind_control(target, ui.keystroke_display_enabled);
    // The master switch keeps a card of its own, like the mouse, clipboard, and key-binding pages:
    // the card boundary is what makes it read as the feature's on/off row instead of the first row
    // of the options list. The options below open a second card with their own header.
    canvas.card(&t("settings.header_keystroke_display"));
    set_settings_search_section(&t("settings.header_keystroke_display"));
    canvas.next_section();

    let mode_labels = [
        t("settings.keystroke_display_mode_all"),
        t("settings.keystroke_display_mode_shortcuts"),
        t("settings.keystroke_display_mode_commands"),
    ];
    let mode_refs: Vec<&str> = mode_labels.iter().map(|s| s.as_str()).collect();
    let mode_metrics = SettingsSelect::metrics();
    let y = canvas.next_row(mode_metrics.row_h);
    record_settings_search_row(
        SettingsPageId::KeystrokeDisplay,
        canvas.last_row(),
        &t("settings.row_keystroke_display_mode"),
        None,
    );
    record_settings_search_options(&mode_refs);
    ui.keystroke_display_mode = SettingsRow::tall_with_height(
        keystroke_display_view,
        label_x,
        y,
        label_w,
        mode_metrics.row_h,
        &t("settings.row_keystroke_display_mode"),
        SettingsControl::popup(
            ctrl_x,
            y + (mode_metrics.row_h - mode_metrics.control_h) / 2.0,
            ctrl_w,
            mode_metrics.control_h,
            &mode_refs,
            0,
        ),
    )
    .1;
    bind_control(target, ui.keystroke_display_mode);

    let tap_labels = [
        t("settings.keystroke_display_tap_session"),
        t("settings.keystroke_display_tap_hid"),
    ];
    let tap_refs: Vec<&str> = tap_labels.iter().map(|s| s.as_str()).collect();
    let tap_metrics = SettingsSelect::metrics();
    let y = canvas.next_row(tap_metrics.row_h);
    SettingsRow::separator_above_row(keystroke_display_view, y, tap_metrics.row_h, content_w);
    record_settings_search_row(
        SettingsPageId::KeystrokeDisplay,
        canvas.last_row(),
        &t("settings.row_keystroke_display_tap_level"),
        None,
    );
    record_settings_search_options(&tap_refs);
    ui.keystroke_display_tap_level = SettingsRow::tall_with_height(
        keystroke_display_view,
        label_x,
        y,
        label_w,
        tap_metrics.row_h,
        &t("settings.row_keystroke_display_tap_level"),
        SettingsControl::popup(
            ctrl_x,
            y + (tap_metrics.row_h - tap_metrics.control_h) / 2.0,
            ctrl_w,
            tap_metrics.control_h,
            &tap_refs,
            0,
        ),
    )
    .1;
    bind_control(target, ui.keystroke_display_tap_level);

    let display_position_labels = [
        t("settings.keystroke_display_position_main"),
        t("settings.keystroke_display_position_caret"),
    ];
    let display_position_refs: Vec<&str> =
        display_position_labels.iter().map(|s| s.as_str()).collect();
    let display_position_metrics = SettingsSelect::metrics();
    let y = canvas.next_row(display_position_metrics.row_h);
    SettingsRow::separator_above_row(
        keystroke_display_view,
        y,
        display_position_metrics.row_h,
        content_w,
    );
    record_settings_search_row(
        SettingsPageId::KeystrokeDisplay,
        canvas.last_row(),
        &t("settings.row_keystroke_display_position"),
        None,
    );
    record_settings_search_options(&display_position_refs);
    ui.keystroke_display_position = SettingsRow::tall_with_height(
        keystroke_display_view,
        label_x,
        y,
        label_w,
        display_position_metrics.row_h,
        &t("settings.row_keystroke_display_position"),
        SettingsControl::popup(
            ctrl_x,
            y + (display_position_metrics.row_h - display_position_metrics.control_h) / 2.0,
            ctrl_w,
            display_position_metrics.control_h,
            &display_position_refs,
            1,
        ),
    )
    .1;
    bind_control(target, ui.keystroke_display_position);

    let initial_position_labels = [
        t("settings.keystroke_display_initial_position_top"),
        t("settings.keystroke_display_initial_position_bottom"),
        t("settings.keystroke_display_initial_position_left"),
        t("settings.keystroke_display_initial_position_right"),
    ];
    let initial_position_refs: Vec<&str> =
        initial_position_labels.iter().map(|s| s.as_str()).collect();
    let initial_position_metrics = SettingsSelect::metrics();
    let y = canvas.next_row(initial_position_metrics.row_h);
    SettingsRow::separator_above_row(
        keystroke_display_view,
        y,
        initial_position_metrics.row_h,
        content_w,
    );
    record_settings_search_row(
        SettingsPageId::KeystrokeDisplay,
        canvas.last_row(),
        &t("settings.row_keystroke_display_initial_position"),
        None,
    );
    record_settings_search_options(&initial_position_refs);
    ui.keystroke_display_initial_position = SettingsRow::tall_with_height(
        keystroke_display_view,
        label_x,
        y,
        label_w,
        initial_position_metrics.row_h,
        &t("settings.row_keystroke_display_initial_position"),
        SettingsControl::popup(
            ctrl_x,
            y + (initial_position_metrics.row_h - initial_position_metrics.control_h) / 2.0,
            ctrl_w,
            initial_position_metrics.control_h,
            &initial_position_refs,
            // Index 1 is "bottom", the documented default.
            1,
        ),
    )
    .1;
    bind_control(target, ui.keystroke_display_initial_position);

    // The panel's stream-axis cap. One value covers both orientations: it is the width of a
    // horizontal stream and the height of a vertical one. Whole tens only, so the readout, the
    // saved value and the config validation all speak the same steps. No marker is drawn at the
    // default stop: a full-height rule there pokes out above and below the knob and reads as a
    // stray line, and the stop is already legible while the user drags. The reset gesture itself is
    // announced by the slider's tooltip.
    // A standard single-line row: the slider control keeps its CONTROL_H height, while the row
    // itself uses the shared SINGLE_LINE_ROW_H like every other row on the page.
    let y = canvas.next_row(described_row_h);
    SettingsRow::separator_above_row(keystroke_display_view, y, described_row_h, content_w);
    record_settings_search_row(
        SettingsPageId::KeystrokeDisplay,
        canvas.last_row(),
        &t("settings.row_keystroke_display_max_extent"),
        None,
    );
    ui.keystroke_display_max_extent = SettingsRow::tall(
        keystroke_display_view,
        label_x,
        y,
        label_w,
        &t("settings.row_keystroke_display_max_extent"),
        SettingsControl::stepped_slider(
            ctrl_x,
            y + 10.0,
            SettingsRow::slider_width(ctrl_w),
            row_h,
            max_extent_slider_min(),
            max_extent_slider_max(),
            crate::config::KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_STEP as i64,
            max_extent_slider_value(
                crate::config::CONFIG
                    .read()
                    .unwrap()
                    .keystroke_display
                    .max_extent_percent,
            ),
            // Double-click restores the 50% default, which is the panel's pre-setting behaviour.
            Some(crate::config::KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_DEFAULT as f64),
        ),
    )
    .1;
    ui.keystroke_display_max_extent_value = SettingsRow::attach_slider_readout(
        keystroke_display_view,
        ui.keystroke_display_max_extent,
        max_extent_readout(
            crate::config::CONFIG
                .read()
                .unwrap()
                .keystroke_display
                .max_extent_percent,
        ),
    );
    bind_control(target, ui.keystroke_display_max_extent);
    canvas.card(&t("settings.header_keystroke_display_options"));
    set_settings_search_section(&t("settings.header_keystroke_display_options"));

    sweep_unindexed_rows(&canvas, SettingsPageId::KeystrokeDisplay);
    let content_bottom = canvas.finish();
    ui.page_canvases[SETTINGS_KEYSTROKE_DISPLAY_PAGE_INDEX] = canvas;
    content_bottom
}

/// The keystroke panel cap slider range, in whole percent. Shared with dispatch.rs so the
/// range is stated once.
pub(super) fn max_extent_slider_min() -> i64 {
    crate::config::KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_MIN as i64
}

pub(super) fn max_extent_slider_max() -> i64 {
    crate::config::KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_MAX as i64
}

/// Slider integer -> config value. Only the slider own steps can arrive, but a stale or
/// hand-set value is snapped rather than trusted, so the persisted number is always valid.
pub(super) fn max_extent_slider_value(percent: u32) -> i64 {
    crate::config::keystroke_display_max_extent_percent_snap(percent) as i64
}

/// The row readout: the percentage, which is the unit the slider itself speaks.
pub(super) fn max_extent_readout(percent: u32) -> String {
    format!(
        "{}%",
        crate::config::keystroke_display_max_extent_percent_snap(percent)
    )
}

/// Write the readout text of the panel-cap slider. Called from the change callback, so the number
/// follows the drag; it formats through the same helper the row was built with.
pub(super) unsafe fn set_max_extent_readout(label: *mut AnyObject, percent: u32) {
    if label.is_null() {
        return;
    }
    set_field(label, max_extent_readout(percent));
}

unsafe impl Send for SettingsPageBuildContext {}
unsafe impl Sync for SettingsPageBuildContext {}

/// Build the General page and return its final content bottom.
pub(super) unsafe fn build_general_page(
    context: &SettingsPageBuildContext,
    general_view: *mut AnyObject,
    general_doc_h: f64,
    ui: &mut SettingsUi,
) -> f64 {
    let content_w = context.content_w;
    let page_x = context.page_x;
    let page_viewport_h = context.page_viewport_h;
    let layout = context.layout;
    let target = context.target;
    let label_x = layout.label_x;
    let label_w = layout.label_w;
    let ctrl_w = layout.control_w;
    let ctrl_x = layout.control_x;
    let row_h = layout.row_h;
    let described_row_h = layout.described_row_h;
    // The whole page-top block (title + first section heading) comes from the component; the
    // returned cursor is that heading's own cursor.
    // The page's layout owner: rows, cards, the title and the document height all follow from it.
    // The page's rows fall back to this page's title as their section until a card sets one.
    begin_settings_search_page();
    let mut canvas = PageCanvas::new(
        general_view,
        content_w,
        layout,
        &t("settings.sidebar_general"),
        general_doc_h,
        context.page_frame.size.height,
    );

    // The banner is a sibling of the scroll views; its strip and bottom gap are reserved so
    // it cannot cover the title or cards.
    let is_permission_migration = crate::update_notice::needs_permission_migration_copy();
    let banner_content_h = if is_permission_migration { 60.0 } else { 48.0 };
    let banner_gap = 12.0;
    let banner_h = banner_content_h + banner_gap;
    let banner: *mut AnyObject = msg_send![class!(NSView), alloc];
    let banner: *mut AnyObject = msg_send![
        banner,
        initWithFrame: NSRect::new(
            NSPoint::new(
                page_x,
                page_viewport_h - SettingsPageHeader::TOP_PADDING - banner_h,
            ),
            NSSize::new(content_w, banner_h)
        )
    ];
    // Stretch horizontally and stay pinned to the content top (WidthSizable|MinYMargin = 10).
    let _: () = msg_send![banner, setAutoresizingMask: 10u64];
    ui.permission_warning_view = banner;

    // warning text: word-wrapped, system red
    let warning_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let warning_label: *mut AnyObject = msg_send![
        warning_label,
        initWithFrame: NSRect::new(
            NSPoint::new(12.0, banner_gap + 6.0),
            NSSize::new(content_w - 160.0, banner_content_h - 12.0)
        )
    ];
    let warning_key = if is_permission_migration {
        "settings.permission_migration_warning"
    } else {
        "settings.accessibility_warning"
    };
    let wl = make_nsstring(&t(warning_key));
    let _: () = msg_send![warning_label, setStringValue: wl];
    CFRelease(wl as *const c_void);
    // The refresh path re-texts this label: the warning shown depends on live state (permissions,
    // and whether the clipboard storage key could be obtained), not only on the build-time one.
    ui.permission_warning_label = warning_label;
    let _: () = msg_send![warning_label, setEditable: false];
    let _: () = msg_send![warning_label, setBezeled: false];
    let _: () = msg_send![warning_label, setDrawsBackground: false];
    let _: () = msg_send![warning_label, setUsesSingleLineMode: false];
    let _: () = msg_send![warning_label, setLineBreakMode: 0isize]; // NSLineBreakByWordWrapping
    let red: *mut AnyObject = msg_send![class!(NSColor), systemRedColor];
    let _: () = msg_send![warning_label, setTextColor: red];
    // Stretches with the banner and stays left-anchored (WidthSizable = 2).
    let _: () = msg_send![warning_label, setAutoresizingMask: 2u64];
    let _: () = msg_send![banner, addSubview: warning_label];
    release_obj(warning_label);

    // "Open Privacy & Security" button
    let open_btn = SettingsButton::action(
        NSRect::new(
            NSPoint::new(
                content_w - 150.0,
                banner_gap + (banner_content_h - 28.0) / 2.0,
            ),
            NSSize::new(140.0, 28.0),
        ),
        &t("settings.btn_open_privacy"),
        target,
        sel!(handleOpenPrivacy:),
        SettingsButtonRole::Action,
    );
    let _: () = msg_send![banner, addSubview: open_btn];
    release_obj(open_btn);
    // The storage warning reuses this strip but has no privacy pane to offer, so the refresh hides
    // the button with the variant.
    ui.permission_warning_button = open_btn;

    // Start hidden; page selection applies the permission state and resizes General's viewport.
    let _: () = msg_send![banner, setHidden: true];

    // Startup card, first on the page: the two settings a new install most often needs.
    let y = canvas.next_row(described_row_h);
    // Launch-at-login switch: no title (the row label on the left already describes it).
    record_settings_search_row(
        SettingsPageId::General,
        canvas.last_row(),
        &t("settings.row_launch_at_login"),
        Some(&t("settings.desc_launch_at_login")),
    );
    ui.launch_at_login = SettingsRow::described(
        general_view,
        label_x,
        y,
        content_w - label_x * 2.0 - 58.0,
        described_row_h,
        &t("settings.row_launch_at_login"),
        &t("settings.desc_launch_at_login"),
        SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
    );
    bind_control(target, ui.launch_at_login);
    canvas.card(&t("settings.header_startup"));
    set_settings_search_section(&t("settings.header_startup"));

    // Menu-bar icon: its own card, because hiding the icon is a distinct decision from what the app
    // does at login — and the row has to say where Settings can still be reached without it.
    canvas.next_section();
    let caption = t("settings.desc_hide_menu_bar_icon");
    let caption_w = content_w - label_x * 2.0 - 58.0;
    let caption_row_h = widgets::captioned_row_height(&caption, caption_w);
    let y = canvas.next_row(caption_row_h);
    // `captioned`, not `described`: the latter never renders its subtitle, and this row has to carry
    // the "Settings can still be reached" promise that keeps a hidden icon from stranding the user
    // (asserted by --smoke-settings-layout).
    record_settings_search_row(
        SettingsPageId::General,
        canvas.last_row(),
        &t("settings.row_hide_menu_bar_icon"),
        Some(&caption),
    );
    ui.hide_menu_bar_icon = SettingsRow::captioned(
        general_view,
        label_x,
        y,
        caption_w,
        caption_row_h,
        &t("settings.row_hide_menu_bar_icon"),
        &caption,
        SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
    )
    .2;
    bind_control(target, ui.hide_menu_bar_icon);
    canvas.card(&t("settings.header_menu_bar_icon"));
    set_settings_search_section(&t("settings.header_menu_bar_icon"));

    canvas.next_section();
    let theme_items = [
        t("settings.theme_dark"),
        t("settings.theme_light"),
        t("settings.theme_auto"),
    ];
    let theme_item_refs: Vec<&str> = theme_items.iter().map(String::as_str).collect();
    let theme_metrics = SettingsSelect::metrics();
    let y = canvas.next_row(theme_metrics.row_h);
    record_settings_search_row(
        SettingsPageId::General,
        canvas.last_row(),
        &t("settings.row_theme"),
        Some(&t("settings.desc_theme")),
    );
    record_settings_search_options(&theme_item_refs);
    ui.theme = SettingsRow::described(
        general_view,
        label_x,
        y,
        ctrl_x - label_x - super::SETTINGS_CONTROL_LABEL_GAP,
        theme_metrics.row_h,
        &t("settings.row_theme"),
        &t("settings.desc_theme"),
        SettingsControl::popup(
            ctrl_x,
            y + 10.0,
            ctrl_w,
            theme_metrics.control_h,
            &theme_item_refs,
            0,
        ),
    );
    bind_control(target, ui.theme);
    let material_items = [
        t("settings.panel_material_liquid_glass"),
        t("settings.panel_material_frost"),
        t("settings.panel_material_opaque"),
    ];
    let material_item_refs: Vec<&str> = material_items.iter().map(String::as_str).collect();
    let material_metrics = SettingsSelect::metrics();
    let y = canvas.next_block(material_metrics.row_h);
    SettingsRow::separator(general_view, y + material_metrics.row_h, content_w);
    record_settings_search_row(
        SettingsPageId::General,
        canvas.last_row(),
        &t("settings.row_panel_material"),
        Some(&t("settings.desc_panel_material")),
    );
    record_settings_search_options(&material_item_refs);
    ui.panel_material = SettingsRow::described(
        general_view,
        label_x,
        y,
        ctrl_x - label_x - super::SETTINGS_CONTROL_LABEL_GAP,
        material_metrics.row_h,
        &t("settings.row_panel_material"),
        &t("settings.desc_panel_material"),
        SettingsControl::popup(
            ctrl_x,
            y + 10.0,
            ctrl_w,
            material_metrics.control_h,
            &material_item_refs,
            0,
        ),
    );
    bind_control(target, ui.panel_material);
    // The glass style is a Liquid-Glass sub-option. It is always built and shown or hidden as a
    // `RowGroup` (see `update_conditional_rows`), so switching the panel material reflows this one
    // row in place instead of rebuilding the whole settings content. The private `_variant` is
    // deliberately NOT a user choice: two attempts at labelling it from a brightness sample were
    // wrong (variant 19 measures the *most* transparent and renders broken rainbow edges), so it
    // stays a config/dev knob until a backdrop-differential measurement exists.
    canvas.group_begin(RowGroup::GlassStyle);
    // The two looks this app has always had: `regular` (index 0) and `clear` (index 1). The index is
    // the config value (see `ControlField::GlassStyle`).
    let glass_style_labels = [
        t("settings.glass_style_regular"),
        t("settings.glass_style_clear"),
    ];
    let glass_style_label_refs: Vec<&str> = glass_style_labels.iter().map(String::as_str).collect();
    let glass_style_metrics = SettingsSelect::metrics();
    let y = canvas.next_block(glass_style_metrics.row_h);
    SettingsRow::separator(general_view, y + glass_style_metrics.row_h, content_w);
    record_settings_search_row(
        SettingsPageId::General,
        canvas.last_row(),
        &t("settings.row_glass_style"),
        Some(&t("settings.desc_glass_style")),
    );
    record_settings_search_options(&glass_style_label_refs);
    ui.glass_style = SettingsRow::described(
        general_view,
        label_x,
        y,
        ctrl_x - label_x - super::SETTINGS_CONTROL_LABEL_GAP,
        glass_style_metrics.row_h,
        &t("settings.row_glass_style"),
        &t("settings.desc_glass_style"),
        SettingsControl::popup(
            ctrl_x,
            y + 10.0,
            ctrl_w,
            glass_style_metrics.control_h,
            &glass_style_label_refs,
            0,
        ),
    );
    bind_control(target, ui.glass_style);
    canvas.group_end();
    canvas.card(&t("settings.header_appearance"));
    set_settings_search_section(&t("settings.header_appearance"));

    canvas.next_section();
    let locale_metrics = SettingsSelect::metrics();
    let y = canvas.next_row(locale_metrics.row_h);
    record_settings_search_row(
        SettingsPageId::General,
        canvas.last_row(),
        &t("settings.row_locale"),
        None,
    );
    record_settings_search_options(&LOCALE_LABELS);
    ui.locale = SettingsRow::plain(
        general_view,
        label_x,
        y,
        label_w,
        locale_metrics.row_h,
        &t("settings.row_locale"),
        SettingsControl::popup(
            ctrl_x,
            y,
            ctrl_w,
            locale_metrics.control_h,
            &LOCALE_LABELS,
            0,
        ),
    );
    bind_control(target, ui.locale);
    canvas.card(&t("settings.header_language"));
    set_settings_search_section(&t("settings.header_language"));

    canvas.next_section();
    // Log level popup: items = [debug, info]; default index 1 (info).
    let log_levels = [t("settings.log_level_debug"), t("settings.log_level_info")];
    let log_level_refs: Vec<&str> = log_levels.iter().map(String::as_str).collect();
    let log_level_metrics = SettingsSelect::metrics();
    let y = canvas.next_row(log_level_metrics.row_h);
    record_settings_search_row(
        SettingsPageId::General,
        canvas.last_row(),
        &t("settings.row_log_level"),
        Some(&t("settings.desc_log_level")),
    );
    record_settings_search_options(&log_level_refs);
    ui.log_level = SettingsRow::described(
        general_view,
        label_x,
        y,
        ctrl_x - label_x - super::SETTINGS_CONTROL_LABEL_GAP,
        log_level_metrics.row_h,
        &t("settings.row_log_level"),
        &t("settings.desc_log_level"),
        SettingsControl::popup(
            ctrl_x,
            y + 10.0,
            ctrl_w,
            log_level_metrics.control_h,
            &log_level_refs,
            1,
        ),
    );
    bind_control(target, ui.log_level);
    // Export logs: title+description on the left, action button on the right (same card
    // as the log level; the button opts out of ControlField live-apply and goes straight
    // through target/action).
    let y = canvas.next_row(described_row_h);
    // In-card divider: the export row sits right below it (separator_above_row owns the
    // row-relative math).
    SettingsRow::separator_above_row(general_view, y, described_row_h, content_w);
    let export_btn = row_action_button(
        ctrl_x,
        ctrl_w,
        y,
        &t("settings.btn_export_logs"),
        target,
        sel!(handleExportLogs:),
    );
    record_settings_search_row(
        SettingsPageId::General,
        canvas.last_row(),
        &t("settings.row_export_logs"),
        Some(&t("settings.desc_export_logs")),
    );
    SettingsRow::described(
        general_view,
        label_x,
        y,
        ctrl_x - label_x - super::SETTINGS_CONTROL_LABEL_GAP,
        described_row_h,
        &t("settings.row_export_logs"),
        &t("settings.desc_export_logs"),
        export_btn,
    );
    canvas.card(&t("settings.header_logging"));
    set_settings_search_section(&t("settings.header_logging"));

    // Backup: export the whole configuration to a file, or restore it on this or another Mac.
    // `captioned`, not `described`: the latter's subtitle is never rendered, and the import row has
    // to carry the "this replaces your settings" warning in place of a confirmation dialog.
    canvas.next_section();
    let caption = t("settings.desc_export_settings");
    let caption_w = ctrl_x - label_x - super::SETTINGS_CONTROL_LABEL_GAP;
    let caption_row_h = widgets::captioned_row_height(&caption, caption_w);
    let y = canvas.next_row(caption_row_h);
    let export_settings_btn = row_action_button(
        ctrl_x,
        ctrl_w,
        y,
        &t("settings.btn_export_settings"),
        target,
        sel!(handleExportSettings:),
    );
    record_settings_search_row(
        SettingsPageId::General,
        canvas.last_row(),
        &t("settings.row_export_settings"),
        Some(&caption),
    );
    SettingsRow::captioned(
        general_view,
        label_x,
        y,
        caption_w,
        caption_row_h,
        &t("settings.row_export_settings"),
        &caption,
        export_settings_btn,
    );
    let caption = t("settings.desc_import_settings");
    let caption_row_h = widgets::captioned_row_height(&caption, caption_w);
    let y = canvas.next_row(caption_row_h);
    SettingsRow::separator_above_row(general_view, y, caption_row_h, content_w);
    let import_settings_btn = row_action_button(
        ctrl_x,
        ctrl_w,
        y,
        &t("settings.btn_import_settings"),
        target,
        sel!(handleImportSettings:),
    );
    record_settings_search_row(
        SettingsPageId::General,
        canvas.last_row(),
        &t("settings.row_import_settings"),
        Some(&caption),
    );
    SettingsRow::captioned(
        general_view,
        label_x,
        y,
        caption_w,
        caption_row_h,
        &t("settings.row_import_settings"),
        &caption,
        import_settings_btn,
    );
    canvas.card(&t("settings.header_backup"));
    set_settings_search_section(&t("settings.header_backup"));

    sweep_unindexed_rows(&canvas, SettingsPageId::General);
    let content_bottom = canvas.finish();
    ui.page_canvases[SETTINGS_GENERAL_PAGE_INDEX] = canvas;
    content_bottom
}

/// Build the Mouse page and return its final content bottom.
pub(super) unsafe fn build_mouse_page(
    context: &SettingsPageBuildContext,
    mouse_view: *mut AnyObject,
    mouse_doc_h: f64,
    ui: &mut SettingsUi,
) -> f64 {
    let content_w = context.content_w;
    let layout = context.layout;
    let target = context.target;
    let label_x = layout.label_x;
    let label_w = layout.label_w;
    let ctrl_w = layout.control_w;
    let ctrl_x = layout.control_x;
    let row_h = layout.row_h;
    let described_row_h = layout.described_row_h;
    // The page's layout owner: rows, cards, the title and the document height all follow from it.
    // The page's rows fall back to this page's title as their section until a card sets one.
    begin_settings_search_page();
    let mut canvas = PageCanvas::new(
        mouse_view,
        content_w,
        layout,
        &t("settings.sidebar_mouse"),
        mouse_doc_h,
        context.page_frame.size.height,
    );

    // Header: every section in this app carries a short-noun heading (Device / Scrolling /
    // Pointer / Button Mappings, Clipboard, ...), and this master-switch card was the only one
    // without it, which read as a blank spot at its top-left. "Mouse" rather than "Mouse
    // control": one notch shorter than the page title, the same way the clipboard page pairs
    // its "Clipboard" heading with the "Clipboard History" title, and it never repeats the row's
    // "Enable mouse control". Its distance from the page title comes from SettingsPageHeader.
    let y = canvas.next_row(described_row_h);
    record_settings_search_row(
        SettingsPageId::Mouse,
        canvas.last_row(),
        &t("settings.row_enable_mouse"),
        Some(&t("settings.desc_enable_mouse")),
    );
    ui.enable_mouse = SettingsRow::described(
        mouse_view,
        label_x,
        y,
        ctrl_x - label_x - super::SETTINGS_CONTROL_LABEL_GAP,
        described_row_h,
        &t("settings.row_enable_mouse"),
        &t("settings.desc_enable_mouse"),
        SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
    );
    // Update OK button title in real time when the switch toggles (OK vs OK && Restart).
    let _: () = msg_send![ui.enable_mouse, setTarget: target];
    let _: () = msg_send![ui.enable_mouse, setAction: sel!(handleEnableMouseToggle:)];
    canvas.card(&t("settings.header_mouse"));
    set_settings_search_section(&t("settings.header_mouse"));

    canvas.next_section();
    // Popup: items are rebuilt dynamically in load_settings_values (device list is mutable).
    // A placeholder is inserted here; the real items are filled by rebuild_device_popup.
    let devices = crate::mouse::device::connected_devices();
    let device_labels: Vec<String> = devices.iter().map(dispatch::device_display_name).collect();
    let device_label_refs: Vec<&str> = if device_labels.is_empty() {
        vec![""]
    } else {
        device_labels.iter().map(|s| s.as_str()).collect()
    };
    let device_metrics = SettingsSelect::metrics();
    let device_caption_w = ctrl_x - label_x - super::SETTINGS_CONTROL_LABEL_GAP;
    // The caption is re-set in place when the popup selection changes, so the row height has
    // to cover the longest caption any connected device can produce, not just the first.
    let device_row_h = devices
        .iter()
        .map(|d| dispatch::device_info_caption(Some(d)))
        .chain(std::iter::once(dispatch::device_info_caption(None)))
        .map(|c| widgets::captioned_row_height(&c, device_caption_w))
        .fold(device_metrics.row_h, f64::max);
    let y = canvas.next_row(device_row_h);
    let dev_popup = SettingsControl::popup(
        ctrl_x,
        y + (device_row_h - device_metrics.control_h) / 2.0,
        ctrl_w,
        device_metrics.control_h,
        &device_label_refs,
        0,
    );
    style_flat_popup(dev_popup);
    // Bind target/action: on selection change, immediately refresh the other controls with
    // the selected device's effective values.
    let _: () = msg_send![dev_popup, setTarget: target];
    let _: () = msg_send![dev_popup, setAction: sel!(handleDeviceChanged:)];
    record_settings_search_row(
        SettingsPageId::Mouse,
        canvas.last_row(),
        &t("settings.header_mouse_device"),
        None,
    );
    record_settings_search_options(&device_label_refs);
    let (_, device_info_caption, device_control) = SettingsRow::captioned(
        mouse_view,
        label_x,
        y,
        device_caption_w,
        device_row_h,
        &t("settings.header_mouse_device"),
        &dispatch::device_info_caption(devices.first()),
        dev_popup,
    );
    ui.device_info_caption = device_info_caption;
    ui.device_indicator = device_control;

    let scroll_mode_labels: Vec<String> = SCROLL_MODE_LABEL_KEYS.iter().map(|key| t(key)).collect();
    let scroll_mode_label_refs: Vec<&str> = scroll_mode_labels.iter().map(String::as_str).collect();
    let scroll_metrics = SettingsSelect::metrics();
    let y = canvas.next_row(scroll_metrics.row_h);
    let scroll_popup = SettingsControl::popup(
        ctrl_x,
        y + (scroll_metrics.row_h - scroll_metrics.control_h) / 2.0,
        ctrl_w,
        scroll_metrics.control_h,
        &scroll_mode_label_refs,
        0,
    );
    style_flat_popup(scroll_popup);
    record_settings_search_row(
        SettingsPageId::Mouse,
        canvas.last_row(),
        &t("settings.row_scroll_mode"),
        None,
    );
    record_settings_search_options(&scroll_mode_label_refs);
    ui.scroll_mode = SettingsRow::tall_with_height(
        mouse_view,
        label_x,
        y,
        label_w,
        scroll_metrics.row_h,
        &t("settings.row_scroll_mode"),
        scroll_popup,
    )
    .1;
    bind_control(target, ui.scroll_mode);
    // The HTML device card contains both rows, with one internal hairline between them.
    SettingsRow::separator_above_row(mouse_view, y, scroll_metrics.row_h, content_w);

    // Keep this conditional row in the same card as Device and Scroll mode.
    // The line-count row only appears in Line scroll mode: one row group the page layout owner
    // skips (its divider goes with it and the shared device card's bottom edge closes the gap).
    canvas.group_begin(RowGroup::LineCount);
    let y = canvas.next_row(described_row_h);
    SettingsRow::separator_above_row(mouse_view, y, described_row_h, content_w);
    record_settings_search_row(
        SettingsPageId::Mouse,
        canvas.last_row(),
        &t("settings.row_line_count"),
        None,
    );
    let (line_label, line_ctrl) = SettingsRow::tall(
        mouse_view,
        label_x,
        y,
        label_w,
        &t("settings.row_line_count"),
        // Leaves the readout's width on the right for the read-only value label (see
        // SettingsRow::slider_width). Stepped 1..=10 in whole lines (matches config validation;
        // the range and interaction mirror LinearMouse's By Lines slider).
        SettingsControl::stepped_slider(
            ctrl_x,
            y + 10.0,
            SettingsRow::slider_width(ctrl_w),
            row_h,
            1,
            10,
            1,
            3,
            // Double-click restores the default line count (3).
            Some(3.0),
        ),
    );
    ui.line_count = line_ctrl;
    ui.line_count_label = line_label;
    // Read-only value label right of the slider: shows the current line count, refreshed
    // live as the slider moves.
    ui.line_count_value_label = SettingsRow::attach_slider_readout(mouse_view, line_ctrl, 3);
    bind_control(target, ui.line_count);
    canvas.group_end();
    canvas.card(&t("settings.header_mouse_device"));
    set_settings_search_section(&t("settings.header_mouse_device"));

    canvas.next_section();
    let y = canvas.next_row(described_row_h);
    // reverse_scroll switch: title + subtitle describe the scroll inversion; the switch
    // keeps the reference page's trailing inset.
    record_settings_search_row(
        SettingsPageId::Mouse,
        canvas.last_row(),
        &t("settings.row_reverse_scroll"),
        Some(&t("settings.desc_reverse_scroll")),
    );
    ui.reverse_scroll = SettingsRow::described(
        mouse_view,
        label_x,
        y,
        ctrl_x - label_x - super::SETTINGS_CONTROL_LABEL_GAP,
        described_row_h,
        &t("settings.row_reverse_scroll"),
        &t("settings.desc_reverse_scroll"),
        SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
    );
    bind_control(target, ui.reverse_scroll);
    canvas.card(&t("settings.header_mouse_scrolling"));
    set_settings_search_section(&t("settings.header_mouse_scrolling"));

    canvas.next_section();
    canvas.group_begin(RowGroup::SmoothScrolling);
    let preset_labels: Vec<String> = crate::mouse::smooth::presets::SmoothPreset::ALL
        .into_iter()
        .map(|preset| t(preset.label_key()))
        .collect();
    let preset_label_refs: Vec<&str> = preset_labels.iter().map(String::as_str).collect();
    let preset_metrics = SettingsSelect::metrics();
    let preset_y = canvas.next_row(preset_metrics.row_h);
    let preset_popup = SettingsControl::popup(
        ctrl_x,
        preset_y + (preset_metrics.row_h - preset_metrics.control_h) / 2.0,
        ctrl_w,
        preset_metrics.control_h,
        &preset_label_refs,
        0,
    );
    style_flat_popup(preset_popup);
    record_settings_search_row(
        SettingsPageId::Mouse,
        canvas.last_row(),
        &t("settings.row_mouse_smooth_preset"),
        None,
    );
    record_settings_search_options(&preset_label_refs);
    let (_, preset_control) = SettingsRow::tall_with_height(
        mouse_view,
        label_x,
        preset_y,
        label_w,
        preset_metrics.row_h,
        &t("settings.row_mouse_smooth_preset"),
        preset_popup,
    );
    ui.smooth_scrolling_preset = preset_control;
    bind_control(target, preset_control);
    SettingsRow::separator_above_row(mouse_view, preset_y, preset_metrics.row_h, content_w);

    let smooth_defaults = crate::mouse::smooth::presets::SmoothPreset::EaseInOut.default_settings();
    let slider_specs = [
        (
            "settings.row_mouse_smooth_response",
            0.0,
            2.0,
            smooth_defaults.0,
        ),
        (
            "settings.row_mouse_smooth_speed",
            0.0,
            8.0,
            smooth_defaults.1,
        ),
        (
            "settings.row_mouse_smooth_acceleration",
            0.0,
            8.0,
            smooth_defaults.2,
        ),
        (
            "settings.row_mouse_smooth_inertia",
            0.0,
            8.0,
            smooth_defaults.3,
        ),
    ];
    let mut smooth_controls = Vec::with_capacity(4);
    for (key, minimum, maximum, default_value) in slider_specs {
        let y = canvas.next_row(SettingsLayout::SINGLE_LINE_ROW_H);
        SettingsRow::separator_above_row(
            mouse_view,
            y,
            SettingsLayout::SINGLE_LINE_ROW_H,
            content_w,
        );
        let slider = SettingsControl::double_slider(
            ctrl_x,
            y + 10.0,
            SettingsRow::slider_width(ctrl_w),
            row_h,
            minimum,
            maximum,
            default_value,
            Some(default_value),
        );
        record_settings_search_row(SettingsPageId::Mouse, canvas.last_row(), &t(key), None);
        let (_, slider) = SettingsRow::tall_with_height(
            mouse_view,
            label_x,
            y,
            label_w,
            SettingsLayout::SINGLE_LINE_ROW_H,
            &t(key),
            slider,
        );
        let value_label =
            SettingsRow::attach_slider_readout(mouse_view, slider, format!("{default_value:.1}"));
        bind_control(target, slider);
        smooth_controls.push((slider, value_label));
    }
    ui.smooth_scrolling_response = smooth_controls[0].0;
    ui.smooth_scrolling_response_value = smooth_controls[0].1;
    ui.smooth_scrolling_speed = smooth_controls[1].0;
    ui.smooth_scrolling_speed_value = smooth_controls[1].1;
    ui.smooth_scrolling_acceleration = smooth_controls[2].0;
    ui.smooth_scrolling_acceleration_value = smooth_controls[2].1;
    ui.smooth_scrolling_inertia = smooth_controls[3].0;
    ui.smooth_scrolling_inertia_value = smooth_controls[3].1;
    canvas.group_end();
    canvas.card(&t("settings.header_mouse_smooth_scrolling"));
    set_settings_search_section(&t("settings.header_mouse_smooth_scrolling"));

    canvas.next_section();
    let caption = t("settings.desc_disable_pointer_accel");
    let caption_w = ctrl_x - label_x - super::SETTINGS_CONTROL_LABEL_GAP;
    let caption_row_h = widgets::captioned_row_height(&caption, caption_w);
    let y = canvas.next_row(caption_row_h);
    // Keep the explanation in the caption instead of lengthening the setting label.
    record_settings_search_row(
        SettingsPageId::Mouse,
        canvas.last_row(),
        &t("settings.row_disable_pointer_accel"),
        Some(&caption),
    );
    let (_, _, pointer_accel_switch) = SettingsRow::captioned(
        mouse_view,
        label_x,
        y,
        caption_w,
        caption_row_h,
        &t("settings.row_disable_pointer_accel"),
        &caption,
        SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
    );
    ui.disable_pointer_accel = pointer_accel_switch;
    bind_control(target, ui.disable_pointer_accel);

    // Tracking speed (shown only while "Disable pointer acceleration (linear tracking)" is
    // on). Under linear tracking HIDPointerAcceleration *is* the tracking speed; with the
    // switch off that property is the acceleration curve's strength, a different meaning, so
    // this row only appears while the switch is on (see the module comment in
    // mouse/pointer.rs). A continuous 0..=40 slider (no tick snapping) plus a read-only value
    // on the right.
    // The separator takes the y of the row BELOW the line (SettingsRow::separator draws it
    // 3pt above that row's top edge); any other y puts it at the card's top, which is what
    // the device card's internal dividers rely on too.
    // The tracking-speed row only appears while pointer acceleration is off: one row group the page
    // layout owner skips.
    canvas.group_begin(RowGroup::PointerAccel);
    let y = canvas.next_row(described_row_h);
    SettingsRow::separator_above_row(mouse_view, y, described_row_h, content_w);
    record_settings_search_row(
        SettingsPageId::Mouse,
        canvas.last_row(),
        &t("settings.row_pointer_tracking_speed"),
        None,
    );
    let (pointer_accel_label, pointer_accel_slider) = SettingsRow::tall_with_height(
        mouse_view,
        label_x,
        y,
        label_w,
        described_row_h,
        &t("settings.row_pointer_tracking_speed"),
        // Leaves the readout's width on the right for the read-only value label (same layout
        // as the line-count row, see SettingsRow::slider_width).
        SettingsControl::double_slider(
            ctrl_x,
            y + 10.0,
            SettingsRow::slider_width(ctrl_w),
            row_h,
            crate::config::MOUSE_ACCELERATION_MIN,
            crate::config::MOUSE_ACCELERATION_MAX,
            crate::mouse::pointer::FALLBACK_ACCELERATION,
            // Double-click restores the default tracking speed (1.00 = macOS's factory default
            // for the mouse key, i.e. what the pointer felt like before this setting existed).
            Some(crate::mouse::pointer::FALLBACK_ACCELERATION),
        ),
    );
    ui.pointer_accel_label = pointer_accel_label;
    ui.pointer_accel_slider = pointer_accel_slider;
    // Read-only value label right of the slider: shows the value on release (2 decimals).
    ui.pointer_accel_value_label = SettingsRow::attach_slider_readout(
        mouse_view,
        pointer_accel_slider,
        pointer_accel_display(crate::mouse::pointer::FALLBACK_ACCELERATION),
    );
    bind_control(target, ui.pointer_accel_slider);
    canvas.group_end();

    canvas.card(&t("settings.header_mouse_pointer"));
    set_settings_search_section(&t("settings.header_mouse_pointer"));

    // Button mappings: an "Enable button mappings" described row + a nested table card
    // (rounded sub-table + the add-mapping button).
    canvas.next_section();
    // "Enable button mappings" described row (HTML card top), replacing the old switch
    // that sat on the section-header row's right edge.
    let y = canvas.next_row(described_row_h);
    record_settings_search_row(
        SettingsPageId::Mouse,
        canvas.last_row(),
        &t("settings.row_mapping_enable"),
        None,
    );
    ui.mapping_enabled = SettingsRow::described(
        mouse_view,
        label_x,
        y,
        ctrl_x - label_x - super::SETTINGS_CONTROL_LABEL_GAP,
        described_row_h,
        &t("settings.row_mapping_enable"),
        &t("settings.desc_mapping_enable"),
        SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
    );
    let _: () = msg_send![ui.mapping_enabled, setTarget: target];
    let _: () = msg_send![ui.mapping_enabled, setAction: sel!(handleMappingEnabledChanged:)];
    canvas.card(&t("settings.header_mouse_mappings"));
    set_settings_search_section(&t("settings.header_mouse_mappings"));

    // A 24pt gap under the section card, then the binding table itself: a block the canvas owns
    // (the mapping list resizes it as its rows change, see `render_mapping_rows_locked`). The
    // block's origin is the card's bottom edge, so its top stays on the gap and it grows downward.
    canvas.next_block(24.0);
    let card_w = content_w;
    let card_h = MAPPING_PANEL_TOP
        + (MAPPING_HEADER_H + MAPPING_ROW_H * 3.0)
        + MAPPING_ACTION_TOP
        + MAPPING_ACTION_H
        + MAPPING_CARD_PAD_BOT;
    let card_bottom = canvas.next_block(card_h);
    ui.mapping_layout_row = canvas.last_row();
    // The outer card is a white settings card (same as every other card); only the nested
    // table and the add button carry the gray "dark" treatment from the HTML reference.
    let card_bg: *mut AnyObject = msg_send![class!(NSView), alloc];
    // Align the mapping card with the other settings cards; the nested table keeps its own
    // inset so only the outer border expands to the shared content width.
    let card_bg: *mut AnyObject = msg_send![card_bg, initWithFrame: NSRect::new(NSPoint::new(0.0, card_bottom), NSSize::new(content_w, card_h))];
    let _: () = msg_send![card_bg, setFlipped: true];
    let _: () = msg_send![card_bg, setAutoresizingMask: 0u64];
    let _: () = msg_send![card_bg, setWantsLayer: true];
    let bg_layer: *mut AnyObject = msg_send![card_bg, layer];
    let _: () = msg_send![bg_layer, setCornerRadius: crate::theme::RADIUS_CARD];
    let _: () = msg_send![bg_layer, setMasksToBounds: true];
    let palette = settings_palette();
    crate::ffi::layer_set_background(bg_layer, crate::ffi::hex_to_cg_color(palette.card_bg));
    crate::ffi::layer_set_border(bg_layer, crate::ffi::hex_to_cg_color(palette.card_border));
    let _: () = msg_send![bg_layer, setBorderWidth: 1.0f64];
    // The nested `.mapping-table`: a rounded, bordered sub-panel behind the rows, giving
    // the bindings the HTML reference's table look.
    let panel: *mut AnyObject = msg_send![class!(NSView), alloc];
    let panel: *mut AnyObject = msg_send![panel, initWithFrame: NSRect::new(NSPoint::new(MAPPING_PANEL_X, MAPPING_PANEL_TOP), NSSize::new(card_w - 2.0 * MAPPING_PANEL_X, MAPPING_HEADER_H + MAPPING_ROW_H * 3.0))];
    let _: () = msg_send![panel, setWantsLayer: true];
    let panel_layer: *mut AnyObject = msg_send![panel, layer];
    let _: () = msg_send![panel_layer, setCornerRadius: crate::theme::RADIUS_CONTROL];
    let _: () = msg_send![panel_layer, setMasksToBounds: true];
    crate::ffi::layer_set_background(panel_layer, crate::ffi::hex_to_cg_color(palette.field_bg));
    crate::ffi::layer_set_border(
        panel_layer,
        crate::ffi::hex_to_cg_color(palette.card_border),
    );
    let _: () = msg_send![panel_layer, setBorderWidth: 1.0f64];
    let _: () = msg_send![card_bg, addSubview: panel];
    ui.mapping_panel = panel;
    release_obj(panel);
    // The header band (.mapping-table thead).
    let header_color = settings_text_color(SettingsTextRole::Secondary);
    let header_font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CAPTION, weight: crate::theme::FONT_WEIGHT_SEMIBOLD];
    for (hx, hw, htext) in [
        (
            MAPPING_PANEL_X + MAPPING_CELL_X,
            120.0,
            t("settings.mapping_column_button"),
        ),
        (
            MAPPING_PANEL_X + MAPPING_CELL_X + 80.0,
            130.0,
            t("settings.mapping_column_action"),
        ),
    ] {
        let hlabel: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let hlabel: *mut AnyObject = msg_send![hlabel, initWithFrame: NSRect::new(NSPoint::new(hx, MAPPING_PANEL_TOP + 7.0), NSSize::new(hw, 18.0))];
        let hns = make_nsstring(&htext);
        let _: () = msg_send![hlabel, setStringValue: hns];
        CFRelease(hns as *const c_void);
        let _: () = msg_send![hlabel, setBezeled: false];
        let _: () = msg_send![hlabel, setDrawsBackground: false];
        let _: () = msg_send![hlabel, setEditable: false];
        let _: () = msg_send![hlabel, setFont: header_font];
        let _: () = msg_send![hlabel, setTextColor: header_color];
        let _: () = msg_send![card_bg, addSubview: hlabel];
        release_obj(hlabel);
    }
    // Hairline under the header band.
    let header_line: *mut AnyObject = msg_send![class!(NSView), alloc];
    let header_line: *mut AnyObject = msg_send![header_line, initWithFrame: NSRect::new(NSPoint::new(MAPPING_PANEL_X + MAPPING_CELL_X, MAPPING_PANEL_TOP + MAPPING_HEADER_H - 1.0), NSSize::new(card_w - 2.0 * (MAPPING_PANEL_X + MAPPING_CELL_X), 1.0))];
    let _: () = msg_send![header_line, setWantsLayer: true];
    let header_line_layer: *mut AnyObject = msg_send![header_line, layer];
    let header_line_color: *mut AnyObject = msg_send![class!(NSColor), separatorColor];
    layer_set_background(header_line_layer, ns_color_to_cg(header_line_color));
    let _: () = msg_send![card_bg, addSubview: header_line];
    release_obj(header_line);
    // Empty-state hint (inside the sub-table when there are no rows).
    let empty: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let empty: *mut AnyObject = msg_send![empty, initWithFrame: NSRect::new(NSPoint::new(MAPPING_PANEL_X + MAPPING_CELL_X, MAPPING_PANEL_TOP + MAPPING_HEADER_H + (MAPPING_ROW_H * 3.0) / 2.0 - 9.0), NSSize::new(card_w - 2.0 * (MAPPING_PANEL_X + MAPPING_CELL_X), 18.0))];
    set_field(empty, 0);
    let _: () = msg_send![empty, setBezeled: false];
    let _: () = msg_send![empty, setDrawsBackground: false];
    let _: () = msg_send![empty, setEditable: false];
    let empty_font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CAPTION];
    let _: () = msg_send![empty, setFont: empty_font];
    let _: () = msg_send![empty, setAlignment: 1isize]; // center
    let empty_ns = make_nsstring(&t("settings.mapping_empty"));
    let _: () = msg_send![empty, setStringValue: empty_ns];
    CFRelease(empty_ns as *const c_void);
    let empty_color = settings_text_color(SettingsTextRole::Muted);
    let _: () = msg_send![empty, setTextColor: empty_color];
    let _: () = msg_send![empty, setHidden: true];
    let _: () = msg_send![card_bg, addSubview: empty];
    release_obj(empty);
    ui.mapping_empty = empty;
    // Add-mapping button: full-width action row at the card bottom.
    let add_btn = SettingsButton::action(
        NSRect::new(
            NSPoint::new(
                MAPPING_PANEL_X,
                MAPPING_PANEL_TOP + MAPPING_HEADER_H + MAPPING_ROW_H * 3.0 + MAPPING_ACTION_TOP,
            ),
            NSSize::new(card_w - 2.0 * MAPPING_PANEL_X, MAPPING_ACTION_H),
        ),
        &t("settings.row_add_mapping"),
        target,
        sel!(handleAddMapping:),
        // A full-width row action on the card surface, not a field: it needs the doc's secondary
        // button surface (and with it a perceptible hover). `Compact`'s field-family wash would leave
        // the hover at 1.02:1 on `card_bg` (see `SettingsButtonRole::ROW_ACTION`).
        SettingsButtonRole::ROW_ACTION,
    );
    let _: () = msg_send![card_bg, addSubview: add_btn];
    release_obj(add_btn);
    ui.add_mapping_button = add_btn;
    let _: () = msg_send![mouse_view, addSubview: card_bg];
    release_obj(card_bg);
    ui.mapping_card = card_bg;
    ui.mapping_scroll = std::ptr::null_mut();
    ui.mapping_doc = card_bg;
    // Settle the page and publish it before the mapping rows render: the table's block has to be
    // part of the layout for `render_mapping_rows` to hand over its height.
    sweep_unindexed_rows(&canvas, SettingsPageId::Mouse);
    let content_bottom = canvas.finish();
    ui.page_canvases[SETTINGS_MOUSE_PAGE_INDEX] = canvas;
    // Render the current device's mappings initially.
    render_mapping_rows();

    content_bottom
}

/// Build the Clipboard page and return its final options-card bottom.
pub(super) unsafe fn build_clipboard_page(
    context: &SettingsPageBuildContext,
    clipboard_view: *mut AnyObject,
    clipboard_doc_h: f64,
    ui: &mut SettingsUi,
) -> f64 {
    let content_w = context.content_w;
    let layout = context.layout;
    let target = context.target;
    let label_x = layout.label_x;
    let label_w = layout.label_w;
    let ctrl_w = layout.control_w;
    let ctrl_x = layout.control_x;
    let row_h = layout.row_h;
    let described_row_h = layout.described_row_h;
    // The page's layout owner: rows, cards, the title and the document height all follow from it.
    // The page's rows fall back to this page's title as their section until a card sets one.
    begin_settings_search_page();
    let mut canvas = PageCanvas::new(
        clipboard_view,
        content_w,
        layout,
        &t("settings.sidebar_clipboard"),
        clipboard_doc_h,
        context.page_frame.size.height,
    );
    let caption = t("settings.desc_clipboard_enabled");
    let caption_w = ctrl_x - label_x - super::SETTINGS_CONTROL_LABEL_GAP;
    let caption_row_h = widgets::captioned_row_height(&caption, caption_w);
    let cy = canvas.next_row(caption_row_h);
    // English "Enable clipboard history" (measured 146pt) plus cell padding sits on
    // the label_w=150 edge; widen to 225 along with the clear_on_quit/move_used_to_top rows.
    // `captioned`, not `described`: the latter's subtitle is not rendered at all, and this row has to
    // state that turning the feature off deletes the saved records -- that line is the only warning
    // the user gets, so it must actually be drawn (asserted by the settings layout smoke).
    record_settings_search_row(
        SettingsPageId::Clipboard,
        canvas.last_row(),
        &t("settings.row_clipboard_enabled"),
        Some(&caption),
    );
    let (_, _, ui_clipboard_enabled) = SettingsRow::captioned(
        clipboard_view,
        label_x,
        cy,
        caption_w,
        caption_row_h,
        &t("settings.row_clipboard_enabled"),
        &caption,
        SettingsControl::switch(ctrl_x + ctrl_w, cy, row_h, false),
    );
    ui.clipboard_enabled = ui_clipboard_enabled;
    let _: () = msg_send![ui.clipboard_enabled, setTarget: target];
    let _: () = msg_send![
        ui.clipboard_enabled,
        setAction: sel!(handleClipboardEnabledToggle:)
    ];
    // Pause recording. A persisted switch, unlike the panel pin: a pause is a standing intent, so
    // a restart must not silently resume collecting. While it is on, the poll timer keeps running
    // and advances the changeCount baseline, so nothing copied during the pause is recorded later.
    let pause_caption = t("settings.desc_clipboard_paused");
    let pause_row_h = widgets::captioned_row_height(&pause_caption, caption_w);
    let cy = canvas.next_row(pause_row_h);
    // Second row of its card: it needs the same hairline the rest of the page's rows carry, or the
    // two switches read as one block.
    SettingsRow::separator_above_row(clipboard_view, cy, pause_row_h, content_w);
    record_settings_search_row(
        SettingsPageId::Clipboard,
        canvas.last_row(),
        &t("settings.row_clipboard_paused"),
        Some(&pause_caption),
    );
    let (_, _, ui_clipboard_paused) = SettingsRow::captioned(
        clipboard_view,
        label_x,
        cy,
        caption_w,
        pause_row_h,
        &t("settings.row_clipboard_paused"),
        &pause_caption,
        SettingsControl::switch(ctrl_x + ctrl_w, cy, row_h, false),
    );
    ui.clipboard_paused = ui_clipboard_paused;
    let _: () = msg_send![ui.clipboard_paused, setTarget: target];
    let _: () = msg_send![ui.clipboard_paused, setAction: sel!(handlePauseRecordingToggle:)];
    canvas.card(&t("settings.header_clipboard"));
    set_settings_search_section(&t("settings.header_clipboard"));
    // Keep the history controls in a second titled card, matching the switcher layout.
    canvas.next_section();
    let active_shortcut = crate::config::CONFIG
        .read()
        .unwrap()
        .clipboard
        .shortcut
        .clone();
    let shortcut_row_h = (row_h + 22.0).max(60.0);
    let cy = canvas.next_row(shortcut_row_h);
    ui.clipboard_shortcut_row_height = shortcut_row_h;
    record_settings_search_row(
        SettingsPageId::Clipboard,
        canvas.last_row(),
        &t("settings.row_clipboard_shortcut"),
        None,
    );
    ui.clipboard_shortcut = SettingsRow::plain(
        clipboard_view,
        label_x,
        cy,
        220.0,
        shortcut_row_h,
        &t("settings.row_clipboard_shortcut"),
        SettingsControl::button(
            ctrl_x + ctrl_w - ROW_ACTION_BTN_W,
            cy + (shortcut_row_h - ROW_ACTION_BTN_H) / 2.0,
            ROW_ACTION_BTN_W,
            ROW_ACTION_BTN_H,
            &crate::mouse::shortcut::display_shortcut(&active_shortcut),
            target,
            sel!(handleClipboardShortcutRecord:),
            SettingsButtonRole::ControlField,
        ),
    );
    ui.clipboard_shortcut_error =
        make_value_label(label_x, cy + 2.0, content_w - label_x - 12.0, 18.0, "");
    apply_settings_text_role(ui.clipboard_shortcut_error, SettingsTextRole::Destructive);
    let _: () = msg_send![ui.clipboard_shortcut_error, setHidden: true];
    let _: () = msg_send![clipboard_view, addSubview: ui.clipboard_shortcut_error];
    release_obj(ui.clipboard_shortcut_error);
    let shortcut_tooltip = make_nsstring(&t("settings.desc_clipboard_shortcut"));
    let _: () = msg_send![ui.clipboard_shortcut, setToolTip: shortcut_tooltip];
    release_obj(shortcut_tooltip);
    // Pin-selection popup: items = [Follow the Pinned Entry, Keep Current Position];
    // default index 0 (follow); the real value is set by load_settings_from.
    let pin_labels = [
        t("settings.pin_follow_entry"),
        t("settings.pin_keep_position"),
    ];
    let pin_label_refs: Vec<&str> = pin_labels.iter().map(|s| s.as_str()).collect();
    let pin_metrics = SettingsSelect::metrics();
    ui.clipboard_pin_follow_row_height = pin_metrics.row_h;
    ui.clipboard_row_gap = layout.row_gap;
    let cy = canvas.next_row(pin_metrics.row_h);
    SettingsRow::separator_above_row(clipboard_view, cy, pin_metrics.row_h, content_w);
    record_settings_search_row(
        SettingsPageId::Clipboard,
        canvas.last_row(),
        &t("settings.row_clipboard_pin_follow"),
        None,
    );
    record_settings_search_options(&pin_label_refs);
    ui.clipboard_pin_follow = SettingsRow::plain(
        clipboard_view,
        label_x,
        cy,
        220.0,
        pin_metrics.row_h,
        &t("settings.row_clipboard_pin_follow"),
        SettingsControl::popup(
            ctrl_x,
            cy + (pin_metrics.row_h - pin_metrics.control_h) / 2.0,
            ctrl_w,
            pin_metrics.control_h,
            &pin_label_refs,
            0,
        ),
    );
    bind_control(target, ui.clipboard_pin_follow);
    let cy = canvas.next_row(described_row_h);
    SettingsRow::separator_above_row(clipboard_view, cy, described_row_h, content_w);
    // Persist switch (saved to disk, survives restarts; plaintext on disk -- the
    // privacy implications are documented in the README).
    // Clear-on-quit switch (history is written to disk while the app runs; this is the way to leave
    // nothing behind after a session). Measured at the row's 14pt label font: en "Clear history on
    // quit or shutdown" 214.2pt, zh-Hans 194.5pt, zh-Hant 222.3pt -- all past the default
    // label_w=150, and the Traditional label also past the 220 this row used before, so it reserves
    // 236 (the control column starts ~150pt further right, so nothing is squeezed).
    record_settings_search_row(
        SettingsPageId::Clipboard,
        canvas.last_row(),
        &t("settings.row_clipboard_clear_on_quit"),
        None,
    );
    ui.clipboard_clear_on_quit = SettingsRow::plain(
        clipboard_view,
        label_x,
        cy,
        236.0,
        described_row_h,
        &t("settings.row_clipboard_clear_on_quit"),
        SettingsControl::switch(ctrl_x + ctrl_w, cy, row_h, false),
    );
    bind_control(target, ui.clipboard_clear_on_quit);
    let cy = canvas.next_row(described_row_h);
    SettingsRow::separator_above_row(clipboard_view, cy, described_row_h, content_w);
    // Opt-in second header button in the history picker ("clear all"): it also takes the pinned
    // entries of the visible scope, so it stays off by default. The plain clear button never
    // touches pinned entries either way.
    record_settings_search_row(
        SettingsPageId::Clipboard,
        canvas.last_row(),
        &t("settings.row_clipboard_separate_clear_all"),
        None,
    );
    ui.clipboard_separate_clear_all = SettingsRow::plain(
        clipboard_view,
        label_x,
        cy,
        236.0,
        described_row_h,
        &t("settings.row_clipboard_separate_clear_all"),
        SettingsControl::switch(ctrl_x + ctrl_w, cy, row_h, false),
    );
    bind_control(target, ui.clipboard_separate_clear_all);
    let cy = canvas.next_row(described_row_h);
    SettingsRow::separator_above_row(clipboard_view, cy, described_row_h, content_w);
    // show the source app.
    record_settings_search_row(
        SettingsPageId::Clipboard,
        canvas.last_row(),
        &t("settings.row_clipboard_show_source_app"),
        None,
    );
    ui.clipboard_show_source_app = SettingsRow::plain(
        clipboard_view,
        label_x,
        cy,
        label_w,
        described_row_h,
        &t("settings.row_clipboard_show_source_app"),
        SettingsControl::switch(ctrl_x + ctrl_w, cy, row_h, false),
    );
    bind_control(target, ui.clipboard_show_source_app);
    let cy = canvas.next_row(described_row_h);
    SettingsRow::separator_above_row(clipboard_view, cy, described_row_h, content_w);
    // Move used entries to the top (whether pasting reorders the history; on by
    // default = current behavior).
    // English "Move used entries to top" (measured 150.3pt) exceeds label_w=150 and
    // rendered truncated ("move used entries to" after switching to English), widened
    // to 225.
    record_settings_search_row(
        SettingsPageId::Clipboard,
        canvas.last_row(),
        &t("settings.row_clipboard_move_used_to_top"),
        None,
    );
    ui.clipboard_move_used_to_top = SettingsRow::plain(
        clipboard_view,
        label_x,
        cy,
        220.0,
        described_row_h,
        &t("settings.row_clipboard_move_used_to_top"),
        SettingsControl::switch(ctrl_x + ctrl_w, cy, row_h, false),
    );
    bind_control(target, ui.clipboard_move_used_to_top);
    let caption = t("settings.desc_clipboard_delete_after_paste");
    let caption_w = ctrl_x - label_x - super::SETTINGS_CONTROL_LABEL_GAP;
    let caption_row_h = widgets::captioned_row_height(&caption, caption_w);
    let cy = canvas.next_row(caption_row_h);
    SettingsRow::separator_above_row(clipboard_view, cy, caption_row_h, content_w);
    // The destructive shortcut remains opt-in; its gesture hint lives in the muted caption.
    record_settings_search_row(
        SettingsPageId::Clipboard,
        canvas.last_row(),
        &t("settings.row_clipboard_delete_after_paste"),
        None,
    );
    let (_, _, ui_control) = SettingsRow::captioned(
        clipboard_view,
        label_x,
        cy,
        caption_w,
        caption_row_h,
        &t("settings.row_clipboard_delete_after_paste"),
        &caption,
        SettingsControl::switch(ctrl_x + ctrl_w, cy, row_h, false),
    );
    ui.clipboard_delete_after_paste = ui_control;
    bind_control(target, ui.clipboard_delete_after_paste);
    // This row is a child of the switch above (indented label): it only appears while "delete
    // entry after paste" is on, so it belongs to a row group the page layout owner skips (the row
    // disappears and the rows below close its gap) rather than being greyed out. The group opens
    // before the row is placed: `next_row` records the group its row belongs to.
    canvas.group_begin(RowGroup::ClipboardDeleteChild);
    let cy = canvas.next_row(described_row_h);
    SettingsRow::separator_above_row(clipboard_view, cy, described_row_h, content_w);
    record_settings_search_row(
        SettingsPageId::Clipboard,
        canvas.last_row(),
        &t("settings.row_clipboard_clear_system_pasteboard_after_paste"),
        None,
    );
    let (_, clear_pasteboard_switch) = SettingsRow::tall_with_height(
        clipboard_view,
        label_x + 18.0,
        cy,
        ctrl_x - label_x - 36.0,
        described_row_h,
        &t("settings.row_clipboard_clear_system_pasteboard_after_paste"),
        SettingsControl::switch(ctrl_x + ctrl_w, cy, row_h, false),
    );
    ui.clipboard_clear_system_pasteboard_after_paste = clear_pasteboard_switch;
    bind_control(target, ui.clipboard_clear_system_pasteboard_after_paste);
    canvas.group_end();
    let cy = canvas.next_row(described_row_h);
    SettingsRow::separator_above_row(clipboard_view, cy, described_row_h, content_w);
    // max entries (number input).
    record_settings_search_row(
        SettingsPageId::Clipboard,
        canvas.last_row(),
        &t("settings.row_clipboard_max_entries"),
        None,
    );
    ui.clipboard_max_entries = SettingsRow::plain(
        clipboard_view,
        label_x,
        cy,
        label_w,
        described_row_h,
        &t("settings.row_clipboard_max_entries"),
        SettingsControl::text_input(ctrl_x, cy, ctrl_w, row_h, "50"),
    );
    let cy = canvas.next_row(described_row_h);
    SettingsRow::separator_above_row(clipboard_view, cy, described_row_h, content_w);
    // Auto-expire days slider: 0..=7, where 0 means never; the current value is shown on
    // the right.
    record_settings_search_row(
        SettingsPageId::Clipboard,
        canvas.last_row(),
        &t("settings.row_clipboard_auto_expire_days"),
        None,
    );
    let (_, auto_expire_slider) = SettingsRow::tall(
        clipboard_view,
        label_x,
        cy,
        label_w,
        &t("settings.row_clipboard_auto_expire_days"),
        SettingsControl::stepped_slider(
            ctrl_x,
            cy + 10.0,
            SettingsRow::slider_width(ctrl_w),
            row_h,
            CLIPBOARD_AUTO_EXPIRE_MIN,
            CLIPBOARD_AUTO_EXPIRE_MAX,
            1,
            CLIPBOARD_AUTO_EXPIRE_DEFAULT,
            // Double-click restores the default (3 days).
            Some(CLIPBOARD_AUTO_EXPIRE_DEFAULT as f64),
        ),
    );
    ui.clipboard_auto_expire_days = auto_expire_slider;
    ui.clipboard_auto_expire_days_value_label = SettingsRow::attach_slider_readout(
        clipboard_view,
        auto_expire_slider,
        CLIPBOARD_AUTO_EXPIRE_DEFAULT,
    );
    bind_control(target, ui.clipboard_auto_expire_days);
    let cy = canvas.next_row(described_row_h);
    SettingsRow::separator_above_row(clipboard_view, cy, described_row_h, content_w);
    // Image row height: how many text-row heights an image entry occupies in the picker
    // (1 = thumbnail row like today, 2/3 = a taller preview row).
    record_settings_search_row(
        SettingsPageId::Clipboard,
        canvas.last_row(),
        &t("settings.row_clipboard_image_rows"),
        None,
    );
    let (_, image_rows_slider) = SettingsRow::tall(
        clipboard_view,
        label_x,
        cy,
        label_w,
        &t("settings.row_clipboard_image_rows"),
        SettingsControl::stepped_slider(
            ctrl_x,
            cy + 10.0,
            SettingsRow::slider_width(ctrl_w),
            row_h,
            1,
            3,
            1,
            1,
            // Double-click restores the default (1 row).
            Some(1.0),
        ),
    );
    ui.clipboard_image_rows = image_rows_slider;
    ui.clipboard_image_rows_value_label =
        SettingsRow::attach_slider_readout(clipboard_view, image_rows_slider, 1);
    bind_control(target, ui.clipboard_image_rows);
    canvas.card(&t("settings.header_clipboard_options"));
    set_settings_search_section(&t("settings.header_clipboard_options"));

    // ---- Excluded applications ----
    // Copies made by these apps are never recorded (matched inside the poll, before the payload is
    // read). The list is a card of its own: it grows and shrinks, so it must not share a card with
    // the fixed rows above. next_section opens the group and card closes it over the rows, which is
    // the order every other page uses -- calling card first would re-title the rows above.
    canvas.next_section();
    let excluded = crate::config::CONFIG
        .read()
        .unwrap()
        .clipboard
        .excluded_apps
        .clone();
    // One row height for the whole card: it follows the binding list's single-line row (tall enough
    // for a 28pt row action with breathing room), not the 32pt control height the page's own rows
    // use. The list rows AND the controls under them share it, so the card reads as one rhythm.
    let excluded_row_h = described_row_h;
    if excluded.is_empty() {
        let cy = canvas.next_row(excluded_row_h);
        record_settings_search_row(
            SettingsPageId::Clipboard,
            canvas.last_row(),
            &t("settings.clipboard_excluded_empty"),
            None,
        );
        let label = make_value_label(
            label_x,
            cy + 10.0,
            content_w - label_x - 12.0,
            18.0,
            &t("settings.clipboard_excluded_empty"),
        );
        apply_settings_text_role(label, SettingsTextRole::Secondary);
        let _: () = msg_send![clipboard_view, addSubview: label];
        release_obj(label);
    } else {
        let last = excluded.len().saturating_sub(1);
        for (index, entry) in excluded.iter().enumerate() {
            let cy = canvas.next_row(excluded_row_h);
            // The row style follows the Key Bindings list: a name and one trailing row action, with a
            // divider between rows instead of a bordered control per row. The LAST row hands that
            // boundary to the row below it (the add control draws its own), so the card never carries
            // two overlapping lines at one edge.
            if index < last {
                SettingsRow::separator(clipboard_view, cy, content_w);
            }
            // The display name is a snapshot taken when the entry was added; the identifier is the
            // only matching key and is shown when the snapshot is missing.
            let display = if entry.name.is_empty() {
                entry.bundle_id.clone()
            } else {
                entry.name.clone()
            };
            record_settings_search_row(
                SettingsPageId::Clipboard,
                canvas.last_row(),
                &display,
                None,
            );
            let btn_y = cy + (excluded_row_h - bindings::BINDING_BTN_H) / 2.0;
            let delete = SettingsButton::action(
                NSRect::new(
                    NSPoint::new(ctrl_x + ctrl_w - bindings::BINDING_BTN_W, btn_y),
                    NSSize::new(bindings::BINDING_BTN_W, bindings::BINDING_BTN_H),
                ),
                &t("settings.clipboard_excluded_remove"),
                target,
                sel!(handleClipboardExcludedRemove:),
                SettingsButtonRole::ROW_ACTION,
            );
            // The returned control is the remove button: the handler finds the entry by comparing
            // the sender pointer against this list.
            // `tall_before_control`, not `plain`: the tall row measures its label for a single-line
            // row, which is what keeps the name and the 28pt row action on one centre line. `plain`
            // lays the label out for the page's 32pt control rows and sits low in a 56pt row.
            let (_, remove) = SettingsRow::tall_before_control(
                clipboard_view,
                label_x,
                cy,
                ctrl_x + ctrl_w - bindings::BINDING_BTN_W,
                super::SETTINGS_CONTROL_LABEL_GAP,
                &display,
                delete,
            );
            ui.clipboard_excluded_remove.push(remove);
        }
    }
    // Add a running app. The candidates are captured here so the handler can map the popup's
    // selection index back to an identifier without re-enumerating (and re-sorting) running apps
    // between the click and the callback.
    let running = crate::app_identity::running_apps_with_bundle_ids();
    let listed: Vec<String> = excluded
        .iter()
        .map(|e| e.bundle_id.to_ascii_lowercase())
        .collect();
    let candidates: Vec<(String, String)> = running
        .into_iter()
        .filter(|(bundle_id, _)| !listed.contains(&bundle_id.to_ascii_lowercase()))
        .collect();
    // ONE add control. The popup lists the running candidates and carries a trailing "choose from
    // disk" entry, the macOS "Other…" pattern: the same intent, reached from one place, instead of
    // two rows that both mean "exclude an application".
    let mut add_labels: Vec<String> = vec![t("settings.clipboard_excluded_add_placeholder")];
    add_labels.extend(candidates.iter().map(|(_, name)| name.clone()));
    add_labels.push(t("settings.clipboard_excluded_choose_from_disk"));
    let add_label_refs: Vec<&str> = add_labels.iter().map(String::as_str).collect();
    let add_metrics = SettingsSelect::metrics();
    let cy = canvas.next_row(excluded_row_h);
    // The list's last row handed this boundary over, so this is the one and only line here.
    SettingsRow::separator_above_row(clipboard_view, cy, excluded_row_h, content_w);
    record_settings_search_row(
        SettingsPageId::Clipboard,
        canvas.last_row(),
        &t("settings.row_clipboard_excluded_add"),
        None,
    );
    let add_popup = SettingsControl::popup(
        ctrl_x,
        cy + (excluded_row_h - add_metrics.control_h) / 2.0,
        ctrl_w,
        add_metrics.control_h,
        &add_label_refs,
        0,
    );
    style_flat_popup(add_popup);
    let _: () = msg_send![add_popup, setTarget: target];
    let _: () = msg_send![add_popup, setAction: sel!(handleClipboardExcludedAdd:)];
    ui.clipboard_excluded_add = SettingsRow::tall_before_control(
        clipboard_view,
        label_x,
        cy,
        ctrl_x,
        super::SETTINGS_CONTROL_LABEL_GAP,
        &t("settings.row_clipboard_excluded_add"),
        add_popup,
    )
    .1;
    ui.clipboard_excluded_candidates = candidates;
    // (The disk picker lives inside the add popup above, as its trailing entry.)
    // Restore the built-in list.
    let cy = canvas.next_row(excluded_row_h);
    SettingsRow::separator_above_row(clipboard_view, cy, excluded_row_h, content_w);
    record_settings_search_row(
        SettingsPageId::Clipboard,
        canvas.last_row(),
        &t("settings.row_clipboard_excluded_restore"),
        None,
    );
    ui.clipboard_excluded_restore = SettingsRow::tall_before_control(
        clipboard_view,
        label_x,
        cy,
        ctrl_x + ctrl_w - ROW_ACTION_BTN_W,
        super::SETTINGS_CONTROL_LABEL_GAP,
        &t("settings.row_clipboard_excluded_restore"),
        SettingsControl::button(
            ctrl_x + ctrl_w - ROW_ACTION_BTN_W,
            cy + (excluded_row_h - ROW_ACTION_BTN_H) / 2.0,
            ROW_ACTION_BTN_W,
            ROW_ACTION_BTN_H,
            &t("settings.clipboard_excluded_restore_action"),
            target,
            sel!(handleClipboardExcludedRestore:),
            SettingsButtonRole::ControlField,
        ),
    )
    .1;

    canvas.card(&t("settings.header_clipboard_excluded"));
    set_settings_search_section(&t("settings.header_clipboard_excluded"));

    sweep_unindexed_rows(&canvas, SettingsPageId::Clipboard);
    let content_bottom = canvas.finish();
    ui.page_canvases[SETTINGS_CLIPBOARD_PAGE_INDEX] = canvas;
    content_bottom
}

/// Build the Key Bindings page: one card holding the master switch, the editable binding list and
/// the add row. Returns the add card's bottom.
///
/// The rows themselves are rendered by `settings::bindings` from the config; this builder creates
/// only the furniture around them and hands the canvas one block whose height the renderer owns (it
/// re-flows the page as rows come and go).
pub(super) unsafe fn build_key_bindings_page(
    context: &SettingsPageBuildContext,
    key_bindings_view: *mut AnyObject,
    key_bindings_doc_h: f64,
    ui: &mut SettingsUi,
) -> f64 {
    let content_w = context.content_w;
    let layout = context.layout;
    let target = context.target;
    let label_x = layout.label_x;
    let ctrl_w = layout.control_w;
    let ctrl_x = layout.control_x;
    let row_h = layout.row_h;
    let described_row_h = layout.described_row_h;
    // The page's rows fall back to this page's title as their section until a card sets one.
    begin_settings_search_page();
    let mut canvas = PageCanvas::new(
        key_bindings_view,
        content_w,
        layout,
        &t("settings.sidebar_key_bindings"),
        key_bindings_doc_h,
        context.page_frame.size.height,
    );

    // ---- Master switch: its own card, like the mouse page's "Enable button mappings" ----
    let y = canvas.next_row(described_row_h);
    record_settings_search_row(
        SettingsPageId::KeyBindings,
        canvas.last_row(),
        &t("settings.row_key_bindings_enabled"),
        Some(&t("settings.desc_key_bindings_enabled")),
    );
    ui.key_bindings_enabled = SettingsRow::described(
        key_bindings_view,
        label_x,
        y,
        ctrl_x - label_x - super::SETTINGS_CONTROL_LABEL_GAP,
        described_row_h,
        &t("settings.row_key_bindings_enabled"),
        &t("settings.desc_key_bindings_enabled"),
        SettingsControl::switch(ctrl_x + ctrl_w, y, row_h, false),
    );
    let _: () = msg_send![ui.key_bindings_enabled, setTarget: target];
    let _: () = msg_send![
        ui.key_bindings_enabled,
        setAction: sel!(handleKeyBindingsToggle:)
    ];
    // The card closes over the rows registered *before* it, so the switch's card is closed here and
    // the list gets a card of its own below — the same split the mouse page's mappings use.
    canvas.card(&t("settings.header_key_bindings"));
    set_settings_search_section(&t("settings.header_key_bindings"));

    // ---- The binding list: its own section, with its own header like every other card ----
    canvas.next_section();
    // The list is a block the canvas owns: its height is provisional here and replaced by the
    // renderer through `set_row_consume`. The canvas then wraps it in a card, which is what draws
    // the section header above it and the surface behind it.
    let origin_y = canvas.next_block(BINDINGS_BLOCK_H);
    ui.key_bindings_row = canvas.last_row();
    let (doc, empty) = build_bindings_block(key_bindings_view, content_w, origin_y);
    ui.key_bindings_doc = doc;
    ui.key_bindings_empty = empty;
    canvas.card(&t("settings.header_key_binding_list"));
    set_settings_search_section(&t("settings.header_key_binding_list"));

    // Adding happens in the floating panel the card's footer ＋ opens, so the page is one list and
    // nothing else: choosing the action, recording the chord and agreeing to it all live in the
    // panel, where the user is already looking.
    sweep_unindexed_rows(&canvas, SettingsPageId::KeyBindings);
    let content_bottom = canvas.finish();
    ui.page_canvases[super::SETTINGS_KEY_BINDINGS_PAGE_INDEX] = canvas;
    content_bottom
}

/// The card the binding rows are rendered into, plus its empty-state hint.
///
/// It is a flipped settings card (surface, border and radius from the shared palette) so it reads
/// like the other cards; the renderer grows it from the top edge and hands the height to the page
/// canvas.
unsafe fn build_bindings_block(
    parent: *mut AnyObject,
    content_w: f64,
    origin_y: f64,
) -> (*mut AnyObject, *mut AnyObject) {
    // A transparent, flipped host for the rows. The canvas card that wraps this block draws the
    // surface, the border and the section header, so the block must not draw a second one.
    let doc: *mut AnyObject = msg_send![class!(NSView), alloc];
    let doc: *mut AnyObject = msg_send![doc, initWithFrame: NSRect::new(NSPoint::new(0.0, origin_y), NSSize::new(content_w, BINDINGS_BLOCK_H))];
    let _: () = msg_send![doc, setFlipped: true];
    let _: () = msg_send![parent, addSubview: doc];
    release_obj(doc);

    let empty: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let empty: *mut AnyObject = msg_send![empty, initWithFrame: NSRect::new(
        NSPoint::new(
            bindings::BINDING_PAD_X,
            bindings::BINDING_PAD_Y + (bindings::BINDING_ROW_H - 18.0) / 2.0
        ),
        NSSize::new(
            (content_w - bindings::BINDING_PAD_X * 2.0).max(1.0),
            18.0
        )
    )];
    set_field(empty, t("settings.binding_empty_group"));
    let _: () = msg_send![empty, setBezeled: false];
    let _: () = msg_send![empty, setDrawsBackground: false];
    let _: () = msg_send![empty, setEditable: false];
    let font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CONTROL];
    let _: () = msg_send![empty, setFont: font];
    apply_settings_text_role(empty, SettingsTextRole::Disabled);
    let _: () = msg_send![doc, addSubview: empty];
    release_obj(empty);
    (doc, empty)
}

/// The provisional height of one group's binding block: the renderer replaces it with the real
/// height as soon as the rows are rendered.
const BINDINGS_BLOCK_H: f64 = 200.0;

/// Build the About page and return its compact update-card bottom.
pub(super) unsafe fn build_about_page(
    context: &SettingsPageBuildContext,
    about_view: *mut AnyObject,
    about_doc_h: f64,
    window: *mut AnyObject,
    ui: &mut SettingsUi,
) -> f64 {
    let content_w = context.content_w;
    let layout = context.layout;
    let target = context.target;
    let label_x = layout.label_x;
    let label_w = layout.label_w;
    let ctrl_w = layout.control_w;
    let ctrl_x = layout.control_x;
    let row_h = layout.row_h;
    let described_row_h = layout.described_row_h;
    // The About page's header keeps the same rhythm as the shared page header: its block starts
    // `PageHeader`-style below the top edge and the first section steps from the block's bottom.
    // `ABOUT_HEADER_TOP` is the header block's own top offset; the block height below is what keeps
    // the first section cursor at `ABOUT_HEADER_TOP + ABOUT_HEADER_BLOCK_H + section_step`.
    const ABOUT_HEADER_TOP: f64 = SettingsPageHeader::TOP_PADDING;
    const ABOUT_HEADER_BLOCK_H: f64 = 80.0;
    let header_top = about_doc_h - ABOUT_HEADER_TOP;
    // The About page draws its own header, so it hands the canvas the distance from the document's
    // top edge down to the first section cursor and pins the header views it creates below.
    let header_offset = ABOUT_HEADER_TOP + ABOUT_HEADER_BLOCK_H + layout.section_step;
    // The page's rows fall back to this page's title as their section until a card sets one.
    begin_settings_search_page();
    let mut canvas = PageCanvas::new_at(
        about_view,
        content_w,
        layout,
        header_offset,
        about_doc_h,
        context.page_frame.size.height,
    );
    let header_mark = canvas.view_mark();
    const ABOUT_APP_ICON_TEXT_INSET: f64 = 73.0;
    // Offset the icon slot by its render overflow so the visible image aligns with the cards below.
    let about_header_slot_x = ABOUT_HEADER_CONTENT_LEADING_X + ABOUT_APP_ICON_RENDER_OVERFLOW;
    let about_header_text_x = about_header_slot_x + ABOUT_APP_ICON_TEXT_INSET;
    add_about_app_icon(about_view, about_header_slot_x, header_top - 58.0);
    let about_title: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let about_title: *mut AnyObject = msg_send![
        about_title,
        initWithFrame: NSRect::new(
            NSPoint::new(about_header_text_x, header_top - 33.0),
            NSSize::new(content_w - about_header_text_x, 32.0),
        )
    ];
    let _: () = msg_send![about_title, setBezeled: false];
    let _: () = msg_send![about_title, setDrawsBackground: false];
    let _: () = msg_send![about_title, setEditable: false];
    widgets::set_page_title_text(about_title, "Oh My Tab");
    let _: () = msg_send![about_view, addSubview: about_title];
    release_obj(about_title);

    let about_subtitle: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let about_subtitle: *mut AnyObject = msg_send![
        about_subtitle,
        initWithFrame: NSRect::new(
            NSPoint::new(about_header_text_x, header_top - 53.0),
            NSSize::new(content_w - about_header_text_x, 18.0),
        )
    ];
    set_field(
        about_subtitle,
        tf(
            "settings.version_label",
            &[("version", env!("CARGO_PKG_VERSION"))],
        ),
    );
    let _: () = msg_send![about_subtitle, setBezeled: false];
    let _: () = msg_send![about_subtitle, setDrawsBackground: false];
    let _: () = msg_send![about_subtitle, setEditable: false];
    let about_subtitle_font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CAPTION];
    let _: () = msg_send![about_subtitle, setFont: about_subtitle_font];
    let about_subtitle_color = settings_text_color(SettingsTextRole::Muted);
    let _: () = msg_send![about_subtitle, setTextColor: about_subtitle_color];
    let _: () = msg_send![about_view, addSubview: about_subtitle];
    release_obj(about_subtitle);
    ui.about_subtitle = about_subtitle;

    // Transparent hit area for the five-click build-version easter egg. It is added after
    // the labels so it receives clicks across the whole header without changing its visuals.
    let about_header_hit: *mut AnyObject = msg_send![about_header_click_view_class(), alloc];
    let about_header_hit: *mut AnyObject = msg_send![
        about_header_hit,
        initWithFrame: NSRect::new(
            NSPoint::new(0.0, header_top - 64.0),
            NSSize::new(content_w, 64.0),
        )
    ];
    let _: () = msg_send![about_view, addSubview: about_header_hit];
    release_obj(about_header_hit);
    canvas.pin_from_mark(header_mark);

    // Rows inside a card are derived from the layout, like every other page: the first row is
    // card top minus the bottom inset minus the row height, then next_row_cursor steps down.
    // The old header_top - 88 - 35 - 27 was legacy magic arithmetic.
    // Keep the App section title close to its card, matching the spacing used by the
    // other settings pages. The About card holds several rows, so its content cursor is lower
    // than a normal section header; placing the title at the old cursor left a large void.
    // The page title and version subtitle occupy the header block; the App section begins below
    // that block to keep its heading clear of the page subtitle.
    // Keep every About row on the same two-column grid: label on the left, value on the right.
    let about_value_x = label_x + 145.0;
    let about_value_w = (content_w - 2.0 * label_x - 145.0).max(1.0);
    // "View guide" leads the App card: the guide covers precisely the permissions and usage the
    // rows below describe, and it can be reopened at any time (the button dispatches by
    // selector and leaves the SettingsButton tag alone).
    // The first row uses the same layout row-step convention as the rest of the card (and every
    // other page); it used card_top - card_bottom_inset - described_row_h before, 6pt more than
    // the convention, which made this row look taller.
    let guide_y = canvas.next_row(described_row_h);
    record_settings_search_row(
        SettingsPageId::About,
        canvas.last_row(),
        &t("settings.row_view_guide"),
        None,
    );
    record_settings_search_row(
        SettingsPageId::About,
        canvas.last_row(),
        &t("settings.website_label"),
        None,
    );
    record_settings_search_row(
        SettingsPageId::About,
        canvas.last_row(),
        &t("settings.github_label"),
        None,
    );
    record_settings_search_row(
        SettingsPageId::About,
        canvas.last_row(),
        &t("settings.version_label_short"),
        None,
    );
    SettingsRow::plain(
        about_view,
        label_x,
        guide_y,
        label_w,
        described_row_h,
        &t("settings.row_view_guide"),
        // Same component and convention as "Export Logs" and "Open Settings": flush to the
        // control column's right edge instead of the App card's value column (about_value_x).
        // The row is a single 52pt line, so the button centers vertically inside it.
        row_action_button(
            ctrl_x,
            ctrl_w,
            guide_y + (described_row_h - ROW_ACTION_BTN_H) / 2.0,
            &t("settings.btn_open"),
            target,
            sel!(handleOpenOnboarding:),
        ),
    );
    let website_y = canvas.next_row(described_row_h);
    // With "View guide" leading the card, the website row needs its own separator (it used to
    // be the first row, so it had none).
    SettingsRow::separator_above_row(about_view, website_y, described_row_h, content_w);
    SettingsRow::plain(
        about_view,
        label_x,
        website_y,
        label_w,
        described_row_h,
        &t("settings.website_label"),
        SettingsControl::external_link(
            about_value_x,
            website_y,
            about_value_w,
            row_h,
            &t("settings.website_url"),
            0,
        ),
    );
    let github_y = canvas.next_row(described_row_h);
    SettingsRow::separator_above_row(about_view, github_y, described_row_h, content_w);
    SettingsRow::plain(
        about_view,
        label_x,
        github_y,
        label_w,
        described_row_h,
        &t("settings.github_label"),
        SettingsControl::external_link(
            about_value_x,
            github_y,
            about_value_w,
            row_h,
            &t("settings.github_url"),
            1,
        ),
    );
    let version_y = canvas.next_row(described_row_h);
    SettingsRow::separator_above_row(about_view, version_y, described_row_h, content_w);
    SettingsRow::plain(
        about_view,
        label_x,
        version_y,
        label_w,
        described_row_h,
        &t("settings.version_label_short"),
        SettingsControl::value_label(
            about_value_x,
            version_y,
            120.0,
            row_h,
            env!("CARGO_PKG_VERSION"),
        ),
    );
    canvas.card(&t("settings.section_app"));
    set_settings_search_section(&t("settings.section_app"));

    canvas.next_section_with_offset(layout.card_bottom_inset);
    // The first row derives from the card top so it matches card_bottom_inset (10); the old
    // 27+44 assumed the retired 44pt row height and left ~11pt of extra space above it.
    let permissions_row_top_y = canvas.next_row(described_row_h);
    let permission_action_gap = 8.0;
    // The status column width is derived from the shared action-button convention so the
    // button's right edge lines up with every other in-row action button.
    let permission_status_w = ctrl_w - ROW_ACTION_BTN_W - permission_action_gap;
    let accessibility_status = SettingsControl::value_label(
        ctrl_x,
        permissions_row_top_y,
        permission_status_w,
        row_h,
        &t("settings.permission_status_missing"),
    );
    let _: () = msg_send![accessibility_status, setAlignment: 1isize];
    record_settings_search_row(
        SettingsPageId::About,
        canvas.last_row(),
        &t("settings.permission_accessibility_label"),
        None,
    );
    ui.accessibility_permission_status = SettingsRow::plain(
        about_view,
        label_x,
        permissions_row_top_y,
        label_w,
        described_row_h,
        &t("settings.permission_accessibility_label"),
        accessibility_status,
    );
    let accessibility_button = row_action_button(
        ctrl_x,
        ctrl_w,
        permissions_row_top_y + (described_row_h - ROW_ACTION_BTN_H) / 2.0,
        &t("settings.btn_open_permission_settings"),
        target,
        sel!(handleOpenPrivacy:),
    );
    let _: () = msg_send![about_view, addSubview: accessibility_button];
    ui.accessibility_permission_button = accessibility_button;
    release_obj(accessibility_button);

    let screen_recording_row_y = canvas.next_row(described_row_h);
    SettingsRow::separator_above_row(
        about_view,
        screen_recording_row_y,
        described_row_h,
        content_w,
    );
    let screen_recording_status = SettingsControl::value_label(
        ctrl_x,
        screen_recording_row_y,
        permission_status_w,
        row_h,
        &t("settings.permission_status_missing"),
    );
    let _: () = msg_send![screen_recording_status, setAlignment: 1isize];
    record_settings_search_row(
        SettingsPageId::About,
        canvas.last_row(),
        &t("settings.permission_screen_recording_label"),
        None,
    );
    ui.screen_recording_permission_status = SettingsRow::plain(
        about_view,
        label_x,
        screen_recording_row_y,
        label_w,
        described_row_h,
        &t("settings.permission_screen_recording_label"),
        screen_recording_status,
    );
    let screen_recording_button = row_action_button(
        ctrl_x,
        ctrl_w,
        screen_recording_row_y + (described_row_h - ROW_ACTION_BTN_H) / 2.0,
        &t("settings.btn_open_permission_settings"),
        target,
        sel!(handleOpenScreenRecordingPrivacy:),
    );
    let _: () = msg_send![about_view, addSubview: screen_recording_button];
    release_obj(screen_recording_button);

    // Keychain access is not a TCC permission -- there is no System Settings pane for it -- so this
    // row carries the app's own action instead of an "open settings" button: it re-reads the
    // clipboard key, which raises the system's authorization prompt once.
    let keychain_row_y = canvas.next_row(described_row_h);
    SettingsRow::separator_above_row(about_view, keychain_row_y, described_row_h, content_w);
    let keychain_status = SettingsControl::value_label(
        ctrl_x,
        keychain_row_y,
        permission_status_w,
        row_h,
        &t("settings.keychain_status_missing"),
    );
    let _: () = msg_send![keychain_status, setAlignment: 1isize];
    record_settings_search_row(
        SettingsPageId::About,
        canvas.last_row(),
        &t("settings.permission_keychain_label"),
        None,
    );
    ui.keychain_access_status = SettingsRow::plain(
        about_view,
        label_x,
        keychain_row_y,
        label_w,
        described_row_h,
        &t("settings.permission_keychain_label"),
        keychain_status,
    );
    // A short label: this button shares the in-row action width with the permission buttons, and the
    // longer wording ("Grant Keychain Access") wraps and overflows the row's height (the settings
    // layout smoke measures exactly that).
    let keychain_button = row_action_button(
        ctrl_x,
        ctrl_w,
        keychain_row_y + (described_row_h - ROW_ACTION_BTN_H) / 2.0,
        &t("settings.btn_grant_keychain_access"),
        target,
        sel!(retryKeychainAccess:),
    );
    let _: () = msg_send![about_view, addSubview: keychain_button];
    ui.keychain_access_button = keychain_button;
    release_obj(keychain_button);
    canvas.card(&t("settings.section_permissions"));
    set_settings_search_section(&t("settings.section_permissions"));

    canvas.next_section_with_offset(layout.card_bottom_inset);
    let update_row_y = canvas.next_row(described_row_h);
    record_settings_search_row(
        SettingsPageId::About,
        canvas.last_row(),
        &t("settings.row_update_auto_check"),
        None,
    );
    ui.update_auto_check = SettingsRow::described(
        about_view,
        label_x,
        update_row_y,
        (ctrl_x + ctrl_w) - label_x - 70.0,
        described_row_h,
        &t("settings.row_update_auto_check"),
        &t("settings.desc_update_auto_check"),
        SettingsControl::switch(ctrl_x + ctrl_w, update_row_y + 10.0, row_h, false),
    );
    bind_control(target, ui.update_auto_check);
    // Automatically-download-and-install switch, between auto-check and the check button.
    let download_row_y = canvas.next_row(described_row_h);
    record_settings_search_row(
        SettingsPageId::About,
        canvas.last_row(),
        &t("settings.row_update_auto_download"),
        None,
    );
    ui.update_auto_download = SettingsRow::described(
        about_view,
        label_x,
        download_row_y,
        (ctrl_x + ctrl_w) - label_x - 70.0,
        described_row_h,
        &t("settings.row_update_auto_download"),
        &t("settings.desc_update_auto_download"),
        SettingsControl::switch(ctrl_x + ctrl_w, download_row_y + 10.0, row_h, false),
    );
    bind_control(target, ui.update_auto_download);
    // Keep the two update toggles visually grouped with the same inset divider used by other
    // multi-row cards. The rows are contiguous here, so the divider sits at their shared edge.
    SettingsRow::separator(about_view, update_row_y, content_w);
    // Check for updates: a taller full-width button whose title switches between
    // "Check for Updates…", "Checking…", and "You're up to date". It sits contiguous with the row
    // above (no shared gap), so it is a block rather than a row -- and the Updates card's bottom
    // edge then derives from its position like every other card.
    let check_button_h = 38.0;
    let check_button_y = canvas.next_block(check_button_h);
    // Reuse the same full-width card divider as the boundary between grouped settings rows. It is
    // created inside this block's capture -- the boundary above the button belongs to this row -- so
    // the layout owner moves it together with the row it separates. It stays hidden while compact and
    // is revealed only when the inline update result replaces the check button area, so the collapsed
    // About page does not gain an empty separator.
    let update_divider = SettingsRow::separator(about_view, download_row_y, content_w);
    let _: () = msg_send![update_divider, setHidden: true];
    ui.update_divider = update_divider;
    let check_button = SettingsButton::action(
        NSRect::new(
            NSPoint::new(label_x, check_button_y),
            NSSize::new(content_w - 2.0 * label_x, check_button_h),
        ),
        &t("settings.btn_check_for_updates"),
        target,
        sel!(handleCheckForUpdates:),
        SettingsButtonRole::Action,
    );
    let _: () = msg_send![check_button, setTag: -3isize];
    let check_layer: *mut AnyObject = msg_send![check_button, layer];
    if !check_layer.is_null() {
        layer_set_background(
            check_layer,
            crate::ffi::hex_to_cg_color(settings_palette().field_bg),
        );
    }
    let _: () = msg_send![about_view, addSubview: check_button];
    ui.update_check_button = check_button;
    release_obj(check_button);
    // Inline update-flow host container: update status/progress/buttons render here instead of
    // a separate NSWindow. Empty and hidden by default, so the About page stays compact; the flow
    // hands its height to the layout owner (`set_row_consume`), which keeps the host's top edge on
    // the check button and grows it downward.
    let compact_host_h = 0.0;
    let host_top_y = canvas.next_block(compact_host_h);
    ui.update_host_row = canvas.last_row();
    let update_host: *mut AnyObject = msg_send![widgets::flipped_settings_view_class(), alloc];
    let update_host: *mut AnyObject = msg_send![
        update_host,
        initWithFrame: NSRect::new(
            NSPoint::new(label_x, host_top_y),
            NSSize::new(content_w - 2.0 * label_x, compact_host_h),
        )
    ];
    let _: () = msg_send![update_host, setHidden: true];
    let _: () = msg_send![about_view, addSubview: update_host];
    release_obj(update_host);
    ui.update_host = update_host;
    ui.update_host_window = window;
    crate::updater::set_update_host(update_host, window, check_button);
    // The collapsed card bottom hugs the check button with an 8pt inset; the inline area is
    // not reserved by default, avoiding a large blank.
    ui.update_card = canvas.card(&t("settings.section_updates")).card;
    set_settings_search_section(&t("settings.section_updates"));
    sweep_unindexed_rows(&canvas, SettingsPageId::About);
    let content_bottom = canvas.finish();
    ui.page_canvases[SETTINGS_ABOUT_PAGE_INDEX] = canvas;
    content_bottom
}

/// Finish page registration, restore-default controls, and document validation.
pub(super) unsafe fn finalize_settings_pages(
    content: *mut AnyObject,
    window: *mut AnyObject,
    _page_frame: NSRect,
    content_w: f64,
    target: *mut AnyObject,
    pages: SettingsPageFinalization,
    ui: &mut SettingsUi,
) {
    let SettingsPageFinalization {
        roots: page_roots,
        documents: page_documents,
        bottoms: page_bottoms,
    } = pages;
    let _: () = msg_send![content, addSubview: ui.permission_warning_view];
    release_obj(ui.permission_warning_view);

    let _: () = msg_send![window, layoutIfNeeded];
    for (index, document) in page_documents.iter().enumerate() {
        ui.page_restores[index] = RestoreDefaultsControl::build_for_page(
            *document,
            target,
            6.0,
            page_bottoms[index] - 16.0,
            content_w,
        );
    }

    for (index, (root, document)) in page_roots.iter().zip(page_documents.iter()).enumerate() {
        // Settle the page *before* validating it: the restore control is part of the page content
        // (it hangs below the last card) and therefore decides the document height. Every page is
        // owned by its layout owner now (see `settings::page_canvas`).
        ui.page_canvases[index].attach_restore(
            ui.page_restores[index].container,
            ui.page_restores[index].surface,
        );
        SettingsPage {
            scroll: *root,
            document: *document,
        }
        .validate(settings_page_name(index));
    }
}

// MARK: - Search index

/// Complete every row's search text from the controls the page actually built. Manual annotations
/// retain their useful labels and descriptions, while this pass includes unannotated captions,
/// button labels and every dropdown choice, including choices that are not currently selected.
pub(super) unsafe fn sweep_unindexed_rows(canvas: &PageCanvas, page: SettingsPageId) {
    let page_index = settings_page_index(page);
    let recorded = sidebar::recorded_search_rows(page_index);
    let total = canvas.last_row() + 1;
    for row in 0..total {
        let mut text = Vec::new();
        if let Some(views) = canvas.row_views(row) {
            for &view in views {
                collect_settings_search_text(view, &mut text);
            }
        }
        if !recorded.contains(&row) {
            let section = canvas.row_section_title(row);
            let section = if section.is_empty() {
                t(settings_page_identity(page_index).title_key())
            } else {
                section
            };
            let label = text.first().map(String::as_str).unwrap_or(&section);
            // Use this row's own section, rather than the last card drawn on the page.
            sidebar::record_settings_search_entry_sectioned(sidebar::SearchEntry::row(
                page_index, row, label, None, &section,
            ));
        }
        sidebar::extend_settings_search_row(page_index, row, &text);
    }
}

/// Collect only the row's rendered subtree. Stop at native controls so AppKit's implementation
/// subviews cannot contribute duplicate or unrelated internal labels. Dropdown items are read
/// explicitly because their menu is not part of the view subtree.
unsafe fn collect_settings_search_text(view: *mut AnyObject, text: &mut Vec<String>) {
    if view.is_null() {
        return;
    }
    let is_popup: bool = msg_send![view, isKindOfClass: class!(NSPopUpButton)];
    let is_field: bool = msg_send![view, isKindOfClass: class!(NSTextField)];
    let is_button: bool = msg_send![view, isKindOfClass: class!(NSButton)];
    if is_popup {
        let count: isize = msg_send![view, numberOfItems];
        for index in 0..count {
            let title: *mut AnyObject = msg_send![view, itemTitleAtIndex: index];
            push_settings_search_text(text, crate::ffi::nsstring_to_rust(title));
        }
    } else if is_field {
        // A label's stringValue retains the full caption even when AppKit draws an ellipsis.
        let value: *mut AnyObject = msg_send![view, stringValue];
        push_settings_search_text(text, crate::ffi::nsstring_to_rust(value));
    } else if is_button {
        let title: *mut AnyObject = msg_send![view, title];
        push_settings_search_text(text, crate::ffi::nsstring_to_rust(title));
    }
    let tooltip: *mut AnyObject = msg_send![view, toolTip];
    if !tooltip.is_null() {
        push_settings_search_text(text, crate::ffi::nsstring_to_rust(tooltip));
    }
    if is_popup || is_field || is_button {
        return;
    }
    let subviews: *mut AnyObject = msg_send![view, subviews];
    let count: usize = msg_send![subviews, count];
    for index in 0..count {
        let child: *mut AnyObject = msg_send![subviews, objectAtIndex: index];
        collect_settings_search_text(child, text);
    }
}

fn push_settings_search_text(text: &mut Vec<String>, value: String) {
    let value = value.trim();
    if !value.is_empty() && !text.iter().any(|existing| existing == value) {
        text.push(value.to_owned());
    }
}

/// Clear the settings search index: the next build records every page's rows again.
///
/// Called once by the window build, before the first page: the index spans every page, so a page
/// that cleared it would delete the rows of every page built before it.
pub(super) fn begin_settings_search_recording() {
    sidebar::begin_settings_search_index();
}

/// Start one page's part of the index: no row is waiting for a card header any more, so the
/// page's first rows (drawn before any card) cannot adopt the previous page's last card.
pub(super) fn begin_settings_search_page() {
    sidebar::begin_settings_search_section();
}

/// Open a card: the rows drawn inside it are searched with `header`.
pub(super) fn set_settings_search_section(header: &str) {
    sidebar::apply_settings_search_section(header);
}

/// Attach the choices of the dropdown row recorded immediately before this call: they are part of
/// what the search matches, so typing an option finds the setting that offers it.
pub(super) fn record_settings_search_options(options: &[&str]) {
    sidebar::set_search_options_for_last_row(options);
}

/// Record one row the page builder is about to draw at canvas row `row`.
///
/// The row's card header is not known here (the page closes the card after drawing it), so the
/// entry is recorded without one; the page's following `set_settings_search_section` fills it in.
pub(super) fn record_settings_search_row(
    page: SettingsPageId,
    row: usize,
    label: &str,
    desc: Option<&str>,
) {
    sidebar::record_settings_search_entry(sidebar::SearchEntry::row(
        settings_page_index(page),
        row,
        label,
        desc,
        "",
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_extent_slider_range_matches_the_config_constants() {
        assert_eq!(
            max_extent_slider_min(),
            crate::config::KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_MIN as i64
        );
        assert_eq!(
            max_extent_slider_max(),
            crate::config::KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_MAX as i64
        );
        assert!(max_extent_slider_min() < max_extent_slider_max());
    }

    #[test]
    fn max_extent_slider_value_snaps_to_accepted_steps() {
        // The config's own tests are the canonical expectations for these edges.
        assert_eq!(max_extent_slider_value(0), 10);
        assert_eq!(max_extent_slider_value(999), 100);
        assert_eq!(max_extent_slider_value(54), 50);
        assert_eq!(max_extent_slider_value(56), 60);
        // An exact step boundary is preserved rather than moved.
        for percent in [10u32, 50, 70, 100] {
            assert_eq!(max_extent_slider_value(percent), percent as i64);
        }
    }

    #[test]
    fn max_extent_readout_is_the_snapped_percent() {
        assert_eq!(max_extent_readout(50), "50%");
        assert_eq!(max_extent_readout(0), "10%");
        assert_eq!(max_extent_readout(999), "100%");
        assert_eq!(max_extent_readout(54), "50%");
        // The readout is user-facing text, never a raw translation/config key.
        let readout = max_extent_readout(54);
        assert!(!readout.contains("settings."));
        assert!(!readout.contains("keystroke_display"));
    }
}
