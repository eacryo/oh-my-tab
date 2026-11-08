//! The published gesture binding table and the epoch that revokes work in flight.
//!
//! Same shape as `key_bindings::table`: an immutable snapshot the tap thread resolves against, and a
//! publisher that replaces it atomically on the main thread. Nothing here takes the config lock.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::config::{Config, GestureBinding};
use crate::key_bindings::Action;
use crate::mouse::shortcut::{self, Binding};

use super::recognizer::parse_key;

/// What a gesture runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    /// One of the app's own actions (`Action::config_id`, e.g. "window.snap_left").
    Action(Action),
    /// A shortcut, a system action, the switcher, or nothing -- the same grammar the mouse button
    /// mappings use.
    Binding(Binding),
}

impl Target {
    /// Resolve a config string. An action id is tried first so one field can hold either kind; the
    /// shortcut grammar has no spelling that could also be an action id, but trying the narrower
    /// meaning first keeps the two registries from disagreeing about a value.
    pub(crate) fn resolve(desc: &str) -> Option<Self> {
        let trimmed = desc.trim();
        if trimmed.is_empty() {
            return None;
        }
        if let Some(action) = Action::from_config_id(trimmed) {
            return Some(Self::Action(action));
        }
        shortcut::parse_binding(trimmed).ok().map(Self::Binding)
    }
}

/// One live row: the sequence on one button, and what it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Row {
    /// The normalised sequence, e.g. "RD".
    pub(crate) key: String,
    pub(crate) button: u32,
    pub(crate) target: Target,
    /// The row's position in the config list, so the settings page can report which row fired.
    pub(crate) index: usize,
}

/// The immutable snapshot the tap thread reads.
#[derive(Debug, Default)]
pub(crate) struct GestureTable {
    rows: HashMap<(u32, String), Row>,
}

impl GestureTable {
    pub(crate) fn lookup(&self, button: u32, key: &str) -> Option<&Row> {
        self.rows.get(&(button, key.to_string()))
    }

    /// Whether any row would claim `button`, which is the condition for taking the press at all.
    /// A disabled switch, an empty list and all-invalid rows all leave the button alone.
    pub(crate) fn claims(&self, button: u32) -> bool {
        self.rows.keys().any(|(candidate, _)| *candidate == button)
    }

    /// How many live rows the table holds, for the settings page's summary line.
    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }
}

/// What a row runs, in the user's words: the same words the settings page shows for it.
pub(crate) fn row_label(row: &Row) -> String {
    match &row.target {
        Target::Action(action) => crate::key_bindings::action_label(*action),
        Target::Binding(binding) => shortcut::display_binding(binding),
    }
}

/// Why a row was dropped. The config layer reports these per index; the table keeps none of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RowError {
    BadGesture,
    BadButton,
    EmptyBinding,
    UnresolvableBinding,
    Duplicate,
}

/// Build the table from the config. Returns the table and one entry per rejected row.
///
/// A rejected row never takes its siblings with it: this is the per-entry fallback the repository
/// requires of config values, and why the table is the place the decision is made rather than the
/// deserialiser.
pub(crate) fn build(mouse: &crate::config::MouseSection) -> (GestureTable, Vec<(usize, RowError)>) {
    build_layer(mouse, &mouse.shared_gesture_layer())
}

/// One device's table: the rows of ITS layer, drawn on ITS trigger.
///
/// A device with a layer of its own has a different list, a different trigger and therefore a
/// different set of claims; building one table for everything would let a press on the right button
/// run a gesture that belongs to another mouse.
pub(crate) fn build_layer(
    mouse: &crate::config::MouseSection,
    layer: &crate::config::GestureLayer,
) -> (GestureTable, Vec<(usize, RowError)>) {
    let mut table = GestureTable::default();
    let mut errors = Vec::new();
    if !mouse.enabled || !mouse.gestures_enabled {
        return (table, errors);
    }
    for (index, row) in layer.gestures.iter().enumerate() {
        match row_error(row) {
            Ok((key, target)) => {
                // Every row lands on the effective trigger, not on the button it was written with:
                // a global trigger makes the list one button's list, and two rows that were
                // distinct only by their buttons now claim the same slot.
                let button = layer.gesture_button.button();
                let slot = (button, key.clone());
                if table.rows.contains_key(&slot) {
                    errors.push((index, RowError::Duplicate));
                    continue;
                }
                table.rows.insert(
                    slot,
                    Row {
                        key,
                        button,
                        target,
                        index,
                    },
                );
            }
            Err(error) => errors.push((index, error)),
        }
    }
    (table, errors)
}

/// The gesture layer a device uses: its own layer when it has one, the shared layer otherwise.
///
/// The snapshot indexes the same choice by (VID, PID) for the tap; this is the rule itself, kept
/// where it can be asserted without a published snapshot.
#[cfg(test)]
pub(crate) fn layer_for<'a>(
    mouse: &'a crate::config::MouseSection,
    shared: &'a crate::config::GestureLayer,
    device: Option<crate::mouse::device::DeviceKey>,
) -> &'a crate::config::GestureLayer {
    let Some(device) = device else {
        return shared;
    };
    mouse
        .gesture_devices
        .iter()
        .find(|layer| {
            if layer.device.is_virtual() {
                return device == crate::mouse::device::VIRTUAL_DEVICE_KEY;
            }
            layer.device.vendor_id == Some(device.0) && layer.device.product_id == Some(device.1)
        })
        .unwrap_or(shared)
}

/// A configured sequence as the key the tap matches against.
///
/// Two spellings name the same stroke: the four direction letters, and the stroke characters
/// ("B" down, "F" up) some gesture lists use. One function decides that, and the table, conflict
/// detection and the settings page all read it, so a row cannot be valid in one place and unknown
/// in another.
pub(crate) fn row_sequence(text: &str) -> Option<String> {
    parse_key(text, 8).or_else(|| parse_key(&stroke_directions(text)?, 8))
}

/// A configuration gesture written in stroke characters ("DB", "BU", "UL") as direction letters.
///
/// `None` when the text holds anything else, including a mix: a stroke is one kind of line.
fn stroke_directions(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(trimmed.len());
    for c in trimmed.chars() {
        out.push(match c.to_ascii_uppercase() {
            'U' => 'U',
            'D' => 'D',
            'L' => 'L',
            'R' => 'R',
            'B' => 'D',
            'F' => 'U',
            _ => return None,
        });
    }
    Some(out)
}

fn row_error(row: &GestureBinding) -> Result<(String, Target), RowError> {
    // The row's own button is no longer a trigger, but an impossible one still means a hand-written
    // row whose author meant something else: it is refused here, and the page reports it with the
    // same rule (`row.problem`), so the two cannot disagree about which rows are live.
    if !(1..=4).contains(&row.button) {
        // The left button (0) is refused on purpose: a gesture on it would swallow ordinary clicks.
        return Err(RowError::BadButton);
    }
    // The rows this app ships are written in the recogniser's own stroke characters ("DB", "BU"),
    // which a mouse can produce and a user cannot type: the four direction letters cannot spell
    // them, so the table reads the stroke directly where `parse_key` would refuse it. A hand-written
    // row that neither reads loads exactly as before.
    let key = row_sequence(&row.gesture).ok_or(RowError::BadGesture)?;
    if row.binding.trim().is_empty() {
        // Covers both the shipped default (disabled, unbound) and an enabled row the user left
        // unbound: neither may claim the button.
        return Err(RowError::EmptyBinding);
    }
    let target = Target::resolve(&row.binding).ok_or(RowError::UnresolvableBinding)?;
    if !row.enabled {
        // Disabled rows still carry a binding in the settings UI, so parse it but refuse to load.
        return Err(RowError::EmptyBinding);
    }
    Ok((key, target))
}

/// The current table plus the epoch that produced it.
#[derive(Debug, Clone)]
pub(crate) struct Published {
    pub(crate) table: Arc<GestureTable>,
    /// One table per device layer, keyed by the device's (VID, PID). A device without an entry, and
    /// a press whose device could not be attributed, uses `table`.
    pub(crate) device_tables: Arc<DeviceTables>,
    pub(crate) epoch: u64,
    pub(crate) excluded: Arc<Vec<String>>,
}

/// Whether two snapshot sets hold the same rows on the same devices.
fn same_device_tables(
    left: &[((u32, u32), Arc<GestureTable>)],
    right: &[((u32, u32), Arc<GestureTable>)],
) -> bool {
    left.len() == right.len()
        && left.iter().all(|(device, table)| {
            right
                .iter()
                .find(|(candidate, _)| candidate == device)
                .is_some_and(|(_, other)| other.rows == table.rows)
        })
}

impl Published {
    /// The table a press from `device` is decided by: that device's layer, else the shared one.
    ///
    /// `None` is a press that could not be attributed, and it falls back to the shared layer --
    /// the same fallback an unattributed button mapping takes.
    pub(crate) fn table_for(
        &self,
        device: Option<crate::mouse::device::DeviceKey>,
    ) -> &GestureTable {
        device
            .and_then(|key| {
                self.device_tables
                    .iter()
                    .find(|(candidate, _)| *candidate == key)
                    .map(|(_, table)| table.as_ref())
            })
            .unwrap_or(&self.table)
    }
}

/// The per-device tables a snapshot carries: one per device, keyed by its (VID, PID).
type DeviceTables = Vec<((u32, u32), Arc<GestureTable>)>;

static EPOCH: AtomicU64 = AtomicU64::new(1);
static PUBLISHED: OnceLock<Mutex<Published>> = OnceLock::new();

fn cell() -> &'static Mutex<Published> {
    PUBLISHED.get_or_init(|| {
        Mutex::new(Published {
            table: Arc::new(GestureTable::default()),
            device_tables: Arc::new(Vec::new()),
            epoch: 0,
            excluded: Arc::new(Vec::new()),
        })
    })
}

/// The epoch a session starts under. A session whose epoch no longer matches the published one has
/// been revoked: the config changed at all, not merely lost its binding. Also what the settings page
/// and a test read to observe that a change took effect.
pub(crate) fn current_epoch() -> u64 {
    cell().lock().unwrap().epoch
}

/// Snapshot for the tap thread. Cloned `Arc`s, so the lock is held for a moment only.
pub(crate) fn snapshot() -> Published {
    cell().lock().unwrap().clone()
}

/// Replace the published table. The epoch advances only when the *derived* snapshot changes, which
/// includes the parent switch and the exclusion list -- a reload that only edits the exclusion list
/// is a revocation too.
pub(crate) fn publish(cfg: &Config) -> Vec<(usize, RowError)> {
    let (table, errors) = build(&cfg.mouse);
    // Each device layer is published beside the shared one, keyed by its device: the tap picks the
    // layer by the device that produced the press, and never reads the configuration itself.
    let mut device_tables = Vec::with_capacity(cfg.mouse.gesture_devices.len());
    for layer in cfg.mouse.gesture_devices.iter() {
        // The KEY first, from the layer's own identity, and the layer itself is what gets built: a
        // virtual-pointer layer has no VID/PID, so asking the (VID, PID) lookup for it would find the
        // shared layer and publish that mouse's gestures under the virtual device's name.
        let key = if layer.device.is_virtual() {
            Some(crate::mouse::device::VIRTUAL_DEVICE_KEY)
        } else {
            match (layer.device.vendor_id, layer.device.product_id) {
                (Some(vendor_id), Some(product_id)) => Some((vendor_id, product_id)),
                _ => None,
            }
        };
        let (built, _layer_errors) = build_layer(&cfg.mouse, layer);
        if let Some(key) = key {
            device_tables.push((key, Arc::new(built)));
        }
    }
    let excluded: Vec<String> = cfg
        .mouse
        .gesture_excluded_apps
        .iter()
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .collect();
    let next = Published {
        table: Arc::new(table),
        device_tables: Arc::new(device_tables),
        epoch: 0,
        excluded: Arc::new(excluded),
    };
    let mut guard = cell().lock().unwrap();
    let changed = guard.table.rows != next.table.rows
        || !same_device_tables(&guard.device_tables, &next.device_tables)
        || guard.excluded.as_slice() != next.excluded.as_slice()
        || !cfg.mouse.enabled
        || !cfg.mouse.gestures_enabled;
    if changed {
        // Monotonic: an A -> B -> A round trip must not restore the old epoch, or a stale session
        // would pass its authorization check again.
        let epoch = EPOCH.fetch_add(1, Ordering::SeqCst) + 1;
        guard.table = next.table;
        guard.device_tables = next.device_tables;
        guard.excluded = next.excluded;
        guard.epoch = epoch;
    }
    errors
}

/// Serialises every test that publishes to, or reads, the process-global published snapshot.
///
/// Cargo runs test functions in parallel threads; without this one module's `publish` lands between
/// another module's press and release, and a test that only asserts a stroke's state fails on a
/// table it never wrote.
#[cfg(test)]
pub(crate) static GLOBAL_SNAPSHOT: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MouseProfile;

    fn with_global_snapshot<R>(f: impl FnOnce() -> R) -> R {
        let _guard = GLOBAL_SNAPSHOT
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        f()
    }

    fn config(enabled: bool, rows: Vec<GestureBinding>) -> Config {
        let mut cfg = Config::default();
        cfg.mouse.enabled = enabled;
        cfg.mouse.gestures_enabled = true;
        cfg.mouse.gestures = rows;
        cfg.mouse.profiles = vec![MouseProfile::default()];
        cfg
    }

    fn row(gesture: &str, button: u32, binding: &str, enabled: bool) -> GestureBinding {
        GestureBinding {
            gesture: gesture.into(),
            button,
            binding: binding.into(),
            enabled,
        }
    }

    #[test]
    fn an_action_id_and_a_shortcut_both_resolve() {
        assert_eq!(
            Target::resolve("window.snap_left"),
            Some(Target::Action(Action::SnapLeft))
        );
        assert_eq!(
            Target::resolve("cmd+shift+t"),
            Some(Target::Binding(Binding::Key(shortcut::Shortcut {
                keycode: 0x11,
                flags: shortcut::FLAG_CMD | shortcut::FLAG_SHIFT,
            })))
        );
        assert_eq!(
            Target::resolve("switcher"),
            Some(Target::Binding(Binding::Switcher))
        );
        assert_eq!(Target::resolve(""), None);
        assert_eq!(Target::resolve("not-a-thing"), None);
    }

    #[test]
    fn the_master_switch_is_enough_to_leave_every_button_alone() {
        let cfg = config(false, vec![row("R", 1, "cmd+t", true)]);
        let (table, _) = build(&cfg.mouse);
        assert!(!table.claims(1));
        assert_eq!(table.len(), 0);
    }

    #[test]
    fn the_shipped_defaults_are_live_once_the_switches_are_on() {
        // A shipped list of disabled, unbound rows made gestures look dead: the switch could be on
        // and the button still did nothing. The defaults are real rows, refused only while the
        // master switches are off.
        let rows = MouseSectionDefault::rows();
        assert!(!rows.is_empty());
        let off = config(false, rows.clone());
        let (table, _) = build(&off.mouse);
        assert!(
            !table.claims(1),
            "the master switch still governs every row"
        );

        let on = config(true, rows);
        let (table, errors) = build(&on.mouse);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(table.claims(1), "the default rows are live once enabled");
    }

    /// The list a user meets on a fresh install: eleven rows, four of them two-step strokes the
    /// four direction letters cannot spell. A row that did not load would look configured and do
    /// nothing, which is the failure the shipped list exists to prevent.
    /// The trigger is the only button the table claims: the button a row was written with is left to
    /// the application.
    #[test]
    fn the_trigger_is_the_only_button_the_table_claims() {
        let mut cfg = config(true, vec![row("L", 1, "window.snap_left", true)]);
        cfg.mouse.gesture_button = crate::config::GestureTrigger::Button(3);
        let (table, errors) = build(&cfg.mouse);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(
            !table.claims(1),
            "the right button is left to the application"
        );
        assert!(table.claims(3), "the chosen side button is claimed");
        assert_eq!(
            table.lookup(3, "L").map(|row| row.target),
            Some(Target::Action(Action::SnapLeft)),
            "the row answers on the chosen button"
        );
        assert!(table.lookup(1, "L").is_none());
    }

    /// Two rows written for different buttons claim one slot under the single trigger, and the table
    /// says so instead of silently dropping the second.
    #[test]
    fn rows_written_for_different_buttons_are_duplicates() {
        let mut cfg = config(
            true,
            vec![
                row("L", 1, "window.snap_left", true),
                row("L", 3, "window.maximize", true),
            ],
        );
        cfg.mouse.gesture_button = crate::config::GestureTrigger::Button(1);
        let (table, errors) = build(&cfg.mouse);
        assert_eq!(
            errors,
            vec![(1, RowError::Duplicate)],
            "the second row is refused"
        );
        assert_eq!(table.len(), 1);
    }

    /// A file written before `gesture_button` existed loads as the right button, and the value it
    /// loads with is what the table claims.
    #[test]
    fn a_file_without_a_trigger_loads_as_the_right_button() {
        // The mouse section's own default truth is the section, not a bare table: read the whole
        // module so a missing key is exercised through the deserializer the app uses.
        let loaded: crate::config::MouseSection =
            toml::from_str("enabled = true\ngestures_enabled = true").expect("a mouse section");
        assert_eq!(loaded.gestures_enabled, true, "the fixture text is read");
        assert_eq!(
            loaded.gesture_button,
            crate::config::GestureTrigger::Button(crate::config::default_gesture_button()),
            "a missing key takes the default trigger"
        );
        // A trigger that WAS written is kept.
        let stated: crate::config::MouseSection =
            toml::from_str("enabled = true\ngesture_button = 3").expect("a mouse section");
        assert_eq!(
            stated.gesture_button,
            crate::config::GestureTrigger::Button(3)
        );
    }

    /// The stroke characters are one spelling of the direction letters, in every reader: the table
    /// loads "BL" as "DL" (B is down), and conflict detection sees two spellings of one stroke as a
    /// duplicate.
    #[test]
    fn stroke_characters_and_direction_letters_name_the_same_stroke() {
        let cfg = config(true, vec![row("BL", 1, "window.snap_left", true)]);
        let (table, errors) = build(&cfg.mouse);
        assert!(errors.is_empty(), "{errors:?}");
        let loaded = table.lookup(1, "DL").expect("BL is the stroke DL");
        assert_eq!(loaded.key, "DL");
        let aliased = config(
            true,
            vec![row("BL", 1, "cmd+t", true), row("DL", 1, "cmd+s", true)],
        );
        let conflicts = crate::mouse::gesture::conflict::find_layer_conflicts(
            &aliased.mouse.shared_gesture_layer(),
        );
        assert!(
            conflicts
                .iter()
                .any(|c| c.severity == crate::mouse::gesture::conflict::Severity::Blocking),
            "one stroke written twice must be reported: {conflicts:?}"
        );
    }

    /// An out-of-range trigger is a validation error, so the load repairs the file instead of the
    /// table guessing which button the user meant.
    #[test]
    fn an_out_of_range_trigger_is_a_validation_error() {
        let mut cfg = crate::config::Config::default();
        // The left button starts every ordinary click, so it can never start a gesture.
        cfg.mouse.gesture_button = crate::config::GestureTrigger::Button(0);
        assert!(
            cfg.validate()
                .iter()
                .any(|error| error.starts_with("mouse.gesture_button")),
            "the left button must be reported"
        );
        // A number beyond the four standard ones is what a recorded side button looks like: the tap
        // can take it, so validation must not refuse it.
        cfg.mouse.gesture_button = crate::config::GestureTrigger::Learned { learned_button: 9 };
        assert!(
            !cfg.validate()
                .iter()
                .any(|error| error.starts_with("mouse.gesture_button")),
            "a recorded side button is a valid trigger"
        );
        cfg.mouse.gesture_button = crate::config::GestureTrigger::Button(3);
        assert!(
            !cfg.validate()
                .iter()
                .any(|error| error.starts_with("mouse.gesture_button")),
            "a side button is a valid trigger"
        );
    }

    /// A device with its own layer runs ITS list, and a device without one runs the shared list.
    /// The same button and stroke on two mice can therefore run two different actions.
    #[test]
    fn each_device_runs_its_own_layer() {
        let mut cfg = config(true, vec![row("L", 1, "window.snap_left", true)]);
        let shared = cfg.mouse.shared_gesture_layer();
        let mouse_a = crate::config::DeviceMatcher {
            vendor_id: Some(0x046d),
            product_id: Some(0xc52b),
            ..Default::default()
        };
        cfg.mouse.gesture_devices.push(crate::config::GestureLayer {
            device: mouse_a.clone(),
            gestures: vec![row("L", 1, "window.maximize", true)],
            gesture_button: crate::config::GestureTrigger::Button(1),
        });

        let (shared_table, shared_errors) = build_layer(&cfg.mouse, &shared);
        assert!(shared_errors.is_empty(), "{shared_errors:?}");
        let own = cfg
            .mouse
            .gesture_layer_of(&mouse_a)
            .expect("the device layer");
        let (own_table, own_errors) = build_layer(&cfg.mouse, own);
        assert!(own_errors.is_empty(), "{own_errors:?}");

        assert_eq!(
            shared_table.lookup(1, "L").map(|row| row.target),
            Some(Target::Action(Action::SnapLeft)),
            "a mouse without a layer uses the shared list"
        );
        assert_eq!(
            own_table.lookup(1, "L").map(|row| row.target),
            Some(Target::Action(Action::Maximize)),
            "the mouse with a layer uses its own list"
        );
    }

    /// An empty device layer means "no gestures on this mouse", never "fall back to the shared
    /// list": deleting every gesture on one mouse must delete them there.
    #[test]
    fn an_empty_device_layer_claims_nothing() {
        let mut cfg = config(true, vec![row("L", 1, "window.snap_left", true)]);
        cfg.mouse.gesture_devices.push(crate::config::GestureLayer {
            device: crate::config::DeviceMatcher {
                vendor_id: Some(0x046d),
                product_id: Some(0xc52b),
                ..Default::default()
            },
            gestures: Vec::new(),
            gesture_button: crate::config::GestureTrigger::Button(1),
        });
        let empty = cfg
            .mouse
            .gesture_layer_of(&cfg.mouse.gesture_devices[0].device)
            .expect("the device layer");
        let (table, errors) = build_layer(&cfg.mouse, empty);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(!table.claims(1), "an empty layer takes no button");
    }

    /// The published snapshot answers per device: the same button and stroke on two mice runs each
    /// mouse's own action, and an unattributed press falls back to the shared layer.
    #[test]
    fn the_snapshot_answers_with_each_devices_own_layer() {
        with_global_snapshot(|| {
            let mut cfg = config(true, vec![row("L", 1, "window.snap_left", true)]);
            cfg.mouse.gesture_devices.push(crate::config::GestureLayer {
                device: crate::config::DeviceMatcher {
                    vendor_id: Some(0x046d),
                    product_id: Some(0xc52b),
                    ..Default::default()
                },
                gestures: vec![row("L", 1, "window.maximize", true)],
                gesture_button: crate::config::GestureTrigger::Button(1),
            });
            publish(&cfg);
            let published = snapshot();
            let own = published.table_for(Some((0x046d, 0xc52b)));
            assert_eq!(
                own.lookup(1, "L").map(|row| row.target),
                Some(Target::Action(Action::Maximize)),
                "the mouse with a layer runs its own action"
            );
            let shared = published.table_for(Some((0x1234, 0x5678)));
            let unattributed = published.table_for(None);
            assert_eq!(
                shared.lookup(1, "L").map(|row| row.target),
                Some(Target::Action(Action::SnapLeft)),
                "another mouse runs the shared action"
            );
            assert_eq!(
                unattributed.lookup(1, "L").map(|row| row.target),
                Some(Target::Action(Action::SnapLeft)),
                "an unattributed press uses the shared layer"
            );
        });
    }

    /// The layer a device uses is its own when it has one, and the shared layer otherwise.
    #[test]
    fn layer_for_picks_the_devices_own_layer() {
        let mut cfg = config(true, vec![row("L", 1, "window.snap_left", true)]);
        let shared = cfg.mouse.shared_gesture_layer();
        cfg.mouse.gesture_devices.push(crate::config::GestureLayer {
            device: crate::config::DeviceMatcher {
                vendor_id: Some(0x046d),
                product_id: Some(0xc52b),
                ..Default::default()
            },
            gestures: vec![row("L", 1, "window.maximize", true)],
            gesture_button: crate::config::GestureTrigger::Button(1),
        });
        let own = layer_for(&cfg.mouse, &shared, Some((0x046d, 0xc52b)));
        assert_eq!(own.gestures.len(), 1);
        assert_eq!(own.gestures[0].binding, "window.maximize");
        let other = layer_for(&cfg.mouse, &shared, Some((0x1234, 0x5678)));
        assert_eq!(other.gestures[0].binding, "window.snap_left");
        let unattributed = layer_for(&cfg.mouse, &shared, None);
        assert_eq!(unattributed.gestures[0].binding, "window.snap_left");
    }

    /// A virtual-pointer layer is published under its own key with ITS OWN list: the (VID, PID)
    /// lookup answers only for real devices, so asking it for the virtual one used to publish the
    /// shared layer under the virtual device.
    #[test]
    fn the_virtual_pointer_runs_its_own_layer() {
        with_global_snapshot(|| {
            let mut cfg = config(true, vec![row("L", 1, "window.snap_left", true)]);
            cfg.mouse.gesture_devices.push(crate::config::GestureLayer {
                device: crate::config::DeviceMatcher {
                    injected: Some(true),
                    ..Default::default()
                },
                gestures: vec![row("L", 1, "window.maximize", true)],
                gesture_button: crate::config::GestureTrigger::Button(2),
            });
            publish(&cfg);
            let published = snapshot();
            let virtual_table = published.table_for(Some(crate::mouse::device::VIRTUAL_DEVICE_KEY));
            assert_eq!(
                virtual_table.lookup(2, "L").map(|row| row.target),
                Some(Target::Action(Action::Maximize)),
                "the virtual pointer's own layer decides"
            );
            assert!(
                !virtual_table.claims(1),
                "the virtual layer's own trigger is the one it claims"
            );
            let shared = published.table_for(None);
            assert_eq!(
                shared.lookup(1, "L").map(|row| row.target),
                Some(Target::Action(Action::SnapLeft)),
                "the shared layer is untouched"
            );
        });
    }

    #[test]
    fn the_shipped_defaults_are_eleven_live_rows() {
        let rows = crate::config::MouseSection::default().gestures;
        assert_eq!(rows.len(), 11);
        let multi_step = rows
            .iter()
            .filter(|row| row.gesture.chars().count() > 1)
            .count();
        assert_eq!(multi_step, 7, "seven two-direction defaults: {rows:?}");
        let on = config(true, rows);
        let (table, errors) = build(&on.mouse);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(table.len(), 11, "every shipped row is live");
    }

    struct MouseSectionDefault;
    impl MouseSectionDefault {
        fn rows() -> Vec<GestureBinding> {
            crate::config::MouseSection::default().gestures
        }
    }

    #[test]
    fn a_valid_row_is_loaded_and_claims_its_button() {
        let cfg = config(true, vec![row("rd", 1, "window.snap_left", true)]);
        let (table, errors) = build(&cfg.mouse);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(table.claims(1));
        assert!(!table.claims(2));
        assert_eq!(
            table.lookup(1, "RD").map(|r| r.target),
            Some(Target::Action(Action::SnapLeft))
        );
    }

    #[test]
    fn the_left_button_is_refused() {
        let cfg = config(true, vec![row("R", 0, "cmd+t", true)]);
        let (table, errors) = build(&cfg.mouse);
        assert_eq!(errors, vec![(0, RowError::BadButton)]);
        assert!(!table.claims(0));
    }

    #[test]
    fn a_bad_row_does_not_take_its_siblings() {
        let cfg = config(
            true,
            vec![
                row("XX", 1, "cmd+t", true),
                row("RD", 1, "window.snap_left", true),
            ],
        );
        let (table, errors) = build(&cfg.mouse);
        assert_eq!(errors, vec![(0, RowError::BadGesture)]);
        assert!(table.claims(1), "the valid sibling still loads");
    }

    #[test]
    fn a_duplicate_row_is_refused() {
        let cfg = config(
            true,
            vec![row("R", 1, "cmd+t", true), row("r", 1, "cmd+s", true)],
        );
        let (_table, errors) = build(&cfg.mouse);
        assert_eq!(errors, vec![(1, RowError::Duplicate)]);
    }

    #[test]
    fn the_epoch_advances_for_an_exclusion_only_change() {
        with_global_snapshot(|| {
            // The reviewer's counter-example: the row set is identical, only the exclusion list moved.
            let mut cfg = config(true, vec![row("R", 1, "cmd+t", true)]);
            publish(&cfg);
            let before = current_epoch();
            cfg.mouse.gesture_excluded_apps = vec!["com.apple.Safari".into()];
            publish(&cfg);
            assert!(current_epoch() > before, "an exclusion change revokes");
        });
    }

    #[test]
    fn the_epoch_advances_when_the_parent_switch_flips_on_an_empty_table() {
        with_global_snapshot(|| {
            let mut cfg = config(true, vec![]);
            publish(&cfg);
            let before = current_epoch();
            cfg.mouse.enabled = false;
            publish(&cfg);
            assert!(current_epoch() > before, "the parent switch revokes");
        });
    }

    #[test]
    fn the_epoch_is_monotonic_across_a_round_trip() {
        with_global_snapshot(|| {
            let cfg_a = config(true, vec![row("R", 1, "cmd+t", true)]);
            let cfg_b = config(true, vec![row("R", 1, "cmd+s", true)]);
            publish(&cfg_a);
            let epoch_a = current_epoch();
            publish(&cfg_b);
            publish(&cfg_a);
            assert!(
                current_epoch() > epoch_a,
                "A -> B -> A must not restore the old epoch"
            );
        });
    }
}
