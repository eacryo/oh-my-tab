//! 快捷操作模块:Option+I 打开设置、Option+E 打开访达、Option+D 显示桌面。
//! 独立 session 层 event tap(专用线程)拦截 Option+字母,事件经既有 bridge
//! (GlobalEvent -> performSelectorOnMainThread)投递到主线程执行动作。
//! 结构与 window_management.rs 相同:专用线程 + RunLoop 引用 + 停止标志。
//!
//! Quick-actions module: Option+I opens Settings, Option+E opens Finder, Option+D shows the
//! desktop. A dedicated session-level event tap (own thread) intercepts Option+letters; events
//! travel through the existing bridge (GlobalEvent -> performSelectorOnMainThread) and run on
//! the main thread. Same shape as window_management.rs: dedicated thread + RunLoop reference +
//! stop flag.

use objc2::runtime::AnyObject;
use objc2::{class, msg_send};

use crate::event_monitor::GlobalEvent;
use crate::event_tap::{
    self, tap_location, tap_options, tap_placement, CFRunLoopGetCurrent, CFRunLoopRef,
    CGEventCreateKeyboardEvent, CGEventFlags, CGEventGetFlags, CGEventGetIntegerValueField,
    CGEventMask, CGEventRef, CGEventSetFlags, CGEventTapProxy, CGEventType,
    K_CG_EVENT_SOURCE_USER_DATA, SYNTHETIC_MARKER,
};
use crate::ffi::{make_nsstring, CFRelease};
use crate::{log_debug, log_info};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;

// ========== 键盘事件常量 / keyboard event constants ==========
// 键码来自 Carbon HIToolbox Events.h(kVK_ANSI_I/E/D)。
// Keycodes are from Carbon HIToolbox Events.h (kVK_ANSI_I/E/D).
const K_CG_EVENT_KEY_DOWN: CGEventType = 10;
const K_CG_EVENT_KEY_UP: CGEventType = 11;
const K_CG_KEYBOARD_EVENT_KEYCODE: i32 = 9;
const K_CG_KEYBOARD_EVENT_AUTOREPEAT: i32 = 8;
const K_VK_I: u16 = 34;
const K_VK_E: u16 = 14;
const K_VK_D: u16 = 2;
// 修饰键位掩码:必须恰好是 Option(带其他修饰键的组合透传,与 Option+方向键同规则)。
// Modifier masks: exactly Option is required; combos with extra modifiers pass through
// (same rule as Option+arrows).
const K_FLAG_OPTION: CGEventFlags = 0x00080000;
const K_FLAG_COMMAND: CGEventFlags = 0x00100000;
const K_FLAG_SHIFT: CGEventFlags = 0x00020000;
const K_FLAG_CONTROL: CGEventFlags = 0x00040000;

/// 快捷动作。数值顺序经 NSNumber 跨线程传递(bridge -> 主线程),只能追加不能重排。
/// Quick actions. The numeric order crosses threads via NSNumber (bridge -> main thread);
/// append-only, never reorder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuickAction {
    OpenSettings = 0,
    OpenFinder = 1,
    ShowDesktop = 2,
}

impl QuickAction {
    /// 从 bridge 传来的整数还原动作(未知值静默丢弃)。
    /// Rebuild an action from the bridge integer (unknown values are dropped).
    pub(crate) fn from_isize(v: isize) -> Option<Self> {
        match v {
            0 => Some(Self::OpenSettings),
            1 => Some(Self::OpenFinder),
            2 => Some(Self::ShowDesktop),
            _ => None,
        }
    }

    fn from_keycode(code: u16) -> Option<Self> {
        match code {
            K_VK_I => Some(Self::OpenSettings),
            K_VK_E => Some(Self::OpenFinder),
            K_VK_D => Some(Self::ShowDesktop),
            _ => None,
        }
    }
}

/// 动作是否被配置启用(总开关 + 该动作的独立开关)。
/// Whether an action is enabled by config (master switch + the action's own switch).
fn action_enabled(action: QuickAction) -> bool {
    crate::config::CONFIG
        .read()
        .map(|c| {
            c.quick_actions.enabled
                && match action {
                    QuickAction::OpenSettings => c.quick_actions.open_settings,
                    QuickAction::OpenFinder => c.quick_actions.open_finder,
                    QuickAction::ShowDesktop => c.quick_actions.show_desktop,
                }
        })
        .unwrap_or(false)
}

/// tap 回调:只关心 Option+I/E/D(不带其他修饰键)。启用时吞掉 keyDown/keyUp 并把非
/// 自动重复的 keyDown 投递给主线程;关闭时全部透传(功能关闭 = 组合键还给系统)。
/// 自己是前台 App 时也透传,设置窗口文本框不受影响。
///
/// The tap callback: only cares about Option+I/E/D (no extra modifiers). When enabled it
/// swallows matching keyDown/keyUp and forwards non-autorepeat keyDowns to the main thread;
/// when disabled everything passes through (a disabled feature returns the combo to the
/// system). Also passes through when we are the frontmost app, keeping our own settings
/// text fields intact.
unsafe extern "C" fn quick_actions_tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    _user_info: *mut c_void,
) -> CGEventRef {
    if event_type != K_CG_EVENT_KEY_DOWN && event_type != K_CG_EVENT_KEY_UP {
        return event;
    }
    let keycode = CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_KEYCODE) as u16;
    let Some(action) = QuickAction::from_keycode(keycode) else {
        return event;
    };
    let flags = CGEventGetFlags(event);
    if flags & K_FLAG_OPTION == 0 || flags & (K_FLAG_COMMAND | K_FLAG_SHIFT | K_FLAG_CONTROL) != 0 {
        return event;
    }
    // 本应用合成的组合键(鼠标映射 Key Press post 到 HID 层后会回到 session tap):
    // 必须透传,否则映射了 Option+字母的侧键会被这里劫持。
    // Our own synthesized combos (mouse Key Press mappings post at HID level and loop back
    // into session taps) must pass through, or a side button mapped to Option+letter gets
    // hijacked here.
    if CGEventGetIntegerValueField(event, K_CG_EVENT_SOURCE_USER_DATA) == SYNTHETIC_MARKER {
        return event;
    }
    if !action_enabled(action) {
        return event;
    }
    let (_name, pid) = crate::ffi::frontmost_app_info();
    if pid == std::process::id() as i32 {
        return event;
    }
    if event_type == K_CG_EVENT_KEY_DOWN {
        // 忽略系统自动重复:动作是幂等的一次性触发,按住不放只应触发一次。
        // Ignore system autorepeat: the actions are idempotent one-shots; holding the key
        // should fire once.
        let autorepeat = CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_AUTOREPEAT);
        if autorepeat == 0 {
            log_debug!("[quick] keyDown Option+{:?}", action);
            if let Some(tx) = crate::STATUS_EVENT_TX.get() {
                let _ = tx.send(GlobalEvent::QuickAction(action as u8));
            } else {
                log_info!(
                    "[quick] keyDown Option+{:?} dropped: event bridge unavailable",
                    action
                );
            }
        }
    }
    // 吞掉匹配的 keyDown/keyUp(含自动重复),应用看不到这组组合键。
    // Swallow matching keyDown/keyUp (autorepeat included); apps never see the combo.
    std::ptr::null_mut()
}

/// 运行时启用快捷操作(设置页热切换 / 启动路径共用)。幂等。
/// Enable quick actions at runtime (shared by the settings hot-switch and the startup path).
/// Idempotent.
pub(crate) fn start() {
    let mut guard = QA_THREAD.lock().unwrap();
    if guard.as_ref().is_some_and(|h| !h.is_finished()) {
        return;
    }
    *guard = Some(spawn_tap_thread());
    log_info!("Quick actions enabled.");
}

/// 运行时停用快捷操作(设置页热切换)。幂等。
/// Disable quick actions at runtime (settings hot-switch). Idempotent.
pub(crate) fn stop() {
    STOP_REQUESTED.store(true, Ordering::Relaxed);
    let rl = runloop_static().lock().unwrap().take();
    if let Some(rl) = rl {
        unsafe {
            event_tap::CFRunLoopStop(rl);
        }
    }
    let handle = QA_THREAD.lock().unwrap().take();
    if let Some(h) = handle {
        let _ = h.join();
    }
    log_info!("Quick actions disabled.");
}

struct RunLoopMutex(Mutex<Option<CFRunLoopRef>>);
unsafe impl Send for RunLoopMutex {}
unsafe impl Sync for RunLoopMutex {}
static RUNLOOP: OnceLock<RunLoopMutex> = OnceLock::new();
static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);
static QA_THREAD: Mutex<Option<thread::JoinHandle<()>>> = Mutex::new(None);

fn runloop_static() -> &'static Mutex<Option<CFRunLoopRef>> {
    &RUNLOOP.get_or_init(|| RunLoopMutex(Mutex::new(None))).0
}

fn spawn_tap_thread() -> thread::JoinHandle<()> {
    // 监听掩码:keyDown + keyUp。
    // Listen mask: keyDown + keyUp.
    let mask: CGEventMask = (1u64 << K_CG_EVENT_KEY_DOWN) | (1u64 << K_CG_EVENT_KEY_UP);
    thread::spawn(move || unsafe {
        crate::performance::set_current_thread_qos(crate::performance::ThreadQos::UserInteractive);
        // 新线程首件事:清掉上次运行残留的停止标志。
        // First thing in the new thread: clear any stale stop flag.
        STOP_REQUESTED.store(false, Ordering::Relaxed);
        // session 层 tap:与切换器同层,能拦截真实硬件按键;DEFAULT_TAP 才能吞事件。
        // Session-level tap: same layer as the switcher, sees real hardware keys; DEFAULT_TAP
        // is required to swallow events.
        let tap = event_tap::create_tap_with_retry(
            tap_location::SESSION_EVENT_TAP,
            tap_placement::HEAD_INSERT,
            tap_options::DEFAULT_TAP,
            mask,
            Some(quick_actions_tap_callback),
            std::ptr::null_mut(),
            "quick",
            Some(&STOP_REQUESTED),
        );
        let tap = match tap {
            Some(t) => t,
            None => return,
        };
        let rl = CFRunLoopGetCurrent();
        let source = event_tap::CFMachPortCreateRunLoopSource(std::ptr::null_mut(), tap, 0);
        event_tap::CFRunLoopAddSource(rl, source, event_tap::kCFRunLoopDefaultMode);
        event_tap::CGEventTapEnable(tap, true);
        // 存入 RunLoop 后复查停止标志,关闭“存入后、run 前置位”的竞态窗口。
        // Re-check the stop flag after storing the RunLoop to close the store-vs-run race.
        *runloop_static().lock().unwrap() = Some(rl);
        if !STOP_REQUESTED.load(Ordering::Relaxed) {
            log_info!("Quick actions event tap started.");
            event_tap::CFRunLoopRun();
        }
        *runloop_static().lock().unwrap() = None;
    })
}

/// 主线程:执行一个快捷动作(bridge 投递过来)。
/// Main thread: run one quick action (delivered by the bridge).
pub(crate) fn apply_action(action: QuickAction) {
    // 事件可能排队到功能关闭之后才被主线程执行,先复核开关。
    // The event may land on the main thread after the feature was switched off; re-check.
    if !action_enabled(action) {
        return;
    }
    match action {
        QuickAction::OpenSettings => {
            // 打开系统设置(x-apple.systempreferences: URL scheme 由系统设置注册,
            // openURL: 会拉起/置前系统设置)。「打开设置」指系统设置,不是本应用的设置窗口。
            // Open System Settings (the x-apple.systempreferences: URL scheme is registered by
            // System Settings; openURL: launches or raises it). "Open Settings" refers to the
            // system's settings, not this app's window.
            unsafe { open_system_settings() };
        }
        QuickAction::OpenFinder => {
            unsafe { open_new_finder_window() };
        }
        QuickAction::ShowDesktop => {
            // 与鼠标系统动作同路径:Dock 通知触发系统「显示桌面」。
            // Same path as the mouse system action: the Dock notification triggers the
            // system's Show Desktop.
            crate::mouse::system_action::fire("com.apple.showdesktop.awake");
        }
    }
}

/// 打开系统设置:x-apple.systempreferences: URL scheme,LaunchServices 拉起/置前系统设置。
/// Open System Settings via the x-apple.systempreferences: URL scheme; LaunchServices
/// launches or raises System Settings.
unsafe fn open_system_settings() {
    let url_str = make_nsstring("x-apple.systempreferences:");
    // URLWithString: 返回自动释放的 NSURL(+0),不归调用者所有,不要 CFRelease。
    // URLWithString: returns an autoreleased NSURL (+0) we do not own; never CFRelease it.
    let url: *mut AnyObject = msg_send![class!(NSURL), URLWithString: url_str];
    CFRelease(url_str as *const c_void);
    let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
    // openURL: 返回 BOOL;objc2 在 debug 下校验返回类型编码,必须用 bool 接收。
    // openURL: returns BOOL; objc2 validates the return encoding, so receive it as bool.
    let opened: bool = msg_send![workspace, openURL: url];
    log_debug!("[quick] System Settings opened: {}", opened);
}

/// 打开一个「新的」访达窗口并最大化(Win+E 语义:每次都新开,而非把旧窗口调到前台)。
/// openURL: 对已在访达中显示的文件夹会去重、只置前旧窗口,不满足需求。
/// 做法:先激活访达(带 IgnoreOtherApps),再用 CGEventPostToPid 向访达进程定向投递一次
/// Cmd+N——pid 定向投递不依赖激活时序,事件一定由访达自己处理并新建窗口。
/// 新窗口由访达异步创建:记录 Cmd+N 前的焦点窗口 ID,轮询等焦点窗口变成「另一个 ID」
/// (即新窗口出现)后立即最大化(等效绿色缩放按钮,非全屏)。AX 调用需主线程,本函数
/// 经 bridge 已在主线程。
/// 应用本身持有辅助功能权限(事件 tap 依赖),合成按键与 AX 操作合法。
/// Open a NEW, maximized Finder window every time (Win+E semantics; openURL: dedupes and
/// only raises an existing window showing the folder). Activate Finder first
/// (IgnoreOtherApps), then post one Cmd+N straight to Finder's process via CGEventPostToPid
/// -- pid-targeted delivery does not depend on activation timing: Finder itself always
/// dequeues it and creates the window. The new window is created asynchronously: remember
/// the focused-window id before Cmd+N, poll until the focused window id changes (the new
/// window has appeared), then maximize it right away (zoom, NOT fullscreen). AX calls need
/// the main thread; this function already runs there (via the event bridge). The app holds
/// the Accessibility permission (its event taps require it), so synthesizing keystrokes and
/// AX writes is legitimate.
unsafe fn open_new_finder_window() {
    let mut finder_app = find_finder_app();
    if finder_app.is_null() {
        // 访达未运行:openURL: 走 LaunchServices 拉起访达并打开个人文件夹(带窗口),
        // 等它启动后同样把窗口最大化。
        // Finder not running: openURL: launches it via LaunchServices with the home folder;
        // wait for the launch, then maximize the window the same way.
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
        let path = make_nsstring(&home);
        // fileURLWithPath: 返回自动释放的 NSURL(+0),不归调用者所有,不要 CFRelease。
        // fileURLWithPath: returns an autoreleased NSURL (+0) we do not own; never release.
        let url: *mut AnyObject = msg_send![class!(NSURL), fileURLWithPath: path];
        CFRelease(path as *const c_void);
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let opened: bool = msg_send![workspace, openURL: url];
        log_debug!("[quick] Finder launched with home folder: {}", opened);
        // 冷启动可能要数秒:最多等 ~4s。
        // Cold start can take seconds: wait up to ~4s.
        for _ in 0..20 {
            std::thread::sleep(std::time::Duration::from_millis(200));
            finder_app = find_finder_app();
            if !finder_app.is_null() {
                break;
            }
        }
        if finder_app.is_null() {
            log_debug!("[quick] Finder did not launch in time");
            return;
        }
        let pid: i32 = msg_send![finder_app, processIdentifier];
        // 启动参数打开的文件夹窗口即新窗口;等它注册到 AX 后最大化。
        // The launch-opened folder window IS the new window; maximize once it shows in AX.
        for _ in 0..12 {
            std::thread::sleep(std::time::Duration::from_millis(150));
            if crate::window_management::focused_cgwid_of_pid(pid).is_some() {
                let ok = crate::window_management::maximize_focused_window_of_pid(pid);
                log_debug!("[quick] launched Finder window maximized: {}", ok);
                return;
            }
        }
        return;
    }

    let pid: i32 = msg_send![finder_app, processIdentifier];
    // Cmd+N 前的焦点窗口:轮询时用它区分「新窗口出现」与「旧窗口仍在」。
    // The focused window before Cmd+N: lets the poll tell the new window from the old one.
    let prev_cgwid = crate::window_management::focused_cgwid_of_pid(pid);
    // NSApplicationActivateIgnoringOtherApps = 1 << 1;activateWithOptions: 返回 BOOL,
    // 必须用 bool 接收(objc2 debug 下校验返回类型编码)。
    // NSApplicationActivateIgnoringOtherApps = 1 << 1; activateWithOptions: returns BOOL and
    // must be received as bool (objc2 validates return encodings in debug builds).
    let activated: bool = msg_send![finder_app, activateWithOptions: 2isize];
    log_debug!("[quick] Finder activated: {} pid={}", activated, pid);
    post_cmd_n_to_pid(pid);
    for _ in 0..12 {
        std::thread::sleep(std::time::Duration::from_millis(150));
        let cur = crate::window_management::focused_cgwid_of_pid(pid);
        if let Some(cgwid) = cur {
            if Some(cgwid) != prev_cgwid {
                let ok = crate::window_management::maximize_focused_window_of_pid(pid);
                log_debug!("[quick] new Finder window maximized: {}", ok);
                return;
            }
        }
    }
    log_debug!("[quick] new Finder window did not appear in time");
}

/// 在运行应用列表里找访达(返回 NSRunningApplication,+0 引用,不归调用者所有)。
/// Find Finder in the running applications (returns an NSRunningApplication, a +0 reference
/// we do not own).
unsafe fn find_finder_app() -> *mut AnyObject {
    let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
    let finder_ns = make_nsstring("com.apple.finder");
    let apps: *mut AnyObject = msg_send![workspace, runningApplications];
    let count: usize = msg_send![apps, count];
    let mut found: *mut AnyObject = std::ptr::null_mut();
    for i in 0..count {
        let app: *mut AnyObject = msg_send![apps, objectAtIndex: i as isize];
        // bundleIdentifier 是 copy 属性的 getter,返回 +0 引用(不归调用者所有),
        // 绝不能 CFRelease(提前释放会在池排空时二次释放,段错误)。
        // bundleIdentifier is a copy-property getter returning a +0 reference we do NOT own;
        // never CFRelease it (early release double-frees when the pool drains).
        let bundle: *mut AnyObject = msg_send![app, bundleIdentifier];
        if bundle.is_null() {
            continue;
        }
        let is_finder: bool = msg_send![bundle, isEqualToString: finder_ns];
        if is_finder {
            found = app;
            break;
        }
    }
    CFRelease(finder_ns as *const c_void);
    found
}

/// 向指定进程定向投递一次 Cmd+N(按下 + 抬起)。事件进入该进程自己的队列,由它处理,
/// 因此不受其他应用焦点切换影响。
/// Post one Cmd+N (down + up) targeted at the given process. The event enters that process's
/// own queue and is handled by it, so focus changes in other apps cannot steal it.
unsafe fn post_cmd_n_to_pid(pid: i32) {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventPostToPid(pid: i32, event: CGEventRef);
    }
    // kVK_ANSI_N = 45(与 shortcut.rs 的 "n" -> 0x2D 一致)。
    // kVK_ANSI_N = 45 (matches shortcut.rs's "n" -> 0x2D).
    const KEY_N: u16 = 0x2D;
    const K_FLAG_COMMAND: CGEventFlags = 0x00100000;
    let down = CGEventCreateKeyboardEvent(std::ptr::null_mut(), KEY_N, true);
    let up = CGEventCreateKeyboardEvent(std::ptr::null_mut(), KEY_N, false);
    CGEventSetFlags(down, K_FLAG_COMMAND);
    CGEventSetFlags(up, K_FLAG_COMMAND);
    CGEventPostToPid(pid, down);
    std::thread::sleep(std::time::Duration::from_millis(30));
    CGEventPostToPid(pid, up);
    // CGEventCreateKeyboardEvent 返回 +1,用完释放。
    // CGEventCreateKeyboardEvent returns +1; release after use.
    CFRelease(down as *const c_void);
    CFRelease(up as *const c_void);
    log_debug!("[quick] Cmd+N posted to pid {}", pid);
}
