# 浮窗松手后偶尔不消失

## 现象

按 Option+Tab（或 Cmd+Tab）呼出切换浮窗后，松开修饰键时浮窗**偶尔不消失**，一直留在屏幕上，直到再次按 Option+Tab 或按 Escape 才清掉。

## 原因

### 原因 1：`on_cmd_released` 的 None 分支不隐藏窗口（确定性 bug）

`src/main.rs:421-448`：

```rust
extern "C" fn on_cmd_released(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    let mut state_opt = TAB_STATE.lock().unwrap();
    let state = state_opt.as_mut().unwrap();
    if !state.visible { return; }

    if let Some(w) = state.windows.get(state.selected) {
        // ...
        hide_overlay();          // ← 只在 Some 分支里调用
        activate_pid(pid);
        raise_ax_window(pid, &wt);
        state.mru.insert(wid, std::time::Instant::now());
    } else {
        eprintln!("... selected index {} out of bounds ...");  // ← None 分支只打印，不 hide
    }
    state.visible = false;       // ← 但 visible 照样置 false
}
```

当 `state.windows.get(state.selected)` 为 `None` 时，`hide_overlay()` 被跳过，但 `state.visible = false` 照常执行。结果：**浮窗留在屏幕上，state 却说"不可见"**。

触发条件：呼出时 `collect_windows` 返回空列表（所有窗口都被过滤掉，或 AX/CG 瞬时异常）。空列表时 `on_cmd_tab_pressed` 仍会 `show_overlay()`（画出空浮窗）并置 `visible=true`；松手时 `get(0)` 命中 None 分支 -> 不隐藏 -> 卡住。

同样的"只在 Some 分支 hide"模式还存在于：

- `container_key_down` 的 `KEY_RETURN` 分支
- `card_mouse_down`

只有 `KEY_ESCAPE` 是无条件 `hide_overlay()`（写法正确，可作参考）。

### 原因 2：松手检测是单点依赖，事件丢了就永远不隐藏（概率性）

隐藏完全依赖 event tap 收到修饰键松开的 `kCGEventFlagsChanged` 事件（`src/event_monitor.rs:105-115`）：

```rust
K_C_G_EVENT_FLAGS_CHANGED => {
    let flags = CGEventGetFlags(event);
    let mod_mask = if SHORTCUT_IS_CMD.load(Ordering::SeqCst) {
        K_C_G_EVENT_FLAG_MASK_COMMAND
    } else {
        K_C_G_EVENT_FLAG_MASK_ALTERNATE
    };
    if (flags & mod_mask) == 0 && TAB_PRESSED.swap(false, Ordering::SeqCst) {
        let _ = sender.send(GlobalEvent::CmdReleased);
    }
}
```

没有任何兜底（没有超时、没有轮询修饰键状态）。只要这一个 `FLAGS_CHANGED` 事件没被 tap 收到——事件合并、tap 被系统瞬时禁用、secure input、系统繁忙——`CmdReleased` 就不发，浮窗永远不隐藏。

加剧因素：回调的 `_ => {}` 把 `kCGEventTapDisabledByTimeout`(事件类型 14) / `kCGEventTapDisabledByUserInput`(13) 吞掉了，**tap 一旦被系统自动禁用就不会自我恢复**（`start` 里只在启动时 `CGEventTapEnable` 一次，`src/event_monitor.rs:142`），直到重启 App。

### 附带：主线程阻塞会让隐藏"延迟"（可能被感知成卡住）

连按 Tab 循环时，`on_cmd_tab_pressed` 的 else 分支会同步跑 `extract_uncached_icons`（含图标渲染 + `rebuild_cards`，都在主线程）。若此时松手，`CmdReleased` 经 `performSelectorOnMainThread` 入队，要等提取完才能处理 -> 浮窗多停留几百毫秒。这是"延迟消失"而非永久卡住，但体感上像"没消失"。

## 如何区分原因 1 和原因 2

原因 1 的 None 分支会向 stderr 打印：

```
[oh-my-tab] CmdReleased: selected index N out of bounds (windows=0)
```

从终端 `cargo run` 运行（日志才能看到），复现"卡住"那一刻：

- 终端出现上面这行 -> **原因 1**（空窗口列表）。
- 没出现 -> 基本是 **原因 2**（松手事件丢失）。

## 解决方案

### 修原因 1：松手时无条件隐藏

把 `hide_overlay()` 提到 `if let` 之外，无论是否选到窗口都先隐藏：

```rust
extern "C" fn on_cmd_released(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    let mut state_opt = TAB_STATE.lock().unwrap();
    let state = state_opt.as_mut().unwrap();
    if !state.visible { return; }

    // 无论是否选到窗口，松手都要隐藏
    let target = state.windows.get(state.selected).map(|w| (w.pid, w.window_id, w.window_title.clone(), w.app_name.clone()));
    state.visible = false;
    drop(state_opt);

    hide_overlay();

    if let Some((pid, wid, wt, app_name)) = target {
        println!("[oh-my-tab] Switching to '{}' (pid={})", app_name, pid);
        activate_pid(pid);
        raise_ax_window(pid, &wt);
        let mut state_opt = TAB_STATE.lock().unwrap();
        if let Some(state) = state_opt.as_mut() {
            state.mru.insert(wid, std::time::Instant::now());
        }
    }
}
```

`KEY_RETURN`、`card_mouse_down` 同理改造（先 `hide_overlay()` 再做后续）。

### 修原因 2：给松手检测加兜底

两层兜底：

1. **轮询修饰键状态**：浮窗呼出后启动一个定时器（或复用现有 run loop），用 `CGEventSourceFlagsState(kCGEventSourceStateCombinedSessionState, ...)` 读修饰键实际状态，发现已松开就主动发 `CmdReleased`。这样即使 tap 漏了 `FLAGS_CHANGED`，也能兜住。

   ```rust
   // 伪代码：呼出后每隔 ~100ms 检查一次修饰键
   let flags = CGEventSourceFlagsState(session_source, mod_mask);
   if (flags & mod_mask) == 0 && TAB_PRESSED.swap(false, Ordering::SeqCst) {
       let _ = sender.send(GlobalEvent::CmdReleased);
   }
   ```

2. **处理 tap 禁用事件**：回调里识别 `kCGEventTapDisabledByTimeout` / `kCGEventTapDisabledByUserInput`，调用 `CGEventTapEnable(tap, true)` 重新启用 tap，避免一次禁用就永久失效。

   ```rust
   match event_type {
       K_C_G_EVENT_KEY_DOWN => { /* ... */ }
       K_C_G_EVENT_FLAGS_CHANGED => { /* ... */ }
       // tap 被系统禁用时重启用
       t if t == 14 || t == 13 => {
           CGEventTapEnable(tap, true);
       }
       _ => {}
   }
   ```
   （注意：回调里需要拿到 `tap` 句柄，可经 `user_info` 传入。）

### 修附带：图标提取移出主线程

`extract_uncached_icons` 的图标渲染丢到后台线程，完成后回主线程调 `rebuild_cards`，避免连按 Tab 时阻塞主线程、延迟 `CmdReleased` 的处理。

## 优先级建议

1. 先修**原因 1**——改动小、确定性强，顺手把 `KEY_RETURN`/`card_mouse_down` 一起改对。
2. 再修**原因 2 的第 2 点**（tap 禁用重启用）——成本低，能挡住"tap 被禁用后永久失效"。
3. 若仍有偶发残留，再加**原因 2 的第 1 点**（修饰键状态轮询兜底）和**附带的图标提取异步化**。
