//! WindowServer 生命周期通知的最小适配层。
//! Minimal adapter for WindowServer lifecycle notifications.
//!
//! 这里只负责注册生命周期/聚焦通知和转发事件；窗口数据仍由现有快照收集器确认。
//! This layer only registers lifecycle/focus notifications and forwards events; the existing
//! snapshot collector remains authoritative for window data.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

use objc2::runtime::AnyObject;
use objc2::{msg_send, sel};

use crate::skylight;
use crate::{log_debug, log_info, CONTROLLER};

const WINDOW_CREATED: u32 = 811;
const WINDOW_DESTROYED: u32 = 804;
const WINDOW_FOCUSED: u32 = 808;

type NotifyCallback = unsafe extern "C" fn(
    event: u32,
    data: *const c_void,
    data_length: usize,
    context: *mut c_void,
    connection: i32,
);
type RegisterNotifyFn = unsafe extern "C" fn(
    connection: i32,
    callback: NotifyCallback,
    event: u32,
    context: *mut c_void,
) -> i32;
type RequestNotificationsFn =
    unsafe extern "C" fn(connection: i32, window_list: *mut u32, window_count: i32) -> i32;

#[derive(Clone, Copy, Debug)]
pub(crate) enum WindowServerEvent {
    Created,
    Destroyed(u32),
    Focused(u32),
}

struct ActivationState {
    activated_at: Instant,
    until: Instant,
    focus_bumped: bool,
    raise_tail: HashSet<u32>,
    own_target: Option<u32>,
}

#[derive(Clone, Copy)]
struct OwnFocusIntent {
    pid: i32,
    window_id: u32,
    at: Instant,
}

static STARTED: AtomicBool = AtomicBool::new(false);
static DELIVERY_SCHEDULED: AtomicBool = AtomicBool::new(false);
static SUBSCRIPTION_FAILURE_LOGGED: AtomicBool = AtomicBool::new(false);
static EVENT_TX: OnceLock<flume::Sender<WindowServerEvent>> = OnceLock::new();
static MAIN_EVENTS: LazyLock<Mutex<VecDeque<WindowServerEvent>>> =
    LazyLock::new(|| Mutex::new(VecDeque::new()));
static ACTIVATIONS: LazyLock<Mutex<HashMap<i32, ActivationState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static OWN_FOCUS_INTENT: LazyLock<Mutex<Option<OwnFocusIntent>>> =
    LazyLock::new(|| Mutex::new(None));
// 监听索引与显示列表分离:CG 里暂时未被 AX 展示的窗口仍需能反查 owner PID。
// Keep the observation index separate from the display list so CG windows temporarily omitted
// by AX can still be resolved back to their owner PID.
#[derive(Default)]
struct WindowRegistry {
    by_pid: HashMap<i32, HashSet<u32>>,
    by_window: HashMap<u32, i32>,
}

static WINDOW_REGISTRY: LazyLock<Mutex<WindowRegistry>> =
    LazyLock::new(|| Mutex::new(WindowRegistry::default()));

fn registry_from_subscriptions(subscriptions: &[(u32, i32)]) -> WindowRegistry {
    let mut by_window = HashMap::new();
    for &(window_id, pid) in subscriptions {
        if window_id != 0 && pid > 0 {
            by_window.entry(window_id).or_insert(pid);
        }
    }
    let mut registry = WindowRegistry {
        by_pid: HashMap::new(),
        by_window,
    };
    for (&window_id, &pid) in &registry.by_window {
        registry.by_pid.entry(pid).or_default().insert(window_id);
    }
    registry
}

// 连接 ID 由 skylight.rs 统一加载并缓存;本模块按接口签名需要 i32。
// The connection ID is loaded and cached centrally in skylight.rs; this module's
// interface signatures want it as i32.
static MAIN_CONNECTION: LazyLock<Option<i32>> =
    LazyLock::new(|| skylight::cgs_main_connection().map(|c| c as i32));

static REGISTER_NOTIFY: LazyLock<Option<RegisterNotifyFn>> = LazyLock::new(|| unsafe {
    skylight::load_private_symbol(skylight::SKYLIGHT_PATH, "SLSRegisterConnectionNotifyProc")
});

static REQUEST_NOTIFICATIONS: LazyLock<Option<RequestNotificationsFn>> = LazyLock::new(|| unsafe {
    skylight::load_private_symbol(skylight::SKYLIGHT_PATH, "SLSRequestNotificationsForWindows")
});

unsafe extern "C" fn window_server_callback(
    event: u32,
    data: *const c_void,
    data_length: usize,
    _context: *mut c_void,
    _connection: i32,
) {
    let Some(window_id) = read_window_id(data, data_length) else {
        return;
    };
    let event = match event {
        WINDOW_CREATED => WindowServerEvent::Created,
        WINDOW_DESTROYED => WindowServerEvent::Destroyed(window_id),
        WINDOW_FOCUSED => WindowServerEvent::Focused(window_id),
        _ => return,
    };
    if let Some(sender) = EVENT_TX.get() {
        let _ = sender.send(event);
    }
}

unsafe fn read_window_id(data: *const c_void, data_length: usize) -> Option<u32> {
    if data.is_null() || data_length < std::mem::size_of::<u32>() {
        return None;
    }
    Some(std::ptr::read_unaligned(data.cast::<u32>()))
}

pub(crate) fn start() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let Some(connection) = *MAIN_CONNECTION else {
        log_info!("WindowServer lifecycle notifications unavailable: no SkyLight connection");
        STARTED.store(false, Ordering::Release);
        return;
    };
    let Some(register) = *REGISTER_NOTIFY else {
        log_info!("WindowServer lifecycle notifications unavailable: missing registration symbol");
        STARTED.store(false, Ordering::Release);
        return;
    };
    let Some(request) = *REQUEST_NOTIFICATIONS else {
        log_info!("WindowServer lifecycle notifications unavailable: missing subscription symbol");
        STARTED.store(false, Ordering::Release);
        return;
    };

    let (sender, receiver) = flume::unbounded();
    let _ = EVENT_TX.set(sender);
    for event in [WINDOW_CREATED, WINDOW_DESTROYED, WINDOW_FOCUSED] {
        let result = unsafe {
            register(
                connection,
                window_server_callback,
                event,
                std::ptr::null_mut(),
            )
        };
        if result != 0 {
            log_info!(
                "WindowServer lifecycle registration failed: event={} status={}",
                event,
                result
            );
        }
    }

    std::thread::Builder::new()
        .name("window-server-events".into())
        .spawn(move || {
            crate::performance::set_current_thread_qos(crate::performance::ThreadQos::Utility);
            while let Ok(event) = receiver.recv() {
                MAIN_EVENTS.lock().unwrap().push_back(event);
                schedule_main_delivery();
            }
        })
        .expect("spawn window-server event bridge");

    // 保留函数指针的读取，确保缺少订阅符号时不会在运行中静默退化。
    // Keep the function pointer read explicit so a missing subscription symbol cannot silently
    // degrade after registration succeeds.
    let _ = request;
    log_debug!("WindowServer lifecycle notifications started");
}

/// 记录一次由切换器发起的目标窗口聚焦，避免后续 808 被误判成外部激活。
/// Record a switcher-initiated target focus so its following 808 is not mistaken for
/// an external activation.
pub(crate) fn note_own_focus(pid: i32, window_id: u32) {
    *OWN_FOCUS_INTENT.lock().unwrap() = Some(OwnFocusIntent {
        pid,
        window_id,
        at: Instant::now(),
    });
}

/// 为一次 App 激活建立焦点状态；激活产生的第一个 808 是真实焦点，后续快照内窗口视为 raise 尾部。
/// Start activation focus state: the first 808 is the real focus and later windows in the
/// activation snapshot are treated as the raise tail.
pub(crate) fn begin_activation(pid: i32, window_ids: &[u32], activated_at: Instant) {
    let own_target = {
        let mut intent = OWN_FOCUS_INTENT.lock().unwrap();
        let matches = intent
            .as_ref()
            .is_some_and(|value| value.pid == pid && value.at.elapsed() < Duration::from_secs(1));
        if matches {
            intent.take().map(|value| value.window_id)
        } else {
            *intent = None;
            None
        }
    };
    let raise_tail = if own_target.is_some() {
        HashSet::new()
    } else {
        window_ids.iter().copied().collect()
    };
    ACTIVATIONS.lock().unwrap().insert(
        pid,
        ActivationState {
            activated_at,
            until: Instant::now() + Duration::from_millis(500),
            focus_bumped: own_target.is_some(),
            raise_tail,
            own_target,
        },
    );
}

pub(crate) fn activation_token(pid: i32) -> Option<Instant> {
    ACTIVATIONS
        .lock()
        .unwrap()
        .get(&pid)
        .map(|state| state.activated_at)
}

/// 判断 WindowServer 的 808 是否应提升 MRU；这是主线程上的状态转移入口。
/// Decide whether a WindowServer 808 should bump MRU; this is the main-thread state transition.
pub(crate) fn focus_should_bump(pid: i32, window_id: u32) -> bool {
    let now = Instant::now();
    {
        let mut intent = OWN_FOCUS_INTENT.lock().unwrap();
        if let Some(value) = *intent {
            if value.at.elapsed() >= Duration::from_secs(1) {
                *intent = None;
            } else if value.pid == pid && value.window_id == window_id {
                *intent = None;
                return false;
            }
        }
    }
    let mut activations = ACTIVATIONS.lock().unwrap();
    let Some(state) = activations.get_mut(&pid) else {
        return true;
    };
    if state.until <= now {
        activations.remove(&pid);
        return true;
    }
    if state.own_target == Some(window_id) {
        state.own_target = None;
        state.focus_bumped = true;
        return false;
    }
    if !state.focus_bumped {
        state.focus_bumped = true;
        return true;
    }
    if state.raise_tail.remove(&window_id) {
        return false;
    }
    true
}

/// AX focused-window 查询只在 808 尚未到达时作为 backstop。
/// The AX focused-window query is a backstop only until a real 808 arrives.
pub(crate) fn ax_focus_backstop_allowed(pid: i32) -> bool {
    let now = Instant::now();
    let mut activations = ACTIVATIONS.lock().unwrap();
    let Some(state) = activations.get_mut(&pid) else {
        return true;
    };
    if state.until <= now {
        activations.remove(&pid);
        return true;
    }
    if state.focus_bumped {
        false
    } else {
        state.focus_bumped = true;
        true
    }
}

/// 更新 WindowServer 的监听集合，同时保存 PID -> 多窗口和 CGWindowID -> PID 索引。
/// Update WindowServer subscriptions and retain both PID -> many windows and CGWindowID -> PID
/// indexes.
pub(crate) fn update_subscriptions(subscriptions: &[(u32, i32)]) {
    let registry = registry_from_subscriptions(subscriptions);
    let mut ids: Vec<u32> = registry.by_window.keys().copied().collect();
    *WINDOW_REGISTRY.lock().unwrap() = registry;

    let Some(connection) = *MAIN_CONNECTION else {
        return;
    };
    let Some(request) = *REQUEST_NOTIFICATIONS else {
        return;
    };
    ids.sort_unstable();
    let (window_ptr, window_count) = if ids.is_empty() {
        (std::ptr::null_mut(), 0)
    } else {
        (ids.as_mut_ptr(), ids.len() as i32)
    };
    let result = unsafe { request(connection, window_ptr, window_count) };
    if result != 0 {
        if !SUBSCRIPTION_FAILURE_LOGGED.swap(true, Ordering::Relaxed) {
            log_info!(
                "WindowServer lifecycle subscription update failed: windows={} status={}",
                ids.len(),
                result
            );
        }
    } else {
        SUBSCRIPTION_FAILURE_LOGGED.store(false, Ordering::Relaxed);
    }
}

pub(crate) fn owner_for_window(window_id: u32) -> Option<i32> {
    WINDOW_REGISTRY
        .lock()
        .unwrap()
        .by_window
        .get(&window_id)
        .copied()
}

pub(crate) fn window_ids_for_pid(pid: i32) -> Vec<u32> {
    let mut ids: Vec<u32> = WINDOW_REGISTRY
        .lock()
        .unwrap()
        .by_pid
        .get(&pid)
        .into_iter()
        .flat_map(|ids| ids.iter().copied())
        .collect();
    ids.sort_unstable();
    ids
}

pub(crate) fn drain_main() -> Vec<WindowServerEvent> {
    let mut events = MAIN_EVENTS.lock().unwrap();
    let drained = events.drain(..).collect::<Vec<_>>();
    DELIVERY_SCHEDULED.store(false, Ordering::Release);
    if !events.is_empty() {
        drop(events);
        schedule_main_delivery();
    }
    drained
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_supports_many_windows_for_one_pid() {
        let registry = registry_from_subscriptions(&[(0x1001, 10), (0x1002, 10), (0x2001, 20)]);

        assert_eq!(registry.by_window.get(&0x1001), Some(&10));
        assert_eq!(registry.by_window.get(&0x1002), Some(&10));
        assert_eq!(registry.by_pid.get(&10).map(HashSet::len), Some(2));
        assert_eq!(registry.by_window.get(&0x9999), None);
    }

    #[test]
    fn external_activation_bumps_first_focus_and_ignores_raise_tail() {
        let pid = i32::MAX - 101;
        let first = 0x1001;
        let sibling = 0x1002;
        ACTIVATIONS.lock().unwrap().remove(&pid);
        OWN_FOCUS_INTENT.lock().unwrap().take();

        begin_activation(pid, &[first, sibling], Instant::now());
        assert!(focus_should_bump(pid, first));
        assert!(!focus_should_bump(pid, sibling));

        ACTIVATIONS.lock().unwrap().remove(&pid);
    }

    #[test]
    fn own_focus_is_not_counted_as_external_mru_bump() {
        let pid = i32::MAX - 102;
        let target = 0x2001;
        ACTIVATIONS.lock().unwrap().remove(&pid);
        OWN_FOCUS_INTENT.lock().unwrap().take();

        note_own_focus(pid, target);
        begin_activation(pid, &[target], Instant::now());
        assert!(!focus_should_bump(pid, target));

        ACTIVATIONS.lock().unwrap().remove(&pid);
        OWN_FOCUS_INTENT.lock().unwrap().take();
    }

    #[test]
    fn ax_backstop_consumes_the_first_focus_slot() {
        let pid = i32::MAX - 103;
        let focused = 0x3001;
        ACTIVATIONS.lock().unwrap().remove(&pid);
        OWN_FOCUS_INTENT.lock().unwrap().take();

        begin_activation(pid, &[focused], Instant::now());
        assert!(ax_focus_backstop_allowed(pid));
        assert!(!focus_should_bump(pid, focused));

        ACTIVATIONS.lock().unwrap().remove(&pid);
    }
}

fn schedule_main_delivery() {
    if DELIVERY_SCHEDULED.swap(true, Ordering::AcqRel) {
        return;
    }
    let Some(controller) = CONTROLLER.lock().unwrap().map(|ptr| ptr.0) else {
        DELIVERY_SCHEDULED.store(false, Ordering::Release);
        return;
    };
    unsafe {
        let _: () = msg_send![controller,
            performSelectorOnMainThread: sel!(handleWindowServerEvent:),
            withObject: std::ptr::null::<AnyObject>(),
            waitUntilDone: false
        ];
    }
}
