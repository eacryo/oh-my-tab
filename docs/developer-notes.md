# 开发环境说明

只影响源码开发和调试器启动的裸二进制的问题，不影响 Homebrew 安装或打包 `.app` 的用户。开发期间的常规启动方式是 `scripts/dev-restart.sh`；裸 `cargo run` 仅用于底层诊断。README 的开发环境说明指向本文。

## 开发模式下图标可能不正确

用 `cargo run` 跑裸二进制进行诊断时，浮层偶尔会把 oh-my-tab 自己的卡片显示成首字母占位块而不是应用图标，而且可能一直持续到手动清空图标缓存。图标缓存按 bundle id 索引，以可执行文件的 **mtime** 作为失效指纹；开发模式下每次构建都会重新链接二进制、改变 mtime，导致运行中实例的缓存条目失效。打包后的 `.app` 不受影响（安装后二进制 mtime 稳定）。正常开发运行请使用 `scripts/dev-restart.sh`；如果诊断用裸二进制出现此问题，可从菜单 *Clear Icon Cache* 清空，或删除 `~/Library/Caches/oh-my-tab-icons/`。

## 调试器启动时鼠标控制可能失效

通过 RustRover 或其他调试器以 Debug 方式启动应用时，如果在启动阶段频繁操作鼠标，反向滚动和按设备配置等功能可能会失效。此时应用不再收到鼠标事件，滚动方向恢复为系统默认，指针加速设置也会停止生效，直到应用重启。目前主要在调试器启动的未签名开发构建中观察到这一问题，可能与 macOS 26 对调试器进程的 HID 层事件监听限制有关；打包的 `.app` 和从终端启动的二进制尚未出现相同行为。

## 全屏 Space 归属组

切换器只有在观察到同一窗口离开普通桌面、加入全屏 Space，并确认目标 Space 类型和显示器后，才会把全屏 Space 关联到来源桌面。应用启动时已经全屏、事件有缺口，或来源／显示器关系不唯一时，窗口只属于实际 Space，不猜测来源。Space 成员通知按顺序处理。归组正确时一次 Space 切换不会改变候选集合，因此 `--e2e-state` 在活跃 Space 上下文变化时额外写出 `refresh_context` 帧；断言只能读已应用候选集的帧（`refresh` / `refresh_context`），其他帧会把新上下文和旧的卡片列表配在一起。只有确知的事件丢失（队列溢出）才清除待确认转换证据；无法解析属主的成员事件只降低诊断连续性标记，因为一次全屏转换会为该应用的辅助窗口发出这类事件，按它们清空会把正在学习的转换本身清掉。配对窗口由 1 秒放宽到 3 秒：1325 加入事件可能早于 SkyLight 把新 Space 标成全屏，配对必须活到后续查询修正类型。旧版成员查询降级只接受明确处于屏幕上的窗口，不再把全屏尺寸当成跨 Space 放行条件。SkyLight 私有事件与查询接口属于尽力而为，未来 macOS 版本可能不再提供。

### 其他桌面的窗口

默认情况下上面的候选范围就是全部规则：其他 macOS 桌面上的窗口不是候选。`windows.show_other_desktops`
把准入放宽到「已受管、但不在任何显示器当前归属组内」的 Space，而且只通过这个开关放宽：被它放行的窗口
没有 AX 元素（`kAXWindows` 按当前 Space 过滤），因此以 CG 窗口名作标题、最小化状态不可读。准入仍然遵守「AX 权威」。当某个应用的 `kAXWindows` 为空、而键/主窗口槽位仍然命名了它**某个**窗口时，
AX 已经对该应用的窗口集合作出答复：一个既不在该答复里、也从未被 AX 识别过的 CG 窗口是 AX 有意排除的
次级表面，必须拒绝——微信的 `3018` 主窗口旁有一个 280×380、离屏、同标题的窗口，从别的桌面看时它曾经
变成第二张死卡（没有 AX 元素，选中只会 `ax NO MATCH`）。该判定放在「其他归属组例外」之前，因为这类应用
正好落在 `ax_empty_pids` 里，否则会被例外先吃掉。一份很小的「AX 曾识别过的窗口」记忆（pid + 进程启动
时间 + CGWindowID；销毁时清除并带否决以防旧收集写回，进程结束时清除）保证真实存在的第二窗口在其桌面被
访问过之后仍可放行；自本进程启动以来从未访问过其桌面的真实窗口会先隐藏。键/主槽位也为空时，AX 对这个应用一个窗口都没命名，它没有可切换的
窗口可展示，其 CG 条目是面板，任何桌面上都拒绝。实测即 Stats：一个 280×800、layer 0、无父窗口、离屏的
菜单栏面板（标题「Combined modules」），对 AX 完全不应答（`kAXWindows` 为空，`AXFocusedWindow`/
`AXMainWindow` 均不支持）；它既会在自己桌面上成为一张死卡，也会在跨桌面时出现，选中没有任何反应。
三种状态必须分清：AX 命名了窗口（走「其他归属组例外」，仍受归属与形状门约束）、**AX 查询失败**
（`ax_failed_pids`，完全未知，保留 CG 兜底）、**AX 应答但未命名任何窗口**（拒绝）。若一个窗口元素都没回来、
而三个属性读里有一个失败，这算「查询失败」而不是「空答复」：窗口列表按 Space 过滤，那个窗口可能只有刚失败的
键/主槽位能拿到。查询失败时**只放行当前在屏的窗口**：没有 AX 答复就分不出真实窗口和已关闭的菜单栏面板，
兜底只覆盖用户看得见的东西（把 Stats 冻结让它的 AX 读超时，修复前会同时放行可见的设置窗口和已关闭的
「Combined modules」面板——正是用户报的那一对）。

准入要求成员
列表非空且命中的 Space 属于已接受的显示器拓扑：空列表或未受管的 Space 正是 orderOut／辅助表面的形状，
开关打开时同样拒绝。该开关不会放宽旧版降级路径——它区分不了「另一个桌面」和「离屏窗口」；最小化开关
仍然先生效。

缩略图：这类卡片**不会请求**新的抓图（WindowServer 抓不到不在活跃桌面上的窗口），但会显示缓存里已有的
那一张——那是它还在自己桌面上时抓到的。生产端把它过滤掉，渲染端照旧读缓存；A2 快照为此发布
`thumbnail_ready`，让这个行为可断言。连缓存帧一起拒绝是错的：两个桌面都去过的用户，大多数卡片本来就有
一张，参考实现也是保留上一张缩略图。

激活：精确窗口的前台切换（`_SLPSSetFrontProcessWithOptions` + 窗口 id + userGenerated 模式 + 定向点击）
与应用激活（`NSRunningApplication activateWithOptions:`）都能移动 Space，谁先落地取决于状态。AltTab 对
跨 Space 目标依赖前台切换（「它同时会让 macOS 切到显示该窗口的 Space」），BetterCmdTab 把它保留为菜单栏
正确的跨 Space 兜底——他们放弃了 `CGSManagedDisplaySetCurrentSpace`，因为那个调用跳过 Space 切换机制，
从全屏 Space 出来时会让目标 Space 没有菜单栏。因此本项目先试应用激活，若窗口在
`OTHER_DESKTOP_ACTIVATION_BUDGET` 内没有进入活跃桌面，再用前台切换兜底；等待上限是
`OTHER_DESKTOP_SETTLE_BUDGET`（过渡是动画的），而在 AX 阶段已经跑起来之后才到达的窗口会补一次精确抬窗
（`OTHER_DESKTOP_LATE_SETTLE_BUDGET`）。这个文件早先的版本只依赖应用激活、跳过了前台切换；macOS 在某些
状态下会拒绝那个激活（`activateWithOptions=false`，目标始终没有成为前台——这正是用户日志里的情形），
于是这类卡片完全没有反应。此前这里写的「前台切换返回成功但活跃 Space 不动」来自一个没有辅助功能信任的
探针程序，是错的：该调用确实会切换，两个参考实现都依赖它。每次抬窗都会发布是哪条路径移动了 Space，
`scripts/e2e/space-desktops.sh` 还会用「抑制应用激活」的一趟单独验证兜底，而不是靠推断。

## 日志与内存诊断

默认日志路径为 `~/Library/Logs/oh-my-tab/oh-my-tab.log`。活动文件达到 10 MB 后依次滚动为 `oh-my-tab.log.1` 到 `oh-my-tab.log.5`，每次启动写入会话标记；旧版按启动生成的日志和超过 30 天的旧备份会在启动时清理。

启动约 60 秒后先记录一行，之后每 5 分钟记录一行 `[mem]`。日志包含当前功能画像、进程 footprint/RSS、采样期间 footprint 峰值、线程数，以及缩略图、剪贴板和窗口账本估算值。`footprint` 是 macOS physical footprint 指标，对应活动监视器的“内存”列，是判断内存压力的主要数字；`rss` 是当前驻留内存，会随 macOS 压缩或回收页面而下降。`footprint_peak_sampled` 是应用采样得到的峰值，`rss_peak_kernel` 是内核记录的进程生命周期峰值。

剪贴板账本会拆分文本、预览和元数据；磁盘缓存中的图片原图不计入驻留内存。日志不包含剪贴板内容和窗口画面。debug 日志只记录切换器按键 tap 中的 `Tab`、`Command`、`Option` 和召唤组合名，其余按键记为 `Other`，不包含键码和修饰位。

## 设备识别细节

应用通过 Generic Desktop 页的 Pointer(1,1)、Mouse(1,2) 和 Trackpad(1,5) 用途判断鼠标或触控板。它使用公开 API `IOHIDServiceClientConformsTo` 检查完整的 `DeviceUsagePairs`，而不是只读取单个 `PrimaryUsage` 值。

有些鼠标会报告错误的主用途。例如 **ATK A9 SE**（Nearlink/星闪鼠标）的 `PrimaryUsage = 6(Keyboard)`，因此在系统设置中显示为键盘，但它的 `DeviceUsagePairs` 同时声明了 Mouse(1,2)。检查完整用途列表可以将其识别为鼠标；如果只看 `PrimaryUsage`，设备事件会落入“最近使用”的档位。

有些蓝牙键盘的 HID 描述符也会声明指针用途，例如 Kzzi-i75 声明了完整的 Mouse 集合。设备下拉框会进一步检查蓝牙 **GAP Appearance**（0x03C1 = 键盘），数据来自 bluetoothd 写入 NVRAM 的缓存，并通过蓝牙地址与 HID 服务匹配。macOS 蓝牙面板的图标也使用这项数据。不在缓存中的设备（如刚配对的设备）以及非蓝牙设备，会回退到纯 HID 判定。

设备连接或断开时，下拉框会刷新。插拔事件会经过防抖处理，延迟重查用于覆盖较短的 BLE 休眠和唤醒过程。

## 构建与测试

开发期间常规的启动方式是 `scripts/dev-restart.sh`。它会构建并组装独立签名的开发版 `.app`，再交给用户级 `launchd` 启动。这样辅助功能与屏幕录制授权会绑定到开发版 bundle，启动的也是本次构建生成的二进制。

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

浮窗运行路径也有一个被忽略的子进程冒烟测试。先运行 `cargo build`，再执行 `cargo test overlay_runtime_smoke -- --ignored`；`--smoke-overlay` 会跳过单实例锁，避免已有开发版实例导致测试未执行却误判通过。

### 本地化与布局 QA

`scripts/dev-restart.sh` 构建的 Debug 版会在语言下拉框中加入 `[TEST] English x3` 选项。选中后会把每一条英文 UI 文案重复三遍，便于在真实设置窗口中检查超长下拉项及其所在行、卡片。`scripts/release-dev.sh` 构建的优化开发包通过 `dev-long-text` Cargo feature 包含同一夹具；正式发布脚本不启用它。旧的 `--pseudo-locale` 开关仍可用于仅限 debug 的符号膨胀。

在 debug 构建中用 `--layout-debug` 启动可开启运行时设置页断言；同级交互控件与标签/控件相交，或 frame 越出 document 时会立即失败，并给出页面名和出错的 frame。原生控件内部的子视图不会参与同级比较。
