//! 剪贴板子系统 · pasteboard:剪
//! 贴

use super::*;

// ========== 剪贴板读写 / pasteboard I/O ==========

/// 读当前剪贴板纯文本(无文本返回 None)。
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

/// 预览最长边上限(px):缩略图 ~64pt 显示,480px 足够;原图再大,内存里也只留
/// 这个小预览——原始字节落盘不入内存。
/// Preview max edge (px): thumbnails display at ~64pt, 480px is plenty; no matter the
/// source size, only this small preview stays in memory -- the original bytes live on
/// disk.
pub(super) const PREVIEW_MAX_DIM: f64 = 480.0;

/// 把任意图片字节解码成**降采样** PNG 预览(缩略图用;解码失败返回 None)。
/// 动图(GIF/WebP)只取第一帧;超过 PREVIEW_MAX_DIM 的原图按比例缩小再编码。
/// Decode arbitrary image bytes into a DOWNSAMPLED PNG preview (for the thumbnail; None
/// on failure). Animations (GIF/WebP) yield their first frame; sources larger than
/// PREVIEW_MAX_DIM are scaled down proportionally before encoding.
/// 图片字节 → 降采样 PNG 预览(最长边 ≤ max_dim)。与缩略图绘制同款缩放管线。
/// Image bytes -> a downsampled PNG (longest edge <= max_dim). Same scaling pipeline as the
/// thumbnail drawing.
pub(super) unsafe fn any_image_to_scaled_png(bytes: &[u8], max_dim: f64) -> Option<Vec<u8>> {
    // NSImage -> (必要时 lockFocus 缩放)-> TIFFRepresentation -> NSBitmapImageRep ->
    // PNG(4)。与缩略图绘制同款缩放管线。
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
    // 需要降采样才画进缩放目标图;小图直接用原图,省一次重绘。
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

/// 图片字节 → 缩略图预览 PNG(最长边 ≤ PREVIEW_MAX_DIM)。/ Bytes -> thumbnail PNG (<= 480px).
pub(super) unsafe fn any_image_to_preview_png(bytes: &[u8]) -> Option<Vec<u8>> {
    any_image_to_scaled_png(bytes, PREVIEW_MAX_DIM)
}

/// 剪贴板图片类型探测优先级(load-bearing,顺序不可随意改):
/// **动图原格式(GIF/WebP)最优先**——应用复制动图时剪贴板上常有"原始动图字节 +
/// 静态重编码(PNG/JPEG/TIFF)"多份并存,必须取原始动图那份,否则历史里就是静态图,
/// Option+V 粘出去不再动(系统 Cmd+V 却能粘出动图)。
/// 静态格式按保真度排:PNG(无损)> JPEG(有损)> HEIC > BMP;TIFF 垫底——它是
/// macOS 各 App 复制图片时几乎都会附带的通用兜底(NSImagePboardType),且是静态的。
/// 命中某类型但解码不出预览时继续探测下一个(同图往往还有 TIFF 可解码)。
///
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

/// 从剪贴板实际存在的类型里,按 PASTEBOARD_IMAGE_UTIS 优先级挑出要用的那一个。
/// 纯函数,便于单测(顺序与 GIF 别名都在这锁定)。
/// Pick the preferred UTI from the types actually present on the pasteboard, following
/// PASTEBOARD_IMAGE_UTIS' priority. Pure, unit-tested (the order and the GIF alias are
/// pinned here).
pub(super) fn preferred_uti(present: &[&str]) -> Option<&'static str> {
    PASTEBOARD_IMAGE_UTIS
        .iter()
        .find(|uti| present.contains(uti))
        .copied()
}

/// 敏感/临时剪贴板标记(nspasteboard.org "Securing Copy" 协议):带这些标记的内容
/// **绝不记录进历史**(内存与磁盘都不会)——密码管理器(1Password 等)复制密码时会
/// 打上 ConcealedType,让剪贴板历史应用跳过。与 Maccy 的处理一致。
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

/// 自家粘贴写回的标记类型:paste_at 写回内容后打上它,轮询据此识别"这是我们的
/// 写回"而非用户的新复制。`clipboard.move_used_to_top` 关闭时,带此标记的
/// changeCount 变化被跳过(粘贴不重排历史);真实复制会 clearContents 清掉标记,
/// 不受影响。与 Maccy 的 `org.p0deje.Maccy` 标记同款做法,对其它应用无害。
/// The marker type for our own paste write-backs: `paste_at` stamps it after writing the
/// content back, so the poll can tell "this is OUR write-back" apart from a genuine new
/// copy. When `clipboard.move_used_to_top` is off, a changeCount bump carrying this
/// marker is skipped (pasting does not reorder the history); a real copy clears the
/// pasteboard and thus the marker, so it is never affected. Same approach as Maccy's
/// `org.p0deje.Maccy` marker; harmless to other apps.
pub(super) const PASTE_MARKER_TYPE: &str = "org.oh-my-tab.paste";

/// 剪贴板是否带自家粘贴标记(stringForType: 非空即命中)。
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

/// 给剪贴板打上自家粘贴标记(写回内容之后调用)。
/// Stamp the pasteboard with our own paste marker (called after a write-back).
pub(super) unsafe fn stamp_paste_marker(pb: *mut AnyObject) {
    let type_ns = make_nsstring(PASTE_MARKER_TYPE);
    let v = make_nsstring("1");
    let _: bool = msg_send![pb, setString: v, forType: type_ns];
    CFRelease(type_ns as *const c_void);
    CFRelease(v as *const c_void);
}

/// 当前是否"使用后移到最前"(从 CONFIG 实时读,设置保存后立即生效)。/// Whether used entries move to the top (read live from CONFIG; takes effect on the
/// next poll after settings are saved).
pub(super) fn move_used_to_top() -> bool {
    CONFIG
        .read()
        .map(|c| c.clipboard.move_used_to_top)
        .unwrap_or(true)
}

/// 是否应跳过本次 changeCount 变化:关闭"使用后移到最前"且剪贴板带自家粘贴标记
/// (即本次变化是我们自己的写回,不是用户的新复制)。纯函数,便于单测。
/// Whether this changeCount bump should be skipped: "move used to top" is off AND the
/// pasteboard carries our paste marker (the change is our own write-back, not a new
/// copy). Pure, unit-tested.
pub(super) fn should_skip_paste_writeback(toggle: bool, has_marker: bool) -> bool {
    !toggle && has_marker
}

/// 剪贴板是否携带敏感标记(availableTypeFromArray: 一次性探测,存在即返回该类型)。
/// Whether the pasteboard carries a sensitive marker (probed in one
/// availableTypeFromArray: call).
pub(super) unsafe fn pasteboard_has_sensitive_marker() -> bool {
    let pb: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
    if pb.is_null() {
        return false;
    }
    // 必须 alloc+init(owned +1),`[NSArray array]` 是 +0 自动释放对象,CFRelease
    // 会过度释放直接崩溃。
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

/// 读当前剪贴板图片:原样取原始格式字节 → 算 hash 落盘 → 派生降采样 PNG 预览
/// (无图片/无法解码/缓存写入失败返回 None)。
/// Read the pasteboard's image: the original-format bytes verbatim -> hashed and written
/// to the disk cache -> a downsampled PNG preview (None when absent, undecodable, or the
/// cache write fails).
pub(super) unsafe fn read_pasteboard_image() -> Option<ImageEntry> {
    let pb: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
    if pb.is_null() {
        return None;
    }
    // NSData -> 字节:dataForType: 返回 NSData,取 bytes/length 拷进 Rust Vec。
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
    // 先收集剪贴板上实际存在的类型(按优先级序),再逐个尝试:优先挑 GIF/WebP 等
    // 原始格式;选中类型解码/落盘失败则试下一个(同图往往还有 TIFF 可解码)。
    // Collect the types actually present (in priority order), then try them one by one:
    // animation-capable originals win; a type whose data fails to decode or cache is
    // skipped (the same image is usually also available as TIFF).
    let mut present: Vec<&str> = PASTEBOARD_IMAGE_UTIS
        .iter()
        .copied()
        .filter(|uti| bytes_for_type(uti).is_some())
        .collect();
    while let Some(uti) = preferred_uti(&present) {
        present.retain(|u| *u != uti);
        let data = bytes_for_type(uti).unwrap();
        let Some(preview_png) = any_image_to_preview_png(&data) else {
            continue;
        };
        let hash = fnv1a64(&data);
        // 缓存写失败则本条目不收(粘贴时将无字节可写回,等于坏条目)。
        // A failed cache write drops the entry (nothing to paste back later).
        if !cache_write_image(hash, &data) {
            continue;
        }
        // 预览单独落盘:持久化历史重启加载时直接读回,无需重新解码。
        // The preview is persisted separately so a persisted history loads without
        // re-decoding after a restart.
        let _ = cache_write_preview(hash, &preview_png);
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

/// 文件扩展名 → 剪贴板 UTI 映射(图片类型清单的唯一来源,测试同步覆盖)。
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

/// 文件扩展名是否为图片类型(小写匹配)。/ Whether a file extension denotes an image.
#[cfg(test)]
pub(super) fn is_image_extension(path: &str) -> bool {
    ext_to_uti(path).is_some()
}

/// 剪贴板是否携带文件复制标记(public.file-url 存在)。文件复制(含多文件)时,
/// 剪贴板文本只是文件名(列表),绝不能按普通文本记录。
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
    // stringForType: 返回 autoreleased 对象,无需手动 release(与 file_copy_image 一致)。
    // stringForType: returns an autoreleased object; no manual release (same as file_copy_image).
    !url_str_obj.is_null()
}

/// 图片文件复制(Finder 里 Cmd+C 一个图片文件):剪贴板上只有文件名文本 + 一个
/// `public.file-url`。识别条件:file-url 存在,且文本恰好等于该文件的文件名——这时按
/// "文件复制"处理:**读一次文件内容(瞬时)**,算内容哈希 + 解码首帧生成缩略图预览,
/// 然后**丢弃字节**(不写数据缓存、无影子副本——磁盘/内存零驻留,粘贴仍走 file-url
/// 引用语义,与 Windows Win+V / Maccy 一致)。内容哈希用于**同内容去重**:原文件与
/// 它在访达里的副本(不同路径、同样字节)只保留一条。
/// 粘贴时恢复 `public.file-url`,应用按需读原文件;源文件被删/移动后该条目粘贴即
/// 失效(无影子副本,这是本设计的取舍)。行内显示缩略图预览;text 存文件名,可搜索。
/// 读取失败 → None(走原文本逻辑);解码失败(损坏/伪扩展名)→ 退化为纯引用条目
/// (hash=0、无预览,粘贴仍可用)。
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
    // 文本必须等于文件名:否则是普通文本复制(碰巧带了 file-url)。
    // The text must equal the file's name: otherwise it is a normal text copy that happens
    // to carry a file-url.
    let name = path.rsplit('/').next().unwrap_or("");
    if name != text {
        return None;
    }
    let uti = ext_to_uti(&path)?;
    // 读一次文件内容(瞬时,不入内存驻留):内容哈希 = 同内容去重键,首帧 = 缩略图。
    // Read the file once (transient): the content hash is the content-dedup key, the first
    // frame becomes the thumbnail.
    let bytes = std::fs::read(&path).ok()?;
    let hash = fnv1a64(&bytes);
    let preview_png = unsafe { any_image_to_preview_png(&bytes) }.unwrap_or_default();
    // 预览落盘({hash}.preview):持久化历史重启加载时直接读回,无需重新解码。
    // The preview is persisted ({hash}.preview) so a persisted history loads without
    // re-decoding after a restart.
    if !preview_png.is_empty() {
        let _ = cache_write_preview(hash, &preview_png);
    }
    Some(ImageEntry {
        uti: uti.to_string(),
        hash,
        data_path: std::path::PathBuf::new(),
        preview_png,
        source_path: Some(path),
    })
}
/// 把文本写回剪贴板(粘贴路径)。写回会 bump changeCount,下次轮询读到的是本文本,
/// 但 record_text 的去重(与栈顶相同)会忽略它,不会产生重复条目;并打上自家
/// 粘贴标记(供"使用后移到最前"关闭时跳过记录)。
/// Write text back to the pasteboard (the paste path). This bumps changeCount; the next
/// poll reads this same text, but record_text's dedup (same as the top entry) skips it.
/// The own-paste marker is stamped too (so the poll can skip the change when "move used
/// entries to top" is off).
pub(super) unsafe fn write_pasteboard_text(text: &str, stamp_marker: bool) {
    let pb: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
    if pb.is_null() {
        return;
    }
    // 标准写入流程:先 clearContents 声明所有权,再 setString——单独调用 setString
    // 在某些场景会返回 NO(实测曾失败,导致 Cmd+V 粘贴的是剪贴板旧内容)。
    // Standard write flow: clearContents first to take ownership, then setString -- calling
    // setString alone returned NO in practice (the Cmd+V then pasted the OLD clipboard
    // content). clearContents returns NSInteger (the new changeCount).
    let _: isize = msg_send![pb, clearContents];
    let type_ns = make_nsstring(NSPASTEBOARD_TYPE_STRING);
    let ns = make_nsstring(text);
    let ok: bool = msg_send![pb, setString: ns, forType: type_ns];
    // 粘贴回写路径打 marker(防轮询把粘贴动作当新复制重新入史/置顶);
    // **用户手动复制所选**不打——那是一次真实复制,应当正常入史。
    // The paste write-back stamps the marker (the poll must not re-record the paste as a
    // fresh copy / reorder history); a USER-INITIATED selection copy does NOT stamp it --
    // it is a genuine copy that should enter the history normally.
    if stamp_marker {
        stamp_paste_marker(pb);
    }
    // 日志只打元数据,绝不打剪贴板内容(隐私:内容可能是密码/正文)。
    // Log metadata only, NEVER the clipboard text (privacy: it may be a password/body text).
    log_debug!(
        "[clip] write back {} chars (setString ok={}, stamp={})",
        text.chars().count(),
        ok,
        stamp_marker
    );
    CFRelease(type_ns as *const c_void);
    CFRelease(ns as *const c_void);
}

/// 把图片按**原始格式**写回剪贴板(图片粘贴路径):先 clearContents 再 setData,
/// UTI 用条目保存的原始类型——JPG 粘回 JPG,GIF 动图粘回动图,不再统一转 PNG。
/// 原始字节在粘贴瞬间从磁盘缓存读回(不入内存驻留);缓存缺失(被清)返回 false,
/// 调用方应跳过合成 Cmd+V,避免把旧剪贴板内容粘出去。
/// Write an image back to the pasteboard in its ORIGINAL format (the image paste path).
/// Same clearContents then setData flow; the UTI is the entry's original type -- a JPG
/// pastes back as JPG, an animated GIF as a GIF, never a blanket PNG re-encode. The
/// original bytes are read back from the disk cache at paste time (never held in memory);
/// a cache miss returns false and the caller must skip the synthesized Cmd+V so the OLD
/// pasteboard content is not pasted.
pub(super) unsafe fn write_pasteboard_image(entry: &ImageEntry) -> bool {
    let Some(data) = cache_read_image(entry.hash) else {
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
    stamp_paste_marker(pb);
    log_debug!(
        "[clip] write back image ({} bytes, uti={}, setData ok={})",
        data.len(),
        entry.uti,
        ok
    );
    CFRelease(type_ns as *const c_void);
    ok
}

/// 把文件复制写回剪贴板(文件复制的粘贴路径):恢复 `public.file-url` + 文件名文本,
/// 与 Finder 原生文件复制一致——粘贴进 Finder 复制原文件(GIF 等格式原封不动)、
/// 粘贴进聊天应用附加文件;而非把图片数据当纯图片粘贴(Finder 会忽略,部分应用
/// 还会重编码成 PNG)。
/// Write a file copy back to the pasteboard (the file-copy paste path): restore
/// `public.file-url` + the filename text, matching Finder's native file copy -- pasting
/// into Finder duplicates the original file (GIF etc. untouched), pasting into a chat app
/// attaches the file; instead of pasting image data as a bare image (which Finder ignores
/// and some apps re-encode into PNG).
pub(super) unsafe fn write_pasteboard_file(path: &str) {
    let pb: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
    if pb.is_null() {
        return;
    }
    let _: isize = msg_send![pb, clearContents];
    // 文件名文本(与 Finder 复制文件时剪贴板上的字符串一致)。
    // The filename text (same string Finder puts on the pasteboard for a file copy).
    let name = path.rsplit('/').next().unwrap_or("");
    let name_ns = make_nsstring(name);
    let type_ns = make_nsstring(NSPASTEBOARD_TYPE_STRING);
    let _: bool = msg_send![pb, setString: name_ns, forType: type_ns];
    CFRelease(type_ns as *const c_void);
    CFRelease(name_ns as *const c_void);
    // file:// URL(file-url + url 两种类型都写,兼容不同读取方)。
    // The file:// URL (written as both file-url and url for reader compatibility).
    let path_ns = make_nsstring(path);
    let url: *mut AnyObject = msg_send![class!(NSURL), fileURLWithPath: path_ns];
    CFRelease(path_ns as *const c_void);
    if url.is_null() {
        return;
    }
    let abs: *mut AnyObject = msg_send![url, absoluteString];
    let url_str = nsstring_to_rust(abs);
    let url_str_ns = make_nsstring(&url_str);
    for uti in [NSPASTEBOARD_TYPE_FILE_URL, NSPASTEBOARD_TYPE_URL] {
        let type_ns = make_nsstring(uti);
        let _: bool = msg_send![pb, setString: url_str_ns, forType: type_ns];
        CFRelease(type_ns as *const c_void);
    }
    CFRelease(url_str_ns as *const c_void);
    stamp_paste_marker(pb);
    log_debug!("[clip] write back file");
}
