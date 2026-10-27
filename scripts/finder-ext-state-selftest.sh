#!/bin/bash
# Selftest for scripts/lib/finder-ext-state.sh: the transaction TOML the dev script writes must be
# the same document the Rust recovery module parses (testdata/finder-ext-transaction.toml), and the
# marker decision must not read any other election state as "enabled".
#
# Offline: sources the library only, writes nothing, touches no UserDefaults and no election.

set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo_dir="$(dirname "$here")"
# shellcheck source=/dev/null
source "$repo_dir/scripts/lib/finder-ext-state.sh"

failed=0
pass() { echo "finder-ext-state-selftest: PASS: $*"; }
fail() { echo "finder-ext-state-selftest: FAIL: $*" >&2; failed=$((failed + 1)); }

fixture="$repo_dir/testdata/finder-ext-transaction.toml"
expected="$(cat "$fixture")"
actual="$(omt_finder_ext_transaction_toml 20260101000000 20260102000000 com.example.app.FinderSync true)"
if [ "$actual" = "$expected" ]; then
    pass "the transaction TOML matches the shared fixture byte for byte"
else
    fail "the transaction TOML differs from $fixture"
    printf 'expected:\n%s\nactual:\n%s\n' "$expected" "$actual" >&2
fi

actual_false="$(omt_finder_ext_transaction_toml 1 2 com.example.app.FinderSync false)"
case "$actual_false" in
    *"enabled_before = false"*) pass "a disabled election is written as false" ;;
    *) fail "a disabled election was not written as false: $actual_false" ;;
esac
if omt_finder_ext_transaction_toml 1 2 id maybe >/dev/null 2>&1; then
    fail "an unknown enabled value must be refused"
else
    pass "an unknown enabled value is refused"
fi
if omt_finder_ext_transaction_toml "" 2 id true >/dev/null 2>&1; then
    fail "an empty source build must be refused"
else
    pass "an empty source build is refused"
fi

omt_finder_ext_marker_enabled '+    id(1)' && pass "'+' reads as enabled" || fail "'+' did not read as enabled"
for marker in '-' '!' '=' '?' ''; do
    if omt_finder_ext_marker_enabled "$marker    id(1)"; then
        fail "marker '$marker' must not read as enabled"
    else
        pass "marker '$marker' does not read as enabled"
    fi
done

if [ "$failed" -ne 0 ]; then
    echo "finder-ext-state-selftest: $failed failure(s)" >&2
    exit 1
fi
echo "finder-ext-state-selftest: all checks passed"
