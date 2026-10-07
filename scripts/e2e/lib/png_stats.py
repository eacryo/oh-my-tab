#!/usr/bin/env python3
"""Measure a rendered panel from a screenshot: its surface tone and ink, whether its material still resolves
the backdrop's texture, and how its edge and elevation shadow land inside the capture.

Used by `scripts/e2e/panel-contrast.sh` to hold translucent panel materials to the documented contrast tiers
on *rendered pixels* (see docs/design-style, "panel tier"), and by the outline/elevation scenarios for the two
judgements a region statistic cannot make: a 1pt outline is a band too thin to survive one, and an elevation
shadow lies outside the panel the region is expressed in. A view-tree check cannot see any of this: the
material behind the panel is composited by the WindowServer, so only the capture has the real surface.

What it deliberately does *not* do, each because the naive version got it wrong in a way that a synthetic
image reproduces (see `--selftest`):

* (all modes) It does not assume a display scale. `screencapture -R` takes *points*, the file is in *pixels*,
  so the region is converted with the scale derived from the capture itself (`w_px / panel_w_pt`). A fixed
  pixel region measures the wrong band on a 1x display.
* (all modes) It does not assume the capture's origin is the panel's origin, or that the margin around the
  panel is the same on every side. A capture the caller enlarged puts the panel's (0,0) at `left`/`top` times
  the scale, and the elevation shadow is offset downward, so the bottom margin is larger than the top: each side
  is stated on its own (`--pad-top`, `--pad-right`, `--pad-bottom`, `--pad-left`), with `--pad-pt` as the
  shorthand for all four. Once the capture is padded, the panel width alone can state neither the scale nor the
  origin, and one margin cannot state an asymmetric capture.
* (region contrast) It does not report the region's most extreme pixels. `p01`/`p99` report the
  *highest*-contrast pair, so a dim caption is masked by a bright title and the check passes whatever the
  caption's own contrast is.
* (region contrast) It does not filter candidate inks by distance from the surface or by pixel count. Both
  filters hide the very text under test: a near-invisible caption sits within a few tone units of the surface
  (so a distance floor drops it) and a short caption has fewer pixels than a long one (so a fraction floor
  drops it).
* (region contrast) It does not treat a histogram peak as a glyph core. Anti-aliased edges form peaks too, and
  a peak near the surface reads as a failed tier for text that is in fact high contrast.
* (`--hf-retention`) It does not decide from one band alone: `hf` alone reads a dark fill as a live blur, and
  `lf` alone reads every unblurred backdrop as blurred. Both ratios are *energy* ratios, so a material that
  passes `k` of the backdrop's amplitude reads `k^2` (a half-amplitude material reads 0.25, not 0.5), and a
  control band with no energy is refused as `unobservable` rather than divided by.
* (`--edge-profile`) It does not measure the outline against the deep interior. A 1pt stroke is 1-2px, so the
  instrument reports the edge band's own tone and the step across the boundary; an implementation that only
  averages the interior reports the same number with and without the outline.
* (`--edge-profile`) It does not measure the interior ring against an absolute tone. The ring is the inner
  band's deviation from the *deeper* interior, so a material whose whole tone changed is not a ring.
* (`--edge-profile`) It does not call a flat outside region a decayed shadow. On a black backdrop a black
  shadow is flat, and flat cannot be told from decayed without a stated backdrop tone: that case is
  `unobservable` with exit code 5, which a caller must read as a failure and never as a pass.

So it identifies glyph cores *spatially* (a 3x3 neighbourhood of one tone, which a 1-2px anti-aliased edge
cannot satisfy), keeps every core tone as a candidate, and rejects a candidate whose core pixels form one
component spanning the region -- that is a flat fill or a band, i.e. another surface rather than text. The
reported ratio is the *worst* candidate, which is the one the tier has to hold for. Give it one text role
per region (a label's own frame) and the number is the role's own contrast.

Each new mode's bands and limits, stated rather than implied:

* `--hf-retention` measures `hf` as the mean squared residual against a 5x5 box mean (structure below ~5px)
  and `lf` as the variance of block means of `--lf-block-px` px (8px by default). The decision floor on both
  ratios is 0.1 (amplitude 0.32 of the control). The block must sit above the material's blur radius in pixels
  and below the backdrop's coarse structure; one capture's two regions are compared on the capture's own block
  grid, so their ratio is not a grid-phase artefact. Both regions are placed with the per-side margins, and the
  control must lie inside the capture: the room available on the side it sits on is that side's margin, so a
  control in the bottom margin is checked against `--pad-bottom`, not `--pad-top`. A control that carries no
  energy in a band, or whose coarse structure is phase-locked to that grid, is `unobservable` rather than
  believed.
* `--edge-profile` reads one-point bands: [0,1)pt inside the edge (the 1pt outline), [1,6)pt inside (an inner
  ring or glow), [6,8)pt inside (the interior the ring is measured against), [1,2)pt outside (the shadow's
  tail) and the outermost point of the capture. The panel edge and the tail's own length come from the margin
  on the side being read -- `--pad-top` for a `top` cut, `--pad-bottom` for a `bottom` one, `--pad-left`/
  `--pad-right` for the row cuts -- and the opposite side's margin is what bounds the panel's thickness along
  the cut, so an asymmetric capture is readable from either edge but not from a single shorthand. The cut must
  lie on bare material across all of those bands. It
  prints `ring=present` when an inner band's deviation from the deeper interior reaches RING_MIN (8 tone
  units), averaged over a 2pt band so the surface's own per-pixel texture cannot fire it. A shadow shallower
  than 3 tone units next to the panel, an outward tail shorter than 5px, or a stated backdrop the capture does
  not sit on is `unobservable`, and that line also carries `reason=`. Without a stated tone the instrument can
  only see a tail still rising by more than 2 tone units over its outermost 4 points, so a shadow wider than
  the margin hides from it and reads `complete`.

Usage:
    png_stats.py <png> <x_pt> <y_pt> <w_pt> <h_pt> <panel_w_pt> [label]
    png_stats.py --diff <png_a> <png_b> <x_pt> <y_pt> <w_pt> <h_pt> <panel_w_pt> [label]
    png_stats.py --hf-retention <png> --inside x,y,w,h --control x,y,w,h (--scale <px/pt> | --panel-w-pt <pt>)
                 (--pad-pt <pt> | --pad-top/right/bottom/left <pt>) [--lf-block-px <px>] [--label <label>]
    png_stats.py --edge-profile <png> --axis top|bottom|left|right --offset <pt>
                 (--scale <px/pt> | --panel-w-pt <pt>) (--pad-pt <pt> | --pad-<axis> <pt>)
                 [--backdrop-tone <0..1>] [--label <label>]
    png_stats.py --selftest

The margin around the panel is stated per side: `--pad-pt` sets all four, and a per-edge option overrides it on
its own side, so `--pad-pt 32 --pad-bottom 44` is the `high` capture whose shadow is offset downward. When both
forms appear the per-edge value wins on that side; a side stated by neither is 0. A per-edge option given
*without* `--pad-pt` must come with all four, because there is then no shorthand for the sides left out and the
missing margin is unknowable -- that mix is refused (rc 4) rather than guessed. Whole-capture geometry: the
capture is `panel_w + left + right` by `panel_h + top + bottom` points, and the panel's own points are measured
from its top-left corner, so a control region in the bottom margin has `bottom` points of room and its
coordinates there are positive (past the panel's height), while one in the top or left margin is negative.

`--scale` states px/pt outright; `--panel-w-pt` derives it from the capture's *padded* width,
`w_px / (panel_w + left + right)`, where `panel_w` is the panel's own width, not the capture's -- so a padded
capture must state its margins as well or the scale is wrong. The two existing modes keep their positional
`panel_w_pt` and accept `--scale`/`--pad-*` as overrides; without them their behaviour and output are unchanged.

Prints one line per measurement. Tones are 0..1 (the unit the tier numbers use); `step` and `ring_delta` are
signed *differences* in tone units (0..255), and `hf`/`lf` are energy ratios.
Fields: `class` is the verdict, with `reason` when it is `unobservable`. `--hf-retention` adds `hf`/`lf` and
the raw `hf_inside|hf_control|lf_inside|lf_control` energies. `--edge-profile` adds `outer` (the shadow's tail
just outside the edge), `bound` (the capture's own outer ring), `edge` (the panel's first ring inside, i.e. the
outline's own tone where one is drawn, so `edge - interior` is the outline's contrast against the surface),
`interior` (the bare interior), `step` (`edge - outer`) and `ring`/`ring_delta`. `scale` is the px/pt the run
used, so a quoted number can be reproduced.
Exit codes: 0 measured, 2 no ink in the region, 3 more than one ink, 4 bad input, 5 `class=unobservable` (the
capture cannot carry a determinable judgement), 1 self-test failed.
"""

import struct
import sys
import zlib

# A candidate ink pixel: locally extreme along at least one direction, so a glyph stroke's centre qualifies
# even when the stroke is one pixel wide, while a flat fill (a keycap's interior, a band) has no extreme
# pixel at all. The margin keeps away the +-1 tone of the material's own noise; a caption at the tier floor
# is further from the surface than this.
CORE_TOLERANCE = 1
LOCAL_MARGIN = 2
MIN_SURFACE_DELTA = 2
# A candidate that owns a component spanning this fraction of the region's width or height is a flat fill or
# a band (a second surface), not text.
BAND_SPAN = 0.8
# Smallest number of core pixels a candidate needs; below this it is capture noise.
MIN_INK_PX = 8
# Differential mode: a pixel whose tone moved this much between the two captures is a glyph pixel. It has to be
# low: a caption at the 1.04:1 floor moves a pixel by only ~4 tone units, and a higher floor drops exactly the
# text the tier is about (measured: at 10 the caption vanished and the region reported its neighbour's 11:1
# instead). That is only safe because both captures come from the *same launch* -- MAX_CHANGED_FRACTION is the
# guard that refuses the other case instead of believing it.
DIFF_TONE = 3
# A changed component that spans this fraction of the region in either direction is a bulk change (a material or
# a surface), not glyphs: text is small and broken up. A plain changed-*fraction* test cannot stand in for this --
# a short label in a small region is legitimately more than a third glyph pixels (measured: 744 of 2040, refused
# for the wrong reason), while a material re-render covers the region edge to edge.
# Pixels of surrounding material used as a component's *local* surface tone.
LOCAL_WINDOW = 8
# A glyph stroke is a component of at least this many pixels (see MIN_COMPONENT_PX).
MIN_GLYPH_PX = 12
# A text stroke is a component of at least this many pixels; the material's own tonal noise comes in blobs of
# a few pixels and must not be read as an ink (measured: a dark frost surface turns dozens of them into
# "inks" at 1.03-1.13:1 otherwise).
MIN_COMPONENT_PX = 12

# --- --hf-retention: does the material still resolve the backdrop's texture? ---------------------------------
# Detail a blur destroys: the mean squared residual against a 5x5 box mean, i.e. structure below ~5px. The
# window is in capture pixels, not points -- a blur is a pixel-domain operation and the caller states the
# scale, so which backdrop detail the window resolves is the caller's to choose.
HF_WINDOW_PX = 2
# Structure a blur keeps: the variance of block means at LF_BLOCK_PX. `lf` holds up while the backdrop's
# coarse structure survives the material's blur *and* is wider than the block; a backdrop finer than the block
# has its means averaged away, so the instrument refuses instead of reporting a blurred panel as a fill, and
# `--lf-block-px` states the band the pinned backdrop actually carries. See the header.
LF_BLOCK_PX = 8
# Blocks per direction below which a block-mean variance is not an estimate of anything.
MIN_BLOCKS = 4
# The decision floor on both ratios, in units of the control's energy. The ratios are energy ratios, so a
# material passing `k` of the backdrop's amplitude reads `k^2`, and 0.1 is amplitude 0.32. Measured from the
# synthetic separation: a 9x9 box blur over the 2px/16px backdrop leaves hf 0.007 and lf 0.273, a
# half-amplitude translucent material reads 0.25 on both, and an opaque fill 0.001 and 0.000.
ENERGY_ALIVE = 0.1
# A control band below this energy (tone units squared) carries nothing to normalise against; capture
# quantisation is well under one tone unit.
MIN_CONTROL_ENERGY = 1.0

# --- --edge-profile: the outline, the interior ring, and whether the shadow fits inside the capture ----------
# Bands in points from the panel edge, converted with the stated scale: [0,1)pt inside is where a 1pt outline
# is drawn, [1,6)pt inside is where an inner ring or a glow shows up, [6,8)pt inside is the bare interior the
# ring is measured against. The cut must lie on bare material across all of them.
EDGE_BAND_PT = (0.0, 1.0)
RING_BAND_PT = (1.0, 6.0)
INTERIOR_BAND_PT = (6.0, 8.0)
# The outward tail starts this far outside the edge so a 1pt outline drawn centred on the edge cannot be read
# as shadow, and every band spans a point so single-pixel capture noise cannot move a level.
OUTER_SKIP_PT = 1.0
BAND_SPAN_PT = 1.0
# Tone thresholds, in tone units (0..255). A shadow shallower than SHADOW_MIN_DEPTH next to the panel cannot
# be told from the backdrop; a residual this small at the capture bounds counts as returned to the backdrop; a
# band deviation under RING_MIN is the interior's own texture. A ring is a band, so the scan averages over a
# RING_WINDOW_PT band: one-point steps of the raw cut read per-pixel texture as a ring (measured: the +-6
# per-pixel texture of the case below leaves that scan at +4, while the 2pt ring itself reads -26).
SHADOW_MIN_DEPTH = 3.0
DECAY_TOL = 2.0
RING_MIN = 8.0
RING_WINDOW_PT = 2.0
# The outward tail must span this many pixels for "has it decayed?" to have an answer, and the no-backdrop
# inference reads the tail's rise over the outermost SLOPE_WINDOW_PT points: a shadow that has decayed is flat
# over that window, while one wider than the capture's margin is still gaining tone (measured over a 20pt
# margin: a shadow reaching 60pt rises 3.3 units across the window, one reaching 400pt rises 1.0).
MIN_TAIL_PX = 5
SLOPE_WINDOW_PT = 4.0
# Fraction of the tail's outward steps that must rise (within DECAY_TOL) for it to read as a shadow's decay
# rather than a pattern: a decay is monotone outward, a checkerboard is not (measured: 1.00 vs 0.58).
MONOTONE_FRACTION = 0.8

# Mode flags take no value; every other `--name` is an option with one.
MODE_FLAGS = ("--selftest", "--diff", "--hf-retention", "--edge-profile")
# The margin the capture carries beyond the panel, stated per side; `--pad-pt` is the shorthand for all four.
PAD_OPTIONS = ("--pad-left", "--pad-top", "--pad-right", "--pad-bottom")
VALUE_OPTIONS = ("--scale", "--pad-pt", "--panel-w-pt", "--lf-block-px", "--inside", "--control",
                 "--axis", "--offset", "--backdrop-tone", "--label") + PAD_OPTIONS


def linear(channel: float) -> float:
    """sRGB channel (0..255) to linear light, per WCAG 2.x."""
    c = channel / 255.0
    return c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4


def relative_luminance(r: int, g: int, b: int) -> float:
    return 0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)


def contrast(a: float, b: float) -> float:
    hi, lo = max(a, b), min(a, b)
    return (hi + 0.05) / (lo + 0.05)


def decode(path: str):
    """Decode an 8-bit RGB/RGBA PNG. Returns (width, height, channels, rows)."""
    with open(path, "rb") as handle:
        blob = handle.read()
    if blob[:8] != b"\x89PNG\r\n\x1a\x0a":
        raise SystemExit("png_stats: not a PNG: " + path)
    width, height = struct.unpack(">II", blob[16:24])
    offset, idat, channels = 8, b"", 0
    while offset < len(blob):
        length = struct.unpack(">I", blob[offset : offset + 4])[0]
        kind = blob[offset + 4 : offset + 8]
        body = blob[offset + 8 : offset + 8 + length]
        if kind == b"IHDR":
            bit_depth, colour_type = body[8], body[9]
            if bit_depth != 8 or colour_type not in (2, 6):
                raise SystemExit("png_stats: only 8-bit RGB/RGBA PNGs are supported")
            channels = 3 if colour_type == 2 else 4
        elif kind == b"IDAT":
            idat += body
        elif kind == b"IEND":
            break
        offset += 12 + length
    raw = zlib.decompress(idat)
    stride = width * channels
    prior = bytearray(stride)
    rows = []
    pos = 0
    for _ in range(height):
        filter_type = raw[pos]
        line = bytearray(raw[pos + 1 : pos + 1 + stride])
        pos += 1 + stride
        for i in range(stride):
            left = line[i - channels] if i >= channels else 0
            up = prior[i]
            upleft = prior[i - channels] if i >= channels else 0
            if filter_type == 1:
                line[i] = (line[i] + left) & 0xFF
            elif filter_type == 2:
                line[i] = (line[i] + up) & 0xFF
            elif filter_type == 3:
                line[i] = (line[i] + (left + up) // 2) & 0xFF
            elif filter_type == 4:
                p = left + up - upleft
                pa, pb, pc = abs(p - left), abs(p - up), abs(p - upleft)
                pred = left if (pa <= pb and pa <= pc) else (up if pb <= pc else upleft)
                line[i] = (line[i] + pred) & 0xFF
        rows.append(bytes(line))
        prior = line
    return width, height, channels, rows


def region_tones(rows, channels, x, y, w, h):
    """The region as tone (0..255 float) and RGB triples."""
    tones = [[0.0] * w for _ in range(h)]
    rgb = [[(0, 0, 0)] * w for _ in range(h)]
    for row in range(h):
        line = rows[y + row]
        for col in range(w):
            base = (x + col) * channels
            r, g, b = line[base], line[base + 1], line[base + 2]
            tones[row][col] = 0.299 * r + 0.587 * g + 0.114 * b
            rgb[row][col] = (r, g, b)
    return tones, rgb


def pixel_tone(rows, channels, x, y):
    base = x * channels
    line = rows[y]
    return 0.299 * line[base] + 0.587 * line[base + 1] + 0.114 * line[base + 2]


def refuse(code: int, message: str):
    """Leave with a chosen code: unobservable (5) must be distinguishable from bad input (4) and no ink (2)."""
    print(message, file=sys.stderr)
    raise SystemExit(code)


def parse_options(argv):
    """Split `--flag value` / `--flag=value` options from the positional arguments.

    An unknown flag is refused rather than ignored: a mistyped `--scal` would otherwise fall back to the
    positional panel width and report a number that looks measured.
    """
    options, rest, index = {}, [], 0
    while index < len(argv):
        arg = argv[index]
        name = arg.split("=", 1)[0]
        if not arg.startswith("--") or name in MODE_FLAGS:
            rest.append(arg)
            index += 1
            continue
        if name not in VALUE_OPTIONS:
            refuse(4, f"png_stats: unknown option {name}")
        if name in options:
            refuse(4, f"png_stats: {name} given twice")
        _, separator, value = arg.partition("=")
        if not separator:
            index += 1
            if index >= len(argv):
                refuse(4, f"png_stats: {name} needs a value")
            value = argv[index]
        options[name] = value
        index += 1
    return options, rest


def number(options, name, default=None):
    if name not in options:
        return default
    try:
        return float(options[name])
    except ValueError:
        refuse(4, f"png_stats: {name} wants a number, got {options[name]!r}")


def parse_region(text, name):
    parts = text.split(",")
    if len(parts) == 4:
        try:
            return tuple(float(part) for part in parts)
        except ValueError:
            pass
    refuse(4, f"png_stats: {name} wants x,y,w,h in panel points, got {text!r}")


def region_px(x_pt, y_pt, w_pt, h_pt, scale, padding):
    """Panel points to capture pixels; `padding` is the margin beyond the panel's origin on each side."""
    return (round((padding["left"] + x_pt) * scale), round((padding["top"] + y_pt) * scale),
            round(w_pt * scale), round(h_pt * scale))


def resolve_padding(options):
    """The capture's margin outside the panel on each side, in points.

    `--pad-pt` states the same margin on all four sides and a per-edge option overrides it on its own side, so
    `--pad-pt 32 --pad-bottom 44` describes the capture whose elevation shadow is offset downward. A per-edge
    option *without* `--pad-pt` must come with all four: with no shorthand, a side left out is either 0 by
    intent or forgotten, and either guess moves the panel's origin -- and every region read from it -- by that
    side's margin, so that form is refused instead of guessed.
    """
    shorthand = number(options, "--pad-pt")
    if shorthand is not None and shorthand < 0:
        refuse(4, f"png_stats: --pad-pt must not be negative ({shorthand:g})")
    stated = {name: number(options, name) for name in PAD_OPTIONS}
    if shorthand is None and any(value is not None for value in stated.values()):
        missing = [name for name in PAD_OPTIONS if stated[name] is None]
        if missing:
            refuse(4, "png_stats: state the whole margin: --pad-pt <pt>, or all four of "
                      f"{'/'.join(PAD_OPTIONS)}; missing {', '.join(missing)}")
    padding = {}
    for name in PAD_OPTIONS:
        value = stated[name] if stated[name] is not None else (shorthand or 0.0)
        if value < 0:
            refuse(4, f"png_stats: {name} must not be negative ({value:g})")
        padding[name[len("--pad-") :]] = value
    return padding


def resolve_scale(width, options, panel_w_pt=None):
    """(pixels per point, per-side padding in points).

    `--scale` states the scale outright. Otherwise it comes from the capture's *padded* width,
    `w_px / (panel_w + left + right)`, where `panel_w` is the panel's own width: the existing modes' positional,
    or `--panel-w-pt`. The margins make up the difference between the panel and the capture, so a padded capture
    must state them or the scale is wrong; `panel_w_pt` is what keeps the existing modes' behaviour when no
    option is given.
    """
    padding = resolve_padding(options)
    stated = options.get("--scale")
    if stated is not None:
        scale = number(options, "--scale")
    else:
        panel_w = number(options, "--panel-w-pt", panel_w_pt)
        if panel_w is None or panel_w <= 0:
            refuse(4, "png_stats: state the scale: --scale <px/pt>, or --panel-w-pt <pt> "
                      "(with the margins when the capture is padded)")
        span_pt = panel_w + padding["left"] + padding["right"]
        scale = width / span_pt
    if not 0.9 < scale < 2.6:
        if stated is not None:
            refuse(4, f"png_stats: unexpected scale {scale:.2f} (--scale)")
        raise SystemExit(f"png_stats: unexpected scale {scale:.2f} ({width}px / {span_pt}pt)")
    return scale, padding


def core_mask(tones, w, h, surface_tone):
    """Locally extreme pixels: a glyph stroke's centre, but never a flat fill or an anti-aliased ramp's body."""
    mask = [[False] * w for _ in range(h)]
    for row in range(1, h - 1):
        for col in range(1, w - 1):
            value = tones[row][col]
            lo = hi = value
            for dr in (-1, 0, 1):
                for dc in (-1, 0, 1):
                    if dr == 0 and dc == 0:
                        continue
                    other = tones[row + dr][col + dc]
                    lo, hi = min(lo, other), max(hi, other)
            if abs(value - surface_tone) < MIN_SURFACE_DELTA:
                continue
            is_max = value >= hi - CORE_TOLERANCE and value > lo + LOCAL_MARGIN
            is_min = value <= lo + CORE_TOLERANCE and value < hi - LOCAL_MARGIN
            mask[row][col] = is_max or is_min
    return mask


def has_band(mask, w, h):
    """True when some 4-connected component spans most of the region: a bulk change, not glyphs."""
    seen = [[False] * w for _ in range(h)]
    for row in range(h):
        for col in range(w):
            if not mask[row][col] or seen[row][col]:
                continue
            stack = [(row, col)]
            seen[row][col] = True
            min_row = max_row = row
            min_col = max_col = col
            while stack:
                r, c = stack.pop()
                min_row, max_row = min(min_row, r), max(max_row, r)
                min_col, max_col = min(min_col, c), max(max_col, c)
                for dr, dc in ((1, 0), (-1, 0), (0, 1), (0, -1)):
                    nr, nc = r + dr, c + dc
                    if 0 <= nr < h and 0 <= nc < w and mask[nr][nc] and not seen[nr][nc]:
                        seen[nr][nc] = True
                        stack.append((nr, nc))
            if (max_col - min_col + 1) >= w * BAND_SPAN or (max_row - min_row + 1) >= h * BAND_SPAN:
                return True
    return False


def spans_region(mask, w, h):
    """True when some 4-connected component spans most of the region in one direction (a band, not text)."""
    seen = [[False] * w for _ in range(h)]
    for row in range(h):
        for col in range(w):
            if not mask[row][col] or seen[row][col]:
                continue
            stack, cells = [(row, col)], []
            seen[row][col] = True
            min_row = max_row = row
            min_col = max_col = col
            while stack:
                r, c = stack.pop()
                cells.append((r, c))
                min_row, max_row = min(min_row, r), max(max_row, r)
                min_col, max_col = min(min_col, c), max(max_col, c)
                for dr, dc in ((1, 0), (-1, 0), (0, 1), (0, -1)):
                    nr, nc = r + dr, c + dc
                    if 0 <= nr < h and 0 <= nc < w and mask[nr][nc] and not seen[nr][nc]:
                        seen[nr][nc] = True
                        stack.append((nr, nc))
            too_small = len(cells) < MIN_COMPONENT_PX
            if too_small or (max_col - min_col + 1) >= w * BAND_SPAN or (max_row - min_row + 1) >= h * BAND_SPAN:
                for r, c in cells:
                    mask[r][c] = False
    return mask


def surface_tone_of(tones, w, h):
    """The region's most common tone: for a panel region, the material behind the text."""
    counts = {}
    for row in range(h):
        for col in range(w):
            tone = int(tones[row][col] + 0.5)
            counts[tone] = counts.get(tone, 0) + 1
    return max(sorted(counts), key=lambda t: counts[t])


def measure(tones, rgb, w, h):
    """Return (surface_tone, surface_lum, [(tone, lum, px)], scale-less) for one region."""
    surface_counts = {}
    core = spans_region(core_mask(tones, w, h, surface_tone_of(tones, w, h)), w, h)
    groups = {}
    surface_tone = None
    for row in range(h):
        for col in range(w):
            tone = int(tones[row][col] + 0.5)
            surface_counts[tone] = surface_counts.get(tone, 0) + 1
            if core[row][col]:
                groups.setdefault(tone, []).append(rgb[row][col])
    surface_tone = max(sorted(surface_counts), key=lambda t: surface_counts[t])
    surface_pixels = [rgb[row][col] for row in range(h) for col in range(w) if int(tones[row][col] + 0.5) == surface_tone]
    surface_lum = sum(relative_luminance(*p) for p in surface_pixels) / len(surface_pixels)
    inks = []
    for tone, pixels in sorted(groups.items()):
        if len(pixels) < MIN_INK_PX:
            continue
        # The surface's own tone forms cores too (it is a large uniform area); it is the surface, not an ink.
        if tone == surface_tone:
            continue
        lum = sum(relative_luminance(*p) for p in pixels) / len(pixels)
        inks.append((tone, lum, len(pixels)))
    return surface_tone, surface_lum, inks


def component_cores(mask, unchanged, rgb, tones, w, h):
    """Per component, its core tone and the *local* material tone it sits on.

    The local tone is what the glyph is actually read against: a panel region covers a gradient, so a
    region-wide average compares the glyph against a tone neither it nor its surroundings have (measured: a
    local contrast of 1.17:1 was reported as 7.52:1). A rim is connected to its core, so taking the
    component's extreme ignores anti-aliasing; separate components are separate glyphs, and the *weakest* of
    their cores is what the tier has to hold for.
    """
    seen = [[False] * w for _ in range(h)]
    cores = []
    for row in range(h):
        for col in range(w):
            if not mask[row][col] or seen[row][col]:
                continue
            stack, cells = [(row, col)], []
            seen[row][col] = True
            r0 = r1 = row
            c0 = c1 = col
            while stack:
                r, c = stack.pop()
                cells.append((r, c))
                r0, r1, c0, c1 = min(r0, r), max(r1, r), min(c0, c), max(c1, c)
                for dr, dc in ((1, 0), (-1, 0), (0, 1), (0, -1)):
                    nr, nc = r + dr, c + dc
                    if 0 <= nr < h and 0 <= nc < w and mask[nr][nc] and not seen[nr][nc]:
                        seen[nr][nc] = True
                        stack.append((nr, nc))
            if len(cells) < MIN_GLYPH_PX:
                continue
            ys, ye = max(0, r0 - LOCAL_WINDOW), min(h, r1 + LOCAL_WINDOW + 1)
            xs, xe = max(0, c0 - LOCAL_WINDOW), min(w, c1 + LOCAL_WINDOW + 1)
            counts, samples = {}, {}
            for r in range(ys, ye):
                for c in range(xs, xe):
                    if not unchanged[r][c]:
                        continue
                    tone = int(tones[r][c] + 0.5)
                    counts[tone] = counts.get(tone, 0) + 1
                    samples.setdefault(tone, []).append(rgb[r][c])
            if not counts:
                continue
            local_tone = max(sorted(counts), key=lambda t: counts[t])
            pixels = samples[local_tone]
            local_lum = sum(relative_luminance(*p) for p in pixels) / len(pixels)
            best = None
            for r, c in cells:
                lum = relative_luminance(*rgb[r][c])
                gap = contrast(lum, local_lum)
                if best is None or gap > best[0]:
                    best = (gap, tones[r][c], local_tone, len(cells))
            cores.append(best)
    return cores


def run_diff(path_a, path_b, x_pt, y_pt, w_pt, h_pt, panel_w_pt, label, options=None):
    """Measure from two captures that differ only in whether the text was drawn.

    Both captures must come from the *same launch* (blank the ink partway through, or use a live toggle):
    two launches of a translucent material do not re-render identically, and the resulting jitter is the same
    size as a low-contrast ink's own contribution, which would be counted here as glyphs.
    """
    width, height, channels, rows_a = decode(path_a)
    other_w, other_h, other_c, rows_b = decode(path_b)
    if (width, height, channels) != (other_w, other_h, other_c):
        raise SystemExit(f"png_stats: the two captures differ ({width}x{height} vs {other_w}x{other_h})")
    if panel_w_pt <= 0 or w_pt <= 0 or h_pt <= 0:
        raise SystemExit("png_stats: non-positive region")
    scale, padding = resolve_scale(width, options or {}, panel_w_pt)
    x, y, w, h = region_px(x_pt, y_pt, w_pt, h_pt, scale, padding)
    if x < 0 or y < 0 or x + w > width or y + h > height:
        raise SystemExit(f"png_stats: region {x},{y},{w},{h} outside the {width}x{height} capture")
    tones_a, rgb_a = region_tones(rows_a, channels, x, y, w, h)
    tones_b, _ = region_tones(rows_b, channels, x, y, w, h)
    glyph = [[False] * w for _ in range(h)]
    unchanged = [[False] * w for _ in range(h)]
    changed = 0
    for row in range(h):
        for col in range(w):
            if abs(tones_a[row][col] - tones_b[row][col]) >= DIFF_TONE:
                glyph[row][col] = True
                changed += 1
            else:
                unchanged[row][col] = True
    if changed == 0:
        print(f"{label} local-surface=none ink=none contrast=none cores=0 changed=0 scale={scale:.2f}")
        raise SystemExit(2)
    if has_band(glyph, w, h):
        print(
            f"{label} {changed} of {w * h} px changed, in a band spanning the region: that is not text -- both "
            f"captures must come from the same launch with only the text hidden (a material or a surface that "
            f"changed covers the region; glyphs do not)"
        )
        raise SystemExit(4)
    cores = component_cores(glyph, unchanged, rgb_a, tones_a, w, h)
    if not cores:
        print(
            f"{label} local-surface=none ink=none contrast=none cores=0 changed={changed} scale={scale:.2f}"
        )
        raise SystemExit(2)
    ratio, tone, local_tone, _ = min(cores, key=lambda core: core[0])
    print(
        f"{label} local-surface={local_tone / 255:.3f} ink={tone / 255:.3f} "
        f"contrast={ratio:.2f}:1 cores={len(cores)} changed={changed} scale={scale:.2f}"
    )
    raise SystemExit(0)


def run(path, x_pt, y_pt, w_pt, h_pt, panel_w_pt, label, options=None):
    width, height, channels, rows = decode(path)
    if panel_w_pt <= 0 or w_pt <= 0 or h_pt <= 0:
        raise SystemExit("png_stats: non-positive region")
    scale, padding = resolve_scale(width, options or {}, panel_w_pt)
    x, y, w, h = region_px(x_pt, y_pt, w_pt, h_pt, scale, padding)
    if x < 0 or y < 0 or x + w > width or y + h > height:
        raise SystemExit(f"png_stats: region {x},{y},{w},{h} outside the {width}x{height} capture")
    tones, rgb = region_tones(rows, channels, x, y, w, h)
    surface_tone, surface_lum, inks = measure(tones, rgb, w, h)
    if not inks:
        print(f"{label} surface={surface_tone / 255:.3f} ink=none contrast=none inks=0 scale={scale:.2f}")
        raise SystemExit(2)
    worst = min(inks, key=lambda ink: contrast(ink[1], surface_lum))
    ratio = contrast(worst[1], surface_lum)
    print(
        f"{label} surface={surface_tone / 255:.3f} ink={worst[0] / 255:.3f} "
        f"contrast={ratio:.2f}:1 inks={len(inks)} scale={scale:.2f}"
    )
    raise SystemExit(3 if len(inks) > 1 else 0)


def high_frequency_energy(tones, w, h):
    """Mean squared residual against a 5x5 box mean: the detail a blur destroys, in tone units squared."""
    radius = HF_WINDOW_PX
    total, count = 0.0, 0
    for row in range(radius, h - radius):
        for col in range(radius, w - radius):
            window = 0.0
            for dr in range(-radius, radius + 1):
                line = tones[row + dr]
                for dc in range(-radius, radius + 1):
                    window += line[col + dc]
            residual = tones[row][col] - window / (2 * radius + 1) ** 2
            total += residual * residual
            count += 1
    return total / count if count else 0.0


def low_frequency_energy(tones, w, h, block, origin_x=0, origin_y=0):
    """Variance of block means at `block` px: the structure that survives a blur, in tone units squared.

    The grid is anchored to the capture's origin rather than each region's, so the two regions of one capture
    (`--inside`/`--control`) sample the same grid and their ratio is a measurement, not a grid-phase artefact.
    """
    means = []
    for row in range((-origin_y) % block, h - block + 1, block):
        for col in range((-origin_x) % block, w - block + 1, block):
            total = 0.0
            for r in range(row, row + block):
                total += sum(tones[r][col : col + block])
            means.append(total / block ** 2)
    if not means:
        return 0.0
    average = sum(means) / len(means)
    return sum((mean - average) ** 2 for mean in means) / len(means)


def region_energy(rows, channels, width, height, name, region, scale, padding, block):
    x, y, w, h = region_px(*region, scale=scale, padding=padding)
    if w <= 0 or h <= 0:
        refuse(4, f"png_stats: the {name} region is empty ({w}x{h}px)")
    if x < 0 or y < 0 or x + w > width or y + h > height:
        # The room a region has is the capture's real size, so a control in the bottom margin is bounded by
        # `--pad-bottom` and not by the top margin: the margins are reported to make that diagnosable.
        refuse(4, f"png_stats: the {name} region {x},{y},{w},{h} is outside the {width}x{height} capture "
                  f"(margins: left {padding['left']:g}pt, top {padding['top']:g}pt, "
                  f"right {padding['right']:g}pt, bottom {padding['bottom']:g}pt)")
    if w < MIN_BLOCKS * block or h < MIN_BLOCKS * block:
        refuse(4, f"png_stats: the {name} region is {w}x{h}px; --hf-retention needs at least "
                  f"{MIN_BLOCKS * block}px each way for {block}px block means")
    tones, _ = region_tones(rows, channels, x, y, w, h)
    return high_frequency_energy(tones, w, h), low_frequency_energy(tones, w, h, block, x, y)


def run_hf_retention(rest, options):
    usage = ("usage: png_stats.py --hf-retention <png> --inside x,y,w,h --control x,y,w,h "
             "(--scale <px/pt> | --panel-w-pt <pt>) (--pad-pt <pt> | --pad-top/right/bottom/left <pt>) "
             "[--lf-block-px <px>] [--label <label>]")
    if len(rest) < 2 or "--inside" not in options or "--control" not in options:
        refuse(4, usage)
    block = int(number(options, "--lf-block-px", LF_BLOCK_PX))
    if block < 2:
        refuse(4, f"png_stats: --lf-block-px wants at least 2px, got {block}")
    label = options.get("--label", "region")
    width, height, channels, rows = decode(rest[1])
    scale, padding = resolve_scale(width, options)
    hf_inside, lf_inside = region_energy(
        rows, channels, width, height, "inside", parse_region(options["--inside"], "--inside"),
        scale, padding, block)
    hf_control, lf_control = region_energy(
        rows, channels, width, height, "control", parse_region(options["--control"], "--control"),
        scale, padding, block)
    levels = (f"hf_inside={hf_inside:.1f} hf_control={hf_control:.1f} "
              f"lf_inside={lf_inside:.1f} lf_control={lf_control:.1f} scale={scale:.2f}")
    if hf_control < MIN_CONTROL_ENERGY or lf_control < MIN_CONTROL_ENERGY:
        print(f"{label} class=unobservable hf=none lf=none {levels} reason=no-control-texture")
        raise SystemExit(5)
    hf = hf_inside / hf_control
    lf = lf_inside / lf_control
    if hf < ENERGY_ALIVE and lf < ENERGY_ALIVE:
        verdict = "covered"
    elif hf < ENERGY_ALIVE:
        verdict = "blurred"
    elif lf < ENERGY_ALIVE:
        # Fine detail without the coarse structure the same backdrop must keep: no blur does that, so the two
        # bands disagree and neither "preserved" nor "blurred" is supportable.
        print(f"{label} class=unobservable hf={hf:.3f} lf={lf:.3f} {levels} reason=bands-disagree")
        raise SystemExit(5)
    else:
        verdict = "preserved"
    print(f"{label} class={verdict} hf={hf:.3f} lf={lf:.3f} {levels}")
    raise SystemExit(0)


def cut_tones(rows, channels, width, height, axis, offset_px, margin_px, depth_px):
    """(inside, outside) tones along one cut, each ordered by distance from the panel edge.

    `inside[0]` is the panel's outermost pixel at that edge and `outside[0]` the first pixel beyond it, so both
    are indexed by distance in pixels; `margin_px` is the margin *on that side* of the panel, which is what
    places the edge, because the capture may carry a different margin on each side.
    """
    if axis in ("top", "bottom"):
        if not 0 <= offset_px < width:
            refuse(4, f"png_stats: --offset lands at x={offset_px}px, outside the {width}px capture")
        edge, step = (margin_px, 1) if axis == "top" else (height - margin_px - 1, -1)
        inside = [pixel_tone(rows, channels, offset_px, edge + step * k) for k in range(depth_px)]
        outside = [pixel_tone(rows, channels, offset_px, edge - step * (k + 1)) for k in range(margin_px)]
    else:
        if not 0 <= offset_px < height:
            refuse(4, f"png_stats: --offset lands at y={offset_px}px, outside the {height}px capture")
        edge, step = (margin_px, 1) if axis == "left" else (width - margin_px - 1, -1)
        inside = [pixel_tone(rows, channels, edge + step * k, offset_px) for k in range(depth_px)]
        outside = [pixel_tone(rows, channels, edge - step * (k + 1), offset_px) for k in range(margin_px)]
    return inside, outside


def band_mean(values, start_pt, span_pt, scale):
    """Mean tone of the band [start_pt, start_pt + span_pt) points from the edge, or None if it is not there."""
    first = max(0, round(start_pt * scale))
    if first >= len(values):
        return None
    last = min(len(values), max(first + 1, round((start_pt + span_pt) * scale)))
    return sum(values[first:last]) / (last - first)


def smoothed(values, radius):
    return [sum(values[max(0, i - radius) : min(len(values), i + radius + 1)])
            / (min(len(values), i + radius + 1) - max(0, i - radius)) for i in range(len(values))]


def interior_ring(inside, scale, interior):
    """The worst sustained deviation of the inner bands from the deeper interior, in tone units, signed.

    A ring is a *band*, so each step averages over RING_WINDOW_PT points: single-point steps of the raw cut
    read the surface's own per-pixel texture as a ring. Measuring against the deeper interior means a material
    whose whole tone moved cannot read as one either.
    """
    first, last = RING_BAND_PT
    window = max(BAND_SPAN_PT, RING_WINDOW_PT)
    worst, depth = 0.0, first
    while depth + window <= last:
        value = band_mean(inside, depth, window, scale)
        if value is not None and abs(value - interior) > abs(worst):
            worst = value - interior
        depth += BAND_SPAN_PT
    return worst


def shadow_verdict(outside, scale, outer, bound, backdrop_tone):
    """('complete'|'clipped'|'unobservable', reason) for the shadow at the capture's window bounds."""
    if len(outside) < MIN_TAIL_PX or outer is None or bound is None:
        return "unobservable", "short-tail"
    if backdrop_tone is not None:
        if backdrop_tone - outer < SHADOW_MIN_DEPTH:
            return "unobservable", "no-shadow-contrast"
        if bound > backdrop_tone + DECAY_TOL:
            return "unobservable", "wrong-backdrop"
        return ("clipped", "") if backdrop_tone - bound > DECAY_TOL else ("complete", "")
    profile = smoothed(outside, max(1, round(0.5 * scale)))
    if max(profile) - min(profile) < DECAY_TOL:
        # A shadow that decayed and a shadow that was never visible are both flat here; without a stated
        # backdrop the instrument cannot tell them apart.
        return "unobservable", "flat-tail"
    steps = len(profile) - 1
    rising = sum(1 for i in range(steps) if profile[i + 1] >= profile[i] - DECAY_TOL)
    if rising < steps * MONOTONE_FRACTION:
        return "unobservable", "no-decay-shape"
    window = max(1, round(SLOPE_WINDOW_PT * scale))
    rise = profile[-1] - profile[max(0, len(profile) - 1 - window)]
    return ("clipped", "") if rise > DECAY_TOL else ("complete", "")


def tone_field(value):
    return "none" if value is None else f"{value / 255:.3f}"


def delta_field(value):
    return "none" if value is None else f"{value:+.1f}"


def run_edge_profile(rest, options):
    usage = ("usage: png_stats.py --edge-profile <png> --axis top|bottom|left|right --offset <pt> "
             "(--scale <px/pt> | --panel-w-pt <pt>) (--pad-pt <pt> | --pad-<axis> <pt>) "
             "[--backdrop-tone <0..1>] [--label <label>]")
    axis = options.get("--axis")
    if len(rest) < 2 or axis not in ("top", "bottom", "left", "right") or "--offset" not in options:
        refuse(4, usage)
    offset_pt = number(options, "--offset")
    backdrop_tone = number(options, "--backdrop-tone")
    if backdrop_tone is not None:
        if not 0 <= backdrop_tone <= 1:
            refuse(4, f"png_stats: --backdrop-tone is a tone 0..1, got {backdrop_tone:g}")
        backdrop_tone *= 255
    label = options.get("--label", "region")
    width, height, channels, rows = decode(rest[1])
    scale, padding = resolve_scale(width, options)
    # The edge is placed by the margin on the side being read: a column cut reads the top or the bottom margin,
    # and the opposite side's margin is what bounds the panel's thickness along the cut.
    if padding[axis] <= 0:
        refuse(4, f"png_stats: --edge-profile reads the shadow outside the panel: state --pad-{axis} <pt> "
                  f"(or --pad-pt), the margin the capture carries on the {axis} side")
    margin_px = round(padding[axis] * scale)
    # One spare pixel past the deepest interior band, so the bands themselves are always fully sampled.
    depth_px = int((INTERIOR_BAND_PT[1] + BAND_SPAN_PT) * scale) + 2
    near, far = ("top", "bottom") if axis in ("top", "bottom") else ("left", "right")
    thickness = ((height if axis in ("top", "bottom") else width)
                 - round(padding[near] * scale) - round(padding[far] * scale))
    if margin_px < 1 or thickness < depth_px:
        refuse(4, f"png_stats: a {axis} cut needs a panel at least {depth_px}px thick between the {near} and "
                  f"{far} margins; got a {thickness}px panel and {margin_px}px of margin on the {axis} side")
    # The offset runs *along* the edge, so it is taken from the origin on its own axis, not from the cut's.
    along = "left" if axis in ("top", "bottom") else "top"
    inside, outside = cut_tones(rows, channels, width, height, axis,
                                round((padding[along] + offset_pt) * scale), margin_px, depth_px)
    outer = band_mean(outside, OUTER_SKIP_PT, BAND_SPAN_PT, scale)
    ring_px = max(1, round(BAND_SPAN_PT * scale))
    bound = sum(outside[-ring_px:]) / len(outside[-ring_px:]) if outside else None
    edge = band_mean(inside, *EDGE_BAND_PT, scale)
    interior = band_mean(inside, INTERIOR_BAND_PT[0], INTERIOR_BAND_PT[1] - INTERIOR_BAND_PT[0], scale)
    if edge is None or interior is None:
        refuse(4, "png_stats: the panel edge is too close for the interior bands")
    ring = interior_ring(inside, scale, interior)
    ring_verdict = "present" if abs(ring) >= RING_MIN else "none"
    verdict, reason = shadow_verdict(outside, scale, outer, bound, backdrop_tone)
    print(f"{label} axis={axis} offset={offset_pt:g} class={verdict} outer={tone_field(outer)} "
          f"bound={tone_field(bound)} step={delta_field(None if outer is None else edge - outer)} "
          f"edge={tone_field(edge)} interior={tone_field(interior)} ring={ring_verdict} "
          f"ring_delta={ring:+.1f} scale={scale:.2f}" + (f" reason={reason}" if reason else ""))
    raise SystemExit(5 if verdict == "unobservable" else 0)


def render(path, w, h, paint):
    """Write an 8-bit RGB PNG from `paint(col, row)`, which returns a tone 0..255 or an (r, g, b) tuple."""
    pixels = []
    for row in range(h):
        for col in range(w):
            value = paint(col, row)
            if isinstance(value, tuple):
                pixels.append(list(value))
            else:
                tone = max(0, min(255, int(round(value))))
                pixels.append([tone, tone, tone])
    raw = b"".join(b"\x00" + bytes(v for p in pixels[row * w : (row + 1) * w] for v in p) for row in range(h))

    def chunk(kind, body):
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))

    with open(path, "wb") as handle:
        handle.write(
            b"\x89PNG\r\n\x1a\x0a"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw))
            + chunk(b"IEND", b"")
        )
    return w, h


# --- self-test: the synthetic images that each broken statistic got wrong ------------------------------------

def write_png(path, w, h, tone, blocks):
    pixels = [[tone] * 3 for _ in range(w * h)]
    for block_tone, x0, y0, x1, y1 in blocks:
        for row in range(y0, y1):
            for col in range(x0, x1):
                pixels[row * w + col] = [block_tone] * 3
    raw = b"".join(b"\x00" + bytes(v for p in pixels[row * w : (row + 1) * w] for v in p) for row in range(h))

    def chunk(kind, body):
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))

    with open(path, "wb") as handle:
        handle.write(
            b"\x89PNG\r\n\x1a\x0a"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw))
            + chunk(b"IEND", b"")
        )


def selftest(tmp="/tmp/png_stats_selftest") -> int:
    import os
    import subprocess

    os.makedirs(tmp, exist_ok=True)
    surface = 205
    cases = []

    # A dim caption beside a large high-contrast title: the caption must be the reported one, and a
    # pixel-count floor must not drop it.
    path = f"{tmp}/mask.png"
    write_png(path, 400, 60, surface, [(25, 0, 0, 240, 24), (174, 10, 40, 90, 52)])
    cases.append(("dim caption beside a strong title", path, 400, 60, 1.40, 2))

    # A caption a few tone units from the surface: a distance floor must not drop it.
    path = f"{tmp}/dim.png"
    write_png(path, 400, 60, surface, [(201, 10, 20, 190, 44)])
    cases.append(("caption close to the surface", path, 400, 60, 1.04, 1))

    # A black glyph core on white with a wide flat near-white band beside it (what anti-aliasing leaves
    # behind, widened): the core is the ink, the band is a second surface.
    path = f"{tmp}/aa.png"
    write_png(path, 400, 60, 255, [(0, 20, 30, 60, 50), (219, 0, 0, 400, 12)])
    cases.append(("core beside an anti-aliased band", path, 400, 60, 21.00, 1))

    failures = 0
    for name, path, w, h, want, want_inks in cases:
        proc = subprocess.run(
            [sys.executable, os.path.abspath(__file__), path, "0", "0", str(w), str(h), str(w), "selftest"],
            capture_output=True,
            text=True,
        )
        line = (proc.stdout or proc.stderr).strip()
        ratio = None
        inks = None
        for field in line.split():
            if field.startswith("contrast="):
                ratio = field.split("=", 1)[1].split(":")[0]
            elif field.startswith("inks="):
                inks = int(field[len("inks=") :])
        ok = ratio is not None and abs(float(ratio) - want) < 0.06 and inks == want_inks
        if not ok:
            failures += 1
        print(f"  [{'ok' if ok else 'FAIL'}] {name}: wanted {want:.2f}:1 with {want_inks} ink(s), got {line}")
    # The case that defeats every single-frame statistic: a noisy material with one-pixel glyph strokes. The
    # differential mode must still report the stroke's own contrast, because the pixels that moved are exactly
    # the glyph pixels. (The single-frame number is printed, not asserted: it is expected to be nonsense here,
    # and improving that path should not break this self-test.)
    def noisy(path, base, jitter, stroke):
        w, h = 400, 60
        pixels = []
        for row in range(h):
            for col in range(w):
                tone = base + ((row * 7 + col * 13) % jitter)
                pixels.append([tone, tone, tone])
        if stroke is not None:
            for col in range(20, 200, 24):
                for row in range(20, 44):
                    pixels[row * w + col] = [stroke] * 3
        raw = b"".join(
            b"\x00" + bytes(v for p in pixels[row * w : (row + 1) * w] for v in p) for row in range(h)
        )

        def chunk(kind, body):
            return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))

        with open(path, "wb") as handle:
            handle.write(
                b"\x89PNG\r\n\x1a\x0a"
                + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
                + chunk(b"IDAT", zlib.compress(raw))
                + chunk(b"IEND", b"")
            )
        return w, h

    w, h = noisy(f"{tmp}/noisy_text.png", 205, 8, 60)
    noisy(f"{tmp}/noisy_bare.png", 205, 8, None)
    # The tool reports against the *local* material tone, not the region's mean: around a stroke the local
    # modal tone is this pattern's base (205) where the region mean is 208.5, so 6.94:1 is the right answer.
    want = contrast(relative_luminance(60, 60, 60), relative_luminance(205, 205, 205))
    proc = subprocess.run(
        [
            sys.executable,
            os.path.abspath(__file__),
            "--diff",
            f"{tmp}/noisy_text.png",
            f"{tmp}/noisy_bare.png",
            "0",
            "0",
            str(w),
            str(h),
            str(w),
            "noisy",
        ],
        capture_output=True,
        text=True,
    )
    line = (proc.stdout or proc.stderr).strip()
    got = None
    for field in line.split():
        if field.startswith("contrast="):
            got = field.split("=", 1)[1].split(":")[0]
    ok = got is not None and got != "none" and abs(float(got) - want) < 0.2
    if not ok:
        failures += 1
    print(f"  [{'ok' if ok else 'FAIL'}] noisy material, 1px strokes (diff): wanted {want:.2f}:1, got {line}")
    single = subprocess.run(
        [
            sys.executable,
            os.path.abspath(__file__),
            f"{tmp}/noisy_text.png",
            "0",
            "0",
            str(w),
            str(h),
            str(w),
            "noisy",
        ],
        capture_output=True,
        text=True,
    )
    print(f"  [--] same pair, single frame (not asserted): {(single.stdout or single.stderr).strip()}")

    # The three counterexamples the differential mode has to refuse or catch. Each was a real defect of an
    # earlier version of it: a threshold high enough to survive cross-launch jitter dropped a caption four tone
    # units from the surface; a region-wide average reported 1.17:1 of local contrast as 7.5:1; and two frames
    # that differ only in the material were read as glyphs.
    def grid(path, w, h, paint):
        pixels = []
        for row in range(h):
            for col in range(w):
                tone = paint(col, row)
                pixels.append([tone, tone, tone])
        raw = b"".join(
            b"\x00" + bytes(v for p in pixels[row * w : (row + 1) * w] for v in p) for row in range(h)
        )

        def chunk(kind, body):
            return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))

        with open(path, "wb") as handle:
            handle.write(
                b"\x89PNG\r\n\x1a\x0a"
                + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
                + chunk(b"IDAT", zlib.compress(raw))
                + chunk(b"IEND", b"")
            )
        return w, h

    def diff_case(name, path_a, path_b, w, h, want, want_rc):
        proc = subprocess.run(
            [
                sys.executable,
                os.path.abspath(__file__),
                "--diff",
                path_a,
                path_b,
                "0",
                "0",
                str(w),
                str(h),
                str(w),
                "selftest",
            ],
            capture_output=True,
            text=True,
        )
        line = (proc.stdout or proc.stderr).strip()
        got = None
        for field in line.split():
            if field.startswith("contrast="):
                got = field.split("=", 1)[1].split(":")[0]
        nonlocal failures
        ok = proc.returncode == want_rc
        if ok and want is not None:
            ok = got not in (None, "none") and abs(float(got) - want) < 0.2
        if not ok:
            failures += 1
        print(f"  [{'ok' if ok else 'FAIL'}] {name}: wanted rc={want_rc} {want}, got {line}")

    surface = 205
    # A caption four tone units from the surface, beside a high-contrast title: the caption is the tier.
    grid(f"{tmp}/d_text.png", 400, 60, lambda c, r: 25 if r < 24 and c < 240 else (201 if 40 <= r < 52 and 10 <= c < 90 else surface))
    grid(f"{tmp}/d_bare.png", 400, 60, lambda c, r: 25 if r < 24 and c < 240 else surface)
    diff_case(
        "caption four tone units from the surface",
        f"{tmp}/d_text.png",
        f"{tmp}/d_bare.png",
        400,
        60,
        contrast(relative_luminance(201, 201, 201), relative_luminance(surface, surface, surface)),
        0,
    )

    # A caption in the bright end of a gradient: the region's average is far from the tone it is read on.
    grid(f"{tmp}/e_text.png", 400, 60, lambda c, r: 235 if 40 <= r < 52 and 340 <= c < 360 else 20 + c * 55 // 100)
    grid(f"{tmp}/e_bare.png", 400, 60, lambda c, r: 20 + c * 55 // 100)
    local = 20 + 350 * 55 // 100
    diff_case(
        "caption on a gradient (local, not average)",
        f"{tmp}/e_text.png",
        f"{tmp}/e_bare.png",
        400,
        60,
        contrast(relative_luminance(235, 235, 235), relative_luminance(local, local, local)),
        0,
    )

    # No text at all: the frames differ only in the material, which must be refused rather than measured.
    grid(f"{tmp}/f_a.png", 400, 60, lambda c, r: 60 + ((c // 8 + r // 8) % 2) * 40)
    grid(f"{tmp}/f_b.png", 400, 60, lambda c, r: 60 + ((c // 8 + r // 8 + 1) % 2) * 40)
    diff_case("material only, no text", f"{tmp}/f_a.png", f"{tmp}/f_b.png", 400, 60, None, 4)

    # Identical frames: nothing to measure at all.
    grid(f"{tmp}/g.png", 400, 60, lambda c, r: 200)
    diff_case("identical frames", f"{tmp}/g.png", f"{tmp}/g.png", 400, 60, None, 2)

    # --- the outline, the elevation shadow, and an interior ring (--edge-profile) ----------------------------
    # One synthetic capture per judgement, including the ones the instrument must refuse. The panel sits in the
    # middle of a capture with `pad` points of margin on every side, which is exactly what --pad-pt converts.
    def tool(*args):
        return subprocess.run(
            [sys.executable, os.path.abspath(__file__), *args], capture_output=True, text=True
        )

    def check(name, proc, want_rc, want_class=None, wants=(), want_fields=None):
        nonlocal failures
        line = (proc.stdout or proc.stderr).strip()
        got = {}
        for field in line.split():
            if "=" in field:
                key, _, value = field.partition("=")
                got[key] = value
        ok = proc.returncode == want_rc
        if ok and want_class is not None:
            ok = got.get("class") == want_class
        for key, value in (want_fields or {}).items():
            if not ok:
                break
            ok = got.get(key) == value
        for key, value, tolerance in wants:
            if not ok:
                break
            try:
                ok = abs(float(got[key]) - value) <= tolerance
            except (KeyError, ValueError):
                ok = False
        if not ok:
            failures += 1
        wanted = f"rc={want_rc}" + (f" class={want_class}" if want_class else "")
        if want_fields:
            wanted += " " + " ".join(f"{k}={v}" for k, v in want_fields.items())
        if wants:
            wanted += " " + " ".join(f"{k}={v:g}+-{t:g}" for k, v, t in wants)
        print(f"  [{'ok' if ok else 'FAIL'}] {name}: wanted {wanted}, got {line}")

    def panel_capture(path, *, pad=20, panel=(200, 80), surface=120, backdrop=235, shadow=(40.0, 12.0),
                      outline=None, ring=None, texture=0, backdrop_pattern=None,
                      pad_top=None, pad_right=None, pad_bottom=None, pad_left=None):
        """A synthetic panel capture; `shadow` is (depth in tone units, reach in points).

        `pad` is the margin on every side and `pad_<side>` overrides it there, because a real capture's margins
        differ (the elevation shadow is offset downward). A centred 1pt outline, a ring at a given inside depth,
        and the surface's own per-pixel texture are all available, because each is a judgement of its own below.
        """
        panel_w, panel_h = panel
        depth, reach = shadow
        left = pad if pad_left is None else pad_left
        top = pad if pad_top is None else pad_top
        right = pad if pad_right is None else pad_right
        bottom = pad if pad_bottom is None else pad_bottom

        def noise(col, row, amount):
            return ((col * 1103515245 + row * 12345) >> 7) % (2 * amount + 1) - amount

        def inside_depth(col, row):
            return min(col - left, row - top, left + panel_w - 1 - col, top + panel_h - 1 - row)

        def tone(col, row):
            if not (left <= col < left + panel_w and top <= row < top + panel_h):
                distance = max(top - row, row - (top + panel_h - 1), left - col, col - (left + panel_w - 1))
                value = backdrop + (backdrop_pattern(col, row) if backdrop_pattern else 0)
                if depth and reach > 0:
                    value -= depth * max(0.0, 1.0 - (distance - 0.5) / reach) ** 2
                return value
            value = surface + (noise(col, row, texture) if texture else 0)
            if outline is not None and inside_depth(col, row) < 1:
                value = outline + (noise(col, row, texture) if texture else 0)
            if ring is not None and ring[0] <= inside_depth(col, row) < ring[1]:
                value = surface + ring[2] + (noise(col, row, texture) if texture else 0)
            return value

        return render(path, left + panel_w + right, top + panel_h + bottom, tone)

    # The outline is 1pt, so a statistic measured against the deep interior cannot see it at all: the edge band
    # and the step across the boundary are what the instrument has to report. Complete shadow, backdrop stated.
    surface, backdrop, outline_tone = 120, 235, 20
    back = f"{backdrop / 255:.6f}"
    edge_cut = ["--axis", "top", "--offset", "100", "--scale", "1", "--pad-pt", "20", "--label", "edge"]
    panel_capture(f"{tmp}/edge_complete.png", outline=outline_tone)
    check("outline present, shadow complete",
          tool("--edge-profile", f"{tmp}/edge_complete.png", *edge_cut, "--backdrop-tone", back),
          0, "complete",
          [("edge", outline_tone / 255, 0.01), ("interior", surface / 255, 0.01),
           ("bound", backdrop / 255, 0.01), ("step", -180.0, 8.0), ("ring_delta", 0.0, 2.0)],
          {"ring": "none"})

    # The same capture with the panel's own width and the margin instead of a stated scale: a padded capture
    # cannot state its scale from the image width alone, and this is the form a scenario has (it knows the
    # frame it captured and the margin it added), so it has to reach the same numbers.
    check("scale stated as the padded extent",
          tool("--edge-profile", f"{tmp}/edge_complete.png", "--axis", "top", "--offset", "100",
               "--panel-w-pt", "200", "--pad-pt", "20", "--backdrop-tone", back, "--label", "extent"),
          0, "complete",
          [("edge", outline_tone / 255, 0.01), ("interior", surface / 255, 0.01),
           ("bound", backdrop / 255, 0.01), ("step", -184.0, 4.0)])

    # Every axis reads the same capture the same way: the shadow is the same on each side, so a wrong outward
    # direction shows up as a different edge/interior/bound here.
    for cut_axis, cut_offset in (("top", 100), ("bottom", 100), ("left", 40), ("right", 40)):
        check(f"{cut_axis} cut reads the same panel",
              tool("--edge-profile", f"{tmp}/edge_complete.png", "--axis", cut_axis, "--offset", str(cut_offset),
                   "--scale", "1", "--pad-pt", "20", "--backdrop-tone", back, "--label", cut_axis),
              0, "complete",
              [("edge", outline_tone / 255, 0.01), ("interior", surface / 255, 0.01),
               ("bound", backdrop / 255, 0.01)])

    # The real capture is padded asymmetrically -- the `high` elevation shadow is offset downward, 32pt
    # top/left/right against 44pt bottom -- so the panel edge and the bands around it are only in the right
    # place when the margin is stated per side. The *bottom* cut is the discriminating one: a top cut does not
    # see the bottom margin at all, so an implementation that forces one --pad-pt can still pass that.
    asym = dict(panel=(200, 80), pad_top=32, pad_right=32, pad_bottom=44, pad_left=32)
    asym_args = ["--pad-top", "32", "--pad-right", "32", "--pad-bottom", "44", "--pad-left", "32"]
    asym_bottom = ["--axis", "bottom", "--offset", "100", "--scale", "1", "--label", "asym"]
    panel_capture(f"{tmp}/edge_asym.png", outline=outline_tone, **asym)
    check("margin stated per side places the bottom edge",
          tool("--edge-profile", f"{tmp}/edge_asym.png", *asym_bottom, *asym_args, "--backdrop-tone", back),
          0, "complete",
          [("edge", outline_tone / 255, 0.01), ("interior", surface / 255, 0.01),
           ("bound", backdrop / 255, 0.01), ("step", -184.0, 8.0)])
    # The shorthand plus the one side that differs is the same statement and reaches the same numbers, so a
    # caller who knows the common margin does not have to repeat it three times.
    check("margin stated as the shorthand plus the differing side",
          tool("--edge-profile", f"{tmp}/edge_asym.png", *asym_bottom, "--pad-pt", "32", "--pad-bottom", "44",
               "--backdrop-tone", back),
          0, "complete",
          [("edge", outline_tone / 255, 0.01), ("interior", surface / 255, 0.01), ("step", -184.0, 8.0)])
    # The shorthand alone puts the bottom edge 12pt too low, inside the shadow, where the tail has already
    # decayed: the cut then has no shadow next to its "edge" and the instrument refuses instead of reporting
    # the backdrop as an interior. (A wrong margin is not detectable in general -- the instrument trusts the
    # caller's statement -- but this capture makes the mistake visible, which is what the case pins.)
    check("symmetric shorthand cannot describe the asymmetric capture",
          tool("--edge-profile", f"{tmp}/edge_asym.png", *asym_bottom, "--pad-pt", "32",
               "--backdrop-tone", back),
          5, "unobservable", [], {"reason": "no-shadow-contrast"})
    # A per-edge margin with no shorthand leaves the other three sides unknowable, so it is refused rather than
    # read as 0: either reading moves the panel's origin by a margin the caller never stated.
    check("per-edge margin without the shorthand",
          tool("--edge-profile", f"{tmp}/edge_asym.png", *asym_bottom, "--pad-bottom", "44"), 4, None)
    # The top cut of the same capture: placed by --pad-top, which here is what the shorthand says too.
    check("the top cut is placed by --pad-top",
          tool("--edge-profile", f"{tmp}/edge_asym.png", "--axis", "top", "--offset", "100", "--scale", "1",
               "--pad-pt", "32", "--backdrop-tone", back, "--label", "asym-top"),
          0, "complete", [("edge", outline_tone / 255, 0.01), ("interior", surface / 255, 0.01)])

    # The shorthand is exactly the four sides stated at once: same capture, same command otherwise, and the
    # whole line -- numbers and verdict -- has to be identical, because the shorthand is the symmetric special
    # case of the per-side form, not a second code path with its own rounding.
    shorthand_run = tool("--edge-profile", f"{tmp}/edge_complete.png", *edge_cut)
    explicit_run = tool("--edge-profile", f"{tmp}/edge_complete.png", "--axis", "top", "--offset", "100",
                        "--scale", "1", "--pad-top", "20", "--pad-right", "20", "--pad-bottom", "20",
                        "--pad-left", "20", "--label", "edge")
    same = ((shorthand_run.returncode, shorthand_run.stdout, shorthand_run.stderr)
            == (explicit_run.returncode, explicit_run.stdout, explicit_run.stderr))
    if not same:
        failures += 1
    print(f"  [{'ok' if same else 'FAIL'}] --pad-pt is the four sides stated at once: "
          + (repr(shorthand_run.stdout.strip()) if same
             else f"{shorthand_run.stdout.strip()!r} vs {explicit_run.stdout.strip()!r}"))

    # The same capture without the outline: the edge band is the surface, so the step loses the outline's own
    # contribution. An implementation that reports only the interior cannot tell these two apart.
    panel_capture(f"{tmp}/edge_bare.png")
    check("outline absent",
          tool("--edge-profile", f"{tmp}/edge_bare.png", *edge_cut, "--backdrop-tone", back),
          0, "complete",
          [("edge", surface / 255, 0.01), ("interior", surface / 255, 0.01), ("step", -80.0, 8.0)])

    # A shadow whose reach far exceeds the capture's margin is still darker than the backdrop at the bounds.
    panel_capture(f"{tmp}/edge_clipped.png", outline=outline_tone, shadow=(40.0, 60.0))
    check("shadow clipped, backdrop stated",
          tool("--edge-profile", f"{tmp}/edge_clipped.png", *edge_cut, "--backdrop-tone", back),
          0, "clipped", [("bound", 216.8 / 255, 0.03)])
    # Without the stated tone the instrument has only the tail's own shape, and this one is still rising over
    # its outermost 4 points -- the inference catches that without a reference.
    check("shadow clipped, no stated backdrop",
          tool("--edge-profile", f"{tmp}/edge_clipped.png", *edge_cut), 0, "clipped")

    # A shadow so wide that the whole margin is nearly flat, yet still far darker than the backdrop (60 tone
    # units deep at the panel, 54 at the bounds). The inferred path cannot see a residual this shallow -- its
    # stated limit, asserted here so it stays stated -- while the stated backdrop makes the judgement absolute.
    # A slope-only implementation passes the first and fails the second.
    panel_capture(f"{tmp}/edge_saturating.png", outline=outline_tone, shadow=(60.0, 400.0))
    check("saturating shadow, no stated backdrop (the documented limit)",
          tool("--edge-profile", f"{tmp}/edge_saturating.png", *edge_cut), 0, "complete")
    check("saturating shadow, backdrop stated",
          tool("--edge-profile", f"{tmp}/edge_saturating.png", *edge_cut, "--backdrop-tone", back),
          0, "clipped")

    # A black shadow on a pure-black backdrop: the outside is flat, and flat cannot be told from decayed. The
    # verdict must be unobservable with the distinct code -- never "complete", which would hand a scenario a
    # pass for a shadow it could not see.
    panel_capture(f"{tmp}/edge_black.png", surface=30, backdrop=0, outline=None, shadow=(40.0, 20.0))
    check("black shadow on a black backdrop, backdrop stated",
          tool("--edge-profile", f"{tmp}/edge_black.png", *edge_cut, "--backdrop-tone", "0"), 5, "unobservable")
    check("black shadow on a black backdrop, no stated backdrop",
          tool("--edge-profile", f"{tmp}/edge_black.png", *edge_cut), 5, "unobservable")

    # An interior ring has to be found on a textured surface, and only a *sustained* band is a ring: per-pixel
    # texture at +-6 tone units must not fire it, while a 2pt band 30 units darker must.
    panel_capture(f"{tmp}/edge_texture.png", outline=outline_tone, texture=6)
    check("interior clean on a textured surface",
          tool("--edge-profile", f"{tmp}/edge_texture.png", *edge_cut, "--backdrop-tone", back),
          0, "complete", [("interior", surface / 255, 0.02), ("ring_delta", 0.0, 4.0)], {"ring": "none"})
    panel_capture(f"{tmp}/edge_ring.png", outline=outline_tone, texture=6, ring=(2.0, 4.0, -30.0))
    check("interior darkened by a ring on a textured surface",
          tool("--edge-profile", f"{tmp}/edge_ring.png", *edge_cut, "--backdrop-tone", back),
          0, "complete", [("ring_delta", -30.0, 6.0)], {"ring": "present"})
    # Only the material tone changed: the inner bands move with the interior instead of against it. A ring
    # detector that compares the inner band against an absolute tone reports a ring here.
    panel_capture(f"{tmp}/edge_material.png", outline=outline_tone, surface=145, texture=6)
    check("material tone changed, no ring",
          tool("--edge-profile", f"{tmp}/edge_material.png", *edge_cut, "--backdrop-tone", back),
          0, "complete", [("interior", 145 / 255, 0.02), ("ring_delta", 0.0, 4.0)], {"ring": "none"})

    # A margin too thin to show a decay: the shadow below is complete, but the verdict must be refused rather
    # than assumed, and a capture with no margin outside the panel is bad input rather than a verdict.
    panel_capture(f"{tmp}/edge_tight.png", pad=2, panel=(60, 40), outline=outline_tone)
    check("margin too thin to show a decay",
          tool("--edge-profile", f"{tmp}/edge_tight.png", "--axis", "top", "--offset", "30", "--scale", "1",
               "--pad-pt", "2", "--backdrop-tone", back), 5, "unobservable")
    check("no margin to read",
          tool("--edge-profile", f"{tmp}/edge_complete.png", "--axis", "top", "--offset", "100", "--scale", "1"),
          4, None)

    # A stated backdrop the capture does not sit on (its bounds are already lighter than the tone it claims)
    # makes the absolute judgement meaningless, so it is refused too rather than read as "decayed".
    check("stated backdrop lighter than the capture's bounds",
          tool("--edge-profile", f"{tmp}/edge_complete.png", *edge_cut, "--backdrop-tone", "0.85"),
          5, "unobservable", [], {"reason": "wrong-backdrop"})

    # A patterned backdrop has no single backdrop tone: the inferred path must refuse instead of reading the
    # pattern's lightest square as "the shadow decayed here".
    panel_capture(f"{tmp}/edge_pattern.png", outline=outline_tone,
                  backdrop_pattern=lambda c, r: 10 if (c + r) % 2 else -10)
    check("patterned backdrop, no stated tone",
          tool("--edge-profile", f"{tmp}/edge_pattern.png", *edge_cut), 5, "unobservable")

    # --- does the material still resolve the backdrop's texture? (--hf-retention) ---------------------------
    # The pinned backdrop carries both bands: 2px detail (what a blur destroys) and 16px blocks (what it
    # keeps). One capture holds both regions: the control sits in the margin, the measured region in the panel.
    def hf_pattern(col, row, phase=0):
        fine = 20 if (col + row + phase) % 2 == 0 else -20
        coarse = 30 if (((col + phase) % 32) // 16 + ((row + phase) % 32) // 16) % 2 == 0 else -30
        return 128 + fine + coarse

    def blurred(source, radius):
        cache = {}

        def tone(col, row):
            key = (col, row)
            if key not in cache:
                total = 0.0
                for dy in range(-radius, radius + 1):
                    for dx in range(-radius, radius + 1):
                        total += source(col + dx, row + dy)
                cache[key] = total / (2 * radius + 1) ** 2
            return cache[key]
        return tone

    def hf_capture(name, material, backdrop=hf_pattern, pad=60, panel=(200, 120),
                   pad_top=None, pad_right=None, pad_bottom=None, pad_left=None):
        panel_w, panel_h = panel
        left = pad if pad_left is None else pad_left
        top = pad if pad_top is None else pad_top
        right = pad if pad_right is None else pad_right
        bottom = pad if pad_bottom is None else pad_bottom

        def tone(col, row):
            if left <= col < left + panel_w and top <= row < top + panel_h:
                return material(col, row)
            return backdrop(col, row)

        return render(f"{tmp}/{name}.png", left + panel_w + right, top + panel_h + bottom, tone)

    hf_regions = ["--inside", "10,10,48,48", "--control", "-56,-56,48,48", "--scale", "1", "--pad-pt", "60",
                  "--label", "hf"]

    hf_capture("hf_sharp", hf_pattern)
    check("texture preserved (unblurred backdrop)",
          tool("--hf-retention", f"{tmp}/hf_sharp.png", *hf_regions), 0, "preserved",
          [("hf", 1.0, 0.2), ("lf", 1.0, 0.25)])

    # A translucent material halves the backdrop's amplitude, so both energies read k^2 = 0.25: the texture is
    # still *there*, and a floor of "hf under 0.5 is a blur" calls this covered or blurred.
    hf_capture("hf_dim", lambda c, r: 128 + 0.5 * (hf_pattern(c, r) - 128))
    check("texture preserved through a half-amplitude material",
          tool("--hf-retention", f"{tmp}/hf_dim.png", *hf_regions), 0, "preserved",
          [("hf", 0.25, 0.06), ("lf", 0.25, 0.10)])

    # A 9x9 box blur: the 2px detail is gone, the 16px block means are not. Both single-band statistics fail
    # here -- hf alone calls the opaque fill below "blurred" too, and lf alone calls the sharp case "blurred".
    hf_capture("hf_blurred", blurred(hf_pattern, 4))
    check("blur alive",
          tool("--hf-retention", f"{tmp}/hf_blurred.png", *hf_regions), 0, "blurred",
          [("hf", 0.0, 0.03), ("lf", 0.5, 0.35)])

    # An opaque fill: no detail and no block structure either, so it is the one case where both ratios die.
    hf_capture("hf_covered", lambda c, r: 128 + ((c * 7919 + r * 104729) % 3 - 1))
    check("texture covered by an opaque fill",
          tool("--hf-retention", f"{tmp}/hf_covered.png", *hf_regions), 0, "covered",
          [("hf", 0.0, 0.02), ("lf", 0.0, 0.02)])

    # A control with no texture at all, e.g. a scenario that pinned a plain backdrop: both ratios would be
    # 0/0, so the instrument refuses instead of reporting them.
    hf_capture("hf_flat", hf_pattern, backdrop=lambda c, r: 128)
    check("control with no texture",
          tool("--hf-retention", f"{tmp}/hf_flat.png", *hf_regions), 5, "unobservable")

    # Fine detail without the coarse structure the same backdrop must keep: no blur does that, so the two
    # bands disagree and neither verdict is supportable.
    hf_capture("hf_fine", lambda c, r: 128 + (20 if (c + r) % 2 == 0 else -20))
    check("bands disagree",
          tool("--hf-retention", f"{tmp}/hf_fine.png", *hf_regions), 5, "unobservable")

    # The regions are placed with the per-side margins: the origin is `left`/`top`, and a control in the bottom
    # margin has `bottom` points of room, not `top`. Here the margins are 24/32/56/44 and the capture is
    # 280x196, so the control at panel y=122 (px 154..193) only fits inside a capture with 44pt below the panel
    # -- and only the per-side form can state that. The derived scale also uses the padded width
    # (200 + 24 + 56), which is what makes `--panel-w-pt` agree with `--scale 1`.
    asym_hf = dict(panel=(200, 120), pad_top=32, pad_right=56, pad_bottom=44, pad_left=24)
    asym_hf_args = ["--panel-w-pt", "200", "--pad-top", "32", "--pad-right", "56",
                    "--pad-bottom", "44", "--pad-left", "24"]
    hf_capture("hf_asym", blurred(hf_pattern, 4), **asym_hf)
    check("control in the larger bottom margin, scale from the padded extent",
          tool("--hf-retention", f"{tmp}/hf_asym.png", "--inside", "20,20,48,48",
               "--control", "60,122,48,40", *asym_hf_args, "--label", "hf-asym"),
          0, "blurred", [("hf", 0.0, 0.03), ("lf", 0.5, 0.35)], {"scale": "1.00"})
    # The same request against a margin the capture does not have: the control runs past the capture's bottom,
    # which is bad input rather than a measurement of something else.
    check("control past the bottom margin",
          tool("--hf-retention", f"{tmp}/hf_asym.png", "--inside", "20,20,48,48",
               "--control", "60,166,48,48", *asym_hf_args, "--label", "hf-over"),
          4, None)

    # A backdrop whose coarse period equals the analysis block and sits anti-phase with the capture's grid:
    # every block mean is the average of both tones, so lf reads zero and is refused rather than believed.
    hf_capture("hf_phase", hf_pattern,
               backdrop=lambda c, r: 128 + (20 if (c + r) % 2 == 0 else -20)
               + (30 if ((c + 4) // 8 + (r + 4) // 8) % 2 == 0 else -30))
    check("control coarse structure phase-locked to the block grid",
          tool("--hf-retention", f"{tmp}/hf_phase.png", *hf_regions), 5, "unobservable")

    # The block is the analysis band, so a backdrop whose coarse structure is *finer* than the default block
    # needs the caller to state it: at 8px the block means average the 4px structure away and the instrument
    # refuses, at 4px the same capture is measured. (`--lf-block-px` exists for exactly this, and for a
    # material that blurs wider than the default block.)
    def fine_grid(col, row):
        fine = 20 if (col + row) % 2 == 0 else -20
        coarse = 30 if ((col % 8) // 4 + (row % 8) // 4) % 2 == 0 else -30
        return 128 + fine + coarse

    hf_capture("hf_fine_grid", fine_grid, backdrop=fine_grid)
    check("finer backdrop than the default block",
          tool("--hf-retention", f"{tmp}/hf_fine_grid.png", *hf_regions), 5, "unobservable")
    check("finer backdrop with its own block stated",
          tool("--hf-retention", f"{tmp}/hf_fine_grid.png", *hf_regions, "--lf-block-px", "4"),
          0, "preserved", [("hf", 1.0, 0.1), ("lf", 1.0, 0.1)])

    if failures:
        print(f"png_stats --selftest: {failures} case(s) failed")
        return 1
    print("png_stats --selftest: all cases match")
    return 0


def main() -> None:
    options, rest = parse_options(sys.argv[1:])
    if rest[:1] == ["--selftest"]:
        raise SystemExit(selftest())
    if rest[:1] == ["--diff"]:
        if len(rest) < 9:
            raise SystemExit("usage: png_stats.py --diff <png_a> <png_b> <x_pt> <y_pt> <w_pt> <h_pt> <panel_w_pt> [label]")
        raise SystemExit(
            run_diff(
                rest[1],
                rest[2],
                *(float(a) for a in rest[3:8]),
                rest[8] if len(rest) > 8 else "region",
                options,
            )
        )
    if rest[:1] == ["--hf-retention"]:
        run_hf_retention(rest, options)
    if rest[:1] == ["--edge-profile"]:
        run_edge_profile(rest, options)
    if len(rest) < 6:
        raise SystemExit("usage: png_stats.py <png> <x_pt> <y_pt> <w_pt> <h_pt> <panel_w_pt> [label]")
    run(
        rest[0],
        *(float(a) for a in rest[1:6]),
        rest[6] if len(rest) > 6 else "region",
        options,
    )


main()
