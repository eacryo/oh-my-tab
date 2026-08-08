//! 历史剪贴板模块(第一版:纯文本、内存态、不持久化)。
//!
//! 架构:
//! - 主线程 NSTimer 每 0.5s 轮询 NSPasteboard 的 changeCount,变化时读纯文本入历史
//!   (连续复制相同内容去重,上限裁剪)。
//! - Option+V 由 event_monitor 的 tap 检测,经 bridge 转主线程调用 on_clipboard_toggle,
//!   显示/关闭浮窗;↑↓/Enter/Esc/点击选择,Enter 或点击 = 写回剪贴板 + 合成 Cmd+V
//!   自动粘贴(行为同 Windows 的 Win+V)。
//! - 只保存纯文本(NSPasteboardTypeString),不做图片,不做持久化。
//!
//! History clipboard module (v1: text-only, in-memory, no persistence).
//!
//! Architecture:
//! - A main-thread NSTimer polls NSPasteboard's changeCount every 0.5s; when it changes,
//!   the plain text is read into the history (duplicates of the top are skipped, overflow
//!   trimmed).
//! - Option+V is detected by the event_monitor tap and marshalled to the main thread via the
//!   bridge (on_clipboard_toggle), showing/hiding the picker. Arrow keys / Enter / Esc /
//!   clicks navigate; Enter or a click = write back to the pasteboard + synthesize Cmd+V for
//!   an automatic paste (mirrors Windows' Win+V).
//! - Text only (NSPasteboardTypeString); no images, no persistence.

use crate::config::CONFIG;
use crate::event_tap::{
    CGEventCreateKeyboardEvent, CGEventFlags, CGEventPost, CGEventSetFlags, K_CG_SESSION_EVENT_TAP,
};
use crate::ffi::{
    class_addMethod, make_nsstring, nsstring_to_rust, objc_allocateClassPair,
    objc_registerClassPair, release_obj, CFRelease, ObjPtr,
};
use crate::{log_debug, log_info};
use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::ffi::{c_void, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};

// ========== 常量 / constants ==========

/// 剪贴板文本类型(与 NSPasteboardTypeString 相同)。
/// The plain-text pasteboard type (same as NSPasteboardTypeString).
const NSPASTEBOARD_TYPE_STRING: &str = "public.utf8-plain-text";
/// 模拟粘贴用的 V 键码 / keycode used when synthesizing Cmd+V.
const VK_V: u16 = 9;
/// 模拟粘贴用的 Command 修饰掩码 / Command modifier mask for synthesized paste.
const K_CG_EVENT_FLAG_MASK_COMMAND: CGEventFlags = 0x00100000;
/// 轮询间隔(秒)/ polling interval (seconds)
const POLL_INTERVAL: f64 = 0.5;
/// 浮窗可视行数上限(超出滚动)/ max visible rows (scrolls beyond).
const PICKER_MAX_ROWS: usize = 10;
/// 浮窗宽度 / picker width.
const PICKER_W: f64 = 420.0;
/// 行距(行按钮 + 间距)/ row pitch.
const ROW_H: f64 = 34.0;
/// 行按钮高度 / row button height.
const ROW_BTN_H: f64 = 28.0;
/// 上下留白 / vertical padding.
const PAD_Y: f64 = 10.0;
/// 左右留白 / horizontal padding.
const PAD_X: f64 = 12.0;
/// 玻璃圆角:小浮窗固定小圆角,不跟随 config 的大圆角(那会让 420pt 小窗成胶囊)。
/// Fixed small corner radius for the glass: NOT the config value (which would turn a 420pt
/// panel into a capsule).
const CORNER_R: f64 = 14.0;
/// 选中行的圆角背景块圆角 / selected-row highlight tile corner radius.
const SEL_TILE_R: f64 = 7.0;

// ========== 状态 / state ==========

/// 历史列表,最新在前 / history, newest first.
static CLIP_HISTORY: LazyLock<Mutex<Vec<String>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// 上次读到的 changeCount(变化才读剪贴板)/ last observed changeCount (read only on change).
static LAST_CHANGE_COUNT: LazyLock<Mutex<i64>> = LazyLock::new(|| Mutex::new(-1));

/// 轮询 timer(主线程)/ the polling timer (main thread).
static POLL_TIMER: OnceLock<Mutex<ObjPtr>> = OnceLock::new();

/// 浮窗是否可见 / whether the picker is visible.
static PICKER_VISIBLE: AtomicBool = AtomicBool::new(false);

/// 当前选中行索引 / the currently selected row index.
static PICKER_SELECTION: Mutex<usize> = Mutex::new(0);

/// 浮窗窗口 / the picker window.
static PICKER_WINDOW: Mutex<Option<ObjPtr>> = Mutex::new(None);

/// 浮窗容器(接收键盘)/ the picker container (receives key events).
static PICKER_CONTAINER: Mutex<Option<ObjPtr>> = Mutex::new(None);

/// 每行按钮指针(按行索引,供高亮/点击)/ row button pointers by index (highlight / click).
static ROW_BUTTONS: LazyLock<Mutex<Vec<ObjPtr>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// 行列表重建进行中:重建期间 addSubview 的新行按钮会因鼠标恰好在区域内而立即派发
/// mouseEntered(ActiveInKeyWindow + InVisibleRect 的 tracking area),若该回调再触发
/// rebuild_rows 就是无限递归(窗口为 key 时键盘导航触发 rebuild 必现,曾导致进程挂起)。
/// 重建期间派发的 mouseEntered 一律忽略;用户真实移动鼠标触发的新事件正常处理。
///
/// A row rebuild is in progress: rows added during a rebuild dispatch mouseEntered
/// immediately when the cursor happens to be inside (ActiveInKeyWindow + InVisibleRect
/// tracking areas), and a handler that re-triggers rebuild_rows would recurse forever
/// (reproducible via keyboard navigation once the window is key; the process used to hang).
/// mouseEntered events dispatched during a rebuild are ignored; real cursor movement after
/// the rebuild is handled normally.
static REBUILDING: AtomicBool = AtomicBool::new(false);

// ========== 纯逻辑(可测)/ pure logic (testable) ==========

/// 把新文本记入历史。规则:
/// - 空文本忽略
/// - 与栈顶相同(连续复制同一内容)忽略,不重复入栈
/// - 超出 max 裁剪最旧条目
///
/// 返回是否真正写入。
///
/// Record a new text into the history:
/// - empty text is ignored
/// - a duplicate of the top entry (re-copying the same text) is skipped
/// - entries beyond `max` are trimmed from the tail
///
/// Returns whether something was actually recorded.
fn record_text(history: &mut Vec<String>, text: &str, max: usize) -> bool {
    if text.is_empty() || max == 0 {
        return false;
    }
    if history.first().map(|s| s == text).unwrap_or(false) {
        return false;
    }
    history.insert(0, text.to_string());
    if history.len() > max {
        history.truncate(max);
    }
    true
}

/// 当前生效的最大条数(从 CONFIG 读,设置保存后下次轮询生效)。
/// The effective max entry count (read from CONFIG; takes effect on the next poll).
fn max_entries() -> usize {
    CONFIG
        .read()
        .map(|c| c.clipboard.max_entries as usize)
        .unwrap_or(50)
        .clamp(1, 100)
}

// ========== 剪贴板读写 / pasteboard I/O ==========

/// 读当前剪贴板纯文本(无文本返回 None)。
/// Read the pasteboard's plain text (None when no text).
unsafe fn read_pasteboard_text() -> Option<String> {
    let pb: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
    if pb.is_null() {
        return None;
    }
    let type_ns = make_nsstring(NSPASTEBOARD_TYPE_STRING);
    let s: *mut AnyObject = msg_send![pb, stringForType: type_ns];
    CFRelease(type_ns as *const c_void);
    if s.is_null() {
        return None;
    }
    Some(nsstring_to_rust(s))
}
/// 把文本写回剪贴板(粘贴路径)。写回会 bump changeCount,下次轮询读到的是本文本,
/// 但 record_text 的去重(与栈顶相同)会忽略它,不会产生重复条目。
/// Write text back to the pasteboard (the paste path). This bumps changeCount; the next poll
/// reads this same text, but record_text's dedup (same as the top entry) skips it.
unsafe fn write_pasteboard_text(text: &str) {
    let pb: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
    if pb.is_null() {
        return;
    }
    // 标准写入流程:先 clearContents 声明所有权,再 setString——单独调用 setString
    // 在某些场景会返回 NO(实测曾失败,导致 Cmd+V 粘贴的是剪贴板旧内容)。
    // Standard write flow: clearContents first to take ownership, then setString -- calling
    // setString alone returned NO in practice (the Cmd+V then pasted the OLD clipboard
    // content). clearContents returns NSInteger (the new changeCount).
    let _: isize = msg_send![pb, clearContents];
    let type_ns = make_nsstring(NSPASTEBOARD_TYPE_STRING);
    let ns = make_nsstring(text);
    let ok: bool = msg_send![pb, setString: ns, forType: type_ns];
    // 读回验证写入结果 / read back to verify the write.
    let back: *mut AnyObject = msg_send![pb, stringForType: type_ns];
    let back_str = if back.is_null() {
        "NULL".to_string()
    } else {
        nsstring_to_rust(back)
    };
    log_debug!(
        "[clip] write back {} chars (setString ok={}) readback=\"{}\"",
        text.chars().count(),
        ok,
        truncate_line(&back_str, 20)
    );
    CFRelease(type_ns as *const c_void);
    CFRelease(ns as *const c_void);
}

// ========== 轮询 / polling ==========

/// 轮询一次:changeCount 变化时读文本入历史。
/// Poll once: read the text into history when changeCount changed.
fn poll_clipboard() {
    let changed = unsafe {
        let pb: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
        if pb.is_null() {
            return;
        }
        let cc: i64 = msg_send![pb, changeCount];
        let mut last = LAST_CHANGE_COUNT.lock().unwrap();
        if *last == cc {
            return;
        }
        let prev = *last;
        *last = cc;
        log_debug!("[clip] pasteboard changeCount {} -> {}", prev, cc);
        true
    };
    if !changed {
        return;
    }
    match unsafe { read_pasteboard_text() } {
        Some(text) => {
            let mut hist = CLIP_HISTORY.lock().unwrap();
            if record_text(&mut hist, &text, max_entries()) {
                log_debug!(
                    "[clip] recorded text ({} chars, total {})",
                    text.chars().count(),
                    hist.len()
                );
            } else {
                log_debug!(
                    "[clip] change skipped: dup/empty (text {} chars, total {})",
                    text.chars().count(),
                    hist.len()
                );
            }
        }
        None => log_debug!("[clip] change but no text (non-text paste?)"),
    }
}

/// timer tick 回调(主线程):继续轮询。
/// Timer tick callback (main thread): keep polling.
extern "C" fn clip_poll_tick(_self: *mut c_void, _cmd: Sel, _timer: *mut c_void) {
    poll_clipboard();
}

/// 启动轮询(幂等):创建主线程 NSTimer,并立刻记录一次当前剪贴板。
/// Start polling (idempotent): create a main-thread NSTimer and record the current
/// pasteboard once immediately.
pub fn start() {
    unsafe {
        let timer_holder = POLL_TIMER.get_or_init(|| Mutex::new(ObjPtr(std::ptr::null_mut())));
        let mut guard = timer_holder.lock().unwrap();
        if !guard.0.is_null() {
            return; // 已在跑 / already running
        }
        // 先记录当前剪贴板,否则首次呼出历史为空。
        // Record the current pasteboard first, or the first summon would show an empty list.
        poll_clipboard();
        // 注册剪贴板变化通知:每次变化即时记录,轮询间隔内的快速连续复制不丢失。
        // Register the pasteboard-change notification: instant recording on every change, so
        // rapid consecutive copies between polling samples are not lost.
        register_pasteboard_observer();
        let timer: *mut AnyObject = msg_send![
            class!(NSTimer),
            scheduledTimerWithTimeInterval: POLL_INTERVAL,
            target: timer_target(),
            selector: sel!(clipPollTick:),
            userInfo: std::ptr::null::<AnyObject>(),
            repeats: true
        ];
        *guard = ObjPtr(timer);
        log_info!(
            "Clipboard history polling started (every {}s).",
            POLL_INTERVAL
        );
    }
}

/// 停止轮询(幂等)。/ Stop polling (idempotent).
pub fn stop() {
    unsafe {
        let timer_holder = POLL_TIMER.get_or_init(|| Mutex::new(ObjPtr(std::ptr::null_mut())));
        let mut guard = timer_holder.lock().unwrap();
        if !guard.0.is_null() {
            let _: () = msg_send![guard.0, invalidate];
            release_obj(guard.0);
            *guard = ObjPtr(std::ptr::null_mut());
            log_info!("Clipboard history polling stopped.");
        }
    }
}

// ========== 通知观察者 / notification observer ==========

/// 通知观察者单例,承载两个回调:
/// - NSPasteboardDidChangeNotification:剪贴板每次变化即时记录——轮询只在 0.5s 间隔
///   采样一次"当前值",两次采样间的快速连续复制会被跳过(历史只剩最近一条);
///   通知在每次变化时都回调,事件不丢。
/// - NSWindowDidResignKeyNotification:浮窗失去 key(点击了外部)→ 自动隐藏。
///
/// A singleton notification observer carrying two callbacks:
/// - NSPasteboardDidChangeNotification: record on every pasteboard change. Polling samples
///   the current value once per 0.5s interval, so rapid consecutive copies between samples
///   are skipped (history ends up with only the newest entry); the notification fires on
///   every change, so no event is lost.
/// - NSWindowDidResignKeyNotification: the picker loses key (a click outside) -> hide.
unsafe fn observer() -> *mut AnyObject {
    static OBSERVER: OnceLock<ObjPtr> = OnceLock::new();
    OBSERVER
        .get_or_init(|| {
            let name = CString::new("OhMyTabClipboardObserver").unwrap();
            let superclass = class!(NSObject) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(clipboardPasteboardChanged:),
                pasteboard_changed as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(clipboardWindowResigned:),
                window_did_resign_key as *mut c_void,
                types.as_ptr(),
            );
            objc_registerClassPair(cls);
            // 实例 alloc(+1):进程级单例,不释放(与静态生命周期一致)。
            // Instance alloc (+1): process-level singleton, never released (matches the
            // static's lifetime).
            let obj: *mut AnyObject = msg_send![cls as *const AnyObject, new];
            ObjPtr(obj)
        })
        .0
}

/// 剪贴板变化通知回调(任意线程):即时记录当前文本。
/// Pasteboard-change notification callback (any thread): record the current text immediately.
extern "C" fn pasteboard_changed(_self: *mut c_void, _cmd: Sel, _note: *mut c_void) {
    poll_clipboard();
}

/// 浮窗失去 key 通知回调(主线程):点击外部等场景自动隐藏。
/// Picker resign-key notification callback (main thread): auto-hide on outside clicks, etc.
extern "C" fn window_did_resign_key(_self: *mut c_void, _cmd: Sel, _note: *mut c_void) {
    hide_picker();
}

/// 是否已注册剪贴板变化通知(幂等,防止 start/stop 反复注册导致重复回调)。
/// Whether the pasteboard-change notification has been registered (idempotent; start/stop
/// cycles must not double-register and duplicate callbacks).
static NOTIFICATION_REGISTERED: AtomicBool = AtomicBool::new(false);

/// 注册剪贴板变化通知(仅一次)。/ Register the pasteboard-change notification (once).
unsafe fn register_pasteboard_observer() {
    if NOTIFICATION_REGISTERED.swap(true, Ordering::SeqCst) {
        return;
    }
    let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
    let name = make_nsstring("NSPasteboardDidChangeNotification");
    let _: () = msg_send![
        center,
        addObserver: observer(),
        selector: sel!(clipboardPasteboardChanged:),
        name: name,
        object: std::ptr::null::<AnyObject>()
    ];
    CFRelease(name as *const c_void);
    log_info!("Pasteboard change observer registered.");
}

/// NSTimer 的 target:NSTimer 会向它发 clipPollTick:。动态注册一个轻量类,方法转发到
/// clip_poll_tick。类只注册一次,实例每次 start 新建(+1,随 timer 持有)。
/// The NSTimer target: NSTimer sends clipPollTick: to it. A tiny dynamic class forwards the
/// method to clip_poll_tick; the class is registered once, and an instance is created per start.
unsafe fn timer_target() -> *mut AnyObject {
    static TIMER_CLS: OnceLock<ObjPtr> = OnceLock::new();
    let cls = *TIMER_CLS.get_or_init(|| {
        let name = CString::new("OhMyTabClipTimerTarget").unwrap();
        let superclass = class!(NSObject) as *const _ as *mut AnyObject;
        let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
        let types = CString::new("v@:@").unwrap();
        class_addMethod(
            cls,
            sel!(clipPollTick:),
            clip_poll_tick as *mut c_void,
            types.as_ptr(),
        );
        objc_registerClassPair(cls);
        ObjPtr(cls)
    });
    let obj: *mut AnyObject = msg_send![cls.0 as *const AnyObject, new];
    obj
}

// ========== 浮窗 / the picker ==========

/// Option+V 呼出/关闭(由 bridge 在主线程调用)。
/// Toggle the picker on Option+V (called on the main thread by the bridge).
pub(crate) extern "C" fn on_clipboard_toggle(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    if PICKER_VISIBLE.load(Ordering::SeqCst) {
        hide_picker();
        return;
    }
    // 历史为空不显示 / show nothing when the history is empty.
    let hist = CLIP_HISTORY.lock().unwrap();
    if hist.is_empty() {
        log_debug!("[clip] toggle with empty history; ignored");
        return;
    }
    drop(hist);
    *PICKER_SELECTION.lock().unwrap() = 0;
    show_picker();
}

/// 显示浮窗(构建一次,复用;窗口高度随可视行数动态调整)。
/// Show the picker (built once, reused; the window height follows the visible row count).
fn show_picker() {
    unsafe {
        ensure_picker_window();
        let window = match *PICKER_WINDOW.lock().unwrap() {
            Some(w) => w.0,
            None => return,
        };
        let hist_len = CLIP_HISTORY.lock().unwrap().len();
        log_debug!("[clip] show picker: history={} entries", hist_len);

        // 窗口高度 = 上下留白 + 可视行数 * 行距(不空留整页)。
        // Window height = paddings + visible rows * row pitch (no empty page).
        let visible = hist_len.min(PICKER_MAX_ROWS);
        let h = PAD_Y * 2.0 + visible as f64 * ROW_H;
        let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
        let screen_frame: NSRect = msg_send![screen, frame];
        let x = (screen_frame.size.width - PICKER_W) / 2.0 + screen_frame.origin.x;
        let y = (screen_frame.size.height - h) / 2.0 + screen_frame.origin.y;
        let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(PICKER_W, h));
        let _: () = msg_send![window, setFrame: frame, display: true];

        rebuild_rows();
        // 每次呼出滚动到顶部(最新条目)。
        // Scroll to the top on every summon (the newest entry).
        if let Some(c) = *PICKER_CONTAINER.lock().unwrap() {
            let _: () = msg_send![c.0, scrollPoint: NSPoint::new(0.0, 0.0)];
        }
        let _: () = msg_send![window, orderFrontRegardless];
        let _: () = msg_send![window, makeKeyWindow];
        // 键盘焦点给容器(方向键/Enter/Esc)。
        // Keyboard focus to the container (arrows / Enter / Esc).
        if let Some(c) = *PICKER_CONTAINER.lock().unwrap() {
            // makeFirstResponder: 返回 BOOL('B')。
            // makeFirstResponder: returns BOOL ('B').
            let _: bool = msg_send![window, makeFirstResponder: c.0];
        }
        PICKER_VISIBLE.store(true, Ordering::SeqCst);
    }
}

/// 隐藏浮窗。/ Hide the picker.
fn hide_picker() {
    PICKER_VISIBLE.store(false, Ordering::SeqCst);
    // 锁内只取指针,orderOut 放到锁外:orderOut 会同步触发 NSWindowDidResignKeyNotification,
    // 回调再进 hide_picker 并锁同一把 Mutex——非重入锁会自死锁(曾导致进程挂起)。
    // Take the pointer under the lock but orderOut outside it: orderOut synchronously fires
    // NSWindowDidResignKeyNotification, whose callback re-enters hide_picker and locks the
    // same non-reentrant Mutex -- a self-deadlock (the process used to hang).
    let win = *PICKER_WINDOW.lock().unwrap();
    unsafe {
        if let Some(w) = win {
            let _: () = msg_send![w.0, orderOut: std::ptr::null::<AnyObject>()];
        }
    }
}

/// 构建浮窗窗口(一次)。/ Build the picker window (once).
unsafe fn ensure_picker_window() {
    if PICKER_WINDOW.lock().unwrap().is_some() {
        return;
    }
    let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
    let screen_frame: NSRect = msg_send![screen, frame];
    let w = PICKER_W;
    // 初始高度按最大可视行数(show_picker 每次按实际条数重设)。
    // Initial height sized for the max visible rows (show_picker re-sizes per summon).
    let h = PAD_Y * 2.0 + PICKER_MAX_ROWS as f64 * ROW_H;
    let x = (screen_frame.size.width - w) / 2.0 + screen_frame.origin.x;
    let y = (screen_frame.size.height - h) / 2.0 + screen_frame.origin.y;
    let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));

    // NSPanel + NSWindowStyleMaskNonactivatingPanel(1<<7):成为 key 但不激活所属 app,
    // 与窗口切换浮窗一致,避免抢焦点。
    // NSPanel + NSWindowStyleMaskNonactivatingPanel (1<<7): becomes key WITHOUT activating
    // the owning app (same as the switcher overlay), so focus isn't stolen.
    let style: u64 = 1 << 7;

    let window_cls = {
        let name = CString::new("OhMyTabClipboardWindow").unwrap();
        let superclass = class!(NSPanel) as *const _ as *mut AnyObject;
        let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
        let types_bool = CString::new("B@:").unwrap();
        class_addMethod(
            cls,
            sel!(canBecomeKeyWindow),
            picker_window_can_become_key as *mut c_void,
            types_bool.as_ptr(),
        );
        objc_registerClassPair(cls);
        cls
    };
    let window: *mut AnyObject = msg_send![window_cls, alloc];
    let window: *mut AnyObject = msg_send![window, initWithContentRect: frame, styleMask: style, backing: 2u64, defer: false];
    let _: () = msg_send![window, setLevel: 3u64];
    let _: () = msg_send![window, setOpaque: false];
    let _: () = msg_send![window, setReleasedWhenClosed: false];
    // 背景与窗口切换浮窗同款:clearColor + 玻璃视图提供视觉效果(见下)。
    // Same backdrop as the switcher overlay: clearColor + a glass view for the visuals (below).
    let clear: *mut AnyObject = msg_send![class!(NSColor), clearColor];
    let _: () = msg_send![window, setBackgroundColor: clear];
    // 玻璃自带深度,窗口阴影是多余的(与窗口切换浮窗一致)。
    // The glass carries its own depth; the window shadow is redundant (same as the overlay).
    let _: () = msg_send![window, setHasShadow: false];

    // --- 玻璃背景(Liquid Glass),与窗口切换浮窗同款 ---
    // macOS 26+  → NSGlassEffectView(新公开 API,自带模糊)
    // macOS <26 → NSVisualEffectView(withinWindow + Dark material)
    // Glass backdrop (Liquid Glass), same as the switcher overlay:
    // macOS 26+ -> NSGlassEffectView (new public API, built-in blur)
    // macOS <26  -> NSVisualEffectView (withinWindow + Dark material).
    let is_macos_26 = AnyClass::get(c"NSGlassEffectView").is_some();
    // 容器将被加进的父视图 / the parent view the container is added into.
    let content_parent: *mut AnyObject;

    if is_macos_26 {
        let glass_cls = AnyClass::get(c"NSGlassEffectView").unwrap();
        let glass: *mut AnyObject = msg_send![glass_cls, alloc];
        let glass: *mut AnyObject =
            msg_send![glass, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h))];
        // 小浮窗固定小圆角(不跟随 config 的大圆角)。
        // Fixed small corner radius for this small panel (not the config's big one).
        let radius = CORNER_R;
        let _: () = msg_send![glass, setCornerRadius: radius];
        let style_i: i64 = match CONFIG.read().unwrap().appearance.glass_style.as_str() {
            "clear" => 1,
            _ => 0,
        };
        let _: () = msg_send![glass, setStyle: style_i];
        let tint_hex = crate::config::parse_hex8(&CONFIG.read().unwrap().appearance.glass_tint);
        let tint = crate::ffi::hex_to_ns_color(tint_hex);
        let _: () = msg_send![glass, setTintColor: tint];
        let _: () = msg_send![glass, setAutoresizingMask: 18u64];
        let _: () = msg_send![window, setContentView: glass];
        // NSGlassEffectView.contentView 初始可能为 nil,自建一个内层视图。
        // NSGlassEffectView.contentView may be nil initially - create our own.
        let inner: *mut AnyObject = msg_send![class!(NSView), alloc];
        let inner: *mut AnyObject =
            msg_send![inner, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h))];
        let _: () = msg_send![inner, setAutoresizingMask: 18u64];
        let _: () = msg_send![glass, setContentView: inner];
        // 硬裁剪背景模糊进圆角(与窗口切换浮窗同款处理)。
        // Hard-clip the backdrop blur into the corner radius (same trick as the overlay).
        let _: () = msg_send![glass, setWantsLayer: true];
        let glass_layer: *mut AnyObject = msg_send![glass, layer];
        if !glass_layer.is_null() {
            let _: () = msg_send![glass_layer, setCornerRadius: radius];
            let _: () = msg_send![glass_layer, setMasksToBounds: true];
        }
        content_parent = inner;
    } else {
        let content: *mut AnyObject = msg_send![window, contentView];
        let ve: *mut AnyObject = msg_send![class!(NSVisualEffectView), alloc];
        let ve: *mut AnyObject =
            msg_send![ve, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h))];
        // withinWindow blending + Dark material(与窗口切换浮窗一致)。
        // withinWindow blending + Dark material (same as the switcher overlay).
        let _: () = msg_send![ve, setBlendingMode: 1u64]; // WithinWindow
        let _: () = msg_send![ve, setMaterial: 12u64]; // Dark
        let _: () = msg_send![ve, setState: 1u64]; // Active
        let _: () = msg_send![ve, setAutoresizingMask: 18u64];
        let _: () = msg_send![content, addSubview: ve];
        content_parent = ve;
    }

    // 容器(接收键盘事件;flipped,行从顶部往下排,最新条目在顶)。
    // Container (receives key events; flipped so rows stack top-down, newest on top).
    let container = {
        let name = CString::new("OhMyTabClipboardContainer").unwrap();
        let superclass = class!(NSView) as *const _ as *mut AnyObject;
        let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
        let types_key = CString::new("v@:@").unwrap();
        class_addMethod(
            cls,
            sel!(keyDown:),
            container_key_down as *mut c_void,
            types_key.as_ptr(),
        );
        let types_bool = CString::new("B@:").unwrap();
        class_addMethod(
            cls,
            sel!(acceptsFirstResponder),
            container_accepts_first_responder as *mut c_void,
            types_bool.as_ptr(),
        );
        // flipped:原点在左上,y 向下增长——行从顶部排起,最新在最上。
        // Flipped: origin at top-left, y grows downward -- rows stack from the top.
        class_addMethod(
            cls,
            sel!(isFlipped),
            container_is_flipped as *mut c_void,
            types_bool.as_ptr(),
        );
        objc_registerClassPair(cls);
        cls
    };
    let container: *mut AnyObject = msg_send![container, alloc];
    let container: *mut AnyObject = msg_send![
        container,
        initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h))
    ];
    // documentView 的高度由 rebuild_rows 按条目数动态设置,不跟随 scroll view 拉伸。
    // The document view's height is set dynamically by rebuild_rows; it must NOT stretch
    // with the scroll view.
    let _: () = msg_send![container, setAutoresizingMask: 0u64];

    // NSScrollView:滚动条 + 自动滚轮滚动(超可视行数时可滚)。
    // NSScrollView: scroller + wheel scrolling (when entries exceed the visible rows).
    let scroll: *mut AnyObject = msg_send![class!(NSScrollView), alloc];
    let scroll: *mut AnyObject =
        msg_send![scroll, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h))];
    let _: () = msg_send![scroll, setAutoresizingMask: 18u64];
    let _: () = msg_send![scroll, setBorderType: 0u64]; // NSNoBorder
    let _: () = msg_send![scroll, setDrawsBackground: false];
    let _: () = msg_send![scroll, setHasVerticalScroller: true];
    let _: () = msg_send![scroll, setHasHorizontalScroller: false];
    let _: () = msg_send![scroll, setAutohidesScrollers: true];
    let _: () = msg_send![scroll, setScrollerStyle: 1isize]; // NSScrollerStyleOverlay(悬浮滚动条)
    let _: () = msg_send![content_parent, addSubview: scroll];
    release_obj(scroll);
    let _: () = msg_send![scroll, setDocumentView: container];
    release_obj(container);
    // 点击外部(浮窗失去 key)→ 自动隐藏。Win+V 同款行为:呼出后点任何地方即消失。
    // Outside clicks (the picker resigns key) -> auto-hide. Same as Win+V: any click after
    // summoning dismisses the picker.
    let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
    let resign_name = make_nsstring("NSWindowDidResignKeyNotification");
    let _: () = msg_send![
        center,
        addObserver: observer(),
        selector: sel!(clipboardWindowResigned:),
        name: resign_name,
        object: window
    ];
    CFRelease(resign_name as *const c_void);
    *PICKER_CONTAINER.lock().unwrap() = Some(ObjPtr(container));
    *PICKER_WINDOW.lock().unwrap() = Some(ObjPtr(window));
}

/// 根据当前历史重建行按钮(选中行高亮 + 圆角背景块)。
/// Rebuild the row buttons from history (selected row highlighted with a rounded tile).
unsafe fn rebuild_rows() {
    let hist = CLIP_HISTORY.lock().unwrap();
    let container = match *PICKER_CONTAINER.lock().unwrap() {
        Some(c) => c.0,
        None => return,
    };
    // 重建期间忽略 mouseEntered(见 REBUILDING 注释)。
    // Ignore mouseEntered during the rebuild (see the REBUILDING note).
    REBUILDING.store(true, Ordering::SeqCst);
    // 记录当前滚动位置(flipped 坐标下,clipView.bounds.origin.y 即滚动偏移),
    // 重建后恢复——悬停/方向键 rebuild 不会把视口弹回顶部。
    // Record the current scroll offset (the clip view's bounds origin y in flipped coords)
    // and restore it after the rebuild, so hover/arrow rebuilds don't snap the viewport.
    let scroll_offset = {
        let clip: *mut AnyObject = msg_send![container, superview];
        if clip.is_null() {
            0.0
        } else {
            let b: NSRect = msg_send![clip, bounds];
            b.origin.y
        }
    };

    // 移除旧行 / remove old rows.
    // 注意:按钮 alloc +1 已在 addSubview 后 release(由父视图持有);
    // removeFromSuperview 会让父视图释放引用(计数归零、对象 dealloc),绝不能
    // 再对其 release——否则二次释放 use-after-free(曾导致第二次呼出 segfault)。
    // Note: the button's alloc +1 was released after addSubview (owned by the parent view);
    // removeFromSuperview drops the parent's reference (refcount hits zero, object deallocs),
    // so it must NOT be released again -- a second release was a use-after-free that crashed
    // on the second summon.
    let mut rows = ROW_BUTTONS.lock().unwrap();
    for &b in rows.iter() {
        let _: () = msg_send![b.0, removeFromSuperview];
    }
    rows.clear();

    // 文档高度 = 全部条目(滚动区域),由 NSScrollView 滚动。
    // Document height covers ALL entries (the scrollable area).
    let total = hist.len();
    let doc_h = PAD_Y * 2.0 + total as f64 * ROW_H;
    let _: () = msg_send![container, setFrameSize: NSSize::new(PICKER_W, doc_h)];

    let sel_idx = *PICKER_SELECTION.lock().unwrap();
    for i in 0..total {
        let y = PAD_Y + i as f64 * ROW_H;
        log_debug!(
            "[clip] row {} created: y={} title=\"{}\"",
            i,
            y,
            truncate_line(&hist[i], 20)
        );
        let btn: *mut AnyObject = msg_send![row_button_class(), alloc];
        let btn: *mut AnyObject = msg_send![
            btn,
            initWithFrame: NSRect::new(NSPoint::new(PAD_X, y), NSSize::new(PICKER_W - PAD_X * 2.0, ROW_BTN_H))
        ];
        let _: () = msg_send![btn, setBordered: false];
        let _: () = msg_send![btn, setAlignment: 0isize]; // left
                                                          // 截断长文本为单行 / truncate long text to a single line.
        let title = truncate_line(&hist[i], 60);
        let attr = make_row_attributed_title(&title, i == sel_idx);
        let _: () = msg_send![btn, setAttributedTitle: attr];
        release_obj(attr);
        // 选中行:半透明白圆角背景块(与文字一起构成强对比)。
        // Selected row: a semi-transparent white rounded tile behind the text.
        if i == sel_idx {
            let _: () = msg_send![btn, setWantsLayer: true];
            let layer: *mut AnyObject = msg_send![btn, layer];
            // 选中背景 = 系统强调色半透明:明暗界面都清晰(白字时代的固定 0.16 白块在
            // 浅色玻璃上不可见)。colorWithAlphaComponent: 的参数是 double。
            // Selected tile = the system accent color at partial alpha: legible on both light
            // and dark glass (the old fixed 0.16-white tile vanished on light glass).
            // colorWithAlphaComponent: takes a double.
            let accent: *mut AnyObject = msg_send![class!(NSColor), controlAccentColor];
            let accent_a: *mut AnyObject = msg_send![accent, colorWithAlphaComponent: 0.35f64];
            // layer_set_background 走 raw objc_msgSend:objc2 的 msg_send! 无法编码
            // CGColor 参数/返回(参数编码 '^{CGColor=}' 与 *mut c_void 的 '^v' 不匹配)。
            // layer_set_background goes through raw objc_msgSend: objc2's msg_send! can't
            // encode CGColor args/returns ('^{CGColor=}' vs '^v').
            crate::ffi::layer_set_background(layer, crate::ffi::ns_color_to_cg(accent_a));
            let _: () = msg_send![layer, setCornerRadius: SEL_TILE_R];
        }
        // 行点击 → handleClipboardRowClick:(tag = 行索引)。
        // Row click -> handleClipboardRowClick: (tag = row index).
        let _: () = msg_send![btn, setTag: i as isize];
        let _: () = msg_send![btn, setTarget: row_target()];
        let _: () = msg_send![btn, setAction: sel!(handleClipboardRowClick:)];
        // 悬停高亮:行按钮类重写 mouseEntered:,选中悬停行(与窗口切换浮窗一致)。
        // Hover highlight: the row-button class overrides mouseEntered: to select the hovered
        // row (same as the switcher overlay).
        let opts: u64 = 0x02 | 0x40 | 0x100; // MouseEnteredAndExited | ActiveInKeyWindow | InVisibleRect
        let ta: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
        let ta: *mut AnyObject = msg_send![
            ta,
            initWithRect: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0)),
            options: opts,
            owner: btn,
            userInfo: std::ptr::null::<AnyObject>()
        ];
        let _: () = msg_send![btn, addTrackingArea: ta];
        release_obj(ta);
        let _: () = msg_send![container, addSubview: btn];
        release_obj(btn);
        rows.push(ObjPtr(btn));
    }

    // 恢复滚动位置 / restore the scroll position.
    if scroll_offset > 0.0 {
        let _: () = msg_send![container, scrollPoint: NSPoint::new(0.0, scroll_offset)];
    }
    REBUILDING.store(false, Ordering::SeqCst);
}

/// 容器 flipped:原点在左上,行从顶部排起(最新在最上)。
/// Container is flipped: origin at top-left, rows stack from the top (newest first).
extern "C" fn container_is_flipped(_self: *mut c_void, _cmd: Sel) -> bool {
    true
}

/// 行按钮类(NSButton 子类,重写 mouseEntered: 实现悬停选中)。
/// Row-button class (NSButton subclass; mouseEntered: implements hover selection).
unsafe fn row_button_class() -> *mut AnyObject {
    static ROW_BTN_CLS: OnceLock<ObjPtr> = OnceLock::new();
    ROW_BTN_CLS
        .get_or_init(|| {
            let name = CString::new("OhMyTabClipboardRowButton").unwrap();
            let superclass = class!(NSButton) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(mouseEntered:),
                row_button_mouse_entered as *mut c_void,
                types.as_ptr(),
            );
            objc_registerClassPair(cls);
            ObjPtr(cls)
        })
        .0
}

/// 悬停行按钮:选中该行并刷新高亮。
/// Hovering a row button: select it and refresh the highlight.
extern "C" fn row_button_mouse_entered(_self: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    // 重建期间派发的 enter 忽略(防无限递归,见 REBUILDING 注释)。
    // Ignore enters dispatched during a rebuild (prevents infinite recursion; see REBUILDING).
    if REBUILDING.load(Ordering::SeqCst) {
        return;
    }
    let idx: isize = unsafe { msg_send![_self as *mut AnyObject, tag] };
    if idx >= 0 {
        *PICKER_SELECTION.lock().unwrap() = idx as usize;
        unsafe { rebuild_rows() };
    }
}

/// 行点击(按钮 tag = 行索引)→ 粘贴该行。
/// Row click (button tag = row index) -> paste that row.
extern "C" fn handle_clipboard_row_click(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    let idx: isize = unsafe { msg_send![sender as *mut AnyObject, tag] };
    if idx >= 0 {
        paste_at(idx as usize);
    }
}

/// 粘贴指定索引的条目:关闭浮窗 + 写回剪贴板 + 模拟 Cmd+V。
/// Paste the entry at `idx`: close the picker + write back to the pasteboard + synthesize Cmd+V.
fn paste_at(idx: usize) {
    let text = {
        let hist = CLIP_HISTORY.lock().unwrap();
        hist.get(idx).cloned()
    };
    match text {
        Some(t) => {
            log_debug!("[clip] paste_at idx={}: \"{}\"", idx, truncate_line(&t, 20));
            // 必须先关闭浮窗再合成 Cmd+V:浮窗是 key window(NonactivatingPanel +
            // makeKeyWindow),此时合成键盘事件会被路由给浮窗所属的 app(我们自己),
            // 输入框收不到;orderOut 后面板失去 key,系统 key window 回归原应用,
            // 合成事件才能到达用户原来的输入框。
            // Close the picker BEFORE synthesizing Cmd+V: the panel is the key window
            // (NonactivatingPanel + makeKeyWindow), so a synthesized key event would be
            // routed to the panel's app (us) and never reach the input field; once ordered
            // out, the panel resigns key, the system key window returns to the previous app,
            // and the synthesized Cmd+V lands in the user's input field.
            hide_picker();
            unsafe {
                write_pasteboard_text(&t);
                // 合成 Cmd+V(keyDown + keyUp),post 到 session 层。
                // Synthesize Cmd+V (keyDown + keyUp), posted at the session level.
                let down = CGEventCreateKeyboardEvent(std::ptr::null(), VK_V, true);
                if !down.is_null() {
                    CGEventSetFlags(down, K_CG_EVENT_FLAG_MASK_COMMAND);
                    CGEventPost(K_CG_SESSION_EVENT_TAP, down);
                }
                let up = CGEventCreateKeyboardEvent(std::ptr::null(), VK_V, false);
                if !up.is_null() {
                    CGEventSetFlags(up, K_CG_EVENT_FLAG_MASK_COMMAND);
                    CGEventPost(K_CG_SESSION_EVENT_TAP, up);
                }
            }
            log_debug!("[clip] pasted entry {}", idx);
        }
        None => {
            log_debug!("[clip] paste index {} out of range", idx);
            hide_picker();
        }
    }
}

/// 方向键导航纯逻辑:↑(126)/↓(125) 返回新的选中索引(循环);其它键返回 None。
/// Pure arrow-key navigation: up (126) / down (125) return the next selection (wrapping);
/// any other key returns None.
fn nav_arrow(keycode: u16, sel: usize, hist_len: usize) -> Option<usize> {
    if hist_len == 0 {
        return None;
    }
    match keycode {
        126 => Some(if sel == 0 { hist_len - 1 } else { sel - 1 }),
        125 => Some(if sel + 1 >= hist_len { 0 } else { sel + 1 }),
        _ => None,
    }
}

/// 键盘导航:↑/↓ 选择,Enter 粘贴,Esc 关闭。
/// Keyboard navigation: up/down to select, Enter to paste, Esc to close.
extern "C" fn container_key_down(_self: *mut c_void, _cmd: Sel, event: *mut c_void) {
    unsafe {
        let keycode: u16 = msg_send![event as *mut AnyObject, keyCode];
        // 可选中范围是全部条目(超出可视部分靠滚动查看)。
        // The selectable range covers ALL entries (scrolling reveals the rest).
        let hist_len = CLIP_HISTORY.lock().unwrap().len();
        let mut sel = PICKER_SELECTION.lock().unwrap();
        match keycode {
            126 | 125 => {
                if let Some(next) = nav_arrow(keycode, *sel, hist_len) {
                    *sel = next;
                }
                let idx = *sel;
                drop(sel);
                refresh_selection(idx);
                // 滚动到选中行可见 / scroll the selection into view.
                if let Some(c) = *PICKER_CONTAINER.lock().unwrap() {
                    let y = PAD_Y + idx as f64 * ROW_H;
                    // scrollRectToVisible: 返回 BOOL('B')。
                    // scrollRectToVisible: returns BOOL ('B').
                    let _: bool = msg_send![
                        c.0,
                        scrollRectToVisible: NSRect::new(
                            NSPoint::new(0.0, y),
                            NSSize::new(1.0, ROW_H)
                        )
                    ];
                }
            }
            36 => {
                // Enter
                let idx = *sel;
                drop(sel);
                paste_at(idx);
            }
            53 => {
                // Esc
                drop(sel);
                hide_picker();
            }
            _ => {}
        }
    }
}

/// 更新选中高亮(重建行)。/ Refresh selection highlight (rebuild rows).
fn refresh_selection(_idx: usize) {
    unsafe {
        rebuild_rows();
    }
}

extern "C" fn container_accepts_first_responder(_self: *mut c_void, _cmd: Sel) -> bool {
    true
}

extern "C" fn picker_window_can_become_key(_self: *mut c_void, _cmd: Sel) -> bool {
    true
}

// ========== 文本/样式 helper ==========

/// 截断到单行显示(超出加省略号)。/ Truncate to a single display line.
fn truncate_line(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}

/// 行标题(attributed):选中 = 白字粗体,未选 = labelColor。
/// Row title (attributed): selected = white bold, unselected = labelColor.
unsafe fn make_row_attributed_title(title: &str, selected: bool) -> *mut AnyObject {
    let font: *mut AnyObject = if selected {
        msg_send![class!(NSFont), boldSystemFontOfSize: 13.0f64]
    } else {
        msg_send![class!(NSFont), systemFontOfSize: 13.0f64]
    };
    // 文字跟随系统明暗:玻璃背景会随桌面明暗变化,固定白色在浅色玻璃上不可读。
    // 选中行 = labelColor(系统文本色)+ 粗体,配合强调色背景块;未选中 = secondaryLabelColor。
    // Text follows the system appearance: the glass backdrop adapts to the desktop's
    // light/dark state, so fixed white becomes unreadable on light glass. The selected row
    // uses labelColor (system text color) + bold over an accent tile; unselected rows use
    // secondaryLabelColor.
    let color: *mut AnyObject = if selected {
        msg_send![class!(NSColor), labelColor]
    } else {
        msg_send![class!(NSColor), secondaryLabelColor]
    };
    let attrs: *mut AnyObject = msg_send![class!(NSMutableDictionary), alloc];
    let attrs: *mut AnyObject = msg_send![attrs, init];
    let font_key = make_nsstring("NSFont");
    let color_key = make_nsstring("NSColor");
    let _: () = msg_send![attrs, setObject: font, forKey: font_key];
    let _: () = msg_send![attrs, setObject: color, forKey: color_key];
    CFRelease(font_key as *const c_void);
    CFRelease(color_key as *const c_void);
    let ns_title = make_nsstring(title);
    let attr: *mut AnyObject = msg_send![class!(NSAttributedString), alloc];
    let attr: *mut AnyObject = msg_send![attr, initWithString: ns_title, attributes: attrs];
    CFRelease(ns_title as *const c_void);
    release_obj(attrs);
    attr
}

/// 行按钮的 target(响应 handleClipboardRowClick:)。
/// 单例:NSControl 的 setTarget: 是弱引用(不 retain),每次 rebuild 都 new 新实例会
/// 永久泄漏;进程内只创建一次,实例存活到进程结束,按钮弱引用它始终有效。
///
/// Target for row buttons (responds to handleClipboardRowClick:).
/// A singleton: NSControl's setTarget: is weak (no retain), so creating a new instance per
/// rebuild would leak forever; one instance per process lives until exit, and the buttons'
/// weak reference to it stays valid.
unsafe fn row_target() -> *mut AnyObject {
    static ROW_TARGET: OnceLock<ObjPtr> = OnceLock::new();
    ROW_TARGET
        .get_or_init(|| {
            let name = CString::new("OhMyTabClipboardRowTarget").unwrap();
            let superclass = class!(NSObject) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(handleClipboardRowClick:),
                handle_clipboard_row_click as *mut c_void,
                types.as_ptr(),
            );
            objc_registerClassPair(cls);
            // 实例 alloc(+1):进程级单例,不释放(与静态生命周期一致)。
            // Instance alloc (+1): process-level singleton, never released (matches the
            // static's lifetime).
            let obj: *mut AnyObject = msg_send![cls as *const AnyObject, new];
            ObjPtr(obj)
        })
        .0
}

// ========== 测试 / tests ==========

/// --smoke-clipboard 入口(主线程调用):注入两条历史后连续两次显示/隐藏浮窗,
/// 覆盖 rebuild_rows 的行清理路径——这里曾是二次释放 UAF(第二次呼出 segfault)。
/// 成功返回 true;崩溃(panic/segfault)即失败。
///
/// --smoke-clipboard entry (called on the main thread): inject two entries, then show/hide
/// the picker twice to exercise rebuild_rows' row-cleanup path -- the site of a double-release
/// UAF that once segfaulted on the second summon. Returns true on success; a crash is a failure.
pub(crate) fn smoke_runner() -> bool {
    {
        let mut hist = CLIP_HISTORY.lock().unwrap();
        // 注入 12 条:超出可视行数(10),覆盖滚动文档(NSScrollView)路径。
        // Inject 12 entries: more than the visible rows (10), covering the scroll-document
        // (NSScrollView) path.
        for i in 0..12 {
            record_text(&mut hist, &format!("smoke entry {i:02}"), 50);
        }
    }
    show_picker();
    hide_picker();
    // 第二次显示:rebuild_rows 会先移除旧行(曾经的 UAF 路径)。
    // Second show: rebuild_rows removes the old rows first (the former UAF path).
    show_picker();
    // 键盘导航冒烟:构造真实 NSEvent 走 container_key_down,覆盖方向键 → 选中 → 滚动
    // 到可见的完整路径(曾因 scrollRectToVisible: 返回类型编码错误 panic)。
    // Keyboard-navigation smoke: build a real NSEvent and drive container_key_down, covering
    // arrow -> select -> scroll-into-view (once panicked on a wrong return-type encoding for
    // scrollRectToVisible:).
    unsafe {
        // 先取指针再进块:if let 的 scrutinee 临时 MutexGuard 存活到块结束,块内调用
        // container_key_down → rebuild_rows 会重锁 PICKER_CONTAINER,同线程非重入
        // Mutex 直接自死锁(曾导致冒烟挂起;sample 采样确认栈停在 rebuild_rows 的 lock)。
        // Take the pointer first, then enter the block: the if-let scrutinee's temporary
        // MutexGuard lives until the block ends, and container_key_down -> rebuild_rows
        // re-locks PICKER_CONTAINER inside the block -- a self-deadlock on the same thread
        // (the smoke run used to hang; sample confirmed the stack stuck in rebuild_rows' lock).
        let c_opt = *PICKER_CONTAINER.lock().unwrap();
        if let Some(c) = c_opt {
            let ev = make_key_event(125); // ↓ / down arrow
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
            let ev2 = make_key_event(126); // ↑ / up arrow
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev2 as *mut c_void);
        }
    }
    hide_picker();
    true
}

/// 构造一个方向键 NSEvent(冒烟用)。/ Build an arrow-key NSEvent (for the smoke run).
unsafe fn make_key_event(keycode: u16) -> *mut AnyObject {
    let chars = make_nsstring("x");
    // keyEventWithType: 参数依次为 NSEventType(unsigned long)、location、modifierFlags、
    // timestamp、windowNumber(NSInteger)、context、characters、charactersIgnoringModifiers、
    // isARepeat、keyCode(unsigned short)。
    // keyEventWithType: takes NSEventType (unsigned long), location, modifierFlags, timestamp,
    // windowNumber (NSInteger), context, characters, charactersIgnoringModifiers, isARepeat,
    // keyCode (unsigned short).
    let ev: *mut AnyObject = msg_send![
        class!(NSEvent),
        keyEventWithType: 10u64,
        location: NSPoint::new(0.0, 0.0),
        modifierFlags: 0u64,
        timestamp: 0.0f64,
        windowNumber: 0isize,
        context: std::ptr::null::<AnyObject>(),
        characters: chars,
        charactersIgnoringModifiers: chars,
        isARepeat: false,
        keyCode: keycode
    ];
    CFRelease(chars as *const c_void);
    ev
}

#[cfg(test)]
mod tests {
    use super::record_text;

    #[test]
    fn empty_text_is_ignored() {
        let mut h = vec!["a".to_string()];
        assert!(!record_text(&mut h, "", 50));
        assert_eq!(h.len(), 1);
    }

    #[test]
    fn duplicate_of_top_is_skipped() {
        // 连续复制同一内容:去重,不重复入栈。
        // Re-copying the same text: dedup, no duplicate entries.
        let mut h = vec!["a".to_string()];
        assert!(!record_text(&mut h, "a", 50));
        assert_eq!(h.len(), 1);
        // 与栈顶不同的重复(历史中间的同内容)正常入栈。
        // A duplicate that isn't the top (same content deeper in history) is recorded normally.
        record_text(&mut h, "b", 50);
        record_text(&mut h, "a", 50);
        assert_eq!(h, vec!["a".to_string(), "b".to_string(), "a".to_string()]);
    }

    #[test]
    fn newest_goes_first() {
        let mut h = Vec::new();
        record_text(&mut h, "first", 50);
        record_text(&mut h, "second", 50);
        assert_eq!(h, vec!["second".to_string(), "first".to_string()]);
    }

    #[test]
    fn overflow_is_trimmed() {
        // 超过上限裁剪最旧条目。
        // Entries beyond the cap are trimmed from the tail.
        let mut h = Vec::new();
        for i in 0..5 {
            record_text(&mut h, &format!("item{i}"), 3);
        }
        assert_eq!(h.len(), 3);
        assert_eq!(h[0], "item4");
        assert_eq!(h[2], "item2");
    }

    #[test]
    fn zero_max_records_nothing() {
        let mut h = Vec::new();
        assert!(!record_text(&mut h, "x", 0));
        assert!(h.is_empty());
    }

    #[test]
    fn truncate_line_keeps_short_and_ellipsizes_long() {
        assert_eq!(super::truncate_line("short", 10), "short");
        assert_eq!(super::truncate_line("abcdef", 3), "abc…");
    }

    #[test]
    fn nav_arrow_moves_and_wraps() {
        use super::nav_arrow;
        // ↓(125)前进,↑(126)后退,循环。
        // Down advances, up retreats, wrapping at both ends.
        assert_eq!(nav_arrow(125, 0, 3), Some(1));
        assert_eq!(nav_arrow(125, 2, 3), Some(0)); // 到底回顶 / wraps to top
        assert_eq!(nav_arrow(126, 2, 3), Some(1));
        assert_eq!(nav_arrow(126, 0, 3), Some(2)); // 到顶回底 / wraps to bottom
                                                   // 其它键不处理;空历史不动。
                                                   // Other keys are ignored; an empty history never moves.
        assert_eq!(nav_arrow(36, 1, 3), None);
        assert_eq!(nav_arrow(125, 0, 0), None);
    }

    // ========== 冒烟测试(需要真实 GUI 会话,手动运行)==========
    // ========== Smoke test (needs a real GUI session; run manually) ==========
    // 运行:先 cargo build,再 cargo test -- --ignored
    //
    // 以子进程方式调用真实 app 二进制(--smoke-clipboard):AppKit 控件构建严格要求主线程,
    // 测试 harness 的工作线程会被主线程限制拦下,必须用真实进程。两次 show_picker 覆盖
    // rebuild_rows 的行清理路径(曾二次释放 UAF,第二次呼出 segfault)。
    //
    // Runs the real app binary as a subprocess (--smoke-clipboard): AppKit control construction
    // is strictly main-thread-only, so the test harness's worker threads can't build the picker.
    // Two show_picker calls exercise rebuild_rows' row cleanup (a double-release UAF that once
    // segfaulted on the second summon).
    #[test]
    #[ignore]
    fn picker_rebuild_smoke() {
        // 前置条件:cargo build 已生成 target/debug/oh-my-tab。
        // Prerequisite: cargo build has produced target/debug/oh-my-tab.
        let exe = std::env::current_exe().expect("current exe");
        let app = exe
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.join("oh-my-tab"))
            .expect("app binary path");
        assert!(
            app.exists(),
            "app binary missing at {}: run `cargo build` first",
            app.display()
        );
        let out = std::process::Command::new(&app)
            .arg("--smoke-clipboard")
            .output()
            .expect("failed to spawn app");
        assert!(
            out.status.success(),
            "clipboard picker smoke failed (exit {:?})\nstderr:\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
