#!/bin/bash
# 打包 release .app 并打成 .dmg:编译 -> 组装 bundle -> 签名 -> DMG。
# 产物输出到 dist/(已 gitignore),放在 target/ 之外以保持 logger 的 is_dev=false(走文件日志)。
#
# Build the release .app and package it as .dmg: build -> assemble bundle -> sign -> DMG.
# Output goes to dist/ (gitignored), outside target/ so the logger's is_dev stays false (file logging).
set -e

# 失败提示:set -e 触发非零退出时打印 + 清理 DMG 临时目录;成功走到末尾退出码为 0,静默。
# Failure notice + DMG staging cleanup on non-zero exit (set -e); silent on success (exit 0).
STAGING=""
trap 'code=$?; [ -n "$STAGING" ] && rm -rf "$STAGING"; [ "$code" -ne 0 ] && echo "❌ Build failed (exit $code)" >&2' EXIT

# 脚本在 scripts/ 下,先切到仓库根再引用相对路径。
# Script lives in scripts/; cd to the repo root before using relative paths.
cd "$(dirname "$0")/.."

APP_BASENAME="${APP_BASENAME:-Oh-My-Tab}"
APP="dist/${APP_BASENAME}.app"
DMG="dist/${APP_BASENAME}.dmg"
ZIP="dist/${APP_BASENAME}.zip"

# Release notes are part of the release contract, not an optional publishing extra. Validate the
# version-specific document before compiling so a release can never produce an archive without its
# matching changelog.
# 发布说明是发布契约的一部分，不是可选的上传附属物。在编译前校验版本文档，避免生成没有
# 对应更新日志的安装包。
VERSION=$(awk -F'"' '/^version/ {print $2; exit}' Cargo.toml)
RELEASE_DOC_DIR="${RELEASE_DOC_DIR:-release_doc}"
RELEASE_DOC="${RELEASE_DOC_DIR}/${VERSION}.md"
if [ ! -s "$RELEASE_DOC" ]; then
  echo "error: release notes not found for version $VERSION: $RELEASE_DOC" >&2
  exit 1
fi

# release-dev.sh sets CARGO_BUILD_FEATURES=dev-long-text so its optimized package keeps the
# long-text layout fixture. The production release script leaves this unset.
# release-dev.sh 设置 CARGO_BUILD_FEATURES=dev-long-text，让优化后的 Dev 包保留长文本夹具；
# 正式 release 脚本不设置该变量。
if [ -n "${CARGO_BUILD_FEATURES:-}" ]; then
  cargo build --release --features "$CARGO_BUILD_FEATURES"
else
  cargo build --release
fi
BIN="target/release/oh-my-tab"

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp "$BIN" "$APP/Contents/MacOS/oh-my-tab"
cp assets/Info.plist "$APP/Contents/Info.plist"

# The production defaults remain unchanged; the dev release wrapper overrides these values so
# both channels can share this packaging implementation without sharing bundle identities.
BUNDLE_ID="${BUNDLE_ID:-com.eacryo.oh-my-tab}"
BUNDLE_NAME="${BUNDLE_NAME:-$APP_BASENAME}"
/usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier $BUNDLE_ID" "$APP/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleName $BUNDLE_NAME" "$APP/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleDisplayName $BUNDLE_NAME" "$APP/Contents/Info.plist"

# Sparkle 2 is loaded by the Rust updater at runtime. Keep the framework out of git and copy a
# locally downloaded release into the bundle when available. Set SPARKLE_FRAMEWORK_PATH to an
# alternate checkout path; the default is vendor/Sparkle.framework.
SPARKLE_FRAMEWORK_PATH="${SPARKLE_FRAMEWORK_PATH:-vendor/Sparkle.framework}"
if [ -d "$SPARKLE_FRAMEWORK_PATH" ]; then
  mkdir -p "$APP/Contents/Frameworks"
  cp -R "$SPARKLE_FRAMEWORK_PATH" "$APP/Contents/Frameworks/Sparkle.framework"
  echo "Sparkle: embedded $SPARKLE_FRAMEWORK_PATH"
else
  echo "warning: Sparkle.framework not found at $SPARKLE_FRAMEWORK_PATH; update checks will be unavailable"
fi

# 从 Cargo.toml 读 version(唯一事实源),写入 .app 的 Info.plist CFBundleShortVersionString,
# 让 app 显示版本与 Cargo.toml 一致(不用手动同步 Info.plist)。
# Read version from Cargo.toml (single source of truth) and write it into the .app's
# Info.plist CFBundleShortVersionString so the displayed version matches Cargo.toml
# (no manual Info.plist sync needed).
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $VERSION" "$APP/Contents/Info.plist"
# Sparkle compares the monotonically increasing CFBundleVersion (build number), not just the
# display version. Use a UTC timestamp by default so every build gets a fresh value; an explicit
# SPARKLE_BUILD_VERSION is still available for deterministic release/test builds.
BUILD_VERSION="${SPARKLE_BUILD_VERSION:-$(date -u +%Y%m%d%H%M%S)}"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $BUILD_VERSION" "$APP/Contents/Info.plist"

# Optional release-time overrides. The public Ed25519 key is safe to ship in the app; the private
# signing key must stay outside this repository and is only used when generating appcast.xml.
SPARKLE_FEED_URL="${SPARKLE_FEED_URL:-https://download.oh-my-tab.app/appcast.xml}"
/usr/libexec/PlistBuddy -c "Set :SUFeedURL $SPARKLE_FEED_URL" "$APP/Contents/Info.plist"
if [ -n "${SPARKLE_PUBLIC_ED_KEY:-}" ]; then
  /usr/libexec/PlistBuddy -c "Add :SUPublicEDKey string $SPARKLE_PUBLIC_ED_KEY" "$APP/Contents/Info.plist" 2>/dev/null \
    || /usr/libexec/PlistBuddy -c "Set :SUPublicEDKey $SPARKLE_PUBLIC_ED_KEY" "$APP/Contents/Info.plist"
fi

# 应用图标:从 assets/AppIcon.icns 拷入 Contents/Resources/(放在 codesign 之前,纳入签名)。
# 该 icns 由 build-icon-from-png.sh 从 Icon-Default-1024x1024@1x.png 生成并提交进 git;缺失则提示先跑该脚本。
# App icon: copy assets/AppIcon.icns into Contents/Resources/ (before codesign so it is covered by the signature).
# The icns is generated from Icon-Default-1024x1024@1x.png by build-icon-from-png.sh and committed; if missing, hint to run that script first.
ICON="assets/AppIcon.icns"
if [ ! -f "$ICON" ]; then
  echo "error: $ICON not found. Run ./scripts/build-icon-from-png.sh first." >&2
  exit 1
fi
mkdir -p "$APP/Contents/Resources"
cp "$ICON" "$APP/Contents/Resources/AppIcon.icns"

# 本地化 .lproj 锚点(与 assets/Info.plist 的 CFBundleLocalizations 配套):缺了它们,
# 系统面板(NSSavePanel 等)的按钮在中文系统上显示英文。放在 codesign 之前纳入签名。
# Localized .lproj anchors (paired with CFBundleLocalizations in assets/Info.plist):
# without them, system panels (NSSavePanel etc.) show English buttons on a Chinese system.
# Copied before codesign so they are covered by the signature.
cp -R assets/Resources/. "$APP/Contents/Resources/"

# macOS 26+ Liquid Glass 图标(目录格式):macOS 自动优先使用 .icon,找不到时回落 .icns。
# macOS 26+ Liquid Glass icon (directory format); macOS auto-prefers .icon, falls back to .icns.
ICON_DIR="assets/AppIcon.icon"
if [ -d "$ICON_DIR" ]; then
  cp -R "$ICON_DIR" "$APP/Contents/Resources/AppIcon.icon"
fi

# 可选的产出标记:dev 渠道包装脚本设置 DEV_BUILD_PROFILE=release-dev,便于
# dev-restart.sh 识别并提示它即将用本地开发包覆盖这个发布渠道包。必须在 codesign 之前写入。
# Optional provenance marker: the dev-channel wrapper sets DEV_BUILD_PROFILE=release-dev so
# dev-restart.sh can tell it is about to replace this channel package with a local dev build.
# Written before codesign so it is covered by the signature.
if [ -n "${DEV_BUILD_PROFILE:-}" ]; then
  printf '%s\n' "$DEV_BUILD_PROFILE" > "$APP/Contents/Resources/dev-build-profile.txt"
fi

# Sparkle 自带的 XPC/helper 默认可能是 ad-hoc 签名。正式发布时按 Sparkle 的发布流程
# 从内向外重签；Downloader.xpc 保留 Sparkle 自己的 entitlements，不能把主应用权限套进去。
# Sparkle's XPC/helper tools may ship ad-hoc signed. Re-sign them inside-out for distribution;
# preserve Downloader.xpc's own entitlements instead of applying the main app's entitlements.
sign_release_component() {
  local path="$1"
  local preserve_entitlements="${2:-0}"
  if [ "$preserve_entitlements" = "1" ]; then
    codesign --force --options runtime --timestamp --preserve-metadata=entitlements \
      --sign "$CODESIGN_IDENTITY" "$path" || {
      echo "error: Developer ID signing failed for $path" >&2
      return 1
    }
  else
    codesign --force --options runtime --timestamp \
      --sign "$CODESIGN_IDENTITY" "$path" || {
      echo "error: Developer ID signing failed for $path" >&2
      return 1
    }
  fi
}

sign_sparkle_for_release() {
  local framework="$1"
  local installer="$framework/Versions/B/XPCServices/Installer.xpc"
  local downloader="$framework/Versions/B/XPCServices/Downloader.xpc"
  local autoupdate="$framework/Versions/B/Autoupdate"
  local updater="$framework/Versions/B/Updater.app"
  local required_path=""

  for required_path in "$installer" "$downloader" "$autoupdate" "$updater"; do
    if [ ! -e "$required_path" ]; then
      echo "error: expected Sparkle release component is missing: $required_path" >&2
      return 1
    fi
  done

  sign_release_component "$installer" || return 1
  sign_release_component "$downloader" 1 || return 1
  sign_release_component "$autoupdate" || return 1
  sign_release_component "$updater" || return 1
  sign_release_component "$framework" || return 1
}

verify_developer_id_signature() {
  local path="$1"
  local expected_team_id="$2"
  local details=""
  local team_id=""
  local timestamp=""

  if ! details="$(codesign --display --verbose=4 "$path" 2>&1)"; then
    echo "error: could not read code signature for $path" >&2
    return 1
  fi
  team_id="$(printf '%s\n' "$details" | awk -F= '$1 == "TeamIdentifier" { print $2; exit }')"
  timestamp="$(printf '%s\n' "$details" | awk -F= '$1 == "Timestamp" { print substr($0, index($0, "=") + 1); exit }')"

  if [ "$team_id" != "$expected_team_id" ]; then
    echo "error: $path has TeamIdentifier '$team_id'; expected '$expected_team_id'" >&2
    return 1
  fi
  if [ -z "$timestamp" ] || [ "$timestamp" = "none" ]; then
    echo "error: $path is missing a secure code-signing timestamp" >&2
    return 1
  fi
}

# 正式公证包必须使用 Developer ID、Hardened Runtime 和安全时间戳;签名失败时立即终止。
# 本机开发打包仍优先使用自签名身份,并保留 ad-hoc 回退。
# Notarized release packages require Developer ID, Hardened Runtime, and a secure timestamp;
# fail closed if that signing fails. Local development packages keep the self-signed/ad-hoc path.
if [ "${RELEASE_SIGNING:-0}" = "1" ]; then
  if [ -z "${CODESIGN_IDENTITY:-}" ]; then
    echo "error: set CODESIGN_IDENTITY to a Developer ID Application identity" >&2
    exit 1
  fi
  SPARKLE_FRAMEWORK="$APP/Contents/Frameworks/Sparkle.framework"
  if [ -d "$SPARKLE_FRAMEWORK" ] && ! sign_sparkle_for_release "$SPARKLE_FRAMEWORK"; then
    echo "error: failed to sign embedded Sparkle code for notarization" >&2
    exit 1
  fi
  if ! codesign --force --options runtime --timestamp \
    --sign "$CODESIGN_IDENTITY" "$APP"; then
    echo "error: Developer ID signing failed for $APP" >&2
    exit 1
  fi
  if ! codesign --verify --deep --strict "$APP"; then
    echo "error: release signature verification failed for $APP" >&2
    exit 1
  fi
  APP_SIGNATURE="$(codesign --display --verbose=4 "$APP" 2>&1)"
  APP_TEAM_ID="$(printf '%s\n' "$APP_SIGNATURE" | awk -F= '$1 == "TeamIdentifier" { print $2; exit }')"
  if [ -z "$APP_TEAM_ID" ] || [ "$APP_TEAM_ID" = "not set" ]; then
    echo "error: $APP does not have a Developer ID TeamIdentifier" >&2
    exit 1
  fi
  verify_developer_id_signature "$APP" "$APP_TEAM_ID"
  if [ -d "$SPARKLE_FRAMEWORK" ]; then
    for path in \
      "$SPARKLE_FRAMEWORK/Versions/B/XPCServices/Installer.xpc" \
      "$SPARKLE_FRAMEWORK/Versions/B/XPCServices/Downloader.xpc" \
      "$SPARKLE_FRAMEWORK/Versions/B/Autoupdate" \
      "$SPARKLE_FRAMEWORK/Versions/B/Updater.app" \
      "$SPARKLE_FRAMEWORK"; do
      verify_developer_id_signature "$path" "$APP_TEAM_ID"
    done
  fi
  echo "signed with $CODESIGN_IDENTITY (Hardened Runtime + secure timestamp)"
else
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
fi

# Sparkle's preferred archive is a zip containing the complete .app bundle. Keep the DMG for
# manual installation and publish both artifacts from the release script when requested.
rm -f "$ZIP"
ditto -c -k --keepParent "$APP" "$ZIP"

# 打 DMG(hdiutil 自带,无额外依赖):staging 里放 .app + Applications 软链,挂载后拖拽安装。
# Build DMG via hdiutil (no extra deps): staging holds .app + Applications symlink for drag-to-install.
STAGING="$(mktemp -d)"
cp -R "$APP" "$STAGING/"
ln -s /Applications "$STAGING/Applications"
rm -f "$DMG"
hdiutil create -volname "Oh-My-Tab" -srcfolder "$STAGING" -ov -format UDZO "$DMG" >/dev/null
rm -rf "$STAGING"; STAGING=""

echo "Install: open $DMG   then drag $APP_BASENAME to Applications"
echo "Sparkle: $ZIP (contains $APP)"
echo "Dev-run: open $APP   (SMAppService only works when launched as a .app, not via cargo run)"
echo "✅ Build success: $DMG + $ZIP (both contain $APP)"
