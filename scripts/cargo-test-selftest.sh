#!/bin/bash
# The inputs this selftest reads, one list, repo-relative. scripts/selftests.sh --changed uses this
# mapping to select it; a selftest without a declaration is refused rather than assumed irrelevant.
# SELFTEST_INPUTS=scripts/cargo-test.sh,scripts/cargo-test-selftest.sh
# Offline selftest for scripts/cargo-test.sh.
#
# Why it exists: the wrapper is the instrument the whole gate rests on, and its promise -- "a run
# that executed nothing is not a pass" -- can only be checked with inputs the real suite never
# produces. This runs the shipped wrapper against a stub `cargo` on PATH, so every case is
# deterministic, offline and cannot touch the real target directory.
#
#   scripts/cargo-test-selftest.sh
#
# Exit code: 0 = every case behaved as specified; non-zero when a case did not.

set -uo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
repo_dir="$(dirname -- "$script_dir")"
wrapper="$script_dir/cargo-test.sh"
failures=0
cases=0

fail() {
    echo "cargo-test-selftest: FAIL: $*" >&2
    failures=$((failures + 1))
}

ok() {
    cases=$((cases + 1))
}

workdir="$(mktemp -d -t cargo-test-selftest)"
trap 'rm -rf "$workdir"' EXIT

# A stub cargo whose output is the fixture file named by $STUB_FIXTURE, and whose exit status is
# $STUB_STATUS. It also reports the argv it was given, so the wrapper's pass-through can be checked.
make_stub() {
    cat > "$workdir/cargo" <<'STUB'
#!/bin/bash
if [ -n "${STUB_ARGV_FILE:-}" ]; then
    printf '%s\n' "$@" > "$STUB_ARGV_FILE"
fi
[ -n "${STUB_FIXTURE:-}" ] && cat "$STUB_FIXTURE"
exit "${STUB_STATUS:-0}"
STUB
    chmod +x "$workdir/cargo"
}

fixture() {  # <name> <content>
    printf '%s' "$2" > "$workdir/$1"
    printf '%s' "$workdir/$1"
}

run() {  # <fixture-path> <status> [wrapper args...] -> captures status in $status_out
    local fixture="$1" wanted="$2"
    shift 2
    STUB_FIXTURE="$fixture" STUB_STATUS="$wanted" STUB_ARGV_FILE="$workdir/argv" \
        CARGO="$workdir/cargo" "$wrapper" "$@" > "$workdir/out" 2>&1
    status_out=$?
}

make_stub

# A whole-suite-shaped run: the main crate ran tests.
suite_pass=$(fixture suite_pass '   Compiling oh-my-tab v0.2.5
     Running unittests src/lib.rs (target/debug/deps/oh_my_tab-1111)

running 3 tests
test a ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

   Doc-tests oh_my_tab

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
')

# A filter that matched nothing in any binary.
nothing=$(fixture nothing '     Running unittests src/lib.rs (target/debug/deps/oh_my_tab-1111)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1273 filtered out; finished in 0.00s
')

# The hole the wrapper exists for: the crate under test matched nothing, but another package (and a
# doc-test target) contributed passes, so a naive sum is non-zero.
other_crate_only=$(fixture other_crate_only '     Running unittests src/lib.rs (target/debug/deps/oh_my_tab-1111)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1273 filtered out; finished in 0.00s

     Running unittests crates/finder-protocol/src/lib.rs (target/debug/deps/finder_protocol-2222)

running 3 tests
test x ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

   Doc-tests oh_my_tab

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
')

# A legitimate filtered run: the crate under test matched, other targets did not. This is the
# positive case: --require-match must NOT reject it.
filtered_legit=$(fixture filtered_legit '     Running unittests src/lib.rs (target/debug/deps/oh_my_tab-1111)

running 10 tests
test a ... ok

test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 1263 filtered out; finished in 0.01s

     Running unittests crates/finder-protocol/src/lib.rs (target/debug/deps/finder_protocol-2222)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 12 filtered out; finished in 0.00s
')

# --- case: the whole suite passed ---------------------------------------------------------------
run "$suite_pass" 0
if [ "$status_out" -eq 0 ] && grep -q "cargo-test ok: 3 test(s)" "$workdir/out"; then
    ok
else
    fail "a passing suite should exit 0 and report 3 executed (got $status_out: $(tail -1 "$workdir/out"))"
fi

# --- case: nothing ran -> fail ------------------------------------------------------------------
run "$nothing" 0
if [ "$status_out" -ne 0 ] && grep -q "executed no test at all" "$workdir/out"; then
    ok
else
    fail "a run that executed no test must fail regardless of cargo's exit status (got $status_out)"
fi

# --- case: only another crate ran -> fail, with and without --require-match ----------------------
# Without --require-match the whole-invocation rule is what decides, and it is satisfied (3 > 0);
# this is the documented behaviour, and the case records it so a change to it is visible.
run "$other_crate_only" 0
if [ "$status_out" -eq 0 ]; then
    ok
else
    fail "without --require-match a run where another crate ran is not the zero test case (got $status_out)"
fi

run "$other_crate_only" 0 --require-match
if [ "$status_out" -ne 0 ] && grep -q "oh-my-tab lib test binary executed 0 tests" "$workdir/out"; then
    ok
else
    fail "--require-match must reject a filter that matched nothing in the crate under test (got $status_out)"
fi

# --- case: a legitimate filtered run is NOT rejected --------------------------------------------
run "$filtered_legit" 0 --require-match --lib logger::
if [ "$status_out" -eq 0 ] && grep -q "cargo-test ok: 10 test(s)" "$workdir/out"; then
    ok
else
    fail "--require-match must accept a filter that matched the crate under test while others ran none (got $status_out)"
fi

# --- case: cargo itself failed -> exit status preserved -----------------------------------------
run "$suite_pass" 101
if [ "$status_out" -eq 101 ]; then
    ok
else
    fail "a failing cargo run must pass its exit status through (got $status_out, wanted 101)"
fi

# --- case: a panic before any `test result:` line ----------------------------------------------
panic_fixture=$(fixture panic '     Running unittests src/lib.rs (target/debug/deps/oh_my_tab-1111)

running 1 test
test boom ... FAILED

thread '"'"'boom'"'"' panicked at src/lib.rs:1:1
')
run "$panic_fixture" 0
if [ "$status_out" -ne 0 ] && grep -q "executed no test at all" "$workdir/out"; then
    ok
else
    fail "a binary that died without a test-result line executed no test and must fail (got $status_out)"
fi

# --- case: the wrapper's own switches are not forwarded to cargo --------------------------------
run "$filtered_legit" 0 --require-match --lib logger::
if ! grep -qx -- "--require-match" "$workdir/argv"; then
    ok
else
    fail "--require-match is the wrapper's switch and must not reach cargo"
fi
if grep -qx -- "--lib" "$workdir/argv"; then
    ok
else
    fail "the filter arguments must still reach cargo unchanged"
fi

if [ "$failures" -ne 0 ]; then
    echo "cargo-test-selftest: $failures failure(s) in $cases case(s)" >&2
    exit 1
fi
echo "cargo-test-selftest ok: $cases case(s)"
