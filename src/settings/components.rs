//! Semantic components for the Settings window: pages, cards, rows, and layout metrics.
//!
//! These components intentionally remain thin wrappers around the existing AppKit builders.
//! Keeping ownership of raw Objective-C pointers in `settings.rs` avoids changing callback and
//! configuration lifetimes while giving every page one place for shared geometry rules.

use objc2::runtime::{AnyObject, Sel};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::sync::{LazyLock, Mutex};

use crate::ffi::release_obj;
use crate::i18n::{t, tf};

use super::{
    settings_page_identity, settings_page_ids, tooltip::SettingsTooltip, widgets, SettingsPageId,
    SETTINGS_PAGE_COUNT,
};

/// Shared dimensions for in-row action buttons.
pub(crate) const ROW_ACTION_BTN_W: f64 = 110.0;
pub(crate) const ROW_ACTION_BTN_H: f64 = 32.0;

/// Build an in-row action button aligned to the control column's right edge.
pub(crate) unsafe fn row_action_button(
    ctrl_x: f64,
    ctrl_w: f64,
    row_y: f64,
    title: &str,
    target: *mut AnyObject,
    action: Sel,
) -> *mut AnyObject {
    SettingsControl::button(
        ctrl_x + ctrl_w - ROW_ACTION_BTN_W,
        row_y,
        ROW_ACTION_BTN_W,
        ROW_ACTION_BTN_H,
        title,
        target,
        action,
        SettingsButtonRole::Action,
    )
}

/// Build the shared custom switch for the onboarding flow.
pub(crate) unsafe fn onboarding_switch(
    right_x: f64,
    y: f64,
    h: f64,
    checked: bool,
) -> *mut AnyObject {
    widgets::make_switch(right_x, y, h, checked)
}

/// Right-hand read-only readout of a slider row: its width, the gap before it, and its own
/// height. The readout hugs the slider's right end and is vertically centred on it, so a slider
/// in such a row takes the control column's width minus the first two.
const SLIDER_READOUT_W: f64 = 44.0;
const SLIDER_READOUT_GAP: f64 = 8.0;
const SLIDER_READOUT_H: f64 = 20.0;
// The macOS SDK uses iOS alignment enum values on arm64 and legacy AppKit values on x86_64.
const TEXT_ALIGNMENT_RIGHT: isize = if cfg!(target_arch = "aarch64") { 2 } else { 1 };
const RESTORE_SHELL_INSET: f64 = 8.0;
const RESTORE_TRIGGER_INSET: f64 = 8.0;
const RESTORE_CONTAINER_ORIGIN: f64 = 16.0;
const RESTORE_SIDEBAR_OUTER_INSET: f64 = RESTORE_CONTAINER_ORIGIN + RESTORE_TRIGGER_INSET;
const RESTORE_ACTION_ROW_GAP: f64 = 16.0;
const SIDEBAR_VERSION_BOTTOM_INSET: f64 = 12.0;
const SIDEBAR_VERSION_HORIZONTAL_INSET: f64 = 8.0;
const SIDEBAR_VERSION_GAP: f64 = 8.0;
const SIDEBAR_VERSION_HEIGHT: f64 = 14.0;

/// Gap between a card's internal divider and the top edge of the row below it
/// (`separator_above_row`). Contiguous rows put the hairline on the shared edge, which is the
/// mockup's `.setting-row + .setting-row { border-top: 1px solid var(--settings-divider) }`.
const SEPARATOR_ABOVE_ROW_GAP: f64 = 0.0;

/// Standard rows keep their label and control as sibling views in the card, so retain the
/// association here instead of forcing every SettingsUi field to grow a second label pointer.
static ROW_LABELS: LazyLock<Mutex<Vec<(usize, usize)>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// Shared horizontal and vertical metrics for all settings pages.
#[derive(Clone, Copy, Debug)]
pub(super) struct SettingsLayout {
    pub label_x: f64,
    pub label_w: f64,
    pub control_x: f64,
    pub control_w: f64,
    pub row_h: f64,
    pub described_row_h: f64,
    /// Distance from one section cursor to the next section header cursor.
    pub section_step: f64,
    /// Vertical gap between rows inside a grouped card.
    pub row_gap: f64,
    /// Legacy page frame inset below the last row; normalized by SettingsSection.
    pub card_bottom_inset: f64,
}

impl SettingsLayout {
    /// Standard visual height of one settings row; controls remain shorter and center inside it.
    ///
    /// The mockup's `.setting-row { min-height: 56px; padding: 12px 16px }` is a 32pt control plus
    /// 12pt of padding above and below.
    pub(super) const SINGLE_LINE_ROW_H: f64 = 56.0;
    pub(super) const CONTROL_H: f64 = 32.0;
    /// Distance from a card's last row to its own bottom edge. The mock's `.settings-group` has
    /// no vertical padding: the card is exactly its rows' bounding box (its height is the rows
    /// plus the 1px border on each edge). The symbol stays so a restored inset lands in every
    /// reader at once.
    pub(super) const CARD_BOTTOM_INSET: f64 = 0.0;

    pub(super) fn new(content_w: f64) -> Self {
        // `.select-wrap { width: 180px }` in the mock.
        let control_w = 180.0;
        Self {
            label_x: 16.0,
            label_w: 220.0,
            control_x: content_w - control_w - super::SETTINGS_CONTROL_TRAILING_INSET,
            control_w,
            row_h: Self::CONTROL_H,
            // Detailed subtitles are no longer rendered; described rows use the same compact
            // height as every other single-line row.
            described_row_h: Self::SINGLE_LINE_ROW_H,
            // The card's top edge sits CARD_BOTTOM_INSET above the first row (it used to tuck 4pt
            // under the row top), so the step grows by that delta to keep the same visible gap
            // between a card's bottom and the next section's heading. The heading term is its box
            // height, so the step follows `add_header` instead of a second number.
            section_step: super::SETTINGS_SECTION_HEADER_GAP
                + widgets::SECTION_HEADER_H
                + Self::CARD_BOTTOM_INSET
                + super::SETTINGS_SECTION_CARD_GAP,
            // The mockup's rows are contiguous inside a group; a `border-top` divides them rather
            // than a gap (`.setting-row + .setting-row`).
            row_gap: 0.0,
            card_bottom_inset: Self::CARD_BOTTOM_INSET,
        }
    }

    /// The card's top edge: the first row's top plus the same inset the bottom edge keeps, so a
    /// row band sits centered inside the card instead of reading high (top inset used to be
    /// `-card_header_gap`, i.e. the first row poked 4pt out of the card).
    pub(super) fn card_top(self, header_y: f64) -> f64 {
        header_y + Self::CARD_BOTTOM_INSET
    }
}

/// Ownership-neutral page parts returned by the AppKit page builder.
#[derive(Clone, Copy)]
pub(super) struct SettingsPage {
    pub(super) scroll: *mut AnyObject,
    pub(super) document: *mut AnyObject,
}

unsafe impl Send for SettingsPage {}
unsafe impl Sync for SettingsPage {}

impl SettingsPage {
    pub(super) unsafe fn new(
        parent: *mut AnyObject,
        frame: NSRect,
        document_h: f64,
        hidden: bool,
    ) -> Self {
        let (scroll, document) = widgets::make_settings_page(parent, frame, document_h, hidden);
        Self { scroll, document }
    }

    pub(super) unsafe fn scroll_to_top(self) {
        widgets::scroll_page_to_top(self.scroll);
    }

    pub(super) unsafe fn validate(self, name: &str) {
        let actual: *mut AnyObject = objc2::msg_send![self.scroll, documentView];
        debug_assert_eq!(actual, self.document);
        widgets::debug_validate_settings_page(self.scroll, name);
    }
}

/// Page header component: the design system's page title (`26px`) plus its
/// distance from the pane top (the mockup's `.page-header { padding: var(--sp-6) var(--sp-7) var(--sp-4) }`
/// below a 36px titlebar), plus the gap down to the first section heading. Owns the whole page-top
/// block so pages neither hardcode its metrics nor drift apart: one call returns the cursor that
/// first heading hangs from.
pub(super) struct SettingsPageHeader;

impl SettingsPageHeader {
    /// Distance from the pane top to the title's frame. The pane top is the bottom edge of the
    /// 36pt title bar band (the window's layout starts under the band, exactly like the mock's
    /// `.settings-layout`); the mock's `.page-header` uses 24pt of top padding. Keep this metric
    /// here so every settings page moves together when the page header spacing changes.
    pub(super) const TOP_PADDING: f64 = 24.0;

    /// Space the page keeps below its lowest content.
    ///
    /// The mockup puts the restore control in a footer with `padding: 0 var(--sp-7) var(--sp-7)`
    /// (28px below it). The settings pages embed that control in the scrolling document, whose
    /// shell already leaves an 8pt inset below the button, so the document's own bottom padding is
    /// that 28px minus the inset. Overshooting it is the dead scroll space users see as "the page
    /// scrolls far past its content".
    pub(super) const BOTTOM_PADDING: f64 = 20.0;

    /// Empty space between the title's frame and the first section heading's box below it.
    ///
    /// Paired with `TOP_PADDING` so their sum stays constant: the title's frame is taller than its
    /// ink, so this value is really "what is left of the title-to-card rhythm after the frame".
    const FIRST_SECTION_GAP: f64 = 8.0;

    /// Build the page title and return its document top plus the first section cursor below it.
    ///
    /// The cursor is that heading's frame BOTTOM: `widgets::add_header` grows its label upwards
    /// from there, and `SettingsSection::attach` puts the card top `SETTINGS_SECTION_CARD_GAP`
    /// below it. Callers hand it to their first `SettingsSection::attach` and advance from it with
    /// `SettingsLayout::next_row_cursor`, exactly like every later section -- which is what keeps
    /// the first card's row on the same 4pt inset as all the others.
    pub(super) unsafe fn attach(
        parent: *mut AnyObject,
        title: &str,
        x: f64,
        doc_top: f64,
        w: f64,
    ) -> (f64, f64) {
        // make_settings_page may grow the document to the viewport height, so the caller's
        // provisional height is no longer the document's top edge. Anchor the title to the
        // actual frame height and fold the delta into the returned consumption so following
        // sections continue calculating from that same real top edge.
        let frame: NSRect = objc2::msg_send![parent, frame];
        let actual_doc_top = if frame.size.height.is_finite() && frame.size.height > 0.0 {
            frame.size.height.max(doc_top)
        } else {
            doc_top
        };
        // widgets::add_page_title places the title's top edge at cursor + 10.
        let title_h = widgets::add_page_title(
            parent,
            title,
            x,
            actual_doc_top - Self::TOP_PADDING - 10.0,
            w,
        );
        // Title frame bottom, one FIRST_SECTION_GAP further down, then the heading label's own
        // height up to its bottom edge -- the cursor callers lay the page out from.
        (
            actual_doc_top,
            actual_doc_top
                - Self::TOP_PADDING
                - title_h
                - Self::FIRST_SECTION_GAP
                - widgets::SECTION_HEADER_H,
        )
    }
}

/// Card component. Rows remain siblings of the card background so native controls keep their
/// normal hit-testing and z-order; the component owns only the card surface.
#[derive(Clone, Copy)]
pub(super) struct SettingsCard {
    pub(super) card: *mut AnyObject,
}

unsafe impl Send for SettingsCard {}
unsafe impl Sync for SettingsCard {}

impl SettingsCard {
    pub(super) unsafe fn attach(parent: *mut AnyObject, frame: NSRect) -> Self {
        let card = widgets::add_settings_card(parent, frame);
        Self { card }
    }
}

/// A titled settings section: the small explanatory heading and its rounded card are one unit.
pub(super) struct SettingsSection;

impl SettingsSection {
    pub(super) unsafe fn attach(
        parent: *mut AnyObject,
        frame: NSRect,
        title: &str,
    ) -> SettingsCard {
        let header_y = frame.origin.y + frame.size.height + super::SETTINGS_SECTION_CARD_GAP;
        widgets::add_header(parent, title, 0.0, header_y, frame.size.width);
        SettingsCard::attach(parent, widgets::settings_card_rect(frame))
    }
}

/// Semantic row entry points. Every row centers its leading text and trailing control internally;
/// described rows retain their legacy subtitle parameter only for call-site compatibility.
pub(super) struct SettingsRow;

impl SettingsRow {
    unsafe fn register_label(label: *mut AnyObject, control: *mut AnyObject) {
        if label.is_null() || control.is_null() {
            return;
        }
        // VoiceOver: expose the row label as the control's accessibility label (design-style §11).
        // AppKit does not associate a sibling NSTextField with a control on its own.
        let text: *mut AnyObject = objc2::msg_send![label, stringValue];
        if !text.is_null() {
            let _: () = objc2::msg_send![control, setAccessibilityLabel: text];
        }
        let mut labels = ROW_LABELS.lock().unwrap();
        let control = control as usize;
        labels.retain(|(registered_control, _)| *registered_control != control);
        labels.push((control, label as usize));
    }

    pub(super) fn label_for(control: *mut AnyObject) -> Option<*mut AnyObject> {
        ROW_LABELS
            .lock()
            .unwrap()
            .iter()
            .find(|(registered_control, _)| *registered_control == control as usize)
            .map(|(_, label)| *label as *mut AnyObject)
    }

    /// Drop row label associations before the settings views are deallocated.
    pub(super) fn clear_runtime_registry() {
        ROW_LABELS.lock().unwrap().clear();
    }

    /// Apply enabled state, disabled appearance, cursor, and optional tooltip to one view.
    pub(super) unsafe fn set_view_enabled_with_tooltip(
        view: *mut AnyObject,
        enabled: bool,
        tooltip: Option<&str>,
    ) {
        if view.is_null() {
            return;
        }
        if objc2::msg_send![view, respondsToSelector: objc2::sel!(setEnabled:)] {
            let _: () = objc2::msg_send![view, setEnabled: enabled];
        }

        let is_text_field: bool = objc2::msg_send![view, isKindOfClass: objc2::class!(NSTextField)];
        if is_text_field {
            let role = if enabled {
                widgets::SettingsTextRole::Primary
            } else {
                widgets::SettingsTextRole::Disabled
            };
            widgets::apply_settings_text_role(view, role);
        } else if !objc2::msg_send![view, respondsToSelector: objc2::sel!(setEnabled:)]
            && objc2::msg_send![view, respondsToSelector: objc2::sel!(setAlphaValue:)]
        {
            // NSImageView and other decorative views have no enabled property; dim them through
            // alpha so custom rows still communicate that they are unavailable.
            let _: () = objc2::msg_send![view, setAlphaValue: if enabled { 1.0 } else { 0.45 }];
        }
        SettingsTooltip::apply(view, enabled, (!enabled).then_some(tooltip).flatten());
    }

    /// Unregister a view that is about to be removed from the hierarchy and released.
    /// Call this before `removeFromSuperview`, never after: the registry lookups are keyed by
    /// the view's address, so a stale entry turns the next settings click into a message to
    /// freed memory.
    pub(super) unsafe fn forget(view: *mut AnyObject) {
        SettingsTooltip::forget(view);
    }

    /// Enable/disable a row and show a native AppKit bubble while it is unavailable.
    pub(super) unsafe fn set_enabled_with_tooltip(
        control: *mut AnyObject,
        enabled: bool,
        tooltip: &str,
    ) {
        Self::set_view_enabled_with_tooltip(control, enabled, Some(tooltip));
        if control.is_null() {
            return;
        }
        let label = Self::label_for(control);
        if let Some(label) = label {
            Self::set_view_enabled_with_tooltip(label, enabled, Some(tooltip));
        }
    }

    /// Enable/disable a standard row as one semantic component, including its leading label.
    pub(super) unsafe fn set_enabled(control: *mut AnyObject, enabled: bool) {
        Self::set_enabled_with_tooltip(control, enabled, "");
    }

    /// Add a card divider at an absolute y (the low-level primitive).
    ///
    /// Most cards want `separator_above_row` instead, which owns the row-relative arithmetic. Use
    /// this directly only when the y is not derived from a row's position (e.g. the General page's
    /// contiguous rows sharing an edge, or the About page's runtime-toggled divider).
    pub(super) unsafe fn separator(parent: *mut AnyObject, y: f64, width: f64) -> *mut AnyObject {
        widgets::add_row_separator(parent, 0.0, y, width)
    }

    /// Draw a grouped card's internal divider just above a row.
    ///
    /// `row_y`/`row_h` are the position and height of the row BELOW the divider -- normally the
    /// row built right after this call. Passing the row above instead is the easy mistake to
    /// make, and it fails silently: the line simply lands at the top of the card.
    pub(super) unsafe fn separator_above_row(
        parent: *mut AnyObject,
        row_y: f64,
        row_h: f64,
        width: f64,
    ) -> *mut AnyObject {
        Self::separator(parent, row_y + row_h + SEPARATOR_ABOVE_ROW_GAP, width)
    }

    /// Width to give a slider that sits in a row with a right-hand readout.
    ///
    /// Pair it with `attach_slider_readout`; the two share `SLIDER_READOUT_*` so the pair can
    /// never drift apart.
    pub(super) fn slider_width(control_w: f64) -> f64 {
        control_w - SLIDER_READOUT_W - SLIDER_READOUT_GAP
    }

    /// Attach the right-hand read-only readout of a slider row and return it (for refreshes and
    /// conditional visibility).
    ///
    /// The whole position comes from the slider's own frame -- one gap past its right end,
    /// vertically centred on it -- so callers compute no coordinates and can never drift from
    /// where the row builder actually put the slider (row builders re-centre the control).
    /// The label itself comes from `widgets::make_value_label`, keeping font, text role, and
    /// truncation shared.
    pub(super) unsafe fn attach_slider_readout(
        parent: *mut AnyObject,
        slider: *mut AnyObject,
        value: impl std::fmt::Display,
    ) -> *mut AnyObject {
        let frame: NSRect = objc2::msg_send![slider, frame];
        let x = frame.origin.x + frame.size.width + SLIDER_READOUT_GAP;
        let y = frame.origin.y + (frame.size.height - SLIDER_READOUT_H) / 2.0;
        let label = widgets::make_value_label(
            x,
            y,
            SLIDER_READOUT_W,
            SLIDER_READOUT_H,
            &format!("{value}"),
        );
        let font: *mut AnyObject = objc2::msg_send![
            objc2::class!(NSFont),
            monospacedDigitSystemFontOfSize: crate::theme::FONT_CONTROL,
            weight: crate::theme::FONT_WEIGHT_REGULAR
        ];
        let _: () = objc2::msg_send![label, setFont: font];
        let _: () = objc2::msg_send![label, setAlignment: TEXT_ALIGNMENT_RIGHT];
        let _: () = objc2::msg_send![parent, addSubview: label];
        release_obj(label);
        label
    }

    fn control_origin_y(y: f64, row_h: f64, control_h: f64) -> f64 {
        y + (row_h - control_h).max(0.0) / 2.0
    }

    /// Center a native control by its view frame.
    unsafe fn center_control(child: *mut AnyObject, y: f64, row_h: f64) {
        if child.is_null() {
            return;
        }
        let mut frame: NSRect = objc2::msg_send![child, frame];
        frame.origin.y = Self::control_origin_y(y, row_h, frame.size.height);
        let _: () = objc2::msg_send![child, setFrame: frame];
    }

    /// NSTextField's glyphs are top-biased when its frame is taller than the measured cell.
    /// Fit the frame to the cell's measured height before centering it, so the glyph baseline
    /// shares the same center line as the trailing control.
    ///
    /// A forced line height (`ffi::set_text_field_line_height`) adds its extra leading above the
    /// glyphs, so inside a centred line box the ink still sits low; the frame is shifted by exactly
    /// that leading-derived offset, signed for the superview's axis (see
    /// `ffi::text_field_line_height_correction`). A field whose line height was never forced
    /// reports 0 and is centred by its box alone, unchanged.
    unsafe fn center_label(child: *mut AnyObject, y: f64, row_h: f64) {
        if child.is_null() {
            return;
        }
        let cell: *mut AnyObject = objc2::msg_send![child, cell];
        if cell.is_null() {
            return;
        }
        // Measure against the full row height; a previous centering pass may have already
        // shrunk the label frame to one line, which would otherwise hide later wrapped text.
        let mut bounds: NSRect = objc2::msg_send![child, bounds];
        bounds.size.height = row_h.max(1.0);
        let measured: objc2_foundation::NSSize = objc2::msg_send![cell, cellSizeForBounds: bounds];
        if !measured.height.is_finite() || measured.height <= 0.0 {
            return;
        }
        let mut frame: NSRect = objc2::msg_send![child, frame];
        frame.origin.y = y + (row_h - measured.height).max(0.0) / 2.0;
        frame.size.height = measured.height;
        // A positive correction moves the ink toward the row's centre line, whichever direction
        // the superview's y axis points.
        frame.origin.y += crate::ffi::text_field_line_height_correction(child);
        let _: () = objc2::msg_send![child, setFrame: frame];
    }

    /// Center a read-only text control without changing editable field geometry.
    unsafe fn center_readonly_text_control(child: *mut AnyObject, y: f64, row_h: f64) {
        if child.is_null() {
            return;
        }
        let is_text_field: bool =
            objc2::msg_send![child, isKindOfClass: objc2::class!(NSTextField)];
        if !is_text_field {
            return;
        }
        let editable: bool = objc2::msg_send![child, isEditable];
        if !editable {
            Self::center_label(child, y, row_h);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn described(
        parent: *mut AnyObject,
        x: f64,
        y: f64,
        text_w: f64,
        row_h: f64,
        title: &str,
        subtitle: &str,
        control: *mut AnyObject,
    ) -> *mut AnyObject {
        let (label, control) =
            widgets::add_described_row(parent, x, y, text_w, row_h, title, subtitle, control);
        Self::center_label(label, y, row_h);
        Self::center_control(control, y, row_h);
        Self::register_label(label, control);
        control
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn captioned(
        parent: *mut AnyObject,
        x: f64,
        y: f64,
        text_w: f64,
        row_h: f64,
        title: &str,
        caption: &str,
        control: *mut AnyObject,
    ) -> (*mut AnyObject, *mut AnyObject, *mut AnyObject) {
        let (title_label, caption_label, control) =
            widgets::add_captioned_row(parent, x, y, text_w, row_h, title, caption, control);
        Self::center_control(control, y, row_h);
        Self::register_label(title_label, control);
        (title_label, caption_label, control)
    }

    pub(super) unsafe fn tall(
        parent: *mut AnyObject,
        label_x: f64,
        y: f64,
        label_w: f64,
        label_text: &str,
        control: *mut AnyObject,
    ) -> (*mut AnyObject, *mut AnyObject) {
        Self::tall_with_height(
            parent,
            label_x,
            y,
            label_w,
            SettingsLayout::SINGLE_LINE_ROW_H,
            label_text,
            control,
        )
    }

    /// Use the space before a control column for the label instead of imposing a fixed width.
    pub(super) unsafe fn tall_before_control(
        parent: *mut AnyObject,
        label_x: f64,
        y: f64,
        control_left_x: f64,
        gap: f64,
        label_text: &str,
        control: *mut AnyObject,
    ) -> (*mut AnyObject, *mut AnyObject) {
        let label_w = Self::label_width_before_control(label_x, control_left_x, gap);
        Self::tall(parent, label_x, y, label_w, label_text, control)
    }

    fn label_width_before_control(label_x: f64, control_left_x: f64, gap: f64) -> f64 {
        (control_left_x - label_x - gap).max(1.0)
    }

    pub(super) unsafe fn tall_with_height(
        parent: *mut AnyObject,
        label_x: f64,
        y: f64,
        label_w: f64,
        row_h: f64,
        label_text: &str,
        control: *mut AnyObject,
    ) -> (*mut AnyObject, *mut AnyObject) {
        // A row must leave its control vertical breathing room. Passing the control's own height
        // as the row height centers it into nothing -- the control ends up flush against both
        // dividers -- which is how the keystroke display's slider row shipped 20pt shorter than
        // its siblings. Fail the debug-build layout smoke instead of shipping it again.
        if !control.is_null() {
            let control_frame: NSRect = objc2::msg_send![control, frame];
            debug_assert!(
                control_frame.size.height <= 0.0 || row_h > control_frame.size.height,
                "tall_with_height: a {row_h}pt row gives no padding to a {}pt control",
                control_frame.size.height
            );
        }
        let (label, control) =
            widgets::add_tall_row(parent, label_x, y, label_w, row_h, label_text, control);
        Self::center_label(label, y, row_h);
        Self::center_control(control, y, row_h);
        Self::register_label(label, control);
        (label, control)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn plain(
        parent: *mut AnyObject,
        label_x: f64,
        y: f64,
        label_w: f64,
        h: f64,
        label_text: &str,
        control: *mut AnyObject,
    ) -> *mut AnyObject {
        let (label, control) =
            widgets::add_row_with_label(parent, label_x, y, label_w, h, label_text, control);
        Self::center_label(label, y, h);
        Self::center_readonly_text_control(control, y, h);
        Self::center_control(control, y, h);
        Self::register_label(label, control);
        control
    }

    /// One shortcut-legend cell: a symbol on a soft wash, vertically centred in the row.
    ///
    /// It is a *legend*, not a control (style doc §6.1): no border, no pointer state, no hit target.
    /// The chip radius comes from the legend scale, not `radius-control`, because a control-sized
    /// radius reads as a pill on a small chip. The caller positions the run horizontally; the
    /// vertical centering is derived here from the shared row height so a chip lines up with the
    /// label and the trailing control without the caller doing arithmetic.
    ///
    /// `size` is the chip's **minimum** square. A label longer than one glyph ("Page Down") widens
    /// its chip to fit rather than being drawn a character per chip, which is what used to let a
    /// legal chord's legend run into the row's buttons. The height stays `size`.
    pub(super) unsafe fn legend_chip(
        parent: *mut AnyObject,
        x: f64,
        row_y: f64,
        row_h: f64,
        size: f64,
        symbol: &str,
    ) -> *mut AnyObject {
        let measured = widgets::measure_control_text_width(symbol);
        // The widest a single chip may grow: past this, the chip has eaten the whole keycap column
        // and the row would overlap its buttons regardless of how the run is positioned.
        let chip_w =
            (measured.ceil() + crate::theme::LEGEND_CHIP_TEXT_PAD_X * 2.0).clamp(size, size * 8.0);
        let chip: *mut AnyObject = objc2::msg_send![objc2::class!(NSTextField), alloc];
        let chip: *mut AnyObject = objc2::msg_send![chip, initWithFrame: NSRect::new(
            NSPoint::new(x, row_y + (row_h - size) / 2.0),
            NSSize::new(chip_w, size)
        )];
        widgets::set_field(chip, symbol);
        let _: () = objc2::msg_send![chip, setBezeled: false];
        let _: () = objc2::msg_send![chip, setDrawsBackground: false];
        let _: () = objc2::msg_send![chip, setEditable: false];
        let _: () = objc2::msg_send![chip, setSelectable: false];
        let _: () = objc2::msg_send![chip, setAlignment: 1isize]; // NSTextAlignmentCenter
        let font: *mut AnyObject = objc2::msg_send![
            objc2::class!(NSFont),
            systemFontOfSize: crate::theme::FONT_CONTROL
        ];
        let _: () = objc2::msg_send![chip, setFont: font];
        widgets::apply_settings_text_role(chip, widgets::SettingsTextRole::Primary);
        let _: () = objc2::msg_send![chip, setWantsLayer: true];
        let layer: *mut AnyObject = objc2::msg_send![chip, layer];
        if !layer.is_null() {
            let base: u32 = if crate::theme::ui_palette().dark {
                0xFFFF_FFFF
            } else {
                0x0000_0000
            };
            crate::ffi::layer_set_background(
                layer,
                crate::ffi::hex_to_cg_color((base & 0xFFFF_FF00) | crate::theme::LEGEND_CHIP_ALPHA),
            );
            // No border: a border is the visual contract of a pressable control (§6.1).
            let _: () = objc2::msg_send![layer, setBorderWidth: 0.0f64];
            let _: () = objc2::msg_send![layer, setCornerRadius: crate::theme::RADIUS_LEGEND_CHIP];
            let _: () = objc2::msg_send![layer, setMasksToBounds: true];
        }
        let _: () = objc2::msg_send![parent, addSubview: chip];
        release_obj(chip);
        chip
    }

    /// A row whose label is preceded by a run of keycap cells (a shortcut legend), with the label
    /// running up to the trailing control.
    ///
    /// Returns the label and the trailing control; the caller owns the keycap views because only it
    /// knows how many cells the chord needs. Everything else — the label build, vertical centering
    /// against the shared row height, the accessibility label and the control registration — is the
    /// shared row behaviour, so a chord row is the same row as any other with a legend in front.
    ///
    /// `label_right` is where the label's box must stop. A row whose only control is the trailing one
    /// can let `add_row_with_label` derive that from the control's leading edge, but a row with
    /// controls on *both* sides of the text — the key bindings rows put Edit and Delete beside the
    /// switch — would otherwise run its label under them, which the layout validator reports as an
    /// overlap. The caller passes the left edge of the first trailing control instead.
    pub(super) unsafe fn with_leading_keycaps(
        parent: *mut AnyObject,
        label_x: f64,
        label_right: f64,
        y: f64,
        h: f64,
        label_text: &str,
        control: *mut AnyObject,
    ) -> (*mut AnyObject, *mut AnyObject) {
        let (label, control) =
            widgets::add_row_with_label(parent, label_x, y, 0.0, h, label_text, control);
        // Clip the label to the bound the caller gave, keeping the height centering below intact.
        if !label.is_null() && label_right > label_x {
            let mut frame: NSRect = objc2::msg_send![label, frame];
            frame.size.width = (label_right - label_x).max(1.0);
            let _: () = objc2::msg_send![label, setFrame: frame];
        }
        Self::center_label(label, y, h);
        Self::center_readonly_text_control(control, y, h);
        Self::center_control(control, y, h);
        Self::register_label(label, control);
        (label, control)
    }
}

/// Native controls shared by settings rows and the window chrome.
pub(super) struct SettingsControl;

/// Animated select component shared by settings rows and auxiliary edit panels.
pub(super) struct SettingsSelect;

#[derive(Clone, Copy, Debug)]
pub(super) struct SettingsSelectMetrics {
    pub(super) control_h: f64,
    pub(super) row_h: f64,
}

impl SettingsSelect {
    /// Keep the trigger and settings row on the fixed grid; option text is sized in the menu.
    pub(super) fn metrics() -> SettingsSelectMetrics {
        SettingsSelectMetrics {
            control_h: SettingsLayout::CONTROL_H,
            row_h: SettingsLayout::SINGLE_LINE_ROW_H,
        }
    }

    pub(super) unsafe fn create(
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        items: &[&str],
        selected: usize,
    ) -> *mut AnyObject {
        widgets::make_popup(x, y, w, h, items, selected)
    }
}

impl SettingsControl {
    pub(super) unsafe fn popup(
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        items: &[&str],
        selected: usize,
    ) -> *mut AnyObject {
        SettingsSelect::create(x, y, w, h, items, selected)
    }

    pub(super) unsafe fn switch(right_x: f64, y: f64, h: f64, checked: bool) -> *mut AnyObject {
        widgets::make_switch(right_x, y, h, checked)
    }

    /// Build a slider that can only rest on whole `step`s of its range (AppKit snapping).
    /// `default_value`: the value a double-click restores (None = double-click untouched).
    #[allow(clippy::too_many_arguments)] // same parameter list as widgets::make_*.
    pub(super) unsafe fn stepped_slider(
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        min: i64,
        max: i64,
        step: i64,
        value: i64,
        default_value: Option<f64>,
    ) -> *mut AnyObject {
        widgets::make_stepped_slider(x, y, w, h, min, max, step, value, default_value)
    }

    /// Build a continuous (fractional) slider.
    /// `default_value`: the value a double-click restores (None = double-click untouched).
    #[allow(clippy::too_many_arguments)] // same parameter list as widgets::make_*.
    pub(super) unsafe fn double_slider(
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        min: f64,
        max: f64,
        value: f64,
        default_value: Option<f64>,
    ) -> *mut AnyObject {
        widgets::make_double_slider(x, y, w, h, min, max, value, default_value)
    }

    pub(super) unsafe fn text_input(x: f64, y: f64, w: f64, h: f64, value: &str) -> *mut AnyObject {
        widgets::make_text_input(x, y, w, h, value)
    }

    /// Build a non-editable value label that can be placed in a `SettingsRow`.
    pub(super) unsafe fn value_label(
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        value: &str,
    ) -> *mut AnyObject {
        widgets::make_value_label(x, y, w, h, value)
    }

    /// Build an external-link value control with the shared link hover/cursor behavior.
    pub(super) unsafe fn external_link(
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        title: &str,
        tag: isize,
    ) -> *mut AnyObject {
        widgets::make_external_link(x, y, w, h, title, tag)
    }

    /// Build an action button usable inside a `SettingsRow` (e.g. "export logs"), so row
    /// actions share the same geometry/centering path as every other trailing control.
    #[allow(clippy::too_many_arguments)] // same parameter list as widgets::make_*.
    pub(super) unsafe fn button(
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        title: &str,
        target: *mut AnyObject,
        action: Sel,
        role: SettingsButtonRole,
    ) -> *mut AnyObject {
        SettingsButton::action(
            NSRect::new(NSPoint::new(x, y), NSSize::new(w, h)),
            title,
            target,
            action,
            role,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn sidebar(
        parent: *mut AnyObject,
        target: *mut AnyObject,
        title: &str,
        symbol: &str,
        tag: isize,
        x: f64,
        y: f64,
        w: f64,
        row_h: f64,
        icon_frame: NSRect,
        label_frame: NSRect,
    ) -> *mut AnyObject {
        widgets::make_sidebar_button(
            parent,
            target,
            title,
            symbol,
            tag,
            x,
            y,
            w,
            row_h,
            icon_frame,
            label_frame,
        )
    }
}

/// Shared action icon component for the action popup and mapping-list rows.
pub(super) struct SettingsMappingActionIcon;

impl SettingsMappingActionIcon {
    /// NSPopUpButton menu cells and standalone image views apply different optical scaling.
    /// `row_size` compensates the latter so both render at the same visible size.
    pub(super) const ROW_SIZE: f64 = 18.0;

    pub(super) fn symbol_name(action_index: usize) -> Option<&'static str> {
        super::MAPPING_ACTION_SYMBOLS.get(action_index).copied()
    }

    pub(super) unsafe fn attach(
        parent: *mut AnyObject,
        action_index: usize,
        frame: NSRect,
    ) -> *mut AnyObject {
        let Some(symbol) = Self::symbol_name(action_index) else {
            return std::ptr::null_mut();
        };
        let icon = widgets::make_symbol_image_view(symbol, frame);
        let _: () = objc2::msg_send![parent, addSubview: icon];
        icon
    }
}

/// Semantic roles for clickable settings buttons. The low-level builder owns AppKit tracking;
/// this role selects the normal surface, text color, and hover behavior without leaking raw color
/// literals into page construction code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SettingsButtonRole {
    Action,
    /// Filled neutral surface used by value controls that also accept clicks, such as a
    /// recorded shortcut. This shares the control-field palette with dropdown triggers.
    ControlField,
    Footer,
    Primary,
    Destructive,
}

impl SettingsButtonRole {
    /// The role for a list row's trailing Edit / Delete buttons.
    ///
    /// It is the design system's **secondary** action surface (`field_bg` + `card_border`), which is
    /// what style doc §6 assigns to a button and what the pages' references draw for these two.
    ///
    /// It exists as its own name because the surface it replaced was wrong in a way the role list did
    /// not show: those buttons used a field-family role whose normal (`field_bg`) and hover
    /// (`hover_bg`) tokens differ by five alpha steps, so over `card_bg` they composited to 1.02:1 --
    /// a hover the user could not see, leaving §10's "every custom-drawn control needs a hover state"
    /// silently unmet. `field_bg` is the documented surface for text fields and read-only value boxes
    /// (§3), not for a button. Naming the role here is what lets the gate assert the resulting pixels
    /// instead of the role's name.
    pub(crate) const ROW_ACTION: Self = Self::Action;

    /// The role's (normal fill, text, palette-slot tag). `widgets` reads the tag back to resolve
    /// hover/exit fills, so the two mappings must stay in one place.
    pub(super) fn style(self, palette: crate::theme::UiPalette) -> (u32, u32, isize) {
        match self {
            // The mock's `.btn` fill is `--field` (a dark wash over the card), not the
            // raised white of `.restore-btn`: on the light card an opaque white fill reads
            // as no fill at all, which is why these buttons looked borderless.
            Self::Action => (palette.field_bg, palette.button_text, -3),
            Self::ControlField => (palette.field_bg, palette.button_text, -6),
            Self::Footer => (palette.footer_button_bg, palette.button_text, -1),
            Self::Primary => (palette.accent, palette.accent_ink, -2),
            Self::Destructive => (palette.destructive, palette.accent_text, -4),
        }
    }
}

/// Shared semantic button component for settings actions. Specialized controls such as toggles,
/// sidebar tabs, clipboard actions, and the overlay close button keep their own interaction model.
pub(crate) struct SettingsButton;

impl SettingsButton {
    pub(crate) unsafe fn action(
        frame: NSRect,
        title: &str,
        target: *mut AnyObject,
        action: Sel,
        role: SettingsButtonRole,
    ) -> *mut AnyObject {
        let (background, text, hover_tag) = role.style(crate::theme::ui_palette());
        widgets::make_settings_styled_button(
            frame, title, target, action, background, text, hover_tag,
        )
    }

    /// Re-apply a role's palette to an existing button. The warning banner's button carries different
    /// warnings in turn (permission guidance, a keychain problem, a destructive storage action), so
    /// its role has to follow the warning it currently carries. The hover palette stays the role's
    /// own tag — never an action id (AGENTS.md).
    pub(super) unsafe fn apply_role(button: *mut AnyObject, role: SettingsButtonRole) {
        if button.is_null() {
            return;
        }
        let (background, text, hover_tag) = role.style(crate::theme::ui_palette());
        widgets::style_html_button(button, background, text);
        let _: () = objc2::msg_send![button, setTag: hover_tag];
    }

    /// The destructive-TEXT treatment for a direct destructive action: the style document's
    /// destructive role is red text/border, and the red FILL is reserved for a confirming step. The
    /// picker's clear actions read the same way, so one destructive action never looks like two
    /// different things. The hover palette is the Action role's tag (a neutral wash), because a red
    /// fill on hover would turn a direct action into a confirming one.
    pub(super) unsafe fn apply_destructive_text(button: *mut AnyObject) {
        if button.is_null() {
            return;
        }
        let palette = crate::theme::ui_palette();
        let (_, _, hover_tag) = SettingsButtonRole::Action.style(palette);
        widgets::style_html_button(button, palette.button_bg, palette.destructive);
        let _: () = objc2::msg_send![button, setTag: hover_tag];
    }
}

/// Restore-defaults footer control: one compact trigger that morphs into confirm/cancel rows.
///
/// The control owns the view graph and animation geometry; settings.rs only coordinates the
/// business action and keeps this component in SettingsUi. This keeps the raw-pointer lifetime
/// with the settings window while making the whole lower-left control one semantic component.
#[derive(Clone, Copy)]
pub(super) struct RestoreDefaultsControl {
    pub(super) trigger: *mut AnyObject,
    pub(super) confirm: *mut AnyObject,
    pub(super) cancel: *mut AnyObject,
    pub(super) surface: *mut AnyObject,
    pub(super) container: *mut AnyObject,
    pub(super) separator: *mut AnyObject,
    // Expanded geometry (sidebar vs footer variants differ; fixed at build time):
    // confirm_y = the expanded confirm row's y inside the container; collapsed/expanded_h = the
    // container's collapsed/expanded height.
    confirm_y: f64,
    collapsed_h: f64,
    expanded_h: f64,
    pub(super) expanded: bool,
}

fn restore_surface_frame(container_origin: NSPoint, trigger_frame: NSRect) -> NSRect {
    NSRect::new(
        NSPoint::new(
            container_origin.x + trigger_frame.origin.x,
            container_origin.y + trigger_frame.origin.y,
        ),
        trigger_frame.size,
    )
}

fn restore_shell_frame(collapsed_surface: NSRect, height: f64) -> NSRect {
    NSRect::new(
        NSPoint::new(
            collapsed_surface.origin.x - RESTORE_SHELL_INSET,
            collapsed_surface.origin.y - RESTORE_SHELL_INSET,
        ),
        NSSize::new(
            collapsed_surface.size.width + RESTORE_SHELL_INSET * 2.0,
            height,
        ),
    )
}

fn restore_sidebar_container_frame(sidebar_width: f64, height: f64, origin_y: f64) -> NSRect {
    NSRect::new(
        NSPoint::new(RESTORE_CONTAINER_ORIGIN, origin_y),
        NSSize::new(
            (sidebar_width - RESTORE_CONTAINER_ORIGIN * 2.0).max(1.0),
            height,
        ),
    )
}

fn restore_sidebar_trigger_frame(sidebar_width: f64, height: f64) -> NSRect {
    let container_width = sidebar_width - RESTORE_CONTAINER_ORIGIN * 2.0;
    NSRect::new(
        NSPoint::new(RESTORE_TRIGGER_INSET, RESTORE_TRIGGER_INSET),
        NSSize::new(
            (container_width - RESTORE_TRIGGER_INSET * 2.0).max(1.0),
            height,
        ),
    )
}

unsafe impl Send for RestoreDefaultsControl {}
unsafe impl Sync for RestoreDefaultsControl {}

impl RestoreDefaultsControl {
    const SHELL_DURATION: f64 = crate::theme::ANIMATION_DURATION_MEDIUM;
    const CONTENT_DURATION: f64 = crate::theme::ANIMATION_DURATION_MEDIUM;
    const LABEL_OPEN_DURATION: f64 = crate::theme::ANIMATION_DURATION_FAST;
    const LABEL_CLOSE_DURATION: f64 =
        crate::theme::ANIMATION_DURATION_FAST * crate::theme::ANIMATION_EXIT_RATIO;

    pub(super) fn empty() -> Self {
        Self {
            trigger: std::ptr::null_mut(),
            confirm: std::ptr::null_mut(),
            cancel: std::ptr::null_mut(),
            surface: std::ptr::null_mut(),
            container: std::ptr::null_mut(),
            separator: std::ptr::null_mut(),
            confirm_y: 0.0,
            collapsed_h: 0.0,
            expanded_h: 0.0,
            expanded: false,
        }
    }

    pub(super) unsafe fn build(
        parent: *mut AnyObject,
        target: *mut AnyObject,
        sidebar_width: f64,
    ) -> Self {
        let button_frame = restore_sidebar_trigger_frame(sidebar_width, 32.0);
        let button_w = button_frame.size.width;
        let trigger_title = t("settings.btn_restore_defaults");
        let confirm_title = t("settings.btn_confirm");
        let cancel_title = t("settings.btn_cancel");
        let trigger = SettingsButton::action(
            button_frame,
            &trigger_title,
            target,
            objc2::sel!(handleRestoreDefaults:),
            // The sidebar restore trigger follows `.restore-btn` in index.html: a quieter
            // raised surface with secondary ink and the compact 12pt label.
            SettingsButtonRole::Footer,
        );
        let trigger_font: *mut AnyObject = objc2::msg_send![
            objc2::class!(NSFont),
            systemFontOfSize: crate::theme::FONT_CAPTION
        ];
        let _: () = objc2::msg_send![trigger, setFont: trigger_font];
        let trigger_ink = crate::ffi::hex_to_ns_color(widgets::settings_palette().secondary_text);
        let _: () = objc2::msg_send![trigger, setContentTintColor: trigger_ink];
        let confirm = SettingsButton::action(
            button_frame,
            &confirm_title,
            target,
            objc2::sel!(handleRestoreDefaultsConfirm:),
            SettingsButtonRole::Destructive,
        );
        let cancel = SettingsButton::action(
            button_frame,
            &cancel_title,
            target,
            objc2::sel!(handleRestoreDefaultsCancel:),
            SettingsButtonRole::Action,
        );
        let button_h = [trigger, confirm, cancel]
            .into_iter()
            .map(|button| widgets::configure_settings_button_wrapping(button, button_w, 3))
            .fold(32.0f64, f64::max);
        let button_frame = NSRect::new(
            button_frame.origin,
            objc2_foundation::NSSize::new(button_w, button_h),
        );
        for button in [trigger, confirm, cancel] {
            let _: () = objc2::msg_send![button, setFrame: button_frame];
            widgets::center_settings_button_label(button, button_h);
            widgets::refresh_settings_button_tracking(button);
        }
        let collapsed_h = button_h + RESTORE_SHELL_INSET * 2.0;
        let expanded_h = button_h * 2.0 + RESTORE_ACTION_ROW_GAP + RESTORE_SHELL_INSET * 2.0;
        let confirm_y = RESTORE_TRIGGER_INSET + button_h + RESTORE_ACTION_ROW_GAP;
        // `.sidebar-version` sits below the restore button in the HTML footer: 12pt from the
        // sidebar bottom, then 8pt before the button. Keep the existing 12pt gap above the rule.
        let version_label: *mut AnyObject = objc2::msg_send![objc2::class!(NSTextField), alloc];
        let version_frame = NSRect::new(
            NSPoint::new(
                RESTORE_CONTAINER_ORIGIN + RESTORE_TRIGGER_INSET + SIDEBAR_VERSION_HORIZONTAL_INSET,
                SIDEBAR_VERSION_BOTTOM_INSET,
            ),
            NSSize::new(
                (sidebar_width
                    - (RESTORE_CONTAINER_ORIGIN
                        + RESTORE_TRIGGER_INSET
                        + SIDEBAR_VERSION_HORIZONTAL_INSET)
                        * 2.0)
                    .max(1.0),
                SIDEBAR_VERSION_HEIGHT,
            ),
        );
        let version_label: *mut AnyObject = objc2::msg_send![
            version_label,
            initWithFrame: version_frame
        ];
        widgets::set_field(
            version_label,
            tf(
                "settings.version_label",
                &[("version", env!("CARGO_PKG_VERSION"))],
            ),
        );
        let _: () = objc2::msg_send![version_label, setBezeled: false];
        let _: () = objc2::msg_send![version_label, setDrawsBackground: false];
        let _: () = objc2::msg_send![version_label, setEditable: false];
        let _: () = objc2::msg_send![version_label, setSelectable: false];
        let version_font: *mut AnyObject = objc2::msg_send![
            objc2::class!(NSFont),
            systemFontOfSize: 11.0f64
        ];
        let _: () = objc2::msg_send![version_label, setFont: version_font];
        let version_ink = crate::ffi::hex_to_ns_color(widgets::settings_palette().muted_text);
        let _: () = objc2::msg_send![version_label, setTextColor: version_ink];
        let _: () = objc2::msg_send![version_label, setAutoresizingMask: 36u64];
        let _: () = objc2::msg_send![parent, addSubview: version_label];

        let container_y =
            SIDEBAR_VERSION_BOTTOM_INSET + SIDEBAR_VERSION_HEIGHT + SIDEBAR_VERSION_GAP
                - RESTORE_TRIGGER_INSET;
        let separator_x = RESTORE_SIDEBAR_OUTER_INSET;
        let separator: *mut AnyObject = objc2::msg_send![objc2::class!(NSView), alloc];
        let separator: *mut AnyObject = objc2::msg_send![
            separator,
            initWithFrame: NSRect::new(
                NSPoint::new(separator_x, container_y + collapsed_h + 4.0),
                objc2_foundation::NSSize::new(sidebar_width - separator_x * 2.0, 1.0),
            )
        ];
        let _: () = objc2::msg_send![separator, setWantsLayer: true];
        let separator_layer: *mut AnyObject = objc2::msg_send![separator, layer];
        if !separator_layer.is_null() {
            crate::ffi::layer_set_background(
                separator_layer,
                crate::ffi::hex_to_cg_color(widgets::settings_palette().separator),
            );
        }
        let _: () = objc2::msg_send![parent, addSubview: separator];

        // The expanded card is a separate surface behind the buttons. Keeping it outside the
        // button container lets the compact trigger retain its original look while the card
        // fades/grows in as one rounded surface.
        let initial_trigger_frame =
            NSRect::new(button_frame.origin, NSSize::new(button_w, button_h));
        let initial_surface_frame = restore_surface_frame(
            NSPoint::new(container_y, container_y),
            initial_trigger_frame,
        );
        let surface: *mut AnyObject = objc2::msg_send![objc2::class!(NSView), alloc];
        let surface: *mut AnyObject = objc2::msg_send![
            surface,
            initWithFrame: initial_surface_frame
        ];
        let _: () = objc2::msg_send![surface, setWantsLayer: true];
        let surface_layer: *mut AnyObject = objc2::msg_send![surface, layer];
        if !surface_layer.is_null() {
            let palette = widgets::settings_palette();
            crate::ffi::layer_set_background(
                surface_layer,
                crate::ffi::hex_to_cg_color(palette.card_bg),
            );
            crate::ffi::layer_set_border(
                surface_layer,
                crate::ffi::hex_to_cg_color(palette.card_border),
            );
            let _: () = objc2::msg_send![surface_layer, setBorderWidth: 1.0f64];
            let _: () = objc2::msg_send![surface_layer, setCornerRadius: crate::theme::RADIUS_CARD];
            let _: () = objc2::msg_send![surface_layer, setMasksToBounds: true];
        }
        // Collapsed state: the shell stays fully hidden. It shares the trigger's frame, and its
        // own hairline border composites with the trigger's border into a muddy double ring
        // (most visible on hover when the trigger fill turns translucent). It is revealed only
        // while expanding.
        let _: () = objc2::msg_send![surface, setHidden: true];
        let _: () = objc2::msg_send![surface, setAlphaValue: 0.0f64];
        let _: () = objc2::msg_send![parent, addSubview: surface];

        let container_frame =
            restore_sidebar_container_frame(sidebar_width, collapsed_h, container_y);
        let container: *mut AnyObject = objc2::msg_send![objc2::class!(NSView), alloc];
        let container: *mut AnyObject = objc2::msg_send![
            container,
            initWithFrame: container_frame
        ];
        let _: () = objc2::msg_send![container, setAutoresizingMask: 36u64];
        let _: () = objc2::msg_send![container, setWantsLayer: true];
        let container_layer: *mut AnyObject = objc2::msg_send![container, layer];
        if !container_layer.is_null() {
            // Match the reference root's `overflow-hidden`: the upper row is revealed only as
            // the shell grows past it.
            let _: () = objc2::msg_send![container_layer, setMasksToBounds: true];
            let _: () =
                objc2::msg_send![container_layer, setCornerRadius: crate::theme::RADIUS_CARD];
        }
        let _: () = objc2::msg_send![parent, addSubview: container];

        let _: () = objc2::msg_send![trigger, setAutoresizingMask: 36u64];
        let _: () = objc2::msg_send![container, addSubview: trigger];

        let _: () = objc2::msg_send![confirm, setAutoresizingMask: 36u64];
        let _: () = objc2::msg_send![confirm, setHidden: true];
        let _: () = objc2::msg_send![confirm, setAlphaValue: 0.0f64];
        let _: () = objc2::msg_send![container, addSubview: confirm];

        let _: () = objc2::msg_send![cancel, setAutoresizingMask: 36u64];
        let _: () = objc2::msg_send![cancel, setHidden: true];
        let _: () = objc2::msg_send![cancel, setAlphaValue: 0.0f64];
        let _: () = objc2::msg_send![container, addSubview: cancel];

        crate::ffi::CFRelease(separator as *const std::ffi::c_void);
        crate::ffi::CFRelease(version_label as *const std::ffi::c_void);
        crate::ffi::CFRelease(surface as *const std::ffi::c_void);
        crate::ffi::CFRelease(container as *const std::ffi::c_void);
        crate::ffi::CFRelease(trigger as *const std::ffi::c_void);
        crate::ffi::CFRelease(confirm as *const std::ffi::c_void);
        crate::ffi::CFRelease(cancel as *const std::ffi::c_void);

        Self {
            trigger,
            confirm,
            cancel,
            surface,
            container,
            separator,
            confirm_y,
            collapsed_h,
            expanded_h,
            expanded: false,
        }
    }

    /// Page variant: a "Restore Page Defaults" control embedded at the end of one page's
    /// scrolling document (no separator).
    ///
    /// `(x, y_bottom)` is the available area's bottom-left corner in document coordinates (y up),
    /// and `width` is its available width. The button and its expanding container are pinned to
    /// the area's trailing edge; the expanded card grows upward over the page content (the
    /// control is the document's last subview).
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn build_for_page(
        parent: *mut AnyObject,
        target: *mut AnyObject,
        x: f64,
        y_bottom: f64,
        width: f64,
    ) -> Self {
        let button_w = width.clamp(1.0, 180.0);
        let horizontal_inset = RESTORE_TRIGGER_INSET;
        let container_w = button_w + horizontal_inset * 2.0;
        let button_frame = NSRect::new(
            NSPoint::new(horizontal_inset, RESTORE_TRIGGER_INSET),
            objc2_foundation::NSSize::new(button_w, 32.0),
        );
        let trigger_title = t("settings.btn_restore_page_defaults");
        let confirm_title = t("settings.btn_confirm");
        let cancel_title = t("settings.btn_cancel");
        let trigger = SettingsButton::action(
            button_frame,
            &trigger_title,
            target,
            objc2::sel!(handlePageRestoreDefaults:),
            SettingsButtonRole::Action,
        );
        let confirm = SettingsButton::action(
            NSRect::new(
                NSPoint::new(
                    horizontal_inset,
                    RESTORE_TRIGGER_INSET + 32.0 + RESTORE_ACTION_ROW_GAP,
                ),
                objc2_foundation::NSSize::new(button_w, 32.0),
            ),
            &confirm_title,
            target,
            objc2::sel!(handlePageRestoreDefaultsConfirm:),
            SettingsButtonRole::Destructive,
        );
        let cancel = SettingsButton::action(
            button_frame,
            &cancel_title,
            target,
            objc2::sel!(handlePageRestoreDefaultsCancel:),
            SettingsButtonRole::Action,
        );
        let button_h = [trigger, confirm, cancel]
            .into_iter()
            .map(|button| widgets::configure_settings_button_wrapping(button, button_w, 3))
            .fold(32.0f64, f64::max);
        let collapsed_h = button_h + RESTORE_SHELL_INSET * 2.0;
        let expanded_h = button_h * 2.0 + RESTORE_ACTION_ROW_GAP + RESTORE_SHELL_INSET * 2.0;
        let confirm_y = RESTORE_TRIGGER_INSET + button_h + RESTORE_ACTION_ROW_GAP;
        let container_frame = NSRect::new(
            NSPoint::new(x + width - container_w, y_bottom - collapsed_h),
            objc2_foundation::NSSize::new(container_w, collapsed_h),
        );
        let container: *mut AnyObject = objc2::msg_send![objc2::class!(NSView), alloc];
        // initWithFrame: returns the object; objc2 validates the return type encoding in debug
        // builds, so the return value must be bound.
        let container: *mut AnyObject = objc2::msg_send![container, initWithFrame: container_frame];
        // The document width is fixed: no autoresizing mask; it simply scrolls with the content.
        let _: () = objc2::msg_send![container, setAutoresizingMask: 0u64];
        let _: () = objc2::msg_send![container, setWantsLayer: true];
        let container_layer: *mut AnyObject = objc2::msg_send![container, layer];
        if !container_layer.is_null() {
            // Match the reference root's `overflow-hidden`: the upper row is revealed only as
            // the shell grows past it.
            let _: () = objc2::msg_send![container_layer, setMasksToBounds: true];
            let _: () =
                objc2::msg_send![container_layer, setCornerRadius: crate::theme::RADIUS_CARD];
        }

        // Keep the trigger compact and pinned to the page content's bottom-right; the expanded
        // confirm/cancel rows reuse the same width.
        for button in [trigger, confirm, cancel] {
            let mut frame: NSRect = objc2::msg_send![button, frame];
            if button == confirm {
                frame.origin.y = confirm_y;
            }
            frame.size.height = button_h;
            let _: () = objc2::msg_send![button, setFrame: frame];
            widgets::center_settings_button_label(button, button_h);
            widgets::refresh_settings_button_tracking(button);
        }

        // The expanded card is a separate surface behind the buttons that fades in and grows
        // upward as one rounded unit.
        // Collapsed shell stays hidden (same double-ring reason as the sidebar variant).
        let initial_trigger_frame =
            NSRect::new(button_frame.origin, NSSize::new(button_w, button_h));
        let initial_surface_frame =
            restore_surface_frame(container_frame.origin, initial_trigger_frame);
        let surface: *mut AnyObject = objc2::msg_send![objc2::class!(NSView), alloc];
        let surface: *mut AnyObject =
            objc2::msg_send![surface, initWithFrame: initial_surface_frame];
        let _: () = objc2::msg_send![surface, setHidden: true];
        let _: () = objc2::msg_send![surface, setAlphaValue: 0.0f64];
        let _: () = objc2::msg_send![surface, setWantsLayer: true];
        let surface_layer: *mut AnyObject = objc2::msg_send![surface, layer];
        if !surface_layer.is_null() {
            let palette = widgets::settings_palette();
            crate::ffi::layer_set_background(
                surface_layer,
                crate::ffi::hex_to_cg_color(palette.card_bg),
            );
            crate::ffi::layer_set_border(
                surface_layer,
                crate::ffi::hex_to_cg_color(palette.card_border),
            );
            let _: () = objc2::msg_send![surface_layer, setBorderWidth: 1.0f64];
            let _: () = objc2::msg_send![surface_layer, setCornerRadius: crate::theme::RADIUS_CARD];
            let _: () = objc2::msg_send![surface_layer, setMasksToBounds: true];
        }
        let _: () = objc2::msg_send![parent, addSubview: surface];
        // The container (with the buttons) must join the hierarchy ABOVE the surface.
        let _: () = objc2::msg_send![parent, addSubview: container];

        let _: () = objc2::msg_send![confirm, setHidden: true];
        let _: () = objc2::msg_send![confirm, setAlphaValue: 0.0f64];
        let _: () = objc2::msg_send![container, addSubview: confirm];

        let _: () = objc2::msg_send![cancel, setHidden: true];
        let _: () = objc2::msg_send![cancel, setAlphaValue: 0.0f64];
        let _: () = objc2::msg_send![container, addSubview: cancel];

        let _: () = objc2::msg_send![container, addSubview: trigger];

        crate::ffi::CFRelease(surface as *const std::ffi::c_void);
        crate::ffi::CFRelease(container as *const std::ffi::c_void);
        crate::ffi::CFRelease(trigger as *const std::ffi::c_void);
        crate::ffi::CFRelease(confirm as *const std::ffi::c_void);
        crate::ffi::CFRelease(cancel as *const std::ffi::c_void);

        Self {
            trigger,
            confirm,
            cancel,
            surface,
            container,
            separator: std::ptr::null_mut(),
            // The cancel row shares the trigger's position; the two action rows keep a 16pt gap.
            confirm_y,
            collapsed_h,
            expanded_h,
            expanded: false,
        }
    }

    pub(super) fn is_ready(self) -> bool {
        // The separator only exists in the sidebar variant (null for footer); it is not part
        // of the readiness check.
        !self.trigger.is_null()
            && !self.confirm.is_null()
            && !self.cancel.is_null()
            && !self.surface.is_null()
            && !self.container.is_null()
    }

    /// Test whether a hit-tested settings view belongs to the expanded restore card.
    pub(super) unsafe fn contains_hit_view(self, hit_view: *mut AnyObject) -> bool {
        if !self.is_ready() || !self.expanded || hit_view.is_null() {
            return false;
        }
        let mut view = hit_view;
        while !view.is_null() {
            if view == self.container || view == self.surface {
                return true;
            }
            view = objc2::msg_send![view, superview];
        }
        false
    }

    /// Toggle the component as one animated unit; the bottom row remains anchored in place.
    pub(super) unsafe fn set_expanded(&mut self, expanded: bool, animated: bool) {
        if !self.is_ready() || self.expanded == expanded {
            return;
        }
        self.expanded = expanded;
        let animated = animated && !crate::theme::reduce_motion_enabled();

        let trigger_frame: NSRect = objc2::msg_send![self.trigger, frame];
        let container_frame: NSRect = objc2::msg_send![self.container, frame];
        // The card is only slightly wider than the original trigger; both expanded rows keep the
        // trigger's original width and horizontal inset. The top padding is intentionally compact
        // so the card does not leave a large empty panel above Confirm.
        let expanded_row_width = trigger_frame.size.width;
        let cancel_frame = NSRect::new(
            // Keep Cancel exactly where the collapsed trigger lives. The card grows upward
            // around this fixed bottom anchor.
            trigger_frame.origin,
            objc2_foundation::NSSize::new(expanded_row_width, trigger_frame.size.height),
        );
        let confirm_frame = NSRect::new(
            NSPoint::new(trigger_frame.origin.x, self.confirm_y),
            objc2_foundation::NSSize::new(expanded_row_width, trigger_frame.size.height),
        );
        // The reference keeps panel content attached to the shell's moving top edge. In an
        // AppKit layer animation, subview layout does not reflow from the presentation bounds, so
        // explicitly animate Confirm from the collapsed top position to its expanded position.
        let confirm_collapsed_frame = NSRect::new(
            NSPoint::new(
                trigger_frame.origin.x,
                self.confirm_y + self.collapsed_h - self.expanded_h,
            ),
            objc2_foundation::NSSize::new(expanded_row_width, trigger_frame.size.height),
        );
        // The footer variant has no separator (null); skip the separator animation.
        let separator_frame: NSRect = if self.separator.is_null() {
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0))
        } else {
            objc2::msg_send![self.separator, frame]
        };
        let target_separator = if self.separator.is_null() {
            separator_frame
        } else {
            let shell_growth = self.expanded_h - self.collapsed_h;
            let target_separator_y = if expanded {
                separator_frame.origin.y + shell_growth
            } else {
                separator_frame.origin.y - shell_growth
            };
            NSRect::new(
                NSPoint::new(separator_frame.origin.x, target_separator_y),
                separator_frame.size,
            )
        };
        // Keep the collapsed shell exactly under the trigger; expansion adds the same inset on
        // each side, so the trigger remains centered in the rounded surface.
        let collapsed_surface = restore_surface_frame(container_frame.origin, trigger_frame);
        let target_container = restore_shell_frame(
            collapsed_surface,
            if expanded {
                self.expanded_h
            } else {
                self.collapsed_h
            },
        );
        let target_surface = if expanded {
            target_container
        } else {
            collapsed_surface
        };

        if expanded {
            // Reveal the shell for the expanded card (it stays hidden while collapsed; see the
            // build sites).
            let _: () = objc2::msg_send![self.surface, setHidden: false];
            let _: () = objc2::msg_send![self.trigger, setHidden: false];
            let _: () = objc2::msg_send![self.cancel, setHidden: false];
            let _: () = objc2::msg_send![self.confirm, setHidden: false];
            // The reference shell remains present for the whole morph; synchronize its visible
            // state without adding a separate fade timeline that would lag the size animation.
            Self::set_opacity_model(self.surface, 1.0);
            // Start the panel content below the collapsed shell and let it rise with the shell,
            // matching the reference's top-docked content reveal.
            let _: () = objc2::msg_send![objc2::class!(CATransaction), begin];
            let _: () = objc2::msg_send![objc2::class!(CATransaction), setDisableActions: true];
            let _: () = objc2::msg_send![self.confirm, setFrame: confirm_collapsed_frame];
            let _: () = objc2::msg_send![objc2::class!(CATransaction), commit];
            let _: () = objc2::msg_send![self.cancel, setFrame: cancel_frame];
            // Cancel takes the fixed dock slot while Restore Defaults fades beneath it.
            let _: () = objc2::msg_send![
                self.container,
                addSubview: self.cancel,
                positioned: 1isize,
                relativeTo: std::ptr::null::<AnyObject>()
            ];
            if animated {
                Self::animate_basic_opacity(
                    self.trigger,
                    0.0,
                    Self::LABEL_CLOSE_DURATION,
                    "restore-trigger-close",
                );
                Self::animate_motion_opacity(
                    self.cancel,
                    1.0,
                    Self::LABEL_OPEN_DURATION,
                    "restore-cancel-open",
                );
                Self::animate_content_open(self.confirm);
            } else {
                Self::set_opacity_model(self.trigger, 0.0);
                Self::set_opacity_model(self.cancel, 1.0);
                Self::set_content_model(self.confirm, 1.0, 0.0, 1.0);
            }
        } else {
            let _: () = objc2::msg_send![self.trigger, setHidden: false];
            let _: () = objc2::msg_send![self.cancel, setHidden: false];
            let _: () = objc2::msg_send![self.confirm, setHidden: false];
            // Reorder the trigger above the transparent closing controls so repeated Cancel/open
            // cycles cannot leave an invisible button swallowing clicks.
            let _: () = objc2::msg_send![
                self.container,
                addSubview: self.trigger,
                positioned: 1isize,
                relativeTo: std::ptr::null::<AnyObject>()
            ];
            if animated {
                Self::animate_content_exit(self.confirm);
                Self::animate_basic_opacity(
                    self.cancel,
                    0.0,
                    Self::LABEL_CLOSE_DURATION,
                    "restore-cancel-close",
                );
                Self::animate_motion_opacity(
                    self.trigger,
                    1.0,
                    Self::LABEL_OPEN_DURATION,
                    "restore-trigger-open",
                );
                // Fade the shell with the shrink so it never ends up painting behind the
                // trigger in the collapsed state.
                Self::animate_basic_opacity(
                    self.surface,
                    0.0,
                    Self::SHELL_DURATION,
                    "restore-shell-hide",
                );
            } else {
                // Confirmation collapses without animation. Cancel any in-flight opening
                // animations first so their presentation layers cannot keep Cancel visible over
                // the restored trigger.
                for view in [self.trigger, self.cancel, self.confirm, self.surface] {
                    let layer: *mut AnyObject = objc2::msg_send![view, layer];
                    if !layer.is_null() {
                        let _: () = objc2::msg_send![layer, removeAllAnimations];
                    }
                }
                Self::set_opacity_model(self.trigger, 1.0);
                Self::set_opacity_model(self.cancel, 0.0);
                Self::set_content_model(self.confirm, 0.0, 6.0, 0.98);
                Self::set_opacity_model(self.surface, 0.0);
                let _: () = objc2::msg_send![self.surface, setHidden: true];
            }
        }

        if animated {
            Self::animate_view_frame(
                self.surface,
                target_surface,
                Self::SHELL_DURATION,
                "restore-shell",
            );
            Self::animate_view_frame(
                self.container,
                target_container,
                Self::SHELL_DURATION,
                "restore-clip",
            );
            if !self.separator.is_null() {
                Self::animate_view_frame(
                    self.separator,
                    target_separator,
                    Self::SHELL_DURATION,
                    "restore-divider",
                );
            }
            Self::animate_view_frame(
                self.confirm,
                if expanded {
                    confirm_frame
                } else {
                    confirm_collapsed_frame
                },
                Self::CONTENT_DURATION,
                "restore-confirm-frame",
            );
        } else {
            let _: () = objc2::msg_send![self.surface, setFrame: target_surface];
            let _: () = objc2::msg_send![self.container, setFrame: target_container];
            if !self.separator.is_null() {
                let _: () = objc2::msg_send![self.separator, setFrame: target_separator];
            }
            let _: () = objc2::msg_send![
                self.confirm,
                setFrame: if expanded {
                    confirm_frame
                } else {
                    confirm_collapsed_frame
                }
            ];
        }
    }

    unsafe fn animate_view_frame(
        view: *mut AnyObject,
        target_frame: NSRect,
        duration: f64,
        key_prefix: &str,
    ) {
        let layer: *mut AnyObject = objc2::msg_send![view, layer];
        if layer.is_null() {
            let _: () = objc2::msg_send![view, setFrame: target_frame];
            return;
        }

        let presentation: *mut AnyObject = objc2::msg_send![layer, presentationLayer];
        let from_bounds: NSRect = if presentation.is_null() {
            objc2::msg_send![layer, bounds]
        } else {
            objc2::msg_send![presentation, bounds]
        };
        let from_position: NSPoint = if presentation.is_null() {
            objc2::msg_send![layer, position]
        } else {
            objc2::msg_send![presentation, position]
        };

        let _: () = objc2::msg_send![objc2::class!(CATransaction), begin];
        let _: () = objc2::msg_send![objc2::class!(CATransaction), setDisableActions: true];
        let _: () = objc2::msg_send![view, setFrame: target_frame];
        let _: () = objc2::msg_send![objc2::class!(CATransaction), commit];

        let to_bounds: NSRect = objc2::msg_send![layer, bounds];
        let to_position: NSPoint = objc2::msg_send![layer, position];
        let from_bounds_value: *mut AnyObject =
            objc2::msg_send![objc2::class!(NSValue), valueWithRect: from_bounds];
        let to_bounds_value: *mut AnyObject =
            objc2::msg_send![objc2::class!(NSValue), valueWithRect: to_bounds];
        Self::add_motion_value(
            layer,
            "bounds",
            from_bounds_value,
            to_bounds_value,
            duration,
            &format!("{key_prefix}-bounds"),
        );

        let from_position_value: *mut AnyObject =
            objc2::msg_send![objc2::class!(NSValue), valueWithPoint: from_position];
        let to_position_value: *mut AnyObject =
            objc2::msg_send![objc2::class!(NSValue), valueWithPoint: to_position];
        Self::add_motion_value(
            layer,
            "position",
            from_position_value,
            to_position_value,
            duration,
            &format!("{key_prefix}-position"),
        );
    }

    unsafe fn add_motion_value(
        layer: *mut AnyObject,
        key_path: &str,
        from: *mut AnyObject,
        to: *mut AnyObject,
        duration: f64,
        animation_key: &str,
    ) {
        if crate::theme::reduce_motion_enabled() {
            return;
        }
        let key_path = crate::ffi::make_nsstring(key_path);
        let animation: *mut AnyObject = objc2::msg_send![
            objc2::class!(CABasicAnimation),
            animationWithKeyPath: key_path
        ];
        crate::ffi::CFRelease(key_path as *const std::ffi::c_void);
        let _: () = objc2::msg_send![animation, setFromValue: from];
        let _: () = objc2::msg_send![animation, setToValue: to];
        let _: () = objc2::msg_send![animation, setDuration: duration];
        let timing = crate::theme::ease_standard_timing_function();
        if !timing.is_null() {
            let _: () = objc2::msg_send![animation, setTimingFunction: timing];
        }
        let animation_key = crate::ffi::make_nsstring(animation_key);
        let _: () = objc2::msg_send![layer, addAnimation: animation, forKey: animation_key];
        crate::ffi::CFRelease(animation_key as *const std::ffi::c_void);
    }

    unsafe fn set_layer_scalar(layer: *mut AnyObject, key_path: &str, value: f64) {
        let value: *mut AnyObject =
            objc2::msg_send![objc2::class!(NSNumber), numberWithDouble: value];
        let key_path = crate::ffi::make_nsstring(key_path);
        let _: () = objc2::msg_send![layer, setValue: value, forKeyPath: key_path];
        crate::ffi::CFRelease(key_path as *const std::ffi::c_void);
    }

    unsafe fn presentation_scalar(layer: *mut AnyObject, key_path: &str, fallback: f64) -> f64 {
        let presentation: *mut AnyObject = objc2::msg_send![layer, presentationLayer];
        if presentation.is_null() {
            return fallback;
        }
        let key_path = crate::ffi::make_nsstring(key_path);
        let value: *mut AnyObject = objc2::msg_send![presentation, valueForKeyPath: key_path];
        crate::ffi::CFRelease(key_path as *const std::ffi::c_void);
        if value.is_null() {
            fallback
        } else {
            objc2::msg_send![value, doubleValue]
        }
    }

    unsafe fn animate_motion_scalar(
        layer: *mut AnyObject,
        key_path: &str,
        from: f64,
        to: f64,
        duration: f64,
        animation_key: &str,
    ) {
        let from: *mut AnyObject =
            objc2::msg_send![objc2::class!(NSNumber), numberWithDouble: from];
        let to_value: *mut AnyObject =
            objc2::msg_send![objc2::class!(NSNumber), numberWithDouble: to];
        let _: () = objc2::msg_send![objc2::class!(CATransaction), begin];
        let _: () = objc2::msg_send![objc2::class!(CATransaction), setDisableActions: true];
        Self::set_layer_scalar(layer, key_path, to);
        let _: () = objc2::msg_send![objc2::class!(CATransaction), commit];
        Self::add_motion_value(layer, key_path, from, to_value, duration, animation_key);
    }

    unsafe fn animate_motion_opacity(
        view: *mut AnyObject,
        target: f64,
        duration: f64,
        animation_key: &str,
    ) {
        let layer: *mut AnyObject = objc2::msg_send![view, layer];
        if layer.is_null() {
            let _: () = objc2::msg_send![view, setAlphaValue: target];
            return;
        }
        let fallback: f64 = objc2::msg_send![view, alphaValue];
        let from = Self::presentation_scalar(layer, "opacity", fallback);
        // AppKit does not reliably mirror direct CALayer opacity writes back to alphaValue.
        // Update both so later animated and immediate transitions share one model value.
        let _: () = objc2::msg_send![view, setAlphaValue: target];
        Self::animate_motion_scalar(layer, "opacity", from, target, duration, animation_key);
    }

    unsafe fn animate_basic_opacity(
        view: *mut AnyObject,
        target: f64,
        duration: f64,
        animation_key: &str,
    ) {
        let layer: *mut AnyObject = objc2::msg_send![view, layer];
        if layer.is_null() {
            let _: () = objc2::msg_send![view, setAlphaValue: target];
            return;
        }
        let fallback: f64 = objc2::msg_send![view, alphaValue];
        let from = Self::presentation_scalar(layer, "opacity", fallback);
        let _: () = objc2::msg_send![view, setAlphaValue: target];
        Self::animate_basic_scalar(layer, "opacity", from, target, duration, animation_key);
    }

    unsafe fn set_opacity_model(view: *mut AnyObject, opacity: f64) {
        let _: () = objc2::msg_send![view, setAlphaValue: opacity];
        let layer: *mut AnyObject = objc2::msg_send![view, layer];
        if layer.is_null() {
            return;
        }
        let _: () = objc2::msg_send![objc2::class!(CATransaction), begin];
        let _: () = objc2::msg_send![objc2::class!(CATransaction), setDisableActions: true];
        Self::set_layer_scalar(layer, "opacity", opacity);
        let _: () = objc2::msg_send![objc2::class!(CATransaction), commit];
    }

    unsafe fn animate_basic_scalar(
        layer: *mut AnyObject,
        key_path: &str,
        from: f64,
        to: f64,
        duration: f64,
        animation_key: &str,
    ) {
        let from_value: *mut AnyObject =
            objc2::msg_send![objc2::class!(NSNumber), numberWithDouble: from];
        let to_value: *mut AnyObject =
            objc2::msg_send![objc2::class!(NSNumber), numberWithDouble: to];
        let _: () = objc2::msg_send![objc2::class!(CATransaction), begin];
        let _: () = objc2::msg_send![objc2::class!(CATransaction), setDisableActions: true];
        Self::set_layer_scalar(layer, key_path, to);
        let _: () = objc2::msg_send![objc2::class!(CATransaction), commit];
        if crate::theme::reduce_motion_enabled() {
            return;
        }
        let key_path = crate::ffi::make_nsstring(key_path);
        let animation: *mut AnyObject = objc2::msg_send![
            objc2::class!(CABasicAnimation),
            animationWithKeyPath: key_path
        ];
        crate::ffi::CFRelease(key_path as *const std::ffi::c_void);
        let _: () = objc2::msg_send![animation, setFromValue: from_value];
        let _: () = objc2::msg_send![animation, setToValue: to_value];
        let _: () = objc2::msg_send![animation, setDuration: duration];
        let timing = crate::theme::ease_standard_timing_function();
        if !timing.is_null() {
            let _: () = objc2::msg_send![animation, setTimingFunction: timing];
        }
        let animation_key = crate::ffi::make_nsstring(animation_key);
        let _: () = objc2::msg_send![layer, addAnimation: animation, forKey: animation_key];
        crate::ffi::CFRelease(animation_key as *const std::ffi::c_void);
    }

    unsafe fn set_content_model(view: *mut AnyObject, opacity: f64, y: f64, scale: f64) {
        Self::set_opacity_model(view, opacity);
        let layer: *mut AnyObject = objc2::msg_send![view, layer];
        if layer.is_null() {
            return;
        }
        let _: () = objc2::msg_send![objc2::class!(CATransaction), begin];
        let _: () = objc2::msg_send![objc2::class!(CATransaction), setDisableActions: true];
        Self::set_layer_scalar(layer, "opacity", opacity);
        Self::set_layer_scalar(layer, "transform.translation.y", y);
        Self::set_layer_scalar(layer, "transform.scale", scale);
        let _: () = objc2::msg_send![objc2::class!(CATransaction), commit];
    }

    unsafe fn animate_content_open(view: *mut AnyObject) {
        let layer: *mut AnyObject = objc2::msg_send![view, layer];
        if layer.is_null() {
            let _: () = objc2::msg_send![view, setAlphaValue: 1.0f64];
            return;
        }
        Self::set_content_model(view, 1.0, 0.0, 1.0);
        // Exact CONTENT_VARIANTS + CONTENT_SPRING mapping from the reference. AppKit's positive
        // Y points upward, so CSS y:-8 maps to native y:+8.
        Self::animate_motion_scalar(
            layer,
            "opacity",
            0.0,
            1.0,
            Self::CONTENT_DURATION,
            "restore-content-opacity",
        );
        Self::animate_motion_scalar(
            layer,
            "transform.translation.y",
            8.0,
            0.0,
            Self::CONTENT_DURATION,
            "restore-content-y",
        );
        Self::animate_motion_scalar(
            layer,
            "transform.scale",
            0.98,
            1.0,
            Self::CONTENT_DURATION,
            "restore-content-scale",
        );
    }

    unsafe fn animate_content_exit(view: *mut AnyObject) {
        let layer: *mut AnyObject = objc2::msg_send![view, layer];
        if layer.is_null() {
            let _: () = objc2::msg_send![view, setAlphaValue: 0.0f64];
            return;
        }
        let opacity = Self::presentation_scalar(layer, "opacity", 1.0);
        let y = Self::presentation_scalar(layer, "transform.translation.y", 0.0);
        let scale = Self::presentation_scalar(layer, "transform.scale", 1.0);
        let _: () = objc2::msg_send![view, setAlphaValue: 0.0f64];
        Self::animate_basic_scalar(
            layer,
            "opacity",
            opacity,
            0.0,
            crate::theme::animation_exit_duration(Self::CONTENT_DURATION),
            "restore-content-opacity",
        );
        Self::animate_basic_scalar(
            layer,
            "transform.translation.y",
            y,
            6.0,
            crate::theme::animation_exit_duration(Self::CONTENT_DURATION),
            "restore-content-y",
        );
        Self::animate_basic_scalar(
            layer,
            "transform.scale",
            scale,
            0.98,
            crate::theme::animation_exit_duration(Self::CONTENT_DURATION),
            "restore-content-scale",
        );
    }
}

/// Sidebar navigation component backed by the shared borderless button builder.
pub(super) struct SettingsSidebar;

/// The icon is part of the sidebar item's semantic data, not inferred from a page tag.
#[derive(Clone, Copy, Debug)]
pub(super) enum SettingsSidebarIcon {
    General,
    Switcher,
    Mouse,
    Clipboard,
    KeyBindings,
    KeystrokeDisplay,
    FinderMenu,
    About,
}

impl SettingsSidebarIcon {
    /// The icon for a page identity. Keyed by identity, never by sidebar index: the order lives in
    /// `SETTINGS_PAGES` alone, so moving a page moves its icon with it.
    pub(super) fn for_page(page: SettingsPageId) -> Self {
        match page {
            SettingsPageId::General => Self::General,
            SettingsPageId::Switcher => Self::Switcher,
            SettingsPageId::Mouse => Self::Mouse,
            SettingsPageId::Clipboard => Self::Clipboard,
            SettingsPageId::KeyBindings => Self::KeyBindings,
            SettingsPageId::KeystrokeDisplay => Self::KeystrokeDisplay,
            SettingsPageId::FinderMenu => Self::FinderMenu,
            SettingsPageId::About => Self::About,
        }
    }

    fn symbol_name(self) -> &'static str {
        match self {
            Self::General => "gearshape",
            Self::Switcher => "rectangle.on.rectangle",
            Self::Mouse => "computermouse",
            // `doc.on.clipboard` has a dark overlapping foreground layer in the system glyph;
            // use the clean document outline so the sidebar stays visually balanced.
            Self::Clipboard => "doc.text",
            // A command-key square: the page is about key chords and the behaviours they run.
            Self::KeyBindings => "command.square",
            Self::KeystrokeDisplay => "keyboard",
            // The Finder menu is a folder-bound integration; folder is an SF Symbol available in
            // every macOS this app supports.
            Self::FinderMenu => "folder",
            Self::About => "info.circle",
        }
    }
}

/// Vertical gap between two sidebar items, from the mockup's
/// `.settings-nav { gap: 2px }`.
pub(super) const SIDEBAR_ROW_GAP: f64 = 2.0;

fn sidebar_item_frames(w: f64, row_h: f64) -> (NSRect, NSRect) {
    let icon_frame = NSRect::new(
        objc2_foundation::NSPoint::new(
            widgets::SIDEBAR_ITEM_PAD_X,
            (row_h - widgets::SIDEBAR_ICON_SIZE) / 2.0,
        ),
        objc2_foundation::NSSize::new(widgets::SIDEBAR_ICON_SIZE, widgets::SIDEBAR_ICON_SIZE),
    );
    let label_frame = NSRect::new(
        objc2_foundation::NSPoint::new(widgets::SIDEBAR_LABEL_X, 0.0),
        objc2_foundation::NSSize::new(
            (w - widgets::SIDEBAR_LABEL_X - widgets::SIDEBAR_LABEL_TRAILING_INSET).max(1.0),
            row_h,
        ),
    );
    (icon_frame, label_frame)
}

/// Rect of the whole-sidebar hover tracker: from the first row's top edge down to
/// the last row's bottom edge. The row count comes from the caller and shares its
/// source with button creation (entries.len()), so adding a sidebar entry keeps the
/// tracker in sync automatically -- the previous hardcoded 6-row rect left the 7th
/// entry outside the tracker, so leaving the sidebar through that last row produced
/// no exit event and left its hover fill active.
fn sidebar_tracking_rect(x: f64, y_top: f64, w: f64, row_h: f64, row_count: usize) -> NSRect {
    let row_step = row_h + SIDEBAR_ROW_GAP;
    let spanned = row_count.saturating_sub(1) as f64;
    NSRect::new(
        objc2_foundation::NSPoint::new(x, y_top - spanned * row_step),
        objc2_foundation::NSSize::new(w, row_h + spanned * row_step),
    )
}

impl SettingsSidebar {
    /// Measure one shared row height for every localized sidebar title.
    pub(super) unsafe fn row_height(w: f64) -> f64 {
        // The titles come from the page identities, so a new page cannot be missing from the
        // measurement and silently under-size every row.
        let titles: Vec<String> = settings_page_ids()
            .iter()
            .map(|page| t(page.title_key()))
            .collect();
        widgets::settings_sidebar_required_row_height(w, &titles)
    }

    /// Set a view frame without allowing AppKit's implicit layer action to race the explicit motion.
    unsafe fn set_frame_without_implicit_animation(view: *mut AnyObject, frame: NSRect) {
        let _: () = objc2::msg_send![objc2::class!(CATransaction), begin];
        let _: () = objc2::msg_send![
            objc2::class!(CATransaction),
            setDisableActions: true
        ];
        let _: () = objc2::msg_send![view, setFrame: frame];
        let _: () = objc2::msg_send![objc2::class!(CATransaction), commit];
    }

    /// Keep sidebar selection immediate while users move through the page list.
    pub(super) unsafe fn move_highlight(highlight: *mut AnyObject, frame: NSRect) {
        if highlight.is_null() {
            return;
        }
        Self::set_frame_without_implicit_animation(highlight, frame);
    }

    pub(super) unsafe fn build(
        parent: *mut AnyObject,
        target: *mut AnyObject,
        x: f64,
        y0: f64,
        w: f64,
        row_h: f64,
    ) -> [*mut AnyObject; SETTINGS_PAGE_COUNT] {
        // The entries come from the page identity table: title key, icon and tag order all follow
        // `SETTINGS_PAGES`, so a page move cannot leave a hand-written sidebar behind.
        let row_step = row_h + SIDEBAR_ROW_GAP;
        widgets::make_sidebar_hover_tracking(
            parent,
            sidebar_tracking_rect(x, y0, w, row_h, SETTINGS_PAGE_COUNT),
        );
        std::array::from_fn(|index| {
            let identity = settings_page_identity(index);
            let icon = SettingsSidebarIcon::for_page(identity);
            SettingsSidebarTab::attach(
                parent,
                target,
                &t(identity.title_key()),
                icon,
                index as isize,
                x,
                y0 - index as f64 * row_step,
                w,
                row_h,
            )
        })
    }
}

/// One independently aligned icon-and-label tab inside the sidebar.
pub(super) struct SettingsSidebarTab;

impl SettingsSidebarTab {
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn attach(
        parent: *mut AnyObject,
        target: *mut AnyObject,
        title: &str,
        icon: SettingsSidebarIcon,
        tag: isize,
        x: f64,
        y: f64,
        w: f64,
        row_h: f64,
    ) -> *mut AnyObject {
        // Keep the icon and title on one explicit center line. NSTextField's cell can otherwise
        // place glyphs near the top of a 28pt frame while SF Symbols use their own optical box.
        let (icon_frame, label_frame) = sidebar_item_frames(w, row_h);
        SettingsControl::sidebar(
            parent,
            target,
            title,
            icon.symbol_name(),
            tag,
            x,
            y,
            w,
            row_h,
            icon_frame,
            label_frame,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{
        restore_shell_frame, restore_sidebar_container_frame, restore_sidebar_trigger_frame,
        restore_surface_frame, sidebar_item_frames, sidebar_tracking_rect, SettingsButtonRole,
        SettingsLayout, SettingsRow, SettingsSelect, RESTORE_ACTION_ROW_GAP,
        RESTORE_CONTAINER_ORIGIN, RESTORE_SHELL_INSET, RESTORE_SIDEBAR_OUTER_INSET,
        RESTORE_TRIGGER_INSET, ROW_ACTION_BTN_H, SEPARATOR_ABOVE_ROW_GAP, SETTINGS_PAGE_COUNT,
        SIDEBAR_ROW_GAP, SLIDER_READOUT_GAP, SLIDER_READOUT_H, SLIDER_READOUT_W,
    };
    use crate::settings::SETTINGS_CONTROL_TRAILING_INSET;

    #[test]
    fn restore_sidebar_trigger_and_shell_are_centered() {
        let sidebar_width = 220.0;
        let container = restore_sidebar_container_frame(sidebar_width, 48.0, 26.0);
        let trigger = restore_sidebar_trigger_frame(sidebar_width, 32.0);
        let collapsed = restore_surface_frame(container.origin, trigger);
        assert_eq!(container.origin.x, RESTORE_CONTAINER_ORIGIN);
        assert_eq!(container.origin.y, 26.0);
        assert_eq!(
            container.origin.x + container.size.width,
            sidebar_width - RESTORE_CONTAINER_ORIGIN
        );
        assert_eq!(collapsed.origin.x, RESTORE_SIDEBAR_OUTER_INSET);
        assert_eq!(
            collapsed.origin.x + collapsed.size.width,
            sidebar_width - RESTORE_SIDEBAR_OUTER_INSET
        );
        assert_eq!(
            collapsed.origin.x * 2.0 + collapsed.size.width,
            sidebar_width
        );

        let shell = restore_shell_frame(collapsed, 48.0);
        assert_eq!(collapsed.origin.x - shell.origin.x, RESTORE_SHELL_INSET);
        assert_eq!(collapsed.origin.y - shell.origin.y, RESTORE_SHELL_INSET);
        assert_eq!(
            shell.origin.x + shell.size.width - (collapsed.origin.x + collapsed.size.width),
            RESTORE_SHELL_INSET
        );
        assert_eq!(
            shell.origin.y + shell.size.height - (collapsed.origin.y + collapsed.size.height),
            RESTORE_SHELL_INSET
        );
        assert_eq!(RESTORE_ACTION_ROW_GAP, 16.0);
    }

    #[test]
    fn sidebar_tracking_rect_spans_every_row() {
        let row_h = 40.0;
        let row_step = row_h + SIDEBAR_ROW_GAP;
        let y0 = 300.0;
        let rect = sidebar_tracking_rect(0.0, y0, 240.0, row_h, SETTINGS_PAGE_COUNT);
        // Top edge = row 0's top; bottom edge = the last row's bottom.
        assert_eq!(rect.origin.y + rect.size.height, y0 + row_h);
        assert_eq!(
            rect.origin.y,
            y0 - (SETTINGS_PAGE_COUNT - 1) as f64 * row_step
        );
        assert_eq!(rect.size.width, 240.0);
        // A one-row sidebar covers exactly that row.
        let single = sidebar_tracking_rect(0.0, y0, 240.0, row_h, 1);
        assert_eq!(single.origin.y, y0);
        assert_eq!(single.size.height, row_h);
    }

    #[test]
    fn layout_keeps_controls_aligned_to_the_trailing_inset() {
        let layout = SettingsLayout::new(600.0);
        assert_eq!(
            layout.control_x + layout.control_w,
            600.0 - SETTINGS_CONTROL_TRAILING_INSET
        );
        assert_eq!(layout.row_h, 32.0);
        // `.setting-row { min-height: 56px }`: 32pt control + 12pt padding on each side.
        assert_eq!(layout.described_row_h, 56.0);
        assert_eq!(SettingsLayout::CONTROL_H, 32.0);
        assert_eq!(SettingsLayout::SINGLE_LINE_ROW_H, 56.0);
        // The mock's section rhythm: 20pt previous card bottom to title top + a 15pt title box +
        // 8pt title bottom to card top, with no card padding.
        assert_eq!(layout.section_step, 43.0);
        // `.setting-row + .setting-row` is a hairline, not a gap.
        assert_eq!(layout.row_gap, 0.0);
        // `card_top`/`card_bottom` moved into the page layout owner (`settings::page_canvas`).
    }

    #[test]
    fn settings_select_metrics_keep_trigger_and_row_on_the_fixed_grid() {
        let metrics = SettingsSelect::metrics();
        assert_eq!(metrics.control_h, 32.0);
        assert_eq!(metrics.row_h, 56.0);
    }

    #[test]
    fn row_label_width_tracks_the_space_before_the_control_column() {
        assert_eq!(
            SettingsRow::label_width_before_control(16.0, 420.0, 16.0),
            388.0
        );
        assert_eq!(
            SettingsRow::label_width_before_control(16.0, 320.0, 16.0),
            288.0
        );
        assert_eq!(
            SettingsRow::label_width_before_control(16.0, 20.0, 16.0),
            1.0
        );
    }

    #[test]
    fn switches_selects_and_sliders_share_the_row_centerline() {
        let y = 120.0;
        let row_h = SettingsLayout::SINGLE_LINE_ROW_H;
        for control_h in [20.0, 32.0, 36.0] {
            let origin = SettingsRow::control_origin_y(y, row_h, control_h);
            assert!((origin + control_h / 2.0 - (y + row_h / 2.0)).abs() < 1e-9);
        }
    }

    /// The page-top rhythm -- title, first heading, its card and that card's first row -- is owned
    /// by `SettingsPageHeader` plus the standard card metrics. Pinning the pieces the six pages
    /// share makes a reintroduced per-page offset fail here instead of only showing up as an
    /// uneven gap under one page's title.
    #[test]
    fn page_top_rhythm_keeps_the_first_card_symmetric() {
        use super::SettingsPageHeader;

        // Every page shares these two metrics, so the title and first section stay aligned across
        // pages while the top inset follows the 24pt padding in the reference.
        assert_eq!(
            SettingsPageHeader::TOP_PADDING + SettingsPageHeader::FIRST_SECTION_GAP,
            32.0
        );
        // The title sits clearly below the pane top and clearly above the first card's heading.
        assert_eq!(SettingsPageHeader::TOP_PADDING, 24.0);
        assert_eq!(SettingsPageHeader::FIRST_SECTION_GAP, 8.0);

        let layout = SettingsLayout::new(600.0);
        // The heading's frame bottom -- the cursor `PageCanvas` starts a page from.
        let heading_cursor = 100.0;
        let row_h = SettingsLayout::SINGLE_LINE_ROW_H;
        let row_bottom = heading_cursor - layout.row_gap - row_h;
        let row_top = row_bottom + row_h;
        // Single-row card: contiguous rows start on the heading cursor, so the row box reaches the
        // card's top inset while the card keeps the same 8pt inset below the row -- the row band
        // sits centered inside the card.
        let card_top = heading_cursor + SettingsLayout::CARD_BOTTOM_INSET;
        assert_eq!(card_top - row_top, layout.card_bottom_inset);
        let card_visible_bottom = super::widgets::settings_card_rect(super::NSRect::new(
            super::NSPoint::new(0.0, row_bottom - layout.card_bottom_inset),
            super::NSSize::new(1.0, 1.0),
        ))
        .origin
        .y;
        assert_eq!(row_bottom - card_visible_bottom, layout.card_bottom_inset);
    }

    /// The section rhythm is the mock's, end to end: 20pt card bottom to title top, a 15pt title
    /// box, 8pt title bottom to card top, and no card padding.
    #[test]
    fn section_step_is_the_mock_rhythm() {
        let layout = SettingsLayout::new(600.0);
        assert_eq!(layout.section_step, 43.0);
        assert_eq!(
            layout.section_step,
            super::super::SETTINGS_SECTION_HEADER_GAP
                + super::super::widgets::SECTION_HEADER_H
                + super::super::SETTINGS_SECTION_CARD_GAP
                + layout.card_bottom_inset
        );
    }

    #[test]
    fn settings_layout_tokens_follow_the_four_point_grid() {
        let layout = SettingsLayout::new(600.0);
        for value in [
            ROW_ACTION_BTN_H,
            RESTORE_SHELL_INSET,
            RESTORE_TRIGGER_INSET,
            RESTORE_CONTAINER_ORIGIN,
            RESTORE_ACTION_ROW_GAP,
            SLIDER_READOUT_W,
            SLIDER_READOUT_GAP,
            SLIDER_READOUT_H,
            SEPARATOR_ABOVE_ROW_GAP,
            layout.label_x,
            layout.control_w,
            layout.row_h,
            layout.described_row_h,
            layout.row_gap,
            layout.card_bottom_inset,
            super::super::SETTINGS_SECTION_CARD_GAP,
            SETTINGS_CONTROL_TRAILING_INSET,
            super::super::SETTINGS_CONTROL_LABEL_GAP,
            super::SettingsPageHeader::TOP_PADDING,
        ] {
            assert_eq!(value % 4.0, 0.0, "off-grid settings metric: {value}");
        }
    }

    #[test]
    fn sidebar_frames_share_the_same_vertical_center() {
        let (icon, label) = sidebar_item_frames(240.0, 38.0);
        let icon_center = icon.origin.y + icon.size.height / 2.0;
        let label_center = label.origin.y + label.size.height / 2.0;
        assert_eq!(icon_center, label_center);
        // `.nav-item { padding: 0 var(--sp-2); gap: var(--sp-2) }` plus the 17pt nav icon.
        assert_eq!(icon.origin.x, 8.0);
        assert_eq!(label.origin.x, 8.0 + 17.0 + 8.0);
    }

    #[test]
    fn button_roles_keep_normal_and_hover_semantics_distinct() {
        for palette in [
            crate::theme::ui_palette_for_mode(false),
            crate::theme::ui_palette_for_mode(true),
        ] {
            assert_eq!(
                SettingsButtonRole::Action.style(palette),
                (palette.field_bg, palette.button_text, -3)
            );
            assert_eq!(
                SettingsButtonRole::Footer.style(palette),
                (palette.footer_button_bg, palette.button_text, -1)
            );
            assert_eq!(
                SettingsButtonRole::Primary.style(palette),
                (palette.accent, palette.accent_ink, -2)
            );
            assert_eq!(
                SettingsButtonRole::Destructive.style(palette),
                (palette.destructive, palette.accent_text, -4)
            );
        }
    }

    /// A row's Edit / Delete buttons must show a hover state the user can actually see.
    ///
    /// The original defect: they used a field-family role whose normal (`field_bg`) and hover
    /// (`hover_bg`) tokens differ by five alpha steps of the same neutral. Over `card_bg` they composite to
    /// **1.024:1** (light) / **1.108:1** (dark) -- a hover nobody can perceive, which style doc §10
    /// requires every custom-drawn control to have. The role check next door could not catch it: it
    /// only asserts a role's hover token differs from its normal token, and those two tokens *are*
    /// different values. Asserting the composited result is what makes the gate fail on the defect.
    ///
    /// The floor is 1.05:1, the same one `theme`'s `hover_and_selection_fills_are_visible_against_the_surface_in_both_modes`
    /// uses to rule out a fill that is *effectively invisible*; the defect's 1.024:1 sits below it
    /// while the shipped secondary surface clears it in both modes.
    #[test]
    fn row_action_buttons_have_a_visible_hover_state() {
        const MIN_HOVER_CONTRAST: f64 = 1.05;
        for dark in [false, true] {
            let palette = crate::theme::ui_palette_for_mode(dark);
            let (normal, _text, tag) = SettingsButtonRole::ROW_ACTION.style(palette);
            let hover = super::widgets::action_button_hover_color(tag, palette);
            // The surfaces are washes over the card, so compare them composited the way they render.
            let normal_rgb = crate::theme::composite_over(normal, palette.card_bg);
            let hover_rgb = crate::theme::composite_over(hover, palette.card_bg);
            let contrast = crate::theme::contrast_ratio(hover_rgb, normal_rgb);
            assert!(
                contrast >= MIN_HOVER_CONTRAST,
                "row action buttons' hover is invisible in {} mode: {contrast:.3}:1 (min {MIN_HOVER_CONTRAST})",
                if dark { "dark" } else { "light" }
            );
        }
    }

    /// Every role the settings pages actually ask for must have a hover the user can see.
    ///
    /// The defect was call-site-shaped, not role-shaped: the row buttons and both "Add" bands sat on
    /// `card_bg` yet asked for a field-family role whose hover composites to 1.024:1 there. Asserting
    /// only the role this change introduces would let the next button pick the same trap, so this
    /// reads the pages' role arguments and holds each named role to the floor on the surface those
    /// pages put buttons on. A future field-family role would have to argue for itself here.
    #[test]
    fn every_role_the_settings_pages_ask_for_has_a_visible_hover_on_the_card() {
        const MIN_HOVER_CONTRAST: f64 = 1.05;
        let sources = [
            ("bindings.rs", include_str!("bindings.rs")),
            ("mapping.rs", include_str!("mapping.rs")),
            ("page_builder.rs", include_str!("page_builder.rs")),
        ];
        let palette = crate::theme::ui_palette_for_mode(false);
        let mut checked = 0usize;
        for (name, source) in sources {
            for (index, line) in source.lines().enumerate() {
                let trimmed = line.trim();
                // A call site passes the role as a bare `SettingsButtonRole::X,` argument. Comments
                // and the test module's own references are not call sites.
                if !trimmed.starts_with("SettingsButtonRole::") || !trimmed.ends_with(',') {
                    continue;
                }
                let role_name = trimmed
                    .trim_start_matches("SettingsButtonRole::")
                    .trim_end_matches(',');
                // `ROW_ACTION` is an alias for `Action`; both spellings are accepted, and the alias
                // exists so a page states *why* it wants that surface.
                let Some(role) = [
                    SettingsButtonRole::Action,
                    SettingsButtonRole::ControlField,
                    SettingsButtonRole::Footer,
                    SettingsButtonRole::Primary,
                    SettingsButtonRole::Destructive,
                ]
                .into_iter()
                .find(|role| format!("{role:?}") == role_name)
                .or((role_name == "ROW_ACTION").then_some(SettingsButtonRole::ROW_ACTION)) else {
                    panic!("{name}:{} names an unknown role {role_name:?}", index + 1);
                };
                let (normal, _text, tag) = role.style(palette);
                let contrast = crate::theme::contrast_ratio(
                    crate::theme::composite_over(
                        super::widgets::action_button_hover_color(tag, palette),
                        palette.card_bg,
                    ),
                    crate::theme::composite_over(normal, palette.card_bg),
                );
                assert!(
                    contrast >= MIN_HOVER_CONTRAST,
                    "{name}:{} builds a {role_name} button whose hover is invisible on the card: \
                     {contrast:.3}:1 (min {MIN_HOVER_CONTRAST})",
                    index + 1
                );
                checked += 1;
            }
        }
        // A source scan that matches nothing would pass vacuously.
        assert!(checked >= 8, "only {checked} role arguments were found");
    }
}
