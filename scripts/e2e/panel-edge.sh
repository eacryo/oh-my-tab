#!/bin/bash
# A2: the floating panel's edge decorations, measured on rendered pixels -- the 1pt outline, the elevation
# shadow, and the backdrop-texture retention that the shadow carrier must not disturb.
#
# Why launches come in pairs: every check compares two launches that differ in exactly one development
# switch, so the pixel difference can be that switch's and nothing else. Neither fact is a region statistic:
# a 1pt stroke is thinner than one region sample, and the shadow lies *outside* the panel rect, in the
# window padding the panels carry for it (`--e2e-state` reports both rects).
#
#   1. outline        --panel-backdrop=white                      vs + --panel-outline=off
#   2. shadow         --panel-backdrop=white, outline on          vs + --panel-shadow=off
#   3. material blur  --panel-backdrop=texture (carrier present)  vs + --panel-shadow=off (carrier gone)
#
# The capture is always the *panel* rect inflated by the padding the app puts around it, asserted against
# `clipboard_picker` vs `clipboard_picker_window` rather than assumed: with `--panel-shadow=off` the window
# carries no padding at all, so capturing the window rect would lose the margin every shadow cut is taken
# in. `--panel-backdrop` pins a known surface behind the panel, because a translucent material's tone
# follows what is behind it. No global hotkey is injected, so this scenario does not steal focus.
set -euo pipefail

# The repository is resolved to an absolute path ONCE, before anything changes directory: a trap that
# re-derives it from a relative `$0` after the script has cd'd resolves to the wrong place (running
# `./panel-edge.sh` from `scripts/e2e` was the failing case).
repo_dir="$(cd "$(dirname "$0")/../.." && pwd)"

# The app is relaunched clean at the end -- not merely with the switches dropped. Every launch here uses
# `--panel-backdrop`, and two of them `--panel-shadow=off`/`--panel-outline=off`, i.e. the counter-example
# frames: leaving the last one running hands the user a panel with a solid backdrop pinned behind it and no
# shadow. A cleanup that fails is a failure, not a note: the run reports it and exits non-zero, and the
# running process is checked for the switches afterwards so a scenario can never report PASS while leaving a
# development instance behind.
restore() {
    local status=$?
    cd "$repo_dir" || {
        echo "panel-edge: cannot return to $repo_dir to restore the app"
        exit 1
    }
    if ! scripts/dev-restart.sh --no-onboarding >/dev/null 2>&1; then
        echo "panel-edge: the clean relaunch FAILED; the app may still carry --panel-backdrop/--panel-outline/--panel-shadow"
        exit 1
    fi
    # Every process with that name, not just the first: the release and development builds are both
    # `oh-my-tab`, so finding a clean one proves nothing about a development instance still carrying a
    # switch -- the same counter-example that shaped `run-all.sh`'s check.
    local pid
    for pid in $(pgrep -x oh-my-tab || true); do
        if ps -o command= -p "$pid" | tr ' ' '\n' | grep -qE '^--(panel-backdrop|panel-outline|panel-shadow)'; then
            echo "panel-edge: pid $pid is still running with a development switch after the restore"
            exit 1
        fi
    done
    exit "$status"
}
trap restore EXIT
trap 'exit 130' INT TERM

cd "$repo_dir"

# The launch's own settle time before the first capture: a translucent material can still be converging at
# the first paint, and a half-settled surface reads as a different material rather than a different layout.
SETTLE_S="${SETTLE_S:-6}"
# After that, how long to wait for a state frame that carries a visible picker before failing.
STATE_WAIT_S="${STATE_WAIT_S:-15}"
# The padding the panels carry for the elevation shadow: radius 32 + tail allowance 32 on top/left/right,
# plus the shadow's 12pt downward offset on the bottom. Asserted against the state on every launch that
# should have a carrier, and used to build the capture rect on every launch.
PAD_L=64
PAD_T=64
PAD_R=64
PAD_B=76
# Tone units (0..255) below which the instrument cannot tell two tones apart: its smallest stated margin is
# 2 units (LOCAL_MARGIN / MIN_SURFACE_DELTA / DECAY_TOL). "edge equals interior" and "the interior is
# unchanged between launches" are asserted against this, so capture noise cannot pass as a decoration.
NOISE_TONE="${NOISE_TONE:-2}"
# The instrument's own SHADOW_MIN_DEPTH: a shadow shallower than 3 tone units next to the panel cannot be
# told from the backdrop, so a "darker than the control" claim needs at least this much.
SHADOW_MIN_DEPTH=3
# The instrument's own RING_MIN: a sustained band deviation below 8 tone units is "the interior's own
# texture", so an outline band that reads less than this was not drawn where the cut samples.
OUTLINE_MIN_TONE=8
# How far the two launches' hf ratios may disagree and still support "the same material, blurred the same
# way". The instrument's class boundary (ENERGY_ALIVE) is 0.1 on this same energy ratio, a translucent
# material does not re-render identically across launches (documented for the outline/shadow dev switches),
# and a carrier that broke the blur would move hf by far more than half a class.
HF_TOL=0.05
# `--hf-retention`'s inside region and its control. The control is the raw pinned ladder in the window's
# top-left margin. The inside region must lie on **bare material**: ink is high-frequency structure the panel
# itself draws, so it reads as hf the material never had to pass and the verdict stops being about the blur.
# `20,62,450,30` is the filter-chip row -- measured hf_inside 528.9 against 0.1 on the bare band below it,
# which is exactly that mistake, so it is not used.
INSIDE="20,352,450,20"
CONTROL="-48,-48,40,40"
# A material that has stopped blurring passes the ladder's fine bars: its hf ratio approaches the control's,
# i.e. 1.0. Anything under this still destroys the detail a blur is supposed to destroy. It is deliberately
# looser than the instrument's own ENERGY_ALIVE floor (0.1): the claim here is "the blur is alive", and the
# instrument's stricter class word cannot be asserted on this ladder (see the note above `assert_blur`).
HF_ALIVE_MAX=0.2
# A translucent panel's interior tone follows the backdrop; an opaque fill's does not. The two pinned
# backdrops are the extremes, so the difference is large (measured on the picker's bare top band: 1.000 over
# white against 0.723 over black, i.e. ~71 tone units), and this floor is set well below that so it states
# "the material is not opaque" rather than re-measuring the tint.
TRANSLUCENT_MIN_TONE=24

state="$(mktemp -t panel-edge-state).json"
shots=()
# The switches of the most recent `launch`, so `capture` can retry a launch that raced the previous
# instance's exit without the caller repeating itself.
RELAUNCH_SWITCHES=()
# 1 when the launch's effective material is `opaque` (see the translucency check in CHECK 3).
OPAQUE_MATERIAL=0
fail=0
check_fail=0
build_version=""
problem() { echo "panel-edge: $*"; fail=1; check_fail=1; }

# One field of an instrument line ("label axis=top ... edge=0.898 ..."), printed alone.
field() { printf '%s\n' "$1" | tr ' ' '\n' | sed -n "s/^$2=//p" | head -1; }

# Difference helpers. `tone_*` take the instrument's 0..1 tones and a tolerance in tone units (0..255).
tone_delta() { python3 -c 'import sys; print(f"{(float(sys.argv[1])-float(sys.argv[2]))*255:+.1f}")' "$1" "$2"; }
tone_abs() { python3 -c 'import sys; print(f"{abs(float(sys.argv[1])-float(sys.argv[2]))*255:.1f}")' "$1" "$2"; }
# |a - b| <= tol tone units.
tone_close() { python3 -c 'import sys; sys.exit(0 if abs(float(sys.argv[1])-float(sys.argv[2]))*255 <= float(sys.argv[3]) else 1)' "$1" "$2" "$3"; }
# |a - b| > tol tone units.
tone_differs() { python3 -c 'import sys; sys.exit(0 if abs(float(sys.argv[1])-float(sys.argv[2]))*255 > float(sys.argv[3]) else 1)' "$1" "$2" "$3"; }
at_least() { python3 -c 'import sys; sys.exit(0 if float(sys.argv[1]) >= float(sys.argv[2]) else 1)' "$1" "$2"; }
exceeds_by() { python3 -c 'import sys; sys.exit(0 if float(sys.argv[1]) - float(sys.argv[2]) > float(sys.argv[3]) else 1)' "$1" "$2" "$3"; }
within() { python3 -c 'import sys; sys.exit(0 if abs(float(sys.argv[1]) - float(sys.argv[2])) <= float(sys.argv[3]) else 1)' "$1" "$2" "$3"; }
summary_line() { if [ "$2" = 0 ]; then printf '  %-32s PASS\n' "$1"; else printf '  %-32s FAIL\n' "$1"; fi; }

launch() {  # <label> <switches...>
    local label="$1"; shift
    RELAUNCH_SWITCHES=("$@")
    local log
    log="$(mktemp -t panel-edge-launch)"
    pkill -f "Oh-My-Tab-Dev.app" 2>/dev/null || true
    sleep 1
    rm -f "$state"
    if ! scripts/dev-restart.sh --no-onboarding --show-clipboard --e2e-state="$state" "$@" >"$log" 2>&1; then
        echo "panel-edge: dev-restart.sh failed for the $label launch:"
        tail -n 20 "$log"
        exit 1
    fi
    [ -n "$build_version" ] || build_version="$(sed -n 's/^build-version: //p' "$log" | tail -1)"
    rm -f "$log"
    sleep "$SETTLE_S"
}

# The state document is written on the app's own schedule, so the capture waits for a frame that carries a
# *visible* picker instead of trusting the launch's settle time alone; a document left by an earlier launch
# cannot satisfy it, because `launch` deletes it first.
state_ready() {
    python3 - "$state" <<'PY'
import json, sys
try:
    s = json.load(open(sys.argv[1]))
except Exception:
    raise SystemExit(1)
raise SystemExit(0 if (s.get("clipboard_picker") or {}).get("visible") else 1)
PY
}

# Holds the launch to the window/panel padding contract and prints
# "<capture rect> <panel_w> <panel_h> <outline> <elevation> <material>".
validate_capture() {  # <label> <yes|no: the window must carry the padding>
    python3 - "$state" "$1" "$2" "$PAD_L" "$PAD_T" "$PAD_R" "$PAD_B" <<'PY'
import json, sys

state, label, expect = sys.argv[1], sys.argv[2], sys.argv[3]
want = tuple(float(x) for x in sys.argv[4:8])
try:
    s = json.load(open(state))
except Exception as error:
    print(f"panel-edge: {label}: the e2e state at {state} could not be read ({error})")
    raise SystemExit(1)
panel = s.get("clipboard_picker") or {}
if not panel.get("visible"):
    print(f"panel-edge: {label}: the picker never reported a visible frame "
          f"(--show-clipboard needs the clipboard module enabled in the config)")
    raise SystemExit(1)
window = s.get("clipboard_picker_window") or {}
if not window:
    print(f"panel-edge: {label}: the state carries no clipboard_picker_window rect")
    raise SystemExit(1)
deco = s.get("panel_decorations") or {}
# The panel's origin inside the window, per side: the same numbers the instrument is given as --pad-*.
padding = (panel["x"] - window["x"], panel["y"] - window["y"],
           (window["x"] + window["w"]) - (panel["x"] + panel["w"]),
           (window["y"] + window["h"]) - (panel["y"] + panel["h"]))
if expect == "yes":
    if any(abs(got - asked) > 0.5 for got, asked in zip(padding, want)):
        print(f"panel-edge: {label}: the window is not the panel padded by {want} "
              f"(panel {panel['x']},{panel['y']},{panel['w']},{panel['h']}; "
              f"window {window['x']},{window['y']},{window['w']},{window['h']}; padding {padding})")
        raise SystemExit(1)
elif any(abs(got) > 0.5 for got in padding):
    # The switch under test is exactly this: `--panel-shadow=off` removes the carrier, and the padding
    # exists for the carrier's shadow. A window still padded means the launch produced no baseline.
    print(f"panel-edge: {label}: --panel-shadow=off must leave the window equal to the panel, "
          f"but the window is padded by {padding}")
    raise SystemExit(1)
rect = (panel["x"] - want[0], panel["y"] - want[1],
        panel["w"] + want[0] + want[2], panel["h"] + want[1] + want[3])
print(f"{rect[0]:.0f},{rect[1]:.0f},{rect[2]:.0f},{rect[3]:.0f} "
      f"{panel['w']:.0f} {panel['h']:.0f} {deco.get('outline')} {deco.get('elevation')} "
      f"{s.get('panel_material')} {padding[0]:.0f}/{padding[1]:.0f}/{padding[2]:.0f}/{padding[3]:.0f}")
PY
}

capture() {  # <label> <yes|no>
    local label="$1" expect="$2" before="" after="" out="" rc=0 waited=0
    while ! state_ready; do
        if [ "$waited" -ge "$STATE_WAIT_S" ]; then break; fi
        sleep 1
        waited=$((waited + 1))
    done
    # A launch that raced the previous instance's exit leaves the new process refused by the single-instance
    # lock, and then no state is ever written. That is a launch failure, not a verdict about the panel, so the
    # launch is retried once (with the same switches, which the caller owns) before the capture is judged: a
    # scenario that fails the suite because two launches overlapped is a broken gate.
    if ! state_ready; then
        echo "  $label: no visible picker after ${STATE_WAIT_S}s; relaunching once (a raced single-instance lock looks the same)"
        launch "$label" "${RELAUNCH_SWITCHES[@]}"
        waited=0
        while ! state_ready; do
            if [ "$waited" -ge "$STATE_WAIT_S" ]; then break; fi
            sleep 1
            waited=$((waited + 1))
        done
    fi
    out="$(validate_capture "$label" "$expect")" || rc=$?
    if [ "$rc" != 0 ]; then
        problem "${out#panel-edge: }"
        return 1
    fi
    read -r rect panel_w panel_h deco_outline deco_elev material padding <<<"$out"
    before="$out"
    SHOT="$(mktemp -t "panel-edge-$label").png"
    shots+=("$SHOT")
    screencapture -x -o -R "$rect" "$SHOT"
    # The panel must still be where the capture was taken: a frame written while the picker was still
    # animating into place would put every band on the wrong surface.
    rc=0
    after="$(validate_capture "$label" "$expect")" || rc=$?
    if [ "$rc" != 0 ] || [ "$after" != "$before" ]; then
        problem "$label: the panel is not where the capture was taken (before: $before; after: ${after:-unreadable})"
        return 1
    fi
    PANEL_W="$panel_w"
    PANEL_H="$panel_h"
    DECO_OUTLINE="$deco_outline"
    DECO_ELEV="$deco_elev"
    MATERIAL="$material"
    # The translucency assertion below has to know which material is *effectively* in play: `opaque` is a
    # legitimate setting (and Reduce Transparency forces it), so its backdrop-independence is asserted the
    # other way round instead of being reported as a regression.
    case "$material" in
        opaque) OPAQUE_MATERIAL=1 ;;
        *) OPAQUE_MATERIAL=0 ;;
    esac
    echo "  $label: panel ${panel_w}x${panel_h}pt, measured window padding ${padding}pt (expected ${PAD_L}/${PAD_T}/${PAD_R}/${PAD_B} with a carrier, 0/0/0/0 without), material $material, outline=$DECO_OUTLINE elevation=$DECO_ELEV"
    echo "           capture $rect (state settled after ${waited}s)"
}

# --- edge-profile: one cut, parsed into E_* ------------------------------------------------------------
EDGE_RAW=""
E_RC=0
edge_profile() {  # <label> <axis> <offset_pt> <backdrop_tone|->
    local label="$1" axis="$2" offset="$3" tone="$4"
    local args
    args=(--edge-profile "$SHOT" --axis "$axis" --offset "$offset"
          --pad-pt "$PAD_L" --pad-bottom "$PAD_B" --panel-w-pt "$PANEL_W" --label "$label")
    if [ "$tone" != "-" ]; then args+=(--backdrop-tone "$tone"); fi
    E_RC=0
    EDGE_RAW="$(python3 scripts/e2e/lib/png_stats.py "${args[@]}" 2>&1)" || E_RC=$?
    E_AXIS="$axis"
    E_CLASS="$(field "$EDGE_RAW" class)"
    E_REASON="$(field "$EDGE_RAW" reason)"
    E_CLASSREASON="$E_CLASS"
    [ -z "$E_REASON" ] || E_CLASSREASON="$E_CLASS/$E_REASON"
    E_OUTER="$(field "$EDGE_RAW" outer)"
    E_BOUND="$(field "$EDGE_RAW" bound)"
    E_STEP="$(field "$EDGE_RAW" step)"
    E_EDGE="$(field "$EDGE_RAW" edge)"
    E_INTERIOR="$(field "$EDGE_RAW" interior)"
    E_RING="$(field "$EDGE_RAW" ring)"
    E_RING_DELTA="$(field "$EDGE_RAW" ring_delta)"
}

edge_header() {
    printf '  %-12s %-7s %-30s %7s %7s %8s %9s %9s %10s %-6s %7s %s\n' \
        what axis class outer bound step edge interior 'edge-int' ring 'ring-d' rc
}

edge_row() {  # <what>
    local delta="-"
    if [ "$E_EDGE" != none ] && [ "$E_INTERIOR" != none ]; then
        delta="$(tone_delta "$E_EDGE" "$E_INTERIOR")"
    fi
    printf '  %-12s %-7s %-30s %7s %7s %8s %9s %9s %10s %-6s %7s %s\n' \
        "$1" "$E_AXIS" "$E_CLASSREASON" "$E_OUTER" "$E_BOUND" "$E_STEP" \
        "$E_EDGE" "$E_INTERIOR" "$delta" "$E_RING" "$E_RING_DELTA" "$E_RC"
}

# --- hf-retention: the texture cut, parsed into H_* -----------------------------------------------------
HF_RAW=""
H_RC=0
hf_retention() {  # <label>
    local label="$1"
    H_RC=0
    HF_RAW="$(python3 scripts/e2e/lib/png_stats.py --hf-retention "$SHOT" \
        --inside "$INSIDE" --control "$CONTROL" \
        --pad-pt "$PAD_L" --pad-bottom "$PAD_B" --panel-w-pt "$PANEL_W" --label "$label" 2>&1)" || H_RC=$?
    H_CLASS="$(field "$HF_RAW" class)"
    H_REASON="$(field "$HF_RAW" reason)"
    H_CLASSREASON="$H_CLASS"
    [ -z "$H_REASON" ] || H_CLASSREASON="$H_CLASS/$H_REASON"
    H_HF="$(field "$HF_RAW" hf)"
    H_LF="$(field "$HF_RAW" lf)"
    H_HF_INSIDE="$(field "$HF_RAW" hf_inside)"
    H_HF_CONTROL="$(field "$HF_RAW" hf_control)"
    H_LF_INSIDE="$(field "$HF_RAW" lf_inside)"
    H_LF_CONTROL="$(field "$HF_RAW" lf_control)"
    H_SCALE="$(field "$HF_RAW" scale)"
}

hf_header() {
    printf '  %-12s %-24s %7s %7s %10s %11s %10s %11s %6s %s\n' \
        what class hf lf hf_inside hf_control lf_inside lf_control scale rc
}

hf_row() {  # <what>
    printf '  %-12s %-24s %7s %7s %10s %11s %10s %11s %6s %s\n' \
        "$1" "$H_CLASSREASON" "$H_HF" "$H_LF" "$H_HF_INSIDE" "$H_HF_CONTROL" \
        "$H_LF_INSIDE" "$H_LF_CONTROL" "$H_SCALE" "$H_RC"
}

# ================= CHECK 1: the outline =================================================================
echo "CHECK 1 -- outline: --panel-backdrop=white vs the same + --panel-outline=off"
launch "outline-on" --panel-backdrop=white
capture "outline-on" yes || exit 1
SHOT_ON="$SHOT"
[ "$DECO_OUTLINE" = True ] || problem "check 1: the default launch reports panel_decorations.outline=$DECO_OUTLINE, expected true"
[ "$DECO_ELEV" = high ] || problem "check 1: the default launch reports panel_decorations.elevation=$DECO_ELEV, expected high"
edge_header
edge_profile "outline-on" top "$((PANEL_W / 2))" 1
edge_row "outline-on"
ON_TOP_RC="$E_RC"; ON_TOP_CLASS="$E_CLASS"; ON_TOP_OUTER="$E_OUTER"; ON_TOP_EDGE="$E_EDGE"; ON_TOP_INT="$E_INTERIOR"
ON_TOP_RING="$E_RING"; ON_TOP_RING_D="$E_RING_DELTA"
# The bottom cut of this same launch is the shadow-on frame CHECK 2 compares against.
edge_profile "outline-on" bottom "$((PANEL_W / 2))" 1
edge_row "outline-on"
ON_BOTTOM_RC="$E_RC"; ON_BOTTOM_CLASS="$E_CLASS"; ON_BOTTOM_OUTER="$E_OUTER"
ON_BOTTOM_INT="$E_INTERIOR"; ON_BOTTOM_RING="$E_RING"; ON_BOTTOM_RING_D="$E_RING_DELTA"
# CHECK 1 asserts on the top edge alone, and that is deliberate: it is the only edge that is bare material in
# both launches. The bottom band carries the footer's wash (measured: interior 208 against the top edge's 220
# with the outline off, i.e. the band is content, not outline) and the left/right edges carry the selected
# row's accent bar, so a cut there reads as ink the outline never drew. The instrument's header states the
# same limit: a band that is not bare material cannot be judged.
# No left/right cut: the picker draws the selected row's accent bar along its left edge, so that edge is not
# bare material and a cut through it reads as ink the outline never drew (measured: 26 tone units with the
# outline *off*). An earlier version cut there and, worse, wrote the result into the `ON_BOTTOM_*` names that
# CHECK 2 compares against the shadow-off launch.

echo
launch "outline-off" --panel-backdrop=white --panel-outline=off
capture "outline-off" yes || exit 1
[ "$DECO_OUTLINE" = False ] || problem "check 1: --panel-outline=off still reports panel_decorations.outline=$DECO_OUTLINE"
edge_header
edge_profile "outline-off" top "$((PANEL_W / 2))" 1
edge_row "outline-off"
OFF_TOP_RC="$E_RC"; OFF_TOP_EDGE="$E_EDGE"; OFF_TOP_INT="$E_INTERIOR"


# Counter-example: an implementation that only averages the interior reports the same `edge` on both
# launches, and one that strokes the window's padded edge instead of the panel's reports it outside every
# band. Both are caught by holding the *step from the interior* to be larger with the outline on, and by
# refusing to let the two launches report the same edge at all.
assert_outline() {  # <axis> <on_rc> <on_edge> <on_int> <off_rc> <off_edge> <off_int>
    local axis="$1" on_rc="$2" on_edge="$3" on_int="$4" off_rc="$5" off_edge="$6" off_int="$7"
    if [ "$on_rc" != 0 ] || [ "$off_rc" != 0 ]; then
        problem "check 1/$axis: the instrument reported rc=$on_rc (outline on) / rc=$off_rc (outline off); rc 4 or 5 is a failure"
        return
    fi
    if [ "$on_edge" = none ] || [ "$on_int" = none ] || [ "$off_edge" = none ] || [ "$off_int" = none ]; then
        problem "check 1/$axis: the instrument reported no edge/interior band"
        return
    fi
    local on_delta off_delta
    on_delta="$(tone_abs "$on_edge" "$on_int")"
    off_delta="$(tone_abs "$off_edge" "$off_int")"
    echo "  check 1/$axis: edge-interior ${on_delta} tone units (outline on) vs ${off_delta} (off)"
    if tone_close "$on_edge" "$off_edge" "$NOISE_TONE"; then
        problem "check 1/$axis: both launches report the same edge ($on_edge vs $off_edge): the switch did not take effect"
    fi
    if ! exceeds_by "$on_delta" "$off_delta" "$NOISE_TONE"; then
        problem "check 1/$axis: the outline's step (${on_delta}) is not larger than the switch-off step (${off_delta}) by the ${NOISE_TONE}-tone noise floor"
    fi
    if ! at_least "$on_delta" "$OUTLINE_MIN_TONE"; then
        problem "check 1/$axis: the outline's step is ${on_delta} tone units, under the instrument's ${OUTLINE_MIN_TONE}-tone band floor"
    fi
    if ! tone_close "$off_delta" 0 "$NOISE_TONE"; then
        problem "check 1/$axis: with the outline off the edge still differs from the interior by ${off_delta} tone units (noise floor ${NOISE_TONE})"
    fi
}
assert_outline top "$ON_TOP_RC" "$ON_TOP_EDGE" "$ON_TOP_INT" "$OFF_TOP_RC" "$OFF_TOP_EDGE" "$OFF_TOP_INT"

check1_fail="$check_fail"; check_fail=0
echo

# ================= CHECK 2: the elevation shadow ========================================================
echo "CHECK 2 -- shadow: --panel-backdrop=white (outline on) vs the same + --panel-shadow=off"
launch "shadow-off" --panel-backdrop=white --panel-shadow=off
capture "shadow-off" no || exit 1
[ "$DECO_ELEV" = none ] || problem "check 2: --panel-shadow=off still reports panel_decorations.elevation=$DECO_ELEV"
edge_header
edge_profile "shadow-off" top "$((PANEL_W / 2))" 1
edge_row "shadow-off"
OFF_TOP_OUTER="$E_OUTER"; OFF_TOP_RC="$E_RC"; OFF_TOP_CLASS="$E_CLASS"; OFF_TOP_REASON="$E_REASON"
OFF_TOP_INT="$E_INTERIOR"; OFF_TOP_RING_D="$E_RING_DELTA"
edge_profile "shadow-off" bottom "$((PANEL_W / 2))" 1
edge_row "shadow-off"
OFF_BOTTOM_OUTER="$E_OUTER"; OFF_BOTTOM_RC="$E_RC"; OFF_BOTTOM_CLASS="$E_CLASS"; OFF_BOTTOM_REASON="$E_REASON"
OFF_BOTTOM_INT="$E_INTERIOR"; OFF_BOTTOM_RING_D="$E_RING_DELTA"

# The shadow-off launch is the control: with no shadow outside the panel, `--backdrop-tone 1` makes the
# instrument report `no-shadow-contrast` (rc 5). That refusal *is* the expected reading -- "nothing darkens
# the backdrop next to this panel" -- and its `outer`/`bound` numbers are asserted below all the same. A
# control that returns 0 (a shadow it can see) or rc 4 (bad input) is the failure.
assert_control() {  # <axis> <rc> <class> <reason> <outer>
    local axis="$1" rc="$2" class="$3" reason="$4" outer="$5"
    if [ "$rc" = 0 ]; then
        problem "check 2/$axis: the --panel-shadow=off launch still reports a shadow (class=$class); the switch did not take effect"
        return
    fi
    if [ "$rc" != 5 ] || [ "$reason" != no-shadow-contrast ]; then
        problem "check 2/$axis: the --panel-shadow=off launch returned rc=$rc class=$class reason=${reason:-none}, expected rc=5/no-shadow-contrast"
        return
    fi
    if [ "$outer" != none ] && ! tone_close "$outer" 1 "$NOISE_TONE"; then
        problem "check 2/$axis: the shadow-off outer band is $outer, not the pinned white backdrop (no shadow outside the panel)"
    fi
}
assert_control top "$OFF_TOP_RC" "$OFF_TOP_CLASS" "$OFF_TOP_REASON" "$OFF_TOP_OUTER"
assert_control bottom "$OFF_BOTTOM_RC" "$OFF_BOTTOM_CLASS" "$OFF_BOTTOM_REASON" "$OFF_BOTTOM_OUTER"

# (a) The shadow-on bottom run must be a *decided* complete tail: `clipped` means the shadow is still
# gaining tone at the capture's outermost ring, i.e. the window padding is too small for the shadow, and
# `unobservable` means the capture cannot carry the judgement at all.
if [ "$ON_BOTTOM_RC" != 0 ] || [ "$ON_BOTTOM_CLASS" != complete ]; then
    problem "check 2/bottom: the shadow-on run is rc=$ON_BOTTOM_RC class=$ON_BOTTOM_CLASS, expected rc=0 class=complete (a clipped tail means the window padding is too small)"
fi
if [ "$ON_TOP_RC" != 0 ] || [ "$ON_TOP_CLASS" != complete ]; then
    problem "check 2/top: the shadow-on run is rc=$ON_TOP_RC class=$ON_TOP_CLASS, expected rc=0 class=complete (a clipped top tail means the padding is too small on that side)"
fi
# (b) The outer band is darker with the shadow on than off. Counter-example: the switch reported the state
# but the carrier's shadow never rendered -- AppKit zeroes a view-backed layer's shadowOpacity, the failure
# the raw-CALayer-sublayer design exists for; the pixels would then be identical apart from the padding.
ON_TOP_DARK="$(tone_abs "$ON_TOP_OUTER" 1)"
OFF_TOP_DARK="$(tone_abs "$OFF_TOP_OUTER" 1)"
ON_BOTTOM_DARK="$(tone_abs "$ON_BOTTOM_OUTER" 1)"
OFF_BOTTOM_DARK="$(tone_abs "$OFF_BOTTOM_OUTER" 1)"
echo "  darkening (backdrop minus outer) top ${ON_TOP_DARK} vs ${OFF_TOP_DARK}, bottom ${ON_BOTTOM_DARK} vs ${OFF_BOTTOM_DARK} tone units"
if ! exceeds_by "$ON_TOP_DARK" "$OFF_TOP_DARK" "$SHADOW_MIN_DEPTH"; then
    problem "check 2/top: the outer band is darker by ${ON_TOP_DARK} with the shadow on vs ${OFF_TOP_DARK} off, under the instrument's ${SHADOW_MIN_DEPTH}-tone shadow floor"
fi
if ! exceeds_by "$ON_BOTTOM_DARK" "$OFF_BOTTOM_DARK" "$SHADOW_MIN_DEPTH"; then
    problem "check 2/bottom: the outer band is darker by ${ON_BOTTOM_DARK} with the shadow on vs ${OFF_BOTTOM_DARK} off, under the instrument's ${SHADOW_MIN_DEPTH}-tone shadow floor"
fi
# (c) The darkening is larger below than above: the shadow is offset 12pt downward. Counter-example: a
# shadow drawn symmetrically (offset 0) or on the wrong edge -- the padding numbers alone (bottom 76 vs top
# 64) would not catch it, because they are geometry, not rendered pixels.
if ! exceeds_by "$ON_BOTTOM_DARK" "$ON_TOP_DARK" "$SHADOW_MIN_DEPTH"; then
    problem "check 2: the darkening below (${ON_BOTTOM_DARK}) is not larger than above (${ON_TOP_DARK}) by the ${SHADOW_MIN_DEPTH}-tone floor, so the shadow is not offset downward"
fi
# (d) The interior band is unchanged between the launches. Counter-example: a shadow path set to the window
# rect instead of the panel rect, or a shadow left on the material's own (unclipped) layer, darkens the
# panel from inside -- the shadow would then be measured on the surface it is supposed to fall behind.
for axis in top bottom; do
    eval "on_ring=\${ON_$(echo "$axis" | tr a-z A-Z)_RING}"
    eval "on_ring_d=\${ON_$(echo "$axis" | tr a-z A-Z)_RING_D} off_ring_d=\${OFF_$(echo "$axis" | tr a-z A-Z)_RING_D}"
    if [ "$on_ring" = present ]; then
        problem "check 2/$axis: the shadow-on inner ring reads present (ring_delta=${on_ring_d}): the panel is darkened from inside by its own shadow"
    fi
    # `ring_delta` is already in tone units (0..255), not the instrument's 0..1 tones, so it is compared
    # directly: `tone_close` would scale it by 255 again and shrink the tolerance to nothing.
    if [ "$on_ring_d" != none ] && [ "$off_ring_d" != none ] && ! within "$on_ring_d" "$off_ring_d" "$NOISE_TONE"; then
        problem "check 2/$axis: the inner ring moved to ${on_ring_d} with the shadow on from ${off_ring_d} off, over the ${NOISE_TONE}-tone noise floor"
    fi
    eval "on_int=\${ON_$(echo "$axis" | tr a-z A-Z)_INT} off_int=\${OFF_$(echo "$axis" | tr a-z A-Z)_INT}"
    if ! tone_close "$on_int" "$off_int" "$NOISE_TONE"; then
        problem "check 2/$axis: the interior band moved from $on_int (shadow on) to $off_int (shadow off) by more than the ${NOISE_TONE}-tone noise floor"
    else
        echo "  check 2/$axis: interior unchanged ($on_int vs $off_int, $(tone_abs "$on_int" "$off_int") tone units)"
    fi
done
check2_fail="$check_fail"; check_fail=0
echo

# ================= CHECK 3: the material still blurs ====================================================
echo "CHECK 3 -- material blur: --panel-backdrop=texture (carrier present) vs the same + --panel-shadow=off (carrier gone)"
launch "carrier-on" --panel-backdrop=texture
capture "carrier-on" yes || exit 1
[ "$DECO_ELEV" = high ] || problem "check 3: the carrier launch reports panel_decorations.elevation=$DECO_ELEV, expected high"
[ "$DECO_OUTLINE" = True ] || problem "check 3: the carrier launch reports panel_decorations.outline=$DECO_OUTLINE, expected true"
hf_header
hf_retention "carrier-on"
hf_row "carrier-on"
ON_HF_RC="$H_RC"; ON_HF_CLASS="$H_CLASS"; ON_HF="$H_HF"; ON_HF_LF="$H_LF"; ON_HF_RAW="$HF_RAW"
echo

launch "carrier-off" --panel-backdrop=texture --panel-shadow=off
capture "carrier-off" no || exit 1
[ "$DECO_ELEV" = none ] || problem "check 3: --panel-shadow=off still reports panel_decorations.elevation=$DECO_ELEV, so the pre-carrier baseline was not reached"
hf_header
hf_retention "carrier-off"
hf_row "carrier-off"
OFF_HF_RC="$H_RC"; OFF_HF_CLASS="$H_CLASS"; OFF_HF="$H_HF"; OFF_HF_LF="$H_LF"; OFF_HF_RAW="$HF_RAW"

# What this gate can and cannot decide, stated rather than implied.
#
# It decides the two failure modes it exists for: **the blur is gone** (the material passes the fine bars, so
# hf approaches the control's and the class word becomes `preserved`), and **the carrier changed what the
# material passes** (the two launches' hf/lf ratios diverge; `--panel-shadow=off` removes the carrier and
# restores the pre-carrier hierarchy, so that pair is the comparison the carrier has to survive).
#
# It does not decide `blurred` vs `covered`: `lf` is the ladder's coarse retention against a control that also
# carries 1-8px bars no blur can keep, so `lf_control` is inflated by detail the material cannot pass and the
# class floor of 0.1 turns a live blur into `covered` on this ladder (measured on the bare band: hf 0.000, lf
# 0.033). A 32px `--lf-block-px` would read the surviving coarse bars, but the instrument refuses that block
# for a region under 4*block, and the picker has no 128px-tall bare band. The opaque-fill failure mode
# (`covered` for real) is asserted elsewhere: the A1 smoke's `backdrop_surface_matches_palette` and
# `panel-contrast.sh`'s surface-tone measurement.
assert_blur() {  # <label> <rc> <class> <hf> <raw>
    if [ "$2" != 0 ]; then
        problem "check 3/$1: the instrument returned rc=$2; any refusal (rc 4/5) is a failure (raw: $5)"
        return
    fi
    if [ "$3" = preserved ]; then
        problem "check 3/$1: class=preserved -- the material passes the fine backdrop detail, so it is not blurring (raw: $5)"
        return
    fi
    if [ "$4" = none ] || ! python3 -c "import sys; sys.exit(0 if float('$4') <= $HF_ALIVE_MAX else 1)"; then
        problem "check 3/$1: hf=$4, over the $HF_ALIVE_MAX a live blur stays under (raw: $5)"
    fi
}
assert_blur "carrier-on" "$ON_HF_RC" "$ON_HF_CLASS" "$ON_HF" "$ON_HF_RAW"
assert_blur "carrier-off" "$OFF_HF_RC" "$OFF_HF_CLASS" "$OFF_HF" "$OFF_HF_RAW"
if [ "$ON_HF" != none ] && [ "$OFF_HF" != none ] && ! within "$ON_HF" "$OFF_HF" "$HF_TOL"; then
    problem "check 3: the hf ratios disagree by more than the ${HF_TOL} tolerance (carrier on $ON_HF, off $OFF_HF): the carrier changed what the material passes"
else
    echo "  check 3: hf $ON_HF (carrier on) vs $OFF_HF (carrier off), within the ${HF_TOL} absolute tolerance"
fi
if [ "$ON_HF_LF" != none ] && [ "$OFF_HF_LF" != none ] && ! within "$ON_HF_LF" "$OFF_HF_LF" "$HF_TOL"; then
    problem "check 3: the lf ratios disagree by more than the ${HF_TOL} tolerance (carrier on $ON_HF_LF, off $OFF_HF_LF)"
fi
echo "  raw carrier-on : $ON_HF_RAW"
echo "  raw carrier-off: $OFF_HF_RAW"

# The failure mode the class word cannot decide on this ladder: a material replaced by an *opaque* cover
# passes neither band, exactly like a live blur does. It is decided directly instead -- a translucent
# surface's interior tone follows the backdrop it is sampling, an opaque one's does not -- so the picker is
# read once more over the opposite extreme (`--panel-backdrop=black`) and the two interiors are compared.
# Counter-example: `--panel-backdrop` is what varies, so a material that ignored the backdrop would read the
# same tone on both, and an implementation that painted an opaque sheet would land here.
echo "  translucency: the same bare band over white vs black"
launch "opaque-check" --panel-backdrop=black
capture "opaque-check" yes || exit 1
edge_profile "opaque-check" top "$((PANEL_W / 2))" 0
edge_row "opaque-check"
BLACK_INT="$E_INTERIOR"
WHITE_INT="$ON_TOP_INT"
if [ "$BLACK_INT" = none ] || [ "$WHITE_INT" = none ]; then
    problem "check 3: the interior tone could not be read over one of the backdrops (white=$WHITE_INT black=$BLACK_INT)"
elif [ "$OPAQUE_MATERIAL" = 1 ]; then
    # `opaque` (the user's setting, or Reduce Transparency forcing it) is *supposed* to ignore the backdrop,
    # so the assertion inverts rather than being skipped: a surface that follows the backdrop there would mean
    # the material setting did not take effect. Recorded, not silently unrun.
    # The claim here is backdrop *independence*, so the bound is the measurement's own noise floor, not the
    # translucency threshold: a dependence between the two would be a real regression even though it is far
    # short of what a translucent surface shows.
    opaque_delta="$(tone_abs "$WHITE_INT" "$BLACK_INT")"
    echo "  interior over white ${WHITE_INT} vs black ${BLACK_INT}: ${opaque_delta} tone units (opaque: must not follow the backdrop, floor ${NOISE_TONE})"
    if ! tone_close "$WHITE_INT" "$BLACK_INT" "$NOISE_TONE"; then
        problem "check 3: the effective material is opaque but its interior still follows the backdrop (${opaque_delta} tone units, over the ${NOISE_TONE}-tone noise floor)"
    fi
else
    translucent_delta="$(tone_abs "$WHITE_INT" "$BLACK_INT")"
    echo "  interior over white ${WHITE_INT} vs black ${BLACK_INT}: ${translucent_delta} tone units (floor ${TRANSLUCENT_MIN_TONE})"
    if ! at_least "$translucent_delta" "$TRANSLUCENT_MIN_TONE"; then
        problem "check 3: the panel's interior differs by only ${translucent_delta} tone units between the white and black backdrops, under the ${TRANSLUCENT_MIN_TONE}-tone floor: the surface no longer follows its backdrop (an opaque cover reads the same over both)"
    fi
fi
check3_fail="$check_fail"; check_fail=0
echo

# ================= summary ==============================================================================
echo "PASS/FAIL SUMMARY"
summary_line "check 1 outline (white backdrop)" "$check1_fail"
summary_line "check 2 shadow (white backdrop)" "$check2_fail"
summary_line "check 3 material blur (texture)" "$check3_fail"
echo
echo "panel-edge: $( [ "$fail" = 0 ] && echo PASS || echo FAIL ) (build ${build_version:-unknown})"
rm -f "$SHOT_ON" "$SHOT" "$state" ${shots[@]+"${shots[@]}"}
exit "$fail"
