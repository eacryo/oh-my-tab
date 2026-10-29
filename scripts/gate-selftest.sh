#!/bin/bash
# The inputs this selftest reads, one list, repo-relative. scripts/selftests.sh --changed
# uses this mapping to select it; a selftest without one is refused rather than assumed irrelevant.
# SELFTEST_INPUTS=scripts/gate.sh,scripts/gate-selftest.sh,scripts/m2.sh,scripts/e2e/run-all.sh
# Offline selftest for scripts/gate.sh outcome vocabulary and exit-status propagation.
#
# Why it exists: the gate claims that a step which could not decide is never reported as a
# pass. That claim can only be checked with failures, and the real steps are expensive and
# mostly green -- so this drives the shipped gate.sh against stub steps in a temporary
# repository, one per outcome, and asserts the printed lines and the exit status.
#
#   scripts/gate-selftest.sh
#
# Exit code: 0 = every case behaved as specified; non-zero when a case did not.

set -uo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
gate="$script_dir/gate.sh"
failures=0
cases=0

fail() {
    echo "gate-selftest: FAIL: $*" >&2
    failures=$((failures + 1))
}

ok() { cases=$((cases + 1)); }

workdir="$(mktemp -d -t gate-selftest)"
# trap off

repo="$workdir/repo"
mkdir -p "$repo/scripts" "$repo/scripts/e2e"
# The gate under test is the shipped one: the stubs live in the temp repository, but the gate must
# be the real file, not a copy of it in the temp tree (a copy would mask which version is under test).
cp "$gate" "$repo/scripts/gate.sh"
chmod +x "$repo/scripts/gate.sh"

# A stub step: its exit status comes from the variable named by STUB_VAR.
step_var() {
    printf 'STUB_%s_STATUS' "$(printf '%s' "$1" | tr 'a-z/.-' 'A-Z___')"
}
make_step_stub() {
    local rel="$1"
    # The stub reads the variable named after its step at RUN time, so the dollar sign must reach
    # the file literally: %s substitution keeps it out of this shell's expansion.
    local var_name
    var_name="$(step_var "$rel")"
    printf '#!/bin/bash\n# SELFTEST_INPUTS=scripts/gate.sh,scripts/gate-selftest.sh,scripts/m2.sh,scripts/e2e/run-all.sh\neval "code=\${%s:-0}"\nexit "$code"\n' "$var_name" > "$repo/scripts/$rel"
    chmod +x "$repo/scripts/$rel"
}

for rel in selftests.sh cargo-test.sh smoke-runners.sh clippy-gate.sh e2e/run-all.sh; do
    make_step_stub "$rel"
done

# The gate also runs cargo directly (fmt/check/clippy/release-check); stub it too.
printf '#!/bin/bash\nexit "${STUB_CARGO_STATUS:-0}"\n' > "$workdir/cargo"
chmod +x "$workdir/cargo"

cargo_var=STUB_CARGO_STATUS

expect_lines() {
    if grep -qE "^gate: $1 $2" "$workdir/out"; then
        ok
    else
        fail "expected gate: $1 $2"
    fi
}

# --- an all-green selected set exits 0 -----------------------------------------------------
PATH="$workdir:$PATH" "$repo/scripts/gate.sh" --tier=fmt --tier=check > "$workdir/out" 2>&1
status_out=$?
if [ "$status_out" -eq 0 ]; then ok; else fail "all-green selected tiers must exit 0 (got $status_out)"; fi
expect_lines fmt PASSED
expect_lines check PASSED

# --- --offline leaves the GUI tiers NOT RUN, so the run is INCOMPLETE (75) -----------------
PATH="$workdir:$PATH" "$repo/scripts/gate.sh" --offline --tier=fmt --tier=m1 --tier=m2 > "$workdir/out" 2>&1
status_out=$?
if [ "$status_out" -eq 75 ]; then ok; else fail "--offline must exit 75 while m1/m2 are NOT RUN (got $status_out)"; fi
expect_lines m1 "NOT RUN"
expect_lines m2 "NOT RUN"

# --- the old single-axis tier names are refused, never reinterpreted -----------------------
# A1 and A2 named cells the classification replaced. An old command must not silently run a
# different set, so the names are an error, and the error is not a tier outcome.
PATH="$workdir:$PATH" "$repo/scripts/gate.sh" --tier=a1 > "$workdir/out" 2>&1
status_out=$?
if [ "$status_out" -eq 2 ]; then ok; else fail "--tier=a1 must be refused with exit 2 (got $status_out)"; fi
if grep -q "is no longer a tier name" "$workdir/out"; then ok; else fail "--tier=a1 must say why it was refused"; fi
if grep -qE "^gate: a1 (PASSED|FAILED|NOT RUN)" "$workdir/out"; then fail "--tier=a1 must not produce a tier outcome"; else ok; fi
PATH="$workdir:$PATH" "$repo/scripts/gate.sh" --tier=a2 > "$workdir/out" 2>&1
status_out=$?
if [ "$status_out" -eq 2 ]; then ok; else fail "--tier=a2 must be refused with exit 2 (got $status_out)"; fi

# --- no argument runs the daily gate (M1), says so, and does not select M2 -----------------
PATH="$workdir:$PATH" "$repo/scripts/gate.sh" > "$workdir/out" 2>&1
status_out=$?
if grep -q "running M1 only" "$workdir/out"; then ok; else fail "a bare run must warn that it runs M1 only"; fi
if grep -qE "^gate: m2 NOT SELECTED" "$workdir/out"; then ok; else fail "a bare run must print m2 as NOT SELECTED"; fi
# M2 being unselected is not incompleteness, so a bare M1 run whose stubs pass exits 0.
if [ "$status_out" -eq 0 ]; then ok; else fail "a bare M1 run must not be incomplete because m2 was not selected (got $status_out)"; fi
if grep -qE "^gate: m1 PASSED" "$workdir/out"; then ok; else fail "a bare run must select and pass m1"; fi

# --- --full selects M2, and an unauthorized M2 leaves the run incomplete -------------------
printf '#!/bin/bash\nexit "${STUB_M2_STATUS:-0}"\n' > "$repo/scripts/m2.sh"
chmod +x "$repo/scripts/m2.sh"
PATH="$workdir:$PATH" "$repo/scripts/gate.sh" --full > "$workdir/out" 2>&1
status_out=$?
if grep -qE "^gate: m2 PASSED" "$workdir/out"; then ok; else fail "--full must run and record m2"; fi
env STUB_M2_STATUS=75 PATH="$workdir:$PATH" "$repo/scripts/gate.sh" --full > "$workdir/out" 2>&1
status_out=$?
if [ "$status_out" -eq 75 ]; then ok; else fail "--full with an incomplete m2 must exit 75 (got $status_out)"; fi
expect_lines m2 "NOT RUN"

# --- a failing step exits 1 ----------------------------------------------------------------
env "$cargo_var=1" PATH="$workdir:$PATH" "$repo/scripts/gate.sh" --tier=fmt > "$workdir/out" 2>&1
status_out=$?
if [ "$status_out" -eq 1 ]; then ok; else fail "a failing step must exit 1 (got $status_out)"; fi
expect_lines fmt FAILED

# --- a sibling that passes is still recorded as passed -------------------------------------
PATH="$workdir:$PATH" "$repo/scripts/gate.sh" --tier=fmt --tier=check > "$workdir/out" 2>&1
expect_lines fmt PASSED
expect_lines check PASSED

# --- a failure outranks incompleteness -----------------------------------------------------
env "$cargo_var=1" PATH="$workdir:$PATH" "$repo/scripts/gate.sh" --offline --tier=check > "$workdir/out" 2>&1
status_out=$?
if [ "$status_out" -eq 1 ]; then ok; else fail "a failure must outrank the NOT RUN tiers (got $status_out)"; fi

# --- a child aggregator's 75 becomes NOT RUN, never FAILED ---------------------------------
env "$(step_var selftests.sh)=75" PATH="$workdir:$PATH" "$repo/scripts/gate.sh" --tier=selftests --tier=fmt > "$workdir/out" 2>&1
status_out=$?
if [ "$status_out" -eq 75 ]; then ok; else fail "a child 75 must propagate as 75 (got $status_out)"; fi
expect_lines selftests "NOT RUN"
if grep -q "gate: selftests FAILED" "$workdir/out"; then
    fail "an incomplete child must not be recorded as FAILED"
else
    ok
fi

# --- a child aggregator's 1 becomes FAILED -------------------------------------------------
env "$(step_var selftests.sh)=1" PATH="$workdir:$PATH" "$repo/scripts/gate.sh" --tier=selftests > "$workdir/out" 2>&1
status_out=$?
if [ "$status_out" -eq 1 ]; then ok; else fail "a child 1 must propagate as 1 (got $status_out)"; fi
expect_lines selftests FAILED

# --- --offline never exempts a headless tier ----------------------------------------------
env "$cargo_var=1" PATH="$workdir:$PATH" "$repo/scripts/gate.sh" --offline --tier=check > "$workdir/out" 2>&1
expect_lines check FAILED

# --- an unknown tier is refused, not silently ignored -------------------------------------
PATH="$workdir:$PATH" "$repo/scripts/gate.sh" --tier=nonsense > "$workdir/out" 2>&1
status_out=$?
if [ "$status_out" -eq 2 ]; then ok; else fail "an unknown tier must be refused with exit 2 (got $status_out)"; fi

# --- the summary repeats every recorded line ----------------------------------------------
PATH="$workdir:$PATH" "$repo/scripts/gate.sh" --tier=fmt --tier=check > "$workdir/out" 2>&1
if grep -q "==== gate summary ====" "$workdir/out" && grep -qE "^  fmt +PASSED" "$workdir/out"; then
    ok
else
    fail "the summary table must repeat each step outcome"
fi

if [ "$failures" -ne 0 ]; then
    echo "gate-selftest: $failures failure(s) in $cases case(s)" >&2
    exit 1
fi
echo "gate-selftest ok: $cases case(s)"
