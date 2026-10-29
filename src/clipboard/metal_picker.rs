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
use crate::metal::{render, text, ui, view as metal_view};

/// Panel-local UI state. The list model itself lives in the shared
/// `ClipboardUiState` (`ui.filtered`, `ui.search_query`, `picker_selection`), so
/// `mapped_index`/`paste_at` see the same display list the Metal picker drew.
struct MetalPicker {
    window: *mut AnyObject,
    view: *mut AnyObject,
    scroll: f64,
    /// The frame's pointer input the immediate-mode build answers: the cursor's
    /// latest position (hover) and the pending mouse-down (click + whether
    /// Option was held, for the burn-after-paste variant). A click is consumed
    /// by the first widget whose rect contains it — drawing is the hit test,
    /// the mygo `ui` pattern.
    cursor: Option<NSPoint>,
    click: Option<(NSPoint, bool)>,
    /// The detail pane's open state: → splits the panel (list left, detail
    /// right) like the AppKit picker's expanded layout.
    detail: bool,
    pitches: Vec<f64>,
    /// The history revision the current `pitches` were computed from; the render
    /// path rebuilds the model when the history moved on or `ui.filtered` was
    /// left non-empty by the AppKit picker's last showing.
    revision: u64,
}

static PICKER: MainThreadSlot<Option<MetalPicker>> = unsafe { MainThreadSlot::new(None) };
static VISIBLE: AtomicBool = AtomicBool::new(false);
static HANDLERS_INSTALLED: AtomicBool = AtomicBool::new(false);

// Liquid-glass palette (clipboard-v2.html): warm glass over the blur backdrop
// — cream whites for fields/orbs, warm greys for washes, a barely-there warm
// panel tint. Dark mode keeps the theme palette instead.
const GLASS_RADIUS: f64 = 32.0;
const ROW_R_LG: f64 = 14.0;
const ORB_BG: [f32; 4] = [1.0, 1.0, 1.0, 0.58];
const ORB_BG_HOT: [f32; 4] = [1.0, 1.0, 1.0, 0.66];
const ORB_BG_ON: [f32; 4] = [1.0, 1.0, 1.0, 0.74];
const ORB_LINE: [f32; 4] = [1.0, 1.0, 1.0, 0.70];
const ORB_LINE_ON: [f32; 4] = [1.0, 1.0, 1.0, 0.90];
const FIELD_BG_LG: [f32; 4] = [1.0, 1.0, 1.0, 0.45];
const FIELD_BG_LG_IDLE: [f32; 4] = [1.0, 1.0, 1.0, 0.32];
const HAIRLINE_LG: [f32; 4] = [52.0 / 255.0, 42.0 / 255.0, 30.0 / 255.0, 0.09];
const ROW_HOVER_LG: [f32; 4] = [1.0, 1.0, 1.0, 0.28];
// The reference's selected row is a warm tan wash, not a grey one.
const ROW_SEL_LG: [f32; 4] = [156.0 / 255.0, 124.0 / 255.0, 84.0 / 255.0, 0.26];
const SCROLL_LG: [f32; 4] = [60.0 / 255.0, 50.0 / 255.0, 36.0 / 255.0, 0.20];
// High-translucency glass (the reference photo): the wallpaper dominates and
// the panel is only a thin frost — whites stay far lower than the v2 mock.
const WARM_TINT: [f32; 4] = [1.0, 1.0, 1.0, 0.10];
const ACCENT_GLOW: [f32; 4] = [47.0 / 255.0, 111.0 / 255.0, 237.0 / 255.0, 0.12];
const ACCENT_LG: [f32; 4] = [47.0 / 255.0, 111.0 / 255.0, 237.0 / 255.0, 1.0];
const ACCENT_DARK: [f32; 4] = [0.20, 0.35, 0.80, 1.0];

/// The detail pane's own metrics (the AppKit pane's, kept simple for v1).
const DETAIL_PAD: f64 = 14.0;
const DETAIL_META_H: f64 = 24.0;
const DETAIL_LINE_GAP: f64 = 3.0;
const DETAIL_EMPTY_LINE_H: f64 = 16.0;

/// What a clicked widget asks the frame to run after the build (the mygo
/// "input read at construction, applied to the next frame" shape: the action
/// executes once the frame encodes, never mid-draw).
enum Action {
    None,
    Paste { idx: usize, burn: bool },
    PinEntry(usize),
    DeleteEntry(usize),
    OpenDetail,
    TogglePause,
    TogglePanelPin,
    ClearSearch,
    OptionsMenu,
    Hide,
}

fn with_picker<R>(default: R, f: impl FnOnce(&mut MetalPicker) -> R) -> R {
    // A reentrant borrow is a *skipped* call, not a panic: `with_picker` is
    // re-entered whenever a msg_send inside a borrow loops back through AppKit
    // (setFrame -> frame_changed -> render), and unwrap()ing a BorrowMutError
    // inside an `extern "C"` frame aborts the whole app — the 22:07 crash.
    let Ok(mut guard) = PICKER.lock() else {
        return default;
    };
    match guard.as_mut() {
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
        p.cursor = None;
        p.click = None;
        p.detail = false;
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
    // The dev flag the AppKit picker opens its detail with; here it just splits
    // the panel (there is no gesture for a script to press → either).
    if crate::dev_flags::present("show-clipboard-detail") {
        set_detail_open(true);
    }
    // The same showing bookkeeping the AppKit picker runs at this point: the
    // generation invalidates stale dismissals, the pin's window props apply,
    // and the outside-click monitor starts guarding the panel.
    super::pin::begin_showing();
    super::apply_pin_window_properties();
    super::sync_outside_click_monitor();
    metal_view::set_display_link_paused(false);
    metal_view::warmup_frames(45);
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
    unsafe {
        metal_view::set_display_link_paused(true);
        super::sync_outside_click_monitor();
    }
}

/// The panel's window, for the shared outside-click monitor's "inside" tests.
pub(super) fn window() -> Option<*mut AnyObject> {
    let w = with_picker(std::ptr::null_mut(), |p| p.window);
    if w.is_null() {
        None
    } else {
        Some(w)
    }
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
            mouse_up: Some(mouse_up_cb),
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
    let window: *mut AnyObject = msg_send![panel_class().0 as *mut AnyObject, alloc];
    let window: *mut AnyObject = msg_send![
        window,
        initWithContentRect: frame,
        styleMask: style,
        backing: 2u64,
        defer: false
    ];
    apply_panel_appearance(window);
    // A panel only becomes key for a text-field view while this is YES (the
    // NSPanel default); the Metal surface draws its own field, so keyboard
    // focus needs the flag off — arrows/Enter/Esc reach keyDown: only then.
    let _: () = msg_send![window, setBecomesKeyOnlyIfNeeded: false];
    let _: () = msg_send![window, setAcceptsMouseMovedEvents: true];
    let _: () = msg_send![window, setLevel: 3u64];
    let _: () = msg_send![window, setOpaque: false];
    let _: () = msg_send![window, setReleasedWhenClosed: false];
    let clear: *mut AnyObject = msg_send![class!(NSColor), clearColor];
    let _: () = msg_send![window, setBackgroundColor: clear];
    let _: () = msg_send![window, setHasShadow: false];

    // The panel's pinned glass: clear style + a 30px/1.6 blur underlay — the
    // tuned look, owned by this panel's options instead of launch flags.
    let backdrop = crate::glass::install_backdrop(
        window,
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h)),
        GLASS_RADIUS,
        picker_backdrop_options()
            .with_glass_style(1)
            .with_blur(30.0, 1.6),
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
        cursor: None,
        click: None,
        detail: false,
        pitches: Vec::new(),
        revision: u64::MAX,
    });
}

/// The panel's class: a borderless NonactivatingPanel answers NO to
/// `canBecomeKey` (no title/resize bar), which silently voids `makeKeyWindow`
/// — arrows/Enter/Esc then never reach `keyDown:`. Both spellings return YES.
fn panel_class() -> crate::ffi::StaticClass {
    static CLS: OnceLock<crate::ffi::StaticClass> = OnceLock::new();
    crate::ffi::StaticClass::register_once(&CLS, c"OhMyTabMetalPanel", || {
        crate::ffi::build_class(c"OhMyTabMetalPanel", class!(NSPanel), |builder| unsafe {
            extern "C" fn yes(_self: *mut AnyObject, _cmd: Sel) -> objc2::runtime::Bool {
                objc2::runtime::Bool::YES
            }
            builder.add_method::<AnyObject, _>(
                sel!(canBecomeKeyWindow),
                yes as extern "C" fn(*mut AnyObject, Sel) -> objc2::runtime::Bool,
            );
            builder.add_method::<AnyObject, _>(
                sel!(canBecomeKey),
                yes as extern "C" fn(*mut AnyObject, Sel) -> objc2::runtime::Bool,
            );
        })
    })
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
        // A pinned panel stays open across focus loss, same as the AppKit one.
        if super::pin::is_pinned() {
            return;
        }
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
    let (scroll, detail, cursor, click) = with_picker((0.0, false, None, None), |p| {
        (p.scroll, p.detail, p.cursor, p.click)
    });
    let burn = click.map(|(_, b)| b).unwrap_or(false);
    let layout = picker_layout(w, detail);
    let list_w = if detail { layout.list_w } else { w };
    let mut scene = render::Scene::new(scale, w, h);
    // The liquid-glass panel: a uniform thin wash over the blur, the white
    // glass rim, and the specular highlight hugging the top edge. No top-glow
    // rect — a solid fill can't feather, so its hard edge read as a seam.
    if !palette.dark {
        scene.fill([0.0, 0.0, w, h], GLASS_RADIUS, WARM_TINT);
        scene.border(
            [0.5, 0.5, w - 1.0, h - 1.0],
            GLASS_RADIUS - 0.5,
            [0.0; 4],
            [1.0, 1.0, 1.0, 0.5],
            1.0,
        );
        scene.fill(
            [GLASS_RADIUS * 0.8, 1.0, w - GLASS_RADIUS * 1.6, 1.0],
            0.5,
            [1.0, 1.0, 1.0, 0.88],
        );
    }
    let mut action = Action::None;

    // The frame's build pass (mygo's `view(ctx)`): every widget answers its
    // own input where it draws, and what it wants done lands in `action`.
    {
        let mut ui = ui::Ui::new(&mut scene, renderer, scale, cursor, click.map(|(p, _)| p));
        draw_header(&mut ui, &palette, w, &mut action);

        let list_rect = [
            0.0,
            header_strip_h(),
            list_w,
            (h - header_strip_h() - FOOTER_H).max(0.0),
        ];
        ui.scene.with_clip(list_rect, |s| {
            let mut ui = ui::Ui::new(s, renderer, scale, cursor, click.map(|(p, _)| p));
            draw_rows(&mut ui, &palette, list_w, scroll, burn, &mut action);
        });
        draw_scroll_indicator(ui.scene, &palette, list_w, list_rect, scroll);
        if detail {
            draw_detail(&mut ui, &palette, &layout, h);
        }
        draw_footer(&mut ui, &palette, w, h);
    }
    renderer.render(metal_view::metal_layer(), &scene);
    with_picker((), |p| p.click = None);
    run_action(action);
}

/// The row backdrop under the liquid-glass palette: hover is a white wash,
/// selection a warm grey — dark mode keeps the theme's washes.
fn metal_wash(palette: crate::theme::UiPalette, selected: bool, hovered: bool) -> [f32; 4] {
    if palette.dark {
        return render::f4(row_backdrop_wash(palette, selected, hovered));
    }
    if selected {
        ROW_SEL_LG
    } else if hovered {
        ROW_HOVER_LG
    } else {
        [0.0; 4]
    }
}

/// The warm glass hairline (the mock's `rgba(52,42,30,0.09)`).
fn hairline_f4(palette: crate::theme::UiPalette) -> [f32; 4] {
    if palette.dark {
        render::f4(picker_hairline())
    } else {
        HAIRLINE_LG
    }
}

/// A header orb (clipboard-v2): a persistent translucent circle with a glass
/// rim — fill and rim strengthen on hover and again when latched. Returns
/// whether the orb was clicked this frame, like `icon_button`.
unsafe fn orb_button(
    ui: &mut ui::Ui,
    rect: NSRect,
    glyph: &str,
    font: *mut AnyObject,
    color: *mut AnyObject,
    tag: u64,
    latched: bool,
    dark: bool,
) -> bool {
    let hot = ui.hot(rect);
    let (fill, line) = if dark {
        let base = if latched { 0.34 } else if hot { 0.24 } else { 0.10 };
        ([1.0, 1.0, 1.0, base], [1.0, 1.0, 1.0, 0.28])
    } else if latched {
        (ORB_BG_ON, ORB_LINE_ON)
    } else if hot {
        (ORB_BG_HOT, ORB_LINE)
    } else {
        (ORB_BG, ORB_LINE)
    };
    ui.scene.border(
        [
            rect.origin.x,
            rect.origin.y,
            rect.size.width,
            rect.size.height,
        ],
        rect.size.height / 2.0,
        fill,
        line,
        1.0,
    );
    // Dimensional glass (the Control-Center look): a specular sheen over the
    // orb's top half and a faint inner shade along the bottom edge.
    let r = rect.size.height / 2.0 - 1.5;
    ui.scene.fill_radii(
        [
            rect.origin.x + 1.5,
            rect.origin.y + 1.5,
            rect.size.width - 3.0,
            rect.size.height * 0.52,
        ],
        [r, r, 0.0, 0.0],
        [1.0, 1.0, 1.0, if dark { 0.16 } else { 0.34 }],
    );
    ui.scene.fill_radii(
        [
            rect.origin.x + 1.5,
            rect.origin.y + rect.size.height * 0.52,
            rect.size.width - 3.0,
            rect.size.height * 0.46,
        ],
        [0.0, 0.0, r, r],
        [0.0, 0.0, 0.0, 0.10],
    );
    ui.centered_label(glyph, font, color, tag, rect);
    ui.clicked(rect)
}

/// Run what the frame's clicked widget asked for — after encoding, so a click
/// never mutates the model mid-draw.
fn run_action(action: Action) {
    match action {
        Action::None => {}
        Action::Paste { idx, burn } => {
            set_picker_selection(idx);
            paste_at_ex(idx, burn);
        }
        Action::PinEntry(idx) => {
            set_picker_selection(idx);
            pin_selected();
        }
        Action::DeleteEntry(idx) => delete_entry_at(idx),
        Action::OpenDetail => unsafe { set_detail_open(true) },
        Action::TogglePause => {
            let _ = set_collection_paused(!collection_paused());
            refresh();
        }
        Action::TogglePanelPin => {
            let h = with_picker(0.0, |p| {
                let f: NSRect = unsafe { msg_send![p.view, frame] };
                f.size.height
            });
            if super::pin::is_pinned() {
                super::pin::unpin();
            } else {
                super::pin::pin(h);
            }
            refresh();
        }
        Action::ClearSearch => {
            with_clipboard_ui(|ui| ui.search_query.clear());
            refresh();
        }
        Action::OptionsMenu => unsafe { show_filter_menu() },
        Action::Hide => hide(),
    }
}

/// The options orb's menu: the AppKit popover's filter rows as a native menu
/// popped under the orb — the observer's `popoverFilterSelected:` applies the
/// chosen filter the same way the popover does.
unsafe fn show_filter_menu() {
    let (view, options) = with_picker(
        (std::ptr::null_mut(), NSRect::default()),
        |p| {
            let f: NSRect = msg_send![p.view, frame];
            let (_, _, options, _, _) = picker_header_frames_for(f.size.width);
            (p.view, options)
        },
    );
    if view.is_null() {
        return;
    }
    let menu: *mut AnyObject = msg_send![class!(NSMenu), alloc];
    let blank = crate::ffi::make_nsstring("");
    let menu: *mut AnyObject = msg_send![menu, initWithTitle: blank];
    let labels = super::localized_filter_labels();
    let rows = super::model::CLIP_FILTER_ROWS;
    let current = *CLIP_FILTER.lock().unwrap();
    let target = super::notifications::observer();
    for (i, label) in labels.iter().enumerate() {
        let title = crate::ffi::make_nsstring(label);
        let item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
        let item: *mut AnyObject = msg_send![
            item,
            initWithTitle: title,
            action: sel!(popoverFilterSelected:),
            keyEquivalent: blank
        ];
        release_obj(title);
        let _: () = msg_send![item, setTarget: target];
        let _: () = msg_send![item, setTag: i as isize];
        let state: isize = if rows[i] == current { 1 } else { 0 };
        let _: () = msg_send![item, setState: state];
        let _: () = msg_send![menu, addItem: item];
        release_obj(item);
    }
    let at = NSPoint::new(options.origin.x, options.origin.y + options.size.height + 4.0);
    let _: bool = msg_send![
        menu,
        popUpMenuPositioningItem: std::ptr::null::<AnyObject>(),
        atLocation: at,
        inView: view
    ];
    release_obj(menu);
    release_obj(blank);
}

/// The expanded layout's right pane: a hairline split, the source line, then
/// the entry's content — full text for text entries, the image for image ones.
unsafe fn draw_detail(
    ui: &mut ui::Ui,
    palette: &crate::theme::UiPalette,
    layout: &PickerLayout,
    h: f64,
) {
    let split_x = layout.list_w;
    ui.scene.fill(
        [split_x, header_strip_h(), 1.0, h - header_strip_h() - FOOTER_H],
        0.0,
        hairline_f4(*palette),
    );
    let pane_x = split_x + 1.0;
    let pane_w = (layout.detail_w - DETAIL_PAD * 2.0).max(0.0);
    let top = header_strip_h() + DETAIL_PAD;
    let bottom = h - FOOTER_H - DETAIL_PAD;
    let sel = picker_selection();
    let hist = CLIP_HISTORY.lock().unwrap();
    let Some(h_idx) = mapped_index(sel).filter(|&i| i < hist.len()) else {
        return;
    };
    let entry = &hist[h_idx];
    let muted = crate::glass::panel_ink(palette.muted_text, crate::glass::PanelInk::Muted);
    let meta_font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CAPTION];
    let mut y = top;
    // Source line: the app the text was copied in (+ the entry's day group).
    let meta = if entry.source_app.is_empty() {
        group_label(day_group(entry.copied_at))
    } else {
        format!("{} · {}", entry.source_app, group_label(day_group(entry.copied_at)))
    };
    ui.label(
        &meta,
        meta_font,
        muted,
        label_style_tag("detail-meta", palette, [0.0; 4]),
        pane_x + DETAIL_PAD,
        y,
        pane_w,
    );
    y += DETAIL_META_H;
    if let Some(image) = &entry.image {
        // The entry's preview PNG, decoded and fitted to the pane.
        let data: *mut AnyObject = msg_send![
            class!(NSData),
            dataWithBytes: image.preview_png.as_ptr() as *const c_void,
            length: image.preview_png.len()
        ];
        let im: *mut AnyObject = msg_send![class!(NSImage), alloc];
        let im: *mut AnyObject = msg_send![im, initWithData: data];
        if !im.is_null() {
            let size: NSSize = msg_send![im, size];
            if size.width > 0.0 && size.height > 0.0 {
                let fit = (pane_w / size.width).min((bottom - y) / size.height).min(1.0);
                if let Some(tex) = text::rasterize_nsimage(ui.renderer, im, ui.scale * fit.max(0.05)) {
                    let dw = size.width * fit;
                    let dh = size.height * fit;
                    ui.scene.image(
                        tex,
                        [pane_x + DETAIL_PAD, y, dw, dh],
                        1.0,
                        None,
                    );
                }
            }
            release_obj(im);
        }
        return;
    }
    // Text: one CTLine per source line, drawn top-down and clipped at the pane.
    let kind = classify_text(&entry.text);
    let style_tag = label_style_tag("detail-lines", palette, [0.0; 4]);
    let renderer = ui.renderer;
    let scale = ui.scale;
    ui.scene.with_clip([pane_x, top - 4.0, layout.detail_w, bottom - top + 8.0], |s| unsafe {
        for line in entry.text.split('\n') {
            if y > bottom {
                break;
            }
            let attr = make_content_attributed(line, kind);
            if attr.is_null() {
                y += DETAIL_EMPTY_LINE_H;
                continue;
            }
            if let Some(bmp) = text::rasterize_attributed(renderer, attr, style_tag, scale, pane_w) {
                s.image(bmp.texture, [pane_x + DETAIL_PAD - text::PAD_PT, y - text::PAD_PT, bmp.w, bmp.h], 1.0, None);
                y += bmp.h - text::PAD_PT * 2.0 + DETAIL_LINE_GAP;
            }
            release_obj(attr);
        }
    });
}

unsafe fn draw_header(
    ui: &mut ui::Ui,
    palette: &crate::theme::UiPalette,
    w: f64,
    action: &mut Action,
) {
    let (search, pause, options, pin, close) = picker_header_frames_for(w);
    // The header orbs (pause / options / pin) and the plain close button.
    let small_font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 12.0];
    let icon_color = crate::glass::panel_ink(
        palette.secondary_text,
        crate::glass::PanelInk::Secondary,
    );
    let hover_wash = metal_wash(*palette, false, true);
    let r = crate::theme::RADIUS_LEGEND_CHIP;
    if orb_button(
        ui,
        pause,
        if collection_paused() { "▶" } else { "⏸" },
        small_font,
        icon_color,
        label_style_tag("hdr-pause", palette, [0.0; 4]),
        collection_paused(),
        palette.dark,
    ) {
        *action = Action::TogglePause;
    }
    if orb_button(
        ui,
        options,
        "⋯",
        small_font,
        icon_color,
        label_style_tag("hdr-opt", palette, [0.0; 4]),
        *CLIP_FILTER.lock().unwrap() != ClipFilter::All,
        palette.dark,
    ) {
        *action = Action::OptionsMenu;
    }
    if orb_button(
        ui,
        pin,
        if super::pin::is_pinned() { "★" } else { "☆" },
        small_font,
        icon_color,
        label_style_tag("hdr-pin", palette, [0.0; 4]),
        super::pin::is_pinned(),
        palette.dark,
    ) {
        *action = Action::TogglePanelPin;
    }
    if ui.icon_button(
        close,
        "×",
        small_font,
        icon_color,
        label_style_tag("hdr-close", palette, [0.0; 4]),
        if palette.dark { hover_wash } else { [1.0, 1.0, 1.0, 0.32] },
        r,
    ) {
        *action = Action::Hide;
    }
    // The search capsule: warm field fill, a white glass rim, and the mock's
    // soft accent halo behind it while focused (always, here).
    let focused = true;
    let (fill, ring) = if palette.dark {
        (
            render::f4(picker_field_fill(focused)),
            render::f4(picker_field_ring(focused)),
        )
    } else if focused {
        (FIELD_BG_LG, [1.0, 1.0, 1.0, 0.58])
    } else {
        (FIELD_BG_LG_IDLE, [1.0, 1.0, 1.0, 0.45])
    };
    let _ = ACCENT_GLOW;
    ui.scene.border(
        [
            search.origin.x,
            search.origin.y,
            search.size.width,
            search.size.height,
        ],
        SEARCH_R.max(18.0),
        fill,
        ring,
        1.0,
    );
    let query = with_clipboard_ui(|u| u.search_query.clone());
    let font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: SEARCH_FONT_SIZE];
    let line_y = search.origin.y + (search.size.height - SEARCH_FONT_SIZE * 1.4) / 2.0;
    if query.is_empty() {
        // "⌕" + placeholder, in one attributed run.
        let hint = t("clipboard.search_placeholder");
        ui.label(
            &format!("⌕  {hint}"),
            font,
            icon_color,
            label_style_tag("search-hint", palette, text::nscolor_f4(icon_color)),
            search.origin.x + SEARCH_PAD_IN,
            line_y,
            search.size.width - SEARCH_PAD_IN * 2.0 - SEARCH_CLEAR_W,
        );
        // ⌘F keycap on the right.
        let chip = ui::rect(
            search.origin.x + search.size.width - SEARCH_PAD_IN - 25.0,
            search.origin.y + (search.size.height - 18.0) / 2.0,
            25.0,
            18.0,
        );
        ui.tile(chip, 5.0, render::f4(palette.field_bg));
        let cap_font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 10.0];
        ui.centered_label(
            "⌘F",
            cap_font,
            icon_color,
            label_style_tag("cap-cmdf", palette, [0.0; 4]),
            chip,
        );
    } else {
        ui.label(
            &format!("⌕  {query}"),
            font,
            crate::glass::panel_ink(palette.primary_text, crate::glass::PanelInk::Primary),
            label_style_tag("search-q", palette, [0.0; 4]),
            search.origin.x + SEARCH_PAD_IN,
            line_y,
            search.size.width - SEARCH_PAD_IN * 2.0 - SEARCH_CLEAR_W,
        );
        // The clear × on the right — a widget, its click clears the query.
        let clear_r = ui::rect(
            search.origin.x + search.size.width - SEARCH_PAD_IN - SEARCH_CLEAR_W,
            search.origin.y,
            SEARCH_CLEAR_W,
            search.size.height,
        );
        if ui.icon_button(
            clear_r,
            "×",
            font,
            icon_color,
            label_style_tag("search-x", palette, [0.0; 4]),
            hover_wash,
            r,
        ) {
            *action = Action::ClearSearch;
        }
    }
    // The filter-active dot on the options orb (the mock's `.orb-dot`).
    if *CLIP_FILTER.lock().unwrap() != ClipFilter::All {
        let dot = [options.origin.x + options.size.width - 9.0, options.origin.y + 4.0, 5.0, 5.0];
        ui.scene.fill([dot[0] - 1.0, dot[1] - 1.0, 7.0, 7.0], 3.5, [1.0, 1.0, 1.0, 0.58]);
        ui.scene.fill(dot, 2.5, if palette.dark { ACCENT_DARK } else { ACCENT_LG });
    }
    // The header's bottom hairline separates it from the list.
    ui.scene.fill(
        [0.0, header_strip_h() - 1.0, w, 1.0],
        0.0,
        hairline_f4(*palette),
    );
}

unsafe fn draw_rows(
    ui: &mut ui::Ui,
    palette: &crate::theme::UiPalette,
    w: f64,
    scroll: f64,
    burn: bool,
    action: &mut Action,
) {
    let hist = CLIP_HISTORY.lock().unwrap();
    let filtered = with_clipboard_ui(|u| u.filtered.clone());
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
                ui.renderer,
                attr,
                label_style_tag("empty", palette, [0.0; 4]),
                ui.scale,
                w - 40.0,
            ) {
                ui.scene.image(
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
            ui.label(
                &group_label(group),
                font,
                crate::glass::panel_ink(palette.muted_text, crate::glass::PanelInk::Muted),
                label_style_tag("group", palette, [0.0; 4]),
                PAD_X + ROW_PAD_L,
                y + GROUP_LABEL_PAD,
                row_w,
            );
            y += GROUP_H;
        }
        let row_h = pitches[i] - if has_hdr { GROUP_H } else { 0.0 };
        // Row wash: selection or hover — the hover is the row's own rect test
        // now, answered where the row is built (the mygo pattern).
        let row_r = ui::rect(PAD_X, y, row_w, row_h);
        let selected = i == sel;
        let hovered = ui.hot(row_r);
        let wash = metal_wash(*palette, selected, hovered);
        ui.scene.fill(
            [PAD_X, y, row_w, row_h],
            ROW_R_LG,
            wash,
        );
        // Selection carries no accent bar: the mock is a warm wash plus a
        // specular inset line along the row's top edge.
        if selected && !palette.dark {
            ui.scene.fill(
                [PAD_X + 10.0, y + 1.0, row_w - 20.0, 1.0],
                0.5,
                [1.0, 1.0, 1.0, 0.26],
            );
        }
        draw_row_content(ui, palette, entry, i, hovered, y, row_h, row_w, w, style_tag, action);
        // The row's click pastes (any action button built above has already
        // claimed a click on its own rect).
        if ui.clicked(row_r) {
            *action = Action::Paste { idx: i, burn };
        }
    }
}

unsafe fn draw_row_content(
    ui: &mut ui::Ui,
    palette: &crate::theme::UiPalette,
    entry: &ClipEntry,
    display_idx: usize,
    hovered: bool,
    y: f64,
    row_h: f64,
    row_w: f64,
    panel_w: f64,
    style_tag: u64,
    action: &mut Action,
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
            if let Some(tex) = text::rasterize_nsimage(ui.renderer, im, ui.scale) {
                ui.scene.image(tex, [content_x, content_y, content_w, content_h], 1.0, None);
            }
            release_obj(im);
        } else {
            draw_entry_text(ui, entry, style_tag, content_x, content_y, content_w, content_h);
        }
    } else {
        draw_entry_text(ui, entry, style_tag, content_x, content_y, content_w, content_h);
    }
    draw_row_actions(ui, palette, entry, display_idx, hovered, y, row_h, panel_w, action);
}

unsafe fn draw_entry_text(
    ui: &mut ui::Ui,
    entry: &ClipEntry,
    style_tag: u64,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
) {
    let kind = classify_text(&entry.text);
    let attr = make_content_attributed(&entry.text, kind);
    if attr.is_null() {
        return;
    }
    if let Some(bmp) = text::rasterize_attributed(ui.renderer, attr, style_tag, ui.scale, w) {
        // Vertically centre the line's bitmap in the content band.
        let ty = y + (h - bmp.h) / 2.0 - text::PAD_PT;
        ui.scene.image(bmp.texture, [x - text::PAD_PT, ty, bmp.w, bmp.h], 1.0, None);
    }
    release_obj(attr);
}

/// The row's right-edge action buttons' rects, right to left: delete, detail,
/// pin (the AppKit row's hover actions).
fn row_action_rects(y: f64, row_h: f64, list_w: f64) -> [NSRect; 3] {
    let act_y = y + (row_h - ACTION_H) / 2.0;
    let x_del = list_w - PAD_X - ROW_PAD_R - ACTION_BTN;
    let x_det = x_del - ACTION_BTN - 2.0;
    let x_pin = x_det - ACTION_BTN - 2.0;
    [
        ui::rect(x_pin, act_y, ACTION_BTN, ACTION_H),
        ui::rect(x_det, act_y, ACTION_BTN, ACTION_H),
        ui::rect(x_del, act_y, ACTION_BTN, ACTION_H),
    ]
}

/// Row actions (clipboard-v2): pin + trash glyphs sit faintly on every row —
/// hover tiles them and reveals the detail arrow between them.
unsafe fn draw_row_actions(
    ui: &mut ui::Ui,
    palette: &crate::theme::UiPalette,
    entry: &ClipEntry,
    display_idx: usize,
    hovered: bool,
    y: f64,
    row_h: f64,
    list_w: f64,
    action: &mut Action,
) {
    let [pin_r, det_r, del_r] = row_action_rects(y, row_h, list_w);
    let font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: ACTION_GLYPH_SIZE];
    let ink = crate::glass::panel_ink(palette.secondary_text, crate::glass::PanelInk::Secondary);
    let r = crate::theme::RADIUS_LEGEND_CHIP;
    if !hovered {
        // Idle rows carry no chrome in the reference — only the pinned mark.
        if entry.pinned {
            let idle = crate::glass::panel_ink(palette.secondary_text, crate::glass::PanelInk::Secondary);
            ui.centered_label(
                "★",
                font,
                idle,
                label_style_tag("row-pin-i", palette, [0.0; 4]),
                pin_r,
            );
            if ui.clicked(pin_r) {
                *action = Action::PinEntry(display_idx);
            }
        }
        return;
    }
    let hover_wash = metal_wash(*palette, false, true);
    if ui.icon_button(
        pin_r,
        if entry.pinned { "★" } else { "☆" },
        font,
        ink,
        label_style_tag("row-pin", palette, [0.0; 4]),
        hover_wash,
        r,
    ) {
        *action = Action::PinEntry(display_idx);
    }
    if ui.icon_button(
        det_r,
        "→",
        font,
        ink,
        label_style_tag("row-det", palette, [0.0; 4]),
        hover_wash,
        r,
    ) {
        *action = Action::OpenDetail;
    }
    if ui.icon_button(
        del_r,
        "×",
        font,
        ink,
        label_style_tag("row-del", palette, [0.0; 4]),
        [0.85, 0.2, 0.2, 0.85],
        r,
    ) {
        *action = Action::DeleteEntry(display_idx);
    }
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
        SCROLL_LG
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
    ui: &mut ui::Ui,
    palette: &crate::theme::UiPalette,
    w: f64,
    h: f64,
) {
    // Top hairline.
    ui.scene.fill(
        [0.0, h - FOOTER_H, w, 1.0],
        0.0,
        hairline_f4(*palette),
    );
    let total = CLIP_HISTORY.lock().unwrap().len();
    let count = t_count("clipboard.footer_count", total);
    let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: FOOTER_TEXT_SIZE];
    let muted = crate::glass::panel_ink(palette.muted_text, crate::glass::PanelInk::Muted);
    ui.label(
        &count,
        font,
        muted,
        label_style_tag("footer-count", palette, [0.0; 4]),
        FOOTER_PAD_X,
        h - FOOTER_H + (FOOTER_H - 14.0) / 2.0,
        FOOTER_COUNT_W,
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
            ui.renderer,
            key_attr,
            label_style_tag("kbd-key", palette, [0.0; 4]),
            ui.scale,
            f64::INFINITY,
        );
        let label_bmp = text::rasterize_attributed(
            ui.renderer,
            label_attr,
            label_style_tag("kbd-label", palette, [0.0; 4]),
            ui.scale,
            f64::INFINITY,
        );
        let key_w = key_bmp.map(|b| b.w - text::PAD_PT * 2.0 + KBD_CELL_PADDING).unwrap_or(21.0).max(21.0);
        let label_w = label_bmp.map(|b| b.w - text::PAD_PT * 2.0).unwrap_or(30.0);
        let group_w = key_w + 5.0 + label_w;
        x -= group_w;
        let kbd_h = 19.0;
        let cap = [x, h - FOOTER_H + (FOOTER_H - kbd_h) / 2.0, key_w, kbd_h];
        ui.scene.fill(cap, crate::theme::RADIUS_LEGEND_CHIP, chip_f4(palette));
        if let Some(bmp) = key_bmp {
            ui.scene.image(
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
            ui.scene.image(
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
// Input — pointer events only feed the frame's cursor/click; the widgets'
// `hot`/`clicked` queries during the build are the hit test (mygo's `ui`
// pattern: no second pass re-deriving the same geometry).
// ---------------------------------------------------------------------------

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
        metal_view::queue_redraw();
    }
}

unsafe fn mouse_moved_cb(event: *mut AnyObject, point: NSPoint) {
    // A grabbed blank region drags the panel: the gesture owns the cursor until
    // its release, so hover bookkeeping stays out of the way.
    if super::drag::drag_in_progress() {
        super::drag::drag_mouse_dragged(std::ptr::null_mut(), sel!(noop:), event);
        return;
    }
    with_picker((), |p| p.cursor = Some(point));
    metal_view::queue_redraw();
}

/// The up-stroke of a panel drag (`handlers.mouse_up` is registered only for it).
unsafe fn mouse_up_cb(event: *mut AnyObject, _point: NSPoint) {
    if super::drag::drag_in_progress() {
        super::drag::drag_mouse_up(std::ptr::null_mut(), sel!(noop:), event);
    }
}

unsafe fn mouse_exited_cb(_event: *mut AnyObject) {
    let changed = with_picker(false, |p| {
        let changed = p.cursor.is_some();
        p.cursor = None;
        changed
    });
    if changed {
        metal_view::queue_redraw();
    }
}

/// Blank regions of the panel (header outside its controls, the footer strip,
/// the detail pane) are drag handles, the same rule the AppKit picker's blank
/// views follow; interactive areas still queue a click for the build pass.
fn in_drag_zone(p: NSPoint, w: f64, h: f64) -> bool {
    let (search, pause, options, pin, close) = picker_header_frames_for(w);
    if p.y < header_strip_h() {
        return !(ui::in_rect(p, search)
            || ui::in_rect(p, pause)
            || ui::in_rect(p, options)
            || ui::in_rect(p, pin)
            || ui::in_rect(p, close));
    }
    p.y >= h - FOOTER_H
}

/// A mouse-down is the frame's pending click: stored with the Option flag,
/// dispatched by the next build pass's `clicked` queries (draw = hit test).
unsafe fn mouse_down_cb(event: *mut AnyObject, point: NSPoint) {
    let w = with_picker(0.0, |p| {
        let f: NSRect = msg_send![p.view, frame];
        f.size.width
    });
    let h = with_picker(0.0, |p| {
        let f: NSRect = msg_send![p.view, frame];
        f.size.height
    });
    if in_drag_zone(point, w, h) {
        // `Sel` is a NonNull-backed type — passing `mem::zeroed()` is UB and a
        // debug-build precondition panic (the 22:07/22:22 crashes). The drag
        // externs ignore `_cmd`, so any valid selector does.
        super::drag::drag_mouse_down(std::ptr::null_mut(), sel!(noop:), event);
        return;
    }
    let mods: u64 = msg_send![event, modifierFlags];
    with_picker((), |p| {
        p.click = Some((point, (mods & 0x0008_0000) != 0));
    });
    metal_view::redraw();
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

/// ← / the row's pin button: toggle the selected entry's pin (the same code
/// path as the AppKit handler — reorder, persist, re-select).
fn pin_selected() {
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

/// ⌫ / the row's delete button: remove the entry, keep the undo affordance.
fn delete_entry_at(display_idx: usize) {
    let Some(h_idx) = mapped_index(display_idx) else {
        return;
    };
    let mut hist = CLIP_HISTORY.lock().unwrap();
    let Some(removed) = remove_entry_for_undo(&mut hist, h_idx) else {
        return;
    };
    // A deleted row ABOVE the selection shifts it down one (same entry stays
    // selected); deleting the selected row or one below leaves it alone.
    let previous = picker_selection();
    if previous != NO_SELECTION && display_idx < previous {
        set_picker_selection(previous - 1);
    }
    drop(hist);
    remember_deleted_clipboard_entry(removed, h_idx);
    save_history();
    refresh();
}

/// → / the row's detail button and Esc's close: resize the panel between the
/// list-only width and the split detail width, centring like show_picker does.
unsafe fn set_detail_open(open: bool) {
    let changed = with_picker(false, |p| {
        let changed = p.detail != open;
        p.detail = open;
        changed
    });
    if !changed {
        return;
    }
    let (window, view) = with_picker((std::ptr::null_mut(), std::ptr::null_mut()), |p| {
        (p.window, p.view)
    });
    if window.is_null() {
        return;
    }
    let frame = crate::glass::panel_frame_of(window);
    let screen_frame = picker_screen_frame(window);
    let layout = picker_layout(screen_frame.size.width, open);
    let mut target = frame;
    target.size.width = layout.panel_w;
    // Widening keeps the panel's centre (same rule as the AppKit expansion).
    target.origin.x += (frame.size.width - layout.panel_w) / 2.0;
    target.origin =
        super::drag::clamp_picker_origin(target.origin, target.size, screen_frame);
    crate::glass::set_panel_frame(window, target, true);
    let _: () = msg_send![view, setFrame: NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(target.size.width, target.size.height)
    )];
    refresh();
}

unsafe fn key_down_cb(event: *mut AnyObject) {
    let keycode: u16 = msg_send![event, keyCode];
    let mods: u64 = msg_send![event, modifierFlags];
    // ⌘Z: undo the last deleted entry (same shortcut gate as the AppKit handler).
    if is_clipboard_undo_shortcut(keycode, mods) {
        if undo_deleted_clipboard_entry().is_some() {
            refresh();
            show_toast(&t("clipboard.toast_undo_delete"));
        }
        return;
    }
    match keycode {
        126 => move_selection(-1), // up
        125 => move_selection(1),  // down
        123 => pin_selected(),     // left: pin toggle
        124 => {
            // Right: toggle the detail pane.
            let detail = with_picker(false, |p| p.detail);
            set_detail_open(!detail);
        }
        36 | 76 => {
            // Enter; Option+Enter = paste and delete (one-shot paste).
            let idx = picker_selection();
            if idx != NO_SELECTION {
                paste_at_ex(idx, (mods & 0x0008_0000) != 0);
            }
        }
        53 => {
            // Esc: detail first, then the query, then (unpinned) dismiss.
            if with_picker(false, |p| p.detail) {
                set_detail_open(false);
                return;
            }
            let has_query = with_clipboard_ui(|ui| !ui.search_query.is_empty());
            if has_query {
                with_clipboard_ui(|ui| ui.search_query.clear());
                refresh();
            } else if !super::pin::is_pinned() {
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
            // Backspace: drop the last query char, or delete the selected entry
            // when the query is empty.
            let removed = with_clipboard_ui(|ui| ui.search_query.pop().is_some());
            if removed {
                set_picker_selection(0);
                refresh();
            } else {
                delete_entry_at(picker_selection());
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
            if (mods & 0x0010_0000) != 0 {
                return; // let Command combos pass elsewhere
            }
            with_clipboard_ui(|ui| ui.search_query.push_str(&s));
            set_picker_selection(0);
            refresh();
        }
    }
}
