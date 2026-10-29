#!/bin/bash
# The inputs this selftest reads, one list, repo-relative. scripts/selftests.sh --changed
# uses this mapping to select it; a selftest without one is refused rather than assumed irrelevant.
# SELFTEST_INPUTS=scripts/e2e/run-all.sh,scripts/e2e-gate-selftest.sh,scripts/lib/e2e-restore.sh
# Offline selftest for scripts/lib/dev-stop.sh and for the A2 aggregator disturbance gates.
#
# Why it exists: the helper exists because a name-based kill can stop the user packaged app, and the
# gate function exists because a scenario must declare every disturbance it causes. Neither claim can be
# verified by reading the code once -- the regressions they guard against are "the check still passes
# while matching nothing" and "one switch admits a scenario that needs another". Both are checked here
# against synthetic inputs, with no GUI, no app and no launchd.
#
#   scripts/e2e-gate-selftest.sh
#
# Exit code: 0 = every case behaved as specified; non-zero when a case did not.

set -uo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
repo_dir="$(dirname -- "$script_dir")"
failures=0
cases=0

fail() {
    echo "e2e-gate-selftest: FAIL: $*" >&2
    failures=$((failures + 1))
}

ok() {
    cases=$((cases + 1))
}

# --- the aggregator gates ----------------------------------------------------------------
# The functions are extracted from run-all.sh rather than re-derived here: a selftest that restates the
# rule tests its own copy. Extracting means the shipped text is what runs.
extract_function() {  # <file> <name>
    local file="$1" name="$2"
    awk -v name="$name" '
        $0 ~ "^" name "\\(\\)" { printing = 1 }
        printing { print }
        printing && /^\}/ { exit }
    ' "$file"
}

run_all="$repo_dir/scripts/e2e/run-all.sh"
extracted="$(extract_function "$run_all" scenario_marks)"
extracted_gate="$(extract_function "$run_all" gate_refusal)"
# A harness that cannot read the functions must stop here: continuing would run every case against an
# empty string, and those failures would describe the extraction, not the gates.
if [ -z "$extracted" ] || [ -z "$extracted_gate" ]; then
    echo "e2e-gate-selftest: FAIL: could not extract the gate functions from $run_all" >&2
    exit 1
fi

# The worker runs in a subshell with the two functions and the four include flags the gate reads, so
# a case can set any combination without leaking into the next one. The instance flag is the fourth
# disturbance: a scenario that restarts the app replaces the instance the person is using, which is
# neither focus, screen nor preferences.
gate() {  # <focus> <prefs> <screen> <instance> <inc_focus> <inc_prefs> <inc_screen> <inc_instance>
    local focus="$1" prefs="$2" screen="$3" instance="$4"
    local inc_focus="$5" inc_prefs="$6" inc_screen="$7" inc_instance="$8"
    (
        include_focus="$inc_focus"
        include_prefs="$inc_prefs"
        include_screen="$inc_screen"
        include_instance="$inc_instance"
        eval "$extracted"
        eval "$extracted_gate"
        gate_refusal "$focus" "$prefs" "$screen" "$instance"
    )
}

expect_empty() {  # <label> <focus> <prefs> <screen> <inc_focus> <inc_prefs> <inc_screen>
    local label="$1"; shift
    local out
    out="$(gate "$@")"
    if [ -n "$out" ]; then
        fail "$label: expected the scenario to run, the gate named $out"
    else
        ok
    fi
}

expect_refusal() {  # <label> <switch> <focus> <prefs> <screen> <inc_focus> <inc_prefs> <inc_screen>
    local label="$1" want="$2"; shift 2
    local out
    out="$(gate "$@")"
    if [ "$out" != "$want" ]; then
        fail "$label: expected $want, got "$out""
    else
        ok
    fi
}

# Nothing declared: always runs, whatever was authorised.
expect_empty "no marks, no switches" 0 0 0 0 0 0 0 0
expect_empty "no marks, all switches" 0 0 0 0 1 1 1 0

# Each declaration alone is refused until its own switch is given -- and the other two switches must NOT
# admit it. That is the counter-example the old single if-chain got wrong.
expect_refusal "focus alone" --include-focus 1 0 0 0 0 0 0 0
expect_refusal "focus alone, prefs+screen given" --include-focus 1 0 0 0 0 1 1 0
expect_empty "focus alone, focus given" 1 0 0 0 1 0 0 0

expect_refusal "prefs alone" --include-prefs 0 1 0 0 0 0 0 0
expect_refusal "prefs alone, focus+screen given" --include-prefs 0 1 0 0 1 0 1 0
expect_empty "prefs alone, prefs given" 0 1 0 0 0 1 0 0

expect_refusal "screen alone" --include-screen 0 0 1 0 0 0 0 0
expect_refusal "screen alone, focus+prefs given" --include-screen 0 0 1 0 1 1 0 0
expect_empty "screen alone, screen given" 0 0 1 0 0 0 1 0

# Stacked declarations need every switch: one of them is never enough. panel-contrast.sh is the real
# case (prefs + screen).
expect_refusal "prefs+screen, only prefs" --include-screen 0 1 1 0 0 1 0 0
expect_refusal "prefs+screen, only screen" --include-prefs 0 1 1 0 0 0 1 0
expect_empty "prefs+screen, both" 0 1 1 0 0 1 1 0
expect_refusal "focus+screen, only focus" --include-screen 1 0 1 0 1 0 0 0
expect_refusal "focus+screen, only screen" --include-focus 1 0 1 0 0 0 1 0
expect_empty "focus+screen, both" 1 0 1 0 1 0 1 0
expect_refusal "all three, only two" --include-screen 1 1 1 0 1 1 0 0
expect_empty "all three, all given" 1 1 1 0 1 1 1 0

# The instance disturbance is independent of the other three: authorising a restart is not the same as
# authorising focus, a screen grab or a preferences change, and none of them admits it.
expect_refusal "instance alone" --include-instance 0 0 0 1 0 0 0 0
expect_refusal "instance alone, focus+screen+prefs given" --include-instance 0 0 0 1 1 1 1 0
expect_empty "instance alone, instance given" 0 0 0 1 0 0 0 1
expect_refusal "focus+instance, only focus" --include-instance 1 0 0 1 1 0 0 0
expect_empty "focus+instance, both" 1 0 0 1 1 0 0 1
expect_refusal "prefs+instance, only prefs" --include-instance 0 1 0 1 0 1 0 0
expect_empty "prefs+instance, both" 0 1 0 1 0 1 0 1

# The marker reader itself: exact header lines only, so prose that mentions a marker cannot widen a gate
# and a marker with a trailing comment cannot silently stop being one.
tmpdir="$(mktemp -d -t e2e-gate-selftest)"
trap 'rm -rf "$tmpdir"' EXIT
printf '# a scenario\n# E2E_STEALS_FOCUS=1\n# E2E_COVERS_SCREEN=1\n' >"$tmpdir/exact.sh"
printf '# E2E_STEALS_FOCUS=1 (injects a hotkey)\n' >"$tmpdir/trailing.sh"
printf '# talking about E2E_COVERS_SCREEN=1 in prose only\n' >"$tmpdir/prose.sh"
printf '# E2E_CHANGES_PREFS = 1\n' >"$tmpdir/spaced.sh"

check_marks() {  # <label> <file> <want>
    local label="$1" file="$2" want="$3"
    local got
    got="$(
        eval "$extracted"
        scenario_marks "$file"
    )"
    if [ "$got" != "$want" ]; then
        fail "$label: expected "$want", got "$got""
    else
        ok
    fi
}
check_marks "exact markers" "$tmpdir/exact.sh" "1 0 1 0"
check_marks "marker with a trailing comment is not a marker" "$tmpdir/trailing.sh" "0 0 0 0"
check_marks "marker named in prose is not a marker" "$tmpdir/prose.sh" "0 0 0 0"
check_marks "spaces around the assignment are not the marker" "$tmpdir/spaced.sh" "0 0 0 0"
printf '# E2E_REPLACES_INSTANCE=1\n' >"$tmpdir/instance.sh"
check_marks "instance marker alone" "$tmpdir/instance.sh" "0 0 0 1"

# --- the conditional restore helper ------------------------------------------------------
# Review finding: an unconditional "hand the front back" can yank a person who deliberately switched
# apps during the run, and a restore that fails silently is a broken gate. Both are checked here against
# a stub `cua-driver`, so the cases are offline and deterministic.
source "$repo_dir/scripts/lib/e2e-restore.sh"

stub_dir="$tmpdir/stub"
mkdir -p "$stub_dir"
cat >"$stub_dir/cua-driver" <<'STUB'
#!/bin/bash
# A stand-in for the parts of cua-driver this helper uses. It answers from files the case writes.
case "$1" in
    list_apps) cat "$STUB_FRONT" ;;
    get_cursor_position) cat "$STUB_CURSOR" ;;
    get_screen_size) echo '{"scale_factor": 2}' ;;
    # The tool name is part of the record, so a case can assert which call was made and with what.
    bring_to_front) printf 'bring_to_front %s\n' "$2" >> "$STUB_LOG" ;;
    move_cursor) printf 'move_cursor %s\n' "$2" >> "$STUB_LOG" ;;
    *) exit 1 ;;
esac
STUB
chmod +x "$stub_dir/cua-driver"
export OMT_CUA="$stub_dir/cua-driver"
export STUB_FRONT="$tmpdir/front.json"
export STUB_CURSOR="$tmpdir/cursor.json"
export STUB_LOG="$tmpdir/log.txt"
: >"$STUB_LOG"

front() {  # <pid...>
    python3 -c 'import json,sys; print(json.dumps({"apps":[{"pid":int(p),"active":True} for p in sys.argv[1:]]}))' "$@" >"$STUB_FRONT"
}
cursor() { printf '{"x": %s, "y": %s}\n' "$1" "$2" >"$STUB_CURSOR"; }
log_has() { grep -q "$1" "$STUB_LOG"; }

# Already in front: nothing is activated, and that is success.
: >"$STUB_LOG"
front 42
omt_hand_front_back 42 99 && ok || fail "omt_hand_front_back: already in front must succeed"
log_has bring_to_front && fail "omt_hand_front_back: activated when the front was already correct" || ok

# The app under test still owns the front: hand it back.
: >"$STUB_LOG"
front 99
omt_hand_front_back 42 99 && ok || fail "omt_hand_front_back: the test app owning the front must be restored"
log_has "bring_to_front {\"pid\": 42}" && ok || fail "omt_hand_front_back: it did not activate the recorded app"

# A THIRD app is in front: the user moved on. The helper keeps their choice and says so.
: >"$STUB_LOG"
front 7
omt_hand_front_back 42 99
[ "$?" -eq 2 ] && ok || fail "omt_hand_front_back: a user switch must be reported as kept (exit 2)"
log_has bring_to_front && fail "omt_hand_front_back: overrode a user who had switched apps" || ok

# The call shape space-groups uses: TextEdit (the app it drove) still in front must be handed back, and
# the same call must not treat oh-my-tab's pid as the owner.
: >"$STUB_LOG"
front 55
omt_hand_front_back 42 55 && ok || fail "omt_hand_front_back: the driven app in front must be restored"
log_has "bring_to_front {\"pid\": 42}" && ok || fail "omt_hand_front_back: it did not hand the front back to the recorded app"
: >"$STUB_LOG"
front 55
omt_hand_front_back 42 77 2>/dev/null
[ "$?" -eq 2 ] && ok || fail "omt_hand_front_back: a driven pid that does not match the front must keep the user's choice"
log_has bring_to_front && fail "omt_hand_front_back: restored although the front belonged to neither app" || ok

# An unknown controlled app cannot prove ownership: the helper must refuse rather than activate.
: >"$STUB_LOG"
front 99
omt_hand_front_back 42 "" 2>/dev/null
[ "$?" -eq 1 ] && ok || fail "omt_hand_front_back: an unknown controlled app must refuse (exit 1)"
log_has bring_to_front && fail "omt_hand_front_back: restored without knowing the controlled app" || ok

# An unreadable front cannot confirm anything: failure, not silence.
printf 'not json\n' >"$STUB_FRONT"
omt_hand_front_back 42 99 && fail "omt_hand_front_back: an unreadable front must fail" || ok

# An unreadable query is also a failure, not "nothing to restore".
omt_hand_front_back "" 99 && fail "omt_hand_front_back: an empty baseline must fail" || ok

# The pointer: restored only while it is still where the scenario left it. Every argument is in desktop
# PIXELS -- the helper compares like with like; the stub reports points at scale 2, so it reads 800x600.
: >"$STUB_LOG"
cursor 400 300
omt_restore_cursor 100 100 800 600 8 && ok || fail "omt_restore_cursor: an untouched pointer must be restored"
log_has move_cursor && ok || fail "omt_restore_cursor: it did not move the pointer back"

: >"$STUB_LOG"
cursor 900 900
omt_restore_cursor 100 100 800 600 8
[ "$?" -eq 2 ] && ok || fail "omt_restore_cursor: a moved pointer must be reported as kept (exit 2)"
log_has move_cursor && fail "omt_restore_cursor: overrode a pointer the user had moved" || ok

# The endpoint check: unchanged passes, and the two failure readings stay distinguishable.
front 42
omt_front_unchanged 42 99 && ok || fail "omt_front_unchanged: an unchanged front must pass"
front 99
omt_front_unchanged 42 99 2>"$tmpdir/endpoint.err" && fail "omt_front_unchanged: the app under test in front must fail" || ok
grep -q "app under test became frontmost" "$tmpdir/endpoint.err" || fail "omt_front_unchanged: the app-under-test reading was not named"
front 7
omt_front_unchanged 42 99 2>"$tmpdir/endpoint.err" && fail "omt_front_unchanged: a third app in front must fail" || ok
grep -q "if a person switched apps" "$tmpdir/endpoint.err" || fail "omt_front_unchanged: the ambiguous reading was not named"
printf 'not json\n' >"$STUB_FRONT"
omt_front_unchanged 42 99 2>/dev/null && fail "omt_front_unchanged: an unreadable front must fail" || ok
omt_front_unchanged "" 99 2>/dev/null && fail "omt_front_unchanged: an empty baseline must fail" || ok

# Unreadable pointer state is a failure, because whether it moved cannot be decided.
printf 'not json\n' >"$STUB_CURSOR"
omt_restore_cursor 100 100 800 600 8 && fail "omt_restore_cursor: an unreadable pointer must fail" || ok

if [ "$failures" -ne 0 ]; then
    echo "e2e-gate-selftest: $failures case(s) failed (of $cases)" >&2
    exit 1
fi
echo "e2e-gate-selftest: $cases case(s) passed"