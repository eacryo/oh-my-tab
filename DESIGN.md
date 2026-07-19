# Oh-My-Tab 设计文档

macOS 窗口切换器。按住 Option+Tab 弹出窗口切换界面，窗口按最近使用顺序排列，松开 Option 切换到选中窗口。

---

## 一、技术栈

| 层 | 选型 | 说明 |
|---|------|------|
| UI 框架 | **GPUI** | Zed 编辑器团队出品的 Rust GPU 渲染框架，零依赖 WebView |
| 窗口枚举 | CoreGraphics FFI | `CGWindowListCopyWindowInfo` 枚举所有屏幕窗口 |
| 全局快捷键 | CoreGraphics FFI | `CGEventTap` + `CFRunLoop` 后台线程拦截 Option+Tab |
| 跨线程通信 | `flume` | 无界 MPMC channel，事件从 event monitor 线程发到 GPUI 主线程 |
| 状态管理 | GPUI `Entity` + `observe` | TabState 作为 Entity 被 OverlayView 观察，修改自动触发渲染 |

### 关键依赖

```toml
[dependencies]
gpui = "0.2.2"
objc2 = "0.6"
flume = "0.11"
```

---

## 二、项目结构

```
oh-my-tab/
├── Cargo.toml
├── DESIGN.md
├── AGENTS.md
├── notes/
│   └── window-activation-approaches.md
└── src/
    ├── main.rs              # GPUI 入口，Entity<TabState>，UI 渲染，keystrokes/spawn 事件处理
    ├── event_monitor.rs     # CGEventTap + CFRunLoop，发送 GlobalEvent 到 flume channel
    └── window_collector.rs  # 窗口枚举 + MRU 排序（HashMap<pid, Instant>）
```

---

## 三、架构设计

### 3.1 核心流程

```
CGEventTap 线程                       GPUI 主线程
┌──────────────────┐   flume channel  ┌──────────────────────────┐
│ 键盘事件          │────────────────▶│ spawn task (AsyncApp)     │
│ Option+Tab 按下   │                 │                          │
│ Option 释放       │                 │ se.update() → cx.notify() │
│                  │                 │ app_cx.activate(true)    │
└──────────────────┘                 └──────────┬───────────────┘
                                                │ Entity observe 触发
                                      ┌─────────▼───────────────┐
                                      │ OverlayView::render()   │
                                      │                         │
                                      │ state.read() → 渲染卡片  │
                                      │ 窗口列表 / 选中高亮      │
                                      └─────────────────────────┘
                                                │
                                      ┌─────────▼───────────────┐
                                      │ observe_keystrokes       │
                                      │                         │
                                      │ overlay 内导航:          │
                                      │ Tab/Right/Left/Enter/Esc│
                                      └─────────────────────────┘
```

### 3.2 状态

`TabState` 作为 GPUI `Entity` 由 `OverlayView` 观察：

```rust
struct TabState {
    windows: Vec<WindowInfo>,  // 当前窗口列表（MRU 排序）
    selected: usize,           // 当前高亮索引
    visible: bool,             // overlay 是否可见
    mru: MruMap,               // HashMap<pid, Instant>，最近使用记录
}
```

### 3.3 事件流

| 事件 | 来源 | 处理 |
|------|------|------|
| Option+Tab 按下（任意 app） | `event_monitor` → flume → spawn task | 显示 overlay + 循环选中窗口 |
| Option 释放 | `event_monitor` → flume → spawn task | `activate_pid(selected.pid)` + 隐藏 overlay |
| Tab / Right（overlay 可见时） | `observe_keystrokes` | 选中下一个窗口 |
| Left（overlay 可见时） | `observe_keystrokes` | 选中上一个窗口 |
| Enter（overlay 可见时） | `observe_keystrokes` | 激活选中窗口 + 隐藏 overlay |
| Escape（overlay 可见时） | `observe_keystrokes` | 隐藏 overlay（不切换） |

---

## 四、模块详解

### 4.1 `event_monitor.rs`

- 在独立线程运行 `CGEventTap` + `CFRunLoop`
- 监听 `kCGEventKeyDown` 和 `kCGEventFlagsChanged`
- 检测 Option+Tab（keyCode=48, flags & kCGEventFlagMaskAlternate）：发送 `GlobalEvent::OptionTabPressed`，**return NULL 吞掉事件**
- 检测 Option 释放（flagsChanged, alt flag 清除）：发送 `GlobalEvent::OptionReleased`
- 其他事件原样放行（return event）
- 需要 Accessibility 权限，创建失败时打印错误

### 4.2 `window_collector.rs`

- `CGWindowListCopyWindowInfo(kCGWindowListOptionOnScreenOnly, 0)` 枚举所有窗口
- 过滤：`kCGWindowLayer == 0`、排除自身 PID、排除 Dock
- MRU 排序：首次扫描记录 `(pid, Instant)` 到 `HashMap`，按时间戳排序（最久远的在前）
- 标记第一个窗口 `is_active = true`

### 4.3 `main.rs` — GPUI 入口

**Entity + Observe 模式**：
```rust
// 创建 Entity<TabState>
let state_entity = cx.new(|_cx| TabState::new());

// OverlayView 持有 Entity clone + Subscription
struct OverlayView {
    state: Entity<TabState>,
    _observer: Subscription,   // cx.observe() 返回，持续持有防止被 drop
}

// View 初始化时注册 observer：
cx.observe(&state, |_: &mut OverlayView, _: Entity<TabState>, cx: &mut Context<OverlayView>| {
    cx.notify();  // entity 变更 → 触发 render()
});
```

**Spawn Task（处理全局事件）**：
```rust
let async_app = cx.to_async();
let _task = Box::leak(Box::new(cx.spawn(move |_: &mut AsyncApp| async move {
    while let Ok(event) = event_rx.recv_async().await {
        async_app.update(|app_cx| {
            se.update(app_cx, |state, cx| { /* 修改 state */ cx.notify(); });
            if should_activate { app_cx.activate(true); }
        });
    }
})));
```

**Keystroke Observer（overlay 内导航）**：
- 监听 Tab / Right / Left / Enter / Escape
- 只处理 `state.visible == true` 时的导航事件
- 注意：Option+Tab 已被 CGEventTap 吞掉，不会到这里

**渲染**：GPUI flexbox div 布局，显示应用名首字母、应用名、窗口标题、底部状态栏。

---

## 五、权限要求

| 权限 | 用途 | 触发弹窗 |
|------|------|----------|
| **Accessibility** | CGEventTap 拦截全局键盘事件 | 首次启动系统弹窗 |
| **Automation** | `osascript` 告诉 System Events 按 PID 激活窗口 | 首次切换窗口时弹窗 |

> 已知可替代 Activation 的方案见 `notes/window-activation-approaches.md`（`NSRunningApplication` 方案不需要额外 Automation 权限）。

---

## 六、已确认的设计决策

| 决策 | 结论 |
|------|------|
| UI 框架 | GPUI（纯 Rust GPU 渲染，无 WebView） |
| 快捷键 | Option+Tab（避免与系统 Cmd+Tab 冲突） |
| 前端 | 无独立前端，UI 通过 GPUI `div()` 构建 |
| 跨线程通信 | `flume` unbound channel |
| 状态管理 | GPUI `Entity<T>` + `observe` 自动触发渲染 |
| 窗口激活 | `osascript` + System Events（按 PID） |
| 显示器支持 | 窗口居中，非全屏 |
| 缩略图 | 暂用应用名首字母替代 |
| 窗口关闭功能 | 暂不实现 |

---

## 七、潜在的已知问题和改进空间

### 7.1 进程管理

CGEvent Tap 是系统级的，当你按下 Option+Tab 的时候，CGEventTap 会吞掉所有带 Option 的 Tab 事件。但如果 omy-tab 意外崩溃，需要系统重新启动才会重新注册 CGEventTap。目前没有进程守护。

### 7.2 窗口生命周期处理

应用关闭后立即重启，MruMap 中的窗口记录会丢失。当前每次打开 overlay 都重新枚举所有窗口，依赖于窗口的实时性。

### 7.3 输入法兼容性

在某些输入法激活时，Option+Tab 可能被输入法拦截无法触发 CGEventTap。

### 7.4 多显示器支持

当前窗口始终在"主屏幕"居中显示，没有考虑多显示器配置。

### 7.5 CSS 样式刷新

当窗口数量或名称变化时，需要重新计算每个卡片的宽度，当前使用固定 160px 宽度来显示，如果窗口过多可能导致卡片不在屏幕内。

---

## 八、参考资料

- [CGEventTap 文档](https://developer.apple.com/documentation/coregraphics/cgeventtap)
- [CGWindowList 文档](https://developer.apple.com/documentation/coregraphics/cgwindowlist)
- [GPUI 仓库](https://github.com/zed-industries/zed/tree/main/crates/gpui)
- [flume 文档](https://crates.io/crates/flume)
