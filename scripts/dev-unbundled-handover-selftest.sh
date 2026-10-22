#!/bin/bash
# Offline check of the unbundled-install handover in dev-restart.sh, running the real control
# flow.
#
# The handover boots the agent out, then stops the pids it parked: the app treats SIGTERM as a
# failure, so while the agent is loaded launchd would start it again. Two properties are easy to
# get wrong, so they are asserted here instead of described in a comment:
#   * an instance that will not stop must stop the launch BEFORE the build, which means the
#     unknown-instance gate has to come AFTER the handover;
#   * a pid that bootout already ended counts as stopped and is still reported with its recovery
#     command, so the ordinary path does not go silent.
#
# The block is cut out of the real script and run against stubs, so reordering dev-restart.sh
# fails here.
#
# Usage: scripts/dev-unbundled-handover-selftest.sh      Exit 0 = every case held.

set -uo pipefail

# The empty CDPATH prefix is deliberate: it keeps a caller's CDPATH from redirecting the cd.
# shellcheck disable=SC1007
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
real_script="$script_dir/dev-restart.sh"
failures=0
workdir=$(mktemp -d)
trap 'rm -rf "$workdir"' EXIT

pass() { printf 'ok   %s\n' "$1"; }
fail() { printf 'FAIL %s\n' "$1"; failures=$((failures + 1)); }

# The patterns are matched against the script's own text, so the ${...} must stay literal.
# shellcheck disable=SC2016
start_line=$(grep -n '^if \[ \${#parked_unbundled_pids\[@\]} -gt 0 \]; then$' "$real_script" | cut -d: -f1)
# shellcheck disable=SC2016
gate_line=$(grep -n '^if \[ \${#unknown_instances\[@\]} -gt 0 \]; then$' "$real_script" | cut -d: -f1)
if [ -z "$start_line" ] || [ -z "$gate_line" ]; then
    echo "FAIL cannot locate the handover block or the unknown-instance gate in $real_script"
    exit 1
fi
if [ "$gate_line" -lt "$start_line" ]; then
    fail "the unknown-instance gate must come after the unbundled handover"
fi
# The handover plus the gate that follows it. The gate ends at the first bare "fi" below it, found
# by search rather than a guessed offset.
gate_end_line=$(awk -v s="$gate_line" 'NR > s && /^fi$/ { print NR; exit }' "$real_script")
if [ -z "$gate_end_line" ]; then
    echo "FAIL cannot find the end of the unknown-instance gate in $real_script"
    exit 1
fi
sed -n "${start_line},${gate_end_line}p" "$real_script" >"$workdir/handover.sh"

# Stubs for launchd and process control, prepended to every case.
cat >"$workdir/prelude.sh" <<'PRELUDE'
set -u
is_live() { grep -qx "$1" "$state/live" 2>/dev/null; }
launchctl() {
    case "${1:-}" in
        bootout)
            echo bootout >>"$state/log"
            if [ -f "$state/bootout_kills" ]; then : >"$state/live"; fi
            return 0
            ;;
        print) [ -f "$state/job_loaded" ]
            ;;
        *) return 0
            ;;
    esac
}
sleep() { :; }
stop_pid_by_identity() {
    echo "stop $1" >>"$state/log"
    is_live "$1" || return 1
    if [ -f "$state/refuses_to_stop" ]; then return 1; fi
    grep -vx "$1" "$state/live" >"$state/live.new"
    mv "$state/live.new" "$state/live"
    return 0
}
kill() {
    if [ "${1:-}" = "-0" ]; then
        if is_live "$2"; then return 0; else return 1; fi
    fi
    return 0
}
PRELUDE

# run_case <name> <parked pid> <live pid or ""> <flag...>
run_case() {
    local name="$1" parked="$2" live="$3"
    shift 3
    local dir="$workdir/$name"
    mkdir -p "$dir/bin"
    : >"$dir/log"
    if [ -n "$live" ]; then printf '%s\n' "$live" >"$dir/live"; else : >"$dir/live"; fi
    local flag
    for flag in "$@"; do : >"$dir/$flag"; done
    {
        printf 'state=%s\n' "$dir"
        printf 'unbundled_domain=gui/501\n'
        printf 'unbundled_label=com.eacryo.oh-my-tab.unbundled\n'
        printf 'unbundled_bin=%s/bin/oh-my-tab\n' "$dir"
        printf 'unknown_instances=()\n'
        printf 'stopped_dev_pids=()\nstopped_dev_owners=()\nstopped_packaged_pids=()\n'
        printf 'parked_unbundled_pids=(%s)\n' "$parked"
        cat "$workdir/prelude.sh"
        cat "$workdir/handover.sh"
        printf ': >"%s/got-past-gate"\n' "$dir"
    } >"$dir/run.sh"
    bash "$dir/run.sh" >"$dir/stdout" 2>&1
}

run_case bootout-ends-it 4242 "" bootout_kills
if [ -f "$workdir/bootout-ends-it/got-past-gate" ] \
   && grep -q 'stopped a running unbundled install (pid 4242)' "$workdir/bootout-ends-it/stdout" \
   && grep -q 'install-unbundled.sh start' "$workdir/bootout-ends-it/stdout"; then
    pass "a process bootout already ended is reported as stopped, with the recovery command"
else
    fail "bootout-ended process was not reported as stopped"
    sed 's/^/     | /' "$workdir/bootout-ends-it/stdout"
fi

run_case refuses 4242 4242 refuses_to_stop
if [ ! -f "$workdir/refuses/got-past-gate" ] && grep -q 'restart FAILED' "$workdir/refuses/stdout"; then
    pass "an instance that will not stop stops the launch before the build"
else
    fail "a surviving unbundled instance did not stop the launch"
    sed 's/^/     | /' "$workdir/refuses/stdout"
fi

run_case ordinary 4242 4242
if [ -f "$workdir/ordinary/got-past-gate" ] && grep -q 'stopped a running unbundled install' "$workdir/ordinary/stdout"; then
    pass "an ordinary stop reports the pid and continues"
else
    fail "an ordinary stop did not report and continue"
    sed 's/^/     | /' "$workdir/ordinary/stdout"
fi

if [ "$failures" -ne 0 ]; then
    printf '\n%d case(s) failed\n' "$failures"
    exit 1
fi
printf '\ndev-unbundled-handover selftest: ok\n'
