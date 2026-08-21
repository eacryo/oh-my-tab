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
use std::sync::{Arc, Mutex, OnceLock};
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

#[derive(Debug, Clone, Copy)]
struct SourceHighlightAnalysis {
    language: Option<&'static str>,
    use_syntect: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CodeDisplayCacheKey {
    content_hash: u64,
    content_len: usize,
    max_columns: usize,
    use_syntect: bool,
    soft_wrap: bool,
}

#[derive(Debug, Clone, Copy)]
struct DisplayHighlightSpan {
    start: usize,
    end: usize,
    foreground: [u8; 4],
}

pub(crate) struct PreparedCodeDisplay {
    pub(crate) text: String,
    spans: Vec<DisplayHighlightSpan>,
    pub(crate) source_map: Option<Arc<DisplaySourceMap>>,
}

struct SyntectState {
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
}

static SYNTECT_STATE: OnceLock<Option<SyntectState>> = OnceLock::new();
// 两级缓存都共享不可变结果。命中时只增加 Arc 引用计数,不再复制长文本和大量 span。
// Both cache levels share immutable results. A hit only increments an Arc reference count
// instead of copying long text and large span arrays.
static SYNTECT_CACHE: OnceLock<Mutex<HashMap<SyntectCacheKey, Arc<[SyntectSpan]>>>> =
    OnceLock::new();
static CODE_DISPLAY_CACHE: OnceLock<Mutex<HashMap<CodeDisplayCacheKey, Arc<PreparedCodeDisplay>>>> =
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
    if max_bytes == 0 || max_lines == 0 || text.len() > max_bytes {
        return false;
    }
    // 达到行数上限后立即退出;无需像 split().count() 一样继续扫描超长输入的剩余部分。
    // Exit as soon as the line limit is exceeded instead of scanning the rest of a long input
    // like split().count() would.
    let mut lines = 1usize;
    for byte in text.bytes() {
        if byte == b'\n' {
            lines += 1;
            if lines > max_lines {
                return false;
            }
        }
    }
    true
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

/// 一次性计算高亮路径共用的限制和语言元数据。之前 display cache 和 syntect cache
/// 分别执行这些检查;内容哈希由调用方同样只计算一次后传入两级缓存。
/// Compute limit and language metadata shared by the complete highlighting path once. Previously
/// the display and syntect caches repeated these checks; callers likewise hash content once and
/// pass that hash through both cache levels.
fn analyze_source_with_decision(source: &str, use_syntect: bool) -> SourceHighlightAnalysis {
    SourceHighlightAnalysis {
        // 高亮被配置/尺寸限制关闭时不做昂贵且不会被使用的语言扫描。
        // Skip the expensive, unused language scan when highlighting is disabled by limits.
        language: if use_syntect {
            detect_language(source)
        } else {
            None
        },
        use_syntect,
    }
}

fn analyze_source_for_highlighting(source: &str) -> SourceHighlightAnalysis {
    analyze_source_with_decision(source, should_use_syntect(source))
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

fn cached_syntect_highlight(
    text: &str,
    content_hash: u64,
    analysis: SourceHighlightAnalysis,
) -> Option<Arc<[SyntectSpan]>> {
    if !analysis.use_syntect {
        return None;
    }
    let language = analysis.language?;
    let key = SyntectCacheKey {
        content_hash,
        content_len: text.len(),
        language,
    };
    let cache = SYNTECT_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(cached) = cache.lock().ok().and_then(|guard| guard.get(&key).cloned()) {
        return Some(cached);
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
    // 预先建立 byte → UTF-16 边界表。以前每个 syntect 片段都从头 encode_utf16(),
    // HTML 的大量片段会反复扫描同一前缀而退化为 O(n²)。
    // Build byte-to-UTF-16 boundaries once. Previously every syntect fragment encoded a prefix
    // from the beginning, which rescanned HTML's many fragments into O(n²).
    let utf16_offsets = source_utf16_offsets(text);
    for line in LinesWithEndings::from(text) {
        let ranges = highlighter.highlight_line(line, &state.syntax_set).ok()?;
        let mut line_offset = 0;
        for (style, fragment) in ranges {
            let end = line_offset + fragment.len();
            if end > line_offset {
                spans.push(SyntectSpan {
                    start: utf16_offsets[source_offset + line_offset],
                    end: utf16_offsets[source_offset + end],
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

    let spans: Arc<[SyntectSpan]> = spans.into();
    if let Ok(mut guard) = cache.lock() {
        if guard.len() >= SYNTECT_CACHE_CAPACITY {
            if let Some(old_key) = guard.keys().next().copied() {
                guard.remove(&old_key);
            }
        }
        guard.insert(key, Arc::clone(&spans));
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
        // boundaries 按原文 UTF-16 偏移单调递增;插入的换行/缩进只会重复同一偏移。
        // 因此一个原文区间在显示文本中仍是连续区间,可以二分查找边界。以前每个
        // syntect span 都全扫 boundaries,使片段多的 HTML 再次退化为 O(n²)。
        // Boundaries are monotonic source UTF-16 offsets; inserted wraps/indents only repeat an
        // offset. A source interval is therefore contiguous in display text and can use binary
        // boundary searches. The former full scan per syntect span was another O(n²) HTML path.
        let first = self
            .boundaries
            .partition_point(|&boundary| boundary < source_start);
        let after_last = self
            .boundaries
            .partition_point(|&boundary| boundary <= source_end);
        let end = after_last
            .saturating_sub(1)
            .min(self.boundaries.len().saturating_sub(1));
        if first >= end {
            Vec::new()
        } else {
            vec![NSRange::new(first, end - first)]
        }
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

/// 一次准备详情显示文本、原文映射和显示高亮范围;缓存和调用方通过 Arc 共享不可变结果。
/// Prepare display text, source mapping, and display highlight ranges once; the cache and callers
/// share the immutable result through Arc.
pub(crate) fn prepare_code_display(source: &str, max_columns: usize) -> Arc<PreparedCodeDisplay> {
    let use_syntect = should_use_syntect(source);
    let content_hash = syntect_fnv1a64(source.as_bytes());
    let key = CodeDisplayCacheKey {
        content_hash,
        content_len: source.len(),
        max_columns,
        use_syntect,
        soft_wrap: false,
    };
    let cache = CODE_DISPLAY_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(cached) = cache.lock().ok().and_then(|guard| guard.get(&key).cloned()) {
        return cached;
    }

    // 语言由内容唯一决定,无需进入 display cache key。只在 cache miss 后识别,
    // 避免每次重开同一详情都对全文执行语言特征扫描。
    // Language is deterministic from content and need not be part of the display-cache key.
    // Detect it only after a miss so reopening the same detail avoids a full language scan.
    let analysis = analyze_source_with_decision(source, use_syntect);
    let formatted = format_code_for_display(source, max_columns);
    let prepared = Arc::new(build_prepared_code(
        source,
        formatted,
        content_hash,
        analysis,
        false,
    ));
    if let Ok(mut guard) = cache.lock() {
        if guard.len() >= SYNTECT_CACHE_CAPACITY {
            if let Some(old_key) = guard.keys().next().copied() {
                guard.remove(&old_key);
            }
        }
        guard.insert(key, Arc::clone(&prepared));
    }
    prepared
}

/// 构建显示字符串的语法高亮,同时保留显示到原文的偏移映射。
/// Build syntax highlights for a display string while preserving its source mapping.
fn build_prepared_code(
    source: &str,
    formatted: FormattedCode,
    content_hash: u64,
    analysis: SourceHighlightAnalysis,
    retain_source_map: bool,
) -> PreparedCodeDisplay {
    let mut spans = Vec::new();
    if let Some(source_spans) = cached_syntect_highlight(source, content_hash, analysis) {
        for span in source_spans.iter() {
            let source_range = NSRange::new(span.start, span.end.saturating_sub(span.start));
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
    if analysis.use_syntect && spans.is_empty() {
        for span in highlight_spans(&formatted.text, TextKind::Code) {
            let range = utf16_range(&formatted.text, span.start, span.end);
            spans.push(DisplayHighlightSpan {
                start: range.location,
                end: range.location.saturating_add(range.length),
                foreground: rgba_from_hex(highlight_color(span.kind)),
            });
        }
    }
    PreparedCodeDisplay {
        text: formatted.text,
        spans: merge_display_spans(spans),
        // 列表预览只需映射高亮范围,构建完成后立即释放;只有详情复制需要长期保留。
        // Row previews need the map only while projecting highlights and release it afterward;
        // only detail copying retains it.
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
    let use_syntect = should_use_syntect(source);
    let content_hash = syntect_fnv1a64(source.as_bytes());
    let key = CodeDisplayCacheKey {
        content_hash,
        content_len: source.len(),
        max_columns,
        use_syntect,
        soft_wrap: true,
    };
    let cache = CODE_DISPLAY_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(cached) = cache.lock().ok().and_then(|guard| guard.get(&key).cloned()) {
        return cached;
    }

    // 自定义软换行插入 U+2028,因此保留共享 source map 供高亮和复制选区使用。
    // 语言识别推迟到 miss 后,cache hit 只做限制检查、哈希和 Arc clone。
    // Custom soft wrapping inserts U+2028, so retain a shared source map for highlighting and
    // copied selections. Language detection is deferred until after a miss; a hit only checks
    // limits, hashes, and clones the Arc.
    let analysis = analyze_source_with_decision(source, use_syntect);
    let formatted = format_code_for_soft_wrap(source, max_columns);
    let prepared = Arc::new(build_prepared_code(
        source,
        formatted,
        content_hash,
        analysis,
        true,
    ));
    if let Ok(mut guard) = cache.lock() {
        if guard.len() >= SYNTECT_CACHE_CAPACITY {
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

unsafe fn apply_lexical_highlights(storage: *mut AnyObject, text: &str, spans: Vec<HighlightSpan>) {
    if spans.is_empty() {
        return;
    }
    let _: () = msg_send![storage, beginEditing];
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
    let _: () = msg_send![storage, endEditing];
}

unsafe fn apply_syntect_highlights(
    storage: *mut AnyObject,
    spans: &[SyntectSpan],
    display_map: Option<&DisplaySourceMap>,
) {
    if spans.is_empty() {
        return;
    }
    let _: () = msg_send![storage, beginEditing];
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
    let _: () = msg_send![storage, endEditing];
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
    let _: () = msg_send![storage, beginEditing];
    let color_key = make_nsstring("NSColor");
    let mut colors: HashMap<[u8; 4], *mut AnyObject> = HashMap::new();
    for span in &prepared.spans {
        let color = if let Some(&color) = colors.get(&span.foreground) {
            color
        } else {
            let color: *mut AnyObject = msg_send![
                class!(NSColor),
                colorWithSRGBRed: f64::from(span.foreground[0]) / 255.0,
                green: f64::from(span.foreground[1]) / 255.0,
                blue: f64::from(span.foreground[2]) / 255.0,
                alpha: f64::from(span.foreground[3]) / 255.0
            ];
            colors.insert(span.foreground, color);
            color
        };
        let _: () = msg_send![
            storage,
            addAttribute: color_key,
            value: color,
            range: NSRange::new(span.start, span.end.saturating_sub(span.start))
        ];
    }
    CFRelease(color_key as *const c_void);
    let _: () = msg_send![storage, endEditing];
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
    let analysis = analyze_source_for_highlighting(source_text);
    if !analysis.use_syntect {
        return;
    }
    let content_hash = syntect_fnv1a64(source_text.as_bytes());
    if let Some(spans) = cached_syntect_highlight(source_text, content_hash, analysis) {
        apply_syntect_highlights(storage, spans.as_ref(), display_map);
        return;
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
    use super::{
        analyze_source_for_highlighting, cached_syntect_highlight, detect_language,
        format_code_for_soft_wrap, prepare_code_for_soft_wrap, should_use_syntect_with_limits,
    };
    use objc2_foundation::NSRange;
    use std::sync::Arc;

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
        let analysis = analyze_source_for_highlighting(source);
        assert_eq!(analysis.language, detect_language(source));
        let content_hash = super::syntect_fnv1a64(source.as_bytes());
        let first =
            cached_syntect_highlight(source, content_hash, analysis).expect("syntect should load");
        let second =
            cached_syntect_highlight(source, content_hash, analysis).expect("cache should hit");
        assert!(!first.is_empty());
        assert!(
            Arc::ptr_eq(&first, &second),
            "syntect cache hits must share spans"
        );
    }

    #[test]
    fn prepared_code_cache_hits_share_text_and_spans() {
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
