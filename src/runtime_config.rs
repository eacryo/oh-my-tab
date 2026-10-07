//! Central runtime application for configuration changes.

use crate::config::Config;

/// Configuration change source, distinguishing initial startup from runtime deltas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigChangeSource {
    Startup,
    Settings,
    Reload,
    RestoreDefaults,
    Menu,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ChangeFlags {
    visual: bool,
    settings_appearance: bool,
    panel_material: bool,
    locale: bool,
    modifier: bool,
    startup: bool,
    logging: bool,
    windows_disabled: bool,
    thumbnails: bool,
    focused_thumbnail_prewarm: bool,
    show_app_name_in_cards: bool,
    // The pinned card size changed: the overlay picks it up on its next layout, and an open settings
    // window has to follow in place (`refresh_switcher_keystroke_and_mouse_controls_from_config`).
    thumbnail_size: bool,
    // The candidate scope itself changed: the cards already built no longer describe what the
    // switch now admits, so an open overlay must be dismissed rather than reused.
    show_other_desktops: bool,
    mouse: bool,
    clipboard_enabled: bool,
    clipboard_shortcut: bool,
    window_control: bool,
    quick_actions: bool,
    keystroke_display: bool,
    updates: bool,
}

/// Whether an already-open settings window has to re-read the config in place after this change.
///
/// Pure so the trigger can be asserted without a window: adding a field to `ChangeFlags` without
/// adding it here leaves the page showing the old value while the app uses the new one (the
/// thumbnail size hit exactly that).
fn needs_in_place_control_sync(flags: &ChangeFlags) -> bool {
    flags.modifier
        || flags.thumbnails
        || flags.focused_thumbnail_prewarm
        || flags.thumbnail_size
        || flags.keystroke_display
        || flags.mouse
}

fn change_flags(old: &Config, new: &Config, source: ConfigChangeSource) -> ChangeFlags {
    let startup = matches!(source, ConfigChangeSource::Startup);
    ChangeFlags {
        visual: old.appearance != new.appearance
            || old.layout.card_text_size != new.layout.card_text_size
            || old.layout.thumbnail_size != new.layout.thumbnail_size
            || old.colors != new.colors
            || old.fonts != new.fonts,
        settings_appearance: old.appearance != new.appearance || old.colors != new.colors,
        panel_material: old.appearance.panel_material != new.appearance.panel_material,
        locale: old.i18n.locale != new.i18n.locale,
        modifier: startup || old.keyboard.modifier != new.keyboard.modifier,
        startup: startup || old.startup.launch_at_login != new.startup.launch_at_login,
        logging: startup || old.logging.level != new.logging.level,
        windows_disabled: !new.windows.enabled
            && (startup || old.windows.enabled != new.windows.enabled),
        thumbnails: startup || old.layout.thumbnails_enabled != new.layout.thumbnails_enabled,
        focused_thumbnail_prewarm: startup
            || old.layout.focused_thumbnail_prewarm != new.layout.focused_thumbnail_prewarm,
        show_app_name_in_cards: old.layout.show_app_name_in_cards
            != new.layout.show_app_name_in_cards,
        thumbnail_size: old.layout.thumbnail_size != new.layout.thumbnail_size,
        show_other_desktops: old.windows.show_other_desktops != new.windows.show_other_desktops,
        mouse: startup || old.mouse != new.mouse,
        clipboard_enabled: startup || old.clipboard.enabled != new.clipboard.enabled,
        clipboard_shortcut: startup || old.clipboard.shortcut != new.clipboard.shortcut,
        window_control: startup || old.window_control.enabled != new.window_control.enabled,
        quick_actions: startup || old.quick_actions.enabled != new.quick_actions.enabled,
        keystroke_display: startup || old.keystroke_display != new.keystroke_display,
        updates: startup
            || old.updates.automatically_check != new.updates.automatically_check
            || old.updates.automatically_download != new.updates.automatically_download,
    }
}

/// Apply all runtime side effects for a new configuration in one pass.
pub(crate) fn apply_config_change(old: &Config, new: &Config, source: ConfigChangeSource) {
    let flags = change_flags(old, new, source);

    if flags.locale {
        crate::i18n::apply_config_locale(&new.i18n.locale);
    }

    if flags.visual || flags.locale {
        // UI refresh must run on the main thread; every caller (settings, menu, reload, startup)
        // enters from the main thread.
        crate::ui_coordinator::apply_theme_and_locale_refresh();
    }

    if flags.settings_appearance || flags.locale {
        // Font-size changes only affect the switcher preview; rebuilding Settings makes other
        // controls jump while a slider is being dragged.
        crate::settings::refresh_system_appearance();
    }

    if flags.panel_material {
        // Each material installs a different root view, so existing panels swap their
        // backdrop; the settings page rebuild comes from the settings_appearance path above
        // (appearance changed), which re-derives the sub-option rows' visibility.
        unsafe {
            crate::overlay::apply_backdrop_material();
            crate::clipboard::apply_backdrop_material();
            crate::keystroke_display::apply_backdrop_material();
        }
    }

    if flags.modifier {
        crate::menu::set_shortcut_mode(new.keyboard.modifier == "command");
    }
    if flags.startup {
        crate::autostart::sync(new.startup.launch_at_login);
    }
    if flags.logging {
        let level = match new.logging.level.as_str() {
            "debug" => crate::logger::LogLevel::Debug,
            _ => crate::logger::LogLevel::Info,
        };
        crate::logger::reconfigure(level);
    }
    if flags.clipboard_shortcut {
        crate::event_monitor::set_clipboard_shortcut(&new.clipboard.shortcut);
        crate::settings::refresh_clipboard_shortcut_control_from_config();
        crate::onboarding::refresh_clipboard_shortcut(&new.clipboard.shortcut);
    }
    if flags.windows_disabled {
        crate::overlay::reset_switcher();
    }

    if flags.thumbnails {
        crate::menu::set_thumbnail_mode(new.layout.thumbnails_enabled);
        crate::overlay::reset_switcher();
        if new.layout.thumbnails_enabled {
            crate::thumbnail::start();
        } else {
            crate::thumbnail::clear_runtime_cache();
            crate::thumbnail::stop_focused_prewarm_worker();
        }
    }

    if flags.focused_thumbnail_prewarm || flags.thumbnails {
        if new.layout.focused_thumbnail_prewarm && new.layout.thumbnails_enabled {
            crate::thumbnail::start_focused_prewarm_worker();
        } else {
            crate::thumbnail::stop_focused_prewarm_worker();
        }
    }

    if flags.show_app_name_in_cards {
        // The caption format changed: dismiss the overlay so the next summon rebuilds the
        // cards from the new signature (reusing them would keep the old captions).
        crate::overlay::reset_switcher();
    }

    if flags.show_other_desktops {
        // The candidate scope changed: dismiss the overlay so the cards are rebuilt for the new
        // policy, and invalidate a collection already running under the old one so its result
        // cannot be applied -- or reused by the next summon -- after the switch moved.
        crate::overlay::reset_switcher();
        crate::window_refresh::invalidate_collection_for_policy_change();
    }

    if needs_in_place_control_sync(&flags) {
        // Keep an already-open settings window in sync in place, without activating the app or
        // rebuilding the window.
        crate::settings::refresh_switcher_keystroke_and_mouse_controls_from_config();
    }

    if flags.mouse {
        crate::mouse::resolve::invalidate_cache();
        crate::mouse::pointer::apply();
        if old.mouse.enabled != new.mouse.enabled || matches!(source, ConfigChangeSource::Startup) {
            if crate::mouse::effective_enabled() {
                crate::mouse::start();
            } else if !matches!(source, ConfigChangeSource::Startup) {
                crate::mouse::stop();
            }
        }
    }

    if flags.clipboard_enabled {
        if new.clipboard.enabled {
            crate::clipboard::start();
        } else if !matches!(source, ConfigChangeSource::Startup) {
            // Turning the switch off means "stop keeping records": the in-memory history and the
            // file/cache it was written to go with it (see the row's description).
            crate::clipboard::stop();
            crate::clipboard::clear_history_and_disk();
        }
    }

    if flags.window_control {
        if new.window_control.enabled {
            crate::window_management::start();
        } else if !matches!(source, ConfigChangeSource::Startup) {
            crate::window_management::stop();
        }
    }
    if flags.quick_actions {
        if new.quick_actions.enabled {
            crate::quick_actions::start();
        } else if !matches!(source, ConfigChangeSource::Startup) {
            crate::quick_actions::stop();
        }
    }
    if flags.keystroke_display {
        crate::keystroke_display::apply_config_change(old, new);
    }
    if flags.updates {
        crate::updater::set_automatic_checks(new.updates.automatically_check);
        crate::updater::set_automatic_downloads(new.updates.automatically_download);
    }

    // Keep the status-bar service toggles current for changes made in Settings or by reload.
    crate::menu::refresh_service_menu();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_configs_are_noop() {
        let cfg = Config::default();
        assert_eq!(
            change_flags(&cfg, &cfg, ConfigChangeSource::Settings),
            ChangeFlags::default()
        );
    }

    #[test]
    fn enabled_service_changes_are_detected() {
        let old = Config::default();
        let mut new = old.clone();
        new.mouse.enabled = true;
        new.clipboard.enabled = true;
        new.window_control.enabled = true;
        new.quick_actions.enabled = true;
        let flags = change_flags(&old, &new, ConfigChangeSource::Settings);
        assert!(
            flags.mouse && flags.clipboard_enabled && flags.window_control && flags.quick_actions
        );
    }

    #[test]
    fn clipboard_shortcut_changes_are_detected_independently_of_the_master_switch() {
        let old = Config::default();
        let mut new = old.clone();
        new.clipboard.shortcut = "cmd+shift+v".into();
        let flags = change_flags(&old, &new, ConfigChangeSource::Settings);
        assert!(flags.clipboard_shortcut);
        assert!(!flags.clipboard_enabled);
    }

    #[test]
    fn a_thumbnail_size_change_alone_resyncs_the_open_settings_window() {
        // Only `layout.thumbnail_size` changes: the overlay picks it up on its next layout, and the
        // settings page must follow in place or it keeps showing the old size.
        let old = Config::default();
        let mut new = old.clone();
        new.layout.thumbnail_size = "85".into();
        let flags = change_flags(&old, &new, ConfigChangeSource::Settings);
        assert!(flags.thumbnail_size, "the field change must be detected");
        assert!(flags.visual, "the overlay repaints from the new size");
        assert!(
            !flags.thumbnails
                && !flags.focused_thumbnail_prewarm
                && !flags.modifier
                && !flags.mouse
                && !flags.keystroke_display,
            "no other service changed"
        );
        assert!(
            needs_in_place_control_sync(&flags),
            "the thumbnail size alone must trigger the in-place control sync"
        );
        assert!(
            !needs_in_place_control_sync(&ChangeFlags::default()),
            "an empty change set must not resync anything"
        );
    }

    #[test]
    fn other_desktop_switch_changes_are_detected_alone() {
        // The switch alone must dismiss the overlay (the cards it was built from no longer describe
        // what is admitted) without touching any other service.
        let old = Config::default();
        let mut new = old.clone();
        new.windows.show_other_desktops = true;
        let flags = change_flags(&old, &new, ConfigChangeSource::Settings);
        assert!(flags.show_other_desktops);
        assert!(
            !flags.show_app_name_in_cards
                && !flags.mouse
                && !flags.thumbnails
                && !flags.clipboard_enabled
        );
    }

    #[test]
    fn keystroke_display_changes_are_detected_without_enabling_other_services() {
        let old = Config::default();
        let mut new = old.clone();
        new.keystroke_display.enabled = true;
        let flags = change_flags(&old, &new, ConfigChangeSource::Settings);
        assert!(flags.keystroke_display);
        assert!(!flags.mouse && !flags.clipboard_enabled && !flags.window_control);
    }
}
