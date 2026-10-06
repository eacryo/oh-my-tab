//! This module writes a JSON snapshot of internal state for the A2 end-to-end layer (script +
//! cua-driver CLI; see the testing tiers in AGENTS.md). AX cannot express CALayer content (the
//! sidebar highlight pill) and cannot express "which card is selected" or "which window was just
//! raised" at all, so the app states the checkable facts itself: the script asserts on JSON and cua
//! is left with input and a few pixel probes. Enabled by `--e2e-state=<path>`; without it every
//! function returns immediately. Writes go through `<path>.tmp` + rename so a reader never sees
//! half a document, and `seq` lets a script wait for a *new* frame instead of sleeping.

use objc2::msg_send;
use objc2::runtime::AnyObject;
use objc2::sel;
use objc2_foundation::NSRect;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::OnceLock;

use crate::log_debug;

static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
static SEQ: AtomicU64 = AtomicU64::new(0);
static SMOOTH_TICKS: AtomicU64 = AtomicU64::new(0);
static SPACE_RECOVERED_ACCEPTED: AtomicU64 = AtomicU64::new(0);
static SPACE_GATE_REJECTED: AtomicU64 = AtomicU64::new(0);
static SPACE_MEMBERSHIP_SOURCE: AtomicU8 = AtomicU8::new(0);
static SPACE_IN_TRANSITION: AtomicBool = AtomicBool::new(false);
/// The last Space context this module published. A Space switch that the grouping feature handles
/// correctly does not change the candidate set, so nothing else would emit a frame for it; A2 needs
/// one to assert the app's own view of which Space is active.
static LAST_SPACE_CONTEXT: OnceLock<std::sync::Mutex<Option<String>>> = OnceLock::new();
static SPACE_TRANSITION_DEADLINE_MS: AtomicU64 = AtomicU64::new(0);
static SMOOTH_PHASES: [AtomicU64; 6] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

/// A held-Tab repeat cannot be synthesized (macOS generates the repeat stream from a physically
/// held key), so the accepted and throttled counts are the state that makes the behaviour
/// checkable: hold Tab, then read the snapshot.
static TAB_REPEAT_STEPS: AtomicU64 = AtomicU64::new(0);
static TAB_REPEAT_THROTTLED: AtomicU64 = AtomicU64::new(0);

pub(crate) fn tab_repeat_step() {
    if is_enabled() {
        TAB_REPEAT_STEPS.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn tab_repeat_throttled() {
    if is_enabled() {
        TAB_REPEAT_THROTTLED.fetch_add(1, Ordering::Relaxed);
    }
}

/// Resolves `--e2e-state=<path>` once, then serves it from cache.
fn state_path() -> Option<&'static PathBuf> {
    PATH.get_or_init(|| crate::dev_flags::value("e2e-state").map(PathBuf::from))
        .as_ref()
}

pub(crate) fn is_enabled() -> bool {
    state_path().is_some()
}

pub(crate) fn smooth_scroll_tick() {
    if is_enabled() {
        SMOOTH_TICKS.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn smooth_scroll_phase(phase: crate::mouse::smooth::engine::Phase) {
    if !is_enabled() {
        return;
    }
    let index = match phase {
        crate::mouse::smooth::engine::Phase::TouchBegan => 0,
        crate::mouse::smooth::engine::Phase::TouchChanged => 1,
        crate::mouse::smooth::engine::Phase::TouchEnded => 2,
        crate::mouse::smooth::engine::Phase::MomentumBegan => 3,
        crate::mouse::smooth::engine::Phase::MomentumChanged => 4,
        crate::mouse::smooth::engine::Phase::MomentumEnded => 5,
    };
    SMOOTH_PHASES[index].fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn space_recovered_accepted() {
    if is_enabled() {
        SPACE_RECOVERED_ACCEPTED.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn space_gate_rejected() {
    if is_enabled() {
        SPACE_GATE_REJECTED.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn set_space_membership_source(skylight: bool) {
    if is_enabled() {
        SPACE_MEMBERSHIP_SOURCE.store(u8::from(skylight), Ordering::Relaxed);
    }
}

/// A stable string for the current per-display Space context: display, active Space, kind, origin.
fn space_context_signature() -> String {
    crate::space_groups::with_tracker(|tracker| {
        let mut contexts: Vec<_> = tracker
            .topology()
            .displays
            .iter()
            .map(|(display_id, display)| {
                let kind = tracker.topology().kind(display.current);
                let origin = tracker
                    .fullscreen_origins()
                    .get(&display.current)
                    .map(|origin| origin.ordinary_space);
                format!("{display_id}:{}:{kind:?}:{origin:?}", display.current)
            })
            .collect();
        contexts.sort();
        contexts.join("|")
    })
}

pub(crate) fn set_space_transition(in_transition: bool, deadline_unix_ms: u64) {
    if is_enabled() {
        SPACE_TRANSITION_DEADLINE_MS.store(deadline_unix_ms, Ordering::Relaxed);
        SPACE_IN_TRANSITION.store(in_transition, Ordering::Relaxed);
    }
}

/// Records one snapshot. Main thread only (it borrows AppState internally).
pub(crate) fn record(event: &str) {
    write(event, None);
}

/// Writes a frame when the app's Space context changed since the last published frame, even when
/// the candidate set did not. Called after a refresh is applied, so the frame's cards are the set
/// computed for the context it reports.
pub(crate) fn record_if_space_context_changed() {
    if !is_enabled() {
        return;
    }
    let signature = Some(space_context_signature());
    let last = LAST_SPACE_CONTEXT.get_or_init(|| std::sync::Mutex::new(None));
    let changed = {
        let mut last = last.lock().unwrap();
        if *last == signature {
            false
        } else {
            *last = signature;
            true
        }
    };
    if changed {
        write("refresh_context", None);
    }
}

/// Records a commit snapshot carrying the window this release targets. Must run *before* the
/// selection is cleared: once the overlay hides, AppState no longer holds a selected index.
pub(crate) fn record_commit(pid: i32, window_id: u32, app: &str, index: usize) {
    write("commit", Some((pid, window_id, app.to_string(), index)));
}

/// A card snapshot (only the fields assertions need, so the JSON is not a mirror of internals).
struct Card {
    pid: i32,
    window_id: u32,
    app: String,
    title: String,
    active: bool,
    minimized: bool,
    fullscreen: bool,
    bounds: (f64, f64, f64, f64),
}

/// One node of the view tree. `frame` is in the parent's coordinate space, so cross-level
/// comparisons are meaningless: scripts must only compare nodes sharing a `parent`.
struct ViewNode {
    root: &'static str,
    parent: i64,
    depth: usize,
    class: String,
    frame: (f64, f64, f64, f64),
    text: Option<String>,
}

/// Walks the view tree recursively. `text` comes from `stringValue` (button title / text-field
/// content) and stays null when the view has none.
unsafe fn walk_views(
    view: *mut AnyObject,
    root: &'static str,
    parent: i64,
    depth: usize,
    out: &mut Vec<ViewNode>,
) {
    /// Depth and node caps: the settings page builds its own view tree, so bound the JSON growth.
    const MAX_DEPTH: usize = 8;
    const MAX_NODES: usize = 3000;
    if view.is_null() || depth > MAX_DEPTH || out.len() >= MAX_NODES {
        return;
    }
    let frame: NSRect = msg_send![view, frame];
    let class: *mut AnyObject = msg_send![view, class];
    let class_name = if class.is_null() {
        String::new()
    } else {
        let description: *mut AnyObject = msg_send![class, description];
        crate::ffi::nsstring_to_rust(description)
    };
    let text = {
        let responds: bool = msg_send![view, respondsToSelector: sel!(stringValue)];
        if responds {
            let value: *mut AnyObject = msg_send![view, stringValue];
            let text = crate::ffi::nsstring_to_rust(value);
            (!text.is_empty()).then_some(text)
        } else {
            None
        }
    };
    let index = out.len() as i64;
    out.push(ViewNode {
        root,
        parent,
        depth,
        class: class_name,
        frame: (
            frame.origin.x,
            frame.origin.y,
            frame.size.width,
            frame.size.height,
        ),
        text,
    });
    let subviews: *mut AnyObject = msg_send![view, subviews];
    if subviews.is_null() {
        return;
    }
    let count: usize = msg_send![subviews, count];
    for position in 0..count {
        let child: *mut AnyObject = msg_send![subviews, objectAtIndex: position];
        walk_views(child, root, index, depth + 1, out);
    }
}

/// Settings scroll geometry: the moment the page's document width differs from the visible width is
/// the failure moment (a legacy scroller takes space, the clip narrows while the document does not,
/// so the right column gets clipped). All three levels are recorded so nothing assumes which level
/// is the scroll view. `scroller_style`: 0 = legacy, 1 = overlay, -1 = not a scroll view / unknown.
struct PageGeometry {
    root: &'static str,
    self_frame: NSRect,
    parent_frame: NSRect,
    grandparent_frame: NSRect,
    /// The clip (contentView) bounds width is the truly visible content width. With a legacy
    /// scroller it is narrower than the document by the scroller width, while the document is still
    /// laid out to the window width -- which clips the right column.
    clip_bounds: NSRect,
    /// Page document height and the content's top/bottom edges (frames of the document's direct
    /// subviews, in document coordinates). A2 asserts "no dead space below the content / content is
    /// not clipped" from these; all three are measured off the document view, never a height
    /// constant.
    doc_height: f64,
    content_top: f64,
    content_bottom: f64,
    style_self: isize,
    style_parent: isize,
    style_grandparent: isize,
}

/// Document height plus the y range its direct subviews cover (content top / bottom edges). An empty
/// document yields (0, 0, 0).
unsafe fn document_extent(scroll: *mut AnyObject) -> (f64, f64, f64) {
    let document: *mut AnyObject = msg_send![scroll, documentView];
    if document.is_null() {
        return (0.0, 0.0, 0.0);
    }
    let frame: NSRect = msg_send![document, frame];
    let subviews: *mut AnyObject = msg_send![document, subviews];
    if subviews.is_null() {
        return (frame.size.height, 0.0, 0.0);
    }
    let count: usize = msg_send![subviews, count];
    let mut top = f64::NEG_INFINITY;
    let mut bottom = f64::INFINITY;
    for index in 0..count {
        let child: *mut AnyObject = msg_send![subviews, objectAtIndex: index as isize];
        if child.is_null() {
            continue;
        }
        let child_frame: NSRect = msg_send![child, frame];
        if child_frame.size.width <= 0.0 && child_frame.size.height <= 0.0 {
            continue;
        }
        top = top.max(child_frame.origin.y + child_frame.size.height);
        bottom = bottom.min(child_frame.origin.y);
    }
    if !top.is_finite() || !bottom.is_finite() {
        return (frame.size.height, 0.0, 0.0);
    }
    (frame.size.height, top, bottom)
}

fn zero_rect() -> NSRect {
    NSRect::new(
        objc2_foundation::NSPoint::new(0.0, 0.0),
        objc2_foundation::NSSize::new(0.0, 0.0),
    )
}

fn collect_pages() -> Vec<PageGeometry> {
    let mut pages = Vec::new();
    for (root, view) in crate::settings::e2e_view_roots() {
        if !root.starts_with("page_") || view.is_null() {
            continue;
        }
        unsafe {
            let scroller_style = |object: *mut AnyObject| -> isize {
                if object.is_null() {
                    return -1;
                }
                let responds: bool = msg_send![object, respondsToSelector: sel!(scrollerStyle)];
                if !responds {
                    return -1;
                }
                msg_send![object, scrollerStyle]
            };
            let frame_of = |object: *mut AnyObject| -> NSRect {
                if object.is_null() {
                    return zero_rect();
                }
                msg_send![object, frame]
            };
            let parent: *mut AnyObject = msg_send![view, superview];
            let grandparent: *mut AnyObject = if parent.is_null() {
                std::ptr::null_mut()
            } else {
                msg_send![parent, superview]
            };
            let clip: *mut AnyObject = {
                let responds: bool = msg_send![view, respondsToSelector: sel!(contentView)];
                if responds {
                    msg_send![view, contentView]
                } else {
                    std::ptr::null_mut()
                }
            };
            let clip_bounds: NSRect = if clip.is_null() {
                zero_rect()
            } else {
                msg_send![clip, bounds]
            };
            let (doc_height, content_top, content_bottom) = document_extent(view);
            pages.push(PageGeometry {
                root,
                self_frame: frame_of(view),
                parent_frame: frame_of(parent),
                grandparent_frame: frame_of(grandparent),
                clip_bounds,
                doc_height,
                content_top,
                content_bottom,
                style_self: scroller_style(view),
                style_parent: scroller_style(parent),
                style_grandparent: scroller_style(grandparent),
            });
        }
    }
    pages
}

fn collect_views() -> Vec<ViewNode> {
    let mut nodes = Vec::new();
    for (root, view) in crate::settings::e2e_view_roots() {
        unsafe { walk_views(view, root, -1, 0, &mut nodes) };
    }
    nodes
}

struct Snapshot {
    visible: bool,
    selected: usize,
    windows: Vec<Card>,
}

fn collect() -> Snapshot {
    crate::with_tab_state(|state_opt| match state_opt.as_ref() {
        Some(state) => Snapshot {
            visible: state.visible,
            selected: state.selected,
            windows: state
                .windows
                .iter()
                .map(|w| Card {
                    pid: w.pid,
                    window_id: w.window_id,
                    app: w.app_name.clone(),
                    title: w.window_title.clone(),
                    active: w.is_active,
                    minimized: w.minimized,
                    fullscreen: w.fullscreen,
                    bounds: w.bounds,
                })
                .collect(),
        },
        None => Snapshot {
            visible: false,
            selected: 0,
            windows: Vec::new(),
        },
    })
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

fn write(event: &str, committed: Option<(i32, u32, String, usize)>) {
    let Some(path) = state_path() else {
        return;
    };
    let snapshot = collect();
    let seq = SEQ.fetch_add(1, Ordering::SeqCst) + 1;
    let (front_app, front_pid) = crate::ffi::frontmost_app_info();
    let thumbnails = crate::overlay::thumbnail_visible_range();

    let mut json = String::with_capacity(2048);
    json.push_str("{\n");
    json.push_str(&format!("  \"seq\": {seq},\n"));
    // The writing process. A scenario restarts the app between runs, so a snapshot left on disk by
    // the previous process can carry a higher `seq` than the new one has reached yet; scoping the
    // baseline to the pid is what makes "wait for a newer frame" mean a frame from *this* run.
    json.push_str(&format!("  \"pid\": {},\n", std::process::id()));
    json.push_str(&format!("  \"event\": {},\n", json_string(event)));
    json.push_str(&format!("  \"visible\": {},\n", snapshot.visible));
    json.push_str(&format!(
        "  \"settings_window_visible\": {},\n",
        crate::settings::settings_window_is_visible()
    ));
    // The installed panel material (after the Reduce Transparency override) — AX cannot tell
    // a glass view from a plain layer, so the app states the fact itself.
    json.push_str(&format!(
        "  \"panel_material\": {},\n",
        json_string(crate::glass::effective_material_id())
    ));
    // Where the clipboard picker actually is, top-left based, so a screenshot can measure inside it
    // instead of guessing from the cursor position. `footer_band` is the footer legend band in the panel's
    // own points: the footer captions are a different text role from the filter row's, so the A2 contrast
    // scenario needs their region to measure each role separately (a whole-row extent would report only the
    // strongest ink and could hide a dim one).
    // One region per footer caption: a union of them is a bounding box that also spans the keycaps, and a
    // region has to hold one text role before its contrast number means anything.
    let hints = {
        let frames = crate::clipboard::picker_footer_hint_frames();
        if frames.is_empty() {
            "null".to_string()
        } else {
            let items: Vec<String> = frames
                .iter()
                .map(|(x, y, w, h)| {
                    format!("{{\"x\": {x:.1}, \"y\": {y:.1}, \"w\": {w:.1}, \"h\": {h:.1}}}")
                })
                .collect();
            format!("[{}]", items.join(", "))
        }
    };
    json.push_str(&format!(
        "  \"clipboard_picker\": {},\n",
        match crate::clipboard::picker_frame_top_left() {
            Some((x, y, w, h)) => format!(
                "{{\"visible\": {}, \"x\": {:.1}, \"y\": {:.1}, \"w\": {:.1}, \"h\": {:.1}, \"footer_hints\": {}}}",
                crate::clipboard::picker_is_visible(),
                x,
                y,
                w,
                h,
                hints
            ),
            None => "null".to_string(),
        }
    ));
    // The effective glass look and the tint it resolved to, verbatim from the config. AX cannot express a
    // tint and `NSGlassEffectView.tintColor` reports a system default, so the app states the value itself:
    // this is what makes "the panel shipped with the wrong tint (or an opaque white sheet)" assertable.
    json.push_str(&format!(
        "  \"glass_look\": {}, \"glass_tint\": \"{:08x}\",\n",
        json_string(&crate::config::effective_glass_style()),
        crate::glass::resolved_glass_tint_hex()
    ));
    // The material-strength knobs, and whether the private `_variant` selector exists on this system:
    // no accessibility tree can express either fact, and the look depends on both.
    json.push_str(&format!(
        "  \"panel_material_tuning\": {{\"opacity\": {:.3}, \"tint\": \"{:08x}\", \"blur_radius\": {:.1}, \"saturation\": {:.2}, \"variant\": {}, \"variant_supported\": {}}},\n",
        crate::config::effective_glass_opacity(),
        crate::glass::resolved_glass_tint_hex(),
        crate::config::effective_glass_blur_radius(),
        crate::config::effective_glass_saturation(),
        crate::config::effective_glass_variant(),
        crate::glass::glass_variant_is_supported()
    ));
    json.push_str(&format!(
        "  \"smooth_scroll\": {{\"ticks\": {}, \"touch_began\": {}, \"touch_changed\": {}, \"touch_ended\": {}, \"momentum_began\": {}, \"momentum_changed\": {}, \"momentum_ended\": {}}},\n",
        SMOOTH_TICKS.load(Ordering::Relaxed),
        SMOOTH_PHASES[0].load(Ordering::Relaxed),
        SMOOTH_PHASES[1].load(Ordering::Relaxed),
        SMOOTH_PHASES[2].load(Ordering::Relaxed),
        SMOOTH_PHASES[3].load(Ordering::Relaxed),
        SMOOTH_PHASES[4].load(Ordering::Relaxed),
        SMOOTH_PHASES[5].load(Ordering::Relaxed),
    ));
    let space_group_state = crate::space_groups::with_tracker(|tracker| {
        let unknown_active = tracker
            .topology()
            .displays
            .iter()
            .filter(|(_, display)| {
                tracker.topology().kind(display.current)
                    == crate::space_groups::SpaceKind::Fullscreen
                    && !tracker.fullscreen_origins().contains_key(&display.current)
            })
            .count();
        let mut contexts: Vec<_> = tracker
            .topology()
            .displays
            .iter()
            .map(|(display_id, display)| {
                let kind = tracker.topology().kind(display.current);
                let origin = tracker
                    .fullscreen_origins()
                    .get(&display.current)
                    .map(|origin| origin.ordinary_space);
                (display_id.clone(), display.current, kind, origin)
            })
            .collect();
        contexts.sort_by(|a, b| a.0.cmp(&b.0));
        (
            tracker.topology().displays.len(),
            tracker.fullscreen_origins().len(),
            unknown_active,
            tracker.evidence_contiguous(),
            crate::window_server::space_membership_tracking_available(),
            contexts,
        )
    });
    let space_contexts = space_group_state
        .5
        .iter()
        .map(|(display_id, space_id, kind, origin)| {
            let kind = match kind {
                crate::space_groups::SpaceKind::Ordinary => "ordinary",
                crate::space_groups::SpaceKind::Fullscreen => "fullscreen",
                crate::space_groups::SpaceKind::Unknown => "unknown",
            };
            let origin = origin
                .map(|origin| origin.to_string())
                .unwrap_or_else(|| "null".into());
            format!(
                "{{\"display\": \"{}\", \"space\": {}, \"kind\": \"{}\", \"origin\": {}}}",
                display_id, space_id, kind, origin
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    json.push_str(&format!("  \"space_contexts\": [{}],\n", space_contexts));
    json.push_str(&format!(
        "  \"space_filter\": {{\"recovered_accepted\": {}, \"gate_rejected\": {}}},\n",
        SPACE_RECOVERED_ACCEPTED.load(Ordering::Relaxed),
        SPACE_GATE_REJECTED.load(Ordering::Relaxed),
    ));
    json.push_str(&format!(
        "  \"space_groups\": {{\"displays\": {}, \"confirmed_fullscreen_origins\": {}, \"unknown_active_fullscreen_spaces\": {}, \"evidence_contiguous\": {}, \"source_learning_available\": {}}},\n",
        space_group_state.0,
        space_group_state.1,
        space_group_state.2,
        space_group_state.3,
        space_group_state.4,
    ));
    json.push_str(&format!(
        "  \"membership_source\": \"{}\",\n",
        if SPACE_MEMBERSHIP_SOURCE.load(Ordering::Relaxed) == 1 {
            "skylight"
        } else {
            "legacy"
        }
    ));
    json.push_str(&format!(
        "  \"space_transition\": {{\"in_transition\": {}, \"deadline_ms\": {}}},\n",
        SPACE_IN_TRANSITION.load(Ordering::Relaxed),
        SPACE_TRANSITION_DEADLINE_MS.load(Ordering::Relaxed),
    ));
    json.push_str(&format!(
        "  \"tab_repeat\": {{\"steps\": {}, \"throttled\": {}}},\n",
        TAB_REPEAT_STEPS.load(Ordering::Relaxed),
        TAB_REPEAT_THROTTLED.load(Ordering::Relaxed),
    ));
    json.push_str(&format!("  \"selected_index\": {},\n", snapshot.selected));
    json.push_str(&format!("  \"cards_count\": {},\n", snapshot.windows.len()));
    let selected_key = snapshot.windows.get(snapshot.selected);
    match selected_key {
        Some(card) => json.push_str(&format!(
            "  \"selected_key\": {{\"pid\": {}, \"window_id\": {}}},\n",
            card.pid, card.window_id
        )),
        None => json.push_str("  \"selected_key\": null,\n"),
    }
    match committed {
        Some((pid, window_id, ref app, index)) => json.push_str(&format!(
            "  \"committed\": {{\"pid\": {pid}, \"window_id\": {window_id}, \"app\": {}, \"index\": {index}}},\n",
            json_string(app)
        )),
        None => json.push_str("  \"committed\": null,\n"),
    }
    json.push_str(&format!(
        "  \"frontmost\": {{\"app\": {}, \"pid\": {front_pid}}},\n",
        json_string(&front_app)
    ));
    json.push_str(&format!(
        "  \"permissions\": {{\"accessibility\": {}, \"screen_recording\": {}}},\n",
        crate::ffi::has_accessibility_permission(),
        crate::thumbnail::capture_allowed()
    ));
    // Stopping any service must never latch the terminal disable; A2 asserts this stays false
    // after toggling a feature off (a self-inflicted `CGEventTapEnable(false)` pseudo-event).
    json.push_str(&format!(
        "  \"taps\": {{\"user_input_disabled\": {}, \"allowed\": {}, \"switcher_active\": {}}},\n",
        crate::input_monitor::user_input_disabled(),
        crate::input_monitor::taps_allowed(),
        // A scenario that synthesizes events must wait for the switcher tap: a frame can already be
        // on disk while the tap thread is still installing, and events posted then are lost.
        crate::event_monitor::tap_is_active()
    ));
    json.push_str(&format!(
        "  \"selected_sidebar\": {},\n",
        crate::settings::e2e_selected_sidebar()
    ));
    json.push_str("  \"pages\": [");
    for (index, page) in collect_pages().iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            "\n    {{\"root\": {}, \"self\": [{}, {}, {}, {}], \"parent\": [{}, {}, {}, {}], \"grandparent\": [{}, {}, {}, {}], \"clip\": [{}, {}, {}, {}], \"styles\": [{}, {}, {}], \"doc\": [{}, {}, {}]}}",
            json_string(page.root),
            page.self_frame.origin.x,
            page.self_frame.origin.y,
            page.self_frame.size.width,
            page.self_frame.size.height,
            page.parent_frame.origin.x,
            page.parent_frame.origin.y,
            page.parent_frame.size.width,
            page.parent_frame.size.height,
            page.grandparent_frame.origin.x,
            page.grandparent_frame.origin.y,
            page.grandparent_frame.size.width,
            page.grandparent_frame.size.height,
            page.clip_bounds.origin.x,
            page.clip_bounds.origin.y,
            page.clip_bounds.size.width,
            page.clip_bounds.size.height,
            page.style_self,
            page.style_parent,
            page.style_grandparent,
            page.doc_height,
            page.content_top,
            page.content_bottom
        ));
    }
    json.push_str("\n  ],\n");
    json.push_str("  \"views\": [");
    for (index, node) in collect_views().iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            "\n    {{\"root\": {}, \"parent\": {}, \"depth\": {}, \"class\": {}, \"frame\": [{}, {}, {}, {}], \"text\": {}}}",
            json_string(node.root),
            node.parent,
            node.depth,
            json_string(&node.class),
            node.frame.0,
            node.frame.1,
            node.frame.2,
            node.frame.3,
            match &node.text {
                Some(text) => json_string(text),
                None => "null".to_string(),
            }
        ));
    }
    json.push_str("\n  ],\n");
    match thumbnails {
        Some(range) => json.push_str(&format!(
            "  \"thumbnail_range\": [{}, {}],\n",
            range.start, range.end
        )),
        None => json.push_str("  \"thumbnail_range\": null,\n"),
    }
    json.push_str("  \"cards\": [");
    for (index, card) in snapshot.windows.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            "\n    {{\"index\": {index}, \"pid\": {}, \"window_id\": {}, \"app\": {}, \"title\": {}, \"active\": {}, \"minimized\": {}, \"fullscreen\": {}, \"bounds\": [{}, {}, {}, {}]}}",
            card.pid,
            card.window_id,
            json_string(&card.app),
            json_string(&card.title),
            card.active,
            card.minimized,
            card.fullscreen,
            card.bounds.0,
            card.bounds.1,
            card.bounds.2,
            card.bounds.3
        ));
    }
    json.push_str("\n  ],\n");
    let keystroke = crate::keystroke_display::e2e_snapshot();
    json.push_str(&format!(
        "  \"keystroke_display\": {{\"visible\": {}, \"badges\": [",
        keystroke.visible
    ));
    for (index, badge) in keystroke.badges.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            "{{\"text\": {}, \"kind\": {}, \"repeats\": {}}}",
            json_string(&badge.text),
            json_string(badge.kind),
            badge.repeats
        ));
    }
    json.push_str(&format!(
        "], \"secure_paused\": {}, \"tap_level\": {}}}\n}}\n",
        keystroke.secure_paused,
        json_string(&keystroke.tap_level)
    ));

    write_atomically(path, &json);
}

fn write_atomically(path: &Path, contents: &str) {
    let tmp = path.with_extension("tmp");
    let result = std::fs::File::create(&tmp)
        .and_then(|mut file| {
            file.write_all(contents.as_bytes())?;
            file.sync_all()
        })
        .and_then(|()| std::fs::rename(&tmp, path));
    if let Err(error) = result {
        log_debug!("[e2e-state] write failed for {}: {error}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Window titles and app names are external data, so they must be escaped: a quote in a title
    /// would otherwise corrupt the document.
    #[test]
    fn json_string_escapes_external_text() {
        assert_eq!(json_string("plain"), "\"plain\"");
        assert_eq!(json_string("a\"b"), "\"a\\\"b\"");
        assert_eq!(json_string("a\\b"), "\"a\\\\b\"");
        assert_eq!(json_string("a\nb\tc"), "\"a\\nb\\tc\"");
        assert_eq!(json_string("bell\u{7}"), "\"bell\\u0007\"");
        // Non-ASCII titles stay literal rather than \\u-escaped so scripts can grep them directly.
        assert_eq!(json_string("微信 — 聊天"), "\"微信 — 聊天\"");
    }
}
