# Oh-My-Tab 需求与架构设计文档

macOS 多任务切换软件。按住 Cmd+Tab 进入 Windows 11 风格的多任务管理界面，窗口按最近使用顺序排列。

---

## 一、需求分析

### 核心功能

| 编号 | 功能 | 描述 |
|------|------|------|
| F1 | 全局快捷键拦截 | Cmd+Tab 触发多任务界面，覆盖系统默认行为 |
| F2 | 最近使用排序 | 窗口按最近激活（MRU）顺序排列，非空间位置 |
| F3 | 可视化覆盖窗口 | 全屏半透明毛玻璃遮罩层，显示窗口列表，参照 Windows 11 切换风格 |
| F4 | Tab 循环切换 | 按住 Cmd 不松手，Tab 向前切换，Shift+Tab 向后切换 |
| F5 | 释放切换 | 松开 Cmd 键后，聚焦到当前选中窗口并关闭覆盖界面 |
| F6 | 鼠标交互 | 鼠标悬停高亮，点击直接选中窗口 |
| F7 | 关闭窗口 | **暂不实现** |

### 权限要求

- **辅助功能权限（Accessibility）**：必需。用于枚举窗口列表、激活窗口、获取应用图标。
- **输入监控权限**：CGEventTap 可能需要，视 `kCGSessionEventTap` 级别而定。

### 非功能性需求

- 仅在主显示器上显示覆盖界面
- Demo 阶段使用应用图标替代窗口缩略图
- 无动画，纯静态覆盖层

---

## 二、技术栈

| 层 | 选型 | 说明 |
|---|------|------|
| 后端 | Rust | 负责 CGEvent 拦截、窗口枚举、状态机编排 |
| 前端 UI | Tauri WebView + 纯原生 HTML/CSS/TS | 无框架依赖，毛玻璃效果用 CSS |
| macOS API | `core-graphics` + `objc2` + `objc2-foundation` | 调用原生 API |
| 通信 | Tauri IPC（`invoke` / `emit`） | 前后端事件驱动通信 |

### 关键依赖

```toml
[dependencies]
tauri = { version = "2", features = ["transparent"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
core-graphics = "0.24"
objc2 = "0.6"
objc2-foundation = "0.3"
parking_lot = "0.12"
```

---

## 三、项目结构

```
oh-my-tab/
├── DESIGN.md                     # 本文档
├── src-tauri/
│   ├── Cargo.toml
│   ├── tauri.conf.json           # 窗口配置（透明、无边框、全屏、置顶）
│   ├── build.rs
│   ├── src/
│   │   ├── lib.rs                # Tauri 入口，注册 commands，持有 AppState
│   │   ├── permissions.rs        # 辅助功能权限检查
│   │   ├── event_monitor.rs      # CGEventTap 键盘事件监听
│   │   ├── window_collector.rs   # 窗口枚举 + MRU 排序 + 图标获取
│   │   └── switcher.rs           # 状态机
│   └── icons/
├── src/                          # 前端
│   ├── index.html                # 覆盖窗口 HTML 结构
│   ├── styles.css                # Windows 11 风格样式
│   └── main.ts                   # 前端逻辑：接收事件，渲染卡片，处理交互
├── package.json                  # 前端依赖（typescript 等）
└── tsconfig.json
```

---

## 四、架构设计

### 4.1 核心流程

```
CGEventTap 线程                    Tauri 主线程
┌──────────────┐   channel    ┌──────────────────┐
│ 键盘事件      │────────────▶│ switcher 状态机    │
│ Cmd↓ Tab↓    │             │                  │
│ Cmd↑         │             │ emit("tick")     │
└──────────────┘             │ 发给前端           │
                             └────────┬─────────┘
                                      │
                             ┌────────▼─────────┐
                             │  前端 WebView     │
                             │                   │
                             │ 监听 tick → 更新高亮│
                             │ invoke("get_windows")│
                             │ 获取窗口列表渲染    │
                             └───────────────────┘
```

### 4.2 状态机

```
        ┌────────┐  Cmd+Tab   ┌───────────────┐
        │  IDLE  │──────────▶│  OVERLAY       │
        └────────┘◀──────────└───┬───────────┘
            ▲        Cmd释放/Esc  │
            │                    │ Tab/Shift+Tab
            │             ┌──────▼──────────┐
            │             │   NAVIGATING    │◀──┐
            │             └──────┬──────────┘   │
            │                    │ Tab/Shift+Tab │
            │              Cmd释放│              │
            │             ┌──────▼──────────┐   │
            │             │    ACTIVATE     │───┘
            │             └─────────────────┘
            │                    │
            └────────────────────┘
```

### 4.3 后端模块详解

#### `permissions.rs`
- 启动时调用 `AXIsProcessTrusted()`
- 若无权限，调用 `AXMakeProcessTrusted()` 弹出系统授权弹窗
- 通过 `runloop` 轮询等待用户授权，最多等待 30 秒
- 超时或拒绝则退出应用，提示用户去系统设置开启

#### `event_monitor.rs`
- 在独立线程运行
- 创建 `CGEventTap`，级别为 `kCGSessionEventTap`
- 监听 keyDown / keyUp / flagsChanged 事件
- 解析出：Cmd 按下/抬起、Tab 按下/抬起、Shift 按下/抬起
- 通过 `std::sync::mpsc::Sender` 发送 `KeyEvent` 枚举给主线程
- 将事件 tap 添加到当前线程的 CFRunLoop

#### `window_collector.rs`
- 调用 `CGWindowListCopyWindowInfo(option, kCGNullWindowID)` 枚举所有窗口
- 过滤规则：
  - `kCGWindowLayer` == 0（普通窗口层）
  - 窗口有标题（`kCGWindowName` 非空）
  - 排除桌面（`kCGWindowOwnerName` != "Dock"）
  - 窗口 bounds 非零
  - 窗口在屏幕区域内
- MRU 排序：
  - 首次扫描所有窗口，记录 `(pid, window_id, timestamp)`
  - 通过 `NSWorkspace.shared.notificationCenter` 监听 `NSWorkspaceDidActivateApplicationNotification`
  - 窗口激活时更新其时间戳，重新排序
- 图标获取：
  - 从 `CGWindowListCopyWindowInfo` 中的 `kCGWindowOwnerPID` 获取 pid
  - 通过 `NSRunningApplication.runningApplicationWithProcessIdentifier` 获取 `NSRunningApplication`
  - 调用 `icon` 属性获取 `NSImage`
  - 转换为 PNG data → base64 → 通过 IPC 返回给前端

#### `switcher.rs`
- 定义状态枚举：`Idle`, `Overlay { windows, selected_index }`, `Navigating { windows, selected_index }`
- 消费 `event_monitor` 发来的 `KeyEvent`
- 事件处理逻辑：
  - `Idle` 状态收到 `CmdDown + TabDown` → 转为 `Overlay`，收集窗口，选中索引 1（第二个窗口，第一个是当前窗口），emit `show_overlay`
  - `Overlay` / `Navigating` 状态收到 `TabDown`（Cmd 仍按住）→ selected_index +1（到末尾则循环到第一个），emit `update_selection`
  - `Overlay` / `Navigating` 状态收到 `ShiftTabDown` → selected_index -1（到开头则循环到末尾），emit `update_selection`
  - `Overlay` / `Navigating` 状态收到 `CmdUp` → 调用 `activate_window`，emit `hide_overlay`，回到 `Idle`
  - `Overlay` / `Navigating` 状态收到 `EscDown` → emit `hide_overlay`，回到 `Idle`，不切换窗口

### 4.4 前端模块详解

#### `index.html`
- `<div.overlay-bg>` — 全屏遮罩，`backdrop-filter: blur(24px)` 毛玻璃效果
- `<div.cards-container>` — 水平居中的 flexbox 容器
  - `<div.card>` × N — 每个窗口卡片
    - `<img.app-icon>` — 64×64 应用图标
    - `<span.app-name>` — 应用名
    - `<span.window-title>` — 窗口标题
- `<div.selected-title>` — 底部居中，显示当前选中窗口的完整标题

#### `styles.css` (Windows 11 风格)
```
- overlay-bg: 全屏 fixed, background: rgba(0,0,15,0.55), backdrop-filter: blur(24px)
- cards-container: flex, gap: 12px, justify-content: center, align-items: center
- card: 
  - width: 160px, border-radius: 8px, overflow: hidden
  - background: rgba(255,255,255,0.08)
  - border: 1px solid transparent
  - transition: transform 150ms, border-color 150ms
  - 选中态: border-color: rgba(255,255,255,0.6), transform: scale(1.05)
- app-icon: width: 100%, 居中, 边距
- app-name: text-align: center, color: white, font-size: 12px, 截断显示
- selected-title: position: fixed, bottom: 40px, text-align: center, width: 100%, color: rgba(255,255,255,0.7)
```

#### `main.ts`
- 监听 Tauri 事件：
  - `show-overlay` → 渲染卡片，显示 overlay
  - `update-selection` → 更新选中卡片的 CSS class
  - `hide-overlay` → 隐藏 overlay
- 用户交互：
  - 鼠标点击卡片 → `invoke("activate_window", { pid, index })`
- 键盘事件交由后端 event_monitor 统一处理（防止 WebView 获取焦点后截断事件）

### 4.5 Tauri Commands（IPC 接口）

| Command | 方向 | 参数 | 返回值 | 说明 |
|---------|------|------|--------|------|
| `get_windows` | 前端 → 后端 | 无 | `Vec<WindowInfo>` | 获取当前所有窗口列表（含 MRU 排序） |
| `activate_window` | 前端 → 后端 | `pid: i32` | `bool` | 激活指定进程的主窗口 |
| `dismiss_overlay` | 前端 → 后端 | 无 | 无 | 关闭覆盖窗口，不切换 |
| `tick` | 后端 → 前端 | `{ action, index, windows }` | — | 状态变更事件 |
| `show_overlay` | 后端 → 前端 | `{ windows, selected }` | — | 显示覆盖窗口 |
| `hide_overlay` | 后端 → 前端 | 无 | — | 隐藏覆盖窗口 |

### 4.6 数据结构

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowInfo {
    pub pid: i32,
    pub window_id: u32,
    pub app_name: String,       // e.g. "Safari"
    pub window_title: String,   // e.g. "GitHub - Google 搜索"
    pub icon_base64: String,    // 应用图标 base64 编码
    pub is_active: bool,        // 当前活跃窗口
}

#[derive(Debug, Clone)]
pub enum KeyEvent {
    CmdDown,
    CmdUp,
    TabDown,
    TabUp,
    ShiftDown,
    ShiftUp,
    EscDown,
}

#[derive(Debug, Clone)]
pub enum SwitcherState {
    Idle,
    Overlay { selected: usize, windows: Vec<WindowInfo> },
    Navigating { selected: usize, windows: Vec<WindowInfo> },
}
```

### 4.7 Tauri 窗口配置 (`tauri.conf.json`)

```json
{
  "app": {
    "withGlobalTauri": true,
    "windows": [
      {
        "label": "overlay",
        "title": "",
        "decorations": false,
        "transparent": true,
        "alwaysOnTop": true,
        "fullscreen": false,
        "visibleOnAllWorkspaces": true,
        "skipTaskbar": true,
        "focus": true,
        "center": true
      }
    ],
    "security": {
      "csp": null
    }
  }
}
```

---

## 五、Step-by-Step 实施计划

### Step 1：初始化 Tauri 项目

**目标**：搭建可运行的 Tauri v2 项目骨架，配置透明无边框全屏窗口。

#### 1.1 安装 Tauri CLI
```bash
cargo install tauri-cli --version "^2"
```

#### 1.2 初始化项目
- 手动改造现有 `Cargo.toml` 为 workspace 结构
- 创建 `src-tauri/` 目录
- 运行 `cargo tauri init` 或手动配置 `tauri.conf.json`

#### 1.3 配置 `src-tauri/Cargo.toml`
- 声明依赖：`tauri`（features: transparent）、`serde`、`serde_json`、`core-graphics`、`objc2`、`objc2-foundation`
- 添加 `[build-dependencies]`：`tauri-build`

#### 1.4 配置 `src-tauri/tauri.conf.json`
- 窗口配置：`decorations: false`, `transparent: true`, `alwaysOnTop: true`
- 设置窗口大小为当前主显示器分辨率
- 关闭 CSP 限制（允许 inline style）

#### 1.5 创建前端骨架
- `src/index.html`：基本 HTML 结构，引用 `styles.css` 和 `main.ts`
- `src/styles.css`：基础样式（body 透明、无 margin、全屏）
- `src/main.ts`：Tauri 事件监听骨架

#### 1.6 前端构建
- 初始化 `package.json`（无需框架依赖）
- 安装 TypeScript
- 创建 `tsconfig.json`
- 在 `tauri.conf.json` 中配置 dev URL 和 build 路径

#### 验收标准
- `cargo tauri dev` 能启动
- 看到一个透明的、无边框的全屏窗口
- 窗口置顶，不显示在 Dock 中
- 可通过 Cmd+Q 或点击关闭按钮退出

---

### Step 2：窗口枚举与权限模块

**目标**：实现权限检查引导流程，以及窗口枚举和 MRU 跟踪功能。

#### 2.1 权限模块 (`src-tauri/src/permissions.rs`)
- 导出 `check_accessibility_permission()` 函数
- 在 `lib.rs` 的 `setup` 钩子中调用
- 调用 `AXIsProcessTrusted()` 检查
- 若无权限：
  - 弹出 `NSAlert` 对话框，引导用户开启
  - 调用 `AXMakeProcessTrusted()` 触发系统弹窗
  - 轮询等待用户授权（最多 30 秒）
  - 超时则 `std::process::exit(1)`
- 需要使用 `objc2` 调用 `AXIsProcessTrusted` 和 `AXMakeProcessTrusted`

#### 2.2 `NSWorkspace` 通知监听
- 创建一个后台线程监听 `NSWorkspaceDidActivateApplicationNotification`
- 在 `NSNotificationCenter` 注册观察者
- 收到通知时：记录激活的 `pid` + 时间戳，写入 `AppState.window_timestamps`
- 需要考虑：通知回调是在非主线程，需通过 `Mutex` 保护共享状态

#### 2.3 窗口枚举 (`src-tauri/src/window_collector.rs`)
- 导出 `collect_windows(mru_timestamps) -> Vec<WindowInfo>`
- 调用 `CGWindowListCopyWindowInfo(kCGWindowListOptionOnScreenOnly, kCGNullWindowID)`
- 遍历返回的 `CFArray` 中的每个 `CFDictionary`
- 提取字段：`kCGWindowLayer`, `kCGWindowName`, `kCGWindowOwnerPID`, `kCGWindowOwnerName`, `kCGWindowNumber`, `kCGWindowBounds`
- 过滤规则见 4.3 节
- 对每个窗口，提取应用图标 → base64（见 2.4）
- 根据 `mru_timestamps` 按最近激活时间降序排列
- 标记当前最前面的窗口为 `is_active = true`

#### 2.4 应用图标提取
- 通过 pid 获取 `NSRunningApplication`：`NSRunningApplication::runningApplicationWithProcessIdentifier(pid)`
- 调用 `.icon()` 获取 `NSImage`
- 转为 TIFF 表示 → `NSBitmapImageRep` → PNG data
- 将 PNG data 转为 base64 字符串
- 注意：图标可能为 nil，需要 fallback 默认图标

#### 验收标准
- 编译通过，无 unsafe 警告（必要的 FFI 标记 `unsafe` 即可）
- 可以手动调用 `collect_windows` 测试，打印有效窗口列表
- 权限模块在无权限时可正确弹窗引导
- MRU 时间戳在窗口切换时能正确更新

---

### Step 3：CGEvent 键盘事件拦截

**目标**：实现 Cmd+Tab 全局快捷键拦截。

#### 3.1 事件监听 (`src-tauri/src/event_monitor.rs`)
- 导出 `start_event_monitor(tx: Sender<KeyEvent>)` 函数，接收一个 mpsc channel 的发送端
- 在内部创建独立线程运行 `CFRunLoop`

#### 3.2 创建 CGEventTap
- 使用 `CGEventTapCreate(kCGSessionEventTap, kCGHeadInsertEventTap, default, mask)`
- eventMask 包含：`kCGEventKeyDown`, `kCGEventKeyUp`, `kCGEventFlagsChanged`
- callback 函数：
  - 解析 CGEvent 中的 keyCode 和 flags
  - 识别 Cmd（keyCode=55/56）、Tab（keyCode=48）、Shift（keyCode=56/60）、Esc（keyCode=53）
  - 构建 `KeyEvent` 枚举值
  - 发送到 channel
  - 对于 Cmd+Tab 事件：**拦截并消费**（返回 NULL），阻止系统处理
  - 其他事件：返回原始 event，让其继续传递

#### 3.3 处理 flagsChanged
- macOS 修饰键状态变更时会触发 `flagsChanged` 而非 `keyDown`/`keyUp`
- 需要用 `prevFlags` 和 `currentFlags` 的差异判断 Cmd 按下或抬起
- `(current & kCGEventFlagMaskCommand) != (prev & kCGEventFlagMaskCommand)` → Cmd 状态变化

#### 3.4 添加到 RunLoop
- `CFRunLoopAddSource` 将 tap 添加到当前线程的 RunLoop
- `CFRunLoopRun()` 让 RunLoop 持续运行

#### 3.5 集成到 `lib.rs`
- 创建 `mpsc::channel()`
- 在 `tauri::Builder::setup()` 中：
  - 调用 `start_event_monitor(tx)`
  - 启动 switcher 循环（接收 rx 端的消息）
- 状态机在独立线程或主线程中运行

#### 注意事项
- CGEventTap 需要辅助功能权限才能工作
- 如果 tap 创建失败（权限不足），给出明确错误提示
- 事件处理要快，避免阻塞 RunLoop

#### 验收标准
- 应用启动后按下 Cmd+Tab，日志能正确打印出事件序列
- 系统默认的 Cmd+Tab 行为被抑制（不弹系统切换器）
- 其它快捷键不受影响
- 停止应用后系统快捷键恢复正常

---

### Step 4：状态机与 Tauri 前后端联调

**目标**：实现 switcher 状态机，完成 Tauri commands 注册和事件通信。

#### 4.1 状态机实现 (`src-tauri/src/switcher.rs`)
- 定义 `SwitcherState` 枚举（见 4.6）
- 定义 `Switcher` 结构体，持有：
  - `state: SwitcherState`
  - `app_handle: tauri::AppHandle`
  - `window_timestamps: HashMap<i32, Instant>` — MRU 记录
- 方法：
  - `handle_key_event(&mut self, event: KeyEvent)` — 消费键盘事件
  - `show_overlay(&mut self)` — 收集窗口，emit 给前端
  - `navigate_next(&mut self)` — 选中下一个
  - `navigate_prev(&mut self)` — 选中上一个
  - `activate_and_hide(&mut self)` — 激活选中窗口，隐藏 overlay
  - `dismiss(&mut self)` — 隐藏 overlay，不切换

#### 4.2 Tauri Commands 注册 (`src-tauri/src/lib.rs`)
- 使用 `#[tauri::command]` 注册：
  - `get_windows` — 调用 `window_collector::collect_windows`
  - `activate_window` — 调用 `activate_window_by_pid`
  - `dismiss_overlay` — 触发 switcher 的 dismiss
- `activate_window_by_pid` 实现：
  - 获取 `NSRunningApplication`
  - 调用 `.activateWithOptions(NSApplicationActivateIgnoringOtherApps)`
  - 通过 `AXUIElement` 获取应用主窗口并提升到前台

#### 4.3 AppState 管理
```rust
pub struct AppState {
    pub switcher: Mutex<Switcher>,
    pub window_timestamps: Mutex<HashMap<i32, Instant>>,
}
```
- 在 `tauri::Builder` 中 `.manage(AppState::new())`
- 各 command 通过 `state: State<AppState>` 访问

#### 4.4 事件循环
- 在 `tauri::Builder::setup` 中启动一个线程：
  - 循环读取 `rx`（来自 event_monitor 的 channel）
  - 调用 `switcher.handle_key_event(event)`
  - 状态机会通过 `app_handle.emit()` 通知前端

#### 验收标准
- 按下 Cmd+Tab → 前端收到 `show-overlay` 事件，显示窗口卡片
- 再次按 Tab → 收到 `update-selection`，高亮移动到下一个卡片
- 松开 Cmd → 收到 `hide-overlay`，卡片消失，目标窗口被激活到前台
- 按下 Esc → overlay 消失，窗口不切换
- 前后端通信正常，无明显延迟

---

### Step 5：前端 UI 实现

**目标**：实现 Windows 11 风格的覆盖窗口界面。

#### 5.1 HTML 结构 (`src/index.html`)
```html
<div id="app" style="display:none">
  <div class="overlay-bg"></div>
  <div class="cards-container" id="cards"></div>
  <div class="selected-title" id="title"></div>
</div>
```
- 初始 `display:none`，接收到 `show-overlay` 事件后显示

#### 5.2 CSS 样式 (`src/styles.css`)
- 详细样式参考 4.4 节
- 关键实现：
  - `backdrop-filter: blur(24px)` 实现毛玻璃
  - 选中卡片使用 CSS class `card--selected` 控制
  - 圆角、阴影、颜色参考 Windows 11 Alt+Tab 配色
  - 响应式：卡片宽度根据窗口数量自适应

#### 5.3 前端逻辑 (`src/main.ts`)
- 初始化 Tauri 事件监听：
```typescript
import { listen } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';

// 显示 overlay
listen('show-overlay', (event) => {
  renderCards(event.payload.windows);
  updateSelection(event.payload.selected);
  showApp();
});

// 更新选中
listen('tick', (event) => {
  updateSelection(event.payload.selected);
});

// 隐藏 overlay
listen('hide-overlay', () => {
  hideApp();
});
```
- `renderCards(windows)`: 遍历窗口列表，生成 card DOM 元素并插入
- `updateSelection(index)`: 移除所有 `.card--selected`，给对应 index 的卡片添加
- 卡片点击事件：`invoke('activate_window', { pid })`
- 双缓冲：window 数据变化时重新渲染

#### 5.4 卡片渲染
- 图标：`<img>` 标签，src 使用 `data:image/png;base64,{icon_base64}`
- 应用名：显示在图标下方，文字截断（`text-overflow: ellipsis`）
- 窗口标题：显示在底部 title bar

#### 验收标准
- 触发 overlay 后显示毛玻璃背景
- 窗口卡片居中排列，显示应用图标和名称
- 选中卡片有醒目的白色边框或高亮效果
- Tab 切换时高亮卡片流畅切换
- 底部正确显示当前选中窗口标题
- 整体视觉效果接近 Windows 11 Alt+Tab 风格

---

### Step 6：端到端联调

**目标**：全流程联调，修复边界问题。

#### 6.1 功能验证清单
- [ ] 应用启动后检查权限，无权限时弹窗引导
- [ ] 打开多个应用窗口，Cmd+Tab 正确弹出 overlay
- [ ] overlay 中窗口列表 MRU 顺序正确
- [ ] Tab 循环切换卡片高亮
- [ ] Shift+Tab 反向切换
- [ ] 松开 Cmd 激活选中窗口，overlay 消失
- [ ] Esc 取消切换，不改变当前窗口
- [ ] 鼠标点击卡片可直接切换
- [ ] 在 overlay 期间新打开的窗口不出现在当前列表中（直到下次触发）
- [ ] 应用本身不在窗口列表中
- [ ] 覆盖窗口完全遮挡 Dock 和菜单栏（视觉上）

#### 6.2 性能检查
- [ ] overlay 显示延迟 < 200ms（从按下到界面显示）
- [ ] Tab 切换延迟 < 50ms
- [ ] 无内存泄漏（CGEvent Tap 正确释放）
- [ ] CPU 占用：空闲时接近 0，overlay 显示时 < 5%

#### 6.3 边界情况
- [ ] 只有一个窗口时（不显示 overlay 或直接切换？）
- [ ] 当前桌面无窗口（Finder 桌面）
- [ ] 应用崩溃时恢复系统快捷键
- [ ] 多显示器：仅主显示器显示 overlay
- [ ] 全屏应用（如 Xcode Full Screen 模式）

#### 验收标准
- 完整功能 checklist 全部通过
- 无编译警告
- 无运行时 panic
- 在日常使用场景下稳定运行

---

## 六、附录

### A. 参考资料
- [CGEventTap 文档](https://developer.apple.com/documentation/coregraphics/cgeventtap)
- [CGWindowList 文档](https://developer.apple.com/documentation/coregraphics/cgwindowlist)
- [NSWorkspace 通知](https://developer.apple.com/documentation/appkit/nsworkspace)
- [Tauri v2 文档](https://v2.tauri.app/)
- [Windows 11 Alt+Tab 设计规范](https://learn.microsoft.com/en-us/windows/apps/design/signature-experiences/task-switching)

### B. 已确认的设计决策
| 决策 | 结论 |
|------|------|
| 前端框架 | 纯原生 HTML/CSS/TS（Demo 阶段） |
| 显示器支持 | 仅主显示器 |
| 缩略图 | 暂用应用图标替代，后期优化 |
| 窗口关闭功能 | 暂不实现 |
| UI 风格 | Windows 11 Alt+Tab 风格 |

### C. 待决策事项
| 事项 | 选项 |
|------|------|
| 应用自身如何退出 | 菜单栏图标 + 右键退出 / 独立设置窗口 / Cmd+Q |
| 是否允许用户自定义快捷键 | 是/否，Demo 阶段先硬编码 Cmd+Tab |
