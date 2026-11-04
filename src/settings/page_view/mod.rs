//! The settings pages as data: a page's rows described, laid out and addressed by value.
//!
//! Each migrated page is built in three stages, and each stage has exactly one owner:
//!
//! ```text
//! page_view(&PageInput) -> PageView            // pure: semantics only, no coordinates
//!      |
//!      v
//! layout_rows(&PageView, &LayoutInput) -> Vec<RowLayout>   // pure: the only layout computation
//!      |
//!      v
//! render_page(canvas, ui, &PageView, &[RowLayout])         // applies the result only
//! ```
//!
//! A [`RowSpec`] carries a card, its copy keys, the kind of control, an explicit [`ControlId`],
//! the value the control starts with, its options, the master switch that freezes it and its
//! search-index entry. It carries no frames: [`layout_rows`] turns the measured page into one
//! [`RowLayout`] per row, and the renderer only applies them. Creation, the "initial value"
//! refill and the in-place refresh all read the same [`RowSpec`], so a value the page shows and
//! the value the refresh writes cannot come from two different expressions.
//!
//! The description and layout stages are pure and are asserted by the unit tests below: nothing
//! above the `MARK: - Rendering` section touches AppKit. That third stage -- `render_page` and the
//! build, refill and `SettingsUi`-binding helpers beside it -- is the AppKit half and sits in the
//! same file, so this module is two halves in one file rather than a pure module. The section
//! markers are what keeps them apart; splitting them into separate files is recorded in
//! `docs/review-backlog.md`.

use super::*;

use std::collections::HashMap;

// The search index is written while a page is BUILT, not while it is described: a page-owned card
// that is not described at all records its rows through these, exactly as the inline builder did.
pub(super) use super::page_builder::{record_settings_search_row, set_settings_search_section};

// One module per page description, so pages can be written independently: each is pure and returns
// a `PageView`, and the shared machinery here (layout, rendering, identities, the update
// description) is the only thing they share.
mod about;
mod clipboard;
mod finder_menu;
mod general;
mod key_bindings;
mod keystroke_display;
mod mouse;
mod switcher;

use about::about_page_view;
pub(super) use about::AboutHooks;
use clipboard::clipboard_page_view;
pub(super) use clipboard::ClipboardHooks;
use finder_menu::finder_menu_page_view;
pub(super) use finder_menu::FinderMenuHooks;
use general::general_page_view;
use key_bindings::key_bindings_page_view;
pub(super) use key_bindings::KeyBindingsHooks;
use keystroke_display::keystroke_display_page_view;
use mouse::mouse_page_view;
pub(super) use mouse::MouseHooks;
use switcher::switcher_page_view;

/// Everything a page description may read, captured once per transaction.
pub(super) struct PageInput {
    pub(super) config: Config,
    /// The theme inputs the row copy and option labels follow (P0). A page that needs no theme
    /// output still carries the snapshot so its description is built from one input type.
    #[allow(dead_code)]
    pub(super) theme: crate::theme::ThemeInput,
    /// The device the Mouse page edits (`None` = "All Mice"). One config with two devices shows
    /// different values, so this is an input of the description, not something it discovers; the
    /// Switcher and General rows read only the config, and this keeps the input type complete for
    /// the pages that follow.
    #[allow(dead_code)]
    pub(super) device: Option<crate::mouse::device::DeviceKey>,
    /// The Finder Menu page's extension status. It comes from the system (is the extension
    /// elected?) and from the recovery transaction's last result, so it is captured here rather
    /// than computed by a description.
    pub(super) finder_menu_status: String,
    /// Whether the storage/permission banner carries the 0.2.3 permission-migration copy instead of
    /// the ordinary accessibility warning. It comes from the bundle's version and from a system
    /// default, so a description that asked the system itself would stop being a function of its
    /// input -- and the banner's HEIGHT depends on it, which is a layout input.
    pub(super) permission_migration_copy: bool,
}

impl PageInput {
    /// Capture the live configuration and the current theme snapshot.
    ///
    /// The single call site is a page's build; the concurrency source test in `settings.rs` counts
    /// these, so a description that reaches for a global instead is visible.
    pub(super) fn capture() -> Self {
        // ONE read of CONFIG, then the theme snapshot is DERIVED from it. Reading it twice in one
        // argument list used to nest a read inside a read (the first guard lives until the end of
        // the statement): a writer queueing between them parks the inner read while the outer guard
        // is held by the thread waiting for itself. It also held the guard across the AppKit query
        // `ThemeInput` makes for the "auto" appearance.
        let config = CONFIG.read().unwrap().clone();
        let device = super::current_selected_device();
        let mut input = Self::from_config(
            config.clone(),
            device,
            crate::theme::ThemeInput::from_config(&config),
        );
        // The one system read the descriptions cannot make: the extension's election and the
        // recovery transaction's outcome are process state, captured at build time.
        input.finder_menu_status =
            super::finder_menu_status_text_now(crate::finder_menu::extension_enabled());
        input.permission_migration_copy = crate::update_notice::needs_permission_migration_copy();
        input
    }

    pub(super) fn from_config(
        config: Config,
        device: Option<crate::mouse::device::DeviceKey>,
        theme: crate::theme::ThemeInput,
    ) -> Self {
        Self {
            config,
            theme,
            device,
            finder_menu_status: String::new(),
            permission_migration_copy: false,
        }
    }
}

/// The measured page a description is laid out into.
///
/// This is the complete layout input, not just font metrics: the content width, the page
/// viewport, the shared column metrics, the text measured in the current language and text size,
/// and the page constants the rows are placed with. Every field is a number the caller measured
/// or the design system owns; `layout_rows` computes the rest.
/// A box stated in another view's coordinates: the banner strip's frame in the settings content
/// view, as the build measured it. The renderer, the window's banner refresh and the test all take
/// this one box.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct BannerFrame {
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) w: f64,
    pub(super) h: f64,
}

impl BannerFrame {
    pub(super) fn of(rect: NSRect) -> Self {
        Self {
            x: rect.origin.x,
            y: rect.origin.y,
            w: rect.size.width,
            h: rect.size.height,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct LayoutInput {
    pub(super) content_w: f64,
    /// The page document's top edge (the height it was provisioned with); the rows are laid out
    /// from this, since they are subviews of the document.
    pub(super) doc_top: f64,
    /// The document's top edge down to the first section cursor, as the page header measured it
    /// (the canvas owns the header block; this is only its measurement).
    pub(super) header_offset: f64,
    /// The banner strip's frame in its FINAL parent (the settings content view), as the caller
    /// measured it: the detail pane's inset and the visible viewport's top. The renderer and the
    /// window's banner refresh both take this box, so the two cannot disagree.
    pub(super) banner_frame: BannerFrame,
    pub(super) layout: SettingsLayout,
    pub(super) text: MeasuredText,
    /// The heights of the page's own rows, resolved from the page input by the caller. A
    /// measured row cannot be resolved here: this type has no `PageInput`, so the page build
    /// resolves them once and the layout reads them.
    pub(super) extra_row_heights: Vec<f64>,
}

/// Text metrics a page's rows are placed with, measured once by the caller.
///
/// Only the described-row subtitle is measured today; a row whose copy wraps (the captioned and
/// stacked rows) already has its height measured by the widget that builds it, and the caller
/// hands that height over as a page constant.
#[derive(Clone, Debug, Default)]
pub(super) struct MeasuredText {
    /// Height of a described row's one-line subtitle in the current language.
    #[allow(dead_code)]
    pub(super) subtitle_h: f64,
    /// Height of a section heading's label box.
    pub(super) heading_h: f64,
    /// The one-line title height of a row's copy block.
    pub(super) title_h: f64,
    /// Gap between a row's title and its caption.
    pub(super) title_caption_gap: f64,
    /// Smallest caption height (one line of the description face).
    pub(super) caption_one_line_h: f64,
    /// Each row's wrapped subtitle height, in row order. A row with no subtitle has none here.
    pub(super) subtitle_heights: Vec<f64>,
}

/// The width a CAPTIONED row's text block occupies.
///
/// The inline builder stated it per row from two expressions:
///
/// - the default, `control_x - label_x - CONTROL_LABEL_GAP`: the text stops where the control
///   column begins. Every captioned row used this, including the ones whose control is a switch;
/// - the wide one, `content_w - label_x * 2 - 58`, used by exactly one row -- the General page's
///   menu-bar switch -- where the copy has to fit the design's two lines.
///
/// A row therefore states which block it takes (\`PageRowSpec::wide_caption\`) instead of the layout
/// guessing from the control: guessing gave every switch the wide block and wrapped the clipboard
/// page's three-line warnings onto two.
pub(super) fn captioned_text_width(layout: SettingsLayout, content_w: f64, row: &RowSpec) -> f64 {
    let narrow = (layout.control_x - layout.label_x - super::SETTINGS_CONTROL_LABEL_GAP).max(1.0);
    let width = if row.wide_caption {
        (content_w - layout.label_x * 2.0 - CAPTION_CONTROL_RESERVE).max(narrow)
    } else {
        narrow
    };
    width.max(layout.label_w).max(1.0)
}

/// Room a row that takes the wide text block keeps clear of a control at the card's far edge.
const CAPTION_CONTROL_RESERVE: f64 = 58.0;

impl MeasuredText {
    /// The text metrics the settings design system always uses. Only a caller that needs the
    /// measured value (a page with wrapping copy) replaces an entry.
    pub(super) fn base() -> Self {
        Self {
            subtitle_h: 0.0,
            heading_h: super::widgets::SECTION_HEADER_H,
            title_h: 18.0,
            title_caption_gap: 3.0,
            caption_one_line_h: 16.0,
            subtitle_heights: Vec::new(),
        }
    }

    /// Measure the page's subtitles: the wrapped height of every row whose copy is a stacked
    /// block. The page supplies the strings; this is the only place a height is measured.
    pub(super) fn measure(page: &PageView, layout: SettingsLayout, content_w: f64) -> Self {
        let mut measured = Self::base();
        measured.subtitle_heights = page
            .rows
            .iter()
            .map(|row| match row.copy {
                RowCopy::Captioned => unsafe {
                    let caption_w = captioned_text_width(layout, content_w, row);
                    super::widgets::wrapped_caption_height(&row.subtitle_text(), caption_w)
                        .max(measured.caption_one_line_h)
                },
                RowCopy::Plain => 0.0,
            })
            .collect();
        measured
    }
}

impl LayoutInput {
    /// An input measured for a page: the text metrics come from the shared row metrics, so a
    /// test can build one without a window.
    #[cfg(test)]
    pub(super) fn for_test(content_w: f64) -> Self {
        Self {
            content_w,
            doc_top: 900.0,
            header_offset: 111.0,
            banner_frame: BannerFrame {
                x: 28.0,
                y: 700.0 - SettingsPageHeader::TOP_PADDING - banner_height(false),
                w: content_w,
                h: banner_height(false),
            },
            layout: SettingsLayout::new(content_w),
            text: MeasuredText::base(),
            extra_row_heights: Vec::new(),
        }
    }
}

/// The kind of control a row carries, and therefore how it is built and how its value is shown.
///
/// The kind is presentation (a switch, a popup, a slider); *which* field the control edits comes
/// from its [`ControlId`], never from the kind -- several switches edit different fields, and a
/// button edits none.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum ControlKind {
    /// A switch, right-aligned in the control column.
    Switch,
    /// A popup in the control column, with its choices.
    Popup { options: Vec<String> },
    /// A stepped slider, with its readout already formatted for display.
    Slider {
        min: i64,
        max: i64,
        step: i64,
        /// The value a double-click restores.
        reset: Option<f64>,
        readout: String,
    },
    /// A row's action button: pressing it runs the row's action. Its `title_key` is the copy the
    /// button DRAWS, which is not the row's label (the About page's guide row reads "View guide"
    /// on the left and "Open" on the button).
    Button { title_key: &'static str },
    /// A read-only value drawn in the control column (a permission status, a device name): the row
    /// carries text, not an editable control, and a callback refreshes it.
    ///
    /// `action` adds the row's own action button beside the value (the Finder Menu page's status
    /// line opens the extension settings; the About page's permission rows open System Settings).
    Value {
        action: Option<ValueAction>,
        /// How the value's text is aligned. Defaults to the platform's natural alignment; a value
        /// that sits against its action button is right-aligned, as the shipped pages draw it.
        align: ValueAlign,
    },
    /// A hyperlink drawn as the row's control (the About page's website and GitHub rows). Its
    /// target is chosen by the `tag` the click handler reads; `text_key` is the copy it draws,
    /// which is the URL, not the row's label.
    Link { tag: isize, text_key: &'static str },
}

/// What a control is, for the dispatcher. A field identity is what the config stores; an action
/// identity is a command with nothing to store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ControlId {
    Field(ControlField),
    Action(&'static str),
}

impl ControlId {
    /// The field this row edits, if it edits one. An action control has no config field to refill,
    /// so it produces no update entry.
    pub(super) fn field(&self) -> Option<ControlField> {
        match self {
            ControlId::Field(field) => Some(*field),
            ControlId::Action(_) => None,
        }
    }
}

/// How a row's copy is stacked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RowCopy {
    /// Title only: the label sits centered in the row.
    Plain,
    /// The title with a subtitle below it that is measured and drawn.
    Captioned,
}

/// One row of a page description: what it says, what it edits and where it sits in the page.
#[derive(Clone, Debug)]
pub(super) struct RowSpec {
    /// The zero-based index of the card this row belongs to. A card boundary also carries the
    /// section's spacing, so the card index alone decides the layout.
    pub(super) card: usize,
    /// The card's heading copy key (read when the card opens).
    pub(super) section: &'static str,
    pub(super) label_key: &'static str,
    pub(super) desc_key: Option<&'static str>,
    /// The caption a stacked row draws (a copy key, like the label). Read for the search entry and
    /// measured by [`MeasuredText::measure`].
    pub(super) caption_key: Option<&'static str>,
    pub(super) control: ControlKind,
    /// Whether this row's LINK or VALUE control is drawn in the page's own value column (the About
    /// page's website, GitHub and version rows) instead of the shared control column: that column
    /// starts further right and runs to the card's far inset, which is where the URL text fits.
    pub(super) value_column: bool,
    /// Whether a captioned row's text block takes the WIDE width (up to the card's far inset)
    /// instead of the default one that stops at the control column. One row in the design needs
    /// it: the General page's menu-bar switch.
    pub(super) wide_caption: bool,
    /// The row's control identity. Every declared row carries one.
    pub(super) id: ControlId,
    pub(super) copy: RowCopy,
    /// The master switch that freezes this row's control, if any (the freeze itself is still
    /// applied by the master switch's own callback, which owns the enabled lists today).
    #[allow(dead_code)]
    pub(super) depends_on: Option<ControlField>,
    /// Whether the card draws a divider above this row (the first row of a card has none).
    pub(super) separator_above: bool,
    /// The page's conditional group this row belongs to, if any (the visibility is toggled on
    /// the canvas's own group list today).
    #[allow(dead_code)]
    pub(super) group: Option<RowGroup>,
    /// The value the control starts with; `None` for an action row.
    pub(super) value: Value,
    pub(super) search: SearchRow,
}
impl RowSpec {
    /// The caption's copy, empty for a row without one.
    pub(super) fn subtitle_text(&self) -> String {
        self.caption_key.map(t).unwrap_or_default()
    }

    /// The field this row edits, if it edits one.
    pub(super) fn field(&self) -> Option<ControlField> {
        self.id.field()
    }

    /// The row's own search entry: its copy keys resolved to text, with the page option list.
    fn search_entry(&self, section: &str) -> SearchRow {
        SearchRow {
            label: t(self.label_key),
            desc: self.desc_key.map(t),
            section: section.to_string(),
            options: match &self.control {
                ControlKind::Popup { options } => options.clone(),
                _ => Vec::new(),
            },
        }
    }
}

/// The search-index entry a row contributes, produced as a value by the page description.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SearchRow {
    pub(super) label: String,
    pub(super) desc: Option<String>,
    pub(super) section: String,
    pub(super) options: Vec<String>,
}

/// The value a control starts with, and the value the refill writes later.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Value {
    Switch(bool),
    Index(usize),
    Slider(i64),
    /// A control whose content is data rather than a locale key: the clipboard shortcut recorder's
    /// title is the recorded chord, and a permission status row shows text a callback maintains.
    Dynamic(String),
    /// An action row has no value.
    None,
}

impl Value {
    pub(super) fn describe(&self) -> String {
        match self {
            Value::Switch(true) => "on".to_string(),
            Value::Switch(false) => "off".to_string(),
            Value::Index(index) => index.to_string(),
            Value::Slider(value) => value.to_string(),
            Value::Dynamic(text) => text.clone(),
            Value::None => String::new(),
        }
    }
}

/// One "field -> value" entry of the refill / in-place refresh description.
///
/// `value` is what the CONTROL takes (a switch state, a popup index, a slider stop); `display` is
/// what its readout shows when that differs from the control's own value (the status-bar slider
/// carries a stop index and shows the point size that stop means).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ControlUpdate {
    pub(super) field: ControlField,
    /// The value the control is SET to, still typed the way the description stated it. The string
    /// form is [`Value::describe`], kept for the display contract and for the tests that read the
    /// description; a control that needs a NUMBER takes it from here instead of parsing the text
    /// back, which is how a popup index and a slider stop used to be recovered.
    pub(super) typed: Value,
    pub(super) value: String,
    pub(super) display: String,
}

/// A page's full description: its rows plus everything else the page draws around them.
pub(super) struct PageView {
    /// The page this description belongs to.
    #[allow(dead_code)]
    pub(super) page: SettingsPageId,
    /// The page title's copy key.
    pub(super) title_key: &'static str,
    /// The rows in document order.
    pub(super) rows: Vec<RowSpec>,
    /// The section heading the page's first rows belong to, until a card opens.
    #[allow(dead_code)]
    pub(super) initial_section: String,
    /// A page-level block above the rows (the General page's permission banner).
    pub(super) banner: Option<BannerSpec>,
    /// How far below the document's top edge the rows start, when the page draws its OWN header
    /// instead of the shared page title. `None` = the shared title (the canvas measures it); the
    /// About page pins an icon, a title and a subtitle of its own, so it states the offset the
    /// canvas must start from.
    pub(super) header_offset: Option<f64>,
    /// Rows the page draws ITSELF, in place inside a described card.
    ///
    /// The description covers controls; a few rows are widgets it has no kind for (a shortcut
    /// recorder with a capture state machine, a numeric text field). Those are ordered here, so the
    /// card still runs in one sequence, and [`PageHooks::extra_row`] draws them where they belong.
    pub(super) extra_rows: Vec<PageRowSpec>,
}

/// One row a page draws itself: where it sits, what the page calls it, and how tall it is on top of
/// the described rows it follows.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct PageRowSpec {
    /// The card the row belongs to, like [`RowSpec::card`].
    pub(super) card: usize,
    /// The card's heading copy key, like [`RowSpec::section`]: a card that holds ONLY page-drawn
    /// rows has no described row to take it from, so the page-drawn row carries it too.
    pub(super) section: &'static str,
    /// The page's own name for this row; passed back to [`PageHooks::extra_row`].
    pub(super) key: &'static str,
    /// The row's height. A described row's height comes from its copy; a page-drawn row states its
    /// own, because only the page knows what it puts there — either a constant, or a measurement
    /// over the page input (a row whose height follows the connected devices, for example).
    pub(super) height: RowHeight,
    /// How many described rows of that card come before it.
    pub(super) after_row: usize,
    /// The page's conditional group this row belongs to, exactly like [`RowSpec::group`]: a
    /// page-drawn row that a mode switch hides must be hidden by the SAME canvas group list, or
    /// the page stops following the switch.
    pub(super) group: Option<RowGroup>,
}

/// A page-drawn row's height: a constant, or a measurement of the page input.
///
/// The device row on the Mouse page is why the measured form exists: its height follows the longest
/// caption any connected device can produce, which is only known once the page input exists.
#[derive(Clone, Copy, Debug)]
pub(super) enum RowHeight {
    Fixed(f64),
    Measured(fn(&PageInput, f64) -> f64),
}

impl PartialEq for RowHeight {
    /// Two rows are equal when they resolve to the same height for the same input; comparing the
    /// function pointers themselves would not answer that (and is not what a test means by it).
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (RowHeight::Fixed(a), RowHeight::Fixed(b)) => a == b,
            _ => std::mem::discriminant(self) == std::mem::discriminant(other),
        }
    }
}

impl Eq for RowHeight {}

impl RowHeight {
    /// The height this row takes for `input` at the page's content width.
    ///
    /// The width is a parameter because a caption row's height depends on the column it wraps in,
    /// and that column is derived from the content width (`control_x` moves with it).
    pub(super) fn resolve(&self, input: &PageInput, content_w: f64) -> f64 {
        match self {
            RowHeight::Fixed(height) => *height,
            RowHeight::Measured(measure) => measure(input, content_w),
        }
    }
}

impl From<f64> for RowHeight {
    fn from(height: f64) -> Self {
        RowHeight::Fixed(height)
    }
}

/// What a page supplies to draw the rows it owns. The default draws none, which is every page whose
/// rows are all described.
pub(super) trait PageHooks {
    /// Draw the page's OWN header, before any row.
    ///
    /// A page that draws its header itself (the About page's icon, title, subtitle and hit view)
    /// states its `PageView::header_offset` and draws the block here, then pins it with the
    /// canvas's `view_mark`/`pin_from_mark`. Every other page leaves this empty and gets the shared
    /// page title from the canvas.
    unsafe fn before_page(
        &mut self,
        _canvas: &mut PageCanvas,
        _ui: &mut SettingsUi,
        _target: *mut AnyObject,
    ) -> Result<(), RegisterError> {
        Ok(())
    }

    /// Draw the extra row `spec`, whose origin the canvas has already advanced to.
    ///
    /// `origin_y` is the row's BOTTOM edge in the page document's unflipped coordinates -- the same
    /// origin a described row's frame has -- so a row taller than one line steps DOWN from it
    /// (`origin_y + offset`), and a subview added to the document uses that value directly.
    ///
    /// `SettingsUi` is passed for the same reason `after_page` takes it: a page-drawn control's
    /// value lives in the legacy field its own callback reads.
    #[allow(clippy::too_many_arguments)] // the row's own inputs, one call site
    unsafe fn extra_row(
        &mut self,
        _canvas: &mut PageCanvas,
        _rendered: &mut RenderedPage,
        _ui: &mut SettingsUi,
        _spec: &PageRowSpec,
        _origin_y: f64,
        _height: f64,
        _target: *mut AnyObject,
    ) -> Result<(), RegisterError> {
        Ok(())
    }

    /// Draw what the page owns BELOW its described cards -- its own trailing card, an action band,
    /// a host view -- on the page's own canvas, before the page is finished and published.
    ///
    /// The canvas sequence stays the page's: what is drawn here belongs to the page document, so a
    /// later reflow moves it together with the cards above it.
    /// `SettingsUi` is passed so the page can write the fields its own callbacks read (a card of
    /// its own that is not described at all, such as the clipboard exclusion list) with exactly the
    /// same writes the inline builder made.
    unsafe fn after_page(
        &mut self,
        _canvas: &mut PageCanvas,
        _rendered: &mut RenderedPage,
        _ui: &mut SettingsUi,
        _target: *mut AnyObject,
    ) -> Result<(), RegisterError> {
        Ok(())
    }
}

/// The hooks a page with no self-drawn rows uses.
pub(super) struct NoHooks;
impl PageHooks for NoHooks {}

/// The permission banner a page carries above its rows.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct BannerSpec {
    pub(super) copy_key: &'static str,
    pub(super) action_id: &'static str,
    pub(super) content_h: f64,
    pub(super) gap: f64,
}

/// The action button a [`ControlKind::Value`] row carries beside its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ValueAction {
    /// The action identity the dispatcher resolves to a selector.
    pub(super) action: &'static str,
    /// The button's copy key.
    pub(super) button_key: &'static str,
}

impl ValueAction {
    pub(super) fn new(action: &'static str, button_key: &'static str) -> Self {
        Self { action, button_key }
    }
}

/// How a value row's text is aligned in its box.
///
/// The shipped pages are not uniform: the Finder Menu status line and the About permission rows
/// right-align their value so it sits against the action button, while a lone value (the version
/// line) is aligned naturally. Making this a row field is what keeps the migrated pages looking
/// exactly as they did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum ValueAlign {
    /// The platform's natural alignment (left in every shipped locale).
    #[default]
    Natural,
    Right,
}

/// A row placed in the page: its frame and the frames of the views it draws.
///
/// These are the numbers the page is built with; the canvas then re-places the views from its own
/// walk, so a row hidden later and its cards stay in step with one another.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct RowLayout {
    /// The row's frame in the page document (unflipped: the origin is the row's bottom edge).
    pub(super) frame: BoxRect,
    /// The action button beside a value row, when the row carries one.
    pub(super) action_frame: Option<BoxRect>,
    pub(super) label_frame: BoxRect,
    /// The copy box the label is centered in or aligned to.
    #[allow(dead_code)]
    pub(super) stack_frame: BoxRect,
    /// The row's resulting height (its frame's height).
    #[allow(dead_code)]
    pub(super) height: f64,
    /// The caption's frame for a stacked row.
    pub(super) caption_frame: Option<BoxRect>,
    pub(super) control_frame: BoxRect,
    /// The readout beside a slider, whose frame follows the slider's.
    pub(super) readout_frame: Option<BoxRect>,
    pub(super) control_h: f64,
    pub(super) stacked: bool,
}

/// Apply the page's values to the controls that were built for it.
///
/// Pure with respect to AppKit: it returns what to write, so the refill and the in-place refresh
/// take their numbers from one place.
pub(super) fn update_description(
    view: &PageView,
    values: &HashMap<ControlField, Value>,
) -> Vec<ControlUpdate> {
    let mut updates = Vec::new();
    for row in &view.rows {
        let Some(field) = row.id.field() else {
            continue;
        };
        let Some(value) = values.get(&field) else {
            continue;
        };
        let display = match (&row.control, value) {
            (ControlKind::Slider { readout, .. }, _) => readout.clone(),
            _ => value.describe(),
        };
        updates.push(ControlUpdate {
            field,
            typed: value.clone(),
            value: value.describe(),
            display,
        });
    }
    updates
}

/// One control's registration: the pointer the dispatcher sees, and what it identifies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ControlEntry {
    pub(super) pointer: usize,
    pub(super) id: ControlId,
}

/// Why registering a control was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RegisterError {
    /// The same identity is already registered.
    DuplicateId,
    /// The same pointer is already registered.
    DuplicatePointer,
}

/// The identity table: which control pointer edits which field, filled while the page is built.
///
/// Lifecycle, all of it asserted below:
/// - register and unregister in pairs; rebuilding a page unregisters its entries before
///   registering again;
/// - a duplicate registration, of the same identity or of the same pointer, is REFUSED here and
///   aborts that page's build (the caller discards the registrations it made and leaves the
///   previous page alone);
/// - a hit on a destroyed control's address therefore cannot be answered by a stale entry,
///   because the entry is gone before the view is released.
#[derive(Debug, Default)]
pub(super) struct ControlTable {
    entries: Vec<ControlEntry>,
}

impl ControlTable {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Register one control. The identity is unique across the table, and so is the pointer.
    pub(super) fn register(
        &mut self,
        pointer: *mut AnyObject,
        id: ControlId,
    ) -> Result<(), RegisterError> {
        let pointer = pointer as usize;
        if pointer == 0 {
            return Ok(());
        }
        if self.entries.iter().any(|entry| entry.id == id) {
            return Err(RegisterError::DuplicateId);
        }
        if self.entries.iter().any(|entry| entry.pointer == pointer) {
            return Err(RegisterError::DuplicatePointer);
        }
        self.entries.push(ControlEntry { pointer, id });
        Ok(())
    }

    /// Remove one registration. Returns whether an entry was removed; repeated cleanup is safe.
    pub(super) fn unregister(&mut self, id: &ControlId) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| &entry.id != id);
        self.entries.len() != before
    }

    /// The identity behind a sender pointer (`None` = an unknown sender: the caller keeps its own
    /// fallback, nothing is silently dropped).
    pub(super) fn lookup(&self, pointer: *mut AnyObject) -> Option<ControlId> {
        let pointer = pointer as usize;
        self.entries
            .iter()
            .find(|entry| entry.pointer == pointer)
            .map(|entry| entry.id.clone())
    }

    /// Whether the identity is registered (publish validates against it; the lifecycle tests read
    /// it too).
    pub(super) fn contains_id(&self, id: &ControlId) -> bool {
        self.entries.iter().any(|entry| &entry.id == id)
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Drop every registration: the whole settings window is being torn down.
    pub(super) fn clear(&mut self) {
        self.entries.clear();
    }
}

/// A page's registrations, applied to the shared table only once the whole page is built.
///
/// This is what makes a failed build harmless: the controls exist in the page's view during the
/// build, but the mapping the dispatcher reads is untouched until `commit`. A page that fails
/// half-way drops every registration it made here, so the mapping and the views it names stay
/// together.
#[derive(Debug, Default)]
pub(super) struct PageRegistrations {
    entries: Vec<ControlEntry>,
}

impl PageRegistrations {
    pub(super) fn register(
        &mut self,
        pointer: *mut AnyObject,
        id: ControlId,
    ) -> Result<(), RegisterError> {
        let pointer = pointer as usize;
        if pointer == 0 {
            return Ok(());
        }
        if self.entries.iter().any(|entry| entry.id == id) {
            return Err(RegisterError::DuplicateId);
        }
        if self.entries.iter().any(|entry| entry.pointer == pointer) {
            return Err(RegisterError::DuplicatePointer);
        }
        self.entries.push(ControlEntry { pointer, id });
        Ok(())
    }

    /// Publish the page's registrations, replacing whatever it registered before.
    ///
    /// The page's old entries are removed FIRST (so a control released right after this cannot be
    /// reached through a stale pointer), then the new ones are added. On a refusal every entry this
    /// call added is removed again, so the table keeps the previous mapping exactly as it was —
    /// the page's views and its mapping either both change or neither does.
    pub(super) fn publish(&mut self, table: &mut ControlTable) -> Result<(), RegisterError> {
        let new_entries = std::mem::take(&mut self.entries);
        // Check FIRST, against the mapping exactly as it will be after the page's own old entries
        // are gone: a page that cannot be published must not have disturbed anything, so the
        // previous controls and the previous mapping stay together.
        let mut conflict: Option<RegisterError> = None;
        for entry in &new_entries {
            let collides = table.entries.iter().any(|live| {
                let replaced = new_entries.iter().any(|new| new.id == live.id);
                !replaced && (live.id == entry.id || live.pointer == entry.pointer)
            });
            if collides {
                conflict = Some(if table.contains_id(&entry.id) {
                    RegisterError::DuplicateId
                } else {
                    RegisterError::DuplicatePointer
                });
                break;
            }
        }
        if let Some(conflict) = conflict {
            self.entries = new_entries;
            return Err(conflict);
        }
        for entry in &new_entries {
            table.unregister(&entry.id);
        }
        let mut added: Vec<ControlId> = Vec::with_capacity(new_entries.len());
        for entry in &new_entries {
            if let Err(error) = table.register(entry.pointer as *mut AnyObject, entry.id.clone()) {
                for id in &added {
                    table.unregister(id);
                }
                self.entries = new_entries;
                return Err(error);
            }
            added.push(entry.id.clone());
        }
        Ok(())
    }
}
// MARK: - Layout

/// Page constants the row placement is stated in terms of.
///
/// They are the page's own copy of the design system's numbers, handed in rather than read, so the
/// placement of a row is a function of its inputs only.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct PageMetrics {
    /// Vertical gap a row adds below itself (`PageCanvas::next_row`'s gap term).
    pub(super) row_gap: f64,
    /// Distance from one section cursor to the next (a card boundary's step).
    pub(super) section_step: f64,
    /// The label column's inset inside a card.
    pub(super) label_x: f64,
    /// Control height (the switch and popup controls).
    pub(super) control_h: f64,
    /// Width of a slider's trailing readout, and the gap before it.
    pub(super) readout_w: f64,
    pub(super) readout_gap: f64,
    pub(super) readout_h: f64,
    /// Left inset of a card and of its section heading.
    pub(super) card_inset: f64,
    /// A switch's drawn width and height.
    pub(super) switch_w: f64,
    pub(super) switch_h: f64,
    /// A row action button's width and height.
    pub(super) action_btn_w: f64,
    pub(super) action_btn_h: f64,
}

/// The page's own value column: where a row's LINK or VALUE control is drawn when the design gives
/// that page one (the About page's website, GitHub and version rows).
///
/// The inline builder stated it as `label_x + 145` wide open to the card's far inset, which is what
/// lets a full URL fit; the shared control column is 180pt wide and truncates it.
fn value_column_box(layout: SettingsLayout, content_w: f64) -> (f64, f64) {
    let x = layout.label_x + VALUE_COLUMN_OFFSET;
    // The column ends at the card's LABEL inset, not its outer inset: the inline builder stated it
    // as `content_w - 2 * label_x - 145`, whose right edge is `content_w - label_x`.
    let w = (content_w - layout.label_x - x).max(1.0);
    (x, w)
}

/// Distance from a row's copy column to the page's own value column.
const VALUE_COLUMN_OFFSET: f64 = 145.0;

impl PageMetrics {
    /// The metrics every settings page is laid out with today, read from the design system.
    pub(super) fn for_layout(layout: SettingsLayout) -> Self {
        Self {
            row_gap: layout.row_gap,
            section_step: layout.section_step,
            label_x: layout.label_x,
            control_h: SettingsLayout::CONTROL_H,
            readout_w: super::SLIDER_READOUT_W,
            readout_gap: super::SLIDER_READOUT_GAP,
            readout_h: super::SLIDER_READOUT_H,
            card_inset: super::SETTINGS_DETAIL_INSET,
            switch_w: super::HTML_SWITCH_W,
            switch_h: super::HTML_SWITCH_H,
            action_btn_w: super::ROW_ACTION_BTN_W,
            action_btn_h: super::ROW_ACTION_BTN_H,
        }
    }
}

/// One box the layout hands to the renderer.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub(super) struct BoxRect {
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) w: f64,
    pub(super) h: f64,
}

impl BoxRect {
    pub(super) fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    pub(super) fn rect(&self) -> NSRect {
        NSRect::new(NSPoint::new(self.x, self.y), NSSize::new(self.w, self.h))
    }

    pub(super) fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }
}

/// A card the page draws: the heading and the rounded surface around one run of rows.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct CardRect {
    /// Whether this card is the page's update-flow card: the About Updates card, whose handle the
    /// inline update flow reads. A card is not a control, so this is the one property the renderer
    /// has to know about it.
    pub(super) holds_update_flow: bool,
    /// The card's own index in the page (`RowSpec::card` / `PageRowSpec::card`). The walk uses
    /// THIS, not the card's position in the vector: a page may have a card that holds only rows the
    /// page draws itself, and a positional index would then address the wrong card.
    pub(super) index: usize,
    pub(super) section: &'static str,
    /// The card surface's frame in the document.
    pub(super) frame: BoxRect,
    /// The section heading's frame, above the card's top edge.
    pub(super) header: BoxRect,
}

/// The permission banner a page draws above its rows.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct BannerRect {
    pub(super) strip: BoxRect,
    pub(super) label: BoxRect,
    pub(super) button: BoxRect,
}

/// One position in a page's document: a described row, or a row the page draws itself. The layout
/// and the renderer both walk a page as a sequence of these, so a page that interleaves the two
/// cannot have one of them laid out at a different offset than it is drawn at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum PlacedRow {
    /// Index into `PageView::rows`.
    Described(usize),
    /// Index into `PageView::extra_rows`.
    Drawn(usize),
}

/// Everything `layout_rows` computes for one page: where every row, card and panel sits.
///
/// Coordinates are the page document's (unflipped), and they come from the same walk the canvas
/// uses: the first section cursor, the section steps and the row heights. The renderer hands the
/// boxes to the canvas, which is what keeps a page's build and its later re-flow in step.
#[derive(Clone, Debug, PartialEq, Default)]
pub(super) struct PageLayout {
    /// The page's rows in document order, top to bottom: the ONE sequence the renderer walks.
    pub(super) sequence: Vec<PlacedRow>,
    /// Every page-drawn row's origin and the height the layout reserved for it, in `extra_rows`
    /// order, so drawing one uses the number the layout above it assumed (index, origin, height).
    pub(super) drawn_rows: Vec<(usize, f64, f64)>,
    /// The height each page-drawn row took, in `PageView::extra_rows` order: the number the layout
    /// reserved, so the renderer advances by exactly that instead of measuring a second time.
    pub(super) extra_row_heights: Vec<f64>,
    pub(super) banner: Option<BannerRect>,
    /// The first card's top edge (the first section cursor).
    pub(super) first_card_top: f64,
    /// One entry per row, in document order.
    pub(super) rows: Vec<RowLayout>,
    /// One entry per card, in card order.
    pub(super) cards: Vec<CardRect>,
    /// The top edge of every card, indexed by card.
    pub(super) card_tops: Vec<f64>,
    /// The run of the page's content the canvas has to re-flow: from the first row's origin (or
    /// the first card's top) down to the last row's origin.
    pub(super) content: BoxRect,
}

/// A row's height: its base height, grown when the copy box it carries needs more room.
fn row_height(
    row: &RowSpec,
    measured_subtitle_h: f64,
    _metrics: &PageMetrics,
    input: &LayoutInput,
) -> f64 {
    let base = match row.control {
        ControlKind::Popup { .. } => SettingsLayout::SINGLE_LINE_ROW_H,
        ControlKind::Switch
        | ControlKind::Button { .. }
        | ControlKind::Slider { .. }
        | ControlKind::Link { .. } => input.layout.described_row_h,
        ControlKind::Value { action, .. } => {
            // A value row with a button is a described row (the button makes it two lines tall,
            // like every other row carrying a control).
            if action.is_some() {
                input.layout.described_row_h
            } else {
                SettingsLayout::SINGLE_LINE_ROW_H
            }
        }
    };
    match row.copy {
        RowCopy::Plain => base,
        RowCopy::Captioned => {
            let stack = input.text.title_h + input.text.title_caption_gap + measured_subtitle_h;
            (stack + 19.0).max(base)
        }
    }
}

/// Place a page description in its measured page. The ONLY layout computation: `render_page`
/// applies these frames and never derives a frame of its own.
pub(super) fn layout_rows(
    view: &PageView,
    input: &LayoutInput,
    metrics: &PageMetrics,
) -> PageLayout {
    let layout = input.layout;
    let content_w = input.content_w;
    let doc_top = input.doc_top;
    let text = &input.text;
    // The banner hangs from the document top, right below the header the canvas drew.
    let banner = view.banner.as_ref().map(|spec| {
        // The strip's FINAL parent is the settings content view (the page build hands it over and
        // the window re-parents it), so the caller states its frame in THAT view's coordinates --
        // the same box the inline builder used (banner_parent_frame).
        let strip = BoxRect::new(
            input.banner_frame.x,
            input.banner_frame.y,
            input.banner_frame.w.max(1.0),
            input.banner_frame.h.max(1.0),
        );
        // The label and the button are SUBVIEWS OF THE STRIP, so their boxes are stated in the
        // strip's own coordinates: adding the strip's origin here would double the parent offset.
        BannerRect {
            strip,
            label: BoxRect::new(
                12.0,
                spec.gap + 6.0,
                (strip.w - 160.0).max(1.0),
                (spec.content_h - 12.0).max(1.0),
            ),
            button: BoxRect::new(
                strip.w - 150.0,
                spec.gap + (spec.content_h - 28.0) / 2.0,
                140.0,
                28.0,
            ),
        }
    });
    let banner_h = banner.as_ref().map_or(0.0, |b| b.strip.h);
    // The first card hangs from the first section cursor, one banner further down when there is
    // one (the banner is a sibling of the page, not part of the header block).
    // ONE walk drives the whole page: each card's described rows and the rows the page draws
    // itself, in document order. The card surfaces, the described rows' frames and the heights the
    // renderer advances by therefore all come from one sequence, so a page that mixes the two (the
    // Mouse page: cards 1 and 3 hold only page-drawn rows) cannot have a row laid out at a
    // different offset than it is drawn at.
    let highest_card = view
        .rows
        .iter()
        .map(|row| row.card)
        .chain(view.extra_rows.iter().map(|extra| extra.card))
        .max()
        .unwrap_or(0);
    let sequence = page_sequence(view);
    let mut cursor = doc_top - input.header_offset - banner_h;
    let first_card_top = cursor;
    let mut card_tops = vec![first_card_top];
    // Each card's lowest edge, described or page-drawn: the surface must cover both kinds of row.
    let mut card_bottoms = vec![first_card_top; highest_card + 1];
    // Every page-drawn row's origin and the height reserved for it, in extra_rows order: the
    // renderer draws it at exactly that origin with exactly that height.
    let mut drawn_rows: Vec<(usize, f64, f64)> =
        vec![(0, first_card_top, 0.0); view.extra_rows.len()];
    let mut placed_rows: Vec<Option<RowLayout>> = vec![None; view.rows.len()];
    for placed in sequence.iter().copied() {
        let card = match placed {
            PlacedRow::Described(index) => view.rows[index].card,
            PlacedRow::Drawn(index) => view.extra_rows[index].card,
        };
        // A card boundary at the page's start has no step: the first card IS the first cursor.
        while card_tops.len() <= card {
            cursor -= metrics.section_step;
            card_tops.push(cursor);
        }
        cursor -= metrics.row_gap;
        match placed {
            PlacedRow::Drawn(index) => {
                let height = input.extra_row_heights.get(index).copied().unwrap_or(0.0);
                cursor -= height;
                drawn_rows[index] = (index, cursor, height);
                card_bottoms[card] = card_bottoms[card].min(cursor);
            }
            PlacedRow::Described(index) => {
                let row = &view.rows[index];
                let subtitle_h = text.subtitle_heights.get(index).copied().unwrap_or(0.0);
                let height = row_height(row, subtitle_h, metrics, input);
                cursor -= height;
                card_bottoms[card] = card_bottoms[card].min(cursor);

                // The card view spans the page's whole content width (x = 0, width = content_w),
                // so a row's copy column starts at the label inset INSIDE the card -- the same
                // \`label_x\` the inline builder used. Adding the card inset again indented every
                // row's text 28pt further right than the inline pages drew it.
                let copy_x = metrics.label_x;
                // A captioned row's text block is WIDER than a plain row's single label: the inline
                // builder measured and drew it up to the control column's leading edge
                // (`content_w - label_x * 2 - 58`), which is what keeps the design's two-line
                // description on two lines. Measuring it at the plain label width wrapped the same
                // copy to three lines and grew the row, so the page stopped matching the design.
                let copy_w = match row.copy {
                    RowCopy::Plain => layout.label_w.max(1.0),
                    RowCopy::Captioned => captioned_text_width(layout, content_w, row),
                };
                let (label, caption, stacked) = match row.copy {
                    RowCopy::Plain => (
                        BoxRect::new(
                            copy_x,
                            cursor + (height - text.title_h) / 2.0,
                            copy_w,
                            text.title_h,
                        ),
                        None,
                        false,
                    ),
                    RowCopy::Captioned => {
                        let stack_h = text.title_h + text.title_caption_gap + subtitle_h;
                        let caption_y = cursor + (height - stack_h.max(0.0)) / 2.0;
                        (
                            BoxRect::new(
                                copy_x,
                                caption_y + subtitle_h + text.title_caption_gap,
                                copy_w,
                                text.title_h,
                            ),
                            Some(BoxRect::new(copy_x, caption_y, copy_w, subtitle_h)),
                            true,
                        )
                    }
                };
                let (control, readout) = match &row.control {
                    ControlKind::Switch => {
                        let right_x = layout.control_x + layout.control_w;
                        // The switch is centred in the row's BAND, stacked copy or not: the inline
                        // builder centred every captioned row's control (`center_control`), and
                        // pinning it to the band's top edge put it 22pt above the row's middle on
                        // every captioned row.
                        let y = cursor + (height - metrics.control_h) / 2.0;
                        (
                            BoxRect::new(
                                right_x - metrics.switch_w,
                                y + (metrics.control_h - metrics.switch_h) / 2.0,
                                metrics.switch_w,
                                metrics.switch_h,
                            ),
                            None,
                        )
                    }
                    ControlKind::Popup { .. } => (
                        BoxRect::new(
                            layout.control_x,
                            cursor + (height - metrics.control_h) / 2.0,
                            layout.control_w,
                            metrics.control_h,
                        ),
                        None,
                    ),
                    // A hyperlink sits at the control column's leading edge, like a value: it is text, not a
                    // right-aligned control.
                    ControlKind::Link { .. } => {
                        let (x, w) = if row.value_column {
                            value_column_box(layout, content_w)
                        } else {
                            (layout.control_x, layout.control_w)
                        };
                        (
                            BoxRect::new(
                                x,
                                cursor + (height - metrics.control_h) / 2.0,
                                w,
                                metrics.control_h,
                            ),
                            None,
                        )
                    }
                    ControlKind::Slider { .. } => {
                        let h = layout.row_h;
                        let w =
                            (layout.control_w - metrics.readout_w - metrics.readout_gap).max(1.0);
                        let y = cursor + (height - h) / 2.0;
                        (
                            BoxRect::new(layout.control_x, y, w, h),
                            Some(BoxRect::new(
                                layout.control_x + w + metrics.readout_gap,
                                y + (h - metrics.readout_h) / 2.0,
                                metrics.readout_w,
                                metrics.readout_h,
                            )),
                        )
                    }
                    ControlKind::Button { .. } => {
                        let right_x = layout.control_x + layout.control_w;
                        // A row's action button is centred in the band, like every other control:
                        // the inline builder centred it, and pinning it to the band's top edge put
                        // a captioned row's button 12pt above the row's middle.
                        (
                            BoxRect::new(
                                right_x - metrics.action_btn_w,
                                cursor + (height - metrics.action_btn_h) / 2.0,
                                metrics.action_btn_w,
                                metrics.action_btn_h,
                            ),
                            None,
                        )
                    }
                    // A read-only value is drawn where the control column puts a control, left-aligned so a
                    // status line reads as text rather than as a disabled button. With an action beside it,
                    // the value takes the room the button leaves.
                    ControlKind::Value { action, .. } => {
                        let y = cursor + (height - metrics.control_h) / 2.0;
                        // A page with its own value column (About) draws its read-only values
                        // there: that column is wider than the shared control column, which is
                        // what lets a version string or a URL read in full.
                        let (column_x, column_w) = if row.value_column && action.is_none() {
                            value_column_box(layout, content_w)
                        } else {
                            (layout.control_x, layout.control_w)
                        };
                        match action {
                            None => (BoxRect::new(column_x, y, column_w, metrics.control_h), None),
                            Some(_) => (
                                BoxRect::new(
                                    layout.control_x,
                                    y,
                                    (layout.control_w
                                        - super::components::ROW_ACTION_BTN_W
                                        - VALUE_ACTION_GAP)
                                        .max(1.0),
                                    metrics.control_h,
                                ),
                                None,
                            ),
                        }
                    }
                };
                let action_frame = match &row.control {
                    ControlKind::Value {
                        action: Some(_), ..
                    } => Some(BoxRect::new(
                        layout.control_x + layout.control_w - super::components::ROW_ACTION_BTN_W,
                        cursor + (height - super::components::ROW_ACTION_BTN_H) / 2.0,
                        super::components::ROW_ACTION_BTN_W,
                        super::components::ROW_ACTION_BTN_H,
                    )),
                    _ => None,
                };
                let _ = &action_frame;
                placed_rows[index] = Some(RowLayout {
                    frame: BoxRect::new(0.0, cursor, content_w, height),
                    action_frame,
                    height,
                    label_frame: label,
                    stack_frame: label,
                    caption_frame: caption,
                    control_frame: control,
                    readout_frame: readout,
                    control_h: control.h,
                    stacked,
                });
            }
        }
    }
    // A card's surface spans from its section cursor down past its last row's bottom edge.
    // Every card index gets a section cursor, including a card that drew no DESCRIBED row: the
    // canvas walks sections, and a page may put rows it draws itself into a card of their own.
    let highest_card = view
        .rows
        .iter()
        .map(|row| row.card)
        .chain(view.extra_rows.iter().map(|extra| extra.card))
        .max()
        .unwrap_or(0);
    while card_tops.len() <= highest_card {
        let next = card_tops[card_tops.len() - 1] - metrics.section_step;
        card_tops.push(next);
    }
    let mut cards = Vec::new();
    for (card_index, card_top) in card_tops.iter().enumerate().take(highest_card + 1) {
        // The rows of this card, described or page-drawn, are what the surface spans.
        let described: Vec<usize> = view
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.card == card_index)
            .map(|(index, _)| index)
            .collect();
        let section = described
            .first()
            .map(|index| view.rows[*index].section)
            // A card with no described row names itself through its first page-drawn row's
            // SECTION -- the heading key the page opened it under. Falling back to the row's own
            // key printed the internal identifier ("device") where the heading belonged.
            .or_else(|| {
                view.extra_rows
                    .iter()
                    .find(|extra| extra.card == card_index)
                    .map(|extra| extra.section)
            })
            .unwrap_or("");
        let top = *card_top;
        // The card's lowest row, described or page-drawn: a card may hold only rows the page draws
        // itself, and its surface has to cover them.
        let bottom = card_bottoms
            .get(card_index)
            .copied()
            .unwrap_or(top)
            .min(top);
        cards.push(CardRect {
            holds_update_flow: view.extra_rows.iter().any(|extra| {
                extra.card == card_index
                    && extra.key == crate::settings::page_view::about::ABOUT_UPDATE_BAND
            }),
            index: card_index,
            section,
            frame: BoxRect::new(0.0, bottom, content_w, (top - bottom).max(0.0)),
            header: BoxRect::new(
                metrics.card_inset,
                layout.section_header_y(top),
                content_w - 2.0 * metrics.card_inset,
                text.heading_h,
            ),
        });
    }
    // The described rows, back in `PageView::rows` order: that is the order the renderer indexes,
    // the search index reads and a canvas lookup names.
    let rows: Vec<RowLayout> = placed_rows
        .into_iter()
        .map(|row| row.expect("every described row was laid out"))
        .collect();
    let content_top = first_card_top;
    let content_bottom = rows
        .last()
        .map(|row| row.frame.y)
        .into_iter()
        .chain(drawn_rows.iter().map(|(_, origin, _)| *origin))
        .fold(content_top, f64::min);
    PageLayout {
        sequence,
        drawn_rows,
        extra_row_heights: input.extra_row_heights.clone(),
        banner,
        first_card_top,
        rows,
        cards,
        card_tops,
        content: BoxRect::new(0.0, content_bottom, content_w, content_top - content_bottom),
    }
}
/// Each card's row range (start, end-exclusive), in card order: one range per run of rows that
/// share a card index. A page that opens a card and draws no row in it draws no card.
/// The canvas row a described row lands in.
///
/// A described row takes one canvas row; a page-drawn row (`extra_row`) takes another. Every
/// described row after one of those is shifted by the count of page-drawn rows opened before it, so
/// this is the index the search index and any canvas lookup must use.
/// The canvas row a described row lands in.
///
/// The canvas opens one row per POSITION in the page's `PlacedRow` sequence, so the canvas row is
/// the row's position there -- not its index among described rows. A page whose earlier cards hold
/// rows it draws itself (Mouse: three device-card rows before its second described row) would
/// otherwise map every later described row to the wrong canvas row, and the search would scroll to
/// a page-drawn row instead of the match.
pub(super) fn canvas_row_of_described_row(view: &PageView, row: usize) -> usize {
    canvas_rows(view).get(row).copied().unwrap_or(row)
}

/// The canvas row of every described row, in `PageView::rows` order, derived from the ONE sequence
/// the layout and the renderer walk.
pub(super) fn canvas_rows(view: &PageView) -> Vec<usize> {
    let mut rows: Vec<Option<usize>> = vec![None; view.rows.len()];
    for (position, placed) in page_sequence(view).iter().enumerate() {
        if let PlacedRow::Described(index) = placed {
            rows[*index] = Some(position);
        }
    }
    rows.into_iter()
        .map(|row| row.expect("every described row is in the sequence"))
        .collect()
}

/// The page's rows in document order: every card's described rows and the rows the page draws
/// itself, exactly as `layout_rows` and `render_page` walk them.
pub(super) fn page_sequence(view: &PageView) -> Vec<PlacedRow> {
    let highest_card = view
        .rows
        .iter()
        .map(|row| row.card)
        .chain(view.extra_rows.iter().map(|extra| extra.card))
        .max()
        .unwrap_or(0);
    let mut sequence: Vec<PlacedRow> = Vec::with_capacity(view.rows.len() + view.extra_rows.len());
    for card_index in 0..=highest_card {
        let mut drawn_in_card = 0usize;
        for (index, row) in view.rows.iter().enumerate() {
            if row.card != card_index {
                continue;
            }
            for (extra_index, extra) in view.extra_rows.iter().enumerate() {
                if extra.card == card_index && extra.after_row == drawn_in_card {
                    sequence.push(PlacedRow::Drawn(extra_index));
                }
            }
            sequence.push(PlacedRow::Described(index));
            drawn_in_card += 1;
        }
        // A page-drawn row declared after the card's last described row still belongs to it.
        for (extra_index, extra) in view.extra_rows.iter().enumerate() {
            if extra.card == card_index && extra.after_row >= drawn_in_card {
                sequence.push(PlacedRow::Drawn(extra_index));
            }
        }
    }
    sequence
}
// MARK: - Row constructors

/// Build rows card by card: `card` opens a card (its heading copy key), and the rows that follow
/// belong to it. The first row of every card starts the card's block; each later row in the card
/// draws a divider above itself.
struct RowWriter {
    page: SettingsPageId,
    initial_section: String,
    card: usize,
    section: &'static str,
    rows: Vec<RowSpec>,
    extra: Vec<PageRowSpec>,
}

impl RowWriter {
    fn new(page: SettingsPageId, initial_section: String) -> Self {
        Self {
            page,
            initial_section,
            card: 0,
            section: "",
            rows: Vec::new(),
            extra: Vec::new(),
        }
    }

    /// Open a card: its rows start after the section step.
    fn card(&mut self, section: &'static str) {
        self.card += 1;
        self.section = section;
    }

    fn in_first_card(&mut self, section: &'static str) {
        self.section = section;
    }

    fn push(&mut self, mut row: RowSpec) {
        debug_assert!(!self.section.is_empty(), "row pushed outside a card");
        row.card = self.card;
        row.section = self.section;
        row.separator_above = self.rows.last().is_some_and(|last| last.card == self.card);
        let header = if self.section.is_empty() {
            self.initial_section.clone()
        } else {
            t(self.section)
        };
        row.search = row.search_entry(&header);
        self.rows.push(row);
    }

    /// Declare a row the PAGE draws itself, after the card's described rows so far.
    ///
    /// The row stays inside the current card, so the described rows that follow still land in the
    /// same card and the card closes where it would have closed anyway.
    fn extra_row(&mut self, key: &'static str, height: f64) {
        self.extra_row_in_group(key, height, None);
    }

    /// Declare a page-drawn row that belongs to a conditional group: the canvas hides it with the
    /// same `set_group_visible` the described rows of that group use.
    fn extra_row_in_group(&mut self, key: &'static str, height: f64, group: Option<RowGroup>) {
        let after = self.rows.iter().filter(|row| row.card == self.card).count();
        self.extra.push(PageRowSpec {
            card: self.card,
            section: self.section,
            key,
            height: RowHeight::Fixed(height),
            after_row: after,
            group,
        });
    }

    /// Declare a row the page draws itself whose height depends on the page input (the Mouse page's
    /// device row: its height follows the longest caption any connected device can produce).
    fn extra_row_measured(&mut self, key: &'static str, measure: fn(&PageInput, f64) -> f64) {
        let after = self.rows.iter().filter(|row| row.card == self.card).count();
        self.extra.push(PageRowSpec {
            card: self.card,
            section: self.section,
            key,
            height: RowHeight::Measured(measure),
            after_row: after,
            group: None,
        });
    }

    fn finish(self) -> PageView {
        PageView {
            page: self.page,
            title_key: settings_page_identity(settings_page_index(self.page)).title_key(),
            rows: self.rows,
            initial_section: self.initial_section,
            banner: None,
            header_offset: None,
            extra_rows: self.extra,
        }
    }
}

/// One row's fields, before the page's card bookkeeping fills them in.
struct RowDraft {
    label_key: &'static str,
    desc_key: Option<&'static str>,
    caption_key: Option<&'static str>,
    control: ControlKind,
    id: ControlId,
    copy: RowCopy,
    value_column: bool,
    wide_caption: bool,
    depends_on: Option<ControlField>,
    group: Option<RowGroup>,
    value: Value,
}

impl RowDraft {
    fn new(label_key: &'static str, control: ControlKind, id: ControlId, value: Value) -> Self {
        Self {
            label_key,
            desc_key: None,
            caption_key: None,
            control,
            id,
            copy: RowCopy::Plain,
            value_column: false,
            wide_caption: false,
            depends_on: None,
            group: None,
            value,
        }
    }

    /// Draw this row's LINK or VALUE control in the page's own value column.
    fn value_column(mut self) -> Self {
        self.value_column = true;
        self
    }

    /// Give this captioned row the WIDE text block (up to the card's far inset). The design uses
    /// it on the one row whose copy must fit two lines beside a right-aligned switch.
    fn wide_caption(mut self) -> Self {
        self.wide_caption = true;
        self
    }

    fn desc(mut self, key: &'static str) -> Self {
        self.desc_key = Some(key);
        self
    }

    fn caption(mut self, key: &'static str) -> Self {
        self.copy = RowCopy::Captioned;
        self.caption_key = Some(key);
        self
    }

    #[allow(dead_code)]
    fn depends_on(mut self, field: ControlField) -> Self {
        self.depends_on = Some(field);
        self
    }

    /// A `Value::Dynamic` control: its content is data the caller computed, not a locale key.
    fn dynamic(mut self, text: impl Into<String>) -> Self {
        self.value = Value::Dynamic(text.into());
        self
    }

    fn group(mut self, group: RowGroup) -> Self {
        self.group = Some(group);
        self
    }

    fn into_spec(self) -> RowSpec {
        let options = match &self.control {
            ControlKind::Popup { options } => options.clone(),
            _ => Vec::new(),
        };
        RowSpec {
            card: 0,
            section: "",
            label_key: self.label_key,
            desc_key: self.desc_key,
            caption_key: self.caption_key,
            control: self.control,
            value_column: self.value_column,
            wide_caption: self.wide_caption,
            id: self.id,
            copy: self.copy,
            depends_on: self.depends_on,
            separator_above: false,
            group: self.group,
            value: self.value,
            search: SearchRow {
                label: t(self.label_key),
                desc: self.desc_key.map(t),
                section: String::new(),
                options,
            },
        }
    }
}

fn switch(label_key: &'static str, field: ControlField, on: bool) -> RowDraft {
    RowDraft::new(
        label_key,
        ControlKind::Switch,
        ControlId::Field(field),
        Value::Switch(on),
    )
}

fn popup(
    label_key: &'static str,
    options: &[String],
    field: ControlField,
    selected: usize,
) -> RowDraft {
    RowDraft::new(
        label_key,
        ControlKind::Popup {
            options: options.to_vec(),
        },
        ControlId::Field(field),
        Value::Index(selected),
    )
}

#[allow(clippy::too_many_arguments)]
fn slider(
    label_key: &'static str,
    field: ControlField,
    range: (i64, i64, i64),
    value: i64,
    reset: Option<f64>,
    readout: String,
) -> RowDraft {
    RowDraft::new(
        label_key,
        ControlKind::Slider {
            min: range.0,
            max: range.1,
            step: range.2,
            reset,
            readout,
        },
        ControlId::Field(field),
        Value::Slider(value),
    )
}

fn button(label_key: &'static str, action: &'static str, title_key: &'static str) -> RowDraft {
    RowDraft::new(
        label_key,
        ControlKind::Button { title_key },
        ControlId::Action(action),
        Value::None,
    )
}

// MARK: - The update description

/// The pure "field -> value" description both the refill and the in-place refresh write.
pub(super) fn page_update_description(
    input: &PageInput,
    pages: &[SettingsPageId],
) -> Vec<ControlUpdate> {
    let mut updates = Vec::new();
    for page in pages {
        let view = view_for(*page, input);
        let values: HashMap<ControlField, Value> = view
            .rows
            .iter()
            .filter_map(|row| row.field().map(|field| (field, row.value.clone())))
            .collect();
        updates.extend(update_description(&view, &values));
    }
    updates
}

// MARK: - Rendering

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
pub(super) struct RenderedPage {
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
fn dedicated_selector(field: Option<ControlField>) -> Sel {
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
        _ => sel!(handleExportLogs:),
    }
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
unsafe fn render_row(
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
        let separator = SettingsRow::separator_above_row(
            parent,
            placed.frame.y,
            placed.frame.h,
            placed.frame.w,
        );
        canvas.place_pending_view(separator, separator_frame(placed));
    }
    let title = t(row.label_key);
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
        ControlKind::Switch => {
            let on = matches!(row.value, Value::Switch(true));
            SettingsControl::switch(
                placed.control_frame.x + placed.control_frame.w,
                placed.control_frame.y,
                placed.control_frame.h,
                on,
            )
        }
        ControlKind::Popup { options } => {
            let refs: Vec<&str> = options.iter().map(String::as_str).collect();
            let selected = match row.value {
                Value::Index(index) => index,
                _ => 0,
            };
            SettingsControl::popup(
                placed.control_frame.x,
                placed.control_frame.y,
                placed.control_frame.w,
                placed.control_frame.h,
                &refs,
                selected,
            )
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
    let control_box = match row.control {
        // Every value row, with or without an action: the ACTION BUTTON is a second view drawn
        // below in its own branch, so fitting the text does not move it.
        ControlKind::Value { .. } => {
            SettingsRow::center_readonly_text_control(control, placed.frame.y, placed.frame.h);
            let frame: NSRect = msg_send![control, frame];
            BoxRect::new(
                frame.origin.x,
                frame.origin.y,
                frame.size.width,
                frame.size.height,
            )
        }
        _ => placed.control_frame,
    };
    canvas.place_pending_view(control, control_box.rect());
    // The shared association contract: VoiceOver label + the ROW_LABELS pairing the freeze lists
    // use to dim the row's label and give it the disabled tooltip.
    SettingsRow::register_label(title_label, control);
    if !matches!(row.control, ControlKind::Button { .. }) {
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

/// The divider a row draws above itself: the row's top edge, full bleed across the page.
pub(super) fn separator_frame(placed: &RowLayout) -> NSRect {
    NSRect::new(
        NSPoint::new(placed.frame.x, placed.frame.y + placed.frame.h),
        NSSize::new(placed.frame.w.max(1.0), 1.0),
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
pub(super) unsafe fn render_page(
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

// MARK: - Refilling the migrated pages

/// Write the migrated pages' controls from the shared update description of `config`.
///
/// This is the computation both the refill (`load_settings_from`) and the in-place refresh
/// (`refresh_switcher_keystroke_and_mouse_controls_from_config`) consume. What the callers decide
/// is the POLICY around it (the page being opened, the page being visible, a shortcut capture in
/// progress, `MOUSE_PAGE_DIRTY`); this function only computes and writes.
pub(super) unsafe fn refresh_migrated_pages(
    ui: &SettingsUi,
    input: &PageInput,
    pages: &[SettingsPageId],
) {
    let updates = page_update_description(input, pages);
    for page in pages {
        if let Some(rendered) = ui.migrated_pages[settings_page_index(*page)].as_ref() {
            apply_page_updates(rendered, &updates);
        }
    }
}

// MARK: - Building one page

/// What a migrated page replaces when its description is rendered: its rows' identities.
/// The page's description, for the pages that have one.
fn view_for(page: SettingsPageId, input: &PageInput) -> PageView {
    match page {
        SettingsPageId::Switcher => switcher_page_view(input),
        SettingsPageId::General => general_page_view(input),
        SettingsPageId::KeyBindings => key_bindings_page_view(input),
        SettingsPageId::KeystrokeDisplay => keystroke_display_page_view(input),
        SettingsPageId::FinderMenu => finder_menu_page_view(input),
        SettingsPageId::Clipboard => clipboard_page_view(input),
        SettingsPageId::Mouse => mouse_page_view(input),
        SettingsPageId::About => about_page_view(input),
    }
}

/// The layout inputs a page is built with. The ONE place a page's layout inputs are made, so the
/// wiring assertion drives exactly what `build_page` drives: the banner's box from the page
/// context (`page_x`, the VISIBLE `page_viewport_h`), the rows' origin from the document height,
/// and the header the canvas measured.
#[allow(clippy::too_many_arguments)] // one call site, one meaning per argument
pub(super) fn page_layout_input(
    view: &PageView,
    input: &PageInput,
    content_w: f64,
    layout: SettingsLayout,
    doc_h: f64,
    header_offset: f64,
    page_x: f64,
    page_viewport_h: f64,
) -> LayoutInput {
    LayoutInput {
        content_w,
        doc_top: doc_h,
        header_offset,
        banner_frame: BannerFrame::of(banner_parent_frame(
            page_x,
            page_viewport_h,
            content_w,
            banner_height(input.permission_migration_copy),
        )),
        layout,
        // A caption is drawn inside the LABEL column, not across the page: measuring it at the page
        // width under-reports its wrapped height -- a two-line caption gets one line's box, and the
        // control on the row below overlaps it. The inline builder measured at exactly the width it
        // drew at; this is the same number, taken from the layout that draws it.
        text: MeasuredText::measure(view, layout, content_w),
        extra_row_heights: view
            .extra_rows
            .iter()
            .map(|extra| extra.height.resolve(input, content_w))
            .collect(),
    }
}
/// The banner strip's rectangle for its FINAL parent, the settings content view.
///
/// The strip is created inside the page document and `finalize_settings_pages` re-parents it to
/// the content view, so its box is stated against that parent and follows the inline builder's
/// geometry exactly: `page_x` is the detail pane's horizontal origin, and the strip hangs from the
/// top of the VISIBLE page area (`page_viewport_h`) by the page header's top padding and its own
/// height. Both inputs come from the build context, which is the only place that knows them.
pub(super) fn banner_parent_frame(
    page_x: f64,
    page_viewport_h: f64,
    content_w: f64,
    banner_h: f64,
) -> NSRect {
    NSRect::new(
        NSPoint::new(
            page_x,
            page_viewport_h - SettingsPageHeader::TOP_PADDING - banner_h,
        ),
        NSSize::new(content_w.max(1.0), banner_h.max(1.0)),
    )
}

/// The gap the banner reserves below its content strip (the strip plus this gap is the room the
/// page keeps clear so the banner never covers the title or the first card).
pub(super) const BANNER_GAP: f64 = 12.0;

/// The banner strip's height for the current state: the permission-migration copy is taller than
/// the ordinary warning.
/// Clearance between a value row's text and the action button beside it.
pub(super) const VALUE_ACTION_GAP: f64 = 8.0;

pub(super) fn banner_height(permission_migration_copy: bool) -> f64 {
    let content_h = if permission_migration_copy {
        60.0
    } else {
        48.0
    };
    // The strip and the gap below it are both reserved, so the banner cannot cover the title or the
    // first card.
    content_h + BANNER_GAP
}

/// Whether the description declares the same identity twice. Pure: the description alone says it,
/// so `build_page` can refuse a page before a single view is created and a first build's failure
/// leaves the page empty instead of leaving half-built controls behind.
pub(super) fn description_identity_conflict(view: &PageView) -> Result<(), RegisterError> {
    let mut seen: Vec<&ControlId> = Vec::with_capacity(view.rows.len());
    for row in &view.rows {
        if seen.contains(&&row.id) {
            return Err(RegisterError::DuplicateId);
        }
        seen.push(&row.id);
    }
    Ok(())
}

/// Build one migrated page from its description and publish its controls.
///
/// Lifecycle (the rules the plan requires):
/// - the description is validated and rendered against the new canvas before anything the page
///   previously published is touched;
/// - a duplicate identity or pointer aborts the build: the registrations made so far are dropped
///   (so the table never holds half a page), the new views are taken out of the document again,
///   and the caller gets Err to fall back to the last usable state -- the old views and the old
///   mapping stay together;
/// - a first build has no previous state: on failure the page is left empty and the failure is
///   logged, with nothing published;
/// - on success the table drops the page's old entries BEFORE the caller releases the old views.
#[allow(clippy::too_many_arguments)] // the page's own inputs, one call site per page
pub(super) unsafe fn build_page(
    ui: &mut SettingsUi,
    page: SettingsPageId,
    document: *mut AnyObject,
    content_w: f64,
    layout: SettingsLayout,
    doc_h: f64,
    clip_height: f64,
    page_x: f64,
    page_viewport_h: f64,
    input: &PageInput,
    target: *mut AnyObject,
    hooks: &mut dyn PageHooks,
) -> Result<BuiltPage, RegisterError> {
    let view = view_for(page, input);
    // Identity is a property of the DESCRIPTION, so a duplicate is found before a single view is
    // created: a refused page leaves the document and the previous mapping untouched, and a first
    // build's failure leaves the page empty instead of leaving half-built views behind.
    if let Err(error) = description_identity_conflict(&view) {
        log_info!("[settings] page description declares a duplicate identity: {error:?}");
        return Err(error);
    }
    // A page that draws its own header states where its rows start; every other page gets the
    // shared page title from the canvas, which measures it.
    let mut canvas = match view.header_offset {
        Some(offset) => PageCanvas::new_at(document, content_w, layout, offset, doc_h, clip_height),
        None => PageCanvas::new(
            document,
            content_w,
            layout,
            &t(view.title_key),
            doc_h,
            clip_height,
        ),
    };
    // A page that reserves a band above its cards (the permission banner) hands it to the canvas
    // first: its rows are laid out below that band, and a canvas that started at the document edge
    // would place every card surface one band higher than the rows inside it.
    if view.banner.is_some() {
        canvas.reserve_top_band(banner_height(input.permission_migration_copy));
    }
    // A page that draws its own header draws it now: the canvas exists (so `view_mark`/`pin_from_mark`
    // work) and no row has been placed yet, which is what the header block is pinned above.
    hooks.before_page(&mut canvas, ui, target)?;
    // The banner's box comes from the page context (the detail pane's inset and the VISIBLE
    // viewport), not from the page document -- its parent is the settings content view.
    let layout_input = page_layout_input(
        &view,
        input,
        content_w,
        layout,
        // The row origins hang from the document's TOP EDGE, and the canvas anchors to the
        // document's ACTUAL height (`SettingsPageHeader::attach` reads the frame): when the window
        // grew the document to the viewport, the provisional `doc_h` is shorter and every row would
        // land lower than the canvas puts it. The canvas reports the top it used.
        doc_h,
        canvas.header_offset(),
        page_x,
        page_viewport_h,
    );
    let metrics = PageMetrics::for_layout(layout);
    let page_layout = layout_rows(&view, &layout_input, &metrics);
    let mut rendered = match render_page(
        &mut canvas,
        target,
        &view,
        &page_layout,
        &metrics,
        ui,
        hooks,
    ) {
        Ok(rendered) => rendered,
        Err(error) => {
            // A duplicate was refused while building: nothing has been published, and the page's
            // half-built views are dropped with it (the caller falls back to the previous state).
            log_info!(
                "[settings] page build refused a duplicate ({error:?}); the previous state is kept"
            );
            return Err(error);
        }
    };
    // The page's own handles the inline update flow reads. They are bound HERE because `ui` is
    // the build's, and the page hook that draws the band cannot reach it after the card is built.
    if !rendered.update_card.is_null() {
        ui.update_card = rendered.update_card;
    }
    if let Err(error) = rendered.registrations.publish(&mut ui.control_table) {
        // Nothing may look alive without being wired: take the half-built page out again.
        log_info!("[settings] page build aborted ({error:?}); the previous state is kept");
        let _: () = msg_send![document, removeFromSuperview];
        return Err(error);
    }
    // The rows are built; hand the page's layout owner its final geometry. Every page's document
    // is settled by `finalize_settings_pages` once all pages exist, so this must not run the
    // restore-control pass here.
    // Whatever the page owns below its described cards: its own trailing card, an action band, a
    // host view. It runs before `finish` so the content height accounts for it.
    hooks.after_page(&mut canvas, &mut rendered, ui, target)?;
    let content_bottom = canvas.finish();
    Ok(BuiltPage {
        view,
        rendered,
        canvas,
        content_bottom,
    })
}

/// One migrated page, built: its description, its views and its layout owner.
pub(super) struct BuiltPage {
    pub(super) view: PageView,
    pub(super) rendered: RenderedPage,
    pub(super) canvas: PageCanvas,
    pub(super) content_bottom: f64,
}

/// A `SettingsUi` with no views behind it: the smoke runner builds pages on a detached
/// document to check what the renderer places, and the bindings it writes there are
/// discarded with it. Every field is empty, so a page hook writing one is harmless.
pub(crate) unsafe fn headless_settings_ui() -> SettingsUi {
    SettingsUi {
        permission_warning_label: std::ptr::null_mut(),
        permission_warning_button: std::ptr::null_mut(),
        page_viewport_h: 0.0,
        window: std::ptr::null_mut(),
        settings_search: sidebar::SettingsSearch::EMPTY,
        sidebar_highlight: std::ptr::null_mut(),
        sidebar_buttons: [std::ptr::null_mut(); SETTINGS_PAGE_COUNT],
        page_views: [std::ptr::null_mut(); SETTINGS_PAGE_COUNT],
        about_subtitle: std::ptr::null_mut(),
        accessibility_permission_status: std::ptr::null_mut(),
        accessibility_permission_button: std::ptr::null_mut(),
        screen_recording_permission_status: std::ptr::null_mut(),
        keychain_access_status: std::ptr::null_mut(),
        keychain_access_button: std::ptr::null_mut(),
        theme: std::ptr::null_mut(),
        glass_style: std::ptr::null_mut(),
        panel_material: std::ptr::null_mut(),
        corner_radius: std::ptr::null_mut(),
        thumbnails_enabled: std::ptr::null_mut(),
        focused_thumbnail_prewarm: std::ptr::null_mut(),
        show_app_name_in_cards: std::ptr::null_mut(),
        thumbnail_size: std::ptr::null_mut(),
        card_text_size: std::ptr::null_mut(),
        card_text_size_value_label: std::ptr::null_mut(),
        clipboard_image_rows: std::ptr::null_mut(),
        clipboard_image_rows_value_label: std::ptr::null_mut(),
        status_bar_text_size: std::ptr::null_mut(),
        status_bar_text_size_value_label: std::ptr::null_mut(),
        show_footer_title: std::ptr::null_mut(),
        modifier: std::ptr::null_mut(),
        locale: std::ptr::null_mut(),
        panel_close_enabled: std::ptr::null_mut(),
        panel_minimize_enabled: std::ptr::null_mut(),
        panel_hide_enabled: std::ptr::null_mut(),
        panel_quit_enabled: std::ptr::null_mut(),
        show_minimized: std::ptr::null_mut(),
        show_hidden_app_windows: std::ptr::null_mut(),
        show_other_desktops: std::ptr::null_mut(),
        windows_enabled: std::ptr::null_mut(),
        overlay_position: std::ptr::null_mut(),
        activation_mode: std::ptr::null_mut(),
        keystroke_display_enabled: std::ptr::null_mut(),
        keystroke_display_mode: std::ptr::null_mut(),
        keystroke_display_tap_level: std::ptr::null_mut(),
        keystroke_display_position: std::ptr::null_mut(),
        keystroke_display_initial_position: std::ptr::null_mut(),
        keystroke_display_max_extent: std::ptr::null_mut(),
        keystroke_display_max_extent_value: std::ptr::null_mut(),
        log_level: std::ptr::null_mut(),
        launch_at_login: std::ptr::null_mut(),
        hide_menu_bar_icon: std::ptr::null_mut(),
        reverse_scroll: std::ptr::null_mut(),
        enable_mouse: std::ptr::null_mut(),
        scroll_mode: std::ptr::null_mut(),
        line_count: std::ptr::null_mut(),
        line_count_label: std::ptr::null_mut(),
        line_count_value_label: std::ptr::null_mut(),
        disable_pointer_accel: std::ptr::null_mut(),
        pointer_accel_slider: std::ptr::null_mut(),
        pointer_accel_label: std::ptr::null_mut(),
        pointer_accel_value_label: std::ptr::null_mut(),
        smooth_scrolling_preset: std::ptr::null_mut(),
        smooth_scrolling_response: std::ptr::null_mut(),
        smooth_scrolling_response_value: std::ptr::null_mut(),
        smooth_scrolling_speed: std::ptr::null_mut(),
        smooth_scrolling_speed_value: std::ptr::null_mut(),
        smooth_scrolling_acceleration: std::ptr::null_mut(),
        smooth_scrolling_acceleration_value: std::ptr::null_mut(),
        smooth_scrolling_inertia: std::ptr::null_mut(),
        smooth_scrolling_inertia_value: std::ptr::null_mut(),
        mapping_layout_row: 0,
        update_host_row: 0,
        update_band_row: 0,
        mapping_scroll: std::ptr::null_mut(),
        mapping_doc: std::ptr::null_mut(),
        mapping_card: std::ptr::null_mut(),
        mapping_panel: std::ptr::null_mut(),
        mapping_rows: Vec::new(),
        clipboard_enabled: std::ptr::null_mut(),
        clipboard_shortcut: std::ptr::null_mut(),
        clipboard_shortcut_error: std::ptr::null_mut(),
        clipboard_shortcut_row_height: 0.0,
        clipboard_pin_follow_row_height: 0.0,
        clipboard_row_gap: 0.0,
        key_bindings_enabled: std::ptr::null_mut(),
        key_bindings_doc: std::ptr::null_mut(),
        key_bindings_empty: std::ptr::null_mut(),
        key_bindings_footer: std::ptr::null_mut(),
        key_bindings_footer_button: std::ptr::null_mut(),
        key_bindings_row: 0,
        key_binding_rows: Vec::new(),
        key_binding_items_rendered: Vec::new(),
        clipboard_clear_on_quit: std::ptr::null_mut(),
        clipboard_separate_clear_all: std::ptr::null_mut(),
        clipboard_move_used_to_top: std::ptr::null_mut(),
        clipboard_delete_after_paste: std::ptr::null_mut(),
        clipboard_clear_system_pasteboard_after_paste: std::ptr::null_mut(),
        clipboard_max_entries: std::ptr::null_mut(),
        clipboard_auto_expire_days: std::ptr::null_mut(),
        clipboard_auto_expire_days_value_label: std::ptr::null_mut(),
        clipboard_show_source_app: std::ptr::null_mut(),
        clipboard_pin_follow: std::ptr::null_mut(),
        clipboard_picker_placement: std::ptr::null_mut(),
        clipboard_paused: std::ptr::null_mut(),
        clipboard_excluded_remove: Vec::new(),
        clipboard_excluded_add: std::ptr::null_mut(),
        clipboard_excluded_candidates: Vec::new(),
        clipboard_excluded_restore: std::ptr::null_mut(),
        add_mapping_button: std::ptr::null_mut(),
        mapping_enabled: std::ptr::null_mut(),
        mapping_empty: std::ptr::null_mut(),
        device_indicator: std::ptr::null_mut(),
        device_info_caption: std::ptr::null_mut(),
        restore_defaults: RestoreDefaultsControl::empty(),
        page_restores: std::array::from_fn(|_| RestoreDefaultsControl::empty()),
        page_canvases: std::array::from_fn(|_| PageCanvas::empty()),
        permission_warning_view: std::ptr::null_mut(),
        update_auto_check: std::ptr::null_mut(),
        update_auto_download: std::ptr::null_mut(),
        update_check_button: std::ptr::null_mut(),
        update_host: std::ptr::null_mut(),
        update_host_window: std::ptr::null_mut(),
        update_card: std::ptr::null_mut(),
        update_divider: std::ptr::null_mut(),
        update_card_expanded: false,
        finder_menu_status: std::ptr::null_mut(),
        finder_menu_enabled: std::ptr::null_mut(),
        finder_menu_layout: std::ptr::null_mut(),
        finder_menu_new_file_switches: Vec::new(),
        finder_menu_terminal_switches: Vec::new(),
        finder_menu_copy_path: std::ptr::null_mut(),
        finder_menu_copy_sha256: std::ptr::null_mut(),
        control_table: ControlTable::new(),
        migrated_pages: std::array::from_fn(|_| None),
    }
}

pub(crate) unsafe fn migrated_pages_smoke_runner() -> bool {
    let mut ok = true;
    for page in [
        SettingsPageId::Switcher,
        SettingsPageId::General,
        SettingsPageId::Clipboard,
        SettingsPageId::About,
        SettingsPageId::KeystrokeDisplay,
        SettingsPageId::FinderMenu,
        SettingsPageId::KeyBindings,
        SettingsPageId::Mouse,
    ] {
        let document: *mut AnyObject = msg_send![class!(NSView), alloc];
        let document: *mut AnyObject = msg_send![
            document,
            initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(600.0, 800.0))
        ];
        let input = PageInput::from_config(
            crate::config::Config::default(),
            None,
            crate::theme::ThemeInput::current(),
        );
        let view = view_for(page, &input);
        let metrics = PageMetrics::for_layout(SettingsLayout::new(600.0));
        let mut canvas = PageCanvas::new(
            document,
            600.0,
            SettingsLayout::new(600.0),
            &t(view.title_key),
            document_fixture_height(),
            700.0,
        );
        let fixture_layout = SettingsLayout::new(600.0);
        let measured = MeasuredText::measure(&view, fixture_layout, 620.0);
        let layout_input = LayoutInput {
            content_w: 600.0,
            doc_top: document_fixture_height(),
            header_offset: canvas.header_offset(),
            banner_frame: BannerFrame::of(banner_parent_frame(
                28.0,
                700.0,
                600.0,
                banner_height(false),
            )),
            layout: fixture_layout,
            text: measured,
            extra_row_heights: view
                .extra_rows
                .iter()
                .map(|extra| extra.height.resolve(&input, 600.0))
                .collect(),
        };
        let layout = layout_rows(&view, &layout_input, &metrics);
        // The description half: one laid-out row per described row, one surface per card
        // (page-drawn-only cards included), and a sequence that places every row exactly once.
        let described_laid_out = layout.rows.len() == view.rows.len();
        let highest_card = view
            .rows
            .iter()
            .map(|row| row.card)
            .chain(view.extra_rows.iter().map(|extra| extra.card))
            .max()
            .map_or(0, |card| card + 1);
        let cards_laid_out =
            layout.cards.len() == highest_card && layout.card_tops.len() >= highest_card;
        let sequence_complete = layout.sequence.len() == view.rows.len() + view.extra_rows.len();
        // The rendered half: build the page's REAL views, with the page's own hooks, on the
        // detached document. This is what catches a control that never entered the view tree and a
        // placement that ignored the row's origin.
        let mut ui = headless_settings_ui();
        let mut hooks = page_hooks(page);
        let rendered = match render_page(
            &mut canvas,
            std::ptr::null_mut(),
            &view,
            &layout,
            &metrics,
            &mut ui,
            hooks.as_mut(),
        ) {
            Ok(rendered) => rendered,
            Err(error) => {
                log_info!("[smoke-migrated-pages] {page:?}: render refused ({error:?})");
                ok = false;
                continue;
            }
        };
        // The canvas settles its row bookkeeping when the page is finished: every view is placed and
        // every row flushed by then, which is what the placement checks read.
        let _: f64 = canvas.finish();
        // Every DESCRIBED row placed its control (by identity): the count is larger than the row
        // count whenever a page also draws rows of its own or a value row carries an action button,
        // and smaller when a row failed to build — which is the failure this catches.
        let described_ok = !view.rows.is_empty()
            && view
                .rows
                .iter()
                .all(|row| rendered.controls.iter().any(|control| control.id == row.id));
        if !described_ok {
            log_info!(
                "[smoke-migrated-pages] {page:?}: a described row placed no control ({} controls)",
                rendered.controls.len()
            );
        }
        let mut placed_ok = scene_holds_placements(&canvas, &view, &layout);
        // Every page-drawn row placed at least one view, inside the row the layout reserved.
        for (index, spec) in view.extra_rows.iter().enumerate() {
            let Some(row) = canvas_row_of_drawn_row(&layout, index) else {
                log_info!(
                    "[smoke-migrated-pages] {page:?}: {} is not in the layout",
                    spec.key
                );
                placed_ok = false;
                continue;
            };
            let (_, origin, height) = layout.drawn_rows[index];
            let Some(views) = canvas.row_views(row) else {
                log_info!(
                    "[smoke-migrated-pages] {page:?}: page-drawn row {} ({}) placed nothing",
                    index,
                    spec.key
                );
                placed_ok = false;
                continue;
            };
            // A row the flow grows later (the About page's inline update host) starts at zero
            // height by design: what matters is that it placed its views inside the page.
            if views.is_empty() || height < 0.0 || origin > layout.first_card_top {
                log_info!(
                    "[smoke-migrated-pages] {page:?}: page-drawn row {} ({}) is empty or outside the page",
                    index,
                    spec.key
                );
                placed_ok = false;
            }
        }
        // DRIVE the production re-flow: collapse a conditional group, re-flow, re-assert; then move
        // the viewport, which shifts every row's origin, and re-assert again. A renderer that placed
        // its views once and never followed the canvas fails HERE.
        let groups = description_groups(&view);
        if let Some(group) = groups.first().copied() {
            canvas.set_group_visible(group, true);
            placed_ok &= scene_holds_placements(&canvas, &view, &layout);
            canvas.set_group_visible(group, false);
            placed_ok &= scene_holds_placements(&canvas, &view, &layout);
        }
        canvas.set_viewport(520.0);
        placed_ok &= scene_holds_placements(&canvas, &view, &layout);
        canvas.set_viewport(700.0);
        let cards_ok = view_cards_contain_their_rows(&view, &layout);
        if !(described_laid_out && cards_laid_out && sequence_complete && cards_ok) {
            log_info!(
                "[smoke-migrated-pages] {page:?}: described={described_laid_out} cards={cards_laid_out} sequence={sequence_complete} cards_cover={cards_ok}"
            );
            ok = false;
        }
        if !(described_ok && placed_ok) {
            log_info!(
                "[smoke-migrated-pages] {page:?}: controls={described_ok} placements={placed_ok}"
            );
            ok = false;
        }
    }
    ok
}

/// The page's REAL hooks, built against the headless UI: a page whose hook draws rows cannot be
/// checked by skipping it.
unsafe fn page_hooks(page: SettingsPageId) -> Box<dyn PageHooks> {
    match page {
        SettingsPageId::About => Box::new(AboutHooks {
            window: std::ptr::null_mut(),
        }),
        SettingsPageId::Clipboard => Box::new(ClipboardHooks {
            shortcut_title: String::new(),
        }),
        SettingsPageId::KeyBindings => Box::new(KeyBindingsHooks::default()),
        SettingsPageId::FinderMenu => Box::new(FinderMenuHooks {}),
        SettingsPageId::Mouse => Box::new(MouseHooks {
            target: std::ptr::null_mut(),
        }),
        _ => Box::new(NoHooks),
    }
}

/// The canvas row a page-drawn row occupies: its position in the layout's sequence, which is also
/// the canvas row the renderer opened for it.
pub(super) fn canvas_row_of_drawn_row(layout: &PageLayout, index: usize) -> Option<usize> {
    layout
        .sequence
        .iter()
        .position(|placed| matches!(placed, PlacedRow::Drawn(drawn) if *drawn == index))
}

/// Each card's surface must cover every row inside it, described or page-drawn.
fn view_cards_contain_their_rows(view: &PageView, layout: &PageLayout) -> bool {
    for card in &layout.cards {
        for (index, row) in view.rows.iter().enumerate() {
            if row.card != card.index {
                continue;
            }
            let frame = &layout.rows[index].frame;
            if frame.y < card.frame.y - 1.0e-6
                || frame.y + frame.h > card.frame.y + card.frame.h + 1.0e-6
            {
                return false;
            }
        }
        for (position, extra) in view.extra_rows.iter().enumerate() {
            if extra.card != card.index {
                continue;
            }
            let (_, origin, height) = layout.drawn_rows[position];
            if origin < card.frame.y - 1.0e-6
                || origin + height > card.frame.y + card.frame.h + 1.0e-6
            {
                return false;
            }
        }
    }
    true
}

/// Every row the description declares has views in the detached document, with a real size: the
/// check the smoke runner drives after building a page's real views.
/// Every view a row placed is in the detached document, has a real size, and sits at its row's
/// CURRENT origin plus the offset the layout gave it — read from the CANVAS, which is what a
/// re-flow moves. A renderer that placed its views once and never followed a re-flow fails here.
/// Every view a row placed is a subview of the document with a real size, and its frame matches the
/// row's CURRENT canvas origin plus the offset the layout gave it. Reading the origin from the
/// CANVAS is the point: it is what a re-flow moves, so a renderer that placed its views once and
/// never followed a re-flow fails here.
/// Every row the description placed is still where the canvas's current re-flow says it is: the
/// canvas records each view's offset from its row's origin, applies the row's CURRENT origin, and
/// `placements_are_stale` reports a view that did not follow. Reading the ORIGIN from the canvas is
/// the point — a group collapse or a viewport change moves it, and a renderer that placed its views
/// once and never followed fails HERE.
unsafe fn scene_holds_placements(
    canvas: &PageCanvas,
    view: &PageView,
    layout: &PageLayout,
) -> bool {
    let document = canvas.document();
    if document.is_null() {
        return false;
    }
    // The canvas row is the row's position in the ONE sequence, never its index among described
    // rows: a page whose earlier cards hold page-drawn rows puts its later rows further down.
    let canvas_rows = canvas_rows(view);
    for (row, _spec) in view.rows.iter().enumerate() {
        let canvas_row = canvas_rows[row];
        let (Some(_origin), Some(placements)) = (
            canvas.current_row_origin(canvas_row),
            canvas.row_placements(canvas_row),
        ) else {
            return false;
        };
        // A described row always draws at least its copy and its control.
        if placements.len() < 2 {
            return false;
        }
        if canvas.placements_are_stale(canvas_row) {
            return false;
        }
        for (placed_view, _offset) in placements {
            if placed_view.is_null() {
                return false;
            }
            let frame: NSRect = msg_send![*placed_view, frame];
            if frame.size.width <= 0.0 || frame.size.height <= 0.0 {
                return false;
            }
            let parent: *mut AnyObject = msg_send![*placed_view, superview];
            if parent != document {
                return false;
            }
        }
    }
    // The layout must still describe the same page it was computed for.
    layout.rows.len() == view.rows.len()
}

/// Drive the About page's inline update flow on a detached document, through the SAME canvas calls
/// the production path uses, and assert its geometry.
///
/// The relations, not the absolute coordinates: the document is laid out from its TOP edge, so
/// growing it moves every row. What must hold is (a) the divider sits on the last row above the
/// band -- before, during and after the flow -- and (b) the expanded host's top edge is that same
/// line, so the panel takes the check button's place instead of starting a row lower.
pub(crate) unsafe fn about_update_flow_smoke_runner() -> bool {
    let document: *mut AnyObject = msg_send![class!(NSView), alloc];
    let document: *mut AnyObject = msg_send![
        document,
        initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(600.0, 900.0))
    ];
    let input = PageInput::from_config(
        crate::config::Config::default(),
        None,
        crate::theme::ThemeInput::current(),
    );
    let view = view_for(SettingsPageId::About, &input);
    let layout = SettingsLayout::new(600.0);
    let metrics = PageMetrics::for_layout(layout);
    let mut canvas = PageCanvas::new_at(
        document,
        600.0,
        layout,
        view.header_offset.unwrap_or(0.0),
        1200.0,
        900.0,
    );
    let mut ui = headless_settings_ui();
    let mut hooks = AboutHooks {
        window: std::ptr::null_mut(),
    };
    let layout_input = LayoutInput {
        content_w: 600.0,
        doc_top: 1200.0,
        header_offset: canvas.header_offset(),
        banner_frame: BannerFrame::of(banner_parent_frame(
            28.0,
            900.0,
            600.0,
            banner_height(false),
        )),
        layout,
        text: MeasuredText::measure(&view, layout, 620.0),
        extra_row_heights: view
            .extra_rows
            .iter()
            .map(|extra| extra.height.resolve(&input, 600.0))
            .collect(),
    };
    let page_layout = layout_rows(&view, &layout_input, &metrics);
    if render_page(
        &mut canvas,
        std::ptr::null_mut(),
        &view,
        &page_layout,
        &metrics,
        &mut ui,
        &mut hooks,
    )
    .is_err()
    {
        log_info!("[smoke-update-flow] the About page refused to build");
        return false;
    }
    let _: f64 = canvas.finish();
    let (band_row, host_row) = (ui.update_band_row, ui.update_host_row);
    let divider = ui.update_divider;
    if divider.is_null() || band_row == host_row || ui.update_host.is_null() {
        log_info!("[smoke-update-flow] the flow has no distinct rows, host and divider");
        return false;
    }
    // The row the divider marks the bottom edge of: the last row settled before the band.
    let row_above = band_row.saturating_sub(1);
    let divider_y = |_canvas: &PageCanvas| -> f64 {
        let frame: NSRect = msg_send![divider, frame];
        frame.origin.y
    };
    // The compact state: the divider is the row above's bottom edge.
    let _: f64 = canvas.finish();
    let row_above_origin = canvas.current_row_origin(row_above);
    let compact_ok = row_above_origin
        .map(|origin| (divider_y(&canvas) - origin).abs() <= 0.5)
        .unwrap_or(false);
    // The flow's own calls, in production's order.
    let host_h = 240.0;
    canvas.set_bottom_padding(SettingsPageHeader::BOTTOM_PADDING.min(24.0));
    canvas.set_row_consume(band_row, 0.0);
    canvas.set_row_consume(host_row, host_h);
    let host: NSRect = msg_send![ui.update_host, frame];
    let _: () = msg_send![
        ui.update_host,
        setFrameSize: NSSize::new(host.size.width, host_h)
    ];
    let _: f64 = canvas.finish();
    // (a) the divider still marks the row above's bottom edge.
    let divider_after = divider_y(&canvas);
    let anchored_ok = canvas
        .current_row_origin(row_above)
        .map(|origin| (divider_after - origin).abs() <= 0.5)
        .unwrap_or(false);
    // (b) the expanded host hangs from that same line.
    let host: NSRect = msg_send![ui.update_host, frame];
    let host_top = host.origin.y + host.size.height;
    let host_ok = (host_top - divider_after).abs() <= 1.0;
    // A PHASE CHANGE inside the flow: Sparkle resizes the host as its state advances, calling the
    // flow's expand again with a different height. The divider must stay on the row above's bottom
    // edge and the host's top edge must still meet it, at every height -- not only the first.
    let mut phase_ok = true;
    for phase_h in [180.0_f64, 300.0, 120.0] {
        canvas.set_row_consume(host_row, phase_h);
        let host: NSRect = msg_send![ui.update_host, frame];
        let _: () = msg_send![
            ui.update_host,
            setFrameSize: NSSize::new(host.size.width, phase_h)
        ];
        let _: f64 = canvas.finish();
        let divider_y = divider_y(&canvas);
        let anchored = canvas
            .current_row_origin(row_above)
            .map(|origin| (divider_y - origin).abs() <= 0.5)
            .unwrap_or(false);
        let host: NSRect = msg_send![ui.update_host, frame];
        let meets = ((host.origin.y + host.size.height) - divider_y).abs() <= 1.0;
        if !(anchored && meets) {
            log_info!(
                "[smoke-update-flow] phase height {phase_h}: anchored={anchored} meets={meets}"
            );
            phase_ok = false;
        }
    }
    // Collapsing restores the compact relation.
    let _: () = msg_send![ui.update_host, setFrameSize: NSSize::new(host.size.width, 0.0)];
    canvas.set_row_consume(host_row, 0.0);
    canvas.set_row_consume(band_row, SettingsLayout::SINGLE_LINE_ROW_H);
    let _: f64 = canvas.finish();
    let collapsed_ok = canvas
        .current_row_origin(row_above)
        .map(|origin| (divider_y(&canvas) - origin).abs() <= 0.5)
        .unwrap_or(false);
    let ok = compact_ok && anchored_ok && host_ok && phase_ok && collapsed_ok;
    if !ok {
        log_info!(
            "[smoke-update-flow] compact={compact_ok} anchored={anchored_ok} host={host_ok} (gap={:.1}) collapsed={collapsed_ok}",
            host_top - divider_after
        );
    } else {
        log_info!("[smoke-update-flow] 4 relations plus 3 phase heights checked");
    }
    ok
}
/// The height the detached smoke document is provisioned with.
fn document_fixture_height() -> f64 {
    900.0
}
// MARK: - Binding a page's controls into SettingsUi

/// The `SettingsUi` pointer the dispatcher, the refill's own consumers and the freeze lists read
/// for a field. Field identity stays the single source: the table says what a control edits, and
/// this table says which legacy pointer field that identity still fills.
///
/// A field with no entry here has no legacy consumer left (nothing reads `ui.<field>` for it), so
/// publishing it would only create a second name for the same control.
fn settings_ui_slot(ui: &mut SettingsUi, field: ControlField) -> Option<&mut *mut AnyObject> {
    if !settings_ui_slot_present(field) {
        return None;
    }
    Some(match field {
        ControlField::Theme => &mut ui.theme,
        ControlField::PanelMaterial => &mut ui.panel_material,
        ControlField::GlassStyle => &mut ui.glass_style,
        ControlField::Locale => &mut ui.locale,
        ControlField::LogLevel => &mut ui.log_level,
        ControlField::LaunchAtLogin => &mut ui.launch_at_login,
        ControlField::HideMenuBarIcon => &mut ui.hide_menu_bar_icon,
        ControlField::WindowsEnabled => &mut ui.windows_enabled,
        ControlField::ShowMinimized => &mut ui.show_minimized,
        ControlField::ShowHiddenAppWindows => &mut ui.show_hidden_app_windows,
        ControlField::ShowOtherDesktops => &mut ui.show_other_desktops,
        ControlField::ThumbnailsEnabled => &mut ui.thumbnails_enabled,
        ControlField::FocusedThumbnailPrewarm => &mut ui.focused_thumbnail_prewarm,
        ControlField::ShowAppNameInCards => &mut ui.show_app_name_in_cards,
        ControlField::ThumbnailSize => &mut ui.thumbnail_size,
        ControlField::CardTextSize => &mut ui.card_text_size,
        ControlField::StatusBarTextSize => &mut ui.status_bar_text_size,
        ControlField::OverlayPosition => &mut ui.overlay_position,
        ControlField::ActivationMode => &mut ui.activation_mode,
        ControlField::PanelCloseEnabled => &mut ui.panel_close_enabled,
        ControlField::PanelMinimizeEnabled => &mut ui.panel_minimize_enabled,
        ControlField::PanelHideEnabled => &mut ui.panel_hide_enabled,
        ControlField::PanelQuitEnabled => &mut ui.panel_quit_enabled,
        ControlField::CornerRadius => &mut ui.corner_radius,
        ControlField::Modifier => &mut ui.modifier,
        ControlField::KeystrokeDisplayEnabled => &mut ui.keystroke_display_enabled,
        ControlField::KeystrokeDisplayMode => &mut ui.keystroke_display_mode,
        ControlField::KeystrokeDisplayTapLevel => &mut ui.keystroke_display_tap_level,
        ControlField::KeystrokeDisplayPosition => &mut ui.keystroke_display_position,
        ControlField::KeystrokeDisplayInitialPosition => &mut ui.keystroke_display_initial_position,
        ControlField::KeystrokeDisplayMaxExtent => &mut ui.keystroke_display_max_extent,
        ControlField::ClipboardEnabled => &mut ui.clipboard_enabled,
        ControlField::ClipboardPaused => &mut ui.clipboard_paused,
        ControlField::ClipboardClearOnQuit => &mut ui.clipboard_clear_on_quit,
        ControlField::ClipboardSeparateClearAll => &mut ui.clipboard_separate_clear_all,
        ControlField::ClipboardShowSourceApp => &mut ui.clipboard_show_source_app,
        ControlField::ClipboardMoveUsedToTop => &mut ui.clipboard_move_used_to_top,
        ControlField::ClipboardDeleteAfterPaste => &mut ui.clipboard_delete_after_paste,
        ControlField::ClipboardClearSystemPasteboardAfterPaste => {
            &mut ui.clipboard_clear_system_pasteboard_after_paste
        }
        ControlField::ClipboardAutoExpireDays => &mut ui.clipboard_auto_expire_days,
        ControlField::ClipboardImageRows => &mut ui.clipboard_image_rows,
        ControlField::ClipboardPinFollow => &mut ui.clipboard_pin_follow,
        ControlField::ClipboardPickerPlacement => &mut ui.clipboard_picker_placement,
        ControlField::ClipboardMaxEntries => &mut ui.clipboard_max_entries,
        ControlField::MouseEnabled => &mut ui.enable_mouse,
        ControlField::ReverseScroll => &mut ui.reverse_scroll,
        ControlField::ScrollMode => &mut ui.scroll_mode,
        ControlField::LineCount => &mut ui.line_count,
        ControlField::DisablePointerAccel => &mut ui.disable_pointer_accel,
        ControlField::PointerAcceleration => &mut ui.pointer_accel_slider,
        ControlField::MappingEnabled => &mut ui.mapping_enabled,
        ControlField::KeyBindingsEnabled => &mut ui.key_bindings_enabled,
        ControlField::UpdateAutoCheck => &mut ui.update_auto_check,
        ControlField::UpdateAutoDownload => &mut ui.update_auto_download,
        ControlField::FinderMenuEnabled => &mut ui.finder_menu_enabled,
        ControlField::FinderMenuLayout => &mut ui.finder_menu_layout,
        ControlField::FinderMenuCopyPath => &mut ui.finder_menu_copy_path,
        ControlField::FinderMenuCopySha256 => &mut ui.finder_menu_copy_sha256,
        ControlField::ShowFooterTitle => &mut ui.show_footer_title,
        // `settings_ui_slot_present` listed every field above; a new one without a slot is a
        // compile error here, which is what keeps the two in step.
        _ => {
            unreachable!("settings_ui_slot_present and settings_ui_slot must list the same fields")
        }
    })
}

/// Bind the page's permission banner to the three fields the window's banner refresh reads
/// (`refresh_settings_warning_banner`). Called only after the page's build succeeded, like every
/// other binding, so a refused build cannot leave the banner fields pointing at views that were
/// dropped.
pub(super) fn bind_page_banner(ui: &mut SettingsUi, rendered: &RenderedPage) {
    if rendered.banner_strip.is_null() {
        return;
    }
    ui.permission_warning_view = rendered.banner_strip;
    ui.permission_warning_label = rendered.banner_label;
    ui.permission_warning_button = rendered.banner_button;
}

/// Bind a built page's controls to the legacy pointer fields their consumers still read.
///
/// One control at a time, after the page's registrations were published: a page whose build was
/// refused binds nothing, and the fields that already held the previous page's controls are left
/// alone until the new page is known good.
pub(super) unsafe fn bind_page_controls(ui: &mut SettingsUi, rendered: &RenderedPage) {
    for control in &rendered.controls {
        // A page-drawn control has no ControlField; it says which of the fields its own callback
        // reads the value belongs in. Without this a page could draw a control the dispatcher can
        // never reach.
        match &control.extra {
            RenderedExtra::None => {}
            RenderedExtra::Shortcut { row_height } => {
                ui.clipboard_shortcut = control.control;
                ui.clipboard_shortcut_error = control.readout;
                ui.clipboard_shortcut_row_height = *row_height;
            }
            RenderedExtra::MaxEntries => ui.clipboard_max_entries = control.control,
        }
        // About's status rows are VALUES with their own action: the refresh function writes the
        // status text and the button that opens the matching pane, both by pointer. A row player
        // of `ControlField` has no room for them (they are not config fields), so they are bound
        // by the action identity that names them.
        if let ControlId::Action(action) = control.id {
            // The status label and the action button are bound SEPARATELY: the refresh function
            // writes the text by pointer and enables/disables the button by its own pointer.
            match action {
                "open_accessibility_permission" => {
                    ui.accessibility_permission_status = control.control;
                }
                "open_screen_recording_permission" => {
                    ui.screen_recording_permission_status = control.control;
                }
                "retry_keychain_access" => ui.keychain_access_status = control.control,
                _ => {}
            }
            if !control.action.is_null() {
                match action {
                    "open_accessibility_permission" => {
                        ui.accessibility_permission_button = control.action
                    }
                    "retry_keychain_access" => ui.keychain_access_button = control.action,
                    _ => {}
                }
            }
            continue;
        }
        let ControlId::Field(field) = control.id else {
            continue;
        };
        if let Some(slot) = settings_ui_slot(ui, field) {
            *slot = control.control;
        }
        // A slider's readout label is a second pointer field on the page.
        if !control.readout.is_null() {
            match field {
                ControlField::CardTextSize => ui.card_text_size_value_label = control.readout,
                ControlField::StatusBarTextSize => {
                    ui.status_bar_text_size_value_label = control.readout
                }
                ControlField::KeystrokeDisplayMaxExtent => {
                    ui.keystroke_display_max_extent_value = control.readout
                }
                // The clipboard page's two numeric rows draw a readout beside the slider; the
                // refill writes it, so it needs the pointer like every other readout.
                ControlField::ClipboardAutoExpireDays => {
                    ui.clipboard_auto_expire_days_value_label = control.readout
                }
                ControlField::ClipboardImageRows => {
                    ui.clipboard_image_rows_value_label = control.readout
                }
                _ => {}
            }
        }
    }
}

/// Whether the field has a legacy `SettingsUi` pointer field the dispatcher and the freeze lists
/// still read (the HIGH 2 coverage assertion). Field-presence only: it never touches a `SettingsUi`.
#[cfg(test)]
pub(super) fn settings_ui_slot_field(field: ControlField) -> Option<()> {
    settings_ui_slot_present(field).then_some(())
}

/// The fields [`bind_page_controls`] fills, as a predicate, so the coverage rule can be asserted
/// without an AppKit window. Kept beside the match that does the filling; `page_view` tests below
/// cross-check the two lists against each other.
pub(super) fn settings_ui_slot_present(field: ControlField) -> bool {
    matches!(
        field,
        ControlField::Theme
            | ControlField::PanelMaterial
            | ControlField::GlassStyle
            | ControlField::Locale
            | ControlField::LogLevel
            | ControlField::LaunchAtLogin
            | ControlField::HideMenuBarIcon
            | ControlField::WindowsEnabled
            | ControlField::ShowMinimized
            | ControlField::ShowHiddenAppWindows
            | ControlField::ShowOtherDesktops
            | ControlField::ThumbnailsEnabled
            | ControlField::FocusedThumbnailPrewarm
            | ControlField::ShowAppNameInCards
            | ControlField::ThumbnailSize
            | ControlField::CardTextSize
            | ControlField::StatusBarTextSize
            | ControlField::OverlayPosition
            | ControlField::ActivationMode
            | ControlField::PanelCloseEnabled
            | ControlField::PanelMinimizeEnabled
            | ControlField::PanelHideEnabled
            | ControlField::PanelQuitEnabled
            | ControlField::CornerRadius
            | ControlField::Modifier
            | ControlField::KeystrokeDisplayEnabled
            | ControlField::KeystrokeDisplayMode
            | ControlField::KeystrokeDisplayTapLevel
            | ControlField::KeystrokeDisplayPosition
            | ControlField::KeystrokeDisplayInitialPosition
            | ControlField::KeystrokeDisplayMaxExtent
            // The pages migrated after the first two: their controls exist only as described rows,
            // so the shared binder is the only place their legacy consumers (which read
            // `SettingsUi` fields directly) can be handed the control the table identified.
            | ControlField::ClipboardEnabled
            | ControlField::ClipboardPaused
            | ControlField::ClipboardClearOnQuit
            | ControlField::ClipboardSeparateClearAll
            | ControlField::ClipboardShowSourceApp
            | ControlField::ClipboardMoveUsedToTop
            | ControlField::ClipboardDeleteAfterPaste
            | ControlField::ClipboardClearSystemPasteboardAfterPaste
            | ControlField::ClipboardAutoExpireDays
            | ControlField::ClipboardImageRows
            | ControlField::ClipboardPinFollow
            | ControlField::ClipboardPickerPlacement
            | ControlField::ClipboardMaxEntries
            | ControlField::MouseEnabled
            | ControlField::ReverseScroll
            | ControlField::ScrollMode
            | ControlField::LineCount
            | ControlField::DisablePointerAccel
            | ControlField::PointerAcceleration
            | ControlField::MappingEnabled
            | ControlField::KeyBindingsEnabled
            | ControlField::UpdateAutoCheck
            | ControlField::UpdateAutoDownload
            | ControlField::FinderMenuEnabled
            | ControlField::FinderMenuLayout
            | ControlField::FinderMenuCopyPath
            | ControlField::FinderMenuCopySha256
            | ControlField::ShowFooterTitle
    )
}

/// Whether a slider field's readout label is bound by [`bind_page_controls`].
#[cfg(test)]
pub(super) fn settings_ui_readout_field(field: ControlField) -> bool {
    matches!(
        field,
        ControlField::CardTextSize
            | ControlField::StatusBarTextSize
            | ControlField::KeystrokeDisplayMaxExtent
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)] // only the ignored M2 smoke runner uses it
    use crate::smoke_verdict::smoke_runner;

    fn input() -> PageInput {
        PageInput::from_config(Config::default(), None, crate::theme::ThemeInput::current())
    }

    fn switcher() -> PageView {
        switcher_page_view(&input())
    }

    fn general() -> PageView {
        general_page_view(&input())
    }

    fn fields(view: &PageView) -> Vec<ControlField> {
        view.rows.iter().filter_map(|row| row.field()).collect()
    }

    #[test]
    fn switcher_page_view_describes_every_row_with_its_identity_and_copy() {
        let view = switcher();
        assert_eq!(view.page, SettingsPageId::Switcher);
        assert_eq!(view.rows.len(), 17);
        // Every declared row carries a label key and an identity; the copy keys are the ones the
        // page's own labels used before the migration.
        let labels: Vec<&str> = view.rows.iter().map(|row| row.label_key).collect();
        assert_eq!(labels[0], "settings.row_windows_enabled");
        assert_eq!(labels[1], "settings.row_show_minimized");
        assert_eq!(labels[4], "settings.row_window_display_mode");
        assert_eq!(labels[8], "settings.row_card_text_size");
        assert_eq!(labels[16], "settings.row_modifier");
        // The field order is the page's own order: a reorder is a visible change, not a detail.
        assert_eq!(
            fields(&view),
            vec![
                ControlField::WindowsEnabled,
                ControlField::ShowMinimized,
                ControlField::ShowHiddenAppWindows,
                ControlField::ShowOtherDesktops,
                ControlField::ThumbnailsEnabled,
                ControlField::FocusedThumbnailPrewarm,
                ControlField::ShowAppNameInCards,
                ControlField::ThumbnailSize,
                ControlField::CardTextSize,
                ControlField::StatusBarTextSize,
                ControlField::OverlayPosition,
                ControlField::ActivationMode,
                ControlField::PanelCloseEnabled,
                ControlField::PanelMinimizeEnabled,
                ControlField::PanelHideEnabled,
                ControlField::PanelQuitEnabled,
                ControlField::Modifier,
            ]
        );
        // The control kinds are presentation only; the identity above is what the dispatcher uses.
        assert!(matches!(view.rows[0].control, ControlKind::Switch));
        assert!(matches!(view.rows[4].control, ControlKind::Popup { .. }));
        assert!(matches!(view.rows[8].control, ControlKind::Slider { .. }));
        // The cards are the inline builder's three, in its order: the overlay switch alone, then
        // every other window setting, then the keyboard row.
        let sections: Vec<&str> = view.rows.iter().map(|row| row.section).collect();
        assert_eq!(sections[0], "settings.header_windows");
        assert_eq!(sections[1], "settings.header_window_options");
        assert_eq!(sections[12], "settings.header_window_options");
        assert_eq!(sections[16], "settings.header_keyboard");
        assert_eq!(view.rows[0].card, 0);
        assert_eq!(view.rows[1].card, 1);
        assert_eq!(view.rows[16].card, 2);
        // The first row of every card has no divider above it; every later row in the card does.
        for (index, row) in view.rows.iter().enumerate() {
            let first_of_card = index == 0 || view.rows[index - 1].card != row.card;
            assert_eq!(
                row.separator_above, !first_of_card,
                "row {index} of card {} states the wrong divider",
                row.card
            );
        }
        // The thumbnail-only block is a conditional group, so its rows can be skipped together.
        let grouped: Vec<usize> = view
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.group == Some(RowGroup::ThumbnailOnly))
            .map(|(index, _)| index)
            .collect();
        assert_eq!(grouped, vec![5, 6, 7]);
    }

    #[test]
    fn general_page_view_describes_every_row_with_its_identity_and_copy() {
        let view = general();
        assert_eq!(view.page, SettingsPageId::General);
        assert_eq!(view.rows.len(), 10);
        assert_eq!(
            fields(&view),
            vec![
                ControlField::LaunchAtLogin,
                ControlField::HideMenuBarIcon,
                ControlField::Theme,
                ControlField::PanelMaterial,
                ControlField::GlassStyle,
                ControlField::Locale,
                ControlField::LogLevel,
            ]
        );
        // The three action rows carry an action identity and no config field.
        let actions: Vec<&str> = view
            .rows
            .iter()
            .filter_map(|row| match row.id {
                ControlId::Action(action) => Some(action),
                ControlId::Field(_) => None,
            })
            .collect();
        assert_eq!(
            actions,
            vec!["export_logs", "export_settings", "import_settings"]
        );
        // The glass style is the Liquid-Glass block; the two backup rows are stacked copy.
        assert_eq!(view.rows[4].group, Some(RowGroup::GlassStyle));
        assert_eq!(view.rows[8].copy, RowCopy::Captioned);
        assert_eq!(view.rows[9].copy, RowCopy::Captioned);
        // The permission banner belongs to the page, with the action its button runs.
        let banner = view.banner.as_ref().expect("General carries the banner");
        assert_eq!(banner.action_id, "open_privacy");
        assert!(!t(banner.copy_key).is_empty());
    }

    #[test]
    fn page_view_reads_only_its_input() {
        // The same config with a different device is still one description: the Switcher and
        // General pages do not depend on the selected mouse.
        let mut config = Config::default();
        config.windows.show_minimized = true;
        let view = switcher_page_view(&PageInput::from_config(
            config.clone(),
            Some((0x1234, 0x5678)),
            crate::theme::ThemeInput::current(),
        ));
        assert_eq!(view.rows[1].value, Value::Switch(true));
        // ... and a changed field changes the description's value, not its shape.
        config.keyboard.modifier = "control".to_string();
        let other = switcher_page_view(&PageInput::from_config(
            config,
            None,
            crate::theme::ThemeInput::current(),
        ));
        let modifier = other
            .rows
            .iter()
            .find(|row| row.field() == Some(ControlField::Modifier))
            .expect("the modifier row");
        assert_eq!(
            modifier.value,
            Value::Index(crate::config::switcher_modifier_index_of("control"))
        );
    }

    #[test]
    fn page_view_search_entries_are_values_not_side_effects() {
        let _guard = sidebar::lock_search_index_for_test();
        let entries_before =
            sidebar::recorded_search_rows(settings_page_index(SettingsPageId::Switcher));
        let view = switcher();
        // A pure description; producing it never writes to the index.
        assert_eq!(
            sidebar::recorded_search_rows(settings_page_index(SettingsPageId::Switcher)),
            entries_before
        );
        assert_eq!(view.rows[0].search.label, t("settings.row_windows_enabled"));
        assert_eq!(
            view.rows[0].search.desc.as_deref(),
            Some(t("settings.desc_windows_enabled").as_str())
        );
        assert_eq!(view.rows[0].search.section, t("settings.header_windows"));
        // A dropdown's choices are part of what the search matches.
        let thumbnail_size = &view.rows[7];
        assert!(!thumbnail_size.search.options.is_empty());
        assert!(matches!(thumbnail_size.control, ControlKind::Popup { .. }));
    }

    #[test]
    fn a_migrated_page_indexes_every_row_the_inline_sweep_used_to() {
        // The sweep the inline pages ran filled a row's entry from the strings its views render.
        // A migrated page must index the same rows with the same baseline text, or settings
        // search silently stops finding a setting it used to find.
        let view = switcher();
        let page = settings_page_index(SettingsPageId::Switcher);
        let _guard = sidebar::lock_search_index_for_test();
        sidebar::begin_settings_search_index();
        sidebar::begin_settings_search_section();
        for (row, spec) in view.rows.iter().enumerate() {
            sidebar::record_settings_search_entry(
                crate::settings::sidebar::SearchEntry::row(
                    page,
                    row,
                    &spec.search.label,
                    spec.search.desc.as_deref(),
                    &spec.search.section,
                )
                .with_options(&spec.search.options),
            );
        }
        // The inline sweep's own invariant: every row of the page has exactly one entry.
        let recorded = sidebar::recorded_search_rows(page);
        assert_eq!(recorded.len(), view.rows.len());
        for row in 0..view.rows.len() {
            assert!(recorded.contains(&row), "row {row} is not searchable");
        }
        // And each entry carries the description's OWN copy, not the sweep's fallback label: a
        // query for a row's label finds exactly that row.
        for (row, spec) in view.rows.iter().enumerate() {
            let hits = sidebar::match_settings_search_for_page(page, &spec.search.label);
            assert!(
                hits.contains(&row),
                "row {row} ({}) is not found by its own label",
                spec.search.label
            );
        }
        sidebar::begin_settings_search_index();
    }

    #[test]
    fn the_description_groups_are_the_ones_the_renderer_registers() {
        // A conditional row hides through the canvas's group list; a group the description names
        // but the renderer never registers would never hide, and one it registers without a row
        // would hide nothing. The renderer's list is asserted through the same function it uses.
        for (page, expected) in [
            (SettingsPageId::Switcher, vec![RowGroup::ThumbnailOnly]),
            (SettingsPageId::General, vec![RowGroup::GlassStyle]),
        ] {
            let view = view_for(page, &input());
            assert_eq!(description_groups(&view), expected, "{page:?}");
        }
    }

    #[test]
    fn the_update_description_carries_the_control_value_and_its_display_separately() {
        // The status-bar slider's domain is stop indices (0 = hidden, 1..=8 = 13..=20pt) while its
        // readout shows the point size: a refill that wrote the index to both showed "1" where
        // the page says "13".
        let mut config = Config::default();
        config.fonts.status_bar_size = 13.0;
        let input = PageInput::from_config(config, None, crate::theme::ThemeInput::current());
        let view = switcher_page_view(&input);
        let updates = page_update_description(&input, &[SettingsPageId::Switcher]);
        let update = updates
            .iter()
            .find(|update| update.field == ControlField::StatusBarTextSize)
            .expect("the footer text-size row");
        assert_eq!(update.value, "1", "the slider rests on stop 1");
        assert_eq!(update.display, "13", "the readout shows the point size");
        assert_ne!(
            update.value, update.display,
            "stop index and point size differ"
        );
        // A field whose readout IS its value keeps the two equal.
        let card_text = updates
            .iter()
            .find(|update| update.field == ControlField::CardTextSize)
            .unwrap();
        assert_eq!(card_text.value, card_text.display);
        let _ = view;
    }

    #[test]
    fn a_failed_publish_leaves_the_previous_mapping_untouched() {
        // The build's own counter-example: a page whose new entries collide with a live entry it
        // does not replace must be refused WITHOUT the old entries being removed first. The first
        // version unregistered before it could fail, which left the page's controls present and
        // unfindable — "looks alive but is not wired".
        let mut table = ControlTable::default();
        let old_theme = 0x1000 as *mut AnyObject;
        table
            .register(old_theme, ControlId::Field(ControlField::Theme))
            .unwrap();
        // The rest of the page's previous controls.
        let old_locale = 0x1001 as *mut AnyObject;
        table
            .register(old_locale, ControlId::Field(ControlField::Locale))
            .unwrap();
        // A new page that reuses the locale control's ADDRESS for a different identity: the old
        // locale entry is not the one being replaced (Theme is), so the build must be refused.
        let mut page = PageRegistrations::default();
        page.register(
            0x2000 as *mut AnyObject,
            ControlId::Field(ControlField::Theme),
        )
        .unwrap();
        page.register(old_locale, ControlId::Field(ControlField::WindowsEnabled))
            .unwrap();
        assert_eq!(
            page.publish(&mut table),
            Err(RegisterError::DuplicatePointer)
        );
        // Nothing moved: the page's two controls still resolve exactly as before.
        assert_eq!(
            table.lookup(old_theme),
            Some(ControlId::Field(ControlField::Theme))
        );
        assert_eq!(
            table.lookup(old_locale),
            Some(ControlId::Field(ControlField::Locale))
        );
    }

    #[test]
    fn a_successful_publish_replaces_the_pages_own_entries() {
        let mut table = ControlTable::default();
        let old_theme = 0x1000 as *mut AnyObject;
        table
            .register(old_theme, ControlId::Field(ControlField::Theme))
            .unwrap();
        let mut page = PageRegistrations::default();
        let new_theme = 0x2000 as *mut AnyObject;
        page.register(new_theme, ControlId::Field(ControlField::Theme))
            .unwrap();
        assert_eq!(page.publish(&mut table), Ok(()));
        assert_eq!(table.lookup(old_theme), None);
        assert_eq!(
            table.lookup(new_theme),
            Some(ControlId::Field(ControlField::Theme))
        );
    }

    #[test]
    fn every_settings_ui_slot_a_migrated_field_needs_is_bound_at_publish() {
        // HIGH 2's counter-example, in its pure half: after the swap the only thing that filled
        // `ui.theme` / `ui.windows_enabled` / ... was the deleted inline builder, so the table
        // resolved a field whose pointer field was still null and `apply_control_field` read NULL.
        // The binding table must cover every field these pages declare and nothing else.
        let input = input();
        for page in [SettingsPageId::Switcher, SettingsPageId::General] {
            let view = view_for(page, &input);
            for row in view.rows.iter().filter(|row| row.field().is_some()) {
                let field = row.field().unwrap();
                assert!(
                    crate::settings::page_view::settings_ui_slot_field(field).is_some(),
                    "{page:?} {field:?} has a row but no SettingsUi slot, so its consumer reads NULL"
                );
            }
        }
        // The readout labels a slider row needs are bound too.
        for field in [ControlField::CardTextSize, ControlField::StatusBarTextSize] {
            assert!(crate::settings::page_view::settings_ui_readout_field(field));
        }
    }

    /// A page canvas over a stand-in document (no window: the canvas's layout walk never messages
    /// the document except to count and re-place subviews, which a null view does not answer).
    #[test]
    fn collapsing_a_group_moves_every_placed_view_with_its_row() {
        // The rule the re-flow applies, through the SAME function the canvas calls. Collapsing the
        // ThumbnailOnly group above a row moves that row up by the rows and gaps it consumed; the
        // row's copy/control/readout must follow it, keeping their offsets, and the control must
        // keep its distance to the card's top edge -- not stay at its build-time frame.
        let input = input();
        let view = switcher_page_view(&input);
        let layout_input = LayoutInput::for_test(600.0);
        let metrics = PageMetrics::for_layout(layout_input.layout);
        let layout = layout_rows(&view, &layout_input, &metrics);
        // The ThumbnailOnly rows are 5..=7 in the window-options card; a row below them in the
        // SAME card is the card-text-size row (index 8).
        let hidden_rows = 5..=7;
        let thumbnail_card = view.rows[5].card;
        let below = layout
            .rows
            .iter()
            .enumerate()
            .find(|(index, _)| {
                !hidden_rows.contains(index)
                    && *index > 7
                    && view.rows[*index].card == thumbnail_card
            })
            .map(|(_, row)| row)
            .expect("a row below the thumbnail group");
        let moved_up: f64 = hidden_rows
            .clone()
            .map(|index| layout.rows[index].frame.h)
            .sum();
        let new_origin = below.frame.y + moved_up;
        let offsets: Vec<NSPoint> = {
            let mut out = Vec::new();
            // The offsets are what `place_pending_view` records: box origin minus row origin.
            for box_ in row_placement_boxes(&view.rows[8], &layout.rows[8]) {
                out.push(NSPoint::new(box_.x, box_.y - below.frame.y));
            }
            out
        };
        for (index, offset) in offsets.iter().enumerate() {
            let origin = crate::settings::page_canvas::placed_view_origin(new_origin, *offset);
            let built = row_placement_boxes(&view.rows[8], &layout.rows[8])[index];
            assert!(
                (origin.x - built.x).abs() < 1e-9,
                "view {index} keeps its column"
            );
            assert!(
                (origin.y - (built.y + moved_up)).abs() < 1e-9,
                "view {index} moved up by exactly the hidden rows' height, not stayed at its build frame"
            );
            assert!(
                (origin.y - built.y).abs() > 1.0,
                "view {index} must NOT still be at its build-time frame"
            );
        }
        // The control keeps its offset FROM ITS ROW: the row moved, and the control moved with it.
        let control_offset = offsets[1];
        let control_origin =
            crate::settings::page_canvas::placed_view_origin(new_origin, control_offset);
        assert!(
            (control_origin.y - (new_origin + control_offset.y)).abs() < 1e-9,
            "the control stays at the layout's offset inside its row"
        );
        assert!(
            (new_origin - below.frame.y - moved_up).abs() < 1e-9,
            "the row itself moved up by the hidden rows"
        );
    }
    #[test]
    fn the_banner_children_lie_inside_their_strip() {
        // The label and the button are subviews of the strip, so their boxes are in the strip's
        // coordinates. Adding the strip's origin to them (the first version) put both children
        // one strip-height above the strip: this fails at origin.y >= strip.y for a zero-origin
        // strip and for every strip placed below the document top.
        let input = input();
        let view = general_page_view(&input);
        let layout_input = LayoutInput::for_test(600.0);
        let metrics = PageMetrics::for_layout(layout_input.layout);
        let layout = layout_rows(&view, &layout_input, &metrics);
        let banner = layout.banner.as_ref().expect("General carries the banner");
        for (name, child) in [("label", banner.label), ("button", banner.button)] {
            assert!(
                child.y >= -0.5 && child.y + child.h <= banner.strip.h + 0.5,
                "the {name} must lie inside the strip's height: child.y={} h={} strip.h={}",
                child.y,
                child.h,
                banner.strip.h
            );
            assert!(
                child.x >= -0.5 && child.x + child.w <= banner.strip.w + 0.5,
                "the {name} must lie inside the strip's width"
            );
        }
    }

    #[test]
    fn every_view_the_renderer_creates_is_released_exactly_once() {
        // #3/#1 rule: each +1 from alloc/init is given up EXACTLY ONCE. The banner strip's is
        // given up by `finalize_settings_pages` (it re-parents the strip to the content view and
        // releases the reference the page build handed over), so `render_page` must NOT release
        // it too: a second release would steal the parent's own reference.
        let view_src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/settings/page_view/mod.rs"
        ))
        .expect("the page renderer's own source");
        let page_start = view_src.find("pub(super) unsafe fn render_page(").unwrap();
        let banner_start = view_src[page_start..]
            .find("if let (Some(banner), Some(spec))")
            .map(|offset| page_start + offset)
            .unwrap();
        let banner_end = view_src[banner_start..]
            .find("for (card_index, card) in layout.cards")
            .map(|offset| banner_start + offset)
            .unwrap();
        let banner = &view_src[banner_start..banner_end];
        let body = &view_src[page_start..];
        let mut release_sites: Vec<&str> = Vec::new();
        for pattern in ["release_obj(label)", "release_obj(button)"] {
            let count = banner.matches(pattern).count();
            release_sites.push(pattern);
            assert!(
                count <= 1,
                "{pattern} appears {count} times: a view must be released once"
            );
        }
        let strip_release = format!("release_obj({})", "strip");
        assert!(
            !body.contains(&strip_release),
            "the strip's reference belongs to finalize_settings_pages, not to render_page"
        );
        // The strip's one release lives in the finalize step.
        let builder_src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/settings/page_builder.rs"
        ))
        .expect("the page builder's own source");
        assert_eq!(
            builder_src
                .matches("release_obj(ui.permission_warning_view)")
                .count(),
            1,
            "the banner strip is released exactly once, by finalize_permission_banner"
        );
        // The other created views are balanced inside the renderer.
        let allocs = body.matches("class!(NSTextField), alloc]").count()
            + body.matches("class!(NSView), alloc]").count()
            - 1; // the strip, released by finalize
        let releases = body.matches("release_obj(label)").count()
            + body.matches("release_obj(button)").count()
            + body.matches("release_obj(control)").count();
        assert!(
            releases >= allocs,
            "render_page allocates {allocs} views itself but balances only {releases}"
        );
        let widgets = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/settings/page_widgets.rs"
        ))
        .expect("the page widgets' own source");
        for name in ["stacked_row_labels", "plain_row_label"] {
            let start = widgets.find(&format!("fn {name}(")).unwrap();
            let end = widgets[start..]
                .find("\n}")
                .map(|offset| start + offset)
                .unwrap();
            let body = &widgets[start..end];
            let allocs = body.matches("class!(NSTextField), alloc]").count()
                + body.matches("class!(NSView), alloc]").count();
            let releases = body.matches("release_obj(").count();
            assert!(
                releases >= allocs,
                "{name}: {allocs} allocs, {releases} releases"
            );
        }
    }
    #[test]
    fn every_migrated_row_uses_the_shared_label_association() {
        // The row path must reuse SettingsRow::register_label (accessibility label + the
        // ROW_LABELS pairing the freeze lists read), not re-implement it. Asserted over the
        // renderer's own source, because the pairing itself lives in an AppKit-only registry the
        // M2 runner drives.
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/settings/page_view/mod.rs"
        ))
        .expect("the page renderer's own source");
        let start = source.find("unsafe fn render_row(").expect("render_row");
        let end = source[start..]
            .find("\n/// Put a view in the box the layout computed.")
            .map(|offset| start + offset)
            .expect("the end of render_row");
        let body = &source[start..end];
        assert!(
            body.contains("SettingsRow::register_label(title_label, control)"),
            "a migrated row must register its label with its control"
        );
        assert!(
            !body.contains("setAccessibilityLabel"),
            "the row path must not re-implement the accessibility label"
        );
    }
    #[test]
    fn a_rows_current_origin_is_what_a_placement_follows() {
        // #3: after a re-flow the canvas's current row origin differs from the build-time
        // PageLayout origin; a placement must follow the FORMER. The occurrence the runner checks
        // is AppKit-only, so this asserts the data it checks: `current_row_origin` is the value
        // `placements_are_stale` and the checker compare against, and it is not the layout's.
        let input = input();
        let view = switcher_page_view(&input);
        let layout_input = LayoutInput::for_test(600.0);
        let metrics = PageMetrics::for_layout(layout_input.layout);
        let layout = layout_rows(&view, &layout_input, &metrics);
        // The same arithmetic the canvas walks: a collapsed ThumbnailOnly group moves every row
        // below it up by the rows it hid.
        let hidden_h: f64 = (5..=7).map(|index| layout.rows[index].frame.h).sum();
        let below = 8;
        let reflowed_origin = layout.rows[below].frame.y + hidden_h;
        assert!(
            (reflowed_origin - layout.rows[below].frame.y).abs() > 1.0,
            "the re-flow origin differs from the build-time origin, which is why the checker must read the canvas"
        );
    }
    #[test]
    fn the_banner_strip_is_placed_in_its_final_parents_coordinates() {
        // #2: the strip is re-parented to the settings CONTENT view, so its frame comes from
        // `banner_parent_frame` -- the inline builder's own box (page_x, the VISIBLE viewport's
        // top, content_w x banner_h) -- not from the page document's height and not from x = 0.
        let input = input();
        let view = general_page_view(&input);
        let mut layout_input = LayoutInput::for_test(600.0);
        let page_x = 340.0;
        let page_viewport_h = 700.0;
        layout_input.banner_frame = BannerFrame::of(banner_parent_frame(
            page_x,
            page_viewport_h,
            600.0,
            banner_height(false),
        ));
        let metrics = PageMetrics::for_layout(layout_input.layout);
        let layout = layout_rows(&view, &layout_input, &metrics);
        let banner = layout.banner.as_ref().expect("General carries the banner");
        assert_eq!(
            banner.strip.x, page_x,
            "the strip starts at the detail pane's inset"
        );
        assert_eq!(banner.strip.w, 600.0);
        assert_eq!(banner.strip.h, banner_height(false));
        let top = banner.strip.y + banner.strip.h;
        assert!(
            (top - (page_viewport_h - SettingsPageHeader::TOP_PADDING)).abs() < 1e-9,
            "the strip hangs from the visible viewport's top, not the page document's"
        );
    }

    #[test]
    fn build_page_lays_the_general_banner_out_from_the_page_context() {
        // The PRODUCTION wiring: `page_layout_input` is the one place `build_page` builds a page's
        // layout inputs from its context. Given General's context values, the box it produces must
        // carry that page_x and that VISIBLE page_viewport_h -- read back from the returned
        // LayoutInput, not computed by this test.
        let input = input();
        let view = general_page_view(&input);
        let page_x = 412.0;
        let page_viewport_h = 634.0;
        let doc_h = 1320.0;
        let content_w = 600.0;
        let layout_input = page_layout_input(
            &view,
            &input,
            content_w,
            SettingsLayout::new(content_w),
            doc_h,
            111.0,
            page_x,
            page_viewport_h,
        );
        assert_eq!(
            layout_input.banner_frame.x, page_x,
            "the banner's box carries the context's page_x"
        );
        assert_eq!(
            layout_input.banner_frame.y,
            page_viewport_h - SettingsPageHeader::TOP_PADDING - banner_height(false),
            "the banner hangs from the VISIBLE viewport, not the page document"
        );
        assert_eq!(
            layout_input.doc_top, doc_h,
            "the rows are laid out from the document height"
        );
        // ... and the strip `layout_rows` places from it is exactly that box.
        let metrics = PageMetrics::for_layout(layout_input.layout);
        let layout = layout_rows(&view, &layout_input, &metrics);
        let strip = layout.banner.as_ref().unwrap().strip;
        assert_eq!(
            (strip.x, strip.y, strip.w, strip.h),
            (
                page_x,
                page_viewport_h - SettingsPageHeader::TOP_PADDING - banner_height(false),
                content_w,
                banner_height(false)
            )
        );
    }
    #[test]
    fn the_separator_belongs_to_the_row_below_it() {
        // #4: the divider is created AFTER its row is opened and handed to the row's own
        // placements, so a re-flow moves it and a hidden group hides it. Both facts are asserted:
        // its box (the row's top edge) and that `render_row` places it through the row.
        let input = input();
        let view = switcher_page_view(&input);
        let layout_input = LayoutInput::for_test(600.0);
        let metrics = PageMetrics::for_layout(layout_input.layout);
        let layout = layout_rows(&view, &layout_input, &metrics);
        let row = view
            .rows
            .iter()
            .position(|row| row.separator_above)
            .expect("a row with a divider above it");
        let placed = &layout.rows[row];
        let separator = separator_frame(placed);
        assert_eq!(separator.origin.y, placed.frame.y + placed.frame.h);
        assert_eq!(separator.size.width, placed.frame.w);
        // The renderer places it on the row's own capture, not before the row opens.
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/settings/page_view/mod.rs"
        ))
        .expect("the page renderer's own source");
        let start = source.find("unsafe fn render_row(").expect("render_row");
        let body = &source[start..source.find("pub(super) fn separator_frame(").unwrap()];
        let open = body.find("canvas.next_row(").expect("the row opens");
        let divider = body
            .find("SettingsRow::separator_above_row(")
            .expect("the divider is built");
        assert!(
            divider > open,
            "the divider must be created after its row opened, or the canvas captures it for the previous row"
        );
        assert!(
            body.contains("canvas.place_pending_view(separator, separator_frame(placed))"),
            "the divider must go through the row's placements, so a re-flow moves it and its group hides it"
        );
        // And the card loop no longer builds dividers before opening their row.
        let page_start = source
            .find("pub(super) unsafe fn render_page(")
            .expect("render_page");
        let page = &source[page_start
            ..source
                .find("pub(super) unsafe fn apply_control_update")
                .unwrap()];
        assert!(
            !page.contains("SettingsRow::separator_above_row("),
            "render_page must not create dividers: they belong to the row's own capture"
        );
    }

    #[test]
    fn a_description_with_a_duplicate_identity_is_refused_before_any_view_exists() {
        // MEDIUM 3: the identity check reads the description, so the refusal happens before the
        // page builds a view. This is the production validator (`build_page`'s first step) applied
        // to a page whose rows repeat an identity.
        let input = input();
        let mut view = switcher_page_view(&input);
        view.rows[1].id = view.rows[0].id.clone();
        assert_eq!(
            description_identity_conflict(&view),
            Err(RegisterError::DuplicateId)
        );
        // The clean description passes the same check.
        let clean = switcher_page_view(&input);
        assert_eq!(description_identity_conflict(&clean), Ok(()));
    }

    #[test]
    fn the_windows_master_switch_keeps_its_dedicated_callback() {
        // MEDIUM 5: the unified handler does not freeze the options below the switch, so the
        // switch's own callback must stay bound to it; every other static control uses the unified
        // one.
        assert_eq!(
            dedicated_selector(Some(ControlField::WindowsEnabled)),
            sel!(handleWindowsEnabledToggle:)
        );
        assert_eq!(
            dedicated_selector(Some(ControlField::ShowMinimized)),
            sel!(handleControlChanged:)
        );
        assert_eq!(dedicated_selector(None), sel!(handleControlChanged:));
        // The callback it names exists and does exactly the freeze the disabled rows need.
        let _ = crate::settings::dispatch::handle_windows_enabled_toggle
            as extern "C" fn(*mut c_void, Sel, *mut c_void);
    }

    #[test]
    fn layout_rows_places_the_switcher_rows_without_gaps_or_overlaps() {
        let view = switcher();
        let layout_input = LayoutInput::for_test(600.0);
        let metrics = PageMetrics::for_layout(layout_input.layout);
        let layout = layout_rows(&view, &layout_input, &metrics);
        assert_eq!(layout.rows.len(), view.rows.len());
        // One card per run of rows: the page's first 16 rows are one card and the modifier row
        // is the second (a card that opens no row is not drawn).
        let first_card = &layout.cards[0];
        assert!((first_card.frame.y + first_card.frame.h - layout.first_card_top).abs() < 1e-9);
        // A card exists exactly where its index changes, and it steps the cursor once.
        let mut changes = 0usize;
        let mut previous = view.rows[0].card;
        for row in &view.rows[1..] {
            if row.card != previous {
                changes += 1;
                previous = row.card;
            }
        }
        assert_eq!(layout.cards.len(), changes + 1);
        // The card surface ends at its last row's origin and tops out at its own section cursor
        // (its card index in the walk, not its position among the drawn cards).
        let mut start = 0usize;
        for card in &layout.cards {
            let card_index = view.rows[start].card;
            let end = view.rows[start..]
                .iter()
                .rposition(|row| row.card == card_index)
                .map(|position| position + start)
                .unwrap();
            assert!((card.frame.y - layout.rows[end].frame.y).abs() < 1e-9);
            assert!(
                (card.frame.y + card.frame.h - layout.card_tops[card_index]).abs() < 1e-9,
                "card {card_index}: bottom {} top {} expected {}",
                card.frame.y,
                card.frame.y + card.frame.h,
                layout.card_tops[card_index]
            );
            start = end + 1;
        }
    }
    #[test]
    fn layout_rows_places_controls_inside_the_row_band() {
        let view = switcher();
        let layout_input = LayoutInput::for_test(600.0);
        let metrics = PageMetrics::for_layout(layout_input.layout);
        let layout = layout_rows(&view, &layout_input, &metrics);
        for (row, placed) in view.rows.iter().zip(layout.rows.iter()) {
            let control = &placed.control_frame;
            assert!(control.y >= placed.frame.y - 1e-9, "{row:?}");
            assert!(
                control.y + control.h <= placed.frame.y + placed.frame.h + 1e-9,
                "{row:?}"
            );
            // The copy never reaches into the control column.
            let label = &placed.label_frame;
            assert!(label.x + label.w <= layout_input.layout.control_x + 1e-9);
            if let Some(readout) = placed.readout_frame {
                assert!(readout.x >= control.x + control.w);
            }
        }
    }

    #[test]
    fn layout_rows_sizes_a_stacked_row_by_its_measured_caption() {
        let view = general();
        let mut layout_input = LayoutInput::for_test(600.0);
        let metrics = PageMetrics::for_layout(layout_input.layout);
        let index = view
            .rows
            .iter()
            .position(|row| row.caption_key.is_some())
            .unwrap();
        layout_input.text = view_measured(&view, index, 24.0);
        let layout = layout_rows(&view, &layout_input, &metrics);
        let placed = &layout.rows[index];
        // Title + gap + the caption that was measured (plus the row's own padding).
        assert!(placed.frame.h >= 18.0 + 3.0 + 24.0);
        let caption = placed.caption_frame.expect("a stacked row has a caption");
        assert!(caption.h >= 24.0 - 1e-9);
        assert!((caption.y + caption.h + 3.0 - placed.label_frame.y).abs() < 1e-9);
    }

    fn view_measured(view: &PageView, index: usize, caption_h: f64) -> MeasuredText {
        let mut text = MeasuredText::base();
        text.subtitle_heights = (0..view.rows.len())
            .map(|row| if row == index { caption_h } else { 0.0 })
            .collect();
        text
    }

    #[test]
    fn control_table_refuses_duplicates_of_identity_and_pointer() {
        let mut table = ControlTable::new();
        let first = 0x1000 as *mut AnyObject;
        let second = 0x2000 as *mut AnyObject;
        assert_eq!(
            table.register(first, ControlId::Field(ControlField::Theme)),
            Ok(())
        );
        assert_eq!(
            table.register(second, ControlId::Field(ControlField::Theme)),
            Err(RegisterError::DuplicateId)
        );
        assert_eq!(
            table.register(first, ControlId::Field(ControlField::Locale)),
            Err(RegisterError::DuplicatePointer)
        );
        // Nothing was added by the refused registrations.
        assert_eq!(table.len(), 1);
        assert_eq!(
            table.lookup(first),
            Some(ControlId::Field(ControlField::Theme))
        );
        assert_eq!(table.lookup(second), None);
    }

    #[test]
    fn control_table_unregister_and_cleanup_are_idempotent() {
        let mut table = ControlTable::new();
        let pointer = 0x1000 as *mut AnyObject;
        table
            .register(pointer, ControlId::Field(ControlField::Theme))
            .unwrap();
        assert!(table.unregister(&ControlId::Field(ControlField::Theme)));
        assert!(!table.unregister(&ControlId::Field(ControlField::Theme)));
        assert!(table.is_empty());
        assert_eq!(table.lookup(pointer), None);
        table.clear();
        assert!(table.is_empty());
    }

    #[test]
    fn a_refused_page_registration_leaves_the_old_mapping_intact() {
        let mut table = ControlTable::new();
        let old = 0x1000 as *mut AnyObject;
        table
            .register(old, ControlId::Field(ControlField::Theme))
            .unwrap();
        // The page rebuild registers a conflicting entry half-way through and is abandoned.
        let mut page = PageRegistrations::default();
        let new_theme = 0x2000 as *mut AnyObject;
        let locale = 0x3000 as *mut AnyObject;
        page.register(new_theme, ControlId::Field(ControlField::Theme))
            .unwrap();
        // The page's own list refuses the duplicate before anything reaches the shared table.
        assert_eq!(
            page.register(locale, ControlId::Field(ControlField::Theme)),
            Err(RegisterError::DuplicateId)
        );
        // Dropping the page's registrations publishes nothing.
        drop(page);
        assert_eq!(
            table.lookup(old),
            Some(ControlId::Field(ControlField::Theme)),
            "the previous mapping must still resolve to the same field"
        );
        assert_eq!(table.lookup(new_theme), None);
        assert_eq!(table.lookup(locale), None);
    }

    #[test]
    fn publishing_a_page_replaces_its_old_entries_and_no_stale_pointer_survives() {
        let mut table = ControlTable::new();
        let old_theme = 0x1000 as *mut AnyObject;
        let old_locale = 0x1001 as *mut AnyObject;
        let other = 0x1002 as *mut AnyObject;
        table
            .register(old_theme, ControlId::Field(ControlField::Theme))
            .unwrap();
        table
            .register(old_locale, ControlId::Field(ControlField::Locale))
            .unwrap();
        // A control from the page that is not being rebuilt keeps its entry.
        table
            .register(other, ControlId::Field(ControlField::WindowsEnabled))
            .unwrap();
        let new_theme = 0x2000 as *mut AnyObject;
        let new_locale = 0x2001 as *mut AnyObject;
        let mut page = PageRegistrations::default();
        page.register(new_theme, ControlId::Field(ControlField::Theme))
            .unwrap();
        page.register(new_locale, ControlId::Field(ControlField::Locale))
            .unwrap();
        page.publish(&mut table).unwrap();
        // The released addresses resolve to nothing; the new ones resolve to the same fields.
        assert_eq!(table.lookup(old_theme), None);
        assert_eq!(table.lookup(old_locale), None);
        assert_eq!(
            table.lookup(new_theme),
            Some(ControlId::Field(ControlField::Theme))
        );
        assert_eq!(
            table.lookup(new_locale),
            Some(ControlId::Field(ControlField::Locale))
        );
        assert_eq!(
            table.lookup(other),
            Some(ControlId::Field(ControlField::WindowsEnabled))
        );
        // Repeated cleanup is safe.
        assert!(table.unregister(&ControlId::Field(ControlField::Theme)));
        assert!(!table.unregister(&ControlId::Field(ControlField::Theme)));
    }

    #[test]
    fn the_update_description_matches_each_row_initial_value_item_by_item() {
        let input = input();
        for (page, view) in [
            (SettingsPageId::Switcher, switcher()),
            (SettingsPageId::General, general()),
        ] {
            let updates = page_update_description(&input, &[page]);
            let fields_with_values: Vec<ControlField> =
                view.rows.iter().filter_map(|row| row.field()).collect();
            assert_eq!(
                updates
                    .iter()
                    .map(|update| update.field)
                    .collect::<Vec<_>>(),
                fields_with_values,
                "{page:?}: one entry per config field, in row order"
            );
            let rows: Vec<&RowSpec> = view
                .rows
                .iter()
                .filter(|row| row.field().is_some())
                .collect();
            for (row, update) in rows.iter().zip(updates.iter()) {
                assert_eq!(update.value, row.value.describe(), "{page:?}: {update:?}");
                match (&row.control, row.value.clone()) {
                    (ControlKind::Switch, Value::Switch(_)) => {
                        assert!(update.value == "on" || update.value == "off");
                    }
                    (ControlKind::Popup { options }, Value::Index(index)) => {
                        assert!(
                            index < options.len(),
                            "the selected index is one of the choices"
                        );
                        assert_eq!(update.value, index.to_string());
                    }
                    (ControlKind::Slider { .. }, Value::Slider(value)) => {
                        assert_eq!(update.value, value.to_string());
                    }
                    (ControlKind::Button { .. }, Value::None) => {
                        panic!("an action row has no field")
                    }
                    _ => panic!("the kind and the value agree"),
                }
            }
        }
    }
    #[test]
    fn every_static_state_control_of_a_migrated_page_appears_in_the_update_description() {
        let input = input();
        let updates: Vec<ControlField> =
            page_update_description(&input, &[SettingsPageId::Switcher, SettingsPageId::General])
                .into_iter()
                .map(|update| update.field)
                .collect();
        for page in [SettingsPageId::Switcher, SettingsPageId::General] {
            for row in view_for(page, &input).rows {
                if matches!(row.control, ControlKind::Button { .. }) {
                    continue;
                }
                if let Some(field) = row.field() {
                    assert!(
                        updates.contains(&field),
                        "{page:?} {field:?} is missing from the description"
                    );
                }
            }
        }
    }

    #[test]
    fn a_field_change_makes_the_in_place_sync_needed() {
        // The pair the checklist calls steps 13/14: a flag that makes the in-place sync run, and a
        // description that carries the new value when it does.
        let old = Config::default();
        let mut new = Config::default();
        new.layout.show_app_name_in_cards = !old.layout.show_app_name_in_cards;
        assert!(
            crate::runtime_config::change_needs_in_place_control_sync(&old, &new),
            "flipping a Switcher page field must make the open settings window re-read it"
        );
        let input = PageInput::from_config(new, None, crate::theme::ThemeInput::current());
        let update = page_update_description(&input, &[SettingsPageId::Switcher])
            .into_iter()
            .find(|update| update.field == ControlField::ShowAppNameInCards)
            .expect("the changed field is in the description");
        assert_eq!(update.value, "on");
    }

    /// Every page's real description must pass the identity check the build runs before creating a
    /// view: a duplicate there refuses the whole page and it renders empty. This is the guard the
    /// About page's two links needed and did not have.
    #[test]
    fn every_pages_description_passes_the_identity_check_the_build_runs() {
        let input =
            PageInput::from_config(Config::default(), None, crate::theme::ThemeInput::current());
        for page in [
            SettingsPageId::Switcher,
            SettingsPageId::General,
            SettingsPageId::Clipboard,
            SettingsPageId::About,
            SettingsPageId::KeystrokeDisplay,
            SettingsPageId::FinderMenu,
            SettingsPageId::KeyBindings,
            SettingsPageId::Mouse,
        ] {
            let view = view_for(page, &input);
            assert!(
                description_identity_conflict(&view).is_ok(),
                "{page:?} declares a duplicate control identity, so its build would be refused"
            );
        }
    }

    /// Every row — described or page-drawn — must belong to a card the layout produced, and the
    /// layout must preserve the card's OWN index. A page whose cards mix described and page-drawn
    /// rows (Mouse: 0,1,2,3,4,5 with 1 and 3 page-only) once lost every card after the gap because
    /// the layout and the renderer both used a positional index.
    #[test]
    fn every_card_and_row_survives_the_layout_for_every_page() {
        let input =
            PageInput::from_config(Config::default(), None, crate::theme::ThemeInput::current());
        for page in [
            SettingsPageId::Switcher,
            SettingsPageId::General,
            SettingsPageId::Clipboard,
            SettingsPageId::About,
            SettingsPageId::KeystrokeDisplay,
            SettingsPageId::FinderMenu,
            SettingsPageId::KeyBindings,
            SettingsPageId::Mouse,
        ] {
            let view = view_for(page, &input);
            let mut layout_input = LayoutInput::for_test(700.0);
            layout_input.extra_row_heights = view
                .extra_rows
                .iter()
                .map(|extra| extra.height.resolve(&input, layout_input.content_w))
                .collect();
            let metrics = PageMetrics::for_layout(layout_input.layout);
            let layout = layout_rows(&view, &layout_input, &metrics);
            let highest = view
                .rows
                .iter()
                .map(|row| row.card)
                .chain(view.extra_rows.iter().map(|extra| extra.card))
                .max()
                .unwrap_or(0);
            assert_eq!(
                layout.cards.len(),
                highest + 1,
                "{page:?} must produce one card per card index, page-drawn-only cards included"
            );
            for card in &layout.cards {
                assert!(
                    (0..=highest).contains(&card.index),
                    "{page:?} produced a card with an out-of-range index"
                );
            }
            let mut indices: Vec<usize> = layout.cards.iter().map(|card| card.index).collect();
            indices.sort_unstable();
            assert_eq!(
                indices,
                (0..=highest).collect::<Vec<_>>(),
                "{page:?} must keep every card index, in order"
            );
            // A card that holds only page-drawn rows still names itself.
            for card in &layout.cards {
                let has_described = view.rows.iter().any(|row| row.card == card.index);
                if !has_described {
                    assert!(
                        !card.section.is_empty(),
                        "{page:?}: a page-drawn-only card must still name its heading"
                    );
                }
            }
        }
    }

    /// Every field a migrated page describes must have a `SettingsUi` slot, because the legacy
    /// consumers (`apply_control_field`, the refill, the enable lists) read those fields directly:
    /// a field the table identifies but nothing binds is a control that looks alive and does
    /// nothing. This is the assertion the Clipboard/Mouse/About pages needed and did not have.
    #[test]
    fn every_described_field_of_every_page_has_a_settings_ui_slot() {
        let input =
            PageInput::from_config(Config::default(), None, crate::theme::ThemeInput::current());
        for page in [
            SettingsPageId::Switcher,
            SettingsPageId::General,
            SettingsPageId::Clipboard,
            SettingsPageId::About,
            SettingsPageId::KeystrokeDisplay,
            SettingsPageId::FinderMenu,
            SettingsPageId::KeyBindings,
            SettingsPageId::Mouse,
        ] {
            let view = view_for(page, &input);
            for row in &view.rows {
                let Some(field) = row.field() else {
                    continue; // an action row stores nothing
                };
                // The Finder Menu page's catalog rows are POSITIONAL (one entry per file type /
                // terminal); they are bound into their Vec fields by the page's own hook, not by
                // the one-slot-per-field table. Every other described field must have a slot.
                if matches!(
                    field,
                    ControlField::FinderMenuNewFileType(_) | ControlField::FinderMenuTerminal(_)
                ) {
                    continue;
                }
                assert!(
                    settings_ui_slot_present(field),
                    "{page:?}: {field:?} is described but no SettingsUi slot is bound for it"
                );
            }
        }
    }

    /// The fields whose controls had a DEDICATED handler in the inline builder must still have one:
    /// each does more than write the config (retitles a button, freezes dependent rows, restarts a
    /// tap, re-reads a device), and losing it is silent — the value changes and the page stops
    /// following it. The list is the old builders' `sel!(handle…:)` assignments.
    #[test]
    fn every_field_with_a_dedicated_handler_still_has_one() {
        let dedicated = [
            (ControlField::WindowsEnabled, "handleWindowsEnabledToggle:"),
            (ControlField::MouseEnabled, "handleEnableMouseToggle:"),
            (ControlField::MappingEnabled, "handleMappingEnabledChanged:"),
            (
                ControlField::ClipboardEnabled,
                "handleClipboardEnabledToggle:",
            ),
            (ControlField::ClipboardPaused, "handlePauseRecordingToggle:"),
            (ControlField::KeyBindingsEnabled, "handleKeyBindingsToggle:"),
        ];
        for (field, selector) in dedicated {
            let resolved = dedicated_selector(Some(field));
            assert_eq!(
                resolved.name().to_str().expect("a selector name is UTF-8"),
                selector,
                "{field:?} lost its dedicated handler and now runs the generic one"
            );
        }
        assert_eq!(
            dedicated_selector(Some(ControlField::Theme))
                .name()
                .to_str()
                .expect("a selector name is UTF-8"),
            "handleControlChanged:",
            "an ordinary field still uses the unified handler"
        );
    }

    /// A page-drawn row must consume the height the LAYOUT reserved for it, at the origin the
    /// layout put it, and a card must cover the rows the page draws inside it. Mouse is the page
    /// this went wrong on: its smooth-slider row was handed the device row's height, and its
    /// page-drawn-only cards had no surface.
    #[test]
    fn page_drawn_rows_are_laid_out_at_their_own_height_and_cards_cover_them() {
        let input =
            PageInput::from_config(Config::default(), None, crate::theme::ThemeInput::current());
        for page in [
            SettingsPageId::Switcher,
            SettingsPageId::General,
            SettingsPageId::Clipboard,
            SettingsPageId::About,
            SettingsPageId::KeystrokeDisplay,
            SettingsPageId::FinderMenu,
            SettingsPageId::KeyBindings,
            SettingsPageId::Mouse,
        ] {
            let view = view_for(page, &input);
            let mut layout_input = LayoutInput::for_test(700.0);
            layout_input.extra_row_heights = view
                .extra_rows
                .iter()
                .map(|extra| extra.height.resolve(&input, layout_input.content_w))
                .collect();
            let metrics = PageMetrics::for_layout(layout_input.layout);
            let layout = layout_rows(&view, &layout_input, &metrics);
            assert_eq!(layout.drawn_rows.len(), view.extra_rows.len());
            assert_eq!(
                layout.sequence.len(),
                view.rows.len() + view.extra_rows.len()
            );
            // The declared height reaches the layout unchanged, and the row's origin is the
            // position in the sequence, not a per-card index into a page-wide array.
            for (index, extra) in view.extra_rows.iter().enumerate() {
                let (row_index, origin, height) = layout.drawn_rows[index];
                assert_eq!(row_index, index);
                let declared = extra.height.resolve(&input, layout_input.content_w);
                assert_eq!(
                    height, declared,
                    "{page:?}: row {index} ({}) was laid out at a different height than declared",
                    extra.key
                );
                assert!(origin <= layout.first_card_top);
                // The row sits inside the card's surface.
                let card = layout
                    .cards
                    .iter()
                    .find(|card| card.index == extra.card)
                    .expect("every card has a surface");
                assert!(
                    origin >= card.frame.y
                        && origin + height <= card.frame.y + card.frame.h + 1.0e-6,
                    "{page:?}: row {} ({}) is outside its card's surface",
                    index,
                    extra.key
                );
            }
        }
    }

    /// The sequence must place every described row exactly once and every page-drawn row exactly
    /// once, in card order: the renderer draws from it, so a missing entry is a row that never
    /// appears.
    #[test]
    fn the_sequence_places_every_row_exactly_once() {
        let input =
            PageInput::from_config(Config::default(), None, crate::theme::ThemeInput::current());
        for page in [
            SettingsPageId::Switcher,
            SettingsPageId::General,
            SettingsPageId::Clipboard,
            SettingsPageId::About,
            SettingsPageId::KeystrokeDisplay,
            SettingsPageId::FinderMenu,
            SettingsPageId::KeyBindings,
            SettingsPageId::Mouse,
        ] {
            let view = view_for(page, &input);
            let mut layout_input = LayoutInput::for_test(700.0);
            layout_input.extra_row_heights = view
                .extra_rows
                .iter()
                .map(|extra| extra.height.resolve(&input, layout_input.content_w))
                .collect();
            let metrics = PageMetrics::for_layout(layout_input.layout);
            let layout = layout_rows(&view, &layout_input, &metrics);
            let mut described: Vec<usize> = Vec::new();
            let mut drawn: Vec<usize> = Vec::new();
            for placed in &layout.sequence {
                match placed {
                    PlacedRow::Described(index) => described.push(*index),
                    PlacedRow::Drawn(index) => drawn.push(*index),
                }
            }
            assert_eq!(
                described,
                (0..view.rows.len()).collect::<Vec<_>>(),
                "{page:?}: every described row appears once, in document order"
            );
            assert_eq!(
                drawn,
                (0..view.extra_rows.len()).collect::<Vec<_>>(),
                "{page:?}: every page-drawn row appears once, in document order"
            );

            // Rows are ordered top to bottom: the sequence's origins descend, and two rows may
            // share an origin only when the lower one takes no space (the About page's inline
            // update host starts empty and grows over the band's place).
            let mut last = layout.first_card_top;
            let mut previous_zero_height = false;
            for placed in &layout.sequence {
                let (origin, height) = match placed {
                    PlacedRow::Described(index) => {
                        (layout.rows[*index].frame.y, layout.rows[*index].frame.h)
                    }
                    PlacedRow::Drawn(index) => {
                        let (_, origin, height) = layout.drawn_rows[*index];
                        (origin, height)
                    }
                };
                assert!(
                    origin <= last,
                    "{page:?}: the sequence must descend (origin={origin}, previous={last}, placed={placed:?})"
                );
                let _ = (height, previous_zero_height);
                previous_zero_height = height <= 0.0;
                last = origin;
            }
        }
    }

    /// Every control a page describes must be REACHABLE after the build: a config field through
    /// its `SettingsUi` slot, a value row's action button through the action identity that names
    /// it, and a slider's readout through the field its value belongs to. A control that is
    /// identified but never bound looks alive and does nothing — exactly the About status rows and
    /// the Clipboard readouts before this guard existed.
    #[test]
    fn every_described_control_has_a_binding() {
        let input =
            PageInput::from_config(Config::default(), None, crate::theme::ThemeInput::current());
        for page in [
            SettingsPageId::Switcher,
            SettingsPageId::General,
            SettingsPageId::Clipboard,
            SettingsPageId::About,
            SettingsPageId::KeystrokeDisplay,
            SettingsPageId::FinderMenu,
            SettingsPageId::KeyBindings,
            SettingsPageId::Mouse,
        ] {
            let view = view_for(page, &input);
            for row in &view.rows {
                match &row.control {
                    ControlKind::Slider { .. } => assert!(
                        row.field().is_some(),
                        "{page:?}: a slider reads a field, so its readout can be bound"
                    ),
                    ControlKind::Value { action, .. } => {
                        // A value with an action needs BOTH bindings named; a plain value has
                        // one (the label itself, bound by the refresh that writes it).
                        let expected = match (&row.id, action) {
                            (ControlId::Action(name), Some(_)) => Some(*name),
                            (ControlId::Action(name), None) => Some(*name),
                            _ => None,
                        };
                        assert!(
                            expected.is_some(),
                            "{page:?}: a value row is bound by an action identity"
                        );
                    }
                    _ => {}
                }
            }
        }
    }

    /// The canvas row a described row lands in is its position in the page's ONE sequence, not its
    /// index among described rows: Mouse's `reverse_scroll` is described row 1 but canvas row 4,
    /// because three rows of the device card are drawn by the page. The search scrolls to, and the
    /// M2 checker inspects, the canvas row — so a mapping that ignores the pure-hook cards sends
    /// both to a page-drawn row.
    #[test]
    fn the_canvas_row_mapping_counts_the_page_drawn_rows_of_every_card() {
        let input =
            PageInput::from_config(Config::default(), None, crate::theme::ThemeInput::current());
        for page in [
            SettingsPageId::Switcher,
            SettingsPageId::General,
            SettingsPageId::Clipboard,
            SettingsPageId::About,
            SettingsPageId::KeystrokeDisplay,
            SettingsPageId::FinderMenu,
            SettingsPageId::KeyBindings,
            SettingsPageId::Mouse,
        ] {
            let view = view_for(page, &input);
            let sequence = page_sequence(&view);
            let rows = canvas_rows(&view);
            // Every described row's canvas row is where the sequence puts it.
            for (index, row) in view.rows.iter().enumerate() {
                let position = sequence
                    .iter()
                    .position(
                        |placed| matches!(placed, PlacedRow::Described(drawn) if *drawn == index),
                    )
                    .expect("every described row is in the sequence");
                assert_eq!(
                    rows[index], position,
                    "{page:?}: described row {index} maps wrong"
                );
                assert_eq!(canvas_row_of_described_row(&view, index), position);
                let _ = row;
            }
            // The mapping is strictly increasing: the canvas rows follow the document order.
            assert!(rows.windows(2).all(|pair| pair[0] < pair[1]));
        }
        // The counterexample, named: Mouse's second described row sits after three page-drawn rows
        // (device, scroll mode, line count) and before the fourth card's own page-drawn rows.
        let mouse = view_for(SettingsPageId::Mouse, &input);
        assert_eq!(canvas_rows(&mouse), vec![0, 4, 10, 12]);
        assert_eq!(mouse.rows[1].label_key, "settings.row_reverse_scroll");
    }

    /// A card's section heading sits ABOVE its card, and the card's first row starts below the
    /// card's top edge. A heading that overlaps its own first row's control is the failure the
    /// settings-layout smoke reports as "views overlap" -- reproduced here without a window.
    #[test]
    fn every_card_heading_hangs_above_its_first_row_for_every_page() {
        let input =
            PageInput::from_config(Config::default(), None, crate::theme::ThemeInput::current());
        for page in [
            SettingsPageId::Switcher,
            SettingsPageId::General,
            SettingsPageId::Clipboard,
            SettingsPageId::About,
            SettingsPageId::KeystrokeDisplay,
            SettingsPageId::FinderMenu,
            SettingsPageId::KeyBindings,
            SettingsPageId::Mouse,
        ] {
            let view = view_for(page, &input);
            let mut layout_input = LayoutInput::for_test(700.0);
            layout_input.extra_row_heights = view
                .extra_rows
                .iter()
                .map(|extra| extra.height.resolve(&input, layout_input.content_w))
                .collect();
            let metrics = PageMetrics::for_layout(layout_input.layout);
            let layout = layout_rows(&view, &layout_input, &metrics);
            for card in &layout.cards {
                let header = card.header;
                // In the document's unflipped coordinates a larger y is HIGHER on screen: the
                // heading's bottom edge must sit above the card's top edge, and it must not
                // overlap the card's own surface.
                let card_top = card.frame.y + card.frame.h;
                assert!(
                    header.y >= card_top - 1.0e-6,
                    "{page:?}: card {} heading (y={}) is below its card top ({card_top}) with h={}",
                    card.index,
                    header.y,
                    header.h
                );
                assert!(
                    header.y >= card_top + header.h - 1.0e-6 || header.y > card_top,
                    "{page:?}: card {} heading ({header:?}) overlaps the card surface",
                    card.index
                );
            }
        }
    }

    /// A described row's label VIEW must sit inside its own row band.
    ///
    /// The regression this pins: the renderer built the plain row's label at the row's band origin
    /// with the band's height, while the band was also where the CONTROL was centred. The label's
    /// frame therefore reached 11pt above the card's top edge on every page, and the settings-layout
    /// smoke reported the first row of each card as a control overlapping the heading above it.
    /// The layout states the label's centred box; the view is built in the band and centred, so the
    /// box the smoke measures and the frame the reader sees agree.
    #[test]
    fn a_described_rows_label_box_sits_inside_its_row_band() {
        let input = input();
        for page in [
            SettingsPageId::Switcher,
            SettingsPageId::General,
            SettingsPageId::Mouse,
        ] {
            let view = view_for(page, &input);
            let layout_input = LayoutInput::for_test(620.0);
            let metrics = PageMetrics::for_layout(layout_input.layout);
            let layout = layout_rows(&view, &layout_input, &metrics);
            for (index, row) in layout.rows.iter().enumerate() {
                if row.frame.h <= 0.0 {
                    continue;
                }
                let label_box = row.label_frame;
                assert!(
                    label_box.y >= row.frame.y - 0.01,
                    "{page:?} row {index}: the label box starts {:.2} below the band's bottom edge",
                    row.frame.y - label_box.y
                );
                assert!(
                    label_box.y + label_box.h <= row.frame.y + row.frame.h + 0.01,
                    "{page:?} row {index}: the label box reaches {:.2} above the band's top edge",
                    label_box.y + label_box.h - (row.frame.y + row.frame.h)
                );
            }
        }
    }

    /// A captioned row's control is centred in the row, not pinned to its band's top edge.
    ///
    /// The inline builder centred every captioned row's control (`SettingsRow::captioned` calls
    /// `center_control`). The migrated layout pinned it to `top - control_h` whenever the copy was
    /// stacked, which lifted the switch 22pt above the row's middle on every captioned row --
    /// visible as a switch sitting at the row's top edge instead of beside its two lines.
    #[test]
    fn a_captioned_rows_control_is_centred_in_its_band() {
        let input = input();
        for page in [SettingsPageId::General, SettingsPageId::Switcher] {
            let view = view_for(page, &input);
            let layout_input = LayoutInput::for_test(620.0);
            let metrics = PageMetrics::for_layout(layout_input.layout);
            let layout = layout_rows(&view, &layout_input, &metrics);
            for (index, row) in layout.rows.iter().enumerate() {
                if view.rows[index].copy != RowCopy::Captioned || row.control_frame.h <= 0.0 {
                    continue;
                }
                let control_mid = row.control_frame.y + row.control_frame.h / 2.0;
                let band_mid = row.frame.y + row.frame.h / 2.0;
                assert!(
                    (control_mid - band_mid).abs() <= 1.0,
                    "{page:?} row {index}: the control's middle sits {:+.2}pt off the row's",
                    control_mid - band_mid
                );
            }
        }
    }

    /// A card holding only page-drawn rows must still carry a translated heading.
    ///
    /// Its heading is taken from its first page-drawn row's SECTION key; falling back to the row's
    /// own key printed the internal identifier (device) on the Mouse page where the design's
    /// heading (Device / 裝置) belongs.
    #[test]
    fn a_page_drawn_only_card_names_a_translated_heading() {
        let input = input();
        for page in [
            SettingsPageId::Mouse,
            SettingsPageId::Switcher,
            SettingsPageId::Clipboard,
        ] {
            let view = view_for(page, &input);
            let layout_input = LayoutInput::for_test(620.0);
            let metrics = PageMetrics::for_layout(layout_input.layout);
            let layout = layout_rows(&view, &layout_input, &metrics);
            for card in &layout.cards {
                let has_described = view.rows.iter().any(|row| row.card == card.index);
                if has_described {
                    continue;
                }
                assert!(
                    card.section.starts_with("settings."),
                    "{page:?} card {} names {:?} instead of a copy key",
                    card.index,
                    card.section
                );
                assert!(
                    t(card.section) != card.section,
                    "{page:?} card {} has no translation for {}",
                    card.index,
                    card.section
                );
            }
        }
    }

    /// A captioned row's text block is the width the inline builder gave it.
    ///
    /// The inline builder used two expressions: the default one that stops at the control column,
    /// and one wide block for the General page's menu-bar switch. Drawing every captioned row at one
    /// width wrapped the clipboard page's three-line warnings onto two lines and pushed the
    /// menu-bar promise to three.
    #[test]
    fn a_captioned_row_states_which_text_block_it_takes() {
        let input = input();
        let narrow = 600.0 - 180.0 - 16.0 - 16.0 - 16.0;
        for page in [SettingsPageId::General, SettingsPageId::Clipboard] {
            let view = view_for(page, &input);
            let layout = SettingsLayout::new(600.0);
            for row in view
                .rows
                .iter()
                .filter(|row| row.copy == RowCopy::Captioned)
            {
                let width = captioned_text_width(layout, 600.0, row);
                if row.wide_caption {
                    assert!(
                        width > narrow,
                        "{page:?} {}: a wide block must be wider than the default",
                        row.label_key
                    );
                } else {
                    assert!(!row.wide_caption);
                }
            }
        }
        // Exactly one row in the whole app takes the wide block.
        let wide: Vec<&str> = [SettingsPageId::General, SettingsPageId::Clipboard]
            .into_iter()
            .flat_map(|page| {
                let view = view_for(page, &input);
                view.rows
                    .iter()
                    .filter(|row| row.wide_caption)
                    .map(|row| row.label_key)
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(wide, vec!["settings.row_hide_menu_bar_icon"]);
    }

    /// A row's control draws its OWN copy, not the row's label.
    ///
    /// The inline builder passed the URL as the link's text and "Open" as the guide button's
    /// title. Drawing the row's label instead put "Website" where the URL belongs and made the
    /// guide button repeat its own row label.
    #[test]
    fn a_rows_control_draws_its_own_copy() {
        let input = input();
        let view = about_page_view(&input);
        let links: Vec<&'static str> = view
            .rows
            .iter()
            .filter_map(|row| match row.control {
                ControlKind::Link { text_key, .. } => Some(text_key),
                _ => None,
            })
            .collect();
        assert_eq!(
            links,
            vec!["settings.website_url", "settings.github_url"],
            "a link draws the URL, not the row label"
        );
        for key in links {
            assert_ne!(t(key), key, "{key} has no translation");
        }
        let buttons: Vec<&'static str> = view
            .rows
            .iter()
            .filter_map(|row| match row.control {
                ControlKind::Button { title_key } => Some(title_key),
                _ => None,
            })
            .collect();
        assert_eq!(buttons, vec!["settings.btn_open"]);
    }

    /// The About page's links and version sit in the page's own VALUE column.
    ///
    /// The inline builder put them at `label_x + 145` and let them run to the card's label inset,
    /// which is what fits a full URL; the shared control column is 180pt wide and truncated it.
    #[test]
    fn about_draws_its_value_column() {
        let input = input();
        let view = about_page_view(&input);
        let layout = SettingsLayout::new(620.0);
        let metrics = PageMetrics::for_layout(layout);
        let layout_input = LayoutInput::for_test(620.0);
        let rows = layout_rows(&view, &layout_input, &metrics);
        let value_x = layout.label_x + 145.0;
        let mut checked = 0;
        for (index, row) in view.rows.iter().enumerate() {
            if !row.value_column {
                continue;
            }
            checked += 1;
            assert!(
                (rows.rows[index].control_frame.x - value_x).abs() < 0.01,
                "About row {index} draws at x={}, not the value column's {value_x}",
                rows.rows[index].control_frame.x
            );
            let want_w = 620.0 - layout.label_x - value_x;
            assert!(
                (rows.rows[index].control_frame.w - want_w).abs() < 0.01,
                "About row {index} is {} wide, not {want_w}",
                rows.rows[index].control_frame.w
            );
        }
        assert!(
            checked >= 3,
            "the links and the version row take the value column"
        );
    }

    /// Every action button draws the copy its action names, not the row's label.
    ///
    /// The inline builder passed the button's own string (`Export`, `Import`, `Open`). Drawing the
    /// row label instead made the General page's three buttons all read "Open" and the About
    /// page's guide button repeat "View guide".
    #[test]
    fn an_action_button_draws_its_own_copy() {
        let input = input();
        // The Clipboard page's restore button is drawn by its hook, not described, so it is not
        // in this list: a described copy of it drew the action twice.
        let expected: [(&str, &str); 4] = [
            ("settings.row_export_logs", "settings.btn_export_logs"),
            (
                "settings.row_export_settings",
                "settings.btn_export_settings",
            ),
            (
                "settings.row_import_settings",
                "settings.btn_import_settings",
            ),
            ("settings.row_view_guide", "settings.btn_open"),
        ];
        let mut seen: Vec<(&str, &str)> = Vec::new();
        for page in [
            SettingsPageId::General,
            SettingsPageId::About,
            SettingsPageId::Clipboard,
        ] {
            let view = view_for(page, &input);
            for row in &view.rows {
                if let ControlKind::Button { title_key } = row.control {
                    seen.push((row.label_key, title_key));
                    assert_ne!(
                        title_key, row.label_key,
                        "{page:?} button row {} draws its own row label",
                        row.label_key
                    );
                    assert_ne!(t(title_key), title_key, "{title_key} has no translation");
                }
            }
        }
        assert_eq!(seen, expected);
    }

    /// No described row duplicates an action the page's own hook draws.
    ///
    /// The Clipboard page draws its excluded-applications card end to end -- the list, the add
    /// popup and the restore action. A static restore ROW in the description was drawn BEFORE the
    /// hook, so the same action appeared twice: once directly under the card heading and once at
    /// the card's end.
    #[test]
    fn no_described_row_duplicates_a_page_drawn_action() {
        let input = input();
        // Actions a page's hooks draw themselves, by page.
        let hook_drawn: [(SettingsPageId, &str); 1] =
            [(SettingsPageId::Clipboard, "restore_excluded")];
        for (page, action) in hook_drawn {
            let view = view_for(page, &input);
            let duplicated = view
                .rows
                .iter()
                .any(|row| matches!(row.id, ControlId::Action(id) if id == action));
            assert!(
                !duplicated,
                "{page:?} describes {action}, which its hook already draws"
            );
        }
    }

    /// A read-only value is TEXT: it shares its row label's origin, not a control box's.
    ///
    /// The inline builder fitted a value label to its own line height and centred that in the
    /// row band (`center_readonly_text_control`). Drawing it in the layout's 32pt control box put
    /// it 7.5pt low and 15pt too tall on every About value row. The layout still reserves a
    /// control-sized box -- the RENDERER fits the label into it and records the fitted frame --
    /// so what M1 can pin here is the box the layout hands over: a value row is a described row
    /// when it carries an action, and a single-line row when it does not.
    #[test]
    fn a_value_row_reserves_a_box_the_renderer_fits_the_label_into() {
        let input = input();
        let view = about_page_view(&input);
        let layout_input = LayoutInput::for_test(620.0);
        let metrics = PageMetrics::for_layout(layout_input.layout);
        let layout = layout_rows(&view, &layout_input, &metrics);
        let mut values = 0;
        for (index, row) in view.rows.iter().enumerate() {
            let ControlKind::Value { action, .. } = row.control else {
                continue;
            };
            values += 1;
            let placed = &layout.rows[index];
            // The renderer fits the label into this box and centres it in `placed.frame`; the
            // box must therefore be at least as tall as the row band's usable height, and the
            // band must be a full row (the fit is centred in it, not in the box).
            assert!(
                placed.control_frame.h > 0.0,
                "About row {index} reserves no control box"
            );
            assert!(
                (placed.control_frame.h - metrics.control_h).abs() < 0.01,
                "About row {index} reserves {}pt, not the control height",
                placed.control_frame.h
            );
            // A value WITH an action keeps the described row height (the button makes it two
            // lines); a bare value is a single-line row.
            let want_band = if action.is_some() {
                layout_input.layout.described_row_h
            } else {
                SettingsLayout::SINGLE_LINE_ROW_H
            };
            assert!(
                (placed.frame.h - want_band).abs() < 0.01,
                "About row {index} band is {}pt, not {want_band}",
                placed.frame.h
            );
        }
        assert_eq!(values, 4, "the About page's four value rows");
    }
    #[test]
    fn dbg_switcher_slider_frames() {
        let input = input();
        let view = switcher_page_view(&input);
        let layout_input = LayoutInput::for_test(620.0);
        let metrics = PageMetrics::for_layout(layout_input.layout);
        let layout = layout_rows(&view, &layout_input, &metrics);
        for (i, row) in view.rows.iter().enumerate() {
            if matches!(row.control, ControlKind::Slider { .. }) {
                let p = &layout.rows[i];
                println!(
                    "row {i} {} slider x={} w={} h={} readout={:?} content_w={}",
                    row.label_key,
                    p.control_frame.x,
                    p.control_frame.w,
                    p.control_frame.h,
                    p.readout_frame.map(|r| (r.x, r.y, r.w, r.h)),
                    layout_input.content_w
                );
            }
        }
    }

    /// The placement recorded for a view is THAT view's box, not the last box in its row.
    ///
    /// `row_placement_boxes` ends with the SLIDER READOUT for a slider row, so taking its last element
    /// as "the control's box" made the canvas record the readout's box as the slider's placement. The
    /// canvas re-applies a recorded placement on every reflow, so the slider was moved onto its own
    /// readout (136pt right on the Switcher page, past the document's edge) and the M2 layout smoke
    /// caught it as `OhMyTabSlider escapes document` plus a control/label overlap.
    #[test]
    fn a_slider_records_its_own_box_not_its_readouts() {
        let input = input();
        let view = switcher_page_view(&input);
        let layout_input = LayoutInput::for_test(620.0);
        let metrics = PageMetrics::for_layout(layout_input.layout);
        let layout = layout_rows(&view, &layout_input, &metrics);
        let mut sliders = 0;
        for (index, row) in view.rows.iter().enumerate() {
            if !matches!(row.control, ControlKind::Slider { .. }) {
                continue;
            }
            sliders += 1;
            let placed = &layout.rows[index];
            let boxes = row_placement_boxes(row, placed);
            // The row's boxes end with the readout, and the control's own frame is NOT that box.
            let last = *boxes.last().expect("a row places at least one box");
            assert!(
                (last.x - placed.control_frame.x).abs() > 0.01,
                "row {index}: the last placement box IS the control's box, so taking the last one",
            );
            assert!(
                (last.x - placed.readout_frame.expect("a slider has a readout").x).abs() < 0.01,
                "row {index}: the last placement box is not the readout's"
            );
        }
        assert_eq!(sliders, 2, "the Switcher page's two size sliders");
    }

    /// A page's description reads only its `PageInput`, and the input is captured from ONE config read.
    ///
    /// Two regressions this pins, both found by an external review:
    ///
    /// - `PageInput::capture` used to call `CONFIG.read()` twice inside one argument list. The first
    ///   guard lives until the end of the statement, so the second read was a NESTED read of a
    ///   `std::sync::RwLock` -- which parks when a writer queues between them -- and the outer guard
    ///   was held across the AppKit query `ThemeInput` makes for the "auto" appearance. A source
    ///   assertion is the only static form available: the deadlock needs a concurrent writer.
    /// - a description that calls into the system (the bundle version, a user default) stops being a
    ///   function of its input, and the banner's HEIGHT -- a layout input -- depended on it.
    #[test]
    fn a_description_reads_only_its_input_and_capture_reads_the_config_once() {
        let source = include_str!("mod.rs");
        let general = include_str!("general.rs");
        let capture_start = source
            .find("    pub(super) fn capture() -> Self {")
            .expect("capture exists");
        let capture = &source[capture_start..];
        let capture = &capture[..capture.find("\n    }\n").expect("capture ends")];
        assert_eq!(
            capture.matches("CONFIG.read()").count(),
            1,
            "capture must read the configuration exactly once: {capture}"
        );
        assert!(
            !capture.contains("ThemeInput::current()"),
            "capture must derive the theme from its own snapshot, not read CONFIG again"
        );
        // `capture` is the ONE place allowed to touch process state; every other part of the
        // description layer must be a function of the input it was handed.
        // Only the production half of the file, with `capture` itself removed: the test module
        // below names these identifiers on purpose.
        let production = &source[..source.find("\nmod tests {").expect("the test module")];
        let outside_capture = production.replace(capture, "");
        for (name, text) in [
            ("page_view/mod.rs", outside_capture.as_str()),
            ("general.rs", general),
        ] {
            for forbidden in [
                "needs_permission_migration_copy()",
                "system_dark_mode()",
                "NSUserDefaults",
            ] {
                assert!(
                    !text.contains(forbidden),
                    "{name} reads {forbidden} outside its PageInput"
                );
            }
        }
    }

    /// A control that needs a NUMBER takes it from the description's typed value.
    ///
    /// `ControlUpdate` used to carry only text, so the applier parsed a popup's index and a
    /// slider's stop back out of the string they had just been formatted into. The typed value is
    /// what the description asserted; the string stays for display and for the tests that read the
    /// description. This pins that no branch of the applier parses.
    #[test]
    fn the_applier_never_parses_a_controls_value_out_of_its_text() {
        let source = include_str!("mod.rs");
        let start = source
            .find("pub(super) unsafe fn apply_control_update")
            .expect("the applier exists");
        let applier = &source[start..];
        let applier = &applier[..applier.find("\n}\n").expect("the applier ends")];
        assert!(
            !applier.contains(".parse::<"),
            "the applier parses a value out of text instead of using the typed one: {applier}"
        );
    }
}
