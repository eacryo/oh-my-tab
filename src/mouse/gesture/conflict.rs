//! Conflicts between gesture rows and the rest of the configuration.
//!
//! The rule the design settled on: a gesture's *output* is not an input. Binding a gesture to
//! \`cmd+shift+t\` does not compete with a keyboard binding on the same chord -- one produces a
//! keystroke, the other intercepts one -- so that is a warning, not an error. What IS an error is two
//! gestures claiming the same button and sequence, because only one of them can ever fire.
//!
//! Nothing here decides whether to save; it reports, and the settings layer decides how loudly.

use std::collections::HashMap;

use crate::config::{Config, GestureBinding};
use crate::mouse::gesture::table::{self, Target};

/// How serious a conflict is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Severity {
    /// The row can never fire, or silently shadows another one.
    Blocking,
    /// The row works, but may not do what the user expects.
    Warning,
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

/// Check every gesture row against the others and against the rest of the configuration.
pub(crate) fn find_conflicts(cfg: &Config) -> Vec<Conflict> {
    let mut conflicts = Vec::new();
    let mut seen: HashMap<(u32, String), usize> = HashMap::new();
    for (index, row) in cfg.mouse.gestures.iter().enumerate() {
        // The SAME sequence key the runtime builds on, aliases included: two rows that spell one
        // stroke differently would otherwise read as distinct here and collide in the table.
        let Some(key) = table::row_sequence(&row.gesture) else {
            // An unparseable sequence is already reported by validation; nothing to cross-check.
            continue;
        };
        // The button is the configuration's one trigger, as in the table.
        let button = table::effective_trigger_button(&cfg.mouse);
        if !(1..=4).contains(&button) {
            continue;
        }
        // Only a row the runtime would actually load can occupy the trigger+sequence. A disabled,
        // unbound, impossible-button or unknown-target row is refused by the table, so it must not
        // make a later, healthy row look like a duplicate here.
        let claims = (1..=4).contains(&row.button)
            && row.enabled
            && !row.binding.trim().is_empty()
            && Target::resolve(&row.binding).is_some();
        if !claims {
            continue;
        }
        match seen.get(&(button, key.clone())) {
            Some(_first) => conflicts.push(Conflict {
                row: index,
                severity: Severity::Blocking,
                message: "errors.errors_gesture_duplicate",
            }),
            None => {
                seen.insert((button, key), index);
            }
        }
    }
    for (index, row) in cfg.mouse.gestures.iter().enumerate() {
        if row.enabled && !row.binding.trim().is_empty() && Target::resolve(&row.binding).is_none()
        {
            conflicts.push(Conflict {
                row: index,
                severity: Severity::Blocking,
                message: "errors.errors_gesture_unknown_target",
            });
        }
        // A gesture output that matches a keyboard binding's trigger is a warning: it may be exactly
        // what the user wants (run the same action from a stroke), and it does not steal anything.
        if let Some(shortcut) = resolvable_shortcut(row) {
            if cfg.key_bindings.items.iter().flatten().any(|item| {
                crate::key_bindings::Trigger::parse(&item.trigger)
                    .is_ok_and(|trigger| trigger.chord() == Some(shortcut))
            }) {
                conflicts.push(Conflict {
                    row: index,
                    severity: Severity::Warning,
                    message: "errors.errors_gesture_shadowed_by_binding",
                });
            }
        }
    }
    conflicts.sort_by_key(|c| (c.severity, c.row));
    conflicts
}

/// The conflicts grouped by the row they concern, keeping the most serious one per row: a row can
/// only state one problem, and the blocking one matters more than the warning.
pub(crate) fn by_row(cfg: &Config) -> HashMap<usize, Conflict> {
    let mut grouped: HashMap<usize, Conflict> = HashMap::new();
    for conflict in find_conflicts(cfg) {
        grouped
            .entry(conflict.row)
            .and_modify(|kept| {
                if conflict.severity < kept.severity {
                    *kept = conflict.clone();
                }
            })
            .or_insert(conflict);
    }
    grouped
}

/// The keyboard chord a row would synthesise, when it runs a key shortcut.
fn resolvable_shortcut(row: &GestureBinding) -> Option<crate::mouse::shortcut::Shortcut> {
    match Target::resolve(&row.binding)? {
        Target::Binding(crate::mouse::shortcut::Binding::Key(shortcut)) => Some(shortcut),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GestureBinding;

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

    /// Two rows written for different buttons both start on the one trigger, so the second can never
    /// fire and the page has to say so.
    #[test]
    fn rows_written_for_different_buttons_conflict_on_the_trigger() {
        let global = config(vec![
            row("rd", 1, "cmd+t", true),
            row("rd", 2, "cmd+s", true),
        ]);
        assert_eq!(global.mouse.gesture_button, 1, "the fixture's trigger");
        let conflicts = find_conflicts(&global);
        assert!(
            conflicts
                .iter()
                .any(|c| c.severity == Severity::Blocking && c.row == 1),
            "the second row is shadowed by the first: {conflicts:?}"
        );
    }

    #[test]
    fn an_unknown_target_blocks() {
        let conflicts = find_conflicts(&config(vec![row("rd", 1, "not-a-target", true)]));
        assert!(conflicts.iter().any(|c| c.severity == Severity::Blocking));
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
