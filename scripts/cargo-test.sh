#!/bin/bash
# Run the Rust test suite, refusing a run that executed nothing.
#
# Why this exists: `cargo test <filter>` exits 0 when the filter matches no test at all
# (measured: `running 0 tests ... ok`), so a typo in a filter reports success for a suite that
# never ran. A gate that cannot decide is not a pass, which is the repository rule; this wrapper
# makes the zero-match case a failure instead of a silent success.
#
# Usage:
#   scripts/cargo-test.sh                      # the whole suite (the handoff gate)
#   scripts/cargo-test.sh --lib logger::       # a filter for iteration
#   scripts/cargo-test.sh --require-match --lib logger::
#
# Every argument that is not one of this wrapper's own switches is passed to `cargo test`
# unchanged.
#
# The guard, and why it is two rules rather than one:
#
# * The whole invocation must execute at least one test. This is the original rule and it is
#   unconditional: a run whose every binary printed `0 passed` did not test anything.
# * With --require-match, a filter must also match something in the crate that is actually under
#   test (the `oh-my-tab` lib test binary). A filter legitimately matches one target and nothing
#   in the others -- a workspace has several packages, and doc-test targets run zero tests by
#   design -- so "every binary ran a test" is NOT required and would reject valid runs. What is
#   rejected is the case this wrapper exists for: the crate's own tests all filtered out while
#   some other package's output made the total look non-zero.
#
# A run that reaches a verdict prints one line the gate reads:
#   cargo-test ok: <n> test(s) executed
# and exits 0; anything else exits non-zero.

set -uo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(dirname -- "$script_dir")

require_match=0
cargo_args=()
for arg in "$@"; do
    case "$arg" in
        --require-match) require_match=1 ;;
        *) cargo_args+=("$arg") ;;
    esac
done

output_file=$(mktemp -t oh-my-tab-test-output)
trap 'rm -f "$output_file"' EXIT

# CARGO may be overridden so the wrapper's own selftest can drive it against a stub.
set +e
"${CARGO:-cargo}" test --manifest-path "$repo_dir/Cargo.toml" "${cargo_args[@]:-}" 2>&1 | tee "$output_file"
cargo_status=${PIPESTATUS[0]}
set -e

# One line per test binary: "name<TAB>passed<TAB>failed<TAB>ignored", parsed from the block that
# `cargo test` prints as
#     Running unittests src/lib.rs (target/debug/deps/oh_my_tab-abc)
#     ...
#     test result: ok. 1269 passed; 0 failed; 27 ignored; ...
# The crate under test is identified by the binary path containing `oh_my_tab-` for the lib target.
binary_summary=$(awk '
    /^ *Running |^ *Doc-tests / { target = $0 }
    /^test result:/ {
        passed = 0; failed = 0; ignored = 0
        for (i = 1; i <= NF; i++) {
            if ($i == "passed;")  passed  = $(i-1)
            if ($i == "failed;")  failed  = $(i-1)
            if ($i == "ignored;") ignored = $(i-1)
        }
        print target "\t" passed "\t" failed "\t" ignored
    }
' "$output_file")

if [ "$cargo_status" -ne 0 ]; then
    exit "$cargo_status"
fi

executed=$(printf '%s\n' "$binary_summary" | awk -F'\t' 'NF >= 2 { total += $2 } END { print total + 0 }')

if [ "$executed" -eq 0 ]; then
    echo "cargo-test FAILED: the run executed no test at all (a filter that matched nothing still exits 0)" >&2
    exit 1
fi

if [ "$require_match" -eq 1 ]; then
    # The main crate's lib test binary. `unittests src/lib.rs` names the target; the workspace's
    # own crate is the only one whose target line says src/lib.rs for the root package.
    main_passed=$(printf '%s\n' "$binary_summary" |
        awk -F'\t' '/unittests src\/lib.rs/ { total += $2 } END { print total + 0 }')
    if [ "$main_passed" -eq 0 ]; then
        echo "cargo-test FAILED: --require-match was given but the oh-my-tab lib test binary executed 0 tests" >&2
        echo "  (other targets may still have run; the filter matched nothing in the crate under test)" >&2
        printf '%s\n' "$binary_summary" | awk -F'\t' 'NF >= 2 && $2 > 0 { print "  ran " $2 " in: " $1 }' >&2
        exit 1
    fi
fi

echo "cargo-test ok: $executed test(s) executed"
