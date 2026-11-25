#!/bin/bash
# M2: the REAL Dock entry — hover a real Dock tile with the real pointer, get the preview panel, put the
# pointer on a truncated card title and get the full-title tooltip over THAT item, prove a complete title
# never shows one, then press the Dock icon (down only) and prove the panel leaves on the press, the tile's
# app comes forward on the release, and the panel does not reopen while the pointer stays on the icon.
#
# Why a real pointer and a real press: the preview panel is a floating-level NSPanel that neither
# `list_windows` nor the accessibility window list reports, and the tooltip is a second one; the entry
# chain and the Dock tap can only be judged from injected input plus the app's own `--e2e-state` facts
# (items' frames/title rects/`tooltip_rect`) and the WindowServer's window list.
#
# Why the tile is discovered instead of named: the Dock's contents belong to the person using the
# machine. The scenario walks the tiles the app itself reports, hovers each, and scores the candidates:
# a truncated-title card whose centre can be told apart from the panel's centre (otherwise the old
# panel-centred anchor would pass), a complete-title item for the negative, and a press target whose app
# is NOT already frontmost (otherwise the release proves nothing). A machine where the Dock cannot supply
# all three is a limitation of the environment, so it is reported as NOT RUN — never as a pass — and a
# failure recorded earlier still wins over NOT RUN.
#
# 本场景会注入真实指针与真实按下/抬起事件：移动用户的鼠标、切换前台 app，并替换正在运行的实例。
# This scenario moves the real pointer, injects a real press and release (which activates a real app), and
# replaces the running instance with a `--e2e-state` build. It does not capture the screen (it reads window
# metadata only), so it declares no screen gate.
# E2E_STEALS_FOCUS=1
# E2E_REPLACES_INSTANCE=1
set -euo pipefail

repo_dir="$(cd "$(dirname "$0")/../.." && pwd)"
source "$repo_dir/scripts/lib/e2e-restore.sh"

state="$(mktemp -t dock-preview-e2e).json"
fail=0
focus_before=""
cursor_x=""
cursor_y=""
last_x=""
last_y=""
scale=2.0
tile_app_pid=""
mouse_down=0
down_point_x=""
down_point_y=""
instance_pid=""
instance_started=""
instance_touched=0
keep_instance=0
previous_tooltip_rect=""
front_moved_by_scenario=0
sampler_pid=""

problem() {
    echo "dock-preview: $1"
    fail=1
}

# NOT RUN is only allowed while nothing failed: a scenario that already recorded a failure must not hand
# the aggregate an "unrun" verdict for a run that did observe a defect.
not_run() {  # <reason>
    echo "NOT RUN: $1"
    if [ "$fail" -ne 0 ]; then
        echo "dock-preview: FAILED (a check failed before the case that could not decide)"
        exit 1
    fi
    exit 0
}

# Every exit path: release a press the scenario started (a down without its up leaves the Dock mid-gesture),
# put the pointer back while it is still where the scenario left it, relaunch the app clean so no
# `--e2e-state` instance stays behind (unless the instance was replaced under the run, in which case the
# replacement is left in place), and hand the front back only if the scenario's own click moved it.
cleanup() {
    local status=$?
    cd "$repo_dir" || {
        echo "dock-preview: cannot return to $repo_dir to restore the app"
        exit 1
    }
    if [ -n "$sampler_pid" ]; then
        kill "$sampler_pid" 2>/dev/null || true
        wait "$sampler_pid" 2>/dev/null || true
        sampler_pid=""
    fi
    if [ "$mouse_down" = "1" ] && [ -n "$down_point_x" ]; then
        if ! mouse up "$down_point_x" "$down_point_y" 2>/dev/null; then
            echo "dock-preview: the release of the injected press could not be posted; the Dock may still be mid-gesture" >&2
            [ "$status" -ne 0 ] || status=1
        fi
        mouse_down=0
    fi
    if [ -n "$last_x" ] && [ -n "$last_y" ]; then
        local cursor_rc=0
        omt_restore_cursor "${cursor_x:-}" "${cursor_y:-}" "$last_x" "$last_y" 12 || cursor_rc=$?
        case "$cursor_rc" in
            0|2) ;;
            *) [ "$status" -ne 0 ] || status=1 ;;
        esac
    fi
    if [ "$instance_touched" = "1" ] && [ "$keep_instance" = "0" ]; then
        # The relaunch is a transaction on an instance this run launched: re-check the identity first, so a
        # replacement that happened while the scenario ran (or before an early exit) is left alone.
        local ident_rc=0
        if [ -n "$instance_pid" ] && [ -n "$instance_started" ]; then
            ./scripts/dev-restart.sh --identity-expects="$instance_pid,$instance_started" >/dev/null 2>&1 || ident_rc=$?
        else
            ident_rc=4
        fi
        if [ "$ident_rc" -ne 0 ]; then
            keep_instance=1
            echo "dock-preview: the instance's identity no longer holds (exit $ident_rc); leaving whatever runs now in place" >&2
            [ "$status" -ne 0 ] || status=1
        fi
    fi
    if [ "$instance_touched" = "1" ] && [ "$keep_instance" = "0" ]; then
        if ! scripts/dev-restart.sh --no-onboarding >/dev/null 2>&1; then
            echo "dock-preview: the clean relaunch FAILED; the app may still carry --e2e-state" >&2
            [ "$status" -ne 0 ] || status=1
        fi
    elif [ "$keep_instance" = "1" ]; then
        echo "dock-preview: the instance was replaced or could not be verified; leaving the running instance in place"
    fi
    if [ -n "$focus_before" ] && [ "$front_moved_by_scenario" = "1" ] && [ -n "$tile_app_pid" ]; then
        # The shared helper hands back by pid, which an app with several windows refuses as ambiguous
        # (measured with Edge: it answered with the candidate list instead of activating). The window the
        # user was looking at is the one to restore, so use its exact id when it was captured.
        local front_rc=0 current=""
        if [ -n "$focus_window_id" ]; then
            current="$(omt_front_pid 2>/dev/null || true)"
            if [ "$current" = "$focus_before" ]; then
                front_rc=0
            elif [ "$current" = "$tile_app_pid" ]; then
                if ! omt_cua bring_to_front "{\"pid\": $focus_before, \"window_id\": $focus_window_id}" >/dev/null 2>&1; then
                    front_rc=1
                    echo "dock-preview: could not hand the front back to pid $focus_before (window $focus_window_id)" >&2
                fi
            else
                front_rc=2
                echo "dock-preview: pid ${current:-unknown} is frontmost and this scenario did not bring it there; keeping the user's choice"
            fi
        else
            omt_hand_front_back "$focus_before" "$tile_app_pid" || front_rc=$?
        fi
        case "$front_rc" in
            0|2) ;;
            *) [ "$status" -ne 0 ] || status=1 ;;
        esac
    fi
    if [ "$keep_instance" = "0" ]; then
        local pid
        for pid in $(pgrep -x oh-my-tab || true); do
            if ps -o command= -p "$pid" | tr ' ' '\n' | grep -qE '^--(e2e-state|show-dock-preview)'; then
                echo "dock-preview: pid $pid is still running with a development switch after the restore" >&2
                [ "$status" -ne 0 ] || status=1
            fi
        done
    fi
    rm -f "$state"
    exit "$status"
}

cd "$repo_dir"

# Preconditions BEFORE the trap: a missing tool or an unreadable baseline must not restart an app the
# scenario has not even launched.
command -v cua-driver >/dev/null 2>&1 || {
    echo "dock-preview: cua-driver CLI not found in PATH"
    exit 1
}
cua-driver status >/dev/null 2>&1 || {
    echo "dock-preview: cua-driver daemon is not running"
    exit 1
}
command -v swift >/dev/null 2>&1 || {
    echo "dock-preview: the Swift toolchain is required for the WindowServer probe and the injected press"
    exit 1
}
command -v python3 >/dev/null 2>&1 || {
    echo "dock-preview: python3 is required"
    exit 1
}

cua() {  # <tool> <json args>
    python3 - "$@" <<'PY'
import subprocess, sys
completed = subprocess.run(
    ["cua-driver", "call", sys.argv[1], sys.argv[2]],
    capture_output=True, text=True,
)
if completed.returncode != 0:
    raise SystemExit(f"cua-driver {sys.argv[1]} failed: {completed.stderr.strip()[:400]}")
print(completed.stdout)
PY
}

# Poll the app's state document for the frame that carries the fact under test. A half-written file is
# "not yet", never a failure.
wait_state() {  # <python expression over s> [timeout seconds, fractional allowed]
    local expression="$1" timeout_s="${2:-25}" deadline
    deadline="$(python3 -c 'import sys, time; print(time.time() + float(sys.argv[1]))' "$timeout_s")"
    while :; do
        if python3 - "$state" "$expression" <<'PY' 2>/dev/null
import json, sys
try:
    s = json.load(open(sys.argv[1]))
except Exception:
    raise SystemExit(1)
try:
    verdict = bool(eval(sys.argv[2], {"s": s}))
except Exception:
    raise SystemExit(1)
raise SystemExit(0 if verdict else 1)
PY
        then
            return 0
        fi
        if ! python3 -c 'import sys, time; sys.exit(0 if time.time() < float(sys.argv[1]) else 1)' "$deadline"; then
            return 1
        fi
        sleep 0.1
    done
}

state_value() {  # <python expression over s, printed>
    python3 - "$state" "$1" <<'PY'
import json, sys
try:
    s = json.load(open(sys.argv[1]))
except Exception:
    raise SystemExit(1)
try:
    print(eval(sys.argv[2], {"s": s}))
except Exception:
    raise SystemExit(1)
PY
}

# The real pointer, in DESKTOP pixels (`move_cursor` converts to points itself); state rectangles are
# points, so every move multiplies by the display's scale.
move_pointer() {  # <x_pt> <y_pt>
    local px py
    px="$(python3 -c 'import sys; print(round(float(sys.argv[1]) * float(sys.argv[3])))' "$1" "$2" "$scale")"
    py="$(python3 -c 'import sys; print(round(float(sys.argv[2]) * float(sys.argv[3])))' "$1" "$2" "$scale")"
    cua move_cursor "{\"scope\": \"desktop\", \"x\": $px, \"y\": $py}" >/dev/null
    last_x="$px"
    last_y="$py"
}

# The press and the release. `cua-driver` has no down/up split, so raw CGEvent posts are the only way to
# observe the panel leaving on the PRESS. They are posted as ONE short gesture (a hand's click): a press
# held for about a second makes the Dock open the app's menu, and the release then dismisses the menu
# instead of activating the app (measured: the same press with a ~0.5s hold activates, with ~1.5s does
# not). Coordinates here are POINTS, unlike `move_cursor`.
# A `.hidSystemState` source and the click count are both required for the Dock to treat the pair as a
# real click (measured: without them the release activates nothing).
mouse() {  # <down|up> <x_pt> <y_pt>
    local kind="$1"
    swift - "$kind" "$2" "$3" <<'SWIFT'
import CoreGraphics
import Foundation

let args = CommandLine.arguments
guard args.count == 4, let x = Double(args[2]), let y = Double(args[3]) else { exit(2) }
let type: CGEventType = (args[1] == "down") ? .leftMouseDown : .leftMouseUp
let source = CGEventSource(stateID: .hidSystemState)
guard let event = CGEvent(mouseEventSource: source, mouseType: type, mouseCursorPosition: CGPoint(x: x, y: y), mouseButton: .left) else {
    exit(1)
}
event.setIntegerValueField(.mouseEventClickState, value: 1)
event.post(tap: .cghidEventTap)
SWIFT
}

click() {  # <x_pt> <y_pt> — one short press-and-release, posted from one process
    swift - "$1" "$2" <<'SWIFT'
import CoreGraphics
import Foundation

let args = CommandLine.arguments
guard args.count == 3, let x = Double(args[1]), let y = Double(args[2]) else { exit(2) }
let source = CGEventSource(stateID: .hidSystemState)
let point = CGPoint(x: x, y: y)
guard let down = CGEvent(mouseEventSource: source, mouseType: .leftMouseDown, mouseCursorPosition: point, mouseButton: .left),
      let up = CGEvent(mouseEventSource: source, mouseType: .leftMouseUp, mouseCursorPosition: point, mouseButton: .left) else {
    exit(1)
}
for event in [down, up] {
    event.setIntegerValueField(.mouseEventClickState, value: 1)
}
down.post(tap: .cghidEventTap)
Thread.sleep(forTimeInterval: 0.18)
up.post(tap: .cghidEventTap)
SWIFT
}

# Every on-screen window of one process, straight from the WindowServer: "layer x y w h" (points, top-left).
app_windows() {  # <pid>
    swift - "$1" <<'SWIFT'
import CoreGraphics
import Foundation

let pid = Int(CommandLine.arguments[1]) ?? 0
let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] ?? []
for window in list {
    guard let owner = window[kCGWindowOwnerPID as String] as? Int, owner == pid else { continue }
    let bounds = window[kCGWindowBounds as String] as? [String: CGFloat] ?? [:]
    let layer = window[kCGWindowLayer as String] as? Int ?? -1
    print(layer, bounds["X"] ?? -1, bounds["Y"] ?? -1, bounds["Width"] ?? -1, bounds["Height"] ?? -1)
}
SWIFT
}

# The panel's items as "index wid truncated title_cx title_cy frame_x frame_y frame_w frame_h" (points).
read_items() {
    python3 - "$state" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))
p = (s.get('dock_preview') or {}).get('panel') or {}
for index, item in enumerate(p.get('rows') or []):
    title = item.get('title_rect') or {}
    frame = item.get('frame') or {}
    print(
        index,
        item.get('wid'),
        1 if item.get('truncated') else 0,
        round(float(title.get('x', 0)) + float(title.get('w', 0)) / 2.0, 1),
        round(float(title.get('y', 0)) + float(title.get('h', 0)) / 2.0, 1),
        round(float(frame.get('x', 0)), 1),
        round(float(frame.get('y', 0)), 1),
        round(float(frame.get('w', 0)), 1),
        round(float(frame.get('h', 0)), 1),
    )
PY
}

# One Dock tile's frame from the current state: "x y w h" (points), or nothing when the tile is gone.
bundle_frame() {  # <bundle>
    python3 - "$state" "$1" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))
for item in (((s.get('dock_preview') or {}).get('dock') or {}).get('items') or []):
    if item.get('bundle') == sys.argv[2]:
        frame = item.get('frame') or {}
        print(frame.get('x', 0), frame.get('y', 0), frame.get('w', 0), frame.get('h', 0))
        raise SystemExit(0)
raise SystemExit(1)
PY
}

# The current panel's bundle, centre x, pid, hovered index and tooltip state in one line, or nothing.
panel_facts() {
    python3 - "$state" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))
p = (s.get('dock_preview') or {}).get('panel')
if not p:
    print("")
    raise SystemExit(0)
panel = p.get('panel') or {}
print(
    p.get('bundle', ''),
    round(float(panel.get('x', 0)) + float(panel.get('w', 0)) / 2.0, 1),
    p.get('pid', 0),
    p.get('hovered') if p.get('hovered') is not None else -1,
    1 if p.get('tooltip_visible') else 0,
)
PY
}

# The frontmost application, read from the system rather than from the app's own snapshot.
focus_before="$(omt_front_pid 2>/dev/null)" || focus_before=""
if [ -z "$focus_before" ]; then
    echo "dock-preview: the frontmost application could not be read; refusing to run without a baseline" >&2
    exit 1
fi
# The front window the user was looking at: the shared restore hands back by pid, which an app with
# several windows refuses as ambiguous, so the exact window id is the fallback.
front_window_id() {  # <pid>
    swift - "$1" <<'SWIFT'
import CoreGraphics
import Foundation

let pid = Int(CommandLine.arguments[1]) ?? 0
let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] ?? []
for window in list {
    guard let owner = window[kCGWindowOwnerPID as String] as? Int, owner == pid,
          let layer = window[kCGWindowLayer as String] as? Int, layer == 0,
          let number = window[kCGWindowNumber as String] as? Int else { continue }
    print(number)
    break
}
SWIFT
}
focus_window_id="$(front_window_id "$focus_before" 2>/dev/null || true)"
echo "  front baseline: pid $focus_before, window ${focus_window_id:-none}"
cursor_px="$(omt_cursor_px 2>/dev/null)" || cursor_px=""
read -r cursor_x cursor_y <<<"$cursor_px"
if [ -z "${cursor_x:-}" ] || [ -z "${cursor_y:-}" ]; then
    echo "dock-preview: the pointer position could not be read; refusing to move it without a baseline" >&2
    exit 1
fi

trap cleanup EXIT
trap 'exit 130' INT TERM

sleep 1
: >"$state"
log="$(mktemp -t dock-preview-launch)"
echo "dock-preview: launching with --e2e-state"
instance_touched=1
if ! scripts/dev-restart.sh --no-onboarding --e2e-state="$state" >"$log" 2>&1; then
    echo "dock-preview: dev-restart.sh failed:"
    tail -n 20 "$log"
    exit 1
fi
build_version="$(sed -n 's/^build-version: //p' "$log" | tail -1)"
launched_pid="$(sed -n 's/^restart ok (app pid \([0-9][0-9]*\)[,)].*/\1/p' "$log" | tail -1)"
rm -f "$log"
echo "  build-version: ${build_version:-unverified}"
if [ -z "$launched_pid" ]; then
    echo "dock-preview: dev-restart.sh did not report the app pid it launched; the instance cannot be bound"
    keep_instance=1
    exit 1
fi

# The identity every observation below belongs to: the state's own pid must be the instance
# `--whose-instance` reports, and the pair is re-checked with `--identity-expects` before the verdict.
identity_rc=0
identity="$(./scripts/dev-restart.sh --whose-instance 2>&1)" || identity_rc=$?
if [ "$identity_rc" -ne 0 ]; then
    echo "dock-preview: --whose-instance exited $identity_rc, so the instance cannot be verified:"
    printf '%s\n' "$identity" | sed 's/^/    /'
    keep_instance=1
    exit 1
fi
case "$identity" in
    *"VERDICT: MINE"*) ;;
    *)
        echo "dock-preview: --whose-instance did not report this worktree's instance:"
        printf '%s\n' "$identity" | sed 's/^/    /'
        keep_instance=1
        exit 1
        ;;
esac
instance_pid="$(printf '%s\n' "$identity" | sed -n 's/^instance: pid \([0-9][0-9]*\),.*/\1/p' | head -1)"
instance_started="$(printf '%s\n' "$identity" | sed -n 's/^instance: pid [0-9][0-9]*, started \(.*\)$/\1/p' | head -1)"
if [ -z "$instance_pid" ] || [ -z "$instance_started" ]; then
    echo "dock-preview: --whose-instance did not name the pid and started time; the observation has no identity"
    keep_instance=1
    exit 1
fi
if [ "$instance_pid" != "$launched_pid" ]; then
    # The probe answered for a process this run did not launch: another start replaced ours between the
    # launch and the probe, so nothing observed from here belongs to this scenario.
    echo "dock-preview: --whose-instance reports pid $instance_pid, but this run launched pid $launched_pid; another start replaced it"
    keep_instance=1
    exit 1
fi
echo "  instance: pid $instance_pid, started $instance_started (launched by this run)"

if ! wait_state "(s.get('dock_preview') or {}).get('dock')" 20; then
    echo "dock-preview: the app never published a Dock snapshot"
    exit 1
fi
app_pid="$(state_value "s.get('pid')")"
if [ "$app_pid" != "$instance_pid" ]; then
    echo "dock-preview: the state document (pid $app_pid) is not the instance --whose-instance reported (pid $instance_pid)"
    exit 1
fi

scale="$(cua get_screen_size '{}' | python3 -c 'import json,sys; print(json.load(sys.stdin)["scale_factor"])')"
read -r screen_w screen_h < <(cua get_screen_size '{}' | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d["width"], d["height"])')
screen_x="$(state_value "((s['dock_preview']['dock'] or {}).get('screen') or {}).get('x', 0.0)")"
screen_y="$(state_value "((s['dock_preview']['dock'] or {}).get('screen') or {}).get('y', 0.0)")"
screen_w_state="$(state_value "((s['dock_preview']['dock'] or {}).get('screen') or {}).get('w', $screen_w)")"
screen_h_state="$(state_value "((s['dock_preview']['dock'] or {}).get('screen') or {}).get('h', $screen_h)")"

if [ "$(state_value "bool((s['dock_preview']['dock'] or {}).get('enabled'))")" != "True" ]; then
    not_run "the Dock previews are off in this configuration, so the real Dock entry cannot be exercised"
fi

# An auto-hidden Dock parks outside the screen; nudge the pointer to where the Dock list is and wait for it
# to come out. A Dock that never reveals is an environment refusal, not a defect.
if [ "$(state_value "bool((s['dock_preview']['dock'] or {}).get('revealed'))")" != "True" ]; then
    list_x="$(state_value "((s['dock_preview']['dock'] or {}).get('list') or {}).get('x', 0.0)")"
    list_w="$(state_value "((s['dock_preview']['dock'] or {}).get('list') or {}).get('w', 0.0)")"
    echo "  the Dock is not revealed; nudging it out"
    move_pointer "$(python3 -c 'import sys; print(float(sys.argv[1]) + float(sys.argv[2]) / 2.0)' "$list_x" "$list_w")" \
        "$(python3 -c 'import sys; print(float(sys.argv[1]) - 4.0)' "$screen_h")"
    if ! wait_state "bool((s['dock_preview']['dock'] or {}).get('revealed'))" 4; then
        not_run "the Dock did not reveal its list, so no tile can be hovered"
    fi
fi

if ! wait_state "bool((s['dock_preview']['dock'] or {}).get('items'))" 5; then
    not_run "the Dock reports no tiles with window frames"
fi

# Positive control before the scan: at least one Dock tile's app must have an on-screen window to
# preview. Without that, "no preview opened" is the machine having nothing to show, not the entry failing.
apps_json="$(cua list_apps '{}')"
owner_pids="$(swift - <<'SWIFT'
import CoreGraphics
import Foundation

let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] ?? []
var seen = Set<Int>()
for window in list {
    guard let owner = window[kCGWindowOwnerPID as String] as? Int,
          let layer = window[kCGWindowLayer as String] as? Int, layer == 0, !seen.contains(owner) else { continue }
    seen.insert(owner)
    print(owner)
}
SWIFT
)"
previewable_file="$(mktemp -t dock-preview-previewable)"
if ! python3 - "$state" "$owner_pids" "$apps_json" "$previewable_file" <<'PY'
import json, sys
state = json.load(open(sys.argv[1]))
owners = {int(line) for line in sys.argv[2].split() if line.strip().isdigit()}
apps = json.loads(sys.argv[3]).get('apps', [])
bundle_to_pid = {}
for app in apps:
    bundle = app.get('bundle_id') or ''
    pid = app.get('pid')
    if bundle and isinstance(pid, int) and pid > 0:
        bundle_to_pid.setdefault(bundle, pid)
tiles = (((state.get('dock_preview') or {}).get('dock') or {}).get('items') or [])
previewable = [item.get('bundle') for item in tiles if bundle_to_pid.get(item.get('bundle') or '') in owners]
with open(sys.argv[4], 'w') as out:
    for bundle in previewable:
        out.write(bundle + "\n")
print("previewable tiles: %d" % len(previewable), file=sys.stderr)
raise SystemExit(0 if previewable else 1)
PY
then
    rm -f "$previewable_file"
    not_run "no Dock tile's app has an on-screen window to preview, so the real entry cannot be exercised"
fi

# Start the scan from a point that is not the Dock: a pointer already resting on a tile when the app
# launched produces no enter notification for that tile, and the loop would read "no panel" for it.
move_pointer "$(python3 -c 'import sys; print(float(sys.argv[1]) / 2.0)' "$screen_w_state")" 220
sleep 0.6
tiles="$(mktemp -t dock-preview-tiles)"
python3 - "$state" "$tiles" "$previewable_file" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))
dock = (s.get('dock_preview') or {}).get('dock') or {}
try:
    preferred = {line.strip() for line in open(sys.argv[3]) if line.strip()}
except Exception:
    preferred = set()
rows = []
for item in dock.get('items') or []:
    bundle = item.get('bundle') or ''
    frame = item.get('frame') or {}
    if not bundle or float(frame.get('w', 0)) <= 1.0:
        continue
    rows.append((bundle, frame.get('x', 0), frame.get('y', 0), frame.get('w', 0), frame.get('h', 0)))
# The positive-control tile is scanned first, so a Dock whose later tiles are folders or separators can
# never push the one known-previewable icon past the attempt cap.
rows.sort(key=lambda row: 0 if row[0] in preferred else 1)
with open(sys.argv[2], 'w') as out:
    for row in rows:
        out.write("{} {:.1f} {:.1f} {:.1f} {:.1f}\n".format(*row))
PY
rm -f "$previewable_file"

# Candidate tables (tab separated): bundle, wid, tile x, tile y, item cx, item cy, panel centre, pid.
trunc_candidates="$(mktemp -t dock-preview-trunc)"
complete_candidates="$(mktemp -t dock-preview-complete)"
press_bundle=""
press_pid=""
press_tile_x=""
press_tile_y=""
attempts=0
checked=0
for sweep in 1 2; do
    [ "$checked" -gt 0 ] && break
    if [ "$sweep" = "2" ]; then
        echo "  no preview opened on the first sweep; moving away and sweeping the tiles again"
        move_pointer "$(python3 -c 'import sys; print(float(sys.argv[1]) / 2.0)' "$screen_w_state")" 220
        sleep 0.8
    fi
    attempts=0
    while read -r bundle tx ty tw th; do
    [ -n "$bundle" ] || continue
    attempts=$((attempts + 1))
    if [ "$attempts" -gt 16 ]; then
        break
    fi
    tile_x="$(python3 -c 'import sys; print(float(sys.argv[1]) + float(sys.argv[3]) / 2.0)' "$tx" "$ty" "$tw")"
    tile_y="$(python3 -c 'import sys; print(float(sys.argv[2]) + float(sys.argv[4]) / 2.0)' "$tx" "$ty" "$tw" "$th")"
    move_pointer "$tile_x" "$tile_y"
    if ! wait_state "((s.get('dock_preview') or {}).get('panel') or {}).get('bundle') == '$bundle'" 2; then
        continue
    fi
    checked=$((checked + 1))
    facts="$(panel_facts)"
    read -r _bundle panel_center panel_pid _hovered _tt <<<"$facts"
    echo "  tile $bundle: preview open (panel centre ${panel_center}, pid ${panel_pid})"
    # The press case needs only "a preview whose app is not frontmost": a release onto the app that is
    # already in front would prove nothing about activation, and the title's truncation is irrelevant here.
    if [ -z "$press_bundle" ] && [ "$panel_pid" != "$focus_before" ]; then
        press_bundle="$_bundle"
        press_pid="$panel_pid"
        press_tile_x="$tile_x"
        press_tile_y="$tile_y"
        echo "    press candidate: $_bundle (pid $panel_pid, not frontmost)"
    fi
    while read -r index wid truncated cx cy _fx _fy _fw _fh; do
        # An item whose centre equals the panel centre cannot tell the two anchors apart, so it is not a
        # candidate for the positive case.
        if [ "$truncated" = "1" ] && python3 -c 'import sys; sys.exit(0 if abs(float(sys.argv[1]) - float(sys.argv[2])) > 40.0 else 1)' "$cx" "$panel_center"; then
            printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$bundle" "$wid" "$tile_x" "$tile_y" "$cx" "$cy" "$panel_center" "$panel_pid" >>"$trunc_candidates"
            echo "    distinguishable truncated title: wid=$wid item centre $cx vs panel centre $panel_center"
        elif [ "$truncated" = "1" ]; then
            echo "    truncated title wid=$wid skipped: its centre equals the panel centre"
        fi
        if [ "$truncated" = "0" ]; then
            printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$bundle" "$wid" "$tile_x" "$tile_y" "$cx" "$cy" "$panel_center" "$panel_pid" >>"$complete_candidates"
        fi
    done < <(read_items)
    done <"$tiles"
done
rm -f "$tiles"
echo "  previews inspected: $checked tile(s)"

if [ "$checked" -eq 0 ]; then
    rm -f "$trunc_candidates" "$complete_candidates"
    # The prerequisites (previews enabled, Dock revealed, tiles published) all held, so a preview that
    # never opens for any tile is the entry failing -- an open Dock menu makes it look the same, and the
    # message says so, but it is not reported as an environment refusal.
    problem "no Dock tile opened a preview although the previews are enabled and the Dock is revealed (an open Dock menu produces the same picture)"
    exit 1
fi
if [ ! -s "$trunc_candidates" ]; then
    rm -f "$trunc_candidates" "$complete_candidates"
    not_run "no Dock tile's preview had a truncated title whose centre differs from the panel's, so the tooltip anchor cannot be judged"
fi
if [ ! -s "$complete_candidates" ]; then
    rm -f "$trunc_candidates" "$complete_candidates"
    not_run "no Dock tile's preview had a complete title, so the negative case cannot decide"
fi

# The tooltip candidate: the truncated item whose centre is farthest from the panel centre (the most
# discriminating one).
read -r trunc_bundle trunc_wid trunc_tile_x trunc_tile_y trunc_cx trunc_cy trunc_panel_center trunc_pid \
    < <(python3 - "$trunc_candidates" <<'PY'
import sys
rows = [line.rstrip("\n").split("\t") for line in open(sys.argv[1]) if line.strip()]
best = max(rows, key=lambda r: abs(float(r[4]) - float(r[6])))
print("\t".join(best))
PY
)
if [ -z "$press_bundle" ]; then
    rm -f "$trunc_candidates" "$complete_candidates"
    not_run "every preview belongs to the app that is already frontmost, so the release cannot prove activation"
fi
read -r complete_bundle complete_wid complete_tile_x complete_tile_y complete_cx complete_cy _cp _cpid \
    < <(head -1 "$complete_candidates" | tr '\t' ' ')
rm -f "$trunc_candidates" "$complete_candidates"

echo "dock-preview: tooltip case on $trunc_bundle (wid $trunc_wid, item centre $trunc_cx, panel centre $trunc_panel_center)"
move_pointer "$trunc_tile_x" "$trunc_tile_y"
if ! wait_state "((s.get('dock_preview') or {}).get('panel') or {}).get('bundle') == '$trunc_bundle'" 3; then
    problem "the preview did not come back for $trunc_bundle after re-hovering the tile"
else
    facts="$(panel_facts)"
    read -r _bundle _centre _pid hovered _tt <<<"$facts"
    row="$(read_items | awk -v w="$trunc_wid" '$2 == w && $3 == 1 { print; exit }')"
    if [ -z "$row" ]; then
        problem "the truncated item $trunc_wid is gone from the reopened preview"
    else
        read -r trunc_index _wid _truncated trunc_cx trunc_cy _fx _fy _fw _fh <<<"$row"
        move_pointer "$trunc_cx" "$trunc_cy"
        if ! wait_state "((s.get('dock_preview') or {}).get('panel') or {}).get('tooltip_visible')" 4; then
            problem "the full-title tooltip did not appear over the truncated title"
        else
            facts="$(panel_facts)"
            read -r _bundle panel_center _pid hovered _tt <<<"$facts"
            if [ "$hovered" != "$trunc_index" ]; then
                problem "the tooltip is up while the panel reports item $hovered hovered, not the truncated item $trunc_index"
            fi
            verdict="$(python3 - "$state" "$trunc_wid" "$screen_x" "$screen_y" "$screen_w_state" "$screen_h_state" "$panel_center" "$trunc_bundle" "$trunc_pid" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))
wid, sx, sy, sw, sh, panel_center = sys.argv[2], *(float(v) for v in sys.argv[3:])
expect_bundle, expect_pid = sys.argv[8], sys.argv[9]
p = (s.get('dock_preview') or {}).get('panel') or {}
tt = p.get('tooltip_rect')
panel = p.get('panel') or {}
# Everything below must describe ONE frame: the same bundle and pid, the hovered index naming the wid
# under test, that item truncated, and the tooltip rectangle read from it.
if str(p.get('bundle')) != expect_bundle:
    print(f"BAD the panel frame is for bundle {p.get('bundle')}, not {expect_bundle}")
    raise SystemExit(0)
if str(p.get('pid')) != expect_pid:
    print(f"BAD the panel frame is for pid {p.get('pid')}, not {expect_pid}")
    raise SystemExit(0)
hovered = p.get('hovered')
rows = p.get('rows') or []
if hovered is None or hovered < 0 or hovered >= len(rows):
    print(f"BAD the frame reports no hovered item ({hovered})")
    raise SystemExit(0)
row = rows[hovered]
if str(row.get('wid')) != str(wid) or not row.get('truncated'):
    print(f"BAD the hovered item is wid {row.get('wid')} (truncated {row.get('truncated')}), not the truncated {wid}")
    raise SystemExit(0)
frame = row.get('frame') or {}
if not tt:
    print("BAD the state reports no tooltip rectangle while the tooltip is visible")
    raise SystemExit(0)
if tt['x'] < sx - 0.5 or tt['x'] + tt['w'] > sx + sw + 0.5 or tt['y'] < sy - 0.5 or tt['y'] + tt['h'] > sy + sh + 0.5:
    print(f"BAD the tooltip is outside the screen: {tt} in ({sx},{sy},{sw},{sh})")
    raise SystemExit(0)
above = tt['y'] + tt['h'] <= panel.get('y', 0) + 0.5
below = tt['y'] >= panel.get('y', 0) + panel.get('h', 0) - 0.5
if not above and not below:
    print(f"BAD the tooltip overlaps the panel: {tt} vs {panel}")
    raise SystemExit(0)
item_center = frame['x'] + frame['w'] / 2.0
tt_center = tt['x'] + tt['w'] / 2.0

def expected_x(centre):
    return max(min(centre - tt['w'] / 2.0, sx + sw - tt['w']), sx)

# The candidate has to be able to tell the two anchors apart after the clamp: with the MEASURED width,
# centring on the item and centring on the panel must not land on the same x (a wide tooltip near an edge
# can push both to the same boundary, and then the old implementation would pass).
if abs(expected_x(item_center) - expected_x(panel_center)) <= 2.0:
    print(f"INDECISIVE the two anchors clamp to the same x with a {tt['w']:.0f}pt tooltip (item {item_center:.1f}, panel {panel_center:.1f})")
    raise SystemExit(0)
centred = abs(tt_center - item_center) <= 1.5
clamped = (tt['x'] <= sx + 0.5 and item_center - tt['w'] / 2.0 < sx) or \
          (tt['x'] + tt['w'] >= sx + sw - 0.5 and item_center + tt['w'] / 2.0 > sx + sw)
if not centred and not clamped:
    print(f"BAD the tooltip is not over the hovered item: tooltip centre {tt_center:.1f}, item centre {item_center:.1f}, panel centre {panel_center:.1f}")
    raise SystemExit(0)
print(f"OK tooltip {tt['x']:.1f},{tt['y']:.1f} {tt['w']:.1f}x{tt['h']:.1f} centred over item centre {item_center:.1f} (panel centre {panel_center:.1f}); {'above' if above else 'below'} the panel")
print("RECT " + json.dumps({'x': tt['x'], 'y': tt['y'], 'w': tt['w'], 'h': tt['h']}))
PY
)"
            verdict_head="$(printf '%s\n' "$verdict" | head -1)"
            previous_tooltip_rect="$(printf '%s\n' "$verdict" | sed -n 's/^RECT //p')"
            case "$verdict_head" in
                OK*) echo "  ${verdict_head#OK }" ;;
                INDECISIVE*) not_run "${verdict_head#INDECISIVE }" ;;
                *) problem "${verdict_head#BAD }" ;;
            esac
            # The WindowServer's own view of the tooltip: a window of this app at that rectangle.
            windows_file="$(mktemp -t dock-preview-windows)"
            app_windows "$app_pid" >"$windows_file"
            if ! python3 - "$previous_tooltip_rect" "$windows_file" <<'PY'
import sys
rect = eval(sys.argv[1])
lines = [line.split() for line in open(sys.argv[2]).read().strip().splitlines() if line.strip()]
want = (rect['x'], rect['y'], rect['w'], rect['h'])
for parts in lines:
    if len(parts) != 5:
        continue
    layer = int(parts[0])
    x, y, w, h = (float(v) for v in parts[1:])
    if layer > 0 and all(abs(b - a) <= 1.0 for a, b in zip((x, y, w, h), want)):
        raise SystemExit(0)
raise SystemExit(1)
PY
            then
                problem "the tooltip window is not visible to the WindowServer at the rect the app reports"
            else
                echo "  the WindowServer reports the tooltip window at the reported rect"
            fi
            rm -f "$windows_file"
        fi
    fi
fi

# --- a complete title never shows one -------------------------------------------------------
echo "dock-preview: complete-title case on $complete_bundle (wid $complete_wid)"
if [ "$(panel_facts | cut -f1)" != "$complete_bundle" ]; then
    move_pointer "$complete_tile_x" "$complete_tile_y"
    if ! wait_state "((s.get('dock_preview') or {}).get('panel') or {}).get('bundle') == '$complete_bundle'" 3; then
        problem "the preview did not come back for $complete_bundle after re-hovering the tile"
    fi
fi
row="$(read_items | awk -v w="$complete_wid" '$2 == w && $3 == 0 { print; exit }')"
if [ -z "$row" ]; then
    problem "the complete item $complete_wid is gone from the reopened preview"
else
    read -r complete_index complete_wid _truncated complete_cx complete_cy _fx _fy _fw _fh <<<"$row"
    move_pointer "$complete_cx" "$complete_cy"
    # It must go at once (the previous tooltip belongs to the previous title) ...
    if ! wait_state "((s.get('dock_preview') or {}).get('panel') or {}).get('tooltip_visible') == False" 1.5; then
        problem "a tooltip stayed up after moving onto a complete title"
    fi
    # ... and must still be absent after the full delay, with the SAME panel and the complete item hovered
    # (a panel that closed under the pointer must not read as "the tooltip hid").
    sleep 0.9
    facts="$(panel_facts)"
    read -r _bundle _centre _pid hovered tooltip_up <<<"$facts"
    if [ "$_bundle" != "$complete_bundle" ]; then
        problem "the panel is no longer showing $complete_bundle while the complete-title case waits"
    elif [ "$hovered" != "$complete_index" ]; then
        problem "the panel reports item $hovered hovered while the complete-title case rests on item $complete_index"
    elif [ "$tooltip_up" != "0" ]; then
        problem "a complete title showed a tooltip after the rest delay"
    fi
    # The previous tooltip's own window must be gone, read from the WindowServer rather than inferred from
    # a window's size.
    if [ -n "$previous_tooltip_rect" ]; then
        windows_file="$(mktemp -t dock-preview-windows)"
        app_windows "$app_pid" >"$windows_file"
        if ! python3 - "$previous_tooltip_rect" "$windows_file" <<'PY'
import sys
rect = eval(sys.argv[1])
lines = [line.split() for line in open(sys.argv[2]).read().strip().splitlines() if line.strip()]
want = (rect['x'], rect['y'], rect['w'], rect['h'])
for parts in lines:
    if len(parts) != 5:
        continue
    layer = int(parts[0])
    x, y, w, h = (float(v) for v in parts[1:])
    if layer > 0 and all(abs(b - a) <= 1.0 for a, b in zip((x, y, w, h), want)):
        raise SystemExit(1)
raise SystemExit(0)
PY
        then
            problem "the tooltip window is still on screen over a complete title"
        else
            echo "  no tooltip over the complete title (immediately, after the delay, and no window left)"
        fi
        rm -f "$windows_file"
    fi
fi

# --- the press: the panel leaves on the press, the click activates, no reopen -------------------
echo "dock-preview: clicking the $press_bundle tile (one short press-and-release)"
press_ready=1
# Always move back onto the icon: the panel may already be the target's while the pointer rests on a title
# inside it, and the click's meaning depends on the pointer being on the tile.
move_pointer "$press_tile_x" "$press_tile_y"
if ! wait_state "((s.get('dock_preview') or {}).get('panel') or {}).get('bundle') == '$press_bundle'" 3; then
    problem "the preview did not open for the press check"
    press_ready=0
fi
if [ "$press_ready" = "1" ]; then
    facts="$(panel_facts)"
    read -r panel_bundle _centre tile_app_pid _hovered _tt <<<"$facts"
    if [ "$panel_bundle" != "$press_bundle" ] || [ -z "$tile_app_pid" ]; then
        problem "the panel on screen is '${panel_bundle}' (pid ${tile_app_pid:-none}), not the press target $press_bundle"
        press_ready=0
    elif [ "$tile_app_pid" != "$press_pid" ]; then
        problem "the panel's app (pid $tile_app_pid) is not the press candidate's (pid $press_pid)"
        press_ready=0
    fi
    front_now="$(omt_front_pid 2>/dev/null || true)"
    if [ -z "$front_now" ]; then
        problem "the frontmost application could not be read before the press, so the click cannot prove activation"
        press_ready=0
    elif [ "$front_now" = "$tile_app_pid" ]; then
        problem "the press target's app (pid $tile_app_pid) is already frontmost, so the click cannot prove activation"
        press_ready=0
    fi
fi
if [ "$press_ready" = "1" ]; then
    dismissals_before="$(state_value "((s.get('dock_preview') or {}).get('press_dismissals') or 0)")"
    opens_before="$(state_value "((s.get('dock_preview') or {}).get('preview_opens') or 0)")"
    # The Dock settles its hover state before it treats a click as a launch: a press posted in the
    # fraction of a second after the tile's preview appears was dismissed by the app's tap but did not
    # activate the app (measured), while the same press after a rest activated it.
    sleep 0.9
    # Re-confirm what the 0.9s wait may have invalidated: the user can move the pointer, switch apps, and a
    # re-layout can move the tile. Nothing is injected unless all of it still holds.
    facts="$(panel_facts)"
    read -r panel_bundle _centre tile_app_pid_now _hovered _tt <<<"$facts"
    cursor_now="$(omt_cursor_px 2>/dev/null || true)"
    read -r cursor_now_x cursor_now_y <<<"$cursor_now"
    pointer_on_tile=0
    tile_now="$(bundle_frame "$press_bundle" 2>/dev/null || true)"
    if [ -n "$tile_now" ] && [ -n "${cursor_now_x:-}" ]; then
        # The cursor is compared against the tile's CURRENT rect from the state, in points: the Dock can
        # move a tile between the aim and the click.
        if python3 -c 'import sys; x,y,w,h,px,py,scale = (float(v) for v in sys.argv[1:8]); sys.exit(0 if x <= px/scale <= x+w and y <= py/scale <= y+h else 1)' \
            $tile_now "$cursor_now_x" "$cursor_now_y" "$scale" 2>/dev/null; then
            pointer_on_tile=1
        fi
    fi
    front_now="$(omt_front_pid 2>/dev/null || true)"
    if [ "$panel_bundle" != "$press_bundle" ] || [ "$tile_app_pid_now" != "$press_pid" ]; then
        problem "the press target changed during the settle (panel '${panel_bundle}', pid ${tile_app_pid_now:-none})"
        press_ready=0
    elif [ "$pointer_on_tile" != "1" ]; then
        problem "the pointer is no longer on the $press_bundle tile"
        press_ready=0
    elif [ -z "$front_now" ]; then
        problem "the frontmost application became unreadable during the settle"
        press_ready=0
    elif [ "$front_now" = "$tile_app_pid_now" ]; then
        problem "the press target's app became frontmost during the settle, so the click cannot prove activation"
        press_ready=0
    fi
    tile_app_pid="$tile_app_pid_now"
    # The press is injected at the VERIFIED live point, not at the scan-time tile centre: the Dock can move
    # a tile between the scan and the click, and the check above passed for the live point.
    click_x="$(python3 -c 'import sys; print(round(float(sys.argv[1]) / float(sys.argv[3]), 2))' "${cursor_now_x:-0}" "${cursor_now_y:-0}" "$scale")"
    click_y="$(python3 -c 'import sys; print(round(float(sys.argv[2]) / float(sys.argv[3]), 2))' "${cursor_now_x:-0}" "${cursor_now_y:-0}" "$scale")"
fi
if [ "$press_ready" = "1" ]; then
    # The press has to be observed BEFORE the release: a sampler runs through the whole click, reading the
    # panel and the front from the app's own state frames (the state exports `frontmost`), and the analysis
    # below only accepts a pre-release sample that already shows the panel gone.
    samples="$(mktemp -t dock-preview-samples)"
    : >"$samples"
    (
        while :; do
            python3 - "$state" >>"$samples" <<'PY' 2>/dev/null
import json, sys
try:
    s = json.load(open(sys.argv[1]))
except Exception:
    raise SystemExit(0)
dp = s.get('dock_preview') or {}
panel = 1 if dp.get('panel') else 0
front = ((s.get('frontmost') or {}).get('pid') or -1)
print(panel, front, s.get('seq', -1), dp.get('press_dismissals', -1))
PY
        done
    ) &
    sampler_pid=$!
    mouse_down=1
    if click "$click_x" "$click_y"; then
        mouse_down=0
    else
        # The helper may have posted the down and died before the up; the flag stays set so the cleanup
        # still posts the release (an extra up over nothing is harmless, a missing one is not).
        problem "the click could not be injected"
    fi
    sleep 3.0
    kill "$sampler_pid" 2>/dev/null || true
    wait "$sampler_pid" 2>/dev/null || true
    # The pre-release evidence, from the app's own frames: a frame that already shows the panel gone while
    # the front is still NOT the tile's app. `write()` captures the front at the moment of the frame, so a
    # dismissal that only happened on the release would carry the app's pid and not count.
    pre_num="$(python3 - "$samples" "$tile_app_pid" "$dismissals_before" <<'PY'
import sys
app = int(sys.argv[2])
before = int(sys.argv[3])
for line in open(sys.argv[1]):
    parts = line.split()
    if len(parts) != 4:
        continue
    panel, front, _seq, dismissals = (int(v) for v in parts)
    # The frame has to be THIS press's (its dismissal count already moved), already show the panel gone,
    # and still name another app as front: the app's own write captures the front at that instant.
    if dismissals == before + 1 and panel == 0 and front != app:
        print(1)
        raise SystemExit(0)
print(0)
PY
)"
    rm -f "$samples"
    if ! wait_state "((s.get('dock_preview') or {}).get('press_dismissals') or 0) >= $dismissals_before + 1 and not (s.get('dock_preview') or {}).get('panel')" 3; then
        problem "the press did not dismiss the preview (dismissals before: $dismissals_before)"
    else
        event_name="$(state_value "s.get('event')")"
        [ "$event_name" = "dock_preview_hide" ] || echo "  note: the state's current frame is '$event_name' (the hide frame may have been overwritten)"
        echo "  the press dismissed the panel; press_dismissals ${dismissals_before} -> $((dismissals_before + 1))"
    fi
    if [ "$pre_num" != "1" ]; then
        problem "no frame before the activation showed the panel gone, so the dismissal cannot be placed on the press"
    else
        echo "  a frame already showed the panel gone while the previous app was still front (pre-release)"
    fi
    front_after="$(omt_front_pid 2>/dev/null || true)"
    if [ "$front_after" != "$tile_app_pid" ]; then
        problem "the Dock click did not bring $press_bundle (pid $tile_app_pid) forward; the front is ${front_after:-unknown}"
    else
        front_moved_by_scenario=1
        echo "  the Dock click activated the tile's app (pid $tile_app_pid)"
    fi
    opens_after="$(state_value "((s.get('dock_preview') or {}).get('preview_opens') or 0)")"
    if [ "$opens_after" != "$opens_before" ]; then
        problem "the preview opened again across the click ($opens_before -> $opens_after) while the pointer rested on the pressed tile"
    fi
    after="$(state_value "((s.get('dock_preview') or {}).get('press_dismissals') or 0)")"
    if [ "$after" != "$((dismissals_before + 1))" ]; then
        problem "the press count changed again while the pointer stayed on the tile ($dismissals_before -> $after)"
    fi
fi

# The observations above belong to one instance; prove it is still that instance before reporting.
if [ -n "$instance_pid" ] && [ -n "$instance_started" ]; then
    identity_rc=0
    ./scripts/dev-restart.sh --identity-expects="$instance_pid,$instance_started" >/dev/null 2>&1 || identity_rc=$?
    if [ "$identity_rc" -ne 0 ]; then
        keep_instance=1
        problem "the instance was replaced during the run (identity exit $identity_rc); the observation is void"
    fi
else
    problem "the instance's started time was unreadable, so the observation cannot be bound to it"
fi

if [ "$fail" -ne 0 ]; then
    echo "dock-preview: FAILED"
    exit 1
fi
echo "dock-preview: PASSED"
exit 0
