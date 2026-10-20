<p align="center">
  <img src="assets/Icon-512x512.png" width="120" height="120" alt="oh-my-tab">
</p>

<br />

<div align="center"><b>——&nbsp;&nbsp;&nbsp;rust开发的 macOS 窗口切换、历史剪贴板、鼠标控制等工具箱套件&nbsp;&nbsp;&nbsp;——</b></div>

<br />

<p align="center">
  <a href="https://github.com/eacryo/oh-my-tab/releases"><img src="https://img.shields.io/github/v/release/eacryo/oh-my-tab?style=for-the-badge" alt="GitHub release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-MIT-blue?style=for-the-badge" alt="MIT License"></a>
  <a href="https://github.com/eacryo/oh-my-tab"><img src="https://img.shields.io/badge/platform-macOS-black?style=for-the-badge" alt="macOS"></a>
</p>

<br />

<p align="center">
  简体中文 | <a href="README.md">English</a>
</p>

<p align="center">
  官方网站：<a href="https://oh-my-tab.app/">oh-my-tab.app</a>
</p>

<br />

oh-my-tab 是一款使用 Rust 原生开发的 macOS 菜单栏工具，以窗口切换为核心，集成了鼠标控制、历史剪贴板、窗口控制、按键显示和按键绑定，不依赖 Electron 或 Tauri 运行时，所有界面都使用原生绘制。

**所有可选功能都可按需启用并热插拔**：包含以下功能

- <img height="14" src="docs/icons/stack.svg"> **窗口切换器**：可选的Windows风格或MacOS风格的窗口切换器。
- <img height="14" src="docs/icons/copy.svg"> **历史剪贴板**：支持图片和文本的历史剪贴板，使用MacOS钥匙串加密持久化，重启不丢失。
- <img height="14" src="docs/icons/sliders.svg"> **鼠标控制**：可在不影响触摸板的前提下调整滚动速度、加速度等参数。
- <img height="14" src="docs/icons/zap.svg"> **按键绑定**: 将按键绑定到快捷操作上，或改键（改键功能开发中）
- <img height="14" src="docs/icons/eye.svg"> **按键显示**：显示当下按下的按键，在输入密码的时候不显示。

## 通过 Homebrew 安装

如果只想使用，不需要从源码构建，可以通过 Homebrew Cask 安装预编译版本：

```sh
brew install --cask eacryo/tap/oh-my-tab
```

它会接入 [homebrew-tap](https://github.com/eacryo/homebrew-tap) 仓库，把 `Oh-My-Tab.app` 安装到 `/Applications`。需要 macOS 13+ 和 Apple Silicon。

- 更新：`brew upgrade --cask oh-my-tab`
- 卸载：`brew uninstall --cask oh-my-tab`

## 截图

<div align="center"><img src="docs/pictures/main_window.png" width="640" alt="主界面"></div>

<div align="center"><img src="docs/videos/settings_page.gif" width="560" alt="设置页面演示"></div>

## 快速使用

- **窗口切换**：按住 Command（可在设置中改为 Option 或 Control），按 Tab / Shift+Tab 或方向键选择窗口，松开修饰键完成切换。使用 Control 时，系统默认把 Control+方向键分配给调度中心，需要先在“系统设置 → 键盘 → 键盘快捷键 → 调度中心”中关闭这些快捷键，方向键才能用于选择；Tab / Shift+Tab 不受影响。
- **面板动作**：浮窗打开期间，Command+W / M / H / Q 会关闭当前高亮窗口、最小化它（已最小化时则还原）、隐藏它所属的应用，或请求该应用退出。四项各有一个开关，默认全部关闭。
- **历史剪贴板**：开启后按 **Option+V** 呼出；支持键盘和鼠标操作，点击浮窗外部关闭。
- **按键绑定**：在**设置 → 按键绑定**里编辑快捷键列表。每一行都能改键、单独关闭或删除；选择器里还提供默认未绑定的动作。
- **权限**：窗口切换需要辅助功能权限；缩略图需要屏幕录制权限；历史剪贴板需要它首次使用时创建的钥匙串密钥。缺少屏幕录制权限时不影响窗口切换，只显示图标卡片。

## <img height="16" src="docs/icons/copy.svg">&nbsp;&nbsp;剪贴板历史

历史剪贴板默认关闭。按 **Option+V** 呼出后，可以使用方向键、Enter、Esc、Backspace 或鼠标操作；点击浮窗外部会将其关闭。按 **←** 可置顶或取消置顶当前条目。按 **→** 会在主浮窗旁展开详情面板，显示完整文本或较大的图片预览；面板会跟随 ↑/↓ 切换内容，并可通过 Esc、←、→ 或点击关闭。**Tab 依次切换筛选**（全部 / 文本 / 图片 / 链接 / 代码），**Cmd+F 聚焦搜索框**，**Cmd+Z 撤销删除**。历史记录包含**三种条目**：

| 种类 | 存储内容 | 粘贴行为 |
|---|---|---|
| **文本** | 复制的文本，存于内存 | 写回文本并合成 Cmd+V |
| **图片数据** | 在应用内复制的图片（如右键“复制图片”）：原始格式字节按内容哈希存磁盘缓存，内存只留降采样缩略图 | 按**原始 UTI** 写回字节并保留格式：JPG 仍为 JPG，GIF 动图仍为 GIF |
| **图片文件** | 在访达里复制的图片文件（Cmd+C）：复制时**读一次文件**算内容哈希并生成缩略图，字节随即丢弃——只保留路径 | 恢复 `public.file-url`（文件语义，同 Windows Win+V / Maccy）：Finder 原样复制文件，聊天应用附加文件；源文件已被删除时跳过粘贴 |

> **已知 v1 取舍**——每条历史只记录一种内容。一次复制**同时带文本和图片**时（例如从网页复制图片），只记录文本。**多文件复制与单个非图片文件不会进入历史。** 同一张图分别以图片数据和文件形式复制时会保留两条，因为两者的粘贴行为不同。文本按完整内容去重，图片按内容哈希去重。

详情面板的**保存按钮**会把条目存成文本文件或图片文件（有原始字节时存原始格式，否则存缓存的预览图）；旁边的分享按钮是预留入口，目前只有悬停反馈。历史默认最多 50 条（上限 100 条），未置顶条目默认 3 天后过期，置顶条目永不过期。

**使用条目默认会重排历史**（同 Maccy）。选中条目后按 Enter，会将其写回剪贴板；记录器会把它当作一次新的复制并移到最前。设置中的**“使用后移到最前”**可以关闭此行为（同 Windows Win+V）。

开启**“粘贴后删除条目”**后，Option+Enter 或 Option+点按会粘贴条目并将其从历史中移除。其从属选项**“同时删除系统剪贴板中对应条目”**会在短暂延迟后清除对应内容；如果期间出现了新的复制，则不执行清除。筛选行旁边的两个清除动作只清除你正在看的那些条目——当前选中的分类，或搜索时的结果——并且保留置顶条目，除非你选的是“全部清除”那一个。**“退出应用或关机后清除历史记录”**（默认关闭）会在有序退出时删除历史文件与缓存的图片数据——Command+Q、菜单里的“退出”、注销或关机——让这次会话不在磁盘上留下东西。

> **开启剪贴板历史期间，历史会加密写入磁盘。** 复制的文本、文件名与图片数据用 AES-256-GCM 封装，密钥是一把随机值、保存在本机登录钥匙串里，因此没有它这些文件读不出来。加密后的历史索引在 `~/.config/oh-my-tab/clipboard-history.enc`，封装过的图片原图与预览图在 `~/Library/Caches/oh-my-tab-clip-images/`。**它不覆盖的部分**：应用运行期间历史在内存中；元数据（文件名、大小与时间戳）可见；关闭功能之前已经同步或备份出去的副本仍留在原处；钥匙串项被其它工具删除后，已保存的历史将永久读不出来。拿不到钥匙串访问时功能不可用、什么都不记录（也没有内存回退）；设置页、历史面板与一条系统通知都会说明，其中两个**应用内界面**各有一个**“授予钥匙串访问”**动作，会要求输入一次钥匙串密码。如果你会复制密码或令牌，请保持功能关闭，或打开**“退出应用或关机后清除历史记录”**；崩溃或强制退出无法执行该清理。详见[官方网站](https://oh-my-tab.app/)。

## 已知问题

- **后台应用缩略图**：空白截图会在写入缓存前被丢弃，因此已有的有效缩略图会继续保留。挂起的 WebView 窗口会保留上一张有效画面；首次激活前显示占位图。切换到该窗口时会刷新预览。外观变化时也会刷新占位图，避免显示上一主题的画面。
- **最小化时若此前从未截过该窗口的图，卡片会一直显示占位图**：召唤期刷新会跳过最小化窗口，所以最小化时还没有缓存画面的窗口不会补拍；选中该卡片或激活该应用后才会补拍，之后保留。每个窗口每次启动最多出现一次；在原生标签窗口上最明显——切到一个还没截过图的标签后立刻最小化。
- Telegram 全屏图片查看器属于特殊高层级浮窗，不会作为独立窗口展示；切换器显示 Telegram 主窗口。
- 某些编辑器或受保护窗口禁止共享画面，即使已授予屏幕录制权限也可能只有占位图或上一张有效画面，但窗口仍可切换。
- 启动时若已有窗口，初始排序不保证与原生 Cmd+Tab 完全一致；运行后会通过激活事件逐步修正窗口级 MRU。
- **按内容网格吸附窗口的 App 最大化后会留一条细缝**：这类 App 把窗口尺寸取整到完整的内容单元，无法精确铺满可视区，因此某一边会留下约一个单元高（终端就是一行文本）的空缝，典型例子是终端。这是 App 自己的最大化结果——点绿钮、Option+点绿钮、双击标题栏都一样；请求略大的尺寸或使用系统窗口平铺也都不会改变它。不按网格吸附的 App（访达、浏览器、编辑器等大多数）会精确铺满。

仅影响源码开发的问题见[开发环境说明](docs/developer-notes.md)。

## 环境要求与权限

- macOS 13+ Apple Silicon。
- **辅助功能**权限：用于全局快捷键事件 tap 和 AX 窗口查询。在“系统设置 → 隐私与安全性 → 辅助功能”中授予。
- **屏幕录制**权限：用于窗口缩略图。在“系统设置 → 隐私与安全性 → 屏幕录制”中授予；画面帧只保存在内存中。
- **钥匙串访问**：历史剪贴板开启期间需要它首次使用时创建的密钥。

**从 0.2.2 或更早版本升级时请注意：**安装新版后，必须手动在“系统设置 → 隐私与安全性”中的“辅助功能”和“屏幕与系统音频录制”（部分 macOS 版本显示为“屏幕录制”）列表里分别选中旧版 Oh My Tab 并点“−”删除，再点“+”从“应用程序”添加新版 `Oh-My-Tab.app`，并开启这两项权限。仅关闭再打开原有开关不能完成重新授权。完成后重新启动 Oh My Tab。

如果事件 tap 启动失败，快捷键不会响应，通常是因为尚未授予辅助功能权限。

图标缓存位于 `~/Library/Caches/oh-my-tab-icons/`，按 bundle id 索引，包含 `.small.png` 小图与记录可执行文件 mtime 的 `.meta` 旁车文件，可从菜单清空。

菜单栏图标可在**设置 → 通用设置**里隐藏。隐藏后仍可从应用切换浮窗底栏的齿轮、剪贴板面板的齿轮进入设置，或从访达 / 聚焦再次打开应用（会重新打开设置窗口）。

## 设置

所有选项都在设置窗口中管理，大多数修改会立即生效。侧边栏共七页：**通用设置**（开机自启、菜单栏图标、外观、语言、日志、设置备份与还原）、**应用切换浮窗**、**鼠标控制**、**剪贴板历史**、**按键绑定**（窗口控制与快捷操作合为一页，可自行增删绑定）、**按键显示**、**关于**（版本、使用指南、权限与钥匙串状态、自动更新）。

**备份**：「导出设置」会把整份配置写成一个可移植的 TOML 文件；「导入设置」用这样的文件替换当前配置。本机的登录项、日志路径与剪贴板历史的存储设置不会被导入覆盖；来自更新版本的文件仍会导入当前版本能识别的部分。

## 从源码构建

开发者可以使用以下命令检查和运行项目：

```sh
cargo fmt
cargo check
cargo clippy
./scripts/cargo-test.sh
./scripts/dev-restart.sh
```

`scripts/cargo-test.sh` 是对 `cargo test` 的包装：过滤器匹配不到任何测试时 `cargo test` 仍会以 0 退出并打印 `running 0 tests ... ok`，这个包装会把「一个测试都没跑」判为失败；把过滤器作为参数传给它时这层保护依然生效。常规套件不依赖 GUI；CG/AX 冒烟测试标记为 `#[ignore]`，需要 GUI 会话和辅助功能权限（`cargo test -- --ignored`）。在 macOS 图形会话中，`scripts/e2e/run-all.sh` 会用真实事件流跑 A2 场景；会抢焦点的场景默认跳过，需要显式加 `--include-focus`。

`scripts/dev-restart.sh` 构建开发版 `.app`，**只在输入变化时**才重新组装与重签，然后交给用户级 `launchd` 启动。签名优先用 Apple 签发的身份（自签名证书是**唯一**允许的回退，**拒绝 ad-hoc**；Apple 身份签名失败即构建失败，除非显式 `--allow-signing-fallback`）。身份稳定，辅助功能、屏幕录制与钥匙串授权才能跨重建保留，因此没有改动的一次重启会复用现有包。启动是否成功由**运行中的进程自己**报告（pid、可执行路径、版本与 bundle）来校验，而不是读磁盘上的 plist；`--check` 会验证启动随后把它停掉，不留下任何进程。脚本在停任何东西之前先取得跨 worktree 的唯一构建/测试/启动事务，两个 worktree 因此不会争抢同一个开发实例；`--lock-wait=<秒>` 限定等待时间，且这份声明并不保护本次运行留下的应用。`--opt` 走 `dev-opt` profile（优化接近 release、保留 debug 断言），用于滚动/动画等体感与性能验证。脚本不认识的 `--flag[=value]` 会原样透传给应用，例如 `--open-settings=key-bindings` 或 `--no-onboarding`。布局 QA 夹具、调试期的布局断言和 GUI 冒烟测试见[开发环境说明](docs/developer-notes.md)。

如需生成可分发的 `.app` 与 `.dmg`，运行 `sh scripts/bundle.sh`：它会组装 `dist/Oh-My-Tab.app`、签名，并打包 `dist/Oh-My-Tab.dmg`（内含 `Applications` 软链，便于拖拽安装）与 Sparkle 更新包 `dist/Oh-My-Tab.zip`。产物放在 `dist/`（已 gitignore）而不是 `target/` 下，因此日志器会把它们当作正式构建（写文件日志而非 stdout）；运行 `.app` 才会启用开机自启（SMAppService）与文件日志。改动代码后需重新运行脚本。

两个渠道都从以 `Cargo.toml` 版本号命名的文件读取发布说明：开发渠道读 `release_doc_dev/<版本号>.md`（缺失或为空时 `scripts/dev-restart.sh` 会在构建前中止），正式流水线读 `release_doc/<版本号>.md`。文件可以包含多个语言块（`<!-- locale: en -->` 到 `<!-- /locale -->`），应用会显示与当前界面语言匹配的那一块。

完整发布流程（含 `--push`）、独立的开发渠道（bundle ID 为 `com.eacryo.oh-my-tab.dev`）、R2 上传、Sparkle 自动更新与代码签名见[发布流程](docs/releasing.md)。

## 日志

默认日志路径为 `~/Library/Logs/oh-my-tab/oh-my-tab.log`。当前文件达到 10 MB 时会轮转出 `oh-my-tab.log.1` 到 `oh-my-tab.log.5`，保留最新 5 份备份；每次启动都会写入一行会话标记，启动时还会清理 30 天前的旧式单次日志与过期备份。需要排查问题时，可在设置窗口将日志级别切换为 Debug；日志不包含剪贴板内容和窗口画面。内存采样字段和开发调试说明见[开发环境说明](docs/developer-notes.md)。

## 致谢

**鼠标控制**功能参考了 [LinearMouse](https://github.com/linearmouse/linearmouse)。**窗口切换器**的浮层设计、卡片式选中和 Liquid Glass 风格参考了 [BetterCmdTab](https://github.com/rokartur/BetterCmdTab)。两者都用 Rust 与 `objc2` FFI 实现。感谢两个项目及其作者。
