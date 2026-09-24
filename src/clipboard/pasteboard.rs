//! Clipboard subsystem · pasteboard: pasteboard reads and writes.

use super::*;

/// Read the pasteboard's plain text (None when no text).
pub(super) unsafe fn read_pasteboard_text() -> Option<String> {
    let pb: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
    if pb.is_null() {
        return None;
    }
    let type_ns = make_nsstring(NSPASTEBOARD_TYPE_STRING);
    let s: *mut AnyObject = msg_send![pb, stringForType: type_ns];
    CFRelease(type_ns as *const c_void);
    if s.is_null() {
        return None;
    }
    Some(nsstring_to_rust(s))
}

/// Preview max edge (px): thumbnails display at ~64pt, 480px is plenty; no matter the
/// source size, only this small preview stays in memory -- the original bytes live on
/// disk.
pub(super) const PREVIEW_MAX_DIM: f64 = 480.0;

/// Decode arbitrary image bytes into a DOWNSAMPLED PNG preview (for the thumbnail; None
/// on failure). Animations (GIF/WebP) yield their first frame; sources larger than
/// PREVIEW_MAX_DIM are scaled down proportionally before encoding.
/// Image bytes -> a downsampled PNG (longest edge <= max_dim). Same scaling pipeline as the
/// thumbnail drawing.
pub(super) unsafe fn any_image_to_scaled_png(bytes: &[u8], max_dim: f64) -> Option<Vec<u8>> {
    // NSImage -> (lockFocus scale when needed) -> TIFFRepresentation -> NSBitmapImageRep ->
    // PNG (4). The same scaling pipeline as the thumbnail drawing.
    let data: *mut AnyObject = msg_send![
        class!(NSData),
        dataWithBytes: bytes.as_ptr() as *const c_void,
        length: bytes.len()
    ];
    let img: *mut AnyObject = msg_send![class!(NSImage), alloc];
    let img: *mut AnyObject = msg_send![img, initWithData: data];
    if img.is_null() {
        return None;
    }
    let src_size: NSSize = msg_send![img, size];
    let (w, h) = (src_size.width, src_size.height);
    // Only draw into a scaled target when downsampling is needed; small sources are used
    // as-is, skipping the extra pass.
    let source: *mut AnyObject = if w > max_dim || h > max_dim {
        let scale = (max_dim / w).min(max_dim / h);
        let (tw, th) = (w * scale, h * scale);
        let target: *mut AnyObject = msg_send![class!(NSImage), alloc];
        let target: *mut AnyObject = msg_send![target, initWithSize: NSSize::new(tw, th)];
        let _: () = msg_send![target, lockFocus];
        let dst = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(tw, th));
        let src = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
        let op: usize = 1; // NSCompositingOperationCopy
        let _: () = msg_send![
            img,
            drawInRect: dst,
            fromRect: src,
            operation: op,
            fraction: 1.0f64
        ];
        let _: () = msg_send![target, unlockFocus];
        target
    } else {
        img
    };
    let tiff: *mut AnyObject = msg_send![source, TIFFRepresentation];
    if tiff.is_null() {
        return None;
    }
    let rep: *mut AnyObject = msg_send![class!(NSBitmapImageRep), imageRepWithData: tiff];
    if rep.is_null() {
        return None;
    }
    let png: *mut AnyObject =
        msg_send![rep, representationUsingType: 4u64, properties: std::ptr::null::<AnyObject>()];
    if png.is_null() {
        return None;
    }
    let len: usize = msg_send![png, length];
    let ptr: *const c_void = msg_send![png, bytes];
    if ptr.is_null() || len == 0 {
        return None;
    }
    Some(std::slice::from_raw_parts(ptr as *const u8, len).to_vec())
}

/// Bytes -> thumbnail PNG (<= 480px).
pub(super) unsafe fn any_image_to_preview_png(bytes: &[u8]) -> Option<Vec<u8>> {
    any_image_to_scaled_png(bytes, PREVIEW_MAX_DIM)
}

/// Pasteboard image type probe order (load-bearing; do NOT reorder casually):
/// **animation-capable originals (GIF/WebP) first** -- when an app copies an animated
/// GIF, the pasteboard usually carries BOTH the original animated bytes AND a static
/// re-encode (PNG/JPEG/TIFF); we must take the animated original, otherwise the history
/// holds a static frame and our Option+V paste stops animating (while the system Cmd+V
/// still pastes the animation from the untouched pasteboard). Static formats follow in
/// fidelity order: PNG (lossless) > JPEG (lossy) > HEIC > BMP; TIFF is last -- it is the
/// generic static fallback almost every macOS app carries (NSImagePboardType). A type
/// whose data fails to decode as a preview is skipped (the same image is usually also
/// available as TIFF).
pub(super) const PASTEBOARD_IMAGE_UTIS: &[&str] = &[
    NSPASTEBOARD_TYPE_GIF,
    NSPASTEBOARD_TYPE_GIF_ALIAS,
    NSPASTEBOARD_TYPE_WEBP,
    NSPASTEBOARD_TYPE_PNG,
    NSPASTEBOARD_TYPE_JPEG,
    NSPASTEBOARD_TYPE_HEIC,
    NSPASTEBOARD_TYPE_BMP,
    NSPASTEBOARD_TYPE_TIFF,
];

/// Pick the preferred UTI from the types actually present on the pasteboard, following
/// PASTEBOARD_IMAGE_UTIS' priority. Pure, unit-tested (the order and the GIF alias are
/// pinned here).
pub(super) fn preferred_uti(present: &[&str]) -> Option<&'static str> {
    PASTEBOARD_IMAGE_UTIS
        .iter()
        .find(|uti| present.contains(uti))
        .copied()
}

/// Sensitive/transient pasteboard markers (the nspasteboard.org "Securing Copy"
/// protocol): content carrying these markers is NEVER recorded (not in memory, not on
/// disk) -- password managers (1Password et al.) stamp ConcealedType when copying
/// passwords so clipboard historians skip them. Same handling as Maccy.
pub(super) const SENSITIVE_PASTEBOARD_TYPES: &[&str] = &[
    "org.nspasteboard.TransientType",
    "org.nspasteboard.ConcealedType",
    "org.nspasteboard.AutoGeneratedType",
    "com.agilebits.onepassword",
];

/// The marker type for our own paste write-backs: `paste_at` stamps it after writing the
/// content back, so the poll can tell "this is OUR write-back" apart from a genuine new
/// copy. When `clipboard.move_used_to_top` is off, a changeCount bump carrying this
/// marker is skipped (pasting does not reorder the history); a real copy clears the
/// pasteboard and thus the marker, so it is never affected. Same approach as Maccy's
/// `org.p0deje.Maccy` marker; harmless to other apps.
pub(super) const PASTE_MARKER_TYPE: &str = "org.oh-my-tab.paste";

/// Whether the pasteboard carries our own paste marker (stringForType: non-nil).
pub(super) unsafe fn pasteboard_has_paste_marker() -> bool {
    let pb: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
    if pb.is_null() {
        return false;
    }
    let type_ns = make_nsstring(PASTE_MARKER_TYPE);
    let s: *mut AnyObject = msg_send![pb, stringForType: type_ns];
    CFRelease(type_ns as *const c_void);
    !s.is_null()
}

/// Stamp the pasteboard with our own paste marker (called after a write-back).
pub(super) unsafe fn stamp_paste_marker(pb: *mut AnyObject) {
    let type_ns = make_nsstring(PASTE_MARKER_TYPE);
    let v = make_nsstring("1");
    let _: bool = msg_send![pb, setString: v, forType: type_ns];
    CFRelease(type_ns as *const c_void);
    CFRelease(v as *const c_void);
}

/// next poll after settings are saved).
pub(super) fn move_used_to_top() -> bool {
    CONFIG
        .read()
        .map(|c| c.clipboard.move_used_to_top)
        .unwrap_or(true)
}

/// Whether "delete after paste" is on (read live from CONFIG; takes effect immediately
/// after settings are saved).
pub(super) fn delete_after_paste() -> bool {
    CONFIG
        .read()
        .map(|c| c.clipboard.delete_after_paste)
        .unwrap_or(false)
}

/// Whether to clear the current system pasteboard after a one-shot paste (depends on
/// delete_after_paste).
pub(super) fn clear_system_pasteboard_after_paste() -> bool {
    CONFIG
        .read()
        .map(|c| c.clipboard.delete_after_paste && c.clipboard.clear_system_pasteboard_after_paste)
        .unwrap_or(false)
}

/// Whether this changeCount bump should be skipped: "move used to top" is off AND the
/// pasteboard carries our paste marker (the change is our own write-back, not a new
/// copy). Pure, unit-tested.
pub(super) fn should_skip_paste_writeback(toggle: bool, has_marker: bool) -> bool {
    !toggle && has_marker
}

/// Whether the pasteboard carries a sensitive marker (probed in one
/// availableTypeFromArray: call).
pub(super) unsafe fn pasteboard_has_sensitive_marker() -> bool {
    let pb: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
    if pb.is_null() {
        return false;
    }
    // Must use alloc+init (owned, +1): `[NSArray array]` returns a +0 autoreleased
    // object, and CFRelease on it over-releases and crashes.
    let array: *mut AnyObject = msg_send![class!(NSMutableArray), alloc];
    let array: *mut AnyObject = msg_send![array, init];
    for t in SENSITIVE_PASTEBOARD_TYPES {
        let t_ns = make_nsstring(t);
        let _: () = msg_send![array, addObject: t_ns];
        CFRelease(t_ns as *const c_void);
    }
    let hit: *mut AnyObject = msg_send![pb, availableTypeFromArray: array];
    CFRelease(array as *const c_void);
    !hit.is_null()
}

/// Read the pasteboard's image: the original-format bytes verbatim -> hashed and written
/// to the disk cache -> a downsampled PNG preview (None when absent, undecodable, or the
/// cache write fails).
pub(super) unsafe fn read_pasteboard_image() -> Option<ImageEntry> {
    let pb: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
    if pb.is_null() {
        return None;
    }
    // NSData -> bytes: dataForType: returns NSData; grab bytes/length into a Rust Vec.
    let bytes_for_type = |t: &str| -> Option<Vec<u8>> {
        let type_ns = make_nsstring(t);
        let data: *mut AnyObject = msg_send![pb, dataForType: type_ns];
        CFRelease(type_ns as *const c_void);
        if data.is_null() {
            return None;
        }
        let len: usize = msg_send![data, length];
        let ptr: *const c_void = msg_send![data, bytes];
        if ptr.is_null() || len == 0 {
            return None;
        }
        Some(std::slice::from_raw_parts(ptr as *const u8, len).to_vec())
    };
    let has_type = |types: *mut AnyObject, t: &str| -> bool {
        if types.is_null() {
            return false;
        }
        let type_ns = make_nsstring(t);
        let present: bool = msg_send![types, containsObject: type_ns];
        CFRelease(type_ns as *const c_void);
        present
    };
    // Fetch the pasteboard's type array ONCE (previously re-fetched for every candidate UTI).
    let types: *mut AnyObject = msg_send![pb, types];
    // Collect the types actually present (in priority order), then try them one by one:
    // animation-capable originals win; a type whose data fails to decode is skipped (the
    // same image is usually also available as TIFF).
    let mut present: Vec<&str> = PASTEBOARD_IMAGE_UTIS
        .iter()
        .copied()
        .filter(|uti| has_type(types, uti))
        .collect();
    while let Some(uti) = preferred_uti(&present) {
        present.retain(|u| *u != uti);
        // A type advertised in `types` can still yield nil from dataForType: (lazy/promised
        // data); skip to the next candidate instead of panicking.
        let Some(data) = bytes_for_type(uti) else {
            continue;
        };
        let Some(preview_png) = any_image_to_preview_png(&data) else {
            continue;
        };
        let hash = fnv1a64(&data);
        let preview_png = Arc::new(preview_png);
        // Delegate the disk writes (original bytes + preview) to a background thread: copying
        // a large image no longer blocks the main thread on I/O. The bytes stay in the PENDING
        // map until written, and paste/save-as fall back to them on a cache miss, so the async
        // write never breaks functionality.
        schedule_image_cache_write(hash, Some(Arc::new(data)), preview_png.clone(), None, true);
        return Some(ImageEntry {
            uti: uti.to_string(),
            hash,
            data_path: clip_image_path(hash),
            preview_png,
            source_path: None,
        });
    }
    None
}

/// File extension -> pasteboard UTI mapping (the single image-format list; tests cover it).
pub(super) fn ext_to_uti(path: &str) -> Option<&'static str> {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some(NSPASTEBOARD_TYPE_PNG),
        "jpg" | "jpeg" => Some(NSPASTEBOARD_TYPE_JPEG),
        "gif" => Some(NSPASTEBOARD_TYPE_GIF),
        "tiff" | "tif" => Some(NSPASTEBOARD_TYPE_TIFF),
        "webp" => Some(NSPASTEBOARD_TYPE_WEBP),
        "heic" | "heif" => Some(NSPASTEBOARD_TYPE_HEIC),
        "bmp" => Some(NSPASTEBOARD_TYPE_BMP),
        _ => None,
    }
}

/// Whether a file extension denotes an image.
#[cfg(test)]
pub(super) fn is_image_extension(path: &str) -> bool {
    ext_to_uti(path).is_some()
}

/// Whether the pasteboard carries a file-copy marker (public.file-url present). On a
/// file copy (including multi-file selections) the text is just the filename(s) and
/// must never be recorded as plain text.
pub(super) unsafe fn pasteboard_has_file_url() -> bool {
    let pb: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
    if pb.is_null() {
        return false;
    }
    let url_type = make_nsstring("public.file-url");
    let url_str_obj: *mut AnyObject = msg_send![pb, stringForType: url_type];
    CFRelease(url_type as *const c_void);
    // stringForType: returns an autoreleased object; no manual release (same as file_copy_image).
    !url_str_obj.is_null()
}

/// An image-FILE copy (Cmd+C on an image file in Finder): the pasteboard carries only the
/// filename as text plus a `public.file-url`. Recognition: a file-url exists AND the text
/// is exactly that file's name -- then it is a FILE copy: the file is read ONCE
/// (transiently) to compute a content hash and decode a first-frame thumbnail preview,
/// then the bytes are DISCARDED (no data-cache write, no shadow copy -- nothing held on
/// disk or in RAM; pasting still restores `public.file-url`, the reference semantics of
/// Windows Win+V / Maccy). The content hash enables CONTENT dedup: a file and its Finder
/// duplicate (different paths, identical bytes) collapse into one entry. Pasting restores
/// `public.file-url` and the target app reads the original file on demand; a deleted or
/// moved source makes the entry unpastable (no shadow copy -- the accepted tradeoff). The
/// row shows the thumbnail preview; `text` holds the filename so entries are searchable.
/// A read failure -> None (fall back to the text path); a decode failure (corrupt file /
/// fake extension) degrades to a reference-only entry (hash=0, no preview, still
/// pasteable). Non-image files / text/name mismatch / multiple files -> None.
pub(super) unsafe fn file_copy_image(text: &str) -> Option<ImageEntry> {
    let pb: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
    if pb.is_null() {
        return None;
    }
    let url_type = make_nsstring("public.file-url");
    let url_str_obj: *mut AnyObject = msg_send![pb, stringForType: url_type];
    CFRelease(url_type as *const c_void);
    if url_str_obj.is_null() {
        return None;
    }
    let url: *mut AnyObject = msg_send![class!(NSURL), URLWithString: url_str_obj];
    if url.is_null() {
        return None;
    }
    let path_obj: *mut AnyObject = msg_send![url, path];
    if path_obj.is_null() {
        return None;
    }
    let path = nsstring_to_rust(path_obj);
    // The text must equal the file's name: otherwise it is a normal text copy that happens
    // to carry a file-url.
    let name = path.rsplit('/').next().unwrap_or("");
    if name != text {
        return None;
    }
    let uti = ext_to_uti(&path)?;
    // Read the file once (transient): the content hash is the content-dedup key, the first
    // frame becomes the thumbnail.
    let bytes = std::fs::read(&path).ok()?;
    let hash = fnv1a64(&bytes);
    let preview_png = Arc::new(unsafe { any_image_to_preview_png(&bytes) }.unwrap_or_default());
    // The preview is persisted ({hash}.preview) via the same background thread (preview only;
    // the original bytes stay uncached per the file-reference semantics).
    schedule_image_cache_write(hash, None, preview_png.clone(), Some(path.clone()), false);
    Some(ImageEntry {
        uti: uti.to_string(),
        hash,
        data_path: std::path::PathBuf::new(),
        preview_png,
        source_path: Some(path),
    })
}
/// Write text back to the pasteboard (the paste path). This bumps changeCount; the next
/// poll reads this same text, but record_text's dedup (same as the top entry) skips it.
/// The own-paste marker is stamped too (so the poll can skip the change when "move used
/// entries to top" is off).
pub(super) unsafe fn write_pasteboard_text(text: &str, stamp_marker: bool) -> bool {
    let pb: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
    if pb.is_null() {
        return false;
    }
    // Standard write flow: clearContents first to take ownership, then setString -- calling
    // setString alone returned NO in practice (the Cmd+V then pasted the OLD clipboard
    // content). clearContents returns NSInteger (the new changeCount).
    let _: isize = msg_send![pb, clearContents];
    let type_ns = make_nsstring(NSPASTEBOARD_TYPE_STRING);
    let ns = make_nsstring(text);
    let ok: bool = msg_send![pb, setString: ns, forType: type_ns];
    // The paste write-back stamps the marker (the poll must not re-record the paste as a
    // fresh copy / reorder history); a USER-INITIATED selection copy does NOT stamp it --
    // it is a genuine copy that should enter the history normally.
    if ok && stamp_marker {
        stamp_paste_marker(pb);
    }
    // Log metadata only, NEVER the clipboard text (privacy: it may be a password/body text).
    log_debug!(
        "[clip] write back {} chars (setString ok={}, stamp={})",
        text.chars().count(),
        ok,
        stamp_marker
    );
    CFRelease(type_ns as *const c_void);
    CFRelease(ns as *const c_void);
    ok
}

/// Write an image back to the pasteboard in its ORIGINAL format (the image paste path).
/// Same clearContents then setData flow; the UTI is the entry's original type -- a JPG
/// pastes back as JPG, an animated GIF as a GIF, never a blanket PNG re-encode. The
/// original bytes are read back from the disk cache at paste time (never held in memory);
/// a cache miss returns false and the caller must skip the synthesized Cmd+V so the OLD
/// pasteboard content is not pasted.
pub(super) unsafe fn write_pasteboard_image(entry: &ImageEntry) -> bool {
    // Read the on-disk cache first; while the background write is still queued, fall back to
    // the in-memory pending bytes -- the async cache write must not break paste-right-after-copy.
    let Some(data) = image_bytes_for_hash(entry.hash) else {
        log_info!(
            "[clip] image cache miss on paste (hash={:016x}, uti={})",
            entry.hash,
            entry.uti
        );
        return false;
    };
    let pb: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
    if pb.is_null() {
        return false;
    }
    let _: isize = msg_send![pb, clearContents];
    let type_ns = make_nsstring(&entry.uti);
    let data_obj: *mut AnyObject = msg_send![
        class!(NSData),
        dataWithBytes: data.as_ptr() as *const c_void,
        length: data.len()
    ];
    let ok: bool = msg_send![pb, setData: data_obj, forType: type_ns];
    if ok {
        stamp_paste_marker(pb);
    }
    log_debug!(
        "[clip] write back image ({} bytes, uti={}, setData ok={})",
        data.len(),
        entry.uti,
        ok
    );
    CFRelease(type_ns as *const c_void);
    ok
}

/// Write a file copy back to the pasteboard (the file-copy paste path): restore
/// `public.file-url` + the filename text, matching Finder's native file copy -- pasting
/// into Finder duplicates the original file (GIF etc. untouched), pasting into a chat app
/// attaches the file; instead of pasting image data as a bare image (which Finder ignores
/// and some apps re-encode into PNG).
pub(super) unsafe fn write_pasteboard_file(path: &str) -> bool {
    let pb: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
    if pb.is_null() {
        return false;
    }
    let _: isize = msg_send![pb, clearContents];
    // The filename text (same string Finder puts on the pasteboard for a file copy).
    let name = path.rsplit('/').next().unwrap_or("");
    let name_ns = make_nsstring(name);
    let type_ns = make_nsstring(NSPASTEBOARD_TYPE_STRING);
    let name_ok: bool = msg_send![pb, setString: name_ns, forType: type_ns];
    CFRelease(type_ns as *const c_void);
    CFRelease(name_ns as *const c_void);
    // The file:// URL (written as both file-url and url for reader compatibility).
    let path_ns = make_nsstring(path);
    let url: *mut AnyObject = msg_send![class!(NSURL), fileURLWithPath: path_ns];
    CFRelease(path_ns as *const c_void);
    if url.is_null() {
        return false;
    }
    let abs: *mut AnyObject = msg_send![url, absoluteString];
    if abs.is_null() {
        return false;
    }
    let url_str = nsstring_to_rust(abs);
    let url_str_ns = make_nsstring(&url_str);
    let mut urls_ok = true;
    for uti in [NSPASTEBOARD_TYPE_FILE_URL, NSPASTEBOARD_TYPE_URL] {
        let type_ns = make_nsstring(uti);
        let ok: bool = msg_send![pb, setString: url_str_ns, forType: type_ns];
        urls_ok &= ok;
        CFRelease(type_ns as *const c_void);
    }
    CFRelease(url_str_ns as *const c_void);
    let ok = name_ok && urls_ok;
    if ok {
        stamp_paste_marker(pb);
    }
    log_debug!(
        "[clip] write back file (name ok={}, urls ok={})",
        name_ok,
        urls_ok
    );
    ok
}
