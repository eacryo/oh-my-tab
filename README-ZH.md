<p align="center">
  <img src="assets/Icon-512x512.png" width="120" height="120" alt="oh-my-tab">
</p>

<br />

<div align="center"><b>——&nbsp;&nbsp;&nbsp;原生 macOS 窗口切换、历史剪贴板、鼠标控制与快捷操作&nbsp;&nbsp;&nbsp;——</b></div>

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

oh-my-tab 是一款使用 Rust 原生开发的 macOS 工具，集成了窗口切换、鼠标控制、历史剪贴板、窗口控制和快捷操作，不依赖 Electron 或 Tauri 运行时。

- <img height="14" src="docs/icons/stack.svg"> **窗口切换器**：显示应用名与窗口标题，每个窗口一张卡片，支持多显示器。
- <img height="14" src="docs/icons/image.svg"> **窗口缩略图**：标题行下方显示通过私有 WindowServer API 截取的 16:10 窗口预览。应用会先显示内存缓存，再在后台刷新；单页时平衡排列，溢出时按 MRU 顺序填满并连续滚动。需要**屏幕录制**权限；未授权时回退为纯图标卡片。关闭缩略图后会释放内存中的窗口截图。
- <img height="14" src="docs/icons/history.svg"> **窗口级 MRU**：切换某个窗口时，该应用的其他窗口保持原有顺序。
- <img height="14" src="docs/icons/eye.svg"> **离屏与最小化窗口**：可选择是否在切换器中显示。
- <img height="14" src="docs/icons/key.svg"> **灵活导航**：支持 Tab、Shift+Tab、方向键和鼠标；快捷键可切换为 Option+Tab。
- <img height="14" src="docs/icons/tools.svg"> **窗口控制**：通过 Option+方向键最大化、分屏、四分屏和最小化窗口；Option+Shift+方向键可把窗口移到相邻显示器。
- <img height="14" src="docs/icons/zap.svg"> **快捷操作**：Option+I 打开设置、Option+E 打开访达、Option+D 显示桌面、Option+L 锁屏，双击 Control 定位鼠标。
- <img height="14" src="docs/icons/copy.svg"> **历史剪贴板**（可选）：文本、图片和文件条目——搜索、置顶、删除、过期与持久化（[剪贴板历史](#剪贴板历史)）。
- <img height="14" src="docs/icons/sliders.svg"> **鼠标控制**（可选）：滚动模式、反向滚动、按设备禁用指针加速，以及**侧键 → 快捷键映射**。
- <img height="14" src="docs/icons/star.svg"> **外观定制**：浅色、深色或跟随系统主题，以及 Liquid Glass 样式（`NSGlassEffectView`，旧系统回退 `NSVisualEffectView`）、色调、圆角和字号。
- <img height="14" src="docs/icons/gear.svg"> **设置**：统一管理外观与各项功能，大多数修改会立即生效。
- <img height="14" src="docs/icons/globe.svg"> **内置多语言支持**：简体中文、繁体中文和英文，可跟随系统语言。
- <img height="14" src="docs/icons/package.svg"> **Rust 原生应用**：缩略图使用有明确上限的内存缓存，不依赖 Electron 或 Tauri 运行时。
- <img height="14" src="docs/icons/note.svg"> **滚动日志**：旧备份和旧版日志超过 30 天后清理（[日志](#日志)）。

## 通过 Homebrew 安装

如果只想使用，不需要从源码构建，可以通过 Homebrew Cask 安装预编译版本：

```sh
brew install --cask eacryo/tap/oh-my-tab
```

需要 macOS 13+ 和 Apple Silicon。更新与卸载：

```sh
brew upgrade --cask oh-my-tab
brew uninstall --cask oh-my-tab
```

## 截图

<div align="center"><img src="docs/pictures/main_window.png" width="640" alt="主界面"></div>

<div align="center"><img src="docs/videos/settings_page.gif" width="560" alt="设置页面演示"></div>

## 快速使用

- **窗口切换**：按住 Command（可在设置中改为 Option），按 Tab / Shift+Tab 或方向键选择窗口，松开修饰键完成切换。
- **历史剪贴板**：开启后按 **Option+V** 呼出；支持键盘和鼠标操作，点击浮窗外部关闭。
- **权限**：窗口切换需要辅助功能权限；缩略图需要屏幕录制权限。缺少屏幕录制权限时不影响窗口切换，只显示图标卡片。

剪贴板持久化默认关闭。开启后会以未加密形式保存文本、文件名和图片数据；如果你会复制密码或令牌，请不要开启。完整说明见[官方网站](https://oh-my-tab.app/)。

## <img height="16" src="docs/icons/copy.svg">&nbsp;&nbsp;剪贴板历史

历史剪贴板默认关闭。按 **Option+V** 呼出后，可以使用方向键、Enter、Esc、Backspace 或鼠标操作；点击浮窗外部会将其关闭。按 **←** 可置顶或取消置顶当前条目。按 **→** 会在主浮窗旁展开详情面板，显示完整文本或较大的图片预览；面板会跟随 ↑/↓ 切换内容，并可通过 Esc、←、→ 或点击关闭。历史记录包含**三种条目**：

| 种类 | 存储内容 | 粘贴行为 |
|---|---|---|
| **文本** | 复制的文本，存于内存 | 写回文本并合成 Cmd+V |
| **图片数据** | 在应用内复制的图片（如右键“复制图片”）：原始格式字节按内容哈希存磁盘缓存，内存只留降采样缩略图 | 按**原始 UTI** 写回字节并保留格式：JPG 仍为 JPG，GIF 动图仍为 GIF |
| **图片文件** | 在访达里复制的图片文件（Cmd+C）：复制时**读一次文件**算内容哈希并生成缩略图，字节随即丢弃——只保留路径 | 恢复 `public.file-url`（文件语义，同 Windows Win+V / Maccy）：Finder 原样复制文件，聊天应用附加文件；源文件已被删除时跳过粘贴 |

> **已知 v1 取舍**——每条历史只记录一种内容。一次复制**同时带文本和图片**时（例如从网页复制图片），只记录文本。**多文件复制与单个非图片文件不会进入历史。** 同一张图分别以图片数据和文件形式复制时会保留两条，因为两者的粘贴行为不同。文本按完整内容去重，图片按内容哈希去重。

**使用条目默认会重排历史**（同 Maccy）。选中条目后按 Enter，会将其写回剪贴板；记录器会把它当作一次新的复制并移到最前。设置中的**“使用后移到最前”**可以关闭此行为（同 Windows Win+V）。

开启**“粘贴后删除条目”**后，Option+Enter 或 Option+点按会粘贴条目并将其从历史中移除。其从属选项**“同时删除系统剪贴板中对应条目”**会在短暂延迟后清除对应内容；如果期间出现了新的复制，则不执行清除。“清空历史”会保留置顶条目。**“保存剪贴板历史记录到磁盘”**可让历史在重启后继续保留，隐私风险见上方「快速使用」中的说明。

## 已知问题

- **后台应用缩略图**：空白截图会在写入缓存前被丢弃，因此已有的有效缩略图会继续保留。挂起的 WebView 窗口会保留上一张有效画面；首次激活前显示占位图。切换到该窗口时会刷新预览。外观变化时也会刷新占位图，避免显示上一主题的画面。
- **最小化时若此前从未截过该窗口的图，卡片会一直显示占位图**：召唤期刷新会跳过最小化窗口，所以最小化时还没有缓存画面的窗口不会补拍；选中该卡片或激活该应用后才会补拍，之后保留。每个窗口每次启动最多出现一次；在原生标签窗口上最明显——切到一个还没截过图的标签后立刻最小化。
- Telegram 全屏图片查看器属于特殊高层级浮窗，不会作为独立窗口展示；切换器显示 Telegram 主窗口。
- 某些编辑器或受保护窗口禁止共享画面，即使已授予屏幕录制权限也可能只有占位图或上一张有效画面，但窗口仍可切换。
- 启动时若已有窗口，初始排序不保证与原生 Cmd+Tab 完全一致；运行后会通过激活事件逐步修正窗口级 MRU。

仅影响源码开发的问题见[开发环境说明](docs/developer-notes.md)。

## 环境要求与权限

- macOS 13+ Apple Silicon。
- **辅助功能**权限：用于全局快捷键事件 tap 和 AX 窗口查询。在“系统设置 → 隐私与安全性 → 辅助功能”中授予。
- **屏幕录制**权限：用于窗口缩略图。在“系统设置 → 隐私与安全性 → 屏幕录制”中授予；画面帧只保存在内存中。

**从 0.2.2 或更早版本升级时请注意：**安装新版后，必须手动在“系统设置 → 隐私与安全性”中的“辅助功能”和“屏幕与系统音频录制”（部分 macOS 版本显示为“屏幕录制”）列表里分别选中旧版 Oh My Tab 并点“−”删除，再点“+”从“应用程序”添加新版 `Oh-My-Tab.app`，并开启这两项权限。仅关闭再打开原有开关不能完成重新授权。完成后重新启动 Oh My Tab。

如果事件 tap 启动失败，快捷键不会响应，通常是因为尚未授予辅助功能权限。图标缓存位于 `~/Library/Caches/oh-my-tab-icons/`。

## 设置

所有选项都在设置窗口中管理，大多数修改会立即生效。设置页包含外观、窗口切换、窗口控制、快捷操作、剪贴板、鼠标、开机启动和自动更新等部分。

剪贴板持久化的行为和隐私说明见上方说明及[官方网站](https://oh-my-tab.app/)。

## 从源码构建

开发者可以使用以下命令检查和运行项目：

```sh
cargo fmt
cargo check
cargo clippy
cargo test
./scripts/dev-restart.sh
```

`scripts/dev-restart.sh` 会构建并组装独立签名的开发版 `.app`，再交给用户级 `launchd` 启动，这样辅助功能与屏幕录制授权会绑定到开发版 bundle，启动的也是本次构建生成的二进制。布局 QA 夹具、调试期的布局断言和 GUI 冒烟测试见[开发环境说明](docs/developer-notes.md)。

如需生成可分发的 `.app` 与 `.dmg`，运行 `sh scripts/bundle.sh`。完整发布流程（含 `--push`）、开发通道、Sparkle 更新与代码签名见[发布流程](docs/releasing.md)。

## 日志

默认日志路径为 `~/Library/Logs/oh-my-tab/oh-my-tab.log`，日志会自动轮转并保留最新的 5 个备份。需要排查问题时，可在设置窗口将日志级别切换为 Debug；日志不包含剪贴板内容和窗口画面。内存采样字段和开发调试说明见[开发环境说明](docs/developer-notes.md)。

## 致谢

**鼠标控制**功能参考了 [LinearMouse](https://github.com/linearmouse/linearmouse)。**窗口切换器**的浮层设计、卡片式选中和 Liquid Glass 风格参考了 [BetterCmdTab](https://github.com/rokartur/BetterCmdTab)。感谢两个项目及其作者。
