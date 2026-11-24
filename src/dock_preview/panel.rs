//! The Dock preview panel and its tooltip.
//!
//! One borderless, non-activating `NSPanel` is built once and reused: opening it for another app
//! rebuilds its cards, and hovering the same app only repositions it. The panel never becomes key,
//! so the app the pointer came from keeps keyboard focus; the container view accepts the first
//! mouse so a click on a card acts immediately instead of being spent activating the window.
//!
//! Everything the panel owns is touched on the AppKit main thread only. The observer and the window
//! collector hand in plain data; this file turns that data into views.

use std::ffi::c_void;
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::Instant;

use objc2::runtime::{AnyObject, Bool, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use crate::dock_preview::layout::{self, DockEdge, PointerZone, Rect, CARD_RADIUS, CLOSE_SIZE};
use crate::dock_preview::windows::{self, PreviewWindow};
use crate::ffi::{
    hex_to_cg_color, hex_to_ns_color, layer_set_background, layer_set_border, layer_set_contents,
    layer_set_contents_scale, make_nsstring, release_obj, MainThreadSlot,
};
use crate::glass;
use crate::theme;

/// One app's panel content, as the collector produced it.
pub(crate) struct PanelRequest {
    pub(crate) pid: i32,
    pub(crate) thumbnail_generation: u64,
    pub(crate) bundle: String,
    pub(crate) identity_key: String,
    pub(crate) process_start_time_us: Option<u64>,
    pub(crate) windows: Vec<PreviewWindow>,
    pub(crate) anchor: Rect,
    pub(crate) edge: DockEdge,
    pub(crate) visible: Rect,
}

/// What a main-thread tick decided about the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TickOutcome {
    /// Nothing changed.
    Idle,
    /// The panel became empty and must be hidden by the caller.
    Empty,
}

struct CardEntry {
    window: PreviewWindow,
    /// The hover plate: the switcher's selection wash + accent rim, hidden until the card is hovered.
    plate: *mut AnyObject,
    thumbnail: *mut AnyObject,
    placeholder: *mut AnyObject,
    placeholder_icon: *mut AnyObject,
    header_icon: *mut AnyObject,
    title: *mut AnyObject,
    close: *mut AnyObject,
    title_truncated: bool,
    has_frame: bool,
    requested_at: Option<Instant>,
    closing_since: Option<Instant>,
    /// The close worker is still talking to the target's Accessibility API.
    close_pending: bool,
    last_probe: Instant,
}

struct Geometry {
    panel: Rect,
    cards: Vec<Rect>,
    anchor: Rect,
    band: Rect,
    edge: DockEdge,
    visible: Rect,
}

struct PanelState {
    panel: *mut AnyObject,
    content_size: (f64, f64),
    container: *mut AnyObject,
    tooltip_panel: *mut AnyObject,
    tooltip_label: *mut AnyObject,
    geometry: Geometry,
    request: u64,
    ticket: u64,
    pid: i32,
    thumbnail_generation: u64,
    bundle: String,
    identity_key: String,
    process_start_time_us: Option<u64>,
    cards: Vec<CardEntry>,
    window_ids: Vec<u32>,
    total_windows: usize,
    hovered: Option<usize>,
    /// Last pointer position in screen top-left coordinates, from the panel's own tracking.
    pointer: Option<(f64, f64)>,
    last_zone: PointerZone,
    title_hover_since: Option<Instant>,
    tooltip_visible: bool,
    tooltip_text: String,
    fill_deadline: Instant,
    last_fill: Instant,
    /// When the panel was last positioned; the tooltip follows it.
    shown_at: Instant,
}

static PANEL: MainThreadSlot<Option<PanelState>> = unsafe { MainThreadSlot::new(None) };

/// `--e2e-state` reads the panel itself, so recording from inside a `PANEL` borrow would re-enter
/// the slot and publish `null`. Events are queued here and flushed after the borrow is released.
static DEFERRED_RECORDS: LazyLock<Mutex<Vec<&'static str>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

fn defer_record(name: &'static str) {
    if let Ok(mut queue) = DEFERRED_RECORDS.lock() {
        queue.push(name);
    }
}

/// Publish the queued `--e2e-state` records. Called after every panel mutation path.
pub(crate) fn flush_records() {
    let pending: Vec<&'static str> = {
        let Ok(mut queue) = DEFERRED_RECORDS.lock() else {
            return;
        };
        std::mem::take(&mut *queue)
    };
    for name in pending {
        crate::e2e_state::record(name);
    }
}

/// The panel's screen rectangle (top-left CoreGraphics coordinates), for the caller's pointer tests.
pub(crate) fn panel_frame() -> Option<Rect> {
    PANEL
        .lock()
        .ok()?
        .as_ref()
        .map(|state| state.geometry.panel)
}

/// The hovered tile rectangle and Dock edge the open panel is anchored to.
pub(crate) fn anchor_rect() -> Option<(Rect, DockEdge)> {
    PANEL
        .lock()
        .ok()?
        .as_ref()
        .map(|state| (state.geometry.anchor, state.geometry.edge))
}

pub(crate) fn is_visible() -> bool {
    PANEL
        .lock()
        .ok()
        .and_then(|state| {
            state.as_ref().map(|state| {
                let visible: Bool = unsafe { msg_send![state.panel, isVisible] };
                visible == Bool::YES
            })
        })
        .unwrap_or(false)
}

/// Build (or reuse) the panel and show `request`'s content.
pub(crate) fn show(request: PanelRequest, request_epoch: u64, ticket: u64) -> bool {
    let geometry = geometry_for(&request);
    let needed = (geometry.panel.w, geometry.panel.h);
    let Ok(mut slot) = PANEL.lock() else {
        return false;
    };
    let needs_rebuild = slot
        .as_ref()
        .is_some_and(|state| state.content_size != needed);
    if needs_rebuild {
        if let Some(old) = slot.take() {
            destroy(old);
        }
    }
    if slot.is_none() {
        *slot = build(needed.0, needed.1);
    }
    let Some(state) = slot.as_mut() else {
        return false;
    };
    let same = state.pid == request.pid
        && state.window_ids
            == request
                .windows
                .iter()
                .map(|window| window.wid)
                .collect::<Vec<_>>();
    if !same {
        rebuild_cards(state, &request, request_epoch, ticket);
    }
    state.pid = request.pid;
    state.thumbnail_generation = request.thumbnail_generation;
    state.bundle = request.bundle.clone();
    state.identity_key = request.identity_key.clone();
    state.process_start_time_us = request.process_start_time_us;
    state.request = request_epoch;
    state.ticket = ticket;
    state.geometry = geometry;
    place_window(state);
    let _: () = unsafe { msg_send![state.panel, orderFrontRegardless] };
    let _: () = unsafe { msg_send![state.panel, setAlphaValue: 1.0f64] };
    state.shown_at = Instant::now();
    state.fill_deadline = Instant::now() + crate::dock_preview::decisions::THUMB_FILL_TIMEOUT;
    state.last_fill = Instant::now();
    apply_hover(state, state.hovered);
    true
}

/// Release a panel whose content size no longer matches the next request. The material hierarchy
/// is sized to the panel, so a different card grid needs a new hierarchy rather than a resized one.
fn destroy(state: PanelState) {
    unsafe {
        let _: () = msg_send![state.panel, orderOut: std::ptr::null::<AnyObject>()];
        let _: () = msg_send![state.tooltip_panel, orderOut: std::ptr::null::<AnyObject>()];
        release_obj(state.tooltip_panel);
        release_obj(state.panel);
    }
}

/// Hide the panel and its tooltip, leaving the views for the next hover.
pub(crate) fn hide() {
    let Ok(mut slot) = PANEL.lock() else {
        return;
    };
    let Some(state) = slot.as_mut() else {
        return;
    };
    hide_tooltip(state);
    let _: () = unsafe { msg_send![state.panel, orderOut: std::ptr::null::<AnyObject>()] };
    for card in state.cards.drain(..) {
        remove_card_views(&card);
    }
    state.hovered = None;
    state.pointer = None;
    state.last_zone = PointerZone::Outside;
    state.title_hover_since = None;
    state.window_ids.clear();
    state.total_windows = 0;
    state.request = 0;
}

/// Main-thread tick: fill thumbnails, confirm pending window closes, update the tooltip.
pub(crate) fn tick(now: Instant, pointer: (f64, f64)) -> TickOutcome {
    let Ok(mut slot) = PANEL.lock() else {
        return TickOutcome::Idle;
    };
    let Some(state) = slot.as_mut() else {
        return TickOutcome::Idle;
    };
    if !visible_now(state) {
        return TickOutcome::Idle;
    }
    fill_thumbnails(state, now);
    let mut removed = false;
    let mut index = 0;
    while index < state.cards.len() {
        let Some(started) = state.cards[index].closing_since else {
            index += 1;
            continue;
        };
        if now.duration_since(state.cards[index].last_probe)
            < crate::dock_preview::decisions::TICK_INTERVAL
        {
            index += 1;
            continue;
        }
        state.cards[index].last_probe = now;
        let presence = windows::window_presence_fast(
            state.cards[index].window.pid,
            state.cards[index].window.wid,
        );
        match crate::dock_preview::decisions::close_confirmation(
            presence,
            now.duration_since(started),
        ) {
            crate::dock_preview::decisions::CloseConfirmation::Remove => {
                let card = state.cards.remove(index);
                remove_card_views(&card);
                // The window no longer exists, so it is not "hidden by the cap" either.
                state.total_windows = state.total_windows.saturating_sub(1);
                removed = true;
            }
            crate::dock_preview::decisions::CloseConfirmation::Keep => {
                state.cards[index].closing_since = None;
                unsafe { set_close_enabled(&state.cards[index], true) };
                index += 1;
            }
            crate::dock_preview::decisions::CloseConfirmation::Wait => index += 1,
        }
    }
    if removed {
        relayout_after_removal(state);
        if state.cards.is_empty() {
            hide_tooltip(state);
            return TickOutcome::Empty;
        }
    }
    // Hover and tooltip are driven by the real pointer position read once per tick: AppKit does
    // not promise mouseMoved delivery to a passive, inactive panel, and a panel that only reacts to
    // events would miss a pointer resting inside it.
    let local = (
        pointer.0 - state.geometry.panel.x,
        pointer.1 - state.geometry.panel.y,
    );
    let zone = layout::pointer_zone(
        local,
        Rect::new(0.0, 0.0, state.geometry.panel.w, state.geometry.panel.h),
        &state.geometry.cards,
        Rect::new(
            state.geometry.anchor.x - state.geometry.panel.x,
            state.geometry.anchor.y - state.geometry.panel.y,
            state.geometry.anchor.w,
            state.geometry.anchor.h,
        ),
        Rect::new(
            state.geometry.band.x - state.geometry.panel.x,
            state.geometry.band.y - state.geometry.panel.y,
            state.geometry.band.w,
            state.geometry.band.h,
        ),
        state.hovered,
    );
    state.pointer = Some(pointer);
    let zone = match zone {
        PointerZone::Card(index) | PointerZone::Title(index) | PointerZone::Close(index)
            if state
                .cards
                .get(index)
                .is_some_and(|card| card.closing_since.is_some() || card.close_pending) =>
        {
            PointerZone::Panel
        }
        zone => zone,
    };
    state.last_zone = zone;
    update_hover(state, zone);
    update_tooltip(state, now);
    TickOutcome::Idle
}

pub(crate) fn card_window(index: usize) -> Option<PreviewWindow> {
    let Ok(slot) = PANEL.lock() else {
        return None;
    };
    let state = slot.as_ref()?;
    state.cards.get(index).map(|card| card.window.clone())
}

/// A close was requested and the worker is about to ask the target's Accessibility API. The card
/// stops taking clicks immediately, but the bounded "wait until the window disappears" clock starts
/// only when the AX call is accepted (`confirm_close_matching`).
pub(crate) fn begin_close_pending(index: usize) {
    let Ok(mut slot) = PANEL.lock() else {
        return;
    };
    let Some(state) = slot.as_mut() else {
        return;
    };
    let Some(card) = state.cards.get_mut(index) else {
        return;
    };
    card.close_pending = true;
    unsafe { set_close_enabled(card, false) };
}

/// Navigate by window identity, never by index: cards can disappear while a close result is in
/// flight, so the index that started the close may now name another card.
pub(crate) fn confirm_close_matching(pid: i32, wid: u32) {
    let Ok(mut slot) = PANEL.lock() else {
        return;
    };
    let Some(state) = slot.as_mut() else {
        return;
    };
    let Some(card) = state
        .cards
        .iter_mut()
        .find(|card| card.window.pid == pid && card.window.wid == wid)
    else {
        return;
    };
    card.close_pending = false;
    card.closing_since = Some(Instant::now());
    card.last_probe = Instant::now() - crate::dock_preview::decisions::TICK_INTERVAL;
}

/// Whether a card is waiting for its window to disappear after an accepted close.
pub(crate) fn card_is_closing(index: usize) -> bool {
    let Ok(slot) = PANEL.lock() else {
        return false;
    };
    slot.as_ref()
        .and_then(|state| state.cards.get(index))
        .is_some_and(|card| card.closing_since.is_some() || card.close_pending)
}

/// The AX close was refused or failed: put the identified card back into its normal state.
pub(crate) fn cancel_close_matching(pid: i32, wid: u32) {
    let Ok(mut slot) = PANEL.lock() else {
        return;
    };
    let Some(state) = slot.as_mut() else {
        return;
    };
    let Some(card) = state
        .cards
        .iter_mut()
        .find(|card| card.window.pid == pid && card.window.wid == wid)
    else {
        return;
    };
    card.closing_since = None;
    card.close_pending = false;
    unsafe { set_close_enabled(card, true) };
}

/// The pointer hover state, recomputed from the real pointer once per tick.
fn update_hover(state: &mut PanelState, zone: PointerZone) {
    let hovered = match zone {
        PointerZone::Card(index) | PointerZone::Title(index) | PointerZone::Close(index) => {
            Some(index)
        }
        _ => None,
    };
    if hovered != state.hovered {
        state.hovered = hovered;
        state.title_hover_since = None;
        apply_hover(state, hovered);
        defer_record("dock_preview_hover");
    }
    if let (PointerZone::Title(index), Some(card)) = (zone, hovered) {
        let truncated = state
            .cards
            .get(card)
            .map(|card| card.title_truncated)
            .unwrap_or(false);
        if truncated && state.title_hover_since.is_none() {
            state.title_hover_since = Some(Instant::now());
        }
        if !truncated {
            state.title_hover_since = None;
        }
        let _ = index;
    } else {
        state.title_hover_since = None;
    }
}

fn visible_now(state: &PanelState) -> bool {
    let visible: Bool = unsafe { msg_send![state.panel, isVisible] };
    visible == Bool::YES
}

fn geometry_for(request: &PanelRequest) -> Geometry {
    let aspects: Vec<Option<f64>> = request.windows.iter().map(|window| window.aspect).collect();
    let widths = layout::widths_for_aspects(&aspects);
    let grid = layout::grid_for(&widths, request.visible.w, request.visible.h);
    let (panel_w, panel_h) = grid.panel_size();
    let panel = layout::panel_frame(
        request.anchor,
        (panel_w, panel_h),
        request.edge,
        request.visible,
    );
    Geometry {
        band: layout::transition_band(request.edge, request.anchor, panel),
        cards: layout::card_rects(&grid),
        panel,
        anchor: request.anchor,
        edge: request.edge,
        visible: request.visible,
    }
}

fn place_window(state: &mut PanelState) {
    let insets = glass::panel_insets_for(backdrop_options());
    let appkit = cg_rect_to_appkit(state.geometry.panel);
    let frame = NSRect::new(
        NSPoint::new(
            appkit.origin.x - insets.left,
            appkit.origin.y - insets.bottom,
        ),
        NSSize::new(
            state.geometry.panel.w + insets.left + insets.right,
            state.geometry.panel.h + insets.bottom + insets.top,
        ),
    );
    unsafe {
        let _: () = msg_send![state.panel, setFrame: frame, display: true];
        let container_frame = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(state.geometry.panel.w, state.geometry.panel.h),
        );
        let _: () = msg_send![state.container, setFrame: container_frame];
    }
}

fn cg_rect_to_appkit(rect: Rect) -> NSRect {
    let primary_h = primary_screen_height();
    NSRect::new(
        NSPoint::new(rect.x, primary_h - rect.y - rect.h),
        NSSize::new(rect.w, rect.h),
    )
}

fn primary_screen_height() -> f64 {
    unsafe {
        let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
        if screens.is_null() {
            return 0.0;
        }
        let count: usize = msg_send![screens, count];
        if count == 0 {
            return 0.0;
        }
        let screen: *mut AnyObject = msg_send![screens, objectAtIndex: 0usize];
        let frame: NSRect = msg_send![screen, frame];
        frame.size.height
    }
}

fn backdrop_options() -> glass::BackdropOptions {
    glass::BackdropOptions::new(Some(glass::INACTIVE_GLASS_COMPENSATION_ALPHA))
        .with_elevation(Some(theme::ELEVATION_HIGH))
}

// ---------------------------------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------------------------------

fn build(panel_w: f64, panel_h: f64) -> Option<PanelState> {
    unsafe {
        let options = backdrop_options();
        let insets = glass::panel_insets_for(options);
        let host_size = NSSize::new(
            panel_w + insets.left + insets.right,
            panel_h + insets.bottom + insets.top,
        );
        let panel: *mut AnyObject = msg_send![panel_class(), alloc];
        let panel: *mut AnyObject = msg_send![
            panel,
            initWithContentRect: NSRect::new(NSPoint::new(0.0, 0.0), host_size),
            styleMask: 1u64 << 7, // borderless | nonactivating panel
            backing: 2u64,
            defer: false
        ];
        if panel.is_null() {
            return None;
        }
        let _: () = msg_send![panel, setReleasedWhenClosed: false];
        let _: () = msg_send![panel, setOpaque: false];
        let clear: *mut AnyObject = msg_send![class!(NSColor), clearColor];
        let _: () = msg_send![panel, setBackgroundColor: clear];
        let _: () = msg_send![panel, setHasShadow: false];
        let _: () = msg_send![panel, setHidesOnDeactivate: false];
        let _: () = msg_send![panel, setAcceptsMouseMovedEvents: true];
        let level = 25isize; // NSStatusWindowLevel: above the Dock, below the switcher overlay.
        let _: () = msg_send![panel, setLevel: level];
        let behavior: usize = (1 << 0) | (1 << 3) | (1 << 6) | (1 << 8) | (1 << 18);
        let _: () = msg_send![panel, setCollectionBehavior: behavior];

        let host: *mut AnyObject = msg_send![class!(NSView), alloc];
        let host: *mut AnyObject = msg_send![host, initWithFrame: NSRect::new(
            NSPoint::new(0.0, 0.0),
            host_size
        )];
        let _: () = msg_send![host, setWantsLayer: true];
        let _: () = msg_send![panel, setContentView: host];

        let backdrop = glass::install_hosted_backdrop(
            host,
            NSPoint::new(insets.left, insets.bottom),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(panel_w, panel_h)),
            layout::PANEL_RADIUS,
            options,
        );
        let container: *mut AnyObject = msg_send![container_class(), alloc];
        let container: *mut AnyObject = msg_send![container, initWithFrame: NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(panel_w, panel_h)
        )];
        let _: () = msg_send![container, setWantsLayer: true];
        let _: () = msg_send![backdrop.content_parent, addSubview: container];
        release_obj(container);
        release_obj(host);

        let tooltip_panel = build_tooltip_panel();

        Some(PanelState {
            panel,
            content_size: (panel_w, panel_h),
            container,
            tooltip_panel: tooltip_panel.0,
            tooltip_label: tooltip_panel.1,
            geometry: Geometry {
                panel: Rect::new(0.0, 0.0, 1.0, 1.0),
                cards: Vec::new(),
                anchor: Rect::new(0.0, 0.0, 1.0, 1.0),
                band: Rect::new(0.0, 0.0, 0.0, 0.0),
                edge: DockEdge::Bottom,
                visible: Rect::new(0.0, 0.0, 1.0, 1.0),
            },
            request: 0,
            ticket: 0,
            pid: 0,
            thumbnail_generation: 0,
            bundle: String::new(),
            identity_key: String::new(),
            process_start_time_us: None,
            cards: Vec::new(),
            window_ids: Vec::new(),
            total_windows: 0,
            hovered: None,
            pointer: None,
            last_zone: PointerZone::Outside,
            title_hover_since: None,
            tooltip_visible: false,
            tooltip_text: String::new(),
            fill_deadline: Instant::now(),
            last_fill: Instant::now(),
            shown_at: Instant::now(),
        })
    }
}

/// The tooltip is a passive panel: it never accepts a click and never becomes key.
unsafe fn build_tooltip_panel() -> (*mut AnyObject, *mut AnyObject) {
    let panel: *mut AnyObject = msg_send![panel_class(), alloc];
    let panel: *mut AnyObject = msg_send![
        panel,
        initWithContentRect: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0)),
        styleMask: 1u64 << 7,
        backing: 2u64,
        defer: false
    ];
    let _: () = msg_send![panel, setReleasedWhenClosed: false];
    let _: () = msg_send![panel, setOpaque: false];
    let clear: *mut AnyObject = msg_send![class!(NSColor), clearColor];
    let _: () = msg_send![panel, setBackgroundColor: clear];
    let _: () = msg_send![panel, setHasShadow: true];
    let _: () = msg_send![panel, setIgnoresMouseEvents: true];
    let _: () = msg_send![panel, setHidesOnDeactivate: false];
    let _: () = msg_send![panel, setLevel: 26isize];
    let behavior: usize = (1 << 0) | (1 << 3) | (1 << 6) | (1 << 8) | (1 << 18);
    let _: () = msg_send![panel, setCollectionBehavior: behavior];

    let view: *mut AnyObject = msg_send![passthrough_view_class(), alloc];
    let view: *mut AnyObject = msg_send![view, initWithFrame: NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(1.0, 1.0)
    )];
    let _: () = msg_send![view, setWantsLayer: true];
    let palette = theme::ui_palette();
    layer_set_background(msg_send![view, layer], hex_to_cg_color(palette.window_bg));
    let view_layer: *mut AnyObject = msg_send![view, layer];
    let _: () = msg_send![view_layer, setCornerRadius: layout::THUMB_RADIUS];
    let _: () = msg_send![view_layer, setBorderWidth: 1.0f64];
    layer_set_border(msg_send![view, layer], hex_to_cg_color(palette.card_border));
    let _: () = msg_send![panel, setContentView: view];
    release_obj(view);

    let label: *mut AnyObject = msg_send![passthrough_label_class(), alloc];
    let label: *mut AnyObject = msg_send![label, initWithFrame: NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(1.0, 1.0)
    )];
    style_label(label, theme::FONT_CAPTION);
    let _: () = msg_send![label, setUsesSingleLineMode: false];
    let _: () = msg_send![label, setLineBreakMode: 0isize]; // word wrapping
    let _: () = msg_send![label, setTextColor: hex_to_ns_color(palette.primary_text)];
    let _: () = msg_send![view, addSubview: label];
    release_obj(label);
    (panel, label)
}

// ---------------------------------------------------------------------------------------------
// Cards
// ---------------------------------------------------------------------------------------------

fn rebuild_cards(state: &mut PanelState, request: &PanelRequest, epoch: u64, ticket: u64) {
    unsafe {
        for card in state.cards.drain(..) {
            remove_card_views(&card);
        }
        let aspects: Vec<Option<f64>> =
            request.windows.iter().map(|window| window.aspect).collect();
        let widths = layout::widths_for_aspects(&aspects);
        let grid = layout::grid_for(&widths, request.visible.w, request.visible.h);
        let rects: Vec<layout::Rect> = layout::card_rects(&grid)
            .into_iter()
            .take(request.windows.len())
            .collect();
        state.window_ids = request.windows.iter().map(|window| window.wid).collect();
        state.total_windows = request.windows.len();
        let icon = crate::app_identity::icon_for_bundle_id(&request.bundle, layout::CARD_ICON);
        for (index, window) in request.windows.iter().take(rects.len()).enumerate() {
            let rect = rects[index];
            let palette = theme::ui_palette();
            // The cards themselves do not paint: the panel's material is the surface between and
            // inside them (the switcher's own rule). A hovered card draws the switcher's selection
            // plate -- `selection_plate_bg` wash plus an `accent` rim -- instead of changing a
            // filled tile.
            let plate = passthrough_view(rect);
            let plate_layer: *mut AnyObject = msg_send![plate, layer];
            layer_set_background(plate_layer, hex_to_cg_color(palette.selection_plate_bg));
            let _: () = msg_send![plate_layer, setCornerRadius: CARD_RADIUS];
            let _: () = msg_send![plate_layer, setBorderWidth: 1.0f64];
            layer_set_border(plate_layer, hex_to_cg_color(palette.accent));
            let _: () = msg_send![plate, setHidden: true];

            let thumb_rect = layout::thumb_rect(rect);
            let thumbnail = passthrough_view(thumb_rect);
            let thumb_layer: *mut AnyObject = msg_send![thumbnail, layer];
            let _: () = msg_send![thumb_layer, setCornerRadius: layout::THUMB_RADIUS];
            let _: () = msg_send![thumb_layer, setMasksToBounds: true];
            let _: () = msg_send![thumb_layer, setBorderWidth: 1.0f64];
            layer_set_background(thumb_layer, hex_to_cg_color(palette.field_bg));
            layer_set_border(thumb_layer, hex_to_cg_color(palette.card_border));
            let gravity = make_nsstring("resizeAspect");
            let _: () = msg_send![thumb_layer, setContentsGravity: gravity];
            release_obj(gravity);
            // Until a frame arrives the card shows the placeholder icon, never an empty rectangle.
            let _: () = msg_send![thumbnail, setHidden: true];

            let placeholder_rect = Rect::new(
                thumb_rect.x + (thumb_rect.w - 48.0) / 2.0,
                thumb_rect.y + (thumb_rect.h - 48.0) / 2.0,
                48.0,
                48.0,
            );
            let placeholder = passthrough_view(placeholder_rect);
            let placeholder_image: *mut AnyObject = msg_send![class!(NSImageView), alloc];
            let placeholder_image: *mut AnyObject = msg_send![placeholder_image, initWithFrame: NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(48.0, 48.0)
            )];
            let _: () = msg_send![placeholder_image, setImageScaling: 3u64];
            if let Some(icon) = icon {
                let _: () = msg_send![placeholder_image, setImage: icon];
            }
            let _: () = msg_send![placeholder, addSubview: placeholder_image];
            release_obj(placeholder_image);

            let header_icon = header_icon_view(rect, icon);

            let title_rect = layout::title_rect(rect);
            let title = passthrough_label(title_rect, &window.title);
            let measured = glass::measure_single_line_text(&window.title, theme::FONT_CAPTION);
            let truncated = layout::title_is_truncated(measured.width, title_rect.w);

            let close_rect = layout::close_rect(rect);
            let close = passthrough_view(close_rect);
            let close_layer: *mut AnyObject = msg_send![close, layer];
            layer_set_background(close_layer, hex_to_cg_color(palette.destructive));
            let _: () = msg_send![close_layer, setCornerRadius: 6.0f64];
            let cross_rect = Rect::new(3.0, 2.0, CLOSE_SIZE - 6.0, CLOSE_SIZE - 4.0);
            let cross = passthrough_label_at(
                NSRect::new(
                    NSPoint::new(cross_rect.x, cross_rect.y),
                    NSSize::new(cross_rect.w, cross_rect.h),
                ),
                "✕",
            );
            let _: () = msg_send![cross, setAlignment: 2isize]; // centered
            let bold: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: theme::FONT_CAPTION, weight: theme::FONT_WEIGHT_BOLD];
            let _: () = msg_send![cross, setFont: bold];
            let _: () = msg_send![cross, setTextColor: hex_to_ns_color(0xFFFFFFFF)];
            let _: () = msg_send![close, addSubview: cross];
            release_obj(cross);
            let _: () = msg_send![close, setHidden: true];

            // Flat siblings: the hover plate is behind everything, the thumbnail over it, and the
            // header copy (icon/title/close) above.
            let _: () = msg_send![state.container, addSubview: plate];
            release_obj(plate);
            let _: () = msg_send![state.container, addSubview: thumbnail];
            release_obj(thumbnail);
            let _: () = msg_send![state.container, addSubview: placeholder];
            release_obj(placeholder);
            let _: () = msg_send![state.container, addSubview: header_icon];
            release_obj(header_icon);
            let _: () = msg_send![state.container, addSubview: title];
            release_obj(title);
            let _: () = msg_send![state.container, addSubview: close];
            release_obj(close);

            state.cards.push(CardEntry {
                window: window.clone(),
                plate,
                thumbnail,
                placeholder,
                placeholder_icon: placeholder_image,
                header_icon,
                title,
                close,
                title_truncated: truncated,
                has_frame: false,
                requested_at: None,
                closing_since: None,
                close_pending: false,
                last_probe: Instant::now(),
            });
        }
        if let Some(icon) = icon {
            release_obj(icon);
        }
        state.request = epoch;
        state.ticket = ticket;
        state.hovered = None;
        state.title_hover_since = None;
    }
}

unsafe fn header_icon_view(rect: Rect, image: Option<*mut AnyObject>) -> *mut AnyObject {
    let view: *mut AnyObject = msg_send![class!(NSImageView), alloc];
    let icon_rect = layout::icon_rect(rect);
    let view: *mut AnyObject = msg_send![view, initWithFrame: NSRect::new(
        NSPoint::new(icon_rect.x, icon_rect.y),
        NSSize::new(icon_rect.w, icon_rect.h)
    )];
    let _: () = msg_send![view, setImageScaling: 3u64];
    if let Some(image) = image {
        let _: () = msg_send![view, setImage: image];
    }
    view
}

fn remove_card_views(card: &CardEntry) {
    unsafe {
        for view in [
            card.plate,
            card.thumbnail,
            card.placeholder,
            card.header_icon,
            card.title,
            card.close,
        ] {
            if !view.is_null() {
                let _: () = msg_send![view, removeFromSuperview];
            }
        }
    }
}

unsafe fn passthrough_view(rect: Rect) -> *mut AnyObject {
    let view: *mut AnyObject = msg_send![passthrough_view_class(), alloc];
    let view: *mut AnyObject = msg_send![view, initWithFrame: NSRect::new(
        NSPoint::new(rect.x, rect.y),
        NSSize::new(rect.w, rect.h)
    )];
    let _: () = msg_send![view, setWantsLayer: true];
    view
}

unsafe fn passthrough_label(rect: Rect, text: &str) -> *mut AnyObject {
    passthrough_label_at(
        NSRect::new(NSPoint::new(rect.x, rect.y), NSSize::new(rect.w, rect.h)),
        text,
    )
}

unsafe fn passthrough_label_at(frame: NSRect, text: &str) -> *mut AnyObject {
    let label: *mut AnyObject = msg_send![passthrough_label_class(), alloc];
    let label: *mut AnyObject = msg_send![label, initWithFrame: frame];
    style_label(label, theme::FONT_CAPTION);
    let string = make_nsstring(text);
    let _: () = msg_send![label, setStringValue: string];
    release_obj(string);
    let _: () = msg_send![label, setTextColor: hex_to_ns_color(theme::ui_palette().primary_text)];
    label
}

unsafe fn style_label(label: *mut AnyObject, size: f64) {
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let _: () = msg_send![label, setSelectable: false];
    let _: () = msg_send![label, setUsesSingleLineMode: true];
    let _: () = msg_send![label, setLineBreakMode: 4isize]; // truncating tail
    let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: size];
    let _: () = msg_send![label, setFont: font];
}

fn apply_hover(state: &mut PanelState, hovered: Option<usize>) {
    for (index, card) in state.cards.iter().enumerate() {
        let active = hovered == Some(index);
        unsafe {
            let _: () = msg_send![card.plate, setHidden: !active];
            let _: () = msg_send![card.close, setHidden: !active];
        }
    }
}

unsafe fn set_close_enabled(card: &CardEntry, enabled: bool) {
    let _: () = msg_send![card.close, setHidden: !enabled];
}

fn relayout_after_removal(state: &mut PanelState) {
    let aspects: Vec<Option<f64>> = state.cards.iter().map(|card| card.window.aspect).collect();
    let widths = layout::widths_for_aspects(&aspects);
    let grid = layout::grid_for(&widths, state.geometry.visible.w, state.geometry.visible.h);
    let (w, h) = grid.panel_size();
    let panel = layout::panel_frame(
        state.geometry.anchor,
        (w, h),
        state.geometry.edge,
        state.geometry.visible,
    );
    state.geometry.panel = panel;
    state.geometry.cards = layout::card_rects(&grid);
    state.geometry.band =
        layout::transition_band(state.geometry.edge, state.geometry.anchor, panel);
    state.window_ids = state.cards.iter().map(|card| card.window.wid).collect();
    for (index, card) in state.cards.iter().enumerate() {
        let rect = state.geometry.cards[index];
        unsafe {
            place_view(card.plate, rect);
            let thumb = layout::thumb_rect(rect);
            place_view(card.thumbnail, thumb);
            place_view(card.placeholder, thumb);
            place_placeholder_icon(card.placeholder_icon, thumb);
            place_view(card.header_icon, layout::icon_rect(rect));
            place_view(card.title, layout::title_rect(rect));
            place_view(card.close, layout::close_rect(rect));
        }
    }
    if state
        .hovered
        .is_some_and(|index| index >= state.cards.len())
    {
        state.hovered = None;
    }
    apply_hover(state, state.hovered);
    place_window(state);
    hide_tooltip(state);
}

/// The placeholder's icon stays centred inside its thumbnail box when the card moves.
unsafe fn place_placeholder_icon(icon: *mut AnyObject, thumb: Rect) {
    let size = 48.0;
    let _: () = msg_send![icon, setFrame: NSRect::new(
        NSPoint::new((thumb.w - size) / 2.0, (thumb.h - size) / 2.0),
        NSSize::new(size, size)
    )];
}

unsafe fn place_view(view: *mut AnyObject, rect: Rect) {
    let _: () = msg_send![view, setFrame: NSRect::new(
        NSPoint::new(rect.x, rect.y),
        NSSize::new(rect.w, rect.h)
    )];
}

/// Update the thumbnail views from the shared capture cache, and ask for the missing frames
/// within the in-flight budget.
fn fill_thumbnails(state: &mut PanelState, now: Instant) {
    if state.cards.is_empty() {
        return;
    }
    // No permission, no work: the shared cache and the capture call are both gated, and a panel
    // built for a replaced process instance must not capture its successor's windows.
    let thumbnails_enabled = crate::config::CONFIG
        .read()
        .map(|config| config.layout.thumbnails_enabled)
        .unwrap_or(false);
    if !thumbnails_enabled
        || !crate::thumbnail::capture_allowed()
        || crate::thumbnail::pid_generation_now(state.pid) != state.thumbnail_generation
    {
        return;
    }
    let mut budget = crate::thumbnail::in_flight_count();
    let scale = unsafe {
        let factor: f64 = msg_send![state.panel, backingScaleFactor];
        if factor > 0.0 {
            factor
        } else {
            2.0
        }
    };
    let target_px = crate::thumbnail::target_px_height(layout::THUMB_H, scale);
    for card in state.cards.iter_mut() {
        if card.has_frame || card.window.wid == 0 {
            continue;
        }
        if let Some((pointer, width, height)) =
            crate::thumbnail::lookup_retained(card.window.pid, card.window.wid)
        {
            unsafe {
                let layer: *mut AnyObject = msg_send![card.thumbnail, layer];
                layer_set_contents(layer, pointer as *mut c_void);
                layer_set_contents_scale(layer, scale);
                let _: () = msg_send![card.thumbnail, setHidden: false];
                let _: () = msg_send![card.placeholder, setHidden: true];
            }
            let _ = (width, height);
            unsafe { crate::ffi::CFRelease(pointer) };
            card.has_frame = true;
            continue;
        }
        if now < state.fill_deadline
            && card.window.may_capture
            && card.requested_at.is_none()
            && crate::dock_preview::decisions::may_request_capture(budget, 1)
        {
            if crate::thumbnail::request_frame(
                card.window.pid,
                card.window.wid,
                target_px,
                state.ticket,
                state.thumbnail_generation,
            ) {
                budget += 1;
            }
            // Mark the request either way: a window another producer is already capturing needs no
            // second request, and the tick will pick the frame up from the cache.
            card.requested_at = Some(now);
        }
        if now.duration_since(card.requested_at.unwrap_or(now))
            > std::time::Duration::from_millis(400)
        {
            card.requested_at = None;
        }
        if now >= state.fill_deadline {
            // Keep polling the cache a little longer without issuing new requests.
            card.requested_at = Some(now);
        }
    }
}

fn update_tooltip(state: &mut PanelState, now: Instant) {
    let zone = pointer_zone_now_current_pointer(state);
    let truncated_card = state.hovered.filter(|index| {
        state
            .cards
            .get(*index)
            .is_some_and(|card| card.title_truncated)
    });
    let decision = crate::dock_preview::decisions::tooltip_decision(
        zone,
        truncated_card,
        state.title_hover_since,
        now,
    );
    match decision {
        crate::dock_preview::decisions::TooltipDecision::Show => {
            if state.tooltip_visible {
                return;
            }
            let Some(index) = truncated_card else {
                return;
            };
            let Some(card) = state.cards.get(index) else {
                return;
            };
            let text = card.window.title.clone();
            unsafe {
                show_tooltip_above(state, &text);
            }
            state.tooltip_visible = true;
            state.tooltip_text = text;
            defer_record("dock_preview_tooltip");
        }
        crate::dock_preview::decisions::TooltipDecision::Hide => {
            if state.tooltip_visible {
                hide_tooltip(state);
            }
        }
    }
}

fn pointer_zone_now_current_pointer(state: &PanelState) -> PointerZone {
    let Some(pointer) = state.pointer else {
        return PointerZone::Outside;
    };
    let local = (
        pointer.0 - state.geometry.panel.x,
        pointer.1 - state.geometry.panel.y,
    );
    layout::pointer_zone(
        local,
        Rect::new(0.0, 0.0, state.geometry.panel.w, state.geometry.panel.h),
        &state.geometry.cards,
        Rect::new(
            state.geometry.anchor.x - state.geometry.panel.x,
            state.geometry.anchor.y - state.geometry.panel.y,
            state.geometry.anchor.w,
            state.geometry.anchor.h,
        ),
        Rect::new(
            state.geometry.band.x - state.geometry.panel.x,
            state.geometry.band.y - state.geometry.panel.y,
            state.geometry.band.w,
            state.geometry.band.h,
        ),
        state.hovered,
    )
}

unsafe fn show_tooltip_above(state: &mut PanelState, text: &str) {
    let content: *mut AnyObject = msg_send![state.tooltip_panel, contentView];
    let label = state.tooltip_label;
    let string = make_nsstring(text);
    let _: () = msg_send![label, setStringValue: string];
    release_obj(string);
    let max_text_w = layout::TOOLTIP_MAX_W - layout::TOOLTIP_PAD * 2.0;
    let cell: *mut AnyObject = msg_send![label, cell];
    let measured: NSSize = msg_send![cell, cellSizeForBounds: NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(max_text_w, 10_000.0)
    )];
    let text_w = measured.width.min(max_text_w).max(40.0);
    let text_h = measured.height.max(theme::FONT_CAPTION + 4.0);
    let size = layout::tooltip_size_for((text_w, text_h), 40.0);
    let frame = layout::tooltip_frame(
        state.geometry.panel,
        size,
        state.geometry.edge,
        state.geometry.visible,
        state.geometry.visible,
    );
    let appkit = cg_rect_to_appkit(frame);
    let _: () = msg_send![state.tooltip_panel, setFrame: appkit, display: true];
    let label_frame = NSRect::new(
        NSPoint::new(layout::TOOLTIP_PAD, layout::TOOLTIP_PAD),
        NSSize::new(
            size.0 - layout::TOOLTIP_PAD * 2.0,
            size.1 - layout::TOOLTIP_PAD * 2.0,
        ),
    );
    let _: () = msg_send![label, setFrame: label_frame];
    let _: () = msg_send![content, setFrame: NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(size.0, size.1)
    )];
    let _: () = msg_send![state.tooltip_panel, orderFrontRegardless];
}

fn hide_tooltip(state: &mut PanelState) {
    unsafe {
        let _: () = msg_send![state.tooltip_panel, orderOut: std::ptr::null::<AnyObject>()];
    }
    let was_visible = state.tooltip_visible;
    state.tooltip_visible = false;
    state.tooltip_text.clear();
    if was_visible {
        defer_record("dock_preview_tooltip_hide");
    }
}

// ---------------------------------------------------------------------------------------------
// Dynamic classes and callbacks
// ---------------------------------------------------------------------------------------------

fn panel_class() -> *mut AnyObject {
    static CLASS: OnceLock<crate::ffi::StaticClass> = OnceLock::new();
    crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabDockPreviewPanel", || {
        crate::ffi::build_class(
            c"OhMyTabDockPreviewPanel",
            class!(NSPanel),
            |builder| unsafe {
                builder.add_method::<AnyObject, _>(
                    sel!(canBecomeKeyWindow),
                    panel_can_become_key as unsafe extern "C" fn(*mut AnyObject, Sel) -> Bool,
                );
                builder.add_method::<AnyObject, _>(
                    sel!(canBecomeMainWindow),
                    panel_can_become_key as unsafe extern "C" fn(*mut AnyObject, Sel) -> Bool,
                );
            },
        )
    })
    .0 as *mut AnyObject
}

extern "C" fn panel_can_become_key(_self: *mut AnyObject, _cmd: Sel) -> Bool {
    crate::callback_guard::value("dock_preview_panel_can_become_key", Bool::NO, || Bool::NO)
}

fn container_class() -> *mut AnyObject {
    static CLASS: OnceLock<crate::ffi::StaticClass> = OnceLock::new();
    crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabDockPreviewContainer", || {
        crate::ffi::build_class(
            c"OhMyTabDockPreviewContainer",
            class!(NSView),
            |builder| unsafe {
                builder.add_method::<AnyObject, _>(
                    sel!(isFlipped),
                    container_is_flipped as extern "C" fn(*mut AnyObject, Sel) -> Bool,
                );
                builder.add_method::<AnyObject, _>(
                    sel!(acceptsFirstMouse:),
                    container_accepts_first_mouse
                        as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject) -> Bool,
                );
                builder.add_method::<AnyObject, _>(
                    sel!(mouseDown:),
                    container_mouse_down
                        as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
            },
        )
    })
    .0 as *mut AnyObject
}

extern "C" fn container_is_flipped(_self: *mut AnyObject, _cmd: Sel) -> Bool {
    crate::callback_guard::value("dock_preview_is_flipped", Bool::YES, || Bool::YES)
}

extern "C" fn container_accepts_first_mouse(
    _self: *mut AnyObject,
    _cmd: Sel,
    _event: *mut AnyObject,
) -> Bool {
    crate::callback_guard::value("dock_preview_accepts_first_mouse", Bool::YES, || Bool::YES)
}

extern "C" fn container_mouse_down(_self: *mut AnyObject, _cmd: Sel, event: *mut AnyObject) {
    crate::callback_guard::void("dock_preview_mouse_down", || unsafe {
        let point = local_point(_self, event);
        let zone = {
            let Ok(slot) = PANEL.lock() else {
                return;
            };
            let Some(state) = slot.as_ref() else {
                return;
            };
            layout::pointer_zone(
                point,
                Rect::new(0.0, 0.0, state.geometry.panel.w, state.geometry.panel.h),
                &state.geometry.cards,
                Rect::new(
                    state.geometry.anchor.x - state.geometry.panel.x,
                    state.geometry.anchor.y - state.geometry.panel.y,
                    state.geometry.anchor.w,
                    state.geometry.anchor.h,
                ),
                Rect::new(
                    state.geometry.band.x - state.geometry.panel.x,
                    state.geometry.band.y - state.geometry.panel.y,
                    state.geometry.band.w,
                    state.geometry.band.h,
                ),
                state.hovered,
            )
        };
        match zone {
            PointerZone::Close(index) if !card_is_closing(index) => {
                crate::dock_preview::click_close(index)
            }
            PointerZone::Card(index) | PointerZone::Title(index) if !card_is_closing(index) => {
                crate::dock_preview::click_card(index)
            }
            _ => {}
        }
        flush_records();
    });
}

unsafe fn local_point(view: *mut AnyObject, event: *mut AnyObject) -> (f64, f64) {
    let in_window: NSPoint = msg_send![event, locationInWindow];
    let local: NSPoint =
        msg_send![view, convertPoint: in_window, fromView: std::ptr::null::<AnyObject>()];
    (local.x, local.y)
}

fn passthrough_view_class() -> *mut AnyObject {
    static CLASS: OnceLock<crate::ffi::StaticClass> = OnceLock::new();
    crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabDockPreviewPassthrough", || {
        crate::ffi::build_class(
            c"OhMyTabDockPreviewPassthrough",
            class!(NSView),
            |builder| unsafe {
                builder.add_method::<AnyObject, _>(
                    sel!(hitTest:),
                    passthrough_hit_test
                        as unsafe extern "C" fn(*mut AnyObject, Sel, NSPoint) -> *mut AnyObject,
                );
            },
        )
    })
    .0 as *mut AnyObject
}

fn passthrough_label_class() -> *mut AnyObject {
    static CLASS: OnceLock<crate::ffi::StaticClass> = OnceLock::new();
    crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabDockPreviewLabel", || {
        crate::ffi::build_class(
            c"OhMyTabDockPreviewLabel",
            class!(NSTextField),
            |builder| unsafe {
                builder.add_method::<AnyObject, _>(
                    sel!(hitTest:),
                    passthrough_hit_test
                        as unsafe extern "C" fn(*mut AnyObject, Sel, NSPoint) -> *mut AnyObject,
                );
            },
        )
    })
    .0 as *mut AnyObject
}

extern "C" fn passthrough_hit_test(
    _self: *mut AnyObject,
    _cmd: Sel,
    _point: NSPoint,
) -> *mut AnyObject {
    crate::callback_guard::nil("dock_preview_passthrough_hit_test", std::ptr::null_mut)
}

/// Follow the Dock tile as the Dock magnifies or the pointer moves along it: the same content,
/// a new anchor and (for a side Dock) a new panel edge.
pub(crate) fn update_anchor(anchor: Rect, edge: DockEdge, visible: Rect) {
    let Ok(mut slot) = PANEL.lock() else {
        return;
    };
    let Some(state) = slot.as_mut() else {
        return;
    };
    if state.cards.is_empty() {
        return;
    }
    state.geometry.anchor = anchor;
    state.geometry.edge = edge;
    state.geometry.visible = visible;
    let aspects: Vec<Option<f64>> = state.cards.iter().map(|card| card.window.aspect).collect();
    let widths = layout::widths_for_aspects(&aspects);
    let grid = layout::grid_for(&widths, visible.w, visible.h);
    let (w, h) = grid.panel_size();
    let panel = layout::panel_frame(anchor, (w, h), edge, visible);
    state.geometry.panel = panel;
    state.geometry.cards = layout::card_rects(&grid);
    state.geometry.band = layout::transition_band(edge, anchor, panel);
    // A smaller visible area can fit fewer cards than the panel was built with: unload the tail
    // instead of indexing a shorter rectangle list, and let the published hidden count account for
    // the difference.
    if state.cards.len() > state.geometry.cards.len() {
        while state.cards.len() > state.geometry.cards.len() {
            if let Some(card) = state.cards.pop() {
                remove_card_views(&card);
            }
        }
        state.window_ids = state.cards.iter().map(|card| card.window.wid).collect();
    }
    for (index, card) in state.cards.iter().enumerate() {
        let rect = state.geometry.cards[index];
        unsafe {
            place_view(card.plate, rect);
            place_view(card.thumbnail, layout::thumb_rect(rect));
            place_view(card.placeholder, layout::thumb_rect(rect));
            place_view(card.header_icon, layout::icon_rect(rect));
            place_view(card.title, layout::title_rect(rect));
            place_view(card.close, layout::close_rect(rect));
        }
    }
    place_window(state);
}

pub(crate) fn visible_card_count() -> usize {
    let Ok(slot) = PANEL.lock() else {
        return 0;
    };
    slot.as_ref().map(|state| state.cards.len()).unwrap_or(0)
}

/// Every card shows either a captured frame or its placeholder icon -- never an empty rectangle.
pub(crate) fn cards_are_renderable() -> bool {
    let Ok(slot) = PANEL.lock() else {
        return false;
    };
    let Some(state) = slot.as_ref() else {
        return false;
    };
    !state.cards.is_empty()
        && state.cards.iter().all(|card| unsafe {
            let thumb_hidden: Bool = msg_send![card.thumbnail, isHidden];
            let placeholder_hidden: Bool = msg_send![card.placeholder, isHidden];
            // Exactly one of the two is visible: the frame, or the icon placeholder.
            thumb_hidden != placeholder_hidden
        })
}

/// The pointer zone from the panel's own tracking, for the close timer. Tracking state is the
/// truth here: the hardware cursor query can lag a synthetic move, which would dismiss a panel the
/// pointer is visibly inside.
pub(crate) fn current_pointer_zone() -> PointerZone {
    let Ok(slot) = PANEL.lock() else {
        return PointerZone::Outside;
    };
    slot.as_ref()
        .map(|state| state.last_zone)
        .unwrap_or(PointerZone::Outside)
}

/// The title box of one card in screen top-left coordinates, for the smoke runner's tooltip check.
pub(crate) fn title_rect_for_smoke(index: usize) -> Option<Rect> {
    let slot = PANEL.lock().ok()?;
    let state = slot.as_ref()?;
    let local = *state.geometry.cards.get(index)?;
    let title = layout::title_rect(local);
    Some(Rect::new(
        state.geometry.panel.x + title.x,
        state.geometry.panel.y + title.y,
        title.w,
        title.h,
    ))
}

/// Whether one card's title is truncated (so the tooltip is its only full-text surface).
pub(crate) fn title_is_truncated(index: usize) -> bool {
    let Ok(slot) = PANEL.lock() else {
        return false;
    };
    slot.as_ref()
        .and_then(|state| state.cards.get(index))
        .is_some_and(|card| card.title_truncated)
}

/// Whether the tooltip's own AppKit window is visible (not the cached `tooltip_visible` flag).
pub(crate) fn tooltip_window_visible() -> bool {
    let Ok(slot) = PANEL.lock() else {
        return false;
    };
    let Some(state) = slot.as_ref() else {
        return false;
    };
    let visible: Bool = unsafe { msg_send![state.tooltip_panel, isVisible] };
    visible == Bool::YES
}

/// The tooltip panel's current visibility and text, for the smoke runner.
pub(crate) fn tooltip_state() -> (bool, String) {
    let Ok(slot) = PANEL.lock() else {
        return (false, String::new());
    };
    match slot.as_ref() {
        Some(state) => (state.tooltip_visible, state.tooltip_text.clone()),
        None => (false, String::new()),
    }
}

// ---------------------------------------------------------------------------------------------
// e2e
// ---------------------------------------------------------------------------------------------

/// The panel's facts for `--e2e-state`, or `null`. Frames are top-left screen coordinates so a
/// scenario can click a card without deriving coordinates from the panel rect.
pub(crate) fn e2e_json() -> String {
    let Ok(slot) = PANEL.lock() else {
        return "null".to_string();
    };
    let Some(state) = slot.as_ref() else {
        return "null".to_string();
    };
    if !visible_now(state) {
        return "null".to_string();
    }
    let panel = state.geometry.panel;
    let cards: Vec<String> = state
        .cards
        .iter()
        .enumerate()
        .map(|(index, card)| {
            let rect = state.geometry.cards[index];
            let title = rect_at(panel, layout::title_rect(rect));
            let close = rect_at(panel, layout::close_rect(rect));
            format!(
                "{{\"wid\": {}, \"title\": {}, \"frame\": {}, \"title_rect\": {}, \"close_rect\": {}, \"truncated\": {}, \"has_frame\": {}, \"closing\": {}}}",
                card.window.wid,
                json_string(&card.window.title),
                json_rect(rect_at(panel, rect)),
                json_rect(title),
                json_rect(close),
                card.title_truncated,
                card.has_frame,
                card.closing_since.is_some()
            )
        })
        .collect();
    format!(
        "{{\"visible\": true, \"pid\": {}, \"bundle\": {}, \"edge\": {}, \"panel\": {}, \"cards\": [{}], \"hovered\": {}, \"tooltip_visible\": {}, \"tooltip\": {}, \"total_windows\": {}, \"hidden_windows\": {}}}",
        state.pid,
        json_string(&state.bundle),
        json_string(state.geometry.edge.name()),
        json_rect(panel),
        cards.join(", "),
        match state.hovered {
            Some(index) => format!("{index}"),
            None => "null".to_string(),
        },
        state.tooltip_visible,
        json_string(&state.tooltip_text),
        state.total_windows,
        state.total_windows.saturating_sub(state.cards.len())
    )
}

fn rect_at(panel: Rect, local: Rect) -> Rect {
    Rect::new(panel.x + local.x, panel.y + local.y, local.w, local.h)
}

fn json_rect(rect: Rect) -> String {
    format!(
        "{{\"x\": {:.1}, \"y\": {:.1}, \"w\": {:.1}, \"h\": {:.1}}}",
        rect.x, rect.y, rect.w, rect.h
    )
}

fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
