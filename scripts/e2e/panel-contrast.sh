#!/bin/bash
# E2E_CHANGES_PREFS=1
# A2: rendered-pixel contrast for the translucent panel materials.
#
# Why a screenshot: the panel material composites *behind* the window, so a view-tree bitmap cannot contain
# it. `--e2e-state` reports the picker's frame (`clipboard_picker`), which is what makes a screenshot
# measurable without guessing where the panel is.
#
# Measured region: the clipboard picker's filter row (全部 / 文本 / 图片 ...). It is bare material with
# caption-sized labels on it, and it holds no thumbnails, so its darkest/lightest percentile is ink rather
# than a preview image. The tier asserted is the panel caption floor from docs/design-style: 3:1.
#
# Usage: scripts/e2e/panel-contrast.sh [material ...]     (defaults to frost liquid-glass)
set -euo pipefail
repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_dir"
materials=("$@")
[ ${#materials[@]} -eq 0 ] && materials=(frost liquid-glass)
config="$HOME/.config/oh-my-tab/config.toml"
backup="$(mktemp)"
cp "$config" "$backup"
restore() { cp "$backup" "$config"; rm -f "$backup"; }
trap restore EXIT
state="$(mktemp)"
fail=0
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
            --panel-material="$material" --panel-backdrop="$shade" >/dev/null 2>&1
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
        if [ "$pw" -le 0 ]; then
            echo "panel-contrast: $theme/$material: the picker never reported a visible frame"
            fail=1
            continue
        fi
        shot="$(mktemp -t panel-contrast).png"
        screencapture -x -R "${px},${py},${pw},${ph}" "$shot"
        printf "%-6s %-13s %-6s " "$theme" "$material" "$shade"
        ink_dir="dark-ink"; [ "$theme" = "dark" ] && ink_dir="light-ink"
        stats="$(python3 scripts/e2e/lib/png_stats.py "$shot" 40 "${ROW_Y:-124}" 900 60 "$ink_dir")"
        echo "$stats"
        rm -f "$shot"
        # The panel caption tier: text drawn on the material must hold 3:1 (docs/design-style, "panel
        # tier"). A material that cannot is not shippable as-is, so this fails the run rather than warning.
        ratio="${stats#*contrast=}"
        ratio="${ratio%%:*}"
        if ! python3 -c "import sys; sys.exit(0 if float('$ratio') >= 3.0 else 1)"; then
            echo "panel-contrast: $theme/$material on a $shade backdrop measured ${ratio}:1, below the 3:1 panel tier"
            fail=1
        fi
        done
    done
done
pkill -f "Oh-My-Tab-Dev.app" 2>/dev/null || true
exit "$fail"
