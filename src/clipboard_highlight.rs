//! 剪贴板文本分类与轻量词法高亮。
//! Clipboard text classification and lightweight lexical highlighting.
//!
//! 这里不构建 AST,只按字符扫描,因此代码片段不完整时仍能稳定显示。
//! This module does not build an AST; it scans characters so incomplete snippets remain safe.

use crate::ffi::{hex_to_ns_color, make_nsstring, release_obj, CFRelease};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::NSRange;
use std::ffi::c_void;

/// 剪贴板条目类型分类,供列表和详情浮窗共用。
/// Clipboard entry classification shared by the list and detail panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextKind {
    Plain,
    Url,
    Code,
}

pub(crate) fn classify_text(text: &str) -> TextKind {
    let t = text.trim();
    if t.is_empty() {
        return TextKind::Plain;
    }
    // URL:含 scheme 或 www. 开头(整段就是一条链接)。
    // URL: contains a scheme or starts with www. (the whole text is one link).
    if t.contains("://") || t.starts_with("www.") {
        return TextKind::Url;
    }
    // HTML 单行片段也应进入代码样式,例如 `<div>hello</div>`。
    // Single-line HTML snippets should also use code styling, e.g. `<div>hello</div>`.
    if looks_like_html(t) {
        return TextKind::Code;
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

/// 轻量判断文本是否像 HTML,避免把普通比较表达式误判成代码。
/// Cheap HTML detection that avoids classifying ordinary comparison expressions as code.
fn looks_like_html(text: &str) -> bool {
    let bytes = text.as_bytes();
    let Some(open) = bytes.iter().position(|&b| b == b'<') else {
        return false;
    };
    let Some(&next) = bytes.get(open + 1) else {
        return false;
    };
    next.is_ascii_alphabetic() || matches!(next, b'/' | b'!' | b'?')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HighlightKind {
    Link,
    Keyword,
    String,
    Comment,
    Number,
    Tag,
    Attribute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HighlightSpan {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) kind: HighlightKind,
}

fn is_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn is_keyword(word: &str) -> bool {
    matches!(
        word,
        "as" | "async"
            | "await"
            | "bool"
            | "break"
            | "class"
            | "const"
            | "continue"
            | "def"
            | "else"
            | "enum"
            | "export"
            | "false"
            | "fn"
            | "for"
            | "from"
            | "func"
            | "if"
            | "impl"
            | "import"
            | "in"
            | "interface"
            | "let"
            | "match"
            | "mod"
            | "mut"
            | "new"
            | "None"
            | "null"
            | "package"
            | "pub"
            | "return"
            | "self"
            | "static"
            | "struct"
            | "trait"
            | "true"
            | "type"
            | "undefined"
            | "use"
            | "var"
            | "where"
            | "while"
            | "with"
            | "yield"
    )
}

fn collect_html_highlights(text: &str, spans: &mut Vec<HighlightSpan>) {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"<!--") {
            let start = i;
            i += 4;
            while i < bytes.len() && !bytes[i..].starts_with(b"-->") {
                i += 1;
            }
            i = (i + 3).min(bytes.len());
            spans.push(HighlightSpan {
                start,
                end: i,
                kind: HighlightKind::Comment,
            });
            continue;
        }
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }

        i += 1;
        if bytes.get(i) == Some(&b'/') {
            i += 1;
        }
        while bytes.get(i).is_some_and(|b| b.is_ascii_whitespace()) {
            i += 1;
        }
        let name_start = i;
        while bytes
            .get(i)
            .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'-' | b'_'))
        {
            i += 1;
        }
        if i > name_start {
            spans.push(HighlightSpan {
                start: name_start,
                end: i,
                kind: HighlightKind::Tag,
            });
        }

        while i < bytes.len() && bytes[i] != b'>' {
            if matches!(bytes[i], b'\'' | b'"') {
                let quote = bytes[i];
                let start = i;
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i = (i + 2).min(bytes.len());
                    } else if bytes[i] == quote {
                        i += 1;
                        break;
                    } else {
                        i += 1;
                    }
                }
                spans.push(HighlightSpan {
                    start,
                    end: i,
                    kind: HighlightKind::String,
                });
            } else if is_identifier_start(bytes[i]) {
                let start = i;
                i += 1;
                while i < bytes.len() && is_identifier_byte(bytes[i]) {
                    i += 1;
                }
                spans.push(HighlightSpan {
                    start,
                    end: i,
                    kind: HighlightKind::Attribute,
                });
            } else {
                i += 1;
            }
        }
        if i < bytes.len() {
            i += 1;
        }
    }
}

fn collect_generic_highlights(text: &str, spans: &mut Vec<HighlightSpan>) {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let line_start = i == 0 || bytes[i - 1] == b'\n';
        if bytes[i..].starts_with(b"//") || bytes[i..].starts_with(b"/*") {
            let start = i;
            if bytes[i..].starts_with(b"//") {
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            } else {
                i += 2;
                while i < bytes.len() && !bytes[i..].starts_with(b"*/") {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
            }
            spans.push(HighlightSpan {
                start,
                end: i,
                kind: HighlightKind::Comment,
            });
            continue;
        }
        if bytes[i] == b'#' && (line_start || bytes[i - 1].is_ascii_whitespace()) {
            let start = i;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            spans.push(HighlightSpan {
                start,
                end: i,
                kind: HighlightKind::Comment,
            });
            continue;
        }
        if matches!(bytes[i], b'\'' | b'"' | b'`') {
            let quote = bytes[i];
            let start = i;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i = (i + 2).min(bytes.len());
                } else if bytes[i] == quote {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            spans.push(HighlightSpan {
                start,
                end: i,
                kind: HighlightKind::String,
            });
            continue;
        }
        if bytes[i].is_ascii_digit() && (i == 0 || !is_identifier_byte(bytes[i - 1])) {
            let start = i;
            i += 1;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'.' | b'_'))
            {
                i += 1;
            }
            spans.push(HighlightSpan {
                start,
                end: i,
                kind: HighlightKind::Number,
            });
            continue;
        }
        if is_identifier_start(bytes[i]) {
            let start = i;
            i += 1;
            while i < bytes.len() && is_identifier_byte(bytes[i]) {
                i += 1;
            }
            if let Some(word) = text.get(start..i) {
                if is_keyword(word) {
                    spans.push(HighlightSpan {
                        start,
                        end: i,
                        kind: HighlightKind::Keyword,
                    });
                }
            }
            continue;
        }
        i += 1;
    }
}

pub(crate) fn highlight_spans(text: &str, kind: TextKind) -> Vec<HighlightSpan> {
    match kind {
        TextKind::Plain => Vec::new(),
        TextKind::Url => vec![HighlightSpan {
            start: 0,
            end: text.len(),
            kind: HighlightKind::Link,
        }],
        TextKind::Code => {
            let mut spans = Vec::new();
            if looks_like_html(text) {
                collect_html_highlights(text, &mut spans);
            } else {
                collect_generic_highlights(text, &mut spans);
            }
            spans
        }
    }
}

fn utf16_range(text: &str, start: usize, end: usize) -> NSRange {
    let location = text[..start].encode_utf16().count();
    let length = text[start..end].encode_utf16().count();
    NSRange::new(location, length)
}

fn highlight_color(kind: HighlightKind) -> u32 {
    match kind {
        HighlightKind::Link => 0x205BA6B8,
        HighlightKind::Keyword => 0x7C3AEDCC,
        HighlightKind::String => 0x047857CC,
        HighlightKind::Comment => 0x6B7280AA,
        HighlightKind::Number => 0xB45309CC,
        HighlightKind::Tag => 0x9D174DCC,
        HighlightKind::Attribute => 0x1D4ED8CC,
    }
}

/// 给 attributed string 批量添加语法颜色;单次扫描即可处理不完整片段。
/// Apply syntax colors to an attributed string in batches; one scan handles incomplete snippets.
/// 详情浮窗中的格式化代码及其原文偏移映射。
/// Formatted detail code together with a mapping back to the original source offsets.
pub(crate) struct FormattedCode {
    pub(crate) text: String,
    pub(crate) source_map: DisplaySourceMap,
}

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

fn visual_width(text: &str) -> usize {
    let mut width = 0;
    for ch in text.chars() {
        width += if ch == '\t' { 4 - (width % 4) } else { 1 };
    }
    width
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

fn safe_breaks(source: &str, start: usize, end: usize) -> Vec<usize> {
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
            points.push(byte);
            continue;
        }
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
        if ch == '.' {
            // Break before a method-chain dot, never in the method name itself.
            points.push(byte);
            byte += 1;
            continue;
        }
        let next = source[byte + ch.len_utf8()..end].chars().next();
        if matches!(ch, ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}')
            || matches!(ch, '=' | '+' | '-' | '*' | '/' | '&' | '|' | '<' | '>')
        {
            byte += ch.len_utf8();
            if next == Some('=') || (matches!(ch, '&' | '|') && next == Some(ch)) {
                byte += 1;
            }
            points.push(byte);
            continue;
        }
        byte += ch.len_utf8();
    }
    points.sort_unstable();
    points.dedup();
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
    out: &mut String,
    boundaries: &mut Vec<usize>,
) {
    if line_start == line_end {
        return;
    }
    let (content_start, indent_columns) = leading_indent(source, line_start, line_end);
    if visual_width(&source[line_start..line_end]) <= max_columns {
        append_mapped_source(out, boundaries, source, offsets, line_start, line_end);
        return;
    }

    let breaks = safe_breaks(source, content_start, line_end);
    let mut chunk_start = line_start;
    let mut first_chunk = true;
    while chunk_start < line_end {
        let prefix_columns = if first_chunk { 0 } else { indent_columns + 4 };
        let available = max_columns.saturating_sub(prefix_columns).max(12);
        if visual_width(&source[chunk_start..line_end]) <= available {
            if !first_chunk {
                append_mapped_insert(
                    out,
                    boundaries,
                    &" ".repeat(indent_columns + 4),
                    offsets[chunk_start],
                );
            }
            append_mapped_source(out, boundaries, source, offsets, chunk_start, line_end);
            break;
        }

        let candidate = breaks
            .iter()
            .copied()
            .filter(|&point| point > chunk_start)
            .take_while(|&point| {
                visual_width(&source[chunk_start..trim_trailing_space(source, chunk_start, point)])
                    <= available
            })
            .last();
        let Some(break_point) =
            candidate.or_else(|| breaks.iter().copied().find(|&point| point > chunk_start))
        else {
            if !first_chunk {
                append_mapped_insert(
                    out,
                    boundaries,
                    &" ".repeat(indent_columns + 4),
                    offsets[chunk_start],
                );
            }
            append_mapped_source(out, boundaries, source, offsets, chunk_start, line_end);
            break;
        };

        let chunk_end = trim_trailing_space(source, chunk_start, break_point);
        if !first_chunk {
            append_mapped_insert(
                out,
                boundaries,
                &" ".repeat(indent_columns + 4),
                offsets[chunk_start],
            );
        }
        append_mapped_source(out, boundaries, source, offsets, chunk_start, chunk_end);
        if let Some(last) = boundaries.last_mut() {
            *last = offsets[break_point];
        }
        append_mapped_insert(out, boundaries, "\n", offsets[break_point]);
        chunk_start = skip_leading_space(source, break_point, line_end);
        first_chunk = false;
    }
}

/// 代码详情的显示格式化:只插入视觉换行和悬挂缩进,原文通过偏移映射保留。
/// Format code for the detail view by inserting only visual breaks and hanging indents;
/// the source remains available through the offset map.
pub(crate) fn format_code_for_display(source: &str, max_columns: usize) -> FormattedCode {
    let offsets = source_utf16_offsets(source);
    let mut display = String::new();
    let mut boundaries = vec![0];
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
            &mut display,
            &mut boundaries,
        );
        if line_end < source.len() {
            append_mapped_source(
                &mut display,
                &mut boundaries,
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
    FormattedCode {
        text: display,
        source_map: DisplaySourceMap {
            source: source.to_owned(),
            boundaries,
        },
    }
}

/// 给代码的每个显示段落设置悬挂缩进,即使 NSTextView 仍需二次换行也不会顶到最左侧。
/// Set hanging indents on every displayed code paragraph so any fallback NSTextView wrap
/// also stays indented instead of jumping to the far left.
pub(crate) unsafe fn apply_code_paragraph_styles(storage: *mut AnyObject, text: &str) {
    let style_key = make_nsstring("NSParagraphStyle");
    let mut location = 0;
    for line in text.split_inclusive('\n') {
        let content = line.strip_suffix('\n').unwrap_or(line);
        let (_, indent_columns) = leading_indent(content, 0, content.len());
        let continuation_columns = indent_columns.max(4);
        let style: *mut AnyObject = msg_send![class!(NSMutableParagraphStyle), alloc];
        let style: *mut AnyObject = msg_send![style, init];
        let _: () = msg_send![
            style,
            setHeadIndent: continuation_columns as f64 * 8.4
        ];
        let _: () = msg_send![style, setFirstLineHeadIndent: 0.0f64];
        let _: () = msg_send![style, setLineBreakMode: 0isize]; // NSLineBreakByWordWrapping
        let length = line.encode_utf16().count();
        if length > 0 {
            let _: () = msg_send![
                storage,
                addAttribute: style_key,
                value: style,
                range: NSRange::new(location, length)
            ];
        }
        release_obj(style);
        location += length;
    }
    CFRelease(style_key as *const c_void);
}

pub(crate) unsafe fn apply_highlights(storage: *mut AnyObject, text: &str, kind: TextKind) {
    let spans = highlight_spans(text, kind);
    if spans.is_empty() {
        return;
    }
    let color_key = make_nsstring("NSColor");
    for span in spans {
        let color = hex_to_ns_color(highlight_color(span.kind));
        let _: () = msg_send![
            storage,
            addAttribute: color_key,
            value: color,
            range: utf16_range(text, span.start, span.end)
        ];
    }
    CFRelease(color_key as *const c_void);
}
