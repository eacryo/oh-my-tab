//! Shared context for building settings pages.

use super::*;

/// Common geometry and runtime inputs shared by every settings page builder.
pub(super) struct SettingsPageBuildContext {
    pub(super) content: *mut AnyObject,
    pub(super) content_w: f64,
    /// The detail pane's left edge in the content view: the banner's final parent there.
    pub(super) page_x: f64,
    /// The VISIBLE page viewport height (the clip), which the banner hangs from.
    pub(super) page_viewport_h: f64,
    pub(super) page_frame: NSRect,
    pub(super) palette: UiPalette,
    pub(super) layout: SettingsLayout,
    pub(super) target: *mut AnyObject,
}

pub(super) struct SettingsPageFinalization {
    pub(super) roots: [*mut AnyObject; SETTINGS_PAGE_COUNT],
    pub(super) documents: [*mut AnyObject; SETTINGS_PAGE_COUNT],
    pub(super) bottoms: [f64; SETTINGS_PAGE_COUNT],
}

/// Build the Switcher page from its description.
///
/// The page is data now: `switcher_page_view` says what it is (rows, copy keys, identities,
/// values, search entries), `layout_rows` says where everything sits, `render_page` applies that,
/// and `page_update_description` is what the refill and the in-place refresh write. There is no
/// second, inline construction path for this page.
pub(super) unsafe fn build_switcher_page(
    context: &SettingsPageBuildContext,
    switcher_view: *mut AnyObject,
    switcher_doc_h: f64,
    ui: &mut SettingsUi,
) -> f64 {
    begin_settings_search_page();
    let input = PageInput::capture();
    match build_page(
        ui,
        SettingsPageId::Switcher,
        switcher_view,
        context.content_w,
        context.layout,
        switcher_doc_h,
        context.page_frame.size.height,
        context.page_x,
        context.page_viewport_h,
        &input,
        context.target,
        &mut page_view::NoHooks,
    ) {
        Ok(built) => publish_built_page(ui, SettingsPageId::Switcher, built),
        Err(_) => {
            // A first build has no previous state: the page stays empty and nothing is
            // published, which is better than a page that looks alive but is not wired.
            empty_page(
                ui,
                SettingsPageId::Switcher,
                switcher_view,
                context,
                switcher_doc_h,
            )
        }
    }
}
/// Build the Keystroke Display page from its description.
///
/// The page is data now: `keystroke_display_page_view` says what it is (rows, copy keys, identities,
/// values, search entries), `layout_rows` says where everything sits, `render_page` applies that,
/// and `page_update_description` is what the refill and the in-place refresh write. There is no
/// second, inline construction path for this page.
pub(super) unsafe fn build_keystroke_display_page(
    context: &SettingsPageBuildContext,
    keystroke_display_view: *mut AnyObject,
    keystroke_display_doc_h: f64,
    ui: &mut SettingsUi,
) -> f64 {
    begin_settings_search_page();
    let input = PageInput::capture();
    match build_page(
        ui,
        SettingsPageId::KeystrokeDisplay,
        keystroke_display_view,
        context.content_w,
        context.layout,
        keystroke_display_doc_h,
        context.page_frame.size.height,
        context.page_x,
        context.page_viewport_h,
        &input,
        context.target,
        &mut page_view::NoHooks,
    ) {
        Ok(built) => publish_built_page(ui, SettingsPageId::KeystrokeDisplay, built),
        Err(_) => {
            // A first build has no previous state: the page stays empty and nothing is
            // published, which is better than a page that looks alive but is not wired.
            empty_page(
                ui,
                SettingsPageId::KeystrokeDisplay,
                keystroke_display_view,
                context,
                keystroke_display_doc_h,
            )
        }
    }
}

/// Build the Finder Menu page from its description.
///
/// Same shape as the Switcher page: `finder_menu_page_view` says what the page is (rows, copy keys,
/// identities, values, search entries), `layout_rows` says where it sits, `render_page` applies it,
/// and `page_update_description` is what the refill and the in-place refresh write. The page's legacy
/// `SettingsUi` pointers are bound by `FinderMenuHooks` after the build, so there is one
/// construction path for this page and no inline builder left.
pub(super) unsafe fn build_finder_menu_page(
    context: &SettingsPageBuildContext,
    finder_menu_view: *mut AnyObject,
    finder_menu_doc_h: f64,
    ui: &mut SettingsUi,
) -> f64 {
    begin_settings_search_page();
    let input = PageInput::capture();
    match build_page(
        ui,
        SettingsPageId::FinderMenu,
        finder_menu_view,
        context.content_w,
        context.layout,
        finder_menu_doc_h,
        context.page_frame.size.height,
        context.page_x,
        context.page_viewport_h,
        &input,
        context.target,
        &mut page_view::FinderMenuHooks::default(),
    ) {
        Ok(built) => publish_built_page(ui, SettingsPageId::FinderMenu, built),
        Err(_) => {
            // A first build has no previous state: the page stays empty and nothing is published,
            // which is better than a page that looks alive but is not wired.
            empty_page(
                ui,
                SettingsPageId::FinderMenu,
                finder_menu_view,
                context,
                finder_menu_doc_h,
            )
        }
    }
}

/// The keystroke panel cap slider range, in whole percent. Shared with dispatch.rs so the
/// range is stated once.
pub(super) fn max_extent_slider_min() -> i64 {
    crate::config::KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_MIN as i64
}

pub(super) fn max_extent_slider_max() -> i64 {
    crate::config::KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_MAX as i64
}

/// Slider integer -> config value. Only the slider own steps can arrive, but a stale or
/// hand-set value is snapped rather than trusted, so the persisted number is always valid.
pub(super) fn max_extent_slider_value(percent: u32) -> i64 {
    crate::config::keystroke_display_max_extent_percent_snap(percent) as i64
}

/// The row readout: the percentage, which is the unit the slider itself speaks.
pub(super) fn max_extent_readout(percent: u32) -> String {
    format!(
        "{}%",
        crate::config::keystroke_display_max_extent_percent_snap(percent)
    )
}

/// Write the readout text of the panel-cap slider. Called from the change callback, so the number
/// follows the drag; it formats through the same helper the row was built with.
pub(super) unsafe fn set_max_extent_readout(label: *mut AnyObject, percent: u32) {
    if label.is_null() {
        return;
    }
    set_field(label, max_extent_readout(percent));
}

unsafe impl Send for SettingsPageBuildContext {}
unsafe impl Sync for SettingsPageBuildContext {}

/// Build the General page from its description.
///
/// Same shape as the Switcher page: the description says what the page is (rows, copy keys,
/// identities, the permission banner), `layout_rows` says where it sits, `render_page` applies
/// that, and `page_update_description` is what the refill and the in-place refresh write. The
/// inline construction is deleted, so the page has one path.
pub(super) unsafe fn build_general_page(
    context: &SettingsPageBuildContext,
    general_view: *mut AnyObject,
    general_doc_h: f64,
    ui: &mut SettingsUi,
) -> f64 {
    begin_settings_search_page();
    let input = PageInput::capture();
    match build_page(
        ui,
        SettingsPageId::General,
        general_view,
        context.content_w,
        context.layout,
        general_doc_h,
        context.page_frame.size.height,
        context.page_x,
        context.page_viewport_h,
        &input,
        context.target,
        &mut page_view::NoHooks,
    ) {
        Ok(built) => publish_built_page(ui, SettingsPageId::General, built),
        Err(_) => empty_page(
            ui,
            SettingsPageId::General,
            general_view,
            context,
            general_doc_h,
        ),
    }
}
/// Build the Mouse page: the described rows, with the rows whose values are device-resolved or
/// fractional drawn by `MouseHooks` and the button-mapping table drawn as its trailing block.
pub(super) unsafe fn build_mouse_page(
    context: &SettingsPageBuildContext,
    mouse_view: *mut AnyObject,
    mouse_doc_h: f64,
    ui: &mut SettingsUi,
) -> f64 {
    begin_settings_search_page();
    let input = PageInput::capture();
    let mut hooks = MouseHooks {
        target: context.target,
    };
    match build_page(
        ui,
        SettingsPageId::Mouse,
        mouse_view,
        context.content_w,
        context.layout,
        mouse_doc_h,
        context.page_frame.size.height,
        context.page_x,
        context.page_viewport_h,
        &input,
        context.target,
        &mut hooks,
    ) {
        Ok(built) => {
            let bottom = publish_built_page(ui, SettingsPageId::Mouse, built);
            // Render the current device's mappings initially: the table's block is part of the
            // layout now, so the rows can hand over their height.
            crate::settings::mapping::render_mapping_rows();
            bottom
        }
        Err(_) => empty_page(ui, SettingsPageId::Mouse, mouse_view, context, mouse_doc_h),
    }
}
/// Build the Gestures page: described rows only, so it needs no page-drawn canvas block.
pub(super) unsafe fn build_gestures_page(
    context: &SettingsPageBuildContext,
    gestures_view: *mut AnyObject,
    gestures_doc_h: f64,
    ui: &mut SettingsUi,
) -> f64 {
    begin_settings_search_page();
    let input = PageInput::capture();
    let mut hooks = NoHooks;
    match build_page(
        ui,
        SettingsPageId::Gestures,
        gestures_view,
        context.content_w,
        context.layout,
        gestures_doc_h,
        context.page_frame.size.height,
        context.page_x,
        context.page_viewport_h,
        &input,
        context.target,
        &mut hooks,
    ) {
        Ok(built) => {
            // The list this build drew is now the one on screen; only this makes a later reload
            // that changes it count as a change again.
            super::dispatch::note_gesture_rows_drawn(&input.config);
            publish_built_page(ui, SettingsPageId::Gestures, built)
        }
        Err(_) => empty_page(
            ui,
            SettingsPageId::Gestures,
            gestures_view,
            context,
            gestures_doc_h,
        ),
    }
}

/// Build the Clipboard page: the described static rows, then the excluded-applications card the
/// page draws itself (its list is not a row set the description can express).
pub(super) unsafe fn build_clipboard_page(
    context: &SettingsPageBuildContext,
    clipboard_view: *mut AnyObject,
    clipboard_doc_h: f64,
    ui: &mut SettingsUi,
) -> f64 {
    begin_settings_search_page();
    let input = PageInput::capture();
    let mut hooks = ClipboardHooks {
        shortcut_title: crate::mouse::shortcut::display_shortcut(&input.config.clipboard.shortcut),
    };
    match build_page(
        ui,
        SettingsPageId::Clipboard,
        clipboard_view,
        context.content_w,
        context.layout,
        clipboard_doc_h,
        context.page_frame.size.height,
        context.page_x,
        context.page_viewport_h,
        &input,
        context.target,
        &mut hooks,
    ) {
        Ok(built) => publish_built_page(ui, SettingsPageId::Clipboard, built),
        Err(_) => {
            // A first build has no previous state: the page stays empty and nothing is
            // published, which is better than a page that looks alive but is not wired.
            empty_page(
                ui,
                SettingsPageId::Clipboard,
                clipboard_view,
                context,
                clipboard_doc_h,
            )
        }
    }
}
/// Build the Key Bindings page: one card holding the master switch, the editable binding list and
/// the add row. Returns the add card's bottom.
///
/// The rows themselves are rendered by `settings::bindings` from the config; this builder creates
/// only the furniture around them and hands the canvas one block whose height the renderer owns (it
/// re-flows the page as rows come and go).
pub(super) unsafe fn build_key_bindings_page(
    context: &SettingsPageBuildContext,
    key_bindings_view: *mut AnyObject,
    key_bindings_doc_h: f64,
    ui: &mut SettingsUi,
) -> f64 {
    begin_settings_search_page();
    let input = PageInput::capture();
    match build_page(
        ui,
        SettingsPageId::KeyBindings,
        key_bindings_view,
        context.content_w,
        context.layout,
        key_bindings_doc_h,
        context.page_frame.size.height,
        context.page_x,
        context.page_viewport_h,
        &input,
        context.target,
        &mut page_view::KeyBindingsHooks::default(),
    ) {
        Ok(built) => publish_built_page(ui, SettingsPageId::KeyBindings, built),
        Err(error) => {
            log_info!("[settings] the Key Bindings page description was refused: {error:?}");
            empty_page(
                ui,
                SettingsPageId::KeyBindings,
                key_bindings_view,
                context,
                key_bindings_doc_h,
            )
        }
    }
}

/// The card the binding rows are rendered into, plus its empty-state hint.
///
/// It is a flipped settings card (surface, border and radius from the shared palette) so it reads
/// like the other cards; the renderer grows it from the top edge and hands the height to the page
/// canvas.
pub(super) unsafe fn build_bindings_block(
    parent: *mut AnyObject,
    content_w: f64,
    origin_y: f64,
) -> (*mut AnyObject, *mut AnyObject) {
    // A transparent, flipped host for the rows. The canvas card that wraps this block draws the
    // surface, the border and the section header, so the block must not draw a second one.
    let doc: *mut AnyObject = msg_send![class!(NSView), alloc];
    let doc: *mut AnyObject = msg_send![doc, initWithFrame: NSRect::new(NSPoint::new(0.0, origin_y), NSSize::new(content_w, BINDINGS_BLOCK_H))];
    let _: () = msg_send![doc, setFlipped: true];
    let _: () = msg_send![parent, addSubview: doc];
    release_obj(doc);

    let empty: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let empty: *mut AnyObject = msg_send![empty, initWithFrame: NSRect::new(
        NSPoint::new(
            bindings::BINDING_PAD_X,
            bindings::BINDING_PAD_Y + (bindings::BINDING_ROW_H - 18.0) / 2.0
        ),
        NSSize::new(
            (content_w - bindings::BINDING_PAD_X * 2.0).max(1.0),
            18.0
        )
    )];
    set_field(empty, t("settings.binding_empty_group"));
    let _: () = msg_send![empty, setBezeled: false];
    let _: () = msg_send![empty, setDrawsBackground: false];
    let _: () = msg_send![empty, setEditable: false];
    let font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CONTROL];
    let _: () = msg_send![empty, setFont: font];
    apply_settings_text_role(empty, SettingsTextRole::Disabled);
    let _: () = msg_send![doc, addSubview: empty];
    release_obj(empty);
    (doc, empty)
}

/// The provisional height of one group's binding block: the renderer replaces it with the real
/// height as soon as the rows are rendered.
const BINDINGS_BLOCK_H: f64 = 200.0;

/// Build the About page: the described rows, with the page's own header block above them and its
/// update band plus inline host inside the last card.
pub(super) unsafe fn build_about_page(
    context: &SettingsPageBuildContext,
    about_view: *mut AnyObject,
    about_doc_h: f64,
    window: *mut AnyObject,
    ui: &mut SettingsUi,
) -> f64 {
    begin_settings_search_page();
    let input = PageInput::capture();
    let mut hooks = AboutHooks { window };
    match build_page(
        ui,
        SettingsPageId::About,
        about_view,
        context.content_w,
        context.layout,
        about_doc_h,
        context.page_frame.size.height,
        context.page_x,
        context.page_viewport_h,
        &input,
        context.target,
        &mut hooks,
    ) {
        Ok(built) => publish_built_page(ui, SettingsPageId::About, built),
        Err(_) => {
            // A first build has no previous state: the page stays empty and nothing is
            // published, which is better than a page that looks alive but is not wired.
            // The inline update flow reads four pointers into this page (the card, the host, the
            // divider and the check button); the page's views are dropped with the refused build,
            // so they must be cleared here or the flow's own null guards would let it dereference
            // views that no longer exist.
            drop_about_update_flow_pointers(ui);
            empty_page(ui, SettingsPageId::About, about_view, context, about_doc_h)
        }
    }
}
/// Forget the four pointers the inline update flow reads into the About page, and its two row
/// indices.
///
/// Defensive, not a reproduced defect: a normal rebuild builds a fresh `SettingsUi` (with these
/// already null), and `empty_page` leaves the refused page's document in place rather than freeing
/// it. Clearing them here keeps the failure path from leaving the flow pointing at a page whose
/// build was just abandoned, which is the state a future partial rebuild would make unsafe.
pub(super) fn drop_about_update_flow_pointers(ui: &mut super::SettingsUi) {
    ui.update_card = std::ptr::null_mut();
    ui.update_host = std::ptr::null_mut();
    ui.update_divider = std::ptr::null_mut();
    ui.update_check_button = std::ptr::null_mut();
    ui.update_band_row = 0;
    ui.update_host_row = 0;
    ui.update_card_expanded = false;
}

/// Finish page registration, restore-default controls, and document validation.
pub(super) unsafe fn finalize_settings_pages(
    content: *mut AnyObject,
    window: *mut AnyObject,
    _page_frame: NSRect,
    content_w: f64,
    target: *mut AnyObject,
    pages: SettingsPageFinalization,
    ui: &mut SettingsUi,
) {
    let SettingsPageFinalization {
        roots: page_roots,
        documents: page_documents,
        bottoms: page_bottoms,
    } = pages;
    let _: () = msg_send![content, addSubview: ui.permission_warning_view];
    release_obj(ui.permission_warning_view);

    let _: () = msg_send![window, layoutIfNeeded];
    for (index, document) in page_documents.iter().enumerate() {
        ui.page_restores[index] = RestoreDefaultsControl::build_for_page(
            *document,
            target,
            6.0,
            page_bottoms[index] - 16.0,
            content_w,
        );
    }

    for (index, (root, document)) in page_roots.iter().zip(page_documents.iter()).enumerate() {
        // Settle the page *before* validating it: the restore control is part of the page content
        // (it hangs below the last card) and therefore decides the document height. Every page is
        // owned by its layout owner now (see `settings::page_canvas`).
        ui.page_canvases[index].attach_restore(
            ui.page_restores[index].container,
            ui.page_restores[index].surface,
        );
        SettingsPage {
            scroll: *root,
            document: *document,
        }
        .validate(settings_page_name(index));
    }
}

/// Store a migrated page: its layout owner and its rendered controls, and record its search
/// index from the description's own values (the pure description never writes to the index).
pub(super) unsafe fn publish_built_page(
    ui: &mut SettingsUi,
    page: SettingsPageId,
    built: BuiltPage,
) -> f64 {
    let index = settings_page_index(page);
    record_page_search_entries(page, &built.view, &built.canvas);
    // The page is good: give its controls to the pointer fields the dispatcher, the refill's own
    // consumers and the freeze lists read, before the page becomes the one on screen.
    bind_page_controls(ui, &built.rendered);
    // Every handle this page just bound must resolve back through the published table. A source
    // reading of the hook cannot show this: what matters is that the pointer the app actually uses
    // is the one the table names. The CALL carries the same cfg as the definition -- release builds
    // compile the function out, so an unguarded call would not resolve there.
    #[cfg(debug_assertions)]
    debug_assert_page_slots_are_reachable(ui);
    ui.page_canvases[index] = built.canvas;
    // The banner's three pointers are bound from the page's own record before it is stored.
    bind_page_banner(ui, &built.rendered);
    ui.migrated_pages[index] = Some(built.rendered);
    built.content_bottom
}

/// Assert that every control a page drew ITSELF resolves through the published table.
///
/// A described row's handle is set from the control the table already identified, so a round trip
/// through it proves little. The hook-drawn controls are the interesting ones: the page writes the
/// pointer it built into `SettingsUi` and registers the same pointer separately, and only running
/// the window shows whether those two agree. A mismatch is a control that looks alive and does
/// nothing -- the failure the description layer exists to prevent.
///
/// Debug only, and it runs on every page publish: whichever page happens to be built last, any
/// handle left over from an earlier build is still checked, so the assertion cannot be skipped by
/// publishing a page that binds nothing.
#[cfg(debug_assertions)]
unsafe fn debug_assert_page_slots_are_reachable(ui: &SettingsUi) {
    let hooks: [(&str, *mut AnyObject, ControlField); 8] = [
        ("scroll_mode", ui.scroll_mode, ControlField::ScrollMode),
        ("line_count", ui.line_count, ControlField::LineCount),
        (
            "pointer_accel_slider",
            ui.pointer_accel_slider,
            ControlField::PointerAcceleration,
        ),
        (
            "smooth_scrolling_preset",
            ui.smooth_scrolling_preset,
            ControlField::SmoothScrollingPreset,
        ),
        (
            "smooth_scrolling_response",
            ui.smooth_scrolling_response,
            ControlField::SmoothScrollingResponse,
        ),
        (
            "smooth_scrolling_speed",
            ui.smooth_scrolling_speed,
            ControlField::SmoothScrollingSpeed,
        ),
        (
            "smooth_scrolling_acceleration",
            ui.smooth_scrolling_acceleration,
            ControlField::SmoothScrollingAcceleration,
        ),
        (
            "smooth_scrolling_inertia",
            ui.smooth_scrolling_inertia,
            ControlField::SmoothScrollingInertia,
        ),
    ];
    for (name, pointer, field) in hooks {
        if pointer.is_null() {
            // The page that owns this control is not built (yet), or a rebuild dropped it.
            continue;
        }
        let resolved = ui.control_table.lookup(pointer);
        debug_assert_eq!(
            resolved,
            Some(ControlId::Field(field)),
            "the {name} control is bound in SettingsUi but the table does not name it; clicking it would do nothing"
        );
    }
}

/// Build an empty layout owner for a page whose description could not be built.
pub(super) unsafe fn empty_page(
    ui: &mut SettingsUi,
    page: SettingsPageId,
    document: *mut AnyObject,
    context: &SettingsPageBuildContext,
    doc_h: f64,
) -> f64 {
    let mut canvas = PageCanvas::new(
        document,
        context.content_w,
        context.layout,
        &t(page.title_key()),
        doc_h,
        context.page_frame.size.height,
    );
    let bottom = canvas.finish();
    ui.page_canvases[settings_page_index(page)] = canvas;
    ui.migrated_pages[settings_page_index(page)] = None;
    bottom
}
/// Record a migrated page's search index from its description: every row's entry is a value the
/// description produced. The sweep that follows fills in the rest, exactly as it does for the
/// pages still built inline.
unsafe fn record_page_search_entries(page: SettingsPageId, view: &PageView, canvas: &PageCanvas) {
    sidebar::begin_settings_search_section();
    let page_index = settings_page_index(page);
    for (row, spec) in view.rows.iter().enumerate() {
        // A row the page draws itself also takes a canvas row, so the described index is not the
        // canvas row once one has been opened before it. Without this correction every entry after
        // such a row is attached to its neighbour and a search hit scrolls to the wrong control.
        let canvas_row = page_view::canvas_row_of_described_row(view, row);
        let entry = sidebar::SearchEntry::row(
            page_index,
            canvas_row,
            &spec.search.label,
            spec.search.desc.as_deref(),
            &spec.search.section,
        )
        .with_options(&spec.search.options);
        sidebar::record_settings_search_entry(entry);
        // The same backfill the pages built inline get: whatever the row's views actually show
        // (a button's title, a dropdown's choices) is searchable too.
        let mut text = Vec::new();
        if let Some(views) = canvas.row_views(canvas_row) {
            for &view in views {
                collect_settings_search_text(view, &mut text);
            }
        }
        sidebar::extend_settings_search_row(page_index, canvas_row, &text);
    }
}

// MARK: - Search index

/// Complete every row's search text from the controls the page actually built. Manual annotations
/// retain their useful labels and descriptions, while this pass includes unannotated captions,
/// button labels and every dropdown choice, including choices that are not currently selected.
pub(super) unsafe fn sweep_unindexed_rows(canvas: &PageCanvas, page: SettingsPageId) {
    let page_index = settings_page_index(page);
    let recorded = sidebar::recorded_search_rows(page_index);
    let total = canvas.last_row() + 1;
    for row in 0..total {
        let mut text = Vec::new();
        if let Some(views) = canvas.row_views(row) {
            for &view in views {
                collect_settings_search_text(view, &mut text);
            }
        }
        if !recorded.contains(&row) {
            let section = canvas.row_section_title(row);
            let section = if section.is_empty() {
                t(settings_page_identity(page_index).title_key())
            } else {
                section
            };
            let label = text.first().map(String::as_str).unwrap_or(&section);
            // Use this row's own section, rather than the last card drawn on the page.
            sidebar::record_settings_search_entry_sectioned(sidebar::SearchEntry::row(
                page_index, row, label, None, &section,
            ));
        }
        sidebar::extend_settings_search_row(page_index, row, &text);
    }
}

/// Collect only the row's rendered subtree. Stop at native controls so AppKit's implementation
/// subviews cannot contribute duplicate or unrelated internal labels. Dropdown items are read
/// explicitly because their menu is not part of the view subtree.
unsafe fn collect_settings_search_text(view: *mut AnyObject, text: &mut Vec<String>) {
    if view.is_null() {
        return;
    }
    let is_popup: bool = msg_send![view, isKindOfClass: class!(NSPopUpButton)];
    let is_field: bool = msg_send![view, isKindOfClass: class!(NSTextField)];
    let is_button: bool = msg_send![view, isKindOfClass: class!(NSButton)];
    if is_popup {
        let count: isize = msg_send![view, numberOfItems];
        for index in 0..count {
            let title: *mut AnyObject = msg_send![view, itemTitleAtIndex: index];
            push_settings_search_text(text, crate::ffi::nsstring_to_rust(title));
        }
    } else if is_field {
        // A label's stringValue retains the full caption even when AppKit draws an ellipsis.
        let value: *mut AnyObject = msg_send![view, stringValue];
        push_settings_search_text(text, crate::ffi::nsstring_to_rust(value));
    } else if is_button {
        let title: *mut AnyObject = msg_send![view, title];
        push_settings_search_text(text, crate::ffi::nsstring_to_rust(title));
    }
    let tooltip: *mut AnyObject = msg_send![view, toolTip];
    if !tooltip.is_null() {
        push_settings_search_text(text, crate::ffi::nsstring_to_rust(tooltip));
    }
    if is_popup || is_field || is_button {
        return;
    }
    let subviews: *mut AnyObject = msg_send![view, subviews];
    let count: usize = msg_send![subviews, count];
    for index in 0..count {
        let child: *mut AnyObject = msg_send![subviews, objectAtIndex: index];
        collect_settings_search_text(child, text);
    }
}

fn push_settings_search_text(text: &mut Vec<String>, value: String) {
    let value = value.trim();
    if !value.is_empty() && !text.iter().any(|existing| existing == value) {
        text.push(value.to_owned());
    }
}

/// Clear the settings search index: the next build records every page's rows again.
///
/// Called once by the window build, before the first page: the index spans every page, so a page
/// that cleared it would delete the rows of every page built before it.
pub(super) fn begin_settings_search_recording() {
    sidebar::begin_settings_search_index();
}

/// Start one page's part of the index: no row is waiting for a card header any more, so the
/// page's first rows (drawn before any card) cannot adopt the previous page's last card.
pub(super) fn begin_settings_search_page() {
    sidebar::begin_settings_search_section();
}

/// Open a card: the rows drawn inside it are searched with `header`.
pub(super) fn set_settings_search_section(header: &str) {
    sidebar::apply_settings_search_section(header);
}

/// Attach the choices of the dropdown row recorded immediately before this call: they are part of
/// what the search matches, so typing an option finds the setting that offers it.
pub(super) fn record_settings_search_options(options: &[&str]) {
    sidebar::set_search_options_for_last_row(options);
}

/// Record one row the page builder is about to draw at canvas row `row`.
///
/// The row's card header is not known here (the page closes the card after drawing it), so the
/// entry is recorded without one; the page's following `set_settings_search_section` fills it in.
pub(super) fn record_settings_search_row(
    page: SettingsPageId,
    row: usize,
    label: &str,
    desc: Option<&str>,
) {
    sidebar::record_settings_search_entry(sidebar::SearchEntry::row(
        settings_page_index(page),
        row,
        label,
        desc,
        "",
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_extent_slider_range_matches_the_config_constants() {
        assert_eq!(
            max_extent_slider_min(),
            crate::config::KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_MIN as i64
        );
        assert_eq!(
            max_extent_slider_max(),
            crate::config::KEYSTROKE_DISPLAY_MAX_EXTENT_PERCENT_MAX as i64
        );
        assert!(max_extent_slider_min() < max_extent_slider_max());
    }

    #[test]
    fn max_extent_slider_value_snaps_to_accepted_steps() {
        // The config's own tests are the canonical expectations for these edges.
        assert_eq!(max_extent_slider_value(0), 10);
        assert_eq!(max_extent_slider_value(999), 100);
        assert_eq!(max_extent_slider_value(54), 50);
        assert_eq!(max_extent_slider_value(56), 60);
        // An exact step boundary is preserved rather than moved.
        for percent in [10u32, 50, 70, 100] {
            assert_eq!(max_extent_slider_value(percent), percent as i64);
        }
    }

    #[test]
    fn max_extent_readout_is_the_snapped_percent() {
        assert_eq!(max_extent_readout(50), "50%");
        assert_eq!(max_extent_readout(0), "10%");
        assert_eq!(max_extent_readout(999), "100%");
        assert_eq!(max_extent_readout(54), "50%");
        // The readout is user-facing text, never a raw translation/config key.
        let readout = max_extent_readout(54);
        assert!(!readout.contains("settings."));
        assert!(!readout.contains("keystroke_display"));
    }
}

#[cfg(test)]
mod update_flow_pointer_tests {
    use super::*;
    /// A refused About build drops the page's views, so the four pointers the inline update flow
    /// reads into that page must be cleared with it: a dangling `update_card` would pass the flow's
    /// own null guard and be dereferenced.
    #[test]
    fn a_refused_about_build_clears_the_update_flow_pointers() {
        let mut ui = unsafe { crate::settings::page_view::headless_settings_ui() };
        ui.update_card = 1usize as *mut AnyObject;
        ui.update_host = 2usize as *mut AnyObject;
        ui.update_divider = 3usize as *mut AnyObject;
        ui.update_check_button = 4usize as *mut AnyObject;
        ui.update_band_row = 7;
        ui.update_host_row = 8;
        ui.update_card_expanded = true;
        super::page_builder::drop_about_update_flow_pointers(&mut ui);
        assert!(ui.update_card.is_null());
        assert!(ui.update_host.is_null());
        assert!(ui.update_divider.is_null());
        assert!(ui.update_check_button.is_null());
        assert_eq!(ui.update_band_row, 0);
        assert_eq!(ui.update_host_row, 0);
        assert!(!ui.update_card_expanded);
    }
}
