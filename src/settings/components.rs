//! Semantic components for the Settings window: pages, cards, rows, and layout metrics.
//!
//! These components intentionally remain thin wrappers around the existing AppKit builders.
//! Keeping ownership of raw Objective-C pointers in `settings.rs` avoids changing callback and
//! configuration lifetimes while giving every page one place for shared geometry rules.

use objc2::runtime::{AnyObject, Sel};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::sync::{LazyLock, Mutex};

use crate::ffi::release_obj;
use crate::i18n::t;

use super::{tooltip::SettingsTooltip, widgets, SETTINGS_PAGE_COUNT};

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

/// Gap between a card's internal divider and the top edge of the row below it (`separator_above_row`).
const SEPARATOR_ABOVE_ROW_GAP: f64 = 4.0;

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
    /// Gap between a section header and the card top edge.
    pub card_header_gap: f64,
}

impl SettingsLayout {
    /// Standard visual height of one settings row; controls remain shorter and center inside it.
    pub(super) const SINGLE_LINE_ROW_H: f64 = 52.0;
    pub(super) const CONTROL_H: f64 = 32.0;

    pub(super) fn new(content_w: f64) -> Self {
        let control_w = 200.0;
        Self {
            label_x: 16.0,
            label_w: 220.0,
            control_x: content_w - control_w - super::SETTINGS_CONTROL_TRAILING_INSET,
            control_w,
            row_h: Self::CONTROL_H,
            // Detailed subtitles are no longer rendered; described rows use the same compact
            // height as every other single-line row.
            described_row_h: Self::SINGLE_LINE_ROW_H,
            section_step: super::SETTINGS_SECTION_HEADER_GAP + 24.0,
            row_gap: 8.0,
            card_bottom_inset: 8.0,
            card_header_gap: super::SETTINGS_SECTION_CARD_GAP,
        }
    }

    pub(super) fn card_top(self, header_y: f64) -> f64 {
        header_y - self.card_header_gap
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
/// distance from the pane top (`.content { padding: 42px 0 72px }`), plus the gap down to the
/// first section heading. Owns the whole page-top block so pages neither hardcode its metrics nor
/// drift apart: one call returns the cursor that first heading hangs from.
pub(super) struct SettingsPageHeader;

impl SettingsPageHeader {
    /// Distance from the pane top to the title's frame. The HTML mockup's `.content` padding is
    /// 42px, but the 26pt title's frame is taller than its ink, so the visible gap was tighter than
    /// the mockup's; 48 keeps `TOP_PADDING + FIRST_SECTION_GAP` unchanged (everything below the
    /// title stays where it was) while giving the title the intended breathing room.
    pub(super) const TOP_PADDING: f64 = 48.0;

    /// HTML `.content`'s bottom padding, i.e. the space the page must keep below its last element.
    /// Page documents keep this much space below their lowest content; overshooting it is the dead
    /// scroll space users see as "the page scrolls far past its content".
    pub(super) const BOTTOM_PADDING: f64 = 72.0;

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

    /// Center a native control by its view frame.
    unsafe fn center_control(child: *mut AnyObject, y: f64, row_h: f64) {
        if child.is_null() {
            return;
        }
        let mut frame: NSRect = objc2::msg_send![child, frame];
        frame.origin.y = y + (row_h - frame.size.height).max(0.0) / 2.0;
        let _: () = objc2::msg_send![child, setFrame: frame];
    }

    /// NSTextField's glyphs are top-biased when its frame is taller than the measured cell.
    /// Fit the frame to the cell's measured height before centering it, so the glyph baseline
    /// shares the same center line as the trailing control.
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
    /// Reserve enough row space for the longest candidate without changing height on selection.
    pub(super) unsafe fn metrics(
        width: f64,
        items: &[&str],
        minimum_control_h: f64,
        minimum_row_h: f64,
    ) -> SettingsSelectMetrics {
        let control_h =
            widgets::settings_select_required_control_height(width, items, minimum_control_h);
        SettingsSelectMetrics {
            control_h,
            row_h: minimum_row_h.max(control_h + 20.0),
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

    /// `default_value`: the value a double-click restores (None = double-click untouched).
    #[allow(clippy::too_many_arguments)] // same parameter list as widgets::make_*.
    pub(super) unsafe fn slider(
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        min: i64,
        max: i64,
        value: i64,
        default_value: Option<f64>,
    ) -> *mut AnyObject {
        widgets::make_slider(x, y, w, h, min, max, value, default_value)
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
    Compact,
    Footer,
    Primary,
    Destructive,
}

impl SettingsButtonRole {
    fn style(self, palette: crate::theme::UiPalette) -> (u32, u32, isize) {
        match self {
            Self::Action => (palette.button_bg, palette.button_text, -3),
            Self::Compact => (palette.field_bg, palette.button_text, 0),
            Self::Footer => (palette.footer_button_bg, palette.button_text, -1),
            Self::Primary => (palette.accent, palette.accent_text, -2),
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

fn restore_sidebar_container_frame(sidebar_width: f64, height: f64) -> NSRect {
    NSRect::new(
        NSPoint::new(RESTORE_CONTAINER_ORIGIN, RESTORE_CONTAINER_ORIGIN),
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
            SettingsButtonRole::Action,
        );
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
        let container_y = RESTORE_CONTAINER_ORIGIN;
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

        let container_frame = restore_sidebar_container_frame(sidebar_width, collapsed_h);
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
    WindowControl,
    QuickActions,
    KeystrokeDisplay,
    About,
}

impl SettingsSidebarIcon {
    fn symbol_name(self) -> &'static str {
        match self {
            Self::General => "gearshape",
            Self::Switcher => "rectangle.on.rectangle",
            Self::Mouse => "computermouse",
            // `doc.on.clipboard` has a dark overlapping foreground layer in the system glyph;
            // use the clean document outline so the sidebar stays visually balanced.
            Self::Clipboard => "doc.text",
            // A rectangle split into left/right halves mirrors the window-control
            // half-screen/quarter-snapping semantics.
            Self::WindowControl => "rectangle.split.2x2",
            // A bolt mirrors the quick-actions "jump straight there" semantics.
            Self::QuickActions => "bolt.circle",
            Self::KeystrokeDisplay => "keyboard",
            Self::About => "info.circle",
        }
    }
}

fn sidebar_item_frames(w: f64, row_h: f64) -> (NSRect, NSRect) {
    const ICON_X: f64 = 16.0;
    const ICON_SIZE: f64 = 18.0;
    const LABEL_X: f64 = 48.0;
    let icon_frame = NSRect::new(
        objc2_foundation::NSPoint::new(ICON_X, (row_h - ICON_SIZE) / 2.0),
        objc2_foundation::NSSize::new(ICON_SIZE, ICON_SIZE),
    );
    let label_frame = NSRect::new(
        objc2_foundation::NSPoint::new(LABEL_X, 0.0),
        objc2_foundation::NSSize::new((w - LABEL_X - 8.0).max(1.0), row_h),
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
    let row_step = row_h + 4.0;
    let spanned = row_count.saturating_sub(1) as f64;
    NSRect::new(
        objc2_foundation::NSPoint::new(x, y_top - spanned * row_step),
        objc2_foundation::NSSize::new(w, row_h + spanned * row_step),
    )
}

impl SettingsSidebar {
    /// Measure one shared row height for every localized sidebar title.
    pub(super) unsafe fn row_height(w: f64) -> f64 {
        let titles = [
            t("settings.sidebar_general"),
            t("settings.sidebar_switcher"),
            t("settings.sidebar_mouse"),
            t("settings.sidebar_clipboard"),
            t("settings.sidebar_window_control"),
            t("settings.sidebar_quick_actions"),
            t("settings.sidebar_keystroke_display"),
            t("settings.sidebar_about"),
        ];
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
        // Add new sidebar entries here: the component owns title keys, icons, tags, and spacing.
        // Keystroke Display follows Quick Actions; About remains last.
        let entries = [
            ("settings.sidebar_general", SettingsSidebarIcon::General),
            ("settings.sidebar_switcher", SettingsSidebarIcon::Switcher),
            ("settings.sidebar_mouse", SettingsSidebarIcon::Mouse),
            ("settings.sidebar_clipboard", SettingsSidebarIcon::Clipboard),
            (
                "settings.sidebar_window_control",
                SettingsSidebarIcon::WindowControl,
            ),
            (
                "settings.sidebar_quick_actions",
                SettingsSidebarIcon::QuickActions,
            ),
            (
                "settings.sidebar_keystroke_display",
                SettingsSidebarIcon::KeystrokeDisplay,
            ),
            ("settings.sidebar_about", SettingsSidebarIcon::About),
        ];
        let row_step = row_h + 4.0;
        widgets::make_sidebar_hover_tracking(
            parent,
            sidebar_tracking_rect(x, y0, w, row_h, entries.len()),
        );
        std::array::from_fn(|index| {
            let (title_key, icon) = entries[index];
            SettingsSidebarTab::attach(
                parent,
                target,
                &t(title_key),
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
        SettingsLayout, SettingsRow, RESTORE_ACTION_ROW_GAP, RESTORE_CONTAINER_ORIGIN,
        RESTORE_SHELL_INSET, RESTORE_SIDEBAR_OUTER_INSET, RESTORE_TRIGGER_INSET, ROW_ACTION_BTN_H,
        SEPARATOR_ABOVE_ROW_GAP, SETTINGS_PAGE_COUNT, SLIDER_READOUT_GAP, SLIDER_READOUT_H,
        SLIDER_READOUT_W,
    };
    use crate::settings::SETTINGS_CONTROL_TRAILING_INSET;

    #[test]
    fn restore_sidebar_trigger_and_shell_are_centered() {
        let sidebar_width = 220.0;
        let container = restore_sidebar_container_frame(sidebar_width, 48.0);
        let trigger = restore_sidebar_trigger_frame(sidebar_width, 32.0);
        let collapsed = restore_surface_frame(container.origin, trigger);
        assert_eq!(container.origin.x, RESTORE_CONTAINER_ORIGIN);
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
        let row_step = row_h + 4.0;
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
        assert_eq!(layout.described_row_h, 52.0);
        assert_eq!(SettingsLayout::CONTROL_H, 32.0);
        assert_eq!(SettingsLayout::SINGLE_LINE_ROW_H, 52.0);
        assert_eq!(layout.section_step, 48.0);
        assert_eq!(layout.row_gap, 8.0);
        // `card_top`/`card_bottom` moved into the page layout owner (`settings::page_canvas`).
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

    /// The page-top rhythm -- title, first heading, its card and that card's first row -- is owned
    /// by `SettingsPageHeader` plus the standard card metrics. Pinning the pieces the six pages
    /// share makes a reintroduced per-page offset fail here instead of only showing up as an
    /// uneven gap under one page's title.
    #[test]
    fn page_top_rhythm_keeps_the_first_card_symmetric() {
        use super::SettingsPageHeader;

        // What positions every page below the title is the *sum* of the top padding and the gap
        // under the title, not either value alone: the title's frame is taller than its ink, so
        // moving the title down must shrink the gap by the same amount and leave this constant
        // (`PageCanvas` re-places the title at `doc_height - TOP_PADDING - title_height`).
        assert_eq!(
            SettingsPageHeader::TOP_PADDING + SettingsPageHeader::FIRST_SECTION_GAP,
            56.0
        );
        // The title sits clearly below the pane top and clearly above the first card's heading.
        assert_eq!(SettingsPageHeader::TOP_PADDING, 48.0);
        assert_eq!(SettingsPageHeader::FIRST_SECTION_GAP, 8.0);

        let layout = SettingsLayout::new(600.0);
        // The heading's frame bottom -- the cursor `PageCanvas` starts a page from.
        let heading_cursor = 100.0;
        let row_h = SettingsLayout::SINGLE_LINE_ROW_H;
        let row_bottom = heading_cursor - layout.row_gap - row_h;
        let row_top = row_bottom + row_h;
        // Single-row card: the row starts 4pt under the heading and the card keeps its 8pt bottom
        // inset below the row.
        let card_top = heading_cursor - layout.card_header_gap;
        assert_eq!(card_top - row_top, 4.0);
        let card_visible_bottom = super::widgets::settings_card_rect(super::NSRect::new(
            super::NSPoint::new(0.0, row_bottom - layout.card_bottom_inset),
            super::NSSize::new(1.0, 1.0),
        ))
        .origin
        .y;
        assert_eq!(row_bottom - card_visible_bottom, 8.0);
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
            layout.card_header_gap,
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
        assert_eq!(icon.origin.x, 16.0);
        assert_eq!(label.origin.x, 48.0);
    }

    #[test]
    fn button_roles_keep_normal_and_hover_semantics_distinct() {
        for palette in [
            crate::theme::ui_palette_for_mode(false),
            crate::theme::ui_palette_for_mode(true),
        ] {
            assert_eq!(
                SettingsButtonRole::Action.style(palette),
                (palette.button_bg, palette.button_text, -3)
            );
            assert_eq!(
                SettingsButtonRole::Compact.style(palette),
                (palette.field_bg, palette.button_text, 0)
            );
            assert_eq!(
                SettingsButtonRole::Footer.style(palette),
                (palette.footer_button_bg, palette.button_text, -1)
            );
            assert_eq!(
                SettingsButtonRole::Primary.style(palette),
                (palette.accent, palette.accent_text, -2)
            );
            assert_eq!(
                SettingsButtonRole::Destructive.style(palette),
                (palette.destructive, palette.accent_text, -4)
            );
        }
    }
}
