# 鼠标手势方案（评审终稿）

- 日期：2026-11-05
- 状态：**设计定稿，未实现**。经 8 轮独立设计评审（Codex，gpt-6.1-sol，reasoning=high），
  第 1–7 轮为 `PLAN-STATUS: CONCERNS`，**第 8 轮为 `PLAN-STATUS: AGREED`**。
  全部发现与处理见 §10。
- 工作树：`oh-my-tab-mouse-gesture`（分支 `mouse-gesture-design`）。主工作树 `oh-my-tab` 未改动。
- 范围：新增 `src/mouse/gesture/`；修改 `src/mouse/event_tap.rs`、`src/mouse/keysim.rs`、
  `src/config.rs`、`src/lib.rs`、`src/key_bindings/*` 的接线点；新增设置页区域与三语文案。

本文是方案，不是实施报告。所引用的仓库行号与 SDK 事实均在 §7 给出核实命令；未实测的兼容性结论在
§8 如实列为 NOT RUN。

## 0. 摘要

用户按住鼠标键（右键 / 中键 / 侧键）拖动画出一条轨迹，抬手时识别为**方向序列**（如 `RD`、`URU`），
执行绑定的快捷键、应用内部动作或系统动作。

核心机制：**按下即接管 + 非手势时重放**。真实 macOS 开源实现（MacGesture / Strokly / sable）
一律在右键 Down 到达时立即吞掉事件，非手势时再补发一次点击，因为应用一旦收到 Down 就已在
`rightMouseDown` 里做出反应，事后无法撤回。本方案采用同一模型。

关键决定：

- **方向序列**而非 $1 模板比对：可读、可录入、可单测。
- **按下即吞**：进入 `Held`；位移不足 → 抬手重放成一次点击（右键菜单照常）；位移达标 → `Engaged`。
- **轨迹 HUD**：独立无边框窗口，`ignoresMouseEvents`，`level = .screenSaver`，从 `Engaged` 时刻开始画。
- **抬手执行**，异步派发，不阻塞 tap。
- **Esc 中止**：吞掉该次 Esc 的 Down 与 Up，直到物理 Up 才清位。
- **异常只作废、不补历史点击**。
- **统一修饰键仲裁**：手势与按钮映射通过同一个仲裁器管理修饰键，避免互相踩踏。
- **冲突检测只覆盖应用内**配置空间；系统级与前台应用级冲突不做（用户明确选择）。

## 1. 产品决策（已与用户逐条确认）

| # | 决策 |
|---|---|
| 1 | 识别方式：方向序列 R/L/U/D |
| 2 | 触发键：右键（默认）、中键、侧键；**右键必须在其中** |
| 3 | 按下即吞 + 非手势重放（采纳自 MacGesture/Strokly 源码证据） |
| 4 | 未命中时重放成一次点击 |
| 5 | **Esc 必须实现** |
| 6 | 执行时机：抬手执行，异步 |
| 7 | 目标：任意快捷键 / 内部动作（Action 36 项）/ 系统动作 / 切换器 |
| 8 | 应用排除名单：整体粒度 |
| 9 | 冲突检测：只做应用内；**系统与前台应用级冲突不做** |
| 10 | HUD：独立窗口，从 `Engaged` 开始画，抬手直接消失 |

## 2. 事件模型

### 2.1 状态机

```
Idle
  └─(触发键 Down，且满足接管条件)→ Held     [吞掉 Down]
Held（持有 Down 数据：坐标、按钮、修饰键、clickState、时间戳）
  ├─ 累计路径 < min_path_length:
  │    Drag → 吞；Up → Replay(Down+Up, 坐标=Up 位置) → Idle
  └─ 累计路径 >= min_path_length → Engaged
Engaged [HUD 开始显示，从此刻坐标画；继续吞 Drag]
  ├─ Esc KeyDown → Abandoned
  └─ Up → Decision:
        ├─ 命中 & 授权有效   → Execute(不重放) → Idle
        ├─ 未命中           → Replay → Idle
        ├─ 用户 Esc 放弃     → Replay → Idle
        └─ 授权失效/生命周期 → Cancel(不执行,不重放) → Idle
Abandoned
  └─ Up → Replay → Idle
```

### 2.2 接管条件（全部满足才吞 Down）

1. `mouse.enabled` 为真；
2. `mouse.gestures_enabled` 为真；
3. 该 button 在 gestures 列表中至少有一条 `enabled` 且 `binding` 非空的条目；
4. 前台应用不在 `gesture_excluded_apps`；
5. 模块处于运行态（非录制、非停止中）。

默认四条 disabled 手势 → **不接管**，功能实际关闭。

### 2.3 所有权

Down 被吞时锁定 `(button, device_key, session_token)`；该会话期间只有这个按钮进入手势分支，
其它按钮/设备的按键继续走 `button_mappings`。会话在 Up 时终结，**所有权保持到 Up**，
配置中途变化不改变所有权。**旧 Up 绝不根据新配置进入 `button_mappings`**。

## 3. 重放事务

重放不是「重发原事件」，而是**新建一对 down+up**：

| 字段 | 来源 |
|---|---|
| button | 右键 `.right(1)` / 中键 `.center(2)` / 侧键 `CGMouseButton(3/4)` |
| 类型 | rightMouseDown/Up 或 otherMouseDown/Up |
| 位置 | Up 事件的全局坐标 |
| flags | Up 事件的 CGEventFlags（保留修饰键） |
| clickState | **由本模块维护的计数**（见 §3.1），不复制物理值 |
| buttonNumber | 原值 |
| source | `CGEventSource(stateID: .hidSystemState)` |
| 标记 | `sourceUserData = GESTURE_REPLAY_MARKER` |
| 投递点 | `kCGHIDEventTap` |

- 先 down 后 up，**两端都创建成功后才一起 post**；任一失败则两端释放、不投递、记日志。
- 统一用 `let _cleanup = guard` 形状保证 CFRelease，失败路径同样释放。
- 投递语义边界：`CGEventPost` 无接收端返回值，只断言「创建成功 → 投递调用成功」；
  接收端表现留给 M2。

### 3.1 多击计数

复制物理 `clickState` 会把**应用从未见过的点击**计入：一次成功的手势吞掉了一次点击，随后短按若携带
`clickState=2`，应用就会把单击解释成双击。因此：

- 计数**按 button 分别维护**；
- 只有**被重放出去的 down** 才推进计数，并刷新时间基准与位置基准；
- 手势命中（不重放）、授权失效取消（不重放）都**不推进**、**不刷新**基准；
- 超过系统 `NSEvent.doubleClickInterval`（主线程读入快照）或位置偏离超过
  `CLICK_DISTANCE_TOLERANCE_PT = 5.0` 屏幕点（**项目自定义兼容容差**，不宣称等于系统双击距离；
  依据是 `NSPressGestureRecognizer.allowableMovement` 的文档语义「in screen points. Defaults to
  double-click distance」，但不读系统值）→ 重置为 1；
- 重放的 down 与 up 使用同一个 clickState。

## 4. 识别

### 4.1 参数

| 参数 | 默认 | 说明 |
|---|---|---|
| `min_path_length` | 30.0 pt | 累计路径长度，超过才进入 Engaged |
| `min_segment_length` | 24.0 pt | 锚点到当前点的直线距离，达到才采纳一个方向 |
| `max_dirs` | 8 | 超过不截断，整条手势判未命中 |
| `noise_distance` | 1.5 pt | 小于此距离的点丢弃 |
| `degraded_gap_ms` | 16 | 相邻有效采样点时间间隔超过即置 degraded |

坐标系：CGEvent 全局坐标（点，非像素），**不做 Retina 换算**。

### 4.2 算法

```
anchor = Down 位置
for point in 采样点:
    if point 与上一个已处理点距离 < noise_distance: continue
    if |point - anchor| < min_segment_length: continue
    dir = dominant_direction(point - anchor)   // 4 方向，45° 平局有确定规则
    anchor = point
    if dir == last_dir: pending = None; continue
    if dir == pending_dir: pending_count += 1
        if pending_count >= 2: 采纳（追加到序列，last_dir = dir, pending = None）
    else: pending_dir = dir; pending_count = 1
Up 时: pending_count == 1 时按 Strokly 追加（该段已满足 min_segment_length，天然满足空间条件）
```

- **两段确认**是空间定义：必须是两个都达到 `min_segment_length` 的空间段，**不是两个事件**。
- **严格 45°**：`|dx| == |dy|` 时按「与上一方向同轴者优先，否则水平优先」的确定性规则，写死并单测。
- **识别独立于 HUD 抽稀**：HUD 用抽稀点画图，识别用完整采样点；环形缓冲只影响 HUD。
- **采样密度下的稳定性**：保证只在「每个主方向段至少 2 个有效采样点，且相邻点间距 <
  `min_segment_length / 2`」时成立；此时允许合并相邻同向段，不允许反转或主方向丢失。
  若整个转向落在采样间隙里，允许少一段但**不得反转**，并置 degraded。
  稠密轨迹只作测试参考；同一观测序列对应不同隐藏轨迹时，运行时结果相同。

### 4.3 degraded 的含义（可观测限定）

`degraded` 只表示**采样质量风险**（相邻有效采样点间隔 > 16ms），**不表示**方向是否丢失——
直线路径与绕行路径可能产生相同的观测点，运行时无从判断。

## 5. 执行

### 5.1 目标解析

```
resolve_target(desc):
    if let Some(action) = Action::from_config_id(desc) → Target::Action(action)
    else parse_binding(desc) → Target::Binding(Binding)   // Key / System / Switcher / None
```

复用 `src/mouse/shortcut.rs:170` 的 `Binding` 与 `src/key_bindings/action.rs` 的 36 个 `Action`。

### 5.2 授权在消费端检查

动作执行前重新检查：父开关、`gestures_enabled`、该条目是否仍在有效配置中（**`GestureEpoch` 单调序号**）、
前台应用排除、录制状态。任一不满足 → **取消（不执行、不重放）**。

`GestureEpoch` 每变化一次单调递增，不回退。事务记住开始时的 epoch；**epoch 不同即判定撤销并终结**，
不再参与后续比较（因此 A→B→A 后旧事务仍是撤销）。哈希只用于内容比较，不参与授权。

### 5.3 统一修饰键仲裁

现有 `keysim::release_up` 假定自己是修饰键的唯一持有者，会逐步把 flags 放回零；手势若另有事务，
两者会互相踩踏。

```
ModifierOwnership:
    physical: u32    // 当前物理修饰键类别位（由改写前的物理事件持续更新）
    synthetic: u32   // 本进程合成持有者要求的类别位（holders 集合的并集）
    holders: Map<bit, Set<HolderId>>   // ButtonMapping(u32) / Gesture(session) / Physical
```

- 只管理**类别位**（Cmd/Opt/Ctrl/Shift），**不区分左右**——与当前快捷键目标一致，文档与测试维持该边界。
- `physical` 初始化：模块启动后用 `CGEventSourceFlagsState(kCGEventSourceStateCombinedSessionState)`
  取一次作为初值；**之后由改写前的物理事件持续更新**，baseline 不再覆盖。
- **放行点改写**：所有透传按键（KeyDown/KeyUp/flagsChanged）不消费，但受管理修饰位按
  `out.flags = (in.flags & !MANAGEABLE_MASK) | (physical | synthetic)` 合并；其余字段原样保留。
  无合成持有时这是恒等变换。
- 合成事件带 `SYNTHETIC_MARKER` 或 `GESTURE_SYNTH_MARKER` 时直通，不改写、不更新 physical。

### 5.4 切换器

- 手势的 `switcher` 目标**只负责打开**，投递一次 `CmdTabPressed`；**不自动提交**（不要定时器）。
- 提交责任交给既有会话入口：Return / 卡片点击 → `commit_selected_window`；Esc → 既有 cancel。
- `SwitcherClaimId`（单调 u64）随打开请求一起入队，**不存旁路槽**：

```
enum TabStep { backward: bool, origin: TabOrigin }
enum TabOrigin { Physical, GestureOpen(SwitcherClaimId) }
```

  `src/lib.rs:341` 的 `PendingGlobalInput.tab_steps` 从 `VecDeque<bool>` 扩展为 `VecDeque<TabStep>`。
- claim 三态：`Active` / `HandedOff` / `Revoked`（`RevokedByCoalesce` 是容量丢弃时的终态）。
- 消费规则：
  - `GestureOpen(id)` 且 id 对应 `Active` → 启动会话，**保持 Active**（自身请求不触发接管）；
  - `GestureOpen(id)` 且已 `Revoked` → 丢弃；
  - `Physical` 且存在 `Active` → 置 `HandedOff`（转交），随后按普通会话流程走。
- 容量合并丢弃 `GestureOpen` 时，该 claim 立即 `RevokedByCoalesce` 并记日志；**先退出聚合锁，
  再安排主线程清理**，不在锁内调用 overlay/AppKit。
- 「已有切换器」判定覆盖三阶段并集：claimed ∪ pending_first_show ∪ visible，入口覆盖
  `CmdTabPressed` **与** `CmdShiftTabPressed`。
- 撤销覆盖三阶段；所有权随 overlay 会话结束（commit / cancel / hide_overlay / 新会话）而失效；
  claim 记录在请求与会话终结后回收。
- 主线程规则：切换器状态判断、取消、claim 清理全部在主线程；判断结束后先释放 `TAB_STATE` 借用，
  再调用 AppKit。

### 5.5 按键事务

手势执行的按键**不进入** `keysim` 的 `active_bindings`（那是按钮映射专用），而是独立
`GestureKeyTransaction`：记录自己按下的键与修饰键，down/up 成对投递，失败或强制清理时**只释放自己按下的**。
**不复用** `release_all_queued`。所有投递走同一个串行队列（复用 `KEY_JOBS`），修饰键状态由仲裁器统一计算。

## 6. 模块划分

```
src/mouse/gesture/
  mod.rs            模块生命周期、start/stop、与 mouse.rs 的关系
  recognizer.rs     纯函数：方向量化、两段确认、序列化、degraded 判定
  engine.rs         纯状态机 Idle/Held/Engaged/Abandoned → Decision::Pass/Swallow/Replay/Cancel
  replay.rs         重放事务构建（纯数据 → CGEvent 对），字段取自 §3
  table.rs          GestureTable 快照 + GestureEpoch（仿 key_bindings::table 的 publish）
  modifiers.rs      ModifierOwnership 仲裁器 + merge_flags 纯函数
  switcher.rs       SwitcherClaim 三态 + 打开请求
  executor.rs       目标解析、消费端授权复核、GestureKeyTransaction
  conflict.rs       纯函数冲突检查矩阵
  hud.rs            主线程独立窗口，session id 驱动，有界合并调度
```

### 6.1 HUD 调度器

- 两个原子 `RUNNING` / `DIRTY`，**一律 SeqCst**，先清 RUNNING 再看 DIRTY，生产者先置 DIRTY 再看 RUNNING；
- 快照**同步发布不可变数据**（`Mutex<Arc<Snapshot>>`），读快照后释放锁再绘制；
- 隐藏通道独立且优先于绘制；所有正常结束路径都隐藏 HUD；
- `hud_visible` 由主线程在实际 `orderOut` 之后写回，反映**窗口实际状态**；
- 每个任务携带 session id，旧会话的隐藏结果不覆盖新会话。

## 7. 核实依据（可复现命令）

```sh
# 键盘 tap 在 Session 级（不能改到 HID）
sed -n '332,352p' src/event_monitor.rs
# 全局输入聚合与容量
sed -n '341,380p' src/lib.rs
# 切换器 release 与 sticky
sed -n '696,730p' src/overlay/card_close.rs
# 按钮映射的事件类型过滤
sed -n '190,280p' src/mouse/event_tap.rs
# 现有合成标记
grep -rn 'SYNTHETIC_MARKER' src/event_tap.rs src/mouse/keysim.rs src/key_bindings/engine.rs
# 修饰键释放假定唯一持有者
sed -n '59,84p' src/mouse/keysim.rs
# 设置字段 16 步清单
sed -n '1,60p' docs/settings-field-checklist.md
# SDK：双击间隔（公开类属性）
grep -n 'doubleClickInterval' /Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk/System/Library/Frameworks/AppKit.framework/Versions/C/Headers/NSEvent.h
# SDK：allowableMovement 默认值 = 双击距离
grep -n 'allowableMovement' /Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk/System/Library/Frameworks/AppKit.framework/Versions/C/Headers/NSPressGestureRecognizer.h
```

参考实现（只读，克隆于 /tmp/gesture-research/）：

- MacGesture（ObjC）：`MacGesture/MacGesture/AppDelegate.m` 的 `mouseEventCallback`
- Strokly（Swift）：`Sources/StroklyCore/Services/{GestureEventMonitor,GestureEngine,GestureRecognizer,GestureOverlayWindow}.swift`
- sable（Swift）：`sable/AppDelegate.swift`、`sable/ViewController.swift`

## 8. 未实测项（如实标为 NOT RUN）

| 项 | 状态 |
|---|---|
| 真实接收端事件序列 | NOT RUN（需 M2 授权） |
| 右键/中键/侧键重放的接收端表现 | NOT RUN |
| 修饰键与多击语义在真实应用中的表现 | NOT RUN |
| 跨屏、混合缩放连续性 | NOT RUN |
| 与 Mac Mouse Fix / Karabiner 等其他重映射器共存 | NOT RUN |
| Session 合成 CmdTab 的兼容性（既有行为，不得回归） | NOT RUN |
| 哪些应用不接受重放 | NOT RUN（提供排除名单作逃生口） |

HID 投递点说明：`CGEventSource(stateID: .hidSystemState)` 指定的是**事件源状态表**，不会把合成事件变成
真实硬件输入。HID 投递会重新经过该位置**其他**重映射器的 tap，本模块的防回环标记只约束本模块自己。
选择 HID 是**明确的兼容性选择**，不是「与硬件等价」的保证；最终取舍由 M2 的接收端测试决定。

## 9. 配置

```toml
[mouse]
gestures_enabled = false          # 默认关闭
gesture_excluded_apps = []        # 整体粒度：命中的应用里所有手势都不生效

[[mouse.gestures]]
gesture = "RD"                    # 方向序列
button  = 1                       # 1=右键 2=中键 3=侧键1 4=侧键2
binding = "cmd+shift+t"           # 快捷键 / window.snap_left / system.mission_control / switcher
enabled = true
```

默认值：4 条 `R` `L` `U` `D`，`binding = ""`、`enabled = false`。**这是合法数据，不是解析错误**。

校验：`binding == ""` 只允许与 `enabled == false` 同时出现；「启用但无绑定」按无效条目处理并给出
`errors.gesture_empty_binding`，由 `merge_valid` 丢弃该条，兄弟条目不受影响。

### 9.1 配置链路（16 步）

| # | 落点 | 内容 |
|---|---|---|
| 1 | `src/config.rs` MouseSection | `gestures_enabled` / `gestures: Vec<GestureBinding>` / `gesture_excluded_apps` |
| 2 | `impl Default for MouseSection` | false / 4 条 disabled / 空 |
| 3 | `merge_valid` | 新增 `[[mouse.gestures]]` 分区，逐条校验与回退 |
| 4 | `locales/{en,zh-Hans,zh-Hant}.toml` | `row_gestures_*`、`desc_gestures_*`、`errors.gesture_*` |
| 5 | `src/settings/page_builder.rs` | 总开关、排除名单、手势列表、行编辑 |
| 6 | `src/settings.rs` SettingsUi | 控件指针字段 |
| 7 | `src/settings/window.rs` 字面量 | `null_mut()` |
| 8 | `src/settings.rs` `load_settings_from` | 回填初值 |
| 9 | `log_config_changes` | 一条 `changed!(gestures)` |
| 10 | `dispatch.rs` ControlField 三件套 | 开关、列表、行编辑、录制 |
| 11 | `update_windows_controls_enabled` | 父开关关闭时冻结手势区 |
| 12 | `runtime_config.rs` ChangeFlags | 新增 `GESTURES` |
| 13 | `needs_in_place_control_sync` | 加入 `GESTURES` |
| 14 | `dispatch.rs` 就地回填函数 | 与第 13 步**成对** |
| 15 | 消费点 | `gesture::table::publish(cfg)`；父开关关闭时清空并作废会话 |
| 16 | 测试 | 见 §10 |

**`ChangeFlags::GESTURES` 的派生快照**（整体比较，不用单一「条目集合」）：

```
GestureSnapshot {
    parent_enabled: bool,        // mouse.enabled
    gestures_enabled: bool,
    excluded: Vec<String>,       // 排序规范化
    entries: Vec<(button, gesture_key, target_key, enabled)>,
}
```

任一字段变化都触发；**只改排除名单**与**有效表为空时翻转父开关**都必须触发刷新。
`GestureEpoch` 是这个快照的单调序号，同时用于「运行期刷新」与「动作授权」。

## 10. 评审记录（8 轮）

| 轮次 | 状态 | 主要发现 |
|---|---|---|
| 1 | CONCERNS | BLOCKER：右键菜单前提错误；HIGH：按钮所有权冲突、Down/Up 配对、Esc 无输入源、HUD 时序、执行授权 |
| 2 | CONCERNS | 修订为「按下即吞 + 重放」后 BLOCKER 解除；HIGH：Esc 生命周期、异常清理、动态所有权、重放字段、切换器入口 |
| 3 | CONCERNS | HIGH：Esc 5 秒清位破坏长按、授权失败不能转成点击、切换器定时提交；MEDIUM：clickState、HID 理由、采样密度 |
| 4 | CONCERNS | HIGH：切换器撤销漏掉未显示会话、GESTURES 快照不含全部撤销条件、按键事务未约束按钮映射 |
| 5 | CONCERNS | HIGH：物理修饰键加入仲裁不能自动维持下游、接管误判自身请求；MEDIUM：哈希≠版本号、degraded 不可观测 |
| 6 | CONCERNS | HIGH：PhysicalBaseline 合并保留过期状态、自身打开请求被误判接管；LOW：5.0 点依据 |
| 7 | CONCERNS | HIGH：普通 KeyDown/KeyUp 原样放行破坏合成持有、单旁路槽无法证明身份；MEDIUM：tap 拓扑 |
| 8 | **AGREED** | 无新增阻断问题 |

### 10.1 第 8 轮的实现回审重点（评审原话）

1. 修饰键双方持有、物理状态变化、普通按键 Down/Up，以及无合成持有时逐位恒等；
2. 请求交错、撤销后重开、容量丢弃、HandedOff 后停止，以及首帧完成后的最终显示状态；
3. M2 中真实接收端事件序列、Session 合成 CmdTab 兼容性和其他重映射器共存。

**容量丢弃发生在生产者路径时，须先退出聚合锁，再安排主线程清理；不得在锁内调用 overlay/AppKit。
claim 记录在请求与会话终结后应回收。** 实现审查会据此核对。

### 10.2 已知限制（需写入用户文档）

- 只管理修饰键**类别位**，不区分左右；不等于完整的左右键仿真。
- Session 级合成事件（例如其他工具合成 Esc）不在本模块改写范围；HID 路径的验证结果不能推广到这些输入。
- 无法检测系统级与前台应用级快捷键冲突（用户明确选择不做）。
- 重放作用于释放位置下的目标，不等价于保留原拖动。

## 11. 测试计划

### M1（cargo-test，离线）

- 默认全禁用时零接管
- 配置中途变化时 Up 所有权不变
- Esc：交错释放（Esc Down → Mouse Up → Esc Up）、重复 KeyDown、先按住 Esc 再开始手势、按住超过 5 秒
- 授权失效三条路径（配置撤销 / 录制 / 生命周期）→ 不执行也不重放
- clickState：成功手势后短按、未命中重放后短按、三击以上、超间隔重置、换按钮、远距离连点
- 重放事务：字段取值、分配失败时两端释放且不投递
- 修饰键：双方持有两种顺序、物理状态变化、最后一个持有者结束、无合成持有时逐位恒等
- 切换器：四种请求交错、撤销后重开、容量丢弃、HandedOff 后停止、首帧完成后不显示
- HUD：退场交错测试（dirty 最后一次检查）、会话隔离
- GESTURES 派生快照：只改排除名单、有效表为空时翻转父开关
- recognizer：阈值边界、超长序列、斜折返、相邻重复、多屏负坐标、严格 45°、不同采样密度（含转向落在采样间隙）
- config：往返、逐字段 fallback、默认四条的合法性
- conflict 矩阵

### M2（用户触发，声明打扰标记，未授权报告 NOT RUN）

- 接收端实际看到的完整事件序列（右键/中键/侧键，短按/手势/未命中）
- 修饰键与多击语义
- HUD 实际隐藏后再执行
- 切换器：手势打开后 Return / 点击 / Esc / 再次触发四条退出路径
- 真实配置入口与控件同步（`--smoke-settings-state-sync`）
- 跨屏、混合缩放、其他重映射器共存

## 12. 实施顺序

| 阶段 | 内容 | 依赖 |
|---|---|---|
| P1 | `recognizer.rs` + 全部纯逻辑单测 | 无 |
| P2 | `engine.rs` 状态机 + 决策 + 单测 | P1 |
| P3 | `modifiers.rs` 仲裁器 + `merge_flags` + 单测 | 无 |
| P4 | `replay.rs` 事务 + 单测 | P2 |
| P5 | tap 接入：扩展 mask（Drag/KeyDown/KeyUp/flagsChanged）、分流、标记过滤 | P2/P3 |
| P6 | `table.rs` + `GestureEpoch` + `switcher.rs` + `executor.rs` | P4 |
| P7 | `conflict.rs` + 配置 16 步链路 | P6 |
| P8 | 设置页 UI + 录入器 | P7 |
| P9 | `hud.rs` | P2 |

**评审待办**：P5 改动 `event_tap.rs`、P6 改动 `keysim.rs`、P7 改动 `config.rs`——这三处是跨模块改动，
按 AGENTS.md 属于用户触发 `--full` 的范围，实现完成后需请用户确认。