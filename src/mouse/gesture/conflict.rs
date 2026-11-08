//! Conflicts between gesture rows and the rest of the configuration.
//!
//! The rule the design settled on: a gesture's *output* is not an input. Binding a gesture to
//! \`cmd+shift+t\` does not compete with a keyboard binding on the same chord -- one produces a
//! keystroke, the other intercepts one -- so that is a warning, not an error. What IS an error is two
//! gestures claiming the same button and sequence, because only one of them can ever fire.
//!
//! Nothing here decides whether to save; it reports, and the settings layer decides how loudly.

use std::collections::HashMap;

use crate::mouse::gesture::table::{self, Target};

/// How serious a conflict is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Severity {
    /// The row can never fire, or silently shadows another one.
    Blocking,
}

/// One reported conflict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Conflict {
    /// Index of the gesture row it concerns.
    pub(crate) row: usize,
    pub(crate) severity: Severity,
    /// An i18n key describing the problem.
    pub(crate) message: &'static str,
}

/// Conflicts inside one gesture layer: rows that claim the same trigger and stroke.
///
/// Two mice may bind one stroke to different actions, so a conflict is never found across layers.
pub(crate) fn find_layer_conflicts(layer: &crate::config::GestureLayer) -> Vec<Conflict> {
    let mut conflicts = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for (index, row) in layer.gestures.iter().enumerate() {
        let Some(key) = table::row_sequence(&row.gesture) else {
            continue;
        };
        let claims = (1..=4).contains(&row.button)
            && row.enabled
            && !row.binding.trim().is_empty()
            && Target::resolve(&row.binding).is_some();
        if !claims {
            continue;
        }
        if seen.insert(key, index).is_some() {
            conflicts.push(Conflict {
                row: index,
                severity: Severity::Blocking,
                message: "errors.errors_gesture_duplicate",
            });
        }
    }
    conflicts
}

/// The rows whose shortcut is ALSO a key binding: a warning, not a conflict.
///
/// It says the same chord can be produced by a stroke and pressed on the keyboard -- which may be
/// exactly what the user wants -- so it belongs on the row, not among the errors. Layer scope only
/// affects duplicates: the binding list is global.
pub(crate) fn rows_shadowed_by_bindings(
    layer: &crate::config::GestureLayer,
    cfg: &crate::config::Config,
) -> Vec<usize> {
    let mut rows = Vec::new();
    for (index, row) in layer.gestures.iter().enumerate() {
        let Some(crate::mouse::gesture::table::Target::Binding(
            crate::mouse::shortcut::Binding::Key(shortcut),
        )) = Target::resolve(&row.binding)
        else {
            continue;
        };
        let overlaps = cfg.key_bindings.items.iter().flatten().any(|item| {
            crate::key_bindings::Trigger::parse(&item.trigger)
                .is_ok_and(|trigger| trigger.chord() == Some(shortcut))
        });
        if overlaps {
            rows.push(index);
        }
    }
    rows
}

/// Whether this layer's trigger button also carries a button mapping on ANY profile.
///
/// The gesture path runs first (the tap checks gestures before mappings), so when both are
/// configured for one button the mapping never fires. The page states that once, beside the
/// trigger, rather than leaving the user to discover a dead mapping.
/// The check is for the device being edited, resolved through the SAME profile rule the mouse
/// settings use: only that device's effective mapping (base layer plus its own, later ones winning)
/// can be shadowed, and a mapping switched off there does not warn. A gesture list that claims
/// nothing (the switch off, or no usable rows) takes no press, so the mapping runs as usual and
/// there is nothing to warn about either.
pub(crate) fn trigger_shadows_mapping(
    layer: &crate::config::GestureLayer,
    mouse: &crate::config::MouseSection,
    device: Option<crate::mouse::device::DeviceKey>,
) -> bool {
    if !mouse.gestures_enabled || !layer.gestures.iter().any(|row| row.enabled) {
        return false;
    }
    let button = layer.gesture_button.button().to_string();
    // Resolve the way the mouse page does: the wildcard profile first, then every profile that
    // matches this device, later ones winning.
    let mut mapping = None;
    for profile in &mouse.profiles {
        if !crate::mouse::resolve::matches(profile, device) {
            continue;
        }
        if let Some(entry) = profile.button_mappings.get(&button) {
            mapping = Some(entry.clone());
        }
        if profile.button_mappings_enabled.is_some() {
            // Disabled on this device drops the inherited mapping.
            if profile.button_mappings_enabled == Some(false) {
                mapping = None;
            }
        }
    }
    mapping.is_some()
}

/// The conflicts the page shows for the layer it is editing, keyed by row.
pub(crate) fn by_row_in_layer(layer: &crate::config::GestureLayer) -> HashMap<usize, Conflict> {
    find_layer_conflicts(layer)
        .into_iter()
        .map(|conflict| (conflict.row, conflict))
        .collect()
}

/// Every gesture layer's conflicts: the shared layer and each device's own.
///
/// A conflict is found INSIDE one layer: two mice may bind the same stroke to different actions,
/// which is the reason layers exist.
#[cfg(test)]
pub(crate) fn find_conflicts(cfg: &crate::config::Config) -> Vec<Conflict> {
    let mut conflicts = find_layer_conflicts(&cfg.mouse.shared_gesture_layer());
    for layer in &cfg.mouse.gesture_devices {
        conflicts.extend(find_layer_conflicts(layer));
    }
    conflicts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, GestureBinding};

    fn row(gesture: &str, button: u32, binding: &str, enabled: bool) -> GestureBinding {
        GestureBinding {
            gesture: gesture.into(),
            button,
            binding: binding.into(),
            enabled,
        }
    }

    fn config(rows: Vec<GestureBinding>) -> Config {
        let mut cfg = Config::default();
        cfg.mouse.enabled = true;
        cfg.mouse.gestures_enabled = true;
        cfg.mouse.gestures = rows;
        cfg
    }

    #[test]
    fn two_rows_on_the_same_button_and_sequence_block() {
        let conflicts = find_conflicts(&config(vec![
            row("rd", 1, "cmd+t", true),
            row("RD", 1, "cmd+s", true),
        ]));
        assert!(
            conflicts
                .iter()
                .any(|c| c.severity == Severity::Blocking && c.row == 1),
            "{conflicts:?}"
        );
    }

    #[test]
    fn rows_written_for_different_buttons_conflict_on_the_trigger() {
        let global = config(vec![
            row("rd", 1, "cmd+t", true),
            row("rd", 2, "cmd+s", true),
        ]);
        assert_eq!(
            global.mouse.gesture_button.button(),
            1,
            "the fixture's trigger"
        );
        let conflicts = find_conflicts(&global);
        assert!(
            conflicts
                .iter()
                .any(|c| c.severity == Severity::Blocking && c.row == 1),
            "the second row is shadowed by the first: {conflicts:?}"
        );
    }

    /// An unknown target is a ROW problem, reported by the page on the row and refused by the
    /// table; it is not a conflict between two rows, so this list does not carry it.
    #[test]
    fn an_unknown_target_is_not_a_conflict() {
        let conflicts = find_conflicts(&config(vec![row("rd", 1, "not-a-target", true)]));
        assert!(conflicts.is_empty(), "{conflicts:?}");
    }

    #[test]
    fn a_disabled_default_row_does_not_block_a_real_one() {
        // The shipped defaults are disabled and unbound; a user's enabled R on the same button is
        // not a duplicate of them.
        let conflicts = find_conflicts(&config(vec![
            row("R", 1, "", false),
            row("R", 1, "cmd+t", true),
        ]));
        assert!(conflicts.is_empty(), "{conflicts:?}");
    }

    #[test]
    fn an_unparseable_sequence_is_left_to_config_validation() {
        // The recognizer refuses it; reporting it twice would make the same mistake look like two.
        let conflicts = find_conflicts(&config(vec![row("XY", 1, "cmd+t", true)]));
        assert!(conflicts.is_empty(), "{conflicts:?}");
    }
}
