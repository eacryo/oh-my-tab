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
        TabState { windows, selected: if win_count > 1 { 1 } else { 0 }, visible: true, mru }
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
    state: Rc<RefCell<TabState>>,
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

fn has_accessibility_permission() -> bool {
    unsafe { AXIsProcessTrusted() }
}

fn activate_app(name: &str) {
    std::process::Command::new("open")
        .arg("-a")
        .arg(name)
        .spawn()
        .ok();
}

impl Render for OverlayView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.borrow();
        let state_ref = &*state;

        if !state_ref.visible {
            let hint: &str = if has_accessibility_permission() { "Hold Option + Tab to switch" } else { "Need Accessibility permission" };
            return div()
                .size_full().flex().flex_col().items_center().justify_center().gap(px(4.))
                .bg(rgb(0x1c1c1e)).text_color(rgb(0x888888)).text_sm()
                .child(hint)
                .child(div().text_xs().text_color(rgb(0x666666)).child(format!("PID: {} | Perm: {}", std::process::id(), has_accessibility_permission())))
                .into_any();
        }

        let selected = state_ref.selected;
        let windows = state_ref.windows.clone();
        let status = match windows.get(selected) {
            Some(w) if !w.window_title.is_empty() => format!("{} — {}", w.app_name, w.window_title),
            Some(w) => w.app_name.clone(),
            None => String::new(),
        };

        let cards: Vec<AnyElement> = windows.iter().enumerate().map(|(i, w)| {
            let is_sel = i == selected;
            let init = w.app_name.chars().next().unwrap_or('?').to_string();
            div().w(px(160.)).rounded_md().border_2()
                .border_color(if is_sel { rgb(0x5a5a8a) } else { rgba(0x00000000) })
                .bg(if is_sel { rgb(0x3a3a5a) } else { rgb(0x2a2a3a) })
                .flex().flex_col().overflow_hidden()
                .child(div().h(px(80.)).flex().items_center().justify_center().bg(rgb(0x222233))
                    .child(div().w(px(40.)).h(px(40.)).rounded_md().bg(rgb(0x3a3a5a)).flex().items_center().justify_center()
                        .text_lg().font_weight(FontWeight::SEMIBOLD).text_color(rgb(0xaaaacc)).child(init)))
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

fn main() {
    let state = Rc::new(RefCell::new(TabState::new()));

    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(900.), px(250.)), cx);
        cx.open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), focus: true, ..Default::default() },
            |_window, cx| cx.new(|_cx| OverlayView { state: state.clone() }),
        ).unwrap();

        let s = state.clone();
        let _sub = Box::leak(Box::new(cx.observe_keystrokes(move |event: &KeystrokeEvent, _window: &mut Window, _app: &mut App| {
            let mut state = s.borrow_mut();
            let keystroke = &event.keystroke;
            let key = keystroke.key.as_str();
            let alt = keystroke.modifiers.alt;

            eprintln!("[KEYSTROKE] key={}, alt={}", key, alt);

            if alt && key == "tab" {
                if !state.visible {
                    state.refresh();
                    state.visible = true;
                    state.selected = if state.windows.len() > 1 { 1 } else { 0 };
                } else {
                    state.selected = (state.selected + 1) % state.windows.len().max(1);
                }
                _window.refresh();
            } else if !alt && state.visible {
                match key {
                    "tab" | "right" => { if !state.windows.is_empty() { state.selected = (state.selected + 1) % state.windows.len(); _window.refresh(); } }
                    "left" => { if !state.windows.is_empty() { state.selected = if state.selected == 0 { state.windows.len() - 1 } else { state.selected - 1 }; _window.refresh(); } }
                    "enter" => {
                        if let Some(w) = state.windows.get(state.selected) { activate_app(&w.app_name); }
                        state.visible = false;
                        _window.refresh();
                    }
                    "escape" => { state.visible = false; _window.refresh(); }
                    _ => {}
                }
            }
        })));

        cx.activate(true);
    });
}
