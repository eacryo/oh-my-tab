#!/bin/bash
# 从 1024x1024 PNG 生成 macOS 应用图标 AppIcon.icns。
# 管线:sips 缩放 10 种 .iconset 尺寸 → iconutil 打包 .icns。
# 产物 assets/AppIcon.icns 提交进 git;bundle.sh 打包时直接拷入 Contents/Resources/。
# 改图标流程:替换 assets/Icon-Default-1024x1024@1x.png → 跑本脚本 → 提交新的 AppIcon.icns。
# 前置:iconutil(Xcode CLT)。
#
# Generate the macOS app icon AppIcon.icns from a 1024x1024 PNG.
# Pipeline: sips resize to 10 .iconset sizes → iconutil pack .icns.
# Output assets/AppIcon.icns is committed; bundle.sh copies it into Contents/Resources/.
# To change the icon: replace assets/Icon-Default-1024x1024@1x.png → run this script → commit.
# Requires: iconutil (Xcode CLT).
set -e

# 失败清理 + 提示 / failure cleanup + message
WORK=""
cleanup() {
  local code=$?
  [ -n "$WORK" ] && rm -rf "$WORK"
  if [ "$code" -ne 0 ]; then
    echo "❌ Build failed (exit $code)" >&2
  fi
}
trap cleanup EXIT

cd "$(dirname "$0")/.."

SRC="assets/Icon-Default-1024x1024@1x.png"
OUT_DIR="assets"
OUT="$OUT_DIR/AppIcon.icns"

command -v iconutil >/dev/null || { echo "error: iconutil not found (install Xcode Command Line Tools)" >&2; exit 1; }
[ -f "$SRC" ] || { echo "error: $SRC not found" >&2; exit 1; }

WORK="$(mktemp -d)"
ICONSET="$WORK/AppIcon.iconset"
mkdir -p "$ICONSET"

# 1. 源 PNG → 10 种 .iconset 尺寸（sips 缩放，保留透明通道）。
# 1. Source PNG → 10 .iconset sizes (sips resize, alpha preserved).
echo "→ Resizing $SRC to 10 iconset sizes..."
sips -z 16 16   "$SRC" --out "$ICONSET/icon_16x16.png"          >/dev/null || { echo "error: resize 16x16 failed" >&2; exit 1; }
sips -z 32 32   "$SRC" --out "$ICONSET/icon_16x16@2x.png"       >/dev/null || { echo "error: resize 16x16@2x failed" >&2; exit 1; }
sips -z 32 32   "$SRC" --out "$ICONSET/icon_32x32.png"          >/dev/null || { echo "error: resize 32x32 failed" >&2; exit 1; }
sips -z 64 64   "$SRC" --out "$ICONSET/icon_32x32@2x.png"       >/dev/null || { echo "error: resize 32x32@2x failed" >&2; exit 1; }
sips -z 128 128 "$SRC" --out "$ICONSET/icon_128x128.png"        >/dev/null || { echo "error: resize 128x128 failed" >&2; exit 1; }
sips -z 256 256 "$SRC" --out "$ICONSET/icon_128x128@2x.png"     >/dev/null || { echo "error: resize 128x128@2x failed" >&2; exit 1; }
sips -z 256 256 "$SRC" --out "$ICONSET/icon_256x256.png"        >/dev/null || { echo "error: resize 256x256 failed" >&2; exit 1; }
sips -z 512 512 "$SRC" --out "$ICONSET/icon_256x256@2x.png"     >/dev/null || { echo "error: resize 256x256@2x failed" >&2; exit 1; }
sips -z 512 512 "$SRC" --out "$ICONSET/icon_512x512.png"        >/dev/null || { echo "error: resize 512x512 failed" >&2; exit 1; }
cp "$SRC" "$ICONSET/icon_512x512@2x.png"                         # 源文件已是 1024x1024

# 2. .iconset → .icns。
# 2. .iconset → .icns.
echo "→ Packing .icns with iconutil..."
mkdir -p "$OUT_DIR"
iconutil -c icns "$ICONSET" -o "$OUT" || { echo "error: iconutil failed" >&2; exit 1; }

echo "✅ Generated $OUT"
