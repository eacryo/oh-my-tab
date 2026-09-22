# 发布流程（维护者指南）

本文面向维护者，收录从 README 精简出来的发布与打包细节：Homebrew cask 发布流水线、代码签名原理、应用图标再生成。日常安装与构建见 README。

## Homebrew cask 发布

`scripts/release.sh` 是正式版发布流水线。不带动作参数时，它会在本地构建产物并生成 `dist/oh-my-tab.rb`。这份 Homebrew cask 包含 DMG 的 `sha256`、`Cargo.toml` 中的版本号，以及卸载时清理图标缓存、日志和应用数据的 `zap trash:` 配置。

正式发布需要另外完成公证流程。Apple 接受公证提交后，再通过 `--push` 发布到 R2。

```sh
sh scripts/release.sh                    # 本地构建产物和 Homebrew cask
sh scripts/release.sh --notarize         # 构建、签名并提交 Apple 公证
sh scripts/release.sh --check            # 查询待处理的公证状态
sh scripts/release.sh --push             # 对公证通过的应用贴票并上传产物
sh scripts/release.sh --push --dry-run   # 准备产物并打印上传计划
```

开发通道使用独立的 Bundle ID、Feed、R2 前缀和包名前缀，与生产更新分开：

```sh
sh scripts/release-dev.sh                 # 只构建开发包，不访问 R2
sh scripts/release-dev.sh --push          # 上传到 dev_release，并发布 dev_release/appcast.xml
sh scripts/release-dev.sh --push --dry-run
```

`release-dev.sh` 仍使用 Release 优化构建，但会专门启用 `dev-long-text` Cargo feature，因此
开发发布包的语言下拉框包含 `[TEST] English x3`，用于验证超长文案的下拉框、设置行和卡片布局。
正式 `release.sh` 以及直接调用 `bundle.sh` 的生产路径不启用该 feature，生产包不会包含这个
测试选项。

正式发布时，`--notarize` 会构建并暂存签名后的 `.app`，`--check` 用于查询 Apple 公证状态。状态变为 `Accepted` 后，`--push` 会给应用贴上公证票据并生成最终 ZIP 和 DMG，然后调用仓库内固定版本的 `vendor/Sparkle/bin/generate_appcast` 和 R2 发布工具。

如果本地已有 appcast，生成脚本会从它开始更新。干净 checkout 中则会读取公开 Feed，以保留历史条目；Feed 不存在时会创建新文件。enclosure URL 使用最终 ZIP 文件名，与发布工具上传的对象保持一致。

appcast 默认从 macOS Keychain 读取名为 `ed25519` 的 Ed25519 私钥，也可以通过 `SPARKLE_ED_KEY_FILE` 指定外部私钥文件；私钥不得提交到仓库。`--push --dry-run` 会在本地准备 ZIP、DMG、appcast 和 cask，再让 R2 发布工具打印上传计划而不实际上传。待处理的公证状态会保留。

cask 里硬编码了 `depends_on macos: :ventura` + `depends_on arch: :arm64`，所以只能安装在 macOS 13+ 的 Apple Silicon 上。它的 `url` 指向 `https://github.com/eacryo/oh-my-tab/releases/download/v#{version}/Oh-My-Tab.dmg`，因此 dmg 必须传到一个 tag 为 `v<version>` 的 GitHub release（与 `Cargo.toml` 的 version 一致）。

发布新版本流程：

1. 改 `Cargo.toml` 里的 `version`。
2. 跑 `sh scripts/release.sh` → 产出 `dist/Oh-My-Tab.dmg` 和 `dist/oh-my-tab.rb`。
3. 建一个 tag 为 `v<version>` 的 GitHub release，把 `dist/Oh-My-Tab.dmg` 传上去。
4. 把 `dist/oh-my-tab.rb` 拷到 [homebrew-tap](https://github.com/eacryo/homebrew-tap) 仓库的 `Casks/` 目录，push。

第 4 步完成后，`brew install --cask eacryo/tap/oh-my-tab`（或 `brew upgrade --cask`）就能拉到新版本。`brew install --cask` 实际读取的是 tap 仓库里已提交的那份 `Casks/oh-my-tab.rb`；`release.sh` 只是在本地重新生成它，方便拷贝。

`--push` 使用 `tools/r2-publisher`。凭证和目标配置只从环境变量读取：`R2_ACCESS_KEY_ID`、`R2_SECRET_ACCESS_KEY`、`R2_BUCKET`，以及 `R2_ENDPOINT` 或 `R2_ACCOUNT_ID`。发布工具先上传 ZIP 和 DMG，再上传 appcast；设置 `R2_LATEST_DMG_KEY` 后，还会更新一个使用短缓存策略的最新 DMG 别名。

生产对象默认放在 `releases/`，appcast 使用 `appcast.xml`，bucket 根目录下的 `Oh-My-Tab.dmg` 作为最新 DMG 别名。开发产物使用 `dev_release/` 和 `Oh-My-Tab-Dev-...` 归档名前缀。`R2_RELEASE_PREFIX`、`R2_APPCAST_KEY`、`R2_ARTIFACT_BASENAME` 和 `R2_LATEST_DMG_KEY` 可覆盖这些值。

上传目标与下载地址相互独立。上传使用 `R2_ENDPOINT`（或由 `R2_ACCOUNT_ID` 推导出的 S3 endpoint）和 `R2_BUCKET`。客户端通过 `https://download.oh-my-tab.app` 访问 appcast 和归档；`R2_PUBLIC_BASE_URL` 用于调整生成的 enclosure URL 和发布计划所使用的公开基础 URL。

## Sparkle 更新材料

发布前置条件：普通的 `cargo build` / `cargo test` 不需要 Sparkle；仓库已提交固定版本的 Sparkle 2 framework 到 `vendor/Sparkle.framework`，因此默认可构建带更新功能的 `.app`。仓库也包含固定版本 Sparkle 2.9.6 的 `vendor/Sparkle/bin/generate_keys` 和 `vendor/Sparkle/bin/generate_appcast`，用于生成密钥和 appcast；这两个工具是 macOS universal 二进制文件。相关工具许可证见 `vendor/Sparkle/LICENSE`。

更新器代码会在运行时加载 `Contents/Frameworks/Sparkle.framework`。框架已提交在 `vendor/Sparkle.framework`（可用 `SPARKLE_FRAMEWORK_PATH` 覆盖路径），`bundle.sh` / `dev-restart.sh` 会自动拷贝它。生产应用包中的 `SUFeedURL` 默认是 `https://download.oh-my-tab.app/appcast.xml`，开发重启和开发发布脚本默认使用 `https://download.oh-my-tab.app/dev_release/appcast.xml`；也可用 `SPARKLE_FEED_URL` 覆盖。

`appcast.xml` 和更新归档由发布者自行上传到 R2。生成 appcast 时使用 Sparkle 的 Ed25519 私钥；打包时只需把对应公钥通过 `SPARKLE_PUBLIC_ED_KEY` 注入 `SUPublicEDKey`。Sparkle 比较 `CFBundleVersion`（build number），脚本默认用 UTC 时间戳生成它；需要可复现的测试时再设置 `SPARKLE_BUILD_VERSION`。`CFBundleShortVersionString` 仍负责展示给用户的版本。私钥不要提交到仓库、不要放进应用包，也不要上传到 R2。

发布说明必须放在版本对应的目录中：生产构建使用 `release_doc/<version>.md`，开发构建使用 `release_doc_dev/<version>.md`。对应版本的文件必须在构建前存在，否则对应构建会在编译前失败。发布脚本会将完整 Markdown 内嵌到新生成的 appcast item 中。

同一个 Markdown 文件可以包含多个语言区块。使用 `<!-- locale: en -->`、`<!-- locale: zh-Hans -->` 开始，并使用 `<!-- /locale -->` 结束。应用读取 Sparkle 的单个 `<description>` 后，会根据当前 UI locale 选择对应区块；找不到翻译时回退到英文，再回退到文件中的第一个区块。旧的单语言 Markdown 文件仍然兼容。

## 代码签名：为什么自签证书能让授权稳定

`bundle.sh` 优先用自签名身份 **`oh-my-tab-sign`** 签名，证书缺失或签名失败时退回 ad-hoc（`codesign -s -`）。

**原因：** ad-hoc 签名应用使用 CDHash 作为指定要求（designated requirement）。重新构建会改变这个哈希，macOS 可能把它视为新的 TCC 身份，并再次要求辅助功能授权（日志中可见 `Failed to match existing code requirement` / `errSecCSReqFailed`）。自签名证书提供了跨构建保持稳定的证书身份。

一次性创建证书（钥匙串访问）：

1. *钥匙串访问 → 证书助理 → 创建证书…*
2. 名称：`oh-my-tab-sign`，身份类型：**自签名根**，证书类型：**代码签名**。
3. 创建。（首次跑 `bundle.sh` 可能弹钥匙串访问提示——点「始终允许」。）

然后重新打包、重装、授予辅助功能一次。之后每次 rebuild 用的是同一个证书身份，无需再重新授权。若授权变陈旧（比如旧 ad-hoc 安装残留），清除：

```sh
tccutil reset Accessibility com.eacryo.oh-my-tab
```

**注意：** 自签名证书只稳定 TCC 身份，**不**满足 Gatekeeper 分发——别人安装后仍会看到「未识别开发者」，需要右键打开。若要通过 Gatekeeper 正常分发，需要使用付费的 Apple **Developer ID Application** 证书；有的话把 `scripts/bundle.sh` 里的 `SIGN_IDENTITY` 改成那个名字。

## 应用图标

应用图标（`AppIcon.icns`）由 `assets/Icon-Default-1024x1024@1x.png` 生成，打包进 `Contents/Resources/`。`assets/AppIcon.icns` 已提交进仓库，`bundle.sh` 直接使用它，因此贡献者构建 `.app` 时无需任何额外工具。

替换源 PNG 后重新生成：

```sh
./scripts/build-icon-from-png.sh   # 1024x1024 PNG -> 10 张 .iconset 尺寸(sips) -> assets/AppIcon.icns
```

只需 `iconutil`（Xcode CLT）；`sips` macOS 自带。生成后把新的 `assets/AppIcon.icns` 连同 `assets/Icon-Default-1024x1024@1x.png` 改动一起提交。

若存在 `assets/AppIcon.icon`（目录），`bundle.sh` 还会把它拷进 `Contents/Resources/`，用于 macOS 26+ 的 Liquid Glass 图标格式（系统优先于 `.icns`）。
