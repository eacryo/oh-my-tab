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
    cfg: &crate::config::Config,
    device: Option<crate::mouse::device::DeviceKey>,
) -> bool {
    learning_would_shadow_mapping(layer, cfg, device, layer.gesture_button.button())
}

/// Whether LEARNING `button` as the layer's trigger would shadow an existing button mapping.
///
/// Same rule as the standing warning, asked about a button that is not configured yet: the capture
/// refuses the press instead of accepting a hotkey whose own mapping would silently stop running.
/// Both conditions come from the runtime, not from a copy of its rules:
///
/// * the mapping is the one `resolve_from_config` would actually run (profiles merged per key, the
///   switch merged on its own -- a profile that disables mappings does NOT delete a mapping another
///   profile contributes);
/// * the layer takes the button only if the gesture TABLE built for it claims that button, so an
///   enabled row whose direction does not parse, whose binding does not resolve or whose button is
///   illegal -- none of which ever fires -- does not turn a working mapping into a refusal.
pub(crate) fn learning_would_shadow_mapping(
    layer: &crate::config::GestureLayer,
    cfg: &crate::config::Config,
    device: Option<crate::mouse::device::DeviceKey>,
    button: u32,
) -> bool {
    hotkey_conflicts_with_mapping(
        layer_takes_its_button(&cfg.mouse, layer),
        mapping_for_button(button, cfg, device).as_deref(),
    )
}

/// The conflict rule itself, from the two facts the runtime can answer anywhere.
///
/// The recording tap cannot read the configuration -- it runs before the main thread and works from
/// the published gesture table and the cached mouse resolution -- but it has to decide whether to
/// swallow a refused learning press. Both callers therefore compose this same rule from their own
/// facts rather than restating it.
pub(crate) fn hotkey_conflicts_with_mapping(takes_button: bool, mapping: Option<&str>) -> bool {
    takes_button && mapping.is_some()
}

/// The mapping that would RUN on `button` in this resolution, if any.
pub(crate) fn mapping_runs_for(
    resolved: &crate::mouse::resolve::ResolvedMouse,
    button: u32,
) -> Option<String> {
    if !resolved.button_mappings_enabled {
        return None;
    }
    resolved.button_mappings.get(&button.to_string()).cloned()
}

/// The mapping the runtime would run on `button` for this device, if any.
///
/// `resolve_from_config` is the runtime's own merge (later profiles win per key; the enable switch
/// is merged separately), so this cannot drift from what the tap executes.
fn mapping_for_button(
    button: u32,
    cfg: &crate::config::Config,
    device: Option<crate::mouse::device::DeviceKey>,
) -> Option<String> {
    let resolved = crate::mouse::resolve::resolve_from_config(cfg, device);
    mapping_runs_for(&resolved, button)
}

/// Whether the layer's gesture rows really take its trigger button.
///
/// The gesture TABLE is the authority: it and the tap are built from the same rules, so a row that
/// contributes nothing to it cannot make the mapping stop running.
fn layer_takes_its_button(
    mouse: &crate::config::MouseSection,
    layer: &crate::config::GestureLayer,
) -> bool {
    let (table, _errors) = crate::mouse::gesture::table::build_layer(mouse, layer);
    layer_is_active(&table)
}

/// Whether this table would run anything: the one fact "gestures take their button" reduces to.
///
/// Every row of a table sits on that layer's trigger, so a non-empty table is the layer taking its
/// button. Learning a NEW button is the case where the candidate is deliberately NOT claimed yet, so
/// a check written as `claims(candidate)` answers "no gestures at all" and lets the mapping run --
/// which is exactly the bug a live test caught. This is the fact both the page and the recording tap
/// ask for, and it is assertable on a table built from a configuration.
pub(crate) fn layer_is_active(table: &crate::mouse::gesture::table::GestureTable) -> bool {
    table.len() > 0
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

    fn profile(mappings: &[(&str, &str)], enabled: Option<bool>) -> crate::config::MouseProfile {
        let mut profile = crate::config::MouseProfile::default();
        for (button, binding) in mappings {
            profile
                .button_mappings
                .insert((*button).to_string(), (*binding).to_string());
        }
        profile.button_mappings_enabled = enabled;
        profile
    }

    fn device_profile(
        device: (u32, u32),
        mappings: &[(&str, &str)],
        enabled: Option<bool>,
    ) -> crate::config::MouseProfile {
        let mut profile = profile(mappings, enabled);
        profile.device.vendor_id = Some(device.0);
        profile.device.product_id = Some(device.1);
        profile
    }

    /// The conflict rule's two facts, and the only combination that is a conflict.
    #[test]
    fn the_conflict_rule_needs_both_a_taken_button_and_a_running_mapping() {
        use super::hotkey_conflicts_with_mapping;
        assert!(!hotkey_conflicts_with_mapping(false, None));
        assert!(!hotkey_conflicts_with_mapping(false, Some("switcher")));
        assert!(!hotkey_conflicts_with_mapping(true, None));
        assert!(hotkey_conflicts_with_mapping(true, Some("switcher")));
    }

    /// A button that already carries a mapping cannot be LEARNED as the hotkey: the capture refuses
    /// it (the clipboard's rule), and the check is the standing warning's rule asked about that
    /// button, so the refusal and the bubble cannot disagree.
    #[test]
    fn learning_a_button_that_carries_a_mapping_is_refused() {
        let mut cfg = config(vec![row("rd", 1, "window.snap_left", true)]);
        cfg.mouse.profiles = vec![profile(&[("3", "switcher")], None)];
        let layer = cfg.mouse.shared_gesture_layer();
        assert!(
            learning_would_shadow_mapping(&layer, &cfg, None, 3),
            "button 3 carries a mapping"
        );
        assert!(
            !learning_would_shadow_mapping(&layer, &cfg, None, 1),
            "button 1 carries none"
        );
        // A profile that switches mappings off stops them running, so nothing is refused.
        cfg.mouse.profiles[0].button_mappings_enabled = Some(false);
        assert!(!learning_would_shadow_mapping(&layer, &cfg, None, 3));
    }

    /// The runtime merges the mapping TABLE per key and the enable SWITCH on its own: a profile that
    /// disables mappings does not delete one another profile contributes, and a profile that adds a
    /// mapping does not switch the feature on. Both orders are a real configuration a user can build
    /// by selecting a device on the Mouse page, and a check that folds them together got each wrong.
    #[test]
    fn the_mapping_check_merges_the_table_and_the_switch_like_the_runtime() {
        let device = (0x1234, 0x5678);
        // Shared: maps button 3, mappings off. Device: mappings on, map untouched -> button 3 runs.
        let mut cfg = config(vec![row("rd", 1, "window.snap_left", true)]);
        cfg.mouse.profiles = vec![
            profile(&[("3", "switcher")], Some(false)),
            device_profile(device, &[], Some(true)),
        ];
        let layer = cfg.mouse.shared_gesture_layer();
        assert!(
            learning_would_shadow_mapping(&layer, &cfg, Some(device), 3),
            "the device profile switches mappings back on, and the shared map survives"
        );
        // Shared: mappings off. Device: adds a mapping and leaves the switch alone -> off wins.
        cfg.mouse.profiles = vec![
            profile(&[], Some(false)),
            device_profile(device, &[("3", "switcher")], None),
        ];
        assert!(
            !learning_would_shadow_mapping(&layer, &cfg, Some(device), 3),
            "mappings stay switched off, so nothing runs to be shadowed"
        );
    }

    /// The fact the tap needs: a layer whose trigger is one button is still ACTIVE when the
    /// candidate being learned is a different one. Asking "does the table claim the candidate"
    /// answers no for every learnable button and lets the conflicting mapping run -- the bug this
    /// pins, which no test of the boolean composition would have caught.
    #[test]
    fn an_active_layer_is_not_a_claim_on_the_candidate_button() {
        let mut cfg = config(vec![row("rd", 1, "window.snap_left", true)]);
        cfg.mouse.profiles = vec![profile(&[("2", "switcher")], None)];
        cfg.mouse.gesture_button = crate::config::GestureTrigger::Learned { learned_button: 3 };
        let layer = cfg.mouse.shared_gesture_layer();
        let (table, _errors) = crate::mouse::gesture::table::build_layer(&cfg.mouse, &layer);
        assert!(
            !table.claims(2),
            "the candidate is not the layer's trigger, so nothing claims it yet"
        );
        assert!(
            layer_is_active(&table),
            "but the layer will run its rows, so a mapping on the candidate is shadowed"
        );
        assert!(hotkey_conflicts_with_mapping(
            layer_is_active(&table),
            Some("switcher")
        ));
        assert!(!hotkey_conflicts_with_mapping(
            layer_is_active(&table),
            None
        ));
    }

    /// Only a row the runtime would actually run counts: an enabled row with an unusable direction
    /// or binding never takes the button, so the mapping keeps working and the capture must accept.
    #[test]
    fn a_row_that_never_fires_does_not_refuse_the_hotkey() {
        let mut cfg = config(vec![row("rd", 1, "not-a-target", true)]);
        cfg.mouse.profiles = vec![profile(&[("3", "switcher")], None)];
        // The layer's own trigger is button 3, so the warning below asks the same question the
        // refusal does.
        cfg.mouse.gesture_button = crate::config::GestureTrigger::Learned { learned_button: 3 };
        let layer = cfg.mouse.shared_gesture_layer();
        assert!(
            !learning_would_shadow_mapping(&layer, &cfg, None, 3),
            "an unresolvable binding never fires"
        );
        assert!(
            !trigger_shadows_mapping(&layer, &cfg, None),
            "and the standing warning agrees with the refusal"
        );
        // A usable row does take the button, and then the mapping is refused.
        cfg.mouse.gestures = vec![row("rd", 1, "window.snap_left", true)];
        let layer = cfg.mouse.shared_gesture_layer();
        assert!(learning_would_shadow_mapping(&layer, &cfg, None, 3));
        assert!(trigger_shadows_mapping(&layer, &cfg, None));
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
