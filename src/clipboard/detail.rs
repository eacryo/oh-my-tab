//! Clipboard subsystem · detail: entry detail scroll view.

use super::*;
use crate::callback_guard;

/// Whether the pinned picker has lost key status and needs to counter Liquid Glass's passive look.
static PICKER_GLASS_INACTIVE: AtomicBool = AtomicBool::new(false);

fn picker_inactive_compensation_alpha() -> u32 {
    if PICKER_GLASS_INACTIVE.load(Ordering::SeqCst) {
        crate::glass::INACTIVE_GLASS_COMPENSATION_ALPHA
    } else {
        0
    }
}

/// Follow the picker's key state while pinned. Liquid Glass chooses a passive appearance after the
/// panel resigns key; the compensation is only present for that interval and never changes the
/// material while the picker is active.
pub(crate) unsafe fn set_picker_glass_inactive(inactive: bool) {
    if PICKER_GLASS_INACTIVE.swap(inactive, Ordering::SeqCst) != inactive {
        apply_glass_properties();
    }
}

/// Build the detail body: one scrolling column inset by the mock's pane padding, holding the
/// text view and, after it where the body section ends, the meta table.
///
/// `w`/`h` are the panel's size; `section_h` is the body's own height (what the preview needs),
/// which the caller already clamped to what the pane leaves under the meta table. `style` picks
/// the ink (link colour) and `kind` the layout (code); `style` wins for a link only, so the two
/// agree for every entry except that a degenerate image falls back to plain text.
#[allow(clippy::too_many_arguments)] // one call site, and the parts are all distinct layout facts
pub(super) unsafe fn add_detail_text(
    content: *mut AnyObject,
    text: &str,
    w: f64,
    h: f64,
    section_h: f64,
    entry: &ClipEntry,
    style: TextKind,
    prepared_code: Option<&PreparedCodeDisplay>,
    code_soft_wrap: bool,
    kind: TextKind,
) {
    let is_code = kind == TextKind::Code;
    debug_assert!(!code_soft_wrap || (is_code && prepared_code.is_some()));
    // The body is laid out on its own and the panel is sized around it (the caller clamps it to
    // what the pane leaves for it), so a measured body keeps every line it needs.
    let no_wrap = is_code && !code_soft_wrap;
    let mut pane = detail_pane_layout_with(
        detail_insets(picker_shows_detail_in_pane(), w),
        w,
        h,
        meta_table_field_count(),
    );
    if no_wrap {
        pane.scroll.size.height = (pane.scroll.size.height - SCROLL_INDICATOR_HIT_W - 6.0).max(0.0);
        pane.body_h = (pane.scroll.size.height - pane.meta.height).max(0.0);
    }
    let body_h = section_h.max(DETAIL_LINE_H);
    // The scroll view fills the pane; its insets are the mock's padding, so the content column
    // starts 10pt below the panel top and ends 16pt above its bottom.
    let scroll_w = pane.scroll.size.width;
    let scroll_h = pane.scroll.size.height;
    let scroll: *mut AnyObject = msg_send![class!(NSScrollView), alloc];
    let scroll: *mut AnyObject = msg_send![
        scroll,
        initWithFrame: NSRect::new(
            pane.scroll.origin,
            NSSize::new(scroll_w, scroll_h)
        )
    ];
    let _: () = msg_send![scroll, setBorderType: 0u64]; // NSNoBorder
    let clear_background: *mut AnyObject = msg_send![class!(NSColor), clearColor];
    let _: () = msg_send![scroll, setBackgroundColor: clear_background];
    let _: () = msg_send![scroll, setDrawsBackground: false];
    // The pane padding is the scroll view's inset: the column starts 10pt below the panel top
    // and ends 16pt above its bottom, as the mock's `.detail-pane` padding says.
    apply_scroll_pane_insets(scroll);
    // Use AppKit Overlay scrollers, which reserve no horizontal/vertical corner. NSScrollView
    // owns endpoints, dragging, and wheel phases; NSClipView also stops drawing its background
    // to avoid the default white corner.
    // Disable native scrollers so they cannot reappear during scrolling/layout and overlap the
    // always-visible custom capsules.
    let _: () = msg_send![scroll, setHasVerticalScroller: false];
    let _: () = msg_send![scroll, setHasHorizontalScroller: false];
    let _: () = msg_send![scroll, setAutohidesScrollers: true];
    let _: () = msg_send![scroll, setScrollerStyle: 1isize]; // NSScrollerStyleOverlay
    let clip_view: *mut AnyObject = msg_send![scroll, contentView];
    if !clip_view.is_null() {
        let _: () = msg_send![clip_view, setDrawsBackground: false];
    }
    // Endpoint rubber banding is left entirely to native elasticity (Automatic: bounces
    // only on axes whose content overflows). Never "disable" it by hard-clamping
    // out-of-range origins inside the bounds notification -- rewriting the clip view
    // mid-gesture poisons NSScrollView's momentum base, so following momentum events
    // reapply their deltas from the stale base and fight the clamp, making the scrollbar
    // twitch at the endpoints (see detail_scroll_indicator_bounds_changed).
    let _: () = msg_send![scroll, setVerticalScrollElasticity: 0isize]; // NSScrollElasticityAutomatic
    let _: () = msg_send![scroll, setHorizontalScrollElasticity: 0isize];

    // With soft wrap off, preserve raw line width and enable horizontal scrolling. When on,
    // use the U+2028 display model and hide the horizontal scroller.
    // The open path has already prepared the soft-wrap model; borrow its text and mapping here
    // without copying the long source.
    let prepared = prepared_code;
    let display_text = prepared.map(|code| code.text.as_str()).unwrap_or(text);
    // Custom soft wrapping inserts line separators only into display text. Share the cached
    // source map so copied selections omit those display characters without cloning the long
    // source or boundary array.
    *DETAIL_SOURCE_MAP.lock().unwrap() = prepared.and_then(|code| code.source_map.clone());
    let tv: *mut AnyObject = msg_send![detail_text_view_class(), alloc];
    // The width must equal the real post-install width (the content column's width, which the
    // clip view enforces). Building narrower (avail_w) makes NSClipView stretch the document view
    // on install; the width delta forces a full TextKit re-layout at display time and spawns the
    // scroll animation that drags the viewport away from the top (scrollbar-not-at-top bug).
    let column_w = pane.content_w.max(1.0);
    let tv: *mut AnyObject = msg_send![
        tv,
        initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(column_w, body_h))
    ];
    *DETAIL_SOFT_WRAP_TEXT_VIEW.lock().unwrap() = code_soft_wrap.then_some(ObjPtr::new(tv));
    // Detail must finish layout before display. Non-contiguous/background layout grows the
    // document view in batches after the panel appears, and NSClipView changes bounds.origin.y
    // to preserve the old visible region—the root cause of the post-open downward jump.
    let layout: *mut AnyObject = msg_send![tv, layoutManager];
    let _: () = msg_send![layout, setAllowsNonContiguousLayout: false];
    let _: () = msg_send![layout, setBackgroundLayoutEnabled: false];

    let ns_text = make_nsstring(display_text);
    let _: () = msg_send![tv, setString: ns_text];
    CFRelease(ns_text as *const c_void);

    // Code retains only monospace layout and soft wrapping, with no syntax coloring; URLs keep
    // the list's link color.
    let storage: *mut AnyObject = msg_send![tv, textStorage];
    // Link color, font, and paragraph styles mutate NSTextStorage. An outer transaction coalesces
    // TextKit invalidation/fix-up instead of repeatedly processing the soft-wrapped detail.
    let _: () = msg_send![storage, beginEditing];
    apply_link_color(storage, display_text, style, clipboard_palette().link_text);

    let _: () = msg_send![tv, setEditable: false];
    // Selectable (it used to be disabled, so a part of a long text could never be
    // copied). In a non-key window NSTextView still supports mouse-drag selection
    // (shown gray); copying goes through the native paths -- the right-click menu, and
    // the Cmd+C forwarding in the picker's container_key_down (see copy_detail_selection).
    let _: () = msg_send![tv, setSelectable: true];
    let _: () = msg_send![tv, setDrawsBackground: false];
    // Plain detail text uses FONT_CONTROL; code uses the monospaced FONT_CONTROL for stable
    // columns/breaks. The mock sets 13px/1.75 plain and 11.5px/1.7 mono; this panel keeps the
    // shared reading size (the 14/12 scale step) and takes the mock's line-height ratio for
    // plain text below, so the density matches while its type stays on the app's scale.
    let font: *mut AnyObject = if is_code {
        msg_send![class!(NSFont), monospacedSystemFontOfSize: 11.5f64, weight: crate::theme::FONT_WEIGHT_REGULAR]
    } else {
        msg_send![class!(NSFont), systemFontOfSize: 13.0f64]
    };
    let _: () = msg_send![tv, setFont: font];
    let ink = crate::glass::panel_ink(
        clipboard_palette().primary_text,
        crate::glass::PanelInk::Primary,
    );
    let _: () = msg_send![tv, setTextColor: ink];
    if is_code {
        // Keep code line height and hanging indentation consistent in wrapped and unwrapped views.
        apply_code_paragraph_styles(storage, display_text);
    } else {
        let style: *mut AnyObject = msg_send![class!(NSMutableParagraphStyle), alloc];
        let style: *mut AnyObject = msg_send![style, init];
        let line_height = DETAIL_BODY_LINE_H;
        let _: () = msg_send![style, setMinimumLineHeight: line_height];
        let _: () = msg_send![style, setMaximumLineHeight: line_height];
        let _: () = msg_send![style, setLineBreakMode: 0isize];
        let style_key = make_nsstring("NSParagraphStyle");
        let _: () = msg_send![storage, addAttribute: style_key, value: style, range: NSRange::new(0, display_text.encode_utf16().count())];
        CFRelease(style_key as *const c_void);
        release_obj(style);
    }
    // Both code-detail modes (soft wrap and no wrap) show intra-paragraph spaces as midpoints with
    // the same tint as list-row space markers. Plain text and links get no midpoints, so this is a no-op.
    if is_code {
        apply_visible_space_markers(storage, display_text);
    }
    let _: () = msg_send![storage, endEditing];
    // The pane's padding is the scroll view's inset, so the text view needs none of its own: its
    // document view starts at the content column's first line (the mock's `.detail-text` has no
    // margin), and the code gutter below aligns with that same edge.
    let _: () = msg_send![tv, setTextContainerInset: NSSize::new(0.0, 0.0)];
    let text_container: *mut AnyObject = msg_send![tv, textContainer];
    // Zero line padding for both kinds: the mock's body starts on the content edge, and the
    // gutter column (code) is the only thing to its left.
    let _: () = msg_send![text_container, setLineFragmentPadding: 0.0f64];
    // Complete all TextKit layout at the final width, then freeze documentView height to the
    // full usedRect. Leaving NSTextView vertically resizable after display would still mutate
    // its frame asynchronously and push the viewport away from the top.
    let _: () = msg_send![text_container, setWidthTracksTextView: !no_wrap];
    // Keep this consistent with the real post-install width (see the initWithFrame note above).
    let container_w = if no_wrap { 1_000_000_000.0 } else { column_w };
    let _: () = msg_send![
        text_container,
        setContainerSize: NSSize::new(container_w, 1_000_000_000.0)
    ];
    let _: () = msg_send![tv, setHorizontallyResizable: no_wrap];
    let _: () = msg_send![tv, setVerticallyResizable: true];
    let _: () = msg_send![layout, ensureLayoutForTextContainer: text_container];
    let used: NSRect = msg_send![layout, usedRectForTextContainer: text_container];
    let text_w = if no_wrap {
        (used.origin.x + used.size.width + DETAIL_TEXT_TRAILING_GAP)
            .ceil()
            .max(column_w)
    } else {
        column_w
    };
    // The meta table below the body is part of the same document (see `install_detail_column`),
    // so the body's own height is complete here: never shorter than its share of the pane.
    let text_h = (used.origin.y + used.size.height).ceil().max(body_h);
    let text_view = install_text_section(
        ObjPtr::new(scroll),
        ColumnText {
            tv,
            w: text_w,
            h: text_h,
            gutter_numbers: (!no_wrap).then_some(text_line_numbers(display_text)),
        },
        scroll_h,
        &pane,
        entry,
        is_code,
    );
    release_obj(text_view);
    *DETAIL_TEXT_VIEW.lock().unwrap() = Some(ObjPtr::new(text_view));

    // Detail scrollbars are drawn by the custom capsules; the bounds notification only
    // refreshes capsule positions. Endpoint rubber banding is handled by native
    // elasticity -- never rewrite bounds here (doing so fights momentum mid-gesture and
    // causes the endpoint twitch).
    let clip: *mut AnyObject = msg_send![scroll, contentView];
    let _: () = msg_send![clip, setPostsBoundsChangedNotifications: true];
    let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
    let bounds_name = make_nsstring("NSViewBoundsDidChangeNotification");
    let _: () = msg_send![
        center,
        addObserver: observer(),
        selector: sel!(detailScrollIndicatorBoundsChanged:),
        name: bounds_name,
        object: clip
    ];
    CFRelease(bounds_name as *const c_void);
    *DETAIL_SCROLL_VIEW.lock().unwrap() = Some(ObjPtr::new(scroll));

    // Native scrollers are fully disabled; NSScrollView/NSClipView still perform scrolling,
    // while the custom views handle visuals and thumb dragging, preventing double scrollbars.
    // The capsule is a subview of the scroll view, so it starts on the scroll view's own edge
    // (outside the content insets) and spans the pane's full visible height.
    let vertical_indicator: *mut AnyObject = msg_send![scroll_indicator_class(), alloc];
    let vertical_indicator: *mut AnyObject = msg_send![
        vertical_indicator,
        initWithFrame: NSRect::new(
            NSPoint::new(
                scroll_w - SCROLL_INDICATOR_HIT_W - SCROLL_INDICATOR_EDGE,
                SCROLL_INDICATOR_EDGE,
            ),
            NSSize::new(
                SCROLL_INDICATOR_HIT_W,
                scroll_h - (SCROLL_INDICATOR_EDGE * 2.0) - SCROLL_INDICATOR_CORNER_RESERVE,
            )
        )
    ];
    update_scroll_indicator_visual(
        vertical_indicator,
        scroll_h - (SCROLL_INDICATOR_EDGE * 2.0) - SCROLL_INDICATOR_CORNER_RESERVE,
        false,
    );
    let _: () = msg_send![scroll, addSubview: vertical_indicator];
    *DETAIL_SCROLL_INDICATOR.lock().unwrap() = Some(ObjPtr::new(vertical_indicator));
    release_obj(vertical_indicator);

    if no_wrap {
        let horizontal_indicator: *mut AnyObject = msg_send![scroll_indicator_class(), alloc];
        let horizontal_indicator: *mut AnyObject = msg_send![
            horizontal_indicator,
            initWithFrame: NSRect::new(
                NSPoint::new(
                    3.0,
                    scroll_h - SCROLL_INDICATOR_HIT_W - 3.0,
                ),
                NSSize::new(
                    scroll_w
                        - (SCROLL_INDICATOR_EDGE * 2.0)
                        - SCROLL_INDICATOR_CORNER_RESERVE,
                    SCROLL_INDICATOR_HIT_W,
                )
            )
        ];
        update_scroll_indicator_visual(
            horizontal_indicator,
            scroll_w - (SCROLL_INDICATOR_EDGE * 2.0) - SCROLL_INDICATOR_CORNER_RESERVE,
            true,
        );
        let _: () = msg_send![content, addSubview: horizontal_indicator];
        *DETAIL_HORIZONTAL_SCROLL_INDICATOR.lock().unwrap() =
            Some(ObjPtr::new(horizontal_indicator));
        release_obj(horizontal_indicator);
    } else {
        *DETAIL_HORIZONTAL_SCROLL_INDICATOR.lock().unwrap() = None;
    }
    let _: () = msg_send![scroll, setAutoresizingMask: 1u64 << 4]; // NSViewHeightSizable
    let _: () = msg_send![content, addSubview: scroll];
    update_scroll_indicator_for(ScrollTarget::Detail);
    if no_wrap {
        update_scroll_indicator_for(ScrollTarget::DetailHorizontal);
    }
    release_obj(scroll);

    // Show the I-beam over the detail text: cursor rects apply only to the KEY window
    // (NSTextView's own I-beam rect never activates in the non-key panel -> it used to be
    // an arrow). A mouseEntered/Exited + ActiveAlways tracking area (identical to the row
    // hover) sets NSCursor manually; cursorUpdate is documented as NOT supported with
    // ActiveAlways (NSTrackingArea.h), so the enter/exit path is used. The area sits on
    // the fixed-size scroll view -- a fresh view per detail open, so the rect never goes
    // stale as the text grows.
    let opts: u64 = 0x01 | 0x80; // MouseEnteredAndExited | ActiveAlways
    let ta: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
    let bounds: NSRect = msg_send![scroll, bounds];
    let ta: *mut AnyObject = msg_send![
        ta,
        initWithRect: bounds,
        options: opts,
        owner: observer(),
        userInfo: std::ptr::null::<AnyObject>()
    ];
    let _: () = msg_send![scroll, addTrackingArea: ta];
    release_obj(ta);
}

/// The detail-text cursor: entering -> I-beam. Cursor rects apply only to the key window,
/// so NSTextView's own I-beam rect never activates in the non-key panel and the mouse
/// showed an arrow over the text; an ActiveAlways mouseEntered/Exited tracking area
/// (owner = the observer, same as the row hover) sets NSCursor manually. cursorUpdate is
/// documented as unsupported with ActiveAlways (NSTrackingArea.h), hence the enter/exit
/// path.
pub(super) extern "C" fn detail_tv_cursor_entered(
    _self: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) {
    callback_guard::void("detail_tv_cursor_entered", || unsafe {
        let ibeam: *mut AnyObject = msg_send![class!(NSCursor), IBeamCursor];
        let _: () = msg_send![ibeam, set];
    });
}

/// The detail-text cursor: leaving -> back to the default arrow.
pub(super) extern "C" fn detail_tv_cursor_exited(
    _self: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) {
    callback_guard::void("detail_tv_cursor_exited", || unsafe {
        let arrow: *mut AnyObject = msg_send![class!(NSCursor), arrowCursor];
        let _: () = msg_send![arrow, set];
    });
}

// ---------------------------------------------------------------------------------------------
// Detail pane geometry (pure; the v2 mock's `.detail-pane` / `.meta-table` / `.meta-row`)
// ---------------------------------------------------------------------------------------------

/// The image preview's drawn size (the mock's `.image-preview img`: `max-width: 100%`,
/// `max-height: 300px`, kept in proportion). It shrinks further when the pane itself is shorter.
pub(super) fn image_preview_fit(px_w: f64, px_h: f64, max_w: f64, pane_body_h: f64) -> (f64, f64) {
    let (px_w, px_h) = if px_w > 0.0 && px_h > 0.0 {
        (px_w, px_h)
    } else {
        (max_w, max_w)
    };
    let pane_scale = if pane_body_h > 0.0 {
        pane_body_h / px_h
    } else {
        1.0
    };
    let scale = (max_w / px_w)
        .min(DETAIL_IMAGE_PREVIEW_MAX_H / px_h)
        .min(pane_scale)
        .min(1.0);
    (px_w * scale, px_h * scale)
}

/// A media body's placement: the section's height and the preview centred inside the content
/// column.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct MediaBodyFrames {
    pub(super) section_h: f64,
    pub(super) media: NSRect,
}

/// Centre an image preview in the content column, inside a section at most `pane_body_h` tall.
pub(super) fn media_body_frames(
    px_w: f64,
    px_h: f64,
    content_w: f64,
    pane_body_h: f64,
) -> MediaBodyFrames {
    let (w, h) = image_preview_fit(px_w, px_h, content_w, pane_body_h);
    MediaBodyFrames {
        section_h: h,
        media: NSRect::new(
            NSPoint::new(((content_w - w) / 2.0).max(0.0), 0.0),
            NSSize::new(w, h),
        ),
    }
}

/// The body every meta table describes: `MetaField::ALL`'s field count. The panel sizes itself
/// before it knows which entry will be shown -- the detail follows the selection, and a rebuild
/// must not make the panel jump -- so this is a constant, not a per-entry count. The layout
/// resolves it into visual rows by width: two fields per row, or one below the two-column
/// minimum.
pub(super) fn meta_table_field_count() -> usize {
    MetaField::ALL.len()
}

/// The panel's minimum height: `DETAIL_TEXT_MIN_H` of body above the meta table, plus the pane
/// insets -- which is also the scroll view's height.
/// The smallest panel that still shows the mock's pane: the body floor plus the meta table.
///
/// `detail_pane_layout` already derives the scroll column from the panel's height by taking the
/// pane padding out, so the padding must not be added here a second time.
pub(super) fn detail_pane_min_height(w: f64, meta_rows: usize) -> f64 {
    let layout = detail_pane_layout(w, 0.0, meta_rows);
    (DETAIL_TEXT_MIN_H + layout.meta.height).ceil()
}

/// The document height of the detail scroll view: the body's own height plus the meta table,
/// but never less than the pane's visible height -- which is what lets the table rest on the
/// pane's bottom edge for a short body (the mock's `margin-top: auto`).
pub(super) fn detail_document_height(visible_h: f64, body_h: f64, meta_h: f64) -> f64 {
    (body_h + meta_h).max(visible_h)
}

/// One meta table row's placement, in the content column's flipped coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct MetaRowFrames {
    /// Padding above the first row (the mock's `.meta-table` padding-top).
    pub(super) table_top: f64,
    /// Each metadata row uses the mock's single value line.
    pub(super) row_h: f64,
    /// The label column's width (56pt, or 50pt in the compact layout).
    pub(super) label_w: f64,
    pub(super) gap: f64,
    pub(super) row_gap: f64,
    /// The right column's x: the second label+value pair of each row lives right of it.
    /// Equals `content_w` in the single-column fallback, so the left value keeps the row.
    pub(super) right_x: f64,
    /// Whether two fields share each row; false below the two-column minimum width, where a
    /// squeezed second column would only render an empty value cell.
    pub(super) two_col: bool,
    /// Number of metadata rows.
    pub(super) rows: usize,
    /// The table's full height, padding included.
    pub(super) height: f64,
}

/// Geometry of the meta table, measured from the TOP of the scrolling content column (so the
/// table is the document's last block). `fields` is the field count; the layout turns it into
/// visual rows by width. `row_gap` is the row spacing.
pub(super) fn meta_table_frames(content_w: f64, fields: usize) -> MetaRowFrames {
    let label_w = if content_w < DETAIL_META_COMPACT_BELOW {
        DETAIL_META_LABEL_W_COMPACT
    } else {
        DETAIL_META_LABEL_W
    };
    // `fields` is a field count, not a row count: the layout packs two per visual row when the
    // column has room, and falls back to one field per row below the two-column minimum so the
    // second column never collapses to a zero-width value.
    let two_col = content_w >= DETAIL_META_TWO_COL_MIN;
    let rows = (if two_col { fields.div_ceil(2) } else { fields }).max(1);
    let row_h = DETAIL_META_LINE_H;
    let height =
        DETAIL_META_PADDING_TOP + rows as f64 * row_h + (rows as f64 - 1.0) * DETAIL_META_ROW_GAP;
    // The right pair's column: a split of the content width, floored so the left value keeps a
    // usable width on narrow panes. In the fallback it sits at the content edge -- no right pair
    // is ever painted in that mode.
    let right_x = if two_col {
        (content_w * DETAIL_META_RIGHT_SPLIT)
            .max(label_w + DETAIL_META_GAP + 80.0 + DETAIL_META_COLUMN_GAP)
            .min(content_w)
    } else {
        content_w
    };
    MetaRowFrames {
        table_top: DETAIL_META_PADDING_TOP,
        row_h,
        label_w,
        gap: DETAIL_META_GAP,
        row_gap: DETAIL_META_ROW_GAP,
        right_x,
        two_col,
        rows,
        height,
    }
}

/// A meta FIELD's frames inside the content column (flipped): the label column and the value.
/// Fields pair two per visual row -- even indices in the left column, odd in the right column at
/// `right_x`; a trailing unpaired field (the copied-at row today) lets its value span the row's
/// full width. The table itself is painted into the document view, so only tests read the
/// geometry back.
#[cfg(test)]
fn meta_row_frames(frames: &MetaRowFrames, index: usize, content_w: f64) -> (NSRect, NSRect) {
    let (row, col) = if frames.two_col {
        (index / 2, index % 2)
    } else {
        (index, 0)
    };
    let y = frames.table_top + row as f64 * (frames.row_h + frames.row_gap);
    let col_x = if col == 0 { 0.0 } else { frames.right_x };
    let label = NSRect::new(
        NSPoint::new(col_x, y),
        NSSize::new(frames.label_w, frames.row_h),
    );
    let value_x = col_x + frames.label_w + frames.gap;
    let value_end = if col == 1 || !frames.two_col || index == MetaField::ALL.len() - 1 {
        content_w
    } else {
        frames.right_x - DETAIL_META_COLUMN_GAP
    };
    let value = NSRect::new(
        NSPoint::new(value_x, y),
        NSSize::new((value_end - value_x).max(0.0), frames.row_h),
    );
    (label, value)
}

/// The scrolling column's placement and its meta table (flipped, measured from the content
/// container's top-left).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct DetailPaneLayout {
    /// The scroll view's frame in the content container.
    pub(super) scroll: NSRect,
    /// The content column's width inside the pane insets.
    pub(super) content_w: f64,
    pub(super) meta: MetaRowFrames,
    /// The body's share of the scroll view's height: what is left once the table has its space.
    pub(super) body_h: f64,
}

/// The `.detail-pane` padding for a mode: the mock's wide pane (`10px 22px 16px`) beside the
/// picker, its narrow branch (`6px 18px 14px`, with the scroll's own 4px trailing pad) when the
/// detail fills the panel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct DetailInsets {
    pub(super) top: f64,
    pub(super) left: f64,
    pub(super) right: f64,
    pub(super) bottom: f64,
}

/// The pane padding for `in_pane` at a panel width `panel_w`.
///
/// The in-pane column is `panel_w - 18 - 22` wide so the scroll capsule keeps the same trailing
/// room as the wide pane: the mock's narrow branch sets `padding: 6px 18px 14px` and lets its
/// native scrollbar share the trailing 18px, while this pane draws its own 10pt track.
pub(super) fn detail_insets(in_pane: bool, panel_w: f64) -> DetailInsets {
    let _ = panel_w;
    if in_pane {
        DetailInsets {
            top: DETAIL_NARROW_INSET_TOP + 34.0,
            left: DETAIL_NARROW_INSET_LEFT,
            right: DETAIL_NARROW_INSET_RIGHT,
            bottom: DETAIL_NARROW_INSET_BOTTOM,
        }
    } else {
        DetailInsets {
            top: DETAIL_INSET_TOP,
            left: DETAIL_INSET_LEFT,
            right: DETAIL_INSET_RIGHT,
            bottom: DETAIL_INSET_BOTTOM,
        }
    }
}

/// Lay out the detail pane: one scrolling column inset by the mode's `.detail-pane` padding,
/// holding the preview body and the meta table after it.
pub(super) fn detail_pane_layout(w: f64, h: f64, meta_rows: usize) -> DetailPaneLayout {
    detail_pane_layout_with(detail_insets(false, w), w, h, meta_rows)
}

/// [`detail_pane_layout`] for an explicit padding (the narrow pane has its own).
pub(super) fn detail_pane_layout_with(
    inset: DetailInsets,
    w: f64,
    h: f64,
    meta_rows: usize,
) -> DetailPaneLayout {
    let scroll = NSRect::new(
        NSPoint::new(inset.left, inset.top),
        NSSize::new(
            (w - inset.left - inset.right).max(0.0),
            (h - inset.top - inset.bottom).max(0.0),
        ),
    );
    // The table is clipped to the column minus the custom scroll capsule's track, so a value is
    // never drawn under it.
    let content_w = (scroll.size.width - DETAIL_META_CLIP_INSET).max(0.0);
    let meta = meta_table_frames(content_w, meta_rows);
    DetailPaneLayout {
        scroll,
        content_w,
        body_h: (scroll.size.height - meta.height).max(0.0),
        meta,
    }
}

/// The mock's scroll-pane padding as `NSScrollView` content insets: the text/document view keeps
/// out of the padding while the wheel and the custom thumb still scroll the whole column.
/// The mock's `.detail-pane` padding, applied to the CLIP VIEW rather than to `NSScrollView`
/// content insets: content insets shrink the scroll view's clip view only when it owns a scroller
/// (an inset scroll view without one keeps the insets for layout but reports a zero-height clip
/// until it is scrolled), and this pane draws its own capsules. The text view already sits inside
/// the insets, so inset the clip conservatively: AppKit clips it to the scroll view's bounds, which
/// is the content the mock shows.
unsafe fn apply_scroll_pane_insets(scroll: *mut AnyObject) {
    let _: () = msg_send![scroll, setAutomaticallyAdjustsContentInsets: false];
    let _: () = msg_send![
        scroll,
        setContentInsets: objc2_foundation::NSEdgeInsets {
            top: 0.0,
            left: 0.0,
            bottom: 0.0,
            right: 0.0,
        }
    ];
}

/// A text body to install: the view, its frame size, and (for code) the gutter's line numbers.
struct ColumnText {
    tv: *mut AnyObject,
    w: f64,
    h: f64,
    gutter_numbers: Option<Vec<String>>,
}

/// The code gutter's numbers for a display text: one per hard line (the source lines the text
/// was built from), so a soft wrap does not advance the count.
pub(super) fn text_line_numbers(display_text: &str) -> Vec<String> {
    let hard_lines = display_text.split('\n').count().max(1);
    (1..=hard_lines).map(|n| n.to_string()).collect()
}

/// Document children use top-down coordinates, independently of NSClipView's orientation.
unsafe fn detail_document_class() -> *mut AnyObject {
    static CLASS: OnceLock<StaticClass> = OnceLock::new();
    crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabClipDocumentView", || {
        crate::ffi::build_class(c"OhMyTabClipDocumentView", class!(NSView), |builder| {
            builder.add_method::<AnyObject, _>(
                sel!(isFlipped),
                header_strip_is_flipped
                    as extern "C" fn(*mut AnyObject, Sel) -> objc2::runtime::Bool,
            );
        })
    })
    .0 as *mut AnyObject
}

/// Install the text body and the meta table into the pane's scroll view. Returns the text view
/// (the caller stores it; the document holds it from here on).
unsafe fn install_text_section(
    scroll: ObjPtr,
    text: ColumnText,
    visible_h: f64,
    pane: &DetailPaneLayout,
    entry: &ClipEntry,
    is_code: bool,
) -> *mut AnyObject {
    let ColumnText {
        tv,
        w,
        h,
        gutter_numbers,
    } = text;
    let content_w = pane.content_w;
    // The code gutter takes the mock's 26pt column plus its 6pt gap out of the code's line width,
    // so the code column starts where the mock's `.lc` does.
    let gutter = if is_code {
        gutter_numbers.as_ref().map(|_| DETAIL_CODE_GUTTER_W)
    } else {
        None
    };
    let view_w = gutter.map_or(w, |g| w + g + DETAIL_CODE_GUTTER_GAP);
    let _: () = msg_send![tv, setVerticallyResizable: false];
    let _: () = msg_send![tv, setSelectedRange: NSRange::new(0, 0)];
    let body_h = h;
    let document_h = detail_document_height(visible_h, body_h, pane.meta.height);
    let document: *mut AnyObject = msg_send![detail_document_class(), alloc];
    let document: *mut AnyObject = msg_send![
        document,
        initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(content_w.max(view_w), document_h))
    ];
    // NSClipView owns clipping; keep the document in AppKit's normal drawing hierarchy.
    // The document is flipped by its parent clip view, so a subview's frame here is measured
    // from the top edge (the section starts at 0 whatever its height).
    let x = gutter.map_or(0.0, |g| g + DETAIL_CODE_GUTTER_GAP);
    let _: () = msg_send![
        tv,
        setFrame: NSRect::new(NSPoint::new(x, 0.0), NSSize::new(view_w, h))
    ];
    let _: () = msg_send![document, addSubview: tv];
    if let (Some(g), Some(numbers)) = (gutter, gutter_numbers.as_ref()) {
        paint_code_gutter(document, numbers, g, h);
    }
    paint_meta_table(document, pane, entry, content_w, document_h);
    let _: () = msg_send![scroll.0, setDocumentView: document];
    release_obj(document);
    tv
}

/// Images use the same independently scrolling detail column as text.
pub(super) unsafe fn add_detail_image(
    content: *mut AnyObject,
    image: *mut AnyObject,
    pixels: (f64, f64),
    pane: &DetailPaneLayout,
    entry: &ClipEntry,
) {
    let scroll: *mut AnyObject = msg_send![class!(NSScrollView), alloc];
    let scroll: *mut AnyObject = msg_send![scroll, initWithFrame: pane.scroll];
    let _: () = msg_send![scroll, setDrawsBackground: false];
    let _: () = msg_send![scroll, setBorderType: 0u64];
    let _: () = msg_send![scroll, setHasVerticalScroller: false];
    let _: () = msg_send![scroll, setHasHorizontalScroller: false];
    apply_scroll_pane_insets(scroll);
    let clip: *mut AnyObject = msg_send![scroll, contentView];
    let _: () = msg_send![clip, setDrawsBackground: false];
    let _: () = msg_send![clip, setPostsBoundsChangedNotifications: true];
    let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
    let name = make_nsstring("NSViewBoundsDidChangeNotification");
    let _: () = msg_send![center, addObserver: observer(), selector: sel!(detailScrollIndicatorBoundsChanged:), name: name, object: clip];
    CFRelease(name as *const c_void);
    let view = install_image_section(scroll, image, pixels, pane.scroll.size.height, pane, entry);
    release_obj(view);
    let indicator: *mut AnyObject = msg_send![scroll_indicator_class(), new];
    let _: () = msg_send![indicator, setHidden: true];
    let _: () = msg_send![scroll, addSubview: indicator];
    *DETAIL_SCROLL_INDICATOR.lock().unwrap() = Some(ObjPtr::new(indicator));
    release_obj(indicator);
    let _: () = msg_send![content, addSubview: scroll];
    *DETAIL_SCROLL_VIEW.lock().unwrap() = Some(ObjPtr::new(scroll));
    release_obj(scroll);
}

/// Install an image body: the centred preview in its well, then the meta table. Returns the
/// image view the caller releases (the document owns it).
pub(crate) unsafe fn install_image_section(
    scroll: *mut AnyObject,
    image: *mut AnyObject,
    px: (f64, f64),
    visible_h: f64,
    pane: &DetailPaneLayout,
    entry: &ClipEntry,
) -> *mut AnyObject {
    let content_w = pane.content_w;
    let frames = media_body_frames(px.0, px.1, content_w, pane.body_h);
    let body_h = frames.section_h.max(1.0);
    let document_h = detail_document_height(visible_h, body_h, pane.meta.height);
    let document: *mut AnyObject = msg_send![detail_document_class(), alloc];
    let document: *mut AnyObject = msg_send![
        document,
        initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(content_w, document_h))
    ];
    // The preview's clipped, rounded well is a layer on the image view itself (radius 12, the
    // mock's `.image-preview`), with the mock's soft shadow under it.
    let view: *mut AnyObject = msg_send![class!(NSImageView), alloc];
    let view: *mut AnyObject = msg_send![view, initWithFrame: frames.media];
    let _: () = msg_send![view, setImage: image];
    let _: () = msg_send![view, setImageScaling: 3u64]; // NSImageScaleProportionallyUpOrDown
    let _: () = msg_send![view, setEditable: false];
    let _: () = msg_send![view, setWantsLayer: true];
    style_preview_well(view);
    let _: () = msg_send![document, addSubview: view];
    paint_meta_table(document, pane, entry, content_w, document_h);
    let _: () = msg_send![scroll, setDocumentView: document];
    release_obj(document);
    view
}

/// The mock's `.image-preview`: a 12pt rounded container, a translucent light well and a soft
/// downward shadow. Dark mode swaps in the palette's own card surface and a darker shadow so
/// the well does not glow on the dark material.
unsafe fn style_preview_well(view: *mut AnyObject) {
    let dark = clipboard_palette().dark;
    let layer: *mut AnyObject = msg_send![view, layer];
    let well = if dark {
        PREVIEW_WELL_DARK
    } else {
        PREVIEW_WELL_LIGHT
    };
    crate::ffi::layer_set_background(layer, crate::ffi::hex_to_cg_color(well));
    let _: () = msg_send![layer, setCornerRadius: crate::theme::RADIUS_CARD];
    let _: () = msg_send![layer, setMasksToBounds: true];
    let shadow = if dark {
        PREVIEW_SHADOW_DARK
    } else {
        PREVIEW_SHADOW_LIGHT
    };
    crate::ffi::layer_set_shadow_color(layer, crate::ffi::hex_to_cg_color(shadow));
    let _: () = msg_send![layer, setShadowRadius: PREVIEW_SHADOW_RADIUS];
    let _: () = msg_send![layer, setShadowOffset: NSSize::new(0.0, -PREVIEW_SHADOW_OFFSET_Y)];
    let _: () = msg_send![layer, setShadowOpacity: PREVIEW_SHADOW_OPACITY as f32];
}

/// Paint the code gutter: the mock's right-aligned line numbers in its 26pt column. Numbers are
/// one per HARD line; a soft wrap continues the same number on the next visual line.
unsafe fn paint_code_gutter(
    document: *mut AnyObject,
    numbers: &[String],
    gutter_w: f64,
    section_h: f64,
) {
    let line_h = DETAIL_LINE_H;
    let font: *mut AnyObject = msg_send![
        class!(NSFont),
        monospacedSystemFontOfSize: DETAIL_GUTTER_FONT,
        weight: crate::theme::FONT_WEIGHT_REGULAR
    ];
    let color = crate::glass::panel_ink(
        clipboard_palette().muted_text,
        crate::glass::PanelInk::Muted,
    );
    for (index, number) in numbers.iter().enumerate() {
        let y = index as f64 * line_h;
        if y >= section_h {
            break;
        }
        // The line box is DETAIL_LINE_H, the text view's own line height, so numbers share the
        // code's baselines; the 4pt trailing pad keeps them off the gutter's right edge (the
        // mock's `.ln { padding-right: 4px }`).
        let label = make_meta_cell(
            NSRect::new(
                NSPoint::new(0.0, y),
                NSSize::new(gutter_w - DETAIL_CODE_GUTTER_PAD, line_h),
            ),
            number,
            font,
            color,
            2isize, // NSTextAlignmentRight
        );
        let _: () = msg_send![document, addSubview: label];
        release_obj(label);
    }
}

/// A row of the meta table: a quiet label in the 56pt column, its value beside it. Size, fonts
/// and ink come from the palette roles, so a mode switch only needs a rebuild.
unsafe fn paint_meta_table(
    document: *mut AnyObject,
    pane: &DetailPaneLayout,
    entry: &ClipEntry,
    content_w: f64,
    document_h: f64,
) {
    let table_y = document_h - pane.meta.height;
    let label_w = pane.meta.label_w;
    let pitch = pane.meta.row_h + pane.meta.row_gap;
    let font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CAPTION];
    let label_color = crate::glass::panel_ink(
        clipboard_palette().secondary_text,
        crate::glass::PanelInk::Muted,
    );
    let value_color = crate::glass::panel_ink(
        clipboard_palette().primary_text,
        crate::glass::PanelInk::Secondary,
    );
    // Fields pair two per row (the user's compact table): even indices at the left column, odd
    // at `right_x`; a trailing unpaired field lets its value span the row. Below the two-column
    // minimum the layout emits one field per row and never reaches the right column.
    let show_source = show_source_app();
    let step = if pane.meta.two_col { 2 } else { 1 };
    for (index, pair) in meta_table(entry, show_source).chunks(step).enumerate() {
        let y = table_y + pane.meta.table_top + index as f64 * pitch;
        let left_end = if pair.len() == 1 {
            content_w
        } else {
            pane.meta.right_x - DETAIL_META_COLUMN_GAP
        };
        paint_meta_pair(
            document,
            font,
            label_color,
            value_color,
            0.0,
            left_end,
            label_w,
            pane.meta.gap,
            pane.meta.row_h,
            y,
            &pair[0].0,
            &pair[0].1,
            // The source row (the very first field) carries the source app's icon.
            if index == 0 {
                meta_source_icon_key(entry, show_source)
            } else {
                None
            },
        );
        if let Some(right) = pair.get(1) {
            paint_meta_pair(
                document,
                font,
                label_color,
                value_color,
                pane.meta.right_x,
                content_w,
                label_w,
                pane.meta.gap,
                pane.meta.row_h,
                y,
                &right.0,
                &right.1,
                None,
            );
        }
    }
}

/// The source row's icon key, and only while the "show source app" setting is on: the icon
/// would leak the same app the hidden name withholds.
fn meta_source_icon_key(entry: &ClipEntry, show_source: bool) -> Option<&str> {
    (show_source && !entry.source_key.is_empty()).then_some(entry.source_key.as_str())
}

/// One label+value pair of the meta table at `col_x`, the value clipped at `value_end`. When
/// `icon_key` names a cached small icon, the value text steps aside for it (the mock's
/// `.meta-v img`).
#[allow(clippy::too_many_arguments)]
unsafe fn paint_meta_pair(
    document: *mut AnyObject,
    font: *mut AnyObject,
    label_color: *mut AnyObject,
    value_color: *mut AnyObject,
    col_x: f64,
    value_end: f64,
    label_w: f64,
    gap: f64,
    row_h: f64,
    y: f64,
    label: &str,
    value: &str,
    icon_key: Option<&str>,
) {
    let label_cell = make_meta_cell(
        NSRect::new(NSPoint::new(col_x, y), NSSize::new(label_w, row_h)),
        label,
        font,
        label_color,
        0,
    );
    let _: () = msg_send![document, addSubview: label_cell];
    release_obj(label_cell);
    let value_x = col_x + label_w + gap;
    let mut text_x = value_x;
    if let Some(key) = icon_key.filter(|key| !key.is_empty()) {
        let icon_path = crate::icon_cache::small_icon_path_for_key(key);
        let path = make_nsstring(&icon_path);
        let img: *mut AnyObject = msg_send![class!(NSImage), alloc];
        let img: *mut AnyObject = msg_send![img, initWithContentsOfFile: path];
        CFRelease(path as *const c_void);
        if !img.is_null() {
            let icon_view: *mut AnyObject = msg_send![class!(NSImageView), alloc];
            let icon_view: *mut AnyObject = msg_send![
                icon_view,
                initWithFrame: NSRect::new(
                    NSPoint::new(value_x, y + (row_h - DETAIL_META_ICON) / 2.0),
                    NSSize::new(DETAIL_META_ICON, DETAIL_META_ICON)
                )
            ];
            let _: () = msg_send![icon_view, setImage: img];
            let _: () = msg_send![icon_view, setImageScaling: 3u64];
            let _: () = msg_send![document, addSubview: icon_view];
            release_obj(icon_view);
            text_x += DETAIL_META_ICON + DETAIL_META_ICON_GAP;
        }
        release_obj(img);
    }
    let value_cell = make_meta_cell(
        NSRect::new(
            NSPoint::new(text_x, y),
            NSSize::new((value_end - text_x).max(0.0), row_h),
        ),
        value,
        font,
        value_color,
        0,
    );
    let cell: *mut AnyObject = msg_send![value_cell, cell];
    let _: () = msg_send![cell, setLineBreakMode: 4isize]; // NSLineBreakByTruncatingTail
    let _: () = msg_send![document, addSubview: value_cell];
    release_obj(value_cell);
}

/// A borderless, background-less one-line label at `frame`.
unsafe fn make_meta_cell(
    frame: NSRect,
    text: &str,
    font: *mut AnyObject,
    color: *mut AnyObject,
    alignment: isize,
) -> *mut AnyObject {
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let label: *mut AnyObject = msg_send![label, initWithFrame: frame];
    let value = make_nsstring(text);
    let _: () = msg_send![label, setStringValue: value];
    CFRelease(value as *const c_void);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setEditable: false];
    let _: () = msg_send![label, setSelectable: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setFont: font];
    let _: () = msg_send![label, setTextColor: color];
    let _: () = msg_send![label, setAlignment: alignment];
    let cell: *mut AnyObject = msg_send![label, cell];
    let _: () = msg_send![cell, setUsesSingleLineMode: true];
    label
}

/// Find a text entry's display index in the current filters. History deduplicates text
/// globally, so text is a stable detail-entry identity; after a copied excerpt is inserted,
/// the source detail must stay selected and remain visible.
/// The empty-state document covers at least the minimum list area, otherwise exactly the
/// live visible height so its hint is truly centered.
pub(super) fn empty_state_doc_height(visible_h: f64) -> f64 {
    visible_h.max(picker_min_height() - header_strip_h() - FOOTER_H)
}

pub(super) fn visible_selection_for_text(
    history: &[ClipEntry],
    query: &str,
    filter: ClipFilter,
    text: &str,
) -> Option<usize> {
    let history_idx = history
        .iter()
        .position(|entry| entry.image.is_none() && entry.text == text)?;
    filtered_indices(history, query, filter)
        .iter()
        .position(|&idx| idx == history_idx)
}

/// The hover-style gate (pure; unit-tested): when the pointer is outside the picker
/// window, the hover index is always treated as invalid (NO_SELECTION). HOVER_ROW only
/// mirrors the last enter/exited event -- lost events (REBUILDING suppression, cross-panel
/// transitions, row teardown) can freeze it into a stale value that would paint a phantom
/// hover fill onto rebuilt rows.
pub(super) fn effective_hover_row(pointer_in_window: bool, hover_row: usize) -> usize {
    if pointer_in_window {
        hover_row
    } else {
        NO_SELECTION
    }
}

pub(super) fn rect_contains_point(rect: NSRect, point: NSPoint) -> bool {
    point.x >= rect.origin.x
        && point.x <= rect.origin.x + rect.size.width
        && point.y >= rect.origin.y
        && point.y <= rect.origin.y + rect.size.height
}

/// Whether the pointer is currently inside the picker window. NSEvent.mouseLocation and
/// the window frame share global screen coordinates (bottom-left origin), so containment
/// is a direct comparison.
unsafe fn pointer_in_picker_window() -> bool {
    let Some(w) = *PICKER_WINDOW.lock().unwrap() else {
        return false;
    };
    // Panel coordinates: the padding around the window is not part of the picker, and treating it as such
    // would keep the detail panel open for a pointer that is outside the panel the user sees.
    let frame = crate::glass::panel_frame_of(w.0);
    let mouse: NSPoint = msg_send![class!(NSEvent), mouseLocation];
    rect_contains_point(frame, mouse)
}

/// Immediately rebuild the open history list after copying from detail. Polling writes to
/// memory, but existing row views do not read the new history until the next summon. Restore
/// the source-detail selection before rebuilding, so a new excerpt at the top does not make
/// the highlight point at it while the right panel still shows the old detail.
fn refresh_open_picker_after_detail_copy(source_detail_text: Option<&str>) {
    if !PICKER_VISIBLE.load(Ordering::SeqCst) {
        return;
    }
    // The pointer is by definition over the detail panel in this flow, so no list hover
    // can exist; clear HOVER_ROW explicitly so the rebuild never paints a stale value onto
    // the newly inserted top entry (the phantom hover fill). A second line of defense --
    // the pointer gate in rebuild_rows is the primary one.
    *HOVER_ROW.lock().unwrap() = NO_SELECTION;
    if let Some(text) = source_detail_text {
        let selection = {
            let history = CLIP_HISTORY.lock().unwrap();
            let query = with_clipboard_ui(|ui| ui.search_query.clone());
            let filter = *CLIP_FILTER.lock().unwrap();
            visible_selection_for_text(&history, &query, filter, text)
        };
        if let Some(selection) = selection {
            set_picker_selection(selection);
        }
    }
    unsafe { rebuild_rows() };
    // Inserting the excerpt moves the source row down. Once rebuilding releases REBUILDING,
    // recompute only the detail position without recreating its text view, preserving the
    // user's current text selection.
    reposition_detail();
}

/// Copy the detail text view's SELECTION to the pasteboard (full text when nothing is
/// selected), toast, and keep the detail open (the user may copy more ranges). Does NOT
/// stamp the paste marker -- this is a genuine copy that should enter the history (the
/// opposite of the paste-write-back suppression).
pub(super) fn copy_detail_selection() {
    let tv = match *DETAIL_TEXT_VIEW.lock().unwrap() {
        Some(t) => t.0,
        None => return,
    };
    let source_detail_text = {
        let sel = picker_selection();
        mapped_index(sel).and_then(|history_idx| {
            CLIP_HISTORY
                .lock()
                .unwrap()
                .get(history_idx)
                .filter(|entry| entry.image.is_none())
                .map(|entry| entry.text.clone())
        })
    };
    unsafe {
        let sel_range: NSRange = msg_send![tv, selectedRange];
        let mapped = {
            let map = DETAIL_SOURCE_MAP.lock().unwrap();
            map.as_ref().map(|source_map| {
                (
                    source_map.source.clone(),
                    source_map.source_range(sel_range),
                )
            })
        };
        let text = if let Some((source, source_range)) = mapped {
            // Code details may contain display-only breaks; extract from the source mapping
            // so formatting characters are never copied.
            let source_ns = make_nsstring(&source);
            let sub: *mut AnyObject = msg_send![source_ns, substringWithRange: source_range];
            let text = nsstring_to_rust(sub);
            CFRelease(source_ns as *const c_void);
            text
        } else {
            let full: *mut AnyObject = msg_send![tv, string];
            if sel_range.length > 0 {
                let sub: *mut AnyObject = msg_send![full, substringWithRange: sel_range];
                nsstring_to_rust(sub)
            } else {
                nsstring_to_rust(full)
            }
        };
        write_pasteboard_text(&text, false);
        // This copy originates in our app, so read it back and rebuild immediately instead
        // of relying only on the 0.5s poll/notification; otherwise the new excerpt appears
        // only after the next picker summon while detail remains open.
        poll_clipboard();
        refresh_open_picker_after_detail_copy(source_detail_text.as_deref());
        show_toast(&t("clipboard.toast_copied"));
    }
}

/// The picker's backdrop policy: its rows, group headers and footer bands carry their own text
/// surfaces, so the material is left honest (design-style §3). The passive compensation layer is
/// transparent while key and only tinted while a pinned picker remains visible after resigning key.
///
/// Elevation stays `HIGH`, and no outline request is added on this side, after reviewing what the
/// v2 mock's own shadow and edge do:
/// - The mock floats the sheet on a heavy drop shadow (`0 34px 84px` at 30%) *and* a 1px glass
///   line. The shared `high` level is the design-system's shadow for floating panels, and the
///   outline is what `glass::install_panel_outline` already adds for frost and `opaque` -- the two
///   materials whose hierarchy carries no directional rim of its own. Liquid Glass is deliberately
///   skipped there: the system draws its own rim, and a uniform 1pt stroke flattens it into a
///   drawn rectangle (measured in `install_panel_outline`'s comment).
/// - So the elevation is required for all three materials (a translucent sheet over a bright
///   desktop has no silhouette without a shadow carrier, and frost/opaque have no rim at all), and
///   the 1pt stroke is required exactly where the shared installer applies it. Neither is a request
///   parameter this module can (or needs to) change.
pub(crate) fn picker_backdrop_options() -> crate::glass::BackdropOptions {
    // `high`: the picker floats over another app's content exactly as the switcher does.
    // Keep a transparent compensation layer available so it can counter the system's passive
    // Liquid Glass treatment only while a pinned picker is non-key.
    crate::glass::BackdropOptions::new(Some(picker_inactive_compensation_alpha()))
        .with_elevation(Some(crate::theme::ELEVATION_HIGH))
}

/// The detail panel is a passive companion of the picker, so its Liquid Glass takes the inactive
/// darkening compensation. It also scrims its own text.
///
/// It keeps the same elevation as the picker: the pair reads as one group only while both cast the
/// same depth, and the detail sits on the same desktop edge. It has no outline request of its own --
/// the shared installer's rule applies unchanged (frost/opaque stroke, Liquid Glass rim).
pub(crate) const fn detail_backdrop_options() -> crate::glass::BackdropOptions {
    crate::glass::BackdropOptions::new(Some(crate::glass::INACTIVE_GLASS_COMPENSATION_ALPHA))
        .with_elevation(Some(crate::theme::ELEVATION_HIGH))
}

/// During the settings live preview, update only the backdrop surfaces and detail
/// compensation layer; do not rebuild clipboard content.
pub(crate) unsafe fn apply_glass_properties() {
    if let Some(backdrop) = *PICKER_BACKDROP.lock().unwrap() {
        crate::glass::apply_live_properties(backdrop, picker_backdrop_options());
    }
    if let Some(backdrop) = *DETAIL_BACKDROP.lock().unwrap() {
        crate::glass::apply_live_properties(backdrop, detail_backdrop_options());
    }
    // An open menu must follow material, theme and passive-compensation changes with the picker.
    if filter_popover_open() {
        let parent = *PICKER_CONTENT_PARENT.lock().unwrap();
        if let Some(parent) = parent {
            build_filter_popover(parent.0);
        }
    }
}

/// Bring both panels' backdrops in line with the effective material. Called when the
/// panel-material setting changes and when a panel is shown: a panel whose window does not
/// exist yet installs the new material on first build, and an existing window only swaps
/// when its installed material actually differs.
pub(crate) unsafe fn apply_backdrop_material() {
    if let Some(new) = swap_backdrop_if_material_changed(
        &PICKER_WINDOW,
        &PICKER_BACKDROP,
        picker_backdrop_options(),
    ) {
        // The picker footer rebuild (locale/text-size changes) hangs content off this
        // pointer; a stale one would point into the retired hierarchy.
        *PICKER_CONTENT_PARENT.lock().unwrap() = Some(ObjPtr::new(new.content_parent));
    }
    swap_backdrop_if_material_changed(&DETAIL_WINDOW, &DETAIL_BACKDROP, detail_backdrop_options());
    apply_glass_properties();
}

/// Swap one panel's backdrop when its installed material differs from the effective one.
/// Returns the new backdrop, or None when there was nothing to swap.
unsafe fn swap_backdrop_if_material_changed(
    window_slot: &crate::ffi::MainThreadSlot<Option<ObjPtr>>,
    backdrop_slot: &crate::ffi::MainThreadSlot<Option<crate::glass::InstalledBackdrop>>,
    options: crate::glass::BackdropOptions,
) -> Option<crate::glass::InstalledBackdrop> {
    let window = (*window_slot.lock().unwrap())?;
    let old = (*backdrop_slot.lock().unwrap())?;
    if old.material == crate::glass::PanelMaterial::effective() {
        return None;
    }
    // The panel rect, not the window's content rect: a swap rebuilds the *material* hierarchy, and the
    // window is padded around the panel for the elevation shadow.
    let panel_rect = crate::glass::panel_frame_of(window.0);
    let new = crate::glass::swap_backdrop(
        window.0,
        &old,
        panel_rect,
        super::PICKER_PANEL_RADIUS,
        options,
    );
    *backdrop_slot.lock().unwrap() = Some(new);
    Some(new)
}

/// Apply the active light/dark appearance to already-created clipboard panels.
pub(crate) unsafe fn apply_theme() {
    if let Some(window) = *PICKER_WINDOW.lock().unwrap() {
        apply_panel_appearance(window.0);
    }
    if let Some(window) = *DETAIL_WINDOW.lock().unwrap() {
        apply_panel_appearance(window.0);
    }
    apply_glass_properties();
    if let Some(buttons) = *CLEAR_HISTORY_ACTION_BUTTONS.lock().unwrap() {
        for button in buttons {
            set_clear_confirmation_button_style(button.0, false);
        }
    }
    // The gear's ink is repainted here too: the picker window is reused across showings, so a tint
    // captured at build time would keep the previous mode's colour.
    if let Some(button) = *PICKER_OPTIONS_ORB.lock().unwrap() {
        set_settings_button_style(button.0, false);
    }
    // The pin's ink and glyph both come from the palette, and this is the system-appearance entry
    // point (the "auto" theme path calls apply_theme directly, without the locale coordinator).
    super::notifications::refresh_pin_button();
}

/// Register a dynamic class exactly once, for the classes here that still declare their methods
/// by hand.
///
/// `crate::ffi::build_class` derives each type encoding from the Rust signature, so it cannot
/// register a method whose callback lives in another module and still takes `*mut c_void` (objc2
/// would then encode `^v` where AppKit expects `@`). These classes keep `class_addMethod` for
/// that reason, but they still need what `build_class` also gives: one registration per process,
/// and a loud failure instead of every following `class_addMethod` writing through nil when the
/// name is already taken.
unsafe fn raw_class(
    slot: &'static OnceLock<StaticClass>,
    name: &'static std::ffi::CStr,
    superclass: &'static objc2::runtime::AnyClass,
    build: impl FnOnce(*mut AnyObject),
) -> StaticClass {
    crate::ffi::StaticClass::register_once(slot, name, || {
        let cls =
            objc_allocateClassPair(superclass as *const _ as *mut AnyObject, name.as_ptr(), 0);
        assert!(
            !cls.is_null(),
            "dynamic class {} could not be allocated; is it registered twice?",
            name.to_string_lossy()
        );
        build(cls);
        objc_registerClassPair(cls);
        crate::ffi::StaticClass::defining(cls)
    })
}

pub(super) unsafe fn ensure_picker_window() {
    if PICKER_WINDOW.lock().unwrap().is_some() {
        return;
    }
    let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
    let screen_frame: NSRect = msg_send![screen, frame];
    let w = PICKER_W;
    // Initial height sized for the max height (placeholder; show_picker re-sizes per
    // summon using the real pitches).
    let h = PICKER_MAX_HEIGHT;
    let x = (screen_frame.size.width - w) / 2.0 + screen_frame.origin.x;
    let y = (screen_frame.size.height - h) / 2.0 + screen_frame.origin.y;
    let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));

    // NSPanel + NSWindowStyleMaskNonactivatingPanel (1<<7): becomes key WITHOUT activating
    // the owning app (same as the switcher overlay), so focus isn't stolen.
    let style: u64 = 1 << 7;

    static WINDOW_CLS: OnceLock<StaticClass> = OnceLock::new();
    let window_cls = raw_class(
        &WINDOW_CLS,
        c"OhMyTabClipboardWindow",
        class!(NSPanel),
        |cls| {
            let types_bool = CString::new("B@:").unwrap();
            class_addMethod(
                cls,
                sel!(canBecomeKeyWindow),
                picker_window_can_become_key as *mut c_void,
                types_bool.as_ptr(),
            );
            let types_event = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(sendEvent:),
                clipboard_window_send_event as *mut c_void,
                types_event.as_ptr(),
            );
        },
    );
    // Recorded so the sendEvent: override's super dispatch (notifications.rs) can name the
    // class that defined it instead of re-deriving it from the instance.
    let _ = PICKER_WINDOW_CLASS.set(window_cls);
    let window_cls = window_cls.0 as *mut AnyObject;
    let window: *mut AnyObject = msg_send![window_cls, alloc];
    let window: *mut AnyObject = msg_send![window, initWithContentRect: frame, styleMask: style, backing: 2u64, defer: false];
    apply_panel_appearance(window);
    // The picker-wide tracking area needs mouseMoved continuously so hover can be reconciled
    // while the pointer is over row padding outside the content buttons.
    let _: () = msg_send![window, setAcceptsMouseMovedEvents: true];
    let _: () = msg_send![window, setLevel: 3u64];
    let _: () = msg_send![window, setOpaque: false];
    let _: () = msg_send![window, setReleasedWhenClosed: false];
    // Same backdrop as the switcher overlay: clearColor + a glass view for the visuals (below).
    let clear: *mut AnyObject = msg_send![class!(NSColor), clearColor];
    let _: () = msg_send![window, setBackgroundColor: clear];
    // The glass carries its own depth; the window shadow is redundant (same as the overlay).
    let _: () = msg_send![window, setHasShadow: false];

    // Share the rounded 26+ Glass / frost / opaque backdrop with the other panels.
    let backdrop = crate::glass::install_backdrop(
        window,
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h)),
        super::PICKER_PANEL_RADIUS,
        picker_backdrop_options(),
    );
    *PICKER_BACKDROP.lock().unwrap() = Some(backdrop);
    *PICKER_CONTENT_PARENT.lock().unwrap() = Some(ObjPtr::new(backdrop.content_parent));
    let content_parent = backdrop.content_parent;

    // Container (receives key events; flipped so rows stack top-down, newest on top).
    static CONTAINER_CLS: OnceLock<StaticClass> = OnceLock::new();
    let container_cls = raw_class(
        &CONTAINER_CLS,
        c"OhMyTabClipboardContainer",
        class!(NSView),
        |cls| {
            let types_key = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(keyDown:),
                container_key_down as *mut c_void,
                types_key.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseMoved:),
                container_mouse_moved as *mut c_void,
                types_key.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseEntered:),
                container_mouse_moved as *mut c_void,
                types_key.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseExited:),
                container_mouse_exited as *mut c_void,
                types_key.as_ptr(),
            );
            // Blank-area drag, the same gesture the transparent surface over the whole panel
            // carries (see `drag.rs`): this extends it over the list's own empty space below the
            // last row. Only a press that lands on the document view itself arrives here -- rows and
            // the action buttons are controls with their own hit targets -- and the handlers claim
            // the press, not the wheel, so scrolling still bubbles to the scroll view.
            class_addMethod(
                cls,
                sel!(mouseDown:),
                super::drag::drag_mouse_down as *mut c_void,
                types_key.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseDragged:),
                super::drag::drag_mouse_dragged as *mut c_void,
                types_key.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseUp:),
                super::drag::drag_mouse_up as *mut c_void,
                types_key.as_ptr(),
            );
            let types_first_mouse = CString::new("B@:@").unwrap();
            class_addMethod(
                cls,
                sel!(acceptsFirstMouse:),
                super::drag::drag_accepts_first_mouse as *mut c_void,
                types_first_mouse.as_ptr(),
            );
            let types_bool = CString::new("B@:").unwrap();
            class_addMethod(
                cls,
                sel!(acceptsFirstResponder),
                container_accepts_first_responder as *mut c_void,
                types_bool.as_ptr(),
            );
            // Flipped: origin at top-left, y grows downward -- rows stack from the top.
            class_addMethod(
                cls,
                sel!(isFlipped),
                container_is_flipped as *mut c_void,
                types_bool.as_ptr(),
            );
        },
    );
    let container = container_cls.0 as *mut AnyObject;
    let container: *mut AnyObject = msg_send![container, alloc];
    let container: *mut AnyObject = msg_send![
        container,
        initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h))
    ];
    // The document view's height is set dynamically by rebuild_rows; it must NOT stretch
    // with the scroll view.
    let _: () = msg_send![container, setAutoresizingMask: 0u64];
    add_picker_hover_tracking(content_parent, container);

    // A fixed header strip holding the search field + the clear button; it does NOT scroll
    // with the list (scrolling text used to bleed through the translucent tiles). Flipped so
    // the search/clear frames work unchanged.
    let header_strip: *mut AnyObject = {
        let strip: *mut AnyObject = msg_send![header_strip_class(), alloc];
        let strip: *mut AnyObject = msg_send![
            strip,
            initWithFrame: NSRect::new(
                NSPoint::new(0.0, h - header_strip_h()),
                NSSize::new(w, header_strip_h())
            )
        ];
        // NSViewMinYMargin (8): the bottom gap adapts -> pinned to the top as the window
        // resizes.
        let _: () = msg_send![strip, setAutoresizingMask: 8u64];
        let _: () = msg_send![content_parent, addSubview: strip];
        release_obj(strip);
        // The search field's own fill is translucent and its text sits on the panel material
        // otherwise; the material is the panel's surface.
        *PICKER_HEADER_STRIP.lock().unwrap() = Some(ObjPtr::new(strip));
        strip
    };

    // NSScrollView: wheel scrolling + a custom scroll indicator (the system scroller is
    // replaced for a cleaner look on the glass). It only occupies the area below the header
    // strip: the list scrolls within its own region and can never overlap the search row.
    let scroll: *mut AnyObject = msg_send![class!(NSScrollView), alloc];
    let scroll: *mut AnyObject = msg_send![
        scroll,
        initWithFrame: NSRect::new(
            NSPoint::new(0.0, FOOTER_H),
            NSSize::new(w, h - header_strip_h() - FOOTER_H)
        )
    ];
    let _: () = msg_send![scroll, setAutoresizingMask: 18u64];
    let _: () = msg_send![scroll, setBorderType: 0u64]; // NSNoBorder
    let _: () = msg_send![scroll, setDrawsBackground: false];
    let _: () = msg_send![scroll, setHasVerticalScroller: false];
    let _: () = msg_send![scroll, setHasHorizontalScroller: false];
    let _: () = msg_send![content_parent, addSubview: scroll];
    release_obj(scroll);
    let _: () = msg_send![scroll, setDocumentView: container];
    release_obj(container);

    // Custom scroll indicator: a 4pt rounded capsule on the right, semi-transparent white;
    // shown while scrolling and faded out 1s after scrolling stops.
    //
    // A plain NSView is not enough here: with the system scroller disabled it neither handles
    // thumb dragging nor changes NSScrollView's content offset, which is why only wheel/key
    // scrolling worked before.
    let indicator: *mut AnyObject = msg_send![scroll_indicator_class(), alloc];
    let indicator: *mut AnyObject = msg_send![
        indicator,
        initWithFrame: NSRect::new(
            NSPoint::new(w - SCROLL_INDICATOR_HIT_W - 3.0, 3.0),
            NSSize::new(
                SCROLL_INDICATOR_HIT_W,
                h - header_strip_h() - FOOTER_H - 6.0,
            )
        )
    ];
    // The transparent hit area is wider than the visible capsule; do not paint the parent
    // layer or all 10pt would become visible.
    update_scroll_indicator_visual(indicator, h - header_strip_h() - FOOTER_H - 6.0, false);
    let _: () = msg_send![indicator, setHidden: true];
    let _: () = msg_send![scroll, addSubview: indicator];
    release_obj(indicator);

    // Observe the clip view's bounds changes (scrolling) -> update the indicator + restart
    // the fade-out timer.
    let clip: *mut AnyObject = msg_send![scroll, contentView];
    let _: () = msg_send![clip, setPostsBoundsChangedNotifications: true];
    let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
    let bounds_name = make_nsstring("NSViewBoundsDidChangeNotification");
    let _: () = msg_send![
        center,
        addObserver: observer(),
        selector: sel!(scrollIndicatorBoundsChanged:),
        name: bounds_name,
        object: clip
    ];
    CFRelease(bounds_name as *const c_void);
    *SCROLL_VIEW.lock().unwrap() = Some(ObjPtr::new(scroll));
    *SCROLL_INDICATOR.lock().unwrap() = Some(ObjPtr::new(indicator));

    // Top search field (an NSSearchField subclass): fuzzy entry filtering. Not auto-focused
    // (the user clicks it to start searching). The subclass only overrides cancelOperation:
    // (Esc) -- while editing, keys go to the field editor, and commands like ↓ are intercepted
    // via the delegate's control:textView:doCommandBySelector: (see search_field_do_command).
    static SEARCH_CLS: OnceLock<StaticClass> = OnceLock::new();
    let search_cls = raw_class(
        &SEARCH_CLS,
        c"OhMyTabClipSearchField",
        class!(NSSearchField),
        |cls| {
            let types_v = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(cancelOperation:),
                search_field_cancel as *mut c_void,
                types_v.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(clearSearch:),
                search_clear_button as *mut c_void,
                types_v.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseDown:),
                search_field_mouse_down as *mut c_void,
                types_v.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseMoved:),
                search_field_mouse_moved as *mut c_void,
                types_v.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseEntered:),
                search_field_mouse_entered as *mut c_void,
                types_v.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseExited:),
                search_field_mouse_exited as *mut c_void,
                types_v.as_ptr(),
            );
        },
    );
    // Recorded so the mouseDown: override's super dispatch (notifications.rs) can name the
    // class that defined it.
    let _ = crate::clipboard::SEARCH_FIELD_CLASS.set(search_cls);
    let search_cls = search_cls.0 as *mut AnyObject;
    // The search field (the mockup's .search): 48pt tall, radius 10, a 4.5% black fill
    // with a 1px inner ring; the placeholder is left-aligned. The corner close button shares the
    // row, so the shared geometry gives the field the remaining width.
    let (search_frame, pause_frame, options_frame, pin_frame, close_frame) = picker_header_frames();
    let search_w = search_frame.size.width;
    let search_surface: *mut AnyObject = msg_send![class!(NSView), alloc];
    let search_surface: *mut AnyObject = msg_send![search_surface, initWithFrame: search_frame];
    let _: () = msg_send![search_surface, setWantsLayer: true];
    let surface_layer: *mut AnyObject = msg_send![search_surface, layer];
    if !surface_layer.is_null() {
        let _: () = msg_send![surface_layer, setCornerRadius: SEARCH_R];
        let _: () = msg_send![surface_layer, setMasksToBounds: true];
        crate::ffi::layer_set_background(
            surface_layer,
            crate::ffi::hex_to_cg_color(picker_field_fill(false)),
        );
        let _: () = msg_send![surface_layer, setBorderWidth: 1.0f64];
        crate::ffi::layer_set_border(
            surface_layer,
            crate::ffi::hex_to_cg_color(picker_field_ring(false)),
        );
    }
    let _: () = msg_send![header_strip, addSubview: search_surface];
    release_obj(search_surface);
    *SEARCH_FIELD_SURFACE.lock().unwrap() = Some(ObjPtr::new(search_surface));

    let search: *mut AnyObject = msg_send![search_cls, alloc];
    let search: *mut AnyObject = msg_send![search, initWithFrame: search_frame];
    // A custom cell: the placeholder = "magnifier SF Symbol + search hint" drawn at the
    // field's left (see search_cell_class), with the ⌘F keycap at the far right.
    let cell: *mut AnyObject = msg_send![search_cell_class(), alloc];
    let empty_ns = make_nsstring("");
    let cell: *mut AnyObject = msg_send![cell, initTextCell: empty_ns];
    CFRelease(empty_ns as *const c_void);
    // NSSearchField's field editor does not reliably inherit the control font; set the cell to
    // the shared size here and apply it again to the live editor in selectWithFrame:.
    let search_font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: SEARCH_FONT_SIZE];
    let _: () = msg_send![cell, setFont: search_font];
    // The placeholder (magnifier + text) is built separately; per the mockup it is a
    // static string (the entry count moved to the footer).
    rebuild_search_hint();
    // Keep the native search button's layout width but clear its image; the cell hand-draws the
    // original ⌕ in every state, preventing AppKit from substituting a different stock magnifier
    // while typing.
    let search_button: *mut AnyObject = msg_send![cell, searchButtonCell];
    if !search_button.is_null() {
        let blank_image: *mut AnyObject = msg_send![class!(NSImage), alloc];
        let blank_image: *mut AnyObject =
            msg_send![blank_image, initWithSize: NSSize::new(22.0, 22.0)];
        let _: () = msg_send![search_button, setImage: blank_image];
        release_obj(blank_image);
    }
    let _: () = msg_send![search, setCell: cell];
    // NSSearchFieldCell normally sends its × cancelOperation: through the responder chain,
    // where the field editor can consume it. Bind it directly to clearSearch: so a click always
    // clears the matching filter too.
    let cancel_cell: *mut AnyObject = msg_send![cell, cancelButtonCell];
    if !cancel_cell.is_null() {
        // Keep the native cancel image transparent for layout/event compatibility; the cell
        // draws the visible × at the HTML size so no system symbol is mixed in.
        let blank_image: *mut AnyObject = msg_send![class!(NSImage), alloc];
        let blank_image: *mut AnyObject =
            msg_send![blank_image, initWithSize: NSSize::new(SEARCH_CLEAR_W, SEARCH_CLEAR_W)];
        let _: () = msg_send![cancel_cell, setImage: blank_image];
        release_obj(blank_image);
        let _: () = msg_send![cancel_cell, setTarget: search];
        let _: () = msg_send![cancel_cell, setAction: sel!(clearSearch:)];
    }
    release_obj(cell);
    // Explicitly empty the placeholder property (belt and braces; no reader finds text).
    let empty_attr: *mut AnyObject = msg_send![class!(NSAttributedString), alloc];
    let empty_attr: *mut AnyObject = msg_send![empty_attr, init];
    let _: () = msg_send![search, setPlaceholderAttributedString: empty_attr];
    release_obj(empty_attr);
    // FIX: a custom cell created via initTextCell: is NOT editable by default
    // (isEditable=false), which makes NSSearchField refuse first responder -- clicks and
    // the ↑ jump could never start editing. Editable must be restored explicitly after
    // replacing the cell (selectable too, for good measure).
    let _: () = msg_send![search, setEditable: true];
    let _: () = msg_send![search, setSelectable: true];
    // The focus ring draws a square outline while editing, breaking the rounded look --
    // disabled.
    let _: () = msg_send![search, setFocusRingType: 1u64]; // NSFocusRingTypeNone
                                                           // Editing text is left-aligned like the placeholder (the mockup's layout).
    let _: () = msg_send![search, setAlignment: 0u64]; // left
                                                       // Match the 14pt hand-drawn retained query after ↓, avoiding a font-size jump on focus change.
    let _: () = msg_send![search, setFont: search_font];
    // Frosted: drop the system bezel and use the shared field surface/ring; the remaining
    // system × is bound directly to clearSearch:, not the responder chain.
    let _: () = msg_send![search, setBezeled: false];
    let _: () = msg_send![search, setDrawsBackground: false];
    let _: () = msg_send![search, setWantsLayer: true];
    let search_layer: *mut AnyObject = msg_send![search, layer];
    style_search_field(search, false);
    let _: () = msg_send![search_layer, setBorderWidth: 0.0f64];
    let _: () = msg_send![search_layer, setCornerRadius: SEARCH_R];
    // Delegate = observer() (reusing the notification singleton): intercepts ↓ (the field
    // editor forwards moveDown:).
    let _: () = msg_send![search, setDelegate: observer()];
    // The search field lives in the fixed header strip (it does not scroll with the list).
    let _: () = msg_send![header_strip, addSubview: search];
    // Track only the search field's × with a tracking area; InVisibleRect lets AppKit update
    // its range automatically if the field is resized.
    let tracking: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
    let tracking: *mut AnyObject = msg_send![
        tracking,
        initWithRect: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(search_w, SEARCH_H)),
        // NSTrackingArea requires one active state; this nonactivating panel needs ActiveAlways.
        options: 0x283u64, // entered/exited + moved + active-always + in-visible-rect
        owner: search,
        userInfo: std::ptr::null::<AnyObject>()
    ];
    let _: () = msg_send![search, addTrackingArea: tracking];
    release_obj(tracking);
    // The hand-drawn × has no native click target, so overlay a transparent NSButton that gets
    // the click before NSSearchFieldCell can consume mouseDown. The cell below still draws its
    // glyph and hover fill consistently.
    let clear_button: *mut AnyObject = msg_send![class!(NSButton), alloc];
    let clear_button: *mut AnyObject = msg_send![
        clear_button,
        initWithFrame: NSRect::new(
            NSPoint::new(
                SEARCH_PAD_X + search_w - SEARCH_PAD_IN - SEARCH_CLEAR_W,
                TOP_PAD_Y + (SEARCH_H - ACTION_H) / 2.0
            ),
            NSSize::new(SEARCH_CLEAR_W, ACTION_H)
        )
    ];
    let _: () = msg_send![clear_button, setBordered: false];
    let empty_title = make_nsstring("");
    let _: () = msg_send![clear_button, setTitle: empty_title];
    CFRelease(empty_title as *const c_void);
    let _: () = msg_send![clear_button, setTarget: search];
    let _: () = msg_send![clear_button, setAction: sel!(clearSearch:)];
    let _: () = msg_send![clear_button, setHidden: true];
    let _: () = msg_send![header_strip, addSubview: clear_button];
    release_obj(clear_button);
    *SEARCH_CLEAR_BUTTON.lock().unwrap() = Some(ObjPtr::new(clear_button));
    // The header's corner close button: the switcher overlay's × (same class, palette and hover
    // feedback) as a persistent second way out beside the outside-click / Esc dismissal.
    let close_button =
        crate::overlay::make_close_button(close_frame, PICKER_CLOSE_BTN_FONT, PICKER_CLOSE_BTN_R);
    let _: () = msg_send![close_button, setTarget: observer()];
    let _: () = msg_send![close_button, setAction: sel!(pickerClose:)];
    let close_label = make_nsstring(&t("clipboard.close"));
    let _: () = msg_send![close_button, setAccessibilityLabel: close_label];
    CFRelease(close_label as *const c_void);
    let _: () = msg_send![header_strip, addSubview: close_button];
    release_obj(close_button);
    *PICKER_CLOSE_BUTTON.lock().unwrap() = Some(ObjPtr::new(close_button));

    // The options orb (the mock's `.orb.has-filter`): opens the filter/options popover. Its
    // `orb-dot` sublayer is the mock's 5px accent dot, shown only while a non-All filter is
    // active.
    let options_button: *mut AnyObject = msg_send![hover_button_class(), alloc];
    let options_button: *mut AnyObject = msg_send![options_button, initWithFrame: options_frame];
    let _: () = msg_send![options_button, setBordered: false];
    let _: () = msg_send![options_button, setWantsLayer: true];
    let options_layer: *mut AnyObject = msg_send![options_button, layer];
    if !options_layer.is_null() {
        // The mock's `.orb`: a circle, not the control radius. The orb is a button filling a
        // square frame, so its corner sublayers (the orb-dot below, and the hover sheen) would
        // otherwise paint over the square corner instead of being clipped to the circle.
        let _: () = msg_send![options_layer, setCornerRadius: PICKER_ORB_SIZE / 2.0];
        let _: () = msg_send![options_layer, setMasksToBounds: true];
    }
    let symbol_name = make_nsstring("ellipsis");
    let options_image: *mut AnyObject = msg_send![
        class!(NSImage),
        imageWithSystemSymbolName: symbol_name,
        accessibilityDescription: std::ptr::null::<AnyObject>()
    ];
    CFRelease(symbol_name as *const c_void);
    if !options_image.is_null() {
        let options_config: *mut AnyObject = msg_send![
            class!(NSImageSymbolConfiguration),
            configurationWithPointSize: PICKER_GEAR_GLYPH_SIZE,
            weight: crate::theme::FONT_WEIGHT_REGULAR
        ];
        let options_image: *mut AnyObject =
            msg_send![options_image, imageWithSymbolConfiguration: options_config];
        let _: () = msg_send![options_image, setTemplate: true];
        let _: () = msg_send![options_button, setImage: options_image];
    }
    let options_title = make_nsstring("");
    let _: () = msg_send![options_button, setTitle: options_title];
    CFRelease(options_title as *const c_void);
    let _: () = msg_send![options_button, setTarget: observer()];
    let _: () = msg_send![options_button, setAction: sel!(toggleFilterPopover:)];
    set_settings_button_style(options_button, false);
    add_hover_tracking(options_button);
    let options_label = make_nsstring(&t("clipboard.options_button"));
    let _: () = msg_send![options_button, setAccessibilityLabel: options_label];
    CFRelease(options_label as *const c_void);
    let options_tooltip = make_nsstring(&t("clipboard.options_button"));
    let _: () = msg_send![options_button, setToolTip: options_tooltip];
    CFRelease(options_tooltip as *const c_void);
    // The orb-dot (`.orb-dot`): an accent dot pinned to the orb's top-right, drawn whenever the
    // active filter is not "All". Its frame comes from `options_orb_dot_frame`, which keeps the
    // whole disc outside the orb's circular clip.
    if !options_layer.is_null() {
        let dot: *mut AnyObject = msg_send![class!(CALayer), layer];
        let _: () = msg_send![dot, setFrame: options_orb_dot_frame()];
        let _: () = msg_send![dot, setCornerRadius: PICKER_ORB_DOT_SIZE / 2.0];
        crate::ffi::layer_set_background(
            dot,
            crate::ffi::hex_to_cg_color(super::picker_accent(clipboard_palette())),
        );
        let _: () = msg_send![options_layer, addSublayer: dot];
        *OPTIONS_ORB_DOT.lock().unwrap() = Some(ObjPtr::new(dot));
    }
    let _: () = msg_send![header_strip, addSubview: options_button];
    release_obj(options_button);
    *PICKER_OPTIONS_ORB.lock().unwrap() = Some(ObjPtr::new(options_button));
    sync_options_orb_dot();

    // The pin button, left of the gear: the corner row's third glyph. It toggles the WHOLE-panel
    // pin (a different thing from the per-row ☆), which is why it lives with the panel-level
    // controls rather than among the rows. Its stateful glyph is the documented §10 exception.
    let pin_button: *mut AnyObject = msg_send![hover_button_class(), alloc];
    let pin_button: *mut AnyObject = msg_send![pin_button, initWithFrame: pin_frame];
    let _: () = msg_send![pin_button, setBordered: false];
    let _: () = msg_send![pin_button, setWantsLayer: true];
    let pin_layer: *mut AnyObject = msg_send![pin_button, layer];
    if !pin_layer.is_null() {
        let _: () = msg_send![pin_layer, setCornerRadius: PICKER_ORB_SIZE / 2.0];
        let _: () = msg_send![pin_layer, setMasksToBounds: true];
    }
    let pin_title = make_nsstring("");
    let _: () = msg_send![pin_button, setTitle: pin_title];
    CFRelease(pin_title as *const c_void);
    let _: () = msg_send![pin_button, setTarget: observer()];
    let _: () = msg_send![pin_button, setAction: sel!(togglePanelPin:)];
    set_pin_button_style(pin_button, false, false);
    add_hover_tracking(pin_button);
    let _: () = msg_send![header_strip, addSubview: pin_button];
    release_obj(pin_button);
    *PICKER_PIN_BUTTON.lock().unwrap() = Some(ObjPtr::new(pin_button));

    // The pause-recording orb, leftmost of the group: the panel's own entry point for stopping
    // recording without turning the feature off. Its glyph swaps (pause <-> filled play) so the
    // state is visible at a glance, and it drives the same persisted intent as the menu item and
    // the settings switch.
    let pause_button: *mut AnyObject = msg_send![hover_button_class(), alloc];
    let pause_button: *mut AnyObject = msg_send![pause_button, initWithFrame: pause_frame];
    let _: () = msg_send![pause_button, setBordered: false];
    let _: () = msg_send![pause_button, setWantsLayer: true];
    let pause_layer: *mut AnyObject = msg_send![pause_button, layer];
    if !pause_layer.is_null() {
        let _: () = msg_send![pause_layer, setCornerRadius: PICKER_ORB_SIZE / 2.0];
        let _: () = msg_send![pause_layer, setMasksToBounds: true];
    }
    let pause_title = make_nsstring("");
    let _: () = msg_send![pause_button, setTitle: pause_title];
    CFRelease(pause_title as *const c_void);
    let _: () = msg_send![pause_button, setTarget: observer()];
    let _: () = msg_send![pause_button, setAction: sel!(togglePauseRecording:)];
    set_settings_button_style(pause_button, false);
    add_hover_tracking(pause_button);
    let _: () = msg_send![header_strip, addSubview: pause_button];
    release_obj(pause_button);
    *PICKER_PAUSE_BUTTON.lock().unwrap() = Some(ObjPtr::new(pause_button));
    refresh_pause_button();

    release_obj(search);
    *SEARCH_FIELD.lock().unwrap() = Some(ObjPtr::new(search));
    // Text changes (including the system clear button / NSSearchField's Esc clear) filter live.
    let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
    let text_name = make_nsstring("NSControlTextDidChangeNotification");
    let _: () = msg_send![
        center,
        addObserver: observer(),
        selector: sel!(searchFieldChanged:),
        name: text_name,
        object: search
    ];
    CFRelease(text_name as *const c_void);
    // Focus style: preserve the frosted fill and strengthen only the inner ring; restore it
    // on blur.
    let begin_name = make_nsstring("NSControlTextDidBeginEditingNotification");
    let _: () = msg_send![
        center,
        addObserver: observer(),
        selector: sel!(searchFocusBegan:),
        name: begin_name,
        object: search
    ];
    CFRelease(begin_name as *const c_void);
    let end_name = make_nsstring("NSControlTextDidEndEditingNotification");
    let _: () = msg_send![
        center,
        addObserver: observer(),
        selector: sel!(searchFocusEnded:),
        name: end_name,
        object: search
    ];
    CFRelease(end_name as *const c_void);

    // Clear actions moved into the options popover (step C); their labels and
    // handlers stay live through `update_clear_action_labels`, which no-ops on no buttons.
    update_clear_action_labels();

    build_footer(content_parent, w);
    // The toast label (the new mockup's .toast): a dark rounded pill at the bottom center,
    // above the footer.
    let toast_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let toast_label: *mut AnyObject = msg_send![
        toast_label,
        initWithFrame: NSRect::new(
            NSPoint::new(200.0, 22.0),
            NSSize::new(120.0, 26.0)
        )
    ];
    let _: () = msg_send![toast_label, setBezeled: false];
    let _: () = msg_send![toast_label, setDrawsBackground: false];
    let _: () = msg_send![toast_label, setEditable: false];
    let _: () = msg_send![toast_label, setSelectable: false];
    let _: () = msg_send![toast_label, setAlignment: 1isize]; // Center on arm64
    let tf: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 11.5];
    let _: () = msg_send![toast_label, setFont: tf];
    let white = crate::glass::panel_ink(
        clipboard_palette().primary_text,
        crate::glass::PanelInk::Primary,
    );
    let _: () = msg_send![toast_label, setTextColor: white];
    let _: () = msg_send![toast_label, setWantsLayer: true];
    let tlayer: *mut AnyObject = msg_send![toast_label, layer];
    let tbg = crate::ffi::hex_to_ns_color(clipboard_palette().card_bg);
    crate::ffi::layer_set_background(tlayer, crate::ffi::ns_color_to_cg(tbg));
    let _: () = msg_send![tlayer, setCornerRadius: 10.0f64];
    let _: () = msg_send![toast_label, setHidden: true];
    let _: () = msg_send![content_parent, addSubview: toast_label];
    release_obj(toast_label);
    *TOAST_LABEL.lock().unwrap() = Some(ObjPtr::new(toast_label));

    // The blank-area drag surface goes on LAST so it is the topmost subview: every blank region of
    // the panel then accepts the gesture, while `hitTest:` declines the controls, the list viewport
    // and text, which keep the events they always had (see `drag.rs`).
    super::drag::install_drag_surface(content_parent);

    // Outside clicks (the picker resigns key) -> auto-hide. Same as Win+V: any click after
    // summoning dismisses the picker.
    let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
    let resign_name = make_nsstring("NSWindowDidResignKeyNotification");
    let _: () = msg_send![
        center,
        addObserver: observer(),
        selector: sel!(clipboardWindowResigned:),
        name: resign_name,
        object: window
    ];
    CFRelease(resign_name as *const c_void);
    let become_key_name = make_nsstring("NSWindowDidBecomeKeyNotification");
    let _: () = msg_send![
        center,
        addObserver: observer(),
        selector: sel!(clipboardWindowBecameKey:),
        name: become_key_name,
        object: window
    ];
    CFRelease(become_key_name as *const c_void);
    *PICKER_CONTAINER.lock().unwrap() = Some(ObjPtr::new(container));
    *PICKER_WINDOW.lock().unwrap() = Some(ObjPtr::new(window));
}

/// Per-row build counters accumulated for the slow-path log (same accounting as before).
#[derive(Default, Clone, Copy)]
struct RowCreateStats {
    image_rows: usize,
    code_rows: usize,
    image_ms: u128,
    content_attributed_ms: u128,
    meta_ms: u128,
}

/// The per-row creation parameters (grouped so `create_row_views` stays readable).
struct RowSpec<'a> {
    entry: &'a ClipEntry,
    display_index: usize,
    y: f64,
    row_h: f64,
    has_header: bool,
    selected: bool,
    hovered: bool,
}

/// The row backdrop wash for one selection/hover combination, shared by the row-build path
/// (`create_row_views`) and the runtime repaint (`update_hover_visuals`) so the two cannot
/// drift again. The selection owns the backdrop: when one row is both selected and hovered
/// (arrow navigation repaints the new selection as hovered; the pointer can rest on the
/// selected row), the selection wash must win. Pure so the precedence is unit-assertable.
///
/// An idle row is the panel scrim rather than transparent: the row is where this panel's text and
/// icon buttons live, and the panel behind them is the user's material (design-style §3), so the
/// row carries the surface the contrast table is evaluated against. The material still shows in the
/// panel margins, the search/tab band and the footer.
pub(super) fn row_backdrop_wash(
    palette: crate::theme::UiPalette,
    selected: bool,
    hovered: bool,
) -> u32 {
    if selected {
        super::picker_row_selected(palette)
    } else if hovered {
        super::picker_row_hover(palette)
    } else {
        // An idle row paints nothing: the panel's material is the surface (its text stays legible through
        // the ink `glass::panel_ink` picks), and a surface here would band against the material wherever a
        // row does not cover the panel.
        0x00000000
    }
}

/// Create one row (optional group header + tile + content/meta buttons + 3 action buttons),
/// attach it to the container, and return its hover-related views. Extracted from
/// `rebuild_rows` so scroll-time incremental materialization (`sync_visible_rows`) can reuse
/// it and create only the rows newly entering the viewport instead of tearing down and
/// rebuilding the whole visible set.
unsafe fn create_row_views(
    container: *mut AnyObject,
    spec: RowSpec<'_>,
    stats: &mut RowCreateStats,
) -> RowHoverViews {
    let RowSpec {
        entry,
        display_index,
        y,
        row_h,
        has_header,
        selected,
        hovered,
    } = spec;
    let i = display_index;
    let palette = clipboard_palette();
    let row_w = picker_list_width() - PAD_X * 2.0;
    let hdr_h = if has_header { GROUP_H } else { 0.0 };
    let content_y = y + hdr_h;

    // The group header: 11px medium text, 27px tall, centered.
    let mut row_group_label = None;
    let row_group_clear = None;
    if has_header {
        let g_label = make_nsstring(&group_label(day_group(entry.copied_at)));
        let g_frame = NSRect::new(
            NSPoint::new(PAD_X + ROW_PAD_L, y + GROUP_LABEL_PAD),
            NSSize::new(row_w - PAD_X, GROUP_H - GROUP_LABEL_PAD),
        );
        // The header sits in the gap between rows, which is material. A text-local surface under
        // the label rides the material like every other label (see `glass::panel_ink`).
        let g: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let g: *mut AnyObject = msg_send![g, initWithFrame: g_frame];
        let _: () = msg_send![g, setStringValue: g_label];
        CFRelease(g_label as *const c_void);
        let _: () = msg_send![g, setBezeled: false];
        let _: () = msg_send![g, setDrawsBackground: false];
        let _: () = msg_send![g, setEditable: false];
        let _: () = msg_send![g, setSelectable: false];
        let g_font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CAPTION, weight: crate::theme::FONT_WEIGHT_REGULAR];
        let _: () = msg_send![g, setFont: g_font];
        let g_color = crate::glass::panel_ink(palette.muted_text, crate::glass::PanelInk::Muted);
        let _: () = msg_send![g, setTextColor: g_color];
        let _: () = msg_send![container, addSubview: g];
        release_obj(g);
        row_group_label = Some(ObjPtr::new(g));

        // Clearing history lives in the options menu alongside the other housekeeping actions.
    }

    // The row backdrop (two distinct styles): hovered (not selected) = the palette's hover
    // wash with NO bar; selected = the selection wash + a 2px left bar. The new mockup's
    // .item:hover vs .item.selected. update_hover_visuals repaints this same pair at runtime
    // through row_backdrop_wash.
    let tile: *mut AnyObject = msg_send![class!(NSView), alloc];
    let tile: *mut AnyObject = msg_send![
        tile,
        initWithFrame: NSRect::new(NSPoint::new(PAD_X, content_y), NSSize::new(row_w, row_h))
    ];
    let _: () = msg_send![tile, setWantsLayer: true];
    let tile_layer: *mut AnyObject = msg_send![tile, layer];
    let bg_hex = row_backdrop_wash(palette, selected, hovered);
    // layer_set_background goes through raw objc_msgSend: objc2's msg_send! can't encode
    // CGColor args/returns ('^{CGColor=}' vs '^v').
    crate::ffi::layer_set_background(tile_layer, crate::ffi::hex_to_cg_color(bg_hex));
    let _: () = msg_send![tile_layer, setCornerRadius: SEL_TILE_R];
    let _: () = msg_send![container, addSubview: tile];
    release_obj(tile);

    // The content button: the whole row, one truncated line -- the v2 mock's `.clip-row`
    // (40px, single title line; the meta line moved to the detail pane's meta table).
    // Borderless, backgroundless.
    let content_x = PAD_X + ROW_PAD_L;
    let content_w = model::row_content_width(row_w);
    let content_h = row_h;
    let content_btn: *mut AnyObject = msg_send![row_button_class(), alloc];
    let is_image = entry.image.is_some();
    if is_image {
        stats.image_rows += 1;
    }
    let content_btn: *mut AnyObject = msg_send![
        content_btn,
        initWithFrame: NSRect::new(
            NSPoint::new(content_x, content_y + ROW_PAD_TOP),
            NSSize::new(content_w, content_h - ROW_PAD_TOP - ROW_PAD_BOT)
        )
    ];
    let _: () = msg_send![content_btn, setBordered: false];
    let _: () = msg_send![content_btn, setAlignment: -1isize]; // NSTextAlignmentNatural
    let cell: *mut AnyObject = msg_send![content_btn, cell];
    let _: () = msg_send![cell, setUsesSingleLineMode: true];
    let _: () = msg_send![cell, setLineBreakMode: 4isize]; // NSLineBreakByTruncatingTail
    if msg_send![cell, respondsToSelector: sel!(setMaximumNumberOfLines:)] {
        let _: () = msg_send![cell, setMaximumNumberOfLines: 1isize];
    }
    let image_started = Instant::now();
    let row_img = make_row_image(entry, content_w);
    stats.image_ms += image_started.elapsed().as_millis();
    let has_preview = !row_img.is_null();
    if has_preview {
        let _: () = msg_send![content_btn, setImage: row_img];
        // The preview already fills the content column, so the row is image-only: a title
        // would fight it for the same space (NSImageLeft + a filename pushed past the edge).
        // Falling back to the text title remains the degenerate-image path below.
        let _: () = msg_send![content_btn, setImagePosition: 1isize]; // NSImageOnly
        release_obj(row_img);
    }
    if !has_preview {
        // Keep the full string and let the native cell wrap/truncate using actual font metrics.
        // Character-count heuristics break on emoji, combining marks, and long unbroken words.
        let content = entry.text.as_str();
        let kind = if is_image {
            TextKind::Plain
        } else {
            classify_text(&entry.text)
        };
        if kind == TextKind::Code {
            stats.code_rows += 1;
        }
        let content_attributed_started = Instant::now();
        let attr = make_content_attributed(content, kind);
        stats.content_attributed_ms += content_attributed_started.elapsed().as_millis();
        let _: () = msg_send![content_btn, setAttributedTitle: attr];
        release_obj(attr);
    }
    let _: () = msg_send![content_btn, setTag: i as isize];
    let _: () = msg_send![content_btn, setTarget: row_target()];
    let _: () = msg_send![content_btn, setAction: sel!(handleClipboardRowClick:)];
    add_hover_tracking(content_btn);
    let _: () = msg_send![container, addSubview: content_btn];
    release_obj(content_btn);

    // Action buttons (detail ▤ · pin ☆/★ · delete ⌫), trailing edge right-to-left: ALWAYS visible
    // on PINNED entries; on unpinned entries they appear only when the row is hovered or selected
    // (the mock's `.row-actions` opacity 0.62->1). Vertically centred on the single-line row; they
    // never paste. The detail toggle sits left of the pin: it is a per-row control, so each row
    // opens its own detail rather than the header acting on whichever row is selected.
    let act_alpha = if entry.pinned || selected || hovered {
        1.0
    } else {
        0.62
    };
    let act_y = content_y + (row_h - ACTION_H) / 2.0;
    let x_del = picker_list_width() - PAD_X - ROW_PAD_R - ACTION_BTN;
    let x_pin = x_del - ACTION_GAP - ACTION_BTN;
    let x_detail = x_pin - ACTION_GAP - ACTION_BTN;
    let detail_active = detail_action_is_active(detail_visible(), picker_selection(), i);
    let detail_btn = make_action_button(
        "",
        sel!(showItemDetails:),
        i as isize,
        x_detail,
        act_y,
        act_alpha,
    );
    if !detail_btn.is_null() {
        set_detail_action_style(detail_btn, detail_active, false);
        // It opens THIS row's detail, and the → still works: the shared "Show details (→)" names both.
        let label = make_nsstring(&t("clipboard.detail_toggle"));
        let _: () = msg_send![detail_btn, setAccessibilityLabel: label];
        CFRelease(label as *const c_void);
        let tip = make_nsstring(&t("clipboard.detail_toggle"));
        let _: () = msg_send![detail_btn, setToolTip: tip];
        CFRelease(tip as *const c_void);
        let _: () = msg_send![container, addSubview: detail_btn];
        release_obj(detail_btn);
    }
    let pin_sym = if entry.pinned { "★" } else { "☆" };
    let pin_btn = make_action_button(
        pin_sym,
        sel!(togglePin:),
        i as isize,
        x_pin,
        act_y,
        act_alpha,
    );
    if !pin_btn.is_null() {
        let _: () = msg_send![container, addSubview: pin_btn];
        release_obj(pin_btn);
    }
    let del_btn = make_action_button("⌫", sel!(deleteEntry:), i as isize, x_del, act_y, act_alpha);
    if !del_btn.is_null() {
        let _: () = msg_send![container, addSubview: del_btn];
        release_obj(del_btn);
    }

    RowHoverViews {
        group_label: row_group_label,
        group_clear: row_group_clear,
        tile: ObjPtr::new(tile),
        content: ObjPtr::new(content_btn),
        detail: ObjPtr::new(detail_btn),
        pin: ObjPtr::new(pin_btn),
        del: ObjPtr::new(del_btn),
    }
}

/// Remove one row's views from the container (the bar is a tile subview and goes with it, and the
/// per-row view list comes from `RowHoverViews::owned`, so nothing a row created can be left behind).
unsafe fn remove_row_views(view: &RowHoverViews) {
    for v in view.owned().into_iter().flatten() {
        if !v.0.is_null() {
            let _: () = msg_send![v.0, removeFromSuperview];
        }
    }
}

/// Insert a materialized row by index (ROW_VIEW_INDICES and ROW_HOVER_VIEWS stay parallel
/// and ascending). Lock order matches `row_view_for_display_index`: indices before views.
fn insert_materialized_row(index: usize, view: RowHoverViews) {
    let mut indices = ROW_VIEW_INDICES.lock().unwrap();
    let pos = indices.partition_point(|&i| i < index);
    if pos < indices.len() && indices[pos] == index {
        return;
    }
    let mut views = ROW_HOVER_VIEWS.lock().unwrap();
    indices.insert(pos, index);
    views.insert(pos, view);
}

/// Remove a materialized row and release its views; no-op when absent.
unsafe fn remove_materialized_row(index: usize) {
    let view = {
        let mut indices = ROW_VIEW_INDICES.lock().unwrap();
        let Some(pos) = indices.iter().position(|&i| i == index) else {
            return;
        };
        indices.remove(pos);
        let mut views = ROW_HOVER_VIEWS.lock().unwrap();
        views.remove(pos)
    };
    remove_row_views(&view);
}

/// Rebuild the row buttons from history (selected row highlighted with a rounded tile).
/// The empty-state hint text for the current model: an empty history, or a query/filter with no
/// matches. Empty when there are rows to show. Locks the history, so callers already holding that
/// lock (`rebuild_rows`) pass their own text to `layout_empty_state` instead.
fn empty_state_hint() -> String {
    // The storage failure outranks the ordinary empty list: while the feature cannot work, the
    // panel says why instead of looking like a history nobody has used yet.
    if let Some(unavailable) = unavailable_copy() {
        return unavailable;
    }
    let total = CLIP_HISTORY.lock().unwrap().len();
    if total == 0 {
        t("clipboard.empty")
    } else if with_clipboard_ui(|ui| ui.filtered.is_empty()) {
        t("clipboard.no_match")
    } else {
        String::new()
    }
}

/// (Re)lays out the empty-state hint against the live viewport: creates the label when missing,
/// otherwise re-centers it and refreshes its text (which flips between "no history" and "no
/// matches").
///
/// The vertical center depends on the visible height, and that height only settles once the summon
/// has applied the final window frame. `show_picker` may reuse a cached row tree when its key
/// matches -- and the key carries no geometry -- while a rebuild can also run at a different height
/// (the clear actions rebuild in place, before the summon shrinks the panel). So the position must
/// be re-derived per presentation instead of being frozen at rebuild time: measured symptom of
/// freezing it was an empty picker whose hint sat just below the 273pt viewport of the minimal
/// panel, with a phantom scroll indicator for a document 583pt tall.
pub(super) unsafe fn layout_empty_state(hint: &str) -> bool {
    if hint.is_empty() {
        return false;
    }
    let Some(container) = picker_container_ptr() else {
        return false;
    };
    // Use the clip view's live visible height, not the minimum window height: filtering can leave
    // the picker tall with no results, and the hint should stay centered in what is on screen.
    let clip: *mut AnyObject = msg_send![container, superview];
    let visible_h = if clip.is_null() {
        picker_min_height() - header_strip_h() - FOOTER_H
    } else {
        let bounds: NSRect = msg_send![clip, bounds];
        bounds.size.height
    };
    let doc_h = empty_state_doc_height(visible_h);
    let _: () = msg_send![container, setFrameSize: NSSize::new(picker_list_width(), doc_h)];
    // While the storage is unavailable the panel carries the path out of it: the action the state
    // admits (the same one the settings banner offers), centred under the sentence that says why
    // nothing can be recorded. The sentence and the button are centred as one block, so the panel
    // does not look top-heavy.
    let action = storage_action();
    let show_action = action != StorageAction::None;
    let button_h = 26.0;
    let block_gap = 10.0;
    // The hint: vertically centered within the visible list area (with the button, when present).
    let label_h = 40.0;
    let block_h = 55.0
        + if show_action {
            label_h + block_gap + button_h
        } else {
            label_h
        };
    // The container is flipped (y grows downward), so the sentence sits at the top of the block and
    // the way out under it -- reading order first, then the action.
    let block_y = (doc_h - block_h) / 2.0;
    let label_y = block_y + 55.0;
    layout_empty_state_action(
        container,
        label_y
            + if show_action {
                label_h + block_gap
            } else {
                0.0
            },
        action,
    );
    let existing_icon = *EMPTY_STATE_ICON.lock().unwrap();
    let icon = if let Some(icon) = existing_icon {
        icon.0
    } else {
        let icon: *mut AnyObject = msg_send![class!(NSImageView), new];
        let _: () = msg_send![icon, setWantsLayer: true];
        let _: () = msg_send![container, addSubview: icon];
        release_obj(icon);
        *EMPTY_STATE_ICON.lock().unwrap() = Some(ObjPtr::new(icon));
        icon
    };
    let symbol = make_nsstring(if search_has_query() {
        "magnifyingglass"
    } else {
        "doc.on.clipboard"
    });
    let image: *mut AnyObject = msg_send![class!(NSImage), imageWithSystemSymbolName: symbol, accessibilityDescription: std::ptr::null::<AnyObject>()];
    CFRelease(symbol as *const c_void);
    if !image.is_null() {
        let config: *mut AnyObject = msg_send![class!(NSImageSymbolConfiguration), configurationWithPointSize: 19.0f64, weight: crate::theme::FONT_WEIGHT_REGULAR];
        let image: *mut AnyObject = msg_send![image, imageWithSymbolConfiguration: config];
        let _: () = msg_send![icon, setImage: image];
    }
    let _: () = msg_send![icon, setFrame: NSRect::new(NSPoint::new((picker_list_width() - 46.0) / 2.0, block_y), NSSize::new(46.0, 46.0))];
    let layer: *mut AnyObject = msg_send![icon, layer];
    let _: () = msg_send![layer, setCornerRadius: 23.0f64];
    crate::ffi::layer_set_background(layer, crate::ffi::hex_to_cg_color(picker_thumb_fill()));
    let frame = NSRect::new(
        NSPoint::new(PAD_X, label_y),
        NSSize::new(picker_list_width() - PAD_X * 2.0, label_h),
    );
    let hint_ns = make_nsstring(hint);
    let existing = *EMPTY_STATE_VIEW.lock().unwrap();
    if let Some(label) = existing {
        if !label.0.is_null() {
            let _: () = msg_send![label.0, setFrame: frame];
            let _: () = msg_send![label.0, setStringValue: hint_ns];
            CFRelease(hint_ns as *const c_void);
            return true;
        }
    }
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let label: *mut AnyObject = msg_send![label, initWithFrame: frame];
    // NOTE (load-bearing): on Apple Silicon TARGET_ABI_USES_IOS_VALUES=1, so
    // NSTextAlignment uses the iOS values -- Center=1, Right=2 (reversed vs classic
    // Mac). 1 is required here for centering; 2 renders right-aligned (a past
    // regression).
    let _: () = msg_send![label, setAlignment: 1isize]; // Center on arm64
    let _: () = msg_send![label, setStringValue: hint_ns];
    CFRelease(hint_ns as *const c_void);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    // The empty state follows the new mockup's .empty-state: 12px, 30% black.
    let text_color = crate::glass::panel_ink(
        clipboard_palette().muted_text,
        crate::glass::PanelInk::Muted,
    );
    let _: () = msg_send![label, setTextColor: text_color];
    let font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CAPTION];
    let _: () = msg_send![label, setFont: font];
    // The empty state is the only content on an otherwise empty panel, so its surface is what the
    // reader sees; the material is the panel's surface.
    let _: () = msg_send![container, addSubview: label];
    release_obj(label);
    *EMPTY_STATE_VIEW.lock().unwrap() = Some(ObjPtr::new(label));
    true
}

/// Place (or hide) the empty state's storage action at `y`, centred.
///
/// Created once and reused, like the hint label: the panel is a long-lived view tree and the button
/// has to survive re-layouts. `StorageAction::None` only hides it — the healthy and purge-pending
/// states offer nothing, and the label above keeps the space otherwise. The title and the selector
/// are re-applied on every layout, because the SAME button serves both actions and the state can
/// change between two layouts (a repaired history and a blocked one must not keep each other's
/// action).
unsafe fn layout_empty_state_action(container: *mut AnyObject, y: f64, action: StorageAction) {
    let (title, selector) = match action {
        StorageAction::GrantKeychain => (
            t("clipboard.grant_keychain_access"),
            sel!(retryKeychainAccess:),
        ),
        StorageAction::ClearAndRestart => (
            t("clipboard.clear_and_restart"),
            sel!(clearClipboardStorage:),
        ),
        StorageAction::None => (String::new(), sel!(retryKeychainAccess:)),
    };
    let show = action != StorageAction::None;
    let applied = match action {
        StorageAction::GrantKeychain => AppliedAction::Grant,
        StorageAction::ClearAndRestart => AppliedAction::Clear,
        StorageAction::None => AppliedAction::Hidden,
    };
    let existing = *EMPTY_STATE_ACTION.lock().unwrap();
    if let Some(button) = existing {
        if !button.0.is_null() {
            let _: () = msg_send![button.0, setHidden: !show];
            if show {
                let ns = make_nsstring(&title);
                let _: () = msg_send![button.0, setTitle: ns];
                CFRelease(ns as *const c_void);
                let _: () = msg_send![button.0, setAction: selector];
                let width = localized_string_width(&title, crate::theme::FONT_CAPTION) + 28.0;
                let frame = NSRect::new(
                    NSPoint::new((picker_list_width() - width) / 2.0, y),
                    NSSize::new(width, 26.0),
                );
                let _: () = msg_send![button.0, setFrame: frame];
                // Re-apply the palette too: the same button serves the keychain action and the
                // destructive clear-and-restart, and the destructive label must not survive the
                // change (nor the plain one cover a destructive action).
                set_clear_confirmation_button_style(button.0, false);
            }
            record_applied_action(NoticeSurface::PickerEmptyState, applied);
            return;
        }
    }
    if !show {
        // No button object exists in this process: "absent", which is a different fact from a
        // button that exists with its action hidden.
        record_applied_action(NoticeSurface::PickerEmptyState, AppliedAction::Absent);
        return;
    }
    let width = localized_string_width(&title, crate::theme::FONT_CAPTION) + 28.0;
    let frame = NSRect::new(
        NSPoint::new((picker_list_width() - width) / 2.0, y),
        NSSize::new(width, 26.0),
    );
    let button: *mut AnyObject = msg_send![hover_button_class(), alloc];
    let button: *mut AnyObject = msg_send![button, initWithFrame: frame];
    let ns = make_nsstring(&title);
    let _: () = msg_send![button, setTitle: ns];
    CFRelease(ns as *const c_void);
    let _: () = msg_send![button, setBordered: false];
    let _: () = msg_send![button, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![button, layer];
    if !layer.is_null() {
        let _: () = msg_send![layer, setCornerRadius: crate::theme::RADIUS_CONTROL];
    }
    let font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CAPTION];
    let _: () = msg_send![button, setFont: font];
    let _: () = msg_send![button, setTarget: observer()];
    let _: () = msg_send![button, setAction: selector];
    // The clear actions' palette is the panel's bordered-control look (fill + label ink), so the
    // button reads as a control on the material rather than as chrome.
    set_clear_confirmation_button_style(button, false);
    let _: () = msg_send![container, addSubview: button];
    release_obj(button);
    *EMPTY_STATE_ACTION.lock().unwrap() = Some(ObjPtr::new(button));
    record_applied_action(NoticeSurface::PickerEmptyState, applied);
}

/// Summon-time entry point: re-derive the empty-state layout for the viewport the summon just
/// applied. No-op when the display list has rows.
pub(super) unsafe fn refresh_empty_state_layout() {
    layout_empty_state(&empty_state_hint());
}

/// Smoke assertion: the empty-state hint exists and sits inside the visible list area, and the
/// document is not taller than the viewport (nothing to scroll in an empty list).
pub(super) unsafe fn empty_state_layout_is_sane() -> bool {
    let Some(label) = *EMPTY_STATE_VIEW.lock().unwrap() else {
        return false;
    };
    let Some(container) = picker_container_ptr() else {
        return false;
    };
    let clip: *mut AnyObject = msg_send![container, superview];
    if clip.is_null() || label.0.is_null() {
        return false;
    }
    let clip_bounds: NSRect = msg_send![clip, bounds];
    let frame: NSRect = msg_send![label.0, frame];
    let doc: NSRect = msg_send![container, frame];
    let visible_top = clip_bounds.origin.y;
    let visible_bottom = visible_top + clip_bounds.size.height;
    let inside = frame.origin.y >= visible_top - 0.5
        && frame.origin.y + frame.size.height <= visible_bottom + 0.5;
    let no_phantom_overflow = doc.size.height <= clip_bounds.size.height + 0.5;
    log_debug!(
        "[clip] empty-state: label_y={:.1} h={:.1} visible=[{:.1},{:.1}] doc_h={:.1} inside={} no_overflow={}",
        frame.origin.y,
        frame.size.height,
        visible_top,
        visible_bottom,
        doc.size.height,
        inside,
        no_phantom_overflow
    );
    inside && no_phantom_overflow
}

pub(super) unsafe fn rebuild_rows() -> Option<PickerTimingSummary> {
    let rebuild_started = Instant::now();
    let hist = CLIP_HISTORY.lock().unwrap();
    let container = picker_container_ptr()?;
    // Rebuild tears down the old rows and gates enter/exit events; discard the old index first
    // so it cannot land on an unrelated new row.
    *HOVER_ROW.lock().unwrap() = NO_SELECTION;
    // Ignore mouseEntered during the rebuild (see the REBUILDING note).
    REBUILDING.store(true, Ordering::SeqCst);
    // Record the current scroll offset (the clip view's bounds origin y in flipped coords)
    // and restore it after the rebuild, so hover/arrow rebuilds don't snap the viewport.
    let scroll_offset = {
        let clip: *mut AnyObject = msg_send![container, superview];
        if clip.is_null() {
            0.0
        } else {
            let b: NSRect = msg_send![clip, bounds];
            b.origin.y
        }
    };

    // Note: the button's alloc +1 was released after addSubview (owned by the parent view);
    // removeFromSuperview drops the parent's reference (refcount hits zero, object deallocs),
    // so it must NOT be released again -- a second release was a use-after-free that crashed
    // on the second summon.
    let remove_old_started = Instant::now();
    // Row views are parent-owned; removeFromSuperview releases them and they must never be
    // released again (a past UAF lesson).
    {
        let old_views = ROW_HOVER_VIEWS.lock().unwrap().clone();
        for view in &old_views {
            remove_row_views(view);
        }
    }
    ROW_HOVER_VIEWS.lock().unwrap().clear();
    ROW_VIEW_INDICES.lock().unwrap().clear();
    let empty_icon = EMPTY_STATE_ICON.lock().unwrap().take();
    if let Some(icon) = empty_icon {
        let _: () = msg_send![icon.0, removeFromSuperview];
    }
    if let Some(empty) = EMPTY_STATE_VIEW.lock().unwrap().take() {
        if !empty.0.is_null() {
            let _: () = msg_send![empty.0, removeFromSuperview];
        }
    }
    let mut pitches = ROW_PITCHES.lock().unwrap();
    pitches.clear();
    let remove_old_ms = remove_old_started.elapsed().as_millis();

    let prepare_started = Instant::now();
    let total = hist.len();
    // The footer's entry count follows the history (the search placeholder is now a
    // static string).
    refresh_footer_count(total);
    // Rebuild the display list (filtered by the query AND the kind filter).
    let query = with_clipboard_ui(|ui| ui.search_query.clone());
    let filter = *CLIP_FILTER.lock().unwrap();
    let filtered_indices = filtered_indices(&hist, &query, filter);
    with_clipboard_ui(|ui| ui.filtered = filtered_indices);
    let filtered = with_clipboard_ui(|ui| ui.filtered.clone());
    // Each row's button height / pitch derives from its wrapped line count. The sequence has
    // to be the FILTERED one: image rows are taller than text rows, so a pitch index that
    // aligned under a uniform height would read the wrong entry's row once a filter drops
    // entries.
    *pitches = compute_pitches_for(filtered.iter().map(|&i| &hist[i]));
    let show_source = show_source_app();

    // Clamp the selection into the fresh display list (out of range -> the tail;
    // NO_SELECTION untouched) so every rebuild path self-heals -- fixes the lost highlight
    // after deleting the last row (the delete paths used to clamp against the stale
    // pre-delete FILTERED / history lengths, leaving the selection past the new list).
    {
        let sel = picker_selection();
        set_picker_selection(clamp_selection(sel, filtered.len()));
    }

    // Empty state: empty history -> "no history"; a query with no matches -> "no match".
    // Both share the same hint rendering.
    // The storage failure outranks the ordinary empty text, exactly like `empty_state_hint` (which
    // cannot be called here: this path already holds the history lock). An unavailable session
    // records nothing, so a non-empty list cannot coexist with it.
    let empty_hint = if let Some(unavailable) = unavailable_copy() {
        unavailable
    } else if total == 0 {
        t("clipboard.empty")
    } else if filtered.is_empty() {
        t("clipboard.no_match")
    } else {
        String::new()
    };
    let prepare_ms = prepare_started.elapsed().as_millis();
    let build_rows_started = Instant::now();
    let mut stats = RowCreateStats::default();
    let mut slowest_row_ms = 0;
    let mut slowest_row_index = None;
    if !empty_hint.is_empty() {
        layout_empty_state(&empty_hint);
        let build_rows_ms = build_rows_started.elapsed().as_millis();
        let finalize_started = Instant::now();
        let rendered_key = picker_rows_key(history_revision(), &query, filter, show_source);
        let finalize_ms = finalize_started.elapsed().as_millis();
        let summary = PickerTimingSummary {
            elapsed_ms: rebuild_started.elapsed().as_millis(),
            history_len: total,
            filtered_len: filtered.len(),
            remove_old_ms,
            prepare_ms,
            build_rows_ms,
            finalize_ms,
            empty: true,
            ..PickerTimingSummary::default()
        };
        with_clipboard_ui(|ui| {
            ui.rendered_rows = Some(rendered_key);
            ui.last_rebuild_timing = Some(summary);
        });
        REBUILDING.store(false, Ordering::SeqCst);
        if summary.elapsed_ms >= CLIPBOARD_SLOW_PATH_MS {
            log_debug!(
                "[clip] picker_rebuild_slow elapsed_ms={} history_len={} filtered_len={} image_rows={} code_rows={} reused=false remove_old_ms={} prepare_ms={} build_rows_ms={} image_ms={} content_attributed_ms={} meta_ms={} finalize_ms={} slowest_row_ms=0 slowest_row_index=none empty=true",
                summary.elapsed_ms,
                summary.history_len,
                summary.filtered_len,
                summary.image_rows,
                summary.code_rows,
                summary.remove_old_ms,
                summary.prepare_ms,
                summary.build_rows_ms,
                summary.image_ms,
                summary.content_attributed_ms,
                summary.meta_ms,
                summary.finalize_ms,
            );
        }
        return Some(summary);
    }

    // The empty state's way-out button belongs to the failure, and the failure can leave while rows
    // are present (a retry that succeeds, or a history that turns out to be readable): the empty-state
    // layout below is skipped for a non-empty list, so the button is hidden here as well -- otherwise
    // it would keep its old frame over the rows.
    layout_empty_state_action(container, 0.0, StorageAction::None);

    // Document height covers ALL displayed entries (the scrollable area). Floored at the
    // visible height (the window minus the header strip): a document shorter than the
    // visible area hangs off the clip view's bottom (the clip isn't flipped), pushing the
    // rows against the bottom edge.
    let doc_h = (rows_top_offset() + pitches.iter().take(filtered.len()).sum::<f64>() + PAD_Y)
        .max(picker_min_height() - header_strip_h() - FOOTER_H);
    let _: () = msg_send![container, setFrameSize: NSSize::new(picker_list_width(), doc_h)];
    let (visible_start, visible_end) = picker_visible_row_range(&pitches, filtered.len());

    let sel_idx = picker_selection();
    // The hovered row (independent of the selection: with keyboard navigation the mouse
    // may park on another row -> both states coexist, like the mockup's :hover/.selected).
    // Hover gate: when the pointer is outside the picker window a hover cannot be valid
    // (enter/exited events may be suppressed by REBUILDING or swallowed across panels,
    // freezing HOVER_ROW into a stale value) -- render without hover and reset the static
    // to self-heal, otherwise rebuilds would paint phantom hover fills onto fresh rows.
    let mut hover_idx = *HOVER_ROW.lock().unwrap();
    if !unsafe { pointer_in_picker_window() } {
        hover_idx = effective_hover_row(false, hover_idx);
        *HOVER_ROW.lock().unwrap() = hover_idx;
    }
    let mut prev_group: Option<DayGroup> = visible_start
        .checked_sub(1)
        .and_then(|index| filtered.get(index))
        .map(|&history_index| day_group(hist[history_index].copied_at));
    let offsets = row_offsets(&pitches);
    for (i, &h_idx) in filtered.iter().enumerate() {
        if i < visible_start || i >= visible_end {
            continue;
        }
        let row_started = Instant::now();
        let y = offsets[i];
        let entry = &hist[h_idx];
        let selected = i == sel_idx;
        let hovered = i == hover_idx;
        let group = day_group(entry.copied_at);
        let has_hdr = prev_group.is_none() || prev_group != Some(group);
        prev_group = Some(group);
        let hdr_h = if has_hdr { GROUP_H } else { 0.0 };
        let row_h = pitches[i] - hdr_h;
        let view = create_row_views(
            container,
            RowSpec {
                entry,
                display_index: i,
                y,
                row_h,
                has_header: has_hdr,
                selected,
                hovered,
            },
            &mut stats,
        );
        ROW_HOVER_VIEWS.lock().unwrap().push(view);
        ROW_VIEW_INDICES.lock().unwrap().push(i);
        let row_ms = row_started.elapsed().as_millis();
        if row_ms > slowest_row_ms {
            slowest_row_ms = row_ms;
            slowest_row_index = Some(i);
        }
    }

    // restore the scroll position.
    if scroll_offset > 0.0 {
        let _: () = msg_send![container, scrollPoint: NSPoint::new(0.0, scroll_offset)];
    }
    let build_rows_ms = build_rows_started.elapsed().as_millis();
    let finalize_started = Instant::now();
    let rendered_key = picker_rows_key(history_revision(), &query, filter, show_source);
    let finalize_ms = finalize_started.elapsed().as_millis();
    let summary = PickerTimingSummary {
        elapsed_ms: rebuild_started.elapsed().as_millis(),
        history_len: total,
        filtered_len: filtered.len(),
        image_rows: stats.image_rows,
        code_rows: stats.code_rows,
        remove_old_ms,
        prepare_ms,
        build_rows_ms,
        image_ms: stats.image_ms,
        content_attributed_ms: stats.content_attributed_ms,
        meta_ms: stats.meta_ms,
        finalize_ms,
        slowest_row_ms,
        slowest_row_index,
        empty: false,
    };
    with_clipboard_ui(|ui| {
        ui.rendered_rows = Some(rendered_key);
        ui.last_rebuild_timing = Some(summary);
    });
    REBUILDING.store(false, Ordering::SeqCst);
    if summary.elapsed_ms >= CLIPBOARD_SLOW_PATH_MS {
        let slowest_row_index = summary
            .slowest_row_index
            .map_or_else(|| "none".to_owned(), |index| index.to_string());
        log_debug!(
            "[clip] picker_rebuild_slow elapsed_ms={} history_len={} filtered_len={} image_rows={} code_rows={} reused=false remove_old_ms={} prepare_ms={} build_rows_ms={} image_ms={} content_attributed_ms={} meta_ms={} finalize_ms={} slowest_row_ms={} slowest_row_index={} empty=false",
            summary.elapsed_ms,
            summary.history_len,
            summary.filtered_len,
            summary.image_rows,
            summary.code_rows,
            summary.remove_old_ms,
            summary.prepare_ms,
            summary.build_rows_ms,
            summary.image_ms,
            summary.content_attributed_ms,
            summary.meta_ms,
            summary.finalize_ms,
            summary.slowest_row_ms,
            slowest_row_index,
        );
    }
    Some(summary)
}

/// Scroll-time incremental materialization: create only the rows newly entering the viewport
/// and remove the ones leaving it, instead of tearing down and rebuilding the whole visible
/// set. Rows are absolutely positioned by display index (subviews scroll natively with the
/// NSScrollView), so surviving rows need no reposition; content/selection/hover are unchanged,
/// so they need no rebuild. Returns whether anything changed.
pub(super) unsafe fn sync_visible_rows() -> bool {
    let Some(container) = picker_container_ptr() else {
        return false;
    };
    let pitches = ROW_PITCHES.lock().unwrap().clone();
    let filtered = with_clipboard_ui(|ui| ui.filtered.clone());
    if pitches.is_empty() || filtered.is_empty() {
        return false;
    }
    let (start, end) = picker_visible_row_range(&pitches, filtered.len());
    {
        // The materialized range is unchanged (ascending contiguous) -> nothing to add/remove.
        let indices = ROW_VIEW_INDICES.lock().unwrap();
        if indices.len() == end.saturating_sub(start)
            && indices.first().copied() == Some(start)
            && indices.last().map(|&last| last + 1) == Some(end)
        {
            return false;
        }
    }
    REBUILDING.store(true, Ordering::SeqCst);
    // Remove rows that left the viewport.
    let stale: Vec<usize> = {
        let indices = ROW_VIEW_INDICES.lock().unwrap();
        indices
            .iter()
            .copied()
            .filter(|&index| index < start || index >= end)
            .collect()
    };
    for index in stale {
        remove_materialized_row(index);
    }
    // Create rows newly entering the viewport.
    let hist = CLIP_HISTORY.lock().unwrap();
    let offsets = row_offsets(&pitches);
    let sel_idx = picker_selection();
    // The same hover gate as rebuild_rows: when the pointer is outside the picker the stale
    // index cannot be valid -- build without hover and self-heal the static, otherwise
    // scroll-time materialization would paint a phantom hover fill onto fresh rows.
    let mut hover_idx = *HOVER_ROW.lock().unwrap();
    if !unsafe { pointer_in_picker_window() } {
        hover_idx = effective_hover_row(false, hover_idx);
        *HOVER_ROW.lock().unwrap() = hover_idx;
    }
    let mut stats = RowCreateStats::default();
    for i in start..end {
        if row_view_for_display_index(i).is_some() {
            continue;
        }
        let Some(&h_idx) = filtered.get(i) else {
            continue;
        };
        let entry = &hist[h_idx];
        let group = day_group(entry.copied_at);
        // The group header depends only on display order, not on which rows are materialized.
        let previous_group = if i == 0 {
            None
        } else {
            filtered
                .get(i - 1)
                .and_then(|&previous| hist.get(previous))
                .map(|e| day_group(e.copied_at))
        };
        let has_hdr = previous_group != Some(group);
        let hdr_h = if has_hdr { GROUP_H } else { 0.0 };
        let view = create_row_views(
            container,
            RowSpec {
                entry,
                display_index: i,
                y: offsets[i],
                row_h: pitches[i] - hdr_h,
                has_header: has_hdr,
                selected: i == sel_idx,
                hovered: i == hover_idx,
            },
            &mut stats,
        );
        insert_materialized_row(i, view);
    }
    REBUILDING.store(false, Ordering::SeqCst);
    true
}

/// Whether `try_delete_picker_row_incremental` may handle a one-row deletion: every row must be
/// materialized in display order, and a row must remain afterwards. An empty list has no rows to
/// relayout, and its empty-state hint (plus the minimal document height) only exists in
/// `rebuild_rows` -- so the delete must fall back to a full rebuild instead of leaving a blank
/// list with no hint.
pub(super) fn incremental_row_delete_applies(
    all_materialized_in_order: bool,
    remaining_after_delete: usize,
    materialized_before: usize,
) -> bool {
    all_materialized_in_order
        && remaining_after_delete > 0
        && remaining_after_delete + 1 == materialized_before
}

/// Remove and relayout existing views for a single deletion; fall back to a full rebuild when
/// the date-group structure changes.
unsafe fn try_delete_picker_row_incremental(idx: usize) -> bool {
    let old_views = ROW_HOVER_VIEWS.lock().unwrap().clone();
    let Some(_) = old_views.get(idx) else {
        return false;
    };
    let filter = *CLIP_FILTER.lock().unwrap();
    let query = with_clipboard_ui(|ui| ui.search_query.clone());
    let show_source = show_source_app();
    let hist = CLIP_HISTORY.lock().unwrap();
    let filtered = filtered_indices(&hist, &query, filter);
    // A virtualized list materializes only rows near the viewport; let the next visible refresh
    // rebind its slots instead of treating physical slots as full-list indices. Small lists can
    // still use the existing no-flash incremental path.
    let materialized_indices = ROW_VIEW_INDICES.lock().unwrap().clone();
    let all_materialized_in_order =
        materialized_indices == (0..old_views.len()).collect::<Vec<_>>();
    if !incremental_row_delete_applies(all_materialized_in_order, filtered.len(), old_views.len()) {
        return false;
    }

    let mut previous_group = None;
    for (new_idx, &history_idx) in filtered.iter().enumerate() {
        let group = day_group(hist[history_idx].copied_at);
        let has_header = previous_group.is_none() || previous_group != Some(group);
        previous_group = Some(group);
        let old_idx = if new_idx < idx { new_idx } else { new_idx + 1 };
        if old_views[old_idx].group_label.is_some() != has_header {
            return false;
        }
    }

    REBUILDING.store(true, Ordering::SeqCst);
    let removed = ROW_HOVER_VIEWS.lock().unwrap().remove(idx);
    ROW_VIEW_INDICES.lock().unwrap().remove(idx);
    // The bar is a tile subview and is released with the tile; never remove the tile and
    // then the bar (use-after-free).
    remove_row_views(&removed);

    let old_hover = *HOVER_ROW.lock().unwrap();
    let new_hover = if old_hover == idx {
        NO_SELECTION
    } else if old_hover > idx && old_hover != NO_SELECTION {
        old_hover - 1
    } else {
        old_hover
    };
    *HOVER_ROW.lock().unwrap() = new_hover;
    with_clipboard_ui(|ui| {
        ui.filtered = filtered.clone();
        ui.picker_selection = clamp_selection(ui.picker_selection, filtered.len());
    });

    let mut pitches = Vec::with_capacity(filtered.len());
    let mut previous_group = None;
    for &history_idx in &filtered {
        let entry = &hist[history_idx];
        let group = day_group(entry.copied_at);
        let header = if previous_group.is_none() || previous_group != Some(group) {
            GROUP_H
        } else {
            0.0
        };
        previous_group = Some(group);
        pitches.push(header + row_content_h(entry));
    }
    *ROW_PITCHES.lock().unwrap() = pitches.clone();
    refresh_footer_count(hist.len());

    let views = ROW_HOVER_VIEWS.lock().unwrap().clone();

    let selection = picker_selection();
    let palette = clipboard_palette();
    let mut previous_group = None;
    let offsets = row_offsets(&pitches);
    for (i, (&history_idx, view)) in filtered.iter().zip(views.iter()).enumerate() {
        let entry = &hist[history_idx];
        let group = day_group(entry.copied_at);
        let has_header = previous_group.is_none() || previous_group != Some(group);
        previous_group = Some(group);
        let y = offsets[i];
        let row_w = picker_list_width() - PAD_X * 2.0;
        let header_h = if has_header { GROUP_H } else { 0.0 };
        let content_y = y + header_h;
        let row_h = pitches[i] - header_h;
        if let Some(group_label) = view.group_label {
            let _: () = msg_send![group_label.0, setFrame: NSRect::new(
                NSPoint::new(PAD_X + ROW_PAD_L, y + GROUP_LABEL_PAD),
                NSSize::new(row_w - PAD_X, GROUP_H - GROUP_LABEL_PAD)
            )];
        }
        let _: () = msg_send![view.tile.0, setFrame: NSRect::new(
            NSPoint::new(PAD_X, content_y),
            NSSize::new(row_w, row_h)
        )];
        let content_x = PAD_X + ROW_PAD_L;
        let content_w = model::row_content_width(row_w);
        let _: () = msg_send![view.content.0, setFrame: NSRect::new(
            NSPoint::new(content_x, content_y + ROW_PAD_TOP),
            NSSize::new(content_w, row_h - ROW_PAD_TOP - ROW_PAD_BOT)
        )];
        let act_y = content_y + (row_h - ACTION_H) / 2.0;
        let x_del = picker_list_width() - PAD_X - ROW_PAD_R - ACTION_BTN;
        let x_pin = x_del - ACTION_GAP - ACTION_BTN;
        let x_detail = x_pin - ACTION_GAP - ACTION_BTN;
        for (button, x) in [
            (view.detail, x_detail),
            (view.pin, x_pin),
            (view.del, x_del),
        ] {
            if !button.0.is_null() {
                let _: () = msg_send![button.0, setFrame: NSRect::new(
                    NSPoint::new(x, act_y),
                    NSSize::new(ACTION_BTN, ACTION_H)
                )];
                let _: () = msg_send![button.0, setTag: i as isize];
            }
        }

        let selected = i == selection;
        let hovered = i == new_hover;
        let background = row_backdrop_wash(palette, selected, hovered);
        let tile_layer: *mut AnyObject = msg_send![view.tile.0, layer];
        crate::ffi::layer_set_background(tile_layer, crate::ffi::hex_to_cg_color(background));
        if !entry.pinned {
            let alpha = if selected || hovered { 1.0 } else { 0.62 };
            for button in [view.detail, view.pin, view.del] {
                if !button.0.is_null() {
                    let _: () = msg_send![button.0, setAlphaValue: alpha];
                }
            }
        }
    }

    if let Some(container) = picker_container_ptr() {
        let document_h = (rows_top_offset() + pitches.iter().sum::<f64>() + PAD_Y)
            .max(picker_min_height() - header_strip_h() - FOOTER_H);
        let _: () =
            msg_send![container, setFrameSize: NSSize::new(picker_list_width(), document_h)];
    }
    let rendered_key = picker_rows_key(history_revision(), &query, filter, show_source);
    with_clipboard_ui(|ui| ui.rendered_rows = Some(rendered_key));
    REBUILDING.store(false, Ordering::SeqCst);
    true
}

/// The header strip's class: flipped, so the search/clear frames are top-anchored.
fn header_strip_class() -> *mut AnyObject {
    static CLASS: OnceLock<StaticClass> = OnceLock::new();
    crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabClipHeaderView", || {
        crate::ffi::build_class(c"OhMyTabClipHeaderView", class!(NSView), |builder| unsafe {
            builder.add_method::<AnyObject, _>(
                sel!(isFlipped),
                header_strip_is_flipped
                    as unsafe extern "C" fn(*mut AnyObject, Sel) -> objc2::runtime::Bool,
            );
        })
    })
    .0 as *mut AnyObject
}

/// The header strip is flipped: the search/clear frames are top-anchored.
extern "C" fn header_strip_is_flipped(_self: *mut AnyObject, _cmd: Sel) -> objc2::runtime::Bool {
    callback_guard::value("header_strip_is_flipped", objc2::runtime::Bool::YES, || {
        objc2::runtime::Bool::YES
    })
}

/// Container is flipped: origin at top-left, rows stack from the top (newest first).
extern "C" fn container_is_flipped(_self: *mut AnyObject, _cmd: Sel) -> objc2::runtime::Bool {
    callback_guard::value("container_is_flipped", objc2::runtime::Bool::YES, || {
        objc2::runtime::Bool::YES
    })
}

/// Resolve the visible row actually under the event. Gate on the list scroll view first so
/// header/footer coordinates cannot convert into an accidental row hit in the document view.
unsafe fn hover_row_at_event(event: *mut c_void) -> usize {
    if event.is_null() {
        return NO_SELECTION;
    }
    let scroll = match *SCROLL_VIEW.lock().unwrap() {
        Some(scroll) => scroll.0,
        None => return NO_SELECTION,
    };
    let Some(container) = picker_container_ptr() else {
        return NO_SELECTION;
    };
    let location: NSPoint = msg_send![event as *mut AnyObject, locationInWindow];
    let scroll_point: NSPoint = msg_send![
        scroll,
        convertPoint: location,
        fromView: std::ptr::null::<AnyObject>()
    ];
    let scroll_bounds: NSRect = msg_send![scroll, bounds];
    if !rect_contains_point(scroll_bounds, scroll_point) {
        return NO_SELECTION;
    }

    let point: NSPoint = msg_send![
        container,
        convertPoint: location,
        fromView: std::ptr::null::<AnyObject>()
    ];
    let indices = ROW_VIEW_INDICES.lock().unwrap().clone();
    ROW_HOVER_VIEWS
        .lock()
        .unwrap()
        .iter()
        .position(|row| {
            if row.tile.0.is_null() {
                return false;
            }
            let frame: NSRect = msg_send![row.tile.0, frame];
            rect_contains_point(frame, point)
        })
        .and_then(|slot| indices.get(slot).copied())
        .unwrap_or(NO_SELECTION)
}

/// Whether an event's pointer is over the options menu. Tracking areas still report enters for
/// covered list rows, so both list-wide and row-local hover handlers must treat the menu as the
/// topmost surface.
pub(super) unsafe fn event_is_over_filter_popover(event: *mut AnyObject) -> bool {
    if event.is_null() || !crate::clipboard::filter_popover_open() {
        return false;
    }
    let Some(popover) = *FILTER_POPOVER.lock().unwrap() else {
        return false;
    };
    let frame: NSRect = msg_send![popover.0, frame];
    let superview: *mut AnyObject = msg_send![popover.0, superview];
    if superview.is_null() {
        return false;
    }
    let in_window: NSRect = msg_send![
        superview,
        convertRect: frame,
        toView: std::ptr::null::<AnyObject>()
    ];
    let point: NSPoint = msg_send![event, locationInWindow];
    rect_contains_point(in_window, point)
}

/// Reconcile hover against the whole row backdrop on every picker mouse move. This prevents
/// a stale style when the pointer leaves a content button through row padding and then exits.
extern "C" fn container_mouse_moved(_self: *mut AnyObject, _cmd: Sel, event: *mut AnyObject) {
    callback_guard::void("container_mouse_moved", || {
        if REBUILDING.load(Ordering::SeqCst) {
            return;
        }
        if unsafe { event_is_over_filter_popover(event) } {
            set_hover_row(NO_SELECTION);
            return;
        }
        let row = unsafe { hover_row_at_event(event as *mut c_void) };
        set_hover_row(row);
    });
}

/// Clear hover when leaving the whole picker. The fixed parent's InVisibleRect tracking area
/// supplies this event independently of whether any row button receives mouseExited.
extern "C" fn container_mouse_exited(_self: *mut AnyObject, _cmd: Sel, _event: *mut AnyObject) {
    callback_guard::void("container_mouse_exited", || {
        if !REBUILDING.load(Ordering::SeqCst) {
            set_hover_row(NO_SELECTION);
        }
    });
}

/// Row-button class (NSButton subclass; mouseEntered: implements hover selection).
unsafe fn row_button_class() -> *mut AnyObject {
    static ROW_BTN_CLS: OnceLock<StaticClass> = OnceLock::new();
    crate::ffi::StaticClass::register_once(&ROW_BTN_CLS, c"OhMyTabClipboardRowButton", || {
        crate::ffi::build_class(
            c"OhMyTabClipboardRowButton",
            class!(NSButton),
            |builder| unsafe {
                builder.add_method::<AnyObject, _>(
                    sel!(mouseEntered:),
                    row_button_mouse_entered
                        as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
                builder.add_method::<AnyObject, _>(
                    sel!(mouseExited:),
                    row_button_mouse_exited
                        as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
            },
        )
    })
    .0 as *mut AnyObject
}

fn update_hover_visuals(prev: usize, new: usize) {
    let sel = picker_selection();
    let hist = CLIP_HISTORY.lock().unwrap();
    let filtered = with_clipboard_ui(|ui| ui.filtered.clone());
    // Repaint through the same pure wash helper create_row_views paints at row build, so the
    // two paths cannot drift. They once did: the incremental repaint used literal black
    // washes (5%/3.2%), which measure ~1.005:1 on the dark panel, so arrow navigation dropped
    // the blue selection wash and hover feedback vanished in dark mode.
    let palette = clipboard_palette();
    unsafe {
        for i in [prev, new] {
            if i == NO_SELECTION {
                continue;
            }
            let Some(rv) = row_view_for_display_index(i) else {
                continue;
            };
            let selected = i == sel;
            let hovered = i == new;
            let bg = row_backdrop_wash(palette, selected, hovered);
            let layer: *mut AnyObject = msg_send![rv.tile.0, layer];
            crate::ffi::layer_set_background(layer, crate::ffi::hex_to_cg_color(bg));
            // Button alpha: pinned entries keep them always visible (fixed 1.0); unpinned
            // entries show them on hover/selection only.
            let pinned = filtered
                .get(i)
                .and_then(|&h| hist.get(h))
                .map(|e| e.pinned)
                .unwrap_or(false);
            if !pinned {
                let act_alpha: f64 = if selected || hovered { 1.0 } else { 0.62 };
                for b in [rv.pin, rv.del] {
                    if !b.0.is_null() {
                        let _: () = msg_send![b.0, setAlphaValue: act_alpha];
                    }
                }
            }
        }
    }
}

fn set_hover_row(new: usize) {
    let mut hover = HOVER_ROW.lock().unwrap();
    let prev = *hover;
    if prev == new {
        return;
    }
    *hover = new;
    drop(hover);
    update_hover_visuals(prev, new);
}

/// Exercise both real AppKit row-paint paths with a row that is selected and hovered.
/// The picker smoke reads the resulting layer colors, so this tests the wiring as well as the
/// pure precedence rule without scanning source text.
pub(super) unsafe fn smoke_row_backdrop_paint_paths() -> bool {
    // The picker opens with the search field focused, so its intentional sentinel can mean
    // "nothing selected". This smoke specifically exercises a selected+hovered row; establish
    // that state explicitly instead of returning false before either paint path runs.
    let previous_selection = picker_selection();
    let selection = if previous_selection == NO_SELECTION {
        set_picker_selection(0);
        0
    } else {
        previous_selection
    };
    let Some(row) = row_view_for_display_index(selection) else {
        eprintln!("[smoke-clipboard] no materialized row for selected index {selection}");
        return false;
    };
    let Some(container) = picker_container_ptr() else {
        eprintln!("[smoke-clipboard] picker has no row container");
        return false;
    };
    let Some(history_index) = with_clipboard_ui(|ui| ui.filtered.get(selection).copied()) else {
        eprintln!("[smoke-clipboard] selected index {selection} is outside filtered history");
        return false;
    };
    let Some(entry) = CLIP_HISTORY.lock().unwrap().get(history_index).cloned() else {
        eprintln!("[smoke-clipboard] selected history index {history_index} is missing");
        return false;
    };

    let expected = super::picker_row_selected(clipboard_palette());
    set_hover_row(NO_SELECTION);
    set_hover_row(selection);
    let repaint_layer: *mut AnyObject = msg_send![row.tile.0, layer];
    let repaint_ok = layer_matches_hex(repaint_layer, expected);

    let frame: NSRect = msg_send![row.tile.0, frame];
    let mut stats = RowCreateStats::default();
    let created = create_row_views(
        container,
        RowSpec {
            entry: &entry,
            display_index: selection,
            y: frame.origin.y,
            row_h: frame.size.height,
            has_header: false,
            selected: true,
            hovered: true,
        },
        &mut stats,
    );
    let create_layer: *mut AnyObject = msg_send![created.tile.0, layer];
    let create_ok = layer_matches_hex(create_layer, expected);
    remove_row_views(&created);
    set_hover_row(NO_SELECTION);
    set_picker_selection(previous_selection);
    if !repaint_ok || !create_ok {
        eprintln!(
            "[smoke-clipboard] row paint paths differ: repaint={repaint_ok} create={create_ok} expected={expected:#010x} palette_dark={}",
            clipboard_palette().dark
        );
    }
    repaint_ok && create_ok
}

unsafe fn layer_matches_hex(layer: *mut AnyObject, expected: u32) -> bool {
    let actual = crate::ffi::layer_background_color(layer);
    if actual.is_null() {
        return false;
    }
    let expected = crate::ffi::hex_to_cg_color(expected);
    crate::ffi::CGColorEqualToColor(actual, expected)
}

/// With search focus, the list has no keyboard-selected row, but filtered entries must still
/// show their independent mouse-hover style.
extern "C" fn row_button_mouse_entered(_self: *mut AnyObject, _cmd: Sel, _event: *mut AnyObject) {
    callback_guard::void("row_button_mouse_entered", || {
        // Ignore enters dispatched during a rebuild (prevents infinite recursion; see REBUILDING).
        if REBUILDING.load(Ordering::SeqCst) {
            return;
        }
        if unsafe { event_is_over_filter_popover(_event) } {
            set_hover_row(NO_SELECTION);
            return;
        }
        let idx: isize = unsafe { msg_send![_self, tag] };
        if idx >= 0 {
            // Hovering only sets the hovered row (the hover wash) and does NOT move the
            // selection (the selection wash + the left bar) -- the selection moves via the
            // keyboard arrows
            // / clicks only. The two states stay independently visible, matching the mockup's
            // separate .item:hover and .item.selected rules (auto-select-on-hover would always
            // render the hovered row as the selected style, making them look identical).
            // Incremental hover visuals, no rebuild.
            set_hover_row(idx as usize);
        }
    });
}

/// Check whether the pointer is still inside the whole row, including its separate action buttons.
pub(super) unsafe fn mouse_inside_row(event: *mut c_void, idx: usize) -> bool {
    let Some(row) = row_view_for_display_index(idx) else {
        return false;
    };
    if row.tile.0.is_null() {
        return false;
    }
    let Some(container) = picker_container_ptr() else {
        return false;
    };
    // locationInWindow uses the window-base coordinate system; fromView must be an NSView,
    // never an NSWindow.
    let location: NSPoint = msg_send![event as *mut AnyObject, locationInWindow];
    let point: NSPoint = msg_send![
        container,
        convertPoint: location,
        fromView: std::ptr::null::<AnyObject>()
    ];
    let frame: NSRect = msg_send![row.tile.0, frame];
    rect_contains_point(frame, point)
}

/// Clear a row's hover state and synchronously collapse its hover visuals.
pub(super) fn clear_hover_row_if(idx: usize) {
    let hovered = *HOVER_ROW.lock().unwrap();
    if hovered == idx {
        set_hover_row(NO_SELECTION);
    }
}

/// Mouse leaves a row button: clear hover only after leaving the whole row, so moving to the
/// bottom-right action buttons does not hide them mid-transition.
extern "C" fn row_button_mouse_exited(_self: *mut AnyObject, _cmd: Sel, event: *mut AnyObject) {
    callback_guard::void("row_button_mouse_exited", || {
        if REBUILDING.load(Ordering::SeqCst) {
            return;
        }
        let idx: isize = unsafe { msg_send![_self, tag] };
        if idx >= 0 {
            if unsafe { mouse_inside_row(event as *mut c_void, idx as usize) } {
                return;
            }
            clear_hover_row_if(idx as usize);
        }
    });
}

/// Row click (button tag = row index) -> paste that row; Option+click (when the setting
/// is on) = paste and delete. The modifiers come from currentEvent: the action fires on
/// mouseUp, so currentEvent IS this click (equivalent to Option+Enter on the keyboard
/// path; with the toggle off Option is ignored and the normal paste runs).
pub(super) extern "C" fn handle_clipboard_row_click(
    _self: *mut c_void,
    _cmd: Sel,
    sender: *mut c_void,
) {
    callback_guard::void("handle_clipboard_row_click", || {
        let idx: isize = unsafe { msg_send![sender as *mut AnyObject, tag] };
        if idx >= 0 {
            // NSEventModifierFlagOption (Alternate) = 1 << 19 = 0x08_0000. currentEvent is an
            // INSTANCE method on NSApplication: go through sharedApplication first. Sending it
            // to the Class object raises an unrecognized-selector exception that unwinds
            // through AppKit's event loop and wedges the picker.
            let option_held = unsafe {
                let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
                let ev: *mut AnyObject = if nsapp.is_null() {
                    std::ptr::null_mut()
                } else {
                    msg_send![nsapp, currentEvent]
                };
                if ev.is_null() {
                    false
                } else {
                    let flags: u64 = msg_send![ev, modifierFlags];
                    (flags & 0x0008_0000) != 0
                }
            };
            if option_held {
                paste_at_ex(idx as usize, true);
            } else {
                paste_at(idx as usize);
            }
        }
    });
}

/// Pin-button callback (tag = display row index) -> mapped history index, pin/unpin, refresh.
pub(super) extern "C" fn toggle_pin(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    callback_guard::void("toggle_pin", || {
        let idx: isize = unsafe { msg_send![sender as *mut AnyObject, tag] };
        if idx < 0 {
            return;
        }
        let Some(h_idx) = mapped_index(idx as usize) else {
            return;
        };
        let mut hist = CLIP_HISTORY.lock().unwrap();
        let (now_pinned, new_h_idx) = toggle_pin_on(&mut hist, h_idx);
        drop(hist);
        save_history();
        unsafe { rebuild_rows() };
        // Follow-pin setting: select the toggled entry (the POST-REORDER index), then rebuild
        // once more.
        if selection_after_pin(new_h_idx) {
            unsafe { rebuild_rows() };
        }
        // An open detail panel follows THIS entry to its new display position -- but only when the
        // user's "after pinning" setting lets the selection move at all. "Keep current position"
        // means the highlight stays where it is, and moving the selection here anyway would both
        // override that choice and leave the pane showing one entry while the highlight names
        // another; the pane therefore follows the selection, wherever the setting put it.
        if detail_visible() {
            if CONFIG.read().unwrap().clipboard.pin_follow_selection {
                let filtered = with_clipboard_ui(|ui| ui.filtered.clone());
                if let Some(pos) = filtered.iter().position(|&h| h == new_h_idx) {
                    set_picker_selection(pos);
                    // The highlight moved with the selection, so the row list has to be redrawn
                    // before the pane is rebuilt from it.
                    unsafe { rebuild_rows() };
                }
            }
            unsafe { show_detail_for_sel() };
        }
        let msg = if now_pinned {
            t("clipboard.toast_pinned")
        } else {
            t("clipboard.toast_unpinned")
        };
        show_toast(&msg);
    });
}

/// Move the selection after pin/unpin per `clipboard.pin_follow_selection`:
/// - Follow (true, default): select the toggled entry's NEW display position (pin -> the
///   top of the list; unpin -> the top of the unpinned block);
/// - Keep (false): leave it (rebuild_rows only clamps; the selection points at the next
///   entry, convenient for batch pinning).
///
/// Returns whether the selection moved (the caller then rebuilds once more to refresh the
/// highlight). `new_h_idx` is the entry's POST-REORDER history index (returned by
/// toggle_pin_on) -- the OLD index already refers to a different entry, so searching the
/// fresh list with it would land on the old position, i.e. exactly "keep current" (the
/// follow mode once failed this way).
fn selection_after_pin(new_h_idx: usize) -> bool {
    if !CONFIG.read().unwrap().clipboard.pin_follow_selection {
        return false;
    }
    let filtered = with_clipboard_ui(|ui| ui.filtered.clone());
    if let Some(pos) = filtered.iter().position(|&h| h == new_h_idx) {
        set_picker_selection(pos);
        true
    } else {
        false
    }
}

/// Delete-button callback (tag = display row index) -> mapped history index, remove, refresh.
/// The details-button callback (tag = display row index) -> select the row and open the
/// detail panel (the same path as the → key). TOGGLE: with the detail already open and
/// the click landing on the CURRENTLY selected row, close it (a second click cancels the
/// detail); otherwise select the row and open/refresh the detail (a different row's click
/// moves the detail along to the new row).
pub(super) extern "C" fn show_item_details_cb(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    callback_guard::void("show_item_details_cb", || {
        let idx: isize = unsafe { msg_send![sender as *mut AnyObject, tag] };
        if idx < 0 {
            return;
        }
        let previous = picker_selection();
        // Detail already open AND the click is on the selected row -> this click cancels it.
        let close = detail_visible() && previous == idx as usize;
        set_picker_selection(idx as usize);
        unsafe {
            // A detail-button click only needs the incremental visual update for the old and new
            // selection; rebuilding the entire clipboard list here made mouse opening feel slow.
            refresh_selection(previous, idx as usize);
            if close {
                hide_detail();
            } else {
                show_detail_for_sel();
            }
        }
    });
}

/// Keep an open detail panel on the selection after a delete: the selection stays in place and
/// now points at the entry that slid into the removed row's slot (or the new tail), so refresh
/// the panel in place instead of collapsing it -- the mock's `deleteItem` does the same
/// (`detailOpen && !currentItem()` is the only close). An empty display list or no selection
/// still closes it.
///
/// `ui.filtered` is current only when `try_delete_picker_row_incremental` handled the row.
/// On the deferred path a synchronous `rebuild_rows` refreshes list, filtered list and
/// selection together -- the queued refresh then finds the rows current and no-ops, and
/// `ui.filtered` never stops describing the on-screen rows in between.
unsafe fn retarget_detail_after_delete(incremental: bool) {
    if !detail_visible() {
        return;
    }
    if !incremental {
        rebuild_rows();
    }
    if mapped_index(picker_selection()).is_some() {
        show_detail_for_sel();
    } else {
        hide_detail();
    }
}

pub(super) extern "C" fn delete_entry_cb(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    callback_guard::void("delete_entry_cb", || {
        let idx: isize = unsafe { msg_send![sender as *mut AnyObject, tag] };
        if idx < 0 {
            return;
        }
        let Some(h_idx) = mapped_index(idx as usize) else {
            return;
        };
        let mut hist = CLIP_HISTORY.lock().unwrap();
        let Some(removed_entry) = remove_entry_for_undo(&mut hist, h_idx) else {
            return;
        };
        // A deleted row ABOVE the selection shifts it down one (the same entry stays selected);
        // deleting the selected row or a row below leaves it alone (the former points at the
        // next entry). The no-selection sentinel (search-field focus) is untouched. The
        // out-of-range clamp happens in rebuild_rows after FILTERED is recomputed -- this used
        // to clamp the display index against hist.len(): correct only by coincidence without a
        // search query, dimensionally wrong under a filter, and still past the list after
        // deleting the tail (the lost highlight).
        let previous = picker_selection();
        let deleted_selected = previous != NO_SELECTION && previous == idx as usize;
        if previous != NO_SELECTION && (idx as usize) < previous {
            set_picker_selection(previous - 1);
        }
        drop(hist);
        remember_deleted_clipboard_entry(removed_entry, h_idx);
        let incremental = unsafe { try_delete_picker_row_incremental(idx as usize) };
        save_history();
        if !incremental {
            schedule_picker_refresh();
        }
        // The detail panel follows the selected entry; when the deleted row WAS the selection,
        // retarget it onto the entry that took the slot rather than collapsing it.
        if deleted_selected {
            unsafe { retarget_detail_after_delete(incremental) };
        }
    });
}

/// Paste the entry at display `idx` (mapped through FILTERED): close the picker + write back
/// to the pasteboard + synthesize Cmd+V.
fn paste_at(idx: usize) {
    paste_at_ex(idx, false);
}

/// The burn-after-paste variant of paste_at: on a successful paste the entry is removed
/// from the history right away (Option+Enter/click, one-shot paste). Removal goes through
/// delete_entry (the image cache file goes too); pinned entries burn all the same.
fn paste_at_ex(idx: usize, delete_after: bool) {
    // These guards are unreachable in practice; they still follow the pinned rule rather than
    // hiding a panel the user is holding open (design decision R3).
    let Some(h_idx) = mapped_index(idx) else {
        log_debug!("[clip] paste index {} out of range", idx);
        if !is_pinned() {
            hide_picker();
        }
        return;
    };
    let entry = {
        let hist = CLIP_HISTORY.lock().unwrap();
        hist.get(h_idx).cloned()
    };
    let Some(entry) = entry else {
        log_debug!("[clip] paste index {} out of range", idx);
        if !is_pinned() {
            hide_picker();
        }
        return;
    };
    // An image whose bytes cannot be produced pastes nothing at all: say so while the picker is
    // still on screen (the toast lives inside it), then leave it visible so the user reads why.
    // This runs BEFORE the burn-after-paste token is armed: a refusal that never writes back must
    // not leave a suppression token behind, or the next real copy would be skipped as our own write.
    if let Some(img) = &entry.image {
        if paste_refused_with_notice(
            paste_kind(img) == PasteKind::Image,
            image_bytes_available(img.hash),
        ) {
            show_toast(&t("clipboard.toast_image_unavailable"));
            return;
        }
    }
    // Effective = gesture AND setting: with the toggle off, Option falls back to a plain
    // paste (the modifier is ignored).
    let burn = delete_after && delete_after_paste();
    // A previous hand-back is superseded HERE, before this paste arms its own suppression token:
    // its cancellation releases whichever token existed at that moment (the old request's), while
    // cancelling later would clear the token this paste is about to arm.
    super::paste_handback::cancel();
    if burn {
        // Arm BEFORE the write-back (the synchronous-notification re-entry case; see the
        // arming function's comment).
        arm_paste_delete_suppression();
    }
    // A pinned panel stays on screen (decision R3), so it cannot rely on ordering out to hand the
    // key window to the target: the bounded hand-back activates the captured target and posts the
    // Cmd+V event directly to that process.
    let pinned = is_pinned();
    if !pinned {
        hide_picker();
    }
    unsafe {
        if let Some(img) = &entry.image {
            // A file-copy entry: if the source file still exists, restore file semantics
            // (file-url) -- the target app reads the original file on demand; if the source
            // is deleted/moved, skip the paste (a file entry stores no bytes to fall back
            // to).
            let ok = match paste_kind(img) {
                PasteKind::File(path) => write_pasteboard_file(&path),
                PasteKind::Image if img.source_path.is_some() => {
                    log_info!("[clip] paste skipped: source file gone (uti={})", img.uti);
                    false
                }
                PasteKind::Image => write_pasteboard_image(img),
            };
            // On a failed write-back (e.g. cache miss) skip the synthesized Cmd+V, so the
            // OLD pasteboard content is not pasted.
            if ok {
                finish_paste(&entry, burn, pinned);
            } else {
                // A failed write-back means nothing was written: disarm so the suppression
                // cannot swallow the next genuine copy, and keep the entry (never lose the
                // record without a paste).
                if burn {
                    disarm_paste_delete_suppression();
                }
            }
        } else {
            // Paste write-back: stamp the marker (the poll skips it, so a paste is never
            // re-captured as a fresh copy that reorders the history).
            let ok = write_pasteboard_text(&entry.text, true);
            if ok {
                finish_paste(&entry, burn, pinned);
            } else if burn {
                log_info!("[clip] paste-and-delete skipped: text write-back failed");
                disarm_paste_delete_suppression();
            }
        }
    }
}

/// The send half of a paste, after the write-back succeeded.
///
/// The two modes differ only in WHEN the keystroke is sent: a hidden panel sends immediately (the
/// key window has already returned to the target), while a pinned panel arms the bounded hand-back,
/// which targets the selected process after activation settles.
unsafe fn finish_paste(entry: &ClipEntry, burn: bool, pinned: bool) {
    if pinned {
        // A missing change count means the hand-back cannot prove its own write is still the one on
        // the pasteboard, so it must not arm at all - and the burn token must not be left behind for
        // a send that is never going to happen.
        match pasteboard_version().map(|v| super::paste_handback::begin(entry, v, burn)) {
            Some(true) => {}
            _ => {
                show_toast(&t("clipboard.toast_paste_no_target"));
                if burn {
                    disarm_paste_delete_suppression();
                }
            }
        }
        return;
    }
    let pasted = synthesize_paste();
    if burn && pasted {
        delete_burned_entry(entry);
        if clear_system_pasteboard_after_paste() {
            schedule_system_pasteboard_clear();
        }
    } else if burn {
        log_info!("[clip] paste-and-delete skipped: Cmd+V event creation failed");
        disarm_paste_delete_suppression();
    }
}

/// The pasteboard's change count, which the hand-back uses to prove its own write is still the one
/// on the pasteboard when the send finally happens.
unsafe fn pasteboard_version() -> Option<i64> {
    let pasteboard: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
    if pasteboard.is_null() {
        return None;
    }
    Some(msg_send![pasteboard, changeCount])
}

/// The removal step of burn-after-paste: drop the entry from the history (the image cache
/// file goes too) and persist; logs record only the kind and count, never the content.
pub(super) fn delete_burned_entry(target: &ClipEntry) {
    let kind = {
        let mut hist = CLIP_HISTORY.lock().unwrap();
        let Some(h_idx) = hist
            .iter()
            .position(|entry| same_clip_entry_identity(entry, target))
        else {
            log_debug!("[clip] paste-and-delete: entry already gone");
            return;
        };
        let kind = match hist.get(h_idx) {
            Some(e) if e.image.is_some() => "image",
            Some(_) => "text",
            None => "gone",
        };
        if kind != "gone" {
            delete_entry(&mut hist, h_idx);
        }
        kind
    };
    save_history();
    log_info!(
        "[clip] pasted and deleted (burn after paste, kind={})",
        kind
    );
}

/// Decide the paste kind: a file-copy entry whose source file still exists pastes as a
/// FILE (restoring the file-url); everything else pastes as image data (original UTI).
/// Pure, unit-tested.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum PasteKind {
    File(String),
    Image,
}

pub(super) fn paste_kind(img: &ImageEntry) -> PasteKind {
    match &img.source_path {
        Some(path) if std::path::Path::new(path).exists() => PasteKind::File(path.clone()),
        _ => PasteKind::Image,
    }
}

/// Synthesize Cmd+V (keyDown + keyUp, posted at the session level) AFTER the picker is
/// closed. The panel is the key window (NonactivatingPanel + makeKeyWindow), so a
/// synthesized key event would be routed to the panel's app (us) and never reach the input
/// field; once ordered out, the panel resigns key, the system key window returns to the
/// previous app, and the synthesized Cmd+V lands in the user's input field.
pub(super) unsafe fn synthesize_paste() -> bool {
    let down = CGEventCreateKeyboardEvent(std::ptr::null(), VK_V, true);
    let Some(down) = (!down.is_null()).then_some(down) else {
        return false;
    };
    CGEventSetFlags(down, K_CG_EVENT_FLAG_MASK_COMMAND);
    CGEventPost(K_CG_SESSION_EVENT_TAP, down);
    let up = CGEventCreateKeyboardEvent(std::ptr::null(), VK_V, false);
    if up.is_null() {
        // Release the already-posted `down` on this failure path too: CGEventCreate* returns
        // +1 and posting does not take ownership.
        CFRelease(down as *const c_void);
        return false;
    }
    CGEventSetFlags(up, K_CG_EVENT_FLAG_MASK_COMMAND);
    CGEventPost(K_CG_SESSION_EVENT_TAP, up);
    CFRelease(down as *const c_void);
    CFRelease(up as *const c_void);
    true
}

/// Send Cmd+V into a specific app's event queue while the clipboard picker remains pinned.
/// Posting at the session tap would route the shortcut to the picker when it owns key status.
pub(super) unsafe fn synthesize_paste_to_pid(pid: i32) -> bool {
    use crate::event_tap::{
        CGEventSetIntegerValueField, K_CG_EVENT_SOURCE_USER_DATA, SYNTHETIC_MARKER,
    };

    if pid <= 0 {
        return false;
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventPostToPid(pid: i32, event: crate::event_tap::CGEventRef);
    }

    let down = CGEventCreateKeyboardEvent(std::ptr::null_mut(), VK_V, true);
    let up = CGEventCreateKeyboardEvent(std::ptr::null_mut(), VK_V, false);
    if down.is_null() || up.is_null() {
        if !down.is_null() {
            CFRelease(down as *const c_void);
        }
        if !up.is_null() {
            CFRelease(up as *const c_void);
        }
        return false;
    }
    for event in [down, up] {
        CGEventSetFlags(event, K_CG_EVENT_FLAG_MASK_COMMAND);
        CGEventSetIntegerValueField(event, K_CG_EVENT_SOURCE_USER_DATA, SYNTHETIC_MARKER);
        CGEventPostToPid(pid, event);
    }
    CFRelease(down as *const c_void);
    CFRelease(up as *const c_void);
    true
}

/// Pure arrow-key navigation: up (126) / down (125) return the next selection (wrapping);
/// any other key returns None.
pub(super) fn nav_arrow(keycode: u16, sel: usize, hist_len: usize) -> Option<usize> {
    if hist_len == 0 {
        return None;
    }
    // sel may be NO_SELECTION (usize::MAX, the sentinel while the search field has focus) --
    // the smoke drives the handler directly so this must not overflow and treats it as
    // "no selection".
    match keycode {
        126 => Some(if sel == 0 || sel >= hist_len {
            hist_len - 1
        } else {
            sel - 1
        }),
        125 => Some(if sel >= hist_len - 1 { 0 } else { sel + 1 }),
        _ => None,
    }
}

/// Clamp a selection index into the current display list after deletions/trims
/// (out of range -> the tail); the no-selection sentinel (NO_SELECTION) is left untouched.
/// Pure function shared by rebuild_rows and the delete paths; unit-tested.
pub(super) fn clamp_selection(sel: usize, len: usize) -> usize {
    if sel == NO_SELECTION || len == 0 {
        return sel;
    }
    sel.min(len - 1)
}

/// Keyboard navigation: Tab cycles filters; up/down select, left pins, right expands
/// details (with the detail open, right closes it), Enter pastes, Esc closes.
/// Panic boundary for the C callback: a panic cannot unwind through an `extern "C"` frame (it
/// aborts the process), so it is contained here.
pub(super) extern "C" fn container_key_down(_self: *mut c_void, _cmd: Sel, event: *mut c_void) {
    callback_guard::void("container_key_down", || unsafe {
        container_key_down_inner(_self, _cmd, event)
    });
}

unsafe fn container_key_down_inner(_self: *mut c_void, _cmd: Sel, event: *mut c_void) {
    unsafe {
        let keycode: u16 = msg_send![event as *mut AnyObject, keyCode];
        // Cmd+F (keycode 3 + Command modifier 0x100000): focus the top search field.
        // When the field is already focused, the key goes to the field editor and never
        // reaches here -- a natural no-op.
        let mods: u64 = msg_send![event as *mut AnyObject, modifierFlags];
        // Cmd+Z (keycode 6) is handled only by the list container; the search field's field
        // editor receives it first, preserving native text-editing undo.
        if is_clipboard_undo_shortcut(keycode, mods) {
            if undo_deleted_clipboard_entry().is_some() {
                rebuild_rows();
                // The undo restored the deleted entry and re-selected it; an open detail
                // must follow it back (a retargeted-after-delete panel would otherwise
                // keep showing the entry that took the slot).
                if detail_visible() {
                    show_detail_for_sel();
                }
                show_toast(&t("clipboard.toast_undo_delete"));
            }
            return;
        }
        // Cmd+C (keycode 8): with the detail open, copy the selection (full text when
        // nothing is selected) -- the keyboard twin of the detail's "copy selection"
        // button. The detail never becomes key, so the system routes Cmd+C to the picker;
        // we forward it here. With the search field focused the key goes to the field
        // editor first, so no conflict.
        if keycode == 8 && (mods & 0x0010_0000) != 0 && detail_visible() {
            copy_detail_selection();
            return;
        }
        if keycode == 3 && (mods & 0x0010_0000) != 0 {
            if let Some(f) = *SEARCH_FIELD.lock().unwrap() {
                let window = match *PICKER_WINDOW.lock().unwrap() {
                    Some(w) => w.0,
                    None => return,
                };
                // makeFirstResponder: returns BOOL ('B').
                let _: bool = msg_send![window, makeFirstResponder: f.0];
            }
            return;
        }
        // Popover key navigation (the mock's menu semantics): while open, ↑/↓ move the
        // highlighted row over the 6 filters + housekeeping rows and Enter picks it. Tab
        // keeps its filter-cycle meaning (the footer legend stays true).
        if crate::clipboard::filter_popover_open() {
            match keycode {
                126 | 125 => {
                    let rows = CLIP_FILTER_ROWS.len() + super::popover_tail_row_count();
                    let cur = POPOVER_KEY_ROW.load(Ordering::SeqCst);
                    let next = if keycode == 125 {
                        if cur == usize::MAX || cur + 1 >= rows {
                            0
                        } else {
                            cur + 1
                        }
                    } else if cur == usize::MAX || cur == 0 {
                        rows - 1
                    } else {
                        cur - 1
                    };
                    POPOVER_KEY_ROW.store(next, Ordering::SeqCst);
                    // Rebuild so the highlight moves; a no-op otherwise.
                    if let Some(parent) = *PICKER_CONTENT_PARENT.lock().unwrap() {
                        build_filter_popover(parent.0);
                    }
                    return;
                }
                36 | 76 => {
                    let row = POPOVER_KEY_ROW.load(Ordering::SeqCst);
                    if row < CLIP_FILTER_ROWS.len() {
                        let f = CLIP_FILTER_ROWS[row];
                        dismiss_filter_popover();
                        apply_clip_filter(f);
                    } else if row < CLIP_FILTER_ROWS.len() + super::popover_tail_row_count() {
                        // Tail rows mirror the popover's own buttons.
                        dismiss_filter_popover();
                        let tail = row - CLIP_FILTER_ROWS.len();
                        if tail == 1 {
                            notifications::clear_clipboard_history(
                                std::ptr::null_mut(),
                                sel!(clearClipboardHistory:),
                                std::ptr::null_mut(),
                            );
                        } else if tail == 2 && super::text_style::clear_all_button_visible() {
                            notifications::clear_clipboard_all(
                                std::ptr::null_mut(),
                                sel!(clearClipboardAll:),
                                std::ptr::null_mut(),
                            );
                        } else {
                            notifications::open_clipboard_settings(
                                std::ptr::null_mut(),
                                sel!(openClipboardSettings:),
                                std::ptr::null_mut(),
                            );
                        }
                    }
                    return;
                }
                _ => {
                    dismiss_filter_popover();
                    if keycode == 53 {
                        return;
                    }
                }
            }
        }
        // The selectable range is the current display list (post-filter; scrolling reveals
        // the rest).
        let display_len = with_clipboard_ui(|ui| ui.filtered.len());
        let sel = picker_selection();
        match keycode {
            48 => {
                // Tab(48): cycle filters in the fixed order. Close an open detail first so
                // filtering cannot leave stale content that no longer belongs to the list.
                let next = {
                    let active = CLIP_FILTER.lock().unwrap();
                    next_clip_filter(*active)
                };
                apply_clip_filter(next);
            }
            123 => {
                // Left: toggle the selected entry's pinned state whether or not the detail
                // panel is open.
                let idx = sel;
                let Some(h_idx) = mapped_index(idx) else {
                    return;
                };
                let mut hist = CLIP_HISTORY.lock().unwrap();
                let (now_pinned, new_h_idx) = toggle_pin_on(&mut hist, h_idx);
                drop(hist);
                save_history();
                rebuild_rows();
                // Follow-pin setting: select the toggled entry (the POST-REORDER index),
                // then rebuild once more to refresh the highlight.
                if selection_after_pin(new_h_idx) {
                    rebuild_rows();
                }
                // Pinning changes the row position; keep the detail open and follow the new
                // selected row.
                if detail_visible() {
                    show_detail_for_sel();
                }
                let msg = if now_pinned {
                    t("clipboard.toast_pinned")
                } else {
                    t("clipboard.toast_unpinned")
                };
                show_toast(&msg);
            }
            124 => {
                // Right: closes the detail panel when it is open (same as ←); otherwise
                // expands the selected entry's details (full text / large image).
                if detail_visible() {
                    hide_detail();
                    return;
                }
                let idx = sel;
                if idx == NO_SELECTION {
                    return;
                }
                show_detail_for_sel();
            }
            126 | 125 => {
                // Up (126): at the first list entry (or no selection), jump focus back to the
                // search field; clear the selection BEFORE entering so the highlight goes away
                // (the controlTextDidBeginEditing: delegate also clears - belt and braces).
                if keycode == 126 && (sel == 0 || sel == NO_SELECTION) {
                    if let Some(f) = *SEARCH_FIELD.lock().unwrap() {
                        // Clear only the previously selected row's highlight (incremental);
                        // do not rebuild the whole list.
                        set_picker_selection(NO_SELECTION);
                        refresh_selection(sel, NO_SELECTION);
                        if detail_visible() {
                            refresh_detail_action_visuals();
                        }
                        let window = match *PICKER_WINDOW.lock().unwrap() {
                            Some(w) => w.0,
                            None => return,
                        };
                        // makeFirstResponder: returns BOOL ('B').
                        let _: bool = msg_send![window, makeFirstResponder: f.0];
                        return;
                    }
                }
                let previous = sel;
                let idx = if let Some(next) = nav_arrow(keycode, sel, display_len) {
                    set_picker_selection(next);
                    next
                } else {
                    sel
                };
                refresh_selection(previous, idx);
                // scroll the selection into view.
                if let Some(container) = picker_container_ptr() {
                    scroll_selection_into_view(container, idx);
                }
                // The detail panel follows the selection live while open (Quick-Look-style
                // browsing).
                if detail_visible() {
                    show_detail_for_sel();
                }
            }
            36 => {
                // Enter; Option+Enter (when the setting is on) = paste and delete (one-shot
                // paste). Modifier bits follow NSEventModifierFlags as at the top of this
                // function: Command=0x10_0000, Option (Alternate)=0x08_0000.
                let idx = sel;
                if (mods & 0x0008_0000) != 0 {
                    paste_at_ex(idx, true);
                } else {
                    paste_at(idx);
                }
            }
            51 => {
                // Backspace (delete): remove the selected entry and refresh.
                let idx = sel;
                let Some(h_idx) = mapped_index(idx) else {
                    return;
                };
                let mut hist = CLIP_HISTORY.lock().unwrap();
                let Some(removed_entry) = remove_entry_for_undo(&mut hist, h_idx) else {
                    return;
                };
                // Deleting the selected row keeps the selection in place (pointing at the
                // next entry); an out-of-range selection (deleted the tail) is clamped to
                // the new tail by rebuild_rows after FILTERED is recomputed -- the old code
                // clamped against the stale pre-delete FILTERED length, so the selection
                // stayed past the new list, no row matched, and the highlight vanished.
                drop(hist);
                remember_deleted_clipboard_entry(removed_entry, h_idx);
                let incremental = try_delete_picker_row_incremental(idx);
                save_history();
                if !incremental {
                    schedule_picker_refresh();
                }
                // The detail panel follows the selected entry: the selection stayed in place
                // and now points at the entry that slid into the removed row's slot, so
                // retarget the panel instead of collapsing it (it closes only when nothing
                // remains to show).
                retarget_detail_after_delete(incremental);
            }
            53 => {
                if crate::clipboard::filter_popover_open() {
                    dismiss_filter_popover();
                    return;
                }
                // Esc: a pinned panel ignores the key entirely (design decision R1). The check is
                // HERE, above the existing actions, not inside hide_picker(): otherwise Esc would
                // still close the detail or clear the query on the way to a hide it no longer does.
                if is_pinned() {
                    return;
                }
                // Esc: with the detail open, the first press closes the detail (the picker
                // and the query stay untouched).
                if detail_visible() {
                    hide_detail();
                    return;
                }
                // Esc: a query gets cleared first (restoring the full list), a second press
                // closes. The search-field-focused first level is handled by the
                // NSSearchField subclass's cancelOperation:; this handles list focus.
                let had_query = with_clipboard_ui(|ui| !ui.search_query.is_empty());
                if had_query {
                    // The shared path, so the field, the clear button and the scope labels all
                    // agree; clearing the query by hand used to leave "results" on the buttons.
                    clear_search();
                    rebuild_rows();
                } else {
                    hide_picker();
                }
            }
            _ => {}
        }
    }
}

/// Compute the scroll offset from the viewport and selected-row geometry, avoiding the race-like
/// interaction between scrollRectToVisible and rapid row rebuilds.
pub(super) fn selection_scroll_offset(
    current: f64,
    viewport_h: f64,
    document_h: f64,
    y: f64,
    h: f64,
) -> f64 {
    let max_offset = (document_h - viewport_h).max(0.0);
    let target = if y < current {
        y
    } else if y + h > current + viewport_h {
        y + h - viewport_h
    } else {
        current
    };
    target.max(0.0).min(max_offset)
}

/// Scroll the selected row into view directly, without relying on AppKit's asynchronous
/// visibility adjustment.
pub(super) unsafe fn scroll_selection_into_view(container: *mut AnyObject, idx: usize) {
    let (y, h) = {
        let pitches = ROW_PITCHES.lock().unwrap();
        let Some(&h) = pitches.get(idx) else {
            return;
        };
        (row_top(idx, &pitches), h)
    };
    let scroll = match *SCROLL_VIEW.lock().unwrap() {
        Some(s) => s.0,
        None => return,
    };
    let clip: *mut AnyObject = msg_send![scroll, contentView];
    if clip.is_null() {
        return;
    }
    let bounds: NSRect = msg_send![clip, bounds];
    let document: NSRect = msg_send![container, frame];
    let target = selection_scroll_offset(
        bounds.origin.y,
        bounds.size.height,
        document.size.height,
        y,
        h,
    );
    if (target - bounds.origin.y).abs() > f64::EPSILON {
        // scrollPoint: uses document-view coordinates; explicit clamping prevents rapid
        // repeated events from being overwritten by a stale position.
        let _: () = msg_send![container, scrollPoint: NSPoint::new(0.0, target)];
    }
}

/// Refresh selection highlight by updating only the previous and new rows, without rebuilding.
pub(super) fn refresh_selection(previous: usize, current: usize) {
    update_hover_visuals(previous, current);
}

extern "C" fn container_accepts_first_responder(
    _self: *mut AnyObject,
    _cmd: Sel,
) -> objc2::runtime::Bool {
    // Objective-C BOOL, not Rust bool: this is what the class's "B@:" encoding declares.
    callback_guard::value(
        "container_accepts_first_responder",
        objc2::runtime::Bool::YES,
        || objc2::runtime::Bool::YES,
    )
}

extern "C" fn picker_window_can_become_key(
    _self: *mut AnyObject,
    _cmd: Sel,
) -> objc2::runtime::Bool {
    callback_guard::value(
        "picker_window_can_become_key",
        objc2::runtime::Bool::YES,
        || objc2::runtime::Bool::YES,
    )
}

/// The popover's frame inside the picker's content parent, centered under the options orb.
/// `parent_h`/`parent_flipped` are the
/// parent's own, so the top-anchored offset lands where the mock puts it whatever the view's
/// coordinate convention is -- the content parent is a plain (bottom-up) `NSView`.
/// Pure so the anchor math is testable.
#[cfg(test)]
pub(super) fn filter_popover_frame(parent_h: f64, parent_flipped: bool) -> NSRect {
    filter_popover_frame_for(parent_h, parent_flipped, PICKER_W)
}

pub(super) fn filter_popover_frame_for(
    parent_h: f64,
    parent_flipped: bool,
    panel_width: f64,
) -> NSRect {
    let h = filter_popover_height(parent_h);
    let (_, _, options, _, _) = picker_header_frames_for(panel_width);
    let max_x = (panel_width - POPOVER_RIGHT_INSET - POPOVER_W).max(0.0);
    let x = (options.origin.x + options.size.width / 2.0 - POPOVER_W / 2.0)
        .clamp(POPOVER_RIGHT_INSET.min(max_x), max_x);
    NSRect::new(
        NSPoint::new(
            x,
            top_anchored_origin_y(filter_popover_top(parent_h, h), parent_h, h, parent_flipped),
        ),
        NSSize::new(POPOVER_W, h),
    )
}

/// The popover's top offset below the panel's top edge: the mock's `POPOVER_TOP`, pulled up when
/// the menu plus that offset would not fit the panel (the picker shrinks to `picker_min_height`
/// when the list is short, and a menu hanging past the panel loses its housekeeping rows).
pub(super) fn filter_popover_top(parent_h: f64, popover_h: f64) -> f64 {
    let max_top = (parent_h - popover_h - POPOVER_BOTTOM_INSET).max(0.0);
    POPOVER_TOP.min(max_top)
}

/// The popover's fixed vertical parts: padding, the eyebrow and the hairline.
pub(super) fn filter_popover_fixed_h() -> f64 {
    // One section label and two dividers; filter rows retain their inter-row gap.
    POPOVER_PAD_V * 2.0 + POPOVER_TITLE_H + POPOVER_DIVIDER_H * 2.0 + 5.0
}

/// The menu's row height for a panel of `parent_h`: the mock's 32pt whenever the panel can hold the
/// whole menu below the anchor, otherwise compressed just enough that every row -- including the
/// housekeeping trio -- stays inside the panel. Clipping the last rows would hide "clear all" and
/// "settings" from exactly the panels most likely to be short.
pub(super) fn filter_popover_row_h(parent_h: f64) -> f64 {
    let rows = (CLIP_FILTER_ROWS.len() + super::popover_tail_row_count()) as f64;
    let room = parent_h - POPOVER_BOTTOM_INSET - POPOVER_TOP - filter_popover_fixed_h();
    (room / rows).clamp(POPOVER_MIN_ROW_H, POPOVER_ROW_H)
}

/// The popover's height for a panel of `parent_h`: the fixed parts plus one row per filter and the
/// two housekeeping rows, at the row height that panel allows.
pub(super) fn filter_popover_height(parent_h: f64) -> f64 {
    let rows = (CLIP_FILTER_ROWS.len() + super::popover_tail_row_count()) as f64;
    filter_popover_fixed_h() + rows * filter_popover_row_h(parent_h)
}

/// Place a child `top` points below its container's top edge, in the container's own convention.
/// One conversion for top-down layout: the picker's content parent is a plain (bottom-up) `NSView`
/// while the list container and the popover are flipped, and a top-down offset must mean the same
/// thing in both.
pub(super) fn top_anchored_origin_y(
    top: f64,
    container_h: f64,
    child_h: f64,
    flipped: bool,
) -> f64 {
    if flipped {
        top
    } else {
        container_h - top - child_h
    }
}

/// The inverse of `top_anchored_origin_y`: a child's distance below its container's top edge.
pub(super) fn top_offset_of(origin_y: f64, container_h: f64, child_h: f64, flipped: bool) -> f64 {
    if flipped {
        origin_y
    } else {
        container_h - origin_y - child_h
    }
}

/// How many history entries each popover filter row would show (the `.filter-row .count`).
pub(super) fn filter_popover_counts() -> [usize; 6] {
    let hist = CLIP_HISTORY.lock().unwrap();
    model::filter_row_counts(&hist)
}

unsafe fn add_popover_section_title(popover: *mut AnyObject, title: &str, y: f64) {
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let (span_x, span_w) = popover_row_span();
    let label: *mut AnyObject = msg_send![
        label,
        initWithFrame: NSRect::new(
            NSPoint::new(span_x + 8.0, y),
            NSSize::new(span_w - 16.0, POPOVER_TITLE_H)
        )
    ];
    let title_ns = make_nsstring(title);
    let _: () = msg_send![label, setStringValue: title_ns];
    CFRelease(title_ns as *const c_void);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let _: () = msg_send![label, setSelectable: false];
    let font: *mut AnyObject = msg_send![
        class!(NSFont),
        systemFontOfSize: 12.0f64,
        weight: crate::theme::FONT_WEIGHT_SEMIBOLD
    ];
    let _: () = msg_send![label, setFont: font];
    let _: () = msg_send![
        label,
        setTextColor: crate::glass::panel_ink(
            clipboard_palette().secondary_text,
            crate::glass::PanelInk::Secondary
        )
    ];
    let _: () = msg_send![popover, addSubview: label];
    release_obj(label);
}

unsafe fn add_popover_divider(popover: *mut AnyObject, y: f64, width: f64) {
    let divider: *mut AnyObject = msg_send![class!(NSView), alloc];
    let (span_x, _) = popover_row_span();
    let divider: *mut AnyObject = msg_send![
        divider,
        initWithFrame: NSRect::new(
            NSPoint::new(span_x + POPOVER_INSET, y + (POPOVER_DIVIDER_H - 1.0) / 2.0),
            NSSize::new(width - POPOVER_INSET * 2.0, 1.0)
        )
    ];
    let _: () = msg_send![divider, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![divider, layer];
    crate::ffi::layer_set_background(
        layer,
        crate::ffi::hex_to_cg_color(clipboard_palette().separator),
    );
    let _: () = msg_send![popover, addSubview: divider];
    release_obj(divider);
}

unsafe fn set_popover_symbol(
    button: *mut AnyObject,
    symbol: &str,
    size: f64,
    tint: *mut AnyObject,
) {
    let symbol_ns = make_nsstring(symbol);
    let image: *mut AnyObject = msg_send![
        class!(NSImage),
        imageWithSystemSymbolName: symbol_ns,
        accessibilityDescription: std::ptr::null::<AnyObject>()
    ];
    CFRelease(symbol_ns as *const c_void);
    if !image.is_null() {
        let config: *mut AnyObject = msg_send![
            class!(NSImageSymbolConfiguration),
            configurationWithPointSize: size,
            weight: crate::theme::FONT_WEIGHT_REGULAR
        ];
        let image: *mut AnyObject = msg_send![image, imageWithSymbolConfiguration: config];
        let _: () = msg_send![image, setTemplate: true];
        let _: () = msg_send![button, setImage: image];
        let _: () = msg_send![button, setImagePosition: 2isize]; // NSImageLeft
    }
    let _: () = msg_send![button, setContentTintColor: tint];
}

extern "C" fn popover_decoration_hit_test(
    _this: *mut AnyObject,
    _cmd: Sel,
    _point: NSPoint,
) -> *mut AnyObject {
    callback_guard::nil("popover_decoration_hit_test", std::ptr::null_mut)
}

fn popover_decoration_class() -> *mut AnyObject {
    static CLASS: OnceLock<StaticClass> = OnceLock::new();
    crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabPopoverDecoration", || {
        crate::ffi::build_class(
            c"OhMyTabPopoverDecoration",
            class!(NSView),
            |builder| unsafe {
                builder.add_method::<AnyObject, _>(
                    sel!(hitTest:),
                    popover_decoration_hit_test
                        as extern "C" fn(*mut AnyObject, Sel, NSPoint) -> *mut AnyObject,
                );
            },
        )
    })
    .0 as *mut AnyObject
}

unsafe fn add_popover_text(
    parent: *mut AnyObject,
    text: &str,
    frame: NSRect,
    size: f64,
    color: *mut AnyObject,
    alignment: isize,
) {
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let label: *mut AnyObject = msg_send![label, initWithFrame: frame];
    let value = make_nsstring(text);
    let _: () = msg_send![label, setStringValue: value];
    let _: () = msg_send![label, setToolTip: value];
    CFRelease(value as *const c_void);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let _: () = msg_send![label, setSelectable: false];
    let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: size];
    let _: () = msg_send![label, setFont: font];
    let _: () = msg_send![label, setTextColor: color];
    let _: () = msg_send![label, setAlignment: alignment];
    let cell: *mut AnyObject = msg_send![label, cell];
    let _: () = msg_send![cell, setLineBreakMode: 4isize];
    let _: () = msg_send![cell, setUsesSingleLineMode: true];
    let _: () = msg_send![parent, addSubview: label];
    release_obj(label);
}

unsafe fn add_popover_row_content(
    row: *mut AnyObject,
    label: &str,
    trailing: &str,
    symbol: &str,
    filter: bool,
    selected: bool,
    danger: bool,
) {
    let palette = clipboard_palette();
    let bounds: NSRect = msg_send![row, bounds];
    // One decoration declines hit testing for its entire subtree, preserving the row's click target.
    let content: *mut AnyObject = msg_send![popover_decoration_class(), alloc];
    let content: *mut AnyObject = msg_send![content, initWithFrame: bounds];
    let ink = if danger {
        super::popover_danger(palette)
    } else {
        palette.primary_text
    };
    let muted = if danger {
        ink
    } else {
        super::popover_muted(palette)
    };
    let icon: *mut AnyObject = msg_send![class!(NSButton), alloc];
    let icon: *mut AnyObject = msg_send![icon, initWithFrame: NSRect::new(NSPoint::new(POPOVER_INSET, (bounds.size.height - POPOVER_GLYPH_W) / 2.0), NSSize::new(POPOVER_GLYPH_W, POPOVER_GLYPH_W))];
    let _: () = msg_send![icon, setBordered: false];
    set_popover_symbol(
        icon,
        symbol,
        POPOVER_META_SIZE,
        if danger {
            crate::ffi::hex_to_ns_color(ink)
        } else if selected {
            crate::glass::panel_ink(ink, crate::glass::PanelInk::Primary)
        } else {
            crate::glass::panel_ink(palette.secondary_text, crate::glass::PanelInk::Secondary)
        },
    );
    let _: () = msg_send![icon, setImagePosition: 1isize];
    let _: () = msg_send![content, addSubview: icon];
    release_obj(icon);
    let end = bounds.size.width - POPOVER_INSET;
    let check_x = end - POPOVER_GLYPH_W;
    let trailing_end = if filter {
        check_x - POPOVER_COLUMN_GAP
    } else {
        end
    };
    // A row without a trailing hint (the settings action: its ⌘, chord only works from the menu
    // bar, so the panel must not advertise it) paints no trailing label at all.
    let trailing_w = localized_string_width(trailing, POPOVER_META_SIZE);
    let text_x = POPOVER_INSET + POPOVER_GLYPH_W + POPOVER_COLUMN_GAP;
    let trailing_x = (trailing_end - trailing_w - 4.0).max(text_x);
    let text_y = (bounds.size.height - POPOVER_TEXT_H) / 2.0;
    add_popover_text(
        content,
        label,
        NSRect::new(
            NSPoint::new(text_x, text_y),
            NSSize::new(
                (trailing_x - POPOVER_COLUMN_GAP - text_x).max(0.0),
                POPOVER_TEXT_H,
            ),
        ),
        POPOVER_TEXT_SIZE,
        if danger {
            crate::ffi::hex_to_ns_color(ink)
        } else {
            crate::glass::panel_ink(ink, crate::glass::PanelInk::Primary)
        },
        0,
    );
    if !trailing.is_empty() {
        add_popover_text(
            content,
            trailing,
            NSRect::new(
                NSPoint::new(trailing_x, text_y),
                NSSize::new(trailing_end - trailing_x, POPOVER_TEXT_H),
            ),
            POPOVER_META_SIZE,
            if danger {
                crate::ffi::hex_to_ns_color(muted)
            } else {
                crate::glass::panel_ink(muted, crate::glass::PanelInk::Secondary)
            },
            2,
        );
    }
    if selected {
        add_popover_text(
            content,
            "✓",
            NSRect::new(
                NSPoint::new(check_x, text_y),
                NSSize::new(POPOVER_GLYPH_W, POPOVER_TEXT_H),
            ),
            POPOVER_META_SIZE,
            crate::ffi::hex_to_ns_color(super::popover_check_color(palette)),
            1,
        );
    }
    let accessible = if trailing.is_empty() {
        make_nsstring(label)
    } else {
        make_nsstring(&format!("{label}, {trailing}"))
    };
    let _: () = msg_send![row, setAccessibilityLabel: accessible];
    let _: () = msg_send![row, setToolTip: accessible];
    CFRelease(accessible as *const c_void);
    let empty = make_nsstring("");
    let _: () = msg_send![row, setTitle: empty];
    CFRelease(empty as *const c_void);
    let _: () = msg_send![row, setImage: std::ptr::null::<AnyObject>()];
    let _: () = msg_send![row, addSubview: content];
    release_obj(content);
}

unsafe fn add_shared_popover_backdrop(
    popover: *mut AnyObject,
    size: NSSize,
) -> crate::glass::InstalledBackdrop {
    let bounds = NSRect::new(NSPoint::new(0.0, 0.0), size);
    let decoration: *mut AnyObject = msg_send![popover_decoration_class(), alloc];
    let decoration: *mut AnyObject = msg_send![decoration, initWithFrame: bounds];
    let _: () = msg_send![decoration, setAutoresizingMask: 18u64];
    let options = picker_backdrop_options().with_elevation(None);
    // `build_backdrop` already applied this material's live properties (tuning and glass tint) as it
    // built them, so a freshly built hierarchy has nothing to re-apply here.
    let backdrop = crate::glass::install_embedded_backdrop(decoration, bounds, POPOVER_R, options);
    let _: () = msg_send![backdrop.content_parent, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![backdrop.content_parent, layer];
    if !layer.is_null() {
        let tint: *mut AnyObject = msg_send![class!(CAGradientLayer), layer];
        let _: () = msg_send![tint, setFrame: bounds];
        let _: () = msg_send![tint, setCornerRadius: POPOVER_R];
        crate::ffi::layer_set_continuous_corner_curve(tint);
        let palette = clipboard_palette();
        let cg_colors = [
            crate::ffi::hex_to_cg_color(if palette.dark {
                POPOVER_LIFT_TOP_DARK
            } else {
                POPOVER_LIFT_TOP_LIGHT
            }),
            crate::ffi::hex_to_cg_color(if palette.dark {
                0
            } else {
                POPOVER_LIFT_BOTTOM_LIGHT
            }),
        ];
        let colors = crate::ffi::CFArrayCreate(
            std::ptr::null(),
            cg_colors.as_ptr() as *const *const c_void,
            cg_colors.len() as isize,
            &crate::ffi::kCFTypeArrayCallBacks as *const c_void,
        );
        if !colors.is_null() {
            let _: () = msg_send![tint, setColors: colors as *mut AnyObject];
            crate::ffi::CFRelease(colors);
        }
        // The shared backdrop's content parent is unflipped.
        let _: () = msg_send![tint, setStartPoint: NSPoint::new(0.5, 1.0)];
        let _: () = msg_send![tint, setEndPoint: NSPoint::new(0.5, 0.0)];
        let _: () = msg_send![layer, addSublayer: tint];
    }
    let _: () = msg_send![popover, addSubview: decoration];
    release_obj(decoration);
    backdrop
}

/// Draw the menu's rim ABOVE its material.
///
/// A layer border on the popover itself is painted under its subviews, and the shared material
/// fills the popover exactly, so that stroke is invisible wherever the material is opaque -- which
/// is the one case where the rim has to do the separating (§3.4: the rim, not a whiter fill, is what
/// marks the menu off from the panel). This adds a transparent decoration on top of the material and
/// strokes its layer instead, so the rim is always the topmost thing the menu draws.
unsafe fn add_popover_rim(popover: *mut AnyObject, size: NSSize) {
    let palette = clipboard_palette();
    let rim: *mut AnyObject = msg_send![popover_decoration_class(), alloc];
    let rim: *mut AnyObject =
        msg_send![rim, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), size)];
    let _: () = msg_send![rim, setAutoresizingMask: 18u64];
    let _: () = msg_send![rim, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![rim, layer];
    if !layer.is_null() {
        let _: () = msg_send![layer, setCornerRadius: POPOVER_R];
        crate::ffi::layer_set_continuous_corner_curve(layer);
        let _: () = msg_send![layer, setBorderWidth: POPOVER_BORDER_W];
        crate::ffi::layer_set_border(
            layer,
            crate::ffi::hex_to_cg_color(if palette.dark {
                palette.card_border
            } else {
                PICKER_POPOVER_BORDER_LIGHT
            }),
        );
    }
    // Above every other subview, including the rows added after this call.
    let _: () = msg_send![
        popover,
        addSubview: rim,
        positioned: 1isize,
        relativeTo: std::ptr::null::<AnyObject>()
    ];
    release_obj(rim);
}

/// Build (or rebuild) the options popover and add it to the picker's content parent, above the
/// rows so its hit-tests win. Filters, settings and clearing actions form separate groups.
pub(super) unsafe fn build_filter_popover(parent: *mut AnyObject) {
    // Remove any previous view WITHOUT closing the popover: the open flag was set by the caller
    // (`toggle_filter_popover`) and must survive the rebuild, or the view would exist with the state
    // reporting it closed -- outside clicks, keyboard navigation and `--e2e-state` all read that flag.
    remove_filter_popover_views();
    let palette = clipboard_palette();
    let parent_bounds: NSRect = msg_send![parent, bounds];
    let parent_flipped: objc2::runtime::Bool = msg_send![parent, isFlipped];
    let frame = filter_popover_frame_for(
        parent_bounds.size.height,
        parent_flipped.as_bool(),
        parent_bounds.size.width,
    );
    // Flipped, like the list container: the rows below are laid out top-down, so their visual order
    // matches `POPOVER_KEY_ROW`'s index order and the maths stays the same as the mock's.
    let popover: *mut AnyObject = msg_send![filter_popover_class(), alloc];
    let popover: *mut AnyObject = msg_send![popover, initWithFrame: frame];
    // NSViewMinYMargin (8): the bottom gap adapts, so a panel resize keeps the popover pinned to
    // the top -- the same mask the header strip uses.
    let _: () = msg_send![popover, setAutoresizingMask: 8u64];
    let _: () = msg_send![popover, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![popover, layer];
    if !layer.is_null() {
        let _: () = msg_send![layer, setCornerRadius: POPOVER_R];
        crate::ffi::layer_set_continuous_corner_curve(layer);
        let _: () = msg_send![layer, setMasksToBounds: false];
        crate::ffi::layer_set_background(layer, crate::ffi::hex_to_cg_color(0));
        // The rim is NOT stroked here: a border on this layer is painted under the material, which
        // fills the popover exactly, so an opaque material would hide it completely. It is drawn on
        // its own topmost decoration instead (`add_popover_rim`), after the rows are in place.
        // A menu floats at the style document's `med` elevation (§7), so it takes that level's
        // shared shadow rather than a shadow of its own: the mock's `0 18px 44px` was a shadow for
        // a surface 26pt above the page, and it is what made the menu look pasted on.
        let shadow = crate::ffi::hex_to_cg_color(crate::theme::ELEVATION_MED_SHADOW_COLOR);
        crate::ffi::layer_set_shadow_color(layer, shadow);
        let _: () = msg_send![layer, setShadowOpacity: crate::theme::ELEVATION_MED_SHADOW_OPACITY];
        let _: () = msg_send![layer, setShadowRadius: crate::theme::ELEVATION_MED_SHADOW_RADIUS];
        let _: () = msg_send![layer, setShadowOffset: NSSize::new(0.0, crate::theme::ELEVATION_MED_SHADOW_OFFSET_Y)];
    }

    let backdrop = add_shared_popover_backdrop(popover, frame.size);

    // `.pop-title`: the filter section label, followed by six 32px filter rows.
    let labels = localized_filter_labels();
    let counts = filter_popover_counts();
    let active = *CLIP_FILTER.lock().unwrap();
    let (row_x, row_w) = popover_row_span();
    // The same row height the frame was measured with: a short panel compresses the rows so the
    // housekeeping pair stays inside it.
    let row_h = filter_popover_row_h(parent_bounds.size.height);
    add_popover_section_title(popover, &t("clipboard.filter_section"), POPOVER_PAD_V);
    let mut y = POPOVER_PAD_V + POPOVER_TITLE_H;
    let key_row = POPOVER_KEY_ROW.load(Ordering::SeqCst);
    for (i, f) in CLIP_FILTER_ROWS.iter().enumerate() {
        let row_frame = NSRect::new(NSPoint::new(row_x, y), NSSize::new(row_w, row_h));
        let row: *mut AnyObject = msg_send![hover_button_class(), alloc];
        let row: *mut AnyObject = msg_send![row, initWithFrame: row_frame];
        let _: () = msg_send![row, setBordered: false];
        let _: () = msg_send![row, setWantsLayer: true];
        let rl: *mut AnyObject = msg_send![row, layer];
        if !rl.is_null() {
            let _: () = msg_send![rl, setCornerRadius: POPOVER_ROW_R];
            if *f == active || i == key_row {
                crate::ffi::layer_set_background(
                    rl,
                    crate::ffi::hex_to_cg_color(picker_popover_active_bg(palette)),
                );
            }
        }
        let symbol = match f {
            ClipFilter::All => "text.bubble",
            ClipFilter::Text => "text.alignleft",
            ClipFilter::Image => "photo",
            ClipFilter::Link => "link",
            ClipFilter::Code => "chevron.left.forwardslash.chevron.right",
            ClipFilter::Pinned => "pin",
        };
        add_popover_row_content(
            row,
            &labels[i],
            &counts[i].to_string(),
            symbol,
            true,
            *f == active,
            false,
        );
        let _: () = msg_send![row, setTag: i as isize];
        let _: () = msg_send![row, setTarget: observer()];
        let _: () = msg_send![row, setAction: sel!(popoverFilterSelected:)];
        add_hover_tracking(row);
        let _: () = msg_send![popover, addSubview: row];
        release_obj(row);
        y += row_h;
        if i + 1 < CLIP_FILTER_ROWS.len() {
            y += 1.0;
        }
    }

    add_popover_divider(popover, y, row_w);
    y += POPOVER_DIVIDER_H;
    let filter = *CLIP_FILTER.lock().unwrap();
    let has_query = with_clipboard_ui(|ui| !ui.search_query.is_empty());
    let (clear_label, clear_all_label) = model::popover_clear_labels(filter, has_query);

    let mut tails = Vec::with_capacity(super::popover_tail_row_count());
    tails.push((
        "clipboard.open_settings",
        sel!(openClipboardSettings:),
        false,
    ));
    tails.push((
        "clipboard.clear_button",
        sel!(clearClipboardHistory:),
        false,
    ));
    if super::text_style::clear_all_button_visible() {
        tails.push(("clipboard.clear_all", sel!(clearClipboardAll:), true));
    }
    for (tail_index, (key, action, danger)) in tails.into_iter().enumerate() {
        if tail_index == 1 {
            add_popover_divider(popover, y, row_w);
            y += POPOVER_DIVIDER_H;
        }
        let row_frame = NSRect::new(NSPoint::new(row_x, y), NSSize::new(row_w, row_h));
        let row: *mut AnyObject = msg_send![hover_button_class(), alloc];
        let row: *mut AnyObject = msg_send![row, initWithFrame: row_frame];
        let _: () = msg_send![row, setBordered: false];
        let _: () = msg_send![row, setWantsLayer: true];
        let rl: *mut AnyObject = msg_send![row, layer];
        if !rl.is_null() {
            let _: () = msg_send![rl, setCornerRadius: POPOVER_ROW_R];
            if CLIP_FILTER_ROWS.len() + tail_index == key_row {
                crate::ffi::layer_set_background(
                    rl,
                    crate::ffi::hex_to_cg_color(picker_popover_active_bg(palette)),
                );
            }
        }
        let label = match key {
            "clipboard.clear_button" => clear_label.clone(),
            "clipboard.clear_all" => clear_all_label.clone(),
            _ => t(key),
        };
        let symbol = if key == "clipboard.open_settings" {
            "gearshape"
        } else {
            "trash"
        };
        add_popover_row_content(row, &label, "", symbol, false, false, danger);
        // Remember the settings row: it is the control that actually opens the settings window, and a
        // scenario that wants to click it cannot derive its place from the orb (the orb's action opens
        // this popover). The pointer is taken once, here, where the row is built.
        if key == "clipboard.open_settings" {
            *super::PICKER_SETTINGS_ROW.lock().unwrap() = Some(ObjPtr::new(row));
        }
        let _: () = msg_send![row, setTarget: observer()];
        let _: () = msg_send![row, setAction: action];
        add_hover_tracking(row);
        let _: () = msg_send![popover, addSubview: row];
        release_obj(row);
        y += row_h;
    }

    // The rim goes on last so it is the topmost thing the menu draws, above its own material.
    add_popover_rim(popover, frame.size);

    // Top of the sibling list, not the bottom: the blank-area drag surface is a sibling that
    // covers the whole content parent, so a popover appended below it would be painted under the
    // surface and receive no press at all -- the press would start a drag, and the drag surface's
    // point is outside the popover's frame, so the window-wide click monitor closed the menu the
    // user had just opened. NSWindowAbove = 1 with a nil relative view is "above everything".
    let _: () = msg_send![
        parent,
        addSubview: popover,
        positioned: 1isize,
        relativeTo: std::ptr::null::<AnyObject>()
    ];
    crate::glass::finish_embedded_backdrop(backdrop, POPOVER_R);
    release_obj(popover);
    *FILTER_POPOVER.lock().unwrap() = Some(ObjPtr::new(popover));
}

/// Keep the options popover in step with its open flag: build on open, remove on close. The
/// flag flips in `toggle_filter_popover`, so `--e2e-state` always reports the intent.
pub(super) unsafe fn sync_filter_popover(open: bool) {
    if open {
        if let Some(parent) = *PICKER_CONTENT_PARENT.lock().unwrap() {
            build_filter_popover(parent.0);
        }
    } else {
        dismiss_filter_popover();
    }
}

/// The popover class: flipped, so its rows are laid out downwards from the top edge like the mock's
/// `.popover` (and like the list container, whose index order the keyboard highlight follows).
fn filter_popover_class() -> *mut AnyObject {
    static CLASS: OnceLock<StaticClass> = OnceLock::new();
    crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabClipboardPopoverView", || {
        crate::ffi::build_class(
            c"OhMyTabClipboardPopoverView",
            class!(NSView),
            |builder| unsafe {
                builder.add_method::<AnyObject, _>(
                    sel!(isFlipped),
                    filter_popover_is_flipped
                        as unsafe extern "C" fn(*mut AnyObject, Sel) -> objc2::runtime::Bool,
                );
            },
        )
    })
    .0 as *mut AnyObject
}

/// The popover is flipped: origin at its top-left, rows stack downwards.
extern "C" fn filter_popover_is_flipped(_self: *mut AnyObject, _cmd: Sel) -> objc2::runtime::Bool {
    callback_guard::value(
        "filter_popover_is_flipped",
        objc2::runtime::Bool::YES,
        || objc2::runtime::Bool::YES,
    )
}

/// Remove the popover's view without touching the open flag. `build_filter_popover` calls this so a
/// rebuild cannot close the popover it is building; every closing path goes through
/// `dismiss_filter_popover` instead, which does both.
pub(super) fn remove_filter_popover_views() {
    if let Some(popover) = *FILTER_POPOVER.lock().unwrap() {
        unsafe {
            let _: () = msg_send![popover.0, removeFromSuperview];
        }
    }
    *FILTER_POPOVER.lock().unwrap() = None;
    // The settings row went away with the popover: drop its pointer here, so a reader can never
    // dereference a released view between the removal and the next build.
    *super::PICKER_SETTINGS_ROW.lock().unwrap() = None;
}

/// Close the popover: clear the open flag and remove the view (outside click, Esc, choice, panel
/// hide).
pub(super) fn dismiss_filter_popover() {
    super::drag::clear_pending_popover_dismiss();
    crate::clipboard::set_filter_popover_open(false);
    POPOVER_KEY_ROW.store(usize::MAX, Ordering::SeqCst);
    remove_filter_popover_views();
    if let Some(parent) = *PICKER_CONTENT_PARENT.lock().unwrap() {
        unsafe { super::drag::raise_drag_surface(parent.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::keyring::{self, KeyUnavailable};
    use crate::clipboard::storage::{self, StorageState};
    use std::sync::atomic::Ordering;

    /// Restores process-global state on drop so a failing assertion cannot leak into the parallel
    /// suite while the storage test lock is still held.
    struct Restore<F: FnMut()>(F);

    impl<F: FnMut()> Drop for Restore<F> {
        fn drop(&mut self) {
            (self.0)();
        }
    }

    /// The table's last row ends on the document's bottom edge: the mock's `margin-top: auto`
    /// plus its own bottom padding is what pins a short body's table to the pane's bottom.
    #[test]
    fn last_meta_row_sits_on_the_document_bottom_edge() {
        let pane = detail_pane_layout(DETAIL_MAX_W, 520.0, meta_table_field_count());
        let doc_h = detail_document_height(pane.scroll.size.height, pane.body_h, pane.meta.height);
        assert_eq!(doc_h, pane.scroll.size.height);
        let (_, last_value) = meta_row_frames(&pane.meta, MetaField::ALL.len() - 1, pane.content_w);
        let table_y = doc_h - pane.meta.height;
        assert!(
            (table_y + last_value.origin.y + last_value.size.height - doc_h).abs() <= 1.0,
            "the last meta row ends at the document's bottom edge, so it sits on the table's own bottom padding"
        );
    }

    /// The source icon is gated by the same "show source app" switch as the name: off means no
    /// icon key at all, so a hidden source cannot leak through its icon.
    #[test]
    fn meta_source_icon_key_follows_the_show_source_switch() {
        let mut entry = text_entry("x");
        entry.source_key = "com.example.app".to_string();
        assert_eq!(meta_source_icon_key(&entry, true), Some("com.example.app"));
        assert_eq!(meta_source_icon_key(&entry, false), None);
        entry.source_key.clear();
        assert_eq!(meta_source_icon_key(&entry, true), None);
    }

    /// Below the two-column minimum the table falls back to one field per row rather than
    /// squeezing the second column into a zero-width value.
    #[test]
    fn meta_table_falls_back_to_one_column_when_narrow() {
        let fields = meta_table_field_count();
        let wide = meta_table_frames(DETAIL_META_TWO_COL_MIN + 100.0, fields);
        assert!(wide.two_col);
        assert_eq!(wide.rows, fields.div_ceil(2));
        let narrow = meta_table_frames(DETAIL_META_TWO_COL_MIN - 1.0, fields);
        assert!(!narrow.two_col);
        assert_eq!(narrow.rows, fields);
        // The fallback's right_x sits at the content edge, so no right pair can draw and the
        // single column's value keeps the full row.
        let (_, value) = meta_row_frames(&narrow, 1, DETAIL_META_TWO_COL_MIN - 1.0);
        assert_eq!(
            value.origin.x + value.size.width,
            DETAIL_META_TWO_COL_MIN - 1.0
        );
    }

    fn text_entry(text: &str) -> ClipEntry {
        ClipEntry {
            text: text.to_string(),
            image: None,
            pinned: false,
            source_app: String::new(),
            source_key: String::new(),
            copied_at: None,
        }
    }

    #[test]
    fn picker_inactive_compensation_alpha_follows_the_static() {
        let _lock = storage::test_lock();
        PICKER_GLASS_INACTIVE.store(false, Ordering::SeqCst);
        assert_eq!(picker_inactive_compensation_alpha(), 0);
        PICKER_GLASS_INACTIVE.store(true, Ordering::SeqCst);
        assert_eq!(
            picker_inactive_compensation_alpha(),
            crate::glass::INACTIVE_GLASS_COMPENSATION_ALPHA
        );
        PICKER_GLASS_INACTIVE.store(false, Ordering::SeqCst);
    }

    #[test]
    fn selection_after_pin_follows_the_setting_and_the_filtered_list() {
        let _lock = storage::test_lock();
        let saved_config = CONFIG.read().unwrap().clone();
        let saved_selection = picker_selection();
        let saved_filtered = with_clipboard_ui(|ui| ui.filtered.clone());
        let _restore = Restore(move || {
            *CONFIG.write().unwrap() = saved_config.clone();
            set_picker_selection(saved_selection);
            with_clipboard_ui(|ui| ui.filtered = saved_filtered.clone());
        });

        with_clipboard_ui(|ui| ui.filtered = vec![5, 7, 9]);

        // (a) follow = true and the index is present: select its display position.
        {
            let mut cfg = CONFIG.read().unwrap().clone();
            cfg.clipboard.pin_follow_selection = true;
            *CONFIG.write().unwrap() = cfg;
        }
        set_picker_selection(0);
        assert!(selection_after_pin(7));
        assert_eq!(picker_selection(), 1);

        // (b) follow = true but the index is absent: no move.
        set_picker_selection(0);
        assert!(!selection_after_pin(99));
        assert_eq!(picker_selection(), 0);

        // (c) follow = false: never move, even when present.
        {
            let mut cfg = CONFIG.read().unwrap().clone();
            cfg.clipboard.pin_follow_selection = false;
            *CONFIG.write().unwrap() = cfg;
        }
        set_picker_selection(0);
        assert!(!selection_after_pin(7));
        assert_eq!(picker_selection(), 0);
    }

    #[test]
    fn empty_state_hint_prioritizes_storage_then_empty_then_no_match() {
        let _lock = storage::test_lock();
        let saved_state = storage::state();
        let saved_history = CLIP_HISTORY.lock().unwrap().clone();
        let saved_filtered = with_clipboard_ui(|ui| ui.filtered.clone());
        let _restore = Restore(move || {
            match saved_state.clone() {
                StorageState::Unavailable(reason) => storage::set_unavailable(reason),
                StorageState::Blocked(reason) => storage::set_blocked(reason),
                StorageState::Ready => storage::set_ready(keyring::test_key()),
                // state() never reports this in a test process.
                StorageState::Uninitialized => {}
            }
            *CLIP_HISTORY.lock().unwrap() = saved_history.clone();
            with_clipboard_ui(|ui| ui.filtered = saved_filtered.clone());
        });

        // (a) A storage failure outranks every list state.
        storage::set_unavailable(KeyUnavailable::Missing);
        assert_eq!(
            empty_state_hint(),
            crate::clipboard::unavailable_copy().unwrap()
        );

        // From here the storage is healthy, so the list decides.
        storage::set_ready(keyring::test_key());
        CLIP_HISTORY.lock().unwrap().clear();
        with_clipboard_ui(|ui| ui.filtered = Vec::new());
        // (b) No history at all.
        assert_eq!(empty_state_hint(), crate::i18n::t("clipboard.empty"));

        // (c) History exists but the current query/filter matches nothing.
        *CLIP_HISTORY.lock().unwrap() = vec![text_entry("copied")];
        with_clipboard_ui(|ui| ui.filtered = Vec::new());
        assert_eq!(empty_state_hint(), crate::i18n::t("clipboard.no_match"));

        // (d) A matching row: no hint.
        with_clipboard_ui(|ui| ui.filtered = vec![0]);
        assert_eq!(empty_state_hint(), "");
    }
}
