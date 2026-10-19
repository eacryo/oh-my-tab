#!/bin/bash
# E2E_STEALS_FOCUS=1
# A2: the clipboard picker's settings button opens the settings window fast, and the measurement can tell
# the warm path from the cold one.
#
# The measurement is the app's own: from the timestamp the click's event carries (so the wait for the main
# thread is inside it, not assumed away) through the window coming up and the panel being taken down. What it
# cannot see is the WindowServer's own compositing after the handler returns, and the scenario says so instead
# of implying the number is pixel-arrival.
#
# Why this scenario exists: opening settings from the panel's gear used to take ~1s (the window was built
# and every page's controls were filled inside the click), and nothing in the suite would have noticed it
# coming back. The app publishes its own wall time for the open path as `settings_open_ms` in the
# `--e2e-state` document, so the bound is asserted against a number the app states, not against a
# screenshot.
#
# Why the counter-example: a bound on a happy path is not a gate. The second launch passes
# `--no-settings-prewarm` (the startup warm-up that makes the first open cheap), and the scenario demands
# that the SAME gesture measure strictly slower there. A broken instrument -- a click that misses, a state
# field the app stopped writing, a warm-up that silently stopped running -- fails that comparison instead
# of passing the bound by accident.
#
# This scenario fronts the app by design (presenting the settings window activates it), so it is declared
# focus-stealing and is skipped unless run-all.sh is given --include-focus.
set -euo pipefail

repo_dir="$(cd "$(dirname "$0")/../.." && pwd)"

state="$(mktemp -t settings-open-latency-state).json"
config_file="$HOME/.config/oh-my-tab/config.toml"
# The launches happen inside a command substitution, so the version dev-restart.sh prints cannot live in a
# shell variable of this process; it is handed over through a file.
version_file="$(mktemp -t settings-open-latency-version)"
build_version=""
fail=0

# The app is relaunched clean at the end: a `--show-clipboard` instance left running keeps a panel on the
# user's screen with a development switch attached.
restore() {
    local status=$?
    cd "$repo_dir" || {
        echo "settings-open-latency: cannot return to $repo_dir to restore the app"
        exit 1
    }
    if ! scripts/dev-restart.sh --no-onboarding >/dev/null 2>&1; then
        echo "settings-open-latency: the clean relaunch FAILED; the app may still be a development instance"
        exit 1
    fi
    local pid
    for pid in $(pgrep -x oh-my-tab || true); do
        if ps -o command= -p "$pid" | tr ' ' '\n' | grep -qE '^--(show-clipboard|e2e-state|no-settings-prewarm)'; then
            echo "settings-open-latency: pid $pid is still running with a development switch after the restore"
            exit 1
        fi
    done
    rm -f "$state" "$version_file"
    exit "$status"
}
trap restore EXIT
trap 'exit 130' INT TERM

cd "$repo_dir"

command -v cua-driver >/dev/null 2>&1 || {
    echo "settings-open-latency: cua-driver CLI not found in PATH"
    exit 1
}
cua-driver status >/dev/null 2>&1 || {
    echo "settings-open-latency: cua-driver daemon is not running"
    exit 1
}

problem() {
    echo "settings-open-latency: $1"
    fail=1
}

# Nanoseconds, so a rewrite inside the same second is still visible.
config_mtime() {
    python3 - "$config_file" <<'PY'
import os, sys
try:
    print(os.stat(sys.argv[1]).st_mtime_ns)
except FileNotFoundError:
    print("missing")
PY
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

launch() {  # <label> [extra app args...]
    local label="$1"; shift
    local log
    log="$(mktemp -t settings-open-latency-launch)"
    sleep 1
    rm -f "$state" "${state%.json}.tmp"
    # --opt: this is a latency measurement, so the build has to be the representative one.
    if ! scripts/dev-restart.sh --opt --no-onboarding --show-clipboard --e2e-state="$state" -- "$@" >"$log" 2>&1; then
        echo "settings-open-latency: dev-restart.sh failed for the $label launch:"
        tail -n 20 "$log"
        exit 1
    fi
    sed -n 's/^build-version: //p' "$log" | tail -1 >"$version_file"
    rm -f "$log"
}

wait_for_state() {  # <python expression over `s`> [seconds]
    local expression="$1"
    local timeout="${2:-25}"
    local deadline=$((SECONDS + timeout))
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
        sleep 0.2
    done
    return 1
}

# The gesture, and the number it produced, for one launch.
measure_open() {  # <label> <extra app args...> -> prints "<ms> <seq>"
    local label="$1"; shift
    echo "  [$label] launching" >&2
    launch "$label" "$@"
    if ! wait_for_state "(s.get('clipboard_picker') or {}).get('visible')"; then
        echo "settings-open-latency: the picker never reported a visible frame ($label)" >&2
        exit 1
    fi
    if ! wait_for_state "s.get('settings_open_ms', 0) == 0"; then
        echo "settings-open-latency: a fresh launch already reports a settings-open time ($label)" >&2
        exit 1
    fi
    # The warm launch must warm the window before the click, and the cold launch must NOT: waiting for (or
    # for the absence of) that state is what makes the two numbers mean what they say. Without this the
    # scenario silently measured whatever state it found -- which it did: a "warm" run once reported 394ms
    # because the click landed before the warm-up.
    if [ "$label" = "warm" ]; then
        if ! wait_for_state "s.get('settings_window_built')"; then
            echo "settings-open-latency: the startup warm-up never built the settings window ($label); the warm bound would be measuring a cold launch" >&2
            exit 1
        fi
    elif wait_for_state "s.get('settings_window_built')" 5; then
        echo "settings-open-latency: --no-settings-prewarm still built the settings window ($label)" >&2
        exit 1
    fi
    # Read through a command substitution and a here-string, not `< <( ... <<'PY' ...)`:
    # macOS ships bash 3.2, whose process-substitution scanner misfires on a heredoc body with
    # parenthesised expressions ("bad substitution: no closing `)'").
    fields="$(python3 - "$state" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))
p = s.get("clipboard_picker") or {}
w = s.get("clipboard_picker_window") or {}
g = p.get("settings") or {}
print(
    p.get("x", 0), p.get("y", 0), p.get("w", 0), p.get("h", 0),
    w.get("x", 0), w.get("y", 0), w.get("w", 0), w.get("h", 0),
    g.get("x", 0), g.get("y", 0), g.get("w", 0), g.get("h", 0),
    "yes" if p.get("settings") else "no",
    s.get("seq", 0),
)
PY
)"
    read -r panel_x panel_y _panel_w _panel_h _win_x _win_y _win_w _win_h \
        gear_x gear_y gear_w gear_h gear_reported baseline_seq <<<"$fields"
    if [ "$gear_reported" != "yes" ]; then
        echo "settings-open-latency: the state reports no settings button ($label)" >&2
        exit 1
    fi
    if python3 -c 'import sys; sys.exit(0 if float(sys.argv[1]) <= 0 or float(sys.argv[2]) <= 0 else 1)' "$gear_w" "$gear_h"; then
        echo "settings-open-latency: the settings button has no usable rect ($label: ${gear_w}x${gear_h})" >&2
        exit 1
    fi
    local scale click_x click_y
    scale="$(cua get_screen_size '{}' | python3 -c 'import json,sys; print(json.load(sys.stdin)["scale_factor"])')"
    click_xy="$(python3 -c 'import sys; p=sys.argv[1:8]; x,y,gx,gy,gw,gh,s=(float(v) for v in p); print(round((x+gx+gw/2.0)*s,1), round((y+gy+gh/2.0)*s,1))' \
        "$panel_x" "$panel_y" "$gear_x" "$gear_y" "$gear_w" "$gear_h" "$scale")"
    read -r click_x click_y <<<"$click_xy"
    # Opening the window must not write the configuration: the prewarmed window holds AppKit's automatic
    # initial first responder, and resigning a field the user never typed into must not persist anything.
    mtime_before="$(config_mtime)"
    echo "  [$label] clicking the settings button at desktop (${click_x},${click_y})" >&2
    cua click "{\"target\": {\"kind\": \"desktop\", \"display_id\": \"primary\"}, \"x\": $click_x, \"y\": $click_y, \"delivery_mode\": \"foreground\"}" >/dev/null
    if ! wait_for_state "s.get('seq', 0) > $baseline_seq and s.get('settings_click_ms', 0) > 0"; then
        echo "settings-open-latency: no state frame with a settings-click time after the click ($label); the click missed or the open did not happen" >&2
        exit 1
    fi
    mtime_after="$(config_mtime)"
    # Every check runs BEFORE the numbers are printed: a measurement that failed must not put a parseable line
    # on stdout, because the caller reads those numbers and would otherwise report PASS (the producer's exit
    # status does not cross a command substitution silently -- it is checked by the caller now).
    if [ "$mtime_before" != "$mtime_after" ]; then
        echo "settings-open-latency: opening the window rewrote $config_file ($label: $mtime_before -> $mtime_after)" >&2
        echo "settings-open-latency: NOT-OK" >&2
        exit 1
    fi
    python3 - "$state" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))
if not s.get("settings_window_visible"):
    raise SystemExit(f"settings-open-latency: the timing was published while the window is not visible: {s.get('seq')}")
click, window = int(s["settings_click_ms"]), int(s.get("settings_open_ms", 0))
delivery = int(s.get("settings_click_delivery_ms", 0))
if window <= 0 or click < window:
    raise SystemExit(
        "settings-open-latency: the click timing does not cover the window phase "
        f"(click={click}ms, window={window}ms)"
    )
print(f"{click} {window} {delivery} {int(s.get('seq', 0))}")
PY
}

# The warm path's bound, measured from the click the event carries to the panel being down: ~90ms on a warm
# window, ~400ms with the warm-up switched off (before this work the same gesture measured ~940ms, most of it
# device enumeration and filling the mouse page for a window nobody had asked for yet). This is a regression
# bound, not a target: it has room for a slower machine while still failing on the cold path it replaced, and
# the cold launch above is what proves the number moves when the mechanism is removed.
warm_bound_ms=400

# The numbers are read through a command substitution so the producer's exit status survives: with a process
# substitution (`< <(...)`) a failed measurement would still leave this shell to print PASS.
warm_line=""
echo "settings-open-latency: measuring the warm path (startup warm-up enabled)"
if ! warm_line="$(measure_open warm)"; then
    problem "the warm measurement failed (see the reason above)"
fi
read -r warm_ms warm_window_ms warm_delivery_ms warm_seq <<<"${warm_line:-0 0 0 0}"
echo "  warm click: ${warm_ms}ms from the event (delivery ${warm_delivery_ms}ms, window ${warm_window_ms}ms; seq=${warm_seq})"
if [ -n "$warm_line" ] && ! python3 -c 'import sys; sys.exit(0 if float(sys.argv[1]) <= float(sys.argv[2]) else 1)' "$warm_ms" "$warm_bound_ms"; then
    problem "the warm click took ${warm_ms}ms, above the ${warm_bound_ms}ms bound"
fi

cold_line=""
echo "settings-open-latency: measuring the cold path (--no-settings-prewarm) as the counter-example"
if ! cold_line="$(measure_open cold --no-settings-prewarm)"; then
    problem "the cold measurement failed (see the reason above)"
fi
read -r cold_ms cold_window_ms cold_delivery_ms cold_seq <<<"${cold_line:-0 0 0 0}"
echo "  cold click: ${cold_ms}ms from the event (delivery ${cold_delivery_ms}ms, window ${cold_window_ms}ms; seq=${cold_seq})"
if [ -n "$cold_line" ] && [ -n "$warm_line" ] && ! python3 -c 'import sys; sys.exit(0 if float(sys.argv[1]) > float(sys.argv[2]) else 1)' "$cold_ms" "$warm_ms"; then
    problem "the cold click (${cold_ms}ms) is not slower than the warm one (${warm_ms}ms): this gesture or this state field does not measure what the bound assumes"
fi

build_version="$(cat "$version_file" 2>/dev/null || true)"
echo
if [ "$fail" = "0" ]; then
    echo "e2e settings-open-latency: PASS: warm click ${warm_ms}ms <= ${warm_bound_ms}ms (delivery ${warm_delivery_ms}ms, window ${warm_window_ms}ms), cold click ${cold_ms}ms > warm (build ${build_version})"
else
    echo "e2e settings-open-latency: FAIL: warm click ${warm_ms}ms, cold click ${cold_ms}ms (build ${build_version})"
fi
exit "$fail"
