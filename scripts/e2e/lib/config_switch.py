"""Rewrite one boolean key inside one table of the app's config, for the e2e scenarios.

A scenario that flips `windows.show_other_desktops` hands the app the user's own
`~/.config/oh-my-tab/config.toml`. Writing the key twice into `[windows]` makes the whole file
unparseable, the app silently falls back to its defaults, and the scenario then asserts against a
state it never set. `set_bool` replaces an existing occurrence of the key however it is indented and
touches no other table; `apply` refuses to write anything the TOML parser cannot read back.

`python3 scripts/e2e/lib/config_switch.py --selftest` runs the regression cases.
"""

from __future__ import annotations

import re
import sys

try:  # Python >= 3.11
    import tomllib
except ModuleNotFoundError:  # pragma: no cover - the rewrite is still duplicate-safe without it
    tomllib = None

_TABLE_HEADER = re.compile(r"^\[\s*([A-Za-z0-9_.\-]+)\s*\]$")


def _table_of(line):
    """The table a line opens, or None when the line is not a table header."""
    match = _TABLE_HEADER.match(line.strip())
    return match.group(1) if match else None


def _is_assignment(line, key):
    head, separator, _ = line.strip().partition("=")
    return bool(separator) and head.strip() == key


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


def _parsed(text):
    return tomllib.loads(text) if tomllib is not None else None


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

    print("config_switch selftest: ok")


if __name__ == "__main__":
    if sys.argv[1:2] == ["--selftest"]:
        _selftest()
    elif len(sys.argv) == 6 and sys.argv[1] == "--set":
        _, _, path, table, key, literal = sys.argv
        if literal not in ("true", "false"):
            raise SystemExit("the value must be true or false")
        apply(path, table, key, literal == "true")
    else:
        raise SystemExit(
            "usage: config_switch.py --selftest | --set <path> <table> <key> <true|false>"
        )
