#!/bin/sh
# 打包 release .app:编译 -> 组装 bundle -> ad-hoc 签名。
# 产物输出到 dist/(已 gitignore),放在 target/ 之外以保持 logger 的 is_dev=false(走文件日志)。
#
# Bundle the release .app: build -> assemble bundle -> ad-hoc sign.
# Output goes to dist/ (gitignored), outside target/ so the logger's is_dev stays false (file logging).
set -e

# 脚本在 scripts/ 下,先切到仓库根再引用相对路径。
# Script lives in scripts/; cd to the repo root before using relative paths.
cd "$(dirname "$0")/.."

APP="dist/oh-my-tab.app"

cargo build --release
BIN="target/release/oh-my-tab"

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp "$BIN" "$APP/Contents/MacOS/oh-my-tab"
cp assets/Info.plist "$APP/Contents/Info.plist"

# 应用图标:从 assets/AppIcon.icns 拷入 Contents/Resources/(放在 codesign 之前,纳入签名)。
# 该 icns 由 build-icon.sh 从 assets/icon.svg 生成并提交进 git;缺失则提示先跑 build-icon.sh。
# App icon: copy assets/AppIcon.icns into Contents/Resources/ (before codesign so it is covered by the signature).
# The icns is generated from assets/icon.svg by build-icon.sh and committed; if missing, hint to run build-icon.sh first.
ICON="assets/AppIcon.icns"
if [ ! -f "$ICON" ]; then
  echo "error: $ICON not found. Run ./scripts/build-icon.sh first." >&2
  exit 1
fi
mkdir -p "$APP/Contents/Resources"
cp "$ICON" "$APP/Contents/Resources/AppIcon.icns"

# ad-hoc 签名:本地足够让 SMAppService 注册(未用 Developer ID 签名的应用,
# 可能需要用户在 系统设置 > 通用 > 登录项与扩展 里手动批准一次)。
# Ad-hoc sign: enough for SMAppService locally. Apps not signed with a Developer ID may require a
# one-time approval in System Settings > General > Login Items & Extensions.
codesign --force --sign - "$APP"

echo "Built $APP"
echo "Launch with: open $APP   (SMAppService only works when launched as a .app, not via cargo run)"
