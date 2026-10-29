#!/bin/bash
# The inputs this selftest reads, one list, repo-relative. scripts/selftests.sh --changed uses this
# mapping to select it; a selftest without a declaration is refused rather than assumed irrelevant.
# SELFTEST_INPUTS=scripts/dev-launch-interrupt-selftest.sh,scripts/dev-restart.sh
# Offline check of the launch-interruption cleanup, executing the real control flow.
#
# An earlier version copied the guard condition into the test and compared strings, which proves
# only that the copy is what its author wrote. This one extracts the real adoption helper and
# finish_run from dev-restart.sh, stubs launchd and process control, and runs the actual
# ordering -- so a cleanup that looks up the instance BEFORE booting out fails here.
#
# Usage: scripts/dev-launch-interrupt-selftest.sh      Exit 0 = every case held.

set -uo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
real_script="$script_dir/dev-restart.sh"
failures=0
workdir=$(mktemp -d)
trap 'rm -rf "$workdir"' EXIT

pass() { printf 'ok   %s\n' "$1"; }
fail() { printf 'FAIL %s\n' "$1"; failures=$((failures + 1)); }

cat >"$workdir/harness-body.sh" <<'HARNESS'
set -u
fake_state="$1"
# The lookup count lives in a FILE, not a variable: the lookup runs inside a command substitution,
# so an in-process counter would reset each call and the late instance would never become visible.
find_dev_app_pid() {
    local n pid
    n=$(cat "$fake_state/lookups" 2>/dev/null || echo 0)
    n=$((n + 1))
    printf '%s' "$n" >"$fake_state/lookups"
    echo "lookup $n" >>"$fake_state/log"
    pid=$(head -1 "$fake_state/pids" 2>/dev/null)
    if [ -z "$pid" ] && [ "$n" -ge 2 ]; then
        pid=$(head -1 "$fake_state/appears" 2>/dev/null)
    fi
    printf '%s' "$pid"
}
# The residue check is the lookup that happens AFTER the stop attempts. A case that must reach it
# sets "late_pid"; the adoption lookups then keep reporting nothing, so only the residue check can
# find the instance -- which is the situation the case is about.
find_all_dev_app_pids() {
    echo "residue-check" >>"$fake_state/log"
    if [ -f "$fake_state/late_pid" ]; then
        cat "$fake_state/late_pid"
    fi
}
launchctl() {
    case "${1:-}" in
        bootout) echo "bootout" >>"$fake_state/log" ;;
        print) [ -f "$fake_state/job" ] ;;
        *) return 0 ;;
    esac
}
wait_for_job_gone() { rm -f "$fake_state/job"; return 0; }
ps() { echo 'started-at-T0'; }
# Only the pids this harness invented are alive; a real pid number must not be consulted.
harness_pid() { [ "$1" = "777" ] || [ "$1" = "888" ] || [ "$1" = "999" ]; }
kill() {
    case "$1" in
        -0) harness_pid "$2" || return 1; [ -f "$fake_state/alive" ] && return 0; return 1 ;;
        *) echo "signal $2" >>"$fake_state/log"; rm -f "$fake_state/alive" ;;
    esac
}
# The quit request goes through this function, which is what dev-restart.sh calls. Defining
# osascript here would be a no-op: the real script invokes the absolute path.
request_app_quit() { echo 'quit' >>"$fake_state/log"; return 0; }
stop_pid_by_identity() { echo 'force-stop' >>"$fake_state/log"; rm -f "$fake_state/alive"; return 0; }
omt_lock_release() { :; }
sleep() { :; }
launch_target="gui/0/fake"
launch_label="fake"
dev_bundle_id="com.example.fake"
dev_app_binary="/nonexistent/oh-my-tab"
check_mode=1
announce_stop=1
owned_pid=""
owned_job=1
owned_launch_attempted=1
app_pid_start=""
. "$2/functions.sh"
finish_run 0
HARNESS

make_harness() {
    local dir="$1"
    mkdir -p "$dir/fake_state"
    : >"$dir/fake_state/log"
    : >"$dir/fake_state/pids"
    rm -f "$dir/fake_state/lookups"
    sed -n '/^adopt_started_instance() {/,/^}/p' "$real_script" >"$dir/functions.sh"
    sed -n '/^quit_then_stop() {/,/^}/p' "$real_script" >>"$dir/functions.sh"
    # The process lookups are deliberately NOT extracted: the harness owns them. Sourcing the real
    # definitions after the stubs would silently replace them, and the residue check would then read
    # the host process table instead of the simulated one -- a selftest passing on the machine's
    # state rather than on the flow under test.
    sed -n '/^finish_run() {/,/^}/p' "$real_script" >>"$dir/functions.sh"
    cp "$workdir/harness-body.sh" "$dir/harness.sh"
}

# Case 1: the job starts the app between the run's knowledge of it and the bootout.
h1="$workdir/c1"; make_harness "$h1"
echo 777 >"$h1/fake_state/appears"
touch "$h1/fake_state/job" "$h1/fake_state/alive"
bash "$h1/harness.sh" "$h1/fake_state" "$h1" >/dev/null 2>&1
log1=$(cat "$h1/fake_state/log")
first_lookup=$(printf '%s\n' "$log1" | grep -n '^lookup ' | head -1 | cut -d: -f1)
bootout_line=$(printf '%s\n' "$log1" | grep -n '^bootout$' | head -1 | cut -d: -f1)
if [ -n "$bootout_line" ] && { [ -z "$first_lookup" ] || [ "$bootout_line" -le "$first_lookup" ]; }; then
    pass "the job is booted out before the lookup that can still see a late instance"
else
    fail "a lookup ran before the bootout; a late start would be missed"
fi
if printf '%s\n' "$log1" | grep -q '^signal\|^force-stop'; then
    pass "the instance that appeared after the first lookup is stopped"
else
    fail "the late instance was never stopped; it would keep running with its development switches"
fi

# Case 2: a process the run never recorded is still residue, so it must be stopped.
h2="$workdir/c2"; make_harness "$h2"
echo 888 >"$h2/fake_state/appears"
touch "$h2/fake_state/job" "$h2/fake_state/alive"
bash "$h2/harness.sh" "$h2/fake_state" "$h2" >/dev/null 2>&1
log2=$(cat "$h2/fake_state/log")
if printf '%s\n' "$log2" | grep -q '^signal\|^force-stop'; then
    pass "a process the run never recorded is still stopped"
else
    fail "only the saved pid is consulted; an unrecorded instance survives cleanup"
fi

# Case 3: the counterexample -- with BOTH discovery paths removed the same scenario must leak.
# One path alone is not the point: the post-bootout adoption and the residue check are independent
# ways to find a late instance, and this case proves the pair is what makes the stop happen rather
# than the scenario passing no matter how cleanup is written.
h3="$workdir/c3"; make_harness "$h3"
echo 999 >"$h3/fake_state/appears"
touch "$h3/fake_state/job" "$h3/fake_state/alive"
sed -i.bak '/^        adopt_started_instance$/d' "$h3/functions.sh"
sed -i.bak2 's/^        residue="$(find_all_dev_app_pids)"$/        residue=""/' "$h3/functions.sh"
sed -i.bak3 '/^            for pid in $residue; do/,/^            done$/d' "$h3/functions.sh"
bash "$h3/harness.sh" "$h3/fake_state" "$h3" >/dev/null 2>&1
log3=$(cat "$h3/fake_state/log")
if printf '%s\n' "$log3" | grep -q '^signal\|^force-stop'; then
    fail "the counterexample still stopped the late instance; case 1 would prove nothing"
else
    pass "without the post-bootout lookup the late instance is missed (what case 1 excludes)"
fi

# Case 3b: the RESIDUE path. The instance is invisible to every adoption lookup and is only
# reported by the residue check, so the branch under test is the one that runs after the stop
# attempts -- precisely the branch an earlier version signalled directly.
#
# It must still get a quit request, it must escalate, and the run must end non-zero without ever
# announcing a clean stop. None of that can be satisfied by the main path here: adoption never sees
# a pid, so the only possible stop is the residue one.
h4="$workdir/c4"; make_harness "$h4"
touch "$h4/fake_state/job" "$h4/fake_state/alive"
echo 999 >"$h4/fake_state/late_pid"
bash "$h4/harness.sh" "$h4/fake_state" "$h4" >"$h4/stdout" 2>&1
status4=$?
log4=$(cat "$h4/fake_state/log")
# Every stop attempt must be preceded by a quit request on the same path, and the run must end
# non-zero once any attempt escalated.
stops=$(printf '%s\n' "$log4" | grep -c '^force-stop')
quits=$(printf '%s\n' "$log4" | grep -c '^quit')
if printf '%s\n' "$log4" | grep -q '^residue-check'; then
    pass "the residue path was actually reached (the earlier cases could not prove this)"
else
    fail "the residue path never ran; this case proves nothing about it"
fi
if [ "$stops" -gt 0 ] && [ "$quits" -ge "$stops" ]; then
    pass "every stop attempt was preceded by a quit request (quits=$quits stops=$stops)"
else
    fail "a stop attempt went straight to a signal (quits=$quits stops=$stops)"
fi
if [ "$status4" -ne 0 ]; then
    pass "a cleanup that had to escalate does not report success (exit $status4)"
else
    fail "a force-stopped instance was reported as a clean stop"
fi
if grep -q 'instance: stopped' "$h4/stdout"; then
    fail "the run announced a clean stop after escalating"
else
    pass "no clean-stop announcement after an escalation"
fi

# Case 4: the ownership flags are settled before the submit call.
attempt_line=$(grep -n '^owned_launch_attempted=1$' "$real_script" | head -1 | cut -d: -f1)
job_line=$(grep -n '^owned_job=1$' "$real_script" | head -1 | cut -d: -f1)
submit_line=$(grep -n 'launchctl submit -l' "$real_script" | head -1 | cut -d: -f1)
if [ -n "$attempt_line" ] && [ -n "$job_line" ] && [ -n "$submit_line" ] && [ "$attempt_line" -lt "$submit_line" ] && [ "$job_line" -lt "$submit_line" ]; then
    pass "both ownership flags are set before the submit call"
else
    fail "ownership flags are not settled before the submit"
fi

if [ $failures -ne 0 ]; then
    printf '\n%d case(s) failed\n' "$failures"
    exit 1
fi
printf '\nall cases held\n'
