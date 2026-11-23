//! The text a copy action puts on the clipboard.
//!
//! Pure, and separate from the action that writes the pasteboard: the exact text is what the user
//! pastes, so every rule here (the path separators, the digest line format) is asserted without
//! AppKit, a Finder or a file. The formats are deliberately different tools: one path per line for
//! scripts and `xargs`, one comma-separated line for reading and pasting into a message, bare
//! digests for comparison, and `digest  name` for `sha256sum -c`.

use finder_protocol::PathLayout;

/// The paths as one clipboard payload: one per line, or one comma-separated line.
///
/// The single-line form is for reading and pasting into a message, not for a shell: `a, b, c` is
/// three list items, not three command arguments. A path that itself contains `, ` is therefore
/// ambiguous inside it — the per-line form is the one to use when that matters. Quotes are not
/// added: in a human-readable list they would be noise, and they would not make the line a valid
/// command either.
pub(crate) fn paths_text(paths: &[String], layout: PathLayout) -> String {
    match layout {
        PathLayout::Newline => paths.join("\n"),
        PathLayout::SingleLine => paths.join(", "),
    }
}

/// The digests as one clipboard payload, in the order the items were selected.
///
/// With `with_name`, each line is GNU `sha256sum`'s `digest  name` form. A name containing a
/// backslash, a newline or a carriage return would make that line ambiguous, so the whole line is
/// prefixed with `\` and those three characters are escaped exactly as GNU does (`cksum.c`'s
/// `strcspn (s, "\\\n\r")`, measured 2026-11-23). Which verifier can read the result is a
/// property of the verifier: GNU's `sha256sum -c` undoes all three; macOS's `/usr/bin/shasum`
/// (Perl) undoes `\\` and `\n` but not `\r`; macOS 26's `/sbin/sha256sum` undoes none of them.
/// A name with a CR in it is therefore verifiable with GNU coreutils only.
pub(crate) fn digests_text(entries: &[(String, String)], with_name: bool) -> String {
    entries
        .iter()
        .map(|(path, digest)| {
            if with_name {
                digest_line(digest, path)
            } else {
                digest.clone()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One `sha256sum`-compatible line.
fn digest_line(digest: &str, path: &str) -> String {
    if path.contains(['\\', '\n', '\r']) {
        let escaped = path
            .replace('\\', "\\\\")
            .replace('\n', "\\n")
            .replace('\r', "\\r");
        format!("\\{digest}  {escaped}")
    } else {
        format!("{digest}  {path}")
    }
}

#[cfg(test)]
mod tests {
    use super::{digests_text, paths_text};
    use finder_protocol::PathLayout;

    fn paths(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    /// The newline layout is the raw list: no trailing newline, no quoting, so `pbcopy` output can
    /// be piped straight into `xargs -d '\n'` (or read line by line).
    #[test]
    fn the_newline_layout_is_one_path_per_line_with_nothing_added() {
        let text = paths_text(&paths(&["/a", "/b c", "/d"]), PathLayout::Newline);
        assert_eq!(text, "/a\n/b c\n/d");
        assert!(!text.ends_with('\n'));
        assert_eq!(paths_text(&paths(&["/only"]), PathLayout::Newline), "/only");
    }

    /// The single-line layout is a human-readable list: comma and space between paths, one
    /// physical line, no quoting. A path that itself contains `, ` is the one ambiguity this format
    /// accepts, and the per-line form is the answer to it — asserted so the trade-off is explicit.
    #[test]
    fn the_single_line_layout_separates_paths_with_a_comma_and_a_space() {
        let text = paths_text(
            &paths(&[
                "/tmp/plain.txt",
                "/tmp/with space.png",
                "/tmp/it's.png",
                "/tmp/a,b.png",
            ]),
            PathLayout::SingleLine,
        );
        assert_eq!(
            text,
            "/tmp/plain.txt, /tmp/with space.png, /tmp/it's.png, /tmp/a,b.png"
        );
        assert!(!text.contains('\n'), "the format is one physical line");
        assert_eq!(
            paths_text(&paths(&["/only/path.png"]), PathLayout::SingleLine),
            "/only/path.png",
            "one path needs no separator at all"
        );
        assert_eq!(paths_text(&[], PathLayout::SingleLine), "");
    }

    /// Bare digests are one per line in selection order: the whole point is comparing a set, and a
    /// name would only get in the way.
    #[test]
    fn bare_digests_are_one_line_each() {
        let entries = vec![
            ("/a".to_string(), "aaa".to_string()),
            ("/b".to_string(), "bbb".to_string()),
        ];
        assert_eq!(digests_text(&entries, false), "aaa\nbbb");
        assert_eq!(digests_text(&[], false), "");
    }

    /// The named form is GNU `sha256sum`'s, including its escape rule, so `sha256sum -c` can verify
    /// the copied text. A name with neither a backslash nor a newline keeps the plain form.
    #[test]
    fn named_digests_follow_the_gnu_line_format() {
        let entries = vec![
            ("/tmp/plain.txt".to_string(), "aaa".to_string()),
            ("/tmp/back\\slash".to_string(), "bbb".to_string()),
            ("/tmp/line\nbreak".to_string(), "ccc".to_string()),
            ("/tmp/cr\rname".to_string(), "ddd".to_string()),
            ("/tmp/both\\and\nbreak".to_string(), "eee".to_string()),
        ];
        assert_eq!(
            digests_text(&entries, true),
            "aaa  /tmp/plain.txt\n\
             \\bbb  /tmp/back\\\\slash\n\
             \\ccc  /tmp/line\\nbreak\n\
             \\ddd  /tmp/cr\\rname\n\
             \\eee  /tmp/both\\\\and\\nbreak"
        );
    }

    /// The named form is not a description: `shasum -c` must accept the lines for names it can
    /// express, which is the only evidence that the copy is verifiable and not merely plausible.
    #[test]
    fn shasum_verifies_the_named_form() {
        let dir = std::env::temp_dir().join(format!("oh-my-tab-format-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        // shasum decodes `\\` and `\n`; a CR name is GNU-only and deliberately absent here.
        let names = ["plain.txt", "back\\slash.txt", "line\nbreak.txt"];
        let mut entries = Vec::new();
        for name in names {
            let path = dir.join(name);
            std::fs::write(&path, b"abc").expect("write the fixture");
            let digest = crate::finder_menu::hash::sha256_file(&path).expect("hash the fixture");
            entries.push((path.to_string_lossy().into_owned(), digest));
        }
        let check_path = dir.join("checks.txt");
        std::fs::write(&check_path, digests_text(&entries, true)).expect("write the check file");
        let output = std::process::Command::new("/usr/bin/shasum")
            .arg("-a")
            .arg("256")
            .arg("-c")
            .arg(&check_path)
            .current_dir(&dir)
            .output()
            .expect("shasum runs");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "shasum -c rejected the copied lines: {stderr}"
        );
        assert!(
            !stderr.contains("improperly formatted"),
            "a line was not parseable: {stderr}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
