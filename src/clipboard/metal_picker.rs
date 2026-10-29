//! Clipboard subsystem · metal_picker: the history picker re-implemented as a
//! Metal-drawn surface (mygo "native UI" style) — one CAMetalLayer view, all
//! chrome and rows rendered by `crate::metal`, keyboard/mouse handled by the
//! view's own callbacks. The data model, filtering, paste/pin/delete actions and
//! the glass panel shell are shared with the AppKit picker; only the view layer
//! differs.
//!
//! Enabled per launch with `--metal-clipboard` (dev flag): the toggle summons
//! this picker instead of the AppKit one. Everything else stays untouched.

use super::*;
use crate::metal::{render, text, view as metal_view};

/// Panel-local UI state. The list model itself lives in the shared
/// `ClipboardUiState` (`ui.filtered`, `ui.search_query`, `picker_selection`), so
/// `mapped_index`/`paste_at` see the same display list the Metal picker drew.
struct MetalPicker {
    window: *mut AnyObject,
    view: *mut AnyObject,
    scroll: f64,
    hover: usize,
    pitches: Vec<f64>,
    /// The history revision the current `pitches` were computed from; the render
    /// path rebuilds the model when the history moved on or `ui.filtered` was
    /// left non-empty by the AppKit picker's last showing.
    revision: u64,
}

static PICKER: MainThreadSlot<Option<MetalPicker>> = unsafe { MainThreadSlot::new(None) };
static VISIBLE: AtomicBool = AtomicBool::new(false);
static HANDLERS_INSTALLED: AtomicBool = AtomicBool::new(false);

fn with_picker<R>(default: R, f: impl FnOnce(&mut MetalPicker) -> R) -> R {
    match PICKER.lock().unwrap().as_mut() {
        Some(p) => f(p),
        None => default,
    }
}

/// Whether `--metal-clipboard` routes the history shortcut to this surface.
pub(super) fn metal_mode() -> bool {
    crate::dev_flags::enabled("metal-clipboard")
}

pub(crate) fn metal_picker_visible() -> bool {
    VISIBLE.load(Ordering::SeqCst)
}

/// Toggle for `on_clipboard_toggle`: hide when open, show otherwise.
pub(super) unsafe fn toggle() {
    if metal_picker_visible() {
        hide();
    } else {
        set_picker_selection(0);
        show();
    }
}

/// Present the picker. Mirrors `show_picker`'s model prep (expire, clean search,
/// placement), then hands drawing to the Metal view.
pub(super) unsafe fn show() {
    super::paste_handback::capture_external_target();
    {
        let mut hist = CLIP_HISTORY.lock().unwrap();
        let removed = expire_entries(&mut hist, now_secs(), ttl_secs());
        if removed > 0 {
            log_debug!("[clip] metal picker: expired {} entries", removed);
        }
    }
    ensure_window();
    with_clipboard_ui(|ui| ui.search_query.clear());
    rebuild_model();
    // Take the pointers and reset the transient state under the lock, then DROP
    // it: `setFrame:` re-enters through `frame_changed` -> render -> with_picker,
    // and a still-held RefCell borrow turns that into a contained panic.
    let (window, view) = {
        let mut slot = PICKER.lock().unwrap();
        let Some(p) = slot.as_mut() else {
            return;
        };
        p.scroll = 0.0;
        p.hover = NO_SELECTION;
        (p.window, p.view)
    };

    // The same size derivation as show_picker: visible rows fit the height cap.
    let (cursor, screen_frame) = configured_picker_area();
    let pad = crate::glass::panel_elevation_padding();
    let max_h = PICKER_MAX_HEIGHT.min(
        screen_frame.size.height - 120.0 - pad.top - pad.bottom - PICKER_EDGE_MARGIN * 2.0,
    );
    let filtered_len = with_clipboard_ui(|ui| ui.filtered.len());
    let pitches = with_picker(Vec::new(), |p| p.pitches.clone());
    let list_budget = max_h - header_strip_h() - FOOTER_H - PAD_Y;
    let list_h = if filtered_len == 0 {
        40.0
    } else {
        list_fill_for_budget(&pitches, list_budget).1.max(ROW_H)
    };
    let h = (header_strip_h() + list_h + FOOTER_H + PAD_Y)
        .max(picker_min_height())
        .min(max_h);
    let frame = configured_picker_frame(
        picker_placement_centers(),
        cursor,
        screen_frame,
        NSSize::new(
            picker_layout(screen_frame.size.width, false).panel_w,
            h,
        ),
    );
    crate::glass::set_panel_frame(window, frame, true);
    let _: () = msg_send![view, setFrame: NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(frame.size.width, frame.size.height)
    )];
    let _: () = msg_send![window, orderFrontRegardless];
    let _: () = msg_send![window, makeKeyWindow];
    let _: bool = msg_send![window, makeFirstResponder: view];
    VISIBLE.store(true, Ordering::SeqCst);
    metal_view::redraw();
}

/// Order the panel out. Called from `hide_picker` so paste paths dismiss this
/// surface too.
pub(crate) fn hide() {
    if !VISIBLE.swap(false, Ordering::SeqCst) {
        return;
    }
    with_picker((), |p| unsafe {
        let _: () = msg_send![p.window, orderOut: std::ptr::null::<AnyObject>()];
    });
}

/// The filtered list + pitches must be recomputed after every model or query
/// change; selection is clamped like `rebuild_rows` does.
fn rebuild_model() {
    let hist = CLIP_HISTORY.lock().unwrap();
    let revision = history_revision();
    let query = with_clipboard_ui(|ui| ui.search_query.clone());
    let filter = *CLIP_FILTER.lock().unwrap();
    let filtered = filtered_indices(&hist, &query, filter);
    let pitches = compute_pitches_for(filtered.iter().map(|&i| &hist[i]));
    with_clipboard_ui(|ui| ui.filtered = filtered);
    let len = with_clipboard_ui(|ui| ui.filtered.len());
    let sel = picker_selection();
    set_picker_selection(clamp_selection(sel, len));
    with_picker((), |p| {
        p.pitches = pitches;
        p.revision = revision;
    });
}

/// Keep the model honest at the top of every frame: a copy arriving while the
/// panel is open bumps the revision, and a `ui.filtered` left over from the
/// AppKit picker's last showing must never be indexed with empty pitches.
fn sync_model() {
    let stale = with_picker(true, |p| {
        p.revision != history_revision() || p.pitches.len() != with_clipboard_ui(|ui| ui.filtered.len())
    });
    if stale {
        rebuild_model();
    }
}

fn list_document_h() -> f64 {
    with_picker(0.0, |p| p.pitches.iter().sum::<f64>() + rows_top_offset())
}

/// Re-derive the list and redraw; the one entry point every mutation shares.
fn refresh() {
    rebuild_model();
    unsafe { metal_view::redraw() };
}

// ---------------------------------------------------------------------------
// Window
// ---------------------------------------------------------------------------

unsafe fn ensure_window() {
    if PICKER.lock().unwrap().is_some() {
        return;
    }
    if !HANDLERS_INSTALLED.swap(true, Ordering::SeqCst) {
        metal_view::install_handlers(metal_view::MetalHandlers {
            render: render_cb,
            key_down: Some(key_down_cb),
            mouse_moved: Some(mouse_moved_cb),
            mouse_down: Some(mouse_down_cb),
            mouse_up: None,
            mouse_exited: Some(mouse_exited_cb),
            scroll_wheel: Some(scroll_cb),
        });
        if metal_view::shared_renderer().is_none() {
            log_info!("[clip] metal picker: no renderer (Metal unavailable)");
        }
    }

    let w = PICKER_W;
    let h = PICKER_MAX_HEIGHT;
    let frame = NSRect::new(NSPoint::new(200.0, 200.0), NSSize::new(w, h));
    let style: u64 = 1 << 7; // NSWindowStyleMaskNonactivatingPanel
    let window: *mut AnyObject = msg_send![class!(NSPanel), alloc];
    let window: *mut AnyObject = msg_send![
        window,
        initWithContentRect: frame,
        styleMask: style,
        backing: 2u64,
        defer: false
    ];
    apply_panel_appearance(window);
    let _: () = msg_send![window, setAcceptsMouseMovedEvents: true];
    let _: () = msg_send![window, setLevel: 3u64];
    let _: () = msg_send![window, setOpaque: false];
    let _: () = msg_send![window, setReleasedWhenClosed: false];
    let clear: *mut AnyObject = msg_send![class!(NSColor), clearColor];
    let _: () = msg_send![window, setBackgroundColor: clear];
    let _: () = msg_send![window, setHasShadow: false];

    let backdrop = crate::glass::install_backdrop(
        window,
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h)),
        PICKER_PANEL_RADIUS,
        picker_backdrop_options(),
    );
    let content_parent = backdrop.content_parent;
    let view = metal_view::create_view(
        content_parent,
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h)),
    );
    // Hide on resign-key, the same dismissal the AppKit picker gets from its observer.
    install_resign_observer(window);
    *PICKER.lock().unwrap() = Some(MetalPicker {
        window,
        view,
        scroll: 0.0,
        hover: NO_SELECTION,
        pitches: Vec::new(),
        revision: u64::MAX,
    });
}

unsafe fn install_resign_observer(window: *mut AnyObject) {
    static TARGET: OnceLock<CallbackTarget> = OnceLock::new();
    let target = TARGET
        .get_or_init(|| {
            let cls = crate::ffi::build_class(
                c"OhMyTabMetalClipObserver",
                class!(NSObject),
                |builder| {
                    builder.add_method::<AnyObject, _>(
                        sel!(metalClipResigned:),
                        resigned_cb as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                    );
                },
            );
            let obj: *mut AnyObject = msg_send![cls.0 as *mut AnyObject, new];
            CallbackTarget::new(obj)
        })
        .0;
    let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
    let name = make_nsstring("NSWindowDidResignKeyNotification");
    let _: () = msg_send![
        center,
        addObserver: target,
        selector: sel!(metalClipResigned:),
        name: name,
        object: window
    ];
    CFRelease(name as *const c_void);
}

extern "C" fn resigned_cb(_self: *mut AnyObject, _cmd: Sel, _note: *mut AnyObject) {
    callback_guard::void("metal_clip_resigned", || {
        // Pinned panels stay; this surface has no pin affordance yet.
        hide();
    });
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

/// Rows' style tag for the text cache: palette + material + font sizes are the
/// inputs `make_content_attributed` baked into the attributed string.
fn rows_style_tag(palette: &crate::theme::UiPalette) -> u64 {
    fnv1a64(
        format!(
            "rows|{}|{}|{}|{}",
            palette.primary_text,
            palette.secondary_text,
            palette.link_text,
            material_cache_key()
        )
        .as_bytes(),
    )
}

fn label_style_tag(tag: &str, palette: &crate::theme::UiPalette, color: [f32; 4]) -> u64 {
    fnv1a64(
        format!(
            "{}|{}|{}|{}|{:?}",
            tag, palette.primary_text, palette.secondary_text, material_cache_key(), color
        )
        .as_bytes(),
    )
}

unsafe fn draw_text(
    scene: &mut render::Scene,
    renderer: &render::Renderer,
    attr: *mut AnyObject,
    style_tag: u64,
    x: f64,
    y_top: f64,
    max_w: f64,
    scale: f64,
) {
    let Some(bmp) = text::rasterize_attributed(renderer, attr, style_tag, scale, max_w) else {
        return;
    };
    scene.image(
        bmp.texture,
        [x - text::PAD_PT, y_top - text::PAD_PT, bmp.w, bmp.h],
        1.0,
        None,
    );
}

/// Convenience: draw a one-off label string; the attr is built and released here.
unsafe fn draw_label(
    scene: &mut render::Scene,
    renderer: &render::Renderer,
    s: &str,
    font: *mut AnyObject,
    color: *mut AnyObject,
    style_tag: u64,
    x: f64,
    y_top: f64,
    max_w: f64,
    scale: f64,
) {
    let attr = text::make_text_attr(s, font, color);
    if attr.is_null() {
        return;
    }
    draw_text(scene, renderer, attr, style_tag, x, y_top, max_w, scale);
    release_obj(attr);
}

/// `handlers.render`: rebuild the whole scene every frame — at these sizes
/// (≤ ~60 visible ops) re-emitting costs nothing, and text bitmaps are cached.
unsafe fn render_cb(view: *mut AnyObject, bounds: NSRect) {
    let Some(renderer) = metal_view::shared_renderer() else {
        return;
    };
    let renderer = &*renderer;
    sync_model();
    let scale = {
        let window: *mut AnyObject = msg_send![view, window];
        if window.is_null() {
            1.0
        } else {
            msg_send![window, backingScaleFactor]
        }
    };
    let palette = clipboard_palette();
    let w = bounds.size.width;
    let h = bounds.size.height;
    let (scroll, hover) = with_picker((0.0, NO_SELECTION), |p| (p.scroll, p.hover));
    let mut scene = render::Scene::new(scale, w, h);

    draw_header(&mut scene, renderer, &palette, w, scale);

    let list_rect = [
        0.0,
        header_strip_h(),
        w,
        (h - header_strip_h() - FOOTER_H).max(0.0),
    ];
    scene.with_clip(list_rect, |s| unsafe {
        draw_rows(s, renderer, &palette, w, scroll, hover, scale);
    });
    draw_scroll_indicator(&mut scene, &palette, w, list_rect, scroll);
    draw_footer(&mut scene, renderer, &palette, w, h, scale);
    let layer: *mut AnyObject = msg_send![view, layer];
    renderer.render(layer, &scene);
}

unsafe fn draw_header(
    scene: &mut render::Scene,
    renderer: &render::Renderer,
    palette: &crate::theme::UiPalette,
    w: f64,
    scale: f64,
) {
    let (search, _pause, _options, _pin, close) = picker_header_frames_for(w);
    // The search capsule: unfocused fill + the resting glass line (focus rings
    // come when editing lands; the field is always logically focused here).
    let focused = true;
    scene.border(
        [
            search.origin.x,
            search.origin.y,
            search.size.width,
            search.size.height,
        ],
        SEARCH_R,
        render::f4(picker_field_fill(focused)),
        render::f4(picker_field_ring(focused)),
        1.0,
    );
    let query = with_clipboard_ui(|ui| ui.search_query.clone());
    let font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: SEARCH_FONT_SIZE];
    let icon_color = crate::glass::panel_ink(
        palette.secondary_text,
        crate::glass::PanelInk::Secondary,
    );
    let line_y = search.origin.y + (search.size.height - SEARCH_FONT_SIZE * 1.4) / 2.0;
    if query.is_empty() {
        // "⌕" + placeholder, in one attributed run.
        let hint = t("clipboard.search_placeholder");
        draw_label(
            scene,
            renderer,
            &format!("⌕  {hint}"),
            font,
            icon_color,
            label_style_tag("search-hint", palette, text::nscolor_f4(icon_color)),
            search.origin.x + SEARCH_PAD_IN,
            line_y,
            search.size.width - SEARCH_PAD_IN * 2.0 - SEARCH_CLEAR_W,
            scale,
        );
        // ⌘F keycap on the right.
        let chip_w = 25.0;
        let chip_h = 18.0;
        let chip = [
            search.origin.x + search.size.width - SEARCH_PAD_IN - chip_w,
            search.origin.y + (search.size.height - chip_h) / 2.0,
            chip_w,
            chip_h,
        ];
        scene.fill(chip, 5.0, render::f4(palette.field_bg));
        let cap_font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 10.0];
        let cap_attr = text::make_text_attr("⌘F", cap_font, icon_color);
        if let Some(bmp) =
            text::rasterize_attributed(renderer, cap_attr, label_style_tag("cap-cmdf", palette, [0.0;4]), scale, f64::INFINITY)
        {
            scene.image(
                bmp.texture,
                [
                    chip[0] + (chip_w - bmp.w) / 2.0 - text::PAD_PT,
                    chip[1] + (chip_h - bmp.h) / 2.0 - text::PAD_PT,
                    bmp.w,
                    bmp.h,
                ],
                1.0,
                None,
            );
        }
        release_obj(cap_attr);
    } else {
        draw_label(
            scene,
            renderer,
            &format!("⌕  {query}"),
            font,
            crate::glass::panel_ink(palette.primary_text, crate::glass::PanelInk::Primary),
            label_style_tag("search-q", palette, [0.0; 4]),
            search.origin.x + SEARCH_PAD_IN,
            line_y,
            search.size.width - SEARCH_PAD_IN * 2.0 - SEARCH_CLEAR_W,
            scale,
        );
        // The clear × on the right (click target wired in the hit test).
        draw_label(
            scene,
            renderer,
            "×",
            font,
            icon_color,
            label_style_tag("search-x", palette, [0.0; 4]),
            search.origin.x + search.size.width - SEARCH_PAD_IN - SEARCH_CLEAR_W,
            line_y,
            SEARCH_CLEAR_W,
            scale,
        );
    }
    // The corner close glyph (a plain ×, centred in its 30pt hit target).
    draw_label(
        scene,
        renderer,
        "×",
        font,
        icon_color,
        label_style_tag("hdr-close", palette, [0.0; 4]),
        close.origin.x + (close.size.width - 12.0) / 2.0,
        close.origin.y + (close.size.height - 18.0) / 2.0,
        12.0,
        scale,
    );
}

unsafe fn draw_rows(
    scene: &mut render::Scene,
    renderer: &render::Renderer,
    palette: &crate::theme::UiPalette,
    w: f64,
    scroll: f64,
    hover: usize,
    scale: f64,
) {
    let hist = CLIP_HISTORY.lock().unwrap();
    let filtered = with_clipboard_ui(|ui| ui.filtered.clone());
    let sel = picker_selection();
    let row_w = w - PAD_X * 2.0;
    let list_top = header_strip_h();
    let list_h = with_picker(0.0, |p| {
        let f: NSRect = unsafe { msg_send![p.view, frame] };
        (f.size.height - list_top - FOOTER_H).max(0.0)
    });
    if filtered.is_empty() {
        // The empty-state hint, centred in the visible list area.
        let hint = if let Some(unavailable) = unavailable_copy() {
            unavailable
        } else if hist.is_empty() {
            t("clipboard.empty")
        } else {
            t("clipboard.no_match")
        };
        if !hint.is_empty() {
            let font: *mut AnyObject =
                msg_send![class!(NSFont), systemFontOfSize: SEARCH_FONT_SIZE];
            let attr = text::make_text_attr(
                &hint,
                font,
                crate::glass::panel_ink(palette.secondary_text, crate::glass::PanelInk::Secondary),
            );
            if let Some(bmp) = text::rasterize_attributed(
                renderer,
                attr,
                label_style_tag("empty", palette, [0.0; 4]),
                scale,
                w - 40.0,
            ) {
                scene.image(
                    bmp.texture,
                    [
                        (w - bmp.w) / 2.0 - text::PAD_PT,
                        list_top + (list_h - bmp.h) / 2.0 - text::PAD_PT,
                        bmp.w,
                        bmp.h,
                    ],
                    1.0,
                    None,
                );
            }
            release_obj(attr);
        }
        return;
    }
    let (pitches, offsets) = with_picker((Vec::new(), Vec::new()), |p| {
        (p.pitches.clone(), row_offsets(&p.pitches))
    });
    let style_tag = rows_style_tag(palette);
    let mut prev_group: Option<DayGroup> = None;
    for (i, &h_idx) in filtered.iter().enumerate().take(offsets.len()) {
        let y_doc = offsets[i] - scroll;
        if y_doc + pitches[i] < 0.0 || y_doc > list_h {
            prev_group = Some(day_group(hist[h_idx].copied_at));
            continue;
        }
        let entry = &hist[h_idx];
        let group = day_group(entry.copied_at);
        let has_hdr = prev_group.is_none() || prev_group != Some(group);
        prev_group = Some(group);
        let mut y = list_top + y_doc;
        if has_hdr {
            let font: *mut AnyObject = msg_send![
                class!(NSFont),
                systemFontOfSize: crate::theme::FONT_CAPTION,
                weight: crate::theme::FONT_WEIGHT_REGULAR
            ];
            draw_label(
                scene,
                renderer,
                &group_label(group),
                font,
                crate::glass::panel_ink(palette.muted_text, crate::glass::PanelInk::Muted),
                label_style_tag("group", palette, [0.0; 4]),
                PAD_X + ROW_PAD_L,
                y + GROUP_LABEL_PAD,
                row_w,
                scale,
            );
            y += GROUP_H;
        }
        let row_h = pitches[i] - if has_hdr { GROUP_H } else { 0.0 };
        // Row wash: selection or hover.
        let selected = i == sel;
        let hovered = i == hover;
        let wash = row_backdrop_wash(*palette, selected, hovered);
        scene.fill(
            [PAD_X, y, row_w, row_h],
            SEL_TILE_R,
            render::f4(wash),
        );
        if selected {
            scene.fill([PAD_X + 4.0, y + 6.0, 2.0, row_h - 12.0], 1.0, render::f4(
                picker_accent(*palette),
            ));
        }
        draw_row_content(scene, renderer, palette, entry, i, y, row_h, row_w, w, style_tag, scale);
    }
}

unsafe fn draw_row_content(
    scene: &mut render::Scene,
    renderer: &render::Renderer,
    palette: &crate::theme::UiPalette,
    entry: &ClipEntry,
    display_index: usize,
    y: f64,
    row_h: f64,
    row_w: f64,
    panel_w: f64,
    style_tag: u64,
    scale: f64,
) {
    let content_x = PAD_X + ROW_PAD_L;
    let content_w = model::row_content_width(row_w);
    let content_y = y + ROW_PAD_TOP;
    let content_h = row_h - ROW_PAD_TOP - ROW_PAD_BOT;
    if entry.image.is_some() {
        // The composited thumbnail canvas the AppKit row uses, rasterized as one
        // texture (well + image + ring baked in).
        let im = make_row_image(entry, content_w);
        if !im.is_null() {
            if let Some(tex) = text::rasterize_nsimage(renderer, im, scale) {
                scene.image(tex, [content_x, content_y, content_w, content_h], 1.0, None);
            }
            release_obj(im);
        } else {
            draw_entry_text(
                scene,
                renderer,
                entry,
                style_tag,
                content_x,
                content_y,
                content_w,
                content_h,
                scale,
            );
        }
    } else {
        draw_entry_text(
            scene,
            renderer,
            entry,
            style_tag,
            content_x,
            content_y,
            content_w,
            content_h,
            scale,
        );
    }
    draw_row_actions(scene, renderer, palette, entry, display_index, y, row_h, panel_w, scale);
}

unsafe fn draw_entry_text(
    scene: &mut render::Scene,
    renderer: &render::Renderer,
    entry: &ClipEntry,
    style_tag: u64,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    scale: f64,
) {
    let kind = classify_text(&entry.text);
    let attr = make_content_attributed(&entry.text, kind);
    if attr.is_null() {
        return;
    }
    if let Some(bmp) = text::rasterize_attributed(renderer, attr, style_tag, scale, w) {
        // Vertically centre the line's bitmap in the content band.
        let ty = y + (h - bmp.h) / 2.0 - text::PAD_PT;
        scene.image(bmp.texture, [x - text::PAD_PT, ty, bmp.w, bmp.h], 1.0, None);
    }
    release_obj(attr);
}

unsafe fn draw_row_actions(
    scene: &mut render::Scene,
    renderer: &render::Renderer,
    palette: &crate::theme::UiPalette,
    entry: &ClipEntry,
    _display_index: usize,
    y: f64,
    row_h: f64,
    panel_w: f64,
    scale: f64,
) {
    if !entry.pinned {
        return;
    }
    // The pin star only for now (hover actions arrive with the hit-test pass).
    let act_y = y + (row_h - ACTION_H) / 2.0;
    let x_pin = panel_w - PAD_X - ROW_PAD_R - ACTION_BTN;
    let font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: ACTION_GLYPH_SIZE];
    let attr = text::make_text_attr(
        "★",
        font,
        crate::glass::panel_ink(palette.secondary_text, crate::glass::PanelInk::Secondary),
    );
    if let Some(bmp) = text::rasterize_attributed(
        renderer,
        attr,
        label_style_tag("pin", palette, [0.0; 4]),
        scale,
        f64::INFINITY,
    ) {
        scene.image(
            bmp.texture,
            [
                x_pin + (ACTION_BTN - bmp.w) / 2.0 - text::PAD_PT,
                act_y + (ACTION_H - bmp.h) / 2.0 - text::PAD_PT,
                bmp.w,
                bmp.h,
            ],
            1.0,
            None,
        );
    }
    release_obj(attr);
}

fn draw_scroll_indicator(
    scene: &mut render::Scene,
    palette: &crate::theme::UiPalette,
    w: f64,
    list_rect: [f64; 4],
    scroll: f64,
) {
    let doc_h = list_document_h();
    if doc_h <= list_rect[3] || list_rect[3] <= 0.0 {
        return;
    }
    let track_h = list_rect[3] - SCROLL_INDICATOR_EDGE * 2.0;
    let ind_h = (track_h * list_rect[3] / doc_h).max(SCROLL_INDICATOR_MIN_LEN);
    let max_scroll = doc_h - list_rect[3];
    let y = list_rect[1]
        + SCROLL_INDICATOR_EDGE
        + (track_h - ind_h) * (scroll / max_scroll.max(1.0)).clamp(0.0, 1.0);
    let color = if palette.dark {
        [1.0, 1.0, 1.0, 0.28]
    } else {
        [0.0, 0.0, 0.0, 0.22]
    };
    scene.fill(
        [
            w - SCROLL_INDICATOR_HIT_W - 1.0,
            y,
            SCROLL_INDICATOR_W,
            ind_h,
        ],
        SCROLL_INDICATOR_W / 2.0,
        color,
    );
}

unsafe fn draw_footer(
    scene: &mut render::Scene,
    renderer: &render::Renderer,
    palette: &crate::theme::UiPalette,
    w: f64,
    h: f64,
    scale: f64,
) {
    // Top hairline.
    scene.fill(
        [0.0, h - FOOTER_H, w, 1.0],
        0.0,
        render::f4(picker_hairline()),
    );
    let total = CLIP_HISTORY.lock().unwrap().len();
    let count = t_count("clipboard.footer_count", total);
    let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: FOOTER_TEXT_SIZE];
    let muted = crate::glass::panel_ink(palette.muted_text, crate::glass::PanelInk::Muted);
    draw_label(
        scene,
        renderer,
        &count,
        font,
        muted,
        label_style_tag("footer-count", palette, [0.0; 4]),
        FOOTER_PAD_X,
        h - FOOTER_H + (FOOTER_H - 14.0) / 2.0,
        FOOTER_COUNT_W,
        scale,
    );
    // Shortcut legends right-to-left: ↵ paste · ⌫ delete · → detail · ← pin · Tab filter.
    let legends: [(&str, String); 5] = [
        ("↵", t("clipboard.kbd_paste")),
        ("⌫", t("clipboard.kbd_delete")),
        ("→", t("clipboard.kbd_detail")),
        ("←", t("clipboard.kbd_pin")),
        ("Tab", t("clipboard.kbd_filter")),
    ];
    let secondary = crate::glass::panel_ink(
        palette.secondary_text,
        crate::glass::PanelInk::Secondary,
    );
    let mut x = w - FOOTER_PAD_X;
    for (key, label) in legends.iter() {
        // Measure from the rasterized bitmaps so chips hug their text.
        let key_attr = text::make_text_attr(key, font, secondary);
        let label_attr = text::make_text_attr(label, font, muted);
        let key_bmp = text::rasterize_attributed(
            renderer,
            key_attr,
            label_style_tag("kbd-key", palette, [0.0; 4]),
            scale,
            f64::INFINITY,
        );
        let label_bmp = text::rasterize_attributed(
            renderer,
            label_attr,
            label_style_tag("kbd-label", palette, [0.0; 4]),
            scale,
            f64::INFINITY,
        );
        let key_w = key_bmp.map(|b| b.w - text::PAD_PT * 2.0 + KBD_CELL_PADDING).unwrap_or(21.0).max(21.0);
        let label_w = label_bmp.map(|b| b.w - text::PAD_PT * 2.0).unwrap_or(30.0);
        let group_w = key_w + 5.0 + label_w;
        x -= group_w;
        let kbd_h = 19.0;
        let cap = [x, h - FOOTER_H + (FOOTER_H - kbd_h) / 2.0, key_w, kbd_h];
        scene.fill(cap, crate::theme::RADIUS_LEGEND_CHIP, chip_f4(palette));
        if let Some(bmp) = key_bmp {
            scene.image(
                bmp.texture,
                [
                    cap[0] + (key_w - bmp.w) / 2.0 - text::PAD_PT,
                    cap[1] + (kbd_h - bmp.h) / 2.0 - text::PAD_PT,
                    bmp.w,
                    bmp.h,
                ],
                1.0,
                None,
            );
        }
        if let Some(bmp) = label_bmp {
            scene.image(
                bmp.texture,
                [
                    cap[0] + key_w + 5.0 - text::PAD_PT,
                    h - FOOTER_H + (FOOTER_H - bmp.h) / 2.0 - text::PAD_PT,
                    bmp.w,
                    bmp.h,
                ],
                1.0,
                None,
            );
        }
        release_obj(key_attr);
        release_obj(label_attr);
        x -= FOOTER_GROUP_GAP;
    }
}

fn chip_f4(palette: &crate::theme::UiPalette) -> [f32; 4] {
    let base: u32 = if palette.dark { 0xFFFF_FFFF } else { 0x0000_0000 };
    render::f4((base & 0xFFFF_FF00) | crate::theme::LEGEND_CHIP_ALPHA)
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

/// Rows' hit test: display index under `y` (view coords), or None over chrome.
fn row_at(view_y: f64) -> Option<usize> {
    let list_top = header_strip_h();
    if view_y < list_top {
        return None;
    }
    let (pitches, scroll) = with_picker((Vec::new(), 0.0), |p| (p.pitches.clone(), p.scroll));
    let offsets = row_offsets(&pitches);
    let doc_y = view_y - list_top + scroll;
    let len = with_clipboard_ui(|ui| ui.filtered.len());
    for i in 0..len.min(pitches.len()) {
        if doc_y >= offsets[i] && doc_y < offsets[i] + pitches[i] {
            return Some(i);
        }
    }
    None
}

unsafe fn scroll_cb(_event: *mut AnyObject, _dx: f64, dy: f64) {
    let changed = with_picker(false, |p| {
        let doc_h = p.pitches.iter().sum::<f64>() + rows_top_offset();
        let f: NSRect = unsafe { msg_send![p.view, frame] };
        let view_h = (f.size.height - header_strip_h() - FOOTER_H).max(0.0);
        let max_scroll = (doc_h - view_h).max(0.0);
        let new = (p.scroll - dy).clamp(0.0, max_scroll);
        let changed = new != p.scroll;
        p.scroll = new;
        changed
    });
    if changed {
        metal_view::redraw();
    }
}

unsafe fn mouse_moved_cb(_event: *mut AnyObject, point: NSPoint) {
    let idx = row_at(point.y);
    let idx = idx.unwrap_or(NO_SELECTION);
    let changed = with_picker(false, |p| {
        let changed = p.hover != idx;
        p.hover = idx;
        changed
    });
    if changed {
        metal_view::redraw();
    }
}

unsafe fn mouse_exited_cb(_event: *mut AnyObject) {
    let changed = with_picker(false, |p| {
        let changed = p.hover != NO_SELECTION;
        p.hover = NO_SELECTION;
        changed
    });
    if changed {
        metal_view::redraw();
    }
}

unsafe fn mouse_down_cb(event: *mut AnyObject, point: NSPoint) {
    let view = with_picker(std::ptr::null_mut(), |p| p.view);
    if view.is_null() {
        return;
    }
    let frame: NSRect = msg_send![view, frame];
    // The search field's clear ×.
    let (search, _, _, _, close) = picker_header_frames_for(frame.size.width);
    if point.y >= search.origin.y
        && point.y <= search.origin.y + search.size.height
        && point.x >= search.origin.x + search.size.width - SEARCH_PAD_IN - SEARCH_CLEAR_W
        && with_clipboard_ui(|ui| !ui.search_query.is_empty())
    {
        with_clipboard_ui(|ui| ui.search_query.clear());
        refresh();
        return;
    }
    if point.x >= close.origin.x
        && point.x <= close.origin.x + close.size.width
        && point.y >= close.origin.y
        && point.y <= close.origin.y + close.size.height
    {
        hide();
        return;
    }
    let Some(idx) = row_at(point.y) else {
        return;
    };
    let mods: u64 = msg_send![event, modifierFlags];
    set_picker_selection(idx);
    paste_at_ex(idx, (mods & 0x0008_0000) != 0);
}

fn ensure_selection_visible() {
    with_picker((), |p| {
        let sel = picker_selection();
        if sel == NO_SELECTION || sel >= p.pitches.len() {
            return;
        }
        let f: NSRect = unsafe { msg_send![p.view, frame] };
        let view_h = (f.size.height - header_strip_h() - FOOTER_H).max(0.0);
        let offsets = row_offsets(&p.pitches);
        let top = offsets[sel];
        let bottom = top + p.pitches[sel];
        if bottom - p.scroll > view_h {
            p.scroll = bottom - view_h;
        }
        if top - p.scroll < 0.0 {
            p.scroll = top.max(0.0);
        }
    });
}

fn move_selection(delta: isize) {
    let len = with_clipboard_ui(|ui| ui.filtered.len());
    if len == 0 {
        return;
    }
    let sel = picker_selection();
    let next = if sel == NO_SELECTION {
        if delta > 0 {
            0
        } else {
            len - 1
        }
    } else {
        (sel as isize + delta).clamp(0, len as isize - 1) as usize
    };
    if next != sel {
        set_picker_selection(next);
        ensure_selection_visible();
        unsafe { metal_view::redraw() };
    }
}

unsafe fn key_down_cb(event: *mut AnyObject) {
    let keycode: u16 = msg_send![event, keyCode];
    match keycode {
        126 => move_selection(-1), // up
        125 => move_selection(1),  // down
        123 => {
            // Left: pin toggle (same code path as the AppKit handler).
            let idx = picker_selection();
            if let Some(h_idx) = mapped_index(idx) {
                let mut hist = CLIP_HISTORY.lock().unwrap();
                let (_now_pinned, new_h_idx) = toggle_pin_on(&mut hist, h_idx);
                drop(hist);
                save_history();
                refresh();
                if selection_after_pin(new_h_idx) {
                    refresh();
                }
            }
        }
        124 => {} // Right: detail panel — not implemented on this surface yet.
        36 | 76 => {
            let idx = picker_selection();
            if idx != NO_SELECTION {
                paste_at_ex(idx, false);
            }
        }
        53 => {
            // Esc: clear the query first, then dismiss.
            let has_query = with_clipboard_ui(|ui| !ui.search_query.is_empty());
            if has_query {
                with_clipboard_ui(|ui| ui.search_query.clear());
                refresh();
            } else {
                hide();
            }
        }
        48 => {
            // Tab: cycle filters.
            let next = {
                let active = CLIP_FILTER.lock().unwrap();
                next_clip_filter(*active)
            };
            *CLIP_FILTER.lock().unwrap() = next;
            set_picker_selection(0);
            refresh();
        }
        51 => {
            // Backspace: drop the last char of the query.
            let removed = with_clipboard_ui(|ui| ui.search_query.pop().is_some());
            if removed {
                set_picker_selection(0);
                refresh();
            }
        }
        117 => {} // Forward delete: ignore for now.
        _ => {
            // Printable input appends to the query.
            let chars: *mut AnyObject = msg_send![event, characters];
            let s = nsstring_to_rust(chars);
            if s.is_empty() || s.chars().any(|c| c.is_control()) {
                return;
            }
            let mods: u64 = msg_send![event, modifierFlags];
            if (mods & 0x0010_0000) != 0 {
                return; // let Command combos pass elsewhere
            }
            with_clipboard_ui(|ui| ui.search_query.push_str(&s));
            set_picker_selection(0);
            refresh();
        }
    }
}
