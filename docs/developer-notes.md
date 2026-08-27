# 开发环境已知问题

只影响源码开发（`cargo run` / 调试器）的问题，不影响 Homebrew 安装或打包 `.app` 的用户。从 README 精简而来。

## 开发模式下图标可能不正确

用 `cargo run` 跑裸二进制时，浮层偶尔会把 oh-my-tab 自己的卡片显示成首字母占位块而不是应用图标，而且可能一直持续到手动清空图标缓存。图标缓存按 bundle id 索引，以可执行文件的 **mtime** 作为失效指纹；开发模式下每次构建都会重链接二进制、改变 mtime，导致运行中实例的缓存条目失效。打包后的 `.app` 不受影响（安装后二进制 mtime 稳定）。开发中遇到此问题，可从菜单 *Clear Icon Cache* 清空，或删除 `~/Library/Caches/oh-my-tab-icons/`。

## 调试器启动时鼠标控制可能失效

通过 RustRover（或其他调试器）以 Debug 方式启动应用时，若在启动阶段频繁操作鼠标（滚动/点击），鼠标控制功能（反向滚动、按设备配置等）可能会失效——应用不再收到鼠标事件，滚动方向恢复为系统默认、指针加速设置停止生效，直到重启应用。直接启动打包的 `.app` 或在终端中直接运行二进制不受影响；该问题只出现在调试器启动的未签名开发构建上（macOS 26 对调试器进程的 HID 层事件监听限制所致）。

## 设备识别细节

判断某设备是否为鼠标/触控板，看它是否符合 Generic Desktop 页的 Pointer(1,1)、Mouse(1,2) 或 Trackpad(1,5) 用途——用公开 API `IOHIDServiceClientConformsTo` 检查设备完整的 `DeviceUsagePairs`，而不是单个 `PrimaryUsage` 值。这是必要的，因为有些真实鼠标的主用途会被系统报错：例如 **ATK A9 SE**（Nearlink/星闪鼠标）在系统里 `PrimaryUsage = 6(Keyboard)`，系统设置会把它显示为键盘——但它的 `DeviceUsagePairs` 里同时声明了 Mouse(1,2)，`ConformsTo` 能识别出来。如果只看 `PrimaryUsage`，这类设备会被静默丢弃，它们的事件会被错误地套到"最近使用"的档位上。

蓝牙键盘即使 HID 描述符虚报了指针用途（如 Kzzi-i75 声明了完整的 Mouse 集合），也会被排除在设备下拉框之外：下拉框会交叉核对蓝牙 **GAP Appearance**（0x03C1 = 键盘）——数据来自 bluetoothd 写入 NVRAM 的缓存，按 HID 服务的蓝牙地址匹配，与 macOS 蓝牙面板的图标同源。不在 NVRAM 缓存里的设备（如新配对）或非蓝牙设备，回退到纯 HID 判定。设备下拉框**实时刷新**：拔掉设备会立即从列表移除，重连也会自动重新出现（插拔事件有防抖但不会丢弃；延迟重查覆盖快速的 BLE 休眠-唤醒模式）。
