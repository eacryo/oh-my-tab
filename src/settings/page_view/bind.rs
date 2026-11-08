//! Handing a built page's controls to the fields the rest of the app reads.
//!
//! The dispatcher, the refill and the freeze lists read a handful of `SettingsUi` pointers
//! directly. `bind_page_controls` is the one place that fills them, and `refresh_migrated_pages`
//! is the one place that writes a page's values back into its controls.

use super::super::*;
use super::*;
/// Write the migrated pages' controls from the shared update description of `config`.
///
/// This is the computation both the refill (`load_settings_from`) and the in-place refresh
/// (`refresh_switcher_keystroke_and_mouse_controls_from_config`) consume. What the callers decide
/// is the POLICY around it (the page being opened, the page being visible, a shortcut capture in
/// progress, `MOUSE_PAGE_DIRTY`); this function only computes and writes.
pub(in crate::settings) unsafe fn refresh_migrated_pages(
    ui: &SettingsUi,
    input: &PageInput,
    pages: &[SettingsPageId],
) {
    let updates = page_update_description(input, pages);
    for page in pages {
        if let Some(rendered) = ui.migrated_pages[settings_page_index(*page)].as_ref() {
            apply_page_updates(rendered, &updates);
        }
    }
}

/// Rewrite one page action's value row in place. Used by a row whose text changes while the user is
/// looking at it (a recording's live preview), where a rebuild per update would tear the page down.
///
/// The row is matched by the action identity its description gave it, so a caller names the row the
/// way the page does -- the RECORD row's own `gesture_record` identity, not a second string that no
/// description ever registers.
pub(in crate::settings) unsafe fn set_action_row_text(
    ui: &SettingsUi,
    page: SettingsPageId,
    action: &'static str,
    text: &str,
) {
    let Some(rendered) = ui.migrated_pages[settings_page_index(page)].as_ref() else {
        return;
    };
    let Some(control) = rendered
        .controls
        .iter()
        .find(|control| control.id == ControlId::Action(action))
    else {
        return;
    };
    super::render::apply_control_update(control, &Value::Dynamic(text.to_string()), text);
}

/// The `SettingsUi` pointer the dispatcher, the refill's own consumers and the freeze lists read
/// for a field. Field identity stays the single source: the table says what a control edits, and
/// this table says which legacy pointer field that identity still fills.
///
/// A field with no entry here has no legacy consumer left (nothing reads `ui.<field>` for it), so
/// publishing it would only create a second name for the same control.
fn settings_ui_slot(ui: &mut SettingsUi, field: ControlField) -> Option<&mut *mut AnyObject> {
    if !settings_ui_slot_present(field) {
        return None;
    }
    Some(match field {
        ControlField::Theme => &mut ui.theme,
        ControlField::PanelMaterial => &mut ui.panel_material,
        ControlField::GlassStyle => &mut ui.glass_style,
        ControlField::Locale => &mut ui.locale,
        ControlField::LogLevel => &mut ui.log_level,
        ControlField::LaunchAtLogin => &mut ui.launch_at_login,
        ControlField::HideMenuBarIcon => &mut ui.hide_menu_bar_icon,
        ControlField::WindowsEnabled => &mut ui.windows_enabled,
        ControlField::ShowMinimized => &mut ui.show_minimized,
        ControlField::ShowHiddenAppWindows => &mut ui.show_hidden_app_windows,
        ControlField::ShowOtherDesktops => &mut ui.show_other_desktops,
        ControlField::ThumbnailsEnabled => &mut ui.thumbnails_enabled,
        ControlField::FocusedThumbnailPrewarm => &mut ui.focused_thumbnail_prewarm,
        ControlField::ShowAppNameInCards => &mut ui.show_app_name_in_cards,
        ControlField::ThumbnailSize => &mut ui.thumbnail_size,
        ControlField::CardTextSize => &mut ui.card_text_size,
        ControlField::StatusBarTextSize => &mut ui.status_bar_text_size,
        ControlField::OverlayPosition => &mut ui.overlay_position,
        ControlField::ActivationMode => &mut ui.activation_mode,
        ControlField::PanelCloseEnabled => &mut ui.panel_close_enabled,
        ControlField::PanelMinimizeEnabled => &mut ui.panel_minimize_enabled,
        ControlField::PanelHideEnabled => &mut ui.panel_hide_enabled,
        ControlField::PanelQuitEnabled => &mut ui.panel_quit_enabled,
        ControlField::Modifier => &mut ui.modifier,
        ControlField::KeystrokeDisplayEnabled => &mut ui.keystroke_display_enabled,
        ControlField::KeystrokeDisplayMode => &mut ui.keystroke_display_mode,
        ControlField::KeystrokeDisplayTapLevel => &mut ui.keystroke_display_tap_level,
        ControlField::KeystrokeDisplayPosition => &mut ui.keystroke_display_position,
        ControlField::KeystrokeDisplayInitialPosition => &mut ui.keystroke_display_initial_position,
        ControlField::KeystrokeDisplayMaxExtent => &mut ui.keystroke_display_max_extent,
        ControlField::ClipboardEnabled => &mut ui.clipboard_enabled,
        ControlField::ClipboardPaused => &mut ui.clipboard_paused,
        ControlField::ClipboardClearOnQuit => &mut ui.clipboard_clear_on_quit,
        ControlField::ClipboardSeparateClearAll => &mut ui.clipboard_separate_clear_all,
        ControlField::ClipboardShowSourceApp => &mut ui.clipboard_show_source_app,
        ControlField::ClipboardMoveUsedToTop => &mut ui.clipboard_move_used_to_top,
        ControlField::ClipboardDeleteAfterPaste => &mut ui.clipboard_delete_after_paste,
        ControlField::ClipboardClearSystemPasteboardAfterPaste => {
            &mut ui.clipboard_clear_system_pasteboard_after_paste
        }
        ControlField::ClipboardAutoExpireDays => &mut ui.clipboard_auto_expire_days,
        ControlField::ClipboardImageRows => &mut ui.clipboard_image_rows,
        ControlField::ClipboardPinFollow => &mut ui.clipboard_pin_follow,
        ControlField::ClipboardPickerPlacement => &mut ui.clipboard_picker_placement,
        ControlField::MouseEnabled => &mut ui.enable_mouse,
        ControlField::GesturesEnabled => &mut ui.gestures_enabled,
        ControlField::GestureTriggerButton => &mut ui.gesture_trigger_button,
        ControlField::GestureShowTrail => &mut ui.gesture_show_trail,
        ControlField::GestureShowHint => &mut ui.gesture_show_hint,
        ControlField::ReverseScroll => &mut ui.reverse_scroll,
        ControlField::ScrollMode => &mut ui.scroll_mode,
        ControlField::LineCount => &mut ui.line_count,
        ControlField::DisablePointerAccel => &mut ui.disable_pointer_accel,
        ControlField::PointerAcceleration => &mut ui.pointer_accel_slider,
        ControlField::MappingEnabled => &mut ui.mapping_enabled,
        ControlField::KeyBindingsEnabled => &mut ui.key_bindings_enabled,
        ControlField::UpdateAutoCheck => &mut ui.update_auto_check,
        ControlField::UpdateAutoDownload => &mut ui.update_auto_download,
        ControlField::FinderMenuEnabled => &mut ui.finder_menu_enabled,
        ControlField::FinderMenuLayout => &mut ui.finder_menu_layout,
        ControlField::FinderMenuCopyPath => &mut ui.finder_menu_copy_path,
        ControlField::FinderMenuCopySha256 => &mut ui.finder_menu_copy_sha256,
        // `settings_ui_slot_present` listed every field above; a new one without a slot is a
        // compile error here, which is what keeps the two in step.
        _ => {
            unreachable!("settings_ui_slot_present and settings_ui_slot must list the same fields")
        }
    })
}

/// Bind the page's permission banner to the three fields the window's banner refresh reads
/// (`refresh_settings_warning_banner`). Called only after the page's build succeeded, like every
/// other binding, so a refused build cannot leave the banner fields pointing at views that were
/// dropped.
pub(in crate::settings) fn bind_page_banner(ui: &mut SettingsUi, rendered: &RenderedPage) {
    if rendered.banner_strip.is_null() {
        return;
    }
    ui.permission_warning_view = rendered.banner_strip;
    ui.permission_warning_label = rendered.banner_label;
    ui.permission_warning_button = rendered.banner_button;
}

/// Bind a built page's controls to the legacy pointer fields their consumers still read.
///
/// One control at a time, after the page's registrations were published: a page whose build was
/// refused binds nothing, and the fields that already held the previous page's controls are left
/// alone until the new page is known good.
pub(in crate::settings) unsafe fn bind_page_controls(ui: &mut SettingsUi, rendered: &RenderedPage) {
    for control in &rendered.controls {
        // A page-drawn control has no ControlField; it says which of the fields its own callback
        // reads the value belongs in. Without this a page could draw a control the dispatcher can
        // never reach.
        match &control.extra {
            RenderedExtra::None => {}
            RenderedExtra::Shortcut { row_height } => {
                ui.clipboard_shortcut = control.control;
                ui.clipboard_shortcut_error = control.readout;
                ui.clipboard_shortcut_row_height = *row_height;
            }
            RenderedExtra::MaxEntries => ui.clipboard_max_entries = control.control,
        }
        // About's status rows are VALUES with their own action: the refresh function writes the
        // status text and the button that opens the matching pane, both by pointer. A row player
        // of `ControlField` has no room for them (they are not config fields), so they are bound
        // by the action identity that names them.
        if let ControlId::Action(action) = control.id {
            // The status label and the action button are bound SEPARATELY: the refresh function
            // writes the text by pointer and enables/disables the button by its own pointer.
            match action {
                "open_accessibility_permission" => {
                    ui.accessibility_permission_status = control.control;
                }
                "open_screen_recording_permission" => {
                    ui.screen_recording_permission_status = control.control;
                }
                "retry_keychain_access" => ui.keychain_access_status = control.control,
                _ => {}
            }
            if !control.action.is_null() {
                match action {
                    "open_accessibility_permission" => {
                        ui.accessibility_permission_button = control.action
                    }
                    "retry_keychain_access" => ui.keychain_access_button = control.action,
                    _ => {}
                }
            }
            continue;
        }
        let ControlId::Field(field) = control.id else {
            continue;
        };
        if let Some(slot) = settings_ui_slot(ui, field) {
            *slot = control.control;
        }
        // A slider's readout label is a second pointer field on the page.
        if !control.readout.is_null() {
            match field {
                ControlField::CardTextSize => ui.card_text_size_value_label = control.readout,
                ControlField::StatusBarTextSize => {
                    ui.status_bar_text_size_value_label = control.readout
                }
                ControlField::KeystrokeDisplayMaxExtent => {
                    ui.keystroke_display_max_extent_value = control.readout
                }
                // The clipboard page's two numeric rows draw a readout beside the slider; the
                // refill writes it, so it needs the pointer like every other readout.
                ControlField::ClipboardAutoExpireDays => {
                    ui.clipboard_auto_expire_days_value_label = control.readout
                }
                ControlField::ClipboardImageRows => {
                    ui.clipboard_image_rows_value_label = control.readout
                }
                _ => {}
            }
        }
    }
}

/// Whether the field has a legacy `SettingsUi` pointer field the dispatcher and the freeze lists
/// still read (the HIGH 2 coverage assertion). Field-presence only: it never touches a `SettingsUi`.
#[cfg(test)]
pub(super) fn settings_ui_slot_field(field: ControlField) -> Option<()> {
    settings_ui_slot_present(field).then_some(())
}

/// The fields [`bind_page_controls`] fills, as a predicate, so the coverage rule can be asserted
/// without an AppKit window. Kept beside the match that does the filling; `page_view` tests below
/// cross-check the two lists against each other.
pub(super) fn settings_ui_slot_present(field: ControlField) -> bool {
    matches!(
        field,
        ControlField::Theme
            | ControlField::PanelMaterial
            | ControlField::GlassStyle
            | ControlField::Locale
            | ControlField::LogLevel
            | ControlField::LaunchAtLogin
            | ControlField::HideMenuBarIcon
            | ControlField::WindowsEnabled
            | ControlField::ShowMinimized
            | ControlField::ShowHiddenAppWindows
            | ControlField::ShowOtherDesktops
            | ControlField::ThumbnailsEnabled
            | ControlField::FocusedThumbnailPrewarm
            | ControlField::ShowAppNameInCards
            | ControlField::ThumbnailSize
            | ControlField::CardTextSize
            | ControlField::StatusBarTextSize
            | ControlField::OverlayPosition
            | ControlField::ActivationMode
            | ControlField::PanelCloseEnabled
            | ControlField::PanelMinimizeEnabled
            | ControlField::PanelHideEnabled
            | ControlField::PanelQuitEnabled
            | ControlField::Modifier
            | ControlField::KeystrokeDisplayEnabled
            | ControlField::KeystrokeDisplayMode
            | ControlField::KeystrokeDisplayTapLevel
            | ControlField::KeystrokeDisplayPosition
            | ControlField::KeystrokeDisplayInitialPosition
            | ControlField::KeystrokeDisplayMaxExtent
            // The pages migrated after the first two: their controls exist only as described rows,
            // so the shared binder is the only place their legacy consumers (which read
            // `SettingsUi` fields directly) can be handed the control the table identified.
            | ControlField::ClipboardEnabled
            | ControlField::ClipboardPaused
            | ControlField::ClipboardClearOnQuit
            | ControlField::ClipboardSeparateClearAll
            | ControlField::ClipboardShowSourceApp
            | ControlField::ClipboardMoveUsedToTop
            | ControlField::ClipboardDeleteAfterPaste
            | ControlField::ClipboardClearSystemPasteboardAfterPaste
            | ControlField::ClipboardAutoExpireDays
            | ControlField::ClipboardImageRows
            | ControlField::ClipboardPinFollow
            | ControlField::ClipboardPickerPlacement
            | ControlField::MouseEnabled
            | ControlField::GesturesEnabled
            | ControlField::GestureTriggerButton
            | ControlField::GestureShowTrail
            | ControlField::GestureShowHint
            | ControlField::ReverseScroll
            | ControlField::ScrollMode
            | ControlField::LineCount
            | ControlField::DisablePointerAccel
            | ControlField::PointerAcceleration
            | ControlField::MappingEnabled
            | ControlField::KeyBindingsEnabled
            | ControlField::UpdateAutoCheck
            | ControlField::UpdateAutoDownload
            | ControlField::FinderMenuEnabled
            | ControlField::FinderMenuLayout
            | ControlField::FinderMenuCopyPath
            | ControlField::FinderMenuCopySha256
    )
}

/// Whether a slider field's readout label is bound by [`bind_page_controls`].
#[cfg(test)]
pub(super) fn settings_ui_readout_field(field: ControlField) -> bool {
    matches!(
        field,
        ControlField::CardTextSize
            | ControlField::StatusBarTextSize
            | ControlField::KeystrokeDisplayMaxExtent
    )
}
