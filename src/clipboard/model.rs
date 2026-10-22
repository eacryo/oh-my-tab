//! Clipboard subsystem · model: captured entries, grouping, and filters.

use super::*;
use std::collections::HashSet;

/// Conservative fallback for detail document height: explicit newlines start a new line. The
/// character units are only used when AppKit measurement is unavailable and never truncate list content.
#[cfg(test)]
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
#[cfg(test)]
pub(super) fn detail_text_units(width: f64) -> usize {
    ((width / content_width() * LINE_MAX_UNITS as f64).floor() as usize).max(1)
}

/// The row content's usable width: window - both paddings - icon - icon gap - actions.
#[cfg(test)]
pub(super) fn content_width() -> f64 {
    // The content button's width: window - list margins - the row's L/R padding.
    PICKER_W - PAD_X * 2.0 - ROW_PAD_L - ROW_PAD_R
}

/// Row title width after reserving the non-overlapping pin/delete hit targets on its trailing edge.
pub(super) fn row_content_width(row_w: f64) -> f64 {
    (row_w - ROW_PAD_L - ROW_PAD_R - ACTION_BTN * 2.0 - ACTION_GAP - 4.0).max(1.0)
}

/// Whether the source app is shown (reads CONFIG; recording is always on, the toggle gates
/// both the name and icon in the row's meta line).
pub(super) fn show_source_app() -> bool {
    CONFIG.read().unwrap().clipboard.show_source_app
}

/// A payload's size as the mock's `sizeText` writes it: the byte count below 1 KiB, otherwise
/// one decimal of KiB, then of MiB.
///
/// The numbers are the clipboard payload measured in bytes, but they are divided by 1000, as the
/// mock does (`bytes / 1024` would print 1000 bytes as "1.0 KiB"; the mock prints "1.0 KB").
/// The unit suffix is the locale's, so an English row reads "402 bytes" / "1.0 KB" -- the mock's
/// own label, since the unit is part of the displayed number rather than a separate word.
pub(super) fn format_byte_size(bytes: u64) -> String {
    if bytes < 1000 {
        return tf(
            "clipboard.meta_size_bytes",
            &[("count", &bytes.to_string())],
        );
    }
    // The unit is chosen on the raw byte count, not on the rounded number: a payload below
    // 1_000_000 reads in KB even when it rounds up to five digits ("1000.0 KB", not "1.0 MB").
    let kb = bytes as f64 / 1000.0;
    if bytes < 1_000_000 {
        return tf("clipboard.meta_size_kb", &[("size", &format!("{kb:.1}"))]);
    }
    tf(
        "clipboard.meta_size_mb",
        &[("size", &format!("{:.1}", kb / 1000.0))],
    )
}

/// The value a meta row shows when the entry has nothing for it: the mock's `'—'`.
fn meta_absent() -> String {
    "—".to_string()
}

/// The detail pane's meta table (the v2 mock's `.meta-table`), in the mock's row order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MetaField {
    Source,
    Type,
    Dimensions,
    Size,
    CopiedAt,
}

impl MetaField {
    /// Every field, in the mock's `renderDetail()` row order.
    pub(super) const ALL: [MetaField; 5] = [
        MetaField::Source,
        MetaField::Type,
        MetaField::Dimensions,
        MetaField::Size,
        MetaField::CopiedAt,
    ];

    /// The row's localized label key (`来源` / `类型` / ... / `复制时间`).
    pub(super) fn label_key(self) -> &'static str {
        match self {
            MetaField::Source => "clipboard.meta_source",
            MetaField::Type => "clipboard.meta_type",
            MetaField::Dimensions => "clipboard.meta_dimensions",
            MetaField::Size => "clipboard.meta_size",
            MetaField::CopiedAt => "clipboard.meta_copied_at",
        }
    }

    /// The value for an entry, or the em dash when the entry has none.
    pub(super) fn value(self, entry: &ClipEntry, show_source: bool) -> String {
        let value = match self {
            // Honours the "show source app" setting, as the row meta line always did.
            MetaField::Source => show_source.then(|| {
                if entry.source_app.is_empty() {
                    t("clipboard.unknown_source")
                } else {
                    entry.source_app.clone()
                }
            }),
            MetaField::Type => Some(entry_kind_label(entry)),
            // Only an image has pixel dimensions; text and code show no number here.
            MetaField::Dimensions => entry.image.as_ref().and_then(|img| {
                let (w, h) = image_dimensions(img)?;
                Some(tf(
                    "clipboard.meta_dimensions_wh",
                    &[("width", &w.to_string()), ("height", &h.to_string())],
                ))
            }),
            MetaField::Size => Some(format_byte_size(entry_payload_bytes(entry))),
            // The full local date and time; a legacy entry without a timestamp shows the dash.
            MetaField::CopiedAt => entry.copied_at.map(copied_at_label),
        };
        value.unwrap_or_else(meta_absent)
    }
}

/// The meta table's rows: `(label, value)`, labels localized, values already formatted.
pub(super) fn meta_table(entry: &ClipEntry, show_source: bool) -> Vec<(String, String)> {
    MetaField::ALL
        .into_iter()
        .map(|field| (t(field.label_key()), field.value(entry, show_source)))
        .collect()
}

/// The entry's kind as the detail pane names it (the mock's `typeMeta`: 文本/链接/图片/代码).
pub(super) fn entry_kind_label(entry: &ClipEntry) -> String {
    let key = if entry.image.is_some() {
        "clipboard.filter_image"
    } else {
        match classify_text(&entry.text) {
            TextKind::Url => "clipboard.filter_link",
            TextKind::Code => "clipboard.filter_code",
            TextKind::Plain => "clipboard.filter_text",
        }
    };
    t(key)
}

/// The payload a "size" row describes: the original image bytes for an image entry, otherwise
/// the text's UTF-8 encoding. `0` for an image whose bytes are not readable without touching the
/// disk (a file copy), which then shows "0 bytes".
pub(super) fn entry_payload_bytes(entry: &ClipEntry) -> u64 {
    match &entry.image {
        Some(img) => image_payload_bytes(img),
        None => entry.text.len() as u64,
    }
}

/// An image entry's original byte length (0 when it has to be read from disk or decoded).
pub(super) fn image_payload_bytes(img: &ImageEntry) -> u64 {
    std::fs::metadata(&img.data_path)
        .map(|meta| meta.len())
        .unwrap_or(0)
}

/// An image entry's pixel size for the meta table's "尺寸" row, or `None` (the em dash).
///
/// The preview PNG is decoded at both ends: its own header before any AppKit object exists
/// (exact, and the only path a worker thread may take), then the decoded image's bitmap
/// representation, which is what accounts for DPI and orientation.
pub(crate) fn image_dimensions(img: &ImageEntry) -> Option<(usize, usize)> {
    if let Some(pixels) = png_pixel_size(&img.preview_png) {
        return Some(pixels);
    }
    // AppKit drawing is main-thread only, and this runs from the picker's rebuild, which is
    // also where a background history write can call back into.
    if !crate::is_main_thread() {
        return None;
    }
    unsafe { bitmap_pixels(&img.preview_png) }
}

/// The "尺寸" row's value for an image whose pixel size is not known without touching the disk.
///
/// `bestRepresentationForRect:` answers with a private `NSImageRepGeometryProxy` on a PNG-backed
/// image: the proxy's distance is reported for layout but it has no `pixelsWide`/`pixelsHigh`
/// (sending one traps, and a trap inside an `extern "C"` callback aborts the process, so the
/// whole app died on "展开详情"). Pixel size therefore comes from the bitmap representation the
/// decoded image actually holds, or from the PNG's own header -- never from a geometry proxy.
unsafe fn bitmap_pixels(png: &[u8]) -> Option<(usize, usize)> {
    let data: *mut AnyObject = msg_send![
        class!(NSData),
        dataWithBytes: png.as_ptr() as *const c_void,
        length: png.len()
    ];
    if data.is_null() {
        return None;
    }
    let image: *mut AnyObject = msg_send![class!(NSImage), alloc];
    let image: *mut AnyObject = msg_send![image, initWithData: data];
    if image.is_null() {
        return None;
    }
    let pixels = bitmap_representation_pixels(image);
    release_obj(image);
    pixels
}

/// The pixel grid of the first representation that really has one.
unsafe fn bitmap_representation_pixels(image: *mut AnyObject) -> Option<(usize, usize)> {
    let reps: *mut AnyObject = msg_send![image, representations];
    if reps.is_null() {
        return None;
    }
    let count: usize = msg_send![reps, count];
    for index in 0..count {
        let rep: *mut AnyObject = msg_send![reps, objectAtIndex: index as isize];
        if rep.is_null() {
            continue;
        }
        let is_bitmap: bool = msg_send![rep, isKindOfClass: class!(NSBitmapImageRep)];
        if !is_bitmap {
            continue;
        }
        // `pixelsWide`/`pixelsHigh` are NSBitmapImageRep's own declared properties; the type
        // check above is what separates it from the geometry proxy that has neither.
        let w: isize = msg_send![rep, pixelsWide];
        let h: isize = msg_send![rep, pixelsHigh];
        if w > 0 && h > 0 {
            return Some((w as usize, h as usize));
        }
    }
    None
}

/// A PNG's pixel size from its IHDR chunk: 8-byte signature, 4-byte length, "IHDR", then width
/// and height as big-endian `u32`. `None` for anything that is not a PNG.
pub(crate) fn png_pixel_size(bytes: &[u8]) -> Option<(usize, usize)> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if bytes.len() < 24 || bytes[..8] != SIGNATURE || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let read =
        |at: usize| u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    let (w, h) = (read(16), read(20));
    (w > 0 && h > 0).then_some((w as usize, h as usize))
}

/// A copy timestamp for the detail pane, ordered as the mock writes it: date units from the
/// largest to the smallest, then the time ("2026年10月19日 08:56"). The separators are localized,
/// so the row reads as one phrase in every language; `format_copied_at` keeps its own compact
/// form for the picker's narrow header.
pub(super) fn copied_at_label(unix_secs: u64) -> String {
    unsafe {
        let mut tm: Tm = std::mem::zeroed();
        let s = unix_secs as i64;
        localtime_r(&s, &mut tm);
        tf(
            "clipboard.meta_copied_at_value",
            &[
                ("year", &(tm.tm_year + 1900).to_string()),
                ("month", &(tm.tm_mon + 1).to_string()),
                ("day", &tm.tm_mday.to_string()),
                ("time", &format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)),
            ],
        )
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

/// Time group: Today / Yesterday / Earlier (by the local day ordinal). Legacy entries
/// without a timestamp join Earlier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DayGroup {
    Today,
    Yesterday,
    Earlier,
}

/// The group-clear button's tag (Today/Yesterday/Earlier -> 0/1/2).
pub(super) fn day_group_from_tag(tag: isize) -> Option<DayGroup> {
    match tag {
        0 => Some(DayGroup::Today),
        1 => Some(DayGroup::Yesterday),
        2 => Some(DayGroup::Earlier),
        _ => None,
    }
}

pub(super) fn day_group(ts: Option<u64>) -> DayGroup {
    let Some(ts) = ts else {
        return DayGroup::Earlier;
    };
    // `day_no` grows with the calendar, so the difference has to be now-minus-then: a day that
    // came BEFORE today is `+1`, not `-1` (the inverted form made `Yesterday` unreachable and
    // filed every entry older than today under `Earlier`).
    match day_no(now_secs()) - day_no(ts) {
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

/// The configured image-row height as a multiple of the text row (1..=3 from
/// `clipboard.image_rows`; anything else collapses to 1 so a bad value cannot stretch the list).
pub(crate) fn image_row_multiplier() -> f64 {
    let n = CONFIG.read().unwrap().clipboard.image_rows;
    (n.clamp(1, 3)) as f64
}

/// The row height an entry takes at a given `image_rows` multiplier, with the configured value
/// injected so callers (and tests) never depend on a global read.
pub(crate) fn row_content_h_for(entry: &ClipEntry, image_rows: f64) -> f64 {
    if entry.image.is_some() {
        ROW_H * image_rows
    } else {
        ROW_H
    }
}

/// The row content height, shared by layout and row views. Image entries occupy
/// `image_rows` text-row heights; text rows are always one.
pub(super) fn row_content_h(entry: &ClipEntry) -> f64 {
    row_content_h_for(entry, image_row_multiplier())
}

/// Compute the per-row pitches. **Every entry keeps ONE fixed pitch** = content + row gap;
/// a group-header height is inserted before the first row of each group (computed on the
/// FILTERED display order, so time boundaries stay correct under filtering).
pub(super) fn compute_pitches(texts: &[ClipEntry]) -> Vec<f64> {
    compute_pitches_for(texts.iter())
}

/// The same pitches over the entries as they are actually displayed: callers must pass the
/// FILTERED sequence, because image rows' height differs from text rows -- a pitch computed
/// over the full history no longer aligns with display index `i` once a filter drops rows.
pub(super) fn compute_pitches_for<'a>(entries: impl Iterator<Item = &'a ClipEntry>) -> Vec<f64> {
    let mut prev: Option<DayGroup> = None;
    entries
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

/// How much of the pitch sequence fits a list budget, floored at one row: image rows can
/// occupy several text rows, so counting `budget / ROW_H` rows would overflow the panel.
/// Returns (row count, height consumed).
pub(crate) fn list_fill_for_budget(pitches: &[f64], budget: f64) -> (usize, f64) {
    let mut used = 0.0;
    let mut count = 0usize;
    for pitch in pitches {
        if count > 0 && used + pitch > budget {
            break;
        }
        used += pitch;
        count += 1;
    }
    (count.max(1), used)
}

/// The fixed header strip's height: top padding + the search row + the gap under it (the mock's
/// `.panel-head` 16/16/10 padding; the filters chip row is gone, its filters live in the
/// options popover). Derived from the same constants the frames use, so a padding change cannot
/// leave the strip and its controls disagreeing.
pub(super) fn header_strip_h() -> f64 {
    TOP_PAD_Y + SEARCH_H + SEARCH_GAP_Y
}

/// The search field and the four header controls side by side in the header strip (flipped
/// coordinates, measured from the strip's top-left): right-to-left close, pin, detail-toggle
/// and the options orb -- the mock's `.panel-head` (`.orb`/`#detailToggle`/`.close-btn`).
/// One source of truth so no two can ever overlap: the field gives up every button's size plus
/// every gap on the right. Returns `(search, options, detail, pin, close)`, laid out left to right.
pub(super) fn picker_header_frames() -> (NSRect, NSRect, NSRect, NSRect, NSRect) {
    picker_header_frames_for(PICKER_W)
}

pub(super) fn picker_header_frames_for(width: f64) -> (NSRect, NSRect, NSRect, NSRect, NSRect) {
    let close = NSRect::new(
        NSPoint::new(
            width - SEARCH_PAD_X - PICKER_CLOSE_BTN_SIZE,
            TOP_PAD_Y + (SEARCH_H - PICKER_CLOSE_BTN_SIZE) / 2.0,
        ),
        NSSize::new(PICKER_CLOSE_BTN_SIZE, PICKER_CLOSE_BTN_SIZE),
    );
    let pin = NSRect::new(
        NSPoint::new(
            close.origin.x - PICKER_CLOSE_BTN_GAP - PICKER_ORB_SIZE,
            TOP_PAD_Y + (SEARCH_H - PICKER_ORB_SIZE) / 2.0,
        ),
        NSSize::new(PICKER_ORB_SIZE, PICKER_ORB_SIZE),
    );
    let detail = NSRect::new(
        NSPoint::new(
            pin.origin.x - PICKER_CLOSE_BTN_GAP - PICKER_ORB_SIZE,
            pin.origin.y,
        ),
        pin.size,
    );
    let options = NSRect::new(
        NSPoint::new(
            detail.origin.x - PICKER_CLOSE_BTN_GAP - PICKER_ORB_SIZE,
            detail.origin.y,
        ),
        detail.size,
    );
    // Four controls and four gaps are reserved on the right, so the options orb can never be
    // drawn over the query/⌘F keycap however long the widest locale's hint is.
    let search = NSRect::new(
        NSPoint::new(SEARCH_PAD_X, TOP_PAD_Y),
        NSSize::new(
            width
                - SEARCH_PAD_X * 2.0
                - (PICKER_ORB_SIZE + PICKER_CLOSE_BTN_GAP) * 3.0
                - PICKER_CLOSE_BTN_SIZE
                - PICKER_CLOSE_BTN_GAP,
            SEARCH_H,
        ),
    );
    (search, options, detail, pin, close)
}

/// The panel's top drag band: the header strip's top padding across the panel's full width
/// (flipped coordinates, measured from the strip's top-left). It covers no control -- the search
/// row starts below it -- so a press here always starts a panel drag (see `drag.rs`). Pure so a
/// test can prove the band adds no height and never reaches the search row.
///
/// This is only the *reported* band (the A2 scenario's start area and the smoke runner's press
/// point): the live gesture accepts any blank region of the panel, not just this one.
pub(super) fn picker_drag_band_frame() -> NSRect {
    NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(PICKER_W, TOP_PAD_Y))
}

/// Reserve enough height for the complete options menu below the header, including clear-all
/// when enabled, so an empty or short history cannot clip its final actions.
pub(super) fn picker_min_height() -> f64 {
    let menu_min = POPOVER_TOP
        + super::detail::filter_popover_fixed_h()
        + (CLIP_FILTER_ROWS.len() + 3) as f64 * POPOVER_MIN_ROW_H
        + POPOVER_BOTTOM_INSET;
    (header_strip_h() + GROUP_H + ROW_H * 3.0 + FOOTER_H + PAD_Y)
        .max(PICKER_MIN_HEIGHT)
        .max(menu_min)
}

/// The row list's top offset INSIDE the document: just the gap to the header strip (the strip is no
/// longer inside the scroll area, so no 38pt clearance is needed -- that left the odd blank band
/// between the first row and the search field).
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

/// Copy timestamp -> "YYYY-MM-DD HH.MM.SS" (local time; the save-as suggested-filename suffix).
/// The format itself lives in `ffi` so the settings export's filename stamp is the same string; the
/// format is unit-tested.
pub(super) fn format_save_stamp(unix_secs: u64) -> String {
    crate::ffi::format_local_timestamp(unix_secs)
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
/// The scope is what the user can see: the active filter's category narrowed by the search query
/// (`filtered_indices`). Clearing only what is on screen keeps a destructive action from deleting
/// entries the user never saw. Pinned entries survive every clear except the explicit "clear all"
/// (`clear_all`), which is opt-in through a setting and still limited to the visible scope.
pub(super) fn remove_history_scope(
    history: &mut Vec<ClipEntry>,
    clear_all: bool,
    filter: ClipFilter,
    query: &str,
) -> Vec<ClipEntry> {
    let visible: HashSet<usize> = filtered_indices(history, query, filter)
        .into_iter()
        .collect();
    remove_scope_by_index(history, &visible, clear_all)
}

/// The one removal primitive every destructive clear funnels through: drop the entries whose
/// history index is in `scope`, keeping pinned entries unless `clear_all` asks for them.
fn remove_scope_by_index(
    history: &mut Vec<ClipEntry>,
    scope: &HashSet<usize>,
    clear_all: bool,
) -> Vec<ClipEntry> {
    let mut removed = Vec::new();
    let mut kept = Vec::with_capacity(history.len());
    for (index, entry) in history.drain(..).enumerate() {
        if scope.contains(&index) && (clear_all || !entry.pinned) {
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

/// The history indices a day-group clear would take: the entries of `group` that the active filter
/// and query actually show. The group NARROWS the visible scope -- it never widens it -- so
/// clearing "today" while the Image filter is on cannot delete the day's hidden text entries.
/// Pure, unit-tested.
pub(super) fn day_group_scope_indices(
    history: &[ClipEntry],
    filter: ClipFilter,
    query: &str,
    group: DayGroup,
) -> Vec<usize> {
    filtered_indices(history, query, filter)
        .into_iter()
        .filter(|&index| day_group(history[index].copied_at) == group)
        .collect()
}

/// Remove one day group's visible entries and return them for the image-cache sweep. Same pinned
/// rule as the plain scope clear: a pinned entry survives unless `clear_all` asks for it.
pub(super) fn remove_day_group_scope(
    history: &mut Vec<ClipEntry>,
    clear_all: bool,
    filter: ClipFilter,
    query: &str,
    group: DayGroup,
) -> Vec<ClipEntry> {
    let scope: HashSet<usize> = day_group_scope_indices(history, filter, query, group)
        .into_iter()
        .collect();
    remove_scope_by_index(history, &scope, clear_all)
}

/// The header's two clear actions. The first ("clear") sweeps the visible scope but never takes
/// pinned entries, and its name follows what it keeps safe: a query is active -> "clear current";
/// a category is selected -> "clear this category"; otherwise the plain "clear". The second
/// ("clear all", shown only when the user opts in through the settings switch) also takes the
/// pinned entries of the visible scope, so its name states the wider reach. One place so the
/// button titles and the behaviour cannot drift apart.
/// Pure, unit-tested.
pub(super) fn clip_clear_labels(filter: ClipFilter, has_query: bool) -> (String, String) {
    let clear = if has_query {
        t("clipboard.clear_button_query")
    } else if filter == ClipFilter::All {
        t("clipboard.clear_button")
    } else {
        t("clipboard.clear_button_category")
    };
    let clear_all = if has_query {
        t("clipboard.clear_scope_all_results")
    } else if filter == ClipFilter::All {
        t("clipboard.clear_confirm_all")
    } else {
        let kind: String = match filter {
            ClipFilter::All => String::new(),
            ClipFilter::Text => t("clipboard.filter_text"),
            ClipFilter::Image => t("clipboard.filter_image"),
            ClipFilter::Link => t("clipboard.filter_link"),
            ClipFilter::Code => t("clipboard.filter_code"),
            ClipFilter::Pinned => t("clipboard.filter_pinned"),
        };
        tf("clipboard.clear_scope_all", &[("kind", kind.as_str())])
    };
    (clear, clear_all)
}

/// The width a clear action shows right now: its CURRENT label measured on the caption font plus
/// a small click slack. The button hugs its text and resizes (and the right-aligned pair
/// repositions) whenever the filter, the query or the locale changes the label.
/// `measure` is injected (the header and the locale refresh both pass `localized_string_width`)
/// so the rule itself is pure and testable.
/// Pure, unit-tested.
pub(super) fn clip_clear_action_width(label: &str, measure: impl Fn(&str) -> f64) -> f64 {
    measure(label) + CLEAR_ACTION_CLICK_SLACK
}

/// Horizontal click slack added around a clear action's measured text width.
pub(super) const CLEAR_ACTION_CLICK_SLACK: f64 = 8.0;

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

/// Picker filters: All / Text / Image / Link / Code / Pinned (the mock's `.filter-row` set).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClipFilter {
    All,
    Text,
    Image,
    Link,
    Code,
    Pinned,
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

/// The Tab filter cycle: All -> Text -> Image -> Link -> Code -> Pinned -> All.
pub(super) fn next_clip_filter(filter: ClipFilter) -> ClipFilter {
    match filter {
        ClipFilter::All => ClipFilter::Text,
        ClipFilter::Text => ClipFilter::Image,
        ClipFilter::Image => ClipFilter::Link,
        ClipFilter::Link => ClipFilter::Code,
        ClipFilter::Code => ClipFilter::Pinned,
        ClipFilter::Pinned => ClipFilter::All,
    }
}

/// The options popover's filter rows, in display order (`.popover .filter-row`).
/// Consumed by the popover in step C; pinned by unit tests today.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) const CLIP_FILTER_ROWS: [ClipFilter; 6] = [
    ClipFilter::All,
    ClipFilter::Text,
    ClipFilter::Image,
    ClipFilter::Link,
    ClipFilter::Code,
    ClipFilter::Pinned,
];

/// Whether an entry matches the filter (cached-classification variant): avoids re-running
/// classify_text for every entry on every keystroke.
fn matches_filter_kind(e: &ClipEntry, kind: TextKind, f: ClipFilter) -> bool {
    match f {
        ClipFilter::All => true,
        ClipFilter::Image => e.image.is_some(),
        ClipFilter::Text => e.image.is_none() && kind == TextKind::Plain,
        ClipFilter::Link => e.image.is_none() && kind == TextKind::Url,
        ClipFilter::Code => e.image.is_none() && kind == TextKind::Code,
        ClipFilter::Pinned => e.pinned,
    }
}

/// Per-filter-row counts for the options popover (the `.filter-row .count` column), in
/// `CLIP_FILTER_ROWS` order.
pub(super) fn filter_row_counts(history: &[ClipEntry]) -> [usize; 6] {
    let mut counts = [0usize; 6];
    for entry in history {
        let kind = classify_text(&entry.text);
        for (i, f) in CLIP_FILTER_ROWS.iter().enumerate() {
            if matches_filter_kind(entry, kind, *f) {
                counts[i] += 1;
            }
        }
    }
    counts
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
