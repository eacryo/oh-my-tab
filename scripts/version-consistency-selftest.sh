#!/bin/bash
# Offline selftest for the workspace release-version agreement.
#
# Why it exists: every packaging input (the app's Info.plist, the Finder extension's nested
# Info.plist, the Sparkle appcast, the Homebrew cask) takes its version from the ROOT Cargo.toml,
# but the two other workspace members carry their own version field that nothing rewrites. Bumping
# only the root manifest therefore ships an .app whose embedded crates claim another version, and no
# build, bundle or publish step notices. src/version_consistency.rs asserts the same fact from the
# compiled side; this checks the manifests with cargo metadata, so a member added under crates/ is
# followed by the metadata rather than by a transcribed list.
#
# The counter-example matters more than the happy path: a check that reads no version at all would
# report agreement for every tree, so the mismatch case builds a throwaway workspace on purpose.
#
#   scripts/version-consistency-selftest.sh
#
# Exit code: 0 = every case behaved as specified; non-zero when a case did not.

set -uo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
repo_dir="$(dirname -- "$script_dir")"
failures=0
cases=0

pass() {
    cases=$((cases + 1))
    echo "ok   $*"
}

fail() {
    echo "version-consistency-selftest: FAIL: $*" >&2
    failures=$((failures + 1))
}

workdir="$(mktemp -d -t version-consistency-selftest)"
trap 'rm -rf "$workdir"' EXIT

# Compare every workspace member's manifest version against the root package's.
#
# Prints one line per disagreement and exits 1 when there is one, 0 when every member agrees, and
# 2 when the metadata itself could not be read (an unreadable tree is never reported as agreement).
check_tree() {
    local manifest="$1" expected="$2" metadata="$3"
    if ! cargo metadata --manifest-path "$manifest" --no-deps --format-version 1 >"$metadata" 2>"$metadata.err"; then
        echo "cannot read cargo metadata for $manifest: $(tail -n 2 "$metadata.err" | tr '\n' ' ')" >&2
        return 2
    fi
    python3 - "$metadata" "$expected" <<'PY'
import json, sys
with open(sys.argv[1]) as fh:
    meta = json.load(fh)
expected = sys.argv[2]
members = meta.get("packages", [])
if not members:
    sys.exit(3)
disagreements = [p for p in members if p.get("version") != expected]
for p in disagreements:
    print(f"{p.get('name')}: {p.get('version')} != {expected}")
sys.exit(1 if disagreements else 0)
PY
}

# --- Case 1: the real repository is consistent, and the check actually sees every member. --------
real_expected="$(awk -F'"' '/^version/ {print $2; exit}' "$repo_dir/Cargo.toml")"
if [ -z "$real_expected" ]; then
    fail "could not read the root Cargo.toml version"
else
    if check_tree "$repo_dir/Cargo.toml" "$real_expected" "$workdir/real.json"; then
        pass "the real workspace members all carry the root version ($real_expected)"
    else
        fail "the real workspace disagrees with the root Cargo.toml version ($real_expected); run: scripts/version-consistency-selftest.sh"
        check_tree "$repo_dir/Cargo.toml" "$real_expected" "$workdir/real.json" 2>&1 | sed 's/^/     /' >&2 || true
    fi
    # The check must not agree for whatever reason: reading it with a wrong expected version has to
    # report every member, which proves the comparison saw them.
    if check_tree "$repo_dir/Cargo.toml" "0.0.0-not-a-real-version" "$workdir/wrong.json" >/dev/null; then
        fail "a wrong expected version still reported agreement, so the check reads no version at all"
    else
        pass "a deliberately wrong expected version is reported, so the check sees the real members"
    fi
fi

# --- Case 2: a throwaway workspace where one member disagrees must be caught. --------------------
mismatch_tree="$workdir/mismatch"
mkdir -p "$mismatch_tree/app" "$mismatch_tree/plugin"
cat >"$mismatch_tree/Cargo.toml" <<'TOML'
[workspace]
members = ["app", "plugin"]
TOML
# cargo metadata needs a target, so each throwaway package gets an empty lib.rs.
mkdir -p "$mismatch_tree/app/src" "$mismatch_tree/plugin/src"
: >"$mismatch_tree/app/src/lib.rs"
: >"$mismatch_tree/plugin/src/lib.rs"
cat >"$mismatch_tree/app/Cargo.toml" <<'TOML'
[package]
name = "app"
version = "9.1.0"
edition = "2021"
TOML
cat >"$mismatch_tree/plugin/Cargo.toml" <<'TOML'
[package]
name = "plugin"
version = "9.0.0"
edition = "2021"
TOML
case2_out="$(check_tree "$mismatch_tree/app/Cargo.toml" "9.1.0" "$workdir/mismatch.json")"
case2_status=$?
cases=$((cases + 1))
if [ "$case2_status" -eq 1 ] && printf '%s' "$case2_out" | grep -q 'plugin: 9.0.0 != 9.1.0'; then
    echo "ok   a disagreeing member in a throwaway workspace is reported"
else
    fail "a disagreeing member was not reported (status $case2_status, output: $case2_out)"
fi

# --- Case 3: the same throwaway workspace agrees once the member is bumped, so case 2 was about ---
# --- the mismatch and not about a reader that always fails. --------------------------------------
sed -i '' 's/version = "9.0.0"/version = "9.1.0"/' "$mismatch_tree/plugin/Cargo.toml" 2>/dev/null \
    || sed -i 's/version = "9.0.0"/version = "9.1.0"/' "$mismatch_tree/plugin/Cargo.toml"
if check_tree "$mismatch_tree/app/Cargo.toml" "9.1.0" "$workdir/agreed.json" >/dev/null; then
    pass "the throwaway workspace agrees once the member is bumped"
else
    fail "the throwaway workspace still reported a mismatch after the bump"
fi

# --- Case 3b: bundle.sh refuses to package a workspace whose members disagree. ------------------
# bundle.sh defines workspace_version_mismatches for exactly this: the packaging path's own gate is
# exercised as a function against a throwaway workspace, instead of running (and paying for) a bundle.
gate_snippet="$(sed -n '/^workspace_version_mismatches() {/,/^}$/p' "$repo_dir/scripts/bundle.sh")"
cases=$((cases + 1))
if [ -z "$gate_snippet" ]; then
    fail "bundle.sh no longer defines workspace_version_mismatches; the packaging path lost its check"
else
    bundle_tree="$workdir/bundle"
    mkdir -p "$bundle_tree/app/src" "$bundle_tree/plugin/src"
    : >"$bundle_tree/app/src/lib.rs"
    : >"$bundle_tree/plugin/src/lib.rs"
    printf '[workspace]\nmembers = ["app", "plugin"]\n' >"$bundle_tree/Cargo.toml"
    printf '[package]\nname = "app"\nversion = "9.1.0"\nedition = "2021"\n' >"$bundle_tree/app/Cargo.toml"
    printf '[package]\nname = "plugin"\nversion = "9.0.0"\nedition = "2021"\n' >"$bundle_tree/plugin/Cargo.toml"
    printf '%s\n' "$gate_snippet" >"$bundle_tree/gate-fn.sh"
    gate_fn="$bundle_tree/gate-fn.sh"
    # A disagreeing workspace must be refused, and the refusal must name the member.
    bundle_out="$(cd "$bundle_tree/app" && bash -c ". \"$gate_fn\"; workspace_version_mismatches 9.1.0" 2>&1)"
    bundle_status=$?
    if [ "$bundle_status" -ne 0 ] && printf '%s' "$bundle_out" | grep -q 'plugin: 9.0.0 (expected 9.1.0)'; then
        pass "bundle.sh's version gate refuses a disagreeing workspace and names the member"
    else
        fail "bundle.sh's version gate did not refuse a disagreeing workspace (status $bundle_status, output: $bundle_out)"
    fi
    # ...and must accept it once the member is bumped, so the case above is about the mismatch.
    sed -i '' 's/version = "9.0.0"/version = "9.1.0"/' "$bundle_tree/plugin/Cargo.toml" 2>/dev/null \
        || sed -i 's/version = "9.0.0"/version = "9.1.0"/' "$bundle_tree/plugin/Cargo.toml"
    if (cd "$bundle_tree/app" && bash -c ". \"$gate_fn\"; workspace_version_mismatches 9.1.0" >/dev/null 2>&1); then
        pass "bundle.sh's version gate accepts a consistent workspace"
    else
        fail "bundle.sh's version gate rejected a consistent workspace"
    fi
fi

# --- Case 3c: the comparison itself failing is a refusal, never agreement. ----------------------
# Caught in review: the gate ran awk inside `if !`, which suspends set -e, so a failing awk left
# `mismatches` empty and the function returned success -- "could not compare" read as "agrees".
if [ -n "${gate_fn:-}" ] && [ -f "$gate_fn" ]; then
    cases=$((cases + 1))
    fake_bin="$workdir/fake-bin"
    mkdir -p "$fake_bin"
    printf '#!/bin/bash\nexit 3\n' >"$fake_bin/awk"
    chmod +x "$fake_bin/awk"
    awk_fail_out="$(cd "$bundle_tree/app" && PATH="$fake_bin:$PATH" bash -c ". \"$gate_fn\"; workspace_version_mismatches 9.1.0" 2>&1)"
    awk_fail_status=$?
    if [ "$awk_fail_status" -ne 0 ] && printf '%s' "$awk_fail_out" | grep -q 'could not compare workspace versions'; then
        pass "a failing comparison refuses instead of reporting agreement"
    else
        fail "a failing comparison was reported as agreement (status $awk_fail_status, output: $awk_fail_out)"
    fi
fi

# --- Case 4: an unreadable tree is never reported as agreement. ---------------------------------
unreadable_tree="$workdir/unreadable"
mkdir -p "$unreadable_tree"
printf '[package]\nname = "broken"\nversion = \n' >"$unreadable_tree/Cargo.toml"
if check_tree "$unreadable_tree/Cargo.toml" "9.1.0" "$workdir/broken.json" >/dev/null 2>&1; then
    fail "an unreadable manifest was reported as agreement"
else
    pass "an unreadable manifest is not reported as agreement"
fi

# --- Case 5: an empty package list is a refusal, not agreement. ---------------------------------
mkdir -p "$workdir/empty"
printf '[workspace]\nmembers = []\n' >"$workdir/empty/Cargo.toml"
# A workspace with no members is what cargo reports as an empty package list.
if check_tree "$workdir/empty/Cargo.toml" "9.1.0" "$workdir/empty.json" >/dev/null 2>&1; then
    fail "an empty package list was reported as agreement"
else
    pass "an empty package list is a refusal"
fi

echo
if [ "$failures" -ne 0 ]; then
    echo "version-consistency-selftest: $failures failure(s) of $((cases + failures)) case(s)" >&2
    exit 1
fi
echo "version-consistency-selftest ok: $cases case(s)"
