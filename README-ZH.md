<p align="center">
  <img src="assets/Icon-512x512.png" width="120" height="120" alt="oh-my-tab">
</p>

<br />

<div align="center"><b>——&nbsp;&nbsp;&nbsp;以 Windows 的方式使用你的 MacBook：带缩略图的窗口切换、历史剪贴板与鼠标反向滚动&nbsp;&nbsp;&nbsp;——</b></div>

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
  官方网站：<a href="https://oh-my-tab.app/">https://oh-my-tab.app/</a>
</p>

<br />

oh-my-tab 是一个 macOS 窗口切换器,补充系统 Cmd+Tab 的使用体验:它以**菜单栏辅助应用**方式运行(无 Dock 图标),拦截全局快捷键(默认 **Command+Tab**,可切换为 Option+Tab),弹出一个 **Liquid Glass** 风格的浮层,以卡片形式展示当前打开的窗口,松开快捷键时把选中的窗口抬到最前(SkyLight 私有 API + AX)。

它是纯 Rust 通过 `objc2` FFI 直接调用 AppKit / CoreGraphics / ApplicationServices -- 没有 Swift 桥接,也没有 Rust UI 框架。

- <img height="14" src="docs/icons/stack.svg"> **原生切换增强**:显示应用名与窗口标题,同一应用的多个窗口分列卡片。
- <img height="14" src="docs/icons/key.svg"> **键盘导航**:按下 Command/Option 后用 Tab、Shift+Tab、方向键或鼠标选择窗口。
- <img height="14" src="docs/icons/zap.svg"> **轻量**:纯 Rust——当前 arm64 release 可执行文件约 3.7MB;实际观测中,开启缩略图和持久化剪贴板的典型活动会话,启动后 footprint 约 80MB,缩略图预热后约 140–160MB。内存缩略图缓存上限约 64MB,无 Electron/Tauri。
- <img height="14" src="docs/icons/star.svg"> **Liquid Glass**:浮层效果(仅 macOS 26;旧版回退 `NSVisualEffectView`)。
- <img height="14" src="docs/icons/image.svg"> **窗口缩略图**:标题行在上方、下方为 16:10 实时预览(经私有 WindowServer API 截取,仅存内存)——缓存旧帧即时显示、后台异步刷新;单页时平衡排列,溢出时优先填满 MRU 前面的行并连续滚动。需要**屏幕录制**权限——未授权时自动回退为纯图标卡片。关闭缩略图后会立即释放内存中的窗口截图。
- <img height="14" src="docs/icons/history.svg"> **窗口级 MRU**:切到某个窗口不会把该应用的其他窗口一起带前。
- <img height="14" src="docs/icons/eye.svg"> **完整窗口可见**:所有真实窗口,含离屏与最小化(可开关)。
- <img height="14" src="docs/icons/gear.svg"> **TOML 热重载**:配置经校验,菜单一键生效。
- <img height="14" src="docs/icons/globe.svg"> **零依赖国际化**:英文/简中/繁中,实时跟随系统语言。
- <img height="14" src="docs/icons/note.svg"> **每次启动一份日志**:保留 30 天([日志](#日志))。
- <img height="14" src="docs/icons/sliders.svg"> **鼠标控制**(可选):滚动模式/反向、按设备加速、**侧键→快捷键映射**([配置](#配置))。
- <img height="14" src="docs/icons/copy.svg"> **剪贴板历史**(可选):文本/图片/文件复制——搜索、置顶、删除、过期、持久化([剪贴板历史](#剪贴板历史))。

<br />

## <img height="16" src="docs/icons/download.svg">&nbsp;&nbsp;通过 Homebrew 安装

如果只是使用、不需要从源码构建,可以通过 Homebrew Cask 安装预编译版本:

> ```sh
> brew install --cask eacryo/tap/oh-my-tab
> ```

这会自动添加 [homebrew-tap](https://github.com/eacryo/homebrew-tap) 仓库,把 `Oh-My-Tab.app` 装进 `/Applications`。需要 macOS 13+ 的 Apple Silicon 机型。

- 更新:`brew upgrade --cask oh-my-tab`
- 卸载:`brew uninstall --cask oh-my-tab`

## <img height="16" src="docs/icons/image.svg">&nbsp;&nbsp;截图

<p style="text-align: center;"><img src="docs/pictures/main_window.png" width="640" alt="主界面"></p>

<table style="border-collapse: collapse;">
  <tr>
    <td style="border: none; padding: 4px;"><img src="docs/pictures/settings.png" style="width: 100%;" alt="设置"></td>
    <td style="border: none; padding: 4px;"><img src="docs/pictures/mouse.png" style="width: 100%;" alt="鼠标控制"></td>
    <td style="border: none; padding: 4px;"><img src="docs/pictures/clipboard.png" style="width: 100%;" alt="剪贴板历史"></td>
  </tr>
</table>

## <img height="16" src="docs/icons/copy.svg">&nbsp;&nbsp;剪贴板历史

可选功能(默认关闭)。**Option+V** 呼出,方向键 / Enter / Esc / Backspace 或点击操作,点击外部关闭。附加按键:**← 置顶/取消置顶**选中条目;**→ 在主浮窗旁展开详情面板**——完整未截断的文本或图片大图(跟随 ↑/↓ 浏览实时切换;Esc、←、→ 或点击面板关闭)。历史记录**三种条目**:

| 种类 | 存储内容 | 粘贴行为 |
|---|---|---|
| **文本** | 复制的文本,存于内存 | 写回文本并合成 Cmd+V |
| **图片数据** | 在应用内复制的图片(如右键"复制图片"):原始格式字节按内容哈希存磁盘缓存,内存只留降采样缩略图 | 按**原始 UTI** 写回字节——JPG 粘回 JPG、GIF 动图粘回动图,绝不重编码成 PNG |
| **图片文件** | 在访达里复制的图片文件(Cmd+C):复制时**读一次文件**算内容哈希 + 生成缩略图,字节随即丢弃——只保留路径 | 恢复 `public.file-url`(文件语义,同 Windows Win+V / Maccy):Finder 原样复制文件、聊天应用附加文件;源文件已被删除时该条目粘贴跳过 |

> **已知 v1 取舍**——每条历史只记录一种内容:一次复制**同时带文本和图片**时(例如从网页复制图片)只记录文本;**多文件复制与单个非图片文件完全不记录**。同一张图既复制过图片又复制过文件,则保留两条(两者粘贴语义不同)。去重按类进行:文本按全文精确匹配,图片按内容哈希。

**使用条目默认会重排历史**(同 Maccy):选中条目回车 = 把它写回剪贴板,记录器视为"又一次复制"并移到最前。设置里的**"使用后移到最前"**开关可关闭此行为(同 Windows Win+V)。浮窗"清除全部"保留置顶条目。可选的**"保存剪贴板历史记录到磁盘"**开关把历史持久化、重启不丢——隐私风险见[配置](#配置)中的说明。

## <img height="16" src="docs/icons/alert.svg">&nbsp;&nbsp;已知问题

**部分后台应用的窗口缩略图可能暂时白屏**:WindowServer 只能截取应用当前提供的窗口画面。长期处于后台休眠状态的 WebView 应用(例如 Clash Verge Rev)可能只返回标题栏和白色内容区,尤其是在 oh-my-tab 刚启动、内存缩略图缓存为空时。激活该应用并等待内容重新绘制后,后续捕获可以恢复预览。

**Telegram 全屏查看图片时不会显示图片查看器的独立缩略图**:Telegram 的媒体查看器是位于普通窗口之上的特殊高层级浮窗。为避免把它误识别成一个可切换窗口,oh-my-tab 会将其排除在窗口列表和缩略图采集之外,因此此时切换器显示的是 Telegram 主窗口的缩略图,而不是当前查看的图片。这是当前版本的已知限制。

**如果应用启动时已有窗口存在,初始排序不保证与原生 Cmd+Tab 完全一致**:oh-my-tab 会先使用 WindowServer 的前后层级顺序建立一个合理的初始近似;启动后再通过实时激活事件逐步修正窗口级 MRU。

**设备识别的问题**:部分特殊鼠标(如 ATK A9 SE,星闪设备)主用途会被系统报错、在系统设置里显示为键盘;设备下拉框按设备完整的 HID usage pairs 匹配,所以仍能识别并配置。HID 描述符虚报指针用途的蓝牙键盘会按蓝牙 GAP appearance 被排除。下拉框插拔/重连实时刷新(插拔事件有防抖但不会丢弃)。

仅影响源码开发的问题(`cargo run` 下的图标失效、调试器下的鼠标控制失效)收录在 [docs/developer-notes.md](docs/developer-notes.md)。

## <img height="16" src="docs/icons/tools.svg">&nbsp;&nbsp;环境要求

- macOS(在 macOS 26 上开发;旧版本通过 `NSVisualEffectView` 回退支持,但不保证其可用性)。
- 已授予应用 **辅助功能** 权限。

## <img height="16" src="docs/icons/terminal.svg">&nbsp;&nbsp;构建与运行

**前置条件:** Rust 稳定版工具链、Xcode Command Line Tools(`xcode-select --install`)、macOS 13+。运行时需要辅助功能权限(见下方「权限与运行须知」)。

### 开发

> ```sh
> cargo check       # 快速类型检查
> cargo run         # 构建并运行(会接管全局快捷键)
> cargo clippy      # 可用,未接入 CI
> cargo test        # 单元测试;加 -- --ignored 运行 CG/AX 冒烟测试
> ```

`cargo run` 以**开发模式**跑裸二进制:日志同时输出到 stdout 和日志文件,开机自启不生效(SMAppService 需要 `.app` bundle)。单元测试默认即可运行(无 GUI 依赖);剪贴板图片/历史测试夹具使用按进程、按线程隔离的系统临时目录,不写用户缓存。少量**冒烟测试**标记为 `#[ignore]` —— 它们调用真实的 CG/AX 栈,需要 GUI 会话和辅助功能权限(用 `cargo test -- --ignored` 运行)。

### Release `.app` + `.dmg`

> ```sh
> sh scripts/bundle.sh        # cargo build --release -> .app -> 签名 -> .dmg + Sparkle .zip
> open dist/Oh-My-Tab.dmg     # 安装:把 Oh-My-Tab 拖到 Applications
> ```

`bundle.sh` 组装 `dist/Oh-My-Tab.app`(release 二进制、`Info.plist` 和应用图标资源)、做签名,再打成 `dist/Oh-My-Tab.dmg`(含 `Applications` 软链,拖拽安装)。两个产物都在 `dist/`(已 gitignore),放在 `target/` 之外,这样 logger 把它识别为生产态(写文件日志,而非 stdout)。运行 `.app` 是开机自启(SMAppService)和文件日志的前提;`.dmg` 用于分发。代码改动后需要重新跑该脚本(bundle 在构建时拷贝 release 二进制);脚本会自定位仓库根,可从任意目录运行。
`bundle.sh` 现在同时生成 `dist/Oh-My-Tab.dmg` 和 Sparkle 使用的 `dist/Oh-My-Tab.zip`。`release.sh` 默认只在本地构建；只有显式传入 `--push` 才会使用 R2 S3 API 上传，避免普通构建误发布：

> ```sh
> sh scripts/release.sh                 # 只构建，不访问 R2
> sh scripts/release.sh --push           # 上传 ZIP、DMG，最后上传 dist/appcast.xml
> sh scripts/release.sh --push --dry-run # 只打印上传计划
> ```

开发更新通道与生产完全隔离（Bundle ID 为 `com.eacryo.oh-my-tab.dev`，Feed 为
`https://download.oh-my-tab.app/dev_release/appcast.xml`，R2 前缀为 `dev_release`）：

> ```sh
> sh scripts/release-dev.sh                 # 只构建开发包
> sh scripts/release-dev.sh --push          # 上传开发包和 dist/appcast-dev.xml
> sh scripts/release-dev.sh --push --dry-run
> ```

要发布已签名的 appcast，先不带 `--push` 运行开发脚本，再从这次生成的 ZIP 生成 appcast，
保存为 `dist/appcast-dev.xml`，最后运行 `sh scripts/release-dev.sh --push`。如果三个开发产物
仍然存在，`--push` 会复用它们；只有明确需要重新构建时才设置 `RELEASE_REBUILD=1`。

`--push` 要求你准备好对应的 appcast（生产为 `dist/appcast.xml`，开发为
`dist/appcast-dev.xml`），并读取 `R2_ACCESS_KEY_ID`、`R2_SECRET_ACCESS_KEY`、`R2_BUCKET` 以及
`R2_ENDPOINT`（或 `R2_ACCOUNT_ID`）等环境变量。上传工具位于 `tools/r2-publisher`，不会把
密钥写入应用或命令行参数。
上传请求始终发送到环境变量配置的 R2 S3 endpoint；`download.oh-my-tab.app` 只作为 Sparkle
链接中的公开 HTTPS 地址。`R2_PUBLIC_BASE_URL` 只会改变显示/公开 URL，不会改变实际上传目标。

### Sparkle 自动更新

About 页的开关和“检查更新”按钮已经接入 Sparkle 2。更新器按标准方式读取 `.app` 内的
`SUFeedURL`：生产通道为 `https://download.oh-my-tab.app/appcast.xml`，开发通道为
`https://download.oh-my-tab.app/dev_release/appcast.xml`。仓库不包含这两个 appcast、更新压缩包或
Sparkle 私钥，这些发布材料由你后续分别放到对应的 R2 路径。

把 Sparkle 2 的 `Sparkle.framework` 放到 `vendor/Sparkle.framework`（或设置 `SPARKLE_FRAMEWORK_PATH`），`scripts/bundle.sh` 和 `scripts/dev-restart.sh` 会自动复制到 `Contents/Frameworks`。没有框架时应用仍可启动，About 页会提示该构建未包含 Sparkle。脚本默认用 UTC 时间戳生成 `CFBundleVersion`，也可用 `SPARKLE_BUILD_VERSION` 指定固定值；发布时可通过 `SPARKLE_FEED_URL` 覆盖 feed 地址，通过 `SPARKLE_PUBLIC_ED_KEY` 写入 appcast 验签公钥。私钥不要放进仓库或 R2。

构建要求需要区分：`cargo build`、`cargo check` 和测试套件不依赖 Sparkle；要让打包后的应用真正具备更新检查功能，必须提供上面的 framework。生成 appcast 还需要 Sparkle 发行包中的 `bin/generate_keys` 和 `bin/generate_appcast`。framework 已按设计加入 Git 忽略，因此每台开发机或 CI 都要自行准备固定版本的 Sparkle 发行包。

作为参考,本次 arm64 构建测得:未签名 release 可执行文件 3,726,864 字节,签名后 `.app` 内普通文件共 4,524,594 字节,生成的 UDZO `.dmg` 为 3,080,950 字节。这些是当前构建产物的实测值,不是固定保证;源码、Rust 工具链和签名方式都可能改变大小。

### 代码签名

`bundle.sh` 优先用自签名身份 **`oh-my-tab-sign`** 签名,没有该证书时退回 ad-hoc(`codesign -s -`)。**强烈建议**一次性创建该证书 -- 它能让辅助功能授权在反复 rebuild 后仍然稳定(ad-hoc 签名每次 rebuild 都变,授权也随之失效):

1. *钥匙串访问 -> 证书助理 -> 创建证书…*
2. 名称:`oh-my-tab-sign`,身份类型:**自签名根**,证书类型:**代码签名**。
3. 创建,然后重新打包、重装。(首次跑 `bundle.sh` 可能弹钥匙串访问提示 -- 点「始终允许」。)

若授权变陈旧(比如旧 ad-hoc 安装残留),清除:`tccutil reset Accessibility com.eacryo.oh-my-tab`。注意:自签名证书只稳定 TCC 身份,**不**满足 Gatekeeper 分发 -- 别人装仍是「未识别开发者」,需右键打开;彻底解决需付费的 Apple Developer ID 证书(把 `scripts/bundle.sh` 里的 `SIGN_IDENTITY` 改成那个名字)。

完整发布流水线(Homebrew cask 生成、签名原理、图标再生成)见 [docs/releasing.md](docs/releasing.md);架构说明见 [AGENTS.md](AGENTS.md)。

## <img height="16" src="docs/icons/shield-lock.svg">&nbsp;&nbsp;权限与运行须知

- 应用需要 **辅助功能** 权限(`AXIsProcessTrusted`),全局按键事件 tap 和 AX 窗口查询都依赖它。在 *系统设置 -> 隐私与安全性 -> 辅助功能* 中授予。重新编译出的二进制需要重新授权 -- 除非用稳定身份签名(见[代码签名](#代码签名)),此时授权跨 rebuild 持续有效。
- **窗口缩略图**还需要**屏幕录制**权限(系统设置 -> 隐私与安全性 -> 屏幕录制;使用与 DockDoor/AltTab 相同的私有 WindowServer 截取 API)。未授权时切换器静默保持纯图标渲染;稍后授予权限后无需重启即可恢复缩略图捕获。画面帧**仅存内存**——绝不落盘。
- 如果事件 tap 创建失败,应用会打印一条错误,快捷键静默失效 -- 几乎总是辅助功能权限没给。
- 运行时配置:`~/.config/oh-my-tab/config.toml`(首次运行自动按默认值创建)。
- 图标缓存:`~/Library/Caches/oh-my-tab-icons/{bundle-id}.png`(按应用 bundle id 索引,配 `.meta` mtime sidecar;可从菜单清空)。

## <img height="16" src="docs/icons/gear.svg">&nbsp;&nbsp;配置

`~/.config/oh-my-tab/config.toml` -- 首次运行按默认值自动创建。加载是**逐字段容错**的,不是全有或全无:非法字段回退默认值(记日志,绝不致命)。可从菜单(*Reload Config*)运行时热重载,同时重新应用主题并刷新浮窗。常用键:

```toml
[appearance]
theme = "light"          # "dark" | "light" | "auto"
glass_style = "regular"  # "regular" | "clear"
glass_tint = "eeeeee66"  # RRGGBBAA — 默认玻璃浮窗 tint 颜色
corner_radius = 32.0

[layout]
thumbnails_enabled = true  # 卡片窗口缩略图;关闭 = 纯图标卡片
card_text_size = 16.0      # 卡片文字大小(点);缩略图模式下左侧图标也按比例缩放,范围 8..=24

[keyboard]
modifier = "command"     # "option"(Option+Tab)| "command"(Cmd+Tab)

[i18n]
locale = "auto"          # "auto" | "en" | "zh-Hans" | "zh-Hant"

[windows]
enabled = true            # 应用切换总开关(关闭后 Cmd+Tab 透传给系统原生切换器)
show_minimized = false    # 在浮层中显示最小化窗口
overlay_position = "active_window"  # "active_window"(跟随激活窗口所在屏幕)| "main"(固定主屏)

[logging]
level = "info"           # "debug" | "info"
file_path = ""           # 空 = 默认带时间戳路径;见下方「日志」

[startup]
launch_at_login = false  # 开机自启(需以 .app 方式运行;macOS 13+)

[updates]
automatically_check = true  # 通过 Sparkle 自动检查更新

[clipboard]
enabled = false          # 剪贴板历史总开关(默认关闭)
max_entries = 50         # 历史最大条数(1..=100)
persist = false          # 把历史保存到磁盘,重启不丢(隐私风险见下方说明)
auto_expire_days = 3     # 非置顶条目超过 N 天自动过期(内存与磁盘同时生效);0 = 关闭
pin_follow_selection = true # 置顶/取消置顶后选中项是否跟随该条目(关闭 = 保持当前位置)
move_used_to_top = true  # 粘贴后把用过的条目移到最前(关闭 = 粘贴不重排历史,同 Win+V)
picker_position = "main" # 剪贴板浮窗位置:"mouse"(跟随鼠标)| "main"(主屏居中)
show_source_app = false  # 条目行是否显示来源应用名(来源始终会记录)

[mouse]
enabled = false          # 鼠标控制总开关(控制 event tap)

# 第一个不含 device_* 字段的档是默认层(所有鼠标)。
# 后续带 device_* 的档按 VID/PID 匹配具体设备,逐字段覆盖默认层。
# 生效配置 = 默认层合并匹配到的设备层。
[[mouse.profiles]]
reverse_scroll = false   # 相对系统方向反转滚动
scroll_mode = "default"  # "default" | "line"(每 tick 固定行数)
line_count = 3           # "line" 模式每 tick 行数(1..=10)
button_mappings_enabled = true  # 该设备档的映射总开关(默认 true)

[mouse.profiles.pointer]
disable_acceleration = false  # 禁用系统指针加速(线性移动)

# 按键映射:把中键/侧键(按钮号 >= 2)绑定成动作(快捷键 / 系统功能 / 禁用)。
# 左键(0)/右键(1)不允许绑定,防止把自己锁死。按钮号:2 = 中键,3 = 后退,
# 4 = 前进,5+ = 其他侧键/宏键(不同鼠标可能不同,按设备分别配置)。
# 值可以是快捷键("cmd+shift+v")、系统动作名("missioncontrol"/"launchpad"/
# "showdesktop"/"appexpose",经 Dock 私有通知触发,不受系统快捷键占用影响)或
# "none"(吞掉按键,按钮失效)。绑定 Cmd+Tab / Option+V 会打开**我们自己的**
# 浮窗/剪贴板(内部派发,不走合成事件)。
[mouse.profiles.button_mappings]
"3" = "cmd+shift+v"
"4" = "alt+tab"

# 按设备覆盖示例(Logitech MCHOSE G3 V2):
[[mouse.profiles]]
device_vendor_id = 10007
device_product_id = 12976
reverse_scroll = true
scroll_mode = "line"
line_count = 3
```

高级段落 `[colors]` 与 `[fonts]`(按主题的卡片文字颜色与字号)会连同默认值一起写进自动创建的配置文件——直接在那儿改即可。

> **剪贴板历史持久化与隐私** — 开启 `persist`(或设置里的"保存剪贴板历史记录"开关)会把
> 剪贴板历史——复制的文本、文件名、图片字节——写入磁盘,重启应用后仍然保留:
>
> - `~/.config/oh-my-tab/clipboard-history.toml`(文本、文件名、来源与元数据;权限 600)
> - `~/Library/Caches/oh-my-tab-clip-images/`(图片字节与预览,按内容哈希命名)
>
> 这些文件以**明文/不加密**形式存放。历史文件**任何以你的用户身份运行的应用都能读取**
> (600 权限只能防其他用户),因此如果你会复制密码、令牌等敏感内容,**不要**开启该开关。
> 作为防线:带有 nspasteboard.org "Securing Copy" 标准标记
> (`org.nspasteboard.ConcealedType` / `TransientType` / `AutoGeneratedType`,以及
> 1Password 标记 `com.agilebits.onepassword`)的内容**绝不会被记录**——密码管理器复制
> 密码时会打上这些标记,这类内容从一开始就不会进入历史(内存和磁盘都不会)。持久化默认关闭。

鼠标设置也可以在设置窗口中调整(用**设备下拉框**选中某款已连接的鼠标,编辑它那一层)。按键映射区显示已绑定的行(按钮名 + 动作描述 + 键帽),**点「编辑」弹出编辑面板**(LinearMouse 同款):面板里录制触发侧键、选动作类型(默认/无/自定义按键/Mission Control/Launchpad/显示桌面/App Expose)、自定义按键时录组合键,确认后写入。切换 `mouse.enabled` **立即生效**,无需重启应用。

## <img height="16" src="docs/icons/note.svg">&nbsp;&nbsp;日志

- **输出目标**:`cargo run`(开发态)-> 同时输出到 stdout **和**日志文件;打包的 `.app`(生产态)-> 只写日志文件。
- **默认文件路径**:`~/Library/Logs/oh-my-tab/oh-my-tab-<启动时间戳>.log` -- **每次启动一个文件**。启动时自动删除默认目录中 mtime 超过 30 天的旧日志(单次长时间运行内无按大小轮转)。
- **自定义路径**:`[logging] file_path`(直接编辑 `config.toml`,不在设置界面暴露)。用户指定路径**原样**使用、append 模式——不加时间戳、不做任何清理,轮转与保留由你自己负责。
- **内存诊断**:启动约 60 秒后先记录一行,之后每 5 分钟记录一行 `[mem]`,包含当前功能画像(`mouse:on|off`、`thumbs:on|off`、`clipboard:off|memory|persistent`)、进程 footprint/RSS、采样期间 footprint 峰值、线程数,以及缩略图/剪贴板/窗口账本的估算值。`footprint` 是 macOS 的 physical footprint 指标,对应活动监视器的「内存」列,是判断内存压力的主要数字;`rss` 是当前驻留内存,会随 macOS 压缩或回收页面而下降。`footprint_peak_sampled` 是应用采样得到的峰值,`rss_peak_kernel` 是内核记录的进程生命周期峰值。近期一次 arm64 会话在开启缩略图和持久化剪贴板时,footprint 从启动后的 84MB 上升到缩略图缓存预热后的约 160MB,随后在后续采样中保持稳定;这只是观测值,不是硬性上限。剪贴板账本会拆分文本、预览和元数据;磁盘缓存中的图片原图不计入驻留内存。持久化主要改变历史的生命周期和启动时恢复的条目,不会改变单条记录的 RAM 模型。不记录剪贴板内容或窗口画面。
- **隐私**:debug 日志绝不记录具体按键。切换器的按键 tap 只记录 `Tab` / `Command` / `Option`(以及召唤组合名);其余按键一律记成 `Other` -- 不记键码、不记修饰位。

## <img height="16" src="docs/icons/heart.svg">&nbsp;&nbsp;致谢

**鼠标控制**功能(反向滚动、滚动模式、按设备配置、禁用指针加速)参考并借鉴了 [LinearMouse](https://github.com/linearmouse/linearmouse)。我们用纯 Rust(通过 `objc2` FFI 直接调 AppKit,无 Swift 桥接)从零重写了它的核心功能,并将其融入 oh-my-tab 的配置模型。衷心感谢原作者以及 LinearMouse 项目做出的优秀工作。

**窗口切换器**(浮层设计、卡片式选中、Liquid Glass 风格)参考并借鉴了 [BetterCmdTab](https://github.com/rokartur/BetterCmdTab)。我们用纯 Rust(通过 `objc2` FFI 直接调 AppKit,无 Swift 桥接)从零重写了这些思路。衷心感谢作者做出的优秀工作。
