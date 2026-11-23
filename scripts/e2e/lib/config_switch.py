"""Rewrite one boolean key inside one table of the app's config, for the e2e scenarios.

A scenario that flips `windows.show_other_desktops` hands the app the user's own
`~/.config/oh-my-tab/config.toml`. Writing the key twice into `[windows]` makes the whole file
unparseable, the app silently falls back to its defaults, and the scenario then asserts against a
state it never set. `set_bool` replaces an existing occurrence of the key however it is indented and
touches no other table; `apply` refuses to write anything the TOML parser cannot read back.

`python3 scripts/e2e/lib/config_switch.py --selftest` runs the regression cases.
"""

from __future__ import annotations

import os
import re
import sys
import tempfile

try:  # Python >= 3.11
    import tomllib
except ModuleNotFoundError:  # pragma: no cover - the rewrite is still duplicate-safe without it
    tomllib = None

_TABLE_HEADER = re.compile(r"^\[\s*([A-Za-z0-9_.\-\"']+)\s*\]$")
_ARRAY_TABLE_HEADER = re.compile(r"^\[\[\s*([A-Za-z0-9_.\-\"']+)\s*\]\]$")


def _unquote_segment(segment):
    """One dotted table segment with its surrounding quotes (and whitespace) removed."""
    segment = segment.strip()
    if len(segment) >= 2 and segment[0] == segment[-1] and segment[0] in "\"'":
        return segment[1:-1]
    return segment


def _normalize_table_name(name):
    """A table header's name with quoted segments unquoted: `a."b"` and `a.b` name the same table."""
    return ".".join(_unquote_segment(part) for part in name.split("."))


def _unquote_key(head):
    """An assignment's left side with its surrounding quotes removed (`"key"` == `key`)."""
    head = head.strip()
    if len(head) >= 2 and head[0] == head[-1] and head[0] in "\"'":
        return head[1:-1]
    return head


def _table_of(line):
    """The table a line opens, or None when the line is not a table header.

    Quoted segments are unquoted: TOML spells `["another"]` and `[another]` for the same table, and a
    matcher that only recognised the bare form left the cursor on the previous table -- the next
    rewrite then landed in the wrong one while the document stayed parseable."""
    match = _TABLE_HEADER.match(line.strip())
    return _normalize_table_name(match.group(1)) if match else None


def _is_assignment(line, key):
    head, separator, _ = line.strip().partition("=")
    return bool(separator) and _unquote_key(head) == key


def set_bool(text, table, key, value):
    """Return `text` with `table`.`key` set to `value`, replacing any existing occurrence."""
    current = None
    kept = []
    for line in text.splitlines(keepends=True):
        opened = _table_of(line)
        if opened is not None:
            current = opened
        if current == table and _is_assignment(line, key):
            continue
        kept.append(line)

    literal = "true" if value else "false"
    updated = []
    inserted = False
    for line in kept:
        updated.append(line)
        if not inserted and _table_of(line) == table:
            updated.append(f"{key} = {literal}\n")
            inserted = True
    if not inserted:
        raise ValueError(f"the config has no [{table}] table to write {key} into")
    return "".join(updated)


def _table_header_of(line):
    """The table a line opens, for both `[table]` and `[[table]]` headers."""
    array_match = _ARRAY_TABLE_HEADER.match(line.strip())
    if array_match:
        return _normalize_table_name(array_match.group(1))
    return _table_of(line)


def set_string(text, table, key, value):
    """Return `text` with `table`.`key` set to the quoted string `value`.

    Same contract as `set_bool` -- one occurrence, inside its own table, every other line untouched --
    because the caller (`panel-contrast.sh`) rewrites a key of the user's real config and must be able
    to put exactly that key back. A missing table is refused rather than invented: appending the key
    nowhere would leave the config in a state the app silently replaces with defaults.
    """
    quoted = '"' + value.replace('\\', '\\\\').replace('"', '\\"') + '"'
    current = None
    kept = []
    for line in text.splitlines(keepends=True):
        opened = _table_of(line)
        if opened is not None:
            current = opened
        if current == table and _is_assignment(line, key):
            continue
        kept.append(line)

    updated = []
    inserted = False
    for line in kept:
        updated.append(line)
        if not inserted and _table_of(line) == table:
            updated.append(f"{key} = {quoted}\n")
            inserted = True
    if not inserted:
        raise ValueError(f"the config has no [{table}] table to write {key} into")
    return "".join(updated)


def replace_key_bindings(text, items):
    """Return `text` with the whole `[key_bindings]` table replaced by `items`.

    `items` is a sequence of (action, trigger) pairs, each written enabled. Every other table is
    preserved verbatim, so a scenario that forces specific bindings does not also drop the user's
    other settings. The caller validates the result with `tomllib` before writing.
    """
    kept = []
    skipping = False
    for line in text.splitlines(keepends=True):
        header = _table_header_of(line)
        if header is not None:
            skipping = header == "key_bindings" or header.startswith("key_bindings.")
            if skipping:
                continue
        if skipping:
            continue
        kept.append(line)

    block = ["\n[key_bindings]\nenabled = true\n"]
    for action, trigger in items:
        block.append("\n[[key_bindings.items]]\n")
        block.append(f'action = "{action}"\n')
        block.append(f'trigger = "{trigger}"\n')
        block.append("enabled = true\n")
    return "".join(kept) + "".join(block)


def apply_key_bindings(path, items):
    """Rewrite only the `[key_bindings]` table; refuse to leave an unparseable config behind."""
    with open(path, encoding="utf-8") as handle:
        text = handle.read()
    updated = replace_key_bindings(text, items)
    if tomllib is not None:
        parsed = tomllib.loads(updated)
        assert parsed["key_bindings"]["enabled"] is True
        assert len(parsed["key_bindings"]["items"]) == len(items)
        for table in tomllib.loads(text):
            if table != "key_bindings":
                assert table in parsed, f"replace_key_bindings dropped [{table}]"
    with open(path, "w", encoding="utf-8") as handle:
        handle.write(updated)
    return updated


def apply(path, table, key, value):
    """Rewrite the key in the file at `path`; never leave an unparseable config behind."""
    with open(path, encoding="utf-8") as handle:
        text = handle.read()
    updated = set_bool(text, table, key, value)
    if tomllib is not None:
        tomllib.loads(updated)
    with open(path, "w", encoding="utf-8") as handle:
        handle.write(updated)
    return updated


def _value_end(text, start):
    """The offset just past the value that begins at `start` (just after `=`).

    Handles a quoted string, a scalar (to end of line) and an array -- INLINE OR MULTI-LINE. A
    caller that assumed "one line per key" would remove the first line of a hand-edited multi-line
    array and leave the rest behind, and the config would then be unparseable.
    """
    n = len(text)
    i = start
    while i < n and text[i] in " \t":
        i += 1
    if i < n and text[i] in "\"'":
        quote = text[i]
        i += 1
        while i < n:
            if quote == '"' and text[i] == "\\":
                i += 2
                continue
            if text[i] == quote:
                return i + 1
            i += 1
        return n
    if i < n and text[i] == "[":
        depth = 0
        while i < n:
            char = text[i]
            if char == "#":
                # A comment runs to the end of the line; brackets inside it are TEXT. Counting them
                # closed the array early, and the rest of a multi-line array was then left behind as
                # key assignments -- invalid TOML the caller would have written back.
                while i < n and text[i] != "\n":
                    i += 1
                continue
            if char in "\"'":
                quote = char
                i += 1
                while i < n:
                    if quote == '"' and text[i] == "\\":
                        i += 2
                        continue
                    if text[i] == quote:
                        i += 1
                        break
                    i += 1
                continue
            if char == "[":
                depth += 1
            elif char == "]":
                depth -= 1
                if depth == 0:
                    return i + 1
            i += 1
        return n
    while i < n and text[i] != "\n":
        i += 1
    return i


def _document_is_balanced(text):
    """Whether every bracket is balanced and every string terminated, ignoring comments.

    Used when no TOML parser is available to validate a rewrite (macOS's /usr/bin/python3 is 3.9 and
    has no tomllib): a document this check accepts may still be rejected by the app, but one it
    REFUSES is guaranteed invalid. It exists because the array scanner used to count a bracket
    inside a `#` comment and produce exactly that.
    """
    depth = 0
    i = 0
    n = len(text)
    while i < n:
        char = text[i]
        if char == "#":
            while i < n and text[i] != "\n":
                i += 1
            continue
        if char in "\"'":
            quote = char
            i += 1
            terminated = False
            while i < n:
                if quote == '"' and text[i] == "\\":
                    i += 2
                    continue
                if text[i] == quote:
                    terminated = True
                    i += 1
                    break
                i += 1
            if not terminated:
                return False
            continue
        if char in "[{":
            depth += 1
        elif char in "]}":
            depth -= 1
            if depth < 0:
                return False
        i += 1
    return depth == 0


def set_string_list(text, table, key, values):
    """Return `text` with `table`.`key` set to an inline string array of `values`.

    Replaces an existing occurrence of the key whatever shape it has (absent, inline array,
    MULTI-LINE array) and inserts the new one right after the table header when it was absent. The
    multi-line shape is the counter-example: removing only the assignment line leaves the array's
    remaining lines in the file, which the app then rejects as unparseable.
    """
    quoted = "[" + ", ".join(
        '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'
        for value in values
    ) + "]"
    current = None
    updated = []
    inserted = False
    i = 0
    n = len(text)
    while i < n:
        line_end = text.find("\n", i)
        if line_end == -1:
            line_end = n
        line = text[i:line_end + 1]
        header = _table_of(line)
        if header is not None:
            current = header
            updated.append(line)
            i = line_end + 1
            if current == table and not inserted:
                updated.append(f"{key} = {quoted}\n")
                inserted = True
            continue
        if current == table and _is_assignment(line, key):
            equal = text.index("=", i) + 1
            end = _value_end(text, equal)
            if end < n and text[end] == "\n":
                end += 1
            i = end
            continue
        updated.append(line)
        i = line_end + 1
    if not inserted:
        raise ValueError(f"the config has no [{table}] table to write {key} into")
    return "".join(updated)


def _first_unchanged_violation(original_text, updated_text, table, key):
    """The first table/key whose parsed value changed between the documents, ignoring the target.

    A rewrite is only allowed to change the ONE key it was asked to change. Comparing the parsed
    documents (rather than the text) catches a mis-located write that leaves a document which still
    parses -- for example landing in a same-named field of a quoted neighbouring table.
    """
    before = _parsed(original_text)
    after = _parsed(updated_text)

    def walk(before_value, after_value, path):
        if isinstance(before_value, dict) and isinstance(after_value, dict):
            for name in sorted(set(before_value) | set(after_value)):
                if path == table and name == key:
                    continue
                child = f"{path}.{name}" if path else name
                if name not in before_value or name not in after_value:
                    return child
                found = walk(before_value[name], after_value[name], child)
                if found is not None:
                    return found
            return None
        return None if before_value == after_value else (path or "<root>")

    return walk(before, after, "")


def read_list(path, table, key):
    """The string list at `table`.`key`, or None when the key is ABSENT.

    Same contract as `read_string`: a file that cannot be read raises instead of answering
    "absent", and a value of the wrong type is reported rather than silently coerced.
    """
    with open(path, encoding="utf-8") as handle:
        text = handle.read()
    parsed = _parsed(text)
    values = parsed
    for part in table.split("."):
        if not isinstance(values, dict) or part not in values:
            return None
        values = values[part]
    if not isinstance(values, dict) or key not in values:
        return None
    value = values[key]
    if not isinstance(value, list) or not all(isinstance(item, str) for item in value):
        return None
    return list(value)


def apply_list(path, table, key, values):
    """Rewrite one string-list key in place; never leave an unparseable config behind.

    Unlike the older boolean/string writers this one REQUIRES a parser (Python's `tomllib`, 3.11+):
    the rewrite must be verified against the whole document, and a machine without the parser must
    stop before touching the user's config rather than write an unverifiable file.
    """
    if tomllib is None:
        raise ConfigUnreadable(
            "no TOML parser (tomllib) is available; refusing to rewrite the config"
        )
    with open(path, encoding="utf-8") as handle:
        text = handle.read()
    updated = set_string_list(text, table, key, values)
    if not _document_is_balanced(updated):
        raise ValueError("the rewritten config is not balanced TOML; refusing to write it")
    parsed = tomllib.loads(updated)
    table_values = parsed
    for part in table.split("."):
        if not isinstance(table_values, dict) or part not in table_values:
            raise ValueError(f"the rewritten config lost its [{table}] table")
        table_values = table_values[part]
    assert table_values[key] == list(values), updated
    violation = _first_unchanged_violation(text, updated, table, key)
    if violation is not None:
        raise ValueError(
            f"the rewrite changed {violation}, which it was not asked to change; refusing to write"
        )
    with open(path, "w", encoding="utf-8") as handle:
        handle.write(updated)
    return updated


def unset_key(text, table, key):
    """Return `text` with `table`.`key` removed, leaving every other line untouched.

    Used by a scenario that had to write a key the user's config did not have: restoring "absent" is
    not the same as restoring a default, and writing the default would silently add a setting the user
    never chose. The line is located the same way `set_string` locates it, so the two cannot disagree
    about which occurrence belongs to the table.
    """
    current = None
    kept = []
    i = 0
    n = len(text)
    while i < n:
        line_end = text.find("\n", i)
        if line_end == -1:
            line_end = n
        line = text[i:line_end + 1]
        opened = _table_of(line)
        if opened is not None:
            current = opened
        if current == table and _is_assignment(line, key):
            equal = text.index("=", i) + 1
            end = _value_end(text, equal)
            if end < n and text[end] == "\n":
                end += 1
            i = end
            continue
        kept.append(line)
        i = line_end + 1
    return "".join(kept)


def apply_string(path, table, key, value):
    """Rewrite one string key in place; never leave an unparseable config behind."""
    with open(path, encoding="utf-8") as handle:
        text = handle.read()
    updated = set_string(text, table, key, value)
    if not _document_is_balanced(updated):
        raise ValueError("the rewritten config is not balanced TOML; refusing to write it")
    if tomllib is not None:
        parsed = tomllib.loads(updated)
        table_values = parsed
        for part in table.split("."):
            table_values = table_values[part]
        assert table_values[key] == value
    with open(path, "w", encoding="utf-8") as handle:
        handle.write(updated)
    return updated


def apply_unset(path, table, key):
    """Remove one key in place; never leave an unparseable config behind."""
    with open(path, encoding="utf-8") as handle:
        text = handle.read()
    updated = unset_key(text, table, key)
    if tomllib is not None:
        parsed = tomllib.loads(updated)
        table_values = parsed
        for part in table.split("."):
            table_values = table_values[part]
        assert key not in table_values
    with open(path, "w", encoding="utf-8") as handle:
        handle.write(updated)
    return updated


def read_string(path, table, key):
    """The string value of `table`.`key`, or None when the key is ABSENT.

    A scenario that has to put a key back needs the value that was really there, read the way the app
    reads it: a table-unaware grep would happily pick up a same-named key from another table. A file
    that cannot be read raises ConfigUnreadable instead of answering "absent": the caller must not
    overwrite what it could not read.
    """
    with open(path, encoding="utf-8") as handle:
        text = handle.read()
    parsed = _parsed(text)
    values = parsed
    for part in table.split("."):
        if not isinstance(values, dict) or part not in values:
            return None
        values = values[part]
    if not isinstance(values, dict) or key not in values:
        return None
    value = values[key]
    return value if isinstance(value, str) else None


class ConfigUnreadable(Exception):
    """The config could not be parsed, so nothing may be concluded about its keys.

    Distinct from "the key is absent": a caller that treats the two alike would overwrite a value it
    never read, and put "absent" back on exit -- deleting a setting the user had.
    """


def _parsed(text):
    """The parsed document, or a raise when there is no parser to read it with.

    Returning None here would be read by every caller as "no keys", which is the failure this raises
    instead: a scenario must stop before modifying a file it cannot read, not fall back to guessing.
    """
    if tomllib is None:
        raise ConfigUnreadable("no TOML parser (tomllib) is available to read the config")
    try:
        return tomllib.loads(text)
    except tomllib.TOMLDecodeError as error:
        raise ConfigUnreadable(f"the config is not valid TOML: {error}") from error


def _selftest():
    base = "[layout]\nthumbnails_enabled = false\n\n[windows]\nenabled = true\n"
    # A missing key is added inside its table.
    out = set_bool(base, "windows", "show_other_desktops", False)
    assert out.count("show_other_desktops") == 1
    parsed = _parsed(out)
    if parsed is not None:
        assert parsed["windows"]["show_other_desktops"] is False

    # An existing key is replaced, however it is written: plain, indented, or with a tab. The
    # indented case is the counter-example that a `startswith` filter gets wrong -- it survives,
    # the insert then adds a second key, and the app reads the defaults.
    for existing in (
        "show_other_desktops = false\n",
        "  show_other_desktops = true\n",
        "\tshow_other_desktops = true\n",
        "show_other_desktops= false   # hand-edited\n",
    ):
        text = base.replace("[windows]\n", "[windows]\n" + existing)
        out = set_bool(text, "windows", "show_other_desktops", True)
        assert out.count("show_other_desktops") == 1, out
        parsed = _parsed(out)
        if parsed is not None:
            assert parsed["windows"]["show_other_desktops"] is True
            assert parsed["windows"]["enabled"] is True

    # The same key in another table is left alone.
    other = "[a]\nshow_other_desktops = false\n\n[b]\nenabled = true\n"
    out = set_bool(other, "b", "show_other_desktops", True)
    parsed = _parsed(out)
    if parsed is not None:
        assert parsed["a"]["show_other_desktops"] is False
        assert parsed["b"]["show_other_desktops"] is True

    # A missing table is refused instead of appending a key nowhere.
    try:
        set_bool("[other]\nx = 1\n", "windows", "show_other_desktops", True)
    except ValueError:
        pass
    else:
        raise AssertionError("a missing table must be refused")

    # The whole key-bindings table is replaced while every other table survives. The
    # counter-example is a scenario that adds its own items without removing the user's list: the
    # app would then see duplicate triggers and silently drop one of them.
    base = (
        '[keyboard]\nmodifier = "command"\n\n'
        "[key_bindings]\nenabled = false\n\n"
        '[[key_bindings.items]]\naction = "quick.open_finder"\ntrigger = "option+e"\nenabled = true\n\n'
        "[windows]\nenabled = true\n"
    )
    out = replace_key_bindings(
        base,
        [
            ("quick.open_finder", "control+option+e"),
            ("quick.open_finder_small", "control+option+shift+e"),
        ],
    )
    assert out.count("[[key_bindings.items]]") == 2, out
    assert 'trigger = "option+e"' not in out, out
    parsed = _parsed(out)
    if parsed is not None:
        assert parsed["keyboard"]["modifier"] == "command"
        assert parsed["windows"]["enabled"] is True
        assert parsed["key_bindings"]["enabled"] is True
        assert [item["action"] for item in parsed["key_bindings"]["items"]] == [
            "quick.open_finder",
            "quick.open_finder_small",
        ]

    # --- string keys ---------------------------------------------------------------------------
    # panel-contrast rewrites the user's `appearance.theme` and puts it back. It has to survive the
    # three shapes a hand-edited config holds it in, and it must not touch any other key.
    appearance = (
        "[appearance]\ntheme = \"auto\"\npanel_material = \"frost\"\n\n"
        "[clipboard]\nenabled = false\n"
    )
    out = set_string(appearance, "appearance", "theme", "dark")
    assert out.count("theme = ") == 1, out
    parsed = _parsed(out)
    if parsed is not None:
        assert parsed["appearance"]["theme"] == "dark"
        assert parsed["appearance"]["panel_material"] == "frost"
        assert parsed["clipboard"]["enabled"] is False

    # An indented key is replaced, not duplicated -- the same counter-example set_bool carries.
    out = set_string("[appearance]\n  theme = \"auto\"\n", "appearance", "theme", "light")
    assert out.count("theme") == 1, out
    parsed = _parsed(out)
    if parsed is not None:
        assert parsed["appearance"]["theme"] == "light"

    # A missing key is added inside its table, not at the end of the file.
    out = set_string("[a]\nx = 1\n\n[b]\ny = 2\n", "a", "theme", "dark")
    assert out.index("theme") < out.index("[b]"), out

    # A missing table is refused instead of appending nowhere.
    try:
        set_string("[other]\nx = 1\n", "appearance", "theme", "dark")
    except ValueError:
        pass
    else:
        raise AssertionError("a missing table must be refused")

    # Removing a key restores "absent", which is what a config the scenario found without the key must
    # get back; the other keys of the table survive.
    out = unset_key('[appearance]\ntheme = "dark"\npanel_material = "frost"\n', "appearance", "theme")
    assert "theme" not in out, out
    assert 'panel_material = "frost"' in out
    parsed = _parsed(out)
    if parsed is not None:
        assert "theme" not in parsed["appearance"]

    # A value that needs escaping is written as a valid TOML string rather than breaking the file.
    out = set_string("[appearance]\n", "appearance", "theme", 'da"rk')
    parsed = _parsed(out)
    if parsed is not None:
        assert parsed["appearance"]["theme"] == 'da"rk', out

    # --- string-list keys ----------------------------------------------------------------------
    # The switcher's exclusion list is written and put back by the A2 scenario. All three shapes a
    # hand-edited config can hold it in must round-trip: absent, inline, and MULTI-LINE (the one a
    # line-oriented remover destroys).
    windows = "[windows]\nenabled = true\nshow_minimized = false\n"
    out = set_string_list(windows, "windows", "excluded_apps", ["com.apple.Safari", "com.example.Editor"])
    assert out.count("excluded_apps") == 1, out
    parsed = _parsed(out)
    if parsed is not None:
        assert parsed["windows"]["excluded_apps"] == ["com.apple.Safari", "com.example.Editor"]
        assert parsed["windows"]["show_minimized"] is False
    for existing in (
        'excluded_apps = ["com.example.one"]\n',
        'excluded_apps = [\n    "com.example.one",\n    "com.example.two",\n]\n',
        '  excluded_apps = [ "com.example.one" ]   # hand-edited\n',
        # A bracket inside a MULTI-LINE array's comment is text, not a closing bracket.
        'excluded_apps = [\n    "com.example.one", # ] not the end\n    "com.example.two",\n]\n',
    ):
        text = windows.replace("[windows]\n", "[windows]\n" + existing)
        out = set_string_list(text, "windows", "excluded_apps", ["com.example.replaced"])
        assert out.count("excluded_apps") == 1, out
        parsed = _parsed(out)
        if parsed is not None:
            assert parsed["windows"]["excluded_apps"] == ["com.example.replaced"]
            assert parsed["windows"]["show_minimized"] is False
    # The same key in another table is left alone.
    other = '[a]\nexcluded_apps = ["keep.me"]\n\n[b]\nenabled = true\n'
    out = set_string_list(other, "b", "excluded_apps", ["com.example.new"])
    parsed = _parsed(out)
    if parsed is not None:
        assert parsed["a"]["excluded_apps"] == ["keep.me"]
        assert parsed["b"]["excluded_apps"] == ["com.example.new"]
    # A missing table is refused.
    try:
        set_string_list("[other]\nx = 1\n", "windows", "excluded_apps", ["a.b"])
    except ValueError:
        pass
    else:
        raise AssertionError("a missing table must be refused for a list too")
    # Removing a multi-line array restores "absent" and leaves the rest of the table valid.
    multi = "[windows]\nenabled = true\nexcluded_apps = [\n  \"a.b\",\n]\nshow_minimized = true\n"
    out = unset_key(multi, "windows", "excluded_apps")
    assert "excluded_apps" not in out, out
    parsed = _parsed(out)
    if parsed is not None:
        assert parsed["windows"]["enabled"] is True
        assert parsed["windows"]["show_minimized"] is True
    # The balance check refuses a document the scanner would have corrupted, and accepts comments
    # that merely contain brackets.
    assert not _document_is_balanced('[windows]\nexcluded_apps = ["a.b"\n')
    assert _document_is_balanced('[windows]\n# ] ( a comment )\nenabled = true\n')
    assert _document_is_balanced('x = [\n  "a.b", # ] inside a comment\n]\n')

    # Quoted keys and quoted table names are legal TOML. A matcher that treated them as text left a
    # duplicate key (quoted form kept, bare form added) or missed the target table entirely.
    quoted_key = '[windows]\n"excluded_apps" = ["com.example.one"]\n'
    out = set_string_list(quoted_key, "windows", "excluded_apps", ["com.example.two"])
    assert out.count("excluded_apps") == 1, out
    parsed = _parsed(out)
    if parsed is not None:
        assert parsed["windows"]["excluded_apps"] == ["com.example.two"]
    quoted_table = '[windows]\nenabled = true\n\n["another"]\nexcluded_apps = ["keep.me"]\n'
    out = set_string_list(quoted_table, "windows", "excluded_apps", ["com.example.new"])
    parsed = _parsed(out)
    if parsed is not None:
        assert parsed["windows"]["excluded_apps"] == ["com.example.new"]
        assert parsed["another"]["excluded_apps"] == ["keep.me"], parsed

    # The whole-document guard: a write that changed another key (or lost a table) is refused.
    assert _first_unchanged_violation("[a]\nx = 1\n", "[a]\nx = 2\n", "a", "x") is None
    assert _first_unchanged_violation("[a]\nx = 1\ny = 3\n", "[a]\nx = 2\ny = 4\n", "a", "x") == "a.y"
    assert _first_unchanged_violation("[a]\nx = 1\n", "[b]\nx = 1\n", "a", "x") is not None

    # Without a parser the list writer refuses to touch the file at all (an unverifiable rewrite is
    # not acceptable), and the boolean/string writers keep their historical fallback.
    saved_parser = tomllib
    try:
        globals()["tomllib"] = None
        with tempfile.NamedTemporaryFile("w", suffix=".toml", delete=False) as handle:
            handle.write("[windows]\nenabled = true\n")
            no_parser_path = handle.name
        try:
            try:
                apply_list(no_parser_path, "windows", "excluded_apps", ["com.example.one"])
            except ConfigUnreadable:
                pass
            else:
                raise AssertionError("apply_list must refuse to write without a parser")
            with open(no_parser_path, encoding="utf-8") as handle:
                assert "excluded_apps" not in handle.read(), "the file must be untouched"
        finally:
            os.unlink(no_parser_path)
    finally:
        globals()["tomllib"] = saved_parser

    # A value that needs escaping stays valid TOML.
    out = set_string_list("[windows]\n", "windows", "excluded_apps", ['com.ex"ample'])
    parsed = _parsed(out)
    if parsed is not None:
        assert parsed["windows"]["excluded_apps"] == ['com.ex"ample']

    # Reading back what a restore has to put in place: a same-named key in another table must not be
    # mistaken for it, and an absent key must be distinguishable from an empty string.
    two_tables = '[appearance]\ntheme = "dark"\n\n[colors.dark]\ntheme = "not-the-setting"\n'
    with tempfile.NamedTemporaryFile("w", suffix=".toml", delete=False) as handle:
        handle.write(two_tables)
        scratch = handle.name
    try:
        assert read_string(scratch, "appearance", "theme") == "dark", read_string(scratch, "appearance", "theme")
        assert read_string(scratch, "appearance", "absent") is None
    finally:
        os.unlink(scratch)

    # An unreadable document is not "the key is absent": a caller that confused them would delete a
    # setting it never read. Both shapes are refused with the dedicated exception.
    with tempfile.NamedTemporaryFile("w", suffix=".toml", delete=False) as handle:
        handle.write("[appearance\ntheme = \"dark\"\n")
        broken = handle.name
    try:
        try:
            read_string(broken, "appearance", "theme")
        except ConfigUnreadable:
            pass
        else:
            raise AssertionError("an unparseable config must not be read as 'no such key'")
    finally:
        os.unlink(broken)

    print("config_switch selftest: ok")


if __name__ == "__main__":
    if sys.argv[1:2] == ["--selftest"]:
        _selftest()
    elif len(sys.argv) == 6 and sys.argv[1] == "--set":
        _, _, path, table, key, literal = sys.argv
        if literal not in ("true", "false"):
            raise SystemExit("the value must be true or false")
        apply(path, table, key, literal == "true")
    elif len(sys.argv) == 6 and sys.argv[1] == "--set-string":
        _, _, path, table, key, value = sys.argv
        apply_string(path, table, key, value)
    elif len(sys.argv) == 5 and sys.argv[1] == "--get-list":
        _, _, path, table, key = sys.argv
        value = read_list(path, table, key)
        if value is None:
            raise SystemExit(1)
        print("\n".join(value))
    elif len(sys.argv) == 5 and sys.argv[1] == "--get-string":
        _, _, path, table, key = sys.argv
        try:
            value = read_string(path, table, key)
        except ConfigUnreadable as error:
            print(f"config_switch: {error}", file=sys.stderr)
            raise SystemExit(4)
        if value is None:
            raise SystemExit(3)
        print(value)
    elif len(sys.argv) == 5 and sys.argv[1] == "--unset":
        _, _, path, table, key = sys.argv
        apply_unset(path, table, key)
    elif len(sys.argv) >= 4 and sys.argv[1] == "--set-bindings":
        path = sys.argv[2]
        items = []
        for spec in sys.argv[3:]:
            action, separator, trigger = spec.partition("=")
            if not separator or not action or not trigger:
                raise SystemExit(f"bad binding spec: {spec!r}")
            items.append((action, trigger))
        apply_key_bindings(path, items)
    else:
        raise SystemExit(
            "usage: config_switch.py --selftest | --set <path> <table> <key> <true|false> |"
            " --set-string <path> <table> <key> <value> | --unset <path> <table> <key> |"
            " --get-string <path> <table> <key> |"
            " --set-bindings <path> <action=trigger>..."
        )
