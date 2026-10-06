#!/usr/bin/env python3
"""Measure one panel region of a screenshot: its surface tone and the ink on it.

Used by `scripts/e2e/panel-contrast.sh` to hold translucent panel materials to the documented contrast
tiers on *rendered pixels* (see docs/design-style, "panel tier"). A view-tree check cannot see this: the
material behind the panel is composited by the WindowServer, so only the capture has the real surface.

What it deliberately does *not* do, each because the naive version got it wrong in a way that a synthetic
image reproduces (see `--selftest`):

* It does not assume a display scale. `screencapture -R` takes *points*, the file is in *pixels*, so the
  region is converted with the scale derived from the capture itself (`w_px / panel_w_pt`). A fixed pixel
  region measures the wrong band on a 1x display.
* It does not report the region's most extreme pixels. `p01`/`p99` report the *highest*-contrast pair, so a
  dim caption is masked by a bright title and the check passes whatever the caption's own contrast is.
* It does not filter candidate inks by distance from the surface or by pixel count. Both filters hide the
  very text under test: a near-invisible caption sits within a few tone units of the surface (so a distance
  floor drops it) and a short caption has fewer pixels than a long one (so a fraction floor drops it).
* It does not treat a histogram peak as a glyph core. Anti-aliased edges form peaks too, and a peak near the
  surface reads as a failed tier for text that is in fact high contrast.

So it identifies glyph cores *spatially* (a 3x3 neighbourhood of one tone, which a 1-2px anti-aliased edge
cannot satisfy), keeps every core tone as a candidate, and rejects a candidate whose core pixels form one
component spanning the region -- that is a flat fill or a band, i.e. another surface rather than text. The
reported ratio is the *worst* candidate, which is the one the tier has to hold for. Give it one text role
per region (a label's own frame) and the number is the role's own contrast.

Usage:
    png_stats.py <png> <x_pt> <y_pt> <w_pt> <h_pt> <panel_w_pt> [label]
    png_stats.py --diff <png_a> <png_b> <x_pt> <y_pt> <w_pt> <h_pt> <panel_w_pt> [label]
    png_stats.py --selftest

Prints one line: `label surface=<tone> ink=<tone> contrast=<ratio>:1 inks=<n> scale=<s>`
Exit codes: 0 measured, 2 no ink in the region, 3 more than one ink, 4 bad input, 1 self-test failed.
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


def run_diff(path_a, path_b, x_pt, y_pt, w_pt, h_pt, panel_w_pt, label):
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
    scale = width / panel_w_pt
    if not 0.9 < scale < 2.6:
        raise SystemExit(f"png_stats: unexpected scale {scale:.2f} ({width}px / {panel_w_pt}pt)")
    x, y = round(x_pt * scale), round(y_pt * scale)
    w, h = round(w_pt * scale), round(h_pt * scale)
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


def run(path, x_pt, y_pt, w_pt, h_pt, panel_w_pt, label):
    width, height, channels, rows = decode(path)
    if panel_w_pt <= 0 or w_pt <= 0 or h_pt <= 0:
        raise SystemExit("png_stats: non-positive region")
    scale = width / panel_w_pt
    if not 0.9 < scale < 2.6:
        raise SystemExit(f"png_stats: unexpected scale {scale:.2f} ({width}px / {panel_w_pt}pt)")
    x, y = round(x_pt * scale), round(y_pt * scale)
    w, h = round(w_pt * scale), round(h_pt * scale)
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

    if failures:
        print(f"png_stats --selftest: {failures} case(s) failed")
        return 1
    print("png_stats --selftest: all cases match")
    return 0


def main() -> None:
    if len(sys.argv) > 1 and sys.argv[1] == "--diff":
        if len(sys.argv) < 9:
            raise SystemExit("usage: png_stats.py --diff <png_a> <png_b> <x_pt> <y_pt> <w_pt> <h_pt> <panel_w_pt> [label]")
        raise SystemExit(
            run_diff(
                sys.argv[2],
                sys.argv[3],
                *(float(a) for a in sys.argv[4:9]),
                sys.argv[9] if len(sys.argv) > 9 else "region",
            )
        )
    if len(sys.argv) > 1 and sys.argv[1] == "--selftest":
        raise SystemExit(selftest())
    if len(sys.argv) < 7:
        raise SystemExit("usage: png_stats.py <png> <x_pt> <y_pt> <w_pt> <h_pt> <panel_w_pt> [label]")
    run(
        sys.argv[1],
        *(float(a) for a in sys.argv[2:7]),
        sys.argv[7] if len(sys.argv) > 7 else "region",
    )


main()
