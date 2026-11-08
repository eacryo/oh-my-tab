//! Applying a laid-out page to AppKit: the third stage, and the only one that draws.
//!
//! `render_page` reads the boxes `layout_rows` computed and applies them; it derives no frame of
//! its own. A control is registered with the `ControlTable` as it is created, which is what lets the
//! dispatcher name a clicked control without a pointer chain.

use super::super::*;
use super::*;

/// A control the page just built, with everything a later write to it needs.
#[derive(Clone, Debug)]
pub(super) struct RenderedControl {
    pub(super) id: ControlId,
    pub(super) kind: ControlKind,
    /// The control the dispatcher sees (the switch, the popup, the slider or the button).
    pub(super) control: *mut AnyObject,
    /// The slider's readout, or null.
    pub(super) readout: *mut AnyObject,
    /// The action button beside a value row, or null. A status row binds its label and its button
    /// separately: the refresh function writes the text and enables the button by its own pointer.
    pub(super) action: *mut AnyObject,
    /// What a page-drawn control needs beyond its identity. A described row is always `None`: its
    /// fields are bound from its `ControlId`.
    pub(super) extra: RenderedExtra,
}

/// What a page-drawn control needs beyond its identity: the few `SettingsUi` fields its own
/// callback reads, which no described row has.
///
/// A page-drawn row is outside the described control set, so its consumer is the page's own code.
/// This is the narrow channel between the two: the hook says what it drew, and
/// `bind_page_controls` puts it on the fields that code reads.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) enum RenderedExtra {
    /// Nothing beyond the identity (every described row).
    #[default]
    None,
    /// The clipboard shortcut recorder: its row height follows the recorded chord, and the capture
    /// state machine reads it back.
    Shortcut { row_height: f64 },
    /// The clipboard history limit: a numeric field whose callback looks it up by pointer.
    MaxEntries,
}

unsafe impl Send for RenderedControl {}
unsafe impl Sync for RenderedControl {}

/// What a page build produced: the views it created and the identity of each control.
#[derive(Default)]
pub(in crate::settings) struct RenderedPage {
    /// The page's controls, in row order.
    pub(super) controls: Vec<RenderedControl>,
    /// The registrations the page made. Not published until the build has finished.
    pub(super) registrations: PageRegistrations,
    /// The banner's strip, label and action button (null when the page has no banner).
    pub(super) banner_strip: *mut AnyObject,
    pub(super) banner_label: *mut AnyObject,
    pub(super) banner_button: *mut AnyObject,
    /// The About page's Updates card, which the inline update flow reads (`ui.update_card`).
    /// The shared renderer builds every card, so it is the only place that can publish it; a page
    /// that built its own card would be a second construction path, which is the defect the
    /// description exists to remove.
    pub(super) update_card: *mut AnyObject,
}

impl RenderedPage {
    /// The control a row's identity built, if this page has that row.
    pub(super) fn control(&self, id: &ControlId) -> Option<&RenderedControl> {
        self.controls.iter().find(|control| &control.id == id)
    }
}

/// The selector a field's control runs instead of the unified `handleControlChanged:`, when the
/// field has effects the unified handler does not cover.
///
/// The Windows master switch is the one case: turning the switcher off has to freeze the options
/// below it in the same action. Every other static control goes through the unified handler.
pub(super) fn dedicated_selector(field: Option<ControlField>) -> Sel {
    // A field whose control had its own handler in the inline builder keeps it: each of these
    // does more than write the config (it retitles a button, freezes dependent rows, re-reads a
    // device, restarts a tap). Losing one is silent — the value still changes, the page just stops
    // following it.
    match field {
        Some(ControlField::WindowsEnabled) => sel!(handleWindowsEnabledToggle:),
        Some(ControlField::MouseEnabled) => sel!(handleEnableMouseToggle:),
        Some(ControlField::MappingEnabled) => sel!(handleMappingEnabledChanged:),
        Some(ControlField::ClipboardEnabled) => sel!(handleClipboardEnabledToggle:),
        Some(ControlField::ClipboardPaused) => sel!(handlePauseRecordingToggle:),
        Some(ControlField::KeyBindingsEnabled) => sel!(handleKeyBindingsToggle:),
        _ => sel!(handleControlChanged:),
    }
}

/// The selector an action row's button runs.
fn action_selector(action: &str) -> Sel {
    match action {
        "export_settings" => sel!(handleExportSettings:),
        // The Gestures page's excluded-application rows: Remove one entry.
        "gesture_excluded_remove" => sel!(handleGestureExcludedRemove:),
        // The Gestures page's "Add an application": opens the system application chooser.
        "gesture_excluded_choose" => sel!(handleGestureExcludedChoose:),
        // A gesture row's Delete. It rebuilds the page itself.
        action if crate::settings::page_view::delete_row_index(action).is_some() => {
            sel!(handleGestureRowDelete:)
        }
        // A gesture row's action popup: the list of buttons that row can start on.
        action if crate::settings::page_view::is_gesture_row_identity(action) => {
            sel!(handleGestureRowTrigger:)
        }
        // One line of that list.
        action if crate::settings::page_view::is_trigger_row_identity(action) => {
            sel!(handleGestureTriggerLine:)
        }
        // The Gestures page's recorder: start a recording, or cancel the one running.
        "gesture_record" => sel!(handleGestureRecordStart:),
        "gesture_record_cancel" => sel!(handleGestureRecordCancel:),
        // The General page's "Export Logs" row. It used to be reached through the fallback arm,
        // which the unknown-action safety net replaced -- so it is spelled out here.
        "export_logs" => sel!(handleExportLogs:),
        "import_settings" => sel!(handleImportSettings:),
        // The Finder Menu page's status row opens the extension's System Settings pane.
        "open_finder_extension_settings" => sel!(handleOpenFinderExtensionManagement:),
        // The About page's permission rows open their own System Settings panes, and its keychain
        // row retries the read.
        "open_accessibility_permission" => sel!(handleOpenPrivacy:),
        "open_screen_recording_permission" => sel!(handleOpenScreenRecordingPrivacy:),
        "retry_keychain_access" => sel!(retryKeychainAccess:),
        // The About page's guide row opens the onboarding window.
        "open_onboarding" => sel!(handleOpenOnboarding:),
        // An action nobody handles must do NOTHING. Defaulting to export-logs made a wrong value
        // run an unrelated feature -- and a read-only row's action is never called at all.
        _ => sel!(handleUnknownPageAction:),
    }
}

/// Whether a row's control is TEXT that the renderer fits to its own line before centring it.
///
/// A read-only value and a hyperlink are both read-only NSTextFields, and an NSTextField draws its
/// cell from the TOP of its box: given the layout's full control box (32pt for an 18pt line) the
/// glyphs sat ~7pt above the row's middle while the row's own label was centred, so the About
/// page's value and link rows read visibly high. Controls that are not text (switch, popup, slider,
/// button) are drawn as the layout places them.
///
/// The predicate is a function, not an inline `matches!`, so the renderer and its test can agree on
/// the branch without the test re-implementing it: a link dropped from this list turns the row high
/// again, and the test fails on that rather than on a copy of the formula.
pub(super) fn fits_as_text(control: &ControlKind) -> bool {
    matches!(
        control,
        ControlKind::Value { .. } | ControlKind::Link { .. }
    )
}

/// The boxes a row's views are placed in, in the order the renderer places them: the copy (one
/// box, or a title and a caption for a stacked row), the control, and a slider's readout.
///
/// Pure, and the single source for both halves of the row's placement: the renderer places the
/// views in exactly these boxes, and the test asserts them without AppKit.
pub(super) fn row_placement_boxes(row: &RowSpec, placed: &RowLayout) -> Vec<BoxRect> {
    let mut boxes = Vec::with_capacity(4);
    match placed.caption_frame {
        Some(caption) => {
            boxes.push(placed.label_frame);
            boxes.push(caption);
        }
        None => boxes.push(placed.label_frame),
    }
    boxes.push(placed.control_frame);
    if matches!(row.control, ControlKind::Slider { .. }) {
        if let Some(readout) = placed.readout_frame {
            boxes.push(readout);
        }
    }
    boxes
}

/// Build one row: its copy and its control, handed to the canvas with the boxes the layout gave
/// them.
///
/// Nothing here keeps a frame of its own: the copy goes through the row's own placement (the
/// canvas captured those views when `next_row` returned and re-places them every walk), and the
/// control and a slider's readout are placed explicitly. The copy is built by the same labels the
/// inline pages used, so its face is unchanged.
pub(super) unsafe fn render_row(
    canvas: &mut PageCanvas,
    target: *mut AnyObject,
    row: &RowSpec,
    placed: &RowLayout,
    rendered: &mut RenderedPage,
) -> Result<(), RegisterError> {
    let parent = canvas.document();
    canvas.next_row(placed.frame.h);
    canvas.set_pending_advance(AdvanceState::Absolute);
    canvas.set_pending_group(row.group);
    // The in-card divider belongs to THIS row: created after the row opened, so the canvas
    // captures it with the row, placed through the row's placements, and hidden with its group.
    if row.separator_above {
        place_row_separator(canvas, &placed.frame);
    }
    // The row's application icon, when the row is about one. It is drawn in the label column's
    // leading edge and the LABEL is moved right by exactly its width and gap, so the text keeps the
    // inset every other row starts at. An application the system cannot locate draws no icon and
    // keeps its plain label: a generic placeholder would claim the application is there.
    let icon_width = match &row.icon_bundle {
        Some(bundle) if place_row_icon(canvas, parent, bundle, placed) => {
            ICON_SLOT_W + ICON_LABEL_GAP
        }
        _ => 0.0,
    };
    let mut placed = placed.clone();
    if icon_width > 0.0 {
        placed.label_frame.x += icon_width;
        placed.label_frame.w = (placed.label_frame.w - icon_width).max(1.0);
    }
    let placed = &placed;
    // The row's label copy: the locale key, or the data the page computed for it.
    let title = row.label_text.clone().unwrap_or_else(|| t(row.label_key));
    // The row's title view: what the accessibility label and the disabled-row pairing name.
    let title_label: *mut AnyObject = match row.caption_key {
        Some(caption_key) => {
            let caption = t(caption_key);
            let (title_label, caption_label) =
                page_widgets::stacked_row_labels(parent, &title, &caption);
            // The block's own views were sized for its build; the layout says where they go.
            set_frame(title_label, &placed.label_frame);
            if let Some(caption_frame) = placed.caption_frame {
                set_frame(caption_label, &caption_frame);
            }
            let boxes = row_placement_boxes(row, placed);
            canvas.place_pending_view(title_label, boxes[0].rect());
            canvas.place_pending_view(caption_label, boxes[1].rect());
            title_label
        }
        None => {
            // A plain row: the shared label builder, exactly the one the inline pages used.
            //
            // The VIEW occupies the row's band, not the layout's centred text box. The box says
            // where the text's first line sits (the smoke and the search highlight read it), but a
            // label that wraps to two lines needs the band's height: built at the centred box it
            // was 18pt tall and the second line was truncated. Master's inline builder built the
            // label as `(x, row_y)` with `row_h - 8`, so the band is also its vertical centring.
            let band = NSRect::new(
                NSPoint::new(placed.label_frame.x, placed.frame.y),
                NSSize::new(placed.label_frame.w.max(1.0), placed.frame.h),
            );
            let label = page_widgets::plain_row_label(
                parent,
                band.origin.x,
                band.origin.y,
                band.size.width,
                band.size.height,
                &title,
            );
            // The band is the VIEW's box, but the LABEL's frame is what the smoke and every
            // geometry assertion in this repository measures. Master's inline builder centred it
            // in the band (`SettingsRow::center_label`): fit the frame to the cell's measured
            // height, then centre that, then apply the line-height correction. Without this the
            // label kept the band's 48pt frame and its glyphs read 11pt above the row's centre.
            crate::settings::components::SettingsRow::center_label(
                label,
                placed.frame.y,
                placed.frame.h,
            );
            let label_frame: NSRect = msg_send![label, frame];
            canvas.place_pending_view(label, label_frame);
            label
        }
    };
    let control: *mut AnyObject = match &row.control {
        // A SELECT reports the chosen option through `sendPendingAction`, and that call reads the
        // select's OWN target/action -- so the row has to install them. The unified change handler
        // is the one thing a select must not get: installing it arms NSButton's press tracking and
        // replaces the select's `mouseDown:`, so the panel never opens (the "popup that does
        // nothing"). The generic bind below therefore skips popups, and the row's own handler is
        // installed here instead; the panel-opening `mouseDown:` is left alone by target/action.
        ControlKind::Switch => {
            let on = matches!(row.value, Value::Switch(true));
            // The switch's visual is 42x26; build it on the ROW BAND so the whole trailing cell is
            // its hit target. At the control box's own 26pt height a click a few points off centre
            // misses the view and lands on the row, which reads as a switch that ignores clicks.
            SettingsControl::switch(
                placed.control_frame.x + placed.control_frame.w,
                placed.frame.y,
                placed.frame.h,
                on,
            )
        }
        ControlKind::Popup { options } => {
            let refs: Vec<&str> = options.iter().map(String::as_str).collect();
            let selected = match row.value {
                Value::Index(index) => index,
                _ => 0,
            };
            let popup = SettingsControl::popup(
                placed.control_frame.x,
                placed.control_frame.y,
                placed.control_frame.w,
                placed.control_frame.h,
                &refs,
                selected,
            );
            if !popup.is_null() {
                let _: () = msg_send![popup, setTarget: target];
                let _: () = msg_send![popup, setAction: dedicated_selector(row.field())];
            }
            popup
        }
        // A hyperlink: the widget opens the URL its tag names on a click.
        ControlKind::Link { tag, text_key } => SettingsControl::external_link(
            placed.control_frame.x,
            placed.control_frame.y,
            placed.control_frame.w,
            placed.control_frame.h,
            &t(text_key),
            *tag,
        ),
        ControlKind::Slider {
            min,
            max,
            step,
            reset,
            ..
        } => {
            let value = match row.value {
                Value::Slider(value) => value,
                _ => *min,
            };
            SettingsControl::stepped_slider(
                placed.control_frame.x,
                placed.control_frame.y,
                placed.control_frame.w,
                placed.control_frame.h,
                *min,
                *max,
                *step,
                value,
                *reset,
            )
        }
        // A read-only value: text in the control column, refreshed by its callback (permission
        // status, keychain status). Its identity still comes from the row, so a later update can
        // find it.
        ControlKind::Value { action, align } => {
            let text = match &row.value {
                Value::Dynamic(text) => text.clone(),
                other => other.describe(),
            };
            // A value row's action button is a second view in the row, drawn in the box the layout
            // reserved for it beside the label.
            let _ = action;
            let label = SettingsControl::value_label(
                placed.control_frame.x,
                placed.control_frame.y,
                placed.control_frame.w,
                placed.control_frame.h,
                &text,
            );
            // The shipped pages are not uniform about this (see [`ValueAlign`]), so the row states
            // it rather than leaving every value on the platform default.
            if matches!(align, ValueAlign::Right) && !label.is_null() {
                let _: () = msg_send![label, setAlignment: super::components::TEXT_ALIGNMENT_RIGHT];
            }
            label
        }
        ControlKind::Button { title_key } => {
            let action = match &row.id {
                ControlId::Action(action) => *action,
                ControlId::Field(_) => "export_logs",
            };
            // The BUTTON's copy, which is not the row's label: the About page's guide row reads
            // "View guide" on the left and "Open" on the button.
            SettingsControl::button(
                placed.control_frame.x,
                placed.control_frame.y,
                placed.control_frame.w,
                placed.control_frame.h,
                &t(title_key),
                target,
                action_selector(action),
                SettingsButtonRole::Action,
            )
        }
    };
    if control.is_null() {
        return Ok(());
    }
    // The control enters the view tree and the page gives up its +1: every widget above is
    // allocated +1 and every inline builder released its view after adding it.
    let _: () = msg_send![control, setFrame: placed.control_frame.rect()];
    let _: () = msg_send![parent, addSubview: control];
    release_obj(control);
    // A read-only value is TEXT, not a control: the inline builder fitted it to its own line height
    // and centred that in the row band (\`center_readonly_text_control\`), so a value shares its row
    // label's origin instead of sitting in a 32pt control box. The fitted frame is what gets RECORDED
    // as the placement: the canvas re-applies a recorded frame on every reflow, so placing the
    // layout's box and centring afterwards would lose the fit on the next pass.
    // The box recorded for THIS view: the control's own frame, never `row_placement_boxes(..).last()`.
    // That list ends with the SLIDER READOUT when the row has one, so taking its last element made the
    // canvas record the readout's box as the slider's placement -- and every reflow then moved the
    // slider onto its own readout, 136pt right and past the document's edge on the Switcher page.
    //
    // A hyperlink is text under the same rule: an NSTextField draws its cell from the top of its
    // box, so a link left in the layout's full 32pt control box sat ~7pt above the row's middle
    // while its label was centred. Fitting it to its own line height and centring that in the row
    // band puts its glyphs on the row's centre line, like a value label's.
    let control_box = if fits_as_text(&row.control) {
        SettingsRow::center_readonly_text_control(control, placed.frame.y, placed.frame.h);
        let frame: NSRect = msg_send![control, frame];
        BoxRect::new(
            frame.origin.x,
            frame.origin.y,
            frame.size.width,
            frame.size.height,
        )
    } else {
        placed.control_frame
    };
    canvas.place_pending_view(control, control_box.rect());
    // The shared association contract: VoiceOver label + the ROW_LABELS pairing the freeze lists
    // use to dim the row's label and give it the disabled tooltip.
    SettingsRow::register_label(title_label, control);
    // A POPUP is not bound. The custom settings select opens its options from its own `mouseDown:`
    // and reports the choice through `sendPendingAction`; installing the unified change handler on
    // it sets target/action AND arms NSButton's press tracking, which replaces that `mouseDown:` --
    // the panel then never opened, and the row read as a control that does nothing. A row's identity
    // still reaches the dispatcher, from the select's own action.
    let is_popup = matches!(row.control, ControlKind::Popup { .. });
    if !matches!(row.control, ControlKind::Button { .. }) && !is_popup {
        // `bind_control` is the unified handler; a field with a dedicated callback keeps it.
        let selector = dedicated_selector(row.field());
        let _: () = msg_send![control, setTarget: target];
        let _: () = msg_send![control, setAction: selector];
    }
    // A value row's action button: it is the row's second control, so it enters the view tree and
    // the page's binding list beside the label rather than instead of it.
    let mut action_button = std::ptr::null_mut();
    if let (
        ControlKind::Value {
            action: Some(action),
            ..
        },
        Some(frame),
    ) = (&row.control, placed.action_frame)
    {
        let button = SettingsControl::button(
            frame.x,
            frame.y,
            frame.w,
            frame.h,
            &t(action.button_key),
            target,
            action_selector(action.action),
            SettingsButtonRole::Action,
        );
        if !button.is_null() {
            if let Some(tag) = row.action_tag {
                // WHICH entry a row's button acts on travels as its IDENTIFIER, never as its tag.
                // The tag already carries the role's palette slot, and the hover callbacks read it
                // to restore the right fill: overwriting it with a list index made every one of
                // these buttons fall back to the generic hover wash and stay there after the
                // pointer left (the fill a row kept was the HOVER color, not the row's own).
                let identifier = crate::settings::ffi_identifier_for_action_tag(tag);
                let _: () = msg_send![button, setIdentifier: identifier];
                crate::ffi::CFRelease(identifier as *const c_void);
            }
            let _: () = msg_send![button, setFrame: frame.rect()];
            let _: () = msg_send![parent, addSubview: button];
            release_obj(button);
            canvas.place_pending_view(button, frame.rect());
            action_button = button;
        }
    }
    let readout = match (&row.control, placed.readout_frame) {
        (ControlKind::Slider { readout, .. }, Some(frame)) => {
            let view = SettingsRow::attach_slider_readout(parent, control, readout);
            set_frame(view, &frame);
            canvas.place_pending_view(view, frame.rect());
            view
        }
        _ => std::ptr::null_mut(),
    };
    rendered.registrations.register(control, row.id.clone())?;
    rendered.controls.push(RenderedControl {
        id: row.id.clone(),
        kind: row.control.clone(),
        control,
        readout,
        action: action_button,
        // A described row binds every field its ControlId names; only a page-drawn row needs more.
        extra: RenderedExtra::None,
    });
    Ok(())
}

/// The width an application icon occupies in the label column, and the gap before the label.
///
/// The sidebar's own icon metrics (`SIDEBAR_ICON_SIZE`/`SIDEBAR_ICON_LABEL_GAP`) are the same visual
/// relationship at a smaller size; a settings row's label is drawn at control size, so the icon is
/// one line tall and the gap matches the sidebar's.
const ICON_SLOT_W: f64 = 18.0;
const ICON_LABEL_GAP: f64 = 8.0;

/// Draw `bundle`'s application icon at the leading edge of the row's label band.
///
/// Returns whether a view was placed, which is what tells `render_row` to move the label right: a
/// row whose bundle cannot be located (the application was moved or deleted) keeps its plain
/// label and the whole column, rather than leaving a blank gap where an icon would have been.
unsafe fn place_row_icon(
    canvas: &mut PageCanvas,
    parent: *mut AnyObject,
    bundle: &str,
    placed: &RowLayout,
) -> bool {
    let Some(image) = crate::app_identity::icon_for_bundle_id(bundle, ICON_SLOT_W) else {
        return false;
    };
    let view: *mut AnyObject = msg_send![class!(NSImageView), alloc];
    let frame = NSRect::new(
        NSPoint::new(
            placed.label_frame.x,
            placed.frame.y + (placed.frame.h - ICON_SLOT_W).max(0.0) / 2.0,
        ),
        NSSize::new(ICON_SLOT_W, ICON_SLOT_W),
    );
    let view: *mut AnyObject = msg_send![view, initWithFrame: frame];
    if view.is_null() {
        release_obj(image);
        return false;
    }
    let _: () = msg_send![view, setImage: image];
    release_obj(image);
    let _: () = msg_send![view, setImageScaling: 3isize]; // NSImageScaleProportionallyUpOrDown
    let _: () = msg_send![parent, addSubview: view];
    release_obj(view);
    canvas.place_pending_view(view, frame);
    true
}

/// Draw a row's in-card divider and hand it to the canvas, for either kind of row.
///
/// A described row and a page-drawn row are both rows of the card, so both take the divider the
/// layout decided for them (`RowSpec::separator_above`, `PageLayout::drawn_separators`); a page
/// that draws its own rows must not have to remember to draw the seam the canvas would otherwise
/// put around it.
pub(super) unsafe fn place_row_separator(canvas: &mut PageCanvas, band: &BoxRect) {
    let separator = SettingsRow::separator_above_row(canvas.document(), band.y, band.h, band.w);
    canvas.place_pending_view(separator, separator_frame_of(band));
}

/// Whether the CARD's divider is drawn above a page-drawn row.
///
/// One decision, used by the renderer and asserted by the test: the layout says "this row continues
/// its card", and the page may overrule it either by drawing that line itself
/// (`PageRowSpec::divides_with_own_line`, the About Updates band, which the update flow collapses)
/// or by having no line there at all (`PageRowSpec::no_divider`, the update host under it).
pub(super) fn card_divider_above(extra: &PageRowSpec, layout: &PageLayout, index: usize) -> bool {
    !extra.divides_with_own_line
        && !extra.no_divider
        && layout.drawn_separators.get(index).copied().unwrap_or(false)
}

/// Draw the divider for a row whose band is known only by its bottom edge and height.
unsafe fn place_row_separator_at(canvas: &mut PageCanvas, line: NSRect) {
    let band = BoxRect::new(line.origin.x, line.origin.y - 1.0, line.size.width, 1.0);
    let separator = SettingsRow::separator_above_row(canvas.document(), band.y, band.h, band.w);
    canvas.place_pending_view(separator, line);
}

/// The line a row's divider draws: the row's top edge, full bleed across the page.
pub(super) unsafe fn separator_frame_of(band: &BoxRect) -> NSRect {
    NSRect::new(
        NSPoint::new(band.x, band.y + band.h),
        NSSize::new(band.w.max(1.0), 1.0),
    )
}

/// Put a view in the box the layout computed.
unsafe fn set_frame(view: *mut AnyObject, box_: &BoxRect) {
    if view.is_null() || box_.is_empty() {
        return;
    }
    let _: () = msg_send![view, setFrame: box_.rect()];
}
/// The conditional groups a description names, in first-appearance order.
pub(super) fn description_groups(view: &PageView) -> Vec<RowGroup> {
    let mut groups: Vec<RowGroup> = Vec::new();
    for row in &view.rows {
        if let Some(group) = row.group {
            if !groups.contains(&group) {
                groups.push(group);
            }
        }
    }
    groups
}

/// Build the page's message: the banner, the cards and the rows, in the frames `layout_rows`
/// computed. Applies the layout only; it derives no frame of its own.
///
/// The page's header block is the canvas's: the layout was measured against the canvas's own
/// `header_offset`, so the first section cursor here is the one the canvas will step from.
#[allow(clippy::too_many_arguments)] // the page build passes one of each: canvas, target, view, layout, metrics, input, ui, hooks
pub(in crate::settings) unsafe fn render_page(
    canvas: &mut PageCanvas,
    target: *mut AnyObject,
    view: &PageView,
    layout: &PageLayout,
    _metrics: &PageMetrics,
    ui: &mut SettingsUi,
    hooks: &mut dyn PageHooks,
) -> Result<RenderedPage, RegisterError> {
    let mut rendered = RenderedPage::default();
    // A conditional row is hidden through the canvas's own group list: the page has no other
    // switch for it today (the generated rows for every group are present in the tree from the
    // start, and hiding is what makes a group's rows disappear together).
    if let (Some(banner), Some(spec)) = (&layout.banner, &view.banner) {
        let strip: *mut AnyObject = msg_send![class!(NSView), alloc];
        let strip: *mut AnyObject = msg_send![strip, initWithFrame: banner.strip.rect()];
        let _: () = msg_send![strip, setAutoresizingMask: 10u64];
        let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let label: *mut AnyObject = msg_send![label, initWithFrame: banner.label.rect()];
        let text = make_nsstring(&t(spec.copy_key));
        let _: () = msg_send![label, setStringValue: text];
        CFRelease(text as *const c_void);
        let _: () = msg_send![label, setEditable: false];
        let _: () = msg_send![label, setBezeled: false];
        let _: () = msg_send![label, setDrawsBackground: false];
        let _: () = msg_send![label, setUsesSingleLineMode: false];
        let _: () = msg_send![label, setLineBreakMode: 0isize]; // NSLineBreakByWordWrapping
        let red: *mut AnyObject = msg_send![class!(NSColor), systemRedColor];
        let _: () = msg_send![label, setTextColor: red];
        let _: () = msg_send![label, setAutoresizingMask: 2u64];
        let _: () = msg_send![strip, addSubview: label];
        // The strip owns the label now; the +1 from alloc/init is given up, exactly as the inline
        // builder did. Without this every settings rebuild leaks one label.
        release_obj(label);
        let button = SettingsButton::action(
            banner.button.rect(),
            &t("settings.btn_open_privacy"),
            target,
            sel!(handleOpenPrivacy:),
            SettingsButtonRole::Action,
        );
        let _: () = msg_send![strip, addSubview: button];
        // The strip and the button are created +1; the page keeps them, so their references are
        // given up here exactly like every inline builder did.
        release_obj(button);
        let _: () = msg_send![strip, setHidden: true];
        let _: () = msg_send![canvas.document(), addSubview: strip];
        // Ownership of the strip's +1 stays with the page build: `finalize_settings_pages`
        // re-parents it to the content view and gives that reference up there (the convention the
        // inline builder had). Releasing it here as well would steal the parent's reference.
        rendered.banner_strip = strip;
        rendered.banner_label = label;
        rendered.banner_button = button;
    }
    for card in layout.cards.iter() {
        // The card's OWN index addresses its rows and its page-drawn slots; a page may have a card
        // that holds no described row, so a positional index would address the wrong card.
        let card_index = card.index;
        // Every card after the first opens its own section: the canvas steps one section per
        // card, which is what the layout's card tops were computed from.
        if card_index > 0 {
            canvas.next_section();
        }
        // The card's rows in document order, described or page-drawn, exactly as the layout walked
        // them. A page-drawn row is drawn at the origin and height the LAYOUT reserved for it, so
        // the controls inside it cannot disagree with the space above them.
        for placed in layout.sequence.iter().copied() {
            match placed {
                PlacedRow::Described(index) => {
                    if view.rows[index].card != card_index {
                        continue;
                    }
                    render_row(
                        canvas,
                        target,
                        &view.rows[index],
                        &layout.rows[index],
                        &mut rendered,
                    )?;
                }
                PlacedRow::Drawn(index) => {
                    let extra = &view.extra_rows[index];
                    if extra.card != card_index {
                        continue;
                    }
                    let (_, origin_y, height) = layout.drawn_rows[index];
                    // The row is OPENED on the canvas before the hook draws into it: the hook's
                    // views belong to this row, and a row the canvas never opened is a row the
                    // page cannot re-flow, resize or scroll to.
                    let drawn_origin = canvas.next_row(height);
                    // The row joins its conditional group BEFORE the hook adds views, so those
                    // views are captured by the row and hidden with the group -- the same
                    // `set_group_visible` a mode switch already drives.
                    canvas.set_pending_group(extra.group);
                    // The seam above this row is the CARD's, not this page's. It is drawn here,
                    // into the row the canvas has just opened, so it is captured with that row and
                    // a re-flow (or the row's group being hidden) moves it like any other row's
                    // divider. A page-drawn row keeps the RELATIVE advance the canvas gave it: the
                    // page's hook owns its views, and unregistered ones only move relatively --
                    // turning the row absolute here would leave About's check-for-updates button
                    // and update host, and the Key Bindings list block, behind on every re-flow.
                    //
                    // A row whose hook draws the line over this boundary itself opts out, or the
                    // boundary would carry two: the About page's Updates band does, anchored to the
                    // row above because the update flow collapses the row it sits in. The update
                    // host under it opts out the other way --- it is part of that block and draws
                    // no line at all.
                    if card_divider_above(extra, layout, index) {
                        place_row_separator_at(
                            canvas,
                            NSRect::new(
                                NSPoint::new(0.0, origin_y + height),
                                NSSize::new(canvas.content_width(), 1.0),
                            ),
                        );
                    }
                    debug_assert!(
                        (drawn_origin - origin_y).abs() < 0.5,
                        "the canvas origin must be the one the layout reserved"
                    );
                    hooks.extra_row(
                        canvas,
                        &mut rendered,
                        ui,
                        extra,
                        drawn_origin,
                        height,
                        target,
                    )?;
                }
            }
        }
        let built = canvas.card(&t(card.section));
        if card.holds_update_flow {
            rendered.update_card = built.card;
        }
    }
    for group in description_groups(view) {
        canvas.set_group_visible(group, false);
    }
    Ok(rendered)
}

/// Write one control's value from the update description's entry.
///
/// The value comes from the description, never from a second expression here.
pub(super) unsafe fn apply_control_update(control: &RenderedControl, value: &Value, display: &str) {
    if control.control.is_null() {
        return;
    }
    match &control.kind {
        ControlKind::Switch => {
            widgets::set_switch_state_immediate(
                control.control,
                matches!(value, Value::Switch(true)),
            );
        }
        ControlKind::Popup { .. } => {
            // The INDEX the description stated, not a number parsed back out of its text.
            let index = match value {
                Value::Index(index) => *index as isize,
                _ => 0,
            };
            let _: () = msg_send![control.control, selectItemAtIndex: index];
        }
        // A hyperlink has no value to write: its text is its own copy.
        ControlKind::Link { .. } => {}
        ControlKind::Slider { .. } => {
            // The STOP the description stated, not a number parsed back out of its text.
            let number = match value {
                Value::Slider(stop) => *stop,
                _ => 0,
            };
            let _: () = msg_send![control.control, setIntegerValue: number];
            if !control.readout.is_null() {
                set_field(control.readout, display);
            }
        }
        // A read-only value shows the update's display text, which is what a status callback
        // computed (never a second formatter here).
        ControlKind::Value { .. } => set_field(control.control, display),
        ControlKind::Button { .. } => {}
    }
}

/// Apply the page's values to the controls the page built: the refill and the in-place refresh
/// both end here, so a field's shown value has one source.
pub(super) unsafe fn apply_page_updates(rendered: &RenderedPage, updates: &[ControlUpdate]) {
    for update in updates {
        if let Some(control) = rendered.control(&ControlId::Field(update.field)) {
            apply_control_update(control, &update.typed, &update.display);
        }
    }
}
