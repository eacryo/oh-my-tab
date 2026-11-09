//! One settings page as data: its rows, their values and their identities.
//!
//! A page module is pure: it reads \`PageInput\`, writes \`RowSpec\`s through \`RowWriter\` and
//! returns a \`PageView\`. It creates no view, reads no global and records no search index --
//! the shared renderer does all of that from the description.
//!
//! The **excluded-applications list** is deliberately NOT part of the description. It is a
//! variable list whose remove buttons have no \`ControlField\`: the handler finds the entry by
//! comparing the SENDER POINTER against \`ui.clipboard_excluded_remove\`, and its add popup's
//! candidates are captured once per build from the running apps. Expressing those rows as
//! \`RowSpec\`s would mean a new action identity per entry (the model carries only static action
//! names), a control kind for a popup whose choice is an action, and a renderer that owns the
//! candidate list -- a new feature of the shared machinery, not a migration. So the page builds
//! that card itself, and every static row around it comes from this description.
//!
//! Two more rows stay page-built for the same reason: the shortcut recorder (a button whose title
//! is the recorded chord, with a capture state machine and an inline error label underneath) and the
//! numeric "maximum entries" field (the model's controls are switches, popups, sliders, values and
//! buttons; a numeric text field is not one of them).

use super::*;

/// The page-drawn row keys, matched by `ClipboardHooks::extra_row`.
pub(super) const CLIPBOARD_SHORTCUT_ROW: &str = "shortcut_recorder";
pub(super) const CLIPBOARD_MAX_ENTRIES_ROW: &str = "max_entries";

/// The two copies the recorder row's caption can show: the instruction while idle, and the state
/// while a capture waits for its chord. Both are measured when the row's height is decided.
const CAPTURE_CAPTION_KEYS: [&str; 2] = [
    "settings.desc_clipboard_shortcut",
    "settings.desc_clipboard_shortcut_recording",
];

/// The recorder row's height: a captioned row's, MEASURED over both caption copies at the column a
/// captioned row's copy wraps in.
///
/// A fixed height clipped the caption: the instruction does not fit one line of a 56pt row in every
/// language, and the state that is not on screen when the page is built still has to fit. The row
/// follows the tallest copy, the same way the Mouse page's device row follows the longest device
/// caption.
fn shortcut_row_height(_input: &PageInput, content_w: f64) -> f64 {
    // Measuring a caption reads AppKit text metrics; the page build runs on the main thread.
    unsafe { shortcut_row_height_inner(content_w) }
}

unsafe fn shortcut_row_height_inner(content_w: f64) -> f64 {
    let caption_w =
        crate::settings::page_view::layout::caption_column_width(SettingsLayout::new(content_w));
    // The label's CELL decides where the copy wraps, so the row is sized from the cell: an
    // attributed-string probe alone promised one line for a copy that wraps to two and clipped it.
    let caption_h = CAPTURE_CAPTION_KEYS
        .iter()
        .map(|key| crate::settings::widgets::caption_cell_height(&t(key), caption_w))
        .fold(0.0, f64::max);
    crate::settings::widgets::captioned_row_height_for(caption_h)
}

/// The numeric entry-limit row's height: a described (single-line) row, as the old builder had it.
fn clipboard_max_entries_h() -> f64 {
    SettingsLayout::SINGLE_LINE_ROW_H
}

/// The Clipboard page's static rows: its rows, their values and their identities.
///
/// The page's cards and rows are declared here; \`clipboard_dynamic_rows\` describes which of them
/// the page builds itself (see the module doc), and \`build_clipboard_page\` sequences the two.
pub(super) fn clipboard_page_view(input: &PageInput) -> PageView {
    let cfg = &input.config;
    let clipboard = &cfg.clipboard;
    let mut w = RowWriter::new(SettingsPageId::Clipboard, t("settings.sidebar_clipboard"));

    // ---- The feature's own card: the master switch and its standing pause ----
    w.in_first_card("settings.header_clipboard");
    w.push(
        switch(
            "settings.row_clipboard_enabled",
            ControlField::ClipboardEnabled,
            clipboard.enabled,
        )
        .caption("settings.desc_clipboard_enabled")
        .into_spec(),
    );
    w.push(
        switch(
            "settings.row_clipboard_paused",
            ControlField::ClipboardPaused,
            clipboard.paused,
        )
        .caption("settings.desc_clipboard_paused")
        .into_spec(),
    );

    // ---- History: the shortcut recorder stays page-built (a button with a capture state), then
    // where the picker opens ----
    w.card("settings.header_clipboard_options");
    // The recorder is a button with a capture state machine: the page draws it itself, and its row
    // is measured because its caption is the instruction and wraps like any captioned row's copy.
    w.extra_row_measured(CLIPBOARD_SHORTCUT_ROW, shortcut_row_height);
    let pin_labels = vec![
        t("settings.pin_follow_entry"),
        t("settings.pin_keep_position"),
    ];
    w.push(
        popup(
            "settings.row_clipboard_pin_follow",
            &pin_labels,
            ControlField::ClipboardPinFollow,
            usize::from(!clipboard.pin_follow_selection),
        )
        .into_spec(),
    );
    let placements = vec![
        t("settings.picker_placement_cursor"),
        t("settings.picker_placement_center"),
    ];
    w.push(
        popup(
            "settings.row_clipboard_picker_placement",
            &placements,
            ControlField::ClipboardPickerPlacement,
            crate::settings::dispatch::picker_placement_index(&clipboard.picker_placement) as usize,
        )
        .into_spec(),
    );

    // ---- Retention and behaviour ----
    w.push(
        switch(
            "settings.row_clipboard_clear_on_quit",
            ControlField::ClipboardClearOnQuit,
            clipboard.clear_on_quit,
        )
        .into_spec(),
    );
    w.push(
        switch(
            "settings.row_clipboard_separate_clear_all",
            ControlField::ClipboardSeparateClearAll,
            clipboard.separate_clear_all,
        )
        .into_spec(),
    );
    w.push(
        switch(
            "settings.row_clipboard_show_source_app",
            ControlField::ClipboardShowSourceApp,
            clipboard.show_source_app,
        )
        .into_spec(),
    );
    w.push(
        switch(
            "settings.row_clipboard_move_used_to_top",
            ControlField::ClipboardMoveUsedToTop,
            clipboard.move_used_to_top,
        )
        .into_spec(),
    );
    w.push(
        switch(
            "settings.row_clipboard_delete_after_paste",
            ControlField::ClipboardDeleteAfterPaste,
            clipboard.delete_after_paste,
        )
        .caption("settings.desc_clipboard_delete_after_paste")
        .into_spec(),
    );
    w.push(
        switch(
            "settings.row_clipboard_clear_system_pasteboard_after_paste",
            ControlField::ClipboardClearSystemPasteboardAfterPaste,
            clipboard.clear_system_pasteboard_after_paste,
        )
        .group(RowGroup::ClipboardDeleteChild)
        .into_spec(),
    );
    // The numeric entry limit is a text field, which the model's control kinds do not cover.
    // It sits between the delete-after-paste switches and the retention sliders, exactly where
    // the inline builder drew it.
    w.extra_row(CLIPBOARD_MAX_ENTRIES_ROW, clipboard_max_entries_h());
    let auto_expire = (clipboard.auto_expire_days as i64)
        .clamp(CLIPBOARD_AUTO_EXPIRE_MIN, CLIPBOARD_AUTO_EXPIRE_MAX);
    w.push(
        slider(
            "settings.row_clipboard_auto_expire_days",
            ControlField::ClipboardAutoExpireDays,
            (CLIPBOARD_AUTO_EXPIRE_MIN, CLIPBOARD_AUTO_EXPIRE_MAX, 1),
            auto_expire,
            Some(CLIPBOARD_AUTO_EXPIRE_DEFAULT as f64),
            auto_expire.to_string(),
        )
        .into_spec(),
    );
    let image_rows = (clipboard.image_rows as i64).clamp(1, 3);
    w.push(
        slider(
            "settings.row_clipboard_image_rows",
            ControlField::ClipboardImageRows,
            (1, 3, 1),
            image_rows,
            Some(1.0),
            image_rows.to_string(),
        )
        .into_spec(),
    );

    // ---- Excluded applications ----
    // The whole card is page-drawn (`after_page`): the list, the add popup and the restore
    // action, in that order. A static restore ROW here would be drawn before the list, and the
    // hook draws the same action again at the card's end -- the row appeared twice, the first
    // copy directly under the heading.
    w.card("settings.header_clipboard_excluded");
    w.finish()
}

/// The page hooks that draw Clipboard's own rows: the shortcut recorder (a button with a capture
/// state machine and an inline error label) and the numeric entry limit (a text field).
///
/// Both sit inside the `header_clipboard_options` card, between described rows, exactly where the
/// inline builder put them. Their views are placed through the canvas like any described row's, so
/// a re-flow moves them with their row.
pub(crate) struct ClipboardHooks {
    /// Whether a capture is running: the row's caption says so and its button asks for a chord.
    pub(crate) recording: bool,
    /// The recorded chord, as the button shows it before any capture starts.
    pub(crate) shortcut_title: String,
}

impl PageHooks for ClipboardHooks {
    unsafe fn extra_row(
        &mut self,
        canvas: &mut PageCanvas,
        rendered: &mut RenderedPage,
        ui: &mut SettingsUi,
        spec: &PageRowSpec,
        origin_y: f64,
        height: f64,
        target: *mut AnyObject,
    ) -> Result<(), RegisterError> {
        match spec.key {
            CLIPBOARD_SHORTCUT_ROW => {
                // The recorder aligns its chord button against the row BELOW it and the gap
                // between them; the page states those two metrics here, where it knows them,
                // instead of the smoke reading them back off a layout the page no longer owns.
                let layout = canvas.layout();
                ui.clipboard_pin_follow_row_height = layout.described_row_h;
                ui.clipboard_row_gap = layout.row_gap;
                rendered.controls.push(draw_shortcut_row(
                    canvas,
                    origin_y,
                    height,
                    &self.shortcut_title,
                    self.recording,
                    target,
                ))
            }
            CLIPBOARD_MAX_ENTRIES_ROW => rendered
                .controls
                .push(draw_max_entries_row(canvas, origin_y, height)),
            other => log_info!("[settings] Clipboard draws no row named {other}"),
        }
        Ok(())
    }

    /// The excluded-applications card: the list, its remove buttons, the add popup and the restore
    /// action. It follows the described cards and is drawn here, with the same writes the inline
    /// builder made (search rows and the `ui.clipboard_excluded_*` fields), because the list is not
    /// a row set the description can express.
    unsafe fn after_page(
        &mut self,
        canvas: &mut PageCanvas,
        _rendered: &mut RenderedPage,
        ui: &mut SettingsUi,
        target: *mut AnyObject,
    ) -> Result<(), RegisterError> {
        draw_excluded_card(canvas, ui, target);
        Ok(())
    }
}

/// The shortcut recorder row: the row label and its instruction caption on the left, and the chord
/// button on the right.
unsafe fn draw_shortcut_row(
    canvas: &mut PageCanvas,
    origin_y: f64,
    row_h: f64,
    title: &str,
    recording: bool,
    target: *mut AnyObject,
) -> RenderedControl {
    // The row's caption carries both the instruction and the capture state: the 110pt button can
    // only hold the chord, so "what this button does" and "a capture is waiting" are the row's copy.
    let idle = t("settings.desc_clipboard_shortcut");
    let waiting = t("settings.desc_clipboard_shortcut_recording");
    let states = [idle.clone(), waiting.clone()];
    draw_capture_row_with(
        canvas,
        origin_y,
        row_h,
        "settings.row_clipboard_shortcut",
        Some(CaptureCaption {
            shown: if recording { &waiting } else { &idle },
            states: &states,
        }),
        title,
        target,
        sel!(handleClipboardShortcutRecord:),
        ControlId::Action("record_shortcut"),
    )
}

/// What a capture row's caption says, for the caller's own state keys.
///
/// A recorder's caption has one copy per capture state (idle, waiting), and a state change only
/// rewrites the string -- so the frame and the row are sized from the TALLEST copy, not from the one
/// that happens to be on screen when the page is built. The caller supplies all of them: the drawer
/// cannot know another page's state keys, and guessing them sized the Gestures row from the
/// Clipboard page's copy (one line for a two-line sentence, and it was cut).
pub(super) struct CaptureCaption<'a> {
    /// The copy shown now.
    pub(super) shown: &'a str,
    /// Every copy the row can show, in any state.
    pub(super) states: &'a [String],
}

impl CaptureCaption<'_> {
    /// The tallest height these copies need in a `width`-wide column.
    fn height(&self, width: f64) -> f64 {
        self.states
            .iter()
            .map(|state| unsafe { crate::settings::widgets::caption_cell_height(state, width) })
            .fold(0.0, f64::max)
    }
}
#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn draw_capture_row_with(
    canvas: &mut PageCanvas,
    origin_y: f64,
    row_h: f64,
    label_key: &str,
    caption: Option<CaptureCaption<'_>>,
    title: &str,
    target: *mut AnyObject,
    selector: Sel,
    id: ControlId,
) -> RenderedControl {
    let document = canvas.document();
    let layout = canvas.layout();
    canvas.set_pending_advance(AdvanceState::Absolute);
    // A captioned row's copy is the design system's stack: the title's line above the caption's,
    // centred as ONE block in the band, and the button is centred in the same band. Both frames take
    // a captioned row's copy column, and the caption's height is the TALLEST copy any capture state
    // can show -- the same measurement the row's height was declared from. Sizing the frame for the
    // state that happens to be on screen would clip the other one: a state change only rewrites the
    // string, so the frame has to have room for both from the start.
    let (label, caption_label) = match caption {
        Some(caption) => {
            let copy_w = crate::settings::page_view::layout::caption_column_width(layout);
            let metrics = crate::settings::page_view::layout::MeasuredText::base();
            let caption_h = caption.height(copy_w).max(metrics.caption_one_line_h);
            let stack_h = metrics.title_h + metrics.title_caption_gap + caption_h;
            let stack_y = origin_y + (row_h - stack_h) / 2.0;
            let (label, caption_label) =
                page_widgets::stacked_row_labels(document, &t(label_key), caption.shown);
            let label_frame = NSRect::new(
                NSPoint::new(
                    layout.label_x,
                    stack_y + caption_h + metrics.title_caption_gap,
                ),
                NSSize::new(copy_w, metrics.title_h),
            );
            let caption_frame = NSRect::new(
                NSPoint::new(layout.label_x, stack_y),
                NSSize::new(copy_w, caption_h),
            );
            let _: () = msg_send![label, setFrame: label_frame];
            canvas.place_pending_view(label, label_frame);
            let _: () = msg_send![caption_label, setFrame: caption_frame];
            canvas.place_pending_view(caption_label, caption_frame);
            (label, caption_label)
        }
        None => {
            let label = page_widgets::plain_row_label(
                document,
                layout.label_x,
                origin_y,
                220.0,
                row_h,
                &t(label_key),
            );
            // `plain_row_label` insets the band by 8, so the frame has to be fitted to the measured
            // cell height and centred: the same call a single-line described row makes.
            crate::settings::components::SettingsRow::center_label(label, origin_y, row_h);
            let label_frame: NSRect = msg_send![label, frame];
            canvas.place_pending_view(label, label_frame);
            (label, std::ptr::null_mut())
        }
    };
    let button = SettingsControl::button(
        layout.control_x + layout.control_w - ROW_ACTION_BTN_W,
        // Centred in the ROW's band, like every described captioned row's control.
        origin_y + (row_h - ROW_ACTION_BTN_H) / 2.0,
        ROW_ACTION_BTN_W,
        ROW_ACTION_BTN_H,
        title,
        target,
        selector,
        SettingsButtonRole::ControlField,
    );
    let button_frame: NSRect = msg_send![button, frame];
    let _: () = msg_send![document, addSubview: button];
    canvas.place_pending_view(button, button_frame);
    release_obj(button);
    SettingsRow::register_label(label, button);
    let tooltip = make_nsstring(&t(label_key));
    let _: () = msg_send![button, setToolTip: tooltip];
    release_obj(tooltip);
    RenderedControl {
        id,
        kind: ControlKind::Button {
            title_key: "settings.btn_open",
        },
        control: button,
        readout: std::ptr::null_mut(),
        action: std::ptr::null_mut(),
        // The row's height and the caption that states its capture state. The capture machine writes
        // the caption's copy and NOTHING else: the page drew the button in the row it belongs to, and
        // the canvas -- or a search result row, which borrows this row's views -- owns where the row
        // sits. A frame written from here would be in neither coordinate space.
        extra: RenderedExtra::Shortcut {
            row_height: row_h,
            caption: caption_label,
        },
    }
}

/// The numeric entry limit row: the label on the left and the text field in the control column.
unsafe fn draw_max_entries_row(
    canvas: &mut PageCanvas,
    origin_y: f64,
    row_h: f64,
) -> RenderedControl {
    let document = canvas.document();
    let layout = canvas.layout();
    canvas.set_pending_advance(AdvanceState::Absolute);
    let label = page_widgets::plain_row_label(
        document,
        layout.label_x,
        origin_y,
        layout.label_w,
        row_h,
        &t("settings.row_clipboard_max_entries"),
    );
    let label_frame: NSRect = msg_send![label, frame];
    canvas.place_pending_view(label, label_frame);
    let initial = crate::config::CONFIG
        .read()
        .unwrap()
        .clipboard
        .max_entries
        .to_string();
    let field = SettingsControl::text_input(
        layout.control_x,
        origin_y + (row_h - layout.row_h) / 2.0,
        layout.control_w,
        layout.row_h,
        &initial,
    );
    let field_frame: NSRect = msg_send![field, frame];
    let _: () = msg_send![document, addSubview: field];
    canvas.place_pending_view(field, field_frame);
    release_obj(field);
    SettingsRow::register_label(label, field);
    RenderedControl {
        id: ControlId::Action("max_entries"),
        kind: ControlKind::Button {
            title_key: "settings.btn_open",
        },
        control: field,
        readout: std::ptr::null_mut(),
        action: std::ptr::null_mut(),
        extra: RenderedExtra::MaxEntries,
    }
}

/// Draw the excluded-applications card on the page canvas, exactly as the inline builder did: the
/// list comes from the configuration, the add popup's candidates are captured once per build, and
/// every remove button is registered in the page's own remove list.
unsafe fn draw_excluded_card(canvas: &mut PageCanvas, ui: &mut SettingsUi, target: *mut AnyObject) {
    let content_w = canvas.content_width();
    let layout = canvas.layout();
    let label_x = layout.label_x;
    let ctrl_w = layout.control_w;
    let ctrl_x = layout.control_x;
    let described_row_h = layout.described_row_h;
    // Copies made by these apps are never recorded (matched inside the poll, before the payload is
    // read). The list is a card of its own: it grows and shrinks, so it must not share a card with
    // the fixed rows above. next_section opens the group and card closes it over the rows, which is
    // the order every other page uses -- calling card first would re-title the rows above.
    canvas.next_section();
    let excluded = crate::config::CONFIG
        .read()
        .unwrap()
        .clipboard
        .excluded_apps
        .clone();
    // One row height for the whole card: it follows the binding list's single-line row (tall enough
    // for a 28pt row action with breathing room), not the 32pt control height the page's own rows
    // use. The list rows AND the controls under them share it, so the card reads as one rhythm.
    let excluded_row_h = described_row_h;
    if excluded.is_empty() {
        let cy = canvas.next_row(excluded_row_h);
        record_settings_search_row(
            SettingsPageId::Clipboard,
            canvas.last_row(),
            &t("settings.clipboard_excluded_empty"),
            None,
        );
        let label = make_value_label(
            label_x,
            cy + 10.0,
            content_w - label_x - 12.0,
            18.0,
            &t("settings.clipboard_excluded_empty"),
        );
        apply_settings_text_role(label, SettingsTextRole::Secondary);
        let _: () = msg_send![canvas.document(), addSubview: label];
        release_obj(label);
    } else {
        let last = excluded.len().saturating_sub(1);
        for (index, entry) in excluded.iter().enumerate() {
            let cy = canvas.next_row(excluded_row_h);
            // The row style follows the Key Bindings list: a name and one trailing row action, with a
            // divider between rows instead of a bordered control per row. The LAST row hands that
            // boundary to the row below it (the add control draws its own), so the card never carries
            // two overlapping lines at one edge.
            if index < last {
                SettingsRow::separator(canvas.document(), cy, content_w);
            }
            // The display name is a snapshot taken when the entry was added; the identifier is the
            // only matching key and is shown when the snapshot is missing.
            let display = if entry.name.is_empty() {
                entry.bundle_id.clone()
            } else {
                entry.name.clone()
            };
            record_settings_search_row(
                SettingsPageId::Clipboard,
                canvas.last_row(),
                &display,
                None,
            );
            let btn_y = cy + (excluded_row_h - bindings::BINDING_BTN_H) / 2.0;
            let delete = SettingsButton::action(
                NSRect::new(
                    NSPoint::new(ctrl_x + ctrl_w - bindings::BINDING_BTN_W, btn_y),
                    NSSize::new(bindings::BINDING_BTN_W, bindings::BINDING_BTN_H),
                ),
                &t("settings.clipboard_excluded_remove"),
                target,
                sel!(handleClipboardExcludedRemove:),
                SettingsButtonRole::ROW_ACTION,
            );
            // The returned control is the remove button: the handler finds the entry by comparing
            // the sender pointer against this list.
            // `tall_before_control`, not `plain`: the tall row measures its label for a single-line
            // row, which is what keeps the name and the 28pt row action on one centre line. `plain`
            // lays the label out for the page's 32pt control rows and sits low in a 56pt row.
            let (_, remove) = SettingsRow::tall_before_control(
                canvas.document(),
                label_x,
                cy,
                ctrl_x + ctrl_w - bindings::BINDING_BTN_W,
                super::SETTINGS_CONTROL_LABEL_GAP,
                &display,
                delete,
            );
            ui.clipboard_excluded_remove.push(remove);
        }
    }
    // Add a running app. The candidates are captured here so the handler can map the popup's
    // selection index back to an identifier without re-enumerating (and re-sorting) running apps
    // between the click and the callback.
    let running = crate::app_identity::running_apps_with_bundle_ids();
    let listed: Vec<String> = excluded
        .iter()
        .map(|e| e.bundle_id.to_ascii_lowercase())
        .collect();
    let candidates: Vec<(String, String)> = running
        .into_iter()
        .filter(|(bundle_id, _)| !listed.contains(&bundle_id.to_ascii_lowercase()))
        .collect();
    // ONE add control. The popup lists the running candidates and carries a trailing "choose from
    // disk" entry, the macOS "Other…" pattern: the same intent, reached from one place, instead of
    // two rows that both mean "exclude an application".
    let mut add_labels: Vec<String> = vec![t("settings.clipboard_excluded_add_placeholder")];
    add_labels.extend(candidates.iter().map(|(_, name)| name.clone()));
    add_labels.push(t("settings.clipboard_excluded_choose_from_disk"));
    let add_label_refs: Vec<&str> = add_labels.iter().map(String::as_str).collect();
    let add_metrics = SettingsSelect::metrics();
    let cy = canvas.next_row(excluded_row_h);
    // The list's last row handed this boundary over, so this is the one and only line here.
    SettingsRow::separator_above_row(canvas.document(), cy, excluded_row_h, content_w);
    record_settings_search_row(
        SettingsPageId::Clipboard,
        canvas.last_row(),
        &t("settings.row_clipboard_excluded_add"),
        None,
    );
    let add_popup = SettingsControl::popup(
        ctrl_x,
        cy + (excluded_row_h - add_metrics.control_h) / 2.0,
        ctrl_w,
        add_metrics.control_h,
        &add_label_refs,
        0,
    );
    style_flat_popup(add_popup);
    let _: () = msg_send![add_popup, setTarget: target];
    let _: () = msg_send![add_popup, setAction: sel!(handleClipboardExcludedAdd:)];
    ui.clipboard_excluded_add = SettingsRow::tall_before_control(
        canvas.document(),
        label_x,
        cy,
        ctrl_x,
        super::SETTINGS_CONTROL_LABEL_GAP,
        &t("settings.row_clipboard_excluded_add"),
        add_popup,
    )
    .1;
    ui.clipboard_excluded_candidates = candidates;
    // (The disk picker lives inside the add popup above, as its trailing entry.)
    // Restore the built-in list.
    let cy = canvas.next_row(excluded_row_h);
    SettingsRow::separator_above_row(canvas.document(), cy, excluded_row_h, content_w);
    record_settings_search_row(
        SettingsPageId::Clipboard,
        canvas.last_row(),
        &t("settings.row_clipboard_excluded_restore"),
        None,
    );
    ui.clipboard_excluded_restore = SettingsRow::tall_before_control(
        canvas.document(),
        label_x,
        cy,
        ctrl_x + ctrl_w - ROW_ACTION_BTN_W,
        super::SETTINGS_CONTROL_LABEL_GAP,
        &t("settings.row_clipboard_excluded_restore"),
        SettingsControl::button(
            ctrl_x + ctrl_w - ROW_ACTION_BTN_W,
            cy + (excluded_row_h - ROW_ACTION_BTN_H) / 2.0,
            ROW_ACTION_BTN_W,
            ROW_ACTION_BTN_H,
            &t("settings.clipboard_excluded_restore_action"),
            target,
            sel!(handleClipboardExcludedRestore:),
            SettingsButtonRole::ControlField,
        ),
    )
    .1;

    canvas.card(&t("settings.header_clipboard_excluded"));
    set_settings_search_section(&t("settings.header_clipboard_excluded"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> PageInput {
        PageInput::from_config(
            crate::config::Config::default(),
            None,
            crate::theme::ThemeInput::current(),
        )
    }

    #[test]
    fn the_shortcut_row_measures_its_caption() {
        // The recorder's row carries a caption, and its copy wraps: the height has to be the
        // measured captioned-row height for both capture copies, not the single-line constant. A
        // fixed height clipped the instruction in any language where it did not fit one line.
        let view = clipboard_page_view(&input());
        let row = view
            .extra_rows
            .iter()
            .find(|row| row.key == CLIPBOARD_SHORTCUT_ROW)
            .expect("the page declares the recorder row");
        assert!(
            matches!(row.height, RowHeight::Measured(_)),
            "the recorder's row height must be measured from its caption"
        );
        // Both states are measured, so neither copy can be the one that overflows.
        assert_eq!(CAPTURE_CAPTION_KEYS.len(), 2);
    }

    #[test]
    fn the_capture_state_is_stated_in_the_row_caption() {
        // The 110pt button can only hold the chord, so the row's caption is where the instruction
        // and the recording state live: a missing key would ship the raw key as the row's copy.
        let idle = t("settings.desc_clipboard_shortcut");
        let recording = t("settings.desc_clipboard_shortcut_recording");
        assert_ne!(
            idle, "settings.desc_clipboard_shortcut",
            "the idle key is missing"
        );
        assert_ne!(
            recording, "settings.desc_clipboard_shortcut_recording",
            "the recording key is missing"
        );
        assert_ne!(
            idle, recording,
            "the two capture states must not read the same, or pressing the button says nothing"
        );
    }

    #[test]
    fn clipboard_page_view_describes_every_static_row_with_its_identity() {
        let view = clipboard_page_view(&input());
        assert_eq!(view.page, SettingsPageId::Clipboard);
        let fields: Vec<ControlField> = view.rows.iter().filter_map(|row| row.field()).collect();
        assert_eq!(
            fields,
            vec![
                ControlField::ClipboardEnabled,
                ControlField::ClipboardPaused,
                ControlField::ClipboardPinFollow,
                ControlField::ClipboardPickerPlacement,
                ControlField::ClipboardClearOnQuit,
                ControlField::ClipboardSeparateClearAll,
                ControlField::ClipboardShowSourceApp,
                ControlField::ClipboardMoveUsedToTop,
                ControlField::ClipboardDeleteAfterPaste,
                ControlField::ClipboardClearSystemPasteboardAfterPaste,
                ControlField::ClipboardAutoExpireDays,
                ControlField::ClipboardImageRows,
            ]
        );
        // The switches that carry a drawn caption (the feature warning and the destructive gesture
        // hint), and the one that is a plain switch.
        assert_eq!(view.rows[0].copy, RowCopy::Captioned);
        assert_eq!(view.rows[1].copy, RowCopy::Captioned);
        assert_eq!(view.rows[8].copy, RowCopy::Captioned);
        assert_eq!(view.rows[2].copy, RowCopy::Plain);
        // The numeric entry limit is page-built: it is not a described row at all.
        let ids: Vec<&str> = view.rows.iter().map(|row| row.label_key).collect();
        assert!(!ids.contains(&"settings.row_clipboard_max_entries"));
        // The clear-system-pasteboard row is the child of "delete after paste".
        assert_eq!(
            view.rows[9].group,
            Some(RowGroup::ClipboardDeleteChild),
            "the clear-pasteboard row is the child of delete-after-paste"
        );
        // No described row carries an action identity: the excluded-applications card is
        // page-drawn end to end (the list, the add popup and the restore action), so a static
        // action row here would draw a second copy of one of them.
        let actions: Vec<&str> = view
            .rows
            .iter()
            .filter_map(|row| match row.id {
                ControlId::Action(action) => Some(action),
                ControlId::Field(_) => None,
            })
            .collect();
        assert!(actions.is_empty(), "{actions:?}");
        // The cards are the page's own, in order.
        assert_eq!(view.rows[0].section, "settings.header_clipboard");
        assert_eq!(view.rows[2].section, "settings.header_clipboard_options");
        assert_eq!(
            view.rows.last().unwrap().section,
            "settings.header_clipboard_options"
        );
    }

    #[test]
    fn clipboard_values_come_from_the_config() {
        let mut config = crate::config::Config::default();
        config.clipboard.paused = true;
        config.clipboard.max_entries = 37;
        config.clipboard.auto_expire_days = 5;
        config.clipboard.pin_follow_selection = false;
        let input = PageInput::from_config(config, None, crate::theme::ThemeInput::current());
        let view = clipboard_page_view(&input);
        let value = |field: ControlField| {
            view.rows
                .iter()
                .find(|row| row.field() == Some(field))
                .map(|row| row.value.clone())
                .expect("the field's row")
        };
        assert_eq!(value(ControlField::ClipboardPaused), Value::Switch(true));
        // The numeric entry limit is not in the description: the page builds it as a text field,
        // which dispatches through `text_field_of`. It therefore has no `ControlField` at all, and
        // this asserts the absence by the label key rather than by a variant that no longer exists.
        assert!(view
            .rows
            .iter()
            .all(|row| row.label_key != "settings.row_clipboard_max_entries"));
        // The auto-expire readout is the value itself (days, not a stop index).
        assert_eq!(
            value(ControlField::ClipboardAutoExpireDays),
            Value::Slider(5)
        );
        // "Keep current position" is the second item, i.e. index 1.
        assert_eq!(value(ControlField::ClipboardPinFollow), Value::Index(1));
    }

    #[test]
    fn every_clipboard_row_has_a_search_entry() {
        let _guard = crate::settings::sidebar::lock_search_index_for_test();
        let view = clipboard_page_view(&input());
        for row in &view.rows {
            assert!(
                !row.search.label.is_empty(),
                "{} has no search label",
                row.label_key
            );
            assert!(
                !row.search.section.is_empty(),
                "{} has no section",
                row.label_key
            );
        }
    }
}
