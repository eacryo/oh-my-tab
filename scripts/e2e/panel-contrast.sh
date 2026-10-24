#!/bin/bash
# E2E_CHANGES_PREFS=1
# E2E_COVERS_SCREEN=1
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
# --include-prefs. It also pins a full-screen backdrop behind the panel and captures the screen, which is
# the separate E2E_COVERS_SCREEN gate (--include-screen): the two agreements are asked for separately.
#
# Timing: BLANK_AFTER_S must exceed the settle time the launch needs, and BLANK_WAIT_S the remainder, so that
# frame A is taken with text and frame B after the hide.
BLANK_AFTER_S="${BLANK_AFTER_S:-12}"
BLANK_WAIT_S="${BLANK_WAIT_S:-14}"

# Regions are given in the panel's own points (top-left origin); the sampler converts them with the scale it
# derives from the capture itself.
#
# The filters row is not one of them: its rect comes from the launch's own --e2e-state, because a constant
# here went stale when the search field above the row moved and every material then reported "a band
# changed, that is not text". Footer captions report their frames the same way.

# The two translucent materials by default: the tier is about them. `opaque` is the control that must not move
# with the backdrop. Any material names on the command line replace the default.
materials=(frost liquid-glass)
if [ "$#" -gt 0 ]; then
    materials=("$@")
fi

repo_dir="$(cd "$(dirname "$0")/../.." && pwd)"
config="${CONFIG_PATH:-$HOME/.config/oh-my-tab/config.toml}"
state="$(mktemp -t panel-contrast-state).json"
# Only the ONE key this scenario rewrites is restored, and it is restored to what the file actually had:
# a full-file backup copied at the start would silently undo anything the user changed while the run was
# in progress, and writing a default back would add a setting that was never there. The marker records
# whether the key existed at all, so "absent" can be put back as absent.
theme_existed=0
original_theme=""
last_written_theme=""
wrote_theme=0
if [ -f "$config" ]; then
    # Three outcomes, and they must stay distinct: a value, "absent" (exit 3), and "could not be read"
    # (exit 4). The third one stops the run BEFORE anything is written -- a config that cannot be parsed
    # must not be treated as a config without a theme, or the restore would delete a setting that was
    # there. Only "absent" may be put back as absent.
    if original_theme="$(python3 -B "$repo_dir/scripts/e2e/lib/config_switch.py" --get-string "$config" appearance theme 2>/dev/null)"; then
        theme_existed=1
    else
        read_status=$?
        if [ "$read_status" -ne 3 ]; then
            echo "panel-contrast: cannot read appearance.theme from $config (exit $read_status); refusing to rewrite it" >&2
            exit 1
        fi
        original_theme=""
    fi
fi

# Everything here ends with a clean relaunch, not just the config: the scenario launches the app with
# development switches (--clipboard-blank-text, --panel-backdrop, --panel-material), and leaving the last one
# running hands the user a panel with no text -- `--clipboard-blank-text` blanks every string that goes through
# `glass::panel_ink` (row titles, filter labels, keycap legends, the search placeholder), while the accent
# colours that do not, such as the red "clear all", keep drawing. That happened once; hence this trap.
# The restore also has to be able to FAIL: a trap that always reports success hands the user a rewritten
# theme and a defeated gate that says PASS. The original status is kept and a failed restore raises it.
restore() {
    local status=$?
    trap - EXIT
    local restore_failed=0
    # Only put the key back while the file still holds what THIS scenario wrote. A person who changed
    # the theme while the run was going has made a newer decision than the snapshot taken at the start;
    # restoring over it would be a second disturbance, not a cleanup.
    current_theme=""
    current_status=3
    if [ -f "$config" ]; then
        # `|| current_status=$?`: under `set -e`, a bare assignment would end the trap on exit 3 (the key
        # is absent) or 4 (unreadable), skipping the relaunch and leaving the dev switches running.
        current_status=0
        current_theme="$(python3 -B "$repo_dir/scripts/e2e/lib/config_switch.py" --get-string "$config" appearance theme 2>/dev/null)" || current_status=$?
    fi
    if [ "$wrote_theme" != "1" ]; then
        # This run never changed the config, so it has nothing to put back: whatever is there now
        # belongs to the user (including a change made between startup and the first write).
        :
    elif [ "$current_status" -ne 0 ] && [ "$current_status" -ne 3 ]; then
        echo "panel-contrast: the config could not be re-read on exit (exit $current_status); leaving it untouched" >&2
        restore_failed=1
    elif [ "$current_status" -eq 3 ] && [ -n "$last_written_theme" ]; then
        # The key this run wrote is GONE: someone removed it. That is a newer decision than the snapshot,
        # so putting the original value back would undo it.
        echo "panel-contrast: appearance.theme was removed during the run; keeping it removed" >&2
    elif [ -z "$last_written_theme" ] || [ "$current_theme" = "$last_written_theme" ]; then
        if [ "$theme_existed" = "1" ]; then
            python3 -B "$repo_dir/scripts/e2e/lib/config_switch.py" --set-string "$config" appearance theme "$original_theme" \
                || restore_failed=1
        else
            python3 -B "$repo_dir/scripts/e2e/lib/config_switch.py" --unset "$config" appearance theme \
                || restore_failed=1
        fi
    else
        echo "panel-contrast: appearance.theme is now '$current_theme', which this run did not write; keeping it" >&2
    fi
    rm -f "$state"
    "$repo_dir/scripts/dev-restart.sh" --no-onboarding >/dev/null 2>&1 || {
        echo "panel-contrast: the clean relaunch FAILED; the app may still carry development switches" >&2
        restore_failed=1
    }
    if [ "$restore_failed" -ne 0 ]; then
        echo "panel-contrast: restore FAILED; the config's appearance.theme may still be a test value" >&2
        [ "$status" -ne 0 ] || status=1
    fi
    exit "$status"
}
trap restore EXIT
trap 'exit 130' INT TERM

# Initialised before any use: with `set -u` an unset `fail` would abort a *successful* run at `exit "$fail"`.
fail=0
filter_row_not_run=0


# Run from anywhere: the launcher below is a repository-relative path, so the wrong cwd would take the panel
# down and then fail to bring it back (the restore trap would fail too, silently). Same convention as the
# other scripts in this directory. The dev instance is stopped by dev-restart.sh, under the transaction
# claim: a stop issued from here first would happen outside it.
cd "$repo_dir"

for theme in light dark; do
    for material in "${materials[@]}"; do
        for shade in black white; do
        # Before every write: the key must still hold what this run left there (the first round accepts
        # the value found at startup, or its absence). A user change between rounds is newer than this
        # scenario and must not be overwritten -- the run stops instead.
        current_status=0
        current_theme="$(python3 -B "$repo_dir/scripts/e2e/lib/config_switch.py" --get-string "$config" appearance theme 2>/dev/null)" || current_status=$?
        if [ "$current_status" -eq 4 ]; then
            echo "panel-contrast: $config cannot be parsed; refusing to rewrite it" >&2
            exit 1
        fi
        if [ -n "$last_written_theme" ]; then
            if [ "$current_status" -ne 0 ] || [ "$current_theme" != "$last_written_theme" ]; then
                echo "panel-contrast: appearance.theme changed during the run ('$current_theme'); keeping that and stopping" >&2
                exit 1
            fi
        elif [ "$current_status" -eq 0 ] && [ "$current_theme" != "$original_theme" ]; then
            # Nobody has written yet, so this change is not this scenario's: do not touch the file on
            # the way out either. `wrote_theme` is what the restore reads to decide that.
            echo "panel-contrast: appearance.theme changed before the first write ('$current_theme'); keeping that and stopping" >&2
            exit 1
        elif [ "$current_status" -eq 3 ] && [ "$theme_existed" = "1" ]; then
            echo "panel-contrast: appearance.theme was removed before the first write; keeping it removed and stopping" >&2
            exit 1
        fi
        python3 -B "$repo_dir/scripts/e2e/lib/config_switch.py" --set-string "$config" appearance theme "$theme" \
            || { echo "panel-contrast: could not write appearance.theme into $config"; exit 1; }
        last_written_theme="$theme"
        wrote_theme=1
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
        # The filters row's rect comes from the app, not from a constant. The constant this replaces
        # (ROW_Y_PT=62) no longer matched the row once the search field above it moved: the capture
        # then spanned the gap between two rows and reported "a band changed, that is not text" for
        # every material, which reads as a contrast failure but is a framing one.
        row_rect="$(python3 - "$state" <<'STATE'
import json, sys
row = (json.load(open(sys.argv[1])).get("clipboard_picker") or {}).get("filter_row")
if row:
    print("{:.1f} {:.1f} {:.1f} {:.1f}".format(row["x"], row["y"], row["w"], row["h"]))
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
        if [ -n "$row_rect" ]; then
            # shellcheck disable=SC2086
            measure "filter-row" $row_rect
        else
            # The filters live in the popover, so with it closed there is no filter row on screen to
            # measure. That is an unrun check, not a contrast failure: the app now reports no rect
            # instead of the options orb (which this scenario measured for glyph ink and then reported
            # "no text" for every material). The footer captions below still cover the panel.
            echo "panel-contrast: $theme/$material/$shade: no filter row is on screen (the popover is closed); check not run"
            filter_row_not_run=$((filter_row_not_run + 1))
        fi
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
# A check that could not run is reported, not counted as either outcome (the tier rule: an unrun check is
# not a pass). The exit status stays 0 when every check that DID run held.
if [ "$filter_row_not_run" -gt 0 ]; then
    echo "panel-contrast: filter-row checks not run in $filter_row_not_run case(s): the popover was closed"
fi
exit "$fail"