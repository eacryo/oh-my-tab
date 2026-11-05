//! The row constructors a page description uses to state one row.
//!
//! A page does not build a `RowSpec` by hand: `RowWriter` assigns the card and section a row
//! belongs to, and a `RowDraft` carries the parts a single row states (its copy keys, its control,
//! its identity, its value). These are the pure constructors; nothing here touches AppKit.

use super::super::*;
use super::*;

/// Build rows card by card: `card` opens a card (its heading copy key), and the rows that follow
/// belong to it. The first row of every card starts the card's block; each later row in the card
/// draws a divider above itself.
pub(super) struct RowWriter {
    page: SettingsPageId,
    initial_section: String,
    card: usize,
    section: &'static str,
    rows: Vec<RowSpec>,
    extra: Vec<PageRowSpec>,
}

impl RowWriter {
    pub(super) fn new(page: SettingsPageId, initial_section: String) -> Self {
        Self {
            page,
            initial_section,
            card: 0,
            section: "",
            rows: Vec::new(),
            extra: Vec::new(),
        }
    }

    /// Open a card: its rows start after the section step.
    pub(super) fn card(&mut self, section: &'static str) {
        self.card += 1;
        self.section = section;
    }

    pub(super) fn in_first_card(&mut self, section: &'static str) {
        self.section = section;
    }

    pub(super) fn push(&mut self, mut row: RowSpec) {
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

    /// Declare a row the PAGE draws itself, after the card's described rows so far.
    ///
    /// The row stays inside the current card, so the described rows that follow still land in the
    /// same card and the card closes where it would have closed anyway.
    pub(super) fn extra_row(&mut self, key: &'static str, height: f64) {
        self.extra_row_in_group(key, height, None);
    }

    /// Declare a page-drawn row that belongs to a conditional group: the canvas hides it with the
    /// same `set_group_visible` the described rows of that group use.
    pub(super) fn extra_row_in_group(
        &mut self,
        key: &'static str,
        height: f64,
        group: Option<RowGroup>,
    ) {
        let after = self.rows.iter().filter(|row| row.card == self.card).count();
        self.extra.push(PageRowSpec {
            card: self.card,
            section: self.section,
            key,
            height: RowHeight::Fixed(height),
            after_row: after,
            group,
        });
    }

    /// Declare a row the page draws itself whose height depends on the page input (the Mouse page's
    /// device row: its height follows the longest caption any connected device can produce).
    pub(super) fn extra_row_measured(
        &mut self,
        key: &'static str,
        measure: fn(&PageInput, f64) -> f64,
    ) {
        let after = self.rows.iter().filter(|row| row.card == self.card).count();
        self.extra.push(PageRowSpec {
            card: self.card,
            section: self.section,
            key,
            height: RowHeight::Measured(measure),
            after_row: after,
            group: None,
        });
    }

    pub(super) fn finish(self) -> PageView {
        PageView {
            page: self.page,
            title_key: settings_page_identity(settings_page_index(self.page)).title_key(),
            rows: self.rows,
            initial_section: self.initial_section,
            banner: None,
            header_offset: None,
            extra_rows: self.extra,
        }
    }
}

/// One row's fields, before the page's card bookkeeping fills them in.
pub(super) struct RowDraft {
    label_key: &'static str,
    desc_key: Option<&'static str>,
    caption_key: Option<&'static str>,
    control: ControlKind,
    id: ControlId,
    copy: RowCopy,
    value_column: bool,
    wide_caption: bool,
    depends_on: Option<ControlField>,
    group: Option<RowGroup>,
    value: Value,
}

impl RowDraft {
    pub(super) fn new(
        label_key: &'static str,
        control: ControlKind,
        id: ControlId,
        value: Value,
    ) -> Self {
        Self {
            label_key,
            desc_key: None,
            caption_key: None,
            control,
            id,
            copy: RowCopy::Plain,
            value_column: false,
            wide_caption: false,
            depends_on: None,
            group: None,
            value,
        }
    }

    /// Draw this row's LINK or VALUE control in the page's own value column.
    pub(super) fn value_column(mut self) -> Self {
        self.value_column = true;
        self
    }

    /// Give this captioned row the WIDE text block (up to the card's far inset). The design uses
    /// it on the one row whose copy must fit two lines beside a right-aligned switch.
    pub(super) fn wide_caption(mut self) -> Self {
        self.wide_caption = true;
        self
    }

    pub(super) fn desc(mut self, key: &'static str) -> Self {
        self.desc_key = Some(key);
        self
    }

    pub(super) fn caption(mut self, key: &'static str) -> Self {
        self.copy = RowCopy::Captioned;
        self.caption_key = Some(key);
        self
    }

    #[allow(dead_code)]
    pub(super) fn depends_on(mut self, field: ControlField) -> Self {
        self.depends_on = Some(field);
        self
    }

    /// A `Value::Dynamic` control: its content is data the caller computed, not a locale key.
    pub(super) fn dynamic(mut self, text: impl Into<String>) -> Self {
        self.value = Value::Dynamic(text.into());
        self
    }

    pub(super) fn group(mut self, group: RowGroup) -> Self {
        self.group = Some(group);
        self
    }

    pub(super) fn into_spec(self) -> RowSpec {
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
            value_column: self.value_column,
            wide_caption: self.wide_caption,
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

pub(super) fn switch(label_key: &'static str, field: ControlField, on: bool) -> RowDraft {
    RowDraft::new(
        label_key,
        ControlKind::Switch,
        ControlId::Field(field),
        Value::Switch(on),
    )
}

pub(super) fn popup(
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
pub(super) fn slider(
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

pub(super) fn button(
    label_key: &'static str,
    action: &'static str,
    title_key: &'static str,
) -> RowDraft {
    RowDraft::new(
        label_key,
        ControlKind::Button { title_key },
        ControlId::Action(action),
        Value::None,
    )
}
