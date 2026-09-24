//! Clipboard subsystem · model: captured entries, grouping, and filters.

use super::*;

/// Conservative fallback for detail document height: explicit newlines start a new line. The
/// character units are only used when AppKit measurement is unavailable and never truncate list content.
pub(super) fn estimate_lines(text: &str, max_units: usize) -> usize {
    let mut units = 0usize;
    let mut lines = 1usize;
    for ch in text.chars() {
        if ch == '\n' {
            lines += 1;
            units = 0;
            continue;
        }
        let w = if ch.is_ascii() { 1 } else { 2 };
        if units + w > max_units {
            lines += 1;
            units = w;
        } else {
            units += w;
        }
    }
    lines
}

/// Detail-panel content width -> per-line width units, using the same estimate as the row
/// buttons (50 units fit the row content width ≈ 346pt).
pub(super) fn detail_text_units(width: f64) -> usize {
    let units_per_pt = LINE_MAX_UNITS as f64 / content_width();
    ((width * units_per_pt).floor() as usize).max(1)
}

/// The row content's usable width: window - both paddings - icon - icon gap - actions.
pub(super) fn content_width() -> f64 {
    // The content button's width: window - list margins - the row's L/R padding.
    PICKER_W - PAD_X * 2.0 - ROW_PAD_L - ROW_PAD_R
}

/// Whether the source app is shown (reads CONFIG; recording is always on, the toggle gates
/// both the name and icon in the row's meta line).
pub(super) fn show_source_app() -> bool {
    CONFIG.read().unwrap().clipboard.show_source_app
}

/// The source icon and name share one display switch; without an icon-cache key there is no
/// file to load either.
pub(super) fn should_show_source_icon(show_source: bool, entry: &ClipEntry) -> bool {
    show_source && !entry.source_key.is_empty()
}

/// The row's meta line (app · relative time · line count): the small text below the content,
/// 10px light gray per the mockup. The kind cue moved INTO the content itself (blue URLs,
/// monospaced code); the meta line carries no badge.
pub(super) fn build_meta_text(entry: &ClipEntry, show_source: bool) -> String {
    let mut parts: Vec<String> = Vec::new();
    if show_source {
        if entry.source_app.is_empty() {
            parts.push(t("clipboard.unknown_source"));
        } else {
            parts.push(entry.source_app.clone());
        }
    }
    if let Some(ts) = entry.copied_at {
        parts.push(relative_time_label(ts, now_secs()));
    }
    // Report only source line breaks; never mistake the list cell's soft wrapping for them.
    if entry.image.is_none() {
        if let Some(count) = physical_line_count(&entry.text) {
            parts.push(tf(
                if count == 1 {
                    "clipboard.meta_lines_one"
                } else {
                    "clipboard.meta_lines_other"
                },
                &[("count", &count.to_string())],
            ));
        }
    }
    parts.join(" · ")
}

/// Return physical line count when source newlines exist; omit it for ordinary single-line text.
pub(super) fn physical_line_count(text: &str) -> Option<usize> {
    if text.contains('\n') {
        Some(text.split('\n').count().max(1))
    } else {
        None
    }
}

/// Relative time: Just now / Today HH:mm / Yesterday HH:mm / older MM-dd HH:mm (local tz).
pub(super) fn relative_time_label(ts: u64, now: u64) -> String {
    if now.saturating_sub(ts) < 60 {
        return t("clipboard.time_just_now");
    }
    let delta = day_no(ts) - day_no(now);
    let hhmm = local_hhmm(ts);
    match delta {
        0 => tf("clipboard.time_today", &[("time", &hhmm)]),
        1 => tf("clipboard.time_yesterday", &[("time", &hhmm)]),
        _ => format_copied_at(ts),
    }
}

/// The local day ordinal (year*366 + yday): same-day diff = 0, yesterday = 1 (DST-proof).
pub(super) fn day_no(unix_secs: u64) -> i64 {
    unsafe {
        let mut tm: Tm = std::mem::zeroed();
        let s = unix_secs as i64;
        localtime_r(&s, &mut tm);
        tm.tm_year as i64 * 366 + tm.tm_yday as i64
    }
}

/// timestamp -> local HH:mm.
pub(super) fn local_hhmm(unix_secs: u64) -> String {
    unsafe {
        let mut tm: Tm = std::mem::zeroed();
        let s = unix_secs as i64;
        localtime_r(&s, &mut tm);
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
    }
}

/// Time group: Today / Yesterday / Earlier (by the local day ordinal). Legacy entries
/// without a timestamp join Earlier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DayGroup {
    Today,
    Yesterday,
    Earlier,
}

pub(super) fn day_group(ts: Option<u64>) -> DayGroup {
    let Some(ts) = ts else {
        return DayGroup::Earlier;
    };
    match day_no(ts) - day_no(now_secs()) {
        0 => DayGroup::Today,
        1 => DayGroup::Yesterday,
        _ => DayGroup::Earlier,
    }
}

/// the group header's label.
pub(super) fn group_label(g: DayGroup) -> String {
    match g {
        DayGroup::Today => t("clipboard.group_today"),
        DayGroup::Yesterday => t("clipboard.group_yesterday"),
        DayGroup::Earlier => t("clipboard.group_earlier"),
    }
}

/// The row content height: uniformly 61pt (the mockup's min-height 61px, content
/// vertically centered).
pub(super) fn row_content_h(_entry: &ClipEntry) -> f64 {
    ROW_H
}

/// Compute the per-row pitches. **Every entry keeps ONE fixed pitch** = content + row gap;
/// a group-header height is inserted before the first row of each group (computed on the
/// FILTERED display order, so time boundaries stay correct under filtering).
pub(super) fn compute_pitches(texts: &[ClipEntry]) -> Vec<f64> {
    let mut prev: Option<DayGroup> = None;
    texts
        .iter()
        .map(|e| {
            let g = day_group(e.copied_at);
            let hdr = if prev.is_none() || prev != Some(g) {
                GROUP_H
            } else {
                0.0
            };
            prev = Some(g);
            hdr + row_content_h(e)
        })
        .collect()
}

/// The fixed header strip's height: top padding + the search/filter/clear row + the gap
/// to the list.
/// The fixed header strip: the search zone (14 + 48 + 8) + the filters row (38).
pub(super) fn header_strip_h() -> f64 {
    TOP_PAD_Y + SEARCH_H + SEARCH_GAP_Y + FILTERS_H
}

/// Calculate the picker minimum from three same-group records and use it for every state, so it
/// stays aligned when row or surrounding-region dimensions change.
pub(super) fn picker_min_height() -> f64 {
    (header_strip_h() + GROUP_H + ROW_H * 3.0 + FOOTER_H + PAD_Y).max(PICKER_MIN_HEIGHT)
}

/// The row list's top offset INSIDE the document: just the gap to the header strip (the
/// strip is no longer inside the scroll area, so no 38pt clearance is needed -- that left
/// the odd blank band between the first row and the search field).
pub(super) fn rows_top_offset() -> f64 {
    CLEAR_BTN_GAP
}

/// The top y of row `idx` (flipped coords): rows_top_offset + the pitches before it.
pub(super) fn row_top(idx: usize, pitches: &[f64]) -> f64 {
    rows_top_offset() + pitches.iter().take(idx).sum::<f64>()
}

/// Compute every row's top offset in one pass (prefix sums): calling `row_top` per row
/// turns a full layout into O(n^2), burning CPU on the rebuild/scroll path as rows grow.
pub(super) fn row_offsets(pitches: &[f64]) -> Vec<f64> {
    let mut offsets = Vec::with_capacity(pitches.len());
    let mut acc = rows_top_offset();
    for &pitch in pitches {
        offsets.push(acc);
        acc += pitch;
    }
    offsets
}

/// u64 serialized as a hex string: TOML integers are i64, so hashes with the high bit
/// set would fail to serialize ("u64 value out of range"); hashes must go as strings.
pub(super) mod u64_hex {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("{v:016x}"))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        let s = String::deserialize(d)?;
        u64::from_str_radix(&s, 16).map_err(serde::de::Error::custom)
    }
}

/// An image entry, in two forms:
/// - a DATA entry (image-data copy): a DISK reference to the original-format bytes + its
///   UTI + a downsampled PNG preview. The original bytes never stay in memory: hashed and
///   written to a cache file at record time, read back on paste and written verbatim under
///   `uti` (a JPG pastes back as JPG, an animated GIF as a GIF); the preview is only for
///   the thumbnail (first frame for animations, never pasted).
/// - a FILE-COPY entry (an image file copied in Finder): the file is read ONCE at record
///   time (transiently) for a content hash + a thumbnail preview, then the bytes are
///   discarded (data_path is always empty, no shadow copy). The hash drives CONTENT dedup
///   (a file and its Finder duplicate collapse into one entry); `source_path` records the
///   source. Pasting restores `public.file-url`, handing the FILE (not the image data) to
///   the target app (Finder duplicates the original file, chat apps attach the file, GIF
///   animation fully preserved; bare image data is ignored by Finder and re-encoded into
///   PNG by some apps); a deleted/moved source makes the entry unpastable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct ImageEntry {
    /// The original format's UTI (public.png / public.jpeg / com.compuserve.gif ...).
    pub(super) uti: String,
    /// The original bytes' content hash (FNV-1a): the cache filename + dedup key for data
    /// entries; the content-dedup key + preview filename for file-copy entries. 0 for a
    /// degenerate file entry whose decode failed. Serialized as a hex string (TOML
    /// integers can't hold 64-bit unsigned values).
    #[serde(with = "u64_hex")]
    pub(super) hash: u64,
    /// The cache file holding the original-format bytes (data entries only; always empty
    /// for file-copy entries -- pasting goes through the file-url). Skipped in
    /// serialization: rebuilt from the hash on load.
    #[serde(skip)]
    pub(super) data_path: std::path::PathBuf,
    /// A downsampled PNG preview (thumbnail drawing; the only image bytes held in memory,
    /// ~100-300KB). Skipped in serialization: the preview lives separately as
    /// `{hash}.preview` and is read back on load (regenerated from the data bytes or the
    /// source file when missing). Held via `Arc`: history snapshots (persistence dispatch,
    /// detail display) clone only a refcount instead of deep-copying every preview on each
    /// copy.
    #[serde(skip)]
    pub(super) preview_png: Arc<Vec<u8>>,
    /// The source path of a file copy (None = a data entry, a bare image copy).
    pub(super) source_path: Option<String>,
}

/// A history entry: text + a pinned flag + the source app name + the source icon-cache key.
/// Pinned entries stay at the top.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct ClipEntry {
    pub(super) text: String,
    /// An image entry (original format + preview; None for text entries). When both text
    /// and an image are on the pasteboard, text wins -- images are only recorded when
    /// there is no text.
    pub(super) image: Option<ImageEntry>,
    pub(super) pinned: bool,
    /// The frontmost app name when the text was copied (empty = unknown, e.g. legacy entries
    /// or an unavailable frontmost app).
    pub(super) source_app: String,
    /// The source app's icon-cache key (resolve_app_identity: bundle id > exec-path hash >
    /// pid). Empty = no identity (e.g. legacy entries) -> no icon in the header.
    pub(super) source_key: String,
    /// The copy timestamp (unix seconds): the basis of auto-expiry; refreshed to the
    /// latest copy time on dedup-move-to-front. None = a legacy entry (no timestamp),
    /// exempt from expiry -- a conservative migration, never wrongly deleted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) copied_at: Option<u64>,
}

/// The current unix seconds.
pub(super) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// localtime_r and Tm now live in ffi.rs (one declaration shared with the logger).

/// Copy timestamp -> "MM-dd HH:mm" (local time; the header bar is narrow, so the year is
/// dropped). Pure function; the format is unit-tested.
pub(super) fn format_copied_at(unix_secs: u64) -> String {
    unsafe {
        let mut tm: Tm = std::mem::zeroed();
        let s = unix_secs as i64;
        localtime_r(&s, &mut tm);
        format!(
            "{:02}-{:02} {:02}:{:02}",
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min
        )
    }
}

/// Copy timestamp -> "YYYY-MM-DD HH.MM.SS" (local time; the save-as suggested-filename
/// suffix). The time part deliberately uses dots instead of colons -- colons are
/// illegal/reserved in HFS+/Finder names (macOS screenshot naming style). Pure function;
/// the format is unit-tested.
pub(super) fn format_save_stamp(unix_secs: u64) -> String {
    unsafe {
        let mut tm: Tm = std::mem::zeroed();
        let s = unix_secs as i64;
        localtime_r(&s, &mut tm);
        format!(
            "{:04}-{:02}-{:02} {:02}.{:02}.{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
}

/// The save-as suggested-filename stamp: the entry's copy time (copied_at); legacy
/// entries without a timestamp degrade to the save moment (better than nothing, and
/// legacy entries age out via expiry anyway).
pub(super) fn save_stamp_for(entry: &ClipEntry) -> String {
    format_save_stamp(entry.copied_at.unwrap_or_else(now_secs))
}

/// The auto-expiry TTL in seconds: 0 days = never -> None. Read live from CONFIG (a hot
/// reload takes effect immediately).
pub(super) fn ttl_secs() -> Option<u64> {
    let days = CONFIG
        .read()
        .map(|c| c.clipboard.auto_expire_days)
        .unwrap_or(0);
    if days == 0 {
        None
    } else {
        Some(days as u64 * 86400)
    }
}

/// Expire entries (pure, synchronous): unpinned entries with a timestamp whose
/// `now - copied_at >= ttl` are removed; pinned entries never expire; legacy entries
/// without a timestamp never expire. Image cache files follow the reference rules
/// (same as delete/truncate: a hash still referenced by a survivor is kept).
/// ttl_secs = None disables expiry (returns 0). Returns the number removed.
pub(super) fn expire_entries(
    history: &mut Vec<ClipEntry>,
    now_secs: u64,
    ttl_secs: Option<u64>,
) -> usize {
    let Some(ttl) = ttl_secs else {
        return 0;
    };
    let mut dropped = 0;
    let mut dropped_image_hashes = Vec::new();
    history.retain(|e| {
        // Clock rollback (now < copied_at): saturating_sub yields 0, under ttl, safe.
        let expired = !e.pinned
            && e.copied_at
                .map(|t| now_secs.saturating_sub(t) >= ttl)
                .unwrap_or(false);
        if expired {
            dropped += 1;
            if let Some(hash) = e.image.as_ref().map(|image| image.hash) {
                if hash != 0 {
                    dropped_image_hashes.push(hash);
                }
            }
        }
        !expired
    });
    dropped_image_hashes.sort_unstable();
    dropped_image_hashes.dedup();
    for hash in dropped_image_hashes {
        cache_delete_for_hash(history, hash);
    }
    if dropped > 0 {
        super::bump_history_revision();
    }
    dropped
}

// The dedup hash itself now lives in crate::hash; the block below is the phase-2
// (content hash) plan.

/*
[PHASE 2] Image CONTENT hash: decode the PNG -> draw a 16x16 thumbnail -> FNV-1a over
its TIFF bytes. Re-encoded copies of the same image (different bytes) hash identically;
decoding failures fall back to the raw-byte hash. Deferred: phase 1 dedups by the raw
byte hash only (cross-encoding dedup waits for this). To enable: uncomment this block and
restore the image_content_hash call + the ClipEntry.image_hash field in record_image
(also in the struct, record_text, and the test helpers).
pub(super) unsafe fn image_content_hash(png: &[u8]) -> u64 {
    let data: *mut AnyObject = msg_send![
        class!(NSData),
        dataWithBytes: png.as_ptr() as *const c_void,
        length: png.len()
    ];
    let img: *mut AnyObject = msg_send![class!(NSImage), alloc];
    let img: *mut AnyObject = msg_send![img, initWithData: data];
    if img.is_null() {
        return fnv1a64(png);
    }
    let thumb: *mut AnyObject = msg_send![class!(NSImage), alloc];
    let thumb: *mut AnyObject = msg_send![thumb, initWithSize: NSSize::new(16.0, 16.0)];
    let _: () = msg_send![thumb, lockFocus];
    let dst = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(16.0, 16.0));
    let src = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
    let op: usize = 1; // NSCompositingOperationCopy
    let _: () = msg_send![img, drawInRect: dst, fromRect: src, operation: op, fraction: 1.0f64];
    let _: () = msg_send![thumb, unlockFocus];
    let tiff: *mut AnyObject = msg_send![thumb, TIFFRepresentation];
    if tiff.is_null() {
        return fnv1a64(png);
    }
    let len: usize = msg_send![tiff, length];
    let ptr: *const c_void = msg_send![tiff, bytes];
    if ptr.is_null() || len == 0 {
        return fnv1a64(png);
    }
    fnv1a64(std::slice::from_raw_parts(ptr as *const u8, len))
}
*/

/// The insertion index for a new (unpinned) entry: right after the pinned block.
pub(super) fn insert_position(history: &[ClipEntry]) -> usize {
    history.iter().take_while(|e| e.pinned).count()
}

/// Find an entry's index by text; None when absent.
pub(super) fn find_by_text(history: &[ClipEntry], text: &str) -> Option<usize> {
    history.iter().position(|e| e.text == text)
}

/// Move an existing entry to the front, KEEPING its pinned state: pinned entries go to the
/// top of the pinned block, unpinned ones to the top of the unpinned block (the newest
/// slot). The list therefore never holds duplicates.
pub(super) fn move_entry_to_front(history: &mut Vec<ClipEntry>, idx: usize) {
    if idx >= history.len() {
        return;
    }
    let e = history.remove(idx);
    let pos = if e.pinned {
        0
    } else {
        insert_position(history)
    };
    history.insert(pos, e);
}

/// Identify whether two history entries represent the same restorable item, ignoring
/// presentation metadata such as source and timestamp.
pub(super) fn same_clip_entry_identity(a: &ClipEntry, b: &ClipEntry) -> bool {
    match (&a.image, &b.image) {
        (None, None) => a.text == b.text,
        (Some(ai), Some(bi)) => {
            if ai.source_path.is_some() != bi.source_path.is_some() {
                return false;
            }
            if ai.hash != 0 && bi.hash != 0 {
                ai.hash == bi.hash
            } else {
                ai.source_path == bi.source_path
            }
        }
        _ => false,
    }
}

/// Remove an entry without deleting its image cache, so a short-lived undo can restore it.
pub(super) fn remove_entry_for_undo(history: &mut Vec<ClipEntry>, idx: usize) -> Option<ClipEntry> {
    let removed = history.get(idx).cloned()?;
    history.remove(idx);
    super::bump_history_revision();
    Some(removed)
}

/// Restore an entry while respecting the pinned boundary; never insert a duplicate.
pub(super) fn restore_entry_at(
    history: &mut Vec<ClipEntry>,
    entry: ClipEntry,
    original_index: usize,
) -> (usize, bool) {
    if let Some(existing) = history
        .iter()
        .position(|item| same_clip_entry_identity(item, &entry))
    {
        return (existing, false);
    }
    let pinned_count = insert_position(history);
    let pos = if entry.pinned {
        original_index.min(pinned_count)
    } else {
        original_index.max(pinned_count).min(history.len())
    };
    history.insert(pos, entry);
    super::bump_history_revision();
    (pos, true)
}

/// Remove the confirmed history scope and return dropped entries for cache-reference checks.
pub(super) fn remove_history_scope(
    history: &mut Vec<ClipEntry>,
    clear_all: bool,
) -> Vec<ClipEntry> {
    let mut removed = Vec::new();
    let mut kept = Vec::with_capacity(history.len());
    for entry in history.drain(..) {
        if clear_all || !entry.pinned {
            removed.push(entry);
        } else {
            kept.push(entry);
        }
    }
    *history = kept;
    if !removed.is_empty() {
        super::bump_history_revision();
    }
    removed
}

/// Record a new text into the history:
/// - empty text is ignored
/// - full-list dedup: an existing text is moved to the front (pinned state kept, see
///   move_entry_to_front), and its source (name + icon key) is updated to this copy's source
///   (it is the "latest copy" now)
/// - a new text is inserted after the pinned block; entries beyond `max` are trimmed
///
/// Returns whether something was actually recorded.
pub(super) fn record_text(
    history: &mut Vec<ClipEntry>,
    text: &str,
    source: &str,
    source_key: &str,
    max: usize,
) -> bool {
    if text.is_empty() || max == 0 {
        return false;
    }
    if let Some(idx) = find_by_text(history, text) {
        // Rebuild the strings only when the source actually changed, avoiding pointless
        // allocations when the same app copies repeatedly.
        if history[idx].source_app != source {
            history[idx].source_app = source.to_string();
        }
        if history[idx].source_key != source_key {
            history[idx].source_key = source_key.to_string();
        }
        // A dedup-move = the latest copy: refresh the timestamp so expiry counts from
        // the most recent copy, not the first one.
        history[idx].copied_at = Some(now_secs());
        move_entry_to_front(history, idx);
        super::bump_history_revision();
        return true;
    }
    let pos = insert_position(history);
    history.insert(
        pos,
        ClipEntry {
            text: text.to_string(),
            image: None,
            pinned: false,
            source_app: source.to_string(),
            source_key: source_key.to_string(),
            copied_at: Some(now_secs()),
        },
    );
    if history.len() > max {
        // When trimming beyond the cap, drop the trimmed image entries' cache files too --
        // but only when the hash is no longer referenced by a survivor (same-hash
        // file/data entries may coexist and share the cache).
        for dropped in &history[max..] {
            cache_delete_for_removed(&history[..max], dropped);
        }
        history.truncate(max);
    }
    super::bump_history_revision();
    true
}

/// Record an image into the history (two kinds, EACH deduped within its own class):
/// - a DATA entry (image-data copy, bytes already cached by the caller): dedup by content
///   hash
/// - a FILE-COPY entry (the file is read once for a hash + thumbnail, bytes never
///   stored): dedup by content hash -- a file and its Finder duplicate (different paths,
///   identical bytes) collapse into one entry; degenerate entries whose decode failed
///   (hash=0) fall back to dedup by source path
///
/// Same rules as record_text:
/// - an empty preview AND no source path (recording failed) is ignored
/// - an existing entry (same hash / same path) moves to the front (pinned kept), the
///   source updates; a file entry's source path also updates to the latest copy
/// - a new entry is inserted after the pinned block; entries beyond `max` are trimmed
///   (cross-encoding dedup waits for phase 2, see the commented-out image_content_hash).
pub(super) fn record_image(
    history: &mut Vec<ClipEntry>,
    image: &ImageEntry,
    source: &str,
    source_key: &str,
    max: usize,
) -> bool {
    if (image.preview_png.is_empty() && image.source_path.is_none()) || max == 0 {
        return false;
    }
    // File entries dedup by content hash among file entries (degenerate hash=0 entries by
    // path); data entries dedup by content hash among data entries. Never across classes.
    let dedup_hit = if image.source_path.is_some() {
        history.iter().position(|e| {
            e.image.as_ref().is_some_and(|i| {
                i.source_path.is_some()
                    && if image.hash != 0 {
                        i.hash == image.hash
                    } else {
                        i.source_path.as_deref() == image.source_path.as_deref()
                    }
            })
        })
    } else {
        history.iter().position(|e| {
            e.image
                .as_ref()
                .is_some_and(|i| i.source_path.is_none() && i.hash == image.hash)
        })
    };
    if let Some(idx) = dedup_hit {
        // Rebuild the strings only when the source actually changed (same as record_text).
        if history[idx].source_app != source {
            history[idx].source_app = source.to_string();
        }
        if history[idx].source_key != source_key {
            history[idx].source_key = source_key.to_string();
        }
        // A dedup-move = the latest copy: refresh the timestamp (same as record_text).
        history[idx].copied_at = Some(now_secs());
        // A file-entry dedup hit: the source path updates to the latest copy (pasting
        // restores the newest file).
        if image.source_path.is_some() {
            history[idx].image.as_mut().unwrap().source_path = image.source_path.clone();
        }
        move_entry_to_front(history, idx);
        super::bump_history_revision();
        return true;
    }
    let pos = insert_position(history);
    history.insert(
        pos,
        ClipEntry {
            // File-reference entries keep the filename in text: the row shows it and it is
            // searchable (paste goes through the image branch; text never gets pasted).
            // Data entries keep an empty text.
            text: image
                .source_path
                .as_deref()
                .map(|p| p.rsplit('/').next().unwrap_or("").to_string())
                .unwrap_or_default(),
            image: Some(image.clone()),
            pinned: false,
            source_app: source.to_string(),
            source_key: source_key.to_string(),
            copied_at: Some(now_secs()),
        },
    );
    if history.len() > max {
        // When trimming beyond the cap, drop the trimmed image entries' cache files too --
        // but only when the hash is no longer referenced by a survivor (same-hash
        // file/data entries may coexist and share the cache).
        for dropped in &history[max..] {
            cache_delete_for_removed(&history[..max], dropped);
        }
        history.truncate(max);
    }
    super::bump_history_revision();
    true
}

/// Pin entry `idx`: move it to the top of the pinned block. Returns the entry's NEW
/// history index (always 0).
pub(super) fn pin_entry(history: &mut Vec<ClipEntry>, idx: usize) -> usize {
    if idx >= history.len() || history[idx].pinned {
        return idx;
    }
    let mut e = history.remove(idx);
    e.pinned = true;
    history.insert(0, e);
    super::bump_history_revision();
    0
}

/// Unpin entry `idx`: move it to the top of the unpinned block (the newest slot). Returns
/// the entry's NEW history index (the insert position, right after the last pinned entry).
pub(super) fn unpin_entry(history: &mut Vec<ClipEntry>, idx: usize) -> usize {
    if idx >= history.len() || !history[idx].pinned {
        return idx;
    }
    let mut e = history.remove(idx);
    e.pinned = false;
    let pos = insert_position(history);
    history.insert(pos, e);
    super::bump_history_revision();
    pos
}

/// Toggle the pinned state of entry `idx`; returns (the new state, the entry's NEW history
/// index). Pure function shared by the pin-button callback and the ← shortcut; unit-tested.
/// The new index feeds "follow-pin" selection -- the OLD index points at a different entry
/// once the list is reordered, so it must not be used.
pub(super) fn toggle_pin_on(history: &mut Vec<ClipEntry>, idx: usize) -> (bool, usize) {
    let Some(entry) = history.get(idx) else {
        return (false, idx);
    };
    let pinned = entry.pinned;
    let new_idx = if pinned {
        unpin_entry(history, idx)
    } else {
        pin_entry(history, idx)
    };
    (!pinned, new_idx)
}

/// Delete entry `idx` (out of range is ignored); an image entry's cache file goes too.
pub(super) fn delete_entry(history: &mut Vec<ClipEntry>, idx: usize) {
    if let Some(removed) = remove_entry_for_undo(history, idx) {
        cache_delete_for_removed(history, &removed);
    }
}

/// Picker filters: All / Text / Image / Link / Code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClipFilter {
    All,
    Text,
    Image,
    Link,
    Code,
}

/// the active filter.
pub(super) static CLIP_FILTER: Mutex<ClipFilter> = Mutex::new(ClipFilter::All);

#[cfg(not(test))]
/// Cached lowercase text and classification: reused across every keystroke/rebuild and
/// rebuilt only when the history revision changes.
#[cfg(not(test))]
type FilterTextLowerCache = Option<(u64, Vec<String>, Vec<TextKind>)>;

#[cfg(not(test))]
static FILTER_TEXT_LOWER_CACHE: LazyLock<Mutex<FilterTextLowerCache>> =
    LazyLock::new(|| Mutex::new(None));

/// The Tab filter cycle: All -> Text -> Image -> Link -> Code -> All.
pub(super) fn next_clip_filter(filter: ClipFilter) -> ClipFilter {
    match filter {
        ClipFilter::All => ClipFilter::Text,
        ClipFilter::Text => ClipFilter::Image,
        ClipFilter::Image => ClipFilter::Link,
        ClipFilter::Link => ClipFilter::Code,
        ClipFilter::Code => ClipFilter::All,
    }
}

/// Whether an entry matches the filter (cached-classification variant): avoids re-running
/// classify_text for every entry on every keystroke.
fn matches_filter_kind(e: &ClipEntry, kind: TextKind, f: ClipFilter) -> bool {
    match f {
        ClipFilter::All => true,
        ClipFilter::Image => e.image.is_some(),
        ClipFilter::Text => e.image.is_none() && kind == TextKind::Plain,
        ClipFilter::Link => e.image.is_none() && kind == TextKind::Url,
        ClipFilter::Code => e.image.is_none() && kind == TextKind::Code,
    }
}

/// Whether an entry matches the filter (simple, uncached-classification variant; tests only).
#[cfg(test)]
pub(super) fn matches_filter(e: &ClipEntry, f: ClipFilter) -> bool {
    matches_filter_kind(e, classify_text(&e.text), f)
}

/// + All = every entry; case-insensitive substring match).
pub(super) fn filtered_indices(
    history: &[ClipEntry],
    query: &str,
    filter: ClipFilter,
) -> Vec<usize> {
    let q = query.to_lowercase();

    // The most common path (empty query + All) builds no cache at all.
    if q.is_empty() && filter == ClipFilter::All {
        return (0..history.len()).collect();
    }

    #[cfg(not(test))]
    {
        // Reuse the lowercase text and classification built for the current history revision so
        // each keystroke avoids a fresh lowercase copy and classify_text for every entry.
        let revision = super::history_revision();
        let mut cache = FILTER_TEXT_LOWER_CACHE.lock().unwrap();
        let rebuild = cache
            .as_ref()
            .is_none_or(|(cached_revision, texts, kinds)| {
                *cached_revision != revision
                    || texts.len() != history.len()
                    || kinds.len() != history.len()
            });
        if rebuild {
            *cache = Some((
                revision,
                history.iter().map(|e| e.text.to_lowercase()).collect(),
                history.iter().map(|e| classify_text(&e.text)).collect(),
            ));
        }
        let (_, texts, kinds) = cache.as_ref().unwrap();
        history
            .iter()
            .enumerate()
            .filter(|(i, e)| {
                matches_filter_kind(e, kinds[*i], filter)
                    && (q.is_empty() || texts[*i].contains(&q))
            })
            .map(|(i, _)| i)
            .collect()
    }

    #[cfg(test)]
    history
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            matches_filter(e, filter) && (q.is_empty() || e.text.to_lowercase().contains(&q))
        })
        .map(|(i, _)| i)
        .collect()
}

/// Display index -> history index (via the current filtered list; None when out of range).
pub(super) fn mapped_index(display_idx: usize) -> Option<usize> {
    super::with_clipboard_ui(|ui| ui.filtered.get(display_idx).copied())
}

/// The effective max entry count (read from CONFIG; takes effect on the next poll).
pub(super) fn max_entries() -> usize {
    CONFIG
        .read()
        .map(|c| c.clipboard.max_entries as usize)
        .unwrap_or(50)
        .clamp(1, 100)
}
