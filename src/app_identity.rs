//! App 身份解析 · 独立下沉模块
//! ----------------------------------------------------------------------------
//! `AppIdentity` + `resolve_app_identity` 原先定义在 window_collector 中,而
//! icon_cache(被 window_collector 依赖)又需要它判断缓存键,形成模块环。下沉为
//! 独立模块后,window_collector / icon_cache / clipboard 全部单向向下依赖本模块。
//!
//! App identity resolution, sunk into its own module. It used to live in
//! window_collector while icon_cache (a window_collector dependency) also needed
//! it for cache keys, forming a module cycle. Both sides now depend one-way on
//! this module instead.

use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use std::ffi::{c_char, CStr};

use crate::hash::fnv1a64_hex;

/// 一个运行中 App 的缓存身份:`key` 用作缓存文件名,`fingerprint` 用于检测 App 更新。
/// A running app's cache identity: `key` is the cache filename, `fingerprint` detects updates.
pub(crate) struct AppIdentity {
    pub(crate) key: String,
    /// 可执行文件 mtime(自 UNIX epoch 的秒数)。None 表示无法校验,退化为「文件存在即有效」。
    /// Executable mtime (seconds since UNIX epoch). None means unverified -> "file exists = valid".
    pub(crate) fingerprint: Option<String>,
    /// 进程启动时间(自 UNIX epoch 起的微秒),用于区分 PID 复用后的不同进程实例。
    /// Process start time in microseconds since UNIX epoch, used to distinguish PID reuse.
    pub(crate) process_start_time_us: Option<u64>,
}

/// 读一个 NSString 到 Rust String(nil -> None)。对象是 autoreleased,调用方需在池内。
/// Read an NSString into a Rust String (nil -> None). The object is autoreleased; caller must be in a pool.
unsafe fn read_nsstring(obj: *mut AnyObject) -> Option<String> {
    if obj.is_null() {
        return None;
    }
    let utf8: *const c_char = msg_send![obj, UTF8String];
    if utf8.is_null() {
        return None;
    }
    Some(CStr::from_ptr(utf8).to_string_lossy().into_owned())
}

/// 解析一个 PID 对应 App 的缓存身份。
/// 键优先级:bundleIdentifier(reverse-DNS,文件名安全)> 可执行文件路径哈希 > `pid_{pid}` 兜底。
/// 指纹取可执行文件 mtime;App 更新会换新 mtime -> 触发重提。
/// 剪贴板记录来源时也复用此身份(与切换器同一套键/回退)。
///
/// Resolve a PID's cache identity. Key priority: bundleIdentifier (reverse-DNS,
/// filename-safe) > hashed executable path > `pid_{pid}` fallback. Fingerprint is the
/// executable mtime; an app update gets a new mtime -> forces re-extract. The clipboard
/// reuses this identity when recording a source (the same key/fallback chain as the switcher).
pub(crate) unsafe fn resolve_app_identity(pid: i32) -> AppIdentity {
    let app: *mut AnyObject =
        msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid];
    if app.is_null() {
        // PID 已失效(App 刚退出)-> 回退到 pid 键,无指纹(无法校验)。
        // PID stale (app just quit) -> fall back to pid key, no fingerprint (can't verify).
        return AppIdentity {
            key: format!("pid_{}", pid),
            fingerprint: None,
            process_start_time_us: None,
        };
    }

    let process_start_time_us = {
        let launch_date: *mut AnyObject = msg_send![app, launchDate];
        if launch_date.is_null() {
            None
        } else {
            let seconds: f64 = msg_send![launch_date, timeIntervalSince1970];
            (seconds.is_finite() && seconds >= 0.0).then_some((seconds * 1_000_000.0) as u64)
        }
    };

    let bid_obj: *mut AnyObject = msg_send![app, bundleIdentifier];
    let bundle_id = read_nsstring(bid_obj);

    let exec_url: *mut AnyObject = msg_send![app, executableURL];
    let exec_path = if exec_url.is_null() {
        None
    } else {
        let path_obj: *mut AnyObject = msg_send![exec_url, path];
        read_nsstring(path_obj)
    };

    // 指纹 = 可执行文件 mtime(秒)。取不到(路径为空 / stat 失败)-> None,退化为不校验。
    // Fingerprint = exec mtime (seconds). Unavailable (empty path / stat fail) -> None, no verification.
    let fingerprint = exec_path.as_ref().and_then(|p| {
        std::fs::metadata(p)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs().to_string())
    });

    let key = if let Some(bid) = bundle_id {
        bid
    } else if let Some(p) = exec_path {
        format!("exec_{}", fnv1a64_hex(&p))
    } else {
        format!("pid_{}", pid)
    };

    AppIdentity {
        key,
        fingerprint,
        process_start_time_us,
    }
}
