//! 剪贴板子系统 · persist:历
//! 史

use super::*;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

// ========== 历史持久化 / history persistence ==========

/// 历史文件格式版本(结构变更时递增;加载遇到更高版本时放弃,按空历史启动)。
/// The history file format version (bump on structural changes; a higher version is
/// ignored on load and the app starts with an empty history).
pub(super) const HISTORY_VERSION: u32 = 1;

/// 历史文件包装结构(带版本号,方便将来演进)。
/// The history file wrapper (versioned, for future evolution).
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct HistoryFile {
    version: u32,
    entries: Vec<ClipEntry>,
}

/// 序列化专用的借用视图:避免 `entries.to_vec()` 把整本历史(含图片预览)再深拷一遍
/// ——预览本身被 `#[serde(skip)]`,那次拷贝纯属浪费。
/// A borrow-only view for serialization: avoids `entries.to_vec()` deep-copying the entire
/// history (image previews included) -- the previews are `#[serde(skip)]`, so that copy was
/// pure waste.
#[derive(Serialize)]
struct HistoryFileRef<'a> {
    version: u32,
    entries: &'a [ClipEntry],
}

/// 持久化历史文件路径(与 config.toml 同目录;测试构建走测试目录)。
/// The persisted-history path (same dir as config.toml; test builds use a test dir).
pub(super) fn history_file_path() -> std::path::PathBuf {
    if SMOKE_MODE.load(Ordering::SeqCst) || cfg!(test) {
        // 测试/冒烟历史与图片缓存必须共用同一临时根目录;从 HOME 移出是因为 Codex
        // 沙箱对 $HOME/Library/Caches 的写入受限,而持久化测试会直接创建该目录。
        // Test/smoke history must share the same temp root as the image cache. It is moved
        // out of HOME because the Codex sandbox restricts writes to $HOME/Library/Caches,
        // and persistence tests create this directory directly.
        return clip_image_cache_dir()
            .join("history")
            .join("clipboard-history.toml");
    }

    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    std::path::PathBuf::from(format!("{}/.config/oh-my-tab/clipboard-history.toml", home))
}

/// 当前是否开启历史持久化(从 CONFIG 读)。
/// Whether history persistence is enabled (read from CONFIG).
pub(super) fn persist_enabled() -> bool {
    CONFIG.read().map(|c| c.clipboard.persist).unwrap_or(false)
}

/// 序列化历史(纯函数,便于单测)。/ Serialize the history (pure, unit-tested).
pub(super) fn serialize_history(entries: &[ClipEntry]) -> Option<String> {
    let payload = HistoryFileRef {
        version: HISTORY_VERSION,
        entries,
    };
    toml::to_string(&payload).ok()
}

struct PersistJob {
    generation: u64,
    path: std::path::PathBuf,
    entries: Vec<ClipEntry>,
}

/// 单一后台写线程把连续的快照合并为最后一个,避免复制时阻塞主线程。
/// One serial worker coalesces consecutive snapshots to the newest one, keeping copy events
/// from blocking the main thread on TOML serialization and filesystem I/O.
static PERSIST_SENDER: OnceLock<Sender<PersistJob>> = OnceLock::new();
static PERSIST_GENERATION: AtomicU64 = AtomicU64::new(0);
static PERSIST_IO_LOCK: Mutex<()> = Mutex::new(());
/// worker 已处理到的代数(含被合并丢弃的中间代):测试用它等待异步回写落定。
/// The newest generation the worker has processed (coalesced-away jobs count as
/// processed): tests wait on it so the async writeback settles deterministically.
static PERSIST_DONE_GENERATION: AtomicU64 = AtomicU64::new(0);

fn persist_sender() -> &'static Sender<PersistJob> {
    PERSIST_SENDER.get_or_init(|| {
        let (sender, receiver) = mpsc::channel();
        thread::Builder::new()
            .name("oh-my-tab-clipboard-persist".to_string())
            .spawn(|| persist_worker(receiver))
            .expect("failed to start clipboard persistence worker");
        sender
    })
}

fn persist_worker(receiver: Receiver<PersistJob>) {
    while let Ok(mut job) = receiver.recv() {
        // 连续复制可能在队列里留下多个完整快照,只保留最新快照降低序列化和写盘次数。
        // A burst of copies can queue several full snapshots; keep only the newest to reduce
        // serialization and filesystem work.
        while let Ok(newer) = receiver.try_recv() {
            job = newer;
        }
        let done_generation = job.generation;
        write_history_snapshot(job);
        PERSIST_DONE_GENERATION.store(done_generation, Ordering::Release);
    }
}

/// 测试辅助:等到 worker 处理完当前代数及之前的全部快照。load_history 末尾的
/// save_history 是异步的,若不排空,旧快照可能在测试改写历史文件之后才落盘,
/// 把测试刚写入的新文件覆盖回旧内容(曾经是 flaky 根因:left:3, right:1)。
/// Test helper: wait until the worker has drained every snapshot up to the current
/// generation. The save_history at the end of load_history is asynchronous; without
/// draining, an older snapshot can land AFTER a test rewrites the history file and
/// clobber it with stale content (the former flake: left:3, right:1).
#[cfg(test)]
pub(super) fn flush_persist_worker_for_tests() {
    let target = PERSIST_GENERATION.load(Ordering::Acquire);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while PERSIST_DONE_GENERATION.load(Ordering::Acquire) < target {
        if std::time::Instant::now() > deadline {
            panic!("clipboard persist worker did not drain within 10s");
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

fn write_history_snapshot(job: PersistJob) {
    let Some(text) = serialize_history(&job.entries) else {
        log_info!("Clipboard history save failed: serialize error.");
        return;
    };
    let Some(dir) = job.path.parent() else {
        return;
    };

    // 与关闭 persist 共用同一把 I/O 锁:关闭操作拿锁后删除文件,不会被旧快照写回。
    // Share the I/O lock with the persist-off path so disabling persistence deletes the file
    // after any in-progress write and prevents an older snapshot from being restored.
    let _io = PERSIST_IO_LOCK.lock().unwrap();
    if !persist_enabled() || PERSIST_GENERATION.load(Ordering::Acquire) != job.generation {
        return;
    }
    if std::fs::create_dir_all(dir).is_err() {
        log_info!("Clipboard history save failed: cannot create dir.");
        return;
    }
    let tmp = dir.join(format!(
        "clipboard-history.toml.tmp{}-{}",
        std::process::id(),
        job.generation
    ));
    let ok = std::fs::write(&tmp, text.as_bytes()).is_ok();
    if ok {
        // 权限 600:仅当前用户可读写。
        // Mode 600: owner-only access.
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    // 再次检查代数,丢弃排队期间已经过时的快照,避免旧内容覆盖新内容。
    // Check the generation again before rename so a snapshot invalidated while preparing the
    // file is discarded instead of replacing newer history.
    let current = persist_enabled() && PERSIST_GENERATION.load(Ordering::Acquire) == job.generation;
    let ok = ok && current && std::fs::rename(&tmp, &job.path).is_ok();
    if !ok {
        let _ = std::fs::remove_file(&tmp);
        if current {
            log_info!("Clipboard history save failed: write error.");
        }
        return;
    }
    log_debug!(
        "[clip] history saved asynchronously ({} entries, generation={})",
        job.entries.len(),
        job.generation
    );
}

/// 解析历史文件文本:损坏或版本不匹配 → None(调用方按空历史处理)。
/// Parse the history text: corruption or a version mismatch -> None (the caller treats it
/// as an empty history).
pub(super) fn parse_history(text: &str) -> Option<Vec<ClipEntry>> {
    let file: HistoryFile = toml::from_str(text).ok()?;
    if file.version > HISTORY_VERSION {
        return None;
    }
    Some(file.entries)
}

/// 加载时恢复条目的运行态字段(data_path/preview_png):
/// - 数据条目:数据字节缺失(缓存被清过)→ None(坏条目丢弃);预览缺失 → 从数据
///   字节重新生成并落盘
/// - 文件复制条目:预览从 `{hash}.preview` 读回(缺失 → 从源文件重新生成并落盘,
///   源文件也不在 → 空预览,行内显示文件名);data_path 恒为空(粘贴走 file-url)
/// - 文本条目:原样返回
///
/// Restore a loaded entry's runtime fields (data_path/preview_png) on load:
/// - data entries: a missing data file (the cache was swept) -> None (the broken entry is
///   dropped); a missing preview is regenerated from the data bytes and re-persisted
/// - file-copy entries: the preview is read back from `{hash}.preview` (when missing,
///   regenerated from the source file and re-persisted; when the source is also gone, the
///   preview stays empty and the row shows the filename); data_path is always empty
///   (pasting goes through the file-url)
/// - text entries: returned as-is
pub(super) fn restore_loaded_entry(entry: ClipEntry) -> Option<ClipEntry> {
    let Some(img) = &entry.image else {
        return Some(entry); // 文本条目 / a text entry
    };
    if img.hash == 0 {
        return Some(entry); // 解码失败的退化文件条目(无预览) / a degenerate file entry
    }
    if let Some(path) = &img.source_path {
        // 文件复制条目:预览优先读落盘的 {hash}.preview;缺失则从源文件重生。
        // File-copy entries: the preview comes from the persisted {hash}.preview; when
        // missing it is regenerated from the source file.
        let preview = cache_read_preview(img.hash).unwrap_or_else(|| {
            std::fs::read(path)
                .ok()
                .and_then(|d| unsafe { any_image_to_preview_png(&d) })
                .unwrap_or_default()
        });
        if !preview.is_empty() {
            let _ = cache_write_preview(img.hash, &preview);
        }
        return Some(ClipEntry {
            text: entry.text,
            image: Some(ImageEntry {
                uti: img.uti.clone(),
                hash: img.hash,
                data_path: std::path::PathBuf::new(),
                preview_png: Arc::new(preview),
                source_path: Some(path.clone()),
            }),
            pinned: entry.pinned,
            source_app: entry.source_app,
            source_key: entry.source_key,
            copied_at: entry.copied_at,
        });
    }
    let data = cache_read_image(img.hash)?;
    let preview = cache_read_preview(img.hash)
        .unwrap_or_else(|| unsafe { any_image_to_preview_png(&data) }.unwrap_or_default());
    let _ = cache_write_preview(img.hash, &preview);
    Some(ClipEntry {
        text: entry.text,
        image: Some(ImageEntry {
            uti: img.uti.clone(),
            hash: img.hash,
            data_path: clip_image_path(img.hash),
            preview_png: Arc::new(preview),
            source_path: None,
        }),
        pinned: entry.pinned,
        source_app: entry.source_app,
        source_key: entry.source_key,
        copied_at: entry.copied_at,
    })
}

/// 把当前历史保存到磁盘(仅 persist 开启时;临时文件 + rename 原子写,权限 600)。
/// 内容为明文,隐私风险见 README。
/// Save the current history to disk (only when persist is on; atomic temp+rename, mode
/// 600). Plaintext -- the privacy implications are documented in the README.
pub(super) fn save_history() {
    if !persist_enabled() {
        return;
    }
    let mut hist = CLIP_HISTORY.lock().unwrap();
    // 写盘前清理过期条目:同步减少磁盘文件中的过期条目(内存与持久化同步过期)。
    // Expire before writing: the disk file never keeps expired entries (expiry applies
    // to memory and persistence alike).
    expire_entries(&mut hist, now_secs(), ttl_secs());
    let entries = hist.clone();
    drop(hist);
    let generation = PERSIST_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    let job = PersistJob {
        generation,
        path: history_file_path(),
        entries,
    };
    if persist_sender().send(job).is_err() {
        log_info!("Clipboard history save failed: persistence worker stopped.");
    }
}

/// 从磁盘加载历史并**合并**进当前内存(去重规则复用;置顶条目进置顶区,其余按
/// 文件顺序(旧→新)追加到列表尾部,再按 max_entries 裁剪)。文件缺失/损坏/版本
/// 不匹配 → 记日志,按空历史处理(与 config 同款弹性)。
/// Load the persisted history and MERGE it into the in-memory history (reusing the dedup
/// rules; pinned entries join the pinned block, the rest append in file order (old ->
/// new) at the tail, then trim to max_entries). A missing/corrupt/version-mismatched file
/// is logged and treated as an empty history (config-style resilience).
pub(super) fn load_history() {
    if !persist_enabled() {
        return;
    }
    let path = history_file_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        let removed = sweep_current_clip_image_cache();
        if removed > 0 {
            log_debug!("[clip] swept {} orphan image cache files", removed);
        }
        return; // 文件不存在 = 首次使用 / a missing file = first run
    };
    let Some(entries) = parse_history(&text) else {
        log_info!(
            "Clipboard history load failed (corrupt/version mismatch, starting empty): {}",
            path.display()
        );
        let removed = sweep_current_clip_image_cache();
        if removed > 0 {
            log_debug!("[clip] swept {} orphan image cache files", removed);
        }
        return;
    };
    let mut hist = CLIP_HISTORY.lock().unwrap();
    let max = max_entries();
    let mut history_changed = false;
    // 过期条目直接跳过:不进入内存(磁盘文件随后由 save_history 回写清理)。
    // Expired entries are skipped outright: they never reach memory (the disk file is
    // cleaned up afterwards by the save_history rewrite).
    let ttl = ttl_secs();
    let now = now_secs();
    for entry in entries {
        if ttl.is_some_and(|ttl| {
            !entry.pinned
                && entry
                    .copied_at
                    .is_some_and(|t| now.saturating_sub(t) >= ttl)
        }) {
            continue;
        }
        // 数据条目:数据字节缺失(被清过缓存)→ 丢弃坏条目;预览缺失 → 重新解码。
        // Data entries: a missing data file (cache was swept) drops the broken entry; a
        // missing preview is regenerated from the data bytes.
        let Some(entry) = restore_loaded_entry(entry) else {
            continue;
        };
        // 去重与 record_image 同规则:**按条目类型区分**。数据条目(source_path 恒为
        // None)按内容 hash 判重——此前对所有图片统一按 source_path 比较,数据条目
        // 之间 None==None 互相判重,重启加载时除第一条外全部被丢(缓存残留为证)。
        // 文件条目按内容 hash 判重,退化条目(hash=0)按来源路径。
        // Dedup follows record_image, **split by entry kind**: data entries (source_path
        // is ALWAYS None) dedup by content hash -- comparing every image by source_path
        // used to make data entries dedup against each other (None==None), dropping all
        // but the first on every load (orphan cache files were the evidence). File entries
        // dedup by content hash; degenerate entries (hash=0) by source path.
        let dup = match &entry.image {
            Some(img) if img.source_path.is_some() => hist.iter().any(|e| {
                e.image.as_ref().is_some_and(|i| {
                    i.source_path.is_some()
                        && if img.hash != 0 {
                            i.hash == img.hash
                        } else {
                            i.source_path.as_deref() == img.source_path.as_deref()
                        }
                })
            }),
            Some(img) => hist.iter().any(|e| {
                e.image
                    .as_ref()
                    .is_some_and(|i| i.source_path.is_none() && i.hash == img.hash)
            }),
            None => hist
                .iter()
                .any(|e| e.image.is_none() && e.text == entry.text),
        };
        if dup {
            continue;
        }
        // 置顶条目进置顶区顶部(最新置顶在前),其余追加到列表尾部(旧→新)。
        // Pinned entries join the top of the pinned block (newest first); the rest append
        // at the tail (old -> new).
        if entry.pinned {
            hist.insert(0, entry);
        } else {
            hist.push(entry);
        }
        history_changed = true;
    }
    if hist.len() > max {
        // 被裁条目的缓存文件一并删除——但仅当其 hash 不再被幸存条目引用。
        // Dropped entries' cache files go too -- but only when the hash is no longer
        // referenced by a survivor.
        for dropped in &hist[max..] {
            cache_delete_for_removed(&hist[..max], dropped);
        }
        hist.truncate(max);
        history_changed = true;
    }
    if history_changed {
        super::bump_history_revision();
    }
    let swept = sweep_clip_image_cache(&hist);
    let total = hist.len();
    drop(hist);
    if swept > 0 {
        log_debug!("[clip] swept {} orphan image cache files", swept);
    }
    log_info!("Clipboard history loaded ({} entries).", total);
    // 加载后立刻回写:合并/裁剪/补预览的结果落盘,保证磁盘与内存一致。
    // Rewrite right after loading so the merge/trim/preview-fill result is on disk,
    // keeping disk and memory in sync.
    save_history();
}

/// persist 开关在设置页热切换时的应用规则:
/// - 开启:从磁盘加载并合并进当前内存历史(load_history)
/// - 关闭:删除磁盘历史文件(内存历史保留到本次退出)
///
/// Applied when the persist toggle changes in Settings:
/// - ON: load and merge the persisted history into memory (load_history)
/// - OFF: delete the history file (the in-memory history stays until this session ends)
pub(crate) fn apply_persist_toggle(on: bool) {
    if on {
        load_history();
        schedule_picker_refresh();
    } else {
        PERSIST_GENERATION.fetch_add(1, Ordering::AcqRel);
        let _io = PERSIST_IO_LOCK.lock().unwrap();
        let path = history_file_path();
        if path.exists() {
            let _ = std::fs::remove_file(path);
            log_info!("Clipboard history file removed (persistence off).");
        }
    }
}
