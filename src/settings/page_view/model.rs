//! The description: what a settings page IS, before anything is laid out or drawn.
//!
//! A `PageInput` is everything a page may read; a `PageView` is what it returns -- one `RowSpec`
//! per row, each with its copy keys, its control kind, its identity and its initial value. The
//! types here are pure: no AppKit, no config reads, no layout.

use super::super::*;
use super::*;
/// A row placed in the page: its frame and the frames of the views it draws.
///
/// These are the numbers the page is built with; the canvas then re-places the views from its own
/// walk, so a row hidden later and its cards stay in step with one another.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::settings) struct RowLayout {
    /// The row's frame in the page document (unflipped: the origin is the row's bottom edge).
    pub(in crate::settings) frame: BoxRect,
    /// The action button beside a value row, when the row carries one.
    pub(in crate::settings) action_frame: Option<BoxRect>,
    pub(in crate::settings) label_frame: BoxRect,
    /// The copy box the label is centered in or aligned to.
    #[allow(dead_code)]
    pub(in crate::settings) stack_frame: BoxRect,
    /// The row's resulting height (its frame's height).
    #[allow(dead_code)]
    pub(in crate::settings) height: f64,
    /// The caption's frame for a stacked row.
    pub(in crate::settings) caption_frame: Option<BoxRect>,
    pub(in crate::settings) control_frame: BoxRect,
    /// The readout beside a slider, whose frame follows the slider's.
    pub(in crate::settings) readout_frame: Option<BoxRect>,
    pub(in crate::settings) control_h: f64,
    pub(in crate::settings) stacked: bool,
}

/// Everything a page description may read, captured once per transaction.
pub(in crate::settings) struct PageInput {
    pub(in crate::settings) config: Config,
    /// The theme inputs the row copy and option labels follow (P0). A page that needs no theme
    /// output still carries the snapshot so its description is built from one input type.
    #[allow(dead_code)]
    pub(in crate::settings) theme: crate::theme::ThemeInput,
    /// The device the Mouse page edits (`None` = "All Mice"). One config with two devices shows
    /// different values, so this is an input of the description, not something it discovers; the
    /// Switcher and General rows read only the config, and this keeps the input type complete for
    /// the pages that follow.
    #[allow(dead_code)]
    pub(in crate::settings) device: Option<crate::mouse::device::DeviceKey>,
    /// The Finder Menu page's extension status. It comes from the system (is the extension
    /// elected?) and from the recovery transaction's last result, so it is captured here rather
    /// than computed by a description.
    pub(in crate::settings) finder_menu_status: String,
    /// Whether the storage/permission banner carries the 0.2.3 permission-migration copy instead of
    /// the ordinary accessibility warning. It comes from the bundle's version and from a system
    /// default, so a description that asked the system itself would stop being a function of its
    /// input -- and the banner's HEIGHT depends on it, which is a layout input.
    pub(in crate::settings) permission_migration_copy: bool,
    /// The gesture layer the Gestures page edits (`None` = the shared layer). Captured with the
    /// device for the same reason: the page must not look the device up again while it renders.
    pub(in crate::settings) gesture_scope: Option<crate::settings::GestureLayerScope>,
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
pub(in crate::settings) struct BannerFrame {
    pub(in crate::settings) x: f64,
    pub(in crate::settings) y: f64,
    pub(in crate::settings) w: f64,
    pub(in crate::settings) h: f64,
}

/// The kind of control a row carries, and therefore how it is built and how its value is shown.
///
/// The kind is presentation (a switch, a popup, a slider); *which* field the control edits comes
/// from its [`ControlId`], never from the kind -- several switches edit different fields, and a
/// button edits none.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::settings) enum ControlKind {
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
pub(in crate::settings) enum ControlId {
    Field(ControlField),
    Action(&'static str),
}

/// How a row's copy is stacked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::settings) enum RowCopy {
    /// Title only: the label sits centered in the row.
    Plain,
    /// The title with a subtitle below it that is measured and drawn.
    Captioned,
}

/// One row of a page description: what it says, what it edits and where it sits in the page.
#[derive(Clone, Debug)]
pub(in crate::settings) struct RowSpec {
    /// The zero-based index of the card this row belongs to. A card boundary also carries the
    /// section's spacing, so the card index alone decides the layout.
    pub(in crate::settings) card: usize,
    /// The card's heading copy key (read when the card opens).
    pub(in crate::settings) section: &'static str,
    pub(in crate::settings) label_key: &'static str,
    /// The row's label as DATA (a gesture's arrows, a recorded chord) instead of the locale key.
    /// `label_key` stays the row's class, which the search index and the accessibility pairing use.
    pub(in crate::settings) label_text: Option<String>,
    pub(in crate::settings) desc_key: Option<&'static str>,
    /// The caption a stacked row draws (a copy key, like the label). Read for the search entry and
    /// measured by [`MeasuredText::measure`].
    pub(in crate::settings) caption_key: Option<&'static str>,
    pub(in crate::settings) control: ControlKind,
    /// Whether this row's LINK or VALUE control is drawn in the page's own value column (the About
    /// page's website, GitHub and version rows) instead of the shared control column: that column
    /// starts further right and runs to the card's far inset, which is where the URL text fits.
    pub(in crate::settings) value_column: bool,
    /// Whether a captioned row's text block takes the WIDE width (up to the card's far inset)
    /// instead of the default one that stops at the control column. One row in the design needs
    /// it: the General page's menu-bar switch.
    pub(in crate::settings) wide_caption: bool,
    /// A number stamped on the row's ACTION BUTTON as its tag. The button is what the callback
    /// receives, and its own text is its title, so this is how a row tells the handler WHICH entry
    /// it is: an excluded-application row stamps its list index here.
    pub(in crate::settings) action_tag: Option<isize>,
    /// The application bundle whose icon this row shows before its label, if any.
    ///
    /// Data, not a view: the page states WHICH application the row is about, and the renderer
    /// resolves the icon from the bundle on disk. Only the exclusion rows use it today -- a list of
    /// application names with no icons is a list a user has to read word by word, while the icon is
    /// the thing they recognise at a glance.
    pub(in crate::settings) icon_bundle: Option<String>,
    /// The row's control identity. Every declared row carries one.
    pub(in crate::settings) id: ControlId,
    pub(in crate::settings) copy: RowCopy,
    /// The master switch that freezes this row's control, if any (the freeze itself is still
    /// applied by the master switch's own callback, which owns the enabled lists today).
    #[allow(dead_code)]
    pub(in crate::settings) depends_on: Option<ControlField>,
    /// Whether the card draws a divider above this row (the first row of a card has none).
    pub(in crate::settings) separator_above: bool,
    /// The page's conditional group this row belongs to, if any (the visibility is toggled on
    /// the canvas's own group list today).
    #[allow(dead_code)]
    pub(in crate::settings) group: Option<RowGroup>,
    /// The value the control starts with; `None` for an action row.
    pub(in crate::settings) value: Value,
    pub(in crate::settings) search: SearchRow,
}

/// The search-index entry a row contributes, produced as a value by the page description.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::settings) struct SearchRow {
    pub(in crate::settings) label: String,
    pub(in crate::settings) desc: Option<String>,
    pub(in crate::settings) section: String,
    pub(in crate::settings) options: Vec<String>,
}

/// The value a control starts with, and the value the refill writes later.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::settings) enum Value {
    Switch(bool),
    Index(usize),
    Slider(i64),
    /// A control whose content is data rather than a locale key: the clipboard shortcut recorder's
    /// title is the recorded chord, and a permission status row shows text a callback maintains.
    Dynamic(String),
    /// An action row has no value.
    None,
}

/// One "field -> value" entry of the refill / in-place refresh description.
///
/// `value` is what the CONTROL takes (a switch state, a popup index, a slider stop); `display` is
/// what its readout shows when that differs from the control's own value (the status-bar slider
/// carries a stop index and shows the point size that stop means).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::settings) struct ControlUpdate {
    pub(in crate::settings) field: ControlField,
    /// The value the control is SET to, still typed the way the description stated it. The string
    /// form is [`Value::describe`], kept for the display contract and for the tests that read the
    /// description; a control that needs a NUMBER takes it from here instead of parsing the text
    /// back, which is how a popup index and a slider stop used to be recovered.
    pub(in crate::settings) typed: Value,
    pub(in crate::settings) value: String,
    pub(in crate::settings) display: String,
}

/// A page's full description: its rows plus everything else the page draws around them.
pub(in crate::settings) struct PageView {
    /// The page this description belongs to.
    #[allow(dead_code)]
    pub(in crate::settings) page: SettingsPageId,
    /// The page title's copy key.
    pub(in crate::settings) title_key: &'static str,
    /// The rows in document order.
    pub(in crate::settings) rows: Vec<RowSpec>,
    /// The section heading the page's first rows belong to, until a card opens.
    #[allow(dead_code)]
    pub(in crate::settings) initial_section: String,
    /// A page-level block above the rows (the General page's permission banner).
    pub(in crate::settings) banner: Option<BannerSpec>,
    /// How far below the document's top edge the rows start, when the page draws its OWN header
    /// instead of the shared page title. `None` = the shared title (the canvas measures it); the
    /// About page pins an icon, a title and a subtitle of its own, so it states the offset the
    /// canvas must start from.
    pub(in crate::settings) header_offset: Option<f64>,
    /// Rows the page draws ITSELF, in place inside a described card.
    ///
    /// The description covers controls; a few rows are widgets it has no kind for (a shortcut
    /// recorder with a capture state machine, a numeric text field). Those are ordered here, so the
    /// card still runs in one sequence, and [`PageHooks::extra_row`] draws them where they belong.
    pub(in crate::settings) extra_rows: Vec<PageRowSpec>,
}

/// One row a page draws itself: where it sits, what the page calls it, and how tall it is on top of
/// the described rows it follows.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::settings) struct PageRowSpec {
    /// The card the row belongs to, like [`RowSpec::card`].
    pub(in crate::settings) card: usize,
    /// The card's heading copy key, like [`RowSpec::section`]: a card that holds ONLY page-drawn
    /// rows has no described row to take it from, so the page-drawn row carries it too.
    pub(in crate::settings) section: &'static str,
    /// The page's own name for this row; passed back to [`PageHooks::extra_row`].
    pub(in crate::settings) key: &'static str,
    /// The row's height. A described row's height comes from its copy; a page-drawn row states its
    /// own, because only the page knows what it puts there — either a constant, or a measurement
    /// over the page input (a row whose height follows the connected devices, for example).
    pub(in crate::settings) height: RowHeight,
    /// How many described rows of that card come before it.
    pub(in crate::settings) after_row: usize,
    /// The page's conditional group this row belongs to, exactly like [`RowSpec::group`]: a
    /// page-drawn row that a mode switch hides must be hidden by the SAME canvas group list, or
    /// the page stops following the switch.
    pub(in crate::settings) group: Option<RowGroup>,
    /// Whether this row's own page code draws the line over its top edge.
    ///
    /// The canvas draws a card's dividers, including the one above a page-drawn row. One row keeps
    /// its line outside that capture -- the About page's Updates band -- because the update flow
    /// collapses the row to nothing, and a line travelling with the row would land over the row
    /// above. Such a page states it here instead of the canvas guessing from a row key.
    pub(in crate::settings) divides_with_own_line: bool,
    /// Whether this row must draw NO card divider above it at all.
    ///
    /// The complement of [`Self::divides_with_own_line`] for a row that is part of the block above
    /// it rather than a row of the page: the About page's update host starts empty and hidden under
    /// the Updates band, and the band's own line is the card's line there. A divider is a document
    /// subview of its own, so the host being hidden would not hide it.
    pub(in crate::settings) no_divider: bool,
}

/// A page-drawn row's height: a constant, or a measurement of the page input.
///
/// The device row on the Mouse page is why the measured form exists: its height follows the longest
/// caption any connected device can produce, which is only known once the page input exists.
#[derive(Clone, Copy, Debug)]
pub(in crate::settings) enum RowHeight {
    Fixed(f64),
    Measured(fn(&PageInput, f64) -> f64),
}

/// What a page supplies to draw the rows it owns. The default draws none, which is every page whose
/// rows are all described.
pub(in crate::settings) trait PageHooks {
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
pub(in crate::settings) struct NoHooks;
impl PageHooks for NoHooks {}

/// The permission banner a page carries above its rows.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::settings) struct BannerSpec {
    pub(in crate::settings) copy_key: &'static str,
    pub(in crate::settings) action_id: &'static str,
    pub(in crate::settings) content_h: f64,
    pub(in crate::settings) gap: f64,
}

/// The action button a [`ControlKind::Value`] row carries beside its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::settings) struct ValueAction {
    /// The action identity the dispatcher resolves to a selector.
    pub(in crate::settings) action: &'static str,
    /// The button's copy key.
    pub(in crate::settings) button_key: &'static str,
}

/// How a value row's text is aligned in its box.
///
/// The shipped pages are not uniform: the Finder Menu status line and the About permission rows
/// right-align their value so it sits against the action button, while a lone value (the version
/// line) is aligned naturally. Making this a row field is what keeps the migrated pages looking
/// exactly as they did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::settings) enum ValueAlign {
    /// The platform's natural alignment (left in every shipped locale).
    #[default]
    Natural,
    Right,
}
