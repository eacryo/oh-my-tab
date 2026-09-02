//! 剪贴板子系统 · image_cache:图
//! 片

use super::*;

// ========== 图片磁盘缓存 / image disk cache ==========

/// 图片字节缓存目录:原始格式字节全部落盘,内存只留降采样预览;持久化关闭时启动清空,
/// 持久化开启时按历史引用扫描孤儿文件。测试构建使用专用目录,绝不触碰真实缓存。
/// The image-byte cache directory: original-format bytes live on disk and memory only keeps
/// the downsampled preview; persistence-off startup wipes it, while persistence-on startup
/// sweeps unreferenced files. Test builds use a dedicated directory, never the real cache.
pub(super) fn clip_image_cache_dir() -> std::path::PathBuf {
    // 冒烟模式(--smoke-clipboard)走专用目录:真实二进制运行时 cfg!(test) 不生效,
    // 不隔离就会把注入的测试条目写进用户的真实历史/缓存(曾污染真实 history 文件)。
    // Smoke mode (--smoke-clipboard) uses a dedicated dir: the smoke runs the REAL binary,
    // so cfg!(test) is off -- without this, injected test entries used to land in the
    // user's real history/cache.
    let name = if SMOKE_MODE.load(Ordering::SeqCst) {
        format!("oh-my-tab-clip-images-smoke-{}", std::process::id())
    } else if cfg!(test) {
        format!(
            "oh-my-tab-clip-images-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        )
    } else {
        "oh-my-tab-clip-images".to_string()
    };

    if SMOKE_MODE.load(Ordering::SeqCst) || cfg!(test) {
        // 测试/冒烟数据从 HOME 改放系统临时目录:Codex 沙箱只允许写工作区和临时目录,
        // 而 $HOME/Library/Caches 可能可读但不可写;否则缓存测试会在 create_dir_all/
        // rename 处失败。进程+线程后缀保留并行测试隔离,也避免复用上次运行的残留目录。
        // Keep test/smoke data under the system temp directory instead of HOME: the Codex
        // sandbox allows writes in the workspace and temp directories, but may allow reads
        // while denying writes under $HOME/Library/Caches. Otherwise cache tests fail at
        // create_dir_all/rename. Process+thread suffixes preserve parallel-test isolation
        // and avoid reusing a directory from an earlier process.
        return std::env::temp_dir().join(name);
    }

    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    std::path::PathBuf::from(format!("{}/Library/Caches/{}", home, name))
}

/// hash → 缓存文件路径(与 fnv1a64 输出同构的 hex)。/ hash -> cache file path.
pub(super) fn clip_image_path(hash: u64) -> std::path::PathBuf {
    clip_image_cache_dir().join(format!("{hash:016x}"))
}

/// 把原始字节写入缓存(幂等:同 hash 已存在则跳过)。/ Write bytes into the cache.
pub(super) fn cache_write_image(hash: u64, bytes: &[u8]) -> bool {
    let dir = clip_image_cache_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let path = clip_image_path(hash);
    if path.exists() {
        return true;
    }
    // 临时文件 + rename,避免写一半被粘贴路径读到。
    // Write to a temp file then rename, so the paste path never reads a half-written file.
    let tmp = dir.join(format!("{hash:016x}.tmp"));
    let ok = std::fs::write(&tmp, bytes).is_ok() && std::fs::rename(&tmp, &path).is_ok();
    if !ok {
        let _ = std::fs::remove_file(&tmp);
    }
    ok
}

/// 从缓存读回原始字节(不存在/读失败返回 None)。/ Read the original bytes back.
pub(super) fn cache_read_image(hash: u64) -> Option<Vec<u8>> {
    std::fs::read(clip_image_path(hash)).ok()
}

/// 删除一个缓存文件(数据字节 + 预览一并删除)。/ Delete a cache file (data + preview).
pub(super) fn cache_delete_image(hash: u64) {
    let _ = std::fs::remove_file(clip_image_path(hash));
    let _ = std::fs::remove_file(clip_image_preview_path(hash));
    let _ = std::fs::remove_file(clip_image_detail_path(hash));
}

/// hash → 预览文件路径(缩略图单独落盘,重启加载历史时不必重新解码)。
/// hash -> the preview file path (the thumbnail is persisted separately so loading the
/// history after a restart needs no re-decoding).
pub(super) fn clip_image_preview_path(hash: u64) -> std::path::PathBuf {
    clip_image_cache_dir().join(format!("{hash:016x}.preview"))
}

/// 把预览 PNG 写入缓存(幂等)。/ Write the preview PNG into the cache (idempotent).
pub(super) fn cache_write_preview(hash: u64, preview: &[u8]) -> bool {
    let dir = clip_image_cache_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let path = clip_image_preview_path(hash);
    if path.exists() {
        return true;
    }
    let tmp = dir.join(format!("{hash:016x}.preview.tmp"));
    let ok = std::fs::write(&tmp, preview).is_ok() && std::fs::rename(&tmp, &path).is_ok();
    if !ok {
        let _ = std::fs::remove_file(&tmp);
    }
    ok
}

/// 读回预览 PNG(缺失返回 None,调用方从数据字节重新生成)。
/// Read the preview back (None when missing; the caller regenerates from the data bytes).
pub(super) fn cache_read_preview(hash: u64) -> Option<Vec<u8>> {
    std::fs::read(clip_image_preview_path(hash)).ok()
}

/// hash → 详情预览文件路径(→ 展开详情的大图;录制时后台预生成,首开 miss 兜底)。
/// hash -> the detail-preview path (the big image shown by the → detail panel;
/// pregenerated in the background at record time, with a first-open fallback).
pub(super) fn clip_image_detail_path(hash: u64) -> std::path::PathBuf {
    clip_image_cache_dir().join(format!("{hash:016x}.detail"))
}

/// 读回详情预览(缺失返回 None)。/ Read the detail preview back (None when missing).
pub(super) fn cache_read_detail_preview(hash: u64) -> Option<Vec<u8>> {
    std::fs::read(clip_image_detail_path(hash)).ok()
}

/// 把详情预览 PNG 写入缓存(幂等)。/ Write the detail preview PNG into the cache (idempotent).
pub(super) fn cache_write_detail_preview(hash: u64, png: &[u8]) -> bool {
    let dir = clip_image_cache_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let path = clip_image_detail_path(hash);
    if path.exists() {
        return true;
    }
    let tmp = dir.join(format!("{hash:016x}.detail.tmp"));
    let ok = std::fs::write(&tmp, png).is_ok() && std::fs::rename(&tmp, &path).is_ok();
    if !ok {
        let _ = std::fs::remove_file(&tmp);
    }
    ok
}

/// 详情预览后台任务的时效判定(纯函数,单测覆盖):仅当详情可见**且**当前选中
/// 条目就是任务条目时,生成结果才值得刷新到 UI。预生成任务(录制时投递)不做
/// 此检查——它们只为落盘缓存,与选中态无关。
/// Freshness predicate for background detail-preview jobs (pure; unit-tested): the
/// result is worth swapping into the UI only when the detail panel is visible AND the
/// currently selected entry IS the job's entry. Pregen jobs (enqueued at record time)
/// skip this check -- they only populate the disk cache and never touch the selection.
pub(super) fn detail_result_still_wanted(
    detail_visible: bool,
    current_hash: Option<u64>,
    job_hash: u64,
) -> bool {
    detail_visible && current_hash == Some(job_hash)
}

/// 当前选中图片条目的 hash(工作线程也会调用;全部经 Mutex,跨线程安全)。
/// 无选中 / 条目非图片 → None。
/// The selected image entry's hash (also called from the worker thread; everything goes
/// through Mutexes, so this is thread-safe). None without a selection / for text entries.
pub(super) fn detail_current_hash() -> Option<u64> {
    let sel = *PICKER_SELECTION.lock().unwrap();
    if sel == NO_SELECTION {
        return None;
    }
    let h_idx = mapped_index(sel)?;
    let hist = CLIP_HISTORY.lock().unwrap();
    hist.get(h_idx)
        .and_then(|e| e.image.as_ref())
        .map(|i| i.hash)
}

/// 后台详情预览任务:从数据缓存/源文件生成 ≤1280px 的 `{hash}.detail` 并原子落盘。
/// deliver = true 表示任务源自"首开 miss"(完成后需尝试刷新 UI);false = 录制/
/// 加载预热(只落盘,不动 UI)。
/// A background detail-preview job: generate the <=1280px `{hash}.detail` from the data
/// cache / source file and write it atomically. deliver = true means the job originated
/// from a first-open miss (try to refresh the UI afterwards); false = record/load warm-up
/// (cache only, no UI interaction).
pub(super) struct DetailPreviewJob {
    hash: u64,
    source_path: Option<String>,
    deliver: bool,
}

/// 详情预览工作线程的发件端(懒启动常驻循环)。flume recv 阻塞等待;线程随进程
/// 退出,无需停机协议(tmp+rename 原子写,中断无害)。
/// The sender side of the detail-preview worker (a lazily started persistent loop).
//  flume recv blocks between jobs; the thread dies with the process (no shutdown
//  protocol needed -- tmp+rename writes are atomic and interruption-safe).
pub(super) fn detail_job_sender() -> flume::Sender<DetailPreviewJob> {
    static SENDER: OnceLock<flume::Sender<DetailPreviewJob>> = OnceLock::new();
    SENDER
        .get_or_init(|| {
            let (tx, rx) = flume::unbounded::<DetailPreviewJob>();
            // 线程名带模块前缀,便于日志/调试器识别。
            // The thread name carries the module prefix for logs/debuggers.
            std::thread::Builder::new()
                .name("clip-detail-preview".into())
                .spawn(move || {
                    for job in rx.iter() {
                        unsafe { run_detail_preview_job(&job) };
                    }
                })
                .expect("spawn clip-detail-preview worker");
            tx
        })
        .clone()
}

/// 工作线程单任务处理:幂等跳过(缓存已存在)→ 按需任务的取件时效检查 →
/// autoreleasepool 内生成 + 原子写盘 → deliver 任务经静态槽 + 主线程回调刷新。
/// One worker iteration: idempotent skip (cache exists) -> dequeue freshness check for
/// on-demand jobs -> generate inside an autoreleasepool + atomic cache write -> deliver
/// jobs stash the bytes and hop to the main thread.
pub(super) unsafe fn run_detail_preview_job(job: &DetailPreviewJob) {
    // 幂等:预生成与按需请求撞车时,先到者已写盘,后来者直接跳过。
    // Idempotent: when pregen and on-demand requests race, whoever lands first writes
    // the file and the other skips.
    if clip_image_detail_path(job.hash).exists() {
        DETAIL_INFLIGHT.lock().unwrap().remove(&job.hash);
        return;
    }
    // 取件时效:按需任务出队时用户可能已经 ↑↓ 切走——跳过省一次解码+编码。
    // 最终防线仍在主线程回调(生成期间也可能切走)。
    // Dequeue freshness: the user may have arrowed away before an on-demand job starts --
    // skipping saves a decode+encode. The final guard stays in the main-thread callback
    // (the user can also navigate away mid-generation).
    if job.deliver
        && !detail_result_still_wanted(
            DETAIL_VISIBLE.load(Ordering::SeqCst),
            detail_current_hash(),
            job.hash,
        )
    {
        DETAIL_INFLIGHT.lock().unwrap().remove(&job.hash);
        return;
    }
    // AppKit 临时对象(NSImage/TIFF/PNG 编码产物)随池回收——与图标提取的
    // "后台线程 + autoreleasepool"先例同款;若实测不稳,备选改纯 CoreGraphics
    // (CGImageSourceCreateThumbnailAtIndex,线程绝对安全)。
    // AppKit temporaries (NSImage/TIFF/PNG encodes) drain with the pool -- same
    // precedent as icon extraction's "background thread + autoreleasepool"; if this ever
    // proves unstable, switch to pure CoreGraphics (CGImageSourceCreateThumbnailAtIndex,
    // unconditionally thread-safe).
    let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
    let png = generate_detail_preview_bytes(job.hash, job.source_path.as_deref());
    let _: () = msg_send![pool, drain];
    if let Some(png) = png {
        if job.deliver {
            // 先写槽再跳主线程:handler/show_detail_for_sel 消费槽位时有完整数据。
            // Stash before hopping to the main thread: the handler / show_detail_for_sel
            // sees complete bytes when consuming the slot.
            *DETAIL_PENDING_HD.lock().unwrap() = Some((job.hash, png));
            let target = observer();
            let _: () = msg_send![
                target,
                performSelectorOnMainThread: sel!(detailPreviewReady:),
                withObject: std::ptr::null_mut::<AnyObject>(),
                waitUntilDone: false
            ];
        }
    }
    DETAIL_INFLIGHT.lock().unwrap().remove(&job.hash);
}

/// 从数据缓存/源文件生成详情预览字节并落盘(原 ensure_detail_preview 的生成半段;
/// 只做纯 IO + 解码编码,不触碰任何 UI 静态,可在任意线程执行)。退化 hash=0 不写
/// 缓存避免孤儿文件。
/// Generate the detail-preview bytes from the data cache / source file and cache them
/// (the generation half of the old ensure_detail_preview; pure IO + decode/encode with no
/// UI statics touched -- safe on any thread). Degenerate hash=0 skips the cache write to
/// avoid orphan files.
pub(super) unsafe fn generate_detail_preview_bytes(
    hash: u64,
    source_path: Option<&str>,
) -> Option<Vec<u8>> {
    let bytes = match source_path {
        None => cache_read_image(hash),
        Some(p) => std::fs::read(p).ok(),
    };
    let bytes = bytes?;
    let png = any_image_to_scaled_png(&bytes, DETAIL_PREVIEW_MAX_DIM)?;
    if hash != 0 {
        cache_write_detail_preview(hash, &png);
    }
    Some(png)
}

/// 投递详情预览生成任务(hash=0 的退化条目不入队)。在途集合去重;入队失败
/// (线程异常终止)回滚在途标记。
/// Enqueue a detail-preview generation job (degenerate hash=0 entries are never queued).
//  Deduplicated via the in-flight set; an enqueue failure (dead worker) rolls the marker
//  back.
pub(super) fn request_detail_preview(img: &ImageEntry, deliver: bool) {
    if img.hash == 0 {
        return;
    }
    {
        let mut inflight = DETAIL_INFLIGHT.lock().unwrap();
        if !inflight.insert(img.hash) {
            return;
        }
    }
    if detail_job_sender()
        .send(DetailPreviewJob {
            hash: img.hash,
            source_path: img.source_path.clone(),
            deliver,
        })
        .is_err()
    {
        DETAIL_INFLIGHT.lock().unwrap().remove(&img.hash);
    }
}

/// 为图片条目取详情展示字节(同步三态,**绝不阻塞**):
/// ① 后台刚生成的单槽命中 → 消费清空;
/// ② `{hash}.detail` 磁盘缓存 → 同步读(毫秒级);
/// ③ 内存 480px 预览立即返回,同时投递后台生成(deliver=true),完成后由
///    detail_preview_ready 触发重建升级为高清。
/// 字节都不可得(None)→ 调用方走文件名文本回退。
///
/// Fetch the detail display bytes for an image entry (synchronous three states, NEVER
/// blocking): 1) a hit in the freshly-generated slot -> consume and clear it; 2) the
/// `{hash}.detail` disk cache -> a millisecond-scale read; 3) return the in-memory 480px
/// preview right away while enqueueing background generation (deliver=true); completion
/// triggers a rebuild through detail_preview_ready to upgrade to hi-res. None (no bytes
//  at all) lets the caller fall back to the filename text.
pub(super) fn ensure_detail_preview(img: &ImageEntry) -> Option<Vec<u8>> {
    {
        let mut slot = DETAIL_PENDING_HD.lock().unwrap();
        if let Some((h, _)) = slot.as_ref() {
            if *h == img.hash {
                return slot.take().map(|(_, p)| p);
            }
        }
    }
    if let Some(png) = cache_read_detail_preview(img.hash) {
        return Some(png);
    }
    if !img.preview_png.is_empty() {
        request_detail_preview(img, true);
        return Some(img.preview_png.clone());
    }
    None
}

/// hash 是否仍被幸存条目引用。文件条目与数据条目同内容(同 hash)可以共存并共享
/// 磁盘缓存(`{hash}` 数据字节 + `{hash}.preview`),所以删除条目时**必须先查引用**,
/// 否则会误删幸存者的文件——数据条目会永久失去粘贴字节。
/// Whether `hash` is still referenced by the surviving entries. A file entry and a data
/// entry with identical content (same hash) coexist and SHARE the disk cache (`{hash}`
/// data bytes + `{hash}.preview`), so deletion MUST check references first -- an unguarded
/// delete would wipe a survivor's files (a data entry would lose its paste bytes forever).
pub(super) fn hash_referenced_by<'a>(
    mut survivors: impl Iterator<Item = &'a ClipEntry>,
    hash: u64,
) -> bool {
    survivors.any(|e| e.image.as_ref().is_some_and(|i| i.hash == hash))
}

/// 删除一个条目时清理它的缓存文件(数据字节 + 预览一并删除),但**仅当该 hash 不再
/// 被任何幸存条目引用**;退化条目(hash=0)无文件可删。
/// Delete a removed entry's cache files (data bytes + preview together), but ONLY when
/// the hash is no longer referenced by any surviving entry; a degenerate entry (hash=0)
/// has no files.
pub(super) fn cache_delete_for_removed(history: &[ClipEntry], removed: &ClipEntry) {
    let Some(img) = &removed.image else {
        return;
    };
    if img.hash != 0 && !hash_referenced_by(history.iter(), img.hash) {
        cache_delete_image(img.hash);
    }
}

/// 清空整个图片缓存目录(启动时调用:历史不持久化,残留文件必为孤儿)。
/// Wipe the whole image cache dir (called at startup: the history is not persisted, so
/// any leftover file is an orphan).
pub(super) fn clear_clip_image_cache() {
    let dir = clip_image_cache_dir();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// 按当前历史条目清理图片缓存中的孤儿文件。
/// 数据条目需要无后缀原图 + preview/detail;文件引用条目只需要 preview/detail。
/// Sweep orphan image-cache files against the current history. DATA entries may keep the
/// extensionless original plus preview/detail; FILE references may keep preview/detail only.
pub(super) fn sweep_clip_image_cache(history: &[ClipEntry]) -> usize {
    let mut all_hashes = HashSet::new();
    let mut data_hashes = HashSet::new();
    for entry in history {
        let Some(img) = &entry.image else {
            continue;
        };
        if img.hash == 0 {
            continue;
        }
        all_hashes.insert(img.hash);
        if img.source_path.is_none() {
            data_hashes.insert(img.hash);
        }
    }

    let dir = clip_image_cache_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let (stem, is_raw) = if let Some(stem) = name.strip_suffix(".preview") {
            (stem, false)
        } else if let Some(stem) = name.strip_suffix(".detail") {
            (stem, false)
        } else {
            (name, true)
        };
        let keep = u64::from_str_radix(stem, 16).ok().is_some_and(|hash| {
            all_hashes.contains(&hash) && (!is_raw || data_hashes.contains(&hash))
        });
        if !keep && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// 按当前内存历史清理缓存,用于启动和持久化热切换路径。
/// Sweep against the in-memory history for startup and persistence-toggle paths.
pub(super) fn sweep_current_clip_image_cache() -> usize {
    let history = CLIP_HISTORY.lock().unwrap();
    sweep_clip_image_cache(&history)
}
