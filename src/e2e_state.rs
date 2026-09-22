//! A2 层 E2E 的状态快照:把 app 内部状态写成 JSON,供 CLI 脚本断言。
//!
//! 为什么需要它:A2 层(脚本 + cua-driver CLI,见 AGENTS.md「测试分层」)要么靠 AX 断言,
//! 要么靠截图目视。但有几类事实两样都拿不到——CALayer 内容(侧栏高亮 pill)不在 AX 里,
//! "当前选中第几张卡""本次抬的是哪个窗口"是纯内部状态,`verify_state` 也问不出来。于是让
//! app 自己把可判定的事实吐成文件:脚本读 JSON 做硬断言,cua 只负责**输入**与少数像素证据。
//!
//! 开启方式:`--e2e-state=<path>`(argv,见 dev_flags;未开启时全部函数立即返回,零副作用)。
//! 写入走 `<path>.tmp` + rename,读者不会读到半截 JSON;`seq` 单调递增,脚本据此判断"这一帧
//! 是新产生的",而不是靠 sleep。
//!
//! This module writes a JSON snapshot of internal state for the A2 end-to-end layer (script +
//! cua-driver CLI; see the testing tiers in AGENTS.md). AX cannot express CALayer content (the
//! sidebar highlight pill) and cannot express "which card is selected" or "which window was just
//! raised" at all, so the app states the checkable facts itself: the script asserts on JSON and cua
//! is left with input and a few pixel probes. Enabled by `--e2e-state=<path>`; without it every
//! function returns immediately. Writes go through `<path>.tmp` + rename so a reader never sees
//! half a document, and `seq` lets a script wait for a *new* frame instead of sleeping.

use objc2::msg_send;
use objc2::runtime::AnyObject;
use objc2::sel;
use objc2_foundation::NSRect;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use crate::log_debug;

static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
static SEQ: AtomicU64 = AtomicU64::new(0);

/// 解析一次 `--e2e-state=<path>`,之后走缓存。
/// Resolves `--e2e-state=<path>` once, then serves it from cache.
fn state_path() -> Option<&'static PathBuf> {
    PATH.get_or_init(|| crate::dev_flags::value("e2e-state").map(PathBuf::from))
        .as_ref()
}

/// 记录一次状态快照。只在主线程调用(透明地借用 AppState)。
/// Records one snapshot. Main thread only (it borrows AppState internally).
pub(crate) fn record(event: &str) {
    write(event, None);
}

/// 记录一次"抬窗"快照,附带本次提交的目标窗口。
/// 必须在清空选中态**之前**调用:浮窗一旦隐藏,AppState 里就没有选中索引了。
/// Records a commit snapshot carrying the window this release targets. Must run *before* the
/// selection is cleared: once the overlay hides, AppState no longer holds a selected index.
pub(crate) fn record_commit(pid: i32, window_id: u32, app: &str, index: usize) {
    write("commit", Some((pid, window_id, app.to_string(), index)));
}

/// 一张卡的快照(只留断言需要的字段,避免 JSON 变成内部结构的镜像)。
/// A card snapshot (only the fields assertions need, so the JSON is not a mirror of internals).
struct Card {
    pid: i32,
    window_id: u32,
    app: String,
    title: String,
    active: bool,
    minimized: bool,
    bounds: (f64, f64, f64, f64),
}

/// 视图树里的一个节点。frame 是**父视图坐标系**下的矩形,所以跨层比较无意义:脚本必须
/// 只比较 `parent` 相同的节点(侧栏高亮与侧栏按钮同父,页面内各行同父)。
/// One node of the view tree. `frame` is in the parent's coordinate space, so cross-level
/// comparisons are meaningless: scripts must only compare nodes sharing a `parent`.
struct ViewNode {
    root: &'static str,
    parent: i64,
    depth: usize,
    class: String,
    frame: (f64, f64, f64, f64),
    text: Option<String>,
}

/// 递归收集视图树。文本取自 `stringValue`(NSButton 标题 / NSTextField 文案),没有就为 null。
/// Walks the view tree recursively. `text` comes from `stringValue` (button title / text-field
/// content) and stays null when the view has none.
unsafe fn walk_views(
    view: *mut AnyObject,
    root: &'static str,
    parent: i64,
    depth: usize,
    out: &mut Vec<ViewNode>,
) {
    /// 深度与节点数上限:设置页是自建 view 树,防止极端情况下把 JSON 撑爆。
    /// Depth and node caps: the settings page builds its own view tree, so bound the JSON growth.
    const MAX_DEPTH: usize = 8;
    const MAX_NODES: usize = 3000;
    if view.is_null() || depth > MAX_DEPTH || out.len() >= MAX_NODES {
        return;
    }
    let frame: NSRect = msg_send![view, frame];
    let class: *mut AnyObject = msg_send![view, class];
    let class_name = if class.is_null() {
        String::new()
    } else {
        let description: *mut AnyObject = msg_send![class, description];
        crate::ffi::nsstring_to_rust(description)
    };
    let text = {
        let responds: bool = msg_send![view, respondsToSelector: sel!(stringValue)];
        if responds {
            let value: *mut AnyObject = msg_send![view, stringValue];
            let text = crate::ffi::nsstring_to_rust(value);
            (!text.is_empty()).then_some(text)
        } else {
            None
        }
    };
    let index = out.len() as i64;
    out.push(ViewNode {
        root,
        parent,
        depth,
        class: class_name,
        frame: (
            frame.origin.x,
            frame.origin.y,
            frame.size.width,
            frame.size.height,
        ),
        text,
    });
    let subviews: *mut AnyObject = msg_send![view, subviews];
    if subviews.is_null() {
        return;
    }
    let count: usize = msg_send![subviews, count];
    for position in 0..count {
        let child: *mut AnyObject = msg_send![subviews, objectAtIndex: position];
        walk_views(child, root, index, depth + 1, out);
    }
}

fn collect_views() -> Vec<ViewNode> {
    let mut nodes = Vec::new();
    for (root, view) in crate::settings::e2e_view_roots() {
        unsafe { walk_views(view, root, -1, 0, &mut nodes) };
    }
    nodes
}

struct Snapshot {
    visible: bool,
    selected: usize,
    windows: Vec<Card>,
}

fn collect() -> Snapshot {
    crate::with_tab_state(|state_opt| match state_opt.as_ref() {
        Some(state) => Snapshot {
            visible: state.visible,
            selected: state.selected,
            windows: state
                .windows
                .iter()
                .map(|w| Card {
                    pid: w.pid,
                    window_id: w.window_id,
                    app: w.app_name.clone(),
                    title: w.window_title.clone(),
                    active: w.is_active,
                    minimized: w.minimized,
                    bounds: w.bounds,
                })
                .collect(),
        },
        None => Snapshot {
            visible: false,
            selected: 0,
            windows: Vec::new(),
        },
    })
}

fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn write(event: &str, committed: Option<(i32, u32, String, usize)>) {
    let Some(path) = state_path() else {
        return;
    };
    let snapshot = collect();
    let seq = SEQ.fetch_add(1, Ordering::SeqCst) + 1;
    let (front_app, front_pid) = crate::ffi::frontmost_app_info();
    let thumbnails = crate::overlay::thumbnail_visible_range();

    let mut json = String::with_capacity(2048);
    json.push_str("{\n");
    json.push_str(&format!("  \"seq\": {seq},\n"));
    json.push_str(&format!("  \"event\": {},\n", json_string(event)));
    json.push_str(&format!("  \"visible\": {},\n", snapshot.visible));
    json.push_str(&format!("  \"selected_index\": {},\n", snapshot.selected));
    json.push_str(&format!("  \"cards_count\": {},\n", snapshot.windows.len()));
    let selected_key = snapshot.windows.get(snapshot.selected);
    match selected_key {
        Some(card) => json.push_str(&format!(
            "  \"selected_key\": {{\"pid\": {}, \"window_id\": {}}},\n",
            card.pid, card.window_id
        )),
        None => json.push_str("  \"selected_key\": null,\n"),
    }
    match committed {
        Some((pid, window_id, ref app, index)) => json.push_str(&format!(
            "  \"committed\": {{\"pid\": {pid}, \"window_id\": {window_id}, \"app\": {}, \"index\": {index}}},\n",
            json_string(app)
        )),
        None => json.push_str("  \"committed\": null,\n"),
    }
    json.push_str(&format!(
        "  \"frontmost\": {{\"app\": {}, \"pid\": {front_pid}}},\n",
        json_string(&front_app)
    ));
    json.push_str(&format!(
        "  \"permissions\": {{\"accessibility\": {}, \"screen_recording\": {}}},\n",
        crate::ffi::has_accessibility_permission(),
        crate::thumbnail::capture_allowed()
    ));
    json.push_str(&format!(
        "  \"selected_sidebar\": {},\n",
        crate::settings::e2e_selected_sidebar()
    ));
    json.push_str("  \"views\": [");
    for (index, node) in collect_views().iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            "\n    {{\"root\": {}, \"parent\": {}, \"depth\": {}, \"class\": {}, \"frame\": [{}, {}, {}, {}], \"text\": {}}}",
            json_string(node.root),
            node.parent,
            node.depth,
            json_string(&node.class),
            node.frame.0,
            node.frame.1,
            node.frame.2,
            node.frame.3,
            match &node.text {
                Some(text) => json_string(text),
                None => "null".to_string(),
            }
        ));
    }
    json.push_str("\n  ],\n");
    match thumbnails {
        Some(range) => json.push_str(&format!(
            "  \"thumbnail_range\": [{}, {}],\n",
            range.start, range.end
        )),
        None => json.push_str("  \"thumbnail_range\": null,\n"),
    }
    json.push_str("  \"cards\": [");
    for (index, card) in snapshot.windows.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            "\n    {{\"index\": {index}, \"pid\": {}, \"window_id\": {}, \"app\": {}, \"title\": {}, \"active\": {}, \"minimized\": {}, \"bounds\": [{}, {}, {}, {}]}}",
            card.pid,
            card.window_id,
            json_string(&card.app),
            json_string(&card.title),
            card.active,
            card.minimized,
            card.bounds.0,
            card.bounds.1,
            card.bounds.2,
            card.bounds.3
        ));
    }
    json.push_str("\n  ]\n}\n");

    write_atomically(path, &json);
}

fn write_atomically(path: &Path, contents: &str) {
    let tmp = path.with_extension("tmp");
    let result = std::fs::File::create(&tmp)
        .and_then(|mut file| {
            file.write_all(contents.as_bytes())?;
            file.sync_all()
        })
        .and_then(|()| std::fs::rename(&tmp, path));
    if let Err(error) = result {
        log_debug!("[e2e-state] write failed for {}: {error}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// JSON 里出现的窗口标题/应用名是外部数据,必须转义——否则标题里的引号会把文档写坏。
    /// Window titles and app names are external data, so they must be escaped: a quote in a title
    /// would otherwise corrupt the document.
    #[test]
    fn json_string_escapes_external_text() {
        assert_eq!(json_string("plain"), "\"plain\"");
        assert_eq!(json_string("a\"b"), "\"a\\\"b\"");
        assert_eq!(json_string("a\\b"), "\"a\\\\b\"");
        assert_eq!(json_string("a\nb\tc"), "\"a\\nb\\tc\"");
        assert_eq!(json_string("bell\u{7}"), "\"bell\\u0007\"");
        // 中文标题原样保留(不转成 \u 形式),便于脚本里直接 grep。
        // Non-ASCII titles stay literal rather than \\u-escaped so scripts can grep them directly.
        assert_eq!(json_string("微信 — 聊天"), "\"微信 — 聊天\"");
    }
}
