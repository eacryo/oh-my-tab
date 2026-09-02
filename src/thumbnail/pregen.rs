//! 窗口缩略图的 AX 预生成子系统:常驻监视线程为每个运行中的 App 安装
//! AXObserver,监听 kAXWindowCreatedNotification,新窗口防抖 300ms 后把补拍任务
//! 投递回捕获管线(super::enqueue_job_for_generation);App 启动/退出经
//! app_launched/app_terminated 转发。
//!
//! The thumbnail module's AX pre-generation subsystem: a resident thread installs
//! an AXObserver per running app, watches kAXWindowCreatedNotification, and after a
//! 300ms debounce posts backfill jobs to the capture pipeline
//! (super::enqueue_job_for_generation). App launch/termination arrive through
//! app_launched/app_terminated.

use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::Duration;

use crate::event_tap::{
    kCFRunLoopDefaultMode, CFRunLoopAddSource, CFRunLoopGetCurrent, CFRunLoopRun,
};
use crate::log_debug;

use super::{
    capture_allowed, enqueue_job_for_generation, CFStringCompare, CapturePriority, ConstPtr,
    BASE_TARGET_PX_H, CACHE, CAPTURE_STATE, STARTUP_PREWARM_MAX,
};
use crate::ffi::{
    make_nsstring, AXObserverAddNotification, AXObserverCreate, AXObserverGetRunLoopSource,
    AXUIElementCreateApplication, AXUIElementGetPid, AxObserverRef, CFRelease,
    CFRunLoopRemoveSource, CFRunLoopSourceContext, CFRunLoopSourceCreate, CFRunLoopSourceSignal,
    CFRunLoopWakeUp,
};

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
struct InstalledMap(Mutex<HashMap<i32, (AxObserverRef, *mut c_void)>>);
unsafe impl Send for InstalledMap {}
unsafe impl Sync for InstalledMap {}
static INSTALLED: LazyLock<InstalledMap> =
    LazyLock::new(|| InstalledMap(Mutex::new(HashMap::new())));

// AX 与 CFRunLoop 的公开框架 extern 已统一到 ffi.rs(kAXWindowCreatedNotification
// 仍走下方运行时解析;CFRunLoopGetCurrent/Run/AddSource 复用 event_tap 的声明)。
// The public-framework AX and CFRunLoop externs now live in ffi.rs (kAXWindowCreated-
// Notification is still resolved at runtime below; CFRunLoopGetCurrent/Run/AddSource are
// reused from event_tap).

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
    let s = make_nsstring("AXWindowCreated");
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
