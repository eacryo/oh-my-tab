mod window_collector;

use gpui::*;
use std::cell::RefCell;
use std::rc::Rc;
use window_collector::{MruMap, WindowInfo};

struct TabState {
    windows: Vec<WindowInfo>,
    selected: usize,
    visible: bool,
    mru: MruMap,
}

impl TabState {
    fn new() -> Self {
        let mut mru = MruMap::new();
        let windows = window_collector::collect_windows(&mut mru);
        TabState { windows, selected: 0, visible: false, mru }
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

struct OverlayView {
    state: Rc<RefCell<TabState>>,
}

fn activate_pid(pid: i32) {
    let cls = objc2::class!(NSRunningApplication);
    let app: Option<objc2::rc::Retained<objc2::runtime::AnyObject>> =
        unsafe { objc2::msg_send![cls, runningApplicationWithProcessIdentifier: pid] };
    if let Some(app) = app {
        unsafe { let _: () = objc2::msg_send![&app, activateWithOptions: 1u64]; }
    }
}

impl Render for OverlayView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let state_clone = self.state.clone();
        let state = self.state.borrow_mut();
        let state_ref = &*state;

        if !state_ref.visible {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgb(0x1c1c1e))
                .text_color(rgb(0x888888))
                .text_sm()
                .child("Hold Option + Tab to switch")
                .on_key_down(move |event: &KeyDownEvent, _window: &mut Window, _cx: &mut App| {
                    let mut s = state_clone.borrow_mut();
                    if event.keystroke.modifiers.alt && event.keystroke.key.as_str() == "tab" {
                        s.refresh();
                        s.visible = true;
                        s.selected = if s.windows.len() > 1 { 1 } else { 0 };
                    }
                })
                .into_any();
        }

        let selected = state_ref.selected;
        let windows = state_ref.windows.clone();
        let status = if let Some(w) = windows.get(selected) {
            if w.window_title.is_empty() { w.app_name.clone() }
            else { format!("{} — {}", w.app_name, w.window_title) }
        } else { String::new() };

        let cards: Vec<AnyElement> = windows.iter().enumerate().map(|(i, w)| {
            let is_sel = i == selected;
            let init = w.app_name.chars().next().unwrap_or('?').to_string();
            let name = w.app_name.clone();
            let title = w.window_title.clone();
            let _pid = w.pid;

            let _card_state = self.state.clone();
            div()
                .w(px(160.))
                .rounded_md()
                .border_2()
                .border_color(if is_sel { rgb(0x5a5a8a) } else { rgba(0x00000000) })
                .bg(if is_sel { rgb(0x3a3a5a) } else { rgb(0x2a2a3a) })
                .flex()
                .flex_col()
                .overflow_hidden()
                .child(
                    div().h(px(80.)).flex().items_center().justify_center().bg(rgb(0x222233))
                        .child(
                            div().w(px(40.)).h(px(40.)).rounded_md().bg(rgb(0x3a3a5a))
                                .flex().items_center().justify_center()
                                .text_lg().font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(0xaaaacc))
                                .child(init)
                        )
                )
                .child(
                    div().px(px(10.)).py(px(8.))
                        .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(rgb(0xdddddd)).overflow_hidden().whitespace_nowrap().child(name))
                        .child(div().text_xs().text_color(rgb(0x888888)).mt(px(2.)).overflow_hidden().whitespace_nowrap().child(title))
                )
                .into_any()
        }).collect();

        let sc = self.state.clone();
        let sc2 = self.state.clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x1e1e2e))
            .child(
                div().flex().flex_row().flex_wrap().justify_center().items_center().gap(px(10.)).p(px(20.)).size_full().children(cards)
            )
            .child(
                div().h(px(36.)).w_full().bg(rgb(0x161622)).flex().items_center().justify_center().text_sm().text_color(rgb(0x999999)).child(status)
            )
            .on_key_down(move |event: &KeyDownEvent, _window: &mut Window, _cx: &mut App| {
                let mut s = sc.borrow_mut();
                if event.keystroke.modifiers.alt {
                    if event.keystroke.key.as_str() == "tab" {
                        s.selected = (s.selected + 1) % s.windows.len().max(1);
                    }
                } else {
                    match event.keystroke.key.as_str() {
                        "tab" | "right" => if s.visible && !s.windows.is_empty() { s.selected = (s.selected + 1) % s.windows.len(); },
                        "left" => if s.visible && !s.windows.is_empty() { s.selected = if s.selected == 0 { s.windows.len() - 1 } else { s.selected - 1 }; },
                        "enter" => if s.visible { activate_pid(s.windows[s.selected].pid); s.visible = false; },
                        "escape" => if s.visible { s.visible = false; },
                        _ => {}
                    }
                }
            })
            .on_key_up(move |event: &KeyUpEvent, _window: &mut Window, _cx: &mut App| {
                let mut s = sc2.borrow_mut();
                if event.keystroke.key.as_str() == "alt" && s.visible {
                    let pid = s.windows.get(s.selected).map(|w| w.pid);
                    s.visible = false;
                    if let Some(pid) = pid { activate_pid(pid); }
                }
            })
            .into_any()
    }
}

fn main() {
    let state = Rc::new(RefCell::new(TabState::new()));

    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(900.), px(250.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                focus: true,
                ..Default::default()
            },
            |_window, cx| cx.new(|_cx| OverlayView { state: state.clone() }),
        )
        .unwrap();
        cx.activate(true);
    });
}
