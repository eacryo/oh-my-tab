//! Global keyboard bindings.
//!
//! The feature replaces two independent interceptions (`window_control` and `quick_actions`), each
//! with its own hardcoded key set and its own event tap, with one data-driven engine:
//!
//! * [`KeyBinding`] is the config entry (`[[key_bindings.items]]`), kept as raw strings so the
//!   config layer can fall back per entry exactly like every other field.
//! * [`action::Action`] is the registry of behaviours; [`trigger::Trigger`] is a chord or a
//!   double-tapped modifier.
//! * [`table::BindingTable`] is the immutable snapshot the tap thread resolves presses against.
//!
//! Configuration is the only place that decides whether a binding is live (group switch and row
//! switch); the engine never reads a config switch again, so there is one source of truth.

mod action;
mod engine;
mod execute;
mod table;
mod trigger;
mod validate;

pub(crate) use action::{Action, Group, ALL};
pub(crate) use engine::{
    debug_self_frontmost, enabled_in, live_binding_count, publish, publish_self_frontmost,
    reset_input_state, start, stop,
};
pub(crate) use execute::{action_label, execute, execute_authorized, reserved_trigger_message};
pub(crate) use table::BindingTable;
pub(crate) use trigger::{modifier_flag, Trigger};
pub(crate) use validate::{
    clipboard_shortcut_status, drop_reserved_conflicts, trigger_is_taken, validate_items,
    ClipboardStatus, Reserved,
};

use serde::{Deserialize, Serialize};

/// One `[[key_bindings.items]]` entry.
///
/// The action and trigger stay strings at this layer (the shape the file holds); they are resolved
/// against the registry when the table is built. An entry whose fields came out empty — a
/// hand-edited file with a wrong TOML type — is reported by validation and dropped, never silently
/// turned into some other action.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub(crate) struct KeyBinding {
    /// Stable action id; see `Action::config_id`.
    pub action: String,
    /// Chord description ("option+e") or gesture name ("double-control").
    pub trigger: String,
    #[serde(default = "default_binding_enabled")]
    pub enabled: bool,
}

/// A binding is on when it is listed: "added a binding" and "enabled it" are the same act, so the
/// field only ever turns one off temporarily.
fn default_binding_enabled() -> bool {
    true
}

impl Default for KeyBinding {
    fn default() -> Self {
        Self {
            action: String::new(),
            trigger: String::new(),
            enabled: default_binding_enabled(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_defaults_to_enabled_with_empty_fields() {
        let entry = KeyBinding::default();
        assert_eq!(entry.action, "");
        assert_eq!(entry.trigger, "");
        assert!(entry.enabled);
    }
}
