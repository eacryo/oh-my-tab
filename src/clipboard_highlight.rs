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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CodeDisplayCacheKey {
    content_hash: u64,
    content_len: usize,
    max_columns: usize,
    soft_wrap: bool,
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
        soft_wrap: false,
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
        soft_wrap: true,
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
        let next_width = *width + if ch == '\t' { 4 - (*width % 4) } else { 1 };
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
    let mut byte = start;
    let mut columns = 0;
    while byte < end {
        let ch = text[byte..].chars().next().unwrap();
        if ch == ' ' {
            columns += 1;
            byte += 1;
        } else if ch == '\t' {
            columns += 4 - (columns % 4);
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
    // 列表预览继续显示空格标记;详情保留普通空格,仅插入不可复制的 U+2028 软换行。
    // Row previews retain visible-space markers; details preserve regular spaces and only insert
    // non-copying U+2028 soft wraps.
    if style == CodeWrapStyle::Preview && output.text.contains(' ') {
        output.text = output.text.replace(' ', "·");
    }
    FormattedCode {
        text: output.text,
        source_map: DisplaySourceMap {
            source: source.to_owned(),
            boundaries: output.boundaries,
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
        at_line_start = ch == '\n';
        if ch != '\n' {
            at_line_start = false;
        }
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
                            setHeadIndent: previous_indent as f64 * 8.4
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
                let _: () = msg_send![style, setHeadIndent: indent as f64 * 8.4];
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
    use super::{format_code_for_soft_wrap, prepare_code_for_soft_wrap};
    use objc2_foundation::NSRange;
    use std::sync::Arc;

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
            format_code_for_soft_wrap(source, 18)
                .text
                .find('\u{2028}')
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

    #[test]
    fn soft_wrap_source_map_excludes_virtual_separators() {
        let source = "veryLongObject.member(firstArgument, secondArgument)";
        let formatted = format_code_for_soft_wrap(source, 16);
        assert!(formatted.text.contains('\u{2028}'));
        let display_len = formatted.text.encode_utf16().count();
        let source_range = formatted
            .source_map
            .source_range(NSRange::new(0, display_len));
        assert_eq!(source_range.length, source.encode_utf16().count());
    }
}
