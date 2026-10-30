//! History clipboard module (text + images, optional persistence).
//!
//! Architecture:
//! - A main-thread NSTimer polls NSPasteboard's changeCount every 0.5s; when it changes,
//!   the text/image is read into the history (duplicates are skipped, overflow trimmed).
//! - The configured history shortcut is detected by the event_monitor tap and marshalled to the
//!   main thread via the bridge (on_clipboard_toggle), showing/hiding the picker. Tab cycles filters; arrow keys
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
use crate::callback_guard;
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
use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSPoint, NSRange, NSRect, NSSize};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ffi::{c_void, CString};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};
mod crypto;
mod detail;
mod drag;
mod image_cache;
mod keyring;
mod keyring_acl;
mod model;
mod monitor;
mod notifications;
mod paste_handback;
mod pasteboard;
mod persist;
mod picker;
mod pin;
mod search;
mod smoke;
mod storage;
mod text_style;
use crypto::*;
use detail::*;
use image_cache::*;
use keyring::*;
use model::*;
use monitor::*;
use notifications::*;
use pasteboard::*;
use persist::*;
use picker::*;
use pin::*;
use search::*;
use smoke::*;
use storage::*;
use text_style::*;
// Entry points exposed to the rest of the crate (implemented in the child modules).
pub(crate) use detail::{apply_backdrop_material, apply_theme, set_picker_glass_inactive};
/// The detail pane's pure geometry (the mock's `.detail-pane` padding and `.meta-table` grid),
/// re-exported so the sizing helpers and their tests can name it without the model module.
#[allow(unused_imports)] // the sizing helpers and tests use them through this path
use detail::{
    detail_document_height, detail_pane_layout, detail_pane_min_height, media_body_frames,
};
#[allow(unused_imports)] // the meta table's rows are built from the model's field table
use model::{format_byte_size, meta_table, MetaField};
pub(crate) use monitor::{start, stop};
pub(crate) use notifications::{
    applied_notice_labels, e2e_settings_click_delivery_ms, e2e_settings_click_ms,
    keychain_row_status, record_applied_about_status, record_applied_action, storage_action,
    AppliedAction, NoticeSurface, StorageAction,
};
/// Whether a pinned paste is waiting for its target to take keyboard focus (for the smoke runner).
pub(crate) use paste_handback::handback_in_flight;
pub(crate) use paste_handback::note_external_app_activated;
pub(crate) use persist::{clear_on_quit_enabled, discard_history_on_disk};
pub(crate) use picker::{
    on_clipboard_toggle, show_and_pin_for_menu, show_picker_detail_for_development,
    show_picker_for_development,
};
/// The frozen panel height while pinned, for the geometry assertions.
pub(crate) use pin::frozen_height;
/// Whether the panel-level pin is set, for the menu bar's mirrored check state.
pub(crate) use pin::is_pinned;
pub(crate) use smoke::{set_smoke_mode, smoke_runner};
pub(crate) use text_style::refresh_localized_ui;
pub(crate) use text_style::update_clear_action_labels;
/// Whether the history file currently exists on disk (used by the onboarding smoke to assert that
/// turning the feature off really cleared it).
pub(crate) fn history_file_exists() -> bool {
    persist::history_file_path().exists()
}

/// The clipboard's storage state, for `--e2e-state` (see `storage.rs` for the three states).
pub(crate) fn storage_state_label() -> &'static str {
    storage::state_label()
}

/// Why the storage state is what it is (a short label, never key material).
pub(crate) fn storage_reason_label() -> &'static str {
    storage::reason_label()
}

/// Where the master key came from in this process (`keychain` / `test` / `none`).
pub(crate) fn key_provider_label() -> &'static str {
    keyring::backend_label(keyring::current_backend())
}

/// Whether this process monitors the system pasteboard. A smoke launch never does (it would record
/// the developer's real clipboard into a temporary history under the fixed test key), and the
/// tier-A smoke asserts it.
pub(crate) fn pasteboard_monitor_active() -> bool {
    monitor::polling_active()
}

/// The one-line "clipboard history is unavailable right now" copy for the current storage state, in
/// the active locale. Used by the settings page and by the picker, so all three surfaces (including
/// the system notification) say the same thing.
/// Whether the session holds a usable clipboard key (the About page's keychain row reports this, and
/// the "grant keychain access" action is offered exactly when it is false).
pub(crate) fn storage_key_available() -> bool {
    storage::key().is_some()
}

/// The form the keychain item's ACL uses (`--e2e-state`). A form report, not a stability verdict: what
/// decides whether a rebuild keeps working is the *creating* app's signing identity (see `keyring_acl`).
pub(crate) fn keychain_acl_label() -> &'static str {
    keyring_acl::cached_acl_identity().label()
}

pub(crate) fn unavailable_copy() -> Option<String> {
    notifications::unavailable_copy()
}

/// Whether a superseded plaintext history index survived this session (reported in `--e2e-state`).
pub(crate) fn legacy_index_leftover() -> bool {
    persist::legacy_index_leftover()
}

/// The user asked for keychain access from the settings banner, the panel's empty state or the About
/// page: re-read the key (one system prompt) and re-apply the storage state either way.
pub(crate) fn retry_keychain_access() {
    if !CONFIG.read().unwrap().clipboard.enabled {
        // The feature is off: acquire the key so the system prompt appears and the row can report
        // "granted", but never touch the storage set -- loading, migrating, sweeping and saving all
        // belong to the master switch.
        log_info!("[clip] authorization requested while the clipboard feature is off; key only");
        persist::acquire_key_only();
        return;
    }
    persist::retry_load_history();
}

/// The storage notice's "clear and restart" action (settings banner and picker empty state): delete
/// the storage set through the purge transaction and start a fresh history.
///
/// Eligibility is decided HERE, before the intent is recorded. Afterwards the deletion is a
/// user-authorized purge and completes whatever the switch or the state does next — the same
/// transaction the switch-off path runs, and the durable intent is what makes it resumable. A
/// request that is no longer eligible is refused rather than carried out: the state can recover
/// between the click and this call, and clearing a storage set that just became readable would
/// delete records the user never agreed to lose.
pub(crate) fn clear_storage_and_restart() {
    if !CONFIG.read().unwrap().clipboard.enabled {
        log_info!("[clip] clear-and-restart ignored: the clipboard feature is off");
        return;
    }
    if storage_action() != StorageAction::ClearAndRestart {
        log_info!(
            "[clip] clear-and-restart ignored: the storage state no longer admits it ({}/{})",
            storage_state_label(),
            storage_reason_label()
        );
        return;
    }
    persist::clear_storage_and_restart();
}

/// Finish a purge an earlier session could not complete. Called at launch and on every config
/// change, BEFORE anything loads or writes, and whether the clipboard switch is on or off
/// (`clear_on_quit` failures happen with the switch on).
pub(crate) use storage::PurgeProgress;

pub(crate) fn process_pending_purge() -> storage::PurgeProgress {
    storage::process_pending_purge()
}

/// Whether the session is waiting for a fresh load to decide its storage state (e.g. right after a
/// purge completed). The caller uses this to schedule one instead of leaving the session unable to
/// save until the next launch.
pub(crate) fn needs_load() -> bool {
    matches!(storage::state(), storage::StorageState::Uninitialized)
}

/// Schedule a fresh storage load on the worker. Separate from `start()`: `start()` also installs the
/// polling timer and returns early when it is already running, while a session recovering from a
/// purge needs the load even then.
pub(crate) fn reload_storage() {
    persist::load_history();
}

/// The clipboard switch was turned off: drop the in-memory history and delete everything it left
/// on disk, so "off" means the app keeps no records at all.
pub(crate) fn clear_history_and_disk() {
    clear_history_state();
    clear_search();
    hide_detail();
    unsafe { rebuild_rows() };
    // The purge committed a new storage state (uninitialized, or blocked on a purge that could not
    // finish). No load follows a switch-off, so this is the only place that can tell the three
    // surfaces — without it a stale notice keeps accusing the keychain while the feature is off.
    persist::refresh_storage_ui();
}

/// Drop what THIS session recorded, without touching anything on disk: the in-memory entries, the
/// undo slot and (by the caller) the pending image bytes. Used when the storage turns out to be
/// unavailable or blocked -- that must never delete the user's stored history, which is exactly what
/// the full switch-off clear below does.
pub(super) fn clear_session_records() {
    let removed = {
        let mut history = CLIP_HISTORY.lock().unwrap();
        remove_history_scope(&mut history, true, ClipFilter::All, "").len()
    };
    if removed > 0 {
        log_info!(
            "[clip] discarded {removed} records from this session (storage is not available)"
        );
    }
    // The undo slot is part of the history: leaving it would let Cmd+Z write a record back.
    notifications::discard_deleted_clipboard_entry();
    notifications::cancel_clipboard_undo_timer();
    DELETED_CLIPBOARD_GENERATION.fetch_add(1, Ordering::SeqCst);
}

/// The recorded-data half of the switch-off: the in-memory history, the undo slot, the file and the
/// image cache. Split out so the picker UI (which needs the main thread) is not part of it.
pub(super) fn clear_history_state() {
    {
        let mut history = CLIP_HISTORY.lock().unwrap();
        let removed = remove_history_scope(&mut history, true, ClipFilter::All, "");
        if !removed.is_empty() {
            log_info!(
                "Clipboard history cleared with the feature switch ({} entries).",
                removed.len()
            );
        }
    }
    // The 30s undo slot is part of the history: leaving it would let Cmd+Z restore an entry (and
    // rewrite the file) after the feature, and its records, were cleared.
    notifications::discard_deleted_clipboard_entry();
    notifications::cancel_clipboard_undo_timer();
    DELETED_CLIPBOARD_GENERATION.fetch_add(1, Ordering::SeqCst);
    // Deletes the file and wipes the image cache (the per-hash deletes above would be redundant).
    discard_history_on_disk();
}

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
/// The image rows' thumbnail box (72x44, control radius, faint fill + inner ring, mirroring the
/// HTML mockup).
/// The mock's `.row-thumb`: a 30×30 well (radius 8) holding the type icon or the image
/// thumbnail. At image_rows > 1 the well keeps the row's padding and grows with the row:
/// `ROW_H * n - (ROW_H - THUMB_H)`, i.e. 30/70/110pt for stops 1/2/3.
const THUMB_W: f64 = 30.0;
const THUMB_H: f64 = 30.0;
/// The thumbnail well's height at the configured `clipboard.image_rows` multiplier.
pub(crate) fn thumb_h() -> f64 {
    ROW_H * model::image_row_multiplier() - (ROW_H - THUMB_H)
}
const THUMB_R: f64 = crate::theme::RADIUS_CONTROL;
/// gap between the app icon and the thumb in the canvas.
/// keycode used when synthesizing Cmd+V.
const VK_V: u16 = keyboard::VK_V;
/// Command modifier mask for synthesized paste.
const K_CG_EVENT_FLAG_MASK_COMMAND: CGEventFlags = keyboard::FLAG_COMMAND;
/// polling interval (seconds)
const POLL_INTERVAL: f64 = 0.5;
/// Maximum picker panel height from the clipboard-v2 mockup (580px). On shorter screens,
/// `show_picker` reduces it to leave room for the screen edge and elevation shadow.
const PICKER_MAX_HEIGHT: f64 = 580.0;
/// the picker's minimum height for regular lists.
const PICKER_MIN_HEIGHT: f64 = 280.0;
/// List-only width from clipboard-v2.html. Expanded geometry is owned by picker_layout.
const PICKER_W: f64 = 640.0;
/// Uniform single-line row height from the design.
const ROW_H: f64 = 40.0;
/// Detail body line height, computed from the font the text view actually uses so NSTextView's
/// line box can never exceed the estimate. The mock sets `line-height: 1.75` at 13px for plain
/// text; that ratio on this app's 12/14 scale step gives 24.5pt for FONT_CONTROL (the mock's own
/// ratio would put 14pt text at 21pt, tightening the reading surface off the shared scale).
const DETAIL_BODY_LINE_RATIO: f64 = 1.75;
const DETAIL_LINE_H: f64 = crate::theme::line_height(13.0, DETAIL_BODY_LINE_RATIO);
/// Detail body line height (the paragraph style the text view installs; equals `DETAIL_LINE_H`).
const DETAIL_BODY_LINE_H: f64 = DETAIL_LINE_H;
/// Trailing slack after an unwrapped code line's width, so its last glyph is never clipped by
/// the horizontal scroll range.
const DETAIL_TEXT_TRAILING_GAP: f64 = 12.0;
/// The mock's `.image-preview img { max-height: 300px }`.
const DETAIL_IMAGE_PREVIEW_MAX_H: f64 = 300.0;
/// The mock's `.code-line` grid: a 26px line-number gutter, a 6px gap, then the code.
const DETAIL_CODE_GUTTER_W: f64 = 26.0;
const DETAIL_CODE_GUTTER_GAP: f64 = 6.0;
/// The right-aligned line numbers: mono 11.5px (the mock's `.code-line` font size).
const DETAIL_GUTTER_FONT: f64 = 11.5;
/// Space the gutter keeps from the first code glyph, on the mock's `.ln { padding-right: 4px }`.
const DETAIL_CODE_GUTTER_PAD: f64 = 4.0;
// The gutter's ink on the mock is rgba(48,48,56,.36) over a light material. This panel draws its
// text with the system's dynamic label colours (glass::panel_ink), which stay legible across all
// four materials, so the gutter takes the muted role instead of a literal alpha that would
// vanish on the dark panel.
/// The image preview's well (the mock's `.image-preview`): a white wash plus a soft shadow.
const PREVIEW_WELL_LIGHT: u32 = 0xFFFF_FF4D;
const PREVIEW_WELL_DARK: u32 = 0xFFFF_FF14;
const PREVIEW_SHADOW_LIGHT: u32 = 0x2820_141F;
const PREVIEW_SHADOW_DARK: u32 = 0x0000_0040;
const PREVIEW_SHADOW_RADIUS: f64 = 28.0;
const PREVIEW_SHADOW_OFFSET_Y: f64 = 10.0;
const PREVIEW_SHADOW_OPACITY: f64 = 0.12;
/// Conservative fallback units for detail-text height estimation. List content no longer uses
/// character units for truncation; native cells lay it out from real font metrics. This value
/// is only used by the headless-safe detail document estimate.
#[cfg(test)]
const LINE_MAX_UNITS: usize = 60;
/// the list's side padding (8px).
const PAD_X: f64 = 10.0;
/// The row's padding, aligned to the spacing scale.
/// `.clip-row` vertical padding (4px each side on the 40px row).
const ROW_PAD_TOP: f64 = 4.0;
const ROW_PAD_R: f64 = 12.0;
const ROW_PAD_BOT: f64 = 4.0;
const ROW_PAD_L: f64 = 12.0;
/// the search bar's inner padding (12px).
const SEARCH_PAD_IN: f64 = 12.0;
/// The reserved search-icon column (the mockup's `.search-icon { width: 22px }`).
const SEARCH_ICON_W: f64 = 22.0;
/// The extra clear × size when a query exists.
const SEARCH_CLEAR_W: f64 = 18.0;
/// The capsule's glass corner clips content, so right-edge glyphs sit this far inside the
/// geometric edge. The clear × and its click target must agree on it or the visible glyph is
/// a dead zone beside a live invisible strip.
const SEARCH_TRAILING_INSET: f64 = 14.0;
/// Shared query font size for both editing and unfocused states.
const SEARCH_FONT_SIZE: f64 = 13.0;
/// Row action buttons meet the shared 28pt minimum hit target.
const ACTION_BTN: f64 = 22.0;
const ACTION_H: f64 = 22.0;
const ACTION_GAP: f64 = 1.0;
/// The point size of the row action glyphs (detail/pin/trash). One constant for all three: the
/// glyphs sit side by side in a row, so a different size for any one of them reads as a defect
/// (the detail toggle used to be a hand-drawn circle on its own 16pt canvas and did exactly that).
const ACTION_GLYPH_SIZE: f64 = 12.0;
/// The time-group header zone height.
const GROUP_H: f64 = 28.0;
/// The group label's top offset (vertically centered).
const GROUP_LABEL_PAD: f64 = 8.0;
/// The header strip's top padding: the v2 mock's `.panel-head` padding-top (16px). The whole
/// padding band is the panel's top drag surface (see `drag.rs`), so it also gives the top bar a
/// blank target without reserving height for a handle that no longer exists.
const TOP_PAD_Y: f64 = 16.0;
/// Pointer travel that separates a blank-area click from a panel drag: below it the press is a
/// no-op (a blank region has no click action), above it the panel follows the pointer.
const PICKER_DRAG_THRESHOLD: f64 = 4.0;
/// the search bar's height (the v2 mock's 40px).
const SEARCH_H: f64 = 40.0;
/// The search bar's side padding.
const SEARCH_PAD_X: f64 = 16.0;
/// The search bar's capsule radius: the v2 mock's 18px on a 40pt field. Deliberately not
/// `RADIUS_CONTROL` (8) -- the field is a pill in the mock, and the field's height/2 makes it read
/// as the search capsule rather than as one of the square corner controls.
const SEARCH_R: f64 = 18.0;
/// Corner close button on the picker's header: the mock's `.close-btn` 30pt wide hit target
/// with a 12pt glyph.
const PICKER_CLOSE_BTN_SIZE: f64 = 30.0;
const PICKER_CLOSE_BTN_R: f64 = crate::theme::RADIUS_CONTROL;
const PICKER_CLOSE_BTN_FONT: f64 = 12.0;
/// Gap between the search field's right edge and the corner close button.
const PICKER_CLOSE_BTN_GAP: f64 = 8.0;
/// The pin/gear "orb" buttons: the mock's `.orb` -- 34×34 circles, so their corner radius is
/// half their size rather than `RADIUS_CONTROL`.
const PICKER_ORB_SIZE: f64 = 34.0;
/// The options orb's "a filter is active" dot (the mock's `.orb-dot`). It hugs the orb's top-right
/// corner, so it is a tiny satellite rather than a control of its own.
pub(super) const PICKER_ORB_DOT_SIZE: f64 = 5.0;
/// The dot's gap from the orb's top and right edges. The orb clips its sublayers to its circle, so
/// the dot stays whole only while the disc fits inside it. Its centre is `17 - gap - 2.5` from the
/// orb's centre on both axes, so the disc fits while `sqrt(2) * (14.5 - gap) + 2.5 <= 17`, which
/// holds from `gap >= 14.5 - 14.5 / sqrt(2) = 4.2469...`. 6 leaves visible margin; the unit test
/// pins both sides of that boundary so the number here cannot drift from the predicate.
pub(super) const PICKER_ORB_DOT_GAP: f64 = 6.0;
/// The settings button's SF Symbol point size. A symbol image carries its own size (it does not
/// follow the button's font), so the corner's second glyph is sized here, a step above the close
/// button's 12pt text × so the two read as the same weight.
const PICKER_GEAR_GLYPH_SIZE: f64 = 13.0;
/// The pin button sits left of the close button: the same 28pt hit target and family as the gear,
/// so the row reads as one group. Its glyph size matches the gear's for the same reason (both are
/// SF Symbols, which do not follow the button's font).
const PICKER_PIN_GLYPH_SIZE: f64 = PICKER_GEAR_GLYPH_SIZE;
/// The gap under the search bar (the v2 mock's `.panel-head` padding-bottom, 10px).
const SEARCH_GAP_Y: f64 = 10.0;
/// The footer's height: the mock's `.panel-foot` 38px.
const FOOTER_H: f64 = 38.0;
/// the window's bottom padding.
const PAD_Y: f64 = 12.0;
/// The footer's side padding: the mock's `.panel-foot` 20px.
const FOOTER_PAD_X: f64 = 20.0;
/// the footer shortcut groups' spacing.
const FOOTER_GROUP_GAP: f64 = 16.0;
/// Horizontal breathing room a footer shortcut label's frame adds on top of the text it must
/// hold. The frame is allocated before the font is assigned, so this slack is what keeps the
/// label from wrapping out of its one-line-high field when the drawn font is the widest one the
/// label can legitimately use (see `legend_required_width`).
const FOOTER_LABEL_SLACK: f64 = 6.0;
/// Fill alpha of a shortcut legend's key chip, applied over the footer surface. Deliberately
/// lighter than `field_bg`: the chip exists to bind a key symbol to its label, not to look
/// pressable. Paired with a zero border width, because the border is what reads as a control.
/// Vertical slack a single-line footer label's frame adds on top of the font's line height. A
/// frame of exactly the line height is one rounding step short of what the cell needs, and a
/// single-line cell that is even fractionally short renders the tail truncation, so "Tab" drew
/// as "T..." even though its width was never in doubt.
const FOOTER_LABEL_HEIGHT_SLACK: f64 = 2.0;
/// Cell padding a centered single-line `NSTextField` adds on top of the text it holds. A keycap
/// is sized from its text with this allowance: measuring only the text under-reserves slightly,
/// and a cell that is a fraction short renders the tail truncation ("Tab" -> "T…") even though
/// the text itself would have fit. Measured: "Tab" needs 28.37pt of cell for 20.37pt of text.
const KBD_CELL_PADDING: f64 = 8.0;
/// Width reserved for the footer's entry-count label. Bounded, not arbitrary: the count is at
/// most three digits (`clipboard.max_entries` is validated to 1..=100) and every locale's
/// template is "{count} 个项目" / "{count} items", whose widest form measures 59.9pt at
/// FONT_CAPTION. It is a ceiling, not a measurement: the shortcut legends are laid out
/// right-to-left and must clear this label's right edge, so an oversized reservation silently
/// pushes the whole legend row over the very text it is naming.
const FOOTER_COUNT_W: f64 = 72.0;
/// The list's top offset inside the document.
const CLEAR_BTN_GAP: f64 = 4.0;
/// Fixed geometry for the clear-confirmation card; two text actions share one horizontal baseline.
/// The gap between the two header clear actions (they sit right of the filter pills).
const CLEAR_ACTION_GAP: f64 = 8.0;
/// The row highlight's radius: the mock's `.clip-row` 14px (not the shared control radius).
const SEL_TILE_R: f64 = 14.0;
/// inset from the selected row's top and bottom edges.
/// Resolve the shared settings/overlay palette for clipboard surfaces and controls.
fn clipboard_palette() -> crate::theme::UiPalette {
    crate::theme::ui_palette()
}

// ---------------------------------------------------------------------------------------------
// Clipboard-only design tokens (the v2 mock, `clipboard-v2.html`)
// ---------------------------------------------------------------------------------------------
//
// The shared token set lives in `theme.rs`, which this subsystem does not own. These are the
// clipboard panel's own values from the v2 mock, kept here as the adapter until they are promoted
// into `UiPalette` (the promotion list is reported with this change). They are applied in light
// mode only: the mock is light-only, and the dark palette's values are the ones measured against
// the dark material.

/// The clipboard panels' outer corner radius: the v2 mock's 32pt sheet curvature.
///
const PICKER_PANEL_RADIUS: f64 = crate::glass::CLIPBOARD_PANEL_RADIUS;
/// The selected row's wash: `rgba(92, 82, 72, 0.16)`.
const PICKER_ROW_SELECTED_LIGHT: u32 = 0x5C52_4829;
/// Hovered (not selected) row wash: `rgba(92, 82, 72, 0.10)`.
///
/// A downward wash, not the mock's upward one. The mock's `rgba(255,255,255,.32)` laid white over
/// an already-pale material: it measured 1.05-1.06:1 on the light panel of all three materials and
/// 1.00:1 over a white desktop, so hovering a row changed nothing the eye could see. Dark mode never
/// had the problem because its wash already darkens. This keeps the mock's warm ink hue -- the same
/// hue as the selected wash -- and only lowers the alpha, so hover reads at ~1.16:1 on every
/// material while staying a clear step below selection (~1.26:1).
const PICKER_ROW_HOVER_LIGHT: u32 = 0x5C52_481A;
/// Hairline separators (footer top line and friends): `rgba(52, 42, 30, 0.09)`.
const PICKER_HAIRLINE_LIGHT: u32 = 0x342A_1E17;
/// Search field fill: `rgba(255, 255, 255, 0.38)`, focused `rgba(255, 255, 255, 0.58)`.
const PICKER_FIELD_BG_LIGHT: u32 = 0xFFFF_FF26;
const PICKER_FIELD_BG_FOCUS_LIGHT: u32 = 0xFFFF_FF38;
/// The glass edge / field border: `rgba(255, 255, 255, 0.5)`, focused `rgba(255, 255, 255, 0.86)`.
const PICKER_GLASS_LINE_LIGHT: u32 = 0xFFFF_FF80;
/// The mock's emphasis colour `#2f6fed`.
const PICKER_ACCENT_LIGHT: u32 = 0x2F6F_EDFF;
/// The image row's thumbnail well: `rgba(255, 255, 255, 0.32)` fill.
const PICKER_THUMB_BG_LIGHT: u32 = 0xFFFF_FF52;

/// The selected row's wash. Light mode follows the v2 mock; dark mode keeps the measured palette.
fn picker_row_selected(palette: crate::theme::UiPalette) -> u32 {
    if palette.dark {
        palette.selection_bg
    } else {
        PICKER_ROW_SELECTED_LIGHT
    }
}

/// The hovered row's wash, by the same rule as [`picker_row_selected`].
fn picker_row_hover(palette: crate::theme::UiPalette) -> u32 {
    if palette.dark {
        palette.hover_bg
    } else {
        PICKER_ROW_HOVER_LIGHT
    }
}

/// Hairline separator colour for the clipboard surfaces.
fn picker_hairline() -> u32 {
    if clipboard_palette().dark {
        clipboard_palette().separator
    } else {
        PICKER_HAIRLINE_LIGHT
    }
}

/// Search field fill, by focus state (the mock strengthens the fill on focus).
fn picker_field_fill(focused: bool) -> u32 {
    let palette = clipboard_palette();
    if palette.dark {
        palette.field_bg
    } else if focused {
        PICKER_FIELD_BG_FOCUS_LIGHT
    } else {
        PICKER_FIELD_BG_LIGHT
    }
}

/// Search field border, by focus state.
///
/// Focus stays **accent**, not the mock's strengthened glass line: the native focus ring is switched off
/// for this field (`detail.rs`, so the rounded shape survives), which leaves this stroke as the only
/// keyboard-focus indication. A near-white line on a light glass panel does not read as focus, and
/// `docs/design-style` §10 makes an accent ring the focus contract. The glass line is kept for the
/// resting state so the field still reads as part of the sheet.
fn picker_field_ring(focused: bool) -> u32 {
    let palette = clipboard_palette();
    if palette.dark {
        if focused {
            palette.accent
        } else {
            palette.card_border
        }
    } else if focused {
        picker_accent(palette)
    } else {
        PICKER_GLASS_LINE_LIGHT
    }
}

/// The image row's thumbnail well fill: the mock's `rgba(255,255,255,0.32)` in light mode.
fn picker_thumb_fill() -> u32 {
    let palette = clipboard_palette();
    if palette.dark {
        palette.field_bg
    } else {
        PICKER_THUMB_BG_LIGHT
    }
}

/// The image row's thumbnail ring: the mock's `rgba(255,255,255,0.5)` glass line in light mode.
fn picker_thumb_ring() -> u32 {
    let palette = clipboard_palette();
    if palette.dark {
        palette.card_border
    } else {
        PICKER_GLASS_LINE_LIGHT
    }
}

/// The clipboard surfaces' emphasis colour (the selected row's bar, the field's focus ring).
fn picker_accent(palette: crate::theme::UiPalette) -> u32 {
    if palette.dark {
        palette.accent
    } else {
        PICKER_ACCENT_LIGHT
    }
}

/// The shortcut legend's chip fill as a CGColor directly usable by a layer: a wash at
/// `LEGEND_CHIP_ALPHA` in the direction that actually separates it from the footer surface --
/// darkening a light surface, lightening a dark one -- rather than a fixed gray that would be
/// invisible in one of the two modes.
fn legend_chip_background() -> *mut c_void {
    let base: u32 = if clipboard_palette().dark {
        0xFFFF_FFFF
    } else {
        0x0000_0000
    };
    crate::ffi::hex_to_cg_color((base & 0xFFFF_FF00) | crate::theme::LEGEND_CHIP_ALPHA)
}
/// visible custom scroll indicator width.
const SCROLL_INDICATOR_W: f64 = 6.0;
/// Actual mouse hit width; transparent side padding enlarges the drag area without changing
/// the visible capsule width.
const SCROLL_INDICATOR_HIT_W: f64 = 10.0;
/// empty inset at each scrollbar track edge.
const SCROLL_INDICATOR_EDGE: f64 = 4.0;
/// The detail panel's scroll area insets: the v2 mock's `.detail-pane` padding
/// `10px 22px 16px` plus `.detail-scroll` padding-right 4px, which leaves a 22pt content
/// column from each panel edge.
const DETAIL_INSET_TOP: f64 = 10.0;
const DETAIL_INSET_LEFT: f64 = 22.0;
const DETAIL_INSET_RIGHT: f64 = 26.0;
const DETAIL_INSET_BOTTOM: f64 = 16.0;
/// `.meta-v` on the mock: 12px with `line-height: 1.45`.
const DETAIL_META_LINE_HEIGHT_RATIO: f64 = 1.45;
/// The detail view always reserves a lower-right safe corner, even when only one scrollbar exists.
const SCROLL_INDICATOR_CORNER_GAP: f64 = 2.0;
/// Lower-right safe-corner reserve = the other scrollbar's hit width plus the visual gap.
const SCROLL_INDICATOR_CORNER_RESERVE: f64 = SCROLL_INDICATOR_HIT_W + SCROLL_INDICATOR_CORNER_GAP;
/// minimum indicator length (too short is unreadable).
const SCROLL_INDICATOR_MIN_LEN: f64 = 24.0;
/// gap between the picker and the detail panel.
#[cfg(test)]
const DETAIL_GAP: f64 = 8.0;
/// Inner width of a meta row's value column: the meta table is clipped to this, so the panel's
/// custom scroll capsule (a 10pt track over the scroll view's trailing 3pt) never covers a value.
const DETAIL_META_CLIP_INSET: f64 = SCROLL_INDICATOR_HIT_W + 3.0;
/// The detail pane's floating actions: a 28pt hit target (the shared minimum), one gap apart
/// and trailing inside the pane's right inset. Save-as is the outermost; the code soft-wrap
/// switch, when the entry is code, sits left of it.
const DETAIL_ACTION_H: f64 = 28.0;
const DETAIL_ACTION_GAP: f64 = 6.0;
/// The wrap switch's chip is wider than the 28pt switch it hosts (the mock's pill button-ish
/// footprint), with the switch inset from the chip's right edge.
const DETAIL_SWITCH_CHIP_W: f64 = 48.0;
const DETAIL_SWITCH_INSET: f64 = 8.0;
/// The soft-wrap caption left of the switch. Wide enough for "Soft Wrap" / "自动换行" at the
/// caption size and narrower than the meta column it sits above, so the row keeps its rhythm.
const DETAIL_WRAP_LABEL_W: f64 = 72.0;
/// The gap between the caption and the switch it names.
const DETAIL_WRAP_LABEL_GAP: f64 = 8.0;

/// The widest picker that still expands the detail INSIDE itself: the mock switches at
/// `@media (max-width: 840px)`, where the picker plus the side pane no longer fit a desktop window.
/// The app's picker is 640pt, so it stands beside the detail today; a narrower panel (a small
/// display, or a future responsive picker) takes the mock's in-pane branch instead.
const DETAIL_IN_PANE_BELOW_W: f64 = 840.0;
/// The widest picker + side pane + gap the app shows side by side. Beyond it the two panels no
/// longer fit a laptop's visible frame, so the side layout is not offered and the in-pane one is
/// used instead.
const DETAIL_SIDE_PANEL_MAX_W: f64 = 960.0;
/// The mock's `.detail-pane` padding on the narrow branch: `6px 18px 14px` (the wide branch is
/// `10px 22px 16px`, which the side pane keeps).
const DETAIL_NARROW_INSET_TOP: f64 = 6.0;
const DETAIL_NARROW_INSET_LEFT: f64 = 18.0;
/// The mock's narrow branch leaves 18px of trailing space plus the scroll area's 4px pad. This pane
/// draws its own scroll capsule inside that band instead of using a native scrollbar, so the column
/// ends before the capsule: 18 + 4 for the pad, and the rest keeps values off the thumb.
const DETAIL_NARROW_INSET_RIGHT: f64 = 26.0;
const DETAIL_NARROW_INSET_BOTTOM: f64 = 14.0;
/// The mock's `.meta-row` grid: a 56px label column, a 10px gap, then the value.
const DETAIL_META_LABEL_W: f64 = 56.0;
const DETAIL_META_GAP: f64 = 10.0;
/// The mock's `.meta-table` `padding-top: 28px` above the first row; the row gap is tighter
/// than the mock's 11px because two label+value pairs now share each row.
const DETAIL_META_PADDING_TOP: f64 = 28.0;
const DETAIL_META_ROW_GAP: f64 = 6.0;
/// The meta table packs two label+value pairs per row: the right pair's column starts at this
/// fraction of the content width, with this much room kept after the left value.
const DETAIL_META_RIGHT_SPLIT: f64 = 0.45;
const DETAIL_META_COLUMN_GAP: f64 = 16.0;
/// Below this content width the table falls back to one field per row: two label+value columns
/// cannot both stay legible, and a zero-width value is worse than a taller table.
const DETAIL_META_TWO_COL_MIN: f64 = 280.0;
/// The source app icon in the first meta value (the mock's `.meta-v img`, 13px + 6px gap).
const DETAIL_META_ICON: f64 = 13.0;
const DETAIL_META_ICON_GAP: f64 = 6.0;
/// The mock's `.meta-v` `line-height: 1.45` at 12px.
const DETAIL_META_LINE_H: f64 =
    crate::theme::line_height(crate::theme::FONT_CAPTION, DETAIL_META_LINE_HEIGHT_RATIO);
// The mock's `.meta-table` margin-top: auto: the table is the last child of the scrolling
// column, so it rests on the pane's bottom edge when the body is shorter than the pane. The table
// lives INSIDE that scroll area -- pinned above it, it would cover the long text it describes.
/// Fixed outer width shared by text, code, and image details to prevent horizontal jumps.
const DETAIL_MAX_W: f64 = 640.0;
/// Code details use the same fixed width; with soft wrap off, native horizontal scrolling shows
/// long lines.
#[cfg(test)]
const DETAIL_CODE_MAX_W: f64 = DETAIL_MAX_W;
/// Safety columns reserved for scrollers and the font's actual advance width.
const DETAIL_CODE_WRAP_SAFETY: usize = 4;
/// The mock's `.meta-table` column width at its narrow breakpoint (50px + 10px gap).
const DETAIL_META_LABEL_W_COMPACT: f64 = 50.0;
/// The meta table switches to the compact label column when the body is narrower than this:
/// the mock's single-pane layout, where the value column would otherwise be squeezed.
const DETAIL_META_COMPACT_BELOW: f64 = 420.0;
/// vertical safety margin for the detail panel.
const DETAIL_SCREEN_MARGIN: f64 = 8.0;
// The detail panel's minimum body height: decoupled from ROW_H (the list row is the mock's
// 40px; the detail pane's floor is the old 78pt reading surface), but it may not shrink below
// the meta table's own required height (see `detail_pane_metrics`).
const DETAIL_TEXT_MIN_H: f64 = 78.0;
/// Detail preview max edge (px): ~89% native density on a retina 640pt panel; generated
/// once on the first detail open and cached as `{hash}.detail`, never held in RAM (RAM
/// still keeps only the 480px thumbnail).
const DETAIL_PREVIEW_MAX_DIM: f64 = 1280.0;
/// Open/close duration for the detail panel; scoped to the clipboard detail window.
/// Initial horizontal content offset while the panel expands from left to right.
const DETAIL_CONTENT_ANIMATION_OFFSET: f64 = 8.0;
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
/// Set when the storage has just become writable again after being unavailable or blocked: the
/// next poll must adopt a baseline instead of recording the copy made while nothing could be kept.
static RECOVERY_BASELINE_PENDING: AtomicBool = AtomicBool::new(false);

/// Record that a storage recovery needs a fresh baseline before recording resumes.
pub(super) fn request_recovery_baseline() {
    RECOVERY_BASELINE_PENDING.store(true, Ordering::SeqCst);
}

/// Whether a storage recovery still owes the session a baseline. The request is cleared by a
/// SUCCESSFUL `establish_baseline`, never by merely reading it: if the pasteboard is unreadable at
/// the moment the poll notices, recording must stay off until a baseline can actually be taken.
fn recovery_baseline_pending() -> bool {
    RECOVERY_BASELINE_PENDING.load(Ordering::SeqCst)
}

/// The poll's recovery step. Returns true when this tick must not record: a pending request either
/// adopts its baseline (and drops this tick's content, which predates the recovery) or keeps
/// collection off because the pasteboard is not readable yet.
pub(super) fn handle_recovery_baseline() -> bool {
    if !recovery_baseline_pending() {
        return false;
    }
    if establish_baseline() {
        log_debug!("[clip] storage recovered: baseline adopted, current pasteboard not recorded");
    } else {
        log_debug!(
            "[clip] storage recovered but the pasteboard is unreadable: recording stays off"
        );
    }
    true
}

/// Serializes every test that drives the process-wide collection gate or the baseline static.
#[cfg(test)]
pub(super) fn gate_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
pub(super) fn last_change_count_for_test() -> i64 {
    *LAST_CHANGE_COUNT.lock().unwrap()
}

#[cfg(test)]
pub(super) fn take_recovery_baseline_for_test() -> bool {
    RECOVERY_BASELINE_PENDING.swap(false, Ordering::SeqCst)
}

#[cfg(test)]
pub(super) fn recovery_baseline_pending_for_test() -> bool {
    recovery_baseline_pending()
}

#[cfg(test)]
pub(super) fn request_recovery_baseline_for_test() {
    request_recovery_baseline();
}

/// The runtime collection gate. The persisted `config.clipboard.paused` is the user's intent; this
/// is the effective state the poll consults, and the two can differ for as long as a resume has not
/// managed to establish its baseline.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CollectionGate {
    /// Recording normally.
    Active = 0,
    /// Not recording. The poll keeps advancing the changeCount baseline, so a later resume cannot
    /// backfill what was copied while paused.
    Paused = 1,
    /// Resuming: the gate stays closed until the baseline has been adopted, so a poll that runs in
    /// between (or a synchronous pasteboard notification) cannot record the current content.
    Baselining = 2,
}

static COLLECTION_GATE: AtomicU8 = AtomicU8::new(CollectionGate::Active as u8);

fn collection_gate() -> CollectionGate {
    match COLLECTION_GATE.load(Ordering::SeqCst) {
        1 => CollectionGate::Paused,
        2 => CollectionGate::Baselining,
        _ => CollectionGate::Active,
    }
}

/// Whether recording is currently off (paused or still establishing a resume baseline).
pub(super) fn collection_paused() -> bool {
    collection_gate() != CollectionGate::Active
}

#[cfg(test)]
thread_local! {
    /// Test override for the pasteboard's changeCount: the gate's contract is about the baseline, and
    /// the real pasteboard cannot be read in a headless test.
    static TEST_CHANGE_COUNT: std::cell::Cell<Option<i64>> = const { std::cell::Cell::new(None) };
    /// The pasteboard is unreadable (the real read would otherwise succeed on the test machine).
    static TEST_PASTEBOARD_UNAVAILABLE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(super) fn set_test_change_count(value: Option<i64>) {
    TEST_CHANGE_COUNT.with(|cell| cell.set(value));
}

#[cfg(test)]
pub(super) fn set_test_pasteboard_unavailable(unavailable: bool) {
    TEST_PASTEBOARD_UNAVAILABLE.with(|cell| cell.set(unavailable));
}

/// The pasteboard's current changeCount (None when the pasteboard is unavailable).
fn current_change_count() -> Option<i64> {
    #[cfg(test)]
    {
        if TEST_PASTEBOARD_UNAVAILABLE.with(|cell| cell.get()) {
            return None;
        }
        if let Some(value) = TEST_CHANGE_COUNT.with(|cell| cell.get()) {
            return Some(value);
        }
    }
    unsafe {
        let pb: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
        if pb.is_null() {
            return None;
        }
        Some(msg_send![pb, changeCount])
    }
}

/// Adopt the current changeCount as "already seen" WITHOUT reading the pasteboard's contents.
fn establish_baseline() -> bool {
    let Some(cc) = current_change_count() else {
        return false;
    };
    *LAST_CHANGE_COUNT.lock().unwrap() = cc;
    disarm_paste_delete_suppression();
    // A successful baseline satisfies a pending storage-recovery request: whether the request came
    // from a resume or from the recovery path itself, the copy made while nothing could be recorded
    // is now "already seen". Clearing it here is what keeps a stale request from swallowing the
    // first NORMAL copy after collection resumes.
    RECOVERY_BASELINE_PENDING.store(false, Ordering::SeqCst);
    true
}

/// Advance the baseline without recording: what a paused poll does. Returns whether the
/// pasteboard actually changed (false also when it could not be read), so the caller logs once per
/// real change instead of once per timer tick.
pub(super) fn advance_baseline() -> bool {
    let Some(cc) = current_change_count() else {
        return false;
    };
    let mut last = LAST_CHANGE_COUNT.lock().unwrap();
    let changed = *last != cc;
    *last = cc;
    changed
}

/// Pause or resume recording. Returns the **effective** paused state: `true` means recording stays
/// off, which also happens when a resume cannot read the pasteboard -- the caller must then keep the
/// persisted intent and every surface (config, menu, settings switch) in the paused state.
pub(crate) fn set_collection_paused(paused: bool) -> bool {
    if paused {
        COLLECTION_GATE.store(CollectionGate::Paused as u8, Ordering::SeqCst);
        disarm_paste_delete_suppression();
        return true;
    }
    COLLECTION_GATE.store(CollectionGate::Baselining as u8, Ordering::SeqCst);
    if establish_baseline() {
        COLLECTION_GATE.store(CollectionGate::Active as u8, Ordering::SeqCst);
        false
    } else {
        COLLECTION_GATE.store(CollectionGate::Paused as u8, Ordering::SeqCst);
        true
    }
}

/// Seed the gate from the persisted intent before the first poll of this process. An unpaused start
/// goes straight to Active so `start()` can record the current pasteboard (the existing "the first
/// summon is not empty" behaviour); a paused start adopts a baseline and records nothing.
pub(super) fn init_collection_gate() {
    let paused = CONFIG.read().map(|c| c.clipboard.paused).unwrap_or(false);
    if paused {
        COLLECTION_GATE.store(CollectionGate::Paused as u8, Ordering::SeqCst);
        let _ = establish_baseline();
    } else {
        COLLECTION_GATE.store(CollectionGate::Active as u8, Ordering::SeqCst);
    }
}

/// Mirror the recording state in the panel header's orb: the glyph swaps between pause and a
/// filled play (the `set_pin_button_style` precedent for a stateful glyph), and the tooltip and
/// accessibility label name the action the click performs.
pub(super) fn refresh_pause_button() {
    let paused = collection_paused() || CONFIG.read().map(|c| c.clipboard.paused).unwrap_or(false);
    let Some(button) = (*PICKER_PAUSE_BUTTON.lock().unwrap()).map(|pointer| pointer.0) else {
        return;
    };
    unsafe {
        let symbol = make_nsstring(if paused {
            "play.circle.fill"
        } else {
            "pause.circle"
        });
        let image: *mut AnyObject = msg_send![
            class!(NSImage),
            imageWithSystemSymbolName: symbol,
            accessibilityDescription: std::ptr::null::<AnyObject>()
        ];
        CFRelease(symbol as *const c_void);
        if !image.is_null() {
            let config: *mut AnyObject = msg_send![
                class!(NSImageSymbolConfiguration),
                configurationWithPointSize: PICKER_GEAR_GLYPH_SIZE,
                weight: crate::theme::FONT_WEIGHT_REGULAR
            ];
            let image: *mut AnyObject = msg_send![image, imageWithSymbolConfiguration: config];
            let _: () = msg_send![image, setTemplate: true];
            let _: () = msg_send![button, setImage: image];
        }
        let label = make_nsstring(&t(if paused {
            "menu.resume_clipboard"
        } else {
            "menu.pause_clipboard"
        }));
        let _: () = msg_send![button, setAccessibilityLabel: label];
        let _: () = msg_send![button, setToolTip: label];
        CFRelease(label as *const c_void);
        // Re-apply the orb surface: the accent border is what makes "paused" visible without reading
        // the glyph, and the hover dispatch only runs on pointer movement. The hover state is re-read
        // rather than assumed false: a click lands while the pointer is still over the orb, so a
        // hard-coded `false` would drop the wash until the pointer left and came back.
        set_settings_button_style(button, pause_orb_hovered());
    }
}

/// Flip the persisted pause intent through the one apply pass every surface uses (menu, settings
/// switch, panel orb), so the gate, the menu title, the settings switch and the panel glyph cannot
/// disagree.
pub(crate) fn toggle_paused_intent() {
    let old_cfg = match CONFIG.read() {
        Ok(cfg) => cfg.clone(),
        Err(_) => return,
    };
    let mut new_cfg = old_cfg.clone();
    new_cfg.clipboard.paused = !old_cfg.clipboard.paused;
    if let Ok(mut current) = CONFIG.write() {
        *current = new_cfg.clone();
    }
    crate::config::persist_config_now();
    crate::runtime_config::apply_config_change(
        &old_cfg,
        &new_cfg,
        crate::runtime_config::ConfigChangeSource::Menu,
    );
    refresh_pause_button();
    crate::menu::refresh_clipboard_pause_item();
    crate::settings::refresh_clipboard_pause_switch_from_config();
    crate::log_info!(
        "Clipboard collection: {}",
        if new_cfg.clipboard.paused {
            "paused"
        } else {
            "resumed"
        }
    );
}

/// The panel orb's action: same entry as the menu item.
pub(super) extern "C" fn toggle_pause_recording(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    callback_guard::void("toggle_pause_recording", || {
        toggle_paused_intent();
    });
}

/// The privacy controls as the running process sees them: the effective pause state and the
/// exclusion list. Read by `--e2e-state` so a scenario can assert them without guessing from UI.
/// The effective collection gate as a word a scenario can assert: `paused`, `baselining` or `active`.
///
/// The ticker gate is the one place where an intent (`config.clipboard.paused`) and the effective
/// state can legitimately differ -- while a resume has not yet adopted its baseline -- and that
/// difference is invisible from the outside: both states refuse to record. A state field is what makes
/// the difference assertable, which is the cheapest form of the promotion rule that fits here.
pub(crate) fn collection_gate_state() -> &'static str {
    match collection_gate() {
        CollectionGate::Active => "active",
        CollectionGate::Paused => "paused",
        CollectionGate::Baselining => "baselining",
    }
}

pub(crate) fn privacy_state_for_e2e() -> (bool, Vec<crate::config::ExcludedApp>) {
    let paused = collection_paused() || CONFIG.read().map(|c| c.clipboard.paused).unwrap_or(false);
    let excluded = CONFIG
        .read()
        .map(|c| c.clipboard.excluded_apps.clone())
        .unwrap_or_default();
    (paused, excluded)
}

/// Whether a copy whose source identifiers are `frontmost` / `declared` must be skipped because
/// the exclusion list names either of them.
///
/// The declared identifier comes from `org.nspasteboard.source`, which the WRITING application
/// reports about itself; it is therefore only ever an ADDITIONAL signal. It never replaces the
/// frontmost check: a writer could claim any identifier, and letting the claim override the
/// frontmost verdict would re-admit a copy from an excluded app.
/// What the exclusion decision needs to know about a copy's source, resolved **once**.
///
/// Why an enum rather than `Option<String>`: `None` currently means both "this app has no bundle
/// identifier" (a real, common state -- a non-bundled command-line tool) and "the lookup failed" (the
/// app quit between the copy and the poll). The first must record; the second must not, because an
/// excluded app whose lookup failed would otherwise slip through the very check that exists to stop it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FrontmostSource {
    /// The PID resolved to a running application.
    Resolved {
        /// Its bundle identifier, when it has one.
        bundle_id: Option<String>,
    },
    /// The lookup could not prove anything about the PID.
    LookupFailed,
}

/// The verdict for one copy, with the reason so the log can say why it was dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExclusionVerdict {
    /// Not excluded: the copy is recorded.
    Record,
    /// An excluded application made the copy; it is dropped before the payload is read.
    Excluded,
    /// The source identity could not be established, so the copy is dropped rather than risk
    /// recording content from an excluded app.
    Blocked,
}

/// Decide whether a copy from `source` may be recorded, given the exclusion `list` and the copier
/// that declared itself through `org.nspasteboard.source`.
///
/// The table (and why each row is the way it is):
///
/// | list empty            | Record   -- nothing to protect, and a failed lookup must not cost the user copies |
/// | resolved, id in list  | Excluded |
/// | resolved, id not in   | Record   |
/// | resolved, no id at all| Record   -- a real state (non-bundled tool), not a failed lookup |
/// | declared id in list   | Excluded -- the declared source can only ADD exclusion, never remove it |
/// | lookup failed         | Blocked  -- cannot prove the source is not excluded |
pub(super) fn exclusion_decision(
    list: &[crate::config::ExcludedApp],
    source: &FrontmostSource,
    declared: Option<&str>,
) -> ExclusionVerdict {
    let named = |id: &str| {
        let id = id.trim();
        !id.is_empty()
            && list
                .iter()
                .any(|entry| entry.bundle_id.trim().eq_ignore_ascii_case(id))
    };
    // The declared id is checked first: it is an ADDITIONAL signal, and an excluded app that
    // declared itself is excluded even when the frontmost lookup also failed.
    if declared.is_some_and(named) {
        return ExclusionVerdict::Excluded;
    }
    if list.is_empty() {
        return ExclusionVerdict::Record;
    }
    match source {
        FrontmostSource::Resolved { bundle_id } => match bundle_id {
            Some(id) if named(id) => ExclusionVerdict::Excluded,
            // Either a different app, or an app with no bundle identifier at all.
            _ => ExclusionVerdict::Record,
        },
        FrontmostSource::LookupFailed => ExclusionVerdict::Blocked,
    }
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
    /// Whether the storage-failure notice is part of this render: the state can change while the
    /// panel is open, and the notice changes both the rows' top offset and the empty-state hint.
    storage_unavailable: bool,
}
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct ContentAttributedKey {
    content: String,
    kind: u8,
    primary_text: u32,
    secondary_text: u32,
    link_text: u32,
    /// The material the ink was resolved for (see `clipboard::text_style::material_cache_key`): the palette
    /// tokens are the same across materials while the colour object they map to is not, so without this a
    /// material switch keeps serving the previous material's ink.
    material: u8,
}
static CONTENT_ATTRIBUTED_CACHE: LazyLock<
    MainThreadSlot<HashMap<ContentAttributedKey, CachedUiObject>>,
> = LazyLock::new(|| unsafe { MainThreadSlot::new(HashMap::new()) });
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
        storage_unavailable: unavailable_copy().is_some(),
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
/// Incremental visual views for materialized rows (group header + its clear button, tile,
/// content + the action buttons); ROW_VIEW_INDICES maps them back to the full display list.
#[derive(Clone, Copy)]
struct RowHoverViews {
    group_label: Option<ObjPtr>,
    /// The group header's "clear" button -- a per-row view like the label it sits beside, so it is
    /// torn down and re-created with the row instead of outliving the day group it belongs to.
    group_clear: Option<ObjPtr>,
    tile: ObjPtr,
    content: ObjPtr,
    detail: ObjPtr,
    pin: ObjPtr,
    del: ObjPtr,
}

impl RowHoverViews {
    /// Every view this row owns. Destructured exhaustively on purpose: a new field stops compiling
    /// here until it is added to the list, so a view cannot be created without also being torn down.
    fn owned(self) -> [Option<ObjPtr>; 7] {
        let RowHoverViews {
            group_label,
            group_clear,
            tile,
            content,
            detail,
            pin,
            del,
        } = self;
        [
            group_label,
            group_clear,
            Some(tile),
            Some(content),
            Some(detail),
            Some(pin),
            Some(del),
        ]
    }
}
static ROW_HOVER_VIEWS: MainThreadSlot<Vec<RowHoverViews>> =
    unsafe { MainThreadSlot::new(Vec::new()) };
/// Display indices for materialized row views; rows outside the viewport have no AppKit views.
static ROW_VIEW_INDICES: MainThreadSlot<Vec<usize>> = unsafe { MainThreadSlot::new(Vec::new()) };
/// the picker window.
static PICKER_WINDOW: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
/// The picker window's dynamic class. Recorded at registration so an override's super dispatch can
/// name the class that defined it, rather than recovering it from the instance.
static PICKER_WINDOW_CLASS: std::sync::OnceLock<crate::ffi::StaticClass> =
    std::sync::OnceLock::new();
/// The search field's dynamic class, recorded the same way as the picker window's.
static SEARCH_FIELD_CLASS: std::sync::OnceLock<crate::ffi::StaticClass> =
    std::sync::OnceLock::new();

pub(crate) fn picker_window_class() -> &'static crate::ffi::StaticClass {
    PICKER_WINDOW_CLASS
        .get()
        .expect("the picker window class is registered before its override can run")
}

pub(crate) fn search_field_class() -> &'static crate::ffi::StaticClass {
    SEARCH_FIELD_CLASS
        .get()
        .expect("the search field class is registered before its override can run")
}
/// the picker container (receives key events).
static PICKER_CONTAINER: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
/// Copy the container pointer and end the slot borrow immediately; never hold a
/// MainThreadSlot RefMut across an AppKit call.
fn picker_container_ptr() -> Option<*mut AnyObject> {
    PICKER_CONTAINER
        .lock()
        .unwrap()
        .map(|container| container.0)
}
/// the localized footer in place.
static PICKER_CONTENT_PARENT: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
/// The picker's installed backdrop, held for live material/tint updates and material swaps.
static PICKER_BACKDROP: MainThreadSlot<Option<crate::glass::InstalledBackdrop>> =
    unsafe { MainThreadSlot::new(None) };
/// The empty-state hint view (shown when the history is empty / nothing matches). Tracked
/// separately from row views so the next rebuild can remove it.
static EMPTY_STATE_ICON: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
static EMPTY_STATE_VIEW: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
/// The empty state's "grant keychain access" button (only while the storage is unavailable).
static EMPTY_STATE_ACTION: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct RowImageKey {
    image_hash: u64,
    field_bg: u32,
    card_border: u32,
    /// The configured image-row height the cached bitmap was drawn at (the well's height changes
    /// with `clipboard.image_rows`, so a cached 30pt bitmap must not serve a 110pt row).
    thumb_h_bits: u64,
    /// The content column width the bitmap was drawn for (the preview spans it like text).
    thumb_w_bits: u64,
}
static ROW_IMAGE_CACHE: LazyLock<MainThreadSlot<HashMap<RowImageKey, CachedUiObject>>> =
    LazyLock::new(|| unsafe { MainThreadSlot::new(HashMap::new()) });
fn next_ui_cache_recency() -> u64 {
    UI_CACHE_RECENCY.fetch_add(1, Ordering::Relaxed) + 1
}
/// varies with the wrapped line count).
static ROW_PITCHES: LazyLock<Mutex<Vec<f64>>> = LazyLock::new(|| Mutex::new(Vec::new()));
/// The filter pills' button pointers (one per tag; restyled on change/rebuild).
/// Persistent clear-history action buttons, relaid out when the locale changes.
static CLEAR_HISTORY_ACTION_BUTTONS: MainThreadSlot<Option<[ObjPtr; 2]>> =
    unsafe { MainThreadSlot::new(None) };
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
pub(super) fn localized_filter_labels() -> [String; 6] {
    [
        t("clipboard.filter_all"),
        t("clipboard.filter_text"),
        t("clipboard.filter_image"),
        t("clipboard.filter_link"),
        t("clipboard.filter_code"),
        t("clipboard.filter_pinned"),
    ]
}
/// the top search field.
static SEARCH_FIELD: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
/// Rounded search surface drawn behind NSSearchField, whose cell paints over its own layer.
pub(super) static SEARCH_FIELD_SURFACE: MainThreadSlot<Option<ObjPtr>> =
    unsafe { MainThreadSlot::new(None) };
/// The search field clear × hover state.
static SEARCH_CLEAR_HOVERED: AtomicBool = AtomicBool::new(false);
/// The real click button over the hand-drawn ×; rendering remains in the cell.
static SEARCH_CLEAR_BUTTON: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
/// The header's corner close button (persistent: the picker window is reused across summons).
static PICKER_CLOSE_BUTTON: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
/// The fixed header strip (search field, drag handle, settings and close buttons). Kept so the
/// smoke runner can assert its controls against the live view tree, not just against geometry.
static PICKER_HEADER_STRIP: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
/// The header's settings button, next to the close button (same persistence).
static PICKER_OPTIONS_ORB: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
/// The options popover's "open settings" row. `PICKER_OPTIONS_ORB` only opens the
/// popover, so a scenario that means "open settings" has to click this row instead.
pub(super) static PICKER_SETTINGS_ROW: MainThreadSlot<Option<ObjPtr>> =
    unsafe { MainThreadSlot::new(None) };
static PICKER_PIN_BUTTON: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
/// The panel header's pause-recording orb (a real control, unlike the pin's check state).
static PICKER_PAUSE_BUTTON: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
/// The centered "magnifier + search hint" attributed string (hand-drawn; the field itself
/// carries NO placeholder property, so the field editor never draws the placeholder
/// left-aligned on a focused-but-empty field).
static SEARCH_HINT_TEXT: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
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
    let icon_font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_SIDEBAR_TITLE];
    let icon_color = crate::glass::panel_ink(
        clipboard_palette().muted_text,
        crate::glass::PanelInk::Muted,
    );
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
    let font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CONTROL];
    let _: () = msg_send![ph_text_attrs, setObject: font, forKey: font_key];
    CFRelease(font_key as *const c_void);
    let color_key = make_nsstring("NSColor");
    // Placeholder text follows the same muted role used by settings labels.
    let ph_color = crate::glass::panel_ink(
        clipboard_palette().muted_text,
        crate::glass::PanelInk::Muted,
    );
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
static SCROLL_VIEW: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
/// the custom scroll indicator view.
static SCROLL_INDICATOR: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
/// The detail text scroll view and its custom indicator; image details have no scroll area.
static DETAIL_SCROLL_VIEW: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
static DETAIL_SCROLL_INDICATOR: MainThreadSlot<Option<ObjPtr>> =
    unsafe { MainThreadSlot::new(None) };
static DETAIL_HORIZONTAL_SCROLL_INDICATOR: MainThreadSlot<Option<ObjPtr>> =
    unsafe { MainThreadSlot::new(None) };
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
static DETAIL_WINDOW: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
/// What the picker's body shows: the list, or the detail that took its place. The narrow panel
/// expands INSIDE itself (the mock's `@media (max-width: 840px)` branch hides `.list-pane` and
/// gives `.detail-pane` the whole panel), so the two are alternate contents of one rect and only
/// one of them is ever installed.
static PICKER_PANE_MODE: MainThreadSlot<PickerPaneMode> =
    unsafe { MainThreadSlot::new(PickerPaneMode::List) };

/// See `PICKER_PANE_MODE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PickerPaneMode {
    List,
    Detail,
}
/// The detail panel's installed backdrop (glass view + inactive compensation layer inside).
static DETAIL_BACKDROP: MainThreadSlot<Option<crate::glass::InstalledBackdrop>> =
    unsafe { MainThreadSlot::new(None) };
/// Keep clipboard panels on the same resolved appearance as the settings window and switcher.
unsafe fn apply_panel_appearance(window: *mut AnyObject) {
    // Vibrant, not plain: these panels draw their text on the user's material (see
    // `glass::vibrant_appearance_name`).
    let name = make_nsstring(crate::glass::vibrant_appearance_name(resolved_is_dark()));
    let appearance: *mut AnyObject = msg_send![class!(NSAppearance), appearanceNamed: name];
    CFRelease(name as *const c_void);
    if !appearance.is_null() {
        let _: () = msg_send![window, setAppearance: appearance];
    }
}
/// Hide every text field inside the picker (`--clipboard-blank-text=after:N`, development only).
///
/// Hiding is what makes the A2 contrast scenario's two frames comparable: every frame in the picker is set
/// explicitly, so hiding a view changes what is drawn and not where anything is, and a capture taken before
/// this runs is the "with text" frame of the same panel, in the same launch, over the same material.
pub(crate) fn dev_hide_picker_text() {
    unsafe {
        let Some(window) = (*PICKER_WINDOW.lock().unwrap()).map(|ptr| ptr.0) else {
            log_info!("[clip] blank-text: the picker has no window yet");
            return;
        };
        let content: *mut AnyObject = msg_send![window, contentView];
        if content.is_null() {
            return;
        }
        hide_text_fields(content);
        let _: () = msg_send![content, setNeedsDisplay: true];
        log_info!("[clip] blank-text: picker text hidden for the differential capture");
    }
}

/// Recursive helper for `dev_hide_picker_text`.
unsafe fn hide_text_fields(view: *mut AnyObject) {
    let is_text_field: bool = msg_send![view, isKindOfClass: class!(NSTextField)];
    if is_text_field {
        let _: () = msg_send![view, setHidden: true];
    }
    // The filter pills are buttons whose label *is* the title (`make_filter_pill`), so hiding text fields alone
    // left the filter row's own text in place -- the differential capture then measured something else in that
    // region. Clearing the title removes the glyphs while the button stays visible, so its frame and its fill are
    // exactly what they were.
    let is_button: bool = msg_send![view, isKindOfClass: class!(NSButton)];
    if is_button {
        let empty: *mut AnyObject = msg_send![class!(NSString), string];
        let _: () = msg_send![view, setTitle: empty];
    }
    let subviews: *mut AnyObject = msg_send![view, subviews];
    if subviews.is_null() {
        return;
    }
    let count: usize = msg_send![subviews, count];
    for index in 0..count {
        let child: *mut AnyObject = msg_send![subviews, objectAtIndex: index];
        if !child.is_null() {
            hide_text_fields(child);
        }
    }
}

/// The picker's frame in screen coordinates, top-left based, or `None` when it has no window yet.
/// A2 and the screenshots need to know where the panel actually is instead of guessing from the cursor;
/// the frame is also the only way to tell "the panel is not on screen" from "the capture missed it".
pub(crate) fn picker_frame_top_left() -> Option<(f64, f64, f64, f64)> {
    unsafe {
        let window = (*PICKER_WINDOW.lock().unwrap())?;
        // The *panel* rect, which is what the A2 scenarios measure in: the window is padded for the
        // elevation shadow, so reporting its frame would shift every region a scenario samples by the
        // padding.
        let frame = crate::glass::panel_frame_of(window.0);
        // The primary display, *not* `NSScreen.mainScreen`: that follows the key window, and the picker
        // is usually the key window, so a picker on a secondary display would be flipped against that
        // display's height and the reported y would be wrong (the same trap `overlay/hover.rs` and
        // `overlay/cards.rs` document).
        let screen_frame = crate::overlay::hover::primary_screen_frame()?;
        Some((
            frame.origin.x,
            screen_frame.size.height - (frame.origin.y + frame.size.height),
            frame.size.width,
            frame.size.height,
        ))
    }
}

/// The picker window's frame in screen coordinates, top-left based, or `None` when it has no window yet.
/// The window is the padded rect, so this is what a scenario needs to find the *outer* edge of the shadow
/// (the outline and interior measurements use `picker_frame_top_left`, the panel).
pub(crate) fn picker_window_frame_top_left() -> Option<(f64, f64, f64, f64)> {
    unsafe {
        let window = (*PICKER_WINDOW.lock().unwrap())?;
        let frame: NSRect = msg_send![window.0, frame];
        let screen_frame = crate::overlay::hover::primary_screen_frame()?;
        Some((
            frame.origin.x,
            screen_frame.size.height - (frame.origin.y + frame.size.height),
            frame.size.width,
            frame.size.height,
        ))
    }
}

/// Each footer caption's own frame in the picker's own points, top-left origin (empty until a footer exists).
/// The A2 contrast scenario measures each caption separately: one region per role, or the number cannot say
/// which role it belongs to.
pub(crate) fn picker_footer_hint_frames() -> Vec<(f64, f64, f64, f64)> {
    text_style::footer_hint_frames()
}

/// The filters row's band in the picker's own points, top-left origin (None until the row is built).
///
/// The A2 contrast scenario used to frame this row from a constant, and the constant fell out of step
/// with the layout (the row sits below the search field, which moved): the capture then covered the gap
/// between two rows and reported "a band changed, that is not text" for every material. Reporting the
/// reports the options orb's rect (the popover is closed or not built); once the popover exists
/// it reports the union of its row rects instead, so the A2 contrast capture follows the real
/// filter UI either way.
pub(crate) fn picker_filter_row_rect_top_left() -> Option<(f64, f64, f64, f64)> {
    if filter_popover_open() {
        if let Some(rect) = filter_popover_rows_union() {
            return Some(rect);
        }
    }
    // The filters exist only inside the popover: with it closed there is no filter row on screen, and
    // this used to answer with the OPTIONS ORB's 34x34 rect instead. A scenario then measured the orb
    // for glyph ink and reported "no text" for every material -- which reads as a contrast failure and
    // is really the absence of the thing it meant to measure. Answer None so the caller can say so.
    None
}

/// Whether the options popover is currently open (`--e2e-state`'s `filter_popover_open`).
pub(crate) fn filter_popover_open() -> bool {
    FILTER_POPOVER_OPEN.load(Ordering::SeqCst)
}
static FILTER_POPOVER_OPEN: AtomicBool = AtomicBool::new(false);
/// The open popover's FILTER-row union in the picker's own points, top-left origin (`.filter-row`
/// cells only -- the contrast scenario measures text, not the title/tail). The orb is the fallback
/// while the popover is not built.
fn filter_popover_rows_union() -> Option<(f64, f64, f64, f64)> {
    let popover = (*FILTER_POPOVER.lock().unwrap())?;
    if popover.0.is_null() {
        return None;
    }
    let frame: NSRect = unsafe { msg_send![popover.0, frame] };
    // The popover is positioned in the content parent's own (bottom-up) coordinates; the union is
    // reported top-left like every other rect this module hands to a scenario.
    let parent = (*PICKER_CONTENT_PARENT.lock().unwrap())?;
    let bounds: NSRect = unsafe { msg_send![parent.0, bounds] };
    let parent_flipped: objc2::runtime::Bool = unsafe { msg_send![parent.0, isFlipped] };
    let popover_top = detail::top_offset_of(
        frame.origin.y,
        bounds.size.height,
        frame.size.height,
        parent_flipped.as_bool(),
    );
    let rows_top = popover_top + POPOVER_PAD_V + POPOVER_TITLE_H;
    let rows_h = model::CLIP_FILTER_ROWS.len() as f64
        * detail::filter_popover_row_h(bounds.size.height)
        + (model::CLIP_FILTER_ROWS.len() - 1) as f64;
    let (row_x, row_w) = popover_row_span();
    Some((frame.origin.x + row_x, rows_top, row_w, rows_h))
}
/// The options orb's 5px accent dot (the mock's `.orb-dot`), shown only while the active
/// filter is not "All".
pub(super) static OPTIONS_ORB_DOT: MainThreadSlot<Option<ObjPtr>> =
    unsafe { MainThreadSlot::new(None) };
/// Toggle the options popover's open flag; the popover view is created/destroyed by the
/// detail module on this transition.
pub(crate) fn toggle_filter_popover() {
    let open = !FILTER_POPOVER_OPEN.load(Ordering::SeqCst);
    if open {
        POPOVER_KEY_ROW.store(usize::MAX, Ordering::SeqCst);
    }
    FILTER_POPOVER_OPEN.store(open, Ordering::SeqCst);
    unsafe { detail::sync_filter_popover(open) };
    // The popover carries the row that opens the settings window, so a scenario that means
    // to click it polls `--e2e-state` for the row rect and for this flag: the flag only
    // changes here, and without a frame the poll would keep reading the previous state.
    crate::e2e_state::record("clipboard_popover");
}
/// Record the popover's open state without a transition (panel hide / programmatic dismiss).
pub(crate) fn set_filter_popover_open(open: bool) {
    FILTER_POPOVER_OPEN.store(open, Ordering::SeqCst);
}

/// Apply a reloaded clear-all preference without retaining a popover whose visible row order was
/// built from the previous value.
/// Rebuild the picker's row list when a row-geometry setting (`clipboard.image_rows`) changed.
/// No-op while the picker is closed -- its next open rebuilds anyway.
pub(crate) fn rebuild_rows_for_layout_change() {
    if PICKER_WINDOW.lock().unwrap().is_none() {
        return;
    }
    unsafe {
        detail::rebuild_rows();
    }
}

pub(crate) fn refresh_clear_actions_after_config_change() {
    if filter_popover_open() {
        detail::dismiss_filter_popover();
    }
    text_style::update_clear_action_labels();
}
/// The options popover view itself (built lazily by `toggle_filter_popover`).
pub(super) static FILTER_POPOVER: MainThreadSlot<Option<ObjPtr>> =
    unsafe { MainThreadSlot::new(None) };
/// The keyboard-highlighted row inside the open popover (usize::MAX = pointer/none).
pub(super) static POPOVER_KEY_ROW: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(usize::MAX);

// Menu metrics follow clipboard-v2-1.html's .popover, .filter-row and .clean-row rules.
pub(super) const POPOVER_W: f64 = 236.0;
/// The menu's horizontal inset.
pub(super) const POPOVER_PAD: f64 = 6.0;
pub(super) const POPOVER_PAD_V: f64 = 12.0;
pub(super) const POPOVER_R: f64 = 18.0;
pub(super) const POPOVER_ROW_H: f64 = 32.0;
/// The lowest the menu rows shrink in a short panel, preserving the documented 28pt hit target.
pub(super) const POPOVER_MIN_ROW_H: f64 = 28.0;
pub(super) const POPOVER_ROW_R: f64 = 8.0;
pub(super) const POPOVER_INSET: f64 = 8.0;
pub(super) const POPOVER_GLYPH_W: f64 = 14.0;
pub(super) const POPOVER_COLUMN_GAP: f64 = 9.0;
pub(super) const POPOVER_TEXT_SIZE: f64 = 13.0;
pub(super) const POPOVER_META_SIZE: f64 = 12.0;
pub(super) const POPOVER_TEXT_H: f64 = 18.0;

pub(super) fn popover_muted(palette: crate::theme::UiPalette) -> u32 {
    if palette.dark {
        palette.secondary_text
    } else {
        0x48453FFF
    }
}

pub(super) fn popover_danger(palette: crate::theme::UiPalette) -> u32 {
    if palette.dark {
        palette.destructive
    } else {
        0x882318FF
    }
}

pub(super) fn popover_hover_bg(palette: crate::theme::UiPalette, danger: bool) -> u32 {
    if danger && !palette.dark {
        0xB3372A12
    } else {
        palette.hover_bg
    }
}
/// `.pop-title` 12px semibold with 6px/5px vertical padding.
pub(super) const POPOVER_TITLE_H: f64 = 23.0;
/// `.pop-divider` 1px hairline with 6px vertical margins.
pub(super) const POPOVER_DIVIDER_H: f64 = 13.0;
/// Minimum clearance the menu keeps inside the panel's horizontal edges.
///
/// It is deliberately a small positive margin, not the centred-reach figure it used to be
/// (the computed inset that let the menu centre on the options orb was negative): centring it on
/// the orb put its right edge ~5pt past the panel's, and the panel's own content clip then sliced
/// the menu's corner, glass edge and shadow into a straight vertical cut. The clamp now trades
/// exact centring for staying whole -- the orb's centre still falls inside the menu's span.
pub(super) const POPOVER_RIGHT_INSET: f64 = 8.0;
pub(super) const POPOVER_TOP: f64 = 70.0;
/// The gap the popover keeps from the panel's bottom edge when the panel is too short to hold the
/// whole menu below `POPOVER_TOP` (it is pulled up rather than clipped).
pub(super) const POPOVER_BOTTOM_INSET: f64 = 8.0;
/// Number of visible housekeeping rows below the hairline (optional 全部清除, then 设置…).
pub(super) fn popover_tail_row_count() -> usize {
    usize::from(text_style::clear_all_button_visible()) + 2
}
/// The shared light window surface, used as the opaque reference for menu ink tests.
#[cfg(test)]
pub(super) const PICKER_POPOVER_SURFACE_LIGHT: u32 = 0xECECECFF;
/// A small lift over the shared panel material, rather than an independent opaque menu tint.
pub(super) const POPOVER_LIFT_TOP_LIGHT: u32 = 0xFFFFFF0C;
pub(super) const POPOVER_LIFT_BOTTOM_LIGHT: u32 = 0xFFFFFF04;
pub(super) const POPOVER_LIFT_TOP_DARK: u32 = 0xFFFFFF08;
/// The light popover's rim. The mock drew a neutral `#362E24` line, which belonged to the beige
/// surface; over the white fill the rim is the sheet's `card_border` stroke, and it is the line
/// (not the fill) that separates the menu from the panel behind it.
pub(super) const PICKER_POPOVER_BORDER_LIGHT: u32 = 0x2D28241A;
/// The popover's rim width. The layer's border stroke is drawn inward, so the row, label and
/// divider insets are derived from it (`popover_row_span`) instead of assuming it away.
pub(super) const POPOVER_BORDER_W: f64 = 1.0;

/// The shared window surface for menu ink reference tests.
#[cfg(test)]
pub(super) fn popover_surface(palette: crate::theme::UiPalette) -> u32 {
    palette.window_bg
}

/// A row's (x, width) inside the menu: the horizontal inset less the rim the layer's border draws
/// inward, so the row wash stops at the rim's inner edge instead of sliding under it. Shared by the
/// filter rows, the housekeeping rows and the section labels, which all sit on that inset.
pub(super) fn popover_row_span() -> (f64, f64) {
    let x = POPOVER_PAD - POPOVER_BORDER_W;
    (x, POPOVER_W - x * 2.0)
}

/// The `.filter-row.is-active` wash and the hover wash (light only; dark keeps hover_bg).
///
/// The accent wash preserves the state contrast floor on the shared opaque reference surface.
pub(super) fn picker_popover_active_bg(palette: crate::theme::UiPalette) -> u32 {
    if palette.dark {
        palette.hover_bg
    } else {
        0x2F6FED28
    }
}
/// `.filter-row.is-active:hover`: the active row's hover deepens the accent wash
/// (`--accent-wash-strong`) instead of switching to the generic one (light only).
pub(super) fn picker_popover_active_hover_bg(palette: crate::theme::UiPalette) -> u32 {
    if palette.dark {
        palette.hover_bg
    } else {
        0x2F6FED2C
    }
}
/// The `.filter-row .check` ink: the mock's `--accent-strong` (#2359c7). Dark keeps the
/// accent -- the deeper blue reads as a shadow on the dark surface. The check identifies the
/// active filter, so the contrast tests hold it to the text floor.
pub(super) fn popover_check_color(palette: crate::theme::UiPalette) -> u32 {
    if palette.dark {
        palette.accent
    } else {
        0x1F50_BAFF
    }
}

/// The picker's top drag band in the picker's own points, top-left origin. The A2 drag scenario has
/// to start its gesture on a real blank part of the panel: a rect derived from the panel frame would
/// put this module's geometry into the test, where a later change to it could not be noticed.
///
/// The name is historical (the field is `grip` in the `--e2e-state` document, and the A2 scenario
/// reads it): the three-dot handle is gone, and every blank region of the panel now drags, but this
/// band stays the reported start area because it is the one region guaranteed to hold no control
/// whatever the locale or the list contents.
pub(crate) fn picker_grip_rect_top_left() -> Option<(f64, f64, f64, f64)> {
    let frame = picker_drag_band_frame();
    Some((
        frame.origin.x,
        frame.origin.y,
        frame.size.width,
        frame.size.height,
    ))
}

/// The visible frame the picker opens in -- the display under the pointer -- top-left based, for
/// `--e2e-state`.
///
/// This is the EXPECTATION a scenario compares `clipboard_picker` against, derived independently of the
/// panel's own rect: with the panel dragged, pinned or placed by the detail layout, the two disagree, and
/// that disagreement is exactly what a scenario asks about. On a single-display machine it can only prove
/// "the panel is on the one display"; the field exists so a two-display machine can assert which one.
/// `None` until a picker window exists, since the picker derives it at summon time.
pub(crate) fn picker_configured_screen_top_left() -> Option<(f64, f64, f64, f64)> {
    if PICKER_WINDOW.lock().unwrap().is_none() {
        return None;
    }
    let (_, screen_frame) = picker::configured_picker_area();
    let primary = unsafe { crate::overlay::hover::primary_screen_frame()? };
    Some((
        screen_frame.origin.x,
        primary.size.height - (screen_frame.origin.y + screen_frame.size.height),
        screen_frame.size.width,
        screen_frame.size.height,
    ))
}

/// The settings button's frame in the picker's own points, top-left origin (None until the header
/// has been built). Read from the live view so the reported rect is the one the user can click.
pub(crate) fn picker_settings_rect_top_left() -> Option<(f64, f64, f64, f64)> {
    let button = (*PICKER_OPTIONS_ORB.lock().unwrap())?;
    unsafe {
        let frame: NSRect = msg_send![button.0, frame];
        Some((
            frame.origin.x,
            frame.origin.y,
            frame.size.width,
            frame.size.height,
        ))
    }
}

/// The options popover's "open settings" row, in the picker's own points with a top-left origin
/// (None until the popover has been built or while it is closed).
///
/// This is the control that actually opens the settings window. `picker_settings_rect_top_left`
/// reports the `...` orb, whose action only opens the popover -- a caller that clicked the orb
/// expecting settings landed on the filter rows instead and never opened anything.
pub(crate) fn picker_settings_row_rect_top_left() -> Option<(f64, f64, f64, f64)> {
    if !filter_popover_open() {
        return None;
    }
    let row = (*PICKER_SETTINGS_ROW.lock().unwrap())?;
    if row.0.is_null() {
        return None;
    }
    unsafe {
        let frame: NSRect = msg_send![row.0, frame];
        let popover = (*FILTER_POPOVER.lock().unwrap())?;
        if popover.0.is_null() {
            return None;
        }
        let popover_frame: NSRect = msg_send![popover.0, frame];
        let parent = (*PICKER_CONTENT_PARENT.lock().unwrap())?;
        let bounds: NSRect = msg_send![parent.0, bounds];
        let parent_flipped: objc2::runtime::Bool = msg_send![parent.0, isFlipped];
        // Three frames stack here: the row's, inside a FLIPPED popover (its rows are laid out
        // top-down), inside the content parent (bottom-up on this platform). So the row's y is
        // already a top-down offset inside the popover and must not be flipped again, while the
        // popover's own origin is in the parent's bottom-up coordinates and must be.
        let popover_top = detail::top_offset_of(
            popover_frame.origin.y,
            bounds.size.height,
            popover_frame.size.height,
            parent_flipped.as_bool(),
        );
        Some((
            popover_frame.origin.x + frame.origin.x,
            popover_top + frame.origin.y,
            frame.size.width,
            frame.size.height,
        ))
    }
}

/// Whether the picker window is on screen right now.
///
/// Asks AppKit rather than reading `PICKER_VISIBLE`: that atomic is this app's *intent*, which can lag (or be
/// wrong), while the question an assertion asks is whether the panel is really on the display. Where the two
/// disagree, AppKit is the answer.
pub(crate) fn picker_is_visible() -> bool {
    unsafe {
        let Some(window) = *PICKER_WINDOW.lock().unwrap() else {
            return false;
        };
        msg_send![window.0, isVisible]
    }
}

/// The detail panel's content container (hosts the text scroll view / the image view;
/// clicking anywhere on the panel dismisses it).
static DETAIL_CONTENT: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
/// The single slot for a freshly generated <=1280px hi-res detail preview (hash, bytes).
//  First priority in show_detail_for_sel's byte lookup -- consumed (cleared) on a hash
//  match so the disk is never re-read; misses fall through to the disk cache / 480px
//  preview. Bounded to one entry (~2MB), overwritten by the next delivery; stale content
//  is dropped by detail_preview_ready.
/// A finished hi-res detail preview waiting for the main thread: entry hash, the cache generation and
/// the hash epoch it was generated in, and the bytes. Both are checked again when the preview is
/// consumed, so a preview generated before the history was discarded -- or before that record was
/// deleted -- can never be shown.
type PendingDetailPreview = (u64, u64, u64, Vec<u8>);
static DETAIL_PENDING_HD: Mutex<Option<PendingDetailPreview>> = Mutex::new(None);
/// In-flight detail-preview generation jobs (hash set): shared by record-time pregen and
/// first-open on-demand requests so the same content never queues twice (arrow-key bounce,
/// record+open races). The worker removes an entry once its job finishes (including skips).
/// De-duplication for detail-preview generation: the jobs currently queued or running, keyed by
/// (hash, cache generation, hash epoch) rather than by hash alone. Clearing one filter scope and
/// copying the same image again must still queue a preview -- the older job for that hash is about to
/// be discarded, and a hash-only marker would swallow the new request, leaving the panel at the
/// low-resolution preview until the user reopens it. Each job removes only its own key.
static DETAIL_INFLIGHT: LazyLock<Mutex<HashSet<(u64, u64, u64)>>> =
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
static DETAIL_TEXT_VIEW: MainThreadSlot<Option<ObjPtr>> = unsafe { MainThreadSlot::new(None) };
/// The code text view using soft-wrap decorations; arrows are drawing-only, while U+2028
/// separators are stripped through the source map.
static DETAIL_SOFT_WRAP_TEXT_VIEW: MainThreadSlot<Option<ObjPtr>> =
    unsafe { MainThreadSlot::new(None) };
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
    use crate::smoke_verdict::smoke_runner;

    /// The collection gate is process-wide, so the tests that drive it must not overlap.
    fn gate_lock() -> std::sync::MutexGuard<'static, ()> {
        super::gate_test_lock()
    }

    /// Every documented mark is present, and Handoff (a deliberate copy made on another device) is
    /// not: that combination is the contract the poll depends on.
    #[test]
    fn sensitive_markers_cover_the_vendor_types_and_exclude_handoff() {
        let markers = super::pasteboard::SENSITIVE_PASTEBOARD_TYPES;
        for expected in [
            "org.nspasteboard.TransientType",
            "org.nspasteboard.ConcealedType",
            "org.nspasteboard.AutoGeneratedType",
            "com.typeit4me.clipping",
            "de.petermaurer.TransientPasteboardType",
            "Pasteboard generator type",
            "com.agilebits.onepassword",
        ] {
            assert!(markers.contains(&expected), "missing marker: {expected}");
        }
        assert!(
            !markers.contains(&"com.apple.is-remote-clipboard"),
            "Handoff content is a user-intended copy and must still be recorded"
        );
    }

    /// A recovery request survives an unreadable pasteboard: clearing it on mere observation would
    /// let a stale baseline record the copy made while nothing could be kept.
    #[test]
    fn a_recovery_request_waits_for_a_baseline_that_succeeds() {
        let _guard = gate_lock();
        super::request_recovery_baseline_for_test();
        super::set_test_pasteboard_unavailable(true);
        assert!(super::handle_recovery_baseline());
        assert!(
            super::recovery_baseline_pending_for_test(),
            "an unreadable pasteboard must not consume the request"
        );
        // The pasteboard comes back: the baseline is taken and the request completes.
        super::set_test_pasteboard_unavailable(false);
        super::set_test_change_count(Some(70));
        assert!(super::handle_recovery_baseline());
        assert!(!super::recovery_baseline_pending_for_test());
        assert_eq!(*super::LAST_CHANGE_COUNT.lock().unwrap(), 70);
        super::set_test_change_count(None);
    }

    /// A resume establishes a baseline, which must also satisfy a pending recovery request; otherwise
    /// the stale request would eat the first normal copy after the resume.
    #[test]
    fn a_successful_resume_satisfies_the_recovery_request() {
        let _guard = gate_lock();
        super::set_test_change_count(Some(80));
        assert!(!super::set_collection_paused(false));
        assert!(super::set_collection_paused(true));
        super::request_recovery_baseline_for_test();
        // The user resumes while the request is still pending.
        super::set_test_change_count(Some(81));
        assert!(!super::set_collection_paused(false));
        assert!(
            !super::recovery_baseline_pending_for_test(),
            "the resume adopted a baseline, so the old recovery request is satisfied"
        );
        // A genuine copy after the resume is therefore NOT consumed by handle_recovery_baseline.
        super::set_test_change_count(Some(82));
        assert!(!super::handle_recovery_baseline());
        super::set_test_change_count(None);
    }

    /// The baseline is taken when writability is published, not at the next poll. Otherwise the first
    /// copy made AFTER the recovery would become the baseline and be dropped.
    #[test]
    fn recovery_takes_its_baseline_before_collection_reopens() {
        let _guard = gate_lock();
        // The pre-recovery content is on the pasteboard when the recovery completes.
        super::set_test_change_count(Some(90));
        super::request_recovery_baseline_for_test();
        assert!(super::handle_recovery_baseline());
        assert_eq!(*super::LAST_CHANGE_COUNT.lock().unwrap(), 90);
        // A copy made after the recovery is a NEW change: it must not be consumed by the stale
        // request, so the next poll records it.
        super::set_test_change_count(Some(91));
        assert!(!super::handle_recovery_baseline());
        assert_eq!(*super::LAST_CHANGE_COUNT.lock().unwrap(), 90);
        super::set_test_change_count(None);
    }

    /// The ordering: `paused` outranks the master switch, and only a genuine pause->resume
    /// transition may try to resume.
    ///
    /// This is the decision `apply_config_change` acts on, so a regression that resumed on
    /// re-enable cannot pass it. The former test asserted `is_resume_transition`, which the
    /// decision now subsumes.
    #[test]
    fn only_a_pause_to_resume_transition_rebaselines() {
        use crate::runtime_config::{pause_gate_action, PauseGateAction};
        assert_eq!(pause_gate_action(true, false), PauseGateAction::Resume);
        // Startup and an ordinary enable keep the documented record-current behaviour.
        assert_eq!(pause_gate_action(false, false), PauseGateAction::Leave);
        // A pause closes the gate, from either state, and a paused session that is re-enabled
        // STAYS paused rather than resuming.
        assert_eq!(pause_gate_action(false, true), PauseGateAction::Pause);
        assert_eq!(pause_gate_action(true, true), PauseGateAction::Pause);
    }

    /// The promise a pause makes: content copied while paused is not recorded later. The counter-example
    /// that motivated the baseline is a copy landing AFTER the last paused poll and BEFORE the resume --
    /// the changeCount is still unseen when collection resumes.
    #[test]
    fn resume_adopts_the_copy_made_while_paused_as_its_baseline() {
        let _guard = gate_lock();
        super::set_test_change_count(Some(41));
        assert!(!super::set_collection_paused(false));
        assert!(super::set_collection_paused(true));
        // A copy lands while paused; no poll runs before the user resumes.
        super::set_test_change_count(Some(42));
        assert!(
            !super::set_collection_paused(false),
            "resume should succeed with a pasteboard"
        );
        assert_eq!(
            *super::LAST_CHANGE_COUNT.lock().unwrap(),
            42,
            "the resume must adopt the paused copy as already seen, so it is never recorded"
        );
        super::set_test_change_count(None);
    }

    /// A resume that cannot read the pasteboard must leave recording off; the caller then keeps the
    /// persisted intent and every surface in the paused state.
    #[test]
    fn resume_without_a_readable_pasteboard_reports_still_paused() {
        let _guard = gate_lock();
        super::set_test_change_count(Some(1));
        assert!(super::set_collection_paused(true));
        super::set_test_pasteboard_unavailable(true);
        assert!(
            super::set_collection_paused(false),
            "an unreadable pasteboard must report the effective paused state"
        );
        assert!(super::collection_paused());
        // Leave the process-wide gate usable for the rest of this binary.
        super::set_test_pasteboard_unavailable(false);
        super::set_test_change_count(Some(7));
        assert!(!super::set_collection_paused(false));
        super::set_test_change_count(None);
    }

    /// The full decision table, including the cases the old union test could not express.
    ///
    /// Why this replaces `excluded_app_matches`: that predicate took `Option<&str>` and so could not
    /// distinguish "this app has no bundle id" from "the lookup failed". The second is the leak it
    /// exists to stop, and the first must still record or a copy made in a non-bundled tool is lost.
    #[test]
    fn the_exclusion_decision_table_holds() {
        use super::{exclusion_decision, ExclusionVerdict, FrontmostSource};
        let list = vec![crate::config::ExcludedApp::new("com.example.pm", "Example")];
        let resolved = |id: Option<&str>| FrontmostSource::Resolved {
            bundle_id: id.map(str::to_string),
        };
        // Resolved and excluded, by either signal.
        assert_eq!(
            exclusion_decision(&list, &resolved(Some("com.example.pm")), None),
            ExclusionVerdict::Excluded
        );
        assert_eq!(
            exclusion_decision(
                &list,
                &resolved(Some("com.other")),
                Some("com.example.pm")
            ),
            ExclusionVerdict::Excluded,
            "the declared source is an additional signal: it can exclude even when the front differs"
        );
        // Case-insensitive, like the list itself.
        assert_eq!(
            exclusion_decision(&list, &resolved(Some("COM.EXAMPLE.PM")), None),
            ExclusionVerdict::Excluded
        );
        // Resolved and different: record.
        assert_eq!(
            exclusion_decision(&list, &resolved(Some("com.other")), Some("com.third")),
            ExclusionVerdict::Record
        );
        // Resolved with NO bundle identifier at all: a real state (a non-bundled tool), not a
        // failed lookup, so the copy is recorded rather than silently lost.
        assert_eq!(
            exclusion_decision(&list, &resolved(None), None),
            ExclusionVerdict::Record
        );
        // The lookup failed with a non-empty list: blocked, whatever the declared id says.
        assert_eq!(
            exclusion_decision(&list, &FrontmostSource::LookupFailed, Some("com.third")),
            ExclusionVerdict::Blocked,
            "an unidentified source cannot be shown to be off the list"
        );
        // An EMPTY list protects nothing, so even a failed lookup must not cost the user copies.
        assert_eq!(
            exclusion_decision(&[], &FrontmostSource::LookupFailed, None),
            ExclusionVerdict::Record
        );
        // A blank entry never matches, not even an empty identifier.
        let blank = vec![crate::config::ExcludedApp::new("  ", "Blank")];
        assert_eq!(
            exclusion_decision(&blank, &resolved(Some("")), Some("")),
            ExclusionVerdict::Record
        );
    }

    /// `org.nspasteboard.source` is self-reported, so only a plausible bundle identifier may be
    /// used at all: anything else must be ignored rather than allowed to influence a verdict.
    #[test]
    fn declared_source_bundle_id_accepts_only_plausible_identifiers() {
        assert_eq!(
            super::pasteboard::valid_source_bundle_id(" com.example.app "),
            Some("com.example.app")
        );
        assert_eq!(super::pasteboard::valid_source_bundle_id(""), None);
        assert_eq!(super::pasteboard::valid_source_bundle_id("   "), None);
        assert_eq!(
            super::pasteboard::valid_source_bundle_id("com.exa mple"),
            None
        );
        assert_eq!(
            super::pasteboard::valid_source_bundle_id("com.example/app"),
            None
        );
        assert_eq!(
            super::pasteboard::valid_source_bundle_id(&"a".repeat(256)),
            None
        );
        assert_eq!(
            super::pasteboard::valid_source_bundle_id(&"a".repeat(255)).map(str::len),
            Some(255)
        );
    }

    /// The row-text and soft-wrap caches key on the material because `glass::panel_ink` returns a different
    /// colour object per material while the palette tokens stay identical: without that, a material switch kept
    /// serving the previous material's ink until a mode change or a restart (the panel's text surface is the
    /// material, so that is the whole ink).
    #[test]
    fn attributed_text_cache_key_is_distinct_per_material() {
        let keys = [
            crate::glass::PanelMaterial::LiquidGlass,
            crate::glass::PanelMaterial::Frost,
            crate::glass::PanelMaterial::Opaque,
            crate::glass::PanelMaterial::Backdrop,
        ]
        .map(super::text_style::material_cache_key_for);
        let mut unique = keys.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            keys.len(),
            "materials share a cache slot: {keys:?}"
        );
    }

    /// Serializes the tests that read or write the shared image-cache directory (or the history file
    /// inside it): `clear_image_cache_dir` and a full discard wipe the whole directory, so a test
    /// asserting on cache files must not run while another one clears them. Also parks the storage
    /// session in a writable state with the test key: the cache helpers refuse to write while the
    /// storage state says otherwise, exactly like production.
    fn clip_cache_dir_guard() -> std::sync::MutexGuard<'static, ()> {
        let guard = super::storage::test_lock();
        super::storage::set_ready(super::keyring::test_key());
        guard
    }

    use super::{
        cache_generation, clip_image_detail_path, clip_image_path, effective_hover_row,
        estimated_entry_bytes, fnv1a64, header_strip_h, options_orb_dot_frame,
        orb_dot_is_drawn_in_full, picker_drag_band_frame, picker_header_frames,
        rect_contains_point, run_image_cache_job, scroll_indicator_geometry,
        wipe_cache_for_discard, ClipEntry, ImageCacheJob, ImageEntry, CLIP_HISTORY, NO_SELECTION,
        NSPASTEBOARD_TYPE_PNG, PICKER_CLOSE_BTN_GAP, PICKER_CLOSE_BTN_SIZE, PICKER_ORB_DOT_GAP,
        PICKER_ORB_DOT_SIZE, PICKER_ORB_SIZE, PICKER_W, SCROLL_INDICATOR_CORNER_RESERVE,
        SCROLL_INDICATOR_EDGE, SEARCH_PAD_X, TOP_PAD_Y,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use std::sync::Arc;
    #[test]
    fn picker_header_separates_the_four_corner_controls() {
        let (search, pause, options, pin, close) = picker_header_frames();
        // The row is inset symmetrically from the strip's edges and all three buttons are square.
        assert_eq!(search.origin.x, SEARCH_PAD_X);
        assert_eq!(search.origin.y, TOP_PAD_Y);
        assert_eq!(close.origin.x + close.size.width, PICKER_W - SEARCH_PAD_X);
        assert_eq!(close.size.width, close.size.height);
        // The header's `.orb` pair (options/pin) are equal 34pt circles; the close keeps its own
        // smaller rect (`.close-btn` 30pt wide). The detail toggle lives on each row instead.
        assert_eq!(options.size, pin.size);
        assert_eq!(pin.size.width, PICKER_ORB_SIZE);
        assert_eq!(close.size.width, PICKER_CLOSE_BTN_SIZE);
        // Every control is clear of the one before it: the close button is never drawn over the
        // pin, the pin never over the gear, the gear never over the query/⌘F keycap, and none of
        // them overlap whichever label is longest. The pause orb is part of the same group and
        // shares the orb metrics.
        assert_eq!(pause.size, options.size);
        assert!(search.origin.x + search.size.width < pause.origin.x);
        assert!(pause.origin.x + pause.size.width < options.origin.x);
        assert!(options.origin.x + options.size.width < pin.origin.x);
        assert!(pin.origin.x + pin.size.width < close.origin.x);
        // The three orbs are evenly spaced by the one gap constant, so the row reads as a group.
        assert_eq!(
            options.origin.x - (pause.origin.x + pause.size.width),
            PICKER_CLOSE_BTN_GAP
        );
        assert_eq!(
            pin.origin.x - (options.origin.x + options.size.width),
            PICKER_CLOSE_BTN_GAP
        );
        assert_eq!(
            close.origin.x - (pin.origin.x + pin.size.width),
            PICKER_CLOSE_BTN_GAP
        );
        // All five stay inside the fixed strip (never over the filters row or the list).
        let strip_bottom = header_strip_h();
        for frame in [search, pause, options, pin, close] {
            assert!(frame.origin.y + frame.size.height <= strip_bottom);
        }
        // The drag band is the WHOLE top bar: it sits in the top padding the strip already reserves
        // (so it adds no height), spans the panel's full width, and ends exactly where the search row
        // begins -- which is also why it cannot reach any of the three controls.
        let band = picker_drag_band_frame();
        assert_eq!(band.origin.x, 0.0);
        assert_eq!(band.size.width, PICKER_W);
        assert_eq!(band.origin.y + band.size.height, search.origin.y);
        assert!(
            band.size.height >= 12.0,
            "the top drag band must stay a usable press target"
        );
    }
    #[test]
    fn options_orb_dot_is_drawn_whole_inside_the_orb_s_circle() {
        let dot = options_orb_dot_frame();
        assert!(
            orb_dot_is_drawn_in_full(PICKER_ORB_SIZE, dot),
            "the orb-dot must be drawn whole, not cut by the orb's clip (frame {dot:?})"
        );
        // It still reads as the orb's corner satellite: inside the orb, in its top-right corner.
        assert!(dot.origin.x > PICKER_ORB_SIZE / 2.0);
        assert!(dot.origin.y < PICKER_ORB_SIZE / 2.0);
        assert!(dot.origin.x + dot.size.width <= PICKER_ORB_SIZE);
        assert!(dot.origin.y + dot.size.height <= PICKER_ORB_SIZE);
        assert_eq!(dot.size.width, PICKER_ORB_DOT_SIZE);
        assert_eq!(dot.size.height, PICKER_ORB_DOT_SIZE);
        // The reported defect: pinned 3pt from the top and 8pt from the right, most of the disc
        // was cut away by the orb's circular clip. That exact frame must stay rejected.
        let reported = NSRect::new(
            NSPoint::new(PICKER_ORB_SIZE - 8.0, 3.0),
            NSSize::new(PICKER_ORB_DOT_SIZE, PICKER_ORB_DOT_SIZE),
        );
        assert!(
            !orb_dot_is_drawn_in_full(PICKER_ORB_SIZE, reported),
            "the frame the clipping defect was reported against must not pass"
        );
        // Pinned flush to the orb's edge the disc hangs over the ring and is cut too.
        let flush = NSRect::new(
            NSPoint::new(PICKER_ORB_SIZE - PICKER_ORB_DOT_SIZE, 0.0),
            NSSize::new(PICKER_ORB_DOT_SIZE, PICKER_ORB_DOT_SIZE),
        );
        assert!(!orb_dot_is_drawn_in_full(PICKER_ORB_SIZE, flush));
        // A dot hanging past the orb's bounds is clipped by the layer bounds as well.
        let outside = NSRect::new(
            NSPoint::new(PICKER_ORB_SIZE - 2.0, PICKER_ORB_DOT_GAP),
            NSSize::new(PICKER_ORB_DOT_SIZE, PICKER_ORB_DOT_SIZE),
        );
        assert!(!orb_dot_is_drawn_in_full(PICKER_ORB_SIZE, outside));
        // The dot has to be visible as well as uncut: over the orb's centre the disc is inside
        // the glyph, so a frame there is not a usable badge position.
        let centred = NSRect::new(NSPoint::new(15.0, 15.0), NSSize::new(5.0, 5.0));
        assert!(orb_dot_is_drawn_in_full(PICKER_ORB_SIZE, centred));
        // The constant's documented threshold, `14.5 - 14.5 / sqrt(2) = 4.2469...`: 4.2469pt is
        // still cut and 4.247pt is not, so the 6pt in use is that boundary plus margin rather than
        // a guess. Both sides are asserted so the comment on the constant cannot drift.
        let frame_at = |gap: f64| {
            NSRect::new(
                NSPoint::new(PICKER_ORB_SIZE - gap - PICKER_ORB_DOT_SIZE, gap),
                NSSize::new(PICKER_ORB_DOT_SIZE, PICKER_ORB_DOT_SIZE),
            )
        };
        assert!(!orb_dot_is_drawn_in_full(PICKER_ORB_SIZE, frame_at(4.22)));
        assert!(!orb_dot_is_drawn_in_full(PICKER_ORB_SIZE, frame_at(4.2469)));
        assert!(orb_dot_is_drawn_in_full(PICKER_ORB_SIZE, frame_at(4.247)));
        assert!(orb_dot_is_drawn_in_full(PICKER_ORB_SIZE, frame_at(5.0)));
        assert!(orb_dot_is_drawn_in_full(
            PICKER_ORB_SIZE,
            frame_at(PICKER_ORB_DOT_GAP)
        ));
        for gap in [0.0, 1.0, 2.0] {
            let frame = NSRect::new(
                NSPoint::new(PICKER_ORB_SIZE - gap - PICKER_ORB_DOT_SIZE, gap),
                NSSize::new(PICKER_ORB_DOT_SIZE, PICKER_ORB_DOT_SIZE),
            );
            assert!(
                !orb_dot_is_drawn_in_full(PICKER_ORB_SIZE, frame),
                "a {gap}pt gap still lets the orb's circular clip cut the dot"
            );
        }
    }

    #[test]
    fn incremental_row_delete_declines_an_empty_result() {
        use super::detail::incremental_row_delete_applies;
        // The normal one-row case: every row materialized, one row left.
        assert!(incremental_row_delete_applies(true, 1, 2));
        // Deleting the last row must fall back to a full rebuild: that is where the empty-state
        // hint and the minimal document height come from (a blank list with no hint was the bug).
        assert!(!incremental_row_delete_applies(true, 0, 1));
        // Partially materialized (virtualized) lists keep using the full rebuild path.
        assert!(!incremental_row_delete_applies(false, 1, 2));
        // A deletion that is not exactly one row is never incremental.
        assert!(!incremental_row_delete_applies(true, 5, 2));
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
    #[test]
    fn detail_scroll_thumb_length_uses_the_shared_proportion() {
        // Phase 2 unified the thumb length on `usable_track * visible / total`. Here the track is
        // `200 - 2*edge`, so the thumb is `192 * 200 / 1000` rather than the old `200^2 / 1000`.
        let (_, length) = scroll_indicator_geometry(200.0, 1000.0, 0.0, 0.0)
            .expect("overflowing detail content must produce a thumb");
        let track = 200.0 - SCROLL_INDICATOR_EDGE * 2.0;
        let expected = track * 200.0 / 1000.0;
        assert!((length - expected).abs() < 1e-9);
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
                    generation: 0,
                    epoch: 0,
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
    /// A text entry copied at `ts`: the day-group tests need an entry in a known group.
    fn entry_at(text: &str, ts: Option<u64>) -> ClipEntry {
        ClipEntry {
            copied_at: ts,
            ..entry(text)
        }
    }
    /// An image entry copied at `ts`.
    fn entry_image_at(png: &[u8], ts: Option<u64>) -> ClipEntry {
        ClipEntry {
            copied_at: ts,
            ..entry_image(png)
        }
    }
    /// A timestamp exactly `days` local calendar days before `now`. Derived by walking back an hour
    /// at a time from `day_no` itself, so a fixture stays correct across a DST transition (where
    /// `now - days * 86_400` can land two calendar days back).
    fn ts_with_day_delta(now: u64, days: i64) -> u64 {
        let today = super::day_no(now);
        let mut ts = now;
        loop {
            if today - super::day_no(ts) == days {
                return ts;
            }
            assert!(ts > 3600, "no timestamp {days} days before {now}");
            ts -= 3600;
        }
    }
    /// An 8x8 solid PNG: real, decodable bytes for the tests that must actually generate a preview
    /// (the same fixture the clipboard smoke uses).
    const TEST_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x08, 0x08, 0x02, 0x00, 0x00, 0x00, 0x4B,
        0x6D, 0x29, 0xDC, 0x00, 0x00, 0x00, 0x11, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x38,
        0xA1, 0xA1, 0x81, 0x15, 0x31, 0x0C, 0x2D, 0x09, 0x00, 0x82, 0x5D, 0x46, 0x01, 0x6A, 0x8D,
        0x16, 0x6B, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

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
        let _cache_guard = clip_cache_dir_guard();
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
        let _cache_guard = clip_cache_dir_guard();
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
        let _cache_guard = clip_cache_dir_guard();
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
        let _cache_guard = clip_cache_dir_guard();
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
        let _cache_guard = clip_cache_dir_guard();
        use super::{
            cache_read_detail_preview, cache_read_image, cache_read_preview,
            cache_write_detail_preview, cache_write_image, cache_write_preview,
            clear_image_cache_dir, clip_image_cache_dir, clip_image_detail_path, clip_image_path,
            clip_image_preview_path, sweep_clip_image_cache,
        };
        clear_image_cache_dir(&clip_image_cache_dir());
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
        clear_image_cache_dir(&clip_image_cache_dir());
    }
    #[test]
    fn clear_clip_image_cache_wipes_the_test_dir_only() {
        let _cache_guard = clip_cache_dir_guard();
        use super::{
            cache_read_image, cache_write_image, clear_image_cache_dir, clip_image_cache_dir,
            fnv1a64,
        };
        let a = b"wipe-test-a";
        let b = b"wipe-test-b";
        let (ha, hb) = (fnv1a64(a), fnv1a64(b));
        assert!(cache_write_image(ha, a));
        assert!(cache_write_image(hb, b));
        clear_image_cache_dir(&clip_image_cache_dir());
        assert_eq!(cache_read_image(ha), None);
        assert_eq!(cache_read_image(hb), None);
    }
    #[test]
    fn cache_preview_roundtrip_and_delete_removes_both() {
        let _cache_guard = clip_cache_dir_guard();
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
        let _cache_guard = clip_cache_dir_guard();
        use super::{
            cache_write_image, clip_image_path, fnv1a64, history_file_path, load_history,
            ImageEntry, CLIP_HISTORY, NSPASTEBOARD_TYPE_PNG,
        };
        // Regression: DISTINCT data-image entries (web copies, source_path always None)
        // must all survive; the old all-images-by-source_path dedup (None==None) dropped
        // every entry after the first.
        let prev_ttl = {
            let cfg = crate::config::CONFIG.read().unwrap();
            cfg.clipboard.auto_expire_days
        };
        crate::config::CONFIG
            .write()
            .unwrap()
            .clipboard
            .auto_expire_days = 0;
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
        // The live history file is sealed now: write it the way the app does (test key).
        assert!(super::persist::test_support::write_sealed_history(&[
            a, b, c
        ]));
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
        assert!(super::persist::test_support::write_sealed_history(&[mk(
            b"load-keep-a"
        )]));
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
        crate::config::CONFIG
            .write()
            .unwrap()
            .clipboard
            .auto_expire_days = prev_ttl;
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
        let _cache_guard = clip_cache_dir_guard();
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
        let _cache_guard = clip_cache_dir_guard();
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
        use super::{remove_history_scope, ClipFilter};
        let mut pinned = entry("keep");
        pinned.pinned = true;
        let mut history = vec![pinned, entry("drop")];
        let removed = remove_history_scope(&mut history, false, ClipFilter::All, "");
        assert_eq!(texts(&history), vec!["keep"]);
        assert_eq!(texts(&removed), vec!["drop"]);
        let removed = remove_history_scope(&mut history, true, ClipFilter::All, "");
        assert!(history.is_empty());
        assert_eq!(texts(&removed), vec!["keep"]);
    }

    #[test]
    fn clear_scope_follows_the_active_filter_and_query() {
        use super::{remove_history_scope, ClipFilter};
        // The header's clear actions are destructive with no undo, so they only ever touch what the
        // user can see: the active filter's category, narrowed by the query.
        let mut history = vec![
            entry("apple text"),
            ClipEntry {
                text: String::new(),
                image: Some(image(b"apple image")),
                pinned: false,
                source_app: String::new(),
                source_key: String::new(),
                copied_at: None,
            },
            entry("banana text"),
        ];
        // Filter = Image: only the image goes, the text stays whatever it says.
        let removed = remove_history_scope(&mut history, true, ClipFilter::Image, "");
        assert_eq!(texts(&history), vec!["apple text", "banana text"]);
        assert_eq!(removed.len(), 1);
        // Filter = Text + query: only the matching, visible entry goes; the non-matching text
        // entry (never on screen under that query) survives.
        let removed = remove_history_scope(&mut history, true, ClipFilter::Text, "apple");
        assert_eq!(texts(&history), vec!["banana text"]);
        assert_eq!(removed.len(), 1);
        // A query with no matches clears nothing.
        let removed = remove_history_scope(&mut history, true, ClipFilter::All, "nothing matches");
        assert_eq!(removed.len(), 0);
        assert_eq!(history.len(), 1);
    }

    /// A group header's "clear" narrows the visible scope to one day group; it never widens it.
    /// The defect it pins: the first implementation filtered the history by date alone, so clearing
    /// "today" under the Image filter deleted the day's hidden text entries as well.
    #[test]
    fn a_day_group_clear_only_takes_what_is_on_screen() {
        use super::{remove_day_group_scope, ClipFilter, DayGroup};
        let now = super::now_secs();
        let yesterday = Some(ts_with_day_delta(now, 1));
        let mut history = vec![
            entry_at("today text", Some(now)),
            entry_image_at(b"today image", Some(now)),
            entry_image_at(b"yesterday image", yesterday),
            entry_at("yesterday text", yesterday),
        ];
        // Image filter: the day group intersects it, so only today's image goes.
        let removed =
            remove_day_group_scope(&mut history, false, ClipFilter::Image, "", DayGroup::Today);
        assert_eq!(removed.len(), 1);
        assert!(removed[0].image.is_some());
        assert_eq!(
            texts(&history),
            vec!["today text", "", "yesterday text"],
            "an entry the filter hides must survive the group clear"
        );
    }

    /// The search query is the other half of the visible scope: a group clear takes the matching
    /// rows of that day, not the whole day.
    #[test]
    fn a_day_group_clear_follows_the_search_query() {
        use super::{remove_day_group_scope, ClipFilter, DayGroup};
        let now = super::now_secs();
        let mut history = vec![
            entry_at("apple today", Some(now)),
            entry_at("banana today", Some(now)),
            entry_at("apple yesterday", Some(ts_with_day_delta(now, 1))),
        ];
        let removed = remove_day_group_scope(
            &mut history,
            false,
            ClipFilter::All,
            "apple",
            DayGroup::Today,
        );
        assert_eq!(texts(&removed), vec!["apple today"]);
        assert_eq!(
            texts(&history),
            vec!["banana today", "apple yesterday"],
            "the query's non-matches and the other day both survive"
        );
    }

    /// The day boundary is a hard edge: clearing Today cannot reach Yesterday or Earlier, and the
    /// group with nothing visible removes nothing.
    #[test]
    fn a_day_group_clear_stops_at_the_day_boundary() {
        use super::{remove_day_group_scope, ClipFilter, DayGroup};
        let now = super::now_secs();
        let mut history = vec![
            entry_at("today", Some(now)),
            entry_at("yesterday", Some(ts_with_day_delta(now, 1))),
            entry_at("earlier", Some(ts_with_day_delta(now, 3))),
            entry_at("legacy without a timestamp", None),
        ];
        let removed =
            remove_day_group_scope(&mut history, false, ClipFilter::All, "", DayGroup::Today);
        assert_eq!(texts(&removed), vec!["today"]);
        assert_eq!(
            texts(&history),
            vec!["yesterday", "earlier", "legacy without a timestamp"]
        );
        // Yesterday is equally narrow.
        let removed = remove_day_group_scope(
            &mut history,
            false,
            ClipFilter::All,
            "",
            DayGroup::Yesterday,
        );
        assert_eq!(texts(&removed), vec!["yesterday"]);
        // A group whose visible set is empty (Image filter, no image today) removes nothing.
        let before = texts(&history);
        let removed = remove_day_group_scope(
            &mut history,
            false,
            ClipFilter::Image,
            "",
            DayGroup::Earlier,
        );
        assert!(removed.is_empty());
        assert_eq!(texts(&history), before);
    }

    /// Same pinned rule as the plain clear: a pinned entry survives a group clear, and only the
    /// explicit "clear all" takes it.
    #[test]
    fn a_day_group_clear_keeps_pinned_entries_unless_clear_all_asks() {
        use super::{remove_day_group_scope, ClipFilter, DayGroup};
        let now = super::now_secs();
        let mut pinned = entry_at("pinned today", Some(now));
        pinned.pinned = true;
        let mut history = vec![pinned, entry_at("plain today", Some(now))];
        let removed =
            remove_day_group_scope(&mut history, false, ClipFilter::All, "", DayGroup::Today);
        assert_eq!(texts(&removed), vec!["plain today"]);
        assert_eq!(texts(&history), vec!["pinned today"]);
        let removed =
            remove_day_group_scope(&mut history, true, ClipFilter::All, "", DayGroup::Today);
        assert_eq!(texts(&removed), vec!["pinned today"]);
        assert!(history.is_empty());
    }

    /// The groups must run FORWARDS through the calendar: an entry copied before today is
    /// `Yesterday` for exactly one day and `Earlier` after that. The day difference used to be
    /// subtracted the wrong way round, which made `Yesterday` unreachable (the header never
    /// appeared, the group-clear tag 1 never matched) and put every older entry in `Earlier`.
    #[test]
    fn day_groups_are_ordered_forwards_through_the_calendar() {
        use super::{day_group, DayGroup};
        let now = super::now_secs();
        assert_eq!(day_group(Some(now)), DayGroup::Today);
        assert_eq!(
            day_group(Some(ts_with_day_delta(now, 1))),
            DayGroup::Yesterday
        );
        assert_eq!(
            day_group(Some(ts_with_day_delta(now, 2))),
            DayGroup::Earlier
        );
        assert_eq!(day_group(None), DayGroup::Earlier);
    }
    #[test]
    fn a_delivered_preview_needs_a_current_generation_epoch_and_selection() {
        use super::detail_slot_deliverable;
        // The delivery decision at the end of the (async) preview pipeline. Every axis matters:
        // - a preview generated before a whole-history discard describes a deleted entry,
        // - a preview generated before that RECORD was deleted (one filter scope cleared) describes a
        //   deleted entry even though the history generation never changed,
        // - a preview the user has navigated away from must not be shown even when it is current.
        assert!(detail_slot_deliverable(7, 7, 3, 3, true));
        assert!(
            !detail_slot_deliverable(6, 7, 3, 3, true),
            "a preview from before a discard must be refused"
        );
        assert!(
            !detail_slot_deliverable(7, 7, 2, 3, true),
            "a preview from before its record was deleted must be refused"
        );
        assert!(!detail_slot_deliverable(7, 7, 3, 3, false));
        assert!(!detail_slot_deliverable(6, 7, 2, 3, false));
    }

    #[test]
    fn only_our_own_history_temp_files_are_recognized() {
        use super::is_history_temp_file_name;
        // Exactly `clipboard-history.toml.tmp<pid>-<generation>`; anything else in that directory
        // (another program's file, a backup, a partial name) must never be swept.
        assert!(is_history_temp_file_name(
            "clipboard-history.toml.tmp1234-7"
        ));
        assert!(is_history_temp_file_name("clipboard-history.toml.tmp1-0"));
        for lookalike in [
            "clipboard-history.toml",
            "clipboard-history.toml.tmp",
            "clipboard-history.toml.tmp1234",
            "clipboard-history.toml.tmp-7",
            "clipboard-history.toml.tmp1234-",
            "clipboard-history.toml.tmp1234-7-8",
            "clipboard-history.toml.tmpa-7",
            "clipboard-history.toml.tmp1234-x",
            "clipboard-history.bak",
            "clipboard-history.toml.tmp1234-7.tmp",
            "other.tmp1234-7",
            "clipboard-history.toml.tmp1234-7 ",
        ] {
            assert!(
                !is_history_temp_file_name(lookalike),
                "{lookalike:?} must not be recognized"
            );
        }
    }

    #[test]
    fn a_session_that_cannot_save_always_has_a_notice_to_post() {
        use super::{unavailable_copy_key, BlockedReason, StorageState};
        use crate::clipboard::KeyUnavailable;
        // Both failure states must produce a system notification: a session that cannot save, or
        // cannot read what is saved, must never stay silent because the picker was never opened.
        for state in [
            StorageState::Unavailable(KeyUnavailable::AccessDenied),
            StorageState::Unavailable(KeyUnavailable::Simulated),
            StorageState::Blocked(BlockedReason::Damaged),
            StorageState::Blocked(BlockedReason::PurgePending),
        ] {
            assert!(
                unavailable_copy_key(&state).is_some(),
                "{state:?} must have a notice"
            );
        }
        // A healthy or not-yet-decided session has nothing to announce.
        assert!(unavailable_copy_key(&StorageState::Ready).is_none());
        assert!(unavailable_copy_key(&StorageState::Uninitialized).is_none());
    }

    #[test]
    fn an_image_without_bytes_is_refused_with_a_visible_notice() {
        use super::paste_refused_with_notice;
        // Only an image whose bytes cannot be produced: a text entry, a file entry or an intact
        // image must still go through the normal paste path.
        assert!(paste_refused_with_notice(true, false));
        assert!(!paste_refused_with_notice(true, true));
        assert!(!paste_refused_with_notice(false, false));
        assert!(!paste_refused_with_notice(false, true));
    }

    #[test]
    fn discarding_the_history_removes_leftover_temp_files() {
        use super::{discard_history_in, is_history_temp_file_name};
        // The cleanup bumps the global generation and clears the shared pending/epoch state, so it
        // must hold the same lock as every other cache test (a private temp dir only isolates files).
        let _cache_guard = clip_cache_dir_guard();
        // A crash between the temp write and the rename leaves the whole plaintext history in a temp
        // file; the clearing paths must not leave it behind, and must not touch anything else.
        let dir = std::env::temp_dir().join(format!("oh-my-tab-tmp-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let history_dir = dir.join("history");
        let cache_dir = dir.join("cache");
        std::fs::create_dir_all(&history_dir).unwrap();
        std::fs::create_dir_all(&cache_dir).unwrap();
        let history = history_dir.join("clipboard-history.toml");
        let leftover = history_dir.join("clipboard-history.toml.tmp4242-9");
        let lookalike = history_dir.join("clipboard-history.bak");
        std::fs::write(&history, "version = 1\nentries = []\n").unwrap();
        std::fs::write(&leftover, "version = 1\nentries = []\n").unwrap();
        std::fs::write(&lookalike, "keep me").unwrap();
        discard_history_in(&history, &cache_dir);
        assert!(!history.exists(), "the history file must go");
        assert!(
            !leftover.exists(),
            "a leftover temp file holds the same plaintext history and must go too"
        );
        assert!(lookalike.exists(), "an unrelated file must stay");
        assert!(is_history_temp_file_name(
            "clipboard-history.toml.tmp4242-9"
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stale_inflight_preview_does_not_block_the_rerecorded_request() {
        use super::{
            cache_write_image, request_detail_preview, run_detail_preview_job, DetailPreviewJob,
            ImageEntry, DETAIL_INFLIGHT,
        };
        use std::os::unix::fs::OpenOptionsExt;
        use std::time::{Duration, Instant};
        const LIB_FCNTL_O_NONBLOCK: i32 = 0x0004;
        let _cache_guard = clip_cache_dir_guard();
        // Reviewer's sequence: copy an image, open its detail (the hi-res job starts), clear that
        // record, copy the same image again and open the detail again -- all while the first job is
        // still decoding. De-duplicating by hash alone would swallow the second request, and the
        // older job is discarded (its epoch is stale), so nothing would refresh the panel: it would
        // sit on the low-resolution preview until the user reopened it. The marker keys on the job's
        // identity instead, and a job releases only its own.
        let root = std::env::temp_dir().join(format!("oh-my-tab-inflight-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let fifo = root.join("slow.pipe");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        assert!(
            made.map(|status| status.success()).unwrap_or(false),
            "mkfifo must be available"
        );
        let bytes: &[u8] = TEST_PNG;
        let hash = fnv1a64(bytes);
        // The first record is a FILE copy, so its job reads the (blocking) source path.
        let first = ImageEntry {
            uti: NSPASTEBOARD_TYPE_PNG.to_string(),
            hash,
            data_path: std::path::PathBuf::new(),
            preview_png: std::sync::Arc::new(bytes.to_vec()),
            source_path: Some(fifo.to_string_lossy().to_string()),
        };
        request_detail_preview(&first, false);
        // Opening the FIFO for write without blocking succeeds only once the worker is inside its
        // read: that pins the first job mid-decode without guessing with a sleep.
        let started_at = Instant::now();
        // The handle is kept only to be closed: closing every writer is what makes the blocked read
        // return (EOF), which is what finishes the pinned job.
        let _release = loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(LIB_FCNTL_O_NONBLOCK)
                .open(&fifo)
            {
                Ok(handle) => break handle,
                Err(error) => {
                    assert!(
                        started_at.elapsed() < Duration::from_secs(10),
                        "the first job never entered its read: {error}"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        };
        // The record is deleted, then the same image is copied again.
        let generation = super::cache_generation();
        let stale_epoch = super::hash_epoch_for_new_job(hash);
        super::cache_delete_for_hash(&[], hash);
        assert!(cache_write_image(hash, bytes));
        // Release the first job: it must finish without writing anything for the deleted record.
        let second = ImageEntry {
            uti: NSPASTEBOARD_TYPE_PNG.to_string(),
            hash,
            data_path: super::clip_image_path(hash),
            preview_png: std::sync::Arc::new(bytes.to_vec()),
            source_path: None,
        };
        request_detail_preview(&second, false);
        assert!(
            DETAIL_INFLIGHT.lock().unwrap().contains(&(
                hash,
                generation,
                super::hash_epoch_for_new_job(hash)
            )),
            "the re-recorded request must be queued, not swallowed by the older job's marker"
        );
        assert_ne!(super::hash_epoch_for_new_job(hash), stale_epoch);
        assert!(
            !super::clip_image_detail_path(hash).exists(),
            "the stale job must not have written a preview for the deleted record"
        );
        // The queued job runs (enqueued by the request above; run here on this thread so the
        // observation is deterministic -- the worker's cache directory is per thread).
        unsafe {
            run_detail_preview_job(&DetailPreviewJob {
                hash,
                source_path: None,
                deliver: false,
                detail_visible: false,
                selected_hash: None,
                generation,
                epoch: super::hash_epoch_for_new_job(hash),
            });
        }
        assert!(
            super::clip_image_detail_path(hash).exists(),
            "the re-recorded image must get its hi-res preview generated"
        );
        assert!(
            !DETAIL_INFLIGHT.lock().unwrap().contains(&(
                hash,
                generation,
                super::hash_epoch_for_new_job(hash)
            )),
            "the finished job releases its own marker"
        );
        let _ = std::fs::remove_file(super::clip_image_detail_path(hash));
        let _ = std::fs::remove_file(super::clip_image_path(hash));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_preview_decoded_before_the_deletion_cannot_be_delivered() {
        use super::{offer_detail_preview, DETAIL_PENDING_HD};
        let _cache_guard = clip_cache_dir_guard();
        // Finding: the preview pipeline decodes outside the cache-write lock, so clearing one filter
        // scope can land while a preview for one of the deleted images is being encoded. The file
        // write is refused by the hash epoch, and the DELIVERY must be refused too -- otherwise the
        // deleted image's bytes come back into memory and the panel shows them.
        let hash = fnv1a64(b"deleted-while-decoding");
        let generation = super::cache_generation();
        let epoch = super::hash_epoch_for_new_job(hash);
        *DETAIL_PENDING_HD.lock().unwrap() = None;
        super::cache_delete_for_hash(&[], hash); // the record is deleted mid-decode
        assert!(
            !offer_detail_preview(generation, hash, epoch, vec![1, 2, 3]),
            "a preview decoded before its record was deleted must not reach the slot"
        );
        assert!(
            DETAIL_PENDING_HD.lock().unwrap().is_none(),
            "the slot must stay empty for a deleted record"
        );
        // The same preview for a record that is still alive is delivered as usual.
        let kept_hash = fnv1a64(b"kept-while-decoding");
        assert!(offer_detail_preview(
            generation,
            kept_hash,
            super::hash_epoch_for_new_job(kept_hash),
            vec![4, 5, 6]
        ));
        *DETAIL_PENDING_HD.lock().unwrap() = None;
    }

    #[test]
    fn clearing_a_scope_retires_its_hashes_but_keeps_other_records_jobs_alive() {
        use super::{
            cache_delete_for_hash, clip_image_preview_path, current_hash_epoch,
            hash_epoch_for_new_job, image_cache::PENDING_IMAGE_DATA, insert_pending_for_tests,
            run_image_cache_job, ImageCacheJob,
        };
        let _cache_guard = clip_cache_dir_guard();
        // Clearing "Images" (or a search result) must kill the cache work for the entries it deleted:
        // a job queued a moment earlier still has a valid generation, so without the per-hash epoch
        // it would write the deleted record's file back after the clear.
        let cleared_hash = fnv1a64(b"cleared-image");
        let kept_hash = fnv1a64(b"kept-image");
        let generation = super::cache_generation();
        let job = |hash: u64, bytes: &[u8]| ImageCacheJob {
            hash,
            data: Some(std::sync::Arc::new(bytes.to_vec())),
            preview: std::sync::Arc::new(b"preview".to_vec()),
            source_path: None,
            warm_detail: false,
            generation,
            epoch: hash_epoch_for_new_job(hash),
        };
        // A job queued before the clear holds the epoch of that moment.
        let queued_job = job(cleared_hash, b"cleared-image");
        insert_pending_for_tests(cleared_hash, std::sync::Arc::new(b"cleared-image".to_vec()));
        cache_delete_for_hash(&[], cleared_hash);
        assert_eq!(
            current_hash_epoch(cleared_hash),
            1,
            "deleting the record bumps its epoch"
        );
        // The image is copied again (a new record): that admits a NEW job for the hash, and must not
        // re-admit the one queued before the deletion -- which is why the epoch is not reset here.
        let new_job = job(cleared_hash, b"cleared-image");
        run_image_cache_job(&queued_job);
        assert!(
            !super::clip_image_path(cleared_hash).exists(),
            "a job queued before the deletion must not write, even after the image is recorded again"
        );
        assert!(
            !clip_image_preview_path(cleared_hash).exists(),
            "nor its preview"
        );
        assert!(
            !PENDING_IMAGE_DATA
                .lock()
                .unwrap()
                .contains_key(&cleared_hash),
            "its pending bytes must go too"
        );
        // The new job -- queued after the deletion -- writes normally.
        run_image_cache_job(&new_job);
        assert!(
            super::clip_image_path(cleared_hash).exists(),
            "the re-recorded image must be writable by its new job"
        );
        let _ = std::fs::remove_file(super::clip_image_path(cleared_hash));
        // ...while another record's queued job was never affected.
        run_image_cache_job(&job(kept_hash, b"kept-image"));
        assert!(
            super::clip_image_path(kept_hash).exists(),
            "another record's job must still write"
        );
        let _ = std::fs::remove_file(super::clip_image_path(kept_hash));
        let _ = std::fs::remove_file(clip_image_preview_path(kept_hash));
    }

    #[test]
    fn an_old_write_cannot_retire_a_rerecorded_entrys_pending_bytes() {
        use super::{
            cache_write_image, clip_image_cache_dir, image_cache::PENDING_IMAGE_DATA,
            insert_pending_for_tests, retire_pending_after_write,
        };
        let _cache_guard = clip_cache_dir_guard();
        // The interleaving this pins, with the old job's pause made explicit (the pause between
        // writing and retiring is what makes the race reachable):
        //   1. the old job writes the image (its generation is still current),
        //   2. the user turns the feature off: the wipe deletes the file, clears the fallback map and
        //      bumps the generation,
        //   3. the user copies the same image again -- same hash, a fresh pending entry, its file not
        //      written yet,
        //   4. the old job resumes and retires "its" entry.
        // Retiring by hash would delete the bytes step 3 queued, and a paste or save-as would then
        // find neither the file nor the fallback bytes.
        let hash = fnv1a64(b"same-image-twice");
        let bytes = b"same-image-twice";
        let old_generation = super::cache_generation();
        let old_epoch = super::hash_epoch_for_new_job(hash);
        assert!(
            cache_write_image(hash, bytes),
            "step 1: the old job's write"
        );
        super::wipe_cache_for_discard(&clip_image_cache_dir()); // step 2: switch off
        assert!(
            !super::clip_image_path(hash).exists(),
            "the wipe must delete the written file"
        );
        assert!(PENDING_IMAGE_DATA.lock().unwrap().is_empty());
        insert_pending_for_tests(hash, std::sync::Arc::new(bytes.to_vec())); // step 3
        retire_pending_after_write(hash, old_generation, old_epoch); // step 4
        assert!(
            PENDING_IMAGE_DATA.lock().unwrap().contains_key(&hash),
            "the re-recorded entry must keep its pending bytes"
        );
        // The entry's own generation retires it as usual once its write lands.
        retire_pending_after_write(
            hash,
            super::cache_generation(),
            super::hash_epoch_for_new_job(hash),
        );
        assert!(!PENDING_IMAGE_DATA.lock().unwrap().contains_key(&hash));
    }

    #[test]
    fn a_discard_does_not_wait_for_an_in_progress_decode() {
        use super::{run_detail_preview_job, wipe_cache_for_discard, DetailPreviewJob};
        use std::os::unix::fs::OpenOptionsExt;
        use std::time::{Duration, Instant};
        // O_NONBLOCK: opening a FIFO for write fails with ENXIO instead of blocking when no reader is
        // there yet, which keeps this test's wait for the worker bounded (0x0004 on Darwin).
        const LIB_FCNTL_O_NONBLOCK: i32 = 0x0004;
        let _cache_guard = clip_cache_dir_guard();
        // Generating a detail preview reads (and decodes) its source, which can be slow, while the
        // discard runs on the main thread (switch off, quit, logout). This pins a worker inside its
        // generate step by making the source a FIFO with no writer -- the read blocks until this test
        // releases it -- and requires the discard to finish anyway. Holding the cache-write lock
        // across the read/decode would make the discard wait here instead of returning.
        let root = std::env::temp_dir().join(format!("oh-my-tab-slow-read-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // The FIFO lives outside the directory the discard wipes: the wipe unlinks every entry.
        let fifo_dir = root.join("source");
        let wipe_dir = root.join("cache");
        std::fs::create_dir_all(&fifo_dir).unwrap();
        std::fs::create_dir_all(&wipe_dir).unwrap();
        let fifo = fifo_dir.join("slow.pipe");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        assert!(
            made.map(|status| status.success()).unwrap_or(false),
            "mkfifo must be available"
        );
        let generation = super::cache_generation();
        let hash = fnv1a64(b"slow-read-job");
        let source = fifo.to_string_lossy().to_string();
        let worker = std::thread::spawn(move || {
            unsafe {
                run_detail_preview_job(&DetailPreviewJob {
                    hash,
                    source_path: Some(source),
                    deliver: false,
                    detail_visible: false,
                    selected_hash: None,
                    generation,
                    epoch: super::hash_epoch_for_new_job(hash),
                })
            };
        });
        // Wait for a READER on the FIFO instead of sleeping: opening for write without blocking
        // succeeds only once the worker is inside its read, which is the state this test needs. It
        // also cannot hang -- no reader means ENXIO, so the loop retries under a deadline.
        let started_at = Instant::now();
        let mut release = loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(LIB_FCNTL_O_NONBLOCK)
                .open(&fifo)
            {
                Ok(handle) => break handle,
                Err(error) => {
                    assert!(
                        started_at.elapsed() < Duration::from_secs(10),
                        "the worker never entered its read: {error}"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        };
        // The reader is pinned inside the read now; discarding from this thread must not wait for it.
        let (tx, rx) = std::sync::mpsc::channel();
        let wipe_target = wipe_dir.clone();
        std::thread::spawn(move || {
            wipe_cache_for_discard(&wipe_target);
            let _ = tx.send(());
        });
        let started = Instant::now();
        assert!(
            rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "the discard must not wait for an in-progress decode"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the discard took {:?}",
            started.elapsed()
        );
        // Release the reader through the handle opened above (it never blocks), and wait for the
        // worker to finish: its write must then be refused (stale generation).
        std::io::Write::write_all(&mut release, b"not an image").expect("release write");
        drop(release);
        let _ = worker.join();
        assert!(
            !clip_image_detail_path(hash).exists(),
            "a preview generated before the discard must not be cached"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_preview_finished_after_the_discard_never_reaches_the_slot() {
        use super::{cache_generation, offer_detail_preview, DETAIL_PENDING_HD};
        // The window the reviewer found: the worker finishes generating, the discard clears the slot,
        // and only then does the worker store its (now orphaned) preview. The store must be
        // generation-checked in the same lock as the check, so this ordering stores nothing.
        let _cache_guard = clip_cache_dir_guard();
        let hash = 0x1234_5678u64;
        *DETAIL_PENDING_HD.lock().unwrap() = None;
        let stale = cache_generation();
        // A directory of our own: the wipe deletes every direct child, so handing it a shared
        // directory would delete other tests' -- or another program's -- files.
        let dir = std::env::temp_dir().join(format!("oh-my-tab-slot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        super::wipe_cache_for_discard(&dir); // the discard wins the race
        assert!(
            !offer_detail_preview(stale, hash, 0, vec![1, 2, 3]),
            "a preview generated before the discard must not be stored"
        );
        assert!(
            DETAIL_PENDING_HD.lock().unwrap().is_none(),
            "the slot must stay empty for a discarded entry"
        );
        // And with the current generation the same store succeeds (the guard rejects stale work only).
        let current = cache_generation();
        assert!(offer_detail_preview(current, hash, 0, vec![4, 5, 6]));
        assert_eq!(
            DETAIL_PENDING_HD
                .lock()
                .unwrap()
                .as_ref()
                .map(|(h, g, e, b)| (*h, *g, *e, b.len())),
            Some((hash, current, 0, 3))
        );
        *DETAIL_PENDING_HD.lock().unwrap() = None;
    }

    #[test]
    fn a_job_queued_before_a_discard_cannot_recreate_cache_files() {
        let _cache_guard = clip_cache_dir_guard();
        // Recording an image hands its bytes to a background job. Discarding the history must
        // invalidate those jobs: one that was queued (or waiting on the write lock) before the
        // discard cannot put the original bytes back after the wipe returned.
        let hash = fnv1a64(b"stale-after-discard");
        let dir = std::env::temp_dir().join(format!("oh-my-tab-stale-{}", std::process::id()));
        let stale_generation = cache_generation();
        wipe_cache_for_discard(&dir);
        let job = |generation| ImageCacheJob {
            hash,
            data: Some(std::sync::Arc::new(b"stale-after-discard".to_vec())),
            preview: std::sync::Arc::new(b"stale-preview".to_vec()),
            source_path: None,
            warm_detail: false,
            generation,
            epoch: super::hash_epoch_for_new_job(hash),
        };
        run_image_cache_job(&job(stale_generation));
        assert!(
            !clip_image_path(hash).exists(),
            "a job from before the discard must not write"
        );
        // The same job with the current generation still works: the guard rejects stale work only.
        run_image_cache_job(&job(cache_generation()));
        assert!(
            clip_image_path(hash).exists(),
            "a current job must still write"
        );
        let _ = std::fs::remove_file(clip_image_path(hash));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn turning_the_feature_off_also_drops_the_undo_slot() {
        // A delete parks the entry in a 30s undo slot. Turning the feature off clears the history
        // and its files, so the slot must go too -- otherwise Cmd+Z restores the text and rewrites
        // the file that was just deleted.
        let _cache_guard = clip_cache_dir_guard();
        CLIP_HISTORY.lock().unwrap().clear();
        super::notifications::remember_deleted_clipboard_entry(entry("undo-me"), 0);
        super::clear_history_state();
        assert!(
            super::notifications::undo_deleted_clipboard_entry().is_none(),
            "a cleared history must not be restorable"
        );
        assert!(CLIP_HISTORY.lock().unwrap().is_empty());
        assert!(!super::history_file_exists());
    }

    #[test]
    fn discarding_the_history_deletes_the_file_and_the_image_cache() {
        let _cache_guard = clip_cache_dir_guard();
        use super::discard_history_in;
        // The switch-off and clear-on-quit paths both call this, and it must leave nothing behind:
        // no history file and no cached image bytes (the plaintext originals). It runs in a
        // directory of its own -- wiping the shared test cache would race every other test.
        let dir = std::env::temp_dir().join(format!("oh-my-tab-discard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let history = dir.join("clipboard-history.toml");
        let cache_dir = dir.join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        std::fs::write(&history, "version = 1\nentries = []\n").unwrap();
        std::fs::write(cache_dir.join("00000000000000ab"), b"image bytes").unwrap();
        std::fs::write(cache_dir.join("00000000000000ab.preview"), b"preview").unwrap();
        discard_history_in(&history, &cache_dir);
        assert!(!history.exists(), "the history file must go");
        assert!(
            std::fs::read_dir(&cache_dir).unwrap().next().is_none(),
            "every cached file must go"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_action_labels_name_the_visible_scope() {
        use super::{
            clip_clear_action_width, clip_clear_labels, ClipFilter, CLEAR_ACTION_CLICK_SLACK,
        };
        use crate::i18n::{t, tf};
        // No query: the plain clear names the category it keeps safe; All keeps the plain wording.
        assert_eq!(
            clip_clear_labels(ClipFilter::All, false),
            (
                t("clipboard.clear_button"),
                t("clipboard.clear_confirm_all")
            )
        );
        let kind = t("clipboard.filter_image");
        assert_eq!(
            clip_clear_labels(ClipFilter::Image, false),
            (
                t("clipboard.clear_button_category"),
                tf("clipboard.clear_scope_all", &[("kind", &kind)])
            )
        );
        // A query narrows the scope to the current view: the plain clear says "current" and the
        // opt-in clear-all says "all results". Both labels are fully substituted templates: an
        // unsubstituted template would show the raw "{kind}".
        for (label, _) in [
            clip_clear_labels(ClipFilter::Image, false),
            clip_clear_labels(ClipFilter::Image, true),
            clip_clear_labels(ClipFilter::All, true),
        ] {
            assert!(!label.is_empty() && !label.contains("{kind}"));
        }
        // The width hugs the CURRENT label: the measured text plus the shared click slack, so a
        // longer wording (a category name) makes the button wider and a shorter one shrinks it.
        let chars = |s: &str| s.chars().count() as f64;
        assert_eq!(
            clip_clear_action_width("清除", chars),
            2.0 + CLEAR_ACTION_CLICK_SLACK
        );
        assert_eq!(
            clip_clear_action_width("清除该分类", chars),
            5.0 + CLEAR_ACTION_CLICK_SLACK
        );
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
        let _cache_guard = clip_cache_dir_guard();
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
        let _cache_guard = clip_cache_dir_guard();
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
        // win (otherwise the history holds a static frame and its summon shortcut stops animating --
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
        // The empty-state minimum is at least a full window containing three same-group records,
        // including the first group's header, and respects the picker floor.
        let min_h = picker_min_height();
        assert!(
            min_h
                >= (header_strip_h()
                    + super::GROUP_H
                    + super::ROW_H * 3.0
                    + FOOTER_H
                    + super::PAD_Y)
                    .max(super::PICKER_MIN_HEIGHT)
        );
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
        // Tab cycles in the options popover's row order and wraps from the last item.
        assert_eq!(next_clip_filter(ClipFilter::All), ClipFilter::Text);
        assert_eq!(next_clip_filter(ClipFilter::Text), ClipFilter::Image);
        assert_eq!(next_clip_filter(ClipFilter::Image), ClipFilter::Link);
        assert_eq!(next_clip_filter(ClipFilter::Link), ClipFilter::Code);
        assert_eq!(next_clip_filter(ClipFilter::Code), ClipFilter::Pinned);
        assert_eq!(next_clip_filter(ClipFilter::Pinned), ClipFilter::All);
    }
    #[test]
    fn the_pinned_filter_matches_only_pinned_entries() {
        use super::{matches_filter, ClipFilter};
        let mut pinned = entry("pinned");
        pinned.pinned = true;
        assert!(matches_filter(&pinned, ClipFilter::Pinned));
        assert!(matches_filter(&pinned, ClipFilter::All));
        assert!(!matches_filter(&entry("plain"), ClipFilter::Pinned));
        let mut img = entry_image(b"png");
        img.pinned = true;
        assert!(matches_filter(&img, ClipFilter::Pinned));
    }
    #[test]
    fn the_popover_frame_anchors_under_the_options_orb() {
        use super::detail::{filter_popover_frame, top_offset_of};
        use super::{POPOVER_TOP, POPOVER_W, SEARCH_H};
        // The picker's content parent is a plain (bottom-up) NSView: the top-anchored offset the
        // mock states is converted into its origin, so the popover's top edge really sits
        // POPOVER_TOP below the panel's top.
        let parent_h = 600.0;
        let frame = filter_popover_frame(parent_h, false);
        let (_, _, options, _, _) = super::picker_header_frames();
        // The mock's promise is "under the options orb", and exact centring is what that clamp
        // now trades away: centring pushed the menu's right edge ~5pt past the panel's, where the
        // panel's own content clip sliced the corner and the glass edge. The promises that survive
        // are the orb's centre inside the menu's span and the whole menu inside the panel.
        let orb_centre = options.origin.x + options.size.width / 2.0;
        assert!(orb_centre >= frame.origin.x);
        assert!(orb_centre <= frame.origin.x + frame.size.width);
        assert_eq!(frame.size.width, POPOVER_W);
        assert!(frame.origin.x >= super::POPOVER_RIGHT_INSET);
        assert!(frame.origin.x + frame.size.width <= super::PICKER_W - super::POPOVER_RIGHT_INSET);
        assert_eq!(
            top_offset_of(frame.origin.y, parent_h, frame.size.height, false),
            POPOVER_TOP
        );
        // The offset is below the head and the popover stays inside the panel.
        assert!(
            top_offset_of(frame.origin.y, parent_h, frame.size.height, false)
                >= TOP_PAD_Y + SEARCH_H
        );
        assert!(frame.origin.y >= 0.0);
        assert!(frame.origin.y + frame.size.height <= parent_h);
        assert!(frame.origin.x >= 0.0);
        // A flipped parent (the convention the list container uses) needs no conversion at all.
        let flipped = filter_popover_frame(parent_h, true);
        assert_eq!(flipped.origin.y, POPOVER_TOP);
    }

    /// A short panel must still show the WHOLE menu: the picker shrinks to `picker_min_height`
    /// when the list is short, and a fixed 296pt menu hung below the anchor lost its housekeeping
    /// rows ("clear all", "settings…") off the panel's bottom edge. The rows compress just enough
    /// that the menu fits, and never below `POPOVER_MIN_ROW_H`.
    #[test]
    fn a_short_panel_compresses_the_menu_instead_of_clipping_it() {
        use super::detail::{
            filter_popover_frame, filter_popover_height, filter_popover_row_h, filter_popover_top,
            top_offset_of,
        };
        use super::{POPOVER_BOTTOM_INSET, POPOVER_MIN_ROW_H, POPOVER_ROW_H, POPOVER_TOP};
        let rows = (super::CLIP_FILTER_ROWS.len() + super::popover_tail_row_count()) as f64;
        // Roomy panel: the mock's row height and anchor, and the menu fits with room to spare.
        assert_eq!(filter_popover_row_h(1000.0), POPOVER_ROW_H);
        let tall_h = filter_popover_height(1000.0);
        assert_eq!(filter_popover_top(1000.0, tall_h), POPOVER_TOP);
        assert!(filter_popover_frame(1000.0, false).size.height == tall_h);
        // Minimum-height panel: compressed rows, and every row still inside the panel.
        let min_h = super::picker_min_height();
        let row_h = filter_popover_row_h(min_h);
        assert!(
            row_h < POPOVER_ROW_H,
            "a short panel must compress the rows"
        );
        assert!(
            row_h >= POPOVER_MIN_ROW_H,
            "rows must stay legible/clickable"
        );
        let h = filter_popover_height(min_h);
        let top = filter_popover_top(min_h, h);
        assert!(
            top + h <= min_h + 0.001,
            "the whole menu must fit the panel it is anchored to (top={top}, h={h}, panel={min_h})"
        );
        assert_eq!(h, super::detail::filter_popover_fixed_h() + rows * row_h);
        let frame = filter_popover_frame(min_h, false);
        assert_eq!(frame.size.height, h);
        assert_eq!(
            top_offset_of(frame.origin.y, min_h, frame.size.height, false),
            top
        );
        // Bottom margin is respected unless the panel is shorter than the minimum row height
        // allows; a degenerate panel pins the menu to the top rather than starting above it.
        assert!(top + h + POPOVER_BOTTOM_INSET >= min_h - 0.001);
        assert_eq!(filter_popover_top(10.0, h), 0.0);
    }

    /// The shared opaque reference surface keeps readable inks and a distinct selected row.
    /// Its border and shadow, rather than a fill contrast threshold, separate it from the picker.
    #[test]
    fn the_menu_surface_and_its_inks_keep_the_palette_floors() {
        use super::{
            popover_check_color, popover_danger, popover_muted, popover_surface,
            PICKER_POPOVER_SURFACE_LIGHT,
        };
        use crate::theme::{
            color_rgb, composite_over, contrast_on, contrast_ratio, ui_palette_for_mode,
        };
        let light = ui_palette_for_mode(false);
        let dark = ui_palette_for_mode(true);
        assert_eq!(popover_surface(light), PICKER_POPOVER_SURFACE_LIGHT);
        assert_eq!(popover_surface(dark), dark.window_bg);
        // Ink on the fill: hard text floors, plus the active row's accent wash on top.
        let surface = color_rgb(PICKER_POPOVER_SURFACE_LIGHT);
        let active = composite_over(
            super::picker_popover_active_bg(light),
            PICKER_POPOVER_SURFACE_LIGHT,
        );
        for (ink, floor, name) in [
            (light.primary_text, 4.5, "primary_text"),
            (popover_muted(light), 4.5, "popover_muted"),
            (popover_danger(light), 4.5, "popover_danger"),
        ] {
            for (bg, where_) in [(surface, "the fill"), (active, "the active row")] {
                let ratio = contrast_on(ink, bg);
                assert!(
                    ratio >= floor,
                    "{name} measures {ratio:.2}:1 on {where_} (min {floor}:1)"
                );
            }
        }
        let check = contrast_on(popover_check_color(light), active);
        assert!(
            check >= 4.5,
            "the check measures {check:.2}:1 on the active row (min 4.5:1: it carries the state)"
        );
        // A selected row is a state, not decoration, so the wash itself has to be perceivable
        // against the fill it lies on: the sheet's structural floor is 1.2:1.
        let wash = contrast_ratio(active, surface);
        assert!(
            wash >= 1.2,
            "the active row's wash measures {wash:.2}:1 against the menu fill (min 1.2:1)"
        );
    }

    /// The layer's 1pt rim is drawn inward, so the rows sit `POPOVER_BORDER_W` inside
    /// `POPOVER_PAD`: a row drawn at the nominal inset would tuck its wash under the rim, and the
    /// two left edges would read as a 2pt step. Pinned so a rim-width change cannot desync them.
    #[test]
    fn menu_rows_clear_the_rim_inward() {
        use super::{popover_row_span, POPOVER_BORDER_W, POPOVER_PAD, POPOVER_W};
        let (x, w) = popover_row_span();
        assert!(POPOVER_BORDER_W > 0.0);
        assert_eq!(x, POPOVER_PAD - POPOVER_BORDER_W);
        // Symmetric about the menu's centre: the rim eats the same 1pt on both sides.
        assert!((x - (POPOVER_W - x - w)).abs() < 1e-9);
        assert!(x > 0.0, "a row must not start outside the menu");
    }

    /// One conversion for top-down layout: placing a child at a top offset and reading that offset
    /// back must agree in both coordinate conventions, or a popover's rows would run upwards (and
    /// its lowest rows could fall outside a short panel).
    #[test]
    fn a_top_anchored_offset_round_trips_in_both_conventions() {
        use super::detail::{top_anchored_origin_y, top_offset_of};
        for flipped in [false, true] {
            for (container_h, child_h, top) in [
                (600.0, 334.0, 60.0),
                (334.0, 334.0, 60.0),
                (300.0, 100.0, 0.0),
            ] {
                let origin = top_anchored_origin_y(top, container_h, child_h, flipped);
                assert_eq!(top_offset_of(origin, container_h, child_h, flipped), top);
            }
        }
        // Bottom-up: the child's origin is measured from the container's bottom edge, so a
        // top-anchored 60pt child in a 600pt parent starts at 600 - 60 - 334.
        assert_eq!(top_anchored_origin_y(60.0, 600.0, 334.0, false), 206.0);
        assert_eq!(top_anchored_origin_y(60.0, 600.0, 334.0, true), 60.0);
    }

    /// A row's views are all owned by one list, built by an exhaustive destructure: adding a view
    /// without adding it there does not compile, which is what keeps a per-row button (the group
    /// clear) from outliving its row.
    #[test]
    fn a_row_owns_every_view_it_creates() {
        use super::RowHoverViews;
        let sentinel = |n: usize| super::ObjPtr::new(n as *mut objc2::runtime::AnyObject);
        let view = RowHoverViews {
            group_label: Some(sentinel(1)),
            group_clear: Some(sentinel(2)),
            tile: sentinel(3),
            content: sentinel(4),
            detail: sentinel(5),
            pin: sentinel(6),
            del: sentinel(7),
        };
        let owned: Vec<usize> = view
            .owned()
            .into_iter()
            .flatten()
            .map(|ptr| ptr.0 as usize)
            .collect();
        assert_eq!(owned, vec![1, 2, 3, 4, 5, 6, 7]);
    }
    #[test]
    fn the_options_popover_rows_match_the_filter_order() {
        use super::{localized_filter_labels, next_clip_filter, CLIP_FILTER_ROWS};
        assert_eq!(CLIP_FILTER_ROWS.len(), localized_filter_labels().len());
        // Every row's filter is reachable from All by walking the cycle.
        let mut seen = vec![super::ClipFilter::All];
        for _ in 1..CLIP_FILTER_ROWS.len() {
            seen.push(next_clip_filter(*seen.last().unwrap()));
        }
        assert_eq!(seen, CLIP_FILTER_ROWS);
    }
    /// The active-row check sits on both accent washes: keep its composite contrast over the light menu surface
    /// at the 4.5:1 text floor so a future token change cannot quietly sink it. The check
    /// says which filter is active -- it is text, not a decorative glyph, and the old
    /// 3.5:1 ceiling let it sit at 3.76:1 on the previous menu fill.
    #[test]
    fn popover_check_keeps_the_contrast_floor_on_both_washes() {
        use super::{
            picker_popover_active_bg, picker_popover_active_hover_bg, popover_check_color,
            PICKER_POPOVER_SURFACE_LIGHT,
        };
        use crate::theme::{color_rgb, composite_over, contrast_ratio};
        let palette = crate::theme::ui_palette_for_mode(false);
        let check = popover_check_color(palette);
        // The same token `build_filter_popover` paints the light surface with.
        let surface = PICKER_POPOVER_SURFACE_LIGHT;
        for (wash, name) in [
            (picker_popover_active_bg(palette), "is-active"),
            (picker_popover_active_hover_bg(palette), "is-active:hover"),
        ] {
            let bg = composite_over(wash, surface);
            let ratio = contrast_ratio(color_rgb(check), bg);
            assert!(
                ratio >= 4.5,
                "check on the {name} wash measures {ratio:.2}:1 (min 4.5:1)"
            );
        }
    }
    #[test]
    fn image_row_pitches_follow_the_configured_multiplier_and_the_budget_cap() {
        use super::{compute_pitches_for, list_fill_for_budget, row_content_h_for, GROUP_H, ROW_H};
        // The multiplier is an input, not a global read: a 3x image row is three text rows
        // while a text row is unaffected, whatever config happens to say.
        assert_eq!(row_content_h_for(&entry_image(b"png"), 3.0), ROW_H * 3.0);
        assert_eq!(row_content_h_for(&entry("a"), 3.0), ROW_H);
        // Once a filter drops the text row in front, display index 0 carries the image's
        // pitch -- GROUP_H + an image row, not GROUP_H + ROW_H.
        let texts = vec![entry("a"), entry_image(b"png")];
        let filtered: Vec<&super::ClipEntry> = texts.iter().skip(1).collect();
        let pitches = compute_pitches_for(filtered.iter().copied());
        let n = crate::clipboard::model::image_row_multiplier();
        assert_eq!(pitches[0], GROUP_H + ROW_H * n);
        // Three 3x image rows do not fit a four-text-row budget: only one is kept whole.
        let (count, used) = list_fill_for_budget(&[ROW_H * 3.0; 3], ROW_H * 4.0);
        assert_eq!((count, used), (1, ROW_H * 3.0));
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
        let _cache_guard = clip_cache_dir_guard();
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
        // Text and image rows share the uniform 40pt pitch -- equal after removing the
        // header.
        let mixed = vec![entry("a"), entry_image(b"png")];
        let p2 = compute_pitches(&mixed);
        assert_eq!(p2[1], p2[0] - GROUP_H);
        // Sanity-check the pitch constant (guards regressions): the mock's 40px row.
        const { assert!((ROW_H - 40.0).abs() < f64::EPSILON) };
    }

    /// The row backdrop wash must resolve per the documented precedence: the selection owns
    /// the backdrop, so selected-beats-hovered. One row is both selected and hovered whenever
    /// arrow navigation repaints the new selection (refresh_selection passes it as both the
    /// old-hover target and the new selection) and whenever the pointer rests on the selected
    /// row. The wash helper is where both paint paths (row build and runtime repaint) meet,
    /// so asserting all four combinations there pins the rule for both.
    #[test]
    fn row_backdrop_wash_selection_beats_hover() {
        use super::detail::row_backdrop_wash;
        for dark in [false, true] {
            let palette = crate::theme::ui_palette_for_mode(dark);
            let selected = super::picker_row_selected(palette);
            let hovered = super::picker_row_hover(palette);
            assert_eq!(
                row_backdrop_wash(palette, true, true),
                selected,
                "selected+hovered must keep the selection wash ({} mode)",
                if dark { "dark" } else { "light" }
            );
            assert_eq!(row_backdrop_wash(palette, true, false), selected);
            assert_eq!(row_backdrop_wash(palette, false, true), hovered);
            // An idle row paints nothing: the panel's material is the surface, and a per-row surface would
            // band against it wherever a row does not cover the panel (see `glass::panel_ink`).
            assert_eq!(
                row_backdrop_wash(palette, false, false),
                0x00000000,
                "an idle row must be bare material ({} mode)",
                if dark { "dark" } else { "light" }
            );
            if dark {
                // Dark mode keeps the measured shared palette.
                assert_eq!(selected, palette.selection_bg);
                assert_eq!(hovered, palette.hover_bg);
            } else {
                // Light mode follows the v2 mock and deliberately diverges from the shared palette
                // until those tokens are promoted into `UiPalette`.
                assert_eq!(selected, super::PICKER_ROW_SELECTED_LIGHT);
                assert_eq!(hovered, super::PICKER_ROW_HOVER_LIGHT);
                assert_ne!(
                    selected, palette.selection_bg,
                    "the light row wash is the mock's warm gray, not the shared blue selection"
                );
            }
        }
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
    /// The meta table row order and values, as the mock's `renderDetail()` builds them:
    /// 来源 / 类型 / 尺寸 / 大小 / 复制时间, with an em dash wherever the entry has nothing.
    #[test]
    fn meta_table_follows_the_mock_row_order_and_values() {
        use super::{entry_kind_label, meta_table, MetaField};
        assert_eq!(
            MetaField::ALL.map(MetaField::label_key),
            [
                "clipboard.meta_source",
                "clipboard.meta_type",
                "clipboard.meta_dimensions",
                "clipboard.meta_size",
                "clipboard.meta_copied_at",
            ]
        );
        let mut e = entry_with_source("hello", "Safari");
        e.copied_at = Some(1_755_000_000);
        let rows = meta_table(&e, true);
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].1, "Safari");
        assert_eq!(rows[1].1, entry_kind_label(&e));
        // Text has no pixel size, and this entry has no image.
        assert_eq!(rows[2].1, "\u{2014}");
        // 5 UTF-8 bytes of text: the mock's `sizeText` counts the bytes, not the characters.
        assert_eq!(
            rows[3].1,
            super::tf("clipboard.meta_size_bytes", &[("count", "5")])
        );
        assert_eq!(rows[4].1, super::copied_at_label(1_755_000_000));
        // Labels are localized, never the raw key.
        assert!(rows
            .iter()
            .all(|(label, _)| !label.starts_with("clipboard.")));
        // The "show source app" setting off hides the name and the em dash takes its place.
        let hidden = meta_table(&e, false);
        assert_eq!(hidden[0].1, "\u{2014}");
        // A legacy entry without a timestamp shows the dash instead of a wrong time.
        let legacy = entry_with_source("t", "Safari");
        assert_eq!(meta_table(&legacy, true)[4].1, "\u{2014}");
        // Code and links are named by their kind, not as plain text.
        assert_eq!(
            entry_kind_label(&entry("fn main() {}\n    let x = 1;")),
            super::t("clipboard.filter_code")
        );
        assert_eq!(
            entry_kind_label(&entry("https://example.com")),
            super::t("clipboard.filter_link")
        );
    }

    /// The size row counts the bytes of the payload (UTF-8 for text, the image's own bytes for an
    /// image) in the mock's `sizeText` units: whole bytes below 1000, then one decimal of KB and
    /// MB. The divisor is decimal because the printed unit is ("1000 bytes" is "1.0 KB", never
    /// "0.98 KB" or "1.0 KiB").
    #[test]
    fn format_byte_size_uses_the_mocks_decimal_units() {
        use super::format_byte_size;
        assert_eq!(
            format_byte_size(48),
            super::tf("clipboard.meta_size_bytes", &[("count", "48")])
        );
        assert_eq!(
            format_byte_size(999),
            super::tf("clipboard.meta_size_bytes", &[("count", "999")])
        );
        assert_eq!(
            format_byte_size(1000),
            super::tf("clipboard.meta_size_kb", &[("size", "1.0")])
        );
        // The unit switches on the raw count (the mock's `sizeText`): at or above 1 MB the row
        // reads in MB, and just below it stays in KB even when the number reaches four digits.
        assert_eq!(
            format_byte_size(411_648),
            super::tf("clipboard.meta_size_kb", &[("size", "411.6")])
        );
        assert_eq!(
            format_byte_size(1_433_600),
            super::tf("clipboard.meta_size_mb", &[("size", "1.4")])
        );
        assert_eq!(
            format_byte_size(999_999),
            super::tf("clipboard.meta_size_kb", &[("size", "1000.0")])
        );
        assert_eq!(
            format_byte_size(1_000_000),
            super::tf("clipboard.meta_size_mb", &[("size", "1.0")])
        );
        assert_eq!(
            format_byte_size(5_242_880),
            super::tf("clipboard.meta_size_mb", &[("size", "5.2")])
        );
        // A multi-byte character is more than one byte: the row must not count characters.
        let e = entry("长");
        assert_eq!(super::entry_payload_bytes(&e), 3);
    }

    /// Screen availability selects the HTML desktop split or narrow replacement.
    #[test]
    fn detail_pane_mode_switch_is_the_mocks_breakpoint() {
        assert!(super::detail_expands_at_width(840.0));
        assert!(!super::detail_expands_at_width(841.0));
        let desktop = super::picker_layout(1440.0, true);
        assert_eq!(desktop.panel_w, 960.0);
        assert_eq!(desktop.list_w, 340.0);
        assert_eq!(desktop.list_w + 1.0 + desktop.detail_w, desktop.panel_w);
        let constrained = super::picker_layout(900.0, true);
        assert!(constrained.split);
        assert_eq!(constrained.panel_w, 884.0);
        let narrow = super::picker_layout(600.0, true);
        assert!(!narrow.split);
        assert_eq!(narrow.panel_w, 584.0);
        assert_eq!(narrow.detail_w, narrow.panel_w);
        let collapsed = super::picker_layout(1440.0, false);
        assert!(!collapsed.split);
        assert_eq!(collapsed.panel_w, 640.0);
    }
    /// The body rect is what the list and the in-pane detail take turns occupying: under the header
    /// strip, above the footer, the full width.
    #[test]
    fn detail_body_rect_is_the_picker_body() {
        use super::{detail_content_frame, header_strip_h, FOOTER_H};
        use objc2_foundation::NSSize;
        let panel = NSSize::new(560.0, 584.0);
        let frame = detail_content_frame(panel);
        assert_eq!(frame.origin.x, 0.0);
        assert_eq!(frame.origin.y, FOOTER_H);
        assert_eq!(frame.size.width, panel.width);
        assert_eq!(
            frame.size.height,
            panel.height - header_strip_h() - FOOTER_H
        );
    }
    /// The "尺寸" row comes from the PNG's own header, so it needs no AppKit: the crash that
    /// made this a header read was an NSImage rep geometry proxy that had no `pixelsWide`.
    #[test]
    fn png_pixel_size_reads_the_ihdr() {
        use super::png_pixel_size;
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&1440u32.to_be_bytes());
        png.extend_from_slice(&900u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        assert_eq!(png_pixel_size(&png), Some((1440, 900)));
        // Anything that is not a PNG header (a JPEG, a truncated buffer, a zero dimension) has
        // no answer here, and the caller then shows the em dash rather than guessing.
        assert_eq!(png_pixel_size(b"\xFF\xD8\xFF\xE0 not a png at all"), None);
        assert_eq!(png_pixel_size(&png[..16]), None);
        let mut zero = png.clone();
        zero[16..20].copy_from_slice(&0u32.to_be_bytes());
        assert_eq!(png_pixel_size(&zero), None);
    }

    /// The copy time in the detail pane carries the year, and it is written in one localized
    /// phrase whose date units run largest to smallest (the mock's "2026年10月19日 08:56"), so a
    /// copy from another year cannot read as this one.
    #[test]
    fn copied_at_label_is_an_ordered_date_and_time() {
        let s = super::copied_at_label(1_755_000_000);
        let shape = super::tf(
            "clipboard.meta_copied_at_value",
            &[
                ("year", "<y>"),
                ("month", "<m>"),
                ("day", "<d>"),
                ("time", "<t>"),
            ],
        );
        let at = |field: &str| {
            shape
                .find(field)
                .unwrap_or_else(|| panic!("missing {field} in {shape}"))
        };
        assert!(at("<y>") < at("<m>"), "got {shape}");
        assert!(at("<m>") < at("<d>"), "got {shape}");
        assert!(at("<d>") < at("<t>"), "got {shape}");
        // Filling the template with the timestamp the value was rendered from must reproduce it
        // exactly: the shape check above then holds for the rendered text too.
        let (year, month, day, hour, minute) = {
            let mut tm: crate::ffi::Tm = unsafe { std::mem::zeroed() };
            let secs: i64 = 1_755_000_000;
            unsafe { crate::ffi::localtime_r(&secs, &mut tm) };
            (
                tm.tm_year + 1900,
                tm.tm_mon + 1,
                tm.tm_mday,
                tm.tm_hour,
                tm.tm_min,
            )
        };
        let filled = shape
            .replace("<y>", &year.to_string())
            .replace("<m>", &month.to_string())
            .replace("<d>", &day.to_string())
            .replace("<t>", &format!("{hour:02}:{minute:02}"));
        assert_eq!(filled, s);
        assert!(s.starts_with(&year.to_string()), "got {s}");
        assert_eq!(s.matches(':').count(), 1, "got {s}");
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
    /// The picker opens on the display the pointer is on, on a two-display desk.
    ///
    /// The regression this holds shut: every summon landed on the primary display whatever display
    /// the pointer was on. The subtler half is the display-membership rule: it was decided from each
    /// display's VISIBLE frame, so a pointer on a secondary display's menu bar or Dock (inside the
    /// display, outside its visible area) fell back to the primary display too.
    #[test]
    fn picker_host_screen_follows_the_cursor() {
        use super::picker_host_screen;
        use super::DisplayArea;
        use objc2_foundation::{NSPoint, NSRect, NSSize};
        let rect =
            |x: f64, y: f64, w: f64, h: f64| NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));

        // Primary at (0,0) 1440x900 with a 25pt menu bar, so its visible area starts at y=25.
        let primary = DisplayArea {
            frame: rect(0.0, 0.0, 1440.0, 900.0),
            visible: rect(0.0, 0.0, 1440.0, 875.0),
        };
        // Secondary to the LEFT at (-800,0) 800x900, with its menu bar in the SAME global band.
        let left = DisplayArea {
            frame: rect(-800.0, 0.0, 800.0, 900.0),
            visible: rect(-800.0, 0.0, 800.0, 875.0),
        };
        let side_by_side = [primary, left];
        let primary_visible = primary.visible;

        // On the secondary display the picker goes there (a negative x origin).
        assert_eq!(
            picker_host_screen(NSPoint::new(-400.0, 200.0), &side_by_side, primary_visible),
            left.visible,
            "the picker must open on the display the pointer is on"
        );
        // On the primary display it goes there.
        assert_eq!(
            picker_host_screen(NSPoint::new(700.0, 500.0), &side_by_side, primary_visible),
            primary_visible
        );
        // The pointer on the SECONDARY display's menu bar is inside that display and outside its
        // visible frame: it must still be reported as that display.
        assert_eq!(
            picker_host_screen(NSPoint::new(-400.0, 890.0), &side_by_side, primary_visible),
            left.visible,
            "a pointer on a secondary display's menu bar belongs to that display, not the primary"
        );
        // The primary display's own menu bar maps to the primary, and the placement derived from it
        // is clamped inside the visible frame, never under the menu bar.
        assert_eq!(
            picker_host_screen(NSPoint::new(700.0, 890.0), &side_by_side, primary_visible),
            primary_visible
        );

        // Vertically stacked displays share every x: only the y tells them apart.
        let above = DisplayArea {
            frame: rect(0.0, 900.0, 1440.0, 600.0),
            visible: rect(0.0, 900.0, 1440.0, 575.0),
        };
        let stacked = [primary, above];
        assert_eq!(
            picker_host_screen(NSPoint::new(700.0, 1200.0), &stacked, primary_visible),
            above.visible,
            "a pointer on the display above must select that display"
        );
        assert_eq!(
            picker_host_screen(NSPoint::new(700.0, 450.0), &stacked, primary_visible),
            primary_visible
        );

        // A pointer on no display at all (a stale location during a display reconfiguration) falls
        // back to the primary display instead of leaving the panel stranded.
        assert_eq!(
            picker_host_screen(NSPoint::new(5000.0, 5000.0), &side_by_side, primary_visible),
            primary_visible
        );
        // No displays reported at all: the primary is the only answer there is.
        assert_eq!(
            picker_host_screen(NSPoint::new(-400.0, 200.0), &[], primary_visible),
            primary_visible
        );
    }
    /// The two `clipboard.picker_placement` modes, on the screen the pointer is on.
    ///
    /// The screen never differs between them -- `configured_picker_area` supplies the pointer's
    /// display either way -- so what is asserted here is placement WITHIN that screen: "center" puts
    /// the panel in the middle of the visible area (never under the menu bar), and "cursor" keeps the
    /// pointer-relative offset with its edge flips. A panel larger than the screen is clamped to the
    /// screen's origin rather than pushed off it.
    #[test]
    fn picker_placement_centers_or_follows_the_cursor_within_one_screen() {
        use super::{picker_frame_centered, picker_frame_for};
        use objc2_foundation::{NSPoint, NSRect, NSSize};

        // A secondary display's visible area: its menu bar is carved out of the top.
        let screen = NSRect::new(NSPoint::new(-1920.0, 0.0), NSSize::new(1920.0, 1055.0));
        let size = NSSize::new(560.0, 400.0);

        let centered = picker_frame_centered(screen, size);
        assert_eq!(centered.size, size);
        assert_eq!(
            centered.origin.x,
            screen.origin.x + (screen.size.width - size.width) / 2.0
        );
        assert_eq!(
            centered.origin.y,
            screen.origin.y + (screen.size.height - size.height) / 2.0
        );
        // It sits on the pointer's screen, not the primary one.
        assert!(centered.origin.x < 0.0, "centered on the secondary display");
        // And inside the visible area: the menu bar cannot cut it off.
        assert!(centered.origin.y >= screen.origin.y);
        assert!(centered.origin.y + size.height <= screen.origin.y + screen.size.height);

        // "cursor" is the pointer-relative placement, unchanged.
        let cursor = NSPoint::new(-1500.0, 500.0);
        let followed = picker_frame_for(cursor, screen, size.width, size.height);
        assert_ne!(
            followed.origin, centered.origin,
            "the two modes must not produce the same origin here, or this proves nothing"
        );
        assert!(followed.origin.x >= screen.origin.x);

        // A panel wider than the screen is clamped to the screen's origin, not pushed off it.
        let narrow = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(500.0, 400.0));
        let huge = picker_frame_centered(narrow, NSSize::new(900.0, 700.0));
        assert_eq!(huge.origin, NSPoint::new(0.0, 0.0));

        // The counter-example the A1 runner depends on: the two modes CAN legitimately coincide, so
        // "the origins differ" is not an assertion about the mode. A pointer up and left of the
        // centred panel by exactly the cursor offset produces the centred frame from the
        // pointer-relative placement. The runner therefore checks each mode against its own expected
        // origin; this pins the case where a difference-based check would have failed a correct build.
        let coincident_cursor = NSPoint::new(
            centered.origin.x - super::PICKER_CURSOR_OFF,
            centered.origin.y + size.height + super::PICKER_CURSOR_OFF,
        );
        let coincident = picker_frame_for(coincident_cursor, screen, size.width, size.height);
        assert_eq!(
            coincident.origin, centered.origin,
            "the fixture must actually coincide, or it does not test the counter-example"
        );
        // ...and the summon's own derivation is the bare one composed with the shadow padding, in
        // both modes: the coincidence above is a property of the bare derivations, and the summon
        // deliberately places inside a padded area (`the_summon_placement_composes_the_shadow_padding_per_mode`).
        let pad = crate::glass::panel_elevation_padding();
        let placeable = NSRect::new(
            NSPoint::new(screen.origin.x + pad.left, screen.origin.y + pad.bottom),
            NSSize::new(
                screen.size.width - pad.left - pad.right,
                screen.size.height - pad.bottom - pad.top,
            ),
        );
        assert_eq!(
            super::configured_picker_frame(false, coincident_cursor, screen, size).origin,
            picker_frame_for(coincident_cursor, placeable, size.width, size.height).origin
        );
        assert_eq!(
            super::configured_picker_frame(true, coincident_cursor, screen, size).origin,
            picker_frame_centered(placeable, size).origin
        );
        // MODE -> IMPLEMENTATION, pinned explicitly. Every other assertion here reads the shared
        // `picker_placement_expected_origin`, which is only proved correct if something says which
        // branch each mode must take: with the two branches of `configured_picker_frame` swapped, the
        // coincident case still passes and a "the two differ" check still passes, because both the
        // production path and the expectation moved together. These two comparisons are against the
        // two implementations directly, so a swap fails here.
        let elsewhere = NSPoint::new(screen.origin.x + 200.0, screen.origin.y + 800.0);
        assert_eq!(
            super::configured_picker_frame(false, elsewhere, screen, size).origin,
            picker_frame_for(elsewhere, placeable, size.width, size.height).origin,
            "'cursor' must be the pointer-relative placement, inside the padded area"
        );
        assert_eq!(
            super::configured_picker_frame(true, elsewhere, screen, size).origin,
            picker_frame_centered(placeable, size).origin,
            "'center' must be the centred placement, inside the padded area"
        );
        // ...and the two are actually different answers at this pointer, so the pair above is a
        // discriminating fixture rather than two names for one value.
        assert_ne!(
            picker_frame_for(elsewhere, screen, size.width, size.height).origin,
            picker_frame_centered(screen, size).origin,
            "the fixture needs a pointer where the modes differ"
        );
    }

    /// The summon's placement is one derivation per mode, and the padding it composes is the shadow's.
    ///
    /// Both modes are exercised from fixed inputs, never from the user's configuration: reading
    /// `picker_placement_centers()` here made a `center` configuration fail the cursor half. The
    /// comparison is against the mode's own derivation run on the PLACEABLE area, which is what
    /// keeps the padded window inside the display (`picker_placement_area`); dropping that
    /// composition is what let AppKit move a placed panel.
    #[test]
    fn the_summon_placement_composes_the_shadow_padding_per_mode() {
        use super::{configured_picker_frame, picker_frame_centered, picker_frame_for};
        use objc2_foundation::{NSPoint, NSRect, NSSize};
        let screen = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1470.0, 923.0));
        let size = NSSize::new(640.0, 544.0);
        let cursor = NSPoint::new(717.0, 372.0);
        let pad = crate::glass::panel_elevation_padding();
        let placeable = NSRect::new(
            NSPoint::new(screen.origin.x + pad.left, screen.origin.y + pad.bottom),
            NSSize::new(
                screen.size.width - pad.left - pad.right,
                screen.size.height - pad.bottom - pad.top,
            ),
        );
        // `false` = cursor placement, `true` = screen centre; the flags, not the config.
        assert_eq!(
            configured_picker_frame(false, cursor, screen, size),
            picker_frame_for(cursor, placeable, size.width, size.height),
            "the cursor placement is `picker_frame_for` on the padded area"
        );
        assert_eq!(
            configured_picker_frame(true, cursor, screen, size),
            picker_frame_centered(placeable, size),
            "the centre placement ignores the pointer and centres in the padded area"
        );
        // The padding is not a no-op on this display, so the comparison above is not a tautology.
        assert_ne!(
            picker_frame_for(cursor, screen, size.width, size.height),
            picker_frame_for(cursor, placeable, size.width, size.height),
            "the fixture must discriminate, or the equality above proves nothing"
        );
        // A composed placement keeps the padded WINDOW inside the visible frame: that is the whole
        // point, and the counter-example is the bare-area placement, which does not.
        let window = crate::theme::window_frame_for_panel(
            configured_picker_frame(false, cursor, screen, size),
            pad,
        );
        assert!(window.origin.y >= screen.origin.y);
        assert!(window.origin.y + window.size.height <= screen.origin.y + screen.size.height);
        let overflows = crate::theme::window_frame_for_panel(
            picker_frame_for(cursor, screen, size.width, size.height),
            pad,
        );
        assert_ne!(
            overflows.origin.y + overflows.size.height <= screen.size.height,
            true,
            "this cursor must overflow an unpadded placement, or the check proves nothing"
        );
    }
    /// A panel that exactly fits its placeable area is placed at the edge, not past it.
    ///
    /// Counter-example this is built to fail on: the clamp used to apply its 8pt edge margin AFTER
    /// the interval, so the reviewer's fixture (visible 768 wide, 64pt shadow each side, panel 640)
    /// placed the panel at x=56 and the padded window at x=-8 -- the panel fit and the window did
    /// not, and AppKit moved it back.
    #[test]
    fn a_panel_that_exactly_fits_is_placed_at_the_edge() {
        use super::{picker_frame_for, PICKER_EDGE_MARGIN};
        use objc2_foundation::{NSPoint, NSRect, NSSize};
        let placeable = NSRect::new(NSPoint::new(64.0, 64.0), NSSize::new(640.0, 400.0));
        let size = NSSize::new(640.0, 400.0);
        for cursor in [
            NSPoint::new(400.0, 300.0),
            NSPoint::new(0.0, 0.0),
            NSPoint::new(2000.0, 2000.0),
        ] {
            let frame = picker_frame_for(cursor, placeable, size.width, size.height);
            assert!(
                frame.origin.x >= placeable.origin.x - 0.001
                    && frame.origin.x + frame.size.width
                        <= placeable.origin.x + placeable.size.width + 0.001
                    && frame.origin.y >= placeable.origin.y - 0.001
                    && frame.origin.y + frame.size.height
                        <= placeable.origin.y + placeable.size.height + 0.001,
                "a fitting panel ({cursor:?}) must stay inside {placeable:?}, got {frame:?}"
            );
            assert_eq!(
                frame.origin, placeable.origin,
                "it can only sit at the edge"
            );
        }
        // With room to spare the margin is still preferred: a panel that cannot sit beside the
        // pointer lands 8pt inside the edge it had to use.
        let roomy = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1470.0, 923.0));
        // A pointer on the roomy area's far right with vertical room below it: the flip and the
        // clamp both bite, and each keeps its own margin.
        let at_edge = picker_frame_for(NSPoint::new(1050.0, 700.0), roomy, 400.0, 300.0);
        assert_eq!(
            at_edge.origin.x + 400.0,
            roomy.size.width - PICKER_EDGE_MARGIN,
            "the panel stops one margin inside the right edge"
        );
        // Vertically the pointer had room, so the panel sits under it at the cursor offset.
        assert_eq!(at_edge.origin.y, 700.0 - 300.0 - super::PICKER_CURSOR_OFF);
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
        let (picker_frame, detail_frame) = detail_group_frames(
            picker,
            820.0,
            screen,
            NSSize::new(720.0, 400.0),
            None,
            true,
            960.0,
        );
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
        let (picker_frame, detail_frame) = detail_group_frames(
            picker,
            600.0,
            screen,
            NSSize::new(720.0, 300.0),
            None,
            true,
            500.0,
        );
        assert_eq!(picker_frame.origin.x, PICKER_EDGE_MARGIN);
        assert!(detail_frame.origin.x >= screen.origin.x + PICKER_EDGE_MARGIN);
        assert!(
            detail_frame.origin.x + detail_frame.size.width
                <= screen.origin.x + screen.size.width - PICKER_EDGE_MARGIN
        );
    }
    /// A dragged panel keeps its x when the detail opens: the group layout must start from the anchor it
    /// is given, not from the mode-derived x. This is the regression the review found -- the code took the
    /// anchor but only used its y, so opening the detail snapped a dragged panel back to the middle.
    #[test]
    fn detail_group_starts_from_the_dragged_anchor_when_there_is_one() {
        use super::{detail_group_frames, DETAIL_GAP, PICKER_EDGE_MARGIN, PICKER_W};
        use objc2_foundation::{NSPoint, NSRect, NSSize};
        let screen = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1920.0, 1080.0));
        let anchored_x = 120.0;
        let picker = NSRect::new(
            NSPoint::new(anchored_x, 300.0),
            NSSize::new(PICKER_W, 600.0),
        );
        let (picker_frame, detail_frame) = detail_group_frames(
            picker,
            820.0,
            screen,
            NSSize::new(720.0, 400.0),
            Some(anchored_x),
            true,
            960.0,
        );
        // The anchor fits on this screen, so nothing corrects it and the group hangs off it.
        assert_eq!(picker_frame.origin.x, anchored_x);
        assert_eq!(
            detail_frame.origin.x,
            anchored_x + PICKER_W + DETAIL_GAP,
            "the detail stays to the right of the anchor"
        );
        // An anchor that would push the group off the right edge is still corrected as a group.
        let far_picker = NSRect::new(NSPoint::new(1700.0, 300.0), NSSize::new(PICKER_W, 600.0));
        let (corrected, corrected_detail) = detail_group_frames(
            far_picker,
            820.0,
            screen,
            NSSize::new(720.0, 400.0),
            Some(1700.0),
            true,
            960.0,
        );
        assert!(corrected.origin.x < 1700.0);
        assert_eq!(
            corrected_detail.origin.x + corrected_detail.size.width,
            1920.0 - PICKER_EDGE_MARGIN
        );
        assert!(corrected.origin.x >= PICKER_EDGE_MARGIN);
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
    /// The sizing helpers report the preview BODY's height; the panel adds the meta table and the
    /// pane padding around it (the mock's detail pane is one column, body above table below).
    #[test]
    fn detail_text_size_reports_body_height_capped_by_the_pane() {
        use super::TextKind;
        use crate::clipboard::{
            detail_max_height, detail_pane_layout, detail_text_size, meta_table_field_count,
        };
        use objc2_foundation::{NSPoint, NSRect, NSSize};
        let picker = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(560.0, 400.0));
        let max_height = detail_max_height(picker);
        // The detail never grows past the picker it stands beside: `detail_frame_for` places it
        // inside the picker's vertical bounds, so a taller panel had no valid position (and the
        // debug assertion there caught it at open).
        assert_eq!(max_height, 400.0);
        // The pane is a column: the body's share is the cap minus the mock's pane padding and the
        // meta table (which can be zero here -- a 400pt picker is shorter than the table alone).
        assert_eq!(
            max_height,
            detail_pane_layout(super::DETAIL_MAX_W, max_height, meta_table_field_count())
                .scroll
                .size
                .height
                + super::DETAIL_INSET_TOP
                + super::DETAIL_INSET_BOTTOM
        );
        let (plain_w, one_line_h) = detail_text_size("hi", TextKind::Plain, max_height);
        assert_eq!(plain_w, super::DETAIL_MAX_W);
        assert_eq!(one_line_h, super::DETAIL_LINE_H);
        let (_, two_line_h) = detail_text_size("first\nsecond", TextKind::Plain, max_height);
        assert_eq!(two_line_h, super::DETAIL_LINE_H * 2.0);
        let (_, three_line_h) = detail_text_size("a\nb\nc", TextKind::Plain, max_height);
        assert_eq!(three_line_h, super::DETAIL_LINE_H * 3.0);
        // A long body stops at the pane's body share: beyond that the column scrolls. The body is
        // a TextKit layout of the same string inside the content column, so the text must exceed
        // that share at every supported width: a JSON-style indented document gives enough lines
        // without a megabyte of input (estimate_lines floors a very long single line to one).
        let long = "{\n  \"key\": \"value\",\n  \"list\": [1, 2, 3]\n},\n".repeat(2_500);
        let pane = detail_pane_layout(super::DETAIL_MAX_W, max_height, meta_table_field_count());
        let long_h = detail_text_size(&long, TextKind::Plain, max_height).1;
        assert_eq!(
            long_h, pane.body_h,
            "estimate produced {long_h}, pane body is {}",
            pane.body_h
        );
        // Long code lines soft-wrap visually and hit the same cap.
        let long_code = "x".repeat(200_000);
        let (code_w, code_h) = detail_text_size(&long_code, TextKind::Code, max_height);
        assert_eq!(code_w, super::DETAIL_CODE_MAX_W);
        assert_eq!(code_h, pane.body_h);
        // With wrapping off, the same long line occupies one visual line and overflows horizontally.
        let (_, no_wrap_h) = super::detail_unwrapped_code_size(&long_code, max_height);
        assert_eq!(no_wrap_h, super::DETAIL_LINE_H);
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
    // SMOKE_CELL=M2 -- read by scripts/smoke-runners.sh, which refuses a runner
    // without exactly one declaration. Spawns the app child (--smoke-clipboard) and drives the real picker panel.
    // SMOKE_DISTURBS=focus
    #[test]
    #[ignore]
    fn picker_rebuild_smoke() {
        smoke_runner!("picker_rebuild_smoke", 1, {
            // The child is the process that ran the assertions, so its recorded verdict is
            // what this parent reports; it never infers one from the exit status.
            crate::smoke_verdict::run_smoke_child("picker_rebuild_smoke", &["--smoke-clipboard"]);
        });
    }
    /// The promise in AGENTS.md, through the decision the reload path uses: a paused session that
    /// is disabled and re-enabled still records nothing.
    ///
    /// `apply_config_change` itself cannot run here (it touches AppKit), so this asserts the
    /// decision it branches on plus the gate the decision drives -- which is the part a regression
    /// would break.
    #[test]
    fn a_paused_session_stays_paused_across_disable_and_enable() {
        let _gate = gate_lock();
        use crate::runtime_config::{pause_gate_action, PauseGateAction};
        // Paused: the persisted intent and the gate both say so.
        super::set_collection_paused(true);
        assert_eq!(super::collection_gate_state(), "paused");
        // Switching the feature OFF and back ON is a pause -> pause transition; neither may resume.
        assert_eq!(
            pause_gate_action(true, true),
            PauseGateAction::Pause,
            "disabling or re-enabling a paused session must not open the gate"
        );
        // Only an explicit resume opens it.
        super::set_test_change_count(Some(5));
        assert!(!super::set_collection_paused(false));
        assert_eq!(super::collection_gate_state(), "active");
        super::set_test_change_count(None);
    }

    /// A copy's recorded source must come from the snapshot taken at copy time, not from a second
    /// lookup: an app that quits while the payload is being read must still be recorded under the
    /// identity that was resolved when it was alive.
    #[test]
    fn the_recorded_source_uses_the_snapshot_taken_at_copy_time() {
        // A snapshot whose PID no longer resolves still carries its identity.
        let snapshot = crate::clipboard::monitor::SourceSnapshot {
            name: "Example".to_string(),
            pid: -1,
            identity: Some(crate::app_identity::AppIdentity {
                key: "com.example.app".to_string(),
                fingerprint: None,
                process_start_time_us: None,
            }),
        };
        let (name, key) = crate::clipboard::monitor::record_source(&snapshot);
        assert_eq!(name, "Example");
        assert_eq!(
            key, "com.example.app",
            "the key must come from the snapshot, not from re-resolving a stale PID"
        );
        // A snapshot with no identity (the lookup failed at copy time) records the name only.
        let empty = crate::clipboard::monitor::SourceSnapshot {
            name: "Gone".to_string(),
            pid: -1,
            identity: None,
        };
        let (name, key) = crate::clipboard::monitor::record_source(&empty);
        assert_eq!(name, "Gone");
        assert!(key.is_empty());
    }
}
