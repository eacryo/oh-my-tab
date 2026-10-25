#!/bin/bash
# A2: the picker's drag handle moves the WHOLE floating panel, the moved position lasts exactly one
# showing, and the header's settings button lands on the clipboard page of the settings window.
#
# Why a real pointer gesture: the panel's position is a window-server fact, so no view-tree or state
# read can prove a drag worked -- the assertion has to be that a press-move-release on the handle
# changed where the window is. `--e2e-state` reports the panel rect, the padded window rect and the
# handle's own band, so the gesture starts on the real band (not on a rect this script derives).
#
# Why no hotkey: `--show-clipboard` opens the panel, and the clipboard panel is nonactivating, so
# neither the launch nor the gesture steals focus. A scenario that pressed the global shortcut would
# have to be authorised separately and could not run unattended.
set -euo pipefail

repo_dir="$(cd "$(dirname "$0")/../.." && pwd)"
source "$repo_dir/scripts/lib/e2e-restore.sh"

state="$(mktemp -t panel-drag-state).json"
config_file="$HOME/.config/oh-my-tab/config.toml"
focus_before=""
focus_recorded=0
cursor_x=""
cursor_y=""
to_x=""
to_y=""
build_version=""
fail=0

# The app is relaunched clean at the end -- not merely with the switches dropped. A `--show-clipboard`
# instance left running would keep a panel on the user's screen with a development switch attached, and
# a cleanup that fails is a failure, not a note: the running process is checked for the switches
# afterwards so this scenario can never report PASS while leaving a development instance behind.
restore() {
    local status=$?
    cd "$repo_dir" || {
        echo "panel-drag: cannot return to $repo_dir to restore the app"
        exit 1
    }
    # The pointer goes back first: a synthetic drag leaves the real pointer where it ended, and that is a
    # disturbance the run itself caused. Conditional like the front restore below: only while it is still
    # where the drag left it.
    if [ -n "${cursor_x:-}" ] && [ -n "${cursor_y:-}" ]; then
        # Captured with `|| rc=$?`: under `set -e` a bare call would end the trap at its first
        # non-zero return -- the "user took over" code 2 and the failure code 1 alike -- and skip every
        # restore after it.
        local cursor_rc=0
        omt_restore_cursor "${cursor_x}" "${cursor_y}" "${to_x:-}" "${to_y:-}" 12 || cursor_rc=$?
        case "$cursor_rc" in
            0|2) ;;
            *) [ "$status" -ne 0 ] || status=1 ;;
        esac
    fi
    # The clean relaunch runs BEFORE the endpoint is judged, on purpose: the user sees the machine as the
    # scenario leaves it, cleanup included, so a teardown that fronts the app is part of what this asserts.
    if ! scripts/dev-restart.sh --no-onboarding >/dev/null 2>&1; then
        echo "panel-drag: the clean relaunch FAILED; the app may still be a --show-clipboard instance" >&2
        [ "$status" -ne 0 ] || status=1
    fi
    local pid
    for pid in $(pgrep -x oh-my-tab || true); do
        if ps -o command= -p "$pid" | tr ' ' '\n' | grep -qE '^--(show-clipboard|e2e-state)'; then
            echo "panel-drag: pid $pid is still running with a development switch after the restore" >&2
            [ "$status" -ne 0 ] || status=1
        fi
    done
    # The final endpoint, after every cleanup step: the frontmost application must be the one that was in
    # front before the first launch. Nothing here may exit early, or a failed cleanup would skip the check.
    if [ -n "$focus_before" ]; then
        local app_pid
        app_pid="$(pgrep -x oh-my-tab 2>/dev/null | head -1 || true)"
        if ! omt_front_unchanged "$focus_before" "${app_pid:-}"; then
            echo "panel-drag: the scenario changed the frontmost application" >&2
            [ "$status" -ne 0 ] || status=1
        fi
    elif [ "$focus_recorded" = "1" ]; then
        # Reached the recording point and got no answer: the claim cannot be checked, which is not a pass.
        echo "panel-drag: the frontmost application was never recorded; the no-focus claim is unverified" >&2
        [ "$status" -ne 0 ] || status=1
    fi
    rm -f "$state"
    exit "$status"
}
trap restore EXIT
trap 'exit 130' INT TERM

cd "$repo_dir"

command -v cua-driver >/dev/null 2>&1 || {
    echo "panel-drag: cua-driver CLI not found in PATH"
    exit 1
}
cua-driver status >/dev/null 2>&1 || {
    echo "panel-drag: cua-driver daemon is not running"
    exit 1
}
command -v swift >/dev/null 2>&1 || {
    echo "panel-drag: the Swift toolchain is required for the WindowServer window probe"
    exit 1
}

problem() {
    echo "panel-drag: $1"
    fail=1
}

clear_state() {
    python3 - "$state" <<'PY'
import sys
open(sys.argv[1], "w").write("{}")
PY
}

launch() {  # <label>
    local label="$1"
    local log
    log="$(mktemp -t panel-drag-launch)"
    sleep 1
    clear_state
    if ! scripts/dev-restart.sh --no-onboarding --show-clipboard --e2e-state="$state" >"$log" 2>&1; then
        echo "panel-drag: dev-restart.sh failed for the $label launch:"
        tail -n 20 "$log"
        exit 1
    fi
    [ -n "$build_version" ] || build_version="$(sed -n 's/^build-version: //p' "$log" | tail -1)"
    rm -f "$log"
}

# The state document is written on the app's own schedule, so every read waits for the frame that
# carries the fact under test instead of trusting a fixed sleep alone.
wait_for_state() {  # <python expression over `s`>
    local expression="$1"
    local deadline=$((SECONDS + 25))
    while [ "$SECONDS" -lt "$deadline" ]; do
        if python3 - "$state" "$expression" <<'PY' 2>/dev/null
import json, sys
try:
    s = json.load(open(sys.argv[1]))
except Exception:
    raise SystemExit(1)
raise SystemExit(0 if eval(sys.argv[2], {"s": s}) else 1)
PY
        then
            return 0
        fi
        sleep 0.5
    done
    return 1
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

# The gesture is addressed in DESKTOP screenshot pixels (`scope=desktop`), not against a window id: the
# picker is a floating-level NSPanel, which `list_windows` (layer 0) and the accessibility window list
# both omit. There is no window id to address, and a window-scoped gesture would silently go to whatever
# other window the app owns -- which is exactly what a first attempt here did. Desktop pixels are the
# display's points times its scale factor, and the state reports the panel in points, so the conversion
# is one multiplication.

# The panel's window rect straight from the WindowServer, independent of anything the app reports: the
# panel is a floating-level NSPanel, which `list_windows` and the accessibility window list both omit, so
# CoreGraphics is the only outside source for "where is this window really". The probe is matched by the
# window's SIZE (the state's padded window rect), because its layer and title are not stable identifiers.
window_server_rect() {  # <w_pt> <h_pt> -> "x y w h" or nothing
    local want_w="$1" want_h="$2"
    local pid
    pid="$(pgrep -x oh-my-tab | head -1)"
    swift - "$pid" "$want_w" "$want_h" <<'SWIFT'
import CoreGraphics
import Foundation

guard CommandLine.arguments.count == 4,
      let wanted = Int(CommandLine.arguments[1]),
      let want_w = Double(CommandLine.arguments[2]),
      let want_h = Double(CommandLine.arguments[3]) else {
    fputs("usage: swift <pid> <w> <h>\n", stderr)
    exit(2)
}
let options: CGWindowListOption = [.optionOnScreenOnly, .excludeDesktopElements]
let list = CGWindowListCopyWindowInfo(options, kCGNullWindowID) as? [[String: Any]] ?? []
for window in list {
    guard let owner = window[kCGWindowOwnerPID as String] as? Int, owner == wanted,
          let bounds = window[kCGWindowBounds as String] as? [String: CGFloat] else { continue }
    let x = Double(bounds["X"] ?? -1), y = Double(bounds["Y"] ?? -1)
    let w = Double(bounds["Width"] ?? -1), h = Double(bounds["Height"] ?? -1)
    if abs(w - want_w) <= 1.0 && abs(h - want_h) <= 1.0 {
        print("\(x) \(y) \(w) \(h)")
        exit(0)
    }
}
exit(1)
SWIFT
}

# The panel, its window and the handle's band, in one read (screen points, top-left origin).
read_picker_rects() {
    python3 - "$state" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))
p = s.get("clipboard_picker") or {}
w = s.get("clipboard_picker_window") or {}
g = p.get("grip") or {}
print(
    p.get("x", 0), p.get("y", 0), p.get("w", 0), p.get("h", 0),
    w.get("x", 0), w.get("y", 0), w.get("w", 0), w.get("h", 0),
    g.get("x", 0), g.get("y", 0), g.get("w", 0), g.get("h", 0),
    "yes" if p.get("settings") else "no",
)
PY
}

# Before anything of this scenario runs: the app a user is looking at. Read from the system, not from
# the app's own snapshot, which only exists once the app is up -- a launch that fronted the app would
# already be outside that window.
focus_recorded=1
focus_before="$(omt_front_pid 2>/dev/null)" || focus_before=""
if [ -z "$focus_before" ]; then
    # Refused BEFORE anything is launched: once the app is up, this scenario can no longer tell whether
    # its own launch fronted something, so a baseline taken afterwards would prove nothing.
    echo "panel-drag: the frontmost application could not be read; refusing to run the no-focus check without a baseline" >&2
    exit 1
fi

echo "panel-drag: launching the picker (--show-clipboard)"
launch baseline
if ! wait_for_state "(s.get('clipboard_picker') or {}).get('visible')"; then
    echo "panel-drag: the picker never reported a visible frame (--show-clipboard produced no panel)"
    exit 1
fi

read -r panel_x panel_y panel_w panel_h win_x win_y win_w win_h grip_x grip_y grip_w grip_h settings_reported \
    < <(read_picker_rects)
scale="$(cua get_screen_size '{}' | python3 -c 'import json,sys; print(json.load(sys.stdin)["scale_factor"])')"
read -r screen_w screen_h < <(cua get_screen_size '{}' | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d["width"], d["height"])')

if python3 -c 'import sys; sys.exit(0 if float(sys.argv[1]) <= 0 or float(sys.argv[3]) <= 0 else 1)' "$panel_w" "$panel_h" "$grip_w" "$grip_h"; then
    echo "panel-drag: the state carries no usable panel/handle rect (panel ${panel_w}x${panel_h}, handle ${grip_w}x${grip_h})"
    exit 1
fi
if [ "$settings_reported" != "yes" ]; then
    problem "the state reports no settings button (clipboard_picker.settings is null)"
fi

baseline_ws="$(window_server_rect "$win_w" "$win_h" || true)"
if [ -z "$baseline_ws" ]; then
    echo "panel-drag: the WindowServer reports no ${win_w}x${win_h} window for the app; the independent probe cannot run"
    exit 1
fi
read -r ws_x ws_y ws_w ws_h <<<"$baseline_ws"
echo "  the WindowServer reports the picker window at (${ws_x},${ws_y}) ${ws_w}x${ws_h}; the app says (${win_x},${win_y}) ${win_w}x${win_h}"
if ! python3 -c 'import sys; sys.exit(0 if max(abs(float(a)-float(b)) for a,b in zip(sys.argv[1:5], sys.argv[5:9])) <= 1.0 else 1)' "$ws_x" "$ws_y" "$ws_w" "$ws_h" "$win_x" "$win_y" "$win_w" "$win_h"; then
    problem "the app and the WindowServer disagree about the picker window's rect before the gesture"
fi

# Toward the middle of the display, so the drag has room whichever mode placed the panel and whichever
# edge it was clamped to.
read -r delta_x delta_y < <(python3 - "$panel_x" "$panel_y" "$panel_w" "$panel_h" "$screen_w" "$screen_h" <<'PY'
import sys
x, y, w, h, sw, sh = (float(v) for v in sys.argv[1:7])
step = 110.0
print(int(step if x + w + step <= sw - 20 else -step), int(step if y + h + step <= sh - 20 else -step))
PY
)
# The gesture's start point in desktop pixels: the middle of the handle's band.
read -r from_x from_y < <(python3 - "$panel_x" "$panel_y" "$grip_x" "$grip_y" "$grip_w" "$grip_h" "$scale" <<'PY'
import sys
px, py, gx, gy, gw, gh, scale = (float(v) for v in sys.argv[1:8])
# A quarter along the band, not its center: the whole top bar is the handle, and a gesture that only
# worked on the centered dots would pass a center-anchored start.
print(round((px + gx + gw * 0.25) * scale, 1), round((py + gy + gh / 2.0) * scale, 1))
PY
)
to_x="$(python3 -c 'import sys; print(round(float(sys.argv[1]) + float(sys.argv[2]) * float(sys.argv[3]), 1))' "$from_x" "$delta_x" "$scale")"
to_y="$(python3 -c 'import sys; print(round(float(sys.argv[1]) + float(sys.argv[2]) * float(sys.argv[3]), 1))' "$from_y" "$delta_y" "$scale")"

# Where the pointer was before the gesture: a synthetic drag leaves it where it ended, and in the
# "follow the cursor" mode that alone would move the next summon. Putting it back keeps the relaunch
# comparison below about persistence and nothing else.
cursor_px="$(omt_cursor_px 2>/dev/null)" || cursor_px=""
read -r cursor_x cursor_y <<<"$cursor_px"
if [ -z "${cursor_x:-}" ] || [ -z "${cursor_y:-}" ]; then
    # The drag moves the real pointer; without a baseline the restore cannot honour the user's position,
    # so the run stops before the gesture rather than dragging and then guessing.
    echo "panel-drag: the pointer position could not be read; refusing to drag without a baseline" >&2
    exit 1
fi

echo "  panel (${panel_x},${panel_y}) ${panel_w}x${panel_h}; handle ${grip_w}x${grip_h} at (${grip_x},${grip_y})"
echo "  gesture in desktop pixels: from (${from_x},${from_y}) by (${delta_x},${delta_y})pt"
# The gesture is paced like a hand, not snapped: 1s across 110pt with one move event per ~33ms.
# The panel measures its travel from each EVENT's own position (see `drag_event_screen_point`), so
# a gesture delivered faster than the app drains its queue still lands where it was aimed; the
# pacing here only keeps the path a realistic one.
cua drag "{\"scope\": \"desktop\", \"from_x\": $from_x, \"from_y\": $from_y, \"to_x\": $to_x, \"to_y\": $to_y, \"duration_ms\": 1000, \"steps\": 30, \"delivery_mode\": \"background\"}" >/dev/null

# The WindowServer is read through a poll, not once: the gesture's release applies a cursor correction after
# the last drag step, so a single read can catch the window one correction short (observed: 100pt of a
# 110pt move, agreeing with the app moments later). Polling for the rect the gesture asked for keeps the
# check independent -- the expected value comes from the pre-gesture WindowServer rect plus the gesture's
# delta, never from the app's own report -- and stops a settle race from reading as a failed drag.
wait_for_window_server_rect() {  # <x> <y> <w> <h> -> prints the rect it settled on
    local want_x="$1" want_y="$2" want_w="$3" want_h="$4"
    local deadline=$((SECONDS + 5))
    local found=""
    while [ "$SECONDS" -lt "$deadline" ]; do
        found="$(window_server_rect "$want_w" "$want_h" || true)"
        # $found is "x y w h"; the wanted origin follows it, so x/y are argv 1/2 and the wanted values
        # 5/6. (Comparing x with w here made the poll wait out its whole deadline every time.)
        if [ -n "$found" ] && python3 -c 'import sys; sys.exit(0 if abs(float(sys.argv[1]) - float(sys.argv[5])) <= 1.0 and abs(float(sys.argv[2]) - float(sys.argv[6])) <= 1.0 else 1)' $found "$want_x" "$want_y"; then
            printf '%s\n' "$found"
            return 0
        fi
        sleep 0.2
    done
    printf '%s\n' "$found"
    return 1
}

# The window moves at once but the state document is written on the app's own schedule, so the assertion
# polls for the position the gesture asked for. Reading the file once right after the drag called a stale
# document a failure -- the drag had worked, the report had not caught up.
want_x="$(python3 -c 'import sys; print(round(float(sys.argv[1]) + float(sys.argv[2]), 1))' "$panel_x" "$delta_x")"
want_y="$(python3 -c 'import sys; print(round(float(sys.argv[1]) + float(sys.argv[2]), 1))' "$panel_y" "$delta_y")"
if ! wait_for_state "(s.get('clipboard_picker') or {}).get('visible') and abs(s['clipboard_picker']['x'] - ($want_x)) <= 3 and abs(s['clipboard_picker']['y'] - ($want_y)) <= 3"; then
    read -r found_x found_y _rest < <(read_picker_rects)
    problem "the panel did not follow the handle: expected (${want_x},${want_y}), the app reports (${found_x},${found_y})"
else
    echo "  after the drag: panel (${want_x},${want_y}) (state settled)"
fi

# The independent check: the window really is where the app now reports it, and it really moved by the
# gesture's delta. The app's own document is written after the move, so this compares two sources that
# were produced by different machinery.
read -r app_win_x app_win_y app_win_w app_win_h < <(read_picker_rects | python3 -c 'import sys; f=sys.stdin.read().split(); print(" ".join(f[4:8]))')
# The rect the gesture asks the WindowServer for: where the window was before the gesture, plus the delta.
want_ws_x="$(python3 -c 'import sys; print(round(float(sys.argv[1]) + float(sys.argv[2]), 1))' "$ws_x" "$delta_x")"
want_ws_y="$(python3 -c 'import sys; print(round(float(sys.argv[1]) + float(sys.argv[2]), 1))' "$ws_y" "$delta_y")"
moved_ws="$(wait_for_window_server_rect "$want_ws_x" "$want_ws_y" "$app_win_w" "$app_win_h" || true)"
if [ -z "$moved_ws" ]; then
    problem "the WindowServer reports no window at the size the app reports after the drag"
else
    read -r mws_x mws_y mws_w mws_h <<<"$moved_ws"
    echo "  after the drag the WindowServer reports the picker window at (${mws_x},${mws_y}); the app says (${app_win_x},${app_win_y})"
    if ! python3 -c 'import sys; sys.exit(0 if max(abs(float(a)-float(b)) for a,b in zip(sys.argv[1:5], sys.argv[5:9])) <= 1.0 else 1)' "$mws_x" "$mws_y" "$mws_w" "$mws_h" "$app_win_x" "$app_win_y" "$app_win_w" "$app_win_h"; then
        problem "the app and the WindowServer disagree about the picker window's rect after the drag"
    fi
    if ! python3 -c 'import sys; sys.exit(0 if abs(float(sys.argv[1]) - (float(sys.argv[3]) + float(sys.argv[5]))) <= 1.0 and abs(float(sys.argv[2]) - (float(sys.argv[4]) + float(sys.argv[6]))) <= 1.0 else 1)' "$mws_x" "$mws_y" "$ws_x" "$ws_y" "$delta_x" "$delta_y"; then
        problem "the WindowServer window did not move by the gesture's delta (from ${ws_x},${ws_y} to ${mws_x},${mws_y}, delta ${delta_x},${delta_y})"
    fi
fi

# Not persisted: no position key may appear in the configuration.
if [ -f "$config_file" ] && grep -qE '^[[:space:]]*panel_position[[:space:]]*=' "$config_file"; then
    problem "the drag wrote a panel position into $config_file; the moved position must last one showing only"
fi

# Put the pointer back before the relaunch: a desktop-scope gesture moves the real pointer, and in the
# "follow the cursor" mode where it is left would decide the next summon's position -- the relaunch
# comparison below is about persistence and nothing else. The trap repeats this on any exit path.
if [ -n "${cursor_x:-}" ] && [ -n "${cursor_y:-}" ]; then
    # A "the user moved it" answer (2) is not an error here, and under `set -e` it would abort the run.
    omt_restore_cursor "${cursor_x}" "${cursor_y}" "${to_x:-}" "${to_y:-}" 12 || true
fi

# A fresh launch is a fresh showing: the configured mode decides again, so the panel must be back at the
# baseline. This is the assertion that "one showing" is real, and it is why the baseline is compared
# after a relaunch rather than after re-showing in-process (re-showing needs the global hotkey).
echo "panel-drag: relaunching to check that the moved position did not survive"
launch relaunch
if ! wait_for_state "(s.get('clipboard_picker') or {}).get('visible')"; then
    echo "panel-drag: the picker never reappeared on the relaunch"
    exit 1
fi
read -r back_x back_y back_h baseline_h back_drift < <(python3 - "$state" "$panel_x" "$panel_y" "$panel_h" <<'PY'
import json, sys
after = (json.load(open(sys.argv[1])).get("clipboard_picker") or {})
drift = max(abs(float(after.get("x", 0)) - float(sys.argv[2])), abs(float(after.get("y", 0)) - float(sys.argv[3])))
print("{:.1f} {:.1f} {:.1f} {:.1f} {:.1f}".format(
    float(after.get("x", 0)), float(after.get("y", 0)), float(after.get("h", 0)), float(sys.argv[4]), drift))
PY
)
echo "  relaunched panel (${back_x},${back_y}) h=${back_h} vs baseline (${panel_x},${panel_y}) h=${baseline_h}, drift ${back_drift}pt"
if ! python3 -c 'import sys; sys.exit(0 if abs(float(sys.argv[1]) - float(sys.argv[2])) <= 1.0 else 1)' "$back_h" "$baseline_h"; then
    problem "the panel's height changed between launches (${back_h} vs ${baseline_h}), so its origin is not comparable; the history changed during the run"
elif ! python3 -c 'import sys; sys.exit(0 if float(sys.argv[1]) <= 1.0 else 1)' "$back_drift"; then
    problem "the relaunched panel is not where the configured position puts it: (${back_x},${back_y}) vs (${panel_x},${panel_y}); the dragged position was remembered"
fi

# The header's settings button is asserted at A1: the clipboard smoke runner finds the live button by
# the action it targets, runs that action, and checks the panel closes while the settings sidebar lands
# on the clipboard page. It is deliberately NOT clicked here -- opening the settings window fronts the
# app, which would put this scenario in the focus-stealing class and take the drag check out of the
# default suite with it.

# And the independent half of "not persisted": the WindowServer's own rect after a relaunch is the
# baseline again, so this is not just the app declining to report the drag.
back_ws="$(window_server_rect "$ws_w" "$ws_h" || true)"
if [ -z "$back_ws" ]; then
    problem "the WindowServer reports no window at the baseline size after the relaunch"
else
    read -r bws_x bws_y bws_w bws_h <<<"$back_ws"
    echo "  the WindowServer reports the relaunched picker window at (${bws_x},${bws_y})"
    if ! python3 -c 'import sys; sys.exit(0 if max(abs(float(a)-float(b)) for a,b in zip(sys.argv[1:5], sys.argv[5:9])) <= 1.0 else 1)' "$bws_x" "$bws_y" "$bws_w" "$bws_h" "$ws_x" "$ws_y" "$ws_w" "$ws_h"; then
        problem "the relaunched panel is not back at the rect the WindowServer reported before the drag"
    fi
fi

echo
if [ "$fail" = 0 ]; then
    echo "panel-drag: PASS (build ${build_version:-unknown})"
else
    echo "panel-drag: FAIL (build ${build_version:-unknown})"
fi
exit "$fail"