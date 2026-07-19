mod window_collector;
mod event_monitor;

use flume;
use gpui::*;
use objc2::{class, msg_send, sel};
use objc2::runtime::{AnyObject, Sel};
use std::collections::HashSet;
use std::ffi::c_void;
use std::mem::transmute;
use window_collector::{MruMap, WindowInfo, ensure_icon_cache_dir, extract_icon_to_cache, raise_ax_window};
use event_monitor::{GlobalEvent, start as start_event_monitor};

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

fn show_window() {
    unsafe {
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let windows: *mut AnyObject = msg_send![nsapp, windows];
        if windows.is_null() { return; }
        let count: usize = msg_send![windows, count];
        if count == 0 { return; }
        let window: *mut AnyObject = msg_send![windows, objectAtIndex: 0u64];
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
            let selector = sel!(activateWithOptions:);
            extern "C" { fn objc_msgSend(); }
            type F = unsafe extern "C" fn(*mut c_void, Sel, isize);
            let f: F = transmute(objc_msgSend as *const ());
            f(app as *mut c_void, selector, 1);
        }
    }
}

impl Render for OverlayView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);

        if !state.visible {
            let hint: &str = if has_accessibility_permission() { "Hold Option + Tab to switch" } else { "Need Accessibility permission" };
            return div()
                .size_full().flex().flex_col().items_center().justify_center().gap(px(4.))
                .bg(rgb(0x1c1c1e)).text_color(rgb(0x888888)).text_sm()
                .child(hint)
                .child(div().text_xs().text_color(rgb(0x666666)).child(format!("PID: {} | Perm: {}", std::process::id(), has_accessibility_permission())))
                .into_any();
        }

        let selected = state.selected;
        let windows = state.windows.clone();
        let status = match windows.get(selected) {
            Some(w) if !w.window_title.is_empty() => format!("{} — {}", w.app_name, w.window_title),
            Some(w) => w.app_name.clone(),
            None => String::new(),
        };

        let cards: Vec<AnyElement> = windows.iter().enumerate().map(|(i, w)| {
            let is_sel = i == selected;
            let init = w.app_name.chars().next().unwrap_or('?').to_string();
            let icon_div: Div = if let Some(ref icon_path) = w.icon_path {
                div().h(px(80.)).flex().items_center().justify_center().bg(rgb(0x222233))
                    .child(img(std::path::PathBuf::from(icon_path.clone())).max_w(px(64.)).max_h(px(64.)))
            } else {
                div().h(px(80.)).flex().items_center().justify_center().bg(rgb(0x222233))
                    .child(div().w(px(40.)).h(px(40.)).rounded_md().bg(rgb(0x3a3a5a)).flex().items_center().justify_center()
                        .text_lg().font_weight(FontWeight::SEMIBOLD).text_color(rgb(0xaaaacc)).child(init.clone()))
            };
            div().w(px(160.)).rounded_md().border_2()
                .border_color(if is_sel { rgb(0x5a5a8a) } else { rgba(0x00000000) })
                .bg(if is_sel { rgb(0x3a3a5a) } else { rgb(0x2a2a3a) })
                .flex().flex_col().overflow_hidden()
                .child(icon_div)
                .child(div().px(px(10.)).py(px(8.))
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(rgb(0xdddddd)).overflow_hidden().whitespace_nowrap().child(w.app_name.clone()))
                    .child(div().text_xs().text_color(rgb(0x888888)).mt(px(2.)).overflow_hidden().whitespace_nowrap().child(w.window_title.clone())))
                .into_any()
        }).collect();

        div()
            .size_full().flex().flex_col().bg(rgb(0x1e1e2e))
            .child(div().flex().flex_row().flex_wrap().justify_center().items_center().gap(px(10.)).p(px(20.)).size_full().children(cards))
            .child(div().h(px(36.)).w_full().bg(rgb(0x161622)).flex().items_center().justify_center().text_sm().text_color(rgb(0x999999)).child(status))
            .into_any()
    }
}

fn window_height(count: usize) -> Pixels {
    let cards_per_row: usize = 5;
    let rows = (count.max(1) + cards_per_row - 1) / cards_per_row;
    px(40.0 + rows as f32 * 160.0 + 36.0)
}

fn main() {
    let (event_tx, event_rx) = flume::unbounded();
    let _monitor = start_event_monitor(event_tx);

    Application::new().run(move |cx: &mut App| {
        ensure_icon_cache_dir();
        let state_entity = cx.new(|_cx| TabState::new());

        let init_count = state_entity.read(cx).windows.len();
        let bounds = Bounds::centered(None, size(px(900.), window_height(init_count)), cx);
        let window_handle = cx.open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), focus: true, ..Default::default() },
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
        hide_window();

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
                                GlobalEvent::OptionTabPressed => {
                                    if !state.visible {
                                        state.refresh();
                                        state.visible = true;
                                        state.selected = if state.windows.len() > 1 { 1 } else { 0 };
                                        should_activate = true;
                                    } else {
                                        state.selected = (state.selected + 1) % state.windows.len().max(1);
                                    }
                                }
                                GlobalEvent::OptionReleased => {
                                    if state.visible {
                                        if let Some(w) = state.windows.get(state.selected) {
                                            raise_ax_window(w.pid, &w.window_title);
                                            hide_window();
                                        }
                                        state.visible = false;
                                    }
                                }
                            }
                            cx.notify();
                        });
                        if should_activate {
                            show_window();
                            let count = se.read(app_cx).windows.len();
                            let _ = wh.update(app_cx, |_, window: &mut Window, _| window.resize(size(px(900.), window_height(count))));
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
                            let _ = app_cx.activate(true);
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
