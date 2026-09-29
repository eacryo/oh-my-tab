//! One owner for a settings page's vertical layout.
//!
//! A page's rows, their separators, the section cards and headers, the page title, the restore
//! control and the document height all follow from a single list of rows. Page builders place
//! their rows through this canvas (`next_row`, `next_section`, `card`), so the canvas learns each
//! row's height and the views that belong to it without any call-site bookkeeping, and it is the
//! only code that moves them afterwards.
//!
//! Conditional rows form a [`RowGroup`]: hiding one skips those rows in the layout walk instead of
//! shifting views by hand, so the cards, the document height and the restore control stay correct
//! by construction -- one walk means they cannot drift apart from the rows.
//!
//! Everything here runs on the settings UI thread. The build phase owns `&mut self`; the runtime
//! phase (a switch toggling a group, the permission banner changing the viewport, the inline
//! update flow handing over its host row's height) goes through `&self` with `RefCell` state, so it
//! can run while a caller holds an immutable settings UI reference.

use super::*;
use std::cell::RefCell;

/// A block of rows that appears and disappears as a unit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum RowGroup {
    /// "Lines per tick" on the Mouse page (Line scroll mode only).
    LineCount,
    /// Pointer acceleration on the Mouse page (only while acceleration is off).
    PointerAccel,
    /// Focused prewarm + "app name on thumbnails" on the App Switcher page.
    ThumbnailOnly,
    /// "Clear the matching system-pasteboard entry" on the Clipboard page.
    ClipboardDeleteChild,
}

/// One laid-out row.
struct Row {
    /// The row's own views: its labels, controls, readouts and the separator above it. They move
    /// together; the canvas never needs to know their offsets inside the row.
    views: Vec<*mut AnyObject>,
    /// Vertical room the row consumes from the page cursor, gap included (`next_row` adds the
    /// shared row gap, `next_block` takes the caller's amount as-is).
    consume: f64,
    /// The row's own box height, i.e. `consume` minus the shared gap for a normal row and the same
    /// as `consume` for a block the page lays out itself.
    height: f64,
    group: Option<RowGroup>,
    /// The section this row belongs to: a new section consumes its heading step before the row.
    section: usize,
}

/// One section: the cursor its card hangs from, plus the header label drawn at that cursor.
struct Section {
    /// Relative to the first section cursor, which sits `header_offset` below the document top.
    rel: f64,
    header: *mut AnyObject,
    /// Extra points the page steps below the previous card (the About page steps from the card's
    /// bottom edge, 10pt below its last row).
    extra: f64,
}

/// One card, derived from its section's cursor down past its last row.
struct Card {
    card: *mut AnyObject,
    shadow: *mut AnyObject,
    section: usize,
    last_row: usize,
    /// Distance from its last row's origin to the card's bottom edge
    /// (`-SettingsLayout::card_bottom_inset` unless the caller places it itself).
    bottom_offset: f64,
}

/// The row whose views are still being appended to the document.
struct PendingRow {
    /// Document subview count when the row started; everything appended after belongs to it.
    mark: usize,
    consume: f64,
    height: f64,
    group: Option<RowGroup>,
    section: usize,
    /// Origin relative to the first section cursor.
    rel: f64,
}

/// Layout state the runtime mutates through `&self`.
struct CanvasState {
    /// Absolute origin (bottom edge; the document is not flipped) of every row. A row inside a
    /// hidden group keeps its last position, which is where its views still are.
    origins: Vec<f64>,
    hidden: Vec<RowGroup>,
    /// Views this canvas hid because their row's group is hidden, each with the hidden state it had
    /// before the canvas touched it. Re-showing the group restores exactly that, so a view whose
    /// owner hid it for its own reason (the inline update flow's check button and host) is never
    /// revealed by a re-flow.
    group_hidden_views: Vec<Option<Vec<(*mut AnyObject, bool)>>>,
    /// Consumption forced by a row's owner: the Mouse page's binding table as its bindings change,
    /// and the About page's inline update host as a flow phase changes its height.
    row_consumes: Vec<f64>,
    /// Absolute y of the lowest content from the last re-flow (0 until one has run).
    content_bottom: f64,
    /// The restore control: its container, the sibling surface that animates behind it, and the
    /// container's offset from the page's content bottom.
    restore: Option<(*mut AnyObject, *mut AnyObject, f64)>,
    /// Viewport height: a document is never shorter than its clip.
    clip_height: f64,
    /// Space kept below the lowest content (`SettingsPageHeader::BOTTOM_PADDING`), which the inline
    /// update flow temporarily lowers to its own card padding.
    bottom_padding: f64,
}

/// A page's layout. See the module docs.
pub(super) struct PageCanvas {
    document: *mut AnyObject,
    content_w: f64,
    layout: SettingsLayout,
    /// The page title label, pinned to the document's top edge, and its measured frame height.
    title: *mut AnyObject,
    title_height: f64,
    /// Distance from the document's top edge down to the first section cursor.
    header_offset: f64,
    rows: Vec<Row>,
    sections: Vec<Section>,
    cards: Vec<Card>,
    pending: Option<PendingRow>,
    /// Views the page pins to the document's top edge, with their distance from it: the About page
    /// draws its own header (app icon + name) instead of the shared page title.
    pinned: Vec<(*mut AnyObject, f64)>,
    /// The group the following rows belong to.
    group: Option<RowGroup>,
    /// Cursor the next row or section is placed from (the previous row's origin).
    cursor: f64,
    /// The first section cursor of the layout in progress.
    base: f64,
    state: RefCell<CanvasState>,
}

/// Each row's origin relative to the first section cursor.
struct RelativeLayout {
    origins: Vec<Option<f64>>,
    /// One entry per section, relative to the first section cursor.
    section_rels: Vec<f64>,
}

impl RelativeLayout {
    /// Walk the rows top-down, skipping the hidden groups. The walk is monotonic, so an empty or
    /// fully hidden page simply has no origins.
    #[cfg(test)]
    fn of(rows: &[Row], hidden: &[RowGroup], layout: SettingsLayout) -> Self {
        let consumes: Vec<f64> = rows.iter().map(|row| row.consume).collect();
        let sections = rows.iter().map(|row| row.section + 1).max().unwrap_or(1);
        Self::of_with(rows, &consumes, hidden, layout, sections, &[])
    }

    /// Same, with the live consumption of every row (`consumes[i]` replaces `rows[i].consume`):
    /// an owner like the mapping table can resize its row between re-flows.
    fn of_with(
        rows: &[Row],
        consumes: &[f64],
        hidden: &[RowGroup],
        layout: SettingsLayout,
        sections: usize,
        section_extras: &[f64],
    ) -> Self {
        let mut cursor = 0.0;
        let mut section = 0;
        let mut section_rels = vec![0.0];
        let mut origins = Vec::with_capacity(rows.len());
        for (index, row) in rows.iter().enumerate() {
            // A section heading step is consumed whether or not the section's rows are visible:
            // the section still exists, it just has fewer rows (and a hidden group never spans a
            // whole section in this UI).
            while section < row.section {
                cursor -=
                    layout.section_step + section_extras.get(section + 1).copied().unwrap_or(0.0);
                section += 1;
                section_rels.push(cursor);
            }
            if row.group.is_some_and(|group| hidden.contains(&group)) {
                origins.push(None);
                continue;
            }
            cursor -= consumes.get(index).copied().unwrap_or(row.consume);
            origins.push(Some(cursor));
        }
        // A trailing section without rows still hangs from the cursor (the last card's section).
        while section_rels.len() < sections {
            cursor -= layout.section_step
                + section_extras
                    .get(section_rels.len())
                    .copied()
                    .unwrap_or(0.0);
            section_rels.push(cursor);
        }
        Self {
            origins,
            section_rels,
        }
    }
}

unsafe fn subview_count(view: *mut AnyObject) -> usize {
    let subviews: *mut AnyObject = msg_send![view, subviews];
    msg_send![subviews, count]
}

unsafe fn subview_at(view: *mut AnyObject, index: usize) -> *mut AnyObject {
    let subviews: *mut AnyObject = msg_send![view, subviews];
    msg_send![subviews, objectAtIndex: index as isize]
}

/// Views appended to `view` since `mark`.
///
/// Only views added with the plain `addSubview:` belong to a row. The card/shadow pair is inserted
/// at the bottom of the z-order (`addSubview:positioned:relativeTo:`) and shifts the array indices,
/// so cards are always created right after a row has been captured (see `card`).
unsafe fn views_added_since(view: *mut AnyObject, mark: usize) -> Vec<*mut AnyObject> {
    let count = subview_count(view);
    if count <= mark {
        return Vec::new();
    }
    (mark..count).map(|index| subview_at(view, index)).collect()
}

impl PageCanvas {
    /// An unbound canvas (before the settings window is built).
    pub(super) const fn empty() -> Self {
        Self {
            document: std::ptr::null_mut(),
            content_w: 0.0,
            layout: SettingsLayout {
                label_x: 0.0,
                label_w: 0.0,
                control_x: 0.0,
                control_w: 0.0,
                row_h: 0.0,
                described_row_h: 0.0,
                section_step: 0.0,
                row_gap: 0.0,
                card_bottom_inset: 0.0,
                card_header_gap: 0.0,
            },
            title: std::ptr::null_mut(),
            title_height: 0.0,
            header_offset: 0.0,
            rows: Vec::new(),
            sections: Vec::new(),
            cards: Vec::new(),
            pending: None,
            pinned: Vec::new(),
            group: None,
            cursor: 0.0,
            base: 0.0,
            state: RefCell::new(CanvasState {
                origins: Vec::new(),
                hidden: Vec::new(),
                group_hidden_views: Vec::new(),
                row_consumes: Vec::new(),
                content_bottom: f64::NAN,
                restore: None,
                clip_height: 0.0,
                bottom_padding: SettingsPageHeader::BOTTOM_PADDING,
            }),
        }
    }

    /// Start a page whose header the page draws itself (the About page's app icon and name):
    /// `header_offset` is the distance from the document's top edge down to the first section
    /// cursor, and the header's own views are pinned through [`PageCanvas::pin_to_top`].
    pub(super) unsafe fn new_at(
        document: *mut AnyObject,
        content_w: f64,
        layout: SettingsLayout,
        header_offset: f64,
        doc_h: f64,
        clip_height: f64,
    ) -> Self {
        let base = doc_h - header_offset;
        Self {
            document,
            content_w,
            layout,
            title: std::ptr::null_mut(),
            title_height: 0.0,
            header_offset,
            rows: Vec::new(),
            sections: vec![Section {
                rel: 0.0,
                header: std::ptr::null_mut(),
                extra: 0.0,
            }],
            cards: Vec::new(),
            pending: None,
            pinned: Vec::new(),
            group: None,
            cursor: base,
            base,
            state: RefCell::new(CanvasState {
                origins: Vec::new(),
                hidden: Vec::new(),
                group_hidden_views: Vec::new(),
                row_consumes: Vec::new(),
                content_bottom: f64::NAN,
                restore: None,
                clip_height: clip_height.max(1.0),
                bottom_padding: SettingsPageHeader::BOTTOM_PADDING,
            }),
        }
    }

    /// Pin a view to the document's top edge (the About page's own header views): it moves with the
    /// document height.
    pub(super) unsafe fn pin_to_top(&mut self, view: *mut AnyObject) {
        if view.is_null() || !self.is_bound() {
            return;
        }
        let frame: NSRect = msg_send![view, frame];
        let offset = (self.base + self.header_offset) - frame.origin.y;
        self.pinned.push((view, offset));
    }

    /// The document's current subview count: pass it to [`PageCanvas::pin_from_mark`] to pin
    /// everything a page's own header block appends afterwards.
    pub(super) unsafe fn view_mark(&self) -> usize {
        subview_count(self.document)
    }

    /// Pin every view appended since `mark` to the document's top edge. Used by pages that draw
    /// their own header instead of the shared page title.
    pub(super) unsafe fn pin_from_mark(&mut self, mark: usize) {
        for view in views_added_since(self.document, mark) {
            self.pin_to_top(view);
        }
    }

    /// Open a section whose cursor the page computed itself: the About page steps its sections from
    /// the previous card's bottom edge, `extra` points below its last row.
    pub(super) unsafe fn next_section_with_offset(&mut self, extra: f64) -> f64 {
        self.cursor -= extra;
        self.next_section();
        if let Some(section) = self.sections.last_mut() {
            section.extra = extra;
        }
        self.cursor
    }

    /// Start a page: draw its title and take over the layout from `doc_h`, the provisional document
    /// height the page builder was given (the first re-flow settles the real one).
    pub(super) unsafe fn new(
        document: *mut AnyObject,
        content_w: f64,
        layout: SettingsLayout,
        title: &str,
        doc_h: f64,
        clip_height: f64,
    ) -> Self {
        let mark = subview_count(document);
        let (actual_doc_top, cursor) =
            SettingsPageHeader::attach(document, title, 6.0, doc_h, content_w);
        let title_view = views_added_since(document, mark)
            .first()
            .copied()
            .unwrap_or(std::ptr::null_mut());
        let title_height = if title_view.is_null() {
            0.0
        } else {
            let frame: NSRect = msg_send![title_view, frame];
            frame.size.height
        };
        // `SettingsPageHeader::attach` consumes the top padding, the measured title height, the
        // gap to the first section and the section heading box: that is the whole offset.
        // `attach` positions the title and cursor from the document's actual height. When the
        // viewport is taller than the provisional page height, using `doc_h` here shifts the
        // reflowed rows upward into the pinned title.
        let header_offset = actual_doc_top - cursor;
        Self {
            document,
            content_w,
            layout,
            title: title_view,
            title_height,
            header_offset,
            rows: Vec::new(),
            sections: vec![Section {
                rel: 0.0,
                header: std::ptr::null_mut(),
                extra: 0.0,
            }],
            cards: Vec::new(),
            pending: None,
            pinned: Vec::new(),
            group: None,
            cursor,
            base: cursor,
            state: RefCell::new(CanvasState {
                origins: Vec::new(),
                hidden: Vec::new(),
                group_hidden_views: Vec::new(),
                row_consumes: Vec::new(),
                content_bottom: f64::NAN,
                restore: None,
                clip_height: clip_height.max(1.0),
                bottom_padding: SettingsPageHeader::BOTTOM_PADDING,
            }),
        }
    }

    pub(super) fn is_bound(&self) -> bool {
        !self.document.is_null()
    }

    /// Open a section and return its cursor: the next card hangs from it and its header label is
    /// drawn on it.
    pub(super) unsafe fn next_section(&mut self) -> f64 {
        self.cursor -= self.layout.section_step;
        self.sections.push(Section {
            rel: self.cursor - self.base,
            header: std::ptr::null_mut(),
            extra: 0.0,
        });
        self.cursor
    }

    /// Open a row of `height` and return the y its views are created at.
    ///
    /// Everything the caller appends to the document before the next `next_row`, `card` or `finish`
    /// call belongs to this row.
    pub(super) unsafe fn next_row(&mut self, height: f64) -> f64 {
        self.advance(self.layout.row_gap + height, height)
    }

    /// Open a block of content the page lays out itself (the General page's switcher preview, and
    /// the rows that draw their own separator on the top edge instead of the shared gap). The
    /// canvas only needs the cursor it consumes, so it can re-place the block's views as one row.
    ///
    /// Returns the cursor after the block, which is also the block's origin.
    pub(super) unsafe fn next_block(&mut self, consume: f64) -> f64 {
        self.advance(consume, consume)
    }

    /// Consume `amount` of the page cursor and register the views appended after this call as one
    /// row that consumes exactly that much.
    unsafe fn advance(&mut self, amount: f64, height: f64) -> f64 {
        self.flush_pending();
        self.cursor -= amount;
        let origin = self.cursor;
        self.pending = Some(PendingRow {
            mark: subview_count(self.document),
            consume: amount,
            height,
            group: self.group,
            section: self.sections.len().saturating_sub(1),
            rel: origin - self.base,
        });
        origin
    }

    /// The group the following rows belong to, until [`PageCanvas::group_end`].
    pub(super) fn group_begin(&mut self, group: RowGroup) {
        self.group = Some(group);
    }

    pub(super) fn group_end(&mut self) {
        self.group = None;
    }

    /// Close the row whose views have just been appended to the document.
    ///
    /// The row's views are whatever was appended since `next_row` returned: its labels, controls,
    /// readouts and the separator above it. Cards are inserted at the bottom of the z-order instead
    /// of being appended, so they never land in a row (and every caller flushes before creating
    /// one).
    unsafe fn flush_pending(&mut self) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        let views = views_added_since(self.document, pending.mark);
        self.rows.push(Row {
            views,
            consume: pending.consume,
            height: pending.height,
            group: pending.group,
            section: pending.section,
        });
        {
            let mut state = self.state.borrow_mut();
            state.origins.push(self.base + pending.rel);
            state.row_consumes.push(pending.consume);
            state.group_hidden_views.push(None);
        }
    }

    /// Close the current section by drawing its card around the rows since the section opened.
    pub(super) unsafe fn card(&mut self, title: &str) -> SettingsCard {
        self.card_with_bottom_offset(title, -self.layout.card_bottom_inset)
    }

    /// Same, for a card whose bottom edge the caller places itself: `offset` is the distance from
    /// its last row's origin to the card's visible bottom (the General page's preview card hangs
    /// 12pt below the preview block rather than the shared 10pt inset).
    pub(super) unsafe fn card_with_bottom_offset(
        &mut self,
        title: &str,
        offset: f64,
    ) -> SettingsCard {
        self.flush_pending();
        let section = self.sections.len().saturating_sub(1);
        let last_row = self.rows.len().saturating_sub(1);
        let card_bottom = self.card_bottom_at(last_row, offset);
        let section_frame = NSRect::new(
            NSPoint::new(6.0, card_bottom),
            NSSize::new(
                self.content_w,
                self.layout.card_top(self.base + self.sections[section].rel) - card_bottom,
            ),
        );
        let card = SettingsSection::attach(self.document, section_frame, title);
        // `SettingsSection::attach` draws the header with a plain append and then inserts the card
        // and its shadow at the bottom of the z-order, so the header is the document's last view.
        let count = subview_count(self.document);
        if count > 0 {
            self.sections[section].header = subview_at(self.document, count - 1);
        }
        self.cards.push(Card {
            card: card.card,
            shadow: card.shadow,
            section,
            last_row,
            bottom_offset: offset,
        });
        card
    }

    /// The index of the row opened last, counting the row still being captured: the handle a block
    /// whose height its owner changes keeps (the Mouse page's binding table and the About page's
    /// inline update host both hand their height over through `set_row_consume`).
    pub(super) fn last_row(&self) -> usize {
        (self.rows.len() + usize::from(self.pending.is_some())).saturating_sub(1)
    }

    /// Resize a row the page registered with [`PageCanvas::next_block`]. The row's top edge stays
    /// put: a taller block grows downward, exactly like the mapping table.
    pub(super) unsafe fn set_row_consume(&self, row: usize, consume: f64) {
        if !self.is_bound() {
            return;
        }
        {
            let mut state = self.state.borrow_mut();
            match state.row_consumes.get_mut(row) {
                Some(entry) => *entry = consume.max(0.0),
                None => return,
            }
        }
        self.reflow();
    }

    /// Finish the page: settle the layout and return the content bottom the caller places its
    /// restore control from.
    pub(super) unsafe fn finish(&mut self) -> f64 {
        self.flush_pending();
        self.reflow();
        self.content_bottom()
    }

    /// The absolute y of the page's lowest content: the card bottoms and the rows a page registers
    /// itself (the Mouse page's binding table), whichever is lower. This is the value page builders
    /// return for the restore control to hang from. Falls back to the last card's bottom before the
    /// first re-flow, which `finish` always runs.
    unsafe fn content_bottom(&self) -> f64 {
        let state = self.state.borrow();
        if state.content_bottom.is_finite() {
            return state.content_bottom;
        }
        match self.cards.last().and_then(|card| {
            state
                .origins
                .get(card.last_row)
                .copied()
                .map(|origin| origin + card.bottom_offset)
        }) {
            Some(bottom) => bottom,
            None => self.cursor,
        }
    }

    /// The absolute card bottom for a row, using the origins recorded at build time.
    fn card_bottom_at(&self, row: usize, offset: f64) -> f64 {
        let origin = self
            .state
            .borrow()
            .origins
            .get(row)
            .copied()
            .unwrap_or(self.cursor);
        origin + offset
    }

    /// Hand the page's restore control to the canvas so it follows the content bottom.
    ///
    /// The container and its surface are both direct children of the document (the surface is a
    /// sibling, not a child of the container), so both have to travel with the page. The surface
    /// follows the container's *actual* movement, which preserves whatever relative position the
    /// collapsed/expanded animation left it in.
    pub(super) unsafe fn attach_restore(&self, container: *mut AnyObject, surface: *mut AnyObject) {
        if container.is_null() || !self.is_bound() {
            return;
        }
        let frame: NSRect = msg_send![container, frame];
        let offset = frame.origin.y - self.content_bottom();
        self.state.borrow_mut().restore = Some((container, surface, offset));
        self.reflow();
    }

    /// Show or hide a conditional group and re-lay out the page.
    pub(super) unsafe fn set_group_visible(&self, group: RowGroup, visible: bool) {
        if !self.is_bound() {
            return;
        }
        {
            let mut state = self.state.borrow_mut();
            let hidden = state.hidden.iter().position(|&hidden| hidden == group);
            match (visible, hidden) {
                (true, Some(index)) => {
                    state.hidden.remove(index);
                }
                (false, None) => state.hidden.push(group),
                _ => return,
            }
        }
        self.reflow();
    }

    /// The permission banner changes the page viewport, and a document may have to grow with it.
    pub(super) unsafe fn set_viewport(&self, clip_height: f64) {
        if !self.is_bound() {
            return;
        }
        let clip_height = clip_height.max(1.0);
        {
            let mut state = self.state.borrow_mut();
            if (clip_height - state.clip_height).abs() <= 0.5 {
                return;
            }
            state.clip_height = clip_height;
        }
        self.reflow();
    }

    /// The page's bottom padding while the inline update flow runs is its own card padding.
    pub(super) unsafe fn set_bottom_padding(&self, padding: f64) {
        if !self.is_bound() {
            return;
        }
        {
            let mut state = self.state.borrow_mut();
            if (padding - state.bottom_padding).abs() <= 0.5 {
                return;
            }
            state.bottom_padding = padding;
        }
        self.reflow();
    }

    /// Re-place every row, card, header, the title, the restore control and the document itself.
    ///
    /// Idempotent: positions come from the walk rather than from a recorded delta, so calling it
    /// twice (or after AppKit re-placed the subviews while displaying the window) changes nothing.
    pub(super) unsafe fn reflow(&self) {
        if !self.is_bound() {
            return;
        }
        let (hidden, row_consumes, restore, bottom_padding, clip_height, previous) = {
            let state = self.state.borrow();
            (
                state.hidden.clone(),
                state.row_consumes.clone(),
                state.restore,
                state.bottom_padding,
                state.clip_height,
                state.origins.clone(),
            )
        };
        let layout = self.layout;
        let extras: Vec<f64> = self.sections.iter().map(|section| section.extra).collect();
        let relative = RelativeLayout::of_with(
            &self.rows,
            &row_consumes,
            &hidden,
            layout,
            self.sections.len(),
            &extras,
        );
        // A card spans from its section's cursor down past its last *visible* row: a hidden group
        // inside the card shrinks the card instead of leaving it behind (a card whose rows are all
        // hidden collapses onto its own top edge). `card_row_rel[i]` is that row's origin.
        let mut card_row_rel: Vec<Option<f64>> = Vec::with_capacity(self.cards.len());
        let mut first_row = 0usize;
        for card in &self.cards {
            let mut visible = None;
            for index in (first_row..=card.last_row).rev() {
                if let Some(Some(rel)) = relative.origins.get(index) {
                    visible = Some(*rel);
                    break;
                }
            }
            card_row_rel.push(visible);
            first_row = card.last_row.saturating_add(1);
        }

        // Every card's bottom edge, in one place: derived from its last visible row -- a row the
        // page registered itself counts as one, so the inline update card follows the host's bottom
        // edge exactly like every other card follows its last row. Used for the card frames, the
        // content bottom and the document height.
        let card_bottom_rel: Vec<Option<f64>> = self
            .cards
            .iter()
            .enumerate()
            .map(|(index, card)| card_row_rel[index].map(|rel| rel + card.bottom_offset))
            .collect();

        // The lowest content: a card's shadow hangs `SETTINGS_CARD_SHADOW_INSET` below the card, a
        // row the page registered itself ends at its origin (the Mouse page's binding table), and
        // the restore control sits below everything.
        let content_bottom_rel = relative
            .origins
            .iter()
            .flatten()
            .copied()
            .chain(card_bottom_rel.iter().flatten().copied())
            .fold(f64::INFINITY, f64::min);
        let content_bottom_rel = if content_bottom_rel.is_finite() {
            content_bottom_rel
        } else {
            0.0
        };
        let mut lowest = content_bottom_rel;
        for rel in card_bottom_rel.iter().flatten() {
            let rect = widgets::settings_card_rect(NSRect::new(
                NSPoint::new(0.0, *rel),
                NSSize::new(0.0, 0.0),
            ));
            lowest = lowest.min(rect.origin.y - widgets::SETTINGS_CARD_SHADOW_INSET);
        }
        if let Some((container, _, offset)) = restore {
            // A hidden restore control does not count: the inline update flow hides it and owns the
            // page's bottom padding while it runs.
            let hidden: bool = msg_send![container, isHidden];
            if !hidden {
                lowest = lowest.min(content_bottom_rel + offset);
            }
        }
        // A card has to cover every row that is visible inside it. A card span that ignored its
        // visible rows was the 2026-09-28 report: the Mouse page's device card ended at its hidden
        // last row, so its visible rows and separators sat outside the card.
        #[cfg(debug_assertions)]
        {
            let mut first_row = 0usize;
            for (index, card) in self.cards.iter().enumerate() {
                let Some(bottom_rel) = card_bottom_rel[index] else {
                    first_row = card.last_row.saturating_add(1);
                    continue;
                };
                let top_rel = relative
                    .section_rels
                    .get(card.section)
                    .copied()
                    .unwrap_or(0.0)
                    - layout.card_header_gap;
                for row_index in first_row..=card.last_row {
                    let Some(Some(origin)) = relative.origins.get(row_index) else {
                        continue;
                    };
                    let height = self.rows[row_index].height;
                    // A row's box may reach the section cursor, which sits `card_header_gap` above
                    // the card's top edge; its bottom must stay inside the card.
                    let top_limit = top_rel + layout.card_header_gap + 0.5;
                    if !(origin + height <= top_limit && *origin >= bottom_rel - 0.5) {
                        log_info!(
                            "[settings] card {index} does not cover row {row_index}: card=[{bottom_rel:.1},{top_rel:.1}] row=[{origin:.1},{:.1}]",
                            origin + height
                        );
                    }
                    debug_assert!(
                        origin + height <= top_limit && *origin >= bottom_rel - 0.5,
                        "card {index} does not cover row {row_index}"
                    );
                }
                first_row = card.last_row.saturating_add(1);
            }
        }
        let doc_height = (self.header_offset - lowest + bottom_padding).max(clip_height);
        let base = doc_height - self.header_offset;

        // Rows: move the visible ones to their place. Visibility is the owner's business except for a
        // group this canvas hides: those views are hidden here (remembering each one's own state) and
        // restored to exactly that state when the group comes back. A visible row is never touched, so
        // a view its owner hid -- the inline update flow hides the check button and the host while the
        // update content replaces them -- stays hidden.
        for (index, row) in self.rows.iter().enumerate() {
            let target = relative
                .origins
                .get(index)
                .copied()
                .flatten()
                .map(|rel| base + rel);
            match target {
                Some(target) => {
                    let saved = {
                        let mut state = self.state.borrow_mut();
                        state
                            .group_hidden_views
                            .get_mut(index)
                            .and_then(|slot| slot.take())
                    };
                    if let Some(saved) = saved {
                        for (view, was_hidden) in saved {
                            if !view.is_null() {
                                let _: () = msg_send![view, setHidden: was_hidden];
                            }
                        }
                    }
                    let delta = target - previous.get(index).copied().unwrap_or(target);
                    if delta != 0.0 {
                        for &view in &row.views {
                            if view.is_null() {
                                continue;
                            }
                            let frame: NSRect = msg_send![view, frame];
                            let _: () = msg_send![
                                view,
                                setFrame: NSRect::new(
                                    NSPoint::new(frame.origin.x, frame.origin.y + delta),
                                    frame.size,
                                )
                            ];
                        }
                    }
                }
                None => {
                    let already_hidden = self
                        .state
                        .borrow()
                        .group_hidden_views
                        .get(index)
                        .map(|slot| slot.is_some())
                        .unwrap_or(false);
                    if already_hidden {
                        continue;
                    }
                    let mut saved = Vec::with_capacity(row.views.len());
                    for &view in &row.views {
                        if view.is_null() {
                            continue;
                        }
                        let was_hidden: bool = msg_send![view, isHidden];
                        saved.push((view, was_hidden));
                        let _: () = msg_send![view, setHidden: true];
                    }
                    let mut state = self.state.borrow_mut();
                    if let Some(slot) = state.group_hidden_views.get_mut(index) {
                        *slot = Some(saved);
                    }
                }
            }
        }

        // Section headers are drawn with their bottom edge on their section's cursor. The cursor
        // comes from the walk, so a hidden group above them pulls them up with the rows.
        for (index, section) in self.sections.iter().enumerate() {
            if section.header.is_null() {
                continue;
            }
            let rel = relative
                .section_rels
                .get(index)
                .copied()
                .unwrap_or(section.rel);
            let frame: NSRect = msg_send![section.header, frame];
            let _: () = msg_send![
                section.header,
                setFrameOrigin: NSPoint::new(frame.origin.x, base + rel)
            ];
        }

        // Cards: from their section's cursor down past their last *visible* row (a row the page
        // registered itself counts as one); a card with no visible row collapses onto its top edge.
        for (index, card) in self.cards.iter().enumerate() {
            if card.card.is_null() {
                continue;
            }
            let frame: NSRect = msg_send![card.card, frame];
            let top = base
                + relative
                    .section_rels
                    .get(card.section)
                    .copied()
                    .unwrap_or(self.sections[card.section].rel)
                - layout.card_header_gap;
            let bottom = match card_bottom_rel.get(index).copied().flatten() {
                Some(rel) => base + rel,
                // Every row of this card is hidden: it collapses onto its own top edge, and the
                // section heading stays.
                None => top,
            };
            let rect = widgets::settings_card_rect(NSRect::new(
                NSPoint::new(frame.origin.x, bottom),
                NSSize::new(frame.size.width, (top - bottom).max(1.0)),
            ));
            let _: () = msg_send![card.card, setFrame: rect];
            if !card.shadow.is_null() {
                let inset = widgets::SETTINGS_CARD_SHADOW_INSET;
                let _: () = msg_send![
                    card.shadow,
                    setFrame: NSRect::new(
                        NSPoint::new(rect.origin.x - inset, rect.origin.y - inset),
                        NSSize::new(
                            rect.size.width + inset * 2.0,
                            rect.size.height + inset * 2.0
                        )
                    )
                ];
            }
        }

        // The page title hangs from the document's top edge, and so do the views a page pinned
        // there itself (the About page's own header).
        if !self.title.is_null() {
            let frame: NSRect = msg_send![self.title, frame];
            // The measured height of the label itself, so the title always sits exactly
            // `TOP_PADDING` below the document's top edge and the first heading keeps its gap.
            let _: () = msg_send![
                self.title,
                setFrameOrigin: NSPoint::new(
                    frame.origin.x,
                    doc_height - SettingsPageHeader::TOP_PADDING - self.title_height
                )
            ];
        }
        for (view, offset) in &self.pinned {
            if view.is_null() {
                continue;
            }
            let frame: NSRect = msg_send![*view, frame];
            let _: () = msg_send![
                *view,
                setFrameOrigin: NSPoint::new(frame.origin.x, doc_height - offset)
            ];
        }

        // The restore control follows the content bottom.
        if let Some((container, surface, offset)) = restore {
            if !container.is_null() {
                let frame: NSRect = msg_send![container, frame];
                let target = base + content_bottom_rel + offset;
                let delta = target - frame.origin.y;
                if delta != 0.0 {
                    let _: () = msg_send![
                        container,
                        setFrameOrigin: NSPoint::new(frame.origin.x, target)
                    ];
                    // The surface is a sibling in the document rather than a child of the container,
                    // so it has to travel by the same delta to stay aligned with it.
                    if !surface.is_null() {
                        let surface_frame: NSRect = msg_send![surface, frame];
                        let _: () = msg_send![
                            surface,
                            setFrameOrigin: NSPoint::new(
                                surface_frame.origin.x,
                                surface_frame.origin.y + delta
                            )
                        ];
                    }
                }
            }
        }

        let current: NSRect = msg_send![self.document, frame];
        if (current.size.height - doc_height).abs() > 0.5 {
            let _: () = msg_send![
                self.document,
                setFrame: NSRect::new(
                    current.origin,
                    NSSize::new(current.size.width, doc_height)
                )
            ];
            let scroll: *mut AnyObject = msg_send![self.document, enclosingScrollView];
            if !scroll.is_null() {
                let clip: *mut AnyObject = msg_send![scroll, contentView];
                if !clip.is_null() {
                    let _: () = msg_send![scroll, reflectScrolledClipView: clip];
                }
            }
        }

        // A hidden row keeps its last position: that is still where its views are, so re-showing it
        // shifts them by the right delta.
        let mut state = self.state.borrow_mut();
        state.content_bottom = base + content_bottom_rel;
        state.origins = relative
            .origins
            .iter()
            .enumerate()
            .map(|(index, rel)| match rel {
                Some(rel) => base + rel,
                None => previous.get(index).copied().unwrap_or(base),
            })
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A normal row: `SettingsLayout::next_row_cursor` consumes the shared gap plus the row's own
    /// height.
    fn row(height: f64, group: Option<RowGroup>) -> Row {
        let layout = SettingsLayout::new(400.0);
        Row {
            views: Vec::new(),
            consume: layout.row_gap + height,
            height,
            group,
            section: 0,
        }
    }

    fn row_in(height: f64, group: Option<RowGroup>, section: usize) -> Row {
        Row {
            views: Vec::new(),
            consume: SettingsLayout::new(400.0).row_gap + height,
            height,
            group,
            section,
        }
    }

    #[test]
    fn a_new_section_consumes_its_heading_step() {
        let layout = SettingsLayout::new(400.0);
        let rows = [
            row(layout.described_row_h, None),
            row_in(layout.described_row_h, None, 1),
        ];
        let walk = RelativeLayout::of(&rows, &[], layout);
        assert_eq!(
            walk.origins[0],
            Some(-(layout.row_gap + layout.described_row_h))
        );
        assert_eq!(
            walk.origins[1],
            Some(
                -(layout.row_gap + layout.described_row_h)
                    - layout.section_step
                    - (layout.row_gap + layout.described_row_h)
            )
        );
    }

    #[test]
    fn hidden_groups_are_skipped_without_moving_the_rows_above_them() {
        let layout = SettingsLayout::new(400.0);
        let rows = [
            row(layout.described_row_h, None),
            row(layout.described_row_h, Some(RowGroup::LineCount)),
            row(layout.described_row_h, None),
        ];
        let shown = RelativeLayout::of(&rows, &[], layout);
        let hidden = RelativeLayout::of(&rows, &[RowGroup::LineCount], layout);
        let step = layout.row_gap + layout.described_row_h;
        // Row 0 is untouched by the group below it.
        assert_eq!(shown.origins[0], hidden.origins[0]);
        assert_eq!(shown.origins[0], Some(-step));
        // Row 1 disappears and row 2 closes its gap exactly.
        assert_eq!(hidden.origins[1], None);
        assert_eq!(shown.origins[2], Some(-3.0 * step));
        assert_eq!(hidden.origins[2], Some(-2.0 * step));
        assert_eq!(hidden.origins[2].unwrap() - shown.origins[2].unwrap(), step);
    }

    #[test]
    fn hiding_a_group_never_moves_the_document_top() {
        // The walk is anchored to the first section cursor (0.0) rather than to the document's
        // bottom, which is what kept the page from shifting when a block collapsed.
        let layout = SettingsLayout::new(400.0);
        let rows = [
            row(layout.described_row_h, Some(RowGroup::PointerAccel)),
            row(layout.described_row_h, None),
        ];
        let shown = RelativeLayout::of(&rows, &[], layout);
        let hidden = RelativeLayout::of(&rows, &[RowGroup::PointerAccel], layout);
        assert!(shown.origins[0].unwrap() > shown.origins[1].unwrap());
        assert!(hidden.origins[0].is_none());
        assert_eq!(
            hidden.origins[1],
            Some(-(layout.row_gap + layout.described_row_h))
        );
    }

    #[test]
    fn every_group_is_independent() {
        let layout = SettingsLayout::new(400.0);
        let rows = [
            row(layout.described_row_h, Some(RowGroup::ThumbnailOnly)),
            row(layout.described_row_h, Some(RowGroup::ThumbnailOnly)),
            row(layout.described_row_h, Some(RowGroup::ClipboardDeleteChild)),
        ];
        let hidden = RelativeLayout::of(&rows, &[RowGroup::ClipboardDeleteChild], layout);
        let step = layout.row_gap + layout.described_row_h;
        assert_eq!(hidden.origins[0], Some(-step));
        assert_eq!(hidden.origins[1], Some(-2.0 * step));
        assert_eq!(hidden.origins[2], None);
    }
}
