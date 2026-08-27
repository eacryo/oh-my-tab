# 发布流程（维护者指南）

本文面向维护者，收录从 README 精简出来的发布与打包细节：Homebrew cask 发布流水线、代码签名原理、应用图标再生成。日常安装与构建见 README。

## Homebrew cask 发布

`scripts/release.sh` 是完整的发布流水线：先跑 `bundle.sh`（构建 .app + .dmg + 签名），再生成 `dist/oh-my-tab.rb` —— 一个 Homebrew cask 文件，内含 dmg 的 `sha256`、从 `Cargo.toml` 读出的 `version`，以及 `zap trash:` 块（`brew uninstall --cask` 会一并清理图标缓存、日志和配置）。

```sh
sh scripts/release.sh        # bundle.sh -> dist/Oh-My-Tab.dmg + dist/oh-my-tab.rb
```

cask 里硬编码了 `depends_on macos: :ventura` + `depends_on arch: :arm64`，所以只能安装在 macOS 13+ 的 Apple Silicon 上。它的 `url` 指向 `https://github.com/eacryo/oh-my-tab/releases/download/v#{version}/Oh-My-Tab.dmg`，因此 dmg 必须传到一个 tag 为 `v<version>` 的 GitHub release（与 `Cargo.toml` 的 version 一致）。

发布新版本流程：

1. 改 `Cargo.toml` 里的 `version`。
2. 跑 `sh scripts/release.sh` -> 产出 `dist/Oh-My-Tab.dmg` 和 `dist/oh-my-tab.rb`。
3. 建一个 tag 为 `v<version>` 的 GitHub release，把 `dist/Oh-My-Tab.dmg` 传上去。
4. 把 `dist/oh-my-tab.rb` 拷到 [homebrew-tap](https://github.com/eacryo/homebrew-tap) 仓库的 `Casks/` 目录，push。

第 4 步完成后，`brew install --cask eacryo/tap/oh-my-tab`（或 `brew upgrade --cask`）就能拉到新版本。`brew install --cask` 实际读取的是 tap 仓库里已提交的那份 `Casks/oh-my-tab.rb`；`release.sh` 只是在本地重新生成它，方便拷贝。

## 代码签名：为什么自签证书能让授权稳定

`bundle.sh` 优先用自签名身份 **`oh-my-tab-sign`** 签名，证书缺失或签名失败时退回 ad-hoc（`codesign -s -`）。

**原因：** ad-hoc 签名应用的指定要求（designated requirement）只是裸 CDHash，每次 rebuild 都变。macOS TCC 按该 CDHash 记辅助功能（Accessibility）授权，所以每次 rebuild 都会让授权失效（TCC 日志报 `Failed to match existing code requirement` / `errSecCSReqFailed`），旧安装残留的条目还会雪上加霜。自签名证书让指定要求变成证书型（`certificate leaf = H"..."`），rebuild 不变，授权就稳了。

一次性创建证书（钥匙串访问）：

1. *钥匙串访问 -> 证书助理 -> 创建证书…*
2. 名称：`oh-my-tab-sign`，身份类型：**自签名根**，证书类型：**代码签名**。
3. 创建。（首次跑 `bundle.sh` 可能弹钥匙串访问提示 -- 点「始终允许」。）

然后重新打包、重装、授予辅助功能一次。之后每次 rebuild 用的是同一个证书身份，无需再重新授权。若授权变陈旧（比如旧 ad-hoc 安装残留），清除：

```sh
tccutil reset Accessibility com.eacryo.oh-my-tab
```

**注意：** 自签名证书只稳定 TCC 身份，**不**满足 Gatekeeper 分发 -- 别人装仍是「未识别开发者」，需右键打开。要彻底解决分发得用付费的 Apple **Developer ID Application** 证书；有的话把 `scripts/bundle.sh` 里的 `SIGN_IDENTITY` 改成那个名字。

## 应用图标

应用图标（`AppIcon.icns`）由 `assets/Icon-Default-1024x1024@1x.png` 生成，打包进 `Contents/Resources/`。`assets/AppIcon.icns` 已提交进仓库，`bundle.sh` 直接使用它，因此贡献者构建 `.app` 时无需任何额外工具。

替换源 PNG 后重新生成：

```sh
./scripts/build-icon-from-png.sh   # 1024x1024 PNG -> 10 张 .iconset 尺寸(sips) -> assets/AppIcon.icns
```

只需 `iconutil`（Xcode CLT）；`sips` macOS 自带。生成后把新的 `assets/AppIcon.icns` 连同 `assets/Icon-Default-1024x1024@1x.png` 改动一起提交。

若存在 `assets/AppIcon.icon`（目录），`bundle.sh` 还会把它拷进 `Contents/Resources/`，用于 macOS 26+ 的 Liquid Glass 图标格式（系统优先于 `.icns`）。
