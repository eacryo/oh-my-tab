//! 剪贴板文本分类、代码换行和显示映射。
//! Clipboard text classification, code wrapping, and display mapping.

use crate::ffi::{hex_to_ns_color, make_nsstring, release_obj, CFRelease};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::NSRange;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Arc, Mutex, OnceLock};

/// 剪贴板条目类型分类,供列表和详情浮窗共用。
/// Clipboard entry classification shared by the list and detail panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextKind {
    Plain,
    Url,
    Code,
}

const CODE_DISPLAY_CACHE_CAPACITY: usize = 64;

/// 等宽代码字体的实测字符步进(pt)。`.AppleSystemUIFontMonospaced-Regular` @14pt 实测
/// 8.654pt;取 8.66 让列数预算略保守(宁可早折一行,不可超出容器触发 AppKit 二次折行)。
/// Measured advance of the monospaced code font in points. `.AppleSystemUIFontMonospaced-Regular`
/// at 14pt measures 8.654pt; 8.66 keeps the column budget slightly conservative -- wrap a line
/// early rather than overflow the container and let AppKit add unmarked breaks.
pub(crate) const CODE_ADVANCE_PT: f64 = 8.66;

/// Tab 显示层展开的制表位宽度(列)。展开只影响显示,复制经 source map 还原原文 tab。
/// Tab stop width (columns) used by the display-layer expansion. Expansion affects display
/// only; copies restore the original tabs through the source map.
const TAB_STOP_COLUMNS: usize = 4;

/// 单个字符在等宽渲染下的视觉列宽:CJK/全角区段按 2 列计。此前一律按 1 列估算,含中文的
/// 行真实像素宽超出容器后被 AppKit 按字符硬切出无标记断点——正是"软换行后格式错乱"
/// 的根因之一(另一处是步进常量偏小)。
/// Visual columns of one character under monospace rendering: CJK/fullwidth ranges count as
/// two. Estimating them as one made CJK-laden lines exceed the container's pixel width, so
/// AppKit inserted unmarked character-level breaks -- one half of the messy-wrapping bug (the
/// other half was the understated advance constant).
fn char_columns(ch: char) -> usize {
    let c = ch as u32;
    if matches!(c,
        0x1100..=0x115F       // Hangul Jamo 谚文字母
        | 0x2E80..=0x303E     // CJK 部首与符号
        | 0x3041..=0x33FF     // 平假名/片假名/兼容区
        | 0x3400..=0x4DBF     // CJK 扩展 A
        | 0x4E00..=0x9FFF     // CJK 统一表意文字
        | 0xA000..=0xA4CF     // 彝文
        | 0xAC00..=0xD7A3     // 谚文音节
        | 0xF900..=0xFAFF     // CJK 兼容表意
        | 0xFE10..=0xFE19     // 竖排形式
        | 0xFE30..=0xFE6F     // CJK 兼容形式
        | 0xFF00..=0xFF60     // 全角 ASCII 与标点
        | 0xFFE0..=0xFFE6     // 全角符号
        | 0x1F300..=0x1FAFF   // Emoji(近似 2 列)
        | 0x20000..=0x3FFFD   // CJK 扩展 B–F
    ) {
        2
    } else {
        1
    }
}

/// Tab 展开的中间产物:`text` 把源码中的 tab 替换为对齐到制表位的空格;`to_source_utf16[k]`
/// 是展开文本第 k 个 UTF-16 位之前的原文 UTF-16 偏移(末位为原文总长),供换行机制把边界
/// 映射回原文。列计数从行首起算(遇 `\n` 清零);一个 tab 展开出的多格空格中,首格映射到
/// tab 起点、其余映射到 tab 结束——选中任意连续子集都能还原出原 tab 或其空区间。
/// Intermediate product of tab expansion: `text` replaces each tab with spaces aligned to tab
/// stops, and `to_source_utf16[k]` holds the source UTF-16 offset before the k-th unit of the
/// expanded text (last entry = source length) so the wrap machinery can map boundaries back.
/// Columns restart at every `\n`; among the spaces one tab expands into, the first maps to the
/// tab's start and the rest to its end -- selecting any contiguous subset restores either the
/// original tab or an empty range, never wrong characters.
struct ExpandedSource {
    text: String,
    to_source_utf16: Vec<usize>,
}

fn expand_tabs(source: &str) -> ExpandedSource {
    let mut text = String::with_capacity(source.len());
    let mut map = Vec::with_capacity(source.encode_utf16().count() + 1);
    let mut col = 0usize;
    let mut src16 = 0usize;
    for ch in source.chars() {
        match ch {
            '\n' => {
                map.push(src16);
                text.push('\n');
                col = 0;
            }
            // \r 渲染零宽,不计列。/ \r renders zero-width; do not count it.
            '\r' => {
                map.push(src16);
                text.push('\r');
            }
            '\t' => {
                let pad = TAB_STOP_COLUMNS - (col % TAB_STOP_COLUMNS);
                let after = src16 + ch.len_utf16();
                for step in 0..pad {
                    map.push(if step == 0 { src16 } else { after });
                    text.push(' ');
                }
                col += pad;
            }
            _ => {
                map.push(src16);
                text.push(ch);
                col += char_columns(ch);
            }
        }
        src16 += ch.len_utf16();
    }
    map.push(src16);
    ExpandedSource {
        text,
        to_source_utf16: map,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CodeDisplayCacheKey {
    content_hash: u64,
    content_len: usize,
    max_columns: usize,
    /// 显示模式:0 = 列表预览,1 = 详情软换行,2 = 详情非软换行(横向滚动)。
    /// Display mode: 0 = row preview, 1 = detail soft wrap, 2 = detail no-wrap.
    mode: u8,
    /// 软换行算法标识(见 `WrapAlgorithm`),防切换后命中脏缓存。
    /// Soft-wrap algorithm id (see `WrapAlgorithm`) so switching never hits stale entries.
    algorithm: u8,
}

/// 详情软换行算法(开发期在 `active_soft_wrap_algorithm` 内切换,不进配置)。
/// Soft-wrap algorithm for the detail panel (switched in code during development via
/// `active_soft_wrap_algorithm`; not persisted to config).
///
/// 启用/关闭软换行的运行时入口仍是详情工具栏的换行按钮(no-wrap 横向滚动路径);
/// 这里的变体只决定"开启时用哪种折行逻辑"。
/// The runtime enable/disable switch remains the wrap button in the detail toolbar (the
/// no-wrap horizontal-scroll path); these variants only pick WHICH logic runs when enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WrapAlgorithm {
    /// 结构优先级折行:逗号 > 运算符 > 成员 > 括号 > 空白,宽度按 `char_columns`
    /// 与 `CODE_ADVANCE_PT` 估算,U+2028 落在结构边界上。
    /// Structural-priority wrapping: comma > operator > member > parens > whitespace,
    /// widths estimated with `char_columns` and `CODE_ADVANCE_PT`, U+2028 on boundaries.
    StructuralPriority,
}

impl WrapAlgorithm {
    fn id(self) -> u8 {
        match self {
            WrapAlgorithm::StructuralPriority => 0,
        }
    }
}

/// 当前生效的软换行算法。开发期对比不同逻辑时改这里并重编译。
/// The active soft-wrap algorithm. While experimenting with alternatives, change this and
/// rebuild.
pub(crate) fn active_wrap_algorithm() -> WrapAlgorithm {
    WrapAlgorithm::StructuralPriority
}

pub(crate) struct PreparedCodeDisplay {
    pub(crate) text: String,
    pub(crate) source_map: Option<Arc<DisplaySourceMap>>,
}

// 格式化模型通过 Arc 共享;缓存命中不复制长文本或原文映射。
// Share formatted models through Arc so cache hits do not copy long text or source maps.
static CODE_DISPLAY_CACHE: OnceLock<Mutex<HashMap<CodeDisplayCacheKey, Arc<PreparedCodeDisplay>>>> =
    OnceLock::new();

fn looks_like_json(text: &str) -> bool {
    let starts = text.starts_with('{') || text.starts_with('[');
    starts && (text.contains("\":") || text.contains("\": "))
}

fn code_fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

pub(crate) fn classify_text(text: &str) -> TextKind {
    let t = text.trim();
    if t.is_empty() {
        return TextKind::Plain;
    }
    // HTML/JSON 结构优先于 URL;结构化内容可能包含链接字段,不能因此被整段判为链接。
    // HTML/JSON structure takes precedence over URLs; structured content may contain URL fields.
    if looks_like_html(t) || looks_like_json(t) {
        return TextKind::Code;
    }
    // 仅整段本身是 URL 才归入链接。不能用 `contains("://")`:代码、JSON 之外的
    // 普通片段也可能含 URL 字符串,却不应整行变蓝或从代码筛选中消失。
    // Classify as Link only when the entire entry is a URL. Do not use `contains("://")`:
    // non-JSON code and prose can contain a URL string without becoming a blue link row.
    if is_standalone_url(t) {
        return TextKind::Url;
    }
    // 代码:多行 + 明显的代码特征(括号对/分号/缩进/常见关键字)。
    // Code: multi-line + code-ish cues (paren pairs / semicolons / indentation / keywords).
    let has_newline = text.contains('\n');
    let has_code_cues = text.contains('{')
        || text.contains(';')
        || text.starts_with('#')
        || text.starts_with("fn ")
        || text.starts_with("def ")
        || text.starts_with("import ")
        || text.starts_with("const ")
        || text.starts_with("let ")
        || text
            .lines()
            .any(|l| l.starts_with(' ') && !l.trim().is_empty());
    if has_newline && has_code_cues {
        TextKind::Code
    } else {
        TextKind::Plain
    }
}

/// 判断去除首尾空白后的完整条目是否是一条 URL。scheme 必须从开头开始,避免将
/// `let endpoint = \"https://…\"` 之类的代码误归入链接;空白也意味着不是单一 URL。
/// Decide whether the complete trimmed entry is one URL. The scheme must begin at offset zero,
/// avoiding code such as `let endpoint = \"https://…\"`; whitespace also means it is not one URL.
fn is_standalone_url(text: &str) -> bool {
    if text.is_empty() || text.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return false;
    }
    if let Some(scheme_end) = text.find("://") {
        let scheme = &text[..scheme_end];
        return !scheme.is_empty()
            && scheme.bytes().enumerate().all(|(index, byte)| {
                if index == 0 {
                    byte.is_ascii_alphabetic()
                } else {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.')
                }
            })
            && !text[scheme_end + 3..].is_empty();
    }
    text.starts_with("www.") && text.len() > "www.".len()
}

fn matches_ignore_ascii_case(word: &str, candidates: &[&str]) -> bool {
    candidates
        .iter()
        .any(|candidate| word.eq_ignore_ascii_case(candidate))
}

fn is_known_html_tag(name: &str) -> bool {
    matches_ignore_ascii_case(
        name,
        &[
            "a", "article", "aside", "audio", "b", "body", "button", "canvas", "code", "details",
            "dialog", "div", "em", "fieldset", "footer", "form", "h1", "h2", "h3", "h4", "h5",
            "h6", "head", "header", "html", "i", "iframe", "img", "input", "label", "li", "link",
            "main", "meta", "nav", "ol", "option", "p", "picture", "pre", "script", "section",
            "select", "small", "source", "span", "strong", "style", "summary", "svg", "table",
            "tbody", "td", "template", "textarea", "tfoot", "th", "thead", "title", "tr", "u",
            "ul", "video",
        ],
    )
}

fn has_matching_closing_tag(bytes: &[u8], name: &[u8], from: usize) -> bool {
    bytes[from..]
        .windows(name.len() + 2)
        .enumerate()
        .any(|(offset, window)| {
            let boundary = from + offset + name.len() + 2;
            window.starts_with(b"</")
                && window[2..].eq_ignore_ascii_case(name)
                && bytes
                    .get(boundary)
                    .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b'>')
        })
}

/// 轻量判断文本是否像 HTML/XML,避免把 Java 泛型和 C++ include 误判成标签。
/// Cheap HTML/XML detection that avoids mistaking Java generics and C++ includes for tags.
fn looks_like_html(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut search_from = 0;
    let mut closing_tag_checks = 0usize;
    while search_from < bytes.len() {
        let Some(relative_open) = bytes[search_from..].iter().position(|&b| b == b'<') else {
            return false;
        };
        let open = search_from + relative_open;
        let Some(&next) = bytes.get(open + 1) else {
            return false;
        };
        if matches!(next, b'!' | b'?') {
            return true;
        }

        let mut name_start = open + 1;
        if bytes.get(name_start) == Some(&b'/') {
            name_start += 1;
        }
        if !bytes
            .get(name_start)
            .is_some_and(|byte| byte.is_ascii_alphabetic())
        {
            search_from = open + 1;
            continue;
        }
        let mut name_end = name_start + 1;
        while bytes
            .get(name_end)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b':'))
        {
            name_end += 1;
        }
        let name = &bytes[name_start..name_end];
        let valid_boundary = bytes
            .get(name_end)
            .is_some_and(|byte| byte.is_ascii_whitespace() || matches!(byte, b'/' | b'>'));
        let tag_end = bytes[name_end..]
            .iter()
            .position(|&byte| byte == b'>')
            .map(|offset| name_end + offset);
        let self_closing = tag_end.is_some_and(|end| {
            bytes[..end]
                .iter()
                .rposition(|byte| !byte.is_ascii_whitespace())
                .is_some_and(|last| bytes[last] == b'/')
        });
        // 未知 XML/JSX 名称只对前几个候选查找闭合标签,防止大量 C++ 泛型触发 O(n²)。
        // Search for closing tags for only the first few unknown XML/JSX candidates, preventing
        // large amounts of C++ generic syntax from turning detection into O(n²).
        let locally_credible = std::str::from_utf8(name).is_ok_and(is_known_html_tag)
            || name.contains(&b'-')
            || name.contains(&b':')
            || self_closing;
        let has_closing_tag = if !locally_credible && closing_tag_checks < 4 {
            closing_tag_checks += 1;
            has_matching_closing_tag(bytes, name, name_end)
        } else {
            false
        };
        let credible_name = locally_credible || has_closing_tag;
        if credible_name && valid_boundary {
            return true;
        }
        search_from = open + 1;
    }
    false
}

fn is_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

/// 详情浮窗中的格式化代码及其原文偏移映射。
/// Formatted detail code together with a mapping back to the original source offsets.
#[derive(Clone)]
pub(crate) struct FormattedCode {
    pub(crate) text: String,
    pub(crate) source_map: DisplaySourceMap,
}

#[derive(Clone)]
pub(crate) struct DisplaySourceMap {
    pub(crate) source: String,
    // Each UTF-16 boundary in the display maps to a UTF-16 boundary in the source.
    boundaries: Vec<usize>,
}

impl DisplaySourceMap {
    pub(crate) fn source_range(&self, display_range: NSRange) -> NSRange {
        let start = self
            .boundaries
            .get(display_range.location)
            .copied()
            .unwrap_or_else(|| self.source.encode_utf16().count());
        let end_index = display_range
            .location
            .saturating_add(display_range.length)
            .min(self.boundaries.len().saturating_sub(1));
        let end = self.boundaries.get(end_index).copied().unwrap_or(start);
        NSRange::new(start.min(end), end.saturating_sub(start))
    }
}

/// 一次准备代码显示文本和原文映射;缓存和调用方通过 Arc 共享不可变结果。
/// Prepare code display text and its source mapping once; the cache and callers share the
/// immutable result through Arc.
pub(crate) fn prepare_code_display(source: &str, max_columns: usize) -> Arc<PreparedCodeDisplay> {
    let content_hash = code_fnv1a64(source.as_bytes());
    let key = CodeDisplayCacheKey {
        content_hash,
        content_len: source.len(),
        max_columns,
        mode: 0,
        algorithm: active_wrap_algorithm().id(),
    };
    let cache = CODE_DISPLAY_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(cached) = cache.lock().ok().and_then(|guard| guard.get(&key).cloned()) {
        return cached;
    }

    let formatted = format_code_for_display(source, max_columns);
    let prepared = Arc::new(build_prepared_code(formatted, false));
    if let Ok(mut guard) = cache.lock() {
        if guard.len() >= CODE_DISPLAY_CACHE_CAPACITY {
            if let Some(old_key) = guard.keys().next().copied() {
                guard.remove(&old_key);
            }
        }
        guard.insert(key, Arc::clone(&prepared));
    }
    prepared
}

fn build_prepared_code(formatted: FormattedCode, retain_source_map: bool) -> PreparedCodeDisplay {
    PreparedCodeDisplay {
        text: formatted.text,
        // 列表预览不需要复制映射,只有代码详情复制需要长期保留。
        // Row previews do not need a copy map; only code-detail copying retains it.
        source_map: retain_source_map.then(|| Arc::new(formatted.source_map)),
    }
}

/// 为 NSTextView 准备自定义软换行显示文本;只插入 U+2028,不插入额外缩进,缓存命中共享 Arc。
/// Prepare custom soft-wrapped display text for NSTextView by inserting only U+2028, without
/// extra indentation; cache hits share the Arc.
pub(crate) fn prepare_code_for_soft_wrap(
    source: &str,
    max_columns: usize,
) -> Arc<PreparedCodeDisplay> {
    let content_hash = code_fnv1a64(source.as_bytes());
    let key = CodeDisplayCacheKey {
        content_hash,
        content_len: source.len(),
        max_columns,
        mode: 1,
        algorithm: active_wrap_algorithm().id(),
    };
    let cache = CODE_DISPLAY_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(cached) = cache.lock().ok().and_then(|guard| guard.get(&key).cloned()) {
        return cached;
    }

    // 自定义软换行插入 U+2028,因此保留共享 source map 供复制选区使用。
    // 缓存命中只做哈希和 Arc clone。
    // Custom soft wrapping inserts U+2028, so retain a shared source map for copied selections.
    // A cache hit only hashes and clones the Arc.
    let formatted = format_code_for_soft_wrap(source, max_columns);
    let prepared = Arc::new(build_prepared_code(formatted, true));
    if let Ok(mut guard) = cache.lock() {
        if guard.len() >= CODE_DISPLAY_CACHE_CAPACITY {
            if let Some(old_key) = guard.keys().next().copied() {
                guard.remove(&old_key);
            }
        }
        guard.insert(key, Arc::clone(&prepared));
    }
    prepared
}

/// 非软换行代码详情的显示文本:不折行、不展开 tab,仅把段内空格显示为中点
/// (行首缩进保留真实空格,与软换行模式观感一致),并携带原文映射供复制还原。
/// 每个中点按 1:1 替换空格、边界即恒等映射——选中任意子集都还原原文。
pub(crate) fn prepare_code_no_wrap_display(source: &str) -> Arc<PreparedCodeDisplay> {
    let content_hash = code_fnv1a64(source.as_bytes());
    let key = CodeDisplayCacheKey {
        content_hash,
        content_len: source.len(),
        max_columns: 0,
        mode: 2,
        algorithm: active_wrap_algorithm().id(),
    };
    let cache = CODE_DISPLAY_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(cached) = cache.lock().ok().and_then(|guard| guard.get(&key).cloned()) {
        return cached;
    }

    // 逐字符构建:普通字符推 after 偏移;替换出的中点同样推该空格的 after 偏移,
    // 因此任意显示区间的边界都能连续覆盖到原文字符。段首空白保持原样。
    let offsets = source_utf16_offsets(source);
    let mut output = MappedCodeOutput {
        text: String::with_capacity(source.len()),
        boundaries: vec![0],
    };
    let mut line_start = 0;
    while line_start <= source.len() {
        let line_end = source[line_start..]
            .find('\n')
            .map(|offset| line_start + offset)
            .unwrap_or(source.len());
        append_marked_source_line(
            &mut output.text,
            &mut output.boundaries,
            source,
            &offsets,
            line_start,
            line_end,
        );
        if line_end < source.len() {
            append_mapped_source(
                &mut output.text,
                &mut output.boundaries,
                source,
                &offsets,
                line_end,
                line_end + 1,
            );
            line_start = line_end + 1;
        } else {
            break;
        }
    }
    let formatted = FormattedCode {
        text: output.text,
        source_map: DisplaySourceMap {
            source: source.to_owned(),
            boundaries: output.boundaries,
        },
    };
    let prepared = Arc::new(build_prepared_code(formatted, true));
    if let Ok(mut guard) = cache.lock() {
        if guard.len() >= CODE_DISPLAY_CACHE_CAPACITY {
            if let Some(old_key) = guard.keys().next().copied() {
                guard.remove(&old_key);
            }
        }
        guard.insert(key, Arc::clone(&prepared));
    }
    prepared
}

/// 单行标记构建:前导空白(tab 与空格)原样直拷;其余空格显示为中点,
/// 但边界仍指向该空格自身(after 偏移),复制还原不受影响。
fn append_marked_source_line(
    out: &mut String,
    boundaries: &mut Vec<usize>,
    source: &str,
    offsets: &[usize],
    start: usize,
    end: usize,
) {
    if let Some(last) = boundaries.last_mut() {
        *last = offsets[start];
    }
    let mut byte = start;
    let mut in_leading_ws = true;
    while byte < end {
        let ch = source[byte..].chars().next().unwrap();
        let next = byte + ch.len_utf8();
        // 首个非空白字符结束缩进段;其后的空格才显示为中点。
        if ch != ' ' && ch != '\t' {
            in_leading_ws = false;
        }
        // 行首空白(tab 与空格)保持字面;其余空格显示为中点,
        // 但边界仍指向该空格自身(after 偏移),复制还原不受影响。
        let displayed = if ch == ' ' && !in_leading_ws {
            '·'
        } else {
            ch
        };
        out.push(displayed);
        for _ in 0..ch.len_utf16() {
            boundaries.push(offsets[next]);
        }
        byte = next;
    }
}

fn source_utf16_offsets(source: &str) -> Vec<usize> {
    let mut offsets = vec![0; source.len() + 1];
    let mut utf16 = 0;
    for (byte, ch) in source.char_indices() {
        offsets[byte] = utf16;
        utf16 += ch.len_utf16();
        offsets[byte + ch.len_utf8()] = utf16;
    }
    offsets
}

fn append_mapped_source(
    out: &mut String,
    boundaries: &mut Vec<usize>,
    source: &str,
    offsets: &[usize],
    start: usize,
    end: usize,
) {
    if let Some(last) = boundaries.last_mut() {
        *last = offsets[start];
    }
    let mut byte = start;
    while byte < end {
        let ch = source[byte..].chars().next().unwrap();
        let next = byte + ch.len_utf8();
        out.push(ch);
        for _ in 0..ch.len_utf16() {
            boundaries.push(offsets[next]);
        }
        byte = next;
    }
}

fn append_mapped_insert(
    out: &mut String,
    boundaries: &mut Vec<usize>,
    text: &str,
    source_offset: usize,
) {
    out.push_str(text);
    for ch in text.chars() {
        for _ in 0..ch.len_utf16() {
            boundaries.push(source_offset);
        }
    }
}

/// 将片段累加到可用列宽;在下一个字符超宽前立即停止并返回 false。
/// Accumulate a segment within the available columns; stop before the next overflowing character
/// and return false.
fn extend_visual_width_with_limit(
    source: &str,
    byte: &mut usize,
    end: usize,
    width: &mut usize,
    trimmed_width: &mut usize,
    available: usize,
) -> bool {
    while *byte < end {
        let ch = source[*byte..].chars().next().unwrap();
        // 输入是 tab 展开后的文本;列宽按真实渲染步进(CJK/全角 = 2)。
        // Input is tab-expanded text; columns follow real rendered widths (CJK/fullwidth = 2).
        let next_width = *width + char_columns(ch);
        if next_width > available {
            return false;
        }
        *width = next_width;
        *byte += ch.len_utf8();
        if ch != ' ' && ch != '\t' {
            *trimmed_width = *width;
        }
    }
    true
}

/// 代码软换行优先级:逗号 > 运算符 > 成员访问 > 参数边界 > 空白。
/// Code soft-wrap priority: comma > operator > member access > parameter boundary > whitespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum CodeBreakPriority {
    Comma,
    Operator,
    Member,
    Parameter,
    Whitespace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CodeBreak {
    byte: usize,
    priority: CodeBreakPriority,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodeWrapStyle {
    /// 列表预览:插入普通换行和悬挂缩进,并显示空格标记。
    /// Row preview: insert regular newlines and hanging indentation, with visible spaces.
    Preview,
    /// 详情软换行:插入 U+2028 行分隔符,不改变段落或复制出的原文。
    /// Detail soft wrap: insert U+2028 line separators without changing the paragraph or copied source.
    Detail,
}

struct MappedCodeOutput {
    text: String,
    boundaries: Vec<usize>,
}

fn leading_indent(text: &str, start: usize, end: usize) -> (usize, usize) {
    // 输入是 tab 展开后的文本,缩进只可能是空格。
    // Input is tab-expanded text, so indentation can only be spaces.
    let mut byte = start;
    let mut columns = 0;
    while byte < end {
        if text.as_bytes()[byte] == b' ' {
            columns += 1;
            byte += 1;
        } else {
            break;
        }
    }
    (byte, columns)
}

fn code_breaks(source: &str, start: usize, end: usize) -> Vec<CodeBreak> {
    let bytes = source.as_bytes();
    let mut points = Vec::new();
    let mut byte = start;
    while byte < end {
        let ch = source[byte..].chars().next().unwrap();
        if ch.is_whitespace() {
            while byte < end {
                let c = source[byte..].chars().next().unwrap();
                if !c.is_whitespace() {
                    break;
                }
                byte += c.len_utf8();
            }
            points.push(CodeBreak {
                byte,
                priority: CodeBreakPriority::Whitespace,
            });
            continue;
        }
        // 字符串内部的标点不是代码结构边界;超长字符串最终仍会走任意字符折行。
        // Punctuation inside strings is not a code-structure boundary; an overlong string still
        // falls back to arbitrary character wrapping.
        if matches!(ch, '\'' | '"' | '`') {
            let quote = ch;
            byte += ch.len_utf8();
            while byte < end {
                let c = source[byte..].chars().next().unwrap();
                byte += c.len_utf8();
                if c == '\\' && byte < end {
                    let escaped = source[byte..].chars().next().unwrap();
                    byte += escaped.len_utf8();
                } else if c == quote {
                    break;
                }
            }
            continue;
        }
        if is_identifier_start(bytes[byte]) || bytes[byte].is_ascii_digit() {
            byte += ch.len_utf8();
            while byte < end {
                let c = source[byte..].chars().next().unwrap();
                if c.is_ascii_alphanumeric() || c == '_' {
                    byte += c.len_utf8();
                } else {
                    break;
                }
            }
            continue;
        }

        let start_byte = byte;
        byte += ch.len_utf8();
        match ch {
            ',' => points.push(CodeBreak {
                byte,
                priority: CodeBreakPriority::Comma,
            }),
            '.' => {
                points.push(CodeBreak {
                    byte: start_byte,
                    priority: CodeBreakPriority::Member,
                });
                points.push(CodeBreak {
                    byte,
                    priority: CodeBreakPriority::Member,
                });
            }
            '(' | ')' => {
                points.push(CodeBreak {
                    byte: start_byte,
                    priority: CodeBreakPriority::Parameter,
                });
                points.push(CodeBreak {
                    byte,
                    priority: CodeBreakPriority::Parameter,
                });
            }
            '=' | '+' | '-' | '&' | '|' => {
                // 把 ==、=>、+=、->、&&、|| 等连续运算符视为一个边界单元。
                // Treat consecutive operators such as ==, =>, +=, ->, &&, and || as one unit.
                while byte < end
                    && matches!(source.as_bytes()[byte], b'=' | b'+' | b'-' | b'&' | b'|')
                {
                    byte += 1;
                }
                points.push(CodeBreak {
                    byte: start_byte,
                    priority: CodeBreakPriority::Operator,
                });
                points.push(CodeBreak {
                    byte,
                    priority: CodeBreakPriority::Operator,
                });
            }
            _ => {}
        }
    }
    points.sort_unstable_by_key(|point| (point.byte, point.priority));
    // 同一位置可能同时属于多个类别,保留优先级最高的一个。
    // A position can belong to multiple categories; retain its highest-priority category.
    points.dedup_by_key(|point| point.byte);
    points
}

fn trim_trailing_space(source: &str, start: usize, end: usize) -> usize {
    let mut trimmed = end;
    while trimmed > start {
        let ch = source[..trimmed].chars().next_back().unwrap();
        if ch == ' ' || ch == '\t' {
            trimmed -= ch.len_utf8();
        } else {
            break;
        }
    }
    trimmed
}

fn skip_leading_space(source: &str, mut byte: usize, end: usize) -> usize {
    while byte < end {
        let ch = source[byte..].chars().next().unwrap();
        if ch == ' ' || ch == '\t' {
            byte += ch.len_utf8();
        } else {
            break;
        }
    }
    byte
}

fn append_code_line(
    source: &str,
    offsets: &[usize],
    line_start: usize,
    line_end: usize,
    max_columns: usize,
    style: CodeWrapStyle,
    output: &mut MappedCodeOutput,
) {
    if line_start == line_end {
        return;
    }
    let (content_start, indent_columns) = leading_indent(source, line_start, line_end);
    let breaks = code_breaks(source, content_start, line_end);
    let mut chunk_start = line_start;
    let mut first_chunk = true;
    while chunk_start < line_end {
        let prefix_columns = if first_chunk {
            0
        } else {
            match style {
                CodeWrapStyle::Preview => indent_columns + 4,
                CodeWrapStyle::Detail => indent_columns.max(4),
            }
        };
        let available = max_columns.saturating_sub(prefix_columns).max(12);
        let mut break_index = breaks.partition_point(|point| point.byte <= chunk_start);
        let mut byte = chunk_start;
        let mut width = 0;
        let mut trimmed_width = 0;
        let mut best = [None; 5];
        let mut fits = true;

        // 每个字符最多参与一个输出段的宽度扫描。遇到超宽字符立即停下,既保留类别
        // 优先级,也避免长标识符/压缩 HTML 在任意字符兜底时退化为 O(n²)。
        // Each character participates in at most one output chunk's width scan. Stop at the
        // overflowing character so category priority is preserved without turning long
        // identifiers or minified HTML into O(n²) during arbitrary-character fallback.
        while break_index < breaks.len() && breaks[break_index].byte <= line_end {
            let point = breaks[break_index];
            if !extend_visual_width_with_limit(
                source,
                &mut byte,
                point.byte,
                &mut width,
                &mut trimmed_width,
                available,
            ) {
                fits = false;
                break;
            }
            if trimmed_width <= available {
                best[point.priority as usize] = Some(point.byte);
            }
            break_index += 1;
        }
        if fits {
            fits = extend_visual_width_with_limit(
                source,
                &mut byte,
                line_end,
                &mut width,
                &mut trimmed_width,
                available,
            );
        }
        if fits {
            if !first_chunk && style == CodeWrapStyle::Preview {
                append_mapped_insert(
                    &mut output.text,
                    &mut output.boundaries,
                    &" ".repeat(indent_columns + 4),
                    offsets[chunk_start],
                );
            }
            append_mapped_source(
                &mut output.text,
                &mut output.boundaries,
                source,
                offsets,
                chunk_start,
                line_end,
            );
            break;
        }

        // 先选最高优先级类别中最靠后的可容纳位置;所有结构边界都放不下时,
        // `byte` 就是最后一个可容纳的 UTF-8 字符边界,允许在任意字符处折行。
        // Pick the latest fitting point from the highest-priority category. If no structural
        // boundary fits, `byte` is the last fitting UTF-8 character boundary and permits an
        // arbitrary-character wrap.
        let mut break_point = best.into_iter().flatten().next().unwrap_or(byte);
        if break_point <= chunk_start {
            let ch = source[chunk_start..].chars().next().unwrap();
            break_point = chunk_start + ch.len_utf8();
        }
        let trimmed = trim_trailing_space(source, chunk_start, break_point);
        let chunk_end = if trimmed > chunk_start {
            trimmed
        } else {
            break_point
        };
        if !first_chunk && style == CodeWrapStyle::Preview {
            append_mapped_insert(
                &mut output.text,
                &mut output.boundaries,
                &" ".repeat(indent_columns + 4),
                offsets[chunk_start],
            );
        }
        append_mapped_source(
            &mut output.text,
            &mut output.boundaries,
            source,
            offsets,
            chunk_start,
            chunk_end,
        );
        if let Some(last) = output.boundaries.last_mut() {
            *last = offsets[break_point];
        }
        let separator = match style {
            CodeWrapStyle::Preview => "\n",
            CodeWrapStyle::Detail => "\u{2028}",
        };
        append_mapped_insert(
            &mut output.text,
            &mut output.boundaries,
            separator,
            offsets[break_point],
        );
        chunk_start = skip_leading_space(source, break_point, line_end);
        first_chunk = false;
    }
}

fn format_code_with_style(source: &str, max_columns: usize, style: CodeWrapStyle) -> FormattedCode {
    // 显示层先展开 tab:折行机制只面对无 tab 文本,列宽即真实渲染宽度;边界最终经
    // to_source_utf16 映射回原文空间,复制保真不受影响。
    // Expand tabs on the display layer first so the wrap machinery only sees tab-free text
    // and column widths equal real rendered widths; boundaries are mapped back into source
    // space at the end, keeping copies faithful.
    let expanded = expand_tabs(source);
    // 原文引用先行保存:下方 source 遮蔽为展开文本,而 map.source 必须是原文。
    // Keep the original reference first: `source` below is shadowed by the expanded text,
    // while the map's `source` field must hold the original.
    let original = source;
    let source = &expanded.text;
    let offsets = source_utf16_offsets(source);
    let mut output = MappedCodeOutput {
        text: String::new(),
        boundaries: vec![0],
    };
    let mut line_start = 0;
    while line_start <= source.len() {
        let line_end = source[line_start..]
            .find('\n')
            .map(|offset| line_start + offset)
            .unwrap_or(source.len());
        append_code_line(
            source,
            &offsets,
            line_start,
            line_end,
            max_columns,
            style,
            &mut output,
        );
        if line_end < source.len() {
            append_mapped_source(
                &mut output.text,
                &mut output.boundaries,
                source,
                &offsets,
                line_end,
                line_end + 1,
            );
            line_start = line_end + 1;
        } else {
            break;
        }
    }
    // 列表预览整行把空格显示为中点;详情同样显示中点,但保留段首缩进空格——
    // apply_code_paragraph_styles 依赖前导空格计算 headIndent,缩进必须是真实空格。
    // 替换为 1:1 UTF-16(' ' ↔ '·'),边界数组与 source map 完全不受影响。
    // Row previews replace every space with a middle dot; details do too, EXCEPT the leading
    // indentation run of each visual line -- apply_code_paragraph_styles derives headIndent
    // from those leading spaces, so they must remain real spaces. The swap is 1:1 UTF-16
    // (' ' ↔ '·'), leaving boundary arrays and the source map untouched.
    match style {
        CodeWrapStyle::Preview => {
            if output.text.contains(' ') {
                output.text = output.text.replace(' ', "·");
            }
        }
        CodeWrapStyle::Detail => {
            let mut marked = String::with_capacity(output.text.len());
            let mut in_leading_ws = true;
            for ch in output.text.chars() {
                match ch {
                    '\n' | '\u{2028}' => {
                        marked.push(ch);
                        in_leading_ws = true;
                    }
                    ' ' if in_leading_ws => marked.push(' '),
                    ' ' => marked.push('·'),
                    other => {
                        in_leading_ws = false;
                        marked.push(other);
                    }
                }
            }
            output.text = marked;
        }
    }
    // 边界从"展开文本"空间映射回原文 UTF-16 空间。
    // Map boundaries from expanded-text space back into original-source space.
    let expanded_len16 = expanded.text.encode_utf16().count();
    let boundaries = output
        .boundaries
        .into_iter()
        .map(|boundary| expanded.to_source_utf16[boundary.min(expanded_len16)])
        .collect();
    FormattedCode {
        text: output.text,
        source_map: DisplaySourceMap {
            source: original.to_owned(),
            boundaries,
        },
    }
}

/// 代码列表预览格式化:插入视觉换行/悬挂缩进并显示空格,原文由映射保留。
/// Format code row previews with visual breaks, hanging indentation, and visible spaces while
/// retaining the source through the offset map.
pub(crate) fn format_code_for_display(source: &str, max_columns: usize) -> FormattedCode {
    format_code_with_style(source, max_columns, CodeWrapStyle::Preview)
}

/// 代码详情软换行:按代码边界优先级插入 U+2028,不插入缩进或可复制字符。
/// Soft-wrap code details by inserting U+2028 according to code-boundary priorities, without
/// inserted indentation or copyable characters.
fn format_code_for_soft_wrap(source: &str, max_columns: usize) -> FormattedCode {
    format_code_with_style(source, max_columns, CodeWrapStyle::Detail)
}

/// 给代码中的可见空格设置淡色,缩进空格比普通空格稍明显。
/// Tint visible code spaces; indentation spaces are slightly stronger than ordinary spaces.
pub(crate) unsafe fn apply_visible_space_markers(storage: *mut AnyObject, text: &str) {
    // 大片段的每个可见空格都会写一次属性;合并编辑避免 NSTextStorage 每次都通知布局。
    // A large snippet writes an attribute for every visible space; batch edits so NSTextStorage
    // does not notify layout after every individual mutation.
    let _: () = msg_send![storage, beginEditing];
    let color_key = make_nsstring("NSColor");
    let mut location = 0;
    let mut at_line_start = true;
    for ch in text.chars() {
        let length = ch.len_utf16();
        if ch == '·' {
            let alpha = if at_line_start { 0.20 } else { 0.16 };
            let color: *mut AnyObject = msg_send![
                class!(NSColor),
                colorWithWhite: 0.0f64,
                alpha: alpha
            ];
            let _: () = msg_send![
                storage,
                addAttribute: color_key,
                value: color,
                range: NSRange::new(location, length)
            ];
            location += length;
            continue;
        }
        // 行首判定包含 U+2028:详情的视觉行由软换行分隔符开启。
        // Line starts include U+2028: detail visual lines begin at the soft-wrap separator.
        at_line_start = ch == '\n' || ch == '\u{2028}';
        location += length;
    }
    CFRelease(color_key as *const c_void);
    let _: () = msg_send![storage, endEditing];
}

/// 给代码的每个显示段落设置悬挂缩进,即使 NSTextView 仍需二次换行也不会顶到最左侧。
/// Set hanging indents on every displayed code paragraph so any fallback NSTextView wrap
/// also stays indented instead of jumping to the far left.
pub(crate) unsafe fn apply_code_paragraph_styles(storage: *mut AnyObject, text: &str) {
    let _: () = msg_send![storage, beginEditing];
    let style_key = make_nsstring("NSParagraphStyle");
    let mut styles: HashMap<usize, *mut AnyObject> = HashMap::new();
    let mut location = 0usize;
    let mut group_start = 0usize;
    let mut group_indent = None;

    for line in text.split_inclusive('\n') {
        let content = line.strip_suffix('\n').unwrap_or(line);
        let (_, indent_columns) = leading_indent(content, 0, content.len());
        let continuation_columns = indent_columns.max(4);
        let length = line.encode_utf16().count();
        if length == 0 {
            continue;
        }

        if group_indent != Some(continuation_columns) {
            if let Some(previous_indent) = group_indent {
                if location > group_start {
                    let style = *styles.entry(previous_indent).or_insert_with(|| {
                        let style: *mut AnyObject =
                            msg_send![class!(NSMutableParagraphStyle), alloc];
                        let style: *mut AnyObject = msg_send![style, init];
                        let _: () = msg_send![
                            style,
                            setHeadIndent: previous_indent as f64 * CODE_ADVANCE_PT
                        ];
                        let _: () = msg_send![style, setFirstLineHeadIndent: 0.0f64];
                        // 自定义 U+2028 已选择结构断点;像素宽度仍溢出时按字符兜底。
                        // U+2028 already selects structural breaks; fall back by character on pixel overflow.
                        let _: () = msg_send![style, setLineBreakMode: 1isize]; // NSLineBreakByCharWrapping
                        style
                    });
                    let _: () = msg_send![
                        storage,
                        addAttribute: style_key,
                        value: style,
                        range: NSRange::new(group_start, location - group_start)
                    ];
                }
            }
            group_start = location;
            group_indent = Some(continuation_columns);
        }
        location += length;
    }

    if let Some(indent) = group_indent {
        if location > group_start {
            let style = *styles.entry(indent).or_insert_with(|| {
                let style: *mut AnyObject = msg_send![class!(NSMutableParagraphStyle), alloc];
                let style: *mut AnyObject = msg_send![style, init];
                let _: () = msg_send![style, setHeadIndent: indent as f64 * CODE_ADVANCE_PT];
                let _: () = msg_send![style, setFirstLineHeadIndent: 0.0f64];
                // 自定义 U+2028 已选择结构断点;像素宽度仍溢出时按字符兜底。
                // U+2028 already selects structural breaks; fall back by character on pixel overflow.
                let _: () = msg_send![style, setLineBreakMode: 1isize]; // NSLineBreakByCharWrapping
                style
            });
            let _: () = msg_send![
                storage,
                addAttribute: style_key,
                value: style,
                range: NSRange::new(group_start, location - group_start)
            ];
        }
    }

    for style in styles.into_values() {
        release_obj(style);
    }
    CFRelease(style_key as *const c_void);
    let _: () = msg_send![storage, endEditing];
}

/// 链接仍使用列表既有的蓝色;代码只保留等宽字体和换行,不再做语法着色。
/// Links retain the list's existing blue color; code keeps only monospace layout and wrapping,
/// with no syntax coloring.
pub(crate) unsafe fn apply_link_color(storage: *mut AnyObject, text: &str, kind: TextKind) {
    if kind != TextKind::Url || text.is_empty() {
        return;
    }
    let _: () = msg_send![storage, beginEditing];
    let color_key = make_nsstring("NSColor");
    let color = hex_to_ns_color(0x205BA6B8);
    let _: () = msg_send![
        storage,
        addAttribute: color_key,
        value: color,
        range: NSRange::new(0, text.encode_utf16().count())
    ];
    CFRelease(color_key as *const c_void);
    let _: () = msg_send![storage, endEditing];
}

#[cfg(test)]
mod tests {
    use super::{
        apply_visible_space_markers, char_columns, format_code_for_soft_wrap,
        prepare_code_for_soft_wrap, CODE_ADVANCE_PT, TAB_STOP_COLUMNS,
    };
    use crate::ffi::{make_nsstring, nsstring_to_rust, CFRelease};
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send, sel};
    use objc2_foundation::NSRange;
    use std::ffi::c_void;
    use std::sync::Arc;

    /// 按 \n 与 U+2028 切出最终视觉行(与详情渲染的行边界一致)。
    /// Split into final visual lines by \n and U+2028 (matching the detail rendering).
    fn visual_segments(text: &str) -> Vec<&str> {
        text.split(['\n', '\u{2028}']).collect()
    }

    /// 用 source map 把显示 UTF-16 区间还原为原文切片。
    /// Map a display UTF-16 range back to a slice of the original source.
    fn utf16_slice(source: &str, range: NSRange) -> String {
        let units: Vec<u16> = source.encode_utf16().collect();
        let start = range.location as usize;
        let end = start + range.length as usize;
        String::from_utf16(&units[start..end]).expect("mapped range must stay on char boundaries")
    }

    #[test]
    fn prepared_code_cache_hits_share_the_model() {
        let source = "fn shared_cache() {\n    println!(\"cached\");\n}";
        let first = prepare_code_for_soft_wrap(source, 48);
        let second = prepare_code_for_soft_wrap(source, 48);
        assert!(
            Arc::ptr_eq(&first, &second),
            "prepared-code cache hits must not clone the model"
        );
    }

    #[test]
    fn soft_wrap_prefers_code_boundaries_then_falls_back_to_characters() {
        let first_break = |source: &str| {
            // 断言按字符位计数:'·' 在 UTF-8 中占 2 字节,字节索引会随点号出现而漂移;
            // 期望值本就是列语义。
            // Assertions count CHARACTER positions: '·' costs 2 bytes in UTF-8, so byte
            // indices drift once dots appear; the expectations are column semantics anyway.
            format_code_for_soft_wrap(source, 18)
                .text
                .char_indices()
                .position(|(_, ch)| ch == '\u{2028}')
                .expect("source must wrap")
        };
        // 每个样例都让更低优先级的断点更靠近右边;仍应选择更高优先级类别。
        // Each sample puts a lower-priority point farther right; the higher-priority category
        // must still win.
        assert_eq!(first_break("aa, bb = cc.dd(ee) tailtailtail"), 3); // comma
        assert_eq!(first_break("aa = bb.cc(dd) tailtailtail"), 4); // operator
        assert_eq!(first_break("aa.bb(cc) dd tailtailtail"), 3); // member access
        assert_eq!(first_break("aabb(cc) dd tailtailtail"), 8); // parameter boundary
        assert_eq!(first_break("aabbcc dd tailtailtail"), 9); // whitespace
        assert_eq!(first_break("abcdefghijklmnopqrstuvwxyz"), 18); // arbitrary character
    }

    /// 详情点号契约:段内空格显示为中点,行首缩进保持真实空格(段落 headIndent 依赖),
    /// 且 source map 往返仍逐字符还原含空格的原文。
    /// Detail dot contract: interior spaces render as middle dots while the leading
    /// indentation run stays real spaces (paragraph headIndent depends on them), and the
    /// source-map roundtrip still restores the original verbatim.
    #[test]
    fn detail_soft_wrap_marks_interior_spaces_but_keeps_indentation() {
        let source = "let value = compute(a, b);\n    return value.field;";
        let formatted = format_code_for_soft_wrap(source, 68);
        assert!(
            formatted.text.contains('·'),
            "interior spaces must render as middle dots"
        );
        for seg in visual_segments(&formatted.text) {
            let indent = seg.len() - seg.trim_start_matches(' ').len();
            assert!(
                !seg[indent..].contains(' '),
                "only leading indentation may keep spaces: {seg:?}"
            );
            if indent > 0 {
                assert!(seg[..indent].chars().all(|ch| ch == ' '));
            }
        }
        let display_len = formatted.text.encode_utf16().count();
        let range = formatted
            .source_map
            .source_range(NSRange::new(0, display_len));
        assert_eq!(
            utf16_slice(&formatted.source_map.source, range),
            source,
            "roundtrip must restore spaces verbatim"
        );
    }

    /// 染色函数行首判定契约:U+2028 之后的首个中点也按"行首"染色。
    /// 用 NSMutableAttributedString 承载(NSTextStorage 没有 initWithString:)。
    /// 注意:macOS 26 运行时只认复数形式 attributesAtIndex:effectiveRange:,
    /// 单数 attributeAtIndex:effectiveRange: 已不存在(respondsToSelector=false),
    /// 因此断言走 raw objc_msgSend + NSSelectorFromString 的复数选择器。
    /// Marker-tint contract: the first dot after a U+2028 must count as a line start.
    /// Uses an NSMutableAttributedString carrier (NSTextStorage has no initWithString:).
    /// NOTE: on macOS 26 the runtime only responds to the PLURAL
    /// attributesAtIndex:effectiveRange: -- the singular variant is gone
    /// (respondsToSelector = false), so assertions go through raw objc_msgSend +
    /// NSSelectorFromString with the plural selector.
    #[test]
    fn space_marker_tint_treats_u2028_as_a_line_start() {
        let text = "ab ·\u{2028} ·cd";
        // UTF-16 位图:a0 b1 sp2 ·3 ␨4 sp5 ·6 c7 d8 —— 中点在位 3 与位 6。
        // UTF-16 map: a0 b1 sp2 dot3 sep4 sp5 dot6 c7 d8 -- dots at offsets 3 and 6.
        unsafe {
            extern "C" {
                fn objc_msgSend();
                fn NSSelectorFromString(name: *const c_void) -> objc2::runtime::Sel;
            }
            type AttrFn = unsafe extern "C" fn(
                *mut c_void,
                objc2::runtime::Sel,
                u64,
                *mut NSRange,
            ) -> *mut AnyObject;
            let attr_fn: AttrFn = std::mem::transmute(objc_msgSend as *const ());

            let attr_alloc: *mut AnyObject = msg_send![class!(NSMutableAttributedString), alloc];
            let ns = make_nsstring(text);
            let storage: *mut AnyObject = msg_send![attr_alloc, initWithString: ns];
            CFRelease(ns as *const c_void);
            apply_visible_space_markers(storage, text);

            for (offset, expected) in [
                (0usize, false),
                (3usize, true),
                (5usize, false),
                (6usize, true),
            ] {
                let mut eff = NSRange::new(0, 0);
                let dict = attr_fn(
                    storage as *mut c_void,
                    NSSelectorFromString(
                        make_nsstring("attributesAtIndex:effectiveRange:") as *const c_void
                    ),
                    offset as u64,
                    &mut eff as *mut NSRange,
                );
                // attributesAtIndex 对界内索引恒返回非空字典(可能为空),空值检查
                // 无法区分;必须查 NSColor 键是否存在(染色标记的唯一属性)。
                // attributesAtIndex always returns a NON-NULL (possibly empty) dictionary
                // for in-bounds indexes, so a null check cannot discriminate -- look up the
                // NSColor key instead (the tint is the only attribute ever applied here).
                let color_key = make_nsstring("NSColor");
                let has_tint: *mut AnyObject = if dict.is_null() {
                    std::ptr::null_mut()
                } else {
                    msg_send![dict, objectForKey: color_key]
                };
                CFRelease(color_key as *const c_void);
                assert_eq!(
                    !has_tint.is_null(),
                    expected,
                    "tint presence at utf16 offset {offset} mismatched"
                );
            }
        }
    }

    /// 契约夹具:置顶片段同款的超宽中文注释、行尾注释、tab 缩进与纯 ASCII 长行。
    /// Contract fixtures: the pinned snippet's over-long CJK comments, a trailing inline
    /// comment, tab indentation, and a long pure-ASCII line.
    const CONTRACT_FIXTURES: [&str; 5] = [
        "// config.rs 在 CONFIG 初始化与 reload 后单向调用 apply_config_locale() 应用配置覆盖。",
        "/// 中文区分简体/繁体:含 Hant 或区域为 TW/HK/MO 视为繁体;其余(含 Hans、CN、SG、纯 zh)为简体。",
        "    messages: HashMap<String, String>, // 当前 locale 的扁平 key->string(locale==\"en\" 时与 EN_MESSAGES 相同)",
        "\tlet value = compute(arg_one, arg_two);\n\t\treturn value.field;",
        "veryLongObjectName.veryLongMethodName(firstArgument, secondArgument).thirdChainLink;",
    ];

    /// 核心回归契约:每个视觉段的估算像素宽不得超过详情容器宽(628pt)。
    /// 修复前 CJK 按 1 列计,这类行被误判"放得下",AppKit 兜底按字符硬切出无标记断点。
    /// Core regression contract: every visual segment's estimated pixel width must stay
    /// within the detail container (628pt). Before the fix CJK counted as one column, such
    /// lines were misjudged as fitting, and AppKit chopped them with unmarked breaks.
    #[test]
    fn soft_wrap_segments_stay_within_the_container_pixel_budget() {
        let container_pt = 628.0;
        // detail_code_max_columns(640):floor(628/8.66)-4
        let max_columns = 68;
        for source in CONTRACT_FIXTURES {
            let formatted = format_code_for_soft_wrap(source, max_columns);
            for seg in visual_segments(&formatted.text) {
                let cols: usize = seg.chars().map(char_columns).sum();
                assert!(
                    (cols as f64) * CODE_ADVANCE_PT <= container_pt + f64::EPSILON,
                    "segment of {cols} columns overflows {container_pt}pt: {seg:?}"
                );
                // 段列数同时不得突破预算(续行的前缀预留已在生成时扣除)。
                // Segment columns must also respect the budget (continuation prefix is
                // already reserved during generation).
                assert!(
                    cols <= max_columns.max(TAB_STOP_COLUMNS),
                    "segment of {cols} columns exceeds budget {max_columns}: {seg:?}"
                );
            }
        }
    }

    /// 复制保真契约:整段显示文本经 source map 还原后必须逐字符等于原文。
    /// Copy-fidelity contract: mapping the whole display text back through the source map
    /// must reproduce the original character-for-character.
    #[test]
    fn soft_wrap_source_map_roundtrips_every_fixture() {
        for source in CONTRACT_FIXTURES {
            let formatted = format_code_for_soft_wrap(source, 68);
            let display_len = formatted.text.encode_utf16().count();
            let range = formatted
                .source_map
                .source_range(NSRange::new(0, display_len));
            assert_eq!(
                utf16_slice(&formatted.source_map.source, range),
                source,
                "roundtrip must restore the original verbatim"
            );
        }
    }

    /// Tab 展开契约:缩进的显示空格映射回原 tab;选中任意子集不产生错位文本。
    /// Tab-expansion contract: the expanded indent spaces map back to the original tab, and
    /// selecting any subset never yields shifted text.
    #[test]
    fn expanded_tab_indent_maps_back_to_the_original_tab() {
        let source = "\tlet x = 1;";
        let formatted = format_code_for_soft_wrap(source, 48);
        assert!(
            formatted.text.starts_with(&" ".repeat(TAB_STOP_COLUMNS)),
            "leading tab must expand to aligned spaces"
        );
        let indent_range = formatted.source_map.source_range(NSRange::new(0, 4));
        assert_eq!((indent_range.location, indent_range.length), (0, 1));
        assert_eq!(
            utf16_slice(&formatted.source_map.source, indent_range),
            "\t"
        );
    }
}
