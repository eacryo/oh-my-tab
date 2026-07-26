#!/bin/sh
# 从 icon.svg 生成 macOS 应用图标 AppIcon.icns。
# 管线:SVG -> 10 张标准 .iconset PNG(tools/svg2png.swift,NSImage/WebKit,保留 alpha) -> .icns(iconutil)。
# 产物 assets/AppIcon.icns 提交进 git;bundle.sh 打包时直接拷入 Contents/Resources/,因此贡献者无需重新生成。
# 改图标流程:编辑 icon.svg -> 跑本脚本 -> 把新生成的 assets/AppIcon.icns 一起提交。
# 前置:swift(Xcode 或 Swift 工具链)+ iconutil(Xcode CLT)。
#
# Generate the macOS app icon AppIcon.icns from icon.svg.
# Pipeline: SVG -> 10 standard .iconset PNGs (tools/svg2png.swift, NSImage/WebKit, alpha preserved) -> .icns (iconutil).
# Output assets/AppIcon.icns is committed; bundle.sh copies it into Contents/Resources/, so contributors need not regenerate.
# To change the icon: edit icon.svg -> run this script -> commit the regenerated assets/AppIcon.icns.
# Requires: swift (Xcode or the Swift toolchain) + iconutil (Xcode CLT).
set -e

cd "$(dirname "$0")"

SRC="icon.svg"
OUT_DIR="assets"
OUT="$OUT_DIR/AppIcon.icns"

command -v swift    >/dev/null || { echo "error: swift not found (install Xcode or the Swift toolchain)" >&2; exit 1; }
command -v iconutil >/dev/null || { echo "error: iconutil not found (install Xcode Command Line Tools)" >&2; exit 1; }
[ -f "$SRC" ] || { echo "error: $SRC not found" >&2; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
ICONSET="$WORK/AppIcon.iconset"

# 1. SVG -> 10 张 .iconset PNG(各尺寸原生光栅化,保留透明圆角)。
# 1. SVG -> 10 .iconset PNGs (native rasterization per size, transparent corners preserved).
swift tools/svg2png.swift "$SRC" "$ICONSET" >/dev/null

# 2. .iconset -> .icns。
# 2. .iconset -> .icns.
mkdir -p "$OUT_DIR"
iconutil -c icns "$ICONSET" -o "$OUT"

echo "Generated $OUT"
