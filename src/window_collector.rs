use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::time::Instant;
use objc2::{class, msg_send};
use objc2::runtime::AnyObject;

#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub pid: i32,
    pub window_id: u32,
    pub app_name: String,
    pub window_title: String,
    pub icon_path: Option<String>,
    pub is_active: bool,
}

pub type MruMap = HashMap<u32, Instant>;

const ICON_CACHE_DIR: &str = "/tmp/oh-my-tab-icons";
const ICON_CACHE_TTL_SECS: u64 = 3600;

const K_C_G_WINDOW_LIST_OPTION_ON_SCREEN_ONLY: u32 = 1;

// AX types
type AXUIElementRef = *const c_void;
type AXError = i32;
const K_AX_SUCCESS: AXError = 0;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGWindowListCopyWindowInfo(option: u32, relative_to_window: u32) -> *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFArrayGetCount(array: *const c_void) -> isize;
    fn CFArrayGetValueAtIndex(array: *const c_void, index: isize) -> *const c_void;
    fn CFDictionaryGetValue(dict: *const c_void, key: *const c_void) -> *const c_void;
    fn CFStringCreateWithCString(
        alloc: *const c_void,
        c_str: *const i8,
        encoding: u32,
    ) -> *const c_void;
    fn CFNumberGetValue(number: *const c_void, the_type: isize, value: *mut c_void) -> bool;
    fn CFStringGetCString(
        string: *const c_void,
        buffer: *mut i8,
        buffer_size: isize,
        encoding: u32,
    ) -> bool;
    fn CFRelease(cf: *const c_void);
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: *const c_void,
        value: *mut *const c_void,
    ) -> AXError;
    fn AXUIElementPerformAction(element: AXUIElementRef, action: *const c_void) -> AXError;
    fn AXUIElementSetAttributeValue(
        element: AXUIElementRef,
        attribute: *const c_void,
        value: *const c_void,
    ) -> AXError;
}

fn cf_string_new(s: &str) -> *const c_void {
    let c_str = std::ffi::CString::new(s).unwrap();
    unsafe { CFStringCreateWithCString(std::ptr::null(), c_str.as_ptr(), 0x08000100) }
}

fn cf_dict_get_string(dict: *const c_void, key: &str) -> Option<String> {
    let cf_key = cf_string_new(key);
    let value = unsafe { CFDictionaryGetValue(dict, cf_key) };
    unsafe { CFRelease(cf_key) };
    if value.is_null() { return None; }
    cf_to_rust_string(value)
}

fn cf_dict_get_i32(dict: *const c_void, key: &str) -> Option<i32> {
    let cf_key = cf_string_new(key);
    let value = unsafe { CFDictionaryGetValue(dict, cf_key) };
    unsafe { CFRelease(cf_key) };
    if value.is_null() { return None; }
    let mut num: i32 = 0;
    let ok = unsafe { CFNumberGetValue(value, 3, &mut num as *mut i32 as *mut c_void) };
    if ok { Some(num) } else { None }
}

fn cf_dict_get_u32(dict: *const c_void, key: &str) -> Option<u32> {
    let cf_key = cf_string_new(key);
    let value = unsafe { CFDictionaryGetValue(dict, cf_key) };
    unsafe { CFRelease(cf_key) };
    if value.is_null() { return None; }
    let mut num: i32 = 0;
    let ok = unsafe { CFNumberGetValue(value, 3, &mut num as *mut i32 as *mut c_void) };
    if ok { Some(num as u32) } else { None }
}

pub fn ensure_icon_cache_dir() {
    let _ = std::fs::create_dir_all(ICON_CACHE_DIR);
}

pub fn check_icon_cache(pid: i32) -> Option<String> {
    let path = format!("{}/{}.png", ICON_CACHE_DIR, pid);
    let meta = std::fs::metadata(&path).ok()?;
    let age = meta.modified().ok()?.elapsed().ok()?;
    if age.as_secs() < ICON_CACHE_TTL_SECS {
        Some(path)
    } else {
        None
    }
}

fn write_png_to_cache(png: *mut AnyObject, pid: i32) -> Option<String> {
    unsafe {
        let path = format!("{}/{}.png", ICON_CACHE_DIR, pid);
        let path_cstr = std::ffi::CString::new(&*path).unwrap();
        let cf_path = CFStringCreateWithCString(std::ptr::null(), path_cstr.as_ptr(), 0x08000100);
        let ok: bool = msg_send![png, writeToFile: cf_path as *mut AnyObject, atomically: false];
        CFRelease(cf_path as *const c_void);
        if ok { Some(path) } else { None }
    }
}

pub fn extract_icon_to_cache(pid: i32) -> Option<String> {
    if let Some(path) = check_icon_cache(pid) {
        return Some(path);
    }
    unsafe {
        use objc2_foundation::{NSPoint, NSRect, NSSize};

        let cls = class!(NSRunningApplication);
        let app: *mut AnyObject = msg_send![cls, runningApplicationWithProcessIdentifier: pid];
        if app.is_null() { return None; }

        let icon: *mut AnyObject = msg_send![app, icon];
        if icon.is_null() { return None; }

        // Render at Retina resolution: 64pt display → 128px (2x) or 64px (1x)
        let scale: f64 = {
            let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
            if screen.is_null() { 2.0 }
            else { msg_send![screen, backingScaleFactor] }
        };
        let px = 128.0 * scale;

        let target_img: *mut AnyObject = msg_send![class!(NSImage), alloc];
        let target_img: *mut AnyObject = msg_send![target_img, initWithSize: NSSize::new(px, px)];

        // Draw icon into target with high-quality interpolation (NSImageInterpolationHigh)
        let _: () = msg_send![target_img, lockFocus];
        let dst = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(px, px));
        let src = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
        let op: usize = 1; // NSCompositingOperationCopy
        let _: () = msg_send![icon, drawInRect: dst, fromRect: src, operation: op, fraction: 1.0f64];
        let _: () = msg_send![target_img, unlockFocus];

        // Convert to PNG at target size
        let tiff: *mut AnyObject = msg_send![target_img, TIFFRepresentation];
        if tiff.is_null() { return None; }

        let rep_cls = class!(NSBitmapImageRep);
        let rep: *mut AnyObject = msg_send![rep_cls, imageRepWithData: tiff];
        if rep.is_null() { return None; }

        // NSBitmapImageFileTypePNG = 4
        let png: *mut AnyObject = msg_send![rep, representationUsingType: 4u64, properties: std::ptr::null::<AnyObject>()];
        if png.is_null() { return None; }

        write_png_to_cache(png, pid)
    }
}

pub fn raise_ax_window(pid: i32, window_title: &str) {
    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() { return; }

        let windows_key = cf_string_new("AXWindows");
        let mut windows_array: *const c_void = std::ptr::null();
        let err = AXUIElementCopyAttributeValue(app, windows_key, &mut windows_array);
        CFRelease(windows_key);
        if err != K_AX_SUCCESS || windows_array.is_null() { CFRelease(app); return; }

        let count = CFArrayGetCount(windows_array);
        let title_key = cf_string_new("AXTitle");
        let raise_key = cf_string_new("AXRaise");

        for i in 0..count {
            let element = CFArrayGetValueAtIndex(windows_array, i);
            if element.is_null() { continue; }
            let mut title_value: *const c_void = std::ptr::null();
            let err = AXUIElementCopyAttributeValue(element, title_key, &mut title_value);
            if err == K_AX_SUCCESS && !title_value.is_null() {
                if let Some(t) = cf_to_rust_string(title_value) {
                    if t == window_title {
                        let focused_key = cf_string_new("AXFocusedWindow");
                        AXUIElementSetAttributeValue(app, focused_key, element);
                        CFRelease(focused_key);
                        AXUIElementPerformAction(element, raise_key);
                        CFRelease(title_value);
                        break;
                    }
                }
                CFRelease(title_value);
            }
        }
        CFRelease(title_key);
        CFRelease(raise_key);
        CFRelease(windows_array);
        CFRelease(app);
    }
}

fn get_ax_windows_for_pid(pid: i32) -> Vec<String> {
    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() { return vec![]; }

        let windows_key = cf_string_new("AXWindows");
        let mut windows_array: *const c_void = std::ptr::null();
        let err = AXUIElementCopyAttributeValue(app, windows_key, &mut windows_array);
        CFRelease(windows_key);
        CFRelease(app);
        if err != K_AX_SUCCESS || windows_array.is_null() { return vec![]; }

        let count = CFArrayGetCount(windows_array);
        let title_key = cf_string_new("AXTitle");
        let subrole_key = cf_string_new("AXSubrole");
        let mut results = Vec::with_capacity(count as usize);

        for i in 0..count {
            let element = CFArrayGetValueAtIndex(windows_array, i);
            if element.is_null() { continue; }

            // 只保留标准窗口（AXStandardWindow），过滤弹出面板/下拉菜单等非标准窗口
            // Only keep AXStandardWindow, filtering out popups/panels/dropdowns
            let mut subrole_value: *const c_void = std::ptr::null();
            let is_standard = if AXUIElementCopyAttributeValue(element, subrole_key, &mut subrole_value) == K_AX_SUCCESS && !subrole_value.is_null() {
                let s = cf_to_rust_string(subrole_value);
                CFRelease(subrole_value);
                s.map_or(true, |sr| sr == "AXStandardWindow")
            } else {
                // 无 subrole → 视为标准窗口（部分 App 不设置此属性）
                // No subrole means standard window for apps that don't set it
                true
            };
            if !is_standard { continue; }

            let mut title_value: *const c_void = std::ptr::null();
            let title = if AXUIElementCopyAttributeValue(element, title_key, &mut title_value) == K_AX_SUCCESS && !title_value.is_null() {
                let t = cf_to_rust_string(title_value);
                CFRelease(title_value);
                t.unwrap_or_default()
            } else {
                String::new()
            };
            results.push(title);
        }
        CFRelease(title_key);
        CFRelease(subrole_key);
        CFRelease(windows_array);
        results
    }
}

fn cf_to_rust_string(cf_string: *const c_void) -> Option<String> {
    let mut buf = vec![0u8; 1024];
    let ok = unsafe { CFStringGetCString(cf_string, buf.as_mut_ptr() as *mut i8, buf.len() as isize, 0x08000100) };
    if ok {
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        Some(String::from_utf8_lossy(&buf[..end]).to_string())
    } else {
        None
    }
}

pub fn collect_windows(mru: &mut MruMap) -> Vec<WindowInfo> {
    let array = unsafe { CGWindowListCopyWindowInfo(K_C_G_WINDOW_LIST_OPTION_ON_SCREEN_ONLY, 0) };
    if array.is_null() { return vec![]; }

    let self_pid = std::process::id() as i32;
    let mut windows: Vec<WindowInfo> = Vec::new();
    let count = unsafe { CFArrayGetCount(array) };
    let now = Instant::now();
    let mut insertion_order: u32 = 0;

    // 第一遍遍历：收集所有 PID，用于批量查询 AX 窗口
    // First pass: collect all PIDs to batch query AX windows
    let mut pids: HashSet<i32> = HashSet::new();
    for i in 0..count {
        let dict = unsafe { CFArrayGetValueAtIndex(array, i) };
        if dict.is_null() { continue; }
        let layer = cf_dict_get_i32(dict, "kCGWindowLayer").unwrap_or(999);
        if layer != 0 { continue; }
        let owner_pid = cf_dict_get_i32(dict, "kCGWindowOwnerPID").unwrap_or(-1);
        if owner_pid <= 0 || owner_pid == self_pid { continue; }
        let owner_name = cf_dict_get_string(dict, "kCGWindowOwnerName").unwrap_or_default();
        if owner_name.is_empty() || owner_name == "Dock" { continue; }
        pids.insert(owner_pid);
    }

    // 以 AX 窗口列表为主数据源（macOS App Switcher 的做法）
    // Use AX window list as primary source (same as macOS App Switcher)
    let mut ax_by_pid: HashMap<i32, Vec<String>> = HashMap::new();
    for &pid in &pids {
        let ax_wins = get_ax_windows_for_pid(pid);
        if !ax_wins.is_empty() {
            ax_by_pid.insert(pid, ax_wins);
        }
    }

    for i in 0..count {
        let dict = unsafe { CFArrayGetValueAtIndex(array, i) };
        if dict.is_null() { continue; }

        let layer = cf_dict_get_i32(dict, "kCGWindowLayer").unwrap_or(999);
        if layer != 0 { continue; }

        let owner_pid = cf_dict_get_i32(dict, "kCGWindowOwnerPID").unwrap_or(-1);
        if owner_pid <= 0 || owner_pid == self_pid { continue; }

        let owner_name = cf_dict_get_string(dict, "kCGWindowOwnerName").unwrap_or_default();
        if owner_name.is_empty() || owner_name == "Dock" { continue; }

        let cg_title = cf_dict_get_string(dict, "kCGWindowName").unwrap_or_default();
        let window_id = cf_dict_get_u32(dict, "kCGWindowNumber").unwrap_or(0);

        // AX 为权威数据源：CG 窗口必须匹配 AX 窗口才保留
        // AX is authoritative: CG windows must match an AX window to be included
        let window_title = if let Some(ax_wins) = ax_by_pid.get(&owner_pid) {
            if cg_title.is_empty() {
                // CG 标题为空 → 分配第一个未使用的 AX 标题
                // Empty CG title → assign first unused AX title
                let mut found: Option<String> = None;
                for ax_title in ax_wins {
                    if !ax_title.is_empty() && !windows.iter().any(|w: &WindowInfo| w.pid == owner_pid && w.window_title == *ax_title) {
                        found = Some(ax_title.clone());
                        break;
                    }
                }
                found.unwrap_or_default()
            } else {
                // CG 标题必须在 AX 列表中存在，否则是弹出面板/下拉菜单
                // CG title must exist in AX list; otherwise it's a popup/panel
                if ax_wins.iter().any(|t| *t == cg_title) {
                    cg_title
                } else {
                    continue; // CG 窗口不在 AX 中 → 弹出面板，跳过
                              // CG window not in AX → popup/panel, skip
                }
            }
        } else {
            // No AX data for this app → fall back to CG title
            cg_title
        };

        if window_title.is_empty() && !ax_by_pid.contains_key(&owner_pid) {
            // Keep windows from apps without AX support even if title is empty
        } else if window_title.is_empty() {
            continue; // empty title only valid for non-AX apps
        }

        let ordered_ts = now.checked_sub(std::time::Duration::from_millis(insertion_order as u64)).unwrap_or(now);
        mru.entry(window_id).or_insert(ordered_ts);
        insertion_order += 1;
        let icon_path = check_icon_cache(owner_pid);
        windows.push(WindowInfo { pid: owner_pid, window_id, app_name: owner_name, window_title, icon_path, is_active: false });
    }

    unsafe { CFRelease(array) };

    windows.sort_by(|a, b| {
        let ta = mru.get(&a.window_id).map(|t| t.elapsed()).unwrap_or(std::time::Duration::from_secs(999));
        let tb = mru.get(&b.window_id).map(|t| t.elapsed()).unwrap_or(std::time::Duration::from_secs(999));
        ta.cmp(&tb)
    });

    if let Some(first) = windows.first_mut() { first.is_active = true; }
    windows
}
