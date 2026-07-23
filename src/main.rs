mod window_collector;
mod event_monitor;

use flume;
use gpui::*;
use objc2::{class, msg_send, sel};
use objc2::runtime::{AnyObject, Sel};
use std::collections::HashSet;
use std::ffi::{c_char, c_void, CString};
use std::mem::transmute;
use std::sync::Mutex;
use std::sync::atomic::Ordering;

struct MenuState {
    item: *mut AnyObject,
    is_dark: bool,
}
unsafe impl Send for MenuState {}
unsafe impl Sync for MenuState {}

static THEME_STATE: Mutex<Option<MenuState>> = Mutex::new(None);
struct ShortcutState {
    item: *mut AnyObject,
}
unsafe impl Send for ShortcutState {}
unsafe impl Sync for ShortcutState {}

static SHORTCUT_ITEM: Mutex<Option<ShortcutState>> = Mutex::new(None);
static STATUS_EVENT_TX: std::sync::OnceLock<flume::Sender<GlobalEvent>> = std::sync::OnceLock::new();

#[link(name = "objc", kind = "dylib")]
extern "C" {
    fn objc_allocateClassPair(superclass: *mut AnyObject, name: *const c_char, extra_bytes: usize) -> *mut AnyObject;
    fn objc_registerClassPair(cls: *mut AnyObject);
    fn class_addMethod(cls: *mut AnyObject, name: Sel, imp: *mut c_void, types: *const c_char) -> bool;
}
use window_collector::{MruMap, WindowInfo, ensure_icon_cache_dir, extract_icon_to_cache, raise_ax_window};
use event_monitor::{GlobalEvent, start as start_event_monitor};


#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithCString(alloc: *const c_void, c_str: *const c_char, encoding: u32) -> *const c_void;
    fn CFRelease(cf: *const c_void);
    fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, return_after_source_handled: u8) -> i32;
    static kCFRunLoopDefaultMode: *mut c_void;
}

fn make_nsstring(s: &str) -> *mut AnyObject {
    unsafe {
        let c_str = CString::new(s).unwrap();
        let cf = CFStringCreateWithCString(std::ptr::null(), c_str.as_ptr(), 0x08000100u32); // kCFStringEncodingUTF8
        if cf.is_null() {
            eprintln!("[oh-my-tab] ERROR: CFStringCreateWithCString failed for '{}'", s);
        }
        cf as *mut AnyObject // toll-free bridged CFString <-> NSString
    }
}

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
    let new_label = if is_cmd { "切换opt+tab" } else { "切换cmd+tab" };
    println!("[oh-my-tab] Shortcut: {}", if is_cmd { "Cmd+Tab" } else { "Opt+Tab" });
    if let Some(ref s) = *SHORTCUT_ITEM.lock().unwrap() {
        unsafe {
            let ns_title = make_nsstring(new_label);
            let _: () = msg_send![s.item, setTitle: ns_title];
            CFRelease(ns_title as *const c_void);
        }
    }
}

extern "C" fn handle_toggle_theme(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    let mut state = THEME_STATE.lock().unwrap();
    if let Some(ref mut s) = *state {
        s.is_dark = !s.is_dark;
        let new_label = if s.is_dark { "切换浅色" } else { "切换深色" }; // dark→浅色, light→深色
        println!(
            "[oh-my-tab] Toggled theme to {}",
            if s.is_dark { "dark" } else { "light" }
        );
        unsafe {
            let ns_title = make_nsstring(new_label);
            let _: () = msg_send![s.item, setTitle: ns_title];
            CFRelease(ns_title as *const c_void);
        }
    }
    if let Some(tx) = STATUS_EVENT_TX.get() {
        let _ = tx.send(GlobalEvent::ThemeToggled);
    }
}

fn setup_status_bar() {
    unsafe {
        let status_bar: *mut AnyObject = msg_send![class!(NSStatusBar), systemStatusBar];
        let status_item: *mut AnyObject = msg_send![status_bar, statusItemWithLength: 30.0f64];
        let _: *mut AnyObject = msg_send![status_item, retain];

        let button: *mut AnyObject = msg_send![status_item, button];

        let ns_name = make_nsstring("square.on.square");
        let image: *mut AnyObject = msg_send![class!(NSImage), imageWithSystemSymbolName: ns_name, accessibilityDescription: std::ptr::null::<AnyObject>()];
        if !image.is_null() {
            let is_template: bool = true;
            let _: () = msg_send![image, setTemplate: is_template];
            let _: () = msg_send![button, setImage: image];
            let _: () = msg_send![button, setImagePosition: 1usize]; // NSImageOnly
        } else {
            let ns_title = make_nsstring("Tab");
            let _: () = msg_send![button, setTitle: ns_title];
            CFRelease(ns_title as *const c_void);
        }
        CFRelease(ns_name as *const c_void);

        let _: () = msg_send![button, sizeToFit];
        let _: () = msg_send![button, setNeedsDisplay: true];

        // Build menu with Quit item
        let menu_title = make_nsstring("");
        let menu: *mut AnyObject = msg_send![class!(NSMenu), alloc];
        let menu: *mut AnyObject = msg_send![menu, initWithTitle: menu_title];
        CFRelease(menu_title as *const c_void);

        // Create menu action target class
        let action_cls = {
            let name = CString::new("OhMyTabMenuTarget").unwrap();
            let superclass: *const objc2::runtime::AnyClass = class!(NSObject);
            let cls = objc_allocateClassPair(superclass as *mut AnyObject, name.as_ptr(), 0);
            if cls.is_null() {
                eprintln!("[oh-my-tab] ERROR: Failed to allocate ObjC class for menu target.");
                return;
            }
            let types = CString::new("v@:@").unwrap();
            class_addMethod(cls, sel!(handleQuit:), handle_quit as *mut c_void, types.as_ptr());
            class_addMethod(cls, sel!(handleToggleTheme:), handle_toggle_theme as *mut c_void, types.as_ptr());
            class_addMethod(cls, sel!(handleToggleShortcut:), handle_toggle_shortcut as *mut c_void, types.as_ptr());
            objc_registerClassPair(cls);
            cls
        };
        let menu_target: *mut AnyObject = msg_send![action_cls as *const AnyObject, new];

        // Toggle theme item (light by default)
        let toggle_title = make_nsstring("切换深色");
        let toggle_key = make_nsstring("");
        let toggle_item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
        let toggle_item: *mut AnyObject = msg_send![toggle_item, initWithTitle: toggle_title, action: sel!(handleToggleTheme:), keyEquivalent: toggle_key];
        CFRelease(toggle_title as *const c_void);
        CFRelease(toggle_key as *const c_void);
        let _: () = msg_send![toggle_item, setTarget: menu_target];
        let _: () = msg_send![menu, addItem: toggle_item];

        // Shortcut toggle item (default: Opt+Tab)
        let shortcut_title = make_nsstring("切换cmd+tab");
        let shortcut_key = make_nsstring("");
        let shortcut_item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
        let shortcut_item: *mut AnyObject = msg_send![shortcut_item, initWithTitle: shortcut_title, action: sel!(handleToggleShortcut:), keyEquivalent: shortcut_key];
        CFRelease(shortcut_title as *const c_void);
        CFRelease(shortcut_key as *const c_void);
        let _: () = msg_send![shortcut_item, setTarget: menu_target];
        let _: () = msg_send![menu, addItem: shortcut_item];
        *SHORTCUT_ITEM.lock().unwrap() = Some(ShortcutState { item: shortcut_item });

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

        // Store toggle item for title updates
        *THEME_STATE.lock().unwrap() = Some(MenuState { item: toggle_item, is_dark: false });

        let _: () = msg_send![status_item, setMenu: menu];

        // Pump run loop to allow NSStatusBar to connect to SystemUIServer
        for _ in 0..10 {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.001, 1u8);
        }
    }
}

struct TabState {
    windows: Vec<WindowInfo>,
    selected: usize,
    visible: bool,
    mru: MruMap,
}

impl TabState {
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
        TabState { windows, selected: if win_count > 1 { 1 } else { 0 }, visible: false, mru }
    }

    fn refresh(&mut self) {
        self.windows = window_collector::collect_windows(&mut self.mru);
        if !self.windows.is_empty() && self.selected >= self.windows.len() {
            self.selected = self.windows.len() - 1;
        }
        if self.windows.is_empty() { self.visible = false; }
    }
}

struct OverlayView {
    state: Entity<TabState>,
    _observer: Subscription,
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

fn has_accessibility_permission() -> bool {
    unsafe { AXIsProcessTrusted() }
}

fn hide_window() {
    unsafe {
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let windows: *mut AnyObject = msg_send![nsapp, windows];
        if windows.is_null() { return; }
        let count: usize = msg_send![windows, count];
        if count == 0 { return; }
        let window: *mut AnyObject = msg_send![windows, objectAtIndex: 0u64];
        let selector = sel!(orderOut:);
        extern "C" { fn objc_msgSend(); }
        type F = unsafe extern "C" fn(*mut c_void, Sel, *mut c_void);
        let f: F = transmute(objc_msgSend as *const ());
        f(window as *mut c_void, selector, nsapp as *mut c_void);
    }
}

fn configure_borderless() {
    unsafe {
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let windows: *mut AnyObject = msg_send![nsapp, windows];
        if windows.is_null() { return; }
        let count: usize = msg_send![windows, count];
        if count == 0 { return; }
        let window: *mut AnyObject = msg_send![windows, objectAtIndex: 0u64];

        // Borderless + full size content
        let current_style: usize = msg_send![window, styleMask];
        let new_style = (current_style & !(1usize | (1 << 1) | (1 << 2))) | (1 << 15);
        let _: () = msg_send![window, setStyleMask: new_style];

        // Clear background for blur
        let _: () = msg_send![window, setOpaque: false];
        let clear_color: *mut AnyObject = msg_send![class!(NSColor), clearColor];
        let _: () = msg_send![window, setBackgroundColor: clear_color];

        // Rounded corners + non-opaque layer for transparency
        let content_view: *mut AnyObject = msg_send![window, contentView];
        let _: () = msg_send![content_view, setWantsLayer: true];
        let layer: *mut AnyObject = msg_send![content_view, layer];
        let _: () = msg_send![layer, setOpaque: false];
        let _: () = msg_send![layer, setCornerRadius: 28.0f64];
        let _: () = msg_send![layer, setMasksToBounds: true];
    }
}

fn show_window() {
    unsafe {
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let windows: *mut AnyObject = msg_send![nsapp, windows];
        if windows.is_null() { return; }
        let count: usize = msg_send![windows, count];
        if count == 0 { return; }
        let window: *mut AnyObject = msg_send![windows, objectAtIndex: 0u64];

        // 确保 contentView layer 圆角裁剪
        let content_view: *mut AnyObject = msg_send![window, contentView];
        let cv_layer: *mut AnyObject = msg_send![content_view, layer];
        if !cv_layer.is_null() {
            let _: () = msg_send![cv_layer, setCornerRadius: 28.0f64];
            let _: () = msg_send![cv_layer, setMasksToBounds: true];
        }

        let selector = sel!(orderFront:);
        extern "C" { fn objc_msgSend(); }
        type F = unsafe extern "C" fn(*mut c_void, Sel, *mut c_void);
        let f: F = transmute(objc_msgSend as *const ());
        f(window as *mut c_void, selector, nsapp as *mut c_void);
    }
}

fn activate_pid(pid: i32) {
    unsafe {
        let cls = class!(NSRunningApplication);
        let app: *mut AnyObject = msg_send![cls, runningApplicationWithProcessIdentifier: pid];
        if !app.is_null() {
            // NSApplicationActivateIgnoringOtherApps = 1
            let _: bool = msg_send![app, activateWithOptions: 1usize];
        } else {
            eprintln!("[oh-my-tab] activate_pid: no running app for pid {}", pid);
        }
    }
}

fn init_app() {
    unsafe {
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: bool = msg_send![nsapp, setActivationPolicy: 1isize]; // NSApplicationActivationPolicyAccessory
    }
}

fn c(hex: u32) -> Hsla { rgb(hex).into() }
fn ca(hex: u32) -> Hsla { rgba(hex).into() }

struct Colors {
    page_bg: Hsla,
    hint_bg: Hsla,
    hint_text: Hsla,
    hint_subtext: Hsla,
    status_bar_bg: Hsla,
    status_bar_text: Hsla,
    card_bg: Hsla,
    card_bg_sel: Hsla,
    card_border_sel: Hsla,
    icon_inner_bg: Hsla,
    icon_text: Hsla,
    app_name: Hsla,
    win_title: Hsla,
}

fn dark_colors() -> Colors {
    // 深色主题：完全透明背景，靠 CGS 模糊提供视觉效果
    Colors {
        page_bg: rgba(0x00000000).into(),
        hint_bg: rgba(0x00000000).into(),
        hint_text: c(0x888888),
        hint_subtext: c(0x666666),
        status_bar_bg: rgba(0x00000000).into(),
        status_bar_text: c(0x999999),
        card_bg: rgba(0x00000000).into(),
        card_bg_sel: rgba(0x22224444).into(),
        card_border_sel: c(0x5577cc),
        icon_inner_bg: rgba(0x22224444).into(),
        icon_text: c(0x9999bb),
        app_name: c(0xdddddd),
        win_title: c(0x888888),
    }
}

fn light_colors() -> Colors {
    // 浅色主题：完全透明背景，靠 CGS 模糊提供视觉效果
    Colors {
        page_bg: rgba(0x00000000).into(),
        hint_bg: rgba(0x00000000).into(),
        hint_text: c(0x666666),
        hint_subtext: c(0x999999),
        status_bar_bg: rgba(0x00000000).into(),
        status_bar_text: c(0x555555),
        card_bg: rgba(0x00000000).into(),
        card_bg_sel: rgba(0xffffff66).into(),
        card_border_sel: c(0x5577cc),
        icon_inner_bg: rgba(0xd0d0e066).into(),
        icon_text: c(0x666688),
        app_name: c(0x1a1a1a),
        win_title: c(0x555555),
    }
}

fn current_colors() -> Colors {
    let is_dark = THEME_STATE.lock().unwrap().as_ref().map_or(false, |s| s.is_dark);
    if is_dark { dark_colors() } else { light_colors() }
}

fn hit_test_card(mx: f32, my: f32, total_cards: usize) -> Option<usize> {
    let cards_per_row: usize = 6;
    let card_w: f32 = 160.0;
    let row_h: f32 = 200.0;
    let gap: f32 = 10.0;
    let pad: f32 = 20.0;

    let cols = cards_per_row.min(total_cards);
    let row_width = cols as f32 * card_w + cols.saturating_sub(1) as f32 * gap;
    let start_x = (1050.0 - row_width) / 2.0;

    let col = ((mx - start_x) / (card_w + gap)).floor() as isize;
    let row = ((my - pad) / row_h).floor() as isize;
    if col < 0 || col >= cards_per_row as isize || row < 0 { return None; }

    let card_left = start_x + col as f32 * (card_w + gap);
    if mx < card_left || mx > card_left + card_w { return None; }

    let idx = row as usize * cards_per_row + col as usize;
    if idx >= total_cards { return None; }
    Some(idx)
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


impl Render for OverlayView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let c = current_colors();

        if !state.visible {
            let hint: &str = if has_accessibility_permission() { "Hold Option + Tab to switch" } else { "Need Accessibility permission" };
            return div()
                .size_full().flex().flex_col().items_center().justify_center().gap(px(4.))
                .bg(c.hint_bg).text_color(c.hint_text).text_sm()
                .child(hint)
                .child(div().text_xs().text_color(c.hint_subtext).child(format!("PID: {} | Perm: {}", std::process::id(), has_accessibility_permission())))
                .into_any();
        }

        let selected = state.selected;
        let windows = state.windows.clone();
        let img_sz = 128.0;
        let letter_sq = 64.0;

        let status = match windows.get(selected) {
            Some(w) if !w.window_title.is_empty() => truncate_text(&format!("{} — {}", w.app_name, w.window_title), 126),
            Some(w) => truncate_text(&w.app_name, 126),
            None => String::new(),
        };

        // ---- 卡片区域 ----
        let cards: Vec<AnyElement> = {
            let state_entity = self.state.clone();
            windows.iter().enumerate().map(|(i, w)| {
                let is_sel = i == selected;
                let init = w.app_name.chars().next().unwrap_or('?').to_string();
                let pid = w.pid;
                let window_id = w.window_id;
                let window_title = w.window_title.clone();
                // 图标 / 首字母
                let icon_div: Div = if let Some(ref icon_path) = w.icon_path {
                    div().flex().items_center().justify_center()
                        .child(img(std::path::PathBuf::from(icon_path.clone())).max_w(px(img_sz)).max_h(px(img_sz)))
                } else {
                    div().flex().items_center().justify_center()
                        .child(div().w(px(letter_sq)).h(px(letter_sq)).rounded_xl().bg(c.icon_inner_bg).flex().items_center().justify_center()
                            .text_lg().font_weight(FontWeight::SEMIBOLD).text_color(c.icon_text).child(init.clone()))
                };
                // 卡片：外层 wrapper 提供外边框 + 内层容器放内容
                // 不设 bg，让页面背景透过来，避免半透明叠加
                let inner = div().w(px(160.)).rounded_3xl()
                    .flex().flex_col().items_center().gap(px(6.)).pt(px(8.)).pb(px(8.)).overflow_hidden().flex_shrink_0()
                    .id(i)
                    .on_hover({
                        let se = state_entity.clone();
                        move |hovered: &bool, _window: &mut Window, app: &mut App| {
                            if *hovered {
                                se.update(app, |state, cx| {
                                    state.selected = i;
                                    cx.notify();
                                });
                                _window.refresh();
                            }
                        }
                    })
                    .on_mouse_down(MouseButton::Left, {
                        let se = state_entity.clone();
                        let wt = window_title.clone();
                        move |_event: &MouseDownEvent, _window: &mut Window, app: &mut App| {
                            se.update(app, |state, cx| {
                                hide_window();
                                activate_pid(pid);
                                raise_ax_window(pid, &wt);
                                state.visible = false;
                                state.mru.insert(window_id, std::time::Instant::now());
                                cx.notify();
                            });
                        }
                    })
                    .child(icon_div)
                    .child(div().w_full().flex().flex_col().items_center().px(px(8.))
                        .child(div().w_full().text_sm().font_weight(FontWeight::MEDIUM).text_center().text_color(c.app_name).whitespace_nowrap().child(truncate_text(&w.app_name, 17)))
                        .child(div().w_full().text_xs().text_center().text_color(c.win_title).mt(px(2.)).whitespace_nowrap().child(truncate_text(&w.window_title, 20))));
                div().rounded_3xl().p(px(3.))
                    .bg(if is_sel { c.card_border_sel } else { rgba(0x00000000).into() })
                    .child(inner)
                    .into_any()
            }).collect()
        };

        // ---- 整体布局：顶部高光 + 卡片网格 + 底部状态栏 ----
        div()
            .size_full().flex().flex_col().bg(c.page_bg)
            .child(div().grid().grid_cols(6).justify_center().gap(px(10.)).py(px(16.)).size_full().children(cards))
            // 底部状态栏
            .child(div().h(px(36.)).w_full().bg(c.status_bar_bg).flex().items_center().px(px(12.))
                .child(div().w_full().text_sm().text_center().text_color(c.status_bar_text).whitespace_nowrap().child(status)))
            .into_any()
    }
}

fn window_height(count: usize) -> Pixels {
    let cards_per_row: usize = 6;
    let rows = (count.max(1) + cards_per_row - 1) / cards_per_row;
    let card_h = 200.0; // 图标 ~128px + 文字 ~36px + 间距
    px(32.0 + rows as f32 * card_h + 36.0)
}

fn main() {
    let (event_tx, event_rx) = flume::unbounded();
    let _monitor = start_event_monitor(event_tx.clone());

    Application::new().run(move |cx: &mut App| {
        ensure_icon_cache_dir();
        let state_entity = cx.new(|_cx| TabState::new());

        let init_count = state_entity.read(cx).windows.len();
        let bounds = Bounds::centered(None, size(px(1050.), window_height(init_count)), cx);
        let window_handle = cx.open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), focus: true, kind: WindowKind::PopUp, window_background: WindowBackgroundAppearance::Blurred, ..Default::default() },
            |_window, cx| {
                let se = state_entity.clone();
                cx.new(|cx| {
                    let state = se.clone();
                    let observer = cx.observe(&state, |_: &mut OverlayView, _: Entity<TabState>, cx: &mut Context<OverlayView>| {
                        cx.notify();
                    });
                    OverlayView { state, _observer: observer }
                })
            },
        ).unwrap();
        configure_borderless();
        init_app();
        hide_window();
        STATUS_EVENT_TX.set(event_tx.clone()).ok();
        setup_status_bar();

        {
            let se = state_entity.clone();
            let async_app = cx.to_async();
            let wh = window_handle;
            let (icon_tx, icon_rx) = flume::unbounded::<(i32, String)>();

            let icon_se = state_entity.clone();
            let icon_app = cx.to_async();
            let _icon_task = Box::leak(Box::new(cx.spawn(move |_: &mut AsyncApp| async move {
                while let Ok((pid, path)) = icon_rx.recv_async().await {
                    let _ = icon_app.update(|app_cx| {
                        icon_se.update(app_cx, |state, cx| {
                            for w in &mut state.windows {
                                if w.pid == pid && w.icon_path.is_none() {
                                    w.icon_path = Some(path.clone());
                                }
                            }
                            cx.notify();
                        });
                    });
                }
            })));

            let _task = Box::leak(Box::new(cx.spawn(move |_: &mut AsyncApp| async move {
                while let Ok(event) = event_rx.recv_async().await {
                    let se = se.clone();
                    let wh = wh;
                    let icon_tx = icon_tx.clone();
                    let _ = async_app.update(move |app_cx| {
                        let mut should_activate = false;
                        se.update(app_cx, |state, cx| {
                            match event {
                                GlobalEvent::CmdTabPressed => {
                                    if !state.visible {
                                        state.refresh();
                                        state.visible = true;
                                        state.selected = if state.windows.len() > 1 { 1 } else { 0 };
                                        should_activate = true;
                                    } else {
                                        state.selected = (state.selected + 1) % state.windows.len().max(1);
                                    }
                                }
                                GlobalEvent::CmdReleased => {
                                    if state.visible {
                                        if let Some(w) = state.windows.get(state.selected) {
                                            let wid = w.window_id;
                                            let pw = w.pid;
                                            let wt = w.window_title.clone();
                                            println!("[oh-my-tab] CmdReleased: switching to '{}' (pid={})", w.app_name, pw);
                                            hide_window();
                                            activate_pid(pw);
                                            raise_ax_window(pw, &wt);
                                            state.mru.insert(wid, std::time::Instant::now());
                                        } else {
                                            eprintln!("[oh-my-tab] CmdReleased: selected index {} out of bounds (windows={})", state.selected, state.windows.len());
                                        }
                                        state.visible = false;
                                    }
                                }
                                GlobalEvent::ThemeToggled => {
                                    // theme state already updated, just need re-render
                                }
                            }
                            cx.notify();
                        });
                        if should_activate {
                            show_window();
                            let count = se.read(app_cx).windows.len();
                            let _ = wh.update(app_cx, |_, window: &mut Window, _| window.resize(size(px(1050.), window_height(count))));
                            let uncached: Vec<i32> = se.read(app_cx).windows.iter()
                                .filter(|w| w.icon_path.is_none())
                                .map(|w| w.pid)
                                .collect::<HashSet<_>>()
                                .into_iter()
                                .collect();
                            for pid in uncached {
                                let tx = icon_tx.clone();
                                std::thread::spawn(move || {
                                    if let Some(path) = extract_icon_to_cache(pid) {
                                        let _ = tx.send((pid, path));
                                    }
                                });
                            }
                        }
                    });
                }
        })));
        }

        let se = state_entity.clone();
        let _sub = Box::leak(Box::new(cx.observe_keystrokes(move |event: &KeystrokeEvent, _window: &mut Window, _app: &mut App| {
            let key = event.keystroke.key.as_str();

            se.update(_app, |state, cx| {
                if !state.visible { return; }
                match key {
                    "tab" | "right" => {
                        if !state.windows.is_empty() {
                            state.selected = (state.selected + 1) % state.windows.len();
                        }
                    }
                    "left" => {
                        if !state.windows.is_empty() {
                            state.selected = if state.selected == 0 { state.windows.len() - 1 } else { state.selected - 1 };
                        }
                    }
                    "enter" => {
                        if let Some(w) = state.windows.get(state.selected) {
                            activate_pid(w.pid);
                        }
                        state.visible = false;
                        hide_window();
                    }
                    "escape" => {
                        state.visible = false;
                        hide_window();
                    }
                    _ => {}
                }
                cx.notify();
            });
            _window.refresh();
        })));

    });
}
