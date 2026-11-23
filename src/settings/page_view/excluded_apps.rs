//! The excluded-applications card, shared by every page that owns a bundle-id exclusion list.
//!
//! Two pages present the same card -- the Gestures page ("Applications where no gesture fires") and
//! the Switcher page ("Excluded from the switcher") -- so its shape lives here once: an "Add an
//! application" command that opens the system chooser, a line stating entries that can never match,
//! the empty state, one row per entry (its icon, its name, its Remove action), and a "+N" row for
//! the entries past [`MAX_SHOWN_EXCLUDED`] that are edited in the config file. The pages differ only
//! in copy keys, action ids and the static row identities their control table addresses.
//!
//! **The description stays pure.** It never scans the disk and never asks AppKit: display names
//! arrive through [`PageInput::known_app_names`], which the settings root prepares on the main thread
//! once per build (`refresh_known_app_names`).

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use super::*;

/// The most entries the card lists before the "+N" row states the rest. A hand-written row-identity
/// table cannot grow, so this is also the page's identity bound.
pub(in crate::settings) const MAX_SHOWN_EXCLUDED: usize = 16;

/// Everything that makes one page's exclusion card its own: copy keys, action ids and the static
/// row identities the control table addresses.
pub(super) struct ExcludedAppsCardSpec {
    pub(super) header_key: &'static str,
    pub(super) desc_key: &'static str,
    pub(super) add_row_key: &'static str,
    pub(super) add_action_id: &'static str,
    pub(super) add_action_key: &'static str,
    pub(super) invalid_key: &'static str,
    pub(super) invalid_identity: &'static str,
    pub(super) empty_key: &'static str,
    pub(super) empty_identity: &'static str,
    pub(super) row_label_key: &'static str,
    pub(super) remove_action_id: &'static str,
    pub(super) remove_action_key: &'static str,
    pub(super) more_key: &'static str,
    pub(super) more_identity: &'static str,
    /// One identity per row slot, in list order.
    pub(super) identities: &'static [&'static str],
}

/// Append `spec`'s card for `entries`, labelling each entry with its application name.
pub(super) fn push_excluded_apps_card(
    w: &mut RowWriter,
    spec: &ExcludedAppsCardSpec,
    entries: &[String],
    known_names: &HashMap<String, String>,
) {
    w.card(spec.header_key);
    // Adding one is a COMMAND, not a selection: it opens the system's own application chooser.
    w.push(
        RowDraft::new(
            spec.add_row_key,
            ControlKind::Value {
                action: Some(ValueAction::new(spec.add_action_id, spec.add_action_key)),
                align: ValueAlign::Right,
            },
            ControlId::Action(spec.add_action_id),
            Value::None,
        )
        .desc(spec.desc_key)
        .into_spec(),
    );

    // A list whose entries cannot match: the matcher compares bundle identifiers, so an entry that
    // is not one never excludes anything the user can see. Saying so is the difference between "the
    // setting is broken" and "this line is wrong".
    let unusable = entries
        .iter()
        .filter(|entry| !looks_like_bundle_id(entry))
        .count();
    if unusable > 0 {
        w.push(
            RowDraft::new(
                spec.invalid_key,
                ControlKind::Value {
                    action: None,
                    align: ValueAlign::Right,
                },
                ControlId::Action(spec.invalid_identity),
                Value::Dynamic(format!("{unusable}")),
            )
            .into_spec(),
        );
    }

    // One row per excluded bundle, each with its own Remove action.
    if entries.is_empty() {
        w.push(
            RowDraft::new(
                spec.empty_key,
                ControlKind::Value {
                    action: None,
                    align: ValueAlign::Right,
                },
                ControlId::Action(spec.empty_identity),
                Value::None,
            )
            .into_spec(),
        );
    }
    for (index, bundle) in entries.iter().take(MAX_SHOWN_EXCLUDED).enumerate() {
        let Some(identity) = spec.identities.get(index).copied() else {
            // A table shorter than the bound is a page that cannot name its rows; the bound above
            // and the tables are asserted to agree, so this would be a build bug, not user data.
            debug_assert!(
                false,
                "exclusion card identity table shorter than MAX_SHOWN_EXCLUDED"
            );
            break;
        };
        // The row's LABEL is the application: a generic "Application" title beside a truncated
        // identifier said nothing about which application is excluded. The bundle id is the
        // fallback, not the default -- an entry can name an application that is no longer installed,
        // and it still has to be listed and removable.
        let label = known_names
            .get(bundle)
            .cloned()
            .unwrap_or_else(|| bundle.clone());
        w.push(
            RowDraft::new(
                spec.row_label_key,
                ControlKind::Value {
                    // One action identity for every Remove button. WHICH entry is removed travels on
                    // the row spec below (the bundle id), because the callback receives the button
                    // and its own text is only its title.
                    action: Some(ValueAction::new(
                        spec.remove_action_id,
                        spec.remove_action_key,
                    )),
                    align: ValueAlign::Right,
                },
                ControlId::Action(identity),
                // The value beside the label stays empty: the name IS the label now.
                Value::None,
            )
            .label_text(label)
            // The icon is the row's shortest answer to "which application is this": a name alone
            // has to be read, a Dock icon is recognised. An entry whose application is gone draws no
            // icon and keeps its name (or its identifier) as plain text.
            .icon_bundle(bundle.clone())
            // The handler removes this bundle id, never "whatever is at row N".
            .action_bundle(bundle.clone())
            .into_spec(),
        );
    }

    // The list has the same identity bound as the gesture rows; the overflow is stated rather than
    // collapsed onto one shared identity. The row's copy says the rest are edited in the config
    // file, which is where they are removed.
    let overflow = entries.len().saturating_sub(MAX_SHOWN_EXCLUDED);
    if overflow > 0 {
        w.push(
            RowDraft::new(
                spec.more_key,
                ControlKind::Value {
                    action: None,
                    align: ValueAlign::Right,
                },
                ControlId::Action(spec.more_identity),
                Value::Dynamic(format!("+{overflow}")),
            )
            .into_spec(),
        );
    }
}

/// Whether an exclusion entry can ever match: the matcher compares it with an application's bundle
/// identifier (`CFBundleIdentifier`, reverse-DNS, at least two dot-separated parts and no spaces).
///
/// An entry that is not a bundle id -- an application NAME, a path, an empty line edited into the
/// file -- is accepted by the config but can never be equal to one, so the runtime ignores it. This
/// is the same predicate the cards use to mark such entries, and it is deliberately conservative: a
/// real bundle id always passes, and a non-conforming but genuinely registered identifier (an Apple
/// utility from an unsigned bundle) is reported rather than silently kept.
pub(in crate::settings) fn looks_like_bundle_id(entry: &str) -> bool {
    let trimmed = entry.trim();
    if trimmed.is_empty() || trimmed.contains(char::is_whitespace) {
        return false;
    }
    // No scheme, no slash: a bundle id is dots and ASCII name characters.
    if trimmed.contains('/') || trimmed.contains(':') {
        return false;
    }
    let parts: Vec<&str> = trimmed.split('.').collect();
    parts.len() >= 2
        && parts.iter().all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
}

/// Display names for bundle identifiers, prepared once per settings build.
///
/// Cached rather than recomputed per page: the scan reads the application folders and asks AppKit
/// for the running set, and a settings build describes nine pages. A lazy first use keeps a headless
/// test or a smoke runner working without an explicit refresh.
/// The cached name table: bundle id -> the name a person recognises.
type KnownAppNames = Arc<HashMap<String, String>>;

static KNOWN_APP_NAMES: LazyLock<Mutex<Option<KnownAppNames>>> = LazyLock::new(|| Mutex::new(None));

/// The cached display-name table (computed on first use).
pub(in crate::settings) fn known_app_names() -> Arc<HashMap<String, String>> {
    let mut slot = KNOWN_APP_NAMES.lock().unwrap();
    if slot.is_none() {
        *slot = Some(Arc::new(collect_known_app_names()));
    }
    slot.as_ref().expect("the slot was just filled").clone()
}

/// Re-read the display-name table. Called once per settings content rebuild, so a newly installed or
/// uninstalled application is named correctly the next time the page is drawn.
pub(in crate::settings) fn refresh_known_app_names() {
    *KNOWN_APP_NAMES.lock().unwrap() = Some(Arc::new(collect_known_app_names()));
}

fn collect_known_app_names() -> HashMap<String, String> {
    let mut names: HashMap<String, String> = HashMap::new();
    for (bundle, name) in crate::app_identity::installed_application_bundles() {
        names.insert(bundle, name);
    }
    // Reading the running-application list is AppKit work, and a settings build runs on the main
    // thread. Running names win for an application installed outside the standard folders.
    for (bundle, name) in unsafe { crate::app_identity::running_apps_with_bundle_ids() } {
        names.entry(bundle).or_insert(name);
    }
    names
}
