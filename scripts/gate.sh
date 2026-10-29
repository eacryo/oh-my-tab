#!/bin/bash
# The repository's full gate, as one command instead of five bullets in AGENTS.md.
#
# Why this exists: AGENTS.md names the gate (cargo fmt, cargo check, cargo clippy, the test suite,
# the A1 GUI smoke runners, the A2 e2e scenarios) but there was no command that ran it and no
# single place that said which parts had actually decided anything. A tier that could not run
# therefore looked exactly like a tier that passed, and "the gate is green" meant different things
# on different days.
#
# Usage:
#   scripts/gate.sh                     # everything this machine can run
#   scripts/gate.sh --offline           # only the tiers that need no graphical session
#   scripts/gate.sh --tier=a1           # a single tier (repeatable)
#   scripts/gate.sh --offline --tier=test
#
# Outcomes. Every step prints exactly one line
#
#     gate: <step> PASSED|FAILED|NOT RUN [reason]
#
# and the run ends with a table of the same lines. The meaning is fixed:
#
#   PASSED   the step ran and its own checks passed
#   NOT RUN  the step could not decide here (no graphical session, a tier this machine cannot
#            perform, or --offline). Never a pass.
#   FAILED   the step ran and failed, could not be started, or died on a signal
#
# Exit status, so that "incomplete" cannot be read as "passed":
#
#   0   every selected step PASSED
#   1   at least one step FAILED (it outranks incompleteness)
#   75  no failure, but at least one step is NOT RUN (EX_TEMPFAIL: the gate is incomplete)
#
# --offline does not weaken the exit status. It changes which tiers are *selected*: the GUI tiers
# are declared out of scope on the command line, and they are still printed as NOT RUN so the
# output never suggests they were checked. Anything headless is never exempted by it.

set -uo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
repo_dir="$(dirname -- "$script_dir")"

offline=0
full=0
include_args=()
requested_tiers=()
selected=()
for arg in "$@"; do
    case "$arg" in
        --offline) offline=1 ;;
        --full) full=1 ;;
        # The four disturbances are carried through to the M2 aggregator verbatim. gate.sh never
        # decides for the operator that one authorization implies another; it only forwards what was
        # asked for. They imply --full, because nothing else selects M2.
        --include-focus|--include-screen|--include-prefs|--include-instance)
            include_args+=("$arg")
            full=1
            ;;
        --tier=*) requested_tiers+=("${arg#--tier=}") ;;
        -h|--help) sed -n '2,60p' "$0"; exit 0 ;;
        *) echo "gate: unknown argument: $arg" >&2; exit 2 ;;
    esac
done

# The two old tier names described the single-axis split (A1/A2/B) that the classification replaced.
# Reinterpreting them as their new-name equivalents would let an existing command run a different set
# than it used to, so they are refused outright instead of aliased (AGENTS.md, "Testing
# classification").
# bash 3.2 (macOS) misbehaves on "${array[@]:-}" for an empty array, and this loop runs on every
# invocation, so it guards the count instead.
if [ "${#requested_tiers[@]}" -gt 0 ]; then
for tier in "${requested_tiers[@]}"; do
    case "$tier" in
        a1|a2)
            echo "gate: --tier=$tier is no longer a tier name: the tiers are M1 (the daily gate) and" >&2
            echo "      M2 (disturbing). Run scripts/gate.sh for M1, or scripts/gate.sh --full for" >&2
            echo "      the complete run. The old name is refused rather than reinterpreted." >&2
            exit 2
            ;;
    esac
done
fi

# Which tiers run. The order is cheapest and most-likely-to-fail first, so a broken build does not
# spend minutes in the GUI tiers before saying so. m1 is the non-disturbing cells (the offline steps
# plus the M1 smoke runners) and m2 is what disturbs the user; --full is the only way to select m2.
all_tiers=(selftests fmt check clippy release-check cargo-test m1 m2)
if [ "${#requested_tiers[@]}" -gt 0 ]; then
    selected=("${requested_tiers[@]}")
elif [ "$full" -eq 1 ]; then
    selected=("${all_tiers[@]}")
else
    # No argument is the DAILY gate, not the complete one. The warning goes to stderr and to the
    # summary so a reader of either learns which gate they just ran.
    echo "gate: warning: no tier selected, running M1 only (the daily gate)." >&2
    echo "gate: warning: the complete run is scripts/gate.sh --full." >&2
    daily_default_warning=1
    selected=(selftests fmt check clippy release-check cargo-test m1)
fi

for tier in "${selected[@]}"; do
    case " ${all_tiers[*]} " in
        *" $tier "*) ;;
        *) echo "gate: unknown tier: $tier (one of: ${all_tiers[*]})" >&2; exit 2 ;;
    esac
done

# Called only in a direct `if` condition or in an assignment: a command substitution would run
# this function in a subshell, where `return` exits the subshell and the assignment looks true.
# Selection is a plain membership scan. It deliberately uses no `local` array copy and no
# sub-function: this gate runs under macOS's bash 3.2, where the more elaborate forms were
# observed to evaluate the wrong answer inside `if`, and a gate that silently skips a tier is
# exactly the failure this script exists to remove.
wants_tier() {
    case " ${selected[*]} " in
        *" $1 "*) return 0 ;;
        *) return 1 ;;
    esac
}


# One string per recorded step, "step|outcome|reason". macOS ships bash 3.2, which has no
# associative arrays, so this stays a plain array: the summary walks it in order and that is the
# only structure the gate needs.
results=()

record() {  # <step> <outcome> [reason]
    # One assignment per line: in bash 3.2 a multi-assignment `local a="$1" b="$2"` has
    # already cleared `$1` by the time `$2` is read, so both would be empty.
    local step="$1"
    local outcome="$2"
    local reason="${3:-}"
    # `local x="$1"` always leaves the status at 0, so nothing after this point may rely on $?.
    results+=("${step}|${outcome}|${reason}")
    if [ -n "$reason" ]; then
        echo "gate: $step $outcome ($reason)"
    else
        echo "gate: $step $outcome"
    fi
}

# A step that was selected but could not start is FAILED, not skipped: the caller asked for it.
run_step() {  # <step> <command...>
    local step="$1"
    shift
    local status=0
    echo "== gate step: $step =="
    "$@" || status=$?
    if [ "$status" -eq 0 ]; then
        record "$step" "PASSED"
    elif [ "$status" -eq 75 ]; then
        # An aggregator child that reported itself incomplete did not fail; it could not decide.
        record "$step" "NOT RUN" "the step reported incomplete"
    else
        record "$step" "FAILED"
    fi
}

# A tier this machine cannot perform. It is recorded every time it was selected, so --offline and a
# missing graphical session produce the same honest line.
skip_step() {  # <reason> <step>
    local reason="$1"
    local step="$2"
    record "$step" "NOT RUN" "$reason"
}

is_graphical_session() {
    # `launchctl managername` prints Aqua for a logged-in graphical session and Background for an
    # ssh session; both exist on macOS, so the check is about which one this shell belongs to.
    [ "$(launchctl managername 2>/dev/null)" = "Aqua" ]
}

cd "$repo_dir" || { echo "gate: cannot enter $repo_dir" >&2; exit 1; }

# --- selftests ------------------------------------------------------------------------------------
# The daily gate selects by change; the complete run checks everything. The selection is visible in
# the step's own output (each chosen selftest is named with the input that selected it, and a baseline
# that cannot be resolved falls back to running them all).
if wants_tier selftests; then
    if [ "$full" -eq 1 ]; then
        run_step selftests "${script_dir}/selftests.sh"
    else
        run_step selftests "${script_dir}/selftests.sh" --changed
    fi
fi

# --- fmt ------------------------------------------------------------------------------------------
if wants_tier fmt; then
    run_step fmt cargo fmt --all --check
fi

# --- check ----------------------------------------------------------------------------------------
if wants_tier check; then
    run_step check cargo check --all-targets
fi

# --- clippy ---------------------------------------------------------------------------------------
if wants_tier clippy; then
    # scripts/clippy-gate.sh denies warnings in production code and reports test-only ones without
    # failing on a baseline that predates this branch (12 warnings, all in #[cfg(test)]).
    run_step clippy "${script_dir}/clippy-gate.sh"
fi

# --- release-check --------------------------------------------------------------------------------
# A debug-only helper called from shared code compiles in dev and fails in release; the repository
# has been bitten by exactly that, so the gate compiles the release profile every time.
if wants_tier release-check; then
    run_step release-check cargo check --release --all-targets
fi

# --- cargo-test -----------------------------------------------------------------------------------
if wants_tier cargo-test; then
    run_step cargo-test "${script_dir}/cargo-test.sh"
fi

# --- M1: the non-disturbing smoke runners ----------------------------------------------------------
# Only the runners that declare M1 are selected here; smoke-runners.sh refuses a runner whose
# declaration is missing or ambiguous rather than guessing it into a cell.
if wants_tier m1; then
    if [ "$offline" -eq 1 ]; then
        skip_step "offline: the M1 smoke runners need a graphical session" m1
    elif ! is_graphical_session; then
        skip_step "no graphical session (launchctl managername is not Aqua)" m1
    else
        run_step m1 "${script_dir}/smoke-runners.sh" --run --cell=M1
    fi
fi

# --- M2: the disturbing smoke runners and the e2e scenarios ---------------------------------------
# This step can disturb the user (focus, screen, preferences, or the running instance), so it is
# selected only by --full. It asks run-all.sh for every disturbance the scenarios declare; any that
# the operator has not authorized leaves the step incomplete instead of quietly skipping it.
if wants_tier m2; then
    if [ "$offline" -eq 1 ]; then
        skip_step "offline: the M2 tier disturbs the machine" m2
    elif ! is_graphical_session; then
        skip_step "no graphical session (launchctl managername is not Aqua)" m2
    else
        # An empty array expands to nothing under bash 3.2 only via this count guard; passing
        # "${include_args[@]:-}" would hand m2.sh one empty argument, which it rejects as unknown.
        if [ "${#include_args[@]}" -gt 0 ]; then
            run_step m2 "${script_dir}/m2.sh" "${include_args[@]}"
        else
            run_step m2 "${script_dir}/m2.sh"
        fi
    fi
else
    # Outside the daily gate, but printed so a reader never mistakes "not selected" for "passed".
    record m2 "NOT SELECTED" "outside the daily gate; --full selects it"
fi

# --- summary --------------------------------------------------------------------------------------
failed=0
not_run=0
for row in "${results[@]}"; do
    IFS='|' read -r _step outcome _reason <<<"$row"
    case "$outcome" in
        FAILED) failed=$((failed + 1)) ;;
        "NOT RUN") not_run=$((not_run + 1)) ;;
    esac
done

echo
echo "==== gate summary ===="
for row in "${results[@]}"; do
    IFS='|' read -r step outcome reason <<<"$row"
    printf '  %-14s %-8s %s\n' "$step" "$outcome" "$reason"
done
echo "======================"

if [ "$failed" -gt 0 ]; then
    echo "gate: FAILED ($failed failed, $not_run not run)" >&2
    exit 1
fi
if [ "$not_run" -gt 0 ]; then
    echo "gate: INCOMPLETE ($not_run not run; a complete gate never reports success on these)" >&2
    exit 75
fi
echo "gate: PASSED"
