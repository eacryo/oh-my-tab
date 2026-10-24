#!/bin/bash
# Run the A1 GUI smoke runners, enumerating them instead of hard-coding their names.
#
# Why this exists: the smoke runners are #[ignore]d, so `cargo test` reports them as ignored and
# nobody runs them without typing 24 module paths by hand; the documented gate therefore had
# an A1 tier that was prose. Worse, a runner that returned early (no Accessibility permission)
# still made libtest print "ok", so the tier could look green while a check had not run.
#
# Usage:
#   scripts/smoke-runners.sh --list     # the runners this build has, with their requirement
#   scripts/smoke-runners.sh --run      # build, check the artifact is fresh, run every runner
#   scripts/smoke-runners.sh --run --filter settings::   # a subset, for iteration
#
# Outcome per runner follows the gate vocabulary:
#   PASSED   exit 0 and the runner printed "smoke-result: PASSED <n checks>" (n > 0)
#   NOT RUN  exit 0 and the runner printed "smoke-result: NOT RUN <reason>", or printed neither
#   FAILED   non-zero exit, a signal, or "smoke-result: FAILED"
# A runner whose silence counts as NOT RUN is the point: silence is what an early return looks
# like, and an early return is not a pass.

set -uo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
repo_dir="$(dirname -- "$script_dir")"

mode="run"
filter=""
for arg in "$@"; do
    case "$arg" in
        --list) mode="list" ;;
        --run) mode="run" ;;
        --filter=*) filter="${arg#--filter=}" ;;
        -h|--help) sed -n "2,24p" "$0"; exit 0 ;;
        *) echo "smoke-runners: unknown argument: $arg" >&2; exit 2 ;;
    esac
done

cd "$repo_dir" || exit 1

# The runner set is DERIVED from the compiled tests, never transcribed: a runner added or
# renamed in src/ shows up here by itself.
listing=$(mktemp -t omt-smoke-listing)
trap 'rm -f "$listing"' EXIT
if ! cargo test --lib -- --list --ignored 2>/dev/null > "$listing"; then
    echo "smoke-runners: could not enumerate the ignored tests (cargo test --list failed)" >&2
    exit 1
fi

runners=()
while IFS= read -r line; do

    # Only `<module path>: test` lines name a test; the `N tests, M benchmarks` summary and
    # the empty lines around it are not runners.
    case "$line" in
        *": test") ;;
        *) continue ;;
    esac
    name="${line%%: test}"
    # Every #[ignore]d test is part of the A1 GUI tier, whether or not its name contains "smoke":
    # the name is a convention and `objc_exception_guard_contains_a_raise` follows it the other way.
    # Filtering on the name silently dropped that runner from the tier it belongs to.
    if [ -n "$filter" ]; then
        case "$name" in
            *"$filter"*) ;;
            *) continue ;;
        esac
    fi
    runners+=("$name")
done < "$listing"

if [ "${#runners[@]}" -eq 0 ]; then
    echo "smoke-runners: no smoke runner matched (filter: ${filter:-none}); an empty tier is not a pass" >&2
    exit 1
fi

if [ "$mode" = "list" ]; then
    printf "runner (%d):\n" "${#runners[@]}"
    printf "  %s\n" "${runners[@]}"
    exit 0
fi

# --- prerequisite: a fresh app artifact ------------------------------------------------------
# Several runners spawn target/debug/oh-my-tab; `cargo test` does not rebuild that binary, and a
# deliberately broken refresh path once passed on a stale one. The binary must be newer than
# every file under src/, or the tier cannot decide and says so instead of measuring old code.
if ! cargo build --quiet; then
    echo "smoke-runners: cargo build failed; the A1 tier cannot run against a missing binary" >&2
    exit 1
fi
app_bin="target/debug/oh-my-tab"
if [ ! -x "$app_bin" ]; then
    echo "smoke-runners: NOT RUN: $app_bin is missing" >&2
    exit 75
fi
newest_src=$(find src -name "*.rs" -newer "$app_bin" -print -quit)
if [ -n "$newest_src" ]; then
    echo "smoke-runners: NOT RUN: $app_bin predates $newest_src" >&2
    exit 75
fi

passed=0; not_run=0; failed=0
declare_missing=()
for name in "${runners[@]}"; do
    log=$(mktemp -t omt-smoke-run)
    verdict_file=$(mktemp -t omt-smoke-verdict)
    # The runner writes its own verdict here; the harness never infers a pass from an exit code,
    # because libtest reports "ok" for a body that returned early without asserting anything.
    OMT_SMOKE_VERDICT="$verdict_file" cargo test --lib "$name" -- --ignored --exact > "$log" 2>&1
    status=$?
    recorded=$(tail -1 "$verdict_file" 2>/dev/null)
    outcome=${recorded#*$(printf "\t")}
    if [ "$status" -ne 0 ]; then
        echo "smoke: $name FAILED (exit $status)"
        tail -5 "$log" | sed "s/^/    /"
        failed=$((failed + 1))
    elif [ -z "$recorded" ]; then
        # No verdict: the runner returned without saying what it checked. Silence is not a pass.
        echo "smoke: $name NOT RUN (the runner recorded no verdict)"
        not_run=$((not_run + 1))
    else
        case "$outcome" in
            "PASSED "*)
                checks=${outcome#PASSED }
                if [ "${checks:-0}" -gt 0 ] 2>/dev/null; then
                    echo "smoke: $name PASSED ($checks checks)"
                    passed=$((passed + 1))
                else
                    echo "smoke: $name NOT RUN (the runner recorded PASSED 0, which is not a pass)"
                    not_run=$((not_run + 1))
                fi
                ;;
            "NOT RUN "*)
                echo "smoke: $name NOT RUN (${outcome#NOT RUN })"
                not_run=$((not_run + 1))
                ;;
            *)
                echo "smoke: $name NOT RUN (unrecognized verdict: $outcome)"
                not_run=$((not_run + 1))
                ;;
        esac
    fi
    rm -f "$log" "$verdict_file"
done
echo "smoke-runners: $passed passed, $not_run not run, $failed failed (of ${#runners[@]})"
if [ "$failed" -gt 0 ]; then exit 1; fi
if [ "$not_run" -gt 0 ]; then exit 75; fi
exit 0
