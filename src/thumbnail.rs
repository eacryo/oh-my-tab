//! 窗口缩略图:私有 SkyLight API `CGSHWCCaptureWindowList` 截取窗口画面,
//! **纯内存 LRU** 缓存(刻意不落盘——屏幕内容明文落盘有隐私风险,BetterCmdTab/
//! DockDoor 同样只保留内存)。三条生产线:
//! 1. 启动预生成:监视线程启动时枚举所有运行中 App 的标准窗口补拍
//! 2. 常驻监听:每 PID 一个 AXObserver 订阅 kAXWindowCreatedNotification,
//!    新窗口防抖 300ms 后预生成(等窗口完成初始化,避免拍到白屏)
//! 3. 召唤补拍:show_overlay 时对可见区间及两侧预取项中的缺失帧、前台 App
//!    的过期帧入队；后台 App 保留最后一张有效帧，完成后主线程原位换卡
//! 4. 激活刷新:NSWorkspace 确认焦点窗口后延迟补拍，等 Web 内容完成恢复/重绘
//!
//! 无屏幕录制权限(TCC)时整个模块休眠,浮窗保持纯图标渲染;运行中授权后
//! 下一个捕获任务自动恢复(worker 每个任务前都重新 preflight)。
//!
//!
//! Window thumbnails: capture window imagery via the private SkyLight API
//! `CGSHWCCaptureWindowList`, cached in a **memory-only LRU** (deliberately never
//! written to disk -- plaintext screen content in ~/Library/Caches is a privacy
//! risk; BetterCmdTab/DockDoor likewise keep frames in RAM only). Three producers:
//! 1. startup pre-generation: enumerate every running app's standard windows
//! 2. resident listener: one AXObserver per PID watching kAXWindowCreatedNotification;
//!    a new window debounces 300ms (letting it finish initializing, avoiding a white
//!    flash) then pre-generates
//! 3. summon refresh: show_overlay enqueues missing windows and stale frames from the
//!    frontmost app in the visible slice plus prefetch margins; background apps retain
//!    their last-known-good frame, and results swap affected cards in place on the main thread
//! 4. activation refresh: after NSWorkspace resolves the focused window, capture it with a
//!    short delay so restored web content has time to redraw.
//!
//! Without the Screen Recording TCC permission the whole module sleeps and the
//! overlay keeps rendering icons only; granting permission mid-run resumes
//! automatically (the worker re-preflights before every capture).

use objc2::runtime::AnyObject;
use objc2::{class, msg_send, sel};
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::{c_char, c_void, CString};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::log_debug;

/// 缩略图缓存键:(进程 ID, CG 窗口 ID)。两者组合才能防 PID 复用串图。
/// Thumbnail cache key: (process id, CG window id). The pair guards against
/// recycled PIDs serving another window's frame.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct ThumbKey {
    pub(crate) pid: i32,
    pub(crate) wid: u32,
}

// ========== FFI:dlopen 私有符号 + 公开 CoreGraphics/AX 符号 ==========
// (照 window_collector 的 per-module 自包含 extern 惯例,不跨模块共享声明。)
// (following window_collector's self-contained per-module extern convention.)

extern "C" {
    fn dlopen(filename: *const c_char, mode: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}
const RTLD_NOW: i32 = 2;

unsafe fn dlopen_path(path: &str) -> *mut c_void {
    let c = CString::new(path).unwrap();
    dlopen(c.as_ptr(), RTLD_NOW)
}

type CGSConnectionID = u32;
type CgsMainConnFn = unsafe extern "C" fn() -> CGSConnectionID;
/// 返回 CFArray(CGImageRef 列表);调用方负责 CFRelease 整个数组。
/// Returns a CFArray of CGImageRefs; the caller owns the array (CFRelease it).
type CgsCaptureListFn =
    unsafe extern "C" fn(CGSConnectionID, *const u32, usize, u32) -> *const c_void;

/// CGSWindowCaptureOptions 位(DockDoor PrivateApis.swift 同源):
/// bestResolution = Retina 原生分辨率,
/// ignoreGlobalClipShape 绕过全局裁剪,fullSize 绕过 Stage Manager 歪斜。
/// CGSWindowCaptureOptions bits (mirroring DockDoor's PrivateApis.swift):
/// bestResolution = native retina pixels,
/// ignoreGlobalClipShape bypasses the global clip shape, fullSize dodges the
/// Stage Manager skew workaround.
const CGS_CAPTURE_BEST_RESOLUTION: u32 = 1 << 8;
const CGS_CAPTURE_IGNORE_GLOBAL_CLIP_SHAPE: u32 = 1 << 11;

static CGS_MAIN_CONN: LazyLock<Option<CgsMainConnFn>> = LazyLock::new(|| unsafe {
    // CGSMainConnectionID 与 CGSHWCCaptureWindowList 都由 SkyLight 导出。
    // Both symbols are exported by the SkyLight framework.
    let h = dlopen_path("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight");
    if h.is_null() {
        return None;
    }
    let name = b"CGSMainConnectionID\0";
    let p = dlsym(h, name.as_ptr() as *const c_char);
    if p.is_null() {
        return None;
    }
    Some(std::mem::transmute::<*mut c_void, CgsMainConnFn>(p))
});

static CGS_CAPTURE_LIST: LazyLock<Option<CgsCaptureListFn>> = LazyLock::new(|| unsafe {
    let h = dlopen_path("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight");
    if h.is_null() {
        return None;
    }
    // 注意:公开名 CGSHWCCaptureWindowList 是 CoreGraphics 对 SkyLight 内部符号
    // _SLSHWCaptureWindowList 的再导出,dlsym(SkyLight, "CGSHWCCaptureWindowList")
    // 拿不到(实测);必须用 SkyLight 原生名 SLSHWCaptureWindowList。
    // Note: the public name CGSHWCCaptureWindowList is CoreGraphics's re-export of
    // SkyLight's internal _SLSHWCaptureWindowList; dlsym(SkyLight,
    // "CGSHWCCaptureWindowList") finds nothing (verified) -- SkyLight's native name
    // SLSHWCaptureWindowList must be used.
    let name = b"SLSHWCaptureWindowList\0";
    let p = dlsym(h, name.as_ptr() as *const c_char);
    if p.is_null() {
        return None;
    }
    Some(std::mem::transmute::<*mut c_void, CgsCaptureListFn>(p))
});

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
    fn CGImageGetWidth(image: *const c_void) -> usize;
    fn CGImageGetHeight(image: *const c_void) -> usize;
    fn CGColorSpaceCreateDeviceRGB() -> *const c_void;
    fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: *const c_void,
        bitmap_info: u32,
    ) -> *mut c_void;
    fn CGContextDrawImage(ctx: *mut c_void, rect: CGRect, image: *const c_void);
    fn CGBitmapContextCreateImage(ctx: *mut c_void) -> *const c_void;
    fn CFArrayGetCount(array: *const c_void) -> isize;
    fn CFArrayGetValueAtIndex(array: *const c_void, index: isize) -> *const c_void;
    fn CFRelease(cf: *const c_void);
    fn CFRetain(cf: *const c_void);
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    /// CFString 值比较:相等返回 0(kCFCompareEqualTo)。
    /// CFString value comparison: 0 when equal (kCFCompareEqualTo).
    fn CFStringCompare(a: *const c_void, b: *const c_void, options: usize) -> isize;
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

/// kCGImageAlphaPremultipliedLast(RGBA, alpha 在低字节序的末字节)。
/// kCGImageAlphaPremultipliedLast (RGBA with alpha in the last byte on LE).
const BITMAP_PREMULTIPLIED_LAST: u32 = 1;

// ========== 屏幕录制权限(TCC) ==========

static PERMISSION_PROMPTED: AtomicBool = AtomicBool::new(false);

/// 是否已授予屏幕录制权限(preflight,廉价可反复调用)。
/// Whether Screen Recording is granted (cheap preflight, safe to call often).
pub(crate) fn capture_allowed() -> bool {
    unsafe { CGPreflightScreenCaptureAccess() }
}

/// 未授权时的主动申请:每次启动至多弹一次系统授权框,之后静默休眠。
/// Active request when unauthorized: the system prompt fires at most once per
/// launch; afterwards the module sleeps silently.
fn request_permission_once() {
    if PERMISSION_PROMPTED.swap(true, Ordering::Relaxed) {
        return;
    }
    unsafe {
        CGRequestScreenCaptureAccess();
    }
}

// ========== 纯函数(单元测试覆盖) ==========

/// 判断缓存帧是否仍新鲜:TTL 内视为新鲜(召唤时直接用,不重截)。
/// Whether a cached frame is still fresh: within the TTL it is served as-is at
/// summon time (no recapture).
fn is_fresh(captured: Instant, now: Instant, ttl_ms: u128) -> bool {
    now.duration_since(captured).as_millis() < ttl_ms
}

/// aspect-fit 适配尺寸:把内容(content_w×content_h)完整放进目标框(box_w×box_h),
/// 等比缩到长边贴合、短边留白(由容器背景补);返回实际绘制宽高(调用方居中定位)。
/// 选 fit 不选 cover:用户预期是"看到完整窗口",cover 会裁掉溢出部分。
/// Aspect-fit sizing: fit the content ENTIRELY inside the target box (long edge
/// fits, short edge letterboxed by the container background); returns the drawn
/// width/height (the caller centers it). Fit over cover: the user expects to see
/// the WHOLE window -- cover would crop the overflow.
pub(crate) fn fit_size(content_w: f64, content_h: f64, box_w: f64, box_h: f64) -> (f64, f64) {
    if content_w <= 0.0 || content_h <= 0.0 || box_w <= 0.0 || box_h <= 0.0 {
        return (box_w.max(0.0), box_h.max(0.0));
    }
    let s = (box_w / content_w).min(box_h / content_h);
    (content_w * s, content_h * s)
}

/// 目标像素尺寸:按最大高度等比缩小(绝不放大);退化输入原样返回。
/// Target pixel size: proportional shrink to a max height (never upscale);
/// degenerate inputs pass through.
fn fit_target(src_w: u32, src_h: u32, max_h: u32) -> (u32, u32) {
    if src_w == 0 || src_h == 0 || max_h == 0 || src_h <= max_h {
        return (src_w, src_h);
    }
    let tw = ((src_w as u64 * max_h as u64) / src_h as u64).max(1) as u32;
    (tw, max_h)
}

/// 根据本次浮窗预览的 pt 高度与目标屏 backing scale 选择捕获像素高度。
/// 分档避免布局小幅变化造成重复升级；启动预热仍使用 512px，召唤时可按实际屏幕
/// 升到 640/768/1024px。输入异常时安全回退 512px。
/// Choose capture pixel height from this overlay's preview height in points and the target
/// screen's backing scale. Buckets avoid repeated upgrades from tiny layout changes; startup
/// pre-generation stays at 512px while summon-time demand may rise to 640/768/1024px.
/// Invalid inputs safely fall back to 512px.
pub(crate) fn target_px_height(preview_h_pt: f64, backing_scale: f64) -> u32 {
    if !preview_h_pt.is_finite()
        || !backing_scale.is_finite()
        || preview_h_pt <= 0.0
        || backing_scale <= 0.0
    {
        return BASE_TARGET_PX_H;
    }
    let required = (preview_h_pt * backing_scale).ceil() as u32;
    CAPTURE_HEIGHT_BUCKETS
        .iter()
        .copied()
        .find(|&bucket| bucket >= required)
        .unwrap_or(MAX_TARGET_PX_H)
}

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
    cost: fn(&V) -> u64,
    total_cost: u64,
    items: VecDeque<(K, V)>, // 队尾 = 最近使用 / back = most recently used
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

    /// 只读元数据探测:不改变 LRU 次序。新鲜度/目标尺寸检查不能把未渲染条目
    /// 伪装成最近使用；只有真正渲染的 get() 才提升 recency。
    /// Read-only metadata probe that does not alter LRU order. Freshness/target-size
    /// checks must not make an unrendered entry look recently used; only rendering
    /// through get() should bump recency.
    fn peek(&self, key: &K) -> Option<V> {
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
    pub(crate) fn put(&mut self, key: K, val: V) -> Vec<V> {
        let mut evicted: Vec<V> = Vec::new();
        if let Some(idx) = self.items.iter().position(|(k, _)| *k == key) {
            let (_, old) = self.items.remove(idx).unwrap();
            self.total_cost = self.total_cost.saturating_sub((self.cost)(&old));
            evicted.push(old);
        }
        self.total_cost = self.total_cost.saturating_add((self.cost)(&val));
        self.items.push_back((key, val));
        // 先挤旧帧;队尾新帧只在条目数超限时才参与驱逐(见函数注释)。
        // Evict old frames first; the back item only participates when the item
        // count itself is over the cap (see the fn doc).
        while self.items.len() > 1
            && (self.items.len() > self.max_items || self.total_cost > self.max_cost)
        {
            let Some((_, v)) = self.items.pop_front() else {
                break;
            };
            self.total_cost = self.total_cost.saturating_sub((self.cost)(&v));
            evicted.push(v);
        }
        while self.items.len() > self.max_items {
            let Some((_, v)) = self.items.pop_front() else {
                break;
            };
            self.total_cost = self.total_cost.saturating_sub((self.cost)(&v));
            evicted.push(v);
        }
        evicted
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
}

// ========== 缓存状态 ==========

/// 大量窗口分页时保留更多相邻帧；窗口列表本身没有数量上限。
/// Retain more neighboring frames around large paged window sets; the authoritative
/// window list itself has no count limit.
const CACHE_MAX_ITEMS: usize = 64;
/// 约 64MB 成本上限(w*h*4 记账)，可容纳约 40 张常见 16:10 缩略图。
/// ~64MB cost budget (accounted as w*h*4), enough for roughly forty typical
/// 16:10 thumbnails.
const CACHE_MAX_COST: u64 = 64_000_000;
/// 新鲜 TTL:召唤时 2s 内的直接复用；前台 App 的过期帧先画旧图再异步重截。
/// Freshness TTL: frames younger than 2s are reused at summon; stale frames from
/// the frontmost app render immediately while an async recapture swaps them in.
const FRESH_TTL_MS: u128 = 2000;
/// App 激活后等待内容进程恢复并完成一轮重绘，再补拍焦点窗口。
/// Wait for a restored content process to redraw once before refreshing the focused window.
const ACTIVATION_CAPTURE_DELAY_MS: u64 = 350;
/// 启动预热与新窗口后台预生成使用的基准高度；召唤时按实际卡片与屏幕倍率升级。
/// Baseline height for startup/new-window pre-generation; summon-time demand upgrades it
/// from the actual card size and target screen scale.
const BASE_TARGET_PX_H: u32 = 512;
const CAPTURE_HEIGHT_BUCKETS: [u32; 4] = [512, 640, 768, 1024];
const MAX_TARGET_PX_H: u32 = 1024;
/// 当前页面两侧的预取窗口数；切到相邻页前通常已经有缓存。
/// Number of windows prefetched on each side of the current page so adjacent-page
/// cards normally already have cached frames.
const VISIBLE_PREFETCH_MARGIN: usize = 4;
/// 启动时只预热最可能出现在第一页的 MRU 工作集，避免窗口数超过缓存容量时
/// 先捕获、后立即驱逐。其余窗口在实际进入可见页时按高优先级补拍。
/// Prewarm only the MRU working set most likely to appear on the first page, avoiding
/// capture-then-immediate-eviction when the window count exceeds cache capacity. The
/// rest are captured at high priority when they actually enter a visible page.
const STARTUP_PREWARM_MAX: usize = 24;

/// Clone 为浅拷贝(CGImageRef 位拷贝),所有权纪律:缓存持有 +1,克隆方仅在
/// 显式 CFRetain 后才能长期持有(见 lookup_retained)。
/// Clone is a shallow bit-copy of the CGImageRef. Ownership discipline: the cache
/// owns +1; a clonee may only hold it long-term after an explicit CFRetain (see
/// lookup_retained).
#[derive(Clone)]
struct CachedThumb {
    /// CGImageRef(+1,缓存持有;驱逐时 CFRelease)。
    /// CGImageRef (+1, owned by the cache; CFRelease on eviction).
    img: *const c_void,
    w_px: u32,
    h_px: u32,
    /// 本帧按哪个目标高度捕获；源窗口小于目标时实际 h_px 可以更小，但同一目标无需重试。
    /// Requested capture height for this frame. A smaller source may yield a lower h_px,
    /// but the same target must not trigger endless retries.
    captured_for_px_h: u32,
    captured: Instant,
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

static CACHE: LazyLock<Mutex<Lru<ThumbKey, CachedThumb>>> =
    LazyLock::new(|| Mutex::new(Lru::new(CACHE_MAX_ITEMS, CACHE_MAX_COST, thumb_cost)));

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

/// 是否新鲜(召唤端及启动诊断用；过期帧仍可继续渲染)。
/// Freshness probe for summon decisions and startup diagnostics; stale frames
/// remain renderable.
fn cached_frame_is_usable(
    captured: Instant,
    captured_for_px_h: u32,
    required_px_h: u32,
    now: Instant,
) -> bool {
    is_fresh(captured, now, FRESH_TTL_MS) && captured_for_px_h >= required_px_h
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SummonRefreshDecision {
    Missing,
    FrontmostStale,
    BackgroundLastGood,
    Fresh,
}

/// 后台窗口即使 TTL 过期或分辨率偏低也保留已有帧，避免休眠 WebView 的白色
/// 内容层覆盖最后一张正常画面；完全缺失时仍允许首次预热。
/// Keep an existing background frame even when stale or undersized so a suspended
/// WebView's white content layer cannot replace the last-known-good image. A wholly
/// missing frame may still be pre-warmed.
fn summon_refresh_decision(
    cached: Option<(Instant, u32)>,
    required_px_h: u32,
    now: Instant,
    is_frontmost: bool,
) -> SummonRefreshDecision {
    let Some((captured, captured_for_px_h)) = cached else {
        return SummonRefreshDecision::Missing;
    };
    if cached_frame_is_usable(captured, captured_for_px_h, required_px_h, now) {
        SummonRefreshDecision::Fresh
    } else if is_frontmost {
        SummonRefreshDecision::FrontmostStale
    } else {
        SummonRefreshDecision::BackgroundLastGood
    }
}

fn cached_summon_refresh_decision(
    pid: i32,
    wid: u32,
    required_px_h: u32,
    is_frontmost: bool,
) -> SummonRefreshDecision {
    let cache = CACHE.lock().unwrap();
    let cached = cache
        .peek(&ThumbKey { pid, wid })
        .map(|t| (t.captured, t.captured_for_px_h));
    summon_refresh_decision(cached, required_px_h, Instant::now(), is_frontmost)
}

fn cached_target_px_height(pid: i32, wid: u32) -> u32 {
    let cache = CACHE.lock().unwrap();
    cache
        .peek(&ThumbKey { pid, wid })
        .map(|t| t.captured_for_px_h.max(BASE_TARGET_PX_H))
        .unwrap_or(BASE_TARGET_PX_H)
}

fn cache_store(pid: i32, wid: u32, t: CachedThumb) {
    // 释放大型 CGImage 可能回收 IOSurface/位图存储；先放开缓存锁，避免主线程
    // lookup 在释放期间被无谓阻塞。
    // Releasing a large CGImage may reclaim IOSurface/bitmap storage. Drop the
    // cache lock first so main-thread lookups are not blocked by destruction.
    let evicted = CACHE.lock().unwrap().put(ThumbKey { pid, wid }, t);
    for evicted in evicted {
        unsafe {
            CFRelease(evicted.img);
        }
    }
}

// ========== 捕获管线(flume 队列 + 单 worker 串行限流) ==========

/// 捕获优先级。值越大越先执行；同优先级保持首次入队 FIFO。启动预热永远可被
/// 后续召唤的选中/可见请求原地提升，不需要复制第二份任务。
/// Capture priority. Higher values run first; equal priorities retain initial FIFO
/// order. Startup prewarm work can be promoted in place by later selected/visible
/// requests without duplicating the job.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum CapturePriority {
    Startup,
    NewWindow,
    Prefetch,
    Visible,
    Activation,
    Selected,
}

impl CapturePriority {
    fn label(self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::NewWindow => "new-window",
            Self::Prefetch => "prefetch",
            Self::Visible => "visible",
            Self::Activation => "activation",
            Self::Selected => "selected",
        }
    }
}

#[derive(Clone, Copy)]
struct PendingCapture {
    target_px_h: u32,
    priority: CapturePriority,
    sequence: u64,
    token: u64,
    pid_generation: u64,
    activation_at: Option<Instant>,
    freshness_sequence: u64,
    enqueued_at: Instant,
    running: bool,
}

#[derive(Clone, Copy)]
struct CaptureJob {
    key: ThumbKey,
    target_px_h: u32,
    priority: CapturePriority,
    token: u64,
    pid_generation: u64,
    activation_at: Option<Instant>,
    freshness_sequence: u64,
    enqueued_at: Instant,
}

/// 同时记录 queued/in-flight 请求的最高目标、最高优先级和生命周期 token。
/// worker 每次被 channel 信号唤醒后从这里选最高优先级任务，因此 channel 自身
/// 只负责计数/唤醒，不再决定执行顺序。
/// Tracks the highest target, priority, and lifecycle token for queued/in-flight
/// requests. The channel is only a count/wakeup mechanism; on each wake the worker
/// selects the highest-priority job here instead of inheriting channel FIFO order.
#[derive(Default)]
struct CaptureState {
    desired: HashMap<ThumbKey, PendingCapture>,
    pid_generations: HashMap<i32, u64>,
    terminated_pids: HashSet<i32>,
    next_sequence: u64,
    next_token: u64,
    next_freshness_sequence: u64,
}

impl CaptureState {
    fn next_counter(counter: &mut u64) -> u64 {
        *counter = counter.wrapping_add(1);
        if *counter == 0 {
            *counter = 1;
        }
        *counter
    }

    fn request(&mut self, key: ThumbKey, target_px_h: u32, priority: CapturePriority) -> bool {
        let pid_generation = self.pid_generations.get(&key.pid).copied().unwrap_or(0);
        self.request_for_generation(key, target_px_h, priority, pid_generation)
    }

    fn request_for_generation(
        &mut self,
        key: ThumbKey,
        target_px_h: u32,
        priority: CapturePriority,
        pid_generation: u64,
    ) -> bool {
        if self.terminated_pids.contains(&key.pid)
            || self.pid_generations.get(&key.pid).copied().unwrap_or(0) != pid_generation
        {
            return false;
        }
        let freshness_sequence = Self::next_counter(&mut self.next_freshness_sequence);
        match self.desired.entry(key) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                let sequence = Self::next_counter(&mut self.next_sequence);
                let token = Self::next_counter(&mut self.next_token);
                entry.insert(PendingCapture {
                    target_px_h,
                    priority,
                    sequence,
                    token,
                    pid_generation,
                    activation_at: None,
                    freshness_sequence: 0,
                    enqueued_at: Instant::now(),
                    running: false,
                });
                true
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let pending = entry.get_mut();
                pending.target_px_h = pending.target_px_h.max(target_px_h);
                pending.priority = pending.priority.max(priority);
                if priority == CapturePriority::Selected && pending.activation_at.take().is_some() {
                    pending.freshness_sequence = freshness_sequence;
                }
                false
            }
        }
    }

    fn request_activation(
        &mut self,
        key: ThumbKey,
        target_px_h: u32,
        activated_at: Instant,
        pid_generation: u64,
    ) -> bool {
        if self.terminated_pids.contains(&key.pid)
            || self.pid_generations.get(&key.pid).copied().unwrap_or(0) != pid_generation
        {
            return false;
        }
        let inserted = self.request_for_generation(
            key,
            target_px_h,
            CapturePriority::Activation,
            pid_generation,
        );
        let freshness_sequence = Self::next_counter(&mut self.next_freshness_sequence);
        if let Some(pending) = self.desired.get_mut(&key) {
            pending.activation_at = Some(activated_at);
            pending.freshness_sequence = freshness_sequence;
        }
        inserted
    }

    fn take_next(&mut self) -> Option<CaptureJob> {
        let key = self
            .desired
            .iter()
            .filter(|(_, pending)| !pending.running)
            .min_by_key(|(_, pending)| (Reverse(pending.priority), pending.sequence))
            .map(|(key, _)| *key)?;
        let pending = self.desired.get_mut(&key)?;
        pending.running = true;
        Some(CaptureJob {
            key,
            target_px_h: pending.target_px_h,
            priority: pending.priority,
            token: pending.token,
            pid_generation: pending.pid_generation,
            activation_at: pending.activation_at,
            freshness_sequence: pending.freshness_sequence,
            enqueued_at: pending.enqueued_at,
        })
    }

    fn is_current(&self, job: CaptureJob) -> bool {
        self.pid_generations.get(&job.key.pid).copied().unwrap_or(0) == job.pid_generation
            && self
                .desired
                .get(&job.key)
                .is_some_and(|pending| pending.token == job.token)
    }

    fn finish(&mut self, job: CaptureJob) -> bool {
        let Some(pending) = self.desired.get_mut(&job.key) else {
            return false;
        };
        if pending.token != job.token || pending.pid_generation != job.pid_generation {
            return false;
        }
        if pending.target_px_h > job.target_px_h
            || pending.priority > job.priority
            || pending.freshness_sequence > job.freshness_sequence
        {
            pending.running = false;
            pending.enqueued_at = Instant::now();
            true
        } else {
            self.desired.remove(&job.key);
            false
        }
    }

    fn cancel_pid(&mut self, pid: i32) {
        let generation = self.pid_generations.entry(pid).or_default();
        *generation = generation.wrapping_add(1);
        self.terminated_pids.insert(pid);
        self.desired.retain(|key, _| key.pid != pid);
    }

    fn activate_pid(&mut self, pid: i32) {
        let generation = self.pid_generations.entry(pid).or_default();
        *generation = generation.wrapping_add(1);
        self.terminated_pids.remove(&pid);
        self.desired.retain(|key, _| key.pid != pid);
    }

    fn pid_generation(&self, pid: i32) -> u64 {
        self.pid_generations.get(&pid).copied().unwrap_or(0)
    }
}

static CAPTURE_STATE: LazyLock<Mutex<CaptureState>> =
    LazyLock::new(|| Mutex::new(CaptureState::default()));
static JOB_TX: OnceLock<flume::Sender<()>> = OnceLock::new();

/// 尝试安排一次捕获；返回 false 表示相同窗口已 pending/in-flight，或 worker 已退出。
/// Try to schedule one capture; false means the same window is already pending/in-flight,
/// or the worker has exited.
fn enqueue_job(pid: i32, wid: u32, target_px_h: u32, priority: CapturePriority) -> bool {
    enqueue_job_inner(pid, wid, target_px_h, priority, None)
}

/// 仅当 PID 仍处于生产者观察到的 generation 时入队，阻止终止前的延迟任务污染
/// PID 复用后的新进程。
/// Enqueue only while the PID remains in the generation observed by the producer,
/// preventing delayed work from an old process from contaminating a reused PID.
fn enqueue_job_for_generation(
    pid: i32,
    wid: u32,
    target_px_h: u32,
    priority: CapturePriority,
    pid_generation: u64,
) -> bool {
    enqueue_job_inner(pid, wid, target_px_h, priority, Some(pid_generation))
}

fn enqueue_activation_job(
    pid: i32,
    wid: u32,
    target_px_h: u32,
    activated_at: Instant,
    pid_generation: u64,
) -> bool {
    let key = ThumbKey { pid, wid };
    let tx = ensure_capture_worker();
    let accepted = CAPTURE_STATE.lock().unwrap().request_activation(
        key,
        target_px_h,
        activated_at,
        pid_generation,
    );
    if !accepted {
        return false;
    }
    if tx.send(()).is_err() {
        CAPTURE_STATE.lock().unwrap().desired.remove(&key);
        return false;
    }
    true
}

fn enqueue_job_inner(
    pid: i32,
    wid: u32,
    target_px_h: u32,
    priority: CapturePriority,
    expected_generation: Option<u64>,
) -> bool {
    let key = ThumbKey { pid, wid };
    let tx = ensure_capture_worker();
    let accepted = {
        let mut state = CAPTURE_STATE.lock().unwrap();
        match expected_generation {
            Some(generation) => {
                state.request_for_generation(key, target_px_h, priority, generation)
            }
            None => state.request(key, target_px_h, priority),
        }
    };
    if !accepted {
        return false;
    }
    if tx.send(()).is_err() {
        CAPTURE_STATE.lock().unwrap().desired.remove(&key);
        return false;
    }
    true
}

fn ensure_capture_worker() -> &'static flume::Sender<()> {
    JOB_TX.get_or_init(|| {
        let (tx, rx) = flume::unbounded::<()>();
        let retry_tx = tx.clone();
        std::thread::Builder::new()
            .name("thumb-capture".into())
            .spawn(move || {
                log_debug!("[thumb] capture worker online");
                for () in rx.iter() {
                    let Some(job) = CAPTURE_STATE.lock().unwrap().take_next() else {
                        continue;
                    };
                    log_debug!(
                        "[thumb] job recv pid={} wid={} target_h={} priority={} queue_ms={}",
                        job.key.pid,
                        job.key.wid,
                        job.target_px_h,
                        job.priority.label(),
                        job.enqueued_at.elapsed().as_millis()
                    );
                    run_capture_job(job);
                    // 捕获期间若来了更高清、更高优先级或更新的 activation 请求，保留
                    // active 并自行补发，避免恢复后的刷新被更早任务吞掉。
                    // If a higher-resolution, higher-priority, or newer activation request
                    // arrived during capture, keep the key active and self-enqueue a follow-up
                    // so an earlier job cannot swallow the post-resume refresh.
                    let follow_up = CAPTURE_STATE.lock().unwrap().finish(job);
                    if follow_up && retry_tx.send(()).is_err() {
                        CAPTURE_STATE.lock().unwrap().desired.remove(&job.key);
                    }
                }
            })
            .expect("spawn thumb-capture worker");
        tx
    })
}

fn run_capture_job(job: CaptureJob) {
    let key = job.key;
    if !CAPTURE_STATE.lock().unwrap().is_current(job) {
        log_debug!(
            "[thumb] job skipped stale pid={} wid={} priority={}",
            key.pid,
            key.wid,
            job.priority.label()
        );
        return;
    }
    // 每个任务前重新 preflight:未授权时静默跳过(运行中授权后自动恢复)。
    // Re-preflight per job: silently skip while unauthorized (auto-resumes once
    // granted mid-run).
    let allowed = capture_allowed();
    let enabled = crate::theme::thumbnails_enabled();
    if !allowed || !enabled {
        log_debug!(
            "[thumb] job skipped (allowed={}, enabled={})",
            allowed,
            enabled
        );
        return;
    }
    if job
        .activation_at
        .is_some_and(|activated_at| !activation_capture_is_valid_now(key.pid, activated_at))
    {
        log_debug!(
            "[thumb] activation job skipped after losing frontmost pid={} wid={}",
            key.pid,
            key.wid
        );
        return;
    }
    let job_started = Instant::now();
    let Some(captured) = (unsafe { capture_window(key.wid, job.target_px_h) }) else {
        log_debug!("[thumb] capture failed pid={} wid={}", key.pid, key.wid);
        return;
    };
    if job
        .activation_at
        .is_some_and(|activated_at| !activation_capture_is_valid_now(key.pid, activated_at))
    {
        unsafe {
            CFRelease(captured.thumb.img);
        }
        log_debug!(
            "[thumb] activation result discarded after losing frontmost pid={} wid={}",
            key.pid,
            key.wid
        );
        return;
    }
    // 生命周期校验与缓存写入共用 CAPTURE_STATE 锁。终止路径按同一锁序取消任务并
    // 清缓存，因此结果不可能在 Remove 之后重新插入。
    // Validate lifecycle and write the cache while holding CAPTURE_STATE. Termination
    // takes the same lock before cancellation/cache eviction, so a result cannot be
    // inserted again after removal.
    let state = CAPTURE_STATE.lock().unwrap();
    if !state.is_current(job) {
        unsafe {
            CFRelease(captured.thumb.img);
        }
        log_debug!(
            "[thumb] captured result discarded stale pid={} wid={} priority={}",
            key.pid,
            key.wid,
            job.priority.label()
        );
        return;
    }
    log_debug!(
        "[thumb] captured pid={} wid={} src={}x{} out={}x{} target_h={} priority={} capture_ms={} scale_ms={} total_ms={}",
        key.pid,
        key.wid,
        captured.src_w,
        captured.src_h,
        captured.thumb.w_px,
        captured.thumb.h_px,
        captured.thumb.captured_for_px_h,
        job.priority.label(),
        captured.capture_ms,
        captured.scale_ms,
        job_started.elapsed().as_millis()
    );
    cache_store(key.pid, key.wid, captured.thumb);
    drop(state);
    // 不再按任务来源预先决定是否投递：启动预热也可能在浮窗打开后才完成。
    // Do not decide delivery from the request source: startup pre-generation may
    // also finish after the overlay has opened.
    if overlay_wants(key.pid, key.wid) {
        // 先写队列再跳主线程(handler 消费时有完整缓存)。
        // Queue before hopping to main (the handler sees complete cache entries).
        enqueue_ready_delivery(key);
    }
}

fn activation_capture_is_valid_now(pid: i32, activated_at: Instant) -> bool {
    if !crate::window_collector::app_activation_is_current(pid, activated_at) {
        return false;
    }
    unsafe {
        let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let app: *mut AnyObject = msg_send![workspace, frontmostApplication];
        let frontmost = if app.is_null() {
            false
        } else {
            let frontmost_pid: i32 = msg_send![app, processIdentifier];
            frontmost_pid == pid
        };
        let _: () = msg_send![pool, drain];
        frontmost
    }
}

/// 浮窗是否可见且该窗口当前确实有卡片(投递时效双重校验的 worker 半段)。
/// Whether the overlay is visible AND the window currently has a rendered card
/// (the worker half of the delivery-freshness double check).
fn overlay_wants(pid: i32, wid: u32) -> bool {
    if !crate::theme::thumbnails_enabled() {
        return false;
    }
    let visible = crate::overlay::thumbnail_visible_range();
    let state_opt = crate::TAB_STATE.lock().unwrap();
    match state_opt.as_ref() {
        Some(s) if s.visible => s
            .windows
            .iter()
            .position(|w| w.pid == pid && w.window_id == wid)
            .is_some_and(|index| visible.as_ref().is_none_or(|range| range.contains(&index))),
        None => false,
        Some(_) => false,
    }
}

/// 主线程回调入口(controller 的 thumbnailReady:):清空待投递队列,逐键校验后
/// 就地重建对应卡片。生成期间用户可能已 ↑↓ 或关浮窗,每键都要重新校验。
/// Main-thread callback entry (the controller's thumbnailReady:): drains the
/// pending queue, re-verifies each key, and rebuilds the affected cards in place.
/// The user may have arrowed away or closed the overlay mid-generation, so every
/// key is re-verified.
pub(crate) fn handle_ready_main() {
    if !crate::theme::thumbnails_enabled() {
        READY_QUEUE.lock().unwrap().clear();
        READY_DELIVERY_SCHEDULED.store(false, Ordering::Release);
        return;
    }
    // scheduled=false 与 drain 必须在同一队列锁内完成：否则 worker 可能在两步之间
    // 看到旧的 true、放入新 key 却不再安排回调。
    // Clear scheduled and drain under the same queue lock. Otherwise a worker can
    // observe the old true between those steps, append a key, and leave it without
    // a future callback.
    let keys: Vec<ThumbKey> = {
        let mut ready = READY_QUEUE.lock().unwrap();
        let keys = std::mem::take(&mut *ready);
        READY_DELIVERY_SCHEDULED.store(false, Ordering::Release);
        keys
    };
    if keys.is_empty() {
        return;
    }
    let visible = crate::overlay::thumbnail_visible_range();
    let keys: HashSet<ThumbKey> = keys.into_iter().collect();
    let indices: Vec<usize> = {
        let state_opt = crate::TAB_STATE.lock().unwrap();
        let state = match state_opt.as_ref() {
            Some(s) => s,
            None => return,
        };
        if !state.visible {
            return;
        }
        state
            .windows
            .iter()
            .enumerate()
            .filter(|(index, window)| {
                keys.contains(&ThumbKey {
                    pid: window.pid,
                    wid: window.window_id,
                }) && visible.as_ref().is_none_or(|range| range.contains(index))
            })
            .map(|(index, _)| index)
            .collect()
    };
    if !indices.is_empty() {
        crate::overlay::refresh_thumbnail_previews(&indices);
    }
}

static READY_QUEUE: Mutex<Vec<ThumbKey>> = Mutex::new(Vec::new());
static READY_DELIVERY_SCHEDULED: AtomicBool = AtomicBool::new(false);

/// 多个 worker 完成通知共享一个主线程 selector；handler 一次清空当前 key 批次。
/// Multiple worker completions share one outstanding main-thread selector; the
/// handler drains the current key batch in one pass.
fn enqueue_ready_delivery(key: ThumbKey) {
    READY_QUEUE.lock().unwrap().push(key);
    if READY_DELIVERY_SCHEDULED.swap(true, Ordering::AcqRel) {
        return;
    }
    let ctrl = match *crate::CONTROLLER.lock().unwrap() {
        Some(c) => c.0,
        None => {
            READY_DELIVERY_SCHEDULED.store(false, Ordering::Release);
            return;
        }
    };
    unsafe {
        let _: () = msg_send![
            ctrl,
            performSelectorOnMainThread: sel!(thumbnailReady:),
            withObject: std::ptr::null::<AnyObject>(),
            waitUntilDone: false
        ];
    }
}

/// 截取一个窗口:CGSHWCaptureWindowList(count=1)→ 取首张 CGImage →
/// 等比缩到目标像素高(降内存:Retina 原生帧可达数十 MB)。
/// Capture one window: CGSHWCCaptureWindowList (count=1) -> first CGImage ->
/// proportionally downscale to the target pixel height (native retina frames can
/// reach tens of MB).
struct CapturedWindow {
    thumb: CachedThumb,
    src_w: u32,
    src_h: u32,
    capture_ms: u128,
    scale_ms: u128,
}

unsafe fn capture_window(wid: u32, target_px_h: u32) -> Option<CapturedWindow> {
    let cap = *CGS_CAPTURE_LIST.as_ref()?;
    // 连接 ID 进程内恒定,缓存一次;0 = 获取失败(私有符号缺失)。
    // The connection ID is process-wide constant; cache it once (0 = unavailable).
    let cid =
        *CONNECTION_ID.get_or_init(|| CGS_MAIN_CONN.as_ref().map(|f| unsafe { f() }).unwrap_or(0));
    if cid == 0 {
        return None;
    }
    let wids = [wid];
    // 显式请求 Retina 原生像素；nominalResolution 只给逻辑点尺寸，小窗口在 4K/5K
    // 屏上放大后仍会发糊，即使后续目标高度提高也无法补回源细节。
    // Explicitly request native Retina pixels. nominalResolution only returns point-sized
    // content, so small windows stay blurry on 4K/5K displays even with a larger target later.
    let opts = CGS_CAPTURE_BEST_RESOLUTION | CGS_CAPTURE_IGNORE_GLOBAL_CLIP_SHAPE;
    let capture_started = Instant::now();
    let arr = cap(cid, wids.as_ptr(), 1, opts);
    if arr.is_null() {
        return None;
    }
    let n = CFArrayGetCount(arr);
    let raw = if n > 0 {
        CFArrayGetValueAtIndex(arr, 0)
    } else {
        std::ptr::null()
    };
    if raw.is_null() {
        CFRelease(arr);
        return None;
    }
    CFRetain(raw); // 数组即将释放,自留一份 / the array goes away; keep our own ref
    CFRelease(arr);
    let capture_ms = capture_started.elapsed().as_millis();

    let src_w = CGImageGetWidth(raw) as u32;
    let src_h = CGImageGetHeight(raw) as u32;
    let target_px_h = target_px_h.clamp(BASE_TARGET_PX_H, MAX_TARGET_PX_H);
    let (tw, th) = fit_target(src_w, src_h, target_px_h);
    let scale_started = Instant::now();
    let img = if tw == src_w && th == src_h {
        raw
    } else {
        let scaled = downscale_cgimage(raw, tw, th);
        CFRelease(raw);
        if scaled.is_null() {
            return None;
        }
        scaled
    };
    if img.is_null() {
        return None;
    }
    Some(CapturedWindow {
        thumb: CachedThumb {
            img,
            w_px: tw,
            h_px: th,
            captured_for_px_h: target_px_h,
            captured: Instant::now(),
        },
        src_w,
        src_h,
        capture_ms,
        scale_ms: scale_started.elapsed().as_millis(),
    })
}

/// CGBitmapContext 重绘降采样(纯 CoreGraphics,线程安全;方向与原图一致)。
/// Downscale by redrawing through a CGBitmapContext (pure CoreGraphics,
/// thread-safe; orientation matches the source).
unsafe fn downscale_cgimage(src: *const c_void, tw: u32, th: u32) -> *const c_void {
    if tw == 0 || th == 0 {
        return std::ptr::null();
    }
    // Device RGB 色彩空间不可变且线程安全，进程级复用；每帧创建/释放没有收益。
    // Device RGB color spaces are immutable and thread-safe, so reuse one for the
    // process instead of creating and releasing it for every frame.
    static RGB_COLOR_SPACE: LazyLock<ConstPtr> =
        LazyLock::new(|| ConstPtr(unsafe { CGColorSpaceCreateDeviceRGB() }));
    let cs = RGB_COLOR_SPACE.0;
    let ctx = CGBitmapContextCreate(
        std::ptr::null_mut(),
        tw as usize,
        th as usize,
        8,
        (tw as usize) * 4,
        cs,
        BITMAP_PREMULTIPLIED_LAST,
    );
    if ctx.is_null() {
        return std::ptr::null();
    }
    CGContextDrawImage(
        ctx,
        CGRect {
            x: 0.0,
            y: 0.0,
            w: tw as f64,
            h: th as f64,
        },
        src,
    );
    let out = CGBitmapContextCreateImage(ctx);
    CFRelease(ctx);
    out
}

static CONNECTION_ID: OnceLock<u32> = OnceLock::new();

// ========== 召唤期刷新(show_overlay 尾部调用) ==========

fn capture_range_for_visible(visible: Option<Range<usize>>, len: usize) -> Range<usize> {
    let visible = visible.unwrap_or(0..len);
    visible
        .start
        .min(len)
        .saturating_sub(VISIBLE_PREFETCH_MARGIN)
        ..visible
            .end
            .min(len)
            .saturating_add(VISIBLE_PREFETCH_MARGIN)
            .min(len)
}

/// 召唤期补拍:对当前可见区间及两侧预取范围中非最小化、有 bounds 的窗口检查
/// 缓存状态。缺失帧和前台 App 的过期帧异步重截；后台 App 的已有帧不因 TTL
/// 或屏幕倍率变化而覆盖。pending/in-flight 键由 enqueue_job 合并；选中窗口排最前。
/// 选中项从 TAB_STATE 内部读取,调用方只在 show_overlay 尾部触发一次。
/// Summon-time refresh: for non-minimized windows with valid bounds in the visible
/// slice plus its prefetch margins, request async recaptures for missing frames and
/// stale frontmost-app frames. Existing background frames survive TTL and display-scale
/// changes. enqueue_job coalesces keys already pending/in-flight; the selected window
/// is requested first.
/// The selection is read from TAB_STATE internally; callers just invoke once at
/// the end of show_overlay.
pub(crate) fn refresh_for_summon(required_px_h: u32) {
    if !crate::theme::thumbnails_enabled() {
        return;
    }
    if !capture_allowed() {
        // 未授权:本次召唤静默跳过;若尚未弹过授权框则申请一次。
        // Unauthorized: skip silently; request permission once if never prompted.
        request_permission_once();
        return;
    }
    // 与 worker 的 overlay_wants 保持锁序：先可见区间，后 TAB_STATE。
    // Match the worker's overlay_wants lock order: visible range before TAB_STATE.
    let visible_snapshot = crate::overlay::thumbnail_visible_range();
    let (jobs, missing, frontmost_stale, background_last_good): (
        Vec<(i32, u32, CapturePriority)>,
        usize,
        usize,
        usize,
    ) = {
        let state_opt = crate::TAB_STATE.lock().unwrap();
        let Some(state) = state_opt.as_ref() else {
            return;
        };
        if !state.visible {
            return;
        }
        let selected = state
            .windows
            .get(state.selected)
            .map(|w| (w.pid, w.window_id));
        // is_active 只标记前台 App 的一个代表窗口；同 PID 的其他窗口也应允许刷新。
        // is_active marks one representative window only; sibling windows from the
        // same frontmost PID must be eligible for refresh too.
        let frontmost_pid = state.windows.iter().find(|w| w.is_active).map(|w| w.pid);
        let capture_range =
            capture_range_for_visible(visible_snapshot.clone(), state.windows.len());
        let decisions: Vec<(usize, i32, u32, SummonRefreshDecision)> = state
            .windows
            .iter()
            .enumerate()
            .filter(|(index, _)| capture_range.contains(index))
            .filter(|(_, w)| !w.minimized && w.bounds.2 > 0.0 && w.bounds.3 > 0.0)
            .map(|(index, w)| {
                (
                    index,
                    w.pid,
                    w.window_id,
                    cached_summon_refresh_decision(
                        w.pid,
                        w.window_id,
                        required_px_h,
                        frontmost_pid == Some(w.pid),
                    ),
                )
            })
            .collect();
        let missing = decisions
            .iter()
            .filter(|(_, _, _, decision)| *decision == SummonRefreshDecision::Missing)
            .count();
        let frontmost_stale = decisions
            .iter()
            .filter(|(_, _, _, decision)| *decision == SummonRefreshDecision::FrontmostStale)
            .count();
        let background_last_good = decisions
            .iter()
            .filter(|(_, _, _, decision)| *decision == SummonRefreshDecision::BackgroundLastGood)
            .count();
        let jobs: Vec<(i32, u32, CapturePriority)> = decisions
            .into_iter()
            .filter(|(_, _, _, decision)| {
                matches!(
                    decision,
                    SummonRefreshDecision::Missing | SummonRefreshDecision::FrontmostStale
                )
            })
            .map(|(index, pid, wid, _)| {
                let priority = if Some((pid, wid)) == selected {
                    CapturePriority::Selected
                } else if visible_snapshot
                    .as_ref()
                    .is_some_and(|range| range.contains(&index))
                {
                    CapturePriority::Visible
                } else {
                    CapturePriority::Prefetch
                };
                (pid, wid, priority)
            })
            .collect();
        (jobs, missing, frontmost_stale, background_last_good)
    };
    let requested = jobs.len();
    let mut enqueued = 0;
    for (pid, wid, priority) in jobs {
        enqueued += usize::from(enqueue_job(pid, wid, required_px_h, priority));
    }
    log_debug!(
        "[thumb] summon refresh: missing={} frontmost_stale={} background_last_good={} requested={} enqueued={} target_h={}",
        missing,
        frontmost_stale,
        background_last_good,
        requested,
        enqueued,
        required_px_h
    );
}

fn activation_capture_is_valid(
    pid: i32,
    activation_is_current: bool,
    frontmost_pid: Option<i32>,
) -> bool {
    activation_is_current && frontmost_pid == Some(pid)
}

/// WebView 等内容进程在 App 从后台恢复时可能晚于 AppKit 标题栏重绘。延迟后再次
/// 核对激活 token 与系统前台 PID，只有仍在前台才刷新最后一张正常缓存。
/// Web content may resume later than its AppKit title bar when an app returns from the
/// background. Recheck the activation token and system frontmost PID after a delay before
/// replacing the last-known-good cache entry.
pub(crate) fn refresh_after_activation(pid: i32, wid: u32, activated_at: Instant) {
    if !crate::theme::thumbnails_enabled() {
        return;
    }
    let pid_generation = CAPTURE_STATE.lock().unwrap().pid_generation(pid);
    let _ = std::thread::Builder::new()
        .name("oh-my-tab-thumb-activation".into())
        .spawn(move || unsafe {
            std::thread::sleep(Duration::from_millis(ACTIVATION_CAPTURE_DELAY_MS));
            let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
            let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
            let front_app: *mut AnyObject = msg_send![workspace, frontmostApplication];
            let frontmost_pid = if front_app.is_null() {
                None
            } else {
                let current_pid: i32 = msg_send![front_app, processIdentifier];
                Some(current_pid)
            };
            let activation_is_current =
                crate::window_collector::app_activation_is_current(pid, activated_at);
            if activation_capture_is_valid(pid, activation_is_current, frontmost_pid) {
                let target_px_h = cached_target_px_height(pid, wid);
                let enqueued =
                    enqueue_activation_job(pid, wid, target_px_h, activated_at, pid_generation);
                log_debug!(
                    "[thumb] activation refresh: pid={} wid={} enqueued={} target_h={}",
                    pid,
                    wid,
                    enqueued,
                    target_px_h
                );
            } else {
                log_debug!(
                    "[thumb] activation refresh skipped: pid={} wid={} stale_or_background",
                    pid,
                    wid
                );
            }
            let _: () = msg_send![pool, drain];
        });
}

// ========== 常驻监视线程(AXObserver + 自有 CFRunLoop) ==========

static STARTED: AtomicBool = AtomicBool::new(false);
static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

/// 裸 CFRunLoopRef 的 Send+Sync 包装(static 存储要求;指针只在观察者线程解引用,
/// 其他线程仅用于 CFRunLoopWakeUp 唤醒——CFWakeUp 线程安全)。
/// Send+Sync wrapper for the raw CFRunLoopRef (required for statics). The pointer
/// is only dereferenced on the observer thread; other threads merely use it for
/// CFRunLoopWakeUp, which is thread-safe.
struct RunLoopSlot(Mutex<Option<*mut c_void>>);
unsafe impl Send for RunLoopSlot {}
unsafe impl Sync for RunLoopSlot {}
static OBSERVER_RL: RunLoopSlot = RunLoopSlot(Mutex::new(None));
/// 命令注入源(观察者线程创建后存入,任意线程 Signal 唤醒命令处理)。
/// The command-injection source (stashed once the observer thread creates it; any
/// thread signals it to wake command processing).
static CMD_SOURCE: RunLoopSlot = RunLoopSlot(Mutex::new(None));

/// 观察者线程命令:安装/卸载某 PID 的 observer(NSWorkspace 通知跨线程转发而来)。
/// Observer-thread commands: install/uninstall a PID's observer (forwarded across
/// threads from NSWorkspace notifications).
enum ObsCmd {
    Install(i32),
    Remove(i32),
}
static CMD_TX: OnceLock<flume::Sender<ObsCmd>> = OnceLock::new();
static CMD_RX: OnceLock<flume::Receiver<ObsCmd>> = OnceLock::new();

/// 已安装的观察者:pid → (AXObserverRef, runloop source)。卸载时成对清理。
/// 同为裸指针,需要 Send+Sync 包装(增删只在观察者线程,读检查任意线程)。
/// Installed observers: pid -> (AXObserverRef, runloop source); removed as a pair.
/// Raw pointers again -- needs the Send+Sync wrapper (inserts/removes happen on
/// the observer thread; lookups from any thread).
type AxObserverRef = *mut c_void;
struct InstalledMap(Mutex<HashMap<i32, (AxObserverRef, *mut c_void)>>);
unsafe impl Send for InstalledMap {}
unsafe impl Sync for InstalledMap {}
static INSTALLED: LazyLock<InstalledMap> =
    LazyLock::new(|| InstalledMap(Mutex::new(HashMap::new())));

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXObserverCreate(
        pid: i32,
        callback: unsafe extern "C" fn(AxObserverRef, *const c_void, *const c_void, *mut c_void),
        out: *mut AxObserverRef,
    ) -> i32;
    fn AXObserverAddNotification(
        observer: AxObserverRef,
        element: *const c_void,
        notification: *const c_void,
        refcon: *mut c_void,
    ) -> i32;
    fn AXObserverGetRunLoopSource(observer: AxObserverRef) -> *mut c_void;
    // 签名与 window_collector 的声明保持一致(AXUIElementRef = *const c_void),
    // 避免跨模块重复 extern 声明的签名冲突警告。
    // Keep the signature identical to window_collector's declaration
    // (AXUIElementRef = *const c_void) to avoid cross-module duplicate-extern
    // signature-clash warnings.
    fn AXUIElementCreateApplication(pid: i32) -> *const c_void;
    fn AXUIElementGetPid(element: *const c_void, pid: *mut i32) -> i32;
    // kAXWindowCreatedNotification 常量不能直接 extern 链接(部分工具链报 undefined
    // symbol),改走下方 dlsym 运行时解析(与 _AXUIElementGetWindow 同款)。
    // The kAXWindowCreatedNotification constant cannot be linked directly (some
    // toolchains report an undefined symbol); it is resolved at runtime via dlsym
    // below (same approach as _AXUIElementGetWindow).
    fn CFRunLoopRemoveSource(rl: *mut c_void, src: *mut c_void, mode: *const c_void);
    fn CFRunLoopSourceCreate(
        alloc: *const c_void,
        order: isize,
        ctx: *const CFRunLoopSourceContext,
    ) -> *mut c_void;
    fn CFRunLoopSourceSignal(src: *mut c_void);
    fn CFRunLoopWakeUp(rl: *mut c_void);
    // CFRunLoopGetCurrent/Run/AddSource 与 kCFRunLoopDefaultMode 复用 event_tap 的
    // pub(crate) 声明(见下方 use),不在此重复。
    // CFRunLoopGetCurrent/Run/AddSource and kCFRunLoopDefaultMode are reused from
    // event_tap's pub(crate) declarations (see the use below), not redeclared here.
}
use crate::event_tap::{
    kCFRunLoopDefaultMode, CFRunLoopAddSource, CFRunLoopGetCurrent, CFRunLoopRun,
};

/// CFRunLoopSource 的 perform 回调上下文(只用 perform 字段)。
/// CFRunLoopSource context (only the perform field is used).
#[repr(C)]
struct CFRunLoopSourceContext {
    version: i64,
    info: *mut c_void,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
    equal: *const c_void,
    hash: *const c_void,
    schedule: *const c_void,
    cancel: *const c_void,
    perform: Option<unsafe extern "C" fn(*mut c_void)>,
}

/// 裸 CF 指针的 Send+Sync 包装(值只在 LazyLock 初始化时解析一次,只读使用)。
/// Send+Sync wrapper for the raw CF pointer (resolved once at LazyLock init,
/// read-only afterwards).
struct ConstPtr(*const c_void);
unsafe impl Send for ConstPtr {}
unsafe impl Sync for ConstPtr {}
impl Clone for ConstPtr {
    fn clone(&self) -> Self {
        *self
    }
}
impl Copy for ConstPtr {}

/// kAXWindowCreatedNotification 的等价物。该常量在新系统上不再作为动态符号导出
/// (extern 链接与 dlsym 均不可得,实测),但 AX 通知名按**字符串值**比较,字面量
/// "AXWindowCreated" 与系统常量语义完全等价(注册与回调都用它)。
/// An equivalent of kAXWindowCreatedNotification. The constant is no longer
/// exported as a dynamic symbol on current macOS (both extern linking and dlsym
/// fail -- verified empirically), but AX notification names compare by STRING
/// VALUE, so the literal "AXWindowCreated" is semantically identical for both
/// registration and callback matching.
static AX_WINDOW_CREATED: LazyLock<ConstPtr> = LazyLock::new(|| unsafe {
    // make_nsstring +1 常驻(静态持有);转 *const c_void 与 CF API 对接。
    // make_nsstring +1 lives for the process lifetime (statically held); cast
    // to *const c_void for the CF APIs.
    let s = crate::ffi::make_nsstring("AXWindowCreated");
    ConstPtr(std::mem::transmute::<*mut AnyObject, *const c_void>(s))
});

/// 启动常驻监视线程(幂等)。线程职责:为现有运行中 App 装 AXObserver → 对已有
/// 标准窗口做启动预生成 → 运行 runloop 处理后续 Install/Remove 命令与 AX 事件。
/// Start the resident listener thread (idempotent). Duties: install AXObservers
/// for running apps -> pre-generate existing standard windows -> run the runloop
/// serving Install/Remove commands and AX events.
pub(crate) fn start() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let (tx, rx) = flume::unbounded::<ObsCmd>();
    let _ = CMD_TX.set(tx);
    let _ = CMD_RX.set(rx);
    STOP_REQUESTED.store(false, Ordering::SeqCst);
    std::thread::Builder::new()
        .name("thumb-observer".into())
        .spawn(|| unsafe {
            let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
            // 先发布命令 source/runloop，再做 AX 安装与预热。启动期间到达的
            // Launch/Terminate 命令会保持 source signaled，进入 runloop 后立即处理，
            // 不会因 source 尚不存在而滞留到下一次偶然唤醒。
            // Publish the command source/runloop before AX installation and prewarming.
            // Launch/Terminate commands arriving during startup keep the source signaled
            // and drain immediately once the runloop starts instead of waiting for a later wake.
            let src = CFRunLoopSourceCreate(
                std::ptr::null(),
                0,
                &CFRunLoopSourceContext {
                    version: 0,
                    info: std::ptr::null_mut(),
                    retain: std::ptr::null(),
                    release: std::ptr::null(),
                    copy_description: std::ptr::null(),
                    equal: std::ptr::null(),
                    hash: std::ptr::null(),
                    schedule: std::ptr::null(),
                    cancel: std::ptr::null(),
                    perform: Some(drain_obs_commands),
                },
            );
            let rl = CFRunLoopGetCurrent();
            if !src.is_null() {
                CFRunLoopAddSource(rl, src, kCFRunLoopDefaultMode);
                *CMD_SOURCE.0.lock().unwrap() = Some(src);
            }
            *OBSERVER_RL.0.lock().unwrap() = Some(rl);
            // source 发布前极小窗口内到达的命令没有机会 signal；主动清空一次补上。
            // Commands arriving in the tiny window before source publication could not
            // signal it, so explicitly drain once after publication.
            drain_obs_commands(std::ptr::null_mut());

            // 本线程枚举运行中 App 并装观察者(AXObserverCreate 必须在将要 pump 其
            // runloop source 的线程上调用)。
            // Enumerate running apps and install observers here (AXObserverCreate
            // must run on the thread whose runloop pumps the observer's source).
            let pids = regular_running_pids();
            for pid in &pids {
                install_observer_for_pid(*pid);
            }
            log_debug!(
                "[thumb] observer thread started: {} regular apps observed, capture_allowed={}",
                pids.len(),
                capture_allowed()
            );
            pregen_startup_windows();
            if !STOP_REQUESTED.load(Ordering::SeqCst) {
                CFRunLoopRun();
            }
            *OBSERVER_RL.0.lock().unwrap() = None;
            let _: () = msg_send![pool, drain];
        })
        .expect("spawn thumb-observer thread");
}

/// runloop source 的 perform:清空命令队列执行 Install/Remove。
/// The runloop source's perform: drains and executes the command queue.
unsafe extern "C" fn drain_obs_commands(_info: *mut c_void) {
    let Some(rx) = CMD_RX.get() else {
        return;
    };
    while let Ok(cmd) = rx.try_recv() {
        match cmd {
            ObsCmd::Install(pid) => {
                install_observer_for_pid(pid);
                pregen_windows_for_pid(pid);
            }
            ObsCmd::Remove(pid) => {
                if let Some((obs, src)) = INSTALLED.0.lock().unwrap().remove(&pid) {
                    let rl = CFRunLoopGetCurrent();
                    CFRunLoopRemoveSource(rl, src, kCFRunLoopDefaultMode);
                    CFRelease(obs as *const c_void);
                }
                // 该 App 的缓存帧一并驱逐:死 App 的帧不会再被展示,占着 LRU 槽位
                // 只会挤掉活窗口的帧。
                // Evict the dead app's cached frames too: they will never be shown
                // again and would only crowd out live windows' frames.
                let evicted = CACHE.lock().unwrap().remove_where(|(k, _)| k.pid == pid);
                for t in evicted {
                    CFRelease(t.img);
                }
            }
        }
    }
}

/// NSWorkspaceDidLaunch 转发点(main 线程调用)。新 App 装 observer + 补拍既有窗口。
/// Forwarding point for NSWorkspaceDidLaunch (called on main). Installs the new
/// app's observer and pre-generates its existing windows.
pub(crate) fn app_launched(pid: i32) {
    // 即使服务尚未启动也先恢复 PID 活跃状态，覆盖极端的快速退出/PID 复用窗口。
    // Restore PID liveness even before the service starts, covering rapid exit/PID
    // reuse during startup.
    CAPTURE_STATE.lock().unwrap().activate_pid(pid);
    if !STARTED.load(Ordering::SeqCst) || pid == std::process::id() as i32 {
        return;
    }
    if let Some(tx) = CMD_TX.get() {
        let _ = tx.send(ObsCmd::Install(pid));
        signal_observer_runloop();
    }
}

/// NSWorkspaceDidTerminate 转发点:立即取消捕获，再异步卸载 observer 与缓存。
/// Forwarding point for NSWorkspaceDidTerminate: cancel captures immediately,
/// then remove the observer and cached frames asynchronously.
pub(crate) fn app_terminated(pid: i32) {
    // 同一生命周期锁内使 queued/in-flight 捕获失效并清缓存；observer 线程随后只需
    // 卸载 AX source。该锁序保证迟到截图无法在清理后重新写回。
    // Invalidate queued/in-flight captures and clear the cache under the same lifecycle
    // lock; the observer thread then only needs to remove the AX source. This lock order
    // prevents a late capture from being inserted after cleanup.
    let mut state = CAPTURE_STATE.lock().unwrap();
    state.cancel_pid(pid);
    let evicted = CACHE
        .lock()
        .unwrap()
        .remove_where(|(key, _)| key.pid == pid);
    drop(state);
    for thumb in evicted {
        unsafe {
            CFRelease(thumb.img);
        }
    }
    if let Some(tx) = CMD_TX.get() {
        let _ = tx.send(ObsCmd::Remove(pid));
        signal_observer_runloop();
    }
}

/// 从任意线程唤醒观察者 runloop 处理刚投递的命令:Signal 唤醒 source(触发
/// perform 清空命令队列)+ WakeUp 确保 runloop 醒来。
/// Wake the observer runloop from any thread: Signal marks the source (its perform
/// drains the command queue) and WakeUp makes sure the runloop actually wakes.
fn signal_observer_runloop() {
    let src = CMD_SOURCE.0.lock().unwrap();
    if let Some(src) = *src {
        unsafe {
            CFRunLoopSourceSignal(src);
        }
    }
    drop(src);
    let rl = OBSERVER_RL.0.lock().unwrap();
    if let Some(rl) = *rl {
        unsafe {
            CFRunLoopWakeUp(rl);
        }
    }
}

/// 当前激活策略为 .regular 的运行中 App(排除自身)。菜单栏小工具/后台进程没有
/// 标准窗口,装了 observer 也只会白耗 AX 往返。
/// Running apps with .regular activation policy (excluding ourselves). Menu-bar
/// agents/background processes have no standard windows -- observing them would
/// burn AX round-trips for nothing.
fn regular_running_pids() -> Vec<i32> {
    unsafe {
        let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
        let mut out: Vec<i32> = Vec::new();
        let ws: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let running: *mut AnyObject = msg_send![ws, runningApplications];
        let count: usize = msg_send![running, count];
        for i in 0..count {
            let app: *mut AnyObject = msg_send![running, objectAtIndex: i];
            let pid: i32 = msg_send![app, processIdentifier];
            // NSApplicationActivationPolicyRegular = 0
            let policy: i64 = msg_send![app, activationPolicy];
            if pid > 0 && policy == 0 && pid != std::process::id() as i32 {
                out.push(pid);
            }
        }
        let _: () = msg_send![pool, drain];
        out
    }
}

/// 为单个 PID 安装 AXObserver(必须在观察者线程调用)。失败仅记日志——个别 App
/// 拒绝 AX 观察属常态,不影响其他 App。
/// Install an AXObserver for one PID (MUST run on the observer thread). Failures
/// are logged only -- some apps refuse AX observation, which is normal.
unsafe fn install_observer_for_pid(pid: i32) {
    {
        let installed = INSTALLED.0.lock().unwrap();
        if installed.contains_key(&pid) {
            return;
        }
    }
    let mut obs: AxObserverRef = std::ptr::null_mut();
    if AXObserverCreate(pid, thumb_ax_observer, &mut obs) != 0 || obs.is_null() {
        log_debug!("[thumb] AXObserverCreate failed for pid={}", pid);
        return;
    }
    let app_el = AXUIElementCreateApplication(pid);
    if app_el.is_null() {
        CFRelease(obs as *const c_void);
        return;
    }
    let err = AXObserverAddNotification(obs, app_el, AX_WINDOW_CREATED.0, std::ptr::null_mut());
    if err != 0 {
        log_debug!(
            "[thumb] add kAXWindowCreated failed for pid={} err={}",
            pid,
            err
        );
        CFRelease(app_el);
        CFRelease(obs as *const c_void);
        return;
    }
    let src = AXObserverGetRunLoopSource(obs);
    let rl = CFRunLoopGetCurrent();
    CFRunLoopAddSource(rl, src, kCFRunLoopDefaultMode);
    CFRelease(app_el); // observer 已持有所需引用 / the observer holds what it needs
    INSTALLED.0.lock().unwrap().insert(pid, (obs, src));
}

/// 启动预热直接复用 AppState 已完成 AX 配对的 MRU 快照，避免按 PID 再做一轮 AX
/// 查询。只取有 bounds、非最小化的前 STARTUP_PREWARM_MAX 个窗口。
/// Startup prewarming reuses AppState's already AX-paired MRU snapshot instead of
/// repeating one AX query per PID. It takes only the first STARTUP_PREWARM_MAX
/// non-minimized windows with usable bounds.
unsafe fn pregen_startup_windows() {
    if !crate::theme::thumbnails_enabled() || !capture_allowed() {
        log_debug!("[thumb] startup prewarm skipped (disabled or unauthorized)");
        return;
    }
    let (jobs, eligible) = {
        let state = crate::TAB_STATE.lock().unwrap();
        let Some(state) = state.as_ref() else {
            return;
        };
        let capture_state = CAPTURE_STATE.lock().unwrap();
        let eligible: Vec<(i32, u32, u64)> = state
            .windows
            .iter()
            .filter(|window| {
                !window.minimized
                    && window.window_id != 0
                    && window.bounds.2 > 0.0
                    && window.bounds.3 > 0.0
            })
            .map(|window| {
                (
                    window.pid,
                    window.window_id,
                    capture_state.pid_generation(window.pid),
                )
            })
            .collect();
        let jobs = eligible
            .iter()
            .copied()
            .take(STARTUP_PREWARM_MAX)
            .collect::<Vec<_>>();
        (jobs, eligible.len())
    };
    let mut queued = 0;
    for (pid, wid, pid_generation) in &jobs {
        queued += usize::from(enqueue_job_for_generation(
            *pid,
            *wid,
            BASE_TARGET_PX_H,
            CapturePriority::Startup,
            *pid_generation,
        ));
    }
    log_debug!(
        "[thumb] startup prewarm: eligible={} bounded={} queued={}",
        eligible,
        jobs.len(),
        queued
    );
}

/// 新启动 App 的既有标准窗口补拍；启动初始批次走 pregen_startup_windows。
/// Pre-generate existing standard windows for a newly launched app; the initial
/// startup batch uses pregen_startup_windows instead.
unsafe fn pregen_windows_for_pid(pid: i32) {
    if !crate::theme::thumbnails_enabled() || !capture_allowed() {
        return;
    }
    let pid_generation = CAPTURE_STATE.lock().unwrap().pid_generation(pid);
    let Some(windows) = crate::window_collector::get_ax_windows_for_pid(pid) else {
        log_debug!("[thumb] pregen pid={}: AX query failed", pid);
        return;
    };
    let mut queued = 0;
    for (wid, _title, minimized) in windows {
        // wid=0 = _AXUIElementGetWindow 解析失败的退化条目,截取必然失败。
        // wid=0 = degenerate entries whose _AXUIElementGetWindow failed; capturing
        // them always fails.
        if minimized || wid == 0 {
            continue; // 最小化窗口无渲染缓冲,截取必然失败 / no backing store while minimized
        }
        queued += usize::from(enqueue_job_for_generation(
            pid,
            wid,
            BASE_TARGET_PX_H,
            CapturePriority::NewWindow,
            pid_generation,
        ));
    }
    log_debug!("[thumb] pregen pid={}: {} windows queued", pid, queued);
}

/// AXObserver 回调:kAXWindowCreated → 解析新窗口 cgwid → 防抖 300ms 后预生成。
/// 防抖放独立短命线程,避免阻塞观察者 runloop。
///
/// The AXObserver callback: kAXWindowCreated -> resolve the new window's cgwid ->
/// pre-generate after a 300ms debounce (on a throwaway thread so the observer
/// runloop never blocks).
unsafe extern "C" fn thumb_ax_observer(
    _observer: AxObserverRef,
    element: *const c_void,
    notification: *const c_void,
    _info: *mut c_void,
) {
    let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
    // 通知名按字符串值比较(字面量与系统常量等价,见 AX_WINDOW_CREATED)。
    // Notification names compare by string value (the literal equals the system
    // constant; see AX_WINDOW_CREATED).
    if !notification.is_null()
        && unsafe { CFStringCompare(notification, AX_WINDOW_CREATED.0, 0) } == 0
    {
        let mut wid: u32 = 0;
        if crate::window_collector::ax_window_cgwid(element).is_some_and(|resolved| {
            wid = resolved;
            wid != 0
        }) {
            let mut pid: i32 = 0;
            AXUIElementGetPid(element, &mut pid);
            if pid > 0 && crate::theme::thumbnails_enabled() {
                let pid_generation = CAPTURE_STATE.lock().unwrap().pid_generation(pid);
                // 防抖 300ms:窗口刚创建可能还在布局/白屏。
                // Debounce 300ms: brand-new windows may still be laying out / blank.
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(300));
                    if crate::theme::thumbnails_enabled() && capture_allowed() {
                        enqueue_job_for_generation(
                            pid,
                            wid,
                            BASE_TARGET_PX_H,
                            CapturePriority::NewWindow,
                            pid_generation,
                        );
                    }
                });
            }
        }
    }
    let _: () = msg_send![pool, drain];
}

// ========== 单元测试 ==========

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_state_coalesces_pending_and_in_flight_requests() {
        let mut state = CaptureState::default();
        let first = ThumbKey { pid: 10, wid: 20 };
        let other = ThumbKey { pid: 10, wid: 21 };

        // 同一键无论还在队列还是正在捕获都只能登记一次；其他窗口不受影响。
        // The same key registers once whether queued or in-flight; another window
        // remains independent.
        assert!(state.request(first, 512, CapturePriority::Startup));
        assert!(!state.request(first, 512, CapturePriority::Selected));
        assert!(state.request(other, 512, CapturePriority::Visible));
        let job = state.take_next().unwrap();
        assert_eq!(job.key, first);
        assert_eq!(job.priority, CapturePriority::Selected);
        assert_eq!(job.target_px_h, 512);
    }

    #[test]
    fn capture_state_preserves_a_higher_request_arriving_in_flight() {
        let mut state = CaptureState::default();
        let key = ThumbKey { pid: 10, wid: 20 };

        assert!(state.request(key, 512, CapturePriority::Startup));
        let low = state.take_next().unwrap();
        // 低清任务已开始后接入高分屏：请求合并但不重复入队；低清完成时返回 640，
        // worker 据此自动补拍。高清完成后才释放 active。
        // A high-DPI display appears after the low-res job starts: merge without a duplicate
        // queue item; finishing 512 returns 640 for an automatic follow-up. The key is released
        // only after the high-res capture finishes.
        assert!(!state.request(key, 640, CapturePriority::Selected));
        assert!(state.finish(low));
        let high = state.take_next().unwrap();
        assert_eq!(high.target_px_h, 640);
        assert_eq!(high.priority, CapturePriority::Selected);
        assert!(!state.finish(high));
        assert!(state.take_next().is_none());
    }

    #[test]
    fn capture_state_retries_an_in_flight_job_after_priority_promotion() {
        let mut state = CaptureState::default();
        let key = ThumbKey { pid: 10, wid: 20 };

        assert!(state.request(key, 512, CapturePriority::Startup));
        let startup = state.take_next().unwrap();
        assert!(!state.request(key, 512, CapturePriority::Activation));
        assert!(state.finish(startup));
        let activation = state.take_next().unwrap();
        assert_eq!(activation.target_px_h, 512);
        assert_eq!(activation.priority, CapturePriority::Activation);
    }

    #[test]
    fn capture_state_preserves_new_activation_freshness_independent_of_priority() {
        let mut state = CaptureState::default();
        let key = ThumbKey { pid: 10, wid: 20 };
        let first_activation = Instant::now();
        let later_activation = first_activation + Duration::from_millis(1);

        assert!(state.request_activation(key, 512, first_activation, 0));
        let first = state.take_next().unwrap();
        assert!(!state.request_activation(key, 512, later_activation, 0));
        assert!(state.finish(first));
        let later = state.take_next().unwrap();
        assert_eq!(later.activation_at, Some(later_activation));

        // 显式选中是独立的用户需求，不再受旧 activation 的前台门控；状态变化
        // 仍触发 follow-up，避免正在执行的旧任务吞掉它。
        // An explicit selection is an independent user demand and clears old activation
        // gating; the state change still schedules a follow-up instead of being swallowed.
        assert!(!state.request(key, 512, CapturePriority::Selected));
        assert!(state.finish(later));
        let selected = state.take_next().unwrap();
        assert_eq!(selected.priority, CapturePriority::Selected);
        assert_eq!(selected.activation_at, None);
    }

    #[test]
    fn visible_capture_range_adds_bounded_prefetch_margins() {
        assert_eq!(capture_range_for_visible(Some(10..20), 40), 6..24);
        assert_eq!(capture_range_for_visible(Some(0..5), 40), 0..9);
        assert_eq!(capture_range_for_visible(Some(36..40), 40), 32..40);
        assert_eq!(capture_range_for_visible(None, 40), 0..40);
        assert_eq!(capture_range_for_visible(Some(50..60), 40), 36..40);
    }

    #[test]
    fn capture_state_finish_allows_failed_job_to_retry() {
        let mut state = CaptureState::default();
        let key = ThumbKey { pid: 10, wid: 20 };

        assert!(state.request(key, 512, CapturePriority::Startup));
        let job = state.take_next().unwrap();
        // worker 对成功、失败和权限跳过统一调用 finish；之后下一次召唤可重试。
        // The worker calls finish after success, failure, or a permission skip;
        // the next summon can then retry.
        assert!(!state.finish(job));
        assert!(state.request(key, 512, CapturePriority::Startup));
    }

    #[test]
    fn capture_state_cancels_queued_and_in_flight_jobs_by_pid_generation() {
        let mut state = CaptureState::default();
        let running_key = ThumbKey { pid: 10, wid: 20 };
        let queued_key = ThumbKey { pid: 10, wid: 21 };
        assert!(state.request(running_key, 512, CapturePriority::Visible));
        assert!(state.request(queued_key, 512, CapturePriority::Startup));
        let running = state.take_next().unwrap();
        assert!(state.is_current(running));

        state.cancel_pid(10);
        assert!(!state.is_current(running));
        assert!(state.take_next().is_none());
        assert!(!state.request(running_key, 640, CapturePriority::Selected));

        // 只有 launch 才能恢复该 PID；旧 generation 的延迟生产者仍必须被拒绝，且
        // 旧任务 finish 不能删除新进程的请求。
        // Only launch reactivates the PID; a delayed producer carrying the old generation
        // must still be rejected, and finishing the old job cannot remove the new request.
        let old_generation = running.pid_generation;
        state.activate_pid(10);
        assert!(!state.request_for_generation(
            running_key,
            512,
            CapturePriority::NewWindow,
            old_generation,
        ));
        assert!(state.request(running_key, 640, CapturePriority::Selected));
        assert!(!state.finish(running));
        let replacement = state.take_next().unwrap();
        assert_eq!(replacement.target_px_h, 640);
        assert!(state.is_current(replacement));
    }

    #[test]
    fn lru_evicts_by_count_and_returns_victims() {
        let mut lru: Lru<(i32, u32), u64> = Lru::new(2, u64::MAX, |v| *v);
        lru.put((1, 1), 10);
        lru.put((1, 2), 20);
        assert_eq!(lru.len(), 2);
        // 第三个插入挤掉最旧的 (1,1)。
        // A third insert evicts the oldest entry.
        let evicted = lru.put((1, 3), 30);
        assert_eq!(evicted, vec![10]);
        assert_eq!(lru.len(), 2);
        assert!(lru.get(&(1, 1)).is_none());
        assert!(lru.get(&(1, 2)).is_some());
        assert!(lru.get(&(1, 3)).is_some());
    }

    #[test]
    fn lru_read_touch_refreshes_recency() {
        let mut lru: Lru<(i32, u32), u64> = Lru::new(2, u64::MAX, |v| *v);
        lru.put((1, 1), 10);
        lru.put((1, 2), 20);
        // 读 (1,1) 使它变成最近使用,下一个插入应挤掉 (1,2)。
        // Reading (1,1) makes it most-recent; the next insert must evict (1,2).
        assert_eq!(lru.get(&(1, 1)), Some(10));
        let evicted = lru.put((1, 3), 30);
        assert_eq!(evicted, vec![20]);
        assert!(lru.get(&(1, 1)).is_some());
    }

    #[test]
    fn lru_peek_preserves_recency_and_tracks_cost_incrementally() {
        let mut lru: Lru<u32, u64> = Lru::new(2, 30, |v| *v);
        lru.put(1, 10);
        lru.put(2, 12);
        assert_eq!(lru.peek(&1), Some(10));

        // peek 只读元数据，不应把最旧的 1 移到队尾；替换值也必须修正总成本。
        // peek reads metadata without moving oldest key 1 to the back; replacing a
        // value must also adjust the running total cost.
        assert_eq!(lru.put(3, 8), vec![10]);
        assert_eq!(lru.put(2, 20), vec![12]);
        assert_eq!(lru.put(4, 6), vec![8]);
        assert!(lru.peek(&2).is_some());
        assert!(lru.peek(&3).is_none());
        assert!(lru.peek(&4).is_some());
    }

    #[test]
    fn lru_put_same_key_moves_and_reports_old_value() {
        let mut lru: Lru<(i32, u32), u64> = Lru::new(3, u64::MAX, |v| *v);
        lru.put((1, 1), 10);
        lru.put((1, 2), 20);
        lru.put((1, 3), 30);
        // 更新已存在的键:旧值返回供释放,顺序提到队尾,不新增容量占用。
        // Updating an existing key returns the old value for release, moves it to
        // the back, and consumes no extra capacity.
        let evicted = lru.put((1, 2), 99);
        assert_eq!(evicted, vec![20]);
        assert_eq!(lru.len(), 3);
        let evicted = lru.put((1, 4), 40);
        // (1,2) 已提到队尾,条目数超限挤掉的是最旧的 (1,1)。
        // (1,2) was moved to the back; the count overrun evicts the oldest, (1,1).
        assert_eq!(evicted, vec![10]);
        assert!(lru.get(&(1, 1)).is_none());
        assert!(lru.get(&(1, 3)).is_some());
        assert_eq!(lru.get(&(1, 2)), Some(99));
    }

    #[test]
    fn lru_cost_budget_drives_eviction() {
        // 成本上限 15:新帧受保护,每次插入只挤掉上一帧(单帧超预算时保留最新)。
        // Cost budget 15: the newest frame is protected; each insert evicts only
        // the previous frame (an over-budget single frame keeps the newest).
        let mut lru: Lru<u32, u64> = Lru::new(100, 15, |v| *v);
        lru.put(1, 10);
        let evicted = lru.put(2, 20);
        assert_eq!(evicted, vec![10]);
        let evicted = lru.put(3, 30);
        assert_eq!(evicted, vec![20]);
        assert!(lru.get(&3).is_some());
        assert!(lru.get(&1).is_none());
        assert!(lru.get(&2).is_none());
    }

    #[test]
    fn lru_remove_where_drops_matching_entries() {
        let mut lru: Lru<(i32, u32), u64> = Lru::new(10, u64::MAX, |v| *v);
        lru.put((1, 1), 10);
        lru.put((2, 2), 20);
        lru.put((1, 3), 30);
        let removed = lru.remove_where(|(k, _)| k.0 == 1);
        assert_eq!(removed, vec![10, 30]);
        assert!(lru.get(&(2, 2)).is_some());
        assert!(lru.get(&(1, 1)).is_none());
    }

    #[test]
    fn freshness_ttl_boundary() {
        let now = Instant::now();
        // 用构造的偏移验证边界:略小于 TTL 新鲜,达到 TTL 即过期。
        // Constructed offsets prove the boundary: just under TTL is fresh, at TTL stale.
        let captured = now - Duration::from_millis(FRESH_TTL_MS as u64 - 1);
        assert!(is_fresh(captured, now, FRESH_TTL_MS));
        let captured = now - Duration::from_millis(FRESH_TTL_MS as u64);
        assert!(!is_fresh(captured, now, FRESH_TTL_MS));
    }

    #[test]
    fn fresh_cache_still_upgrades_when_the_display_needs_more_pixels() {
        let now = Instant::now();
        let captured = now - Duration::from_millis(100);
        assert!(cached_frame_is_usable(captured, 512, 512, now));
        assert!(!cached_frame_is_usable(captured, 512, 640, now));
        // 切回低需求屏时高清缓存直接复用，不降级重截。
        // A high-resolution frame remains usable after returning to a lower-demand display.
        assert!(cached_frame_is_usable(captured, 640, 512, now));
    }

    #[test]
    fn summon_refresh_preserves_background_last_known_good_frames() {
        let now = Instant::now();
        let stale = now - Duration::from_millis(FRESH_TTL_MS as u64);
        let fresh = now - Duration::from_millis(100);

        assert_eq!(
            summon_refresh_decision(None, 640, now, false),
            SummonRefreshDecision::Missing
        );
        assert_eq!(
            summon_refresh_decision(Some((stale, 512)), 640, now, false),
            SummonRefreshDecision::BackgroundLastGood
        );
        assert_eq!(
            summon_refresh_decision(Some((stale, 512)), 640, now, true),
            SummonRefreshDecision::FrontmostStale
        );
        assert_eq!(
            summon_refresh_decision(Some((fresh, 640)), 640, now, false),
            SummonRefreshDecision::Fresh
        );
    }

    #[test]
    fn activation_refresh_requires_current_token_and_frontmost_pid() {
        assert!(activation_capture_is_valid(42, true, Some(42)));
        assert!(!activation_capture_is_valid(42, false, Some(42)));
        assert!(!activation_capture_is_valid(42, true, Some(7)));
        assert!(!activation_capture_is_valid(42, true, None));
    }

    #[test]
    fn target_height_tracks_card_size_and_live_screen_scale() {
        // 多窗口基准卡在 2x 屏不超过 512px；少窗口 1.5x 卡约 271pt，在 2x
        // 4K/5K 屏升级到 640px。非 Retina 外屏无需升级，未来 3x 走 1024px。
        // A base card on 2x fits 512px; a ~271pt 1.5x card upgrades to 640px on a
        // 2x 4K/5K display. A 1x external display needs no upgrade; future 3x uses 1024px.
        assert_eq!(target_px_height(177.5, 2.0), 512);
        assert_eq!(target_px_height(271.25, 2.0), 640);
        assert_eq!(target_px_height(271.25, 1.0), 512);
        assert_eq!(target_px_height(271.25, 3.0), 1024);
        assert_eq!(target_px_height(f64::NAN, 2.0), 512);
    }

    #[test]
    fn fit_size_fits_long_edge_and_letterboxes_short_edge() {
        // 16:9 内容放进 4:3 框:内容更"宽",宽度贴合框、高度按比例缩小(上下留白)。
        // 16:9 content into a 4:3 box: the content is wider, so the width fits the
        // box and the height shrinks proportionally (letterboxed top/bottom).
        let (w, h) = fit_size(160.0, 90.0, 400.0, 300.0);
        assert_eq!(w, 400.0);
        assert!((h - 225.0).abs() < 1e-9);
        // 竖版内容放进横框:内容更"窄",高度贴合框、宽度按比例缩小(左右留白)。
        // Portrait content into a landscape box: the content is narrower, so the
        // height fits the box and the width shrinks (letterboxed left/right).
        let (w, h) = fit_size(90.0, 160.0, 400.0, 300.0);
        assert_eq!(h, 300.0);
        assert!((w - 168.75).abs() < 1e-9);
        // 完全同比例:恰好铺满。
        // Same aspect ratio: an exact fill.
        let (w, h) = fit_size(200.0, 100.0, 400.0, 200.0);
        assert_eq!((w, h), (400.0, 200.0));
        // 退化输入回退为目标框尺寸(不产生负值/NaN)。
        // Degenerate inputs fall back to the box size (no negatives / NaN).
        assert_eq!(fit_size(0.0, 90.0, 400.0, 300.0), (400.0, 300.0));
        // 核心不变量:fit 的结果必须完整放进目标框(宽高都不超过)。
        // Core invariant: the fit result must fit ENTIRELY inside the box.
        let (w, h) = fit_size(1920.0, 1080.0, 184.0, 115.0);
        assert!(w <= 184.0 && h <= 115.0);
    }

    #[test]
    fn fit_target_shrinks_proportionally_and_never_upscales() {
        // 大图按高度等比缩。
        // Large images shrink proportionally by height.
        assert_eq!(fit_target(1920, 1080, 512), (910, 512));
        // 小于上限的原样保留(不放大)。
        // Below-cap images stay untouched (no upscale).
        assert_eq!(fit_target(800, 450, 512), (800, 450));
        // 极端比例下宽度至少 1px。
        // Extreme ratios keep a floor of 1px width.
        assert_eq!(fit_target(10, 5000, 512), (1, 512));
        // 退化输入原样返回。
        // Degenerate inputs pass through.
        assert_eq!(fit_target(0, 0, 512), (0, 0));
    }
}

#[test]
#[ignore]
fn cgshwc_capture_smoke() {
    // 真实截取 Finder 的窗口:验证 dlsym 符号解析、CGS 调用链与降采样。
    // 需要 GUI 会话 + 屏幕录制权限;CI 无权限自动跳过。
    // Really capture a Finder window: verifies dlsym symbol resolution, the CGS
    // call chain, and downscaling. Needs a GUI session + Screen Recording; CI
    // skips automatically without the permission.
    if !capture_allowed() {
        eprintln!("[smoke] Screen Recording not granted; skipping capture smoke");
        return;
    }
    let pid: i32 = unsafe {
        let key = crate::ffi::make_nsstring("com.apple.finder");
        let apps: *mut AnyObject = msg_send![
            class!(NSRunningApplication),
            runningApplicationsWithBundleIdentifier: key
        ];
        CFRelease(key as *const c_void);
        let count: usize = msg_send![apps, count];
        if count == 0 {
            eprintln!("[smoke] Finder not running; skipping");
            return;
        }
        let app: *mut AnyObject = msg_send![apps, objectAtIndex: 0usize];
        msg_send![app, processIdentifier]
    };
    let windows = crate::window_collector::get_ax_windows_for_pid(pid).expect("Finder AX windows");
    let wid = windows
        .iter()
        .find(|(_, _, minimized)| !*minimized)
        .map(|(wid, _, _)| *wid)
        .expect("Finder has no visible window");
    let scale: f64 = unsafe {
        let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
        if screen.is_null() {
            2.0
        } else {
            msg_send![screen, backingScaleFactor]
        }
    };
    // 用最大放大卡片的预览高度驱动真实捕获；当前 2x Retina 环境会走 640px，
    // 同时验证 bestResolution 与动态降采样路径。
    // Drive the real capture with a maximally enlarged card preview; the current 2x Retina
    // environment takes the 640px path, covering bestResolution plus dynamic downscaling.
    let target_px_h = target_px_height(271.25, scale);
    let t = unsafe { capture_window(wid, target_px_h) }
        .expect("CGSHWCCaptureWindowList failed")
        .thumb;
    assert!(t.w_px > 0 && t.h_px > 0, "degenerate capture size");
    assert_eq!(t.captured_for_px_h, target_px_h);
    println!(
        "[smoke] captured Finder window {wid}: {}x{} target={}",
        t.w_px, t.h_px, target_px_h
    );
    // 连带验证渲染侧的 CGImage -> NSImage 转换(msg_send 编码陷阱回归位)。
    // Also verify the render-side CGImage -> NSImage conversion (regression site
    // of the msg_send encoding trap).
    let ns = unsafe {
        crate::overlay::nsimage_from_cgimage(
            t.img,
            objc2_foundation::NSSize::new(t.w_px as f64 / scale, t.h_px as f64 / scale),
        )
    };
    assert!(!ns.is_null(), "nsimage_from_cgimage returned null");
    unsafe { CFRelease(t.img) };
}
