<p align="center">
  <img src="assets/Icon-512x512.png" width="120" height="120" alt="oh-my-tab">
</p>

<br />

<div align="center"><b>——&nbsp;&nbsp;&nbsp;面向 macOS 的桌面效率工具箱：窗口切换、鼠标控制、历史剪贴板与快捷操作&nbsp;&nbsp;&nbsp;——</b></div>

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

oh-my-tab 是一套 Rust 原生开发的 macOS 桌面效率工具箱，没有使用 Electron 或 Tauri，集合了 Windows / macOS 风格的窗口切换器、鼠标控制、历史剪贴板、窗口控制与快捷操作。

- <img height="14" src="docs/icons/image.svg"> **窗口切换器**：提供 Windows / macOS 风格的窗口切换体验；支持窗口级卡片、缩略图、MRU 顺序以及多屏场景。
- <img height="14" src="docs/icons/stack.svg"> **窗口控制**：通过 Option+方向键最大化、分屏、四分屏和最小化窗口，也可以用 Option+Shift+方向键将窗口移动到相邻显示器。
- <img height="14" src="docs/icons/zap.svg"> **快捷操作**：通过 Option+I 打开设置、Option+E 打开访达、Option+D 显示桌面、Option+L 锁屏，双击 Control 定位鼠标。
- <img height="14" src="docs/icons/key.svg"> **灵活导航**：支持 Tab、Shift+Tab、方向键和鼠标；快捷键可切换为 Option+Tab。
- <img height="14" src="docs/icons/history.svg"> **历史剪贴板**：支持文本、图片和文件条目，可搜索、置顶、删除、过期和选择性持久化。
- <img height="14" src="docs/icons/sliders.svg"> **鼠标控制**：可选的反向滚动、滚动模式、指针加速和按设备侧键映射。
- <img height="14" src="docs/icons/star.svg"> **外观定制**：支持浅色、深色和跟随系统主题，以及 Liquid Glass 样式、颜色、圆角和字体调整。
- <img height="14" src="docs/icons/gear.svg"> **设置中心**：统一管理外观、窗口切换、窗口控制、快捷操作、剪贴板、鼠标、开机启动和自动更新，修改后即时应用。
- <img height="14" src="docs/icons/globe.svg"> **多语言**：简体中文、繁体中文和英文，跟随系统语言。

## 通过 Homebrew 安装

如果只是使用、不需要从源码构建，可以通过 Homebrew Cask 安装预编译版本：

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

- **窗口切换**：按住 Command（或设置的 Option），按 Tab / Shift+Tab 或方向键选择窗口，松开修饰键完成切换。
- **历史剪贴板**：开启后按 **Option+V** 呼出；支持键盘和鼠标操作，点击浮窗外部关闭。
- **权限**：窗口切换需要辅助功能权限；缩略图需要屏幕录制权限。缺少屏幕录制权限时不影响窗口切换，只显示图标卡片。

剪贴板持久化默认关闭。开启后会以明文保存文本、文件名和图片数据，请不要在会复制密码或令牌的环境中开启。完整说明见[官方网站](https://oh-my-tab.app/)。

## 已知问题

- 后台 WebView 应用偶尔只能提供标题栏或白色内容区。应用会保留上一张有效缩略图，激活窗口后再尝试刷新。
- Telegram 全屏图片查看器属于特殊高层级浮窗，不会作为独立窗口展示；切换器显示 Telegram 主窗口。
- 某些编辑器或受保护窗口禁止共享画面，即使已授予屏幕录制权限也可能只有占位图或上一张有效画面，但窗口仍可切换。
- 应用启动时已有窗口时，初始排序不保证与原生 Cmd+Tab 完全一致；运行后会通过激活事件逐步修正窗口级 MRU。

仅影响源码开发的问题见[开发环境说明](docs/developer-notes.md)。

## 环境要求与权限

- macOS 13+ Apple Silicon。
- **辅助功能**权限：用于全局快捷键事件 tap 和 AX 窗口查询。在“系统设置 → 隐私与安全性 → 辅助功能”中授予。
- **屏幕录制**权限：用于窗口缩略图。在“系统设置 → 隐私与安全性 → 屏幕录制”中授予；画面帧只保存在内存中。

如果事件 tap 创建失败，快捷键通常是因为没有授予辅助功能权限。图标缓存位于 `~/Library/Caches/oh-my-tab-icons/`。

## 设置

所有功能选项推荐在设置窗口中修改，修改后会立即应用。设置页包含外观、窗口切换、窗口控制、快捷操作、剪贴板、鼠标、开机启动和自动更新等部分。

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

生产发布流程见[发布流程](docs/releasing.md)。

## 日志

默认日志路径为 `~/Library/Logs/oh-my-tab/oh-my-tab.log`，日志会自动轮转并保留最新的 5 个备份。需要排查问题时，可在设置窗口将日志级别切换为 Debug；日志不包含剪贴板内容和窗口画面。内存采样字段和开发调试说明见[开发环境说明](docs/developer-notes.md)。

## 致谢

**鼠标控制**功能参考并借鉴了 [LinearMouse](https://github.com/linearmouse/linearmouse)。**窗口切换器**的浮层设计、卡片式选中和 Liquid Glass 风格参考并借鉴了 [BetterCmdTab](https://github.com/rokartur/BetterCmdTab)。感谢两个项目及其作者。
