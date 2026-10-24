#!/bin/bash
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
    printf '#!/bin/bash\neval "code=\${%s:-0}"\nexit "$code"\n' "$var_name" > "$repo/scripts/$rel"
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
PATH="$workdir:$PATH" "$repo/scripts/gate.sh" --offline --tier=fmt --tier=a1 --tier=a2 > "$workdir/out" 2>&1
status_out=$?
if [ "$status_out" -eq 75 ]; then ok; else fail "--offline must exit 75 while a1/a2 are NOT RUN (got $status_out)"; fi
expect_lines a1 "NOT RUN"
expect_lines a2 "NOT RUN"

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
