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
    // The footer title's visibility changed. That switch also decides whether the footer band exists,
    // and the band is summon-time geometry (card viewport, document, scroller, panel height), so a
    // standing panel is dismissed rather than repainted; an open settings window follows in place
    // like any other reloaded control.
    show_footer_title: bool,
    // The footer title's *size* changed without crossing 0: the band height follows the text size, so
    // the geometry has to be re-laid out and an open settings page has to update the slider + readout.
    status_bar_size: bool,
    // The pinned card size changed: the overlay picks it up on its next layout, and an open settings
    // window has to follow in place (`refresh_switcher_keystroke_and_mouse_controls_from_config`).
    thumbnail_size: bool,
    // The candidate scope itself changed: the cards already built no longer describe what the
    // switch now admits, so an open overlay must be dismissed rather than reused.
    show_other_desktops: bool,
    mouse: bool,
    clipboard_enabled: bool,
    clipboard_shortcut: bool,
    /// The persisted pause intent changed (menu, settings switch, or an edited config file).
    clipboard_paused: bool,
    /// The excluded-applications list changed (the settings list, or an edited config file).
    clipboard_excluded_apps: bool,
    // The "separate clear all" switch: the picker header shows or hides its second button live.
    clipboard_clear_actions: bool,
    clipboard_image_rows: bool,
    // The binding list or either group switch changed: the engine republishes its table and starts
    // or stops the tap. The two legacy switches (which used to own a tap each) are covered by this
    // one flag: one engine now serves both groups.
    key_bindings: bool,
    keystroke_display: bool,
    updates: bool,
    // Any of the four in-panel action switches changed: the panel's published key snapshot has to
    // be rebuilt, or the tap keeps claiming (or ignoring) the old chord set.
    panel_actions: bool,
    // The menu-bar icon visibility changed. Separate from `startup`, which syncs the login item:
    // toggling the icon must not re-register the app with the system, and a login-item change must
    // not restyle the status item.
    menu_bar_icon: bool,
    // Any Finder Menu field changed. The extension renders a model the main app pushes, so it has
    // to be broadcast; an open Settings page has to follow in place like any other section.
    finder_menu: bool,
}

/// Whether an already-open settings window has to re-read the config in place after this change.
///
/// Pure so the trigger can be asserted without a window: adding a field to `ChangeFlags` without
/// adding it here leaves the page showing the old value while the app uses the new one (the
/// thumbnail size hit exactly that).
fn needs_in_place_control_sync(flags: &ChangeFlags) -> bool {
    flags.modifier
        || flags.panel_actions
        || flags.thumbnails
        || flags.focused_thumbnail_prewarm
        || flags.thumbnail_size
        // The card switches live on the Switcher page and are re-read in place below. Without
        // this a reload that only flips one of them updates the cards and leaves the switch showing
        // the old value.
        || flags.show_app_name_in_cards
        || flags.show_footer_title
        || flags.status_bar_size
        || flags.clipboard_image_rows
        || flags.clipboard_paused
        || flags.keystroke_display
        || flags.mouse
        || flags.key_bindings
        || flags.finder_menu
}

/// Appearance fields that, when they change, require rebuilding the settings content: the window's
/// own layers are painted from concrete palette colors at construction time. `panel_material` is
/// deliberately excluded -- the settings palette follows the theme, and the material's only
/// structural effect on the page (the glass-style row) is reflowed in place.
fn appearance_excluding_material_changed(old: &Config, new: &Config) -> bool {
    old.appearance.theme != new.appearance.theme
        || old.appearance.glass_style != new.appearance.glass_style
        || old.appearance.corner_radius != new.appearance.corner_radius
        || old.colors != new.colors
}

/// Whether a change requires rebuilding the settings content in place.
///
/// Shared by the runtime apply path and the import replay so the two cannot disagree about whether
/// `apply_config_change` rebuilt the window. Fonts and layout stay out on purpose: a slider drag
/// must not rebuild.
pub(crate) fn settings_window_rebuild_required(old: &Config, new: &Config) -> bool {
    appearance_excluding_material_changed(old, new) || old.i18n.locale != new.i18n.locale
}

fn change_flags(old: &Config, new: &Config, source: ConfigChangeSource) -> ChangeFlags {
    let startup = matches!(source, ConfigChangeSource::Startup);
    ChangeFlags {
        visual: old.appearance != new.appearance
            || old.layout.card_text_size != new.layout.card_text_size
            || old.layout.thumbnail_size != new.layout.thumbnail_size
            || old.colors != new.colors
            || old.fonts != new.fonts,
        settings_appearance: appearance_excluding_material_changed(old, new),
        panel_material: old.appearance.panel_material != new.appearance.panel_material,
        locale: old.i18n.locale != new.i18n.locale,
        modifier: startup || old.keyboard.modifier != new.keyboard.modifier,
        startup: startup || old.startup.launch_at_login != new.startup.launch_at_login,
        menu_bar_icon: startup || old.startup.hide_menu_bar_icon != new.startup.hide_menu_bar_icon,
        logging: startup || old.logging.level != new.logging.level,
        windows_disabled: !new.windows.enabled
            && (startup || old.windows.enabled != new.windows.enabled),
        thumbnails: startup || old.layout.thumbnails_enabled != new.layout.thumbnails_enabled,
        focused_thumbnail_prewarm: startup
            || old.layout.focused_thumbnail_prewarm != new.layout.focused_thumbnail_prewarm,
        show_app_name_in_cards: old.layout.show_app_name_in_cards
            != new.layout.show_app_name_in_cards,
        // The footer band is driven by the size now (0 = no band), so this flag tracks the derived
        // visibility: comparing the retired `layout.show_footer_title` bool would leave a standing
        // panel with its old geometry after the size crosses 0.
        show_footer_title: (old.fonts.status_bar_size > 0.0) != (new.fonts.status_bar_size > 0.0),
        status_bar_size: old.fonts.status_bar_size != new.fonts.status_bar_size,
        thumbnail_size: old.layout.thumbnail_size != new.layout.thumbnail_size,
        show_other_desktops: old.windows.show_other_desktops != new.windows.show_other_desktops,
        mouse: startup || old.mouse != new.mouse,
        clipboard_enabled: startup || old.clipboard.enabled != new.clipboard.enabled,
        clipboard_shortcut: startup || old.clipboard.shortcut != new.clipboard.shortcut,
        clipboard_paused: startup || old.clipboard.paused != new.clipboard.paused,
        clipboard_excluded_apps: startup
            || old.clipboard.excluded_apps != new.clipboard.excluded_apps,
        clipboard_clear_actions: startup
            || old.clipboard.separate_clear_all != new.clipboard.separate_clear_all,
        clipboard_image_rows: startup || old.clipboard.image_rows != new.clipboard.image_rows,
        key_bindings: startup || old.key_bindings != new.key_bindings,
        keystroke_display: startup || old.keystroke_display != new.keystroke_display,
        updates: startup
            || old.updates.automatically_check != new.updates.automatically_check
            || old.updates.automatically_download != new.updates.automatically_download,
        panel_actions: startup
            || old.windows.panel_close_enabled != new.windows.panel_close_enabled
            || old.windows.panel_minimize_enabled != new.windows.panel_minimize_enabled
            || old.windows.panel_hide_enabled != new.windows.panel_hide_enabled
            || old.windows.panel_quit_enabled != new.windows.panel_quit_enabled,
        // Not tied to `startup`: the main app's startup path already owns the first broadcast, and
        // the comparison is what makes an edit, a reload and a defaults restore reach the extension.
        finder_menu: old.finder_menu != new.finder_menu,
    }
}

/// Whether this change can create or dissolve a reserved-chord collision: the switcher's modifier,
/// the clipboard shortcut and the binding list are the three inputs the coordination reads.
///
/// Pure, so the wiring is assertable without running the side effects (the same shape as
/// `needs_in_place_control_sync`).
fn needs_reserved_shortcut_coordination(flags: &ChangeFlags) -> bool {
    flags.modifier || flags.clipboard_shortcut || flags.key_bindings
}

/// Apply all runtime side effects for a new configuration in one pass.
/// What the reload path should do with the clipboard gate, from the two configs.
///
/// Pure so the ordering can be asserted without the AppKit main thread: `paused` outranks the master
/// switch, so neither disabling nor re-enabling a paused session may open the gate -- only a genuine
/// pause->resume transition may try, and even that can fail to baseline.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PauseGateAction {
    /// Leave the gate exactly as it is.
    Leave,
    /// The session became paused: close the gate.
    Pause,
    /// The session was paused and is now not: try to resume (which re-baselines).
    Resume,
}

pub(crate) fn pause_gate_action(old_paused: bool, new_paused: bool) -> PauseGateAction {
    if new_paused {
        PauseGateAction::Pause
    } else if old_paused {
        PauseGateAction::Resume
    } else {
        PauseGateAction::Leave
    }
}

/// Whether a change to the exclusion list has to rebuild the settings content.
///
/// The settings UI's own edits queue that rebuild themselves, and startup is not a list change:
/// `change_flags` marks this field true for every startup, so queueing there would leave a rebuild
/// request behind that runs on a later run-loop turn -- possibly while someone is interacting with
/// the page it replaces. A change from an edited config file, a restore, or the menu still is a list
/// change and rebuilds.
fn rebuilds_settings_for_excluded_list(list_changed: bool, source: ConfigChangeSource) -> bool {
    list_changed
        && !matches!(
            source,
            ConfigChangeSource::Settings | ConfigChangeSource::Startup
        )
}

pub(crate) fn apply_config_change(old: &Config, new: &Config, source: ConfigChangeSource) {
    let started = std::time::Instant::now();
    let flags = change_flags(old, new, source);

    // Reserved chords are settled once, before anything below publishes a table, caches the
    // clipboard chord or paints a control: a modifier change, a clipboard change and a binding edit
    // can each create a collision, and every consumer has to see the same settled config. The
    // settled copy is what the branches below read, so no later `publish` can disagree with it.
    let effective = if needs_reserved_shortcut_coordination(&flags) {
        let mut candidate = match crate::config::CONFIG.read() {
            Ok(cfg) => cfg.clone(),
            Err(_) => new.clone(),
        };
        let conflicts = candidate.settle_reserved_shortcuts();
        if conflicts.is_empty() {
            candidate
        } else {
            for conflict in &conflicts {
                crate::log_info!("[keybind] {} (reserved shortcut coordination)", conflict);
            }
            if let Ok(mut current) = crate::config::CONFIG.write() {
                *current = candidate.clone();
            }
            crate::config::persist_config_now();
            candidate
        }
    } else {
        match crate::config::CONFIG.read() {
            Ok(cfg) => cfg.clone(),
            Err(_) => new.clone(),
        }
    };

    // A clipboard purge an earlier session could not finish runs first, on every apply and whether
    // the clipboard switch is on or off (`clear_on_quit` failures happen with it on). It must come
    // before anything loads or writes, or the user sees records they asked to delete. Cheap when
    // nothing is pending: two existence checks.
    let purge = crate::clipboard::process_pending_purge();
    if purge == crate::clipboard::PurgeProgress::Completed
        && new.clipboard.enabled
        && crate::clipboard::needs_load()
    {
        // The purge left the session uninitialized (its key is acquired by a load, off the main
        // thread). Without this, a session that recovered from a purge failure would stay unable to
        // save until the next launch. `reload_storage`, not `start`: the polling timer is usually
        // already running, and `start` would return before loading.
        crate::clipboard::reload_storage();
    }

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
        // Each material installs a different root view, so existing panels swap their backdrop.
        unsafe {
            crate::overlay::apply_backdrop_material();
            crate::clipboard::apply_backdrop_material();
            crate::keystroke_display::apply_backdrop_material();
        }
        // The settings content is not rebuilt for a material-only change: re-read the material and
        // glass-style controls and reflow the dependent row in place.
        crate::settings::refresh_appearance_controls_from_config();
    }

    if flags.modifier {
        crate::menu::set_shortcut_mode(&effective.keyboard.modifier);
    }
    if flags.startup {
        crate::autostart::sync(new.startup.launch_at_login);
    }
    if flags.menu_bar_icon {
        crate::menu::set_status_item_visible(!new.startup.hide_menu_bar_icon);
    }
    if flags.logging {
        let level = match new.logging.level.as_str() {
            "debug" => crate::logger::LogLevel::Debug,
            _ => crate::logger::LogLevel::Info,
        };
        crate::logger::reconfigure(level);
    }
    if flags.clipboard_shortcut || flags.modifier || flags.key_bindings {
        // The settled chord, not the requested one: the coordination may have moved it.
        crate::event_monitor::set_clipboard_shortcut(&effective.clipboard.shortcut);
        crate::settings::refresh_clipboard_shortcut_control_from_config();
        // The settled chord: the coordination above may have moved it, and the guide must name
        // the shortcut the app actually listens for.
        crate::onboarding::refresh_clipboard_shortcut(&effective.clipboard.shortcut);
    }
    if flags.clipboard_image_rows {
        crate::clipboard::rebuild_rows_for_layout_change();
    }
    if flags.clipboard_clear_actions {
        crate::clipboard::refresh_clear_actions_after_config_change();
        // An already-open settings window must re-read the switch too, or it shows the old
        // value while the picker header already follows the new one.
        crate::settings::refresh_clipboard_separate_clear_all_switch_from_config();
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

    if flags.show_footer_title || flags.status_bar_size {
        // The switch owns the footer band's height (`theme::footer_band_h` folds to zero while the
        // title is off), so a standing panel would keep the old geometry: the card viewport, the
        // document and the scroller are all sized from the band at summon time. Dismiss it instead
        // of repainting, and the next summon lays out against the new band.
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

    // The pause gate must be settled BEFORE the clipboard can poll. `start()` records the current
    // pasteboard immediately (the existing "the first summon is not empty" behaviour), so a resume
    // whose baseline has not been adopted yet would backfill exactly the content the pause
    // promised to drop. Only a genuine pause->resume transition re-baselines; an ordinary startup
    // and an ordinary "enable" keep their documented record-current behaviour.
    if flags.clipboard_paused {
        // The ordering lives in `pause_gate_action`: paused outranks the master switch, so a
        // disable or a re-enable of a paused session is `Pause`/`Leave`, never a resume.
        match pause_gate_action(old.clipboard.paused, new.clipboard.paused) {
            PauseGateAction::Pause => {
                crate::clipboard::set_collection_paused(true);
            }
            PauseGateAction::Resume if crate::clipboard::set_collection_paused(false) => {
                // The baseline could not be established (no readable pasteboard). Keep every surface
                // in the paused state instead of claiming that recording resumed.
                if let Ok(mut w) = crate::config::CONFIG.write() {
                    w.clipboard.paused = true;
                }
                crate::config::persist_config_now();
                crate::settings::refresh_clipboard_pause_switch_from_config();
                crate::menu::refresh_clipboard_pause_item();
                crate::log_info!(
                    "Clipboard collection: resume failed (pasteboard unreadable), staying paused"
                );
            }
            PauseGateAction::Resume => {}
            PauseGateAction::Leave => {}
        }
    }

    if flags.clipboard_paused {
        // The panel's orb is built with the window; keep its glyph and label in step with the
        // effective state (a resume that failed above must leave it showing "paused"), and re-read
        // the settings switch so neither surface can disagree with the gate.
        crate::clipboard::refresh_pause_button();
        crate::settings::refresh_clipboard_pause_switch_from_config();
    }
    if rebuilds_settings_for_excluded_list(flags.clipboard_excluded_apps, source) {
        // The list card is laid out from the list, so a change from an edited config file has to
        // rebuild the page. The settings UI's own edits already queue that rebuild themselves, and
        // startup is not a list change: `change_flags` marks this field true for every startup, so
        // queueing here would leave a rebuild request behind that runs on a later run-loop turn --
        // possibly while a smoke or the user is interacting with the page it replaces.
        crate::settings::rebuild_settings_content_deferred();
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

    // The panel's action switches change whether the tap must run at all, so they belong to the
    // SAME branch as the binding switches: publishing a snapshot that claims chords while the tap
    // is not running makes the switches look live while the keys still go to the app.
    if flags.key_bindings || flags.modifier || flags.clipboard_shortcut || flags.panel_actions {
        // Publish first, then start: a tap that begins between the two must never read a stale
        // table. The published config is the settled one, so a binding that had to yield is gone
        // from the table as well. Stopping with both groups off is the graceful direction.
        crate::key_bindings::publish(&effective);
        if crate::key_bindings::enabled_in(&effective)
            || crate::panel_actions::configured_in(&effective)
        {
            crate::key_bindings::start();
        } else if !matches!(source, ConfigChangeSource::Startup) {
            crate::key_bindings::stop();
        }
        // The tolerated hold modifier follows the trigger mode, so a mode change republishes too.
        let visible = crate::with_tab_state(|state_opt| {
            state_opt
                .as_ref()
                .is_some_and(|state| state.visible || state.pending_first_show)
        });
        crate::overlay::publish_panel_snapshot(visible);
    }
    if flags.keystroke_display {
        crate::keystroke_display::apply_config_change(old, new);
    }
    if flags.panel_actions {
        // Rebuild the tap-visible chord set. Visibility is read live here rather than cached: a
        // switch flipped while the panel is open must take effect on the very next key.
        let visible = crate::with_tab_state(|state_opt| {
            state_opt
                .as_ref()
                .is_some_and(|state| state.visible || state.pending_first_show)
        });
        crate::overlay::publish_panel_snapshot(visible);
    }
    if flags.updates {
        crate::updater::set_automatic_checks(new.updates.automatically_check);
        crate::updater::set_automatic_downloads(new.updates.automatically_download);
    }
    // The menu model carries final titles, so a locale change has to rebuild it even though no
    // finder_menu field changed: the extension would otherwise keep the previous language.
    if flags.finder_menu || flags.locale {
        // The extension renders the model the main app owns: push the rebuilt one so the next
        // context menu reflects the new settings, without waiting for a restart.
        crate::finder_menu::broadcast_model();
    }

    // Keep the status-bar service toggles current for changes made in Settings or by reload.
    crate::menu::refresh_service_menu();
    // The boundary over the full synchronous apply path, so a change that lost its rebuild (a
    // panel-material switch) can be compared against one that still rebuilds.
    crate::log_debug!(
        "[config] applied source={:?} in {}ms",
        source,
        started.elapsed().as_millis()
    );
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
    fn only_a_real_list_change_rebuilds_the_settings_content() {
        // Startup sets `clipboard_excluded_apps` on every launch, so queueing a rebuild there left a
        // request that ran on a later run-loop turn -- which replaced the settings page while a smoke
        // (or the user) was interacting with it. The settings UI's own edits already queue theirs.
        assert!(!rebuilds_settings_for_excluded_list(
            false,
            ConfigChangeSource::Reload
        ));
        assert!(!rebuilds_settings_for_excluded_list(
            true,
            ConfigChangeSource::Startup
        ));
        assert!(!rebuilds_settings_for_excluded_list(
            true,
            ConfigChangeSource::Settings
        ));
        // A real external change still rebuilds: an edited config file, a restore, or the menu.
        assert!(rebuilds_settings_for_excluded_list(
            true,
            ConfigChangeSource::Reload
        ));
        assert!(rebuilds_settings_for_excluded_list(
            true,
            ConfigChangeSource::RestoreDefaults
        ));
        assert!(rebuilds_settings_for_excluded_list(
            true,
            ConfigChangeSource::Menu
        ));
    }
    #[test]
    fn a_panel_action_switch_changes_alone_and_republishes() {
        // Each of the four switches must be detected on its own: a flag that only fired when two
        // changed would leave the tap claiming a chord the user had just switched off.
        for set in [
            |c: &mut Config| c.windows.panel_close_enabled = true,
            |c: &mut Config| c.windows.panel_minimize_enabled = true,
            |c: &mut Config| c.windows.panel_hide_enabled = true,
            |c: &mut Config| c.windows.panel_quit_enabled = true,
        ] {
            let old = Config::default();
            let mut new = old.clone();
            set(&mut new);
            let flags = change_flags(&old, &new, ConfigChangeSource::Settings);
            assert!(
                flags.panel_actions,
                "a panel action switch must be detected"
            );
            assert!(needs_in_place_control_sync(&flags));
        }
        // Toggling one back off is a change too.
        let mut on = Config::default();
        on.windows.panel_quit_enabled = true;
        let mut off = on.clone();
        off.windows.panel_quit_enabled = false;
        assert!(change_flags(&on, &off, ConfigChangeSource::Settings).panel_actions);
    }

    #[test]
    fn enabled_service_changes_are_detected() {
        let old = Config::default();
        let mut new = old.clone();
        new.mouse.enabled = true;
        new.clipboard.enabled = true;
        new.key_bindings.enabled = true;
        let flags = change_flags(&old, &new, ConfigChangeSource::Settings);
        assert!(flags.mouse && flags.clipboard_enabled && flags.key_bindings);
    }

    /// A panel-material change must not rebuild the settings content -- the material's only
    /// structural effect (the glass-style row) reflows in place. The fields painted into the
    /// window's own layers, and the locale, still rebuild; fonts and layout still never do.
    #[test]
    fn panel_material_changes_do_not_rebuild_but_baked_appearance_does() {
        let base = Config::default();
        let mutate = |f: fn(&mut Config)| {
            let mut new = base.clone();
            f(&mut new);
            new
        };

        let material = mutate(|cfg| cfg.appearance.panel_material = "frost".into());
        let flags = change_flags(&base, &material, ConfigChangeSource::Settings);
        assert!(flags.panel_material, "the material change was not detected");
        assert!(
            !flags.settings_appearance,
            "a panel-material change must not set the settings-appearance rebuild flag"
        );
        assert!(!settings_window_rebuild_required(&base, &material));

        // Material mixed with a field no narrow sync covers (logging) still must not rebuild: the
        // import/restore replay refills the controls in place for exactly this case. Use a level
        // that genuinely differs from the default, or the "mix" would change nothing.
        let mut mixed = material.clone();
        mixed.logging.level = if base.logging.level == "debug" {
            "info".into()
        } else {
            "debug".into()
        };
        assert_ne!(mixed.logging.level, base.logging.level);
        assert!(!settings_window_rebuild_required(&base, &mixed));

        for (label, new) in [
            ("theme", mutate(|cfg| cfg.appearance.theme = "dark".into())),
            (
                "glass style",
                mutate(|cfg| cfg.appearance.glass_style = "clear".into()),
            ),
            (
                "corner radius",
                mutate(|cfg| cfg.appearance.corner_radius = 12.0),
            ),
            ("locale", mutate(|cfg| cfg.i18n.locale = "zh-Hans".into())),
        ] {
            assert!(
                settings_window_rebuild_required(&base, &new),
                "{label} must rebuild the settings content"
            );
        }

        // Fonts and layout stay out: a slider drag must not rebuild.
        assert!(!settings_window_rebuild_required(
            &base,
            &mutate(|cfg| cfg.fonts.status_bar_size += 1.0)
        ));
        assert!(!settings_window_rebuild_required(
            &base,
            &mutate(|cfg| cfg.layout.card_text_size += 1.0)
        ));
    }

    /// Any part of the binding feature republishes the engine: the master switch, a row switch, or
    /// an edit to the list itself.
    /// The coordination has to run for all three inputs, and only for them.
    /// Every consumer of a settled field inside `apply_config_change` must read the coordinated
    /// copy: a new one reading the requested config would cache or paint a value the runtime does
    /// not use (the onboarding hint did exactly that). Source-scanning, like the motion-token test.
    #[test]
    fn settled_fields_are_read_from_the_effective_config() {
        let source = include_str!("runtime_config.rs");
        let body_start = source
            .find("pub(crate) fn apply_config_change")
            .expect("apply_config_change is in this file");
        let body = &source[body_start..];
        let body_end = body.find("\n#[cfg(test)]").unwrap_or(body.len());
        let body = &body[..body_end];
        for line in body.lines() {
            if line.contains("clipboard.shortcut") || line.contains("key_bindings") {
                assert!(
                    !line.contains("new."),
                    "settled field read from the requested config: {line}"
                );
                assert!(
                    !line.contains("new:"),
                    "settled field read from the requested config: {line}"
                );
            }
        }
    }

    #[test]
    fn reserved_shortcut_coordination_covers_modifier_clipboard_and_bindings() {
        let base = Config::default();
        let flags_for = |mutate: fn(&mut Config)| {
            let mut new = base.clone();
            mutate(&mut new);
            change_flags(&base, &new, ConfigChangeSource::Settings)
        };
        assert!(needs_reserved_shortcut_coordination(&flags_for(|cfg| {
            cfg.keyboard.modifier = "option".into()
        })));
        assert!(needs_reserved_shortcut_coordination(&flags_for(|cfg| {
            cfg.clipboard.shortcut = "cmd+shift+v".into()
        })));
        assert!(needs_reserved_shortcut_coordination(&flags_for(|cfg| {
            cfg.key_bindings.items.as_mut().unwrap()[0].trigger = "option+j".into()
        })));
        // An unrelated change must not pay for it.
        assert!(!needs_reserved_shortcut_coordination(&flags_for(|cfg| {
            cfg.appearance.theme = "dark".into()
        })));
    }

    #[test]
    fn binding_changes_are_detected_for_switches_and_list_edits() {
        let old = Config::default();
        for mutate in [
            (|cfg: &mut Config| cfg.key_bindings.enabled = true) as fn(&mut Config),
            |cfg: &mut Config| {
                cfg.key_bindings.items.as_mut().unwrap()[0].enabled = false;
            },
            |cfg: &mut Config| {
                cfg.key_bindings.items.as_mut().unwrap()[0].trigger = "option+j".into();
            },
        ] {
            let mut new = old.clone();
            mutate(&mut new);
            let flags = change_flags(&old, &new, ConfigChangeSource::Settings);
            assert!(flags.key_bindings, "a binding change was not detected");
        }
        assert!(!change_flags(&old, &old.clone(), ConfigChangeSource::Settings).key_bindings);
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
    fn separate_clear_all_changes_are_detected_in_both_directions_alone() {
        // The reload path keys the picker relayout and the settings-switch sync off this flag
        // alone: it must fire in both directions and stay silent for unrelated fields.
        let old = Config::default();
        let mut on = old.clone();
        on.clipboard.separate_clear_all = true;
        let flags_on = change_flags(&old, &on, ConfigChangeSource::Reload);
        assert!(
            flags_on.clipboard_clear_actions,
            "off -> on must be detected"
        );
        assert!(
            !flags_on.clipboard_enabled && !flags_on.clipboard_shortcut && !flags_on.visual,
            "no unrelated flag may fire"
        );
        assert!(
            !needs_in_place_control_sync(&flags_on),
            "the switch sync is targeted, not a full settings-page rebuild"
        );
        let flags_off = change_flags(&on, &old, ConfigChangeSource::Reload);
        assert!(
            flags_off.clipboard_clear_actions,
            "on -> off must be detected"
        );
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
    fn the_clipboard_image_rows_change_flags_the_rebuild_and_the_settings_sync() {
        let old = crate::config::Config::default();
        let mut new = old.clone();
        new.clipboard.image_rows = 3;
        let flags = change_flags(&old, &new, ConfigChangeSource::Settings);
        assert!(flags.clipboard_image_rows);
        assert!(needs_in_place_control_sync(&flags));
    }

    #[test]
    fn other_desktop_switch_changes_are_detected_alone() {
        // The switch alone must dismiss the overlay (the cards it was built from no longer describe
        // what is admitted) without touching any other service.
        let old = Config::default();
        let mut new = old.clone();
        new.windows.show_other_desktops = !old.windows.show_other_desktops;
        let flags = change_flags(&old, &new, ConfigChangeSource::Settings);
        assert!(flags.show_other_desktops);
        assert!(
            !flags.show_app_name_in_cards
                && !flags.mouse
                && !flags.thumbnails
                && !flags.clipboard_enabled
        );
    }

    /// Every Finder Menu field is detected on its own: the extension only learns about a change
    /// when the model is broadcast, and an open Settings page must follow in place.
    #[test]
    fn finder_menu_changes_broadcast_the_model_and_sync_the_open_page() {
        let old = Config::default();
        let mutate = |f: fn(&mut Config)| {
            let mut new = old.clone();
            f(&mut new);
            new
        };
        // The default copy_path is on, so turning it off is the change.
        let copy_path_off = mutate(|cfg| cfg.finder_menu.copy_path = false);
        let cases = [
            ("enabled", mutate(|cfg| cfg.finder_menu.enabled = true)),
            (
                "layout",
                mutate(|cfg| cfg.finder_menu.layout = "flat".into()),
            ),
            ("copy path", copy_path_off),
            (
                "file types",
                mutate(|cfg| {
                    cfg.finder_menu
                        .new_file_types
                        .retain(|value| value != "json")
                }),
            ),
            (
                "terminals",
                mutate(|cfg| {
                    cfg.finder_menu.terminals =
                        vec!["com.apple.Terminal".into(), "com.googlecode.iterm2".into()]
                }),
            ),
        ];
        for (label, new) in cases {
            let flags = change_flags(&old, &new, ConfigChangeSource::Settings);
            assert!(flags.finder_menu, "{label} must be detected");
            assert!(
                needs_in_place_control_sync(&flags),
                "{label} must resync the open settings page"
            );
        }
        assert!(!change_flags(&old, &old.clone(), ConfigChangeSource::Settings).finder_menu);
    }

    #[test]
    fn keystroke_display_changes_are_detected_without_enabling_other_services() {
        let old = Config::default();
        let mut new = old.clone();
        new.keystroke_display.enabled = true;
        let flags = change_flags(&old, &new, ConfigChangeSource::Settings);
        assert!(flags.keystroke_display);
        assert!(!flags.mouse && !flags.clipboard_enabled && !flags.key_bindings);
    }
    /// A reload that changes ONLY `layout.show_app_name_in_cards` must still ask for an in-place
    /// refresh, or an open settings window keeps showing the old switch state while the cards
    /// follow the new value.
    #[test]
    fn changing_only_the_card_switch_still_refreshes_the_settings_controls() {
        let old = Config::default();
        let mut new = old.clone();
        new.layout.show_app_name_in_cards = !old.layout.show_app_name_in_cards;
        let flags = change_flags(&old, &new, ConfigChangeSource::Reload);
        assert!(
            flags.show_app_name_in_cards,
            "the change flag must see the switch and nothing else"
        );
        assert!(
            needs_in_place_control_sync(&flags),
            "a standalone card-switch change must refresh the settings controls"
        );
        // Counter-example: an unchanged config asks for no refresh at all.
        assert!(!needs_in_place_control_sync(&change_flags(
            &old,
            &old.clone(),
            ConfigChangeSource::Reload
        )));
    }
}
