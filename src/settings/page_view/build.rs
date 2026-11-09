//! Building one page: description -> layout -> views, in that order, and nothing else.
//!
//! `build_page` is the only function that turns a `PageView` into AppKit views. It captures the
//! description once, lays it out once, renders once, and publishes the result; a page whose
//! build refuses keeps the previous state rather than half-replacing it.

use super::super::*;
use super::*;

/// What a migrated page replaces when its description is rendered: its rows' identities.
/// The page's description, for the pages that have one.
pub(super) fn view_for(page: SettingsPageId, input: &PageInput) -> PageView {
    match page {
        SettingsPageId::Switcher => switcher_page_view(input),
        SettingsPageId::General => general_page_view(input),
        SettingsPageId::KeyBindings => key_bindings_page_view(input),
        SettingsPageId::KeystrokeDisplay => keystroke_display_page_view(input),
        SettingsPageId::FinderMenu => finder_menu_page_view(input),
        SettingsPageId::Gestures => gestures_page_view(input),
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
pub(in crate::settings) unsafe fn build_page(
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
pub(in crate::settings) struct BuiltPage {
    pub(in crate::settings) view: PageView,
    pub(in crate::settings) rendered: RenderedPage,
    pub(in crate::settings) canvas: PageCanvas,
    pub(in crate::settings) content_bottom: f64,
}

/// The page's REAL hooks, built against the headless UI: a page whose hook draws rows cannot be
/// checked by skipping it.
pub(super) unsafe fn page_hooks(page: SettingsPageId) -> Box<dyn PageHooks> {
    match page {
        SettingsPageId::About => Box::new(AboutHooks {
            window: std::ptr::null_mut(),
        }),
        SettingsPageId::Clipboard => Box::new(ClipboardHooks {
            shortcut_title: String::new(),
            recording: false,
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
