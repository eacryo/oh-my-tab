#!/bin/bash
# The inputs this selftest reads, one list, repo-relative. scripts/selftests.sh --changed uses this
# mapping to select it; a selftest without a declaration is refused rather than assumed irrelevant.
# SELFTEST_INPUTS=scripts/lib/demo-path.sh,scripts/demo-path-selftest.sh,scripts/dev-restart.sh
# Offline self-test for --demo= path forwarding: no build, no app, no launchd job.
#
# The symlink case is the point: collapsing `..` with shell logical-path rules before the kernel
# resolves a symlink can select a different TOML file. The helper must prefix the physical cwd and
# preserve the user's relative components for the kernel to resolve.

set -uo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
source "$script_dir/lib/demo-path.sh"
failures=0

pass() { printf 'ok   %s\n' "$1"; }
fail() { printf 'FAIL %s\n' "$1"; failures=$((failures + 1)); }

workdir=$(mktemp -d)
trap 'rm -rf "$workdir"' EXIT

absolute="/tmp/demo script.toml"
if [ "$(demo_path_for_launch "$absolute")" = "$absolute" ]; then
    pass "an absolute demo path passes through unchanged"
else
    fail "an absolute demo path was rewritten"
fi

(
    cd "$workdir" || exit 1
    out=$(demo_path_for_launch "demo script.toml")
    expected="$(pwd -P)/demo script.toml"
    [ "$out" = "$expected" ]
) && pass "a relative path is prefixed with the physical caller cwd, spaces intact" \
  || fail "a relative path lost its cwd prefix or whitespace"

mkdir -p "$workdir/real/sub" "$workdir/real/x"
printf '[[step]]\ndelay_ms = 1\n' >"$workdir/real/x/demo.toml"
ln -s "$workdir/real/sub" "$workdir/link"
(
    cd "$workdir/link" || exit 1
    out=$(demo_path_for_launch "../x/demo.toml")
    expected="$(pwd -P)/../x/demo.toml"
    [ "$out" = "$expected" ] && [ -f "$out" ]
) && pass "a symlinked cwd keeps .. for kernel resolution instead of selecting another script" \
  || fail "symlink + .. resolution would select another script"

if [ "$failures" -eq 0 ]; then
    echo "demo-path selftest: ok"
    exit 0
fi
echo "demo-path selftest: $failures failure(s)" >&2
exit 1
