//! 配置变更的统一运行时应用入口。
//! Central runtime application for configuration changes.

use crate::config::Config;

/// 配置变更来源,用于区分启动时的首次应用与运行中的增量变更。
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
    locale: bool,
    modifier: bool,
    startup: bool,
    logging: bool,
    windows_disabled: bool,
    thumbnails: bool,
    mouse: bool,
    clipboard_enabled: bool,
    clipboard_persist: bool,
    window_control: bool,
    quick_actions: bool,
    updates: bool,
}

fn change_flags(old: &Config, new: &Config, source: ConfigChangeSource) -> ChangeFlags {
    let startup = matches!(source, ConfigChangeSource::Startup);
    ChangeFlags {
        visual: old.appearance != new.appearance
            || old.layout.card_text_size != new.layout.card_text_size
            || old.colors != new.colors
            || old.fonts != new.fonts,
        locale: old.i18n.locale != new.i18n.locale,
        modifier: startup || old.keyboard.modifier != new.keyboard.modifier,
        startup: startup || old.startup.launch_at_login != new.startup.launch_at_login,
        logging: startup || old.logging.level != new.logging.level,
        windows_disabled: !new.windows.enabled
            && (startup || old.windows.enabled != new.windows.enabled),
        thumbnails: startup || old.layout.thumbnails_enabled != new.layout.thumbnails_enabled,
        mouse: startup || old.mouse != new.mouse,
        clipboard_enabled: startup || old.clipboard.enabled != new.clipboard.enabled,
        clipboard_persist: old.clipboard.persist != new.clipboard.persist,
        window_control: startup || old.window_control.enabled != new.window_control.enabled,
        quick_actions: startup || old.quick_actions.enabled != new.quick_actions.enabled,
        updates: startup
            || old.updates.automatically_check != new.updates.automatically_check
            || old.updates.automatically_download != new.updates.automatically_download,
    }
}

/// 将一份新配置的运行时副作用一次性应用完毕。
/// Apply all runtime side effects for a new configuration in one pass.
pub(crate) fn apply_config_change(old: &Config, new: &Config, source: ConfigChangeSource) {
    let flags = change_flags(old, new, source);

    if flags.locale {
        crate::i18n::apply_config_locale(&new.i18n.locale);
    }

    if flags.visual || flags.locale {
        // UI 刷新必须在主线程调用;所有入口(设置、菜单、Reload、启动)都在主线程。
        // UI refresh must run on the main thread; every caller (settings, menu, reload, startup)
        // enters from the main thread.
        crate::ui_coordinator::apply_theme_and_locale_refresh();
        crate::settings::refresh_system_appearance();
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
        }
    }

    if flags.modifier || flags.thumbnails {
        // 菜单或设置页修改后,原位同步已打开的设置窗口,不激活应用也不重建窗口。
        // Keep an already-open settings window in sync in place, without activating the app or
        // rebuilding the window.
        crate::settings::refresh_switcher_controls_from_config();
    }

    if flags.mouse {
        crate::mouse::resolve::invalidate_cache();
        crate::mouse::pointer::apply();
        if old.mouse.enabled != new.mouse.enabled || matches!(source, ConfigChangeSource::Startup) {
            if new.mouse.enabled {
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
            crate::clipboard::stop();
        }
    }
    if flags.clipboard_persist {
        crate::clipboard::apply_persist_toggle(new.clipboard.persist);
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
    if flags.updates {
        crate::updater::set_automatic_checks(new.updates.automatically_check);
        crate::updater::set_automatic_downloads(new.updates.automatically_download);
    }

    // Keep the status-bar service toggles current for changes made in Settings or by reload.
    // 保证设置页或重新加载配置后，状态栏中的功能大类开关立即同步。
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
}
