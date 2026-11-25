//! The Dock preview panel and its tooltip.
//!
//! One borderless, non-activating `NSPanel` is built once and reused: opening it for another app
//! rebuilds its content, and hovering the same app only repositions it. The panel never becomes key,
//! so the app the pointer came from keeps keyboard focus; the container view accepts the first
//! mouse so a click on a row acts immediately instead of being spent activating the window.
//!
//! Two layouts share one view tree shape: a single row of thumbnail cards while the window count is
//! at or under the configured threshold, and a scrolling list of compact rows above it. Both live
//! inside a clipping viewport, inside a content view that is the only thing a scroll moves, so a row
//! never has to be repositioned (and a stale one can never be left behind).
//!
//! Everything the panel owns is touched on the AppKit main thread only. The observer and the window
//! collector hand in plain data; this file turns that data into views.

use std::ffi::c_void;
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

use objc2::runtime::{AnyObject, Bool, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use crate::dock_preview::layout::{
    self, DockEdge, HitGeometry, PanelLayout, PanelMode, PointerZone, Rect, RowAxis, CLOSE_SIZE,
};
use crate::dock_preview::windows::{self, PreviewWindow};
use crate::ffi::{
    hex_to_cg_color, hex_to_ns_color, layer_set_background, layer_set_border, layer_set_contents,
    layer_set_contents_scale, make_nsstring, release_obj, MainThreadSlot,
};
use crate::glass;
use crate::log_debug;
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

/// One thumbnail card of the single-row layout.
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
    /// The × glyph inside `close`; its colour carries the hover emphasis (see `set_close_style`).
    close_cross: *mut AnyObject,
    /// Whether the pointer is on the close button itself (not merely on the card).
    close_hot: bool,
    title_truncated: bool,
    has_frame: bool,
    /// The captured image's pixel dimensions; only its aspect matters for the centred fit.
    image_size: Option<(f64, f64)>,
    requested_at: Option<Instant>,
    closing_since: Option<Instant>,
    /// The close worker is still talking to the target's Accessibility API.
    close_pending: bool,
    last_probe: Instant,
}

/// One compact row of the list layout. Rows live at fixed positions inside the content view;
/// scrolling moves that one view, never a row.
struct RowEntry {
    window: PreviewWindow,
    /// The hover plate: one brightness wash over the whole row, hidden until the row is hovered.
    plate: *mut AnyObject,
    icon: *mut AnyObject,
    title: *mut AnyObject,
    close: *mut AnyObject,
    close_cross: *mut AnyObject,
    close_hot: bool,
    /// Whether the title view exists (a row with no room for a title band has none).
    has_title: bool,
    title_truncated: bool,
    closing_since: Option<Instant>,
    close_pending: bool,
    last_probe: Instant,
}

/// What a visible tooltip was placed against: the item's screen rect, the panel's, and the
/// placement context the frame is derived from.
///
/// The frame depends on the item's HORIZONTAL span (it is centred on the item), on the panel's rect
/// (the vertical reference and the flip decision) and on the edge and visible frame (which side the
/// tooltip goes to and how it is clamped) -- never on the item's own y. Keeping all of it is what
/// lets a list scroll vertically without re-typesetting the tooltip while a panel that moved, a
/// Dock that changed sides or a visible frame that narrowed still moves the window.
#[derive(Clone, Copy, PartialEq)]
struct TooltipAnchor {
    item: Rect,
    panel: Rect,
    edge: DockEdge,
    visible: Rect,
}

impl TooltipAnchor {
    /// Whether a tooltip placed at `self` is already where the current placement puts it.
    fn same_place(self, item: Rect, panel: Rect, edge: DockEdge, visible: Rect) -> bool {
        self.panel == panel
            && self.edge == edge
            && self.visible == visible
            && self.item.x == item.x
            && self.item.w == item.w
    }
}

struct Geometry {
    /// The glass area in screen coordinates (the window frame adds the elevation insets).
    panel: Rect,
    /// The scrolling viewport in panel-local coordinates.
    viewport: Rect,
    /// The rows/cards in content coordinates.
    items: Vec<Rect>,
    /// How far the content view is scrolled.
    scroll: (f64, f64),
    /// Items whose title band is too narrow to draw (the body carries the tooltip).
    body_is_title: Vec<usize>,
    anchor: Rect,
    band: Rect,
    edge: DockEdge,
    visible: Rect,
    mode: PanelMode,
    row_axis: RowAxis,
    max_scroll: f64,
    /// Cards on the widest row of the card layout; 0 for the list.
    columns: usize,
}

/// The surface facts baked into the panel shell at build time: the effective material (Reduce
/// Transparency included), the glass style and the resolved mode. The compensation veil, the glass
/// tint, the frost material, the vibrant appearance and the bevel are all chosen from these three
/// when the hierarchy is built, so a reuse across a change of any of them would keep the old shell
/// behind freshly drawn content.
#[derive(Clone, Copy, PartialEq)]
struct SurfaceTag {
    material: glass::PanelMaterial,
    glass_style: i64,
    dark: bool,
}

fn surface_tag() -> SurfaceTag {
    SurfaceTag {
        material: glass::PanelMaterial::effective(),
        glass_style: glass::glass_style_index(&crate::config::effective_glass_style()),
        dark: theme::resolved_is_dark(),
    }
}

/// Whether the cached shell can serve the next open. The size shapes the view tree, and the
/// surface tag is everything the shell baked in -- a same-size reopen after a material, style or
/// mode change must still rebuild. Pure so the reuse rule is assertable without a panel.
fn shell_reuse_allowed(
    cached_size: (f64, f64),
    cached_surface: SurfaceTag,
    needed: (f64, f64),
    surface: SurfaceTag,
) -> bool {
    cached_size == needed && cached_surface == surface
}

struct PanelState {
    panel: *mut AnyObject,
    content_size: (f64, f64),
    surface: SurfaceTag,
    container: *mut AnyObject,
    /// The clipping viewport: the only boundary that decides what part of a row is drawn or hit.
    viewport: *mut AnyObject,
    /// The content view: every row or card lives here, and a scroll only moves this view.
    content: *mut AnyObject,
    /// The "there is more that way" hints at the viewport's leading and trailing edges.
    fade_lead: *mut AnyObject,
    fade_trail: *mut AnyObject,
    tooltip_panel: *mut AnyObject,
    tooltip_label: *mut AnyObject,
    geometry: Geometry,
    /// The current viewport height, in points (the clip and the layout agree on it).
    request: u64,
    ticket: u64,
    pid: i32,
    thumbnail_generation: u64,
    bundle: String,
    identity_key: String,
    process_start_time_us: Option<u64>,
    mode: PanelMode,
    cards: Vec<CardEntry>,
    rows: Vec<RowEntry>,
    /// The bundle icon every row shares (one NSImage per content build, released with it).
    shared_icon: *mut AnyObject,
    /// Measured title widths, so a rebuild or a scroll never re-measures the same string.
    title_measurements: std::collections::HashMap<String, f64>,
    window_ids: Vec<u32>,
    total_windows: usize,
    hovered: Option<usize>,
    /// Last pointer position in screen top-left coordinates, from the panel's own tracking.
    pointer: Option<(f64, f64)>,
    last_zone: PointerZone,
    title_hover_since: Option<Instant>,
    tooltip_visible: bool,
    tooltip_text: String,
    /// What the visible tooltip was placed against. A scroll can move the item under a resting
    /// pointer, and the tooltip has to move with it.
    tooltip_anchor: Option<TooltipAnchor>,
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
///
/// Returns false when the Dock leaves no operable space at all: the panel is not shown, and the
/// caller keeps its normal "nothing appeared" path. The mode is never changed to make something fit.
pub(crate) fn show(request: PanelRequest, request_epoch: u64, ticket: u64) -> bool {
    // The list is forced by the user's switch or by a missing Screen Recording permission: the
    // cards would be permanent empty placeholders without it, so the panel degrades to the list
    // rather than presenting its loading state as the design.
    let force_list = crate::dock_preview::list_mode() || !crate::thumbnail::capture_allowed();
    let mode = layout::presentation_mode(
        request.windows.len(),
        crate::dock_preview::cards_per_row(),
        force_list,
    );
    let aspects: Vec<Option<f64>> = request.windows.iter().map(|window| window.aspect).collect();
    let Some(layout_out) = layout::layout_for(
        mode,
        &aspects,
        request.windows.len(),
        request.anchor,
        request.edge,
        request.visible,
    ) else {
        log_debug!(
            "[dock-preview] not enough room beside the tile for a {mode:?} panel; not showing"
        );
        return false;
    };
    let needed = (layout_out.panel.w, layout_out.panel.h);
    let Ok(mut slot) = PANEL.lock() else {
        return false;
    };
    // A same-size reopen after a material, glass-style or mode change (a config edit, the system
    // appearance under the auto theme, a Reduce Transparency toggle) must rebuild too, or the new
    // content draws on the previous mode's veil and rim.
    let surface = surface_tag();
    let needs_rebuild = slot.as_ref().is_some_and(|state| {
        !shell_reuse_allowed(state.content_size, state.surface, needed, surface)
    });
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
    // The mode is part of the reuse condition: the same windows can need a different layout after
    // the threshold changed, and a kept view tree would then draw the old one.
    let same = state.pid == request.pid
        && state.mode == layout_out.mode
        && state.window_ids
            == request
                .windows
                .iter()
                .map(|window| window.wid)
                .collect::<Vec<_>>();
    let geometry = geometry_from(&layout_out, request.anchor, request.edge, request.visible);
    if !same {
        rebuild_content(state, &request, geometry);
    } else {
        rebind_geometry(state, &request, geometry);
    }
    state.content_size = needed;
    state.pid = request.pid;
    state.thumbnail_generation = request.thumbnail_generation;
    state.bundle = request.bundle.clone();
    state.identity_key = request.identity_key.clone();
    state.process_start_time_us = request.process_start_time_us;
    state.request = request_epoch;
    state.ticket = ticket;
    // Every open starts at the top, even when the view tree is reused: a kept scroll offset would
    // show the middle of a list the user never scrolled.
    state.geometry.scroll = (0.0, 0.0);
    state.hovered = None;
    state.title_hover_since = None;
    state.pointer = None;
    state.last_zone = PointerZone::Outside;
    hide_tooltip(state);
    place_window(state);
    place_viewport(state);
    let _: () = unsafe { msg_send![state.panel, orderFrontRegardless] };
    let _: () = unsafe { msg_send![state.panel, setAlphaValue: 1.0f64] };
    state.shown_at = Instant::now();
    state.fill_deadline = Instant::now() + crate::dock_preview::decisions::THUMB_FILL_TIMEOUT;
    state.last_fill = Instant::now();
    apply_hover(state, state.hovered);
    true
}

/// Release a panel whose content size no longer matches the next request. The material hierarchy
/// is sized to the panel, so a different content size needs a new hierarchy rather than a resized one.
fn destroy(state: PanelState) {
    unsafe {
        let _: () = msg_send![state.panel, orderOut: std::ptr::null::<AnyObject>()];
        let _: () = msg_send![state.tooltip_panel, orderOut: std::ptr::null::<AnyObject>()];
        release_obj(state.tooltip_panel);
        release_obj(state.panel);
        release_obj(state.viewport);
        release_obj(state.content);
        release_obj(state.fade_lead);
        release_obj(state.fade_trail);
        if !state.shared_icon.is_null() {
            release_obj(state.shared_icon);
        }
    }
}

/// Drop the cached panel so the next open builds it with the current material, glass style and
/// palette. The panel is built once and reused across hovers, and its surface is baked at build
/// time, so a visual change would otherwise never reach it.
pub(crate) fn drop_cached_panel() {
    let Ok(mut slot) = PANEL.lock() else {
        return;
    };
    if let Some(old) = slot.take() {
        destroy(old);
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
    remove_row_views(state, true);
    state.hovered = None;
    state.pointer = None;
    state.last_zone = PointerZone::Outside;
    state.title_hover_since = None;
    state.geometry.scroll = (0.0, 0.0);
    state.window_ids.clear();
    state.total_windows = 0;
    state.request = 0;
}

/// Put the panel back above a window that just came forward.
///
/// The panel is a non-activating panel and never becomes key, but a peek brings another application
/// to the front and its windows are ordered in its own activation: the preview must stay visible
/// over the application it is previewing, or the pointer would be resting on a panel the user
/// cannot see. Ordering it front does not take key or main, so the peeked window keeps the focus it
/// was just given.
pub(crate) fn order_front() {
    let Ok(slot) = PANEL.lock() else {
        return;
    };
    let Some(state) = slot.as_ref() else {
        return;
    };
    let visible: Bool = unsafe { msg_send![state.panel, isVisible] };
    if visible != Bool::YES {
        return;
    }
    unsafe {
        let _: () = msg_send![state.panel, orderFrontRegardless];
        if state.tooltip_visible {
            let _: () = msg_send![state.tooltip_panel, orderFrontRegardless];
        }
    }
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
    if state.mode == PanelMode::Row {
        fill_thumbnails(state, now);
    }
    if confirm_pending_closes(state, now) {
        match relayout_content(state, true) {
            ContentFit::Placed => {}
            ContentFit::Resized(size, scroll) => {
                // The slot has to be released before the rebuild can take it again.
                drop(slot);
                if !apply_resize(size, scroll) {
                    crate::dock_preview::hide_panel();
                }
                return TickOutcome::Idle;
            }
            ContentFit::Gone => {
                hide_tooltip(state);
                return TickOutcome::Empty;
            }
        }
        if state.cards.is_empty() && state.rows.is_empty() {
            hide_tooltip(state);
            return TickOutcome::Empty;
        }
    }
    // Hover and tooltip are driven by the real pointer position read once per tick: AppKit does
    // not promise mouseMoved delivery to a passive, inactive panel, and a panel that only reacts to
    // events would miss a pointer resting inside it.
    state.pointer = Some(pointer);
    let local = (
        pointer.0 - state.geometry.panel.x,
        pointer.1 - state.geometry.panel.y,
    );
    let zone = zone_at(state, local);
    let zone = match zone {
        PointerZone::Card(index) | PointerZone::Title(index) | PointerZone::Close(index)
            if item_is_closing(state, index) =>
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

/// Probe every row whose accepted close is waiting for its window to disappear. Returns whether any
/// row was removed.
fn confirm_pending_closes(state: &mut PanelState, now: Instant) -> bool {
    use crate::dock_preview::decisions::{close_confirmation, CloseConfirmation, TICK_INTERVAL};
    let mut removed = false;
    // A close that did not take puts its item back to "not closing": the button rule is re-derived
    // ONCE for the whole pass, not once per timed-out item.
    let mut restored = false;
    let mut index = 0;
    while index < state.cards.len() {
        let Some(started) = state.cards[index].closing_since else {
            index += 1;
            continue;
        };
        if now.duration_since(state.cards[index].last_probe) < TICK_INTERVAL {
            index += 1;
            continue;
        }
        state.cards[index].last_probe = now;
        let presence = windows::window_presence_fast(
            state.cards[index].window.pid,
            state.cards[index].window.wid,
        );
        match close_confirmation(presence, now.duration_since(started)) {
            CloseConfirmation::Remove => {
                let card = state.cards.remove(index);
                remove_card_views(&card);
                state.total_windows = state.total_windows.saturating_sub(1);
                removed = true;
            }
            CloseConfirmation::Keep => {
                state.cards[index].closing_since = None;
                restored = true;
                index += 1;
            }
            CloseConfirmation::Wait => index += 1,
        }
    }
    let mut index = 0;
    while index < state.rows.len() {
        let Some(started) = state.rows[index].closing_since else {
            index += 1;
            continue;
        };
        if now.duration_since(state.rows[index].last_probe) < TICK_INTERVAL {
            index += 1;
            continue;
        }
        state.rows[index].last_probe = now;
        let presence = windows::window_presence_fast(
            state.rows[index].window.pid,
            state.rows[index].window.wid,
        );
        match close_confirmation(presence, now.duration_since(started)) {
            CloseConfirmation::Remove => {
                let row = state.rows.remove(index);
                remove_single_row(&row);
                state.total_windows = state.total_windows.saturating_sub(1);
                removed = true;
            }
            CloseConfirmation::Keep => {
                state.rows[index].closing_since = None;
                restored = true;
                index += 1;
            }
            CloseConfirmation::Wait => index += 1,
        }
    }
    if restored {
        apply_hover(state, state.hovered);
    }
    removed
}

/// The window behind the item at `index` (used by tests and by the smoke runner).
pub(crate) fn item_window(pid: i32, wid: u32) -> Option<PreviewWindow> {
    let slot = PANEL.lock().ok()?;
    let state = slot.as_ref()?;
    state
        .cards
        .iter()
        .find(|card| card.window.pid == pid && card.window.wid == wid)
        .map(|card| card.window.clone())
        .or_else(|| {
            state
                .rows
                .iter()
                .find(|row| row.window.pid == pid && row.window.wid == wid)
                .map(|row| row.window.clone())
        })
}

/// A close was requested and the worker is about to ask the target's Accessibility API. The item
/// stops taking clicks immediately, but the bounded "wait until the window disappears" clock starts
/// only when the AX call is accepted (`confirm_close_matching`).
///
/// Returns false when the item is already closing, so a second click on the same row in the same
/// generation is refused here rather than reaching the target's Accessibility API twice.
pub(crate) fn begin_close_pending(pid: i32, wid: u32) -> bool {
    let Ok(mut slot) = PANEL.lock() else {
        return false;
    };
    let Some(state) = slot.as_mut() else {
        return false;
    };
    let card = state
        .cards
        .iter()
        .position(|card| card.window.pid == pid && card.window.wid == wid);
    let row = if card.is_none() {
        state
            .rows
            .iter()
            .position(|row| row.window.pid == pid && row.window.wid == wid)
    } else {
        None
    };
    let Some(index) = card.or(row) else {
        return false;
    };
    let closing = state
        .cards
        .get(index)
        .map(|card| card.close_pending || card.closing_since.is_some())
        .or_else(|| {
            state
                .rows
                .get(index)
                .map(|row| row.close_pending || row.closing_since.is_some())
        })
        .unwrap_or(false);
    if closing {
        return false;
    }
    if let Some(card) = state.cards.get_mut(index) {
        card.close_pending = true;
    }
    if let Some(row) = state.rows.get_mut(index) {
        row.close_pending = true;
    }
    set_item_close_visible(state, index, false);
    true
}

/// Navigate by window identity, never by index: rows can disappear while a close result is in
/// flight, so the index that started the close may now name another row.
pub(crate) fn confirm_close_matching(pid: i32, wid: u32) {
    let Ok(mut slot) = PANEL.lock() else {
        return;
    };
    let Some(state) = slot.as_mut() else {
        return;
    };
    let now = Instant::now();
    let started_at = now - crate::dock_preview::decisions::TICK_INTERVAL;
    if let Some(index) = state
        .cards
        .iter()
        .position(|card| card.window.pid == pid && card.window.wid == wid)
    {
        let card = &mut state.cards[index];
        card.close_pending = false;
        card.closing_since = Some(now);
        card.last_probe = started_at;
        set_item_close_visible(state, index, false);
        return;
    }
    if let Some(index) = state
        .rows
        .iter()
        .position(|row| row.window.pid == pid && row.window.wid == wid)
    {
        let row = &mut state.rows[index];
        row.close_pending = false;
        row.closing_since = Some(now);
        row.last_probe = started_at;
        set_item_close_visible(state, index, false);
    }
}

/// The AX close was refused or failed: put the identified item back into its normal state.
pub(crate) fn cancel_close_matching(pid: i32, wid: u32) {
    let Ok(mut slot) = PANEL.lock() else {
        return;
    };
    let Some(state) = slot.as_mut() else {
        return;
    };
    let mut item = None;
    if let Some(index) = state
        .cards
        .iter()
        .position(|card| card.window.pid == pid && card.window.wid == wid)
    {
        state.cards[index].closing_since = None;
        state.cards[index].close_pending = false;
        item = Some(index);
    } else if let Some(index) = state
        .rows
        .iter()
        .position(|row| row.window.pid == pid && row.window.wid == wid)
    {
        state.rows[index].closing_since = None;
        state.rows[index].close_pending = false;
        item = Some(index);
    }
    if let Some(index) = item {
        // Back to normal: the button follows the hover again, it is not shown unconditionally.
        let hovered = state.hovered == Some(index);
        let _ = hovered;
        apply_hover(state, state.hovered);
    }
}

/// The pointer hover state, recomputed from the real pointer once per tick and after every scroll.
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
    // The row's hover does not paint its close button; the button's own rectangle does.
    let hot = match zone {
        PointerZone::Close(index) => Some(index),
        _ => None,
    };
    for index in 0..state.cards.len() {
        let want = hot == Some(index);
        if state.cards[index].close_hot != want {
            state.cards[index].close_hot = want;
            set_close_style(&state.cards[index], want);
        }
    }
    for index in 0..state.rows.len() {
        let want = hot == Some(index);
        if state.rows[index].close_hot != want {
            state.rows[index].close_hot = want;
            set_row_close_style(&state.rows[index], want);
        }
    }
    // A row whose title band is too narrow carries the tooltip on its whole body; either way the
    // timer only starts once the pointer rests on that row's text area.
    let truncated = match (zone, hovered) {
        (PointerZone::Title(_) | PointerZone::Card(_), Some(index)) => item_title(state, index)
            .map(|(_, truncated)| truncated)
            .unwrap_or(false),
        _ => false,
    };
    if truncated {
        if state.title_hover_since.is_none() {
            state.title_hover_since = Some(Instant::now());
        }
    } else {
        state.title_hover_since = None;
    }
}

fn visible_now(state: &PanelState) -> bool {
    let visible: Bool = unsafe { msg_send![state.panel, isVisible] };
    visible == Bool::YES
}

// ---------------------------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------------------------

/// Build the panel's geometry from a [`PanelLayout`]. The layout function decides the mode, the
/// panel size, the viewport and the scroll range; this turns them into the shapes the views and the
/// hit test read.
fn geometry_from(
    layout_out: &PanelLayout,
    anchor: Rect,
    edge: DockEdge,
    visible: Rect,
) -> Geometry {
    let (items, body_is_title) = match layout_out.mode {
        PanelMode::Row => {
            let row = layout_out
                .row
                .as_ref()
                .expect("a row layout carries its cards");
            let hidden = row
                .cards
                .iter()
                .enumerate()
                .filter(|(_, card)| layout::row_boxes(card.w).title_hidden)
                .map(|(index, _)| index)
                .collect();
            (row.cards.clone(), hidden)
        }
        PanelMode::List => {
            let list = layout_out
                .list
                .as_ref()
                .expect("a list layout carries its rows");
            let count = ((list.content_h / layout::ROW_H).round() as usize).max(1);
            let boxes = layout::row_boxes(list.content_w);
            let hidden = if boxes.title_hidden {
                (0..count).collect()
            } else {
                Vec::new()
            };
            let items = (0..count)
                .map(|index| Rect::new(0.0, layout::row_top(index), list.content_w, layout::ROW_H))
                .collect();
            (items, hidden)
        }
    };
    let row_axis = layout_out
        .row
        .as_ref()
        .map(|row| row.axis)
        .unwrap_or(RowAxis::None);
    let max_scroll = layout_out
        .list
        .as_ref()
        .map(|list| list.max_scroll)
        .or_else(|| {
            layout_out.row.as_ref().and_then(|row| match row.axis {
                RowAxis::Horizontal => Some((row.content_w - layout_out.viewport.w).max(0.0)),
                RowAxis::Vertical => Some((row.content_h - layout_out.viewport.h).max(0.0)),
                RowAxis::None => None,
            })
        })
        .unwrap_or(0.0);
    let columns = layout_out.row.as_ref().map(|row| row.columns).unwrap_or(0);
    Geometry {
        columns,
        band: layout::transition_band(edge, anchor, layout_out.panel),
        panel: layout_out.panel,
        viewport: layout_out.viewport,
        items,
        scroll: (0.0, 0.0),
        body_is_title,
        anchor,
        edge,
        visible,
        mode: layout_out.mode,
        row_axis,
        max_scroll,
    }
}

/// The pointer zone for one panel-local point, with the panel's current scroll applied.
fn zone_at(state: &PanelState, local: (f64, f64)) -> PointerZone {
    layout::pointer_zone(
        local,
        HitGeometry {
            panel: Rect::new(0.0, 0.0, state.geometry.panel.w, state.geometry.panel.h),
            viewport: state.geometry.viewport,
            scroll: state.geometry.scroll,
            mode: state.mode,
            items: &state.geometry.items,
            anchor: Rect::new(
                state.geometry.anchor.x - state.geometry.panel.x,
                state.geometry.anchor.y - state.geometry.panel.y,
                state.geometry.anchor.w,
                state.geometry.anchor.h,
            ),
            band: Rect::new(
                state.geometry.band.x - state.geometry.panel.x,
                state.geometry.band.y - state.geometry.panel.y,
                state.geometry.band.w,
                state.geometry.band.h,
            ),
            close_visible: state.hovered,
            body_is_title: &state.geometry.body_is_title,
        },
    )
}

/// Whether an item is waiting for a close (its AX call is in flight or its window is disappearing).
fn item_is_closing(state: &PanelState, index: usize) -> bool {
    let card = state
        .cards
        .get(index)
        .is_some_and(|card| card.closing_since.is_some() || card.close_pending);
    let row = state
        .rows
        .get(index)
        .is_some_and(|row| row.closing_since.is_some() || row.close_pending);
    card || row
}

/// A target resolved once, under the panel lock, so the work that follows never re-derives what
/// was clicked from an index that a scroll or a removal may have moved.
#[derive(Clone, Debug)]
pub(crate) struct HitTarget {
    pub(crate) pid: i32,
    pub(crate) wid: u32,
    pub(crate) request: u64,
    pub(crate) kind: HitKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HitKind {
    Activate,
    Close,
}

/// Resolve the click at `local` into a target, or `None` when it hits nothing actionable.
///
/// The closing state is judged here, in the same read that resolves the target: a row whose window
/// is already being closed takes no click, whether the click lands on its body or its close slot.
pub(crate) fn hit_target(local: (f64, f64)) -> Option<HitTarget> {
    let slot = PANEL.lock().ok()?;
    let state = slot.as_ref()?;
    let zone = zone_at(state, local);
    let (index, kind) = match zone {
        PointerZone::Close(index) => (index, HitKind::Close),
        PointerZone::Card(index) | PointerZone::Title(index) => (index, HitKind::Activate),
        _ => return None,
    };
    if item_is_closing(state, index) {
        return None;
    }
    let window = state
        .cards
        .get(index)
        .map(|card| card.window.clone())
        .or_else(|| state.rows.get(index).map(|row| row.window.clone()))?;
    Some(HitTarget {
        pid: window.pid,
        wid: window.wid,
        request: state.request,
        kind,
    })
}

/// One window's identity across a rebuild.
type WindowKey = (i32, u32);
/// A window's close state across a rebuild: when the close was accepted, whether it is in flight,
/// and when the probe last looked.
type CloseState = (Option<Instant>, bool, Instant);

/// Rebuild the panel at a new glass size, keeping its windows, its mode and its (clamped) scroll.
///
/// The old state is taken out of the slot first and returned as the caller's `state` is gone: the
/// build itself needs the slot, and two mutable borrows of it cannot exist at once.
pub(crate) fn apply_resize(size: (f64, f64), scroll: (f64, f64)) -> bool {
    let Ok(mut slot) = PANEL.lock() else {
        return false;
    };
    let Some(old) = slot.take() else {
        return false;
    };
    let windows: Vec<PreviewWindow> = old
        .cards
        .iter()
        .map(|card| card.window.clone())
        .chain(old.rows.iter().map(|row| row.window.clone()))
        .collect();
    let mode = old.mode;
    let anchor = old.geometry.anchor;
    let edge = old.geometry.edge;
    let visible = old.geometry.visible;
    let request = PanelRequest {
        pid: old.pid,
        thumbnail_generation: old.thumbnail_generation,
        bundle: old.bundle.clone(),
        identity_key: old.identity_key.clone(),
        process_start_time_us: old.process_start_time_us,
        windows,
        anchor,
        edge,
        visible,
    };
    let ticket = old.ticket;
    let epoch = old.request;
    // A rebuild replaces every view, but not the close state: a window whose close is already in
    // flight must keep that state across the rebuild, or it becomes clickable again and the probe
    // that waits for it to disappear stops.
    let close_states: Vec<(WindowKey, CloseState)> = old
        .cards
        .iter()
        .map(|card| {
            (
                (card.window.pid, card.window.wid),
                (card.closing_since, card.close_pending, card.last_probe),
            )
        })
        .chain(old.rows.iter().map(|row| {
            (
                (row.window.pid, row.window.wid),
                (row.closing_since, row.close_pending, row.last_probe),
            )
        }))
        .collect();
    // The old card/row views belong to the material that is about to be released.
    destroy(old);
    let Some(mut state) = build(size.0, size.1) else {
        return false;
    };
    state.mode = mode;
    state.pid = request.pid;
    state.thumbnail_generation = request.thumbnail_generation;
    state.bundle = request.bundle.clone();
    state.identity_key = request.identity_key.clone();
    state.process_start_time_us = request.process_start_time_us;
    state.request = epoch;
    state.ticket = ticket;
    state.content_size = size;
    let aspects: Vec<Option<f64>> = request.windows.iter().map(|window| window.aspect).collect();
    let Some(layout_out) =
        layout::layout_for(mode, &aspects, request.windows.len(), anchor, edge, visible)
    else {
        destroy(state);
        return false;
    };
    let mut geometry = geometry_from(&layout_out, anchor, edge, visible);
    geometry.scroll = (
        scroll.0.min(geometry.max_scroll),
        scroll.1.min(geometry.max_scroll),
    );
    state.geometry = geometry;
    state.total_windows = request.windows.len();
    state.window_ids = request.windows.iter().map(|window| window.wid).collect();
    let geometry = std::mem::replace(
        &mut state.geometry,
        Geometry {
            panel: Rect::new(0.0, 0.0, 1.0, 1.0),
            viewport: Rect::new(0.0, 0.0, 1.0, 1.0),
            items: Vec::new(),
            scroll: (0.0, 0.0),
            body_is_title: Vec::new(),
            anchor,
            band: Rect::new(0.0, 0.0, 0.0, 0.0),
            edge,
            visible,
            mode,
            row_axis: RowAxis::None,
            max_scroll: 0.0,
            columns: 1,
        },
    );
    rebuild_content(&mut state, &request, geometry);
    for (key, (closing_since, close_pending, last_probe)) in close_states {
        for card in state
            .cards
            .iter_mut()
            .filter(|card| card.window.pid == key.0 && card.window.wid == key.1)
        {
            card.closing_since = closing_since;
            card.close_pending = close_pending;
            card.last_probe = last_probe;
            if close_pending || closing_since.is_some() {
                unsafe { set_close_enabled(card, false) };
            }
        }
        for row in state
            .rows
            .iter_mut()
            .filter(|row| row.window.pid == key.0 && row.window.wid == key.1)
        {
            row.closing_since = closing_since;
            row.close_pending = close_pending;
            row.last_probe = last_probe;
            if close_pending || closing_since.is_some() {
                unsafe { set_close_enabled(row, false) };
            }
        }
    }
    place_window(&mut state);
    let _: () = unsafe { msg_send![state.panel, orderFrontRegardless] };
    state.fill_deadline = Instant::now() + crate::dock_preview::decisions::THUMB_FILL_TIMEOUT;
    state.last_fill = Instant::now();
    *slot = Some(state);
    true
}

/// Apply a scroll delta to the panel's own viewport and move the content view with it.
pub(crate) fn scroll_by(axis: layout::Axis, delta: f64) {
    let Ok(mut slot) = PANEL.lock() else {
        return;
    };
    let Some(state) = slot.as_mut() else {
        return;
    };
    let current = match axis {
        layout::Axis::X => state.geometry.scroll.0,
        layout::Axis::Y => state.geometry.scroll.1,
    };
    let next = layout::scroll_offset(current, delta, state.geometry.max_scroll);
    if (next - current).abs() < f64::EPSILON {
        return;
    }
    match axis {
        layout::Axis::X => state.geometry.scroll.0 = next,
        layout::Axis::Y => state.geometry.scroll.1 = next,
    }
    apply_content_origin(state);
    // The hints follow the offset: the edge the content just came from may have nothing beyond it
    // any more, and the one it heads towards may have just gained something.
    let viewport = state.geometry.viewport;
    unsafe { install_edge_fades(state, viewport) };
    reclassify(state);
    defer_record("dock_preview_scroll");
}

/// Move the content view so the scrolled region lines up with the viewport.
fn apply_content_origin(state: &PanelState) {
    let origin = layout::content_origin(state.geometry.scroll);
    unsafe {
        let _: () = msg_send![state.content, setFrameOrigin: NSPoint::new(origin.0, origin.1)];
    }
}

/// Recompute the hover, the close emphasis and the tooltip from the pointer the panel last saw.
///
/// Every geometry change (a scroll, a removal, an anchor move) ends here, so a click that follows
/// it arrives at the same answer the drawing did.
fn reclassify(state: &mut PanelState) {
    let Some(pointer) = state.pointer else {
        return;
    };
    let local = (
        pointer.0 - state.geometry.panel.x,
        pointer.1 - state.geometry.panel.y,
    );
    let zone = zone_at(state, local);
    state.last_zone = zone;
    update_hover(state, zone);
    update_tooltip(state, Instant::now());
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

/// Size the viewport view, size the content view from the scroll range, place the content, and
/// refresh the scroll hints (they depend on both the geometry and the offset).
fn place_viewport(state: &mut PanelState) {
    let viewport = state.geometry.viewport;
    let (content_w, content_h) = content_extent(state);
    unsafe {
        place_view(state.viewport, viewport);
        let layer: *mut AnyObject = msg_send![state.viewport, layer];
        let _: () =
            msg_send![layer, setCornerRadius: (layout::PANEL_RADIUS - layout::PANEL_PAD).max(0.0)];
        // Clip only when the content can scroll: the clip is what scrolling needs, but it also
        // cuts the hover plate at the viewport edge, which is what left the padding ring around a
        // hovered card unhighlighted. With nowhere to scroll the content never moves, so the
        // plate may draw its one-padding-step overhang out to the glass edge.
        let clips = state.geometry.max_scroll > 0.0;
        let _: () = msg_send![layer, setMasksToBounds: clips];
        // The content is a CHILD of the viewport: its origin is the scroll offset inside it, not a
        // position in the panel. Adding the viewport origin here shifted every row by the padding.
        let origin = layout::content_origin(state.geometry.scroll);
        let _: () = msg_send![state.content, setFrame: NSRect::new(
            NSPoint::new(origin.0, origin.1),
            NSSize::new(content_w, content_h)
        )];
        install_edge_fades(state, viewport);
    }
    apply_content_origin(state);
}

/// Place the "more content this way" hints at the viewport's edges.
///
/// The two fade views are created once with the panel and only shown, hidden and moved afterwards:
/// building them per re-layout appended a new layer every time and never removed the old ones. The
/// hints follow the scroll position, so a scroll refreshes them too.
unsafe fn install_edge_fades(state: &mut PanelState, viewport: Rect) {
    let scroll = state.geometry.scroll.0.max(state.geometry.scroll.1);
    let (lead, trail) = layout::edge_fades(scroll, state.geometry.max_scroll);
    let horizontal = state.geometry.row_axis == RowAxis::Horizontal;
    let size = if horizontal {
        NSSize::new(layout::LIST_FADE_H, viewport.h)
    } else {
        NSSize::new(viewport.w, layout::LIST_FADE_H)
    };
    let palette = theme::ui_palette();
    let fade = palette.window_bg & 0xFFFF_FF00;
    for (leading, visible) in [(true, lead), (false, trail)] {
        let view = if leading {
            state.fade_lead
        } else {
            state.fade_trail
        };
        if view.is_null() {
            continue;
        }
        let _: () = msg_send![view, setHidden: !visible];
        if !visible {
            continue;
        }
        let origin = if horizontal {
            let x = if leading {
                viewport.x
            } else {
                viewport.max_x() - layout::LIST_FADE_H
            };
            NSPoint::new(x, viewport.y)
        } else {
            let y = if leading {
                viewport.y
            } else {
                viewport.max_y() - layout::LIST_FADE_H
            };
            NSPoint::new(viewport.x, y)
        };
        let _: () = msg_send![view, setFrame: NSRect::new(origin, size)];
        let gradient: *mut AnyObject = msg_send![view, layer];
        let sublayers: *mut AnyObject = msg_send![gradient, sublayers];
        let count: usize = if sublayers.is_null() {
            0
        } else {
            msg_send![sublayers, count]
        };
        if count == 0 {
            continue;
        }
        let first: *mut AnyObject = msg_send![sublayers, objectAtIndex: 0usize];
        let _: () = msg_send![first, setFrame: NSRect::new(NSPoint::new(0.0, 0.0), size)];
        let start = if horizontal {
            NSPoint::new(if leading { 0.0 } else { 1.0 }, 0.5)
        } else {
            NSPoint::new(0.5, if leading { 0.0 } else { 1.0 })
        };
        let end = if horizontal {
            NSPoint::new(if leading { 1.0 } else { 0.0 }, 0.5)
        } else {
            NSPoint::new(0.5, if leading { 1.0 } else { 0.0 })
        };
        let _: () = msg_send![first, setStartPoint: start];
        let _: () = msg_send![first, setEndPoint: end];
        let _ = fade;
    }
}

/// One fade view: a passthrough view holding a gradient layer, hidden until a scrolled edge needs
/// it. The passthrough class makes it transparent to hit testing and to the wheel.
unsafe fn build_fade_view() -> *mut AnyObject {
    let view: *mut AnyObject = msg_send![passthrough_view_class(), alloc];
    let view: *mut AnyObject = msg_send![view, initWithFrame: NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(1.0, 1.0)
    )];
    let _: () = msg_send![view, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![view, layer];
    let gradient: *mut AnyObject = msg_send![class!(CAGradientLayer), layer];
    let _: () = msg_send![layer, addSublayer: gradient];
    let palette = theme::ui_palette();
    let fade = palette.window_bg & 0xFFFF_FF00;
    let cg_colors = [hex_to_cg_color(fade | 0x8C), hex_to_cg_color(fade)];
    let array = crate::ffi::CFArrayCreate(
        std::ptr::null(),
        cg_colors.as_ptr() as *const *const c_void,
        cg_colors.len() as isize,
        &crate::ffi::kCFTypeArrayCallBacks as *const c_void,
    );
    if !array.is_null() {
        let _: () = msg_send![gradient, setColors: array as *mut AnyObject];
        crate::ffi::CFRelease(array);
    }
    let _: () = msg_send![view, setHidden: true];
    view
}

/// The content view's full extent: wider than the viewport exactly when the row scrolls sideways.
fn content_extent(state: &PanelState) -> (f64, f64) {
    let viewport = state.geometry.viewport;
    let w = match state.geometry.row_axis {
        RowAxis::Horizontal => viewport.w + state.geometry.max_scroll,
        RowAxis::None | RowAxis::Vertical => viewport.w,
    };
    let h = match state.geometry.row_axis {
        RowAxis::Vertical => viewport.h + state.geometry.max_scroll,
        _ => state
            .geometry
            .items
            .last()
            .map(|item| item.max_y())
            .unwrap_or(viewport.h)
            .max(viewport.h),
    };
    (w, h)
}

/// Whether the current content has no place to be drawn any more (the space under it went away).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ContentFit {
    Placed,
    /// The content needs a different glass size: the caller rebuilds the panel, keeping the
    /// windows, the mode and the scroll offset it was given.
    Resized((f64, f64), (f64, f64)),
    /// The layout no longer exists: the caller hides the panel.
    Gone,
}

/// Recompute the layout from the CURRENT items (after a removal, a window change or an anchor
/// move), keep the scroll position it can still honour, and re-place everything.
///
/// This is the only re-layout entry: a removal that only moved frames would leave the content
/// height and the scroll range describing rows that are gone, and a user could scroll into them.
///
/// `items_changed` says whether the ITEMS are different (a removal re-indexed them) rather than
/// the panel having only moved: an anchor move must keep a tooltip that still names the title under
/// the pointer, while a removal must drop it (and its rest timer, which would otherwise hand the
/// new item at that index a tooltip at once).
fn relayout_content(state: &mut PanelState, items_changed: bool) -> ContentFit {
    let count = match state.mode {
        PanelMode::List => state.rows.len(),
        PanelMode::Row => state.cards.len(),
    };
    if count == 0 {
        return ContentFit::Gone;
    }
    let aspects: Vec<Option<f64>> = match state.mode {
        PanelMode::List => Vec::new(),
        PanelMode::Row => state.cards.iter().map(|card| card.window.aspect).collect(),
    };
    let anchor = state.geometry.anchor;
    let edge = state.geometry.edge;
    let visible = state.geometry.visible;
    let Some(layout_out) = layout::layout_for(state.mode, &aspects, count, anchor, edge, visible)
    else {
        return ContentFit::Gone;
    };
    let scroll = state.geometry.scroll;
    if (layout_out.panel.w, layout_out.panel.h) != state.content_size {
        // The glass changed size, and only a new material hierarchy can show that: the caller
        // rebuilds the panel at the new size instead of letting the preview disappear.
        return ContentFit::Resized((layout_out.panel.w, layout_out.panel.h), scroll);
    }
    let mut geometry = geometry_from(&layout_out, anchor, edge, visible);
    // The offset survives the change, clamped to what the new content can still show.
    geometry.scroll = (
        scroll.0.min(geometry.max_scroll),
        scroll.1.min(geometry.max_scroll),
    );
    state.geometry = geometry;
    // The window id list belongs to the panel's content, not to the request that built it.
    state.window_ids = state
        .cards
        .iter()
        .map(|card| card.window.wid)
        .chain(state.rows.iter().map(|row| row.window.wid))
        .collect();
    place_window(state);
    place_viewport(state);
    if state.mode == PanelMode::List {
        place_rows(state);
    } else {
        place_cards(state);
    }
    if state
        .hovered
        .is_some_and(|index| index >= state.geometry.items.len())
    {
        state.hovered = None;
    }
    apply_hover(state, state.hovered);
    if items_changed {
        // A removal re-indexes the items: the tooltip may now name a window that is no longer
        // here, and the rest that started on the old item must not count towards whatever now
        // sits under the pointer. The reclassification below re-arms the timer from scratch.
        hide_tooltip(state);
        state.title_hover_since = None;
    }
    reclassify(state);
    ContentFit::Placed
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

/// The preview never becomes key, so it shares the keystroke HUD's passive-panel recipe: the same
/// compensation veil (transparent under clear glass), the native glass edge and the high elevation.
/// What stays its own is the 8pt corner radius -- the deliberately squarish taskbar-preview shape.
fn backdrop_options() -> glass::BackdropOptions {
    let tint = glass::passive_glass_compensation_tint(
        &crate::config::effective_glass_style(),
        theme::resolved_is_dark(),
    );
    glass::BackdropOptions::new(Some(tint & 0xFF))
        .with_compensation_tint(tint)
        .with_elevation(Some(theme::ELEVATION_HIGH))
        .with_native_glass_edge()
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
        // Vibrant, not plain: the labels are drawn on the user's material, so under frost they use
        // the system's dynamic label colors and AppKit resolves them against the live backdrop
        // (design-style §3, "the ink adapts"). Liquid Glass reads its absolute tokens either way.
        let appearance_name =
            make_nsstring(glass::vibrant_appearance_name(theme::resolved_is_dark()));
        let appearance: *mut AnyObject =
            msg_send![class!(NSAppearance), appearanceNamed: appearance_name];
        crate::ffi::CFRelease(appearance_name as *const c_void);
        if !appearance.is_null() {
            let _: () = msg_send![panel, setAppearance: appearance];
        }
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
        // The HUD's bevel, not the active panels': a passive panel's native specular rim is dimmed
        // with the rest of the glass, so clear mode needs the stronger drawn rim to read as glass
        // at all. Inert on frost and opaque (the draw returns early off Liquid Glass). The view
        // hierarchy owns it, so nothing here has to keep the pointer.
        let _ = glass::install_hud_bevel(&backdrop, layout::PANEL_RADIUS);
        let container: *mut AnyObject = msg_send![container_class(), alloc];
        let container: *mut AnyObject = msg_send![container, initWithFrame: NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(panel_w, panel_h)
        )];
        let _: () = msg_send![container, setWantsLayer: true];
        let _: () = msg_send![backdrop.content_parent, addSubview: container];
        // The viewport clips (it is the only boundary a row can be cut by), and the content view
        // inside it is what a scroll moves -- rows keep their own positions for their whole life.
        let viewport: *mut AnyObject = msg_send![flipped_passive_view_class(), alloc];
        let viewport: *mut AnyObject = msg_send![viewport, initWithFrame: NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(1.0, 1.0)
        )];
        let _: () = msg_send![viewport, setWantsLayer: true];
        let viewport_layer: *mut AnyObject = msg_send![viewport, layer];
        let _: () = msg_send![viewport_layer, setMasksToBounds: true];
        let _: () = msg_send![container, addSubview: viewport];
        let content: *mut AnyObject = msg_send![flipped_passive_view_class(), alloc];
        let content: *mut AnyObject = msg_send![content, initWithFrame: NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(1.0, 1.0)
        )];
        let _: () = msg_send![content, setWantsLayer: true];
        let _: () = msg_send![viewport, addSubview: content];
        release_obj(container);
        release_obj(host);

        // The scroll hints are siblings of the viewport (the clip would cut them at its own edge),
        // created once here and only shown, hidden and moved afterwards.
        let fade_lead = build_fade_view();
        let fade_trail = build_fade_view();
        let _: () = msg_send![container, addSubview: fade_lead];
        let _: () = msg_send![container, addSubview: fade_trail];

        let tooltip_panel = build_tooltip_panel();

        Some(PanelState {
            panel,
            content_size: (panel_w, panel_h),
            surface: surface_tag(),
            container,
            viewport,
            content,
            fade_lead,
            fade_trail,
            tooltip_panel: tooltip_panel.0,
            tooltip_label: tooltip_panel.1,
            geometry: Geometry {
                panel: Rect::new(0.0, 0.0, 1.0, 1.0),
                viewport: Rect::new(0.0, 0.0, 1.0, 1.0),
                items: Vec::new(),
                scroll: (0.0, 0.0),
                body_is_title: Vec::new(),
                anchor: Rect::new(0.0, 0.0, 1.0, 1.0),
                band: Rect::new(0.0, 0.0, 0.0, 0.0),
                edge: DockEdge::Bottom,
                visible: Rect::new(0.0, 0.0, 1.0, 1.0),
                mode: PanelMode::Row,
                row_axis: RowAxis::None,
                max_scroll: 0.0,
                columns: 1,
            },
            request: 0,
            ticket: 0,
            pid: 0,
            thumbnail_generation: 0,
            bundle: String::new(),
            identity_key: String::new(),
            process_start_time_us: None,
            mode: PanelMode::Row,
            cards: Vec::new(),
            rows: Vec::new(),
            shared_icon: std::ptr::null_mut(),
            title_measurements: std::collections::HashMap::new(),
            window_ids: Vec::new(),
            total_windows: 0,
            hovered: None,
            pointer: None,
            last_zone: PointerZone::Outside,
            title_hover_since: None,
            tooltip_visible: false,
            tooltip_text: String::new(),
            tooltip_anchor: None,
            fill_deadline: Instant::now(),
            last_fill: Instant::now(),
            shown_at: Instant::now(),
        })
    }
}

/// The tooltip is a passive panel: it never accepts a click and never becomes key.
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
// Content: rows and cards
// ---------------------------------------------------------------------------------------------

/// Build the whole content for `request`. The mode was decided by `show`, so this only materialises
/// the view kind that mode uses.
fn rebuild_content(state: &mut PanelState, request: &PanelRequest, geometry: Geometry) {
    unsafe {
        for card in state.cards.drain(..) {
            remove_card_views(&card);
        }
        remove_row_views(state, true);
        state.mode = geometry.mode;
        state.geometry = geometry;
        state.window_ids = request.windows.iter().map(|window| window.wid).collect();
        state.total_windows = request.windows.len();
        state.title_measurements.clear();
        match state.mode {
            PanelMode::Row => build_cards(state, request),
            PanelMode::List => build_rows(state, request),
        }
        place_viewport(state);
        if state.mode == PanelMode::List {
            place_rows(state);
        } else {
            place_cards(state);
        }
        state.hovered = None;
        state.title_hover_since = None;
    }
}

/// Keep the existing view tree but take the new geometry, and refresh what the request may have
/// changed without changing the window identities (titles, aspects, window state).
fn rebind_geometry(state: &mut PanelState, request: &PanelRequest, geometry: Geometry) {
    let mode = geometry.mode;
    state.mode = mode;
    state.geometry = geometry;
    state.total_windows = request.windows.len();
    state.window_ids = request.windows.iter().map(|window| window.wid).collect();
    if mode == PanelMode::Row {
        for (index, window) in request.windows.iter().enumerate() {
            let Some(card) = state.cards.get_mut(index) else {
                break;
            };
            unsafe { rebind_card(card, window, &mut state.title_measurements) };
        }
    } else {
        for (index, window) in request.windows.iter().enumerate() {
            let Some(row) = state.rows.get_mut(index) else {
                break;
            };
            unsafe { rebind_row(row, window, &mut state.title_measurements) };
        }
    }
    place_viewport(state);
    if mode == PanelMode::List {
        place_rows(state);
    } else {
        place_cards(state);
    }
}

/// The bundle icon every row shares, built once per content build.
unsafe fn shared_row_icon(state: &mut PanelState, bundle: &str) {
    if !state.shared_icon.is_null() {
        release_obj(state.shared_icon);
        state.shared_icon = std::ptr::null_mut();
    }
    state.shared_icon = crate::app_identity::icon_for_bundle_id(bundle, layout::ROW_ICON)
        .unwrap_or(std::ptr::null_mut());
}

/// The measured width of `text`, measured once per content build.
unsafe fn measured_title(state: &mut PanelState, text: &str) -> f64 {
    if let Some(width) = state.title_measurements.get(text) {
        return *width;
    }
    let width = glass::measure_single_line_text(text, theme::FONT_CONTROL).width;
    state.title_measurements.insert(text.to_string(), width);
    width
}

unsafe fn build_cards(state: &mut PanelState, request: &PanelRequest) {
    let rects: Vec<Rect> = state.geometry.items.clone();
    let icon = crate::app_identity::icon_for_bundle_id(&request.bundle, layout::CARD_ICON);
    let panel = (state.geometry.panel.w, state.geometry.panel.h);
    let plates = layout::plates_for_grid(&rects, state.geometry.columns.max(1), panel);
    for (index, rect) in rects.iter().enumerate() {
        let Some(window) = request.windows.get(index) else {
            break;
        };
        let (plate_rect, plate_radius) = plates
            .get(index)
            .copied()
            .unwrap_or((*rect, layout::PANEL_RADIUS));
        // The cards themselves do not paint: the panel's material is the surface between and
        // inside them (the switcher's own rule). A hovered card draws a brightness-only plate
        // that follows the tile out to the panel edge (see `layout::hover_plate_rect`).
        let plate = passthrough_view(plate_rect);
        let plate_layer: *mut AnyObject = msg_send![plate, layer];
        layer_set_background(plate_layer, hex_to_cg_color(layout::HOVER_PLATE_FILL));
        let _: () = msg_send![plate_layer, setCornerRadius: plate_radius];
        let _: () = msg_send![plate, setHidden: true];

        let thumb_rect = layout::thumb_rect(*rect);
        let thumbnail = passthrough_view(thumb_rect);
        let thumb_layer: *mut AnyObject = msg_send![thumbnail, layer];
        let _: () = msg_send![thumb_layer, setCornerRadius: layout::THUMB_RADIUS];
        let _: () = msg_send![thumb_layer, setMasksToBounds: true];
        // Transparent and borderless like the switcher's preview container: leftover space around
        // a thumbnail shows the panel's glass directly instead of a flat plate that hides it.
        let _: () = msg_send![thumb_layer, setBorderWidth: 0.0f64];
        layer_set_background(thumb_layer, std::ptr::null_mut());
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
        // `alloc/init` hands this function the only reference and the superview now keeps the view
        // alive: releasing it is what stops every rebuild from leaking one image view per card (the
        // card keeps the non-owning pointer it lays the icon out with).
        release_obj(placeholder_image);

        let header_icon = header_icon_view(*rect, icon);

        let title_rect = layout::title_rect(*rect);
        let title = passthrough_label(title_rect, &window.title);
        let measured = measured_title(state, &window.title);
        let truncated = layout::title_is_truncated(measured, title_rect.w);

        let close_rect = layout::close_rect(*rect);
        let close = passthrough_view(close_rect);
        let close_layer: *mut AnyObject = msg_send![close, layer];
        let _: () = msg_send![close_layer, setCornerRadius: 4.0f64];
        // Same shape as the switcher's card close button: a plain × in `muted_text` on the
        // surface, with the destructive emphasis only while the pointer is on the button.
        let cross_rect = Rect::new(3.0, 2.0, CLOSE_SIZE - 6.0, CLOSE_SIZE - 4.0);
        let cross = close_cross_view(cross_rect);
        let _: () = msg_send![close, addSubview: cross];
        // `alloc/init` gives this function the only reference; the superview now keeps the view
        // alive, so the local one is released or every rebuilt list leaks one label per row.
        release_obj(cross);
        let _: () = msg_send![close, setHidden: true];

        // Flat siblings: the hover plate is behind everything, the thumbnail over it, and the
        // header copy (icon/title/close) above.
        let _: () = msg_send![state.content, addSubview: plate];
        let _: () = msg_send![state.content, addSubview: thumbnail];
        let _: () = msg_send![state.content, addSubview: placeholder];
        let _: () = msg_send![state.content, addSubview: header_icon];
        let _: () = msg_send![state.content, addSubview: title];
        let _: () = msg_send![state.content, addSubview: close];

        state.cards.push(CardEntry {
            window: window.clone(),
            plate,
            thumbnail,
            placeholder,
            placeholder_icon: placeholder_image,
            header_icon,
            title,
            close,
            close_cross: cross,
            close_hot: false,
            title_truncated: truncated,
            has_frame: false,
            image_size: None,
            requested_at: None,
            closing_since: None,
            close_pending: false,
            last_probe: Instant::now(),
        });
        release_obj(plate);
        release_obj(thumbnail);
        release_obj(placeholder);
        release_obj(header_icon);
        release_obj(title);
        release_obj(close);
    }
    if let Some(icon) = icon {
        release_obj(icon);
    }
}

unsafe fn build_rows(state: &mut PanelState, request: &PanelRequest) {
    shared_row_icon(state, &request.bundle);
    let shared = state.shared_icon;
    let palette = theme::ui_palette();
    let row_w = state
        .geometry
        .items
        .first()
        .map(|item| item.w)
        .unwrap_or(state.geometry.viewport.w);
    let boxes = layout::row_boxes(row_w);
    let icon = if shared.is_null() { None } else { Some(shared) };
    for (index, window) in request.windows.iter().enumerate() {
        let item = state
            .geometry
            .items
            .get(index)
            .copied()
            .unwrap_or(Rect::new(0.0, layout::row_top(index), row_w, layout::ROW_H));
        let plate = row_plate(item);
        let icon_view = image_view_at(
            Rect::new(
                item.x + boxes.icon.x,
                item.y + boxes.icon.y,
                boxes.icon.w,
                boxes.icon.h,
            ),
            icon,
        );
        let (title, has_title, truncated) = if boxes.title_hidden {
            (std::ptr::null_mut(), false, true)
        } else {
            let rect = Rect::new(
                item.x + boxes.title.x,
                item.y + boxes.title.y,
                boxes.title.w,
                boxes.title.h,
            );
            let label = passthrough_label(rect, &window.title);
            style_label(label, theme::FONT_CONTROL);
            let _: () = msg_send![label, setTextColor: glass::panel_ink(palette.primary_text, glass::PanelInk::Primary)];
            let measured = measured_title(state, &window.title);
            (
                label,
                true,
                layout::title_is_truncated(measured, boxes.title.w),
            )
        };
        let close = passthrough_view(Rect::new(
            item.x + boxes.close.x,
            item.y + boxes.close.y,
            boxes.close.w,
            boxes.close.h,
        ));
        let close_layer: *mut AnyObject = msg_send![close, layer];
        let _: () = msg_send![close_layer, setCornerRadius: 4.0f64];
        let cross = close_cross_view(Rect::new(3.0, 2.0, CLOSE_SIZE - 6.0, CLOSE_SIZE - 4.0));
        let _: () = msg_send![close, addSubview: cross];
        release_obj(cross);
        let _: () = msg_send![close, setHidden: true];

        let _: () = msg_send![state.content, addSubview: plate];
        let _: () = msg_send![state.content, addSubview: icon_view];
        if has_title {
            let _: () = msg_send![state.content, addSubview: title];
        }
        let _: () = msg_send![state.content, addSubview: close];
        release_obj(plate);
        release_obj(icon_view);
        release_obj(close);
        if has_title {
            release_obj(title);
        }
        state.rows.push(RowEntry {
            window: window.clone(),
            plate,
            icon: icon_view,
            title,
            close,
            close_cross: cross,
            close_hot: false,
            has_title,
            title_truncated: truncated,
            closing_since: None,
            close_pending: false,
            last_probe: Instant::now(),
        });
    }
}

/// The hover plate of one row: the row grown to the content's sides and, for the first and last
/// row, to the content's top and bottom, so the wash follows the panel rather than floating inside.
unsafe fn row_plate(item: Rect) -> *mut AnyObject {
    let plate = passthrough_view(item);
    let layer: *mut AnyObject = msg_send![plate, layer];
    layer_set_background(layer, hex_to_cg_color(layout::HOVER_PLATE_FILL));
    let _: () = msg_send![layer, setCornerRadius: layout::CARD_RADIUS];
    let _: () = msg_send![plate, setHidden: true];
    plate
}

/// Refresh a card's text and truncation from a new request without rebuilding its views.
unsafe fn rebind_card(
    card: &mut CardEntry,
    window: &PreviewWindow,
    measurements: &mut std::collections::HashMap<String, f64>,
) {
    card.window = window.clone();
    let string = make_nsstring(&window.title);
    let _: () = msg_send![card.title, setStringValue: string];
    release_obj(string);
    let frame: NSRect = msg_send![card.title, frame];
    let measured = match measurements.get(&window.title) {
        Some(width) => *width,
        None => {
            let width = glass::measure_single_line_text(&window.title, theme::FONT_CAPTION).width;
            measurements.insert(window.title.clone(), width);
            width
        }
    };
    card.title_truncated = layout::title_is_truncated(measured, frame.size.width);
}

/// Refresh a row's text and truncation from a new request without rebuilding its views.
unsafe fn rebind_row(
    row: &mut RowEntry,
    window: &PreviewWindow,
    measurements: &mut std::collections::HashMap<String, f64>,
) {
    row.window = window.clone();
    if !row.has_title {
        return;
    }
    let string = make_nsstring(&window.title);
    let _: () = msg_send![row.title, setStringValue: string];
    release_obj(string);
    let frame: NSRect = msg_send![row.title, frame];
    let measured = match measurements.get(&window.title) {
        Some(width) => *width,
        None => {
            let size = glass::measure_single_line_text(&window.title, theme::FONT_CONTROL);
            measurements.insert(window.title.clone(), size.width);
            size.width
        }
    };
    row.title_truncated = layout::title_is_truncated(measured, frame.size.width);
}

fn remove_row_views(state: &mut PanelState, release_shared: bool) {
    for row in state.rows.drain(..) {
        remove_single_row(&row);
    }
    if release_shared && !state.shared_icon.is_null() {
        unsafe { release_obj(state.shared_icon) };
        state.shared_icon = std::ptr::null_mut();
    }
}

fn remove_single_row(row: &RowEntry) {
    unsafe {
        for view in [row.plate, row.icon, row.title, row.close] {
            if !view.is_null() {
                let _: () = msg_send![view, removeFromSuperview];
            }
        }
    }
}

/// Place the cards of the single-row layout, and move the content viewport to its scroll origin.
fn place_cards(state: &PanelState) {
    let rects = &state.geometry.items;
    let panel = (state.geometry.panel.w, state.geometry.panel.h);
    let plates = layout::plates_for_grid(rects, state.geometry.columns.max(1), panel);
    for (index, card) in state.cards.iter().enumerate() {
        let Some(rect) = rects.get(index).copied() else {
            break;
        };
        let (plate_rect, radius) = plates
            .get(index)
            .copied()
            .unwrap_or((rect, layout::PANEL_RADIUS));
        unsafe {
            place_view(card.plate, plate_rect);
            let layer: *mut AnyObject = msg_send![card.plate, layer];
            let _: () = msg_send![layer, setCornerRadius: radius];
            let thumb = layout::thumb_rect(rect);
            let frame = card
                .image_size
                .map(|(w, h)| layout::centered_thumb_frame(w, h, thumb))
                .unwrap_or(thumb);
            place_view(card.thumbnail, frame);
            place_view(card.placeholder, layout::thumb_rect(rect));
            place_placeholder_icon(card.placeholder_icon, layout::thumb_rect(rect));
            place_view(card.header_icon, layout::icon_rect(rect));
            place_view(card.title, layout::title_rect(rect));
            place_view(card.close, layout::close_rect(rect));
        }
    }
}

/// Place every list row inside the content view. A scroll never calls this: it only moves the
/// content view, which is why a row can never be left behind at an old position.
fn place_rows(state: &PanelState) {
    let row_w = state
        .geometry
        .items
        .first()
        .map(|item| item.w)
        .unwrap_or(state.geometry.viewport.w);
    let boxes = layout::row_boxes(row_w);
    for (index, row) in state.rows.iter().enumerate() {
        let item = state
            .geometry
            .items
            .get(index)
            .copied()
            .unwrap_or(Rect::new(0.0, layout::row_top(index), row_w, layout::ROW_H));
        unsafe {
            place_view(row.plate, item);
            place_view(
                row.icon,
                Rect::new(
                    item.x + boxes.icon.x,
                    item.y + boxes.icon.y,
                    boxes.icon.w,
                    boxes.icon.h,
                ),
            );
            if row.has_title {
                place_view(
                    row.title,
                    Rect::new(
                        item.x + boxes.title.x,
                        item.y + boxes.title.y,
                        boxes.title.w,
                        boxes.title.h,
                    ),
                );
            }
            place_view(
                row.close,
                Rect::new(
                    item.x + boxes.close.x,
                    item.y + boxes.close.y,
                    boxes.close.w,
                    boxes.close.h,
                ),
            );
        }
    }
}

unsafe fn header_icon_view(rect: Rect, image: Option<*mut AnyObject>) -> *mut AnyObject {
    let icon_rect = layout::icon_rect(rect);
    image_view_at(icon_rect, image)
}

unsafe fn image_view_at(rect: Rect, image: Option<*mut AnyObject>) -> *mut AnyObject {
    let view: *mut AnyObject = msg_send![class!(NSImageView), alloc];
    let view: *mut AnyObject = msg_send![view, initWithFrame: NSRect::new(
        NSPoint::new(rect.x, rect.y),
        NSSize::new(rect.w, rect.h)
    )];
    let _: () = msg_send![view, setImageScaling: 3u64];
    if let Some(image) = image.filter(|image| !image.is_null()) {
        let _: () = msg_send![view, setImage: image];
    }
    view
}

/// The × glyph of a close button, in `muted_text` until the pointer reaches the button itself.
unsafe fn close_cross_view(rect: Rect) -> *mut AnyObject {
    let palette = theme::ui_palette();
    let cross = passthrough_label_at(
        NSRect::new(NSPoint::new(rect.x, rect.y), NSSize::new(rect.w, rect.h)),
        "×",
    );
    let _: () = msg_send![cross, setAlignment: 2isize]; // centered
    let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: theme::FONT_CAPTION];
    let _: () = msg_send![cross, setFont: font];
    let _: () = msg_send![cross, setTextColor: glass::panel_ink(palette.muted_text, glass::PanelInk::Muted)];
    cross
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
    let _: () = msg_send![label, setTextColor: glass::panel_ink(theme::ui_palette().primary_text, glass::PanelInk::Primary)];
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
    for index in 0..state.cards.len() {
        let active = hovered == Some(index);
        let closing =
            state.cards[index].close_pending || state.cards[index].closing_since.is_some();
        {
            let card = &state.cards[index];
            unsafe {
                let _: () = msg_send![card.plate, setHidden: !active];
            }
            if !active {
                set_close_style(card, false);
            }
        }
        if !active {
            state.cards[index].close_hot = false;
        }
        set_item_close_visible(state, index, active && !closing);
        let _ = closing;
    }
    for index in 0..state.rows.len() {
        let active = hovered == Some(index);
        let closing = state.rows[index].close_pending || state.rows[index].closing_since.is_some();
        {
            let row = &state.rows[index];
            unsafe {
                let _: () = msg_send![row.plate, setHidden: !active];
            }
            if !active {
                set_row_close_style(row, false);
            }
        }
        if !active {
            state.rows[index].close_hot = false;
        }
        set_item_close_visible(state, index, active && !closing);
    }
}

/// Show or hide one item's close button from the ONE rule every path uses: a button is visible
/// only while the pointer is on its own item and that item is not already closing.
///
/// A refused or timed-out close used to show the button unconditionally, so a row the pointer was
/// no longer on kept a close button that the hit test would not accept -- and the next tick, finding
/// the hovered index unchanged, had no reason to correct it.
fn set_item_close_visible(state: &PanelState, index: usize, visible: bool) {
    let close = state
        .cards
        .get(index)
        .map(|card| card.close)
        .or_else(|| state.rows.get(index).map(|row| row.close));
    let Some(close) = close else {
        return;
    };
    if close.is_null() {
        return;
    }
    unsafe {
        let _: () = msg_send![close, setHidden: !visible];
    }
}

/// The switcher's close-button emphasis: at rest the glyph is `muted_text` on the bare surface;
/// only the pointer being on the button itself turns it `destructive` and adds the matching wash.
fn set_close_style(card: &CardEntry, hot: bool) {
    emphasize_close(card.close, card.close_cross, hot);
}

fn set_row_close_style(row: &RowEntry, hot: bool) {
    emphasize_close(row.close, row.close_cross, hot);
}

fn emphasize_close(close: *mut AnyObject, cross: *mut AnyObject, hot: bool) {
    let palette = theme::ui_palette();
    unsafe {
        let color = if hot {
            hex_to_ns_color(palette.destructive)
        } else {
            glass::panel_ink(palette.muted_text, glass::PanelInk::Muted)
        };
        let _: () = msg_send![cross, setTextColor: color];
        let layer: *mut AnyObject = msg_send![close, layer];
        if hot {
            layer_set_background(
                layer,
                hex_to_cg_color(palette.destructive & 0xFFFF_FF00 | 0x12),
            );
        } else {
            layer_set_background(layer, std::ptr::null_mut());
        }
    }
}

/// Show or hide one item's close button. A hidden button is also not part of hit-testing.
trait CloseSlot {
    fn close_view(&self) -> *mut AnyObject;
}

impl CloseSlot for CardEntry {
    fn close_view(&self) -> *mut AnyObject {
        self.close
    }
}

impl CloseSlot for RowEntry {
    fn close_view(&self) -> *mut AnyObject {
        self.close
    }
}

/// Hide a closing item's close button. Showing one is never done by a toggle: a button comes back
/// only through [`apply_hover`], which is the single rule for "hovered and not closing".
unsafe fn set_close_enabled(item: &impl CloseSlot, enabled: bool) {
    let close = item.close_view();
    if close.is_null() {
        return;
    }
    let _: () = msg_send![close, setHidden: !enabled];
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
    if view.is_null() {
        return;
    }
    let _: () = msg_send![view, setFrame: NSRect::new(
        NSPoint::new(rect.x, rect.y),
        NSSize::new(rect.w, rect.h)
    )];
}

/// Update the thumbnail views from the shared capture cache, and ask for the missing frames
/// within the in-flight budget. Only the single-row layout ever asks for a capture.
fn fill_thumbnails(state: &mut PanelState, now: Instant) {
    if state.cards.is_empty() {
        return;
    }
    // No permission, no work: the shared cache and the capture call are both gated, and a panel
    // built for a replaced process instance must not capture its successor's windows. The
    // switcher overlay's own display mode does NOT gate this: the dock preview's cards are its
    // feature's shape, so they capture whenever the permission allows, even while the overlay
    // presents icons only.
    if !crate::thumbnail::capture_allowed()
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
    for (index, card) in state.cards.iter_mut().enumerate() {
        if card.has_frame || card.window.wid == 0 {
            continue;
        }
        if let Some((pointer, width, height)) =
            crate::thumbnail::lookup_retained(card.window.pid, card.window.wid)
        {
            card.image_size = Some((width as f64, height as f64));
            // Centre the freshly fitted frame inside the thumbnail box right away; the box alone
            // is only correct while the aspect is unknown (see `centered_thumb_frame`).
            if let Some(rect) = state.geometry.items.get(index).copied() {
                let thumb = layout::thumb_rect(rect);
                let frame = layout::centered_thumb_frame(width as f64, height as f64, thumb);
                unsafe { place_view(card.thumbnail, frame) };
            }
            unsafe {
                let layer: *mut AnyObject = msg_send![card.thumbnail, layer];
                layer_set_contents(layer, pointer as *mut c_void);
                layer_set_contents_scale(layer, scale);
                let _: () = msg_send![card.thumbnail, setHidden: false];
                let _: () = msg_send![card.placeholder, setHidden: true];
            }
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
    let zone = match state.pointer {
        Some(pointer) => zone_at(
            state,
            (
                pointer.0 - state.geometry.panel.x,
                pointer.1 - state.geometry.panel.y,
            ),
        ),
        None => PointerZone::Outside,
    };
    let truncated_item = state.hovered.filter(|index| {
        item_title(state, *index)
            .map(|(_, truncated)| truncated)
            .unwrap_or(false)
    });
    let decision = crate::dock_preview::decisions::tooltip_decision(
        zone,
        truncated_item,
        state.title_hover_since,
        now,
    );
    match decision {
        crate::dock_preview::decisions::TooltipDecision::Show => {
            let Some(index) = truncated_item else {
                return;
            };
            // The anchor is resolved before the title: a visible tooltip only has to be re-placed
            // when what the window is placed against moved -- the item's HORIZONTAL span (it is
            // centred on the item, and the item's y never reaches the window) or the
            // panel/edge/visible context (the vertical reference, the side and the clamp). At the
            // same index the text is the one already shown, so a list scrolling up and down costs
            // nothing. A show that cannot place the item must not commit `tooltip_visible`,
            // because the next tick reads a visible tooltip and never retries.
            let Some(item) = state.geometry.items.get(index).copied() else {
                return;
            };
            let item = content_to_screen(state, item);
            let anchor = TooltipAnchor {
                item,
                panel: state.geometry.panel,
                edge: state.geometry.edge,
                visible: state.geometry.visible,
            };
            if state.tooltip_visible {
                if state.tooltip_anchor.is_some_and(|placed| {
                    placed.same_place(item, anchor.panel, anchor.edge, anchor.visible)
                }) {
                    return;
                }
                let Some((text, _)) = item_title(state, index) else {
                    return;
                };
                unsafe {
                    show_tooltip(state, item, &text);
                }
                state.tooltip_anchor = Some(anchor);
                return;
            }
            let Some((text, _)) = item_title(state, index) else {
                return;
            };
            unsafe {
                show_tooltip(state, item, &text);
            }
            state.tooltip_visible = true;
            state.tooltip_text = text;
            state.tooltip_anchor = Some(anchor);
            defer_record("dock_preview_tooltip");
        }
        crate::dock_preview::decisions::TooltipDecision::Hide => {
            if state.tooltip_visible {
                hide_tooltip(state);
            }
        }
    }
}

/// The window title and truncation of the item at `index`.
fn item_title(state: &PanelState, index: usize) -> Option<(String, bool)> {
    state
        .cards
        .get(index)
        .map(|card| (card.window.title.clone(), card.title_truncated))
        .or_else(|| {
            state
                .rows
                .get(index)
                .map(|row| (row.window.title.clone(), row.title_truncated))
        })
}

unsafe fn show_tooltip(state: &PanelState, item: Rect, text: &str) {
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
        item,
        state.geometry.panel,
        size,
        state.geometry.edge,
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
    state.tooltip_anchor = None;
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
                builder.add_method::<AnyObject, _>(
                    sel!(scrollWheel:),
                    container_scroll_wheel
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

/// A click: resolve it to a target under one lock, then dispatch it by that window's identity.
///
/// The resolution happens before the dispatch and the panel lock is released first, because the
/// dispatch re-enters the preview module (which reads the panel again for the app it belongs to).
extern "C" fn container_mouse_down(_self: *mut AnyObject, _cmd: Sel, event: *mut AnyObject) {
    crate::callback_guard::void("dock_preview_mouse_down", || {
        let Some(point) = container_point(event) else {
            return;
        };
        let Some(target) = hit_target(point) else {
            flush_records();
            return;
        };
        match target.kind {
            HitKind::Close => crate::dock_preview::close_target(target),
            HitKind::Activate => crate::dock_preview::activate_target(target),
        }
        flush_records();
    });
}

/// Wheel and trackpad input scrolls the viewport the pointer is over, and nothing else.
///
/// A gesture outside the viewport, or with nothing to scroll, is left unconsumed so it reaches the
/// Dock like any other wheel event.
extern "C" fn container_scroll_wheel(_self: *mut AnyObject, _cmd: Sel, event: *mut AnyObject) {
    crate::callback_guard::void("dock_preview_scroll_wheel", || unsafe {
        let Some((mode, row_axis, max_scroll, viewport)) = scroll_context() else {
            return;
        };
        let Some(point) = container_point(event) else {
            return;
        };
        let in_viewport = layout::pointer_in_viewport(point, viewport);
        let dx: f64 = msg_send![event, scrollingDeltaX];
        let dy: f64 = msg_send![event, scrollingDeltaY];
        let precise: Bool = msg_send![event, hasPreciseScrollingDeltas];
        let step = |delta: f64| {
            if precise == Bool::YES {
                -delta
            } else {
                // A discrete wheel reports whole lines; one notch keeps its sign and moves one row.
                -delta.signum() * layout::ROW_H
            }
        };
        let Some((axis, raw)) =
            layout::scroll_axis_for(mode, row_axis, in_viewport, max_scroll, dx, dy)
        else {
            return;
        };
        // The axis was chosen by magnitude; the delta that moves the content is the step of the
        // input that axis came from, so both wheel directions still reach opposite ends.
        scroll_by(axis, step(raw));
        flush_records();
    });
}

/// The values the wheel handler needs, read in one lock.
fn scroll_context() -> Option<(PanelMode, RowAxis, f64, Rect)> {
    let slot = PANEL.lock().ok()?;
    let state = slot.as_ref()?;
    Some((
        state.geometry.mode,
        state.geometry.row_axis,
        state.geometry.max_scroll,
        state.geometry.viewport,
    ))
}

/// A pointer event in the CONTAINER's coordinates, whatever view received it.
///
/// The hit test and the viewport test are both expressed in the container's frame, so the event is
/// always converted against the panel's own container rather than against the sender: a gesture that
/// reached the content view (which is scrolled and offset inside the viewport) would otherwise have
/// the padding and the scroll applied to it a second time.
fn container_point(event: *mut AnyObject) -> Option<(f64, f64)> {
    let slot = PANEL.lock().ok()?;
    let state = slot.as_ref()?;
    let in_window: NSPoint = unsafe { msg_send![event, locationInWindow] };
    let local: NSPoint = unsafe {
        msg_send![
            state.container,
            convertPoint: in_window,
            fromView: std::ptr::null::<AnyObject>()
        ]
    };
    Some((local.x, local.y))
}

/// A flipped view that takes no events: the layout's y grows downwards everywhere (panel, viewport,
/// content), so the viewport and content must be flipped too, and the CONTAINER is the only view
/// that receives a click or a wheel event.
fn flipped_passive_view_class() -> *mut AnyObject {
    static CLASS: OnceLock<crate::ffi::StaticClass> = OnceLock::new();
    crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabDockPreviewFlipped", || {
        crate::ffi::build_class(
            c"OhMyTabDockPreviewFlipped",
            class!(NSView),
            |builder| unsafe {
                builder.add_method::<AnyObject, _>(
                    sel!(isFlipped),
                    container_is_flipped as extern "C" fn(*mut AnyObject, Sel) -> Bool,
                );
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
///
/// Returns false when the new position leaves no operable space; the caller hides the panel rather
/// than leaving it parked where it was built.
pub(crate) fn update_anchor(anchor: Rect, edge: DockEdge, visible: Rect) -> bool {
    let Ok(mut slot) = PANEL.lock() else {
        return true;
    };
    let Some(state) = slot.as_mut() else {
        return true;
    };
    if state.cards.is_empty() && state.rows.is_empty() {
        return true;
    }
    // The anchor move is a re-layout through the same entry a removal uses, so the scroll offset is
    // kept and clamped against the NEW range rather than against the geometry it came from.
    state.geometry.anchor = anchor;
    state.geometry.edge = edge;
    state.geometry.visible = visible;
    match relayout_content(state, false) {
        ContentFit::Placed => true,
        ContentFit::Resized(size, scroll) => {
            drop(slot);
            defer_record("dock_preview_anchor_resize");
            apply_resize(size, scroll)
        }
        ContentFit::Gone => {
            defer_record("dock_preview_anchor_resize");
            false
        }
    }
}

pub(crate) fn visible_card_count() -> usize {
    let Ok(slot) = PANEL.lock() else {
        return 0;
    };
    slot.as_ref()
        .map(|state| state.cards.len() + state.rows.len())
        .unwrap_or(0)
}

/// Every card shows either a captured frame or its placeholder icon -- never an empty rectangle.
pub(crate) fn cards_are_renderable() -> bool {
    let Ok(slot) = PANEL.lock() else {
        return false;
    };
    let Some(state) = slot.as_ref() else {
        return false;
    };
    if state.mode == PanelMode::List {
        return !state.rows.is_empty()
            && state.rows.iter().all(|row| unsafe {
                let hidden: Bool = msg_send![row.icon, isHidden];
                hidden == Bool::NO
            });
    }
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

/// The item index a pointer zone names when the zone is that item's BODY.
///
/// A card's title box is not a body: it owns the tooltip. A list row whose title band is too narrow
/// to draw ("body is title") is a body everywhere except its close slot, so it counts.
pub(crate) fn peek_body_index(zone: PointerZone) -> Option<usize> {
    let slot = PANEL.lock().ok()?;
    let state = slot.as_ref()?;
    match zone {
        PointerZone::Card(index) => Some(index),
        PointerZone::Title(index) if state.geometry.body_is_title.contains(&index) => Some(index),
        _ => None,
    }
}

/// The window an item index names, for code that already resolved a pointer zone and needs the
/// window behind it (the peek's own target checks).
pub(crate) fn window_at_index(index: usize) -> Option<(i32, u32)> {
    let slot = PANEL.lock().ok()?;
    let state = slot.as_ref()?;
    let window = state
        .cards
        .get(index)
        .map(|card| card.window.clone())
        .or_else(|| state.rows.get(index).map(|row| row.window.clone()))?;
    Some((window.pid, window.wid))
}

/// The item a panel-local point hits, ignoring the close slot: the window under the REAL cursor
/// right now, hit against the panel's current geometry and scroll.
///
/// `current_pointer_zone` answers with the zone the last tick sampled, which is up to one frame old.
/// A decision that lands between two ticks (a worker coming back) must not act on a stale pointer,
/// so it hits the cursor itself instead. An item whose close is in flight is not a target: the tick
/// treats it as the panel's background, and a stale hover result landing on it would raise a window
/// the user just asked to close.
pub(crate) fn window_under_pointer() -> Option<(i32, u32)> {
    let pointer = crate::dock_preview::pointer_in_cg()?;
    let slot = PANEL.lock().ok()?;
    let state = slot.as_ref()?;
    let local = (
        pointer.0 - state.geometry.panel.x,
        pointer.1 - state.geometry.panel.y,
    );
    let index = match zone_at(state, local) {
        PointerZone::Card(index) | PointerZone::Title(index) => index,
        _ => return None,
    };
    if item_is_closing(state, index) {
        return None;
    }
    let window = state
        .cards
        .get(index)
        .map(|card| card.window.clone())
        .or_else(|| state.rows.get(index).map(|row| row.window.clone()))?;
    Some((window.pid, window.wid))
}

pub(crate) fn title_rect_for_smoke(index: usize) -> Option<Rect> {
    let slot = PANEL.lock().ok()?;
    let state = slot.as_ref()?;
    let item = *state.geometry.items.get(index)?;
    // The SAME boxes the drawing and the hit test use, converted by the one content->screen helper:
    // a probe that adds the offsets itself reports a box that is one viewport away from the real one.
    let boxes = layout::boxes_for(state.mode, item);
    let title = if boxes.title_hidden {
        boxes.row
    } else {
        boxes.title
    };
    Some(content_to_screen(state, title))
}

/// Whether one item's title is truncated (so the tooltip is its only full-text surface).
pub(crate) fn title_is_truncated(index: usize) -> bool {
    let Ok(slot) = PANEL.lock() else {
        return false;
    };
    slot.as_ref()
        .and_then(|state| item_title(state, index))
        .map(|(_, truncated)| truncated)
        .unwrap_or(false)
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

/// The item rectangle the visible tooltip was placed against, for the smoke runner. The window's own
/// frame is clamped by the visible frame, so a constructed room (which pins the panel to a corner)
/// asserts the anchor it was given rather than the clamped frame.
pub(crate) fn tooltip_anchor_for_smoke() -> Option<Rect> {
    let slot = PANEL.lock().ok()?;
    let state = slot.as_ref()?;
    state.tooltip_anchor.map(|placed| placed.item)
}

/// The tooltip window's screen rectangle, for the smoke runner.
pub(crate) fn tooltip_frame_for_smoke() -> Option<Rect> {
    let slot = PANEL.lock().ok()?;
    let state = slot.as_ref()?;
    let frame: NSRect = unsafe { msg_send![state.tooltip_panel, frame] };
    let height = primary_screen_height();
    Some(Rect::new(
        frame.origin.x,
        height - frame.origin.y - frame.size.height,
        frame.size.width,
        frame.size.height,
    ))
}

/// Re-apply the panel's current anchor, edge and visible frame, for the smoke runner's check that a
/// geometry refresh for the same Dock item keeps a visible tooltip. This is the entry the
/// observer's Dock notification reaches, not a private shortcut.
pub(crate) fn refresh_anchor_for_smoke() -> bool {
    let Ok(slot) = PANEL.lock() else {
        return false;
    };
    let Some(state) = slot.as_ref() else {
        return false;
    };
    let (anchor, edge, visible) = (
        state.geometry.anchor,
        state.geometry.edge,
        state.geometry.visible,
    );
    drop(slot);
    update_anchor(anchor, edge, visible)
}

/// Backdate the title rest timer, for a smoke case that has to drive a geometry change while the
/// pointer still rests on a title. The alternative is a real wait, and during one the panel's own
/// tick would read the REAL pointer (which the smoke cannot park) and change the hover. Geometry and
/// the tooltip's visibility are untouched: only the rest clock is moved.
pub(crate) fn dev_age_title_hover_for_smoke(age: Duration) {
    let Ok(mut slot) = PANEL.lock() else {
        return;
    };
    let Some(state) = slot.as_mut() else {
        return;
    };
    if state.title_hover_since.is_some() {
        state.title_hover_since = Some(Instant::now() - age);
    }
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

/// The panel's layout and scroll facts, for the smoke runner (screen rectangles).
pub(crate) struct ListFacts {
    pub(crate) mode: PanelMode,
    pub(crate) viewport: Rect,
    pub(crate) scroll: f64,
    pub(crate) max_scroll: f64,
    /// Rows that fit entirely inside the viewport: list rows, or wrapped card rows.
    pub(crate) full_rows: usize,
    /// Every item in screen coordinates.
    pub(crate) item_screen: Vec<Rect>,
    /// Cards on the widest row of the card layout (0 for the list).
    pub(crate) columns: usize,
    /// Rows of cards the card layout wrapped into (0 for the list).
    pub(crate) card_rows: usize,
}

pub(crate) fn list_facts() -> Option<ListFacts> {
    let slot = PANEL.lock().ok()?;
    let state = slot.as_ref()?;
    let panel = state.geometry.panel;
    let item_screen = state
        .geometry
        .items
        .iter()
        .map(|item| content_to_screen(state, *item))
        .collect();
    let card_rows = {
        let mut ys: Vec<i64> = state
            .geometry
            .items
            .iter()
            .map(|item| item.y.round() as i64)
            .collect();
        ys.dedup();
        if state.geometry.mode == PanelMode::Row {
            ys.len().max(1)
        } else {
            0
        }
    };
    Some(ListFacts {
        columns: state.geometry.columns,
        card_rows,
        mode: state.geometry.mode,
        viewport: to_screen(
            panel,
            Rect::new(
                state.geometry.viewport.x,
                state.geometry.viewport.y,
                state.geometry.viewport.w,
                state.geometry.viewport.h,
            ),
        ),
        scroll: state.geometry.scroll.0.max(state.geometry.scroll.1),
        max_scroll: state.geometry.max_scroll,
        full_rows: match state.geometry.mode {
            PanelMode::List => (state.geometry.viewport.h / layout::ROW_H).floor() as usize,
            PanelMode::Row => state.geometry.items.len(),
        },
        item_screen,
    })
}

/// The AppKit view of one item's position: its frame converted through its own view hierarchy, the
/// viewport's frame the same way, and whether the ancestor that clips it is the viewport.
///
/// This is deliberately an INDEPENDENT reading of what the views report, not the layout model's own
/// arithmetic: it is what makes "the row is really clipped at the viewport edge" a checkable claim
/// rather than a restatement of the input.
pub(crate) fn clip_evidence(index: usize) -> Result<(Rect, Rect, bool), String> {
    let slot = PANEL
        .lock()
        .map_err(|_| "the panel lock is poisoned".to_string())?;
    let state = slot.as_ref().ok_or_else(|| "no panel".to_string())?;
    let view = match state.geometry.mode {
        PanelMode::List => state
            .rows
            .get(index)
            .map(|row| row.plate)
            .ok_or_else(|| format!("no row {index}")),
        PanelMode::Row => state
            .cards
            .get(index)
            .map(|card| card.plate)
            .ok_or_else(|| format!("no card {index}")),
    }?;
    let primary = primary_screen_height();
    unsafe {
        // `convertRect:toView:nil` answers in WINDOW coordinates, and the panel IS the window, so
        // the second step is asked of the panel itself rather than of its `window` accessor.
        let to_window = |view: *mut AnyObject| -> NSRect {
            msg_send![view, convertRect: bounds_of(view), toView: std::ptr::null::<AnyObject>()]
        };
        let to_screen_ns =
            |rect: NSRect| -> NSRect { msg_send![state.panel, convertRectToScreen: rect] };
        let row_frame: NSRect = to_screen_ns(to_window(view));
        let viewport_frame: NSRect = to_screen_ns(to_window(state.viewport));
        let to_cg = |rect: NSRect| {
            Rect::new(
                rect.origin.x,
                primary - rect.origin.y - rect.size.height,
                rect.size.width,
                rect.size.height,
            )
        };
        // Walk up from the row: the first ancestor whose layer clips is the one that decides what
        // of the row can be seen or clicked.
        let mut ancestor: *mut AnyObject = msg_send![view, superview];
        let mut clipping = std::ptr::null_mut::<AnyObject>();
        let mut guard = 0;
        while !ancestor.is_null() && guard < 64 {
            guard += 1;
            let layer: *mut AnyObject = msg_send![ancestor, layer];
            if !layer.is_null() {
                let masks: Bool = msg_send![layer, masksToBounds];
                if masks == Bool::YES {
                    clipping = ancestor;
                    break;
                }
            }
            ancestor = msg_send![ancestor, superview];
        }
        Ok((
            to_cg(row_frame),
            to_cg(viewport_frame),
            clipping == state.viewport,
        ))
    }
}

unsafe fn bounds_of(view: *mut AnyObject) -> NSRect {
    msg_send![view, bounds]
}

// ---------------------------------------------------------------------------------------------
// e2e
// ---------------------------------------------------------------------------------------------

/// The panel's facts for `--e2e-state`, or `null`. Every rectangle is a screen rectangle so a
/// scenario can click without deriving coordinates from the panel rect.
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
    let viewport = to_screen(panel, state.geometry.viewport);
    let items_json = state
        .geometry
        .items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let (wid, title, truncated, has_frame, closing) = match state.mode {
                PanelMode::Row => state
                    .cards
                    .get(index)
                    .map(|card| {
                        (
                            card.window.wid,
                            card.window.title.clone(),
                            card.title_truncated,
                            card.has_frame,
                            card.closing_since.is_some() || card.close_pending,
                        )
                    })
                    .unwrap_or((0, String::new(), false, false, false)),
                PanelMode::List => state
                    .rows
                    .get(index)
                    .map(|row| {
                        (
                            row.window.wid,
                            row.window.title.clone(),
                            row.title_truncated,
                            true,
                            row.closing_since.is_some() || row.close_pending,
                        )
                    })
                    .unwrap_or((0, String::new(), false, false, false)),
            };
            let (title_rect, close_rect) = item_boxes(state.mode, *item);
            let frame = content_to_screen(state, *item);
            let title_screen = content_to_screen(state, title_rect);
            let close_screen = content_to_screen(state, close_rect);
            format!(
                "{{\"wid\": {}, \"title\": {}, \"frame\": {}, \"title_rect\": {}, \"close_rect\": {}, \"truncated\": {}, \"has_frame\": {}, \"closing\": {}, \"touched\": {}}}",
                wid,
                json_string(&title),
                json_rect(frame),
                json_rect(title_screen),
                json_rect(close_screen),
                truncated,
                has_frame,
                closing,
                frame.overlaps(viewport)
            )
        })
        .collect::<Vec<String>>()
        .join(", ");
    // `cards` is the pre-list name of the same array; it is kept so an older scenario still reads it.
    let cards_json = if state.geometry.mode == PanelMode::Row {
        items_json.clone()
    } else {
        String::new()
    };
    // The card layout may be several wrapped rows: report the real content height, the row count and
    // the width of the widest row, so a scenario can assert the wrap instead of guessing from rects.
    let card_rows = {
        let mut ys: Vec<f64> = state
            .geometry
            .items
            .iter()
            .map(|item| item.y.round() as i64 as f64)
            .collect();
        ys.dedup();
        ys.len().max(1)
    };
    let (row_h, full_rows, max_scroll, content_h) = match state.geometry.mode {
        PanelMode::Row => (
            layout::CARD_H,
            card_rows,
            state.geometry.max_scroll,
            state
                .geometry
                .items
                .last()
                .map(|item| item.max_y())
                .unwrap_or(layout::CARD_H),
        ),
        PanelMode::List => {
            let viewport_h = state.geometry.viewport.h;
            (
                layout::ROW_H,
                (viewport_h / layout::ROW_H).floor() as usize,
                state.geometry.max_scroll,
                state
                    .geometry
                    .items
                    .last()
                    .map(|item| item.max_y())
                    .unwrap_or(0.0),
            )
        }
    };
    // The tooltip window's own screen rect while it is up: a scenario compares it with the hovered
    // item and with what the WindowServer reports for the same window.
    let tooltip_rect = if state.tooltip_visible {
        let window: NSRect = unsafe { msg_send![state.tooltip_panel, frame] };
        let primary = primary_screen_height();
        json_rect(Rect::new(
            window.origin.x,
            primary - window.origin.y - window.size.height,
            window.size.width,
            window.size.height,
        ))
    } else {
        "null".to_string()
    };
    format!(
        "{{\"visible\": true, \"pid\": {}, \"bundle\": {}, \"edge\": {}, \"mode\": {}, \"row_axis\": {}, \"panel\": {}, \"viewport\": {}, \"layout\": {{\"row_h\": {:.1}, \"full_rows\": {}, \"max_scroll\": {:.1}, \"content_h\": {:.1}, \"columns\": {}, \"card_rows\": {}}}, \"scroll\": {:.1}, \"content_origin\": {}, \"rows\": [{}], \"cards\": [{}], \"hovered\": {}, \"tooltip_visible\": {}, \"tooltip\": {}, \"tooltip_rect\": {}, \"total_windows\": {}, \"hidden_windows\": {}}}",
        state.pid,
        json_string(&state.bundle),
        json_string(state.geometry.edge.name()),
        json_string(state.geometry.mode.name()),
        json_string(state.geometry.row_axis.name()),
        json_rect(panel),
        json_rect(viewport),
        row_h,
        full_rows,
        max_scroll,
        content_h,
        state.geometry.columns,
        card_rows,
        state.geometry.scroll.0.max(state.geometry.scroll.1),
        json_origin(
            panel.x + state.geometry.viewport.x + layout::content_origin(state.geometry.scroll).0,
            panel.y + state.geometry.viewport.y + layout::content_origin(state.geometry.scroll).1,
        ),
        items_json,
        cards_json,
        match state.hovered {
            Some(index) => format!("{index}"),
            None => "null".to_string(),
        },
        state.tooltip_visible,
        json_string(&state.tooltip_text),
        tooltip_rect,
        state.total_windows,
        state
            .total_windows
            .saturating_sub(state.geometry.items.len().min(state.total_windows))
    )
}

/// The title and close boxes of one item, in content coordinates.
fn item_boxes(mode: PanelMode, item: Rect) -> (Rect, Rect) {
    match mode {
        PanelMode::Row => (layout::title_rect(item), layout::close_rect(item)),
        PanelMode::List => {
            let boxes = layout::row_boxes(item.w);
            (
                Rect::new(
                    item.x + boxes.title.x,
                    item.y + boxes.title.y,
                    boxes.title.w,
                    boxes.title.h,
                ),
                Rect::new(
                    item.x + boxes.close.x,
                    item.y + boxes.close.y,
                    boxes.close.w,
                    boxes.close.h,
                ),
            )
        }
    }
}

/// A CONTENT-coordinate rectangle as a screen rectangle.
///
/// Content coordinates start at the scrolled content view's origin, which sits inside the viewport,
/// which sits inside the panel. Every published rectangle goes through this one function, so a
/// rectangle cannot be published with the viewport's offset or the scroll missing -- the two ways a
/// drawn row and its reported position disagreed.
fn content_to_screen(state: &PanelState, rect: Rect) -> Rect {
    let panel = state.geometry.panel;
    let viewport = state.geometry.viewport;
    let origin = layout::content_origin(state.geometry.scroll);
    Rect::new(
        panel.x + viewport.x + origin.0 + rect.x,
        panel.y + viewport.y + origin.1 + rect.y,
        rect.w,
        rect.h,
    )
}

/// A panel-local rectangle as a screen rectangle.
fn to_screen(panel: Rect, local: Rect) -> Rect {
    Rect::new(panel.x + local.x, panel.y + local.y, local.w, local.h)
}

fn json_origin(x: f64, y: f64) -> String {
    format!("{{\"x\": {x:.1}, \"y\": {y:.1}}}")
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

#[cfg(test)]
mod shell_reuse_tests {
    use super::*;

    fn tag(material: glass::PanelMaterial, glass_style: i64, dark: bool) -> SurfaceTag {
        SurfaceTag {
            material,
            glass_style,
            dark,
        }
    }

    #[test]
    fn a_same_size_reopen_with_a_changed_surface_rebuilds() {
        let size = (656.0, 172.0);
        let built = tag(glass::PanelMaterial::LiquidGlass, 1, false);
        // The pre-fix rule compared only the size: every one of these reused the old shell.
        assert!(!shell_reuse_allowed(
            size,
            built,
            size,
            tag(glass::PanelMaterial::Opaque, 1, false)
        ));
        assert!(!shell_reuse_allowed(
            size,
            built,
            size,
            tag(glass::PanelMaterial::LiquidGlass, 0, false)
        ));
        assert!(!shell_reuse_allowed(
            size,
            built,
            size,
            tag(glass::PanelMaterial::LiquidGlass, 1, true)
        ));
        // The same size with the same surface is the reuse the cache exists for.
        assert!(shell_reuse_allowed(size, built, size, built));
        // A different size rebuilds whatever the surface says.
        assert!(!shell_reuse_allowed(size, built, (656.0, 200.0), built));
    }
}

#[cfg(test)]
mod tooltip_anchor_tests {
    use super::*;

    /// The placement of a visible tooltip: three cards in one row, the third one hovered, above a
    /// bottom Dock.
    fn placed() -> TooltipAnchor {
        TooltipAnchor {
            item: Rect::new(936.0, 504.0, 216.0, 164.0),
            panel: Rect::new(500.0, 500.0, 656.0, 172.0),
            edge: DockEdge::Bottom,
            visible: Rect::new(0.0, 24.0, 1470.0, 862.0),
        }
    }

    #[test]
    fn a_visible_tooltip_is_replaced_only_when_its_placement_changed() {
        let anchor = placed();
        let (item, panel, edge, visible) = (anchor.item, anchor.panel, anchor.edge, anchor.visible);
        assert!(anchor.same_place(item, panel, edge, visible));

        // A list scrolling vertically moves the item's y alone: the tooltip's y is the panel's, so
        // the window does not move and its label is not measured again.
        let scrolled = Rect::new(item.x, item.y - 32.0, item.w, item.h);
        assert!(anchor.same_place(scrolled, panel, edge, visible));

        // A horizontal scroll does move it.
        let sideways = Rect::new(item.x - 20.0, item.y, item.w, item.h);
        assert!(!anchor.same_place(sideways, panel, edge, visible));

        // So do a moved panel, a Dock on another edge and a narrowed visible frame: the frame
        // derives its side and its clamp from those, even when the item and the panel are unchanged.
        let moved = Rect::new(panel.x + 10.0, panel.y, panel.w, panel.h);
        assert!(!anchor.same_place(item, moved, edge, visible));
        assert!(!anchor.same_place(item, panel, DockEdge::Top, visible));
        let narrowed = Rect::new(0.0, 24.0, 1200.0, 862.0);
        assert!(!anchor.same_place(item, panel, edge, narrowed));
    }
}
