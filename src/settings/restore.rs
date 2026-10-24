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
    crate::callback_guard::void("handle_restore_defaults", || {
        set_page_restore_confirmation_expanded(false, true);
        set_restore_confirmation_expanded(true, true);
    });
}

/// Collapse the whole-app restore confirmation without changing any values.
pub(crate) extern "C" fn handle_restore_defaults_cancel(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    crate::callback_guard::void("handle_restore_defaults_cancel", || {
        set_restore_confirmation_expanded(false, true);
    });
}

/// Confirm whole-app restore: write the default config in one shot and refresh the UI,
/// runtime, and persisted state together.
pub(crate) extern "C" fn handle_restore_defaults_confirm(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    crate::callback_guard::void("handle_restore_defaults_confirm", || {
        // Collapse without animation: the window may be rebuilt right after (theme/locale
        // defaults), so animating is pointless.
        set_restore_confirmation_expanded(false, false);
        restore_all_defaults();
        show_restore_success(false);
    });
}

/// Expand the "Restore Page Defaults" (bottom-right, current tab) inline confirmation card;
/// collapse the whole-app card first so only one restore confirmation is open at a time.
pub(crate) extern "C" fn handle_page_restore_defaults(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    crate::callback_guard::void("handle_page_restore_defaults", || {
        set_restore_confirmation_expanded(false, true);
        set_page_restore_confirmation_expanded(true, true);
    });
}

/// Collapse the per-page restore confirmation without changing any values.
pub(crate) extern "C" fn handle_page_restore_defaults_cancel(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    crate::callback_guard::void("handle_page_restore_defaults_cancel", || {
        set_page_restore_confirmation_expanded(false, true);
    });
}

/// Confirm per-page restore: reset every setting of the current sidebar tab in one shot and
/// apply immediately.
pub(crate) extern "C" fn handle_page_restore_defaults_confirm(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    crate::callback_guard::void("handle_page_restore_defaults_confirm", || {
        set_page_restore_confirmation_expanded(false, false);
        restore_page_defaults(settings_page_identity(
            SIDEBAR_SELECTED.load(Ordering::SeqCst),
        ));
        show_restore_success(true);
    });
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
    // Apply every tab's runtime side effects in one pass. When that needs a settings rebuild it does
    // it; otherwise refill every control in place, so a field the narrow syncs do not cover cannot
    // keep showing the pre-restore value.
    let rebuilt = crate::runtime_config::settings_window_rebuild_required(&old, &defaults);
    apply_config_change(&old, &defaults, ConfigChangeSource::RestoreDefaults);
    if !rebuilt {
        load_settings_values();
    }
}

/// Restore every default of one page in one atomic update (other pages untouched).
///
/// Keyed by the page's IDENTITY, not its sidebar number: the scope of a restore has to follow the
/// page the user is on when a page moves, or Clipboard's restore resets the Switcher.
pub(crate) fn restore_page_defaults(page: SettingsPageId) {
    let old = CONFIG.read().unwrap().clone();
    let cfg = restored_config_for_page(&old, page);
    if let Ok(mut w) = CONFIG.write() {
        *w = cfg.clone();
    }
    log_config_changes(&old, &CONFIG.read().unwrap());
    persist_config_now();
    log_debug!("[settings] page {page:?} restored to defaults");
    if page == SettingsPageId::General {
        // General owns appearance/locale/logging. Rebuild when those need it, otherwise refill the
        // controls in place: a material-only restore must not rebuild the whole window.
        let rebuilt = crate::runtime_config::settings_window_rebuild_required(&old, &cfg);
        apply_config_change(&old, &cfg, ConfigChangeSource::RestoreDefaults);
        if !rebuilt {
            load_settings_values();
        }
    } else {
        // Other tabs: refill the controls (freeze states / conditional visibility refresh
        // with load_settings_from) and apply the tab's side effects.
        load_settings_values();
        apply_config_change(&old, &cfg, ConfigChangeSource::RestoreDefaults);
    }
}

/// The config a single page's "Restore Defaults" writes: `old` with only the fields that page
/// owns replaced by their defaults. Kept pure so the page boundary (which settings a page may
/// touch) is asserted without a settings window; `restore_page_defaults` applies the result.
fn restored_config_for_page(old: &Config, page: SettingsPageId) -> Config {
    let mut cfg = old.clone();
    let d = Config::default();
    // Per-page config scope: partitioned by page ownership, strictly limited to this page.
    match page {
        SettingsPageId::General => {
            // General: appearance + locale + logging. launch_at_login (system-level) is kept;
            // hide_menu_bar_icon is a plain display preference, so it returns to its default
            // (visible) with the rest of the page.
            cfg.appearance = d.appearance;
            cfg.i18n = d.i18n;
            cfg.logging = d.logging;
            cfg.startup.hide_menu_bar_icon = d.startup.hide_menu_bar_icon;
        }
        SettingsPageId::Switcher => {
            // Switcher page: windows/layout/fonts/keyboard + corner radius (lives on this page).
            cfg.windows = d.windows;
            cfg.layout = d.layout;
            cfg.fonts = d.fonts;
            cfg.keyboard = d.keyboard;
            cfg.appearance.corner_radius = d.appearance.corner_radius;
        }
        SettingsPageId::Mouse => {
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
                prof.smooth_scrolling = dp.smooth_scrolling;
                prof.button_mappings = Default::default();
                prof.button_mappings_enabled = dp.button_mappings_enabled;
            }
        }
        SettingsPageId::Clipboard => {
            cfg.clipboard = d.clipboard;
        }
        SettingsPageId::KeyBindings => {
            // One page, one section: restoring it puts the factory bindings and both group
            // switches back, which is also what drops every binding the user added.
            cfg.key_bindings = d.key_bindings;
        }
        SettingsPageId::KeystrokeDisplay => {
            cfg.keystroke_display = d.keystroke_display;
        }
        SettingsPageId::FinderMenu => {
            cfg.finder_menu = d.finder_menu;
        }
        SettingsPageId::About => {
            cfg.updates = d.updates;
        }
    }
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A config where every page-owning group differs from its default, so a page restore that
    /// touches a field it does not own becomes visible.
    fn sentinel_config() -> Config {
        let mut cfg = Config::default();
        cfg.appearance.theme = "light".into();
        cfg.appearance.corner_radius = 7.0;
        cfg.i18n.locale = "en".into();
        cfg.logging.file_path = "/tmp/omt-sentinel.log".into();
        cfg.windows.show_minimized = true;
        cfg.layout.thumbnail_size = "120".into();
        cfg.fonts.title_size = 16.0;
        cfg.keyboard.modifier = "option".into();
        cfg.clipboard.enabled = true;
        cfg.clipboard.max_entries = 7;
        cfg.mouse.enabled = true;
        cfg.mouse.profiles[0].line_count = Some(9);
        cfg.key_bindings.enabled = true;
        cfg.keystroke_display.max_extent_percent = 80;
        cfg.updates.automatically_check = false;
        cfg
    }

    /// The whole `Config` a page restore must produce, written from the page-ownership promise
    /// rather than copied from the production match: `old` with only the fields the page owns
    /// replaced by their defaults. Comparing the whole value catches a restore that resets an
    /// extra field or forgets an owned one, which per-group spot checks miss.
    fn expected_after_restore(old: &Config, page: SettingsPageId) -> Config {
        let mut expected = old.clone();
        let d = Config::default();
        match page {
            SettingsPageId::General => {
                expected.appearance = d.appearance;
                expected.i18n = d.i18n;
                expected.logging = d.logging;
            }
            SettingsPageId::Switcher => {
                expected.windows = d.windows;
                expected.layout = d.layout;
                expected.fonts = d.fonts;
                expected.keyboard = d.keyboard;
                expected.appearance.corner_radius = d.appearance.corner_radius;
            }
            SettingsPageId::Mouse => {
                expected.mouse.enabled = d.mouse.enabled;
                // Tests never calibrate SELECTED_DEVICE, so the selected device is "All Mice"
                // and its profile is index 0.
                let dp = d
                    .mouse
                    .profiles
                    .first()
                    .cloned()
                    .expect("a default profile");
                let prof = &mut expected.mouse.profiles[0];
                prof.reverse_scroll = dp.reverse_scroll;
                prof.scroll_mode = dp.scroll_mode;
                prof.line_count = dp.line_count;
                prof.pointer = dp.pointer;
                prof.smooth_scrolling = dp.smooth_scrolling;
                prof.button_mappings = Default::default();
                prof.button_mappings_enabled = dp.button_mappings_enabled;
            }
            SettingsPageId::Clipboard => expected.clipboard = d.clipboard,
            SettingsPageId::KeyBindings => expected.key_bindings = d.key_bindings,
            SettingsPageId::KeystrokeDisplay => expected.keystroke_display = d.keystroke_display,
            SettingsPageId::FinderMenu => expected.finder_menu = d.finder_menu,
            SettingsPageId::About => expected.updates = d.updates,
        }
        expected
    }

    #[test]
    fn every_page_restore_changes_exactly_its_own_scope() {
        let pages = [
            SettingsPageId::General,
            SettingsPageId::Switcher,
            SettingsPageId::Mouse,
            SettingsPageId::Clipboard,
            SettingsPageId::KeyBindings,
            SettingsPageId::KeystrokeDisplay,
            SettingsPageId::About,
        ];
        for page in pages {
            let old = sentinel_config();
            let out = restored_config_for_page(&old, page);
            assert_eq!(
                out,
                expected_after_restore(&old, page),
                "restoring {page:?} must change exactly that page's fields"
            );
        }
    }
}
