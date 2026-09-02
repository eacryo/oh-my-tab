# 发布流程（维护者指南）

本文面向维护者，收录从 README 精简出来的发布与打包细节：Homebrew cask 发布流水线、代码签名原理、应用图标再生成。日常安装与构建见 README。

## Homebrew cask 发布

`scripts/release.sh` 是完整的发布流水线：先跑 `bundle.sh`（构建 .app + .dmg + Sparkle .zip + 签名），再生成 `dist/oh-my-tab.rb` —— 一个 Homebrew cask 文件，内含 dmg 的 `sha256`、从 `Cargo.toml` 读出的 `version`，以及 `zap trash:` 块（`brew uninstall --cask` 会一并清理图标缓存、日志和配置）。默认不会上传；只有显式传入 `--push` 才会调用 R2 发布工具。

```sh
sh scripts/release.sh                  # 本地构建，不访问 R2
sh scripts/release.sh --push          # 构建后上传 ZIP、DMG、appcast.xml
sh scripts/release.sh --push --dry-run  # 检查并打印上传计划
```

开发通道使用独立的 Bundle ID、Feed、R2 前缀和包名前缀，不会混入生产更新：

```sh
sh scripts/release-dev.sh                 # 只构建开发包，不访问 R2
sh scripts/release-dev.sh --push          # 上传到 dev_release，并发布 dev_release/appcast.xml
sh scripts/release-dev.sh --push --dry-run
```

带 `--push` 时，脚本会始终基于当前源码重新构建 `.app`、ZIP 和 DMG，然后调用仓库内固定版本的
`vendor/Sparkle/bin/generate_appcast`。
如果本地已有 appcast，脚本会先使用它；在干净 checkout 中会从公开 Feed 读取旧 appcast，
以保留历史条目。若 Feed 尚不存在，则创建新的 appcast。工具从临时目录中的最终 ZIP
文件名生成 enclosure URL，确保它和 R2 publisher 随后上传的对象一致。

appcast 默认从 macOS Keychain 读取名为 `ed25519` 的 Ed25519 私钥。也可以通过
`SPARKLE_ED_KEY_FILE` 指定外部私钥文件；私钥不得提交到仓库。`--push --dry-run` 保持为
只打印上传计划，不会生成或上传文件。

cask 里硬编码了 `depends_on macos: :ventura` + `depends_on arch: :arm64`，所以只能安装在 macOS 13+ 的 Apple Silicon 上。它的 `url` 指向 `https://github.com/eacryo/oh-my-tab/releases/download/v#{version}/Oh-My-Tab.dmg`，因此 dmg 必须传到一个 tag 为 `v<version>` 的 GitHub release（与 `Cargo.toml` 的 version 一致）。

发布新版本流程：

1. 改 `Cargo.toml` 里的 `version`。
2. 跑 `sh scripts/release.sh` -> 产出 `dist/Oh-My-Tab.dmg` 和 `dist/oh-my-tab.rb`。
3. 建一个 tag 为 `v<version>` 的 GitHub release，把 `dist/Oh-My-Tab.dmg` 传上去。
4. 把 `dist/oh-my-tab.rb` 拷到 [homebrew-tap](https://github.com/eacryo/homebrew-tap) 仓库的 `Casks/` 目录，push。

第 4 步完成后，`brew install --cask eacryo/tap/oh-my-tab`（或 `brew upgrade --cask`）就能拉到新版本。`brew install --cask` 实际读取的是 tap 仓库里已提交的那份 `Casks/oh-my-tab.rb`；`release.sh` 只是在本地重新生成它，方便拷贝。

`--push` 使用 `tools/r2-publisher`，凭证只从环境变量读取：`R2_ACCESS_KEY_ID`、`R2_SECRET_ACCESS_KEY`、`R2_BUCKET`、`R2_ENDPOINT`（或 `R2_ACCOUNT_ID`）。工具会先上传 ZIP 和 DMG，最后上传刚生成的 appcast；生产默认对象路径是 `releases/` + `appcast.xml`，开发脚本统一放在 `dev_release/`（归档和 `appcast.xml` 都在这里），并使用 `Oh-My-Tab-Dev-...` 归档名前缀；这些值仍可通过环境变量（包括 `R2_RELEASE_PREFIX`、`R2_APPCAST_KEY`、`R2_ARTIFACT_BASENAME`）覆盖。

上传目标与下载地址分离：上传始终使用 `R2_ENDPOINT`（或 `R2_ACCOUNT_ID` 推导出的 S3 endpoint）和 `R2_BUCKET`；`https://download.oh-my-tab.app` 只用于客户端访问 appcast 和归档文件。`R2_PUBLIC_BASE_URL` 只影响发布计划中显示的公开 URL。

## Sparkle 更新材料

发布前置条件：普通的 `cargo build` / `cargo test` 不需要 Sparkle；仓库已提交固定版本的 Sparkle 2 framework 到 `vendor/Sparkle.framework`，因此默认可构建带更新功能的 `.app`。仓库也包含固定版本 Sparkle 2.9.6 的 `vendor/Sparkle/bin/generate_keys` 和 `vendor/Sparkle/bin/generate_appcast`，用于生成密钥和 appcast；这两个工具是 macOS universal 二进制文件。相关工具许可证见 `vendor/Sparkle/LICENSE`。

更新器代码会在运行时加载 `Contents/Frameworks/Sparkle.framework`。把 Sparkle 2 的框架放到 `vendor/Sparkle.framework`，或设置 `SPARKLE_FRAMEWORK_PATH`，`bundle.sh` / `dev-restart.sh` 会自动拷贝它。生产应用包中的 `SUFeedURL` 默认是 `https://download.oh-my-tab.app/appcast.xml`，开发重启和开发发布脚本默认使用 `https://download.oh-my-tab.app/dev_release/appcast.xml`；也可用 `SPARKLE_FEED_URL` 覆盖。

`appcast.xml` 和更新归档由发布者自行上传到 R2。生成 appcast 时使用 Sparkle 的 Ed25519 私钥；打包时只需把对应公钥通过 `SPARKLE_PUBLIC_ED_KEY` 注入 `SUPublicEDKey`。Sparkle 比较 `CFBundleVersion`（build number），脚本默认用 UTC 时间戳生成它；需要可复现的测试时再设置 `SPARKLE_BUILD_VERSION`。`CFBundleShortVersionString` 仍负责展示给用户的版本。私钥不要提交到仓库、不要放进应用包，也不要上传到 R2。

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
