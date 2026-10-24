#!/bin/bash
# Run clippy as a gate: warnings in production code fail, test-only warnings are reported.
#
# Why two classes: this repository's clippy baseline (before the branch this script came with) is 12
# warnings, all inside `#[cfg(test)]` code. Failing on them would make the gate red for reasons no
# change caused; ignoring clippy would let a real warning in a production path scroll past. So a
# warning is classified by **where it points**, not by its lint name: a diagnostic on a line inside a
# `#[cfg(test)]` item is test code, anything else is production. The split is computed from the
# file's own attributes, so a new lint in test code cannot be mistaken for production and a new lint
# in production code cannot hide behind a familiar-looking message.
#
# Usage: scripts/clippy-gate.sh
# Exit:  0 = no production warnings; 1 = at least one production warning; other = clippy failed.

set -uo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
repo_dir="$(dirname -- "$script_dir")"

log=$(mktemp -t oh-my-tab-clippy)
trap 'rm -f "$log"' EXIT

set +e
cargo clippy --manifest-path "$repo_dir/Cargo.toml" --all-targets --message-format=short 2>&1 | tee "$log"
status=${PIPESTATUS[0]}
set -e

if [ "$status" -ne 0 ]; then
    echo "clippy-gate: clippy did not complete" >&2
    exit "$status"
fi

# Classify each "path:line:col: warning: ..." against the file's own #[cfg(test)] regions.
python3 - "$repo_dir" "$log" <<'PY'
import re, sys, os
import re, sys, os, tempfile
repo, log = sys.argv[1], sys.argv[2]

diag = re.compile(r'^(\S[^:]*\.rs):(\d+):(\d+): warning: (.*)$')
NL = chr(10)
QUOTE = chr(34)
BACKSLASH = chr(92)


def strip_code(line):
    """The line with string literals removed and a trailing line comment dropped."""
    out = []
    in_str = False
    escaped = False
    for ch in line:
        if in_str:
            if escaped:
                escaped = False
            elif ch == BACKSLASH:
                escaped = True
            elif ch == QUOTE:
                in_str = False
            continue
        if ch == QUOTE:
            in_str = True
            continue
        if ch == '/' and out and out[-1] == '/':
            break
        out.append(ch)
    return ''.join(out)


def test_regions(path):
    """Line ranges that belong to a #[cfg(test)] item in this file.

    A `#[cfg(test)]` attribute attaches to the next item. Two shapes bound it: a braced item ends at
    the brace that closes it, and a declaration (static/const/use/type) ends at its first `;`. Braces
    inside string literals do not count, and a region that cannot be bounded is DROPPED rather than
    stretched to EOF: a bounded-but-wrong region exempts production warnings silently, which is the
    failure mode this function exists to avoid.
    """
    try:
        lines = open(path, encoding='utf-8', errors='replace').read().split(NL)
    except OSError:
        return []
    attr = re.compile(r'^\s*#\[cfg\(test\)\]\s*$')
    regions = []
    for i, line in enumerate(lines):
        if not attr.match(line):
            continue
        depth = 0
        started = False
        end = None
        for j in range(i + 1, len(lines)):
            code = strip_code(lines[j])
            if not started and ';' in code and '{' not in code:
                end = j + 1
                break
            opened = code.count('{')
            closed = code.count('}')
            depth += opened - closed
            if opened:
                started = True
            if started and depth <= 0:
                end = j + 1
                break
        if end is not None:
            regions.append((i + 1, end))
    return regions


def selftest():
    """Counter-examples for the classifier: an exempting classifier that is wrong is silent."""
    cases = [
        # A test-only static must end at its semicolon, not swallow the production fn after it.
        ('#[cfg(test)]' + NL + 'static X: u8 = 0;' + NL + 'fn production() {}' + NL, [(1, 2)],
         'test static'),
        # Braces inside a string literal must not move the end of the item.
        ('#[cfg(test)]' + NL + 'mod t {' + NL + '    fn a() { let s = "}{"; }' + NL
         + '    fn b() {}' + NL + '}' + NL + 'fn production() {}' + NL, [(1, 5)], 'string braces'),
        # A normal test module covers to its closing brace.
        ('#[cfg(test)]' + NL + 'mod t {' + NL + '    fn a() {}' + NL + '}' + NL
         + 'fn production() {}' + NL, [(1, 4)], 'plain module'),
    ]
    failures = 0
    for text, want, label in cases:
        handle = tempfile.NamedTemporaryFile('w', suffix='.rs', delete=False)
        handle.write(text)
        handle.close()
        try:
            got = test_regions(handle.name)
        finally:
            os.unlink(handle.name)
        if got != want:
            print(f'clippy-gate: classifier selftest FAILED for {label}: {got} != {want}',
                  file=sys.stderr)
            failures += 1
    return failures


if selftest():
    sys.exit(1)

cache = {}
production, testonly = [], 0
for line in open(log, encoding='utf-8', errors='replace'):
    m = diag.match(line.rstrip(NL))
    if not m:
        continue
    path, lineno, msg = m.group(1), int(m.group(2)), m.group(4)
    if not os.path.isabs(path):
        path = os.path.join(repo, path)
    if path not in cache:
        cache[path] = test_regions(path)
    if any(a <= lineno <= b for a, b in cache[path]):
        testonly += 1
    else:
        production.append(line.rstrip(NL))

if testonly:
    print(f'clippy-gate: {testonly} warning(s) inside #[cfg(test)] code (reported, not failing)')
if production:
    print('clippy-gate: production warnings:', file=sys.stderr)
    for p in production:
        print('  ' + p, file=sys.stderr)
    sys.exit(1)
print('clippy-gate: no production warnings')
PY
