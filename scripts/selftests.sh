#!/bin/bash
# Run the offline selftests in scripts/, all of them or the ones a change could plausibly affect.
#
# Why this exists: nine selftests already guarded the gate helpers (the lock, the Finder extension
# cleanup, the e2e disturbance gates, the reviewer script, the launch codecs), and **nothing ran
# them**. A selftest nobody invokes is documentation: it can rot without anyone noticing, and the
# invariant it guards is then unguarded while looking guarded.
#
# Why --changed exists: the full set takes 99s, most of it agent-review-selftest.sh at 84s, and the
# daily gate is supposed to be the cheap loop. Selecting by directory ("anything under scripts/")
# would be wrong: doc-stats-selftest.sh asserts on the real style guides and the version selftests
# read Cargo.toml, so a documentation or manifest change selects them while touching nothing under
# scripts/. Selection therefore comes from each selftest's own declaration:
#
#   # SELFTEST_INPUTS=<repo-relative path>[,<path>...]
#
# A path ending in "/" matches everything under it. The declaration is the whole mapping; there is
# no dependency analyser behind it, and docs/developer-notes-en.md says so.
#
# Usage:
#   scripts/selftests.sh                     # every selftest
#   scripts/selftests.sh --changed           # the ones a change since HEAD could affect
#   scripts/selftests.sh --changed --since=<ref>
#
# Exit: 0 every selected selftest passed; 1 one failed, or the selection could not be made
#       (a selftest with no SELFTEST_INPUTS, an empty selection, a baseline that cannot be read).

set -uo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
repo_dir="$(dirname -- "$script_dir")"

changed=0
since=""
for arg in "$@"; do
    case "$arg" in
        --changed) changed=1 ;;
        --since=*) since="${arg#--since=}"; changed=1 ;;
        -h|--help) sed -n '2,31p' "$0"; exit 0 ;;
        *) echo "selftests: unknown argument: $arg" >&2; exit 2 ;;
    esac
done

cd "$repo_dir" || exit 1

selftests=("$script_dir"/*-selftest.sh)

# --- the input mapping ---------------------------------------------------------------------------
# Every declaration in the file's header is honoured, so a long list can be split over several lines.
# The header is the run of lines before the first line that is not a comment or blank (the shebang
# counts as a comment): a declaration further down belongs to some fixture the selftest writes, not to
# this file's contract, and is not read.
declared_inputs() {  # <script> -> its inputs, one per line
    # The header is the leading run of comment and blank lines. Once a non-comment line appears, the
    # rest of the file belongs to the test itself (including any text it writes into a fixture), so
    # every declaration found before that point is honoured and nothing after it is read.
    awk '
        /^[[:space:]]*$/ { next }
        /^#/ {
            if ($0 ~ /^# SELFTEST_INPUTS=/) {
                sub(/^# SELFTEST_INPUTS=/, "")
                print
            }
            next
        }
        { exit }
    ' "$1" 2>/dev/null | tr ',' '\n'
}

# A declaration is only a mapping once its paths are real. A path that no longer exists (a renamed
# helper) selects nothing and the check it guarded silently stops running; here it is an error instead.
invalid=()
for test_script in "${selftests[@]}"; do
    while IFS= read -r input; do
        [ -n "$input" ] || continue
        case "$input" in
            */) [ -d "${input%/}" ] || invalid+=("$(basename "$test_script"): $input (no such directory)") ;;
            *) [ -e "$input" ] || invalid+=("$(basename "$test_script"): $input (no such path)") ;;
        esac
    done <<< "$(declared_inputs "$test_script")"
done

# A selftest with no declaration cannot be selected correctly, so --changed refuses to guess and says
# which ones are missing instead of quietly leaving them out. The full run does not depend on the
# mapping, and reports the gap too so it cannot persist unnoticed.
undeclared=()
for test_script in "${selftests[@]}"; do
    [ -n "$(declared_inputs "$test_script")" ] || undeclared+=("$(basename "$test_script")")
done

# --- the change set ------------------------------------------------------------------------------
# The baseline is HEAD unless --since names another commit. Staged, unstaged and untracked files all
# count: a change is not less relevant because it has not been committed. If the baseline cannot be
# read (no git, no HEAD, a bad ref) the selection is not narrowed at all — the conservative answer is
# to run everything, never to run nothing.
changed_files() {
    local base ref
    if [ -n "$since" ]; then base="$since"; else base="HEAD"; fi
    if ! ref="$(git rev-parse --verify --quiet "$base^{commit}" 2>/dev/null)"; then
        echo "selftests: cannot resolve the baseline '$base'; running every selftest (never none)" >&2
        return 1
    fi
    {
        git diff --name-only "$ref" 2>/dev/null
        git ls-files --others --exclude-standard 2>/dev/null
    } | sort -u
}

selection_wanted() {  # <file> <input pattern>
    case "$2" in
        */) case "$1" in "$2"*) return 0 ;; *) return 1 ;; esac ;;
        *) [ "$1" = "$2" ] ;;
    esac
}

if [ "${#invalid[@]}" -gt 0 ]; then
    echo "selftests: ${#invalid[@]} declared input(s) do not exist, so the mapping is stale:" >&2
    printf '  %s\n' "${invalid[@]}" >&2
    echo "selftests: fix the SELFTEST_INPUTS declaration; a mapping to nothing selects nothing" >&2
    exit 1
fi

selected=()
if [ "$changed" -eq 1 ]; then
    if [ "${#undeclared[@]}" -gt 0 ]; then
        echo "selftests: ${#undeclared[@]} selftest(s) declare no SELFTEST_INPUTS, so --changed cannot" >&2
        echo "  tell whether a change affects them; refusing to select rather than skip them:" >&2
        printf '  %s\n' "${undeclared[@]}" >&2
        echo "selftests: add '# SELFTEST_INPUTS=<paths>' to each, or run without --changed" >&2
        exit 1
    fi
    changes=""
    if ! changes="$(changed_files)"; then
        selected=("${selftests[@]}")
    elif [ -z "$changes" ]; then
        # A tree that matches the baseline has no change to select from -- and "nothing selected"
        # must never read as "all passed". The agreed fallback is the conservative one: run
        # everything, so a check made after a commit still checks something.
        echo "selftests: the tree matches ${since:-HEAD}; running every selftest rather than none" >&2
        selected=("${selftests[@]}")
    else
        for test_script in "${selftests[@]}"; do
            while IFS= read -r pattern; do
                [ -n "$pattern" ] || continue
                hit=0
                while IFS= read -r file; do
                    if selection_wanted "$file" "$pattern"; then hit=1; break; fi
                done <<< "$changes"
                if [ "$hit" -eq 1 ]; then
                    selected+=("$test_script")
                    echo "selftests: $(basename "$test_script") selected by $pattern"
                    break
                fi
            done <<< "$(declared_inputs "$test_script")"
        done
        # Everything not chosen is named too: a run that says only what it checked leaves the reader
        # unable to tell "not relevant" from "forgotten", and this is a gate.
        for test_script in "${selftests[@]}"; do
            chosen=0
            for picked in "${selected[@]:-}"; do
                [ "$picked" = "$test_script" ] && chosen=1 && break
            done
            [ "$chosen" -eq 1 ] || echo "selftests: $(basename "$test_script") NOT SELECTED (none of its declared inputs changed)"
        done
    fi
else
    selected=("${selftests[@]}")
fi

if [ "${#selected[@]}" -eq 0 ]; then
    echo "selftests: the change matched no declared input; running every selftest rather than none" >&2
    selected=("${selftests[@]}")
fi

# --- run ------------------------------------------------------------------------------------------
failures=0
ran=0
failed_names=()

for test_script in "${selected[@]}"; do
    [ -x "$test_script" ] || {
        echo "selftests: $test_script is not executable; a selftest that cannot run is not a pass" >&2
        failures=$((failures + 1))
        failed_names+=("$(basename "$test_script") (not executable)")
        continue
    }
    ran=$((ran + 1))
    name="$(basename "$test_script")"
    echo "== selftest: $name =="
    if "$test_script" </dev/null; then
        echo "selftests: $name ok"
    else
        status=$?
        echo "selftests: $name FAILED (exit $status)" >&2
        failures=$((failures + 1))
        failed_names+=("$name")
    fi
done

if [ "${#selftests[@]}" -eq 0 ]; then
    echo "selftests: no selftest found in $script_dir" >&2
    exit 1
fi

if [ "${#undeclared[@]}" -gt 0 ] && [ "$changed" -eq 0 ]; then
    echo "selftests: note: ${#undeclared[@]} selftest(s) declare no SELFTEST_INPUTS:" >&2
    printf '  %s\n' "${undeclared[@]}" >&2
fi

if [ "$failures" -ne 0 ]; then
    echo "selftests: $failures of $ran failed:" >&2
    printf '  %s\n' "${failed_names[@]}" >&2
    exit 1
fi

echo "selftests ok: $ran selftest(s)"
