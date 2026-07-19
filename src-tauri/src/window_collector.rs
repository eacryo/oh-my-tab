use std::collections::HashMap;
use std::ffi::c_void;
use std::time::Instant;

use base64::Engine;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use parking_lot::Mutex;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct WindowInfo {
    pub pid: i32,
    pub window_id: u32,
    pub app_name: String,
    pub window_title: String,
    pub icon_base64: String,
    pub is_active: bool,
}

pub type MruMap = HashMap<i32, Instant>;

const K_C_G_WINDOW_LIST_OPTION_ON_SCREEN_ONLY: u32 = 1;

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

fn cf_string_new(s: &str) -> *const c_void {
    let c_str = std::ffi::CString::new(s).unwrap();
    unsafe { CFStringCreateWithCString(std::ptr::null(), c_str.as_ptr(), 0x08000100) }
}

fn cf_dict_get_string(dict: *const c_void, key: &str) -> Option<String> {
    let cf_key = cf_string_new(key);
    let value = unsafe { CFDictionaryGetValue(dict, cf_key) };
    unsafe { CFRelease(cf_key) };
    if value.is_null() {
        return None;
    }
    let mut buf = vec![0u8; 1024];
    let ok = unsafe {
        CFStringGetCString(
            value,
            buf.as_mut_ptr() as *mut i8,
            buf.len() as isize,
            0x08000100,
        )
    };
    if ok {
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        Some(String::from_utf8_lossy(&buf[..end]).to_string())
    } else {
        None
    }
}

fn cf_dict_get_i32(dict: *const c_void, key: &str) -> Option<i32> {
    let cf_key = cf_string_new(key);
    let value = unsafe { CFDictionaryGetValue(dict, cf_key) };
    unsafe { CFRelease(cf_key) };
    if value.is_null() {
        return None;
    }
    let mut num: i32 = 0;
    let ok = unsafe { CFNumberGetValue(value, 3, &mut num as *mut i32 as *mut c_void) };
    if ok { Some(num) } else { None }
}

fn cf_dict_get_u32(dict: *const c_void, key: &str) -> Option<u32> {
    let cf_key = cf_string_new(key);
    let value = unsafe { CFDictionaryGetValue(dict, cf_key) };
    unsafe { CFRelease(cf_key) };
    if value.is_null() {
        return None;
    }
    let mut num: i32 = 0;
    let ok = unsafe { CFNumberGetValue(value, 3, &mut num as *mut i32 as *mut c_void) };
    if ok { Some(num as u32) } else { None }
}

pub fn get_app_icon_base64(pid: i32) -> String {
    let cls = objc2::class!(NSRunningApplication);
    let app: Option<Retained<AnyObject>> = unsafe {
        msg_send![cls, runningApplicationWithProcessIdentifier: pid]
    };
    let Some(app) = app else {
        return String::new();
    };

    let icon: Option<Retained<AnyObject>> = unsafe { msg_send![&app, icon] };
    let Some(icon) = icon else {
        return String::new();
    };

    let tiff_data: Option<Retained<AnyObject>> = unsafe { msg_send![&icon, TIFFRepresentation] };
    let Some(tiff_data) = tiff_data else {
        return String::new();
    };

    let bmp_cls = objc2::class!(NSBitmapImageRep);
    let rep: Option<Retained<AnyObject>> =
        unsafe { msg_send![bmp_cls, imageRepWithData: &*tiff_data] };
    let Some(rep) = rep else {
        return String::new();
    };

    let png_data: Option<Retained<AnyObject>> = unsafe {
        msg_send![&rep, representationUsingType: 4u64, properties: std::ptr::null::<c_void>()]
    };
    let Some(png_data) = png_data else {
        return String::new();
    };

    let len: usize = unsafe { msg_send![&png_data, length] };
    let bytes: *const u8 = unsafe { msg_send![&png_data, bytes] };
    let buf = unsafe { std::slice::from_raw_parts(bytes, len) };

    base64::engine::general_purpose::STANDARD.encode(buf)
}

pub fn collect_windows(mru: &MruMap) -> Vec<WindowInfo> {
    let array = unsafe { CGWindowListCopyWindowInfo(K_C_G_WINDOW_LIST_OPTION_ON_SCREEN_ONLY, 0) };
    if array.is_null() {
        return vec![];
    }

    let self_pid = std::process::id() as i32;
    let mut windows: Vec<WindowInfo> = Vec::new();
    let count = unsafe { CFArrayGetCount(array) };

    for i in 0..count {
        let dict = unsafe { CFArrayGetValueAtIndex(array, i) };
        if dict.is_null() {
            continue;
        }

        let layer = cf_dict_get_i32(dict, "kCGWindowLayer").unwrap_or(999);
        if layer != 0 {
            continue;
        }

        let owner_pid = cf_dict_get_i32(dict, "kCGWindowOwnerPID").unwrap_or(-1);
        if owner_pid <= 0 || owner_pid == self_pid {
            continue;
        }

        let owner_name = cf_dict_get_string(dict, "kCGWindowOwnerName").unwrap_or_default();
        if owner_name.is_empty() || owner_name == "Dock" {
            continue;
        }

        let window_title = cf_dict_get_string(dict, "kCGWindowName").unwrap_or_default();
        let window_id = cf_dict_get_u32(dict, "kCGWindowNumber").unwrap_or(0);

        windows.push(WindowInfo {
            pid: owner_pid,
            window_id,
            app_name: owner_name,
            window_title,
            icon_base64: String::new(),
            is_active: false,
        });
    }

    unsafe { CFRelease(array) };

    windows.sort_by(|a, b| {
        let ta = mru
            .get(&a.pid)
            .map(|t| t.elapsed())
            .unwrap_or(std::time::Duration::from_secs(999));
        let tb = mru
            .get(&b.pid)
            .map(|t| t.elapsed())
            .unwrap_or(std::time::Duration::from_secs(999));
        ta.cmp(&tb)
    });

    if let Some(first) = windows.first_mut() {
        first.is_active = true;
    }

    for w in &mut windows {
        w.icon_base64 = get_app_icon_base64(w.pid);
    }

    windows
}

pub fn start_mru_poller(mru: std::sync::Arc<Mutex<MruMap>>) {
    std::thread::spawn(move || {
        let cls = objc2::class!(NSWorkspace);
        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let workspace: Option<Retained<AnyObject>> = unsafe { msg_send![cls, sharedWorkspace] };
            let Some(workspace) = workspace else {
                continue;
            };
            let front_app: Option<Retained<AnyObject>> =
                unsafe { msg_send![&workspace, frontmostApplication] };
            let Some(front_app) = front_app else {
                continue;
            };
            let pid: i32 = unsafe { msg_send![&front_app, processIdentifier] };
            if pid > 0 {
                mru.lock().insert(pid, Instant::now());
            }
        }
    });
}
