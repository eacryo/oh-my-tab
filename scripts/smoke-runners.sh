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
cell=""
disturb_query=""
cell_query=""
check_query=""
for arg in "$@"; do
    case "$arg" in
        --list) mode="list" ;;
        --run) mode="run" ;;
        --filter=*) filter="${arg#--filter=}" ;;
        --cell=M1|--cell=M2) cell="${arg#--cell=}" ;;
        # Answer a disturbance query from the source alone: no enumeration, no cargo, no build. The
        # authorization gate in m2.sh asks this before anything may compile.
        --disturbances=*) disturb_query="${arg#--disturbances=}" ;;
        --cell-of=*) cell_query="${arg#--cell-of=}" ;;
        --check-disturbances=*) check_query="${arg#--check-disturbances=}" ;;
        -h|--help) sed -n "2,30p" "$0"; exit 0 ;;
        *) echo "smoke-runners: unknown argument: $arg" >&2; exit 2 ;;
    esac
done

cd "$repo_dir" || exit 1

# (The disturbance query is answered below, after the readers are defined.)

# The M-cell a runner declares, read from its own source: exactly one of these lines, placed above
# the `fn` that defines it. The declaration lives next to the runner rather than in a table so that
# a runner added or renamed in src/ is admitted (or refused) without anyone transcribing a list.
#
#   // SMOKE_CELL=M1     non-disturbing: no app instance, no window on screen, no screen grab,
#                        no injected input, no instance replaced
#   // SMOKE_CELL=M2     anything else: it brings up the real AppKit tree, spawns a child, or grabs
#                        the screen
#
# A missing declaration, both declarations, or an unknown value is refused: the runner is a row in
# the admission report and the run fails. Guessing a cell is exactly how a disturbing runner would
# end up in the daily gate.
runner_cell() {  # <runner name> -> prints M1, M2, AMBIGUOUS, UNDEFINED, or nothing
    local name="$1" short hits file line found cell
    short="${name##*::}"
    # The definition is found by exact text, not by regular expression: the name ends at "(", so
    # fn base_runner( can never match fn base_runner_extra(.
    hits="$(grep -rl --include='*.rs' -E "fn ${short}\\(" src 2>/dev/null)"
    if [ -z "$hits" ]; then
        hits="$(grep -rl --include='*.rs' -E "fn ${short}(_runner|_smoke)\\(" src 2>/dev/null)"
    fi
    if [ -z "$hits" ]; then
        printf 'UNDEFINED'
        return 0
    fi
    cell=""
    while IFS= read -r file; do
        [ -n "$file" ] || continue
        # The definition line, matched so that "fn app_menu_smoke(" cannot also match inside
        # "fn app_menu_smoke_runner(": the character after the captured name must be "(".
        # One pass, first hit wins: the plain definition is preferred over the _runner/_smoke
        # fallbacks, and the chosen line is printed once. A later fallback must never overwrite the
        # line that was found first -- that is how app_menu_smoke resolved to its _runner helper and
        # lost its declaration.
        line="$(awk -v n="$short" '
            {
                if (index($0, "fn " n "(") > 0) { print NR; exit }
            }
        ' "$file")"
        if [ -z "$line" ]; then
            line="$(awk -v n="$short" '
                {
                    if (index($0, "fn " n "_runner(") > 0) { print NR; exit }
                    if (index($0, "fn " n "_smoke(") > 0) { print NR; exit }
                }
            ' "$file")"
        fi
        [ -n "$line" ] || continue
        # Walk upward from the definition through the attribute/comment block directly above it, and
        # stop at the first line that is neither. That boundary is what stops one fn borrowing the
        # declaration written above a different fn: the intervening blank line or closing brace ends
        # the block.
        found="$(awk -v start="$line" '
            NR >= start { exit }
            { lines[NR] = $0 }
            END {
                for (i = start - 1; i >= 1; i--) {
                    line = lines[i]
                    if (line ~ /^[[:space:]]*$/) { if (started) break; else continue }
                    if (line ~ /^[[:space:]]*(\/\/|#)/) { started = 1; buf[++n] = line; continue }
                    break
                }
                for (i = n; i >= 1; i--) print buf[i]
            }
        ' "$file" | sed -n 's/.*SMOKE_CELL=\([^ ]*\).*/\1/p' | sort -u | tr '\n' ' ')"
        # The whole declared value is compared, never a prefix: SMOKE_CELL=M1_M2 and
        # SMOKE_CELL=M1-invalid are not M1, they are declarations this reader does not understand.
        case "$found" in
            "") ;;
            "M1 "|"M2 ") cell="$cell${found}" ;;
            *) printf 'AMBIGUOUS'; return 0 ;;
        esac
    done <<< "$hits"
    cell="$(printf '%s' "$cell" | sed 's/ //g')"
    case "$cell" in
        "") return 0 ;;
        M1|M2) printf '%s' "$cell" ;;
        *) printf 'AMBIGUOUS' ;;
    esac
}

runner_disturbances() {  # <runner name> -> comma-separated markers (empty for none)
    local short file line
    short="${1##*::}"
    file="$(grep -rl --include='*.rs' -E "fn ${short}\\(" src 2>/dev/null | head -1)"
    [ -n "$file" ] || file="$(grep -rl --include='*.rs' -E "fn ${short}(_runner|_smoke)\\(" src 2>/dev/null | head -1)"
    [ -n "$file" ] || return 0
    line="$(awk -v n="$short" '
        index($0, "fn " n "(") { print NR; exit }
        index($0, "fn " n "_runner(") { print NR; exit }
        index($0, "fn " n "_smoke(") { print NR; exit }
    ' "$file")"
    [ -n "$line" ] || return 0
    awk -v start="$line" '
        NR >= start { exit }
        { lines[NR] = $0 }
        END {
            for (i = start - 1; i >= 1; i--) {
                line = lines[i]
                if (line ~ /^[[:space:]]*$/) { if (started) break; else continue }
                if (line ~ /^[[:space:]]*(\/\/|#)/) { started = 1; buf[++n] = line; continue }
                break
            }
            for (i = n; i >= 1; i--) print buf[i]
        }
    ' "$file" | grep -oE 'SMOKE_DISTURBS=[^[:space:]]+' | sed 's/SMOKE_DISTURBS=//' | sort -u | tr '\n' ' '
}

# Whether a disturbance list is well formed: comma-separated members of the four known markers, with no
# repeats and no unknown token. Anything else is a declaration this reader does not understand, and an
# unreadable disturbance list is refused rather than read as "nothing to authorize" -- a typo such as
# "scren" would otherwise let a screen capture run under a focus-only authorization.
disturbances_well_formed() {  # <list> -> 0 when well formed
    local list="$1" m seen=" "
    [ -n "$list" ] || return 1
    # A leading, trailing or doubled comma leaves an empty member, which is a malformed list rather
    # than a list of the markers around it.
    case "$list" in
        ,*|*,|*,,*) return 1 ;;
    esac
    for m in $(printf '%s' "$list" | tr ',' ' '); do
        case "$m" in
            focus|screen|prefs|instance) ;;
            *) return 1 ;;
        esac
        case "$seen" in
            *" $m "*) return 1 ;;
        esac
        seen="$seen$m "
    done
    return 0
}

# These three queries are answered from the source files only, so a caller can decide what a run would
# disturb -- and refuse to run it -- before anything is compiled.
if [ -n "${cell_query:-}" ]; then
    runner_cell "x::$cell_query"
    echo
    exit 0
fi
if [ -n "${check_query:-}" ]; then
    if disturbances_well_formed "$check_query"; then exit 0; else exit 1; fi
fi
if [ -n "${disturb_query:-}" ]; then
    runner_disturbances "$disturb_query"
    echo
    exit 0
fi

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
    # Every #[ignore]d test is a smoke runner, whether or not its name contains "smoke": the name is
    # a convention and `objc_exception_guard_contains_a_raise` follows it the other way. Filtering on
    # the name silently dropped that runner from the tier it belongs to.
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

# --- admission: every runner must name exactly one M-cell -------------------------------------------
# Read before any selection, because a runner whose declaration is missing or ambiguous is not a
# runner this tier may silently drop: it is a hole in the classification. All of them are reported
# together so one run fixes them all.
unadmitted=()
declared_cell() {  # <runner> -> prints the declared cell, or nothing
    runner_cell "$1"
}
for name in "${runners[@]}"; do
    cell_of="$(declared_cell "$name")"
    case "$cell_of" in
        M1|M2) ;;
        *)
            unadmitted+=("$name (declares: ${cell_of:-nothing})")
            ;;
    esac
done

if [ "${#unadmitted[@]}" -gt 0 ]; then
    echo "smoke-runners: ${#unadmitted[@]} runner(s) without exactly one SMOKE_CELL=M1|M2 declaration:" >&2
    printf '  %s\n' "${unadmitted[@]}" >&2
    echo "smoke-runners: a runner with no cell, or with two, is refused rather than guessed into one" >&2
    if [ "$mode" = "list" ]; then
        # --list still shows the whole set, so a developer can see what exists and repair it.
        printf "runner (%d):\n" "${#runners[@]}"
        for name in "${runners[@]}"; do
            printf "  %-70s %s\n" "$name" "$(declared_cell "$name")"
        done
    fi
    exit 1
fi

# The cell filter selects; it never reclassifies. A runner it excludes is printed as NOT SELECTED so
# a reader of a --cell=M1 run can see the M2 runners it did not run.
if [ -n "$cell" ]; then
    selected_runners=()
    for name in "${runners[@]}"; do
        if [ "$(declared_cell "$name")" = "$cell" ]; then
            selected_runners+=("$name")
        else
            echo "smoke: $name NOT SELECTED (declared $(declared_cell "$name"), this run selects $cell)"
        fi
    done
    runners=("${selected_runners[@]:-}")
    if [ "${#runners[@]}" -eq 0 ] || [ -z "${runners[0]:-}" ]; then
        echo "smoke-runners: no runner declares SMOKE_CELL=$cell; an empty selection is not a pass" >&2
        exit 1
    fi
fi

if [ "$mode" = "list" ]; then
    printf "runner (%d):\n" "${#runners[@]}"
    for name in "${runners[@]}"; do
        printf "  %-70s %s\n" "$name" "$(declared_cell "$name")"
    done
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
    # The scope reaches the runner so the shared child launcher can refuse to start an app under M1.
    # A hand-run test sets neither variable and is not constrained.
    OMT_SMOKE_VERDICT="$verdict_file" OMT_SMOKE_CELL="${cell:-ALL}" cargo test --lib "$name" -- --ignored --exact > "$log" 2>&1
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
