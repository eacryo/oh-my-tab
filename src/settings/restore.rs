//! Settings restore-confirmation and reset actions.
//!
//! The restore flow is kept separate from window construction and control binding so the
//! settings module exposes only the callbacks and state transitions needed by the app.

use super::*;

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
        // First button is the default (rightmost, Return); confirm first, cancel second.
        let n_confirm = make_nsstring(confirm_label);
        let _: *mut AnyObject = msg_send![alert, addButtonWithTitle: n_confirm];
        CFRelease(n_confirm as *const c_void);
        let n_cancel = make_nsstring(cancel_label);
        let _: *mut AnyObject = msg_send![alert, addButtonWithTitle: n_cancel];
        CFRelease(n_cancel as *const c_void);
        let resp: isize = msg_send![alert, runModal];
        release_obj(alert);
        resp == 1000 // confirm
    }
}

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

/// Collapse the whole-app restore confirmation without changing any values.
pub(crate) extern "C" fn handle_restore_defaults_cancel(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    set_restore_confirmation_expanded(false, true);
}

/// Confirm whole-app restore: write the default config in one shot and refresh the UI,
/// runtime, and persisted state together.
pub(crate) extern "C" fn handle_restore_defaults_confirm(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    // Collapse without animation: the window may be rebuilt right after (theme/locale
    // defaults), so animating is pointless.
    set_restore_confirmation_expanded(false, false);
    restore_all_defaults();
    show_restore_success(false);
}

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

/// Collapse the per-page restore confirmation without changing any values.
pub(crate) extern "C" fn handle_page_restore_defaults_cancel(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    set_page_restore_confirmation_expanded(false, true);
}

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

/// Collapse both restore confirmation cards (page switch / window hide).
pub(crate) fn collapse_restore_confirmations(animated: bool) {
    set_restore_confirmation_expanded(false, animated);
    set_page_restore_confirmation_expanded(false, animated);
}

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

/// Restore the whole app's defaults in one atomic update (launch_at_login preserved).
fn restore_all_defaults() {
    let old = CONFIG.read().unwrap().clone();
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

    // Reset the mouse-page device selection (first device if any; clear if none).
    *SELECTED_DEVICE.lock().unwrap() = None;
    // Apply every tab's runtime side effects in one pass, then rebuild the settings window
    // in place (theme/locale may have changed).
    apply_config_change(&old, &defaults, ConfigChangeSource::RestoreDefaults);
}

/// Restore every default of the current tab in one atomic update (other tabs untouched).
fn restore_tab_defaults(tab: usize) {
    let old = CONFIG.read().unwrap().clone();
    let mut cfg = old.clone();
    let d = Config::default();
    // Per-tab config scope: partitioned by page ownership, strictly limited to this tab.
    match tab {
        0 => {
            // General: appearance + locale + logging. launch_at_login (system-level) is kept.
            cfg.appearance = d.appearance;
            cfg.i18n = d.i18n;
            cfg.logging = d.logging;
        }
        1 => {
            // Switcher page: windows/layout/fonts/keyboard + corner radius (lives on this page).
            cfg.windows = d.windows;
            cfg.layout = d.layout;
            cfg.fonts = d.fonts;
            cfg.keyboard = d.keyboard;
            cfg.appearance.corner_radius = d.appearance.corner_radius;
        }
        2 => {
            // Mouse page: master switch + every field of the selected device's profile (an
            // explicit default profile is created when absent, so this page's visible and
            // effective values return to defaults without touching other devices).
            cfg.mouse.enabled = d.mouse.enabled;
            let idx = super::selected_device_profile_index(&mut cfg);
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
        // The General page involves theme/locale defaults: apply effects, then rebuild the
        // settings window in place.
        apply_config_change(&old, &cfg, ConfigChangeSource::RestoreDefaults);
    } else {
        // Other tabs: refill the controls (freeze states / conditional visibility refresh
        // with load_settings_from) and apply the tab's side effects.
        load_settings_values();
        apply_config_change(&old, &cfg, ConfigChangeSource::RestoreDefaults);
    }
}
