//! 窗口收集 · raiser:AX 阶段后台化(专职 raiser 线程)与抬升代数防乱序回跳。
//! Background AX phase (dedicated raiser thread) with generation-guarded ordering.

use super::*;

// ========== AX 阶段后台化(专职 raiser 线程) / background AX phase (dedicated raiser) ==========

// 最新抬升意图代号:每次提交切换自增;后台任务在应用 AX 变更前重查,已被更新的切换
// 取代就中止——快速连续切换时,避免旧任务把旧窗口又抬回新窗口上面(乱序回跳)。
// Generation of the latest raise intent: bumped on every committed switch. Background jobs
// re-check before applying AX mutations and abort once a newer switch supersedes them --
// during rapid consecutive switches, a stale job must never re-raise an old window over the
// newest one (out-of-order flicker).
pub(super) static RAISE_GENERATION: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

pub(super) fn raise_intent_current(generation: u64) -> bool {
    RAISE_GENERATION.load(std::sync::atomic::Ordering::Acquire) == generation
}

struct RaiseJob {
    pid: i32,
    cgwid: u32,
    minimized: bool,
    fast_path_ok: bool,
    generation: u64,
    enqueued_at: Instant,
}

/// AX mutations are delivered to AppKit and must run on the main thread.  Keep the potentially
/// blocking AX lookup on `ax-raiser`, then retain the resolved objects until the main-thread
/// callback applies the final raise/focus actions.
struct MainThreadAxRaise {
    pid: i32,
    cgwid: u32,
    app: AXUIElementRef,
    element: AXUIElementRef,
    focused_key: AXUIElementRef,
    raise_key: AXUIElementRef,
    minimized_key: Option<AXUIElementRef>,
    force_focus: bool,
    generation: u64,
}

unsafe impl Send for MainThreadAxRaise {}

static MAIN_THREAD_AX_RAISES: LazyLock<Mutex<Vec<MainThreadAxRaise>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// Drain AX mutations on the AppKit main thread.  `AXUIElementPerformAction` can synchronously
/// enter AppKit (for example `makeKeyAndOrderFront:`), so invoking it from the background AX
/// worker triggers macOS's "Must only be used from the main thread" trap.
pub(crate) fn handle_ax_raise_main() {
    let jobs = MAIN_THREAD_AX_RAISES
        .lock()
        .unwrap()
        .drain(..)
        .collect::<Vec<_>>();
    for job in jobs {
        unsafe {
            if !raise_intent_current(job.generation) {
                release_main_thread_ax_raise(&job);
                continue;
            }

            let minimized_set_err = job
                .minimized_key
                .map(|key| AXUIElementSetAttributeValue(job.element, key, kCFBooleanFalse));
            if minimized_set_err.is_some() {
                let (slps_ok, click_ok) = raise_window_fast(job.pid, job.cgwid);
                log_debug!(
                    "[raise] precise fast after main-thread unminimize: pid={} cgwid={} slps={} click={}",
                    job.pid,
                    job.cgwid,
                    slps_ok,
                    click_ok
                );
            }
            let raise_started = Instant::now();
            let raise_first_err = AXUIElementPerformAction(job.element, job.raise_key);
            let raise_first_us = raise_started.elapsed().as_micros();
            // 对端超时(-25204)时不再做 focus 兜底与重试:这两次调用同样只会再各等一个超时,
            // 实测 first/set_focused/retry 三次全是 -25204,主线程被占住约 4.5s。快速路径的 SLPS
            // 抬窗已经生效,这里直接放弃 AX 兜底,把这 3 段超时压成 1 段。
            // Drop the focus backstop and its retry once the target times out (-25204): both calls
            // would only wait out another timeout each -- the log shows first/set_focused/retry all
            // returning -25204 with the main thread blocked for ~4.5s. The SLPS fast raise already
            // ran, so give up the AX backstop and collapse three timeouts into one.
            let timed_out = raise_first_err == K_AX_CANNOT_COMPLETE;
            let (focused_set_err, raise_retry_err) = if timed_out
                || (!job.force_focus
                    && (raise_first_err == K_AX_SUCCESS
                        || raise_first_err == K_AX_INVALID_UI_ELEMENT))
            {
                (None, None)
            } else {
                let focused_set_err =
                    AXUIElementSetAttributeValue(job.app, job.focused_key, job.element);
                let raise_retry_err = AXUIElementPerformAction(job.element, job.raise_key);
                (Some(focused_set_err), Some(raise_retry_err))
            };
            log_debug!(
                "[raise] AXRaise main: pid={} cgwid={} first_err={} first_us={} set_focused={:?} retry={:?} set_minimized={:?}",
                job.pid,
                job.cgwid,
                raise_first_err,
                raise_first_us,
                focused_set_err,
                raise_retry_err,
                minimized_set_err
            );
            release_main_thread_ax_raise(&job);
        }
    }
}

unsafe fn release_main_thread_ax_raise(job: &MainThreadAxRaise) {
    CFRelease(job.app);
    CFRelease(job.element);
    CFRelease(job.focused_key);
    CFRelease(job.raise_key);
    if let Some(key) = job.minimized_key {
        CFRelease(key);
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn enqueue_main_thread_ax_raise(
    pid: i32,
    cgwid: u32,
    app: AXUIElementRef,
    element: AXUIElementRef,
    focused_key: AXUIElementRef,
    raise_key: AXUIElementRef,
    minimized_key: Option<AXUIElementRef>,
    force_focus: bool,
    generation: u64,
) {
    if !raise_intent_current(generation) {
        return;
    }
    // The caller retains these objects for its own cleanup.  Take an additional retain for the
    // queued main-thread job, including the array-borrowed window element.
    CFRetain(app);
    CFRetain(element);
    CFRetain(focused_key);
    CFRetain(raise_key);
    if let Some(key) = minimized_key {
        CFRetain(key);
    }
    let pending = {
        let mut pending = MAIN_THREAD_AX_RAISES.lock().unwrap();
        let old = std::mem::take(&mut *pending);
        pending.push(MainThreadAxRaise {
            pid,
            cgwid,
            app,
            element,
            focused_key,
            raise_key,
            minimized_key,
            force_focus,
            generation,
        });
        old
    };
    // 主线程尚未消费时只保留最新 generation，旧任务的 retained AX 对象立即释放。
    // Keep only the newest generation while the main thread is busy; release retained AX
    // objects from superseded jobs immediately.
    for old in pending {
        release_main_thread_ax_raise(&old);
    }

    if let Some(controller) = crate::CONTROLLER.lock().unwrap().map(|ptr| ptr.0) {
        let _: () = msg_send![controller,
            performSelectorOnMainThread: sel!(handleAxRaise:),
            withObject: std::ptr::null::<AnyObject>(),
            waitUntilDone: false
        ];
    } else {
        // This can only happen during early shutdown/startup, but do not leave retained AX
        // objects behind if the controller has already gone away.
        let pending = MAIN_THREAD_AX_RAISES
            .lock()
            .unwrap()
            .drain(..)
            .collect::<Vec<_>>();
        for job in pending {
            release_main_thread_ax_raise(&job);
        }
    }
}

// 单一专职 raiser 线程:串行 FIFO 消费任务,保证两次切换的 AX 阶段不并发、
// 完成顺序与提交顺序一致(配合 supersede 检查,最终状态保持为最后一次切换)。
// A single dedicated raiser thread consumes jobs serially, so AX phases of consecutive
// switches never overlap and completion order equals commit order (combined with the
// supersede check, the final state is always the last switch).
struct RaiseQueue {
    tx: flume::Sender<RaiseJob>,
    rx: flume::Receiver<RaiseJob>,
}

static RAISE_QUEUE: std::sync::LazyLock<RaiseQueue> = std::sync::LazyLock::new(|| {
    let (tx, rx) = flume::bounded::<RaiseJob>(1);
    let worker_rx = rx.clone();
    std::thread::Builder::new()
        .name("ax-raiser".into())
        .spawn(move || {
            for job in worker_rx.iter() {
                run_raise_ax_job(job);
            }
        })
        .expect("spawn ax-raiser thread");
    RaiseQueue { tx, rx }
});

/// 提交后台 AX 精确抬升任务。普通窗口只执行 cached AXRaise;已知最小化窗口先还原。
/// Enqueue the serialized AX backstop. Normal windows only perform cached AXRaise; known
/// minimized windows are restored first.
/// AX 枚举对无响应 App 可能阻塞几十至上百毫秒,应放在后台线程执行。
///
/// AX enumeration can block tens to hundreds of milliseconds on an unresponsive app; it must
/// stay off the main thread.
pub(crate) fn raise_window_ax_async(
    pid: i32,
    cgwid: u32,
    minimized: bool,
    fast_path_ok: bool,
) -> u64 {
    if cgwid == 0 {
        return 0;
    }
    let generation = RAISE_GENERATION.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
    let job = RaiseJob {
        pid,
        cgwid,
        minimized,
        fast_path_ok,
        generation,
        enqueued_at: Instant::now(),
    };
    match RAISE_QUEUE.tx.try_send(job) {
        Ok(()) => {}
        Err(flume::TrySendError::Full(job)) => {
            // 队列满表示旧任务尚未开始；移除它并保留最新 generation。
            // A full slot means the old job has not started; replace it with the latest
            // generation instead of letting stale raises accumulate.
            let _ = RAISE_QUEUE.rx.try_recv();
            if RAISE_QUEUE.tx.try_send(job).is_err() {
                log_debug!("[raise] latest job could not replace queued job");
            }
        }
        Err(flume::TrySendError::Disconnected(_)) => {
            log_debug!("[raise] ax-raiser queue disconnected");
        }
    }
    generation
}

fn run_raise_ax_job(job: RaiseJob) {
    if !raise_intent_current(job.generation) {
        log_debug!(
            "[raise] ax superseded before start: pid={} cgwid={} gen={}",
            job.pid,
            job.cgwid,
            job.generation
        );
        return;
    }
    let started = Instant::now();
    log_debug!(
        "[raise] ax job start: pid={} cgwid={} gen={} queue_ms={}",
        job.pid,
        job.cgwid,
        job.generation,
        job.enqueued_at.elapsed().as_millis()
    );
    // 后台线程一律包 autorelease pool:当前只用 CF 对象,包一层防将来引入 ObjC 调用后泄漏。
    // Always wrap background work in an autorelease pool: this path only touches CF objects
    // today; the pool guards against leaks if ObjC calls are ever added.
    unsafe {
        let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
        let force_ax_focus = if job.minimized || job.fast_path_ok {
            !job.fast_path_ok
        } else {
            !retry_failed_fast_path(&job)
        };
        raise_window_ax_job(&job, started, force_ax_focus);
        let _: () = msg_send![pool, drain];
    }
}

/// Recover a failed synchronous raise without blocking the main thread.
/// The first attempt already ran at commit time; these retries cover the short-lived
/// process-table/WindowServer race seen when switching away from another application.
unsafe fn retry_failed_fast_path(job: &RaiseJob) -> bool {
    const RETRY_DELAYS_MS: [u64; 2] = [8, 20];
    let started = Instant::now();
    let activate_ok = activate_pid(job.pid);
    let mut last = (false, false);
    let mut attempts = 0;
    for delay_ms in RETRY_DELAYS_MS {
        if !raise_intent_current(job.generation) {
            log_debug!(
                "[raise] fast recovery superseded: pid={} cgwid={} gen={}",
                job.pid,
                job.cgwid,
                job.generation
            );
            return false;
        }
        std::thread::sleep(Duration::from_millis(delay_ms));
        attempts += 1;
        last = raise_window_fast(job.pid, job.cgwid);
        if last.0 && last.1 {
            log_debug!(
                "[raise] fast recovery succeeded: pid={} cgwid={} activate={} attempts={} elapsed={}ms",
                job.pid,
                job.cgwid,
                activate_ok,
                attempts,
                started.elapsed().as_millis()
            );
            return true;
        }
    }
    log_debug!(
        "[raise] fast recovery exhausted: pid={} cgwid={} activate={} slps={} click={} attempts={} elapsed={}ms",
        job.pid,
        job.cgwid,
        activate_ok,
        last.0,
        last.1,
        attempts,
        started.elapsed().as_millis()
    );
    false
}

/// AX 阶段本体:缓存命中时普通窗口只执行 AXRaise;已知最小化窗口先还原并补跑快速路径。
/// 缓存失效才枚举 AXWindows,按 CGWindowID 重新配对并刷新缓存。
/// AX phase: on a cache hit, normal windows only perform AXRaise; known minimized windows are
/// restored first and then run the fast path. Only a stale/missing cache enumerates AXWindows,
/// pairs by CGWindowID, and refreshes the cache.
unsafe fn raise_window_ax_job(job: &RaiseJob, started: Instant, force_ax_focus: bool) {
    let app_started = Instant::now();
    let app = AXUIElementCreateApplication(job.pid);
    if app.is_null() {
        log_info!("[raise] ax skipped: no AX app for pid={}", job.pid);
        return;
    }
    let process_start_time_us = resolve_app_identity(job.pid).process_start_time_us;
    let app_create_us = app_started.elapsed().as_micros();
    // 抬窗路径的 AX 全部按 AX_RAISE_MESSAGING_TIMEOUT 限时:窗口元素在 raise_ax_element 里
    // 单独设置,app 元素在这里设置(它不传递给子元素)。
    // Every AX call on the raise path is bounded by AX_RAISE_MESSAGING_TIMEOUT: the window element
    // is set inside raise_ax_element, the app element here (it is not inherited by children).
    AXUIElementSetMessagingTimeout(app, AX_RAISE_MESSAGING_TIMEOUT);

    let raise_key = cf_string_new("AXRaise");
    let focused_key = cf_string_new("AXFocusedWindow");
    let minimized_key = job.minimized.then(|| cf_string_new("AXMinimized"));

    // collect_windows 已经为当前窗口保留了 AX 元素。正常切换直接复用它，避免每次都做
    // 一次 AXWindows IPC；只有元素失效时才回退到下面的实时枚举。
    // collect_windows retains the AX element for the current window. Reuse it for normal
    // switches to avoid an AXWindows IPC round trip; only stale elements use the live scan below.
    if let Some(element) = cached_ax_window_element(job.pid, process_start_time_us, job.cgwid) {
        if !raise_intent_current(job.generation) {
            CFRelease(element);
            CFRelease(raise_key);
            CFRelease(focused_key);
            if let Some(minimized_key) = minimized_key {
                CFRelease(minimized_key);
            }
            CFRelease(app);
            return;
        }
        // Unminimize is an AX mutation and is performed by the main-thread queue below.
        let minimized_set_err: Option<AXError> = None;
        let (raise_first_err, focused_set_err, raise_retry_err) = raise_ax_element(
            job.pid,
            job.cgwid,
            app,
            element,
            focused_key,
            raise_key,
            minimized_key,
            force_ax_focus,
            job.generation,
        );
        let effective_raise_err = raise_retry_err.unwrap_or(raise_first_err);
        if effective_raise_err != K_AX_INVALID_UI_ELEMENT {
            log_debug!(
                "[raise] ax raised cached: pid={} cgwid={} known_minimized={} set_minimized={:?} raise_first={} set_focused={:?} raise_retry={:?} app_create_us={} waited={}ms total={}ms",
                job.pid,
                job.cgwid,
                job.minimized,
                minimized_set_err,
                raise_first_err,
                focused_set_err,
                raise_retry_err,
                app_create_us,
                job.enqueued_at.elapsed().as_millis(),
                started.elapsed().as_millis()
            );
            CFRelease(element);
            CFRelease(raise_key);
            CFRelease(focused_key);
            if let Some(minimized_key) = minimized_key {
                CFRelease(minimized_key);
            }
            CFRelease(app);
            return;
        }
        log_debug!(
            "[raise] cached AX element stale: pid={} cgwid={} raise={} — refreshing",
            job.pid,
            job.cgwid,
            effective_raise_err
        );
        invalidate_cached_ax_window_element(job.pid, process_start_time_us, job.cgwid, element);
        CFRelease(element);
    }

    let windows_key = cf_string_new("AXWindows");
    let mut windows_array: *const c_void = std::ptr::null();
    let err = AXUIElementCopyAttributeValue(app, windows_key, &mut windows_array);
    CFRelease(windows_key);
    if err != K_AX_SUCCESS || windows_array.is_null() {
        CFRelease(raise_key);
        CFRelease(focused_key);
        if let Some(minimized_key) = minimized_key {
            CFRelease(minimized_key);
        }
        CFRelease(app);
        log_info!(
            "[raise] ax NO MATCH: pid={} cgwid={} ax_query_err={} waited={}ms",
            job.pid,
            job.cgwid,
            err,
            job.enqueued_at.elapsed().as_millis()
        );
        return;
    }
    let count = CFArrayGetCount(windows_array);
    let mut matched = false;
    let mut superseded = false;
    for i in 0..count {
        let element = CFArrayGetValueAtIndex(windows_array, i);
        if element.is_null() {
            continue;
        }
        // 同采集路径:窗口元素不继承 app 元素超时,逐个设,否则匹配遍历会在无响应 App 上
        // 每次读属性都等满系统默认超时。
        // Same as the collection path: window elements do not inherit the app element's timeout,
        // so set it per element or the match loop waits out the system default on each attribute
        // read against an unresponsive app.
        AXUIElementSetMessagingTimeout(element, AX_RAISE_MESSAGING_TIMEOUT);
        if ax_window_cgwid(element) == Some(job.cgwid) {
            // 应用变更前的最后一道 supersede 闸:枚举期间若来了更新的切换,本任务整体
            // 放弃(枚举结果作废),由新任务重新执行。
            // Final supersede gate right before applying: if a newer switch arrived during
            // enumeration, drop this job entirely (stale match) and let the new one run.
            if !raise_intent_current(job.generation) {
                superseded = true;
                log_debug!(
                    "[raise] ax superseded before apply: pid={} cgwid={} gen={}",
                    job.pid,
                    job.cgwid,
                    job.generation
                );
                break;
            }
            // 找到实时元素后更新缓存,下一次切换就不必重新枚举。
            // Refresh the cache with the live element so the next switch skips enumeration.
            cache_ax_window_element(job.pid, process_start_time_us, job.cgwid, element);
            // Unminimize is an AX mutation and is performed by the main-thread queue below.
            let minimized_set_err: Option<AXError> = None;
            let (raise_first_err, focused_set_err, raise_retry_err) = raise_ax_element(
                job.pid,
                job.cgwid,
                app,
                element,
                focused_key,
                raise_key,
                minimized_key,
                force_ax_focus,
                job.generation,
            );
            matched = true;
            log_debug!(
                "[raise] ax raised refreshed: pid={} cgwid={} ax_windows={} known_minimized={} set_minimized={:?} raise_first={} set_focused={:?} raise_retry={:?} app_create_us={} waited={}ms total={}ms",
                job.pid,
                job.cgwid,
                count,
                job.minimized,
                minimized_set_err,
                raise_first_err,
                focused_set_err,
                raise_retry_err,
                app_create_us,
                job.enqueued_at.elapsed().as_millis(),
                started.elapsed().as_millis()
            );
            break;
        }
    }
    if !matched && !superseded {
        log_info!(
            "[raise] ax NO MATCH: pid={} cgwid={} ax_windows={} waited={}ms total={}ms",
            job.pid,
            job.cgwid,
            count,
            job.enqueued_at.elapsed().as_millis(),
            started.elapsed().as_millis()
        );
    }
    CFRelease(raise_key);
    CFRelease(focused_key);
    if let Some(minimized_key) = minimized_key {
        CFRelease(minimized_key);
    }
    CFRelease(windows_array);
    CFRelease(app);
}

/// 普通路径只执行一次 AXRaise。仅当它明确失败且不是 stale element 时,才设置
/// AXFocusedWindow 并重试;成功路径不再支付额外 AX IPC。
/// The normal path performs one AXRaise. Only an explicit non-stale failure sets
/// AXFocusedWindow and retries; a successful raise pays no extra AX IPC.
#[allow(clippy::too_many_arguments)]
unsafe fn raise_ax_element(
    pid: i32,
    cgwid: u32,
    app: AXUIElementRef,
    element: AXUIElementRef,
    focused_key: AXUIElementRef,
    raise_key: AXUIElementRef,
    minimized_key: Option<AXUIElementRef>,
    force_focus: bool,
    generation: u64,
) -> (AXError, Option<AXError>, Option<AXError>) {
    log_debug!(
        "[raise] AXRaise queued for main thread: pid={} cgwid={} force_focus={}",
        pid,
        cgwid,
        force_focus
    );
    // AXRaise 打的是窗口元素,而 app 元素上的超时不会传给它(见 AX_WINDOW_MESSAGING_TIMEOUT 注释)。
    // 下面这些 AX 调用都在主线程执行,漏设超时就会按系统默认值(约 1.5s)冻结界面。
    // AXRaise targets the window element, which does not inherit the app element's timeout
    // (see the AX_WINDOW_MESSAGING_TIMEOUT note). Every AX call below runs on the main thread, so
    // a missing timeout freezes the UI for the system default (~1.5s).
    AXUIElementSetMessagingTimeout(element, AX_RAISE_MESSAGING_TIMEOUT);
    AXUIElementSetMessagingTimeout(app, AX_RAISE_MESSAGING_TIMEOUT);
    enqueue_main_thread_ax_raise(
        pid,
        cgwid,
        app,
        element,
        focused_key,
        raise_key,
        minimized_key,
        force_focus,
        generation,
    );
    (K_AX_SUCCESS, None, None)
}

/// 查一个 PID 的 AX 标准窗口列表。
/// 返回 None = AX 查询失败(该 App 无 AX 数据,可走 CG 回退);
/// Some(vec) = 查询成功,subrole 过滤后可能为空(无标准窗口 = 调度中心不显示它,直接跳过)。
///
/// Query an app's AX standard-window list.
/// None = AX query failed (app has no AX data; CG fallback allowed);
/// Some(vec) = query succeeded, possibly empty after subrole filtering (no standard windows =
/// Mission Control won't show it; skip entirely).
/// AX 窗口角色白名单:标准窗口(AXStandardWindow)任意;对话框(AXDialog——
/// JetBrains 系 IDE 的主窗口角色)必须有非空标题;部分 App(如 Xcode)的普通窗口
/// 报告 AXUnknown,只有同时具备 AXWindow role 和非空标题才放行;其余角色(弹窗/面板/
/// 隐形窗口)一律过滤;无 subrole 视为标准窗口。纯函数,单测覆盖。
/// AX-window subrole keep-rule: standard windows always pass; AXDialog (the subrole
/// JetBrains IDEs use for their MAIN windows) only when titled; some apps (such as Xcode)
/// report ordinary windows as AXUnknown, so allow that only with AXWindow role and a non-empty
/// title. Everything else (popups/panels/invisible windows) is filtered; a missing subrole
/// counts as standard. Pure function, unit-tested.
pub(super) fn ax_subrole_kept(subrole: Option<&str>, role: Option<&str>, titled: bool) -> bool {
    match subrole {
        Some("AXStandardWindow") => true,
        Some("AXDialog") => titled,
        Some("AXUnknown") => role == Some("AXWindow") && titled,
        Some(_) => false,
        // 无 subrole → 视为标准窗口(部分 App 不设置此属性)。
        // A missing subrole counts as standard (some apps don't set it).
        None => true,
    }
}

// Match AltTab's substantial custom-window boundary. Standard windows and titled dialogs do
// not use this size gate; it only prevents an untitled/unknown custom root from becoming a
// switch destination when it is merely a tiny auxiliary surface.
// 与 AltTab 的自定义窗口边界保持一致。标准窗口和有标题的对话框不走此尺寸门槛；仅防止
// AXUnknown 自定义根元素在很小时被当成可切换窗口。
const CUSTOM_WINDOW_MIN_WIDTH: f64 = 100.0;
const CUSTOM_WINDOW_MIN_HEIGHT: f64 = 50.0;

pub(super) fn admissible_window_placement(layer: i32, is_main: bool, is_fullscreen: bool) -> bool {
    layer == 0 || is_main || is_fullscreen
}

pub(super) fn custom_window_is_substantial(bounds: (f64, f64, f64, f64)) -> bool {
    bounds.2 >= CUSTOM_WINDOW_MIN_WIDTH && bounds.3 >= CUSTOM_WINDOW_MIN_HEIGHT
}

pub(super) fn is_attached_surface(parent_id: Option<u32>) -> bool {
    parent_id.is_some_and(|parent| parent != 0)
}

/// Decide whether an AX-only window may be backfilled into the switcher.
/// A missing CG entry means an orderOut'd window, which AX can legitimately recover; a known
/// non-zero CG layer means an app-owned overlay/menu and must stay out of the window switcher.
///
/// 判断 AX-only 窗口是否可以补回切换器。CG 中完全没有对应项表示 orderOut 的窗口,AX
/// 仍可能合法地补回;但如果 CG 已知该窗口处于非 0 层,它就是应用浮层/菜单,不能进入切换器。
pub(super) fn should_backfill_ax_window(cg_layer: Option<i32>) -> bool {
    match cg_layer {
        Some(layer) => layer == 0,
        None => true,
    }
}

pub(super) fn should_backfill_ax_window_for_process(
    pid: i32,
    cgwid: u32,
    process_start_time_us: Option<u64>,
    cg_layer: Option<i32>,
) -> bool {
    should_backfill_ax_window(cg_layer)
        && !is_known_non_normal_window(pid, process_start_time_us, cgwid)
}

pub(super) fn remember_non_normal_cg_windows(
    cg_window_layers: &HashMap<(i32, u32), i32>,
    identities: &HashMap<i32, AppIdentity>,
    ax_windows: &HashMap<i32, HashMap<u32, AxWindowInfo>>,
    parent_ids: &HashMap<u32, u32>,
) {
    for (&(pid, cgwid), &layer) in cg_window_layers {
        if is_attached_surface(parent_ids.get(&cgwid).copied()) {
            continue;
        }
        let is_main_or_fullscreen = ax_windows
            .get(&pid)
            .and_then(|windows| windows.get(&cgwid))
            .is_some_and(|window| window.is_main || window.is_fullscreen);
        if layer != 0 && !is_main_or_fullscreen {
            let process_start_time_us = identities
                .get(&pid)
                .and_then(|identity| identity.process_start_time_us);
            remember_non_normal_window(pid, process_start_time_us, cgwid);
        }
    }
}

pub(super) fn remember_non_normal_cg_windows_for_process(
    pid: i32,
    process_start_time_us: Option<u64>,
    cg_window_layers: &HashMap<u32, i32>,
    ax_windows: &HashMap<u32, AxWindowInfo>,
    parent_ids: &HashMap<u32, u32>,
) {
    for (&cgwid, &layer) in cg_window_layers {
        if is_attached_surface(parent_ids.get(&cgwid).copied()) {
            continue;
        }
        let is_main_or_fullscreen = ax_windows
            .get(&cgwid)
            .is_some_and(|window| window.is_main || window.is_fullscreen);
        if layer != 0 && !is_main_or_fullscreen {
            remember_non_normal_window(pid, process_start_time_us, cgwid);
        }
    }
}

/// 查某 PID 的全部标准 AX 窗口:(cgwid, 标题, 是否最小化)。collect_windows 与
/// 缩略图模块的启动预生成共用。
/// All standard AX windows for a PID: (cgwid, title, minimized). Shared between
/// collect_windows and the thumbnail module's startup pre-generation.
pub(crate) fn get_ax_windows_for_pid(pid: i32) -> Option<Vec<(u32, String, bool)>> {
    let process_start_time_us = unsafe { resolve_app_identity(pid).process_start_time_us };
    get_ax_windows_for_pid_with_identity(pid, process_start_time_us).map(|windows| {
        windows
            .into_iter()
            .map(|window| (window.cgwid, window.title, window.minimized))
            .collect()
    })
}

pub(super) fn get_ax_windows_for_pid_with_identity(
    pid: i32,
    process_start_time_us: Option<u64>,
) -> Option<Vec<AxWindowInfo>> {
    if let Some(windows) = cached_ax_snapshot(pid, process_start_time_us) {
        return Some(windows);
    }

    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return None;
        }

        // 设 50ms 消息超时:慢/无响应 App 的 AX 查询会快速失败,而不是卡默认 10s 超时
        // (后者会让该 App 整体走 CG 回退,混入隐形窗口)。
        // Set a 50ms messaging timeout: AX queries on slow/unresponsive apps fail fast instead
        // of hitting the default 10s timeout (which would push the app to the CG fallback path
        // and let invisible windows through).
        AXUIElementSetMessagingTimeout(app, 0.05);

        let windows_key = cf_string_new("AXWindows");
        let mut windows_array: *const c_void = std::ptr::null();
        let err = AXUIElementCopyAttributeValue(app, windows_key, &mut windows_array);
        CFRelease(windows_key);
        CFRelease(app);
        if err != K_AX_SUCCESS || windows_array.is_null() {
            return None;
        }

        let count = CFArrayGetCount(windows_array);
        let title_key = cf_string_new("AXTitle");
        let role_key = cf_string_new("AXRole");
        let subrole_key = cf_string_new("AXSubrole");
        let minimized_key = cf_string_new("AXMinimized");
        let main_key = cf_string_new("AXMain");
        let fullscreen_key = cf_string_new("AXFullScreen");
        let mut results = Vec::with_capacity(count as usize);

        for i in 0..count {
            let element = CFArrayGetValueAtIndex(windows_array, i);
            if element.is_null() {
                continue;
            }
            // app 元素上的 50ms 不会传给窗口元素;逐个设超时,否则无响应 App 的窗口查询会
            // 走系统默认值(约 1.5s),把整轮采集拖住。
            // The 50ms on the app element does not carry over to the window elements; set it per
            // element, or an unresponsive app's window queries fall back to the system default
            // (~1.5s) and stall the whole collection pass.
            AXUIElementSetMessagingTimeout(element, AX_WINDOW_MESSAGING_TIMEOUT);

            // 只保留标准窗口/有标题的对话框,以及 role=AXWindow 且有标题的 AXUnknown
            // 普通窗口(如 Xcode);过滤弹出面板/下拉菜单等非标准窗口。
            // Keep standard windows, titled dialogs, and titled AXUnknown elements whose
            // role is AXWindow (ordinary windows in apps such as Xcode); filter popups,
            // panels, and other non-standard elements.
            let mut subrole_value: *const c_void = std::ptr::null();
            let subrole = if AXUIElementCopyAttributeValue(element, subrole_key, &mut subrole_value)
                == K_AX_SUCCESS
                && !subrole_value.is_null()
            {
                let s = cf_to_rust_string(subrole_value);
                CFRelease(subrole_value);
                s
            } else {
                None
            };
            let kept = if subrole.is_some() {
                let mut role_value: *const c_void = std::ptr::null();
                let role = if AXUIElementCopyAttributeValue(element, role_key, &mut role_value)
                    == K_AX_SUCCESS
                    && !role_value.is_null()
                {
                    let role = cf_to_rust_string(role_value);
                    CFRelease(role_value);
                    role
                } else {
                    None
                };
                // AXDialog/AXUnknown 需额外判断标题;无标题元素按弹出/隐形窗口过滤。
                // AXDialog/AXUnknown require a non-empty title; untitled elements stay
                // filtered as popups/invisible windows.
                let titled = if matches!(subrole.as_deref(), Some("AXDialog") | Some("AXUnknown")) {
                    let mut title_value: *const c_void = std::ptr::null();
                    if AXUIElementCopyAttributeValue(element, title_key, &mut title_value)
                        == K_AX_SUCCESS
                        && !title_value.is_null()
                    {
                        let t = cf_to_rust_string(title_value);
                        CFRelease(title_value);
                        t.is_some_and(|t| !t.is_empty())
                    } else {
                        false
                    }
                } else {
                    false
                };
                ax_subrole_kept(subrole.as_deref(), role.as_deref(), titled)
            } else {
                // 无 subrole → 视为标准窗口(部分 App 不设置此属性)。
                // No subrole means standard window for apps that don't set it.
                true
            };
            if !kept {
                continue;
            }

            let mut title_value: *const c_void = std::ptr::null();
            let title = if AXUIElementCopyAttributeValue(element, title_key, &mut title_value)
                == K_AX_SUCCESS
                && !title_value.is_null()
            {
                let t = cf_to_rust_string(title_value);
                CFRelease(title_value);
                t.unwrap_or_default()
            } else {
                String::new()
            };
            // AXMinimized:窗口是否最小化。无此属性(部分 App)按 false 处理。
            // AXMinimized: whether the window is minimized. Absent attribute (some apps) -> false.
            let minimized = {
                let mut min_value: *const c_void = std::ptr::null();
                if AXUIElementCopyAttributeValue(element, minimized_key, &mut min_value)
                    == K_AX_SUCCESS
                    && !min_value.is_null()
                {
                    let m = CFBooleanGetValue(min_value);
                    CFRelease(min_value);
                    m
                } else {
                    false
                }
            };
            let is_main = {
                let mut main_value: *const c_void = std::ptr::null();
                if AXUIElementCopyAttributeValue(element, main_key, &mut main_value) == K_AX_SUCCESS
                    && !main_value.is_null()
                {
                    let value = CFBooleanGetValue(main_value);
                    CFRelease(main_value);
                    value
                } else {
                    false
                }
            };
            let is_fullscreen = {
                let mut fullscreen_value: *const c_void = std::ptr::null();
                if AXUIElementCopyAttributeValue(element, fullscreen_key, &mut fullscreen_value)
                    == K_AX_SUCCESS
                    && !fullscreen_value.is_null()
                {
                    let value = CFBooleanGetValue(fullscreen_value);
                    CFRelease(fullscreen_value);
                    value
                } else {
                    false
                }
            };
            // 取该 AX 窗口的 CGWindowID（私有 API），用于和 CG 窗口精确配对。
            let cgwid = ax_window_cgwid(element).unwrap_or(0);
            // 保留精确元素供激活路径复用,这样正常切换不必再次读取 AXWindows。
            // Retain the exact element for the activation path so normal raises do not need
            // another AXWindows round trip.
            cache_ax_window_element(pid, process_start_time_us, cgwid, element);
            results.push(AxWindowInfo {
                cgwid,
                title,
                minimized,
                is_main,
                is_fullscreen,
                is_custom_root: subrole.as_deref() == Some("AXUnknown"),
            });
        }
        CFRelease(title_key);
        CFRelease(subrole_key);
        CFRelease(minimized_key);
        CFRelease(main_key);
        CFRelease(fullscreen_key);
        CFRelease(windows_array);
        cache_ax_snapshot(pid, process_start_time_us, &results);
        Some(results)
    }
}

/// CFString -> Rust String(None = 转换失败)。窗口控制模块复用。
/// CFString -> Rust String (None = conversion failed). Reused by window control.
pub(crate) fn cf_to_rust_string(cf_string: *const c_void) -> Option<String> {
    let mut buf = vec![0u8; 1024];
    let ok = unsafe {
        CFStringGetCString(
            cf_string,
            buf.as_mut_ptr() as *mut i8,
            buf.len() as isize,
            0x08000100,
        )
    };
    if ok {
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        Some(String::from_utf8_lossy(&buf[..end]).to_string())
    } else {
        None
    }
}
