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
requested_tiers=()
selected=()
for arg in "$@"; do
    case "$arg" in
        --offline) offline=1 ;;
        --tier=*) requested_tiers+=("${arg#--tier=}") ;;
        -h|--help) sed -n '2,45p' "$0"; exit 0 ;;
        *) echo "gate: unknown argument: $arg" >&2; exit 2 ;;
    esac
done

# Which tiers run. The order is cheapest and most-likely-to-fail first, so a broken build does not
# spend minutes in the GUI tiers before saying so.
all_tiers=(selftests fmt check clippy release-check cargo-test a1 a2)
if [ "${#requested_tiers[@]}" -eq 0 ]; then
    selected=("${all_tiers[@]}")
else
    selected=("${requested_tiers[@]}")
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
if wants_tier selftests; then
    run_step selftests "${script_dir}/selftests.sh"
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

# --- A1: the GUI smoke runners --------------------------------------------------------------------
if wants_tier a1; then
    if [ "$offline" -eq 1 ]; then
        skip_step "offline: the GUI smoke runners need a graphical session" a1
    elif ! is_graphical_session; then
        skip_step "no graphical session (launchctl managername is not Aqua)" a1
    else
        run_step a1 "${script_dir}/smoke-runners.sh" --run
    fi
fi

# --- A2: the e2e scenarios ------------------------------------------------------------------------
if wants_tier a2; then
    if [ "$offline" -eq 1 ]; then
        skip_step "offline: the e2e scenarios drive a real app and real input" a2
    elif ! is_graphical_session; then
        skip_step "no graphical session (launchctl managername is not Aqua)" a2
    else
        run_step a2 "${script_dir}/e2e/run-all.sh" --gate
    fi
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
