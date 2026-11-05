//! How big each row is and where its boxes sit: the measuring half of the layout.
//!
//! `PageMetrics` answers what a row needs; `BoxRect`/`CardRect`/`BannerRect` say where a box
//! lands. Nothing here decides the ORDER of the rows -- that is `place`.

use super::super::*;
use super::*;

mod place;

pub(in crate::settings) use place::canvas_row_of_described_row;
#[cfg(test)]
pub(in crate::settings::page_view) use place::page_sequence;
pub(in crate::settings::page_view) use place::{canvas_rows, layout_rows, PageLayout, PlacedRow};

#[derive(Clone, Debug, Default)]
pub(in crate::settings) struct LayoutInput {
    pub(in crate::settings::page_view) content_w: f64,
    /// The page document's top edge (the height it was provisioned with); the rows are laid out
    /// from this, since they are subviews of the document.
    pub(in crate::settings::page_view) doc_top: f64,
    /// The document's top edge down to the first section cursor, as the page header measured it
    /// (the canvas owns the header block; this is only its measurement).
    pub(in crate::settings::page_view) header_offset: f64,
    /// The banner strip's frame in its FINAL parent (the settings content view), as the caller
    /// measured it: the detail pane's inset and the visible viewport's top. The renderer and the
    /// window's banner refresh both take this box, so the two cannot disagree.
    pub(in crate::settings::page_view) banner_frame: BannerFrame,
    pub(in crate::settings::page_view) layout: SettingsLayout,
    pub(in crate::settings::page_view) text: MeasuredText,
    /// The heights of the page's own rows, resolved from the page input by the caller. A
    /// measured row cannot be resolved here: this type has no `PageInput`, so the page build
    /// resolves them once and the layout reads them.
    pub(in crate::settings::page_view) extra_row_heights: Vec<f64>,
}

/// Text metrics a page's rows are placed with, measured once by the caller.
///
/// Only the described-row subtitle is measured today; a row whose copy wraps (the captioned and
/// stacked rows) already has its height measured by the widget that builds it, and the caller
/// hands that height over as a page constant.
#[derive(Clone, Debug, Default)]
pub(in crate::settings) struct MeasuredText {
    /// Height of a described row's one-line subtitle in the current language.
    #[allow(dead_code)]
    pub(in crate::settings::page_view) subtitle_h: f64,
    /// Height of a section heading's label box.
    pub(in crate::settings::page_view) heading_h: f64,
    /// The one-line title height of a row's copy block.
    pub(in crate::settings::page_view) title_h: f64,
    /// Gap between a row's title and its caption.
    pub(in crate::settings::page_view) title_caption_gap: f64,
    /// Smallest caption height (one line of the description face).
    pub(in crate::settings::page_view) caption_one_line_h: f64,
    /// Each row's wrapped subtitle height, in row order. A row with no subtitle has none here.
    pub(in crate::settings::page_view) subtitle_heights: Vec<f64>,
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
pub(in crate::settings) fn captioned_text_width(
    layout: SettingsLayout,
    content_w: f64,
    row: &RowSpec,
) -> f64 {
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
    pub(in crate::settings) fn base() -> Self {
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
    pub(in crate::settings) fn measure(
        page: &PageView,
        layout: SettingsLayout,
        content_w: f64,
    ) -> Self {
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

/// Page constants the row placement is stated in terms of.
///
/// They are the page's own copy of the design system's numbers, handed in rather than read, so the
/// placement of a row is a function of its inputs only.
#[derive(Clone, Copy, Debug, Default)]
pub(in crate::settings) struct PageMetrics {
    /// Vertical gap a row adds below itself (`PageCanvas::next_row`'s gap term).
    pub(in crate::settings::page_view) row_gap: f64,
    /// Distance from one section cursor to the next (a card boundary's step).
    pub(in crate::settings::page_view) section_step: f64,
    /// The label column's inset inside a card.
    pub(in crate::settings::page_view) label_x: f64,
    /// Control height (the switch and popup controls).
    pub(in crate::settings::page_view) control_h: f64,
    /// Width of a slider's trailing readout, and the gap before it.
    pub(in crate::settings::page_view) readout_w: f64,
    pub(in crate::settings::page_view) readout_gap: f64,
    pub(in crate::settings::page_view) readout_h: f64,
    /// Left inset of a card and of its section heading.
    pub(in crate::settings::page_view) card_inset: f64,
    /// A switch's drawn width and height.
    pub(in crate::settings::page_view) switch_w: f64,
    pub(in crate::settings::page_view) switch_h: f64,
    /// A row action button's width and height.
    pub(in crate::settings::page_view) action_btn_w: f64,
    pub(in crate::settings::page_view) action_btn_h: f64,
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
    pub(in crate::settings) fn for_layout(layout: SettingsLayout) -> Self {
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
pub(in crate::settings) struct BoxRect {
    pub(in crate::settings::page_view) x: f64,
    pub(in crate::settings::page_view) y: f64,
    pub(in crate::settings::page_view) w: f64,
    pub(in crate::settings::page_view) h: f64,
}

impl BoxRect {
    pub(in crate::settings) fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    pub(in crate::settings) fn rect(&self) -> NSRect {
        NSRect::new(NSPoint::new(self.x, self.y), NSSize::new(self.w, self.h))
    }

    pub(in crate::settings) fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }
}

/// A card the page draws: the heading and the rounded surface around one run of rows.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::settings) struct CardRect {
    /// Whether this card is the page's update-flow card: the About Updates card, whose handle the
    /// inline update flow reads. A card is not a control, so this is the one property the renderer
    /// has to know about it.
    pub(in crate::settings::page_view) holds_update_flow: bool,
    /// The card's own index in the page (`RowSpec::card` / `PageRowSpec::card`). The walk uses
    /// THIS, not the card's position in the vector: a page may have a card that holds only rows the
    /// page draws itself, and a positional index would then address the wrong card.
    pub(in crate::settings::page_view) index: usize,
    pub(in crate::settings::page_view) section: &'static str,
    /// The card surface's frame in the document.
    pub(in crate::settings::page_view) frame: BoxRect,
    /// The section heading's frame, above the card's top edge.
    pub(in crate::settings::page_view) header: BoxRect,
}

/// The permission banner a page draws above its rows.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::settings) struct BannerRect {
    pub(in crate::settings::page_view) strip: BoxRect,
    pub(in crate::settings::page_view) label: BoxRect,
    pub(in crate::settings::page_view) button: BoxRect,
}
