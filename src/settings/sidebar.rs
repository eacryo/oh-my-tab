//! Settings sidebar and two-pane background construction.
//!
//! The sidebar also owns the settings search: the field below the brand row, the index built from
//! the page builders' own row strings, and the results view that stands in for the detail pane
//! while a query is active.

use super::*;

use std::sync::{LazyLock, Mutex};

pub(super) struct SettingsSidebarGeometry {
    pub(super) content_h: f64,
    pub(super) view_w: f64,
    pub(super) card_margin: f64,
    pub(super) card_w: f64,
    pub(super) card_radius: f64,
}

pub(super) const SOLID_SIDEBAR_IDENTIFIER: &str = "settings-sidebar-solid-surface";
/// Width of the sidebar column, from the mockup's `.settings-sidebar { width: 212px }`.
///
/// This module owns the sidebar's skeleton, so the design value lives here with the rest of its
/// geometry. `settings::SETTINGS_SIDEBAR_WIDTH` (220) is the superseded token and is no longer the
/// authority for this number.
pub(super) const SIDEBAR_WIDTH: f64 = 212.0;

/// The mock's brand row: `.brand { display: flex; align-items: center; gap: var(--sp-2);
/// padding: var(--sp-1) var(--sp-2) var(--sp-4) }` around a 34x34 icon and a 14px/600
/// `.brand-name`. The sidebar itself is a column whose own padding (`--sp-3`) is already spent on
/// the nav rows' 12pt inset, so the `--sp-1` and `--sp-4` of the label's box are counted with the
/// whole box, not as a gap between siblings.
const SIDEBAR_BRAND_ICON_SIZE: f64 = 34.0;
/// Height of the label's line box: the `--fs-14` scale's 14pt line height.
const SIDEBAR_BRAND_LABEL_H: f64 = 20.0;
/// `.brand`'s own vertical padding: `var(--sp-1)` above and `var(--sp-4)` below the icon row.
const SIDEBAR_BRAND_PAD_TOP: f64 = 4.0;
const SIDEBAR_BRAND_PAD_BOTTOM: f64 = 16.0;
/// `.nav-item { height: 35px }`.
const SIDEBAR_NAV_ROW_H: f64 = 35.0;
/// Height of one whole brand row: the icon's line box plus the row's own padding.
const SIDEBAR_BRAND_ROW_H: f64 =
    SIDEBAR_BRAND_PAD_TOP + SIDEBAR_BRAND_ICON_SIZE + SIDEBAR_BRAND_PAD_BOTTOM;
/// `.settings-nav { margin-top: var(--sp-4) }`.
#[cfg_attr(not(test), allow(dead_code))]
const SIDEBAR_NAV_TOP_GAP: f64 = 16.0;
/// Leading edge of the brand's icon: the sidebar's 12pt content edge plus the brand's own `--sp-2`.
const SIDEBAR_BRAND_LEADING: f64 = 12.0 + 8.0;
/// `.brand img { border-radius: 9px }`: sized for a 34pt icon, not for an 8pt control.
pub(super) const SIDEBAR_BRAND_ICON_RADIUS: f64 = 9.0;

/// Frames of the mock's brand row inside a sidebar whose box is `sidebar` (the sidebar's box
/// already includes its own `--sp-3` padding): the 34pt app icon and the `.brand-name` label.
///
/// `.brand { padding: var(--sp-1) var(--sp-2) var(--sp-4) }` with `align-items: center`: the icon's
/// 34pt exceeds the label's 20pt line, so both children are centered in one 4 + 34 + 16 = 54pt row,
/// and the label's box spans from the sidebar's 12 + 8=20pt content edge to its 12pt trailing one.
fn brand_row_frames(sidebar: NSRect) -> (NSRect, NSRect) {
    let top = sidebar.origin.y + sidebar.size.height;
    let row_mid = top - SIDEBAR_BRAND_ROW_H / 2.0;
    let icon = NSRect::new(
        NSPoint::new(
            sidebar.origin.x + SIDEBAR_BRAND_LEADING,
            row_mid - SIDEBAR_BRAND_ICON_SIZE / 2.0,
        ),
        NSSize::new(SIDEBAR_BRAND_ICON_SIZE, SIDEBAR_BRAND_ICON_SIZE),
    );
    let name_leading = icon.origin.x + SIDEBAR_BRAND_ICON_SIZE + 8.0;
    let trailing = sidebar.origin.x + sidebar.size.width - 12.0;
    let name = NSRect::new(
        NSPoint::new(name_leading, row_mid - SIDEBAR_BRAND_LABEL_H / 2.0),
        NSSize::new((trailing - name_leading).max(1.0), SIDEBAR_BRAND_LABEL_H),
    );
    (icon, name)
}

// MARK: - Search field (the mock's .field-shell)

/// Gap between the brand row and the search shell (the mock keeps the two 8pt apart; the shell
/// itself carries no vertical margin).
const SIDEBAR_SEARCH_GAP: f64 = 8.0;
/// `.field-shell { height: 34px }`.
const SIDEBAR_SEARCH_H: f64 = 34.0;
/// Space between the shell's bottom edge and the nav's top edge. The mock measures 16pt there
/// (8pt between the shell and `.settings-nav` plus the nav's `--sp-4` margin).
const SIDEBAR_SEARCH_TO_NAV: f64 = 16.0;
/// `.field-shell { padding: 0 var(--sp-2); gap: var(--sp-2) }`.
const SIDEBAR_SEARCH_PAD_X: f64 = 8.0;
const SIDEBAR_SEARCH_GAP_X: f64 = 8.0;
/// How far an `NSTextFieldCell` inset its text from the field's own left edge.
///
/// The cell adds this before it draws the line, so a field placed `GAP_X` after the magnifier drew
/// its placeholder `GAP_X + this` away from it (measured on screen: 20.5pt instead of 8). The box is
/// moved left by it so the TEXT, not the invisible cell margin, keeps the gap.
const SIDEBAR_SEARCH_TEXT_INSET: f64 = 12.5;
/// `.field-shell > svg { width: 14px; height: 14px }`.
const SIDEBAR_SEARCH_GLYPH: f64 = 14.0;
/// The clear button's diameter (the mock's `kbd` chip is 18pt; the round clear chip is 16pt).
const SIDEBAR_SEARCH_CLEAR_SIZE: f64 = 16.0;
/// The trailing `⌘F` hint: bare 10pt monospace ink on the shell's midline, 4pt of side padding
/// inside its own box, and no chip around it — a bordered box inside the field's shell read as a
/// second control. `⌘F` focuses this field (see `settings_window_perform_key_equivalent`), so the
/// hint states a binding the app really has rather than decorating the shell.
const SIDEBAR_SEARCH_HINT_H: f64 = 18.0;
const SIDEBAR_SEARCH_HINT_PAD_X: f64 = 4.0;
const SIDEBAR_SEARCH_HINT_FONT: f64 = 10.0;
/// The narrowest the hint's chip can be, so a one-glyph hint is square rather than a sliver.
const SIDEBAR_SEARCH_HINT_MIN_W: f64 = SIDEBAR_SEARCH_HINT_H;

/// The trailing slot's width: the wider of the clear chip and the `⌘F` hint's chip, so the field
/// reserves the same space in both states and the text never shifts when a query appears. The
/// hint's own width follows its label, which the caller measures.
fn search_trailing_slot_width(hint_label_width: f64) -> f64 {
    let hint_w =
        (hint_label_width + SIDEBAR_SEARCH_HINT_PAD_X * 2.0).max(SIDEBAR_SEARCH_HINT_MIN_W);
    SIDEBAR_SEARCH_CLEAR_SIZE.max(hint_w)
}
/// The hint's text, from the same key-cap vocabulary the rest of the settings UI draws keycaps
/// with, so the focus shortcut reads identically wherever a chord is shown.
const SIDEBAR_SEARCH_HINT: &str = "⌘F";
/// Room the text keeps from the trailing slot's left edge: the shell's own `gap`, which is also
/// what separates the text from the clear chip and from the `⌘F` hint.
const SIDEBAR_SEARCH_CLEAR_GAP: f64 = SIDEBAR_SEARCH_GAP_X;
/// The shell's border is 1pt in every state (transparent until focus), matching the mock's
/// `border: 1px solid transparent`, so focusing never moves the text.
const SIDEBAR_SEARCH_BORDER_W: f64 = 1.0;

/// The trailing `⌘F` hint's frame inside `shell`: a box exactly one text line tall, centred on
/// the shell's midline and sitting at the shell's right inner edge, with 4pt of padding around its
/// label and no border. Its width follows the label.
///
/// The box IS the line: a taller box does not centre its text (`NSTextField` lays a single line
/// out from the box's top edge, and the cell has no `setVerticallyCentered:` on this SDK), which is
/// what drew the chord high inside the field.
///
/// Pure so the hint's slot is assertable without a live window; the label width and line height are
/// measured by the caller (AppKit needs a live font to measure text), so both are parameters here.
fn search_hint_frame(shell: NSRect, label_width: f64, line_h: f64) -> NSRect {
    let chip_w = search_trailing_slot_width(label_width);
    // The hint's frame is where the shell's padding ends.
    let trailing_edge =
        shell.origin.x + shell.size.width - SIDEBAR_SEARCH_BORDER_W - SIDEBAR_SEARCH_PAD_X;
    let line_h = line_h.max(1.0);
    let mid_y = shell.origin.y + shell.size.height / 2.0;
    NSRect::new(
        NSPoint::new(trailing_edge - chip_w, mid_y - line_h / 2.0),
        NSSize::new(chip_w, line_h),
    )
}

/// Frames inside one `.field-shell` box: the 14pt magnifier, the editable text field and, when
/// `has_clear`, the clear button. All are centered on the shell's vertical midline.
///
/// The field is the mock's `.field-shell input { flex: 1 }` with no vertical inset of its own, so its
/// box spans the shell's full inner height and AppKit centres the line in it. The old box was a
/// fixed 22pt tall and centred on the shell, which left the line's own descent hanging below the
/// midline and drew the placeholder visibly high: the box, not the font, decides where text lands.
///
/// Pure so the shell's metrics are assertable without a live window; `shell` is the outer box
/// (border included), in whatever coordinate space the caller draws in.
fn search_shell_frames(
    shell: NSRect,
    has_clear: bool,
    hint_label_width: f64,
) -> (NSRect, NSRect, Option<NSRect>) {
    let inner_x = shell.origin.x + SIDEBAR_SEARCH_BORDER_W + SIDEBAR_SEARCH_PAD_X;
    let inner_right =
        shell.origin.x + shell.size.width - SIDEBAR_SEARCH_BORDER_W - SIDEBAR_SEARCH_PAD_X;
    let inner_y = shell.origin.y + SIDEBAR_SEARCH_BORDER_W;
    let inner_h = (shell.size.height - SIDEBAR_SEARCH_BORDER_W * 2.0).max(1.0);
    let mid_y = shell.origin.y + shell.size.height / 2.0;
    let glyph = NSRect::new(
        NSPoint::new(inner_x, mid_y - SIDEBAR_SEARCH_GLYPH / 2.0),
        NSSize::new(SIDEBAR_SEARCH_GLYPH, SIDEBAR_SEARCH_GLYPH),
    );
    let trailing = if has_clear {
        Some(NSRect::new(
            NSPoint::new(
                inner_right - SIDEBAR_SEARCH_CLEAR_SIZE,
                mid_y - SIDEBAR_SEARCH_CLEAR_SIZE / 2.0,
            ),
            NSSize::new(SIDEBAR_SEARCH_CLEAR_SIZE, SIDEBAR_SEARCH_CLEAR_SIZE),
        ))
    } else {
        None
    };
    let input_x =
        glyph.origin.x + SIDEBAR_SEARCH_GLYPH + SIDEBAR_SEARCH_GAP_X - SIDEBAR_SEARCH_TEXT_INSET;
    // The field ends before the trailing slot: the `⌘F` hint while the query is empty, the clear
    // chip once there is one, both at the shell's right inner edge. The slot is sized to the wider
    // of the two and keeps the shell's own 8pt gap to the text, so a query appearing never shifts
    // the placeholder.
    let input_right =
        inner_right - search_trailing_slot_width(hint_label_width) - SIDEBAR_SEARCH_CLEAR_GAP;
    let input = NSRect::new(
        NSPoint::new(input_x, inner_y),
        NSSize::new((input_right - input_x).max(1.0), inner_h),
    );
    (glyph, input, trailing)
}

/// The shell's box: from the sidebar's 12pt content edge, `SIDEBAR_SEARCH_GAP` below the brand
/// row, as wide as the nav's columns.
fn sidebar_search_shell_frame(sidebar: NSRect) -> NSRect {
    let top = sidebar.origin.y + sidebar.size.height;
    NSRect::new(
        NSPoint::new(
            sidebar.origin.x + 12.0,
            top - SIDEBAR_BRAND_ROW_H - SIDEBAR_SEARCH_GAP - SIDEBAR_SEARCH_H,
        ),
        NSSize::new((sidebar.size.width - 24.0).max(1.0), SIDEBAR_SEARCH_H),
    )
}

/// Top edge of the first nav row: the brand row, the search shell, and `SIDEBAR_SEARCH_TO_NAV`
/// down to the first 35pt item.
///
/// It is a constant of the sidebar's layout, so a query never moves the nav: only the rows below
/// it (which come and go with an empty query) change.
fn sidebar_first_nav_row_top(sidebar: NSRect) -> f64 {
    sidebar_search_shell_frame(sidebar).origin.y - SIDEBAR_SEARCH_TO_NAV - SIDEBAR_NAV_ROW_H
}

// MARK: - Search index

/// One searchable setting: the row a page builder drew, with the strings it was drawn from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SearchEntry {
    /// The sidebar index of the page that owns the row.
    pub(super) page: usize,
    /// The row on that page: `PageCanvas`'s row number, which is where the highlight goes.
    pub(super) row: usize,
    /// The strings the row was drawn from. Baked in rather than looked up again, because the index
    /// is rebuilt from the very calls that build the pages.
    label: String,
    desc: String,
    /// The card header (section title) the row sits under. Empty until the card closes: a page
    /// draws a card's header only after drawing the rows inside it, so the header is applied
    /// retroactively (see `apply_settings_search_section`).
    section: String,
    /// The choices a dropdown row offers, which are searched as well: a query equal to one of a
    /// popup's options has to find the setting that offers it, not only settings whose label or
    /// description mention it.
    options: Vec<String>,
    rendered_text: Vec<String>,
}

impl SearchEntry {
    /// One page-builder row: `label`, its optional description and the card header (section title)
    /// it sits under.
    pub(super) fn row(
        page: usize,
        row: usize,
        label: &str,
        desc: Option<&str>,
        header: &str,
    ) -> Self {
        Self {
            page,
            row,
            label: label.to_string(),
            desc: desc.unwrap_or("").to_string(),
            section: header.to_string(),
            options: Vec::new(),
            rendered_text: Vec::new(),
        }
    }

    /// The choices this row's dropdown offers (empty for a row without one).
    fn set_options(&mut self, options: &[&str]) {
        self.options = options
            .iter()
            .filter(|option| !option.is_empty())
            .map(|option| (*option).to_string())
            .collect();
    }

    /// The row's label, as the results list draws it.
    fn label(&self) -> &str {
        &self.label
    }

    /// The card header the row sits under (empty for a row the page never labeled).
    fn section(&self) -> &str {
        &self.section
    }

    /// The strings a query is matched against, each on its own: a query cannot match across two of
    /// them by accident, and an empty one (a row without a description) can never match.
    fn fields(&self) -> impl Iterator<Item = &str> {
        [
            self.label.as_str(),
            self.desc.as_str(),
            self.section.as_str(),
        ]
        .into_iter()
        .chain(self.options.iter().map(String::as_str))
        .chain(self.rendered_text.iter().map(String::as_str))
    }

    /// Give the row the header of the card it was drawn into.
    fn set_section(&mut self, header: &str) {
        self.section = header.to_string();
    }
}

/// How many result rows the results view holds at most; a longer list scrolls.
pub(super) const SEARCH_RESULT_LIMIT: usize = 200;

static SETTINGS_SEARCH_INDEX: LazyLock<Mutex<Vec<SearchEntry>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// The entries whose card has not closed yet: a page closes a card only after drawing its rows, so
/// the card's header arrives afterwards and is applied to these. Indices into the index above.
static PENDING_SECTION: LazyLock<Mutex<Vec<usize>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// The entries the rows currently on screen were drawn from: a result row's tag indexes this list,
/// so a click resolves the same entry the render used even if the index changed since.
static RENDERED_RESULTS: LazyLock<Mutex<Vec<SearchEntry>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// The query the pages are currently filtered by, and the row key of the entry a rebuild still has
/// to highlight (a jump survives the rebuild a locale or theme change triggers).
struct SettingsSearchState {
    query: String,
    /// The page the search covered, restored when the query is cleared.
    return_page: Option<usize>,
}

static SETTINGS_SEARCH: LazyLock<Mutex<SettingsSearchState>> = LazyLock::new(|| {
    Mutex::new(SettingsSearchState {
        query: String::new(),
        return_page: None,
    })
});

/// The user's query as the search compares it: trimmed and case-folded. Folded per character with
/// the ASCII fast path first, because most settings copy is ASCII and `to_lowercase` on a whole
/// non-ASCII string allocates a String per call for no benefit.
pub(super) fn normalize_search_query(query: &str) -> String {
    query
        .trim()
        .chars()
        .flat_map(char::to_lowercase)
        .collect::<String>()
}

/// The entries whose label, description or section header contains `query` (already normalized),
/// in index order -- which is sidebar order, then page order: the mock's results list.
///
/// Every field is folded with `normalize_search_query` as well, so a locale's own casing rules
/// apply to the setting copy exactly as they do to the query.
pub(super) fn match_settings_search(index: &[SearchEntry], query: &str) -> Vec<usize> {
    let query = normalize_search_query(query);
    if query.is_empty() {
        return Vec::new();
    }
    index
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            entry
                .fields()
                .any(|field| !field.is_empty() && normalize_search_query(field).contains(&query))
        })
        .map(|(position, _)| position)
        .take(SEARCH_RESULT_LIMIT)
        .collect()
}

/// Start a new settings build's search index: the rows are re-recorded as the page builders run.
pub(super) fn begin_settings_search_index() {
    SETTINGS_SEARCH_INDEX.lock().unwrap().clear();
    PENDING_SECTION.lock().unwrap().clear();
}

/// Start a page's part of the index: the rows of the previous page have all had their cards closed
/// (or will never get one), so nothing is left waiting for a header.
pub(super) fn begin_settings_search_section() {
    PENDING_SECTION.lock().unwrap().clear();
}

/// The rows the current build has recorded for `page`, so a page can fill the gaps it left.
pub(super) fn recorded_search_rows(page: usize) -> Vec<usize> {
    SETTINGS_SEARCH_INDEX
        .lock()
        .unwrap()
        .iter()
        .filter(|entry| entry.page == page)
        .map(|entry| entry.row)
        .collect()
}

/// Record one row the page builders drew. Its section header is not known yet -- the page closes
/// the row's card after drawing it -- so the header is filled in by
/// `apply_settings_search_section`; a row recorded with a header already (the sweep's fallback)
/// simply keeps it.
pub(super) fn record_settings_search_entry(entry: SearchEntry) {
    let mut index = SETTINGS_SEARCH_INDEX.lock().unwrap();
    index.push(entry);
    PENDING_SECTION.lock().unwrap().push(index.len() - 1);
}

/// Attach the choices of the row's dropdown to the row recorded immediately before it: a query
/// that equals one of them finds the setting that offers it.
pub(super) fn set_search_options_for_last_row(options: &[&str]) {
    if let Some(entry) = SETTINGS_SEARCH_INDEX.lock().unwrap().last_mut() {
        entry.set_options(options);
    }
}

pub(super) fn extend_settings_search_row(page: usize, row: usize, text: &[String]) {
    if let Some(entry) = SETTINGS_SEARCH_INDEX
        .lock()
        .unwrap()
        .iter_mut()
        .find(|entry| entry.page == page && entry.row == row)
    {
        entry.rendered_text = text.to_vec();
    }
}

/// Record one row whose card header is already settled (the page sweep's blind-spot rows): it is
/// never re-labelled by a card that closes later.
pub(super) fn record_settings_search_entry_sectioned(entry: SearchEntry) {
    SETTINGS_SEARCH_INDEX.lock().unwrap().push(entry);
}

/// The card around the pending rows closed: every row recorded since the previous card belongs to
/// it and is searched with its header.
pub(super) fn apply_settings_search_section(header: &str) {
    let mut index = SETTINGS_SEARCH_INDEX.lock().unwrap();
    let mut pending = PENDING_SECTION.lock().unwrap();
    for position in pending.drain(..) {
        if let Some(entry) = index.get_mut(position) {
            entry.set_section(header);
        }
    }
}

fn settings_search_index() -> Vec<SearchEntry> {
    let mut entries = SETTINGS_SEARCH_INDEX.lock().unwrap().clone();
    entries.sort_by_key(|entry| (entry.page, entry.row));
    entries
}

/// The current query, trimmed (empty when the sidebar shows the normal page content).
pub(super) fn settings_search_query() -> String {
    SETTINGS_SEARCH.lock().unwrap().query.trim().to_string()
}

fn set_settings_search_query(query: &str) {
    SETTINGS_SEARCH.lock().unwrap().query = query.to_string();
}

/// Whether the detail pane is showing search results rather than a page.
pub(super) fn settings_search_is_active() -> bool {
    !settings_search_query().is_empty()
}

/// The results for the live query: every matching entry, each with the page index it lives on.
pub(super) fn settings_search_results() -> Vec<SearchEntry> {
    let query = settings_search_query();
    if query.is_empty() {
        return Vec::new();
    }
    let index = settings_search_index();
    match_settings_search(&index, &query)
        .into_iter()
        .filter_map(|position| index.get(position).cloned())
        .filter(|entry| entry.page < SETTINGS_PAGE_COUNT)
        .collect()
}

// MARK: - Search field and results view

/// The sidebar's search objects and the detail pane's results view, as one component.
///
/// The field lives in the sidebar; the results stand in for the pages in the detail pane, so the
/// component needs both parents and resets its results view whenever the window is rebuilt.
#[derive(Clone, Copy)]
pub(super) struct SettingsSearch {
    pub(super) field: *mut AnyObject,
    pub(super) shell: *mut AnyObject,
    pub(super) clear: *mut AnyObject,
    /// The trailing `⌘F` hint. It shares its slot with `clear`: the hint while the query is empty,
    /// the clear chip once there is one.
    hint: *mut AnyObject,
    pub(super) results: *mut AnyObject,
    pub(super) results_scroll: *mut AnyObject,
    pub(super) results_doc: *mut AnyObject,
    results_w: f64,
    results_h: f64,
    /// The results view's rows container, replaced whenever a new query renders.
    results_rows: *mut AnyObject,
}

impl SettingsSearch {
    pub(super) const EMPTY: Self = Self {
        field: std::ptr::null_mut(),
        shell: std::ptr::null_mut(),
        clear: std::ptr::null_mut(),
        hint: std::ptr::null_mut(),
        results: std::ptr::null_mut(),
        results_scroll: std::ptr::null_mut(),
        results_doc: std::ptr::null_mut(),
        results_w: 0.0,
        results_h: 0.0,
        results_rows: std::ptr::null_mut(),
    };
}

/// The rendered width of `text` in the shared monospace face at `size`, in points. The shell's
/// `⌘F` hint is the only monospace label it draws, and its chip is sized to its own text.
unsafe fn measure_monospace_text_width(text: &str, size: f64) -> f64 {
    let value = make_nsstring(text);
    let font: *mut AnyObject = msg_send![
        class!(NSFont),
        monospacedSystemFontOfSize: size,
        weight: crate::theme::FONT_WEIGHT_REGULAR
    ];
    let attributes: *mut AnyObject = msg_send![
        class!(NSDictionary),
        dictionaryWithObject: font,
        forKey: NSFontAttributeName
    ];
    let measured: NSSize = msg_send![value, sizeWithAttributes: attributes];
    release_obj(value);
    if measured.width.is_finite() && measured.width > 0.0 {
        measured.width
    } else {
        0.0
    }
}

/// The shell's magnifier: the 14pt system `magnifyingglass` symbol.
///
/// NSImageView has no public tint (only NSButton and NSMenuItem do), so the template symbol is
/// drawn in AppKit's secondary label color, which is the mock's `--text-3` role for this glyph.
unsafe fn make_search_glyph(frame: NSRect) -> *mut AnyObject {
    let view = widgets::make_symbol_image_view("magnifyingglass", frame);
    let _: () = msg_send![view, setImageScaling: 3isize]; // NSImageScaleProportionallyUpOrDown
    view
}

unsafe fn set_view_hidden(view: *mut AnyObject, hidden: bool) {
    if !view.is_null() {
        let _: () = msg_send![view, setHidden: hidden];
    }
}

/// The shell's background and ring for the current state. The mock's `.field-shell` paints
/// `field_bg` with a transparent ring and switches to `field_hover` (its stronger field step) plus
/// the accent ring while focused (`:focus-within`). The palette has no `field_hover` token yet, so
/// the pressed-field step uses `hover_bg` locally.
pub(super) unsafe fn apply_search_shell_state(search: &SettingsSearch, focused: bool) {
    if search.shell.is_null() {
        return;
    }
    let palette = settings_palette();
    let layer: *mut AnyObject = msg_send![search.shell, layer];
    if layer.is_null() {
        return;
    }
    let fill = if focused {
        palette.hover_bg
    } else {
        palette.field_bg
    };
    let ring = if focused {
        palette.accent
    } else {
        0x00000000u32
    };
    layer_set_background(layer, crate::ffi::hex_to_cg_color(fill));
    crate::ffi::layer_set_border(layer, crate::ffi::hex_to_cg_color(ring));
    let _: () =
        msg_send![layer, setBorderWidth: if focused { SIDEBAR_SEARCH_BORDER_W } else { 0.0f64 }];
    let ink = crate::ffi::hex_to_cg_color(palette.muted_text);
    // The glyph is an NSImageView, which has no contentTintColor; a template symbol is drawn in the
    // image view's own label color, so the token only applies where a control can take it.
    let _ = ink;
}

/// Build the sidebar's search field (`.field-shell`) between the brand row and the nav.
///
/// Returns the component; the caller stores it in `SettingsUi` and binds its notification.
pub(super) unsafe fn build_settings_search_field(
    sidebar: *mut AnyObject,
    target: *mut AnyObject,
    card_w: f64,
    content_h: f64,
) -> SettingsSearch {
    let sidebar_rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(card_w, content_h));
    let shell_frame = sidebar_search_shell_frame(sidebar_rect);
    let shell: *mut AnyObject = msg_send![class!(NSView), alloc];
    let shell: *mut AnyObject = msg_send![shell, initWithFrame: shell_frame];
    let _: () = msg_send![shell, setWantsLayer: true];
    let _: () = msg_send![shell, setAutoresizingMask: 10u64]; // width-sizable, top-anchored
    let layer: *mut AnyObject = msg_send![shell, layer];
    if !layer.is_null() {
        let _: () = msg_send![layer, setCornerRadius: crate::theme::RADIUS_CONTROL];
        let _: () = msg_send![layer, setMasksToBounds: true];
    }
    let _: () = msg_send![sidebar, addSubview: shell];
    // The sidebar owns the shell from here on; the component keeps only a borrowed pointer.
    release_obj(shell);

    // Measure the hint's label before laying anything out: the trailing slot reserves room for
    // whichever of the hint and the clear chip is wider, and the field's width follows from it.
    let hint_label_w =
        unsafe { measure_monospace_text_width(SIDEBAR_SEARCH_HINT, SIDEBAR_SEARCH_HINT_FONT) };
    // The hint's box is one line tall, so the box has to be the height the font actually draws:
    // `NSTextField` lays a single line out from its box's top edge, so a taller box draws it high.
    let hint_line_h = unsafe {
        let font: *mut AnyObject = msg_send![
            class!(NSFont),
            monospacedSystemFontOfSize: SIDEBAR_SEARCH_HINT_FONT,
            weight: crate::theme::FONT_WEIGHT_REGULAR
        ];
        if font.is_null() {
            SIDEBAR_SEARCH_HINT_FONT
        } else {
            // The layout manager's own line height, not the glyph box: the box has to be as tall as
            // the line AppKit lays out, or the single line sits at the box's top and reads high.
            let line_h: f64 = msg_send![font, defaultLineHeightForFont];
            line_h.max(SIDEBAR_SEARCH_HINT_FONT)
        }
    };
    let (glyph_frame, input_frame, _) = search_shell_frames(shell_frame, false, hint_label_w);
    let glyph = make_search_glyph(NSRect::new(
        NSPoint::new(
            glyph_frame.origin.x - shell_frame.origin.x,
            glyph_frame.origin.y - shell_frame.origin.y,
        ),
        glyph_frame.size,
    ));
    let _: () = msg_send![glyph, setAutoresizingMask: 8u64]; // top-anchored only
    let _: () = msg_send![shell, addSubview: glyph];
    release_obj(glyph);

    // The editable field: borderless and transparent inside the shell, 13pt like the mock's
    // `.field-shell input { font-size: var(--fs-13) }`, with the localized placeholder in the muted
    // ink. The box spans the shell's inner height, and the class's `textRectForBounds:` override is
    // what actually centres the line in it -- a taller frame alone does not, because NSTextField
    // lays its text out from the top of the cell's drawing rect.
    let input_local = NSRect::new(
        NSPoint::new(
            input_frame.origin.x - shell_frame.origin.x,
            input_frame.origin.y - shell_frame.origin.y,
        ),
        input_frame.size,
    );
    let field: *mut AnyObject = msg_send![settings_search_field_class(), alloc];
    let field: *mut AnyObject = msg_send![field, initWithFrame: input_local];
    // The shared centered cell: a bare NSTextFieldCell draws its single line from the top of the
    // drawing rect, which is what put the placeholder above the shell's midline. This subclass
    // centres the line and also hands the field editor the same vertical inset, so the text does
    // not jump between the empty and the editing state.
    let empty_value = make_nsstring("");
    let cell: *mut AnyObject = msg_send![widgets::centered_text_field_cell_class(), alloc];
    let cell: *mut AnyObject = msg_send![cell, initTextCell: empty_value];
    let _: () = msg_send![field, setCell: cell];
    release_obj(cell);
    release_obj(empty_value);
    let _: () = msg_send![field, setBezeled: false];
    let _: () = msg_send![field, setBordered: false];
    let _: () = msg_send![field, setDrawsBackground: false];
    let _: () = msg_send![field, setEditable: true];
    let _: () = msg_send![field, setSelectable: true];
    let _: () = msg_send![field, setUsesSingleLineMode: true];
    let _: () = msg_send![field, setFocusRingType: 1u64]; // NSFocusRingTypeNone: the shell draws the ring
    let field_font: *mut AnyObject = msg_send![
        class!(NSFont),
        systemFontOfSize: crate::theme::FONT_BODY
    ];
    let _: () = msg_send![field, setFont: field_font];
    let _: () = msg_send![field, setTextColor: settings_text_color(SettingsTextRole::Primary)];
    apply_search_placeholder(field, &t("settings.sidebar_search_placeholder"));
    let _: () = msg_send![field, setAutoresizingMask: 8u64 | 2u64]; // width-sizable, top-anchored
    let _: () = msg_send![field, setTarget: target];
    let _: () = msg_send![shell, addSubview: field];
    release_obj(field);

    // The mock's trailing `⌘F` hint, in the slot the clear chip takes over once a query exists:
    // the shell shows exactly one of the two, so the row never carries both.
    let hint_frame = search_hint_frame(shell_frame, hint_label_w, hint_line_h);
    let hint: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let hint: *mut AnyObject = msg_send![hint, initWithFrame: NSRect::new(
        NSPoint::new(
            hint_frame.origin.x - shell_frame.origin.x,
            hint_frame.origin.y - shell_frame.origin.y,
        ),
        hint_frame.size,
    )];
    widgets::set_field(hint, SIDEBAR_SEARCH_HINT);
    let _: () = msg_send![hint, setBezeled: false];
    let _: () = msg_send![hint, setDrawsBackground: false];
    let _: () = msg_send![hint, setEditable: false];
    let _: () = msg_send![hint, setSelectable: false];
    let _: () = msg_send![hint, setAlignment: 1isize]; // NSTextAlignmentCenter
    let hint_font: *mut AnyObject = msg_send![
        class!(NSFont),
        monospacedSystemFontOfSize: SIDEBAR_SEARCH_HINT_FONT,
        weight: crate::theme::FONT_WEIGHT_REGULAR
    ];
    let _: () = msg_send![hint, setFont: hint_font];
    let hint_ink = crate::ffi::hex_to_ns_color(settings_palette().muted_text);
    let _: () = msg_send![hint, setTextColor: hint_ink];
    // No chip around the chord: a bordered box inside the field's own shell read as a second
    // control, so the hint is bare ink centred on the shell's midline.
    let _: () = msg_send![hint, setAutoresizingMask: 8u64 | 2u64]; // top-anchored, left-anchored
    let _: () = msg_send![shell, addSubview: hint];
    release_obj(hint);

    // The clear chip: shown only while the query is non-empty, so an empty field keeps the
    // mock's clean shell.
    let (_, _, clear_frame) = search_shell_frames(shell_frame, true, hint_label_w);
    let clear_local = clear_frame.map(|frame| {
        NSRect::new(
            NSPoint::new(
                frame.origin.x - shell_frame.origin.x,
                frame.origin.y - shell_frame.origin.y,
            ),
            frame.size,
        )
    });
    let mut clear = std::ptr::null_mut();
    if let Some(frame) = clear_local {
        let button: *mut AnyObject = msg_send![class!(NSButton), alloc];
        let button: *mut AnyObject = msg_send![button, initWithFrame: frame];
        let _: () = msg_send![button, setBordered: false];
        let empty = make_nsstring("");
        let _: () = msg_send![button, setTitle: empty];
        release_obj(empty);
        let clear_image: *mut AnyObject = msg_send![
            class!(NSImage),
            imageWithSystemSymbolName: make_nsstring("xmark.circle.fill"),
            accessibilityDescription: std::ptr::null::<AnyObject>()
        ];
        if !clear_image.is_null() {
            let _: () = msg_send![button, setImage: clear_image];
        }
        // setContentTintColor: takes an NSColor (select.rs uses this helper for its buttons).
        let _: () = msg_send![button, setContentTintColor: crate::ffi::hex_to_ns_color(settings_palette().muted_text)];
        let _: () = msg_send![button, setTarget: target];
        let _: () = msg_send![button, setAction: sel!(handleSettingsSearchClear:)];
        let _: () = msg_send![button, setAutoresizingMask: 12u64]; // top- and left-anchored
        let _: () = msg_send![button, setHidden: true];
        let label = make_nsstring(&t("settings.sidebar_search_clear"));
        let _: () = msg_send![button, setAccessibilityLabel: label];
        release_obj(label);
        let _: () = msg_send![shell, addSubview: button];
        release_obj(button);
        clear = button;
    }

    // The glyph was released once the shell took ownership of it, so the component never keeps its
    // address: the shell's state is applied to the views it still owns.
    let search = SettingsSearch {
        field,
        shell,
        clear,
        hint,
        ..SettingsSearch::EMPTY
    };
    apply_search_shell_state(&search, false);
    search
}

// MARK: - Sidebar build (brand row, search field, nav, footer)

pub(super) unsafe fn build_settings_sidebar(
    content: *mut AnyObject,
    geometry: SettingsSidebarGeometry,
    palette: UiPalette,
    target: *mut AnyObject,
    ui: &mut SettingsUi,
) {
    // The sidebar registries are keyed by raw view address, so building the sidebar invalidates
    // every address a previous build inserted: the old buttons are released with the old content
    // while their keys would survive. `sidebar_button_under_pointer` then messages those freed
    // pointers on the next hover, which traps in objc's receiver check. Clear here, where the
    // buttons are created, so every rebuild path -- including the in-place
    // `rebuild_settings_content` one, which never goes through window teardown -- drops them.
    widgets::clear_sidebar_view_registries();
    let SettingsSidebarGeometry {
        content_h,
        view_w,
        card_margin,
        card_w,
        card_radius,
    } = geometry;
    // `content_h` is the layout height BELOW the window's title-bar band (the caller measures it):
    // the sidebar is the mock's first .settings-layout column, so its top edge is the band's bottom
    // edge and it never subtracts the band a second time.
    let card_h = content_h - card_margin * 2.0;
    let sidebar_view: *mut AnyObject = msg_send![class!(NSView), alloc];
    let sidebar_view: *mut AnyObject = msg_send![
        sidebar_view,
        initWithFrame: NSRect::new(
            NSPoint::new(card_margin, card_margin),
            NSSize::new(card_w, card_h)
        )
    ];
    let identifier = make_nsstring(SOLID_SIDEBAR_IDENTIFIER);
    let _: () = msg_send![sidebar_view, setIdentifier: identifier];
    release_obj(identifier);
    let _: () = msg_send![sidebar_view, setWantsLayer: true];
    // A plain layer keeps sidebar_bg as a solid color instead of compositing it through system glass.
    let sidebar_layer: *mut AnyObject = msg_send![sidebar_view, layer];
    if !sidebar_layer.is_null() {
        let _: () = msg_send![sidebar_layer, setCornerRadius: card_radius];
        let _: () = msg_send![sidebar_layer, setMasksToBounds: true];
        let _: () = msg_send![sidebar_layer, setOpaque: true];
        layer_set_background(
            sidebar_layer,
            crate::ffi::hex_to_cg_color(palette.sidebar_bg),
        );
    }
    let sidebar_content = sidebar_view;
    // Adaptive: left-anchored, height stretches with the window.
    let _: () = msg_send![sidebar_view, setAutoresizingMask: 20u64];
    let _: () = msg_send![content, addSubview: sidebar_view];
    release_obj(sidebar_view);

    // HTML `.sidebar { border-right: 1px solid rgba(0,0,0,.055) }`.
    let sidebar_divider: *mut AnyObject = msg_send![class!(NSView), alloc];
    let sidebar_divider: *mut AnyObject = msg_send![
        sidebar_divider,
        initWithFrame: NSRect::new(
            NSPoint::new(card_w - 1.0, 0.0),
            NSSize::new(1.0, content_h)
        )
    ];
    let _: () = msg_send![sidebar_divider, setWantsLayer: true];
    let divider_layer: *mut AnyObject = msg_send![sidebar_divider, layer];
    if !divider_layer.is_null() {
        layer_set_background(
            divider_layer,
            crate::ffi::hex_to_cg_color(palette.separator),
        );
    }
    let _: () = msg_send![sidebar_divider, setAutoresizingMask: 20u64];
    let _: () = msg_send![content, addSubview: sidebar_divider];
    release_obj(sidebar_divider);

    // The right detail pane has a flat token-colored surface (no radial highlight), directly
    // beside the gray sidebar.
    let main_background: *mut AnyObject =
        msg_send![widgets::settings_pane_surface_view_class(), alloc];
    let main_background: *mut AnyObject = msg_send![
        main_background,
        initWithFrame: NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(view_w, content_h)
        )
    ];
    let _: () = msg_send![main_background, setWantsLayer: true];
    let main_layer: *mut AnyObject = msg_send![main_background, layer];
    if !main_layer.is_null() {
        layer_set_background(main_layer, crate::ffi::hex_to_cg_color(palette.detail_bg));
    }
    let _: () = msg_send![main_background, setAutoresizingMask: 18u64];
    let _: () = msg_send![
        content,
        addSubview: main_background,
        positioned: -1isize,
        relativeTo: sidebar_view
    ];
    release_obj(main_background);

    // The mock's brand row: a 34pt app icon and the 14pt name beside it, directly under the title
    // bar band (this sidebar starts below the band, where the mock's .settings-layout does).
    let (app_icon, app_name) = brand_row_frames(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(card_w, content_h),
    ));
    let icon_view: *mut AnyObject = msg_send![class!(NSImageView), alloc];
    let icon_view: *mut AnyObject = msg_send![icon_view, initWithFrame: app_icon];
    let image = crate::load_embedded_app_icon();
    if !image.is_null() {
        let _: () = msg_send![icon_view, setImage: image];
        release_obj(image);
    }
    let _: () = msg_send![icon_view, setImageScaling: 3isize]; // NSImageScaleProportionallyUpOrDown
    let _: () = msg_send![icon_view, setEditable: false];
    let _: () = msg_send![icon_view, setWantsLayer: true];
    let icon_layer: *mut AnyObject = msg_send![icon_view, layer];
    if !icon_layer.is_null() {
        // .brand img { border-radius: 9px }: its own curve, not the 8pt control radius.
        let _: () = msg_send![icon_layer, setCornerRadius: SIDEBAR_BRAND_ICON_RADIUS];
        let _: () = msg_send![icon_layer, setMasksToBounds: true];
    }
    let _: () = msg_send![icon_view, setAutoresizingMask: 12u64]; // top- and left-anchored
    let _: () = msg_send![sidebar_content, addSubview: icon_view];
    release_obj(icon_view);

    let app_title: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let app_title: *mut AnyObject = msg_send![app_title, initWithFrame: app_name];
    set_field(app_title, "Oh My Tab");
    let _: () = msg_send![app_title, setBezeled: false];
    let _: () = msg_send![app_title, setDrawsBackground: false];
    let _: () = msg_send![app_title, setEditable: false];
    // .brand-name { font-size: var(--fs-14); font-weight: 600 }.
    let app_title_font: *mut AnyObject = msg_send![
        class!(NSFont),
        systemFontOfSize: crate::theme::FONT_CONTROL,
        weight: crate::theme::FONT_WEIGHT_SEMIBOLD
    ];
    let _: () = msg_send![app_title, setFont: app_title_font];
    let app_title_color = settings_text_color(SettingsTextRole::Primary);
    let _: () = msg_send![app_title, setTextColor: app_title_color];
    // Top- and left-anchored: the window height is adjustable, so the brand stays under the band.
    let _: () = msg_send![app_title, setAutoresizingMask: 12u64];
    let _: () = msg_send![sidebar_content, addSubview: app_title];
    release_obj(app_title);

    // The mock's search field, between the brand row and the nav.
    ui.settings_search = build_settings_search_field(sidebar_content, target, card_w, content_h);

    // Highlight background for the selected sidebar row (layer-backed NSView, theme-aware color);
    // added before the buttons so button titles draw on top of it.
    let btn_w = card_w - 24.0;
    let btn_h = SettingsSidebar::row_height(btn_w);
    let btn_y0 = sidebar_first_nav_row_top(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(card_w, content_h),
    ));
    let highlight: *mut AnyObject = msg_send![class!(NSView), alloc];
    let highlight: *mut AnyObject = msg_send![highlight, initWithFrame: NSRect::new(NSPoint::new(12.0, btn_y0), NSSize::new(btn_w, btn_h))];
    let _: () = msg_send![highlight, setAutoresizingMask: 12u64]; // top- and left-anchored
    let _: () = msg_send![highlight, setWantsLayer: true];
    let hl_layer: *mut AnyObject = msg_send![highlight, layer];
    let _: () = msg_send![hl_layer, setCornerRadius: crate::theme::RADIUS_CONTROL];
    // The mock's `.nav-item.active`: a flat tinted fill (`nav_active_bg`) with its own ink,
    // not the accent-on-wash selection look it replaced.
    layer_set_background(hl_layer, crate::ffi::hex_to_cg_color(palette.nav_active_bg));
    let _: () = msg_send![sidebar_content, addSubview: highlight];
    release_obj(highlight);
    ui.sidebar_highlight = highlight;

    // Sidebar buttons are created in page-index order; each tag selects the matching content view.
    // The slot each button lands in is chosen by page identity, so a table reorder moves the button
    // and the field it is stored in together instead of binding one page's field to another page.
    let sidebar_buttons =
        SettingsSidebar::build(sidebar_content, target, 12.0, btn_y0, btn_w, btn_h);
    for (index, button) in sidebar_buttons.into_iter().enumerate() {
        ui.sidebar_buttons[index] = button;
    }
    widgets::set_sidebar_update_indicator(
        ui.sidebar_button(SettingsPageId::About),
        UPDATE_AVAILABLE.load(Ordering::SeqCst) || crate::restart::restart_required(),
    );

    // HTML `.sidebar-footer`: the complete restore control is one semantic component, with
    // its separator and morphing confirm/cancel rows owned together.
    ui.restore_defaults = RestoreDefaultsControl::build(sidebar_content, target, card_w);
}

#[cfg(test)]
mod brand_row_tests {
    use super::super::{NSPoint, NSRect, NSSize};
    use super::{
        brand_row_frames, sidebar_first_nav_row_top, SIDEBAR_BRAND_ICON_RADIUS,
        SIDEBAR_BRAND_ICON_SIZE, SIDEBAR_BRAND_ROW_H, SIDEBAR_NAV_ROW_H, SIDEBAR_NAV_TOP_GAP,
        SIDEBAR_SEARCH_GAP, SIDEBAR_SEARCH_H, SIDEBAR_SEARCH_TO_NAV,
    };

    // The module itself is named `sidebar`, so the fixture is spelled out rather than shadowing it.
    fn sidebar_box(height: f64) -> NSRect {
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(212.0, height))
    }

    /// The live mock measures .brand at x=39 (window x=27 + sidebar 12) with the layout top at y=79:
    /// the icon is 34pt at 12pt from the sidebar's left edge, and the name begins 8pt after it.
    #[test]
    fn brand_row_sits_one_sidebar_inset_below_the_title_bar_band() {
        let (icon, name) = brand_row_frames(sidebar_box(600.0));
        assert_eq!(icon.origin.x, 20.0);
        // The row's 4 + 34 + 16 box starts one sidebar inset below the top; centering the 34pt icon in
        // it leaves 10pt above the icon (4 + 16 / 2).
        assert_eq!(600.0 - (icon.origin.y + icon.size.height), 10.0);
        assert_eq!(
            icon.size,
            NSSize::new(SIDEBAR_BRAND_ICON_SIZE, SIDEBAR_BRAND_ICON_SIZE)
        );
        assert_eq!(name.origin.x, icon.origin.x + SIDEBAR_BRAND_ICON_SIZE + 8.0);
        assert_eq!(name.origin.x + name.size.width, 200.0);
        // The name is centered on the icon, not hung from its top edge.
        assert_eq!(
            name.origin.y + name.size.height / 2.0,
            icon.origin.y + icon.size.height / 2.0
        );
        assert_eq!(SIDEBAR_BRAND_ICON_RADIUS, 9.0);
    }

    /// The nav begins below the brand row, the search shell and the mock's 16pt gap: the first
    /// row's top edge is the brand row plus 8 + 34 + 16 points down from the sidebar's top.
    #[test]
    fn the_first_nav_row_hangs_below_the_brand_row_and_the_search_shell() {
        let top = sidebar_first_nav_row_top(sidebar_box(600.0));
        assert_eq!(
            600.0 - top,
            SIDEBAR_BRAND_ROW_H
                + SIDEBAR_SEARCH_GAP
                + SIDEBAR_SEARCH_H
                + SIDEBAR_SEARCH_TO_NAV
                + SIDEBAR_NAV_ROW_H
        );
        assert_eq!(SIDEBAR_BRAND_ROW_H, 54.0);
        assert_eq!(SIDEBAR_NAV_TOP_GAP, 16.0);
    }
}

// MARK: - Result row (click target)

/// The class of one result row: a flat view that answers its own press (the mock's
/// `.search-result` button) and paints the mock's hover wash.
fn search_result_row_class() -> *mut AnyObject {
    static CLASS: std::sync::OnceLock<crate::ffi::StaticClass> = std::sync::OnceLock::new();
    crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabSettingsSearchResultRow", || {
        crate::ffi::build_class(
            c"OhMyTabSettingsSearchResultRow",
            class!(NSView),
            |builder| unsafe {
                builder.add_method::<AnyObject, _>(
                    sel!(mouseDown:),
                    search_result_row_mouse_down
                        as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
            },
        )
    })
    .0 as *mut AnyObject
}

// MARK: - Results list

/// The results card hangs from the pane top where the page title does (below the band).
const RESULTS_TOP: f64 = SettingsPageHeader::TOP_PADDING;

/// Height of one result row: the settings row height, so the results list sits on the page grid.
fn result_row_height(row: &ResultRow) -> f64 {
    row.frame.size.height.max(SettingsLayout::SINGLE_LINE_ROW_H)
}

/// Height of one section header above its card (`.section-header`).
fn results_header_height() -> f64 {
    widgets::SECTION_HEADER_H
}

/// Block gap between a section's card bottom and the next section header.
fn results_section_gap() -> f64 {
    super::SETTINGS_SECTION_HEADER_GAP
}

/// The results content's width: the detail pane's content column (the page cards' own width), so a
/// result card and the page card it mirrors share one grid.
fn results_content_width(width: f64) -> f64 {
    (width - super::SETTINGS_DETAIL_INSET).max(1.0)
}

/// One mirrored row: the page row's box in its document and the views it drew, kept only for the
/// duration of one render (the page owns them; a rebuild destroys them).
struct ResultRow {
    entry: SearchEntry,
    frame: NSRect,
    views: Vec<*mut AnyObject>,
    group_hidden: bool,
    content_hidden: Vec<bool>,
}

/// One result chunk: the card header the matched rows belong to, plus those rows. The chunk is the
/// unit a page draws (section header + card), so the results are assembled from the same parts.
struct ResultSection {
    header: String,
    rows: Vec<ResultRow>,
}

/// Group the matched rows by their card header, in index order (page order), and resolve each
/// row's mirrored views from its page. Rows whose page/row no longer exists are skipped: a page
/// rebuilt between the query and the render cannot be mirrored, and drawing a guess is worse.
fn prepare_result_sections(entries: &[SearchEntry], canvases: &[PageCanvas]) -> Vec<ResultSection> {
    let mut sections: Vec<ResultSection> = Vec::new();
    for (header, group) in group_result_entries(entries) {
        let mut rows = Vec::with_capacity(group.len());
        for entry in group {
            if entry.page >= canvases.len() {
                continue;
            }
            let canvas = &canvases[entry.page];
            let (Some(frame), Some(views)) =
                (canvas.row_frame(entry.row), canvas.row_views(entry.row))
            else {
                continue;
            };
            rows.push(ResultRow {
                frame,
                views: views.to_vec(),
                group_hidden: canvas.row_is_hidden(entry.row),
                content_hidden: views
                    .iter()
                    .map(|view| unsafe { canvas.row_view_content_hidden(entry.row, *view) })
                    .collect(),
                entry,
            });
        }
        if !rows.is_empty() {
            sections.push(ResultSection { header, rows });
        }
    }
    sections
}

/// The matched rows grouped by their card header, in index order (page order): one chunk per card,
/// so a result never repeats a heading for every row. Pure, so the grouping is assertable.
fn group_result_entries(entries: &[SearchEntry]) -> Vec<(String, Vec<SearchEntry>)> {
    let mut groups: Vec<(String, Vec<SearchEntry>)> = Vec::new();
    for entry in entries {
        match groups.last_mut() {
            Some((header, group))
                if header == entry.section()
                    && group.last().is_some_and(|last| last.page == entry.page) =>
            {
                group.push(entry.clone())
            }
            _ => groups.push((entry.section().to_string(), vec![entry.clone()])),
        }
    }
    groups
}

/// The matched rows in render order: the identifier a result row carries indexes this list, so a
/// click resolves the same entry the render drew.
fn rendered_entries(sections: &[ResultSection]) -> Vec<SearchEntry> {
    sections
        .iter()
        .flat_map(|section| section.rows.iter().map(|row| row.entry.clone()))
        .collect()
}

/// Height a results document needs for these chunks (each: header + gap + card, then the block
/// gap), or for the empty state. Pure, so the scroll geometry is assertable without a window.
fn results_panel_height(sections: &[ResultSection]) -> f64 {
    if sections.is_empty() {
        return RESULTS_TOP + results_caption_line_height() + SettingsPageHeader::BOTTOM_PADDING;
    }
    let mut height = RESULTS_TOP;
    for (index, section) in sections.iter().enumerate() {
        if index > 0 {
            height += results_section_gap();
        }
        height += results_header_height()
            + super::SETTINGS_SECTION_CARD_GAP
            + SettingsLayout::CARD_BOTTOM_INSET
            + section.rows.iter().map(result_row_height).sum::<f64>();
    }
    height + SettingsPageHeader::BOTTOM_PADDING
}

/// Line box of the empty-state hint: one caption line.
fn results_caption_line_height() -> f64 {
    crate::theme::line_height(
        crate::theme::FONT_CAPTION,
        crate::theme::LINE_HEIGHT_CAPTION_RATIO,
    )
}

/// One caption-role line on the results pane, in the shared text role and font.
unsafe fn make_results_caption(
    frame: NSRect,
    text: &str,
    role: SettingsTextRole,
) -> *mut AnyObject {
    let label = widgets::make_label_with_role(
        frame.origin.x,
        frame.origin.y,
        frame.size.width,
        frame.size.height,
        text,
        role,
    );
    let font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CAPTION];
    let _: () = msg_send![label, setFont: font];
    label
}

struct BorrowedResultView {
    view: *mut AnyObject,
    parent: *mut AnyObject,
    original: NSRect,
    page: usize,
    row: usize,
    row_origin: f64,
    group_hidden: bool,
    content_hidden: bool,
}

thread_local! {
    static BORROWED_RESULT_VIEWS: RefCell<Vec<BorrowedResultView>> = const { RefCell::new(Vec::new()) };
}

unsafe fn restore_result_views() {
    let borrowed = BORROWED_RESULT_VIEWS.with(|slot| std::mem::take(&mut *slot.borrow_mut()));
    if borrowed.is_empty() {
        return;
    }
    with_settings_ui(|ui| {
        for borrowed in borrowed {
            let origin = ui
                .as_ref()
                .and_then(|ui| ui.page_canvases.get(borrowed.page))
                .and_then(|canvas| canvas.row_frame(borrowed.row))
                .map(|frame| frame.origin.y)
                .unwrap_or(borrowed.row_origin);
            let mut frame = borrowed.original;
            frame.origin.y += origin - borrowed.row_origin;
            // addSubview retains the view before releasing its previous parent's ownership.
            let _: () = msg_send![borrowed.parent, addSubview: borrowed.view];
            let _: () = msg_send![borrowed.view, setFrame: frame];
            if borrowed.group_hidden {
                let group_hidden = ui
                    .as_ref()
                    .and_then(|ui| ui.page_canvases.get(borrowed.page))
                    .is_none_or(|canvas| canvas.row_is_hidden(borrowed.row));
                let _: () =
                    msg_send![borrowed.view, setHidden: group_hidden || borrowed.content_hidden];
            }
        }
    });
}

/// The page and results share the same control instances, including their dispatch pointers,
/// state, enabled status, tooltips and custom component behavior.
unsafe fn add_result_row(
    card: *mut AnyObject,
    reflected: &ResultRow,
    row_top: f64,
    content_w: f64,
    index: usize,
    first_in_section: bool,
) -> bool {
    let row_h = result_row_height(reflected);
    let row: *mut AnyObject = msg_send![search_result_row_class(), alloc];
    let row: *mut AnyObject = msg_send![
        row,
        initWithFrame: NSRect::new(
            NSPoint::new(0.0, row_top - row_h),
            NSSize::new(content_w, row_h)
        )
    ];
    let identifier = make_nsstring(&index.to_string());
    let _: () = msg_send![row, setIdentifier: identifier];
    release_obj(identifier);
    if !first_in_section {
        SettingsRow::separator(row, row_h - 1.0, content_w);
    }
    for (view_index, &view) in reflected.views.iter().enumerate() {
        if view.is_null() {
            continue;
        }
        let frame: NSRect = msg_send![view, frame];
        // Page separators are replaced by the shared result-card separator, so the first match
        // in a section never carries the separator of an omitted preceding row.
        if frame.size.height <= 1.0 {
            continue;
        }
        let parent: *mut AnyObject = msg_send![view, superview];
        if parent.is_null() {
            continue;
        }
        BORROWED_RESULT_VIEWS.with(|slot| {
            slot.borrow_mut().push(BorrowedResultView {
                view,
                parent,
                original: frame,
                page: reflected.entry.page,
                row: reflected.entry.row,
                row_origin: reflected.frame.origin.y,
                group_hidden: reflected.group_hidden,
                content_hidden: reflected.content_hidden[view_index],
            })
        });
        let rel = NSRect::new(
            NSPoint::new(
                frame.origin.x - reflected.frame.origin.x,
                frame.origin.y - reflected.frame.origin.y,
            ),
            frame.size,
        );
        let _: () = msg_send![row, addSubview: view];
        let _: () = msg_send![view, setFrame: rel];
        if reflected.group_hidden {
            let _: () = msg_send![view, setHidden: reflected.content_hidden[view_index]];
        }
    }
    let _: () = msg_send![card, addSubview: row];
    release_obj(row);
    true
}

/// Build the results pane content: the empty state, or the matched rows drawn as the page chunks
/// they come from (section header + card + the row's own views). Returns the container and the
/// document height it needs.
unsafe fn build_results_panel(
    document: *mut AnyObject,
    width: f64,
    sections: &[ResultSection],
) -> (*mut AnyObject, f64) {
    let content_w = results_content_width(width);
    let document_h = results_panel_height(sections);
    let container: *mut AnyObject = msg_send![class!(NSView), alloc];
    let container: *mut AnyObject = msg_send![
        container,
        initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(content_w, document_h))
    ];
    let _: () = msg_send![document, addSubview: container];
    // The results document owns the rendered container; the render keeps its borrowed address only
    // to remove it before the next render.
    release_obj(container);
    // The document is not flipped: lay the pane out from its top edge downwards.
    let mut cursor = document_h - RESULTS_TOP;

    if sections.is_empty() {
        let caption_h = results_caption_line_height();
        let available = (content_w).max(1.0);
        let empty = make_results_caption(
            NSRect::new(
                NSPoint::new(0.0, cursor - caption_h),
                NSSize::new(available, caption_h),
            ),
            &t("settings.sidebar_search_empty"),
            SettingsTextRole::Muted,
        );
        // Centered by GEOMETRY, not by the cell's alignment: center alignment means different enum
        // values on different AppKit architectures (see `TEXT_ALIGNMENT_RIGHT` in components), so
        // the frame is the text's own width placed on the content column's midline.
        let cell: *mut AnyObject = msg_send![empty, cell];
        let text_size: NSSize = msg_send![cell, cellSize];
        let hint_w = if text_size.width.is_finite() && text_size.width > 0.0 {
            text_size.width.min(available)
        } else {
            available
        };
        let hint_frame = NSRect::new(
            NSPoint::new((content_w - hint_w) / 2.0, cursor - caption_h),
            NSSize::new(hint_w, caption_h),
        );
        let _: () = msg_send![empty, setFrame: hint_frame];
        let _: () = msg_send![container, addSubview: empty];
        release_obj(empty);
        return (container, document_h);
    }

    let header_h = results_header_height();
    let mut result_index = 0usize;
    for (section_index, section) in sections.iter().enumerate() {
        if section_index > 0 {
            cursor -= results_section_gap();
        }
        // Section header: the shared header component, one card title above its card.
        widgets::add_header(
            container,
            &section.header,
            0.0,
            cursor - header_h,
            content_w,
        );
        let card_top = cursor - header_h - super::SETTINGS_SECTION_CARD_GAP;
        let card_h = SettingsLayout::CARD_BOTTOM_INSET
            + section.rows.iter().map(result_row_height).sum::<f64>();
        let card = widgets::add_settings_card(
            container,
            NSRect::new(
                NSPoint::new(0.0, card_top - card_h),
                NSSize::new(content_w, card_h),
            ),
        );
        let using_container = card.is_null();
        let card = if using_container { container } else { card };
        let rows_top = if using_container {
            card_top
        } else {
            card_h - SettingsLayout::CARD_BOTTOM_INSET
        };
        let mut row_top = rows_top;
        for (row_index, reflected) in section.rows.iter().enumerate() {
            if add_result_row(
                card,
                reflected,
                row_top,
                content_w,
                result_index,
                row_index == 0,
            ) {
                result_index += 1;
            }
            row_top -= result_row_height(reflected);
        }
        cursor = card_top - card_h;
    }

    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(content_w, document_h));
    let _: () = msg_send![container, setFrame: frame];
    (container, document_h)
}

/// The results view's live geometry: the container spans the results view's width, and the
/// viewport is the scroll view's current height. The settings window is height-resizable, so both
/// are read at render time instead of trusting the values captured when the window was built.
/// Pure, so a resized window's effect on the layout is assertable without a window.
fn results_viewport_metrics(results: NSRect, scroll: NSRect) -> (f64, f64) {
    (results.size.width.max(1.0), scroll.size.height.max(1.0))
}

/// Render the live query into the open results view, and return how many entries it drew.
unsafe fn render_results_into(search: &mut SettingsSearch) -> usize {
    if search.results.is_null() || search.results_doc.is_null() {
        return 0;
    }
    // The previous render's rows go first: they are keyed by position, and a stale row would
    // answer its tag from a list that no longer exists. Removing it from the document releases the
    // document's ownership; we hold no reference of our own.
    if !search.results_rows.is_null() {
        let _: () = msg_send![search.results_rows, removeFromSuperview];
        search.results_rows = std::ptr::null_mut();
    }
    let entries = settings_search_results();
    let sections = with_settings_ui(|ui| {
        ui.as_ref()
            .map(|ui| prepare_result_sections(&entries, &ui.page_canvases))
            .unwrap_or_default()
    });
    let rendered = rendered_entries(&sections);
    // A resized window changes the viewport under the results view; measure it now, or a longer
    // list would be scrolled against the height the window had when the search started.
    let results_frame: NSRect = msg_send![search.results, frame];
    let scroll_frame: NSRect = msg_send![search.results_scroll, frame];
    let (live_w, live_h) = results_viewport_metrics(results_frame, scroll_frame);
    search.results_w = live_w;
    search.results_h = live_h;
    log_debug!(
        "[settings] results query={:?} entries={} rendered={} sections={} index={}",
        settings_search_query(),
        entries.len(),
        rendered.len(),
        sections.len(),
        SETTINGS_SEARCH_INDEX.lock().unwrap().len()
    );
    *RENDERED_RESULTS.lock().unwrap() = rendered;
    let (rows, height) = build_results_panel(search.results_doc, search.results_w, &sections);
    search.results_rows = rows;
    let (doc_h, panel_origin_y) = results_document_geometry(height, search.results_h);
    let doc_frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(search.results_w, doc_h));
    let _: () = msg_send![search.results_doc, setFrame: doc_frame];
    // The document is not flipped: laying the panel out from the document's top edge is what puts
    // the summary (or the empty state) where a page's title sits, instead of at the pane's bottom.
    let panel_frame = NSRect::new(
        NSPoint::new(0.0, panel_origin_y),
        NSSize::new(search.results_w, height),
    );
    let _: () = msg_send![search.results_rows, setFrame: panel_frame];
    let clip: *mut AnyObject = msg_send![search.results_scroll, contentView];
    let _: () = msg_send![clip, scrollToPoint: NSPoint::new(0.0, doc_h - search.results_h)];
    let _: () = msg_send![search.results_scroll, reflectScrolledClipView: clip];
    sections.iter().map(|section| section.rows.len()).sum()
}

/// The results document's height and the y its panel starts at, for a panel `panel_h` tall in a
/// `viewport_h` viewport.
///
/// The document is never shorter than the viewport (AppKit pins a shorter unflipped document to the
/// clip's bottom), and the panel hangs from the document's TOP edge: the results list then reads
/// from where a page's title sits. A panel taller than the viewport scrolls instead, from the
/// document's own top. Pure, so the anchoring is assertable without a window.
fn results_document_geometry(panel_h: f64, viewport_h: f64) -> (f64, f64) {
    let doc_h = panel_h.max(viewport_h);
    (doc_h, doc_h - panel_h)
}

/// Focus the sidebar's search field, the `⌘F` shortcut. Returns whether there was a field to focus,
/// so the window's key-equivalent override only claims the chord when the settings window is the one
/// showing the shell.
pub(super) fn focus_settings_search() -> bool {
    unsafe {
        let Some(search) = with_settings_ui(|ui| ui.as_ref().map(|ui| ui.settings_search)) else {
            return false;
        };
        if search.field.is_null() {
            return false;
        }
        // `makeFirstResponder:` puts the caret in the field; the search field class applies the
        // focused shell state from the editing callbacks, so the ring and the text agree with the
        // keyboard. Selecting the range matches what clicking into an empty field does.
        let window: *mut AnyObject = msg_send![search.field, window];
        if window.is_null() {
            return false;
        }
        let ok: bool = msg_send![window, makeFirstResponder: search.field];
        if ok {
            let _: () = msg_send![search.field, selectText: std::ptr::null::<AnyObject>()];
        }
        ok
    }
}

/// Whether the search field currently holds the input focus: it selects the mock's focused shell
/// treatment, and it makes Esc mean "clear the query" instead of "close the window".
static SEARCH_FOCUSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The row a result click asked for. The page's layout is only settled once it is on screen, so
/// the jump is stashed here and applied after the page switch.
static PENDING_ROW_FOCUS: LazyLock<Mutex<Option<(usize, usize)>>> =
    LazyLock::new(|| Mutex::new(None));

/// Apply the live query to the sidebar and the detail pane: the results in place of the pages
/// while a query is active, the normal page content back when it is empty.
///
/// Only visibility and the results content change here -- never a page frame or the nav, whose
/// rows are positioned from a fixed sidebar layout -- so typing cannot make the nav jump.
pub(super) unsafe fn refresh_settings_search() {
    let active = settings_search_is_active();
    let query = settings_search_query();
    let Some(mut search) = with_settings_ui(|ui| ui.as_ref().map(|ui| ui.settings_search)) else {
        return;
    };
    if search.results.is_null() {
        return;
    }
    restore_result_views();
    if active {
        materialize_mouse_page_for_search();
        with_settings_ui(|ui| {
            if let Some(ui) = ui.as_ref() {
                for (page, canvas) in ui.page_canvases.iter().enumerate() {
                    page_builder::sweep_unindexed_rows(canvas, settings_page_identity(page));
                }
            }
        });
    }
    if !search.field.is_null() {
        let current: *mut AnyObject = msg_send![search.field, stringValue];
        if crate::ffi::nsstring_to_rust(current) != query {
            set_field(search.field, &query);
        }
    }
    set_view_hidden(search.clear, query.is_empty());
    set_view_hidden(search.hint, !query.is_empty());
    if active {
        render_results_into(&mut search);
    } else if !search.results_rows.is_null() {
        let _: () = msg_send![search.results_rows, removeFromSuperview];
        search.results_rows = std::ptr::null_mut();
        RENDERED_RESULTS.lock().unwrap().clear();
    }
    let _: () = msg_send![search.results, setHidden: !active];
    with_settings_ui(|ui| {
        if let Some(ui) = ui.as_mut() {
            let selected = SIDEBAR_SELECTED.load(Ordering::SeqCst);
            for (index, view) in ui.page_views.iter().enumerate() {
                let _: () = msg_send![*view, setHidden: active || index != selected];
            }
            ui.settings_search = search;
        }
    });
    crate::e2e_state::record("settings");
}

/// Set the query from the field (typing, the clear chip, Esc) and re-render.
pub(super) fn apply_settings_search_query(query: &str) {
    let was_active = settings_search_is_active();
    set_settings_search_query(query);
    let active = settings_search_is_active();
    if !was_active && active {
        // Starting a search covers the page the user was reading; ending it returns there.
        SETTINGS_SEARCH.lock().unwrap().return_page = Some(SIDEBAR_SELECTED.load(Ordering::SeqCst));
    }
    unsafe { refresh_settings_search() };
    if was_active && !active {
        let page = SETTINGS_SEARCH
            .lock()
            .unwrap()
            .return_page
            .take()
            .unwrap_or_else(|| SIDEBAR_SELECTED.load(Ordering::SeqCst));
        select_sidebar(page);
    }
}

/// The field's text changed (its text-change notification): search for what it holds.
pub(super) fn settings_search_field_changed(field: *mut AnyObject) {
    if field.is_null() {
        return;
    }
    let text = unsafe {
        let value: *mut AnyObject = msg_send![field, stringValue];
        crate::ffi::nsstring_to_rust(value)
    };
    apply_settings_search_query(&text);
}

/// Clear the query: the clear chip and Esc both land here.
pub(super) fn clear_settings_search() {
    if settings_search_is_active() {
        apply_settings_search_query("");
    }
}

/// A1 smoke check (`--smoke-settings-layout`): drive the live search through a real query and
/// verify the two facts a screenshot had to catch before -- a row on any page is found (the index
/// spans every page, not just the last one built), and the results/empty state occupy exactly the
/// page area, top-anchored and centered -- then put the window back as it was.
pub(super) unsafe fn settings_search_smoke_check() -> bool {
    let original = with_settings_ui(|ui| {
        ui.as_ref().map(|ui| {
            let view = ui.launch_at_login;
            let parent: *mut AnyObject = msg_send![view, superview];
            (view, parent)
        })
    });
    let Some((control, original_parent)) = original else {
        return false;
    };
    let row_label = t("settings.row_launch_at_login");
    let mut ok = true;
    for query in [&row_label, &t("settings.desc_launch_at_login")] {
        if query.is_empty() {
            continue;
        }
        apply_settings_search_query(query);
        let entries = RENDERED_RESULTS.lock().unwrap().clone();
        let row_matches = entries.iter().any(|entry| entry.label() == row_label);
        let live = with_settings_ui(|ui| {
            ui.as_ref().is_some_and(|ui| {
                let parent: *mut AnyObject = msg_send![control, superview];
                let descendant: bool =
                    msg_send![control, isDescendantOf: ui.settings_search.results_rows];
                let card: *mut AnyObject = msg_send![parent, superview];
                let bounds: NSRect = msg_send![control, bounds];
                let point: NSPoint = msg_send![control,
                    convertPoint: NSPoint::new(bounds.size.width / 2.0, bounds.size.height / 2.0),
                    toView: card
                ];
                let hit: *mut AnyObject = msg_send![parent, hitTest: point];
                let hits_control =
                    !hit.is_null() && (hit == control || msg_send![hit, isDescendantOf: control]);
                parent != original_parent && descendant && hits_control
            })
        });
        ok &= row_matches && live;
    }

    // An unselected option matches its owning row, without dragging along its card's sibling.
    let option = t("settings.panel_material_frost");
    apply_settings_search_query(&option);
    let entries = RENDERED_RESULTS.lock().unwrap().clone();
    ok &= entries
        .iter()
        .any(|entry| entry.label() == t("settings.row_panel_material"));
    ok &= !entries
        .iter()
        .any(|entry| entry.label() == t("settings.row_theme"));
    ok &= with_settings_ui(|ui| {
        ui.as_ref().is_some_and(|ui| {
            let descendant: bool =
                msg_send![ui.panel_material, isDescendantOf: ui.settings_search.results_rows];
            let original: isize = msg_send![ui.panel_material, indexOfSelectedItem];
            let _: () = msg_send![ui.panel_material, selectItemAtIndex: 1isize];
            let selected: isize = msg_send![ui.panel_material, indexOfSelectedItem];
            let _: () = msg_send![ui.panel_material, selectItemAtIndex: original];
            descendant && selected == 1
        })
    });

    let glass_hidden = with_settings_ui(|ui| {
        ui.as_ref().map(|ui| {
            let hidden: bool = msg_send![ui.glass_style, isHidden];
            hidden
        })
    });
    apply_settings_search_query(&t("settings.row_glass_style"));
    ok &= with_settings_ui(|ui| {
        ui.as_ref().is_some_and(|ui| {
            let hidden: bool = msg_send![ui.glass_style, isHidden];
            let descendant: bool =
                msg_send![ui.glass_style, isDescendantOf: ui.settings_search.results_rows];
            !hidden && descendant
        })
    });
    clear_settings_search();
    ok &= with_settings_ui(|ui| {
        ui.as_ref().is_some_and(|ui| {
            let hidden: bool = msg_send![ui.glass_style, isHidden];
            Some(hidden) == glass_hidden
        })
    });

    // Every live row fits inside its result-card row, including the captioned and variable-height
    // rows. No transparent click catcher may cover the controls.
    apply_settings_search_query(&row_label.chars().take(1).collect::<String>());
    ok &= BORROWED_RESULT_VIEWS.with(|slot| {
        slot.borrow().iter().all(|borrowed| {
            let parent: *mut AnyObject = msg_send![borrowed.view, superview];
            let frame: NSRect = msg_send![borrowed.view, frame];
            let bounds: NSRect = msg_send![parent, bounds];
            frame.origin.y >= -4.0 && frame.origin.y + frame.size.height <= bounds.size.height + 4.0
        })
    });

    apply_settings_search_query("\u{1}no-such-setting");
    ok &= RENDERED_RESULTS.lock().unwrap().is_empty();
    ok &= with_settings_ui(|ui| {
        ui.as_ref().is_some_and(|ui| {
            let search = ui.settings_search;
            let doc: NSRect = msg_send![search.results_doc, frame];
            let panel: NSRect = msg_send![search.results_rows, frame];
            let views: *mut AnyObject = msg_send![search.results_rows, subviews];
            let count: usize = msg_send![views, count];
            count == 1 && (doc.size.height - panel.size.height - panel.origin.y).abs() < 0.5
        })
    });
    clear_settings_search();
    ok &= with_settings_ui(|ui| {
        ui.as_ref().is_some_and(|ui| {
            let parent: *mut AnyObject = msg_send![control, superview];
            parent == original_parent
                && ui.page_canvases.iter().all(|canvas| {
                    (0..=canvas.last_row()).all(|row| canvas.row_box_holds_its_views(row))
                })
        })
    });
    ok &= with_settings_ui(|ui| {
        ui.as_ref().is_some_and(|ui| {
            let document: *mut AnyObject =
                msg_send![ui.page_views[SETTINGS_GENERAL_PAGE_INDEX], documentView];
            flash_row_band(
                document,
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(120.0, 20.0)),
            );
            let transparent = matches!(search_flash_state(), Some((_, true)));
            clear_search_flash();
            transparent && search_flash_state().is_none()
        })
    });
    log_info!(
        "[smoke-settings-search] live controls, matched-only cards, options, descriptions and restore ok={ok}"
    );
    ok
}

/// Repaint the shell for the current editor state (the mock's focused ring and field step).
pub(super) fn set_settings_search_focused(focused: bool) {
    SEARCH_FOCUSED.store(focused, Ordering::SeqCst);
    unsafe {
        with_settings_ui(|ui| {
            if let Some(ui) = ui.as_ref() {
                apply_search_shell_state(&ui.settings_search, focused);
            }
        });
    }
}

/// Jump to one result: switch to its page, scroll the row into view, flash it, and end the search.
///
/// `index` is the row's tag inside the live results view, which indexes the same result list the
/// render used, so a click can never navigate to a row that no longer exists.
pub(super) unsafe fn activate_settings_search_result(index: usize) {
    let rendered = RENDERED_RESULTS.lock().unwrap().clone();
    let entry = match rendered.get(index) {
        Some(entry) => entry.clone(),
        None => return,
    };
    // The row is applied after the page switch: the page's cards are only settled once it is on
    // screen, and the flash has to follow the row's own frame.
    PENDING_ROW_FOCUS
        .lock()
        .unwrap()
        .replace((entry.page, entry.row));
    set_settings_search_query("");
    refresh_settings_search();
    // Switching through the sidebar keeps its page-editor bookkeeping.
    select_sidebar(entry.page);
    refresh_settings_search();
    flash_pending_row();
}

/// Scroll this page's requested row into view and flash its card, then forget the request.
unsafe fn flash_pending_row() {
    let Some((page, row)) = PENDING_ROW_FOCUS.lock().unwrap().take() else {
        return;
    };
    with_settings_ui(|ui| {
        let Some(ui) = ui.as_ref() else {
            return;
        };
        if page >= SETTINGS_PAGE_COUNT {
            return;
        }
        let scroll_view = ui.page_views[page];
        if scroll_view.is_null() {
            return;
        }
        let clip: *mut AnyObject = msg_send![scroll_view, contentView];
        let document: *mut AnyObject = msg_send![scroll_view, documentView];
        if clip.is_null() || document.is_null() {
            return;
        }
        let clip_bounds: NSRect = msg_send![clip, bounds];
        let document_frame: NSRect = msg_send![document, frame];
        // The row's box comes from the page's layout owner, which knows exactly where it placed
        // the row: the search index records the row's ordinal in build order.
        let Some(frame_rect) = ui.page_canvases[page].row_frame(row) else {
            return;
        };
        let target_y = frame_rect.origin.y + frame_rect.size.height / 2.0;
        let target = centered_scroll_origin(
            target_y,
            clip_bounds.size.height,
            document_frame.size.height,
        );
        let _: () = msg_send![
            clip,
            scrollToPoint: NSPoint::new(clip_bounds.origin.x, target)
        ];
        let _: () = msg_send![scroll_view, reflectScrolledClipView: clip];
        flash_row_band(document, frame_rect);
    });
}

/// A result row's press: the row answers it itself, so the click cannot fall through to the page
/// underneath the results view.
pub(super) extern "C" fn search_result_row_mouse_down(
    this: *mut AnyObject,
    _cmd: Sel,
    _event: *mut AnyObject,
) {
    // A rebuild/hide running inside `with_settings_ui` may deliver this callback
    // re-entrantly (e.g. resigning a field's first responder). Borrow-check it: a dropped
    // notification loses one refresh; an abort loses the app.
    if crate::settings::settings_ui_borrowed() {
        return;
    }

    unsafe {
        let identifier: *mut AnyObject = msg_send![this, identifier];
        if identifier.is_null() {
            return;
        }
        if let Ok(index) = crate::ffi::nsstring_to_rust(identifier).parse::<usize>() {
            activate_settings_search_result(index);
        }
    }
}

fn search_results_view_class_handle() -> crate::ffi::StaticClass {
    static CLASS: std::sync::OnceLock<crate::ffi::StaticClass> = std::sync::OnceLock::new();
    crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabSearchResultsView", || {
        crate::ffi::build_class(
            c"OhMyTabSearchResultsView",
            class!(NSView),
            |builder| unsafe {
                builder.add_method::<AnyObject, _>(
                    sel!(setFrameSize:),
                    search_results_view_set_frame_size
                        as unsafe extern "C" fn(*mut AnyObject, Sel, NSSize),
                );
            },
        )
    })
}

fn search_results_view_class() -> *mut AnyObject {
    search_results_view_class_handle().0 as *mut AnyObject
}

/// Resize hook: after AppKit resizes the results view (the window's height changed), re-render the
/// live query against the new viewport. The refresh is deferred to the next run-loop turn so it
/// never re-enters AppKit's layout pass.
extern "C" fn search_results_view_set_frame_size(this: *mut AnyObject, _cmd: Sel, size: NSSize) {
    unsafe {
        type F = unsafe extern "C" fn(*mut crate::ffi::ObjcSuper, Sel, NSSize);
        let mut sup = crate::ffi::ObjcSuper::for_defined_class(
            this as *mut std::ffi::c_void,
            search_results_view_class_handle(),
        );
        let send: F = std::mem::transmute(crate::ffi::objc_msgSendSuper as *const ());
        send(&mut sup, sel!(setFrameSize:), size);
        schedule_search_refresh();
    }
}

pub(super) fn schedule_search_refresh() {
    unsafe {
        let target = search_refresh_target();
        let _: () = msg_send![class!(NSObject),
            cancelPreviousPerformRequestsWithTarget: target,
            selector: sel!(refreshSearchViews:),
            object: std::ptr::null::<AnyObject>()
        ];
        let _: () = msg_send![
            target,
            performSelector: sel!(refreshSearchViews:),
            withObject: std::ptr::null::<AnyObject>(),
            afterDelay: 0.0f64
        ];
    }
}

unsafe fn search_refresh_target() -> *mut AnyObject {
    static CLASS: std::sync::OnceLock<crate::ffi::StaticClass> = std::sync::OnceLock::new();
    let target: *mut AnyObject =
        crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabSearchRefreshTarget", || {
            crate::ffi::build_class(
                c"OhMyTabSearchRefreshTarget",
                class!(NSObject),
                |builder| unsafe {
                    builder.add_method::<AnyObject, _>(
                        sel!(refreshSearchViews:),
                        search_refresh_target_fired
                            as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                    );
                },
            )
        })
        .0 as *mut AnyObject;
    static TARGET: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *TARGET.get_or_init(|| {
        let instance: *mut AnyObject = msg_send![target, new];
        instance as usize
    }) as *mut AnyObject
}

extern "C" fn search_refresh_target_fired(
    _this: *mut AnyObject,
    _cmd: Sel,
    _argument: *mut AnyObject,
) {
    // A rebuild/hide running inside `with_settings_ui` may deliver this callback
    // re-entrantly (e.g. resigning a field's first responder). Borrow-check it: a dropped
    // notification loses one refresh; an abort loses the app.
    if crate::settings::settings_ui_borrowed() {
        return;
    }

    if settings_search_is_active() {
        unsafe { refresh_settings_search() };
    }
}

/// Build the results shell over the detail pane. Called once per settings build, after the pages
/// exist (the results cover them; the pages keep their frames, so leaving the search returns the
/// user to the same scroll position).
pub(super) unsafe fn build_settings_search_results(
    search: &mut SettingsSearch,
    content: *mut AnyObject,
    frame: NSRect,
) {
    // The previous build's results view is a child of the old content view and goes with it; the
    // pointer is only cleared here so a stale address is never messaged.
    search.results = std::ptr::null_mut();
    search.results_scroll = std::ptr::null_mut();
    search.results_doc = std::ptr::null_mut();
    search.results_rows = std::ptr::null_mut();
    let results: *mut AnyObject = msg_send![search_results_view_class(), alloc];
    let results: *mut AnyObject = msg_send![results, initWithFrame: frame];
    let _: () = msg_send![results, setAutoresizingMask: 18u64];
    let _: () = msg_send![results, setHidden: true];
    let _: () = msg_send![results, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![results, layer];
    if !layer.is_null() {
        layer_set_background(
            layer,
            crate::ffi::hex_to_cg_color(settings_palette().detail_bg),
        );
    }
    let _: () = msg_send![content, addSubview: results];
    release_obj(results);
    // The scroll view is built INSIDE the results view, so its frame is local to it: passing the
    // page-area frame here as well would offset the whole results document by the results view's
    // own origin (the empty state ended up clipped past the window's right edge).
    let local_frame = NSRect::new(NSPoint::new(0.0, 0.0), frame.size);
    let (scroll, document) =
        widgets::make_settings_page(results, local_frame, local_frame.size.height, false);
    search.results = results;
    search.results_scroll = scroll;
    search.results_doc = document;
    search.results_w = frame.size.width;
    search.results_h = frame.size.height;
    search.results_rows = std::ptr::null_mut();
}

// MARK: - Row jump

// The document views are laid out in one flat coordinate space and every row is a contiguous
// block of them, so a row is found by its position among the page's views: the index records
// exactly that ordinal, and a page rebuild cannot mis-address a row.

/// The clip origin that puts a document point on the middle of the viewport, clamped to the
/// scrollable range `[0, document_h - visible_h]`.
///
/// The page document is not flipped: y grows upward and the clip origin is the bottom edge of the
/// visible band. Pure, so the jump's landing position is assertable without a window.
fn centered_scroll_origin(point_y: f64, visible_h: f64, document_h: f64) -> f64 {
    let max_origin = (document_h - visible_h).max(0.0);
    // The absolute origin that puts the point on the viewport's midline, clamped to the scroll
    // range. Clamping the OFFSET from the current origin first (the old formula) lost the negative
    // direction, so a row below the current band never scrolled into view.
    (point_y - visible_h / 2.0).clamp(0.0, max_origin)
}

/// How long the jump's row wash stays on screen: the fade-out under normal motion, and a short
/// static hold under Reduce Motion (which skips the animation but still marks the row briefly).
const ROW_FLASH_HOLD_SECS: f64 = crate::theme::ANIMATION_DURATION_MEDIUM * 4.0;

/// Draw the mock's `.row-flash` over a jumped-to row: a selection wash with its accent hairline
/// that fades out and then removes itself. It declines hit testing, so the row's controls stay
/// reachable while it is on screen, and it never outlives the page document it is drawn into (a
/// rebuild destroys both).
unsafe fn flash_row_band(document: *mut AnyObject, rect: NSRect) {
    // A second jump replaces the previous flash's leftover instead of stacking another overlay.
    clear_search_flash();
    let palette = settings_palette();
    let flash: *mut AnyObject = msg_send![widgets::passthrough_view_class(), alloc];
    let flash: *mut AnyObject = msg_send![flash, initWithFrame: rect];
    let _: () = msg_send![flash, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![flash, layer];
    if layer.is_null() {
        release_obj(flash);
        return;
    }
    layer_set_background(layer, crate::ffi::hex_to_cg_color(palette.selection_bg));
    crate::ffi::layer_set_border(layer, crate::ffi::hex_to_cg_color(palette.accent));
    let _: () = msg_send![layer, setBorderWidth: 1.0f64];
    let _: () = msg_send![layer, setCornerRadius: crate::theme::RADIUS_CONTROL];
    let _: () = msg_send![document, addSubview: flash];
    // The layer animation moves only the presentation layer; the view itself must be removed when
    // the wash is over, or it would stay on screen at full opacity and swallow the row's clicks.
    let hold = if crate::theme::reduce_motion_enabled() {
        ROW_FLASH_HOLD_SECS.min(0.6)
    } else {
        let key_path = make_nsstring("opacity");
        let animation: *mut AnyObject = msg_send![
            class!(CABasicAnimation),
            animationWithKeyPath: key_path
        ];
        release_obj(key_path);
        let from: *mut AnyObject = msg_send![class!(NSNumber), numberWithDouble: 1.0f64];
        let to: *mut AnyObject = msg_send![class!(NSNumber), numberWithDouble: 0.0f64];
        let _: () = msg_send![animation, setFromValue: from];
        let _: () = msg_send![animation, setToValue: to];
        let _: () = msg_send![animation, setDuration: ROW_FLASH_HOLD_SECS];
        let _: () =
            msg_send![layer, addAnimation: animation, forKey: std::ptr::null::<AnyObject>()];
        ROW_FLASH_HOLD_SECS
    };
    schedule_flash_removal(flash, hold);
    release_obj(flash);
}

static SEARCH_FLASH: LazyLock<Mutex<SearchFlash>> =
    LazyLock::new(|| Mutex::new(SearchFlash::EMPTY));

/// The live row wash and its removal timer, so a later jump or a settings teardown can cancel the
/// pending removal before the page document (which owns the view) goes away.
struct SearchFlash {
    view: *mut AnyObject,
    timer: *mut AnyObject,
}

impl SearchFlash {
    const EMPTY: Self = Self {
        view: std::ptr::null_mut(),
        timer: std::ptr::null_mut(),
    };
}

unsafe impl Send for SearchFlash {}
unsafe impl Sync for SearchFlash {}

unsafe fn search_flash_target() -> *mut AnyObject {
    static CLASS: std::sync::OnceLock<crate::ffi::StaticClass> = std::sync::OnceLock::new();
    let target: *mut AnyObject =
        crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabSearchFlashTarget", || {
            crate::ffi::build_class(
                c"OhMyTabSearchFlashTarget",
                class!(NSObject),
                |builder| unsafe {
                    builder.add_method::<AnyObject, _>(
                        sel!(removeFlash:),
                        search_flash_timer_fired
                            as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                    );
                },
            )
        })
        .0 as *mut AnyObject;
    static TARGET: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let instance: *mut AnyObject = unsafe { msg_send![target, new] };
    *TARGET.get_or_init(|| instance as usize) as *mut AnyObject
}

extern "C" fn search_flash_timer_fired(_this: *mut AnyObject, _cmd: Sel, _timer: *mut AnyObject) {
    // No settings_ui_borrowed() guard here: this callback never borrows SETTINGS_UI, and an early
    // return would leave `flash.timer` pointing at a self-invalidated one-shot timer, which the
    // next `clear_search_flash` would invalidate a second time.
    unsafe {
        let mut flash = SEARCH_FLASH.lock().unwrap();
        if !flash.view.is_null() {
            let _: () = msg_send![flash.view, removeFromSuperview];
        }
        flash.view = std::ptr::null_mut();
        flash.timer = std::ptr::null_mut();
    }
}

/// Schedule the flash view's removal; `delay` is the fade time (a short hold under Reduce Motion).
unsafe fn schedule_flash_removal(view: *mut AnyObject, delay: f64) {
    let timer: *mut AnyObject = msg_send![
        class!(NSTimer),
        scheduledTimerWithTimeInterval: delay.max(0.05),
        target: search_flash_target(),
        selector: sel!(removeFlash:),
        userInfo: std::ptr::null::<AnyObject>(),
        repeats: false
    ];
    let mut flash = SEARCH_FLASH.lock().unwrap();
    flash.view = view;
    flash.timer = timer;
}

/// Cancel a pending row-wash removal and drop the view's registration. Called before a jump
/// replaces the wash and before the settings views are torn down (the page document owns it).
pub(super) unsafe fn clear_search_flash() {
    let mut flash = SEARCH_FLASH.lock().unwrap();
    if !flash.timer.is_null() {
        let _: () = msg_send![flash.timer, invalidate];
    }
    if !flash.view.is_null() {
        let _: () = msg_send![flash.view, removeFromSuperview];
    }
    *flash = SearchFlash::EMPTY;
}

/// Whether the row wash is on screen and click-through: the smoke check asserts both, because a
/// flash that never leaves swallows the row's controls for good.
pub(super) unsafe fn search_flash_state() -> Option<(&'static str, bool)> {
    let flash = SEARCH_FLASH.lock().unwrap();
    if flash.view.is_null() {
        return None;
    }
    let hit: *mut AnyObject = msg_send![flash.view, hitTest: NSPoint::new(1.0, 1.0)];
    Some(("panel", hit.is_null()))
}

/// The class of the search field itself: Esc while editing must clear the query, not reach the
/// window's key handling and close anything. The field editor forwards Esc as `cancelOperation:`.
fn settings_search_field_class() -> *mut AnyObject {
    static CLASS: std::sync::OnceLock<crate::ffi::StaticClass> = std::sync::OnceLock::new();
    crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabSettingsSearchField", || {
        crate::ffi::build_class(
            c"OhMyTabSettingsSearchField",
            class!(NSTextField),
            |builder| unsafe {
                builder.add_method::<AnyObject, _>(
                    sel!(cancelOperation:),
                    settings_search_field_cancel
                        as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
            },
        )
    })
    .0 as *mut AnyObject
}

/// Esc in the search field: clear the query (the mock's Esc behaviour) and keep the field's
/// focus, so the next keystroke types a new query instead of reopening what the user was hiding.
pub(super) extern "C" fn settings_search_field_cancel(
    this: *mut AnyObject,
    _cmd: Sel,
    _sender: *mut AnyObject,
) {
    // A rebuild/hide running inside `with_settings_ui` may deliver this callback
    // re-entrantly (e.g. resigning a field's first responder). Borrow-check it: a dropped
    // notification loses one refresh; an abort loses the app.
    if crate::settings::settings_ui_borrowed() {
        return;
    }

    clear_settings_search();
    unsafe {
        let value: *mut AnyObject = msg_send![this, stringValue];
        if !crate::ffi::nsstring_to_rust(value).is_empty() {
            set_field(this, "");
            apply_settings_search_query("");
        }
    }
}

#[cfg(test)]
mod search_tests {
    use super::super::{NSPoint, NSRect, NSSize};
    use super::{
        apply_settings_search_section, begin_settings_search_index, begin_settings_search_section,
        centered_scroll_origin, group_result_entries, match_settings_search,
        normalize_search_query, record_settings_search_entry,
        record_settings_search_entry_sectioned, results_document_geometry, results_header_height,
        results_panel_height, results_section_gap, search_hint_frame, search_shell_frames,
        search_trailing_slot_width, set_search_options_for_last_row, settings_search_index,
        sidebar_first_nav_row_top, sidebar_search_shell_frame, ResultRow, ResultSection,
        SearchEntry, SIDEBAR_NAV_ROW_H,
    };
    use crate::settings::SettingsLayout;

    fn sidebar_box(height: f64) -> NSRect {
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(212.0, height))
    }

    /// The mock's `.field-shell` box and its contents: the field sits below the brand row, the
    /// magnifier starts at the shell's 1pt border plus its 8pt padding, and every glyph shares the
    /// shell's midline.
    #[test]
    fn the_search_shell_sits_between_the_brand_row_and_the_nav() {
        let shell = sidebar_search_shell_frame(sidebar_box(600.0));
        assert_eq!(shell.origin.x, 12.0);
        assert_eq!(shell.size.width, 188.0);
        assert_eq!(shell.size.height, 34.0);
        // Brand row (54) then the shell's 8pt gap: the shell's top edge is 62 under the sidebar's top.
        assert_eq!(600.0 - (shell.origin.y + shell.size.height), 62.0);
        // The nav starts 16pt under the shell's bottom edge and keeps its distance from the field,
        // so typing a query never moves it.
        assert_eq!(
            sidebar_first_nav_row_top(sidebar_box(600.0)),
            shell.origin.y - 16.0 - SIDEBAR_NAV_ROW_H
        );

        let (glyph, input, clear) = search_shell_frames(shell, false, 16.0);
        assert_eq!(glyph.origin.x, 12.0 + 1.0 + 8.0);
        assert_eq!(glyph.size.width, 14.0);
        assert_eq!(
            glyph.origin.y + glyph.size.height / 2.0,
            shell.origin.y + shell.size.height / 2.0
        );
        assert_eq!(input.origin.x, glyph.origin.x + 14.0 + 8.0);
        // Without a query the text runs to the shell's padding; with one the clear chip takes the
        // trailing inset.
        assert_eq!(clear, None);
        // The trailing slot is reserved whether or not a query is active, so the placeholder does
        // not shift sideways when the clear chip appears.
        // Slot = the wider of the clear chip and the hint, plus the shell's own 8pt gap.
        let slot = search_trailing_slot_width(16.0);
        assert_eq!(
            input.origin.x + input.size.width,
            shell.origin.x + shell.size.width - 1.0 - 8.0 - slot - 8.0
        );
        let (_, input_with_clear, clear) = search_shell_frames(shell, true, 16.0);
        let clear = clear.expect("a query shows the clear chip");
        assert_eq!(input_with_clear, input);
        assert!(input_with_clear.origin.x + input_with_clear.size.width <= clear.origin.x);
        assert_eq!(
            clear.origin.x + clear.size.width,
            shell.origin.x + shell.size.width - 1.0 - 8.0
        );
    }

    /// The text sits on the shell's midline. The field's box used to be a fixed 22pt line centred in
    /// the shell, which put AppKit's line box high enough that the placeholder read as off-centre
    /// against the magnifier beside it; the box now spans the shell's inner height, so the line is
    /// centred on the box's own midline.
    #[test]
    fn the_search_text_is_centred_on_the_shell_not_hung_from_its_top() {
        let shell = sidebar_search_shell_frame(sidebar_box(600.0));
        let (glyph, input, _) = search_shell_frames(shell, false, 16.0);
        let shell_mid = shell.origin.y + shell.size.height / 2.0;

        assert_eq!(input.origin.y, shell.origin.y + 1.0);
        assert_eq!(input.size.height, shell.size.height - 2.0);
        assert_eq!(input.origin.y + input.size.height / 2.0, shell_mid);
        // The magnifier and the text box share that midline, so they cannot disagree on screen.
        assert_eq!(
            glyph.origin.y + glyph.size.height / 2.0,
            input.origin.y + input.size.height / 2.0
        );

        // The frame is necessary but NOT sufficient: a bare NSTextFieldCell draws its line from the
        // top of the drawing rect whatever the frame's height, which is what kept the placeholder
        // above the midline even after the box was made full height. The field therefore uses the
        // shared centered cell, and this asserts the geometry that cell centres -- one 22pt line box
        // on the midpoint of the field's inner height, so the text shares the magnifier's midline.
        let cell = super::super::widgets::centered_text_field_cell_frame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            input.size,
        ));
        assert_eq!(cell.size.height, 22.0);
        // The cell rect is in the field's own (origin 0) space, so compare against the field's
        // midpoint there rather than a window coordinate. The cell sits a small deliberate distance
        // BELOW the geometric midpoint: it also carries the shared +2pt baseline compensation for
        // the font drawing its glyphs above the line box's centre, which is exactly what lands the
        // visible text on the magnifier's midline.
        let field_mid = mid_of(input) - input.origin.y;
        assert!((mid_of(cell) - field_mid - 2.0).abs() < 0.5);
        // The cell's horizontal inset starts at the field's own leading edge (its local x = 0), so
        // the text can never slide back under the magnifier.
        assert!(cell.origin.x >= 0.0);
        assert!(cell.origin.x <= 8.0);
    }

    /// The vertical midpoint of a rect.
    fn mid_of(rect: NSRect) -> f64 {
        rect.origin.y + rect.size.height / 2.0
    }

    /// The trailing `⌘F` hint: bare ink in a box exactly one line tall, at the shell's right inner
    /// edge and centred on the shell's midline. Its width follows its label, and it never reaches
    /// the text. The height is the measured line box, not a fixed chip size: a taller box drew the
    /// chord high inside the field (measured after the border was removed).
    #[test]
    fn the_trailing_hint_sits_in_the_slot_the_clear_chip_takes_over() {
        let shell = sidebar_search_shell_frame(sidebar_box(600.0));
        let line_h = 12.0;
        let hint = search_hint_frame(shell, 16.0, line_h);
        let (_, _, clear) = search_shell_frames(shell, true, 16.0);
        let clear = clear.expect("a query shows the clear chip");

        assert_eq!(hint.size.height, line_h);
        assert_eq!(
            hint.origin.x + hint.size.width,
            clear.origin.x + clear.size.width
        );
        assert_eq!(
            hint.origin.y + hint.size.height / 2.0,
            shell.origin.y + shell.size.height / 2.0
        );
        // A label measured as zero still yields a square chip, and the field reserves at least the
        // clear chip's width either way, so the text never moves.
        assert_eq!(search_hint_frame(shell, 0.0, line_h).size.width, 18.0);
        assert_eq!(search_trailing_slot_width(0.0), 18.0);
        // A wider label widens both its chip and the reserved slot by the same amount, so the chip
        // can never grow left into the text.
        let wide_label = 40.0;
        let wide = search_hint_frame(shell, wide_label, line_h);
        let (_, input_wide, _) = search_shell_frames(shell, false, wide_label);
        assert!(wide.size.width > hint.size.width);
        assert_eq!(search_trailing_slot_width(wide_label), wide.size.width);
        assert!(wide.origin.x >= input_wide.origin.x + input_wide.size.width);
    }

    /// The query matching: trimmed, case-folded per character, matched against each field on its
    /// own (label, description, section header), in index order.
    #[test]
    fn the_search_matches_labels_descriptions_and_section_headers() {
        let index = vec![
            SearchEntry::row(0, 0, "Theme", Some("Follow the system"), "Appearance"),
            SearchEntry::row(1, 2, "Show minimized windows", None, "Windows"),
            SearchEntry::row(2, 5, "剪贴板历史", Some("记录复制内容"), "剪贴板"),
        ];
        assert_eq!(normalize_search_query("  ThEMe "), "theme");
        // A label, ignoring the query's case and padding.
        assert_eq!(match_settings_search(&index, "  THEME "), vec![0]);
        // A description.
        assert_eq!(match_settings_search(&index, "system"), vec![0]);
        // A section header.
        assert_eq!(match_settings_search(&index, "appearance"), vec![0]);
        // Every field of a row is indexed, so "minimized" matches its label.
        assert_eq!(match_settings_search(&index, "minimiz"), vec![1]);
        // A Chinese query and a Chinese label.
        assert_eq!(match_settings_search(&index, "剪贴"), vec![2]);
        // An empty query is "no search", not "everything".
        assert!(match_settings_search(&index, "   ").is_empty());
        // A query that spans two fields of one row does not match across them.
        assert!(match_settings_search(&index, "theme follow").is_empty());
    }

    fn test_section(header: &str, rows: usize) -> ResultSection {
        ResultSection {
            header: header.to_string(),
            rows: (0..rows)
                .map(|index| ResultRow {
                    entry: SearchEntry::row(0, index, "row", None, header),
                    frame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0)),
                    views: Vec::new(),
                    group_hidden: false,
                    content_hidden: Vec::new(),
                })
                .collect(),
        }
    }

    /// Result geometry: one chunk per card (header + gap + card with its rows and bottom inset), a
    /// block gap between chunks, and the empty state as a single caption line.
    #[test]
    fn the_results_panel_is_laid_out_one_page_chunk_per_section() {
        let empty = results_panel_height(&[]);
        let one = results_panel_height(&[test_section("Startup", 1)]);
        let two = results_panel_height(&[test_section("Startup", 2)]);
        assert!(one > 0.0 && empty > 0.0);
        // A second row in the same card adds exactly one settings row.
        assert!((two - one - SettingsLayout::SINGLE_LINE_ROW_H).abs() < 1e-9);
        // The empty state is a caption line, not a chunk.
        assert!((one - empty).abs() > 1.0);
        // A second chunk adds its header block and the block gap on top of one row's chunk.
        let two_chunks = results_panel_height(&[test_section("A", 1), test_section("B", 1)]);
        let header_block = results_header_height()
            + super::SETTINGS_SECTION_CARD_GAP
            + SettingsLayout::CARD_BOTTOM_INSET
            + SettingsLayout::SINGLE_LINE_ROW_H;
        assert!((two_chunks - one - results_section_gap() - header_block).abs() < 1e-9);
    }

    #[test]
    fn results_preserve_variable_row_heights() {
        let base = results_panel_height(&[test_section("Appearance", 1)]);
        let mut section = test_section("Appearance", 1);
        section.rows[0].frame.size.height = SettingsLayout::SINGLE_LINE_ROW_H + 80.0;
        assert_eq!(results_panel_height(&[section]) - base, 80.0);
    }

    #[test]
    fn matching_an_option_keeps_only_its_row_and_page_section() {
        let mut material = SearchEntry::row(0, 1, "Panel material", None, "Appearance");
        material.set_options(&["Liquid Glass", "Frosted Glass", "Opaque"]);
        let index = vec![
            SearchEntry::row(0, 0, "Theme", None, "Appearance"),
            material,
            SearchEntry::row(1, 0, "Another setting", None, "Appearance"),
        ];
        let matches: Vec<_> = match_settings_search(&index, "frosted")
            .into_iter()
            .map(|index_pos| index[index_pos].clone())
            .collect();
        let sections = group_result_entries(&matches);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].1.len(), 1);
        assert_eq!(sections[0].1[0].row, 1);
        assert_eq!(group_result_entries(&index).len(), 2);
    }

    /// Results are grouped by their card header in index order: consecutive rows under one card
    /// share a chunk, and a new header starts a new chunk.
    #[test]
    fn results_are_grouped_by_their_card_header() {
        let entries = vec![
            SearchEntry::row(0, 0, "Launch at login", None, "Startup"),
            SearchEntry::row(0, 1, "Hide menu bar icon", None, "Startup"),
            SearchEntry::row(1, 0, "Show minimized windows", None, "Windows"),
        ];
        let groups = group_result_entries(&entries);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].0, "Startup");
        assert_eq!(groups[0].1.len(), 2);
        assert_eq!(groups[1].0, "Windows");
        assert_eq!(groups[1].1.len(), 1);
    }

    /// A jump centers its row and stays inside the document's scroll range.
    #[test]
    fn the_jump_centers_its_row_inside_the_scroll_range() {
        // A point on the viewport's middle stays put.
        assert_eq!(centered_scroll_origin(500.0, 480.0, 2000.0), 260.0);
        // A point above the current band scrolls up, clamped at the document's bottom.
        assert_eq!(centered_scroll_origin(1500.0, 480.0, 2000.0), 1260.0);
        // A point past the document's top clamps to the last scrollable origin.
        assert_eq!(centered_scroll_origin(1900.0, 480.0, 2000.0), 1520.0);
        // A point near the document's BOTTOM scrolls DOWN to the start of the range; the old
        // formula clamped the negative offset to zero and left a lower row off screen.
        assert_eq!(centered_scroll_origin(100.0, 480.0, 2000.0), 0.0);
        // A document shorter than the viewport has one origin.
        assert_eq!(centered_scroll_origin(100.0, 480.0, 300.0), 0.0);
        // Never negative.
        assert_eq!(centered_scroll_origin(0.0, 480.0, 2000.0), 0.0);
    }

    /// The results panel hangs from the top of its document: the empty state and the page chunks
    /// The results panel hangs from the top of its document: the empty state and the summary line
    /// sit where a page's title does, not at the bottom of an otherwise empty pane. A list taller
    /// than the viewport grows the document and starts at its own top, scrolling from there.
    #[test]
    fn the_results_panel_is_anchored_to_the_document_top() {
        // Shorter than the viewport: the document is the viewport and the panel's top edge is the
        // document's own top edge.
        let (doc_h, panel_y) = results_document_geometry(168.0, 636.0);
        assert_eq!(doc_h, 636.0);
        assert_eq!(panel_y + 168.0, doc_h);
        // Taller than the viewport: the document grows with the panel and starts at its bottom.
        let (doc_h, panel_y) = results_document_geometry(900.0, 636.0);
        assert_eq!(doc_h, 900.0);
        assert_eq!(panel_y, 0.0);
        // An exactly-fitting panel is at the document's top too.
        let (doc_h, panel_y) = results_document_geometry(636.0, 636.0);
        assert_eq!((doc_h, panel_y), (636.0, 0.0));
    }

    /// The index spans every page: a page starting its own section scope keeps the rows of the
    /// pages built before it. The card header a page sets after drawing its rows applies to THOSE
    /// rows (the page closes the card when the header is drawn), and only to them.
    #[test]
    fn the_index_keeps_every_page_and_its_cards_header() {
        begin_settings_search_index();

        // Page 0: a row, then its card closes and names the section.
        record_settings_search_entry(SearchEntry::row(0, 0, "Launch at login", None, ""));
        apply_settings_search_section("Startup");
        // Page 1 starts: this may not clear page 0's rows, and must not inherit page 0's header.
        begin_settings_search_section();
        record_settings_search_entry(SearchEntry::row(1, 0, "Show minimized windows", None, ""));
        apply_settings_search_section("Windows");

        let index = settings_search_index();
        assert_eq!(
            index.len(),
            2,
            "a page must not wipe the rows recorded before it: {index:?}"
        );
        assert_eq!(match_settings_search(&index, "launch"), vec![0]);
        assert_eq!(match_settings_search(&index, "windows"), vec![1]);
        // The header set after the row was drawn belongs to that row.
        assert_eq!(match_settings_search(&index, "startup"), vec![0]);
        // ... and a query cannot match across two fields of one row.
        assert!(match_settings_search(&index, "windows windows").is_empty());

        // A dropdown row is searched by the choices it offers as well as by its own text: the
        // option list is attached to the row recorded just before it.
        set_search_options_for_last_row(&["Auto", "Light", "Dark"]);
        let index = settings_search_index();
        assert_eq!(match_settings_search(&index, "dark"), vec![1]);
        assert_eq!(match_settings_search(&index, "light"), vec![1]);
        // A choice that belongs to no row does not match anything.
        assert!(match_settings_search(&index, "glass").is_empty());

        // Live component text replaces its previous snapshot instead of accumulating stale values.
        super::extend_settings_search_row(1, 0, &["Full explanatory caption".to_string()]);
        assert_eq!(
            match_settings_search(&settings_search_index(), "explanatory"),
            vec![1]
        );
        super::extend_settings_search_row(1, 0, &["Updated caption".to_string()]);
        assert!(match_settings_search(&settings_search_index(), "explanatory").is_empty());
        assert_eq!(
            match_settings_search(&settings_search_index(), "updated"),
            vec![1]
        );

        // The sweep's blind-spot rows carry a settled header and are not re-labelled later.
        record_settings_search_entry_sectioned(SearchEntry::row(
            1,
            1,
            "Fallback row",
            None,
            "Windows",
        ));
        begin_settings_search_section();
        apply_settings_search_section("Appearance");
        let index = settings_search_index();
        assert_eq!(
            match_settings_search(&index, "appearance"),
            Vec::<usize>::new()
        );
        assert_eq!(match_settings_search(&index, "windows"), vec![1, 2]);

        begin_settings_search_index();
        assert!(settings_search_index().is_empty());
    }
}

/// The settings window is gone: its result rows are address-keyed views, and a click after the
/// teardown would message a freed pointer, so the rendered list is dropped with the window.
pub(super) fn reset_settings_search_rows() {
    BORROWED_RESULT_VIEWS.with(|slot| slot.borrow_mut().clear());
    *RENDERED_RESULTS.lock().unwrap() = Vec::new();
    unsafe { clear_search_flash() };
}

/// The field's placeholder, drawn in the muted ink.
///
/// A bare `placeholderString` is drawn by AppKit with its own translucent gray, which washes out on
/// the shell's field surface; an attributed placeholder lets the design system's own color speak.
unsafe fn apply_search_placeholder(field: *mut AnyObject, text: &str) {
    if field.is_null() {
        return;
    }
    let font: *mut AnyObject = msg_send![
        class!(NSFont),
        systemFontOfSize: crate::theme::FONT_CAPTION
    ];
    let color = settings_text_color(SettingsTextRole::Muted);
    let value = make_nsstring(text);
    let attributes: *mut AnyObject = msg_send![class!(NSMutableDictionary), alloc];
    let attributes: *mut AnyObject = msg_send![attributes, init];
    let font_key = make_nsstring("NSFont");
    let color_key = make_nsstring("NSColor");
    let _: () = msg_send![attributes, setObject: font, forKey: font_key];
    let _: () = msg_send![attributes, setObject: color, forKey: color_key];
    release_obj(font_key);
    release_obj(color_key);
    let attributed: *mut AnyObject = msg_send![class!(NSAttributedString), alloc];
    let attributed: *mut AnyObject =
        msg_send![attributed, initWithString: value, attributes: attributes];
    release_obj(value);
    release_obj(attributes);
    if !attributed.is_null() {
        let _: () = msg_send![field, setPlaceholderAttributedString: attributed];
        release_obj(attributed);
    }
}
