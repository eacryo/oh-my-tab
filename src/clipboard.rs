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
use crate::i18n::t;
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
/// 浮窗最小高度(内容再少也不低于此,约 5-6 行的高度)。
/// The picker's minimum height (never smaller, ~5-6 rows worth), also applied to the empty
/// state for a consistent look.
const PICKER_MIN_HEIGHT: f64 = 250.0;
/// 浮窗宽度 / picker width.
const PICKER_W: f64 = 420.0;
/// 行按钮内的行高(13pt 字体的行高约 17.5pt,留余量取 20)。
/// Line height inside a row button (13pt font's line height is ~17.5pt; 20 keeps headroom).
const LINE_H: f64 = 20.0;
/// 行按钮内上下内边距 / row button vertical padding.
const BTN_PAD_Y: f64 = 3.0;
/// 行按钮之间的间距 / gap between row buttons.
const ROW_GAP: f64 = 8.0;
/// 每条文本最多显示的行数(超出第 3 行截断加省略号)。
/// Max lines shown per entry (truncated with an ellipsis beyond line 3).
const MAX_TEXT_LINES: usize = 3;
/// 单行显示宽度上限(以 ASCII 字符为单位;中文/全角按 2 折算)。
/// Per-line width cap in ASCII-character units (CJK/full-width chars count as 2).
const LINE_MAX_UNITS: usize = 60;
/// 上下留白 / vertical padding.
const PAD_Y: f64 = 10.0;
/// 右上角"清除全部"按钮尺寸 / the "clear all" button's size.
const CLEAR_BTN_W: f64 = 60.0;
const CLEAR_BTN_H: f64 = 22.0;
/// 清除按钮与行列表的间距 / gap between the clear button and the row list.
const CLEAR_BTN_GAP: f64 = 6.0;
/// 顶部搜索框宽度(清除按钮左侧)/ the top search field's width (left of the clear button).
const SEARCH_BAR_W: f64 = 240.0;
/// 行内图钉按钮宽度 / the per-row pin button width.
const PIN_BTN_W: f64 = 24.0;
/// 行内图钉按钮高度(标题栏内,矮小)/ the per-row pin button height (inside the header bar).
const PIN_BTN_H: f64 = 14.0;
/// 行内删除按钮宽度 / the per-row delete button width.
const DEL_BTN_W: f64 = 24.0;
/// 行内删除按钮高度(标题栏内,矮小)/ the per-row delete button height (inside the header bar).
const DEL_BTN_H: f64 = 14.0;
/// 条目标题栏高度(来源应用名 + 图钉/删除图标)/ the per-entry header bar height (source app
/// name + pin/delete icons).
const HEADER_H: f64 = 22.0;
/// 标题栏与正文按钮的间隙 / gap between the header bar and the body button.
const BODY_GAP: f64 = 2.0;
/// 正文左右内边距:文字不要贴卡片边缘(卡片 x=PAD_X,文字再缩进 BODY_PAD_X)。
/// Body horizontal padding: the text shouldn't hug the card's edge (the card starts at
/// PAD_X; the text is inset by another BODY_PAD_X on each side).
const BODY_PAD_X: f64 = 6.0;
/// 标题栏图标与标题文字的间距 / the header icon-to-title gap.
const HEADER_ICON_GAP: f64 = 4.0;
/// 行内图标按钮(图钉/删除)的着色:比系统 labelColor 稍深,浅色界面上更清晰。
/// Tint for the per-row icon buttons (pin/delete): slightly darker than the system
/// labelColor for legibility on light glass.
const ROW_ICON_TINT: f64 = 0.25;
/// 左右留白 / horizontal padding.
const PAD_X: f64 = 12.0;
/// 玻璃圆角:小浮窗固定小圆角,不跟随 config 的大圆角(那会让 420pt 小窗成胶囊)。
/// Fixed small corner radius for the glass: NOT the config value (which would turn a 420pt
/// panel into a capsule).
const CORNER_R: f64 = 14.0;
/// 选中行的圆角背景块圆角 / selected-row highlight tile corner radius.
const SEL_TILE_R: f64 = 9.0;
/// 卡片细边框:白色半透明,把卡片从亮玻璃上"立起来"。
/// Card hairline border: translucent white, lifting the cards off the bright glass.
const CARD_BORDER_ALPHA: f64 = 0.6;
/// 卡片细边框宽度(pt)/ card hairline border width (pt).
const CARD_BORDER_W: f64 = 0.5;
/// 标题栏横条颜色:浅色磨砂(白 0.18 叠在卡片上,靠轻微亮度差分层,比黑色灰条现代)。
/// Header strip: light frosted (white 0.18 over the card; a subtle brightness step layers it
/// cleanly, more modern than the dark gray band).
const HEADER_BG_ALPHA: f64 = 0.18;
/// 选中行标题栏:accent 加深一档(0.5),避免"accent 卡 + 灰条"两色拼贴。
/// The selected row's header: a deeper accent (0.5), avoiding the two-tone "accent card +
/// gray strip" patchwork.
const HEADER_SEL_ALPHA: f64 = 0.5;
/// 每行背景块透明度(白色,明暗玻璃上都清晰)/ per-row tile alpha (white; legible on both
/// light and dark glass). 8% 在亮色玻璃上不可见(玻璃本身亮度 ~0.78,零对比度),提到
/// 0.35 才能形成清晰的磨砂卡片观感。8% vanished on the bright glass (the glass itself is
/// ~0.78 luminance -- zero contrast); 0.35 reads as a visible frosted card.
const ROW_TILE_ALPHA: f64 = 0.35;
/// 自定义滚动指示器宽度 / custom scroll indicator width.
const SCROLL_INDICATOR_W: f64 = 4.0;
/// 指示器最短显示长度(条太短不可读)/ minimum indicator length (too short is unreadable).
const SCROLL_INDICATOR_MIN_LEN: f64 = 24.0;

// ========== 状态 / state ==========

/// 历史列表,最新在前 / history, newest first.
static CLIP_HISTORY: LazyLock<Mutex<Vec<ClipEntry>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// 上次读到的 changeCount(变化才读剪贴板)/ last observed changeCount (read only on change).
static LAST_CHANGE_COUNT: LazyLock<Mutex<i64>> = LazyLock::new(|| Mutex::new(-1));

/// 轮询 timer(主线程)/ the polling timer (main thread).
static POLL_TIMER: OnceLock<Mutex<ObjPtr>> = OnceLock::new();

/// 浮窗是否可见 / whether the picker is visible.
static PICKER_VISIBLE: AtomicBool = AtomicBool::new(false);

/// 当前选中行索引 / the currently selected row index.
static PICKER_SELECTION: Mutex<usize> = Mutex::new(0);

/// 无选中行的哨兵值:焦点在搜索框时使用(↑ 从列表顶跳入搜索框 / 点击搜索框),
/// 此时列表不该有高光;↓ 回列表时 search_field_do_command 重置为 0。
/// Sentinel for "no selected row": used while the search field is focused (↑ from the list
/// top into the search field, or a click on it), so no row keeps its highlight; ↓ back into
/// the list resets it to 0 in search_field_do_command.
const NO_SELECTION: usize = usize::MAX;

/// 浮窗窗口 / the picker window.
static PICKER_WINDOW: Mutex<Option<ObjPtr>> = Mutex::new(None);

/// 浮窗容器(接收键盘)/ the picker container (receives key events).
static PICKER_CONTAINER: Mutex<Option<ObjPtr>> = Mutex::new(None);

/// 每行按钮指针(按行索引,供高亮/点击)/ row button pointers by index (highlight / click).
static ROW_BUTTONS: LazyLock<Mutex<Vec<ObjPtr>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// 每行背景块视图(与 ROW_BUTTONS 一一对应、同顺序;选中行不创建)。
/// Per-row background tiles (one per entry, same order as ROW_BUTTONS; skipped for the
/// selected row).
static ROW_TILES: LazyLock<Mutex<Vec<ObjPtr>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// 每行的实际行距(按钮高 + 间距,随换行行数变化)/ per-row pitch (button height + gap,
/// varies with the wrapped line count).
static ROW_PITCHES: LazyLock<Mutex<Vec<f64>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// 顶部搜索框指针 / the top search field.
static SEARCH_FIELD: Mutex<Option<ObjPtr>> = Mutex::new(None);

/// 当前搜索词(空 = 不过滤)。/ The current search query (empty = no filtering).
static SEARCH_QUERY: LazyLock<Mutex<String>> = LazyLock::new(|| Mutex::new(String::new()));

/// 当前显示列表:历史索引(过滤后的顺序)。空查询时 = 全部索引。
/// The current display list: history indices (filtered order). All indices when no query.
static FILTERED: LazyLock<Mutex<Vec<usize>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// 滚动视图 / the scroll view.
static SCROLL_VIEW: Mutex<Option<ObjPtr>> = Mutex::new(None);

/// 自定义滚动指示器 / the custom scroll indicator view.
static SCROLL_INDICATOR: Mutex<Option<ObjPtr>> = Mutex::new(None);

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

/// 按显示宽度估算文本的换行行数:ASCII 字符记 1 单位,中文/全角记 2;显式换行符单独成行。
/// Estimate the wrapped line count by display width: ASCII counts as 1 unit, CJK/full-width
/// as 2; explicit newlines start a new line.
fn estimate_lines(text: &str, max_units: usize) -> usize {
    let mut units = 0usize;
    let mut lines = 1usize;
    for ch in text.chars() {
        if ch == '\n' {
            lines += 1;
            units = 0;
            continue;
        }
        let w = if ch.is_ascii() { 1 } else { 2 };
        if units + w > max_units {
            lines += 1;
            units = w;
        } else {
            units += w;
        }
    }
    lines
}

/// 把文本截断为最多 max_lines 行(按显示宽度),截断处(第 max_lines 行末尾)加省略号。
/// Truncate the text to at most `max_lines` display lines (by width), appending an ellipsis
/// at the truncation point (the end of the last kept line).
fn truncate_to_lines(text: &str, max_units: usize, max_lines: usize) -> String {
    let mut out = String::new();
    let mut units = 0usize;
    let mut lines = 1usize;
    for ch in text.chars() {
        if ch == '\n' {
            if lines >= max_lines {
                out.push('…');
                return out;
            }
            lines += 1;
            units = 0;
            out.push('\n');
            continue;
        }
        let w = if ch.is_ascii() { 1 } else { 2 };
        if units + w > max_units {
            if lines >= max_lines {
                out.push('…');
                return out;
            }
            lines += 1;
            units = w;
            out.push(ch);
        } else {
            units += w;
            out.push(ch);
        }
    }
    out
}

/// 文本的行数(上限 MAX_TEXT_LINES,超出按 3 行计——第 3 行截断)。
/// The entry's display line count (capped at MAX_TEXT_LINES; beyond that it renders 3 lines
/// with the last one truncated).
fn text_lines(text: &str) -> usize {
    estimate_lines(text, LINE_MAX_UNITS).min(MAX_TEXT_LINES)
}

/// 行按钮高度 = 行数 * 行高 + 上下内边距(按钮随文本行数紧凑包裹)。
/// Row-button height = lines * line height + vertical padding (the button hugs its text).
fn row_button_height(lines: usize) -> f64 {
    lines as f64 * LINE_H + BTN_PAD_Y * 2.0
}

/// 行距(按钮高 + 间距)。/ Row pitch (button height + gap).
fn row_pitch(btn_h: f64) -> f64 {
    btn_h + ROW_GAP
}

/// 是否显示来源应用(读 CONFIG;记录始终进行,开关只控制标题栏里的名称显示)。
/// Whether the source app name is shown (reads CONFIG; recording is always on, the toggle
/// only gates the name in the header bar).
fn show_source_app() -> bool {
    CONFIG.read().unwrap().clipboard.show_source_app
}

/// 标题栏文字:开关开 → 来源应用名(无来源显示"未知来源");开关关 → 空串(横条只放图标)。
/// The header text: toggle on -> the source app name ("unknown source" when absent);
/// toggle off -> empty (the strip only hosts the icons).
fn header_title(entry: &ClipEntry, show_source: bool) -> String {
    if !show_source {
        return String::new();
    }
    if entry.source_app.is_empty() {
        t("clipboard.unknown_source")
    } else {
        entry.source_app.clone()
    }
}

/// 正文按钮高度 = 正文行数(≤3),紧凑包裹文本、顶对齐在条目空间内(标题栏恒占顶部)。
/// The body button height = text lines (≤3); it hugs the text, top-aligned inside the slot
/// (the header bar always occupies the top).
fn body_button_height(entry: &ClipEntry) -> f64 {
    row_button_height(text_lines(&entry.text))
}

/// 计算一批文本的每行行距。**所有条目固定同一行距** = 标题栏 + 正文 3 行:列表高度
/// 整齐,短文本的按钮紧凑包裹文本、顶对齐在条目空间内。
/// Compute the per-row pitches. **All entries share ONE fixed pitch** = header + 3 text
/// lines: the list height stays even, and short-text buttons hug their text, top-aligned
/// inside the slot.
fn compute_pitches(texts: &[ClipEntry]) -> Vec<f64> {
    // 标题栏 + 间隙 + 3 行正文 + 行距。
    // Header + gap + 3 body lines + row gap.
    let fixed_pitch = HEADER_H + BODY_GAP + row_button_height(MAX_TEXT_LINES) + ROW_GAP;
    let _ = texts;
    texts.iter().map(|_| fixed_pitch).collect()
}

/// 行列表的顶部偏移:顶部留白 + 清除按钮行 + 按钮与列表间距。
/// The row list's top offset: top padding + the clear button row + its gap.
fn rows_top_offset() -> f64 {
    PAD_Y + CLEAR_BTN_H + CLEAR_BTN_GAP
}

/// 第 idx 行的顶部 y(flipped 坐标):rows_top_offset + 前 idx 行行距之和。
/// The top y of row `idx` (flipped coords): rows_top_offset + the pitches before it.
fn row_top(idx: usize, pitches: &[f64]) -> f64 {
    rows_top_offset() + pitches.iter().take(idx).sum::<f64>()
}

/// 历史条目:文本 + 置顶标记 + 来源应用名 + 来源图标缓存键。置顶条目恒在列表顶部。
/// A history entry: text + a pinned flag + the source app name + the source icon-cache key.
/// Pinned entries stay at the top.
#[derive(Debug, Clone, PartialEq)]
struct ClipEntry {
    text: String,
    pinned: bool,
    /// 复制该文本时的前台应用名(空 = 未知,如旧条目/取不到前台应用)。
    /// The frontmost app name when the text was copied (empty = unknown, e.g. legacy entries
    /// or an unavailable frontmost app).
    source_app: String,
    /// 来源应用的图标缓存键(resolve_app_identity: bundle id > exec 路径哈希 > pid)。
    /// 空 = 取不到身份(旧条目等),标题栏不显示图标。
    /// The source app's icon-cache key (resolve_app_identity: bundle id > exec-path hash >
    /// pid). Empty = no identity (e.g. legacy entries) -> no icon in the header.
    source_key: String,
}

/// 新条目(非置顶)应插入的位置:置顶区之后(第一个非置顶条目的下标)。
/// The insertion index for a new (unpinned) entry: right after the pinned block.
fn insert_position(history: &[ClipEntry]) -> usize {
    history.iter().take_while(|e| e.pinned).count()
}

/// 按文本查找条目下标;找不到返回 None。
/// Find an entry's index by text; None when absent.
fn find_by_text(history: &[ClipEntry], text: &str) -> Option<usize> {
    history.iter().position(|e| e.text == text)
}

/// 把已存在的条目提到最前:保留其置顶状态——置顶条目移到置顶区顶部,
/// 未置顶条目移到非置顶区顶部(即"最新位置")。列表因此永不重复。
/// Move an existing entry to the front, KEEPING its pinned state: pinned entries go to the
/// top of the pinned block, unpinned ones to the top of the unpinned block (the newest
/// slot). The list therefore never holds duplicates.
fn move_entry_to_front(history: &mut Vec<ClipEntry>, idx: usize) {
    if idx >= history.len() {
        return;
    }
    let e = history.remove(idx);
    let pos = if e.pinned {
        0
    } else {
        insert_position(history)
    };
    history.insert(pos, e);
}

/// 把新文本记入历史。规则:
/// - 空文本忽略
/// - 全表查重:文本已存在 → 把旧条目提到最前(保留置顶状态,见 move_entry_to_front),
///   并把来源(名称 + 图标键)更新为本次复制的来源(它是"最新一次复制"的来源)
/// - 未命中 → 新条目插到置顶区之后;超出 max 裁剪最旧条目
///
/// 返回是否真正写入。
///
/// Record a new text into the history:
/// - empty text is ignored
/// - full-list dedup: an existing text is moved to the front (pinned state kept, see
///   move_entry_to_front), and its source (name + icon key) is updated to this copy's source
///   (it is the "latest copy" now)
/// - a new text is inserted after the pinned block; entries beyond `max` are trimmed
///
/// Returns whether something was actually recorded.
fn record_text(
    history: &mut Vec<ClipEntry>,
    text: &str,
    source: &str,
    source_key: &str,
    max: usize,
) -> bool {
    if text.is_empty() || max == 0 {
        return false;
    }
    if let Some(idx) = find_by_text(history, text) {
        history[idx].source_app = source.to_string();
        history[idx].source_key = source_key.to_string();
        move_entry_to_front(history, idx);
        return true;
    }
    let pos = insert_position(history);
    history.insert(
        pos,
        ClipEntry {
            text: text.to_string(),
            pinned: false,
            source_app: source.to_string(),
            source_key: source_key.to_string(),
        },
    );
    if history.len() > max {
        history.truncate(max);
    }
    true
}

/// 置顶第 idx 条:移到置顶区顶部(最上)。
/// Pin entry `idx`: move it to the top of the pinned block.
fn pin_entry(history: &mut Vec<ClipEntry>, idx: usize) {
    if idx >= history.len() || history[idx].pinned {
        return;
    }
    let mut e = history.remove(idx);
    e.pinned = true;
    history.insert(0, e);
}

/// 取消第 idx 条的置顶:移到非置顶区顶部(最新位置)。
/// Unpin entry `idx`: move it to the top of the unpinned block (the newest slot).
fn unpin_entry(history: &mut Vec<ClipEntry>, idx: usize) {
    if idx >= history.len() || !history[idx].pinned {
        return;
    }
    let mut e = history.remove(idx);
    e.pinned = false;
    let pos = insert_position(history);
    history.insert(pos, e);
}

/// 删除第 idx 条(越界忽略)。/ Delete entry `idx` (out of range is ignored).
fn delete_entry(history: &mut Vec<ClipEntry>, idx: usize) {
    if idx < history.len() {
        history.remove(idx);
    }
}

/// 过滤:返回匹配 query 的历史索引列表(空 query = 全部;大小写不敏感子串匹配)。
/// Filter: indices of entries matching `query` (empty query = all; case-insensitive substring).
fn filtered_indices(history: &[ClipEntry], query: &str) -> Vec<usize> {
    if query.is_empty() {
        return (0..history.len()).collect();
    }
    let q = query.to_lowercase();
    history
        .iter()
        .enumerate()
        .filter(|(_, e)| e.text.to_lowercase().contains(&q))
        .map(|(i, _)| i)
        .collect()
}

/// 显示索引 → 历史索引(经当前过滤列表映射;越界返回 None)。
/// Display index -> history index (via the current filtered list; None when out of range).
fn mapped_index(display_idx: usize) -> Option<usize> {
    FILTERED.lock().unwrap().get(display_idx).copied()
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
    // 日志只打元数据,绝不打剪贴板内容(隐私:内容可能是密码/正文)。
    // Log metadata only, NEVER the clipboard text (privacy: it may be a password/body text).
    log_debug!(
        "[clip] write back {} chars (setString ok={})",
        text.chars().count(),
        ok
    );
    CFRelease(type_ns as *const c_void);
    CFRelease(ns as *const c_void);
}

// ========== 轮询 / polling ==========

/// 轮询一次:changeCount 变化时读文本入历史。
/// Poll once: read the text into history when changeCount changed.
fn poll_clipboard() {
    // 总开关关闭时停止记录(timer 已被 stop() 停掉,但全局通知观察者仍在,
    // 回调必须自行检查——否则关闭后历史还在后台累积)。
    // Stop recording when the master switch is off (stop() kills the timer, but the
    // process-wide pasteboard notification observer stays registered, so the callback must
    // check the switch itself -- otherwise history keeps accumulating while disabled).
    if !CONFIG.read().unwrap().clipboard.enabled {
        return;
    }
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
            // 来源 = 复制瞬间的前台应用(始终记录;显示与否由 CONFIG 的
            // clipboard.show_source_app 决定)。轮询间隔(0.5s)内切应用可能记错来源,
            // 通知路径(changeCount 变化即时回调)则基本精确,第一版接受这个误差。
            // 图标缓存键 = resolve_app_identity 的 key(与切换器同一套回退);
            // 顺带提取 16pt 小图标(app 此刻存活,提取最可靠;失败不影响记录)。
            // Source = the frontmost app at copy time (always recorded; whether it is shown
            // is gated by CONFIG.clipboard.show_source_app). Switching apps within the 0.5s
            // poll interval can misattribute; the notification path (immediate callback) is
            // mostly accurate -- acceptable for v1. The icon-cache key comes from
            // resolve_app_identity (same fallbacks as the switcher); the 16pt small icon is
            // extracted here too (the app is alive now -- the most reliable moment; failure
            // only means no icon).
            let (source, pid) = crate::ffi::frontmost_app_info();
            let source_key = if pid > 0 {
                let id = unsafe { crate::window_collector::resolve_app_identity(pid) };
                let key = id.key.clone();
                let _ = crate::window_collector::extract_small_icon(pid);
                key
            } else {
                String::new()
            };
            let mut hist = CLIP_HISTORY.lock().unwrap();
            if record_text(&mut hist, &text, &source, &source_key, max_entries()) {
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
            // scheduledTimerWithTimeInterval: 返回 +0(runloop 持有),不能 release
            // (over-release 会崩溃);invalidate 后 runloop 自行释放。
            // scheduledTimerWithTimeInterval: returns +0 (owned by the run loop); it must
            // NOT be released (over-release crashes); invalidate lets the run loop release it.
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
            class_addMethod(
                cls,
                sel!(scrollIndicatorBoundsChanged:),
                scroll_indicator_bounds_changed as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(clearClipboardHistory:),
                clear_clipboard_history as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(searchFieldChanged:),
                search_field_changed as *mut c_void,
                types.as_ptr(),
            );
            // 搜索框 delegate:拦截字段编辑器翻译出的命令(如 ↓ → moveDown:)。
            // Search-field delegate: intercepts commands the field editor translates
            // (e.g. ↓ -> moveDown:).
            let types_cmd = CString::new("B@:@@:").unwrap();
            class_addMethod(
                cls,
                sel!(control:textView:doCommandBySelector:),
                search_field_do_command as *mut c_void,
                types_cmd.as_ptr(),
            );
            // 搜索框开始编辑(↑ 从列表顶跳入 / 鼠标点击)→ 清除列表选中高光。
            // The search field begins editing (↑ from the list top / a mouse click) ->
            // clear the list's selection highlight.
            class_addMethod(
                cls,
                sel!(controlTextDidBeginEditing:),
                search_field_began_editing as *mut c_void,
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

/// 更新滚动指示器的位置/长度:内容溢出时显示(恒显示,不淡出),否则隐藏。
/// 由 clipView 的 bounds 变化通知回调与 show_picker(首次呼出即显示)调用。
/// Update the scroll indicator's position/length: shown while the content overflows
/// (always visible, no fade-out), hidden otherwise. Called by the clip-view bounds-change
/// notification callback AND by show_picker (visible on the first summon).
fn update_scroll_indicator() {
    unsafe {
        let scroll = match *SCROLL_VIEW.lock().unwrap() {
            Some(s) => s.0,
            None => return,
        };
        let indicator = match *SCROLL_INDICATOR.lock().unwrap() {
            Some(i) => i.0,
            None => return,
        };
        let clip: *mut AnyObject = msg_send![scroll, contentView];
        let clip_bounds: NSRect = msg_send![clip, bounds];
        let visible_h = clip_bounds.size.height;
        let doc: *mut AnyObject = msg_send![scroll, documentView];
        let doc_h = if doc.is_null() {
            0.0
        } else {
            let df: NSRect = msg_send![doc, frame];
            df.size.height
        };

        // 需要滚动(内容超出可视高度)才显示;无需滚动则隐藏。
        // Shown only when scrolling is needed (content exceeds the visible height).
        if doc_h <= visible_h || visible_h <= 0.0 {
            let _: () = msg_send![indicator, setHidden: true];
            return;
        }
        let ratio = visible_h / doc_h;
        let knob_h = (visible_h * ratio)
            .max(SCROLL_INDICATOR_MIN_LEN)
            .min(visible_h - 6.0);
        let mut knob_y = 3.0 + clip_bounds.origin.y * ratio;
        if knob_y + knob_h > visible_h - 3.0 {
            knob_y = visible_h - 3.0 - knob_h;
        }
        if knob_y < 3.0 {
            knob_y = 3.0;
        }
        let _: () = msg_send![
            indicator,
            setFrame: NSRect::new(
                NSPoint::new(
                    clip_bounds.size.width - SCROLL_INDICATOR_W - 3.0,
                    knob_y
                ),
                NSSize::new(SCROLL_INDICATOR_W, knob_h)
            )
        ];
        let _: () = msg_send![indicator, setHidden: false];
    }
}

/// clipView bounds 变化通知回调(滚动发生)→ 更新指示器。
/// Clip-view bounds-change notification callback (scrolling) -> update the indicator.
extern "C" fn scroll_indicator_bounds_changed(_self: *mut c_void, _cmd: Sel, _note: *mut c_void) {
    update_scroll_indicator();
}

/// "清除全部"按钮回调:清空剪贴板历史并关闭浮窗(空历史呼出会被忽略)。
/// "Clear all" button callback: empty the clipboard history and close the picker (an empty
/// history is ignored on summon).
extern "C" fn clear_clipboard_history(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    // 清除全部时保留置顶条目(置顶 = 用户主动保存的常用内容)。
    // "Clear all" keeps the pinned entries (pinned = content the user deliberately saved).
    let mut hist = CLIP_HISTORY.lock().unwrap();
    let kept = hist.iter().filter(|e| e.pinned).count();
    hist.retain(|e| e.pinned);
    log_info!(
        "Clipboard history cleared by user ({} pinned entries kept).",
        kept
    );
    drop(hist);
    // 顺带清空搜索词与搜索框文本。
    // Also clear the search query and the search field's text.
    clear_search();
    hide_picker();
}

/// 清空搜索词 + 搜索框文本(不重建;调用方按需 rebuild)。
/// Clear the search query and the search field's text (no rebuild; callers rebuild as needed).
fn clear_search() {
    SEARCH_QUERY.lock().unwrap().clear();
    if let Some(f) = *SEARCH_FIELD.lock().unwrap() {
        unsafe {
            let empty_ns = make_nsstring("");
            let _: () = msg_send![f.0, setStringValue: empty_ns];
            CFRelease(empty_ns as *const c_void);
        }
    }
}

/// 搜索框文本变化通知回调:更新搜索词并重建过滤列表。
/// Search-field text-change notification callback: update the query and rebuild the filter.
extern "C" fn search_field_changed(_self: *mut c_void, _cmd: Sel, note: *mut c_void) {
    let field: *mut AnyObject = unsafe { msg_send![note as *mut AnyObject, object] };
    if field.is_null() {
        return;
    }
    let s: *mut AnyObject = unsafe { msg_send![field, stringValue] };
    let q = unsafe { nsstring_to_rust(s) };
    *SEARCH_QUERY.lock().unwrap() = q;
    // 不重置选中:编辑期间(焦点在搜索框)保持无选中;回列表时(↓)由
    // search_field_do_command 重置为首条。
    // Do NOT reset the selection: while editing (focus in the search field) it stays
    // "no selection"; returning to the list (↓) resets it to the first entry in
    // search_field_do_command.
    unsafe { rebuild_rows() };
}

/// NSSearchField 的 Esc(cancelOperation:):有搜索词 → 清空并恢复全列表(方案 A 第一级);
/// 无搜索词 → 关闭浮窗(第二级)。
/// NSSearchField's Esc (cancelOperation:): a query gets cleared and the full list restored
/// (scheme A, level one); with no query the picker closes (level two).
extern "C" fn search_field_cancel(_self: *mut c_void, _cmd: Sel) {
    let has_query = !SEARCH_QUERY.lock().unwrap().is_empty();
    if has_query {
        clear_search();
        // 焦点仍在搜索框,保持无选中(高光不恢复)。
        // Focus stays in the search field: keep "no selection" (no highlight returns).
        unsafe { rebuild_rows() };
    } else {
        hide_picker();
    }
}

/// 搜索框 delegate 的命令拦截:↓(moveDown:) → 焦点切到列表并选中过滤结果第一条,返回
/// YES 吞掉该命令;其余命令返回 NO 交给字段编辑器正常处理(光标移动/输入等)。
/// Search-field delegate command interception: ↓ (moveDown:) moves focus into the list and
/// selects the first filtered entry, returning YES (consumed); any other command returns NO
/// so the field editor handles it (cursor movement / text input).
///
/// 为什么必须走这里:搜索框开始编辑后第一响应者是窗口的字段编辑器(NSTextView),键盘事件
/// 根本不经过搜索框的 keyDown:;编辑器把 ↓ 翻译成 moveDown: 命令后通过
/// control:textView:doCommandBySelector: 转发给搜索框的 delegate——这是文本控件拦截按键
/// 的官方机制。
/// Why this is necessary: once the search field edits, the FIRST RESPONDER is the window's
/// field editor (an NSTextView) -- key events never reach the search field's keyDown:. The
/// editor translates ↓ into a moveDown: command and forwards it to the field's delegate via
/// control:textView:doCommandBySelector: -- the official way to intercept keys on text controls.
extern "C" fn search_field_do_command(
    _self: *mut c_void,
    _cmd: Sel,
    _control: *mut c_void,
    _text_view: *mut c_void,
    command_selector: Sel,
) -> bool {
    if command_selector != sel!(moveDown:) {
        return false;
    }
    unsafe {
        // 搜索词/过滤结果保留,仅把焦点与选中交给列表。
        // The query/filter stays; only focus and the selection move to the list.
        *PICKER_SELECTION.lock().unwrap() = 0;
        rebuild_rows();
        if let Some(c) = *PICKER_CONTAINER.lock().unwrap() {
            let window = match *PICKER_WINDOW.lock().unwrap() {
                Some(w) => w.0,
                None => return true,
            };
            // makeFirstResponder: 返回 BOOL('B')。
            // makeFirstResponder: returns BOOL ('B').
            let _: bool = msg_send![window, makeFirstResponder: c.0];
        }
    }
    true
}

/// 搜索框开始编辑(↑ 从列表顶跳入 / 鼠标点击):清除列表选中,高光消失。
/// 通知在 makeFirstResponder: 同步派发;selection 未变化(如再次聚焦)时跳过重建。
/// The search field begins editing (↑ from the list top / a mouse click): clear the list's
/// selection so the highlight disappears. The notification fires synchronously inside
/// makeFirstResponder:; skip the rebuild when the selection is unchanged (e.g. re-focus).
extern "C" fn search_field_began_editing(_self: *mut c_void, _cmd: Sel, _note: *mut c_void) {
    let mut sel = PICKER_SELECTION.lock().unwrap();
    if *sel == NO_SELECTION {
        return;
    }
    *sel = NO_SELECTION;
    drop(sel);
    unsafe { rebuild_rows() };
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

/// 浮窗相对光标的偏移(右下 16pt;空间不足时翻转到左/上)。
/// The picker's offset from the cursor (16pt to the bottom-right; flips to the left/top
/// when there isn't room).
const PICKER_CURSOR_OFF: f64 = 16.0;
/// 边缘最小留白 / minimum margin from the screen edge.
const PICKER_EDGE_MARGIN: f64 = 8.0;

/// 纯逻辑:包含光标的屏幕 frame(找不到返回 None)。
/// Pure: the screen frame containing the cursor (None when no screen contains it).
fn screen_containing(cursor: NSPoint, frames: &[NSRect]) -> Option<NSRect> {
    frames.iter().copied().find(|f| {
        f.origin.x <= cursor.x
            && cursor.x < f.origin.x + f.size.width
            && f.origin.y <= cursor.y
            && cursor.y < f.origin.y + f.size.height
    })
}

/// 纯逻辑:计算浮窗 frame——光标右下偏移,右侧/下方空间不足时翻转到左侧/上方,
/// 仍不足则贴屏边缘 clamp,永不越出屏幕。
///
/// Pure: compute the picker frame -- offset to the cursor's bottom-right; flip to the
/// left/top when the right/bottom side lacks room; clamp to the screen edge otherwise.
/// Never placed outside the screen.
fn picker_frame_for(cursor: NSPoint, screen: NSRect, w: f64, h: f64) -> NSRect {
    let min_x = screen.origin.x;
    let max_x = screen.origin.x + screen.size.width;
    let min_y = screen.origin.y;
    let max_y = screen.origin.y + screen.size.height;

    // x:优先光标右侧;不足翻转到左侧;再不足贴左右缘。
    // x: prefer the cursor's right; flip to the left when tight; clamp to the edges.
    let mut x = cursor.x + PICKER_CURSOR_OFF;
    if x + w > max_x {
        x = cursor.x - w - PICKER_CURSOR_OFF;
    }
    if x < min_x {
        x = min_x + PICKER_EDGE_MARGIN;
    }
    if x + w > max_x {
        x = max_x - w - PICKER_EDGE_MARGIN;
    }

    // y:优先光标下方(面板顶边距光标 16pt);下方不足翻转到上方;再不足贴上下缘。
    // y: prefer below the cursor (the panel's top edge sits 16pt under it); flip above when
    // tight; clamp to the edges.
    let mut y = cursor.y - h - PICKER_CURSOR_OFF;
    if y < min_y {
        y = cursor.y + PICKER_CURSOR_OFF;
    }
    if y + h > max_y {
        y = max_y - h - PICKER_EDGE_MARGIN;
    }
    if y < min_y {
        y = min_y + PICKER_EDGE_MARGIN;
    }

    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

/// Option+V 呼出/关闭(由 bridge 在主线程调用)。
/// Toggle the picker on Option+V (called on the main thread by the bridge).
pub(crate) extern "C" fn on_clipboard_toggle(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    // 总开关关闭时忽略呼出(设置里关闭后 Option+V 不应再显示浮窗)。
    // Ignore the summon when the master switch is off (Option+V must not open the picker
    // after the user disabled the feature in Settings).
    if !CONFIG.read().unwrap().clipboard.enabled {
        log_debug!("[clip] toggle ignored: clipboard history disabled");
        return;
    }
    if PICKER_VISIBLE.load(Ordering::SeqCst) {
        hide_picker();
        return;
    }
    // 历史为空也显示浮窗(空状态提示,见 rebuild_rows 的空分支)。
    // Show the picker even with an empty history (the empty-state hint lives in
    // rebuild_rows' empty branch).
    *PICKER_SELECTION.lock().unwrap() = 0;
    show_picker();
}

/// 显示浮窗(构建一次,复用;窗口高度随可视行数动态调整)。
/// Show the picker (built once, reused; the window height follows the visible row count).
fn show_picker() {
    unsafe {
        ensure_picker_window();
        // 每次呼出重置搜索(干净起点)。
        // Reset the search on every summon (a clean slate).
        clear_search();
        let window = match *PICKER_WINDOW.lock().unwrap() {
            Some(w) => w.0,
            None => return,
        };
        let hist_len = CLIP_HISTORY.lock().unwrap().len();
        log_debug!("[clip] show picker: history={} entries", hist_len);

        // 窗口高度 = 上下留白 + 可视行的行距之和(行距由各条文本的换行行数决定)。
        // Window height = paddings + the sum of the visible rows' pitches (each pitch follows
        // the entry's wrapped line count).
        let pitches = {
            let hist = CLIP_HISTORY.lock().unwrap();
            compute_pitches(&hist)
        };
        let visible = hist_len.min(PICKER_MAX_ROWS);
        // 空历史时列表区高度 = 一条提示行的高度。
        // With an empty history the list area is one hint row tall.
        let list_h = if hist_len == 0 {
            row_button_height(1)
        } else {
            pitches.iter().take(visible).sum::<f64>()
        };
        // 最小高度兜底(内容再少也不低于 PICKER_MIN_HEIGHT,含空历史态)。
        // Floor at the minimum height (never smaller, empty state included).
        let h = (rows_top_offset() + list_h + PAD_Y).max(PICKER_MIN_HEIGHT);

        // 定位:跟随光标(光标右下偏移,空间不足翻转;光标所在屏幕,找不到回退主屏)。
        // Position: follow the cursor (bottom-right offset, flips when tight; the screen the
        // cursor is on, falling back to the main screen).
        let cursor: NSPoint = msg_send![class!(NSEvent), mouseLocation];
        let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
        let count: usize = msg_send![screens, count];
        let mut frames: Vec<NSRect> = Vec::with_capacity(count);
        for i in 0..count {
            // objectAtIndex: 的参数编码是 'q'(signed long),必须传 isize。
            // objectAtIndex: expects 'q' (signed long); pass isize.
            let s: *mut AnyObject = msg_send![screens, objectAtIndex: i as isize];
            frames.push(msg_send![s, frame]);
        }
        let screen_frame = screen_containing(cursor, &frames).unwrap_or_else(|| {
            let main: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
            msg_send![main, frame]
        });
        let frame = picker_frame_for(cursor, screen_frame, PICKER_W, h);
        log_debug!(
            "[clip] picker frame: ({:.0},{:.0}) {}x{} on screen ({:.0},{:.0})",
            frame.origin.x,
            frame.origin.y,
            frame.size.width,
            frame.size.height,
            screen_frame.origin.x,
            screen_frame.origin.y
        );
        let _: () = msg_send![window, setFrame: frame, display: true];

        rebuild_rows();
        // 每次呼出滚动到顶部(最新条目)。
        // Scroll to the top on every summon (the newest entry).
        if let Some(c) = *PICKER_CONTAINER.lock().unwrap() {
            let _: () = msg_send![c.0, scrollPoint: NSPoint::new(0.0, 0.0)];
        }
        // 首次呼出即更新滚动指示器(内容溢出时右侧立即显示,不必等滚动触发)。
        // Update the scroll indicator right on the first summon (shown immediately when the
        // content overflows, not only after scrolling).
        update_scroll_indicator();
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
    // 初始高度按最大可视行数(占位;show_picker 每次按实际 pitch 重设)。
    // Initial height sized for the max visible rows (placeholder; show_picker re-sizes per
    // summon using the real pitches).
    let h = rows_top_offset() + PICKER_MAX_ROWS as f64 * row_pitch(row_button_height(1)) + PAD_Y;
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

    // NSScrollView:滚轮滚动 + 自定义滚动指示器(去掉系统滚动条,视觉更贴合玻璃)。
    // NSScrollView: wheel scrolling + a custom scroll indicator (the system scroller is
    // replaced for a cleaner look on the glass).
    let scroll: *mut AnyObject = msg_send![class!(NSScrollView), alloc];
    let scroll: *mut AnyObject =
        msg_send![scroll, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h))];
    let _: () = msg_send![scroll, setAutoresizingMask: 18u64];
    let _: () = msg_send![scroll, setBorderType: 0u64]; // NSNoBorder
    let _: () = msg_send![scroll, setDrawsBackground: false];
    let _: () = msg_send![scroll, setHasVerticalScroller: false];
    let _: () = msg_send![scroll, setHasHorizontalScroller: false];
    let _: () = msg_send![content_parent, addSubview: scroll];
    release_obj(scroll);
    let _: () = msg_send![scroll, setDocumentView: container];
    release_obj(container);

    // 自定义滚动指示器:右侧 4pt 宽胶囊条,半透明白,滚动时显示、停止 1s 后淡出。
    // Custom scroll indicator: a 4pt rounded capsule on the right, semi-transparent white;
    // shown while scrolling and faded out 1s after scrolling stops.
    let indicator: *mut AnyObject = msg_send![class!(NSView), alloc];
    let indicator: *mut AnyObject = msg_send![
        indicator,
        initWithFrame: NSRect::new(
            NSPoint::new(w - SCROLL_INDICATOR_W - 3.0, 3.0),
            NSSize::new(SCROLL_INDICATOR_W, h - 6.0)
        )
    ];
    let _: () = msg_send![indicator, setWantsLayer: true];
    let ind_layer: *mut AnyObject = msg_send![indicator, layer];
    // 深色半透明:浅色玻璃背景下白色指示器完全融入背景不可见;半透明黑在明暗玻璃上都清晰。
    // Dark semi-transparent: a white indicator vanishes into light glass; a translucent black
    // knob stays legible on both light and dark glass.
    let ind_bg: *mut AnyObject = msg_send![class!(NSColor), colorWithWhite: 0.0f64, alpha: 0.35f64];
    // layer_set_background 走 raw objc_msgSend:objc2 无法编码 CGColor 参数。
    // layer_set_background goes through raw objc_msgSend: objc2 can't encode CGColor args.
    crate::ffi::layer_set_background(ind_layer, crate::ffi::ns_color_to_cg(ind_bg));
    let _: () = msg_send![ind_layer, setCornerRadius: SCROLL_INDICATOR_W / 2.0];
    let _: () = msg_send![indicator, setHidden: true];
    let _: () = msg_send![scroll, addSubview: indicator];
    release_obj(indicator);

    // 观察 clipView 的 bounds 变化(滚动发生)→ 更新指示器 + 重启淡出计时器。
    // Observe the clip view's bounds changes (scrolling) -> update the indicator + restart
    // the fade-out timer.
    let clip: *mut AnyObject = msg_send![scroll, contentView];
    let _: () = msg_send![clip, setPostsBoundsChangedNotifications: true];
    let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
    let bounds_name = make_nsstring("NSViewBoundsDidChangeNotification");
    let _: () = msg_send![
        center,
        addObserver: observer(),
        selector: sel!(scrollIndicatorBoundsChanged:),
        name: bounds_name,
        object: clip
    ];
    CFRelease(bounds_name as *const c_void);
    *SCROLL_VIEW.lock().unwrap() = Some(ObjPtr(scroll));
    *SCROLL_INDICATOR.lock().unwrap() = Some(ObjPtr(indicator));

    // 顶部搜索框(NSSearchField 子类):模糊过滤条目。不自动聚焦(用户点击才开始搜索)。
    // 子类只重写 cancelOperation:(Esc)——编辑期间的按键由字段编辑器处理,↓ 等命令经
    // delegate 的 control:textView:doCommandBySelector: 拦截(见 search_field_do_command)。
    // Top search field (an NSSearchField subclass): fuzzy entry filtering. Not auto-focused
    // (the user clicks it to start searching). The subclass only overrides cancelOperation:
    // (Esc) -- while editing, keys go to the field editor, and commands like ↓ are intercepted
    // via the delegate's control:textView:doCommandBySelector: (see search_field_do_command).
    let search_cls = {
        let name = CString::new("OhMyTabClipSearchField").unwrap();
        let superclass = class!(NSSearchField) as *const _ as *mut AnyObject;
        let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
        let types_v = CString::new("v@:").unwrap();
        class_addMethod(
            cls,
            sel!(cancelOperation:),
            search_field_cancel as *mut c_void,
            types_v.as_ptr(),
        );
        objc_registerClassPair(cls);
        cls
    };
    let search: *mut AnyObject = msg_send![search_cls, alloc];
    let search: *mut AnyObject = msg_send![
        search,
        initWithFrame: NSRect::new(
            NSPoint::new(PAD_X, PAD_Y),
            NSSize::new(SEARCH_BAR_W, CLEAR_BTN_H)
        )
    ];
    // 占位文字基线偏移 -2.5pt:NSSearchFieldCell 画的占位文字中心比图标/输入文字/
    // 清除按钮低约 2.6pt(像素测量),负基线偏移上移对齐(正偏移反而更低)。
    // Placeholder baseline offset -2.5pt: NSSearchFieldCell draws the placeholder ~2.6pt
    // lower than the search icon / typed text / clear button (pixel-measured); a NEGATIVE
    // baseline offset raises it into alignment (a positive one lowers it further).
    let ph_attrs: *mut AnyObject = msg_send![class!(NSMutableDictionary), alloc];
    let ph_attrs: *mut AnyObject = msg_send![ph_attrs, init];
    let off_key = make_nsstring("NSBaselineOffset");
    let off_val: *mut AnyObject = msg_send![class!(NSNumber), numberWithDouble: -2.5f64];
    let _: () = msg_send![ph_attrs, setObject: off_val, forKey: off_key];
    CFRelease(off_key as *const c_void);
    let ph_ns = make_nsstring(&t("clipboard.search_hint"));
    let ph_attr: *mut AnyObject = msg_send![class!(NSAttributedString), alloc];
    let ph_attr: *mut AnyObject = msg_send![ph_attr, initWithString: ph_ns, attributes: ph_attrs];
    CFRelease(ph_ns as *const c_void);
    release_obj(ph_attrs);
    let _: () = msg_send![search, setPlaceholderAttributedString: ph_attr];
    release_obj(ph_attr);
    // 磨砂化:去掉系统描边/bezel,改用与"清除全部"按钮同款的白色圆角 tile,顶部控件
    // 风格统一(系统 ✕ 清除按钮随之不渲染,清空由 Esc/cancelOperation: 覆盖)。
    // Frosted look: drop the system bezel and use the same white rounded tile as the
    // "clear all" button, unifying the top bar (the system ✕ clear button no longer renders;
    // clearing is covered by Esc/cancelOperation:).
    let _: () = msg_send![search, setBezeled: false];
    let _: () = msg_send![search, setDrawsBackground: false];
    let _: () = msg_send![search, setWantsLayer: true];
    let search_layer: *mut AnyObject = msg_send![search, layer];
    let s_bg: *mut AnyObject =
        msg_send![class!(NSColor), colorWithWhite: 1.0f64, alpha: ROW_TILE_ALPHA];
    crate::ffi::layer_set_background(search_layer, crate::ffi::ns_color_to_cg(s_bg));
    let _: () = msg_send![search_layer, setCornerRadius: SEL_TILE_R];
    // delegate = observer()(复用通知单例):↓ 命令拦截(字段编辑器转发 moveDown:)。
    // Delegate = observer() (reusing the notification singleton): intercepts ↓ (the field
    // editor forwards moveDown:).
    let _: () = msg_send![search, setDelegate: observer()];
    let _: () = msg_send![scroll, addSubview: search];
    release_obj(search);
    *SEARCH_FIELD.lock().unwrap() = Some(ObjPtr(search));
    // 文本变化(含系统清除按钮/NSSearchField 的 Esc 清空)→ 实时过滤。
    // Text changes (including the system clear button / NSSearchField's Esc clear) filter live.
    let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
    let text_name = make_nsstring("NSControlTextDidChangeNotification");
    let _: () = msg_send![
        center,
        addObserver: observer(),
        selector: sel!(searchFieldChanged:),
        name: text_name,
        object: search
    ];
    CFRelease(text_name as *const c_void);

    // 右上角"清除全部"按钮:清空剪贴板历史。
    // "Clear all" button at the top-right: empties the clipboard history.
    let clear_btn: *mut AnyObject = msg_send![class!(NSButton), alloc];
    let clear_btn: *mut AnyObject = msg_send![
        clear_btn,
        initWithFrame: NSRect::new(
            NSPoint::new(w - CLEAR_BTN_W - 3.0, PAD_Y),
            NSSize::new(CLEAR_BTN_W, CLEAR_BTN_H)
        )
    ];
    let _: () = msg_send![clear_btn, setBordered: false];
    let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 11.0f64];
    let _: () = msg_send![clear_btn, setFont: font];
    // 居中显示在磨砂白块内(与行卡片同款观感),而不是贴右边的裸文字。
    // Centered inside the frosted-white tile (same look as the row cards), not bare text
    // hugging the right edge.
    let _: () = msg_send![clear_btn, setAlignment: 1isize]; // center
                                                            // 磨砂白块背景:与行背景块同色同圆角,顶部工具栏与列表视觉统一。
                                                            // Frosted-white tile background: same color and corner radius as the row tiles, so the
                                                            // top bar matches the list.
    let _: () = msg_send![clear_btn, setWantsLayer: true];
    let clear_layer: *mut AnyObject = msg_send![clear_btn, layer];
    let white: *mut AnyObject =
        msg_send![class!(NSColor), colorWithWhite: 1.0f64, alpha: ROW_TILE_ALPHA];
    // layer_set_background 走 raw objc_msgSend(与行卡片同款)。
    // layer_set_background goes through raw objc_msgSend (same as the row tiles).
    crate::ffi::layer_set_background(clear_layer, crate::ffi::ns_color_to_cg(white));
    let _: () = msg_send![clear_layer, setCornerRadius: SEL_TILE_R];
    let title_ns = make_nsstring(&t("clipboard.clear_all"));
    let _: () = msg_send![clear_btn, setTitle: title_ns];
    CFRelease(title_ns as *const c_void);
    let _: () = msg_send![clear_btn, setTarget: observer()];
    let _: () = msg_send![clear_btn, setAction: sel!(clearClipboardHistory:)];
    let _: () = msg_send![scroll, addSubview: clear_btn];
    release_obj(clear_btn);
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
    // 背景块与按钮同生命周期:同样由父视图持有,removeFromSuperview 即释放,绝不二次
    // release(同按钮的 UAF 教训)。
    // Tiles share the buttons' lifecycle: parent-owned, released by removeFromSuperview,
    // never released again (same UAF lesson as the buttons).
    let mut tiles = ROW_TILES.lock().unwrap();
    for &t in tiles.iter() {
        let _: () = msg_send![t.0, removeFromSuperview];
    }
    tiles.clear();
    let mut pitches = ROW_PITCHES.lock().unwrap();
    pitches.clear();
    // 每行的按钮高/行距由文本换行行数决定。
    // Each row's button height / pitch derives from its wrapped line count.
    *pitches = compute_pitches(&hist);
    let total = hist.len();
    // 重建当前显示列表(按搜索词过滤)。
    // Rebuild the display list (filtered by the search query).
    *FILTERED.lock().unwrap() = filtered_indices(&hist, &SEARCH_QUERY.lock().unwrap());
    let filtered = FILTERED.lock().unwrap();

    // 空态:历史为空 → "暂无历史";有搜索词但无匹配 → "无匹配结果"。共用提示渲染。
    // Empty state: empty history -> "no history"; a query with no matches -> "no match".
    // Both share the same hint rendering.
    let empty_hint = if total == 0 {
        t("clipboard.empty")
    } else if filtered.is_empty() {
        t("clipboard.no_match")
    } else {
        String::new()
    };
    if !empty_hint.is_empty() {
        // 容器高度 = 可视区高度,而不是单行高:文档视图悬挂在 clip 底部(clip 不翻转),
        // 太矮会被裁掉/贴底,提示文字(按窗口居中定位)会落在容器外而不可见。
        // The container height = the visible area, NOT one row: the document view hangs off
        // the clip view's bottom (the clip isn't flipped); a short document gets clipped, and
        // the hint (positioned for the window center) would land outside the container.
        let doc_h = PICKER_MIN_HEIGHT - PAD_Y;
        let _: () = msg_send![container, setFrameSize: NSSize::new(PICKER_W, doc_h)];
        // 提示文本:在窗口内垂直居中(与容器高度无关,直接按窗口算)。
        // The hint: vertically centered within the window (independent of the container
        // height).
        let label_h = row_button_height(1);
        let label_y = (PICKER_MIN_HEIGHT - label_h) / 2.0;
        // 空态卡片:磨砂白块包住提示文字(细边框 + 圆角,与条目卡片同款),不再孤零零
        // 漂在玻璃上。
        // Empty-state card: a frosted tile wrapping the hint (hairline border + radius, same
        // style as the entry cards), no longer floating bare on the glass.
        let card_h = label_h + 16.0;
        let card_y = label_y - 8.0;
        let card: *mut AnyObject = msg_send![class!(NSView), alloc];
        let card: *mut AnyObject = msg_send![
            card,
            initWithFrame: NSRect::new(
                NSPoint::new(PAD_X + 60.0, card_y),
                NSSize::new(PICKER_W - (PAD_X + 60.0) * 2.0, card_h)
            )
        ];
        let _: () = msg_send![card, setWantsLayer: true];
        let card_layer: *mut AnyObject = msg_send![card, layer];
        let c_bg: *mut AnyObject =
            msg_send![class!(NSColor), colorWithWhite: 1.0f64, alpha: ROW_TILE_ALPHA];
        crate::ffi::layer_set_background(card_layer, crate::ffi::ns_color_to_cg(c_bg));
        let _: () = msg_send![card_layer, setCornerRadius: SEL_TILE_R];
        let c_border: *mut AnyObject =
            msg_send![class!(NSColor), colorWithWhite: 1.0f64, alpha: CARD_BORDER_ALPHA];
        crate::ffi::layer_set_border(card_layer, crate::ffi::ns_color_to_cg(c_border));
        let _: () = msg_send![card_layer, setBorderWidth: CARD_BORDER_W];
        let _: () = msg_send![container, addSubview: card];
        release_obj(card);
        tiles.push(ObjPtr(card));
        let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let label: *mut AnyObject = msg_send![
            label,
            initWithFrame: NSRect::new(
                NSPoint::new(PAD_X, label_y),
                NSSize::new(PICKER_W - PAD_X * 2.0, label_h)
            )
        ];
        let _: () = msg_send![label, setAlignment: 1isize]; // NSTextAlignmentCenter
        let hint_ns = make_nsstring(&empty_hint);
        let _: () = msg_send![label, setStringValue: hint_ns];
        CFRelease(hint_ns as *const c_void);
        let _: () = msg_send![label, setBezeled: false];
        let _: () = msg_send![label, setDrawsBackground: false];
        let _: () = msg_send![label, setEditable: false];
        // 提示文字用 labelColor(比 secondaryLabelColor 深一档):亮色玻璃上太浅会
        // 看不见(用户报告的"只有四个字")。
        // The hint uses labelColor (one notch darker than secondaryLabelColor): the lighter
        // shade vanished on the bright glass (the reported "only four characters").
        let text_color: *mut AnyObject = msg_send![class!(NSColor), labelColor];
        let _: () = msg_send![label, setTextColor: text_color];
        let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 13.0f64];
        let _: () = msg_send![label, setFont: font];
        let _: () = msg_send![container, addSubview: label];
        release_obj(label);
        rows.push(ObjPtr(label));
        REBUILDING.store(false, Ordering::SeqCst);
        return;
    }

    // 文档高度 = 全部显示条目(滚动区域),由 NSScrollView 滚动。
    // 下限 = 可视区高度:文档比窗口矮时悬挂在 clip 底部(clip 不翻转),行会贴底。
    // Document height covers ALL displayed entries (the scrollable area). Floored at the
    // visible height: a document shorter than the window hangs off the clip view's bottom
    // (the clip isn't flipped), pushing the rows against the bottom edge.
    let doc_h = (rows_top_offset() + pitches.iter().take(filtered.len()).sum::<f64>() + PAD_Y)
        .max(PICKER_MIN_HEIGHT - PAD_Y);
    let _: () = msg_send![container, setFrameSize: NSSize::new(PICKER_W, doc_h)];

    let sel_idx = *PICKER_SELECTION.lock().unwrap();
    // 读一次配置:标题栏里是否显示应用名(标题栏本身恒在,承载图钉/删除图标)。
    // Read the toggle once: whether the header bar shows the app name (the header itself is
    // always present, hosting the pin/delete icons).
    let show_source = show_source_app();
    for (i, &h_idx) in filtered.iter().enumerate() {
        let y = row_top(i, &pitches);
        // 日志只打索引/坐标,绝不打条目内容(隐私)。
        // Log the index/position only, NEVER the entry text (privacy).
        log_debug!("[clip] row {} created: y={}", i, y);
        let row_w = PICKER_W - PAD_X * 2.0;
        let entry = &hist[h_idx];
        let selected = i == sel_idx;

        // 卡片背景块:占满固定行距(行距 - 间距);选中行用强调色盖满整卡。
        // 细边框 + 圆角把卡片从亮玻璃上"立起来"。
        // The card tile: fills the fixed pitch (pitch - gap); the selected row gets the
        // accent color across the whole card. A hairline border + radius lift the cards off
        // the bright glass.
        let tile: *mut AnyObject = msg_send![class!(NSView), alloc];
        let tile: *mut AnyObject = msg_send![
            tile,
            initWithFrame: NSRect::new(NSPoint::new(PAD_X, y), NSSize::new(row_w, pitches[i] - ROW_GAP))
        ];
        let _: () = msg_send![tile, setWantsLayer: true];
        let tile_layer: *mut AnyObject = msg_send![tile, layer];
        let bg: *mut AnyObject = if selected {
            // 选中 = 系统强调色半透明(明暗玻璃都清晰);colorWithAlphaComponent: 是 double。
            // Selected = the system accent at partial alpha (legible on both glasses);
            // colorWithAlphaComponent: takes a double.
            let accent: *mut AnyObject = msg_send![class!(NSColor), controlAccentColor];
            msg_send![accent, colorWithAlphaComponent: 0.35f64]
        } else {
            msg_send![class!(NSColor), colorWithWhite: 1.0f64, alpha: ROW_TILE_ALPHA]
        };
        // layer_set_background 走 raw objc_msgSend:objc2 的 msg_send! 无法编码
        // CGColor 参数/返回(参数编码 '^{CGColor=}' 与 *mut c_void 的 '^v' 不匹配)。
        // layer_set_background goes through raw objc_msgSend: objc2's msg_send! can't encode
        // CGColor args/returns ('^{CGColor=}' vs '^v').
        crate::ffi::layer_set_background(tile_layer, crate::ffi::ns_color_to_cg(bg));
        let _: () = msg_send![tile_layer, setCornerRadius: SEL_TILE_R];
        // 细边框(同 raw FFI 路径)。
        // The hairline border (same raw-FFI path).
        let border: *mut AnyObject =
            msg_send![class!(NSColor), colorWithWhite: 1.0f64, alpha: CARD_BORDER_ALPHA];
        crate::ffi::layer_set_border(tile_layer, crate::ffi::ns_color_to_cg(border));
        let _: () = msg_send![tile_layer, setBorderWidth: CARD_BORDER_W];
        let _: () = msg_send![container, addSubview: tile];
        release_obj(tile);
        tiles.push(ObjPtr(tile));

        // 标题栏:浅色磨砂横条(选中行 = accent 加深)+ 应用名,只圆顶部两角(与卡片顶角
        // 贴合);整条可点击 = 粘贴,悬停选中该行(与正文按钮同款行为)。
        // Header bar: a light frosted strip (a deeper accent when selected) + the app name,
        // top corners rounded only (flush with the card's top); the whole strip is clickable
        // = paste, hover selects (same as the body).
        let header: *mut AnyObject = msg_send![row_button_class(), alloc];
        let header: *mut AnyObject = msg_send![
            header,
            initWithFrame: NSRect::new(NSPoint::new(PAD_X, y), NSSize::new(row_w, HEADER_H))
        ];
        let _: () = msg_send![header, setBordered: false];
        let _: () = msg_send![header, setAlignment: 0isize]; // left
        let _: () = msg_send![header, setWantsLayer: true];
        let h_layer: *mut AnyObject = msg_send![header, layer];
        let h_bg: *mut AnyObject = if selected {
            let accent: *mut AnyObject = msg_send![class!(NSColor), controlAccentColor];
            msg_send![accent, colorWithAlphaComponent: HEADER_SEL_ALPHA]
        } else {
            msg_send![class!(NSColor), colorWithWhite: 1.0f64, alpha: HEADER_BG_ALPHA]
        };
        crate::ffi::layer_set_background(h_layer, crate::ffi::ns_color_to_cg(h_bg));
        let _: () = msg_send![h_layer, setCornerRadius: SEL_TILE_R];
        // 只圆顶部两角(CALayerCornerMask: minXMinY = 1<<0, maxXMinY = 1<<1)。
        // Round only the top corners (CALayerCornerMask: minXMinY = 1<<0, maxXMinY = 1<<1).
        let _: () = msg_send![h_layer, setMaskedCorners: 3u64];
        // 来源应用小图标:开关开 + 有缓存键 + 小图存在 → 图标画进一个带左补白的透明
        // 画布再挂到按钮上(NSImageLeft):图标从 x=BODY_PAD_X 处显示、不拦截点击(仍是
        // 按钮的 image,整条标题栏可点=粘贴);标题在图标后自动右移,无需额外缩进。
        // The source app's small icon: toggle on + a cache key + the small PNG exists -> the
        // icon is drawn into a transparent canvas with left padding and set on the button
        // (NSImageLeft): the icon shows from x=BODY_PAD_X and never intercepts clicks (it is
        // still the button's image, so the whole header stays clickable = paste); the title
        // follows the image, no extra indent needed.
        let mut has_icon = false;
        if show_source && !entry.source_key.is_empty() {
            let icon_path = crate::window_collector::small_icon_path_for_key(&entry.source_key);
            if std::path::Path::new(&icon_path).exists() {
                let ns_path = make_nsstring(&icon_path);
                let icon_image: *mut AnyObject = msg_send![class!(NSImage), alloc];
                let icon_image: *mut AnyObject =
                    msg_send![icon_image, initWithContentsOfFile: ns_path];
                CFRelease(ns_path as *const c_void);
                if !icon_image.is_null() {
                    // 32px PNG 需 setSize 否则按 32pt 显示会溢出 22pt 标题栏。
                    // A 32px PNG must be setSize'd or it renders at 32pt and overflows the
                    // 22pt header.
                    let _: () = msg_send![icon_image, setSize: NSSize::new(16.0, 16.0)];
                    let pad_w = BODY_PAD_X + 16.0 + HEADER_ICON_GAP;
                    let canvas: *mut AnyObject = msg_send![class!(NSImage), alloc];
                    let canvas: *mut AnyObject =
                        msg_send![canvas, initWithSize: NSSize::new(pad_w, 16.0)];
                    let _: () = msg_send![canvas, lockFocus];
                    let dst = NSRect::new(NSPoint::new(BODY_PAD_X, 0.0), NSSize::new(16.0, 16.0));
                    let src = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
                    let op: usize = 1; // NSCompositingOperationCopy
                    let _: () = msg_send![
                        icon_image,
                        drawInRect: dst,
                        fromRect: src,
                        operation: op,
                        fraction: 1.0f64
                    ];
                    let _: () = msg_send![canvas, unlockFocus];
                    let _: () = msg_send![header, setImage: canvas];
                    let _: () = msg_send![header, setImagePosition: 2isize]; // NSImageLeft
                    release_obj(canvas);
                    release_obj(icon_image);
                    has_icon = true;
                }
            }
        }
        let h_title = header_title(entry, show_source);
        // 无图标时标题首行缩进与正文对齐;有图标时图标位已由补白画布让出。
        // Without an icon the title's first line aligns with the body text; with an icon the
        // padded canvas already reserves the slot.
        let title_indent = if has_icon { 0.0 } else { BODY_PAD_X };
        let h_attr = make_header_title(&h_title, title_indent);
        let _: () = msg_send![header, setAttributedTitle: h_attr];
        release_obj(h_attr);
        let _: () = msg_send![header, setTag: i as isize];
        let _: () = msg_send![header, setTarget: row_target()];
        let _: () = msg_send![header, setAction: sel!(handleClipboardRowClick:)];
        add_hover_tracking(header);
        let _: () = msg_send![container, addSubview: header];
        release_obj(header);
        rows.push(ObjPtr(header));

        // 正文按钮:标题栏下方,占满卡宽(图钉/删除已搬进标题栏,不再占宽度)。
        // Body button: below the header, full card width (pin/delete moved into the header,
        // no longer reserving width).
        let body: *mut AnyObject = msg_send![row_button_class(), alloc];
        let body_h = body_button_height(entry);
        let body: *mut AnyObject = msg_send![
            body,
            initWithFrame: NSRect::new(
                NSPoint::new(PAD_X + BODY_PAD_X, y + HEADER_H + BODY_GAP),
                NSSize::new(row_w - BODY_PAD_X * 2.0, body_h)
            )
        ];
        let _: () = msg_send![body, setBordered: false];
        let _: () = msg_send![body, setAlignment: 0isize]; // left
                                                           // 裁剪:行文本渲染不得溢出按钮边界(正文外还有标题栏/卡片边缘)。
                                                           // Clip: the row text must not render outside the button.
        let _: () = msg_send![body, setWantsLayer: true];
        let body_layer: *mut AnyObject = msg_send![body, layer];
        if !body_layer.is_null() {
            let _: () = msg_send![body_layer, setMasksToBounds: true];
        }
        // 超长文本换行显示,最多 3 行(第 3 行超出截断加省略号)。
        // Long text wraps, up to 3 lines (truncated with an ellipsis on line 3).
        let title = truncate_to_lines(&entry.text, LINE_MAX_UNITS, MAX_TEXT_LINES);
        let attr = make_row_attributed_title(&title, selected);
        let _: () = msg_send![body, setAttributedTitle: attr];
        release_obj(attr);
        let _: () = msg_send![body, setTag: i as isize];
        let _: () = msg_send![body, setTarget: row_target()];
        let _: () = msg_send![body, setAction: sel!(handleClipboardRowClick:)];
        add_hover_tracking(body);
        let _: () = msg_send![container, addSubview: body];
        release_obj(body);
        rows.push(ObjPtr(body));

        // 行内图钉按钮:标题栏右侧,垂直居中。独立于行按钮(点击不会触发粘贴)。
        // Per-row pin button: the header's right side, vertically centered. Separate from the
        // row buttons (clicking the pin does not paste).
        let pin_btn: *mut AnyObject = msg_send![class!(NSButton), alloc];
        let pin_btn: *mut AnyObject = msg_send![
            pin_btn,
            initWithFrame: NSRect::new(
                NSPoint::new(
                    PICKER_W - PAD_X - DEL_BTN_W - PIN_BTN_W - 2.0,
                    y + (HEADER_H - PIN_BTN_H) / 2.0
                ),
                NSSize::new(PIN_BTN_W, PIN_BTN_H)
            )
        ];
        let _: () = msg_send![pin_btn, setBordered: false];
        // NSImageOnly = 1。曾误用 2(NSImageLeft = 图像与标题并排),按钮默认标题
        // 会显示在图标右侧(用户看到的"字母")。
        // NSImageOnly = 1. Was mistakenly 2 (NSImageLeft = image beside the title), which
        // showed the button's default title right of the icon (the stray letter).
        let _: () = msg_send![pin_btn, setImagePosition: 1isize];
        // 显式清空标题(默认值不可靠)。
        // Explicitly clear the title (the default is unreliable).
        let empty_ns = make_nsstring("");
        let _: () = msg_send![pin_btn, setTitle: empty_ns];
        CFRelease(empty_ns as *const c_void);
        // SF Symbol:置顶用 pin.fill,未置顶用 pin。
        // SF Symbol: pin.fill when pinned, pin otherwise.
        let symbol = if entry.pinned { "pin.fill" } else { "pin" };
        let sym_ns = make_nsstring(symbol);
        let desc = make_nsstring("Pin");
        let img: *mut AnyObject = msg_send![
            class!(NSImage),
            imageWithSystemSymbolName: sym_ns,
            accessibilityDescription: desc
        ];
        CFRelease(sym_ns as *const c_void);
        CFRelease(desc as *const c_void);
        if !img.is_null() {
            let _: () = msg_send![pin_btn, setImage: img];
        }
        let _: () = msg_send![pin_btn, setTag: i as isize];
        let _: () = msg_send![pin_btn, setTarget: row_target()];
        let _: () = msg_send![pin_btn, setAction: sel!(togglePin:)];
        // 图标加深(比系统默认 labelColor 深一档)。
        // Darken the icon (one notch darker than the default labelColor).
        let tint: *mut AnyObject =
            msg_send![class!(NSColor), colorWithWhite: ROW_ICON_TINT, alpha: 1.0f64];
        let _: () = msg_send![pin_btn, setContentTintColor: tint];
        let _: () = msg_send![container, addSubview: pin_btn];
        release_obj(pin_btn);
        rows.push(ObjPtr(pin_btn));

        // 行内删除按钮(Backspace 图标):单条删除。独立于行按钮(点击不会触发粘贴)。
        // Per-row delete button (the Backspace icon): removes the entry. Separate from the
        // row buttons (clicking it does not paste).
        let del_btn: *mut AnyObject = msg_send![class!(NSButton), alloc];
        let del_btn: *mut AnyObject = msg_send![
            del_btn,
            initWithFrame: NSRect::new(
                NSPoint::new(
                    PICKER_W - PAD_X - DEL_BTN_W,
                    y + (HEADER_H - DEL_BTN_H) / 2.0
                ),
                NSSize::new(DEL_BTN_W, DEL_BTN_H)
            )
        ];
        let _: () = msg_send![del_btn, setBordered: false];
        // NSImageOnly = 1;显式清空标题,只显示图标。
        // NSImageOnly = 1; title cleared explicitly, icon only.
        let _: () = msg_send![del_btn, setImagePosition: 1isize];
        let empty_ns = make_nsstring("");
        let _: () = msg_send![del_btn, setTitle: empty_ns];
        CFRelease(empty_ns as *const c_void);
        // SF Symbol "delete.left" = Backspace 图标。
        // SF Symbol "delete.left" = the Backspace icon.
        let sym_ns = make_nsstring("delete.left");
        let desc = make_nsstring("Delete");
        let img: *mut AnyObject = msg_send![
            class!(NSImage),
            imageWithSystemSymbolName: sym_ns,
            accessibilityDescription: desc
        ];
        CFRelease(sym_ns as *const c_void);
        CFRelease(desc as *const c_void);
        if !img.is_null() {
            let _: () = msg_send![del_btn, setImage: img];
        }
        let _: () = msg_send![del_btn, setTag: i as isize];
        let _: () = msg_send![del_btn, setTarget: row_target()];
        let _: () = msg_send![del_btn, setAction: sel!(deleteEntry:)];
        // 图标加深(与图钉一致)。
        // Darken the icon (same as the pin).
        let tint: *mut AnyObject =
            msg_send![class!(NSColor), colorWithWhite: ROW_ICON_TINT, alpha: 1.0f64];
        let _: () = msg_send![del_btn, setContentTintColor: tint];
        let _: () = msg_send![container, addSubview: del_btn];
        release_obj(del_btn);
        rows.push(ObjPtr(del_btn));
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

/// 悬停行按钮:选中该行并刷新高亮。搜索框编辑中(光标在搜索框)时忽略——用户要求
/// 焦点在搜索框时列表不得有任何选中行,而 rebuild 后光标仍可能停在行上触发 enter。
/// Hovering a row button: select it and refresh the highlight. Ignored while the search
/// field is editing (focus in the search box): the requirement is no selected row while the
/// search box has focus, and after a rebuild the cursor may still sit over a row and fire
/// mouseEntered.
extern "C" fn row_button_mouse_entered(_self: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    // 重建期间派发的 enter 忽略(防无限递归,见 REBUILDING 注释)。
    // Ignore enters dispatched during a rebuild (prevents infinite recursion; see REBUILDING).
    if REBUILDING.load(Ordering::SeqCst) {
        return;
    }
    // 搜索框正在编辑(currentEditor 非空)→ 悬停不选中。
    // The search field is editing (currentEditor non-nil) -> hovering must not select.
    if let Some(f) = *SEARCH_FIELD.lock().unwrap() {
        let editor: *mut AnyObject = unsafe { msg_send![f.0, currentEditor] };
        if !editor.is_null() {
            return;
        }
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

/// 图钉按钮回调(tag = 显示行索引)→ 映射历史索引置顶/取消置顶并刷新列表。
/// Pin-button callback (tag = display row index) -> mapped history index, pin/unpin, refresh.
extern "C" fn toggle_pin(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    let idx: isize = unsafe { msg_send![sender as *mut AnyObject, tag] };
    if idx < 0 {
        return;
    }
    let Some(h_idx) = mapped_index(idx as usize) else {
        return;
    };
    let mut hist = CLIP_HISTORY.lock().unwrap();
    if hist[h_idx].pinned {
        unpin_entry(&mut hist, h_idx);
    } else {
        pin_entry(&mut hist, h_idx);
    }
    drop(hist);
    unsafe { rebuild_rows() };
}

/// 删除按钮回调(tag = 显示行索引)→ 映射历史索引删除并刷新列表。
/// Delete-button callback (tag = display row index) -> mapped history index, remove, refresh.
extern "C" fn delete_entry_cb(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    let idx: isize = unsafe { msg_send![sender as *mut AnyObject, tag] };
    if idx < 0 {
        return;
    }
    let Some(h_idx) = mapped_index(idx as usize) else {
        return;
    };
    let mut hist = CLIP_HISTORY.lock().unwrap();
    delete_entry(&mut hist, h_idx);
    // 删除后选中保持同位置(指向原下一条);越界则回退到末条。
    // Selection stays at the same index (pointing at the next entry); clamps to the tail.
    let mut sel = PICKER_SELECTION.lock().unwrap();
    if *sel >= hist.len() {
        *sel = hist.len().saturating_sub(1);
    }
    drop(sel);
    drop(hist);
    unsafe { rebuild_rows() };
}

/// 粘贴指定显示索引的条目(经 FILTERED 映射):关闭浮窗 + 写回剪贴板 + 模拟 Cmd+V。
/// Paste the entry at display `idx` (mapped through FILTERED): close the picker + write back
/// to the pasteboard + synthesize Cmd+V.
fn paste_at(idx: usize) {
    let Some(h_idx) = mapped_index(idx) else {
        log_debug!("[clip] paste index {} out of range", idx);
        hide_picker();
        return;
    };
    let text = {
        let hist = CLIP_HISTORY.lock().unwrap();
        hist.get(h_idx).map(|e| e.text.clone())
    };
    match text {
        Some(t) => {
            // 日志只打索引,绝不打粘贴内容(隐私)。
            // Log the index only, NEVER the pasted text (privacy).
            log_debug!("[clip] paste_at idx={}", idx);
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
    // sel 可能为 NO_SELECTION(usize::MAX,焦点在搜索框时的哨兵)——冒烟直接驱动
    // handler 会走到这里,必须防溢出并视为"无选中"处理。
    // sel may be NO_SELECTION (usize::MAX, the sentinel while the search field has focus) --
    // the smoke drives the handler directly so this must not overflow and treats it as
    // "no selection".
    match keycode {
        126 => Some(if sel == 0 || sel >= hist_len {
            hist_len - 1
        } else {
            sel - 1
        }),
        125 => Some(if sel >= hist_len - 1 { 0 } else { sel + 1 }),
        _ => None,
    }
}

/// 键盘导航:↑/↓ 选择,Enter 粘贴,Esc 关闭。
/// Keyboard navigation: up/down to select, Enter to paste, Esc to close.
extern "C" fn container_key_down(_self: *mut c_void, _cmd: Sel, event: *mut c_void) {
    unsafe {
        let keycode: u16 = msg_send![event as *mut AnyObject, keyCode];
        // 可选中范围是当前显示列表(搜索过滤后;超出可视部分靠滚动查看)。
        // The selectable range is the current display list (post-filter; scrolling reveals
        // the rest).
        let display_len = FILTERED.lock().unwrap().len();
        let mut sel = PICKER_SELECTION.lock().unwrap();
        match keycode {
            126 | 125 => {
                // ↑(126):已在列表第一条(或无选中)时跳回搜索框;进入前清除选中,
                // 高光消失(delegate 的 controlTextDidBeginEditing: 也会清,双保险)。
                // Up (126): at the first list entry (or no selection), jump focus back to the
                // search field; clear the selection BEFORE entering so the highlight goes away
                // (the controlTextDidBeginEditing: delegate also clears - belt and braces).
                if keycode == 126 && (*sel == 0 || *sel == NO_SELECTION) {
                    if let Some(f) = *SEARCH_FIELD.lock().unwrap() {
                        drop(sel);
                        *PICKER_SELECTION.lock().unwrap() = NO_SELECTION;
                        rebuild_rows();
                        let window = match *PICKER_WINDOW.lock().unwrap() {
                            Some(w) => w.0,
                            None => return,
                        };
                        // makeFirstResponder: 返回 BOOL('B')。
                        // makeFirstResponder: returns BOOL ('B').
                        let _: bool = msg_send![window, makeFirstResponder: f.0];
                        return;
                    }
                }
                if let Some(next) = nav_arrow(keycode, *sel, display_len) {
                    *sel = next;
                }
                let idx = *sel;
                drop(sel);
                refresh_selection(idx);
                // 滚动到选中行可见 / scroll the selection into view.
                if let Some(c) = *PICKER_CONTAINER.lock().unwrap() {
                    let pitches = ROW_PITCHES.lock().unwrap();
                    let y = row_top(idx, &pitches);
                    let h = pitches.get(idx).copied().unwrap_or(ROW_GAP);
                    drop(pitches);
                    // scrollRectToVisible: 返回 BOOL('B')。
                    // scrollRectToVisible: returns BOOL ('B').
                    let _: bool = msg_send![
                        c.0,
                        scrollRectToVisible: NSRect::new(
                            NSPoint::new(0.0, y),
                            NSSize::new(1.0, h)
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
            51 => {
                // Backspace(删除键):删除选中条目并刷新。
                // Backspace (delete): remove the selected entry and refresh.
                let idx = *sel;
                drop(sel);
                let Some(h_idx) = mapped_index(idx) else {
                    return;
                };
                let mut hist = CLIP_HISTORY.lock().unwrap();
                delete_entry(&mut hist, h_idx);
                // 删除后选中保持同位置(指向原下一条);越界则回退到显示列表末条。
                // Selection stays at the same position (the next entry); clamps to the tail
                // of the display list.
                let display_len = FILTERED.lock().unwrap().len();
                let mut sel = PICKER_SELECTION.lock().unwrap();
                if *sel >= display_len {
                    *sel = display_len.saturating_sub(1);
                }
                drop(sel);
                drop(hist);
                rebuild_rows();
            }
            53 => {
                // Esc:清空搜索词则恢复全列表,再按才关闭——搜索框聚焦时的第一级由
                // NSSearchField 子类的 cancelOperation: 处理;这里处理列表聚焦时。
                // Esc: a query gets cleared first (restoring the full list), a second press
                // closes. The search-field-focused first level is handled by the
                // NSSearchField subclass's cancelOperation:; this handles list focus.
                let mut q = SEARCH_QUERY.lock().unwrap();
                if !q.is_empty() {
                    q.clear();
                    drop(q);
                    // rebuild_rows 会重锁 PICKER_SELECTION,必须先释放 sel。
                    // rebuild_rows re-locks PICKER_SELECTION; sel must be dropped first.
                    drop(sel);
                    rebuild_rows();
                } else {
                    drop(q);
                    drop(sel);
                    hide_picker();
                }
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
    // 段落样式:单词换行——长文本在行按钮内换行显示(最多 3 行由 truncate_to_lines 保证)。
    // Paragraph style: word wrapping -- long text wraps inside the row button (the 3-line cap
    // is guaranteed by truncate_to_lines).
    let pstyle: *mut AnyObject = msg_send![class!(NSMutableParagraphStyle), alloc];
    let pstyle: *mut AnyObject = msg_send![pstyle, init];
    let _: () = msg_send![pstyle, setLineBreakMode: 0isize]; // NSLineBreakByWordWrapping
    let attrs: *mut AnyObject = msg_send![class!(NSMutableDictionary), alloc];
    let attrs: *mut AnyObject = msg_send![attrs, init];
    let font_key = make_nsstring("NSFont");
    let color_key = make_nsstring("NSColor");
    let pstyle_key = make_nsstring("NSParagraphStyle");
    let _: () = msg_send![attrs, setObject: font, forKey: font_key];
    let _: () = msg_send![attrs, setObject: color, forKey: color_key];
    let _: () = msg_send![attrs, setObject: pstyle, forKey: pstyle_key];
    CFRelease(font_key as *const c_void);
    CFRelease(color_key as *const c_void);
    CFRelease(pstyle_key as *const c_void);
    release_obj(pstyle);
    let ns_title = make_nsstring(title);
    let attr: *mut AnyObject = msg_send![class!(NSAttributedString), alloc];
    let attr: *mut AnyObject = msg_send![attr, initWithString: ns_title, attributes: attrs];
    CFRelease(ns_title as *const c_void);
    release_obj(attrs);
    attr
}

/// 标题栏文字(attributed):10pt,次要色,在浅色磨砂横条上可读;空串时整行留空。
/// `indent`: 首行左缩进(pt)——有图标时让出图标位,无图标时与正文文字对齐。
/// Header-bar text (attributed): 10pt, secondary color, legible on the light frosted strip;
/// an empty string renders nothing. `indent`: the first-line left indent (pt) -- reserves the
/// icon slot when present, aligns with the body text otherwise.
unsafe fn make_header_title(title: &str, indent: f64) -> *mut AnyObject {
    if title.is_empty() {
        let empty = make_nsstring("");
        let attr: *mut AnyObject = msg_send![class!(NSAttributedString), alloc];
        let attr: *mut AnyObject = msg_send![attr, initWithString: empty];
        CFRelease(empty as *const c_void);
        return attr;
    }
    let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 10.0f64];
    let color: *mut AnyObject = msg_send![class!(NSColor), secondaryLabelColor];
    let attrs: *mut AnyObject = msg_send![class!(NSMutableDictionary), alloc];
    let attrs: *mut AnyObject = msg_send![attrs, init];
    let font_key = make_nsstring("NSFont");
    let color_key = make_nsstring("NSColor");
    let _: () = msg_send![attrs, setObject: font, forKey: font_key];
    let _: () = msg_send![attrs, setObject: color, forKey: color_key];
    CFRelease(font_key as *const c_void);
    CFRelease(color_key as *const c_void);
    if indent > 0.0 {
        // 首行缩进:与正文文字缩进对齐,有图标时让出图标位。
        // First-line indent: aligned with the body text; reserves the icon slot when shown.
        let pstyle: *mut AnyObject = msg_send![class!(NSMutableParagraphStyle), alloc];
        let pstyle: *mut AnyObject = msg_send![pstyle, init];
        let _: () = msg_send![pstyle, setHeadIndent: indent];
        let _: () = msg_send![pstyle, setFirstLineHeadIndent: indent];
        let pstyle_key = make_nsstring("NSParagraphStyle");
        let _: () = msg_send![attrs, setObject: pstyle, forKey: pstyle_key];
        CFRelease(pstyle_key as *const c_void);
        release_obj(pstyle);
    }
    let ns_title = make_nsstring(title);
    let attr: *mut AnyObject = msg_send![class!(NSAttributedString), alloc];
    let attr: *mut AnyObject = msg_send![attr, initWithString: ns_title, attributes: attrs];
    CFRelease(ns_title as *const c_void);
    release_obj(attrs);
    attr
}

/// 给行内按钮(标题栏/正文)挂悬停跟踪区:悬停 = 选中该行(与窗口切换浮窗一致)。
/// Attach a hover tracking area to a row button (header/body): hovering selects the row
/// (same as the switcher overlay).
unsafe fn add_hover_tracking(view: *mut AnyObject) {
    let opts: u64 = 0x02 | 0x40 | 0x100; // MouseEnteredAndExited | ActiveInKeyWindow | InVisibleRect
    let ta: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
    let ta: *mut AnyObject = msg_send![
        ta,
        initWithRect: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0)),
        options: opts,
        owner: view,
        userInfo: std::ptr::null::<AnyObject>()
    ];
    let _: () = msg_send![view, addTrackingArea: ta];
    release_obj(ta);
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
            class_addMethod(
                cls,
                sel!(togglePin:),
                toggle_pin as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(deleteEntry:),
                delete_entry_cb as *mut c_void,
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
            record_text(
                &mut hist,
                &format!("smoke entry {i:02}"),
                "Ghostty",
                "com.mitchellh.ghostty",
                50,
            );
        }
        record_text(
            &mut hist,
            "apple pie recipe",
            "Safari",
            "com.apple.Safari",
            50,
        );
        record_text(&mut hist, "banana bread", "Chrome", "com.google.Chrome", 50);
        // 无来源条目:标题栏应显示"未知来源",无图标。
        // A source-less entry: the header shows "unknown source", no icon.
        record_text(&mut hist, "legacy entry without a source", "", "", 50);
    }
    show_picker();
    hide_picker();
    // 第二次显示:rebuild_rows 会先移除旧行(曾经的 UAF 路径)。
    // Second show: rebuild_rows removes the old rows first (the former UAF path).
    show_picker();
    // 搜索冒烟:设置搜索词 → 重建(过滤显示)→ 方向键在过滤列表内导航 → 清空恢复。
    // Search smoke: set a query -> rebuild (filtered display) -> arrow navigation within the
    // filtered list -> clear restores everything.
    unsafe {
        *SEARCH_QUERY.lock().unwrap() = "apple".to_string();
        rebuild_rows();
        let c_opt = *PICKER_CONTAINER.lock().unwrap();
        if let Some(c) = c_opt {
            let ev = make_key_event(125); // ↓ / down arrow
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
        }
        // 搜索框 ↓:经 delegate 命令拦截(moveDown:)焦点切到列表并选中第一条(过滤结果
        // 保留)。直接调 handler:真实链路里字段编辑器把 ↓ 翻译成 moveDown: 后调它。
        // Search-field down-arrow: the delegate command interception (moveDown:) moves focus
        // into the list and selects the first filtered entry. The handler is called directly
        // -- in the real chain the field editor translates ↓ to moveDown: and invokes it.
        if let Some(_f) = *SEARCH_FIELD.lock().unwrap() {
            search_field_do_command(
                std::ptr::null_mut(),
                sel!(control:textView:doCommandBySelector:),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                sel!(moveDown:),
            );
        }
        // 列表顶部 ↑:焦点跳回搜索框(搜索词保留);搜索框开始编辑的 delegate 回调
        // (search_field_began_editing)会清除选中 → 高光消失。
        // Up at the list top: focus jumps back to the search field (query kept); the
        // search field's begin-editing delegate callback (search_field_began_editing) clears
        // the selection -> the highlight disappears.
        let c_opt = *PICKER_CONTAINER.lock().unwrap();
        if let Some(c) = c_opt {
            let ev = make_key_event(126); // ↑ / up arrow
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
        }
        // 断言:焦点进搜索框后选中被清除(无行高亮)。
        // Assert: after focus enters the search field the selection is cleared (no row
        // highlight).
        assert_eq!(
            *PICKER_SELECTION.lock().unwrap(),
            NO_SELECTION,
            "selection must clear when focus moves into the search field"
        );
        // Esc 第一级:清空搜索词并恢复全列表(列表聚焦路径)。
        // Esc level one: clear the query and restore the full list (list-focus path).
        let ev_esc = make_key_event(53);
        let c_opt = *PICKER_CONTAINER.lock().unwrap();
        if let Some(c) = c_opt {
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev_esc as *mut c_void);
        }
        clear_search();
    }
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
            // 多次滚动:每次滚动都触发 clipView bounds 变化通知 → 滚动指示器更新路径
            // (setOpacity: 等 msg_send 曾因 float/double 编码错误 panic,冒烟必须覆盖)。
            // Scroll several times: each scroll fires the clip-view bounds-change notification
            // -> the indicator-update path (setOpacity: once panicked on a float/double
            // encoding mismatch; the smoke run must cover it).
            for step in 1..=5u8 {
                let _: () = msg_send![c.0, scrollPoint: NSPoint::new(0.0, step as f64 * 30.0)];
            }
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
    use super::ClipEntry;

    /// 测试用的 3 参便捷包装(来源与图标键留空,既有用例不受签名变化影响)。
    /// A 3-arg convenience wrapper for tests (empty source and icon key; existing cases are
    /// unaffected by the signature change).
    fn record_text(h: &mut Vec<ClipEntry>, text: &str, max: usize) -> bool {
        super::record_text(h, text, "", "", max)
    }

    fn entry(text: &str) -> ClipEntry {
        ClipEntry {
            text: text.to_string(),
            pinned: false,
            source_app: String::new(),
            source_key: String::new(),
        }
    }

    fn entry_with_source(text: &str, source: &str) -> ClipEntry {
        ClipEntry {
            text: text.to_string(),
            pinned: false,
            source_app: source.to_string(),
            source_key: String::new(),
        }
    }

    fn entry_with_identity(text: &str, source: &str, key: &str) -> ClipEntry {
        ClipEntry {
            text: text.to_string(),
            pinned: false,
            source_app: source.to_string(),
            source_key: key.to_string(),
        }
    }

    fn texts(h: &[ClipEntry]) -> Vec<String> {
        h.iter().map(|e| e.text.clone()).collect()
    }

    #[test]
    fn empty_text_is_ignored() {
        let mut h = vec![entry("a")];
        assert!(!record_text(&mut h, "", 50));
        assert_eq!(h.len(), 1);
    }

    #[test]
    fn duplicate_is_moved_to_front_not_duplicated() {
        // 全表查重:再次复制历史中已有的文本 → 旧条目提到最前,不新增重复。
        // Full-list dedup: re-copying an existing text moves the old entry to the front
        // instead of adding a duplicate.
        let mut h = vec![entry("a"), entry("b")];
        // 复制 "a"(已在列表末尾)→ 提到最前。
        // Copy "a" (already at the tail) -> moved to the front.
        assert!(record_text(&mut h, "a", 50));
        assert_eq!(texts(&h), vec!["a", "b"]);
        // 连续复制同一内容:原地不动,列表不重复。
        // Re-copying the same text again: no move, no duplicate.
        assert!(record_text(&mut h, "a", 50));
        assert_eq!(texts(&h), vec!["a", "b"]);
        assert_eq!(h.len(), 2);
    }

    #[test]
    fn dedup_updates_the_source_to_the_latest_copy() {
        // 同一文本从不同应用复制:去重移前时来源更新为最新复制的应用。
        // Re-copying the same text from another app: the dedup move updates the source to the
        // latest copy's app.
        let mut h = Vec::new();
        super::record_text(&mut h, "token", "Safari", "com.apple.Safari", 50);
        assert_eq!(h[0].source_app, "Safari");
        assert_eq!(h[0].source_key, "com.apple.Safari");
        super::record_text(&mut h, "token", "Chrome", "com.google.Chrome", 50);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].source_app, "Chrome");
        // 去重移前时图标键一并更新为最新来源。
        // The dedup move also updates the icon key to the latest source.
        assert_eq!(h[0].source_key, "com.google.Chrome");
    }

    #[test]
    fn record_keeps_the_source_and_pin_moves_preserve_it() {
        use super::pin_entry;
        // 来源随条目走:置顶/移动不影响来源。
        // The source travels with the entry: pin/move operations preserve it.
        let mut h = Vec::new();
        super::record_text(&mut h, "A", "Safari", "com.apple.Safari", 50);
        super::record_text(&mut h, "B", "Chrome", "com.google.Chrome", 50);
        pin_entry(&mut h, 0);
        assert_eq!(h[0].text, "B");
        assert_eq!(h[0].source_app, "Chrome");
        assert_eq!(h[0].source_key, "com.google.Chrome");
        assert_eq!(h[1].source_app, "Safari");
    }

    #[test]
    fn duplicate_pinned_entry_stays_pinned_and_moves_to_pin_top() {
        use super::pin_entry;
        // 置顶 B 后,再复制 B:保持置顶并移到置顶区顶部。
        // After pinning B, re-copying B keeps it pinned and moves it to the top of the
        // pinned block.
        let mut h = Vec::new();
        record_text(&mut h, "A", 50);
        record_text(&mut h, "B", 50);
        pin_entry(&mut h, 0); // 置顶 B / pin B
        record_text(&mut h, "C", 50);
        record_text(&mut h, "D", 50);
        // 现在:B(置顶) D C A(新条目插到置顶区之后,最新在前)。
        // Now: B (pinned) D C A (new entries land after the pinned block, newest first).
        assert_eq!(texts(&h), vec!["B", "D", "C", "A"]);
        // 复制 B → 保持置顶且在置顶区顶部(顺序不变)。
        // Copying B keeps it pinned at the top of the pinned block (order unchanged).
        record_text(&mut h, "B", 50);
        assert!(h[0].pinned);
        assert_eq!(texts(&h), vec!["B", "D", "C", "A"]);
        // 复制 A → 提到非置顶区顶部(D 比 C 新,仍在 C 前)。
        // Copying A moves it to the top of the unpinned block (D is newer than C, so it
        // stays before C).
        record_text(&mut h, "A", 50);
        assert_eq!(texts(&h), vec!["B", "A", "D", "C"]);
        assert!(!h[1].pinned);
    }

    #[test]
    fn newest_goes_first() {
        let mut h = Vec::new();
        record_text(&mut h, "first", 50);
        record_text(&mut h, "second", 50);
        assert_eq!(texts(&h), vec!["second", "first"]);
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
        assert_eq!(h[0].text, "item4");
        assert_eq!(h[2].text, "item2");
    }

    #[test]
    fn zero_max_records_nothing() {
        let mut h = Vec::new();
        assert!(!record_text(&mut h, "x", 0));
        assert!(h.is_empty());
    }

    #[test]
    fn pinned_entries_stay_on_top_of_new_records() {
        use super::{pin_entry, unpin_entry};
        // 置顶 B 后,新复制的 C 插到置顶区之后(B 仍在顶部)。
        // After pinning B, a new copy of C lands after the pinned block (B stays on top).
        let mut h = Vec::new();
        record_text(&mut h, "A", 50);
        record_text(&mut h, "B", 50);
        pin_entry(&mut h, 0); // 置顶 B / pin B
        assert!(h[0].pinned);
        record_text(&mut h, "C", 50);
        assert_eq!(texts(&h), vec!["B", "C", "A"]);
        // 取消置顶 B:移到非置顶区顶部。
        // Unpinning B moves it to the top of the unpinned block.
        unpin_entry(&mut h, 0);
        assert!(!h[0].pinned);
        assert_eq!(texts(&h), vec!["B", "C", "A"]);
        // 新复制 D 排到 B 之前。
        // A new copy of D lands before B.
        record_text(&mut h, "D", 50);
        assert_eq!(texts(&h), vec!["D", "B", "C", "A"]);
    }

    #[test]
    fn pin_moves_entry_to_top_and_is_idempotent() {
        use super::pin_entry;
        let mut h = Vec::new();
        record_text(&mut h, "A", 50);
        record_text(&mut h, "B", 50);
        record_text(&mut h, "C", 50);
        pin_entry(&mut h, 2); // 置顶 A / pin A
        assert_eq!(texts(&h), vec!["A", "C", "B"]);
        assert!(h[0].pinned);
        // 再次置顶同一位置:无变化。
        // Pinning the same entry again: no change.
        pin_entry(&mut h, 0);
        assert_eq!(h.len(), 3);
        // 越界置顶:忽略。
        // Out-of-range pin: ignored.
        pin_entry(&mut h, 99);
        assert_eq!(h.len(), 3);
    }

    #[test]
    fn delete_entry_removes_by_index_and_ignores_out_of_range() {
        use super::delete_entry;
        let mut h = Vec::new();
        record_text(&mut h, "A", 50);
        record_text(&mut h, "B", 50);
        record_text(&mut h, "C", 50);
        // 删除中间条目 / remove the middle entry.
        delete_entry(&mut h, 1);
        assert_eq!(texts(&h), vec!["C", "A"]);
        // 越界删除:无变化 / out-of-range delete: no change.
        delete_entry(&mut h, 99);
        assert_eq!(h.len(), 2);
        // 删除后列表顺序保持 / order is preserved after deletion.
        delete_entry(&mut h, 0);
        assert_eq!(texts(&h), vec!["A"]);
    }

    #[test]
    fn delete_pinned_entry_keeps_others_pinned() {
        use super::{delete_entry, pin_entry};
        let mut h = Vec::new();
        record_text(&mut h, "A", 50);
        record_text(&mut h, "B", 50);
        record_text(&mut h, "C", 50);
        pin_entry(&mut h, 0); // 置顶 C / pin C
        pin_entry(&mut h, 1); // 置顶 B / pin B
        assert_eq!(texts(&h), vec!["B", "C", "A"]);
        // 删除置顶的 C:其余置顶保留,列表无空洞。
        // Delete the pinned C: the other pin stays, no hole in the list.
        delete_entry(&mut h, 1);
        assert_eq!(texts(&h), vec!["B", "A"]);
        assert!(h[0].pinned);
        assert!(!h[1].pinned);
    }

    #[test]
    fn filtered_indices_matches_case_insensitively() {
        use super::filtered_indices;
        let h = vec![
            entry("Apple Pie"),
            entry("Banana"),
            entry("apple cider"),
            entry("Pineapple"),
        ];
        // 空查询 = 全部 / an empty query returns everything.
        assert_eq!(filtered_indices(&h, ""), vec![0, 1, 2, 3]);
        // 大小写不敏感子串 / case-insensitive substring.
        assert_eq!(filtered_indices(&h, "apple"), vec![0, 2, 3]);
        // 无匹配 → 空 / no match -> empty.
        assert!(filtered_indices(&h, "orange").is_empty());
        // 前缀/单字符 / prefix and single chars.
        assert_eq!(filtered_indices(&h, "ban"), vec![1]);
    }

    #[test]
    fn mapped_index_goes_through_the_filtered_list() {
        use super::{filtered_indices, mapped_index};
        let h = vec![
            entry("Apple"),
            entry("Banana"),
            entry("Cherry"),
            entry("Apricot"),
        ];
        let filtered = filtered_indices(&h, "a");
        // 显示顺序 = 匹配项的顺序;映射回历史索引(Apple / Banana / Apricot 均含 'a')。
        // Display order = the matched order; mapped back to history indices (all contain 'a').
        let expected = vec![0usize, 1usize, 3usize];
        assert_eq!(filtered, expected);
        // mapped_index 需要 FILTERED 是当前列表——直接构造验证边界行为。
        // mapped_index reads the global FILTERED; construct it to verify boundary behavior.
        *super::FILTERED.lock().unwrap() = filtered.clone();
        assert_eq!(mapped_index(0), Some(0));
        assert_eq!(mapped_index(2), Some(3));
        assert_eq!(mapped_index(3), None);
    }

    #[test]
    fn estimate_lines_handles_width_and_newlines() {
        use super::estimate_lines;
        // 短文本一行 / short text stays on one line.
        assert_eq!(estimate_lines("hello", 60), 1);
        // 恰好 60 单位占满一行,不折行;61 个单位折成 2 行。
        // Exactly 60 units fill a line (no wrap); 61 units wrap to 2 lines.
        assert_eq!(estimate_lines(&"a".repeat(60), 60), 1);
        assert_eq!(estimate_lines(&"a".repeat(61), 60), 2);
        // 中文按 2 单位折算:30 个汉字占满一行,第 31 个折行。
        // CJK counts as 2 units: 30 hanzi fill a line, the 31st wraps.
        let hanzi: String = "中".repeat(30);
        assert_eq!(estimate_lines(&hanzi, 60), 1);
        assert_eq!(estimate_lines(&(hanzi + "中"), 60), 2);
        // 显式换行符单独成行。
        // Explicit newlines start new lines.
        assert_eq!(estimate_lines("ab\ncd", 60), 2);
        assert_eq!(estimate_lines("ab\ncd\nef", 60), 3);
    }

    #[test]
    fn truncate_to_lines_caps_at_max_lines() {
        use super::truncate_to_lines;
        // 短文本原样保留 / short text passes through.
        assert_eq!(truncate_to_lines("hello", 60, 3), "hello");
        // 超过 3 行:3 整行 + 省略号(180 字符 + '…')。
        // More than 3 lines: 3 full lines + the ellipsis (180 chars + '…').
        let long: String = "a".repeat(200);
        let t = truncate_to_lines(&long, 60, 3);
        assert_eq!(t.chars().count(), 181);
        assert!(t.ends_with('…'));
        // 显式换行也计入行数 / explicit newlines count toward the cap.
        let t3 = truncate_to_lines("l1\nl2\nl3\nl4", 60, 3);
        assert_eq!(t3, "l1\nl2\nl3…");
    }

    #[test]
    fn row_pitch_follows_line_count() {
        use super::{
            body_button_height, compute_pitches, row_button_height, text_lines, BODY_GAP, HEADER_H,
            MAX_TEXT_LINES, ROW_GAP,
        };
        // 固定行距:所有条目(长短文本)行距相同 = 标题栏 + 3 行正文 + 行距。
        // Fixed pitch: every entry (short or long text) shares the same pitch = header +
        // 3 body lines + the row gap.
        let texts = vec![entry("short"), entry(&"长".repeat(100))];
        let pitches = compute_pitches(&texts);
        assert_eq!(pitches[0], pitches[1]);
        assert_eq!(
            pitches[0],
            HEADER_H + BODY_GAP + row_button_height(MAX_TEXT_LINES) + ROW_GAP
        );
        // 按钮高 = 行数 * 行高 + 内边距。
        // Button height = lines * line height + padding.
        assert_eq!(
            row_button_height(1),
            1.0 * super::LINE_H + 2.0 * super::BTN_PAD_Y
        );
        assert_eq!(
            row_button_height(3),
            3.0 * super::LINE_H + 2.0 * super::BTN_PAD_Y
        );
        // 100 个汉字(200 单位)> 3 行上限 → 3 行。
        // 100 hanzi (200 units) exceeds the 3-line cap -> 3 lines.
        assert_eq!(text_lines(&"长".repeat(100)), 3);
        // 正文按钮高度只看正文行数,与来源无关(来源在标题栏里)。
        // The body button height follows the text only; the source lives in the header.
        let with_src = entry_with_source("short", "Safari");
        assert_eq!(body_button_height(&with_src), row_button_height(1));
        assert_eq!(
            body_button_height(&entry(&"长".repeat(100))),
            row_button_height(3)
        );
    }

    #[test]
    fn header_title_has_three_branches() {
        use super::header_title;
        // 开关开 + 有来源 → 应用名。
        // Toggle on + a source -> the app name.
        assert_eq!(
            header_title(&entry_with_source("t", "Safari"), true),
            "Safari"
        );
        // 开关开 + 无来源 → "未知来源"。
        // Toggle on + no source -> "unknown source".
        assert_eq!(
            header_title(&entry("t"), true),
            super::t("clipboard.unknown_source")
        );
        // 开关关 → 空(横条只放图标)。
        // Toggle off -> empty (the strip only hosts the icons).
        assert_eq!(header_title(&entry_with_source("t", "Safari"), false), "");
        assert_eq!(header_title(&entry("t"), false), "");
    }

    #[test]
    fn nav_arrow_moves_and_wraps() {
        use super::{nav_arrow, NO_SELECTION};
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
        // 无选中哨兵(usize::MAX):不溢出,按无选中处理(↓ → 0,↑ → 末条)。
        // The no-selection sentinel (usize::MAX): no overflow; treated as "no selection"
        // (↓ -> 0, ↑ -> the tail).
        assert_eq!(nav_arrow(125, NO_SELECTION, 3), Some(0));
        assert_eq!(nav_arrow(126, NO_SELECTION, 3), Some(2));
    }

    #[test]
    fn screen_containing_finds_the_cursor_screen() {
        use super::screen_containing;
        use objc2_foundation::{NSPoint, NSRect, NSSize};
        // 主屏 (0,0) 1440x900,副屏在左侧 (-800,0) 800x900。
        // Main screen at (0,0) 1440x900; a second screen to the left at (-800,0) 800x900.
        let frames = [
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1440.0, 900.0)),
            NSRect::new(NSPoint::new(-800.0, 0.0), NSSize::new(800.0, 900.0)),
        ];
        // 光标在主屏 / cursor on the main screen.
        assert_eq!(
            screen_containing(NSPoint::new(700.0, 500.0), &frames),
            Some(frames[0])
        );
        // 光标在左侧副屏(负 x)/ cursor on the left second screen (negative x).
        assert_eq!(
            screen_containing(NSPoint::new(-400.0, 200.0), &frames),
            Some(frames[1])
        );
        // 光标在屏幕外 → None / cursor off-screen -> None.
        assert_eq!(
            screen_containing(NSPoint::new(3000.0, 500.0), &frames),
            None
        );
    }

    #[test]
    fn picker_frame_follows_cursor_with_flips() {
        use super::{picker_frame_for, PICKER_CURSOR_OFF, PICKER_EDGE_MARGIN};
        use objc2_foundation::{NSPoint, NSRect, NSSize};
        let screen = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1440.0, 900.0));
        let w = 420.0;
        let h = 300.0;
        // 光标居中:窗口在光标右下方 16pt。
        // Centered cursor: the panel sits 16pt to its bottom-right.
        let f = picker_frame_for(NSPoint::new(700.0, 500.0), screen, w, h);
        assert_eq!(f.origin.x, 700.0 + PICKER_CURSOR_OFF);
        assert_eq!(f.origin.y, 500.0 - h - PICKER_CURSOR_OFF);
        // 右侧空间不足 → 翻转到光标左侧。
        // Not enough room on the right -> flip to the cursor's left.
        let f = picker_frame_for(NSPoint::new(1300.0, 500.0), screen, w, h);
        assert_eq!(f.origin.x, 1300.0 - w - PICKER_CURSOR_OFF);
        // 下方空间不足 → 翻转到光标上方。
        // Not enough room below -> flip above the cursor.
        let f = picker_frame_for(NSPoint::new(700.0, 50.0), screen, w, h);
        assert_eq!(f.origin.y, 50.0 + PICKER_CURSOR_OFF);
        // 角落(右下):两边都翻转后仍越界 → clamp 进屏幕内。
        // Bottom-right corner: both flips still overflow -> clamped inside the screen.
        let f = picker_frame_for(NSPoint::new(1400.0, 20.0), screen, w, h);
        assert!(f.origin.x >= screen.origin.x + PICKER_EDGE_MARGIN);
        assert!(f.origin.x + w <= screen.origin.x + screen.size.width - PICKER_EDGE_MARGIN);
        assert!(f.origin.y >= screen.origin.y + PICKER_EDGE_MARGIN);
        assert!(f.origin.y + h <= screen.origin.y + screen.size.height - PICKER_EDGE_MARGIN);
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
