mod autostart;
mod clipboard;
mod clipboard_highlight;
mod config;
mod event_monitor;
mod event_tap;
mod ffi;
mod i18n;
mod logger;
mod menu;
mod mouse;
mod overlay;
mod settings;
mod theme;
mod thumbnail;
mod window_collector;

use config::CONFIG;
use i18n::t;
// FFI 基础工具(make_nsstring/release_obj/CFRelease/ObjPtr/颜色图层 helper 等)集中在 ffi.rs
// FFI primitives (make_nsstring/release_obj/CFRelease/ObjPtr/color+layer helpers) live in ffi.rs
use ffi::*;
// 主题与布局(Colors/配色/卡片窗口尺寸访问器/STATUS_H/H_PADDING)集中在 theme.rs
// Theme and layout (Colors/colors/card+window size accessors/STATUS_H/H_PADDING) live in theme.rs
use theme::*;
// 切换器浮窗与卡片 UI(浮窗状态/卡片索引/回调/渲染/激活)集中在 overlay.rs
// Switcher overlay & card UI (overlay state/card index/callbacks/rendering/activation) live in overlay.rs
use overlay::*;
// 状态栏菜单(菜单项状态/动作回调/标题刷新)集中在 menu.rs
// Status bar menu (menu-item state/action callbacks/title refresh) live in menu.rs
use menu::*;
// 设置窗口(控件构造/窗口构建显示收集/校验告警/配置热应用)集中在 settings.rs
// Settings window (control builders/window build-show-collect/validation alerts/hot config apply) live in settings.rs
use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use settings::*;
use std::ffi::{c_void, CString};
use std::sync::Mutex;
use std::thread;

use event_monitor::{start as start_event_monitor, GlobalEvent};
use window_collector::{
    bump_window_mru, cache_running_app_icons, cache_running_app_icons_small, ensure_icon_cache_dir,
    extract_icon_to_cache, focused_window_cgwid, migrate_legacy_cache, note_app_activated, MruMap,
    WindowInfo,
};

// FFI 声明与 ObjC 桥接基础工具已移至 `ffi.rs` / FFI declarations and ObjC bridging primitives moved to `ffi.rs`

// 布局常量 STATUS_H / H_PADDING 已移至 `theme.rs` / layout constants moved to `theme.rs`

// ========== Types ==========

// 跨模块共享的应用状态(overlay/menu/settings 均会访问,故 pub(crate))。
// AppState 与 TAB_STATE 留在 main.rs,避免 overlay↔menu 互相依赖形成环。
// Cross-module shared app state (pub(crate) so overlay/menu/settings can access).
// AppState + TAB_STATE stay in main.rs to avoid an overlay<->menu dependency cycle.
pub(crate) struct AppState {
    pub(crate) windows: Vec<WindowInfo>,
    pub(crate) selected: usize,
    pub(crate) visible: bool,
    pub(crate) mru: MruMap,
}

impl AppState {
    pub(crate) fn new() -> Self {
        // 启动时用系统窗口前→后顺序预种 MRU:重启后初始顺序与原生 Cmd+Tab 的
        // 应用级顺序一致(见 window_collector::seed_mru_from_system_order)。
        // Seed the MRU from the system's front-to-back window order at startup, so the
        // initial ordering after a restart matches the native app-level Cmd+Tab order.
        let mut mru = window_collector::seed_mru_from_system_order();
        let windows = if has_accessibility_permission() {
            window_collector::collect_windows(&mut mru)
        } else {
            Vec::new()
        };
        if !has_accessibility_permission() {
            log_info!("No accessibility permission.");
            log_info!("Go to System Settings → Privacy & Security → Accessibility");
        }
        let win_count = windows.len();
        AppState {
            windows,
            selected: if win_count > 1 { 1 } else { 0 },
            visible: false,
            mru,
        }
    }

    pub(crate) fn refresh(&mut self) {
        self.windows = window_collector::collect_windows(&mut self.mru);
        if !self.windows.is_empty() && self.selected >= self.windows.len() {
            self.selected = self.windows.len() - 1;
        }
        if self.windows.is_empty() {
            self.visible = false;
        }
    }
}

// Colors 结构已移至 `theme.rs` / moved to `theme.rs`

// ObjPtr / ObjClassPtr 已移至 `ffi.rs` / moved to `ffi.rs`

// ========== Global State ==========

pub(crate) static TAB_STATE: Mutex<Option<AppState>> = Mutex::new(None);
pub(crate) static CONTROLLER: Mutex<Option<ObjPtr>> = Mutex::new(None);

/// 菜单项与设置按钮共用的 ObjC target 对象（OhMyTabMenuTarget2 实例）。
/// Shared ObjC target object for menu items and settings buttons.
pub(crate) static MENU_TARGET: Mutex<Option<ObjPtr>> = Mutex::new(None);
pub(crate) static STATUS_EVENT_TX: std::sync::OnceLock<flume::Sender<GlobalEvent>> =
    std::sync::OnceLock::new();

// ========== Helper Functions ==========
// make_nsstring / release_obj / has_accessibility_permission / hex_to_*color / layer_set_* 已移至 `ffi.rs`
// make_nsstring / release_obj / has_accessibility_permission / hex_to_*color / layer_set_* moved to `ffi.rs`

// colors_from_config / system_dark_mode / current_colors / card_* / icon_px / letter_px /
// window_height / window_width 已移至 `theme.rs`
// colors_from_config / system_dark_mode / current_colors / card_* / icon_px / letter_px /
// window_height / window_width moved to `theme.rs`

// ========== ObjC Method Implementations ==========

// --- Controller ---

extern "C" fn on_app_activated(_self: *mut c_void, _cmd: Sel, notification: *mut c_void) {
    unsafe {
        let user_info: *mut AnyObject = msg_send![notification as *mut AnyObject, userInfo];
        if user_info.is_null() {
            return;
        }
        let key = make_nsstring("NSWorkspaceApplicationKey");
        let app: *mut AnyObject = msg_send![user_info, objectForKey: key];
        CFRelease(key as *const c_void);
        if app.is_null() {
            return;
        }
        // 非常规策略 = 后台/辅助进程(如嵌套 helper):无窗口、图标多为通用占位图,
        // 提取会以相同 bundle id 污染主应用的图标缓存;缩略图观察者也不需要。
        // Non-regular policy = background/helper processes: no windows, icons are
        // usually the generic placeholder, and extracting would poison the main
        // app's shared cache key; the thumbnail observer doesn't need them either.
        let policy: i64 = msg_send![app, activationPolicy];
        if policy != 0 {
            return;
        }
        let pid: i32 = msg_send![app, processIdentifier];
        // 更新 App 级激活时间（仅用于诊断，不参与排序）
        // Update app-level activation time (diagnostics only, not used in sorting)
        note_app_activated(pid);
        // 后台线程解析焦点窗口的 CGWindowID 并 bump 窗口级 MRU。
        // 系统 Cmd+Tab / Dock 点击等外部焦点切换通过此路径反馈到窗口排序中。
        // kAXFocusedWindow 的 AX 查询可能阻塞最高 50ms（目标 App 无响应时），
        // 必须放到后台线程避免卡住主线程 UI。
        // Resolve the focused window's CGWindowID off-main and bump window MRU.
        // External focus switches (system Cmd+Tab, Dock clicks) feed into window
        // ordering through this path. The kAXFocusedWindow AX query can block up
        // to 50ms (target app unresponsive), so it must run off the main thread.
        thread::spawn(move || {
            // 日志用于诊断 MRU 是否被正确 bump:成功打印 pid+cgwid;失败(AX 查询超时/无聚焦窗口)
            // 也打印,便于排查"所有窗口 mru 都停在 ancient 回退值"这类问题。
            // Log to diagnose MRU bumping: print pid+cgwid on success; also log on failure
            // (AX timeout / no focused window) to investigate "all windows stuck at ancient fallback".
            if let Some(cgwid) = focused_window_cgwid(pid) {
                let mut state_opt = TAB_STATE.lock().unwrap();
                if let Some(ref mut state) = *state_opt {
                    bump_window_mru(&mut state.mru, pid, cgwid);
                    log_debug!("app-activated bump: pid={} cgwid={}", pid, cgwid);
                }
            } else {
                log_debug!(
                    "app-activated bump: pid={} (no focused window / AX timeout)",
                    pid
                );
            }
        });
    }
}

extern "C" fn on_app_launched(_self: *mut c_void, _cmd: Sel, notification: *mut c_void) {
    // Pre-cache the launched app's icon so it's on disk before the user summons
    // the switcher. Run off the main thread (with an autorelease pool) so the
    // launch notification doesn't block the UI; extract_icon_to_cache is
    // defensive (null-safe) and a failure simply leaves the letter-icon fallback.
    let pid: i32 = unsafe {
        let user_info: *mut AnyObject = msg_send![notification as *mut AnyObject, userInfo];
        if user_info.is_null() {
            return;
        }
        let key = make_nsstring("NSWorkspaceApplicationKey");
        let app: *mut AnyObject = msg_send![user_info, objectForKey: key];
        CFRelease(key as *const c_void);
        if app.is_null() {
            return;
        }
        msg_send![app, processIdentifier]
    };
    if pid <= 0 {
        return;
    }
    thread::spawn(move || unsafe {
        let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
        match extract_icon_to_cache(pid) {
            Some(_) => log_debug!("app-launch icon cached: pid={}", pid),
            None => {
                // 刚启动瞬间 AppKit 的 icon 可能尚未就绪(app.icon 返回 nil),导致
                // 提取失败且无缓存留下——之后浮窗显示字母占位。延迟 ~1s 重试一次。
                // The icon may not be ready the instant the app launches (app.icon nil),
                // silently failing the extract and leaving the letter placeholder in the
                // switcher. Retry once after ~1s.
                log_debug!(
                    "app-launch icon extract failed for pid={}, retrying in 1s",
                    pid
                );
                std::thread::sleep(std::time::Duration::from_secs(1));
                let _ = extract_icon_to_cache(pid);
            }
        }
        let _: () = msg_send![pool, drain];
    });
    // 缩略图:给新启动的 App 安装 AXObserver 并预生成其既有窗口。
    // Thumbnails: install the new app's AXObserver and pre-generate its windows.
    thumbnail::app_launched(pid);
}

/// NSWorkspaceDidTerminateApplicationNotification 转发点(main 线程):
/// 通知缩略图模块卸载该 App 的 observer。
/// Forwarding point for NSWorkspaceDidTerminateApplicationNotification (main
/// thread): tells the thumbnail module to uninstall that app's observer.
extern "C" fn on_app_terminated(_self: *mut c_void, _cmd: Sel, notification: *mut c_void) {
    let pid: i32 = unsafe {
        let user_info: *mut AnyObject = msg_send![notification as *mut AnyObject, userInfo];
        if user_info.is_null() {
            return;
        }
        let key = make_nsstring("NSWorkspaceApplicationKey");
        let app: *mut AnyObject = msg_send![user_info, objectForKey: key];
        CFRelease(key as *const c_void);
        if app.is_null() {
            return;
        }
        msg_send![app, processIdentifier]
    };
    if pid > 0 {
        thumbnail::app_terminated(pid);
    }
}

extern "C" fn on_locale_changed(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    unsafe {
        // 通知投递线程不保证是主线程,而刷新 UI 必须在主线程;非主线程时转到主线程重入本方法。
        // Notification delivery thread isn't guaranteed to be main, but UI refresh must run on
        // main; when off main, hop to main and re-enter this method.
        let is_main: bool = msg_send![class!(NSThread), isMainThread];
        if !is_main {
            let _: () = msg_send![_self as *mut AnyObject,
                performSelectorOnMainThread: sel!(handleLocaleChanged:),
                withObject: std::ptr::null::<AnyObject>(),
                waitUntilDone: false
            ];
            return;
        }
    }
    // 系统语言变更(NSLocaleCurrentLocaleDidChangeNotification)。
    // 仅当 locale 为 auto(系统派生)时 apply_config_locale 会真正改变解析结果;显式 locale 下短路。
    // 重新解析后刷新菜单标题、作废设置窗口待下次按新 locale 重建。
    // System language changed (NSLocaleCurrentLocaleDidChangeNotification).
    // apply_config_locale only re-resolves when locale is auto (system-derived); it short-circuits
    // for an explicit locale. After re-resolving, refresh menu titles and invalidate the settings
    // window so it rebuilds with the new locale on next open.
    let locale_cfg = CONFIG.read().unwrap().i18n.locale.clone();
    i18n::apply_config_locale(&locale_cfg);
    refresh_menu_titles();
    invalidate_settings_window();
    clipboard::refresh_localized_ui();
}

/// 鼠标插拔回调(在鼠标线程执行)经 performSelectorOnMainThread 转到主线程后的重入点:
/// 设置窗口开着时即时刷新设备下拉框(重连后立即显示,无需点确定/重开)。
/// Re-entry point after the mouse-thread plug/unplug callback hops to the main thread via
/// performSelectorOnMainThread: refresh the settings device popup live (a reconnect shows
/// immediately, no OK/reopen needed).
extern "C" fn on_devices_changed(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    unsafe {
        let is_main: bool = msg_send![class!(NSThread), isMainThread];
        if !is_main {
            let _: () = msg_send![_self as *mut AnyObject,
                performSelectorOnMainThread: sel!(handleDevicesChanged:),
                withObject: std::ptr::null::<AnyObject>(),
                waitUntilDone: false
            ];
            return;
        }
    }
    crate::settings::refresh_device_popup_if_open();
}

/// 缩略图捕获完成(worker 线程经 performSelectorOnMainThread 跳来):清空待投递
/// 队列并原位重建受影响卡片。
/// Thumbnail capture finished (hopped from the worker thread via
/// performSelectorOnMainThread): drains the pending queue and rebuilds the
/// affected cards in place.
extern "C" fn on_thumbnail_ready(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    thumbnail::handle_ready_main();
}

// ========== Class Registration ==========

fn register_classes() {
    unsafe {
        // --- OhMyTabCardView : NSView ---
        let card_cls = {
            let name = CString::new("OhMyTabCardView").unwrap();
            let superclass = class!(NSView) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types_v_obj = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(mouseDown:),
                card_mouse_down as *mut c_void,
                types_v_obj.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseEntered:),
                card_mouse_entered as *mut c_void,
                types_v_obj.as_ptr(),
            );
            objc_registerClassPair(cls);
            cls
        };
        *CARD_CLASS.lock().unwrap() =
            Some(ObjClassPtr(card_cls as *const objc2::runtime::AnyClass));
    }
}

fn create_overlay_window() -> *mut AnyObject {
    unsafe {
        let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
        let screen_frame: NSRect = msg_send![screen, frame];
        let h = window_height(6); // initial reasonable default
        let w = window_width(cards_per_row()); // max possible width
        let x = (screen_frame.size.width - w) / 2.0 + screen_frame.origin.x;
        let y = (screen_frame.size.height - h) / 2.0 + screen_frame.origin.y;
        let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));

        // Borderless 窗口(styleMask = 0):无标题栏 -> 窗口可见形状 = 玻璃的圆角 alpha,
        // 消除"圆角玻璃 + 方角窗口"在四角留下的透明突出,且四角对称。
        // borderless 默认不能成为 key 窗口(收不到键盘),所以用自定义 NSPanel 子类
        // 重写 canBecomeKeyWindow -> YES。原先用 titled + 透明标题栏绕开此子类,代价就是
        // 四角不对称的透明突出(Regular 下尤为明显)。
        //
        // 关键:NSPanel + NSWindowStyleMaskNonactivatingPanel(1<<7) —— 面板成为 key 窗口时
        // **不激活所属 app**(BetterCmdTab 面板同款)。召唤时 app 保持非激活,设置窗口就不会
        // 被抬到活动 App 前面,切换器因此不再需要 stash/orderBack 机制。
        //
        // Borderless window (styleMask = 0): no title bar -> the window's visible shape equals
        // the glass's rounded alpha, eliminating the transparent protrusions left at the corners by
        // a rounded glass inside a square window, and keeping all four corners symmetric. A borderless
        // window can't become key by default (no keyboard input), so a custom NSPanel subclass
        // overrides canBecomeKeyWindow -> YES. The earlier titled + transparent-titlebar approach
        // avoided this subclass at the cost of those asymmetric transparent corners (especially
        // visible under the Regular glass style).
        //
        // Key: NSPanel + NSWindowStyleMaskNonactivatingPanel (1<<7) -- the panel becomes key
        // WITHOUT activating the owning app (same as BetterCmdTab's panel). While the app stays
        // inactive during summon, the settings window is never raised above the active app, so
        // the switcher no longer needs the stash/orderBack machinery.
        let style: u64 = 1 << 7; // NSWindowStyleMaskBorderless(0) | NSWindowStyleMaskNonactivatingPanel

        // 注册自定义窗口子类 OhMyTabOverlayWindow : NSPanel(仅重写 canBecomeKeyWindow)。
        // 仿 OhMyTabContainerView 的 inline 注册;create_overlay_window 只调用一次,无重复注册风险。
        // Register the custom window subclass OhMyTabOverlayWindow : NSPanel (only overrides
        // canBecomeKeyWindow). Inline, like OhMyTabContainerView; create_overlay_window is called
        // once, so no double-registration.
        let window_cls = {
            let name = CString::new("OhMyTabOverlayWindow").unwrap();
            let superclass = class!(NSPanel) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types_bool = CString::new("B@:").unwrap();
            class_addMethod(
                cls,
                sel!(canBecomeKeyWindow),
                overlay_window_can_become_key as *mut c_void,
                types_bool.as_ptr(),
            );
            objc_registerClassPair(cls);
            cls
        };

        let window: *mut AnyObject = msg_send![window_cls, alloc];
        let window: *mut AnyObject = msg_send![window, initWithContentRect: frame, styleMask: style, backing: 2u64, defer: false];

        // NSFloatingWindowLevel = 3 (should be above normal windows during app switch)
        let _: () = msg_send![window, setLevel: 3u64];

        // ========== Window transparency / Liquid Glass settings ==========
        //
        // (1) Window must be non-opaque so the compositor allows content
        //     behind the window to show through.
        let _: () = msg_send![window, setOpaque: false];
        //
        // (2) Window background must be clear, otherwise NSThemeFrame draws
        //     a solid color that blocks everything behind it.
        let clear_color: *mut AnyObject = msg_send![class!(NSColor), clearColor];
        let _: () = msg_send![window, setBackgroundColor: clear_color];
        //
        // (3) 关闭窗口阴影:NSGlassEffectView 自带 Liquid Glass 深度,窗口阴影是多余的。
        //     其强度随内容 alpha 变化(Regular 玻璃不透明 -> 强阴影圈;Clear 近透明 -> 无),
        //     会在 Regular 下沿玻璃边缘形成一圈多余暗环;且投影向下偏移,底角与顶角不一致
        //     (这正是 borderless 后"边上还有一圈、底角形状不同"的来源)。关掉后只留玻璃自身
        //     深度,边缘干净、四角一致。
        // (3) Disable the window shadow: NSGlassEffectView already provides Liquid Glass depth, so
        //     the window shadow is redundant. Its strength scales with content alpha (Regular's
        //     opaque glass -> a strong shadow ring; Clear's near-transparent glass -> none), which
        //     shows up under Regular as an unwanted dark ring along the glass edge; the drop shadow
        //     is also offset downward, so the bottom corners differ from the top (this is the "ring
        //     still on the edge, bottom corners shaped differently" seen after going borderless).
        //     With it off, only the glass's own depth remains -- clean edges, symmetric corners.
        let _: () = msg_send![window, setHasShadow: false];
        // =================================================================

        let _: () = msg_send![window, setReleasedWhenClosed: false];
        // Don't let the window hide on deactivate (we manage show/hide)
        let _: () = msg_send![window, setHidesOnDeactivate: false];

        // --- Liquid Glass ---
        // macOS 26+  → NSGlassEffectView  (new public API, built-in blur)
        // macOS <26 → NSVisualEffectView  (withinWindow + Dark material)
        let is_macos_26 = AnyClass::get(c"NSGlassEffectView").is_some();

        // The view that will contain the card container.
        // On macOS 26 this is the glass view's inner contentView;
        // on older macOS it's the NSVisualEffectView itself.
        let content_parent: *mut AnyObject;

        if is_macos_26 {
            let glass_cls = AnyClass::get(c"NSGlassEffectView").unwrap();
            let glass: *mut AnyObject = msg_send![glass_cls, alloc];
            let glass: *mut AnyObject = msg_send![glass, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h))];
            *GLASS_VIEW.lock().unwrap() = Some(ObjPtr(glass)); // 保存指针，供热重载重新应用 / save for hot reload
                                                               // (4) Corner radius — native NSGlassEffectView property, from config.
            let _: () =
                msg_send![glass, setCornerRadius: CONFIG.read().unwrap().appearance.corner_radius];
            // (5) Glass style — "regular" (0) or "clear" (1), from config.
            let style: i64 = match CONFIG.read().unwrap().appearance.glass_style.as_str() {
                "clear" => 1,
                _ => 0, // regular (default)
            };
            let _: () = msg_send![glass, setStyle: style];
            // (6) Tint color — hex RRGGBBAA from config.
            let tint_hex = config::parse_hex8(&CONFIG.read().unwrap().appearance.glass_tint);
            let tint = hex_to_ns_color(tint_hex);
            let _: () = msg_send![glass, setTintColor: tint];
            // (7) Autoresizing so the glass view fills the window on resize.
            let _: () = msg_send![glass, setAutoresizingMask: 18u64];
            let _: () = msg_send![window, setContentView: glass];
            // NSGlassEffectView.contentView may be nil initially - create our own.
            let inner: *mut AnyObject = msg_send![class!(NSView), alloc];
            let inner: *mut AnyObject = msg_send![inner, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h))];
            let _: () = msg_send![inner, setAutoresizingMask: 18u64];
            let _: () = msg_send![glass, setContentView: inner];
            // (6.5) 硬裁剪背景模糊:NSGlassEffectView 的 cornerRadius 属性只圆了着色/外观,背景模糊
            //       仍填满方角。给 layer 设 masksToBounds + cornerRadius 把模糊也裁进圆角(对
            //       NSVisualEffectView 是公认有效的做法,NSGlassEffectView 待验证)。
            //       放在 setContentView 之后 + 显式 setWantsLayer,确保 layer 已落实(非 nil),
            //       masksToBounds 真正生效;并打日志确认。
            // (6.5) Hard-clip the backdrop blur: NSGlassEffectView's cornerRadius property only rounds
            //       the tint/appearance, not the backdrop blur. Setting masksToBounds + cornerRadius on
            //       the layer clips the blur into the round (the standard trick for NSVisualEffectView;
            //       unverified for NSGlassEffectView). Done after setContentView + an explicit
            //       setWantsLayer so the layer is realized (non-nil) and masksToBounds actually takes
            //       effect; logged to confirm.
            let radius = CONFIG.read().unwrap().appearance.corner_radius;
            let _: () = msg_send![glass, setWantsLayer: true];
            let glass_layer: *mut AnyObject = msg_send![glass, layer];
            if !glass_layer.is_null() {
                let _: () = msg_send![glass_layer, setCornerRadius: radius];
                let _: () = msg_send![glass_layer, setMasksToBounds: true];
            }
            content_parent = inner;
        } else {
            let content: *mut AnyObject = msg_send![window, contentView];
            let ve: *mut AnyObject = msg_send![class!(NSVisualEffectView), alloc];
            let ve: *mut AnyObject = msg_send![ve, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h))];
            // withinWindow blending + Dark material (same as the GPUI version used)
            let _: () = msg_send![ve, setBlendingMode: 1u64]; // WithinWindow
            let _: () = msg_send![ve, setMaterial: 12u64]; // Dark
            let _: () = msg_send![ve, setState: 1u64]; // Active
            let _: () = msg_send![ve, setAutoresizingMask: 18u64];
            let _: () = msg_send![content, addSubview: ve];
            content_parent = ve;
        }

        // --- Container view for cards ---
        // Register OhMyTabContainerView : NSView
        let container_cls = {
            let name = CString::new("OhMyTabContainerView").unwrap();
            let superclass = class!(NSView) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types_v_obj = CString::new("v@:@").unwrap();
            let types_bool = CString::new("B@:").unwrap();
            class_addMethod(
                cls,
                sel!(keyDown:),
                container_key_down as *mut c_void,
                types_v_obj.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(acceptsFirstResponder),
                container_accepts_first_responder as *mut c_void,
                types_bool.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseMoved:),
                container_mouse_moved as *mut c_void,
                types_v_obj.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseDragged:),
                container_mouse_moved as *mut c_void,
                types_v_obj.as_ptr(),
            );
            objc_registerClassPair(cls);
            cls
        };

        let container: *mut AnyObject = msg_send![container_cls, alloc];
        let container: *mut AnyObject = msg_send![container, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h))];
        let _: () = msg_send![container, setAutoresizingMask: 18u64];
        let _: () = msg_send![content_parent, addSubview: container];
        *CONTAINER.lock().unwrap() = Some(ObjPtr(container));

        // --- Status label at bottom (standard coords: y=0 is bottom) ---
        let status_font: *mut AnyObject = {
            let cfg = CONFIG.read().unwrap();
            msg_send![class!(NSFont), systemFontOfSize: cfg.fonts.status_bar_size, weight: cfg.fonts.status_bar_weight]
        };
        let status_color = hex_to_ns_color(0x999999ff);
        let status_label = make_centered_label("", status_font, status_color, 0.0, w, STATUS_H);
        let _: () = msg_send![container, addSubview: status_label];
        *STATUS_LABEL.lock().unwrap() = Some(ObjPtr(status_label));

        window
    }
}

fn create_controller() -> *mut AnyObject {
    unsafe {
        let name = CString::new("OhMyTabController").unwrap();
        let superclass = class!(NSObject) as *const _ as *mut AnyObject;
        let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
        let types_v_obj = CString::new("v@:@").unwrap();
        class_addMethod(
            cls,
            sel!(handleCmdTabPressed:),
            on_cmd_tab_pressed as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleCmdReleased:),
            on_cmd_released as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(closeCard:),
            on_close_card as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(onClipboardToggled:),
            clipboard::on_clipboard_toggle as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleAppActivation:),
            on_app_activated as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleAppLaunch:),
            on_app_launched as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleLocaleChanged:),
            on_locale_changed as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleDelayedOrderOut:),
            on_delayed_order_out as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleDevicesChanged:),
            on_devices_changed as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(thumbnailReady:),
            on_thumbnail_ready as *mut c_void,
            types_v_obj.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(handleAppTerminate:),
            on_app_terminated as *mut c_void,
            types_v_obj.as_ptr(),
        );
        objc_registerClassPair(cls);
        msg_send![cls, new]
    }
}

fn init_app() {
    unsafe {
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        // NSApplicationActivationPolicyAccessory = 1
        let _: bool = msg_send![nsapp, setActivationPolicy: 1isize];
    }
}

/// 切换应用激活策略。设置窗口打开时切 .regular(进 Dock / 系统 Cmd+Tab / 调度中心图标),
/// 关闭时切回 .accessory(纯菜单栏)。LSUIElement 默认 .accessory,设置窗口需要 .regular
/// 才能正常激活抬升(打开设置时从别的 App 顶部弹出来)。
/// Switch the app activation policy: .regular while the settings window is open (so it can
/// activate normally and raise itself above the active app when opened), .accessory when
/// closed (pure menu-bar). LSUIElement defaults to .accessory.
pub(crate) fn set_settings_activation_policy(regular: bool) {
    unsafe {
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        // NSApplicationActivationPolicyRegular = 0, .Accessory = 1
        let policy: isize = if regular { 0 } else { 1 };
        let _: bool = msg_send![nsapp, setActivationPolicy: policy];
    }
}

fn setup_status_bar() {
    unsafe {
        let status_bar: *mut AnyObject = msg_send![class!(NSStatusBar), systemStatusBar];
        // NSVariableStatusItemLength = -1.0:槽位按 button 内容自适应,sizeToFit 后贴合图标,
        // 不留多余边距。固定长度(曾经的 30.0)不会被 sizeToFit 缩小,槽位恒为 30pt,而图标只有
        // ~17pt,居中/靠左后两侧留空,看着和邻居图标之间有很大间距。
        // NSVariableStatusItemLength = -1.0: the slot auto-sizes to the button's content, so after
        // sizeToFit it hugs the icon with no extra padding. A fixed length (the former 30.0) is not
        // shrunk by sizeToFit, so the slot stays 30pt while the icon is ~17pt, leaving visible gaps
        // around the icon.
        let status_item: *mut AnyObject = msg_send![status_bar, statusItemWithLength: -1.0f64];
        let _: *mut AnyObject = msg_send![status_item, retain];

        let button: *mut AnyObject = msg_send![status_item, button];

        // Status bar icon: 单色 template PNG(两个矩形叠放,assets/statusbar-icon.png 嵌入二进制)。
        // setTemplate:YES 让系统按菜单栏前景色渲染(浅/深色 menu bar 都清晰);sizeToFit 让 button 贴合 image。
        // Status bar icon: monochrome template PNG (two overlapped rects, assets/statusbar-icon.png embedded
        // in the binary). setTemplate:YES makes the system render it in the menu bar foreground color
        // (clear on both light/dark menu bars); sizeToFit hugs the image.
        let png_bytes: &[u8] = include_bytes!("../assets/statusbar-icon.png");
        let nsdata: *mut AnyObject = msg_send![
            class!(NSData),
            dataWithBytes: png_bytes.as_ptr() as *const c_void,
            length: png_bytes.len()
        ];
        let image: *mut AnyObject = msg_send![class!(NSImage), alloc];
        let image: *mut AnyObject = msg_send![image, initWithData: nsdata];
        if !image.is_null() {
            // statusbar-icon.png 是 162x128(横向留白更宽,让图标与邻居间距更舒展)。
            // 设高度为 status bar 厚度、宽度按 PNG 纵横比等比缩放,保持图形像素大小不变、
            // 只放大左右空隙。强制方形(icon_size x icon_size)会压扁非方形 PNG。
            // statusbar-icon.png is 162x128 (wider horizontal margins so the icon sits looser
            // from its neighbors). Set the height to the status-bar thickness and scale the width
            // by the PNG aspect ratio, keeping the glyph pixel size while widening the gaps.
            // Forcing a square (icon_size x icon_size) would squash the non-square PNG.
            let icon_size: f64 = msg_send![status_bar, thickness];
            let aspect: f64 = 162.0 / 128.0; // statusbar-icon.png 纵横比 / PNG aspect ratio
            let _: () = msg_send![image, setSize: NSSize::new(icon_size * aspect, icon_size)];
            let is_template: bool = true;
            let _: () = msg_send![image, setTemplate: is_template];
            let _: () = msg_send![button, setImage: image];
            // NSImageOnly = 1
            let _: () = msg_send![button, setImagePosition: 1usize];
            let _: () = msg_send![image, release]; // button retained it; drop our alloc +1
        } else {
            let ns_title = make_nsstring("Tab");
            let _: () = msg_send![button, setTitle: ns_title];
            CFRelease(ns_title as *const c_void);
        }

        let _: () = msg_send![button, sizeToFit];
        let _: () = msg_send![button, setNeedsDisplay: true];

        // Build menu
        let menu_title = make_nsstring("");
        let menu: *mut AnyObject = msg_send![class!(NSMenu), alloc];
        let menu: *mut AnyObject = msg_send![menu, initWithTitle: menu_title];
        CFRelease(menu_title as *const c_void);

        // Menu action target class
        let action_cls = {
            let name = CString::new("OhMyTabMenuTarget2").unwrap();
            let superclass: *const objc2::runtime::AnyClass = class!(NSObject);
            let cls = objc_allocateClassPair(superclass as *mut AnyObject, name.as_ptr(), 0);
            if cls.is_null() {
                log_info!("Failed to allocate ObjC class for menu target.");
                return;
            }
            let types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(handleQuit:),
                handle_quit as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleToggleShortcut:),
                handle_toggle_shortcut as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleReloadConfig:),
                handle_reload_config as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleClearIconCache:),
                handle_clear_icon_cache as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleSettings:),
                on_settings_open as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleSettingsOk:),
                on_settings_ok as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleSettingsCancel:),
                on_settings_cancel as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleSettingsSidebar:),
                on_sidebar_select as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleRestoreDefaults:),
                handle_restore_defaults as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleOpenPrivacy:),
                handle_open_privacy as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleEnableMouseToggle:),
                handle_enable_mouse_toggle as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleDeviceChanged:),
                handle_device_changed as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleScrollModeChanged:),
                handle_scroll_mode_changed as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleLineCountChanged:),
                handle_line_count_changed as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleAddMapping:),
                handle_add_mapping as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleMappingEnabledChanged:),
                handle_mapping_enabled_changed as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleMappingEdit:),
                handle_mapping_edit as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleDeleteMapping:),
                handle_delete_mapping as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handlePanelRecordTrigger:),
                handle_panel_record_trigger as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handlePanelRecordCombo:),
                handle_panel_record_combo as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handlePanelActionChanged:),
                handle_panel_action_changed as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleMappingConfirm:),
                handle_mapping_confirm as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleMappingCancel:),
                handle_mapping_cancel as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleRecordingFinished:),
                handle_recording_finished as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleRecordingCancelled:),
                handle_recording_cancelled as *mut c_void,
                types.as_ptr(),
            );
            objc_registerClassPair(cls);
            cls
        };
        let menu_target: *mut AnyObject = msg_send![action_cls as *const AnyObject, new];
        *MENU_TARGET.lock().unwrap() = Some(ObjPtr(menu_target));

        // Shortcut toggle item
        let shortcut_title = make_nsstring(&t("menu.toggle_shortcut.cmd"));
        let shortcut_key = make_nsstring("");
        let shortcut_item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
        let shortcut_item: *mut AnyObject = msg_send![shortcut_item, initWithTitle: shortcut_title, action: sel!(handleToggleShortcut:), keyEquivalent: shortcut_key];
        CFRelease(shortcut_title as *const c_void);
        CFRelease(shortcut_key as *const c_void);
        let _: () = msg_send![shortcut_item, setTarget: menu_target];
        let _: () = msg_send![menu, addItem: shortcut_item];
        *SHORTCUT_ITEM.lock().unwrap() = Some(ShortcutState {
            item: shortcut_item,
        });

        // 设置... item (opens the settings window)
        let settings_title = make_nsstring(&t("menu.settings"));
        let settings_key = make_nsstring("");
        let settings_item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
        let settings_item: *mut AnyObject = msg_send![settings_item, initWithTitle: settings_title, action: sel!(handleSettings:), keyEquivalent: settings_key];
        CFRelease(settings_title as *const c_void);
        CFRelease(settings_key as *const c_void);
        let _: () = msg_send![settings_item, setTarget: menu_target];
        let _: () = msg_send![menu, addItem: settings_item];

        // Reload Config item
        let reload_title = make_nsstring(&t("menu.reload_config"));
        let reload_key = make_nsstring("");
        let reload_item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
        let reload_item: *mut AnyObject = msg_send![reload_item, initWithTitle: reload_title, action: sel!(handleReloadConfig:), keyEquivalent: reload_key];
        CFRelease(reload_title as *const c_void);
        CFRelease(reload_key as *const c_void);
        let _: () = msg_send![reload_item, setTarget: menu_target];
        let _: () = msg_send![menu, addItem: reload_item];

        // Clear Icon Cache item
        let clear_cache_title = make_nsstring(&t("menu.clear_icon_cache"));
        let clear_cache_key = make_nsstring("");
        let clear_cache_item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
        let clear_cache_item: *mut AnyObject = msg_send![clear_cache_item, initWithTitle: clear_cache_title, action: sel!(handleClearIconCache:), keyEquivalent: clear_cache_key];
        CFRelease(clear_cache_title as *const c_void);
        CFRelease(clear_cache_key as *const c_void);
        let _: () = msg_send![clear_cache_item, setTarget: menu_target];
        let _: () = msg_send![menu, addItem: clear_cache_item];

        // Separator
        let sep_item: *mut AnyObject = msg_send![class!(NSMenuItem), separatorItem];
        let _: () = msg_send![menu, addItem: sep_item];

        // Quit item
        let quit_title = make_nsstring(&t("menu.quit"));
        // 绑定 Cmd+Q:仅在本 app 为前台(如设置窗口打开)时生效——菜单栏常驻 app
        // 在后台时 Cmd+Q 由当前前台 app 处理,这是 macOS 语义。
        // Bind Cmd+Q: only effective while this app is frontmost (e.g. settings window open) --
        // when it's a background menu-bar app, Cmd+Q goes to the frontmost app per macOS semantics.
        let quit_key = make_nsstring("q");
        let quit_item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
        let quit_item: *mut AnyObject = msg_send![quit_item, initWithTitle: quit_title, action: sel!(handleQuit:), keyEquivalent: quit_key];
        CFRelease(quit_title as *const c_void);
        CFRelease(quit_key as *const c_void);
        let _: () = msg_send![quit_item, setTarget: menu_target];
        let _: () = msg_send![menu, addItem: quit_item];

        // 登记固定标题项,供热重载 locale 时批量重设标题 / register fixed-title items for locale hot-reload
        *FIXED_MENU_ITEMS.lock().unwrap() = Some(FixedMenuItems {
            settings: settings_item,
            reload: reload_item,
            clear_cache: clear_cache_item,
            quit: quit_item,
        });

        let _: () = msg_send![status_item, setMenu: menu];

        // 同时设为 mainMenu:让 AppKit 的 Cmd+Q keyEquivalent 分发能找到 Quit 项
        // (accessory app 的 mainMenu 不会显示在菜单栏最左侧——那里只显示 regular app
        // 的菜单,但快捷键路由仍生效,同 LinearMouse 的 storyboard mainMenu 机制)。
        // Also set as mainMenu so AppKit's Cmd+Q keyEquivalent dispatch can find the Quit item.
        // An accessory app's mainMenu is NOT shown in the menu bar's app area (that only shows
        // regular apps' menus), but key-equivalent routing still works -- same mechanism as
        // LinearMouse's storyboard mainMenu.
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![nsapp, setMainMenu: menu];

        // Pump run loop to let SystemUIServer connect
        for _ in 0..10 {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.001, 1u8);
        }
    }
}

// ========== Main ==========

/// 打开「系统设置 -> 隐私与安全性 -> 辅助功能」面板(深链)。
/// 供启动告警框与设置里的警告条按钮共用。
/// Open System Settings -> Privacy & Security -> Accessibility (deep link).
/// Shared by the startup alert and the settings warning banner's button.
pub(crate) fn open_privacy_accessibility() {
    unsafe {
        let url_str = make_nsstring(
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
        );
        let url: *mut AnyObject = msg_send![class!(NSURL), URLWithString: url_str];
        CFRelease(url_str as *const c_void);
        if !url.is_null() {
            let ws: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
            let _: bool = msg_send![ws, openURL: url];
        }
    }
}

/// 启动时若缺 Accessibility 权限,弹自定义告警框引导用户去授权。
/// 事件监听线程已在后台有限次重试,用户授权后 tap 会自动建成,无需重启。
/// Prompts the user with a custom alert at launch if Accessibility permission is missing.
/// The event-monitor thread is already retrying in the background; once the user grants
/// permission the tap is created automatically - no restart needed.
fn prompt_accessibility_if_needed() {
    if has_accessibility_permission() {
        return;
    }
    // AppState::new 已记过 "No accessibility permission." 日志,这里只负责弹框,不重复记。
    // AppState::new already logged "No accessibility permission."; this only shows the alert.
    // 辅助应用无 Dock 图标,主动激活以免告警框被其它窗口遮挡。
    // Accessory apps have no Dock icon; activate so the alert isn't hidden behind other windows.
    unsafe {
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![nsapp, activateIgnoringOtherApps: true];
    }
    let open = confirm_alert(
        &t("alert.accessibility_title"),
        &t("alert.accessibility_msg"),
        &t("alert.accessibility_open"),
        &t("alert.accessibility_later"),
    );
    if open {
        open_privacy_accessibility();
    }
}

fn main() {
    // 冒烟测试入口(--smoke-clipboard):在真实主线程 + NSApplication 环境里两次显示
    // 剪贴板浮窗(覆盖 rebuild_rows 行清理路径),成功 exit(0)。由 clipboard 模块的
    // #[ignore] 测试以子进程方式调用——测试 harness 的工作线程会被 AppKit 主线程限制拦下。
    // Smoke-test entry (--smoke-clipboard): show the clipboard picker twice on the real main
    // thread inside a real NSApplication (exercising rebuild_rows' row cleanup), exit(0) on
    // success. Invoked as a subprocess by the #[ignore] test in clipboard.rs -- the test
    // harness's worker threads trip AppKit's main-thread guard.
    if std::env::args().any(|a| a == "--smoke-clipboard") {
        unsafe {
            let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
            init_app();
            // 冒烟模式:历史/缓存隔离到专用目录,绝不能写入用户的真实数据。
            // Smoke mode: isolate the history/cache into a dedicated directory so the
            // injected entries can never touch the user's real data.
            clipboard::set_smoke_mode();
            drop(CONFIG.read().unwrap()); // 触发 CONFIG 初始化,与正常启动一致
            let ok = clipboard::smoke_runner();
            let _: () = msg_send![pool, drain];
            if !ok {
                eprintln!("[smoke-clipboard] picker smoke failed");
                std::process::exit(1);
            }
            std::process::exit(0);
        }
    }

    // 1. Init NSApplication as accessory (no dock icon)
    init_app();

    // 1b. 初始化 logger:早于一切,从 CONFIG 读日志级别,根据 cargo run / .app 决定输出目标。
    //     Init logger: before everything else, read log level from CONFIG, auto-detect dev/prod.
    {
        let cfg = CONFIG.read().unwrap(); // 触发 LazyLock 初始化和 config 加载 / triggers LazyLock init + config load
        let level = match cfg.logging.level.as_str() {
            "debug" => logger::LogLevel::Debug,
            _ => logger::LogLevel::Info,
        };
        let is_dev = std::env::current_exe()
            .map(|p| p.to_string_lossy().contains("target/"))
            .unwrap_or(false);
        logger::init(
            &logger::LogConfig {
                level,
                file_path: cfg.logging.file_path.clone(),
            },
            is_dev,
        );
    }

    // 2. Register custom ObjC classes
    register_classes();

    // 2b. 强制 CONFIG 初始化(顺带应用 i18n locale),保证菜单按配置 locale 构建。
    //     CONFIG 的 LazyLock 初始化会调用 i18n::apply_config_locale,无循环依赖。
    // Force CONFIG init (also applies i18n locale) so the menu is built with the configured
    // locale. CONFIG's LazyLock init calls i18n::apply_config_locale; no cycle (see i18n.rs).
    drop(CONFIG.read().unwrap());

    // 3. Setup status bar menu
    setup_status_bar();

    // 3a. 按配置初始化快捷键模式:SHORTCUT_IS_CMD 默认 false(Option),启动时必须从 config 读回,
    //     否则设置里改的 modifier 重启后会丢失。放在 refresh_menu_titles 之前,让菜单标题刷新时
    //     读到的已是正确值。
    // Initialize shortcut mode from config: SHORTCUT_IS_CMD defaults to false (Option) and must be
    // read back at startup, otherwise the modifier chosen in Settings is lost on restart. Runs
    // before refresh_menu_titles so the label refresh sees the correct value.
    set_shortcut_mode(CONFIG.read().unwrap().keyboard.modifier == "command");

    // 同步开机自启(SMAppService):TOML [startup] launch_at_login 为唯一事实源。
    // 仅以 .app 方式启动时生效(cargo run 裸二进制下 mainApp 不可用,会记 warn,不影响其它功能)。
    // Sync launch-at-login (SMAppService): TOML's [startup] launch_at_login is the source of truth.
    // Only effective when launched as a .app (raw `cargo run` has no main bundle -> logs a warn, no other impact).
    autostart::sync(CONFIG.read().unwrap().startup.launch_at_login);

    // 3b. 按实际主题修正初始菜单标签。setup_status_bar 用占位标题(is_dark=false +
    //     "切换深色");若 config 主题为 dark/auto,这里修正为正确的 toggle 标签。
    //     locale 已在 2b 应用,菜单文本本身已正确,此处只修正 toggle 方向。
    // Fix initial menu labels against the real theme. setup_status_bar used a placeholder
    // (is_dark=false + "switch to dark"); if the config theme is dark/auto this corrects the
    // toggle direction. Locale was applied in 2b so text is already correct; this only fixes
    // the toggle direction.
    refresh_menu_titles();

    // 4. Initialize state
    ensure_icon_cache_dir();
    // 一次性清理旧版按 PID 命名的缓存文件(纯数字 stem 的 .png),它们对新版无用、只会占地方。
    // One-shot cleanup of legacy PID-named cache files (purely-numeric-stem .png); useless to
    // the new version and just take up space.
    migrate_legacy_cache();
    cache_running_app_icons(); // pre-warm icon cache for all running apps
                               // 剪贴板标题栏小图标预热:仅当剪贴板功能开启时才生成,否则跳过(避免无谓的提取)。
                               // Pre-warm the clipboard header's small icons: only when the clipboard feature is enabled,
                               // otherwise skip (no point extracting for a disabled feature).
    if CONFIG.read().unwrap().clipboard.enabled {
        cache_running_app_icons_small();
    }

    // 4b. Force CONFIG to initialise and report any validation errors
    {
        let cfg = CONFIG.read().unwrap();
        // First load already happened via LazyLock; re-run validate to report problems
        let errs = cfg.validate();
        if !errs.is_empty() {
            log_info!(
                "Config errors in ~/.config/oh-my-tab/config.toml ({} issue(s)):",
                errs.len()
            );
            for e in &errs {
                log_info!("  • {}", e);
            }
            log_info!("Using defaults for invalid fields.");
        }
    }

    *TAB_STATE.lock().unwrap() = Some(AppState::new());

    // 5. Create overlay window (hidden initially)
    let window = create_overlay_window();
    *OVERLAY_WINDOW.lock().unwrap() = Some(ObjPtr(window));
    // Hide initially
    hide_overlay();
    // 点击浮窗外部 → 取消切换(面板失去 key 时收起,同 Esc 语义)。
    // A click outside the overlay cancels the switch (dismissed when the panel loses key).
    install_click_to_cancel();

    // 6. Create controller object
    let controller = create_controller();
    *CONTROLLER.lock().unwrap() = Some(ObjPtr(controller));

    // 6b. Listen for system app activation so MRU stays in sync
    // when the user switches apps via Dock, Cmd+Tab, etc.
    unsafe {
        let ws: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let nc: *mut AnyObject = msg_send![ws, notificationCenter];
        let name = make_nsstring("NSWorkspaceDidActivateApplicationNotification");
        let _: () = msg_send![nc,
            addObserver: controller,
            selector: sel!(handleAppActivation:),
            name: name,
            object: std::ptr::null::<AnyObject>(),
        ];
        CFRelease(name as *const c_void);

        // Pre-cache icons for apps launched after startup so they're ready
        // before the user summons the switcher (fixes missing icons for apps
        // opened while oh-my-tab is already running).
        let launch_name = make_nsstring("NSWorkspaceDidLaunchApplicationNotification");
        let _: () = msg_send![nc,
            addObserver: controller,
            selector: sel!(handleAppLaunch:),
            name: launch_name,
            object: std::ptr::null::<AnyObject>(),
        ];
        CFRelease(launch_name as *const c_void);

        // App 退出通知:缩略图模块据此卸载该 App 的 AXObserver(缓存由 LRU 自然淘汰)。
        // App-terminate notice: the thumbnail module uninstalls that app's observer
        // (cached entries age out via the LRU).
        let term_name = make_nsstring("NSWorkspaceDidTerminateApplicationNotification");
        let _: () = msg_send![nc,
            addObserver: controller,
            selector: sel!(handleAppTerminate:),
            name: term_name,
            object: std::ptr::null::<AnyObject>(),
        ];
        CFRelease(term_name as *const c_void);

        // 监听系统语言变更,locale 为 auto 时实时跟随。NSLocaleCurrentLocaleDidChangeNotification
        // 投递在默认通知中心(不是 workspace 中心),故用 NSNotificationCenter defaultCenter。
        // Listen for system language changes to live-follow when locale is auto.
        // NSLocaleCurrentLocaleDidChangeNotification is posted to the default notification center
        // (not the workspace center), so use NSNotificationCenter defaultCenter.
        let default_nc: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
        let locale_name = make_nsstring("NSLocaleCurrentLocaleDidChangeNotification");
        let _: () = msg_send![default_nc,
            addObserver: controller,
            selector: sel!(handleLocaleChanged:),
            name: locale_name,
            object: std::ptr::null::<AnyObject>(),
        ];
        CFRelease(locale_name as *const c_void);
    }

    // 7. Start event monitor + bridge thread
    let (event_tx, event_rx) = flume::unbounded();
    let _monitor = start_event_monitor(event_tx.clone());
    STATUS_EVENT_TX.set(event_tx).ok();

    // 7b. Start the mouse event tap only if enabled in config.
    // 鼠标事件 tap:仅在配置启用时启动(start 幂等,日志在 mouse::start 内)。
    // Mouse event tap: start only if enabled (start is idempotent; logging lives inside).
    let mouse_enabled = CONFIG.read().map(|c| c.mouse.enabled).unwrap_or(false);
    if mouse_enabled {
        mouse::start();
    }

    // 7b2. hover 轮询定时器在浮窗显示/隐藏时由 overlay 自行启停(show_overlay 调用
    // start_hover_timer),无需在此启动:主线程 runloop 每 16ms 读全局鼠标位置命中
    // 卡片,不依赖事件投递(侧键按住期间移动事件无法通过任何 tap/tracking 获取,实测)。
    // The hover poll timer is started/stopped by the overlay itself (start_hover_timer
    // from show_overlay): the main-thread runloop reads the global cursor position every
    // 16ms and hit-tests the cards, independent of event delivery (moves while a side
    // button is held can't be obtained via any tap/tracking, verified).

    // 7c. Apply pointer settings (disable system acceleration if configured).
    // 指针设置(配置了禁用系统加速时立即生效)。
    mouse::pointer::apply();

    // 7d. Start the clipboard-history poller only if enabled in config.
    // 剪贴板历史轮询:仅在配置启用时启动(幂等)。
    // Clipboard-history polling: start only if enabled (idempotent).
    let clip_enabled = CONFIG.read().map(|c| c.clipboard.enabled).unwrap_or(false);
    if clip_enabled {
        clipboard::start();
    }

    // 7e. 窗口缩略图服务:常驻 AXObserver 监听新窗口 + 内存 LRU 缓存。无屏幕录制
    // 权限时 worker 每任务前 preflight 自动休眠,浮窗保持纯图标渲染。
    // Window thumbnails: resident AXObserver listener + memory LRU. Without the
    // Screen Recording permission the worker sleeps per-job (preflighted) and the
    // overlay keeps icon-only rendering.
    let thumbs_enabled = CONFIG
        .read()
        .map(|c| c.layout.thumbnails_enabled)
        .unwrap_or(false);
    if thumbs_enabled {
        thumbnail::start();
    }

    // Bridge thread: flume events → main thread via performSelectorOnMainThread
    thread::spawn(move || {
        while let Ok(event) = event_rx.recv() {
            let action = match event {
                GlobalEvent::CmdTabPressed => sel!(handleCmdTabPressed:),
                GlobalEvent::CmdReleased => sel!(handleCmdReleased:),
                GlobalEvent::ClipboardToggled => sel!(onClipboardToggled:),
            };
            // Read controller pointer from static (only written once, safe to read)
            let ctrl = CONTROLLER.lock().unwrap().unwrap().0;
            unsafe {
                let _: () = msg_send![ctrl,
                    performSelectorOnMainThread: action,
                    withObject: std::ptr::null::<AnyObject>(),
                    waitUntilDone: false
                ];
            }
        }
        log_info!("Bridge thread exiting.");
    });

    // 冒烟测试入口(--smoke-overlay):完整初始化后直接驱动召唤路径，再遍历并循环
    // 一次窗口列表，覆盖超量布局的连续滑动/回绕；随后泵 2 秒主 runloop 让异步
    // 缩略图投递(thumbnailReady)落地，无崩溃 exit(0)。用于无头验证 Cmd+Tab
    // 链路(合成按键到不了 CGEventTap，无法从外部触发)。
    // Smoke-test entry (--smoke-overlay): after full init, drive a summon directly,
    // then traverse and wrap the window list once to cover overflow sliding. Pump
    // the main runloop for 2s so async thumbnail deliveries (thumbnailReady) land,
    // and exit(0) on survival. Synthetic keystrokes never reach the CGEventTap, so
    // this path must be driven internally.
    if std::env::args().any(|a| a == "--smoke-overlay") {
        unsafe {
            let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![nsapp, finishLaunching];
            log_info!("[smoke-overlay] summoning");
            on_cmd_tab_pressed(
                std::ptr::null_mut(),
                sel!(handleCmdTabPressed:),
                std::ptr::null_mut(),
            );
            let window_count = TAB_STATE
                .lock()
                .unwrap()
                .as_ref()
                .map_or(0, |state| state.windows.len());
            for _ in 0..window_count.saturating_add(1) {
                on_cmd_tab_pressed(
                    std::ptr::null_mut(),
                    sel!(handleCmdTabPressed:),
                    std::ptr::null_mut(),
                );
            }
            let rl: *mut AnyObject = msg_send![class!(NSRunLoop), currentRunLoop];
            let date: *mut AnyObject =
                msg_send![class!(NSDate), dateWithTimeIntervalSinceNow: 2.0f64];
            let _: () = msg_send![rl, runUntilDate: date];
            log_info!("[smoke-overlay] summon survived");
            std::process::exit(0);
        }
    }

    // 8. Run the main event loop (blocks until [NSApp terminate:])
    unsafe {
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![nsapp, finishLaunching];
        // 启动后若缺 Accessibility 权限,弹告警框引导授权(事件监听线程已在后台有限次重试)。
        // Prompt for Accessibility if missing (the event-monitor thread is already retrying in the background).
        prompt_accessibility_if_needed();
        let _: () = msg_send![nsapp, run];
    }
}
