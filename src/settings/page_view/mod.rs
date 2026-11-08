//! The settings pages as data: a page's rows described, laid out and addressed by value.
//!
//! Each migrated page is built in three stages, and each stage has exactly one owner:
//!
//! ```text
//! page_view(&PageInput) -> PageView            // pure: semantics only, no coordinates
//!      |
//!      v
//! layout_rows(&PageView, &LayoutInput) -> Vec<RowLayout>   // pure: the only layout computation
//!      |
//!      v
//! render_page(canvas, ui, &PageView, &[RowLayout])         // applies the result only
//! ```
//!
//! A [`RowSpec`] carries a card, its copy keys, the kind of control, an explicit [`ControlId`],
//! the value the control starts with, its options, the master switch that freezes it and its
//! search-index entry. It carries no frames: [`layout_rows`] turns the measured page into one
//! [`RowLayout`] per row, and the renderer only applies them. Creation, the "initial value"
//! refill and the in-place refresh all read the same [`RowSpec`], so a value the page shows and
//! the value the refresh writes cannot come from two different expressions.
//!
//! The first two stages are pure, and each lives in its own file: `model.rs` (the description
//! types), `rows.rs` (the row constructors), `update.rs` (the update description) and `layout/`
//! (`mod.rs` measures, `place.rs` places). The third stage is AppKit and lives in `render.rs`,
//! with the build in `build.rs` and the `SettingsUi` binding in `bind.rs`. `identity.rs` holds the
//! `ControlTable` that names each control, and `smoke.rs` the M2 runners.
//!
//! This file is the module's surface and nothing else: it declares those modules and re-exports
//! the names the page files, the page builder, the settings root and the dispatcher already use,
//! so no call site outside `page_view` had to change when the split happened. Re-exports carry the
//! scope the item is VISIBLE at, not the keyword it had before: an item declared directly here was
//! `pub(super)` = `settings`, so it is re-exported `pub(super)` and declares `pub(in crate::settings)`.

use super::*;

// The search index is written while a page is BUILT, not while it is described: a page-owned card
// that is not described at all records its rows through these, exactly as the inline builder did.
pub(super) use super::page_builder::{record_settings_search_row, set_settings_search_section};

// One module per page description, so pages can be written independently: each is pure and returns
// a `PageView`, and the shared machinery here (layout, rendering, identities, the update
// description) is the only thing they share.
mod about;
mod bind;
mod build;
mod clipboard;
mod finder_menu;
mod general;
mod gestures;
mod identity;
mod key_bindings;
mod keystroke_display;
mod layout;
mod model;
mod mouse;
mod render;
mod rows;
mod smoke;
mod switcher;
#[cfg(test)]
mod tests;
mod update;

use about::about_page_view;
pub(super) use about::AboutHooks;
use clipboard::clipboard_page_view;
pub(super) use clipboard::ClipboardHooks;
use finder_menu::finder_menu_page_view;
pub(super) use finder_menu::FinderMenuHooks;
use general::general_page_view;
use gestures::gestures_page_view;
pub(super) use gestures::GesturesHooks;
pub(crate) use gestures::{
    action_row_index, delete_row_index, gesture_target_option_ids, gesture_trigger_option_ids,
    is_gesture_row_identity, is_trigger_row_identity, remove_gesture_row, set_row_trigger,
    trigger_row_of,
};
use key_bindings::key_bindings_page_view;
pub(super) use key_bindings::KeyBindingsHooks;
use keystroke_display::keystroke_display_page_view;
use mouse::mouse_page_view;
pub(super) use mouse::MouseHooks;
pub(crate) use smoke::{about_update_flow_smoke_runner, migrated_pages_smoke_runner};
// Re-exported at the scope each item is VISIBLE at: `pub(super)` for the two `settings` reaches
// (the page builder calls `render_page` and the settings root stores a `RenderedPage`), and
// page_view scope for everything the page files and the renderer's own callers use. Re-exporting
// the second group at `pub(super)` would widen it to `settings`, which the compiler refuses.
pub(super) use bind::{bind_page_banner, bind_page_controls, refresh_migrated_pages};

/// The Gestures page's record row, matched by the action its description registers.
///
/// The string is written once: the page registers this row as `gesture_record`, and a refresh that
/// addressed it by any other name would silently show nothing -- which is how a recorder that was
/// armed looked like a button that did nothing.
pub(crate) const GESTURE_RECORD_ROW_ACTION: &str = "gesture_record";

/// Whether the Gestures page is waiting for the user to press the trigger button they are learning.
static GESTURE_CAPTURE_WAITING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Tell the Gestures page (and its trigger row) that a capture is running.
pub(crate) fn set_gesture_capture_waiting(waiting: bool) {
    GESTURE_CAPTURE_WAITING.store(waiting, std::sync::atomic::Ordering::SeqCst);
}

/// Whether a trigger capture is waiting for a press.
pub(crate) fn gesture_capture_waiting() -> bool {
    GESTURE_CAPTURE_WAITING.load(std::sync::atomic::Ordering::SeqCst)
}

/// Rewrite the Gestures page's record row, while a recording runs (its live preview) or after one
/// ended (the row back to its resting text). A recording changes its text on every sample, so the
/// row is updated in place: a rebuild per sample would tear the page down.
pub(crate) fn sync_record_row_with_recorder() {
    let stage = crate::mouse::gesture::recording::stage();
    let text = if stage == crate::mouse::gesture::recording::Stage::Idle {
        String::new()
    } else {
        crate::settings::gesture_recorder::record_row_text(
            stage,
            &crate::settings::gesture_recorder::preview_text(),
        )
    };
    set_record_row_text(&text);
}

/// Write the record row's text, whatever the recorder's state is. Called from a settings callback
/// that already holds the page (see `gesture_recorder::show_record_row_text`).
pub(crate) fn set_record_row_text(text: &str) {
    unsafe {
        crate::settings::with_settings_ui(|ui| {
            let Some(u) = ui.as_ref() else {
                return;
            };
            bind::set_action_row_text(u, SettingsPageId::Gestures, GESTURE_RECORD_ROW_ACTION, text);
        });
    }
}
pub(in crate::settings::page_view) use build::{
    banner_height, banner_parent_frame, canvas_row_of_drawn_row, page_hooks, view_for,
    VALUE_ACTION_GAP,
};
// `description_identity_conflict` is asserted by the page files' own tests and has no production
// caller here.
pub(super) use build::{build_page, BuiltPage};
#[cfg(test)]
pub(in crate::settings::page_view) use build::{description_identity_conflict, page_layout_input};
// `ControlTable` is a field of `SettingsUi`, so it is visible where that struct is.
pub(super) use identity::ControlTable;
pub(super) use identity::{PageRegistrations, RegisterError};
pub(super) use layout::canvas_row_of_described_row;
#[cfg(test)]
pub(in crate::settings::page_view) use layout::captioned_text_width;
#[cfg(test)]
pub(in crate::settings::page_view) use layout::page_sequence;
pub(in crate::settings::page_view) use layout::{
    canvas_rows, layout_rows, BoxRect, LayoutInput, MeasuredText, PageLayout, PageMetrics,
    PlacedRow,
};
pub(in crate::settings) use model::{
    BannerFrame, BannerSpec, ControlId, ControlKind, ControlUpdate, NoHooks, PageHooks, PageInput,
    PageRowSpec, PageView, RowCopy, RowHeight, RowLayout, RowSpec, SearchRow, Value, ValueAction,
    ValueAlign,
};
pub(in crate::settings::page_view) use render::{
    apply_page_updates, description_groups, RenderedControl, RenderedExtra,
};
// The slot predicates answer without a SettingsUi and the placement boxes are read by the page
// tests: neither has a production caller, so the re-exports are test-only.
#[cfg(test)]
pub(in crate::settings::page_view) use bind::{
    settings_ui_readout_field, settings_ui_slot_field, settings_ui_slot_present,
};
#[cfg(test)]
pub(in crate::settings::page_view) use render::{
    card_divider_above, fits_as_text, row_placement_boxes,
};
pub(super) use render::{render_page, RenderedPage};
pub(in crate::settings::page_view) use rows::{button, popup, slider, switch, RowDraft, RowWriter};
pub(in crate::settings::page_view) use update::page_update_description;
// A headless `SettingsUi` exists only for tests and the smoke runners' own unit checks.
#[cfg(test)]
pub(crate) use smoke::headless_settings_ui;
use switcher::switcher_page_view;

impl PageInput {
    /// Capture the live configuration and the current theme snapshot.
    ///
    /// The single call site is a page's build; the concurrency source test in `settings.rs` counts
    /// these, so a description that reaches for a global instead is visible.
    pub(super) fn capture() -> Self {
        // ONE read of CONFIG, then the theme snapshot is DERIVED from it. Reading it twice in one
        // argument list used to nest a read inside a read (the first guard lives until the end of
        // the statement): a writer queueing between them parks the inner read while the outer guard
        // is held by the thread waiting for itself. It also held the guard across the AppKit query
        // `ThemeInput` makes for the "auto" appearance.
        let config = CONFIG.read().unwrap().clone();
        let device = super::current_selected_device();
        let mut input = Self::from_config(
            config.clone(),
            device,
            crate::theme::ThemeInput::from_config(&config),
        );
        // The one system read the descriptions cannot make: the extension's election and the
        // recovery transaction's outcome are process state, captured at build time.
        input.finder_menu_status =
            super::finder_menu_status_text_now(crate::finder_menu::extension_enabled());
        input.permission_migration_copy = crate::update_notice::needs_permission_migration_copy();
        // This capture owns the config; making sure the device's gesture layer exists is a write, so
        // it happens here and the page only reads the scope.
        input.gesture_scope = Some(super::selected_gesture_layer(&mut input.config));
        input
    }

    pub(super) fn from_config(
        config: Config,
        device: Option<crate::mouse::device::DeviceKey>,
        theme: crate::theme::ThemeInput,
    ) -> Self {
        Self {
            config,
            theme,
            device,
            finder_menu_status: String::new(),
            permission_migration_copy: false,
            gesture_scope: None,
        }
    }
}

impl BannerFrame {
    pub(super) fn of(rect: NSRect) -> Self {
        Self {
            x: rect.origin.x,
            y: rect.origin.y,
            w: rect.size.width,
            h: rect.size.height,
        }
    }
}

impl LayoutInput {
    /// An input measured for a page: the text metrics come from the shared row metrics, so a
    /// test can build one without a window.
    #[cfg(test)]
    pub(super) fn for_test(content_w: f64) -> Self {
        Self {
            content_w,
            doc_top: 900.0,
            header_offset: 111.0,
            banner_frame: BannerFrame {
                x: 28.0,
                y: 700.0 - SettingsPageHeader::TOP_PADDING - banner_height(false),
                w: content_w,
                h: banner_height(false),
            },
            layout: SettingsLayout::new(content_w),
            text: MeasuredText::base(),
            extra_row_heights: Vec::new(),
        }
    }
}

impl ControlId {
    /// The field this row edits, if it edits one. An action control has no config field to refill,
    /// so it produces no update entry.
    pub(super) fn field(&self) -> Option<ControlField> {
        match self {
            ControlId::Field(field) => Some(*field),
            ControlId::Action(_) => None,
        }
    }
}

impl RowSpec {
    /// The caption's copy, empty for a row without one.
    pub(super) fn subtitle_text(&self) -> String {
        self.caption_key.map(t).unwrap_or_default()
    }

    /// The field this row edits, if it edits one.
    pub(super) fn field(&self) -> Option<ControlField> {
        self.id.field()
    }

    /// The row's own search entry: its copy keys resolved to text, with the page option list.
    fn search_entry(&self, section: &str) -> SearchRow {
        SearchRow {
            label: t(self.label_key),
            desc: self.desc_key.map(t),
            section: section.to_string(),
            options: match &self.control {
                ControlKind::Popup { options } => options.clone(),
                _ => Vec::new(),
            },
        }
    }
}

impl Value {
    pub(super) fn describe(&self) -> String {
        match self {
            Value::Switch(true) => "on".to_string(),
            Value::Switch(false) => "off".to_string(),
            Value::Index(index) => index.to_string(),
            Value::Slider(value) => value.to_string(),
            Value::Dynamic(text) => text.clone(),
            Value::None => String::new(),
        }
    }
}

impl PartialEq for RowHeight {
    /// Two rows are equal when they resolve to the same height for the same input; comparing the
    /// function pointers themselves would not answer that (and is not what a test means by it).
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (RowHeight::Fixed(a), RowHeight::Fixed(b)) => a == b,
            _ => std::mem::discriminant(self) == std::mem::discriminant(other),
        }
    }
}

impl Eq for RowHeight {}

impl RowHeight {
    /// The height this row takes for `input` at the page's content width.
    ///
    /// The width is a parameter because a caption row's height depends on the column it wraps in,
    /// and that column is derived from the content width (`control_x` moves with it).
    pub(super) fn resolve(&self, input: &PageInput, content_w: f64) -> f64 {
        match self {
            RowHeight::Fixed(height) => *height,
            RowHeight::Measured(measure) => measure(input, content_w),
        }
    }
}

impl From<f64> for RowHeight {
    fn from(height: f64) -> Self {
        RowHeight::Fixed(height)
    }
}

impl ValueAction {
    pub(super) fn new(action: &'static str, button_key: &'static str) -> Self {
        Self { action, button_key }
    }
}
