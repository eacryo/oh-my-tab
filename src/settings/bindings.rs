//! Key-bindings page: the dynamic binding rows and the add-a-binding flow.
//!
//! Rows are data, not build-time furniture: adding, deleting or toggling a binding rebuilds the
//! affected group's rows from the config and hands the new height to the page canvas. Two
//! invariants from AGENTS.md shape the code:
//!
//! * a `SettingsButton`'s `tag` selects its hover/normal palette, so an item index must never
//!   travel in `setTag:` — the row controls are looked up by their own pointer instead;
//! * every view about to be released is unregistered first (`SettingsRow::forget`), because the
//!   disabled-hint/tooltip registries are keyed by the view's address.

use super::*;
use crate::callback_guard;

use crate::key_bindings::{Action, Trigger};

/// The binding row's height: the shared settings row height, so a binding row is the same height as
/// every other settings row (style doc §5, "Rows | 52 tall, control 32 tall, vertically centred").
/// It is an alias rather than its own number because a separate 40pt table row was exactly the
/// drift the layout grid exists to prevent.
pub(crate) const BINDING_ROW_H: f64 = SettingsLayout::SINGLE_LINE_ROW_H;
// The alias has to stay an alias: a private row height is the drift this replaced.
const _: () = assert!(BINDING_ROW_H == SettingsLayout::SINGLE_LINE_ROW_H);
const BINDING_CAP_SIZE: f64 = 20.0;
const BINDING_CAP_GAP: f64 = 2.0;
const BINDING_SWITCH_W: f64 = 44.0;
/// The row's trailing action buttons (Edit / Delete).
///
/// The height is the design system's minimum control hit target (§10, ≥ 28×28), not the mouse page's
/// 27pt: that page predates the rule, and copying it would have shrunk this row's only way to edit a
/// binding below the size the style document promises. The band is one settings row (56pt) tall, so
/// 28 fits with 14pt of slack above and below.
pub(super) const BINDING_BTN_W: f64 = 60.0;
pub(super) const BINDING_BTN_H: f64 = 28.0;
const BINDING_BTN_GAP: f64 = 8.0;
/// The keycap column's offset from the card's leading edge, so a row's chips align in a column
/// after the action name rather than following the name's length.
const BINDING_KEYCAP_COL: f64 = 216.0;
// The row's buttons are the only way to change a binding, so they carry the design system's minimum
// control hit target (§10, >= 28x28) rather than the mouse page's older 27pt. A control below it is a
// rule-10 defect, and this row would have introduced one.
const _: () = assert!(BINDING_BTN_H >= 28.0);
const _: () = assert!(BINDING_BTN_W >= 28.0);
// They must also fit the row without touching its edges, or the row's own height would have to grow
// and every row below it would move.
const _: () = assert!(BINDING_BTN_H < BINDING_ROW_H);
// The keycap column must be wide enough for a legible action-name box beside the chips. Chip
// *placement* is not asserted here: a run that does not fit its column slides left as a unit
// (`keycap_run_origin`), and the real fit -- measured text widths included, for chords whose key name
// is longer than one glyph -- is asserted by `every_legal_chord_fits_before_the_edit_button`.
const _: () = assert!(BINDING_KEYCAP_COL > 0.0);
/// The card's inner padding: `SettingsLayout::label_x` horizontally and its card bottom inset
/// vertically, so the rows line up with every other card's content. `padding_matches_the_shared_layout`
/// pins the two against those tokens, so moving a token cannot silently misalign this table.
pub(crate) const BINDING_PAD_X: f64 = 16.0;
pub(crate) const BINDING_PAD_Y: f64 = 0.0;
const BINDING_COL_GAP: f64 = 10.0;
/// The add panel's geometry. Named here rather than at the call sites so the panel's size, its
/// padding and its control rows stay one set of numbers (the mouse page's panel has its own; the
/// two are separate surfaces).
const PANEL_W: f64 = 440.0;
const PANEL_H: f64 = 200.0;
/// One control row's height inside the panel, and the panel's own edge padding.
const PANEL_ROW_H: f64 = 26.0;
/// The width of a panel row's leading label column, and the record button's own width.
const PANEL_LABEL_W: f64 = 88.0;
/// The height of one panel label line.
const PANEL_LABEL_H: f64 = 20.0;
const PANEL_RECORD_W: f64 = 134.0;
/// The gap between the panel's Cancel and OK buttons.
const PANEL_BTN_GAP: f64 = 8.0;
const PANEL_PAD: f64 = 16.0;
/// The panel's action/OK/Cancel button size.
const PANEL_BTN_H: f64 = 28.0;
const PANEL_BTN_W: f64 = 88.0;
/// Distance from the panel's top edge to each control row.
const PANEL_ACTION_Y: f64 = 150.0;
const PANEL_CHORD_Y: f64 = 110.0;
const PANEL_MESSAGE_Y: f64 = 76.0;
const PANEL_BUTTON_Y: f64 = 24.0;

/// The footer band that carries the full-width "Add binding" action below the last row, inside the
/// same card (the mouse page's "Add mapping" row). It is part of the card's height, so the canvas
/// moves the rows below it by exactly this much.
pub(crate) const BINDING_FOOTER_H: f64 = 44.0;

/// The footer band's padding above and below its button. Equal on both sides is what centres the
/// button in the band; the band's height and this padding are the only two numbers involved, so
/// changing either can only move the button symmetrically.
const BINDING_FOOTER_PAD: f64 = (BINDING_FOOTER_H - BINDING_BTN_H) / 2.0;
/// Where the footer's button sits, measured from the band's top edge. Production uses this and the
/// test asserts it, so the centring rule has exactly one definition.
fn footer_control_offset() -> f64 {
    BINDING_FOOTER_PAD
}
// "Centred" is only meaningful when the band is taller than what it holds.
const _: () = assert!(BINDING_FOOTER_H > BINDING_BTN_H);

/// A rendered row. Owns every view it created so a rebuild can unregister and remove them.
pub(crate) struct BindingRow {
    /// Index into `key_bindings.items`: what this row edits, what its switch toggles, and what its
    /// Edit / Delete buttons act on.
    item: usize,
    keycaps: Vec<*mut AnyObject>,
    label: *mut AnyObject,
    toggle: *mut AnyObject,
    /// The row's own actions. They carry no selection state: each addresses its binding by the
    /// item index the address-keyed registry maps it to.
    edit: *mut AnyObject,
    delete: *mut AnyObject,
    separator: *mut AnyObject,
}

impl BindingRow {
    /// The controls that carry a target/action, and so are the ones a click can arrive on.
    fn interactive(&self) -> [*mut AnyObject; 3] {
        [self.toggle, self.edit, self.delete]
    }

    /// Every view that may be registered in the address-keyed tooltip/hint registries. The label
    /// joins the controls because the master switch dims it.
    fn registered(&self) -> [*mut AnyObject; 4] {
        [self.toggle, self.edit, self.delete, self.label]
    }

    fn views(&self) -> Vec<*mut AnyObject> {
        let mut views = self.keycaps.clone();
        views.push(self.label);
        views.push(self.toggle);
        views.push(self.edit);
        views.push(self.delete);
        views.push(self.separator);
        views
    }
}

/// A recorded chord waiting for the user's confirmation: the action it would run and the chord as
/// it will be written. Nothing reaches the config until the confirm button is pressed, so the add
/// flow keeps its "choose, record, then agree" shape.
static BINDING_DRAFT: LazyLock<Mutex<Option<(Action, String)>>> =
    LazyLock::new(|| Mutex::new(None));

/// The binding the open panel is editing, as an index into `key_bindings.items`; `None` means the
/// panel is adding a new one.
///
/// The panel is one surface for both flows — the fields, the recorder and the confirm step are the
/// same — so the difference lives in this one value rather than in a second panel: `None` appends on
/// confirm, `Some(index)` rewrites that entry in place. An index is only read while the panel is
/// open, and every path that changes the list's shape closes the panel first, so it cannot outlive
/// the list it was resolved against.
static BINDING_EDITING: MainThreadSlot<Option<EditTarget>> = unsafe { MainThreadSlot::new(None) };

/// The binding the open panel is editing: its index **and** the identity it had when the panel opened.
///
/// The index alone is not enough to write safely. A config reload can change the list while the panel
/// is open, and an index that still resolves can then point at a *different* binding: editing `B` in
/// `[A, B, C]`, having `B` removed externally, reloading to `[A, C]`, and confirming would overwrite
/// `C` — silently destroying a binding the user never chose. The identity is what makes the write
/// safe: confirm compares it against the entry the index currently resolves to and refuses when they
/// disagree, instead of trusting a number that outlived its meaning.
#[derive(Clone, Debug, PartialEq, Eq)]
struct EditTarget {
    item: usize,
    /// The entry's identity: the pair the row was drawn from, which is what the user saw and chose.
    action: String,
    trigger: String,
}

/// The item the open panel is editing, or `None` when it is adding.
fn binding_editing() -> Option<usize> {
    BINDING_EDITING
        .lock()
        .ok()
        .and_then(|slot| slot.as_ref().map(|t| t.item))
}

/// The whole edit target, for the write path's identity check.
fn editing_target() -> Option<EditTarget> {
    BINDING_EDITING.lock().ok().and_then(|slot| slot.clone())
}

/// Enter edit mode (`Some`) or add mode (`None`). A failed borrow leaves the previous mode, which
/// can only mean a nested call on this same thread; the panel is not rebuilt in that case.
fn set_binding_editing(editing: Option<EditTarget>) {
    if let Ok(mut slot) = BINDING_EDITING.lock() {
        *slot = editing;
    }
}

/// Sender view address -> item index, for the row controls. Rebuilt with the rows and cleared
/// before they are released, so a click can never reach a freed address.
static ROW_CONTROLS: LazyLock<Mutex<HashMap<usize, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The add panel and the views inside it whose text or enabled state changes while it is open.
///
/// Each is a bare pointer into a window this module owns; every one is cleared in
/// `close_binding_panel` *before* the panel is released, because releasing the panel deallocates its
/// content view's whole subtree. They live in `MainThreadSlot`s like the mouse page's panel slots,
/// so a background thread cannot reach them and the borrow is checked at the call site.
static PANEL: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
static PANEL_ACTION: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
static PANEL_CHORD_LABEL: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
static PANEL_RECORD: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
static PANEL_MESSAGE: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
static PANEL_OK: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
static PANEL_DIM: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };

fn register_control(view: *mut AnyObject, item: usize) {
    if view.is_null() {
        return;
    }
    if let Ok(mut controls) = ROW_CONTROLS.lock() {
        controls.insert(view as usize, item);
    }
}

/// The item index a row control belongs to, or `None` when the sender is not one of ours.
fn control_item(sender: *mut AnyObject) -> Option<usize> {
    ROW_CONTROLS
        .lock()
        .ok()
        .and_then(|controls| controls.get(&(sender as usize)).copied())
}

/// The selected row index, as `--e2e-state` publishes it.
///
/// The page has no selection any more: every row carries its own Edit and Delete actions, so there
/// is no index that a page-level control has to be aimed at. The field the contract publishes stays
/// for the readers that parse it (`scripts/e2e/settings-layout.sh`), and is always `null` now.
pub(crate) fn selected_binding_index() -> Option<usize> {
    None
}

/// Smoke check for the engine's "this app is frontmost" flag wiring.
///
/// The flag lets a window action's chord pass through while our own window is up, so a settings text
/// field keeps its Option+arrow editing keys. The tap reads it on its own thread; two things are
/// decidable from the main thread:
///
/// * the controller class answers both activation selectors, so the publication has somewhere to
///   arrive (a missing or renamed registration leaves the flag permanently false);
/// * publishing then reading round-trips, so the tap cannot read a stale or unwired slot.
///
/// The notification delivery itself is NOT asserted: a smoke run never makes this process the active
/// application, so AppKit does not deliver it. Claiming to check it here would be asserting
/// `false == false`. Recorded in `docs/review-backlog.md`.
pub(crate) unsafe fn debug_validate_self_frontmost_wiring() -> bool {
    // The notifications are registered on the controller the app installs as their observer, which is
    // `OhMyTabController` (see `create_controller`), not the menu target class.
    let cls = objc2::class!(OhMyTabController);
    for selector in [
        sel!(handleSelfBecameActive:),
        sel!(handleSelfResignedActive:),
    ] {
        let responds: bool = msg_send![cls, instancesRespondToSelector: selector];
        if !responds {
            log_info!("[smoke-settings-layout] the controller does not answer {selector:?}");
            return false;
        }
    }
    // Publish both ways and confirm the tap's reader sees each value.
    let before = crate::key_bindings::debug_self_frontmost();
    crate::key_bindings::publish_self_frontmost(true);
    let saw_true = crate::key_bindings::debug_self_frontmost();
    crate::key_bindings::publish_self_frontmost(false);
    let saw_false = crate::key_bindings::debug_self_frontmost();
    // Leave the flag as the app's own state says it is, not as this probe left it.
    let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
    let active: bool = msg_send![nsapp, isActive];
    crate::key_bindings::publish_self_frontmost(active);
    if saw_true && !saw_false {
        return true;
    }
    log_info!(
        "[smoke-settings-layout] the self-frontmost flag does not round-trip (before={before}, saw_true={saw_true}, saw_false={saw_false})"
    );
    false
}

/// Smoke check that the footer's "Add binding" button follows the master switch.
///
/// It is never disabled for an empty list — adding the first binding is exactly what the user needs
/// there — so the master switch is the only thing that may freeze it. Like the row check this reads
/// AppKit's own `isEnabled`, because that is where the state lives.
pub(crate) unsafe fn debug_validate_footer_button_gating() -> bool {
    let button = with_settings_ui(|ui| ui.as_ref().map(|ui| ui.key_bindings_footer_button))
        .unwrap_or(std::ptr::null_mut());
    if button.is_null() {
        return true; // The page is not built: nothing to validate.
    }
    let master_on = CONFIG
        .read()
        .map(|cfg| cfg.key_bindings.enabled)
        .unwrap_or(false);
    let enabled: bool = msg_send![button, isEnabled];
    if enabled != master_on {
        log_info!(
            "[smoke-settings-layout] the add button is gated wrong: enabled={enabled} (want {master_on})"
        );
        return false;
    }
    true
}

/// Smoke check that every row's own actions follow the master switch.
///
/// The row controls are the only way to change a binding now, so the rule "with the feature off,
/// nothing on this page may be edited" rests entirely on this pass. Before the redesign the footer's
/// `−` had its own assertion; with Edit and Delete moved into the rows, the assertion has to cover
/// the controls that actually exist, or a row whose button stayed live would let a binding be
/// changed while the feature is off.
///
/// It is checked on the real view tree because `isEnabled` lives in AppKit's cell, which a pure rule
/// cannot observe. The caller drives the production enable pass first.
pub(crate) unsafe fn debug_validate_row_actions_follow_master_switch() -> bool {
    let rows = with_settings_ui(|ui| {
        ui.as_ref().map(|ui| {
            ui.key_binding_rows
                .iter()
                .map(|row| row.interactive())
                .collect::<Vec<_>>()
        })
    })
    .unwrap_or_default();
    if rows.is_empty() {
        return true; // The page is not built, or the list is empty: nothing to validate.
    }
    let master_on = CONFIG
        .read()
        .map(|cfg| cfg.key_bindings.enabled)
        .unwrap_or(false);
    for controls in rows {
        for control in controls {
            if control.is_null() {
                continue;
            }
            let enabled: bool = msg_send![control, isEnabled];
            if enabled != master_on {
                log_info!(
                    "[smoke-settings-layout] a row action is gated wrong: enabled={enabled} (want {master_on})"
                );
                return false;
            }
        }
    }
    true
}

/// Rebuild the page's rows from `cfg`. Called on every open, after every edit, and after restore
/// defaults; the page canvas re-flows the single card from the height handed over here.
pub(crate) unsafe fn render_binding_rows(ui: &mut SettingsUi, cfg: &Config) {
    // The footer is one container view holding its joined +/− control and the caption. It is removed
    // as a unit: rebuilding just the rows would leave every previous band in place, and the views
    // would accumulate on each edit.
    if !ui.key_bindings_footer.is_null() {
        // Unregister before removing: the button carries a disabled-hint registration keyed by its
        // address, and that table outlives the view. A stale key would later be handed a recycled
        // address, so a new button would inherit the old one's hint state.
        if !ui.key_bindings_footer_button.is_null() {
            SettingsRow::forget(ui.key_bindings_footer_button);
        }
        let _: () = msg_send![ui.key_bindings_footer, removeFromSuperview];
        ui.key_bindings_footer = std::ptr::null_mut();
        ui.key_bindings_footer_button = std::ptr::null_mut();
    }
    // Tear the previous rows down first: unregister, then remove. The order matters (see the module
    // comment), and the address map is cleared in the same pass.
    for row in ui.key_binding_rows.drain(..) {
        for control in row.registered() {
            if !control.is_null() {
                SettingsRow::forget(control);
            }
        }
        for view in row.views() {
            if !view.is_null() {
                let _: () = msg_send![view, removeFromSuperview];
            }
        }
        // A separator-less last row carries a null view; the loop's null check skips it.
    }
    if let Ok(mut controls) = ROW_CONTROLS.lock() {
        controls.clear();
    }

    let items = cfg.key_bindings.items.clone().unwrap_or_default();
    let doc = ui.key_bindings_doc;
    let _: () = msg_send![ui.key_bindings_empty, setHidden: !items.is_empty()];

    let doc_frame: NSRect = msg_send![doc, frame];
    let width = doc_frame.size.width.max(1.0);
    // The rows fill the card from its top inset down; the card's own surface is the only chrome, and
    // its section header sits above the card, drawn by the page canvas.
    let mut y = BINDING_PAD_Y;
    for (item, binding) in items.iter().enumerate() {
        // A flat list in config order: the user's arrangement is the page's arrangement, and an
        // entry that names an unknown action stays visible (its raw id is shown) instead of
        // vanishing from a list that is the only place to delete it. Config validation drops such
        // an entry on the next load, so it never survives a restart.
        let row = render_row(cfg, doc, width, y, item, binding);
        ui.key_binding_rows.push(row);
        y += BINDING_ROW_H;
    }
    // One row of height even when empty: the empty-state hint sits in that band, and a
    // zero-height card would clip it away.
    let rows_h = (y - BINDING_PAD_Y).max(BINDING_ROW_H);
    let card_h = card_height(rows_h);
    render_footer(cfg, ui, doc, width, rows_h);
    let _: () = msg_send![doc, setFrameSize: NSSize::new(width, card_h)];
    // The canvas owns the card's place: hand it the height and it shifts the card's origin down
    // by the growth, so the top edge stays put and the rows below follow.
    ui.page_canvases[SETTINGS_KEY_BINDINGS_PAGE_INDEX].set_row_consume(ui.key_bindings_row, card_h);
    ui.key_binding_items_rendered = binding_list_shape(&items);
}

/// The card's height for a given rows band: the rows, plus exactly one footer band, plus the card's
/// bottom inset. One function so the renderer and its assertion cannot drift apart.
fn card_height(rows_h: f64) -> f64 {
    rows_h + BINDING_FOOTER_H + BINDING_PAD_Y
}

/// The identity of the rendered list: which action each row runs and which chord it holds, in
/// order. `enabled` is deliberately absent — a switch flip needs no rebuild, and rebuilding would
/// destroy the switch that is currently dispatching its own action.
fn binding_list_shape(items: &[crate::key_bindings::KeyBinding]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|item| (item.action.clone(), item.trigger.clone()))
        .collect()
}

/// Whether a config change altered the list's identity (order, actions or chords) rather than just
/// its switch states. A same-shape change needs only its switch states synced, so the rows — and the
/// button that is currently dispatching its own action — are left alone.
fn shape_changed(rendered: &[(String, String)], now: &[(String, String)]) -> bool {
    rendered != now
}

/// Where one row's contents sit, derived from the card's width in one place.
///
/// The renderer and its regression test both read this, so the order the row is drawn in cannot drift
/// from the order the test asserts: restating the arithmetic in the test is exactly how the
/// "keycaps before the action name" defect shipped unnoticed, because the test agreed with itself
/// rather than with the renderer.
#[derive(Debug, Clone, Copy, PartialEq)]
struct RowLayout {
    /// The action name's box: starts at the card's leading padding and stops before the keycap column.
    label_x: f64,
    label_right: f64,
    /// The keycap column: its preferred left edge, and the right edge the run may never cross.
    ///
    /// The run is **right-bounded** rather than merely starting at a fixed offset. A preferred offset
    /// alone is not enough: the widest legal chord (four modifiers plus a long-named key such as
    /// "Page Down") is wider than the space that leaves, and it would cross the Edit button -- which
    /// is exactly the overlap `--smoke-settings-layout` reported. The right bound is what makes the
    /// row safe for every legal chord: a run that does not fit its column is shifted left as a whole
    /// (like a right-aligned table cell), and the label is clipped to end before it.
    keycap_x: f64,
    keycap_right: f64,
    /// The trailing controls, left to right: Edit, Delete, switch.
    edit_x: f64,
    delete_x: f64,
    switch_x: f64,
}

/// Derive one row's column geometry from the card's content width.
fn row_layout(width: f64) -> RowLayout {
    let switch_x = width - BINDING_PAD_X - BINDING_SWITCH_W;
    let delete_x = switch_x - BINDING_COL_GAP - BINDING_BTN_W;
    let edit_x = delete_x - BINDING_BTN_GAP - BINDING_BTN_W;
    let keycap_x = BINDING_PAD_X + BINDING_KEYCAP_COL;
    RowLayout {
        label_x: BINDING_PAD_X,
        label_right: keycap_x - BINDING_COL_GAP,
        keycap_x,
        keycap_right: edit_x - BINDING_COL_GAP,
        edit_x,
        delete_x,
        switch_x,
    }
}

/// Where one row's chips actually go, given their measured widths.
///
/// Pure, and shared with the regression test, so the chip run the renderer draws is the run the test
/// checks. The run prefers the aligned column (`cols.keycap_x`) and slides left as a unit when it
/// would cross `cols.keycap_right`; the label is then clipped to end before the run's real left edge.
/// Returns the run's left edge and the width the label may occupy.
fn keycap_run_origin(cols: RowLayout, chip_widths: &[f64]) -> (f64, f64) {
    let gap = BINDING_CAP_GAP;
    let run_w: f64 =
        chip_widths.iter().sum::<f64>() + gap * (chip_widths.len().saturating_sub(1)) as f64;
    // Preferred: the aligned column. Never past the right bound, and never left of the label's own
    // start (a run wider than the whole row is clamped by the chip width cap upstream).
    let left = (cols.keycap_right - run_w)
        .min(cols.keycap_x)
        .max(cols.label_x);
    (left, (left - BINDING_COL_GAP - cols.label_x).max(0.0))
}

/// Sync the page from a config that changed outside this page (a menu toggle, a reloaded file):
/// the master switch and the add row always follow, the rows only when their shape changed, and a
/// same-shape change just moves each row's switch to the new state.
pub(crate) unsafe fn sync_from_config(ui: &mut SettingsUi, cfg: &Config) {
    let _: () = msg_send![
        ui.key_bindings_enabled,
        setState: if cfg.key_bindings.enabled { 1isize } else { 0isize }
    ];
    let items = cfg.key_bindings.items.clone().unwrap_or_default();
    if shape_changed(&ui.key_binding_items_rendered, &binding_list_shape(&items)) {
        // The shape changed behind the page's back (an external edit, a reload, restore defaults),
        // so the rows have to be rebuilt: each one carries the index it acts on.
        render_binding_rows(ui, cfg);
    } else {
        for row in &ui.key_binding_rows {
            let Some(binding) = items.get(row.item) else {
                continue;
            };
            if !row.toggle.is_null() {
                let _: () = msg_send![
                    row.toggle,
                    setState: if binding.enabled { 1isize } else { 0isize }
                ];
            }
        }
    }
    update_binding_controls_enabled(ui);
}

/// Render one row and return its handle. `y` is the row's top edge in the (flipped) document.
unsafe fn render_row(
    cfg: &Config,
    doc: *mut AnyObject,
    width: f64,
    y: f64,
    item: usize,
    binding: &crate::key_bindings::KeyBinding,
) -> BindingRow {
    let target = MENU_TARGET
        .lock()
        .unwrap()
        .map(|t| t.0)
        .unwrap_or(std::ptr::null_mut());
    let action = Action::from_config_id(&binding.action);
    // The row's own switch gates the binding; the master switch still applies on top (the runtime
    // table ANDs them), which is what the disabled state below communicates.
    let master_on = cfg.key_bindings.enabled;

    // Keycaps: one cell per *logical key*, in their own column after the action name. The chips are
    // laid out here but the *column* is a fixed offset from the card's leading edge, so every row's
    // keycaps line up regardless of how long its action name is.
    //
    // The chips come from `keycap_chips`, never from `keycap().chars()`: a key named "Page Down"
    // would otherwise be drawn one cell per character, so `⌥Page Down` became one 20pt chip per
    // character and ran into this row's Edit and Delete buttons. Each chip sizes itself to its own
    // label, and the run is bounded by the column so even the longest legal chord stops before the
    // buttons.
    let chips = Trigger::parse(&binding.trigger)
        .map(|trigger| trigger.keycap_chips())
        .unwrap_or_else(|_| vec![binding.trigger.clone()]);
    let cols = row_layout(width);
    // Measure every chip before placing any of them: the run is positioned as a unit against the
    // column's right bound, so its total width has to be known first.
    let chip_widths: Vec<f64> = chips
        .iter()
        .map(|label| {
            let text_w = measure_control_text_width(label);
            (text_w.ceil() + crate::theme::LEGEND_CHIP_TEXT_PAD_X * 2.0)
                .clamp(BINDING_CAP_SIZE, BINDING_CAP_SIZE * 8.0)
        })
        .collect();
    let (run_x, label_width_available) = keycap_run_origin(cols, &chip_widths);
    let mut keycaps = Vec::new();
    let mut cap_x = run_x;
    for (chip_label, chip_w) in chips.iter().zip(chip_widths.iter()) {
        keycaps.push(SettingsRow::legend_chip(
            doc,
            cap_x,
            y,
            BINDING_ROW_H,
            BINDING_CAP_SIZE,
            chip_label,
        ));
        cap_x += chip_w + BINDING_CAP_GAP;
    }
    let _ = label_width_available;

    // The trailing switch is built first: the shared row derives the label's width from the
    // control's leading edge, so the control has to exist before the row is laid out.
    let switch_x = cols.switch_x;
    let toggle = SettingsControl::switch(
        switch_x + BINDING_SWITCH_W,
        0.0,
        SettingsLayout::CONTROL_H,
        binding.enabled,
    );
    let _: () = msg_send![toggle, setTarget: target];
    let _: () = msg_send![toggle, setAction: sel!(handleBindingToggle:)];
    register_control(toggle, item);

    // Action name: the registry's localized label, never a config id.
    let text = action
        .map(crate::key_bindings::action_label)
        .unwrap_or_else(|| binding.action.clone());
    // The action name leads the row, and the label stops before the keycap column: this row has
    // content on both sides of the text, so it cannot derive its width from the trailing control.
    let label_x = cols.label_x;
    // End the name before the chips' real left edge, which may have slid left of the column.
    let label_right = (run_x - BINDING_COL_GAP).max(label_x);
    let (label, toggle) = SettingsRow::with_leading_keycaps(
        doc,
        label_x,
        label_right,
        y,
        BINDING_ROW_H,
        &text,
        toggle,
    );

    // A switch has no visible title, so its accessible name carries the whole statement: what it
    // controls, and with which chord.
    let toggle_name = tf("settings.binding_toggle_named", &[("action", &text)]);
    let toggle_name_ns = make_nsstring(&toggle_name);
    let _: () = msg_send![toggle, setAccessibilityLabel: toggle_name_ns];
    CFRelease(toggle_name_ns as *const c_void);
    // The label truncates rather than growing a hover bubble (none exists in the settings window):
    // the switch's accessible name above carries the full statement for assistive technologies.

    // The row's own actions, at the trailing edge before the switch. The pair mirrors the mouse
    // page's mapping rows (`settings/mapping.rs`), so a settings row's buttons read the same on both
    // pages. Each carries the item index in the address-keyed registry rather than in its tag: a
    // `SettingsButton`'s tag selects its hover palette (AGENTS.md).
    let delete_x = cols.delete_x;
    let edit_x = cols.edit_x;
    let btn_y = y + (BINDING_ROW_H - BINDING_BTN_H) / 2.0;
    let edit = SettingsButton::action(
        NSRect::new(
            NSPoint::new(edit_x, btn_y),
            NSSize::new(BINDING_BTN_W, BINDING_BTN_H),
        ),
        &t("settings.binding_edit"),
        target,
        sel!(handleBindingEdit:),
        SettingsButtonRole::ROW_ACTION,
    );
    let _: () = msg_send![doc, addSubview: edit];
    release_obj(edit);
    register_control(edit, item);
    // The buttons' visible titles name the verb; the accessible name says which binding it applies
    // to, because a screen reader reaches the row's controls without the label beside them.
    let edit_name = tf("settings.binding_edit_named", &[("action", &text)]);
    let edit_name_ns = make_nsstring(&edit_name);
    let _: () = msg_send![edit, setAccessibilityLabel: edit_name_ns];
    CFRelease(edit_name_ns as *const c_void);
    let delete = SettingsButton::action(
        NSRect::new(
            NSPoint::new(delete_x, btn_y),
            NSSize::new(BINDING_BTN_W, BINDING_BTN_H),
        ),
        &t("settings.binding_delete"),
        target,
        sel!(handleBindingDelete:),
        SettingsButtonRole::ROW_ACTION,
    );
    let _: () = msg_send![doc, addSubview: delete];
    release_obj(delete);
    register_control(delete, item);
    let delete_name = tf("settings.binding_delete_named", &[("action", &text)]);
    let delete_name_ns = make_nsstring(&delete_name);
    let _: () = msg_send![delete, setAccessibilityLabel: delete_name_ns];
    CFRelease(delete_name_ns as *const c_void);

    // A divider under every row, including the last: the footer band below draws its own top border,
    // so the last row's divider is what separates the rows from that band.
    let separator = SettingsRow::separator(doc, y + BINDING_ROW_H - 1.0, width);

    let row = BindingRow {
        item,
        keycaps,
        label,
        toggle,
        edit,
        delete,
        separator,
    };
    let tooltip_key = if master_on {
        "settings.binding_toggle"
    } else {
        "settings.tooltip_key_bindings_disabled"
    };
    for control in row.interactive() {
        SettingsRow::set_view_enabled_with_tooltip(control, master_on, Some(&t(tooltip_key)));
    }
    row
}

/// The footer band below the rows: a joined ＋ / − segmented control plus a caption naming what each
/// half does. Both halves act on the page's state, not on a row, so neither carries an item index.
///
/// The whole band lives in one container view (`ui.key_bindings_footer`). That is what makes a
/// rebuild safe: the renderer removes the previous container and builds a fresh one, so repeated
/// edits cannot accumulate footer views on top of each other.
///
/// A − with nothing selected has no target, so it says so by being disabled; ＋ is never disabled for
/// an empty list, because adding the first binding is exactly what the user needs there.
unsafe fn render_footer(
    cfg: &Config,
    ui: &mut SettingsUi,
    doc: *mut AnyObject,
    width: f64,
    rows_h: f64,
) {
    let target = MENU_TARGET
        .lock()
        .unwrap()
        .map(|t| t.0)
        .unwrap_or(std::ptr::null_mut());
    let master_on = cfg.key_bindings.enabled;
    let y = BINDING_PAD_Y + rows_h;

    let container: *mut AnyObject = msg_send![class!(NSView), alloc];
    let container: *mut AnyObject = msg_send![container, initWithFrame: NSRect::new(
        NSPoint::new(0.0, y),
        NSSize::new(width, BINDING_FOOTER_H)
    )];
    // No separator inside the band: the band's top edge is where the rows end, and the row above
    // already drew its own divider. A second line here reads as an extra rule under the button.
    let _: () = msg_send![doc, addSubview: container];

    // One full-width button, spanning the card's content width — the mouse page's "Add mapping" row.
    // It replaces the old joined ＋/－ segments: with Delete moved onto each row, the only page-level
    // action left is adding, and a single button says that without a legend beside it.
    let add = SettingsButton::action(
        NSRect::new(
            NSPoint::new(BINDING_PAD_X, footer_control_offset()),
            NSSize::new((width - BINDING_PAD_X * 2.0).max(1.0), BINDING_BTN_H),
        ),
        &t("settings.binding_add"),
        target,
        sel!(handleBindingAdd:),
        // A full-width row action, so it takes the doc's secondary button surface (§6) like the row
        // buttons above it. `Compact`'s field-family wash would leave its hover at 1.02:1 on the card.
        SettingsButtonRole::ROW_ACTION,
    );
    if add.is_null() {
        log_debug!("[keybind] footer add button unavailable");
        release_obj(container);
        return;
    }
    let _: () = msg_send![container, addSubview: add];
    // `SettingsButton::action` returns +1. The container retained it, so the creator's reference is
    // balanced here; without this every rebuild (each edit, each restore) leaks a button. The page
    // keeps the bare pointer, which stays valid while the container lives.
    release_obj(add);
    // Adding is never disabled for an empty list: adding the first binding is exactly what the user
    // needs there. The master switch is the only thing that freezes it.
    SettingsRow::set_view_enabled_with_tooltip(
        add,
        master_on,
        Some(&t("settings.tooltip_key_bindings_disabled")),
    );
    // The document view owns the container now; the page keeps the bare pointers so a rebuild can
    // remove the band and the master switch can freeze its button.
    release_obj(container);
    ui.key_bindings_footer = container;
    ui.key_bindings_footer_button = add;
}

/// Freeze the row controls while the master switch is off, and enable them again when it is on.
pub(crate) unsafe fn update_binding_controls_enabled(ui: &SettingsUi) {
    let master_on = match CONFIG.read() {
        Ok(cfg) => cfg.key_bindings.enabled,
        Err(_) => return,
    };
    let enabled = t("settings.binding_toggle");
    let disabled = t("settings.tooltip_key_bindings_disabled");
    let tooltip = if master_on { &enabled } else { &disabled };
    for row in &ui.key_binding_rows {
        for control in row.interactive() {
            if !control.is_null() {
                SettingsRow::set_view_enabled_with_tooltip(control, master_on, Some(tooltip));
            }
        }
        if !row.label.is_null() {
            SettingsRow::set_view_enabled_with_tooltip(row.label, master_on, Some(tooltip));
        }
    }
    // The footer's single "Add binding" button follows the master switch like every row control; it
    // is never disabled for an empty list, because adding the first binding is exactly what the user
    // needs there.
    if !ui.key_bindings_footer_button.is_null() {
        SettingsRow::set_view_enabled_with_tooltip(
            ui.key_bindings_footer_button,
            master_on,
            Some(&disabled),
        );
    }
    // An open panel is one of this page's editors, so the master switch freezes it too: turning the
    // feature off from the status-bar menu must not leave a live OK button that writes a binding.
    update_panel_enabled();
}

/// The add panel's action popup: the same list, read off the panel's own control.
pub(crate) fn panel_popup_action() -> Option<Action> {
    let popup = PANEL_ACTION.lock().ok().and_then(|slot| *slot)?;
    let index: isize = unsafe { msg_send![popup.0, indexOfSelectedItem] };
    add_action_at(index.max(0) as usize)
}

/// The add panel: one floating window where the action is chosen and the chord recorded, so the page
/// itself stays a plain list (macOS System Settings' "＋ opens a small window" model, which is what
/// the mouse page's mapping panel established).
///
/// Open the panel in its add mode: no prefilled binding, and confirm appends.
pub(crate) fn open_binding_panel() {
    open_binding_panel_inner(None);
}

/// Open the panel in its edit mode, prefilled from the binding at `item`.
///
/// The item index is validated against the config here rather than trusted from the row that drew
/// the button: a rebuild re-registers every index, so a stale one cannot arrive, and this is the
/// cheap second check on the same door.
fn open_binding_edit_panel(item: usize) {
    let len = CONFIG
        .read()
        .map(|cfg| cfg.key_bindings.items.as_ref().map_or(0, Vec::len))
        .unwrap_or(0);
    if item >= len {
        return;
    }
    open_binding_panel_inner(Some(item));
}

/// Build the panel. `editing` selects the mode: `None` adds a new binding, `Some(index)` edits the
/// one at that index. Both modes share the whole surface — the action popup, the chord recorder, the
/// message line and the buttons are the same — because the two differ only in what confirm writes
/// and in the values the fields start from.
///
/// Ownership: the panel is created `alloc`/`init` (+1) and `setReleasedWhenClosed: false`, so
/// `close_binding_panel` is the only thing that releases it. Every view it stores is taken out of its
/// slot *before* that release, because a slot is a bare pointer and the panel deallocates its content
/// view's subtree with it. Each subview's own `+1` is balanced right after `addSubview`, so the
/// panel's release is the last reference and nothing else may release them.
fn open_binding_panel_inner(editing: Option<usize>) {
    let existing = editing.and_then(|item| {
        CONFIG
            .read()
            .ok()
            .and_then(|cfg| cfg.key_bindings.items.clone())
            .and_then(|items| items.get(item).cloned())
    });
    // An edit that cannot read its own entry is not an edit: fall through to nothing rather than
    // opening a panel whose confirm would rewrite a stranger.
    if editing.is_some() && existing.is_none() {
        return;
    }
    // Capture the identity alongside the index, so confirm can tell "the same binding, still there"
    // from "a different binding that happens to sit at that index now".
    let editing = editing.map(|item| {
        let binding = existing.as_ref().expect("checked above");
        EditTarget {
            item,
            action: binding.action.clone(),
            trigger: binding.trigger.clone(),
        }
    });
    unsafe {
        // A recording in flight belongs to the panel that started it; reopening must not leave two.
        if PANEL.lock().unwrap().is_some() {
            close_binding_panel();
        }
        clear_draft();
        set_binding_editing(editing.clone());
        // Edit mode opens with a draft already holding the binding's own action and chord. Without it
        // the confirm button would stay disabled until the user re-recorded: changing only the action
        // is the commonest edit, and it would be impossible to commit. The seeded draft is the row's
        // own pair, so confirming it unchanged rewrites that entry with the values it already has.
        if let Some(binding) = existing.as_ref() {
            if let Some(action) = Action::from_config_id(&binding.action) {
                if let Ok(mut draft) = BINDING_DRAFT.lock() {
                    *draft = Some((action, binding.trigger.clone()));
                }
            }
        }
        let target = match MENU_TARGET.lock().unwrap().map(|t| t.0) {
            Some(target) => target,
            None => return,
        };
        let win = match with_settings_ui(|ui| ui.as_ref().map(|ui| ui.window)) {
            Some(win) if !win.is_null() => win,
            _ => return,
        };
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(PANEL_W, PANEL_H));
        let panel: *mut AnyObject = msg_send![class!(NSPanel), alloc];
        let panel: *mut AnyObject = msg_send![panel, initWithContentRect: frame, styleMask: 0u64, backing: 2u64, defer: false];
        apply_settings_window_appearance(panel);
        let _: () = msg_send![panel, setReleasedWhenClosed: false];
        let _: () = msg_send![panel, setOpaque: false];
        let _: () = msg_send![panel, setLevel: 3isize]; // NSFloatingWindowLevel
        let clear_ns: *mut AnyObject = msg_send![class!(NSColor), clearColor];
        let _: () = msg_send![panel, setBackgroundColor: clear_ns];
        let ve: *mut AnyObject = msg_send![class!(NSView), alloc];
        let ve: *mut AnyObject = msg_send![ve, initWithFrame: frame];
        let _: () = msg_send![ve, setWantsLayer: true];
        let ve_layer: *mut AnyObject = msg_send![ve, layer];
        // A dropdown/dialog panel takes `radius-panel`, not `radius-control` (style doc §6).
        let _: () = msg_send![ve_layer, setCornerRadius: crate::theme::RADIUS_PANEL];
        // The shadow is drawn outside the panel's bounds, so the layer must not clip to them.
        let _: () = msg_send![ve_layer, setMasksToBounds: false];
        layer_set_background(
            ve_layer,
            crate::ffi::hex_to_cg_color(settings_palette().card_bg),
        );
        let _: () = msg_send![ve_layer, setBorderWidth: 1.0f64];
        crate::ffi::layer_set_border(
            ve_layer,
            crate::ffi::hex_to_cg_color(settings_palette().card_border),
        );
        // A floating panel carries the medium elevation (style doc §7: popovers and dropdowns).
        crate::ffi::layer_set_shadow_color(
            ve_layer,
            crate::ffi::hex_to_cg_color(crate::theme::ELEVATION_MED_SHADOW_COLOR),
        );
        let _: () =
            msg_send![ve_layer, setShadowOpacity: crate::theme::ELEVATION_MED_SHADOW_OPACITY];
        let _: () = msg_send![ve_layer, setShadowRadius: crate::theme::ELEVATION_MED_SHADOW_RADIUS];
        let _: () = msg_send![ve_layer, setShadowOffset: NSSize::new(0.0, crate::theme::ELEVATION_MED_SHADOW_OFFSET_Y)];
        let _: () = msg_send![panel, setContentView: ve];
        release_obj(ve);

        // Action row.
        let action_label = make_label_with_role(
            PANEL_PAD,
            PANEL_ACTION_Y,
            PANEL_LABEL_W,
            PANEL_LABEL_H,
            &t("settings.binding_panel_action"),
            SettingsTextRole::Secondary,
        );
        // The shared label helper returns +1 and leaves attaching to the caller, so each label is
        // added and then balanced: the panel's content view owns it from here.
        let _: () = msg_send![ve, addSubview: action_label];
        release_obj(action_label);
        let titles: Vec<String> = picker_actions()
            .iter()
            .map(|action| crate::key_bindings::action_label(*action))
            .collect();
        let titles: Vec<&str> = titles.iter().map(String::as_str).collect();
        // In edit mode the popup opens on the binding's own action; in add mode on the first entry.
        let selected = picker_index_for(
            existing
                .as_ref()
                .and_then(|binding| Action::from_config_id(&binding.action)),
        );
        let popup = make_popup(
            crate::settings::select::SelectOwner::SettingsPage,
            PANEL_PAD + PANEL_LABEL_W,
            PANEL_ACTION_Y - 2.0,
            PANEL_W - PANEL_PAD * 2.0 - PANEL_LABEL_W,
            PANEL_ROW_H,
            &titles,
            selected,
        );
        let _: () = msg_send![popup, setTarget: target];
        let _: () = msg_send![popup, setAction: sel!(handleBindingPanelAction:)];
        let _: () = msg_send![ve, addSubview: popup];
        release_obj(popup);
        *PANEL_ACTION.lock().unwrap() = Some(ObjPtr::new(popup));

        // Chord row: the recorded chord (or "not recorded yet") plus its record button. In edit mode
        // the row starts on the binding's current chord, which is what the user came to change.
        let chord_label = make_label_with_role(
            PANEL_PAD,
            PANEL_CHORD_Y,
            PANEL_LABEL_W,
            20.0,
            &t("settings.binding_panel_chord"),
            SettingsTextRole::Secondary,
        );
        let _: () = msg_send![ve, addSubview: chord_label];
        release_obj(chord_label);
        let chord_text = chord_text_for(existing.as_ref());
        let chord_value = make_label_with_role(
            PANEL_PAD + PANEL_LABEL_W,
            PANEL_CHORD_Y,
            PANEL_W - PANEL_PAD * 2.0 - PANEL_LABEL_W - PANEL_RECORD_W - PANEL_BTN_GAP,
            20.0,
            &chord_text,
            SettingsTextRole::Primary,
        );
        let _: () = msg_send![ve, addSubview: chord_value];
        // Balance the helper's +1: the content view owns the label now, and the slot holds a bare
        // alias so `update_panel_ok` can rewrite its text.
        release_obj(chord_value);
        *PANEL_CHORD_LABEL.lock().unwrap() = Some(ObjPtr::new(chord_value));
        let record = SettingsButton::action(
            NSRect::new(
                NSPoint::new(PANEL_W - PANEL_PAD - PANEL_RECORD_W, PANEL_CHORD_Y - 4.0),
                NSSize::new(PANEL_RECORD_W, PANEL_BTN_H),
            ),
            &t("settings.binding_add_record"),
            target,
            sel!(handleBindingPanelRecord:),
            SettingsButtonRole::Action,
        );
        let _: () = msg_send![ve, addSubview: record];
        release_obj(record);
        *PANEL_RECORD.lock().unwrap() = Some(ObjPtr::new(record));

        // The panel's own message line: a refused chord is reported here, where the user is looking,
        // instead of in a page row behind the dim layer.
        let message = make_label_with_role(
            PANEL_PAD,
            PANEL_MESSAGE_Y,
            PANEL_W - PANEL_PAD * 2.0,
            20.0,
            "",
            SettingsTextRole::Secondary,
        );
        let _: () = msg_send![ve, addSubview: message];
        release_obj(message);
        *PANEL_MESSAGE.lock().unwrap() = Some(ObjPtr::new(message));

        // Cancel / OK.
        let cancel = SettingsButton::action(
            NSRect::new(
                NSPoint::new(
                    PANEL_W - PANEL_PAD - PANEL_BTN_W * 2.0 - PANEL_BTN_GAP,
                    PANEL_BUTTON_Y,
                ),
                NSSize::new(PANEL_BTN_W, PANEL_BTN_H),
            ),
            &t("settings.recording_cancel"),
            target,
            sel!(handleBindingPanelCancel:),
            SettingsButtonRole::Footer,
        );
        let _: () = msg_send![ve, addSubview: cancel];
        release_obj(cancel);
        // The confirm button names the mode: adding writes a new binding, editing rewrites one.
        let ok_title = if editing.is_some() {
            t("settings.binding_edit_confirm")
        } else {
            t("settings.binding_add_confirm")
        };
        let ok = SettingsButton::action(
            NSRect::new(
                NSPoint::new(PANEL_W - PANEL_PAD - PANEL_BTN_W, PANEL_BUTTON_Y),
                NSSize::new(PANEL_BTN_W, PANEL_BTN_H),
            ),
            &ok_title,
            target,
            sel!(handleBindingPanelConfirm:),
            SettingsButtonRole::Primary,
        );
        let _: () = msg_send![ve, addSubview: ok];
        release_obj(ok);
        *PANEL_OK.lock().unwrap() = Some(ObjPtr::new(ok));
        *PANEL.lock().unwrap() = Some(ObjPtr::new(panel));

        // Centre on the settings window, then dim behind it (the panel floats above the dim).
        let win_frame: NSRect = msg_send![win, frame];
        let pf: NSRect = msg_send![panel, frame];
        let _: () = msg_send![panel, setFrameOrigin: NSPoint::new(
            win_frame.origin.x + (win_frame.size.width - pf.size.width) / 2.0,
            win_frame.origin.y + (win_frame.size.height - pf.size.height) / 2.0
        )];
        let content: *mut AnyObject = msg_send![win, contentView];
        let content_bounds: NSRect = msg_send![content, bounds];
        let dim: *mut AnyObject = msg_send![class!(NSView), alloc];
        let dim: *mut AnyObject = msg_send![dim, initWithFrame: content_bounds];
        let _: () = msg_send![dim, setWantsLayer: true];
        let dim_layer: *mut AnyObject = msg_send![dim, layer];
        layer_set_background(dim_layer, hex_to_cg_color(crate::theme::MODAL_SCRIM));
        let _: () = msg_send![content, addSubview: dim];
        release_obj(dim);
        *PANEL_DIM.lock().unwrap() = Some(ObjPtr::new(dim));
        let _: () = msg_send![panel, orderFrontRegardless];
    }
    update_panel_ok();
    log_debug!(
        "[keybind] binding panel opened ({})",
        if editing.is_some() { "edit" } else { "add" }
    );
}

/// Whether the add panel is open (the page switch uses this to close it).
pub(crate) fn panel_is_open() -> bool {
    PANEL.lock().map(|slot| slot.is_some()).unwrap_or(false)
}

/// Whether each panel control may act, given the master switch, the draft and the recorder.
///
/// One pure predicate owns the rule, and **both** the UI (`update_panel_enabled`) and the handlers
/// (`handle_panel_record`, `handle_panel_confirm`) read it, so a regression test exercises the same
/// decision the product makes instead of a copy of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PanelGates {
    pub(crate) record: bool,
    pub(crate) ok: bool,
    pub(crate) popup: bool,
}

/// `master` = the feature switch, `draft` = a recorded chord is held, `recording` = the recorder is
/// currently running.
pub(crate) fn panel_gates(master: bool, draft: bool, recording: bool) -> PanelGates {
    PanelGates {
        record: master && !recording,
        // OK needs a chord to write *and* the recorder stopped: confirming mid-recording would write
        // a draft the recorder is about to replace.
        ok: master && draft && !recording,
        popup: master,
    }
}

/// Whether the master switch currently allows editing bindings at all.
pub(crate) fn master_switch_on() -> bool {
    CONFIG
        .read()
        .map(|cfg| cfg.key_bindings.enabled)
        .unwrap_or(false)
}

/// Freeze or unfreeze the add panel to match the master switch. Called from the switch handler and
/// from `update_panel_ok`, so a panel left open while the feature is turned off cannot write, and
/// turning it back on restores the panel without reopening it.
pub(crate) fn update_panel_enabled() {
    let gates = panel_gates(
        master_switch_on(),
        has_draft(),
        super::mapping::binding_recording_pending(),
    );
    if let Some(record) = PANEL_RECORD.lock().ok().and_then(|slot| *slot) {
        unsafe {
            let tip = t("settings.tooltip_key_bindings_disabled");
            SettingsRow::set_view_enabled_with_tooltip(record.0, gates.record, Some(&tip));
        }
    }
    if let Some(ok) = PANEL_OK.lock().ok().and_then(|slot| *slot) {
        unsafe {
            let tip = t("settings.tooltip_key_bindings_disabled");
            SettingsRow::set_view_enabled_with_tooltip(ok.0, gates.ok, Some(&tip));
            apply_panel_button_dim(ok.0, gates.ok);
        }
    }
    if let Some(popup) = PANEL_ACTION.lock().ok().and_then(|slot| *slot) {
        unsafe {
            let _: () = msg_send![popup.0, setEnabled: gates.popup];
        }
    }
}

/// The chord row's text when no draft is held: in edit mode the binding's current chord, so the
/// prefilled row is not wiped by the first refresh; in add mode the "not recorded yet" placeholder.
fn panel_idle_chord_text() -> String {
    chord_text_for(edited_binding().as_ref())
}

/// Refresh the chord row and the buttons after any change to the draft or the recorder.
pub(crate) fn update_panel_ok() {
    // The buttons' enabled state has one owner (`update_panel_enabled`), because it depends on the
    // master switch, the recorder and the draft together.
    update_panel_enabled();
    if let Some(label) = PANEL_CHORD_LABEL.lock().ok().and_then(|slot| *slot) {
        let text = match BINDING_DRAFT.lock().ok().and_then(|draft| draft.clone()) {
            Some((_, trigger)) => Trigger::parse(&trigger)
                .map(|parsed| parsed.keycap())
                .unwrap_or(trigger),
            None => panel_idle_chord_text(),
        };
        unsafe { set_field(label.0, text) };
    }
}

/// Close the add panel and release it (idempotent). The recording, if any, is cancelled first so no
/// late result can reach a panel that is gone.
pub(crate) fn close_binding_panel() {
    // Cancel outside the slot-taking below: the recorder's cleanup marshals to this same thread and
    // must not observe a half-dismantled panel.
    if super::mapping::binding_recording_pending() {
        super::mapping::cancel_recording_from_main();
    }
    let panel = PANEL.lock().unwrap().take();
    let action = PANEL_ACTION.lock().unwrap().take();
    let chord_label = PANEL_CHORD_LABEL.lock().unwrap().take();
    let record = PANEL_RECORD.lock().unwrap().take();
    let message = PANEL_MESSAGE.lock().unwrap().take();
    let ok = PANEL_OK.lock().unwrap().take();
    let dim = PANEL_DIM.lock().unwrap().take();
    // Every view registered in an address-keyed table is unregistered while it is still alive: those
    // tables outlive the views, so a stale key either reaches freed memory or -- once a new view
    // reuses the address -- hands a new view's state to an old entry.
    unsafe {
        if let Some(popup) = action {
            super::select::forget_settings_select(popup.0);
        }
        for view in [record, ok].into_iter().flatten() {
            if !view.0.is_null() {
                SettingsRow::forget(view.0);
            }
        }
    }
    // The slots are empty now; the handles themselves are Copy, so this is a plain discard.
    let _ = (action, chord_label, record, message, ok);
    unsafe {
        if let Some(d) = dim {
            let _: () = msg_send![d.0, removeFromSuperview];
        }
        if let Some(p) = panel {
            let _: () = msg_send![p.0, orderOut: std::ptr::null::<AnyObject>()];
            release_obj(p.0);
        }
    }
    // The panel is gone, so the mode it was in goes with it: a later add must not inherit this
    // panel's edit target.
    set_binding_editing(None);
    clear_draft();
}

/// Show a styled panel button as disabled or usable.
///
/// These buttons carry their own layer fill, and `setEnabled:` does not dim a layer fill, so a
/// disabled primary button keeps its full accent and reads as pressable. The design system's
/// disabled rule is half opacity on the control's fill (style doc §10), applied here to the panel's
/// own two buttons rather than to every styled button in the app.
unsafe fn apply_panel_button_dim(button: *mut AnyObject, enabled: bool) {
    if button.is_null() {
        return;
    }
    let _: () = msg_send![button, setAlphaValue: if enabled {
        1.0f64
    } else {
        crate::theme::DISABLED_FILL_OPACITY
    }];
}

/// The panel's message line: a refusal, or empty.
pub(crate) fn set_panel_message(message: Option<&str>) {
    let Some(label) = PANEL_MESSAGE.lock().ok().and_then(|slot| *slot) else {
        return;
    };
    let text = message.unwrap_or("").to_string();
    unsafe {
        set_field(label.0, &text);
        let role = if message.is_some() {
            SettingsTextRole::Destructive
        } else {
            SettingsTextRole::Secondary
        };
        apply_settings_text_role(label.0, role);
    }
    if let Some(message) = message {
        crate::log_info!("[keybind] recording refused: {}", message);
    }
}

/// The add panel "Record chord": start the recorder for the action the panel has selected.
pub(crate) extern "C" fn handle_panel_record(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    callback_guard::void("handle_panel_record", || {
        // Same predicate as the button's own enablement: a click can arrive after the switch changed.
        if !panel_gates(
            master_switch_on(),
            has_draft(),
            super::mapping::binding_recording_pending(),
        )
        .record
        {
            return;
        }
        // The chord must be recorded for the action the panel is showing *now*; a stale draft from an
        // earlier action is dropped rather than confirmed against the new one.
        clear_draft_state();
        set_panel_message(None);
        let Some(action) = panel_popup_action() else {
            return;
        };
        unsafe { super::mapping::start_key_binding_recording(action) };
        update_panel_ok();
    });
}

/// The panel's action popup changed.
///
/// In **add** mode any recorded chord belonged to the previous action, so the draft is dropped and the
/// chord row goes back to its empty text: confirming it would bind a chord the user recorded for
/// something else.
///
/// In **edit** mode the chord belongs to the row, not to a recording session, so it follows the new
/// action instead of being thrown away. That is what lets a user change an action while keeping its
/// shortcut, and it is honest: the chord row keeps showing the chord that confirm will write.
pub(crate) extern "C" fn handle_panel_action_changed(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    callback_guard::void("handle_panel_action_changed", || {
        if let Some(action) = panel_popup_action() {
            let editing = binding_editing().is_some();
            let held = BINDING_DRAFT.lock().ok().and_then(|draft| draft.clone());
            let entry_trigger = edited_binding().map(|binding| binding.trigger);
            let next_draft = draft_after_action_change(editing, held, entry_trigger, action);
            if let Ok(mut draft) = BINDING_DRAFT.lock() {
                *draft = next_draft.clone();
            }
            // The message must name the action confirm would actually write: leaving the recorded
            // sentence in place would have the panel say one action while confirming another.
            match message_after_action_change(&next_draft) {
                Some(sentence) => set_panel_message(Some(&sentence)),
                None => set_panel_message(None),
            }
        }
        update_panel_ok();
    });
}

/// What the draft becomes when the action popup changes.
///
/// Pure, so the two modes' opposite answers are pinned by a test instead of by reading the handler:
/// in **edit** mode the chord belongs to the row and follows the new action (otherwise changing an
/// action while keeping its shortcut would be impossible); in **add** mode a chord recorded for the
/// previous action is dropped, because confirming it would bind a shortcut the user chose for
/// something else. `entry_trigger` is the edited entry's stored chord, used when nothing was seeded
/// (its action was unknown, so the panel opened without a draft).
fn draft_after_action_change(
    editing: bool,
    held: Option<(Action, String)>,
    entry_trigger: Option<String>,
    action: Action,
) -> Option<(Action, String)> {
    if !editing {
        return None;
    }
    match held {
        Some((_, trigger)) => Some((action, trigger)),
        None => entry_trigger.map(|trigger| (action, trigger)),
    }
}

/// The message line's text after an action change, or `None` for "clear it".
///
/// The sentence the recorder wrote names an action, so it goes stale the moment the popup moves. This
/// is the rule the handler applies: a surviving draft re-describes itself around the new action, and
/// no draft means no sentence. Pure so the consistency is asserted rather than eyeballed.
fn message_after_action_change(next_draft: &Option<(Action, String)>) -> Option<String> {
    next_draft.as_ref().map(|(action, trigger)| {
        tf(
            "settings.binding_draft",
            &[
                (
                    "trigger",
                    &Trigger::parse(trigger)
                        .map(|t| t.keycap())
                        .unwrap_or_else(|_| trigger.clone()),
                ),
                ("action", &crate::key_bindings::action_label(*action)),
            ],
        )
    })
}

/// The binding the open panel is editing, read fresh from the config.
fn edited_binding() -> Option<crate::key_bindings::KeyBinding> {
    let item = binding_editing()?;
    CONFIG
        .read()
        .ok()
        .and_then(|cfg| cfg.key_bindings.items.clone())
        .and_then(|items| items.get(item).cloned())
}

/// The panel "OK": write the draft, adding a new binding or rewriting the one being edited.
pub(crate) extern "C" fn handle_panel_confirm(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    callback_guard::void("handle_panel_confirm", || {
        // Re-read here, not trusted from the button's state: a config reload can turn the feature off
        // between the render and the click. Same predicate as the button's enablement.
        if !panel_gates(
            master_switch_on(),
            has_draft(),
            super::mapping::binding_recording_pending(),
        )
        .ok
        {
            return;
        }
        let Some((action, trigger)) = BINDING_DRAFT.lock().ok().and_then(|draft| draft.clone())
        else {
            return;
        };
        // Re-validate against the config as it is now, and against the action the popup is showing: the
        // draft is only worth writing while those two still agree.
        if panel_popup_action() != Some(action) {
            clear_draft_state();
            update_panel_ok();
            return;
        }
        // The whole target, not just the index: the write path compares identity as well.
        let editing = editing_target();
        match write_binding(editing, action, &trigger) {
            Ok(()) => {
                close_binding_panel();
            }
            Err(error) => set_panel_message(Some(&error)),
        }
    });
}

/// The panel "Cancel".
pub(crate) extern "C" fn handle_panel_cancel(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    callback_guard::void("handle_panel_cancel", || {
        close_binding_panel();
    });
}

/// The action at one popup row, in the order the picker builds its menu.
pub(crate) fn add_action_at(index: usize) -> Option<Action> {
    picker_actions().get(index).copied()
}

/// Every bindable action, in picker order (the registry order grouped for reading).
pub(crate) fn picker_actions() -> Vec<Action> {
    let mut actions = crate::key_bindings::ALL.to_vec();
    // Display order only: the two Finder variants read as one pair. The registry index (the bridge
    // value) is frozen and must not move.
    if let Some(small) = actions
        .iter()
        .position(|action| *action == Action::OpenFinderSmall)
    {
        let small = actions.remove(small);
        match actions
            .iter()
            .position(|action| *action == Action::OpenFinder)
        {
            Some(finder) => actions.insert(finder + 1, small),
            None => actions.push(small),
        }
    }
    actions
}

/// The popup row a binding's action sits on, or the first row when there is nothing to prefill.
///
/// Pure, and separate from the panel so the prefill rule is testable without a window: a binding
/// whose action id is unknown (one a hand-edited file left behind) opens on the first row rather
/// than at a nonsense index.
fn picker_index_for(action: Option<Action>) -> usize {
    action
        .and_then(|action| {
            picker_actions()
                .iter()
                .position(|candidate| *candidate == action)
        })
        .unwrap_or(0)
}

/// The chord row's text for a binding, or the "not recorded yet" placeholder when there is none.
///
/// The keycap form is language-neutral, so a prefilled row reads the same as the row it was opened
/// from; an unparseable stored chord falls back to its raw text so the user can still see and fix it.
fn chord_text_for(binding: Option<&crate::key_bindings::KeyBinding>) -> String {
    match binding {
        Some(binding) => Trigger::parse(&binding.trigger)
            .map(|trigger| trigger.keycap())
            .unwrap_or_else(|_| binding.trigger.clone()),
        None => t("settings.binding_panel_no_chord"),
    }
}

/// Append a binding for `action` with a freshly recorded `trigger`, or report why it was refused.
///
/// The chord is checked against the effective config before it is written, so a refused recording
/// changes nothing: the same reserved-chord rules the config loader enforces apply here, and the
/// user hears about it in the panel instead of finding a binding missing after a reload.
pub(crate) fn add_binding(action: Action, trigger: &str) -> Result<(), String> {
    write_binding(None, action, trigger)
}

/// Write a binding: `editing` is `None` to append a new one, or `Some(index)` to rewrite that entry
/// in place, keeping its switch state.
///
/// Both modes go through the same validation, which is the point: an edit can collide exactly the
/// way an add can, and the two must not disagree about what is acceptable. The one difference is
/// what a duplicate check means — an edit's own row still claims its current chord, so the entry
/// being replaced is excluded from the collision test rather than counted against itself.
fn write_binding(editing: Option<EditTarget>, action: Action, trigger: &str) -> Result<(), String> {
    let parsed = Trigger::parse(trigger).map_err(|_| {
        tf(
            "errors.key_binding_trigger_invalid",
            &[("value", trigger.trim())],
        )
    })?;
    if parsed.is_bare_key() {
        return Err(tf(
            "errors.key_binding_trigger_bare",
            &[("value", &parsed.describe())],
        ));
    }
    let old = CONFIG.read().map(|cfg| cfg.clone()).unwrap_or_default();
    let all = old.key_bindings.items.clone().unwrap_or_default();
    // The entry an edit replaces does not collide with itself: drop it before the collision test.
    // Its `enabled` state is carried over, because a switch flip is not part of what this panel
    // edits.
    let carried_enabled = match editing.as_ref() {
        // The index has to still hold the binding the panel was opened on. Checking only that the
        // index is in range is not enough: a reload can move a *different* binding into that slot, and
        // writing to it would silently destroy a binding the user never touched.
        Some(target) => match resolve_edit_target(&all, target) {
            Some(current) => current.enabled,
            // The list changed under the panel (an external reload, restore defaults, or the row
            // moved). Refuse rather than overwrite whichever binding is there now.
            None => return Err(t("errors.key_binding_edit_stale")),
        },
        None => true,
    };
    let mut items = items_without_edited(&all, editing.as_ref().map(|target| target.item));
    if crate::key_bindings::trigger_is_taken(&items, parsed) {
        return Err(tf(
            "errors.key_binding_trigger_duplicate",
            &[("value", &parsed.describe()), ("index", "")],
        ));
    }
    let reserved = crate::key_bindings::Reserved::from_effective(
        &old.keyboard.modifier,
        &old.clipboard.shortcut,
    );
    let entry = crate::key_bindings::KeyBinding {
        action: action.config_id().to_string(),
        trigger: parsed.describe(),
        enabled: carried_enabled,
    };
    let mut probe = vec![entry.clone()];
    if !crate::key_bindings::drop_reserved_conflicts(&mut probe, &reserved).is_empty() {
        return Err(crate::key_bindings::reserved_trigger_message(
            &parsed.describe(),
        ));
    }
    // Put the new entry back where the old one was; an add appends at the end. The index is known to
    // be valid here: the identity check above already refused every other case.
    match editing.as_ref() {
        Some(target) if target.item <= items.len() => items.insert(target.item, entry),
        _ => items.push(entry),
    }
    let mut updated = old.clone();
    updated.key_bindings.items = Some(items);
    // The apply pass inside `write_binding_config` re-renders the rows (their shape changed), so
    // the registry is rebuilt before the next click can reach a stale index.
    write_binding_config(&old, updated);
    Ok(())
}

/// Delete the binding at `item`, keeping every other entry (and its order) untouched.
///
/// `write_binding_config` rebuilds the rows, so nothing here has to move a cursor: every row carries
/// its own item index and the rebuild re-registers them from scratch.
fn delete_binding(item: usize) {
    let old = CONFIG.read().map(|cfg| cfg.clone()).unwrap_or_default();
    let mut updated = old.clone();
    let Some(items) = updated.key_bindings.items.as_mut() else {
        return;
    };
    if item >= items.len() {
        return;
    }
    items.remove(item);
    write_binding_config(&old, updated);
}

/// Toggle the binding at `item`.
fn toggle_binding(item: usize, enabled: bool) {
    let old = CONFIG.read().map(|cfg| cfg.clone()).unwrap_or_default();
    let mut updated = old.clone();
    let Some(items) = updated.key_bindings.items.as_mut() else {
        return;
    };
    let Some(binding) = items.get_mut(item) else {
        return;
    };
    if binding.enabled == enabled {
        return;
    }
    binding.enabled = enabled;
    write_binding_config(&old, updated);
}

/// Write an edited binding list and make it live: in-memory config, the usual apply pass (which
/// republishes the engine's table and starts or stops the tap), a debounced persist, and a rebuild
/// of the rows.
fn write_binding_config(old: &Config, updated: Config) {
    if let Ok(mut config) = CONFIG.write() {
        *config = updated.clone();
    }
    schedule_config_persist();
    apply_config_change(old, &updated, ConfigChangeSource::Settings);
    log_debug!(
        "[keybind] bindings edited: {} entr(ies)",
        updated
            .key_bindings
            .items
            .as_ref()
            .map(Vec::len)
            .unwrap_or(0)
    );
}

/// The add panel's message line: an error from a refused recording, or empty.
pub(crate) fn set_add_message(message: Option<&str>) {
    set_panel_message(message);
}

/// A recording that was capturing for this page is over without a chord to confirm (cancelled, or
/// stopped by a permission revocation on another thread, which marshals here): drop the draft and
/// put the panel back in its "no chord yet" state.
pub(crate) fn recording_ended() {
    clear_draft();
}

/// A capture that produced no trigger at all: the key has no config spelling (the top row in its
/// media mode reports an Apple private keycode), so no chord can be written for it.
///
/// Reported in the same message line a refused chord uses — the panel asks the user to record
/// something else rather than validating an invented token, which used to read as "invalid
/// shortcut" and blamed a chord the user never typed.
pub(crate) fn recording_unbindable(message: &str) {
    clear_draft_state();
    set_panel_message(Some(message));
    update_panel_ok();
}

/// The recording captured a chord: hold it as a draft until the panel's OK writes it.
pub(crate) fn recording_captured(action: Action, trigger: &str) -> Result<(), String> {
    // Validate before asking: telling the user now is kinder than after they press OK, and the
    // confirm step re-validates against the config as it is at that moment.
    if let Err(error) = validate_draft(action, trigger) {
        clear_draft_state();
        set_panel_message(Some(&error));
        update_panel_ok();
        return Err(error);
    }
    if let Ok(mut draft) = BINDING_DRAFT.lock() {
        *draft = Some((action, trigger.to_string()));
    }
    // The panel names both halves before OK writes them, so the user sees what they are agreeing to.
    show_draft(action, trigger);
    Ok(())
}

/// Whether a recorded chord is waiting for confirmation.
pub(crate) fn has_draft() -> bool {
    BINDING_DRAFT
        .lock()
        .map(|draft| draft.is_some())
        .unwrap_or(false)
}

/// Drop the draft and put the panel's controls back to their "nothing recorded yet" state.
fn clear_draft() {
    clear_draft_state();
    set_panel_message(None);
    update_panel_ok();
}

/// Write the draft, or report why it was refused (keeping it so the user can cancel or retry).
fn confirm_draft() {
    let Some((action, trigger)) = BINDING_DRAFT.lock().ok().and_then(|draft| draft.clone()) else {
        return;
    };
    match add_binding(action, &trigger) {
        Ok(()) => clear_draft(),
        Err(error) => set_add_message(Some(&error)),
    }
}

/// Forget the draft without touching the UI (the UI half is `clear_draft`).
fn clear_draft_state() {
    if let Ok(mut draft) = BINDING_DRAFT.lock() {
        *draft = None;
    }
}

/// Show the draft in the panel: the chord row shows the recorded chord and the message line names
/// both halves, so the user can see what OK would write.
fn show_draft(action: Action, trigger: &str) {
    // One sentence builder for both paths (a fresh recording and an action change), so the two can
    // never phrase the same statement differently.
    if let Some(sentence) = message_after_action_change(&Some((action, trigger.to_string()))) {
        set_panel_message(Some(&sentence));
    }
    update_panel_ok();
}

/// The validation the record and confirm steps share: parseable, not a bare key, not already
/// taken, and not reserved. Pure over the effective config's binding list and reserved set, so the
/// add flow refuses exactly what the config loader would drop.
fn draft_rejection(
    action: Action,
    trigger: &str,
    items: &[crate::key_bindings::KeyBinding],
    reserved: &crate::key_bindings::Reserved,
) -> Option<String> {
    let parsed = match Trigger::parse(trigger) {
        Ok(parsed) => parsed,
        Err(_) => {
            return Some(tf(
                "errors.key_binding_trigger_invalid",
                &[("value", trigger.trim())],
            ));
        }
    };
    if parsed.is_bare_key() {
        return Some(tf(
            "errors.key_binding_trigger_bare",
            &[("value", &parsed.describe())],
        ));
    }
    if crate::key_bindings::trigger_is_taken(items, parsed) {
        return Some(tf(
            "errors.key_binding_trigger_duplicate",
            &[("value", &parsed.describe()), ("index", "")],
        ));
    }
    // The reserved check runs on the entry that would be written, so a chord the switcher or the
    // clipboard shortcut owns is refused here rather than silently dropped at the next load.
    let mut probe = vec![crate::key_bindings::KeyBinding {
        action: action.config_id().to_string(),
        trigger: parsed.describe(),
        enabled: true,
    }];
    if !crate::key_bindings::drop_reserved_conflicts(&mut probe, reserved).is_empty() {
        return Some(crate::key_bindings::reserved_trigger_message(
            &parsed.describe(),
        ));
    }
    None
}

/// The list a chord check should run against, with the row being edited taken out.
///
/// The entry an edit replaces must not collide with itself: recording a binding's own current chord
/// is the commonest thing an edit does (the user opened the panel to change the action, not the
/// chord), and counting that row against itself would refuse it. An add has nothing to exclude.
fn items_without_edited(
    items: &[crate::key_bindings::KeyBinding],
    editing: Option<usize>,
) -> Vec<crate::key_bindings::KeyBinding> {
    match editing {
        Some(index) if index < items.len() => {
            let mut rest = items.to_vec();
            rest.remove(index);
            rest
        }
        _ => items.to_vec(),
    }
}

/// The entry an edit target still refers to, or `None` when the index no longer holds it.
///
/// Pure, so the rule the write path depends on is testable on its own: an index that merely resolves
/// is **not** enough, because a reload can move a different binding into that slot. The identity --
/// the action and chord the row was drawn from -- is what the user chose, so it is what is compared.
fn resolve_edit_target<'a>(
    items: &'a [crate::key_bindings::KeyBinding],
    target: &EditTarget,
) -> Option<&'a crate::key_bindings::KeyBinding> {
    items
        .get(target.item)
        .filter(|current| current.action == target.action && current.trigger == target.trigger)
}

/// The same check against the live config.
fn validate_draft(action: Action, trigger: &str) -> Result<(), String> {
    let old = CONFIG.read().map(|cfg| cfg.clone()).unwrap_or_default();
    let all = old.key_bindings.items.clone().unwrap_or_default();
    let items = items_without_edited(&all, binding_editing());
    let reserved = crate::key_bindings::Reserved::from_effective(
        &old.keyboard.modifier,
        &old.clipboard.shortcut,
    );
    match draft_rejection(action, trigger, &items, &reserved) {
        Some(reason) => Err(reason),
        None => Ok(()),
    }
}

/// Row switch: turn one binding on or off.
pub(crate) extern "C" fn handle_binding_toggle(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    callback_guard::void("handle_binding_toggle", || {
        let sender = sender as *mut AnyObject;
        let Some(item) = control_item(sender) else {
            return;
        };
        let state: isize = unsafe { msg_send![sender, state] };
        toggle_binding(item, state == 1);
    });
}

/// The footer's "Add binding" button: open the panel in its add mode.
pub(crate) extern "C" fn handle_binding_add(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    callback_guard::void("handle_binding_add", || {
        // The control disables itself with the master switch, but a keyboard or programmatic invocation
        // can still arrive, so the rule is checked here too.
        if !master_switch_on() {
            return;
        }
        open_binding_panel();
    });
}

/// A row's "Edit" button: open the panel prefilled with that binding.
pub(crate) extern "C" fn handle_binding_edit(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    callback_guard::void("handle_binding_edit", || {
        if !master_switch_on() {
            return;
        }
        let Some(item) = control_item(sender as *mut AnyObject) else {
            return;
        };
        open_binding_edit_panel(item);
    });
}

/// A row's "Delete" button: remove that binding.
///
/// The index is re-read against the config at click time rather than trusted from the row that drew
/// the button. A rebuild replaces every row and re-registers every index, so a stale sender cannot
/// reach here — but the length check is the second lock on the same door, and it costs nothing.
pub(crate) extern "C" fn handle_binding_delete(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    callback_guard::void("handle_binding_delete", || {
        if !master_switch_on() {
            return;
        }
        let Some(item) = control_item(sender as *mut AnyObject) else {
            return;
        };
        let len = CONFIG
            .read()
            .map(|cfg| cfg.key_bindings.items.as_ref().map_or(0, Vec::len))
            .unwrap_or(0);
        if item >= len {
            return;
        }
        log_debug!("[keybind] deleting row {}", item);
        delete_binding(item);
    });
}

/// "Add": write the recorded draft.
pub(crate) extern "C" fn handle_binding_confirm(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    callback_guard::void("handle_binding_confirm", || {
        confirm_draft();
    });
}

/// "Cancel": drop the draft and go back to the record button.
pub(crate) extern "C" fn handle_binding_cancel(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    callback_guard::void("handle_binding_cancel", || {
        clear_draft();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the add flow refuses, and why: the same rules the config loader enforces, so a chord
    /// refused here is one a reload would have dropped anyway.
    #[test]
    fn the_add_flow_refuses_bare_duplicate_and_reserved_chords() {
        use crate::key_bindings::KeyBinding;
        let existing = vec![KeyBinding {
            action: "quick.open_finder".into(),
            trigger: "option+e".into(),
            enabled: true,
        }];
        let reserved = crate::key_bindings::Reserved::from_effective("command", "option+v");
        let check = |trigger: &str| draft_rejection(Action::Center, trigger, &existing, &reserved);

        assert!(check("cmd+ctrl+c").is_none());
        assert!(check("option+e").is_some(), "already bound");
        assert!(check("e").is_some(), "a bare key is refused");
        assert!(check("shift+e").is_some(), "shift alone is still typing");
        assert!(check("not-a-trigger").is_some());
        assert!(check("cmd+tab").is_some(), "the switcher chord");
        assert!(check("option+v").is_some(), "the clipboard chord");
    }

    /// The defect this pins: editing `B` in `[A, B, C]`, having `B` removed externally, reloading to
    /// `[A, C]`, and confirming. The index still resolves -- but to `C`, so an index-only check would
    /// overwrite `C` and silently destroy a binding the user never chose. The identity is what makes
    /// that write unsafe to perform, and this is the assertion that fails if the check is weakened.
    #[test]
    fn an_edit_target_that_moved_to_another_binding_is_refused() {
        let binding =
            |action: &str, trigger: &str, enabled: bool| crate::key_bindings::KeyBinding {
                action: action.into(),
                trigger: trigger.into(),
                enabled,
            };
        let target = EditTarget {
            item: 1,
            action: "window.snap_left".into(),
            trigger: "option+left".into(),
        };

        // Still the same binding at that index: resolved, and its switch state is the one carried over.
        let intact = vec![
            binding("window.snap_up", "option+up", true),
            binding("window.snap_left", "option+left", false),
            binding("window.snap_right", "option+right", true),
        ];
        let resolved = resolve_edit_target(&intact, &target).expect("the same entry resolves");
        assert!(
            !resolved.enabled,
            "the carried switch state comes from that entry"
        );

        // The reviewer's case: B was removed, so C slid into index 1. The index resolves, the
        // identity does not -- and this is the whole point, because an index-only check accepts it.
        let shifted = vec![
            binding("window.snap_up", "option+up", true),
            binding("window.snap_right", "option+right", true),
        ];
        assert!(
            shifted.get(target.item).is_some(),
            "the index still resolves"
        );
        assert!(
            resolve_edit_target(&shifted, &target).is_none(),
            "a different binding at the same index must not be written"
        );

        // A same-index replacement (the action changed, or the chord did) is refused too: the user
        // never saw that entry.
        let replaced = vec![
            binding("window.snap_up", "option+up", true),
            binding("window.center", "cmd+ctrl+c", true),
        ];
        assert!(resolve_edit_target(&replaced, &target).is_none());

        // The row moved but is otherwise untouched: still not the row that was opened.
        let reordered = vec![
            binding("window.snap_left", "option+left", false),
            binding("window.snap_up", "option+up", true),
        ];
        assert!(resolve_edit_target(&reordered, &target).is_none());

        // A list that shrank past the index cannot resolve either -- the original range check, kept.
        let empty: Vec<crate::key_bindings::KeyBinding> = Vec::new();
        assert!(resolve_edit_target(&empty, &target).is_none());
    }

    /// An edit must not count the row it replaces against itself: recording a binding's own current
    /// chord is the commonest thing an edit does, and a list that still held that row would refuse
    /// it as a duplicate. An add excludes nothing, because it replaces nothing.
    #[test]
    fn an_edit_does_not_collide_with_the_row_it_replaces() {
        use crate::key_bindings::KeyBinding;
        let items = vec![
            KeyBinding {
                action: "quick.open_finder".into(),
                trigger: "option+e".into(),
                enabled: true,
            },
            KeyBinding {
                action: "window.snap_left".into(),
                trigger: "option+left".into(),
                enabled: false,
            },
        ];
        let reserved = crate::key_bindings::Reserved::from_effective("command", "option+v");

        // Editing row 0 and keeping its own chord: the row is excluded, so this is accepted.
        let editing = items_without_edited(&items, Some(0));
        assert_eq!(editing.len(), 1);
        assert!(editing
            .iter()
            .all(|item| item.action != "quick.open_finder"));
        assert!(draft_rejection(Action::OpenFinder, "option+e", &editing, &reserved).is_none());

        // The same chord is still refused for a row that is *not* the one being edited.
        assert!(draft_rejection(Action::OpenFinder, "option+left", &editing, &reserved).is_some());

        // An add excludes nothing, so the chord really is taken.
        assert!(draft_rejection(Action::OpenFinder, "option+e", &items, &reserved).is_some());

        // An out-of-range index excludes nothing rather than panicking: the write path refuses that
        // case separately, and this helper must stay total.
        assert_eq!(items_without_edited(&items, Some(9)).len(), items.len());
        assert_eq!(items_without_edited(&items, None).len(), items.len());
    }

    /// Opening the panel on an existing binding prefills both fields from that binding, and an
    /// unknown action or an unparseable chord degrades to a defined row instead of a nonsense index
    /// or a blank field. Pure, so the rule is pinned without a window.
    #[test]
    fn the_edit_panel_prefills_from_the_binding_it_opens() {
        // The popup opens on the row that holds the binding's action, not always on row 0.
        let finder = picker_actions()
            .iter()
            .position(|action| *action == Action::OpenFinder)
            .expect("OpenFinder is in the picker");
        assert_eq!(picker_index_for(Some(Action::OpenFinder)), finder);
        assert!(finder > 0, "the picker must not put OpenFinder first");
        // Nothing to prefill, or an action the registry no longer knows: the first row.
        assert_eq!(picker_index_for(None), 0);

        // The chord row shows the binding's own chord in keycap form.
        let binding = |trigger: &str| crate::key_bindings::KeyBinding {
            action: "quick.open_finder".into(),
            trigger: trigger.into(),
            enabled: true,
        };
        assert_eq!(chord_text_for(Some(&binding("option+e"))), "⌥E");
        // A chord a hand-edited file left unparseable stays visible rather than blanking the row.
        assert_eq!(chord_text_for(Some(&binding("not-a-chord"))), "not-a-chord");
        // Add mode has nothing to show, so it says so.
        assert_eq!(chord_text_for(None), t("settings.binding_panel_no_chord"));
    }

    /// The two Finder variants are one pair in the picker even though their frozen registry indices
    /// are far apart. The display order must stay a permutation of the registry and map back to the
    /// same action through the popup index.
    #[test]
    fn the_picker_keeps_the_two_finder_variants_adjacent() {
        let actions = picker_actions();
        let mut by_index = actions.clone();
        by_index.sort_by_key(|action| action.index());
        assert_eq!(by_index, crate::key_bindings::ALL.to_vec());
        let finder = actions
            .iter()
            .position(|action| *action == Action::OpenFinder)
            .expect("OpenFinder is in the picker");
        assert_eq!(actions[finder + 1], Action::OpenFinderSmall);
        for action in [Action::OpenFinder, Action::OpenFinderSmall] {
            let index = picker_index_for(Some(action));
            assert_eq!(add_action_at(index), Some(action), "{action:?}");
        }
    }

    /// The recorded sentence names an action, so it must be rebuilt when the action changes; leaving
    /// it would have the panel describe one action while confirm writes another. Pinned on the pure
    /// rule, so the message the user reads is asserted rather than eyeballed.
    #[test]
    fn the_confirmation_sentence_follows_the_action_it_will_write() {
        // After recording option+E for OpenFinder, the panel says so.
        let recorded = Some((Action::OpenFinder, "option+e".into()));
        let before = message_after_action_change(&recorded).expect("a sentence");
        assert!(
            before.contains(&crate::key_bindings::action_label(Action::OpenFinder)),
            "the sentence must name the recorded action: {before}"
        );

        // Switching the popup to OpenSettings keeps the chord but must re-name the action.
        let switched = draft_after_action_change(true, recorded, None, Action::OpenSettings);
        let after = message_after_action_change(&switched).expect("a sentence");
        assert!(
            after.contains(&crate::key_bindings::action_label(Action::OpenSettings)),
            "the sentence must name the new action: {after}"
        );
        assert!(
            !after.contains(&crate::key_bindings::action_label(Action::OpenFinder)),
            "the stale action name must not survive: {after}"
        );

        // No draft, no sentence: with nothing to write the message line goes back to empty.
        assert_eq!(message_after_action_change(&None), None);
    }

    /// Changing the action keeps the chord in edit mode and discards it in add mode. The two modes
    /// give opposite answers on purpose, and each answer is load-bearing: keeping the chord is what
    /// lets a user change an action without re-recording, and discarding it is what stops an add from
    /// binding a shortcut that was recorded for a different action.
    #[test]
    fn the_chord_follows_the_action_when_editing_and_is_dropped_when_adding() {
        // Edit mode with a draft (the normal case: the panel was seeded from the binding).
        assert_eq!(
            draft_after_action_change(
                true,
                Some((Action::OpenFinder, "option+e".into())),
                None,
                Action::OpenSettings,
            ),
            Some((Action::OpenSettings, "option+e".into())),
            "an edit must keep its chord when the action changes"
        );
        // Edit mode with nothing seeded (the stored action was unknown): the entry's own chord is used.
        assert_eq!(
            draft_after_action_change(true, None, Some("option+d".into()), Action::ShowDesktop),
            Some((Action::ShowDesktop, "option+d".into()))
        );
        // Edit mode with neither: nothing to carry, so OK stays disabled rather than writing a guess.
        assert_eq!(
            draft_after_action_change(true, None, None, Action::Center),
            None
        );

        // Add mode always drops the chord, whether or not one was recorded.
        assert_eq!(
            draft_after_action_change(
                false,
                Some((Action::OpenFinder, "option+e".into())),
                Some("option+d".into()),
                Action::OpenSettings,
            ),
            None,
            "an add must not inherit a chord recorded for another action"
        );
        assert_eq!(
            draft_after_action_change(false, None, Some("option+d".into()), Action::Center),
            None
        );
    }

    /// The draft is state the cancellation paths can see and clear without a window: that is what
    /// lets a window close, a permission revocation or a page switch clear a chord the user
    /// recorded but never confirmed.
    #[test]
    fn a_draft_is_visible_and_clearable_without_ui() {
        clear_draft_state();
        assert!(!has_draft());
        if let Ok(mut draft) = BINDING_DRAFT.lock() {
            *draft = Some((Action::OpenFinder, "option+e".into()));
        }
        assert!(has_draft());
        clear_draft_state();
        assert!(!has_draft());
    }

    /// Every editor on the page is gated by the master switch, and the panel is one of them: a
    /// switch that freezes the rows and the footer but left a live OK button would let a binding be
    /// written with the feature off. This pins the rule the two paths share — OK needs the switch
    /// *and* a draft and a stopped recorder; the record button needs the switch and a stopped
    /// recorder; the action popup needs only the switch.
    #[test]
    fn the_master_switch_gates_every_panel_control() {
        // (master_on, has_draft, recording) -> (record, ok, popup)
        // The production predicate decides this, not a copy of it: deleting its master-switch check
        // fails these assertions.
        let gate = |master: bool, draft: bool, recording: bool| {
            let gates = panel_gates(master, draft, recording);
            (gates.record, gates.ok, gates.popup)
        };
        // Feature off: nothing in the panel may act, whatever else is true.
        assert_eq!(gate(false, true, false), (false, false, false));
        assert_eq!(gate(false, true, true), (false, false, false));
        // Feature on, no chord yet: the popup and the record button work, OK does not.
        assert_eq!(gate(true, false, false), (true, false, true));
        // Feature on, recorder running: neither the record button nor OK may run again.
        assert_eq!(gate(true, false, true), (false, false, true));
        assert_eq!(gate(true, true, true), (false, false, true));
        // Feature on, a chord is held and the recorder stopped: everything is usable.
        assert_eq!(gate(true, true, false), (true, true, true));
    }

    /// The card is exactly its rows, one footer band and the bottom inset. The card's height comes
    /// from production code, so the assertion is against that function rather than a copy of its
    /// arithmetic.
    #[test]
    fn the_card_reserves_one_footer_band() {
        let rows_h = BINDING_ROW_H;
        // The card is taller than its rows by exactly the footer band plus the bottom inset: the
        // delta is what the canvas is handed, so it is asserted as a delta rather than as a
        // restatement of the function.
        assert_eq!(
            card_height(rows_h) - rows_h,
            BINDING_FOOTER_H + BINDING_PAD_Y,
            "the card must reserve exactly one footer band and the bottom inset beyond its rows"
        );
    }

    /// The vertical placement the footer uses for its control.
    ///
    /// This is the production rule, not a restatement of it: the renderer positions the control with
    /// this offset from the band's top, so a change here moves the control and nothing else. The
    /// defect it fixes was an edge-anchored coordinate that left 5pt above the buttons and 15pt
    /// below; deriving the offset as half the slack makes the two sides equal by construction.
    #[test]
    fn the_footer_offset_centres_the_control_in_its_band() {
        assert_eq!(
            footer_control_offset(),
            BINDING_FOOTER_PAD,
            "the control's offset must be the band's padding"
        );
        // Falsify the old rule explicitly: the offset is NOT measured from the band's bottom edge,
        // which is what once put the control too high.
        let from_bottom = BINDING_FOOTER_H - footer_control_offset() - BINDING_BTN_H;
        assert_eq!(
            from_bottom,
            footer_control_offset(),
            "the space above and below the control must be equal"
        );
    }

    /// The row's trailing buttons fit inside the row without colliding with each other or with the
    /// switch: the layout places Edit, then Delete, then the switch at fixed gaps, so a change to any
    /// of the three widths has to keep the sum inside the card.
    #[test]
    fn the_row_actions_fit_beside_the_switch() {
        // Two buttons and the two gaps between (Edit | Delete | switch) plus the switch and the
        // card's trailing padding must fit the narrowest card the page can be laid out at.
        let needed = BINDING_BTN_W * 2.0 + BINDING_BTN_GAP + BINDING_COL_GAP + BINDING_SWITCH_W;
        assert!(
            needed < 360.0,
            "the row's trailing controls need {needed}pt, which does not fit a narrow card"
        );
    }

    /// The row reads left to right as action name -> keycaps -> Edit/Delete -> switch, asserted on
    /// the **production** geometry (`row_layout` / `keycap_run_origin`, the same functions the
    /// renderer positions with), not on a restatement of the arithmetic. Restating it is how the
    /// original defect -- keycaps drawn before the action name -- shipped with a test that agreed
    /// with itself.
    #[test]
    fn the_row_reads_name_then_keycaps_then_actions() {
        // The real page's content width (window 820 - sidebar 220 - page insets 64, scroller 0).
        let cols = row_layout(536.0);
        let chip_w = vec![BINDING_CAP_SIZE; 2];
        let (run_x, _) = keycap_run_origin(cols, &chip_w);

        assert!(cols.label_x < run_x, "the name must have positive width");
        assert!(
            run_x + chip_w.iter().sum::<f64>() + BINDING_CAP_GAP <= cols.edit_x,
            "the chips (from {run_x}pt) must finish before the Edit button ({})",
            cols.edit_x
        );
        assert!(
            cols.edit_x < cols.delete_x,
            "Edit ({}) must sit left of Delete ({})",
            cols.edit_x,
            cols.delete_x
        );
        assert!(
            cols.delete_x + BINDING_BTN_W <= cols.switch_x,
            "Delete must finish before the switch ({})",
            cols.switch_x
        );
        assert!(
            (cols.switch_x + BINDING_SWITCH_W - 536.0).abs() <= BINDING_PAD_X + 0.5,
            "the switch must end at the card's trailing padding"
        );

        // Counter-example: the order is decidable, and this is the assertion that would have caught
        // the defect. `row_layout` puts the name first; a build that swapped the two columns (name at
        // the keycap offset, chips at the row's leading edge) produces a layout where the name no
        // longer precedes the run, which the first check rejects. Reproduce that swapped layout here
        // and assert the same check fails on it.
        let swapped = RowLayout {
            label_x: cols.keycap_x,
            ..cols
        };
        let swapped_run_x = cols.label_x;
        // The order check, stated once and applied to both layouts: the name must start strictly left
        // of the chip run. The production layout satisfies it; the swapped one inverts it, which is
        // what makes this test able to catch the swapped build at all.
        let name_precedes_chips = |name_x: f64, run_x: f64| name_x < run_x;
        assert!(
            name_precedes_chips(cols.label_x, run_x),
            "the production layout must put the name before the chips"
        );
        assert!(
            !name_precedes_chips(swapped.label_x, swapped_run_x),
            "a build that drew the chips first must fail the name-before-chips check"
        );
    }

    /// Every legal chord fits: a long-named key becomes one wide chip rather than one chip per
    /// character, and the run as a whole stays left of the Edit button even at the narrowest content
    /// width the page is laid out at. This is the assertion for the defect where `option+pagedown`
    /// drew one chip per character and crossed the buttons -- `--smoke-settings-layout` reported it as an
    /// overlap, and this test fails the same way for a chord that no longer fits.
    #[test]
    fn every_legal_chord_fits_before_the_edit_button() {
        // The narrowest width the page reaches in practice, so the check is not flattered by a wide
        // window: 820 - 220 sidebar - 64 insets = 536, and a legacy scroller can take ~15 more.
        for width in [536.0, 500.0, 460.0] {
            let cols = row_layout(width);
            for desc in [
                "option+e",
                "option+space",
                "option+pagedown",
                "cmd+shift+alt+ctrl+e",
                "cmd+shift+alt+ctrl+pagedown",
                "cmd+shift+alt+ctrl+escape",
                "double-control",
            ] {
                let trigger = Trigger::parse(desc).expect("legal chord");
                let chips = trigger.keycap_chips();
                let widths: Vec<f64> = chips
                    .iter()
                    .map(|label| {
                        let text_w = unsafe { super::measure_control_text_width(label) };
                        (text_w.ceil() + crate::theme::LEGEND_CHIP_TEXT_PAD_X * 2.0)
                            .clamp(BINDING_CAP_SIZE, BINDING_CAP_SIZE * 8.0)
                    })
                    .collect();
                let (run_x, label_w) = keycap_run_origin(cols, &widths);
                let run_right = run_x
                    + widths.iter().sum::<f64>()
                    + BINDING_CAP_GAP * (widths.len() - 1) as f64;
                assert!(
                    run_right <= cols.edit_x + 0.5,
                    "{desc} at width {width}: chips reach {run_right}pt, Edit starts at {}pt",
                    cols.edit_x
                );
                assert!(
                    run_x >= cols.label_x,
                    "{desc} at width {width}: run left the row"
                );
                assert!(
                    label_w > 0.0,
                    "{desc} at width {width}: the name must still have room"
                );
            }
        }
    }

    /// The table's inner padding is the shared card padding. If a layout token moves, this fails
    /// instead of letting the bindings list drift out of line with every other card.
    #[test]
    fn padding_matches_the_shared_layout() {
        let layout = SettingsLayout::new(512.0);
        assert_eq!(BINDING_PAD_X, layout.label_x);
        assert_eq!(BINDING_PAD_Y, layout.card_bottom_inset);
    }

    /// A config edit that changes the list's *shape* must rebuild the rows: each row carries the
    /// item index its Edit and Delete buttons act on, so a reorder, an insertion or a removal makes
    /// every stored index stale. A switch-only edit keeps the rows, which is what lets the switch
    /// that is dispatching its own action survive the write it caused.
    #[test]
    fn only_a_shape_change_rebuilds_the_rows() {
        // The shape is the rendered identity: action + trigger per row, in order.
        let shape = |rows: &[(&str, &str, bool)]| {
            rows.iter()
                .map(|(action, trigger, _enabled)| (action.to_string(), trigger.to_string()))
                .collect::<Vec<_>>()
        };
        let base = shape(&[("quick.open_finder", "option+e", true)]);
        // A switch flip: same shape, so the selection survives.
        let flipped = shape(&[("quick.open_finder", "option+e", false)]);
        assert!(!shape_changed(&base, &flipped));
        // A reorder, an insertion and a removal all change the shape.
        let reordered = shape(&[
            ("window.snap_left", "option+left", true),
            ("quick.open_finder", "option+e", true),
        ]);
        assert!(shape_changed(&base, &reordered));
        let mut removed = base.clone();
        removed.clear();
        assert!(shape_changed(&base, &removed));
        // A re-chorded row is a shape change too: the row's identity moved with its trigger.
        let rekeyed = shape(&[("quick.open_finder", "option+g", true)]);
        assert!(shape_changed(&base, &rekeyed));
    }

    /// Flipping a row's switch must not change the list's shape: a rebuild there would destroy the
    /// control that is dispatching its own action. Adding, deleting, or changing a chord does.
    #[test]
    fn the_list_shape_ignores_the_per_row_switch() {
        let binding =
            |action: &str, trigger: &str, enabled: bool| crate::key_bindings::KeyBinding {
                action: action.to_string(),
                trigger: trigger.to_string(),
                enabled,
            };
        let base = vec![
            binding("quick.open_finder", "option+e", true),
            binding("window.snap_left", "option+left", true),
        ];
        let flipped = vec![
            binding("quick.open_finder", "option+e", false),
            binding("window.snap_left", "option+left", true),
        ];
        assert_eq!(binding_list_shape(&base), binding_list_shape(&flipped));
        let mut removed = base.clone();
        removed.remove(0);
        assert_ne!(binding_list_shape(&base), binding_list_shape(&removed));
        let mut rekeyed = base.clone();
        rekeyed[0].trigger = "option+g".into();
        assert_ne!(binding_list_shape(&base), binding_list_shape(&rekeyed));
        let mut added = base.clone();
        added.push(binding("window.center", "cmd+ctrl+c", true));
        assert_ne!(binding_list_shape(&base), binding_list_shape(&added));
    }

    /// One row of height is reserved even when a group has no bindings: the empty-state hint lives
    /// in that band, and a zero-height card would clip it away.
    #[test]
    fn an_empty_group_still_has_one_row_of_height() {
        let rows_h = (BINDING_PAD_Y - BINDING_PAD_Y).max(BINDING_ROW_H);
        assert_eq!(rows_h, BINDING_ROW_H);
        assert_eq!(
            rows_h + BINDING_PAD_Y * 2.0,
            BINDING_ROW_H + BINDING_PAD_Y * 2.0
        );
    }
}
