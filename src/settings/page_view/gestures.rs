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

    w.card("settings.gestures_header_list");
    // The recorder row: its value IS the recording's state, so the page follows the pointer while
    // the user draws. The button beside it starts a recording, and cancels one already running.
    // The state text is the same pure description the in-place refresh writes, so the row a user
    // sees after a rebuild and the row a live preview rewrites cannot disagree.
    let state = crate::settings::gesture_recorder::record_row_text(
        crate::mouse::gesture::recording::stage(),
        &crate::settings::gesture_recorder::preview_text(),
    );
    // ONE action for the row, whatever the state: a row's action identity is fixed when the page is
    // built, so a live row cannot switch which handler it runs without a rebuild -- and rebuilding
    // during a capture is wrong. The handler reads the recording state and starts or cancels.
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

    w.push(
        RowDraft::new(
            "settings.gestures_row_restore",
            ControlKind::Value {
                action: Some(ValueAction::new(
                    "gesture_restore_defaults",
                    "settings.gestures_restore_action",
                )),
                align: ValueAlign::Right,
            },
            ControlId::Action("gesture_restore_defaults"),
            Value::None,
        )
        .desc("settings.gestures_desc_restore")
        .into_spec(),
    );

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
        // The stroke itself, with the state it will not run in when there is one: the sequence and
        // the button never leave the left column, so the right one holds only what it runs.
        // The left column is the whole trigger: which button starts the gesture and the stroke it
        // wants. The action alone is in the right column, so the row still reads as two columns.
        let mut stroke = format!(
            "{} · {}",
            crate::mouse::shortcut::button_label(row.trigger),
            stroke_label(&row.sequence)
        );
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
    // Applications where no gesture fires. The add row is a popup of the running
    // applications, because a bundle id cannot be typed from memory; each excluded bundle then
    // gets its own row.
    w.card("settings.gestures_header_excluded");
    let candidates = running_app_candidates(cfg);
    let mut options: Vec<String> = vec![t("settings.gestures_excluded_add_placeholder")];
    options.extend(candidates.iter().map(|(_, name)| name.clone()));
    w.push(
        RowDraft::new(
            "settings.gestures_row_add_app",
            ControlKind::Popup { options },
            ControlId::Field(ControlField::GestureExcludedAdd),
            Value::Index(0),
        )
        .desc("settings.gestures_desc_excluded")
        .into_spec(),
    );

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
    for (index, bundle) in cfg
        .mouse
        .gesture_excluded_apps
        .iter()
        .take(MAX_SHOWN_EXCLUDED)
        .enumerate()
    {
        let id = excluded_row_identity(index);
        // The bundle id IS the value the configuration holds. It is also the only name that is
        // always available: an excluded application need not be installed (the user may exclude it
        // before opening it), and inventing a friendlier name for some entries but not others would
        // make the list inconsistent.
        let label = bundle.clone();
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
                Value::Dynamic(label),
            )
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
    const ACTIONS: [&str; 32] = [
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
        "gesture_row_l",
        "gesture_row_m",
        "gesture_row_n",
        "gesture_row_o",
        "gesture_row_p",
        "gesture_row_q",
        "gesture_row_r",
        "gesture_row_s",
        "gesture_row_t",
        "gesture_row_u",
        "gesture_row_v",
        "gesture_row_w",
        "gesture_row_x",
        "gesture_row_y",
        "gesture_row_z",
        "gesture_row_aa",
        "gesture_row_ab",
        "gesture_row_ac",
        "gesture_row_ad",
        "gesture_row_ae",
        "gesture_row_af",
    ];
    const TRIGGERS: [[&str; 4]; 32] = [
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
        [
            "gesture_trigger_l_0",
            "gesture_trigger_l_1",
            "gesture_trigger_l_2",
            "gesture_trigger_l_3",
        ],
        [
            "gesture_trigger_m_0",
            "gesture_trigger_m_1",
            "gesture_trigger_m_2",
            "gesture_trigger_m_3",
        ],
        [
            "gesture_trigger_n_0",
            "gesture_trigger_n_1",
            "gesture_trigger_n_2",
            "gesture_trigger_n_3",
        ],
        [
            "gesture_trigger_o_0",
            "gesture_trigger_o_1",
            "gesture_trigger_o_2",
            "gesture_trigger_o_3",
        ],
        [
            "gesture_trigger_p_0",
            "gesture_trigger_p_1",
            "gesture_trigger_p_2",
            "gesture_trigger_p_3",
        ],
        [
            "gesture_trigger_q_0",
            "gesture_trigger_q_1",
            "gesture_trigger_q_2",
            "gesture_trigger_q_3",
        ],
        [
            "gesture_trigger_r_0",
            "gesture_trigger_r_1",
            "gesture_trigger_r_2",
            "gesture_trigger_r_3",
        ],
        [
            "gesture_trigger_s_0",
            "gesture_trigger_s_1",
            "gesture_trigger_s_2",
            "gesture_trigger_s_3",
        ],
        [
            "gesture_trigger_t_0",
            "gesture_trigger_t_1",
            "gesture_trigger_t_2",
            "gesture_trigger_t_3",
        ],
        [
            "gesture_trigger_u_0",
            "gesture_trigger_u_1",
            "gesture_trigger_u_2",
            "gesture_trigger_u_3",
        ],
        [
            "gesture_trigger_v_0",
            "gesture_trigger_v_1",
            "gesture_trigger_v_2",
            "gesture_trigger_v_3",
        ],
        [
            "gesture_trigger_w_0",
            "gesture_trigger_w_1",
            "gesture_trigger_w_2",
            "gesture_trigger_w_3",
        ],
        [
            "gesture_trigger_x_0",
            "gesture_trigger_x_1",
            "gesture_trigger_x_2",
            "gesture_trigger_x_3",
        ],
        [
            "gesture_trigger_y_0",
            "gesture_trigger_y_1",
            "gesture_trigger_y_2",
            "gesture_trigger_y_3",
        ],
        [
            "gesture_trigger_z_0",
            "gesture_trigger_z_1",
            "gesture_trigger_z_2",
            "gesture_trigger_z_3",
        ],
        [
            "gesture_trigger_aa_0",
            "gesture_trigger_aa_1",
            "gesture_trigger_aa_2",
            "gesture_trigger_aa_3",
        ],
        [
            "gesture_trigger_ab_0",
            "gesture_trigger_ab_1",
            "gesture_trigger_ab_2",
            "gesture_trigger_ab_3",
        ],
        [
            "gesture_trigger_ac_0",
            "gesture_trigger_ac_1",
            "gesture_trigger_ac_2",
            "gesture_trigger_ac_3",
        ],
        [
            "gesture_trigger_ad_0",
            "gesture_trigger_ad_1",
            "gesture_trigger_ad_2",
            "gesture_trigger_ad_3",
        ],
        [
            "gesture_trigger_ae_0",
            "gesture_trigger_ae_1",
            "gesture_trigger_ae_2",
            "gesture_trigger_ae_3",
        ],
        [
            "gesture_trigger_af_0",
            "gesture_trigger_af_1",
            "gesture_trigger_af_2",
            "gesture_trigger_af_3",
        ],
    ];
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
    [1u32, 2, 3, 4]
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

/// Put the default gesture list back, and say whether anything changed.
///
/// Only the gesture list: the excluded applications and every other mouse setting belong to the
/// user's other choices, and a "restore the gestures" button must not reach them.
pub(crate) fn restore_default_gestures() -> Option<(crate::config::Config, crate::config::Config)> {
    let old = crate::config::CONFIG.read().ok()?.clone();
    let mut updated = old.clone();
    updated.mouse.gestures = crate::config::default_gestures_for_restore();
    if updated.mouse.gestures == old.mouse.gestures {
        return None;
    }
    if let Ok(mut config) = crate::config::CONFIG.write() {
        *config = updated.clone();
    }
    Some((old, updated))
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
const MAX_SHOWN_ROWS: usize = 8;

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

/// The identity of one gesture row's DELETE action.
fn delete_row_identity(index: usize) -> &'static str {
    const IDS: [&str; 32] = [
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
        "gesture_del_l",
        "gesture_del_m",
        "gesture_del_n",
        "gesture_del_o",
        "gesture_del_p",
        "gesture_del_q",
        "gesture_del_r",
        "gesture_del_s",
        "gesture_del_t",
        "gesture_del_u",
        "gesture_del_v",
        "gesture_del_w",
        "gesture_del_x",
        "gesture_del_y",
        "gesture_del_z",
        "gesture_del_aa",
        "gesture_del_ab",
        "gesture_del_ac",
        "gesture_del_ad",
        "gesture_del_ae",
        "gesture_del_af",
    ];
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
        match direction {
            'L' => out.push('←'),
            'R' => out.push('→'),
            'U' => out.push('↑'),
            'D' => out.push('↓'),
            other => out.push(other),
        }
    }
    out
}

/// The configured gestures in file order, each described in words a user recognises.
/// The running applications that are not excluded yet, for the add popup: an application
/// already on the list must not be offered twice.
/// Every application offered by the exclusion picker, before the configured ones are removed: the
/// installed ones plus whatever is running but installed outside the standard folders.
pub(crate) fn gesture_excluded_candidates() -> Vec<(String, String)> {
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

fn running_app_candidates(cfg: &crate::config::Config) -> Vec<(String, String)> {
    let excluded = &cfg.mouse.gesture_excluded_apps;
    // INSTALLED applications, not just the running ones: the list answers "which app must never act
    // on my gestures", and the app a user wants to exclude is usually the one they have open in
    // another window. Apps started outside the standard folders (a disk image, a build output) are
    // merged in from the running set, so nothing that is actually on screen is missing.
    let mut apps = gesture_excluded_candidates();
    apps.retain(|(bundle, _)| !excluded.iter().any(|id| id == bundle));
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
    if crate::mouse::gesture::recognizer::parse_key(&entry.gesture, 8).is_none() {
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
        // The left column is the button plus the stroke's arrows; the right column is the action's
        // own name.
        assert!(
            text.starts_with(&crate::mouse::shortcut::button_label(1)),
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
        // or the page shows two "Record" controls.
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
