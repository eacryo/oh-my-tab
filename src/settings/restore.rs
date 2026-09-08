//! Settings restore-confirmation and reset actions.
//!
//! The restore flow is kept separate from window construction and control binding so the
//! settings module exposes only the callbacks and state transitions needed by the app.
//!
//! 设置恢复确认与重置动作。
//!
//! 将恢复流程与窗口构建、控件绑定分离，设置模块只暴露应用所需的回调和状态迁移接口。

use super::*;

/// 确认弹窗(两个按钮)。返回 true = 用户点了确认按钮。
/// Confirm dialog (two buttons). Returns true if the user clicked the confirm button.
pub(crate) fn confirm_alert(
    title: &str,
    msg: &str,
    confirm_label: &str,
    cancel_label: &str,
) -> bool {
    unsafe {
        let alert: *mut AnyObject = msg_send![class!(NSAlert), new];
        let ns1 = make_nsstring(title);
        let _: () = msg_send![alert, setMessageText: ns1];
        CFRelease(ns1 as *const c_void);
        let ns2 = make_nsstring(msg);
        let _: () = msg_send![alert, setInformativeText: ns2];
        CFRelease(ns2 as *const c_void);
        // 第一个按钮为默认(右,回车);确认在前,取消在后。
        // First button is the default (rightmost, Return); confirm first, cancel second.
        let n_confirm = make_nsstring(confirm_label);
        let _: *mut AnyObject = msg_send![alert, addButtonWithTitle: n_confirm];
        CFRelease(n_confirm as *const c_void);
        let n_cancel = make_nsstring(cancel_label);
        let _: *mut AnyObject = msg_send![alert, addButtonWithTitle: n_cancel];
        CFRelease(n_cancel as *const c_void);
        let resp: isize = msg_send![alert, runModal];
        release_obj(alert);
        resp == 1000 // NSAlertFirstButtonReturn = 确认 / confirm
    }
}

/// 展开「恢复默认设置」(左下角,整应用)的内联确认卡片;同时收起本页确认卡片,
/// 保证同一时间只有一个恢复确认卡片。
/// Expand the "Restore Defaults" (bottom-left, whole app) inline confirmation card; collapse
/// the per-page card first so only one restore confirmation is open at a time.
pub(crate) extern "C" fn handle_restore_defaults(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    set_page_restore_confirmation_expanded(false, true);
    set_restore_confirmation_expanded(true, true);
}

/// 收起「恢复默认设置」的内联确认卡片,不修改任何设置值。
/// Collapse the whole-app restore confirmation without changing any values.
pub(crate) extern "C" fn handle_restore_defaults_cancel(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    set_restore_confirmation_expanded(false, true);
}

/// 确认恢复默认设置(整应用):一次性写入默认配置、刷新 UI/运行时/持久化。
/// Confirm whole-app restore: write the default config in one shot and refresh the UI,
/// runtime, and persisted state together.
pub(crate) extern "C" fn handle_restore_defaults_confirm(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    // 收起不播动画:确认后窗口可能因主题/locale 默认值变化而重建,动画没有意义。
    // Collapse without animation: the window may be rebuilt right after (theme/locale
    // defaults), so animating is pointless.
    set_restore_confirmation_expanded(false, false);
    restore_all_defaults();
    show_restore_success(false);
}

/// 展开「恢复本页默认设置」(右下角,当前 Tab)的内联确认卡片;同时收起整应用确认卡片,
/// 保证同一时间只有一个恢复确认卡片。
/// Expand the "Restore Page Defaults" (bottom-right, current tab) inline confirmation card;
/// collapse the whole-app card first so only one restore confirmation is open at a time.
pub(crate) extern "C" fn handle_page_restore_defaults(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    set_restore_confirmation_expanded(false, true);
    set_page_restore_confirmation_expanded(true, true);
}

/// 收起「恢复本页默认设置」的内联确认卡片,不修改任何设置值。
/// Collapse the per-page restore confirmation without changing any values.
pub(crate) extern "C" fn handle_page_restore_defaults_cancel(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    set_page_restore_confirmation_expanded(false, true);
}

/// 确认恢复本页默认设置:一次性恢复当前 Sidebar Tab 对应的全部设置并立即生效。
/// Confirm per-page restore: reset every setting of the current sidebar tab in one shot and
/// apply immediately.
pub(crate) extern "C" fn handle_page_restore_defaults_confirm(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    set_page_restore_confirmation_expanded(false, false);
    restore_tab_defaults(SIDEBAR_SELECTED.load(Ordering::SeqCst));
    show_restore_success(true);
}

/// Show the post-reset success feedback after configuration and UI refreshes have completed.
/// 在配置与设置界面刷新完成后显示恢复默认成功提示。
fn show_restore_success(page_only: bool) {
    let text = if page_only {
        t("settings.toast_page_defaults_restored")
    } else {
        t("settings.toast_all_defaults_restored")
    };
    let window =
        with_settings_ui(|ui| ui.as_ref().map(|ui| ui.window)).unwrap_or(std::ptr::null_mut());
    unsafe {
        tooltip::SettingsTooltip::show_success_bubble(window, &text);
    }
}

/// 收起两套恢复确认卡片(切页/开关设置窗口时调用)。
/// Collapse both restore confirmation cards (page switch / window hide).
pub(crate) fn collapse_restore_confirmations(animated: bool) {
    set_restore_confirmation_expanded(false, animated);
    set_page_restore_confirmation_expanded(false, animated);
}

/// 点击所有恢复确认组件之外的区域时收起确认卡片,恢复原始恢复按钮。
/// Collapse the confirmation card when a click lands outside every restore control, returning
/// each control to its original restore button.
pub(crate) fn collapse_restore_confirmations_on_external_click(
    window: *mut AnyObject,
    event: *mut AnyObject,
) {
    if window.is_null() || event.is_null() {
        return;
    }
    unsafe {
        let location: NSPoint = msg_send![event, locationInWindow];
        let inside_restore = with_settings_ui(|ui| {
            let Some(ui) = ui.as_ref() else { return false };
            let any_expanded = ui.restore_defaults.expanded
                || ui.page_restores.iter().any(|control| control.expanded);
            if !any_expanded {
                return true;
            }
            let content: *mut AnyObject = msg_send![window, contentView];
            if content.is_null() {
                return true;
            }
            let point: NSPoint = msg_send![
                content,
                convertPoint: location,
                fromView: std::ptr::null::<AnyObject>()
            ];
            let hit_view: *mut AnyObject = msg_send![content, hitTest: point];
            ui.restore_defaults.contains_hit_view(hit_view)
                || ui
                    .page_restores
                    .iter()
                    .any(|control| control.contains_hit_view(hit_view))
        });
        if !inside_restore {
            collapse_restore_confirmations(true);
        }
    }
}

/// 将整应用恢复确认区域切换到展开或收起状态。底部按钮保持锚定,确认按钮向上展开。
/// Toggle the whole-app restore confirmation area. The bottom action stays anchored while
/// confirmation grows upward into the available space.
fn set_restore_confirmation_expanded(expanded: bool, animated: bool) {
    with_settings_ui(|ui| {
        if let Some(ui) = ui.as_mut() {
            unsafe {
                ui.restore_defaults.set_expanded(expanded, animated);
            }
        }
    });
}

/// 将全部「恢复本页默认设置」确认卡片切换到展开或收起状态。
/// 只有点中页的卡片可能处于展开态,遍历全部实例保证无残留。
/// Toggle every per-page restore confirmation card. Only the selected page's card can be
/// expanded; iterating all instances guarantees no stale state.
fn set_page_restore_confirmation_expanded(expanded: bool, animated: bool) {
    with_settings_ui(|ui| {
        if let Some(ui) = ui.as_mut() {
            unsafe {
                for control in ui.page_restores.iter_mut() {
                    control.set_expanded(expanded, animated);
                }
            }
        }
    });
}

/// 一次性恢复整个应用的全部默认设置并立即生效(保留 launch_at_login)。
/// Restore the whole app's defaults in one atomic update (launch_at_login preserved).
fn restore_all_defaults() {
    let old = CONFIG.read().unwrap().clone();
    // 保留 launch_at_login -- 它是系统级登录项开关,不属于外观/布局/快捷键这类设置,
    // 不该被恢复默认重置(否则会注销用户已勾选的登录项)。
    // Preserve launch_at_login -- it's a system-level login-item toggle, not an
    // appearance/layout/shortcut setting, so Restore Defaults must not reset it.
    let preserved_launch_at_login = old.startup.launch_at_login;
    let mut defaults = Config::default();
    defaults.startup.launch_at_login = preserved_launch_at_login;
    if let Ok(mut w) = CONFIG.write() {
        *w = defaults.clone();
    }
    log_config_changes(&old, &CONFIG.read().unwrap());
    persist_config_now();
    log_info!("[settings] all settings restored to defaults");

    // 鼠标页设备选择复位(有设备则回退第一个;无设备则清空)。
    // Reset the mouse-page device selection (first device if any; clear if none).
    *SELECTED_DEVICE.lock().unwrap() = None;
    // 一次性应用全部 Tab 的运行时副作用,再原位重建设置窗口(主题/locale 可能变化)。
    // Apply every tab's runtime side effects in one pass, then rebuild the settings window
    // in place (theme/locale may have changed).
    apply_config_change(&old, &defaults, ConfigChangeSource::RestoreDefaults);
}

/// 一次性恢复当前 Tab 的全部默认设置并立即生效(原子更新,不影响其他 Tab)。
/// Restore every default of the current tab in one atomic update (other tabs untouched).
fn restore_tab_defaults(tab: usize) {
    let old = CONFIG.read().unwrap().clone();
    let mut cfg = old.clone();
    let d = Config::default();
    // 每个 Tab 的配置范围:按页面归属划分,严格限定在本 Tab,不触碰其他 Tab 的字段。
    // Per-tab config scope: partitioned by page ownership, strictly limited to this tab.
    match tab {
        0 => {
            // 通用页:外观 + 语言 + 日志。launch_at_login 是系统级登录项开关,保留。
            // General: appearance + locale + logging. launch_at_login (system-level) is kept.
            cfg.appearance = d.appearance;
            cfg.i18n = d.i18n;
            cfg.logging = d.logging;
        }
        1 => {
            // 应用切换浮窗页:窗口/布局/字体/快捷键 + 位于本页的圆角。
            // Switcher page: windows/layout/fonts/keyboard + corner radius (lives on this page).
            cfg.windows = d.windows;
            cfg.layout = d.layout;
            cfg.fonts = d.fonts;
            cfg.keyboard = d.keyboard;
            cfg.appearance.corner_radius = d.appearance.corner_radius;
        }
        2 => {
            // 鼠标页:总开关 + 选中设备档的全部字段(无专属档则创建显式默认档,
            // 使本页显示与生效值回到默认,同时不影响其他设备)。
            // Mouse page: master switch + every field of the selected device's profile (an
            // explicit default profile is created when absent, so this page's visible and
            // effective values return to defaults without touching other devices).
            cfg.mouse.enabled = d.mouse.enabled;
            let dev = current_selected_device();
            let idx = find_profile_index(&cfg, dev);
            let idx = match idx {
                Some(i) => i,
                None => {
                    let new_p = MouseProfile {
                        device: match dev {
                            Some((vid, pid)) => DeviceMatcher {
                                vendor_id: Some(vid),
                                product_id: Some(pid),
                            },
                            None => DeviceMatcher::default(),
                        },
                        ..Default::default()
                    };
                    cfg.mouse.profiles.push(new_p);
                    cfg.mouse.profiles.len() - 1
                }
            };
            // 默认配置含一个「所有鼠标」档,字段即默认生效值。
            // The default config has one "All Mice" profile whose fields are the default
            // effective values.
            if let Some(dp) = d.mouse.profiles.first().cloned() {
                let prof = &mut cfg.mouse.profiles[idx];
                prof.reverse_scroll = dp.reverse_scroll;
                prof.scroll_mode = dp.scroll_mode;
                prof.line_count = dp.line_count;
                prof.pointer = dp.pointer;
                prof.button_mappings = Default::default();
                prof.button_mappings_enabled = dp.button_mappings_enabled;
            }
        }
        3 => {
            cfg.clipboard = d.clipboard;
        }
        4 => {
            cfg.window_control = d.window_control;
        }
        5 => {
            cfg.quick_actions = d.quick_actions;
        }
        _ => {
            cfg.updates = d.updates;
        }
    }
    if let Ok(mut w) = CONFIG.write() {
        *w = cfg.clone();
    }
    log_config_changes(&old, &CONFIG.read().unwrap());
    persist_config_now();
    log_debug!("[settings] tab {} restored to defaults", tab);
    if tab == 0 {
        // 通用页涉及主题/locale 默认值:应用副作用后原位重建设置窗口。
        // The General page involves theme/locale defaults: apply effects, then rebuild the
        // settings window in place.
        apply_config_change(&old, &cfg, ConfigChangeSource::RestoreDefaults);
    } else {
        // 其余 Tab:重填控件(冻结态/条件显隐随 load_settings_from 刷新)+ 应用副作用。
        // Other tabs: refill the controls (freeze states / conditional visibility refresh
        // with load_settings_from) and apply the tab's side effects.
        load_settings_values();
        apply_config_change(&old, &cfg, ConfigChangeSource::RestoreDefaults);
    }
}
