//! 缩略图 · cache:内存 LRU 核心与缓存状态。
//! In-memory LRU core and cache state.

use super::*;

// ========== 内存 LRU(泛型核心,便于无 CG 依赖地测试) ==========

/// 确定性 LRU:读命中提升到队尾,插入超限从队头驱逐并**返回被逐项**(值可能持有
/// CGImageRef 等 +1 资源,由调用方释放)。刻意不用 NSCache——它会在内存压力下
/// 自作主张驱逐(BetterCmdTab #82 的教训),而本应用是常驻 accessory,随机丢帧
/// 表现为浮窗偶发闪图标。
///
/// A deterministic LRU: reads bump to the back, over-limit inserts evict from the
/// front and RETURN the evicted values (they may hold +1 resources like
/// CGImageRefs, released by the caller). Deliberately not NSCache -- it evicts on
/// its own under memory pressure (BetterCmdTab #82's lesson), and random frame
/// loss in a permanent accessory shows up as flickering placeholder icons.
pub(crate) struct Lru<K: Eq + Clone, V: Clone> {
    max_items: usize,
    max_cost: u64,
    pub(super) cost: fn(&V) -> u64,
    total_cost: u64,
    items: VecDeque<(K, V)>, // 队尾 = 最近使用 / back = most recently used
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LruPutOperation {
    Insert,
    Replace,
}

pub(super) struct LruPutResult<K, V> {
    pub(super) key: K,
    pub(super) cost: u64,
    pub(super) operation: LruPutOperation,
    pub(super) replaced_cost: Option<u64>,
    pub(super) replaced: Option<V>,
    pub(super) evicted: Vec<(K, V)>,
    pub(super) count_over_limit: bool,
    pub(super) cost_over_limit: bool,
}

impl<K: Eq + Clone, V: Clone> Lru<K, V> {
    pub(crate) fn new(max_items: usize, max_cost: u64, cost: fn(&V) -> u64) -> Self {
        Self {
            max_items,
            max_cost,
            cost,
            total_cost: 0,
            items: VecDeque::new(),
        }
    }

    /// 读命中:提升最近使用并克隆值(CGImageRef 为浅拷贝,所有权仍在缓存)。
    /// Hit: bumps recency and clones the value (a CGImageRef clone is shallow;
    /// ownership stays with the cache).
    pub(crate) fn get(&mut self, key: &K) -> Option<V> {
        let idx = self.items.iter().position(|(k, _)| k == key)?;
        let (k, v) = self.items.remove(idx).unwrap();
        self.items.push_back((k, v.clone()));
        Some(v)
    }

    /// 只刷新访问次序,不克隆值。用于已经显示且复用的卡片,避免为 CGImageRef 做无意义的浅拷贝。
    /// Touch recency without cloning the value. Used by already-visible reused cards so a
    /// CGImageRef does not incur an unnecessary shallow clone.
    pub(crate) fn touch(&mut self, key: &K) -> bool {
        let Some(idx) = self.items.iter().position(|(k, _)| k == key) else {
            return false;
        };
        let item = self.items.remove(idx).unwrap();
        self.items.push_back(item);
        true
    }

    /// 只读元数据探测:不改变 LRU 次序。新鲜度/目标尺寸检查不能把未渲染条目
    /// 伪装成最近使用；只有真正渲染的 get() 才提升 recency。
    /// Read-only metadata probe that does not alter LRU order. Freshness/target-size
    /// checks must not make an unrendered entry look recently used; only rendering
    /// through get() should bump recency.
    pub(super) fn peek(&self, key: &K) -> Option<V> {
        self.items
            .iter()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, value)| value.clone())
    }

    /// 插入/更新(移到队尾);超出容量或总成本时从头驱逐**旧条目**,驱逐项原样
    /// 返回由调用方释放资源。刚插入的队尾新帧受保护:成本超限只挤旧帧(单帧超
    /// 预算时保留最新、旧帧让路),仅条目数上限能把新帧本身挤掉。
    /// Insert/update (moves to the back); over-capacity OLD entries are evicted
    /// from the front and returned verbatim for the caller to release. The
    /// just-inserted back item is protected: a cost overrun only evicts older
    /// frames (an over-budget single frame keeps the newest and sacrifices the
    /// old), and only the item-count cap can evict the newcomer itself.
    #[cfg(test)]
    pub(super) fn put_detailed(&mut self, key: K, val: V) -> LruPutResult<K, V> {
        self.put_detailed_with_priority(key, val, |_| false)
    }

    pub(super) fn put_detailed_with_priority(
        &mut self,
        key: K,
        val: V,
        is_recent_workset: impl Fn(&K) -> bool,
    ) -> LruPutResult<K, V> {
        let inserted_key = key.clone();
        let inserted_cost = (self.cost)(&val);
        let mut replaced_cost = None;
        let mut replaced = None;
        let operation;
        if let Some(idx) = self.items.iter().position(|(k, _)| *k == key) {
            let (_, old) = self.items.remove(idx).unwrap();
            let old_cost = (self.cost)(&old);
            self.total_cost = self.total_cost.saturating_sub(old_cost);
            replaced_cost = Some(old_cost);
            replaced = Some(old);
            operation = LruPutOperation::Replace;
        } else {
            operation = LruPutOperation::Insert;
        }
        self.total_cost = self.total_cost.saturating_add(inserted_cost);
        self.items.push_back((key, val));
        let count_over_limit = self.items.len() > self.max_items;
        let cost_over_limit = self.total_cost > self.max_cost;
        let mut evicted = Vec::new();
        // 先挤旧帧;队尾新帧只在条目数超限时才参与驱逐(见函数注释)。
        // Evict old frames first; the back item only participates when the item
        // count itself is over the cap (see the fn doc).
        while self.items.len() > 1
            && (self.items.len() > self.max_items || self.total_cost > self.max_cost)
        {
            let Some((evicted_key, v)) =
                self.pop_oldest_eviction_candidate(&is_recent_workset, false)
            else {
                break;
            };
            self.total_cost = self.total_cost.saturating_sub((self.cost)(&v));
            evicted.push((evicted_key, v));
        }
        while self.items.len() > self.max_items {
            let Some((evicted_key, v)) =
                self.pop_oldest_eviction_candidate(&is_recent_workset, true)
            else {
                break;
            };
            self.total_cost = self.total_cost.saturating_sub((self.cost)(&v));
            evicted.push((evicted_key, v));
        }
        LruPutResult {
            key: inserted_key,
            cost: inserted_cost,
            operation,
            replaced_cost,
            replaced,
            evicted,
            count_over_limit,
            cost_over_limit,
        }
    }

    fn pop_oldest_eviction_candidate(
        &mut self,
        is_recent_workset: &impl Fn(&K) -> bool,
        allow_newest: bool,
    ) -> Option<(K, V)> {
        let eligible_end = if allow_newest {
            self.items.len()
        } else {
            self.items.len().saturating_sub(1)
        };
        if eligible_end == 0 {
            return None;
        }
        // Tier 1: old entries outside the recent summon workset. Tier 2: old workset entries.
        // 分两级淘汰:先淘汰近期召唤工作集之外的旧帧,再淘汰工作集内的旧帧。
        let index = (0..eligible_end)
            .find(|&index| !is_recent_workset(&self.items[index].0))
            .or(Some(0))?;
        self.items.remove(index)
    }

    #[cfg(test)]
    pub(crate) fn put(&mut self, key: K, val: V) -> Vec<V> {
        let result = self.put_detailed(key, val);
        result
            .replaced
            .into_iter()
            .chain(result.evicted.into_iter().map(|(_, value)| value))
            .collect()
    }

    /// 条件删除(如按 pid 清退),返回被删值供释放。
    /// Conditional removal (e.g. by pid); removed values returned for releasing.
    pub(crate) fn remove_where(&mut self, pred: impl Fn((&K, &V)) -> bool) -> Vec<V> {
        let mut removed: Vec<V> = Vec::new();
        let mut i = 0;
        while i < self.items.len() {
            if pred((&self.items[i].0, &self.items[i].1)) {
                let (_, v) = self.items.remove(i).unwrap();
                self.total_cost = self.total_cost.saturating_sub((self.cost)(&v));
                removed.push(v);
            } else {
                i += 1;
            }
        }
        removed
    }

    /// 当前条目数(测试断言用)。
    /// Current entry count (for test assertions).
    #[allow(dead_code)]
    pub(crate) fn len(&self) -> usize {
        self.items.len()
    }

    /// 快照当前缓存键,不改变 LRU 顺序。
    /// Snapshot current cache keys without changing LRU order.
    pub(crate) fn keys(&self) -> Vec<K> {
        self.items.iter().map(|(key, _)| key.clone()).collect()
    }

    /// 记账总成本(驱逐判定用的同一计数;内存采样器直接读)。
    /// Total accounted cost (the same counter the eviction check uses; the memory
    /// sampler reads it directly).
    pub(crate) fn total_cost(&self) -> u64 {
        self.total_cost
    }
}

// ========== 缓存状态 ==========

/// 大量窗口分页时保留更多相邻帧；窗口列表本身没有数量上限。
/// Retain more neighboring frames around large paged window sets; the authoritative
/// window list itself has no count limit.
const CACHE_MAX_ITEMS: usize = 64;
/// 约 64MB 成本上限(w*h*4 记账)，可容纳约 40 张常见 16:10 缩略图。
/// ~64MB cost budget (accounted as w*h*4), enough for roughly forty typical
/// 16:10 thumbnails.
pub(super) const CACHE_MAX_COST: u64 = 64_000_000;
/// 新鲜 TTL:召唤时 2s 内的直接复用；前台 App 的过期帧先画旧图再异步重截。
/// Freshness TTL: frames younger than 2s are reused at summon; stale frames from
/// the frontmost app render immediately while an async recapture swaps them in.
pub(super) const FRESH_TTL_MS: u128 = 2000;
/// App 激活后等待内容进程恢复并完成一轮重绘，再补拍焦点窗口。
/// Wait for a restored content process to redraw once before refreshing the focused window.
pub(super) const ACTIVATION_CAPTURE_DELAY_MS: u64 = 350;
/// 浮窗关闭时前台窗口预热的最小间隔；激活补拍仍走 350ms 快速路径。
/// Hidden-overlay prewarm interval; activation refreshes retain their separate 350ms fast path.
/// Several seconds between thumbnail-sized captures avoids turning a video surface into a
/// continuous full-resolution stream while keeping summon-time content reasonably fresh.
pub(super) const FOCUSED_PREWARM_INTERVAL_MS: u64 = 5_000;
pub(super) const FOCUSED_PREWARM_MAX_FAILURES: u8 = 3;
/// 启动预热与新窗口后台预生成使用的基准高度；召唤时按实际卡片与屏幕倍率升级。
/// Baseline height for startup/new-window pre-generation; summon-time demand upgrades it
/// from the actual card size and target screen scale.
pub(super) const BASE_TARGET_PX_H: u32 = 512;
pub(super) const CAPTURE_HEIGHT_BUCKETS: [u32; 4] = [512, 640, 768, 1024];
pub(super) const MAX_TARGET_PX_H: u32 = 1024;
/// 当前页面两侧的预取窗口数；切到相邻页前通常已经有缓存。
/// Number of windows prefetched on each side of the current page so adjacent-page
/// cards normally already have cached frames.
pub(super) const VISIBLE_PREFETCH_MARGIN: usize = 4;
/// 启动时只预热最可能出现在第一页的 MRU 工作集，避免窗口数超过缓存容量时
/// 先捕获、后立即驱逐。其余窗口在实际进入可见页时按高优先级补拍。
/// Prewarm only the MRU working set most likely to appear on the first page, avoiding
/// capture-then-immediate-eviction when the window count exceeds cache capacity. The
/// rest are captured at high priority when they actually enter a visible page.
pub(super) const STARTUP_PREWARM_MAX: usize = 24;
/// Only compare evictions with a recent summon workset; older snapshots may describe a
/// different Space or window ordering and would make the diagnostic misleading.
/// 只把淘汰与近期召唤工作集比较；过期快照可能来自另一个 Space 或窗口顺序。
pub(super) const WORKSET_SNAPSHOT_MAX_AGE: Duration = Duration::from_secs(2);

/// Clone 为浅拷贝(CGImageRef 位拷贝),所有权纪律:缓存持有 +1,克隆方仅在
/// 显式 CFRetain 后才能长期持有(见 lookup_retained)。
/// Clone is a shallow bit-copy of the CGImageRef. Ownership discipline: the cache
/// owns +1; a clonee may only hold it long-term after an explicit CFRetain (see
/// lookup_retained).
#[derive(Clone)]
pub(super) struct CachedThumb {
    /// CGImageRef(+1,缓存持有;驱逐时 CFRelease)。
    /// CGImageRef (+1, owned by the cache; CFRelease on eviction).
    pub(super) img: *const c_void,
    pub(super) w_px: u32,
    pub(super) h_px: u32,
    /// 本帧按哪个目标高度捕获；源窗口小于目标时实际 h_px 可以更小，但同一目标无需重试。
    /// Requested capture height for this frame. A smaller source may yield a lower h_px,
    /// but the same target must not trigger endless retries.
    pub(super) captured_for_px_h: u32,
    pub(super) captured: Instant,
    /// 全局递增的帧版本号(cache_store 时分配)。浮窗卡片签名携带它,帧在浮窗关闭
    /// 期间被替换(种子→真实、激活补拍、外观重拍)后,下一次召唤签名失配走 Replace
    /// 重建,复用路径不会持续展示旧图。
    /// Globally increasing frame version (assigned in cache_store). Overlay card
    /// signatures carry it: after a frame is replaced while the overlay is closed
    /// (seed -> real, activation refresh, appearance recapture), the next summon's
    /// signature mismatch forces a Replace rebuild, so the reuse path can never keep
    /// showing the stale image forever.
    pub(super) epoch: u64,
}

/// CachedThumb 内含裸 CGImageRef,需要 Send+Sync 才能放进跨线程 static;
/// 读写全部经 CACHE 互斥锁,CF 类型本身线程安全。
/// CachedThumb holds a raw CGImageRef, so it needs Send+Sync for the cross-thread
/// static; all access goes through the CACHE mutex and CF types are thread-safe.
unsafe impl Send for CachedThumb {}
unsafe impl Sync for CachedThumb {}

fn thumb_cost(t: &CachedThumb) -> u64 {
    (t.w_px as u64) * (t.h_px as u64) * 4
}

pub(super) static CACHE: LazyLock<Mutex<Lru<ThumbKey, CachedThumb>>> =
    LazyLock::new(|| Mutex::new(Lru::new(CACHE_MAX_ITEMS, CACHE_MAX_COST, thumb_cost)));

#[derive(Default)]
pub(super) struct WorksetSnapshot {
    pub(super) keys: HashSet<ThumbKey>,
    pub(super) updated_at: Option<Instant>,
}

static LAST_SUMMON_WORKSET: LazyLock<Mutex<WorksetSnapshot>> =
    LazyLock::new(|| Mutex::new(WorksetSnapshot::default()));

pub(super) fn update_summon_workset(keys: impl IntoIterator<Item = ThumbKey>) {
    let mut snapshot = LAST_SUMMON_WORKSET.lock().unwrap();
    snapshot.keys.clear();
    snapshot.keys.extend(keys);
    snapshot.updated_at = Some(Instant::now());
}

fn clear_summon_workset() {
    let mut snapshot = LAST_SUMMON_WORKSET.lock().unwrap();
    snapshot.keys.clear();
    snapshot.updated_at = None;
}

fn recent_workset_membership(key: ThumbKey) -> Option<bool> {
    let snapshot = LAST_SUMMON_WORKSET.lock().unwrap();
    recent_workset_membership_at(&snapshot, key, Instant::now())
}

pub(super) fn recent_workset_membership_at(
    snapshot: &WorksetSnapshot,
    key: ThumbKey,
    now: Instant,
) -> Option<bool> {
    let updated_at = snapshot.updated_at?;
    if now.duration_since(updated_at) > WORKSET_SNAPSHOT_MAX_AGE {
        return None;
    }
    Some(snapshot.keys.contains(&key))
}

fn recent_workset_keys() -> HashSet<ThumbKey> {
    let snapshot = LAST_SUMMON_WORKSET.lock().unwrap();
    if snapshot
        .updated_at
        .is_some_and(|updated_at| updated_at.elapsed() <= WORKSET_SNAPSHOT_MAX_AGE)
    {
        snapshot.keys.clone()
    } else {
        HashSet::new()
    }
}

pub(super) fn format_workset(keys: &[ThumbKey]) -> String {
    keys.iter()
        .map(|key| format!("{}:{}", key.pid, key.wid))
        .collect::<Vec<_>>()
        .join(",")
}

/// 内存采样器的账本读数:(条目数, 记账成本字节)。只取统计字段,不触碰 CGImageRef,
/// 不改变 LRU 次序(锁只持到读出两个整数)。
/// Ledger reading for the memory sampler: (item count, accounted cost in bytes). Reads only
/// the two counter fields -- no CGImageRef is touched and LRU order is unchanged (the lock is
/// held just long enough to copy two integers out).
pub(crate) fn cache_stats() -> (usize, u64) {
    let cache = CACHE.lock().unwrap();
    (cache.len(), cache.total_cost())
}

/// 捕获管线的轻量状态:待处理、进行中和主线程待交付的 key 数量。
/// Lightweight capture-pipeline state: queued, in-flight, and keys awaiting main-thread delivery.
pub(super) fn capture_pipeline_stats() -> (usize, usize, usize) {
    let (pending, in_flight) = {
        let state = CAPTURE_STATE.lock().unwrap();
        let in_flight = state.desired.values().filter(|job| job.running).count();
        (state.desired.len().saturating_sub(in_flight), in_flight)
    };
    let ready = READY_QUEUE.lock().unwrap().len();
    (pending, in_flight, ready)
}

/// 关闭缩略图模式时清掉进程内所有窗口截图,并使排队/进行中的捕获失效。
/// The thumbnail service stays resident for a cheap re-enable, but disabling the mode
/// releases all cached window images and invalidates queued/in-flight captures.
pub(crate) fn clear_runtime_cache() {
    crate::mem::log_debug_snapshot("thumb-cache-clear-before");
    // 与 app_terminated 使用相同的锁序:先失效任务,再清缓存。这样正在捕获的迟到结果
    // 在写入缓存前会发现 token 已失效,不会在关闭后把截图重新塞回来。
    // Keep the same lock order as app_terminated: invalidate jobs, then clear the cache. An
    // in-flight result therefore observes the invalid state before it can be cached.
    let mut state = CAPTURE_STATE.lock().unwrap();
    state.cancel_all();
    let evicted = CACHE.lock().unwrap().remove_where(|_| true);
    drop(state);

    let released = evicted.len();
    for thumb in evicted {
        unsafe {
            CFRelease(thumb.img);
        }
    }

    // 丢弃已经排队但尚未处理的 UI 更新,避免重新开启时消费旧批次。
    // Drop queued UI updates so a later re-enable cannot consume an old batch.
    READY_QUEUE.lock().unwrap().clear();
    READY_DELIVERY_SCHEDULED.store(false, Ordering::Release);
    PENDING_BLANK_RETRIES.lock().unwrap().clear();
    clear_summon_workset();
    let (cache_items, cache_bytes) = cache_stats();
    let (pending, in_flight, ready) = capture_pipeline_stats();
    log_debug!(
        "[thumb] runtime cache cleared: frames_released={} cache_items={} cache_bytes={} pending={} in_flight={} ready={}",
        released,
        cache_items,
        cache_bytes,
        pending,
        in_flight,
        ready,
    );
    crate::mem::log_debug_snapshot("thumb-cache-clear-after");
}

/// Drop a prewarm target when its process terminates; PID reuse must not inherit it.
/// 进程退出时清理预热目标，避免 PID 复用继承旧目标。
pub(super) fn forget_focused_prewarm_for_pid(pid: i32) {
    let _ = invalidate_focused_prewarm_target(
        |target| target.is_some_and(|target| target.pid == pid),
        "process-terminated",
    );
}

/// 取缩略图(+1 返回,调用方用完必须 CFRelease;缓存自己的引用不受影响)。
/// Fetch a thumbnail (+1 returned; the caller MUST CFRelease when done -- the
/// cache's own reference is unaffected).
pub(crate) fn lookup_retained(pid: i32, wid: u32) -> Option<(*const c_void, u32, u32)> {
    let mut cache = CACHE.lock().unwrap();
    let t = cache.get(&ThumbKey { pid, wid })?;
    unsafe {
        CFRetain(t.img);
    }
    Some((t.img, t.w_px, t.h_px))
}

/// 已显示卡片被复用时只推进 LRU 次序,不复制或保留 CGImageRef。
/// Touch the LRU entry when an already-displayed card is reused, without copying or retaining
/// its CGImageRef.
pub(crate) fn touch_cached_frame(pid: i32, wid: u32) -> bool {
    CACHE.lock().unwrap().touch(&ThumbKey { pid, wid })
}

/// 是否新鲜(召唤端及启动诊断用；过期帧仍可继续渲染)。
/// Freshness probe for summon decisions and startup diagnostics; stale frames
/// remain renderable.
pub(super) fn cached_frame_is_usable(
    captured: Instant,
    captured_for_px_h: u32,
    required_px_h: u32,
    now: Instant,
) -> bool {
    is_fresh(captured, now, FRESH_TTL_MS) && captured_for_px_h >= required_px_h
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SummonRefreshDecision {
    Missing,
    FrontmostStale,
    BackgroundLastGood,
    Fresh,
}

/// 后台窗口即使 TTL 过期或分辨率偏低也保留已有帧，避免休眠 WebView 的白色
/// 内容层覆盖最后一张正常画面；完全缺失时仍允许首次预热。
/// 焦点窗口例外:它的内容随用户操作随时变化(切标签页/滚动/播放视频),时间上的
/// "新鲜"不代表内容正确——系统层面没有任何事件通知这些变化,所以每次召唤都
/// 无条件重截,预览始终是实时画面(单窗 ~30ms,异步完成)。
/// Keep an existing background frame even when stale or undersized so a suspended
/// WebView's white content layer cannot replace the last-known-good image; a wholly
/// missing frame may still be pre-warmed.
/// The focused window is the exception: its content changes with every user action
/// (tab switches / scrolling / video playback), so temporal freshness does not mean
/// correct content -- no system event announces those changes. It is recaptured
/// unconditionally on every summon so its preview is always live (~30ms per window,
/// completed asynchronously).
pub(super) fn summon_refresh_decision(
    cached: Option<(Instant, u32)>,
    required_px_h: u32,
    now: Instant,
    is_frontmost: bool,
    is_focused: bool,
) -> SummonRefreshDecision {
    let Some((captured, captured_for_px_h)) = cached else {
        return SummonRefreshDecision::Missing;
    };
    if is_frontmost && is_focused {
        return SummonRefreshDecision::FrontmostStale;
    }
    if cached_frame_is_usable(captured, captured_for_px_h, required_px_h, now) {
        SummonRefreshDecision::Fresh
    } else if is_frontmost {
        SummonRefreshDecision::FrontmostStale
    } else {
        SummonRefreshDecision::BackgroundLastGood
    }
}

pub(super) fn cached_summon_refresh_decision(
    pid: i32,
    wid: u32,
    required_px_h: u32,
    is_frontmost: bool,
    is_focused: bool,
) -> SummonRefreshDecision {
    let cache = CACHE.lock().unwrap();
    let cached = cache
        .peek(&ThumbKey { pid, wid })
        .map(|t| (t.captured, t.captured_for_px_h));
    summon_refresh_decision(
        cached,
        required_px_h,
        Instant::now(),
        is_frontmost,
        is_focused,
    )
}

pub(super) fn cached_target_px_height(pid: i32, wid: u32) -> u32 {
    let cache = CACHE.lock().unwrap();
    cache
        .peek(&ThumbKey { pid, wid })
        .map(|t| t.captured_for_px_h.max(BASE_TARGET_PX_H))
        .unwrap_or(BASE_TARGET_PX_H)
}

pub(super) fn cache_store(pid: i32, wid: u32, mut t: CachedThumb) {
    // 帧版本号在唯一入库点分配,所有存储路径(预热/召唤/激活/外观)都会推进。
    // The frame version is assigned at the single store point; every path (prewarm /
    // summon / activation / appearance) advances it.
    t.epoch = FRAME_EPOCH_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
    // 释放大型 CGImage 可能回收 IOSurface/位图存储；先放开缓存锁，避免主线程
    // lookup 在释放期间被无谓阻塞。
    // Releasing a large CGImage may reclaim IOSurface/bitmap storage. Drop the
    // cache lock first so main-thread lookups are not blocked by destruction.
    let recent_keys = recent_workset_keys();
    let result =
        CACHE
            .lock()
            .unwrap()
            .put_detailed_with_priority(ThumbKey { pid, wid }, t, |key| recent_keys.contains(key));
    log_debug!(
        "[thumb] cache store operation={:?} pid={} wid={} cost={} replaced_cost={:?} evicted={} count_over_limit={} cost_over_limit={}",
        result.operation,
        pid,
        wid,
        result.cost,
        result.replaced_cost,
        result.evicted.len(),
        result.count_over_limit,
        result.cost_over_limit,
    );
    for (evicted_key, evicted) in &result.evicted {
        log_debug!(
            "[thumb] cache eviction inserted_pid={} inserted_wid={} inserted_cost={} evicted_pid={} evicted_wid={} evicted_cost={} evicted_in_recent_workset={:?} trigger_count={} trigger_cost={}",
            result.key.pid,
            result.key.wid,
            result.cost,
            evicted_key.pid,
            evicted_key.wid,
            thumb_cost(evicted),
            recent_workset_membership(*evicted_key),
            result.count_over_limit,
            result.cost_over_limit,
        );
    }
    if let Some(replaced) = result.replaced {
        unsafe {
            CFRelease(replaced.img);
        }
    }
    for (_, evicted) in result.evicted {
        unsafe {
            CFRelease(evicted.img);
        }
    }
}

pub(crate) fn forget_destroyed_window(pid: i32, wid: u32) {
    let key = ThumbKey { pid, wid };
    // 与 cache_store/clear_runtime_cache 使用相同的锁序:先使任务失效,再移除缓存。
    // Keep the same lock order as cache_store/clear_runtime_cache: invalidate jobs first,
    // then remove the cached frame.
    let mut state = CAPTURE_STATE.lock().unwrap();
    let invalidated = state.invalidate_window(key);
    let retry_cleared = PENDING_BLANK_RETRIES.lock().unwrap().remove(&key);
    let removed = CACHE
        .lock()
        .unwrap()
        .remove_where(|(candidate, _)| *candidate == key);
    drop(state);

    let removed_count = removed.len();
    for thumb in removed {
        unsafe {
            CFRelease(thumb.img);
        }
    }
    if invalidated || retry_cleared || removed_count > 0 {
        log_debug!(
            "[thumb] destroyed window cleanup pid={} wid={} invalidated={} retry_cleared={} removed={}",
            pid,
            wid,
            invalidated,
            retry_cleared,
            removed_count,
        );
    }
}

/// 全局帧版本计数器(cache_store 内部分配;从 1 开始,0 表示无帧)。
/// Global frame version counter (assigned inside cache_store; starts at 1, 0 = none).
static FRAME_EPOCH_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 当前缓存帧的版本号(只读探测,不改变 LRU 次序;0 = 无缓存帧)。卡片签名用它
/// 感知"浮窗关闭期间帧被替换",签名失配触发 Replace 重建。
/// The cached frame's current version (read-only probe, LRU order untouched;
/// 0 = no frame). Card signatures use it to notice frames replaced while the
/// overlay was closed; the signature mismatch then triggers a Replace rebuild.
pub(crate) fn frame_epoch(pid: i32, wid: u32) -> u64 {
    CACHE
        .lock()
        .unwrap()
        .peek(&ThumbKey { pid, wid })
        .map(|t| t.epoch)
        .unwrap_or(0)
}
