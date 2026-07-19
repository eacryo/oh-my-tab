# 窗口激活方案对比

当前采用 `osascript` + System Events 按 PID 激活窗口（`src/main.rs:55-65`）。
优点是可靠、不受应用本地化名称影响。
缺点是首次调用会触发 macOS Automation 权限弹窗（`"oh-my-tab" wants to control "System Events"`）。

## 方案 A：`NSRunningApplication activateWithOptions:`（无需额外权限）

```rust
use objc2::{class, msg_send, sel};
use objc2::runtime::AnyObject;
use std::ffi::c_void;

fn activate_pid(pid: i32) {
    unsafe {
        let cls = class!(NSRunningApplication);
        let app: *mut AnyObject = msg_send![cls, runningApplicationWithProcessIdentifier: pid];
        if !app.is_null() {
            // activateWithOptions: 返回 void，objc2 0.6 的 msg_send! 在处理 void 返回时
            // 有类型推断问题（会把 () 误判为 BOOL）。需要用 extern "C" 声明
            // 正确签名的 objc_msgSend 来绕过：
            let sel = sel!(activateWithOptions:);

            #[link(name = "objc", kind = "dylib")]
            extern "C" {
                fn objc_msgSend(obj: *mut c_void, sel: *mut c_void, opts: isize);
            }

            objc_msgSend(
                app as *mut c_void,
                &sel as *const _ as *mut c_void,
                1, // NSApplicationActivateIgnoringOtherApps
            );
        }
    }
}
```

- **优点**：不需要额外系统权限（已被 Accessibility 覆盖），直接 API 调用无子进程开销
- **缺点**：需要对 `objc_msgSend` 做 FFI 声明（objc2 0.6 的 `msg_send!` 宏在 void 返回方法上有类型编码推断 bug）
- **适用场景**：如果后续不再需要 System Events 权限，可切换到此方案减少权限弹窗

## 方案 B：`open -a`（已废弃）

```rust
std::process::Command::new("open").arg("-a").arg(&app_name).spawn().ok();
```

- **优点**：最简单，无需特殊权限
- **缺点**：对中文/本地化名称的应用匹配失败（如"微信"对应 bundle name "WeChat"）
- **已弃用原因**：无法可靠激活本地化名称的应用

## 方案 C：`osascript` + System Events（当前方案）

```rust
fn activate_pid(pid: i32) {
    let script = format!(
        "tell application \"System Events\" to set frontmost of first process whose unix id is {} to true",
        pid
    );
    std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .spawn()
        .ok();
}
```

- **优点**：可靠激活任意 PID，不受应用名称/本地化影响
- **缺点**：触发 Automation 权限弹窗，子进程开销
