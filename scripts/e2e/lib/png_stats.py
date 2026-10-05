#!/usr/bin/env python3
"""Rendered-pixel statistics for a PNG region, standard library only.

The panel materials composite *behind* the window, so a view-tree bitmap cannot see them
(`cacheDisplay` renders the view's own drawing; the backdrop belongs to the WindowServer). Contrast for
those surfaces therefore has to be read off a real screenshot, which is what this sampler exists for.

Usage: png_stats.py FILE X Y W H
Prints one line:  mode=<0..1> p01=<0..1> p99=<0..1> contrast=<x.xx>:1 polarity=<dark-ink|light-ink>
`mode` is the surface (the histogram peak), the percentile is the ink's rendered tone in the direction the
surface implies, and `contrast` is the WCAG ratio between them.
Coordinates are top-left based, in the PNG's own pixels.
"""

import struct
import sys
import zlib


def load_png(path):
    data = open(path, "rb").read()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise SystemExit(f"{path}: not a PNG")
    pos, idat, width, height, depth, colour = 8, bytearray(), 0, 0, 0, 0
    while pos < len(data):
        (length,) = struct.unpack(">I", data[pos : pos + 4])
        kind = data[pos + 4 : pos + 8]
        body = data[pos + 8 : pos + 8 + length]
        pos += 12 + length
        if kind == b"IHDR":
            width, height, depth, colour = struct.unpack(">IIBB", body[:10])
        elif kind == b"IDAT":
            idat += body
        elif kind == b"IEND":
            break
    if depth != 8 or colour not in (2, 6):
        raise SystemExit(f"unsupported PNG: depth={depth} colour={colour} (need 8-bit RGB/RGBA)")
    channels = 3 if colour == 2 else 4
    raw = zlib.decompress(bytes(idat))
    stride = width * channels
    out = bytearray(height * stride)
    prev = bytearray(stride)
    p = 0
    for y in range(height):
        f = raw[p]
        p += 1
        line = bytearray(raw[p : p + stride])
        p += stride
        if f == 1:
            for i in range(channels, stride):
                line[i] = (line[i] + line[i - channels]) & 0xFF
        elif f == 2:
            for i in range(stride):
                line[i] = (line[i] + prev[i]) & 0xFF
        elif f == 3:
            for i in range(stride):
                a = line[i - channels] if i >= channels else 0
                line[i] = (line[i] + ((a + prev[i]) >> 1)) & 0xFF
        elif f == 4:
            for i in range(stride):
                a = line[i - channels] if i >= channels else 0
                b = prev[i]
                c = prev[i - channels] if i >= channels else 0
                pa, pb, pc = abs(b - c), abs(a - c), abs(a + b - 2 * c)
                pr = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                line[i] = (line[i] + pr) & 0xFF
        out[y * stride : (y + 1) * stride] = line
        prev = line
    return width, height, channels, out


def linear(v):
    v /= 255.0
    return v / 12.92 if v <= 0.04045 else ((v + 0.055) / 1.055) ** 2.4


def main():
    path, x, y, w, h = sys.argv[1], *(int(a) for a in sys.argv[2:6])
    # The ink's direction comes from the theme, not from the surface's own lightness: a mid-grey material
    # would otherwise be guessed as "dark ink" in dark mode, which is exactly backwards.
    polarity_dark = (sys.argv[6] if len(sys.argv) > 6 else "dark-ink") == "dark-ink"
    width, height, channels, px = load_png(path)
    hist = [0] * 64
    lums = []
    for row in range(y, min(y + h, height)):
        base = row * width * channels
        for col in range(x, min(x + w, width)):
            o = base + col * channels
            r, g, b = px[o], px[o + 1], px[o + 2]
            tone = (0.299 * r + 0.587 * g + 0.114 * b) / 255.0
            hist[min(63, int(tone * 64))] += 1
            lums.append(linear(0.299 * r + 0.587 * g + 0.114 * b))
    if not lums:
        raise SystemExit("empty region")
    lums.sort()
    mode_tone = hist.index(max(hist)) / 64.0 + 1 / 128.0
    surface = linear(mode_tone * 255)
    dark_ink = polarity_dark
    ink = lums[int(len(lums) * 0.01)] if dark_ink else lums[int(len(lums) * 0.99)]
    hi, lo = max(surface, ink), min(surface, ink)
    ratio = (hi + 0.05) / (lo + 0.05)
    print(
        f"mode={mode_tone:.3f} p01={lums[int(len(lums)*0.01)]:.3f} p99={lums[int(len(lums)*0.99)]:.3f} "
        f"contrast={ratio:.2f}:1 polarity={'dark-ink' if dark_ink else 'light-ink'}"
    )


main()
