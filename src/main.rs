mod window_collector;
mod event_monitor;
mod config;

use config::{reload_config, CONFIG};
use flume;
use objc2::{class, msg_send, sel};
use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::collections::{HashMap, HashSet};
use std::ffi::{c_char, c_void, CString};
use std::sync::{LazyLock, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use window_collector::{
    MruMap, WindowInfo, cache_running_app_icons, ensure_icon_cache_dir,
    extract_icon_to_cache, note_app_activated, raise_ax_window,
};
use event_monitor::{GlobalEvent, start as start_event_monitor};

// ========== FFI ==========

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithCString(
        alloc: *const c_void,
        c_str: *const c_char,
        encoding: u32,
    ) -> *const c_void;
    fn CFRelease(cf: *const c_void);
    fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, return_after_source_handled: u8) -> i32;
    static kCFRunLoopDefaultMode: *mut c_void;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

#[link(name = "AppKit", kind = "framework")]
extern "C" {}

#[link(name = "objc", kind = "dylib")]
extern "C" {
    fn objc_allocateClassPair(
        superclass: *mut AnyObject,
        name: *const c_char,
        extra_bytes: usize,
    ) -> *mut AnyObject;
    fn objc_registerClassPair(cls: *mut AnyObject);
    fn class_addMethod(
        cls: *mut AnyObject,
        name: Sel,
        imp: *mut c_void,
        types: *const c_char,
    ) -> bool;
}

// ========== Keyboard Key Codes ==========

const KEY_TAB: u16 = 48;
const KEY_LEFT: u16 = 123;
const KEY_RIGHT: u16 = 124;
const KEY_DOWN: u16 = 125;
const KEY_UP: u16 = 126;
const KEY_ESCAPE: u16 = 53;
const KEY_RETURN: u16 = 36;

// ========== Layout Constants ==========

const STATUS_H: f64 = 36.0;
const H_PADDING: f64 = 32.0; // horizontal padding inside the window

// ========== Types ==========

struct MenuState {
    item: *mut AnyObject,
    is_dark: bool,
}
unsafe impl Send for MenuState {}
unsafe impl Sync for MenuState {}

struct ShortcutState {
    item: *mut AnyObject,
}
unsafe impl Send for ShortcutState {}
unsafe impl Sync for ShortcutState {}

struct AppState {
    windows: Vec<WindowInfo>,
    selected: usize,
    visible: bool,
    mru: MruMap,
}

impl AppState {
    fn new() -> Self {
        let mut mru = MruMap::new();
        let windows = if has_accessibility_permission() {
            window_collector::collect_windows(&mut mru)
        } else {
            Vec::new()
        };
        if !has_accessibility_permission() {
            println!("[oh-my-tab] WARNING: No accessibility permission.");
            println!("[oh-my-tab] Go to System Settings → Privacy & Security → Accessibility");
        }
        let win_count = windows.len();
        AppState {
            windows,
            selected: if win_count > 1 { 1 } else { 0 },
            visible: false,
            mru,
        }
    }

    fn refresh(&mut self) {
        self.windows = window_collector::collect_windows(&mut self.mru);
        if !self.windows.is_empty() && self.selected >= self.windows.len() {
            self.selected = self.windows.len() - 1;
        }
        if self.windows.is_empty() {
            self.visible = false;
        }
    }
}

#[allow(dead_code)]
struct Colors {
    page_bg: u32,
    hint_bg: u32,
    hint_text: u32,
    hint_subtext: u32,
    status_bar_bg: u32,
    status_bar_text: u32,
    card_bg: u32,
    card_bg_sel: u32,
    card_border_sel: u32,
    icon_inner_bg: u32,
    icon_text: u32,
    app_name: u32,
    win_title: u32,
}

// ========== Send+Sync Wrappers for Raw ObjC Pointers ==========

/// Thread-safe wrapper for raw ObjC object pointers.
/// All accesses are guarded by a Mutex — only Send/Sync for static storage.
#[derive(Clone, Copy)]
struct ObjPtr(*mut AnyObject);
unsafe impl Send for ObjPtr {}
unsafe impl Sync for ObjPtr {}

/// Thread-safe wrapper for raw ObjC class pointers.
#[derive(Clone, Copy)]
struct ObjClassPtr(*const objc2::runtime::AnyClass);
unsafe impl Send for ObjClassPtr {}
unsafe impl Sync for ObjClassPtr {}

// ========== Global State ==========

static TAB_STATE: Mutex<Option<AppState>> = Mutex::new(None);
static CONTROLLER: Mutex<Option<ObjPtr>> = Mutex::new(None);
static OVERLAY_WINDOW: Mutex<Option<ObjPtr>> = Mutex::new(None);
static CONTAINER: Mutex<Option<ObjPtr>> = Mutex::new(None);
static STATUS_LABEL: Mutex<Option<ObjPtr>> = Mutex::new(None);
static CARD_CLASS: Mutex<Option<ObjClassPtr>> = Mutex::new(None);
/// Maps card view pointer (as usize) → card index, avoiding property accessor
/// msg_send! issues on dynamically-registered ObjC classes.
static CARD_INDEX_MAP: LazyLock<Mutex<HashMap<usize, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static THEME_STATE: Mutex<Option<MenuState>> = Mutex::new(None);
static SHORTCUT_ITEM: Mutex<Option<ShortcutState>> = Mutex::new(None);
static STATUS_EVENT_TX: std::sync::OnceLock<flume::Sender<GlobalEvent>> =
    std::sync::OnceLock::new();
/// Prevents hover-selection on the card under the cursor when the window first
/// opens. Set to false in show_overlay(), flipped to true on first mouseMoved:.
static MOUSE_MOVED: AtomicBool = AtomicBool::new(false);

// ========== Helper Functions ==========

fn make_nsstring(s: &str) -> *mut AnyObject {
    unsafe {
        let c_str = CString::new(s).unwrap();
        let cf = CFStringCreateWithCString(std::ptr::null(), c_str.as_ptr(), 0x08000100u32);
        if cf.is_null() {
            eprintln!("[oh-my-tab] ERROR: CFStringCreateWithCString failed for '{}'", s);
        }
        cf as *mut AnyObject
    }
}

/// 释放 alloc 出来的 +1 对象。objc2 的 msg_send! 是裸 MRC（无 ARC）：
/// alloc/init 返回 +1，必须手动 release；addSubview:/setImage:/addTrackingArea:
/// 只是再加自己的 retain，不会抵消 alloc 的那 +1。交给父视图/子视图持有后即可 release。
/// Release a +1 object obtained via alloc. objc2's msg_send! is raw MRC (no ARC):
/// alloc/init return +1 and must be released; addSubview:/setImage:/addTrackingArea:
/// only add their own retain and don't balance the alloc +1. Once the owning view
/// retains it, we drop our alloc +1.
unsafe fn release_obj(obj: *mut AnyObject) {
    if !obj.is_null() {
        let _: () = msg_send![obj, release];
    }
}

fn has_accessibility_permission() -> bool {
    unsafe { AXIsProcessTrusted() }
}

fn hex_to_ns_color(hex: u32) -> *mut AnyObject {
    let r = ((hex >> 24) & 0xFF) as f64 / 255.0;
    let g = ((hex >> 16) & 0xFF) as f64 / 255.0;
    let b = ((hex >> 8) & 0xFF) as f64 / 255.0;
    let a = (hex & 0xFF) as f64 / 255.0;
    unsafe { msg_send![class!(NSColor), colorWithRed: r, green: g, blue: b, alpha: a] }
}

/// Convert hex u32 → CGColorRef for use with CALayer.setBackgroundColor / setBorderColor.
/// Uses raw objc_msgSend because objc2's msg_send! doesn't handle CF/CG types.
fn hex_to_cg_color(hex: u32) -> *mut c_void {
    let ns = hex_to_ns_color(hex);
    unsafe {
        let sel = sel!(CGColor);
        extern "C" {
            fn objc_msgSend();
        }
        type F = unsafe extern "C" fn(*mut c_void, Sel) -> *mut c_void;
        let f: F = std::mem::transmute(objc_msgSend as *const ());
        f(ns as *mut c_void, sel)
    }
}

/// Set CALayer.backgroundColor using raw objc_msgSend (CGColorRef, not NSColor*).
unsafe fn layer_set_background(layer: *mut AnyObject, cg: *mut c_void) {
    let sel = sel!(setBackgroundColor:);
    extern "C" {
        fn objc_msgSend();
    }
    type F = unsafe extern "C" fn(*mut c_void, Sel, *mut c_void);
    let f: F = std::mem::transmute(objc_msgSend as *const ());
    f(layer as *mut c_void, sel, cg);
}

/// Set CALayer.borderColor using raw objc_msgSend (CGColorRef, not NSColor*).
unsafe fn layer_set_border(layer: *mut AnyObject, cg: *mut c_void) {
    let sel = sel!(setBorderColor:);
    extern "C" {
        fn objc_msgSend();
    }
    type F = unsafe extern "C" fn(*mut c_void, Sel, *mut c_void);
    let f: F = std::mem::transmute(objc_msgSend as *const ());
    f(layer as *mut c_void, sel, cg);
}

fn colors_from_config(dark: bool) -> Colors {
    let cfg = CONFIG.read().unwrap();
    let c = if dark { &cfg.colors.dark } else { &cfg.colors.light };
    Colors {
        page_bg: 0x00000000,
        hint_bg: 0x00000000,
        hint_text: 0x888888ff,
        hint_subtext: 0x666666ff,
        status_bar_bg: 0x00000000,
        status_bar_text: config::parse_hex8(&c.status_bar_text),
        card_bg: 0x00000000,
        card_bg_sel: config::parse_hex8(&c.card_bg_sel),
        card_border_sel: config::parse_hex8(&c.card_border_sel),
        icon_inner_bg: config::parse_hex8(&c.icon_inner_bg),
        icon_text: config::parse_hex8(&c.icon_text),
        app_name: config::parse_hex8(&c.app_name),
        win_title: config::parse_hex8(&c.win_title),
    }
}

fn system_dark_mode() -> bool {
    unsafe {
        let key = make_nsstring("AppleInterfaceStyle");
        let defaults: *mut AnyObject = msg_send![class!(NSUserDefaults), standardUserDefaults];
        let style: *mut AnyObject = msg_send![defaults, stringForKey: key];
        CFRelease(key as *const c_void);
        !style.is_null()
    }
}

fn current_colors() -> Colors {
    let is_dark = match CONFIG.read().unwrap().appearance.theme.as_str() {
        "light" => false,
        "dark" => true,
        _ => system_dark_mode(),
    };
    colors_from_config(is_dark)
}

// Layout helpers that read from CONFIG at runtime.
fn cards_per_row() -> usize { CONFIG.read().unwrap().layout.cards_per_row }
fn card_w() -> f64 { CONFIG.read().unwrap().layout.card_width }
fn card_h() -> f64 { CONFIG.read().unwrap().layout.card_height }
fn card_gap() -> f64 { CONFIG.read().unwrap().layout.card_gap }
fn icon_px() -> f64 { CONFIG.read().unwrap().layout.icon_size }
fn letter_px() -> f64 { icon_px() * 0.5 }

fn window_height(count: usize) -> f64 {
    let cpr = cards_per_row();
    let rows = (count.max(1) + cpr - 1) / cpr;
    32.0 + rows as f64 * card_h() + STATUS_H
}

fn window_width(cards_in_row: usize) -> f64 {
    cards_in_row as f64 * card_w()
        + (cards_in_row.saturating_sub(1)) as f64 * card_gap()
        + H_PADDING * 2.0
}

/// Read the card index from the card index map (keyed by view pointer).
/// This avoids msg_send! encoding issues with property accessors on
/// dynamically-registered ObjC classes.
fn get_card_index(view: *mut AnyObject) -> usize {
    let map = CARD_INDEX_MAP.lock().unwrap();
    map.get(&(view as usize)).copied().unwrap_or(0)
}

fn set_card_index(view: *mut AnyObject, idx: usize) {
    let mut map = CARD_INDEX_MAP.lock().unwrap();
    map.insert(view as usize, idx);
}

fn remove_card_index(view: *mut AnyObject) {
    let mut map = CARD_INDEX_MAP.lock().unwrap();
    map.remove(&(view as usize));
}

fn clear_card_indices() {
    let mut map = CARD_INDEX_MAP.lock().unwrap();
    map.clear();
}

/// Create a simple (non-attributed) NSTextField label, size it to fit text,
/// then center it horizontally within `container_width`. Returns the label.
unsafe fn make_centered_label(
    text: &str,
    font: *mut AnyObject,
    color: *mut AnyObject,
    y: f64,
    container_width: f64,
    height: f64,
) -> *mut AnyObject {
    let ns_str = make_nsstring(text);
    // Create with a wide enough frame
    let init_frame = NSRect::new(NSPoint::new(0.0, y), NSSize::new(container_width, height));
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let label: *mut AnyObject = msg_send![label, initWithFrame: init_frame];
    let _: () = msg_send![label, setStringValue: ns_str];
    CFRelease(ns_str as *const c_void);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let _: () = msg_send![label, setSelectable: false];
    let _: () = msg_send![label, setFont: font];
    let _: () = msg_send![label, setTextColor: color];
    // Size to fit content, then center horizontally
    let _: () = msg_send![label, sizeToFit];
    let fitted: NSRect = msg_send![label, frame];
    let text_w = fitted.size.width;
    let center_x = ((container_width - text_w) / 2.0).max(0.0);
    let _: () = msg_send![label, setFrame: NSRect::new(NSPoint::new(center_x, y), NSSize::new(text_w, height))];
    label
}

fn truncate_text(text: &str, max_width: usize) -> String {
    let mut width: usize = 0;
    for (i, c) in text.char_indices() {
        let w = if c.is_ascii() { 1 } else { 2 };
        if width + w > max_width {
            let t: String = text[..i].chars().collect();
            return format!("{}…", t);
        }
        width += w;
    }
    text.to_string()
}

/// 占位符：窗口没有标题时（如 Microsoft To Do，AXTitle 为空）显示一个短横线。
/// 注意：仅用于显示。内部 `window_title` 仍保持空串，这样 raise_ax_window 仍能
/// 按空标题匹配到对应的 AX 窗口并聚焦。
/// Placeholder shown for windows that expose no title (e.g. Microsoft To Do,
/// whose custom title bar yields an empty AXTitle). Display-only: the internal
/// `window_title` stays empty so raise_ax_window can still match the AX window
/// by its empty title.
fn display_title(title: &str) -> String {
    if title.is_empty() {
        "-".to_string()
    } else {
        title.to_string()
    }
}

// ========== ObjC Method Implementations ==========

// --- Controller ---

extern "C" fn on_cmd_tab_pressed(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    let mut state_opt = TAB_STATE.lock().unwrap();
    let state = state_opt.as_mut().unwrap();

    if !state.visible {
        state.refresh();
        state.visible = true;
        state.selected = if state.windows.len() > 1 { 1 } else { 0 };
        drop(state_opt);
        show_overlay();
    } else {
        state.selected = (state.selected + 1) % state.windows.len().max(1);
        drop(state_opt);
        refresh_highlight();
        update_status_label();
        extract_uncached_icons();
    }
}

extern "C" fn on_cmd_released(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    let mut state_opt = TAB_STATE.lock().unwrap();
    let state = state_opt.as_mut().unwrap();
    if !state.visible {
        return;
    }

    if let Some(w) = state.windows.get(state.selected) {
        let pid = w.pid;
        let wid = w.window_id;
        let wt = w.window_title.clone();
        println!(
            "[oh-my-tab] Switching to '{}' (pid={})",
            w.app_name, pid
        );
        hide_overlay();
        activate_pid(pid);
        raise_ax_window(pid, &wt);
        state.mru.insert(wid, std::time::Instant::now());
    } else {
        eprintln!(
            "[oh-my-tab] CmdReleased: selected index {} out of bounds (windows={})",
            state.selected,
            state.windows.len()
        );
    }
    state.visible = false;
}

extern "C" fn on_theme_toggled(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    apply_theme();
}

extern "C" fn on_app_activated(_self: *mut c_void, _cmd: Sel, notification: *mut c_void) {
    unsafe {
        let user_info: *mut AnyObject = msg_send![notification as *mut AnyObject, userInfo];
        if user_info.is_null() { return; }
        let key = make_nsstring("NSWorkspaceApplicationKey");
        let app: *mut AnyObject = msg_send![user_info, objectForKey: key];
        CFRelease(key as *const c_void);
        if app.is_null() { return; }
        let pid: i32 = msg_send![app, processIdentifier];
        note_app_activated(pid);
    }
}

extern "C" fn on_app_launched(_self: *mut c_void, _cmd: Sel, notification: *mut c_void) {
    // Pre-cache the launched app's icon so it's on disk before the user summons
    // the switcher. Run off the main thread (with an autorelease pool) so the
    // launch notification doesn't block the UI; extract_icon_to_cache is
    // defensive (null-safe) and a failure simply leaves the letter-icon fallback.
    let pid: i32 = unsafe {
        let user_info: *mut AnyObject = msg_send![notification as *mut AnyObject, userInfo];
        if user_info.is_null() { return; }
        let key = make_nsstring("NSWorkspaceApplicationKey");
        let app: *mut AnyObject = msg_send![user_info, objectForKey: key];
        CFRelease(key as *const c_void);
        if app.is_null() { return; }
        msg_send![app, processIdentifier]
    };
    if pid <= 0 { return; }
    thread::spawn(move || unsafe {
        let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
        let _ = extract_icon_to_cache(pid);
        let _: () = msg_send![pool, drain];
    });
}

// --- Card View ---

extern "C" fn card_mouse_down(_self: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    let idx = get_card_index(_self as *mut AnyObject);
    let mut state_opt = TAB_STATE.lock().unwrap();
    let state = state_opt.as_mut().unwrap();
    if let Some(w) = state.windows.get(idx) {
        let pid = w.pid;
        let wid = w.window_id;
        let wt = w.window_title.clone();
        hide_overlay();
        activate_pid(pid);
        raise_ax_window(pid, &wt);
        state.mru.insert(wid, std::time::Instant::now());
        state.visible = false;
    }
}

extern "C" fn card_mouse_entered(_self: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    // Ignore hover until the user has moved the mouse at least once.
    // Prevents selecting the card under the cursor when the window first opens.
    if !MOUSE_MOVED.load(Ordering::Relaxed) {
        return;
    }
    let idx = get_card_index(_self as *mut AnyObject);
    let mut state_opt = TAB_STATE.lock().unwrap();
    let state = state_opt.as_mut().unwrap();
    if state.selected != idx {
        state.selected = idx;
        drop(state_opt);
        refresh_highlight();
        update_status_label();
    }
}

// --- Container View ---

extern "C" fn container_key_down(_self: *mut c_void, _cmd: Sel, event: *mut c_void) {
    unsafe {
        let key_code: u16 = msg_send![event as *mut AnyObject, keyCode];
        let mut state_opt = TAB_STATE.lock().unwrap();
        let state = state_opt.as_mut().unwrap();

        if !state.visible {
            return;
        }

        match key_code {
            KEY_TAB | KEY_RIGHT => {
                if !state.windows.is_empty() {
                    state.selected = (state.selected + 1) % state.windows.len();
                    drop(state_opt);
                    refresh_highlight();
                    update_status_label();
                    return;
                }
            }
            KEY_LEFT => {
                if !state.windows.is_empty() {
                    state.selected = if state.selected == 0 {
                        state.windows.len() - 1
                    } else {
                        state.selected - 1
                    };
                    drop(state_opt);
                    refresh_highlight();
                    update_status_label();
                    return;
                }
            }
            KEY_UP => {
                if !state.windows.is_empty() {
                    if state.selected >= cards_per_row() {
                        state.selected -= cards_per_row();
                        drop(state_opt);
                        refresh_highlight();
                        update_status_label();
                    }
                    return;
                }
            }
            KEY_DOWN => {
                if !state.windows.is_empty() {
                    let new_idx = state.selected + cards_per_row();
                    if new_idx < state.windows.len() {
                        state.selected = new_idx;
                        drop(state_opt);
                        refresh_highlight();
                        update_status_label();
                    }
                    return;
                }
            }
            KEY_RETURN => {
                if let Some(w) = state.windows.get(state.selected) {
                    let pid = w.pid;
                    let wid = w.window_id;
                    let wt = w.window_title.clone();
                    hide_overlay();
                    activate_pid(pid);
                    raise_ax_window(pid, &wt);
                    state.mru.insert(wid, std::time::Instant::now());
                }
                state.visible = false;
            }
            KEY_ESCAPE => {
                state.visible = false;
                hide_overlay();
            }
            _ => {}
        }
    }
}

extern "C" fn container_accepts_first_responder(_self: *mut c_void, _cmd: Sel) -> bool {
    true
}

extern "C" fn container_mouse_moved(_self: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    MOUSE_MOVED.store(true, Ordering::Relaxed);
}

// ========== Status Bar Menu Handlers ==========

extern "C" fn handle_quit(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    println!("[oh-my-tab] User quit via menu bar.");
    unsafe {
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![nsapp, terminate: std::ptr::null::<AnyObject>()];
    }
}

extern "C" fn handle_toggle_shortcut(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    let old = event_monitor::SHORTCUT_IS_CMD.load(Ordering::SeqCst);
    let is_cmd = !old;
    event_monitor::SHORTCUT_IS_CMD.store(is_cmd, Ordering::SeqCst);
    let new_label = if is_cmd {
        "切换opt+tab"
    } else {
        "切换cmd+tab"
    };
    println!(
        "[oh-my-tab] Shortcut: {}",
        if is_cmd { "Cmd+Tab" } else { "Opt+Tab" }
    );
    if let Some(ref s) = *SHORTCUT_ITEM.lock().unwrap() {
        unsafe {
            let ns_title = make_nsstring(new_label);
            let _: () = msg_send![s.item, setTitle: ns_title];
            CFRelease(ns_title as *const c_void);
        }
    }
}

extern "C" fn handle_toggle_theme(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    // Flip theme in CONFIG and persist to file so menu ↔ config are linked.
    let new_theme = match CONFIG.read().unwrap().appearance.theme.as_str() {
        "dark" => "light",
        _ => "dark",
    };
    {
        let mut cfg = CONFIG.write().unwrap();
        cfg.appearance.theme = new_theme.to_string();
        // Save to file
        let path = {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            std::path::PathBuf::from(home).join(".config/oh-my-tab/config.toml")
        };
        if let Ok(toml_str) = toml::to_string_pretty(&*cfg) {
            let _ = std::fs::write(&path, toml_str);
        }
    }
    let is_dark = new_theme == "dark";
    let new_label = if is_dark { "切换浅色" } else { "切换深色" };
    println!(
        "[oh-my-tab] Toggled theme to {}",
        if is_dark { "dark" } else { "light" }
    );
    // Update menu item title
    let mut state = THEME_STATE.lock().unwrap();
    if let Some(ref mut s) = *state {
        s.is_dark = is_dark;
        unsafe {
            let ns_title = make_nsstring(new_label);
            let _: () = msg_send![s.item, setTitle: ns_title];
            CFRelease(ns_title as *const c_void);
        }
    }
    drop(state);
    if let Some(tx) = STATUS_EVENT_TX.get() {
        let _ = tx.send(GlobalEvent::ThemeToggled);
    }
}

extern "C" fn handle_reload_config(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    let errs = reload_config();
    if errs.is_empty() {
        println!("[oh-my-tab] Config reloaded successfully.");
    } else {
        eprintln!("[oh-my-tab] Config reload: {} error(s):", errs.len());
        for e in &errs {
            eprintln!("[oh-my-tab]   • {}", e);
        }
    }
    // Sync menu label with new config theme
    let is_dark = CONFIG.read().unwrap().appearance.theme.as_str() != "light";
    let new_label = if is_dark { "切换浅色" } else { "切换深色" };
    let mut state = THEME_STATE.lock().unwrap();
    if let Some(ref mut s) = *state {
        s.is_dark = is_dark;
        unsafe {
            let ns_title = make_nsstring(new_label);
            let _: () = msg_send![s.item, setTitle: ns_title];
            CFRelease(ns_title as *const c_void);
        }
    }
    drop(state);
    // Apply immediately
    apply_theme();
    refresh_highlight();
    update_status_label();
}

// ========== UI Functions ==========

fn activate_pid(pid: i32) {
    unsafe {
        let app: *mut AnyObject =
            msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid];
        if !app.is_null() {
            let _: bool = msg_send![app, activateWithOptions: 1usize];
        } else {
            eprintln!("[oh-my-tab] activate_pid: no running app for pid {}", pid);
        }
    }
}

fn update_status_label() {
    unsafe {
        let status_label = match *STATUS_LABEL.lock().unwrap() {
            Some(l) => l.0,
            None => return,
        };
        let state_opt = TAB_STATE.lock().unwrap();
        let state = match state_opt.as_ref() {
            Some(s) => s,
            None => return,
        };
        let selected = state.selected;
        // status_text 是窗口下面那一行长的应用名称
        let status_text = match state.windows.get(selected) {
            Some(w) => truncate_text(&display_title(&w.window_title), 126),
            None => String::new(),
        };
        drop(state_opt);

        let colors = current_colors();
        let status_font: *mut AnyObject =
            {
    let cfg = CONFIG.read().unwrap();
    msg_send![class!(NSFont), systemFontOfSize: cfg.fonts.status_bar_size, weight: cfg.fonts.status_bar_weight]
};
        let status_color = hex_to_ns_color(colors.status_bar_text);
        let ns_stat = make_nsstring(&status_text);
        let _: () = msg_send![status_label, setStringValue: ns_stat];
        CFRelease(ns_stat as *const c_void);
        let _: () = msg_send![status_label, setFont: status_font];
        let _: () = msg_send![status_label, setTextColor: status_color];
        // Size to fit + recenter horizontally
        let _: () = msg_send![status_label, sizeToFit];
        let fitted: NSRect = msg_send![status_label, frame];
        let stat_w = fitted.size.width;
        let container_w = {
            let container = CONTAINER.lock().unwrap();
            let c = container.unwrap().0;
            let f: NSRect = msg_send![c, frame];
            f.size.width
        };
        let stat_x = ((container_w - stat_w) / 2.0).max(0.0);
        let _: () = msg_send![status_label, setFrame: NSRect::new(NSPoint::new(stat_x, 0.0), NSSize::new(stat_w, STATUS_H))];
    }
}

fn hide_overlay() {
    unsafe {
        if let Some(window) = *OVERLAY_WINDOW.lock().unwrap() {
            let _: () = msg_send![window.0, orderOut: std::ptr::null::<AnyObject>()];
        }
    }
}

fn refresh_highlight() {
    unsafe {
        let container = match *CONTAINER.lock().unwrap() {
            Some(c) => c.0,
            None => return,
        };
        let state_opt = TAB_STATE.lock().unwrap();
        let state = match state_opt.as_ref() {
            Some(s) => s,
            None => return,
        };
        if !state.visible {
            return;
        }
        let selected = state.selected;
        let colors = current_colors();
        let sel_color = hex_to_cg_color(colors.card_border_sel);

        let subviews: *mut AnyObject = msg_send![container, subviews];
        let sv_count: usize = msg_send![subviews, count];

        for i in 0..sv_count {
            let sv: *mut AnyObject = msg_send![subviews, objectAtIndex: i];
            // Only operate on card views (skip status label which is NSTextField)
            let is_nstextfield: bool = msg_send![sv, isKindOfClass: class!(NSTextField)];
            if is_nstextfield {
                continue;
            }
            let layer: *mut AnyObject = msg_send![sv, layer];
            let tag = get_card_index(sv);
            if tag == selected {
                let _: () = msg_send![layer, setBorderWidth: 3.0f64];
                layer_set_border(layer, sel_color);
            } else {
                let _: () = msg_send![layer, setBorderWidth: 0.0f64];
                layer_set_border(layer, std::ptr::null_mut());
            }
        }
    }
}

fn extract_uncached_icons() {
    let uncached: Vec<i32> = {
        let state_opt = TAB_STATE.lock().unwrap();
        if let Some(ref state) = *state_opt {
            state
                .windows
                .iter()
                .filter(|w| w.icon_path.is_none())
                .map(|w| w.pid)
                .collect::<HashSet<_>>()
                .into_iter()
                .collect()
        } else {
            return;
        }
    };

    // Record which window indices got a freshly cached icon so we can re-render
    // just those cards in place (otherwise the on-screen letter icons wouldn't
    // update until the next summon).
    let mut updated_indices: Vec<usize> = Vec::new();
    for pid in uncached {
        if let Some(ref path) = extract_icon_to_cache(pid) {
            let path = path.clone();
            let mut state_opt = TAB_STATE.lock().unwrap();
            if let Some(ref mut state) = *state_opt {
                for (i, w) in state.windows.iter_mut().enumerate() {
                    if w.pid == pid && w.icon_path.is_none() {
                        w.icon_path = Some(path.clone());
                        updated_indices.push(i);
                    }
                }
            }
        }
    }

    if !updated_indices.is_empty() {
        rebuild_cards(&updated_indices);
    }
}

/// Rebuild the card views for the given window indices in place, so newly
/// extracted icons appear immediately without re-summoning. Each affected card
/// is replaced by a fresh one built from the updated `WindowInfo` (which now has
/// an icon_path), preserving its frame and card index.
fn rebuild_cards(indices: &[usize]) {
    if indices.is_empty() {
        return;
    }
    let affected: HashSet<usize> = indices.iter().copied().collect();
    let to_rebuild: HashMap<usize, WindowInfo> = {
        let state_opt = TAB_STATE.lock().unwrap();
        let state = match state_opt.as_ref() {
            Some(s) => s,
            None => return,
        };
        if !state.visible {
            return;
        }
        affected
            .iter()
            .filter_map(|&i| state.windows.get(i).map(|w| (i, w.clone())))
            .collect()
    };
    if to_rebuild.is_empty() {
        return;
    }

    unsafe {
        let container = match *CONTAINER.lock().unwrap() {
            Some(c) => c.0,
            None => return,
        };
        let subviews: *mut AnyObject = msg_send![container, subviews];
        let sv_count: usize = msg_send![subviews, count];

        // Collect affected card views + their frames first; don't mutate the
        // subview array while iterating it.
        let mut replacements: Vec<(*mut AnyObject, NSRect, usize)> = Vec::new();
        for i in 0..sv_count {
            let sv: *mut AnyObject = msg_send![subviews, objectAtIndex: i];
            let is_label: bool = msg_send![sv, isKindOfClass: class!(NSTextField)];
            if is_label {
                continue;
            }
            let idx = get_card_index(sv);
            if to_rebuild.contains_key(&idx) {
                let frame: NSRect = msg_send![sv, frame];
                replacements.push((sv, frame, idx));
            }
        }

        for (old_view, frame, idx) in replacements {
            if let Some(w) = to_rebuild.get(&idx) {
                remove_card_index(old_view);
                let new_card = create_card_view(w, idx);
                let _: () = msg_send![new_card, setFrame: frame];
                let _: () = msg_send![old_view, removeFromSuperview];
                let _: () = msg_send![container, addSubview: new_card];
                release_obj(new_card); // container owns the card; drop create_card_view's alloc +1
            }
        }

        // New card views have no selection border; re-apply the highlight.
        refresh_highlight();
    }
}

fn apply_theme() {
    unsafe {
        let is_dark = THEME_STATE
            .lock()
            .unwrap()
            .as_ref()
            .map_or(false, |s| s.is_dark);

        // Update window appearance for blur material tint
        if let Some(window) = *OVERLAY_WINDOW.lock().unwrap() {
            let appearance_name = if is_dark {
                make_nsstring("NSAppearanceNameDarkAqua")
            } else {
                make_nsstring("NSAppearanceNameAqua")
            };
            let appearance: *mut AnyObject =
                msg_send![class!(NSAppearance), appearanceNamed: appearance_name];
            CFRelease(appearance_name as *const c_void);
            if !appearance.is_null() {
                let _: () = msg_send![window.0, setAppearance: appearance];
            }
        }

        refresh_highlight();
    }
}

fn create_card_view(w: &WindowInfo, index: usize) -> *mut AnyObject {
    unsafe {
        let card_cls = CARD_CLASS.lock().unwrap().unwrap();
        let card_cls_ptr = card_cls.0 as *mut AnyObject;

        let frame = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(card_w(), card_h()),
        );
        let view: *mut AnyObject = msg_send![card_cls_ptr, alloc];
        let view: *mut AnyObject = msg_send![view, initWithFrame: frame];

        // Enable layer for selection border
        let _: () = msg_send![view, setWantsLayer: true];
        let layer: *mut AnyObject = msg_send![view, layer];
        let _: () = msg_send![layer, setCornerRadius: 24.0f64];
        let _: () = msg_send![layer, setMasksToBounds: true];

        // Store card index in side map (avoids msg_send! issues on dynamic classes)
        set_card_index(view, index);

        let colors = current_colors();
        let icon_x = (card_w() - icon_px()) / 2.0; // 16.0
        // Standard coords: y=0 at bottom, y=200 at top.
        // Icon: 8px from top → y = 200 - 8 - 128 = 64
        let icon_bottom = card_h() - 8.0 - icon_px(); // 64.0

        // --- Icon ---
        if let Some(ref icon_path) = w.icon_path {
            let ns_path = make_nsstring(icon_path);
            let ns_image: *mut AnyObject = msg_send![class!(NSImage), alloc];
            let ns_image: *mut AnyObject =
                msg_send![ns_image, initWithContentsOfFile: ns_path];
            CFRelease(ns_path as *const c_void);

            if !ns_image.is_null() {
                let img_frame = NSRect::new(
                    NSPoint::new(icon_x, icon_bottom),
                    NSSize::new(icon_px(), icon_px()),
                );
                let img_view: *mut AnyObject = msg_send![class!(NSImageView), alloc];
                let img_view: *mut AnyObject = msg_send![img_view, initWithFrame: img_frame];
                let _: () = msg_send![img_view, setImage: ns_image];
                release_obj(ns_image); // img_view owns the image now; drop our alloc +1
                // NSImageScaleProportionallyUpOrDown = 3
                let _: () = msg_send![img_view, setImageScaling: 3u64];
                let _: () = msg_send![view, addSubview: img_view];
                release_obj(img_view); // view owns the image view now; drop our alloc +1
            }
        } else {
            // Letter icon: rounded square with first letter
            let letter_sq = letter_px();
            let letter_x = icon_x + (icon_px() - letter_sq) / 2.0;
            // Center the 64x64 square within the 128x128 icon area
            let letter_y = icon_bottom + (icon_px() - letter_sq) / 2.0;
            let letter_frame = NSRect::new(
                NSPoint::new(letter_x, letter_y),
                NSSize::new(letter_sq, letter_sq),
            );

            let letter_view: *mut AnyObject = msg_send![class!(NSView), alloc];
            let letter_view: *mut AnyObject = msg_send![letter_view, initWithFrame: letter_frame];
            let _: () = msg_send![letter_view, setWantsLayer: true];
            let ll: *mut AnyObject = msg_send![letter_view, layer];
            let _: () = msg_send![ll, setCornerRadius: 14.0f64];
            let _: () = msg_send![ll, setMasksToBounds: true];
            let bg_color = hex_to_cg_color(colors.icon_inner_bg);
            layer_set_background(ll, bg_color);

            let init = w
                .app_name
                .chars()
                .next()
                .unwrap_or('?')
                .to_string();
            let font: *mut AnyObject =
                msg_send![class!(NSFont), systemFontOfSize: 28.0f64, weight: 0.4f64];
            let text_color = hex_to_ns_color(colors.icon_text);
            let label = make_centered_label(&init, font, text_color, 0.0, letter_sq, letter_sq);
            let _: () = msg_send![letter_view, addSubview: label];
            release_obj(label); // letter_view owns the label; drop our alloc +1
            let _: () = msg_send![view, addSubview: letter_view];
            release_obj(letter_view); // view owns the letter view; drop our alloc +1
        }

        // Gap below icon before text starts
        let text_gap: f64 = 6.0;
        // App name: 18px tall, 2px above window title
        let name_bottom = icon_bottom - text_gap - 18.0; // 64 - 6 - 18 = 40
        // Window title: 16px tall, sits at bottom
        let title_bottom = name_bottom - 2.0 - 16.0; // 40 - 2 - 16 = 22

        // --- App name label ---
        let name_font: *mut AnyObject =
            {
    let cfg = CONFIG.read().unwrap();
    msg_send![class!(NSFont), systemFontOfSize: cfg.fonts.app_name_size, weight: cfg.fonts.app_name_weight]
};
        let name_color = hex_to_ns_color(colors.app_name);
        let name_label = make_centered_label(
            &truncate_text(&w.app_name, 17), name_font, name_color,
            name_bottom, card_w(), 18.0,
        );
        let _: () = msg_send![view, addSubview: name_label];
        release_obj(name_label); // view owns the label; drop our alloc +1

        // --- Window title label ---
        let title_font: *mut AnyObject = {
    let cfg = CONFIG.read().unwrap();
    msg_send![class!(NSFont), systemFontOfSize: cfg.fonts.title_size, weight: cfg.fonts.title_weight]
};
        let win_color = hex_to_ns_color(colors.win_title);
        let title_label = make_centered_label(
            &truncate_text(&display_title(&w.window_title), 20), title_font, win_color,
            title_bottom, card_w(), 16.0,
        );
        let _: () = msg_send![view, addSubview: title_label];
        release_obj(title_label); // view owns the label; drop our alloc +1

        // --- Tracking area for hover ---
        // NSTrackingMouseEnteredAndExited | NSTrackingActiveInActiveApp
        let opts: u64 = 0x01 | 0x40;
        let ta: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
        let bounds = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(card_w(), card_h()),
        );
        let ta: *mut AnyObject = msg_send![ta, initWithRect: bounds, options: opts, owner: view, userInfo: std::ptr::null::<AnyObject>()];
        let _: () = msg_send![view, addTrackingArea: ta];
        release_obj(ta); // view owns the tracking area; drop our alloc +1

        view
    }
}

fn show_overlay() {
    unsafe {
        let state_opt = TAB_STATE.lock().unwrap();
        let state = state_opt.as_ref().unwrap();
        let count = state.windows.len();
        let windows = state.windows.clone();
        drop(state_opt);

        let window = OVERLAY_WINDOW.lock().unwrap().unwrap().0;
        let container = CONTAINER.lock().unwrap().unwrap().0;

        // Remove old card subviews (keep status label)
        let subviews: *mut AnyObject = msg_send![container, subviews];
        let sv_count: usize = msg_send![subviews, count];
        // Iterate in reverse since we're removing from the array
        let mut i = sv_count;
        while i > 0 {
            i -= 1;
            let sv: *mut AnyObject = msg_send![subviews, objectAtIndex: i];
            let is_label: bool = msg_send![sv, isKindOfClass: class!(NSTextField)];
            if !is_label {
                let _: () = msg_send![sv, removeFromSuperview];
            }
        }

        // Clear old card index mappings, then create new card views
        clear_card_indices();
        let h = window_height(count);
        let cards_in_row = cards_per_row().min(count);
        let w = window_width(cards_in_row);
        let row_width = cards_in_row as f64 * card_w()
            + (cards_in_row.saturating_sub(1)) as f64 * card_gap();
        let start_x = (w - row_width) / 2.0;

        for (idx, w) in windows.iter().enumerate() {
            let card = create_card_view(w, idx);

            // Standard coords: y=0 at bottom. Cards stack from top down.
            let col = idx % cards_per_row();
            let row = idx / cards_per_row();
            let card_x = start_x + col as f64 * (card_w() + card_gap());
            // topmost card origin_y = h - 32.0 - card_h() (32 = top padding area)
            let card_y = h - 32.0 - (row + 1) as f64 * card_h();
            let card_frame = NSRect::new(
                NSPoint::new(card_x, card_y),
                NSSize::new(card_w(), card_h()),
            );
            let _: () = msg_send![card, setFrame: card_frame];

            let _: () = msg_send![container, addSubview: card];
            release_obj(card); // container owns the card; drop create_card_view's alloc +1
        }

        update_status_label();

        // Resize window (h computed above)
        let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
        let screen_frame: NSRect = msg_send![screen, frame];
        let x = (screen_frame.size.width - w) / 2.0 + screen_frame.origin.x;
        let y = (screen_frame.size.height - h) / 2.0 + screen_frame.origin.y;
        let new_frame = NSRect::new(
            NSPoint::new(x, y),
            NSSize::new(w, h),
        );
        let _: () = msg_send![window, setFrame: new_frame, display: true];

        // wrapper / VFX view / container all have autoresizingMask = 18
        // (width + height sizable), so they resize automatically when the
        // window frame changes. Just update the container explicitly.
        let _: () = msg_send![container, setFrameSize: NSSize::new(w, h)];

        // Ignore initial mouse position — require a real mouse movement before
        // hover-selection kicks in (matches native Cmd+Tab behaviour).
        MOUSE_MOVED.store(false, Ordering::Relaxed);
        let _: () = msg_send![window, setAcceptsMouseMovedEvents: true];

        // Activate and show window
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![nsapp, activateIgnoringOtherApps: true];
        let _: () = msg_send![window, makeKeyAndOrderFront: std::ptr::null::<AnyObject>()];
        let _: bool = msg_send![window, makeFirstResponder: container];

        // Highlight selected card
        refresh_highlight();
    }
}

// ========== Class Registration ==========

fn register_classes() {
    unsafe {
        // --- OhMyTabCardView : NSView ---
        let card_cls = {
            let name = CString::new("OhMyTabCardView").unwrap();
            let superclass = class!(NSView) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types_v_obj = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(mouseDown:),
                card_mouse_down as *mut c_void,
                types_v_obj.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseEntered:),
                card_mouse_entered as *mut c_void,
                types_v_obj.as_ptr(),
            );
            objc_registerClassPair(cls);
            cls
        };
        *CARD_CLASS.lock().unwrap() = Some(ObjClassPtr(card_cls as *const objc2::runtime::AnyClass));
    }
}

fn create_overlay_window() -> *mut AnyObject {
    unsafe {
        let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
        let screen_frame: NSRect = msg_send![screen, frame];
        let h = window_height(6); // initial reasonable default
        let w = window_width(cards_per_row()); // max possible width
        let x = (screen_frame.size.width - w) / 2.0 + screen_frame.origin.x;
        let y = (screen_frame.size.height - h) / 2.0 + screen_frame.origin.y;
        let frame = NSRect::new(
            NSPoint::new(x, y),
            NSSize::new(w, h),
        );

        // Use standard NSWindow with hidden title bar (avoids dynamic-subclass
        // msg_send! issues). NSTitledWindowMask allows the window to become key
        // without needing a custom subclass with canBecomeKeyWindow override.
        // NSTitledWindowMask = 1 << 0, NSFullSizeContentViewWindowMask = 1 << 15
        let style: u64 = 1 | (1 << 15);

        let window: *mut AnyObject = msg_send![class!(NSWindow), alloc];
        let window: *mut AnyObject = msg_send![window, initWithContentRect: frame, styleMask: style, backing: 2u64, defer: false];

        // Hide the title bar completely
        let _: () = msg_send![window, setTitlebarAppearsTransparent: true];
        let _: () = msg_send![window, setTitleVisibility: 1u64]; // NSWindowTitleHidden = 1

        // NSFloatingWindowLevel = 3 (should be above normal windows during app switch)
        let _: () = msg_send![window, setLevel: 3u64];

        // ========== Window transparency / Liquid Glass settings ==========
        //
        // (1) Window must be non-opaque so the compositor allows content
        //     behind the window to show through.
        let _: () = msg_send![window, setOpaque: false];
        //
        // (2) Window background must be clear, otherwise NSThemeFrame draws
        //     a solid color that blocks everything behind it.
        let clear_color: *mut AnyObject = msg_send![class!(NSColor), clearColor];
        let _: () = msg_send![window, setBackgroundColor: clear_color];
        //
        // (3) Window shadow — setting hasShadow true with a non-opaque
        //     window gives the floating glass look.
        let _: () = msg_send![window, setHasShadow: true];
        // =================================================================

        let _: () = msg_send![window, setReleasedWhenClosed: false];
        // Don't let the window hide on deactivate (we manage show/hide)
        let _: () = msg_send![window, setHidesOnDeactivate: false];

        // --- Liquid Glass ---
        // macOS 26+  → NSGlassEffectView  (new public API, built-in blur)
        // macOS <26 → NSVisualEffectView  (withinWindow + Dark material)
        let is_macos_26 = AnyClass::get(c"NSGlassEffectView").is_some();

        // The view that will contain the card container.
        // On macOS 26 this is the glass view's inner contentView;
        // on older macOS it's the NSVisualEffectView itself.
        let content_parent: *mut AnyObject;

        if is_macos_26 {
            let glass_cls = AnyClass::get(c"NSGlassEffectView").unwrap();
            let glass: *mut AnyObject = msg_send![glass_cls, alloc];
            let glass: *mut AnyObject = msg_send![glass, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h))];
            // (4) Corner radius — native NSGlassEffectView property, from config.
            let _: () = msg_send![glass, setCornerRadius: CONFIG.read().unwrap().appearance.corner_radius];
            // (5) Glass style — "regular" (0) or "clear" (1), from config.
            let style: i64 = match CONFIG.read().unwrap().appearance.glass_style.as_str() {
                "clear" => 1,
                _ => 0, // regular (default)
            };
            let _: () = msg_send![glass, setStyle: style];
            // (6) Tint color — hex RRGGBBAA from config.
            let tint_hex = config::parse_hex8(&CONFIG.read().unwrap().appearance.glass_tint);
            let tint = hex_to_ns_color(tint_hex);
            let _: () = msg_send![glass, setTintColor: tint];
            // (7) Autoresizing so the glass view fills the window on resize.
            let _: () = msg_send![glass, setAutoresizingMask: 18u64];
            let _: () = msg_send![window, setContentView: glass];
            // NSGlassEffectView.contentView may be nil initially — create our own.
            let inner: *mut AnyObject = msg_send![class!(NSView), alloc];
            let inner: *mut AnyObject = msg_send![inner, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h))];
            let _: () = msg_send![inner, setAutoresizingMask: 18u64];
            let _: () = msg_send![glass, setContentView: inner];
            content_parent = inner;
        } else {
            let content: *mut AnyObject = msg_send![window, contentView];
            let ve: *mut AnyObject = msg_send![class!(NSVisualEffectView), alloc];
            let ve: *mut AnyObject = msg_send![ve, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h))];
            // withinWindow blending + Dark material (same as the GPUI version used)
            let _: () = msg_send![ve, setBlendingMode: 1u64];  // WithinWindow
            let _: () = msg_send![ve, setMaterial: 12u64];      // Dark
            let _: () = msg_send![ve, setState: 1u64];           // Active
            let _: () = msg_send![ve, setAutoresizingMask: 18u64];
            let _: () = msg_send![content, addSubview: ve];
            content_parent = ve;
        }

        // --- Container view for cards ---
        // Register OhMyTabContainerView : NSView
        let container_cls = {
            let name = CString::new("OhMyTabContainerView").unwrap();
            let superclass = class!(NSView) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types_v_obj = CString::new("v@:@").unwrap();
            let types_bool = CString::new("B@:").unwrap();
            class_addMethod(
                cls,
                sel!(keyDown:),
                container_key_down as *mut c_void,
                types_v_obj.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(acceptsFirstResponder),
                container_accepts_first_responder as *mut c_void,
                types_bool.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseMoved:),
                container_mouse_moved as *mut c_void,
                types_v_obj.as_ptr(),
            );
            objc_registerClassPair(cls);
            cls
        };

        let container: *mut AnyObject = msg_send![container_cls, alloc];
        let container: *mut AnyObject = msg_send![container, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h))];
        let _: () = msg_send![container, setAutoresizingMask: 18u64];
        let _: () = msg_send![content_parent, addSubview: container];
        *CONTAINER.lock().unwrap() = Some(ObjPtr(container));

        // --- Status label at bottom (standard coords: y=0 is bottom) ---
        let status_font: *mut AnyObject =
            {
    let cfg = CONFIG.read().unwrap();
    msg_send![class!(NSFont), systemFontOfSize: cfg.fonts.status_bar_size, weight: cfg.fonts.status_bar_weight]
};
        let status_color = hex_to_ns_color(0x999999ff);
        let status_label = make_centered_label("", status_font, status_color, 0.0, w, STATUS_H);
        let _: () = msg_send![container, addSubview: status_label];
        *STATUS_LABEL.lock().unwrap() = Some(ObjPtr(status_label));

        window
    }
}

fn create_controller() -> *mut AnyObject {
    unsafe {
        let name = CString::new("OhMyTabController").unwrap();
        let superclass = class!(NSObject) as *const _ as *mut AnyObject;
        let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
        let types_v_obj = CString::new("v@:@").unwrap();
        class_addMethod(
            cls,
            sel!(handleCmdTabPressed:),
            on_cmd_tab_pressed as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleCmdReleased:),
            on_cmd_released as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleThemeToggled:),
            on_theme_toggled as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleAppActivation:),
            on_app_activated as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleAppLaunch:),
            on_app_launched as *mut c_void,
            types_v_obj.as_ptr(),
        );
        objc_registerClassPair(cls);
        msg_send![cls as *mut AnyObject, new]
    }
}

fn init_app() {
    unsafe {
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        // NSApplicationActivationPolicyAccessory = 1
        let _: bool = msg_send![nsapp, setActivationPolicy: 1isize];
    }
}

fn setup_status_bar() {
    unsafe {
        let status_bar: *mut AnyObject = msg_send![class!(NSStatusBar), systemStatusBar];
        let status_item: *mut AnyObject =
            msg_send![status_bar, statusItemWithLength: 30.0f64];
        let _: *mut AnyObject = msg_send![status_item, retain];

        let button: *mut AnyObject = msg_send![status_item, button];

        // Status bar icon
        let ns_name = make_nsstring("square.on.square");
        let image: *mut AnyObject = msg_send![class!(NSImage), imageWithSystemSymbolName: ns_name, accessibilityDescription: std::ptr::null::<AnyObject>()];
        if !image.is_null() {
            let is_template: bool = true;
            let _: () = msg_send![image, setTemplate: is_template];
            let _: () = msg_send![button, setImage: image];
            // NSImageOnly = 1
            let _: () = msg_send![button, setImagePosition: 1usize];
        } else {
            let ns_title = make_nsstring("Tab");
            let _: () = msg_send![button, setTitle: ns_title];
            CFRelease(ns_title as *const c_void);
        }
        CFRelease(ns_name as *const c_void);

        let _: () = msg_send![button, sizeToFit];
        let _: () = msg_send![button, setNeedsDisplay: true];

        // Build menu
        let menu_title = make_nsstring("");
        let menu: *mut AnyObject = msg_send![class!(NSMenu), alloc];
        let menu: *mut AnyObject = msg_send![menu, initWithTitle: menu_title];
        CFRelease(menu_title as *const c_void);

        // Menu action target class
        let action_cls = {
            let name = CString::new("OhMyTabMenuTarget2").unwrap();
            let superclass: *const objc2::runtime::AnyClass = class!(NSObject);
            let cls =
                objc_allocateClassPair(superclass as *mut AnyObject, name.as_ptr(), 0);
            if cls.is_null() {
                eprintln!(
                    "[oh-my-tab] ERROR: Failed to allocate ObjC class for menu target."
                );
                return;
            }
            let types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(handleQuit:),
                handle_quit as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleToggleTheme:),
                handle_toggle_theme as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleToggleShortcut:),
                handle_toggle_shortcut as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleReloadConfig:),
                handle_reload_config as *mut c_void,
                types.as_ptr(),
            );
            objc_registerClassPair(cls);
            cls
        };
        let menu_target: *mut AnyObject = msg_send![action_cls as *const AnyObject, new];

        // Toggle theme item
        let toggle_title = make_nsstring("切换深色");
        let toggle_key = make_nsstring("");
        let toggle_item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
        let toggle_item: *mut AnyObject = msg_send![toggle_item, initWithTitle: toggle_title, action: sel!(handleToggleTheme:), keyEquivalent: toggle_key];
        CFRelease(toggle_title as *const c_void);
        CFRelease(toggle_key as *const c_void);
        let _: () = msg_send![toggle_item, setTarget: menu_target];
        let _: () = msg_send![menu, addItem: toggle_item];

        // Shortcut toggle item
        let shortcut_title = make_nsstring("切换cmd+tab");
        let shortcut_key = make_nsstring("");
        let shortcut_item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
        let shortcut_item: *mut AnyObject = msg_send![shortcut_item, initWithTitle: shortcut_title, action: sel!(handleToggleShortcut:), keyEquivalent: shortcut_key];
        CFRelease(shortcut_title as *const c_void);
        CFRelease(shortcut_key as *const c_void);
        let _: () = msg_send![shortcut_item, setTarget: menu_target];
        let _: () = msg_send![menu, addItem: shortcut_item];
        *SHORTCUT_ITEM.lock().unwrap() = Some(ShortcutState {
            item: shortcut_item,
        });

        // Reload Config item
        let reload_title = make_nsstring("Reload Config");
        let reload_key = make_nsstring("");
        let reload_item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
        let reload_item: *mut AnyObject = msg_send![reload_item, initWithTitle: reload_title, action: sel!(handleReloadConfig:), keyEquivalent: reload_key];
        CFRelease(reload_title as *const c_void);
        CFRelease(reload_key as *const c_void);
        let _: () = msg_send![reload_item, setTarget: menu_target];
        let _: () = msg_send![menu, addItem: reload_item];

        // Separator
        let sep_item: *mut AnyObject = msg_send![class!(NSMenuItem), separatorItem];
        let _: () = msg_send![menu, addItem: sep_item];

        // Quit item
        let quit_title = make_nsstring("Quit");
        let quit_key = make_nsstring("");
        let quit_item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
        let quit_item: *mut AnyObject = msg_send![quit_item, initWithTitle: quit_title, action: sel!(handleQuit:), keyEquivalent: quit_key];
        CFRelease(quit_title as *const c_void);
        CFRelease(quit_key as *const c_void);
        let _: () = msg_send![quit_item, setTarget: menu_target];
        let _: () = msg_send![menu, addItem: quit_item];

        // Store toggle item reference for title updates
        *THEME_STATE.lock().unwrap() = Some(MenuState {
            item: toggle_item,
            is_dark: false,
        });

        let _: () = msg_send![status_item, setMenu: menu];

        // Pump run loop to let SystemUIServer connect
        for _ in 0..10 {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.001, 1u8);
        }
    }
}

// ========== Main ==========

fn main() {
    // 1. Init NSApplication as accessory (no dock icon)
    init_app();

    // 2. Register custom ObjC classes
    register_classes();

    // 3. Setup status bar menu
    setup_status_bar();

    // 4. Initialize state
    ensure_icon_cache_dir();
    cache_running_app_icons(); // pre-warm icon cache for all running apps

    // 4b. Force CONFIG to initialise and report any validation errors
    {
        let cfg = CONFIG.read().unwrap();
        // First load already happened via LazyLock; re-run validate to report problems
        let errs = cfg.validate();
        if !errs.is_empty() {
            eprintln!(
                "[oh-my-tab] Config errors in ~/.config/oh-my-tab/config.toml ({} issue(s)):",
                errs.len()
            );
            for e in &errs {
                eprintln!("[oh-my-tab]   • {}", e);
            }
            eprintln!("[oh-my-tab] Using defaults for invalid fields.");
        }
    }

    *TAB_STATE.lock().unwrap() = Some(AppState::new());

    // 5. Create overlay window (hidden initially)
    let window = create_overlay_window();
    *OVERLAY_WINDOW.lock().unwrap() = Some(ObjPtr(window));
    // Hide initially
    hide_overlay();

    // 6. Create controller object
    let controller = create_controller();
    *CONTROLLER.lock().unwrap() = Some(ObjPtr(controller));

    // 6b. Listen for system app activation so MRU stays in sync
    // when the user switches apps via Dock, Cmd+Tab, etc.
    unsafe {
        let ws: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let nc: *mut AnyObject = msg_send![ws, notificationCenter];
        let name = make_nsstring("NSWorkspaceDidActivateApplicationNotification");
        let _: () = msg_send![nc,
            addObserver: controller,
            selector: sel!(handleAppActivation:),
            name: name,
            object: std::ptr::null::<AnyObject>(),
        ];
        CFRelease(name as *const c_void);

        // Pre-cache icons for apps launched after startup so they're ready
        // before the user summons the switcher (fixes missing icons for apps
        // opened while oh-my-tab is already running).
        let launch_name = make_nsstring("NSWorkspaceDidLaunchApplicationNotification");
        let _: () = msg_send![nc,
            addObserver: controller,
            selector: sel!(handleAppLaunch:),
            name: launch_name,
            object: std::ptr::null::<AnyObject>(),
        ];
        CFRelease(launch_name as *const c_void);
    }

    // 7. Start event monitor + bridge thread
    let (event_tx, event_rx) = flume::unbounded();
    let _monitor = start_event_monitor(event_tx.clone());
    STATUS_EVENT_TX.set(event_tx).ok();

    // Bridge thread: flume events → main thread via performSelectorOnMainThread
    thread::spawn(move || {
        while let Ok(event) = event_rx.recv() {
            let action = match event {
                GlobalEvent::CmdTabPressed => sel!(handleCmdTabPressed:),
                GlobalEvent::CmdReleased => sel!(handleCmdReleased:),
                GlobalEvent::ThemeToggled => sel!(handleThemeToggled:),
            };
            // Read controller pointer from static (only written once, safe to read)
            let ctrl = CONTROLLER.lock().unwrap().unwrap().0;
            unsafe {
                let _: () = msg_send![ctrl,
                    performSelectorOnMainThread: action,
                    withObject: std::ptr::null::<AnyObject>(),
                    waitUntilDone: false
                ];
            }
        }
        println!("[oh-my-tab] Bridge thread exiting.");
    });

    // 8. Run the main event loop (blocks until [NSApp terminate:])
    unsafe {
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![nsapp, finishLaunching];
        let _: () = msg_send![nsapp, run];
    }
}
