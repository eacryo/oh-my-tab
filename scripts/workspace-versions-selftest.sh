#!/bin/bash
# The inputs this selftest reads, one list, repo-relative. scripts/selftests.sh --changed uses this
# mapping to select it; a selftest without a declaration is refused rather than assumed irrelevant.
# SELFTEST_INPUTS=scripts/lib/workspace-versions.py,scripts/workspace-versions-selftest.sh,Cargo.toml,crates/,finder-ext/,tools/r2-publisher/Cargo.toml
# Offline self-test for scripts/lib/workspace-versions.py -- the check dev-restart.sh runs before
# it builds anything.
#
# Why this exists: the defect it guards is a version bump that touches the root Cargo.toml and not
# the workspace members. The .app takes its version from the root manifest alone and writes it into
# both Info.plists, so the mismatch is invisible in the product and was invisible in a restart. Each
# case below is one way that check can be wrong while still "passing":
#   - a member left at the old version must be refused (the defect itself);
#   - the comparison must not be satisfied by a version that only appears in a comment, a string or
#     a dependency table, which is what a hand-rolled `grep version = ` scan would accept;
#   - a manifest that cannot be read or parsed is exit 2 (could not decide), never agreement and
#     never a plain disagreement -- the caller has to be able to tell those apart;
#   - a workspace whose members are listed by a glob is refused rather than checked partially,
#     because reporting agreement for members it never read is the failure this check exists for.
#
# Usage: scripts/workspace-versions-selftest.sh      Exit 0 = every case held.

set -uo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
checker="$script_dir/lib/workspace-versions.py"
failures=0

pass() { printf 'ok   %s\n' "$1"; }
fail() { printf 'FAIL %s\n' "$1"; failures=$((failures + 1)); }

workdir=$(mktemp -d)
trap 'rm -rf "$workdir"' EXIT

# A minimal but real workspace: a root manifest that lists two members, and the two members.
make_workspace() {
    local root="$1" root_version="$2" ext_version="$3" proto_version="$4"
    mkdir -p "$root/finder-ext" "$root/crates/finder-protocol"
    printf '[workspace]\nmembers = ["finder-ext", "crates/finder-protocol"]\n\n[package]\nname = "oh-my-tab"\nversion = "%s"\nedition = "2021"\n' "$root_version" > "$root/Cargo.toml"
    printf '[package]\nname = "oh-my-tab-finder-ext"\nversion = "%s"\nedition = "2021"\n' "$ext_version" > "$root/finder-ext/Cargo.toml"
    printf '[package]\nname = "finder-protocol"\nversion = "%s"\nedition = "2021"\n' "$proto_version" > "$root/crates/finder-protocol/Cargo.toml"
}

# expect_exit <case> <want-status> <root>
expect_exit() {
    local name="$1" want="$2" root="$3" got
    python3 "$checker" --root "$root" >/dev/null 2>&1
    got=$?
    if [ "$got" -eq "$want" ]; then
        pass "$name"
    else
        fail "$name (expected exit $want, got $got)"
    fi
}

case_dir="$workdir/agree"
make_workspace "$case_dir" 0.2.6 0.2.6 0.2.6
expect_exit "a workspace whose members agree with the root passes" 0 "$case_dir"

# The defect: exactly what a root-only bump leaves behind.
case_dir="$workdir/ext-old"
make_workspace "$case_dir" 0.2.6 0.2.5 0.2.6
expect_exit "a member left at the previous version is refused" 1 "$case_dir"

case_dir="$workdir/proto-old"
make_workspace "$case_dir" 0.2.6 0.2.6 0.2.5
expect_exit "the protocol crate is checked too, not only the extension" 1 "$case_dir"

# What a naive scan gets wrong: the OLD version is the package's, and the new one appears only in
# text that is not the package version. A grep-based check reports agreement here.
case_dir="$workdir/decoy"
make_workspace "$case_dir" 0.2.6 0.2.6 0.2.6
cat > "$case_dir/finder-ext/Cargo.toml" <<'MANIFEST'
[package]
name = "oh-my-tab-finder-ext"
version = "0.2.5"
edition = "2021"
description = """
version = "0.2.6"
"""
# version = "0.2.6"

[dependencies]
objc2 = { version = "0.6" }
MANIFEST
expect_exit "a version in a string or a comment is not the package version" 1 "$case_dir"

# An inherited version is a form this check cannot read; it must not be silently skipped.
case_dir="$workdir/inherited"
make_workspace "$case_dir" 0.2.6 0.2.6 0.2.6
printf '[package]\nname = "finder-protocol"\nversion.workspace = true\n' > "$case_dir/crates/finder-protocol/Cargo.toml"
expect_exit "an inherited version is reported, not skipped" 1 "$case_dir"

case_dir="$workdir/malformed"
make_workspace "$case_dir" 0.2.6 0.2.6 0.2.6
printf '[package\nversion = "0.2.6"\n' > "$case_dir/crates/finder-protocol/Cargo.toml"
expect_exit "a malformed member manifest cannot decide, so it is exit 2" 2 "$case_dir"

case_dir="$workdir/missing"
make_workspace "$case_dir" 0.2.6 0.2.6 0.2.6
rm "$case_dir/crates/finder-protocol/Cargo.toml"
expect_exit "a missing member manifest cannot decide, so it is exit 2" 2 "$case_dir"

case_dir="$workdir/glob"
mkdir -p "$case_dir/crates/a"
printf '[workspace]\nmembers = ["crates/*"]\n\n[package]\nname = "oh-my-tab"\nversion = "0.2.6"\n' > "$case_dir/Cargo.toml"
printf '[package]\nname = "a"\nversion = "0.2.6"\n' > "$case_dir/crates/a/Cargo.toml"
expect_exit "a members glob is refused rather than partially checked" 2 "$case_dir"

case_dir="$workdir/no-workspace"
mkdir -p "$case_dir"
printf '[package]\nname = "oh-my-tab"\nversion = "0.2.6"\n' > "$case_dir/Cargo.toml"
expect_exit "a manifest with no [workspace] table cannot decide" 2 "$case_dir"

case_dir="$workdir/no-members"
mkdir -p "$case_dir"
printf '[workspace]\nmembers = []\n\n[package]\nname = "oh-my-tab"\nversion = "0.2.6"\n' > "$case_dir/Cargo.toml"
expect_exit "an empty members list cannot decide" 2 "$case_dir"

# The ROOT manifest failing is a different code path from a member failing, and it is the one where
# a wrong answer is worst: without a basis to compare against, a checker that reports "members
# disagree" tells the user to bump versions that may be fine. Both root failures are exit 2.
case_dir="$workdir/root-no-version"
make_workspace "$case_dir" 0.2.6 0.2.6 0.2.6
printf '[workspace]\nmembers = ["finder-ext", "crates/finder-protocol"]\n\n[package]\nname = "oh-my-tab"\n'> "$case_dir/Cargo.toml"
expect_exit "a root manifest with no version cannot decide" 2 "$case_dir"

case_dir="$workdir/root-inherited"
make_workspace "$case_dir" 0.2.6 0.2.6 0.2.6
printf '[workspace]\nmembers = ["finder-ext", "crates/finder-protocol"]\n\n[package]\nname = "oh-my-tab"\nversion.workspace = true\n'> "$case_dir/Cargo.toml"
expect_exit "a root manifest with an inherited version cannot decide" 2 "$case_dir"

# A python3 without tomllib (macOS ships 3.9 at /usr/bin/python3) must say so and exit 2. Exiting 1
# here would be read as "the workspace disagrees", for a workspace that may be perfectly fine.
#
# The simulation must let the CHECKER decide. A stub that spots `import tomllib` and exits 2 on its
# own would pass this case even with the checker's try/except deleted -- it would test the stub. So
# the REAL interpreter runs the REAL checker, with PYTHONPATH pointing at a directory whose
# sitecustomize.py makes `import tomllib` raise ModuleNotFoundError: the only difference from a 3.9
# machine. Exit code and diagnostic therefore come from the code under test.
#
# The interpreter is resolved BEFORE that directory joins PYTHONPATH, and is what the cases run.
real_python="$(command -v python3)"
nolib="$workdir/nolib"
mkdir -p "$nolib"
cat > "$nolib/sitecustomize.py" <<'NOPY'
# Makes this interpreter behave like a python3 without tomllib: the module exists in the standard
# library here, so it is removed from the import system before the script runs.
import sys
sys.modules.pop("tomllib", None)
import importlib.abc
import importlib.machinery

class _NoTomllib(importlib.abc.MetaPathFinder):
    def find_spec(self, name, path=None, target=None):
        if name == "tomllib" or name.startswith("tomllib."):
            raise ModuleNotFoundError("No module named %r" % name)
        return None

sys.meta_path.insert(0, _NoTomllib())
NOPY
case_dir="$workdir/agree-for-nolib"
make_workspace "$case_dir" 0.2.6 0.2.6 0.2.6
no_tomllib_status=0
PYTHONPATH="$nolib" "$real_python" "$checker" --root "$case_dir" >/dev/null 2>&1 || no_tomllib_status=$?
if [ "$no_tomllib_status" -eq 2 ]; then
    pass "a python3 without tomllib cannot decide, so it is exit 2"
else
    fail "a python3 without tomllib cannot decide, so it is exit 2 (got $no_tomllib_status)"
fi
nolib_message="$(PYTHONPATH="$nolib" "$real_python" "$checker" --root "$case_dir" 2>&1 >/dev/null)"
if printf '%s' "$nolib_message" | grep -q "no tomllib"; then
    pass "the missing TOML reader is named in the diagnostic"
else
    fail "the missing TOML reader is named in the diagnostic (got: $nolib_message)"
fi
# Counter-example: the injection really removes tomllib, so the case above is about a missing
# TOML reader and not about some other failure. A plain import through the same injection fails,
# and through the ordinary interpreter succeeds.
direct_tomllib_status=0
PYTHONPATH="$nolib" "$real_python" -c 'import tomllib' >/dev/null 2>&1 || direct_tomllib_status=$?
if [ "$direct_tomllib_status" -ne 0 ]; then
    pass "the injection really makes tomllib unimportable, so the case above is not vacuous"
else
    fail "the injection really makes tomllib unimportable, so the case above is not vacuous"
fi
if "$real_python" -c 'import tomllib' >/dev/null 2>&1; then
    pass "the same interpreter without the injection does import tomllib"
else
    fail "the same interpreter without the injection does import tomllib"
fi

# The real repository must pass: a checker that refuses everything would satisfy every case above.
if python3 "$checker" --root "$(dirname "$script_dir")" >/dev/null 2>&1; then
    pass "this repository's workspace agrees"
else
    fail "this repository's workspace agrees"
fi

if [ "$failures" -eq 0 ]; then
    echo "workspace-versions selftest: all cases held"
    exit 0
fi
echo "workspace-versions selftest: $failures case(s) failed" >&2
exit 1
