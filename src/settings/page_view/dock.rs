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

/// The Dock page's description: the Dock preview feature, its behaviour switch and its threshold.
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
    w.finish()
}
