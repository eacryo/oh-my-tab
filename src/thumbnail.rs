//! 窗口缩略图:私有 SkyLight API `SLSHWCaptureWindowList` 截取窗口画面,
//! **纯内存 LRU** 缓存(刻意不落盘——屏幕内容明文落盘有隐私风险,BetterCmdTab/
//! DockDoor 同样只保留内存)。五条生产线:
//! 1. 启动预生成:监视线程启动时枚举所有运行中 App 的标准窗口补拍
//! 2. 常驻监听:每 PID 一个 AXObserver 订阅 kAXWindowCreatedNotification,
//!    新窗口防抖 300ms 后预生成(等窗口完成初始化,避免拍到白屏)
//! 3. 召唤补拍:show_overlay 时对可见区间及两侧预取项中的缺失帧、前台 App
//!    的过期帧入队；后台 App 保留最后一张有效帧，完成后主线程原位换卡
//! 4. 激活刷新:NSWorkspace 确认焦点窗口后延迟补拍，等 Web 内容完成恢复/重绘
//! 5. 前台预热:浮窗关闭时仅对当前前台窗口低频补拍，减少视频/动态页面在召唤时使用旧帧
//!
//! 6. 空白帧门控:WKWebView(Tauri/Electron 等)的页面由独立 WebContent 进程
//!    渲染,窗口长时间后台后该进程被挂起、内容表面被 WindowServer 丢弃,截出来
//!    只剩"标题栏(红绿灯)+纯色白屏"。此类帧按场景分流(AltTab 同策略):
//!    - 后台 + 缓存有帧:丢弃,保住最后一张有效帧(升级单向,避免回退)
//!    - 后台 + 缓存为空:入缓存作为占位种子(好过图标卡;激活后自动升级)
//!    - 前台:如实入缓存(用户眼前的真实画面)
//!    - 激活补拍仍空白:丢弃并延迟重试一次,给 WebContent 恢复重绘留时间
//!    - 外观(明暗)切换重拍:空白也覆盖——旧外观帧与新主题不协调比占位更刺眼
//!    - 切换器自己切过去的窗口:激活补拍在 backstop 静默出口放行;同应用窗口切换
//!      (无激活通知、808 被静音)在 raise 时铸造 token 直接调度,到达即刷新
//! 7. WindowServer 几何过渡:显示器/Space/窗口动画期间延迟捕获并有界退避重试;
//!    scheduler 为进程级单例,随进程结束
//!
//! 无屏幕录制权限(TCC)时整个模块休眠,浮窗保持纯图标渲染;运行中授权后
//! 下一个捕获任务自动恢复(worker 每个任务前都重新 preflight)。
//!
//!
//! Window thumbnails: capture window imagery via the private SkyLight API
//! `SLSHWCaptureWindowList`, cached in a **memory-only LRU** (deliberately never
//! written to disk -- plaintext screen content in ~/Library/Caches is a privacy
//! risk; BetterCmdTab/DockDoor likewise keep frames in RAM only). Five producers:
//! 1. startup pre-generation: enumerate every running app's standard windows
//! 2. resident listener: one AXObserver per PID watching kAXWindowCreatedNotification;
//!    a new window debounces 300ms (letting it finish initializing, avoiding a white
//!    flash) then pre-generates
//! 3. summon refresh: show_overlay enqueues missing windows and stale frames from the
//!    frontmost app in the visible slice plus prefetch margins; background apps retain
//!    their last-known-good frame, and results swap affected cards in place on the main thread
//! 4. activation refresh: after NSWorkspace resolves the focused window, capture it with a
//!    short delay so restored web content has time to redraw.
//! 5. focused prewarm: while the overlay is hidden, recapture only the current frontmost
//!    window at a low rate so video/dynamic pages are less stale at summon time.
//!
//! 6. blank-frame gating: WKWebView-based apps (Tauri/Electron et al.) render in a
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
//! 7. WindowServer geometry transitions: defer captures during display/Space/window
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
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Condvar, LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::ffi::{
    CFArrayGetCount, CFArrayGetValueAtIndex, CFRelease, CFRetain, CFStringCompare,
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGBitmapContextGetData,
    CGColorSpaceCreateDeviceRGB, CGContextDrawImage, CGImageGetHeight, CGImageGetWidth,
    CGPreflightScreenCaptureAccess, CGRect, CGRequestScreenCaptureAccess, RetainedCf,
};
use crate::skylight;
use crate::{log_debug, log_info};

mod blank_frame;
mod cache;
mod capture;
mod pregen;
mod summon;
use blank_frame::*;
use cache::*;
use capture::*;
// 父模块自身不再直接调用 summon(调用方在 overlay);glob 仅测试模块需要。
// The parent no longer calls summon directly (overlay does); the glob is test-only.
pub(crate) use pregen::{app_launched, app_terminated, start};
#[cfg(test)]
use summon::*;
// 对 crate 其他模块暴露的入口(内部子模块实现)。
// Entry points exposed to the rest of the crate (implemented in the child modules).
pub(crate) use cache::{
    cache_stats, clear_runtime_cache, forget_destroyed_window, frame_epoch, lookup_retained,
    touch_cached_frame,
};
pub(crate) use capture::{
    handle_ready_main, log_capture_metrics, start_focused_prewarm_worker,
    stop_focused_prewarm_worker, wake_capture_worker,
};
pub(crate) use summon::{
    refresh_for_display_change, refresh_for_summon, refresh_for_theme, refresh_selected_for_summon,
};

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
    schedule_focused_prewarm(pid, wid);
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
/// stored, the retry task terminates unsuccessfully, the retry is abandoned, or the
/// app terminates.
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
            let Some(result) =
                enqueue_blank_retry_job(key, target_px_h, activated_at, pid_generation)
            else {
                log_debug!(
                    "[thumb] blank retry skipped (slot cleared) pid={} wid={}",
                    key.pid,
                    key.wid
                );
                return;
            };
            // 结果明确区分独立入队、合入排队任务和未入队；后者在 helper 内已归还名额。
            // Distinguish an independent enqueue, a merge into a queued task, and a
            // dropped retry; the helper releases the slot for the last case.
            log_debug!(
                "[thumb] blank retry: pid={} wid={} result={:?} target_h={}",
                key.pid,
                key.wid,
                result,
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

    static FOCUSED_PREWARM_LIFECYCLE_TEST_LOCK: LazyLock<Mutex<()>> =
        LazyLock::new(|| Mutex::new(()));

    #[test]
    fn focused_prewarm_failures_back_off_and_converge() {
        assert_eq!(focused_prewarm_failure_backoff(0), None);
        assert_eq!(
            focused_prewarm_failure_backoff(1),
            Some(Duration::from_millis(5_000))
        );
        assert_eq!(
            focused_prewarm_failure_backoff(2),
            Some(Duration::from_millis(10_000))
        );
        assert_eq!(focused_prewarm_failure_backoff(3), None);
        // Three consecutive failures clear the target in note_focused_prewarm_failure; there is
        // no seconds-scale infinite retry loop.
        // 连续三次失败会清理目标，不会形成数秒级无限重试。
    }

    #[test]
    fn focused_prewarm_worker_cas_handoff_rejects_stale_cleanup() {
        let _guard = FOCUSED_PREWARM_LIFECYCLE_TEST_LOCK.lock().unwrap();
        let old_epoch = 41;
        let replacement_epoch = old_epoch + 1;
        FOCUSED_PREWARM_EPOCH.store(old_epoch, Ordering::Release);
        FOCUSED_PREWARM_WORKER_STARTED.store(false, Ordering::Release);
        FOCUSED_PREWARM_ACTIVE_GENERATION.store(0, Ordering::Release);

        assert!(claim_focused_prewarm_worker(old_epoch));
        assert!(!claim_focused_prewarm_worker(old_epoch));
        assert_eq!(
            FOCUSED_PREWARM_ACTIVE_GENERATION.load(Ordering::Acquire),
            old_epoch + 1
        );

        // Stop/re-enable advances the epoch; the old cleanup releases its slot and the real
        // restart decision hands ownership to exactly one replacement worker.
        // 停止/重新启用会推进代际；旧清理释放槽位，真实重启判定只交接给一个新 worker。
        FOCUSED_PREWARM_EPOCH.store(replacement_epoch, Ordering::Release);
        assert!(release_focused_prewarm_worker(old_epoch));
        assert_eq!(
            focused_prewarm_exit_action(old_epoch, replacement_epoch, true, true),
            FocusedPrewarmExitAction::Restart
        );
        assert!(claim_focused_prewarm_worker(replacement_epoch));

        // A late cleanup from the old generation cannot clear the replacement worker.
        // 旧代际迟到的清理不能清掉新 worker。
        assert!(!release_focused_prewarm_worker(old_epoch));
        assert!(FOCUSED_PREWARM_WORKER_STARTED.load(Ordering::Acquire));
        assert_eq!(
            FOCUSED_PREWARM_ACTIVE_GENERATION.load(Ordering::Acquire),
            replacement_epoch + 1
        );
        assert!(release_focused_prewarm_worker(replacement_epoch));
        assert!(!FOCUSED_PREWARM_WORKER_STARTED.load(Ordering::Acquire));
        assert_eq!(FOCUSED_PREWARM_ACTIVE_GENERATION.load(Ordering::Acquire), 0);
    }

    #[test]
    fn focused_prewarm_throttle_uses_cache_age_and_next_deadline() {
        let now = Instant::now();
        let due = now - Duration::from_millis(FOCUSED_PREWARM_INTERVAL_MS);
        let too_fresh = now - Duration::from_millis(FOCUSED_PREWARM_INTERVAL_MS - 1);
        assert!(focused_prewarm_due(now, None, due));
        assert!(focused_prewarm_due(now, Some(due), due));
        assert!(!focused_prewarm_due(now, Some(too_fresh), due));
        assert!(!focused_prewarm_due(
            now,
            Some(due),
            now + Duration::from_secs(1)
        ));
        assert_eq!(
            focused_prewarm_ready_at(Some(too_fresh), due),
            now + Duration::from_millis(1)
        );
        assert_eq!(
            focused_prewarm_ready_at(Some(due), now + Duration::from_secs(1)),
            now + Duration::from_secs(1)
        );
    }

    #[test]
    fn focused_prewarm_target_revision_rejects_an_old_enumeration() {
        let _guard = FOCUSED_PREWARM_LIFECYCLE_TEST_LOCK.lock().unwrap();
        let expected = FocusedPrewarmTarget {
            pid: 10,
            wid: 20,
            pid_generation: 3,
            target_revision: next_focused_prewarm_target_revision(),
            needs_resolution: false,
            consecutive_failures: 0,
            next_attempt: Instant::now(),
        };
        let mut replacement = expected.clone();
        replacement.wid = 21;
        replacement.target_revision = next_focused_prewarm_target_revision();
        *FOCUSED_PREWARM_TARGET.lock().unwrap() = Some(replacement.clone());

        assert!(update_focused_prewarm_target_if_current(&expected, 22).is_none());
        assert!(!focused_prewarm_revision_is_current(Some(
            expected.target_revision
        )));
        assert!(focused_prewarm_revision_is_current(Some(
            replacement.target_revision
        )));
        assert_eq!(
            FOCUSED_PREWARM_TARGET.lock().unwrap().as_ref(),
            Some(&replacement)
        );
        *FOCUSED_PREWARM_TARGET.lock().unwrap() = None;
    }

    #[test]
    fn clearing_focused_prewarm_target_invalidates_queued_job_revision() {
        let _guard = FOCUSED_PREWARM_LIFECYCLE_TEST_LOCK.lock().unwrap();
        let old_revision = next_focused_prewarm_target_revision();
        *FOCUSED_PREWARM_TARGET.lock().unwrap() = Some(FocusedPrewarmTarget {
            pid: 10,
            wid: 20,
            pid_generation: 3,
            target_revision: old_revision,
            needs_resolution: false,
            consecutive_failures: 0,
            next_attempt: Instant::now(),
        });

        assert!(focused_prewarm_revision_is_current(Some(old_revision)));
        assert!(invalidate_focused_prewarm_target(|_| true, "test-clear"));
        assert!(FOCUSED_PREWARM_TARGET.lock().unwrap().is_none());
        assert!(!focused_prewarm_revision_is_current(Some(old_revision)));
    }

    #[test]
    fn new_window_priority_beats_focused_prewarm() {
        let mut state = CaptureState::default();
        let prewarm = ThumbKey { pid: 10, wid: 20 };
        let new_window = ThumbKey { pid: 10, wid: 21 };
        assert!(state.request(prewarm, 512, CapturePriority::FocusedPrewarm));
        assert!(state.request(new_window, 512, CapturePriority::NewWindow));
        assert_eq!(state.take_next().unwrap().key, new_window);
    }

    #[test]
    fn geometry_plausibility_reports_independent_reasons() {
        assert_eq!(
            geometry_reject_reason(32, 32, None),
            Some(GeometryRejectReason::SourceTooSmall)
        );
        assert_eq!(
            geometry_reject_reason(1_000, 64, None),
            Some(GeometryRejectReason::SourceAspect)
        );
        assert_eq!(
            geometry_reject_reason(1_000, 500, Some((0.0, 0.0, 20.0, 100.0))),
            Some(GeometryRejectReason::ExpectedAspect)
        );
        assert_eq!(geometry_reject_reason(1_000, 500, None), None);
    }

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

        assert_eq!(
            state.request_activation(key, 512, first_activation, 0),
            ActivationRequestResult::Enqueued
        );
        let first = state.take_next().unwrap();
        assert_eq!(
            state.request_activation(key, 512, later_activation, 0),
            ActivationRequestResult::Merged { running: true }
        );
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
    fn capture_state_invalidates_queued_and_in_flight_jobs_by_window() {
        let mut state = CaptureState::default();
        let running_key = ThumbKey { pid: 10, wid: 20 };
        let queued_key = ThumbKey { pid: 10, wid: 21 };
        assert!(state.request(running_key, 512, CapturePriority::Visible));
        assert!(state.request(queued_key, 512, CapturePriority::Startup));
        let running = state.take_next().unwrap();
        assert!(state.is_current(running));

        assert!(state.invalidate_window(running_key));
        assert!(!state.is_current(running));
        assert!(state.take_next().is_some());
        assert!(state.invalidate_window(queued_key));
        assert!(state.take_next().is_none());
        assert!(!state.invalidate_window(running_key));
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
    fn lru_detailed_insert_reports_capacity_eviction_without_replacement() {
        let mut lru: Lru<u32, u64> = Lru::new(2, u64::MAX, |v| *v);
        lru.put_detailed(1, 10);
        lru.put_detailed(2, 20);
        let result = lru.put_detailed(3, 30);

        assert_eq!(result.key, 3);
        assert_eq!(result.cost, 30);
        assert_eq!(result.operation, LruPutOperation::Insert);
        assert!(result.replaced.is_none());
        assert_eq!(result.evicted, vec![(1, 10)]);
        assert!(result.count_over_limit);
        assert!(!result.cost_over_limit);
    }

    #[test]
    fn lru_detailed_replacement_can_evict_for_increased_cost() {
        let mut lru: Lru<u32, u64> = Lru::new(3, 30, |v| *v);
        lru.put_detailed(1, 10);
        lru.put_detailed(2, 10);
        let result = lru.put_detailed(1, 25);

        assert_eq!(result.operation, LruPutOperation::Replace);
        assert_eq!(result.replaced_cost, Some(10));
        assert_eq!(result.replaced, Some(10));
        assert_eq!(result.evicted, vec![(2, 10)]);
        assert!(!result.count_over_limit);
        assert!(result.cost_over_limit);
        assert_eq!(lru.get(&1), Some(25));
        assert!(lru.get(&2).is_none());
    }

    #[test]
    fn lru_priority_evicts_outside_recent_workset_first() {
        let mut lru: Lru<u32, u64> = Lru::new(2, u64::MAX, |v| *v);
        lru.put_detailed_with_priority(1, 10, |_| false);
        lru.put_detailed_with_priority(2, 20, |_| true);
        let result = lru.put_detailed_with_priority(3, 30, |key| *key != 1);

        assert_eq!(result.evicted, vec![(1, 10)]);
        assert!(lru.get(&2).is_some());
        assert!(lru.get(&3).is_some());
    }

    #[test]
    fn recent_workset_membership_expires_after_ttl() {
        let key = ThumbKey { pid: 10, wid: 20 };
        let now = Instant::now();
        let fresh = WorksetSnapshot {
            keys: HashSet::from([key]),
            updated_at: Some(now - WORKSET_SNAPSHOT_MAX_AGE),
        };
        assert_eq!(recent_workset_membership_at(&fresh, key, now), Some(true));

        let expired = WorksetSnapshot {
            keys: HashSet::from([key]),
            updated_at: Some(now - WORKSET_SNAPSHOT_MAX_AGE - Duration::from_millis(1)),
        };
        assert_eq!(recent_workset_membership_at(&expired, key, now), None);
    }

    #[test]
    fn format_workset_uses_pid_and_window_id_pairs() {
        let keys = [ThumbKey { pid: 10, wid: 20 }, ThumbKey { pid: 11, wid: 21 }];
        assert_eq!(format_workset(&keys), "10:20,11:21");
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
        assert_eq!(
            state.request_activation(key, 512, Instant::now(), 0),
            ActivationRequestResult::Enqueued
        );
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

        assert_eq!(
            state.request_activation(key, 512, activated_at, 0),
            ActivationRequestResult::Enqueued
        );
        let running = state.take_next().unwrap();
        assert_eq!(running.activation_at, Some(activated_at));

        // 同 token 的第二次调度(808 与 backstop 双路径)合入运行中任务:不推进
        // 新鲜度,任务完成时不得追加多余的 follow-up 重拍。
        // A second scheduling of the SAME token (the 808 + backstop dual paths)
        // merges into the running job without advancing freshness; finishing it
        // must not append a redundant follow-up capture.
        assert_eq!(
            state.request_activation(key, 512, activated_at, 0),
            ActivationRequestResult::Merged { running: true }
        );
        assert!(!state.finish(running));
        assert!(state.take_next().is_none());

        // 真正的新激活(不同 token)仍推进新鲜度并触发补拍。
        // A genuinely new activation (different token) still advances freshness.
        let later = activated_at + Duration::from_millis(1);
        assert_eq!(
            state.request_activation(key, 512, later, 0),
            ActivationRequestResult::Enqueued
        );
        let job = state.take_next().unwrap();
        assert_eq!(job.activation_at, Some(later));
        assert!(!state.finish(job));
        assert!(state.take_next().is_none());
    }

    #[test]
    fn activation_request_reports_merge_and_rejection_separately() {
        let mut state = CaptureState::default();
        let key = ThumbKey { pid: 10, wid: 20 };
        let activated_at = Instant::now();

        assert_eq!(
            state.request_activation(key, 512, activated_at, 0),
            ActivationRequestResult::Enqueued
        );
        assert_eq!(
            state.request_activation(key, 512, activated_at, 0),
            ActivationRequestResult::Merged { running: false }
        );

        let running = state.take_next().unwrap();
        assert_eq!(
            state.request_activation(key, 512, activated_at, 0),
            ActivationRequestResult::Merged { running: true }
        );
        assert!(!state.finish(running));

        state.cancel_pid(key.pid);
        assert_eq!(
            state.request_activation(key, 512, activated_at, 1),
            ActivationRequestResult::Rejected
        );
    }

    #[test]
    fn blank_retry_marker_is_cleared_only_for_its_captured_job() {
        let mut state = CaptureState::default();
        let key = ThumbKey { pid: 10, wid: 20 };
        assert_eq!(
            state.request_activation(key, 512, Instant::now(), 0),
            ActivationRequestResult::Enqueued
        );
        state.desired.get_mut(&key).unwrap().blank_retry = true;
        let job = state.take_next().unwrap();
        assert!(state.clear_blank_retry_marker(job));
        assert!(!state.desired.get(&key).unwrap().blank_retry);

        let stale = CaptureJob {
            token: job.token.wrapping_add(1),
            ..job
        };
        state.desired.get_mut(&key).unwrap().blank_retry = true;
        assert!(!state.clear_blank_retry_marker(stale));
        assert!(state.desired.get(&key).unwrap().blank_retry);
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
