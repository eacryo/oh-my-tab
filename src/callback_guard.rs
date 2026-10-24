//! Panic boundaries for Rust callbacks invoked by C and Objective-C runtimes.
//!
//! A panic cannot unwind through an `extern "C"` callback. These small helpers keep the
//! boundary explicit and provide conservative fallbacks for each callback return shape.

use std::panic::{catch_unwind, AssertUnwindSafe};

/// Run a void callback and log instead of allowing panic to cross the ABI boundary.
pub(crate) fn void(label: &'static str, callback: impl FnOnce()) {
    if catch_unwind(AssertUnwindSafe(callback)).is_err() {
        crate::log_info!("[callback] panic contained in {label}");
    }
}

/// Run a query callback and return a conservative fallback when it panics.
pub(crate) fn bool(label: &'static str, fallback: bool, callback: impl FnOnce() -> bool) -> bool {
    match catch_unwind(AssertUnwindSafe(callback)) {
        Ok(value) => value,
        Err(_) => {
            crate::log_info!("[callback] panic contained in {label}; using fallback={fallback}");
            fallback
        }
    }
}

/// Run a callback whose return type is neither void nor an event, and return `fallback` when it
/// panics. This is the shape a selector needs when its contract names a definite answer: an
/// `NSApplicationDelegate` method returning `Bool` must return a value, not unwind.
pub(crate) fn value<T>(label: &'static str, fallback: T, callback: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(callback)) {
        Ok(value) => value,
        Err(_) => {
            crate::log_info!("[callback] panic contained in {label}; using fallback");
            fallback
        }
    }
}

/// Run an object-returning Objective-C selector and answer null when it panics.
///
/// Null is the conservative answer for every selector in this crate that returns an object: a field
/// editor, a hit-test and a dragged object all mean "this object does not handle it", which is the
/// same answer the selector would give with no implementation installed at all.
pub(crate) fn nil<T>(label: &'static str, callback: impl FnOnce() -> *mut T) -> *mut T {
    match catch_unwind(AssertUnwindSafe(callback)) {
        Ok(value) => value,
        Err(_) => {
            crate::log_info!("[callback] panic contained in {label}; answering null");
            std::ptr::null_mut()
        }
    }
}

/// Run an event-tap callback and preserve the original event when its Rust body panics.
pub(crate) fn event<T>(label: &'static str, original: T, callback: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(callback)) {
        Ok(value) => value,
        Err(_) => {
            crate::log_info!("[callback] panic contained in {label}; passing event through");
            original
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{bool, event, nil, value, void};

    #[test]
    fn void_callback_contains_panics() {
        void("test_void", || panic!("expected test panic"));
    }

    #[test]
    fn bool_callback_uses_conservative_fallback() {
        assert!(!bool("test_bool", false, || panic!("expected test panic")));
        assert!(bool("test_bool", false, || true));
    }

    #[test]
    fn event_callback_preserves_original_event_on_panic() {
        let original = 42usize;
        assert_eq!(
            event("test_event", original, || panic!("expected test panic")),
            original
        );
        assert_eq!(event("test_event", original, || 7), 7);
    }
    #[test]
    fn value_callback_uses_the_supplied_fallback() {
        assert_eq!(
            value("test_value", 7usize, || panic!("expected test panic")),
            7
        );
        assert_eq!(value("test_value", 7usize, || 9), 9);
    }

    #[test]
    fn nil_callback_answers_null() {
        let answer: *mut u8 = nil("test_nil", || panic!("expected test panic"));
        assert!(answer.is_null());
    }

    /// Every Rust callback entered from C or Objective-C must keep its panic inside this module.
    ///
    /// Why a source scan: the rule is stated in ARCHITECTURE.md as mandatory, and measured at the
    /// time it was written, 288 of 375 `extern "C" fn` bodies in this crate did not call the guard
    /// at all -- a panic in any of them aborts the process instead of failing one click. Nothing
    /// could fail when a new callback was added without the guard, so the rule decayed silently.
    ///
    /// The scan reads this crate's own sources and requires that, for every `extern "C" fn` with a
    /// body, the **whole body** is one call to `callback_guard::`. A guard around an inner fragment
    /// leaves the rest of the body outside the boundary, which is the failure this test exists for.
    #[test]
    fn every_c_callback_keeps_its_panic_inside_the_guard() {
        let src_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        let mut total = 0usize;
        let mut files = Vec::new();
        collect_rust_files(&src_dir, &mut files);
        files.sort();
        for file in &files {
            let text = match std::fs::read_to_string(file) {
                Ok(text) => text,
                Err(_) => continue,
            };
            for (line_no, body) in c_callback_bodies(&text) {
                total += 1;
                if !body_is_guarded(&body) {
                    let rel = file.strip_prefix(&src_dir).unwrap_or(file);
                    offenders.push(format!("{}:{}", rel.display(), line_no));
                }
            }
        }
        assert!(
            total > 300,
            "the scan found only {total} C callbacks; it is looking at the wrong thing and cannot decide"
        );
        assert!(
            offenders.is_empty(),
            "{} of {total} C callbacks do not keep their whole body inside callback_guard:: -- a panic in \
             any of them aborts the process:\n  {}",
            offenders.len(),
            offenders.join("\n  ")
        );
    }

    fn collect_rust_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rust_files(&path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }

    /// Every `extern "C" fn` definition with a body, as (first line, body text).
    ///
    /// A definition is recognised by the `extern "C" fn <name>(` shape at the start of a line (a
    /// type alias or a field would have `type` or `:` before `fn`); the body runs from the opening
    /// brace to the matching close, counted per line, which is exact enough for this crate's
    /// formatting because `cargo fmt` keeps one statement per line.
    /// Whether `text` names a function: `extern "C" fn <name>(`.
    const PREFIX: &str = "extern \"C\" fn";

    fn has_fn_name(text: &str) -> bool {
        let Some(rest) = text.strip_prefix(PREFIX) else {
            return false;
        };
        rest.trim_start()
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == 95u8 as char)
    }
    /// `line` with a leading visibility (`pub`, optionally with `(…)`) and `unsafe` removed.
    fn strip_visibility(line: &str) -> &str {
        let line = line.trim_start();
        let rest = match line.strip_prefix("pub") {
            Some(rest) => {
                let rest = rest.trim_start();
                match rest.strip_prefix('(') {
                    Some(inner) => match inner.find(')') {
                        Some(close) => inner[close + 1..].trim_start(),
                        None => rest,
                    },
                    None => rest,
                }
            }
            None => line,
        };
        rest.strip_prefix("unsafe ").unwrap_or(rest)
    }
    fn c_callback_bodies(text: &str) -> Vec<(usize, String)> {
        let mut out = Vec::new();
        let lines: Vec<&str> = text.lines().collect();
        let mut i = 0usize;
        while i < lines.len() {
            let line = lines[i].trim_start();
            if line.contains("type ") {
                i += 1;
                continue;
            }
            let visible = strip_visibility(line);
            // A type alias (`type Tap = unsafe extern "C" fn(...) -> ...;`) is not a definition:
            // it has no body to guard.
            // Only a definition starts with `extern "C" fn` (after visibility/unsafe). A type
            // alias or a field puts `type` / `:` before `fn`, and may wrap across lines, so the
            // leading word is what decides.
            // A definition has the function name after `extern "C" fn`; a type alias or an
            // `extern` block entry has only a parameter list after it.
            if !visible.starts_with(PREFIX) || !has_fn_name(visible) {
                i += 1;
                continue;
            }
            // The body's closing brace is the first line that consists of `}` alone (after
            // trimming) at the item's own indentation. Every statement inside is indented
            // further, so this is exact for rustfmt output and needs no brace counting -- which
            // kept being fooled by braces in strings and in multi-line signatures.
            let item_indent = lines[i].len() - lines[i].trim_start().len();
            let mut j = i + 1;
            while j < lines.len() {
                let trimmed = lines[j].trim();
                let indent = lines[j].len() - lines[j].trim_start().len();
                if trimmed == "}" && indent == item_indent {
                    break;
                }
                j += 1;
            }
            // A declaration inside an `extern` block ends with `;` and has no body to guard.
            let is_declaration = j < lines.len() && lines[j].trim().ends_with(';');
            if j < lines.len() && !is_declaration {
                out.push((i + 1, lines[i..=j].join("\n")));
            }
            i = j + 1;
        }
        out
    }

    /// `text` with string literals and `//` comments blanked.
    ///
    /// The guard check counts parentheses to find where a call ends. A `(` or `)` inside a comment
    /// would otherwise be counted too, and a body like
    /// `callback_guard::void(name, || { // (\n });\n panic!("outside");` would read as fully guarded.
    fn blank_code(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut in_str = false;
        let mut in_line_comment = false;
        let mut in_block_comment = false;
        let mut escaped = false;
        let mut chars = text.chars().peekable();
        while let Some(ch) = chars.next() {
            if in_line_comment {
                if ch == '\n' {
                    in_line_comment = false;
                    out.push(ch);
                } else {
                    out.push(' ');
                }
                continue;
            }
            if in_block_comment {
                if ch == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    in_block_comment = false;
                    out.push_str("  ");
                } else {
                    out.push(if ch == '\n' { '\n' } else { ' ' });
                }
                continue;
            }
            if in_str {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    in_str = false;
                }
                out.push(if ch == '\n' { '\n' } else { ' ' });
                continue;
            }
            if ch == '/' && chars.peek() == Some(&'/') {
                chars.next();
                in_line_comment = true;
                out.push_str("  ");
                continue;
            }
            if ch == '/' && chars.peek() == Some(&'*') {
                chars.next();
                in_block_comment = true;
                out.push_str("  ");
                continue;
            }
            if ch == '"' {
                in_str = true;
                out.push(' ');
                continue;
            }
            out.push(ch);
        }
        out
    }
    /// The whole body must be exactly one `callback_guard::<helper>(...)` call.
    ///
    /// Checking only the first line was not enough: a body that called a guard around an empty
    /// closure and then did its real work would have passed. This checks the shape instead --
    /// after the opening brace there is exactly one statement, it is a guard call, and its
    /// closing paren is the last thing before the body's own closing brace. String literals are
    /// blanked first, so a message containing braces cannot shift the counting.
    fn body_is_guarded(body: &str) -> bool {
        // The body opens at the brace after the parameter list: the first `{` that follows the
        // `)` balancing the `(` of `fn name(...)`. A signature can span lines and its arguments
        // can contain braces, so taking the first `{` in the item is not precise enough.
        let Some(open) = body_opening_brace(body) else {
            return false;
        };
        let blanked = blank_code(&body[open + 1..]);
        // Drop the closing brace of the body. The statement that ends the body may close with
        // `});` on the line before it, so the trailing text after removing `}` is trimmed below.
        let tail = blanked.trim_end();
        // The body's closing brace is the last line; drop it (and the newline before it).
        let Some(tail) = tail
            .strip_suffix(
                "
}",
            )
            .or_else(|| tail.strip_suffix("}"))
        else {
            return false;
        };
        // Leading comments are not part of the statement.
        let tail: String = tail
            .lines()
            .skip_while(|line| line.trim_start().starts_with("//") || line.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        let tail = tail.trim();
        // A closure may open with `unsafe {` or `move |...| {`; blank the `unsafe` keyword so the
        // shapes read the same. The `||` itself is left alone -- it is an argument, not syntax
        // this check needs to understand.
        let normalised = tail.replace("|| unsafe {", "|| {").replace("move |", "|");
        let tail = normalised.trim_start();
        let tail = tail
            .strip_prefix("unsafe {")
            .map(str::trim_start)
            .unwrap_or(tail);
        let Some(rest) = tail
            .strip_prefix("callback_guard::")
            .or_else(|| tail.strip_prefix("crate::callback_guard::"))
        else {
            return false;
        };
        let Some(call_start) = rest.find('(') else {
            return false;
        };
        let helper = &rest[..call_start];
        if helper.is_empty()
            || !helper
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return false;
        }
        let args = &rest[call_start..];
        let Some(close) = closing_paren(args) else {
            return false;
        };
        // The statement ends with the call: `)` followed only by the statement terminator or by
        // nothing (a tail expression).
        let after = args[close + 1..].trim();
        after.is_empty() || after == ";"
    }

    /// The byte offset of the `{` that opens the body of an item, or `None`.
    fn body_opening_brace(item: &str) -> Option<usize> {
        let blanked = blank_literals(item);
        let mut parens = 0i32;
        let mut seen_paren = false;
        for (i, ch) in blanked.char_indices() {
            match ch {
                '(' => {
                    parens += 1;
                    seen_paren = true;
                }
                ')' => parens -= 1,
                '{' if seen_paren && parens <= 0 => return Some(i),
                _ => {}
            }
        }
        None
    }

    /// The index of the paren that closes the call starting at `text`'s first `(`.
    fn closing_paren(text: &str) -> Option<usize> {
        let mut depth = 0i32;
        for (i, ch) in text.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// `text` with everything inside string literals replaced by spaces.
    /// `text` with double-quoted string literals replaced by spaces.
    ///
    /// Only `"` starts a literal in Rust; treating `'` as a delimiter as well blanked every
    /// lifetime and char literal, which silently swallowed the code that followed.
    fn blank_literals(text: &str) -> String {
        let quote = '"';
        let mut out = String::with_capacity(text.len());
        let mut in_str = false;
        let mut escaped = false;
        for ch in text.chars() {
            if in_str {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == quote {
                    in_str = false;
                }
                out.push(' ');
                continue;
            }
            if ch == quote {
                in_str = true;
                out.push(' ');
                continue;
            }
            out.push(ch);
        }
        out
    }
    #[test]
    fn the_body_check_rejects_a_guard_that_does_not_cover_the_body() {
        let guarded =
            "fn f() {\n    callback_guard::void(\"x\", || unsafe {\n        work();\n    });\n}";
        assert!(
            body_is_guarded(guarded),
            "a whole-body guard must pass: {guarded}"
        );
        let fragment = "fn f() {\n    callback_guard::void(\"x\", || {});\n    work();\n}";
        assert!(
            !body_is_guarded(fragment),
            "a guard around a fragment must not pass"
        );
        let other = "fn f() {\n    work();\n}";
        assert!(!body_is_guarded(other), "no guard must not pass");
    }

    #[test]
    fn the_extractor_finds_a_multiline_signature_with_a_brace_in_a_string() {
        let text = "extern \"C\" fn a(\n    x: u8,\n) {\n    callback_guard::void(\"a\", || {\n        log(\"{\");\n    });\n}\n";
        let bodies = c_callback_bodies(text);
        assert_eq!(bodies.len(), 1);
        assert!(body_is_guarded(&bodies[0].1), "body: {:?}", bodies[0].1);
    }

    #[test]
    fn comments_cannot_hide_code_outside_the_guard() {
        // A `(` in a comment must not balance the real call's `)`, and a later comment's `)` must
        // not be read as the end of the call.
        let attack = "fn f() {\n    callback_guard::void(\"f\", || {\n        // (\n    });\n    panic!(\"outside\");\n    // )\n}";
        assert!(
            !body_is_guarded(attack),
            "a comment bracket must not close the guard call"
        );
        let guarded = "fn f() {\n    callback_guard::void(\"f\", || {\n        // (\n    });\n}";
        assert!(
            body_is_guarded(guarded),
            "a comment inside a real guard is fine"
        );
    }

    #[test]
    fn any_visibility_is_recognised() {
        let text = "pub(in crate::settings) extern \"C\" fn a() {\n    callback_guard::void(\"a\", || {});\n}\n";
        let bodies = c_callback_bodies(text);
        assert_eq!(
            bodies.len(),
            1,
            "pub(in ...) must be recognised as a definition"
        );
        assert!(body_is_guarded(&bodies[0].1));
    }
}
