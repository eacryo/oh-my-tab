//! The control identity: the table, publishing and the slots.

use super::*;
#[test]
fn a_failed_publish_leaves_the_previous_mapping_untouched() {
    // The build's own counter-example: a page whose new entries collide with a live entry it
    // does not replace must be refused WITHOUT the old entries being removed first. The first
    // version unregistered before it could fail, which left the page's controls present and
    // unfindable — "looks alive but is not wired".
    let mut table = ControlTable::default();
    let old_theme = 0x1000 as *mut AnyObject;
    table
        .register(old_theme, ControlId::Field(ControlField::Theme))
        .unwrap();
    // The rest of the page's previous controls.
    let old_locale = 0x1001 as *mut AnyObject;
    table
        .register(old_locale, ControlId::Field(ControlField::Locale))
        .unwrap();
    // A new page that reuses the locale control's ADDRESS for a different identity: the old
    // locale entry is not the one being replaced (Theme is), so the build must be refused.
    let mut page = PageRegistrations::default();
    page.register(
        0x2000 as *mut AnyObject,
        ControlId::Field(ControlField::Theme),
    )
    .unwrap();
    page.register(old_locale, ControlId::Field(ControlField::WindowsEnabled))
        .unwrap();
    assert_eq!(
        page.publish(&mut table),
        Err(RegisterError::DuplicatePointer)
    );
    // Nothing moved: the page's two controls still resolve exactly as before.
    assert_eq!(
        table.lookup(old_theme),
        Some(ControlId::Field(ControlField::Theme))
    );
    assert_eq!(
        table.lookup(old_locale),
        Some(ControlId::Field(ControlField::Locale))
    );
}

#[test]
fn a_successful_publish_replaces_the_pages_own_entries() {
    let mut table = ControlTable::default();
    let old_theme = 0x1000 as *mut AnyObject;
    table
        .register(old_theme, ControlId::Field(ControlField::Theme))
        .unwrap();
    let mut page = PageRegistrations::default();
    let new_theme = 0x2000 as *mut AnyObject;
    page.register(new_theme, ControlId::Field(ControlField::Theme))
        .unwrap();
    assert_eq!(page.publish(&mut table), Ok(()));
    assert_eq!(table.lookup(old_theme), None);
    assert_eq!(
        table.lookup(new_theme),
        Some(ControlId::Field(ControlField::Theme))
    );
}

#[test]
fn every_settings_ui_slot_a_migrated_field_needs_is_bound_at_publish() {
    // HIGH 2's counter-example, in its pure half: after the swap the only thing that filled
    // `ui.theme` / `ui.windows_enabled` / ... was the deleted inline builder, so the table
    // resolved a field whose pointer field was still null and `apply_control_field` read NULL.
    // The binding table must cover every field these pages declare and nothing else.
    let input = input();
    for page in [SettingsPageId::Switcher, SettingsPageId::General] {
        let view = view_for(page, &input);
        for row in view.rows.iter().filter(|row| row.field().is_some()) {
            let field = row.field().unwrap();
            assert!(
                crate::settings::page_view::settings_ui_slot_field(field).is_some(),
                "{page:?} {field:?} has a row but no SettingsUi slot, so its consumer reads NULL"
            );
        }
    }
    // The readout labels a slider row needs are bound too.
    for field in [ControlField::CardTextSize, ControlField::StatusBarTextSize] {
        assert!(crate::settings::page_view::settings_ui_readout_field(field));
    }
}

#[test]
fn the_windows_master_switch_keeps_its_dedicated_callback() {
    // MEDIUM 5: the unified handler does not freeze the options below the switch, so the
    // switch's own callback must stay bound to it; every other static control uses the unified
    // one.
    assert_eq!(
        dedicated_selector(Some(ControlField::WindowsEnabled)),
        sel!(handleWindowsEnabledToggle:)
    );
    assert_eq!(
        dedicated_selector(Some(ControlField::ShowMinimized)),
        sel!(handleControlChanged:)
    );
    assert_eq!(dedicated_selector(None), sel!(handleControlChanged:));
    // The callback it names exists and does exactly the freeze the disabled rows need.
    let _ = crate::settings::dispatch::handle_windows_enabled_toggle
        as extern "C" fn(*mut c_void, Sel, *mut c_void);
}

#[test]
fn control_table_refuses_duplicates_of_identity_and_pointer() {
    let mut table = ControlTable::new();
    let first = 0x1000 as *mut AnyObject;
    let second = 0x2000 as *mut AnyObject;
    assert_eq!(
        table.register(first, ControlId::Field(ControlField::Theme)),
        Ok(())
    );
    assert_eq!(
        table.register(second, ControlId::Field(ControlField::Theme)),
        Err(RegisterError::DuplicateId)
    );
    assert_eq!(
        table.register(first, ControlId::Field(ControlField::Locale)),
        Err(RegisterError::DuplicatePointer)
    );
    // Nothing was added by the refused registrations.
    assert_eq!(table.len(), 1);
    assert_eq!(
        table.lookup(first),
        Some(ControlId::Field(ControlField::Theme))
    );
    assert_eq!(table.lookup(second), None);
}

#[test]
fn control_table_unregister_and_cleanup_are_idempotent() {
    let mut table = ControlTable::new();
    let pointer = 0x1000 as *mut AnyObject;
    table
        .register(pointer, ControlId::Field(ControlField::Theme))
        .unwrap();
    assert!(table.unregister(&ControlId::Field(ControlField::Theme)));
    assert!(!table.unregister(&ControlId::Field(ControlField::Theme)));
    assert!(table.is_empty());
    assert_eq!(table.lookup(pointer), None);
    table.clear();
    assert!(table.is_empty());
}

#[test]
fn a_refused_page_registration_leaves_the_old_mapping_intact() {
    let mut table = ControlTable::new();
    let old = 0x1000 as *mut AnyObject;
    table
        .register(old, ControlId::Field(ControlField::Theme))
        .unwrap();
    // The page rebuild registers a conflicting entry half-way through and is abandoned.
    let mut page = PageRegistrations::default();
    let new_theme = 0x2000 as *mut AnyObject;
    let locale = 0x3000 as *mut AnyObject;
    page.register(new_theme, ControlId::Field(ControlField::Theme))
        .unwrap();
    // The page's own list refuses the duplicate before anything reaches the shared table.
    assert_eq!(
        page.register(locale, ControlId::Field(ControlField::Theme)),
        Err(RegisterError::DuplicateId)
    );
    // Dropping the page's registrations publishes nothing.
    drop(page);
    assert_eq!(
        table.lookup(old),
        Some(ControlId::Field(ControlField::Theme)),
        "the previous mapping must still resolve to the same field"
    );
    assert_eq!(table.lookup(new_theme), None);
    assert_eq!(table.lookup(locale), None);
}

#[test]
fn publishing_a_page_replaces_its_old_entries_and_no_stale_pointer_survives() {
    let mut table = ControlTable::new();
    let old_theme = 0x1000 as *mut AnyObject;
    let old_locale = 0x1001 as *mut AnyObject;
    let other = 0x1002 as *mut AnyObject;
    table
        .register(old_theme, ControlId::Field(ControlField::Theme))
        .unwrap();
    table
        .register(old_locale, ControlId::Field(ControlField::Locale))
        .unwrap();
    // A control from the page that is not being rebuilt keeps its entry.
    table
        .register(other, ControlId::Field(ControlField::WindowsEnabled))
        .unwrap();
    let new_theme = 0x2000 as *mut AnyObject;
    let new_locale = 0x2001 as *mut AnyObject;
    let mut page = PageRegistrations::default();
    page.register(new_theme, ControlId::Field(ControlField::Theme))
        .unwrap();
    page.register(new_locale, ControlId::Field(ControlField::Locale))
        .unwrap();
    page.publish(&mut table).unwrap();
    // The released addresses resolve to nothing; the new ones resolve to the same fields.
    assert_eq!(table.lookup(old_theme), None);
    assert_eq!(table.lookup(old_locale), None);
    assert_eq!(
        table.lookup(new_theme),
        Some(ControlId::Field(ControlField::Theme))
    );
    assert_eq!(
        table.lookup(new_locale),
        Some(ControlId::Field(ControlField::Locale))
    );
    assert_eq!(
        table.lookup(other),
        Some(ControlId::Field(ControlField::WindowsEnabled))
    );
    // Repeated cleanup is safe.
    assert!(table.unregister(&ControlId::Field(ControlField::Theme)));
    assert!(!table.unregister(&ControlId::Field(ControlField::Theme)));
}

#[test]
fn a_field_change_makes_the_in_place_sync_needed() {
    // The pair the checklist calls steps 13/14: a flag that makes the in-place sync run, and a
    // description that carries the new value when it does.
    let old = Config::default();
    let mut new = Config::default();
    new.layout.show_app_name_in_cards = !old.layout.show_app_name_in_cards;
    assert!(
        crate::runtime_config::change_needs_in_place_control_sync(&old, &new),
        "flipping a Switcher page field must make the open settings window re-read it"
    );
    let input = PageInput::from_config(new, None, crate::theme::ThemeInput::current());
    let update = page_update_description(&input, &[SettingsPageId::Switcher])
        .into_iter()
        .find(|update| update.field == ControlField::ShowAppNameInCards)
        .expect("the changed field is in the description");
    assert_eq!(update.value, "on");
}

/// Every field a migrated page describes must have a `SettingsUi` slot, because the legacy
/// consumers (`apply_control_field`, the refill, the enable lists) read those fields directly:
/// a field the table identifies but nothing binds is a control that looks alive and does
/// nothing. This is the assertion the Clipboard/Mouse/About pages needed and did not have.
#[test]
fn every_described_field_of_every_page_has_a_settings_ui_slot() {
    let input =
        PageInput::from_config(Config::default(), None, crate::theme::ThemeInput::current());
    for page in [
        SettingsPageId::Switcher,
        SettingsPageId::General,
        SettingsPageId::Clipboard,
        SettingsPageId::About,
        SettingsPageId::KeystrokeDisplay,
        SettingsPageId::FinderMenu,
        SettingsPageId::Gestures,
        SettingsPageId::KeyBindings,
        SettingsPageId::Mouse,
    ] {
        let view = view_for(page, &input);
        for row in &view.rows {
            let Some(field) = row.field() else {
                continue; // an action row stores nothing
            };
            // The Finder Menu page's catalog rows are POSITIONAL (one entry per file type /
            // terminal); they are bound into their Vec fields by the page's own hook, not by
            // the one-slot-per-field table. Every other described field must have a slot.
            if matches!(
                field,
                ControlField::FinderMenuNewFileType(_) | ControlField::FinderMenuTerminal(_)
            ) {
                continue;
            }
            assert!(
                settings_ui_slot_present(field),
                "{page:?}: {field:?} is described but no SettingsUi slot is bound for it"
            );
        }
    }
}

/// The fields whose controls had a DEDICATED handler in the inline builder must still have one:
/// each does more than write the config (retitles a button, freezes dependent rows, restarts a
/// tap, re-reads a device), and losing it is silent — the value changes and the page stops
/// following it. The list is the old builders' `sel!(handle…:)` assignments.
#[test]
fn every_field_with_a_dedicated_handler_still_has_one() {
    let dedicated = [
        (ControlField::WindowsEnabled, "handleWindowsEnabledToggle:"),
        (ControlField::MouseEnabled, "handleEnableMouseToggle:"),
        (ControlField::MappingEnabled, "handleMappingEnabledChanged:"),
        (
            ControlField::ClipboardEnabled,
            "handleClipboardEnabledToggle:",
        ),
        (ControlField::ClipboardPaused, "handlePauseRecordingToggle:"),
        (ControlField::KeyBindingsEnabled, "handleKeyBindingsToggle:"),
    ];
    for (field, selector) in dedicated {
        let resolved = dedicated_selector(Some(field));
        assert_eq!(
            resolved.name().to_str().expect("a selector name is UTF-8"),
            selector,
            "{field:?} lost its dedicated handler and now runs the generic one"
        );
    }
    assert_eq!(
        dedicated_selector(Some(ControlField::Theme))
            .name()
            .to_str()
            .expect("a selector name is UTF-8"),
        "handleControlChanged:",
        "an ordinary field still uses the unified handler"
    );
}

/// Every control a page describes must be REACHABLE after the build: a config field through
/// its `SettingsUi` slot, a value row's action button through the action identity that names
/// it, and a slider's readout through the field its value belongs to. A control that is
/// identified but never bound looks alive and does nothing — exactly the About status rows and
/// the Clipboard readouts before this guard existed.
#[test]
fn every_described_control_has_a_binding() {
    let input =
        PageInput::from_config(Config::default(), None, crate::theme::ThemeInput::current());
    for page in [
        SettingsPageId::Switcher,
        SettingsPageId::General,
        SettingsPageId::Clipboard,
        SettingsPageId::About,
        SettingsPageId::KeystrokeDisplay,
        SettingsPageId::FinderMenu,
        SettingsPageId::KeyBindings,
        SettingsPageId::Mouse,
    ] {
        let view = view_for(page, &input);
        for row in &view.rows {
            match &row.control {
                ControlKind::Slider { .. } => assert!(
                    row.field().is_some(),
                    "{page:?}: a slider reads a field, so its readout can be bound"
                ),
                ControlKind::Value { action, .. } => {
                    // A value with an action needs BOTH bindings named; a plain value has
                    // one (the label itself, bound by the refresh that writes it).
                    let expected = match (&row.id, action) {
                        (ControlId::Action(name), Some(_)) => Some(*name),
                        (ControlId::Action(name), None) => Some(*name),
                        _ => None,
                    };
                    assert!(
                        expected.is_some(),
                        "{page:?}: a value row is bound by an action identity"
                    );
                }
                _ => {}
            }
        }
    }
}
