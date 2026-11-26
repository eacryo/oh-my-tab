//! One settings page as data: its rows, their values and their identities.
//!
//! A page module is pure: it reads `PageInput`, writes `RowSpec`s through `RowWriter` and
//! returns a `PageView`. It creates no view, reads no global and records no search index --
//! the shared renderer does all of that from the description.

use super::*;

/// The threshold's popup options: one per legal value, in order, so the popup's index IS
/// `cards_per_row - DOCK_CARDS_PER_ROW_MIN`. The row is described like every other popup on every
/// other page, which is why it needs no draft, no commit-on-end-editing and no reject-and-restore
/// path: a value the user cannot pick does not exist.
pub(in crate::settings) fn cards_per_row_options() -> Vec<String> {
    let first = crate::config::DOCK_CARDS_PER_ROW_MIN;
    let count = crate::config::DOCK_CARDS_PER_ROW_MAX - first + 1;
    (0..count)
        .map(|offset| {
            let windows = first + offset;
            tf(
                "settings.dock_cards_per_row_option",
                &[("count", &windows.to_string())],
            )
        })
        .collect()
}

/// The popup index a stored threshold shows. An out-of-range value (a hand-edited config that the
/// loader would have repaired, or a newer build's file) falls back to the default's index.
pub(in crate::settings) fn cards_per_row_index(cards_per_row: u32) -> usize {
    let first = crate::config::DOCK_CARDS_PER_ROW_MIN;
    let value = cards_per_row.clamp(first, crate::config::DOCK_CARDS_PER_ROW_MAX);
    (value - first) as usize
}

/// The threshold a popup index means.
pub(in crate::settings) fn cards_per_row_for_index(index: usize) -> u32 {
    let first = crate::config::DOCK_CARDS_PER_ROW_MIN;
    let span = (crate::config::DOCK_CARDS_PER_ROW_MAX - first + 1) as usize;
    first + index.min(span.saturating_sub(1)) as u32
}

/// The frontmost-click popup options, in the same order as `DOCK_FRONTMOST_CLICK_VALUES`.
fn frontmost_click_options() -> Vec<String> {
    crate::config::DOCK_FRONTMOST_CLICK_VALUES
        .iter()
        .map(|value| {
            t(match *value {
                "hide" => "settings.dock_frontmost_click_option_hide",
                "minimize" => "settings.dock_frontmost_click_option_minimize",
                _ => "settings.dock_frontmost_click_option_off",
            })
        })
        .collect()
}

fn frontmost_click_index(value: &str) -> usize {
    crate::config::dock_frontmost_click_index_of(value).unwrap_or(0)
}

/// The Dock page's description: the Dock preview feature, the behaviour switches that act on its
/// panel (the hover peek and quit-on-last-close), and the row/list threshold.
pub(super) fn dock_page_view(input: &PageInput) -> PageView {
    let cfg = &input.config;
    let mut w = RowWriter::new(SettingsPageId::Dock, t("settings.sidebar_dock"));
    w.in_first_card("settings.header_dock_preview");
    w.push(
        switch(
            "settings.row_dock_preview_enabled",
            ControlField::DockPreviewEnabled,
            cfg.dock_preview.enabled,
        )
        .caption("settings.desc_dock_preview_enabled")
        // The copy is two lines in the label column; the wide text block is the design's shape for
        // a caption that carries a full promise beside the control.
        .wide_caption()
        .into_spec(),
    );
    // The master switch is its own card, and every option that only means something while it is on
    // is the next one -- the same split the App Switcher page makes between its overlay switch and
    // its window options, so one switch never shares a card with the switches it governs.
    w.card("settings.header_dock_preview_options");
    // The panel's cards own the close button, so this row lives with the feature it belongs to and
    // is frozen by `update_dock_preview_controls_enabled` when the switch above it is off.
    w.push(
        switch(
            "settings.row_dock_quit_on_last_window_close",
            ControlField::DockQuitOnLastWindowClose,
            cfg.dock_preview.quit_on_last_window_close,
        )
        .into_spec(),
    );
    // The list presentation sits ahead of the threshold because it governs it: while every
    // preview is a list, the cards the threshold counts never exist and the popup freezes.
    w.push(
        switch(
            "settings.row_dock_list_mode",
            ControlField::DockListMode,
            cfg.dock_preview.list_mode,
        )
        .caption("settings.desc_dock_list_mode")
        .into_spec(),
    );
    // The threshold sits under the switch because it only means anything while previews are on,
    // and the switch freezes it.
    w.push(
        popup(
            "settings.row_dock_cards_per_row",
            &cards_per_row_options(),
            ControlField::DockCardsPerRow,
            cards_per_row_index(cfg.dock_preview.cards_per_row),
        )
        .caption("settings.desc_dock_cards_per_row")
        .into_spec(),
    );
    // The peek acts on the preview's cards too, so it sits with them. Its caption carries the whole
    // promise because every peek activates another application for real.
    w.push(
        switch(
            "settings.row_dock_peek_on_hover",
            ControlField::DockPeekOnHover,
            cfg.dock_preview.peek_on_hover,
        )
        .caption("settings.desc_dock_peek_on_hover")
        .wide_caption()
        .into_spec(),
    );
    w.push(
        popup(
            "settings.row_dock_frontmost_icon_click_action",
            &frontmost_click_options(),
            ControlField::DockFrontmostIconClickAction,
            frontmost_click_index(&cfg.dock_preview.frontmost_icon_click_action),
        )
        .caption("settings.desc_dock_frontmost_icon_click_action")
        .into_spec(),
    );
    w.push(
        switch(
            "settings.row_dock_restore_minimized_on_icon_click",
            ControlField::DockRestoreMinimizedOnIconClick,
            cfg.dock_preview.restore_minimized_on_icon_click,
        )
        .caption("settings.desc_dock_restore_minimized_on_icon_click")
        .into_spec(),
    );
    w.push(
        switch(
            "settings.row_dock_cmd_right_click_quit",
            ControlField::DockCmdRightClickQuit,
            cfg.dock_preview.cmd_right_click_quit,
        )
        .caption("settings.desc_dock_cmd_right_click_quit")
        .into_spec(),
    );
    // The Dock list is separate from the switcher's, but both are admitted through the same card.
    // Its entries are additive at collection time: a switcher exclusion still refuses a preview.
    push_excluded_apps_card(
        &mut w,
        &dock_excluded_card(),
        &cfg.dock_preview.excluded_apps,
        &input.known_app_names,
    );
    w.finish()
}

/// The Dock page's exclusion card: same shared shape as the switcher's, with Dock-specific copy,
/// actions and stable row identities.
fn dock_excluded_card() -> ExcludedAppsCardSpec {
    static IDENTITIES: [&str; MAX_SHOWN_EXCLUDED] = [
        "dock_excluded_a",
        "dock_excluded_b",
        "dock_excluded_c",
        "dock_excluded_d",
        "dock_excluded_e",
        "dock_excluded_f",
        "dock_excluded_g",
        "dock_excluded_h",
        "dock_excluded_i",
        "dock_excluded_j",
        "dock_excluded_k",
        "dock_excluded_l",
        "dock_excluded_m",
        "dock_excluded_n",
        "dock_excluded_o",
        "dock_excluded_p",
    ];
    ExcludedAppsCardSpec {
        header_key: "settings.header_dock_excluded",
        desc_key: "settings.desc_dock_excluded",
        add_row_key: "settings.row_dock_excluded_add",
        add_action_id: "dock_excluded_choose",
        add_action_key: "settings.dock_excluded_choose_action",
        invalid_key: "settings.row_dock_excluded_invalid",
        invalid_identity: "dock_excluded_invalid",
        empty_key: "settings.row_dock_excluded_empty",
        empty_identity: "dock_excluded_empty",
        row_label_key: "settings.row_dock_excluded",
        remove_action_id: "dock_excluded_remove",
        remove_action_key: "settings.dock_excluded_remove",
        more_key: "settings.row_dock_excluded_more",
        more_identity: "dock_excluded_more",
        identities: &IDENTITIES,
    }
}
