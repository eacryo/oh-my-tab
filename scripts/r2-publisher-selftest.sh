#!/bin/bash
# The inputs this selftest reads, one list, repo-relative. scripts/selftests.sh --changed uses this
# mapping to select it; a selftest without a declaration is refused rather than assumed irrelevant.
# SELFTEST_INPUTS=scripts/r2-publisher-selftest.sh,tools/r2-publisher/
# Offline selftest for the R2 publisher's workspace independence.
#
# Why it exists: both release scripts publish through
#
#     cargo run --manifest-path tools/r2-publisher/Cargo.toml
#
# and that command resolves the package against the nearest ancestor workspace. Commit 98d2aa3 added
# the root [workspace] for the Finder extension without excluding the publisher, so the dev channel
# built, signed and appcasted the release and then died at the upload with
#
#     error: current package believes it's in a workspace when it's not
#
# and no release reached R2. The gate never looked, because release-dev-signing-selftest.sh stubs
# `cargo`. This selftest runs the real resolver instead, and it proves it can fail.
#
# Usage: scripts/r2-publisher-selftest.sh      Exit 0 = every case held.

set -uo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
repo_dir="$(dirname -- "$script_dir")"
publisher_dir="$repo_dir/tools/r2-publisher"
publisher_manifest="$publisher_dir/Cargo.toml"

failures=0
cases=0

pass() {
    printf 'ok   %s\n' "$1"
    cases=$((cases + 1))
}

fail() {
    printf 'FAIL %s\n' "$1" >&2
    failures=$((failures + 1))
}

workdir="$(mktemp -d -t r2-publisher-selftest)"
trap 'rm -rf "$workdir"' EXIT

for tool in cargo python3; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "r2-publisher-selftest: $tool not found; the publisher's workspace cannot be resolved" >&2
        exit 1
    }
done

# Manifest-only work: --no-deps never contacts a registry, and the offline switch keeps that explicit
# so the selftests tier stays honest on a machine with no network.
export CARGO_NET_OFFLINE=true

# <manifest> <out-prefix> -> exit status; metadata JSON in <prefix>.json, stderr in <prefix>.err.
metadata() {
    cargo metadata --manifest-path "$1" --no-deps --format-version 1 >"$2.json" 2>"$2.err"
}

# Prints "<workspace root>|<member count>|<first member's package name>" for a metadata file.
describe() {
    python3 - "$1" <<'PY'
import json, sys
data = json.load(open(sys.argv[1]))
name = "-"
for member in data["workspace_members"]:
    if "#" in member:
        name = member.split("#", 1)[1].rsplit("@", 1)[0]
        break
print("%s|%d|%s" % (data["workspace_root"].rstrip("/"), len(data["workspace_members"]), name))
PY
}

# --- case: the publisher resolves as its own workspace ------------------------------------------
# This is the assertion that would have failed on the 98d2aa3 tree, before any release was signed.
if metadata "$publisher_manifest" "$workdir/publisher"; then
    got="$(describe "$workdir/publisher.json")"
    want="$publisher_dir|1|oh-my-tab-r2-publisher"
    if [ "$got" = "$want" ]; then
        pass "publisher resolves as its own workspace ($got)"
    else
        fail "publisher must resolve standalone; got '$got', want '$want'"
    fi
else
    fail "cargo metadata failed for $publisher_manifest, so every --push would die at the upload: $(tail -n 3 "$workdir/publisher.err" | tr '\n' ' ')"
fi

# --- case: resolving from the publisher's own directory works too --------------------------------
# The release scripts pass --manifest-path, but a developer building the tool in place takes this
# path, and it consults the same ancestor workspace.
if ( cd "$publisher_dir" && cargo metadata --no-deps --format-version 1 >"$workdir/cwd.json" 2>"$workdir/cwd.err" ); then
    got="$(describe "$workdir/cwd.json")"
    want="$publisher_dir|1|oh-my-tab-r2-publisher"
    if [ "$got" = "$want" ]; then
        pass "publisher resolves from its own directory ($got)"
    else
        fail "from $publisher_dir the publisher must be its own workspace; got '$got', want '$want'"
    fi
else
    fail "cargo metadata failed inside $publisher_dir: $(tail -n 3 "$workdir/cwd.err" | tr '\n' ' ')"
fi

# --- case: the app workspace does not build the publisher ---------------------------------------
# Excluding it keeps the AWS client dependencies out of the app workspace, its lockfile and the gate.
if metadata "$repo_dir/Cargo.toml" "$workdir/app"; then
    if grep -q 'tools/r2-publisher' "$workdir/app.json"; then
        fail "the app workspace metadata mentions tools/r2-publisher; it must stay excluded (not a member or path dependency)"
    else
        pass "the app workspace does not absorb the publisher"
    fi
else
    fail "cargo metadata failed for the app workspace: $(tail -n 3 "$workdir/app.err" | tr '\n' ' ')"
fi

# --- counter-example: a workspace that does not exclude the publisher must be refused -----------
# Without this case the assertion above cannot tell "resolves standalone" from "the resolver was not
# exercised at all": the failure below is the exact error the 98d2aa3 tree produced.
tree="$workdir/counterexample"
mkdir -p "$tree/finder-ext/src" "$tree/tools/r2-publisher/src"
printf 'fn main() {}\n' >"$tree/finder-ext/src/main.rs"
printf 'fn main() {}\n' >"$tree/tools/r2-publisher/src/main.rs"
printf '[package]\nname = "finder-ext"\nversion = "0.1.0"\nedition = "2021"\n' >"$tree/finder-ext/Cargo.toml"
printf '[package]\nname = "publisher"\nversion = "0.1.0"\nedition = "2021"\n' >"$tree/tools/r2-publisher/Cargo.toml"
printf '[workspace]\nmembers = ["finder-ext"]\n' >"$tree/Cargo.toml"

if metadata "$tree/tools/r2-publisher/Cargo.toml" "$workdir/broken"; then
    fail "counter-example: a workspace that lists only finder-ext should refuse the publisher, but cargo accepted it"
elif grep -q "believes it's in a workspace" "$workdir/broken.err"; then
    pass "counter-example: a workspace that does not exclude the publisher is refused"
else
    fail "counter-example: expected the workspace-refusal error, got: $(tail -n 3 "$workdir/broken.err" | tr '\n' ' ')"
fi

printf '[workspace]\nmembers = ["finder-ext"]\nexclude = ["tools/r2-publisher"]\n' >"$tree/Cargo.toml"
if metadata "$tree/tools/r2-publisher/Cargo.toml" "$workdir/excluded"; then
    got="$(describe "$workdir/excluded.json")"
    want="$tree/tools/r2-publisher|1|publisher"
    if [ "$got" = "$want" ]; then
        pass "counter-example: adding the exclude makes the same package resolve standalone"
    else
        fail "counter-example: exclude should make the package standalone; got '$got', want '$want'"
    fi
else
    fail "counter-example: the exclude should resolve the package, but cargo failed: $(tail -n 3 "$workdir/excluded.err" | tr '\n' ' ')"
fi

# --- case: the release scripts still publish through the path checked above ---------------------
# A different invocation would leave this selftest guarding a command nobody runs.
for script in release-dev.sh release.sh; do
    if grep -qF -- 'cargo run --manifest-path tools/r2-publisher/Cargo.toml' "$repo_dir/scripts/$script"; then
        pass "scripts/$script still publishes through tools/r2-publisher/Cargo.toml"
    else
        fail "scripts/$script no longer runs 'cargo run --manifest-path tools/r2-publisher/Cargo.toml'; update this selftest so it guards the real publish path"
    fi
done

if [ "$failures" -ne 0 ]; then
    echo "r2-publisher-selftest: $failures failure(s) in $cases case(s)" >&2
    exit 1
fi
echo "r2-publisher-selftest ok: $cases case(s)"
