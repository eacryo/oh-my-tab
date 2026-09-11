# 开发环境说明

只影响源码开发（`cargo run` / 调试器）的问题，不影响 Homebrew 安装或打包 `.app` 的用户。README 的开发环境说明指向本文。

## 开发模式下图标可能不正确

用 `cargo run` 跑裸二进制时，浮层偶尔会把 oh-my-tab 自己的卡片显示成首字母占位块而不是应用图标，而且可能一直持续到手动清空图标缓存。图标缓存按 bundle id 索引，以可执行文件的 **mtime** 作为失效指纹；开发模式下每次构建都会重新链接二进制、改变 mtime，导致运行中实例的缓存条目失效。打包后的 `.app` 不受影响（安装后二进制 mtime 稳定）。开发中遇到此问题，可从菜单 *Clear Icon Cache* 清空，或删除 `~/Library/Caches/oh-my-tab-icons/`。

## 调试器启动时鼠标控制可能失效

通过 RustRover（或其他调试器）以 Debug 方式启动应用时，若在启动阶段频繁操作鼠标（滚动/点击），鼠标控制功能（反向滚动、按设备配置等）可能会失效——应用不再收到鼠标事件，滚动方向恢复为系统默认，指针加速设置停止生效，直到重启应用。直接启动打包的 `.app` 或在终端中直接运行二进制不受影响；该问题只出现在调试器启动的未签名开发构建上（macOS 26 对调试器进程的 HID 层事件监听限制所致）。

## 日志与内存诊断

默认日志路径为 `~/Library/Logs/oh-my-tab/oh-my-tab.log`。活动文件达到 10 MB 后依次滚动为 `oh-my-tab.log.1` 到 `oh-my-tab.log.5`，每次启动写入会话标记；旧版按启动生成的日志和超过 30 天的旧备份会在启动时清理。

启动约 60 秒后先记录一行，之后每 5 分钟记录一行 `[mem]`。日志包含当前功能画像、进程 footprint/RSS、采样期间 footprint 峰值、线程数，以及缩略图、剪贴板和窗口账本估算值。`footprint` 是 macOS physical footprint 指标，对应活动监视器的“内存”列，是判断内存压力的主要数字；`rss` 是当前驻留内存，会随 macOS 压缩或回收页面而下降。`footprint_peak_sampled` 是应用采样得到的峰值，`rss_peak_kernel` 是内核记录的进程生命周期峰值。

剪贴板账本会拆分文本、预览和元数据；磁盘缓存中的图片原图不计入驻留内存。日志不包含剪贴板内容和窗口画面。debug 日志只记录切换器按键 tap 中的 `Tab`、`Command`、`Option` 和召唤组合名，其余按键记为 `Other`，不包含键码和修饰位。

## 设备识别细节

判断某设备是否为鼠标/触控板，看它是否符合 Generic Desktop 页的 Pointer(1,1)、Mouse(1,2) 或 Trackpad(1,5) 用途——用公开 API `IOHIDServiceClientConformsTo` 检查设备完整的 `DeviceUsagePairs`，而不是单个 `PrimaryUsage` 值。这是必要的，因为有些真实鼠标的主用途会被系统报错：例如 **ATK A9 SE**（Nearlink/星闪鼠标）在系统里 `PrimaryUsage = 6(Keyboard)`，系统设置会把它显示为键盘——但它的 `DeviceUsagePairs` 里同时声明了 Mouse(1,2)，`ConformsTo` 能识别出来。如果只看 `PrimaryUsage`，这类设备会被静默丢弃，它们的事件会被错误地套到“最近使用”的档位上。

蓝牙键盘即使 HID 描述符虚报了指针用途（如 Kzzi-i75 声明了完整的 Mouse 集合），也会被排除在设备下拉框之外：下拉框会交叉核对蓝牙 **GAP Appearance**（0x03C1 = 键盘）——数据来自 bluetoothd 写入 NVRAM 的缓存，按 HID 服务的蓝牙地址匹配，与 macOS 蓝牙面板的图标同源。不在 NVRAM 缓存里的设备（如新配对）或非蓝牙设备，回退到纯 HID 判定。设备下拉框**实时刷新**：拔掉设备会立即从列表移除，重连也会自动重新出现（插拔事件有防抖但不会丢弃；延迟重查覆盖快速的 BLE 休眠-唤醒模式）。

## 构建与测试

开发期间常规的启动方式是 `scripts/dev-restart.sh`。它会构建并组装独立签名的开发版 `.app`，再交给用户级 `launchd` 启动，因此辅助功能与屏幕录制授权会绑定到开发版 bundle，且实际运行的始终是最新构建。

单元测试默认无 GUI 依赖；剪贴板图片/历史测试夹具使用按进程、按线程隔离的系统临时目录。CG/AX **冒烟测试**标记为 `#[ignore]`，需要 GUI 会话和辅助功能权限：

```sh
cargo test -- --ignored
```

在 macOS GUI 会话中，真实的 AppKit 设置页冒烟测试可以遍历所有设置页并执行相同的布局后置检查，无需手工点击：

```sh
cargo build
cargo test settings_layout_smoke -- --ignored
```

它会以 `--smoke-settings-layout` 启动 debug 二进制，在主线程打开真实设置窗口，遍历全部七个页面，校验其子视图 frame 后退出。

### 本地化与布局 QA

`scripts/dev-restart.sh` 构建的 Debug 版会在语言下拉框中加入 `[TEST] English x3` 选项。选中后会把每一条英文 UI 文案重复三遍，便于在真实设置窗口中检查超长下拉项及其所在行、卡片。`scripts/release-dev.sh` 构建的优化开发包通过 `dev-long-text` Cargo feature 包含同一夹具；正式发布脚本不启用它。旧的 `OH_MY_TAB_PSEUDO_LOCALE=1` 开关仍可用于仅限 debug 的符号膨胀。

在 debug 构建中设置 `OH_MY_TAB_LAYOUT_DEBUG=1` 可开启运行时设置页断言；控件重叠、frame 越界、以及图层顺序非法的分隔线会立即失败，并给出页面名和出错的 frame。这些检查用于补充视觉检查，而不是要求每次布局改动都必须人工目视。
