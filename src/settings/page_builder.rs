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
    pub(super) roots: [*mut AnyObject; 7],
    pub(super) documents: [*mut AnyObject; 7],
    pub(super) bottoms: [f64; 7],
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
    let mut y = SettingsPageHeader::attach(
        switcher_view,
        &t("settings.sidebar_switcher"),
        6.0,
        switcher_doc_h,
        content_w - 12.0,
    );

    let windows_header_y = y;
    y = layout.next_row_cursor(y, described_row_h);
    // App-switcher master switch: off = Cmd+Tab passes through to the system.
    let windows_master_row_y = y;
    ui.windows_enabled = SettingsRow::described(
        switcher_view,
        label_x,
        y,
        ctrl_x - label_x - 18.0,
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
    SettingsSection::attach(
        switcher_view,
        NSRect::new(
            NSPoint::new(6.0, layout.card_bottom(windows_master_row_y)),
            NSSize::new(
                content_w - 12.0,
                layout.card_top(windows_header_y) - layout.card_bottom(windows_master_row_y),
            ),
        ),
        &t("settings.header_windows"),
    );
    // The remaining window settings form a second card with its own section title.
    y = layout.next_section_cursor(y);
    let windows_options_header_y = y;
    y = layout.next_row_cursor(y, described_row_h);
    // Let the label fill the space before the control column, adapting to the available page width.
    ui.show_minimized = SettingsRow::tall_before_control(
        switcher_view,
        label_x,
        y,
        ctrl_x,
        18.0,
        &t("settings.row_show_minimized"),
        SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
    )
    .1;
    bind_control(target, ui.show_minimized);
    y = layout.next_row_cursor(y, described_row_h);
    SettingsRow::separator_above_row(switcher_view, y, described_row_h, content_w);
    ui.show_hidden_app_windows = SettingsRow::tall_before_control(
        switcher_view,
        label_x,
        y,
        ctrl_x,
        18.0,
        &t("settings.row_show_hidden_app_windows"),
        SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
    )
    .1;
    bind_control(target, ui.show_hidden_app_windows);
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
    let display_mode_metrics =
        SettingsSelect::metrics(ctrl_w, &window_display_mode_refs, row_h, described_row_h);
    // Reserve the popup's measured row height so wrapped options cannot overlap the preceding row.
    y = layout.next_row_cursor(y, display_mode_metrics.row_h);
    SettingsRow::separator_above_row(switcher_view, y, display_mode_metrics.row_h, content_w);
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
    y = layout.next_row_cursor(y, described_row_h);
    let prewarm_separator =
        SettingsRow::separator_above_row(switcher_view, y, described_row_h, content_w);
    // These two rows only mean something in icons-and-thumbnails mode: there is no thumbnail
    // to prewarm in icon-only mode, and the app name already gets its own line there (see
    // below), so the whole block follows the display mode.
    let (prewarm_label, prewarm_switch) = SettingsRow::tall_with_height(
        switcher_view,
        label_x,
        y,
        ctrl_x - label_x - 18.0,
        described_row_h,
        &t("settings.row_focused_thumbnail_prewarm"),
        SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
    );
    ui.focused_thumbnail_prewarm = prewarm_switch;
    bind_control(target, ui.focused_thumbnail_prewarm);
    // App name in card titles: the switch controls whether the thumbnail card's caption
    // shows the app name before the window title, separated by " · "; icon-only mode
    // already shows the app name on its own line below the title, so it is unaffected.
    y = layout.next_row_cursor(y, described_row_h);
    let app_name_separator =
        SettingsRow::separator_above_row(switcher_view, y, described_row_h, content_w);
    let (app_name_label, app_name_switch) = SettingsRow::tall(
        switcher_view,
        label_x,
        y,
        220.0,
        &t("settings.row_show_app_name_in_cards"),
        SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
    );
    ui.show_app_name_in_cards = app_name_switch;
    bind_control(target, ui.show_app_name_in_cards);
    y = layout.next_row_cursor(y, described_row_h);
    SettingsRow::separator_above_row(switcher_view, y, described_row_h, content_w);
    ui.card_text_size = SettingsRow::described(
        switcher_view,
        label_x,
        y,
        ctrl_x - label_x - 18.0,
        described_row_h,
        &t("settings.row_card_text_size"),
        &t("settings.desc_card_text_size"),
        SettingsControl::slider(
            ctrl_x,
            y + 10.0,
            SettingsRow::slider_width(ctrl_w),
            row_h,
            TEXT_SIZE_MIN,
            TEXT_SIZE_MAX,
            TEXT_SIZE_DEFAULT,
            // Double-click restores the default size (15pt).
            Some(TEXT_SIZE_DEFAULT as f64),
        ),
    );
    ui.card_text_size_value_label =
        SettingsRow::attach_slider_readout(switcher_view, ui.card_text_size, TEXT_SIZE_DEFAULT);
    bind_control(target, ui.card_text_size);
    y = layout.next_row_cursor(y, described_row_h);
    SettingsRow::separator_above_row(switcher_view, y, described_row_h, content_w);
    ui.status_bar_text_size = SettingsRow::described(
        switcher_view,
        label_x,
        y,
        ctrl_x - label_x - 18.0,
        described_row_h,
        &t("settings.row_status_bar_text_size"),
        &t("settings.desc_status_bar_text_size"),
        SettingsControl::slider(
            ctrl_x,
            y + 10.0,
            SettingsRow::slider_width(ctrl_w),
            row_h,
            TEXT_SIZE_MIN,
            TEXT_SIZE_MAX,
            TEXT_SIZE_DEFAULT,
            // Double-click restores the default size (15pt).
            Some(TEXT_SIZE_DEFAULT as f64),
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
    let op_metrics = SettingsSelect::metrics(ctrl_w, &op_label_refs, row_h, described_row_h);
    y = layout.next_row_cursor(y, op_metrics.row_h);
    SettingsRow::separator_above_row(switcher_view, y, op_metrics.row_h, content_w);
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
    let activation_metrics =
        SettingsSelect::metrics(ctrl_w, &activation_label_refs, row_h, described_row_h);
    y = layout.next_row_cursor(y, activation_metrics.row_h);
    SettingsRow::separator_above_row(switcher_view, y, activation_metrics.row_h, content_w);
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
    y = layout.next_row_cursor(y, described_row_h);
    SettingsRow::separator_above_row(switcher_view, y, described_row_h, content_w);
    ui.corner_radius = SettingsRow::tall(
        switcher_view,
        label_x,
        y,
        label_w,
        &t("settings.row_corner_radius"),
        SettingsControl::text_input(ctrl_x, y + 10.0, ctrl_w, row_h, "64"),
    )
    .1;
    let options_card_parts = SettingsSection::attach(
        switcher_view,
        NSRect::new(
            NSPoint::new(6.0, layout.card_bottom(y)),
            NSSize::new(
                content_w - 12.0,
                layout.card_top(windows_options_header_y) - layout.card_bottom(y),
            ),
        ),
        &t("settings.header_window_options"),
    );
    // The thumbnail-only pair: a block two rows tall (row_gap + described_row_h each), with
    // each row's own divider going along with it.
    ui.thumbnail_only_block = CollapsibleRows::new(
        options_card_parts.card,
        options_card_parts.shadow,
        vec![
            prewarm_label,
            prewarm_switch,
            app_name_label,
            app_name_switch,
        ],
        vec![prewarm_separator, app_name_separator],
        2.0 * (layout.row_gap + SettingsLayout::SINGLE_LINE_ROW_H),
    );

    y = layout.next_section_cursor(y);
    let keyboard_header_y = y;
    // Modifier popup shows Option+Tab / Command+Tab; the index maps to option/command.
    let mod_labels = [
        t("settings.modifier_option"),
        t("settings.modifier_command"),
    ];
    let mod_label_refs: Vec<&str> = mod_labels.iter().map(|s| s.as_str()).collect();
    let mod_metrics = SettingsSelect::metrics(ctrl_w, &mod_label_refs, row_h, described_row_h);
    y = layout.next_row_cursor(y, mod_metrics.row_h);
    let keyboard_card_bottom = layout.card_bottom(y);
    let keyboard_card_top = layout.card_top(keyboard_header_y);
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
    SettingsSection::attach(
        switcher_view,
        NSRect::new(
            NSPoint::new(6.0, keyboard_card_bottom),
            NSSize::new(content_w - 12.0, keyboard_card_top - keyboard_card_bottom),
        ),
        &t("settings.header_keyboard"),
    );

    keyboard_card_bottom
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
    let mut y = SettingsPageHeader::attach(
        general_view,
        &t("settings.sidebar_general"),
        6.0,
        general_doc_h,
        content_w - 12.0,
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

    // Start hidden; page selection applies the permission state and resizes General's viewport.
    let _: () = msg_send![banner, setHidden: true];

    let appearance_header_y = y;
    let theme_items = [
        t("settings.theme_dark"),
        t("settings.theme_light"),
        t("settings.theme_auto"),
    ];
    let theme_item_refs: Vec<&str> = theme_items.iter().map(String::as_str).collect();
    let theme_metrics = SettingsSelect::metrics(ctrl_w, &theme_item_refs, row_h, described_row_h);
    y = layout.next_row_cursor(y, theme_metrics.row_h);
    ui.theme = SettingsRow::described(
        general_view,
        label_x,
        y,
        ctrl_x - label_x - 18.0,
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
    let glass_style_metrics =
        SettingsSelect::metrics(ctrl_w, &["Regular", "Clear"], row_h, described_row_h);
    y -= glass_style_metrics.row_h;
    SettingsRow::separator(general_view, y + glass_style_metrics.row_h, content_w);
    ui.glass_style = SettingsRow::described(
        general_view,
        label_x,
        y,
        ctrl_x - label_x - 18.0,
        glass_style_metrics.row_h,
        &t("settings.row_glass_style"),
        &t("settings.desc_glass_style"),
        SettingsControl::popup(
            ctrl_x,
            y + 10.0,
            ctrl_w,
            glass_style_metrics.control_h,
            &["Regular", "Clear"],
            0,
        ),
    );
    bind_control(target, ui.glass_style);
    y -= described_row_h;
    SettingsRow::separator(general_view, y + described_row_h, content_w);
    ui.glass_tint = SettingsRow::described(
        general_view,
        label_x,
        y,
        ctrl_x - label_x - 18.0,
        described_row_h,
        &t("settings.row_glass_tint"),
        &t("settings.desc_glass_tint"),
        make_color_well(
            ctrl_x,
            y + 10.0,
            ctrl_w,
            row_h,
            &Config::default().appearance.glass_tint,
            target,
        ),
    );
    configure_glass_tint_panel(target);
    let appearance_card_bottom = layout.card_bottom(y);
    let appearance_card_top = layout.card_top(appearance_header_y);
    SettingsSection::attach(
        general_view,
        NSRect::new(
            NSPoint::new(6.0, appearance_card_bottom),
            NSSize::new(
                content_w - 12.0,
                appearance_card_top - appearance_card_bottom,
            ),
        ),
        &t("settings.header_appearance"),
    );

    y = layout.next_section_cursor(y);
    let preview_header_y = y;
    y = layout.next_row_cursor(y, row_h);
    let preview_h = 90.0;
    let preview_y = y - preview_h;
    let preview_w = (content_w - 2.0 * label_x - 12.0) / 2.0;
    let right_preview_x = label_x + preview_w + 12.0;
    add_preview_caption(
        general_view,
        &t("settings.preview_switcher"),
        label_x,
        preview_y + preview_h + 3.0,
        preview_w,
    );
    add_preview_caption(
        general_view,
        &t("settings.preview_clipboard"),
        right_preview_x,
        preview_y + preview_h + 3.0,
        preview_w,
    );
    ui.glass_preview_switcher =
        make_glass_preview(general_view, label_x, preview_y, preview_w, preview_h, true);
    ui.glass_preview_clipboard = make_glass_preview(
        general_view,
        right_preview_x,
        preview_y,
        preview_w,
        preview_h,
        false,
    );
    y = preview_y;
    SettingsSection::attach(
        general_view,
        NSRect::new(
            NSPoint::new(6.0, preview_y - 12.0),
            NSSize::new(
                content_w - 12.0,
                (preview_header_y - layout.card_header_gap) - (preview_y - 12.0),
            ),
        ),
        &t("settings.header_preview"),
    );

    y = layout.next_section_cursor(y);
    let language_header_y = y;
    let locale_metrics = SettingsSelect::metrics(ctrl_w, &LOCALE_LABELS, row_h, described_row_h);
    y = layout.next_row_cursor(y, locale_metrics.row_h);
    let language_card_bottom = layout.card_bottom(y);
    let language_card_top = layout.card_top(language_header_y);
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
    SettingsSection::attach(
        general_view,
        NSRect::new(
            NSPoint::new(6.0, language_card_bottom),
            NSSize::new(content_w - 12.0, language_card_top - language_card_bottom),
        ),
        &t("settings.header_language"),
    );

    y = layout.next_section_cursor(y);
    let logging_header_y = y;
    // Log level popup: items = [debug, info]; default index 1 (info).
    let log_levels: [&str; 2] = ["Debug", "Info"];
    let log_level_metrics = SettingsSelect::metrics(ctrl_w, &log_levels, row_h, described_row_h);
    y = layout.next_row_cursor(y, log_level_metrics.row_h);
    ui.log_level = SettingsRow::described(
        general_view,
        label_x,
        y,
        ctrl_x - label_x - 18.0,
        log_level_metrics.row_h,
        &t("settings.row_log_level"),
        &t("settings.desc_log_level"),
        SettingsControl::popup(
            ctrl_x,
            y + 10.0,
            ctrl_w,
            log_level_metrics.control_h,
            &log_levels,
            1,
        ),
    );
    bind_control(target, ui.log_level);
    // Export logs: title+description on the left, action button on the right (same card
    // as the log level; the button opts out of ControlField live-apply and goes straight
    // through target/action).
    y = layout.next_row_cursor(y, described_row_h);
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
    SettingsRow::described(
        general_view,
        label_x,
        y,
        ctrl_x - label_x - 18.0,
        described_row_h,
        &t("settings.row_export_logs"),
        &t("settings.desc_export_logs"),
        export_btn,
    );
    SettingsSection::attach(
        general_view,
        NSRect::new(
            NSPoint::new(6.0, layout.card_bottom(y)),
            NSSize::new(
                content_w - 12.0,
                layout.card_top(logging_header_y) - layout.card_bottom(y),
            ),
        ),
        &t("settings.header_logging"),
    );

    y = layout.next_section_cursor(y);
    let startup_header_y = y;
    y = layout.next_row_cursor(y, described_row_h);
    // Launch-at-login switch: no title (the row label on the left already describes it).
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
    SettingsSection::attach(
        general_view,
        NSRect::new(
            NSPoint::new(6.0, layout.card_bottom(y)),
            NSSize::new(
                content_w - 12.0,
                layout.card_top(startup_header_y) - layout.card_bottom(y),
            ),
        ),
        &t("settings.header_startup"),
    );

    layout.card_bottom(y)
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
    let mut y = SettingsPageHeader::attach(
        mouse_view,
        &t("settings.sidebar_mouse"),
        6.0,
        mouse_doc_h,
        content_w - 12.0,
    );

    // Header: every section in this app carries a short-noun heading (Device / Scrolling /
    // Pointer / Button Mappings, Clipboard, ...), and this master-switch card was the only one
    // without it, which read as a blank spot at its top-left. "Mouse" rather than "Mouse
    // control": one notch shorter than the page title, the same way the clipboard page pairs
    // its "Clipboard" heading with the "Clipboard History" title, and it never repeats the row's
    // "Enable mouse control". Its distance from the page title comes from SettingsPageHeader.
    let mouse_header_y = y;
    y = layout.next_row_cursor(y, described_row_h);
    let enable_mouse_bottom = y;
    ui.enable_mouse = SettingsRow::described(
        mouse_view,
        label_x,
        y,
        ctrl_x - label_x - 18.0,
        described_row_h,
        &t("settings.row_enable_mouse"),
        &t("settings.desc_enable_mouse"),
        SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
    );
    // Update OK button title in real time when the switch toggles (OK vs OK && Restart).
    let _: () = msg_send![ui.enable_mouse, setTarget: target];
    let _: () = msg_send![ui.enable_mouse, setAction: sel!(handleEnableMouseToggle:)];
    let _ = SettingsSection::attach(
        mouse_view,
        NSRect::new(
            NSPoint::new(6.0, layout.card_bottom(enable_mouse_bottom)),
            NSSize::new(
                content_w - 12.0,
                layout.card_top(mouse_header_y) - layout.card_bottom(enable_mouse_bottom),
            ),
        ),
        &t("settings.header_mouse"),
    );

    y = layout.next_section_cursor(y);
    let device_header_y = y;
    // Popup: items are rebuilt dynamically in load_settings_values (device list is mutable).
    // A placeholder is inserted here; the real items are filled by rebuild_device_popup.
    let device_labels: Vec<String> = crate::mouse::device::connected_devices()
        .iter()
        .map(|d| format!("{} ({:#x}:{:#x})", d.name, d.vendor_id, d.product_id))
        .collect();
    let device_label_refs: Vec<&str> = if device_labels.is_empty() {
        vec![""]
    } else {
        device_labels.iter().map(|s| s.as_str()).collect()
    };
    let device_metrics =
        SettingsSelect::metrics(ctrl_w, &device_label_refs, row_h, described_row_h);
    y = layout.next_row_cursor(y, device_metrics.row_h);
    let dev_popup = SettingsControl::popup(
        ctrl_x,
        y + (device_metrics.row_h - device_metrics.control_h) / 2.0,
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
    ui.device_indicator = SettingsRow::tall_with_height(
        mouse_view,
        label_x,
        y,
        label_w,
        device_metrics.row_h,
        &t("settings.header_mouse_device"),
        dev_popup,
    )
    .1;

    let scroll_metrics =
        SettingsSelect::metrics(ctrl_w, &SCROLL_MODE_LABELS, row_h, described_row_h);
    y = layout.next_row_cursor(y, scroll_metrics.row_h);
    let scroll_popup = SettingsControl::popup(
        ctrl_x,
        y + (scroll_metrics.row_h - scroll_metrics.control_h) / 2.0,
        ctrl_w,
        scroll_metrics.control_h,
        &SCROLL_MODE_LABELS,
        0,
    );
    style_flat_popup(scroll_popup);
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
    y = layout.next_row_cursor(y, described_row_h);
    let line_count_separator =
        SettingsRow::separator_above_row(mouse_view, y, described_row_h, content_w);
    let (line_label, line_ctrl) = SettingsRow::tall(
        mouse_view,
        label_x,
        y,
        label_w,
        &t("settings.row_line_count"),
        // Leaves the readout's width on the right for the read-only value label (see
        // SettingsRow::slider_width). Integer slider 1..=10 (matches config validation;
        // mirrors LinearMouse's By Lines slider interaction).
        SettingsControl::slider(
            ctrl_x,
            y + 10.0,
            SettingsRow::slider_width(ctrl_w),
            row_h,
            1,
            10,
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
    let device_card_parts = SettingsSection::attach(
        mouse_view,
        NSRect::new(
            NSPoint::new(6.0, layout.card_bottom(y)),
            NSSize::new(
                content_w - 12.0,
                layout.card_top(device_header_y) - layout.card_bottom(y),
            ),
        ),
        &t("settings.header_mouse_device"),
    );
    let device_card = device_card_parts.card;
    let device_shadow = device_card_parts.shadow;
    // The line-count row is conditional (Line mode only): the card is the shared device card,
    // whose bottom edge rises when the row goes away.
    ui.line_count_block = CollapsibleRows::new(
        device_card,
        device_shadow,
        vec![
            ui.line_count,
            ui.line_count_label,
            ui.line_count_value_label,
        ],
        vec![line_count_separator],
        layout.row_gap + SettingsLayout::SINGLE_LINE_ROW_H,
    );

    y = layout.next_section_cursor(y);
    let scrolling_header_y = y;
    y = layout.next_row_cursor(y, described_row_h);
    // reverse_scroll switch: title + subtitle describe the scroll inversion; the switch
    // keeps the reference page's trailing inset.
    ui.reverse_scroll = SettingsRow::described(
        mouse_view,
        label_x,
        y,
        ctrl_x - label_x - 18.0,
        described_row_h,
        &t("settings.row_reverse_scroll"),
        &t("settings.desc_reverse_scroll"),
        SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
    );
    bind_control(target, ui.reverse_scroll);
    SettingsSection::attach(
        mouse_view,
        NSRect::new(
            NSPoint::new(6.0, layout.card_bottom(y)),
            NSSize::new(
                content_w - 12.0,
                layout.card_top(scrolling_header_y) - layout.card_bottom(y),
            ),
        ),
        &t("settings.header_mouse_scrolling"),
    );

    y = layout.next_section_cursor(y);
    let pointer_header_y = y;
    y = layout.next_row_cursor(y, described_row_h);
    // disable_pointer_accel switch: disable system pointer acceleration for 1:1 linear
    // cursor tracking. The subtitle explains linear tracking; the switch keeps the same
    // trailing inset as every other switch row.
    ui.disable_pointer_accel = SettingsRow::described(
        mouse_view,
        label_x,
        y,
        ctrl_x - label_x - 18.0,
        described_row_h,
        &t("settings.row_disable_pointer_accel"),
        &t("settings.desc_disable_pointer_accel"),
        SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
    );
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
    y = layout.next_row_cursor(y, described_row_h);
    let pointer_accel_separator =
        SettingsRow::separator_above_row(mouse_view, y, described_row_h, content_w);
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

    let pointer_card_parts = SettingsSection::attach(
        mouse_view,
        NSRect::new(
            NSPoint::new(6.0, layout.card_bottom(y)),
            NSSize::new(
                content_w - 12.0,
                layout.card_top(pointer_header_y) - layout.card_bottom(y),
            ),
        ),
        &t("settings.header_mouse_pointer"),
    );
    // The tracking-speed row is conditional: hand the card, its shadow, the row's three views,
    // and the divider above it to the component.
    ui.pointer_accel_block = CollapsibleRows::new(
        pointer_card_parts.card,
        pointer_card_parts.shadow,
        vec![
            ui.pointer_accel_label,
            ui.pointer_accel_slider,
            ui.pointer_accel_value_label,
        ],
        vec![pointer_accel_separator],
        layout.row_gap + SettingsLayout::SINGLE_LINE_ROW_H,
    );

    // Button mappings: an "Enable button mappings" described row + a nested table card
    // (rounded sub-table + the add-mapping button).
    y = layout.next_section_cursor(y);
    let mappings_header_y = y;
    // "Enable button mappings" described row (HTML card top), replacing the old switch
    // that sat on the section-header row's right edge.
    y = layout.next_row_cursor(y, described_row_h);
    ui.mapping_enabled = SettingsRow::described(
        mouse_view,
        label_x,
        y,
        ctrl_x - label_x - 18.0,
        described_row_h,
        &t("settings.row_mapping_enable"),
        &t("settings.desc_mapping_enable"),
        SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
    );
    let _: () = msg_send![ui.mapping_enabled, setTarget: target];
    let _: () = msg_send![ui.mapping_enabled, setAction: sel!(handleMappingEnabledChanged:)];
    SettingsSection::attach(
        mouse_view,
        NSRect::new(
            NSPoint::new(6.0, layout.card_bottom(y)),
            NSSize::new(
                content_w - 12.0,
                layout.card_top(mappings_header_y) - layout.card_bottom(y),
            ),
        ),
        &t("settings.header_mouse_mappings"),
    );

    y -= 24.0;
    let card_top = y;
    let card_w = content_w - 12.0;
    let card_h = MAPPING_PANEL_TOP
        + (MAPPING_HEADER_H + MAPPING_ROW_H * 3.0)
        + MAPPING_ACTION_TOP
        + MAPPING_ACTION_H
        + MAPPING_CARD_PAD_BOT;
    let card_bottom = card_top - card_h;
    // The outer card is a white settings card (same as every other card); only the nested
    // table and the add button carry the gray "dark" treatment from the HTML reference.
    let card_bg: *mut AnyObject = msg_send![class!(NSView), alloc];
    // Align the mapping card with the other settings cards; the nested table keeps its own
    // inset so only the outer border expands to the shared content width.
    let card_bg: *mut AnyObject = msg_send![card_bg, initWithFrame: NSRect::new(NSPoint::new(6.0, card_bottom), NSSize::new(content_w - 12.0, card_h))];
    let _: () = msg_send![card_bg, setFlipped: true];
    let _: () = msg_send![card_bg, setAutoresizingMask: 0u64];
    let _: () = msg_send![card_bg, setWantsLayer: true];
    let bg_layer: *mut AnyObject = msg_send![card_bg, layer];
    let _: () = msg_send![bg_layer, setCornerRadius: 14.0f64];
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
    let _: () = msg_send![panel_layer, setCornerRadius: 10.0f64];
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
    let header_font: *mut AnyObject = msg_send![class!(NSFont), boldSystemFontOfSize: 12.0f64];
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
        SettingsButtonRole::Compact,
    );
    let _: () = msg_send![card_bg, addSubview: add_btn];
    release_obj(add_btn);
    ui.add_mapping_button = add_btn;
    let _: () = msg_send![mouse_view, addSubview: card_bg];
    release_obj(card_bg);
    ui.mapping_card = card_bg;
    ui.mapping_scroll = std::ptr::null_mut();
    ui.mapping_doc = card_bg;
    let mouse_content_bottom = card_bottom;
    // Render the current device's mappings initially.
    render_mapping_rows();

    mouse_content_bottom
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
    // Independent layout cursor (this page's content is unrelated to the mouse page).
    let mut cy = SettingsPageHeader::attach(
        clipboard_view,
        &t("settings.sidebar_clipboard"),
        6.0,
        clipboard_doc_h,
        content_w - 12.0,
    );
    let clipboard_header_y = cy;
    cy = layout.next_row_cursor(cy, described_row_h);
    // English "Enable clipboard history" (measured 146pt) plus cell padding sits on
    // the label_w=150 edge; widen to 225 along with the persist/move_used_to_top rows.
    let clipboard_master_row_y = cy;
    ui.clipboard_enabled = SettingsRow::described(
        clipboard_view,
        label_x,
        cy,
        ctrl_x - label_x - 18.0,
        described_row_h,
        &t("settings.row_clipboard_enabled"),
        &t("settings.desc_clipboard_enabled"),
        SettingsControl::switch(ctrl_x + ctrl_w, cy, row_h, false),
    );
    let _: () = msg_send![ui.clipboard_enabled, setTarget: target];
    let _: () = msg_send![
        ui.clipboard_enabled,
        setAction: sel!(handleClipboardEnabledToggle:)
    ];
    SettingsSection::attach(
        clipboard_view,
        NSRect::new(
            NSPoint::new(6.0, layout.card_bottom(clipboard_master_row_y)),
            NSSize::new(
                content_w - 12.0,
                layout.card_top(clipboard_header_y) - layout.card_bottom(clipboard_master_row_y),
            ),
        ),
        &t("settings.header_clipboard"),
    );
    // Keep the history controls in a second titled card, matching the switcher layout.
    cy = layout.next_section_cursor(cy);
    let clipboard_options_header_y = cy;
    // Pin-selection popup: items = [Follow the Pinned Entry, Keep Current Position];
    // default index 0 (follow); the real value is set by load_settings_from.
    let pin_labels = [
        t("settings.pin_follow_entry"),
        t("settings.pin_keep_position"),
    ];
    let pin_label_refs: Vec<&str> = pin_labels.iter().map(|s| s.as_str()).collect();
    let pin_metrics = SettingsSelect::metrics(ctrl_w, &pin_label_refs, row_h, described_row_h);
    cy = layout.next_row_cursor(cy, pin_metrics.row_h);
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
    cy = layout.next_row_cursor(cy, described_row_h);
    SettingsRow::separator_above_row(clipboard_view, cy, described_row_h, content_w);
    // Persist switch (saved to disk, survives restarts; plaintext on disk -- the
    // privacy implications are documented in the README).
    // Persist switch (saved to disk, survives restarts; plaintext on disk -- the
    // privacy implications are documented in the README). The Chinese (11 CJK
    // chars) and English labels both exceed the default label_w=150 (rendered
    // truncated), so this row widens its label to 225 -- same as the
    // show_minimized row; the switch keeps the trailing inset and stays clear of the edge.
    ui.clipboard_persist = SettingsRow::plain(
        clipboard_view,
        label_x,
        cy,
        220.0,
        described_row_h,
        &t("settings.row_clipboard_persist"),
        SettingsControl::switch(ctrl_x + ctrl_w, cy, row_h, false),
    );
    bind_control(target, ui.clipboard_persist);
    cy = layout.next_row_cursor(cy, described_row_h);
    SettingsRow::separator_above_row(clipboard_view, cy, described_row_h, content_w);
    // show the source app.
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
    cy = layout.next_row_cursor(cy, described_row_h);
    SettingsRow::separator_above_row(clipboard_view, cy, described_row_h, content_w);
    // Move used entries to the top (whether pasting reorders the history; on by
    // default = current behavior).
    // English "Move used entries to top" (measured 150.3pt) exceeds label_w=150 and
    // rendered truncated ("move used entries to" after switching to English), widened
    // to 225.
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
    cy = layout.next_row_cursor(cy, described_row_h);
    SettingsRow::separator_above_row(clipboard_view, cy, described_row_h, content_w);
    // Delete after paste (Option+Enter/click = one-shot paste). Off by default -- a
    // destructive gesture, strictly opt-in. Row subtitles are no longer rendered (see
    // add_described_row's _subtitle), so the gesture hint lives in the label itself;
    // the text width follows the master described row's full width so the long label
    // never truncates.
    ui.clipboard_delete_after_paste = SettingsRow::described(
        clipboard_view,
        label_x,
        cy,
        ctrl_x - label_x - 18.0,
        described_row_h,
        &t("settings.row_clipboard_delete_after_paste"),
        "",
        SettingsControl::switch(ctrl_x + ctrl_w, cy, row_h, false),
    );
    bind_control(target, ui.clipboard_delete_after_paste);
    cy = layout.next_row_cursor(cy, described_row_h);
    let clear_pasteboard_separator =
        SettingsRow::separator_above_row(clipboard_view, cy, described_row_h, content_w);
    // This row is a child of the switch above (indented label): it only appears while "delete
    // entry after paste" is on, so it goes through the conditional-row component (whole row
    // shown/hidden, sections below closing the gap) rather than being greyed out.
    let (clear_pasteboard_label, clear_pasteboard_switch) = SettingsRow::tall_with_height(
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
    cy = layout.next_row_cursor(cy, described_row_h);
    SettingsRow::separator_above_row(clipboard_view, cy, described_row_h, content_w);
    // max entries (number input).
    ui.clipboard_max_entries = SettingsRow::plain(
        clipboard_view,
        label_x,
        cy,
        label_w,
        described_row_h,
        &t("settings.row_clipboard_max_entries"),
        SettingsControl::text_input(ctrl_x, cy, ctrl_w, row_h, "50"),
    );
    cy = layout.next_row_cursor(cy, described_row_h);
    SettingsRow::separator_above_row(clipboard_view, cy, described_row_h, content_w);
    // Auto-expire days slider: 0..=7, where 0 means never; the current value is shown on
    // the right.
    let (_, auto_expire_slider) = SettingsRow::tall(
        clipboard_view,
        label_x,
        cy,
        label_w,
        &t("settings.row_clipboard_auto_expire_days"),
        SettingsControl::slider(
            ctrl_x,
            cy + 10.0,
            SettingsRow::slider_width(ctrl_w),
            row_h,
            CLIPBOARD_AUTO_EXPIRE_MIN,
            CLIPBOARD_AUTO_EXPIRE_MAX,
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
    let clipboard_options_card_bottom = layout.card_bottom(cy);
    let clipboard_options_card_parts = SettingsSection::attach(
        clipboard_view,
        NSRect::new(
            NSPoint::new(6.0, clipboard_options_card_bottom),
            NSSize::new(
                content_w - 12.0,
                layout.card_top(clipboard_options_header_y) - clipboard_options_card_bottom,
            ),
        ),
        &t("settings.header_clipboard_options"),
    );
    // The "clear the matching system-pasteboard entry" row follows "delete entry after paste"
    // (one row tall).
    ui.clipboard_delete_block = CollapsibleRows::new(
        clipboard_options_card_parts.card,
        clipboard_options_card_parts.shadow,
        vec![clear_pasteboard_label, clear_pasteboard_switch],
        vec![clear_pasteboard_separator],
        layout.row_gap + SettingsLayout::SINGLE_LINE_ROW_H,
    );

    clipboard_options_card_bottom
}

/// Build the Window Control page and return its shortcuts-card bottom.
pub(super) unsafe fn build_window_control_page(
    context: &SettingsPageBuildContext,
    window_control_view: *mut AnyObject,
    window_control_doc_h: f64,
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
    // Independent layout cursor (unrelated to the clipboard page).
    let mut wy = SettingsPageHeader::attach(
        window_control_view,
        &t("settings.sidebar_window_control"),
        6.0,
        window_control_doc_h,
        content_w - 12.0,
    );
    let window_control_header_y = wy;
    wy = layout.next_row_cursor(wy, described_row_h);
    // Enable window control (master switch): the global Option+arrow interception is off
    // by default and must be explicitly opted in.
    ui.window_control_enabled = SettingsRow::described(
        window_control_view,
        label_x,
        wy,
        ctrl_x - label_x - 18.0,
        described_row_h,
        &t("settings.row_window_control_enabled"),
        &t("settings.desc_window_control_enabled"),
        SettingsControl::switch(ctrl_x + ctrl_w, wy, row_h, false),
    );
    let _: () = msg_send![ui.window_control_enabled, setTarget: target];
    let _: () = msg_send![
        ui.window_control_enabled,
        setAction: sel!(handleWindowControlEnabledToggle:)
    ];
    SettingsSection::attach(
        window_control_view,
        NSRect::new(
            NSPoint::new(6.0, layout.card_bottom(wy)),
            NSSize::new(
                content_w - 12.0,
                layout.card_top(window_control_header_y) - layout.card_bottom(wy),
            ),
        ),
        &t("settings.header_window_control"),
    );

    // Put the direction shortcuts in their own card so the master switch is separate from
    // the per-direction settings.
    wy = layout.next_section_cursor(wy);
    let window_control_shortcuts_header_y = wy;
    wy = layout.next_row_cursor(wy, described_row_h);
    ui.window_control_up = SettingsRow::described(
        window_control_view,
        label_x,
        wy,
        ctrl_x - label_x - 18.0,
        described_row_h,
        &t("settings.row_window_control_up"),
        &t("settings.desc_window_control_up"),
        SettingsControl::switch(ctrl_x + ctrl_w, wy, row_h, false),
    );
    bind_control(target, ui.window_control_up);
    wy = layout.next_row_cursor(wy, described_row_h);
    SettingsRow::separator_above_row(window_control_view, wy, described_row_h, content_w);
    ui.window_control_down = SettingsRow::described(
        window_control_view,
        label_x,
        wy,
        ctrl_x - label_x - 18.0,
        described_row_h,
        &t("settings.row_window_control_down"),
        &t("settings.desc_window_control_down"),
        SettingsControl::switch(ctrl_x + ctrl_w, wy, row_h, false),
    );
    bind_control(target, ui.window_control_down);
    wy = layout.next_row_cursor(wy, described_row_h);
    SettingsRow::separator_above_row(window_control_view, wy, described_row_h, content_w);
    ui.window_control_left = SettingsRow::described(
        window_control_view,
        label_x,
        wy,
        ctrl_x - label_x - 18.0,
        described_row_h,
        &t("settings.row_window_control_left"),
        &t("settings.desc_window_control_left"),
        SettingsControl::switch(ctrl_x + ctrl_w, wy, row_h, false),
    );
    bind_control(target, ui.window_control_left);
    wy = layout.next_row_cursor(wy, described_row_h);
    SettingsRow::separator_above_row(window_control_view, wy, described_row_h, content_w);
    ui.window_control_right = SettingsRow::described(
        window_control_view,
        label_x,
        wy,
        ctrl_x - label_x - 18.0,
        described_row_h,
        &t("settings.row_window_control_right"),
        &t("settings.desc_window_control_right"),
        SettingsControl::switch(ctrl_x + ctrl_w, wy, row_h, false),
    );
    bind_control(target, ui.window_control_right);
    wy = layout.next_row_cursor(wy, described_row_h);
    SettingsRow::separator_above_row(window_control_view, wy, described_row_h, content_w);
    ui.window_control_display_up = SettingsRow::described(
        window_control_view,
        label_x,
        wy,
        ctrl_x - label_x - 18.0,
        described_row_h,
        &t("settings.row_window_control_display_up"),
        &t("settings.desc_window_control_display_up"),
        SettingsControl::switch(ctrl_x + ctrl_w, wy, row_h, false),
    );
    bind_control(target, ui.window_control_display_up);
    wy = layout.next_row_cursor(wy, described_row_h);
    SettingsRow::separator_above_row(window_control_view, wy, described_row_h, content_w);
    ui.window_control_display_down = SettingsRow::described(
        window_control_view,
        label_x,
        wy,
        ctrl_x - label_x - 18.0,
        described_row_h,
        &t("settings.row_window_control_display_down"),
        &t("settings.desc_window_control_display_down"),
        SettingsControl::switch(ctrl_x + ctrl_w, wy, row_h, false),
    );
    bind_control(target, ui.window_control_display_down);
    wy = layout.next_row_cursor(wy, described_row_h);
    SettingsRow::separator_above_row(window_control_view, wy, described_row_h, content_w);
    ui.window_control_display_left = SettingsRow::described(
        window_control_view,
        label_x,
        wy,
        ctrl_x - label_x - 18.0,
        described_row_h,
        &t("settings.row_window_control_display_left"),
        &t("settings.desc_window_control_display_left"),
        SettingsControl::switch(ctrl_x + ctrl_w, wy, row_h, false),
    );
    bind_control(target, ui.window_control_display_left);
    wy = layout.next_row_cursor(wy, described_row_h);
    SettingsRow::separator_above_row(window_control_view, wy, described_row_h, content_w);
    ui.window_control_display_right = SettingsRow::described(
        window_control_view,
        label_x,
        wy,
        ctrl_x - label_x - 18.0,
        described_row_h,
        &t("settings.row_window_control_display_right"),
        &t("settings.desc_window_control_display_right"),
        SettingsControl::switch(ctrl_x + ctrl_w, wy, row_h, false),
    );
    bind_control(target, ui.window_control_display_right);
    let window_control_shortcuts_card_bottom = layout.card_bottom(wy);
    SettingsSection::attach(
        window_control_view,
        NSRect::new(
            NSPoint::new(6.0, window_control_shortcuts_card_bottom),
            NSSize::new(
                content_w - 12.0,
                layout.card_top(window_control_shortcuts_header_y)
                    - window_control_shortcuts_card_bottom,
            ),
        ),
        &t("settings.header_window_control_shortcuts"),
    );

    window_control_shortcuts_card_bottom
}

/// Build the Quick Actions page and return its shortcuts-card bottom.
pub(super) unsafe fn build_quick_actions_page(
    context: &SettingsPageBuildContext,
    quick_actions_view: *mut AnyObject,
    quick_actions_doc_h: f64,
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
    // Independent layout cursor (unrelated to the window-control page).
    let mut qy = SettingsPageHeader::attach(
        quick_actions_view,
        &t("settings.sidebar_quick_actions"),
        6.0,
        quick_actions_doc_h,
        content_w - 12.0,
    );
    let quick_actions_header_y = qy;
    qy = layout.next_row_cursor(qy, described_row_h);
    // Enable quick actions (master switch): the global Option+I/E/D/L interception is off
    // by default and must be explicitly opted in.
    ui.quick_actions_enabled = SettingsRow::described(
        quick_actions_view,
        label_x,
        qy,
        ctrl_x - label_x - 18.0,
        described_row_h,
        &t("settings.row_quick_actions_enabled"),
        &t("settings.desc_quick_actions_enabled"),
        SettingsControl::switch(ctrl_x + ctrl_w, qy, row_h, false),
    );
    let _: () = msg_send![ui.quick_actions_enabled, setTarget: target];
    let _: () = msg_send![
        ui.quick_actions_enabled,
        setAction: sel!(handleQuickActionsEnabledToggle:)
    ];
    SettingsSection::attach(
        quick_actions_view,
        NSRect::new(
            NSPoint::new(6.0, layout.card_bottom(qy)),
            NSSize::new(
                content_w - 12.0,
                layout.card_top(quick_actions_header_y) - layout.card_bottom(qy),
            ),
        ),
        &t("settings.header_quick_actions"),
    );

    // Put the four action switches in their own card so the master switch stays separate
    // from the per-action settings (matching the window-control page).
    qy = layout.next_section_cursor(qy);
    let quick_actions_shortcuts_header_y = qy;
    qy = layout.next_row_cursor(qy, described_row_h);
    ui.quick_actions_open_settings = SettingsRow::described(
        quick_actions_view,
        label_x,
        qy,
        ctrl_x - label_x - 18.0,
        described_row_h,
        &t("settings.row_quick_action_open_settings"),
        &t("settings.desc_quick_action_open_settings"),
        SettingsControl::switch(ctrl_x + ctrl_w, qy, row_h, false),
    );
    bind_control(target, ui.quick_actions_open_settings);
    qy = layout.next_row_cursor(qy, described_row_h);
    SettingsRow::separator_above_row(quick_actions_view, qy, described_row_h, content_w);
    ui.quick_actions_open_finder = SettingsRow::described(
        quick_actions_view,
        label_x,
        qy,
        ctrl_x - label_x - 18.0,
        described_row_h,
        &t("settings.row_quick_action_open_finder"),
        &t("settings.desc_quick_action_open_finder"),
        SettingsControl::switch(ctrl_x + ctrl_w, qy, row_h, false),
    );
    bind_control(target, ui.quick_actions_open_finder);
    qy = layout.next_row_cursor(qy, described_row_h);
    SettingsRow::separator_above_row(quick_actions_view, qy, described_row_h, content_w);
    ui.quick_actions_show_desktop = SettingsRow::described(
        quick_actions_view,
        label_x,
        qy,
        ctrl_x - label_x - 18.0,
        described_row_h,
        &t("settings.row_quick_action_show_desktop"),
        &t("settings.desc_quick_action_show_desktop"),
        SettingsControl::switch(ctrl_x + ctrl_w, qy, row_h, false),
    );
    bind_control(target, ui.quick_actions_show_desktop);
    qy = layout.next_row_cursor(qy, described_row_h);
    SettingsRow::separator_above_row(quick_actions_view, qy, described_row_h, content_w);
    ui.quick_actions_lock_screen = SettingsRow::described(
        quick_actions_view,
        label_x,
        qy,
        ctrl_x - label_x - 18.0,
        described_row_h,
        &t("settings.row_quick_action_lock_screen"),
        &t("settings.desc_quick_action_lock_screen"),
        SettingsControl::switch(ctrl_x + ctrl_w, qy, row_h, false),
    );
    bind_control(target, ui.quick_actions_lock_screen);
    qy = layout.next_row_cursor(qy, described_row_h);
    SettingsRow::separator_above_row(quick_actions_view, qy, described_row_h, content_w);
    ui.quick_actions_locate_pointer = SettingsRow::described(
        quick_actions_view,
        label_x,
        qy,
        ctrl_x - label_x - 18.0,
        described_row_h,
        &t("settings.row_quick_action_locate_pointer"),
        &t("settings.desc_quick_action_locate_pointer"),
        SettingsControl::switch(ctrl_x + ctrl_w, qy, row_h, false),
    );
    bind_control(target, ui.quick_actions_locate_pointer);
    let quick_actions_card_bottom = layout.card_bottom(qy);
    SettingsSection::attach(
        quick_actions_view,
        NSRect::new(
            NSPoint::new(6.0, quick_actions_card_bottom),
            NSSize::new(
                content_w - 12.0,
                layout.card_top(quick_actions_shortcuts_header_y) - quick_actions_card_bottom,
            ),
        ),
        &t("settings.header_quick_actions_shortcuts"),
    );

    quick_actions_card_bottom
}

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
    let header_top = about_doc_h - 68.0;
    add_about_app_icon(about_view, label_x, header_top - 58.0);

    let about_title: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let about_title: *mut AnyObject = msg_send![
        about_title,
        initWithFrame: NSRect::new(
            NSPoint::new(label_x + 73.0, header_top - 33.0),
            NSSize::new(content_w - 73.0 - label_x, 28.0),
        )
    ];
    set_field(about_title, "Oh My Tab");
    let _: () = msg_send![about_title, setBezeled: false];
    let _: () = msg_send![about_title, setDrawsBackground: false];
    let _: () = msg_send![about_title, setEditable: false];
    let about_title_font: *mut AnyObject = msg_send![class!(NSFont), boldSystemFontOfSize: 24.0f64];
    let _: () = msg_send![about_title, setFont: about_title_font];
    let _: () = msg_send![about_view, addSubview: about_title];
    release_obj(about_title);

    let about_subtitle: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let about_subtitle: *mut AnyObject = msg_send![
        about_subtitle,
        initWithFrame: NSRect::new(
            NSPoint::new(label_x + 73.0, header_top - 53.0),
            NSSize::new(content_w - 73.0 - label_x, 18.0),
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
    let about_subtitle_font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 13.0f64];
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
            NSPoint::new(6.0, header_top - 64.0),
            NSSize::new(content_w - 12.0, 66.0),
        )
    ];
    let _: () = msg_send![about_view, addSubview: about_header_hit];
    release_obj(about_header_hit);

    // Rows inside a card are derived from the layout, like every other page: the first row is
    // card top minus the bottom inset minus the row height, then next_row_cursor steps down.
    // The old header_top - 88 - 35 - 27 was legacy magic arithmetic.
    // Keep the App section title close to its card, matching the spacing used by the
    // other settings pages. The About card holds several rows, so its content cursor is lower
    // than a normal section header; placing the title at the old cursor left a large void.
    // The page header (icon + title + version subtitle) occupies 88pt below header_top; a
    // section title must step down from the header's BOTTOM or it lands on the icon (measured:
    // next_section_cursor(header_top) put the "App" title inside the icon's lower half).
    const ABOUT_HEADER_BLOCK_H: f64 = 88.0;
    let app_label_y = layout.next_section_cursor(header_top - ABOUT_HEADER_BLOCK_H);
    // Keep every About row on the same two-column grid: label on the left, value on the right.
    let about_value_x = label_x + 145.0;
    let about_value_w = (content_w - 2.0 * label_x - 145.0).max(1.0);
    // "View guide" leads the App card: the guide covers precisely the permissions and usage the
    // rows below describe, and it can be reopened at any time (the button dispatches by
    // selector and leaves the SettingsButton tag alone).
    // The first row uses the same layout row-step convention as the rest of the card (and every
    // other page); it used card_top - card_bottom_inset - described_row_h before, 6pt more than
    // the convention, which made this row look taller.
    let guide_y = layout.next_row_cursor(app_label_y, described_row_h);
    SettingsRow::plain(
        about_view,
        label_x,
        guide_y,
        label_w,
        described_row_h,
        &t("settings.row_view_guide"),
        // Same component and convention as "Export Logs" and "Open Settings": flush to the
        // control column's right edge instead of the App card's value column (about_value_x).
        // The row is a single 54pt line, so the button centers vertically inside it.
        row_action_button(
            ctrl_x,
            ctrl_w,
            guide_y + (described_row_h - ROW_ACTION_BTN_H) / 2.0,
            &t("settings.btn_open"),
            target,
            sel!(handleOpenOnboarding:),
        ),
    );
    let website_y = layout.next_row_cursor(guide_y, described_row_h);
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
    let github_y = layout.next_row_cursor(website_y, described_row_h);
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
    let version_y = layout.next_row_cursor(github_y, described_row_h);
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
    let app_card_bottom = layout.card_bottom(version_y);
    SettingsSection::attach(
        about_view,
        NSRect::new(
            NSPoint::new(6.0, app_card_bottom),
            NSSize::new(
                content_w - 12.0,
                layout.card_top(app_label_y) - app_card_bottom,
            ),
        ),
        &t("settings.section_app"),
    );

    let permissions_label_y = layout.next_section_cursor(app_card_bottom);
    // The first row derives from the card top so it matches card_bottom_inset (10); the old
    // 27+44 assumed the retired 44pt row height and left ~11pt of extra space above it.
    let permissions_row_top_y = layout.next_row_cursor(permissions_label_y, described_row_h);
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

    let screen_recording_row_y = layout.next_row_cursor(permissions_row_top_y, described_row_h);
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

    let permissions_card_bottom = layout.card_bottom(screen_recording_row_y);
    SettingsSection::attach(
        about_view,
        NSRect::new(
            NSPoint::new(6.0, permissions_card_bottom),
            NSSize::new(
                content_w - 12.0,
                layout.card_top(permissions_label_y) - permissions_card_bottom,
            ),
        ),
        &t("settings.section_permissions"),
    );

    let updates_label_y = layout.next_section_cursor(permissions_card_bottom);
    let update_row_y = layout.next_row_cursor(updates_label_y, described_row_h);
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
    let download_row_y = layout.next_row_cursor(update_row_y, described_row_h);
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
    // "Check for Updates…", "Checking…", and "You're up to date".
    let check_button_h = 38.0;
    // Keep the check button directly below the second toggle. When the inline update host
    // replaces it, the result content can then start directly at the divider without retaining
    // the old button's vertical slot or its extra 14pt spacer.
    let check_button_y = download_row_y - check_button_h;
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
            crate::ffi::hex_to_cg_color(settings_palette().button_bg),
        );
    }
    let _: () = msg_send![about_view, addSubview: check_button];
    ui.update_check_button = check_button;
    release_obj(check_button);
    // Inline update-flow host container: update status/progress/buttons render here instead of
    // a separate NSWindow. Empty and hidden by default, so the About page stays compact; an
    // active flow expands the card + host via expand_update_section.
    // The host occupies the check button's position; its top-down content replaces the button
    // instead of being appended below it. With an initial height of 0, origin.y is the top.
    let compact_host_h = 0.0;
    let host_origin_y = check_button_y;
    let update_host: *mut AnyObject = msg_send![widgets::flipped_settings_view_class(), alloc];
    let update_host: *mut AnyObject = msg_send![
        update_host,
        initWithFrame: NSRect::new(
            NSPoint::new(label_x, host_origin_y),
            NSSize::new(content_w - 2.0 * label_x, compact_host_h),
        )
    ];
    let _: () = msg_send![update_host, setHidden: true];
    let _: () = msg_send![about_view, addSubview: update_host];
    release_obj(update_host);
    ui.update_host = update_host;
    ui.update_host_origin_y = host_origin_y;
    ui.update_host_window = window;
    crate::updater::set_update_host(update_host, window, check_button);
    // The collapsed card bottom hugs the check button with a 10pt inset; the inline area is
    // not reserved by default, avoiding a large blank.
    let compact_card_bottom = check_button_y - 10.0;
    let update_card_parts = SettingsSection::attach(
        about_view,
        NSRect::new(
            NSPoint::new(6.0, compact_card_bottom),
            NSSize::new(
                content_w - 12.0,
                layout.card_top(updates_label_y) - compact_card_bottom,
            ),
        ),
        &t("settings.section_updates"),
    );
    let update_card = update_card_parts.card;
    let update_card_shadow = update_card_parts.shadow;
    ui.update_card = update_card;
    ui.update_card_shadow = update_card_shadow;
    // Reuse the same full-width card divider as the boundary between grouped settings rows.
    // It is hidden while compact and revealed only when the inline update result replaces the
    // check button area, so the collapsed About page does not gain an empty separator.
    let update_divider = SettingsRow::separator(about_view, download_row_y, content_w);
    let _: () = msg_send![update_divider, setHidden: true];
    ui.update_divider = update_divider;
    ui.update_card_compact_h = {
        let compact_frame: NSRect = msg_send![update_card, frame];
        compact_frame.size.height
    };

    compact_card_bottom
}

/// Finish page registration, restore-default controls, and document validation.
pub(super) unsafe fn finalize_settings_pages(
    content: *mut AnyObject,
    window: *mut AnyObject,
    page_frame: NSRect,
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
    for (name, (scroll, document)) in [
        ("general", (page_roots[0], page_documents[0])),
        ("switcher", (page_roots[1], page_documents[1])),
        ("mouse", (page_roots[2], page_documents[2])),
        ("clipboard", (page_roots[3], page_documents[3])),
        ("quick-actions", (page_roots[5], page_documents[5])),
        ("about", (page_roots[6], page_documents[6])),
    ] {
        SettingsPage { scroll, document }.validate(name);
    }
    let update_host_frame: NSRect = msg_send![ui.update_host, frame];
    ui.update_host_origin_y = update_host_frame.origin.y;

    for (index, document) in page_documents.iter().enumerate() {
        ui.page_restores[index] = RestoreDefaultsControl::build_for_page(
            *document,
            target,
            6.0,
            page_bottoms[index] - 16.0,
            content_w - 12.0,
        );
    }

    let page_names: [&str; 7] = [
        "general",
        "switcher",
        "mouse",
        "clipboard",
        "window_control",
        "quick_actions",
        "about",
    ];
    for (index, (root, document)) in page_roots.iter().zip(page_documents.iter()).enumerate() {
        let _ = widgets::fit_page_document_height(
            *document,
            page_frame.size.height,
            SettingsPageHeader::BOTTOM_PADDING,
        );
        SettingsPage {
            scroll: *root,
            document: *document,
        }
        .validate(page_names[index]);
    }
    let update_host_frame: NSRect = msg_send![ui.update_host, frame];
    ui.update_host_origin_y = update_host_frame.origin.y;
}
