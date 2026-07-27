#!/bin/sh
# 打包 release .app 并打成 .dmg:编译 -> 组装 bundle -> ad-hoc 签名 -> DMG。
# 产物输出到 dist/(已 gitignore),放在 target/ 之外以保持 logger 的 is_dev=false(走文件日志)。
#
# Build the release .app and package it as .dmg: build -> assemble bundle -> ad-hoc sign -> DMG.
# Output goes to dist/ (gitignored), outside target/ so the logger's is_dev stays false (file logging).
set -e

# 失败提示:set -e 触发非零退出时打印 + 清理 DMG 临时目录;成功走到末尾退出码为 0,静默。
# Failure notice + DMG staging cleanup on non-zero exit (set -e); silent on success (exit 0).
STAGING=""
trap 'code=$?; [ -n "$STAGING" ] && rm -rf "$STAGING"; [ "$code" -ne 0 ] && echo "❌ Build failed (exit $code)" >&2' EXIT

# 脚本在 scripts/ 下,先切到仓库根再引用相对路径。
# Script lives in scripts/; cd to the repo root before using relative paths.
cd "$(dirname "$0")/.."

APP="dist/Oh-My-Tab.app"
DMG="dist/Oh-My-Tab.dmg"

cargo build --release
BIN="target/release/oh-my-tab"

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp "$BIN" "$APP/Contents/MacOS/oh-my-tab"
cp assets/Info.plist "$APP/Contents/Info.plist"

# 从 Cargo.toml 读 version(唯一事实源),写入 .app 的 Info.plist CFBundleShortVersionString,
# 让 app 显示版本与 Cargo.toml 一致(不用手动同步 Info.plist)。
# Read version from Cargo.toml (single source of truth) and write it into the .app's
# Info.plist CFBundleShortVersionString so the displayed version matches Cargo.toml
# (no manual Info.plist sync needed).
VERSION=$(awk -F'"' '/^version/ {print $2; exit}' Cargo.toml)
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $VERSION" "$APP/Contents/Info.plist"

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

# 签名:优先用自签名证书 "oh-my-tab-sign"(让 TCC 身份稳定,Accessibility 授权不会因 rebuild 失效);
# 签名失败(证书缺失 / 钥匙串拒绝)时退回 ad-hoc(TCC 授权随 CDHash 变化失效,仅适合临时本机调试)。
# 注意:security find-identity 对未设信任的自签名证书会漏报(返回 0),所以这里直接试签、失败再回退。
# 建证书(一次性):钥匙串访问 -> 证书助理 -> 创建证书 ->
#   名称 "oh-my-tab-sign",身份类型「自签名根」,证书类型「代码签名」。
# Sign: prefer the self-signed "oh-my-tab-sign" identity so the TCC identity stays stable across
# rebuilds (Accessibility grants won't break when the CDHash changes); fall back to ad-hoc (grants
# break on every rebuild - local debugging only) when the cert is absent or signing fails.
# Note: `security find-identity` under-reports untrusted self-signed certs (returns 0), so we just
# attempt to sign and fall back on failure.
# Create the cert (one-time): Keychain Access > Certificate Assistant > Create a Certificate >
#   name "oh-my-tab-sign", Identity Type "Self Signed Root", Certificate Type "Code Signing".
SIGN_IDENTITY="oh-my-tab-sign"
SIGN_ERR="$(mktemp)"
if codesign --force --sign "$SIGN_IDENTITY" "$APP" 2>"$SIGN_ERR"; then
  :
else
  echo "warning: signing with '$SIGN_IDENTITY' failed; falling back to ad-hoc (TCC grants won't persist):" >&2
  sed 's/^/         /' "$SIGN_ERR" >&2
  codesign --force --sign - "$APP"
fi
rm -f "$SIGN_ERR"

# 打 DMG(hdiutil 自带,无额外依赖):staging 里放 .app + Applications 软链,挂载后拖拽安装。
# Build DMG via hdiutil (no extra deps): staging holds .app + Applications symlink for drag-to-install.
STAGING="$(mktemp -d)"
cp -R "$APP" "$STAGING/"
ln -s /Applications "$STAGING/Applications"
rm -f "$DMG"
hdiutil create -volname "Oh-My-Tab" -srcfolder "$STAGING" -ov -format UDZO "$DMG" >/dev/null
rm -rf "$STAGING"; STAGING=""

echo "Install: open $DMG   then drag Oh-My-Tab to Applications"
echo "Dev-run: open $APP   (SMAppService only works when launched as a .app, not via cargo run)"
echo "✅ Build success: $DMG (contains $APP)"
