# Mouse 增强功能架构设计 / Mouse Enhancement Architecture

将 LinearMouse 的核心鼠标增强功能集成进 oh-my-tab,用纯 Rust + objc2 调 AppKit 实现,不引入新依赖。

Integrate LinearMouse's core mouse enhancement features into oh-my-tab, implemented in pure Rust + objc2 calling AppKit, with no new dependencies.

---

## 1. 已确认的关键决策 / Confirmed Key Decisions

| 决策点 | 选择 | 理由 |
|---|---|---|
| UI 方案 | 纯 objc2 调 AppKit | 对齐 oh-my-tab 现有风格,保持单二进制、self-contained |
| 功能范围 | 聚焦核心功能 | 占 LinearMouse 90% 用户价值,排除 Logitech HID++(3648 行)/手势按钮/自动滚动/指针重定向 |
| 项目关系 | 集成进 oh-my-tab | 复用现有配置/i18n/菜单/权限基础设施 |
| Event Tap | 独立第二个 tap | mask 与现有键盘 tap 不重叠,可单独启停,代码清晰 |
| 事件处理 | tap callback 内同步处理 | 鼠标/滚轮事件必须同步返回改写后事件,不能走 flume 异步 |
| 配置格式 | 扩展 oh-my-tab 现有 TOML | 新增 `[mouse]` 段,沿用 per-field resilient validate/merge |
| 覆盖语义 | 首个匹配胜出(first match wins) | 无需递归 merge,实现简单,用户意图明确 |
| 依赖 | 不引入新 crate | 私有 API 走手写 extern + 系统 framework |

---

## 2. 集成约束与决策 / Integration Constraints

### 2.1 Event Tap 的根本性扩展

oh-my-tab 现有 event tap(`event_monitor.rs:140`)只监听 `K_CG_EVENT_KEY_DOWN | K_CG_EVENT_FLAGS_CHANGED`,且 callback 里直接 return event 透传(只吞掉 Cmd+Tab 组合)。LinearMouse 核心功能需要拦截并改写鼠标移动/滚轮/按钮事件 -- 这是完全不同的事件类型和回调语义。

**决策:独立第二个 event tap,不复用现有的。**

- 两个 tap 的 mask 完全不重叠(键盘 vs 鼠标/滚轮),合并只会让单个 callback 变成巨大的 match 分支。
- 现有 tap 是 `headInsertEventTap` 且只做"吞掉特定组合键",新 tap 需要"读取->变换->返回改写后事件",生命周期管理、健康检查、看门狗都不同。
- 独立 tap 可单独启停 -- 用户禁用鼠标增强功能时不影响窗口切换。
- 两个 event tap 共存是 macOS 支持的常规用法,系统按注册顺序串行调用。

新 tap 监听事件类型:
- `kCGEventScrollWheel`(22)
- `kCGEventLeftMouseDown/Up/Dragged`(1/2/6)
- `kCGEventRightMouseDown/Up/Dragged`(3/4/7)
- `kCGEventOtherMouseDown/Up`(25/26)
- `kCGEventMouseMoved`(5)

### 2.2 事件处理不能阻塞主线程的 Cmd+Tab

oh-my-tab 现有架构是"event tap 线程 -> flume -> 主线程 marshal"。LinearMouse 的事件变换必须在 event tap callback 内同步完成并返回改写后事件 -- 不能走 flume 异步,否则事件已经透传出去了。

**决策:鼠标/滚轮事件在独立线程的 callback 内同步处理;只把"需要通知 UI 刷新"的副作用(如设备切换、电池状态)走 flume 异步通知主线程。**

新增一个 `mouse_event_thread`(类似 oh-my-tab 现有 event 线程),跑自己的 `CFRunLoop`,承载 CGEventTap + 所有 MouseTransformer 状态。所有 transformer 状态必须在该线程访问,用 `Mutex` 保护跨线程读取(如配置热重载时)。

### 2.3 配置系统扩展

oh-my-tab 用 TOML + `serde` + per-field resilient validate。LinearMouse 原版用 JSON + FSEvents 热重载 + Scheme 合并语义。

**决策:统一进 oh-my-tab 的 TOML 配置,新增 `[mouse]` 顶层段。不沿用 LinearMouse 的 JSON/Scheme 模型,改用更贴合 oh-my-tab 风格的扁平配置 + per-device/per-app 覆盖数组。**

- oh-my-tab 已有成熟的 per-field resilient TOML 配置体系,引入第二套 JSON+FSEvents 会造成两套持久化逻辑。
- Scheme 的"有序数组合并"语义对 Rust serde 不友好且用户难手写;改用"基础配置 + 覆盖规则数组"更直观。

### 2.4 依赖一致性

oh-my-tab 刻意保持极简依赖(6 个 crate)。LinearMouse 需要 IOKit/HID FFI,但这些是系统 framework,通过 `objc2`/`libc`/手写 extern 声明即可,**不引入新 crate**。

---

## 3. 模块架构 / Module Architecture

新增 14 个文件,集中在 `src/mouse/` 目录下,与 oh-my-tab 现有模块物理隔离,降低耦合。

```
src/
├── (现有 oh-my-tab 模块不动)
├── mouse/
│   ├── mod.rs                      # 模块入口,导出公共 API
│   ├── ffi.rs                      # CGS/IOHID 私有 API extern 声明
│   ├── event_view.rs               # CGEvent 字段读取/修改的 safe wrapper
│   ├── event_tap.rs                # 第二个 CGEventTap:鼠标/滚轮事件拦截线程
│   ├── device.rs                   # IOKit/HID 设备枚举与属性(精简版,无 Logitech HID++)
│   ├── pointer.rs                  # 指针速度/加速度/分辨率设置(IOHIDSetParameter)
│   ├── action.rs                   # 动作执行:CGS 私有 API / 合成事件 / 运行命令
│   ├── key_simulator.rs            # 按键模拟(修饰键 down/up + 重复定时器)
│   ├── state.rs                    # 鼠标功能的线程内状态(当前设备、修饰键状态、transformer 状态)
│   └── transformer/
│       ├── mod.rs                  # Transformer trait + 链构建器 + LRU 缓存
│       ├── reverse_scroll.rs       # 反向滚动
│       ├── linear_scroll.rs        # 线性滚动(行/像素距离归一化)
│       ├── smoothed_scroll.rs      # 平滑滚动引擎(核心,三态状态机 + 120Hz 定时器)
│       ├── scroll_accel.rs         # 滚动加速/速度调整
│       ├── modifier_actions.rs     # 修饰键 + 滚轮 = 动作
│       ├── button_actions.rs       # 按钮映射(核心,映射匹配 + 三种行为模式)
│       ├── universal_back_forward.rs  # 通用前进/后退
│       ├── switch_buttons.rs       # 主次按钮互换
│       └── click_debounce.rs       # 点击去抖
├── mouse_ui.rs                     # 设置窗口的 Mouse tab(纯 objc2 调 AppKit)
└── (config.rs / settings.rs / menu.rs / i18n.rs 扩展)
```

现有文件改动:

| 文件 | 改动 |
|---|---|
| `Cargo.toml` | 无新依赖(私有 API 走手写 extern + 系统 framework) |
| `config.rs` | 新增 `[mouse]` 配置段,走现有 per-field resilient validate/merge |
| `main.rs` | 启动序列加 `mouse::start()` + bridge 线程扩展 |
| `settings.rs` | 新增 Mouse 导航项 |
| `menu.rs` | 新增鼠标增强菜单入口 |
| `i18n.rs` + 3 个 `locales/*.toml` | 新增 `mouse.*` i18n key,全部过 `t()`/`tf()` |

---

## 4. 核心子系统设计 / Core Subsystems

### 4.1 事件拦截层(`mouse/event_tap.rs`)

**线程模型**:独立 `mouse_event_thread`,QoS = `userInteractive`,跑 `CFRunLoop`。

**Event Tap 配置**:
- Location: `kCGHIDEventTap`(0)-- 比 oh-my-tab 用的 `kCGSessionEventTap`(1)更底层,能在 HID 层拦截。两者 mask 不重叠,无实际冲突。
- Place: `headInsertEventTap`(0)-- 队首插入,最先看到事件。
- Options: `defaultTap`(1)-- 允许修改/丢弃事件(需要 AX 权限,oh-my-tab 已有)。
- Mask: scroll wheel + 所有 mouse down/up/dragged/moved。

**Callback 流程**(同步,在 mouse_event_thread 内):
1. 从 CGEvent 提取 senderID(经 `CGEventCopyIOHIDEvent` -> `IOHIDEventGetSenderID`)-> 匹配设备
2. 提取光标位置 -> 匹配屏幕/进程(`CGWindowListCopyWindowInfo` + mouseLocationOwnerPid)
3. 查 transformer 链(LRU 缓存,key = device+app+display 组合)
4. 调用 transformer.transform(event) 链式处理
5. 返回改写后 event(或 null 丢弃)

**看门狗**:复用 oh-my-tab 已有的重试模式(`event_monitor.rs` 的 `RETRY_INTERVAL`/`RETRY_MAX`),额外加 5s health check -- event tap 会被系统在系统设置切换、睡眠唤醒后静默禁用,需定时 `CGEventTapIsEnabled` 检测并重新 enable。

### 4.2 Transformer 架构(`mouse/transformer/mod.rs`)

**核心 trait**:
```
trait MouseTransformer: Send {
    fn transform(&mut self, event: &mut EventView, ctx: &TransformContext) -> Option<EventAction>;
}
```
`EventAction` = `Pass` | `Drop` | `Replace(CGEventRef)`。`TransformContext` 携带当前设备/进程/屏幕/配置快照。

**链构建**:按固定优先级顺序追加 transformer(只加入启用的,未启用不实例化,零开销)。顺序决定优先级,见第 5 节。

**状态隔离**:每个 transformer 是独立结构体,持有自己的状态(如 smoothed_scroll 的相位状态机、click_debounce 的计时器)。

**LRU 缓存**:key = `(device_id, app_pid, display_id)`,缓存已构建的 transformer 链。配置变更或设备切换时失效。容量 16(同 LinearMouse)。

### 4.3 平滑滚动引擎(`mouse/transformer/smoothed_scroll.rs`)-- 最复杂模块

这是整个项目的技术核心,对应 LinearMouse 的 626 行状态机。建议单独隔离,优先实现并加单元测试。

**状态机**:三态 `idle` / `touching`(滚轮在动)/ `momentum`(松手后惯性衰减)。

**核心算法**(对应 `SmoothedScrollingEngine`):
- **速度估计器**:记录最近若干个滚轮 tick 的时间戳,估计输入速率。
- **期望速度计算**:`desiredVelocity = baseMagnitude * curve(inputExponent) * velocityScale * speedBoost * accelerationBoost`(13 种预设曲线,每种一组 profile 参数)。
- **速度融合**:当前速度按 `blendFactor` 追赶 `desiredVelocity`,避免突变。
- **惯性衰减**:进入 momentum 后按 `momentumDecay` 指数衰减,直到低于阈值停止。
- **合成事件发射**:120Hz `CFRunLoopTimer` 在 mouse_event_thread 上发射连续 pixel scroll 事件,附带 GestureScroll companion 事件(用 CGEventField 私有字段 118/119/132)。

**预设曲线**:13 种(linear/easeIn/easeOut/easeInOut/quadratic/cubic/quartic/easeOutCubic/easeInOutCubic/easeOutQuartic/easeInOutQuartic/smooth/custom),每种硬编码 5 个参数(inputExponent/accelerationGain/decay/velocityScale/response)。用 `enum` + `match` 表达,零运行时开销。

**关键风险**:这块逻辑高度时序敏感,移植时必须保留 LinearMouse 的单元测试作为行为基准(项目有 `LinearMouseUnitTests/`)。建议先把原测试用例翻译成 Rust 测试,再实现算法直到通过。

### 4.4 按钮映射(`mouse/transformer/button_actions.rs`)-- 第二复杂模块

**映射匹配**:遍历配置中的 mappings,匹配 button number + modifier flags + scroll direction,取 last 匹配(后者优先)。

**三种行为模式**(对应 `KeyPressBehavior`):
- `send_on_release`:mouseUp 时执行一次(默认)。
- `repeat`:mouseDown 执行一次,等系统 `keyRepeatDelay` 后按 `keyRepeatInterval` 重复(用 `CFRunLoopTimer` 在 event thread 调度)。
- `hold_while_pressed`:mouseDown 时 `key_simulator.down()`,mouseUp 时 `key_simulator.up()`。

**动作执行**(`mouse/action.rs`),按动作类型分发:
- `mission_control` / `launchpad` / `app_expose` / `show_desktop` / `space_left|right` -> **CGS 私有 API**(`CGSSetSymbolicHotKeyEnabled`)。手写 FFI extern 声明 + 链接 `ApplicationServices`。
- 媒体键(音量/亮度)-> **IOHIDPostEvent** + NXEventData(通过 `IOServiceOpen(kIOHIDParamConnectType)` 句柄)。
- 鼠标按钮 -> 合成 `mouseDown` + `mouseUp`(`CGEventCreateMouseEvent`)。
- 滚轮 -> 合成 `scrollWheelEvent2`。
- 运行命令 -> `std::process::Command::new("/bin/bash").arg("-c").arg(cmd)`。
- 按键序列 -> `key_simulator.press(keys)`。

### 4.5 设备管理(`mouse/device.rs`)-- 精简版

LinearMouse 的 DeviceManager 较重(含 Logitech HID++)。核心功能范围内只需:

- **设备枚举**:`IOHIDManagerCreate` + 匹配 `kHIDUsage_GD_Mouse`,获取 vendorID/productID/productName/serialNumber。
- **活跃设备跟踪**:`IOHIDManagerRegisterDeviceMatchingCallback` / `IOHIDManagerRegisterDeviceRemovalCallback`,维护 `active_devices: Vec<Device>`。
- **CGEvent -> 设备关联**:从 CGEvent 提取 `IOHIDEventGetSenderID` -> `IOHIDEventSystemClientCopyServiceForRegistryID` 反查设备。这是匹配 per-device 配置的关键链路。

**不做**:Logitech 接收器监控、HID++ 协议、电池读取、硬件 DPI 调节、高分辨率滚轮归一化(这些都在核心功能范围外)。

### 4.6 指针速度/加速度(`mouse/pointer.rs`)

- `IOServiceOpen(kIOHIDParamConnectType)` 获取参数连接句柄。
- `IOHIDSetAccelerationWithKey` / `IOHIDSetPointerResolution` 设置加速曲线和分辨率。
- per-device:通过设备的 IORegistryEntry 设置 `HIDPointerAcceleration` / `HIDPointerResolution` 属性。
- `Unsettable<f64>` 语义:配置为 unset 时不调用 Set,恢复系统默认(对应 LinearMouse 的三态)。

---

## 5. Transformer 链顺序 / Transformer Chain Order

按固定优先级顺序追加(只加入启用的,未启用不实例化):

1. `reverse_scroll` -- 反向滚动
2. `linear_scroll` -- 线性滚动(若 distance 配置且未启用 smoothed)
3. `smoothed_scroll` -- 平滑滚动(若启用)
4. `scroll_accel` -- 滚动加速/速度调整(若未启用 smoothed)
5. `modifier_actions` -- 修饰键动作
6. `button_actions`(scroll 方向映射)
7. `button_actions`(按钮映射)
8. `switch_buttons` -- 主次按钮互换
9. `universal_back_forward` -- 通用前进/后退
10. `click_debounce` -- 点击去抖

---

## 6. 配置模型 / Configuration Model

新增 `[mouse]` 顶层段,沿用 oh-my-tab 现有的 per-field resilient `validate()` / `merge_valid()` 机制。

### 6.1 基础配置

```toml
[mouse]
enabled = true

# 指针 / Pointer
[mouse.pointer]
acceleration = 0.6875       # 0-40,"unset" = 恢复系统默认
speed = 0.0                 # 0-1
disable_acceleration = false

# 滚动 / Scrolling
[mouse.scrolling]
reverse_vertical = true
reverse_horizontal = false
distance_vertical = "auto"  # "auto" | <行数> | "<N>px"
distance_horizontal = "auto"
acceleration_vertical = 1.0
speed_vertical = 0.0

# 平滑滚动 / Smoothed scrolling
[mouse.scrolling.smoothed]
enabled = true
preset = "easeInOut"        # 13 种预设
response = 1.0              # 0-2
speed = 2.0                 # 0-8
acceleration = 3.0          # 0-8
inertia = 4.0               # 0-8

# 修饰键动作 / Modifier actions
[mouse.scrolling.modifiers]
command = "auto"            # auto|ignore|preventDefault|changeSpeed|changeOrientation|zoom|...
shift = "changeOrientation"
option = "changeSpeed"
control = "auto"

# 按钮 / Buttons
[[mouse.buttons.mappings]]
button = "mouse:3"          # mouse:N | scroll:up|down|left|right
modifiers = []
action = "missionControl"
# 或 action = { run = "open -a Finder" }
# 或 action = { key_press = ["cmd", "c"] }

[mouse.buttons]
universal_back_forward = "both"   # none|both|backOnly|forwardOnly
switch_primary_secondary = false

[mouse.buttons.click_debouncing]
timeout_ms = 0
buttons = [1]
reset_on_mouse_up = false
```

### 6.2 覆盖规则(首个匹配胜出)

> **注**:本节描述的 `[[mouse.overrides]]` 设计已被 **配置档模型** 取代(见
> `src/config.rs` 的 `MouseProfile` + `src/mouse/resolve.rs`)。实际实现采用
> `[[mouse.profiles]]`:无 device 字段的档 = "所有鼠标"默认层,有 device 字段的档 =
> per-device 覆盖;合并语义为"遍历所有匹配档,后者优先"(非首个匹配胜出)。
> 设备身份按 VID+PID 匹配;事件归因链见 `src/mouse/device.rs`
> (`CGEventCopyIOHIDEvent` -> `IOHIDEventGetSenderID` -> registry ID 查表)。
> 以下为历史设计记录,保留供参考。
>
> **Note**: the `[[mouse.overrides]]` design below has been superseded by the **profile model**
> (see `MouseProfile` in `src/config.rs` + `src/mouse/resolve.rs`). The actual implementation uses
> `[[mouse.profiles]]`: a profile without a device field = the "All Mice" default layer, one with
> device fields = a per-device override; merge semantics are "iterate all matching profiles, later
> wins" (not first-match-wins). Device identity matches on VID+PID; the event-attribution chain is
> in `src/mouse/device.rs` (`CGEventCopyIOHIDEvent` -> `IOHIDEventGetSenderID` -> registry-ID
> lookup). The text below is the historical design, kept for reference.

per-device / per-app 覆盖解决"不同场景下鼠标行为不同"的需求。例如:
- 不同鼠标不同设置(游戏鼠标关加速度,普通鼠标保留)
- 特定 App 下改滚动方向(Terminal 反转)
- 特定 App 下改按钮映射(Figma 侧键变缩放,VSCode 侧键变切换 tab)
- 外接显示器下改指针速度

**语义**:遍历 `overrides` 数组,**首个匹配的规则胜出**,后续不再检查。无需递归 merge,实现简单,用户意图明确。局限是无法表达"同一设备 + 不同 app 叠加"的复合场景(可接受)。

```toml
# 覆盖规则:首个匹配胜出
[[mouse.overrides]]
[mouse.overrides.if]
device_vendor_id = 1133       # Logitech
# device_product_id = 17492  # 可选,进一步精确匹配
# app = "com.apple.Terminal" # bundle id
# display = "DELL U2720Q"    # 显示器名
[mouse.overrides.pointer]
disable_acceleration = true

[[mouse.overrides]]
[mouse.overrides.if]
app = "com.apple.Terminal"
[mouse.overrides.scrolling]
reverse_vertical = false
```

匹配条件字段(`[mouse.overrides.if]`):
- `device_vendor_id` / `device_product_id` -- 按 USB VID/PID 匹配设备
- `app` -- 当前前台 app bundle id
- `display` -- 当前光标所在显示器名

覆盖字段只列需要改的,未列的继承基础配置。

---

## 7. 私有 API FFI 策略 / Private API FFI Strategy

LinearMouse 依赖四类私有 API,Rust 侧的处理方式:

| 私有 API | 用途 | Rust 处理 |
|---|---|---|
| **CGSSymbolicHotKey** | 触发 Mission Control/Launchpad 等 | 手写 `extern "C"` + `#[link(ApplicationServices)]`,常量用裸 `i32` |
| **CGEventField 私有字段**(110-135) | 合成手势事件 | 直接用 `CGEventSetIntegerValueField` 传数字常量,无需 extern |
| **IOHIDEventSystemClient** | 设备枚举/属性 | 手写 extern + `#[link(IOKit)]`,或用 `libloading` 运行时加载 |
| **IOHIDPostEvent** | 系统定义键(音量/亮度) | 手写 extern + `IOServiceOpen` 句柄 |

**统一放 `src/mouse/ffi.rs`**,所有私有 API 声明集中管理,加 `// SAFETY:` 注释说明为何安全调用。

**CGS 私有 API 降级策略**:用 `libloading` 运行时加载 + 符号存在性检查,未来 macOS 若移除该符号可降级为 `CGEventPost` 虚拟键码(丢失"系统设置中可能被禁用"的语义,但功能不中断)。

---

## 8. UI 层 / UI Layer

在 oh-my-tab 现有设置窗口的侧边栏新增 "Mouse" 导航项。

新建 `mouse_ui.rs`,内部组织:
- `MouseSettingsPanel` -- 侧边栏项 + 详情视图容器
- `PointerSettingsView` -- 指针速度/加速度滑块 + disableAcceleration 开关
- `ScrollingSettingsView` -- 反向滚动开关 + distance 选择 + smoothed 预设 + 修饰键动作下拉
- `ButtonsSettingsView` -- mappings 列表(TableView)+ universalBackForward + switchPrimary + clickDebouncing

全部用 objc2 调 `NSView`/`NSSlider`/`NSButton`/`NSTableView`/`NSTextField`,风格对齐 oh-my-tab 现有 `settings.rs`。

**按钮映射录制**:支持在设置里"录制"按钮映射(按下按钮自动填入)。Rust 侧需要临时 event tap 捕获下一次鼠标按下,走 flume 通知 UI。此特性可后置。

---

## 9. 启动集成流程 / Startup Integration

扩展 `main.rs` 的启动序列(现有 8 步,在 event_monitor 启动之后插入):

```
现有步骤 1-6(配置/日志/i18n/菜单/控制器/事件监听)...
6.5 (新)若 config.mouse.enabled:
     - mouse::device::start()          # IOKit 设备枚举线程
     - mouse::pointer::apply_defaults() # 应用默认指针设置
     - mouse::event_tap::start()        # 第二个 CGEventTap(鼠标事件线程)
7. (现有)event_monitor + bridge thread
8. (现有)NSApp run
```

**生命周期**:复用 oh-my-tab 现有的 `NSWorkspace` 通知(session active/sleep/wake),扩展为同时启停窗口切换 event tap 和鼠标 event tap。

---

## 10. i18n 扩展 / i18n Extension

新增 i18n key 范围:
- `settings.mouse.*` -- 设置面板所有标签
- `menu.mouse_*` -- 菜单项(启用/禁用鼠标增强、打开鼠标设置)
- `alert.mouse_*` -- 权限提示等

同步添加到 `locales/en.toml`、`locales/zh-Hans.toml`、`locales/zh-Hant.toml`,全部过 `t()`/`tf()`。

---

## 11. 风险与缓解 / Risks & Mitigations

| 风险 | 严重度 | 缓解 |
|---|---|---|
| **平滑滚动引擎移植偏差** | 高 | 先翻译 LinearMouse 单元测试为 Rust,算法实现到测试通过为止 |
| **CGS 私有 API 在未来 macOS 被移除** | 中 | 用 `libloading` 运行时加载 + 符号存在性检查,缺失时降级为 `CGEventPost` 虚拟键码 |
| **两个 event tap 共存的系统资源** | 低 | macOS 支持多 tap,实测无问题;若担心可合并为单 tap(但会牺牲代码清晰度) |
| **objc2 调 AppKit 构建 4-tab 设置 UI 工作量** | 高 | 分 tab 增量实现,先 Pointer + Scrolling,Buttons 后置 |
| **per-app 配置匹配的进程查询性能** | 中 | `CGWindowListCopyWindowInfo` 结果缓存,mouseDown/Up 时失效(同 LinearMouse) |
| **HID senderID -> 设备关联链路不稳** | 中 | 保留 fallback:关联失败时用"默认配置"而非报错 |

---

## 12. 实施顺序 / Implementation Order

建议分 6 个可独立提交的步骤:

1. **FFI + EventView**:`mouse/ffi.rs` + `mouse/event_view.rs`(私有 API extern + CGEvent safe wrapper,可单测)
2. **设备管理 + 指针设置 + 配置**:`mouse/device.rs` + `mouse/pointer.rs` + `[mouse]` 配置段 + `config.rs` 扩展
3. **Event Tap + Transformer 框架**:`mouse/event_tap.rs` + `transformer/mod.rs`(trait + 链构建 + LRU,先接 reverse_scroll 验证链路)
4. **核心 Transformer 逐个实现**:reverse -> linear -> scroll_accel -> modifier_actions -> button_actions -> switch_buttons -> universal_back_forward -> click_debounce
5. **平滑滚动引擎**:`smoothed_scroll.rs`(三态状态机 + 13 预设 + 120Hz 定时器,优先翻译 LinearMouse 单元测试为基准)
6. **UI + i18n + 菜单集成**:`mouse_ui.rs` + `settings.rs`/`menu.rs`/`i18n.rs` 扩展 + 三个 locales 文件

每步可独立提交,逐步可用。

---

## 13. 功能边界(不含)/ Out of Scope

以下 LinearMouse 功能**不在本次实现范围**,留作未来扩展:

- **Logitech HID++ 协议**(3648 行):硬件 DPI 调节、高分辨率滚轮、Reprogrammable Controls diversion。仅服务罗技设备用户,耗时极长。
- **手势按钮**(按住某键 + 拖动触发四方向手势)
- **自动滚动**(中键自动滚动,479 行)
- **指针重定向为滚动**(mouseMoved 转 scrollWheel + CGWarpMouseCursorPosition)
- **开机启动管理**(oh-my-tab 已有 `autostart.rs`,可复用)
- **自动更新**(Sparkle,Rust 侧用 `self_update` 或自研,非鼠标功能范畴)
- **电池状态显示**(依赖 Logitech HID++)
