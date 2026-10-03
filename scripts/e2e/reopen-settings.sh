#!/bin/bash
# E2E_STEALS_FOCUS=1
# LaunchServices reopen delivery opens Settings in the already-running accessory app.

set -uo pipefail

repo_dir="$(cd "$(dirname "$0")/../.." && pwd)"
app_bundle="$repo_dir/dist/Oh-My-Tab-Dev.app"
state_file="/tmp/omt-e2e-reopen-settings.json"

fail() { echo "e2e reopen-settings: FAIL: $*" >&2; exit 1; }

rm -f "$state_file" "${state_file%.json}.tmp"
restart_out="$("$repo_dir/scripts/dev-restart.sh" --e2e-state="$state_file" --no-onboarding 2>&1)"
echo "$restart_out" | grep -q "^restart ok" || {
    echo "$restart_out" | tail -20 >&2
    fail "dev-restart.sh did not bring the app up"
}
echo "$restart_out" | grep -E "^(restart ok|build-version|app args)" || true

# The instance that must be driven is the one dev-restart reports. A launchd job left over from a
# previous run can respawn an older instance while this run starts, and a snapshot read at that
# moment can belong to a process this scenario is not driving. Clearing the snapshot after the
# restart (rather than before it) also removes any frame the older instance wrote.
app_pid="$(printf '%s\n' "$restart_out" \
    | sed -n 's/^restart ok (app pid \([0-9][0-9]*\).*/\1/p' | head -1)"
[ -n "$app_pid" ] || fail "could not read the app pid from dev-restart.sh output"
rm -f "$state_file" "${state_file%.json}.tmp"

baseline_seq="$(python3 - "$state_file" <<'PY'
import json
import sys
import time

path = sys.argv[1]
deadline = time.time() + 5
while time.time() < deadline:
    try:
        with open(path) as handle:
            state = json.load(handle)
    except (FileNotFoundError, json.JSONDecodeError):
        time.sleep(0.05)
        continue
    if state.get("settings_window_visible"):
        raise SystemExit("Settings was already visible before the reopen probe")
    print(state.get("seq", 0))
    raise SystemExit(0)
raise SystemExit("no initial e2e-state snapshot")
PY
)" || fail "could not establish a hidden-Settings baseline"

pid_before="$(pgrep -x oh-my-tab | head -1)"
[ -n "$pid_before" ] || fail "the dev app process is missing before reopen"
/usr/bin/open -a "$app_bundle" || fail "LaunchServices open failed"
pid_after="$(pgrep -x oh-my-tab | head -1)"
[ "$pid_before" = "$pid_after" ] || fail "reopen replaced the running process ($pid_before -> $pid_after)"

python3 - "$state_file" "$baseline_seq" <<'PY'
import json
import sys
import time

path = sys.argv[1]
baseline_seq = int(sys.argv[2])
deadline = time.time() + 5
last = None
while time.time() < deadline:
    try:
        with open(path) as handle:
            last = json.load(handle)
    except (FileNotFoundError, json.JSONDecodeError):
        pass
    if (
        last
        and last.get("seq", 0) > baseline_seq
        and last.get("settings_window_visible")
    ):
        if last.get("selected_sidebar") != 0:
            raise SystemExit(
                "e2e reopen-settings: FAIL: Settings opened on an unexpected page "
                f"({last.get('selected_sidebar')})"
            )
        print(
            "e2e reopen-settings: PASS: Settings visible after LaunchServices reopen "
            f"(seq={last.get('seq')}, event={last.get('event')})"
        )
        raise SystemExit(0)
    time.sleep(0.05)

raise SystemExit(
    "e2e reopen-settings: FAIL: no visible Settings snapshot after open; "
    f"last={last}"
)
PY
