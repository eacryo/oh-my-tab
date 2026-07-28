#!/bin/sh
# Release 打包:先跑 bundle.sh(构建 .app + .dmg + 签名),再生成 Homebrew cask 文件
# dist/oh-my-tab.rb(算 dmg 的 sha256 + 从 Cargo.toml 读 version 填模板,带 zap 清理)。
# 只支持 macOS 13+ Apple Silicon(arm64):cask 用 depends_on macos: :ventura + depends_on arch: :arm64
# 限制,Linux(cask 本身不支持)/ Intel Mac / macOS < 13 装都会报错。
# 把它拷到你的 homebrew tap 仓库的 Casks/ 目录,push 即可。
#
# Release packaging: runs bundle.sh (build .app + .dmg + sign) first, then generates the Homebrew
# cask file dist/oh-my-tab.rb (sha256 of the dmg + version from Cargo.toml filled into a template,
# with zap cleanup). macOS 13+ Apple Silicon (arm64) only: the cask uses depends_on macos: :ventura
# + depends_on arch: :arm64, so Linux / Intel Mac / macOS < 13 are rejected at install.
# Copy it into your homebrew tap repo's Casks/ directory and push.
set -e

# 脚本在 scripts/ 下,先切到仓库根再引用相对路径。
# Script lives in scripts/; cd to the repo root before using relative paths.
cd "$(dirname "$0")/.."

# 1. 先跑构建脚本(打包 .app + .dmg + 签名)。
# 1. Run the build script first (build .app + .dmg + sign).
sh scripts/bundle.sh

DMG="dist/Oh-My-Tab.dmg"
OUT="dist/oh-my-tab.rb"

if [ ! -f "$DMG" ]; then
  echo "❌ Build failed: $DMG not found" >&2
  exit 1
fi

# 2. 从 Cargo.toml 读 version(与 bundle.sh 同源)。
# 2. Read version from Cargo.toml (same source as bundle.sh).
VERSION=$(awk -F'"' '/^version/ {print $2; exit}' Cargo.toml)

# 3. 算 dmg sha256。
# 3. Compute the dmg sha256.
SHA=$(shasum -a 256 "$DMG" | awk '{print $1}')

# 4. 生成 cask(带 zap:卸载时清理缓存/日志/配置)。
#    heredoc 不带引号 -> $VERSION/$SHA 由 shell 展开;#{version} 是 Ruby 插值,shell 不动它。
# 4. Generate the cask (with zap to clean caches/logs/config on uninstall).
#    Unquoted heredoc -> $VERSION/$SHA are expanded by the shell; #{version} is Ruby interpolation
#    and left intact.
cat > "$OUT" <<EOF
cask "oh-my-tab" do
  depends_on macos: :ventura
  depends_on arch: :arm64
  version "$VERSION"
  sha256 "$SHA"
  url "https://github.com/eacryo/oh-my-tab/releases/download/v#{version}/Oh-My-Tab.dmg"
  name "Oh-My-Tab"
  desc "macOS window switcher (Cmd+Tab alternative)"
  homepage "https://github.com/eacryo/oh-my-tab"
  app "Oh-My-Tab.app"

  zap trash: [
    "~/Library/Caches/oh-my-tab-icons",
    "~/Library/Logs/oh-my-tab",
    "~/.config/oh-my-tab",
  ]
end
EOF

echo "✅ Generated $OUT (version=$VERSION, sha256=$SHA)"
echo "Copy it to your homebrew tap repo's Casks/ directory, then push."
