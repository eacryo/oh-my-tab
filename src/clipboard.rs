//! History clipboard module (text + images, optional persistence).
//!
//! Architecture:
//! - A main-thread NSTimer polls NSPasteboard's changeCount every 0.5s; when it changes,
//!   the text/image is read into the history (duplicates are skipped, overflow trimmed).
//! - Option+V is detected by the event_monitor tap and marshalled to the main thread via the
//!   bridge (on_clipboard_toggle), showing/hiding the picker. Tab cycles filters; arrow keys
//!   / Enter / Esc / clicks navigate: up/down select, left pins (also while the detail panel
//!   is open), right expands a detail panel (full text / large image; it follows ↑/↓ browsing
//!   live; pressing → again closes it), Enter or a click = write back to the
//!   pasteboard + synthesize Cmd+V for an automatic paste (mirrors Windows' Win+V). The
//!   detail panel is a passive display (never becomes key, so keyboard focus stays in the
//!   list); a click anywhere on it or Esc closes it, and it hides together with the picker.
//!   The detail's text is mouse-selectable; copying goes through the native paths only --
//!   the right-click menu, and Cmd+C in the picker (forwarded by container_key_down to
//!   copy_detail_selection: the selection, or the full text when nothing is selected). No
//!   paste marker is stamped -- a selection copy is a genuine copy that enters the history
//!   normally.
//! - Text entries keep the raw text; image-DATA entries keep their ORIGINAL bytes ON DISK
//!   (`~/Library/Caches/oh-my-tab-clip-images/`, keyed by a content hash) with only a
//!   downsampled PNG preview in memory; pasting reads the bytes back on demand and writes
//!   them under the original UTI (a JPG pastes back as JPG, an animated GIF as a GIF).
//!   FILE-COPY entries read the file ONCE at record time (transiently) for a content hash
//!   and a thumbnail preview, then discard the bytes (no data-cache write, no shadow
//!   copy -- the reference semantics of Windows Win+V / Maccy): pasting restores
//!   `public.file-url` and the target app reads the original file on demand; a deleted or
//!   moved source makes the entry unpastable; the content hash collapses a file and its
//!   Finder duplicate (different paths, identical bytes) into one entry. The row shows the
//!   thumbnail, and `text` holds the filename (searchable). The cache dir is wiped at
//!   startup when persistence is off; when persistence is on, a reference-based sweep
//!   removes orphan files. Files are also removed in sync with delete/clear-all/trim. A separate `{hash}.detail` preview (longest
//!   edge <= 1280px, pregenerated in the background at record time with a first-open
//!   fallback, never held in RAM) feeds the detail panel and shares the same deletion lifecycle.
use crate::clipboard_highlight::{
    apply_code_paragraph_styles, apply_link_color, apply_visible_space_markers, classify_text,
    prepare_code_display, prepare_code_for_soft_wrap, prepare_code_no_wrap_display,
    DisplaySourceMap, PreparedCodeDisplay, TextKind, CODE_ADVANCE_PT,
};
use crate::config::CONFIG;
use crate::event_tap::{
    keyboard, CGEventCreateKeyboardEvent, CGEventFlags, CGEventPost, CGEventSetFlags,
    K_CG_SESSION_EVENT_TAP,
};
use crate::ffi::{
    class_addMethod, localtime_r, make_nsstring, nsstring_to_rust, objc_allocateClassPair,
    objc_msgSendSuper, objc_registerClassPair, release_obj, run_save_panel, CFRelease, CFRetain,
    CallbackTarget, MainThreadSlot, ObjPtr, ObjcSuper, StaticClass, Tm,
};
use crate::hash::fnv1a64;
use crate::i18n::{t, t_count, tf};
use crate::theme::resolved_is_dark;
use crate::{log_debug, log_info};
use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSPoint, NSRange, NSRect, NSSize};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ffi::{c_void, CString};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};
mod detail;
mod image_cache;
mod model;
mod monitor;
mod notifications;
mod pasteboard;
mod persist;
mod picker;
mod search;
mod smoke;
mod text_style;
use detail::*;
use image_cache::*;
use model::*;
use monitor::*;
use notifications::*;
use pasteboard::*;
use persist::*;
use picker::*;
use search::*;
use smoke::*;
use text_style::*;
// Entry points exposed to the rest of the crate (implemented in the child modules).
pub(crate) use detail::{apply_glass_properties, apply_theme};
pub(crate) use monitor::{start, stop};
pub(crate) use persist::apply_persist_toggle;
pub(crate) use picker::on_clipboard_toggle;
pub(crate) use smoke::{set_smoke_mode, smoke_runner};
pub(crate) use text_style::refresh_localized_ui;
/// The pasteboard file-URL type (carried by Finder file copies; restoring it on paste =
/// file semantics).
const NSPASTEBOARD_TYPE_FILE_URL: &str = "public.file-url";
/// The generic pasteboard URL type (carried by Finder file copies; compatibility).
const NSPASTEBOARD_TYPE_URL: &str = "public.url";
/// The plain-text pasteboard type (same as NSPasteboardTypeString).
const NSPASTEBOARD_TYPE_STRING: &str = "public.utf8-plain-text";
/// The pasteboard PNG type (same as NSPasteboardTypePNG).
const NSPASTEBOARD_TYPE_PNG: &str = "public.png";
/// The pasteboard JPEG type (same as NSPasteboardTypeJPEG).
const NSPASTEBOARD_TYPE_JPEG: &str = "public.jpeg";
/// The pasteboard GIF type (same as NSPasteboardTypeGIF; animation preserved).
const NSPASTEBOARD_TYPE_GIF: &str = "com.compuserve.gif";
/// The GIF type's alias (some apps declare public.gif instead of
/// com.compuserve.gif; recognizing only the canonical one would fall through to the
/// static TIFF fallback).
const NSPASTEBOARD_TYPE_GIF_ALIAS: &str = "public.gif";
/// the pasteboard WebP type.
const NSPASTEBOARD_TYPE_WEBP: &str = "org.webmproject.webp";
/// the pasteboard HEIC type.
const NSPASTEBOARD_TYPE_HEIC: &str = "public.heic";
/// the pasteboard BMP type.
const NSPASTEBOARD_TYPE_BMP: &str = "com.microsoft.bmp";
/// The pasteboard TIFF type (the generic macOS fallback, same as NSPasteboardTypeTIFF).
const NSPASTEBOARD_TYPE_TIFF: &str = "public.tiff";
/// The image rows' thumbnail box (45x38, radius 6, faint fill + inner ring, mirroring the
/// HTML mockup).
const THUMB_W: f64 = 72.0;
const THUMB_H: f64 = 44.0;
const THUMB_R: f64 = 6.0;
/// gap between the app icon and the thumb in the canvas.
/// keycode used when synthesizing Cmd+V.
const VK_V: u16 = keyboard::VK_V;
/// Command modifier mask for synthesized paste.
const K_CG_EVENT_FLAG_MASK_COMMAND: CGEventFlags = keyboard::FLAG_COMMAND;
/// polling interval (seconds)
const POLL_INTERVAL: f64 = 0.5;
/// The picker's max height: 61pt rows + the header (108) + the footer (43), ~8-9 rows +
/// paddings ≈ 720pt, ~73% of a 1080p screen's usable height; more entries scroll. Small
/// screens shrink it further (see show_picker).
const PICKER_MAX_HEIGHT: f64 = 720.0;
/// the picker's minimum height for regular lists.
const PICKER_MIN_HEIGHT: f64 = 250.0;
/// Picker width (the mockup's 720px scaled to 560pt -- readable content without losing
/// density).
const PICKER_W: f64 = 560.0;
/// the uniform row height (mockup min-height 61px).
const ROW_H: f64 = 78.0;
/// the item's bottom meta bar height.
const META_FOOTER_H: f64 = 17.0;
/// Detail body line height (a safe layout height for 14pt text), preventing NSTextView's
/// actual line box from exceeding the estimate.
const DETAIL_LINE_H: f64 = 18.0;
/// NSTextView's vertical textContainerInset applies at both the top and bottom. Detail text
/// reuses the list's 11pt top inset, so sizing must include both sides; otherwise exactly
/// two lines overflow and incorrectly show a scrollbar.
const DETAIL_TEXT_INSET_H: f64 = ROW_PAD_TOP * 2.0;
/// Conservative fallback units for detail-text height estimation. List content no longer uses
/// character units for truncation; native cells lay it out from real font metrics. This value
/// is only used by the headless-safe detail document estimate.
const LINE_MAX_UNITS: usize = 60;
/// the list's side padding (8px).
const PAD_X: f64 = 8.0;
/// The row's padding (the new mockup's 11 11 8 13).
const ROW_PAD_TOP: f64 = 11.0;
const ROW_PAD_R: f64 = 11.0;
const ROW_PAD_BOT: f64 = 8.0;
const ROW_PAD_L: f64 = 13.0;
/// the search bar's inner padding (12px).
const SEARCH_PAD_IN: f64 = 12.0;
/// The reserved search-icon column (the mockup's `.search-icon { width: 22px }`).
const SEARCH_ICON_W: f64 = 22.0;
/// The extra clear × size when a query exists.
const SEARCH_CLEAR_W: f64 = 18.0;
/// Shared query font size for both editing and unfocused states.
const SEARCH_FONT_SIZE: f64 = 14.0;
/// The meta line's source-app icon size (the new mockup's .app-icon 13px).
const META_ICON: f64 = 13.0;
/// The per-row action buttons' size (the new mockup's 23x21, gap 2).
const ACTION_BTN: f64 = 23.0;
const ACTION_H: f64 = 21.0;
const ACTION_GAP: f64 = 2.0;
/// Detail SVG-style icon canvas (16px in mockup).
const DETAIL_ACTION_ICON: f64 = 16.0;
/// the actions strip's width.
const ACTIONS_W: f64 = ACTION_BTN * 3.0 + ACTION_GAP * 2.0;
/// the time-group header zone height (27px).
const GROUP_H: f64 = 27.0;
/// the group label's top offset (vertically centered).
const GROUP_LABEL_PAD: f64 = 7.0;
/// The header strip: the search zone's top padding (the new mockup's 12px).
const TOP_PAD_Y: f64 = 12.0;
/// the search bar's height (the new mockup's 40px).
const SEARCH_H: f64 = 40.0;
/// the search bar's side padding (14px).
const SEARCH_PAD_X: f64 = 14.0;
/// the search bar's corner radius (the new mockup's 9px).
const SEARCH_R: f64 = 9.0;
/// the gap under the search bar (the new mockup's 6px).
const SEARCH_GAP_Y: f64 = 6.0;
/// the filters row's height (the new mockup's 36px).
const FILTERS_H: f64 = 36.0;
/// the filters row's side padding (20px).
const FILTERS_PAD_X: f64 = 20.0;
/// the gap between filter items (17px).
const FILTER_GAP: f64 = 17.0;
/// Fixed underline dimensions and animation duration; tab changes move its center only.
const FILTER_UNDERLINE_W: f64 = 16.0;
const FILTER_UNDERLINE_H: f64 = 2.0;
const FILTER_UNDERLINE_ANIMATION_DURATION: f64 = 0.20;
/// the footer's height (43px).
const FOOTER_H: f64 = 43.0;
/// the window's bottom padding.
const PAD_Y: f64 = 12.0;
/// the footer's side padding (16px).
const FOOTER_PAD_X: f64 = 16.0;
/// the footer shortcut groups' spacing.
const FOOTER_GROUP_GAP: f64 = 16.0;
/// The list's top offset inside the document (mockup 2px).
const CLEAR_BTN_GAP: f64 = 2.0;
/// Fixed geometry for the clear-confirmation card; two text actions share one horizontal baseline.
const CLEAR_CONFIRM_BUTTON_H: f64 = 24.0;
const CLEAR_CONFIRM_BUTTON_PAD_X: f64 = 8.0;
const CLEAR_CONFIRM_GAP: f64 = 10.0;
const CLEAR_CONFIRM_CARD_PAD_X: f64 = 8.0;
const CLEAR_CONFIRM_CARD_PAD_Y: f64 = 6.0;
const CLEAR_CONFIRM_CARD_H: f64 = CLEAR_CONFIRM_CARD_PAD_Y * 2.0 + CLEAR_CONFIRM_BUTTON_H;
const CLEAR_CONFIRM_BUTTON_FONT_SIZE: f64 = 11.0;
const CLEAR_CONFIRM_SHELL_DURATION: f64 = 0.46;
const CLEAR_CONFIRM_CONTENT_DURATION: f64 = 0.36;
/// the glass panel's corner radius (16px).
const CORNER_R: f64 = 16.0;
/// the row highlight's corner radius (8px).
const SEL_TILE_R: f64 = 8.0;
/// inset 9px top/bottom).
const SEL_BAR_W: f64 = 2.0;
const SEL_BAR_X: f64 = 1.0;
const SEL_BAR_INSET_Y: f64 = 10.0;
/// Resolve the shared settings/overlay palette for clipboard surfaces and controls.
fn clipboard_palette() -> crate::theme::UiPalette {
    crate::theme::ui_palette()
}
/// visible custom scroll indicator width.
const SCROLL_INDICATOR_W: f64 = 6.0;
/// Actual mouse hit width; transparent side padding enlarges the drag area without changing
/// the visible capsule width.
const SCROLL_INDICATOR_HIT_W: f64 = 10.0;
/// empty inset at each scrollbar track edge.
const SCROLL_INDICATOR_EDGE: f64 = 3.0;
/// The detail view always reserves a lower-right safe corner, even when only one scrollbar exists.
const SCROLL_INDICATOR_CORNER_GAP: f64 = 2.0;
/// Lower-right safe-corner reserve = the other scrollbar's hit width plus the visual gap.
const SCROLL_INDICATOR_CORNER_RESERVE: f64 = SCROLL_INDICATOR_HIT_W + SCROLL_INDICATOR_CORNER_GAP;
/// visible scrollbar capsule radius (matching its 6pt width).
const SCROLL_INDICATOR_R: f64 = 3.0;
/// minimum indicator length (too short is unreadable).
const SCROLL_INDICATOR_MIN_LEN: f64 = 24.0;
/// gap between the picker and the detail panel.
const DETAIL_GAP: f64 = 8.0;
/// the detail panel's inner padding.
const DETAIL_PAD: f64 = 12.0;
/// Heights of the detail toolbar and source/statistics footer.
const DETAIL_TOOLBAR_H: f64 = 36.0;
const DETAIL_FOOTER_H: f64 = 42.0;
const DETAIL_CHROME_H: f64 = DETAIL_TOOLBAR_H + DETAIL_FOOTER_H;
/// AppKit darkens Liquid Glass in the passive detail window. A 55% overlay of the current
/// glass tint compensates it back to the picker's unselected base surface.
const DETAIL_INACTIVE_GLASS_COMPENSATION_A: u32 = 0x8D;
/// Fixed outer width shared by text, code, and image details to prevent horizontal jumps.
const DETAIL_MAX_W: f64 = 640.0;
/// Code details use the same fixed width; with soft wrap off, native horizontal scrolling shows
/// long lines.
const DETAIL_CODE_MAX_W: f64 = DETAIL_MAX_W;
/// Safety columns reserved for scrollers and the font's actual advance width.
const DETAIL_CODE_WRAP_SAFETY: usize = 4;
/// vertical safety margin for the detail panel.
const DETAIL_SCREEN_MARGIN: f64 = 8.0;
/// Match the detail body's minimum height to one history-list row; toolbar and footer are extra.
const DETAIL_TEXT_MIN_H: f64 = ROW_H;
const DETAIL_PANEL_MIN_H: f64 = DETAIL_TEXT_MIN_H + DETAIL_CHROME_H;
/// max inner image width after fixed-panel padding.
const DETAIL_IMAGE_MAX_W: f64 = DETAIL_MAX_W - DETAIL_PAD * 2.0;
/// Detail preview max edge (px): ~89% native density on a retina 640pt panel; generated
/// once on the first detail open and cached as `{hash}.detail`, never held in RAM (RAM
/// still keeps only the 480px thumbnail).
const DETAIL_PREVIEW_MAX_DIM: f64 = 1280.0;
/// Open/close duration for the detail panel; scoped to the clipboard detail window.
const DETAIL_PANEL_ANIMATION_DURATION: f64 = 0.24;
/// Initial horizontal content offset while the panel expands from left to right.
const DETAIL_CONTENT_ANIMATION_OFFSET: f64 = 10.0;
/// history, newest first.
static CLIP_HISTORY: LazyLock<Mutex<Vec<ClipEntry>>> = LazyLock::new(|| Mutex::new(Vec::new()));
/// Clipboard ledger for the memory sampler. Original image bytes live in the disk cache and
/// are excluded from resident memory; `resident_bytes` estimates structs and buffer capacity.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct HistoryStats {
    pub(crate) entries: usize,
    pub(crate) resident_bytes: u64,
    pub(crate) text_bytes: u64,
    pub(crate) preview_bytes: u64,
    pub(crate) metadata_bytes: u64,
}
/// Hold the lock only while reading these counters.
pub(crate) fn history_stats() -> HistoryStats {
    let history = CLIP_HISTORY.lock().unwrap();
    let mut stats = HistoryStats {
        entries: history.len(),
        ..HistoryStats::default()
    };
    for entry in history.iter() {
        stats.resident_bytes += estimated_entry_bytes(entry);
        stats.text_bytes += entry.text.capacity() as u64;
        stats.metadata_bytes += entry.source_app.capacity() as u64;
        stats.metadata_bytes += entry.source_key.capacity() as u64;
        if let Some(image) = &entry.image {
            stats.metadata_bytes += image.uti.capacity() as u64;
            stats.metadata_bytes += image.data_path.capacity() as u64;
            if let Some(source_path) = &image.source_path {
                stats.metadata_bytes += source_path.capacity() as u64;
            }
            stats.preview_bytes += image.preview_png.capacity() as u64;
        }
    }
    stats
}
/// Estimate one entry's resident memory: the inline structs plus the capacities of dynamic
/// fields. Original image bytes on disk are intentionally excluded; only Vec/String buffers
/// currently held by this process are counted.
fn estimated_entry_bytes(entry: &ClipEntry) -> u64 {
    let mut bytes = std::mem::size_of::<ClipEntry>() as u64
        + entry.text.capacity() as u64
        + entry.source_app.capacity() as u64
        + entry.source_key.capacity() as u64;
    if let Some(image) = &entry.image {
        bytes += image.uti.capacity() as u64;
        bytes += image.data_path.capacity() as u64;
        bytes += image.preview_png.capacity() as u64;
        if let Some(source_path) = &image.source_path {
            bytes += source_path.capacity() as u64;
        }
    }
    bytes
}
/// last observed changeCount (read only on change).
static LAST_CHANGE_COUNT: LazyLock<Mutex<i64>> = LazyLock::new(|| Mutex::new(-1));
/// One-shot suppression target for "paste and delete" write-backs. An exact count handles
/// synchronous notifications; if notification delivery is delayed or counts jump, the
/// paste marker is used as a fallback so the deleted entry cannot be resurrected.
static PASTE_DELETE_SUPPRESS_CC: LazyLock<Mutex<Option<i64>>> = LazyLock::new(|| Mutex::new(None));
/// Pending system-pasteboard clear task: records the post-write changeCount so the delayed
/// callback can confirm that our content is still present.
static PENDING_SYSTEM_PASTEBOARD_CLEAR: LazyLock<Mutex<Option<i64>>> =
    LazyLock::new(|| Mutex::new(None));
const SYSTEM_PASTEBOARD_CLEAR_DELAY: f64 = 0.35;
/// Arm the paste-and-delete suppression: MUST be called BEFORE the write-back -- if the
/// pasteboard-change notification re-enters the poll synchronously, the suppression has
/// to be in place already. The target is the current changeCount + 1; the paste marker is
/// the fallback when notification delivery observes a different count.
pub(super) fn arm_paste_delete_suppression() {
    let mut suppression = PASTE_DELETE_SUPPRESS_CC.lock().unwrap();
    let cc: i64 = unsafe {
        let pb: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
        if pb.is_null() {
            *suppression = None;
            return;
        }
        msg_send![pb, changeCount]
    };
    *suppression = Some(cc + 1);
}
/// Disarm (call when the write-back failed or never happened, so the suppression cannot
/// swallow the next genuine copy).
pub(super) fn disarm_paste_delete_suppression() {
    *PASTE_DELETE_SUPPRESS_CC.lock().unwrap() = None;
}
/// Suppression verdict (pure, unit-tested): an exact count hit or our marker still being
/// present identifies our own write-back.
fn paste_delete_suppression_hit(stored: Option<i64>, cc: i64, marker_present: bool) -> bool {
    stored == Some(cc) || (stored.is_some() && marker_present)
}
/// the polling timer (main thread).
static POLL_TIMER: OnceLock<MainThreadSlot<ObjPtr>> = OnceLock::new();
/// whether the picker is visible.
static PICKER_VISIBLE: AtomicBool = AtomicBool::new(false);
/// whether a clipboard-history UI refresh is already queued.
static PICKER_REFRESH_PENDING: AtomicBool = AtomicBool::new(false);
/// whether a search-triggered row rebuild is queued.
static PICKER_SEARCH_REFRESH_PENDING: AtomicBool = AtomicBool::new(false);
/// Monotonic history-model version; row-snapshot comparisons avoid rescanning text and preview bytes.
static HISTORY_REVISION: AtomicU64 = AtomicU64::new(0);
pub(super) fn history_revision() -> u64 {
    HISTORY_REVISION.load(Ordering::Relaxed)
}
pub(super) fn bump_history_revision() {
    HISTORY_REVISION.fetch_add(1, Ordering::Relaxed);
}
/// whether a virtual-row viewport refresh is already queued.
static PICKER_VISIBLE_ROWS_REFRESH_PENDING: AtomicBool = AtomicBool::new(false);
/// Sentinel for "no selected row": used while the search field is focused (↑ from the list
/// top into the search field, or a click on it), so no row keeps its highlight; ↓ back into
/// the list resets it to 0 in search_field_do_command.
const NO_SELECTION: usize = usize::MAX;
/// Clipboard picker interaction state is main-thread owned; background monitors deliver
/// data through the existing entry points instead of touching this state directly.
struct ClipboardUiState {
    picker_selection: usize,
    search_query: String,
    filtered: Vec<usize>,
    detail_visible: bool,
    rendered_rows: Option<PickerRowsKey>,
    last_rebuild_timing: Option<PickerTimingSummary>,
}
const CLIPBOARD_SLOW_PATH_MS: u128 = 100;
#[derive(Clone, Copy, Debug, Default)]
struct PickerTimingSummary {
    elapsed_ms: u128,
    history_len: usize,
    filtered_len: usize,
    image_rows: usize,
    code_rows: usize,
    remove_old_ms: u128,
    prepare_ms: u128,
    build_rows_ms: u128,
    image_ms: u128,
    content_attributed_ms: u128,
    meta_ms: u128,
    finalize_ms: u128,
    slowest_row_ms: u128,
    slowest_row_index: Option<usize>,
    empty: bool,
}
/// Snapshot of the inputs represented by the current row views; an unchanged snapshot lets a
/// subsequent summon show the existing AppKit views without rebuilding them.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PickerRowsKey {
    history_signature: u64,
    filter: ClipFilter,
    query: String,
    show_source: bool,
    minute_bucket: u64,
}
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct ContentAttributedKey {
    content: String,
    kind: u8,
    primary_text: u32,
    secondary_text: u32,
    accent: u32,
}
static CONTENT_ATTRIBUTED_CACHE: LazyLock<
    MainThreadSlot<HashMap<ContentAttributedKey, CachedUiObject>>,
> = LazyLock::new(|| MainThreadSlot::new(HashMap::new()));
const UI_CACHE_CAPACITY: usize = 128;
static UI_CACHE_RECENCY: AtomicU64 = AtomicU64::new(0);
#[derive(Clone, Copy)]
struct CachedUiObject {
    object: ObjPtr,
    last_used: u64,
}
fn picker_rows_key(
    revision: u64,
    query: &str,
    filter: ClipFilter,
    show_source: bool,
) -> PickerRowsKey {
    PickerRowsKey {
        history_signature: revision,
        filter,
        query: query.to_string(),
        show_source,
        minute_bucket: now_secs() / 60,
    }
}
thread_local! {
    static CLIPBOARD_UI: RefCell<ClipboardUiState> = const { RefCell::new(ClipboardUiState {
        picker_selection: 0,
        search_query: String::new(),
        filtered: Vec::new(),
        detail_visible: false,
        rendered_rows: None,
        last_rebuild_timing: None,
    }) };
}
fn with_clipboard_ui<R>(f: impl FnOnce(&mut ClipboardUiState) -> R) -> R {
    #[cfg(not(test))]
    crate::debug_assert_main_thread();
    CLIPBOARD_UI.with(|ui| f(&mut ui.borrow_mut()))
}
fn picker_selection() -> usize {
    with_clipboard_ui(|ui| ui.picker_selection)
}
fn set_picker_selection(selection: usize) {
    with_clipboard_ui(|ui| ui.picker_selection = selection);
}
fn detail_visible() -> bool {
    with_clipboard_ui(|ui| ui.detail_visible)
}
fn set_detail_visible(visible: bool) {
    with_clipboard_ui(|ui| ui.detail_visible = visible);
}
fn take_detail_visible() -> bool {
    with_clipboard_ui(|ui| {
        let visible = ui.detail_visible;
        ui.detail_visible = false;
        visible
    })
}
/// The row currently under the cursor (shows the faint hover backdrop; independent of the
/// selection -- with keyboard navigation the mouse may park on another row). NO_SELECTION
/// when nothing is hovered. Maintained by mouseEntered/mouseExited.
static HOVER_ROW: Mutex<usize> = Mutex::new(NO_SELECTION);
/// Incremental visual views for materialized rows (tile, selection bar + 3 action buttons);
/// ROW_VIEW_INDICES maps them back to the full display list.
#[derive(Clone, Copy)]
struct RowHoverViews {
    group_label: Option<ObjPtr>,
    tile: ObjPtr,
    bar: ObjPtr,
    content: ObjPtr,
    meta: ObjPtr,
    pin: ObjPtr,
    details: ObjPtr,
    del: ObjPtr,
}
static ROW_HOVER_VIEWS: MainThreadSlot<Vec<RowHoverViews>> = MainThreadSlot::new(Vec::new());
/// Display indices for materialized row views; rows outside the viewport have no AppKit views.
static ROW_VIEW_INDICES: MainThreadSlot<Vec<usize>> = MainThreadSlot::new(Vec::new());
/// the picker window.
static PICKER_WINDOW: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
/// the picker container (receives key events).
static PICKER_CONTAINER: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
/// Copy the container pointer and end the slot borrow immediately; never hold a
/// MainThreadSlot RefMut across an AppKit call.
fn picker_container_ptr() -> Option<*mut AnyObject> {
    PICKER_CONTAINER
        .lock()
        .unwrap()
        .map(|container| container.0)
}
/// the localized footer in place.
static PICKER_CONTENT_PARENT: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
/// The macOS 26+ picker glass view, used for live tint/style preview updates.
static PICKER_GLASS: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
/// The empty-state hint view (shown when the history is empty / nothing matches). Tracked
/// separately from row views so the next rebuild can remove it.
static EMPTY_STATE_VIEW: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
/// Process-lifetime cache for clipboard source icons; avoids decoding the same small icon from
/// disk again on every row rebuild.
struct CachedSourceIcon {
    image: ObjPtr,
    modified: Option<std::time::SystemTime>,
}
static SOURCE_ICON_CACHE: LazyLock<MainThreadSlot<HashMap<(String, u64), CachedSourceIcon>>> =
    LazyLock::new(|| MainThreadSlot::new(HashMap::new()));
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct RowImageKey {
    image_hash: u64,
    field_bg: u32,
    card_border: u32,
}
static ROW_IMAGE_CACHE: LazyLock<MainThreadSlot<HashMap<RowImageKey, CachedUiObject>>> =
    LazyLock::new(|| MainThreadSlot::new(HashMap::new()));
fn next_ui_cache_recency() -> u64 {
    UI_CACHE_RECENCY.fetch_add(1, Ordering::Relaxed) + 1
}
/// varies with the wrapped line count).
static ROW_PITCHES: LazyLock<Mutex<Vec<f64>>> = LazyLock::new(|| Mutex::new(Vec::new()));
/// The filter pills' button pointers (one per tag; restyled on change/rebuild).
static FILTER_PILLS: MainThreadSlot<Vec<ObjPtr>> = MainThreadSlot::new(Vec::new());
/// Persistent clear-history action buttons, relaid out when the locale changes.
static CLEAR_HISTORY_ACTION_BUTTONS: MainThreadSlot<Option<[ObjPtr; 2]>> =
    MainThreadSlot::new(None);
const CLIPBOARD_UNDO_WINDOW: Duration = Duration::from_secs(30);
fn clipboard_undo_expired(expires_at: Instant, now: Instant) -> bool {
    expires_at <= now
}
fn is_clipboard_undo_shortcut(keycode: u16, modifiers: u64) -> bool {
    const COMMAND: u64 = 0x0010_0000;
    const SECONDARY: u64 = 0x000E_0000;
    keycode == 6 && (modifiers & COMMAND) != 0 && (modifiers & SECONDARY) == 0
}
struct DeletedClipboardEntry {
    entry: ClipEntry,
    original_index: usize,
    expires_at: Instant,
    generation: u64,
}
static DELETED_CLIPBOARD_ENTRY: LazyLock<Mutex<Option<DeletedClipboardEntry>>> =
    LazyLock::new(|| Mutex::new(None));
static DELETED_CLIPBOARD_GENERATION: AtomicU64 = AtomicU64::new(0);
/// Legacy confirmation views retained for compatibility with the existing collapse animation.
/// The normal picker now uses persistent action buttons and never creates this card.
#[derive(Clone, Copy)]
struct ClearHistoryConfirmationViews {
    surface: ObjPtr,
    unpinned: ObjPtr,
    all: ObjPtr,
}
static CLEAR_HISTORY_BUTTON: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
static CLEAR_HISTORY_CONFIRMATION: MainThreadSlot<Option<ClearHistoryConfirmationViews>> =
    MainThreadSlot::new(None);
static CLEAR_HISTORY_CONFIRMATION_EXPANDED: AtomicBool = AtomicBool::new(false);
/// The active filter's underline (one shared view, moved under the active item).
static FILTER_UNDERLINE: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
fn localized_filter_labels() -> [String; 5] {
    [
        t("clipboard.filter_all"),
        t("clipboard.filter_text"),
        t("clipboard.filter_image"),
        t("clipboard.filter_link"),
        t("clipboard.filter_code"),
    ]
}
/// the top search field.
static SEARCH_FIELD: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
/// The search field clear × hover state.
static SEARCH_CLEAR_HOVERED: AtomicBool = AtomicBool::new(false);
/// The real click button over the hand-drawn ×; rendering remains in the cell.
static SEARCH_CLEAR_BUTTON: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
/// The centered "magnifier + search hint" attributed string (hand-drawn; the field itself
/// carries NO placeholder property, so the field editor never draws the placeholder
/// left-aligned on a focused-but-empty field).
static SEARCH_HINT_TEXT: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
/// Rebuild the search field's placeholder (magnifier + "Search clipboard", 15pt): the
/// placeholder is NOT set on the field (the field editor would draw it left-aligned on a
/// focused-but-empty field); it lives in a static and is hand-drawn at the field's left
/// (see search_cell_draw_interior).
unsafe fn rebuild_search_hint() {
    // The icon is the ⌕ text glyph (U+2315, matching the HTML's .search-icon), appended
    // as an 18pt / 42% black run before the 14pt / 40% black placeholder text.
    let ph_m: *mut AnyObject = msg_send![class!(NSMutableAttributedString), alloc];
    let empty_ns2 = make_nsstring("");
    let ph_m: *mut AnyObject = msg_send![ph_m, initWithString: empty_ns2];
    CFRelease(empty_ns2 as *const c_void);
    // the icon run.
    let icon_attrs: *mut AnyObject = msg_send![class!(NSMutableDictionary), alloc];
    let icon_attrs: *mut AnyObject = msg_send![icon_attrs, init];
    let icon_font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 18.0f64];
    let icon_color = crate::ffi::hex_to_ns_color(clipboard_palette().muted_text);
    let font_key = make_nsstring("NSFont");
    let color_key = make_nsstring("NSColor");
    let _: () = msg_send![icon_attrs, setObject: icon_font, forKey: font_key];
    let _: () = msg_send![icon_attrs, setObject: icon_color, forKey: color_key];
    CFRelease(font_key as *const c_void);
    CFRelease(color_key as *const c_void);
    let icon_ns = make_nsstring("\u{2315}  ");
    let icon_part: *mut AnyObject = msg_send![class!(NSAttributedString), alloc];
    let icon_part: *mut AnyObject =
        msg_send![icon_part, initWithString: icon_ns, attributes: icon_attrs];
    CFRelease(icon_ns as *const c_void);
    release_obj(icon_attrs);
    let _: () = msg_send![ph_m, appendAttributedString: icon_part];
    release_obj(icon_part);
    // the placeholder run.
    let ph_text_attrs: *mut AnyObject = msg_send![class!(NSMutableDictionary), alloc];
    let ph_text_attrs: *mut AnyObject = msg_send![ph_text_attrs, init];
    let font_key = make_nsstring("NSFont");
    let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 14.0f64];
    let _: () = msg_send![ph_text_attrs, setObject: font, forKey: font_key];
    CFRelease(font_key as *const c_void);
    let color_key = make_nsstring("NSColor");
    // Placeholder text follows the same muted role used by settings labels.
    let ph_color = crate::ffi::hex_to_ns_color(clipboard_palette().muted_text);
    let _: () = msg_send![ph_text_attrs, setObject: ph_color, forKey: color_key];
    CFRelease(color_key as *const c_void);
    let ph_ns = make_nsstring(&t("clipboard.search_placeholder"));
    let ph_text: *mut AnyObject = msg_send![class!(NSAttributedString), alloc];
    let ph_text: *mut AnyObject =
        msg_send![ph_text, initWithString: ph_ns, attributes: ph_text_attrs];
    CFRelease(ph_ns as *const c_void);
    release_obj(ph_text_attrs);
    let _: () = msg_send![ph_m, appendAttributedString: ph_text];
    release_obj(ph_text);
    let mut hint = SEARCH_HINT_TEXT.lock().unwrap();
    if let Some(old) = *hint {
        release_obj(old.0);
    }
    *hint = Some(ObjPtr::new(ph_m));
}
/// The current display list: history indices (filtered order). All indices when no query.
/// The entry pending save-as: detail_save_as_action runs inside the button's mouseDown
/// tracking loop, and a runModal nested loop must not start in that context (the panel's
/// name field never gets keyboard focus). The action stashes the entry here and hops to
/// the next runloop turn via performSelectorOnMainThread; detail_save_as_deferred takes
/// it and runs the save with tracking unwound. At most one pending entry at a time (the
/// modal blocks input while up).
static PENDING_SAVE_AS: Mutex<Option<ClipEntry>> = Mutex::new(None);
/// the scroll view.
static SCROLL_VIEW: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
/// the custom scroll indicator view.
static SCROLL_INDICATOR: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
/// The detail text scroll view and its custom indicator; image details have no scroll area.
static DETAIL_SCROLL_VIEW: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
static DETAIL_SCROLL_INDICATOR: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
static DETAIL_HORIZONTAL_SCROLL_INDICATOR: MainThreadSlot<Option<ObjPtr>> =
    MainThreadSlot::new(None);
/// Drag state for the custom scroll indicator; once the system scroller is disabled, an
/// NSView does not implement thumb dragging for us.
#[derive(Clone, Copy)]
enum ScrollTarget {
    Picker,
    Detail,
    DetailHorizontal,
}
#[derive(Clone, Copy)]
struct ScrollDragState {
    target: ScrollTarget,
    start_axis: f64,
    start_offset: f64,
    max_offset: f64,
    thumb_travel: f64,
}
static SCROLL_DRAG: Mutex<Option<ScrollDragState>> = Mutex::new(None);
/// the detail panel window (right-arrow expands).
static DETAIL_WINDOW: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
/// The macOS 26+ detail glass view and its inactive compensation layer.
static DETAIL_GLASS: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
static DETAIL_GLASS_FILL_LAYER: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
/// Keep clipboard panels on the same resolved appearance as the settings window and switcher.
unsafe fn apply_panel_appearance(window: *mut AnyObject) {
    let name = make_nsstring(if resolved_is_dark() {
        "NSAppearanceNameDarkAqua"
    } else {
        "NSAppearanceNameAqua"
    });
    let appearance: *mut AnyObject = msg_send![class!(NSAppearance), appearanceNamed: name];
    CFRelease(name as *const c_void);
    if !appearance.is_null() {
        let _: () = msg_send![window, setAppearance: appearance];
    }
}
/// The detail panel's content container (hosts the text scroll view / the image view;
/// clicking anywhere on the panel dismisses it).
static DETAIL_CONTENT: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
/// The single slot for a freshly generated <=1280px hi-res detail preview (hash, bytes).
//  First priority in show_detail_for_sel's byte lookup -- consumed (cleared) on a hash
//  match so the disk is never re-read; misses fall through to the disk cache / 480px
//  preview. Bounded to one entry (~2MB), overwritten by the next delivery; stale content
//  is dropped by detail_preview_ready.
static DETAIL_PENDING_HD: Mutex<Option<(u64, Vec<u8>)>> = Mutex::new(None);
/// In-flight detail-preview generation jobs (hash set): shared by record-time pregen and
/// first-open on-demand requests so the same content never queues twice (arrow-key bounce,
/// record+open races). The worker removes an entry once its job finishes (including skips).
static DETAIL_INFLIGHT: LazyLock<Mutex<HashSet<u64>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
/// Code-detail soft-wrap toggle, retained for the session; when off, raw text uses the native
/// horizontal scroller.
static DETAIL_SOFT_WRAP_ENABLED: AtomicBool = AtomicBool::new(true);
/// The picker's origin before opening detail; restored on close while preserving any height
/// changes that may have occurred while the detail was open.
static DETAIL_PICKER_ORIGINAL_ORIGIN: Mutex<Option<NSPoint>> = Mutex::new(None);
/// The detail panel's current text view (selectable; feeds "copy selection"). MUST be
/// cleared when the old content is removed / the panel hides, or a dangling pointer would
/// be dereferenced on Cmd+C (use-after-free).
static DETAIL_TEXT_VIEW: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
/// The code text view using soft-wrap decorations; arrows are drawing-only, while U+2028
/// separators are stripped through the source map.
static DETAIL_SOFT_WRAP_TEXT_VIEW: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
/// Shared mapping from detail display text to source, ensuring U+2028 and other display-only
/// characters never enter copied content.
static DETAIL_SOURCE_MAP: Mutex<Option<Arc<DisplaySourceMap>>> = Mutex::new(None);
/// A row rebuild is in progress: rows added during a rebuild dispatch mouseEntered
/// immediately when the cursor happens to be inside (ActiveInKeyWindow + InVisibleRect
/// tracking areas), and a handler that re-triggers rebuild_rows would recurse forever
/// (reproducible via keyboard navigation once the window is key; the process used to hang).
/// mouseEntered events dispatched during a rebuild are ignored; real cursor movement after
/// the rebuild is handled normally.
static REBUILDING: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
mod tests {
    use super::{
        clear_history_confirmation_layout, effective_hover_row, estimated_entry_bytes,
        header_strip_h, rect_contains_point, scroll_indicator_geometry, ClipEntry, ImageEntry,
        NO_SELECTION, NSPASTEBOARD_TYPE_PNG, SCROLL_INDICATOR_CORNER_RESERVE,
        SCROLL_INDICATOR_EDGE,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use std::sync::Arc;
    #[test]
    fn clear_confirmation_buttons_are_compact_and_horizontal() {
        let anchor = NSRect::new(NSPoint::new(420.0, 66.0), NSSize::new(60.0, 20.0));
        let (surface, buttons) = clear_history_confirmation_layout(anchor);
        assert!(buttons[0].origin.x < buttons[1].origin.x);
        assert_eq!(buttons[0].origin.y, buttons[1].origin.y);
        assert_eq!(
            buttons[1].origin.x - (buttons[0].origin.x + buttons[0].size.width),
            super::CLEAR_CONFIRM_GAP
        );
        for button in buttons {
            assert!(button.origin.x >= 0.0);
            assert!(button.origin.x + button.size.width <= surface.size.width);
            assert!(button.origin.y + button.size.height <= surface.size.height);
        }
        let surface_bottom = surface.origin.y + surface.size.height;
        assert!(surface.origin.x + surface.size.width <= anchor.origin.x + anchor.size.width);
        assert!(surface.origin.y >= anchor.origin.y);
        assert!(surface_bottom > header_strip_h());
    }
    #[test]
    fn row_hover_hit_test_includes_edges_and_rejects_padding_outside() {
        let rect = NSRect::new(NSPoint::new(10.0, 20.0), NSSize::new(100.0, 40.0));
        assert!(rect_contains_point(rect, NSPoint::new(10.0, 20.0)));
        assert!(rect_contains_point(rect, NSPoint::new(110.0, 60.0)));
        assert!(rect_contains_point(rect, NSPoint::new(55.0, 35.0)));
        assert!(!rect_contains_point(rect, NSPoint::new(9.9, 35.0)));
        assert!(!rect_contains_point(rect, NSPoint::new(55.0, 60.1)));
    }
    #[test]
    fn estimated_entry_bytes_counts_capacity_once() {
        let mut text = String::with_capacity(32);
        text.push_str("hello");
        let entry = ClipEntry {
            text,
            image: None,
            pinned: false,
            source_app: String::with_capacity(16),
            source_key: String::with_capacity(24),
            copied_at: None,
        };
        let expected = std::mem::size_of::<ClipEntry>() as u64
            + entry.text.capacity() as u64
            + entry.source_app.capacity() as u64
            + entry.source_key.capacity() as u64;
        assert_eq!(estimated_entry_bytes(&entry), expected);
        let image = ImageEntry {
            uti: String::with_capacity(16),
            hash: 1,
            data_path: std::path::PathBuf::from("/tmp/image"),
            preview_png: Arc::new(Vec::with_capacity(64)),
            source_path: Some(String::with_capacity(32)),
        };
        let image_entry = ClipEntry {
            text: String::new(),
            image: Some(image),
            pinned: false,
            source_app: String::new(),
            source_key: String::new(),
            copied_at: None,
        };
        let image = image_entry.image.as_ref().unwrap();
        let expected = std::mem::size_of::<ClipEntry>() as u64
            + image_entry.text.capacity() as u64
            + image_entry.source_app.capacity() as u64
            + image_entry.source_key.capacity() as u64
            + image.uti.capacity() as u64
            + image.data_path.capacity() as u64
            + image.preview_png.capacity() as u64
            + image.source_path.as_ref().unwrap().capacity() as u64;
        assert_eq!(estimated_entry_bytes(&image_entry), expected);
    }
    #[test]
    fn detail_scroll_geometry_keeps_a_fixed_lower_right_reserve() {
        let visible = 100.0;
        let document = 400.0;
        let max_offset = document - visible;
        let (position, length) = scroll_indicator_geometry(
            visible,
            document,
            max_offset,
            SCROLL_INDICATOR_CORNER_RESERVE,
        )
        .expect("overflowing detail content must produce a thumb");
        let end = position + length;
        let expected_end = visible - SCROLL_INDICATOR_EDGE - SCROLL_INDICATOR_CORNER_RESERVE;
        assert!((end - expected_end).abs() < f64::EPSILON);
    }
    //  Truth table for the hi-res detail freshness predicate: swap into the UI only when
    //  the panel is visible AND the selected entry is the job's entry; everything else
    //  (panel closed / navigated away / no selection) drops the result.
    #[test]
    fn detail_result_still_wanted_truth_table() {
        use super::detail_result_still_wanted as wanted;
        assert!(wanted(true, Some(42), 42));
        // visible but navigated to another entry.
        assert!(!wanted(true, Some(7), 42));
        // no selection (search-field focus).
        assert!(!wanted(true, None, 42));
        // panel closed (incl. hidden with the picker).
        assert!(!wanted(false, Some(42), 42));
    }
    //  Truth table for the hover gate: pointer inside -> keep the hover index as-is
    //  (including the no-selection sentinel); pointer outside -> always reset to
    //  NO_SELECTION (the phantom hover fill fix).
    #[test]
    fn effective_hover_row_requires_pointer_inside() {
        assert_eq!(effective_hover_row(true, 3), 3);
        assert_eq!(effective_hover_row(true, 0), 0);
        assert_eq!(effective_hover_row(true, NO_SELECTION), NO_SELECTION);
        assert_eq!(effective_hover_row(false, 0), NO_SELECTION);
        assert_eq!(effective_hover_row(false, 7), NO_SELECTION);
        assert_eq!(effective_hover_row(false, NO_SELECTION), NO_SELECTION);
    }
    #[test]
    fn picker_row_visibility_keeps_overscan_rows_drawable() {
        use super::picker_row_is_drawable;
        let viewport = NSRect::new(NSPoint::new(0.0, 100.0), NSSize::new(560.0, 600.0));
        let overscan = 117.0;
        assert!(picker_row_is_drawable(
            NSRect::new(NSPoint::new(0.0, -95.0), NSSize::new(560.0, 78.0)),
            viewport,
            overscan
        ));
        assert!(!picker_row_is_drawable(
            NSRect::new(NSPoint::new(0.0, -96.0), NSSize::new(560.0, 78.0)),
            viewport,
            overscan
        ));
        assert!(picker_row_is_drawable(
            NSRect::new(NSPoint::new(0.0, 817.0), NSSize::new(560.0, 78.0)),
            viewport,
            overscan
        ));
        assert!(!picker_row_is_drawable(
            NSRect::new(NSPoint::new(0.0, 818.0), NSSize::new(560.0, 78.0)),
            viewport,
            overscan
        ));
    }
    #[test]
    fn picker_visible_range_materializes_only_viewport_rows_with_overscan() {
        use super::picker_visible_range;
        let pitches = vec![100.0; 10];
        let viewport = NSRect::new(NSPoint::new(0.0, 350.0), NSSize::new(560.0, 100.0));
        assert_eq!(picker_visible_range(&pitches, viewport, 50.0), (2, 5));
    }
    #[test]
    fn oldest_pending_hash_evicts_by_insertion_order_not_map_order() {
        use super::{oldest_pending_hash, PendingImage};
        use std::collections::HashMap;
        assert_eq!(oldest_pending_hash(&HashMap::new()), None);
        // Insertion is unrelated to HashMap bucket order: seq-minimal 3 is the oldest.
        let mut pending = HashMap::new();
        for (hash, seq) in [(7u64, 2u64), (3, 1), (9, 3)] {
            pending.insert(
                hash,
                PendingImage {
                    seq,
                    bytes: Arc::new(vec![hash as u8]),
                },
            );
        }
        assert_eq!(oldest_pending_hash(&pending), Some(3));
        pending.remove(&3);
        assert_eq!(oldest_pending_hash(&pending), Some(7));
    }
    #[test]
    fn picker_rows_key_changes_when_rendered_inputs_change() {
        use super::{picker_rows_key, ClipFilter};
        let base = picker_rows_key(1, "", ClipFilter::All, false);
        let same = picker_rows_key(1, "", ClipFilter::All, false);
        assert_eq!(
            base.history_signature, same.history_signature,
            "unchanged history should have the same render signature"
        );
        assert_eq!(base.filter, same.filter);
        assert_eq!(base.query, same.query);
        assert_eq!(base.show_source, same.show_source);
        assert_ne!(base, picker_rows_key(2, "", ClipFilter::All, false));
        assert_ne!(base, picker_rows_key(1, "query", ClipFilter::All, false));
        assert_ne!(base, picker_rows_key(1, "", ClipFilter::Text, false));
        assert_ne!(base, picker_rows_key(1, "", ClipFilter::All, true));
    }
    /// A 3-arg convenience wrapper for tests (empty source and icon key; existing cases are
    /// unaffected by the signature change).
    fn record_text(h: &mut Vec<ClipEntry>, text: &str, max: usize) -> bool {
        super::record_text(h, text, "", "", max)
    }
    fn entry(text: &str) -> ClipEntry {
        ClipEntry {
            text: text.to_string(),
            image: None,
            pinned: false,
            source_app: String::new(),
            source_key: String::new(),
            copied_at: None,
        }
    }
    fn entry_with_source(text: &str, source: &str) -> ClipEntry {
        ClipEntry {
            text: text.to_string(),
            image: None,
            pinned: false,
            source_app: source.to_string(),
            source_key: String::new(),
            copied_at: None,
        }
    }
    /// A test image entry: the bytes are written into the TEST cache dir and referenced,
    /// mirroring the real record path (the preview shares the small byte set). No file
    /// source.
    fn image(data: &[u8]) -> ImageEntry {
        let hash = super::fnv1a64(data);
        assert!(
            super::cache_write_image(hash, data),
            "test cache write must succeed"
        );
        ImageEntry {
            uti: NSPASTEBOARD_TYPE_PNG.to_string(),
            hash,
            data_path: super::clip_image_path(hash),
            preview_png: Arc::new(data.to_vec()),
            source_path: None,
        }
    }
    /// A test FILE-COPY entry: bytes -> content hash + preview (data doubles as the
    /// preview), data_path always empty, only the source path is carried -- same as the
    /// real file-copy path (bytes are never stored).
    fn image_from_file(data: &[u8], path: &str) -> ImageEntry {
        ImageEntry {
            uti: NSPASTEBOARD_TYPE_PNG.to_string(),
            hash: super::fnv1a64(data),
            data_path: std::path::PathBuf::new(),
            preview_png: Arc::new(data.to_vec()),
            source_path: Some(path.to_string()),
        }
    }
    /// an image entry for tests.
    fn entry_image(png: &[u8]) -> ClipEntry {
        ClipEntry {
            text: String::new(),
            image: Some(image(png)),
            pinned: false,
            source_app: "Safari".to_string(),
            source_key: "com.apple.Safari".to_string(),
            copied_at: None,
        }
    }
    fn texts(h: &[ClipEntry]) -> Vec<String> {
        h.iter().map(|e| e.text.clone()).collect()
    }
    #[test]
    fn empty_text_is_ignored() {
        let mut h = vec![entry("a")];
        assert!(!record_text(&mut h, "", 50));
        assert_eq!(h.len(), 1);
    }
    #[test]
    fn duplicate_is_moved_to_front_not_duplicated() {
        // Full-list dedup: re-copying an existing text moves the old entry to the front
        // instead of adding a duplicate.
        let mut h = vec![entry("a"), entry("b")];
        // Copy "a" (already at the tail) -> moved to the front.
        assert!(record_text(&mut h, "a", 50));
        assert_eq!(texts(&h), vec!["a", "b"]);
        // Re-copying the same text again: no move, no duplicate.
        assert!(record_text(&mut h, "a", 50));
        assert_eq!(texts(&h), vec!["a", "b"]);
        assert_eq!(h.len(), 2);
    }
    #[test]
    fn dedup_updates_the_source_to_the_latest_copy() {
        // Re-copying the same text from another app: the dedup move updates the source to the
        // latest copy's app.
        let mut h = Vec::new();
        super::record_text(&mut h, "token", "Safari", "com.apple.Safari", 50);
        assert_eq!(h[0].source_app, "Safari");
        assert_eq!(h[0].source_key, "com.apple.Safari");
        super::record_text(&mut h, "token", "Chrome", "com.google.Chrome", 50);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].source_app, "Chrome");
        // The dedup move also updates the icon key to the latest source.
        assert_eq!(h[0].source_key, "com.google.Chrome");
    }
    #[test]
    fn record_keeps_the_source_and_pin_moves_preserve_it() {
        use super::pin_entry;
        // The source travels with the entry: pin/move operations preserve it.
        let mut h = Vec::new();
        super::record_text(&mut h, "A", "Safari", "com.apple.Safari", 50);
        super::record_text(&mut h, "B", "Chrome", "com.google.Chrome", 50);
        pin_entry(&mut h, 0);
        assert_eq!(h[0].text, "B");
        assert_eq!(h[0].source_app, "Chrome");
        assert_eq!(h[0].source_key, "com.google.Chrome");
        assert_eq!(h[1].source_app, "Safari");
    }
    #[test]
    fn fnv1a64_is_stable_and_distinct() {
        use super::fnv1a64;
        // same input -> same hash.
        assert_eq!(fnv1a64(b"png-a"), fnv1a64(b"png-a"));
        // different inputs (even one byte) differ.
        assert_ne!(fnv1a64(b"png-a"), fnv1a64(b"png-b"));
        assert_ne!(fnv1a64(b""), fnv1a64(b"x"));
    }
    #[test]
    fn is_image_extension_covers_common_formats() {
        use super::is_image_extension;
        // common image formats (case-insensitive).
        for p in [
            "/a/b/photo.png",
            "/a/b/photo.PNG",
            "/a/b/pic.jpg",
            "/a/b/pic.JPEG",
            "/a/b/anim.gif",
            "/a/b/scan.tiff",
            "/a/b/img.webp",
            "/a/b/img.heic",
            "/a/b/img.bmp",
        ] {
            assert!(is_image_extension(p), "{}", p);
        }
        // Non-images / no extension / a directory are excluded.
        assert!(!is_image_extension("/a/b/doc.pdf"));
        assert!(!is_image_extension("/a/b/notes.txt"));
        assert!(!is_image_extension("/a/b/noext"));
        assert!(!is_image_extension("/a/b/"));
    }
    #[test]
    fn ext_to_uti_maps_every_supported_extension() {
        use super::{
            ext_to_uti, NSPASTEBOARD_TYPE_BMP, NSPASTEBOARD_TYPE_GIF, NSPASTEBOARD_TYPE_HEIC,
            NSPASTEBOARD_TYPE_JPEG, NSPASTEBOARD_TYPE_PNG, NSPASTEBOARD_TYPE_TIFF,
            NSPASTEBOARD_TYPE_WEBP,
        };
        // Every supported extension maps to its UTI (case-insensitive).
        assert_eq!(ext_to_uti("/a/b/p.png"), Some(NSPASTEBOARD_TYPE_PNG));
        assert_eq!(ext_to_uti("/a/b/p.PNG"), Some(NSPASTEBOARD_TYPE_PNG));
        assert_eq!(ext_to_uti("/a/b/p.jpg"), Some(NSPASTEBOARD_TYPE_JPEG));
        assert_eq!(ext_to_uti("/a/b/p.JPEG"), Some(NSPASTEBOARD_TYPE_JPEG));
        assert_eq!(ext_to_uti("/a/b/p.gif"), Some(NSPASTEBOARD_TYPE_GIF));
        assert_eq!(ext_to_uti("/a/b/p.tiff"), Some(NSPASTEBOARD_TYPE_TIFF));
        assert_eq!(ext_to_uti("/a/b/p.tif"), Some(NSPASTEBOARD_TYPE_TIFF));
        assert_eq!(ext_to_uti("/a/b/p.webp"), Some(NSPASTEBOARD_TYPE_WEBP));
        assert_eq!(ext_to_uti("/a/b/p.heic"), Some(NSPASTEBOARD_TYPE_HEIC));
        assert_eq!(ext_to_uti("/a/b/p.heif"), Some(NSPASTEBOARD_TYPE_HEIC));
        assert_eq!(ext_to_uti("/a/b/p.bmp"), Some(NSPASTEBOARD_TYPE_BMP));
        // non-image formats don't map.
        assert_eq!(ext_to_uti("/a/b/doc.pdf"), None);
        assert_eq!(ext_to_uti("/a/b/noext"), None);
    }
    #[test]
    fn record_image_dedups_by_bytes_and_updates_source() {
        use super::record_image;
        let img_a = image(b"fake-image-bytes-a");
        let img_b = image(b"fake-image-bytes-b");
        let mut h = Vec::new();
        assert!(record_image(
            &mut h,
            &img_a,
            "Safari",
            "com.apple.Safari",
            50
        ));
        assert_eq!(h.len(), 1);
        assert!(h[0].image.is_some());
        assert!(h[0].text.is_empty());
        // Re-copying the same bytes -> dedup to the front, source updated.
        assert!(record_image(
            &mut h,
            &img_a,
            "Chrome",
            "com.google.Chrome",
            50
        ));
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].source_app, "Chrome");
        // Different bytes -> a new entry (newest first).
        assert!(record_image(
            &mut h,
            &img_b,
            "Safari",
            "com.apple.Safari",
            50
        ));
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].text, "");
        assert!(h[1].image.is_some());
        // empty data is ignored.
        assert!(!record_image(
            &mut h,
            &image(b""),
            "Safari",
            "com.apple.Safari",
            50
        ));
    }
    #[test]
    fn record_image_respects_the_max_cap() {
        use super::record_image;
        let mut h = Vec::new();
        for i in 0..3u8 {
            record_image(&mut h, &image(&[i]), "Safari", "com.apple.Safari", 2);
        }
        assert_eq!(h.len(), 2);
    }
    #[test]
    fn image_cache_write_read_delete_roundtrip() {
        use super::{cache_delete_image, cache_read_image, cache_write_image, fnv1a64};
        let bytes = b"cache-roundtrip-bytes";
        let hash = fnv1a64(bytes);
        // write -> read back identical.
        assert!(cache_write_image(hash, bytes));
        assert_eq!(cache_read_image(hash).as_deref(), Some(&bytes[..]));
        // idempotent: re-writing the same hash is fine.
        assert!(cache_write_image(hash, bytes));
        // delete -> read back None.
        cache_delete_image(hash);
        assert_eq!(cache_read_image(hash), None);
    }
    #[test]
    fn delete_entry_removes_the_image_cache_file() {
        use super::{
            cache_read_detail_preview, cache_read_image, cache_write_detail_preview, delete_entry,
        };
        let bytes = b"delete-entry-cleanup";
        let img = image(bytes);
        assert!(cache_read_image(img.hash).is_some());
        // The detail preview (.detail) shares the entry's lifecycle: written here, it must
        // be removed with the entry.
        assert!(cache_write_detail_preview(
            img.hash,
            b"detail-preview-bytes"
        ));
        assert!(cache_read_detail_preview(img.hash).is_some());
        let mut h = vec![ClipEntry {
            text: String::new(),
            image: Some(img.clone()),
            pinned: false,
            source_app: String::new(),
            source_key: String::new(),
            copied_at: None,
        }];
        delete_entry(&mut h, 0);
        assert!(h.is_empty());
        // the entry is gone -> so is its cache file.
        assert_eq!(cache_read_image(img.hash), None);
        assert_eq!(cache_read_detail_preview(img.hash), None);
    }
    #[test]
    fn trim_beyond_max_deletes_dropped_image_cache_files() {
        use super::{cache_read_image, record_image};
        let mut h = Vec::new();
        // max=2: 3 images, the oldest is trimmed and its cache file must go.
        let imgs: Vec<_> = (0..3u8).map(|i| image(&[b't', i, b'x'])).collect();
        for img in &imgs {
            record_image(&mut h, img, "Safari", "com.apple.Safari", 2);
        }
        assert_eq!(h.len(), 2);
        assert_eq!(
            cache_read_image(imgs[0].hash),
            None,
            "trimmed entry's file deleted"
        );
        assert!(cache_read_image(imgs[1].hash).is_some());
        assert!(cache_read_image(imgs[2].hash).is_some());
    }
    #[test]
    fn text_record_trim_deletes_dropped_image_cache_files() {
        use super::{cache_read_image, record_image, record_text};
        let mut h = Vec::new();
        let img = image(b"text-trim-image");
        record_image(&mut h, &img, "Safari", "com.apple.Safari", 2);
        // Two text entries push the image out of max=2 -> its cache file is deleted.
        record_text(&mut h, "a", "Ghostty", "com.mitchellh.ghostty", 2);
        record_text(&mut h, "b", "Ghostty", "com.mitchellh.ghostty", 2);
        assert_eq!(h.len(), 2);
        assert!(h.iter().all(|e| e.image.is_none()));
        assert_eq!(cache_read_image(img.hash), None);
    }
    #[test]
    fn sweep_clip_image_cache_removes_orphans_and_respects_file_refs() {
        use super::{
            cache_read_detail_preview, cache_read_image, cache_read_preview,
            cache_write_detail_preview, cache_write_image, cache_write_preview,
            clear_clip_image_cache, clip_image_detail_path, clip_image_path,
            clip_image_preview_path, sweep_clip_image_cache,
        };
        clear_clip_image_cache();
        let keep = image(b"sweep-keep-data");
        let orphan = image(b"sweep-orphan-data");
        let file_bytes = b"sweep-file-reference";
        let file_img = image_from_file(file_bytes, "/tmp/sweep-file.png");
        let _ = cache_write_preview(keep.hash, b"keep-preview");
        let _ = cache_write_detail_preview(keep.hash, b"keep-detail");
        let _ = cache_write_preview(orphan.hash, b"orphan-preview");
        let _ = cache_write_detail_preview(orphan.hash, b"orphan-detail");
        let _ = cache_write_image(file_img.hash, file_bytes);
        let _ = cache_write_preview(file_img.hash, b"file-preview");
        let _ = cache_write_detail_preview(file_img.hash, b"file-detail");
        let history = vec![
            entry_image(b"sweep-keep-data"),
            ClipEntry {
                text: "sweep-file.png".to_string(),
                image: Some(file_img.clone()),
                pinned: false,
                source_app: String::new(),
                source_key: String::new(),
                copied_at: None,
            },
        ];
        assert!(sweep_clip_image_cache(&history) >= 4);
        assert!(cache_read_image(keep.hash).is_some());
        assert!(cache_read_preview(keep.hash).is_some());
        assert!(cache_read_detail_preview(keep.hash).is_some());
        assert!(cache_read_image(orphan.hash).is_none());
        assert!(cache_read_preview(orphan.hash).is_none());
        assert!(cache_read_detail_preview(orphan.hash).is_none());
        assert!(cache_read_image(file_img.hash).is_none());
        assert!(cache_read_preview(file_img.hash).is_some());
        assert!(cache_read_detail_preview(file_img.hash).is_some());
        assert!(!clip_image_path(file_img.hash).exists());
        assert!(clip_image_preview_path(file_img.hash).exists());
        assert!(clip_image_detail_path(file_img.hash).exists());
        clear_clip_image_cache();
    }
    #[test]
    fn clear_clip_image_cache_wipes_the_test_dir_only() {
        use super::{cache_read_image, cache_write_image, clear_clip_image_cache, fnv1a64};
        let a = b"wipe-test-a";
        let b = b"wipe-test-b";
        let (ha, hb) = (fnv1a64(a), fnv1a64(b));
        assert!(cache_write_image(ha, a));
        assert!(cache_write_image(hb, b));
        clear_clip_image_cache();
        assert_eq!(cache_read_image(ha), None);
        assert_eq!(cache_read_image(hb), None);
    }
    #[test]
    fn cache_preview_roundtrip_and_delete_removes_both() {
        use super::{
            cache_delete_image, cache_read_image, cache_read_preview, cache_write_image,
            cache_write_preview, fnv1a64,
        };
        let data = b"preview-test-data";
        let preview = b"fake-preview-png";
        let hash = fnv1a64(data);
        assert!(cache_write_image(hash, data));
        assert!(cache_write_preview(hash, preview));
        assert_eq!(cache_read_preview(hash).as_deref(), Some(&preview[..]));
        // deleting removes data and preview together.
        cache_delete_image(hash);
        assert_eq!(cache_read_image(hash), None);
        assert_eq!(cache_read_preview(hash), None);
    }
    #[test]
    fn history_serialize_parse_roundtrip_skips_runtime_fields() {
        use super::{fnv1a64, parse_history};
        // All three entry kinds + unicode/newline text + pinned + source survive the
        // roundtrip; runtime fields (preview_png/data_path) are NOT serialized.
        let img = image(b"history-roundtrip-img");
        let file_ref = image_from_file(
            b"history-roundtrip-file",
            "/Users/ceres/Downloads/vva划船.gif",
        );
        let entries = vec![
            ClipEntry {
                text: "密码 A\n第二行 🎉".to_string(),
                image: None,
                pinned: true,
                source_app: "1Password".to_string(),
                source_key: "com.agilebits.onepassword".to_string(),
                copied_at: None,
            },
            ClipEntry {
                text: String::new(),
                image: Some(img.clone()),
                pinned: false,
                source_app: "Safari".to_string(),
                source_key: "com.apple.Safari".to_string(),
                copied_at: None,
            },
            ClipEntry {
                text: "vva划船.gif".to_string(),
                image: Some(file_ref),
                pinned: false,
                source_app: "Finder".to_string(),
                source_key: String::new(),
                copied_at: None,
            },
        ];
        let text = super::serialize_history(&entries).expect("serialize");
        let parsed = parse_history(&text).expect("parse");
        assert_eq!(parsed.len(), 3);
        // the text entry matches fully.
        assert_eq!(parsed[0].text, "密码 A\n第二行 🎉");
        assert!(parsed[0].pinned);
        assert_eq!(parsed[0].source_app, "1Password");
        // The data entry keeps uti/hash; the preview and data_path are skipped (restore
        // fills them back).
        let p_img = parsed[1].image.as_ref().unwrap();
        assert_eq!(p_img.uti, img.uti);
        assert_eq!(p_img.hash, img.hash);
        assert!(p_img.preview_png.is_empty());
        assert!(p_img.data_path.as_os_str().is_empty());
        // the file copy keeps its path + hash.
        let p_file = parsed[2].image.as_ref().unwrap();
        assert_eq!(
            p_file.source_path.as_deref(),
            Some("/Users/ceres/Downloads/vva划船.gif")
        );
        assert_eq!(p_file.hash, fnv1a64(b"history-roundtrip-file"));
        // re-serializing is idempotent.
        assert!(super::serialize_history(&parsed).is_some());
        // The timestamp survives the roundtrip (None is skipped, Some is written).
        assert!(parsed.iter().all(|e| e.copied_at.is_none()));
        let with_ts = ClipEntry {
            text: "ts".to_string(),
            image: None,
            pinned: false,
            source_app: String::new(),
            source_key: String::new(),
            copied_at: Some(1755000000),
        };
        let parsed_ts = parse_history(&super::serialize_history(&[with_ts]).unwrap()).unwrap();
        assert_eq!(parsed_ts[0].copied_at, Some(1755000000));
    }
    #[test]
    fn load_history_keeps_distinct_data_images() {
        use super::{
            cache_write_image, clip_image_path, fnv1a64, history_file_path, load_history,
            serialize_history, ImageEntry, CLIP_HISTORY, NSPASTEBOARD_TYPE_PNG,
        };
        // Regression: DISTINCT data-image entries (web copies, source_path always None)
        // must all survive; the old all-images-by-source_path dedup (None==None) dropped
        // every entry after the first.
        let prev = {
            let cfg = crate::config::CONFIG.read().unwrap();
            (cfg.clipboard.persist, cfg.clipboard.auto_expire_days)
        };
        {
            let mut cfg = crate::config::CONFIG.write().unwrap();
            cfg.clipboard.persist = true;
            cfg.clipboard.auto_expire_days = 0;
        }
        let mk = |bytes: &[u8]| {
            let hash = fnv1a64(bytes);
            assert!(cache_write_image(hash, bytes));
            ClipEntry {
                text: String::new(),
                image: Some(ImageEntry {
                    uti: NSPASTEBOARD_TYPE_PNG.to_string(),
                    hash,
                    data_path: clip_image_path(hash),
                    preview_png: Arc::new(bytes.to_vec()),
                    source_path: None,
                }),
                pinned: false,
                source_app: String::new(),
                source_key: String::new(),
                copied_at: Some(1755000000),
            }
        };
        let a = mk(b"load-keep-a");
        let b = mk(b"load-keep-b");
        let c = mk(b"load-keep-c");
        let path = history_file_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serialize_history(&[a, b, c]).unwrap()).unwrap();
        CLIP_HISTORY.lock().unwrap().clear();
        load_history();
        // The save_history trailing load_history writes back asynchronously: drain the
        // worker before rewriting the history file, or the first 3-entry snapshot lands
        // late and clobbers the next round's dup file (the flake's root cause).
        super::persist::flush_persist_worker_for_tests();
        let hashes: Vec<u64> = CLIP_HISTORY
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| e.image.as_ref().map(|i| i.hash))
            .collect();
        assert_eq!(
            hashes.len(),
            3,
            "all three entries with distinct data must survive, got: {hashes:?}"
        );
        // Re-copying the same image (same hash) still dedups to one.
        let dup_path = history_file_path();
        std::fs::write(&dup_path, serialize_history(&[mk(b"load-keep-a")]).unwrap()).unwrap();
        CLIP_HISTORY.lock().unwrap().clear();
        load_history();
        super::persist::flush_persist_worker_for_tests();
        let n = CLIP_HISTORY
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.image.is_some())
            .count();
        assert_eq!(n, 1, "entries sharing a hash must merge into one");
        // restore the original config.
        let mut cfg = crate::config::CONFIG.write().unwrap();
        cfg.clipboard.persist = prev.0;
        cfg.clipboard.auto_expire_days = prev.1;
    }
    #[test]
    fn load_history_skips_expired_entries() {
        use super::{expire_entries, now_secs};
        // The persist load path: expired entries (unpinned, past TTL) never reach memory;
        // pinned and fresh ones stay.
        let now = now_secs();
        let ttl = Some(30u64 * 86400);
        let mut h = vec![
            ClipEntry {
                text: "expired".to_string(),
                image: None,
                pinned: false,
                source_app: String::new(),
                source_key: String::new(),
                copied_at: Some(now - 31 * 86400),
            },
            ClipEntry {
                text: "pinned-old".to_string(),
                image: None,
                pinned: true,
                source_app: String::new(),
                source_key: String::new(),
                copied_at: Some(now - 365 * 86400),
            },
            ClipEntry {
                text: "fresh".to_string(),
                image: None,
                pinned: false,
                source_app: String::new(),
                source_key: String::new(),
                copied_at: Some(now - 1000),
            },
        ];
        assert_eq!(expire_entries(&mut h, now, ttl), 1);
        assert_eq!(texts(&h), vec!["pinned-old", "fresh"]);
        // Off (None) touches nothing.
        let mut h = vec![ClipEntry {
            text: "expired".to_string(),
            image: None,
            pinned: false,
            source_app: String::new(),
            source_key: String::new(),
            copied_at: Some(now - 400 * 86400),
        }];
        assert_eq!(expire_entries(&mut h, now, None), 0);
        assert_eq!(h.len(), 1);
    }
    #[test]
    fn history_parse_rejects_corruption_and_future_versions() {
        use super::parse_history;
        assert_eq!(parse_history("not toml at all {{{"), None);
        // a future version is rejected.
        let entries = super::serialize_history(&[]).unwrap();
        let future = entries.replace("version = 1", "version = 2");
        assert_eq!(parse_history(&future), None);
        // the current version parses.
        assert_eq!(parse_history(&entries), Some(vec![]));
    }
    #[test]
    fn restore_loaded_entry_recovers_preview_and_drops_broken_data_entries() {
        use super::{cache_write_image, cache_write_preview, fnv1a64, restore_loaded_entry};
        // A data entry with a persisted preview -> the preview is restored and data_path
        // rebuilt.
        let bytes = b"restore-test-img";
        let hash = fnv1a64(bytes);
        assert!(cache_write_image(hash, bytes));
        let preview = b"restore-test-preview";
        assert!(cache_write_preview(hash, preview));
        let img = super::ImageEntry {
            uti: NSPASTEBOARD_TYPE_PNG.to_string(),
            hash,
            data_path: std::path::PathBuf::new(),
            preview_png: Arc::new(Vec::new()),
            source_path: None,
        };
        let entry = ClipEntry {
            text: String::new(),
            image: Some(img.clone()),
            pinned: true,
            source_app: "Safari".to_string(),
            source_key: String::new(),
            copied_at: None,
        };
        let restored = restore_loaded_entry(entry.clone()).expect("restore");
        let r_img = restored.image.as_ref().unwrap();
        assert_eq!(r_img.preview_png.as_slice(), preview);
        assert_eq!(r_img.data_path, super::clip_image_path(hash));
        assert_eq!(restored.pinned, entry.pinned);
        // a missing data file drops the entry.
        let ghost = super::ImageEntry {
            uti: NSPASTEBOARD_TYPE_PNG.to_string(),
            hash: fnv1a64(b"ghost"),
            data_path: std::path::PathBuf::new(),
            preview_png: Arc::new(Vec::new()),
            source_path: None,
        };
        let ghost_entry = ClipEntry {
            text: String::new(),
            image: Some(ghost),
            pinned: false,
            source_app: String::new(),
            source_key: String::new(),
            copied_at: None,
        };
        assert!(restore_loaded_entry(ghost_entry).is_none());
        // a text entry passes through.
        let text_entry = ClipEntry {
            text: "hello".to_string(),
            image: None,
            pinned: false,
            source_app: String::new(),
            source_key: String::new(),
            copied_at: None,
        };
        assert_eq!(restore_loaded_entry(text_entry.clone()), Some(text_entry));
        // A file-copy entry: the preview is restored from {hash}.preview, data_path stays
        // empty, the source path is kept.
        let fbytes = b"restore-test-file";
        let fhash = fnv1a64(fbytes);
        let fpreview = b"restore-test-file-preview";
        assert!(cache_write_preview(fhash, fpreview));
        let file_ref = image_from_file(fbytes, "/tmp/exists.gif");
        let file_entry = ClipEntry {
            text: "exists.gif".to_string(),
            image: Some(file_ref),
            pinned: false,
            source_app: "Finder".to_string(),
            source_key: String::new(),
            copied_at: None,
        };
        let restored_file = restore_loaded_entry(file_entry.clone()).expect("restore file");
        let rf_img = restored_file.image.as_ref().unwrap();
        assert_eq!(rf_img.preview_png.as_slice(), fpreview);
        assert!(rf_img.data_path.as_os_str().is_empty());
        assert_eq!(rf_img.source_path.as_deref(), Some("/tmp/exists.gif"));
        // A degenerate file entry (hash=0, no preview) passes through.
        let degenerate = super::ImageEntry {
            uti: NSPASTEBOARD_TYPE_PNG.to_string(),
            hash: 0,
            data_path: std::path::PathBuf::new(),
            preview_png: Arc::new(Vec::new()),
            source_path: Some("/tmp/broken.gif".to_string()),
        };
        let degen_entry = ClipEntry {
            text: "broken.gif".to_string(),
            image: Some(degenerate),
            pinned: false,
            source_app: String::new(),
            source_key: String::new(),
            copied_at: None,
        };
        assert_eq!(restore_loaded_entry(degen_entry.clone()), Some(degen_entry));
    }
    #[test]
    fn sensitive_marker_list_covers_the_securing_copy_protocol() {
        use super::SENSITIVE_PASTEBOARD_TYPES;
        // All four Securing-Copy markers must be in the skip list.
        assert!(SENSITIVE_PASTEBOARD_TYPES.contains(&"org.nspasteboard.TransientType"));
        assert!(SENSITIVE_PASTEBOARD_TYPES.contains(&"org.nspasteboard.ConcealedType"));
        assert!(SENSITIVE_PASTEBOARD_TYPES.contains(&"org.nspasteboard.AutoGeneratedType"));
        assert!(SENSITIVE_PASTEBOARD_TYPES.contains(&"com.agilebits.onepassword"));
    }
    #[test]
    fn paste_writeback_skip_only_when_toggle_off_and_marker_present() {
        use super::should_skip_paste_writeback;
        // Toggle on (default) -> never skip (used entries keep moving to the top).
        assert!(!should_skip_paste_writeback(true, false));
        assert!(!should_skip_paste_writeback(true, true));
        // Toggle off: our marker present -> skip (pasting does not reorder); no marker
        // (a genuine copy) -> record normally.
        assert!(should_skip_paste_writeback(false, true));
        assert!(!should_skip_paste_writeback(false, false));
    }
    #[test]
    fn paste_delete_suppression_hits_the_armed_count_or_marker() {
        use super::paste_delete_suppression_hit;
        assert!(paste_delete_suppression_hit(Some(41), 41, false));
        assert!(paste_delete_suppression_hit(Some(41), 42, true));
        assert!(!paste_delete_suppression_hit(None, 41, true));
        assert!(!paste_delete_suppression_hit(Some(41), 42, false));
        assert!(!paste_delete_suppression_hit(Some(42), 41, false));
    }
    #[test]
    fn paste_delete_identity_ignores_reorder_metadata() {
        use super::same_clip_entry_identity;
        let target = entry("secret");
        let mut current = entry_with_source("secret", "Other App");
        current.pinned = true;
        current.copied_at = Some(123);
        assert!(same_clip_entry_identity(&target, &current));
        assert!(!same_clip_entry_identity(&target, &entry("different")));
        assert!(!same_clip_entry_identity(&target, &entry_image(b"secret")));
    }
    #[test]
    fn explicit_delete_restore_preserves_metadata_and_original_position() {
        use super::{remove_entry_for_undo, restore_entry_at};
        let mut removed = entry_with_source("restore me", "Safari");
        removed.pinned = true;
        removed.source_key = "com.apple.Safari".to_string();
        removed.copied_at = Some(1234);
        let original = removed.clone();
        let mut history = vec![removed, entry("other")];
        let deleted = remove_entry_for_undo(&mut history, 0).expect("entry must be removed");
        assert_eq!(deleted, original);
        let (index, inserted) = restore_entry_at(&mut history, deleted, 0);
        assert!(inserted);
        assert_eq!(index, 0);
        assert_eq!(history[0], original);
    }
    #[test]
    fn undo_removal_keeps_image_cache_available_for_restore() {
        use super::{cache_read_image, remove_entry_for_undo, restore_entry_at};
        let image_entry = entry_image(b"undo-cache-bytes");
        let hash = image_entry.image.as_ref().unwrap().hash;
        let mut history = vec![image_entry.clone()];
        let removed = remove_entry_for_undo(&mut history, 0).unwrap();
        assert!(cache_read_image(hash).is_some());
        let (_, inserted) = restore_entry_at(&mut history, removed, 0);
        assert!(inserted);
        assert_eq!(history, vec![image_entry]);
        assert!(cache_read_image(hash).is_some());
    }
    #[test]
    fn restore_respects_pinned_boundary_and_deduplicates() {
        use super::{remove_entry_for_undo, restore_entry_at};
        let mut pinned = entry("pinned");
        pinned.pinned = true;
        let mut history = vec![pinned.clone(), entry("newest")];
        let unpinned = history.pop().unwrap();
        let (index, inserted) = restore_entry_at(&mut history, unpinned.clone(), 0);
        assert!(inserted);
        assert_eq!(index, 1, "unpinned entries must stay below pinned entries");
        let (duplicate_index, duplicate_inserted) = restore_entry_at(&mut history, unpinned, 0);
        assert_eq!(duplicate_index, 1);
        assert!(!duplicate_inserted);
        assert_eq!(history.len(), 2);
        let deleted = remove_entry_for_undo(&mut history, 0).unwrap();
        let (pinned_index, pinned_inserted) = restore_entry_at(&mut history, deleted, 99);
        assert!(pinned_inserted);
        assert_eq!(pinned_index, 0);
    }
    #[test]
    fn clear_scope_keeps_pinned_only_when_requested() {
        use super::remove_history_scope;
        let mut pinned = entry("keep");
        pinned.pinned = true;
        let mut history = vec![pinned, entry("drop")];
        let removed = remove_history_scope(&mut history, false);
        assert_eq!(texts(&history), vec!["keep"]);
        assert_eq!(texts(&removed), vec!["drop"]);
        let removed = remove_history_scope(&mut history, true);
        assert!(history.is_empty());
        assert_eq!(texts(&removed), vec!["keep"]);
    }
    #[test]
    fn clipboard_undo_window_and_shortcut_are_strict() {
        use super::{clipboard_undo_expired, is_clipboard_undo_shortcut};
        let now = std::time::Instant::now();
        assert!(!clipboard_undo_expired(
            now + std::time::Duration::from_secs(1),
            now
        ));
        assert!(clipboard_undo_expired(now, now));
        assert!(is_clipboard_undo_shortcut(6, 0x0010_0000));
        assert!(!is_clipboard_undo_shortcut(6, 0x0010_0000 | 0x0002_0000));
        assert!(!is_clipboard_undo_shortcut(7, 0x0010_0000));
    }
    #[test]
    fn paste_kind_prefers_the_file_when_it_still_exists() {
        use super::{paste_kind, PasteKind};
        use std::fs;
        // A bare image copy (no source path) -> image data paste.
        assert_eq!(paste_kind(&image(b"x")), PasteKind::Image);
        // A file copy whose source file is gone -> Image (the caller skips the paste;
        // a file copy holds no bytes to fall back to).
        assert_eq!(
            paste_kind(&image_from_file(b"x", "/nonexistent/omt-gone.gif")),
            PasteKind::Image
        );
        // A file copy whose source file still exists -> file paste (path carried back).
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("anim.gif");
        fs::write(&p, b"GIF89a").unwrap();
        assert_eq!(
            paste_kind(&image_from_file(b"GIF89a", p.to_str().unwrap())),
            PasteKind::File(p.to_str().unwrap().to_string())
        );
    }
    #[test]
    fn record_image_file_copies_dedup_by_content_and_keep_the_filename() {
        use super::record_image;
        // Re-copying the same path -> dedup by content hash, no duplicate.
        let mut h = Vec::new();
        let p = "/Users/ceres/Downloads/vva划船.gif";
        let bytes = b"GIF89a-anim";
        assert!(record_image(
            &mut h,
            &image_from_file(bytes, p),
            "Finder",
            "",
            50
        ));
        assert_eq!(h.len(), 1);
        // The entry's text holds the filename (row display + search).
        assert_eq!(h[0].text, "vva划船.gif");
        assert!(record_image(
            &mut h,
            &image_from_file(bytes, p),
            "Ghostty",
            "",
            50
        ));
        assert_eq!(h.len(), 1, "same content must dedup");
        assert_eq!(h[0].source_app, "Ghostty");
        // A file and its Finder duplicate: different paths, identical bytes -> still one
        // entry, with the source path updated to the latest copy (pasting restores the
        // newest file).
        let copy = "/Users/ceres/Downloads/vva划船_副本.gif";
        assert!(record_image(
            &mut h,
            &image_from_file(bytes, copy),
            "Finder",
            "",
            50
        ));
        assert_eq!(h.len(), 1, "same content at another path must dedup");
        assert_eq!(
            h[0].image.as_ref().unwrap().source_path.as_deref(),
            Some(copy)
        );
        // different content -> a new entry.
        let q = "/Users/ceres/Downloads/other.gif";
        assert!(record_image(
            &mut h,
            &image_from_file(b"different-bytes", q),
            "Finder",
            "",
            50
        ));
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].text, "other.gif");
        // File copies and data entries never cross-dedup (same content, different forms).
        let data_img = image(bytes);
        assert!(record_image(&mut h, &data_img, "Safari", "", 50));
        assert_eq!(h.len(), 3);
        // An empty preview AND no source path -> rejected (recording failed).
        let dead = super::ImageEntry {
            uti: super::NSPASTEBOARD_TYPE_PNG.to_string(),
            hash: 0,
            data_path: std::path::PathBuf::new(),
            preview_png: Arc::new(Vec::new()),
            source_path: None,
        };
        assert!(!record_image(&mut h, &dead, "Safari", "", 50));
        assert_eq!(h.len(), 3);
    }
    #[test]
    fn same_hash_file_and_data_entries_keep_shared_cache_until_both_are_gone() {
        use super::{
            cache_read_image, cache_read_preview, cache_write_preview, delete_entry, record_image,
        };
        // A file entry and a data entry with identical content (same hash) coexist across
        // classes. Deleting the FILE entry must NOT wipe the shared `{hash}` data bytes and
        // `{hash}.preview` (the data entry still needs them); only after both are gone may
        // the cache be cleaned.
        let bytes = b"shared-cache-test";
        let hash = super::fnv1a64(bytes);
        let mut h = Vec::new();
        assert!(record_image(
            &mut h,
            &image_from_file(bytes, "/tmp/a.gif"),
            "Finder",
            "",
            50
        ));
        assert!(record_image(&mut h, &image(bytes), "Safari", "", 50));
        assert_eq!(h.len(), 2, "file + data entries coexist");
        assert!(cache_read_image(hash).is_some());
        assert!(cache_write_preview(hash, b"shared-preview"));
        // Order: [data, file]; deleting the file entry (idx=1) must keep the shared cache.
        delete_entry(&mut h, 1);
        assert_eq!(h.len(), 1);
        assert!(h[0].image.as_ref().unwrap().source_path.is_none());
        assert!(
            cache_read_image(hash).is_some(),
            "data entry's paste bytes must survive"
        );
        assert!(
            cache_read_preview(hash).is_some(),
            "shared preview must survive"
        );
        // Deleting the data entry: no references left -> the cache is cleaned up.
        delete_entry(&mut h, 0);
        assert!(h.is_empty());
        assert_eq!(cache_read_image(hash), None);
        assert_eq!(cache_read_preview(hash), None);
    }
    #[test]
    fn trim_keeps_shared_cache_for_the_surviving_same_hash_entry() {
        use super::{
            cache_read_image, cache_read_preview, cache_write_preview, record_image, record_text,
        };
        // max=2: a file entry (H) + a data entry (H) fill the list; a text entry trims the
        // oldest FILE entry, but the surviving data entry (H) keeps the shared cache; the
        // next trim drops the data entry and the cache is cleaned.
        let bytes = b"trim-shared-test";
        let hash = super::fnv1a64(bytes);
        let mut h = Vec::new();
        assert!(record_image(
            &mut h,
            &image_from_file(bytes, "/tmp/t.gif"),
            "Finder",
            "",
            2
        ));
        assert!(record_image(&mut h, &image(bytes), "Safari", "", 2));
        assert!(cache_write_preview(hash, b"preview"));
        record_text(&mut h, "x", "Ghostty", "", 2);
        // [x, data, file] -> the file is trimmed.
        assert_eq!(h.len(), 2);
        assert!(h.iter().any(|e| {
            e.image
                .as_ref()
                .is_some_and(|i| i.hash == hash && i.source_path.is_none())
        }));
        assert!(
            cache_read_image(hash).is_some(),
            "trimmed file entry must not wipe the data entry's bytes"
        );
        assert!(cache_read_preview(hash).is_some());
        // Another trim pushes the data entry out -> unreferenced -> cache cleaned.
        record_text(&mut h, "y", "Ghostty", "", 2);
        assert_eq!(h.len(), 2);
        assert!(h.iter().all(|e| e.image.is_none()));
        assert_eq!(cache_read_image(hash), None);
        assert_eq!(cache_read_preview(hash), None);
    }
    #[test]
    fn reference_check_honors_pinned_survivors_for_clear_all() {
        use super::hash_referenced_by;
        // Simulates clear-all (keeps pinned): a pinned data entry and an unpinned file
        // entry share a hash -> the dropped file entry's hash is still referenced by the
        // surviving pinned entry -> the cache must be kept.
        let bytes = b"clear-shared-test";
        let hash = super::fnv1a64(bytes);
        let data_entry = ClipEntry {
            text: String::new(),
            image: Some(image(bytes)),
            pinned: true,
            source_app: String::new(),
            source_key: String::new(),
            copied_at: None,
        };
        let file_entry = ClipEntry {
            text: "x.gif".to_string(),
            image: Some(image_from_file(bytes, "/tmp/x.gif")),
            pinned: false,
            source_app: String::new(),
            source_key: String::new(),
            copied_at: None,
        };
        let all = [file_entry, data_entry];
        assert!(hash_referenced_by(all.iter().filter(|e| e.pinned), hash));
        assert!(!hash_referenced_by(
            all.iter().filter(|e| e.pinned),
            0xdeadbeef
        ));
    }
    #[test]
    fn preferred_uti_picks_the_animated_original_over_static_reencodes() {
        use super::{
            preferred_uti, NSPASTEBOARD_TYPE_GIF, NSPASTEBOARD_TYPE_GIF_ALIAS,
            NSPASTEBOARD_TYPE_JPEG, NSPASTEBOARD_TYPE_PNG, NSPASTEBOARD_TYPE_TIFF,
            NSPASTEBOARD_TYPE_WEBP,
        };
        // Core regression: when an animated GIF coexists with static PNG/TIFF, GIF must
        // win (otherwise the history holds a static frame and Option+V stops animating --
        // the exact bug where Cmd+V pasted a GIF but ours pasted a static image).
        assert_eq!(
            preferred_uti(&[NSPASTEBOARD_TYPE_PNG, NSPASTEBOARD_TYPE_GIF]),
            Some(NSPASTEBOARD_TYPE_GIF)
        );
        assert_eq!(
            preferred_uti(&[NSPASTEBOARD_TYPE_TIFF, NSPASTEBOARD_TYPE_GIF]),
            Some(NSPASTEBOARD_TYPE_GIF)
        );
        assert_eq!(
            preferred_uti(&[
                NSPASTEBOARD_TYPE_PNG,
                NSPASTEBOARD_TYPE_JPEG,
                NSPASTEBOARD_TYPE_GIF
            ]),
            Some(NSPASTEBOARD_TYPE_GIF)
        );
        // The GIF alias: a pasteboard carrying only public.gif is recognized too.
        assert_eq!(
            preferred_uti(&[NSPASTEBOARD_TYPE_PNG, NSPASTEBOARD_TYPE_GIF_ALIAS]),
            Some(NSPASTEBOARD_TYPE_GIF_ALIAS)
        );
        assert_eq!(
            preferred_uti(&[NSPASTEBOARD_TYPE_TIFF, NSPASTEBOARD_TYPE_GIF_ALIAS]),
            Some(NSPASTEBOARD_TYPE_GIF_ALIAS)
        );
        // Animation wins over every static format; WebP follows right after GIF.
        assert_eq!(
            preferred_uti(&[NSPASTEBOARD_TYPE_PNG, NSPASTEBOARD_TYPE_WEBP]),
            Some(NSPASTEBOARD_TYPE_WEBP)
        );
        // Without GIF, static fidelity order: PNG > JPEG > TIFF.
        assert_eq!(
            preferred_uti(&[NSPASTEBOARD_TYPE_JPEG, NSPASTEBOARD_TYPE_PNG]),
            Some(NSPASTEBOARD_TYPE_PNG)
        );
        assert_eq!(
            preferred_uti(&[NSPASTEBOARD_TYPE_TIFF, NSPASTEBOARD_TYPE_JPEG]),
            Some(NSPASTEBOARD_TYPE_JPEG)
        );
        assert_eq!(
            preferred_uti(&[NSPASTEBOARD_TYPE_TIFF]),
            Some(NSPASTEBOARD_TYPE_TIFF)
        );
        // nothing present -> None.
        assert_eq!(preferred_uti(&[]), None);
    }
    #[test]
    fn preferred_uti_order_pins_gif_before_static_fallbacks() {
        use super::{
            preferred_uti, NSPASTEBOARD_TYPE_BMP, NSPASTEBOARD_TYPE_GIF, NSPASTEBOARD_TYPE_HEIC,
            NSPASTEBOARD_TYPE_PNG, NSPASTEBOARD_TYPE_TIFF,
        };
        // Order invariant: GIF always ranks before every static fallback (guards against
        // someone reverting the order).
        for static_uti in [
            NSPASTEBOARD_TYPE_PNG,
            NSPASTEBOARD_TYPE_HEIC,
            NSPASTEBOARD_TYPE_BMP,
            NSPASTEBOARD_TYPE_TIFF,
        ] {
            assert_eq!(
                preferred_uti(&[static_uti, NSPASTEBOARD_TYPE_GIF]),
                Some(NSPASTEBOARD_TYPE_GIF),
                "GIF must rank before {static_uti}"
            );
        }
    }
    #[test]
    fn filtered_indices_hides_images_when_querying() {
        use super::{filtered_indices, ClipFilter};
        // Image entries have no text: shown with an empty query, excluded when querying.
        let h = vec![
            entry_image(b"png"),
            entry("apple pie"),
            entry_image(b"png2"),
            entry("fn main() {\n    let answer = 42;\n}"),
        ];
        assert_eq!(filtered_indices(&h, "", ClipFilter::All), vec![0, 1, 2, 3]);
        assert_eq!(filtered_indices(&h, "apple", ClipFilter::All), vec![1]);
        assert!(filtered_indices(&h, "png", ClipFilter::All).is_empty());
        // The kind filters: Image / Text / Link / Code.
        assert_eq!(filtered_indices(&h, "", ClipFilter::Image), vec![0, 2]);
        assert_eq!(filtered_indices(&h, "", ClipFilter::Text), vec![1]);
        assert!(filtered_indices(&h, "", ClipFilter::Link).is_empty());
        assert_eq!(filtered_indices(&h, "", ClipFilter::Code), vec![3]);
    }
    #[test]
    fn detail_action_is_active_only_for_the_open_selected_row() {
        use super::{detail_action_is_active, NO_SELECTION};
        assert!(detail_action_is_active(true, 2, 2));
        assert!(!detail_action_is_active(false, 2, 2));
        assert!(!detail_action_is_active(true, 2, 1));
        assert!(!detail_action_is_active(true, NO_SELECTION, 0));
    }
    #[test]
    fn empty_state_hint_uses_the_live_viewport_height() {
        use super::{empty_state_doc_height, header_strip_h, picker_min_height, FOOTER_H};
        // The empty-state minimum matches a full window containing three same-group records,
        // including the first group's header.
        let min_h = picker_min_height();
        assert_eq!(min_h, 410.0);
        let min_list_h = min_h - header_strip_h() - FOOTER_H;
        assert_eq!(empty_state_doc_height(min_list_h - 20.0), min_list_h);
        // When a category filters to no results, the picker can remain tall; the hint document
        // must grow with the viewport to stay centered.
        assert_eq!(empty_state_doc_height(480.0), 480.0);
    }
    #[test]
    fn detail_copy_refresh_restores_source_selection_in_filtered_list() {
        use super::{visible_selection_for_text, ClipFilter};
        // After copying an excerpt, the new entry goes to the top; the detail source moves
        // down, but the displayed selection must follow it.
        let h = vec![
            entry("copied excerpt"),
            entry("source detail"),
            entry("other"),
        ];
        assert_eq!(
            visible_selection_for_text(&h, "", ClipFilter::All, "source detail"),
            Some(1)
        );
        assert_eq!(
            visible_selection_for_text(&h, "source", ClipFilter::All, "source detail"),
            Some(0)
        );
        assert_eq!(
            visible_selection_for_text(&h, "", ClipFilter::Link, "source detail"),
            None
        );
    }
    #[test]
    fn filtered_indices_link_filter_matches_urls_only() {
        use super::{filtered_indices, ClipFilter};
        let h = vec![
            entry("hello world"),
            entry("https://github.com/eacryo/oh-my-tab"),
        ];
        assert_eq!(filtered_indices(&h, "", ClipFilter::Link), vec![1]);
        assert_eq!(
            filtered_indices(&h, "hello", ClipFilter::Link),
            Vec::<usize>::new()
        );
    }
    #[test]
    fn tab_filter_cycle_visits_every_category_and_wraps() {
        use super::{next_clip_filter, ClipFilter};
        // Tab cycles in the UI's left-to-right filter order and wraps from the last item.
        assert_eq!(next_clip_filter(ClipFilter::All), ClipFilter::Text);
        assert_eq!(next_clip_filter(ClipFilter::Text), ClipFilter::Image);
        assert_eq!(next_clip_filter(ClipFilter::Image), ClipFilter::Link);
        assert_eq!(next_clip_filter(ClipFilter::Link), ClipFilter::Code);
        assert_eq!(next_clip_filter(ClipFilter::Code), ClipFilter::All);
    }
    #[test]
    fn compute_pitches_sizes_image_rows_for_the_thumbnail() {
        use super::{compute_pitches, GROUP_H, ROW_H, THUMB_H};
        // Every row is a uniform 61pt (text and image alike); the first row of the group
        // carries the header, the second (same group) does not.
        let texts = vec![entry("short"), entry_image(b"png")];
        let pitches = compute_pitches(&texts);
        assert_eq!(pitches[0], GROUP_H + ROW_H);
        assert_eq!(pitches[1], ROW_H);
        assert!(pitches[1] < pitches[0]);
        // The thumbnail box fits inside the row.
        const { assert!(THUMB_H <= ROW_H) };
    }
    #[test]
    fn duplicate_pinned_entry_stays_pinned_and_moves_to_pin_top() {
        use super::pin_entry;
        // After pinning B, re-copying B keeps it pinned and moves it to the top of the
        // pinned block.
        let mut h = Vec::new();
        record_text(&mut h, "A", 50);
        record_text(&mut h, "B", 50);
        pin_entry(&mut h, 0); // pin B
        record_text(&mut h, "C", 50);
        record_text(&mut h, "D", 50);
        // Now: B (pinned) D C A (new entries land after the pinned block, newest first).
        assert_eq!(texts(&h), vec!["B", "D", "C", "A"]);
        // Copying B keeps it pinned at the top of the pinned block (order unchanged).
        record_text(&mut h, "B", 50);
        assert!(h[0].pinned);
        assert_eq!(texts(&h), vec!["B", "D", "C", "A"]);
        // Copying A moves it to the top of the unpinned block (D is newer than C, so it
        // stays before C).
        record_text(&mut h, "A", 50);
        assert_eq!(texts(&h), vec!["B", "A", "D", "C"]);
        assert!(!h[1].pinned);
    }
    #[test]
    fn newest_goes_first() {
        let mut h = Vec::new();
        record_text(&mut h, "first", 50);
        record_text(&mut h, "second", 50);
        assert_eq!(texts(&h), vec!["second", "first"]);
    }
    #[test]
    fn overflow_is_trimmed() {
        // Entries beyond the cap are trimmed from the tail.
        let mut h = Vec::new();
        for i in 0..5 {
            record_text(&mut h, &format!("item{i}"), 3);
        }
        assert_eq!(h.len(), 3);
        assert_eq!(h[0].text, "item4");
        assert_eq!(h[2].text, "item2");
    }
    #[test]
    fn zero_max_records_nothing() {
        let mut h = Vec::new();
        assert!(!record_text(&mut h, "x", 0));
        assert!(h.is_empty());
    }
    #[test]
    fn pinned_entries_stay_on_top_of_new_records() {
        use super::{pin_entry, unpin_entry};
        // After pinning B, a new copy of C lands after the pinned block (B stays on top).
        let mut h = Vec::new();
        record_text(&mut h, "A", 50);
        record_text(&mut h, "B", 50);
        pin_entry(&mut h, 0); // pin B
        assert!(h[0].pinned);
        record_text(&mut h, "C", 50);
        assert_eq!(texts(&h), vec!["B", "C", "A"]);
        // Unpinning B moves it to the top of the unpinned block.
        unpin_entry(&mut h, 0);
        assert!(!h[0].pinned);
        assert_eq!(texts(&h), vec!["B", "C", "A"]);
        // A new copy of D lands before B.
        record_text(&mut h, "D", 50);
        assert_eq!(texts(&h), vec!["D", "B", "C", "A"]);
    }
    #[test]
    fn pin_moves_entry_to_top_and_is_idempotent() {
        use super::pin_entry;
        let mut h = Vec::new();
        record_text(&mut h, "A", 50);
        record_text(&mut h, "B", 50);
        record_text(&mut h, "C", 50);
        pin_entry(&mut h, 2); // pin A
        assert_eq!(texts(&h), vec!["A", "C", "B"]);
        assert!(h[0].pinned);
        // Pinning the same entry again: no change.
        pin_entry(&mut h, 0);
        assert_eq!(h.len(), 3);
        // Out-of-range pin: ignored.
        pin_entry(&mut h, 99);
        assert_eq!(h.len(), 3);
    }
    #[test]
    fn delete_entry_removes_by_index_and_ignores_out_of_range() {
        use super::delete_entry;
        let mut h = Vec::new();
        record_text(&mut h, "A", 50);
        record_text(&mut h, "B", 50);
        record_text(&mut h, "C", 50);
        // remove the middle entry.
        delete_entry(&mut h, 1);
        assert_eq!(texts(&h), vec!["C", "A"]);
        // out-of-range delete: no change.
        delete_entry(&mut h, 99);
        assert_eq!(h.len(), 2);
        // order is preserved after deletion.
        delete_entry(&mut h, 0);
        assert_eq!(texts(&h), vec!["A"]);
    }
    #[test]
    fn delete_pinned_entry_keeps_others_pinned() {
        use super::{delete_entry, pin_entry};
        let mut h = Vec::new();
        record_text(&mut h, "A", 50);
        record_text(&mut h, "B", 50);
        record_text(&mut h, "C", 50);
        pin_entry(&mut h, 0); // pin C
        pin_entry(&mut h, 1); // pin B
        assert_eq!(texts(&h), vec!["B", "C", "A"]);
        // Delete the pinned C: the other pin stays, no hole in the list.
        delete_entry(&mut h, 1);
        assert_eq!(texts(&h), vec!["B", "A"]);
        assert!(h[0].pinned);
        assert!(!h[1].pinned);
    }
    #[test]
    fn expire_entries_respects_pin_ttl_and_legacy_entries() {
        use super::expire_entries;
        // An entry with an explicit timestamp.
        let mk = |text: &str, pinned: bool, copied_at: Option<u64>| ClipEntry {
            text: text.to_string(),
            image: None,
            pinned,
            source_app: String::new(),
            source_key: String::new(),
            copied_at,
        };
        // ttl None (off) -> nothing is removed.
        let mut h = vec![mk("old", false, Some(1))];
        assert_eq!(expire_entries(&mut h, 1_000_000, None), 0);
        assert_eq!(h.len(), 1);
        // Not yet expired -> kept.
        let mut h = vec![mk("a", false, Some(90))];
        assert_eq!(expire_entries(&mut h, 100, Some(30)), 0);
        assert_eq!(h.len(), 1);
        // Boundary: == ttl -> expired (>=).
        let mut h = vec![mk("a", false, Some(70))];
        assert_eq!(expire_entries(&mut h, 100, Some(30)), 1);
        assert!(h.is_empty());
        // Expired unpinned -> removed; pinned -> kept.
        let mut h = vec![
            mk("old-pinned", true, Some(1)),
            mk("old-free", false, Some(1)),
            mk("fresh", false, Some(99)),
        ];
        assert_eq!(expire_entries(&mut h, 100, Some(30)), 1);
        assert_eq!(texts(&h), vec!["old-pinned", "fresh"]);
        // Legacy entries -> kept.
        let mut h = vec![mk("legacy", false, None)];
        assert_eq!(expire_entries(&mut h, 1_000_000, Some(30)), 0);
        assert_eq!(h.len(), 1);
        // Clock rollback -> safe.
        let mut h = vec![mk("future", false, Some(200))];
        assert_eq!(expire_entries(&mut h, 100, Some(30)), 0);
        assert_eq!(h.len(), 1);
    }
    #[test]
    fn expire_entries_deletes_image_cache_only_when_unreferenced() {
        use super::expire_entries;
        let img = image(b"expire-cache-test-bytes");
        // An expired image entry takes its cache files (data + preview) with it.
        let mut h = vec![ClipEntry {
            text: String::new(),
            image: Some(img.clone()),
            pinned: false,
            source_app: String::new(),
            source_key: String::new(),
            copied_at: Some(1),
        }];
        assert!(super::cache_read_image(img.hash).is_some());
        assert_eq!(expire_entries(&mut h, 100, Some(30)), 1);
        assert!(h.is_empty());
        assert!(
            super::cache_read_image(img.hash).is_none(),
            "expired image's cache must be swept"
        );
        // A pinned survivor sharing the hash keeps the cache files.
        // Re-write the cache: the previous scenario deleted it (proof of the sweep).
        let img = image(b"expire-cache-test-bytes");
        let mut h = vec![
            ClipEntry {
                text: String::new(),
                image: Some(img.clone()),
                pinned: true, // pinned survivor
                source_app: String::new(),
                source_key: String::new(),
                copied_at: Some(1),
            },
            ClipEntry {
                text: String::new(),
                image: Some(img.clone()),
                pinned: false,
                source_app: String::new(),
                source_key: String::new(),
                copied_at: Some(1),
            },
        ];
        assert_eq!(expire_entries(&mut h, 100, Some(30)), 1);
        assert_eq!(h.len(), 1);
        assert!(h[0].pinned);
        assert!(
            super::cache_read_image(img.hash).is_some(),
            "a pinned survivor keeps the shared cache"
        );
    }
    #[test]
    fn filtered_indices_matches_case_insensitively() {
        use super::{filtered_indices, ClipFilter};
        let h = vec![
            entry("Apple Pie"),
            entry("Banana"),
            entry("apple cider"),
            entry("Pineapple"),
        ];
        // an empty query returns everything.
        assert_eq!(filtered_indices(&h, "", ClipFilter::All), vec![0, 1, 2, 3]);
        // case-insensitive substring.
        assert_eq!(
            filtered_indices(&h, "apple", ClipFilter::All),
            vec![0, 2, 3]
        );
        // no match -> empty.
        assert!(filtered_indices(&h, "orange", ClipFilter::All).is_empty());
        // prefix and single chars.
        assert_eq!(filtered_indices(&h, "ban", ClipFilter::All), vec![1]);
    }
    #[test]
    fn filtered_indices_handles_cjk_emoji_and_combining_input() {
        use super::{filtered_indices, ClipFilter};
        let h = vec![
            entry("中文输入法候选词"),
            entry("👨‍👩‍👧‍👦 family"),
            entry("e\u{301} accent"),
        ];
        // IME composition results, emoji ZWJ sequences, and combining marks should remain
        // searchable as ordinary Unicode strings; filtering must not use byte-width heuristics.
        assert_eq!(filtered_indices(&h, "输入法", ClipFilter::All), vec![0]);
        assert_eq!(filtered_indices(&h, "family", ClipFilter::All), vec![1]);
        assert_eq!(filtered_indices(&h, "e\u{301}", ClipFilter::All), vec![2]);
    }
    #[test]
    fn mapped_index_goes_through_the_filtered_list() {
        use super::{filtered_indices, mapped_index, ClipFilter};
        let h = vec![
            entry("Apple"),
            entry("Banana"),
            entry("Cherry"),
            entry("Apricot"),
        ];
        let filtered = filtered_indices(&h, "a", ClipFilter::All);
        // Display order = the matched order; mapped back to history indices (all contain 'a').
        let expected = vec![0usize, 1usize, 3usize];
        assert_eq!(filtered, expected);
        // mapped_index reads the global FILTERED; construct it to verify boundary behavior.
        super::CLIPBOARD_UI.with(|ui| ui.borrow_mut().filtered = filtered.clone());
        assert_eq!(mapped_index(0), Some(0));
        assert_eq!(mapped_index(2), Some(3));
        assert_eq!(mapped_index(3), None);
    }
    #[test]
    fn estimate_lines_handles_width_and_newlines() {
        use super::estimate_lines;
        // short text stays on one line.
        assert_eq!(estimate_lines("hello", 60), 1);
        // Exactly 60 units fill a line (no wrap); 61 units wrap to 2 lines.
        assert_eq!(estimate_lines(&"a".repeat(60), 60), 1);
        assert_eq!(estimate_lines(&"a".repeat(61), 60), 2);
        // CJK counts as 2 units: 30 hanzi fill a line, the 31st wraps.
        let hanzi: String = "中".repeat(30);
        assert_eq!(estimate_lines(&hanzi, 60), 1);
        assert_eq!(estimate_lines(&(hanzi + "中"), 60), 2);
        // Explicit newlines start new lines.
        assert_eq!(estimate_lines("ab\ncd", 60), 2);
        assert_eq!(estimate_lines("ab\ncd\nef", 60), 3);
    }
    #[test]
    fn estimate_lines_keeps_unicode_grapheme_sequences_in_one_fallback_line() {
        use super::estimate_lines;
        // Emoji ZWJ sequences and combining marks must not panic or be treated as truncation
        // boundaries; this fallback only estimates detail height when AppKit is unavailable.
        assert_eq!(estimate_lines("👨‍👩‍👧‍👦", 60), 1);
        assert_eq!(estimate_lines("e\u{301}", 60), 1);
        assert_eq!(
            estimate_lines("https://example.com/".repeat(4).as_str(), 60),
            2
        );
    }
    #[test]
    fn row_pitch_is_fixed_per_kind_with_group_headers() {
        use super::{compute_pitches, GROUP_H, ROW_H};
        // In the same time group: the first row carries the header; the rest share an
        // identical pitch (short/long alike).
        let texts = vec![entry("short"), entry(&"长".repeat(100))];
        let pitches = compute_pitches(&texts);
        // The first = header + row; the second = the plain pitch.
        assert_eq!(pitches[0], GROUP_H + ROW_H);
        assert_eq!(pitches[1], ROW_H);
    }
    #[test]
    fn group_headers_break_into_new_groups() {
        use super::{compute_pitches, GROUP_H, ROW_H};
        // The first entry (no timestamp -> Earlier) carries the header; the rest of the
        // same group does not.
        let now = super::now_secs();
        let mut old = entry("old");
        old.copied_at = Some(now - 3 * 86400);
        let texts = vec![old.clone(), old.clone(), old.clone()];
        let pitches = compute_pitches(&texts);
        // The first = header + row; the others are plain pitches.
        assert_eq!(pitches[0] - pitches[1], GROUP_H);
        assert_eq!(pitches[1], pitches[2]);
        // A today entry after earlier ones gets its own header too (equal pitches).
        let mut today = entry("today");
        today.copied_at = Some(now - 60);
        let texts = vec![old.clone(), today];
        let pitches = compute_pitches(&texts);
        assert_eq!(pitches[0], pitches[1]);
        // Text and image rows share the uniform 61pt pitch -- equal after removing the
        // header.
        let mixed = vec![entry("a"), entry_image(b"png")];
        let p2 = compute_pitches(&mixed);
        assert_eq!(p2[1], p2[0] - GROUP_H);
        // Sanity-check the pitch constant (guards regressions): a uniform 61pt row.
        const { assert!(ROW_H >= 50.0 && ROW_H < 80.0) };
    }
    #[test]
    fn classify_text_distinguishes_urls_and_code() {
        use super::{classify_text, TextKind};
        // URL: a scheme or a www. prefix.
        assert_eq!(classify_text("https://github.com"), TextKind::Url);
        assert_eq!(classify_text("www.example.com/a"), TextKind::Url);
        assert_eq!(classify_text("a://b"), TextKind::Url);
        // A URL field inside structured content must not classify the whole JSON as a link.
        assert_eq!(
            classify_text(r#"{"homepage":"https://example.com","enabled":true}"#),
            TextKind::Code
        );
        // Code/prose containing a URL is not a standalone link and must not turn the entire row
        // into Link.
        assert_eq!(
            classify_text("const endpoint = \"https://api.example.com\";\nfetch(endpoint);"),
            TextKind::Code
        );
        assert_eq!(
            classify_text("See https://example.com for details"),
            TextKind::Plain
        );
        // Code: multi-line + obvious code cues.
        assert_eq!(
            classify_text("fn main() {\n    let x = 1;\n}"),
            TextKind::Code
        );
        assert_eq!(
            classify_text("int main() {\n  return 0;\n}"),
            TextKind::Code
        );
        // Conservative: single-line paren prose and plain text are not code; empty is safe.
        assert_eq!(
            classify_text("just some (parenthesis) prose"),
            TextKind::Plain
        );
        assert_eq!(classify_text("hello world"), TextKind::Plain);
        assert_eq!(classify_text(""), TextKind::Plain);
        assert_eq!(classify_text("  "), TextKind::Plain);
    }
    #[test]
    fn formatted_code_breaks_at_safe_points_and_maps_back_to_source() {
        use crate::clipboard_highlight::format_code_for_display;
        use objc2_foundation::NSRange;
        let source =
            "const result = veryLongObjectName.veryLongMethodName(firstArgument, secondArgument);";
        let formatted = format_code_for_display(source, 32);
        assert!(formatted.text.contains('·'));
        // The method name stays intact; breaks land at call/method-chain boundaries, never in
        // the middle of an identifier.
        assert!(formatted
            .text
            .lines()
            .any(|line| line.contains("veryLongMethodName(")));
        let display_len = formatted.text.encode_utf16().count();
        let source_range = formatted
            .source_map
            .source_range(NSRange::new(0, display_len));
        assert_eq!(source_range.length, source.encode_utf16().count());
    }
    #[test]
    fn source_icon_visibility_follows_the_source_display_toggle() {
        use super::should_show_source_icon;
        let mut e = entry("copied");
        e.source_key = "com.example.app".to_string();
        assert!(should_show_source_icon(true, &e));
        assert!(!should_show_source_icon(false, &e));
        e.source_key.clear();
        assert!(!should_show_source_icon(true, &e));
    }
    #[test]
    fn build_meta_text_joins_app_and_relative_time() {
        use super::build_meta_text;
        let mut e = entry_with_source("hello", "Safari");
        e.copied_at = Some(super::now_secs() - 5);
        // toggle on + source + time (the mockup's "app · time"; no kind badge).
        let m = build_meta_text(&e, true);
        assert!(m.starts_with("Safari · "));
        // Relative time is localized and parallel tests may change the global locale; verify
        // only that its dynamic segment is present.
        assert!(m.len() > "Safari · ".len(), "got {m}");
        // Toggle off -> no source name, just the time.
        let m2 = build_meta_text(&e, false);
        assert!(!m2.contains("Safari"));
        assert!(!m2.is_empty());
        // Legacy (no timestamp) -> the source name only.
        let legacy = entry_with_source("t", "Safari");
        let ml = build_meta_text(&legacy, true);
        assert_eq!(ml, "Safari");
        // No source (toggle on) -> the "unknown source" placeholder.
        let bare = entry("t");
        assert_eq!(
            build_meta_text(&bare, true),
            super::t("clipboard.unknown_source")
        );
    }
    #[test]
    fn build_meta_text_reports_source_line_count_after_time() {
        use super::build_meta_text;
        let mut e = entry_with_source("long text\n    ", "Safari");
        e.copied_at = Some(super::now_secs() - 5);
        let m = build_meta_text(&e, true);
        assert!(m.starts_with("Safari · "));
        let line_label = super::tf("clipboard.meta_lines_other", &[("count", "2")]);
        assert!(m.ends_with(&line_label), "got {m}");
        assert!(m.find(&line_label).unwrap() > m.find(" · ").unwrap());
        // Soft wrapping from a long single line is not a source line break.
        let single = entry_with_source(&"长".repeat(200), "Safari");
        assert!(!build_meta_text(&single, true).contains(&line_label));
    }
    #[test]
    fn physical_line_count_preserves_trailing_and_empty_lines() {
        use super::physical_line_count;
        assert_eq!(physical_line_count("single"), None);
        assert_eq!(physical_line_count("first\nsecond"), Some(2));
        assert_eq!(physical_line_count("first\n    "), Some(2));
        assert_eq!(physical_line_count("first\n\n"), Some(3));
    }
    #[test]
    fn format_copied_at_is_mm_dd_hh_mm() {
        use super::format_copied_at;
        // Timezone-independent structure assertion: length 11, shaped "MM-dd HH:mm".
        let s = format_copied_at(1755000000);
        assert_eq!(s.len(), 11, "got {s}");
        assert_eq!(s.as_bytes()[2], b'-', "got {s}");
        assert_eq!(s.as_bytes()[5], b' ', "got {s}");
        assert_eq!(s.as_bytes()[8], b':', "got {s}");
        // all-numeric fields.
        let digits: Vec<u8> = s
            .bytes()
            .filter(|b| !matches!(b, b'-' | b' ' | b':'))
            .collect();
        assert_eq!(digits.len(), 8);
        assert!(digits.iter().all(u8::is_ascii_digit));
    }
    #[test]
    fn format_save_stamp_is_yyyy_mm_dd_hh_mm_ss_with_dots() {
        use super::format_save_stamp;
        // Timezone-independent structure assertion: length 19, shaped
        // "YYYY-MM-DD HH.MM.SS" (the time separators MUST be dots -- colons are
        // illegal in HFS+/Finder filenames).
        let s = format_save_stamp(1755000000);
        assert_eq!(s.len(), 19, "got {s}");
        assert_eq!(s.as_bytes()[4], b'-', "got {s}");
        assert_eq!(s.as_bytes()[7], b'-', "got {s}");
        assert_eq!(s.as_bytes()[10], b' ', "got {s}");
        assert_eq!(s.as_bytes()[13], b'.', "got {s}");
        assert_eq!(s.as_bytes()[16], b'.', "got {s}");
        // all-numeric fields.
        let digits: Vec<u8> = s
            .bytes()
            .filter(|b| !matches!(b, b'-' | b' ' | b'.'))
            .collect();
        assert_eq!(digits.len(), 14);
        assert!(digits.iter().all(u8::is_ascii_digit));
        // the year field is 4 digits wide.
        assert!(s[..=3].bytes().all(|b| b.is_ascii_digit()), "got {s}");
    }
    #[test]
    fn nav_arrow_moves_and_wraps() {
        use super::{nav_arrow, NO_SELECTION};
        // Down advances, up retreats, wrapping at both ends.
        assert_eq!(nav_arrow(125, 0, 3), Some(1));
        assert_eq!(nav_arrow(125, 2, 3), Some(0)); // wraps to top
        assert_eq!(nav_arrow(126, 2, 3), Some(1));
        assert_eq!(nav_arrow(126, 0, 3), Some(2)); // wraps to bottom
                                                   // Other keys are ignored; an empty history never moves.
        assert_eq!(nav_arrow(36, 1, 3), None);
        assert_eq!(nav_arrow(125, 0, 0), None);
        // The no-selection sentinel (usize::MAX): no overflow; treated as "no selection"
        // (↓ -> 0, ↑ -> the tail).
        assert_eq!(nav_arrow(125, NO_SELECTION, 3), Some(0));
        assert_eq!(nav_arrow(126, NO_SELECTION, 3), Some(2));
    }
    #[test]
    fn selection_scroll_offset_keeps_the_selected_row_visible() {
        use super::selection_scroll_offset;
        // A row below the viewport scrolls down until its bottom meets the viewport bottom.
        assert_eq!(
            selection_scroll_offset(0.0, 300.0, 1000.0, 500.0, 78.0),
            278.0
        );
        // A row above the viewport scrolls up until its top is visible.
        assert_eq!(
            selection_scroll_offset(300.0, 300.0, 1000.0, 200.0, 78.0),
            200.0
        );
        // An already-visible row keeps the current offset; a target past the document clamps
        // to the maximum offset.
        assert_eq!(
            selection_scroll_offset(300.0, 300.0, 1000.0, 350.0, 78.0),
            300.0
        );
        assert_eq!(
            selection_scroll_offset(0.0, 300.0, 1000.0, 950.0, 78.0),
            700.0
        );
    }
    #[test]
    fn clamp_selection_after_delete_lands_on_the_new_tail() {
        use super::{clamp_selection, NO_SELECTION};
        // After deleting the tail: an out-of-range selection clamps to the new tail
        // (the core scenario of this fix).
        assert_eq!(clamp_selection(3, 3), 2);
        assert_eq!(clamp_selection(2, 3), 2); // in range, untouched
        assert_eq!(clamp_selection(0, 1), 0);
        // Empty list: no tail to clamp to, returned unchanged (the empty-state hint
        // renders no rows, so no highlight concern).
        assert_eq!(clamp_selection(0, 0), 0);
        assert_eq!(clamp_selection(3, 0), 3);
        // The no-selection sentinel (search-field focus): never resurrect a highlight.
        assert_eq!(clamp_selection(NO_SELECTION, 3), NO_SELECTION);
        // Way out of range (accumulated deletions) -> the tail.
        assert_eq!(clamp_selection(5, 2), 1);
    }
    #[test]
    fn screen_containing_finds_the_cursor_screen() {
        use super::screen_containing;
        use objc2_foundation::{NSPoint, NSRect, NSSize};
        // Main screen at (0,0) 1440x900; a second screen to the left at (-800,0) 800x900.
        let frames = [
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1440.0, 900.0)),
            NSRect::new(NSPoint::new(-800.0, 0.0), NSSize::new(800.0, 900.0)),
        ];
        // cursor on the main screen.
        assert_eq!(
            screen_containing(NSPoint::new(700.0, 500.0), &frames),
            Some(frames[0])
        );
        // cursor on the left second screen (negative x).
        assert_eq!(
            screen_containing(NSPoint::new(-400.0, 200.0), &frames),
            Some(frames[1])
        );
        // cursor off-screen -> None.
        assert_eq!(
            screen_containing(NSPoint::new(3000.0, 500.0), &frames),
            None
        );
    }
    #[test]
    fn picker_frame_follows_cursor_with_flips() {
        use super::{picker_frame_for, PICKER_CURSOR_OFF, PICKER_EDGE_MARGIN};
        use objc2_foundation::{NSPoint, NSRect, NSSize};
        let screen = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1440.0, 900.0));
        let w = 420.0;
        let h = 300.0;
        // Centered cursor: the panel sits 16pt to its bottom-right.
        let f = picker_frame_for(NSPoint::new(700.0, 500.0), screen, w, h);
        assert_eq!(f.origin.x, 700.0 + PICKER_CURSOR_OFF);
        assert_eq!(f.origin.y, 500.0 - h - PICKER_CURSOR_OFF);
        // Not enough room on the right -> flip to the cursor's left.
        let f = picker_frame_for(NSPoint::new(1300.0, 500.0), screen, w, h);
        assert_eq!(f.origin.x, 1300.0 - w - PICKER_CURSOR_OFF);
        // Not enough room below -> flip above the cursor.
        let f = picker_frame_for(NSPoint::new(700.0, 50.0), screen, w, h);
        assert_eq!(f.origin.y, 50.0 + PICKER_CURSOR_OFF);
        // Bottom-right corner: both flips still overflow -> clamped inside the screen.
        let f = picker_frame_for(NSPoint::new(1400.0, 20.0), screen, w, h);
        assert!(f.origin.x >= screen.origin.x + PICKER_EDGE_MARGIN);
        assert!(f.origin.x + w <= screen.origin.x + screen.size.width - PICKER_EDGE_MARGIN);
        assert!(f.origin.y >= screen.origin.y + PICKER_EDGE_MARGIN);
        assert!(f.origin.y + h <= screen.origin.y + screen.size.height - PICKER_EDGE_MARGIN);
    }
    #[test]
    fn detail_frame_stays_right_of_picker_and_clamps() {
        use super::{detail_frame_for, DETAIL_GAP, PICKER_EDGE_MARGIN};
        use objc2_foundation::{NSPoint, NSRect, NSSize};
        let screen = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1440.0, 900.0));
        let w = 480.0;
        let h = 400.0;
        // Normal: right of the picker, top aligned to align_top_y (the selected row's screen
        // y; a taller panel extends downward).
        let picker = NSRect::new(NSPoint::new(0.0, 300.0), NSSize::new(420.0, 500.0));
        let align = 300.0 + 500.0; // this case equals the window top (unscrolled, rows aligned to the top)
        let f = detail_frame_for(picker, align, screen, w, h);
        assert_eq!(f.origin.x, 0.0 + 420.0 + DETAIL_GAP);
        assert_eq!(f.origin.y, align - h);
        // When the right side is tight, stay in the right-side region instead of flipping left.
        let picker = NSRect::new(NSPoint::new(1000.0, 300.0), NSSize::new(420.0, 500.0));
        let align = 300.0 + 500.0;
        let f = detail_frame_for(picker, align, screen, w, h);
        assert_eq!(f.origin.x, 1440.0 - PICKER_EDGE_MARGIN - w);
        assert_eq!(f.origin.y, align - h);
        // Neither side fits -> clamped inside the screen.
        let picker = NSRect::new(NSPoint::new(0.0, 300.0), NSSize::new(1440.0, 500.0));
        let align = 300.0 + 500.0;
        let f = detail_frame_for(picker, align, screen, w, h);
        assert!(f.origin.x >= screen.origin.x + PICKER_EDGE_MARGIN);
        assert!(f.origin.x + w <= screen.origin.x + screen.size.width - PICKER_EDGE_MARGIN);
        assert!(f.origin.y >= screen.origin.y + PICKER_EDGE_MARGIN);
        assert!(f.origin.y + h <= screen.origin.y + screen.size.height - PICKER_EDGE_MARGIN);
        // With few entries the window is floored at the min height: the selected row's top
        // is NOT the window top (38pt of header strip + row offset below it) -- the detail
        // must align to the row's screen y, not float at the window top.
        let picker = NSRect::new(NSPoint::new(0.0, 300.0), NSSize::new(420.0, 250.0));
        let row_top_y = 300.0 + 250.0 - 44.0; // first row top = window top - 44pt (header bar + row offset)
        let f = detail_frame_for(picker, row_top_y, screen, 480.0, 200.0);
        assert_eq!(f.origin.y, row_top_y - 200.0);
        // A tall detail aligned to the top clamps upward, keeping both edges inside the picker.
        let picker = NSRect::new(NSPoint::new(0.0, 800.0), NSSize::new(420.0, 100.0));
        let f = detail_frame_for(picker, 900.0, screen, 480.0, 100.0);
        assert_eq!(f.origin.y, picker.origin.y);
        assert_eq!(
            f.origin.y + f.size.height,
            picker.origin.y + picker.size.height
        );
        // The same holds at the screen bottom: detail may not pass either picker edge.
        let picker = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(420.0, 50.0));
        let f = detail_frame_for(picker, 50.0, screen, 480.0, 50.0);
        assert_eq!(f.origin.y, picker.origin.y);
        assert_eq!(
            f.origin.y + f.size.height,
            picker.origin.y + picker.size.height
        );
    }
    #[test]
    fn detail_group_centers_picker_and_keeps_detail_on_the_right() {
        use super::{detail_group_frames, DETAIL_GAP, PICKER_EDGE_MARGIN, PICKER_W};
        use objc2_foundation::{NSPoint, NSRect, NSSize};
        let screen = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1920.0, 1080.0));
        let picker = NSRect::new(NSPoint::new(680.0, 300.0), NSSize::new(PICKER_W, 600.0));
        let (picker_frame, detail_frame) =
            detail_group_frames(picker, 820.0, screen, 720.0, 400.0, true, 960.0);
        let group_w = PICKER_W + DETAIL_GAP + 720.0;
        assert_eq!(picker_frame.origin.x, (1920.0 - group_w) / 2.0);
        assert_eq!(
            detail_frame.origin.x,
            picker_frame.origin.x + PICKER_W + DETAIL_GAP
        );
        assert!(detail_frame.origin.x >= picker_frame.origin.x + PICKER_W);
        assert!(detail_frame.origin.x + detail_frame.size.width <= 1920.0 - PICKER_EDGE_MARGIN);
    }
    #[test]
    fn detail_group_clamps_without_flipping_on_a_narrow_screen() {
        use super::{detail_group_frames, PICKER_EDGE_MARGIN, PICKER_W};
        use objc2_foundation::{NSPoint, NSRect, NSSize};
        let screen = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1000.0, 800.0));
        let picker = NSRect::new(NSPoint::new(0.0, 200.0), NSSize::new(PICKER_W, 400.0));
        let (picker_frame, detail_frame) =
            detail_group_frames(picker, 600.0, screen, 720.0, 300.0, true, 500.0);
        assert_eq!(picker_frame.origin.x, PICKER_EDGE_MARGIN);
        assert!(detail_frame.origin.x >= screen.origin.x + PICKER_EDGE_MARGIN);
        assert!(
            detail_frame.origin.x + detail_frame.size.width
                <= screen.origin.x + screen.size.width - PICKER_EDGE_MARGIN
        );
    }
    #[test]
    fn detail_text_units_scales_with_width() {
        use super::detail_text_units;
        // The row content width (~520pt) maps to ~60 units, the same basis as the row buttons.
        let cw = super::content_width();
        assert_eq!(detail_text_units(cw), 60);
        assert_eq!(detail_text_units(cw / 2.0), 30);
        // A tiny width floors at 1 unit (never 0).
        assert_eq!(detail_text_units(1.0), 1);
    }
    #[test]
    fn detail_text_size_clamps_to_screen_height() {
        use super::{detail_max_height, detail_text_size, TextKind};
        use objc2_foundation::{NSPoint, NSRect, NSSize};
        let picker = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(560.0, 400.0));
        let max_height = detail_max_height(picker);
        // Short text -> the minimum; long text -> the picker height (scrolling only beyond it).
        let (plain_w, plain_h) = detail_text_size("hi", TextKind::Plain, max_height);
        assert_eq!(plain_w, super::DETAIL_MAX_W);
        assert_eq!(plain_h, super::DETAIL_PANEL_MIN_H);
        // Two lines must grow beyond the 78pt minimum, leaving room for both 11pt sides of
        // textContainerInset; otherwise the document exceeds the scroll view and exposes a
        // scrollbar despite having no content that should need scrolling.
        let (_, two_line_h) = detail_text_size("first\nsecond", TextKind::Plain, max_height);
        assert_eq!(
            two_line_h,
            super::DETAIL_LINE_H * 2.0
                + super::DETAIL_PAD * 2.0
                + super::DETAIL_TEXT_INSET_H
                + super::DETAIL_CHROME_H
        );
        assert!(two_line_h > super::DETAIL_PANEL_MIN_H);
        let long = "a".repeat(200_000);
        assert_eq!(
            detail_text_size(&long, TextKind::Plain, max_height).1,
            max_height
        );
        // Long code lines soft-wrap visually, while the detail panel still clamps to its height.
        let long_code = "x".repeat(200_000);
        let (code_w, code_h) = detail_text_size(&long_code, TextKind::Code, max_height);
        assert_eq!(code_w, super::DETAIL_CODE_MAX_W);
        assert_eq!(code_h, max_height);
        // With wrapping off, the same long line occupies one visual line and overflows horizontally.
        let (_, no_wrap_h) = super::detail_unwrapped_code_size(&long_code, max_height);
        assert_eq!(no_wrap_h, super::DETAIL_PANEL_MIN_H);
    }
    #[test]
    fn toggle_pin_on_roundtrips_pinned_state() {
        use super::toggle_pin_on;
        let mut h = vec![entry("a"), entry("b"), entry("c")];
        // Pin: the entry moves to the top of the pinned block; returns (true, new index 0).
        let (now_pinned, new_idx) = toggle_pin_on(&mut h, 1);
        assert!(now_pinned);
        assert_eq!(new_idx, 0);
        assert!(h[0].pinned);
        assert_eq!(h[0].text, "b");
        // Toggle again: unpin, to the top of the unpinned block (new index = right after
        // the pinned block = 0 here).
        let (now_pinned, new_idx) = toggle_pin_on(&mut h, 0);
        assert!(!now_pinned);
        assert_eq!(new_idx, 0);
        assert!(!h[0].pinned);
        // Out of range: safe no-op, returns (false, idx).
        let (ok, idx) = toggle_pin_on(&mut h, 99);
        assert!(!ok);
        assert_eq!(idx, 99);
        assert_eq!(h.len(), 3);
    }
    #[test]
    fn toggle_pin_on_returns_new_index_for_follow_selection() {
        // Follow-pin selection locates the entry by its POST-REORDER index; the old index
        // already points at a different entry after the reorder.
        use super::{pin_entry, unpin_entry};
        // With an existing pinned block, a newly pinned entry lands at index 0; unpinning
        // lands right after the pinned block.
        let mut h = vec![entry("p1"), entry("u1"), entry("u2")];
        pin_entry(&mut h, 0); // p1 already pinned
        assert_eq!(pin_entry(&mut h, 1), 0); // u1 pinned -> index 0
        assert_eq!(h[0].text, "u1");
        assert!(h[0].pinned);
        assert_eq!(unpin_entry(&mut h, 0), 1); // u1 unpinned -> index 1 after the pinned region
        assert_eq!(h[1].text, "u1");
        assert!(!h[1].pinned);
    }
    // Runs the real app binary as a subprocess (--smoke-clipboard): AppKit control construction
    // is strictly main-thread-only, so the test harness's worker threads can't build the picker.
    // Two show_picker calls exercise rebuild_rows' row cleanup (a double-release UAF that once
    // segfaulted on the second summon).
    #[test]
    #[ignore]
    fn picker_rebuild_smoke() {
        // Prerequisite: cargo build has produced target/debug/oh-my-tab.
        let exe = std::env::current_exe().expect("current exe");
        let app = exe
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.join("oh-my-tab"))
            .expect("app binary path");
        assert!(
            app.exists(),
            "app binary missing at {}: run `cargo build` first",
            app.display()
        );
        let out = std::process::Command::new(&app)
            .arg("--smoke-clipboard")
            .output()
            .expect("failed to spawn app");
        assert!(
            out.status.success(),
            "clipboard picker smoke failed (exit {:?})\nstderr:\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
