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
    let mut table = GestureTable::default();
    let mut errors = Vec::new();
    if !mouse.enabled || !mouse.gestures_enabled {
        return (table, errors);
    }
    for (index, row) in mouse.gestures.iter().enumerate() {
        match row_error(row) {
            Ok((key, target)) => {
                // Every row lands on the effective trigger, not on the button it was written with:
                // a global trigger makes the list one button's list, and two rows that were
                // distinct only by their buttons now claim the same slot.
                let button = effective_trigger_button(mouse);
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

/// The button a row is drawn on: the configuration's one trigger.
///
/// The row's own `button` is not consulted. Validation keeps the trigger inside 1..=4, so a
/// hand-written value outside it is repaired by the per-field merge rather than guessed at here.
pub(crate) fn effective_trigger_button(mouse: &crate::config::MouseSection) -> u32 {
    mouse.gesture_button
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
    pub(crate) epoch: u64,
    pub(crate) excluded: Arc<Vec<String>>,
}

static EPOCH: AtomicU64 = AtomicU64::new(1);
static PUBLISHED: OnceLock<Mutex<Published>> = OnceLock::new();

fn cell() -> &'static Mutex<Published> {
    PUBLISHED.get_or_init(|| {
        Mutex::new(Published {
            table: Arc::new(GestureTable::default()),
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
    let excluded: Vec<String> = cfg
        .mouse
        .gesture_excluded_apps
        .iter()
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .collect();
    let next = Published {
        table: Arc::new(table),
        epoch: 0,
        excluded: Arc::new(excluded),
    };
    let mut guard = cell().lock().unwrap();
    let changed = guard.table.rows != next.table.rows
        || guard.excluded.as_slice() != next.excluded.as_slice()
        || !cfg.mouse.enabled
        || !cfg.mouse.gestures_enabled;
    if changed {
        // Monotonic: an A -> B -> A round trip must not restore the old epoch, or a stale session
        // would pass its authorization check again.
        let epoch = EPOCH.fetch_add(1, Ordering::SeqCst) + 1;
        guard.table = next.table;
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
        cfg.mouse.gesture_button = 3;
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
        cfg.mouse.gesture_button = 1;
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
            crate::config::default_gesture_button(),
            "a missing key takes the default trigger"
        );
        // A trigger that WAS written is kept.
        let stated: crate::config::MouseSection =
            toml::from_str("enabled = true\ngesture_button = 3").expect("a mouse section");
        assert_eq!(stated.gesture_button, 3);
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
        let conflicts = crate::mouse::gesture::conflict::find_conflicts(&config(
            true,
            vec![row("BL", 1, "cmd+t", true), row("DL", 1, "cmd+s", true)],
        ));
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
        cfg.mouse.gesture_button = 9;
        assert!(
            cfg.validate()
                .iter()
                .any(|error| error.starts_with("mouse.gesture_button")),
            "an impossible trigger must be reported"
        );
        cfg.mouse.gesture_button = 3;
        assert!(
            !cfg.validate()
                .iter()
                .any(|error| error.starts_with("mouse.gesture_button")),
            "a side button is a valid trigger"
        );
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
