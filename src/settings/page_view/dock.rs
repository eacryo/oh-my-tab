//! One settings page as data: its rows, their values and their identities.
//!
//! A page module is pure: it reads `PageInput`, writes `RowSpec`s through `RowWriter` and
//! returns a `PageView`. It creates no view, reads no global and records no search index --
//! the shared renderer does all of that from the description.

use super::*;

/// The Dock page's description: the Dock preview feature and its two behaviour switches.
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
    w.push(
        switch(
            "settings.row_dock_peek_on_hover",
            ControlField::DockPeekOnHover,
            cfg.dock_preview.peek_on_hover,
        )
        .caption("settings.desc_dock_peek_on_hover")
        // A real activation of another application happens on every hover, so the caption has to
        // carry the whole promise; the same wide block the previews switch uses.
        .wide_caption()
        .group(RowGroup::DockPeek)
        .into_spec(),
    );
    w.finish()
}
