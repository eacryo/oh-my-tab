//! 应用图标磁盘缓存:`~/Library/Caches/oh-my-tab-icons/`。
//! 键 = AppIdentity(bundle id > exec 路径哈希 > pid 兜底),`.meta` sidecar 存
//! 可执行文件 mtime 做失效指纹;切换器大图(128pt)与剪贴板小图(16pt)共用
//! 同一套提取管线与指纹。
//!
//! The app icon disk cache under `~/Library/Caches/oh-my-tab-icons/`. Keys come
//! from AppIdentity (bundle id > hashed exec path > pid fallback); a `.meta`
//! sidecar stores the executable mtime as the invalidation fingerprint. The
//! switcher's big icon (128pt) and the clipboard's small one (16pt) share this
//! pipeline and fingerprint.

use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use std::ffi::c_void;

use crate::ffi::{CFRelease, CFStringCreateWithCString};
use crate::log_debug;
use crate::window_collector::{resolve_app_identity, AppIdentity};

fn icon_cache_dir() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    // 测试构建使用与真实缓存同级的专用目录:冒烟测试里的 clear_icon_cache()
    // 只清测试目录,绝不触碰用户的真实图标缓存(曾清空真实缓存导致下次
    // summon 全部重提取,卡 ~400ms)。
    // Test builds use a dedicated sibling directory: the smoke tests' clear_icon_cache()
    // only clears the test dir, never the user's real icon cache (clearing the real one
    // used to force a full re-extract on the next summon, stalling ~400ms).
    let name = if cfg!(test) {
        "oh-my-tab-icons-test"
    } else {
        "oh-my-tab-icons"
    };
    format!("{}/Library/Caches/{}", home, name)
}

/// 缓存 PNG 路径(支持后缀:"" = 切换器大图,".small" = 剪贴板小图)。
/// The cached PNG path (suffix-aware: "" = the switcher's big icon, ".small" = the
/// clipboard's small one).
fn cache_path_for_key_suffix(key: &str, suffix: &str) -> String {
    format!("{}/{}{}.png", icon_cache_dir(), key, suffix)
}

fn meta_path_for_key(key: &str) -> String {
    format!("{}/{}.meta", icon_cache_dir(), key)
}

/// 校验缓存是否有效:PNG 存在,且(若有指纹)sidecar 指纹与当前一致。
/// App 更新会换 mtime -> 指纹不符 -> 返回 None,触发重提。
///
/// Validate the cache: PNG exists, and (when a fingerprint is present) the sidecar
/// matches. An app update changes the mtime -> fingerprint mismatch -> None -> re-extract.
pub(crate) fn check_cache_for_identity(id: &AppIdentity) -> Option<String> {
    check_cache_for_suffix(id, "")
}

/// 同上,支持小图后缀(.small);大小图共享同一份 .meta 指纹。
/// Same as above, suffix-aware for the small icon; both sizes share one .meta fingerprint.
fn check_cache_for_suffix(id: &AppIdentity, suffix: &str) -> Option<String> {
    let png = cache_path_for_key_suffix(&id.key, suffix);
    if std::fs::metadata(&png).is_err() {
        return None;
    }
    match &id.fingerprint {
        Some(fp) => match std::fs::read_to_string(meta_path_for_key(&id.key)) {
            Ok(stored) if stored.trim() == *fp => Some(png),
            _ => None,
        },
        None => Some(png), // 无指纹(极端兜底)-> 文件存在即有效 / no fingerprint -> file exists = valid
    }
}

pub fn ensure_icon_cache_dir() {
    let _ = std::fs::create_dir_all(icon_cache_dir());
}

/// 一次性迁移:删除旧版按 PID 命名的缓存文件(文件名 stem 纯数字)。
/// 新版键为 bundle id(含字母/点)或 `exec_`/`pid_` 前缀,绝不会是纯数字,故不会误删。
/// One-shot migration: remove legacy PID-named cache files (purely-numeric filename stem).
/// New keys are bundle ids (letters/dots) or `exec_`/`pid_`-prefixed, never purely numeric,
/// so nothing legitimate is touched.
pub fn migrate_legacy_cache() {
    let Ok(entries) = std::fs::read_dir(icon_cache_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // 只删 .png 且 stem 纯数字的旧 PID 文件。
        // Only remove .png files whose stem is purely numeric (legacy PID files).
        if path.extension().and_then(|e| e.to_str()) == Some("png") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                if !stem.is_empty() && stem.bytes().all(|b| b.is_ascii_digit()) {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
}

/// 清空图标缓存目录(删除所有 {key}.png + {key}.meta),然后重建空目录。
/// 内存里 WindowInfo.icon_path 不会自动失效,调用方需自行将其置 None 并触发重提取。
///
/// Clear the icon cache directory (remove all {key}.png + {key}.meta), then recreate it empty.
/// In-memory WindowInfo.icon_path is NOT invalidated here; the caller must reset it to None
/// and trigger re-extraction.
pub fn clear_icon_cache() {
    let dir = icon_cache_dir();
    // remove_dir_all 在目录不存在时会报错,忽略即可 / errors if the dir doesn't exist; ignore
    let _ = std::fs::remove_dir_all(&dir);
    ensure_icon_cache_dir();
}

/// 图标缓存按应用 bundle id 索引(非 bundle 应用回退到可执行文件路径),不再按 PID:
/// PID 复用不会再读到别的 App 的旧图标。每个条目配一个 `.meta` sidecar 存可执行文件
/// mtime,App 更新/重装会换 mtime -> 校验不符 -> 重新提取,因此无需 TTL。运行时改图标
/// 的 App(日历日期 / Dock 角标)仍会冻结到下次清缓存--这是已接受的次要限制。
///
/// The icon cache is keyed by the app's bundle id (falling back to the executable path for
/// non-bundle apps), NOT by PID: PID recycling can never serve another app's stale icon. Each
/// entry has a `.meta` sidecar storing the executable mtime; an app update/reinstall changes
/// the mtime -> verification fails -> re-extract, so no TTL is needed. Apps that change their
/// icon at runtime (Calendar date, dock badge) still freeze until the cache is cleared - an
/// accepted minor limitation.
pub fn check_icon_cache(pid: i32) -> Option<String> {
    let id = unsafe { resolve_app_identity(pid) };
    check_cache_for_identity(&id)
}

fn write_png_to_cache(png: *mut AnyObject, key: &str, suffix: &str) -> Option<String> {
    unsafe {
        let path = cache_path_for_key_suffix(key, suffix);
        let path_cstr = std::ffi::CString::new(&*path).unwrap();
        let cf_path = CFStringCreateWithCString(std::ptr::null(), path_cstr.as_ptr(), 0x08000100);
        // 原子写入：先写临时文件再重命名，避免写一半崩溃留下半截 PNG。
        // Atomic write: write to a temp file then rename, so a mid-write crash
        // can't leave a half-written PNG that "file exists = valid" would trust.
        let ok: bool = msg_send![png, writeToFile: cf_path as *mut AnyObject, atomically: true];
        CFRelease(cf_path);
        if ok {
            Some(path)
        } else {
            None
        }
    }
}

/// 提取图标到缓存(按目标 pt 尺寸渲染):切换器大图(128pt)与剪贴板小图(16pt)共用管线。
/// `suffix`: 文件名后缀("" = {key}.png,".small" = {key}.small.png),大小图共享同一份
/// {key}.meta 指纹(同一可执行文件 mtime)。
/// Extract an app icon into the cache at the target point size: the switcher's big icon
/// (128pt) and the clipboard's small one (16pt) share this pipeline. `suffix`: the filename
/// suffix ("" -> {key}.png, ".small" -> {key}.small.png); both sizes share one {key}.meta
/// fingerprint (the same executable mtime).
fn extract_icon_to_cache_sized(pid: i32, pt_size: f64, suffix: &str) -> Option<String> {
    unsafe {
        use objc2_foundation::{NSPoint, NSRect, NSSize};

        // 包一个 autorelease 池：app/icon/tiff/rep/png 都是 autoreleased（+0）。
        // 启动早期在 NSApp run 之前调用时主线程还没有池子，这些对象（尤其源 icon，可达数 MB）
        // 会整体泄漏——这是启动 ~40MB 的主因。
        // Wrap in an autorelease pool: app/icon/tiff/rep/png are autoreleased (+0). At startup
        // this runs before NSApp run (no pool yet), so they'd all leak - the ~40MB startup cause.
        let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];

        let id = resolve_app_identity(pid);
        // 命中既有且有效的缓存(含 mtime 校验)-> 跳过提取。
        // Hit an existing valid cache (mtime-verified) -> skip extraction.
        if let Some(path) = check_cache_for_suffix(&id, suffix) {
            let _: () = msg_send![pool, drain];
            return Some(path);
        }

        // 源图标:自身进程用编译期嵌入的 AppIcon.icns--cargo run 是裸 exec 无 bundle,
        // NSRunningApplication.icon 会返回通用 exec 图标(带 EXEC 字样);这里强制用我们
        // 自己的图标,开发与打包表现一致。其他进程仍走 NSRunningApplication.icon。
        //
        // Source icon: for our own process use the compile-time-embedded AppIcon.icns --
        // cargo run is a bare exec with no bundle, so NSRunningApplication.icon returns the
        // generic exec icon (the "EXEC" placeholder); this forces our own icon so dev and
        // bundled builds match. Other processes still go through NSRunningApplication.icon.
        let icon: *mut AnyObject = if pid == std::process::id() as i32 {
            let icns_bytes: &[u8] = include_bytes!("../assets/AppIcon.icns");
            let nsdata: *mut AnyObject = msg_send![
                class!(NSData),
                dataWithBytes: icns_bytes.as_ptr() as *const c_void,
                length: icns_bytes.len()
            ];
            // NSImage 没有 +imageWithData: 类方法,用 alloc + initWithData:(+1)再 autorelease,
            // 让它和下面的 app.icon 一样由池子回收,无需手动 release。
            // NSImage has no +imageWithData: class method; use alloc + initWithData: (+1) then
            // autorelease so it's pool-managed like app.icon below, with no manual release.
            let img: *mut AnyObject = msg_send![class!(NSImage), alloc];
            let img: *mut AnyObject = msg_send![img, initWithData: nsdata];
            if !img.is_null() {
                let _: *mut AnyObject = msg_send![img, autorelease];
            }
            img
        } else {
            let cls = class!(NSRunningApplication);
            let app: *mut AnyObject = msg_send![cls, runningApplicationWithProcessIdentifier: pid];
            if app.is_null() {
                let _: () = msg_send![pool, drain];
                return None;
            }
            msg_send![app, icon]
        };
        if icon.is_null() {
            let _: () = msg_send![pool, drain];
            return None;
        }

        // Render at Retina resolution: pt_size pt display → 2x (or 1x) pixels.
        let scale: f64 = {
            let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
            if screen.is_null() {
                2.0
            } else {
                msg_send![screen, backingScaleFactor]
            }
        };
        let px = pt_size * scale;

        let target_img: *mut AnyObject = msg_send![class!(NSImage), alloc];
        let target_img: *mut AnyObject = msg_send![target_img, initWithSize: NSSize::new(px, px)];

        // Draw icon into target with high-quality interpolation (NSImageInterpolationHigh)
        let _: () = msg_send![target_img, lockFocus];
        let dst = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(px, px));
        let src = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
        let op: usize = 1; // NSCompositingOperationCopy
        let _: () =
            msg_send![icon, drawInRect: dst, fromRect: src, operation: op, fraction: 1.0f64];
        let _: () = msg_send![target_img, unlockFocus];

        // Convert to PNG at target size
        let tiff: *mut AnyObject = msg_send![target_img, TIFFRepresentation];
        let _: () = msg_send![target_img, release]; // target_img 是 alloc 出来的 +1，池子接不住，手动 release
        if tiff.is_null() {
            let _: () = msg_send![pool, drain];
            return None;
        }

        let rep_cls = class!(NSBitmapImageRep);
        let rep: *mut AnyObject = msg_send![rep_cls, imageRepWithData: tiff];
        if rep.is_null() {
            let _: () = msg_send![pool, drain];
            return None;
        }

        // NSBitmapImageFileTypePNG = 4
        let png: *mut AnyObject = msg_send![rep, representationUsingType: 4u64, properties: std::ptr::null::<AnyObject>()];
        if png.is_null() {
            let _: () = msg_send![pool, drain];
            return None;
        }

        let result = write_png_to_cache(png, &id.key, suffix);
        // 写 mtime sidecar:下次命中时据此判断 App 是否更新过(mtime 变 -> 重提)。
        // 仅在 PNG 写成功时写,避免留下无 PNG 的孤儿 meta。
        // Write the mtime sidecar: next hit checks it to detect app updates (mtime change ->
        // re-extract). Only written when the PNG succeeds, so no orphan meta is left behind.
        if result.is_some() {
            if let Some(fp) = &id.fingerprint {
                let _ = std::fs::write(meta_path_for_key(&id.key), fp);
            }
        }
        let _: () = msg_send![pool, drain];
        result
    }
}

pub fn extract_icon_to_cache(pid: i32) -> Option<String> {
    extract_icon_to_cache_sized(pid, 128.0, "")
}

/// 剪贴板标题栏的小图标(16pt,2x = 32px)。记录来源时调用(app 此刻必存活);
/// 键与 .meta 指纹和切换器大图共用,`{key}.small.png` 独立于大图文件。
/// The clipboard header's small icon (16pt, 32px @2x). Called when the source is recorded
/// (the app is guaranteed alive then); the key and .meta fingerprint are shared with the
/// switcher's big icon, while `{key}.small.png` is a separate file.
pub fn extract_small_icon(pid: i32) -> Option<String> {
    extract_icon_to_cache_sized(pid, 16.0, ".small")
}

/// 剪贴板小图的路径(存在性检查用;key = resolve_app_identity 的缓存键)。
/// The clipboard small-icon path (for existence checks; key = resolve_app_identity's key).
pub fn small_icon_path_for_key(key: &str) -> String {
    cache_path_for_key_suffix(key, ".small")
}

/// Pre‑cache icons for every currently‑running regular application.
/// Called once at startup so the overlay never shows a missing icon.
///
/// 只处理 .regular 策略的应用:嵌套 helper 后台进程(如像素蛋糕的 pix-worker/
/// pix-camera-link)与主应用共用 bundle id,其 NSRunningApplication.icon 是
/// AppKit 通用占位图,提取会以相同缓存键污染主应用图标,且 helper 与主程序
/// 二进制 mtime 相同(同一安装包),meta 校验无法察觉——错误图标会整个会话
/// 有效。helper 无窗口,其图标本就不需要。
///
/// Pre-cache icons for every currently-running REGULAR application. Helper
/// background processes (e.g. PixCake's nested pix-worker/pix-camera-link) share
/// the main app's bundle id, and their NSRunningApplication.icon is the AppKit
/// generic placeholder -- extracting poisons the shared cache key, and since the
/// helpers' binary mtimes equal the main binary's (same install), the .meta check
/// can never detect it. Helpers have no windows; their icons are never needed.
pub(crate) fn cache_running_app_icons() {
    let mut cached: Vec<String> = Vec::new();
    let mut skipped: usize = 0;
    unsafe {
        // 本函数在 NSApp run 之前调用，主线程还没有 autorelease 池；
        // runningApplications / localizedName 都是 autoreleased，套个池子及时回收。
        // This runs before NSApp run, when the main thread has no autorelease pool yet;
        // runningApplications / localizedName are autoreleased, so wrap in a pool to drain them.
        let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let running: *mut AnyObject = msg_send![workspace, runningApplications];
        let count: usize = msg_send![running, count];
        for i in 0..count {
            let app: *mut AnyObject = msg_send![running, objectAtIndex: i];
            // NSApplicationActivationPolicyRegular = 0;后台/辅助进程跳过。
            // NSApplicationActivationPolicyRegular = 0; skip background/helper apps.
            let policy: i64 = msg_send![app, activationPolicy];
            if policy != 0 {
                skipped += 1;
                continue;
            }
            let pid: i32 = msg_send![app, processIdentifier];
            if check_icon_cache(pid).is_none() {
                let name_str = crate::ffi::ns_running_app_name(app);
                let name_str = if name_str.is_empty() {
                    "?".to_string()
                } else {
                    name_str
                };
                log_debug!("cached icon: {} (pid {})", name_str, pid);
                cached.push(name_str);
                extract_icon_to_cache(pid);
            } else {
                skipped += 1;
            }
        }
        let _: () = msg_send![pool, drain];
    }
    log_debug!(
        "icon cache done: {} cached, {} skipped (already fresh / non-regular)",
        cached.len(),
        skipped,
    );
}

/// 启动时预热剪贴板标题栏的小图标(16pt)。仅当配置开启剪贴板时才调用(main.rs 门控),
/// 否则小图标缓存不会被生成——剪贴板功能关闭时没必要为每个运行应用提取。
/// Pre-warm the clipboard header's small icons (16pt) at startup. Only called when the
/// clipboard feature is enabled (gated in main.rs); the small cache stays ungenerated when
/// the feature is off -- extracting it for every running app would be wasted work.
pub(crate) fn cache_running_app_icons_small() {
    let mut cached: Vec<String> = Vec::new();
    unsafe {
        // 与 cache_running_app_icons 同理:NSApp run 之前主线程没有 autorelease 池。
        // Same as cache_running_app_icons: no autorelease pool before NSApp run.
        let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let running: *mut AnyObject = msg_send![workspace, runningApplications];
        let count: usize = msg_send![running, count];
        for i in 0..count {
            let app: *mut AnyObject = msg_send![running, objectAtIndex: i];
            // 同 cache_running_app_icons:跳过后台/辅助进程(helper 图标会污染共享缓存键)。
            // Same as cache_running_app_icons: skip background/helper apps (their icons
            // would poison the shared cache key).
            let policy: i64 = msg_send![app, activationPolicy];
            if policy != 0 {
                continue;
            }
            let pid: i32 = msg_send![app, processIdentifier];
            // extract_small_icon 内部按 {key}.small.png + mtime 指纹校验,命中即跳过。
            // extract_small_icon verifies {key}.small.png + the mtime fingerprint, hitting
            // the cache skips the work.
            if extract_small_icon(pid).is_some() {
                cached.push(pid.to_string());
            }
        }
        let _: () = msg_send![pool, drain];
    }
    log_debug!(
        "small icon cache done: {} cached/verified (clipboard)",
        cached.len()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_icon_path_uses_dot_small_suffix() {
        // 剪贴板小图 = {key}.small.png,与切换器大图({key}.png)同 key 同目录。
        // The clipboard small icon = {key}.small.png, same key and dir as the switcher's
        // big icon ({key}.png).
        let key = "com.apple.Safari";
        let path = small_icon_path_for_key(key);
        assert!(path.ends_with(&format!("{}.small.png", key)), "{}", path);
        assert!(!path.contains("..png"));
        // 大小图路径只差后缀。
        // The big/small paths differ only by the suffix.
        let big = cache_path_for_key_suffix(key, "");
        assert!(big.ends_with(&format!("{}.png", key)), "{}", big);
        assert_eq!(path, format!("{}.small.png", &big[..big.len() - 4]));
    }

    #[test]
    #[ignore]
    fn icon_cache_roundtrip_smoke() {
        if !crate::ffi::has_accessibility_permission() {
            eprintln!("[smoke] Accessibility not granted; skipping icon roundtrip");
            return;
        }
        // 用 Finder(bundle id 稳定)做往返——测试二进制自身是裸 exec,身份解析不可靠。
        // Use Finder (stable bundle id) for the roundtrip; the test binary itself is a bare
        // exec whose identity resolution is unreliable.
        let pid = unsafe {
            let ns_key = crate::ffi::make_nsstring("com.apple.finder");
            let apps: *mut AnyObject = msg_send![
                class!(NSRunningApplication),
                runningApplicationsWithBundleIdentifier: ns_key
            ];
            CFRelease(ns_key as *const c_void);
            let count: usize = msg_send![apps, count];
            let mut pid: i32 = 0;
            if count > 0 {
                let app: *mut AnyObject = msg_send![apps, objectAtIndex: 0usize];
                pid = msg_send![app, processIdentifier];
            }
            pid
        };
        assert!(pid > 0, "Finder must be running in a GUI session");
        // 清空缓存从干净状态开始(冒烟测试会重提图标,是可接受的副作用)。
        // Start clean by clearing the cache (the smoke test re-extracts; acceptable side effect).
        clear_icon_cache();
        let path = extract_icon_to_cache(pid).expect("Finder icon extraction failed");
        assert!(std::fs::metadata(&path).is_ok(), "extracted PNG must exist");
        // 再次查询应命中缓存,幂等返回同一路径。
        // A second query hits the cache; idempotent same path.
        assert_eq!(check_icon_cache(pid).as_deref(), Some(path.as_str()));
        assert_eq!(
            extract_icon_to_cache(pid).as_deref(),
            Some(path.as_str()),
            "re-extract must short-circuit on a valid cache"
        );
        clear_icon_cache();
    }
}
