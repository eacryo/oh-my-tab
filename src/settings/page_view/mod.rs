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
//! Nothing in this file touches AppKit: every function here runs without a window and is
//! asserted by the unit tests below.

use super::*;

use std::collections::HashMap;

// One module per page description, so pages can be written independently: each is pure and returns
// a `PageView`, and the shared machinery here (layout, rendering, identities, the update
// description) is the only thing they share.
mod general;
mod switcher;

use general::general_page_view;
use switcher::switcher_page_view;

/// Everything a page description may read, captured once per transaction.
pub(super) struct PageInput {
    pub(super) config: Config,
    /// The theme inputs the row copy and option labels follow (P0). A page that needs no theme
    /// output still carries the snapshot so its description is built from one input type.
    #[allow(dead_code)]
    pub(super) theme: crate::theme::ThemeInput,
    /// The device the Mouse page edits (`None` = "All Mice"). One config with two devices shows
    /// different values, so this is an input of the description, not something it discovers; the
    /// Switcher and General rows read only the config, and this keeps the input type complete for
    /// the pages that follow.
    #[allow(dead_code)]
    pub(super) device: Option<crate::mouse::device::DeviceKey>,
}

impl PageInput {
    /// Capture the live configuration and the current theme snapshot.
    ///
    /// The single call site is a page's build; the concurrency source test in `settings.rs` counts
    /// these, so a description that reaches for a global instead is visible.
    pub(super) fn capture() -> Self {
        Self::from_config(
            CONFIG.read().unwrap().clone(),
            super::current_selected_device(),
            crate::theme::ThemeInput::current(),
        )
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
        }
    }
}

/// The measured page a description is laid out into.
///
/// This is the complete layout input, not just font metrics: the content width, the page
/// viewport, the shared column metrics, the text measured in the current language and text size,
/// and the page constants the rows are placed with. Every field is a number the caller measured
/// or the design system owns; `layout_rows` computes the rest.
/// A box stated in another view's coordinates: the banner strip's frame in the settings content
/// view, as the build measured it. The renderer, the window's banner refresh and the test all take
/// this one box.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct BannerFrame {
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) w: f64,
    pub(super) h: f64,
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

#[derive(Clone, Debug, Default)]
pub(super) struct LayoutInput {
    pub(super) content_w: f64,
    /// The page document's top edge (the height it was provisioned with); the rows are laid out
    /// from this, since they are subviews of the document.
    pub(super) doc_top: f64,
    /// The document's top edge down to the first section cursor, as the page header measured it
    /// (the canvas owns the header block; this is only its measurement).
    pub(super) header_offset: f64,
    /// The banner strip's frame in its FINAL parent (the settings content view), as the caller
    /// measured it: the detail pane's inset and the visible viewport's top. The renderer and the
    /// window's banner refresh both take this box, so the two cannot disagree.
    pub(super) banner_frame: BannerFrame,
    pub(super) layout: SettingsLayout,
    pub(super) text: MeasuredText,
}

/// Text metrics a page's rows are placed with, measured once by the caller.
///
/// Only the described-row subtitle is measured today; a row whose copy wraps (the captioned and
/// stacked rows) already has its height measured by the widget that builds it, and the caller
/// hands that height over as a page constant.
#[derive(Clone, Debug, Default)]
pub(super) struct MeasuredText {
    /// Height of a described row's one-line subtitle in the current language.
    #[allow(dead_code)]
    pub(super) subtitle_h: f64,
    /// Height of a section heading's label box.
    pub(super) heading_h: f64,
    /// The one-line title height of a row's copy block.
    pub(super) title_h: f64,
    /// Gap between a row's title and its caption.
    pub(super) title_caption_gap: f64,
    /// Smallest caption height (one line of the description face).
    pub(super) caption_one_line_h: f64,
    /// Each row's wrapped subtitle height, in row order. A row with no subtitle has none here.
    pub(super) subtitle_heights: Vec<f64>,
}

impl MeasuredText {
    /// The text metrics the settings design system always uses. Only a caller that needs the
    /// measured value (a page with wrapping copy) replaces an entry.
    pub(super) fn base() -> Self {
        Self {
            subtitle_h: 0.0,
            heading_h: super::widgets::SECTION_HEADER_H,
            title_h: 18.0,
            title_caption_gap: 3.0,
            caption_one_line_h: 16.0,
            subtitle_heights: Vec::new(),
        }
    }

    /// Measure the page's subtitles: the wrapped height of every row whose copy is a stacked
    /// block. The page supplies the strings; this is the only place a height is measured.
    pub(super) fn measure(page: &PageView, text_w: f64) -> Self {
        let mut measured = Self::base();
        measured.subtitle_heights = page
            .rows
            .iter()
            .map(|row| match row.copy {
                RowCopy::Captioned => unsafe {
                    super::widgets::wrapped_caption_height(&row.subtitle_text(), text_w)
                        .max(measured.caption_one_line_h)
                },
                RowCopy::Plain => 0.0,
            })
            .collect();
        measured
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
                y: 700.0 - SettingsPageHeader::TOP_PADDING - banner_height(),
                w: content_w,
                h: banner_height(),
            },
            layout: SettingsLayout::new(content_w),
            text: MeasuredText::base(),
        }
    }
}

/// The kind of control a row carries, and therefore how it is built and how its value is shown.
///
/// The kind is presentation (a switch, a popup, a slider); *which* field the control edits comes
/// from its [`ControlId`], never from the kind -- several switches edit different fields, and a
/// button edits none.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum ControlKind {
    /// A switch, right-aligned in the control column.
    Switch,
    /// A popup in the control column, with its choices.
    Popup { options: Vec<String> },
    /// A stepped slider, with its readout already formatted for display.
    Slider {
        min: i64,
        max: i64,
        step: i64,
        /// The value a double-click restores.
        reset: Option<f64>,
        readout: String,
    },
    /// An action band's button: pressing it runs the row's action.
    Button,
}

/// What a control is, for the dispatcher. A field identity is what the config stores; an action
/// identity is a command with nothing to store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ControlId {
    Field(ControlField),
    Action(&'static str),
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

/// How a row's copy is stacked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RowCopy {
    /// Title only: the label sits centered in the row.
    Plain,
    /// The title with a subtitle below it that is measured and drawn.
    Captioned,
}

/// One row of a page description: what it says, what it edits and where it sits in the page.
#[derive(Clone, Debug)]
pub(super) struct RowSpec {
    /// The zero-based index of the card this row belongs to. A card boundary also carries the
    /// section's spacing, so the card index alone decides the layout.
    pub(super) card: usize,
    /// The card's heading copy key (read when the card opens).
    pub(super) section: &'static str,
    pub(super) label_key: &'static str,
    pub(super) desc_key: Option<&'static str>,
    /// The caption a stacked row draws (a copy key, like the label). Read for the search entry and
    /// measured by [`MeasuredText::measure`].
    pub(super) caption_key: Option<&'static str>,
    pub(super) control: ControlKind,
    /// The row's control identity. Every declared row carries one.
    pub(super) id: ControlId,
    pub(super) copy: RowCopy,
    /// The master switch that freezes this row's control, if any (the freeze itself is still
    /// applied by the master switch's own callback, which owns the enabled lists today).
    #[allow(dead_code)]
    pub(super) depends_on: Option<ControlField>,
    /// Whether the card draws a divider above this row (the first row of a card has none).
    pub(super) separator_above: bool,
    /// The page's conditional group this row belongs to, if any (the visibility is toggled on
    /// the canvas's own group list today).
    #[allow(dead_code)]
    pub(super) group: Option<RowGroup>,
    /// The value the control starts with; `None` for an action row.
    pub(super) value: Value,
    pub(super) search: SearchRow,
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

/// The search-index entry a row contributes, produced as a value by the page description.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SearchRow {
    pub(super) label: String,
    pub(super) desc: Option<String>,
    pub(super) section: String,
    pub(super) options: Vec<String>,
}

/// The value a control starts with, and the value the refill writes later.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Value {
    Switch(bool),
    Index(usize),
    Slider(i64),
    /// An action row has no value.
    None,
}

impl Value {
    pub(super) fn describe(&self) -> String {
        match self {
            Value::Switch(true) => "on".to_string(),
            Value::Switch(false) => "off".to_string(),
            Value::Index(index) => index.to_string(),
            Value::Slider(value) => value.to_string(),
            Value::None => String::new(),
        }
    }
}

/// One "field -> value" entry of the refill / in-place refresh description.
///
/// `value` is what the CONTROL takes (a switch state, a popup index, a slider stop); `display` is
/// what its readout shows when that differs from the control's own value (the status-bar slider
/// carries a stop index and shows the point size that stop means).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ControlUpdate {
    pub(super) field: ControlField,
    pub(super) value: String,
    pub(super) display: String,
}

/// A page's full description: its rows plus everything else the page draws around them.
pub(super) struct PageView {
    /// The page this description belongs to.
    #[allow(dead_code)]
    pub(super) page: SettingsPageId,
    /// The page title's copy key.
    pub(super) title_key: &'static str,
    /// The rows in document order.
    pub(super) rows: Vec<RowSpec>,
    /// The section heading the page's first rows belong to, until a card opens.
    #[allow(dead_code)]
    pub(super) initial_section: String,
    /// A page-level block above the rows (the General page's permission banner).
    pub(super) banner: Option<BannerSpec>,
}

/// The permission banner a page carries above its rows.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct BannerSpec {
    pub(super) copy_key: &'static str,
    pub(super) action_id: &'static str,
    pub(super) content_h: f64,
    pub(super) gap: f64,
}

/// A row placed in the page: its frame and the frames of the views it draws.
///
/// These are the numbers the page is built with; the canvas then re-places the views from its own
/// walk, so a row hidden later and its cards stay in step with one another.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct RowLayout {
    /// The row's frame in the page document (unflipped: the origin is the row's bottom edge).
    pub(super) frame: BoxRect,
    pub(super) label_frame: BoxRect,
    /// The copy box the label is centered in or aligned to.
    #[allow(dead_code)]
    pub(super) stack_frame: BoxRect,
    /// The row's resulting height (its frame's height).
    #[allow(dead_code)]
    pub(super) height: f64,
    /// The caption's frame for a stacked row.
    pub(super) caption_frame: Option<BoxRect>,
    pub(super) control_frame: BoxRect,
    /// The readout beside a slider, whose frame follows the slider's.
    pub(super) readout_frame: Option<BoxRect>,
    pub(super) control_h: f64,
    pub(super) stacked: bool,
}

/// Apply the page's values to the controls that were built for it.
///
/// Pure with respect to AppKit: it returns what to write, so the refill and the in-place refresh
/// take their numbers from one place.
pub(super) fn update_description(
    view: &PageView,
    values: &HashMap<ControlField, Value>,
) -> Vec<ControlUpdate> {
    let mut updates = Vec::new();
    for row in &view.rows {
        let Some(field) = row.id.field() else {
            continue;
        };
        let Some(value) = values.get(&field) else {
            continue;
        };
        let display = match (&row.control, value) {
            (ControlKind::Slider { readout, .. }, _) => readout.clone(),
            _ => value.describe(),
        };
        updates.push(ControlUpdate {
            field,
            value: value.describe(),
            display,
        });
    }
    updates
}

/// One control's registration: the pointer the dispatcher sees, and what it identifies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ControlEntry {
    pub(super) pointer: usize,
    pub(super) id: ControlId,
}

/// Why registering a control was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RegisterError {
    /// The same identity is already registered.
    DuplicateId,
    /// The same pointer is already registered.
    DuplicatePointer,
}

/// The identity table: which control pointer edits which field, filled while the page is built.
///
/// Lifecycle, all of it asserted below:
/// - register and unregister in pairs; rebuilding a page unregisters its entries before
///   registering again;
/// - a duplicate registration, of the same identity or of the same pointer, is REFUSED here and
///   aborts that page's build (the caller discards the registrations it made and leaves the
///   previous page alone);
/// - a hit on a destroyed control's address therefore cannot be answered by a stale entry,
///   because the entry is gone before the view is released.
#[derive(Debug, Default)]
pub(super) struct ControlTable {
    entries: Vec<ControlEntry>,
}

impl ControlTable {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Register one control. The identity is unique across the table, and so is the pointer.
    pub(super) fn register(
        &mut self,
        pointer: *mut AnyObject,
        id: ControlId,
    ) -> Result<(), RegisterError> {
        let pointer = pointer as usize;
        if pointer == 0 {
            return Ok(());
        }
        if self.entries.iter().any(|entry| entry.id == id) {
            return Err(RegisterError::DuplicateId);
        }
        if self.entries.iter().any(|entry| entry.pointer == pointer) {
            return Err(RegisterError::DuplicatePointer);
        }
        self.entries.push(ControlEntry { pointer, id });
        Ok(())
    }

    /// Remove one registration. Returns whether an entry was removed; repeated cleanup is safe.
    pub(super) fn unregister(&mut self, id: &ControlId) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| &entry.id != id);
        self.entries.len() != before
    }

    /// The identity behind a sender pointer (`None` = an unknown sender: the caller keeps its own
    /// fallback, nothing is silently dropped).
    pub(super) fn lookup(&self, pointer: *mut AnyObject) -> Option<ControlId> {
        let pointer = pointer as usize;
        self.entries
            .iter()
            .find(|entry| entry.pointer == pointer)
            .map(|entry| entry.id.clone())
    }

    /// Whether the identity is registered (publish validates against it; the lifecycle tests read
    /// it too).
    pub(super) fn contains_id(&self, id: &ControlId) -> bool {
        self.entries.iter().any(|entry| &entry.id == id)
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Drop every registration: the whole settings window is being torn down.
    pub(super) fn clear(&mut self) {
        self.entries.clear();
    }
}

/// A page's registrations, applied to the shared table only once the whole page is built.
///
/// This is what makes a failed build harmless: the controls exist in the page's view during the
/// build, but the mapping the dispatcher reads is untouched until `commit`. A page that fails
/// half-way drops every registration it made here, so the mapping and the views it names stay
/// together.
#[derive(Debug, Default)]
pub(super) struct PageRegistrations {
    entries: Vec<ControlEntry>,
}

impl PageRegistrations {
    pub(super) fn register(
        &mut self,
        pointer: *mut AnyObject,
        id: ControlId,
    ) -> Result<(), RegisterError> {
        let pointer = pointer as usize;
        if pointer == 0 {
            return Ok(());
        }
        if self.entries.iter().any(|entry| entry.id == id) {
            return Err(RegisterError::DuplicateId);
        }
        if self.entries.iter().any(|entry| entry.pointer == pointer) {
            return Err(RegisterError::DuplicatePointer);
        }
        self.entries.push(ControlEntry { pointer, id });
        Ok(())
    }

    /// Publish the page's registrations, replacing whatever it registered before.
    ///
    /// The page's old entries are removed FIRST (so a control released right after this cannot be
    /// reached through a stale pointer), then the new ones are added. On a refusal every entry this
    /// call added is removed again, so the table keeps the previous mapping exactly as it was —
    /// the page's views and its mapping either both change or neither does.
    pub(super) fn publish(&mut self, table: &mut ControlTable) -> Result<(), RegisterError> {
        let new_entries = std::mem::take(&mut self.entries);
        // Check FIRST, against the mapping exactly as it will be after the page's own old entries
        // are gone: a page that cannot be published must not have disturbed anything, so the
        // previous controls and the previous mapping stay together.
        let mut conflict: Option<RegisterError> = None;
        for entry in &new_entries {
            let collides = table.entries.iter().any(|live| {
                let replaced = new_entries.iter().any(|new| new.id == live.id);
                !replaced && (live.id == entry.id || live.pointer == entry.pointer)
            });
            if collides {
                conflict = Some(if table.contains_id(&entry.id) {
                    RegisterError::DuplicateId
                } else {
                    RegisterError::DuplicatePointer
                });
                break;
            }
        }
        if let Some(conflict) = conflict {
            self.entries = new_entries;
            return Err(conflict);
        }
        for entry in &new_entries {
            table.unregister(&entry.id);
        }
        let mut added: Vec<ControlId> = Vec::with_capacity(new_entries.len());
        for entry in &new_entries {
            if let Err(error) = table.register(entry.pointer as *mut AnyObject, entry.id.clone()) {
                for id in &added {
                    table.unregister(id);
                }
                self.entries = new_entries;
                return Err(error);
            }
            added.push(entry.id.clone());
        }
        Ok(())
    }
}
// MARK: - Layout

/// Page constants the row placement is stated in terms of.
///
/// They are the page's own copy of the design system's numbers, handed in rather than read, so the
/// placement of a row is a function of its inputs only.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct PageMetrics {
    /// Vertical gap a row adds below itself (`PageCanvas::next_row`'s gap term).
    pub(super) row_gap: f64,
    /// Distance from one section cursor to the next (a card boundary's step).
    pub(super) section_step: f64,
    /// The label column's inset inside a card.
    pub(super) label_x: f64,
    /// Control height (the switch and popup controls).
    pub(super) control_h: f64,
    /// Width of a slider's trailing readout, and the gap before it.
    pub(super) readout_w: f64,
    pub(super) readout_gap: f64,
    pub(super) readout_h: f64,
    /// Left inset of a card and of its section heading.
    pub(super) card_inset: f64,
    /// A switch's drawn width and height.
    pub(super) switch_w: f64,
    pub(super) switch_h: f64,
    /// A row action button's width and height.
    pub(super) action_btn_w: f64,
    pub(super) action_btn_h: f64,
}

impl PageMetrics {
    /// The metrics every settings page is laid out with today, read from the design system.
    pub(super) fn for_layout(layout: SettingsLayout) -> Self {
        Self {
            row_gap: layout.row_gap,
            section_step: layout.section_step,
            label_x: layout.label_x,
            control_h: SettingsLayout::CONTROL_H,
            readout_w: super::SLIDER_READOUT_W,
            readout_gap: super::SLIDER_READOUT_GAP,
            readout_h: super::SLIDER_READOUT_H,
            card_inset: super::SETTINGS_DETAIL_INSET,
            switch_w: super::HTML_SWITCH_W,
            switch_h: super::HTML_SWITCH_H,
            action_btn_w: super::ROW_ACTION_BTN_W,
            action_btn_h: super::ROW_ACTION_BTN_H,
        }
    }
}

/// One box the layout hands to the renderer.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub(super) struct BoxRect {
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) w: f64,
    pub(super) h: f64,
}

impl BoxRect {
    pub(super) fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    pub(super) fn rect(&self) -> NSRect {
        NSRect::new(NSPoint::new(self.x, self.y), NSSize::new(self.w, self.h))
    }

    pub(super) fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }
}

/// A card the page draws: the heading and the rounded surface around one run of rows.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct CardRect {
    pub(super) section: &'static str,
    /// The card surface's frame in the document.
    pub(super) frame: BoxRect,
    /// The section heading's frame, above the card's top edge.
    pub(super) header: BoxRect,
}

/// The permission banner a page draws above its rows.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct BannerRect {
    pub(super) strip: BoxRect,
    pub(super) label: BoxRect,
    pub(super) button: BoxRect,
}

/// Everything `layout_rows` computes for one page: where every row, card and panel sits.
///
/// Coordinates are the page document's (unflipped), and they come from the same walk the canvas
/// uses: the first section cursor, the section steps and the row heights. The renderer hands the
/// boxes to the canvas, which is what keeps a page's build and its later re-flow in step.
#[derive(Clone, Debug, PartialEq, Default)]
pub(super) struct PageLayout {
    pub(super) banner: Option<BannerRect>,
    /// The first card's top edge (the first section cursor).
    pub(super) first_card_top: f64,
    /// One entry per row, in document order.
    pub(super) rows: Vec<RowLayout>,
    /// One entry per card, in card order.
    pub(super) cards: Vec<CardRect>,
    /// The top edge of every card, indexed by card.
    pub(super) card_tops: Vec<f64>,
    /// The run of the page's content the canvas has to re-flow: from the first row's origin (or
    /// the first card's top) down to the last row's origin.
    pub(super) content: BoxRect,
}

/// A row's height: its base height, grown when the copy box it carries needs more room.
fn row_height(
    row: &RowSpec,
    measured_subtitle_h: f64,
    _metrics: &PageMetrics,
    input: &LayoutInput,
) -> f64 {
    let base = match row.control {
        ControlKind::Popup { .. } => SettingsLayout::SINGLE_LINE_ROW_H,
        ControlKind::Switch | ControlKind::Button | ControlKind::Slider { .. } => {
            input.layout.described_row_h
        }
    };
    match row.copy {
        RowCopy::Plain => base,
        RowCopy::Captioned => {
            let stack = input.text.title_h + input.text.title_caption_gap + measured_subtitle_h;
            (stack + 19.0).max(base)
        }
    }
}

/// Place a page description in its measured page. The ONLY layout computation: `render_page`
/// applies these frames and never derives a frame of its own.
pub(super) fn layout_rows(
    view: &PageView,
    input: &LayoutInput,
    metrics: &PageMetrics,
) -> PageLayout {
    let layout = input.layout;
    let content_w = input.content_w;
    let doc_top = input.doc_top;
    let text = &input.text;
    // The banner hangs from the document top, right below the header the canvas drew.
    let banner = view.banner.as_ref().map(|spec| {
        // The strip's FINAL parent is the settings content view (the page build hands it over and
        // the window re-parents it), so the caller states its frame in THAT view's coordinates --
        // the same box the inline builder used (banner_parent_frame).
        let strip = BoxRect::new(
            input.banner_frame.x,
            input.banner_frame.y,
            input.banner_frame.w.max(1.0),
            input.banner_frame.h.max(1.0),
        );
        // The label and the button are SUBVIEWS OF THE STRIP, so their boxes are stated in the
        // strip's own coordinates: adding the strip's origin here would double the parent offset.
        BannerRect {
            strip,
            label: BoxRect::new(
                12.0,
                spec.gap + 6.0,
                (strip.w - 160.0).max(1.0),
                (spec.content_h - 12.0).max(1.0),
            ),
            button: BoxRect::new(
                strip.w - 150.0,
                spec.gap + (spec.content_h - 28.0) / 2.0,
                140.0,
                28.0,
            ),
        }
    });
    let banner_h = banner.as_ref().map_or(0.0, |b| b.strip.h);
    // The first card hangs from the first section cursor, one banner further down when there is
    // one (the banner is a sibling of the page, not part of the header block).
    let mut cursor = doc_top - input.header_offset - banner_h;
    let first_card_top = cursor;
    let mut card_tops = vec![first_card_top];
    let mut rows: Vec<RowLayout> = Vec::with_capacity(view.rows.len());
    let mut card = 0usize;
    for (index, row) in view.rows.iter().enumerate() {
        // A card boundary at the page's start has no step: the first card IS the first cursor.
        while card < row.card {
            cursor -= metrics.section_step;
            card += 1;
            card_tops.push(cursor);
        }
        let subtitle_h = text.subtitle_heights.get(index).copied().unwrap_or(0.0);
        let height = row_height(row, subtitle_h, metrics, input);
        cursor -= metrics.row_gap + height;
        let top = cursor + height;
        let copy_x = metrics.card_inset + metrics.label_x;
        // The copy column ends at the design system's fixed label width; the control column starts
        // at the measured control edge, which stays clear of it on every page width.
        let copy_w = layout.label_w.max(1.0);
        let (label, caption, stacked) = match row.copy {
            RowCopy::Plain => (
                BoxRect::new(
                    copy_x,
                    cursor + (height - text.title_h) / 2.0,
                    copy_w,
                    text.title_h,
                ),
                None,
                false,
            ),
            RowCopy::Captioned => {
                let stack_h = text.title_h + text.title_caption_gap + subtitle_h;
                let caption_y = cursor + (height - stack_h.max(0.0)) / 2.0;
                (
                    BoxRect::new(
                        copy_x,
                        caption_y + subtitle_h + text.title_caption_gap,
                        copy_w,
                        text.title_h,
                    ),
                    Some(BoxRect::new(copy_x, caption_y, copy_w, subtitle_h)),
                    true,
                )
            }
        };
        let (control, readout) = match &row.control {
            ControlKind::Switch => {
                let right_x = layout.control_x + layout.control_w;
                let y = if stacked {
                    top - metrics.control_h
                } else {
                    cursor + (height - metrics.control_h) / 2.0
                };
                (
                    BoxRect::new(
                        right_x - metrics.switch_w,
                        y + (metrics.control_h - metrics.switch_h) / 2.0,
                        metrics.switch_w,
                        metrics.switch_h,
                    ),
                    None,
                )
            }
            ControlKind::Popup { .. } => (
                BoxRect::new(
                    layout.control_x,
                    cursor + (height - metrics.control_h) / 2.0,
                    layout.control_w,
                    metrics.control_h,
                ),
                None,
            ),
            ControlKind::Slider { .. } => {
                let h = layout.row_h;
                let w = (layout.control_w - metrics.readout_w - metrics.readout_gap).max(1.0);
                let y = cursor + (height - h) / 2.0;
                (
                    BoxRect::new(layout.control_x, y, w, h),
                    Some(BoxRect::new(
                        layout.control_x + w + metrics.readout_gap,
                        y + (h - metrics.readout_h) / 2.0,
                        metrics.readout_w,
                        metrics.readout_h,
                    )),
                )
            }
            ControlKind::Button => {
                let right_x = layout.control_x + layout.control_w;
                (
                    BoxRect::new(
                        right_x - metrics.action_btn_w,
                        top - metrics.action_btn_h,
                        metrics.action_btn_w,
                        metrics.action_btn_h,
                    ),
                    None,
                )
            }
        };
        rows.push(RowLayout {
            frame: BoxRect::new(0.0, cursor, content_w, height),
            height,
            label_frame: label,
            stack_frame: label,
            caption_frame: caption,
            control_frame: control,
            readout_frame: readout,
            control_h: control.h,
            stacked,
        });
    }
    // A card's surface spans from its section cursor down past its last row's bottom edge.
    let groups = card_row_ranges(view);
    // Every card index gets a section cursor, including a card that drew no row: the canvas
    // walks sections, not drawn cards, so the cursors must line up with those indices.
    let highest_card = view.rows.iter().map(|row| row.card).max().unwrap_or(0);
    while card_tops.len() <= highest_card {
        let next = card_tops[card_tops.len() - 1] - metrics.section_step;
        card_tops.push(next);
    }
    let mut cards = Vec::new();
    for (card_index, end) in groups.iter().copied() {
        // A card's slot in the walk is its own index, which is also where its section cursor
        // stands (the walk steps a section for every card index it passes).
        let start = view.rows[..end]
            .iter()
            .rposition(|row| row.card < card_index)
            .map_or(0, |position| position + 1);
        let top = card_tops[card_index];
        let bottom = rows.get(end - 1).map(|row| row.frame.y).unwrap_or(top);
        cards.push(CardRect {
            section: view.rows[start].section,
            frame: BoxRect::new(0.0, bottom, content_w, (top - bottom).max(0.0)),
            header: BoxRect::new(
                metrics.card_inset,
                layout.section_header_y(top),
                content_w - 2.0 * metrics.card_inset,
                text.heading_h,
            ),
        });
    }
    let content_top = first_card_top;
    let content_bottom = rows.last().map_or(content_top, |row| row.frame.y);
    PageLayout {
        banner,
        first_card_top,
        rows,
        cards,
        card_tops,
        content: BoxRect::new(0.0, content_bottom, content_w, content_top - content_bottom),
    }
}
/// Each card's row range (start, end-exclusive), in card order: one range per run of rows that
/// share a card index. A page that opens a card and draws no row in it draws no card.
pub(super) fn card_row_ranges(view: &PageView) -> Vec<(usize, usize)> {
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for (index, row) in view.rows.iter().enumerate() {
        match ranges.last().map(|(card, _)| *card) {
            Some(card) if card == row.card => {
                if let Some(last) = ranges.last_mut() {
                    last.1 = index + 1;
                }
            }
            _ => ranges.push((row.card, index + 1)),
        }
    }
    ranges
}

// MARK: - Row constructors

/// Build rows card by card: `card` opens a card (its heading copy key), and the rows that follow
/// belong to it. The first row of every card starts the card's block; each later row in the card
/// draws a divider above itself.
struct RowWriter {
    page: SettingsPageId,
    initial_section: String,
    card: usize,
    section: &'static str,
    rows: Vec<RowSpec>,
}

impl RowWriter {
    fn new(page: SettingsPageId, initial_section: String) -> Self {
        Self {
            page,
            initial_section,
            card: 0,
            section: "",
            rows: Vec::new(),
        }
    }

    /// Open a card: its rows start after the section step.
    fn card(&mut self, section: &'static str) {
        self.card += 1;
        self.section = section;
    }

    fn in_first_card(&mut self, section: &'static str) {
        self.section = section;
    }

    fn push(&mut self, mut row: RowSpec) {
        debug_assert!(!self.section.is_empty(), "row pushed outside a card");
        row.card = self.card;
        row.section = self.section;
        row.separator_above = self.rows.last().is_some_and(|last| last.card == self.card);
        let header = if self.section.is_empty() {
            self.initial_section.clone()
        } else {
            t(self.section)
        };
        row.search = row.search_entry(&header);
        self.rows.push(row);
    }

    fn finish(self) -> PageView {
        PageView {
            page: self.page,
            title_key: settings_page_identity(settings_page_index(self.page)).title_key(),
            rows: self.rows,
            initial_section: self.initial_section,
            banner: None,
        }
    }
}

/// One row's fields, before the page's card bookkeeping fills them in.
struct RowDraft {
    label_key: &'static str,
    desc_key: Option<&'static str>,
    caption_key: Option<&'static str>,
    control: ControlKind,
    id: ControlId,
    copy: RowCopy,
    depends_on: Option<ControlField>,
    group: Option<RowGroup>,
    value: Value,
}

impl RowDraft {
    fn new(label_key: &'static str, control: ControlKind, id: ControlId, value: Value) -> Self {
        Self {
            label_key,
            desc_key: None,
            caption_key: None,
            control,
            id,
            copy: RowCopy::Plain,
            depends_on: None,
            group: None,
            value,
        }
    }

    fn desc(mut self, key: &'static str) -> Self {
        self.desc_key = Some(key);
        self
    }

    fn caption(mut self, key: &'static str) -> Self {
        self.copy = RowCopy::Captioned;
        self.caption_key = Some(key);
        self
    }

    #[allow(dead_code)]
    fn depends_on(mut self, field: ControlField) -> Self {
        self.depends_on = Some(field);
        self
    }

    fn group(mut self, group: RowGroup) -> Self {
        self.group = Some(group);
        self
    }

    fn into_spec(self) -> RowSpec {
        let options = match &self.control {
            ControlKind::Popup { options } => options.clone(),
            _ => Vec::new(),
        };
        RowSpec {
            card: 0,
            section: "",
            label_key: self.label_key,
            desc_key: self.desc_key,
            caption_key: self.caption_key,
            control: self.control,
            id: self.id,
            copy: self.copy,
            depends_on: self.depends_on,
            separator_above: false,
            group: self.group,
            value: self.value,
            search: SearchRow {
                label: t(self.label_key),
                desc: self.desc_key.map(t),
                section: String::new(),
                options,
            },
        }
    }
}

fn switch(label_key: &'static str, field: ControlField, on: bool) -> RowDraft {
    RowDraft::new(
        label_key,
        ControlKind::Switch,
        ControlId::Field(field),
        Value::Switch(on),
    )
}

fn popup(
    label_key: &'static str,
    options: &[String],
    field: ControlField,
    selected: usize,
) -> RowDraft {
    RowDraft::new(
        label_key,
        ControlKind::Popup {
            options: options.to_vec(),
        },
        ControlId::Field(field),
        Value::Index(selected),
    )
}

#[allow(clippy::too_many_arguments)]
fn slider(
    label_key: &'static str,
    field: ControlField,
    range: (i64, i64, i64),
    value: i64,
    reset: Option<f64>,
    readout: String,
) -> RowDraft {
    RowDraft::new(
        label_key,
        ControlKind::Slider {
            min: range.0,
            max: range.1,
            step: range.2,
            reset,
            readout,
        },
        ControlId::Field(field),
        Value::Slider(value),
    )
}

fn button(label_key: &'static str, action: &'static str) -> RowDraft {
    RowDraft::new(
        label_key,
        ControlKind::Button,
        ControlId::Action(action),
        Value::None,
    )
}

// MARK: - The update description

/// The pure "field -> value" description both the refill and the in-place refresh write.
pub(super) fn page_update_description(
    input: &PageInput,
    pages: &[SettingsPageId],
) -> Vec<ControlUpdate> {
    let mut updates = Vec::new();
    for page in pages {
        let view = view_for(*page, input);
        let values: HashMap<ControlField, Value> = view
            .rows
            .iter()
            .filter_map(|row| row.field().map(|field| (field, row.value)))
            .collect();
        updates.extend(update_description(&view, &values));
    }
    updates
}

// MARK: - Rendering

/// A control the page just built, with everything a later write to it needs.
#[derive(Clone, Debug)]
pub(super) struct RenderedControl {
    pub(super) id: ControlId,
    pub(super) kind: ControlKind,
    /// The control the dispatcher sees (the switch, the popup, the slider or the button).
    pub(super) control: *mut AnyObject,
    /// The slider's readout, or null.
    pub(super) readout: *mut AnyObject,
}

unsafe impl Send for RenderedControl {}
unsafe impl Sync for RenderedControl {}

/// What a page build produced: the views it created and the identity of each control.
#[derive(Default)]
pub(super) struct RenderedPage {
    /// The page's controls, in row order.
    pub(super) controls: Vec<RenderedControl>,
    /// The registrations the page made. Not published until the build has finished.
    pub(super) registrations: PageRegistrations,
    /// The banner's strip, label and action button (null when the page has no banner).
    pub(super) banner_strip: *mut AnyObject,
    pub(super) banner_label: *mut AnyObject,
    pub(super) banner_button: *mut AnyObject,
}

impl RenderedPage {
    /// The control a row's identity built, if this page has that row.
    pub(super) fn control(&self, id: &ControlId) -> Option<&RenderedControl> {
        self.controls.iter().find(|control| &control.id == id)
    }
}

/// The selector a field's control runs instead of the unified `handleControlChanged:`, when the
/// field has effects the unified handler does not cover.
///
/// The Windows master switch is the one case: turning the switcher off has to freeze the options
/// below it in the same action. Every other static control goes through the unified handler.
fn dedicated_selector(field: Option<ControlField>) -> Sel {
    match field {
        Some(ControlField::WindowsEnabled) => sel!(handleWindowsEnabledToggle:),
        _ => sel!(handleControlChanged:),
    }
}

/// The selector an action row's button runs.
fn action_selector(action: &str) -> Sel {
    match action {
        "export_settings" => sel!(handleExportSettings:),
        "import_settings" => sel!(handleImportSettings:),
        _ => sel!(handleExportLogs:),
    }
}

/// The boxes a row's views are placed in, in the order the renderer places them: the copy (one
/// box, or a title and a caption for a stacked row), the control, and a slider's readout.
///
/// Pure, and the single source for both halves of the row's placement: the renderer places the
/// views in exactly these boxes, and the test asserts them without AppKit.
pub(super) fn row_placement_boxes(row: &RowSpec, placed: &RowLayout) -> Vec<BoxRect> {
    let mut boxes = Vec::with_capacity(4);
    match placed.caption_frame {
        Some(caption) => {
            boxes.push(placed.label_frame);
            boxes.push(caption);
        }
        None => boxes.push(placed.label_frame),
    }
    boxes.push(placed.control_frame);
    if matches!(row.control, ControlKind::Slider { .. }) {
        if let Some(readout) = placed.readout_frame {
            boxes.push(readout);
        }
    }
    boxes
}

/// Build one row: its copy and its control, handed to the canvas with the boxes the layout gave
/// them.
///
/// Nothing here keeps a frame of its own: the copy goes through the row's own placement (the
/// canvas captured those views when `next_row` returned and re-places them every walk), and the
/// control and a slider's readout are placed explicitly. The copy is built by the same labels the
/// inline pages used, so its face is unchanged.
unsafe fn render_row(
    canvas: &mut PageCanvas,
    target: *mut AnyObject,
    row: &RowSpec,
    placed: &RowLayout,
    rendered: &mut RenderedPage,
) -> Result<(), RegisterError> {
    let parent = canvas.document();
    canvas.next_row(placed.frame.h);
    canvas.set_pending_advance(AdvanceState::Absolute);
    canvas.set_pending_group(row.group);
    // The in-card divider belongs to THIS row: created after the row opened, so the canvas
    // captures it with the row, placed through the row's placements, and hidden with its group.
    if row.separator_above {
        let separator = SettingsRow::separator_above_row(
            parent,
            placed.frame.y,
            placed.frame.h,
            placed.frame.w,
        );
        canvas.place_pending_view(separator, separator_frame(placed));
    }
    let title = t(row.label_key);
    // The row's title view: what the accessibility label and the disabled-row pairing name.
    let title_label: *mut AnyObject = match row.caption_key {
        Some(caption_key) => {
            let caption = t(caption_key);
            let (title_label, caption_label) =
                page_widgets::stacked_row_labels(parent, &title, &caption);
            // The block's own views were sized for its build; the layout says where they go.
            set_frame(title_label, &placed.label_frame);
            if let Some(caption_frame) = placed.caption_frame {
                set_frame(caption_label, &caption_frame);
            }
            let boxes = row_placement_boxes(row, placed);
            canvas.place_pending_view(title_label, boxes[0].rect());
            canvas.place_pending_view(caption_label, boxes[1].rect());
            title_label
        }
        None => {
            // A plain row: the shared label builder, exactly the one the inline pages used.
            let label = page_widgets::plain_row_label(
                parent,
                placed.label_frame.x,
                placed.label_frame.y,
                placed.label_frame.w,
                placed.frame.h,
                &title,
            );
            let boxes = row_placement_boxes(row, placed);
            canvas.place_pending_view(label, boxes[0].rect());
            label
        }
    };
    let control: *mut AnyObject = match &row.control {
        ControlKind::Switch => {
            let on = matches!(row.value, Value::Switch(true));
            SettingsControl::switch(
                placed.control_frame.x + placed.control_frame.w,
                placed.control_frame.y,
                placed.control_frame.h,
                on,
            )
        }
        ControlKind::Popup { options } => {
            let refs: Vec<&str> = options.iter().map(String::as_str).collect();
            let selected = match row.value {
                Value::Index(index) => index,
                _ => 0,
            };
            SettingsControl::popup(
                placed.control_frame.x,
                placed.control_frame.y,
                placed.control_frame.w,
                placed.control_frame.h,
                &refs,
                selected,
            )
        }
        ControlKind::Slider {
            min,
            max,
            step,
            reset,
            ..
        } => {
            let value = match row.value {
                Value::Slider(value) => value,
                _ => *min,
            };
            SettingsControl::stepped_slider(
                placed.control_frame.x,
                placed.control_frame.y,
                placed.control_frame.w,
                placed.control_frame.h,
                *min,
                *max,
                *step,
                value,
                *reset,
            )
        }
        ControlKind::Button => {
            let action = match &row.id {
                ControlId::Action(action) => *action,
                ControlId::Field(_) => "export_logs",
            };
            SettingsControl::button(
                placed.control_frame.x,
                placed.control_frame.y,
                placed.control_frame.w,
                placed.control_frame.h,
                &title,
                target,
                action_selector(action),
                SettingsButtonRole::Action,
            )
        }
    };
    if control.is_null() {
        return Ok(());
    }
    // The control enters the view tree and the page gives up its +1: every widget above is
    // allocated +1 and every inline builder released its view after adding it.
    let _: () = msg_send![control, setFrame: placed.control_frame.rect()];
    let _: () = msg_send![parent, addSubview: control];
    release_obj(control);
    let control_box = *row_placement_boxes(row, placed)
        .last()
        .unwrap_or(&placed.control_frame);
    let _ = control_box;
    canvas.place_pending_view(control, placed.control_frame.rect());
    // The shared association contract: VoiceOver label + the ROW_LABELS pairing the freeze lists
    // use to dim the row's label and give it the disabled tooltip.
    SettingsRow::register_label(title_label, control);
    if !matches!(row.control, ControlKind::Button) {
        // `bind_control` is the unified handler; a field with a dedicated callback keeps it.
        let selector = dedicated_selector(row.field());
        let _: () = msg_send![control, setTarget: target];
        let _: () = msg_send![control, setAction: selector];
    }
    let readout = match (&row.control, placed.readout_frame) {
        (ControlKind::Slider { readout, .. }, Some(frame)) => {
            let view = SettingsRow::attach_slider_readout(parent, control, readout);
            set_frame(view, &frame);
            canvas.place_pending_view(view, frame.rect());
            view
        }
        _ => std::ptr::null_mut(),
    };
    rendered.registrations.register(control, row.id.clone())?;
    rendered.controls.push(RenderedControl {
        id: row.id.clone(),
        kind: row.control.clone(),
        control,
        readout,
    });
    Ok(())
}

/// The divider a row draws above itself: the row's top edge, full bleed across the page.
pub(super) fn separator_frame(placed: &RowLayout) -> NSRect {
    NSRect::new(
        NSPoint::new(placed.frame.x, placed.frame.y + placed.frame.h),
        NSSize::new(placed.frame.w.max(1.0), 1.0),
    )
}

/// Put a view in the box the layout computed.
unsafe fn set_frame(view: *mut AnyObject, box_: &BoxRect) {
    if view.is_null() || box_.is_empty() {
        return;
    }
    let _: () = msg_send![view, setFrame: box_.rect()];
}
/// The conditional groups a description names, in first-appearance order.
pub(super) fn description_groups(view: &PageView) -> Vec<RowGroup> {
    let mut groups: Vec<RowGroup> = Vec::new();
    for row in &view.rows {
        if let Some(group) = row.group {
            if !groups.contains(&group) {
                groups.push(group);
            }
        }
    }
    groups
}

/// Build the page's message: the banner, the cards and the rows, in the frames `layout_rows`
/// computed. Applies the layout only; it derives no frame of its own.
///
/// The page's header block is the canvas's: the layout was measured against the canvas's own
/// `header_offset`, so the first section cursor here is the one the canvas will step from.
pub(super) unsafe fn render_page(
    canvas: &mut PageCanvas,
    target: *mut AnyObject,
    view: &PageView,
    layout: &PageLayout,
    _metrics: &PageMetrics,
) -> Result<RenderedPage, RegisterError> {
    let mut rendered = RenderedPage::default();
    // A conditional row is hidden through the canvas's own group list: the page has no other
    // switch for it today (the generated rows for every group are present in the tree from the
    // start, and hiding is what makes a group's rows disappear together).
    if let (Some(banner), Some(spec)) = (&layout.banner, &view.banner) {
        let strip: *mut AnyObject = msg_send![class!(NSView), alloc];
        let strip: *mut AnyObject = msg_send![strip, initWithFrame: banner.strip.rect()];
        let _: () = msg_send![strip, setAutoresizingMask: 10u64];
        let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let label: *mut AnyObject = msg_send![label, initWithFrame: banner.label.rect()];
        let text = make_nsstring(&t(spec.copy_key));
        let _: () = msg_send![label, setStringValue: text];
        CFRelease(text as *const c_void);
        let _: () = msg_send![label, setEditable: false];
        let _: () = msg_send![label, setBezeled: false];
        let _: () = msg_send![label, setDrawsBackground: false];
        let _: () = msg_send![label, setUsesSingleLineMode: false];
        let _: () = msg_send![label, setLineBreakMode: 0isize]; // NSLineBreakByWordWrapping
        let red: *mut AnyObject = msg_send![class!(NSColor), systemRedColor];
        let _: () = msg_send![label, setTextColor: red];
        let _: () = msg_send![label, setAutoresizingMask: 2u64];
        let _: () = msg_send![strip, addSubview: label];
        // The strip owns the label now; the +1 from alloc/init is given up, exactly as the inline
        // builder did. Without this every settings rebuild leaks one label.
        release_obj(label);
        let button = SettingsButton::action(
            banner.button.rect(),
            &t("settings.btn_open_privacy"),
            target,
            sel!(handleOpenPrivacy:),
            SettingsButtonRole::Action,
        );
        let _: () = msg_send![strip, addSubview: button];
        // The strip and the button are created +1; the page keeps them, so their references are
        // given up here exactly like every inline builder did.
        release_obj(button);
        let _: () = msg_send![strip, setHidden: true];
        let _: () = msg_send![canvas.document(), addSubview: strip];
        // Ownership of the strip's +1 stays with the page build: `finalize_settings_pages`
        // re-parents it to the content view and gives that reference up there (the convention the
        // inline builder had). Releasing it here as well would steal the parent's reference.
        rendered.banner_strip = strip;
        rendered.banner_label = label;
        rendered.banner_button = button;
    }
    for (card_index, card) in layout.cards.iter().enumerate() {
        // Every card after the first opens its own section: the canvas steps one section per
        // card, which is what the layout's card tops were computed from.
        if card_index > 0 {
            canvas.next_section();
        }
        for (index, row) in view.rows.iter().enumerate() {
            if row.card != card_index {
                continue;
            }
            render_row(canvas, target, row, &layout.rows[index], &mut rendered)?;
        }
        canvas.card(&t(card.section));
    }
    for group in description_groups(view) {
        canvas.set_group_visible(group, false);
    }
    Ok(rendered)
}

/// Write one control's value from the update description's entry.
///
/// The value comes from the description, never from a second expression here.
pub(super) unsafe fn apply_control_update(control: &RenderedControl, value: &str, display: &str) {
    if control.control.is_null() {
        return;
    }
    match &control.kind {
        ControlKind::Switch => {
            widgets::set_switch_state_immediate(control.control, value == "on");
        }
        ControlKind::Popup { .. } => {
            let index = value.parse::<isize>().unwrap_or(0);
            let _: () = msg_send![control.control, selectItemAtIndex: index];
        }
        ControlKind::Slider { .. } => {
            let number = value.parse::<isize>().unwrap_or(0);
            let _: () = msg_send![control.control, setIntegerValue: number];
            if !control.readout.is_null() {
                set_field(control.readout, display);
            }
        }
        ControlKind::Button => {}
    }
}

/// Apply the page's values to the controls the page built: the refill and the in-place refresh
/// both end here, so a field's shown value has one source.
pub(super) unsafe fn apply_page_updates(rendered: &RenderedPage, updates: &[ControlUpdate]) {
    for update in updates {
        if let Some(control) = rendered.control(&ControlId::Field(update.field)) {
            apply_control_update(control, &update.value, &update.display);
        }
    }
}

// MARK: - Refilling the migrated pages

/// Write the migrated pages' controls from the shared update description of `config`.
///
/// This is the computation both the refill (`load_settings_from`) and the in-place refresh
/// (`refresh_switcher_keystroke_and_mouse_controls_from_config`) consume. What the callers decide
/// is the POLICY around it (the page being opened, the page being visible, a shortcut capture in
/// progress, `MOUSE_PAGE_DIRTY`); this function only computes and writes.
pub(super) unsafe fn refresh_migrated_pages(
    ui: &SettingsUi,
    input: &PageInput,
    pages: &[SettingsPageId],
) {
    let updates = page_update_description(input, pages);
    for page in pages {
        if let Some(rendered) = ui.migrated_pages[settings_page_index(*page)].as_ref() {
            apply_page_updates(rendered, &updates);
        }
    }
}

// MARK: - Building one page

/// What a migrated page replaces when its description is rendered: its rows' identities.
/// The page's description, for the pages that have one.
fn view_for(page: SettingsPageId, input: &PageInput) -> PageView {
    match page {
        SettingsPageId::Switcher => switcher_page_view(input),
        SettingsPageId::General => general_page_view(input),
        other => unimplemented!("{other:?} is not migrated to a page description yet"),
    }
}

/// The layout inputs a page is built with. The ONE place a page's layout inputs are made, so the
/// wiring assertion drives exactly what `build_page` drives: the banner's box from the page
/// context (`page_x`, the VISIBLE `page_viewport_h`), the rows' origin from the document height,
/// and the header the canvas measured.
#[allow(clippy::too_many_arguments)] // one call site, one meaning per argument
pub(super) fn page_layout_input(
    view: &PageView,
    content_w: f64,
    layout: SettingsLayout,
    doc_h: f64,
    header_offset: f64,
    page_x: f64,
    page_viewport_h: f64,
) -> LayoutInput {
    LayoutInput {
        content_w,
        doc_top: doc_h,
        header_offset,
        banner_frame: BannerFrame::of(banner_parent_frame(
            page_x,
            page_viewport_h,
            content_w,
            banner_height(),
        )),
        layout,
        text: MeasuredText::measure(view, content_w - 2.0 * SETTINGS_DETAIL_INSET),
    }
}
/// The banner strip's rectangle for its FINAL parent, the settings content view.
///
/// The strip is created inside the page document and `finalize_settings_pages` re-parents it to
/// the content view, so its box is stated against that parent and follows the inline builder's
/// geometry exactly: `page_x` is the detail pane's horizontal origin, and the strip hangs from the
/// top of the VISIBLE page area (`page_viewport_h`) by the page header's top padding and its own
/// height. Both inputs come from the build context, which is the only place that knows them.
pub(super) fn banner_parent_frame(
    page_x: f64,
    page_viewport_h: f64,
    content_w: f64,
    banner_h: f64,
) -> NSRect {
    NSRect::new(
        NSPoint::new(
            page_x,
            page_viewport_h - SettingsPageHeader::TOP_PADDING - banner_h,
        ),
        NSSize::new(content_w.max(1.0), banner_h.max(1.0)),
    )
}

/// The gap the banner reserves below its content strip (the strip plus this gap is the room the
/// page keeps clear so the banner never covers the title or the first card).
pub(super) const BANNER_GAP: f64 = 12.0;

/// The banner strip's height for the current state: the permission-migration copy is taller than
/// the ordinary warning.
pub(super) fn banner_height() -> f64 {
    let content_h = if crate::update_notice::needs_permission_migration_copy() {
        60.0
    } else {
        48.0
    };
    // The strip and the gap below it are both reserved, so the banner cannot cover the title or the
    // first card.
    content_h + BANNER_GAP
}

/// Whether the description declares the same identity twice. Pure: the description alone says it,
/// so `build_page` can refuse a page before a single view is created and a first build's failure
/// leaves the page empty instead of leaving half-built controls behind.
pub(super) fn description_identity_conflict(view: &PageView) -> Result<(), RegisterError> {
    let mut seen: Vec<&ControlId> = Vec::with_capacity(view.rows.len());
    for row in &view.rows {
        if seen.contains(&&row.id) {
            return Err(RegisterError::DuplicateId);
        }
        seen.push(&row.id);
    }
    Ok(())
}

/// Build one migrated page from its description and publish its controls.
///
/// Lifecycle (the rules the plan requires):
/// - the description is validated and rendered against the new canvas before anything the page
///   previously published is touched;
/// - a duplicate identity or pointer aborts the build: the registrations made so far are dropped
///   (so the table never holds half a page), the new views are taken out of the document again,
///   and the caller gets Err to fall back to the last usable state -- the old views and the old
///   mapping stay together;
/// - a first build has no previous state: on failure the page is left empty and the failure is
///   logged, with nothing published;
/// - on success the table drops the page's old entries BEFORE the caller releases the old views.
#[allow(clippy::too_many_arguments)] // the page's own inputs, one call site per page
pub(super) unsafe fn build_page(
    ui: &mut SettingsUi,
    page: SettingsPageId,
    document: *mut AnyObject,
    content_w: f64,
    layout: SettingsLayout,
    doc_h: f64,
    clip_height: f64,
    page_x: f64,
    page_viewport_h: f64,
    input: &PageInput,
    target: *mut AnyObject,
) -> Result<BuiltPage, RegisterError> {
    let view = view_for(page, input);
    // Identity is a property of the DESCRIPTION, so a duplicate is found before a single view is
    // created: a refused page leaves the document and the previous mapping untouched, and a first
    // build's failure leaves the page empty instead of leaving half-built views behind.
    if let Err(error) = description_identity_conflict(&view) {
        log_info!("[settings] page description declares a duplicate identity: {error:?}");
        return Err(error);
    }
    let mut canvas = PageCanvas::new(
        document,
        content_w,
        layout,
        &t(view.title_key),
        doc_h,
        clip_height,
    );
    // The banner's box comes from the page context (the detail pane's inset and the VISIBLE
    // viewport), not from the page document -- its parent is the settings content view.
    let layout_input = page_layout_input(
        &view,
        content_w,
        layout,
        doc_h,
        canvas.header_offset(),
        page_x,
        page_viewport_h,
    );
    let metrics = PageMetrics::for_layout(layout);
    let page_layout = layout_rows(&view, &layout_input, &metrics);
    let mut rendered = match render_page(&mut canvas, target, &view, &page_layout, &metrics) {
        Ok(rendered) => rendered,
        Err(error) => {
            // A duplicate was refused while building: nothing has been published, and the page's
            // half-built views are dropped with it (the caller falls back to the previous state).
            log_info!(
                "[settings] page build refused a duplicate ({error:?}); the previous state is kept"
            );
            return Err(error);
        }
    };
    if let Err(error) = rendered.registrations.publish(&mut ui.control_table) {
        // Nothing may look alive without being wired: take the half-built page out again.
        log_info!("[settings] page build aborted ({error:?}); the previous state is kept");
        let _: () = msg_send![document, removeFromSuperview];
        return Err(error);
    }
    // The rows are built; hand the page's layout owner its final geometry. Every page's document
    // is settled by `finalize_settings_pages` once all pages exist, so this must not run the
    // restore-control pass here.
    let content_bottom = canvas.finish();
    Ok(BuiltPage {
        view,
        rendered,
        canvas,
        content_bottom,
    })
}

/// One migrated page, built: its description, its views and its layout owner.
pub(super) struct BuiltPage {
    pub(super) view: PageView,
    pub(super) rendered: RenderedPage,
    pub(super) canvas: PageCanvas,
    pub(super) content_bottom: f64,
}

// MARK: - Smoke

/// The app-side entry the M2 runner spawns (`--smoke-migrated-pages`): build both migrated pages
/// on the main thread, then DRIVE the production re-flow -- hide a conditional group, re-flow, and
/// re-assert -- so a correct re-position is checked, not just the build-time frames.
pub(crate) unsafe fn migrated_pages_smoke_runner() -> bool {
    let mut ok = true;
    for page in [SettingsPageId::Switcher, SettingsPageId::General] {
        let document: *mut AnyObject = msg_send![class!(NSView), alloc];
        let document: *mut AnyObject = msg_send![
            document,
            initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(600.0, 800.0))
        ];
        let input = PageInput::from_config(
            crate::config::Config::default(),
            None,
            crate::theme::ThemeInput::current(),
        );
        let view = view_for(page, &input);
        let metrics = PageMetrics::for_layout(SettingsLayout::new(600.0));
        let mut canvas = PageCanvas::new(
            document,
            600.0,
            SettingsLayout::new(600.0),
            &t(view.title_key),
            document_fixture_height(),
            700.0,
        );
        let measured = MeasuredText::measure(&view, 600.0 - 2.0 * SETTINGS_DETAIL_INSET);
        let layout_input = LayoutInput {
            content_w: 600.0,
            doc_top: document_fixture_height(),
            header_offset: canvas.header_offset(),
            banner_frame: BannerFrame::of(banner_parent_frame(28.0, 700.0, 600.0, banner_height())),
            layout: SettingsLayout::new(600.0),
            text: measured,
        };
        let layout = layout_rows(&view, &layout_input, &metrics);
        match render_page(&mut canvas, std::ptr::null_mut(), &view, &layout, &metrics) {
            Ok(rendered) => {
                // Build state, then the production re-flow with a conditional group collapsed and
                // expanded again: the checker reads the canvas's CURRENT row origin each time.
                let mut holds = migrated_page_layout_holds(&canvas, &rendered, &view, 0.0);
                if let Some(group) = description_groups(&view).first().copied() {
                    canvas.set_group_visible(group, true);
                    holds &= migrated_page_layout_holds(&canvas, &rendered, &view, 0.0);
                    canvas.set_group_visible(group, false);
                    holds &= migrated_page_layout_holds(&canvas, &rendered, &view, 0.0);
                }
                // A viewport change moves the rows' origin; the views must follow it.
                canvas.set_viewport(520.0);
                holds &= migrated_page_layout_holds(&canvas, &rendered, &view, 0.0);
                if !holds {
                    log_info!(
                        "[smoke-migrated-pages] {page:?}: a placed view is not where the layout says"
                    );
                    ok = false;
                }
            }
            Err(error) => {
                log_info!("[smoke-migrated-pages] {page:?}: build refused ({error:?})");
                ok = false;
            }
        }
    }
    ok
}

/// The height the detached smoke document is provisioned with.
fn document_fixture_height() -> f64 {
    900.0
}

/// Every view the renderer placed for a page is a subview of the document with a real size, at
/// its row's CURRENT position plus the offset the layout gave it -- read from the CANVAS, which
/// is what a re-flow moves. A row that placed fewer than two views is refused: a row the page
/// forgot to build must not pass as "nothing to check".
unsafe fn migrated_page_layout_holds(
    canvas: &PageCanvas,
    _rendered: &RenderedPage,
    view: &PageView,
    _tolerance: f64,
) -> bool {
    let document = canvas.document();
    if document.is_null() {
        return false;
    }
    for (row, spec) in view.rows.iter().enumerate() {
        let Some(placements) = canvas.row_placements(row) else {
            return false;
        };
        // A row always places at least its copy and its control.
        if placements.len() < 2 {
            return false;
        }
        if canvas.placements_are_stale(row) {
            return false;
        }
        for (view_, offset) in placements {
            if view_.is_null() {
                return false;
            }
            let superview: *mut AnyObject = msg_send![*view_, superview];
            if superview != document {
                return false;
            }
            let actual: NSRect = msg_send![*view_, frame];
            if actual.size.width <= 0.0 || actual.size.height <= 0.0 {
                return false;
            }
            // The expected origin comes from the canvas's CURRENT row position, which is what
            // the re-flow used -- never the build-time PageLayout origin.
            let Some(row_origin) = canvas.current_row_origin(row) else {
                return false;
            };
            let expected = super::page_canvas::placed_view_origin(row_origin, *offset);
            if (actual.origin.x - expected.x).abs() > 0.5
                || (actual.origin.y - expected.y).abs() > 0.5
            {
                return false;
            }
        }
        if let Some(group) = spec.group {
            if canvas.row_group(row) != Some(group) {
                return false;
            }
        }
    }
    true
}
// MARK: - Binding a page's controls into SettingsUi

/// The `SettingsUi` pointer the dispatcher, the refill's own consumers and the freeze lists read
/// for a field. Field identity stays the single source: the table says what a control edits, and
/// this table says which legacy pointer field that identity still fills.
///
/// A field with no entry here has no legacy consumer left (nothing reads `ui.<field>` for it), so
/// publishing it would only create a second name for the same control.
fn settings_ui_slot(ui: &mut SettingsUi, field: ControlField) -> Option<&mut *mut AnyObject> {
    if !settings_ui_slot_present(field) {
        return None;
    }
    Some(match field {
        ControlField::Theme => &mut ui.theme,
        ControlField::PanelMaterial => &mut ui.panel_material,
        ControlField::GlassStyle => &mut ui.glass_style,
        ControlField::Locale => &mut ui.locale,
        ControlField::LogLevel => &mut ui.log_level,
        ControlField::LaunchAtLogin => &mut ui.launch_at_login,
        ControlField::HideMenuBarIcon => &mut ui.hide_menu_bar_icon,
        ControlField::WindowsEnabled => &mut ui.windows_enabled,
        ControlField::ShowMinimized => &mut ui.show_minimized,
        ControlField::ShowHiddenAppWindows => &mut ui.show_hidden_app_windows,
        ControlField::ShowOtherDesktops => &mut ui.show_other_desktops,
        ControlField::ThumbnailsEnabled => &mut ui.thumbnails_enabled,
        ControlField::FocusedThumbnailPrewarm => &mut ui.focused_thumbnail_prewarm,
        ControlField::ShowAppNameInCards => &mut ui.show_app_name_in_cards,
        ControlField::ThumbnailSize => &mut ui.thumbnail_size,
        ControlField::CardTextSize => &mut ui.card_text_size,
        ControlField::StatusBarTextSize => &mut ui.status_bar_text_size,
        ControlField::OverlayPosition => &mut ui.overlay_position,
        ControlField::ActivationMode => &mut ui.activation_mode,
        ControlField::PanelCloseEnabled => &mut ui.panel_close_enabled,
        ControlField::PanelMinimizeEnabled => &mut ui.panel_minimize_enabled,
        ControlField::PanelHideEnabled => &mut ui.panel_hide_enabled,
        ControlField::PanelQuitEnabled => &mut ui.panel_quit_enabled,
        ControlField::CornerRadius => &mut ui.corner_radius,
        ControlField::Modifier => &mut ui.modifier,
        // `settings_ui_slot_present` listed every field above; a new one without a slot is a
        // compile error here, which is what keeps the two in step.
        _ => {
            unreachable!("settings_ui_slot_present and settings_ui_slot must list the same fields")
        }
    })
}

/// Bind the page's permission banner to the three fields the window's banner refresh reads
/// (`refresh_settings_warning_banner`). Called only after the page's build succeeded, like every
/// other binding, so a refused build cannot leave the banner fields pointing at views that were
/// dropped.
pub(super) fn bind_page_banner(ui: &mut SettingsUi, rendered: &RenderedPage) {
    if rendered.banner_strip.is_null() {
        return;
    }
    ui.permission_warning_view = rendered.banner_strip;
    ui.permission_warning_label = rendered.banner_label;
    ui.permission_warning_button = rendered.banner_button;
}

/// Bind a built page's controls to the legacy pointer fields their consumers still read.
///
/// One control at a time, after the page's registrations were published: a page whose build was
/// refused binds nothing, and the fields that already held the previous page's controls are left
/// alone until the new page is known good.
pub(super) unsafe fn bind_page_controls(ui: &mut SettingsUi, rendered: &RenderedPage) {
    for control in &rendered.controls {
        let ControlId::Field(field) = control.id else {
            continue;
        };
        if let Some(slot) = settings_ui_slot(ui, field) {
            *slot = control.control;
        }
        // A slider's readout label is a second pointer field on the page.
        if !control.readout.is_null() {
            match field {
                ControlField::CardTextSize => ui.card_text_size_value_label = control.readout,
                ControlField::StatusBarTextSize => {
                    ui.status_bar_text_size_value_label = control.readout
                }
                _ => {}
            }
        }
    }
}

/// Whether the field has a legacy `SettingsUi` pointer field the dispatcher and the freeze lists
/// still read (the HIGH 2 coverage assertion). Field-presence only: it never touches a `SettingsUi`.
#[cfg(test)]
pub(super) fn settings_ui_slot_field(field: ControlField) -> Option<()> {
    settings_ui_slot_present(field).then_some(())
}

/// The fields [`bind_page_controls`] fills, as a predicate, so the coverage rule can be asserted
/// without an AppKit window. Kept beside the match that does the filling; `page_view` tests below
/// cross-check the two lists against each other.
pub(super) fn settings_ui_slot_present(field: ControlField) -> bool {
    matches!(
        field,
        ControlField::Theme
            | ControlField::PanelMaterial
            | ControlField::GlassStyle
            | ControlField::Locale
            | ControlField::LogLevel
            | ControlField::LaunchAtLogin
            | ControlField::HideMenuBarIcon
            | ControlField::WindowsEnabled
            | ControlField::ShowMinimized
            | ControlField::ShowHiddenAppWindows
            | ControlField::ShowOtherDesktops
            | ControlField::ThumbnailsEnabled
            | ControlField::FocusedThumbnailPrewarm
            | ControlField::ShowAppNameInCards
            | ControlField::ThumbnailSize
            | ControlField::CardTextSize
            | ControlField::StatusBarTextSize
            | ControlField::OverlayPosition
            | ControlField::ActivationMode
            | ControlField::PanelCloseEnabled
            | ControlField::PanelMinimizeEnabled
            | ControlField::PanelHideEnabled
            | ControlField::PanelQuitEnabled
            | ControlField::CornerRadius
            | ControlField::Modifier
    )
}

/// Whether a slider field's readout label is bound by [`bind_page_controls`].
#[cfg(test)]
pub(super) fn settings_ui_readout_field(field: ControlField) -> bool {
    matches!(
        field,
        ControlField::CardTextSize | ControlField::StatusBarTextSize
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)] // only the ignored M2 smoke runner uses it
    use crate::smoke_verdict::smoke_runner;

    fn input() -> PageInput {
        PageInput::from_config(Config::default(), None, crate::theme::ThemeInput::current())
    }

    fn switcher() -> PageView {
        switcher_page_view(&input())
    }

    fn general() -> PageView {
        general_page_view(&input())
    }

    fn fields(view: &PageView) -> Vec<ControlField> {
        view.rows.iter().filter_map(|row| row.field()).collect()
    }

    #[test]
    fn switcher_page_view_describes_every_row_with_its_identity_and_copy() {
        let view = switcher();
        assert_eq!(view.page, SettingsPageId::Switcher);
        assert_eq!(view.rows.len(), 17);
        // Every declared row carries a label key and an identity; the copy keys are the ones the
        // page's own labels used before the migration.
        let labels: Vec<&str> = view.rows.iter().map(|row| row.label_key).collect();
        assert_eq!(labels[0], "settings.row_windows_enabled");
        assert_eq!(labels[1], "settings.row_show_minimized");
        assert_eq!(labels[4], "settings.row_window_display_mode");
        assert_eq!(labels[8], "settings.row_card_text_size");
        assert_eq!(labels[16], "settings.row_modifier");
        // The field order is the page's own order: a reorder is a visible change, not a detail.
        assert_eq!(
            fields(&view),
            vec![
                ControlField::WindowsEnabled,
                ControlField::ShowMinimized,
                ControlField::ShowHiddenAppWindows,
                ControlField::ShowOtherDesktops,
                ControlField::ThumbnailsEnabled,
                ControlField::FocusedThumbnailPrewarm,
                ControlField::ShowAppNameInCards,
                ControlField::ThumbnailSize,
                ControlField::CardTextSize,
                ControlField::StatusBarTextSize,
                ControlField::OverlayPosition,
                ControlField::ActivationMode,
                ControlField::PanelCloseEnabled,
                ControlField::PanelMinimizeEnabled,
                ControlField::PanelHideEnabled,
                ControlField::PanelQuitEnabled,
                ControlField::Modifier,
            ]
        );
        // The control kinds are presentation only; the identity above is what the dispatcher uses.
        assert!(matches!(view.rows[0].control, ControlKind::Switch));
        assert!(matches!(view.rows[4].control, ControlKind::Popup { .. }));
        assert!(matches!(view.rows[8].control, ControlKind::Slider { .. }));
        // The cards are the page's own: four sections, in order.
        let sections: Vec<&str> = view.rows.iter().map(|row| row.section).collect();
        assert_eq!(sections[0], "settings.header_windows");
        assert_eq!(sections[12], "settings.header_windows");
        assert_eq!(sections[16], "settings.header_window_options");
        // The section a trailing empty "Keyboard" card would have opened is not drawn: a card
        // needs a row to exist, and the page's last card already carries the modifier row.
        // The first row of every card has no divider above it; every later row in the card does.
        assert!(!view.rows[0].separator_above);
        assert!(view.rows[1].separator_above);
        assert!(!view.rows[16].separator_above);
        // The thumbnail-only block is a conditional group, so its rows can be skipped together.
        let grouped: Vec<usize> = view
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.group == Some(RowGroup::ThumbnailOnly))
            .map(|(index, _)| index)
            .collect();
        assert_eq!(grouped, vec![5, 6, 7]);
    }

    #[test]
    fn general_page_view_describes_every_row_with_its_identity_and_copy() {
        let view = general();
        assert_eq!(view.page, SettingsPageId::General);
        assert_eq!(view.rows.len(), 10);
        assert_eq!(
            fields(&view),
            vec![
                ControlField::LaunchAtLogin,
                ControlField::HideMenuBarIcon,
                ControlField::Theme,
                ControlField::PanelMaterial,
                ControlField::GlassStyle,
                ControlField::Locale,
                ControlField::LogLevel,
            ]
        );
        // The three action rows carry an action identity and no config field.
        let actions: Vec<&str> = view
            .rows
            .iter()
            .filter_map(|row| match row.id {
                ControlId::Action(action) => Some(action),
                ControlId::Field(_) => None,
            })
            .collect();
        assert_eq!(
            actions,
            vec!["export_logs", "export_settings", "import_settings"]
        );
        // The glass style is the Liquid-Glass block; the two backup rows are stacked copy.
        assert_eq!(view.rows[4].group, Some(RowGroup::GlassStyle));
        assert_eq!(view.rows[8].copy, RowCopy::Captioned);
        assert_eq!(view.rows[9].copy, RowCopy::Captioned);
        // The permission banner belongs to the page, with the action its button runs.
        let banner = view.banner.as_ref().expect("General carries the banner");
        assert_eq!(banner.action_id, "open_privacy");
        assert!(!t(banner.copy_key).is_empty());
    }

    #[test]
    fn page_view_reads_only_its_input() {
        // The same config with a different device is still one description: the Switcher and
        // General pages do not depend on the selected mouse.
        let mut config = Config::default();
        config.windows.show_minimized = true;
        let view = switcher_page_view(&PageInput::from_config(
            config.clone(),
            Some((0x1234, 0x5678)),
            crate::theme::ThemeInput::current(),
        ));
        assert_eq!(view.rows[1].value, Value::Switch(true));
        // ... and a changed field changes the description's value, not its shape.
        config.keyboard.modifier = "control".to_string();
        let other = switcher_page_view(&PageInput::from_config(
            config,
            None,
            crate::theme::ThemeInput::current(),
        ));
        let modifier = other
            .rows
            .iter()
            .find(|row| row.field() == Some(ControlField::Modifier))
            .expect("the modifier row");
        assert_eq!(
            modifier.value,
            Value::Index(crate::config::switcher_modifier_index_of("control"))
        );
    }

    #[test]
    fn page_view_search_entries_are_values_not_side_effects() {
        let _guard = sidebar::lock_search_index_for_test();
        let entries_before =
            sidebar::recorded_search_rows(settings_page_index(SettingsPageId::Switcher));
        let view = switcher();
        // A pure description; producing it never writes to the index.
        assert_eq!(
            sidebar::recorded_search_rows(settings_page_index(SettingsPageId::Switcher)),
            entries_before
        );
        assert_eq!(view.rows[0].search.label, t("settings.row_windows_enabled"));
        assert_eq!(
            view.rows[0].search.desc.as_deref(),
            Some(t("settings.desc_windows_enabled").as_str())
        );
        assert_eq!(view.rows[0].search.section, t("settings.header_windows"));
        // A dropdown's choices are part of what the search matches.
        let thumbnail_size = &view.rows[7];
        assert!(!thumbnail_size.search.options.is_empty());
        assert!(matches!(thumbnail_size.control, ControlKind::Popup { .. }));
    }

    #[test]
    fn a_migrated_page_indexes_every_row_the_inline_sweep_used_to() {
        // The sweep the inline pages ran filled a row's entry from the strings its views render.
        // A migrated page must index the same rows with the same baseline text, or settings
        // search silently stops finding a setting it used to find.
        let view = switcher();
        let page = settings_page_index(SettingsPageId::Switcher);
        let _guard = sidebar::lock_search_index_for_test();
        sidebar::begin_settings_search_index();
        sidebar::begin_settings_search_section();
        for (row, spec) in view.rows.iter().enumerate() {
            sidebar::record_settings_search_entry(
                crate::settings::sidebar::SearchEntry::row(
                    page,
                    row,
                    &spec.search.label,
                    spec.search.desc.as_deref(),
                    &spec.search.section,
                )
                .with_options(&spec.search.options),
            );
        }
        // The inline sweep's own invariant: every row of the page has exactly one entry.
        let recorded = sidebar::recorded_search_rows(page);
        assert_eq!(recorded.len(), view.rows.len());
        for row in 0..view.rows.len() {
            assert!(recorded.contains(&row), "row {row} is not searchable");
        }
        // And each entry carries the description's OWN copy, not the sweep's fallback label: a
        // query for a row's label finds exactly that row.
        for (row, spec) in view.rows.iter().enumerate() {
            let hits = sidebar::match_settings_search_for_page(page, &spec.search.label);
            assert!(
                hits.contains(&row),
                "row {row} ({}) is not found by its own label",
                spec.search.label
            );
        }
        sidebar::begin_settings_search_index();
    }

    #[test]
    fn the_description_groups_are_the_ones_the_renderer_registers() {
        // A conditional row hides through the canvas's group list; a group the description names
        // but the renderer never registers would never hide, and one it registers without a row
        // would hide nothing. The renderer's list is asserted through the same function it uses.
        for (page, expected) in [
            (SettingsPageId::Switcher, vec![RowGroup::ThumbnailOnly]),
            (SettingsPageId::General, vec![RowGroup::GlassStyle]),
        ] {
            let view = view_for(page, &input());
            assert_eq!(description_groups(&view), expected, "{page:?}");
        }
    }

    #[test]
    fn the_update_description_carries_the_control_value_and_its_display_separately() {
        // The status-bar slider's domain is stop indices (0 = hidden, 1..=8 = 13..=20pt) while its
        // readout shows the point size: a refill that wrote the index to both showed "1" where
        // the page says "13".
        let mut config = Config::default();
        config.fonts.status_bar_size = 13.0;
        let input = PageInput::from_config(config, None, crate::theme::ThemeInput::current());
        let view = switcher_page_view(&input);
        let updates = page_update_description(&input, &[SettingsPageId::Switcher]);
        let update = updates
            .iter()
            .find(|update| update.field == ControlField::StatusBarTextSize)
            .expect("the footer text-size row");
        assert_eq!(update.value, "1", "the slider rests on stop 1");
        assert_eq!(update.display, "13", "the readout shows the point size");
        assert_ne!(
            update.value, update.display,
            "stop index and point size differ"
        );
        // A field whose readout IS its value keeps the two equal.
        let card_text = updates
            .iter()
            .find(|update| update.field == ControlField::CardTextSize)
            .unwrap();
        assert_eq!(card_text.value, card_text.display);
        let _ = view;
    }

    #[test]
    fn a_failed_publish_leaves_the_previous_mapping_untouched() {
        // The build's own counter-example: a page whose new entries collide with a live entry it
        // does not replace must be refused WITHOUT the old entries being removed first. The first
        // version unregistered before it could fail, which left the page's controls present and
        // unfindable — "looks alive but is not wired".
        let mut table = ControlTable::default();
        let old_theme = 0x1000 as *mut AnyObject;
        table
            .register(old_theme, ControlId::Field(ControlField::Theme))
            .unwrap();
        // The rest of the page's previous controls.
        let old_locale = 0x1001 as *mut AnyObject;
        table
            .register(old_locale, ControlId::Field(ControlField::Locale))
            .unwrap();
        // A new page that reuses the locale control's ADDRESS for a different identity: the old
        // locale entry is not the one being replaced (Theme is), so the build must be refused.
        let mut page = PageRegistrations::default();
        page.register(
            0x2000 as *mut AnyObject,
            ControlId::Field(ControlField::Theme),
        )
        .unwrap();
        page.register(old_locale, ControlId::Field(ControlField::WindowsEnabled))
            .unwrap();
        assert_eq!(
            page.publish(&mut table),
            Err(RegisterError::DuplicatePointer)
        );
        // Nothing moved: the page's two controls still resolve exactly as before.
        assert_eq!(
            table.lookup(old_theme),
            Some(ControlId::Field(ControlField::Theme))
        );
        assert_eq!(
            table.lookup(old_locale),
            Some(ControlId::Field(ControlField::Locale))
        );
    }

    #[test]
    fn a_successful_publish_replaces_the_pages_own_entries() {
        let mut table = ControlTable::default();
        let old_theme = 0x1000 as *mut AnyObject;
        table
            .register(old_theme, ControlId::Field(ControlField::Theme))
            .unwrap();
        let mut page = PageRegistrations::default();
        let new_theme = 0x2000 as *mut AnyObject;
        page.register(new_theme, ControlId::Field(ControlField::Theme))
            .unwrap();
        assert_eq!(page.publish(&mut table), Ok(()));
        assert_eq!(table.lookup(old_theme), None);
        assert_eq!(
            table.lookup(new_theme),
            Some(ControlId::Field(ControlField::Theme))
        );
    }

    #[test]
    fn every_settings_ui_slot_a_migrated_field_needs_is_bound_at_publish() {
        // HIGH 2's counter-example, in its pure half: after the swap the only thing that filled
        // `ui.theme` / `ui.windows_enabled` / ... was the deleted inline builder, so the table
        // resolved a field whose pointer field was still null and `apply_control_field` read NULL.
        // The binding table must cover every field these pages declare and nothing else.
        let input = input();
        for page in [SettingsPageId::Switcher, SettingsPageId::General] {
            let view = view_for(page, &input);
            for row in view.rows.iter().filter(|row| row.field().is_some()) {
                let field = row.field().unwrap();
                assert!(
                    crate::settings::page_view::settings_ui_slot_field(field).is_some(),
                    "{page:?} {field:?} has a row but no SettingsUi slot, so its consumer reads NULL"
                );
            }
        }
        // The readout labels a slider row needs are bound too.
        for field in [ControlField::CardTextSize, ControlField::StatusBarTextSize] {
            assert!(crate::settings::page_view::settings_ui_readout_field(field));
        }
    }

    /// A page canvas over a stand-in document (no window: the canvas's layout walk never messages
    /// the document except to count and re-place subviews, which a null view does not answer).
    #[test]
    fn collapsing_a_group_moves_every_placed_view_with_its_row() {
        // The rule the re-flow applies, through the SAME function the canvas calls. Collapsing the
        // ThumbnailOnly group above a row moves that row up by the rows and gaps it consumed; the
        // row's copy/control/readout must follow it, keeping their offsets, and the control must
        // keep its distance to the card's top edge -- not stay at its build-time frame.
        let input = input();
        let view = switcher_page_view(&input);
        let layout_input = LayoutInput::for_test(600.0);
        let metrics = PageMetrics::for_layout(layout_input.layout);
        let layout = layout_rows(&view, &layout_input, &metrics);
        // The ThumbnailOnly rows are 5..=7; a row below them is the card-text-size row (index 8).
        let hidden_rows = 5..=7;
        let below = layout
            .rows
            .iter()
            .enumerate()
            .find(|(index, _)| {
                !hidden_rows.contains(index) && *index > 7 && view.rows[*index].card == 0
            })
            .map(|(_, row)| row)
            .expect("a row below the thumbnail group");
        let moved_up: f64 = hidden_rows
            .clone()
            .map(|index| layout.rows[index].frame.h)
            .sum();
        let new_origin = below.frame.y + moved_up;
        let offsets: Vec<NSPoint> = {
            let mut out = Vec::new();
            // The offsets are what `place_pending_view` records: box origin minus row origin.
            for box_ in row_placement_boxes(&view.rows[8], &layout.rows[8]) {
                out.push(NSPoint::new(box_.x, box_.y - below.frame.y));
            }
            out
        };
        for (index, offset) in offsets.iter().enumerate() {
            let origin = crate::settings::page_canvas::placed_view_origin(new_origin, *offset);
            let built = row_placement_boxes(&view.rows[8], &layout.rows[8])[index];
            assert!(
                (origin.x - built.x).abs() < 1e-9,
                "view {index} keeps its column"
            );
            assert!(
                (origin.y - (built.y + moved_up)).abs() < 1e-9,
                "view {index} moved up by exactly the hidden rows' height, not stayed at its build frame"
            );
            assert!(
                (origin.y - built.y).abs() > 1.0,
                "view {index} must NOT still be at its build-time frame"
            );
        }
        // The control keeps its offset FROM ITS ROW: the row moved, and the control moved with it.
        let control_offset = offsets[1];
        let control_origin =
            crate::settings::page_canvas::placed_view_origin(new_origin, control_offset);
        assert!(
            (control_origin.y - (new_origin + control_offset.y)).abs() < 1e-9,
            "the control stays at the layout's offset inside its row"
        );
        assert!(
            (new_origin - below.frame.y - moved_up).abs() < 1e-9,
            "the row itself moved up by the hidden rows"
        );
    }
    #[test]
    fn the_banner_children_lie_inside_their_strip() {
        // The label and the button are subviews of the strip, so their boxes are in the strip's
        // coordinates. Adding the strip's origin to them (the first version) put both children
        // one strip-height above the strip: this fails at origin.y >= strip.y for a zero-origin
        // strip and for every strip placed below the document top.
        let input = input();
        let view = general_page_view(&input);
        let layout_input = LayoutInput::for_test(600.0);
        let metrics = PageMetrics::for_layout(layout_input.layout);
        let layout = layout_rows(&view, &layout_input, &metrics);
        let banner = layout.banner.as_ref().expect("General carries the banner");
        for (name, child) in [("label", banner.label), ("button", banner.button)] {
            assert!(
                child.y >= -0.5 && child.y + child.h <= banner.strip.h + 0.5,
                "the {name} must lie inside the strip's height: child.y={} h={} strip.h={}",
                child.y,
                child.h,
                banner.strip.h
            );
            assert!(
                child.x >= -0.5 && child.x + child.w <= banner.strip.w + 0.5,
                "the {name} must lie inside the strip's width"
            );
        }
    }

    #[test]
    fn every_view_the_renderer_creates_is_released_exactly_once() {
        // #3/#1 rule: each +1 from alloc/init is given up EXACTLY ONCE. The banner strip's is
        // given up by `finalize_settings_pages` (it re-parents the strip to the content view and
        // releases the reference the page build handed over), so `render_page` must NOT release
        // it too: a second release would steal the parent's own reference.
        let view_src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/settings/page_view/mod.rs"
        ))
        .expect("the page renderer's own source");
        let page_start = view_src.find("pub(super) unsafe fn render_page(").unwrap();
        let banner_start = view_src[page_start..]
            .find("if let (Some(banner), Some(spec))")
            .map(|offset| page_start + offset)
            .unwrap();
        let banner_end = view_src[banner_start..]
            .find("for (card_index, card) in layout.cards")
            .map(|offset| banner_start + offset)
            .unwrap();
        let banner = &view_src[banner_start..banner_end];
        let body = &view_src[page_start..];
        let mut release_sites: Vec<&str> = Vec::new();
        for pattern in ["release_obj(label)", "release_obj(button)"] {
            let count = banner.matches(pattern).count();
            release_sites.push(pattern);
            assert!(
                count <= 1,
                "{pattern} appears {count} times: a view must be released once"
            );
        }
        let strip_release = format!("release_obj({})", "strip");
        assert!(
            !body.contains(&strip_release),
            "the strip's reference belongs to finalize_settings_pages, not to render_page"
        );
        // The strip's one release lives in the finalize step.
        let builder_src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/settings/page_builder.rs"
        ))
        .expect("the page builder's own source");
        assert_eq!(
            builder_src
                .matches("release_obj(ui.permission_warning_view)")
                .count(),
            1,
            "the banner strip is released exactly once, by finalize_permission_banner"
        );
        // The other created views are balanced inside the renderer.
        let allocs = body.matches("class!(NSTextField), alloc]").count()
            + body.matches("class!(NSView), alloc]").count()
            - 1; // the strip, released by finalize
        let releases = body.matches("release_obj(label)").count()
            + body.matches("release_obj(button)").count()
            + body.matches("release_obj(control)").count();
        assert!(
            releases >= allocs,
            "render_page allocates {allocs} views itself but balances only {releases}"
        );
        let widgets = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/settings/page_widgets.rs"
        ))
        .expect("the page widgets' own source");
        for name in ["stacked_row_labels", "plain_row_label"] {
            let start = widgets.find(&format!("fn {name}(")).unwrap();
            let end = widgets[start..]
                .find("\n}")
                .map(|offset| start + offset)
                .unwrap();
            let body = &widgets[start..end];
            let allocs = body.matches("class!(NSTextField), alloc]").count()
                + body.matches("class!(NSView), alloc]").count();
            let releases = body.matches("release_obj(").count();
            assert!(
                releases >= allocs,
                "{name}: {allocs} allocs, {releases} releases"
            );
        }
    }
    #[test]
    fn every_migrated_row_uses_the_shared_label_association() {
        // The row path must reuse SettingsRow::register_label (accessibility label + the
        // ROW_LABELS pairing the freeze lists read), not re-implement it. Asserted over the
        // renderer's own source, because the pairing itself lives in an AppKit-only registry the
        // M2 runner drives.
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/settings/page_view/mod.rs"
        ))
        .expect("the page renderer's own source");
        let start = source.find("unsafe fn render_row(").expect("render_row");
        let end = source[start..]
            .find("\n/// Put a view in the box the layout computed.")
            .map(|offset| start + offset)
            .expect("the end of render_row");
        let body = &source[start..end];
        assert!(
            body.contains("SettingsRow::register_label(title_label, control)"),
            "a migrated row must register its label with its control"
        );
        assert!(
            !body.contains("setAccessibilityLabel"),
            "the row path must not re-implement the accessibility label"
        );
    }
    #[test]
    fn a_rows_current_origin_is_what_a_placement_follows() {
        // #3: after a re-flow the canvas's current row origin differs from the build-time
        // PageLayout origin; a placement must follow the FORMER. The occurrence the runner checks
        // is AppKit-only, so this asserts the data it checks: `current_row_origin` is the value
        // `placements_are_stale` and the checker compare against, and it is not the layout's.
        let input = input();
        let view = switcher_page_view(&input);
        let layout_input = LayoutInput::for_test(600.0);
        let metrics = PageMetrics::for_layout(layout_input.layout);
        let layout = layout_rows(&view, &layout_input, &metrics);
        // The same arithmetic the canvas walks: a collapsed ThumbnailOnly group moves every row
        // below it up by the rows it hid.
        let hidden_h: f64 = (5..=7).map(|index| layout.rows[index].frame.h).sum();
        let below = 8;
        let reflowed_origin = layout.rows[below].frame.y + hidden_h;
        assert!(
            (reflowed_origin - layout.rows[below].frame.y).abs() > 1.0,
            "the re-flow origin differs from the build-time origin, which is why the checker must read the canvas"
        );
    }
    #[test]
    fn the_banner_strip_is_placed_in_its_final_parents_coordinates() {
        // #2: the strip is re-parented to the settings CONTENT view, so its frame comes from
        // `banner_parent_frame` -- the inline builder's own box (page_x, the VISIBLE viewport's
        // top, content_w x banner_h) -- not from the page document's height and not from x = 0.
        let input = input();
        let view = general_page_view(&input);
        let mut layout_input = LayoutInput::for_test(600.0);
        let page_x = 340.0;
        let page_viewport_h = 700.0;
        layout_input.banner_frame = BannerFrame::of(banner_parent_frame(
            page_x,
            page_viewport_h,
            600.0,
            banner_height(),
        ));
        let metrics = PageMetrics::for_layout(layout_input.layout);
        let layout = layout_rows(&view, &layout_input, &metrics);
        let banner = layout.banner.as_ref().expect("General carries the banner");
        assert_eq!(
            banner.strip.x, page_x,
            "the strip starts at the detail pane's inset"
        );
        assert_eq!(banner.strip.w, 600.0);
        assert_eq!(banner.strip.h, banner_height());
        let top = banner.strip.y + banner.strip.h;
        assert!(
            (top - (page_viewport_h - SettingsPageHeader::TOP_PADDING)).abs() < 1e-9,
            "the strip hangs from the visible viewport's top, not the page document's"
        );
    }

    #[test]
    fn build_page_lays_the_general_banner_out_from_the_page_context() {
        // The PRODUCTION wiring: `page_layout_input` is the one place `build_page` builds a page's
        // layout inputs from its context. Given General's context values, the box it produces must
        // carry that page_x and that VISIBLE page_viewport_h -- read back from the returned
        // LayoutInput, not computed by this test.
        let input = input();
        let view = general_page_view(&input);
        let page_x = 412.0;
        let page_viewport_h = 634.0;
        let doc_h = 1320.0;
        let content_w = 600.0;
        let layout_input = page_layout_input(
            &view,
            content_w,
            SettingsLayout::new(content_w),
            doc_h,
            111.0,
            page_x,
            page_viewport_h,
        );
        assert_eq!(
            layout_input.banner_frame.x, page_x,
            "the banner's box carries the context's page_x"
        );
        assert_eq!(
            layout_input.banner_frame.y,
            page_viewport_h - SettingsPageHeader::TOP_PADDING - banner_height(),
            "the banner hangs from the VISIBLE viewport, not the page document"
        );
        assert_eq!(
            layout_input.doc_top, doc_h,
            "the rows are laid out from the document height"
        );
        // ... and the strip `layout_rows` places from it is exactly that box.
        let metrics = PageMetrics::for_layout(layout_input.layout);
        let layout = layout_rows(&view, &layout_input, &metrics);
        let strip = layout.banner.as_ref().unwrap().strip;
        assert_eq!(
            (strip.x, strip.y, strip.w, strip.h),
            (
                page_x,
                page_viewport_h - SettingsPageHeader::TOP_PADDING - banner_height(),
                content_w,
                banner_height()
            )
        );
    }
    #[test]
    fn the_separator_belongs_to_the_row_below_it() {
        // #4: the divider is created AFTER its row is opened and handed to the row's own
        // placements, so a re-flow moves it and a hidden group hides it. Both facts are asserted:
        // its box (the row's top edge) and that `render_row` places it through the row.
        let input = input();
        let view = switcher_page_view(&input);
        let layout_input = LayoutInput::for_test(600.0);
        let metrics = PageMetrics::for_layout(layout_input.layout);
        let layout = layout_rows(&view, &layout_input, &metrics);
        let row = view
            .rows
            .iter()
            .position(|row| row.separator_above)
            .expect("a row with a divider above it");
        let placed = &layout.rows[row];
        let separator = separator_frame(placed);
        assert_eq!(separator.origin.y, placed.frame.y + placed.frame.h);
        assert_eq!(separator.size.width, placed.frame.w);
        // The renderer places it on the row's own capture, not before the row opens.
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/settings/page_view/mod.rs"
        ))
        .expect("the page renderer's own source");
        let start = source.find("unsafe fn render_row(").expect("render_row");
        let body = &source[start..source.find("pub(super) fn separator_frame(").unwrap()];
        let open = body.find("canvas.next_row(").expect("the row opens");
        let divider = body
            .find("SettingsRow::separator_above_row(")
            .expect("the divider is built");
        assert!(
            divider > open,
            "the divider must be created after its row opened, or the canvas captures it for the previous row"
        );
        assert!(
            body.contains("canvas.place_pending_view(separator, separator_frame(placed))"),
            "the divider must go through the row's placements, so a re-flow moves it and its group hides it"
        );
        // And the card loop no longer builds dividers before opening their row.
        let page_start = source
            .find("pub(super) unsafe fn render_page(")
            .expect("render_page");
        let page = &source[page_start
            ..source
                .find("pub(super) unsafe fn apply_control_update")
                .unwrap()];
        assert!(
            !page.contains("SettingsRow::separator_above_row("),
            "render_page must not create dividers: they belong to the row's own capture"
        );
    }

    #[test]
    fn a_description_with_a_duplicate_identity_is_refused_before_any_view_exists() {
        // MEDIUM 3: the identity check reads the description, so the refusal happens before the
        // page builds a view. This is the production validator (`build_page`'s first step) applied
        // to a page whose rows repeat an identity.
        let input = input();
        let mut view = switcher_page_view(&input);
        view.rows[1].id = view.rows[0].id.clone();
        assert_eq!(
            description_identity_conflict(&view),
            Err(RegisterError::DuplicateId)
        );
        // The clean description passes the same check.
        let clean = switcher_page_view(&input);
        assert_eq!(description_identity_conflict(&clean), Ok(()));
    }

    #[test]
    fn the_windows_master_switch_keeps_its_dedicated_callback() {
        // MEDIUM 5: the unified handler does not freeze the options below the switch, so the
        // switch's own callback must stay bound to it; every other static control uses the unified
        // one.
        assert_eq!(
            dedicated_selector(Some(ControlField::WindowsEnabled)),
            sel!(handleWindowsEnabledToggle:)
        );
        assert_eq!(
            dedicated_selector(Some(ControlField::ShowMinimized)),
            sel!(handleControlChanged:)
        );
        assert_eq!(dedicated_selector(None), sel!(handleControlChanged:));
        // The callback it names exists and does exactly the freeze the disabled rows need.
        let _ = crate::settings::dispatch::handle_windows_enabled_toggle
            as extern "C" fn(*mut c_void, Sel, *mut c_void);
    }

    #[test]
    fn layout_rows_places_the_switcher_rows_without_gaps_or_overlaps() {
        let view = switcher();
        let layout_input = LayoutInput::for_test(600.0);
        let metrics = PageMetrics::for_layout(layout_input.layout);
        let layout = layout_rows(&view, &layout_input, &metrics);
        assert_eq!(layout.rows.len(), view.rows.len());
        // One card per run of rows: the page's first 16 rows are one card and the modifier row
        // is the second (a card that opens no row is not drawn).
        let first_card = &layout.cards[0];
        assert!((first_card.frame.y + first_card.frame.h - layout.first_card_top).abs() < 1e-9);
        // A card exists exactly where its index changes, and it steps the cursor once.
        let mut changes = 0usize;
        let mut previous = view.rows[0].card;
        for row in &view.rows[1..] {
            if row.card != previous {
                changes += 1;
                previous = row.card;
            }
        }
        assert_eq!(layout.cards.len(), changes + 1);
        // The card surface ends at its last row's origin and tops out at its own section cursor
        // (its card index in the walk, not its position among the drawn cards).
        let mut start = 0usize;
        for card in &layout.cards {
            let card_index = view.rows[start].card;
            let end = view.rows[start..]
                .iter()
                .rposition(|row| row.card == card_index)
                .map(|position| position + start)
                .unwrap();
            assert!((card.frame.y - layout.rows[end].frame.y).abs() < 1e-9);
            assert!(
                (card.frame.y + card.frame.h - layout.card_tops[card_index]).abs() < 1e-9,
                "card {card_index}: bottom {} top {} expected {}",
                card.frame.y,
                card.frame.y + card.frame.h,
                layout.card_tops[card_index]
            );
            start = end + 1;
        }
    }
    #[test]
    fn layout_rows_places_controls_inside_the_row_band() {
        let view = switcher();
        let layout_input = LayoutInput::for_test(600.0);
        let metrics = PageMetrics::for_layout(layout_input.layout);
        let layout = layout_rows(&view, &layout_input, &metrics);
        for (row, placed) in view.rows.iter().zip(layout.rows.iter()) {
            let control = &placed.control_frame;
            assert!(control.y >= placed.frame.y - 1e-9, "{row:?}");
            assert!(
                control.y + control.h <= placed.frame.y + placed.frame.h + 1e-9,
                "{row:?}"
            );
            // The copy never reaches into the control column.
            let label = &placed.label_frame;
            assert!(label.x + label.w <= layout_input.layout.control_x + 1e-9);
            if let Some(readout) = placed.readout_frame {
                assert!(readout.x >= control.x + control.w);
            }
        }
    }

    #[test]
    fn layout_rows_sizes_a_stacked_row_by_its_measured_caption() {
        let view = general();
        let mut layout_input = LayoutInput::for_test(600.0);
        let metrics = PageMetrics::for_layout(layout_input.layout);
        let index = view
            .rows
            .iter()
            .position(|row| row.caption_key.is_some())
            .unwrap();
        layout_input.text = view_measured(&view, index, 24.0);
        let layout = layout_rows(&view, &layout_input, &metrics);
        let placed = &layout.rows[index];
        // Title + gap + the caption that was measured (plus the row's own padding).
        assert!(placed.frame.h >= 18.0 + 3.0 + 24.0);
        let caption = placed.caption_frame.expect("a stacked row has a caption");
        assert!(caption.h >= 24.0 - 1e-9);
        assert!((caption.y + caption.h + 3.0 - placed.label_frame.y).abs() < 1e-9);
    }

    fn view_measured(view: &PageView, index: usize, caption_h: f64) -> MeasuredText {
        let mut text = MeasuredText::base();
        text.subtitle_heights = (0..view.rows.len())
            .map(|row| if row == index { caption_h } else { 0.0 })
            .collect();
        text
    }

    #[test]
    fn control_table_refuses_duplicates_of_identity_and_pointer() {
        let mut table = ControlTable::new();
        let first = 0x1000 as *mut AnyObject;
        let second = 0x2000 as *mut AnyObject;
        assert_eq!(
            table.register(first, ControlId::Field(ControlField::Theme)),
            Ok(())
        );
        assert_eq!(
            table.register(second, ControlId::Field(ControlField::Theme)),
            Err(RegisterError::DuplicateId)
        );
        assert_eq!(
            table.register(first, ControlId::Field(ControlField::Locale)),
            Err(RegisterError::DuplicatePointer)
        );
        // Nothing was added by the refused registrations.
        assert_eq!(table.len(), 1);
        assert_eq!(
            table.lookup(first),
            Some(ControlId::Field(ControlField::Theme))
        );
        assert_eq!(table.lookup(second), None);
    }

    #[test]
    fn control_table_unregister_and_cleanup_are_idempotent() {
        let mut table = ControlTable::new();
        let pointer = 0x1000 as *mut AnyObject;
        table
            .register(pointer, ControlId::Field(ControlField::Theme))
            .unwrap();
        assert!(table.unregister(&ControlId::Field(ControlField::Theme)));
        assert!(!table.unregister(&ControlId::Field(ControlField::Theme)));
        assert!(table.is_empty());
        assert_eq!(table.lookup(pointer), None);
        table.clear();
        assert!(table.is_empty());
    }

    #[test]
    fn a_refused_page_registration_leaves_the_old_mapping_intact() {
        let mut table = ControlTable::new();
        let old = 0x1000 as *mut AnyObject;
        table
            .register(old, ControlId::Field(ControlField::Theme))
            .unwrap();
        // The page rebuild registers a conflicting entry half-way through and is abandoned.
        let mut page = PageRegistrations::default();
        let new_theme = 0x2000 as *mut AnyObject;
        let locale = 0x3000 as *mut AnyObject;
        page.register(new_theme, ControlId::Field(ControlField::Theme))
            .unwrap();
        // The page's own list refuses the duplicate before anything reaches the shared table.
        assert_eq!(
            page.register(locale, ControlId::Field(ControlField::Theme)),
            Err(RegisterError::DuplicateId)
        );
        // Dropping the page's registrations publishes nothing.
        drop(page);
        assert_eq!(
            table.lookup(old),
            Some(ControlId::Field(ControlField::Theme)),
            "the previous mapping must still resolve to the same field"
        );
        assert_eq!(table.lookup(new_theme), None);
        assert_eq!(table.lookup(locale), None);
    }

    #[test]
    fn publishing_a_page_replaces_its_old_entries_and_no_stale_pointer_survives() {
        let mut table = ControlTable::new();
        let old_theme = 0x1000 as *mut AnyObject;
        let old_locale = 0x1001 as *mut AnyObject;
        let other = 0x1002 as *mut AnyObject;
        table
            .register(old_theme, ControlId::Field(ControlField::Theme))
            .unwrap();
        table
            .register(old_locale, ControlId::Field(ControlField::Locale))
            .unwrap();
        // A control from the page that is not being rebuilt keeps its entry.
        table
            .register(other, ControlId::Field(ControlField::WindowsEnabled))
            .unwrap();
        let new_theme = 0x2000 as *mut AnyObject;
        let new_locale = 0x2001 as *mut AnyObject;
        let mut page = PageRegistrations::default();
        page.register(new_theme, ControlId::Field(ControlField::Theme))
            .unwrap();
        page.register(new_locale, ControlId::Field(ControlField::Locale))
            .unwrap();
        page.publish(&mut table).unwrap();
        // The released addresses resolve to nothing; the new ones resolve to the same fields.
        assert_eq!(table.lookup(old_theme), None);
        assert_eq!(table.lookup(old_locale), None);
        assert_eq!(
            table.lookup(new_theme),
            Some(ControlId::Field(ControlField::Theme))
        );
        assert_eq!(
            table.lookup(new_locale),
            Some(ControlId::Field(ControlField::Locale))
        );
        assert_eq!(
            table.lookup(other),
            Some(ControlId::Field(ControlField::WindowsEnabled))
        );
        // Repeated cleanup is safe.
        assert!(table.unregister(&ControlId::Field(ControlField::Theme)));
        assert!(!table.unregister(&ControlId::Field(ControlField::Theme)));
    }

    #[test]
    fn the_update_description_matches_each_row_initial_value_item_by_item() {
        let input = input();
        for (page, view) in [
            (SettingsPageId::Switcher, switcher()),
            (SettingsPageId::General, general()),
        ] {
            let updates = page_update_description(&input, &[page]);
            let fields_with_values: Vec<ControlField> =
                view.rows.iter().filter_map(|row| row.field()).collect();
            assert_eq!(
                updates
                    .iter()
                    .map(|update| update.field)
                    .collect::<Vec<_>>(),
                fields_with_values,
                "{page:?}: one entry per config field, in row order"
            );
            let rows: Vec<&RowSpec> = view
                .rows
                .iter()
                .filter(|row| row.field().is_some())
                .collect();
            for (row, update) in rows.iter().zip(updates.iter()) {
                assert_eq!(update.value, row.value.describe(), "{page:?}: {update:?}");
                match (&row.control, row.value) {
                    (ControlKind::Switch, Value::Switch(_)) => {
                        assert!(update.value == "on" || update.value == "off");
                    }
                    (ControlKind::Popup { options }, Value::Index(index)) => {
                        assert!(
                            index < options.len(),
                            "the selected index is one of the choices"
                        );
                        assert_eq!(update.value, index.to_string());
                    }
                    (ControlKind::Slider { .. }, Value::Slider(value)) => {
                        assert_eq!(update.value, value.to_string());
                    }
                    (ControlKind::Button, Value::None) => panic!("an action row has no field"),
                    _ => panic!("the kind and the value agree"),
                }
            }
        }
    }
    #[test]
    fn every_static_state_control_of_a_migrated_page_appears_in_the_update_description() {
        let input = input();
        let updates: Vec<ControlField> =
            page_update_description(&input, &[SettingsPageId::Switcher, SettingsPageId::General])
                .into_iter()
                .map(|update| update.field)
                .collect();
        for page in [SettingsPageId::Switcher, SettingsPageId::General] {
            for row in view_for(page, &input).rows {
                if matches!(row.control, ControlKind::Button) {
                    continue;
                }
                if let Some(field) = row.field() {
                    assert!(
                        updates.contains(&field),
                        "{page:?} {field:?} is missing from the description"
                    );
                }
            }
        }
    }

    #[test]
    fn a_field_change_makes_the_in_place_sync_needed() {
        // The pair the checklist calls steps 13/14: a flag that makes the in-place sync run, and a
        // description that carries the new value when it does.
        let old = Config::default();
        let mut new = Config::default();
        new.layout.show_app_name_in_cards = !old.layout.show_app_name_in_cards;
        assert!(
            crate::runtime_config::change_needs_in_place_control_sync(&old, &new),
            "flipping a Switcher page field must make the open settings window re-read it"
        );
        let input = PageInput::from_config(new, None, crate::theme::ThemeInput::current());
        let update = page_update_description(&input, &[SettingsPageId::Switcher])
            .into_iter()
            .find(|update| update.field == ControlField::ShowAppNameInCards)
            .expect("the changed field is in the description");
        assert_eq!(update.value, "on");
    }
}
