use std::collections::HashMap;
use std::ffi::c_void;
use std::time::Instant;

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

pub fn collect_windows(mru: &mut MruMap) -> Vec<WindowInfo> {
    let array = unsafe { CGWindowListCopyWindowInfo(K_C_G_WINDOW_LIST_OPTION_ON_SCREEN_ONLY, 0) };
    if array.is_null() {
        return vec![];
    }

    let self_pid = std::process::id() as i32;
    let mut windows: Vec<WindowInfo> = Vec::new();
    let count = unsafe { CFArrayGetCount(array) };
    let now = Instant::now();

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

        mru.entry(owner_pid).or_insert(now);

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

    windows
}
