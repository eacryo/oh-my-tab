//! 剪贴板文本分类与轻量词法高亮。
//! Clipboard text classification and lightweight lexical highlighting.
//!
//! 这里不构建 AST,只按字符扫描,因此代码片段不完整时仍能稳定显示。
//! This module does not build an AST; it scans characters so incomplete snippets remain safe.

use crate::config::CONFIG;
use crate::ffi::{hex_to_ns_color, make_nsstring, release_obj, CFRelease};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::NSRange;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

/// 剪贴板条目类型分类,供列表和详情浮窗共用。
/// Clipboard entry classification shared by the list and detail panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextKind {
    Plain,
    Url,
    Code,
}

const SYNTECT_THEME: &str = "InspiredGitHub";
const SYNTECT_CACHE_CAPACITY: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct SyntectCacheKey {
    content_hash: u64,
    content_len: usize,
    language: &'static str,
}

#[derive(Debug, Clone, Copy)]
struct SyntectSpan {
    start: usize,
    end: usize,
    foreground: [u8; 4],
}

#[derive(Debug, Clone)]
struct CachedSyntectHighlight {
    spans: Vec<SyntectSpan>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CodeDisplayCacheKey {
    content_hash: u64,
    content_len: usize,
    max_columns: usize,
    language: Option<&'static str>,
    use_syntect: bool,
}

#[derive(Debug, Clone, Copy)]
struct DisplayHighlightSpan {
    start: usize,
    end: usize,
    foreground: [u8; 4],
}

#[derive(Clone)]
pub(crate) struct PreparedCodeDisplay {
    pub(crate) text: String,
    pub(crate) source_map: DisplaySourceMap,
    spans: Vec<DisplayHighlightSpan>,
}

struct SyntectState {
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
}

static SYNTECT_STATE: OnceLock<Option<SyntectState>> = OnceLock::new();
static SYNTECT_CACHE: OnceLock<Mutex<HashMap<SyntectCacheKey, CachedSyntectHighlight>>> =
    OnceLock::new();
static CODE_DISPLAY_CACHE: OnceLock<Mutex<HashMap<CodeDisplayCacheKey, PreparedCodeDisplay>>> =
    OnceLock::new();

/// 保守地从剪贴板片段推断语言;没有足够证据时返回 None,交给现有通用高亮兜底。
/// Conservatively infer a language from a clipboard snippet; return None when uncertain so
/// the existing generic highlighter can remain the fallback.
pub(crate) fn detect_language(text: &str) -> Option<&'static str> {
    let trimmed = text.trim_start();
    if let Some(first) = trimmed.lines().next() {
        if let Some(token) = first
            .trim()
            .strip_prefix("```")
            .or_else(|| first.trim().strip_prefix("~~~"))
        {
            if let Some(language) = normalize_language_hint(token.trim()) {
                return Some(language);
            }
        }
        if first.starts_with("#!") {
            let lower = first.to_ascii_lowercase();
            if lower.contains("python") {
                return Some("py");
            }
            if lower.contains("ruby") {
                return Some("rb");
            }
            if lower.contains("node") || lower.contains("deno") {
                return Some("js");
            }
            if lower.contains("bash") || lower.contains("zsh") || lower.contains("fish") {
                return Some("sh");
            }
            if lower.contains("php") {
                return Some("php");
            }
        }
    }
    if looks_like_html(trimmed) {
        return Some("html");
    }
    if looks_like_json(trimmed) {
        return Some("json");
    }

    let lower = text.to_ascii_lowercase();
    // 只使用足够有辨识度的特征;低于阈值或得分并列时返回 None,交给通用兜底。
    // Use distinctive cues only; below the threshold or on a tie, return None for the
    // generic fallback instead of applying the wrong grammar.
    let candidates: &[(&str, &[&str])] = &[
        (
            "rs",
            &[
                "fn ", "let ", "impl ", "pub ", "use ", "match ", "::", "->", "trait ",
            ],
        ),
        (
            "py",
            &[
                "def ", "import ", "from ", "elif ", "__name__", "except ", "yield ", "self.",
            ],
        ),
        (
            "java",
            &[
                "package ",
                "import java.",
                "public ",
                "private ",
                "protected ",
                "public class",
                "private class",
                "protected class",
                "@override",
                "@test",
                "static ",
                "final ",
                "void ",
                "string ",
                "boolean ",
                "throws ",
                "return ",
                "system.out.",
                "implements ",
            ],
        ),
        (
            "ts",
            &[
                "interface ",
                "type ",
                ": string",
                ": number",
                " as const",
                "readonly ",
                "implements ",
            ],
        ),
        (
            "js",
            &[
                "const ",
                "let ",
                "function ",
                "=>",
                "console.",
                "require(",
                "export ",
                "import ",
            ],
        ),
        (
            "go",
            &["package ", "func ", ":=", "defer ", "chan ", "go func"],
        ),
        (
            "swift",
            &[
                "import foundation",
                "guard ",
                "func ",
                "let ",
                "var ",
                "struct ",
                "protocol ",
            ],
        ),
        (
            "c",
            &[
                "#include <stdio.h>",
                "#include <stdlib.h>",
                "printf(",
                "scanf(",
                "sizeof(",
                "typedef struct",
                "null",
            ],
        ),
        (
            "cpp",
            &[
                "#include <iostream>",
                "#include <vector>",
                "std::",
                "cout <<",
                "cin >>",
                "nullptr",
                "template<",
            ],
        ),
        (
            "cs",
            &[
                "using system",
                "namespace ",
                "console.",
                "async task",
                "string[] args",
                "get; set;",
            ],
        ),
        (
            "kt",
            &[
                "fun ",
                "val ",
                "data class",
                "when ",
                "println(",
                "companion object",
            ],
        ),
        (
            "dart",
            &[
                "import 'dart:",
                "void main(",
                "future<",
                "widget build(",
                "@override",
                "print(",
            ],
        ),
        (
            "ruby",
            &[
                "def ", "require ", "attr_", "do |", "puts ", "unless ", "end\n",
            ],
        ),
        (
            "php",
            &["<?php", "echo ", "namespace ", "$this->", "function ", "->"],
        ),
        (
            "sql",
            &[
                "select ",
                "insert into ",
                "update ",
                "delete from ",
                "create table ",
                "alter table ",
            ],
        ),
        (
            "css",
            &[
                "@media",
                "font-family",
                "background:",
                "display:",
                "!important",
            ],
        ),
        (
            "sh",
            &[
                "set -e", "#!/bin/", "$(", "echo ", "export ", "fi\n", "then\n",
            ],
        ),
    ];
    let mut best: Option<(&'static str, usize)> = None;
    let mut tied = false;
    for (language, cues) in candidates {
        let score = cues.iter().filter(|cue| lower.contains(**cue)).count();
        if score == 0 {
            continue;
        }
        match best {
            None => best = Some((language, score)),
            Some((_, best_score)) if score > best_score => {
                best = Some((language, score));
                tied = false;
            }
            Some((_, best_score)) if score == best_score => tied = true,
            _ => {}
        }
    }
    best.filter(|(_, score)| *score >= 2 && !tied)
        .map(|(language, _)| language)
}

fn normalize_language_hint(hint: &str) -> Option<&'static str> {
    let normalized = hint
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '+' && c != '#')
        .to_ascii_lowercase();
    match normalized.as_str() {
        "rust" | "rs" => Some("rs"),
        "python" | "py" => Some("py"),
        "javascript" | "js" => Some("js"),
        "typescript" | "ts" => Some("ts"),
        "java" => Some("java"),
        "c" => Some("c"),
        "c++" | "cpp" => Some("cpp"),
        "c#" | "cs" | "csharp" => Some("cs"),
        "go" | "golang" => Some("go"),
        "swift" => Some("swift"),
        "kotlin" | "kt" => Some("kt"),
        "dart" => Some("dart"),
        "shell" | "bash" | "zsh" | "sh" => Some("sh"),
        "html" | "xml" => Some("html"),
        "css" => Some("css"),
        "json" => Some("json"),
        "sql" => Some("sql"),
        "ruby" | "rb" => Some("rb"),
        "php" => Some("php"),
        "yaml" | "yml" => Some("yml"),
        "markdown" | "md" => Some("md"),
        _ => None,
    }
}

fn looks_like_json(text: &str) -> bool {
    let starts = text.starts_with('{') || text.starts_with('[');
    starts && (text.contains("\":") || text.contains("\": "))
}

/// 根据配置判断是否允许对该片段运行 syntect;0 表示主动关闭高亮。
/// Decide whether syntect may run for this snippet; zero explicitly disables highlighting.
fn should_use_syntect_with_limits(text: &str, max_bytes: usize, max_lines: usize) -> bool {
    max_bytes > 0
        && max_lines > 0
        && text.len() <= max_bytes
        && text.split('\n').count() <= max_lines
}

pub(crate) fn should_use_syntect(text: &str) -> bool {
    let (max_bytes, max_lines) = CONFIG
        .read()
        .map(|cfg| {
            (
                cfg.clipboard.max_highlight_bytes as usize,
                cfg.clipboard.max_highlight_lines as usize,
            )
        })
        .unwrap_or((64 * 1024, 1000));
    should_use_syntect_with_limits(text, max_bytes, max_lines)
}

fn syntect_fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// 在剪贴板功能启动时后台预热语法集,避免首次查看 HTML 阻塞主线程。
/// Warm the syntax sets in the background when clipboard support starts, so the first HTML
/// detail open does not block the main thread.
pub(crate) fn warm_up_syntect() {
    std::thread::spawn(|| {
        let _ = syntect_state();
    });
}

fn syntect_state() -> Option<&'static SyntectState> {
    SYNTECT_STATE
        .get_or_init(|| {
            // 语法集和主题集只在进程内加载一次,后续详情打开直接复用。
            // Load syntax and theme sets once per process; later detail opens reuse them.
            Some(SyntectState {
                syntax_set: SyntaxSet::load_defaults_newlines(),
                theme_set: ThemeSet::load_defaults(),
            })
        })
        .as_ref()
}

fn cached_syntect_highlight(text: &str, language: &'static str) -> Option<Vec<SyntectSpan>> {
    if !should_use_syntect(text) {
        return None;
    }
    let key = SyntectCacheKey {
        content_hash: syntect_fnv1a64(text.as_bytes()),
        content_len: text.len(),
        language,
    };
    let cache = SYNTECT_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(cached) = cache.lock().ok()?.get(&key) {
        return Some(cached.spans.clone());
    }

    let state = syntect_state()?;
    let syntax = state.syntax_set.find_syntax_by_token(language)?;
    let theme = state
        .theme_set
        .themes
        .get(SYNTECT_THEME)
        .or_else(|| state.theme_set.themes.values().next())?;
    let mut highlighter = HighlightLines::new(syntax, theme);
    let mut source_offset = 0;
    let mut spans = Vec::new();
    for line in LinesWithEndings::from(text) {
        let ranges = highlighter.highlight_line(line, &state.syntax_set).ok()?;
        let mut line_offset = 0;
        for (style, fragment) in ranges {
            let end = line_offset + fragment.len();
            if end > line_offset {
                spans.push(SyntectSpan {
                    start: text[..source_offset + line_offset].encode_utf16().count(),
                    end: text[..source_offset + end].encode_utf16().count(),
                    foreground: [
                        style.foreground.r,
                        style.foreground.g,
                        style.foreground.b,
                        style.foreground.a,
                    ],
                });
            }
            line_offset = end;
        }
        source_offset += line.len();
    }

    let result = CachedSyntectHighlight {
        spans: spans.clone(),
    };
    if let Ok(mut guard) = cache.lock() {
        if guard.len() >= SYNTECT_CACHE_CAPACITY {
            if let Some(old_key) = guard.keys().next().copied() {
                guard.remove(&old_key);
            }
        }
        guard.insert(key, result);
    }
    Some(spans)
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
    // URL:含 scheme 或 www. 开头(整段就是一条链接)。
    // URL: contains a scheme or starts with www. (the whole text is one link).
    if t.contains("://") || t.starts_with("www.") {
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

/// 轻量判断文本是否像 HTML,避免把 Java 泛型 `<T>` 等普通代码误判成标签。
/// Cheap HTML detection that avoids mistaking ordinary code such as Java generics `<T>` for tags.
fn looks_like_html(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut search_from = 0;
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
        // 单字母标签只保留 HTML 中常见的真实标签,过滤 `<T>`、`<K>` 等泛型。
        // Keep only common real one-letter HTML tags, filtering generics such as `<T>` and `<K>`.
        let valid_name = name.len() >= 2
            || (name.len() == 1
                && matches!(
                    name[0].to_ascii_lowercase(),
                    b'a' | b'b' | b'i' | b'p' | b'q' | b's' | b'u'
                ));
        if valid_name {
            return true;
        }
        search_from = open + 1;
    }
    false
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

    /// 将原文高亮范围反向投影到插入了视觉换行的显示文本。
    /// Project a source highlight range back onto display text containing visual breaks.
    fn display_ranges_for_source_range(&self, source_range: NSRange) -> Vec<NSRange> {
        let source_start = source_range.location;
        let source_end = source_start.saturating_add(source_range.length);
        let mut ranges = Vec::new();
        let mut run_start: Option<usize> = None;
        for index in 0..self.boundaries.len().saturating_sub(1) {
            let start = self.boundaries[index];
            let end = self.boundaries[index + 1];
            let included = (start < source_end && end > source_start)
                || (start == end && start >= source_start && start <= source_end);
            if included {
                run_start.get_or_insert(index);
            } else if let Some(start_index) = run_start.take() {
                ranges.push(NSRange::new(start_index, index - start_index));
            }
        }
        if let Some(start_index) = run_start {
            ranges.push(NSRange::new(
                start_index,
                self.boundaries.len().saturating_sub(1) - start_index,
            ));
        }
        ranges
    }
}

fn rgba_from_hex(color: u32) -> [u8; 4] {
    [
        (color >> 24) as u8,
        (color >> 16) as u8,
        (color >> 8) as u8,
        color as u8,
    ]
}

fn merge_display_spans(mut spans: Vec<DisplayHighlightSpan>) -> Vec<DisplayHighlightSpan> {
    spans.sort_unstable_by_key(|span| (span.start, span.end));
    let mut merged: Vec<DisplayHighlightSpan> = Vec::with_capacity(spans.len());
    for span in spans {
        if span.start >= span.end {
            continue;
        }
        if let Some(last) = merged.last_mut() {
            if last.end == span.start && last.foreground == span.foreground {
                last.end = span.end;
                continue;
            }
        }
        merged.push(span);
    }
    merged
}

/// 一次准备详情显示文本、原文映射和显示高亮范围,尺寸计算与视图创建共享这份缓存。
/// Prepare display text, source mapping, and display highlight ranges once; size calculation
/// and view creation share this cached result.
pub(crate) fn prepare_code_display(source: &str, max_columns: usize) -> PreparedCodeDisplay {
    let use_syntect = should_use_syntect(source);
    let language = detect_language(source);
    let key = CodeDisplayCacheKey {
        content_hash: syntect_fnv1a64(source.as_bytes()),
        content_len: source.len(),
        max_columns,
        language,
        use_syntect,
    };
    let cache = CODE_DISPLAY_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(cached) = cache.lock().ok().and_then(|guard| guard.get(&key).cloned()) {
        return cached;
    }

    let formatted = format_code_for_display(source, max_columns);
    let mut spans = Vec::new();
    if use_syntect {
        if let Some(language) = language {
            if let Some(source_spans) = cached_syntect_highlight(source, language) {
                for span in source_spans {
                    let source_range =
                        NSRange::new(span.start, span.end.saturating_sub(span.start));
                    for range in formatted
                        .source_map
                        .display_ranges_for_source_range(source_range)
                    {
                        spans.push(DisplayHighlightSpan {
                            start: range.location,
                            end: range.location.saturating_add(range.length),
                            foreground: span.foreground,
                        });
                    }
                }
            }
        }
    }
    if use_syntect && spans.is_empty() {
        for span in highlight_spans(&formatted.text, TextKind::Code) {
            let range = utf16_range(&formatted.text, span.start, span.end);
            spans.push(DisplayHighlightSpan {
                start: range.location,
                end: range.location.saturating_add(range.length),
                foreground: rgba_from_hex(highlight_color(span.kind)),
            });
        }
    }
    let prepared = PreparedCodeDisplay {
        text: formatted.text,
        source_map: formatted.source_map,
        spans: merge_display_spans(spans),
    };
    if let Ok(mut guard) = cache.lock() {
        if guard.len() >= SYNTECT_CACHE_CAPACITY {
            if let Some(old_key) = guard.keys().next().copied() {
                guard.remove(&old_key);
            }
        }
        guard.insert(key, prepared.clone());
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
    // 只改变显示文本;中点和普通 ASCII 空格都是一个 UTF-16 单元,因此原文映射无需改变。
    // Change only the display text; a middle dot and an ASCII space are both one UTF-16 unit,
    // so the source mapping remains unchanged.
    if display.contains(' ') {
        display = display.replace(' ', "·");
    }
    FormattedCode {
        text: display,
        source_map: DisplaySourceMap {
            source: source.to_owned(),
            boundaries,
        },
    }
}

/// 给代码中的可见空格设置淡色,缩进空格比普通空格稍明显。
/// Tint visible code spaces; indentation spaces are slightly stronger than ordinary spaces.
pub(crate) unsafe fn apply_visible_space_markers(storage: *mut AnyObject, text: &str) {
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

unsafe fn apply_lexical_highlights(storage: *mut AnyObject, text: &str, spans: Vec<HighlightSpan>) {
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

unsafe fn apply_syntect_highlights(
    storage: *mut AnyObject,
    spans: &[SyntectSpan],
    display_map: Option<&DisplaySourceMap>,
) {
    if spans.is_empty() {
        return;
    }
    let color_key = make_nsstring("NSColor");
    for span in spans {
        let color: *mut AnyObject = msg_send![
            class!(NSColor),
            colorWithSRGBRed: f64::from(span.foreground[0]) / 255.0,
            green: f64::from(span.foreground[1]) / 255.0,
            blue: f64::from(span.foreground[2]) / 255.0,
            alpha: f64::from(span.foreground[3]) / 255.0
        ];
        let source_range = NSRange::new(span.start, span.end.saturating_sub(span.start));
        let ranges = display_map
            .map(|map| map.display_ranges_for_source_range(source_range))
            .unwrap_or_else(|| vec![source_range]);
        for range in ranges {
            if range.length == 0 {
                continue;
            }
            let _: () = msg_send![
                storage,
                addAttribute: color_key,
                value: color,
                range: range
            ];
        }
    }
    CFRelease(color_key as *const c_void);
}

/// 直接应用已缓存的显示范围,避免每次打开详情重新扫描原文映射。
/// Apply cached display ranges directly, avoiding a fresh source-map scan on every detail open.
pub(crate) unsafe fn apply_prepared_code_highlights(
    storage: *mut AnyObject,
    prepared: &PreparedCodeDisplay,
) {
    if prepared.spans.is_empty() {
        return;
    }
    let color_key = make_nsstring("NSColor");
    for span in &prepared.spans {
        let color: *mut AnyObject = msg_send![
            class!(NSColor),
            colorWithSRGBRed: f64::from(span.foreground[0]) / 255.0,
            green: f64::from(span.foreground[1]) / 255.0,
            blue: f64::from(span.foreground[2]) / 255.0,
            alpha: f64::from(span.foreground[3]) / 255.0
        ];
        let _: () = msg_send![
            storage,
            addAttribute: color_key,
            value: color,
            range: NSRange::new(span.start, span.end.saturating_sub(span.start))
        ];
    }
    CFRelease(color_key as *const c_void);
}

/// 使用缓存的 syntect 结果高亮代码;语言不确定时回退到原有轻量扫描器。
/// Apply cached syntect results to code; fall back to the existing lightweight scanner when
/// the language cannot be identified confidently.
pub(crate) unsafe fn apply_code_highlights(
    storage: *mut AnyObject,
    source_text: &str,
    display_text: &str,
    display_map: Option<&DisplaySourceMap>,
) {
    if !should_use_syntect(source_text) {
        return;
    }
    if let Some(language) = detect_language(source_text) {
        if let Some(spans) = cached_syntect_highlight(source_text, language) {
            apply_syntect_highlights(storage, &spans, display_map);
            return;
        }
    }
    apply_lexical_highlights(
        storage,
        display_text,
        highlight_spans(display_text, TextKind::Code),
    );
}

pub(crate) unsafe fn apply_highlights(storage: *mut AnyObject, text: &str, kind: TextKind) {
    if kind == TextKind::Code {
        apply_code_highlights(storage, text, text, None);
    } else {
        apply_lexical_highlights(storage, text, highlight_spans(text, kind));
    }
}

#[cfg(test)]
mod tests {
    use super::{cached_syntect_highlight, detect_language, should_use_syntect_with_limits};

    #[test]
    fn highlight_limits_skip_large_snippets() {
        let source = "<div>\ncontent\n</div>";
        assert!(should_use_syntect_with_limits(source, 64, 3));
        assert!(!should_use_syntect_with_limits(source, 8 * 1024, 2));
        assert!(!should_use_syntect_with_limits(source, 0, 100));
        assert!(!should_use_syntect_with_limits(source, 1024, 0));
    }

    #[test]
    fn syntect_highlights_and_reuses_cached_code() {
        let source = "fn main() {\n    let answer = 42;\n}";
        let language = detect_language(source).expect("Rust snippet should be detected");
        let first = cached_syntect_highlight(source, language).expect("syntect should load");
        let second = cached_syntect_highlight(source, language).expect("cache should hit");
        assert!(!first.is_empty());
        assert_eq!(first.len(), second.len());
        assert_eq!(first[0].start, second[0].start);
        assert_eq!(first[0].foreground, second[0].foreground);
    }
}
