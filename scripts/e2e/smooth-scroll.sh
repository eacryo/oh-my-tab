#!/bin/bash
# A2 E2E scenario: a HID-level wheel event traverses the mouse tap and produces a timed
# smooth-scroll stream. Swift/CoreGraphics posts the wheel so it reaches the HID event tap; cua-driver
# is used only to inspect the desktop and guard against sending input at the login screen.
# It sends no keyboard or click input and checks that the active pid is unchanged before and after.
#
# Requires the Swift toolchain, Accessibility permission, and cua-driver:
#   scripts/e2e/smooth-scroll.sh
#
# The feature is enabled for this launch only through argv; no config file is edited.

set -uo pipefail

repo_dir="$(cd "$(dirname "$0")/../.." && pwd)"
state_file="/tmp/omt-e2e-smooth-scroll.json"
screenshot_file="/private/tmp/omt-e2e-smooth-scroll.png"
before_active_file="/tmp/omt-e2e-smooth-scroll-active-pid"
scroll_direction="down"
scroll_amount=4

fail() { echo "e2e smooth-scroll: FAIL: $*" >&2; exit 1; }

command -v cua-driver >/dev/null 2>&1 || fail "cua-driver CLI not found in PATH"
cua-driver status >/dev/null 2>&1 || fail "cua-driver daemon is not running"
rm -f "$state_file" "${state_file%.json}.tmp" "$screenshot_file" "$before_active_file"

restart_out="$("$repo_dir/scripts/dev-restart.sh" \
    --smooth-scroll-force-on --e2e-state="$state_file" --no-onboarding 2>&1)"
echo "$restart_out" | grep -q "^restart ok" || {
    echo "$restart_out" | tail -20 >&2
    fail "dev-restart.sh did not bring the app up"
}
echo "$restart_out" | grep -E "^(restart ok|build-version|app args)" || true
pid="$(printf '%s\n' "$restart_out" | sed -n 's/^restart ok (app pid \([0-9][0-9]*\),.*/\1/p')"
[ -n "$pid" ] || fail "could not read the app pid from dev-restart output"

if python3 - "$state_file" "$screenshot_file" "$pid" >"$before_active_file" <<'PY'
import json
import subprocess
import sys
import time

state_file, screenshot_file, pid_text = sys.argv[1:]
pid = int(pid_text)


def cua(tool: str, args: dict) -> dict:
    completed = subprocess.run(
        ["cua-driver", "call", tool, json.dumps(args)],
        capture_output=True,
        text=True,
    )
    if completed.returncode:
        raise SystemExit(
            f"cua-driver {tool} failed ({completed.returncode}): "
            f"{completed.stderr[-600:]} {completed.stdout[-600:]}"
        )
    try:
        return json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        raise SystemExit(f"cua-driver {tool} returned invalid JSON: {error}")


process = subprocess.run(["ps", "-p", str(pid), "-o", "pid="], capture_output=True, text=True)
if process.returncode or process.stdout.strip() != str(pid):
    raise SystemExit(f"dev-restart process {pid} is no longer running")
desktop = cua(
    "get_desktop_state",
    {"screenshot_out_file": screenshot_file},
)
width = desktop.get("screenshot_width")
height = desktop.get("screenshot_height")
if not isinstance(width, (int, float)) or not isinstance(height, (int, float)):
    raise SystemExit(f"desktop snapshot lacks native screenshot dimensions: {desktop.keys()}")
if width < 500 or height < 350:
    raise SystemExit(f"unexpected primary display screenshot size: {width}x{height}")

# Use the app's own initial snapshot to avoid sending input while the login screen is frontmost.
deadline = time.time() + 5
initial_state = None
while time.time() < deadline:
    try:
        with open(state_file) as handle:
            initial_state = json.load(handle)
    except (FileNotFoundError, json.JSONDecodeError):
        initial_state = None
    if initial_state and initial_state.get("frontmost"):
        break
    time.sleep(0.05)
frontmost = initial_state.get("frontmost", {}) if initial_state else {}
if not frontmost or "loginwindow" in frontmost.get("app", "").lower():
    raise SystemExit("login screen is frontmost; unlock the desktop before running this A2 scenario")
before_active = frontmost.get("pid")
if not before_active:
    raise SystemExit(f"initial e2e-state has no frontmost pid: {frontmost}")
print(before_active)
PY
then
    :
else
    fail "desktop preflight failed"
fi
before_active="$(cat "$before_active_file")"
rm -f "$before_active_file"

if ! swift - "$scroll_direction" "$scroll_amount" <<'SWIFT'
import CoreGraphics
import Foundation

guard CommandLine.arguments.count == 3,
      let amount = Int(CommandLine.arguments[2]), amount > 0 else {
    fputs("usage: swift <direction> <positive amount>\n", stderr)
    exit(2)
}

let delta: Int32
switch CommandLine.arguments[1] {
case "down": delta = 1
case "up": delta = -1
default:
    fputs("direction must be 'up' or 'down'\n", stderr)
    exit(2)
}

for index in 0..<amount {
    guard let event = CGEvent(
        scrollWheelEvent2Source: nil,
        units: .line,
        wheelCount: 2,
        wheel1: delta,
        wheel2: 0,
        wheel3: 0
    ) else {
        fputs("could not create HID scroll event\n", stderr)
        exit(1)
    }
    event.post(tap: .cghidEventTap)
    if index + 1 < amount {
        Thread.sleep(forTimeInterval: 0.02)
    }
}
SWIFT
then
    fail "Swift could not post HID-level wheel events"
fi

python3 - "$state_file" "$pid" "$before_active" <<'PY'
import json
import subprocess
import sys
import time

state_file, pid_text, before_active = sys.argv[1:]
pid = int(pid_text)
before_active = int(before_active)

process = subprocess.run(["ps", "-p", str(pid), "-o", "pid="], capture_output=True, text=True)
if process.returncode or process.stdout.strip() != str(pid):
    raise SystemExit(f"dev-restart process {pid} is no longer running")

deadline = time.time() + 8
state = None
while time.time() < deadline:
    try:
        with open(state_file) as handle:
            candidate = json.load(handle)
    except (FileNotFoundError, json.JSONDecodeError):
        candidate = None
    smooth = candidate.get("smooth_scroll", {}) if candidate else {}
    if (
        smooth.get("ticks", 0) > 0
        and smooth.get("touch_began", 0) >= 1
        and smooth.get("momentum_began", 0) >= 1
    ):
        state = candidate
        break
    time.sleep(0.05)

if not state:
    raise SystemExit("mouse tap did not record smooth-scroll touch and momentum start events")

# Let the engine's momentum phase finish before checking the final e2e-state snapshot.
time.sleep(1.0)
try:
    with open(state_file) as handle:
        state = json.load(handle)
except (FileNotFoundError, json.JSONDecodeError) as error:
    raise SystemExit(f"could not read settled e2e-state: {error}")

smooth = state["smooth_scroll"]
required = (
    "ticks",
    "touch_began",
    "touch_changed",
    "touch_ended",
    "momentum_began",
    "momentum_changed",
    "momentum_ended",
)
missing = [key for key in required if key not in smooth]
if missing:
    raise SystemExit(f"smooth-scroll counters missing from e2e-state: {missing}")
if (
    smooth["ticks"] <= 0
    or smooth["touch_began"] < 1
    or smooth["momentum_began"] < 1
    or smooth["momentum_ended"] < 1
):
    raise SystemExit(f"smooth-scroll phase sequence incomplete: {smooth}")
after_active = state.get("frontmost", {}).get("pid")
if before_active != after_active:
    raise SystemExit(
        f"background wheel changed the frontmost pid: before={before_active} after={after_active}"
    )
print(
    "e2e smooth-scroll: PASS: "
    f"active pid stayed {after_active}; ticks={smooth['ticks']}; phases="
    + ",".join(f"{key}={smooth[key]}" for key in required[1:])
)
PY
