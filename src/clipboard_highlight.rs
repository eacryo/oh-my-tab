//! 剪贴板文本分类与轻量词法高亮。
//! Clipboard text classification and lightweight lexical highlighting.
//!
//! 这里不构建 AST,只按字符扫描,因此代码片段不完整时仍能稳定显示。
//! This module does not build an AST; it scans characters so incomplete snippets remain safe.

use crate::ffi::{hex_to_ns_color, make_nsstring, CFRelease};
use objc2::msg_send;
use objc2::runtime::AnyObject;
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
