#!/bin/bash
# E2E_CHANGES_PREFS=1
# A2: rendered-pixel contrast for the translucent panel materials.
#
# Why a screenshot: the panel material composites *behind* the window, so a view-tree bitmap cannot contain
# it. `--e2e-state` reports the picker's frame (`clipboard_picker`), which is what makes a screenshot
# measurable without guessing where the panel is.
#
# Regions: the picker's filter row and its footer legend band, both bare material with caption-sized labels
# and no thumbnails. The tier asserted is the panel caption floor from docs/design-style: 3:1.
#
# Usage: scripts/e2e/panel-contrast.sh [material ...]     (defaults to frost liquid-glass)
set -euo pipefail
# What this measures, and how: the two frames come from *one* launch. The first is captured while the panel
# still has its text; the app then hides the panel's text fields in place (`--clipboard-blank-text=after:N`,
# same window, same layout, same material), and the second frame is that same panel without glyphs. The pixels
# that differ between the two are therefore the glyph pixels -- not the material's own tonal noise, which a
# single capture cannot separate from text (on dark frost a single frame yields a dozen "inks" at 1.03-1.13:1).
# Nothing about this is inferred from the desktop: a solid window is pinned behind the panel
# (`--panel-backdrop`), because a translucent surface's tone follows what is behind it.
#
# The measurement itself lives in lib/png_stats.py (see its --selftest for the nine synthetic cases it is held
# to). Its limits, stated rather than implied: an ink whose own contribution is under DIFF_TONE tone units is
# not visible to it; the regions are the filter row and the footer legend band, so roles outside them are not
# covered here; and the scenario rewrites the theme for its run, which is why run-all.sh gates it behind
# --include-prefs.
#
# Timing: BLANK_AFTER_S must exceed the settle time the launch needs, and BLANK_WAIT_S the remainder, so that
# frame A is taken with text and frame B after the hide.
BLANK_AFTER_S="${BLANK_AFTER_S:-12}"
BLANK_WAIT_S="${BLANK_WAIT_S:-14}"

# Regions are given in the panel's own points (top-left origin); the sampler converts them with the scale it
# derives from the capture itself.
ROW_X_PT="${ROW_X_PT:-20}"
ROW_Y_PT="${ROW_Y_PT:-62}"
ROW_W_PT="${ROW_W_PT:-450}"
ROW_H_PT="${ROW_H_PT:-30}"

# The two translucent materials by default: the tier is about them. `opaque` is the control that must not move
# with the backdrop. Any material names on the command line replace the default.
materials=(frost liquid-glass)
if [ "$#" -gt 0 ]; then
    materials=("$@")
fi

config="${CONFIG_PATH:-$HOME/.config/oh-my-tab/config.toml}"
backup="$(mktemp -t panel-contrast-config)"
cp "$config" "$backup"
state="$(mktemp -t panel-contrast-state).json"

# Everything here ends with a clean relaunch, not just the config: the scenario launches the app with
# development switches (--clipboard-blank-text, --panel-backdrop, --panel-material), and leaving the last one
# running hands the user a panel with no text -- `--clipboard-blank-text` blanks every string that goes through
# `glass::panel_ink` (row titles, filter labels, keycap legends, the search placeholder), while the accent
# colours that do not, such as the red "clear all", keep drawing. That happened once; hence this trap.
restore() {
    cp "$backup" "$config"
    rm -f "$backup"
    scripts/dev-restart.sh --no-onboarding >/dev/null 2>&1 || true
}
trap restore EXIT

# Initialised before any use: with `set -u` an unset `fail` would abort a *successful* run at `exit "$fail"`.
fail=0

# Run from anywhere: the launcher below is a repository-relative path, and this scenario kills the app before
# relaunching it, so the wrong cwd would take the panel down and then fail to bring it back (the restore trap
# would fail too, silently). Same convention as the other scripts in this directory.
repo_dir="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$repo_dir"

for theme in light dark; do
    for material in "${materials[@]}"; do
        for shade in black white; do
        python3 - "$config" "$theme" <<'PY'
import pathlib, re, sys
p = pathlib.Path(sys.argv[1]); s = p.read_text()
p.write_text(re.sub(r'^theme = ".*"$', f'theme = "{sys.argv[2]}"', s, flags=re.M))
PY
        pkill -f "Oh-My-Tab-Dev.app" 2>/dev/null || true
        sleep 1
        # The backdrop is pinned: a translucent material's tone follows what is behind it, so measuring over
        # whatever the desktop shows would make the verdict depend on the desktop. `--panel-backdrop` puts a
        # known solid window *behind* the panel for this launch.
        scripts/dev-restart.sh --no-onboarding --show-clipboard --e2e-state="$state" \
            --panel-material="$material" --panel-backdrop="$shade" \
            --clipboard-blank-text="after:$BLANK_AFTER_S" >/dev/null 2>&1
        sleep 5
        read -r px py pw ph < <(python3 - "$state" <<'PY'
import json, sys
try:
    frame = json.load(open(sys.argv[1])).get("clipboard_picker")
except Exception:
    frame = None
if not frame or not frame.get("visible"):
    print("0 0 0 0")
else:
    print(int(frame["x"]), int(frame["y"]), int(frame["w"]), int(frame["h"]))
PY
)
        hints="$(python3 - "$state" <<'STATE'
import json, sys
state = json.load(open(sys.argv[1]))
for hint in (state.get("clipboard_picker") or {}).get("footer_hints") or []:
    print("{:.1f} {:.1f} {:.1f} {:.1f}".format(hint["x"], hint["y"], hint["w"], hint["h"]))
STATE
)"
        if [ "$pw" -le 0 ]; then
            echo "panel-contrast: $theme/$material: the picker never reported a visible frame"
            fail=1
            continue
        fi
        shot="$(mktemp -t panel-contrast).png"
        screencapture -x -R "${px},${py},${pw},${ph}" "$shot"
        # Same launch, same window: wait past the hide and take the frame without glyphs. The picker must still
        # be where it was, or the difference would be its position rather than its text.
        sleep "$BLANK_WAIT_S"
        after="$(python3 - "$state" <<'STATE'
import json, sys
frame = (json.load(open(sys.argv[1])).get("clipboard_picker") or {})
print("" if not frame else "{:.1f} {:.1f} {:.1f} {:.1f}".format(frame["x"], frame["y"], frame["w"], frame["h"]))
STATE
)"
        if [ "$after" != "$(printf '%.1f %.1f %.1f %.1f' "$px" "$py" "$pw" "$ph")" ]; then
            echo "panel-contrast: $theme/$material/$shade: the panel is no longer where the first frame was ($after vs $px $py $pw $ph)"
            fail=1
            rm -f "$shot"
            continue
        fi
        shot_blank="$(mktemp -t panel-contrast-blank).png"
        screencapture -x -R "${px},${py},${pw},${ph}" "$shot_blank"
        printf "%-6s %-13s %-6s\n" "$theme" "$material" "$shade"
        # One region, one text role, one tier. Each region holds a single ink (a chip label / a legend label),
        # so the sampler measures *that* ink instead of the strongest pixel anywhere in the region.
        measure() {  # <label> <x_pt> <y_pt> <w_pt> <h_pt>
            local out rc ratio
            rc=0
            out="$(python3 scripts/e2e/lib/png_stats.py --diff "$shot" "$shot_blank" "$2" "$3" "$4" "$5" "$pw" "$1")" || rc=$?
            echo "           $out"
            case "$rc" in
                0) ;;
                2) echo "panel-contrast: $theme/$material/$shade: the two captures do not differ inside the $1 region, so it holds no text; the area is wrong"; fail=1; return ;;
                *) echo "panel-contrast: $theme/$material/$shade: the $1 region could not be measured"; fail=1; return ;;
            esac
            ratio="${out#*contrast=}"
            ratio="${ratio%%:*}"
            # The panel caption tier: text drawn on the material must hold 3:1 (docs/design-style, "panel
            # tier"). A material that cannot is not shippable as-is, so this fails the run rather than warns.
            if ! python3 -c "import sys; sys.exit(0 if float('$ratio') >= 3.0 else 1)"; then
                echo "panel-contrast: $theme/$material/$shade: the $1 region measured ${ratio}:1, below the 3:1 panel tier"
                fail=1
            fi
        }
        measure "filter-row" "$ROW_X_PT" "$ROW_Y_PT" "$ROW_W_PT" "$ROW_H_PT"
        if [ -n "$hints" ]; then
            # One region per caption: the union of them is a bounding box that also spans the keycaps, so a
            # change anywhere along that row used to be read as the caption's ink.
            index=0
            while read -r hx hy hw hh; do
                [ -n "$hx" ] || continue
                index=$((index + 1))
                measure "footer-hint-$index" "$hx" "$hy" "$hw" "$hh"
            done <<<"$hints"
        else
            echo "panel-contrast: $theme/$material/$shade: the e2e state carries no footer captions to measure"
            fail=1
        fi
        rm -f "$shot" "$shot_blank"
        done
    done
done
pkill -f "Oh-My-Tab-Dev.app" 2>/dev/null || true
exit "$fail"
