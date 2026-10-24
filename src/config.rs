use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, RwLock};

use crate::i18n::{self, tf};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
#[derive(Default)]
pub struct Config {
    pub appearance: Appearance,
    pub layout: Layout,
    pub colors: ColorsSection,
    pub fonts: Fonts,
    pub keyboard: Keyboard,
    pub i18n: I18nSection,
    pub windows: WindowsSection,
    pub logging: LoggingSection,
    pub startup: StartupSection,
    pub updates: UpdatesSection,
    pub clipboard: ClipboardSection,
    pub mouse: MouseSection,
    /// Legacy (pre-`key_bindings`) sections: still read so an upgrade keeps the user's switches,
    /// never written back — the migration into `key_bindings.items` is one-way.
    #[serde(skip_serializing)]
    pub window_control: WindowControlSection,
    #[serde(skip_serializing)]
    pub quick_actions: QuickActionsSection,
    pub key_bindings: KeyBindingsSection,
    pub keystroke_display: KeystrokeDisplaySection,
    /// Finder context-menu items, rendered by the Finder Sync app extension.
    pub finder_menu: FinderMenuSection,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct KeystrokeDisplaySection {
    pub enabled: bool,
    pub mode: String,
    pub tap_level: String,
    // The old boolean is accepted on load: true becomes caret-following (with a focused-window
    // fallback), false remains pinned to the main display.
    #[serde(
        default = "default_keystroke_display_position",
        alias = "follow_frontmost_screen",
        deserialize_with = "deserialize_keystroke_display_position"
    )]
    pub display_position: String,
    /// Where on that screen the panel first appears. `top`/`bottom` lay the keys out in a row;
    /// `left`/`right` stack them in a column, because the panel then sits against a vertical
    /// screen edge. A dragged position still wins until this setting changes, at which point the
    /// remembered position is dropped so the new choice takes effect immediately.
    #[serde(default = "default_keystroke_display_initial_position")]
    pub initial_position: String,
    /// The panel's largest extent along its stream axis, as a percentage of the target screen:
    /// the width of a horizontal stream (bottom/top) or the height of a vertical one
    /// (left/right). One value covers both, because a stream only grows along the axis its
    /// placement defines. Kept to whole tens so the slider's readout matches what is saved.
    #[serde(default = "default_keystroke_display_max_extent_percent")]
    pub max_extent_percent: u32,
    pub position: Option<KeystrokeDisplayPosition>,
}

fn default_keystroke_display_position() -> String {
    "caret".into()
}

/// The documented default: the bottom edge of the screen.
fn default_keystroke_display_initial_position() -> String {
    "bottom".into()
}

/// The panel cap's default, in percent of the target screen's stream-axis extent. 50% is what the
/// panel hard-coded before the setting existed, so an upgrade keeps the previous feel.
fn default_keystroke_display_max_extent_percent() -> u32 {
    KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_DEFAULT
}

/// Whether a configured percentage is one of the accepted steps. The slider can only produce
/// these values, so a differently-stepped value in the file is a hand edit: it is reported and
/// falls back per field rather than being silently rounded.
pub fn keystroke_display_max_extent_percent_is_valid(percent: u32) -> bool {
    (KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_MIN..=KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_MAX)
        .contains(&percent)
        && percent.is_multiple_of(KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_STEP)
}

/// Snap an arbitrary percentage onto the accepted steps. Used where a value arrives from a control
/// rather than from the file (the settings slider), so an out-of-step number is corrected instead
/// of being written back invalid.
pub fn keystroke_display_max_extent_percent_snap(percent: u32) -> u32 {
    let clamped = percent.clamp(
        KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_MIN,
        KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_MAX,
    );
    let steps = (clamped as f64 / KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_STEP as f64).round() as u32;
    (steps * KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_STEP).clamp(
        KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_MIN,
        KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_MAX,
    )
}

fn deserialize_keystroke_display_position<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum PositionSetting {
        Mode(String),
        LegacyFollowWindow(bool),
    }

    Ok(match PositionSetting::deserialize(deserializer)? {
        PositionSetting::Mode(mode) => mode,
        PositionSetting::LegacyFollowWindow(true) => "caret".into(),
        PositionSetting::LegacyFollowWindow(false) => "main".into(),
    })
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct KeystrokeDisplayPosition {
    pub x: f64,
    pub y: f64,
}

impl Default for KeystrokeDisplayPosition {
    fn default() -> Self {
        Self { x: 0.0, y: 0.0 }
    }
}

impl Default for KeystrokeDisplaySection {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: "all".into(),
            tap_level: "session".into(),
            display_position: default_keystroke_display_position(),
            initial_position: default_keystroke_display_initial_position(),
            max_extent_percent: default_keystroke_display_max_extent_percent(),
            position: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct QuickActionsSection {
    // Quick-actions master switch (Option+I/E/D/L and double-Control global hotkeys). Default false: the global
    // interception overrides other apps' Option+letters (dead keys / special chars on some
    // layouts), so it must be explicitly opted in.
    pub enabled: bool,
    // Sub-switches default to true so existing configs with only enabled=true keep working.
    #[serde(default = "default_quick_action_enabled")]
    pub open_settings: bool,
    #[serde(default = "default_quick_action_enabled")]
    pub open_finder: bool,
    #[serde(default = "default_quick_action_enabled")]
    pub show_desktop: bool,
    #[serde(default = "default_quick_action_enabled")]
    pub lock_screen: bool,
    #[serde(default = "default_quick_action_enabled")]
    pub locate_pointer: bool,
}

fn default_quick_action_enabled() -> bool {
    true
}

impl Default for QuickActionsSection {
    fn default() -> Self {
        Self {
            enabled: false,
            open_settings: true,
            open_finder: true,
            show_desktop: true,
            lock_screen: true,
            locate_pointer: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct WindowControlSection {
    // Window-control master switch (Option + arrow keys). Default false: the global
    // interception overrides other apps' Option+arrows (e.g. move-by-word in text), so it
    // must be explicitly opted in.
    pub enabled: bool,
    // Direction switches default to true so existing configs with only enabled=true keep working.
    #[serde(default = "default_window_control_direction_enabled")]
    pub up: bool,
    #[serde(default = "default_window_control_direction_enabled")]
    pub down: bool,
    #[serde(default = "default_window_control_direction_enabled")]
    pub left: bool,
    #[serde(default = "default_window_control_direction_enabled")]
    pub right: bool,
    // Cross-display shortcuts default to enabled, matching the direction shortcuts.
    #[serde(default = "default_window_control_direction_enabled")]
    pub display_up: bool,
    #[serde(default = "default_window_control_direction_enabled")]
    pub display_down: bool,
    #[serde(default = "default_window_control_direction_enabled")]
    pub display_left: bool,
    #[serde(default = "default_window_control_direction_enabled")]
    pub display_right: bool,
}

fn default_window_control_direction_enabled() -> bool {
    true
}

impl Default for WindowControlSection {
    fn default() -> Self {
        Self {
            enabled: false,
            up: true,
            down: true,
            left: true,
            right: true,
            display_up: true,
            display_down: true,
            display_left: true,
            display_right: true,
        }
    }
}

/// Global key bindings: one ordered list of "trigger -> action" entries, replacing the two
/// hardcoded interceptions (`window_control`, `quick_actions`).
///
/// One master switch gates the whole feature; each entry's own `enabled` is what the old
/// per-direction/per-action flag was, so an upgrade changes no behaviour. A binding is live only
/// when the master switch and its own switch are both on. The two legacy `enabled` flags are OR-ed
/// into the master switch on the way in, and a group that was off contributes no live row (see
/// `seed_key_bindings`): a user who had either group on keeps exactly what they had, and turning
/// both off stays off. A file that states `key_bindings.enabled` itself has already made that merged
/// decision, so its group switches are not reconciled into the rows — the file's per-row flags stand
/// as written, and a later switch-on finds them unchanged.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct KeyBindingsSection {
    pub enabled: bool,
    /// The bindings, in display order. `None` means "not stated" (the key is absent, or its TOML
    /// type was unusable) and the list is seeded from the legacy sections; `Some(vec![])` is a user
    /// who deleted every binding and is respected as such.
    pub items: Option<Vec<crate::key_bindings::KeyBinding>>,
}

/// The factory binding list, seeded from the two legacy sections: a row takes the matching
/// per-direction/per-action flag, and `window_on` / `quick_on` are the two groups' effective
/// switches (the legacy section's `enabled`, or an interim file's retired group key).
///
/// The `&&` that decides whether a binding is live is applied when the runtime table is built,
/// never here, and a row keeps its flag whenever the two groups agree or `fold_groups` is false —
/// with both off the master switch is off too, so the flags decide nothing until the user turns the
/// feature on, and a later switch-on must still arm the configured bindings rather than find every
/// default row dead.
///
/// `fold_groups` is false when the file states its own master switch. That switch is then the
/// user's decision about the merged feature rather than the OR of these two group switches, so
/// there is nothing to reconcile and the rows keep exactly the per-row flags the file carries --
/// otherwise the same explicit switch would mean different things depending on whether the file
/// happened to also carry a list.
///
/// The one case that *does* have to be resolved here is a derived master switch over a disagreeing
/// pair: one group on keeps the single master switch on, so the other group's rows would come alive
/// with it and arm the very interceptions the user had turned off (the window-direction rows
/// override Option+arrows in text fields). Those rows are seeded off, which is the only place the
/// retired group switches can still be honoured now that they share one master switch.
fn default_binding_items(
    window_control: &WindowControlSection,
    quick_actions: &QuickActionsSection,
    window_on: bool,
    quick_on: bool,
    fold_groups: bool,
) -> Vec<crate::key_bindings::KeyBinding> {
    let gated =
        |flag: bool, group_on: bool| flag && (!fold_groups || window_on == quick_on || group_on);
    let row = |action: &str, trigger: &str, enabled: bool| crate::key_bindings::KeyBinding {
        action: action.to_string(),
        trigger: trigger.to_string(),
        enabled,
    };
    vec![
        row(
            "window.snap_up",
            "option+up",
            gated(window_control.up, window_on),
        ),
        row(
            "window.snap_down",
            "option+down",
            gated(window_control.down, window_on),
        ),
        row(
            "window.snap_left",
            "option+left",
            gated(window_control.left, window_on),
        ),
        row(
            "window.snap_right",
            "option+right",
            gated(window_control.right, window_on),
        ),
        row(
            "window.display_up",
            "option+shift+up",
            gated(window_control.display_up, window_on),
        ),
        row(
            "window.display_down",
            "option+shift+down",
            gated(window_control.display_down, window_on),
        ),
        row(
            "window.display_left",
            "option+shift+left",
            gated(window_control.display_left, window_on),
        ),
        row(
            "window.display_right",
            "option+shift+right",
            gated(window_control.display_right, window_on),
        ),
        row(
            "quick.open_settings",
            "option+i",
            gated(quick_actions.open_settings, quick_on),
        ),
        row(
            "quick.open_finder",
            "option+e",
            gated(quick_actions.open_finder, quick_on),
        ),
        row(
            "quick.show_desktop",
            "option+d",
            gated(quick_actions.show_desktop, quick_on),
        ),
        row(
            "quick.lock_screen",
            "option+l",
            gated(quick_actions.lock_screen, quick_on),
        ),
        row(
            "quick.locate_pointer",
            "double-control",
            gated(quick_actions.locate_pointer, quick_on),
        ),
    ]
}

impl Default for KeyBindingsSection {
    fn default() -> Self {
        // A config that never mentioned bindings (a fresh install, or a pre-binding file about to
        // be migrated) starts from the factory list of the default legacy sections, both off.
        let window_control = WindowControlSection::default();
        let quick_actions = QuickActionsSection::default();
        Self {
            // Either legacy group being on means the feature was in use; the master switch has to
            // preserve that, or an upgrade would silently disable a shortcut the user relies on.
            enabled: window_control.enabled || quick_actions.enabled,
            items: Some(default_binding_items(
                &window_control,
                &quick_actions,
                window_control.enabled,
                quick_actions.enabled,
                // The factory state: nothing has been stated, so the two group defaults are all
                // there is to reconcile.
                true,
            )),
        }
    }
}

impl Config {
    /// Seed `key_bindings` from the legacy sections. Called once per load, right after
    /// deserialization and before validation.
    ///
    /// Presence is read off the raw document rather than from the deserialized value, because
    /// `#[serde(default)]` on the section fills a missing field from the *section's* `Default`,
    /// whose `items` is the factory list — so "the file never stated a list" and "the file stated
    /// the factory list" are indistinguishable afterwards.
    ///
    /// * No `[key_bindings]` table at all: the legacy sections hand over both the group switches and
    ///   the list.
    /// * A table without `items`: its group switches are the user's and are kept; only the list is
    ///   seeded from the legacy flags.
    /// * A stated list (including `items = []`) is authoritative and is left alone.
    ///
    /// Settle every reserved chord in one pass, and report what had to give way.
    ///
    /// The order is the rule: the clipboard shortcut yields first (it can be rebound, the switcher
    /// cannot), and then the bindings yield to whatever the switcher and the clipboard finally own.
    /// A clipboard that already holds its default and still collides is left alone, so the binding
    /// is the side that gives way rather than a no-op fallback being reported.
    ///
    /// Used after per-field fallback at load, and at runtime before anything is published, so the
    /// persisted config, the clipboard's cached chord, the settings page and the engine's table all
    /// come from the same settled values.
    pub(crate) fn settle_reserved_shortcuts(&mut self) -> Vec<String> {
        let mut errors = Vec::new();
        let items = self.key_bindings.items.clone().unwrap_or_default();
        let switcher_modifier = crate::key_bindings::modifier_flag(&self.keyboard.modifier);
        if let Err(status) = crate::key_bindings::clipboard_shortcut_status(
            &self.clipboard.shortcut,
            &items,
            switcher_modifier,
        ) {
            let default = ClipboardSection::default().shortcut;
            if default != self.clipboard.shortcut {
                let value = self.clipboard.shortcut.clone();
                errors.push(match status {
                    crate::key_bindings::ClipboardStatus::Invalid => format!(
                        "clipboard.shortcut: {}",
                        tf("errors.clipboard_shortcut_invalid", &[("value", &value)])
                    ),
                    crate::key_bindings::ClipboardStatus::Conflict => format!(
                        "clipboard.shortcut: {}",
                        i18n::t("errors.clipboard_shortcut_conflict")
                    ),
                });
                self.clipboard.shortcut = default;
            }
        }
        errors.extend(self.repair_key_binding_conflicts());
        errors
    }

    /// Drop the bindings whose chord a reserved shortcut now owns (the app switcher's modifier
    /// plus Tab, and the effective clipboard shortcut) and report each one.
    ///
    /// Called after per-field fallback at load, and again when `keyboard.modifier` changes at
    /// runtime: switching the switcher to Command+Tab turns a perfectly legal Command+Tab binding
    /// into one that would swallow the switcher chord. The binding yields in both cases; the
    /// switcher and the clipboard shortcut cannot be rebound by a binding.
    pub(crate) fn repair_key_binding_conflicts(&mut self) -> Vec<String> {
        let reserved = crate::key_bindings::Reserved::from_effective(
            &self.keyboard.modifier,
            &self.clipboard.shortcut,
        );
        match self.key_bindings.items.as_mut() {
            Some(items) => crate::key_bindings::drop_reserved_conflicts(items, &reserved),
            None => Vec::new(),
        }
    }

    /// Drop everything in `[finder_menu]` this build cannot honour, keeping the rest.
    ///
    /// Called on every load and on every runtime merge: an unknown layout falls back to the default
    /// and unknown list entries are dropped, so a hand-edited file cannot produce a menu the
    /// settings page cannot represent. A well-formed TOML file can still name a layout this build
    /// does not implement, which is why this cannot live in validation alone.
    pub(crate) fn normalize_finder_menu(&mut self) {
        if !FINDER_MENU_LAYOUTS.contains(&self.finder_menu.layout.as_str()) {
            self.finder_menu.layout = FinderMenuSection::default().layout;
        }
        self.finder_menu
            .new_file_types
            .retain(|extension| FINDER_MENU_FILE_TYPES.contains(&extension.as_str()));
        self.finder_menu
            .terminals
            .retain(|bundle_id| FINDER_MENU_TERMINALS.contains(&bundle_id.as_str()));
    }

    /// Seed `key_bindings` from the legacy sections and from the retired group switches. Called once
    /// per load, right after deserialization and before validation.
    ///
    /// Presence is read off the raw document rather than from the deserialized value, because
    /// `#[serde(default)]` on the section fills a missing field from the *section's* `Default`,
    /// whose `items` is the factory list — so "the file never stated a list" and "the file stated
    /// the factory list" are indistinguishable afterwards.
    ///
    /// * The master switch is the file's `key_bindings.enabled` when it states one.
    /// * Otherwise it is the OR of the legacy sections' `enabled` flags and of the retired
    ///   `window_control_enabled` / `quick_actions_enabled` keys (a config written by the interim
    ///   binding build): a user who had either group on keeps exactly what they had, and a file
    ///   that had both off stays off. Writing the seeded value back is what retires those keys.
    /// * A stated list (including `items = []`) keeps its own entries and order; a missing one is
    ///   seeded from the legacy per-direction/per-action flags.
    /// * Either way, the group that is off in a *disagreeing* pair has its rows switched off: those
    ///   rows would otherwise come alive with the one master switch the other group holds, arming the
    ///   interceptions the user had turned off.
    ///
    /// `section` is the raw `[key_bindings]` table, so `enabled` and the retired group keys can be
    /// told apart from their defaults.
    fn seed_key_bindings(&mut self, section: Option<&toml::Value>) {
        let stated = |key: &str| {
            section
                .and_then(|table| table.get(key))
                .and_then(toml::Value::as_bool)
        };
        // The retired keys are the interim build's group switches: they stand in for the legacy
        // sections' own `enabled`, so a file written by that build resolves to the same two groups.
        let window_on = stated("window_control_enabled").unwrap_or(self.window_control.enabled);
        let quick_on = stated("quick_actions_enabled").unwrap_or(self.quick_actions.enabled);
        // An explicitly stated master switch is the user's own decision about the merged feature and
        // says nothing about the groups, so only a derived one may fold the group state into rows.
        let stated_master = stated("enabled").is_some();
        if !stated_master {
            self.key_bindings.enabled = window_on || quick_on;
        }
        // Presence has to come from the raw document, never from `self.key_bindings.items`: the
        // section is `#[serde(default)]`, so a file that never stated a list already carries the
        // factory list, and "absent" would be indistinguishable from "the file holds exactly the
        // factory rows" -- which is how a legacy `up = false` would be resurrected.
        let has_items = section.is_some_and(|table| table.get("items").is_some());
        if !has_items {
            self.key_bindings.items = Some(default_binding_items(
                &self.window_control,
                &self.quick_actions,
                window_on,
                quick_on,
                // A stated master switch owns the merged feature and says nothing about the groups,
                // so the seeded rows keep the file's per-row flags: the same explicit switch must
                // not mean one thing with a list present and another with it absent.
                !stated_master,
            ));
        } else if !stated_master {
            // An interim file already carries its own list. It was written when the two group
            // switches were separate, so the group that was off is folded into those rows now --
            // otherwise that list would be honoured verbatim and its rows for the disabled group
            // would start intercepting with the master switch the other group holds. A stated
            // master switch is the user's own merged decision and folds nothing.
            if let Some(items) = self.key_bindings.items.as_mut() {
                apply_group_switches(items, window_on, quick_on);
            }
        }
    }
}

/// Fold the two retired group switches into an existing binding list: a group that is off in a
/// disagreeing pair has its rows switched off, and the other group's rows are left exactly as the
/// file stated them (order, triggers and the user's own per-row switches all preserved).
///
/// An entry whose action is unknown belongs to no group and is left alone — validation drops it
/// with its own error, and guessing a group for it would rewrite a row the user never disabled.
/// With both groups off, or both on, nothing is touched: the master switch alone says whether the
/// feature is on, and a later switch-on must find the rows as configured.
fn apply_group_switches(
    items: &mut [crate::key_bindings::KeyBinding],
    window_on: bool,
    quick_on: bool,
) {
    if window_on == quick_on {
        return;
    }
    for item in items.iter_mut() {
        let Some(action) = crate::key_bindings::Action::from_config_id(&item.action) else {
            continue;
        };
        let group_on = match action.group() {
            crate::key_bindings::Group::WindowControl => window_on,
            crate::key_bindings::Group::QuickActions => quick_on,
        };
        if !group_on {
            item.enabled = false;
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Appearance {
    pub theme: String,
    /// Floating-panel material family: liquid glass, plain frost, or opaque.
    /// `glass_style` and the strength knobs are sub-options that only apply to liquid glass.
    pub panel_material: String,
    pub glass_style: String,
    pub corner_radius: f64,
}

/// Valid `appearance.panel_material` values, in settings-dropdown order.
pub const PANEL_MATERIAL_VALUES: [&str; 3] = ["liquid-glass", "frost", "opaque"];

/// The dropdown index for a stored material, or `None` when the value is unknown (which the config
/// validation rejects, so only a hand-edited file can produce it).
///
/// Paired with [`panel_material_value_at`] so the two directions live in one place: the settings page
/// fills the popup from the value and the popup's action stores the index it reports, and an off-by-one
/// between those two would show one material while persisting another. The round-trip is unit-tested.
pub fn panel_material_index_of(value: &str) -> Option<usize> {
    PANEL_MATERIAL_VALUES
        .iter()
        .position(|candidate| *candidate == value)
}

/// The stored material for a dropdown index; the first item is the fallback for an out-of-range index.
pub fn panel_material_value_at(index: usize) -> &'static str {
    PANEL_MATERIAL_VALUES
        .get(index)
        .copied()
        .unwrap_or(PANEL_MATERIAL_VALUES[0])
}

/// Valid `keyboard.modifier` values, in the settings popup's item order.
pub const SWITCHER_MODIFIER_VALUES: [&str; 3] = ["option", "command", "control"];

/// The settings popup index for a `keyboard.modifier` value (unknown values show the first item).
pub fn switcher_modifier_index_of(value: &str) -> usize {
    SWITCHER_MODIFIER_VALUES
        .iter()
        .position(|candidate| *candidate == value)
        .unwrap_or(0)
}

/// The `keyboard.modifier` value for a settings popup index; out of range falls back to the first.
pub fn switcher_modifier_value_at(index: usize) -> &'static str {
    SWITCHER_MODIFIER_VALUES
        .get(index)
        .copied()
        .unwrap_or(SWITCHER_MODIFIER_VALUES[0])
}

/// Valid `layout.thumbnail_size` values, in settings-dropdown order: the automatic step first, then
/// the ladder from largest to smallest. The numbers are percent of the base card (see
/// `thumbnail_scale_of`), which is what the dropdown shows.
pub const THUMBNAIL_SIZE_VALUES: [&str; 9] =
    ["auto", "120", "110", "100", "95", "90", "85", "80", "75"];

/// The dropdown index for a stored thumbnail size, or `None` when the value is unknown (which the
/// config validation rejects, so only a hand-edited file can produce it). Paired with
/// [`thumbnail_size_value_at`] so the two directions live in one place and cannot drift by one.
pub fn thumbnail_size_index_of(value: &str) -> Option<usize> {
    THUMBNAIL_SIZE_VALUES
        .iter()
        .position(|candidate| *candidate == value)
}

/// The stored thumbnail size for a dropdown index; the first item is the fallback for an
/// out-of-range index.
pub fn thumbnail_size_value_at(index: usize) -> &'static str {
    THUMBNAIL_SIZE_VALUES
        .get(index)
        .copied()
        .unwrap_or(THUMBNAIL_SIZE_VALUES[0])
}

/// The card scale a stored size pins, or `None` for the automatic step.
pub fn thumbnail_scale_of(value: &str) -> Option<f64> {
    let percent: f64 = value.parse().ok()?;
    THUMBNAIL_SIZE_VALUES
        .contains(&value)
        .then_some(percent / 100.0)
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Layout {
    // Window-thumbnail master switch: off = the overlay keeps icon-only rendering
    // and the thumbnail service never starts. Default on; auto-sleeps without the
    // Screen Recording permission (see the thumbnail module).
    pub thumbnails_enabled: bool,
    // Focused-window thumbnail prewarm: when enabled, refresh the frontmost window at a low rate
    // while the overlay is hidden. Default off to keep background work bounded.
    pub focused_thumbnail_prewarm: bool,
    // App name in card titles: when enabled the thumbnail card's caption shows the app name
    // before the window title, separated by " · "; a titleless window, or one whose title equals
    // the app name, shows a single copy. Default off (window title only).
    pub show_app_name_in_cards: bool,
    // Window title in the switcher's bottom footer: when off, the title line is not drawn and the
    // footer band disappears with it -- the panel gets shorter by the band's height and the settings
    // gear moves from the band to the panel's bottom-right corner. The empty-state hint is not a
    // window title and stays visible (centred in the panel). Default on (the title is shown).
    pub show_footer_title: bool,
    // Card text size (points): the window title and app name scale proportionally; the large
    // icon in icon-only mode is unaffected.
    pub card_text_size: f64,
    // Thumbnail card size: "auto" picks the step from the window count, or a percent of the base
    // card (see THUMBNAIL_SIZE_VALUES) pins it. Thumbnail mode only.
    pub thumbnail_size: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct ColorsSection {
    pub dark: ThemeColors,
    pub light: ThemeColors,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct ThemeColors {
    pub status_bar_text: String,
    pub app_name: String,
    pub win_title: String,
    pub icon_inner_bg: String,
    pub icon_text: String,
    pub card_bg_sel: String,
    pub card_border_sel: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Fonts {
    pub status_bar_size: f64,
    pub status_bar_weight: f64,
    pub title_size: f64,
    pub title_weight: f64,
    pub app_name_size: f64,
    pub app_name_weight: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Keyboard {
    pub modifier: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct I18nSection {
    pub locale: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct WindowsSection {
    // Master switch for the app switcher: when off, Cmd+Tab passes through to the system
    // (the native switcher takes over) and the tap stops intercepting. Defaults to true.
    pub enabled: bool,
    // Defaults to false (hide minimized windows, matching prior behavior); bool::default() is
    // false, so Default can be derived directly.
    pub show_minimized: bool,
    // Do not show windows belonging to Command+H-hidden apps by default.
    pub show_hidden_app_windows: bool,
    // Show windows that live on another macOS desktop (Space) -- not another display. On by
    // default since 0.2.5: it is the one switch that widens the candidate scope beyond the current
    // desktop's group, and an existing config that stores `false` keeps that explicit choice. Such
    // a window has no AX element (the AX window list is Space-filtered), so its card keeps the CG
    // title and is icon-only.
    pub show_other_desktops: bool,
    // In-overlay action keys (⌘W / ⌘M / ⌘H / ⌘Q). Each acts on the HIGHLIGHTED card while the
    // switcher panel is open, and each is gated by its own switch. All default off: ⌘Q quits an
    // app and ⌘H hides one, so the honest default is opt-in -- and the panel already closes the
    // selected card with backspace, so nothing is lost by starting off. Command is the required
    // modifier in EVERY trigger mode: an Option+Tab or Control+Tab user keeps holding that modifier
    // and adds Command.
    pub panel_close_enabled: bool,
    pub panel_minimize_enabled: bool,
    pub panel_hide_enabled: bool,
    pub panel_quit_enabled: bool,
    // Overlay display position: "active_window" = follow the active window's screen,
    // "main" = always on the main screen. Defaults to following the active window.
    pub overlay_position: String,
    // Window activation mode: "hover" activates on hover; "click" activates on click.
    // Defaults to hover.
    pub activation_mode: String,
}

impl Default for WindowsSection {
    fn default() -> Self {
        Self {
            enabled: true,
            show_minimized: false,
            show_hidden_app_windows: false,
            show_other_desktops: true,
            panel_close_enabled: false,
            panel_minimize_enabled: false,
            panel_hide_enabled: false,
            panel_quit_enabled: false,
            overlay_position: "active_window".to_string(),
            activation_mode: "hover".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct LoggingSection {
    // Log level: "debug" | "info"; default "info" (normal tier, no spam; debug emits all detail).
    pub level: String,
    // Log file path; empty = use the default rolling file under ~/Library/Logs/oh-my-tab/.
    pub file_path: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct StartupSection {
    // Launch at login; defaults to false (bool::default() is false, so Default derives directly).
    pub launch_at_login: bool,
    // Hide the menu-bar status item. Default false. Unlike launch_at_login this travels with
    // export/import: it is a display preference, not a per-machine system registration.
    pub hide_menu_bar_icon: bool,
}

/// Sparkle update settings.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct UpdatesSection {
    /// Whether Sparkle may check for updates in the background.
    pub automatically_check: bool,
    /// Whether Sparkle automatically downloads and installs updates.
    pub automatically_download: bool,
}

impl Default for UpdatesSection {
    fn default() -> Self {
        Self {
            automatically_check: true,
            automatically_download: false,
        }
    }
}

/// Finder context-menu settings. The menu itself is rendered by the Finder Sync app extension in a
/// sandboxed process; this section is the main app's own description of what the menu contains, and
/// it is what every menu model is built from.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct FinderMenuSection {
    /// Master switch. Off by default: the items intrude on every context menu.
    pub enabled: bool,
    /// How the items are arranged: one of FINDER_MENU_LAYOUTS.
    pub layout: String,
    /// Show "Copy Path" for the selected item or the current folder.
    pub copy_path: bool,
    /// File extensions offered under "New File", in menu order.
    pub new_file_types: Vec<String>,
    /// Bundle ids of the terminal applications offered under "Open in Terminal", in menu order.
    pub terminals: Vec<String>,
}

impl Default for FinderMenuSection {
    fn default() -> Self {
        Self {
            enabled: false,
            layout: FINDER_MENU_DEFAULT_LAYOUT.to_string(),
            copy_path: true,
            new_file_types: FINDER_MENU_FILE_TYPES
                .iter()
                .map(|extension| extension.to_string())
                .collect(),
            terminals: vec![FINDER_MENU_TERMINALS[0].to_string()],
        }
    }
}

/// The three menu arrangements the settings page offers.
pub(crate) const FINDER_MENU_LAYOUTS: [&str; 3] = ["flat", "grouped", "submenu"];
pub(crate) const FINDER_MENU_DEFAULT_LAYOUT: &str = "grouped";

/// The file types the menu can create. A type is an extension without the dot.
pub(crate) const FINDER_MENU_FILE_TYPES: [&str; 3] = ["md", "txt", "json"];

/// Known terminal applications, in menu order. Only the installed ones are offered; this list is
/// the identity table the config is validated against, so an unknown bundle id in a hand-written
/// file is dropped instead of producing a dead menu item.
pub(crate) const FINDER_MENU_TERMINALS: [&str; 7] = [
    "com.apple.Terminal",
    "com.googlecode.iterm2",
    "com.mitchellh.ghostty",
    "dev.warp.Warp-Stable",
    "com.github.wez.wezterm",
    "net.kovidgoyal.kitty",
    "org.alacritty",
];
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct ClipboardSection {
    // History-clipboard master switch; defaults to false (no pasteboard polling).
    pub enabled: bool,
    // Global key chord that opens history. Uses the mouse button-mapping shortcut grammar.
    pub shortcut: String,
    // Max history entries (1..=100, default 50).
    pub max_entries: u32,
    // Show the source app: the source is ALWAYS recorded at copy time (ClipEntry.source_app);
    // this switch only controls whether the row displays the app name. Default false.
    pub show_source_app: bool,
    // Clear the history when the app quits, so nothing is left on disk after a session
    // (see `clipboard::discard_history_on_disk`). History is written to disk while the app
    // runs, whether or not this is on. Default false.
    pub clear_on_quit: bool,
    // Show a second header button ("clear all") in the history picker that also takes the
    // pinned entries of the visible scope. The plain clear button never touches pinned
    // entries either way. Default false.
    pub separate_clear_all: bool,
    // Move used entries to the top: pasting (select + Enter) brings the entry to the
    // front (a side effect: the write-back is re-captured by the poll as another copy).
    // When off, pasting does not reorder the history (like Windows Win+V). Default true.
    pub move_used_to_top: bool,
    // Delete after paste: when on, Option+click or Option+Enter pastes the entry and
    // removes it from the history right away (one-shot paste). Default false -- a
    // destructive gesture, strictly opt-in.
    pub delete_after_paste: bool,
    // Clear the current system pasteboard after pasting: effective only when
    // delete_after_paste is also enabled. Default false.
    pub clear_system_pasteboard_after_paste: bool,
    // Auto-expire days: unpinned entries older than N days are removed from the history
    // (memory AND persistence); pinned entries never expire. Range 0..=7; 0 = never.
    // Default 3 days.
    pub auto_expire_days: u32,
    // Image rows' height in the picker list, as a multiple of the 40pt text row: 1..=3.
    // Default 1 (a thumbnail-sized image row like any other row).
    pub image_rows: u32,
    // The clipboard picker position: "mouse" = follows the cursor, "main" = centered on
    // the main screen. Defaults to the center of the main screen.
    pub picker_position: String,
    // Where the selection lands after pin/unpin: true = follow the toggled entry to its
    // new position; false = keep the current display position (pointing at the next
    // entry, convenient for batch pinning). Defaults to follow.
    pub pin_follow_selection: bool,
    // Stop recording without turning the feature off. Persisted on purpose: a pause is a standing
    // intent, so restarting the app must not silently resume collecting. While it is on, the timer
    // keeps running and advances the changeCount baseline, so nothing copied during the pause is
    // recorded afterwards (see clipboard::set_collection_paused).
    pub paused: bool,
    // Applications whose copies are never recorded. Matched by bundle identifier against the
    // frontmost app and, when the pasteboard declares one, the copier named by
    // `org.nspasteboard.source`. The built-in list is the default; an explicitly emptied list stays
    // empty. New built-in entries added in later versions do NOT propagate into a list the user has
    // already saved (there is no defaults-provenance marker by design: auto-adding an entry would
    // re-introduce an app the user deliberately removed).
    pub excluded_apps: Vec<ExcludedApp>,
}

/// One entry of `[clipboard] excluded_apps`. `bundle_id` is the only matching key; `name` is a
/// display snapshot taken when the entry was added and is never compared, so an app that renames
/// itself still matches.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ExcludedApp {
    pub bundle_id: String,
    #[serde(default)]
    pub name: String,
}

impl ExcludedApp {
    pub(crate) fn new(bundle_id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            bundle_id: bundle_id.into(),
            name: name.into(),
        }
    }
}

/// The built-in exclusion list: macOS's own credential stores plus the password managers whose
/// bundle identifiers are stable and publicly documented.
///
/// The list stays short on purpose. A wrong identifier is harmless (it never matches) but a long
/// list of unverified ones is not evidence of coverage, so only identifiers with a source are kept:
/// `com.apple.Passwords` was read from `/System/Applications/Passwords.app/Contents/Info.plist`
/// on macOS 26; `com.apple.keychainaccess` is the system Keychain Access bundle; the four
/// third-party identifiers are the ones those vendors ship in their public macOS builds.
pub(crate) fn default_excluded_apps() -> Vec<ExcludedApp> {
    vec![
        ExcludedApp::new("com.apple.keychainaccess", "Keychain Access"),
        ExcludedApp::new("com.apple.Passwords", "Passwords"),
        ExcludedApp::new("com.1password.1password", "1Password"),
        ExcludedApp::new("com.agilebits.onepassword7", "1Password 7"),
        ExcludedApp::new("com.bitwarden.desktop", "Bitwarden"),
        ExcludedApp::new("org.keepassxc.keepassxc", "KeePassXC"),
    ]
}

impl Default for ClipboardSection {
    fn default() -> Self {
        Self {
            enabled: false,
            shortcut: "option+v".to_string(),
            max_entries: 50,
            show_source_app: false,
            clear_on_quit: false,
            separate_clear_all: false,
            move_used_to_top: true,
            delete_after_paste: false,
            clear_system_pasteboard_after_paste: false,
            auto_expire_days: 3,
            image_rows: 1,
            picker_position: "main".to_string(),
            pin_follow_selection: true,
            paused: false,
            excluded_apps: default_excluded_apps(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct PointerSection {
    // Disable system pointer acceleration for 1:1 linear cursor tracking. Default false.
    pub disable_acceleration: bool,
}

/// The valid range for pointer acceleration / tracking speed: **0..=10**.
///
/// The platform property's own domain is [0, 40] ∪ {-1}, but the usable band is only ~0.3-3
/// (macOS ships 1.00 for the mouse key and 0.6875 for the trackpad/pointer key; a value of 10
/// already feels unusably fast), so anything above 10 is
/// wasted travel that also destroys a linear slider's precision. The accepted range is therefore
/// narrowed to 0..=10; should a low-DPI device ever need a larger multiplier, widening this single
/// constant is enough. Semantics: 0 is the bottom of the normal range (slowest, and it really
/// takes effect), -1 is the "acceleration and sensitivity disabled" sentinel (used by the legacy
/// fallback path); only "unset" (None) leaves the device value alone.
pub const MOUSE_ACCELERATION_MIN: f64 = 0.0;
pub const MOUSE_ACCELERATION_MAX: f64 = 10.0;

/// The keystroke panel's stream-axis cap, as a percentage of the target screen. The floor keeps
/// the two controls (drag handle, settings button) and at least one keycap; the ceiling is "no
/// extra limit", because the panel never overflows the screen anyway. Whole tens only: the
/// settings slider's readout, the saved value and the validation all speak the same steps.
pub const KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_MIN: u32 = 10;
pub const KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_MAX: u32 = 100;
pub const KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_STEP: u32 = 10;
pub const KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_DEFAULT: u32 = 50;

/// Device matcher (None = wildcard, i.e. "All Mice"). Config matches devices by VID+PID.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct DeviceMatcher {
    // Flattened as device_vendor_id / device_product_id (top-level scalars for hand-written TOML).
    #[serde(rename = "device_vendor_id", skip_serializing_if = "Option::is_none")]
    pub vendor_id: Option<u32>,
    #[serde(rename = "device_product_id", skip_serializing_if = "Option::is_none")]
    pub product_id: Option<u32>,
    // Virtual-pointer profile: the pointer a software KVM (e.g. Deskflow) injects. It has no HID
    // device and no VID/PID, so it can only be identified by "some other process injected this
    // event" (CGEventSourceUnixProcessID != 0) -- VID/PID matching cannot express it.
    // true = this profile matches injected events only; None = an ordinary device (or wildcard).
    #[serde(rename = "device_injected", skip_serializing_if = "Option::is_none")]
    pub injected: Option<bool>,
}

impl DeviceMatcher {
    /// Whether this matcher is the virtual-pointer one (matches injected events only).
    pub fn is_virtual(&self) -> bool {
        self.injected == Some(true)
    }
}

/// Pointer override (partial; None = inherit from the lower layer).
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct PartialPointerSection {
    pub disable_acceleration: Option<bool>,
    // Tracking speed in linear mode (0..=40), written to HIDPointerAcceleration (IOFixed:
    // value × 65536). Only takes effect while disable_acceleration is on: under linear scaling
    // that property is the tracking speed itself, whereas with the switch off it is the strength
    // of macOS's acceleration curve, a different meaning, so it is not written then.
    // None = leave the device's current value alone.
    pub acceleration: Option<f64>,
}

/// Per-device smooth-scroll overrides. Every field is optional so matching mouse profiles merge
/// independently, with later profiles overriding only fields they specify.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct SmoothPartial {
    // Read only so old configs can be migrated to MouseProfile::scroll_mode. The migration clears
    // this field before any resolved runtime config is built; it is never written back.
    #[serde(skip_serializing)]
    pub enabled: Option<bool>,
    pub preset: Option<String>,
    pub response: Option<f64>,
    pub speed: Option<f64>,
    pub acceleration: Option<f64>,
    pub inertia: Option<f64>,
}

/// A single profile. device = None is the "All Mice" profile (the default layer).
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct MouseProfile {
    /// Device matcher; None = matches all mice (serves as the default layer).
    #[serde(flatten)]
    pub device: DeviceMatcher,
    // Reverse scroll direction. true = flip relative to the system's current direction (same as
    // LinearMouse; no natural-scroll setting is read: HID-tap events already carry the system
    // natural-scroll flip and synthetic events aren't flipped again, see should_flip in scrolling.rs).
    pub reverse_scroll: Option<bool>,
    // The active wheel path: passthrough, line normalization, or timed smooth pixels.
    pub scroll_mode: Option<String>,
    // Line mode lines per notch (1..=10).
    pub line_count: Option<u32>,
    pub pointer: Option<PartialPointerSection>,
    pub smooth_scrolling: Option<SmoothPartial>,
    // Button mappings: button number (string, >= 2) -> shortcut description (e.g. "cmd+shift+v").
    // Left (0) and right (1) buttons cannot be bound, so the user can never lock themselves
    // out of clicking.
    #[serde(default)]
    pub button_mappings: std::collections::HashMap<String, String>,
    // Per-profile master switch for button mappings (None = inherit the lower layer;
    // defaults to true, so bindings take effect as soon as they exist). Independent per
    // device -- different mice can differ. When off, the device's mappings are skipped
    // (events pass through).
    #[serde(default)]
    pub button_mappings_enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct MouseSection {
    // Enable mouse control features (master switch). Default false.
    pub enabled: bool,
    // Profile list: the first profile without a device field is the "All Mice" default layer;
    // subsequent per-device profiles override it. Merge semantics: all matching profiles are
    // traversed, later ones win.
    pub profiles: Vec<MouseProfile>,

    // Option-typed on purpose: only old-format files that explicitly wrote these flat keys
    // deserialize to Some; new-format files skip them on serialize, so they always reload as
    // None -- eliminating the "serde-default masquerades as legacy content" overwrite bug
    // (see docs/test-review.md).
    #[serde(skip_serializing)]
    pub reverse_scroll: Option<bool>,
    #[serde(skip_serializing)]
    pub scroll_mode: Option<String>,
    #[serde(skip_serializing)]
    pub line_count: Option<u32>,
    #[serde(skip_serializing)]
    pub pointer: Option<PointerSection>,
}

impl Default for MouseSection {
    fn default() -> Self {
        Self {
            enabled: false,
            // Default includes an "All Mice" profile matching the old defaults.
            profiles: vec![MouseProfile {
                reverse_scroll: Some(false),
                scroll_mode: Some("default".into()),
                line_count: Some(3),
                pointer: Some(PartialPointerSection {
                    disable_acceleration: Some(false),
                    acceleration: None,
                }),
                smooth_scrolling: Some(SmoothPartial {
                    enabled: None,
                    preset: Some("ease_in_out".into()),
                    response: Some(0.68),
                    speed: Some(1.02),
                    acceleration: Some(1.10),
                    inertia: Some(0.74),
                }),
                ..Default::default()
            }],
            reverse_scroll: None,
            scroll_mode: None,
            line_count: None,
            pointer: None,
        }
    }
}

impl MouseSection {
    /// Migrate legacy flat fields into an "All Mice" profile and the legacy smooth-enabled flag
    /// into the single scroll mode. Returns whether the caller should rewrite the config.
    pub(crate) fn migrate_legacy(&mut self) -> bool {
        // Legacy fields are Option-typed: only keys explicitly written by old-format files are
        // Some. New-format files skip them on serialize, so they reload as None and no longer
        // trigger a migration that would clobber user profiles (serde used to backfill
        // "default" into the legacy scroll_mode, making has_legacy always true).
        let has_legacy = self.reverse_scroll.is_some()
            || self.scroll_mode.is_some()
            || self.line_count.is_some()
            || self.pointer.is_some();

        let mut changed = false;
        if has_legacy {
            // Legacy fields present: fold them into (or create) an "All Mice" profile.
            let legacy_profile = MouseProfile {
                reverse_scroll: self.reverse_scroll,
                scroll_mode: self.scroll_mode.clone().map(|m| {
                    if m.is_empty() {
                        "default".into()
                    } else {
                        m
                    }
                }),
                line_count: self.line_count.map(|n| if n == 0 { 3 } else { n }),
                pointer: self.pointer.take().map(|p| PartialPointerSection {
                    disable_acceleration: Some(p.disable_acceleration),
                    ..Default::default()
                }),
                ..Default::default()
            };

            // Preserve profile fields that have no corresponding legacy flat key, including
            // smooth tuning. Explicit legacy values still replace their profile counterparts.
            if let Some(idx) = self
                .profiles
                .iter()
                .position(|p| p.device.vendor_id.is_none() && p.device.product_id.is_none())
            {
                let profile = &mut self.profiles[idx];
                if legacy_profile.reverse_scroll.is_some() {
                    profile.reverse_scroll = legacy_profile.reverse_scroll;
                }
                if legacy_profile.scroll_mode.is_some() {
                    profile.scroll_mode = legacy_profile.scroll_mode;
                }
                if legacy_profile.line_count.is_some() {
                    profile.line_count = legacy_profile.line_count;
                }
                if legacy_profile.pointer.is_some() {
                    profile.pointer = legacy_profile.pointer;
                }
            } else {
                // No "All Mice" profile: insert at the front (default first, per-device after).
                self.profiles.insert(0, legacy_profile);
            }

            // Clear legacy fields (prevents re-migration; serialization skips them anyway).
            self.reverse_scroll = None;
            self.scroll_mode = None;
            self.line_count = None;
            self.pointer = None;
            changed = true;
        }

        // The old runtime let enabled=true win over either Default or Line. Convert that choice
        // independently in every profile before layering, then discard the non-serialized flag.
        for profile in &mut self.profiles {
            if let Some(smooth) = profile.smooth_scrolling.as_mut() {
                if smooth.enabled == Some(true) {
                    profile.scroll_mode = Some("smooth".into());
                }
                if smooth.enabled.take().is_some() {
                    changed = true;
                }
            }
        }

        // Fresh and already-migrated files still need an "All Mice" base profile.
        let has_default = self
            .profiles
            .iter()
            .any(|p| p.device.vendor_id.is_none() && p.device.product_id.is_none());
        if !has_default {
            self.profiles.insert(0, Self::default().profiles[0].clone());
            changed = true;
        }
        changed
    }
}

impl Default for Appearance {
    fn default() -> Self {
        Appearance {
            // Follow the system by default while preserving explicit user choices from existing
            // config files.
            theme: "auto".into(),
            panel_material: "liquid-glass".into(),
            glass_style: "regular".into(),
            // Default Liquid Glass overlay tint (RRGGBBAA); the settings page lets users pick another color.
            corner_radius: 32.0,
        }
    }
}

impl Default for Layout {
    fn default() -> Self {
        Layout {
            thumbnails_enabled: true,
            focused_thumbnail_prewarm: false,
            show_app_name_in_cards: false,
            show_footer_title: true,
            card_text_size: 15.0,
            thumbnail_size: "auto".into(),
        }
    }
}

impl Default for ColorsSection {
    fn default() -> Self {
        ColorsSection {
            dark: ThemeColors::dark_default(),
            light: ThemeColors::light_default(),
        }
    }
}

impl Default for ThemeColors {
    fn default() -> Self {
        Self::dark_default()
    }
}

impl ThemeColors {
    fn dark_default() -> Self {
        // Card text colors follow the HTML reference: primary (window title)
        // rgba(0,0,0,.82) -> same alpha in white for the dark theme; secondary (app
        // name) rgba(0,0,0,.34). alpha = 0.82/0.34 x 255 ~= D1/57.
        ThemeColors {
            status_bar_text: "999999ff".into(),
            app_name: "FFFFFF57".into(),
            win_title: "FFFFFFD1".into(),
            icon_inner_bg: "22224444".into(),
            icon_text: "9999bbff".into(),
            card_bg_sel: "22224444".into(),
            card_border_sel: "5577ccff".into(),
        }
    }

    fn light_default() -> Self {
        ThemeColors {
            status_bar_text: "333333ff".into(),
            // rgba(0,0,0,.82) / rgba(0,0,0,.34); see the dark_default comment.
            app_name: "00000057".into(),
            win_title: "000000D1".into(),
            // The mockup's preview background #f6f7f9: a near-white neutral so the
            // preview reads as the same surface as the card.
            icon_inner_bg: "F6F7F9FF".into(),
            icon_text: "666688ff".into(),
            // The mockup's .item.selected background rgba(255,255,255,.88): a clearly
            // visible light surface carrying the caption and preview (the faint accent
            // tint was invisible on the glass panel -- user-reported).
            card_bg_sel: "FFFFFFE0".into(),
            // The mockup's crisp 1.5px selected border rgba(75,123,236,.78) -- the only
            // outline visible against the white surface (a 2px soft ring disappears
            // on white; see the refresh_highlight comment).
            card_border_sel: "4B7BECC7".into(),
        }
    }
}

impl Default for Fonts {
    fn default() -> Self {
        // Built-in overlay typography follows the documented 400/600/700 ladder. Explicit
        // config values remain advanced user overrides; legacy default weights migrate on load.
        Fonts {
            status_bar_size: 15.0,
            status_bar_weight: crate::theme::FONT_WEIGHT_SEMIBOLD,
            title_size: 12.0,
            title_weight: crate::theme::FONT_WEIGHT_SEMIBOLD,
            app_name_size: 10.0,
            app_name_weight: crate::theme::FONT_WEIGHT_REGULAR,
        }
    }
}

impl Default for Keyboard {
    fn default() -> Self {
        Keyboard {
            // Default Cmd+Tab; users can switch back to Option+Tab in Settings.
            modifier: "command".into(),
        }
    }
}

impl Default for I18nSection {
    fn default() -> Self {
        I18nSection {
            locale: "auto".into(), // follow system language
        }
    }
}

impl Default for LoggingSection {
    fn default() -> Self {
        LoggingSection {
            level: "info".into(),
            file_path: String::new(),
        }
    }
}

fn is_hex8(s: &str) -> bool {
    s.len() == 8 && s.chars().all(|c| c.is_ascii_hexdigit())
}

impl Config {
    pub fn validate(&self) -> Vec<String> {
        let mut errs: Vec<String> = Vec::new();

        if !["dark", "light", "auto"].contains(&self.appearance.theme.as_str()) {
            errs.push(tf(
                "errors.appearance_theme_invalid",
                &[("value", &self.appearance.theme)],
            ));
        }
        if !["regular", "clear"].contains(&self.appearance.glass_style.as_str()) {
            errs.push(tf(
                "errors.appearance_glass_style_invalid",
                &[("value", &self.appearance.glass_style)],
            ));
        }
        if !THUMBNAIL_SIZE_VALUES.contains(&self.layout.thumbnail_size.as_str()) {
            errs.push(tf(
                "errors.layout_thumbnail_size_invalid",
                &[("value", &self.layout.thumbnail_size)],
            ));
        }
        if !PANEL_MATERIAL_VALUES.contains(&self.appearance.panel_material.as_str()) {
            errs.push(tf(
                "errors.appearance_panel_material_invalid",
                &[("value", &self.appearance.panel_material)],
            ));
        }
        if self.appearance.corner_radius < 0.0 {
            errs.push(tf(
                "errors.appearance_corner_radius_invalid",
                &[("value", &self.appearance.corner_radius.to_string())],
            ));
        }

        for (theme, colors) in [("dark", &self.colors.dark), ("light", &self.colors.light)] {
            let prefix = format!("colors.{theme}");
            if !is_hex8(&colors.status_bar_text) {
                errs.push(tf(
                    "errors.colors_not_hex8",
                    &[("field", &format!("{prefix}.status_bar_text"))],
                ));
            }
            if !is_hex8(&colors.app_name) {
                errs.push(tf(
                    "errors.colors_not_hex8",
                    &[("field", &format!("{prefix}.app_name"))],
                ));
            }
            if !is_hex8(&colors.win_title) {
                errs.push(tf(
                    "errors.colors_not_hex8",
                    &[("field", &format!("{prefix}.win_title"))],
                ));
            }
            if !is_hex8(&colors.icon_inner_bg) {
                errs.push(tf(
                    "errors.colors_not_hex8",
                    &[("field", &format!("{prefix}.icon_inner_bg"))],
                ));
            }
            if !is_hex8(&colors.icon_text) {
                errs.push(tf(
                    "errors.colors_not_hex8",
                    &[("field", &format!("{prefix}.icon_text"))],
                ));
            }
            if !is_hex8(&colors.card_bg_sel) {
                errs.push(tf(
                    "errors.colors_not_hex8",
                    &[("field", &format!("{prefix}.card_bg_sel"))],
                ));
            }
            if !is_hex8(&colors.card_border_sel) {
                errs.push(tf(
                    "errors.colors_not_hex8",
                    &[("field", &format!("{prefix}.card_border_sel"))],
                ));
            }
        }

        if !self.layout.card_text_size.is_finite()
            || !(13.0..=20.0).contains(&self.layout.card_text_size)
        {
            errs.push(tf(
                "errors.layout_card_text_size_invalid",
                &[("value", &self.layout.card_text_size.to_string())],
            ));
        }

        if !self.fonts.status_bar_size.is_finite()
            || !(0.0..=20.0).contains(&self.fonts.status_bar_size)
        {
            errs.push(tf(
                "errors.fonts_size_invalid",
                &[
                    ("field", "fonts.status_bar_size"),
                    ("value", &self.fonts.status_bar_size.to_string()),
                ],
            ));
        }
        if self.fonts.status_bar_weight < 0.0 || self.fonts.status_bar_weight > 1.0 {
            errs.push(tf(
                "errors.fonts_weight_invalid",
                &[
                    ("field", "fonts.status_bar_weight"),
                    ("value", &self.fonts.status_bar_weight.to_string()),
                ],
            ));
        }
        if self.fonts.title_size < 8.0 {
            errs.push(tf(
                "errors.fonts_size_invalid",
                &[
                    ("field", "fonts.title_size"),
                    ("value", &self.fonts.title_size.to_string()),
                ],
            ));
        }
        if self.fonts.title_weight < 0.0 || self.fonts.title_weight > 1.0 {
            errs.push(tf(
                "errors.fonts_weight_invalid",
                &[
                    ("field", "fonts.title_weight"),
                    ("value", &self.fonts.title_weight.to_string()),
                ],
            ));
        }
        if self.fonts.app_name_size < 8.0 {
            errs.push(tf(
                "errors.fonts_size_invalid",
                &[
                    ("field", "fonts.app_name_size"),
                    ("value", &self.fonts.app_name_size.to_string()),
                ],
            ));
        }
        if self.fonts.app_name_weight < 0.0 || self.fonts.app_name_weight > 1.0 {
            errs.push(tf(
                "errors.fonts_weight_invalid",
                &[
                    ("field", "fonts.app_name_weight"),
                    ("value", &self.fonts.app_name_weight.to_string()),
                ],
            ));
        }

        if !SWITCHER_MODIFIER_VALUES.contains(&self.keyboard.modifier.as_str()) {
            errs.push(tf(
                "errors.keyboard_modifier_invalid",
                &[("value", &self.keyboard.modifier)],
            ));
        }

        let locale_valid =
            ["auto", "en", "zh-Hans", "zh-Hant"].contains(&self.i18n.locale.as_str());
        #[cfg(any(debug_assertions, feature = "dev-long-text"))]
        let locale_valid = locale_valid || self.i18n.locale == i18n::TEST_LONG_LOCALE;
        if !locale_valid {
            errs.push(tf(
                "errors.i18n_locale_invalid",
                &[("value", &self.i18n.locale)],
            ));
        }

        if !["debug", "info"].contains(&self.logging.level.as_str()) {
            errs.push(tf(
                "errors.logging_level_invalid",
                &[("value", &self.logging.level)],
            ));
        }

        if !["active_window", "main"].contains(&self.windows.overlay_position.as_str()) {
            errs.push(tf(
                "errors.windows_overlay_position_invalid",
                &[("value", &self.windows.overlay_position)],
            ));
        }
        if !["hover", "click"].contains(&self.windows.activation_mode.as_str()) {
            errs.push(tf(
                "errors.windows_activation_mode_invalid",
                &[("value", &self.windows.activation_mode)],
            ));
        }

        if !["all", "shortcuts", "commands"].contains(&self.keystroke_display.mode.as_str()) {
            errs.push(tf(
                "errors.keystroke_display_mode_invalid",
                &[("value", &self.keystroke_display.mode)],
            ));
        }
        if !["session", "hid"].contains(&self.keystroke_display.tap_level.as_str()) {
            errs.push(tf(
                "errors.keystroke_display_tap_level_invalid",
                &[("value", &self.keystroke_display.tap_level)],
            ));
        }
        if !["main", "caret"].contains(&self.keystroke_display.display_position.as_str()) {
            errs.push(tf(
                "errors.keystroke_display_position_mode_invalid",
                &[("value", &self.keystroke_display.display_position)],
            ));
        }
        if !["top", "bottom", "left", "right"]
            .contains(&self.keystroke_display.initial_position.as_str())
        {
            errs.push(tf(
                "errors.keystroke_display_initial_position_invalid",
                &[("value", &self.keystroke_display.initial_position)],
            ));
        }
        if !keystroke_display_max_extent_percent_is_valid(self.keystroke_display.max_extent_percent)
        {
            errs.push(tf(
                "errors.keystroke_display_max_extent_percent_invalid",
                &[(
                    "value",
                    &self.keystroke_display.max_extent_percent.to_string(),
                )],
            ));
        }
        if let Some(position) = self.keystroke_display.position {
            for (axis, value) in [("x", position.x), ("y", position.y)] {
                if !value.is_finite() {
                    errs.push(format!(
                        "keystroke_display.position.{axis}: {}",
                        tf(
                            "errors.keystroke_display_position_invalid",
                            &[("field", axis), ("value", &value.to_string())]
                        )
                    ));
                }
            }
        }

        if !(1..=100).contains(&self.clipboard.max_entries) {
            errs.push(tf(
                "errors.clipboard_max_entries_invalid",
                &[("value", &self.clipboard.max_entries.to_string())],
            ));
        }
        if !(1..=3).contains(&self.clipboard.image_rows) {
            errs.push(tf(
                "errors.clipboard_image_rows_invalid",
                &[("value", &self.clipboard.image_rows.to_string())],
            ));
        }
        if self.clipboard.auto_expire_days > 7 {
            errs.push(tf(
                "errors.clipboard_auto_expire_days_invalid",
                &[("value", &self.clipboard.auto_expire_days.to_string())],
            ));
        }
        if !["mouse", "main"].contains(&self.clipboard.picker_position.as_str()) {
            errs.push(tf(
                "errors.clipboard_picker_position_invalid",
                &[("value", &self.clipboard.picker_position)],
            ));
        }
        match crate::key_bindings::clipboard_shortcut_status(
            &self.clipboard.shortcut,
            self.key_bindings.items.as_deref().unwrap_or(&[]),
            crate::key_bindings::modifier_flag(&self.keyboard.modifier),
        ) {
            Ok(_) => {}
            Err(crate::key_bindings::ClipboardStatus::Invalid) => errs.push(format!(
                "clipboard.shortcut: {}",
                tf(
                    "errors.clipboard_shortcut_invalid",
                    &[("value", &self.clipboard.shortcut)]
                )
            )),
            Err(crate::key_bindings::ClipboardStatus::Conflict) => errs.push(format!(
                "clipboard.shortcut: {}",
                i18n::t("errors.clipboard_shortcut_conflict")
            )),
        }
        errs.extend(crate::key_bindings::validate_items(
            self.key_bindings.items.as_ref(),
        ));

        for (i, p) in self.mouse.profiles.iter().enumerate() {
            let prefix = format!("mouse.profiles[{i}]");
            if let Some(ref mode) = p.scroll_mode {
                if !["default", "line", "smooth"].contains(&mode.as_str()) {
                    errs.push(tf("errors.mouse_scroll_mode_invalid", &[("value", mode)]));
                    // Use the prefix to indicate which profile failed.
                    if let Some(last) = errs.last_mut() {
                        *last = format!("{prefix}.scroll_mode: {last}");
                    }
                }
            }
            if let Some(lc) = p.line_count {
                if !(1..=10).contains(&lc) {
                    let msg = tf(
                        "errors.mouse_line_count_invalid",
                        &[("value", &lc.to_string())],
                    );
                    errs.push(format!("{prefix}.line_count: {msg}"));
                }
            }
            if let Some(smooth) = &p.smooth_scrolling {
                if let Some(preset) = &smooth.preset {
                    if crate::mouse::smooth::presets::SmoothPreset::from_str(preset).is_none() {
                        errs.push(format!(
                            "{prefix}.smooth_scrolling.preset: {}",
                            tf(
                                "errors.mouse_smooth_field_invalid",
                                &[("field", "preset"), ("value", preset)]
                            )
                        ));
                    }
                }
                for (field, value, min, max) in [
                    ("response", smooth.response, 0.0, 2.0),
                    ("speed", smooth.speed, 0.0, 8.0),
                    ("acceleration", smooth.acceleration, 0.0, 8.0),
                    ("inertia", smooth.inertia, 0.0, 8.0),
                ] {
                    if let Some(value) = value {
                        if !value.is_finite() || !(min..=max).contains(&value) {
                            errs.push(format!(
                                "{prefix}.smooth_scrolling.{field}: {}",
                                tf(
                                    "errors.mouse_smooth_field_invalid",
                                    &[("field", field), ("value", &value.to_string())]
                                )
                            ));
                        }
                    }
                }
            }
            // Pointer acceleration / tracking speed: 0..=10 (see MOUSE_ACCELERATION_MAX).
            if let Some(acc) = p.pointer.as_ref().and_then(|ptr| ptr.acceleration) {
                if !(MOUSE_ACCELERATION_MIN..=MOUSE_ACCELERATION_MAX).contains(&acc) {
                    let msg = tf(
                        "errors.mouse_pointer_acceleration_invalid",
                        &[("value", &acc.to_string())],
                    );
                    errs.push(format!("{prefix}.pointer.acceleration: {msg}"));
                }
            }
            // Button mappings: valid button numbers (numeric, >= 2) + parseable shortcuts.
            errs.extend(crate::mouse::shortcut::validate_mappings(
                &p.button_mappings,
                &prefix,
            ));
        }

        errs
    }

    /// Merge valid fields from `other` into `self`, keeping defaults for invalid fields.
    /// Returns the list of fields that were rejected (with reasons).
    pub fn merge_valid(&mut self, other: Config, errs: &[String]) {
        // For each top-level section, if validation had no errors for that section,
        // keep the loaded value; otherwise the Default (already in `self`) stays.
        //
        // Simpler approach: use `other` wholesale but reset individual fields that
        // had errors back to the defaults.
        let has_error = |prefix: &str| errs.iter().any(|e| e.starts_with(prefix));

        // appearance
        if !has_error("appearance") {
            self.appearance = other.appearance;
        } else {
            // Selective merge: only keep valid sub-fields
            if !errs.iter().any(|e| e.starts_with("appearance.theme")) {
                self.appearance.theme = other.appearance.theme;
            }
            if !errs.iter().any(|e| e.starts_with("appearance.glass_style")) {
                self.appearance.glass_style = other.appearance.glass_style;
            }
            if !errs
                .iter()
                .any(|e| e.starts_with("appearance.panel_material"))
            {
                self.appearance.panel_material = other.appearance.panel_material;
            }
            if !errs
                .iter()
                .any(|e| e.starts_with("appearance.corner_radius"))
            {
                self.appearance.corner_radius = other.appearance.corner_radius;
            }
        }

        // layout
        if !has_error("layout.") {
            self.layout = other.layout;
        } else {
            // Booleans are always valid; adopt the loaded value unconditionally
            // (same convention as the other boolean switches).
            self.layout.thumbnails_enabled = other.layout.thumbnails_enabled;
            self.layout.focused_thumbnail_prewarm = other.layout.focused_thumbnail_prewarm;
            self.layout.show_app_name_in_cards = other.layout.show_app_name_in_cards;
            self.layout.show_footer_title = other.layout.show_footer_title;
            if !errs.iter().any(|e| e.starts_with("layout.card_text_size")) {
                self.layout.card_text_size = other.layout.card_text_size;
            }
            if !errs.iter().any(|e| e.starts_with("layout.thumbnail_size")) {
                self.layout.thumbnail_size = other.layout.thumbnail_size;
            }
        }

        // colors
        if !has_error("colors.") {
            self.colors = other.colors;
        } else {
            // Per-theme, per-field merge
            for (theme, ours, theirs) in [
                ("dark", &mut self.colors.dark, &other.colors.dark),
                ("light", &mut self.colors.light, &other.colors.light),
            ] {
                Self::merge_colors(ours, theirs, theme, errs);
            }
        }

        // fonts
        if !has_error("fonts.") {
            self.fonts = other.fonts;
        } else {
            if !errs.iter().any(|e| e.starts_with("fonts.status_bar_size")) {
                self.fonts.status_bar_size = other.fonts.status_bar_size;
            }
            if !errs
                .iter()
                .any(|e| e.starts_with("fonts.status_bar_weight"))
            {
                self.fonts.status_bar_weight = other.fonts.status_bar_weight;
            }
            if !errs.iter().any(|e| e.starts_with("fonts.title_size")) {
                self.fonts.title_size = other.fonts.title_size;
            }
            if !errs.iter().any(|e| e.starts_with("fonts.title_weight")) {
                self.fonts.title_weight = other.fonts.title_weight;
            }
            if !errs.iter().any(|e| e.starts_with("fonts.app_name_size")) {
                self.fonts.app_name_size = other.fonts.app_name_size;
            }
            if !errs.iter().any(|e| e.starts_with("fonts.app_name_weight")) {
                self.fonts.app_name_weight = other.fonts.app_name_weight;
            }
        }

        // keyboard
        if !has_error("keyboard.") {
            self.keyboard = other.keyboard;
        } else {
            if !errs.iter().any(|e| e.starts_with("keyboard.modifier")) {
                self.keyboard.modifier = other.keyboard.modifier;
            }
        }

        // i18n
        if !has_error("i18n.") {
            self.i18n = other.i18n;
        } else {
            if !errs.iter().any(|e| e.starts_with("i18n.locale")) {
                self.i18n.locale = other.i18n.locale;
            }
        }

        // windows
        if !has_error("windows.") {
            self.windows = other.windows;
        } else {
            if !errs.iter().any(|e| e.starts_with("windows.show_minimized")) {
                self.windows.show_minimized = other.windows.show_minimized;
            }
            if !errs
                .iter()
                .any(|e| e.starts_with("windows.show_hidden_app_windows"))
            {
                self.windows.show_hidden_app_windows = other.windows.show_hidden_app_windows;
            }
            // A boolean cannot fail validation, so it is adopted unconditionally like the
            // other switches (same convention as the layout booleans above).
            self.windows.show_other_desktops = other.windows.show_other_desktops;
            self.windows.panel_close_enabled = other.windows.panel_close_enabled;
            self.windows.panel_minimize_enabled = other.windows.panel_minimize_enabled;
            self.windows.panel_hide_enabled = other.windows.panel_hide_enabled;
            self.windows.panel_quit_enabled = other.windows.panel_quit_enabled;
            if !errs
                .iter()
                .any(|e| e.starts_with("windows.overlay_position"))
            {
                self.windows.overlay_position = other.windows.overlay_position;
            }
            if !errs
                .iter()
                .any(|e| e.starts_with("windows.activation_mode"))
            {
                self.windows.activation_mode = other.windows.activation_mode;
            }
        }

        // logging
        if !has_error("logging.") {
            self.logging = other.logging;
        } else {
            if !errs.iter().any(|e| e.starts_with("logging.level")) {
                self.logging.level = other.logging.level;
            }
            // file_path has no validation, always valid
            self.logging.file_path = other.logging.file_path;
        }

        // key bindings
        if !has_error("key_bindings") {
            self.key_bindings = other.key_bindings;
        } else {
            // The master switch is a plain boolean, so it is always valid. The list is merged
            // entry by entry: a binding whose own path carries an error drops out of the list
            // without taking its siblings with it, which is what keeps one bad hand-written entry
            // from discarding every other binding the user has.
            self.key_bindings.enabled = other.key_bindings.enabled;
            if let Some(items) = other.key_bindings.items {
                let kept: Vec<_> = items
                    .into_iter()
                    .enumerate()
                    .filter(|(index, _)| {
                        // Only an unusable *identity* drops the entry: its action id or its trigger
                        // is what the row's behaviour is. A mistyped `enabled` fell back on its own
                        // above and must not take the whole binding with it.
                        let prefix = format!("key_bindings.items[{index}]");
                        !errs.iter().any(|error| {
                            error.starts_with(&format!("{prefix}.action"))
                                || error.starts_with(&format!("{prefix}.trigger"))
                        })
                    })
                    .map(|(_, item)| item)
                    .collect();
                // Dropping unusable entries can leave the list empty, and it stays empty: seeding
                // the factory bindings here would arm shortcuts the user never configured (they
                // may have deleted every one of them and then mistyped a line). Only a list that is
                // absent, or whose TOML type is unusable, falls back to the factory list -- that
                // happens before validation, in `seed_key_bindings`.
                self.key_bindings.items = Some(kept);
            }
        }

        // keystroke display
        if !has_error("keystroke_display.") {
            self.keystroke_display = other.keystroke_display;
        } else {
            self.keystroke_display.enabled = other.keystroke_display.enabled;
            if !errs
                .iter()
                .any(|e| e.starts_with("keystroke_display.max_extent_percent"))
            {
                self.keystroke_display.max_extent_percent =
                    other.keystroke_display.max_extent_percent;
            }
            if !errs
                .iter()
                .any(|e| e.starts_with("keystroke_display.display_position"))
            {
                self.keystroke_display.display_position = other.keystroke_display.display_position;
            }
            if !errs
                .iter()
                .any(|e| e.starts_with("keystroke_display.initial_position"))
            {
                self.keystroke_display.initial_position = other.keystroke_display.initial_position;
            }
            if !errs.iter().any(|e| e.starts_with("keystroke_display.mode")) {
                self.keystroke_display.mode = other.keystroke_display.mode;
            }
            if !errs
                .iter()
                .any(|e| e.starts_with("keystroke_display.tap_level"))
            {
                self.keystroke_display.tap_level = other.keystroke_display.tap_level;
            }
            if !errs
                .iter()
                .any(|e| e.starts_with("keystroke_display.position."))
            {
                self.keystroke_display.position = other.keystroke_display.position;
            }
        }

        // startup (bool field needs no validation, always valid)
        self.startup = other.startup;

        // updates (bool field needs no validation; Sparkle applies it at runtime).
        self.updates = other.updates;

        // finder menu: booleans are always valid; the layout and the two lists are normalized.
        self.finder_menu = other.finder_menu;
        self.normalize_finder_menu();

        // clipboard (enabled always valid; max_entries is validated)
        self.clipboard.enabled = other.clipboard.enabled;
        // show_source_app / persist / move_used_to_top / delete_after_paste /
        // show_source_app / persist / move_used_to_top / delete_after_paste /
        // clear_system_pasteboard_after_paste are bools, always valid.
        self.clipboard.show_source_app = other.clipboard.show_source_app;
        self.clipboard.clear_on_quit = other.clipboard.clear_on_quit;
        self.clipboard.separate_clear_all = other.clipboard.separate_clear_all;
        self.clipboard.move_used_to_top = other.clipboard.move_used_to_top;
        self.clipboard.delete_after_paste = other.clipboard.delete_after_paste;
        self.clipboard.clear_system_pasteboard_after_paste =
            other.clipboard.clear_system_pasteboard_after_paste;
        if !errs.iter().any(|e| e.starts_with("clipboard.max_entries")) {
            self.clipboard.max_entries = other.clipboard.max_entries;
        }
        if !errs
            .iter()
            .any(|e| e.starts_with("clipboard.auto_expire_days"))
        {
            self.clipboard.auto_expire_days = other.clipboard.auto_expire_days;
        }
        if !errs.iter().any(|e| e.starts_with("clipboard.image_rows")) {
            self.clipboard.image_rows = other.clipboard.image_rows;
        }
        if !errs
            .iter()
            .any(|e| e.starts_with("clipboard.picker_position"))
        {
            self.clipboard.picker_position = other.clipboard.picker_position.clone();
        }
        if !errs.iter().any(|e| e.starts_with("clipboard.shortcut")) {
            self.clipboard.shortcut = other.clipboard.shortcut.clone();
        }
        // pin_follow_selection and paused are bools, always valid.
        self.clipboard.pin_follow_selection = other.clipboard.pin_follow_selection;
        self.clipboard.paused = other.clipboard.paused;
        // A list of identifiers has no invalid member to fall back on: take it as written
        // (duplicates are tolerated here and collapsed when the settings page adds an entry).
        self.clipboard.excluded_apps = other.clipboard.excluded_apps.clone();

        // mouse: per-profile, per-field merge (continuing the per-field resilient pattern).
        // enabled and bool fields are always valid; each profile's fields are kept or dropped
        // based on per-field validation results.
        self.mouse.enabled = other.mouse.enabled;
        // Migrate other's legacy fields (if any) before merging.
        let mut other_mouse = other.mouse;
        other_mouse.migrate_legacy();
        self.mouse.profiles = Vec::new();
        for (i, p) in other_mouse.profiles.iter().enumerate() {
            let prefix = format!("mouse.profiles[{i}]");
            let mut merged_p = MouseProfile {
                device: p.device.clone(),
                ..Default::default()
            };
            // Bool fields are always valid.
            merged_p.reverse_scroll = p.reverse_scroll;
            if p.scroll_mode.is_some()
                && !errs
                    .iter()
                    .any(|e| e.starts_with(&format!("{prefix}.scroll_mode")))
            {
                merged_p.scroll_mode = p.scroll_mode.clone();
            }
            if p.line_count.is_some()
                && !errs
                    .iter()
                    .any(|e| e.starts_with(&format!("{prefix}.line_count")))
            {
                merged_p.line_count = p.line_count;
            }
            // Button mappings: per-entry validation; only valid entries survive.
            for (btn, desc) in &p.button_mappings {
                let ep = format!("{prefix}.button_mappings[{btn}]");
                if !errs.iter().any(|e| e.starts_with(&ep)) {
                    merged_p.button_mappings.insert(btn.clone(), desc.clone());
                }
            }
            // pointer.disable_acceleration is a bool (always valid); acceleration must pass
            // the range check.
            let accel_ok = !errs
                .iter()
                .any(|e| e.starts_with(&format!("{prefix}.pointer.acceleration")));
            merged_p.pointer = p.pointer.clone().map(|mut ptr| {
                if !accel_ok {
                    ptr.acceleration = None;
                }
                ptr
            });
            merged_p.smooth_scrolling = p.smooth_scrolling.clone().map(|mut smooth| {
                for field in ["preset", "response", "speed", "acceleration", "inertia"] {
                    if errs.iter().any(|error| {
                        error.starts_with(&format!("{prefix}.smooth_scrolling.{field}"))
                    }) {
                        match field {
                            "preset" => smooth.preset = None,
                            "response" => smooth.response = None,
                            "speed" => smooth.speed = None,
                            "acceleration" => smooth.acceleration = None,
                            "inertia" => smooth.inertia = None,
                            _ => unreachable!(),
                        }
                    }
                }
                smooth
            });
            self.mouse.profiles.push(merged_p);
        }
        // After merge, clear our own legacy fields (avoid serializing cruft).
        self.mouse.reverse_scroll = None;
        self.mouse.scroll_mode = None;
        self.mouse.line_count = None;
        self.mouse.pointer = None;
    }

    fn merge_colors(ours: &mut ThemeColors, theirs: &ThemeColors, theme: &str, errs: &[String]) {
        let p = format!("colors.{theme}");
        if !errs
            .iter()
            .any(|e| e.starts_with(&format!("{p}.status_bar_text")))
        {
            ours.status_bar_text = theirs.status_bar_text.clone();
        }
        if !errs.iter().any(|e| e.starts_with(&format!("{p}.app_name"))) {
            ours.app_name = theirs.app_name.clone();
        }
        if !errs
            .iter()
            .any(|e| e.starts_with(&format!("{p}.win_title")))
        {
            ours.win_title = theirs.win_title.clone();
        }
        if !errs
            .iter()
            .any(|e| e.starts_with(&format!("{p}.icon_inner_bg")))
        {
            ours.icon_inner_bg = theirs.icon_inner_bg.clone();
        }
        if !errs
            .iter()
            .any(|e| e.starts_with(&format!("{p}.icon_text")))
        {
            ours.icon_text = theirs.icon_text.clone();
        }
        if !errs
            .iter()
            .any(|e| e.starts_with(&format!("{p}.card_bg_sel")))
        {
            ours.card_bg_sel = theirs.card_bg_sel.clone();
        }
        if !errs
            .iter()
            .any(|e| e.starts_with(&format!("{p}.card_border_sel")))
        {
            ours.card_border_sel = theirs.card_border_sel.clone();
        }
    }
}

/// Return a TOML value containing the expected type shape for every configuration field.
///
/// `serde(default)` handles missing fields, but Serde rejects a whole struct when one field
/// has the wrong TOML type. The schema below lets the loader inspect fields independently.
/// Optional fields are populated only in the schema (not in the fallback value), so a type
/// error still falls back to `None` where that is the runtime meaning of an omitted field.
fn config_type_schema() -> toml::Value {
    let mut schema = Config::default();
    for profile in &mut schema.mouse.profiles {
        profile.device.vendor_id = Some(0);
        profile.device.product_id = Some(0);
        profile.button_mappings_enabled = Some(true);
        profile.smooth_scrolling = Some(SmoothPartial {
            enabled: Some(false),
            preset: Some("ease_in_out".to_string()),
            response: Some(0.68),
            speed: Some(1.02),
            acceleration: Some(1.10),
            inertia: Some(0.74),
        });
    }
    schema.mouse.reverse_scroll = Some(false);
    schema.mouse.scroll_mode = Some("default".to_string());
    schema.mouse.line_count = Some(3);
    schema.mouse.pointer = Some(PointerSection::default());

    let mut value = toml::to_string(&schema)
        .expect("default config must serialize")
        .parse::<toml::Value>()
        .expect("serialized default config must parse as TOML");

    // smooth_scrolling.enabled is deserialize-only: retain its legacy bool shape in the read
    // schema even though serde intentionally omits it from the serialized config defaults.
    if let Some(profile) = value
        .as_table_mut()
        .and_then(|root| root.get_mut("mouse"))
        .and_then(toml::Value::as_table_mut)
        .and_then(|mouse| mouse.get_mut("profiles"))
        .and_then(toml::Value::as_array_mut)
        .and_then(|profiles| profiles.first_mut())
        .and_then(toml::Value::as_table_mut)
        .and_then(|profile| profile.get_mut("smooth_scrolling"))
        .and_then(toml::Value::as_table_mut)
    {
        profile.insert("enabled".into(), toml::Value::Boolean(false));
    }

    // Legacy mouse fields are intentionally skipped during serialization, but they are still
    // accepted while reading old files and therefore need to be present in the type schema.
    if let Some(mouse) = value
        .as_table_mut()
        .and_then(|root| root.get_mut("mouse"))
        .and_then(toml::Value::as_table_mut)
    {
        mouse.insert("reverse_scroll".into(), toml::Value::Boolean(false));
        mouse.insert("scroll_mode".into(), toml::Value::String("default".into()));
        mouse.insert("line_count".into(), toml::Value::Integer(3));
        mouse.insert(
            "pointer".into(),
            toml::Value::Table(
                [("disable_acceleration".into(), toml::Value::Boolean(false))]
                    .into_iter()
                    .collect(),
            ),
        );
    }

    // The pre-`key_bindings` sections are deserialize-only, so their shape has to be described
    // here too: without it a wrong TOML type in a legacy file would survive sanitizing as an
    // unknown field, fail deserialization, and take the whole config down with it.
    if let Some(root) = value.as_table_mut() {
        let switches = |names: &[&str]| {
            let mut table = toml::map::Map::new();
            for name in names {
                table.insert((*name).into(), toml::Value::Boolean(false));
            }
            toml::Value::Table(table)
        };
        root.insert(
            "window_control".into(),
            switches(&[
                "enabled",
                "up",
                "down",
                "left",
                "right",
                "display_up",
                "display_down",
                "display_left",
                "display_right",
            ]),
        );
        root.insert(
            "quick_actions".into(),
            switches(&[
                "enabled",
                "open_settings",
                "open_finder",
                "show_desktop",
                "lock_screen",
                "locate_pointer",
            ]),
        );
        // `items` is an `Option` in the config (absent = "seed from the legacy sections"), so the
        // serialized defaults omit it. Describe one element here anyway: the sanitizer walks an
        // array element-by-element against the schema's first element, and without it a single
        // mistyped field inside one entry would fail the whole document's deserialization.
        if let Some(key_bindings) = root
            .get_mut("key_bindings")
            .and_then(toml::Value::as_table_mut)
        {
            let entry: toml::map::Map<String, toml::Value> = [
                ("action".to_string(), toml::Value::String(String::new())),
                ("trigger".to_string(), toml::Value::String(String::new())),
                ("enabled".to_string(), toml::Value::Boolean(true)),
            ]
            .into_iter()
            .collect();
            key_bindings.insert(
                "items".into(),
                toml::Value::Array(vec![toml::Value::Table(entry)]),
            );
            // The retired group switches are deserialize-only like the legacy sections above: a
            // file written by the interim binding build still carries them, and the sanitizer has
            // to know their type or a wrong value would fail the whole document.
            key_bindings.insert("window_control_enabled".into(), toml::Value::Boolean(false));
            key_bindings.insert("quick_actions_enabled".into(), toml::Value::Boolean(false));
        }
    }
    value
}

fn toml_value_type(value: &toml::Value) -> &'static str {
    match value {
        toml::Value::String(_) => "string",
        toml::Value::Integer(_) => "integer",
        toml::Value::Float(_) => "float",
        toml::Value::Boolean(_) => "boolean",
        toml::Value::Datetime(_) => "datetime",
        toml::Value::Array(_) => "array",
        toml::Value::Table(_) => "table",
    }
}

fn toml_path_child(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

fn toml_path_index(path: &str, index: usize) -> String {
    format!("{path}[{index}]")
}

fn type_error(path: &str, actual: &toml::Value, expected: &toml::Value) -> String {
    let field = if path.is_empty() { "<root>" } else { path };
    tf(
        "errors.config_field_type_invalid",
        &[
            ("field", field),
            ("actual", toml_value_type(actual)),
            ("expected", toml_value_type(expected)),
        ],
    )
}

/// Sanitize one TOML value against the expected type shape while preserving valid siblings.
///
/// The return value is `None` when a malformed optional field should be omitted so Serde can
/// apply its normal default. Unknown fields remain untouched and are ignored by Serde.
fn sanitize_config_value(
    actual: &toml::Value,
    fallback: Option<&toml::Value>,
    schema: &toml::Value,
    path: &str,
    errors: &mut Vec<String>,
) -> Option<toml::Value> {
    // `button_mappings` is a dynamic string map, so an empty default table cannot describe its
    // value type. Validate each value explicitly instead of adding a fake schema key.
    if path.ends_with(".button_mappings") {
        let Some(table) = actual.as_table() else {
            errors.push(type_error(path, actual, schema));
            return fallback.cloned();
        };
        let mut sanitized = table.clone();
        for (key, value) in table {
            if !value.is_str() {
                let entry_path = format!("{path}[{key}]");
                let expected = toml::Value::String(String::new());
                errors.push(type_error(&entry_path, value, &expected));
                sanitized.remove(key);
            }
        }
        return Some(toml::Value::Table(sanitized));
    }

    // The exclusion list is user data: one malformed entry (a missing or non-string bundle_id,
    // the only matching key) is dropped on its own instead of failing the whole file. The generic
    // per-field path would leave the missing key out, and the required-field decode would then
    // reject the ENTIRE config -- the opposite of the per-field tolerance invariant.
    if path == "clipboard.excluded_apps" {
        let Some(array) = actual.as_array() else {
            errors.push(type_error(path, actual, schema));
            return fallback.cloned();
        };
        let mut sanitized = Vec::with_capacity(array.len());
        for (index, item) in array.iter().enumerate() {
            let item_path = toml_path_index(path, index);
            let Some(table) = item.as_table() else {
                errors.push(type_error(&item_path, item, schema));
                continue;
            };
            let Some(bundle_id) = table.get("bundle_id").and_then(toml::Value::as_str) else {
                errors.push(format!("{item_path}: missing bundle_id"));
                continue;
            };
            let bundle_id = bundle_id.trim();
            if bundle_id.is_empty() {
                errors.push(format!("{item_path}: empty bundle_id"));
                continue;
            }
            let mut entry = toml::map::Map::new();
            entry.insert(
                "bundle_id".to_string(),
                toml::Value::String(bundle_id.to_string()),
            );
            entry.insert(
                "name".to_string(),
                toml::Value::String(
                    table
                        .get("name")
                        .and_then(toml::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                ),
            );
            sanitized.push(toml::Value::Table(entry));
        }
        return Some(toml::Value::Array(sanitized));
    }

    match (actual, schema) {
        (toml::Value::Table(actual_table), toml::Value::Table(schema_table)) => {
            let fallback_table = fallback.and_then(toml::Value::as_table);
            let mut sanitized = actual_table.clone();
            for (key, schema_child) in schema_table {
                let Some(actual_child) = actual_table.get(key) else {
                    continue;
                };
                let child_path = toml_path_child(path, key);
                let fallback_child = fallback_table.and_then(|table| table.get(key));
                match sanitize_config_value(
                    actual_child,
                    fallback_child,
                    schema_child,
                    &child_path,
                    errors,
                ) {
                    Some(value) => {
                        sanitized.insert(key.clone(), value);
                    }
                    None => {
                        sanitized.remove(key);
                    }
                }
            }
            Some(toml::Value::Table(sanitized))
        }
        (toml::Value::Array(actual_array), toml::Value::Array(schema_array)) => {
            let Some(schema_item) = schema_array.first() else {
                return Some(toml::Value::Array(actual_array.clone()));
            };
            let fallback_array = fallback.and_then(toml::Value::as_array);
            let mut sanitized = Vec::with_capacity(actual_array.len());
            for (index, actual_item) in actual_array.iter().enumerate() {
                let item_path = toml_path_index(path, index);
                let item_fallback = if path == "key_bindings.items" {
                    // Every binding entry stands on its own. A mistyped field inside one entry must
                    // fall back to that field's own default — an empty action id or trigger, which
                    // validation then rejects and drops — and never inherit the first seeded
                    // entry's action or chord.
                    None
                } else if path == "mouse.profiles" && index > 0 {
                    // Only the first profile is the explicit wildcard default. A malformed
                    // optional field in a per-device profile must fall back to omission, not
                    // accidentally inherit the wildcard profile's concrete value.
                    None
                } else {
                    fallback_array.and_then(|items| items.first())
                };
                match sanitize_config_value(
                    actual_item,
                    item_fallback,
                    schema_item,
                    &item_path,
                    errors,
                ) {
                    Some(value) => sanitized.push(value),
                    None if path == "key_bindings.items" => {
                        // Keep the slot. The errors already carry this element's *original* index
                        // and the validation that follows reports an unusable entry for it, so
                        // dropping the element here would renumber every later binding and let the
                        // identity filter delete the wrong one.
                        sanitized.push(schema_item.clone());
                    }
                    None => {}
                }
            }
            Some(toml::Value::Array(sanitized))
        }
        (toml::Value::String(_), toml::Value::String(_))
        | (toml::Value::Integer(_), toml::Value::Integer(_))
        | (toml::Value::Boolean(_), toml::Value::Boolean(_))
        | (toml::Value::Datetime(_), toml::Value::Datetime(_))
        | (toml::Value::Float(_), toml::Value::Float(_))
        // TOML integers are accepted by Serde when loading an f64 field.
        | (toml::Value::Integer(_), toml::Value::Float(_)) => Some(actual.clone()),
        _ => {
            errors.push(type_error(path, actual, schema));
            fallback.cloned()
        }
    }
}

/// Parse and sanitize one config file. The boolean says whether the repaired/migrated value
/// should be persisted back to disk; TOML syntax or whole-document deserialization failures
/// return Err so the original file remains available for diagnosis.
///
/// This is the loader the running app uses, and the settings import deliberately reuses it
/// instead of deserializing the export on its own: field-by-field fallback, the migration hooks
/// and the legacy-section seeding are exactly the compatibility the import needs (see
/// `crate::settings::portability`).
pub(crate) fn parse_config_content(
    content: &str,
) -> Result<(Config, Vec<String>, bool), Vec<String>> {
    let actual = match content.parse::<toml::Value>() {
        Ok(value) => value,
        Err(error) => {
            return Err(vec![tf(
                "errors.config_read_failed",
                &[("error", &error.to_string())],
            )]);
        }
    };
    let defaults = toml::to_string(&Config::default())
        .expect("default config must serialize")
        .parse::<toml::Value>()
        .expect("serialized default config must parse as TOML");
    let schema = config_type_schema();
    let mut errors = Vec::new();
    let sanitized = sanitize_config_value(&actual, Some(&defaults), &schema, "", &mut errors)
        .unwrap_or(defaults.clone());
    let mut loaded: Config = match sanitized.try_into() {
        Ok(config) => config,
        Err(error) => {
            // This is a defensive guard for a schema mismatch introduced by a future field.
            // The normal path above should make every field independently deserializable.
            errors.push(tf(
                "errors.config_read_failed",
                &[("error", &error.to_string())],
            ));
            return Err(errors);
        }
    };

    let mut needs_persist = !errors.is_empty();
    needs_persist |= loaded.mouse.migrate_legacy();
    needs_persist |= loaded.migrate_card_text_style();
    // Presence is read off the raw document: Serde cannot tell a missing table (or a missing
    // `items` key) from one whose fields all took their defaults, and the difference decides what
    // may be taken over from the legacy sections. The two retired group switches are still accepted
    // here so an interim file migrates into the master switch instead of losing its state.
    loaded.seed_key_bindings(actual.get("key_bindings"));
    // `[finder_menu]`'s layout and its two id lists are semantics, not types: normalizing them on
    // every load keeps the runtime, the settings controls and the persisted file in agreement even
    // when nothing else failed validation (merge_valid only runs on an error).
    loaded.normalize_finder_menu();
    errors.extend(loaded.validate());
    let mut config = if !errors.is_empty() {
        // Any validation error means the repaired snapshot on disk differs from what was loaded --
        // including a purely semantic one (an unknown binding action) that the type sanitizer above
        // cannot see. It has to be written back, or the same repair is repeated on every launch.
        needs_persist = true;
        let mut merged = Config::default();
        merged.merge_valid(loaded, &errors);
        merged
    } else {
        loaded
    };
    needs_persist |= config.migrate_footer_title_switch();
    // Reserved chords are only known once every other field has settled: the clipboard yields
    // first (an invalid value has fallen back to its default by now), then the bindings yield to
    // whatever the switcher and the clipboard finally own. Runs on every load, so a hand-edited
    // file is repaired rather than half-honoured.
    let conflicts = config.settle_reserved_shortcuts();
    if !conflicts.is_empty() {
        errors.extend(conflicts);
        needs_persist = true;
    }
    Ok((config, errors, needs_persist))
}

fn config_path() -> std::path::PathBuf {
    // Unit tests must never touch the developer's real config: loading repairs invalid fields and
    // persists the result, so a test binary that does not know a value (another branch, an older
    // checkout) would silently rewrite it. Each test process gets its own temporary home.
    if cfg!(test) {
        return config_path_in(&test_config_home().to_string_lossy());
    }
    config_path_in(&std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()))
}

fn test_config_home() -> std::path::PathBuf {
    static HOME: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let home = std::env::temp_dir().join(format!("oh-my-tab-test-home-{}", std::process::id()));
        // PIDs are reused and $TMPDIR outlives a run: start every test process from a clean home
        // rather than from whatever an earlier process with the same PID saved.
        let _ = std::fs::remove_dir_all(&home);
        home
    })
    .clone()
}

/// Whether the config file already exists. The guide uses it to tell a fresh install from an
/// upgrade (which must not be nagged).
pub(crate) fn config_file_exists() -> bool {
    config_path().exists()
}

/// Compute the config path under a given home (pure; tests inject a temp dir).
fn config_path_in(home: &str) -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(home).join(".config/oh-my-tab");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("config.toml")
}

/// Parse hex string like "999999ff" → u32 0x999999ff.
pub fn parse_hex8(s: &str) -> u32 {
    u32::from_str_radix(s, 16).unwrap_or(0)
}

static ATOMIC_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write through a same-directory temp file, fsync, and atomically rename it into place so a
/// reader never observes a partial file and stale writers cannot truncate a newer file.
///
/// Shared with the settings export: overwriting an existing backup through a plain write would
/// truncate it first, so a failed write would destroy the very file it was meant to replace.
pub(crate) fn atomic_write(path: &std::path::Path, contents: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("config path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("create config directory {}: {}", parent.display(), e))?;

    let serial = ATOMIC_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(
        ".{}.tmp-{}-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("config"),
        std::process::id(),
        serial
    ));
    let result = (|| {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|e| format!("create {}: {}", temp.display(), e))?;
        file.write_all(contents)
            .map_err(|e| format!("write {}: {}", temp.display(), e))?;
        file.sync_all()
            .map_err(|e| format!("sync {}: {}", temp.display(), e))?;
        drop(file);
        std::fs::rename(&temp, path).map_err(|e| format!("replace {}: {}", path.display(), e))?;
        // Best effort directory sync: the rename is durable on filesystems that support it.
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

impl Config {
    /// Serialize to a given path (pure logic; tests inject a temp dir).
    pub(crate) fn save_to(&self, path: &std::path::Path) -> Result<(), String> {
        let toml_str =
            toml::to_string_pretty(self).map_err(|e| format!("serialize config: {}", e))?;
        atomic_write(path, toml_str.as_bytes())
    }

    pub fn load_or_default() -> (Self, Vec<String>) {
        let path = config_path();
        Self::load_or_default_from(&path)
    }

    /// Fold the retired `layout.show_footer_title` switch into the size that replaced it.
    ///
    /// The switch is gone from the UI and the band now follows `fonts.status_bar_size` (0 = no band),
    /// so a config saved with the switch *off* and a positive size would silently get its footer back.
    /// The switch is then re-armed to `true` so this runs exactly once and never overrides a size the
    /// user picks later.
    ///
    /// Runs after per-field fallback (`merge_valid`), not on the pre-validation snapshot: a
    /// *typed-invalid* size like `"bad"` is recorded as a field error and falls back to the default
    /// in the merged config, and the 0 written here must still win over that default. Running it on
    /// `loaded` instead would re-arm the switch and then let `merge_valid` restore the 15pt default.
    pub(crate) fn migrate_footer_title_switch(&mut self) -> bool {
        if self.layout.show_footer_title {
            return false;
        }
        self.layout.show_footer_title = true;
        // Unconditionally 0: this runs after fallback, so an invalid size has already become the
        // 15pt default -- and the hidden footer must still win.
        self.fonts.status_bar_size = 0.0;
        true
    }

    /// Migrate exact legacy defaults only. The old overlay medium weight (0.23) is no longer
    /// part of the built-in typography ladder; customized weight values are preserved. The
    /// previous title/app-name size migration remains gated on the complete old default combo.
    /// Legacy theme colors still migrate only on an exact old-pair match.
    /// Returns whether anything changed.
    pub(crate) fn migrate_card_text_style(&mut self) -> bool {
        let mut changed = false;
        // Old font defaults: title_size 11 / weight 0.23, app_name_size 13 / weight 0.5.
        let f = &mut self.fonts;
        let legacy_title_weight = f.title_weight == 0.23;
        if f.title_size == 11.0
            && legacy_title_weight
            && f.app_name_size == 13.0
            && f.app_name_weight == 0.5
        {
            f.title_size = 12.0;
            f.app_name_size = 10.0; // app name 10px regular
            f.app_name_weight = crate::theme::FONT_WEIGHT_REGULAR;
            changed = true;
        }
        if legacy_title_weight {
            f.title_weight = crate::theme::FONT_WEIGHT_SEMIBOLD;
            changed = true;
        }
        if f.status_bar_weight == 0.23 {
            f.status_bar_weight = crate::theme::FONT_WEIGHT_SEMIBOLD;
            changed = true;
        }
        // Colors migrate per theme section: each section's pair independently and
        // exactly matches the old dark (dddddd/888888) or old light (1a1a1a/333333)
        // defaults before being rewritten to its new same-family pair (D1/57 alpha).
        for theme in [&mut self.colors.dark, &mut self.colors.light] {
            let pair = (theme.app_name.as_str(), theme.win_title.as_str());
            match pair {
                ("ddddddff", "888888ff") => {
                    theme.app_name = "FFFFFF57".into();
                    theme.win_title = "FFFFFFD1".into();
                    changed = true;
                }
                ("1a1a1aff", "333333ff") => {
                    theme.app_name = "00000057".into();
                    theme.win_title = "000000D1".into();
                    changed = true;
                }
                _ => {}
            }
        }
        changed
    }

    /// Load from a given path (pure logic; tests inject a temp dir).
    fn load_or_default_from(path: &std::path::Path) -> (Self, Vec<String>) {
        Self::load_or_default_from_result(path, std::fs::read_to_string(path))
    }

    fn load_or_default_from_result(
        path: &std::path::Path,
        read_result: std::io::Result<String>,
    ) -> (Self, Vec<String>) {
        match read_result {
            Ok(content) => match parse_config_content(&content) {
                Ok((loaded, errs, needs_persist)) => {
                    if needs_persist {
                        let _ = loaded.save_to(path);
                    }
                    (loaded, errs)
                }
                Err(errs) => (Config::default(), errs),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // Write defaults only when the file is genuinely missing; preserve it on all
                // other read errors.
                let defaults = Config::default();
                let _ = defaults.save_to(path);
                (defaults, Vec::new())
            }
            Err(error) => (
                Config::default(),
                vec![tf(
                    "errors.config_read_failed",
                    &[("error", &error.to_string())],
                )],
            ),
        }
    }

    pub fn reload() -> Result<(Self, Vec<String>, bool), Vec<String>> {
        let path = config_path();
        Self::reload_from(&path)
    }

    fn reload_from(path: &std::path::Path) -> Result<(Self, Vec<String>, bool), Vec<String>> {
        let content = std::fs::read_to_string(path).map_err(|error| {
            vec![tf(
                "errors.config_read_failed",
                &[("error", &error.to_string())],
            )]
        })?;
        parse_config_content(&content)
    }
}

/// What the first load of the config file had to say about it. The load happens once, in `CONFIG`'s
/// initializer, and `run()` cannot repeat it: by then the value is the already-repaired config, so
/// re-validating it reports nothing even when the file was unparseable and every field fell back to
/// its default. Keeping the loader's own errors is what lets the startup report name the real problem
/// (measured: a syntax error in the file produced an empty report and the app silently ran on
/// defaults).
static CONFIG_LOAD_ERRORS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();

/// The errors the config loader reported when `CONFIG` initialised (empty for a healthy file or a
/// first run). Safe to call from any thread after `CONFIG` has been touched.
pub fn config_load_errors() -> Vec<String> {
    CONFIG_LOAD_ERRORS.get().cloned().unwrap_or_else(|| {
        // Force the one-time load so the answer does not depend on who asked first.
        let _ = &*CONFIG;
        CONFIG_LOAD_ERRORS.get().cloned().unwrap_or_default()
    })
}

pub static CONFIG: std::sync::LazyLock<RwLock<Config>> = std::sync::LazyLock::new(|| {
    let (cfg, errs) = Config::load_or_default();
    let _ = CONFIG_LOAD_ERRORS.set(errs);
    // Apply the locale from config (I18N init only used the system locale).
    // No cycle: I18N does not read CONFIG; see the note at the top of i18n.rs.
    i18n::apply_config_locale(&cfg.i18n.locale);
    RwLock::new(cfg)
});

/// Return the effective glass style.
pub fn effective_glass_style() -> String {
    // Development channel: `--glass-style=regular|clear` A/Bs the material for one launch. An
    // unrecognised value is ignored rather than rendered as nothing, like `--panel-material`.
    if let Some(value) = crate::dev_flags::value("glass-style") {
        if value == "regular" || value == "clear" {
            return value;
        }
    }
    CONFIG.read().unwrap().appearance.glass_style.clone()
}

/// Return the effective floating-panel material. A valid `--panel-material=<id>` launch
/// switch overrides the config for that launch (the dev-verification channel); an invalid
/// one is ignored so a typo can never break the panels.
pub fn effective_panel_material() -> String {
    // The launch switch is the only channel that can name a development-only material (`backdrop`), so it
    // resolves through the enum's own mapping; `PANEL_MATERIAL_VALUES` stays the user-facing set that the
    // config and the settings writer are held to.
    if let Some(value) = crate::dev_flags::value("panel-material") {
        if crate::glass::PanelMaterial::from_config_value(&value).is_some() {
            return value;
        }
    }
    CONFIG.read().unwrap().appearance.panel_material.clone()
}

/// The material-strength knobs, each with a `--…` launch switch that overrides it for one launch.
fn numeric_flag_or(name: &str, fallback: f64) -> f64 {
    crate::dev_flags::value(name)
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite())
        .unwrap_or(fallback)
}

/// How strongly the glass renders (the glass view's own alpha). Development channel only: the shipped
/// look renders at full alpha, exactly as the two looks this app shipped with did.
pub fn effective_glass_opacity() -> f64 {
    numeric_flag_or("glass-opacity", 1.0).clamp(0.0, 1.0)
}

/// Backdrop blur radius for the owned underlay (development-only, see `glass::apply_blur_underlay`).
pub fn effective_glass_blur_radius() -> f64 {
    numeric_flag_or("glass-blur", 0.0).clamp(0.0, 64.0)
}

/// Backdrop saturation for the owned underlay (development-only, same note).
pub fn effective_glass_saturation() -> f64 {
    numeric_flag_or("glass-saturation", 1.0).clamp(0.0, 4.0)
}

/// The frosted material's light-mode brightness floor, resolved: the shipped
/// [`crate::glass::FROST_FLOOR_ALPHA`], or the `--glass-floor` launch override while the value is being
/// tuned on a dark backdrop. Frost only -- Liquid Glass and `opaque` never read this.
pub fn effective_glass_floor() -> f64 {
    numeric_flag_or("glass-floor", crate::glass::FROST_FLOOR_ALPHA).clamp(0.0, 1.0)
}

/// `NSGlassEffectView`'s private `_variant`, from the `--glass-variant` launch switch only.
///
/// Deliberately not read from the config: the variant is not a user choice (two attempts at labelling
/// it from a brightness sample were wrong -- variant 19 measures the *most* transparent and renders
/// broken rainbow edges), and a value left over from a removed settings row would otherwise keep every
/// panel in that state with no way to undo it from the UI.
pub fn effective_glass_variant() -> i64 {
    crate::dev_flags::value("glass-variant")
        .and_then(|value| value.trim().parse::<i64>().ok())
        .unwrap_or(0)
        .clamp(0, 19)
}

/// Return whether focused-window thumbnail prewarming is enabled.
pub(crate) fn focused_thumbnail_prewarm_enabled() -> bool {
    CONFIG.read().unwrap().layout.focused_thumbnail_prewarm
}

/// Persistence debounce window: bursts of changes (slider drags, fast typing) coalesce into
/// one disk write after this quiet period.
const PERSIST_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(400);

enum PersistMsg {
    Schedule {
        snapshot: Config,
        revision: u64,
    },
    Flush {
        snapshot: Config,
        revision: u64,
        ack: Option<mpsc::Sender<Result<(), String>>>,
    },
}

static CONFIG_REVISION: AtomicU64 = AtomicU64::new(0);

/// The background persistence thread's channel (lazy; control callbacks and restores send).
static PERSIST_TX: LazySender = LazySender::new();

/// A lazy wrapper for the mpsc Sender (the thread spawns with the channel).
struct LazySender(std::sync::Mutex<Option<std::sync::mpsc::Sender<PersistMsg>>>);

impl LazySender {
    const fn new() -> Self {
        Self(std::sync::Mutex::new(None))
    }

    fn send(&self, msg: PersistMsg) -> Result<(), String> {
        let mut guard = self.0.lock().unwrap();
        if guard.is_none() {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::Builder::new()
                .name("config-persist".into())
                .spawn(move || persist_thread(rx))
                .expect("spawn config-persist thread");
            *guard = Some(tx);
        }
        // A send failure only means the thread exited (receiver dropped); dropping the
        // message is safer than panicking.
        guard
            .as_ref()
            .unwrap()
            .send(msg)
            .map_err(|_| "config persistence writer stopped".to_string())
    }
}

/// The persistence thread: Schedule opens a debounce window (write once after 400ms of
/// quiet); Flush writes immediately. It always snapshots the newest CONFIG, so bursts
/// naturally coalesce into a single write.
fn persist_thread(rx: std::sync::mpsc::Receiver<PersistMsg>) {
    let mut persisted_revision = 0;
    while let Ok(msg) = rx.recv() {
        match msg {
            PersistMsg::Flush {
                snapshot,
                revision,
                ack,
            } => {
                let result = persist_if_newer(snapshot, revision, &mut persisted_revision);
                if let Some(ack) = ack {
                    let _ = ack.send(result);
                }
            }
            PersistMsg::Schedule { snapshot, revision } => {
                let mut latest = (snapshot, revision);
                loop {
                    match rx.recv_timeout(PERSIST_DEBOUNCE) {
                        Ok(PersistMsg::Schedule { snapshot, revision }) => {
                            if revision > latest.1 {
                                latest = (snapshot, revision);
                            }
                        }
                        Ok(PersistMsg::Flush {
                            snapshot,
                            revision,
                            ack,
                        }) => {
                            if revision > latest.1 {
                                latest = (snapshot, revision);
                            }
                            let result = persist_if_newer(
                                latest.0.clone(),
                                latest.1,
                                &mut persisted_revision,
                            );
                            if let Some(ack) = ack {
                                let _ = ack.send(result);
                            }
                            break;
                        }
                        // Timeout = quiet period elapsed; write once.
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            let _ = persist_if_newer(
                                latest.0.clone(),
                                latest.1,
                                &mut persisted_revision,
                            );
                            break;
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
            }
        }
    }
}

fn persist_if_newer(
    snapshot: Config,
    revision: u64,
    persisted_revision: &mut u64,
) -> Result<(), String> {
    if revision <= *persisted_revision {
        return Ok(());
    }
    let result = snapshot.save_to(&config_path());
    if result.is_ok() {
        *persisted_revision = revision;
    }
    result
}

fn snapshot_with_revision() -> (Config, u64) {
    let snapshot = CONFIG.read().unwrap().clone();
    let revision = CONFIG_REVISION.fetch_add(1, Ordering::SeqCst) + 1;
    (snapshot, revision)
}

/// Call after a setting change: schedule one debounced disk write (immediate effect and disk
/// persistence are decoupled to avoid burst IO while dragging/typing).
pub fn schedule_config_persist() {
    let (snapshot, revision) = snapshot_with_revision();
    if let Err(e) = PERSIST_TX.send(PersistMsg::Schedule { snapshot, revision }) {
        eprintln!("[config] persist schedule failed: {}", e);
    }
}

/// Queue an immediate write request for the writer (restore-default / blur-commit paths).
pub fn persist_config_now() {
    let (snapshot, revision) = snapshot_with_revision();
    if let Err(e) = PERSIST_TX.send(PersistMsg::Flush {
        snapshot,
        revision,
        ack: None,
    }) {
        eprintln!("[config] persist request failed: {}", e);
    }
}

/// Install `candidate` only when its write actually succeeded.
///
/// Split out from [`commit_config`] so the ordering rule is assertable without a disk: a failed
/// write must leave the running configuration exactly as it was, or the app reports an import that
/// silently reverts on the next launch.
pub(crate) fn publish_after_write(
    current: &mut Config,
    candidate: Config,
    write: Result<(), String>,
) -> Result<(), String> {
    write?;
    *current = candidate;
    Ok(())
}

/// Write and publish a whole candidate configuration as one transaction.
///
/// Order matters and is the whole point of this entry point: the candidate goes to disk through
/// the same monotonic-revision writer every other change uses, and only a durable write publishes
/// it to `CONFIG`.
///
/// The synchronous wait exists because the caller reports the outcome to the user; every other
/// path stays fire-and-forget on the background thread.
pub fn commit_config(candidate: Config) -> Result<(), String> {
    let revision = CONFIG_REVISION.fetch_add(1, Ordering::SeqCst) + 1;
    let (ack_tx, ack_rx) = mpsc::channel();
    PERSIST_TX.send(PersistMsg::Flush {
        snapshot: candidate.clone(),
        revision,
        ack: Some(ack_tx),
    })?;
    let write = ack_rx
        .recv()
        .map_err(|_| "config persistence writer stopped".to_string())?;
    let mut current = CONFIG
        .write()
        .map_err(|_| "config lock poisoned".to_string())?;
    publish_after_write(&mut current, candidate, write)
}

/// Synchronously wait until the newest configuration snapshot is durable, used before quitting.
pub fn flush_config_sync() -> Result<(), String> {
    let (snapshot, revision) = snapshot_with_revision();
    let (ack_tx, ack_rx) = mpsc::channel();
    PERSIST_TX.send(PersistMsg::Flush {
        snapshot,
        revision,
        ack: Some(ack_tx),
    })?;
    ack_rx
        .recv()
        .map_err(|_| "config persistence writer stopped".to_string())?
}

/// Reload config from disk and apply. Returns validation errors (empty = success).
pub fn reload_config() -> Vec<String> {
    let (new_cfg, errs, needs_persist) = match Config::reload() {
        Ok(result) => result,
        Err(errs) => return errs,
    };
    let old_cfg = CONFIG.read().unwrap().clone();
    if let Ok(mut cfg) = CONFIG.write() {
        *cfg = new_cfg.clone();
    }
    crate::runtime_config::apply_config_change(
        &old_cfg,
        &new_cfg,
        crate::runtime_config::ConfigChangeSource::Reload,
    );
    if needs_persist {
        // Reload callbacks never write synchronously; repaired/migrated snapshots go through
        // the single writer.
        persist_config_now();
    }
    errs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exclusion list's defaulting rule has three distinct cases and they must stay distinct:
    /// a config written before the field existed gets the built-in list, an explicitly emptied list
    /// stays empty (the user removed every entry), and an explicit list is taken as written.
    #[test]
    fn clipboard_excluded_apps_default_and_explicit_forms_stay_distinct() {
        let defaults = Config::default();
        assert!(!defaults.clipboard.paused);
        assert!(defaults
            .clipboard
            .excluded_apps
            .iter()
            .any(|entry| entry.bundle_id == "com.apple.Passwords"));
        assert!(defaults
            .clipboard
            .excluded_apps
            .iter()
            .any(|entry| entry.bundle_id == "com.apple.keychainaccess"));

        let legacy: Config = toml::from_str("[clipboard]\nenabled = true\n").unwrap();
        assert_eq!(legacy.clipboard.excluded_apps, default_excluded_apps());
        assert!(!legacy.clipboard.paused);

        let emptied: Config = toml::from_str("[clipboard]\nexcluded_apps = []\n").unwrap();
        assert!(emptied.clipboard.excluded_apps.is_empty());

        let custom: Config = toml::from_str(
            "[[clipboard.excluded_apps]]\nbundle_id = \"com.example.one\"\nname = \"One\"\n",
        )
        .unwrap();
        assert_eq!(custom.clipboard.excluded_apps.len(), 1);
        assert_eq!(
            custom.clipboard.excluded_apps[0].bundle_id,
            "com.example.one"
        );
        assert_eq!(custom.clipboard.excluded_apps[0].name, "One");
    }

    /// One malformed exclusion entry must be dropped on its own: the list is user data, and failing
    /// the whole file would discard every other valid setting.
    #[test]
    fn a_malformed_excluded_app_entry_does_not_reject_the_rest_of_the_config() {
        let content = "[clipboard]\nenabled = true\n\n[[clipboard.excluded_apps]]\nname = \"Missing identifier\"\n\n[[clipboard.excluded_apps]]\nbundle_id = \"com.example.good\"\nname = \"Good\"\n";
        let (config, errors, _needs_persist) =
            parse_config_content(content).expect("the rest of the file must still load");
        assert!(config.clipboard.enabled);
        assert_eq!(
            config.clipboard.excluded_apps,
            vec![ExcludedApp::new("com.example.good", "Good")]
        );
        assert!(
            !errors.is_empty(),
            "the dropped entry must still be reported"
        );
    }

    #[test]
    fn merge_valid_takes_paused_and_the_excluded_list_verbatim() {
        let mut ours = Config::default();
        let mut other = Config::default();
        other.clipboard.paused = true;
        other.clipboard.excluded_apps = vec![ExcludedApp::new("com.example.pm", "PM")];
        ours.merge_valid(other, &[]);
        assert!(ours.clipboard.paused);
        assert_eq!(
            ours.clipboard.excluded_apps,
            vec![ExcludedApp::new("com.example.pm", "PM")]
        );
    }

    #[test]
    fn parse_hex8_parses_valid_rgb_alpha() {
        assert_eq!(parse_hex8("999999ff"), 0x999999ff);
        assert_eq!(parse_hex8("00000000"), 0x00000000);
        assert_eq!(parse_hex8("FFFFFFFF"), 0xffffffff);
        assert_eq!(parse_hex8("12345678"), 0x12345678);
    }

    #[test]
    fn parse_hex8_invalid_inputs_fall_back_to_zero() {
        // Invalid/empty/overlong inputs all fall back to 0, never panic.
        assert_eq!(parse_hex8(""), 0);
        assert_eq!(parse_hex8("xyz"), 0);
        assert_eq!(parse_hex8("999999999"), 0); // 9 chars overflows u32
        assert_eq!(parse_hex8("gggggggg"), 0);
    }

    #[test]
    fn thumbnail_size_values_round_trip_and_pin_scales() {
        // The dropdown index and the stored value must round-trip, or the page shows one size
        // while persisting another.
        for (index, value) in THUMBNAIL_SIZE_VALUES.iter().enumerate() {
            assert_eq!(thumbnail_size_index_of(value), Some(index));
            assert_eq!(thumbnail_size_value_at(index), *value);
        }
        assert_eq!(thumbnail_size_index_of("nonsense"), None);
        assert_eq!(thumbnail_size_value_at(99), "auto");
        // "auto" pins nothing; every other value pins its percent.
        assert_eq!(thumbnail_scale_of("auto"), None);
        assert_eq!(thumbnail_scale_of("120"), Some(1.2));
        assert_eq!(thumbnail_scale_of("75"), Some(0.75));
        assert_eq!(thumbnail_scale_of("100"), Some(1.0));
        assert_eq!(thumbnail_scale_of("nonsense"), None);
    }

    #[test]
    fn an_unknown_thumbnail_size_is_rejected_per_field() {
        let mut cfg = Config::default();
        assert_eq!(cfg.layout.thumbnail_size, "auto");
        cfg.layout.thumbnail_size = "1.2".into(); // the scale, not the stored percent
        assert_err_count(&cfg, 1);
    }

    fn assert_err_count(cfg: &Config, expected: usize) {
        let errs = cfg.validate();
        assert_eq!(errs.len(), expected, "errors: {:?}", errs);
    }

    #[test]
    fn defaults_validate_clean() {
        let cfg = Config::default();
        assert_eq!(cfg.clipboard.shortcut, "option+v");
        assert_eq!(cfg.fonts.title_weight, crate::theme::FONT_WEIGHT_SEMIBOLD);
        assert_eq!(
            cfg.fonts.status_bar_weight,
            crate::theme::FONT_WEIGHT_SEMIBOLD
        );
        assert_err_count(&cfg, 0);
        assert!(!cfg.windows.show_minimized);
        assert!(!cfg.windows.show_hidden_app_windows);
        // The in-overlay action keys ship OFF: they act on other apps (hide/quit especially), so
        // opting in is the honest default.
        assert!(!cfg.windows.panel_close_enabled);
        assert!(!cfg.windows.panel_minimize_enabled);
        assert!(!cfg.windows.panel_hide_enabled);
        assert!(!cfg.windows.panel_quit_enabled);
        assert!(cfg.windows.show_other_desktops);
    }

    #[test]
    fn old_clipboard_config_without_shortcut_keeps_the_option_v_default() {
        let cfg: Config = toml::from_str("[clipboard]\nenabled = true\n").unwrap();
        assert!(cfg.clipboard.enabled);
        assert_eq!(cfg.clipboard.shortcut, "option+v");
    }

    #[test]
    fn window_control_direction_defaults_preserve_existing_configs() {
        let cfg: Config = toml::from_str("[window_control]\nenabled = true\n").unwrap();
        assert!(cfg.window_control.enabled);
        assert!(cfg.window_control.up);
        assert!(cfg.window_control.down);
        assert!(cfg.window_control.left);
        assert!(cfg.window_control.right);
        assert!(cfg.window_control.display_up);
        assert!(cfg.window_control.display_down);
        assert!(cfg.window_control.display_left);
        assert!(cfg.window_control.display_right);
    }

    #[test]
    fn validate_catches_appearance_theme() {
        let mut cfg = Config::default();
        cfg.appearance.theme = "neon".into();
        assert_err_count(&cfg, 1);
    }

    #[test]
    fn validate_catches_bad_hex_colors() {
        let mut cfg = Config::default();
        cfg.colors.dark.app_name = "nothex".into();
        cfg.colors.light.card_bg_sel = "12345".into();
        assert_err_count(&cfg, 2);
    }

    /// `layout.show_footer_title` is additive, so the two compatibility directions matter: a config
    /// written before the switch existed must load as "title shown" (not hidden), and an explicit
    /// `false` must survive a round trip and a sibling field's per-field fallback. Getting the first
    /// one wrong hides the footer for every existing user on upgrade.
    #[test]
    fn the_footer_title_switch_defaults_on_and_round_trips() {
        // An old file has no key for it.
        let old: Config = toml::from_str("[layout]\ncard_text_size = 15.0\n").unwrap();
        assert!(
            old.layout.show_footer_title,
            "an absent key must mean shown"
        );

        // Explicitly off survives serialization.
        let mut cfg = Config::default();
        cfg.layout.show_footer_title = false;
        let text = toml::to_string(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert!(!back.layout.show_footer_title);

        // A sibling field's fallback must not discard it: booleans are adopted unconditionally.
        let mut other = Config::default();
        other.layout.show_footer_title = false;
        let mut merged = Config::default();
        merged.merge_valid(other, &["layout.card_text_size: out of range".to_string()]);
        assert!(
            !merged.layout.show_footer_title,
            "a per-field fallback elsewhere in [layout] must not reset this switch"
        );
    }

    #[test]
    fn retired_icon_layout_fields_are_ignored() {
        // Retired experimental layout fields are ignored when loading and do not cause errors.
        let cfg: Config = toml::from_str(
            "[layout]\ncards_per_row = 10\ncard_width = 300.0\ncard_height = 400.0\ncard_gap = 8.0\nicon_size = 200.0\n",
        )
        .unwrap();
        assert!(cfg.validate().is_empty());
        assert!(cfg.layout.thumbnails_enabled);
    }

    #[test]
    fn validate_catches_keyboard_and_locale() {
        let mut cfg = Config::default();
        cfg.keyboard.modifier = "hyper".into();
        cfg.i18n.locale = "fr".into();
        cfg.logging.level = "verbose".into();
        cfg.windows.overlay_position = "nowhere".into();
        cfg.windows.activation_mode = "double_click".into();
        assert_err_count(&cfg, 5);
    }

    #[test]
    fn validate_rejects_card_text_size_outside_range() {
        let mut cfg = Config::default();
        cfg.layout.card_text_size = 12.9;
        assert_err_count(&cfg, 1);
        cfg.layout.card_text_size = 20.1;
        assert_err_count(&cfg, 1);
        cfg.layout.card_text_size = 13.0;
        assert_err_count(&cfg, 0);
    }

    #[test]
    fn the_retired_footer_switch_folds_into_its_size_once() {
        let mut cfg = Config::default();
        cfg.layout.show_footer_title = false;
        cfg.fonts.status_bar_size = 15.0;
        assert!(cfg.migrate_footer_title_switch());
        assert_eq!(cfg.fonts.status_bar_size, 0.0);
        // Re-armed: a second load must not clobber a size the user chose after upgrading.
        cfg.fonts.status_bar_size = 14.0;
        assert!(!cfg.migrate_footer_title_switch());
        assert_eq!(cfg.fonts.status_bar_size, 14.0);
    }

    #[test]
    fn the_footer_switch_migrates_through_a_typed_invalid_size() {
        let (config, errors, needs_persist) = parse_config_content(
            "[layout]\nshow_footer_title = false\n[fonts]\nstatus_bar_size = \"bad\"\n",
        )
        .unwrap();
        assert_eq!(config.fonts.status_bar_size, 0.0);
        assert!(config.layout.show_footer_title);
        assert!(needs_persist);
        assert!(errors
            .iter()
            .any(|error| error.starts_with("fonts.status_bar_size")));
    }

    #[test]
    fn the_footer_switch_migrates_through_a_negative_size() {
        let (config, _, needs_persist) = parse_config_content(
            "[layout]\nshow_footer_title = false\n[fonts]\nstatus_bar_size = -1.0\n",
        )
        .unwrap();
        assert_eq!(config.fonts.status_bar_size, 0.0);
        assert!(config.layout.show_footer_title);
        assert!(needs_persist);
    }

    #[test]
    fn a_round_trip_of_the_migrated_config_keeps_the_footer_hidden() {
        let (config, _, _) = parse_config_content(
            "[layout]\nshow_footer_title = false\n[fonts]\nstatus_bar_size = \"bad\"\n",
        )
        .unwrap();
        let serialized = toml::to_string(&config).unwrap();
        let (roundtripped, _, needs_persist) = parse_config_content(&serialized).unwrap();
        assert_eq!(roundtripped.fonts.status_bar_size, 0.0);
        assert!(!needs_persist);
    }

    #[test]
    fn validate_rejects_status_bar_text_size_outside_range() {
        let mut cfg = Config::default();
        // 0 is legal: the slider's far-left stop means "draw no footer" (the switch this replaced).
        cfg.fonts.status_bar_size = 0.0;
        assert_err_count(&cfg, 0);
        cfg.fonts.status_bar_size = -0.1;
        assert_err_count(&cfg, 1);
        cfg.fonts.status_bar_size = 20.1;
        assert_err_count(&cfg, 1);
        cfg.fonts.status_bar_size = 13.0;
        assert_err_count(&cfg, 0);
    }

    #[test]
    fn switcher_text_defaults_to_fifteen_points() {
        let cfg = Config::default();
        assert_eq!(cfg.layout.card_text_size, 15.0);
        assert_eq!(cfg.fonts.status_bar_size, 15.0);
    }

    #[test]
    fn validate_catches_mouse_profile_ranges() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.push(MouseProfile {
            scroll_mode: Some("turbo".into()),
            line_count: Some(99),
            ..Default::default()
        });
        assert_err_count(&cfg, 2);
    }

    #[test]
    fn smooth_mouse_fields_validate_and_fall_back_independently() {
        let mut loaded = Config::default();
        loaded.mouse.profiles[0].scroll_mode = Some("line".into());
        let smooth = loaded.mouse.profiles[0].smooth_scrolling.as_mut().unwrap();
        smooth.enabled = Some(true);
        smooth.preset = Some("unknown".into());
        smooth.response = Some(3.0);
        smooth.speed = Some(2.4);
        let errors = loaded.validate();
        assert!(errors
            .iter()
            .any(|error| error.contains("smooth_scrolling.preset")));
        assert!(errors
            .iter()
            .any(|error| error.contains("smooth_scrolling.response")));

        let mut merged = Config::default();
        merged.merge_valid(loaded, &errors);
        let smooth = merged.mouse.profiles[0].smooth_scrolling.as_ref().unwrap();
        assert_eq!(
            merged.mouse.profiles[0].scroll_mode.as_deref(),
            Some("smooth")
        );
        assert_eq!(smooth.enabled, None);
        assert_eq!(smooth.preset, None);
        assert_eq!(smooth.response, None);
        assert_eq!(smooth.speed, Some(2.4));
    }

    #[test]
    fn an_invalid_image_rows_falls_back_without_losing_sibling_fields() {
        let mut ours = Config::default();
        ours.clipboard.image_rows = 2;
        let mut other = Config::default();
        other.clipboard.image_rows = 9;
        other.clipboard.auto_expire_days = 5;
        let errs = other.validate();
        assert!(
            errs.iter().any(|e| e.starts_with("clipboard.image_rows")),
            "the field must be rejected with its own prefix: {errs:?}"
        );
        ours.merge_valid(other, &errs);
        assert_eq!(ours.clipboard.image_rows, 2);
        assert_eq!(ours.clipboard.auto_expire_days, 5);
    }

    #[test]
    fn merge_valid_keeps_healthy_sections_wholesale() {
        // A fully valid config is merged wholesale with all custom values kept (the old test
        // merged defaults into defaults, making the assertions vacuous).
        let mut other = Config::default();
        other.appearance.theme = "dark".into();
        other.keyboard.modifier = "option".into();
        other.i18n.locale = "zh-Hant".into();
        other.mouse.enabled = true;
        other.clipboard.enabled = true;
        other.clipboard.shortcut = "cmd+shift+v".into();
        other.clipboard.max_entries = 30;
        other.clipboard.clear_on_quit = true;
        other.clipboard.move_used_to_top = false;
        other.clipboard.delete_after_paste = true;
        other.clipboard.clear_system_pasteboard_after_paste = true;
        other.clipboard.pin_follow_selection = false;
        other.layout.card_text_size = 16.0;
        let mut merged = Config::default();
        merged.merge_valid(other, &[]);
        assert_eq!(merged.appearance.theme, "dark");
        assert_eq!(merged.keyboard.modifier, "option");
        assert_eq!(merged.i18n.locale, "zh-Hant");
        assert!(merged.mouse.enabled);
        assert!(merged.clipboard.enabled);
        assert_eq!(merged.clipboard.shortcut, "cmd+shift+v");
        assert_eq!(merged.clipboard.max_entries, 30);
        assert!(merged.clipboard.clear_on_quit);
        assert!(!merged.clipboard.move_used_to_top);
        assert!(merged.clipboard.delete_after_paste);
        assert!(merged.clipboard.clear_system_pasteboard_after_paste);
        assert!(!merged.clipboard.pin_follow_selection);
        assert_eq!(merged.layout.card_text_size, 16.0);
    }

    #[test]
    fn validate_rejects_out_of_range_clipboard_max_entries() {
        // Clipboard max entries must be within 1..=100.
        let mut cfg = Config::default();
        cfg.clipboard.max_entries = 0;
        assert_err_count(&cfg, 1);
        cfg.clipboard.max_entries = 101;
        assert_err_count(&cfg, 1);
        cfg.clipboard.max_entries = 50;
        assert_err_count(&cfg, 0);
    }

    /// The popup's index and the stored value must be inverses: the settings page fills the popup from
    /// the value, and the popup's action stores the index it reports. A drift here would display one
    /// material while persisting another -- the "the panel looks like frost but the settings say liquid
    /// glass" report, which is exactly what these two helpers exist to make impossible.
    #[test]
    fn panel_material_index_and_value_round_trip() {
        for (index, value) in PANEL_MATERIAL_VALUES.iter().enumerate() {
            assert_eq!(panel_material_index_of(value), Some(index));
            assert_eq!(panel_material_value_at(index), *value);
        }
        assert_eq!(panel_material_index_of("nope"), None);
        assert_eq!(
            panel_material_value_at(99),
            PANEL_MATERIAL_VALUES[0],
            "an impossible index must fall back to the first item, never to a wrong material"
        );
        // The list order is what the dropdown shows, so it is part of the contract.
        assert_eq!(PANEL_MATERIAL_VALUES, ["liquid-glass", "frost", "opaque"]);
    }

    /// Same contract for the switcher modifier popup, plus validation accepting every popup value.
    #[test]
    fn switcher_modifier_index_and_value_round_trip() {
        for (index, value) in SWITCHER_MODIFIER_VALUES.iter().enumerate() {
            assert_eq!(switcher_modifier_index_of(value), index);
            assert_eq!(switcher_modifier_value_at(index), *value);
            let mut cfg = Config::default();
            cfg.keyboard.modifier = (*value).into();
            assert!(
                cfg.validate().is_empty(),
                "{value} must be a valid modifier"
            );
        }
        assert_eq!(switcher_modifier_index_of("nope"), 0);
        assert_eq!(switcher_modifier_value_at(99), "option");
        assert_eq!(SWITCHER_MODIFIER_VALUES, ["option", "command", "control"]);
    }

    #[test]
    fn clipboard_shortcut_invalid_value_falls_back_per_field() {
        let mut loaded = Config::default();
        loaded.clipboard.enabled = true;
        loaded.clipboard.shortcut = "cmd+not-a-key".into();
        loaded.clipboard.max_entries = 42;
        let errors = loaded.validate();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].starts_with("clipboard.shortcut:"));

        let mut merged = Config::default();
        merged.merge_valid(loaded, &errors);
        assert_eq!(merged.clipboard.shortcut, "option+v");
        assert!(merged.clipboard.enabled);
        assert_eq!(merged.clipboard.max_entries, 42);
    }

    #[test]
    fn clipboard_shortcut_conflict_falls_back_per_field() {
        let mut loaded = Config::default();
        loaded.clipboard.shortcut = "option+left".into();
        loaded.clipboard.clear_on_quit = true;
        let errors = loaded.validate();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].starts_with("clipboard.shortcut:"));

        let mut merged = Config::default();
        merged.merge_valid(loaded, &errors);
        assert_eq!(merged.clipboard.shortcut, "option+v");
        assert!(merged.clipboard.clear_on_quit);
    }

    #[test]
    fn a_config_from_an_older_version_that_still_has_persist_loads() {
        // `persist` was retired: history is always written to disk now. Old files still carry the
        // key, and an unknown key must not make the whole config fail to load.
        let cfg: Config = toml::from_str("[clipboard]\nenabled = true\npersist = false\n").unwrap();
        assert!(cfg.clipboard.enabled);
        assert!(!cfg.clipboard.clear_on_quit);
        let cfg: Config = toml::from_str("[clipboard]\nenabled = true\npersist = true\n").unwrap();
        assert!(cfg.clipboard.enabled);
    }

    #[test]
    fn retired_clipboard_highlight_limits_are_ignored() {
        // Highlight limits in old configs are retired, but serde must ignore them so upgrades do
        // not make the entire config fail to load.
        let cfg: Config = toml::from_str(
            "[clipboard]\nmax_highlight_bytes = 65536\nmax_highlight_lines = 1000\n",
        )
        .unwrap();
        assert_eq!(cfg.clipboard.max_entries, 50);
    }

    #[test]
    fn validate_rejects_unknown_clipboard_picker_position() {
        // The picker position only accepts mouse / main.
        let mut cfg = Config::default();
        assert_eq!(cfg.clipboard.picker_position, "main");
        cfg.clipboard.picker_position = "top-right".into();
        assert_err_count(&cfg, 1);
        cfg.clipboard.picker_position = "mouse".into();
        assert_err_count(&cfg, 0);
        cfg.clipboard.picker_position = "main".into();
        assert_err_count(&cfg, 0);
    }

    #[test]
    fn validate_accepts_auto_expire_days_0_to_7() {
        // Auto-expire days: 0 (never) and 7 are valid, 8 is rejected.
        let mut cfg = Config::default();
        assert_eq!(cfg.clipboard.auto_expire_days, 3, "default is 3 days");
        assert_err_count(&cfg, 0);
        cfg.clipboard.auto_expire_days = 0;
        assert_err_count(&cfg, 0);
        cfg.clipboard.auto_expire_days = 7;
        assert_err_count(&cfg, 0);
        cfg.clipboard.auto_expire_days = 8;
        assert_err_count(&cfg, 1);
    }

    #[test]
    fn merge_valid_resets_only_invalid_fields() {
        let mut other = Config::default();
        // Valid field: everything customized.
        other.appearance.theme = "dark".into();
        other.appearance.glass_style = "regular".into();
        other.appearance.corner_radius = 12.0;
        // Invalid field: corner_radius < 0.
        let mut cfg = other.clone();
        cfg.appearance.corner_radius = -5.0;
        let errs = cfg.validate();
        assert_eq!(errs.len(), 1);

        let mut merged = Config::default();
        merged.merge_valid(cfg, &errs);
        // Valid fields survive; the invalid one falls back to the default.
        assert_eq!(merged.appearance.theme, "dark");
        assert_eq!(
            merged.appearance.corner_radius,
            Config::default().appearance.corner_radius
        );
    }

    #[test]
    fn panel_material_validates_and_merges_per_field() {
        let mut cfg = Config::default();
        assert_err_count(&cfg, 0);
        cfg.appearance.panel_material = "frost".into();
        assert_err_count(&cfg, 0);
        cfg.appearance.panel_material = "opaque".into();
        assert_err_count(&cfg, 0);
        cfg.appearance.panel_material = "glass".into();
        assert_err_count(&cfg, 1);

        // An invalid material falls back to the default while valid neighbors survive.
        let mut loaded = Config::default();
        loaded.appearance.theme = "dark".into();
        loaded.appearance.panel_material = "transparent".into();
        let errors = loaded.validate();
        assert_eq!(errors.len(), 1);
        let mut merged = Config::default();
        merged.merge_valid(loaded, &errors);
        assert_eq!(merged.appearance.theme, "dark");
        assert_eq!(merged.appearance.panel_material, "liquid-glass");
    }

    #[test]
    fn keystroke_display_defaults_and_invalid_enum_fallbacks_are_per_field() {
        let defaults = Config::default();
        assert!(!defaults.keystroke_display.enabled);
        assert_eq!(defaults.keystroke_display.mode, "all");
        assert_eq!(defaults.keystroke_display.tap_level, "session");
        assert_eq!(defaults.keystroke_display.display_position, "caret");
        assert_eq!(defaults.keystroke_display.initial_position, "bottom");

        let mut loaded = defaults.clone();
        loaded.keystroke_display.enabled = true;
        loaded.keystroke_display.display_position = "main".into();
        loaded.keystroke_display.mode = "shortcuts".into();
        loaded.keystroke_display.tap_level = "invalid".into();
        let errors = loaded.validate();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].starts_with("keystroke_display.tap_level"));

        let mut merged = defaults.clone();
        merged.merge_valid(loaded, &errors);
        assert!(merged.keystroke_display.enabled);
        assert_eq!(merged.keystroke_display.display_position, "main");
        assert_eq!(merged.keystroke_display.mode, "shortcuts");
        assert_eq!(merged.keystroke_display.tap_level, "session");
    }

    /// The initial position is one more per-field enum: an unusable value must fall back to the
    /// documented default without discarding the section's other, valid settings.
    #[test]
    fn keystroke_display_initial_position_validates_and_falls_back_per_field() {
        for valid in ["top", "bottom", "left", "right"] {
            let mut config = Config::default();
            config.keystroke_display.initial_position = valid.into();
            assert!(
                config.validate().is_empty(),
                "\"{valid}\" must be an accepted initial position"
            );
        }

        let mut loaded = Config::default();
        loaded.keystroke_display.enabled = true;
        loaded.keystroke_display.display_position = "main".into();
        loaded.keystroke_display.initial_position = "sideways".into();
        let errors = loaded.validate();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].starts_with("keystroke_display.initial_position"));

        let mut merged = Config::default();
        merged.merge_valid(loaded, &errors);
        // The bad enum falls back, its neighbours survive.
        assert_eq!(merged.keystroke_display.initial_position, "bottom");
        assert_eq!(merged.keystroke_display.display_position, "main");
        assert!(merged.keystroke_display.enabled);
    }

    /// The panel cap is a whole-tens percentage. A value off the step or outside the range must be
    /// reported and fall back per field, and the default must be the panel pre-setting behaviour.
    #[test]
    fn keystroke_display_max_extent_percent_validates_steps_and_falls_back_per_field() {
        for valid in [10u32, 20, 50, 100] {
            let mut config = Config::default();
            config.keystroke_display.max_extent_percent = valid;
            assert!(
                config.validate().is_empty(),
                "{valid}% must be an accepted cap"
            );
        }
        // The pre-setting behaviour: half the screen.
        assert_eq!(
            Config::default().keystroke_display.max_extent_percent,
            50,
            "the default cap must be the old hard-coded 50%"
        );

        for invalid in [0u32, 5, 9, 15, 55, 101, 1000] {
            let mut loaded = Config::default();
            loaded.keystroke_display.enabled = true;
            loaded.keystroke_display.mode = "shortcuts".into();
            loaded.keystroke_display.max_extent_percent = invalid;
            let errors = loaded.validate();
            assert_eq!(errors.len(), 1, "{invalid}% must be refused once");
            assert!(errors[0].starts_with("keystroke_display.max_extent_percent"));

            let mut merged = Config::default();
            merged.merge_valid(loaded, &errors);
            // The bad step falls back, its neighbours survive.
            assert_eq!(merged.keystroke_display.max_extent_percent, 50);
            assert_eq!(merged.keystroke_display.mode, "shortcuts");
            assert!(merged.keystroke_display.enabled);
        }

        // The slider can only produce accepted steps, but a stale or hand-set number is snapped
        // rather than written back invalid.
        assert_eq!(keystroke_display_max_extent_percent_snap(0), 10);
        assert_eq!(keystroke_display_max_extent_percent_snap(54), 50);
        assert_eq!(keystroke_display_max_extent_percent_snap(56), 60);
        assert_eq!(keystroke_display_max_extent_percent_snap(999), 100);
        for percent in [10u32, 30, 70, 100] {
            assert!(keystroke_display_max_extent_percent_is_valid(
                keystroke_display_max_extent_percent_snap(percent)
            ));
        }
    }

    #[test]
    fn keystroke_display_max_extent_percent_roundtrips_through_toml() {
        let mut config = Config::default();
        config.keystroke_display.max_extent_percent = 80;
        let text = toml::to_string(&config).unwrap();
        assert!(text.contains("max_extent_percent = 80"));
        let parsed: Config = toml::from_str(&text).unwrap();
        assert_eq!(parsed.keystroke_display.max_extent_percent, 80);
        // An older file without the key keeps the documented default.
        let legacy: Config = toml::from_str("[keystroke_display]\nenabled = true\n").unwrap();
        assert_eq!(legacy.keystroke_display.max_extent_percent, 50);
    }
    #[test]
    fn keystroke_display_position_migrates_legacy_boolean_and_validates_modes() {
        let follows_window: Config =
            toml::from_str("[keystroke_display]\nfollow_frontmost_screen = true\n").unwrap();
        assert_eq!(follows_window.keystroke_display.display_position, "caret");
        let serialized = toml::to_string(&follows_window).unwrap();
        assert!(serialized.contains("display_position = \"caret\""));
        assert!(!serialized.contains("follow_frontmost_screen"));
        let stays_on_main: Config =
            toml::from_str("[keystroke_display]\nfollow_frontmost_screen = false\n").unwrap();
        assert_eq!(stays_on_main.keystroke_display.display_position, "main");

        let mut invalid = Config::default();
        invalid.keystroke_display.display_position = "pointer".into();
        let errors = invalid.validate();
        assert!(errors
            .iter()
            .any(|error| error.starts_with("keystroke_display.display_position")));
        let mut merged = Config::default();
        merged.merge_valid(invalid, &errors);
        assert_eq!(merged.keystroke_display.display_position, "caret");
    }

    #[test]
    fn keystroke_display_position_roundtrips_and_invalid_position_falls_back_per_field() {
        let mut loaded = Config::default();
        loaded.keystroke_display.enabled = true;
        loaded.keystroke_display.position = Some(KeystrokeDisplayPosition {
            x: -1200.5,
            y: 88.0,
        });
        let serialized = toml::to_string(&loaded).unwrap();
        let (roundtripped, errors, _) = parse_config_content(&serialized).unwrap();
        assert!(errors.is_empty());
        assert_eq!(
            roundtripped.keystroke_display.position,
            loaded.keystroke_display.position
        );

        loaded.keystroke_display.position = Some(KeystrokeDisplayPosition {
            x: f64::INFINITY,
            y: 88.0,
        });
        let errors = loaded.validate();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].starts_with("keystroke_display.position.x"));
        let mut merged = Config::default();
        merged.merge_valid(loaded, &errors);
        assert!(merged.keystroke_display.enabled);
        assert_eq!(merged.keystroke_display.position, None);
    }

    #[test]
    fn migrate_legacy_is_idempotent() {
        let mut cfg = Config::default();
        let before = cfg.mouse.clone();
        cfg.mouse.migrate_legacy();
        cfg.mouse.migrate_legacy();
        // A second migration must not change anything.
        assert_eq!(cfg.mouse.profiles.len(), before.profiles.len());
        assert!(cfg.mouse.reverse_scroll.is_none());
    }

    #[test]
    fn card_text_style_migration_rewrites_old_defaults() {
        let mut cfg = Config::default();
        // Inject the old-default combo (pre-swap fonts and colors): fonts + the old dark
        // pair in the dark section + the old light pair in the light section.
        cfg.fonts.title_size = 11.0;
        cfg.fonts.title_weight = 0.23;
        cfg.fonts.app_name_size = 13.0;
        cfg.fonts.app_name_weight = 0.5;
        cfg.colors.dark.app_name = "ddddddff".into();
        cfg.colors.dark.win_title = "888888ff".into();
        cfg.colors.light.app_name = "1a1a1aff".into();
        cfg.colors.light.win_title = "333333ff".into();

        assert!(cfg.migrate_card_text_style());
        assert_eq!(cfg.fonts.title_size, 12.0);
        assert_eq!(cfg.fonts.title_weight, crate::theme::FONT_WEIGHT_SEMIBOLD);
        assert_eq!(cfg.fonts.app_name_size, 10.0);
        assert_eq!(cfg.fonts.app_name_weight, crate::theme::FONT_WEIGHT_REGULAR);
        // each section migrates to its same-family new pair.
        assert_eq!(cfg.colors.dark.app_name, "FFFFFF57");
        assert_eq!(cfg.colors.dark.win_title, "FFFFFFD1");
        assert_eq!(cfg.colors.light.app_name, "00000057");
        assert_eq!(cfg.colors.light.win_title, "000000D1");
        // Idempotent: the new defaults no longer match the old values; a second call is a no-op.
        assert!(!cfg.migrate_card_text_style());
    }

    #[test]
    fn card_text_style_migration_skips_customized_values() {
        let mut cfg = Config::default();
        // A single customized font value -> the whole group is skipped (conservative;
        // colors are at the new defaults here and do not trigger either).
        cfg.fonts.title_size = 11.0;
        cfg.fonts.title_weight = 0.35; // customized
        cfg.fonts.app_name_size = 13.0;
        cfg.fonts.app_name_weight = 0.4; // customized
        cfg.fonts.status_bar_weight = 0.45; // customized
        assert!(!cfg.migrate_card_text_style());
        assert_eq!(cfg.fonts.app_name_weight, 0.4);
        assert_eq!(cfg.fonts.title_weight, 0.35);
        assert_eq!(cfg.fonts.status_bar_weight, 0.45);

        // Colors likewise: with NEITHER section pairing up against an old default, nothing moves.
        cfg.colors.dark.app_name = "custom01ff".into();
        cfg.colors.light.app_name = "custom01ff".into();
        assert!(!cfg.migrate_card_text_style());
        assert_eq!(cfg.colors.dark.app_name, "custom01ff");
        assert_eq!(cfg.colors.light.app_name, "custom01ff");
    }

    #[test]
    fn legacy_overlay_medium_weight_defaults_migrate_to_semibold() {
        let mut cfg = Config::default();
        cfg.fonts.title_weight = 0.23;
        cfg.fonts.status_bar_weight = 0.23;
        assert!(cfg.migrate_card_text_style());
        assert_eq!(cfg.fonts.title_weight, crate::theme::FONT_WEIGHT_SEMIBOLD);
        assert_eq!(
            cfg.fonts.status_bar_weight,
            crate::theme::FONT_WEIGHT_SEMIBOLD
        );
        assert!(!cfg.migrate_card_text_style());
    }

    #[test]
    fn card_text_style_migration_handles_dark_color_pair() {
        let mut cfg = Config::default();
        cfg.fonts.title_size = 12.0; // fonts already new
        cfg.colors.dark.app_name = "ddddddff".into();
        cfg.colors.dark.win_title = "888888ff".into();
        assert!(cfg.migrate_card_text_style());
        assert_eq!(cfg.colors.dark.app_name, "FFFFFF57");
        assert_eq!(cfg.colors.dark.win_title, "FFFFFFD1");
    }

    #[test]
    fn migrate_legacy_folds_legacy_fields_into_wildcard_profile() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();
        cfg.mouse.reverse_scroll = Some(true);
        cfg.mouse.scroll_mode = Some("line".into());
        cfg.mouse.line_count = Some(7);
        cfg.mouse.pointer = Some(PointerSection {
            disable_acceleration: true,
        });
        let changed = cfg.mouse.migrate_legacy();
        assert!(changed);
        // Folded into the "All Mice" profile; legacy fields cleared.
        assert_eq!(cfg.mouse.profiles.len(), 1);
        let p = &cfg.mouse.profiles[0];
        assert_eq!(p.reverse_scroll, Some(true));
        assert_eq!(p.scroll_mode.as_deref(), Some("line"));
        assert_eq!(p.line_count, Some(7));
        assert_eq!(
            p.pointer.as_ref().and_then(|x| x.disable_acceleration),
            Some(true)
        );
        assert!(cfg.mouse.reverse_scroll.is_none());
        assert!(cfg.mouse.scroll_mode.is_none());
    }

    #[test]
    fn legacy_smooth_enabled_migrates_each_profile_without_changing_tuning() {
        let mut cfg = Config::default();
        cfg.mouse.profiles = vec![
            MouseProfile {
                scroll_mode: Some("line".into()),
                line_count: Some(6),
                smooth_scrolling: Some(SmoothPartial {
                    enabled: Some(true),
                    preset: Some("custom".into()),
                    response: Some(1.4),
                    speed: Some(2.1),
                    acceleration: Some(3.2),
                    inertia: Some(4.3),
                }),
                ..Default::default()
            },
            MouseProfile {
                device: DeviceMatcher {
                    vendor_id: Some(7),
                    product_id: Some(9),
                    ..Default::default()
                },
                scroll_mode: Some("default".into()),
                line_count: Some(5),
                smooth_scrolling: Some(SmoothPartial {
                    enabled: Some(true),
                    response: Some(1.2),
                    ..Default::default()
                }),
                ..Default::default()
            },
            MouseProfile {
                device: DeviceMatcher {
                    vendor_id: Some(8),
                    product_id: Some(10),
                    ..Default::default()
                },
                scroll_mode: Some("line".into()),
                smooth_scrolling: Some(SmoothPartial {
                    enabled: Some(false),
                    speed: Some(2.7),
                    ..Default::default()
                }),
                ..Default::default()
            },
            MouseProfile {
                device: DeviceMatcher {
                    vendor_id: Some(8),
                    product_id: Some(11),
                    ..Default::default()
                },
                scroll_mode: Some("default".into()),
                smooth_scrolling: Some(SmoothPartial {
                    enabled: Some(false),
                    acceleration: Some(1.8),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ];

        assert!(cfg.mouse.migrate_legacy());
        let wildcard = &cfg.mouse.profiles[0];
        assert_eq!(wildcard.scroll_mode.as_deref(), Some("smooth"));
        assert_eq!(wildcard.line_count, Some(6));
        let tuning = wildcard.smooth_scrolling.as_ref().unwrap();
        assert_eq!(tuning.enabled, None);
        assert_eq!(tuning.preset.as_deref(), Some("custom"));
        assert_eq!(tuning.response, Some(1.4));
        assert_eq!(tuning.speed, Some(2.1));
        assert_eq!(tuning.acceleration, Some(3.2));
        assert_eq!(tuning.inertia, Some(4.3));

        let device_smooth = &cfg.mouse.profiles[1];
        assert_eq!(device_smooth.scroll_mode.as_deref(), Some("smooth"));
        assert_eq!(device_smooth.line_count, Some(5));
        assert_eq!(
            device_smooth.smooth_scrolling.as_ref().unwrap().enabled,
            None
        );
        for (profile, expected) in [
            (&cfg.mouse.profiles[2], "line"),
            (&cfg.mouse.profiles[3], "default"),
        ] {
            assert_eq!(profile.scroll_mode.as_deref(), Some(expected));
            assert_eq!(profile.smooth_scrolling.as_ref().unwrap().enabled, None);
        }
        let saved: toml::Value = toml::to_string(&cfg).unwrap().parse().unwrap();
        let profiles = saved["mouse"]["profiles"].as_array().unwrap();
        assert!(profiles.iter().all(|profile| {
            profile
                .get("smooth_scrolling")
                .and_then(|smooth| smooth.get("enabled"))
                .is_none()
        }));
        assert!(!cfg.mouse.migrate_legacy());
    }

    #[test]
    fn config_loader_accepts_and_migrates_legacy_smooth_enabled_in_device_layers() {
        let content = r#"
[mouse]
enabled = true

[[mouse.profiles]]
scroll_mode = "line"
line_count = 6

[mouse.profiles.smooth_scrolling]
enabled = true
response = 1.35

[[mouse.profiles]]
device_vendor_id = 7
device_product_id = 9
scroll_mode = "default"

[mouse.profiles.smooth_scrolling]
enabled = true
speed = 2.6
"#;
        let (loaded, errors, needs_persist) = parse_config_content(content).unwrap();
        assert!(errors.is_empty());
        assert!(needs_persist);
        assert_eq!(
            loaded.mouse.profiles[0].scroll_mode.as_deref(),
            Some("smooth")
        );
        assert_eq!(loaded.mouse.profiles[0].line_count, Some(6));
        assert_eq!(
            loaded.mouse.profiles[0]
                .smooth_scrolling
                .as_ref()
                .unwrap()
                .response,
            Some(1.35)
        );
        assert_eq!(
            loaded.mouse.profiles[1].scroll_mode.as_deref(),
            Some("smooth")
        );
        assert_eq!(
            loaded.mouse.profiles[1]
                .smooth_scrolling
                .as_ref()
                .unwrap()
                .speed,
            Some(2.6)
        );
        assert!(loaded.mouse.profiles.iter().all(|profile| {
            profile
                .smooth_scrolling
                .as_ref()
                .map(|smooth| smooth.enabled.is_none())
                .unwrap_or(true)
        }));
    }

    #[test]
    fn unknown_scroll_mode_falls_back_without_discarding_other_profile_fields() {
        let mut loaded = Config::default();
        loaded.mouse.profiles[0].scroll_mode = Some("turbo".into());
        loaded.mouse.profiles[0].line_count = Some(8);
        loaded.mouse.profiles[0]
            .smooth_scrolling
            .as_mut()
            .unwrap()
            .speed = Some(2.4);
        let errors = loaded.validate();
        assert!(errors
            .iter()
            .any(|error| { error.starts_with("mouse.profiles[0].scroll_mode") }));

        let mut merged = Config::default();
        merged.merge_valid(loaded, &errors);
        assert_eq!(merged.mouse.profiles[0].scroll_mode, None);
        assert_eq!(merged.mouse.profiles[0].line_count, Some(8));
        assert_eq!(
            merged.mouse.profiles[0]
                .smooth_scrolling
                .as_ref()
                .unwrap()
                .speed,
            Some(2.4)
        );
    }

    #[test]
    fn a_config_without_the_panel_action_keys_defaults_them_off() {
        // An existing config predates these switches; it must gain them switched OFF, and the
        // surrounding values must survive untouched.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[windows]\nshow_minimized = true\nactivation_mode = \"click\"\n",
        )
        .unwrap();
        let (loaded, errs) = Config::load_or_default_from(&path);
        assert!(errs.is_empty(), "errors: {:?}", errs);
        assert!(!loaded.windows.panel_close_enabled);
        assert!(!loaded.windows.panel_minimize_enabled);
        assert!(!loaded.windows.panel_hide_enabled);
        assert!(!loaded.windows.panel_quit_enabled);
        assert!(loaded.windows.show_minimized);
        assert_eq!(loaded.windows.activation_mode, "click");
    }

    #[test]
    fn a_wrongly_typed_panel_action_key_falls_back_without_discarding_siblings() {
        // The per-field promise: one bad TOML type must not take the valid neighbours with it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[windows]\npanel_quit_enabled = \"yes please\"\nshow_minimized = true\n",
        )
        .unwrap();
        let (loaded, errs) = Config::load_or_default_from(&path);
        assert!(!errs.is_empty(), "a wrong TOML type must be reported");
        assert!(!loaded.windows.panel_quit_enabled);
        assert!(loaded.windows.show_minimized, "the valid sibling is kept");
    }

    #[test]
    fn save_and_load_roundtrip_preserves_custom_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut cfg = Config::default();
        // All fields set to non-default values -- the old default-value roundtrip masked the
        // migrate_legacy overwrite bug (see docs/test-review.md); this is the regression guard.
        cfg.appearance.theme = "dark".into();
        cfg.keyboard.modifier = "option".into();
        cfg.i18n.locale = "zh-Hans".into();
        cfg.updates.automatically_check = false;
        cfg.windows.overlay_position = "main".into();
        cfg.windows.activation_mode = "click".into();
        cfg.windows.show_minimized = true;
        cfg.windows.show_hidden_app_windows = false;
        cfg.windows.panel_close_enabled = true;
        cfg.windows.panel_minimize_enabled = true;
        cfg.windows.panel_hide_enabled = true;
        cfg.windows.panel_quit_enabled = true;
        // Non-default: the shipped default is true, so storing false proves the roundtrip.
        cfg.windows.show_other_desktops = false;
        cfg.windows.enabled = false; // non-default: verify the roundtrip
        cfg.mouse.enabled = true;
        cfg.mouse.profiles = vec![
            MouseProfile {
                reverse_scroll: Some(true),
                scroll_mode: Some("line".into()),
                line_count: Some(5),
                pointer: Some(PartialPointerSection {
                    disable_acceleration: Some(true),
                    acceleration: Some(1.25),
                }),
                ..Default::default()
            },
            MouseProfile {
                device: DeviceMatcher {
                    vendor_id: Some(1133),
                    product_id: Some(17492),
                    ..Default::default()
                },
                reverse_scroll: Some(false),
                ..Default::default()
            },
        ];
        cfg.save_to(&path).unwrap();

        let (loaded, errs) = Config::load_or_default_from(&path);
        assert!(errs.is_empty());
        // Non-mouse fields survive the roundtrip.
        assert_eq!(loaded.appearance.theme, "dark");
        assert_eq!(loaded.keyboard.modifier, "option");
        assert_eq!(loaded.i18n.locale, "zh-Hans");
        assert!(!loaded.updates.automatically_check);
        assert_eq!(loaded.windows.overlay_position, "main");
        assert_eq!(loaded.windows.activation_mode, "click");
        assert!(loaded.windows.show_minimized);
        assert!(!loaded.windows.show_hidden_app_windows);
        assert!(loaded.windows.panel_close_enabled);
        assert!(loaded.windows.panel_minimize_enabled);
        assert!(loaded.windows.panel_hide_enabled);
        assert!(loaded.windows.panel_quit_enabled);
        assert!(!loaded.windows.show_other_desktops);
        assert!(!loaded.windows.enabled);
        // Mouse profiles survive untouched (one wildcard + one per-device).
        assert!(loaded.mouse.enabled);
        assert_eq!(loaded.mouse.profiles.len(), 2);
        let w = &loaded.mouse.profiles[0];
        assert_eq!(w.device.vendor_id, None);
        assert_eq!(w.reverse_scroll, Some(true));
        assert_eq!(w.scroll_mode.as_deref(), Some("line"));
        assert_eq!(w.line_count, Some(5));
        assert_eq!(
            w.pointer.as_ref().and_then(|x| x.disable_acceleration),
            Some(true)
        );
        assert_eq!(w.pointer.as_ref().and_then(|x| x.acceleration), Some(1.25));
        let d = &loaded.mouse.profiles[1];
        assert_eq!(d.device.vendor_id, Some(1133));
        assert_eq!(d.device.product_id, Some(17492));
        assert_eq!(d.reverse_scroll, Some(false));
    }

    #[cfg(unix)]
    #[test]
    fn atomic_save_sets_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        Config::default().save_to(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("[appearance]"));
    }

    #[test]
    fn load_new_format_config_leaves_profiles_untouched() {
        // Negative constraint: a new-format config (profiles only, no legacy flat fields) must
        // load untouched -- migrate_legacy must not mistake it for legacy content (the old bug).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[mouse]
enabled = true

[[mouse.profiles]]
reverse_scroll = true
scroll_mode = "line"
line_count = 5

[mouse.profiles.pointer]
disable_acceleration = true

[[mouse.profiles]]
device_vendor_id = 1133
device_product_id = 17492
reverse_scroll = false
"#,
        )
        .unwrap();
        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(errs.is_empty());
        assert_eq!(cfg.mouse.profiles.len(), 2);
        // Wildcard profile: custom values preserved.
        let w = &cfg.mouse.profiles[0];
        assert_eq!(w.reverse_scroll, Some(true));
        assert_eq!(w.scroll_mode.as_deref(), Some("line"));
        assert_eq!(w.line_count, Some(5));
        assert_eq!(
            w.pointer.as_ref().and_then(|x| x.disable_acceleration),
            Some(true)
        );
        // Per-device profile preserved.
        let d = &cfg.mouse.profiles[1];
        assert_eq!(d.device.vendor_id, Some(1133));
        assert_eq!(d.device.product_id, Some(17492));
        assert_eq!(d.reverse_scroll, Some(false));
    }

    #[test]
    fn load_missing_file_writes_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.toml");
        let (cfg, errs) = Config::load_or_default_from(&path);
        // Missing file: defaults written and returned, no errors.
        assert!(errs.is_empty());
        assert_eq!(cfg.appearance.theme, "auto");
        assert!(path.exists());
    }

    #[test]
    fn load_read_error_keeps_existing_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let original = "custom_setting = 'keep me'";
        std::fs::write(&path, original).unwrap();

        let (cfg, errs) = Config::load_or_default_from_result(
            &path,
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "permission denied",
            )),
        );

        assert_eq!(cfg.appearance.theme, Config::default().appearance.theme);
        assert!(!errs.is_empty());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn reload_fatal_errors_leave_the_file_untouched_and_missing_files_missing() {
        let dir = tempfile::tempdir().unwrap();
        let invalid_path = dir.path().join("invalid.toml");
        let original = "[appearance\ntheme = 'dark'";
        std::fs::write(&invalid_path, original).unwrap();

        assert!(Config::reload_from(&invalid_path).is_err());
        assert_eq!(std::fs::read_to_string(&invalid_path).unwrap(), original);

        let missing_path = dir.path().join("missing.toml");
        assert!(Config::reload_from(&missing_path).is_err());
        assert!(!missing_path.exists());
    }

    #[test]
    fn load_invalid_file_merges_valid_fields_and_reports_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[appearance]
theme = "dark"
corner_radius = -5.0
"#,
        )
        .unwrap();
        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(!errs.is_empty());
        // Valid fields survive; the out-of-range value falls back to its default, on its own.
        assert_eq!(cfg.appearance.theme, "dark");
        assert_eq!(cfg.appearance.corner_radius, 32.0);
    }

    #[test]
    fn load_type_error_keeps_valid_siblings_and_persists_repair() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[appearance]
theme = "dark"
corner_radius = "not-a-number"

[keyboard]
modifier = "option"
"#,
        )
        .unwrap();

        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(errs.iter().any(|error| {
            error.contains("appearance.corner_radius") && error.contains("string")
        }));
        assert_eq!(cfg.appearance.theme, "dark");
        assert_eq!(cfg.keyboard.modifier, "option");
        assert_eq!(
            cfg.appearance.corner_radius,
            Config::default().appearance.corner_radius
        );

        let persisted = std::fs::read_to_string(&path).unwrap();
        assert!(persisted.contains("theme = \"dark\""));
        assert!(!persisted.contains("not-a-number"));
    }

    #[test]
    fn load_type_errors_are_isolated_per_mouse_profile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[mouse]
enabled = true

[[mouse.profiles]]
reverse_scroll = true
scroll_mode = 123
line_count = 5

[[mouse.profiles]]
device_vendor_id = 1133
device_product_id = 17492
scroll_mode = 456
reverse_scroll = false
"#,
        )
        .unwrap();

        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(errs
            .iter()
            .any(|error| error.contains("mouse.profiles[0].scroll_mode")));
        assert!(errs
            .iter()
            .any(|error| error.contains("mouse.profiles[1].scroll_mode")));
        assert!(cfg.mouse.enabled);
        assert_eq!(cfg.mouse.profiles.len(), 2);
        assert_eq!(cfg.mouse.profiles[0].reverse_scroll, Some(true));
        assert_eq!(cfg.mouse.profiles[0].line_count, Some(5));
        assert_eq!(cfg.mouse.profiles[0].scroll_mode, None);
        assert_eq!(cfg.mouse.profiles[1].device.vendor_id, Some(1133));
        assert_eq!(cfg.mouse.profiles[1].device.product_id, Some(17492));
        assert_eq!(cfg.mouse.profiles[1].reverse_scroll, Some(false));
        assert_eq!(cfg.mouse.profiles[1].scroll_mode, None);
    }

    #[test]
    fn load_out_of_range_pointer_acceleration_is_dropped_per_profile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[mouse]
enabled = true

[[mouse.profiles]]
[mouse.profiles.pointer]
disable_acceleration = true
acceleration = 2.5

[[mouse.profiles]]
device_vendor_id = 1133
device_product_id = 17492
[mouse.profiles.pointer]
acceleration = 41.0
"#,
        )
        .unwrap();

        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(errs
            .iter()
            .any(|error| error.contains("mouse.profiles[1].pointer.acceleration")));
        // The valid profile keeps its value; the out-of-range one loses only acceleration,
        // while disable_acceleration still applies.
        assert_eq!(
            cfg.mouse.profiles[0].pointer.as_ref().unwrap().acceleration,
            Some(2.5)
        );
        assert_eq!(
            cfg.mouse.profiles[1].pointer.as_ref().unwrap().acceleration,
            None
        );
    }

    #[test]
    fn load_type_error_drops_only_invalid_button_mapping_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[mouse]

[[mouse.profiles]]
[mouse.profiles.button_mappings]
"2" = "cmd+v"
"3" = 42
"#,
        )
        .unwrap();

        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(errs
            .iter()
            .any(|error| error.contains("mouse.profiles[0].button_mappings[3]")));
        let mappings = &cfg.mouse.profiles[0].button_mappings;
        assert_eq!(mappings.get("2").map(String::as_str), Some("cmd+v"));
        assert!(!mappings.contains_key("3"));
    }

    #[test]
    fn load_syntax_error_reports_without_overwriting_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let original = "[appearance\ntheme = \"dark\"\n";
        std::fs::write(&path, original).unwrap();

        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(!errs.is_empty());
        assert_eq!(cfg.appearance.theme, Config::default().appearance.theme);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    /// The Finder menu's layout and its two id lists are normalized on every load, not only when
    /// some other field failed validation: a well-formed file naming an unknown layout or an
    /// unknown terminal must not reach the runtime.
    #[test]
    fn load_normalizes_finder_menu_layout_and_lists() {
        let content = r#"
[finder_menu]
enabled = true
layout = "typo"
copy_path = false
new_file_types = ["md", "exe"]
terminals = ["com.mitchellh.ghostty", "com.example.unknown"]
"#;
        let (config, errors, _) = parse_config_content(content).expect("config parses");
        assert!(
            errors.is_empty(),
            "a semantic repair is not a type error: {errors:?}"
        );
        assert_eq!(config.finder_menu.layout, FINDER_MENU_DEFAULT_LAYOUT);
        assert_eq!(config.finder_menu.new_file_types, vec!["md".to_string()]);
        assert_eq!(
            config.finder_menu.terminals,
            vec!["com.mitchellh.ghostty".to_string()]
        );
        assert!(config.finder_menu.enabled);
        assert!(!config.finder_menu.copy_path);
    }

    #[test]
    fn load_unknown_fields_keeps_known_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[appearance]
theme = "dark"
future_option = "kept for forward compatibility"
"#,
        )
        .unwrap();

        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(errs.is_empty());
        assert_eq!(cfg.appearance.theme, "dark");
    }

    #[test]
    fn load_legacy_flat_mouse_fields_migrates_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[mouse]
enabled = true
reverse_scroll = true
"#,
        )
        .unwrap();
        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(errs.is_empty());
        // Legacy fields migrated into an "All Mice" profile and persisted.
        assert_eq!(cfg.mouse.profiles.len(), 1);
        assert_eq!(cfg.mouse.profiles[0].reverse_scroll, Some(true));
        let persisted = std::fs::read_to_string(&path).unwrap();
        assert!(!persisted.contains("device_vendor_id"));
        assert!(persisted.contains("reverse_scroll"));
    }

    #[test]
    fn config_path_in_uses_home_and_creates_dir() {
        let dir = tempfile::tempdir().unwrap();
        let p = config_path_in(dir.path().to_str().unwrap());
        // Path: home/.config/oh-my-tab/config.toml with the dir auto-created.
        assert_eq!(p, dir.path().join(".config/oh-my-tab/config.toml"));
        assert!(p.parent().unwrap().exists());
    }

    /// Tests resolve the config under a per-process temporary home, never under `$HOME`: the load
    /// path rewrites a config whose values it does not know, which reset a developer's real
    /// `keyboard.modifier` when the suite of another branch ran.
    #[test]
    fn tests_never_resolve_the_real_config_path() {
        let path = config_path();
        assert!(path.starts_with(std::env::temp_dir()), "{}", path.display());
        if let Ok(home) = std::env::var("HOME") {
            assert!(!path.starts_with(std::path::Path::new(&home).join(".config")));
        }
    }

    /// The factory config carries the thirteen bindings of the two legacy sections, all of them
    /// with the master switch off — the same "installed but not enabled" state as before.
    #[test]
    fn default_config_seeds_the_factory_bindings() {
        let cfg = Config::default();
        let items = cfg.key_bindings.items.as_ref().expect("factory list");
        assert_eq!(items.len(), 13);
        assert!(!cfg.key_bindings.enabled);
        assert!(items.iter().all(|item| item.enabled));
        assert!(cfg.validate().is_empty(), "{:?}", cfg.validate());
    }

    /// The two legacy group switches are OR-ed into the one master switch: a user who had either
    /// group on keeps it, and a file that had both off (or stated neither) stays off.
    #[test]
    fn the_legacy_group_switches_or_into_the_master_switch() {
        let load = |body: &str| {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.toml");
            std::fs::write(&path, body).unwrap();
            let (cfg, errs) = Config::load_or_default_from(&path);
            assert!(errs.is_empty(), "{errs:?}");
            cfg.key_bindings.enabled
        };
        assert!(load(
            "[window_control]
enabled = true
"
        ));
        assert!(load(
            "[quick_actions]
enabled = true
"
        ));
        assert!(load(
            "[window_control]
enabled = true

[quick_actions]
enabled = true
"
        ));
        assert!(!load(
            "[window_control]
enabled = false

[quick_actions]
enabled = false
"
        ));
        // A binding-era file that only ever had `items` never enabled the feature.
        assert!(!load(
            "[key_bindings]
items = []
"
        ));
        // A config written by the interim binding build still carries the retired group keys; they
        // are read into the master switch rather than dropped. The second case pins that the
        // retired keys are consulted even without a legacy section.
        assert!(load(
            "[key_bindings]
window_control_enabled = true
quick_actions_enabled = false
"
        ));
        assert!(load(
            "[key_bindings]
window_control_enabled = false
quick_actions_enabled = true
"
        ));
        assert!(!load(
            "[key_bindings]
window_control_enabled = false
quick_actions_enabled = false
"
        ));
        // A stated master switch is authoritative: the retired keys must not override it.
        assert!(!load(
            "[key_bindings]
enabled = false
window_control_enabled = true
"
        ));
    }

    /// A legacy file keeps its per-row switches, and a group that was off contributes no live row:
    /// the two legacy switches now share one master switch, so leaving a disabled group's rows
    /// enabled would arm the very interceptions the user had turned off.
    #[test]
    fn legacy_sections_migrate_into_bindings_without_losing_row_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[window_control]
enabled = true
up = false
left = true

[quick_actions]
enabled = false
open_finder = false
"#,
        )
        .unwrap();
        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(errs.is_empty(), "{errs:?}");
        assert!(
            cfg.key_bindings.enabled,
            "window control was on, so the master switch is too"
        );
        let items = cfg.key_bindings.items.as_ref().unwrap();
        let row = |action: &str| {
            items
                .iter()
                .find(|item| item.action == action)
                .unwrap_or_else(|| panic!("{action} missing"))
        };
        assert!(!row("window.snap_up").enabled, "up = false must survive");
        assert!(row("window.snap_left").enabled);
        assert!(
            !row("quick.open_finder").enabled,
            "quick_actions was off, so its rows are seeded off"
        );
        assert!(
            !row("quick.open_settings").enabled,
            "quick_actions was off, so its rows are seeded off"
        );
        // Live set: the master switch on, the "up" row switched off, and no quick-action row live at
        // all -- the disabled group must not come alive with the master switch the other group holds.
        let table = crate::key_bindings::BindingTable::build(items, cfg.key_bindings.enabled);
        assert_eq!(
            table.lookup_chord(0x7B, crate::mouse::shortcut::FLAG_ALT),
            Some(crate::key_bindings::Action::SnapLeft)
        );
        assert_eq!(
            table.lookup_chord(0x7E, crate::mouse::shortcut::FLAG_ALT),
            None,
            "the row switch is off"
        );
        // quick_actions was off in this file: none of its rows may be live, whatever the row flags.
        assert_eq!(
            table.lookup_chord(0x0E, crate::mouse::shortcut::FLAG_ALT),
            None
        );
        assert_eq!(
            table.lookup_chord(0x22, crate::mouse::shortcut::FLAG_ALT),
            None,
            "quick.open_settings must not be live either"
        );
        // The legacy sections are read but never written back. Merely loading does not rewrite the
        // file (seeding is idempotent, so the old sections can stay for a downgrade); the next save
        // is what drops them.
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("[window_control]"));
        cfg.save_to(&path).unwrap();
        let persisted = std::fs::read_to_string(&path).unwrap();
        assert!(!persisted.contains("[window_control]"), "{persisted}");
        assert!(!persisted.contains("[quick_actions]"), "{persisted}");
        assert!(persisted.contains("[[key_bindings.items]]"), "{persisted}");
    }

    /// The mirror of the mixed case, and the reason the group switch has to reach the rows: with
    /// `quick_actions` on and `window_control` off, the master switch is on for the quick actions,
    /// and the window-direction rows must stay dead -- they override Option+arrows in text fields.
    #[test]
    fn a_disabled_group_contributes_no_live_row_to_the_master_switch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[window_control]
enabled = false

[quick_actions]
enabled = true
"#,
        )
        .unwrap();
        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(errs.is_empty(), "{errs:?}");
        assert!(cfg.key_bindings.enabled, "quick_actions was on");
        let items = cfg.key_bindings.items.as_ref().unwrap();
        let row = |action: &str| {
            items
                .iter()
                .find(|item| item.action == action)
                .unwrap_or_else(|| panic!("{action} missing"))
        };
        assert!(
            !row("window.snap_up").enabled,
            "every window row of the disabled group is seeded off"
        );
        assert!(!row("window.display_left").enabled);
        assert!(row("quick.open_finder").enabled);
        let table = crate::key_bindings::BindingTable::build(items, cfg.key_bindings.enabled);
        assert_eq!(
            table.lookup_chord(0x7E, crate::mouse::shortcut::FLAG_ALT),
            None,
            "Option+Up must not be intercepted: window_control was off"
        );
        assert_eq!(
            table.lookup_chord(0x7B, 0x0006_0000),
            None,
            "Option+Shift+Left must not be intercepted either"
        );
        assert_eq!(
            table.lookup_chord(0x0E, crate::mouse::shortcut::FLAG_ALT),
            Some(crate::key_bindings::Action::OpenFinder),
            "the enabled group's rows are live"
        );
    }

    /// An interim file that already carries its own list: it was written while the two group
    /// switches were separate, so the disabled group's rows in *that* list must be switched off
    /// too. Honouring the list verbatim would let those rows start intercepting with the master
    /// switch the other group holds -- the same upgrade regression as the seeded path, one step on.
    #[test]
    fn an_interim_list_folds_the_disabled_group_switch_into_its_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[key_bindings]
window_control_enabled = false
quick_actions_enabled = true

[[key_bindings.items]]
action = "window.snap_left"
trigger = "option+left"
enabled = true

[[key_bindings.items]]
action = "window.snap_up"
trigger = "option+up"
enabled = false

[[key_bindings.items]]
action = "quick.open_finder"
trigger = "option+e"
enabled = true

[[key_bindings.items]]
action = "window.nope"
trigger = "option+j"
enabled = true
"#,
        )
        .unwrap();
        let (cfg, errs) = Config::load_or_default_from(&path);
        // The unknown action is still reported; it belongs to no group, so it is left untouched.
        assert!(
            errs.iter()
                .any(|error| error.starts_with("key_bindings.items[3].action")),
            "{errs:?}"
        );
        assert!(cfg.key_bindings.enabled, "quick_actions was on");
        let items = cfg.key_bindings.items.as_ref().unwrap();
        let row = |action: &str| {
            items
                .iter()
                .find(|item| item.action == action)
                .unwrap_or_else(|| panic!("{action} missing"))
        };
        assert!(
            !row("window.snap_left").enabled,
            "the window row must be folded off: window_control was off"
        );
        assert!(!row("window.snap_up").enabled, "already off, stays off");
        assert!(
            row("quick.open_finder").enabled,
            "the enabled group is untouched"
        );
        // Order and triggers are preserved: the folded list is edited, never rebuilt.
        let order: Vec<&str> = items.iter().map(|item| item.action.as_str()).collect();
        assert_eq!(
            order,
            vec!["window.snap_left", "window.snap_up", "quick.open_finder"],
            "the surviving entries keep the file's order"
        );
        // And the live table agrees.
        let table = crate::key_bindings::BindingTable::build(items, cfg.key_bindings.enabled);
        assert_eq!(
            table.lookup_chord(0x7B, crate::mouse::shortcut::FLAG_ALT),
            None,
            "Option+Left must not be intercepted: window_control was off"
        );
        assert_eq!(
            table.lookup_chord(0x0E, crate::mouse::shortcut::FLAG_ALT),
            Some(crate::key_bindings::Action::OpenFinder)
        );
    }

    /// The mirror direction, and the other half of the fold: an interim file with the window group
    /// on and the quick group off must kill the quick rows in its own list.
    #[test]
    fn an_interim_list_folds_the_other_disabled_group_too() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[key_bindings]
window_control_enabled = true
quick_actions_enabled = false

[[key_bindings.items]]
action = "window.snap_left"
trigger = "option+left"
enabled = true

[[key_bindings.items]]
action = "quick.open_finder"
trigger = "option+e"
enabled = true
"#,
        )
        .unwrap();
        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(errs.is_empty(), "{errs:?}");
        assert!(cfg.key_bindings.enabled, "window_control was on");
        let items = cfg.key_bindings.items.as_ref().unwrap();
        let row = |action: &str| {
            items
                .iter()
                .find(|item| item.action == action)
                .unwrap_or_else(|| panic!("{action} missing"))
        };
        assert!(row("window.snap_left").enabled);
        assert!(
            !row("quick.open_finder").enabled,
            "quick_actions was off, so its row is folded off"
        );
        let table = crate::key_bindings::BindingTable::build(items, cfg.key_bindings.enabled);
        assert_eq!(
            table.lookup_chord(0x0E, crate::mouse::shortcut::FLAG_ALT),
            None,
            "Option+E must not be intercepted"
        );
        assert_eq!(
            table.lookup_chord(0x7B, crate::mouse::shortcut::FLAG_ALT),
            Some(crate::key_bindings::Action::SnapLeft)
        );
    }

    /// The same rule on the *seeded* path: a stated master switch does not let the retired group
    /// keys decide which rows are live. Otherwise the identical explicit switch would mean one thing
    /// with a list present (no fold) and another with it absent (fold), silently dropping shortcuts.
    #[test]
    fn a_stated_master_switch_does_not_fold_a_seeded_list() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[key_bindings]
enabled = true
window_control_enabled = false
quick_actions_enabled = true
"#,
        )
        .unwrap();
        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(errs.is_empty(), "{errs:?}");
        assert!(cfg.key_bindings.enabled);
        let items = cfg.key_bindings.items.as_ref().unwrap();
        let row = |action: &str| {
            items
                .iter()
                .find(|item| item.action == action)
                .unwrap_or_else(|| panic!("{action} missing"))
        };
        assert!(
            row("window.snap_left").enabled,
            "the user enabled the merged feature; the retired group key folds nothing"
        );
        assert!(row("quick.open_finder").enabled);
        let table = crate::key_bindings::BindingTable::build(items, cfg.key_bindings.enabled);
        assert_eq!(
            table.lookup_chord(0x7B, crate::mouse::shortcut::FLAG_ALT),
            Some(crate::key_bindings::Action::SnapLeft),
            "the seeded window rows are live under the stated master switch"
        );
    }

    /// The mirror: with the window group off and quick on, a stated master switch still seeds both
    /// groups' rows from their own flags.
    #[test]
    fn a_stated_master_switch_does_not_fold_a_seeded_list_the_other_way() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[key_bindings]
enabled = true
window_control_enabled = true
quick_actions_enabled = false
"#,
        )
        .unwrap();
        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(errs.is_empty(), "{errs:?}");
        let items = cfg.key_bindings.items.as_ref().unwrap();
        let row = |action: &str| {
            items
                .iter()
                .find(|item| item.action == action)
                .unwrap_or_else(|| panic!("{action} missing"))
        };
        assert!(row("window.snap_left").enabled);
        assert!(
            row("quick.open_finder").enabled,
            "the retired quick key folds nothing under a stated master switch"
        );
    }

    /// A stated master switch still respects a legacy per-row flag: the fold is what the explicit
    /// switch suppresses, not the file's own `up = false`.
    #[test]
    fn a_stated_master_switch_keeps_legacy_row_flags_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[window_control]
enabled = false
up = false

[key_bindings]
enabled = true
"#,
        )
        .unwrap();
        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(errs.is_empty(), "{errs:?}");
        let items = cfg.key_bindings.items.as_ref().unwrap();
        let row = |action: &str| {
            items
                .iter()
                .find(|item| item.action == action)
                .unwrap_or_else(|| panic!("{action} missing"))
        };
        assert!(
            !row("window.snap_up").enabled,
            "the file's own up = false still survives"
        );
        assert!(
            row("window.snap_left").enabled,
            "the retired/legacy group switch does not fold it off"
        );
    }

    /// A file that states its own master switch has made the merged decision itself, so the retired
    /// group keys must not fold anything into its list: only a *derived* master switch may.
    #[test]
    fn a_stated_master_switch_leaves_the_list_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[key_bindings]
enabled = true
window_control_enabled = false
quick_actions_enabled = true

[[key_bindings.items]]
action = "window.snap_left"
trigger = "option+left"
enabled = true

[[key_bindings.items]]
action = "quick.open_finder"
trigger = "option+e"
enabled = true
"#,
        )
        .unwrap();
        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(errs.is_empty(), "{errs:?}");
        assert!(cfg.key_bindings.enabled);
        let items = cfg.key_bindings.items.as_ref().unwrap();
        let row = |action: &str| {
            items
                .iter()
                .find(|item| item.action == action)
                .unwrap_or_else(|| panic!("{action} missing"))
        };
        assert!(
            row("window.snap_left").enabled,
            "a stated master switch owns the whole list; the retired key is ignored"
        );
        assert!(row("quick.open_finder").enabled);
    }

    /// An explicit `items = []` stays empty through the fold.
    #[test]
    fn an_interim_empty_list_survives_the_group_fold() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[key_bindings]
window_control_enabled = false
quick_actions_enabled = true
items = []
"#,
        )
        .unwrap();
        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(errs.is_empty(), "{errs:?}");
        assert!(cfg.key_bindings.enabled);
        assert_eq!(cfg.key_bindings.items.as_deref(), Some(&[][..]));
    }

    /// A file that has the section (and its switch) but no list contributes only the list: the
    /// switch the user stated is kept.
    #[test]
    fn a_present_section_keeps_its_switch_and_only_seeds_the_list() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[window_control]
enabled = true

[key_bindings]
enabled = true
"#,
        )
        .unwrap();
        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(errs.is_empty(), "{errs:?}");
        assert!(
            cfg.key_bindings.enabled,
            "the section's own value wins over the legacy one"
        );
        assert_eq!(cfg.key_bindings.items.as_ref().unwrap().len(), 13);
    }

    /// A present `[key_bindings]` that omits `items` contributes only the list; the rows take the
    /// legacy flags (a legacy `up = false` must not come back enabled from the factory list).
    #[test]
    fn a_section_without_items_seeds_the_rows_from_the_legacy_flags() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[window_control]
enabled = true
up = false
left = false

[key_bindings]
enabled = true
"#,
        )
        .unwrap();
        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(errs.is_empty(), "{errs:?}");
        assert!(cfg.key_bindings.enabled);
        let row = |action: &str| {
            cfg.key_bindings
                .items
                .as_ref()
                .unwrap()
                .iter()
                .find(|item| item.action == action)
                .unwrap_or_else(|| panic!("{action} missing"))
        };
        assert!(
            !row("window.snap_up").enabled,
            "the legacy up = false survived"
        );
        assert!(!row("window.snap_left").enabled);
        assert!(row("window.snap_right").enabled);
        assert!(row("window.snap_down").enabled);
    }

    /// An explicitly empty list is a user who deleted every binding, not a missing list.
    #[test]
    fn an_explicitly_empty_list_stays_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[key_bindings]\nitems = []\n").unwrap();
        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(cfg.key_bindings.items.as_deref(), Some(&[][..]));
    }

    /// One bad entry is dropped with an error; its siblings survive, and the repair is persisted.
    #[test]
    fn one_bad_binding_never_discards_its_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[[key_bindings.items]]
action = "quick.open_finder"
trigger = "option+e"

[[key_bindings.items]]
action = "window.nope"
trigger = "option+j"

[[key_bindings.items]]
action = "quick.lock_screen"
trigger = "not-a-trigger"

[[key_bindings.items]]
action = "quick.open_settings"
trigger = "option+i"
"#,
        )
        .unwrap();
        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(errs
            .iter()
            .any(|error| error.starts_with("key_bindings.items[1].action")));
        assert!(errs
            .iter()
            .any(|error| error.starts_with("key_bindings.items[2].trigger")));
        let items = cfg.key_bindings.items.as_ref().unwrap();
        let actions: Vec<&str> = items.iter().map(|item| item.action.as_str()).collect();
        assert_eq!(actions, vec!["quick.open_finder", "quick.open_settings"]);
    }

    /// A purely semantic repair (an unknown action, which the type sanitizer cannot see) still has
    /// to be written back: otherwise the same error is reported on every launch and the repaired
    /// list in memory never reaches the disk.
    #[test]
    fn a_semantic_repair_is_persisted_even_without_a_type_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[[key_bindings.items]]
action = "window.nope"
trigger = "option+j"

[[key_bindings.items]]
action = "quick.open_finder"
trigger = "option+e"
"#,
        )
        .unwrap();
        let (_, errs) = Config::load_or_default_from(&path);
        assert!(
            errs.iter()
                .any(|error| error.starts_with("key_bindings.items[0].action")),
            "{errs:?}"
        );
        let persisted = std::fs::read_to_string(&path).unwrap();
        assert!(
            !persisted.contains("window.nope"),
            "the repaired list must be written back: {persisted}"
        );
        assert!(persisted.contains("quick.open_finder"), "{persisted}");
        // And reloading is clean: the repair does not repeat on every launch.
        let (_, again) = Config::load_or_default_from(&path);
        assert!(again.is_empty(), "{again:?}");
    }

    /// A wrong TOML type inside one entry is repaired to that field's default, so the entry is
    /// dropped by validation instead of inheriting the first seeded entry's action or chord.
    #[test]
    fn a_mistyped_field_inside_one_entry_does_not_inherit_the_first_seeded_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[[key_bindings.items]]
action = 123
trigger = "option+j"

[[key_bindings.items]]
action = "quick.lock_screen"
trigger = "option+l"
"#,
        )
        .unwrap();
        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(
            errs.iter()
                .any(|error| error.contains("key_bindings.items[0].action")),
            "{errs:?}"
        );
        let items = cfg.key_bindings.items.as_ref().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].action, "quick.lock_screen");
        assert_eq!(items[0].trigger, "option+l");
    }

    /// A wrong type for the whole list falls back to the field's default — the factory list — and
    /// says so, rather than taking the document down.
    #[test]
    fn a_mistyped_list_falls_back_to_the_factory_bindings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[key_bindings]\nitems = \"option+e\"\n").unwrap();
        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(
            errs.iter()
                .any(|error| error.starts_with("key_bindings.items")),
            "{errs:?}"
        );
        assert_eq!(cfg.key_bindings.items.as_ref().unwrap().len(), 13);
    }

    /// A non-table element keeps its slot while it is cleaned, so its siblings keep their original
    /// indices: the identity filter then drops the right entry instead of the one that slid into
    /// its place.
    #[test]
    fn a_malformed_element_does_not_shift_its_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // A malformed element is authored by hand (a bare key where a table belongs): the sanitizer
        // has to keep the slot so the two valid entries stay at their own indices.
        let authored = r#"
[[key_bindings.items]]
action = "quick.open_finder"
trigger = "option+e"

[[key_bindings.items]]
action = "quick.lock_screen"
trigger = "option+l"
"#;
        let mixed = authored.replace(
            "[[key_bindings.items]]\naction = \"quick.open_finder\"",
            "items_placeholder = true\n[[key_bindings.items]]\naction = \"quick.open_finder\"",
        );
        std::fs::write(&path, mixed).unwrap();
        let (cfg, errs) = Config::load_or_default_from(&path);
        let actions: Vec<&str> = cfg
            .key_bindings
            .items
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .map(|item| item.action.as_str())
            .collect();
        assert_eq!(
            actions,
            vec!["quick.open_finder", "quick.lock_screen"],
            "{errs:?}"
        );
    }

    /// A mistyped `enabled` is repaired on its own: the binding keeps its action and chord instead
    /// of being deleted with the field.
    #[test]
    fn a_mistyped_enabled_keeps_the_binding() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[[key_bindings.items]]
action = "quick.open_finder"
trigger = "option+e"
enabled = "yes"

[[key_bindings.items]]
action = "quick.lock_screen"
trigger = "option+l"
"#,
        )
        .unwrap();
        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(
            errs.iter()
                .any(|error| error.contains("key_bindings.items[0].enabled")),
            "{errs:?}"
        );
        let items = cfg.key_bindings.items.as_ref().unwrap();
        let actions: Vec<&str> = items.iter().map(|item| item.action.as_str()).collect();
        assert_eq!(actions, vec!["quick.open_finder", "quick.lock_screen"]);
        assert!(items[0].enabled, "the field falls back to its default");
    }

    /// When every entry is unusable the list stays empty: seeding the factory bindings would arm
    /// shortcuts the user may have deleted on purpose.
    #[test]
    fn an_entirely_unusable_list_does_not_resurrect_the_factory_bindings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
items = []

[[key_bindings.items]]
action = "window.nope"
trigger = "option+j"

[[key_bindings.items]]
action = "quick.open_finder"
trigger = "not-a-trigger"
"#,
        )
        .unwrap();
        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(errs
            .iter()
            .any(|error| error.starts_with("key_bindings.items[0].action")));
        assert_eq!(
            cfg.key_bindings.items.as_deref(),
            Some(&[][..]),
            "an emptied list stays empty"
        );
    }

    #[test]
    fn duplicate_triggers_keep_the_first_and_drop_the_later_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[[key_bindings.items]]
action = "quick.open_finder"
trigger = "option+e"

[[key_bindings.items]]
action = "quick.lock_screen"
trigger = "option+e"
"#,
        )
        .unwrap();
        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(errs
            .iter()
            .any(|error| error.starts_with("key_bindings.items[1].trigger")));
        let items = cfg.key_bindings.items.as_ref().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].action, "quick.open_finder");
    }

    /// Reserved chords are resolved against the effective config, so the check sees the switcher
    /// modifier and the clipboard shortcut as they will actually run.
    ///
    /// Precedence: the clipboard shortcut refuses a chord a binding already holds (so the clipboard
    /// falls back), while a binding that collides with the *switcher* chord is dropped. Both UIs
    /// refuse to create either pair, so this only settles a hand-edited file.
    #[test]
    fn reserved_chords_are_resolved_against_the_effective_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[keyboard]
modifier = "option"

[clipboard]
shortcut = "cmd+shift+v"

[[key_bindings.items]]
action = "quick.open_finder"
trigger = "option+tab"

[[key_bindings.items]]
action = "quick.lock_screen"
trigger = "cmd+shift+v"

[[key_bindings.items]]
action = "quick.open_settings"
trigger = "cmd+tab"
"#,
        )
        .unwrap();
        let (cfg, errs) = Config::load_or_default_from(&path);
        // The clipboard shortcut collided with a binding, so it took its default; the binding that
        // held the chord survives. Option+Tab is the switcher chord here, so that binding is the
        // one that yields.
        assert!(errs
            .iter()
            .any(|error| error.starts_with("clipboard.shortcut")));
        assert_eq!(cfg.clipboard.shortcut, "option+v");
        let actions: Vec<&str> = cfg
            .key_bindings
            .items
            .as_ref()
            .unwrap()
            .iter()
            .map(|item| item.action.as_str())
            .collect();
        assert_eq!(actions, vec!["quick.lock_screen", "quick.open_settings"]);
        assert_eq!(errs.len(), 2, "{errs:?}");
    }

    /// The reviewer's case: an invalid clipboard shortcut falls back to its default, and a binding
    /// that only collides with that *fallback* value is still caught — the check runs after
    /// per-field fallback, not against the value the file stated.
    #[test]
    fn a_binding_on_the_fallback_clipboard_chord_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[clipboard]
shortcut = "not-a-key"

[[key_bindings.items]]
action = "quick.open_finder"
trigger = "option+v"

[[key_bindings.items]]
action = "quick.lock_screen"
trigger = "option+l"
"#,
        )
        .unwrap();
        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(errs
            .iter()
            .any(|error| error.starts_with("clipboard.shortcut")));
        assert_eq!(cfg.clipboard.shortcut, "option+v");
        let actions: Vec<&str> = cfg
            .key_bindings
            .items
            .as_ref()
            .unwrap()
            .iter()
            .map(|item| item.action.as_str())
            .collect();
        assert_eq!(actions, vec!["quick.lock_screen"], "{errs:?}");
        assert!(errs
            .iter()
            .any(|error| error.starts_with("key_bindings.items[0].trigger")));
    }

    /// The exact three-way permutation the review named: an invalid `keyboard.modifier` falls back
    /// to Command, the clipboard shortcut the file stated collides with the switcher and falls back
    /// to Option+V, and the binding that holds Option+V must then yield to that fallback.
    #[test]
    fn an_invalid_modifier_plus_a_clipboard_conflict_still_drops_the_binding() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[keyboard]
modifier = "ctrl"

[clipboard]
shortcut = "cmd+tab"

[[key_bindings.items]]
action = "quick.open_finder"
trigger = "option+v"

[[key_bindings.items]]
action = "quick.lock_screen"
trigger = "option+l"
"#,
        )
        .unwrap();
        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(errs
            .iter()
            .any(|error| error.starts_with("keyboard.modifier")));
        assert_eq!(cfg.keyboard.modifier, "command");
        assert_eq!(cfg.clipboard.shortcut, "option+v");
        let actions: Vec<&str> = cfg
            .key_bindings
            .items
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .map(|item| item.action.as_str())
            .collect();
        assert_eq!(
            actions,
            vec!["quick.lock_screen"],
            "the binding on the fallback clipboard chord yields: {errs:?}"
        );
    }

    /// A clipboard shortcut that only conflicts after per-field fallback is caught too: the
    /// invalid `keyboard.modifier` falls back to Command, which makes cmd+tab the switcher chord.
    #[test]
    fn a_clipboard_shortcut_that_conflicts_only_after_fallback_is_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[keyboard]
modifier = "ctrl"

[clipboard]
shortcut = "cmd+tab"
"#,
        )
        .unwrap();
        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(errs
            .iter()
            .any(|error| error.starts_with("keyboard.modifier")));
        assert_eq!(cfg.keyboard.modifier, "command");
        assert!(
            errs.iter()
                .any(|error| error.starts_with("clipboard.shortcut")),
            "{errs:?}"
        );
        assert_eq!(cfg.clipboard.shortcut, "option+v");
    }

    /// Switching the app switcher to another modifier at runtime can hand it a chord a binding
    /// holds; the binding yields, and the repair reports it.
    #[test]
    fn switching_the_switcher_modifier_drops_the_binding_that_would_own_its_chord() {
        let mut cfg = Config::default();
        cfg.keyboard.modifier = "option".into();
        cfg.key_bindings.items = Some(vec![crate::key_bindings::KeyBinding {
            action: "quick.open_finder".into(),
            trigger: "cmd+tab".into(),
            enabled: true,
        }]);
        // Option+Tab is the switcher chord here, so a Command+Tab binding is free.
        assert!(cfg.repair_key_binding_conflicts().is_empty());

        cfg.keyboard.modifier = "command".into();
        let conflicts = cfg.repair_key_binding_conflicts();
        assert_eq!(conflicts.len(), 1, "{conflicts:?}");
        assert!(conflicts[0].starts_with("key_bindings.items[0].trigger"));
        assert_eq!(cfg.key_bindings.items.as_deref(), Some(&[][..]));
    }

    /// The clipboard yields when the switcher takes its chord: switching the switcher to Command
    /// makes a Command+Tab clipboard shortcut unusable, and the clipboard is the side that moves.
    #[test]
    fn a_switcher_change_makes_the_clipboard_shortcut_yield() {
        let mut cfg = Config::default();
        cfg.keyboard.modifier = "option".into();
        cfg.clipboard.shortcut = "cmd+tab".into();
        assert!(cfg.settle_reserved_shortcuts().is_empty());
        assert_eq!(
            cfg.clipboard.shortcut, "cmd+tab",
            "free while Option+Tab is the switcher"
        );

        cfg.keyboard.modifier = "command".into();
        let errors = cfg.settle_reserved_shortcuts();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].starts_with("clipboard.shortcut"));
        assert_eq!(cfg.clipboard.shortcut, "option+v");
    }

    /// A restored binding that takes the clipboard's chord makes the clipboard move, and the
    /// binding survives -- the same precedence a load applies.
    #[test]
    fn a_restored_binding_takes_the_chord_from_the_clipboard() {
        let mut cfg = Config::default();
        cfg.clipboard.shortcut = "option+e".into();
        cfg.key_bindings.items = Some(vec![crate::key_bindings::KeyBinding {
            action: "quick.open_finder".into(),
            trigger: "option+e".into(),
            enabled: true,
        }]);
        let errors = cfg.settle_reserved_shortcuts();
        assert!(errors.iter().any(|e| e.starts_with("clipboard.shortcut")));
        assert_eq!(cfg.clipboard.shortcut, "option+v");
        assert_eq!(
            cfg.key_bindings.items.as_deref().map(<[_]>::len),
            Some(1),
            "the binding keeps its chord"
        );
    }

    /// The other direction: a binding that takes the *default* clipboard chord yields, with one
    /// error rather than a no-op clipboard fallback plus a dropped binding.
    #[test]
    fn a_binding_on_the_default_clipboard_chord_yields_on_its_own() {
        let mut cfg = Config::default();
        cfg.clipboard.shortcut = "option+v".into();
        cfg.key_bindings.items = Some(vec![crate::key_bindings::KeyBinding {
            action: "quick.open_finder".into(),
            trigger: "option+v".into(),
            enabled: true,
        }]);
        let errors = cfg.settle_reserved_shortcuts();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].starts_with("key_bindings.items[0].trigger"));
        assert_eq!(cfg.key_bindings.items.as_deref(), Some(&[][..]));
        assert_eq!(cfg.clipboard.shortcut, "option+v");
    }

    #[test]
    fn bindings_round_trip_through_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut cfg = Config::default();
        cfg.key_bindings.enabled = true;
        cfg.key_bindings.items = Some(vec![
            crate::key_bindings::KeyBinding {
                action: "quick.open_finder".into(),
                trigger: "option+g".into(),
                enabled: true,
            },
            crate::key_bindings::KeyBinding {
                action: "window.center".into(),
                trigger: "cmd+ctrl+c".into(),
                enabled: false,
            },
        ]);
        cfg.save_to(&path).unwrap();

        let (loaded, errs) = Config::load_or_default_from(&path);
        assert!(errs.is_empty(), "{errs:?}");
        assert!(loaded.key_bindings.enabled);
        let items = loaded.key_bindings.items.as_ref().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].trigger, "option+g");
        assert_eq!(items[1].action, "window.center");
        assert!(!items[1].enabled);
    }

    /// Bindings reserve their chords against the clipboard shortcut in both directions.
    #[test]
    fn a_clipboard_shortcut_that_collides_with_a_binding_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[clipboard]
shortcut = "option+i"

[[key_bindings.items]]
action = "quick.open_settings"
trigger = "option+i"
"#,
        )
        .unwrap();
        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(
            errs.iter()
                .any(|error| error.starts_with("clipboard.shortcut")),
            "{errs:?}"
        );
        assert_eq!(cfg.clipboard.shortcut, "option+v");
        // The binding itself is untouched: the clipboard shortcut is what loses.
        assert_eq!(
            cfg.key_bindings.items.as_ref().unwrap()[0].trigger,
            "option+i"
        );
    }
    /// A config file that cannot be parsed must not read as a healthy launch.
    ///
    /// Why this is a regression test: the loader discarded its own errors, and run() then
    /// re-validated the already-repaired config, so a syntax error produced an **empty** report and
    /// the app silently ran on defaults for every field -- shortcut back to Cmd+Tab, clipboard off,
    /// theme default -- with nothing naming the file. These assertions pin the loader's half: the
    /// error survives the call, and the per-field fallback still happens for one invalid value.
    #[test]
    fn a_syntax_error_is_reported_instead_of_disappearing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "this is [ not toml\n=== broken").unwrap();
        let (_cfg, errs) = Config::load_or_default_from(&path);
        assert!(
            !errs.is_empty(),
            "a file that cannot be parsed must produce at least one error for the startup report"
        );
    }

    #[test]
    fn an_invalid_field_falls_back_without_discarding_its_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // Distinct non-default siblings, so a whole-section reset is visible rather than hidden
        // by the defaults matching the expected answer.
        let content = [
            "[layout]",
            "thumbnail_size = \"enormous\"",
            "card_text_size = 14.0",
            "",
            "[keyboard]",
            "modifier = \"option\"",
            "",
        ]
        .join("\n");
        std::fs::write(&path, content).unwrap();
        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(
            !errs.is_empty(),
            "the invalid thumbnail size must be reported"
        );
        assert_eq!(
            cfg.layout.thumbnail_size,
            crate::config::Layout::default().thumbnail_size,
            "the invalid field falls back to its default"
        );
        assert_eq!(
            cfg.layout.card_text_size, 14.0,
            "a valid sibling keeps its value"
        );
        assert_eq!(
            cfg.keyboard.modifier, "option",
            "another section keeps its value"
        );
    }
    /// The per-field fallback promise, tested through the real load path rather than a list of keys.
    ///
    /// Why one test per section instead of a registry: `validate` and `merge_valid` are joined by error
    /// message prefixes, and a second list of keys would check the classifier rather than the promise.
    /// Each case below writes a file with an INVALID value for one field and legal, NON-DEFAULT values
    /// for its siblings, then asserts that only the invalid field fell back. Non-default siblings are
    /// what make a whole-section reset visible -- with defaults on both sides it cannot be seen.
    #[test]
    fn an_invalid_field_falls_back_without_taking_its_section_with_it() {
        fn load(content: &str) -> Config {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.toml");
            std::fs::write(&path, content).unwrap();
            Config::load_or_default_from(&path).0
        }

        // layout: one invalid size, a valid non-default sibling in the same section.
        let cfg = load(
            &[
                "[layout]",
                "thumbnail_size = \"enormous\"",
                "card_text_size = 14.0",
                "",
            ]
            .join("\n"),
        );
        assert_eq!(
            cfg.layout.thumbnail_size,
            Layout::default().thumbnail_size,
            "the invalid field falls back to its default"
        );
        assert_eq!(
            cfg.layout.card_text_size, 14.0,
            "a valid sibling keeps its value: the section was not reset wholesale"
        );

        // appearance: an invalid enum, a valid non-default sibling.
        let cfg = load(
            &[
                "[appearance]",
                "theme = \"purple\"",
                "corner_radius = 9.0",
                "",
            ]
            .join("\n"),
        );
        assert_eq!(cfg.appearance.theme, Appearance::default().theme);
        assert_eq!(
            cfg.appearance.corner_radius, 9.0,
            "a valid sibling keeps its value"
        );

        // keyboard: an invalid modifier, a valid non-default sibling in another section.
        let cfg = load(
            &[
                "[keyboard]",
                "modifier = \"hyper\"",
                "",
                "[layout]",
                "card_text_size = 13.0",
                "",
            ]
            .join("\n"),
        );
        assert_eq!(cfg.keyboard.modifier, Keyboard::default().modifier);
        assert_eq!(
            cfg.layout.card_text_size, 13.0,
            "other sections keep theirs"
        );
    }
}
