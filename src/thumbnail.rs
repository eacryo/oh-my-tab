//! 窗口缩略图:私有 SkyLight API `SLSHWCaptureWindowList` 截取窗口画面,
//! **纯内存 LRU** 缓存(刻意不落盘——屏幕内容明文落盘有隐私风险,BetterCmdTab/
//! DockDoor 同样只保留内存)。四条生产线:
//! 1. 启动预生成:监视线程启动时枚举所有运行中 App 的标准窗口补拍
//! 2. 常驻监听:每 PID 一个 AXObserver 订阅 kAXWindowCreatedNotification,
//!    新窗口防抖 300ms 后预生成(等窗口完成初始化,避免拍到白屏)
//! 3. 召唤补拍:show_overlay 时对可见区间及两侧预取项中的缺失帧、前台 App
//!    的过期帧入队；后台 App 保留最后一张有效帧，完成后主线程原位换卡
//! 4. 激活刷新:NSWorkspace 确认焦点窗口后延迟补拍，等 Web 内容完成恢复/重绘
//!
//! 5. 空白帧门控:WKWebView(Tauri/Electron 等)的页面由独立 WebContent 进程
//!    渲染,窗口长时间后台后该进程被挂起、内容表面被 WindowServer 丢弃,截出来
//!    只剩"标题栏(红绿灯)+纯色白屏"。此类帧按场景分流(AltTab 同策略):
//!    - 后台 + 缓存有帧:丢弃,保住最后一张有效帧(升级单向,避免回退)
//!    - 后台 + 缓存为空:入缓存作为占位种子(好过图标卡;激活后自动升级)
//!    - 前台:如实入缓存(用户眼前的真实画面)
//!    - 激活补拍仍空白:丢弃并延迟重试一次,给 WebContent 恢复重绘留时间
//!    - 外观(明暗)切换重拍:空白也覆盖——旧外观帧与新主题不协调比占位更刺眼
//!    - 切换器自己切过去的窗口:激活补拍在 backstop 静默出口放行;同应用窗口切换
//!      (无激活通知、808 被静音)在 raise 时铸造 token 直接调度,到达即刷新
//! 6. WindowServer 几何过渡:显示器/Space/窗口动画期间延迟捕获并有界退避重试;
//!    scheduler 为进程级单例,随进程结束
//!
//! 无屏幕录制权限(TCC)时整个模块休眠,浮窗保持纯图标渲染;运行中授权后
//! 下一个捕获任务自动恢复(worker 每个任务前都重新 preflight)。
//!
//!
//! Window thumbnails: capture window imagery via the private SkyLight API
//! `SLSHWCaptureWindowList`, cached in a **memory-only LRU** (deliberately never
//! written to disk -- plaintext screen content in ~/Library/Caches is a privacy
//! risk; BetterCmdTab/DockDoor likewise keep frames in RAM only). Four producers:
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
//! 5. blank-frame gating: WKWebView-based apps (Tauri/Electron et al.) render in a
//!    separate WebContent process; once the window stays in the background that process
//!    is suspended and WindowServer drops the content surface, so a capture degrades to
//!    "title bar (traffic lights) + solid white". Such frames are routed by scenario
//!    (AltTab's strategy):
//!    - background + cached frame: dropped, keeping the last-known-good image (the
//!      upgrade to a real frame is one-way and never regresses)
//!    - background + empty cache: stored as a placeholder seed (beats an icon card;
//!      any frontmost capture upgrades it automatically)
//!    - frontmost: stored as-is (what the user literally sees)
//!    - still blank on an activation refresh: dropped with one delayed retry so the
//!      WebContent process gets time to redraw
//!    - appearance (light/dark) transition recaptures: blank overwrites too -- a
//!      stale-appearance frame amid the new theme looks worse than a placeholder
//!    - windows switched to via our own switcher: the activation refresh is let
//!      through at the backstop's silent exit; same-app window switches (no
//!      activation notification, 808 silenced) mint a token at raise time and
//!      schedule the refresh directly, so arriving refreshes the thumbnail
//! 6. WindowServer geometry transitions: defer captures during display/Space/window
//!    animations and retry with bounded backoff; the scheduler is process-wide and
//!    exits with the process.
//!
//! Without the Screen Recording TCC permission the whole module sleeps and the
//! overlay keeps rendering icons only; granting permission mid-run resumes
//! automatically (the worker re-preflights before every capture).

use objc2::runtime::AnyObject;
use objc2::{class, msg_send, sel};
use std::cmp::{Ordering as CmpOrdering, Reverse};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::c_void;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::ffi::{
    CFArrayGetCount, CFArrayGetValueAtIndex, CFRelease, CFRetain, CFStringCompare,
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGBitmapContextGetData,
    CGColorSpaceCreateDeviceRGB, CGContextDrawImage, CGImageGetHeight, CGImageGetWidth,
    CGPreflightScreenCaptureAccess, CGRect, CGRequestScreenCaptureAccess, RetainedCf,
};
use crate::skylight;
use crate::{log_debug, log_info};

mod pregen;
pub(crate) use pregen::{app_launched, app_terminated, start};

/// 缩略图缓存键:(进程 ID, CG 窗口 ID)。两者组合才能防 PID 复用串图。
/// Thumbnail cache key: (process id, CG window id). The pair guards against
/// recycled PIDs serving another window's frame.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct ThumbKey {
    pub(crate) pid: i32,
    pub(crate) wid: u32,
}

// ========== FFI:dlopen 私有符号 + 公开 CoreGraphics/AX 符号 ==========
// (dlopen/dlsym 与 SkyLight 连接加载统一走 skylight.rs;本模块只留捕获符号的类型。)
// (dlopen/dlsym and the SkyLight connection loading live in skylight.rs; this module
// only keeps the capture symbol's type.)

type CGSConnectionID = u32;
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

static CGS_CAPTURE_LIST: LazyLock<Option<CgsCaptureListFn>> = LazyLock::new(|| unsafe {
    // 注意:公开名 CGSHWCCaptureWindowList 是 CoreGraphics 对 SkyLight 内部符号
    // _SLSHWCaptureWindowList 的再导出,dlsym(SkyLight, "CGSHWCCaptureWindowList")
    // 拿不到(实测);必须用 SkyLight 原生名 SLSHWCaptureWindowList。
    // Note: the public name CGSHWCCaptureWindowList is CoreGraphics's re-export of
    // SkyLight's internal _SLSHWCaptureWindowList; dlsym(SkyLight,
    // "CGSHWCCaptureWindowList") finds nothing (verified) -- SkyLight's native name
    // SLSHWCaptureWindowList must be used.
    skylight::load_private_symbol(skylight::SKYLIGHT_PATH, "SLSHWCaptureWindowList")
});

// 公开框架的 CG/CF extern 与 CGRect 已统一到 ffi.rs;本模块只保留私有捕获符号。
// The public-framework CG/CF externs and CGRect now live in ffi.rs; this module keeps
// only its private capture symbols.

/// kCGImageAlphaPremultipliedLast(RGBA, alpha 在低字节序的末字节)。
/// kCGImageAlphaPremultipliedLast (RGBA with alpha in the last byte on LE).
const BITMAP_PREMULTIPLIED_LAST: u32 = 1;

// ========== 屏幕录制权限(TCC) ==========

static PERMISSION_PROMPTED: AtomicBool = AtomicBool::new(false);
static LAST_CAPTURE_PERMISSION: AtomicU8 = AtomicU8::new(0);

fn report_capture_permission(allowed: bool) {
    let state = if allowed { 2 } else { 1 };
    if LAST_CAPTURE_PERMISSION.swap(state, Ordering::Relaxed) == state {
        return;
    }
    if allowed {
        log_info!("[thumb] Screen Recording permission available; thumbnail capture enabled");
    } else {
        log_info!("[thumb] Screen Recording permission unavailable; using icon-only presentation");
    }
}

/// 是否已授予屏幕录制权限(preflight,廉价可反复调用)。
/// Whether Screen Recording is granted (cheap preflight, safe to call often).
pub(crate) fn capture_allowed() -> bool {
    let allowed = unsafe { CGPreflightScreenCaptureAccess() };
    report_capture_permission(allowed);
    allowed
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

/// 目标像素尺寸:按最大高度等比缩小(不放大);退化输入原样返回。
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
    /// 全局递增的帧版本号(cache_store 时分配)。浮窗卡片签名携带它,帧在浮窗关闭
    /// 期间被替换(种子→真实、激活补拍、外观重拍)后,下一次召唤签名失配走 Replace
    /// 重建,复用路径不会持续展示旧图。
    /// Globally increasing frame version (assigned in cache_store). Overlay card
    /// signatures carry it: after a frame is replaced while the overlay is closed
    /// (seed -> real, activation refresh, appearance recapture), the next summon's
    /// signature mismatch forces a Replace rebuild, so the reuse path can never keep
    /// showing the stale image forever.
    epoch: u64,
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
fn capture_pipeline_stats() -> (usize, usize, usize) {
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
fn summon_refresh_decision(
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

fn cached_summon_refresh_decision(
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

fn cached_target_px_height(pid: i32, wid: u32) -> u32 {
    let cache = CACHE.lock().unwrap();
    cache
        .peek(&ThumbKey { pid, wid })
        .map(|t| t.captured_for_px_h.max(BASE_TARGET_PX_H))
        .unwrap_or(BASE_TARGET_PX_H)
}

fn cache_store(pid: i32, wid: u32, mut t: CachedThumb) {
    // 帧版本号在唯一入库点分配,所有存储路径(预热/召唤/激活/外观)都会推进。
    // The frame version is assigned at the single store point; every path (prewarm /
    // summon / activation / appearance) advances it.
    t.epoch = FRAME_EPOCH_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
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

// ========== 捕获管线(flume 队列 + 单 worker 串行限流) ==========

/// 捕获优先级。值越大越先执行；同优先级保持首次入队 FIFO。启动预热可被
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
    ready_since: Instant,
    running: bool,
    geometry_retry_not_before: Option<Instant>,
    geometry_retry_attempts: u8,
    geometry_retry_started_at: Option<Instant>,
    /// 外观(明暗主题)切换触发的重拍:允许空白帧覆盖已有帧——旧帧是旧外观像素,
    /// 与其他卡片不一致比暂时空白更刺眼。合并请求时按"或"传播。
    /// Appearance (light/dark) transition recapture: blank frames MAY overwrite the
    /// cached frame -- a stale-appearance frame clashes with every other card worse
    /// than a temporary blank. Merging requests propagates the flag with OR.
    appearance_refresh: bool,
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
    ready_since: Instant,
    appearance_refresh: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureJobResult {
    Finished,
    GeometryDeferred,
}

#[derive(Clone, Copy, Debug, Eq)]
struct GeometryRetryDeadline {
    deadline: Instant,
    sequence: u64,
    attempt: u8,
}

impl PartialEq for GeometryRetryDeadline {
    fn eq(&self, other: &Self) -> bool {
        self.deadline == other.deadline && self.sequence == other.sequence
    }
}

// `attempt` is diagnostic metadata only; equality and ordering identify a wake by
// deadline and sequence. `attempt` 仅用于诊断,不参与 deadline 的身份和排序。

impl Ord for GeometryRetryDeadline {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        other
            .deadline
            .cmp(&self.deadline)
            .then_with(|| self.sequence.cmp(&other.sequence))
    }
}

impl PartialOrd for GeometryRetryDeadline {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GeometryDeferResult {
    Deferred(GeometryRetryDeadline),
    Exhausted,
    Stale,
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

    #[cfg(test)]
    fn request(&mut self, key: ThumbKey, target_px_h: u32, priority: CapturePriority) -> bool {
        let pid_generation = self.pid_generations.get(&key.pid).copied().unwrap_or(0);
        self.request_for_generation(key, target_px_h, priority, pid_generation, false)
    }

    fn request_for_generation(
        &mut self,
        key: ThumbKey,
        target_px_h: u32,
        priority: CapturePriority,
        pid_generation: u64,
        appearance_refresh: bool,
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
                let now = Instant::now();
                entry.insert(PendingCapture {
                    target_px_h,
                    priority,
                    sequence,
                    token,
                    pid_generation,
                    activation_at: None,
                    freshness_sequence: 0,
                    enqueued_at: now,
                    ready_since: now,
                    running: false,
                    geometry_retry_not_before: None,
                    geometry_retry_attempts: 0,
                    geometry_retry_started_at: None,
                    appearance_refresh,
                });
                true
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let pending = entry.get_mut();
                pending.target_px_h = pending.target_px_h.max(target_px_h);
                pending.priority = pending.priority.max(priority);
                pending.appearance_refresh |= appearance_refresh;
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
            false,
        );
        if let Some(pending) = self.desired.get_mut(&key) {
            // 同一激活 token 的重复调度(外部激活时 808 路径与 backstop 路径先后
            // 到达)不得推进新鲜度:否则会为正在执行的任务追加一次多余的 follow-up
            // 重拍。真正的新激活(不同 token)照常推进。
            // Duplicate scheduling of the SAME activation token (the 808 path and
            // the backstop path arriving within one external activation) must not
            // advance freshness: it would append a redundant follow-up capture to
            // the running job. A genuinely new activation (different token) still
            // advances it.
            if pending.activation_at != Some(activated_at) {
                let freshness_sequence = Self::next_counter(&mut self.next_freshness_sequence);
                pending.activation_at = Some(activated_at);
                pending.freshness_sequence = freshness_sequence;
            }
        }
        inserted
    }

    #[cfg(test)]
    fn take_next(&mut self) -> Option<CaptureJob> {
        self.take_next_for(false)
    }

    #[cfg(test)]
    fn take_next_for(&mut self, interaction_active: bool) -> Option<CaptureJob> {
        self.take_next_for_at(interaction_active, Instant::now())
    }

    fn geometry_retry_ready(pending: &PendingCapture, now: Instant) -> bool {
        pending
            .geometry_retry_not_before
            .is_none_or(|not_before| not_before <= now)
    }

    fn take_next_for_at(&mut self, interaction_active: bool, now: Instant) -> Option<CaptureJob> {
        let key = self
            .desired
            .iter()
            .filter(|(_, pending)| {
                !pending.running
                    && (!interaction_active || pending.priority >= CapturePriority::Visible)
                    && Self::geometry_retry_ready(pending, now)
            })
            .min_by_key(|(_, pending)| (Reverse(pending.priority), pending.sequence))
            .map(|(key, _)| *key)?;
        let pending = self.desired.get_mut(&key)?;
        pending.running = true;
        pending.geometry_retry_not_before = None;
        Some(CaptureJob {
            key,
            target_px_h: pending.target_px_h,
            priority: pending.priority,
            token: pending.token,
            pid_generation: pending.pid_generation,
            activation_at: pending.activation_at,
            freshness_sequence: pending.freshness_sequence,
            enqueued_at: pending.enqueued_at,
            ready_since: pending.ready_since,
            appearance_refresh: pending.appearance_refresh,
        })
    }

    fn is_current(&self, job: CaptureJob) -> bool {
        self.pid_generations.get(&job.key.pid).copied().unwrap_or(0) == job.pid_generation
            && self
                .desired
                .get(&job.key)
                .is_some_and(|pending| pending.token == job.token)
    }

    fn defer_geometry_transition(&mut self, job: CaptureJob, now: Instant) -> GeometryDeferResult {
        if !self.is_current(job) {
            return GeometryDeferResult::Stale;
        }
        let Some(pending) = self.desired.get_mut(&job.key) else {
            return GeometryDeferResult::Stale;
        };
        if !pending.running {
            return GeometryDeferResult::Stale;
        }
        // 几何过渡期间只恢复同一个有效任务；token/generation 任一失配都不能复活旧任务。
        // Restore only the same live job during a geometry transition; a token or generation
        // mismatch must never resurrect stale or cancelled work.
        let started_at = *pending.geometry_retry_started_at.get_or_insert(now);
        if pending.geometry_retry_attempts >= GEOMETRY_RETRY_MAX_ATTEMPTS
            || now.duration_since(started_at) >= GEOMETRY_RETRY_BUDGET
        {
            self.desired.remove(&job.key);
            return GeometryDeferResult::Exhausted;
        }
        let attempt = pending.geometry_retry_attempts;
        pending.geometry_retry_attempts += 1;
        let deadline = now + geometry_retry_delay(attempt);
        pending.running = false;
        pending.geometry_retry_not_before = Some(deadline);
        pending.ready_since = deadline;
        GeometryDeferResult::Deferred(GeometryRetryDeadline {
            deadline,
            sequence: next_geometry_retry_sequence(),
            attempt: attempt + 1,
        })
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
            // 外观刷新合入正在执行的任务时不改变分辨率/优先级/新鲜度,必须单列,
            // 否则主题重拍会被 finish 静默吞掉,该窗口残留旧主题帧。
            // An appearance refresh merging into a running job changes none of the
            // three fields above, so it needs its own check -- otherwise finish()
            // silently swallows the theme recapture and the window keeps a
            // stale-appearance frame.
            || (pending.appearance_refresh && !job.appearance_refresh)
        {
            pending.running = false;
            pending.ready_since = Instant::now();
            pending.geometry_retry_not_before = None;
            pending.geometry_retry_attempts = 0;
            pending.geometry_retry_started_at = None;
            true
        } else {
            self.desired.remove(&job.key);
            false
        }
    }

    fn discard_deferred(&mut self, job: CaptureJob) -> bool {
        if !self.is_current(job) {
            return false;
        }
        let Some(pending) = self.desired.get(&job.key) else {
            return false;
        };
        if pending.running || pending.geometry_retry_not_before.is_none() {
            return false;
        }
        self.desired.remove(&job.key);
        true
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

    fn cancel_all(&mut self) {
        self.desired.clear();
    }

    fn pid_generation(&self, pid: i32) -> u64 {
        self.pid_generations.get(&pid).copied().unwrap_or(0)
    }
}

static CAPTURE_STATE: LazyLock<Mutex<CaptureState>> =
    LazyLock::new(|| Mutex::new(CaptureState::default()));
static JOB_TX: OnceLock<flume::Sender<()>> = OnceLock::new();
static THUMB_ENQUEUED: AtomicU64 = AtomicU64::new(0);
static THUMB_INTERACTION_DEFERRED: AtomicU64 = AtomicU64::new(0);
static THUMB_GEOMETRY_DEFERRED: AtomicU64 = AtomicU64::new(0);
static THUMB_GEOMETRY_RETRY_EXHAUSTED: AtomicU64 = AtomicU64::new(0);
static THUMB_COMPLETED: AtomicU64 = AtomicU64::new(0);
static THUMB_CAPTURE_FAILED: AtomicU64 = AtomicU64::new(0);
static THUMB_QUEUE_TOTAL_MS: AtomicU64 = AtomicU64::new(0);
static THUMB_QUEUE_MAX_MS: AtomicU64 = AtomicU64::new(0);
static THUMB_QUEUE_SAMPLES: AtomicU64 = AtomicU64::new(0);
static THUMB_CAPTURE_TOTAL_MS: AtomicU64 = AtomicU64::new(0);
static THUMB_CAPTURE_MAX_MS: AtomicU64 = AtomicU64::new(0);
static GEOMETRY_RETRY_SCHEDULER: OnceLock<flume::Sender<GeometryRetryDeadline>> = OnceLock::new();
static GEOMETRY_RETRY_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const GEOMETRY_RETRY_INITIAL_DELAY: Duration = Duration::from_millis(300);
const GEOMETRY_RETRY_MAX_DELAY: Duration = Duration::from_secs(2);
const GEOMETRY_RETRY_MAX_ATTEMPTS: u8 = 6;
const GEOMETRY_RETRY_BUDGET: Duration = Duration::from_secs(10);

#[derive(Default)]
struct GeometryTransitionProbeState {
    space_signature: Vec<u32>,
    unstable_streak: u8,
    stable_streak: u8,
    active: bool,
}

#[derive(Clone, Debug, Default)]
struct GeometryProbeSnapshot {
    active: bool,
    abnormal_windows: HashSet<u32>,
    abnormal_details: HashMap<u32, GeometryAnomaly>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct GeometryAnomaly {
    center_residual: (f64, f64),
    center_shift: bool,
    transform_changed: bool,
    heavily_clipped: bool,
}

/// 将 CGS presentation bounds 的原点归一化到公开 CG window bounds 的坐标空间。
/// CGSGetOnscreenWindowBounds 在当前 macOS 上返回与 kCGWindowBounds 相反的原点，
/// 因此不能直接比较两个矩形的中心；宽高仍直接使用 presentation bounds 的值。
///
/// Normalize the CGS presentation bounds into the public CG window-bounds space.
/// On current macOS, CGSGetOnscreenWindowBounds returns the opposite origin from
/// kCGWindowBounds, so their centers must not be compared directly; presentation
/// width and height are still used as reported.
fn normalized_presentation_center(presentation_bounds: CGRect) -> (f64, f64) {
    (
        -presentation_bounds.x + presentation_bounds.w / 2.0,
        -presentation_bounds.y + presentation_bounds.h / 2.0,
    )
}

/// 返回归一化 presentation center 相对公开 bounds center 的残差。
/// Return the residual between the normalized presentation center and public center.
fn normalized_center_residual(
    public_bounds: (f64, f64, f64, f64),
    presentation_bounds: CGRect,
) -> (f64, f64) {
    let public_center = (
        public_bounds.0 + public_bounds.2 / 2.0,
        public_bounds.1 + public_bounds.3 / 2.0,
    );
    let presentation_center = normalized_presentation_center(presentation_bounds);
    (
        presentation_center.0 - public_center.0,
        presentation_center.1 - public_center.1,
    )
}

fn center_shift_exceeds(bounds: (f64, f64, f64, f64), residual: (f64, f64)) -> bool {
    residual.0.abs() > (bounds.2 * 0.25).max(80.0) || residual.1.abs() > (bounds.3 * 0.25).max(80.0)
}

fn presentation_is_heavily_clipped(
    public_bounds: (f64, f64, f64, f64),
    presentation_bounds: CGRect,
) -> bool {
    let normal_area = public_bounds.2.max(0.0) * public_bounds.3.max(0.0);
    let visible_area = presentation_bounds.w.max(0.0) * presentation_bounds.h.max(0.0);
    normal_area > 0.0 && visible_area / normal_area < 0.25
}

fn geometry_anomaly(
    public_bounds: (f64, f64, f64, f64),
    transform: skylight::CGAffineTransform,
    presentation_bounds: CGRect,
) -> Option<GeometryAnomaly> {
    let center_residual = normalized_center_residual(public_bounds, presentation_bounds);
    let center_shift = center_shift_exceeds(public_bounds, center_residual);
    let transform_changed = (transform.a - 1.0).abs()
        + (transform.d - 1.0).abs()
        + transform.b.abs()
        + transform.c.abs()
        > 0.15;
    let heavily_clipped = presentation_is_heavily_clipped(public_bounds, presentation_bounds);
    (center_shift || transform_changed || heavily_clipped).then_some(GeometryAnomaly {
        center_residual,
        center_shift,
        transform_changed,
        heavily_clipped,
    })
}

fn geometry_capture_should_defer(snapshot: &GeometryProbeSnapshot, window_id: u32) -> bool {
    // 全局过渡期间保护所有窗口;退出 hysteresis 后只保护本轮仍异常的目标窗口。
    // During a global transition guard every window; after hysteresis exits, guard only
    // the target window still abnormal in this sample.
    snapshot.active || snapshot.abnormal_windows.contains(&window_id)
}

/// 基于当前 Space 的窗口变换判断 WindowServer geometry transition 阶段。
/// The detector is intentionally heuristic: private CGS geometry is sampled together
/// with the public current-Space window set. A Space change clears only the old window
/// set's evidence; the current set may still activate on its own sample.
static GEOMETRY_TRANSITION_PROBE: LazyLock<Mutex<GeometryTransitionProbeState>> =
    LazyLock::new(|| Mutex::new(GeometryTransitionProbeState::default()));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GeometryProbeTransition {
    None,
    Activated,
    Deactivated,
}

fn update_geometry_probe_state(
    state: &mut GeometryTransitionProbeState,
    signature: &[u32],
    abnormal_count: usize,
) -> GeometryProbeTransition {
    if state.space_signature != signature {
        // Space 变化会清除旧窗口集合的证据;当前新集合若本次异常数达到阈值仍可独立激活。
        // A Space change clears evidence for the old window set; the current new set may
        // still activate independently if this sample reaches the abnormality threshold.
        state.space_signature = signature.to_vec();
        state.unstable_streak = 0;
        state.stable_streak = 0;
        state.active = false;
    }

    if abnormal_count >= 2 {
        state.unstable_streak = state.unstable_streak.saturating_add(1);
        state.stable_streak = 0;
        if !state.active {
            state.active = true;
            return GeometryProbeTransition::Activated;
        }
    } else if state.active {
        state.stable_streak = state.stable_streak.saturating_add(1);
        if state.stable_streak >= 2 {
            state.active = false;
            state.unstable_streak = 0;
            return GeometryProbeTransition::Deactivated;
        }
    } else {
        state.stable_streak = 0;
    }
    GeometryProbeTransition::None
}

/// 在当前 Space 里取最多三个普通窗口，判断它们是否同时发生了明显的异常变换。
/// Sample up to three ordinary windows in the current Space and detect simultaneous
/// abnormal transforms. Two consecutive samples below the abnormal threshold are required
/// to leave the state; a Space change clears only the old set's evidence.
fn window_server_geometry_transition_snapshot() -> GeometryProbeSnapshot {
    let windows = crate::window_collector::ordinary_onscreen_window_bounds();
    let mut signature: Vec<u32> = windows.iter().map(|(wid, _)| *wid).collect();
    signature.sort_unstable();
    let Some(connection) = skylight::cgs_main_connection() else {
        return GeometryProbeSnapshot::default();
    };

    let mut abnormal_windows = HashSet::new();
    let mut abnormal_details = HashMap::new();
    for (window_id, bounds) in &windows {
        let Some((transform, onscreen_bounds)) =
            skylight::cgs_window_presentation_geometry(connection, *window_id)
        else {
            return GeometryProbeSnapshot::default();
        };
        if let Some(anomaly) = geometry_anomaly(*bounds, transform, onscreen_bounds) {
            abnormal_windows.insert(*window_id);
            abnormal_details.insert(*window_id, anomaly);
        }
    }

    let abnormal_count = abnormal_windows.len();
    let mut state = GEOMETRY_TRANSITION_PROBE.lock().unwrap();
    let transition = update_geometry_probe_state(&mut state, &signature, abnormal_count);
    match transition {
        GeometryProbeTransition::Activated => {
            let center_count = abnormal_details
                .values()
                .filter(|detail| detail.center_shift)
                .count();
            let transform_count = abnormal_details
                .values()
                .filter(|detail| detail.transform_changed)
                .count();
            let clipped_count = abnormal_details
                .values()
                .filter(|detail| detail.heavily_clipped)
                .count();
            log_debug!(
                "[geometry-transition] WindowServer geometry transition active abnormal_windows={} center={} transform={} clipped={} sample_streak={}",
                abnormal_count,
                center_count,
                transform_count,
                clipped_count,
                state.unstable_streak
            )
        }
        GeometryProbeTransition::Deactivated => {
            log_debug!("[geometry-transition] WindowServer geometry transition inactive after below-threshold samples")
        }
        GeometryProbeTransition::None => {}
    }
    GeometryProbeSnapshot {
        active: state.active,
        abnormal_windows,
        abnormal_details,
    }
}

/// 拒绝 WindowServer 在动画中返回的细长/裁剪源帧，避免污染已有缓存。
/// Reject thin or clipped source frames returned during WindowServer animations.
fn capture_geometry_is_plausible(key: ThumbKey, captured: &CapturedWindow) -> bool {
    if captured.source_w_px < 64 || captured.source_h_px < 64 {
        return false;
    }
    let source_aspect = captured.source_w_px as f64 / captured.source_h_px as f64;
    if !(0.08..=12.0).contains(&source_aspect) {
        return false;
    }
    let expected_bounds = crate::window_collector::ordinary_onscreen_window_bounds()
        .into_iter()
        .find_map(|(wid, bounds)| (wid == key.wid).then_some(bounds));
    let Some((_, _, expected_w, expected_h)) = expected_bounds else {
        return true;
    };
    if expected_w <= 0.0 || expected_h <= 0.0 {
        return true;
    }
    let expected_aspect = expected_w / expected_h;
    let aspect_ratio = source_aspect / expected_aspect;
    (0.25..=4.0).contains(&aspect_ratio)
}

fn update_max(metric: &AtomicU64, value: u64) {
    let mut current = metric.load(Ordering::Relaxed);
    while value > current {
        match metric.compare_exchange_weak(current, value, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(next) => current = next,
        }
    }
}

fn record_thumb_queue_wait(queue_ms: u64) {
    THUMB_QUEUE_SAMPLES.fetch_add(1, Ordering::Relaxed);
    THUMB_QUEUE_TOTAL_MS.fetch_add(queue_ms, Ordering::Relaxed);
    update_max(&THUMB_QUEUE_MAX_MS, queue_ms);
}

fn record_thumb_capture(capture_ms: u64) {
    THUMB_COMPLETED.fetch_add(1, Ordering::Relaxed);
    THUMB_CAPTURE_TOTAL_MS.fetch_add(capture_ms, Ordering::Relaxed);
    update_max(&THUMB_CAPTURE_MAX_MS, capture_ms);
}

fn record_thumb_capture_failed() {
    THUMB_CAPTURE_FAILED.fetch_add(1, Ordering::Relaxed);
}

fn geometry_retry_delay(attempt: u8) -> Duration {
    match attempt {
        0 => GEOMETRY_RETRY_INITIAL_DELAY,
        1 => Duration::from_millis(600),
        2 => Duration::from_millis(1_200),
        _ => GEOMETRY_RETRY_MAX_DELAY,
    }
}

fn next_geometry_retry_sequence() -> u64 {
    let mut current = GEOMETRY_RETRY_SEQUENCE.load(Ordering::Relaxed);
    loop {
        let next = current.wrapping_add(1).max(1);
        match GEOMETRY_RETRY_SEQUENCE.compare_exchange_weak(
            current,
            next,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return next,
            Err(observed) => current = observed,
        }
    }
}

fn run_geometry_retry_scheduler(
    rx: flume::Receiver<GeometryRetryDeadline>,
    job_tx: flume::Sender<()>,
) {
    let mut deadlines = std::collections::BinaryHeap::new();
    loop {
        let Some(next) = deadlines.peek().copied() else {
            match rx.recv() {
                Ok(deadline) => deadlines.push(deadline),
                Err(_) => return,
            }
            continue;
        };

        let wait = next.deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(wait) {
            Ok(deadline) => deadlines.push(deadline),
            Err(flume::RecvTimeoutError::Timeout) => {
                let now = Instant::now();
                let mut due = false;
                while deadlines
                    .peek()
                    .is_some_and(|deadline| deadline.deadline <= now)
                {
                    deadlines.pop();
                    due = true;
                }
                if due {
                    // 调度器只负责唤醒；是否仍然有效由 CaptureState 的 token、generation
                    // 和 retry_not_before 决定，过期 deadline 的额外 wake 是安全的。
                    // The scheduler only wakes the worker; CaptureState remains authoritative
                    // for token, generation, and retry_not_before, so stale deadlines are safe.
                    // `Full` means a wake is already queued. The worker drains CaptureState
                    // and rereads Instant::now() every round, so coalescing this wake is safe.
                    // `Full` 表示已有 wake;worker 每轮 drain 都重读 Instant,合并唤醒是安全的。
                    match job_tx.try_send(()) {
                        Ok(()) | Err(flume::TrySendError::Full(_)) => {}
                        Err(flume::TrySendError::Disconnected(_)) => return,
                    }
                }
            }
            Err(flume::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn schedule_geometry_retry(deadline: GeometryRetryDeadline, job_tx: flume::Sender<()>) -> bool {
    if let Some(scheduler) = GEOMETRY_RETRY_SCHEDULER.get() {
        return scheduler.send(deadline).is_ok();
    }
    let (scheduler_tx, rx) = flume::unbounded();
    if std::thread::Builder::new()
        .name("thumb-geometry-scheduler".into())
        .spawn(move || run_geometry_retry_scheduler(rx, job_tx))
        .is_err()
    {
        log_debug!(
            "[geometry-transition] failed to start retry scheduler; deferred job will be dropped"
        );
        return false;
    }
    let _ = GEOMETRY_RETRY_SCHEDULER.set(scheduler_tx);
    let Some(scheduler) = GEOMETRY_RETRY_SCHEDULER.get() else {
        log_debug!("[geometry-transition] retry scheduler became unavailable; deferred job will be dropped");
        return false;
    };
    if scheduler.send(deadline).is_err() {
        log_debug!(
            "[geometry-transition] retry scheduler rejected deadline; deferred job will be dropped"
        );
        false
    } else {
        true
    }
}

pub(crate) fn log_capture_metrics(context: &str) {
    let enqueued = THUMB_ENQUEUED.load(Ordering::Relaxed);
    let completed = THUMB_COMPLETED.load(Ordering::Relaxed);
    let queue_total = THUMB_QUEUE_TOTAL_MS.load(Ordering::Relaxed);
    let capture_total = THUMB_CAPTURE_TOTAL_MS.load(Ordering::Relaxed);
    let queue_samples = THUMB_QUEUE_SAMPLES.load(Ordering::Relaxed);
    let avg_queue_ms = queue_total.checked_div(queue_samples.max(1)).unwrap_or(0);
    let avg_capture_ms = capture_total.checked_div(completed.max(1)).unwrap_or(0);
    log_debug!(
        "[perf] thumbnail metrics context={} enqueued={} completed={} failed={} queue_samples={} interaction_deferred={} geometry_deferred={} geometry_retry_exhausted={} avg_queue_ms={} max_queue_ms={} avg_capture_ms={} max_capture_ms={}",
        context,
        enqueued,
        completed,
        THUMB_CAPTURE_FAILED.load(Ordering::Relaxed),
        queue_samples,
        THUMB_INTERACTION_DEFERRED.load(Ordering::Relaxed),
        THUMB_GEOMETRY_DEFERRED.load(Ordering::Relaxed),
        THUMB_GEOMETRY_RETRY_EXHAUSTED.load(Ordering::Relaxed),
        avg_queue_ms,
        THUMB_QUEUE_MAX_MS.load(Ordering::Relaxed),
        avg_capture_ms,
        THUMB_CAPTURE_MAX_MS.load(Ordering::Relaxed)
    );
}

/// Wake the capture worker after the interaction gate is lifted so deferred background jobs can
/// resume without waiting for another thumbnail request.
pub(crate) fn wake_capture_worker() {
    if let Some(tx) = JOB_TX.get() {
        // 单个 wake 足以让 worker 持续从 CaptureState 取任务；不按 pending 数量复制
        // 唤醒令牌，避免交互结束时一次性灌入大量过期 wake。
        // One wake is enough: the worker drains CaptureState itself. Do not enqueue one token
        // per pending job, which would replay a burst of stale wakes after interaction ends.
        let _ = tx.try_send(());
    }
}

/// 尝试安排一次捕获；返回 false 表示相同窗口已 pending/in-flight，或 worker 已退出。
/// Try to schedule one capture; false means the same window is already pending/in-flight,
/// or the worker has exited.
fn enqueue_job(pid: i32, wid: u32, target_px_h: u32, priority: CapturePriority) -> bool {
    enqueue_job_inner(pid, wid, target_px_h, priority, None, false)
}

/// 外观(明暗主题)切换的重拍:空白帧允许覆盖已有帧(旧外观像素比暂时空白更刺眼)。
/// Appearance (light/dark) transition recapture: blank frames may overwrite the
/// cached frame (stale-appearance pixels clash harder than a temporary blank).
fn enqueue_appearance_job(pid: i32, wid: u32, target_px_h: u32, priority: CapturePriority) -> bool {
    enqueue_job_inner(pid, wid, target_px_h, priority, None, true)
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
    enqueue_job_inner(pid, wid, target_px_h, priority, Some(pid_generation), false)
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
    THUMB_ENQUEUED.fetch_add(1, Ordering::Relaxed);
    if matches!(tx.try_send(()), Err(flume::TrySendError::Disconnected(_))) {
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
    appearance_refresh: bool,
) -> bool {
    let key = ThumbKey { pid, wid };
    let tx = ensure_capture_worker();
    let accepted = {
        let mut state = CAPTURE_STATE.lock().unwrap();
        // 无预期 generation 时按当前值解析(等价于原 request();terminated 判定仍在
        // request_for_generation 内生效)。
        // Without an expected generation, resolve the current one (equivalent to the
        // old request(); the terminated check still applies inside
        // request_for_generation).
        let generation = expected_generation.unwrap_or_else(|| state.pid_generation(key.pid));
        state.request_for_generation(key, target_px_h, priority, generation, appearance_refresh)
    };
    if !accepted {
        return false;
    }
    THUMB_ENQUEUED.fetch_add(1, Ordering::Relaxed);
    if matches!(tx.try_send(()), Err(flume::TrySendError::Disconnected(_))) {
        CAPTURE_STATE.lock().unwrap().desired.remove(&key);
        return false;
    }
    true
}

fn ensure_capture_worker() -> &'static flume::Sender<()> {
    JOB_TX.get_or_init(|| {
        let (tx, rx) = flume::bounded::<()>(1);
        let worker_tx = tx.clone();
        std::thread::Builder::new()
            .name("thumb-capture".into())
            .spawn(move || {
                crate::performance::set_current_thread_qos(crate::performance::ThreadQos::Utility);
                log_debug!("[thumb] capture worker online");
                for () in rx.iter() {
                    let interaction_active = crate::performance::switcher_interaction_active();
                    let drain_started = Instant::now();
                    let mut drained_jobs = 0usize;
                    // 一个 wake 令牌只负责启动一次 drain;bounded channel 会合并后续
                    // wake,因此必须在同一轮持续消费 CaptureState,否则启动预热只会处理
                    // 前一两项,其余任务虽仍在 desired 中却再也收不到令牌。
                    // One wake token starts a drain. Because the bounded channel coalesces
                    // later wakes, keep consuming CaptureState in this round; otherwise
                    // startup prewarm processes only the first couple of jobs while the rest
                    // remain in `desired` with no token left to wake the worker.
                    loop {
                        let now = Instant::now();
                        let Some(job) = CAPTURE_STATE
                            .lock()
                            .unwrap()
                            .take_next_for_at(interaction_active, now)
                        else {
                            if interaction_active {
                                let pending_background = CAPTURE_STATE
                                    .lock()
                                    .unwrap()
                                    .desired
                                    .values()
                                    .any(|pending| {
                                        !pending.running
                                            && pending.priority < CapturePriority::Visible
                                            && CaptureState::geometry_retry_ready(pending, now)
                                    });
                                if pending_background {
                                    let deferred =
                                        THUMB_INTERACTION_DEFERRED.fetch_add(1, Ordering::Relaxed)
                                            + 1;
                                    if deferred == 1 || deferred.is_multiple_of(16) {
                                        log_debug!(
                                            "[perf] thumbnail background work deferred during interaction count={}",
                                            deferred
                                        );
                                    }
                                }
                            }
                            break;
                        };
                        // Measure only the time spent waiting until this attempt became ready;
                        // capture execution time is recorded separately by run_capture_job.
                        // 只统计任务 ready 后到 worker 取出的等待时间,捕获耗时单独统计。
                        record_thumb_queue_wait(
                            Instant::now()
                                .saturating_duration_since(job.ready_since)
                                .as_millis() as u64,
                        );
                        match run_capture_job(job) {
                            CaptureJobResult::Finished => {
                                let mut state = CAPTURE_STATE.lock().unwrap();
                                let _ = state.finish(job);
                            }
                            CaptureJobResult::GeometryDeferred => {
                                let result = CAPTURE_STATE.lock().unwrap().defer_geometry_transition(
                                    job,
                                    Instant::now(),
                                );
                                match result {
                                    GeometryDeferResult::Deferred(deadline) => {
                                        let deferred = THUMB_GEOMETRY_DEFERRED
                                            .fetch_add(1, Ordering::Relaxed)
                                            + 1;
                                        if deferred == 1 || deferred.is_multiple_of(16) {
                                            log_debug!(
                                                "[geometry-transition] thumbnail capture deferred during WindowServer geometry transition count={} retry_attempt={}",
                                                deferred,
                                                deadline.attempt
                                            );
                                        }
                                        if !schedule_geometry_retry(deadline, worker_tx.clone()) {
                                            let removed = CAPTURE_STATE
                                                .lock()
                                                .unwrap()
                                                .discard_deferred(job);
                                            log_debug!(
                                                "[geometry-transition] retry scheduling failed pid={} wid={} deferred_job_removed={}",
                                                job.key.pid,
                                                job.key.wid,
                                                removed
                                            );
                                        }
                                    }
                                    GeometryDeferResult::Exhausted => {
                                        let exhausted = THUMB_GEOMETRY_RETRY_EXHAUSTED
                                            .fetch_add(1, Ordering::Relaxed)
                                            + 1;
                                        log_debug!(
                                            "[geometry-transition] thumbnail capture retry budget exhausted pid={} wid={} elapsed_ms={} count={}",
                                            job.key.pid,
                                            job.key.wid,
                                            job.enqueued_at.elapsed().as_millis(),
                                            exhausted
                                        );
                                    }
                                    GeometryDeferResult::Stale => {
                                        log_debug!(
                                            "[geometry-transition] stale deferred thumbnail job dropped pid={} wid={}",
                                            job.key.pid,
                                            job.key.wid
                                        );
                                    }
                                }
                            }
                        }
                        drained_jobs += 1;
                    }
                    if drained_jobs > 0 {
                        let (pending, in_flight, ready) = capture_pipeline_stats();
                        let (cache_items, cache_bytes) = cache_stats();
                        log_debug!(
                            "[perf] thumbnail drain complete jobs={} elapsed_ms={} pending={} in_flight={} ready={} cache_items={} cache_bytes={}",
                            drained_jobs,
                            drain_started.elapsed().as_millis(),
                            pending,
                            in_flight,
                            ready,
                            cache_items,
                            cache_bytes,
                        );
                        log_capture_metrics("drain");
                        crate::mem::log_debug_snapshot("thumb-drain-complete");
                    }
                }
            })
            .expect("spawn thumb-capture worker");
        tx
    })
}

fn run_capture_job(job: CaptureJob) -> CaptureJobResult {
    let key = job.key;
    if !CAPTURE_STATE.lock().unwrap().is_current(job) {
        log_debug!(
            "[thumb] job skipped stale pid={} wid={} priority={}",
            key.pid,
            key.wid,
            job.priority.label()
        );
        return CaptureJobResult::Finished;
    }
    // 在真正捕获前再次探测，覆盖任务取出后到调用 WindowServer 之间的动画竞态。
    // Probe again immediately before capture to cover the race between job selection
    // and the WindowServer call when the animation starts.
    let geometry = window_server_geometry_transition_snapshot();
    if geometry_capture_should_defer(&geometry, key.wid) {
        let anomaly = geometry
            .abnormal_details
            .get(&key.wid)
            .copied()
            .unwrap_or_default();
        log_debug!(
            "[geometry-transition] capture deferred during WindowServer geometry transition pid={} wid={} priority={} global_active={} target_abnormal={} center_shift={} center_residual=({:.1},{:.1}) transform_changed={} clipped={}",
            key.pid,
            key.wid,
            job.priority.label(),
            geometry.active,
            geometry.abnormal_windows.contains(&key.wid),
            anomaly.center_shift,
            anomaly.center_residual.0,
            anomaly.center_residual.1,
            anomaly.transform_changed,
            anomaly.heavily_clipped
        );
        return CaptureJobResult::GeometryDeferred;
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
        return CaptureJobResult::Finished;
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
        return CaptureJobResult::Finished;
    }
    let job_started = Instant::now();
    let Some(captured) = (unsafe { capture_window(key.wid, job.target_px_h) }) else {
        record_thumb_capture_failed();
        log_debug!("[thumb] capture failed pid={} wid={}", key.pid, key.wid);
        return CaptureJobResult::Finished;
    };
    log_debug!(
        "[thumb] capture result pid={} wid={} source={}x{} cached={}x{} target_h={}",
        key.pid,
        key.wid,
        captured.source_w_px,
        captured.source_h_px,
        captured.thumb.w_px,
        captured.thumb.h_px,
        job.target_px_h
    );
    if !capture_geometry_is_plausible(key, &captured) {
        unsafe {
            CFRelease(captured.thumb.img);
        }
        log_debug!(
            "[thumb] captured result discarded by geometry guard pid={} wid={} source={}x{} priority={}",
            key.pid,
            key.wid,
            captured.source_w_px,
            captured.source_h_px,
            job.priority.label()
        );
        return CaptureJobResult::Finished;
    }
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
        return CaptureJobResult::Finished;
    }
    record_thumb_capture(job_started.elapsed().as_millis() as u64);
    // 空白帧门控:后台挂起的 WKWebView(Tauri/Electron 等)截出来只剩"标题栏+
    // 纯色内容",这样的帧不应覆盖缓存里的最后一张有效帧;前台窗口的空白是
    // 用户眼前的真实画面,如实保留。
    // Blank-frame gating: a background-suspended WKWebView (Tauri/Electron et al.)
    // captures as title bar + solid content only; such a frame must never clobber
    // the cached last-known-good image. A blank frontmost window is real and is
    // stored as-is.
    if unsafe { frame_blankness(captured.thumb.img, captured.thumb.w_px, captured.thumb.h_px) }
        .unwrap_or(false)
    {
        let frontmost = pid_is_frontmost(key.pid);
        let cache_has_frame = CACHE.lock().unwrap().peek(&key).is_some();
        let retry_slot_acquired = job.activation_at.is_some()
            && frontmost
            && cache_has_frame
            && PENDING_BLANK_RETRIES.lock().unwrap().insert(key);
        match blank_frame_action(
            frontmost,
            job.activation_at.is_some(),
            cache_has_frame,
            retry_slot_acquired,
            job.appearance_refresh,
        ) {
            BlankFrameAction::Store => {}
            BlankFrameAction::StoreSeed => {
                log_debug!(
                    "[thumb] blank first frame stored as placeholder seed pid={} wid={}",
                    key.pid,
                    key.wid
                );
            }
            BlankFrameAction::StoreAppearanceRefresh => {
                log_debug!(
                    "[thumb] blank frame stored for appearance refresh pid={} wid={}",
                    key.pid,
                    key.wid
                );
            }
            BlankFrameAction::DiscardKeepLastGood => {
                unsafe {
                    CFRelease(captured.thumb.img);
                }
                log_debug!(
                    "[thumb] blank frame discarded, keeping last-known-good pid={} wid={} priority={}",
                    key.pid,
                    key.wid,
                    job.priority.label()
                );
                return CaptureJobResult::Finished;
            }
            BlankFrameAction::DiscardRetryActivation => {
                unsafe {
                    CFRelease(captured.thumb.img);
                }
                schedule_blank_activation_retry(job);
                log_debug!(
                    "[thumb] blank activation frame discarded, retry scheduled pid={} wid={}",
                    key.pid,
                    key.wid
                );
                return CaptureJobResult::Finished;
            }
        }
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
        return CaptureJobResult::Finished;
    }
    // 帧最终入库:激活补拍链的重试名额随之释放(空白帧"如实入库"路径同样到此为止)。
    // The frame is finally stored: the activation chain's retry slot is released with
    // it (the store-blank-as-truth path also ends here).
    if job.activation_at.is_some() {
        PENDING_BLANK_RETRIES.lock().unwrap().remove(&key);
    }
    cache_store(key.pid, key.wid, captured.thumb);
    drop(state);
    // 不再按任务来源预先决定是否投递：启动预热也可能在浮窗打开后才完成。
    // Do not decide delivery from the request source: startup pre-generation may
    // also finish after the overlay has opened.
    // 结果统一交给主线程做可见性和卡片存在性校验。捕获 worker 不再读取 TAB_STATE，
    // 避免后台线程直接观察主线程运行时状态；隐藏浮窗时主线程会快速丢弃这批通知。
    // Let the main thread validate visibility and card membership. The capture worker no longer
    // reads TAB_STATE, and the main-thread handler quickly drops notifications while hidden.
    enqueue_ready_delivery(key);
    CaptureJobResult::Finished
}

/// 激活补拍的有效性:激活 token 未过时,且该 App 此刻仍是系统前台。
/// Activation refresh validity: the activation token is current AND the app is
/// still the system-frontmost one right now.
fn activation_capture_is_valid_now(pid: i32, activated_at: Instant) -> bool {
    crate::window_collector::app_activation_is_current(pid, activated_at) && pid_is_frontmost(pid)
}

/// 查询 NSWorkspace 当前前台 App 是否就是指定 PID。捕获 worker 与延迟补拍线程
/// 都会调用;NSWorkspace 的这类只读消息发送线程安全,不依赖 AppKit 主线程。
/// Whether NSWorkspace currently reports the given PID as the frontmost app.
/// Called from the capture worker and delayed refresh threads; these read-only
/// NSWorkspace messages are thread-safe and do not require the AppKit main thread.
fn pid_is_frontmost(pid: i32) -> bool {
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
    let ready_batch_size = keys.len();
    let visible = crate::overlay::thumbnail_visible_range();
    let keys: HashSet<ThumbKey> = keys.into_iter().collect();
    let Some(ready_keys) = crate::with_tab_state(|state_opt| {
        let state = state_opt.as_ref()?;
        if !state.visible {
            return None;
        }
        Some(
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
                .map(|(_, window)| (window.pid, window.window_id))
                .collect::<Vec<_>>(),
        )
    }) else {
        return;
    };
    if !ready_keys.is_empty() {
        log_debug!(
            "[perf] thumbnail ready batch={} matched_visible={}",
            ready_batch_size,
            ready_keys.len()
        );
        crate::overlay::refresh_thumbnail_previews(&ready_keys);
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
    source_w_px: u32,
    source_h_px: u32,
    thumb: CachedThumb,
}

unsafe fn capture_window(wid: u32, target_px_h: u32) -> Option<CapturedWindow> {
    let cap = *CGS_CAPTURE_LIST.as_ref()?;
    // 连接 ID 进程内恒定,缓存一次;0 = 获取失败(私有符号缺失)。
    // The connection ID is process-wide constant; cache it once (0 = unavailable).
    let cid = *CONNECTION_ID.get_or_init(|| skylight::cgs_main_connection().unwrap_or(0));
    if cid == 0 {
        return None;
    }
    let wids = [wid];
    // 显式请求 Retina 原生像素；nominalResolution 只给逻辑点尺寸，小窗口在 4K/5K
    // 屏上放大后仍会发糊，即使后续目标高度提高也无法补回源细节。
    // Explicitly request native Retina pixels. nominalResolution only returns point-sized
    // content, so small windows stay blurry on 4K/5K displays even with a larger target later.
    let opts = CGS_CAPTURE_BEST_RESOLUTION | CGS_CAPTURE_IGNORE_GLOBAL_CLIP_SHAPE;
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
    let src_w = CGImageGetWidth(raw) as u32;
    let src_h = CGImageGetHeight(raw) as u32;
    let target_px_h = target_px_h.clamp(BASE_TARGET_PX_H, MAX_TARGET_PX_H);
    let (tw, th) = fit_target(src_w, src_h, target_px_h);
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
        source_w_px: src_w,
        source_h_px: src_h,
        thumb: CachedThumb {
            img,
            w_px: tw,
            h_px: th,
            captured_for_px_h: target_px_h,
            captured: Instant::now(),
            // 占位值;实际版本号由 cache_store 统一分配。
            // Placeholder; the real version is assigned centrally in cache_store.
            epoch: 0,
        },
    })
}

/// CGBitmapContext 重绘降采样(纯 CoreGraphics,线程安全;方向与原图一致)。
/// Downscale by redrawing through a CGBitmapContext (pure CoreGraphics,
/// thread-safe; orientation matches the source).
unsafe fn downscale_cgimage(src: *const c_void, tw: u32, th: u32) -> *const c_void {
    if tw == 0 || th == 0 {
        return std::ptr::null();
    }
    let cs = DEVICE_RGB_COLOR_SPACE.ptr;
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

// ========== 空白帧检测(后台挂起的 WKWebView 窗口) ==========
// WKWebView(Tauri/Electron/wry 等)的页面由独立 WebContent 进程渲染;窗口长时间
// 后台/被遮挡后 macOS 挂起该进程并丢弃 WindowServer 侧的内容表面,此时截窗口
// 只剩宿主进程绘制的标题栏(红绿灯),内容区域退化为逐像素一致的纯色(通常白)。
// macOS 没有任何公开 API 能强制别的进程重渲染,AltTab 的结论是唯一可行策略:
// 前台时捕获 + 保留最后一张有效帧,避免被空白帧覆盖(alt-tab-macos WindowThumbnails.swift)。
// 本模块在缓存写入前对每帧做空白判定:
// - 后台窗口 + 已有缓存帧:丢弃,保住最后一张有效帧(升级单向)
// - 后台窗口 + 缓存为空:入缓存作为占位种子(之后前台补拍自动升级)
// - 前台窗口:如实入缓存(用户眼前就是空白画面)
// - 激活补拍仍空白:丢弃并延迟重拍一次(Web 内容尚未恢复重绘完)
// - 外观切换重拍:空白也覆盖(旧外观帧与新主题不协调比占位更刺眼)
//
// Blank-frame detection for background-suspended WKWebView windows: the page of a
// WKWebView-based app (Tauri/Electron/wry) renders in a separate WebContent
// process; once the window stays backgrounded/occluded, macOS suspends it and drops
// the WindowServer-side content surface, so captures degrade to the host-drawn
// title bar (traffic lights) over a pixel-uniform solid body (usually white). No
// public API can force another process to redraw; AltTab's proven answer is the
// one adopted here: capture while frontmost and never let a blank frame overwrite
// the last-known-good thumbnail (alt-tab-macos WindowThumbnails.swift). Before any
// cache write each frame is classified:
// - background + cached frame: dropped, keeping the last-known-good image (one-way)
// - background + empty cache: stored as a placeholder seed (frontmost captures
//   upgrade it automatically afterwards)
// - frontmost: stored as-is (the user is literally looking at it)
// - still blank on an activation refresh: dropped with one delayed retry
// - appearance-transition recapture: blank overwrites too (a stale-appearance
//   frame amid the new theme looks worse than a placeholder)

/// Device RGB 色彩空间不可变且线程安全,进程级复用;降采样与空白判定共用。
/// The Device RGB color space is immutable and thread-safe; one process-wide
/// instance is shared by downscaling and blank analysis.
static DEVICE_RGB_COLOR_SPACE: LazyLock<RetainedCf<c_void>> =
    LazyLock::new(|| unsafe { RetainedCf::from_retained(CGColorSpaceCreateDeviceRGB()) });

/// 空白判定的采样最长边:把帧重绘到 ≤64px 的 RGBA 小位图再统计,单帧开销微秒级。
/// Sampling longest edge for blank analysis: the frame is redrawn into a small
/// (<=64px) RGBA bitmap first; per-frame cost stays in the microsecond range.
const BLANK_SAMPLE_MAX_DIM: u32 = 64;
/// 内容区从标题栏/工具条之下开始统计:挂起 WebView 的标题栏由宿主进程绘制,仍是
/// 正常画面,必须排除在"内容近纯色"判定外。28pt 标题栏在 400~1200pt 高的窗口中
/// 占 2.3%~7%,取 12% 覆盖标题栏加常见工具条。
/// Content rows start below the title bar / toolbar strip: a suspended WebView's
/// title bar is host-drawn and still renders normally, so it must be excluded from
/// the near-uniform test. A 28pt title bar spans 2.3%~7% of 400~1200pt-tall windows;
/// 12% covers the bar plus a common toolbar.
const BLANK_TITLE_STRIP_FRACTION: f64 = 0.12;
/// 单一颜色桶覆盖率 ≥99% 判为空白:挂起 WebView 的内容区逐像素一致,覆盖率≈1.0;
/// 真实 UI(侧栏/文本/控件/边框)远达不到 99%。桶按通道 5bit 量化,轻微压缩
/// 噪声不会造成假阴性。
/// A single quantized color bucket covering >=99% of content rows classifies as
/// blank: suspended WebView bodies are pixel-uniform (coverage ~1.0) while real UIs
/// (sidebars/text/controls/borders) never approach 99%. Buckets quantize channels
/// to 5 bits so mild compression noise cannot fake a negative.
const BLANK_MODAL_COVERAGE_MIN: f64 = 0.99;
/// 激活补拍遇到空白帧后的重试延迟:给 WebContent 进程恢复并完成一轮重绘的时间
/// (首拍 350ms 仍白说明恢复偏慢,重试给到约 1.25s 总窗口)。
/// Retry delay after a blank activation refresh: gives the WebContent process time
/// to restore and finish a redraw pass (a blank frame at the initial 350ms means
/// restoration is slow; the retry lands at a ~1.25s total window).
const ACTIVATION_BLANK_RETRY_MS: u64 = 900;

/// 已安排延迟重试的窗口键。名额在帧最终入库、重试因失焦/换代放弃或 App 退出时
/// 释放;同一激活补拍链至多重试一次,防止空白-重拍死循环。
/// Window keys with a delayed retry scheduled. A slot is released when a frame is
/// finally stored, the retry is abandoned (focus lost / generation changed), or the
/// app terminates; each activation chain retries at most once so blank-recapture
/// cannot loop forever.
static PENDING_BLANK_RETRIES: LazyLock<Mutex<HashSet<ThumbKey>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// 内容区(跳过顶部标题条带后)单一颜色桶的覆盖率;None = 没有可统计像素。
/// 纯函数:输入 RGBA 字节流,便于单元测试。
/// Coverage of the single most common color bucket over the content rows (below the
/// title strip); None when there is nothing to measure. Pure function over an RGBA
/// byte buffer so it is unit-testable without CoreGraphics.
fn blank_modal_coverage(rgba: &[u8], w: usize, skip_rows: usize, h: usize) -> Option<f64> {
    if w == 0 || h <= skip_rows || rgba.len() < w * h * 4 {
        return None;
    }
    let mut counts: HashMap<u16, usize> = HashMap::new();
    let mut total = 0usize;
    for y in skip_rows..h {
        let row = y * w * 4;
        for x in 0..w {
            let i = row + x * 4;
            let bucket = (((rgba[i] as u16) >> 3) << 10)
                | (((rgba[i + 1] as u16) >> 3) << 5)
                | ((rgba[i + 2] as u16) >> 3);
            *counts.entry(bucket).or_default() += 1;
            total += 1;
        }
    }
    let modal = counts.values().copied().max()?;
    Some(modal as f64 / total as f64)
}

/// 空白帧的处理决策(纯函数,便于矩阵化测试)。
/// What to do with a blank frame (pure function for matrix testing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlankFrameAction {
    /// 前台窗口的空白是用户眼前的真实画面,如实入缓存。
    /// A blank FRONTMOST window is what the user literally sees; store it.
    Store,
    /// 后台窗口的首帧(缓存为空):入缓存作为占位种子。挂起 WebView 只能截到
    /// "标题栏+纯色内容",但一张占位帧好过图标卡——之后任何一次前台补拍
    /// (激活/前台召唤)都会把它升级成真实画面,而门控保证升级不可逆。
    /// The FIRST frame of a background window (empty cache): store as a placeholder
    /// seed. A suspended WebView only yields title bar + solid body, yet a
    /// placeholder beats an icon card -- any later frontmost capture (activation /
    /// frontmost summon) upgrades it to the real page, and the gating makes that
    /// upgrade one-way.
    StoreSeed,
    /// 外观(明暗主题)切换的重拍:空白帧也覆盖。旧帧是旧外观像素,与其他卡片
    /// 不一致比暂时空白更刺眼;真实画面在下次前台时自然补回。
    /// Appearance (light/dark) transition recapture: blank overwrites too. The old
    /// frame carries stale-appearance pixels that clash with every other card worse
    /// than a temporary blank; the real page returns on the next frontmost capture.
    StoreAppearanceRefresh,
    /// 后台窗口的空白帧 = 挂起 WebView;丢弃,保住缓存里的最后一张有效帧。
    /// A blank BACKGROUND frame means a suspended WebView; drop it and keep the
    /// cached last-known-good image.
    DiscardKeepLastGood,
    /// 前台激活补拍仍空白 = Web 内容尚未重绘完成;丢弃并安排一次延迟重拍。
    /// A blank frame on an activation refresh means Web content has not repainted
    /// yet; drop it and schedule one delayed recapture.
    DiscardRetryActivation,
}

fn blank_frame_action(
    frontmost: bool,
    activation_job: bool,
    cache_has_frame: bool,
    retry_slot_acquired: bool,
    appearance_refresh: bool,
) -> BlankFrameAction {
    // 激活补拍的空白重试优先于外观覆盖:重试拍到的真实帧同样满足外观一致性,
    // 而直接入库会跳过 1.4s 兜底重试,把"还没画完"的画面定格成占位帧(主题任务
    // 与激活请求合并时两个标志会同帧出现,必须在此处分出先后)。
    // The activation blank retry outranks the appearance overwrite: the retried
    // real frame satisfies appearance consistency too, while storing now would
    // skip the 1.4s backstop retry and freeze an unfinished repaint as the
    // placeholder (a theme job merged with an activation request carries both
    // flags on one frame, so the order must be settled here).
    if frontmost && activation_job && cache_has_frame && retry_slot_acquired {
        return BlankFrameAction::DiscardRetryActivation;
    }
    if appearance_refresh {
        // 外观一致性优先于内容保真:即使前台空白会走 Store,后台空白也会覆盖
        // 有效帧,统一由本分支放行。
        // Appearance consistency wins over content fidelity: blank frames store for
        // both the frontmost and background cases through this single branch.
        return BlankFrameAction::StoreAppearanceRefresh;
    }
    if !frontmost {
        return if cache_has_frame {
            BlankFrameAction::DiscardKeepLastGood
        } else {
            BlankFrameAction::StoreSeed
        };
    }
    BlankFrameAction::Store
}

/// 把帧重绘进 ≤64px 的 RGBA 位图后做空白判定;分析不可用时返回 None,调用方按
/// 非空白处理(保守起见维持原入缓存行为)。
/// Redraw the frame into a small (<=64px) RGBA bitmap and run the blank test.
/// Analysis failures return None and the caller treats the frame as non-blank
/// (conservatively preserving the original store behavior).
unsafe fn frame_blankness(img: *const c_void, w_px: u32, h_px: u32) -> Option<bool> {
    if img.is_null() || w_px == 0 || h_px == 0 {
        return None;
    }
    let scale = BLANK_SAMPLE_MAX_DIM as f64 / u32::max(w_px, h_px) as f64;
    let sw = (((w_px as f64) * scale).round() as usize).max(1);
    let sh = (((h_px as f64) * scale).round() as usize).max(1);
    let ctx = CGBitmapContextCreate(
        std::ptr::null_mut(),
        sw,
        sh,
        8,
        sw * 4,
        DEVICE_RGB_COLOR_SPACE.ptr,
        BITMAP_PREMULTIPLIED_LAST,
    );
    if ctx.is_null() {
        return None;
    }
    CGContextDrawImage(
        ctx,
        CGRect {
            x: 0.0,
            y: 0.0,
            w: sw as f64,
            h: sh as f64,
        },
        img,
    );
    let data = CGBitmapContextGetData(ctx) as *const u8;
    let coverage = if data.is_null() {
        None
    } else {
        let skip_rows = ((sh as f64) * BLANK_TITLE_STRIP_FRACTION).round() as usize;
        blank_modal_coverage(
            std::slice::from_raw_parts(data, sw * sh * 4),
            sw,
            skip_rows,
            sh,
        )
    };
    CFRelease(ctx);
    Some(coverage.is_some_and(|c| c >= BLANK_MODAL_COVERAGE_MIN))
}

/// App 退出时一并丢弃其挂起的空白重试名额(pregen 的终止路径调用)。
/// Drop a terminated app's pending blank-retry slots (called from pregen's
/// termination path).
pub(crate) fn forget_blank_retries_for_pid(pid: i32) {
    PENDING_BLANK_RETRIES
        .lock()
        .unwrap()
        .retain(|key| key.pid != pid);
}

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
    let interaction_active = crate::performance::switcher_interaction_active();
    let Some((jobs, missing, frontmost_stale, background_last_good, deferred_prefetch)) =
        crate::with_tab_state(|state_opt| {
            let state = state_opt.as_ref()?;
            if !state.visible {
                return None;
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
                            // 焦点窗口(is_active)无视 TTL 每次召唤重截;同 PID 兄弟窗口
                            // 仍按 TTL 判定,避免多窗口 App 召唤时成串重截。
                            // The focused window (is_active) ignores the TTL and is
                            // recaptured on every summon; same-PID siblings keep the
                            // TTL rules so multi-window apps do not recapture in bulk.
                            w.is_active,
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
                .filter(|(_, _, _, decision)| {
                    *decision == SummonRefreshDecision::BackgroundLastGood
                })
                .count();
            let mut deferred_prefetch = 0usize;
            let jobs: Vec<(i32, u32, CapturePriority)> = decisions
                .into_iter()
                .filter(|(_, _, _, decision)| {
                    matches!(
                        decision,
                        SummonRefreshDecision::Missing | SummonRefreshDecision::FrontmostStale
                    )
                })
                .filter_map(|(index, pid, wid, _)| {
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
                    if interaction_active && priority < CapturePriority::Visible {
                        deferred_prefetch += 1;
                        None
                    } else {
                        Some((pid, wid, priority))
                    }
                })
                .collect();
            Some((
                jobs,
                missing,
                frontmost_stale,
                background_last_good,
                deferred_prefetch,
            ))
        })
    else {
        return;
    };
    let requested = jobs.len();
    let (pending_before, in_flight_before, ready_before) = capture_pipeline_stats();
    let (cache_items_before, cache_bytes_before) = cache_stats();
    log_debug!(
        "[perf] thumbnail summon start requested={} target_h={} cache_items={} cache_bytes={} pending={} in_flight={} ready={}",
        requested,
        required_px_h,
        cache_items_before,
        cache_bytes_before,
        pending_before,
        in_flight_before,
        ready_before,
    );
    crate::mem::log_debug_snapshot("thumb-summon-before-enqueue");
    let mut enqueued = 0;
    for (pid, wid, priority) in jobs {
        enqueued += usize::from(enqueue_job(pid, wid, required_px_h, priority));
    }
    log_debug!(
        "[perf] thumbnail summon: active={} missing={} frontmost_stale={} background_last_good={} deferred_prefetch={} requested={} enqueued={} target_h={}",
        interaction_active,
        missing,
        frontmost_stale,
        background_last_good,
        deferred_prefetch,
        requested,
        enqueued,
        required_px_h
    );
    log_capture_metrics("summon");
    crate::mem::log_debug_snapshot("thumb-summon-after-enqueue");
}

/// 主题切换后强制重拍当前窗口集合,不使用召唤期的 TTL/前台判断。
/// Force a recapture of the current window set after a theme change, bypassing
/// summon-time TTL and frontmost/background freshness decisions.
///
/// The cache stores real window pixels, so changing the system appearance can
/// leave a dark/light surface stale even though the card itself is rebuilt.
/// These jobs carry the appearance-refresh permit: a blank recapture of a
/// suspended WebView still replaces the old frame -- one stale-appearance card
/// amid the new theme looks worse than a temporary placeholder.
/// 缓存的是真实窗口像素,系统外观切换后即使卡片树重建,缓存帧可能仍是旧明暗。
/// 这批任务携带外观刷新许可:挂起 WebView 的空白重截也覆盖旧帧——新主题下
/// 独独一张旧外观卡片比暂时占位更刺眼。
pub(crate) fn refresh_for_theme(required_px_h: u32) {
    if !crate::theme::thumbnails_enabled() {
        return;
    }
    if !capture_allowed() {
        request_permission_once();
        return;
    }

    // Snapshot keys before enqueueing: enqueue_job takes CAPTURE_STATE and may
    // wake the worker, so never hold TAB_STATE across the queue operations.
    // 入队前先快照 key,避免持有 TAB_STATE 时进入捕获队列锁并唤醒 worker。
    let (selected, state_keys): (Option<ThumbKey>, Vec<ThumbKey>) =
        crate::with_tab_state(|state_opt| match state_opt.as_ref() {
            Some(state) => {
                let selected = state.windows.get(state.selected).map(|window| ThumbKey {
                    pid: window.pid,
                    wid: window.window_id,
                });
                let keys = state
                    .windows
                    .iter()
                    .filter(|window| {
                        !window.minimized && window.bounds.2 > 0.0 && window.bounds.3 > 0.0
                    })
                    .map(|window| ThumbKey {
                        pid: window.pid,
                        wid: window.window_id,
                    })
                    .collect();
                (selected, keys)
            }
            None => (None, Vec::new()),
        });

    // Include pre-generated/cache-only windows as well. The settings window can change theme
    // before the first switcher summon, when TAB_STATE has no current snapshot yet.
    // 同时纳入启动预热或仅存在于缓存中的窗口:用户可能在首次召唤切换器前就切换主题,
    // 此时 TAB_STATE 还没有窗口快照。
    let keys: Vec<ThumbKey> = {
        let mut keys: HashSet<ThumbKey> = state_keys.into_iter().collect();
        keys.extend(CACHE.lock().unwrap().keys());
        keys.into_iter().collect()
    };

    let target_px_h = required_px_h.max(BASE_TARGET_PX_H);
    let requested = keys.len();
    let (pending_before, in_flight_before, ready_before) = capture_pipeline_stats();
    let (cache_items_before, cache_bytes_before) = cache_stats();
    log_debug!(
        "[perf] thumbnail theme start requested={} target_h={} cache_items={} cache_bytes={} pending={} in_flight={} ready={}",
        requested,
        target_px_h,
        cache_items_before,
        cache_bytes_before,
        pending_before,
        in_flight_before,
        ready_before,
    );
    crate::mem::log_debug_snapshot("thumb-theme-before-enqueue");
    let mut enqueued = 0usize;
    for key in keys {
        let priority = if selected == Some(key) {
            CapturePriority::Selected
        } else {
            // Visible priority deliberately bypasses the interaction gate so
            // every card is refreshed as part of one theme transition.
            CapturePriority::Visible
        };
        // 外观任务携带空白覆盖许可:挂起 WebView 重截回的白板(新外观标题栏)也
        // 要替换旧外观的有效帧,避免一张浅色帧混在深色卡片中间。
        // Appearance jobs carry the blank-overwrite permit: even a suspended
        // WebView's blank recapture (new-appearance title bar) must replace the
        // stale-appearance frame, or one light frame lingers among dark cards.
        enqueued += usize::from(enqueue_appearance_job(
            key.pid,
            key.wid,
            target_px_h,
            priority,
        ));
    }
    log_debug!(
        "[thumb] theme refresh: requested={} enqueued={} target_h={}",
        requested,
        enqueued,
        target_px_h
    );
    log_capture_metrics("theme");
    crate::mem::log_debug_snapshot("thumb-theme-after-enqueue");
}

/// 显示器配置变化(外接/内建切换、分辨率调整)后的强制重拍。
/// 缓存帧携带旧屏幕配置下的窗口宽高比与像素高度:分辨率变化会改写窗口 bounds,
/// 显示器切换会改变 backing scale,旧帧塞进按新比例布局的卡片会被错误留白。
/// 与主题重拍不同,这批任务走普通通道:挂起 WebView 的空白帧不得覆盖最后一张
/// 有效帧(旧比例的真实画面好过新比例的白板),几何守卫也会丢弃动画中的畸变帧。
/// Forced recapture after a display reconfiguration (external/built-in switch or
/// resolution change). Cached frames carry the old configuration's window aspect and
/// pixel height: a resolution change rewrites window bounds and a display switch
/// changes the backing scale, so an old frame letterboxes wrongly inside a card laid
/// out for the new aspect. Unlike the theme refresh these jobs use the normal
/// channel: a suspended WebView's blank frame must NOT overwrite the last-known-good
/// image (a real frame with the old aspect beats a correctly-shaped blank), and the
/// geometry guard still drops frames captured mid-animation.
pub(crate) fn refresh_for_display_change(required_px_h: u32) {
    if !crate::theme::thumbnails_enabled() {
        return;
    }
    if !capture_allowed() {
        request_permission_once();
        return;
    }

    // 与 refresh_for_theme 相同的键收集:TAB_STATE 里未最小化且有 bounds 的窗口,
    // 并上仅存在于缓存中的窗口(预生成帧),覆盖浮窗未召唤时的全部已知目标。
    // Same key collection as refresh_for_theme: non-minimized windows with bounds
    // from TAB_STATE, unioned with cache-only windows (pre-generated frames), so
    // every known target is covered while the overlay is not summoned.
    let (selected, state_keys): (Option<ThumbKey>, Vec<ThumbKey>) =
        crate::with_tab_state(|state_opt| match state_opt.as_ref() {
            Some(state) => {
                let selected = state.windows.get(state.selected).map(|window| ThumbKey {
                    pid: window.pid,
                    wid: window.window_id,
                });
                let keys = state
                    .windows
                    .iter()
                    .filter(|window| {
                        !window.minimized && window.bounds.2 > 0.0 && window.bounds.3 > 0.0
                    })
                    .map(|window| ThumbKey {
                        pid: window.pid,
                        wid: window.window_id,
                    })
                    .collect();
                (selected, keys)
            }
            None => (None, Vec::new()),
        });
    let keys: Vec<ThumbKey> = {
        let mut keys: HashSet<ThumbKey> = state_keys.into_iter().collect();
        keys.extend(CACHE.lock().unwrap().keys());
        keys.into_iter().collect()
    };

    let target_px_h = required_px_h.max(BASE_TARGET_PX_H);
    let requested = keys.len();
    let (pending_before, in_flight_before, ready_before) = capture_pipeline_stats();
    let (cache_items_before, cache_bytes_before) = cache_stats();
    log_debug!(
        "[perf] thumbnail display-change start requested={} target_h={} cache_items={} cache_bytes={} pending={} in_flight={} ready={}",
        requested,
        target_px_h,
        cache_items_before,
        cache_bytes_before,
        pending_before,
        in_flight_before,
        ready_before,
    );
    crate::mem::log_debug_snapshot("thumb-display-change-before-enqueue");
    let mut enqueued = 0usize;
    for key in keys {
        // Selected/Visible 优先级均不受切换交互门控约束,任务不会被推迟丢弃。
        // Both Selected and Visible priorities bypass the interaction gate, so
        // these jobs are never deferred away.
        let priority = if selected == Some(key) {
            CapturePriority::Selected
        } else {
            CapturePriority::Visible
        };
        enqueued += usize::from(enqueue_job(key.pid, key.wid, target_px_h, priority));
    }
    log_debug!(
        "[thumb] display-change refresh: requested={} enqueued={} target_h={}",
        requested,
        enqueued,
        target_px_h
    );
    log_capture_metrics("display-change");
    crate::mem::log_debug_snapshot("thumb-display-change-after-enqueue");
}

/// 激活补拍门控的纯逻辑(供单元测试;运行时走 activation_capture_is_valid_now)。
/// Pure gating logic for activation refreshes (unit tests; runtime goes through
/// activation_capture_is_valid_now).
#[cfg(test)]
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
        .spawn(move || {
            std::thread::sleep(Duration::from_millis(ACTIVATION_CAPTURE_DELAY_MS));
            if activation_capture_is_valid_now(pid, activated_at) {
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
        });
}

/// 同应用窗口切换的到达补拍:目标应用已是前台时,raise 它的某个窗口既不会带来
/// 新的 App 激活通知(应用本就活跃),808 又被 own-focus 静音,激活补拍链完全不会
/// 启动。这里在 raise 时铸造新 token 并直接调度一次激活补拍(仍受 350ms 后的前台
/// 校验与空白重试约束);跨应用切换不满足前台前提,函数内部短路,仍走激活通知驱动
/// 的补拍链。
/// Arrival refresh for SAME-APP window switches: with the target app already
/// frontmost, raising one of its windows brings no new app-activation notification
/// (the app is already active) and its 808 is silenced as an own-focus echo, so the
/// activation-refresh chain never starts. Mint a fresh token at raise time and
/// schedule the activation refresh directly (still gated by the frontmost checks at
/// +350ms and by the blank retry). Cross-app switches fail the frontmost
/// precondition inside and keep using the notification-driven chain.
pub(crate) fn refresh_after_same_app_switch(pid: i32, wid: u32) {
    if !crate::theme::thumbnails_enabled() {
        return;
    }
    if !pid_is_frontmost(pid) {
        return;
    }
    let activated_at = crate::window_collector::note_app_activated(pid);
    refresh_after_activation(pid, wid, activated_at);
}

/// 激活补拍仍得到空白帧:WebContent 进程尚未完成恢复重绘。延迟 ACTIVATION_BLANK_RETRY_MS
/// 后再走一次激活补拍(仍要求前台且激活 token 未过时)。每条激活链至多重试一次,
/// 名额在帧入库、重试放弃或 App 退出时释放。
/// The activation refresh still produced a blank frame: the WebContent process has
/// not finished restoring/redrawing. After ACTIVATION_BLANK_RETRY_MS one more
/// activation refresh runs (still gated on frontmost + current activation token).
/// Each activation chain retries at most once; the slot is released when a frame is
/// stored, the retry is abandoned, or the app terminates.
fn schedule_blank_activation_retry(job: CaptureJob) {
    let key = job.key;
    let Some(activated_at) = job.activation_at else {
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("oh-my-tab-thumb-blank-retry".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_millis(ACTIVATION_BLANK_RETRY_MS));
            if !activation_capture_is_valid_now(key.pid, activated_at) {
                PENDING_BLANK_RETRIES.lock().unwrap().remove(&key);
                log_debug!(
                    "[thumb] blank retry skipped (no longer frontmost) pid={} wid={}",
                    key.pid,
                    key.wid
                );
                return;
            }
            let target_px_h = cached_target_px_height(key.pid, key.wid);
            let pid_generation = CAPTURE_STATE.lock().unwrap().pid_generation(key.pid);
            let enqueued =
                enqueue_activation_job(key.pid, key.wid, target_px_h, activated_at, pid_generation);
            // 合并进已有任务时名额交给该任务完成时释放;独立入队则由入库路径释放。
            // Merged into an already-pending job, the slot is released when that job
            // finishes; an independent queue entry is released by the store path.
            log_debug!(
                "[thumb] blank retry: pid={} wid={} enqueued={} target_h={}",
                key.pid,
                key.wid,
                enqueued,
                target_px_h
            );
        });
    if spawned.is_err() {
        PENDING_BLANK_RETRIES.lock().unwrap().remove(&key);
    }
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
    fn capture_state_defers_background_jobs_during_interaction() {
        let mut state = CaptureState::default();
        let startup = ThumbKey { pid: 10, wid: 20 };
        let visible = ThumbKey { pid: 10, wid: 21 };

        assert!(state.request(startup, 512, CapturePriority::Startup));
        assert!(state.request(visible, 512, CapturePriority::Visible));

        let job = state.take_next_for(true).unwrap();
        assert_eq!(job.key, visible);
        assert!(state.take_next_for(true).is_none());
        assert_eq!(state.take_next_for(false).unwrap().key, startup);
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
    fn capture_state_defers_geometry_transition_and_retrieves_same_job() {
        let mut state = CaptureState::default();
        let key = ThumbKey { pid: 10, wid: 20 };
        let now = Instant::now();

        assert!(state.request(key, 640, CapturePriority::Visible));
        let job = state.take_next_for_at(false, now).unwrap();
        let deadline = match state.defer_geometry_transition(job, now) {
            GeometryDeferResult::Deferred(deadline) => deadline,
            result => panic!("unexpected defer result: {result:?}"),
        };
        assert!(state.is_current(job));
        assert_eq!(
            state.desired.get(&key).unwrap().ready_since,
            deadline.deadline
        );

        // 未到截止时间的几何任务不可阻塞其他就绪任务；到期后保留原 token/generation。
        // A not-yet-due geometry retry must not block other ready work; once due it keeps
        // the original token and PID generation.
        let other = ThumbKey { pid: 10, wid: 21 };
        assert!(state.request(other, 640, CapturePriority::Visible));
        let ready = state.take_next_for_at(false, now).unwrap();
        assert_eq!(ready.key, other);
        assert!(!state.finish(ready));
        assert!(state.take_next_for_at(false, now).is_none());
        let retry = state.take_next_for_at(false, deadline.deadline).unwrap();
        assert_eq!(retry.key, job.key);
        assert_eq!(retry.token, job.token);
        assert_eq!(retry.pid_generation, job.pid_generation);
        assert_eq!(retry.enqueued_at, job.enqueued_at);
        assert_eq!(retry.ready_since, deadline.deadline);
        assert!(!state.finish(retry));
    }

    #[test]
    fn capture_state_does_not_revive_stale_or_cancelled_geometry_job() {
        let key = ThumbKey { pid: 10, wid: 20 };

        let mut stale_state = CaptureState::default();
        assert!(stale_state.request(key, 512, CapturePriority::Visible));
        let stale_job = stale_state.take_next().unwrap();
        stale_state.cancel_all();
        assert_eq!(
            stale_state.defer_geometry_transition(stale_job, Instant::now()),
            GeometryDeferResult::Stale
        );
        assert!(stale_state.take_next().is_none());

        let mut cancelled_state = CaptureState::default();
        assert!(cancelled_state.request(key, 512, CapturePriority::Visible));
        let cancelled_job = cancelled_state.take_next().unwrap();
        cancelled_state.cancel_pid(key.pid);
        assert_eq!(
            cancelled_state.defer_geometry_transition(cancelled_job, Instant::now()),
            GeometryDeferResult::Stale
        );
        assert!(cancelled_state.take_next().is_none());
    }

    #[test]
    fn normalized_presentation_center_matches_public_bounds_for_offset_displays() {
        let identity = skylight::CGAffineTransform {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            tx: 0.0,
            ty: 0.0,
        };
        let cases = [
            ((265.0, 144.0, 940.0, 701.0), (-265.0, -144.0)),
            // 左侧副显示器: public x 为负, presentation x 为正。
            ((-1600.0, 144.0, 940.0, 701.0), (1600.0, -144.0)),
            // 纵向显示器同时覆盖负 y 和正 y 的情况。
            ((-900.0, -1200.0, 940.0, 701.0), (900.0, 1200.0)),
            ((265.0, 1200.0, 940.0, 701.0), (-265.0, -1200.0)),
        ];

        for (public_bounds, (presentation_x, presentation_y)) in cases {
            let presentation_bounds = CGRect {
                x: presentation_x,
                y: presentation_y,
                w: public_bounds.2,
                h: public_bounds.3,
            };
            let residual = normalized_center_residual(public_bounds, presentation_bounds);
            assert!(residual.0.abs() < 0.001, "x residual={:?}", residual);
            assert!(residual.1.abs() < 0.001, "y residual={:?}", residual);
            assert!(!center_shift_exceeds(public_bounds, residual));
            assert!(geometry_anomaly(public_bounds, identity, presentation_bounds).is_none());
        }
    }

    #[test]
    fn normalized_center_residual_detects_real_motion_and_handles_size_change() {
        let public_bounds = (265.0, 144.0, 940.0, 701.0);
        let shifted = CGRect {
            x: -515.0,
            y: -144.0,
            w: 940.0,
            h: 701.0,
        };
        let residual = normalized_center_residual(public_bounds, shifted);
        assert_eq!(residual, (250.0, 0.0));
        assert!(center_shift_exceeds(public_bounds, residual));
        let identity = skylight::CGAffineTransform {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            tx: 0.0,
            ty: 0.0,
        };
        let anomaly = geometry_anomaly(public_bounds, identity, shifted).unwrap();
        assert!(anomaly.center_shift);
        assert_eq!(anomaly.center_residual, residual);

        // 只改变 presentation 尺寸时，中心变化按半个尺寸差计算；小变化不应误报。
        // A small presentation-size change moves the center by half the size delta and
        // should remain below the existing relative/absolute threshold.
        let resized = CGRect {
            x: -265.0,
            y: -144.0,
            w: 1_100.0,
            h: 701.0,
        };
        let resize_residual = normalized_center_residual(public_bounds, resized);
        assert_eq!(resize_residual, (80.0, 0.0));
        assert!(!center_shift_exceeds(public_bounds, resize_residual));
    }

    #[test]
    fn geometry_anomaly_reasons_keep_clipping_guard_independent() {
        let public_bounds = (0.0, 33.0, 960.0, 640.0);
        let clipped = CGRect {
            x: -280.0,
            y: -253.0,
            w: 400.0,
            h: 200.0,
        };
        let residual = normalized_center_residual(public_bounds, clipped);
        assert!(!center_shift_exceeds(public_bounds, residual));
        assert!(presentation_is_heavily_clipped(public_bounds, clipped));
        let identity = skylight::CGAffineTransform {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            tx: 0.0,
            ty: 0.0,
        };
        let anomaly = geometry_anomaly(public_bounds, identity, clipped).unwrap();
        assert!(!anomaly.center_shift);
        assert!(anomaly.heavily_clipped);
    }

    #[test]
    fn target_specific_snapshot_ignores_stable_offset_and_defers_real_target() {
        let public_bounds = (265.0, 144.0, 940.0, 701.0);
        let stable_presentation = CGRect {
            x: -265.0,
            y: -144.0,
            w: 940.0,
            h: 701.0,
        };
        let identity = skylight::CGAffineTransform {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            tx: 0.0,
            ty: 0.0,
        };
        assert!(geometry_anomaly(public_bounds, identity, stable_presentation).is_none());
        let stable_snapshot = GeometryProbeSnapshot::default();
        assert!(!geometry_capture_should_defer(&stable_snapshot, 194));

        let moving_presentation = CGRect {
            x: -515.0,
            y: stable_presentation.y,
            w: stable_presentation.w,
            h: stable_presentation.h,
        };
        let anomaly = geometry_anomaly(public_bounds, identity, moving_presentation).unwrap();
        let moving_snapshot = GeometryProbeSnapshot {
            abnormal_windows: HashSet::from([194]),
            abnormal_details: HashMap::from([(194, anomaly)]),
            ..GeometryProbeSnapshot::default()
        };
        assert!(geometry_capture_should_defer(&moving_snapshot, 194));
        assert!(!geometry_capture_should_defer(&moving_snapshot, 10));
    }

    #[test]
    fn geometry_probe_leaves_after_two_consecutive_normal_samples() {
        let mut state = GeometryTransitionProbeState::default();
        let signature = [10, 20];

        assert_eq!(
            update_geometry_probe_state(&mut state, &signature, 2),
            GeometryProbeTransition::Activated
        );
        assert!(state.active);
        assert_eq!(
            update_geometry_probe_state(&mut state, &signature, 1),
            GeometryProbeTransition::None
        );
        assert!(state.active);
        assert_eq!(
            update_geometry_probe_state(&mut state, &signature, 1),
            GeometryProbeTransition::Deactivated
        );
        assert!(!state.active);
    }

    #[test]
    fn geometry_probe_keeps_lone_abnormal_target_guarded_after_global_exit() {
        let mut state = GeometryTransitionProbeState::default();
        let signature = [10, 20];
        assert_eq!(
            update_geometry_probe_state(&mut state, &signature, 2),
            GeometryProbeTransition::Activated
        );
        assert_eq!(
            update_geometry_probe_state(&mut state, &signature, 1),
            GeometryProbeTransition::None
        );
        assert_eq!(
            update_geometry_probe_state(&mut state, &signature, 1),
            GeometryProbeTransition::Deactivated
        );
        let snapshot = GeometryProbeSnapshot {
            active: state.active,
            abnormal_windows: HashSet::from([20]),
            abnormal_details: HashMap::from([(
                20,
                GeometryAnomaly {
                    center_residual: (250.0, 0.0),
                    center_shift: true,
                    ..GeometryAnomaly::default()
                },
            )]),
        };
        assert!(geometry_capture_should_defer(&snapshot, 20));
        assert!(!geometry_capture_should_defer(&snapshot, 10));
    }

    #[test]
    fn geometry_retry_budget_is_bounded_and_fresh_request_can_reenter() {
        let mut state = CaptureState::default();
        let key = ThumbKey { pid: 10, wid: 20 };
        let mut now = Instant::now();
        assert!(state.request(key, 512, CapturePriority::Visible));

        for attempt in 0..GEOMETRY_RETRY_MAX_ATTEMPTS {
            let job = state.take_next_for_at(false, now).unwrap();
            let deferred = state.defer_geometry_transition(job, now);
            let deadline = match deferred {
                GeometryDeferResult::Deferred(deadline) => deadline,
                result => panic!("attempt {attempt} unexpectedly returned {result:?}"),
            };
            assert_eq!(
                deadline.deadline.duration_since(now),
                geometry_retry_delay(attempt)
            );
            assert!(state.take_next_for_at(false, now).is_none());
            now = deadline.deadline;
        }

        let exhausted_job = state.take_next_for_at(false, now).unwrap();
        assert_eq!(
            state.defer_geometry_transition(exhausted_job, now),
            GeometryDeferResult::Exhausted
        );
        assert!(state.take_next_for_at(false, now).is_none());
        assert!(state.request(key, 512, CapturePriority::Selected));
        assert!(state.take_next_for_at(false, now).is_some());
    }

    #[test]
    fn geometry_retry_heap_keeps_later_deadlines_after_earlier_wake() {
        let now = Instant::now();
        let later = GeometryRetryDeadline {
            deadline: now + Duration::from_secs(2),
            sequence: 2,
            attempt: 2,
        };
        let earlier = GeometryRetryDeadline {
            deadline: now + Duration::from_millis(300),
            sequence: 1,
            attempt: 1,
        };
        let mut heap = std::collections::BinaryHeap::new();
        heap.push(later);
        heap.push(earlier);
        assert_eq!(heap.pop().unwrap().sequence, 1);
        assert_eq!(heap.pop().unwrap().sequence, 2);
    }

    #[test]
    fn geometry_retry_scheduler_wakes_earlier_and_later_deadlines_and_exits_on_disconnect() {
        let (deadline_tx, deadline_rx) = flume::unbounded();
        let (job_tx, job_rx) = flume::bounded(1);
        let scheduler =
            std::thread::spawn(move || run_geometry_retry_scheduler(deadline_rx, job_tx));
        let now = Instant::now();
        deadline_tx
            .send(GeometryRetryDeadline {
                deadline: now + Duration::from_millis(350),
                sequence: 2,
                attempt: 2,
            })
            .unwrap();
        deadline_tx
            .send(GeometryRetryDeadline {
                deadline: now + Duration::from_millis(100),
                sequence: 1,
                attempt: 1,
            })
            .unwrap();

        assert!(job_rx.recv_timeout(Duration::from_secs(2)).is_ok());
        assert!(job_rx.recv_timeout(Duration::from_secs(2)).is_ok());
        drop(deadline_tx);
        scheduler.join().unwrap();
    }

    #[test]
    fn geometry_retry_deadline_eq_matches_ordering_equality() {
        let now = Instant::now();
        let same = GeometryRetryDeadline {
            deadline: now,
            sequence: 7,
            attempt: 1,
        };
        let same_ordering = GeometryRetryDeadline {
            deadline: now,
            sequence: 7,
            attempt: 4,
        };
        let different_deadline = GeometryRetryDeadline {
            deadline: now + Duration::from_millis(1),
            sequence: 7,
            attempt: 1,
        };
        assert_eq!(same, same_ordering);
        assert_eq!(same.cmp(&same_ordering), CmpOrdering::Equal);
        assert_ne!(same, different_deadline);
        assert_ne!(same.cmp(&different_deadline), CmpOrdering::Equal);
    }

    #[test]
    fn finishing_geometry_retry_follow_up_resets_retry_budget() {
        let mut state = CaptureState::default();
        let key = ThumbKey { pid: 10, wid: 20 };
        let now = Instant::now();
        assert!(state.request(key, 512, CapturePriority::Visible));
        let first = state.take_next_for_at(false, now).unwrap();
        let deadline = match state.defer_geometry_transition(first, now) {
            GeometryDeferResult::Deferred(deadline) => deadline.deadline,
            result => panic!("unexpected defer result: {result:?}"),
        };
        let retry = state.take_next_for_at(false, deadline).unwrap();
        assert!(!state.request(key, 640, CapturePriority::Selected));
        let before_finish = Instant::now();
        assert!(state.finish(retry));

        let pending = state.desired.get(&key).unwrap();
        assert_eq!(pending.geometry_retry_not_before, None);
        assert_eq!(pending.geometry_retry_attempts, 0);
        assert_eq!(pending.geometry_retry_started_at, None);
        assert!(pending.ready_since >= before_finish);
        let follow_up = state.take_next_for_at(false, deadline).unwrap();
        assert_eq!(follow_up.target_px_h, 640);
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
            false,
        ));
        assert!(state.request(running_key, 640, CapturePriority::Selected));
        assert!(!state.finish(running));
        let replacement = state.take_next().unwrap();
        assert_eq!(replacement.target_px_h, 640);
        assert!(state.is_current(replacement));
    }

    #[test]
    fn cancel_all_invalidates_queued_and_running_jobs() {
        let mut state = CaptureState::default();
        let key = ThumbKey { pid: 10, wid: 20 };
        assert!(state.request(key, 512, CapturePriority::Visible));
        let job = state.take_next().unwrap();
        assert!(state.is_current(job));

        state.cancel_all();
        assert!(!state.is_current(job));
        assert!(state.take_next().is_none());
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
    fn lru_remove_where_can_clear_all_and_reset_cost() {
        let mut lru: Lru<u32, u64> = Lru::new(10, u64::MAX, |v| *v);
        lru.put(1, 10);
        lru.put(2, 20);

        let removed = lru.remove_where(|_| true);
        assert_eq!(removed, vec![10, 20]);
        assert_eq!(lru.len(), 0);
        assert_eq!(lru.total_cost(), 0);
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
            summon_refresh_decision(None, 640, now, false, false),
            SummonRefreshDecision::Missing
        );
        assert_eq!(
            summon_refresh_decision(Some((stale, 512)), 640, now, false, false),
            SummonRefreshDecision::BackgroundLastGood
        );
        assert_eq!(
            summon_refresh_decision(Some((stale, 512)), 640, now, true, false),
            SummonRefreshDecision::FrontmostStale
        );
        assert_eq!(
            summon_refresh_decision(Some((fresh, 640)), 640, now, false, false),
            SummonRefreshDecision::Fresh
        );
    }

    #[test]
    fn summon_refresh_recaptures_the_focused_window_regardless_of_ttl() {
        let now = Instant::now();
        let stale = now - Duration::from_millis(FRESH_TTL_MS as u64);
        let fresh = now - Duration::from_millis(100);

        // 焦点窗口内容随用户操作随时变化(切标签页/滚动/播放),TTL 内的"新鲜"帧
        // 也可能内容已过时:每次召唤一律重截,预览保持实时。
        // The focused window's content changes with user actions at any moment
        // (tab switches / scrolling / playback); even a TTL-"fresh" frame can be
        // outdated content -- recapture unconditionally at every summon.
        assert_eq!(
            summon_refresh_decision(Some((fresh, 640)), 640, now, true, true),
            SummonRefreshDecision::FrontmostStale
        );
        assert_eq!(
            summon_refresh_decision(Some((stale, 512)), 640, now, true, true),
            SummonRefreshDecision::FrontmostStale
        );
        assert_eq!(
            summon_refresh_decision(None, 640, now, true, true),
            SummonRefreshDecision::Missing
        );
        // 同 PID 的兄弟窗口与后台窗口不受影响:TTL 内依旧 Fresh,不随焦点窗口
        // 成串重截。
        // Same-PID siblings and background windows are unaffected: still Fresh
        // within the TTL, no bulk recapture piggybacking on the focused window.
        assert_eq!(
            summon_refresh_decision(Some((fresh, 640)), 640, now, true, false),
            SummonRefreshDecision::Fresh
        );
        assert_eq!(
            summon_refresh_decision(Some((fresh, 640)), 640, now, false, true),
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
    fn blank_modal_coverage_detects_suspended_webview_frames() {
        // 挂起 WebView 特征:内容区逐像素一致的纯白,标题条带内有红绿灯——
        // 红绿灯在被跳过的标题条带里,不影响内容区覆盖率 1.0。
        // Suspended-WebView signature: a pixel-uniform white body plus traffic
        // lights inside the skipped title strip -- coverage over content rows is 1.0.
        let (w, h) = (16usize, 16usize);
        let mut rgba = vec![0xFFu8; w * h * 4];
        for (x, rgb) in [(2, (237, 106, 94)), (5, (245, 191, 79)), (8, (99, 197, 84))] {
            let i = x * 4;
            rgba[i] = rgb.0;
            rgba[i + 1] = rgb.1;
            rgba[i + 2] = rgb.2;
        }
        let coverage = blank_modal_coverage(&rgba, w, 2, h).unwrap();
        assert_eq!(coverage, 1.0);
        assert!(coverage >= BLANK_MODAL_COVERAGE_MIN);
    }

    #[test]
    fn blank_modal_coverage_accepts_realistic_ui() {
        // 侧栏 + 白底 + 文本行的真实 UI:多颜色桶分摊,单一桶覆盖率远低于阈值。
        // A realistic UI (sidebar + white canvas + text rows) spreads across many
        // buckets; no single bucket approaches the threshold.
        let (w, h) = (16usize, 16usize);
        let mut rgba = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                if x < 5 {
                    rgba[i] = 40;
                    rgba[i + 1] = 40;
                    rgba[i + 2] = 46;
                } else if y % 2 == 0 {
                    rgba[i] = 255;
                    rgba[i + 1] = 255;
                    rgba[i + 2] = 255;
                } else {
                    rgba[i] = 200;
                    rgba[i + 1] = 210;
                    rgba[i + 2] = 220;
                }
                rgba[i + 3] = 255;
            }
        }
        let coverage = blank_modal_coverage(&rgba, w, 2, h).unwrap();
        assert!(coverage < BLANK_MODAL_COVERAGE_MIN);
    }

    #[test]
    fn blank_modal_coverage_flags_uniform_dark_content() {
        // 暗色主题的挂起帧同样判空白:内容区是逐像素一致的深色,桶覆盖率 1.0。
        // A dark-theme suspended frame is blank too: the body is a pixel-uniform
        // dark color with bucket coverage 1.0.
        let (w, h) = (8usize, 8usize);
        let mut rgba = Vec::with_capacity(w * h * 4);
        for _ in 0..w * h {
            rgba.extend_from_slice(&[10, 10, 12, 255]);
        }
        assert!(blank_modal_coverage(&rgba, w, 1, h).unwrap() >= BLANK_MODAL_COVERAGE_MIN);
    }

    #[test]
    fn blank_modal_coverage_quantizes_mild_noise_and_rejects_empty_region() {
        // 5bit 量化把 250 与 255 归入同桶:轻微压缩噪声不会把空白帧误判为正常。
        // 5-bit quantization buckets 250 with 255: mild compression noise cannot
        // hide a blank frame.
        let (w, h) = (8usize, 8usize);
        let mut rgba = vec![255u8; w * h * 4];
        for y in 1..h {
            rgba[(y * w + 1) * 4] = 250;
        }
        assert!(blank_modal_coverage(&rgba, w, 1, h).unwrap() >= BLANK_MODAL_COVERAGE_MIN);
        // 没有内容行可统计时返回 None。
        // No content rows to measure -> None.
        assert!(blank_modal_coverage(&rgba, w, h, h).is_none());
        assert!(blank_modal_coverage(&[], 0, 0, 0).is_none());
    }

    #[test]
    fn blank_frame_action_matrix() {
        use BlankFrameAction::*;
        // 后台 + 缓存为空:首帧入缓存作为占位种子(替代原先的图标回落)。
        // Background + empty cache: the first frame is stored as a placeholder
        // seed (replacing the old icon fallback).
        assert_eq!(
            blank_frame_action(false, false, false, false, false),
            StoreSeed
        );
        assert_eq!(
            blank_frame_action(false, true, false, false, false),
            StoreSeed
        );
        // 后台 + 已有帧:空白帧一律丢弃保住最后有效帧(升级单向)。
        // Background + cached frame: blanks are always dropped to keep the
        // last-known-good image (one-way upgrade).
        assert_eq!(
            blank_frame_action(false, false, true, false, false),
            DiscardKeepLastGood
        );
        assert_eq!(
            blank_frame_action(false, true, true, true, false),
            DiscardKeepLastGood
        );
        // 前台空白是用户眼前的真实画面,如实入缓存(包括激活补拍无旧帧可保时)。
        // A blank frontmost frame is what the user sees; store it (also when an
        // activation refresh has no good frame to protect).
        assert_eq!(blank_frame_action(true, false, false, false, false), Store);
        assert_eq!(blank_frame_action(true, true, false, true, false), Store);
        // 前台激活补拍仍空白且已有旧帧:丢弃并延迟重试一次。
        // A blank frontmost activation refresh with an existing frame: drop it and
        // schedule one delayed retry.
        assert_eq!(
            blank_frame_action(true, true, true, true, false),
            DiscardRetryActivation
        );
        // 重试名额已占用:不再无限重试,如实入缓存。
        // Retry slot already taken: stop retrying and store the truth.
        assert_eq!(blank_frame_action(true, true, true, false, false), Store);
        // 外观切换重拍:空白覆盖后台保护。
        // Appearance-transition recaptures: blank overwrites the background
        // protection.
        assert_eq!(
            blank_frame_action(false, false, true, false, true),
            StoreAppearanceRefresh
        );
        assert_eq!(
            blank_frame_action(false, false, false, false, true),
            StoreAppearanceRefresh
        );
        // 激活重试仍优先于外观覆盖(主题任务与激活请求合并时):先走 1.4s 兜底,
        // 重试拍到的真实帧同样满足外观一致性。
        // The activation retry still outranks the appearance overwrite (a theme
        // job merged with an activation request): take the 1.4s backstop first --
        // the retried real frame satisfies appearance consistency too.
        assert_eq!(
            blank_frame_action(true, true, true, true, true),
            DiscardRetryActivation
        );
        // 重试名额已占用的激活任务携带外观标志:如实入库(外观语义)。
        // An activation job with the appearance flag but no retry slot left:
        // store as truth (appearance semantics).
        assert_eq!(
            blank_frame_action(true, true, true, false, true),
            StoreAppearanceRefresh
        );
    }

    #[test]
    fn capture_state_propagates_appearance_refresh_flag() {
        let mut state = CaptureState::default();
        let key = ThumbKey { pid: 10, wid: 20 };

        // 独立的外观任务:标志随任务带出。
        // A standalone appearance request: the flag rides out with the job.
        assert!(state.request_for_generation(key, 512, CapturePriority::Visible, 0, true));
        let job = state.take_next().unwrap();
        assert!(job.appearance_refresh);
        assert!(!state.finish(job));

        // 普通任务默认不带标志。
        // Ordinary jobs carry no flag by default.
        assert!(state.request(key, 512, CapturePriority::Startup));
        let job = state.take_next().unwrap();
        assert!(!job.appearance_refresh);
        assert!(!state.finish(job));

        // 外观请求合并进已 pending 的普通任务:标志点亮(OR 语义)。
        // An appearance request merging into a pending ordinary job lights the
        // flag up (OR semantics).
        assert!(state.request(key, 512, CapturePriority::Startup));
        assert!(!state.request_for_generation(key, 512, CapturePriority::Visible, 0, true));
        let job = state.take_next().unwrap();
        assert!(job.appearance_refresh);
        assert_eq!(job.priority, CapturePriority::Visible);
        assert!(!state.finish(job));

        // 激活补拍路径不携带外观标志。
        // The activation path never carries the appearance flag.
        assert!(state.request_activation(key, 512, Instant::now(), 0));
        let job = state.take_next().unwrap();
        assert!(!job.appearance_refresh);
    }

    #[test]
    fn capture_state_requeues_when_appearance_refresh_merges_into_running_job() {
        let mut state = CaptureState::default();
        let key = ThumbKey { pid: 10, wid: 20 };

        // 同优先级的普通任务已开始执行;主题刷新此时合入,三个常规字段都不变。
        // An ordinary job of the SAME priority is already running when the theme
        // refresh merges in -- none of the three regular fields change.
        assert!(state.request(key, 512, CapturePriority::Visible));
        let running = state.take_next().unwrap();
        assert!(!running.appearance_refresh);
        assert!(!state.request_for_generation(key, 512, CapturePriority::Visible, 0, true));

        // 旧任务完成时必须保留 pending 触发外观补拍,而不是静默删除。
        // Finishing the old job must keep the pending entry for the appearance
        // recapture instead of silently dropping it.
        assert!(state.finish(running));
        let follow_up = state.take_next().unwrap();
        assert!(follow_up.appearance_refresh);

        // 补拍任务自身携带标志,完成时不再循环。
        // The follow-up carries the flag itself, so finishing it does not loop.
        assert!(!state.finish(follow_up));
        assert!(state.take_next().is_none());
    }

    #[test]
    fn capture_state_ignores_duplicate_activation_scheduling_for_the_same_token() {
        let mut state = CaptureState::default();
        let key = ThumbKey { pid: 10, wid: 20 };
        let activated_at = Instant::now();

        assert!(state.request_activation(key, 512, activated_at, 0));
        let running = state.take_next().unwrap();
        assert_eq!(running.activation_at, Some(activated_at));

        // 同 token 的第二次调度(808 与 backstop 双路径)合入运行中任务:不推进
        // 新鲜度,任务完成时不得追加多余的 follow-up 重拍。
        // A second scheduling of the SAME token (the 808 + backstop dual paths)
        // merges into the running job without advancing freshness; finishing it
        // must not append a redundant follow-up capture.
        assert!(!state.request_activation(key, 512, activated_at, 0));
        assert!(!state.finish(running));
        assert!(state.take_next().is_none());

        // 真正的新激活(不同 token)仍推进新鲜度并触发补拍。
        // A genuinely new activation (different token) still advances freshness.
        let later = activated_at + Duration::from_millis(1);
        assert!(state.request_activation(key, 512, later, 0));
        let job = state.take_next().unwrap();
        assert_eq!(job.activation_at, Some(later));
        assert!(!state.finish(job));
        assert!(state.take_next().is_none());
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
