#!/bin/bash
# The M2 tier: every step that can disturb the person using the machine.
#
# Why this exists: the disturbing checks are the expensive half of the complete gate (measured
# 2026-10-24: about 226s for the e2e scenarios alone) and the half that takes the user's frontmost
# application away. They belong to the release loop, not to every edit, so they live behind one
# aggregator that the daily gate never selects.
#
# Authorization comes before anything is compiled, built, launched or locked. The order below is the
# contract:
#
#   1. read the declarations from the source (no cargo, no enumeration);
#   2. refuse any runner whose declaration is missing, unknown or contradictory;
#   3. only then enumerate the compiled set and run what was authorised.
#
# There are four independent disturbances and four independent switches. A step runs only when the
# operator authorised exactly the ones it declares: --include-focus does not admit a screen capture,
# and authorising nothing runs nothing.
#
# Usage:
#   scripts/m2.sh                                      # nothing authorized: every step NOT RUN
#   scripts/m2.sh --include-focus --include-instance   # only the steps those cover
#
# Exit: 0 every selected step passed; 1 something failed (outranks 75), or the set could not be read;
#      75 nothing failed, something did not decide (unauthorized disturbance, no graphical session,
#      a runner reporting NOT RUN).

set -uo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
repo_dir="$(dirname -- "$script_dir")"

include_focus=0
include_screen=0
include_prefs=0
include_instance=0
for arg in "$@"; do
    case "$arg" in
        --include-focus) include_focus=1 ;;
        --include-screen) include_screen=1 ;;
        --include-prefs) include_prefs=1 ;;
        --include-instance) include_instance=1 ;;
        -h|--help) sed -n '2,29p' "$0"; exit 0 ;;
        *) echo "m2: unknown argument: $arg" >&2; exit 2 ;;
    esac
done

forward=()
[ "$include_focus" = "1" ] && forward+=(--include-focus)
[ "$include_screen" = "1" ] && forward+=(--include-screen)
[ "$include_prefs" = "1" ] && forward+=(--include-prefs)
[ "$include_instance" = "1" ] && forward+=(--include-instance)

cd "$repo_dir" || exit 1

failed=0
not_run=0

# --- 1. the declarations, read from the source only -------------------------------------------
# Each M2 smoke runner names its cell and the disturbances it causes. This is plain text in src/, so
# reading it compiles nothing and starts nothing.
# One pass over src/: every fn directly preceded by a SMOKE_CELL=M2 line is an M2 runner, and the
# SMOKE_DISTURBS line in the same header block names what it disturbs. Text only -- no cargo.
# One pass over src/ text: no cargo, no enumeration, no build. The status is checked, not just the
# output: an awk that printed a partial list and then failed would otherwise run part of the tier and
# let everything else report success.
scan_file="$(mktemp -t omt-m2-cells)"
source_files="$(find src -name '*.rs' 2>/dev/null)"
if [ -z "$source_files" ]; then
    echo "m2: src/ holds no source files; refusing to report a pass" >&2
    exit 1
fi
# Only the files that declare an M2 cell can contribute, and the awk helper is given just those.
m2_files="$(grep -rl 'SMOKE_CELL=M2' src --include='*.rs' 2>/dev/null)"
if ! awk -f "$script_dir/lib/smoke-cells.awk" $m2_files > "$scan_file" 2>/tmp/omt-m2-scan.err; then
    echo "m2: the source scan failed; refusing to run part of the tier:" >&2
    sed 's/^/  /' /tmp/omt-m2-scan.err >&2
    exit 1
fi
known_runners="$(grep -v '|none$' "$scan_file")"
rm -f "$scan_file"

# A conflicting declaration is not a runner with a default: the gate cannot know what it disturbs.
bad_decl="$(printf '%s\n' "$known_runners" | grep '|conflicting$')"
if [ -n "$bad_decl" ]; then
    echo "m2: these tests declare conflicting disturbances; failing the tier:" >&2
    printf '  %s\n' $bad_decl >&2
    exit 1
fi

if [ -z "$known_runners" ]; then
    echo "m2: no M2 smoke runner could be read from src/; refusing to report a pass" >&2
    exit 1
fi

# An empty, unknown or contradictory declaration is a failure, not a runner to leave out: it means
# the gate cannot know what the runner disturbs.
bad_decl=0
while IFS='|' read -r name markers status; do
    [ -n "$name" ] || continue
    markers="$(printf '%s' "$markers" | tr -d ' ')"
    if [ -z "$markers" ] || ! "$script_dir/smoke-runners.sh" --check-disturbances="$markers" >/dev/null 2>&1; then
        echo "m2: $name has no readable disturbance declaration ($markers); failing the tier" >&2
        bad_decl=1
    fi
done <<< "$known_runners"
if [ "$bad_decl" -ne 0 ]; then exit 1; fi

# --- 2. authorization, still without building anything -----------------------------------------
authorized_every() {  # <comma separated markers> -> 0 when each one is authorized
    local markers="$1" m
    for m in $(printf '%s' "$markers" | tr ',' ' '); do
        case "$m" in
            focus) [ "$include_focus" = "1" ] || return 1 ;;
            screen) [ "$include_screen" = "1" ] || return 1 ;;
            prefs) [ "$include_prefs" = "1" ] || return 1 ;;
            instance) [ "$include_instance" = "1" ] || return 1 ;;
            *) return 1 ;;
        esac
    done
    return 0
}

selected_runners=""
while IFS='|' read -r name markers status; do
    [ -n "$name" ] || continue
    markers="$(printf '%s' "$markers" | tr -d ' ')"
    if authorized_every "$markers"; then
        selected_runners="$selected_runners$name
"
    else
        echo "m2: $name NOT RUN (declares $markers; not all authorized)"
        not_run=$((not_run + 1))
    fi
done <<< "$known_runners"

# --- 2b. the source list and the compiled list must agree ---------------------------------------
# The scan reads declarations from src/; the compiled set is what libtest will actually run. A name in
# one and not the other means either a helper was mistaken for a test (the scan would filter for a
# name libtest does not have) or a test has no declaration. Both are holes, so this is checked before
# anything is built -- by asking the harness for its list, which is the same enumeration the run uses.
if [ -n "$selected_runners" ]; then
    # The listing prints the selected rows and, for the ones it excludes, "NOT SELECTED" lines too.
    # Only the selected rows carry a cell as their last field, so the cell column is the discriminator.
    # The enumeration's STATUS is checked, not just its output: an admission failure still prints the
    # rows it recognised before exiting 1, and those rows can look exactly like a complete set. A pipe
    # into awk would discard that status, so the listing goes to a file first.
    listing="$(mktemp -t omt-m2-listing)"
    if ! "$script_dir/smoke-runners.sh" --list --cell=M2 > "$listing" 2>/dev/null; then
        echo "m2: the compiled runner enumeration failed; refusing to report a pass" >&2
        sed 's/^/  /' "$listing" | tail -5 >&2
        exit 1
    fi
    compiled="$(awk '$NF == "M2" { print $1 }' "$listing" | sed 's/.*:://' | sort -u)"
    rm -f "$listing"
    if [ -z "$compiled" ]; then
        echo "m2: the compiled M2 runner set is empty; refusing to report a pass" >&2
        exit 1
    fi
    scanned="$(printf '%s\n' "$known_runners" | cut -d'|' -f1 | sort -u)"
    if [ "$scanned" != "$compiled" ]; then
        echo "m2: the source scan and the compiled runner set disagree; failing the tier:" >&2
        printf '%s\n' "$scanned" > /tmp/omt-m2-scanned
        printf '%s\n' "$compiled" > /tmp/omt-m2-compiled
        echo "  only in src/:" >&2; comm -23 /tmp/omt-m2-scanned /tmp/omt-m2-compiled | sed 's/^/    /' >&2
        echo "  only in the compiled set:" >&2; comm -13 /tmp/omt-m2-scanned /tmp/omt-m2-compiled | sed 's/^/    /' >&2
        exit 1
    fi
fi

# --- 3. only now is anything compiled or started ------------------------------------------------
if [ -n "$selected_runners" ]; then
    echo "== m2 step: smoke (M2) =="
    while IFS= read -r runner; do
        [ -n "$runner" ] || continue
        "$script_dir/smoke-runners.sh" --run --filter="$runner" > /tmp/omt-m2-smoke.log 2>&1
        status=$?
        case "$status" in
            0) ;;
            75) not_run=$((not_run + 1)) ;;
            *) failed=$((failed + 1)) ;;
        esac
        grep -E '^smoke: ' /tmp/omt-m2-smoke.log | sed 's/^/  /'
    done <<< "$selected_runners"
fi

echo "== m2 step: e2e =="
if [ "${#forward[@]}" -gt 0 ]; then
    "$script_dir/e2e/run-all.sh" --gate "${forward[@]}"
else
    "$script_dir/e2e/run-all.sh" --gate
fi
status=$?
case "$status" in
    0) ;;
    75) not_run=$((not_run + 1)) ;;
    *) failed=$((failed + 1)) ;;
esac

echo "m2: $not_run not run, $failed failed"
if [ "$failed" -gt 0 ]; then exit 1; fi
if [ "$not_run" -gt 0 ]; then exit 75; fi
exit 0
