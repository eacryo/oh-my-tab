#!/bin/bash
# A2: the switcher footer's settings gear really opens the App Switcher settings page on a real
# click, and that click does NOT commit the highlighted card.
#
# Why a real pointer gesture: the defect this scenario exists for was a gear that looked right and
# did nothing. Its mouseDown: override replaced NSButton's tracking loop without calling super, so
# no action was ever dispatched -- every geometry assertion passed and performClick: (which sends
# the action directly) passed too. Only a gesture through the window server reaches the broken
# path, so the assertion has to be a real click at the app's own reported gear rect.
#
# Why --show-overlay: the switcher normally appears only from the global hotkey, which steals
# focus and cannot run unattended. The development switch summons the same panel and leaves it
# standing, so the gesture works on the real view tree without touching the focused app.
#
# 但**这一下点击本身会抢焦点**: 齿轮走产品自己的入口,会激活 app 并把设置窗口置前
# (src/settings/window.rs 的 show_settings_inner)。这是被测行为,所以本场景按抢焦点场景声明。
# The CLICK is what fronts the app: the gear goes through the product's own entry, which activates the
# app and orders the Settings window front. That is the behaviour under test.
# E2E_STEALS_FOCUS=1
# E2E_REPLACES_INSTANCE=1
#
# Exit code: 0 = every assertion passed; non-zero = failed, with the reason printed.
set -euo pipefail

repo_dir="$(cd "$(dirname "$0")/../.." && pwd)"
source "$repo_dir/scripts/lib/e2e-restore.sh"

state="$(mktemp -t switcher-settings-state).json"
build_version=""
fail=0
launched=0
front_app_before=""

restore() {
    local status=$?
    # No exit before the last restore step: a failed relaunch must not skip the front restore.
    cd "$repo_dir" || { echo "switcher-settings: cannot return to $repo_dir"; exit 1; }
    if [ "$launched" = 1 ]; then
        if ! scripts/dev-restart.sh --no-onboarding >/dev/null 2>&1; then
            echo "switcher-settings: the clean relaunch FAILED; the app may still be a --show-overlay instance" >&2
            [ "$status" -ne 0 ] || status=1
        fi
    fi
    local pid
    for pid in $(pgrep -x oh-my-tab || true); do
        if ps -o command= -p "$pid" | tr ' ' '\n' | grep -qE '^--(show-overlay|e2e-state)'; then
            echo "switcher-settings: pid $pid is still running with a development switch after the restore" >&2
            [ "$status" -ne 0 ] || status=1
        fi
    done
    # The click fronts the app; hand the front back to whoever had it. Declared focus-stealing, so this
    # is part of the agreement rather than a courtesy.
    # Conditional, and reported: hand the front back only while it still belongs to the app this
    # scenario drove. A person who switched apps during the run has made a newer decision.
    if [ -n "${front_app_before:-}" ]; then
        local test_pid
        test_pid="$(pgrep -x oh-my-tab 2>/dev/null | head -1 || true)"
        # Captured with `|| front_rc=$?`: under `set -e` a bare call would end the trap on code 2
        # ("the user moved on") before the rest of the cleanup ran.
        local front_rc=0
        omt_hand_front_back "$front_app_before" "${test_pid:-}" || front_rc=$?
        case "$front_rc" in
            0|2) ;;
            *) [ "$status" -ne 0 ] || status=1 ;;
        esac
    fi
    rm -f "$state"
    exit "$status"
}
trap restore EXIT
trap 'exit 130' INT TERM

cd "$repo_dir"

command -v cua-driver >/dev/null 2>&1 || { echo "switcher-settings: cua-driver CLI not found in PATH"; exit 1; }
cua-driver status >/dev/null 2>&1 || { echo "switcher-settings: cua-driver daemon is not running"; exit 1; }

problem() { echo "switcher-settings: $1"; fail=1; }

cua() {  # <tool> <json args>
    python3 - "$@" <<'PY'
import subprocess, sys
completed = subprocess.run(["cua-driver", "call", sys.argv[1], sys.argv[2]], capture_output=True, text=True)
if completed.returncode != 0:
    raise SystemExit(f"cua-driver {sys.argv[1]} failed: {completed.stderr.strip()[:400]}")
print(completed.stdout)
PY
}

launch() {  # <label>
    local label="$1"
    local log
    log="$(mktemp -t switcher-settings-launch)"
    sleep 1
    echo '{}' > "$state"
    if ! scripts/dev-restart.sh --no-onboarding --show-overlay --e2e-state="$state" >"$log" 2>&1; then
        echo "switcher-settings: dev-restart.sh failed for the $label launch:"
        tail -n 20 "$log"
        exit 1
    fi
    launched=1
    [ -n "$build_version" ] || build_version="$(sed -n 's/^build-version: //p' "$log" | tail -1)"
    rm -f "$log"
}

wait_for_state() {  # <python expression over s>
    local expression="$1"
    local deadline=$((SECONDS + 25))
    while [ "$SECONDS" -lt "$deadline" ]; do
        if python3 - "$state" "$expression" "$repo_dir/scripts/e2e/lib" <<'PY' 2>/dev/null
import json, sys
# A duplicated member would make the gate read a different value than the app wrote,
# so the state is parsed by the duplicate-rejecting loader (see lib/state_json.py).
sys.path.insert(0, sys.argv[3])
from state_json import load_state
try:
    with open(sys.argv[1]) as handle:
        s = load_state(handle)
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

front_app_before="$(omt_front_pid 2>/dev/null)" || front_app_before=""
if [ -z "$front_app_before" ]; then
    echo "switcher-settings: the frontmost application could not be read; refusing to run without a baseline" >&2
    exit 1
fi

echo "switcher-settings: launching the overlay (--show-overlay)"
launch baseline
if ! wait_for_state "(s.get('switcher_panel') or {}).get('visible')"; then
    echo "switcher-settings: the overlay never reported a visible panel (--show-overlay produced no panel)"
    exit 1
fi

read -r panel_x panel_y panel_w panel_h gear_x gear_y gear_w gear_h clears < <(python3 - "$state" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))
p = s.get("switcher_panel") or {}
g = s.get("switcher_footer_settings") or {}
print(p.get("x", 0), p.get("y", 0), p.get("w", 0), p.get("h", 0),
      g.get("x", 0), g.get("y", 0), g.get("w", 0), g.get("h", 0),
      "yes" if g.get("clears_text") else "no")
PY
)

if python3 -c 'import sys; sys.exit(0 if float(sys.argv[1]) <= 0 or float(sys.argv[2]) <= 0 else 1)' "$gear_w" "$gear_h"; then
    echo "switcher-settings: the state carries no usable gear rect (panel ${panel_w}x${panel_h}, gear ${gear_w}x${gear_h})"
    exit 1
fi
if [ "$clears" != "yes" ]; then
    problem "the footer gear overlaps the status text (switcher_footer_settings.clears_text is false)"
fi

scale="$(cua get_screen_size '{}' | python3 -c 'import json,sys; print(json.load(sys.stdin)["scale_factor"])')"

# The click point in desktop pixels: the centre of the gear's real rect, which the app reports in
# screen points with a top-left origin -- the same space cua's desktop scope uses, so the only
# conversion is the display scale.
read -r click_x click_y < <(python3 - "$gear_x" "$gear_y" "$gear_w" "$gear_h" "$scale" <<'PY'
import sys
gx, gy, gw, gh, scale = (float(v) for v in sys.argv[1:6])
print(round((gx + gw / 2.0) * scale, 1), round((gy + gh / 2.0) * scale, 1))
PY
)

commits_before="$(python3 - "$state" <<'PY'
import json, sys
print((json.load(open(sys.argv[1])).get("last_commit") or {}).get("count", 0))
PY
)"
echo "  panel (${panel_x},${panel_y}) ${panel_w}x${panel_h}; gear ${gear_w}x${gear_h} at (${gear_x},${gear_y})"
echo "  clicking the gear in desktop pixels at (${click_x},${click_y}); commits so far ${commits_before}"

cua click "{\"target\": {\"kind\": \"desktop\", \"display_id\": \"primary\"}, \"x\": $click_x, \"y\": $click_y, \"delivery_mode\": \"foreground\"}" >/dev/null

# The assertion the broken gear failed: the settings window must actually come up on its page.
if ! wait_for_state "s.get('settings_window_visible') and s.get('selected_sidebar_name') == 'switcher'"; then
    page="$(python3 - "$state" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))
print("visible" if s.get("settings_window_visible") else "not visible", s.get("selected_sidebar_name"))
PY
)"
    problem "clicking the gear did not open the settings window on the App Switcher page (page: ${page})"
else
    presented="$(python3 - "$state" <<'PY'
import json, sys
print(json.load(open(sys.argv[1])).get("settings_presented_page"))
PY
)"
    echo "  settings opened: selected_sidebar_name=switcher, presented_page=${presented}"
    if [ "$presented" != "1" ] && [ "$presented" != "None" ]; then
        problem "the settings window first appeared on page ${presented}, not the App Switcher page"
    fi
fi

# The click must not also commit the highlighted card: the gear is an exit, not a launch of the
# selected window. A stale mouseUp or a missing session retirement would show up here.
commits_after="$(python3 - "$state" <<'PY'
import json, sys
print((json.load(open(sys.argv[1])).get("last_commit") or {}).get("count", 0))
PY
)"
echo "  commits after the click: ${commits_after}"
if [ "$commits_after" != "$commits_before" ]; then
    problem "the settings click committed a card (commit count ${commits_before} -> ${commits_after})"
fi

echo
if [ "$fail" = 0 ]; then
    echo "switcher-settings: PASS (build ${build_version:-unknown})"
else
    echo "switcher-settings: FAIL (build ${build_version:-unknown})"
fi
exit "$fail"
