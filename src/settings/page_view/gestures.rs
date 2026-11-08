//! One settings page as data: the mouse-gesture rows, their values and their identities.
//!
//! A page module is pure: it reads `PageInput`, writes `RowSpec`s through `RowWriter` and returns a
//! `PageView`. It creates no view, reads no global and records no search index -- the shared
//! renderer does all of that from the description.
//!
//! The page is its own sidebar entry rather than a card on the Mouse page: a gesture is a whole
//! input path (a sequence, a button and a target), and burying it under smooth-scrolling tuning
//! would make it look like another scroll option.
//!
//! The list is built from the CONFIGURATION, not from the runtime table: the table decides whether a
//! press is taken and is empty whenever the master switches are off, so a page built from it would
//! show nothing exactly when the user wants to look at what they configured.
//!
//! Every row is two columns, the way a gesture manager reads: the stroke (its arrows and, when the
//! user has not picked a picture for it, its letters) on the left, and the action it runs in a popup
//! on the right.

use super::*;

/// The page's master switch: it owns whether any gesture fires.
pub(super) const GESTURES_ENABLED_FIELD: ControlField = ControlField::GesturesEnabled;

/// The trigger choices, in the order the popup lists them: the buttons a mouse has, and nothing
/// else -- the left button stays out on purpose, because a gesture on it would swallow every click.
pub(crate) const GESTURE_TRIGGER_BUTTONS: [u32; 4] = [1, 2, 3, 4];

/// One configured gesture, as the page shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ShownRow {
    /// The row's position in the config list, and its stable identity.
    index: usize,
    /// The sequence a user would say ("RD"), or the raw text when it does not parse.
    sequence: String,
    /// The trigger button in the user's words.
    button: String,
    /// The trigger button as the configuration holds it; the row's trigger popup reopens the list
    /// from it, and the dispatcher matches it against the option table.
    trigger: u32,
    /// What it runs, in the user's words.
    target: String,
    /// The binding as the configuration holds it: the popup's selection is matched against this,
    /// not against the words the row shows.
    binding: String,
    /// Why the row will not run, when it will not.
    problem: Option<&'static str>,
}

/// Whether a gesture row can be edited on the page.
///
/// The list is fixed for now: every default gesture is configured in code, a row a user recorded
/// cannot be acted on yet, and the only writer of `mouse.gestures` in the settings UI is the
/// recorder's own "add" step. So the page LISTS the rows and does not edit them -- no action popup,
/// no trigger choice, no delete -- which is why it draws them as plain values.
pub(super) const GESTURE_ROWS_EDITABLE: bool = false;

/// The page's description: its rows, their values and their identities.
pub(super) fn gestures_page_view(input: &PageInput) -> PageView {
    // A row's action popup puts the page into that row's TRIGGER row: the same gesture, listed once
    // per button it can start on. It is a page of its own rather than a nested list because the
    // control table is what the dispatcher reads a clicked line back from.
    if let Some(index) = crate::settings::open_gesture_trigger_row() {
        if index < input.config.mouse.gestures.len() {
            return trigger_page_view(index, &input.config);
        }
        // The list shrank under the open trigger row (a recording was added, a row was removed):
        // fall back to the list rather than showing a row that no longer exists.
        crate::settings::set_open_gesture_trigger_row(None);
    }
    let cfg = &input.config;
    let mut w = RowWriter::new(SettingsPageId::Gestures, t("settings.sidebar_gestures"));

    w.in_first_card("settings.gestures_header");
    w.push(
        switch(
            "settings.gestures_row_enabled",
            GESTURES_ENABLED_FIELD,
            cfg.mouse.gestures_enabled,
        )
        .desc("settings.gestures_desc_enabled")
        .wide_caption()
        .into_spec(),
    );

    // Two presentation switches, beside the master one: a user who has just turned gestures on needs
    // to SEE the trail to trust the feature, and the hint comes with it. Both are theirs to turn off.
    w.push(
        switch(
            "settings.gestures_row_show_trail",
            ControlField::GestureShowTrail,
            cfg.mouse.gesture_show_trail,
        )
        .desc("settings.gestures_desc_show_trail")
        .into_spec(),
    );
    w.push(
        switch(
            "settings.gestures_row_show_hint",
            ControlField::GestureShowHint,
            cfg.mouse.gesture_show_hint,
        )
        .desc("settings.gestures_desc_show_hint")
        .into_spec(),
    );

    // The one button every gesture starts on. The list itself is one button's list, so the rows
    // below say only what they run, and this row is the single place the button is chosen -- it
    // sits in the switches' own card, where a trigger belongs to that set of choices.
    w.card("settings.gestures_header");
    let trigger_choices = trigger_options();
    let trigger_selected = trigger_choices
        .iter()
        .position(|option| option.button == cfg.mouse.gesture_button)
        .unwrap_or(0);
    w.push(
        RowDraft::new(
            "settings.gestures_row_gesture_button",
            ControlKind::Popup {
                options: trigger_choices
                    .iter()
                    .map(|option| option.label.clone())
                    .collect(),
            },
            ControlId::Field(ControlField::GestureTriggerButton),
            Value::Index(trigger_selected),
        )
        .desc("settings.gestures_desc_trigger")
        .into_spec(),
    );

    w.card("settings.gestures_header_list");
    if GESTURE_ROWS_EDITABLE {
        // The recorder row: its value IS the recording's state, so the page follows the pointer while
        // the user draws. The button beside it starts a recording, and cancels one already running.
        // The state text is the same pure description the in-place refresh writes, so the row a user
        // sees after a rebuild and the row a live preview rewrites cannot disagree.
        let state = crate::settings::gesture_recorder::record_row_text(
            crate::mouse::gesture::recording::stage(),
            &crate::settings::gesture_recorder::preview_text(),
        );
        // ONE action for the row, whatever the state: a row's action identity is fixed when the page
        // is built, so a live row cannot switch which handler it runs without a rebuild -- and
        // rebuilding during a capture is wrong. The handler reads the recording state and starts or
        // cancels.
        let action = crate::settings::page_view::GESTURE_RECORD_ROW_ACTION;
        w.push(
            RowDraft::new(
                "settings.gestures_row_record",
                ControlKind::Value {
                    action: Some(ValueAction::new(action, "settings.gestures_record_action")),
                    align: ValueAlign::Right,
                },
                ControlId::Action(action),
                Value::Dynamic(state),
            )
            .desc("settings.gestures_desc_record")
            .into_spec(),
        );
    }

    // The control table addresses a row by its identity, and the identity is a `&'static str`, so the
    // page can only show a bounded number of rows. That bound is stated rather than hidden: a
    // configuration with more rows than this gets an explicit "and N more" line instead of two rows
    // silently sharing one identity (which the table would refuse, failing the whole page build).
    let all_rows = configured_rows(cfg);
    let overflow = all_rows.len().saturating_sub(MAX_SHOWN_ROWS);
    let rows: Vec<ShownRow> = all_rows.into_iter().take(MAX_SHOWN_ROWS).collect();
    if rows.is_empty() {
        w.push(
            RowDraft::new(
                "settings.gestures_row_empty",
                ControlKind::Value {
                    action: None,
                    align: ValueAlign::Right,
                },
                ControlId::Action("gesture_list_empty"),
                Value::None,
            )
            .into_spec(),
        );
    }
    // Conflicts are reported ON the row they concern: the user is looking at the list, and a
    // separate summary would make them hunt for which row is wrong.
    let conflicts = crate::mouse::gesture::conflict::by_row(cfg);
    let options = target_options();
    for row in rows {
        let selected = options
            .iter()
            .position(|option| option.id == row.binding)
            .unwrap_or(0);
        // The stroke's arrows and its name in words, with the state it will not run in when there is
        // one. The button is not part of the row: it is the page's one trigger, chosen above.
        let mut stroke = stroke_words(&row.sequence);
        if let Some(problem) = row.problem {
            let label = problem_label(problem);
            if !label.is_empty() {
                stroke.push_str(" · ");
                stroke.push_str(&label);
            }
        }
        if let Some(conflict) = conflicts.get(&row.index) {
            stroke.push_str(" · ");
            stroke.push_str(&t(conflict.message));
        }
        if !GESTURE_ROWS_EDITABLE {
            // A fixed row: the stroke on the left, what it runs on the right, nothing to press.
            w.push(
                RowDraft::new(
                    "settings.gestures_row_gesture",
                    ControlKind::Value {
                        action: None,
                        align: ValueAlign::Right,
                    },
                    ControlId::Action(row_identity(row.index)),
                    Value::Dynamic(row.target.clone()),
                )
                .label_text(stroke)
                .into_spec(),
            );
            continue;
        }
        w.push(
            RowDraft::new(
                // The key stays the row's class (the search index and the tests match it); the
                // stroke itself is DATA -- arrows or the letters the user drew -- so it travels as
                // the row's label text rather than as a locale key.
                "settings.gestures_row_gesture",
                ControlKind::Popup {
                    options: options.iter().map(|option| option.label.clone()).collect(),
                },
                // Every row needs its OWN identity: the control table refuses a duplicate, and a
                // refused registration fails the whole page build.
                ControlId::Action(row_identity(row.index)),
                Value::Index(selected),
            )
            .label_text(stroke)
            .action_tag(row.index as isize)
            // Delete is parked until the renderer draws a popup row action button: the row action
            // button is placed from a Value row today, so a popup row drew nothing pressable.
            // The action popup also carries the row's trigger: its submenu lists the buttons the
            // gesture can start on, and picking one replaces the page with that row's trigger row.
            // (The submenu entries themselves are informational: the row's own action reopens the
            // trigger row, because the click lands on the row, not on a submenu entry.)
            .into_spec(),
        );
    }
    // The rows past the identity bound are stated at the END of the gesture list, where the reader
    // looks for them: drawn after the exclusion card it read as part of that list, "+3" with no
    // gesture in sight.
    if overflow > 0 {
        w.push(
            RowDraft::new(
                "settings.gestures_row_more",
                ControlKind::Value {
                    action: None,
                    align: ValueAlign::Right,
                },
                ControlId::Action("gesture_rows_more"),
                Value::Dynamic(format!("+{overflow}")),
            )
            .into_spec(),
        );
    }
    // Applications where no gesture fires.
    //
    // Adding one is a COMMAND, not a selection: it opens the system's own application chooser. It
    // used to be a dropdown of every installed application (137 on a normal Mac, no search), which
    // made a one-shot action look like a setting that holds a value and then had to reset itself.
    // `settings.gestures_desc_excluded` still states the rule the list implements.
    w.card("settings.gestures_header_excluded");
    w.push(
        RowDraft::new(
            "settings.gestures_row_add_app",
            ControlKind::Value {
                action: Some(ValueAction::new(
                    "gesture_excluded_choose",
                    "settings.gestures_excluded_choose_action",
                )),
                align: ValueAlign::Right,
            },
            ControlId::Action("gesture_excluded_choose"),
            Value::None,
        )
        .desc("settings.gestures_desc_excluded")
        .into_spec(),
    );

    // A list whose entries cannot match: the tap matches a bundle id, so an entry that is not one
    // never excludes anything the user can see. Saying so is the difference between "the setting is
    // broken" and "this line is wrong".
    let unusable = cfg
        .mouse
        .gesture_excluded_apps
        .iter()
        .filter(|entry| !looks_like_bundle_id(entry))
        .count();
    if unusable > 0 {
        w.push(
            RowDraft::new(
                "settings.gestures_row_excluded_invalid",
                ControlKind::Value {
                    action: None,
                    align: ValueAlign::Right,
                },
                ControlId::Action("gesture_excluded_invalid"),
                Value::Dynamic(format!("{unusable}")),
            )
            .into_spec(),
        );
    }

    // One row per excluded bundle, each with its own Remove action.
    if cfg.mouse.gesture_excluded_apps.is_empty() {
        w.push(
            RowDraft::new(
                "settings.gestures_row_excluded_empty",
                ControlKind::Value {
                    action: None,
                    align: ValueAlign::Right,
                },
                ControlId::Action("gesture_excluded_empty"),
                Value::None,
            )
            .into_spec(),
        );
    }
    let excluded_total = cfg.mouse.gesture_excluded_apps.len();
    // The names of everything the picker knows, so a listed bundle can be shown under the name the
    // user recognises instead of its reverse-DNS identifier.
    let display_names: HashMap<String, String> =
        gesture_excluded_candidates().into_iter().collect();
    for (index, bundle) in cfg
        .mouse
        .gesture_excluded_apps
        .iter()
        .take(MAX_SHOWN_EXCLUDED)
        .enumerate()
    {
        let id = excluded_row_identity(index);
        // The row's LABEL is the application, the way the gesture rows label themselves with the
        // stroke they hold: a generic "Application" title beside a truncated identifier said nothing
        // about which applications are excluded. The bundle id is the fallback, not the default --
        // an entry can name an application that is no longer installed, and it still has to be
        // listed and removable.
        let label = display_names
            .get(bundle)
            .cloned()
            .unwrap_or_else(|| bundle.clone());
        w.push(
            RowDraft::new(
                "settings.gestures_row_excluded",
                ControlKind::Value {
                    // One action identity for every Remove button. WHICH entry is removed travels
                    // as the button's tag below, because the callback receives the button and its
                    // own text is only its title.
                    action: Some(ValueAction::new(
                        "gesture_excluded_remove",
                        "settings.gestures_excluded_remove",
                    )),
                    align: ValueAlign::Right,
                },
                ControlId::Action(id),
                // The value beside the label stays empty: the name IS the label now.
                Value::None,
            )
            .label_text(label)
            // The icon is the row's shortest answer to "which application is this": a name alone
            // has to be read, a Dock icon is recognised. An entry whose application is gone draws
            // no icon and keeps its name (or its identifier) as plain text.
            .icon_bundle(bundle.clone())
            .action_tag(index as isize)
            .into_spec(),
        );
    }

    // The exclusion list has the same identity bound as the gesture rows; the overflow is
    // stated rather than collapsed onto one shared identity.
    let excluded_overflow = excluded_total.saturating_sub(MAX_SHOWN_EXCLUDED);
    if excluded_overflow > 0 {
        w.push(
            RowDraft::new(
                "settings.gestures_row_more",
                ControlKind::Value {
                    action: None,
                    align: ValueAlign::Right,
                },
                ControlId::Action("gesture_excluded_more"),
                Value::Dynamic(format!("+{excluded_overflow}")),
            )
            .into_spec(),
        );
    }
    w.finish()
}

/// Whether a row identity names one of this page's gesture rows.
///
/// The dispatcher has to answer that from an identity alone, because the control is all it holds:
/// a popup that edits a gesture must not be mistaken for another page's action row.
pub(crate) fn is_gesture_row_identity(identity: &str) -> bool {
    (0..MAX_SHOWN_ROWS).any(|index| row_identity(index) == identity)
        || identity == "gesture_row_over"
}

/// Whether an identity names a trigger popup. Those report a BUTTON, not an action, so the target
/// popup handler must not read them as one.
pub(crate) fn is_trigger_row_identity(identity: &str) -> bool {
    trigger_row_of(identity).is_some()
}

/// The binding ids of the action popup, in the order its entries are listed.
///
/// The page matches a row's binding against this list to choose the selected entry, and the
/// dispatcher reads the CHOSEN entry back through it, so the two cannot disagree about what an
/// index means.
pub(crate) fn gesture_target_option_ids() -> Vec<String> {
    target_options()
        .into_iter()
        .map(|option| option.id)
        .collect()
}

/// The identity of a gesture control: the row itself, or one button line of its trigger row.
///
/// The action path and the trigger path answer different questions about the same row, so their
/// identities are spelled apart: the row's own popup names what the gesture RUNS, while the trigger
/// row that replaces the page names the button it starts on.
fn gesture_control_identity(index: usize, trigger: Option<usize>) -> &'static str {
    // One identity list, generated from one alphabet, so the page's row bound cannot outgrow it:
    // a hand-written table long enough for today's rows is a page that fails to build on the
    // first row it cannot name, and a row that silently shares another's identity.
    const ACTIONS: [&str; MAX_IDENTITY_SLOTS] = IDENTITY_NAMES;
    const TRIGGERS: [[&str; 4]; MAX_IDENTITY_SLOTS] = TRIGGER_IDENTITY_NAMES;
    match trigger {
        None => ACTIONS.get(index).copied().unwrap_or("gesture_row_over"),
        Some(slot) => TRIGGERS
            .get(index)
            .and_then(|row| row.get(slot))
            .copied()
            .unwrap_or("gesture_row_over"),
    }
}

/// The row a plain action identity names, for the row's own popup.
pub(crate) fn action_row_index(identity: &str) -> Option<usize> {
    (0..MAX_SHOWN_ROWS).find(|index| gesture_control_identity(*index, None) == identity)
}

/// The row and the button line a trigger identity names.
pub(crate) fn trigger_row_of(identity: &str) -> Option<(usize, usize)> {
    for index in 0..MAX_SHOWN_ROWS {
        for slot in 0..4 {
            if gesture_control_identity(index, Some(slot)) == identity {
                return Some((index, slot));
            }
        }
    }
    None
}

/// Which button a gesture starts on, in the order a mouse has them. The left button is absent on
/// purpose: a gesture on it would swallow ordinary clicks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TriggerOption {
    pub(crate) button: u32,
    pub(crate) label: String,
}

pub(crate) fn trigger_options() -> Vec<TriggerOption> {
    GESTURE_TRIGGER_BUTTONS
        .into_iter()
        .map(|button| TriggerOption {
            button,
            label: crate::mouse::shortcut::button_label(button),
        })
        .collect()
}

/// The trigger button of one gesture row, in the words the row's left column uses.
pub(crate) fn gesture_trigger_option_ids() -> Vec<u32> {
    trigger_options()
        .into_iter()
        .map(|option| option.button)
        .collect()
}

/// Move one gesture row to another trigger button, and report the configuration before and after.
///
/// The row is looked up by position, because that is the identity the page stamped on the control. A
/// button the configuration could not use (the left one) is refused here as well as by the loader,
/// so a popup entry can never write a row the tap thread would ignore.
pub(crate) fn set_row_trigger(
    index: usize,
    button: u32,
) -> Option<(crate::config::Config, crate::config::Config)> {
    if !(1..=4).contains(&button) {
        return None;
    }
    let old = crate::config::CONFIG.read().ok()?.clone();
    let mut updated = old.clone();
    let row = updated.mouse.gestures.get_mut(index)?;
    if row.button == button {
        return None;
    }
    row.button = button;
    if let Ok(mut config) = crate::config::CONFIG.write() {
        *config = updated.clone();
    }
    Some((old, updated))
}

/// The row that appears while a gesture row's trigger row is open: that gesture and every button it
/// can start on, one line each, with the one in use marked by its copy.
fn trigger_page_view(index: usize, config: &crate::config::Config) -> PageView {
    let mut w = RowWriter::new(SettingsPageId::Gestures, t("settings.sidebar_gestures"));
    w.in_first_card("settings.gestures_header");
    let Some(entry) = config.mouse.gestures.get(index) else {
        return w.finish();
    };
    let stroke = stroke_label(
        &crate::mouse::gesture::recognizer::parse_key(&entry.gesture, 8)
            .unwrap_or_else(|| entry.gesture.clone()),
    );
    // The heading line states which gesture is being moved; the buttons follow it.
    w.push(
        RowDraft::new(
            "settings.gestures_row_trigger_heading",
            ControlKind::Value {
                action: None,
                align: ValueAlign::Right,
            },
            ControlId::Action("gesture_trigger_heading"),
            Value::None,
        )
        .label_text(format!("{} · {}", stroke, target_label(&entry.binding)))
        .into_spec(),
    );
    for (slot, button) in gesture_trigger_option_ids().into_iter().enumerate() {
        let current = entry.button == button;
        let key = if current {
            "settings.gestures_row_trigger_current"
        } else {
            "settings.gestures_row_trigger"
        };
        let identity = gesture_control_identity(index, Some(slot));
        w.push(
            RowDraft::new(
                key,
                ControlKind::Value {
                    action: Some(ValueAction::new(identity, "settings.gestures_trigger_use")),
                    align: ValueAlign::Right,
                },
                ControlId::Action(identity),
                Value::None,
            )
            .label_text(crate::mouse::shortcut::button_label(button))
            .into_spec(),
        );
    }
    w.finish()
}

/// Remove one configured gesture, and report the configuration before and after.
pub(crate) fn remove_gesture_row(
    index: usize,
) -> Option<(crate::config::Config, crate::config::Config)> {
    let old = crate::config::CONFIG.read().ok()?.clone();
    if index >= old.mouse.gestures.len() {
        return None;
    }
    let mut updated = old.clone();
    updated.mouse.gestures.remove(index);
    if let Ok(mut config) = crate::config::CONFIG.write() {
        *config = updated.clone();
    }
    Some((old, updated))
}

/// The most rows the page shows. Matched by [[row_identity]]'s table: a row count above this is
/// reported as a "+N" line, never by sharing an identity.
///
/// Eleven, because that is what the shipped default list holds: a bound under it would present the
/// app's own configuration as an overflow.
const MAX_SHOWN_ROWS: usize = 11;

/// The identity of one excluded-application row. Same bound and same reason as the gesture rows:
/// the control table addresses a row by a \`&'static str\` identity.
const MAX_SHOWN_EXCLUDED: usize = 16;

fn excluded_row_identity(index: usize) -> &'static str {
    const IDS: [&str; MAX_SHOWN_EXCLUDED] = [
        "gesture_excluded_a",
        "gesture_excluded_b",
        "gesture_excluded_c",
        "gesture_excluded_d",
        "gesture_excluded_e",
        "gesture_excluded_f",
        "gesture_excluded_g",
        "gesture_excluded_h",
        "gesture_excluded_i",
        "gesture_excluded_j",
        "gesture_excluded_k",
        "gesture_excluded_l",
        "gesture_excluded_m",
        "gesture_excluded_n",
        "gesture_excluded_o",
        "gesture_excluded_p",
    ];
    IDS.get(index).copied().unwrap_or("gesture_excluded_over")
}

/// The most rows any identity table can name: the page's own row bound.
///
/// A table shorter than the bound is a page that fails to build at the first row it cannot name.
const MAX_IDENTITY_SLOTS: usize = MAX_SHOWN_ROWS;

/// The control identities this page hands to the control table, one per row slot.
///
/// Written out rather than formatted: an identity is a `&'static str` the table looks up, and
/// a value that never changes should not allocate on every page build. `MAX_IDENTITY_SLOTS`
/// is the page's row bound, so a table shorter than the bound cannot happen.
const IDENTITY_NAMES: [&str; MAX_IDENTITY_SLOTS] = [
    "gesture_row_a",
    "gesture_row_b",
    "gesture_row_c",
    "gesture_row_d",
    "gesture_row_e",
    "gesture_row_f",
    "gesture_row_g",
    "gesture_row_h",
    "gesture_row_i",
    "gesture_row_j",
    "gesture_row_k",
];

/// The four trigger-button lines of each row's trigger page.
const TRIGGER_IDENTITY_NAMES: [[&str; 4]; MAX_IDENTITY_SLOTS] = [
    [
        "gesture_trigger_a_0",
        "gesture_trigger_a_1",
        "gesture_trigger_a_2",
        "gesture_trigger_a_3",
    ],
    [
        "gesture_trigger_b_0",
        "gesture_trigger_b_1",
        "gesture_trigger_b_2",
        "gesture_trigger_b_3",
    ],
    [
        "gesture_trigger_c_0",
        "gesture_trigger_c_1",
        "gesture_trigger_c_2",
        "gesture_trigger_c_3",
    ],
    [
        "gesture_trigger_d_0",
        "gesture_trigger_d_1",
        "gesture_trigger_d_2",
        "gesture_trigger_d_3",
    ],
    [
        "gesture_trigger_e_0",
        "gesture_trigger_e_1",
        "gesture_trigger_e_2",
        "gesture_trigger_e_3",
    ],
    [
        "gesture_trigger_f_0",
        "gesture_trigger_f_1",
        "gesture_trigger_f_2",
        "gesture_trigger_f_3",
    ],
    [
        "gesture_trigger_g_0",
        "gesture_trigger_g_1",
        "gesture_trigger_g_2",
        "gesture_trigger_g_3",
    ],
    [
        "gesture_trigger_h_0",
        "gesture_trigger_h_1",
        "gesture_trigger_h_2",
        "gesture_trigger_h_3",
    ],
    [
        "gesture_trigger_i_0",
        "gesture_trigger_i_1",
        "gesture_trigger_i_2",
        "gesture_trigger_i_3",
    ],
    [
        "gesture_trigger_j_0",
        "gesture_trigger_j_1",
        "gesture_trigger_j_2",
        "gesture_trigger_j_3",
    ],
    [
        "gesture_trigger_k_0",
        "gesture_trigger_k_1",
        "gesture_trigger_k_2",
        "gesture_trigger_k_3",
    ],
];

/// One identity per row's DELETE action.
const DELETE_IDENTITY_NAMES: [&str; MAX_IDENTITY_SLOTS] = [
    "gesture_del_a",
    "gesture_del_b",
    "gesture_del_c",
    "gesture_del_d",
    "gesture_del_e",
    "gesture_del_f",
    "gesture_del_g",
    "gesture_del_h",
    "gesture_del_i",
    "gesture_del_j",
    "gesture_del_k",
];

/// The identity of one gesture row's DELETE action.
fn delete_row_identity(index: usize) -> &'static str {
    const IDS: [&str; MAX_IDENTITY_SLOTS] = DELETE_IDENTITY_NAMES;
    IDS.get(index).copied().unwrap_or("gesture_del_over")
}

/// The row a delete identity names.
pub(crate) fn delete_row_index(identity: &str) -> Option<usize> {
    (0..MAX_SHOWN_ROWS).find(|index| delete_row_identity(*index) == identity)
}

/// A row's control identity: the same row in the page's action column.
fn row_identity(index: usize) -> &'static str {
    gesture_control_identity(index, None)
}

/// One entry of every gesture row's action popup: the binding the configuration holds, and the
/// words the page shows for it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TargetOption {
    id: String,
    label: String,
}

/// The action popup's entries, in one fixed order: the app's own actions first (grouped as the
/// binding page groups them), then the switcher, the system actions and finally "none".
///
/// One shared list is what lets a popup show the row's CURRENT target: the page matches the row's
/// binding string against these ids, so an action or a shortcut the user picked stays selected.
/// The list is built once per page build rather than per row.
fn target_options() -> Vec<TargetOption> {
    let mut options = Vec::with_capacity(crate::key_bindings::ALL.len() + 6);
    options.push(TargetOption {
        id: String::new(),
        label: t("settings.gestures_target_unset"),
    });
    for action in crate::key_bindings::ALL {
        options.push(TargetOption {
            id: action.config_id().to_string(),
            label: crate::key_bindings::action_label(action),
        });
    }
    for (id, key) in [
        ("switcher", "settings.gestures_target_switcher"),
        ("missioncontrol", "settings.mapping_action_missioncontrol"),
        ("launchpad", "settings.mapping_action_launchpad"),
        ("showdesktop", "settings.mapping_action_showdesktop"),
        ("appexpose", "settings.mapping_action_appexpose"),
        ("none", "settings.gestures_target_none"),
    ] {
        options.push(TargetOption {
            id: id.to_string(),
            label: t(key),
        });
    }
    options
}

/// The stroke in the left column: the arrows for each direction the user drew, or the letters
/// themselves when a direction has no picture.
///
/// Arrows are the shape of the gesture, which is what a person recognises; the letters stay for the
/// inputs people describe as letters, and a sequence the recognizer refused keeps its raw text
/// because it has no directions to draw.
fn stroke_label(sequence: &str) -> String {
    if crate::mouse::gesture::recognizer::parse_key(sequence, 8).is_none() {
        return sequence.to_string();
    }
    let mut out = String::new();
    for direction in sequence.chars() {
        out.push(direction_arrow(direction));
    }
    out
}

/// One direction as the arrow the stroke is drawn with.
fn direction_arrow(direction: char) -> char {
    match direction {
        'L' => '←',
        'R' => '→',
        'U' => '↑',
        'D' => '↓',
        other => other,
    }
}

/// The stroke in words beside its arrows: "← 左", "↑← 先向上再向左".
///
/// The arrows are the shape of a gesture and the words are its name -- the same words the list has
/// always used for the trigger page ("先向上再向左"), so a row and the trigger page it opens read
/// alike. They are glued into the row's own label instead of its caption: a caption would double
/// every row's height, and the list is the page's main body.
fn stroke_words(sequence: &str) -> String {
    let directions: Vec<char> = crate::mouse::gesture::table::row_sequence(sequence)
        .map(|key| key.chars().collect())
        .unwrap_or_default();
    if directions.is_empty() {
        // A sequence the recognizer refused keeps its raw text: there are no directions to name.
        return sequence.to_string();
    }
    let arrows: String = directions.iter().copied().map(direction_arrow).collect();
    let named: Vec<String> = directions.iter().copied().map(direction_word).collect();
    match named.as_slice() {
        [only] => format!("{arrows} {only}"),
        [first, rest @ ..] => {
            let mut text =
                crate::i18n::tf("settings.gestures_stroke_first", &[("direction", first)]);
            for direction in rest {
                text = crate::i18n::tf(
                    "settings.gestures_stroke_next",
                    &[("text", &text), ("direction", direction)],
                );
            }
            format!("{arrows} {text}")
        }
        [] => sequence.to_string(),
    }
}

/// One direction in the user's words.
fn direction_word(direction: char) -> String {
    match direction {
        'L' => t("settings.gestures_direction_left"),
        'R' => t("settings.gestures_direction_right"),
        'U' => t("settings.gestures_direction_up"),
        _ => t("settings.gestures_direction_down"),
    }
}

/// Every application the exclusion picker can offer, before the configured ones are removed: the
/// installed ones plus whatever is running but installed outside the standard folders.
///
/// This is the raw catalogue; `running_app_candidates` is what the popup actually lists.
fn gesture_excluded_candidates() -> Vec<(String, String)> {
    let mut apps = crate::app_identity::installed_application_bundles();
    // Reading the running-application list is AppKit work, and the page build runs on the
    // main thread.
    for entry in unsafe { crate::app_identity::running_apps_with_bundle_ids() } {
        if !apps.iter().any(|(bundle, _)| *bundle == entry.0) {
            apps.push(entry);
        }
    }
    apps.sort_by_key(|(_, name)| name.to_lowercase());
    apps
}

fn configured_rows(cfg: &crate::config::Config) -> Vec<ShownRow> {
    cfg.mouse
        .gestures
        .iter()
        .enumerate()
        .map(|(index, entry)| ShownRow {
            index,
            sequence: crate::mouse::gesture::recognizer::parse_key(&entry.gesture, 8)
                .unwrap_or_else(|| entry.gesture.clone()),
            button: crate::mouse::shortcut::button_label(entry.button),
            trigger: entry.button,
            target: target_label(&entry.binding),
            binding: entry.binding.trim().to_string(),
            problem: row_problem(entry),
        })
        .collect()
}

/// What a row will not run, or `None` when it is live.
///
/// The checks mirror the loader's: a sequence the recognizer refuses, a button outside 1..=4 (the
/// left button would swallow ordinary clicks), and a binding the target parser refuses are rows the
/// tap thread never fires, so the page must not present them as live. "Off" is a separate state: a
/// disabled row is the user's own choice, not an error.
fn row_problem(entry: &crate::config::GestureBinding) -> Option<&'static str> {
    if !(1..=4).contains(&entry.button) {
        return Some("bad_button");
    }
    if crate::mouse::gesture::table::row_sequence(&entry.gesture).is_none() {
        return Some("bad_sequence");
    }
    if !entry.enabled {
        return Some("off");
    }
    if entry.binding.trim().is_empty() {
        return Some("unbound");
    }
    if crate::mouse::gesture::table::Target::resolve(&entry.binding).is_none() {
        return Some("bad_target");
    }
    None
}

/// Whether an exclusion entry can ever match: the tap compares it with an application's bundle
/// identifier (`CFBundleIdentifier`, reverse-DNS, at least two dot-separated parts and no spaces).
///
/// An entry that is not a bundle id -- an application NAME, a path, an empty line edited into the
/// file -- is accepted by the config but can never be equal to one, so the runtime ignores it. This
/// is the same predicate the page uses to mark such entries, and it is deliberately conservative:
/// a real bundle id always passes, and a non-conforming but genuinely registered identifier (an
/// Apple utility from an unsigned bundle) is reported rather than silently kept.
pub(crate) fn looks_like_bundle_id(entry: &str) -> bool {
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

/// A row state in the user's language. Never a config value.
fn problem_label(problem: &str) -> String {
    match problem {
        "off" => t("settings.gestures_state_off"),
        "unbound" => t("settings.gestures_state_unbound"),
        "bad_sequence" => t("settings.gestures_state_bad_sequence"),
        "bad_button" => t("settings.gestures_state_bad_button"),
        "bad_target" => t("settings.gestures_state_bad_target"),
        _ => String::new(),
    }
}

/// A target in the user's words: the action's localised name, the localised name of a system
/// action, or the shortcut's display form. Never a raw config id ("window.snap_left", "switcher",
/// a Dock notification): those are internal identifiers the style guide forbids showing.
fn target_label(binding: &str) -> String {
    let trimmed = binding.trim();
    if trimmed.is_empty() {
        return t("settings.gestures_target_unset");
    }
    if let Some(action) = crate::key_bindings::Action::from_config_id(trimmed) {
        return crate::key_bindings::action_label(action);
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "switcher" => t("settings.gestures_target_switcher"),
        "none" => t("settings.gestures_target_none"),
        // The Dock-backed system actions the button mappings already name in the UI; reusing their
        // keys means one product term has one string in every language.
        "missioncontrol" => t("settings.mapping_action_missioncontrol"),
        "launchpad" => t("settings.mapping_action_launchpad"),
        "showdesktop" => t("settings.mapping_action_showdesktop"),
        "appexpose" => t("settings.mapping_action_appexpose"),
        _ => crate::mouse::shortcut::display_shortcut(trimmed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(enabled: bool, rows: Vec<crate::config::GestureBinding>) -> PageInput {
        let mut page_input = PageInput::capture();
        page_input.config.mouse.enabled = true;
        page_input.config.mouse.gestures_enabled = enabled;
        page_input.config.mouse.gestures = rows;
        page_input
    }

    fn row(
        gesture: &str,
        button: u32,
        binding: &str,
        enabled: bool,
    ) -> crate::config::GestureBinding {
        crate::config::GestureBinding {
            gesture: gesture.into(),
            button,
            binding: binding.into(),
            enabled,
        }
    }

    /// A gesture row's left column (the stroke and its state) followed by its right column (the
    /// selected action), joined the way a person reads the row.
    fn row_values(view: &PageView) -> Vec<String> {
        view.rows
            .iter()
            .filter(|r| r.label_key == "settings.gestures_row_gesture")
            .map(|r| {
                let stroke = r.label_text.clone().unwrap_or_default();
                let action = match &r.control {
                    // A fixed row carries what it runs as its own value; an editable one as the
                    // selected entry of its action popup.
                    ControlKind::Value { .. } => match r.value {
                        Value::Dynamic(ref text) => text.clone(),
                        _ => String::new(),
                    },
                    ControlKind::Popup { options } => match r.value {
                        Value::Index(index) => options.get(index).cloned().unwrap_or_default(),
                        _ => String::new(),
                    },
                    _ => String::new(),
                };
                format!("{stroke} · {action}")
            })
            .collect()
    }

    #[test]
    fn the_page_declares_its_identity_and_switches_on_the_master_flag() {
        let view = gestures_page_view(&input(true, Vec::new()));
        assert_eq!(view.page, SettingsPageId::Gestures);
        assert_eq!(view.title_key, "settings.sidebar_gestures");
        assert_eq!(view.rows[0].label_key, "settings.gestures_row_enabled");
        assert!(matches!(view.rows[0].value, Value::Switch(true)));
    }

    /// The master switch and the two presentation switches, in that order, each reading its own
    /// configuration field: the page exists to let a user turn the trail or the hint off.
    #[test]
    fn the_page_offers_the_master_switch_and_both_presentation_switches() {
        let mut page_input = input(true, Vec::new());
        page_input.config.mouse.gesture_show_trail = false;
        page_input.config.mouse.gesture_show_hint = true;
        let view = gestures_page_view(&page_input);
        let switches: Vec<(&str, bool)> = view
            .rows
            .iter()
            .filter_map(|row| match (&row.control, &row.value) {
                (ControlKind::Switch, Value::Switch(on)) => Some((row.label_key, *on)),
                _ => None,
            })
            .collect();
        assert_eq!(
            switches,
            vec![
                ("settings.gestures_row_enabled", true),
                ("settings.gestures_row_show_trail", false),
                ("settings.gestures_row_show_hint", true),
            ],
            "the switches read the configuration, in page order"
        );
    }

    /// A conflicting row says so in the user's language: the words are looked up, and a raw
    /// translation key on a row is a key that does not exist.
    #[test]
    fn a_conflicting_row_says_so_in_words() {
        let view = gestures_page_view(&input(
            true,
            vec![
                row("rd", 1, "window.snap_left", true),
                row("rd", 1, "window.maximize", true),
            ],
        ));
        let labels: Vec<String> = view
            .rows
            .iter()
            .filter(|r| r.label_key == "settings.gestures_row_gesture")
            .filter_map(|r| r.label_text.clone())
            .collect();
        assert_eq!(labels.len(), 2);
        for label in &labels {
            assert!(
                !label.contains("errors.") && !label.contains("settings."),
                "a raw translation key reached a row: {label}"
            );
        }
        assert!(
            labels[1].contains(&crate::i18n::t("errors.errors_gesture_duplicate")),
            "the shadowed row says why it will not run: {labels:?}"
        );
    }

    /// A row names its stroke in words as well as in arrows, so a user does not have to read a
    /// sequence of symbols: one direction is named, and a longer stroke is told step by step.
    #[test]
    fn a_row_names_its_stroke_in_words() {
        let view = gestures_page_view(&input(
            true,
            vec![
                row("l", 1, "window.snap_left", true),
                row("ul", 1, "window.snap_left", true),
                row("udl", 1, "window.snap_left", true),
            ],
        ));
        let labels: Vec<String> = view
            .rows
            .iter()
            .filter(|r| r.label_key == "settings.gestures_row_gesture")
            .filter_map(|r| r.label_text.clone())
            .collect();
        assert_eq!(labels[0], "← 向左", "{labels:?}");
        assert_eq!(labels[1], "↑← 先向上再向左", "{labels:?}");
        assert_eq!(labels[2], "↑↓← 先向上再向下再向左", "{labels:?}");
    }

    /// A sequence the recognizer refused keeps its raw text: there are no directions to name.
    #[test]
    fn a_refused_sequence_keeps_its_own_text_in_the_row() {
        assert_eq!(stroke_words("XYZ"), "XYZ");
        assert_eq!(stroke_words(""), "");
    }

    /// The trigger is one row, a popup over the four buttons a mouse has, and it shows the button
    /// the configuration actually uses.
    #[test]
    fn the_page_offers_one_trigger_popup_with_four_buttons() {
        let mut page_input = input(true, vec![row("l", 1, "window.snap_left", true)]);
        page_input.config.mouse.gesture_button = 3;
        let view = gestures_page_view(&page_input);
        let trigger = view
            .rows
            .iter()
            .find(|r| r.label_key == "settings.gestures_row_gesture_button")
            .expect("the trigger row");
        match &trigger.control {
            ControlKind::Popup { options } => assert_eq!(
                options.len(),
                4,
                "right, middle and both side buttons: {options:?}"
            ),
            other => panic!("the trigger is a popup, not {other:?}"),
        }
        assert_eq!(
            trigger.id,
            ControlId::Field(ControlField::GestureTriggerButton)
        );
        assert_eq!(trigger.value, Value::Index(2), "side button 1 is selected");
        // No gesture row names a button any more.
        for row in view
            .rows
            .iter()
            .filter(|r| r.label_key == "settings.gestures_row_gesture")
        {
            let label = row.label_text.clone().unwrap_or_default();
            assert!(
                !label.contains(&crate::mouse::shortcut::button_label(1)),
                "{label}"
            );
        }
    }

    /// The shipped default list fits the page: an overflow line under the app's own rows read as
    /// part of the exclusion card, with no gesture in sight.
    #[test]
    fn the_shipped_defaults_fit_without_an_overflow_line() {
        let shipped = crate::config::MouseSection::default().gestures;
        let view = gestures_page_view(&input(true, shipped));
        let more = view
            .rows
            .iter()
            .filter(|r| r.id == ControlId::Action("gesture_rows_more"))
            .count();
        assert_eq!(more, 0, "the shipped list is not an overflow");
    }

    /// Rows past the bound are stated at the END of the gesture list, before the exclusion card:
    /// the line has to be readable as part of the list it summarises.
    #[test]
    fn the_overflow_line_sits_under_the_gesture_rows() {
        let many: Vec<_> = (0..MAX_SHOWN_ROWS + 3)
            .map(|_| row("u", 1, "cmd+t", true))
            .collect();
        let view = gestures_page_view(&input(true, many));
        // Cards are what the writer numbers: the gesture rows and their overflow line belong to
        // one card, and the exclusion rows to the next.
        let gesture_card = view
            .rows
            .iter()
            .find(|r| r.label_key == "settings.gestures_row_gesture")
            .map(|r| r.card)
            .expect("gesture rows");
        let more = view
            .rows
            .iter()
            .find(|r| r.label_key == "settings.gestures_row_more")
            .expect("the overflow line");
        assert_eq!(
            more.card, gesture_card,
            "the overflow line is drawn in the gesture rows' own card"
        );
        let excluded_card = view
            .rows
            .iter()
            .filter(|r| r.label_key.starts_with("settings.gestures_row_excluded"))
            .map(|r| r.card)
            .next()
            .expect("the exclusion rows");
        assert!(
            more.card < excluded_card,
            "the overflow line belongs before the exclusion card"
        );
    }

    /// The list is fixed, so the rows cannot be edited: every gesture row is a plain value with
    /// nothing to press, and the recorder that would add a row is not on the page. This is the
    /// assertion that keeps a half-editable page from coming back unnoticed.
    #[test]
    fn a_fixed_list_draws_plain_rows_and_no_recorder() {
        assert!(!GESTURE_ROWS_EDITABLE, "this test pins the shipped state");
        let view = gestures_page_view(&input(true, vec![row("l", 1, "window.snap_left", true)]));
        assert!(
            !view
                .rows
                .iter()
                .any(|r| r.label_key == "settings.gestures_row_record"),
            "the recorder adds a row, which a fixed list does not offer"
        );
        let gesture_rows: Vec<&RowSpec> = view
            .rows
            .iter()
            .filter(|r| r.label_key == "settings.gestures_row_gesture")
            .collect();
        assert_eq!(gesture_rows.len(), 1);
        match &gesture_rows[0].control {
            ControlKind::Value { action, .. } => {
                assert!(action.is_none(), "a fixed row has no action button")
            }
            other => panic!("a fixed row is a plain value, not {other:?}"),
        }
        assert!(
            matches!(gesture_rows[0].value, Value::Dynamic(ref text) if !text.is_empty()),
            "the row still says what it runs"
        );
    }

    /// The in-place refresh can write both presentation switches, because the shared update
    /// description carries them: a switch the description skipped would keep showing the old state
    /// after a config reload while the HUD already obeyed the new one.
    #[test]
    fn the_update_description_carries_both_presentation_switches() {
        let mut page_input = input(true, Vec::new());
        page_input.config.mouse.gesture_show_trail = false;
        page_input.config.mouse.gesture_show_hint = false;
        let updates = page_update_description(&page_input, &[SettingsPageId::Gestures]);
        for (field, name) in [
            (ControlField::GestureShowTrail, "GestureShowTrail"),
            (ControlField::GestureShowHint, "GestureShowHint"),
        ] {
            let update = updates
                .iter()
                .find(|update| update.field == field)
                .unwrap_or_else(|| panic!("the description must carry {name}"));
            assert_eq!(update.value, "off", "{name} follows the configuration");
        }
    }

    /// Each excluded row is LABELLED with the application's name and carries no second copy of it.
    ///
    /// The generic "Application" title said nothing, and the identifier beside it was truncated to
    /// "com.o…" -- a list of rows that could not be told apart. The name is the label now, the same
    /// way a gesture row labels itself with its stroke. An entry that names nothing installed keeps
    /// its identifier, because it still has to be listed and removable.
    #[test]
    fn an_excluded_row_is_labelled_with_the_application_name() {
        let mut page_input = input(true, Vec::new());
        page_input.config.mouse.gesture_excluded_apps = vec![
            "com.apple.Safari".into(),
            "com.example.not-installed-anywhere".into(),
        ];
        let view = gestures_page_view(&page_input);
        let excluded: Vec<&RowSpec> = view
            .rows
            .iter()
            .filter(|r| r.label_key == "settings.gestures_row_excluded")
            .collect();
        assert_eq!(excluded.len(), 2, "one row per entry");
        let texts: Vec<Option<&str>> = excluded.iter().map(|r| r.label_text.as_deref()).collect();
        assert_eq!(
            texts,
            vec![Some("Safari"), Some("com.example.not-installed-anywhere")],
            "the name is the label; an unknown bundle keeps its identifier"
        );
        for row in excluded {
            assert_eq!(
                row.value,
                Value::None,
                "the name is the label, not repeated as the value"
            );
            assert!(matches!(
                &row.control,
                ControlKind::Value {
                    action: Some(_),
                    ..
                }
            ));
        }
    }

    /// A row's action tag must not travel in the button's `tag`.
    ///
    /// That slot carries the ROLE's palette slot, which the hover and focus callbacks read to
    /// restore the right fill. Stamping a list index there made both fill lookups fall through to
    /// the generic hover wash, so an excluded row's Remove button was painted with the HOVER colour
    /// from the moment it was built and never returned (measured: tag was 0/1 instead of -3).
    /// The index travels as the button's identifier instead; this asserts the page hands the row an
    /// action tag at all, and the identifier round-trip is covered where the identifier is made.
    #[test]
    fn an_excluded_row_carries_its_index_as_row_data() {
        let mut page_input = input(true, Vec::new());
        page_input.config.mouse.gesture_excluded_apps =
            vec!["com.apple.Safari".into(), "com.apple.Notes".into()];
        let view = gestures_page_view(&page_input);
        let tags: Vec<Option<isize>> = view
            .rows
            .iter()
            .filter(|r| r.label_key == "settings.gestures_row_excluded")
            .map(|r| r.action_tag)
            .collect();
        assert_eq!(
            tags,
            vec![Some(0), Some(1)],
            "each row states its own entry"
        );
        for row in view
            .rows
            .iter()
            .filter(|r| r.label_key == "settings.gestures_row_excluded")
        {
            assert!(
                matches!(
                    &row.control,
                    ControlKind::Value {
                        action: Some(_),
                        ..
                    }
                ),
                "the row still offers its Remove action"
            );
        }
    }

    /// An entry the runtime can never match is marked, so a typo is not mistaken for a broken
    /// feature. The empty state text changes too: "no applications excluded" would be a lie while
    /// an entry sits there that excludes nothing.
    #[test]
    fn an_entry_that_cannot_match_is_marked_and_the_empty_text_says_so() {
        let mut page_input = input(true, Vec::new());
        page_input.config.mouse.gesture_excluded_apps = vec!["com.apple.Safari".into()];
        let view = gestures_page_view(&page_input);
        let labels: Vec<&str> = view.rows.iter().map(|r| r.label_key).collect();
        assert!(
            !labels.contains(&"settings.gestures_row_excluded_invalid"),
            "a real bundle id is not marked: {labels:?}"
        );
        assert!(
            !labels.contains(&"settings.gestures_row_excluded_empty"),
            "a non-empty list must not say it is empty: {labels:?}"
        );

        page_input.config.mouse.gesture_excluded_apps = vec!["Safari".into()];
        let view = gestures_page_view(&page_input);
        let labels: Vec<&str> = view.rows.iter().map(|r| r.label_key).collect();
        assert!(
            labels.contains(&"settings.gestures_row_excluded_invalid"),
            "an application name is not a bundle id: {labels:?}"
        );
        assert!(
            view.rows.iter().any(|r| {
                r.label_key == "settings.gestures_row_excluded_invalid"
                    && matches!(r.value, Value::Dynamic(ref count) if count == "1")
            }),
            "the row counts the unusable entries: {labels:?}"
        );
    }

    #[test]
    fn the_bundle_id_predicate_is_conservative() {
        for good in [
            "com.apple.Safari",
            "com.eacryo.oh-my-tab.dev",
            "dev.zed.Zed",
            "org.mozilla.firefox",
        ] {
            assert!(looks_like_bundle_id(good), "{good}");
        }
        for bad in [
            "",
            " ",
            "Safari",
            "Safari App",
            "/Applications/Safari.app",
            "file:///Applications/Safari.app",
            "com.apple Safari",
            "com..x",
            ".com.x",
            "com.apple.",
        ] {
            assert!(!looks_like_bundle_id(bad), "{bad:?}");
        }
    }

    /// The refusal is about the application a gesture is DRAWN IN, so the switch that governs it is
    /// the gestures switch; the mouse-section switch cannot stop it (`gesture::runtime::set_active`
    /// gates on both). A page that let the mouse switch hide the list would describe a state the tap
    /// does not implement.
    #[test]
    fn the_exclusion_list_stays_listed_while_the_mouse_section_is_off() {
        let mut page_input = input(true, Vec::new());
        page_input.config.mouse.enabled = false;
        page_input.config.mouse.gesture_excluded_apps = vec!["com.apple.Safari".into()];
        let view = gestures_page_view(&page_input);
        let labels: Vec<&str> = view.rows.iter().map(|r| r.label_key).collect();
        assert!(
            labels.contains(&"settings.gestures_row_add_app"),
            "the add row must still be described: {labels:?}"
        );
        assert!(
            view.rows.iter().any(|r| {
                r.label_key == "settings.gestures_row_excluded"
                    && r.label_text.as_deref() == Some("Safari")
            }),
            "the excluded application must still be listed, under its name: {labels:?}"
        );
    }

    #[test]
    fn every_configured_row_gets_its_own_control_identity() {
        // The control table refuses a duplicate id, and a refused registration fails the whole page
        // build -- so two configured gestures must not share one.
        let view = gestures_page_view(&input(
            true,
            (0..3)
                .map(|i| row(if i == 0 { "rd" } else { "u" }, 1, "window.snap_left", true))
                .collect(),
        ));
        let mut ids: Vec<String> = view.rows.iter().map(|r| format!("{:?}", r.id)).collect();
        let total = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), total, "duplicate control identities");
    }

    #[test]
    fn rows_past_the_shown_limit_are_reported_not_shared() {
        // Review finding: past the identity table two rows shared one id and the BUILD failed. A
        // list longer than the page can address now ends with an explicit "+N" line.
        let rows: Vec<_> = (0..(MAX_SHOWN_ROWS + 3))
            .map(|_| row("u", 1, "cmd+t", true))
            .collect();
        let view = gestures_page_view(&input(true, rows));
        let mut ids: Vec<String> = view.rows.iter().map(|r| format!("{:?}", r.id)).collect();
        let total = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(
            ids.len(),
            total,
            "duplicate identities in an over-long list"
        );
        assert!(
            view.rows
                .iter()
                .any(|r| r.label_key == "settings.gestures_row_more"),
            "the overflow is stated"
        );
    }

    #[test]
    fn more_than_ten_rows_stay_unique() {
        // Review finding: the identity used to collapse every row past index 7 onto one spelling, so
        // the eleventh configured gesture made the page fail to build.
        let rows: Vec<_> = (0..12).map(|_| row("u", 1, "cmd+t", true)).collect();
        let view = gestures_page_view(&input(true, rows));
        let mut ids: Vec<String> = view.rows.iter().map(|r| format!("{:?}", r.id)).collect();
        let total = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), total, "duplicate identities past row ten");
    }

    #[test]
    fn rows_survive_the_master_switch_being_off() {
        let view = gestures_page_view(&input(false, vec![row("rd", 1, "window.snap_left", true)]));
        // The left column is the whole trigger: the button that starts it, then the arrows.
        assert!(
            row_values(&view).iter().any(|text| text.contains("→↓")),
            "the configured row is still listed"
        );
    }

    #[test]
    fn a_row_states_what_it_runs_in_words_a_user_recognises() {
        let view = gestures_page_view(&input(true, vec![row("rd", 1, "window.snap_left", true)]));
        let text = row_values(&view).remove(0);
        assert!(
            !text.contains("window."),
            "a raw config id reached the page: {text}"
        );
        assert!(!text.contains("snap_left"), "{text}");
        // The left column is the stroke alone -- the button is the page's one trigger, chosen
        // above the list -- and the right column is the action's own name.
        assert!(
            !text.contains(&crate::mouse::shortcut::button_label(1)),
            "{text}"
        );
        assert!(text.contains("→↓"), "{text}");
        assert!(
            text.contains(&t("settings.binding_action_snap_left")),
            "{text}"
        );
    }

    #[test]
    fn system_targets_and_states_are_localised() {
        let view = gestures_page_view(&input(
            true,
            vec![
                row("u", 1, "switcher", true),
                row("l", 1, "wall-of-text", true),
                row("d", 0, "cmd+t", true),
            ],
        ));
        let values = row_values(&view);
        for text in &values {
            assert!(!text.contains("switcher"), "internal target id: {text}");
            assert!(!text.contains("bad_"), "internal state: {text}");
            assert!(
                !text.contains("unbound") && !text.contains("off"),
                "internal state: {text}"
            );
        }
        // The left button is refused by the loader, and the row says so instead of looking live;
        // the sequence that cannot be drawn keeps its own letters.
        let button = values
            .iter()
            .find(|text| text.contains('↓'))
            .unwrap_or_else(|| panic!("the left-button row: {values:?}"));
        assert!(
            button.contains(&t("settings.gestures_state_bad_button")),
            "{button}"
        );
        assert!(
            values
                .iter()
                .any(|text| text.contains(&t("settings.gestures_state_bad_target"))),
            "{values:?}"
        );
    }

    #[test]
    fn a_trigger_row_lists_every_button_and_states_the_current_one() {
        // The action popup opens the row's trigger row: one line per button a gesture may start on,
        // and the line the row is on now says so instead of looking like the others.
        crate::settings::set_open_gesture_trigger_row(Some(0));
        let mut page_input = PageInput::capture();
        page_input.config.mouse.enabled = true;
        page_input.config.mouse.gestures_enabled = true;
        page_input.config.mouse.gestures = vec![row("rd", 2, "window.snap_left", true)];
        let view = gestures_page_view(&page_input);
        crate::settings::set_open_gesture_trigger_row(None);

        let current: Vec<String> = view
            .rows
            .iter()
            .filter(|r| r.label_key == "settings.gestures_row_trigger_current")
            .map(|r| r.label_text.clone().unwrap_or_default())
            .collect();
        assert_eq!(current, vec![crate::mouse::shortcut::button_label(2)]);
        let all: Vec<String> = view
            .rows
            .iter()
            .filter(|r| r.label_key == "settings.gestures_row_trigger")
            .filter_map(|r| r.label_text.clone())
            .collect();
        assert_eq!(all.len(), 3, "the other three buttons are offered: {all:?}");
        assert!(!all.contains(&crate::mouse::shortcut::button_label(2)));
    }

    #[test]
    fn a_trigger_row_falls_back_to_the_list_when_its_row_is_gone() {
        // The list can shrink under an open trigger row (a row removed, the config reloaded). The
        // page must show the gestures again rather than a trigger row for a row that is gone.
        crate::settings::set_open_gesture_trigger_row(Some(7));
        let view = gestures_page_view(&input(true, vec![row("u", 1, "cmd+t", true)]));
        assert_eq!(
            crate::settings::open_gesture_trigger_row(),
            None,
            "the stale selection is cleared"
        );
        assert!(view
            .rows
            .iter()
            .any(|r| r.label_key == "settings.gestures_row_gesture"));
    }

    #[test]
    fn a_trigger_identity_and_an_action_identity_never_collide() {
        // Two dispatchers answer these: the action popup moves to the trigger row, and a trigger
        // line moves the gesture. One spelling for both would make one of them unreachable.
        for index in 0..4 {
            let action = row_identity(index);
            assert!(is_gesture_row_identity(action));
            assert!(!is_trigger_row_identity(action), "{action}");
            for slot in 0..4 {
                let trigger = gesture_control_identity(index, Some(slot));
                assert!(is_trigger_row_identity(trigger), "{trigger}");
                assert!(!is_gesture_row_identity(trigger), "{trigger}");
                assert_eq!(trigger_row_of(trigger), Some((index, slot)));
            }
        }
        assert_eq!(action_row_index(row_identity(3)), Some(3));
    }

    #[test]
    fn the_idle_record_row_does_not_repeat_the_buttons_title() {
        // A value row with its own action draws the button; the value cell must not repeat it,
        // or the page shows two "Record" controls. The row only exists on the editable page.
        if !GESTURE_ROWS_EDITABLE {
            return;
        }
        let view = gestures_page_view(&input(true, Vec::new()));
        let record = view
            .rows
            .iter()
            .find(|r| r.label_key == "settings.gestures_row_record")
            .expect("the record row");
        match &record.value {
            Value::Dynamic(text) => assert!(text.is_empty(), "repeats the button title: {text}"),
            other => panic!("unexpected value: {other:?}"),
        }
    }

    /// The page description and the in-place refresh write ONE row text, so a recording that
    /// started cannot redraw "nothing" while the page still promises a capture.
    #[test]
    fn every_recording_stage_has_its_own_row_text() {
        use crate::mouse::gesture::recording::Stage;
        // Idle: the button carries the copy, and the value cell stays empty.
        assert!(crate::settings::gesture_recorder::record_row_text(Stage::Idle, "").is_empty());
        assert!(
            crate::settings::gesture_recorder::record_row_text(Stage::Finished, "R").is_empty()
        );
        // Armed: the user is told to press something, which is the acknowledgement the button
        // never gave.
        let waiting = crate::settings::gesture_recorder::record_row_text(Stage::WaitingPress, "");
        assert!(!waiting.is_empty(), "an armed recording says nothing");
        assert!(
            waiting.contains(&t("settings.gestures_record_waiting")),
            "{waiting}"
        );
        // Drawing: what was drawn so far.
        let drawing = crate::settings::gesture_recorder::record_row_text(Stage::Drawing, "RD");
        assert!(drawing.contains("RD"), "{drawing}");
        assert!(
            drawing.contains(&t("settings.gestures_record_drawing")),
            "{drawing}"
        );
    }

    /// The refresh addresses the row the page registers. A second spelling is a refresh that
    /// silently finds nothing -- how an armed recorder came to look like a dead button.
    #[test]
    fn the_record_row_refresh_addresses_the_registered_action() {
        if !GESTURE_ROWS_EDITABLE {
            return;
        }
        let view = gestures_page_view(&input(true, Vec::new()));
        let record = view
            .rows
            .iter()
            .find(|r| r.label_key == "settings.gestures_row_record")
            .expect("the record row");
        assert_eq!(
            record.id,
            ControlId::Action(crate::settings::page_view::GESTURE_RECORD_ROW_ACTION)
        );
    }

    #[test]
    fn an_empty_list_says_so() {
        let view = gestures_page_view(&input(true, Vec::new()));
        assert!(view
            .rows
            .iter()
            .any(|r| r.label_key == "settings.gestures_row_empty"));
    }
}
