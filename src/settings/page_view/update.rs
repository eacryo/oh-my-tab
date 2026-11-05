//! The one field-to-value description both the refill and the in-place refresh consume.
//!
//! A `ControlUpdate` carries the value the control is SET to (typed, as the description stated it)
//! and the text its display shows when those differ; nothing else formats a value on the way in.

use super::super::*;
use super::*;

/// The pure "field -> value" description both the refill and the in-place refresh write.
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
            typed: value.clone(),
            value: value.describe(),
            display,
        });
    }
    updates
}

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
            .filter_map(|row| row.field().map(|field| (field, row.value.clone())))
            .collect();
        updates.extend(update_description(&view, &values));
    }
    updates
}
